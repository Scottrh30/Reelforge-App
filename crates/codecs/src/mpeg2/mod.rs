//! MPEG-2 video (ITU-T H.262 / ISO 13818-2), Main and 4:2:2 profiles:
//! I, P and B pictures, frame and field pictures, frame/field/16×8/dual-prime
//! motion, field DCT, alternate scan, non-linear quantiser, custom matrices,
//! 4:2:0, 4:2:2 and 4:4:4.
//!
//! Packets hold one picture (or both fields of one). Pictures come in
//! decode order — each B picture after the two reference pictures it sits
//! between — and go out in display order: a reference picture is held back
//! until the next one arrives.

mod tables;
#[doc(hidden)]
pub mod testgen;

use crate::bits::BitReader;
use crate::idct::{fast_qmat, idct_fast};
use crate::vlc::Vlc;
use crate::{invalid, Decoder, Error, Format, Matrix, Output, Picture, Result, StreamInfo};
use std::sync::{Arc, OnceLock};
use tables::*;

// ---- Code tables ---------------------------------------------------------------

struct Vlcs {
    dc_lum: Vlc,
    dc_chroma: Vlc,
    b14: Vlc,
    b15: Vlc,
    mbincr: Vlc,
    cbp: Vlc,
    mv: Vlc,
    ptype: Vlc,
    btype: Vlc,
}

fn vlcs() -> &'static Vlcs {
    static V: OnceLock<Vlcs> = OnceLock::new();
    V.get_or_init(|| {
        let types = |t: &[(u32, u8, u8)]| {
            let codes: Vec<u32> = t.iter().map(|e| e.0).collect();
            let lens: Vec<u8> = t.iter().map(|e| e.1).collect();
            let syms: Vec<u16> = t.iter().map(|e| e.2 as u16).collect();
            Vlc::new(&codes, &lens, Some(&syms))
        };
        Vlcs {
            dc_lum: Vlc::new(&DC_LUM_CODES, &DC_LUM_BITS, None),
            dc_chroma: Vlc::new(&DC_CHROMA_CODES, &DC_CHROMA_BITS, None),
            b14: Vlc::new(&B14_CODES, &B14_BITS, None),
            b15: Vlc::new(&B15_CODES, &B15_BITS, None),
            mbincr: Vlc::new(&MBINCR_CODES, &MBINCR_BITS, None),
            cbp: Vlc::new(&CBP_CODES, &CBP_BITS, None),
            mv: Vlc::new(&MV_CODES, &MV_BITS, None),
            ptype: types(&P_TYPES),
            btype: types(&B_TYPES),
        }
    })
}

const AC_ESCAPE: u16 = 111;
const AC_EOB: u16 = 112;

// ---- Headers ---------------------------------------------------------------------

#[derive(Clone)]
struct Sequence {
    width: u32,
    height: u32,
    /// 1 = 4:2:0, 2 = 4:2:2, 3 = 4:4:4.
    chroma_format: u32,
    progressive: bool,
    low_delay: bool,
    intra_matrix: [u8; 64],
    non_intra_matrix: [u8; 64],
    chroma_intra_matrix: [u8; 64],
    chroma_non_intra_matrix: [u8; 64],
    matrix: Matrix,
}

impl Sequence {
    fn mb_width(&self) -> usize {
        self.width.div_ceil(16) as usize
    }
    /// Macroblock rows of a frame (pairs of field rows for interlaced sequences).
    fn mb_height(&self) -> usize {
        if self.progressive {
            self.height.div_ceil(16) as usize
        } else {
            2 * self.height.div_ceil(32) as usize
        }
    }
    fn chroma_shift(&self) -> (usize, usize) {
        match self.chroma_format {
            1 => (1, 1),
            2 => (1, 0),
            _ => (0, 0),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    I,
    P,
    B,
}

#[derive(Clone, Copy)]
struct PictureHeader {
    kind: Kind,
    f_code: [[u32; 2]; 2],
    intra_dc_precision: u32,
    /// 1 top field, 2 bottom field, 3 frame.
    structure: u32,
    top_field_first: bool,
    frame_pred_frame_dct: bool,
    concealment_mvs: bool,
    q_scale_type: bool,
    intra_vlc_format: bool,
    alternate_scan: bool,
}

/// Read an 8-bit-per-entry matrix in zigzag order into raster order.
fn read_matrix(r: &mut BitReader) -> [u8; 64] {
    let mut m = [0u8; 64];
    for &z in ZIGZAG.iter() {
        m[z as usize] = r.read(8).max(1) as u8;
    }
    m
}

// ---- Frames ----------------------------------------------------------------------

/// A decoded (or being decoded) frame, padded to whole macroblocks.
struct Frame {
    planes: [Vec<u8>; 3],
    strides: [usize; 3],
    heights: [usize; 3],
    pts: i64,
    interlaced: bool,
}

impl Frame {
    fn new(seq: &Sequence) -> Frame {
        let w = seq.mb_width() * 16;
        let h = seq.mb_height() * 16;
        let (sx, sy) = seq.chroma_shift();
        let (cw, ch) = (w >> sx, h >> sy);
        Frame { planes: [vec![16; w * h], vec![128; cw * ch], vec![128; cw * ch]], strides: [w, cw, cw], heights: [h, ch, ch], pts: 0, interlaced: false }
    }
}

/// How to read one plane of a reference: the whole frame, or one field of it.
#[derive(Clone, Copy)]
struct View<'a> {
    data: &'a [u8],
    stride: usize,
    /// Lines of the frame plane.
    lines: usize,
    /// None: the frame; Some(p): field of parity p (lines p, p+2, …).
    field: Option<usize>,
}

impl View<'_> {
    #[inline]
    fn rows(&self) -> i32 {
        // (never more lines than the plane holds: a damaged stream can disagree with it)
        let lines = self.lines.min(self.data.len() / self.stride.max(1)).min(i32::MAX as usize);
        match self.field {
            None => lines as i32,
            Some(p) => (lines.saturating_sub(p).div_ceil(2)) as i32,
        }
    }
    #[inline]
    fn line(&self, r: i32) -> usize {
        match self.field {
            None => r as usize,
            Some(p) => p + 2 * r as usize,
        }
    }
}

