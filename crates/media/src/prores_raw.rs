//! Apple ProRes RAW: developing the sensor's picture.
//!
//! FFmpeg (8.0 and later) decodes ProRes RAW, but only as far as the sensor's mosaic: a
//! 16-bit RGGB Bayer pattern, with no white balance and in the camera's own colours. The
//! camera's settings travel in each frame's header, so Reelforge reads them itself
//! ([`parse`]) and develops the mosaic ([`develop`]) the way a RAW converter does:
//!
//! 1. black and white levels: the samples from the black level (256 of 65 535) to the
//!    white level become 0 to 1;
//! 2. demosaicing (Malvar, He and Cutler's gradient-corrected interpolation, or 2 × 2
//!    "superpixels" when the picture is wanted at half size or less, for the viewer);
//! 3. the camera's white balance (its red and blue factors);
//! 4. its colour matrix, from the camera's colours to a standard space, and its gain
//!    (ISO);
//! 5. into linear Rec.709 for the compositor, as half floats. Highlights above 1.0 are kept
//!    (ProRes RAW is high dynamic range), so grading can bring them back.
//!
//! What the colour matrix maps to isn't published. A matrix that sends white to white maps
//! to an RGB space, taken as linear Rec.2020 (Apple's wide gamut); one that sends white to
//! D65's or D50's XYZ maps to XYZ, and is treated as such.
//!
//! The header's layout (all big-endian), from the start of each frame:
//!
//! | Bytes | |
//! |---|---|
//! | 0–3 | frame size |
//! | 4–7 | `prrf` |
//! | 8–9 | header size (from here) |
//! | 10 | reserved |
//! | 11 | version |
//! | 12–15 | vendor |
//! | 16–19 | width, height |
//! | 20–23 | recommended crop: left, right, top, bottom |
//! | 24–25 | Bayer pattern (low two bits; 0 is RGGB) |
//! | 26–27 | sensel range: the white level is this + 256 |
//! | 28–35 | white balance: red, blue factors (floats) |
//! | 36–71 | colour matrix: 3 × 3 floats, row by row |
//! | 72–75 | gain (float, after the matrix) |
//! | 76–77 | white balance colour temperature (K, informational) |
//! | 78–79 | flags |

use rayon::prelude::*;
use reelforge_core::frame::{f32_to_f16, FloatPixels};

/// The camera's settings for one frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RawParams {
    /// The black and white levels, as shares of 65 535.
    pub black: f32,
    pub white: f32,
    /// White balance: red and blue factors.
    pub wb: [f32; 2],
    /// From white-balanced camera colours to the camera's output space, row by row.
    pub matrix: [[f32; 3]; 3],
    /// Gain after the matrix (ISO).
    pub gain: f32,
    /// The white balance's colour temperature (K), for showing.
    pub kelvin: u16,
    /// The Bayer pattern (0: RGGB, the only one seen).
    pub pattern: u8,
}

impl Default for RawParams {
    fn default() -> Self {
        RawParams { black: 256.0 / 65535.0, white: 1.0, wb: [1.0, 1.0], matrix: IDENTITY, gain: 1.0, kelvin: 0, pattern: 0 }
    }
}

const IDENTITY: [[f32; 3]; 3] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];

/// Is `packet` a ProRes RAW frame (its `prrf` tag)?
pub fn is_prores_raw(packet: &[u8]) -> bool {
    packet.get(4..8) == Some(b"prrf")
}

