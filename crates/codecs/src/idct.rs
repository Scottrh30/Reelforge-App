//! The 8×8 inverse DCT used by the intra codecs (ProRes, DNxHD, MPEG-2,
//! JPEG-style), in the usual normalisation:
//! `f(x,y) = ¼ ΣΣ C(u)C(v) F(u,v) cos((2x+1)uπ/16) cos((2y+1)vπ/16)`, C(0) = 1/√2.
//!
//! Done in two passes with a 64-entry table in f32, which is well inside the
//! ±1 LSB accuracy the codecs ask for, and rounds once at the end.

use std::sync::OnceLock;

fn table() -> &'static [[f32; 8]; 8] {
    static T: OnceLock<[[f32; 8]; 8]> = OnceLock::new();
    T.get_or_init(|| {
        let mut t = [[0f32; 8]; 8];
        for (x, row) in t.iter_mut().enumerate() {
            for (u, v) in row.iter_mut().enumerate() {
                let c = if u == 0 { std::f64::consts::FRAC_1_SQRT_2 } else { 1.0 };
                *v = (0.5 * c * (((2 * x + 1) * u) as f64 * std::f64::consts::PI / 16.0).cos()) as f32;
            }
        }
        t
    })
}

/// The table as columns: `cols()[u][x] = T[x][u]` (8 lanes, for vector maths).
fn cols() -> &'static [[f32; 8]; 8] {
    static C: OnceLock<[[f32; 8]; 8]> = OnceLock::new();
    C.get_or_init(|| {
        let t = table();
        std::array::from_fn(|u| std::array::from_fn(|x| t[x][u]))
    })
}

/// Inverse-transform `block` (row-major coefficients F[v*8+u]) into `out`
/// (row-major samples, not yet offset or clipped).
///
/// Written as two matrix products on rows of 8 lanes (which the compiler
/// turns into vector instructions), skipping the zero coefficients that make
/// up most of a typical block.
pub fn idct8x8(block: &[i32; 64], out: &mut [f32; 64]) {
    let c = cols();
    let t = table();
    // tmp[v] = Σ_u F[v][u] · column u of T (a row of 8 samples across x).
    let mut tmp = [[0f32; 8]; 8];
    let mut live = [false; 8];
    for v in 0..8 {
        let row = &block[v * 8..v * 8 + 8];
        let mut acc = [0f32; 8];
        for (u, &f) in row.iter().enumerate() {
            if f != 0 {
                let k = f as f32;
                let cu = &c[u];
                for x in 0..8 {
                    acc[x] += k * cu[x];
                }
                live[v] = true;
            }
        }
        tmp[v] = acc;
    }
    // out[y] = Σ_v T[y][v] · tmp[v].
    for y in 0..8 {
        let ty = &t[y];
        let mut acc = [0f32; 8];
        for v in 0..8 {
            if live[v] {
                let k = ty[v];
                let r = &tmp[v];
                for x in 0..8 {
                    acc[x] += k * r[x];
                }
            }
        }
        out[y * 8..y * 8 + 8].copy_from_slice(&acc);
    }
}

type V8 = [f32; 8];

#[inline(always)]
fn add(a: V8, b: V8) -> V8 {
    std::array::from_fn(|i| a[i] + b[i])
}
#[inline(always)]
fn sub(a: V8, b: V8) -> V8 {
    std::array::from_fn(|i| a[i] - b[i])
}
#[inline(always)]
fn mul(a: V8, k: f32) -> V8 {
    std::array::from_fn(|i| a[i] * k)
}

/// One 8-point AAN inverse transform along the index of `r`, done for eight
/// independent lanes at once (`r[k]` holds coefficient k of every lane).
#[inline(always)]
fn aan_1d(r: &mut [V8; 8]) {
    // Even part.
    let tmp10 = add(r[0], r[4]);
    let tmp11 = sub(r[0], r[4]);
    let tmp13 = add(r[2], r[6]);
    let tmp12 = sub(mul(sub(r[2], r[6]), 1.414_213_6), tmp13);
    let e0 = add(tmp10, tmp13);
    let e3 = sub(tmp10, tmp13);
    let e1 = add(tmp11, tmp12);
    let e2 = sub(tmp11, tmp12);
    // Odd part.
    let z13 = add(r[5], r[3]);
    let z10 = sub(r[5], r[3]);
    let z11 = add(r[1], r[7]);
    let z12 = sub(r[1], r[7]);
    let o7 = add(z11, z13);
    let o11 = mul(sub(z11, z13), 1.414_213_6);
    let z5 = mul(add(z10, z12), 1.847_759);
    let o10 = sub(mul(z12, 1.082_392_2), z5);
    let o12 = add(mul(z10, -2.613_126), z5);
    let o6 = sub(o12, o7);
    let o5 = sub(o11, o6);
    let o4 = add(o10, o5);
    r[0] = add(e0, o7);
    r[7] = sub(e0, o7);
    r[1] = add(e1, o6);
    r[6] = sub(e1, o6);
    r[2] = add(e2, o5);
    r[5] = sub(e2, o5);
    r[4] = add(e3, o4);
    r[3] = sub(e3, o4);
}

#[inline(always)]
fn transpose(m: &mut [V8; 8]) {
    for i in 0..8 {
        for j in i + 1..8 {
            let t = m[i][j];
            m[i][j] = m[j][i];
            m[j][i] = t;
        }
    }
}