/// Half-sample motion compensation of a `w`×`h` block from `src` at (`x`, `y`)
/// plus half-sample flags, written (or averaged, for the second direction)
/// into `dst` rows `row0 + k·row_step`.
#[allow(clippy::too_many_arguments)]
fn mc(src: &View, x: i32, y: i32, hx: bool, hy: bool, w: usize, h: usize, dst: &mut [u8], dst_stride: usize, row0: usize, row_step: usize, avg: bool) {
    let cols = src.stride as i32;
    let rows = src.rows();
    if rows <= 0 || cols <= 0 {
        return;
    }
    let inside = x >= 0 && y >= 0 && x + w as i32 + hx as i32 <= cols && y + h as i32 + hy as i32 <= rows;
    let at = |r: i32, c: i32| -> u32 {
        let r = r.clamp(0, rows - 1);
        let c = c.clamp(0, cols - 1);
        src.data.get(src.line(r) * src.stride + c as usize).copied().unwrap_or(128) as u32
    };
    for k in 0..h {
        let d = &mut dst[(row0 + k * row_step) * dst_stride..(row0 + k * row_step) * dst_stride + w];
        let r = y + k as i32;
        if inside {
            let n = w + hx as usize;
            let l0 = src.line(r) * src.stride + x as usize;
            let a = &src.data[l0..l0 + n];
            let b = if hy {
                let l1 = src.line(r + 1) * src.stride + x as usize;
                &src.data[l1..l1 + n]
            } else {
                a
            };
            for i in 0..w {
                let p = match (hx, hy) {
                    (false, false) => a[i] as u32,
                    (true, false) => (a[i] as u32 + a[i + 1] as u32 + 1) >> 1,
                    (false, true) => (a[i] as u32 + b[i] as u32 + 1) >> 1,
                    (true, true) => (a[i] as u32 + a[i + 1] as u32 + b[i] as u32 + b[i + 1] as u32 + 2) >> 2,
                };
                d[i] = if avg { ((d[i] as u32 + p + 1) >> 1) as u8 } else { p as u8 };
            }
        } else {
            for (i, dv) in d.iter_mut().enumerate() {
                let c = x + i as i32;
                let p = match (hx, hy) {
                    (false, false) => at(r, c),
                    (true, false) => (at(r, c) + at(r, c + 1) + 1) >> 1,
                    (false, true) => (at(r, c) + at(r + 1, c) + 1) >> 1,
                    (true, true) => (at(r, c) + at(r, c + 1) + at(r + 1, c) + at(r + 1, c + 1) + 2) >> 2,
                };
                *dv = if avg { ((*dv as u32 + p + 1) >> 1) as u8 } else { p as u8 };
            }
        }
    }
}

// ---- Macroblocks -----------------------------------------------------------------

/// The prediction / reconstruction buffers of one macroblock, in the
/// macroblock's own rows (frame lines for frame pictures, field lines for
/// field pictures).
struct MbPixels {
    y: [u8; 256],
    c: [[u8; 256]; 2],
}

/// Motion of one macroblock.
#[derive(Clone, Copy, Default)]
struct Motion {
    /// 0 frame (16×16), 1 field (two 16×8 field predictions in a frame picture; 16×16 in a field picture),
    /// 2 16×8 (field pictures), 3 dual prime.
    kind: u8,
    /// [direction][vector][x/y]; dual prime uses vectors 2 and 3 for the derived ones.
    mv: [[[i32; 2]; 4]; 2],
    field_select: [[usize; 2]; 2],
    dirs: u8,
}

struct Slicer<'a> {
    seq: &'a Sequence,
    ph: &'a PictureHeader,
    /// The two references: [forward, backward].
    refs: [Option<&'a Frame>; 2],
    /// The second field's same-frame reference (the first field), when it is one.
    second_field: bool,
    q: [f32; 64],
}

struct SliceState {
    qscale: i32,
    last_dc: [i32; 3],
    pmv: [[[i32; 2]; 2]; 2],
    last_motion: Motion,
}

fn decode_motion(r: &mut BitReader, f_code: u32, pred: i32) -> Result<i32> {
    let v = vlcs();
    let code = v.mv.read(r).ok_or_else(|| Error::Invalid("MPEG-2: bad motion code".into()))? as i32;
    if code == 0 {
        return Ok(pred);
    }
    let neg = r.bit();
    let shift = f_code as i32 - 1;
    let mut val = code;
    if shift > 0 {
        val = ((val - 1) << shift) | r.read(shift as u32) as i32;
        val += 1;
    }
    if neg {
        val = -val;
    }
    val += pred;
    // Wrap into range.
    let bits = 5 + shift;
    Ok((val << (32 - bits)) >> (32 - bits))
}

fn dmv(r: &mut BitReader) -> i32 {
    if r.bit() {
        if r.bit() {
            -1
        } else {
            1
        }
    } else {
        0
    }
}