/// The camera's settings from a ProRes RAW frame's header; `None` if it isn't one. Values
/// that are missing or make no sense fall back to neutral ones.
pub fn parse(packet: &[u8]) -> Option<RawParams> {
    if !is_prores_raw(packet) {
        return None;
    }
    let header_len = be16(packet, 8)? as usize;
    // The header's own bytes (it counts its two length bytes).
    let end = 8usize.saturating_add(header_len).min(packet.len());
    let h = packet.get(..end)?;
    let mut p = RawParams::default();
    if let Some(range) = be16(h, 26) {
        p.white = ((range as f32 + 256.0) / 65535.0).clamp(p.black + 1.0 / 65535.0, 1.0);
    }
    p.pattern = (be16(h, 24).unwrap_or(0) & 3) as u8;
    let sane = |v: f32, lo: f32, hi: f32| v.is_finite() && v > lo && v < hi;
    if let (Some(r), Some(b)) = (bef(h, 28), bef(h, 32)) {
        if sane(r, 0.01, 50.0) && sane(b, 0.01, 50.0) {
            p.wb = [r, b];
        }
    }
    let mut m = [[0f32; 3]; 3];
    let mut ok = true;
    for (i, v) in m.iter_mut().flatten().enumerate() {
        match bef(h, 36 + i * 4) {
            Some(x) if x.is_finite() && x.abs() < 50.0 => *v = x,
            _ => ok = false,
        }
    }
    // A matrix of zeros (or with a zero row) would black the picture out.
    if ok && m.iter().all(|r| r.iter().any(|v| v.abs() > 1e-4)) {
        p.matrix = m;
    }
    if let Some(g) = bef(h, 72) {
        if sane(g, 0.001, 1000.0) {
            p.gain = g;
        }
    }
    p.kelvin = be16(h, 76).unwrap_or(0);
    Some(p)
}

fn be16(b: &[u8], at: usize) -> Option<u16> {
    let s = b.get(at..at.checked_add(2)?)?;
    Some(u16::from_be_bytes([*s.first()?, *s.get(1)?]))
}

fn bef(b: &[u8], at: usize) -> Option<f32> {
    let s = b.get(at..at.checked_add(4)?)?;
    Some(f32::from_bits(u32::from_be_bytes(s.try_into().ok()?)))
}

fn mul(a: &[[f32; 3]; 3], b: &[[f32; 3]; 3]) -> [[f32; 3]; 3] {
    let mut o = [[0f32; 3]; 3];
    for (r, row) in o.iter_mut().enumerate() {
        for (c, v) in row.iter_mut().enumerate() {
            *v = (0..3).map(|k| a[r][k] * b[k][c]).sum();
        }
    }
    o
}

/// Linear Rec.2020 to linear Rec.709.
const REC2020_TO_709: [[f32; 3]; 3] = [[1.660_491, -0.587_641, -0.072_850], [-0.124_550, 1.132_9, -0.008_349], [-0.018_151, -0.100_579, 1.118_73]];
/// XYZ (D65) to linear Rec.709.
const XYZ65_TO_709: [[f32; 3]; 3] = [[3.240_97, -1.537_383, -0.498_611], [-0.969_244, 1.875_968, 0.041_555], [0.055_630, -0.203_977, 1.056_972]];
/// XYZ (D50) to linear Rec.709, Bradford-adapted to D65.
const XYZ50_TO_709: [[f32; 3]; 3] = [[3.133_856, -1.616_867, -0.490_615], [-0.978_768, 1.916_142, 0.033_454], [0.071_945, -0.228_991, 1.405_243]];

/// What the camera's matrix maps to, judged by where it sends white.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    Rec2020,
    XyzD65,
    XyzD50,
}

pub fn target(m: &[[f32; 3]; 3]) -> Target {
    let w: Vec<f32> = m.iter().map(|r| r.iter().sum()).collect();
    let near = |t: [f32; 3]| w.iter().zip(t).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
    let (rgb, d65, d50) = (near([1.0, 1.0, 1.0]), near([0.9505, 1.0, 1.089]), near([0.9642, 1.0, 0.8249]));
    if d65 < rgb && d65 < d50 && d65 < 0.05 {
        Target::XyzD65
    } else if d50 < rgb && d50 < 0.05 {
        Target::XyzD50
    } else {
        Target::Rec2020
    }
}

/// From the 0–1 camera samples straight to linear Rec.709: white balance, matrix, gain and
/// the change of space in one matrix.
pub fn camera_to_709(p: &RawParams) -> [[f32; 3]; 3] {
    let wb = [[p.wb[0], 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, p.wb[1]]];
    let to = match target(&p.matrix) {
        Target::Rec2020 => REC2020_TO_709,
        Target::XyzD65 => XYZ65_TO_709,
        Target::XyzD50 => XYZ50_TO_709,
    };
    let m = mul(&to, &mul(&p.matrix, &wb));
    m.map(|r| r.map(|v| v * p.gain))
}

