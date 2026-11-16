//! Inverse transforms (RFC 6386 section 14), intra prediction (section 12) and inter
//! prediction filters (section 18), on planes without borders: samples outside a plane are its
//! nearest edge sample.

/// A plane of 8-bit samples, a whole number of macroblocks in size.
#[derive(Clone)]
pub struct Plane {
    pub data: Vec<u8>,
    pub width: usize,
    pub height: usize,
}

impl Plane {
    pub fn new(width: usize, height: usize) -> Self {
        Plane { data: vec![0; width * height], width, height }
    }
    #[inline]
    pub fn at(&self, x: usize, y: usize) -> u8 {
        self.data.get(y * self.width + x).copied().unwrap_or(0)
    }
    /// The sample at (x, y) with coordinates clamped into the plane.
    #[inline]
    pub fn clamped(&self, x: i32, y: i32) -> u8 {
        let x = x.clamp(0, self.width as i32 - 1) as usize;
        let y = y.clamp(0, self.height as i32 - 1) as usize;
        self.at(x, y)
    }
}

// ---- Transforms --------------------------------------------------------------------------

/// Inverse Walsh-Hadamard transform of the Y2 block: the DC of each of the 16 luma blocks.
pub fn iwht(input: &[i16; 16]) -> [i16; 16] {
    let mut t = [0i16; 16];
    for i in 0..4 {
        let (i0, i4, i8, i12) = (i32::from(input[i]), i32::from(input[4 + i]), i32::from(input[8 + i]), i32::from(input[12 + i]));
        let a1 = i0 + i12;
        let b1 = i4 + i8;
        let c1 = i4 - i8;
        let d1 = i0 - i12;
        t[i] = (a1 + b1) as i16;
        t[4 + i] = (c1 + d1) as i16;
        t[8 + i] = (a1 - b1) as i16;
        t[12 + i] = (d1 - c1) as i16;
    }
    let mut out = [0i16; 16];
    for r in 0..4 {
        let ip = &t[r * 4..r * 4 + 4];
        let (i0, i1, i2, i3) = (i32::from(ip[0]), i32::from(ip[1]), i32::from(ip[2]), i32::from(ip[3]));
        let a1 = i0 + i3;
        let b1 = i1 + i2;
        let c1 = i1 - i2;
        let d1 = i0 - i3;
        out[r * 4] = ((a1 + b1 + 3) >> 3) as i16;
        out[r * 4 + 1] = ((c1 + d1 + 3) >> 3) as i16;
        out[r * 4 + 2] = ((a1 - b1 + 3) >> 3) as i16;
        out[r * 4 + 3] = ((d1 - c1 + 3) >> 3) as i16;
    }
    out
}

const C1: i32 = 20091; // cos(pi/8) * sqrt(2) - 1, in 1/65536
const S1: i32 = 35468; // sin(pi/8) * sqrt(2)

#[inline]
fn mul_c(x: i32) -> i32 {
    x + ((x * C1) >> 16)
}
#[inline]
fn mul_s(x: i32) -> i32 {
    (x * S1) >> 16
}

/// Inverse DCT of `c` added to the 4x4 block at `off` of `dst` (row stride `stride`).
pub fn idct_add(c: &[i16; 16], dst: &mut [u8], off: usize, stride: usize) {
    let mut t = [0i16; 16];
    for i in 0..4 {
        let (i0, i4, i8, i12) = (i32::from(c[i]), i32::from(c[4 + i]), i32::from(c[8 + i]), i32::from(c[12 + i]));
        let a1 = i0 + i8;
        let b1 = i0 - i8;
        let c1 = mul_s(i4) - mul_c(i12);
        let d1 = mul_c(i4) + mul_s(i12);
        t[i] = (a1 + d1) as i16;
        t[12 + i] = (a1 - d1) as i16;
        t[4 + i] = (b1 + c1) as i16;
        t[8 + i] = (b1 - c1) as i16;
    }
    for r in 0..4 {
        let (i0, i1, i2, i3) = (i32::from(t[r * 4]), i32::from(t[r * 4 + 1]), i32::from(t[r * 4 + 2]), i32::from(t[r * 4 + 3]));
        let a1 = i0 + i2;
        let b1 = i0 - i2;
        let c1 = mul_s(i1) - mul_c(i3);
        let d1 = mul_c(i1) + mul_s(i3);
        let row = [(a1 + d1 + 4) >> 3, (b1 + c1 + 4) >> 3, (b1 - c1 + 4) >> 3, (a1 - d1 + 4) >> 3];
        let o = off + r * stride;
        if let Some(px) = dst.get_mut(o..o + 4) {
            for (p, v) in px.iter_mut().zip(row) {
                *p = (i32::from(*p) + v).clamp(0, 255) as u8;
            }
        }
    }
}