impl Slicer<'_> {
    fn qscale(&self, code: u32) -> i32 {
        if self.ph.q_scale_type {
            NON_LINEAR_QSCALE[code as usize] as i32
        } else {
            code as i32 * 2
        }
    }

    fn blocks_per_mb(&self) -> usize {
        match self.seq.chroma_format {
            1 => 6,
            2 => 8,
            _ => 12,
        }
    }

    /// One intra block's coefficients (dequantised, mismatch-controlled).
    fn intra_block(&self, r: &mut BitReader, st: &mut SliceState, n: usize, out: &mut [i32; 64]) -> Result<()> {
        let v = vlcs();
        let (comp, matrix) = if n < 4 { (0, &self.seq.intra_matrix) } else { (1 + (n & 1), &self.seq.chroma_intra_matrix) };
        let size = if comp == 0 { v.dc_lum.read(r) } else { v.dc_chroma.read(r) }.ok_or_else(|| Error::Invalid("MPEG-2: bad DC size".into()))? as u32;
        let diff = if size == 0 {
            0
        } else {
            let bits = r.read(size) as i32;
            if bits >> (size - 1) != 0 { bits } else { bits - ((1 << size) - 1) }
        };
        st.last_dc[comp] += diff;
        *out = [0; 64];
        out[0] = st.last_dc[comp] << (3 - self.ph.intra_dc_precision);
        let mut parity = out[0] & 1;
        let scan = if self.ph.alternate_scan { &ALTERNATE_SCAN } else { &ZIGZAG };
        let table = if self.ph.intra_vlc_format { &v.b15 } else { &v.b14 };
        let mut i = 0usize;
        loop {
            let sym = table.read(r).ok_or_else(|| Error::Invalid("MPEG-2: bad coefficient code".into()))?;
            if sym == AC_EOB {
                break;
            }
            let (run, level) = if sym == AC_ESCAPE {
                (r.read(6) as usize, r.read_signed(12))
            } else {
                let l = AC_LEVEL[sym as usize] as i32;
                (AC_RUN[sym as usize] as usize, if r.bit() { -l } else { l })
            };
            i += run + 1;
            if i > 63 {
                return invalid("MPEG-2: coefficients past the block");
            }
            let j = scan[i] as usize;
            let mag = (level.abs() * st.qscale * matrix[j] as i32) >> 4;
            let val = (if level < 0 { -mag } else { mag }).clamp(-2048, 2047);
            out[j] = val;
            parity ^= val & 1;
        }
        if parity == 0 {
            out[63] ^= 1;
        }
        Ok(())
    }

    fn non_intra_block(&self, r: &mut BitReader, st: &SliceState, n: usize, out: &mut [i32; 64]) -> Result<()> {
        let v = vlcs();
        let matrix = if n < 4 { &self.seq.non_intra_matrix } else { &self.seq.chroma_non_intra_matrix };
        let scan = if self.ph.alternate_scan { &ALTERNATE_SCAN } else { &ZIGZAG };
        *out = [0; 64];
        let mut parity = 0;
        let mut i: isize = -1;
        let mut first = true;
        loop {
            let (run, level) = if first && r.peek(1) == 1 {
                // The first coefficient's "1s" code: run 0, level ±1.
                r.skip(1);
                (0usize, if r.bit() { -1 } else { 1 })
            } else {
                let sym = v.b14.read(r).ok_or_else(|| Error::Invalid("MPEG-2: bad coefficient code".into()))?;
                if sym == AC_EOB {
                    break;
                }
                if sym == AC_ESCAPE {
                    (r.read(6) as usize, r.read_signed(12))
                } else {
                    let l = AC_LEVEL[sym as usize] as i32;
                    (AC_RUN[sym as usize] as usize, if r.bit() { -l } else { l })
                }
            };
            first = false;
            i += run as isize + 1;
            if i > 63 {
                return invalid("MPEG-2: coefficients past the block");
            }
            let j = scan[i as usize] as usize;
            let mag = ((2 * level.abs() + 1) * st.qscale * matrix[j] as i32) >> 5;
            let val = (if level < 0 { -mag } else { mag }).clamp(-2048, 2047);
            out[j] = val;
            parity ^= val & 1;
        }
        if parity == 0 {
            out[63] ^= 1;
        }
        Ok(())
    }

    /// Inverse DCT of `c`, written (`add`: added to what's there) into `dst`
    /// at (`x`, `row0`), rows stepping by `step`.
    fn put_block(&self, c: &[i32; 64], dst: &mut [u8], stride: usize, x: usize, row0: usize, step: usize, add: bool) {
        let mut o = [[0f32; 8]; 8];
        idct_fast(c, &self.q, &mut o);
        for (k, row) in o.iter().enumerate() {
            let d = &mut dst[(row0 + k * step) * stride + x..(row0 + k * step) * stride + x + 8];
            for i in 0..8 {
                let v = row[i].round() as i32 + if add { d[i] as i32 } else { 0 };
                d[i] = v.clamp(0, 255) as u8;
            }
        }
    }

    /// Prediction for one direction into `px` (`avg` for the second direction).
    fn predict(&self, m: &Motion, dir: usize, mb_x: usize, mb_y: usize, cur: &Frame, px: &mut MbPixels, avg: bool) {
        let field_pic = self.ph.structure != 3;
        let (sx, sy) = self.seq.chroma_shift();
        let cw = 16 >> sx;
        // The reference frame for a field (picture-relative parity), honouring
        // second-field references to the first field of this frame.
        let reference = |field_select: usize| -> Option<&Frame> {
            let same_frame = field_pic && self.second_field && self.ph.kind == Kind::P && field_select + 1 != self.ph.structure as usize;
            if same_frame {
                Some(cur)
            } else {
                self.refs[dir]
            }
        };
        // One prediction: `field` = which field of the reference to read (None = frame),
        // block rows `rows` starting at `y_base` in the view, into MB rows row0/step.
        let one = |frame: &Frame, field: Option<usize>, mv: [i32; 2], y_base: i32, h: usize, row0: usize, step: usize, avg: bool, px: &mut MbPixels| {
            let (mx, my) = (mv[0], mv[1]);
            let v = View { data: &frame.planes[0], stride: frame.strides[0], lines: frame.heights[0], field };
            mc(&v, mb_x as i32 * 16 + (mx >> 1), y_base + (my >> 1), mx & 1 != 0, my & 1 != 0, 16, h, &mut px.y, 16, row0, step, avg);
            // Chroma vectors (truncating division, then half-sample flags).
            let (cmx, cmy) = (if sx == 1 { mx / 2 } else { mx }, if sy == 1 { my / 2 } else { my });
            let cy_base = y_base >> sy;
            let chh = h >> sy;
            // Rows of the macroblock's chroma: halves scale down, interleaved fields don't.
            let crow0 = if step == 1 { row0 >> sy } else { row0 };
            for p in 0..2 {
                let v = View { data: &frame.planes[1 + p], stride: frame.strides[1 + p], lines: frame.heights[1 + p], field };
                mc(&v, (mb_x * cw) as i32 + (cmx >> 1), cy_base + (cmy >> 1), cmx & 1 != 0, cmy & 1 != 0, cw, chh, &mut px.c[p], 16, crow0, step, avg);
            }
        };
        let mv = m.mv[dir];
        let fs = m.field_select[dir];
        if !field_pic {
            match m.kind {
                0 => {
                    if let Some(f) = self.refs[dir] {
                        one(f, None, mv[0], (mb_y * 16) as i32, 16, 0, 1, avg, px);
                    }
                }
                1 => {
                    if let Some(f) = self.refs[dir] {
                        for j in 0..2 {
                            one(f, Some(fs[j]), mv[j], (mb_y * 8) as i32, 8, j, 2, avg, px);
                        }
                    }
                }
                _ => {
                    // Dual prime: same-parity prediction averaged with the opposite-parity one.
                    if let Some(f) = self.refs[dir] {
                        for i in 0..2 {
                            for j in 0..2 {
                                one(f, Some(j ^ i), mv[2 * i + j], (mb_y * 8) as i32, 8, j, 2, avg || i == 1, px);
                            }
                        }
                    }
                }
            }
        } else {
            let fy = mb_y as i32; // field macroblock row
            match m.kind {
                1 => {
                    if let Some(f) = reference(fs[0]) {
                        one(f, Some(fs[0]), mv[0], fy * 16, 16, 0, 1, avg, px);
                    }
                }
                2 => {
                    for i in 0..2 {
                        if let Some(f) = reference(fs[i]) {
                            // The lower half: rows 8–15 of the macroblock.
                            one(f, Some(fs[i]), mv[i], fy * 16 + 8 * i as i32, 8, 8 * i, 1, avg, px);
                        }
                    }
                }
                _ => {
                    let parity = (self.ph.structure as usize).saturating_sub(1).min(1);
                    for i in 0..2 {
                        let sel = if i == 0 { parity } else { 1 - parity };
                        if let Some(f) = reference(sel) {
                            one(f, Some(sel), mv[2 * i], fy * 16, 16, 0, 1, avg || i == 1, px);
                        }
                    }
                }
            }
        }
    }
}

