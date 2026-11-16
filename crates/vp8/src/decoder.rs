//! Frame decoding: header, modes, residuals, reconstruction, loop filter and reference
//! updates (RFC 6386).

use crate::boolcoder::BoolDecoder;
use crate::header::{self, Header, Persist, Probs};
use crate::loopfilter::{self, Strength};
use crate::modes::{self, B_PRED, Grid, INTRA, MbInfo, ModeCtx, SPLITMV, ZEROMV};
use crate::recon::{self, Filter, Plane};
use crate::residual::{self, Dequant, TYPE_UV, TYPE_Y_AFTER_Y2, TYPE_Y_WITH_DC, TYPE_Y2};
use crate::{Error, Picture};

const LAST: usize = modes::LAST as usize;
const GOLDEN: usize = modes::GOLDEN as usize;
const ALTREF: usize = modes::ALTREF as usize;
use std::sync::Arc;

/// The largest frame accepted (8192 x 8192): VP8's own limit is 16383 x 16383, far beyond any
/// real stream, and that much memory per reference frame isn't worth risking on bad data.
const MAX_SIDE: usize = 8192;

struct Frame {
    planes: [Plane; 3],
}

/// A VP8 decoder. Feed it one frame (container sample) at a time.
pub struct Decoder {
    probs: Probs,
    persist: Persist,
    grid: Grid,
    width: usize,
    height: usize,
    /// Indexed by reference frame (LAST, GOLDEN, ALTREF; 0 unused).
    refs: [Option<Arc<Frame>>; 4],
    sign_bias: [bool; 4],
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}

/// Per-column (above) and per-row (left) "has coefficients" flags: 4 Y, 2 U, 2 V, Y2.
type Nz = [u8; 9];

impl Decoder {
    pub fn new() -> Self {
        Decoder { probs: Probs::default(), persist: Persist::default(), grid: Grid::new(0, 0), width: 0, height: 0, refs: [None, None, None, None], sign_bias: [false; 4] }
    }