/// Only a DC coefficient: the same value added to every sample.
pub fn dc_add(dc: i16, dst: &mut [u8], off: usize, stride: usize) {
    let v = (i32::from(dc) + 4) >> 3;
    for r in 0..4 {
        let o = off + r * stride;
        if let Some(px) = dst.get_mut(o..o + 4) {
            for p in px {
                *p = (i32::from(*p) + v).clamp(0, 255) as u8;
            }
        }
    }
}

// ---- Intra prediction --------------------------------------------------------------------

/// The samples around a macroblock (luma: n = 16, chroma: n = 8) as intra prediction sees
/// them: 127 above the frame, 129 left of it.
pub struct Edges {
    /// above[0] is the above-left sample; then n above, then 4 above-right (luma).
    pub above: [u8; 21],
    pub left: [u8; 16],
    pub have_above: bool,
    pub have_left: bool,
}

pub fn edges(p: &Plane, x0: usize, y0: usize, n: usize, last_col: bool) -> Edges {
    let mut e = Edges { above: [127; 21], left: [129; 16], have_above: y0 > 0, have_left: x0 > 0 };
    if y0 > 0 {
        e.above[0] = if x0 > 0 { p.at(x0 - 1, y0 - 1) } else { 129 };
        for i in 0..n {
            e.above[1 + i] = p.at(x0 + i, y0 - 1);
        }
        if n == 16 {
            // Above-right: the next macroblock's bottom row, or at the right edge the last
            // sample of the row above repeated.
            for i in 0..4 {
                e.above[17 + i] = if last_col { p.at(x0 + 15, y0 - 1) } else { p.at(x0 + 16 + i, y0 - 1) };
            }
        }
    }
    if x0 > 0 {
        for i in 0..n {
            e.left[i] = p.at(x0 - 1, y0 + i);
        }
    }
    e
}

/// A whole-block mode (DC, V, H, TM) over n x n samples at (x0, y0).
pub fn predict_block(p: &mut Plane, x0: usize, y0: usize, n: usize, mode: u8, e: &Edges) {
    let w = p.width;
    let above = &e.above[1..1 + n];
    let left = &e.left[..n];
    let shift = if n == 16 { 3 } else { 2 };
    let dc = || -> u8 {
        let mut sum = 0u32;
        let mut sh = shift;
        if e.have_above {
            sum += above.iter().map(|&v| u32::from(v)).sum::<u32>();
            sh += 1;
        }
        if e.have_left {
            sum += left.iter().map(|&v| u32::from(v)).sum::<u32>();
            sh += 1;
        }
        if sh == shift { 128 } else { ((sum + (1 << (sh - 1))) >> sh) as u8 }
    };
    let dcv = if mode == crate::modes::DC_PRED { dc() } else { 0 };
    let tl = i32::from(e.above[0]);
    for y in 0..n {
        let o = (y0 + y) * w + x0;
        let Some(row) = p.data.get_mut(o..o + n) else { return };
        match mode {
            crate::modes::V_PRED => row.copy_from_slice(above),
            crate::modes::H_PRED => row.fill(left[y]),
            crate::modes::TM_PRED => {
                let l = i32::from(left[y]) - tl;
                for (d, &a) in row.iter_mut().zip(above) {
                    *d = (l + i32::from(a)).clamp(0, 255) as u8;
                }
            }
            _ => row.fill(dcv),
        }
    }
}

#[inline]
fn avg2(a: u8, b: u8) -> u8 {
    ((u16::from(a) + u16::from(b) + 1) >> 1) as u8
}
#[inline]
fn avg3(a: u8, b: u8, c: u8) -> u8 {
    ((u16::from(a) + 2 * u16::from(b) + u16::from(c) + 2) >> 2) as u8
}