/// The mosaic: `w` × `h` 16-bit samples, rows `stride` samples apart, its first row
/// starting R G R G…
pub struct Mosaic<'a> {
    pub data: &'a [u16],
    pub stride: usize,
    pub w: usize,
    pub h: usize,
}

impl Mosaic<'_> {
    /// The sample at (x, y), mirrored at the edges so the colours stay in place.
    #[inline]
    fn at(&self, x: isize, y: isize) -> f32 {
        let x = mirror(x, self.w);
        let y = mirror(y, self.h);
        self.data.get(y * self.stride + x).copied().unwrap_or(0) as f32
    }
}

/// `i` folded back into 0..n, two at a time (so the Bayer colour at `i` is kept).
#[inline]
fn mirror(i: isize, n: usize) -> usize {
    let n = n as isize;
    let mut i = i;
    if i < 0 {
        i = -i;
    }
    if i >= n {
        i = 2 * (n - 1) - i;
    }
    i.clamp(0, (n - 1).max(0)) as usize
}

/// The mosaic developed into `dw` × `dh` half-float linear Rec.709 RGBA (opaque).
pub fn develop(m: &Mosaic, p: &RawParams, dw: u32, dh: u32) -> FloatPixels {
    let (dw, dh) = (dw.max(1) as usize, dh.max(1) as usize);
    let t = camera_to_709(p);
    let span = (p.white - p.black).max(1e-6) * 65535.0;
    let black = p.black * 65535.0;
    let norm = move |v: f32| (v - black) / span;
    let swap = p.pattern != 0;
    // Half size or less: 2 × 2 superpixels, then shrunk; else the full demosaic.
    let (rgb, w, h) = if m.w < 4 || m.h < 4 || (dw * 2 <= m.w && dh * 2 <= m.h) {
        let (w, h) = ((m.w / 2).max(1), (m.h / 2).max(1));
        let mut out = vec![0f32; w * h * 3];
        out.par_chunks_mut(w * 3).enumerate().for_each(|(y, row)| {
            for (x, px) in row.chunks_exact_mut(3).enumerate() {
                let (x2, y2) = (2 * x as isize, 2 * y as isize);
                let r = norm(m.at(x2, y2));
                let g = 0.5 * (norm(m.at(x2 + 1, y2)) + norm(m.at(x2, y2 + 1)));
                let b = norm(m.at(x2 + 1, y2 + 1));
                let (r, b) = if swap { (b, r) } else { (r, b) };
                px.copy_from_slice(&apply(&t, [r, g, b]));
            }
        });
        (out, w, h)
    } else {
        let (w, h) = (m.w, m.h);
        let mut out = vec![0f32; w * h * 3];
        out.par_chunks_mut(w * 3).enumerate().for_each(|(y, row)| {
            for (x, px) in row.chunks_exact_mut(3).enumerate() {
                let [r, g, b] = mhc(m, x as isize, y as isize).map(norm_mhc(black, span));
                let (r, b) = if swap { (b, r) } else { (r, b) };
                px.copy_from_slice(&apply(&t, [r, g, b]));
            }
        });
        (out, w, h)
    };
    let rgb = if (w, h) == (dw, dh) { rgb } else { shrink(&rgb, w, h, dw, dh) };
    let one = f32_to_f16(1.0);
    let mut rgba = vec![0u16; dw * dh * 4];
    rgba.par_chunks_mut(dw * 4).zip(rgb.par_chunks(dw * 3)).for_each(|(o, i)| {
        for (o, i) in o.chunks_exact_mut(4).zip(i.chunks_exact(3)) {
            for (o, v) in o.iter_mut().zip(i) {
                *o = f32_to_f16(if v.is_finite() { v.clamp(0.0, 65000.0) } else { 0.0 });
            }
            if let Some(a) = o.get_mut(3) {
                *a = one;
            }
        }
    });
    FloatPixels { width: dw as u32, height: dh as u32, rgba }
}

fn norm_mhc(black: f32, span: f32) -> impl Fn(f32) -> f32 {
    move |v| (v - black) / span
}