/// A finished macroblock, waiting to be written into the frame.
type Decoded = (usize, usize, MbPixels);

/// Decode one slice, appending its macroblocks to `out`. `cur` is only read (the
/// second field of a P frame may predict from the first), so slices run in parallel.
fn decode_slice(s: &Slicer, cur: &Frame, data: &[u8], mb_row: usize, out: &mut Vec<Decoded>) -> Result<()> {
    let v = vlcs();
    let mut r = BitReader::new(data);
    let field_pic = s.ph.structure != 3;
    let mbw = s.seq.mb_width();
    let rows = if field_pic { s.seq.mb_height() / 2 } else { s.seq.mb_height() };
    if mb_row >= rows {
        return invalid("MPEG-2: slice below the picture");
    }
    let qcode = r.read(5);
    let mut st = SliceState { qscale: s.qscale(qcode), last_dc: [128 << s.ph.intra_dc_precision; 3], pmv: [[[0; 2]; 2]; 2], last_motion: Motion::default() };
    // Extra slice information.
    while r.bit() {
        r.skip(8);
    }
    let nblocks = s.blocks_per_mb();
    let mut mb_x: isize = -1;
    let mut blocks = vec![[0i32; 64]; nblocks];
    loop {
        // Macroblock address increment (skipped macroblocks in between).
        let mut inc = 0usize;
        loop {
            let c = v.mbincr.read(&mut r).ok_or_else(|| Error::Invalid("MPEG-2: bad address increment".into()))?;
            match c {
                33 => inc += 33,
                34 => {}
                35 => return Ok(()),
                n => {
                    inc += n as usize + 1;
                    break;
                }
            }
        }
        let first = mb_x < 0;
        let target = if first { inc as isize - 1 } else { mb_x + inc as isize };
        if !first {
            // Skipped macroblocks.
            for x in (mb_x + 1)..target {
                if s.ph.kind == Kind::I {
                    return invalid("MPEG-2: skipped macroblock in an I picture");
                }
                st.last_dc = [128 << s.ph.intra_dc_precision; 3];
                let mut m = Motion::default();
                if s.ph.kind == Kind::P {
                    st.pmv = [[[0; 2]; 2]; 2];
                    m.dirs = 1;
                } else {
                    m.dirs = st.last_motion.dirs;
                    m.mv[0][0] = st.pmv[0][0];
                    m.mv[1][0] = st.pmv[1][0];
                }
                m.kind = if field_pic { 1 } else { 0 };
                let parity = (s.ph.structure as usize).wrapping_sub(1) & 1;
                m.field_select = [[parity; 2]; 2];
                reconstruct(s, cur, out, x as usize, mb_row, &m, None, false, &blocks, 0);
            }
        }
        mb_x = target;
        if mb_x < 0 || mb_x as usize >= mbw {
            return invalid("MPEG-2: macroblock past the row");
        }

        // Macroblock type.
        let flags = match s.ph.kind {
            Kind::I => {
                if r.bit() {
                    INTRA
                } else if r.bit() {
                    INTRA | QUANT
                } else {
                    return invalid("MPEG-2: bad I macroblock type");
                }
            }
            Kind::P => v.ptype.read(&mut r).ok_or_else(|| Error::Invalid("MPEG-2: bad P macroblock type".into()))? as u8,
            Kind::B => v.btype.read(&mut r).ok_or_else(|| Error::Invalid("MPEG-2: bad B macroblock type".into()))? as u8,
        };
        let mut motion = Motion::default();
        let mut field_dct = false;
        if flags & INTRA != 0 {
            if !field_pic && !s.ph.frame_pred_frame_dct {
                field_dct = r.bit();
            }
            if flags & QUANT != 0 {
                st.qscale = s.qscale(r.read(5));
            }
            if s.ph.concealment_mvs {
                if field_pic {
                    r.skip(1);
                }
                for k in 0..2 {
                    let val = decode_motion(&mut r, s.ph.f_code[0][k], st.pmv[0][0][k])?;
                    st.pmv[0][0][k] = val;
                    st.pmv[0][1][k] = val;
                }
                r.skip(1);
            } else {
                st.pmv = [[[0; 2]; 2]; 2];
            }
            for (n, b) in blocks.iter_mut().enumerate() {
                s.intra_block(&mut r, &mut st, n, b)?;
            }
            reconstruct(s, cur, out, mb_x as usize, mb_row, &motion, None, field_dct, &blocks, (1 << nblocks) - 1);
            st.last_motion = Motion::default();
        } else {
            if flags & NO_MC != 0 {
                if !field_pic && !s.ph.frame_pred_frame_dct {
                    field_dct = r.bit();
                }
                if flags & QUANT != 0 {
                    st.qscale = s.qscale(r.read(5));
                }
                st.pmv[0] = [[0; 2]; 2];
                motion.dirs = 1;
                motion.kind = if field_pic { 1 } else { 0 };
                let parity = (s.ph.structure as usize).wrapping_sub(1) & 1;
                motion.field_select = [[parity; 2]; 2];
            } else {
                let kind_code = if !field_pic && s.ph.frame_pred_frame_dct { 2 } else { r.read(2) };
                if !field_pic && flags & PATTERN != 0 && !s.ph.frame_pred_frame_dct {
                    field_dct = r.bit();
                }
                if flags & QUANT != 0 {
                    st.qscale = s.qscale(r.read(5));
                }
                motion.dirs = (flags & FORWARD != 0) as u8 | (((flags & BACKWARD != 0) as u8) << 1);
                motion.kind = match (field_pic, kind_code) {
                    (false, 2) => 0,
                    (_, 1) => 1,
                    (true, 2) => 2,
                    (_, 3) => 3,
                    _ => return invalid("MPEG-2: bad motion type"),
                };
                for dir in 0..2 {
                    if motion.dirs & (1 << dir) == 0 {
                        continue;
                    }
                    let f = s.ph.f_code[dir];
                    match (field_pic, motion.kind) {
                        (false, 0) => {
                            for k in 0..2 {
                                let val = decode_motion(&mut r, f[k], st.pmv[dir][0][k])?;
                                st.pmv[dir][0][k] = val;
                                st.pmv[dir][1][k] = val;
                                motion.mv[dir][0][k] = val;
                            }
                        }
                        (false, 1) => {
                            for j in 0..2 {
                                motion.field_select[dir][j] = r.bit() as usize;
                                let x = decode_motion(&mut r, f[0], st.pmv[dir][j][0])?;
                                st.pmv[dir][j][0] = x;
                                let y = decode_motion(&mut r, f[1], st.pmv[dir][j][1] >> 1)?;
                                st.pmv[dir][j][1] = 2 * y;
                                motion.mv[dir][j] = [x, y];
                            }
                        }
                        (true, 1) => {
                            motion.field_select[dir][0] = r.bit() as usize;
                            for k in 0..2 {
                                let val = decode_motion(&mut r, f[k], st.pmv[dir][0][k])?;
                                st.pmv[dir][0][k] = val;
                                st.pmv[dir][1][k] = val;
                                motion.mv[dir][0][k] = val;
                            }
                        }
                        (true, 2) => {
                            for j in 0..2 {
                                motion.field_select[dir][j] = r.bit() as usize;
                                for k in 0..2 {
                                    let val = decode_motion(&mut r, f[k], st.pmv[dir][j][k])?;
                                    st.pmv[dir][j][k] = val;
                                    motion.mv[dir][j][k] = val;
                                }
                            }
                        }
                        (_, _) => {
                            // Dual prime.
                            if s.seq.progressive {
                                return invalid("MPEG-2: dual prime in a progressive sequence");
                            }
                            let my_shift = !field_pic as i32;
                            let mx = decode_motion(&mut r, f[0], st.pmv[dir][0][0])?;
                            st.pmv[dir][0][0] = mx;
                            st.pmv[dir][1][0] = mx;
                            let dmx = dmv(&mut r);
                            let my = decode_motion(&mut r, f[1], st.pmv[dir][0][1] >> my_shift)?;
                            let dmy = dmv(&mut r);
                            st.pmv[dir][0][1] = my << my_shift;
                            st.pmv[dir][1][1] = my << my_shift;
                            motion.mv[dir][0] = [mx, my];
                            motion.mv[dir][1] = [mx, my];
                            if !field_pic {
                                let mut m = if s.ph.top_field_first { 1 } else { 3 };
                                motion.mv[dir][2] = [((mx * m + (mx > 0) as i32) >> 1) + dmx, ((my * m + (my > 0) as i32) >> 1) + dmy - 1];
                                m = 4 - m;
                                motion.mv[dir][3] = [((mx * m + (mx > 0) as i32) >> 1) + dmx, ((my * m + (my > 0) as i32) >> 1) + dmy + 1];
                            } else {
                                let adj = if s.ph.structure == 1 { -1 } else { 1 };
                                motion.mv[dir][2] = [((mx + (mx > 0) as i32) >> 1) + dmx, ((my + (my > 0) as i32) >> 1) + dmy + adj];
                            }
                        }
                    }
                }
            }
            st.last_dc = [128 << s.ph.intra_dc_precision; 3];
            let mut cbp = 0u32;
            if flags & PATTERN != 0 {
                cbp = v.cbp.read(&mut r).ok_or_else(|| Error::Invalid("MPEG-2: bad block pattern".into()))? as u32;
                if nblocks > 6 {
                    cbp = (cbp << (nblocks - 6)) | r.read(nblocks as u32 - 6);
                }
                if cbp == 0 {
                    return invalid("MPEG-2: empty block pattern");
                }
                for n in 0..nblocks {
                    if cbp & (1 << (nblocks - 1 - n)) != 0 {
                        s.non_intra_block(&mut r, &st, n, &mut blocks[n])?;
                    }
                }
            }
            // Pattern bit order: block 0 is the most significant.
            let mut mask = 0u32;
            for n in 0..nblocks {
                if cbp & (1 << (nblocks - 1 - n)) != 0 {
                    mask |= 1 << n;
                }
            }
            reconstruct(s, cur, out, mb_x as usize, mb_row, &motion, Some(()), field_dct, &blocks, mask);
            st.last_motion = motion;
        }
        if r.overrun {
            return invalid("MPEG-2: slice cut short");
        }
        // End of the slice: only zero bits (the next start code) left.
        let left = r.bits_left();
        if left <= 0 || (left < 24 && r.peek(left as u32) == 0) || (left >= 23 && r.peek(23) == 0) {
            return Ok(());
        }
    }
}