/// A 4x4 subblock mode. `a` is 8 samples above (4 + 4 above-right), `l` the 4 to the left and
/// `tl` the one above-left. Returns the prediction in raster order.
pub fn predict_sub(mode: u8, a: &[u8; 8], l: &[u8; 4], tl: u8) -> [u8; 16] {
    let mut b = [0u8; 16];
    let mut set = |r: usize, c: usize, v: u8| b[r * 4 + c] = v;
    // Edge samples from bottom-left round to top-right: L3 L2 L1 L0 TL A0 A1 A2 A3.
    let e = [l[3], l[2], l[1], l[0], tl, a[0], a[1], a[2], a[3]];
    match mode {
        0 => {
            // B_DC
            let s: u32 = a[..4].iter().chain(l.iter()).map(|&v| u32::from(v)).sum();
            let v = ((s + 4) >> 3) as u8;
            return [v; 16];
        }
        1 => {
            // B_TM
            for r in 0..4 {
                for c in 0..4 {
                    set(r, c, (i32::from(l[r]) + i32::from(a[c]) - i32::from(tl)).clamp(0, 255) as u8);
                }
            }
        }
        2 => {
            // B_VE
            let v = [avg3(tl, a[0], a[1]), avg3(a[0], a[1], a[2]), avg3(a[1], a[2], a[3]), avg3(a[2], a[3], a[4])];
            for r in 0..4 {
                for c in 0..4 {
                    set(r, c, v[c]);
                }
            }
        }
        3 => {
            // B_HE
            let v = [avg3(tl, l[0], l[1]), avg3(l[0], l[1], l[2]), avg3(l[1], l[2], l[3]), avg3(l[2], l[3], l[3])];
            for r in 0..4 {
                for c in 0..4 {
                    set(r, c, v[r]);
                }
            }
        }
        4 => {
            // B_LD
            for r in 0..4 {
                for c in 0..4 {
                    let i = r + c;
                    let v = if i == 6 { avg3(a[6], a[7], a[7]) } else { avg3(a[i], a[i + 1], a[i + 2]) };
                    set(r, c, v);
                }
            }
        }
        5 => {
            // B_RD
            for r in 0..4 {
                for c in 0..4 {
                    let i = 3 - r + c;
                    set(r, c, avg3(e[i], e[i + 1], e[i + 2]));
                }
            }
        }
        6 => {
            // B_VR
            set(3, 0, avg3(e[1], e[2], e[3]));
            set(2, 0, avg3(e[2], e[3], e[4]));
            let v = avg3(e[3], e[4], e[5]);
            set(3, 1, v);
            set(1, 0, v);
            let v = avg2(e[4], e[5]);
            set(2, 1, v);
            set(0, 0, v);
            let v = avg3(e[4], e[5], e[6]);
            set(3, 2, v);
            set(1, 1, v);
            let v = avg2(e[5], e[6]);
            set(2, 2, v);
            set(0, 1, v);
            let v = avg3(e[5], e[6], e[7]);
            set(3, 3, v);
            set(1, 2, v);
            let v = avg2(e[6], e[7]);
            set(2, 3, v);
            set(0, 2, v);
            set(1, 3, avg3(e[6], e[7], e[8]));
            set(0, 3, avg2(e[7], e[8]));
        }
        7 => {
            // B_VL
            set(0, 0, avg2(a[0], a[1]));
            set(1, 0, avg3(a[0], a[1], a[2]));
            let v = avg2(a[1], a[2]);
            set(2, 0, v);
            set(0, 1, v);
            let v = avg3(a[1], a[2], a[3]);
            set(1, 1, v);
            set(3, 0, v);
            let v = avg2(a[2], a[3]);
            set(2, 1, v);
            set(0, 2, v);
            let v = avg3(a[2], a[3], a[4]);
            set(3, 1, v);
            set(1, 2, v);
            let v = avg2(a[3], a[4]);
            set(2, 2, v);
            set(0, 3, v);
            let v = avg3(a[3], a[4], a[5]);
            set(3, 2, v);
            set(1, 3, v);
            set(2, 3, avg3(a[4], a[5], a[6]));
            set(3, 3, avg3(a[5], a[6], a[7]));
        }
        8 => {
            // B_HD
            set(3, 0, avg2(e[0], e[1]));
            set(3, 1, avg3(e[0], e[1], e[2]));
            let v = avg2(e[1], e[2]);
            set(2, 0, v);
            set(3, 2, v);
            let v = avg3(e[1], e[2], e[3]);
            set(2, 1, v);
            set(3, 3, v);
            let v = avg2(e[2], e[3]);
            set(2, 2, v);
            set(1, 0, v);
            let v = avg3(e[2], e[3], e[4]);
            set(2, 3, v);
            set(1, 1, v);
            let v = avg2(e[3], e[4]);
            set(1, 2, v);
            set(0, 0, v);
            let v = avg3(e[3], e[4], e[5]);
            set(1, 3, v);
            set(0, 1, v);
            set(0, 2, avg3(e[4], e[5], e[6]));
            set(0, 3, avg3(e[5], e[6], e[7]));
        }
        _ => {
            // B_HU
            set(0, 0, avg2(l[0], l[1]));
            set(0, 1, avg3(l[0], l[1], l[2]));
            let v = avg2(l[1], l[2]);
            set(0, 2, v);
            set(1, 0, v);
            let v = avg3(l[1], l[2], l[3]);
            set(0, 3, v);
            set(1, 1, v);
            let v = avg2(l[2], l[3]);
            set(1, 2, v);
            set(2, 0, v);
            let v = avg3(l[2], l[3], l[3]);
            set(1, 3, v);
            set(2, 1, v);
            for (r, c) in [(2, 2), (2, 3), (3, 0), (3, 1), (3, 2), (3, 3)] {
                set(r, c, l[3]);
            }
        }
    }
    b
}