#[inline]
fn apply(t: &[[f32; 3]; 3], c: [f32; 3]) -> [f32; 3] {
    t.map(|r| r[0] * c[0] + r[1] * c[1] + r[2] * c[2])
}

/// Malvar–He–Cutler demosaicing at (x, y) of an RGGB mosaic: the raw R, G, B there.
fn mhc(m: &Mosaic, x: isize, y: isize) -> [f32; 3] {
    let s = |dx: isize, dy: isize| m.at(x + dx, y + dy);
    let c = s(0, 0);
    // The four kinds of neighbourhood.
    let cross = s(-1, 0) + s(1, 0) + s(0, -1) + s(0, 1);
    let far = s(-2, 0) + s(2, 0) + s(0, -2) + s(0, 2);
    let diag = s(-1, -1) + s(1, -1) + s(-1, 1) + s(1, 1);
    let far_h = s(-2, 0) + s(2, 0);
    let far_v = s(0, -2) + s(0, 2);
    let near_h = s(-1, 0) + s(1, 0);
    let near_v = s(0, -1) + s(0, 1);
    // Green where red or blue was measured.
    let g_at_rb = (4.0 * c + 2.0 * cross - far) / 8.0;
    // Red/blue at the opposite of the two (diagonal neighbours).
    let rb_at_br = (6.0 * c + 2.0 * diag - 1.5 * far) / 8.0;
    // Red/blue at green, from the row's neighbours and from the column's.
    let from_row = (5.0 * c + 4.0 * near_h - diag - far_h + 0.5 * far_v) / 8.0;
    let from_col = (5.0 * c + 4.0 * near_v - diag - far_v + 0.5 * far_h) / 8.0;
    match ((y & 1) == 1, (x & 1) == 1) {
        // Red.
        (false, false) => [c, g_at_rb, rb_at_br],
        // Green on a red row: red left and right, blue above and below.
        (false, true) => [from_row, c, from_col],
        // Green on a blue row: blue left and right, red above and below.
        (true, false) => [from_col, c, from_row],
        // Blue.
        (true, true) => [rb_at_br, g_at_rb, c],
    }
}