/// Predict (if `inter`) and add the coded blocks in `mask`.
#[allow(clippy::too_many_arguments)]
fn reconstruct(s: &Slicer, cur: &Frame, out: &mut Vec<Decoded>, mb_x: usize, mb_y: usize, m: &Motion, inter: Option<()>, field_dct: bool, blocks: &[[i32; 64]], mask: u32) {
    let ch = 16 >> s.seq.chroma_shift().1;
    let mut px = MbPixels { y: [128; 256], c: [[128; 256]; 2] };
    let is_inter = inter.is_some() || m.dirs != 0;
    if is_inter {
        let mut first = true;
        for dir in 0..2 {
            if m.dirs & (1 << dir) != 0 {
                s.predict(m, dir, mb_x, mb_y, cur, &mut px, !first);
                first = false;
            }
        }
    }
    let add = is_inter;
    // Luma blocks: frame DCT (quadrants) or field DCT (alternate lines).
    for n in 0..4 {
        if mask & (1 << n) == 0 {
            continue;
        }
        let x = (n & 1) * 8;
        let (row0, step) = if field_dct { (n >> 1, 2) } else { ((n >> 1) * 8, 1) };
        s.put_block(&blocks[n], &mut px.y, 16, x, row0, step, add);
    }
    // Chroma: 4:2:0 one block per plane; 4:2:2 two (above each other); 4:4:4 four.
    let nchroma = blocks.len() - 4;
    for k in 0..nchroma {
        let n = 4 + k;
        if mask & (1 << n) == 0 {
            continue;
        }
        let p = k & 1;
        let idx = k >> 1; // 0: top-left, 1: below, 2: right, 3: right below
        let x = if idx >= 2 { 8 } else { 0 };
        let lower = idx & 1 == 1;
        let (row0, step) = if ch == 16 && field_dct { (lower as usize, 2) } else { (if lower { 8 } else { 0 }, 1) };
        s.put_block(&blocks[n], &mut px.c[p], 16, x, row0, step, add);
    }
    out.push((mb_x, mb_y, px));
}