    /// Forget all state: the next frame must be a key frame.
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    /// Decode one frame. Returns its picture when the frame is meant to be shown (hidden
    /// "alternate reference" frames only update references).
    pub fn decode(&mut self, data: &[u8], pts: i64) -> Result<Option<Picture>, Error> {
        let tag = header::frame_tag(data)?;
        if tag.key {
            let (w, h) = (usize::from(tag.width), usize::from(tag.height));
            if w > MAX_SIDE || h > MAX_SIDE {
                return Err(Error::Unsupported("frame larger than 8192 x 8192"));
            }
            let (cols, rows) = (w.div_ceil(16), h.div_ceil(16));
            if cols != self.grid.cols || rows != self.grid.rows {
                self.grid = Grid::new(cols, rows);
            }
            self.width = w;
            self.height = h;
            self.probs = Probs::default();
            self.persist = Persist::default();
            self.sign_bias = [false; 4];
        } else if self.refs[LAST].is_none() {
            return Err(Error::NoKeyframe);
        }
        let start = tag.header_len;
        let end = start.checked_add(tag.first_part_size).filter(|&e| e <= data.len()).ok_or(Error::Invalid("first partition runs past the frame"))?;
        let first = data.get(start..end).ok_or(Error::Invalid("first partition runs past the frame"))?;
        let mut bd = BoolDecoder::new(first);
        let mut saved = None;
        let h = header::read(&mut bd, tag.key, &mut self.probs, &mut self.persist, &mut saved);
        if !tag.key {
            self.sign_bias[GOLDEN] = h.sign_bias_golden;
            self.sign_bias[ALTREF] = h.sign_bias_alt;
        }

        // Token partitions.
        let rest = data.get(end..).unwrap_or(&[]);
        let n = h.partitions.max(1);
        let table = 3 * (n - 1);
        if rest.len() < table {
            return Err(Error::Invalid("token partition sizes cut short"));
        }
        let mut parts = Vec::with_capacity(n);
        let mut pos = table;
        for i in 0..n {
            let len = if i + 1 < n {
                let s = rest.get(3 * i..3 * i + 3).unwrap_or(&[0, 0, 0]);
                usize::from(s[0]) | usize::from(s[1]) << 8 | usize::from(s[2]) << 16
            } else {
                rest.len().saturating_sub(pos)
            };
            let e = pos.saturating_add(len).min(rest.len());
            parts.push(BoolDecoder::new(rest.get(pos..e).unwrap_or(&[])));
            pos = e;
        }

        // Modes and vectors of every macroblock.
        let cx = ModeCtx { h: &h, probs: &self.probs, key: tag.key, sign_bias: self.sign_bias };
        for r in 0..self.grid.rows {
            for c in 0..self.grid.cols {
                modes::read_mb(&mut bd, &cx, &mut self.grid, r, c);
            }
        }

        let frame = self.reconstruct(&h, tag.key, tag.version, &mut parts)?;

        if let Some(p) = saved {
            self.probs = p;
        }
        let frame = Arc::new(frame);
        let pic = tag.show.then(|| self.picture(&frame, pts, tag.key));
        if !tag.key {
            // As libvpx: the alternate reference first, then golden (from the new alternate).
            match h.copy_to_alt {
                1 => self.refs[ALTREF] = self.refs[LAST].clone(),
                2 => self.refs[ALTREF] = self.refs[GOLDEN].clone(),
                _ => {}
            }
            match h.copy_to_golden {
                1 => self.refs[GOLDEN] = self.refs[LAST].clone(),
                2 => self.refs[GOLDEN] = self.refs[ALTREF].clone(),
                _ => {}
            }
        }
        if h.refresh_golden {
            self.refs[GOLDEN] = Some(frame.clone());
        }
        if h.refresh_alt {
            self.refs[ALTREF] = Some(frame.clone());
        }
        if h.refresh_last {
            self.refs[LAST] = Some(frame);
        }
        Ok(pic)
    }

    fn picture(&self, f: &Frame, pts: i64, key: bool) -> Picture {
        let (w, h) = (self.width, self.height);
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        let crop = |p: &Plane, w: usize, h: usize| {
            let mut out = Vec::with_capacity(w * h);
            for y in 0..h {
                let o = y * p.width;
                out.extend_from_slice(p.data.get(o..o + w).unwrap_or(&[]));
            }
            out
        };
        Picture {
            width: w as u32,
            height: h as u32,
            chroma_width: cw as u32,
            chroma_height: ch as u32,
            y: crop(&f.planes[0], w, h),
            u: crop(&f.planes[1], cw, ch),
            v: crop(&f.planes[2], cw, ch),
            y_stride: w,
            uv_stride: cw,
            pts,
            key,
        }
    }