/// `src` (`w` × `h` RGB) averaged down to `dw` × `dh`, each output pixel the mean of the
/// source area it covers.
fn shrink(src: &[f32], w: usize, h: usize, dw: usize, dh: usize) -> Vec<f32> {
    // Each output column: the source columns it covers and how much of each.
    let spans = |n: usize, dn: usize| -> Vec<Vec<(usize, f32)>> {
        let k = n as f64 / dn as f64;
        (0..dn)
            .map(|o| {
                let (a, b) = (o as f64 * k, ((o + 1) as f64 * k).min(n as f64));
                let mut v = Vec::new();
                let mut i = a.floor() as usize;
                while (i as f64) < b && i < n {
                    let lo = (i as f64).max(a);
                    let hi = ((i + 1) as f64).min(b);
                    if hi > lo {
                        v.push((i, ((hi - lo) / (b - a).max(1e-9)) as f32));
                    }
                    i += 1;
                }
                if v.is_empty() {
                    v.push(((a as usize).min(n.saturating_sub(1)), 1.0));
                }
                v
            })
            .collect()
    };
    let (xs, ys) = (spans(w, dw), spans(h, dh));
    // Across, then down.
    let mut across = vec![0f32; dw * h * 3];
    across.par_chunks_mut(dw * 3).enumerate().for_each(|(y, row)| {
        let Some(line) = src.get(y * w * 3..(y + 1) * w * 3) else { return };
        for (px, span) in row.chunks_exact_mut(3).zip(&xs) {
            let mut acc = [0f32; 3];
            for &(i, wt) in span {
                if let Some(s) = line.get(i * 3..i * 3 + 3) {
                    for k in 0..3 {
                        acc[k] += s[k] * wt;
                    }
                }
            }
            px.copy_from_slice(&acc);
        }
    });
    let mut out = vec![0f32; dw * dh * 3];
    out.par_chunks_mut(dw * 3).zip(ys.par_iter()).for_each(|(row, span)| {
        for &(j, wt) in span {
            let Some(line) = across.get(j * dw * 3..(j + 1) * dw * 3) else { continue };
            for (o, s) in row.iter_mut().zip(line) {
                *o += s * wt;
            }
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use reelforge_core::frame::f16_to_f32;

    /// A ProRes RAW header with these settings (and a frame size and tag).
    fn header(range: u16, wb: [f32; 2], m: [[f32; 3]; 3], gain: f32) -> Vec<u8> {
        let mut h = vec![0u8; 80];
        h[4..8].copy_from_slice(b"prrf");
        h[8..10].copy_from_slice(&72u16.to_be_bytes());
        h[16..18].copy_from_slice(&64u16.to_be_bytes());
        h[18..20].copy_from_slice(&32u16.to_be_bytes());
        h[26..28].copy_from_slice(&range.to_be_bytes());
        h[28..32].copy_from_slice(&wb[0].to_bits().to_be_bytes());
        h[32..36].copy_from_slice(&wb[1].to_bits().to_be_bytes());
        for (i, v) in m.iter().flatten().enumerate() {
            h[36 + i * 4..40 + i * 4].copy_from_slice(&v.to_bits().to_be_bytes());
        }
        h[72..76].copy_from_slice(&gain.to_bits().to_be_bytes());
        h[76..78].copy_from_slice(&5600u16.to_be_bytes());
        let n = h.len() as u32;
        h[0..4].copy_from_slice(&n.to_be_bytes());
        h
    }

    #[test]
    fn reads_the_header() {
        let m = [[1.6, -0.4, -0.2], [-0.3, 1.5, -0.2], [0.0, -0.5, 1.5]];
        let p = parse(&header(4000 - 256, [2.0, 1.5], m, 1.25)).expect("ProRes RAW");
        assert_eq!(p.wb, [2.0, 1.5]);
        assert_eq!(p.matrix, m);
        assert_eq!((p.gain, p.kelvin, p.pattern), (1.25, 5600, 0));
        assert!((p.white * 65535.0 - 4000.0).abs() < 0.01 && (p.black * 65535.0 - 256.0).abs() < 0.01);
        // Not ProRes RAW; and odd headers fall back to neutral settings without harm.
        assert_eq!(parse(b"\0\0\0\x10icpf\0\0\0\0\0\0\0\0"), None);
        let mut bad = header(0, [f32::NAN, -1.0], [[0.0; 3]; 3], f32::INFINITY);
        let p = parse(&bad).expect("still ProRes RAW");
        assert_eq!((p.wb, p.matrix, p.gain), ([1.0, 1.0], IDENTITY, 1.0));
        assert!(p.white > p.black);
        bad.truncate(30);
        assert!(parse(&bad).is_some());
        for n in 0..12 {
            let _ = parse(&header(100, [1.0; 2], IDENTITY, 1.0)[..n]);
        }
    }

    #[test]
    fn tells_where_the_matrix_goes() {
        assert_eq!(target(&IDENTITY), Target::Rec2020);
        // sRGB's own matrix to XYZ: white goes to D65.
        let srgb_xyz = [[0.4124, 0.3576, 0.1805], [0.2126, 0.7152, 0.0722], [0.0193, 0.1192, 0.9505]];
        assert_eq!(target(&srgb_xyz), Target::XyzD65);
        let d50 = [[0.4360, 0.3851, 0.1431], [0.2225, 0.7169, 0.0606], [0.0139, 0.0971, 0.7139]];
        assert_eq!(target(&d50), Target::XyzD50);
        // And so through it, white stays white in Rec.709.
        let p = RawParams { matrix: srgb_xyz, ..Default::default() };
        let w = apply(&camera_to_709(&p), [1.0, 1.0, 1.0]);
        assert!(w.iter().all(|v| (v - 1.0).abs() < 0.01), "{w:?}");
    }

    /// A mosaic of a flat colour (`rgb` as 0–1 camera values over black).
    fn flat(w: usize, h: usize, rgb: [f32; 3], p: &RawParams) -> Vec<u16> {
        let (black, span) = (p.black * 65535.0, (p.white - p.black) * 65535.0);
        (0..h)
            .flat_map(|y| (0..w).map(move |x| (y, x)))
            .map(|(y, x)| {
                let v = match (y & 1, x & 1) {
                    (0, 0) => rgb[0],
                    (1, 1) => rgb[2],
                    _ => rgb[1],
                };
                (black + v * span).round() as u16
            })
            .collect()
    }

    fn pixel(f: &FloatPixels, x: usize, y: usize) -> [f32; 3] {
        let i = (y * f.width as usize + x) * 4;
        [f16_to_f32(f.rgba[i]), f16_to_f32(f.rgba[i + 1]), f16_to_f32(f.rgba[i + 2])]
    }

    #[test]
    fn develops_white_balance_matrix_and_gain() {
        // A grey card under warm light: the camera saw less blue and more red. Its white
        // balance makes it grey again; the identity matrix (Rec.2020) and a gain of 2.
        let p = RawParams { wb: [0.5, 2.0], gain: 2.0, white: 4095.0 / 65535.0, ..Default::default() };
        let data = flat(32, 16, [0.36, 0.18, 0.09], &p);
        let mosaic = Mosaic { data: &data, stride: 32, w: 32, h: 16 };
        for (dw, dh) in [(32, 16), (16, 8), (7, 3)] {
            let f = develop(&mosaic, &p, dw, dh);
            assert_eq!((f.width, f.height), (dw, dh));
            let c = pixel(&f, dw as usize / 2, dh as usize / 2);
            // Grey at 0.36 (0.18 × 2), the same in any space.
            assert!(c.iter().all(|v| (v - 0.36).abs() < 0.01), "{dw}×{dh}: {c:?}");
            // Every pixel, the edges too.
            for y in 0..dh as usize {
                for x in 0..dw as usize {
                    let c = pixel(&f, x, y);
                    assert!(c.iter().all(|v| (v - 0.36).abs() < 0.02), "{dw}×{dh} at {x},{y}: {c:?}");
                }
            }
        }
        // Highlights past the white level's worth are kept, not clipped.
        let hot = RawParams { gain: 8.0, ..p };
        let f = develop(&mosaic, &hot, 32, 16);
        assert!(pixel(&f, 16, 8)[1] > 1.0);
    }

    #[test]
    fn demosaics_colour_edges_cleanly() {
        // Red on the left half, blue on the right: away from the edge, pure colours; Rec.2020
        // red is redder than Rec.709 can show, so it comes out past 1 with the rest negative
        // (clamped to 0).
        let p = RawParams { white: 1.0, ..Default::default() };
        let (w, h) = (24usize, 12usize);
        let left = flat(w, h, [0.5, 0.0, 0.0], &p);
        let right = flat(w, h, [0.0, 0.0, 0.5], &p);
        let data: Vec<u16> = (0..h).flat_map(|y| (0..w).map(move |x| (y, x))).map(|(y, x)| if x < w / 2 { left[y * w + x] } else { right[y * w + x] }).collect();
        let f = develop(&Mosaic { data: &data, stride: w, w, h }, &p, w as u32, h as u32);
        let red = pixel(&f, 3, 6);
        let blue = pixel(&f, 20, 6);
        assert!(red[0] > 0.7 && red[1] < 0.02 && red[2] < 0.02, "{red:?}");
        assert!(blue[2] > 0.5 && blue[0] < 0.02 && blue[1] < 0.02, "{blue:?}");
    }

    #[test]
    fn odd_sizes_and_numbers_do_no_harm() {
        let p = RawParams::default();
        for (w, h) in [(1, 1), (2, 2), (3, 5), (6, 2)] {
            let data = vec![30000u16; w * h];
            let f = develop(&Mosaic { data: &data, stride: w, w, h }, &p, 2, 2);
            assert_eq!(f.rgba.len(), 16);
        }
        // Too little data for the size said.
        let f = develop(&Mosaic { data: &[1, 2, 3], stride: 64, w: 64, h: 64 }, &p, 8, 8);
        assert_eq!(f.rgba.len(), 8 * 8 * 4);
        let wild = RawParams { matrix: [[1e30, -1e30, 0.0]; 3], gain: 1e30, ..p };
        let data = vec![65535u16; 64];
        let f = develop(&Mosaic { data: &data, stride: 8, w: 8, h: 8 }, &wild, 8, 8);
        assert!(f.rgba.iter().all(|&v| f16_to_f32(v).is_finite()));
    }
}