/// Write a macroblock into the frame (field lines for field pictures).
fn write_mb(cur: &mut Frame, structure: u32, (cw, ch): (usize, usize), (mb_x, mb_y, px): &Decoded) {
    let (mb_x, mb_y) = (*mb_x, *mb_y);
    let field = match structure {
        1 => Some(0usize),
        2 => Some(1),
        _ => None,
    };
    let put = |plane: &mut Vec<u8>, stride: usize, lines: usize, src: &[u8], w: usize, h: usize, x0: usize, y0: usize| {
        for k in 0..h {
            let line = match field {
                None => y0 + k,
                Some(p) => p + 2 * (y0 + k),
            };
            if line >= lines || x0 + w > stride {
                break;
            }
            // (a picture whose size disagrees with the frame's is clipped, not a crash)
            let (Some(d), Some(s)) = (plane.get_mut(line * stride + x0..line * stride + x0 + w), src.get(k * 16..k * 16 + w)) else { break };
            d.copy_from_slice(s);
        }
    };
    let (ys, yl) = (cur.strides[0], cur.heights[0]);
    put(&mut cur.planes[0], ys, yl, &px.y, 16, 16, mb_x * 16, mb_y * 16);
    for p in 0..2 {
        let (cs, cl) = (cur.strides[1 + p], cur.heights[1 + p]);
        put(&mut cur.planes[1 + p], cs, cl, &px.c[p], cw, ch, mb_x * cw, mb_y * ch);
    }
}

// ---- The decoder -------------------------------------------------------------------

pub struct Mpeg2Decoder {
    seq: Option<Sequence>,
    /// Forward and backward references (older, newer).
    refs: [Option<Arc<Frame>>; 2],
    /// The newest reference, not shown yet (it's displayed after the B pictures that follow it).
    held: Option<Arc<Frame>>,
    /// A frame whose first field is done, waiting for its second.
    half: Option<(Frame, PictureHeader)>,
    out: Output,
    /// Tests: report any damaged slice (B pictures included).
    strict: bool,
}

impl Mpeg2Decoder {
    pub fn new(info: &StreamInfo) -> Self {
        let mut d = Mpeg2Decoder { seq: None, refs: [None, None], held: None, half: None, out: Output::default(), strict: false };
        if !info.extradata.is_empty() {
            let _ = d.headers_only(&info.extradata);
        }
        d
    }

    fn headers_only(&mut self, data: &[u8]) -> Result<()> {
        for (code, body) in units(data) {
            match code {
                0xB3 => self.sequence_header(body)?,
                0xB5 => self.extension(body, None)?,
                _ => {}
            }
        }
        Ok(())
    }

    fn sequence_header(&mut self, body: &[u8]) -> Result<()> {
        let mut r = BitReader::new(body);
        let width = r.read(12);
        let height = r.read(12);
        r.skip(4 + 4 + 18 + 1 + 10 + 1);
        let intra = if r.bit() { read_matrix(&mut r) } else { DEFAULT_INTRA_MATRIX };
        let non_intra = if r.bit() { read_matrix(&mut r) } else { [16; 64] };
        if width == 0 || height == 0 {
            return invalid("MPEG-2: empty picture size");
        }
        let old = self.seq.as_ref();
        let (chroma_format, progressive, low_delay, matrix) = old.map_or((1, true, false, Matrix::Unknown), |s| (s.chroma_format, s.progressive, s.low_delay, s.matrix));
        let (w_hi, h_hi) = old.map_or((0, 0), |s| (s.width & !0xFFF, s.height & !0xFFF));
        self.seq = Some(Sequence {
            width: width | w_hi,
            height: height | h_hi,
            chroma_format,
            progressive,
            low_delay,
            intra_matrix: intra,
            non_intra_matrix: non_intra,
            chroma_intra_matrix: intra,
            chroma_non_intra_matrix: non_intra,
            matrix,
        });
        Ok(())
    }