    fn reconstruct(&mut self, h: &Header, key: bool, version: u8, parts: &mut [BoolDecoder]) -> Result<Frame, Error> {
        let (cols, rows) = (self.grid.cols, self.grid.rows);
        let mut planes = [Plane::new(cols * 16, rows * 16), Plane::new(cols * 8, rows * 8), Plane::new(cols * 8, rows * 8)];
        let dq: [Dequant; 4] = std::array::from_fn(|s| {
            let q = if h.segmentation {
                let v = self.persist.seg_quant[s];
                if self.persist.seg_abs { v } else { h.q_index + v }
            } else {
                h.q_index
            };
            Dequant::new(q.clamp(0, 127), h.y1_dc, h.y2_dc, h.y2_ac, h.uv_dc, h.uv_ac)
        });
        let filter = if version == 0 { Filter::SixTap } else { Filter::Bilinear };
        let full_pixel = version == 3;
        let refs = self.refs.clone();
        let mut above: Vec<Nz> = vec![[0; 9]; cols];
        let mut inner = vec![false; cols * rows];
        let n = parts.len().max(1);
        for r in 0..rows {
            let mut left: Nz = [0; 9];
            let Some(bd) = parts.get_mut(r % n) else { return Err(Error::Invalid("no token partition")) };
            for c in 0..cols {
                let mb = *self.grid.at(r, c);
                let seg = if h.segmentation { usize::from(mb.segment & 3) } else { 0 };
                let Some(a) = above.get_mut(c) else { continue };
                let res = residuals(bd, &self.probs, &mb, &dq[seg], a, &mut left);
                if let Some(f) = inner.get_mut(r * cols + c) {
                    *f = mb.ymode == B_PRED || mb.ymode == SPLITMV || res.is_some();
                }
                if mb.ref_frame == INTRA {
                    intra_mb(&mut planes, &mb, r, c, cols, res.as_ref());
                } else {
                    let Some(rf) = refs.get(usize::from(mb.ref_frame)).and_then(|f| f.as_ref()) else {
                        return Err(Error::Invalid("reference frame missing"));
                    };
                    inter_mb(&mut planes, &rf.planes, &mb, r, c, filter, full_pixel);
                    if let Some(res) = &res {
                        add_residual(&mut planes, res, r, c);
                    }
                }
            }
        }
        if h.filter_level != 0 {
            let mut cache: Vec<Option<Strength>> = vec![None; 64];
            for r in 0..rows {
                for c in 0..cols {
                    let mb = self.grid.at(r, c);
                    let level = self.filter_level(h, mb);
                    if level == 0 {
                        continue;
                    }
                    let st = match cache.get(level as usize).copied().flatten() {
                        Some(s) => s,
                        None => {
                            let s = Strength::new(level, h.sharpness, key);
                            if let Some(slot) = cache.get_mut(level as usize) {
                                *slot = Some(s);
                            }
                            s
                        }
                    };
                    let inn = inner.get(r * cols + c).copied().unwrap_or(true);
                    loopfilter::filter_mb(&mut planes, c, r, &st, h.simple_filter, inn);
                }
            }
        }
        Ok(Frame { planes })
    }

    fn filter_level(&self, h: &Header, mb: &MbInfo) -> i32 {
        let mut lvl = h.filter_level;
        if h.segmentation {
            let v = self.persist.seg_lf[usize::from(mb.segment & 3)];
            lvl = if self.persist.seg_abs { v } else { lvl + v };
            lvl = lvl.clamp(0, 63);
        }
        if h.lf_delta {
            lvl += self.persist.ref_lf_delta[usize::from(mb.ref_frame & 3)];
            if mb.ref_frame == INTRA {
                if mb.ymode == B_PRED {
                    lvl += self.persist.mode_lf_delta[0];
                }
            } else if mb.ymode == ZEROMV {
                lvl += self.persist.mode_lf_delta[1];
            } else if mb.ymode == SPLITMV {
                lvl += self.persist.mode_lf_delta[3];
            } else {
                lvl += self.persist.mode_lf_delta[2];
            }
            lvl = lvl.clamp(0, 63);
        }
        lvl
    }
}

/// One macroblock's dequantised coefficients: 16 Y blocks (DC already from the Y2 transform
/// where there is one), then 4 U and 4 V.
struct Residual {
    blocks: [[i16; 16]; 24],
}