// ---- Inter prediction ----------------------------------------------------------------------

const SIXTAP: [[i32; 6]; 8] = [
    [0, 0, 128, 0, 0, 0],
    [0, -6, 123, 12, -1, 0],
    [2, -11, 108, 36, -8, 1],
    [0, -9, 93, 50, -6, 0],
    [3, -16, 77, 77, -16, 3],
    [0, -6, 50, 93, -9, 0],
    [1, -8, 36, 108, -11, 2],
    [0, -1, 12, 123, -6, 0],
];

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Filter {
    SixTap,
    Bilinear,
}

/// Predict a `w` x `h` block (w, h ≤ 16) from `refp` at whole-sample position (x, y) plus the
/// eighth-sample fraction (fx, fy), into `dst` at `off` with row stride `stride`.
#[allow(clippy::too_many_arguments)]
pub fn predict_inter(refp: &Plane, x: i32, y: i32, fx: usize, fy: usize, w: usize, h: usize, filter: Filter, dst: &mut [u8], off: usize, stride: usize) {
    let (w, h) = (w.min(16), h.min(16));
    // Source window: 2 samples before to 3 after for the 6-tap filter. Read in place when it
    // lies inside the reference, else copied with the edges extended.
    let mut tmp = [0u8; 21 * 21];
    let (x0, y0) = (x - 2, y - 2);
    let inside = x0 >= 0 && y0 >= 0 && (x0 as usize + w + 5) <= refp.width && (y0 as usize + h + 5) <= refp.height;
    let (src, ss): (&[u8], usize) = if inside {
        let o = y0 as usize * refp.width + x0 as usize;
        (refp.data.get(o..).unwrap_or(&[]), refp.width)
    } else {
        for r in 0..h + 5 {
            for c in 0..w + 5 {
                tmp[r * 21 + c] = refp.clamped(x0 + c as i32, y0 + r as i32);
            }
        }
        (&tmp[..], 21)
    };
    if src.len() < (h + 4) * ss + w + 5 {
        return;
    }
    let at = |r: usize, c: usize| src[(r + 2) * ss + c + 2];
    let mut out = [0u8; 16 * 16];
    if fx == 0 && fy == 0 {
        for r in 0..h {
            for c in 0..w {
                out[r * 16 + c] = at(r, c);
            }
        }
    } else {
        match filter {
            Filter::SixTap => {
                // A zero fraction is the identity filter ((128 v + 64) >> 7 = v): that pass is
                // skipped.
                let hf = &SIXTAP[fx & 7];
                let vf = &SIXTAP[fy & 7];
                let mut mid = [0u8; 21 * 16];
                let rows = if fy == 0 { 2..h + 2 } else { 0..h + 5 };
                for r in rows {
                    let row = &src[r * ss..r * ss + w + 5];
                    let m = &mut mid[r * 16..r * 16 + w];
                    if fx == 0 {
                        m.copy_from_slice(&row[2..2 + w]);
                    } else {
                        for (c, d) in m.iter_mut().enumerate() {
                            let s = &row[c..c + 6];
                            let v = i32::from(s[0]) * hf[0] + i32::from(s[1]) * hf[1] + i32::from(s[2]) * hf[2] + i32::from(s[3]) * hf[3] + i32::from(s[4]) * hf[4] + i32::from(s[5]) * hf[5];
                            *d = ((v + 64) >> 7).clamp(0, 255) as u8;
                        }
                    }
                }
                for r in 0..h {
                    for c in 0..w {
                        out[r * 16 + c] = if fy == 0 {
                            mid[(r + 2) * 16 + c]
                        } else {
                            let m = |k: usize| i32::from(mid[(r + k) * 16 + c]);
                            let v = m(0) * vf[0] + m(1) * vf[1] + m(2) * vf[2] + m(3) * vf[3] + m(4) * vf[4] + m(5) * vf[5];
                            ((v + 64) >> 7).clamp(0, 255) as u8
                        };
                    }
                }
            }
            Filter::Bilinear => {
                let (h0, h1) = (128 - 16 * fx as i32, 16 * fx as i32);
                let (v0, v1) = (128 - 16 * fy as i32, 16 * fy as i32);
                let mut mid = [0u16; 17 * 16];
                for r in 0..h + 1 {
                    for c in 0..w {
                        let v = i32::from(at(r, c)) * h0 + i32::from(at(r, c + 1)) * h1;
                        mid[r * 16 + c] = ((v + 64) >> 7) as u16;
                    }
                }
                for r in 0..h {
                    for c in 0..w {
                        let v = i32::from(mid[r * 16 + c]) * v0 + i32::from(mid[(r + 1) * 16 + c]) * v1;
                        out[r * 16 + c] = ((v + 64) >> 7).clamp(0, 255) as u8;
                    }
                }
            }
        }
    }
    for r in 0..h {
        if let Some(d) = dst.get_mut(off + r * stride..off + r * stride + w) {
            d.copy_from_slice(&out[r * 16..r * 16 + w]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dc_only_idct_matches_the_full_transform() {
        for dc in [-2048i16, -9, -1, 0, 1, 3, 4, 100, 2047] {
            let mut c = [0i16; 16];
            c[0] = dc;
            let mut a = vec![100u8; 16];
            let mut b = a.clone();
            idct_add(&c, &mut a, 0, 4);
            dc_add(dc, &mut b, 0, 4);
            assert_eq!(a, b, "dc {dc}");
        }
    }

    #[test]
    fn walsh_of_a_dc_spreads_it() {
        let mut c = [0i16; 16];
        c[0] = 80;
        assert_eq!(iwht(&c), [10; 16]);
    }

    #[test]
    fn whole_sample_prediction_copies_and_extends_edges() {
        let mut p = Plane::new(16, 16);
        for (i, v) in p.data.iter_mut().enumerate() {
            *v = (i % 16) as u8;
        }
        let mut out = [0u8; 16];
        predict_inter(&p, -3, 0, 0, 0, 4, 4, Filter::SixTap, &mut out, 0, 4);
        assert_eq!(&out[..4], &[0, 0, 0, 0]);
        predict_inter(&p, 14, 2, 0, 0, 4, 4, Filter::SixTap, &mut out, 0, 4);
        assert_eq!(&out[..4], &[14, 15, 15, 15]);
        // A constant plane stays constant under any filter.
        let q = Plane { data: vec![77; 256], width: 16, height: 16 };
        for f in [Filter::SixTap, Filter::Bilinear] {
            predict_inter(&q, 5, 5, 3, 5, 4, 4, f, &mut out, 0, 4);
            assert!(out.iter().all(|&v| v == 77));
        }
    }
}
