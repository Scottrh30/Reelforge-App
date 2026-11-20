//! Slice data decoding: macroblock parsing (CAVLC), motion vector derivation, and reconstruction.

use crate::cavlc;
use crate::deblock;
use crate::error::{Result, ensure, invalid};
use crate::inter::{self, Weight};
use crate::intra::{self, Avail};
use crate::mbtypes::*;
use crate::params::{Pps, Sps};
use crate::picture::*;
use crate::slice::{SliceHeader, SliceType};
use crate::tables::*;
use crate::transform::{self, LevelScale};
use reelforge_bitstream::BitReader;

/// Per-slice data kept with the picture (deblocking, motion field export).
#[derive(Clone, Debug, Default)]
pub struct SliceInfo {
    pub disable_deblocking_filter_idc: u32,
    pub alpha_offset: i32,
    pub beta_offset: i32,
    /// Unique ids of the pictures in RefPicList0/1.
    pub ref_ids: [Vec<u32>; 2],
}

/// The picture under construction: private reconstruction buffers plus the shared [`Frame`] that
/// finished (deblocked) macroblock rows are published into.
pub struct PicState<P: Pel = u8> {
    pub planes: Planes<P>,
    pub mbs: Vec<MbState>,
    pub slices: Vec<SliceInfo>,
    pub mb_w: usize,
    pub mb_h: usize,
    pub poc: i32,
    pub decoded_mbs: usize,
    pub frame: FrameRef<P>,
    /// Decoded macroblocks per MB row.
    row_count: Vec<u16>,
    /// Leading MB rows that are fully reconstructed / deblocked / published.
    rows_recon: usize,
    rows_deblocked: usize,
    rows_published: usize,
    /// Draft mode for a non-reference picture: rows are published without deblocking.
    pub skip_deblock: bool,
    /// An MBAFF frame: deblocked by macroblock pairs.
    pub mbaff: bool,
    /// A field picture (deblocked as field macroblocks).
    pub field_pic: bool,
}

impl<P: Pel> PicState<P> {
    pub fn new(frame: FrameRef<P>) -> Self {
        let (mb_w, mb_h) = (frame.mb_w, frame.mb_h());
        PicState {
            planes: Planes::new(mb_w * 16, mb_h * 16, frame.fmt),
            mbs: vec![MbState::default(); mb_w * mb_h],
            slices: Vec::new(),
            mb_w,
            mb_h,
            poc: frame.poc,
            decoded_mbs: 0,
            frame,
            row_count: vec![0; mb_h],
            rows_recon: 0,
            rows_deblocked: 0,
            rows_published: 0,
            skip_deblock: false,
            mbaff: false,
            field_pic: false,
        }
    }

    /// Reuse the buffers of a finished picture of the same size for a new frame.
    pub fn reset(&mut self, frame: FrameRef<P>) {
        debug_assert_eq!((self.mb_w, self.mb_h), (frame.mb_w, frame.mb_h()));
        // start_mb() re-initialises each macroblock before use; only availability must be reset
        for m in &mut self.mbs {
            m.slice_num = u32::MAX;
        }
        self.slices.clear();
        self.poc = frame.poc;
        self.decoded_mbs = 0;
        self.frame = frame;
        self.row_count.fill(0);
        self.rows_recon = 0;
        self.rows_deblocked = 0;
        self.rows_published = 0;
        self.skip_deblock = false;
        self.mbaff = false;
        self.field_pic = false;
    }

    /// Record a decoded macroblock and deblock / publish rows that became final.
    #[inline]
    pub fn mb_done(&mut self, addr: usize) {
        self.decoded_mbs += 1;
        let r = addr / self.mb_w;
        self.row_count[r] += 1;
        if self.row_count[r] as usize >= self.mb_w && r == self.rows_recon {
            while self.rows_recon < self.mb_h && self.row_count[self.rows_recon] as usize >= self.mb_w {
                self.rows_recon += 1;
            }
            self.flush_rows(false);
        }
    }