/// Decode a macroblock's tokens. None when it has no coefficients at all.
fn residuals(bd: &mut BoolDecoder, probs: &Probs, mb: &MbInfo, dq: &Dequant, above: &mut Nz, left: &mut Nz) -> Option<Residual> {
    let has_y2 = mb.ymode != B_PRED && mb.ymode != SPLITMV;
    if mb.skip {
        above[..8].fill(0);
        left[..8].fill(0);
        if has_y2 {
            above[8] = 0;
            left[8] = 0;
        }
        return None;
    }
    let mut res = Residual { blocks: [[0; 16]; 24] };
    let mut total = 0usize;
    let mut dc = None;
    if has_y2 {
        let mut y2 = [0i16; 16];
        let ctx = usize::from(above[8] + left[8]);
        let eob = residual::block(bd, &probs.coef[TYPE_Y2], 0, ctx, dq.y2, &mut y2);
        let f = u8::from(eob > 0);
        above[8] = f;
        left[8] = f;
        total += eob;
        dc = Some(recon::iwht(&y2));
    }
    let (ty, first) = if has_y2 { (TYPE_Y_AFTER_Y2, 1) } else { (TYPE_Y_WITH_DC, 0) };
    for by in 0..4 {
        for bx in 0..4 {
            let b = by * 4 + bx;
            let ctx = usize::from(above[bx] + left[by]);
            let eob = residual::block(bd, &probs.coef[ty], first, ctx, dq.y1, &mut res.blocks[b]);
            let f = u8::from(eob > first);
            above[bx] = f;
            left[by] = f;
            total += eob - first;
        }
    }
    for (plane, base) in [(0usize, 16usize), (1, 20)] {
        for by in 0..2 {
            for bx in 0..2 {
                let (ai, li) = (4 + 2 * plane + bx, 4 + 2 * plane + by);
                let ctx = usize::from(above[ai] + left[li]);
                let eob = residual::block(bd, &probs.coef[TYPE_UV], 0, ctx, dq.uv, &mut res.blocks[base + by * 2 + bx]);
                let f = u8::from(eob > 0);
                above[ai] = f;
                left[li] = f;
                total += eob;
            }
        }
    }
    if total == 0 {
        return None;
    }
    if let Some(dc) = dc {
        for (b, &d) in res.blocks.iter_mut().zip(dc.iter()) {
            b[0] = d;
        }
    }
    Some(res)
}

fn add_block(p: &mut Plane, c: &[i16; 16], x: usize, y: usize) {
    if c.iter().skip(1).all(|&v| v == 0) {
        if c[0] != 0 {
            recon::dc_add(c[0], &mut p.data, y * p.width + x, p.width);
        }
    } else {
        recon::idct_add(c, &mut p.data, y * p.width + x, p.width);
    }
}

fn add_residual(planes: &mut [Plane; 3], res: &Residual, r: usize, c: usize) {
    let (x0, y0) = (c * 16, r * 16);
    for b in 0..16 {
        add_block(&mut planes[0], &res.blocks[b], x0 + 4 * (b & 3), y0 + 4 * (b >> 2));
    }
    add_chroma(planes, res, r, c);
}

fn add_chroma(planes: &mut [Plane; 3], res: &Residual, r: usize, c: usize) {
    let (x0, y0) = (c * 8, r * 8);
    for (k, plane) in [(16usize, 1usize), (20, 2)] {
        for b in 0..4 {
            add_block(&mut planes[plane], &res.blocks[k + b], x0 + 4 * (b & 1), y0 + 4 * (b >> 1));
        }
    }
}