    fn extension(&mut self, body: &[u8], ph: Option<&mut PictureHeader>) -> Result<()> {
        let mut r = BitReader::new(body);
        let id = r.read(4);
        match id {
            1 => {
                // Sequence extension.
                let Some(seq) = self.seq.as_mut() else { return Ok(()) };
                r.skip(8);
                seq.progressive = r.bit();
                seq.chroma_format = r.read(2).max(1);
                let (hx, vx) = (r.read(2), r.read(2));
                seq.width = (seq.width & 0xFFF) | (hx << 12);
                seq.height = (seq.height & 0xFFF) | (vx << 12);
                r.skip(12 + 1 + 8);
                seq.low_delay = r.bit();
            }
            2 => {
                // Sequence display extension: colour description.
                let Some(seq) = self.seq.as_mut() else { return Ok(()) };
                r.skip(3);
                if r.bit() {
                    r.skip(16);
                    seq.matrix = match r.read(8) {
                        1 => Matrix::Bt709,
                        4..=6 => Matrix::Bt601,
                        9 | 10 => Matrix::Bt2020,
                        _ => Matrix::Unknown,
                    };
                }
            }
            3 => {
                // Quant matrix extension.
                let Some(seq) = self.seq.as_mut() else { return Ok(()) };
                if r.bit() {
                    let m = read_matrix(&mut r);
                    seq.intra_matrix = m;
                    seq.chroma_intra_matrix = m;
                }
                if r.bit() {
                    let m = read_matrix(&mut r);
                    seq.non_intra_matrix = m;
                    seq.chroma_non_intra_matrix = m;
                }
                if r.bit() {
                    seq.chroma_intra_matrix = read_matrix(&mut r);
                }
                if r.bit() {
                    seq.chroma_non_intra_matrix = read_matrix(&mut r);
                }
            }
            8 => {
                // Picture coding extension.
                if let Some(ph) = ph {
                    for d in 0..2 {
                        for k in 0..2 {
                            ph.f_code[d][k] = r.read(4).max(1);
                        }
                    }
                    ph.intra_dc_precision = r.read(2);
                    ph.structure = r.read(2);
                    ph.top_field_first = r.bit();
                    ph.frame_pred_frame_dct = r.bit();
                    ph.concealment_mvs = r.bit();
                    ph.q_scale_type = r.bit();
                    ph.intra_vlc_format = r.bit();
                    ph.alternate_scan = r.bit();
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// A frame is complete: send it towards the output in display order.
    fn finish_frame(&mut self, f: Frame, kind: Kind) {
        let f = Arc::new(f);
        if kind == Kind::B {
            self.emit(&f);
        } else {
            if let Some(h) = self.held.take() {
                self.emit(&h);
            }
            self.refs[0] = self.refs[1].take();
            self.refs[1] = Some(f.clone());
            if self.seq.as_ref().is_some_and(|s| s.low_delay) {
                self.emit(&f);
            } else {
                self.held = Some(f);
            }
        }
    }

    fn emit(&mut self, f: &Frame) {
        let Some(seq) = &self.seq else { return };
        let format = match seq.chroma_format {
            1 => Format::Yuv420P8,
            2 => Format::Yuv422P8,
            _ => Format::Yuv444P8,
        };
        let (w, h) = (seq.width, seq.height);
        let mut pic = Picture::new(w, h, format);
        for p in 0..3 {
            let (pw, ph) = format.plane_size(p, w, h);
            let dst = &mut pic.planes[p];
            for y in 0..ph as usize {
                // (a frame decoded before a size change is smaller than the new size: what it
                // has is copied, the rest stays black)
                let n = (pw as usize).min(f.strides[p]);
                let (Some(s), Some(d)) = (f.planes[p].get(y * f.strides[p]..y * f.strides[p] + n), dst.data.get_mut(y * dst.stride..y * dst.stride + n)) else { break };
                d.copy_from_slice(s);
            }
        }
        pic.pts = f.pts;
        pic.matrix = seq.matrix;
        pic.interlaced = f.interlaced;
        self.out.push(pic);
    }

    fn picture(&mut self, header: &[u8], rest: &[(u8, &[u8])], pts: i64) -> Result<()> {
        let Some(seq) = self.seq.clone() else { return invalid("MPEG-2: picture before any sequence header") };
        let mut r = BitReader::new(header);
        r.skip(10);
        let kind = match r.read(3) {
            1 => Kind::I,
            2 => Kind::P,
            3 => Kind::B,
            t => return Err(Error::Unsupported(format!("MPEG-2 picture type {t}"))),
        };
        let mut ph = PictureHeader {
            kind,
            f_code: [[15; 2]; 2],
            intra_dc_precision: 0,
            structure: 3,
            top_field_first: false,
            frame_pred_frame_dct: true,
            concealment_mvs: false,
            q_scale_type: false,
            intra_vlc_format: false,
            alternate_scan: false,
        };
        // Extensions that belong to this picture, then its slices.
        let mut slices = Vec::new();
        for &(code, body) in rest {
            match code {
                0xB5 => self.extension(body, Some(&mut ph))?,
                0x01..=0xAF => slices.push((code, body)),
                _ => {}
            }
        }
        let seq = self.seq.clone().unwrap_or(seq);
        // Field pictures: the second field completes the frame started by the first.
        let (mut cur, second) = match self.half.take() {
            Some((f, first)) if ph.structure != 3 && first.structure != ph.structure => (f, true),
            Some((f, first)) => {
                // An unpaired field: show what we have and start afresh.
                self.finish_frame(f, first.kind);
                (Frame::new(&seq), false)
            }
            None => (Frame::new(&seq), false),
        };
        if !second {
            cur.pts = pts;
            cur.interlaced = ph.structure != 3 || !seq.progressive;
        }
        // B pictures need both references; without them (after a seek into an open GOP) skip it.
        if kind == Kind::B && (self.refs[0].is_none() || self.refs[1].is_none()) {
            return Ok(());
        }
        // Likewise a P picture with nothing to predict from (decoding began after its I
        // picture), unless it's the second field of an I frame, which uses the first.
        if kind == Kind::P && self.refs[1].is_none() && !second {
            return Ok(());
        }
        let refs: [Option<&Frame>; 2] = match kind {
            Kind::I => [None, None],
            Kind::P => [self.refs[1].as_deref(), None],
            Kind::B => [self.refs[0].as_deref(), self.refs[1].as_deref()],
        };
        let slicer = Slicer { seq: &seq, ph: &ph, refs, second_field: second, q: fast_qmat(&[1; 64], 1.0) };
        // Slices are independent: decode them in parallel (in order-preserving chunks),
        // then write the macroblocks into the frame.
        let one = |&(code, body): &(u8, &[u8]), out: &mut Vec<Decoded>| -> std::result::Result<(), String> {
            let mut row = code as usize - 1;
            let shifted;
            let mut data = body;
            if seq.height > 2800 {
                // A 3-bit vertical position extension comes first.
                row += (BitReader::new(body).read(3) as usize) << 7;
                shifted = shift_bits(body, 3);
                data = &shifted;
            }
            decode_slice(&slicer, &cur, data, row, out).map_err(|e| format!("{kind:?} picture, structure {}, slice row {row}: {e}", ph.structure))
        };
        // Slices on the shared worker pool, a few per task; results in order.
        let threads = rayon::current_num_threads().max(1);
        let per = slices.len().div_ceil(threads * 2).max(1);
        let results: Vec<(Vec<Decoded>, Vec<String>)> = if slices.len() < 4 || threads == 1 {
            let mut out = Vec::new();
            let errs = slices.iter().filter_map(|sl| one(sl, &mut out).err()).collect();
            vec![(out, errs)]
        } else {
            use rayon::prelude::*;
            slices
                .par_chunks(per)
                .map(|chunk| {
                    let mut out = Vec::with_capacity(chunk.len() * seq.mb_width());
                    let errs = chunk.iter().filter_map(|sl| one(sl, &mut out).err()).collect();
                    (out, errs)
                })
                .collect()
        };
        let (sx, sy) = seq.chroma_shift();
        let mut errors = 0;
        for (mbs, errs) in &results {
            for mb in mbs {
                write_mb(&mut cur, ph.structure, (16 >> sx, 16 >> sy), mb);
            }
            if let (true, Some(e)) = (self.strict, errs.first()) {
                return Err(Error::Invalid(e.clone()));
            }
            errors += errs.len();
        }
        if errors > 0 && kind != Kind::B {
            // A damaged reference would spoil what follows: let FFmpeg take over.
            return invalid(format!("MPEG-2: {errors} damaged slices"));
        }
        if ph.structure != 3 && !second {
            self.half = Some((cur, ph));
        } else {
            self.finish_frame(cur, kind);
        }
        Ok(())
    }
}

/// `data` shifted left by `n` bits (for the rare slice with a vertical position extension).
fn shift_bits(data: &[u8], n: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    for i in 0..data.len() {
        let a = (data[i] as u16) << n;
        let b = *data.get(i + 1).unwrap_or(&0) as u16 >> (8 - n);
        out.push((a | b) as u8);
    }
    out
}

/// The start-code units of `data`: (code, bytes after the code up to the next one).
fn units(data: &[u8]) -> Vec<(u8, &[u8])> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 < data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            starts.push(i);
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut out = Vec::with_capacity(starts.len());
    for (k, &s) in starts.iter().enumerate() {
        let end = starts.get(k + 1).copied().unwrap_or(data.len());
        out.push((data[s + 3], &data[s + 4..end.max(s + 4)]));
    }
    out
}

impl Decoder for Mpeg2Decoder {
    fn send(&mut self, packet: &[u8], pts: i64) -> Result<()> {
        let us = units(packet);
        let mut i = 0;
        while i < us.len() {
            let (code, body) = us[i];
            match code {
                0xB3 => self.sequence_header(body)?,
                0xB5 => self.extension(body, None)?,
                0x00 => {
                    // The picture and everything up to the next picture / sequence header.
                    let mut j = i + 1;
                    while j < us.len() && !matches!(us[j].0, 0x00 | 0xB3 | 0xB8 | 0xB7) {
                        j += 1;
                    }
                    self.picture(body, &us[i + 1..j], pts)?;
                    i = j;
                    continue;
                }
                _ => {}
            }
            i += 1;
        }
        Ok(())
    }

    fn receive(&mut self) -> Option<Picture> {
        self.out.pop()
    }

    fn flush(&mut self) {
        self.out.clear();
        self.refs = [None, None];
        self.held = None;
        self.half = None;
    }

    fn finish(&mut self) {
        if let Some((f, ph)) = self.half.take() {
            self.finish_frame(f, ph.kind);
        }
        if let Some(h) = self.held.take() {
            self.emit(&h);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_build() {
        let v = vlcs();
        // "1" is an increment of 1; "011" of 2.
        let d = [0b1011_0000];
        let mut r = BitReader::new(&d);
        assert_eq!(v.mbincr.read(&mut r), Some(0));
        assert_eq!(v.mbincr.read(&mut r), Some(1));
    }

    #[test]
    fn splits_start_code_units() {
        let d = [0, 0, 1, 0xB3, 9, 9, 0, 0, 1, 0, 7];
        let u = units(&d);
        assert_eq!(u, vec![(0xB3, &[9u8, 9][..]), (0x00, &[7u8][..])]);
    }

    #[test]
    fn motion_vectors_wrap_around() {
        // code 1 ("01"), positive (sign 0), f_code 1: pred 15 + 1 wraps to -16.
        let d = [0b0100_0000];
        let mut r = BitReader::new(&d);
        assert_eq!(decode_motion(&mut r, 1, 15).unwrap(), -16);
    }
}

#[cfg(test)]
mod gen_tests {
    use super::testgen::{generate, Options};
    use super::*;

    /// Every generated stream parses without a damaged slice.
    #[test]
    fn generated_streams_parse_cleanly() {
        for (field_pictures, chroma_format) in [(true, 1), (false, 1), (true, 2), (false, 2)] {
            for seed in 0..3 {
                let data = generate(Options { field_pictures, chroma_format, seed, b_only: None }, 10);
                let mut d = Mpeg2Decoder::new(&StreamInfo::default());
                d.strict = true;
                d.send(&data, 0).unwrap_or_else(|e| panic!("fields={field_pictures} chroma={chroma_format} seed={seed}: {e}"));
            }
        }
    }
}