    /// Deblock and publish rows. With `finished`, everything remaining is processed (also rows with
    /// missing macroblocks, so that waiting readers never block forever).
    pub fn flush_rows(&mut self, finished: bool) {
        // Deblocking row r must wait until row r + 1 is reconstructed (its intra prediction reads the
        // unfiltered bottom line of row r); row r is final once row r + 1 is deblocked.
        if self.mbaff {
            // by macroblock pairs, in decoding order, once the pair below is reconstructed
            let limit = if finished { self.mb_h } else { (self.rows_recon & !1).saturating_sub(2) };
            while self.rows_deblocked < limit {
                let r = self.rows_deblocked;
                if !self.skip_deblock {
                    for x in 0..self.mb_w {
                        for y in r..(r + 2).min(self.mb_h) {
                            deblock::deblock_mb_mbaff(self, y * self.mb_w + x, self.mb_w);
                        }
                    }
                }
                self.rows_deblocked = (r + 2).min(self.mb_h);
            }
        }
        let deblock_limit = if finished { self.mb_h } else { self.rows_recon.saturating_sub(1) };
        while !self.mbaff && self.rows_deblocked < deblock_limit {
            let r = self.rows_deblocked;
            if !self.skip_deblock {
                for addr in r * self.mb_w..(r + 1) * self.mb_w {
                    deblock::deblock_mb(self, addr, self.mb_w);
                }
            }
            self.rows_deblocked += 1;
        }
        let publish_limit = if finished { self.mb_h } else { self.rows_deblocked.saturating_sub(1) };
        while self.rows_published < publish_limit {
            let r = self.rows_published;
            let slices = &self.slices;
            // (field macroblocks of MBAFF frames: field ri & 1 of frame ri / 2, the same parity
            // as the macroblock (row r) for even indices)
            let ids = |st: &MbState, r: usize, l: usize, ri: i8| {
                let s = slices.get(st.slice_num as usize);
                if st.field {
                    s.and_then(|s| s.ref_ids[l].get((ri >> 1) as usize)).map_or(u32::MAX, |k| key_field(*k, (r & 1) ^ (ri & 1) as usize))
                } else {
                    s.and_then(|s| s.ref_ids[l].get(ri as usize).copied()).unwrap_or(u32::MAX)
                }
            };
            let row = Frame::make_row(&self.planes, r, &self.mbs, &ids);
            self.frame.publish(r, row);
            self.rows_published += 1;
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WpMode {
    Default,
    Explicit,
    Implicit,
}

/// Prediction weight tables of a slice.
struct Weights {
    mode: WpMode,
    luma_denom: i32,
    chroma_denom: i32,
    /// [list][ref] -> (weight, offset) for luma and two chroma components.
    luma: [Vec<(i32, i32)>; 2],
    chroma: [Vec<[(i32, i32); 2]>; 2],
    /// Implicit weights (w0, w1) indexed by ref0 * n1 + ref1.
    implicit: Vec<(i32, i32)>,
    n1: usize,
}

/// Current macroblock syntax (prediction part).
#[derive(Clone, Copy, Default)]
pub(crate) struct MbCur {
    pub info: MbTypeInfo,
    pub sub: [SubMbInfo; 4],
    /// rem_intra_pred_mode per block, or -1 when prev_intra_pred_mode_flag was set.
    pub rem_mode: [i8; 16],
    pub cbp: u8,
    pub ref_idx: [[i8; 4]; 2],
    /// mvd per (partition * 4 + sub-partition).
    pub mvd: [[[i16; 2]; 16]; 2],
}

/// Coefficient scratch for one macroblock.
pub(crate) struct Scratch<P: Pel> {
    /// Luma: 16 4x4 blocks (raster block index * 16 + raster coefficient) or 4 8x8 blocks (b8 * 64 + raster).
    pub coef: [i32; 256],
    /// Chroma: [Cb, Cr][raster block (2 wide) * 16 + raster coefficient].
    pub coef_c: [[i32; 128]; 2],
    /// Luma blocks with coefficients: raster 4x4 bits, or b8 bits in 8x8 mode.
    pub blk_nz: u16,
    /// Chroma blocks with coefficients (bit per raster block).
    pub c_nz: [u8; 2],
    pub pred: [[P; 256]; 2],
    /// Chroma predictions, stride 8 (8x8 or 8x16), or 16 (4:4:4).
    pub pred_c: [[[P; 256]; 2]; 2],
    /// 4:4:4: Cb and Cr coded as luma is: coefficients (as `coef`) and blocks with coefficients.
    pub coef444: [[i32; 256]; 2],
    pub nz444: [u16; 2],
}

impl<P: Pel> Scratch<P> {
    pub fn new() -> Box<Self> {
        let z = P::default();
        Box::new(Scratch { coef: [0; 256], coef_c: [[0; 128]; 2], blk_nz: 0, c_nz: [0; 2], pred: [[z; 256]; 2], pred_c: [[[z; 256]; 2]; 2], coef444: [[0; 256]; 2], nz444: [0; 2] })
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Nb {
    pub avail: bool,
    pub ref_idx: i8,
    pub mv: [i16; 2],
}

const NB_NA: Nb = Nb { avail: false, ref_idx: -1, mv: [0, 0] };

pub struct SliceDecoder<'a, P: Pel = u8> {
    pub sh: &'a SliceHeader,
    pub pps: &'a Pps,
    pub sps: &'a Sps,
    pub pic: &'a mut PicState<P>,
    pub refs: &'a [Vec<RefPic<P>>; 2],
    /// Sample format; QpBdOffsetY / QpBdOffsetC.
    pub fmt: Fmt,
    pub qp_off_y: i32,
    pub qp_off_c: i32,
    /// Chroma lines per MB (8 or 16) and rows of 4x4 chroma blocks (2 or 4).
    pub mbch: usize,
    pub crows: usize,
    pub ls: &'a LevelScale,
    pub slice_num: u32,
    pub mb_w: usize,
    pub mb_h: usize,
    pub qp: i32,
    pub chroma_qp_offset: [i32; 2],
    // current macroblock
    pub mb_addr: usize,
    pub mb_x: usize,
    pub mb_y: usize,
    /// Neighbouring macroblocks A (left), B (above), C (above right), D (above left) when available.
    pub nb: [Option<usize>; 4],
    pub mv_done: u16,
    pub cur: MbCur,
    pub s: Box<Scratch<P>>,
    weights: Weights,
    /// Temporal direct: DistScaleFactor per refIdxL0, and whether pic0 is long-term; for the
    /// field macroblocks of MBAFF frames (per parity) by field reference index.
    tdirect: Vec<(i32, bool)>,
    tdirect_field: [Vec<(i32, bool)>; 2],
    /// CABAC: previous MB in decoding order had a non-zero mb_qp_delta.
    pub prev_qp_delta_nz: bool,
    /// The colour plane the luma-style residual functions work on: 0, or 1 / 2 for the Cb and
    /// Cr of 4:4:4 (coded as luma is).
    pub plane: usize,
    /// TransformBypassModeFlag of the current macroblock (lossless: qpprime_y_zero_transform_bypass
    /// and QP'Y 0).
    pub bypass: bool,
    /// MBAFF frame (macroblock pairs, each frame or field coded).
    pub mbaff: bool,
    /// The current macroblock is a field macroblock, and the field decoding flag of the
    /// current pair.
    pub field: bool,
    pub pair_field: bool,
    /// A field picture: its parity (0 top, 1 bottom).
    pub pic_par: Option<usize>,
}

/// Interpolate a bw x bh luma block at (px, py) with motion vector `mv` from reference `f` into `y`,
/// and the corresponding chroma blocks into `c` (stride `cs`).
/// `read`: the field of `f` to read (field macroblocks, fields of frame-coded pictures;
/// positions are then field positions). `coff`: the 4:2:0 chroma offset (quarter samples)
/// between fields of different parity.
#[allow(clippy::too_many_arguments)]
#[inline]
fn predict_block<P: Pel>(f: &Frame<P>, read: Option<usize>, coff: i32, mv: [i16; 2], px: usize, py: usize, bw: usize, bh: usize, y: (&mut [P], usize), c: [&mut [P]; 2], cs: usize) {
    const LW: usize = 24;
    let mut win = [P::default(); LW * 21];
    let ix = px as i32 + (mv[0] as i32 >> 2);
    let iy = py as i32 + (mv[1] as i32 >> 2);
    match read {
        Some(par) => f.luma_window_field(par, ix - 2, iy - 2, bw + 5, bh + 5, &mut win, LW),
        None => f.luma_window(ix - 2, iy - 2, bw + 5, bh + 5, &mut win, LW),
    }
    inter::mc_luma_win(&win, LW, (mv[0] & 3) as u32, (mv[1] & 3) as u32, bw, bh, y.0, y.1, f.fmt.max_y());
    if f.fmt.cf == 3 {
        // 4:4:4: chroma interpolated as luma is (8.4.2.2)
        for (comp, out) in c.into_iter().enumerate() {
            match read {
                Some(par) => f.chroma_window_field(comp, par, ix - 2, iy - 2, bw + 5, bh + 5, &mut win, LW),
                None => f.chroma_window(comp, ix - 2, iy - 2, bw + 5, bh + 5, &mut win, LW),
            }
            inter::mc_luma_win(&win, LW, (mv[0] & 3) as u32, (mv[1] & 3) as u32, bw, bh, out, cs, f.fmt.max_c());
        }
        return;
    }
    let cx = (px / 2) as i32 + (mv[0] as i32 >> 3);
    let fx = (mv[0] & 7) as u32;
    // 4:2:2 chroma has full height: vertical motion in quarter samples (8.4.2.2.2).
    let (cy, ch, fy) = if f.fmt.cf == 2 {
        (py as i32 + (mv[1] as i32 >> 2), bh, ((mv[1] & 3) << 1) as u32)
    } else {
        // 4:2:0 field prediction from the other parity: chroma sits a quarter sample
        // higher or lower (Table 8-10)
        let my = mv[1] as i32 + coff;
        ((py / 2) as i32 + (my >> 3), bh / 2, (my & 7) as u32)
    };
    let cw = bw / 2;
    for (comp, out) in c.into_iter().enumerate() {
        let mut w = [P::default(); 16 * 17];
        match read {
            Some(par) => f.chroma_window_field(comp, par, cx, cy, cw + 1, ch + 1, &mut w, 16),
            None => f.chroma_window(comp, cx, cy, cw + 1, ch + 1, &mut w, 16),
        }
        inter::mc_chroma_win(&w, 16, fx, fy, cw, ch, out, cs);
    }
}

/// [`SliceDecoder::ref_field`] with the macroblock's field parity (`cur_par`, None for frame
/// macroblocks).
#[inline]
fn ref_field_of<P: Pel>(refs: &[Vec<RefPic<P>>; 2], cur_par: Option<usize>, l: usize, r: i8) -> Option<(&RefPic<P>, usize)> {
    match cur_par {
        Some(c) => Some((refs[l].get((r >> 1) as usize)?, c ^ (r & 1) as usize)),
        None => Some((refs[l].get(r as usize)?, 0)),
    }
}

/// The reference block source for reference index `r` of list `l`: the picture, the field of
/// it to read and the chroma offset (see [`predict_block`]). `mbaff_par`: the parity of a
/// field macroblock of an MBAFF frame; `pic_par`: of a field picture.
#[inline]
fn ref_src_of<P: Pel>(refs: &[Vec<RefPic<P>>; 2], mbaff_par: Option<usize>, pic_par: Option<usize>, l: usize, r: i8) -> Option<(&RefPic<P>, Option<usize>, i32)> {
    match mbaff_par {
        Some(c) => {
            let rp = refs[l].get((r >> 1) as usize)?;
            let par = c ^ (r & 1) as usize;
            Some((rp, Some(par), 2 * (c as i32 - par as i32)))
        }
        None => {
            let rp = refs[l].get(r as usize)?;
            let off = match (pic_par, rp.parity) {
                (Some(c), Some(p)) => 2 * (c as i32 - p as i32),
                _ => 0,
            };
            Some((rp, rp.read_par, off))
        }
    }
}

/// Vertical motion scaling from the co-located picture (Table 8-8).
#[derive(Clone, Copy, PartialEq, Eq)]
enum VScale {
    One,
    FrmToFld,
    FldToFrm,
}

/// Co-located motion: mvCol, refIdxCol and the key of the picture it refers to.
#[derive(Clone, Copy)]
struct Col {
    mv: [i16; 2],
    ref_idx: i8,
    key: u32,
    scale: VScale,
}

/// Add a lossless residual (`w` x `h`, row-major) to the prediction in `dst`, after the
/// residual DPCM of 8.5.15 (`dpcm` 1: down the columns, 2: along the rows).
fn add_raw<P: Pel>(r: &mut [i32], w: usize, h: usize, dpcm: u8, dst: &mut [P], stride: usize, max: i32) {
    match dpcm {
        1 => {
            for i in w..w * h {
                r[i] = r[i].wrapping_add(r[i - w]);
            }
        }
        2 => {
            for row in r.chunks_exact_mut(w) {
                for x in 1..w {
                    row[x] = row[x].wrapping_add(row[x - 1]);
                }
            }
        }
        _ => {}
    }
    for y in 0..h {
        let Some(d) = dst.get_mut(y * stride..y * stride + w) else { return };
        for (d, &v) in d.iter_mut().zip(&r[y * w..y * w + w]) {
            *d = P::p((d.i() + v).clamp(0, max));
        }
    }
}

#[inline(always)]
fn median(a: i16, b: i16, c: i16) -> i16 {
    a.max(b).min(a.min(b).max(c))
}

impl<'a, P: Pel> SliceDecoder<'a, P> {
    pub fn new(sh: &'a SliceHeader, pps: &'a Pps, sps: &'a Sps, pic: &'a mut PicState<P>, refs: &'a [Vec<RefPic<P>>; 2], ls: &'a LevelScale) -> Result<Self> {
        let fmt = pic.frame.fmt;
        ensure!(fmt == sps.fmt(), "picture format changed within a picture");
        let slice_num = pic.slices.len() as u32;
        let chroma_qp_offset = [pps.chroma_qp_index_offset, pps.second_chroma_qp_index_offset];
        pic.slices.push(SliceInfo {
            disable_deblocking_filter_idc: sh.disable_deblocking_filter_idc,
            alpha_offset: sh.slice_alpha_c0_offset_div2 * 2,
            beta_offset: sh.slice_beta_offset_div2 * 2,
            ref_ids: [refs[0].iter().map(|r| r.id()).collect(), refs[1].iter().map(|r| r.id()).collect()],
        });
        let weights = Self::build_weights(sh, pps, refs, pic.poc, fmt);
        pic.mbaff = sps.mb_adaptive_frame_field && !sh.field_pic;
        pic.field_pic = sh.field_pic;
        let mut tdirect = Vec::new();
        let mut tdirect_field = [Vec::new(), Vec::new()];
        let dsf = |cur: i32, poc0: i32, poc1: i32, long: bool| {
            let tb = cur.saturating_sub(poc0).clamp(-128, 127);
            let td = poc1.saturating_sub(poc0).clamp(-128, 127);
            if td == 0 || long {
                (256, true)
            } else {
                let tx = (16384 + (td / 2).abs()) / td;
                (((tb * tx + 32) >> 6).clamp(-1024, 1023), false)
            }
        };
        if sh.slice_type == SliceType::B && !sh.direct_spatial_mv_pred && !refs[1].is_empty() {
            let r1 = &refs[1][0];
            for r0 in refs[0].iter() {
                tdirect.push(dsf(pic.poc, r0.poc(), r1.poc(), r0.long_term));
            }
            if pic.mbaff {
                // field macroblocks: the fields of the current frame and of RefPicList1[0] of
                // the macroblock's parity, and field refIdxL0 (8.4.1.2.3)
                for (p, t) in tdirect_field.iter_mut().enumerate() {
                    for i in 0..2 * refs[0].len() {
                        let r0 = &refs[0][i >> 1];
                        t.push(dsf(pic.frame.field_poc[p], r0.field_poc[p ^ (i & 1)], r1.field_poc[p], r0.long_term));
                    }
                }
            }
        }
        let (mb_w, mb_h) = (pic.mb_w, pic.mb_h);
        Ok(SliceDecoder {
            sh,
            pps,
            sps,
            pic,
            refs,
            fmt,
            qp_off_y: 6 * (fmt.bd_y as i32 - 8),
            qp_off_c: 6 * (fmt.bd_c as i32 - 8),
            mbch: fmt.mbch(),
            crows: fmt.mbch() / 4,
            ls,
            slice_num,
            mb_w,
            mb_h,
            qp: sh.qp(pps),
            chroma_qp_offset,
            mb_addr: 0,
            mb_x: 0,
            mb_y: 0,
            nb: [None; 4],
            mv_done: 0,
            cur: MbCur::default(),
            s: Scratch::new(),
            weights,
            tdirect,
            tdirect_field,
            prev_qp_delta_nz: false,
            plane: 0,
            bypass: false,
            mbaff: sps.mb_adaptive_frame_field && !sh.field_pic,
            field: false,
            pair_field: false,
            pic_par: sh.field_pic.then_some(sh.bottom_field as usize),
        })
    }

    fn build_weights(sh: &SliceHeader, pps: &Pps, refs: &[Vec<RefPic<P>>; 2], cur_poc: i32, fmt: Fmt) -> Weights {
        // Explicit offsets are in 8-bit units (7.4.3.2: scaled by 1 << (BitDepth - 8)).
        let (sy, sc) = (fmt.bd_y as i32 - 8, fmt.bd_c as i32 - 8);
        let mut w = Weights {
            mode: WpMode::Default,
            luma_denom: 0,
            chroma_denom: 0,
            luma: [Vec::new(), Vec::new()],
            chroma: [Vec::new(), Vec::new()],
            implicit: Vec::new(),
            n1: refs[1].len(),
        };
        let explicit = (pps.weighted_pred && sh.slice_type.is_p()) || (pps.weighted_bipred_idc == 1 && sh.slice_type.is_b());
        if explicit {
            if let Some(t) = &sh.pred_weight_table {
                w.mode = WpMode::Explicit;
                w.luma_denom = t.luma_log2_denom as i32;
                w.chroma_denom = t.chroma_log2_denom as i32;
                for (l, entries) in [&t.l0, &t.l1].into_iter().enumerate() {
                    w.luma[l] = entries.iter().map(|e| (e.luma_weight, e.luma_offset << sy)).collect();
                    w.chroma[l] = entries.iter().map(|e| [(e.chroma_weight[0], e.chroma_offset[0] << sc), (e.chroma_weight[1], e.chroma_offset[1] << sc)]).collect();
                }
            }
        } else if pps.weighted_bipred_idc == 2 && sh.slice_type.is_b() {
            w.mode = WpMode::Implicit;
            for r0 in refs[0].iter() {
                for r1 in refs[1].iter() {
                    let tb = (cur_poc - r0.poc()).clamp(-128, 127);
                    let td = (r1.poc() - r0.poc()).clamp(-128, 127);
                    let pair = if td == 0 || r0.long_term || r1.long_term {
                        (32, 32)
                    } else {
                        let tx = (16384 + (td / 2).abs()) / td;
                        let dsf = ((tb * tx + 32) >> 6).clamp(-1024, 1023);
                        if (dsf >> 2) < -64 || (dsf >> 2) > 128 { (32, 32) } else { (64 - (dsf >> 2), dsf >> 2) }
                    };
                    w.implicit.push(pair);
                }
            }
        }
        w
    }

    // ------------------------------------------------------------------------------------------
    // Neighbour helpers
    // ------------------------------------------------------------------------------------------

    /// The index into the picture's macroblock state of macroblock address `addr` (in MBAFF
    /// frames addresses go down each pair before across).
    #[inline]
    pub fn idx_of(&self, addr: usize) -> usize {
        if self.mbaff {
            let pair = addr / 2;
            ((pair / self.mb_w) * 2 + (addr & 1)) * self.mb_w + pair % self.mb_w
        } else {
            addr
        }
    }

    /// The neighbouring location (xn, yn), relative to the current macroblock in a `maxw` x
    /// `maxh` component block (16x16 luma, or the chroma block): the macroblock it lies in and
    /// the location within that one (6.4.12; Table 6-4 for MBAFF frames).
    #[inline(always)]
    pub(crate) fn nbr(&self, xn: i32, yn: i32, maxw: i32, maxh: i32) -> Option<(usize, usize, usize)> {
        if yn > maxh - 1 || (xn > maxw - 1 && yn >= 0) {
            return None;
        }
        if !self.mbaff {
            let m = match (xn < 0, yn < 0, xn >= maxw) {
                (true, true, _) => self.nb[3],
                (true, false, _) => self.nb[0],
                (false, true, false) => self.nb[1],
                (false, true, true) => self.nb[2],
                _ => Some(self.mb_addr),
            }?;
            return Some((m, (xn & (maxw - 1)) as usize, (yn & (maxh - 1)) as usize));
        }
        self.nbr_mbaff(xn, yn, maxw, maxh)
    }

    /// [`Self::nbr`] in an MBAFF frame (Table 6-4).
    #[inline(never)]
    fn nbr_mbaff(&self, xn: i32, yn: i32, maxw: i32, maxh: i32) -> Option<(usize, usize, usize)> {
        let w = self.mb_w;
        // MBAFF: pairs A/B/C/D around the current pair, then the member and row (Table 6-4)
        let (px, py) = (self.mb_x as i32, (self.mb_y / 2) as i32);
        let top = self.mb_y & 1 == 0;
        let sn = self.slice_num;
        let pair = |dx: i32, dy: i32| -> Option<(usize, bool)> {
            let (qx, qy) = (px + dx, py + dy);
            if qx < 0 || qy < 0 || qx >= w as i32 {
                return None;
            }
            let t = (2 * qy as usize) * w + qx as usize;
            let st = self.pic.mbs.get(t)?;
            (st.slice_num == sn).then_some((t, st.field))
        };
        let bot = |t: usize| t + w;
        let cur_field = self.field;
        let (m, ym) = if xn < 0 {
            if yn < 0 {
                if !cur_field {
                    if top {
                        let (d, _) = pair(-1, -1)?;
                        (bot(d), yn)
                    } else {
                        let (a, af) = pair(-1, 0)?;
                        if af { (bot(a), (yn + maxh) >> 1) } else { (a, yn) }
                    }
                } else if top {
                    let (d, df) = pair(-1, -1)?;
                    if df { (d, yn) } else { (bot(d), 2 * yn) }
                } else {
                    let (d, _) = pair(-1, -1)?;
                    (bot(d), yn)
                }
            } else {
                let (a, af) = pair(-1, 0)?;
                match (cur_field, top, af) {
                    (false, true, false) => (a, yn),
                    (false, true, true) => (if yn & 1 == 0 { a } else { bot(a) }, yn >> 1),
                    (false, false, false) => (bot(a), yn),
                    (false, false, true) => (if yn & 1 == 0 { a } else { bot(a) }, (yn + maxh) >> 1),
                    (true, true, false) => if yn < maxh >> 1 { (a, yn << 1) } else { (bot(a), (yn << 1) - maxh) },
                    (true, true, true) => (a, yn),
                    (true, false, false) => if yn < maxh >> 1 { (a, (yn << 1) + 1) } else { (bot(a), (yn << 1) + 1 - maxh) },
                    (true, false, true) => (bot(a), yn),
                }
            }
        } else if xn < maxw {
            if yn < 0 {
                if !cur_field {
                    if top {
                        let (b, _) = pair(0, -1)?;
                        (bot(b), yn)
                    } else {
                        (self.mb_addr - w, yn)
                    }
                } else if top {
                    let (b, bf) = pair(0, -1)?;
                    if bf { (b, yn) } else { (bot(b), 2 * yn) }
                } else {
                    let (b, _) = pair(0, -1)?;
                    (bot(b), yn)
                }
            } else {
                (self.mb_addr, yn)
            }
        } else {
            // xn >= maxw, yn < 0
            if !cur_field {
                if !top {
                    return None;
                }
                let (c, _) = pair(1, -1)?;
                (bot(c), yn)
            } else if top {
                let (c, cf) = pair(1, -1)?;
                if cf { (c, yn) } else { (bot(c), 2 * yn) }
            } else {
                let (c, _) = pair(1, -1)?;
                (bot(c), yn)
            }
        };
        Some((m, (xn & (maxw - 1)) as usize, (ym & (maxh - 1)) as usize))
    }

    /// The macroblock containing luma location (xn, yn) relative to the current one.
    #[inline]
    fn nbr_mb(&self, xn: i32, yn: i32) -> Option<usize> {
        self.nbr(xn, yn, 16, 16).map(|n| n.0)
    }

    /// Set up the current macroblock (address `addr` in decoding order) and its neighbours.
    pub fn start_mb(&mut self, addr: usize) {
        let idx = self.idx_of(addr);
        self.mb_addr = idx;
        self.mb_x = idx % self.mb_w;
        self.mb_y = idx / self.mb_w;
        self.field = self.mbaff && self.pair_field;
        if self.mbaff {
            let sn = self.slice_num;
            self.pic.mbs[idx] = MbState { slice_num: sn, field: self.field, ..MbState::default() };
            self.mv_done = 0;
            self.nb = [self.nbr_mb(-1, 0), self.nbr_mb(0, -1), self.nbr_mb(16, -1), self.nbr_mb(-1, -1)];
            return;
        }
        let addr = idx;
        let sn = self.slice_num;
        let mbs = &self.pic.mbs;
        let w = self.mb_w;
        let (x, y) = (self.mb_x, self.mb_y);
        let ok = |a: usize| if mbs[a].slice_num == sn { Some(a) } else { None };
        self.nb = [
            if x > 0 { ok(addr - 1) } else { None },
            if y > 0 { ok(addr - w) } else { None },
            if y > 0 && x + 1 < w { ok(addr - w + 1) } else { None },
            if y > 0 && x > 0 { ok(addr - w - 1) } else { None },
        ];
        self.mv_done = 0;
        self.pic.mbs[addr] = MbState { slice_num: sn, ..MbState::default() };
    }

    #[inline]
    pub fn mb(&self) -> &MbState {
        &self.pic.mbs[self.mb_addr]
    }
    #[inline]
    pub fn mb_mut(&mut self) -> &mut MbState {
        &mut self.pic.mbs[self.mb_addr]
    }

    /// Neighbour MB usable for intra prediction (constrained_intra_pred aware).
    fn intra_nb(&self, n: usize) -> bool {
        match self.nb[n] {
            None => false,
            Some(a) => !self.pps.constrained_intra_pred || self.pic.mbs[a].kind.is_intra(),
        }
    }

    /// QPc (without QpBdOffsetC) for luma QP `qp`, 8.5.8.
    fn chroma_qp(&self, qp: i32, c: usize) -> i32 {
        let qpi = (qp + self.chroma_qp_offset[c]).clamp(-self.qp_off_c, 51);
        if qpi < 30 { qpi } else { QPC_TABLE[qpi as usize] as i32 }
    }

    /// nC for a luma 4x4 block (raster index) - 9.2.1.
    fn nc_luma(&self, raster: usize) -> i32 {
        let (bx, by) = (raster & 3, raster >> 2);
        let cur = self.mb();
        let p = self.plane;
        let at = |(m, x, y): (usize, usize, usize)| self.pic.mbs[m].nnz_of(p)[(y >> 2) * 4 + (x >> 2)];
        let a = if bx > 0 { Some(cur.nnz_of(p)[raster - 1]) } else { self.nbr(-1, by as i32 * 4, 16, 16).map(at) };
        let b = if by > 0 { Some(cur.nnz_of(p)[raster - 4]) } else { self.nbr(bx as i32 * 4, -1, 16, 16).map(at) };
        match (a, b) {
            (Some(a), Some(b)) => (a as i32 + b as i32 + 1) >> 1,
            (Some(a), None) => a as i32,
            (None, Some(b)) => b as i32,
            _ => 0,
        }
    }

    fn nc_chroma(&self, c: usize, raster: usize) -> i32 {
        let (bx, by) = (raster & 1, raster >> 1);
        let cur = self.mb();
        let h = self.mbch as i32;
        let at = |(m, x, y): (usize, usize, usize)| self.pic.mbs[m].nnz_c[c][(y >> 2) * 2 + (x >> 2)];
        let a = if bx > 0 { Some(cur.nnz_c[c][raster - 1]) } else { self.nbr(-1, by as i32 * 4, 8, h).map(at) };
        let b = if by > 0 { Some(cur.nnz_c[c][raster - 2]) } else { self.nbr(bx as i32 * 4, -1, 8, h).map(at) };
        match (a, b) {
            (Some(a), Some(b)) => (a as i32 + b as i32 + 1) >> 1,
            (Some(a), None) => a as i32,
            (None, Some(b)) => b as i32,
            _ => 0,
        }
    }

    /// Motion data of the neighbouring 4x4 block at (x, y) (4x4 units, relative to the current MB).
    /// Motion of the block containing neighbouring luma sample (xn, yn), relative to the
    /// current macroblock (exact sample positions matter in MBAFF frames).
    pub(crate) fn nb_motion(&self, list: usize, xn: i32, yn: i32) -> Nb {
        let inside = (0..16).contains(&xn) && (0..16).contains(&yn);
        let (m, bx, by) = if inside {
            let r = ((yn >> 2) * 4 + (xn >> 2)) as usize;
            if self.mv_done & (1 << r) == 0 {
                return NB_NA;
            }
            (self.mb_addr, (xn >> 2) as usize, (yn >> 2) as usize)
        } else {
            let Some((m, xw, yw)) = self.nbr(xn, yn, 16, 16) else { return NB_NA };
            (m, xw >> 2, yw >> 2)
        };
        let st = &self.pic.mbs[m];
        let b8 = (by >> 1) * 2 + (bx >> 1);
        let r = st.ref_idx[list][b8];
        if r < 0 {
            return Nb { avail: true, ref_idx: -1, mv: [0, 0] };
        }
        let mut nb = Nb { avail: true, ref_idx: r, mv: st.mv[list][by * 4 + bx] };
        // MBAFF: a neighbour coded the other way, in the current MB's units (8.4.1.3.1)
        if self.mbaff && !inside && st.field != self.field {
            if self.field {
                nb.ref_idx = r.saturating_mul(2);
                nb.mv[1] /= 2;
            } else {
                nb.ref_idx = r >> 1;
                nb.mv[1] = nb.mv[1].wrapping_mul(2);
            }
        }
        nb
    }

    /// Returns (A, B, C) neighbour motion for a partition at (x, y) of width w (4x4 units), with C
    /// replaced by D when unavailable.
    fn nb_abc(&self, list: usize, x: usize, y: usize, w: usize) -> (Nb, Nb, Nb) {
        // the partition's top-left sample and width in samples
        let (x, y, w) = (x as i32 * 4, y as i32 * 4, w as i32 * 4);
        let a = self.nb_motion(list, x - 1, y);
        let b = self.nb_motion(list, x, y - 1);
        let mut c = self.nb_motion(list, x + w, y - 1);
        if !c.avail {
            c = self.nb_motion(list, x - 1, y - 1);
        }
        (a, b, c)
    }

    /// Luma motion vector prediction (8.4.1.3). `shape`: 1 = 16x8, 2 = 8x16, 0 = other.
    #[allow(clippy::too_many_arguments)]
    fn mvp(&self, list: usize, x: usize, y: usize, w: usize, ref_idx: i8, shape: u8, part: usize) -> [i16; 2] {
        let (a, b, c) = self.nb_abc(list, x, y, w);
        if shape == 1 {
            if part == 0 && b.ref_idx == ref_idx {
                return b.mv;
            }
            if part == 1 && a.ref_idx == ref_idx {
                return a.mv;
            }
        } else if shape == 2 {
            if part == 0 && a.ref_idx == ref_idx {
                return a.mv;
            }
            if part == 1 && c.ref_idx == ref_idx {
                return c.mv;
            }
        }
        Self::median_pred(a, b, c, ref_idx)
    }

    fn median_pred(a: Nb, mut b: Nb, mut c: Nb, ref_idx: i8) -> [i16; 2] {
        if !b.avail && !c.avail && a.avail {
            b = a;
            c = a;
        }
        let ma = a.ref_idx == ref_idx;
        let mb = b.ref_idx == ref_idx;
        let mc = c.ref_idx == ref_idx;
        match (ma, mb, mc) {
            (true, false, false) => a.mv,
            (false, true, false) => b.mv,
            (false, false, true) => c.mv,
            _ => [median(a.mv[0], b.mv[0], c.mv[0]), median(a.mv[1], b.mv[1], c.mv[1])],
        }
    }

    /// Store motion for a rectangle (4x4 units) of the current MB and mark it decoded.
    fn set_motion(&mut self, x: usize, y: usize, w: usize, h: usize, refs: [i8; 2], mvs: [[i16; 2]; 2]) {
        let st = &mut self.pic.mbs[self.mb_addr];
        for yy in y..y + h {
            for xx in x..x + w {
                let r = yy * 4 + xx;
                for l in 0..2 {
                    st.mv[l][r] = if refs[l] >= 0 { mvs[l] } else { [0, 0] };
                }
                self.mv_done |= 1 << r;
            }
        }
        for l in 0..2 {
            // ref_idx is per 8x8; rectangles never straddle 8x8 blocks with different refs
            for yy in (y..y + h).step_by(1) {
                for xx in x..x + w {
                    st.ref_idx[l][(yy >> 1) * 2 + (xx >> 1)] = refs[l];
                }
            }
        }
    }

    // ------------------------------------------------------------------------------------------
    // Direct prediction (8.4.1.2)
    // ------------------------------------------------------------------------------------------

    /// The co-located motion for 4x4 block `raster` of the current macroblock (8.4.1.2.1):
    /// from the first entry of RefPicList1 (the field or frame, Table 8-6) at the macroblock
    /// and block Table 8-8 gives, with the vertical scaling between them.
    fn colocated(&self, raster: usize) -> Col {
        const NONE: Col = Col { mv: [0, 0], ref_idx: -1, key: u32::MAX, scale: VScale::One };
        let Some(col) = self.refs[1].first() else { return NONE };
        let (bx, by) = (raster & 3, raster >> 2);
        // direct_8x8_inference: the corner block of the 8x8 (in 4x4 blocks)
        let (xc, yc) = if self.sps.direct_8x8_inference { (if bx >= 2 { 3 } else { 0 }, if by >= 2 { 3 } else { 0 }) } else { (bx, by) };
        let f = &col.frame;
        if f.mb_w != self.mb_w {
            return NONE;
        }
        let x = self.mb_x;
        let pair_field = |row: usize| row < f.mb_h() && f.row(row).field[x];
        // (macroblock row in the co-located picture, 4x4 row in it, scaling)
        let (row, ym, scale) = match (self.pic_par, self.field) {
            (Some(_), _) if col.read_par.is_none() && col.parity.is_some() => (self.mb_y, yc, VScale::One),
            (Some(p), _) => {
                // a field of a frame-coded picture: the macroblock pair at this field row
                let pair = 2 * self.mb_y;
                if pair_field(pair) { (pair + p, yc, VScale::One) } else { (pair + yc / 2, (2 * yc) % 4, VScale::FrmToFld) }
            }
            (None, true) => {
                let pair = self.mb_y & !1;
                if pair_field(pair) { (self.mb_y, yc, VScale::One) } else { (pair + yc / 2, (2 * yc) % 4, VScale::FrmToFld) }
            }
            (None, false) => {
                if pair_field(self.mb_y) {
                    // over a field pair (or a frame coded as two fields): the field nearer in POC
                    let cur = self.pic.frame.poc as i64;
                    let [t, b] = col.field_poc.map(|p| p as i64);
                    let q = ((t - cur).abs() >= (b - cur).abs()) as usize;
                    ((self.mb_y & !1) + q, 2 * (self.mb_y & 1) + yc / 2, VScale::FldToFrm)
                } else {
                    (self.mb_y, yc, VScale::One)
                }
            }
        };
        if row >= f.mb_h() {
            return NONE;
        }
        let m = f.row(row);
        if m.intra[x] {
            return NONE;
        }
        let b8 = x * 4 + (ym >> 1) * 2 + (xc >> 1);
        let l = if m.ref_idx[0][b8] >= 0 { 0 } else { 1 };
        Col { mv: m.mv[l][x * 16 + ym * 4 + xc], ref_idx: m.ref_idx[l][b8], key: m.ref_id[l][b8], scale }
    }

    /// Temporal direct: the lowest index into RefPicList0 of the picture the co-located block
    /// refers to (`key`), as a field for field macroblocks and field pictures, else the frame
    /// (8.4.1.2.3). 0 when it isn't in the list.
    fn map_col_to_list0(&self, key: u32) -> i8 {
        let found = if let Some(c) = self.field.then_some(self.mb_y & 1) {
            let target = if key & 3 == 3 { key_field(key, c) } else { key };
            (0..2 * self.refs[0].len()).find(|&i| key_field(self.refs[0][i >> 1].key, c ^ (i & 1)) == target)
        } else if let Some(c) = self.pic_par {
            let target = if key & 3 == 3 { key_field(key, c) } else { key };
            self.refs[0].iter().position(|r| r.key == target)
        } else {
            let target = key_frame(key);
            self.refs[0].iter().position(|r| r.key == target)
        };
        found.map_or(0, |i| i as i8)
    }

    /// Spatial direct: MB-level reference indices, predicted mvs and directZeroPrediction (8.4.1.2.2).
    fn spatial_direct_base(&self) -> ([i8; 2], [[i16; 2]; 2], bool) {
        let mut refs = [-1i8; 2];
        let mut nbs = [(NB_NA, NB_NA, NB_NA); 2];
        let min_pos = |x: i8, y: i8| if x >= 0 && y >= 0 { x.min(y) } else { x.max(y) };
        for (l, nb) in nbs.iter_mut().enumerate() {
            *nb = self.nb_abc(l, 0, 0, 4);
            refs[l] = min_pos(nb.0.ref_idx, min_pos(nb.1.ref_idx, nb.2.ref_idx));
        }
        let mut mvs = [[0i16; 2]; 2];
        if refs[0] < 0 && refs[1] < 0 {
            return ([0, 0], mvs, true);
        }
        for l in 0..2 {
            if refs[l] >= 0 {
                let (a, b, c) = nbs[l];
                mvs[l] = Self::median_pred(a, b, c, refs[l]);
            }
        }
        (refs, mvs, false)
    }

    /// Derive direct-mode motion for the 8x8 blocks in `b8_mask` of the current MB.
    fn derive_direct(&mut self, b8_mask: u8) -> Result<()> {
        let spatial = self.sh.direct_spatial_mv_pred;
        let (base_refs, base_mvs, direct_zero) = if spatial { self.spatial_direct_base() } else { ([0, 0], [[0, 0]; 2], false) };
        let col_short_term = self.refs[1].first().map(|r| !r.long_term).unwrap_or(false);
        for b8 in 0..4 {
            if b8_mask & (1 << b8) == 0 {
                continue;
            }
            let (x8, y8) = ((b8 & 1) * 2, (b8 >> 1) * 2);
            let unit = if self.sps.direct_8x8_inference { 2 } else { 1 };
            for sub in (0..4).filter(|s| unit == 1 || *s == 0) {
                let (x, y) = (x8 + (sub & 1), y8 + (sub >> 1));
                let raster = y * 4 + x;
                let col = self.colocated(raster);
                let (refs, mvs) = if spatial {
                    let col_zero = col_short_term && col.ref_idx == 0 && (-1..=1).contains(&col.mv[0]) && (-1..=1).contains(&col.mv[1]);
                    let mut mvs = base_mvs;
                    for l in 0..2 {
                        if direct_zero || base_refs[l] < 0 || (base_refs[l] == 0 && col_zero) {
                            mvs[l] = [0, 0];
                        }
                    }
                    (base_refs, mvs)
                } else {
                    let ref0 = if col.ref_idx < 0 { 0 } else { self.map_col_to_list0(col.key) };
                    let mv_col = match col.scale {
                        VScale::One => col.mv,
                        VScale::FrmToFld => [col.mv[0], col.mv[1] / 2],
                        VScale::FldToFrm => [col.mv[0], col.mv[1].saturating_mul(2)],
                    };
                    let table = if self.field { &self.tdirect_field[self.mb_y & 1] } else { &self.tdirect };
                    let (dsf, ltr) = table.get(ref0 as usize).copied().unwrap_or((256, true));
                    let (mv0, mv1) = if ltr {
                        (mv_col, [0, 0])
                    } else {
                        let s = |c: i16| ((dsf * c as i32 + 128) >> 8) as i16;
                        let m0 = [s(mv_col[0]), s(mv_col[1])];
                        (m0, [m0[0] - mv_col[0], m0[1] - mv_col[1]])
                    };
                    ([ref0, 0], [mv0, mv1])
                };
                self.set_motion(x, y, unit, unit, refs, mvs);
            }
        }
        let st = &mut self.pic.mbs[self.mb_addr];
        st.direct8x8 |= b8_mask;
        Ok(())
    }

    // ------------------------------------------------------------------------------------------
    // Motion vector derivation for a parsed inter macroblock
    // ------------------------------------------------------------------------------------------

    /// P_Skip motion (8.4.1.1).
    pub fn derive_p_skip(&mut self) {
        let a = self.nb_motion(0, -1, 0);
        let b = self.nb_motion(0, 0, -1);
        let mv = if self.nb[0].is_none() || self.nb[1].is_none() || (a.ref_idx == 0 && a.mv == [0, 0]) || (b.ref_idx == 0 && b.mv == [0, 0]) {
            [0, 0]
        } else {
            self.mvp(0, 0, 0, 4, 0, 0, 0)
        };
        self.set_motion(0, 0, 4, 4, [0, -1], [mv, [0, 0]]);
    }

    /// Derive mvs of the current (non-skip) inter MB from `self.cur` (ref_idx, mvd).
    pub fn derive_inter_motion(&mut self) -> Result<()> {
        let info = self.cur.info;
        if info.kind == MbKind::BDirect16x16 || info.kind == MbKind::BSkip {
            return self.derive_direct(0xf);
        }
        let n = [self.refs[0].len() << self.field as u32, self.refs[1].len() << self.field as u32];
        if info.part != Part::P8x8 {
            let shape = match info.part {
                Part::P16x8 => 1,
                Part::P8x16 => 2,
                _ => 0,
            };
            for p in 0..info.part.num_parts() {
                let (x, y, w, h) = info.part.rect(p);
                let mut refs = [-1i8; 2];
                let mut mvs = [[0i16; 2]; 2];
                for l in 0..2 {
                    if info.pred[p] & (1 << l) != 0 {
                        let r = self.cur.ref_idx[l][p];
                        ensure!((r as usize) < n[l], "ref_idx {r} out of range (list {l}, size {})", n[l]);
                        let mvp = self.mvp(l, x, y, w, r, shape, p);
                        let d = self.cur.mvd[l][p * 4];
                        refs[l] = r;
                        mvs[l] = [mvp[0].wrapping_add(d[0]), mvp[1].wrapping_add(d[1])];
                    }
                }
                self.set_motion(x, y, w, h, refs, mvs);
            }
            return Ok(());
        }
        for p in 0..4 {
            let sub = self.cur.sub[p];
            let (x8, y8) = ((p & 1) * 2, (p >> 1) * 2);
            if sub.direct {
                self.derive_direct(1 << p)?;
                continue;
            }
            let mut refs = [-1i8; 2];
            for (l, r) in refs.iter_mut().enumerate() {
                if sub.pred & (1 << l) != 0 {
                    let v = self.cur.ref_idx[l][p];
                    ensure!((v as usize) < n[l], "ref_idx {v} out of range (list {l}, size {})", n[l]);
                    *r = v;
                }
            }
            for j in 0..sub.shape.num_parts() {
                let (sx, sy, w, h) = sub.shape.rect(j);
                let (x, y) = (x8 + sx, y8 + sy);
                let mut mvs = [[0i16; 2]; 2];
                for l in 0..2 {
                    if refs[l] >= 0 {
                        let mvp = self.mvp(l, x, y, w, refs[l], 0, 0);
                        let d = self.cur.mvd[l][p * 4 + j];
                        mvs[l] = [mvp[0].wrapping_add(d[0]), mvp[1].wrapping_add(d[1])];
                    }
                }
                self.set_motion(x, y, w, h, refs, mvs);
            }
        }
        Ok(())
    }

    // ------------------------------------------------------------------------------------------
    // Inter prediction
    // ------------------------------------------------------------------------------------------

    /// Field macroblocks and the macroblocks of field pictures use field scans and contexts.
    #[inline(always)]
    pub fn coded_field(&self) -> bool {
        self.field || self.pic_par.is_some()
    }

    /// The reference frame and field parity (0 top, 1 bottom) a reference index names: a frame
    /// for frame macroblocks; for field macroblocks of MBAFF frames index / 2, the same parity
    /// as the macroblock for even indices and the other for odd ones (8.4.2.1).
    fn ref_field(&self, l: usize, r: i8) -> Option<(&'a RefPic<P>, usize)> {
        ref_field_of(self.refs, self.field.then_some(self.mb_y & 1), l, r)
    }

    /// Implicit bi-prediction weights (w0, w1) from POC distances (8.4.2.3.1): of the current
    /// picture and the two references, or of fields for field macroblocks.
    fn implicit_weights(&self, refs: [i8; 2]) -> (i32, i32) {
        let w = &self.weights;
        if !self.field {
            return w.implicit.get(refs[0] as usize * w.n1 + refs[1] as usize).copied().unwrap_or((32, 32));
        }
        let (Some((r0, p0)), Some((r1, p1))) = (self.ref_field(0, refs[0]), self.ref_field(1, refs[1])) else { return (32, 32) };
        let cur = self.pic.frame.field_poc[self.mb_y & 1];
        let (poc0, poc1) = (r0.field_poc[p0], r1.field_poc[p1]);
        let tb = (cur - poc0).clamp(-128, 127);
        let td = (poc1 - poc0).clamp(-128, 127);
        if td == 0 || r0.long_term || r1.long_term {
            return (32, 32);
        }
        let tx = (16384 + (td / 2).abs()) / td;
        let dsf = ((tb * tx + 32) >> 6).clamp(-1024, 1023);
        if (dsf >> 2) < -64 || (dsf >> 2) > 128 { (32, 32) } else { (64 - (dsf >> 2), dsf >> 2) }
    }

    fn weight_for(&self, refs: [i8; 2], comp: usize) -> Weight {
        let w = &self.weights;
        match w.mode {
            WpMode::Default => Weight::Default,
            WpMode::Implicit => {
                if refs[0] >= 0 && refs[1] >= 0 {
                    let (w0, w1) = self.implicit_weights(refs);
                    Weight::Weighted { log_wd: 5, w0, w1, o0: 0, o1: 0 }
                } else {
                    Weight::Default
                }
            }
            WpMode::Explicit => {
                let get = |l: usize| -> (i32, i32) {
                    if refs[l] < 0 {
                        return (0, 0);
                    }
                    // refIdxL0WP: the frame's weights for both its fields
                    let r = (refs[l] >> self.field as u32) as usize;
                    if comp == 0 {
                        w.luma[l].get(r).copied().unwrap_or((1 << w.luma_denom, 0))
                    } else {
                        w.chroma[l].get(r).map(|c| c[comp - 1]).unwrap_or((1 << w.chroma_denom, 0))
                    }
                };
                let (w0, o0) = get(0);
                let (w1, o1) = get(1);
                let log_wd = if comp == 0 { w.luma_denom } else { w.chroma_denom };
                Weight::Weighted { log_wd, w0, w1, o0, o1 }
            }
        }
    }

    /// Motion-compensated prediction of a rectangle (4x4 units) written into the picture.
    fn mc_rect(&mut self, x4: usize, y4: usize, w4: usize, h4: usize) {
        let st = &self.pic.mbs[self.mb_addr];
        let r = y4 * 4 + x4;
        let b8 = (y4 >> 1) * 2 + (x4 >> 1);
        let refs = [st.ref_idx[0][b8], st.ref_idx[1][b8]];
        let mvs = [st.mv[0][r], st.mv[1][r]];
        let (bw, bh) = (w4 * 4, h4 * 4);
        let wts = [self.weight_for(refs, 0), self.weight_for(refs, 1), self.weight_for(refs, 2)];
        // position in the plane, or in the macroblock's field for field macroblocks
        let (ystart, ys, x0, y0) = self.view(0);
        let (cstart, cs, _, _) = self.view(1);
        let (px, py) = (x0 + x4 * 4, y0 + y4 * 4);
        let mbaff_par = self.field.then_some(self.mb_y & 1);
        let pic_par = self.pic_par;
        let use0 = refs[0] >= 0 && ref_src_of(self.refs, mbaff_par, pic_par, 0, refs[0]).is_some();
        let use1 = refs[1] >= 0 && ref_src_of(self.refs, mbaff_par, pic_par, 1, refs[1]).is_some();
        let (cx, cy, cbw, cbh) = match self.fmt.cf {
            3 => (px, py, bw, bh),
            2 => (px / 2, py, bw / 2, bh),
            _ => (px / 2, py / 2, bw / 2, bh / 2),
        };
        let cps = if self.fmt.cf == 3 { 16 } else { 8 };
        let (max_y, max_c) = (self.fmt.max_y(), self.fmt.max_c());
        if use0 != use1 && wts.iter().all(|w| *w == Weight::Default) {
            // single-list unweighted prediction: interpolate straight into the picture
            let l = if use0 { 0 } else { 1 };
            let Some((rp, read, coff)) = ref_src_of(self.refs, mbaff_par, pic_par, l, refs[l]) else { return };
            let planes = &mut self.pic.planes;
            predict_block(&rp.frame, read, coff, mvs[l], px, py, bw, bh, (&mut planes.y[ystart + py * ys + px..], ys), [&mut planes.cb[cstart + cy * cs + cx..], &mut planes.cr[cstart + cy * cs + cx..]], cs);
            return;
        }
        for l in 0..2 {
            if refs[l] < 0 {
                continue;
            }
            let Some((rp, read, coff)) = ref_src_of(self.refs, mbaff_par, pic_par, l, refs[l]) else { continue };
            let s = &mut *self.s;
            let [pc0, pc1] = &mut s.pred_c[l];
            predict_block(&rp.frame, read, coff, mvs[l], px, py, bw, bh, (&mut s.pred[l][..], 16), [&mut pc0[..], &mut pc1[..]], cps);
        }
        let s = &mut *self.s;
        inter::weighted_store(
            &mut self.pic.planes.y[ystart + py * ys + px..],
            ys,
            if use0 { Some(&s.pred[0][..]) } else { None },
            if use1 { Some(&s.pred[1][..]) } else { None },
            16,
            bw,
            bh,
            wts[0],
            max_y,
        );
        for c in 0..2 {
            let wc = wts[c + 1];
            let a = if use0 { Some(&s.pred_c[0][c][..]) } else { None };
            let b = if use1 { Some(&s.pred_c[1][c][..]) } else { None };
            let plane = if c == 0 { &mut self.pic.planes.cb } else { &mut self.pic.planes.cr };
            inter::weighted_store(&mut plane[cstart + cy * cs + cx..], cs, a, b, cps, cbw, cbh, wc, max_c);
        }
    }

    /// All 16 blocks of the current MB share references and motion vectors.
    fn uniform_motion(&self) -> bool {
        let st = self.mb();
        (0..2).all(|l| {
            let r = st.ref_idx[l];
            r[1] == r[0] && r[2] == r[0] && r[3] == r[0] && st.mv[l].iter().all(|m| *m == st.mv[l][0])
        })
    }

    /// Inter prediction for the whole current MB, using the stored motion.
    pub fn predict_inter(&mut self) {
        let info = self.cur.info;
        let direct_unit = if self.sps.direct_8x8_inference { 2 } else { 1 };
        match info.kind {
            MbKind::PSkip => self.mc_rect(0, 0, 4, 4),
            MbKind::BSkip | MbKind::BDirect16x16 if self.uniform_motion() => self.mc_rect(0, 0, 4, 4),
            MbKind::BSkip | MbKind::BDirect16x16 => {
                for y in (0..4).step_by(direct_unit) {
                    for x in (0..4).step_by(direct_unit) {
                        self.mc_rect(x, y, direct_unit, direct_unit);
                    }
                }
            }
            _ => {
                if info.part != Part::P8x8 {
                    for p in 0..info.part.num_parts() {
                        let (x, y, w, h) = info.part.rect(p);
                        self.mc_rect(x, y, w, h);
                    }
                } else {
                    for p in 0..4 {
                        let sub = self.cur.sub[p];
                        let (x8, y8) = ((p & 1) * 2, (p >> 1) * 2);
                        if sub.direct {
                            for y in (0..2).step_by(direct_unit) {
                                for x in (0..2).step_by(direct_unit) {
                                    self.mc_rect(x8 + x, y8 + y, direct_unit, direct_unit);
                                }
                            }
                        } else {
                            for j in 0..sub.shape.num_parts() {
                                let (sx, sy, w, h) = sub.shape.rect(j);
                                self.mc_rect(x8 + sx, y8 + sy, w, h);
                            }
                        }
                    }
                }
            }
        }
    }

    // ------------------------------------------------------------------------------------------
    // Intra prediction mode derivation (8.3.1.1 / 8.3.2.1)
    // ------------------------------------------------------------------------------------------

    /// The intra 4x4 / 8x8 mode of the block containing neighbouring luma location (xn, yn), or
    /// None when dcPredModePredictedFlag must be set.
    fn intra_mode_nb(&self, xn: i32, yn: i32) -> Option<u8> {
        let (m, x, y) = self.nbr(xn, yn, 16, 16)?;
        let st = &self.pic.mbs[m];
        if self.pps.constrained_intra_pred && !st.kind.is_intra() {
            return None;
        }
        Some(match st.kind {
            MbKind::I4x4 | MbKind::I8x8 => st.intra_modes[(y >> 2) * 4 + (x >> 2)],
            _ => 2,
        })
    }

    /// Resolve Intra4x4PredMode / Intra8x8PredMode for the current MB from `self.cur.rem_mode`.
    pub fn derive_intra_modes(&mut self, is8x8: bool) {
        let step = if is8x8 { 2 } else { 1 };
        let nblk = if is8x8 { 4 } else { 16 };
        for i in 0..nblk {
            let (bx, by) = if is8x8 {
                ((i & 1) * 2, (i >> 1) * 2)
            } else {
                let (x, y) = BLK4_XY[i];
                (x as usize, y as usize)
            };
            let a = if bx > 0 { Some(self.mb().intra_modes[by * 4 + bx - 1]) } else { self.intra_mode_nb(-1, by as i32 * 4) };
            let b = if by > 0 { Some(self.mb().intra_modes[(by - 1) * 4 + bx]) } else { self.intra_mode_nb(bx as i32 * 4, -1) };
            let pred = match (a, b) {
                (Some(a), Some(b)) => a.min(b),
                _ => 2,
            };
            let rem = self.cur.rem_mode[i];
            let mode = if rem < 0 {
                pred
            } else if (rem as u8) < pred {
                rem as u8
            } else {
                rem as u8 + 1
            };
            let st = self.mb_mut();
            for yy in by..by + step {
                for xx in bx..bx + step {
                    st.intra_modes[yy * 4 + xx] = mode;
                }
            }
        }
    }

    // ------------------------------------------------------------------------------------------
    // Reconstruction
    // ------------------------------------------------------------------------------------------

    /// The current macroblock in a plane: (slice start, line stride, x, y) of its top-left
    /// sample, a field macroblock seen in its own field (every other line), for luma (`c` 0)
    /// or chroma.
    #[inline]
    pub(crate) fn view(&self, c: usize) -> (usize, usize, usize, usize) {
        let (w, mbw, mbh) = if c == 0 { (self.pic.planes.width, 16, 16) } else { (self.pic.planes.cwidth, self.fmt.mbcw(), self.mbch) };
        if self.field {
            ((self.mb_y & 1) * w, 2 * w, self.mb_x * mbw, (self.mb_y >> 1) * mbh)
        } else {
            (0, w, self.mb_x * mbw, self.mb_y * mbh)
        }
    }

    /// The coefficients and coded blocks of colour plane `p` (see [`Self::plane`]).
    fn coef_of(&mut self, p: usize) -> (&mut [i32; 256], &mut u16) {
        let s = &mut *self.s;
        match p {
            0 => (&mut s.coef, &mut s.blk_nz),
            1 => (&mut s.coef444[0], &mut s.nz444[0]),
            _ => (&mut s.coef444[1], &mut s.nz444[1]),
        }
    }

    /// Plane `p` of the picture (0 luma, 1 Cb, 2 Cr), its view of the current macroblock and
    /// largest sample value.
    fn plane_mut(&mut self, p: usize) -> (&mut [P], usize, usize, usize, i32) {
        let (start, stride, x0, y0) = self.view(p.min(1));
        let max = if p == 0 { self.fmt.max_y() } else { self.fmt.max_c() };
        let planes = &mut self.pic.planes;
        let plane = match p {
            0 => &mut planes.y,
            1 => &mut planes.cb,
            _ => &mut planes.cr,
        };
        (&mut plane[start..], stride, x0, y0, max)
    }

    /// The residual DPCM of a lossless intra block (8.5.15): 1 vertical, 2 horizontal, 0 none.
    fn bypass_dpcm(&self, mode: u8) -> u8 {
        if !self.mb().kind.is_intra() {
            return 0;
        }
        match mode {
            0 => 1,
            1 => 2,
            _ => 0,
        }
    }

    fn add_luma_block4(&mut self, p: usize, raster: usize) {
        let bypass = self.bypass;
        let dpcm = if self.mb().kind == MbKind::I4x4 { self.bypass_dpcm(self.mb().intra_modes[raster]) } else { 0 };
        let (coef, nz) = self.coef_of(p);
        if *nz & (1 << raster) == 0 {
            return;
        }
        let Some(blk) = coef.get_mut(raster * 16..).and_then(|c| c.first_chunk_mut::<16>()) else {
            return;
        };
        let blk = std::mem::take(blk);
        let (plane, stride, x0, y0, max) = self.plane_mut(p);
        let o = (y0 + (raster >> 2) * 4) * stride + x0 + (raster & 3) * 4;
        let Some(dst) = plane.get_mut(o..) else { return };
        if bypass {
            let mut r = blk;
            add_raw(&mut r, 4, 4, dpcm, dst, stride, max);
        } else {
            transform::idct4_add(&blk, dst, stride, max);
        }
    }

    fn add_luma_block8(&mut self, p: usize, b8: usize) {
        let bypass = self.bypass;
        let (bx, by) = ((b8 & 1) * 2, (b8 >> 1) * 2);
        let dpcm = if self.mb().kind == MbKind::I8x8 { self.bypass_dpcm(self.mb().intra_modes[by * 4 + bx]) } else { 0 };
        let (coef, nz) = self.coef_of(p);
        if *nz & (1 << b8) == 0 {
            return;
        }
        let Some(blk) = coef.get_mut(b8 * 64..).and_then(|c| c.first_chunk_mut::<64>()) else {
            return;
        };
        let blk = std::mem::replace(blk, [0; 64]);
        let (plane, stride, x0, y0, max) = self.plane_mut(p);
        let o = (y0 + (b8 >> 1) * 8) * stride + x0 + (b8 & 1) * 8;
        let Some(dst) = plane.get_mut(o..) else { return };
        if bypass {
            let mut r = blk;
            add_raw(&mut r, 8, 8, dpcm, dst, stride, max);
        } else {
            transform::idct8_add(&blk, dst, stride, max);
        }
    }

    /// Lossless Intra16x16: the whole macroblock's residual at once (its DPCM runs across the
    /// 4x4 blocks).
    fn add_luma_bypass16(&mut self, p: usize) {
        let dpcm = self.bypass_dpcm(self.cur.info.i16_mode);
        let (coef, nz) = self.coef_of(p);
        if *nz == 0 {
            return;
        }
        let mut r = [0i32; 256];
        for b in 0..16 {
            for k in 0..16 {
                r[((b >> 2) * 4 + (k >> 2)) * 16 + (b & 3) * 4 + (k & 3)] = coef[b * 16 + k];
            }
        }
        coef.fill(0);
        *nz = 0;
        let (plane, stride, x0, y0, max) = self.plane_mut(p);
        if let Some(dst) = plane.get_mut(y0 * stride + x0..) {
            add_raw(&mut r, 16, 16, dpcm, dst, stride, max);
        }
    }

    fn add_chroma(&mut self) {
        let (start, stride, cx0, cy0) = self.view(1);
        let max = self.fmt.max_c();
        if self.bypass {
            // lossless: each component's residual at once, with the chroma mode's DPCM
            let dpcm = match (self.mb().kind.is_intra(), self.mb().intra_chroma_mode) {
                (true, 1) => 2,
                (true, 2) => 1,
                _ => 0,
            };
            let h = self.mbch;
            for c in 0..2 {
                if self.s.c_nz[c] == 0 {
                    continue;
                }
                let mut r = [0i32; 128];
                for b in 0..self.crows * 2 {
                    for k in 0..16 {
                        r[((b >> 1) * 4 + (k >> 2)) * 8 + (b & 1) * 4 + (k & 3)] = self.s.coef_c[c][b * 16 + k];
                    }
                }
                self.s.coef_c[c].fill(0);
                self.s.c_nz[c] = 0;
                let plane = if c == 0 { &mut self.pic.planes.cb } else { &mut self.pic.planes.cr };
                if let Some(dst) = plane.get_mut(start + cy0 * stride + cx0..) {
                    add_raw(&mut r[..8 * h], 8, h, dpcm, dst, stride, max);
                }
            }
            return;
        }
        for c in 0..2 {
            if self.s.c_nz[c] == 0 {
                continue;
            }
            for b in 0..self.crows * 2 {
                if self.s.c_nz[c] & (1 << b) == 0 {
                    continue;
                }
                let px = cx0 + (b & 1) * 4;
                let py = cy0 + (b >> 1) * 4;
                let Some(blk) = self.s.coef_c[c].get_mut(b * 16..).and_then(|c| c.first_chunk_mut::<16>()) else {
                    continue;
                };
                let plane = if c == 0 { &mut self.pic.planes.cb } else { &mut self.pic.planes.cr };
                transform::idct4_add(blk, &mut plane[start + py * stride + px..], stride, max);
                blk.fill(0);
            }
            self.s.c_nz[c] = 0;
        }
    }

    /// Whether the left neighbouring samples of rows `y0..y0 + n` (luma, or chroma rows) are
    /// all usable for intra prediction. In MBAFF frames with constrained intra prediction they
    /// can lie in two macroblocks, one intra and one not, so every row is checked.
    fn left_avail(&self, y0: i32, n: i32, chroma: bool) -> bool {
        if !(self.mbaff && self.pps.constrained_intra_pred) {
            return self.intra_nb(0);
        }
        let (w, h) = if chroma { (self.fmt.mbcw() as i32, self.mbch as i32) } else { (16, 16) };
        (y0..y0 + n).all(|y| self.nbr(-1, y, w, h).is_some_and(|(m, _, _)| self.pic.mbs[m].kind.is_intra()))
    }

    fn luma_avail4(&self, bx: usize, by: usize) -> Avail {
        let b = self.intra_nb(1);
        let c = self.intra_nb(2);
        let d = self.intra_nb(3);
        let left = if bx > 0 { true } else { self.left_avail(by as i32 * 4, 4, false) };
        let top = if by > 0 { true } else { b };
        let top_left = match (bx > 0, by > 0) {
            (true, true) => true,
            (false, true) => self.left_avail(by as i32 * 4 - 1, 1, false),
            (true, false) => b,
            (false, false) => d,
        };
        let top_right = if by == 0 {
            if bx < 3 { b } else { c }
        } else if bx == 3 {
            false
        } else {
            RASTER_TO_BLK4[(by - 1) * 4 + bx + 1] < RASTER_TO_BLK4[by * 4 + bx]
        };
        Avail { left, top, top_left, top_right }
    }

    /// Reconstruct an intra MB (prediction + residual) after its syntax has been parsed.
    pub fn reconstruct_intra(&mut self) {
        let kind = self.mb().kind;
        // the luma process for every colour plane of 4:4:4
        let nplanes = if self.fmt.cf == 3 { 3 } else { 1 };
        for p in 0..nplanes {
            let bd = if p == 0 { self.fmt.bd_y } else { self.fmt.bd_c } as u32;
            match kind {
                MbKind::I4x4 => {
                    for &(bx, by) in BLK4_XY.iter() {
                        let (bx, by) = (bx as usize, by as usize);
                        let av = self.luma_avail4(bx, by);
                        let mode = self.mb().intra_modes[by * 4 + bx];
                        let (plane, stride, x0, y0, _) = self.plane_mut(p);
                        intra::pred4x4(plane, stride, x0 + bx * 4, y0 + by * 4, mode, av, bd);
                        self.add_luma_block4(p, by * 4 + bx);
                    }
                }
                MbKind::I8x8 => {
                    for b8 in 0..4 {
                        let (bx, by) = ((b8 & 1) * 2, (b8 >> 1) * 2);
                        let mut av = self.luma_avail4(bx, by);
                        if bx == 0 {
                            av.left = self.left_avail(by as i32 * 4, 8, false);
                        }
                        av.top_right = match b8 {
                            0 => self.intra_nb(1),
                            1 => self.intra_nb(2),
                            2 => true,
                            _ => false,
                        };
                        let mode = self.mb().intra_modes[by * 4 + bx];
                        let (plane, stride, x0, y0, _) = self.plane_mut(p);
                        intra::pred8x8(plane, stride, x0 + bx * 4, y0 + by * 4, mode, av, bd);
                        self.add_luma_block8(p, b8);
                    }
                }
                MbKind::I16x16 => {
                    let av = Avail { left: self.left_avail(0, 16, false), top: self.intra_nb(1), top_left: self.intra_nb(3), top_right: false };
                    let mode = self.cur.info.i16_mode;
                    let (plane, stride, x0, y0, _) = self.plane_mut(p);
                    intra::pred16x16(plane, stride, x0, y0, mode, av, bd);
                    if self.bypass {
                        self.add_luma_bypass16(p);
                    } else {
                        for r in 0..16 {
                            self.add_luma_block4(p, r);
                        }
                    }
                }
                _ => {}
            }
        }
        if self.fmt.cf != 3 {
            let av = Avail { left: self.left_avail(0, self.mbch as i32, true), top: self.intra_nb(1), top_left: self.intra_nb(3), top_right: false };
            let (cstart, cs, cx, cy) = self.view(1);
            let mode = self.mb().intra_chroma_mode;
            let (bd, h) = (self.fmt.bd_c as u32, self.mbch);
            intra::pred_chroma(&mut self.pic.planes.cb[cstart..], cs, cx, cy, mode, av, bd, h);
            intra::pred_chroma(&mut self.pic.planes.cr[cstart..], cs, cx, cy, mode, av, bd, h);
            self.add_chroma();
        }
        self.s.blk_nz = 0;
        self.s.nz444 = [0; 2];
    }

    /// Add the residual of an inter MB (prediction already written).
    pub fn reconstruct_inter_residual(&mut self) {
        let nplanes = if self.fmt.cf == 3 { 3 } else { 1 };
        for p in 0..nplanes {
            if self.mb().transform_8x8 {
                for b8 in 0..4 {
                    self.add_luma_block8(p, b8);
                }
            } else if *self.coef_of(p).1 != 0 {
                for r in 0..16 {
                    self.add_luma_block4(p, r);
                }
            }
        }
        if self.fmt.cf != 3 {
            self.add_chroma();
        }
        self.s.blk_nz = 0;
        self.s.nz444 = [0; 2];
    }

    /// Bytes of an I_PCM macroblock's samples.
    pub fn pcm_bytes(&self) -> usize {
        (256 * self.fmt.bd_y as usize + 2 * self.fmt.mbcw() * self.mbch * self.fmt.bd_c as usize) / 8
    }

    /// Write I_PCM samples (`data` holds at least [`Self::pcm_bytes`] bytes: pcm_sample_luma
    /// then chroma, each BitDepth bits).
    pub fn write_pcm(&mut self, data: &[u8]) -> Result<()> {
        ensure!(data.len() >= self.pcm_bytes(), "truncated I_PCM macroblock");
        let mut r = BitReader::new(data);
        let (start, stride, x0, y0) = self.view(0);
        let (by, bc) = (self.fmt.bd_y as u32, self.fmt.bd_c as u32);
        for row in 0..16 {
            for col in 0..16 {
                let v = r.read_bits(by)? as i32;
                self.pic.planes.y[start + (y0 + row) * stride + x0 + col] = P::p(v);
            }
        }
        let (cstart, cs, cx, cy) = self.view(1);
        for c in 0..2 {
            let plane = if c == 0 { &mut self.pic.planes.cb } else { &mut self.pic.planes.cr };
            for row in 0..self.mbch {
                for col in 0..self.fmt.mbcw() {
                    let v = r.read_bits(bc)? as i32;
                    plane[cstart + (cy + row) * cs + cx + col] = P::p(v);
                }
            }
        }
        Ok(())
    }

    /// Common state for an I_PCM MB.
    pub fn finish_pcm(&mut self) {
        let qpc = [self.chroma_qp(0, 0) as i8, self.chroma_qp(0, 1) as i8];
        let st = self.mb_mut();
        st.kind = MbKind::IPcm;
        st.qp = 0;
        st.qpc = qpc;
        st.nnz = [16; 16];
        st.nnz_c = [[16; 8]; 2];
        st.nnz444 = [[16; 16]; 2];
        st.cbf_dc = 7;
        st.cbp = 0x2f;
        st.nz_mask = 0xffff;
        st.ref_idx = [[-1; 4]; 2];
    }

    /// Update QP after mb_qp_delta.
    pub fn apply_qp_delta(&mut self, delta: i32) -> Result<()> {
        let off = self.qp_off_y;
        ensure!((-(26 + off / 2)..=25 + off / 2).contains(&delta), "mb_qp_delta {delta} out of range");
        // 7.4.5: QPY wraps within -QpBdOffsetY..=51.
        self.qp = (self.qp + delta + 52 + 2 * off).rem_euclid(52 + off) - off;
        Ok(())
    }

    pub fn store_qp(&mut self) {
        let qp = self.qp;
        self.bypass = self.sps.qpprime_y_zero_transform_bypass && qp + self.qp_off_y == 0;
        let qpc = [self.chroma_qp(qp, 0) as i8, self.chroma_qp(qp, 1) as i8];
        let st = self.mb_mut();
        st.qp = qp as i8;
        st.qpc = qpc;
    }

    // ------------------------------------------------------------------------------------------
    // Residual storage helpers (shared by CAVLC and CABAC)
    // ------------------------------------------------------------------------------------------

    /// QP'Y, or QP'C of the current plane's chroma component (4:4:4).
    fn plane_qp(&self) -> i32 {
        if self.plane == 0 { self.qp + self.qp_off_y } else { self.chroma_qp(self.qp, self.plane - 1) + self.qp_off_c }
    }

    /// Store a 4x4 block of the current plane given in scan order (levels[0..16]); `ac_only`
    /// skips position 0.
    pub fn put_luma4(&mut self, raster: usize, levels: &[i32; 16], ac_only: bool) {
        let qp = self.plane_qp();
        let p = self.plane;
        let list = if self.mb().kind.is_intra() { p } else { 3 + p };
        let bypass = self.bypass;
        let scan = if self.field || self.pic_par.is_some() { &FIELD4 } else { &ZIGZAG4 };
        let ls = self.ls.ls4[list][(qp.rem_euclid(6)) as usize];
        let (coef, nz) = self.coef_of(p);
        let blk = &mut coef[raster * 16..raster * 16 + 16];
        let mut any = false;
        for (k, &lv) in levels.iter().enumerate().skip(ac_only as usize) {
            if lv != 0 {
                let q = scan[k] as usize;
                blk[q] = if bypass { lv } else { transform::scale4(lv, ls[q], qp) };
                any = true;
            }
        }
        if any {
            *nz |= 1 << raster;
            if p == 0 {
                self.pic.mbs[self.mb_addr].nz_mask |= 1 << raster;
            }
        }
    }

    /// Store an 8x8 block of the current plane given in scan order.
    pub fn put_luma8(&mut self, b8: usize, levels: &[i32; 64]) {
        let qp = self.plane_qp();
        let p = self.plane;
        let list = 2 * p + !self.mb().kind.is_intra() as usize;
        let bypass = self.bypass;
        let scan = if self.field || self.pic_par.is_some() { &FIELD8 } else { &ZIGZAG8 };
        let ls = self.ls.ls8[list][(qp.rem_euclid(6)) as usize];
        let (coef, nz) = self.coef_of(p);
        let blk = &mut coef[b8 * 64..b8 * 64 + 64];
        let mut any = false;
        for (k, &lv) in levels.iter().enumerate() {
            if lv != 0 {
                let q = scan[k] as usize;
                blk[q] = if bypass { lv } else { transform::scale8(lv, ls[q], qp) };
                any = true;
            }
        }
        if any {
            *nz |= 1 << b8;
            if p == 0 {
                let (bx, by) = ((b8 & 1) * 2, (b8 >> 1) * 2);
                let m = (0b11u16 << (by * 4 + bx)) | (0b11u16 << ((by + 1) * 4 + bx));
                self.pic.mbs[self.mb_addr].nz_mask |= m;
            }
        }
    }

    /// Intra16x16 DC levels in scan order -> scaled DC of each 4x4 block (of the current plane).
    pub fn put_luma_dc(&mut self, levels: &[i32; 16]) {
        let mut c = [0i32; 16];
        let scan = if self.field || self.pic_par.is_some() { &FIELD4 } else { &ZIGZAG4 };
        for (k, &lv) in levels.iter().enumerate() {
            c[scan[k] as usize] = lv;
        }
        let qp = self.plane_qp();
        let p = self.plane;
        if !self.bypass {
            let ls00 = self.ls.ls4[p][(qp.rem_euclid(6)) as usize][0];
            transform::luma_dc_dequant(&mut c, qp, ls00);
        }
        let (coef, nz) = self.coef_of(p);
        for (r, &dc) in c.iter().enumerate() {
            if dc != 0 {
                coef[r * 16] = dc;
                *nz |= 1 << r;
            }
        }
    }

    /// Chroma DC levels for component `c`: 4 in raster order (4:2:0) or 8 in the 4:2:2 scan.
    pub fn put_chroma_dc(&mut self, c: usize, levels: &[i32]) {
        let qpc = self.chroma_qp(self.qp, c) + self.qp_off_c;
        let list = if self.mb().kind.is_intra() { 1 + c } else { 4 + c };
        let mut dc = [0i32; 8];
        if self.fmt.cf == 2 {
            let mut m = [0i32; 8];
            for (k, &lv) in levels.iter().take(8).enumerate() {
                m[transform::CHROMA_DC_422_SCAN[k]] = lv;
            }
            let qp_dc = qpc + 3;
            let ls00 = self.ls.ls4[list][(qp_dc % 6) as usize][0];
            if !self.bypass {
                transform::chroma_dc_dequant_422(&mut m, qp_dc, ls00);
            }
            dc = m;
        } else {
            let mut v = [0i32; 4];
            for (d, &lv) in v.iter_mut().zip(levels) {
                *d = lv;
            }
            let ls00 = self.ls.ls4[list][(qpc % 6) as usize][0];
            if !self.bypass {
                transform::chroma_dc_dequant_420(&mut v, qpc, ls00);
            }
            dc[..4].copy_from_slice(&v);
        }
        for (b, &d) in dc.iter().enumerate().take(self.crows * 2) {
            if d != 0 {
                self.s.coef_c[c][b * 16] = d;
                self.s.c_nz[c] |= 1 << b;
            }
        }
    }

    /// Chroma AC levels (scan positions 1..16 in levels[1..]) for block `b` of component `c`.
    pub fn put_chroma_ac(&mut self, c: usize, b: usize, levels: &[i32; 16]) {
        let qpc = self.chroma_qp(self.qp, c) + self.qp_off_c;
        let list = if self.mb().kind.is_intra() { 1 + c } else { 4 + c };
        let ls = &self.ls.ls4[list][(qpc % 6) as usize];
        let blk = &mut self.s.coef_c[c][b * 16..b * 16 + 16];
        let mut any = false;
        let scan = if self.field || self.pic_par.is_some() { &FIELD4 } else { &ZIGZAG4 };
        for (k, &lv) in levels.iter().enumerate().skip(1) {
            if lv != 0 {
                let p = scan[k] as usize;
                blk[p] = if self.bypass { lv } else { transform::scale4(lv, ls[p], qpc) };
                any = true;
            }
        }
        if any {
            self.s.c_nz[c] |= 1 << b;
        }
    }

    // ------------------------------------------------------------------------------------------
    // CAVLC macroblock layer
    // ------------------------------------------------------------------------------------------

    /// Decode a P_Skip or B_Skip macroblock at the current address.
    pub fn decode_skip(&mut self) -> Result<()> {
        let b = self.sh.slice_type.is_b();
        self.cur = MbCur::default();
        self.cur.info.kind = if b { MbKind::BSkip } else { MbKind::PSkip };
        self.mb_mut().kind = self.cur.info.kind;
        self.store_qp();
        if b {
            self.derive_direct(0xf)?;
        } else {
            ensure!(!self.refs[0].is_empty(), "P_Skip with empty reference list");
            self.derive_p_skip();
        }
        self.predict_inter();
        self.prev_qp_delta_nz = false;
        Ok(())
    }

    fn read_te(r: &mut BitReader, range: u32) -> Result<u32> {
        Ok(if range > 1 { r.read_ue()? } else { !r.read_bit()? as u32 })
    }

    /// Parse and reconstruct one macroblock_layer() with CAVLC.
    pub fn decode_mb_cavlc(&mut self, r: &mut BitReader) -> Result<()> {
        let mb_type = r.read_ue()?;
        let st = self.sh.slice_type;
        let info = match st {
            SliceType::I | SliceType::Si => intra_mb_type(mb_type),
            SliceType::P | SliceType::Sp => {
                if mb_type < 5 {
                    p_mb_type(mb_type)
                } else {
                    intra_mb_type(mb_type - 5)
                }
            }
            SliceType::B => {
                if mb_type < 23 {
                    b_mb_type(mb_type)
                } else {
                    intra_mb_type(mb_type - 23)
                }
            }
        };
        let Some(mut info) = info else { return invalid(format!("mb_type {mb_type} out of range")) };
        self.cur = MbCur { info, ..Default::default() };
        if info.kind == MbKind::IPcm {
            r.byte_align();
            let pos = r.byte_pos();
            let n = self.pcm_bytes();
            let Some(samples) = r.data().get(pos..pos + n) else {
                return invalid("truncated I_PCM macroblock");
            };
            let samples = samples.to_vec();
            r.skip(n * 8)?;
            self.finish_pcm();
            self.write_pcm(&samples)?;
            return Ok(());
        }
        let mut transform_8x8 = false;
        if info.kind == MbKind::I4x4 && self.pps.transform_8x8_mode && r.read_flag()? {
            transform_8x8 = true;
            info.kind = MbKind::I8x8;
            self.cur.info.kind = MbKind::I8x8;
        }
        self.mb_mut().kind = info.kind;
        self.mb_mut().transform_8x8 = transform_8x8;
        // field macroblocks of MBAFF frames index the fields of the reference frames
        let n_ref = self.sh.num_ref_idx_active.map(|n| n << self.field as u32);
        match info.kind {
            MbKind::I4x4 | MbKind::I8x8 => {
                let n = if info.kind == MbKind::I8x8 { 4 } else { 16 };
                for i in 0..n {
                    self.cur.rem_mode[i] = if r.read_flag()? { -1 } else { r.read_bits(3)? as i8 };
                }
                self.derive_intra_modes(info.kind == MbKind::I8x8);
                if self.fmt.cf != 3 {
                    let m = r.read_ue()?;
                    ensure!(m <= 3, "intra_chroma_pred_mode out of range");
                    self.mb_mut().intra_chroma_mode = m as u8;
                }
            }
            MbKind::I16x16 => {
                if self.fmt.cf != 3 {
                    let m = r.read_ue()?;
                    ensure!(m <= 3, "intra_chroma_pred_mode out of range");
                    self.mb_mut().intra_chroma_mode = m as u8;
                }
            }
            MbKind::BDirect16x16 => {}
            _ => {
                if info.part == Part::P8x8 {
                    for p in 0..4 {
                        let t = r.read_ue()?;
                        let sub = if st.is_b() { b_sub_mb_type(t) } else { p_sub_mb_type(t) };
                        let Some(sub) = sub else { return invalid(format!("sub_mb_type {t} out of range")) };
                        self.cur.sub[p] = sub;
                    }
                    for l in 0..2 {
                        for p in 0..4 {
                            let sub = self.cur.sub[p];
                            if sub.direct || sub.pred & (1 << l) == 0 {
                                continue;
                            }
                            self.cur.ref_idx[l][p] = if n_ref[l] > 1 && !info.ref0 { Self::read_te(r, n_ref[l] - 1)? as i8 } else { 0 };
                        }
                    }
                    for l in 0..2 {
                        for p in 0..4 {
                            let sub = self.cur.sub[p];
                            if sub.direct || sub.pred & (1 << l) == 0 {
                                continue;
                            }
                            for j in 0..sub.shape.num_parts() {
                                let x = r.read_se()?;
                                let y = r.read_se()?;
                                self.cur.mvd[l][p * 4 + j] = [x as i16, y as i16];
                            }
                        }
                    }
                } else {
                    let np = info.part.num_parts();
                    for l in 0..2 {
                        for p in 0..np {
                            if info.pred[p] & (1 << l) != 0 {
                                self.cur.ref_idx[l][p] = if n_ref[l] > 1 { Self::read_te(r, n_ref[l] - 1)? as i8 } else { 0 };
                            }
                        }
                    }
                    for l in 0..2 {
                        for p in 0..np {
                            if info.pred[p] & (1 << l) != 0 {
                                let x = r.read_se()?;
                                let y = r.read_se()?;
                                self.cur.mvd[l][p * 4] = [x as i16, y as i16];
                            }
                        }
                    }
                }
            }
        }
        // coded_block_pattern
        let cbp = if info.kind == MbKind::I16x16 {
            info.i16_cbp
        } else if self.fmt.cf == 3 {
            // Table 9-4 for ChromaArrayType 0 or 3: luma bits only
            let code = r.read_ue()?;
            ensure!(code < 16, "coded_block_pattern out of range");
            let (i, p) = (CBP_ME_GRAY_INTRA[code as usize], CBP_ME_GRAY_INTER[code as usize]);
            if info.kind.is_intra() { i } else { p }
        } else {
            let code = r.read_ue()?;
            ensure!(code < 48, "coded_block_pattern out of range");
            let (i, p) = CBP_ME[code as usize];
            if info.kind.is_intra() { i } else { p }
        };
        let cbp = if self.fmt.cf == 3 { cbp & 15 } else { cbp };
        self.cur.cbp = cbp;
        self.mb_mut().cbp = cbp;
        if !info.kind.is_intra() && cbp & 15 != 0 && self.pps.transform_8x8_mode && self.no_sub_8x8_lt() {
            let t = r.read_flag()?;
            self.mb_mut().transform_8x8 = t;
        }
        if !info.kind.is_intra() {
            self.derive_inter_motion()?;
        }
        if cbp != 0 || info.kind == MbKind::I16x16 {
            let d = r.read_se()?;
            self.apply_qp_delta(d)?;
        }
        self.store_qp();
        self.residual_cavlc(r, cbp)?;
        if info.kind.is_intra() {
            self.reconstruct_intra();
        } else {
            self.predict_inter();
            self.reconstruct_inter_residual();
        }
        Ok(())
    }

    /// Whether transform_size_8x8_flag may be present for an inter MB (NoSubMbPartSizeLessThan8x8 etc.).
    pub fn no_sub_8x8_lt(&self) -> bool {
        let info = self.cur.info;
        match info.kind {
            MbKind::BDirect16x16 => self.sps.direct_8x8_inference,
            MbKind::Inter if info.part == Part::P8x8 => {
                self.cur.sub.iter().all(|s| if s.direct { self.sps.direct_8x8_inference } else { s.shape == SubPart::S8x8 })
            }
            _ => true,
        }
    }

    fn residual_cavlc(&mut self, r: &mut BitReader, cbp: u8) -> Result<()> {
        // residual_luma() for luma, and for Cb and Cr of 4:4:4
        for p in 0..if self.fmt.cf == 3 { 3 } else { 1 } {
            self.plane = p;
            let res = self.residual_luma_cavlc(r, cbp);
            self.plane = 0;
            res?;
        }
        if self.fmt.cf == 3 {
            return Ok(());
        }
        self.residual_chroma_cavlc(r, cbp)
    }

    fn residual_luma_cavlc(&mut self, r: &mut BitReader, cbp: u8) -> Result<()> {
        let kind = self.mb().kind;
        let t8 = self.mb().transform_8x8;
        let p = self.plane;
        if kind == MbKind::I16x16 {
            let nc = self.nc_luma(0);
            let mut lv = [0i32; 16];
            cavlc::residual_block(r, &mut lv, 0, 15, 16, nc)?;
            self.put_luma_dc(&lv);
        }
        for b8 in 0..4 {
            let (bx8, by8) = ((b8 & 1) * 2, (b8 >> 1) * 2);
            if cbp & (1 << b8) == 0 {
                continue;
            }
            if t8 {
                let mut lv8 = [0i32; 64];
                for i4 in 0..4 {
                    let raster = (by8 + (i4 >> 1)) * 4 + bx8 + (i4 & 1);
                    let nc = self.nc_luma(raster);
                    let mut lv = [0i32; 16];
                    let tc = cavlc::residual_block(r, &mut lv, 0, 15, 16, nc)?;
                    self.mb_mut().nnz_of_mut(p)[raster] = tc;
                    for (i, &v) in lv.iter().enumerate() {
                        lv8[4 * i + i4] = v;
                    }
                }
                self.put_luma8(b8, &lv8);
            } else {
                for i4 in 0..4 {
                    let raster = (by8 + (i4 >> 1)) * 4 + bx8 + (i4 & 1);
                    let nc = self.nc_luma(raster);
                    let mut lv = [0i32; 16];
                    let tc = if kind == MbKind::I16x16 {
                        cavlc::residual_block(r, &mut lv[1..], 0, 14, 15, nc)?
                    } else {
                        cavlc::residual_block(r, &mut lv, 0, 15, 16, nc)?
                    };
                    self.mb_mut().nnz_of_mut(p)[raster] = tc;
                    self.put_luma4(raster, &lv, kind == MbKind::I16x16);
                }
            }
        }
        Ok(())
    }

    fn residual_chroma_cavlc(&mut self, r: &mut BitReader, cbp: u8) -> Result<()> {
        let cbp_c = cbp >> 4;
        let nblk = self.crows * 2;
        if cbp_c & 3 != 0 {
            for c in 0..2 {
                let mut lv = [0i32; 8];
                if nblk == 8 {
                    cavlc::residual_block(r, &mut lv, 0, 7, 8, -2)?;
                } else {
                    cavlc::residual_block(r, &mut lv, 0, 3, 4, -1)?;
                }
                self.put_chroma_dc(c, &lv[..nblk]);
            }
        }
        if cbp_c & 2 != 0 {
            for c in 0..2 {
                for b in 0..nblk {
                    let nc = self.nc_chroma(c, b);
                    let mut lv = [0i32; 16];
                    let tc = cavlc::residual_block(r, &mut lv[1..], 0, 14, 15, nc)?;
                    self.mb_mut().nnz_c[c][b] = tc;
                    self.put_chroma_ac(c, b, &lv);
                }
            }
        }
        Ok(())
    }

    /// MBAFF: the field decoding flag inferred for a pair that doesn't code it (7.4.4): the left
    /// pair's, else the above pair's, else frame.
    pub fn infer_pair_field(&mut self, addr: usize) {
        let idx = self.idx_of(addr);
        let (x, y) = (idx % self.mb_w, idx / self.mb_w);
        let sn = self.slice_num;
        let w = self.mb_w;
        let pair = |t: Option<usize>| t.and_then(|t| self.pic.mbs.get(t)).filter(|st| st.slice_num == sn).map(|st| st.field);
        let left = pair((x > 0).then(|| y * w + x - 1));
        let above = pair((y >= 2).then(|| (y - 2) * w + x));
        self.pair_field = left.or(above).unwrap_or(false);
    }

    /// Decode slice_data() with CAVLC. `r` is positioned after the slice header.
    pub fn decode_cavlc(&mut self, r: &mut BitReader) -> Result<()> {
        let total = self.mb_w * self.mb_h;
        let mbaff = self.mbaff;
        let mut addr = self.sh.first_mb_in_slice as usize * (1 + mbaff as usize);
        let intra_slice = self.sh.slice_type.is_intra();
        loop {
            ensure!(addr < total, "macroblock address {addr} out of range");
            if !intra_slice {
                let run = r.read_ue()? as usize;
                ensure!(addr + run <= total, "mb_skip_run out of range");
                for i in 0..run {
                    if mbaff && addr & 1 == 0 {
                        self.infer_pair_field(addr);
                        // the last skipped MB of a run at the top of a pair: the bottom one is
                        // coded, and its field decoding flag (the pair's) comes first
                        if i + 1 == run && r.more_rbsp_data() {
                            self.pair_field = r.read_flag()?;
                        }
                    }
                    self.start_mb(addr);
                    self.decode_skip()?;
                    self.pic.mb_done(self.mb_addr);
                    addr += 1;
                }
                if run > 0 && !r.more_rbsp_data() {
                    break;
                }
                ensure!(addr < total, "macroblock address {addr} out of range");
            }
            if mbaff && addr & 1 == 0 {
                self.pair_field = r.read_flag()?;
            }
            self.start_mb(addr);
            self.decode_mb_cavlc(r)?;
            self.pic.mb_done(self.mb_addr);
            addr += 1;
            if !r.more_rbsp_data() {
                break;
            }
        }
        Ok(())
    }
}