fn intra_mb(planes: &mut [Plane; 3], mb: &MbInfo, r: usize, c: usize, cols: usize, res: Option<&Residual>) {
    let last_col = c + 1 == cols;
    let (x0, y0) = (c * 16, r * 16);
    let e = recon::edges(&planes[0], x0, y0, 16, last_col);
    if mb.ymode == B_PRED {
        let p = &mut planes[0];
        for b in 0..16 {
            let (bx, by) = (b & 3, b >> 2);
            let (x, y) = (x0 + 4 * bx, y0 + 4 * by);
            let mut a = [0u8; 8];
            let mut l = [0u8; 4];
            for i in 0..4 {
                a[i] = if by == 0 { e.above[1 + 4 * bx + i] } else { p.at(x + i, y - 1) };
                l[i] = if bx == 0 { e.left[4 * by + i] } else { p.at(x - 1, y + i) };
            }
            for i in 0..4 {
                a[4 + i] = if bx == 3 || by == 0 { e.above[1 + 4 * bx + 4 + i] } else { p.at(x + 4 + i, y - 1) };
            }
            let tl = match (bx, by) {
                (0, 0) => e.above[0],
                (_, 0) => e.above[4 * bx],
                (0, _) => e.left[4 * by - 1],
                _ => p.at(x - 1, y - 1),
            };
            let pred = recon::predict_sub(mb.bmodes[b], &a, &l, tl);
            for (row, chunk) in pred.chunks_exact(4).enumerate() {
                let o = (y + row) * p.width + x;
                if let Some(d) = p.data.get_mut(o..o + 4) {
                    d.copy_from_slice(chunk);
                }
            }
            if let Some(res) = res {
                add_block(p, &res.blocks[b], x, y);
            }
        }
    } else {
        recon::predict_block(&mut planes[0], x0, y0, 16, mb.ymode, &e);
        if let Some(res) = res {
            for b in 0..16 {
                add_block(&mut planes[0], &res.blocks[b], x0 + 4 * (b & 3), y0 + 4 * (b >> 2));
            }
        }
    }
    for plane in 1..3 {
        let ce = recon::edges(&planes[plane], c * 8, r * 8, 8, last_col);
        recon::predict_block(&mut planes[plane], c * 8, r * 8, 8, mb.uv_mode, &ce);
    }
    if let Some(res) = res {
        add_chroma(planes, res, r, c);
    }
}

fn inter_mb(planes: &mut [Plane; 3], refs: &[Plane; 3], mb: &MbInfo, r: usize, c: usize, filter: Filter, full_pixel: bool) {
    let (x0, y0) = ((c * 16) as i32, (r * 16) as i32);
    let (cx0, cy0) = ((c * 8) as i32, (r * 8) as i32);
    let full = |v: i32| if full_pixel { v & !7 } else { v };
    let mut put = |plane: usize, x: i32, y: i32, mv_col: i32, mv_row: i32, w: usize, hh: usize| {
        let (dst, src) = (&mut planes[plane], &refs[plane]);
        let (dx, dy) = (x + (mv_col >> 3), y + (mv_row >> 3));
        let stride = dst.width;
        let off = y as usize * stride + x as usize;
        recon::predict_inter(src, dx, dy, (mv_col & 7) as usize, (mv_row & 7) as usize, w, hh, filter, &mut dst.data, off, stride);
    };
    if mb.ymode != SPLITMV {
        let (row, col) = (i32::from(mb.mv.row), i32::from(mb.mv.col));
        put(0, x0, y0, col, row, 16, 16);
        // Chroma: half the luma vector, rounded away from zero, in eighths of a chroma sample.
        let half = |v: i32| full((v + if v < 0 { -1 } else { 1 }) / 2);
        let (cr, cc) = (half(row), half(col));
        put(1, cx0, cy0, cc, cr, 8, 8);
        put(2, cx0, cy0, cc, cr, 8, 8);
        return;
    }
    for (b, mv) in mb.bmvs.iter().enumerate() {
        let (bx, by) = ((b & 3) as i32, (b >> 2) as i32);
        put(0, x0 + 4 * bx, y0 + 4 * by, i32::from(mv.col), i32::from(mv.row), 4, 4);
    }
    // Each 4x4 chroma block takes the average vector of the 2x2 luma blocks it covers.
    for j in 0..2usize {
        for i in 0..2usize {
            let k = j * 8 + i * 2;
            let blocks = [k, k + 1, k + 4, k + 5];
            let avg = |f: fn(&modes::Mv) -> i16| {
                let s: i32 = blocks.iter().map(|&b| i32::from(f(&mb.bmvs[b]))).sum();
                full((s + if s < 0 { -4 } else { 4 }) / 8)
            };
            let (row, col) = (avg(|m| m.row), avg(|m| m.col));
            let (x, y) = (cx0 + 4 * i as i32, cy0 + 4 * j as i32);
            put(1, x, y, col, row, 4, 4);
            put(2, x, y, col, row, 4, 4);
        }
    }
}