/// AAN's per-coefficient prescale, `s(u)·s(v)/8` with s(0) = 1, s(k) = √2·cos(kπ/16).
fn aan_scale() -> &'static [f32; 64] {
    static S: OnceLock<[f32; 64]> = OnceLock::new();
    S.get_or_init(|| {
        let s = |k: usize| if k == 0 { 1.0 } else { std::f64::consts::SQRT_2 * (k as f64 * std::f64::consts::PI / 16.0).cos() };
        std::array::from_fn(|i| (s(i / 8) * s(i % 8) / 8.0) as f32)
    })
}

/// A dequantisation table with the AAN prescale folded in (and an extra
/// output `scale`), for [`idct_fast`]. Make it once per slice.
pub fn fast_qmat(qmat: &[i32; 64], scale: f32) -> [f32; 64] {
    let a = aan_scale();
    std::array::from_fn(|i| qmat[i] as f32 * a[i] * scale)
}

/// Dequantise with a [`fast_qmat`] table and inverse-transform: the samples
/// come back row by row (`out[y][x]`), already multiplied by the table's scale.
#[inline]
pub fn idct_fast(coeffs: &[i32], q: &[f32; 64], out: &mut [V8; 8]) {
    // Rows of coefficients: m[v][u].
    for v in 0..8 {
        let c = &coeffs[v * 8..v * 8 + 8];
        let qv = &q[v * 8..v * 8 + 8];
        out[v] = std::array::from_fn(|u| c[u] as f32 * qv[u]);
    }
    // Transform down the columns (along v) for all u at once, then along the rows.
    aan_1d(out);
    transpose(out);
    aan_1d(out);
    transpose(out);
}

/// Dequantise (`coeffs[i] × qmat[i]`), inverse-transform, and write 8×8
/// samples `out × scale + offset`, clamped to `lo..=hi`, into `dst` (`stride`
/// samples per row). The fast path the intra decoders use; `q` comes from
/// [`fast_qmat`] (which takes the scale).
#[inline]
pub fn idct_put_u16(coeffs: &[i32], q: &[f32; 64], offset: f32, lo: f32, hi: f32, dst: &mut [u16], stride: usize) {
    let mut o = [[0f32; 8]; 8];
    idct_fast(coeffs, q, &mut o);
    // +0.5 then truncate rounds (everything is positive after the clamp).
    let off = offset + 0.5;
    for (y, src) in o.iter().enumerate() {
        let row = &mut dst[y * stride..y * stride + 8];
        for x in 0..8 {
            // SAFETY: clamped to a finite range inside u16 just before.
            row[x] = unsafe { (src[x] + off).clamp(lo + 0.5, hi + 0.5).to_int_unchecked::<i32>() } as u16;
        }
    }
}

/// The same for 8-bit output.
#[inline]
pub fn idct_put_u8(coeffs: &[i32], q: &[f32; 64], offset: f32, dst: &mut [u8], stride: usize) {
    let mut o = [[0f32; 8]; 8];
    idct_fast(coeffs, q, &mut o);
    let off = offset + 0.5;
    for (y, src) in o.iter().enumerate() {
        let row = &mut dst[y * stride..y * stride + 8];
        for x in 0..8 {
            // SAFETY: clamped to a finite range inside u8 just before.
            row[x] = unsafe { (src[x] + off).clamp(0.5, 255.5).to_int_unchecked::<i32>() } as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fast_transform_matches_the_reference() {
        let mut b = [0i32; 64];
        for (i, v) in b.iter_mut().enumerate() {
            *v = ((i * 53 % 29) as i32) - 14;
        }
        let ones = [1i32; 64];
        let mut want = [0f32; 64];
        idct8x8(&b, &mut want);
        let mut got = [[0f32; 8]; 8];
        idct_fast(&b, &fast_qmat(&ones, 1.0), &mut got);
        for y in 0..8 {
            for x in 0..8 {
                assert!((got[y][x] - want[y * 8 + x]).abs() < 1e-3, "({x},{y}) {} vs {}", got[y][x], want[y * 8 + x]);
            }
        }
    }

    #[test]
    fn dc_only_is_flat() {
        let mut b = [0i32; 64];
        b[0] = 80;
        let mut o = [0f32; 64];
        idct8x8(&b, &mut o);
        assert!(o.iter().all(|v| (v - 10.0).abs() < 1e-4), "{o:?}");
    }

    #[test]
    fn matches_the_direct_formula() {
        let mut b = [0i32; 64];
        for (i, v) in b.iter_mut().enumerate() {
            *v = ((i * 37 % 23) as i32) - 11;
        }
        let mut o = [0f32; 64];
        idct8x8(&b, &mut o);
        for y in 0..8 {
            for x in 0..8 {
                let mut s = 0f64;
                for v in 0..8 {
                    for u in 0..8 {
                        let cu = if u == 0 { std::f64::consts::FRAC_1_SQRT_2 } else { 1.0 };
                        let cv = if v == 0 { std::f64::consts::FRAC_1_SQRT_2 } else { 1.0 };
                        s += 0.25 * cu * cv * b[v * 8 + u] as f64
                            * (((2 * x + 1) * u) as f64 * std::f64::consts::PI / 16.0).cos()
                            * (((2 * y + 1) * v) as f64 * std::f64::consts::PI / 16.0).cos();
                    }
                }
                assert!((o[y * 8 + x] as f64 - s).abs() < 1e-3);
            }
        }
    }
}
