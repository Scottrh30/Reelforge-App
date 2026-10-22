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

// Plain loops (not `array::from_fn`, which doesn't always inline), so the
// compiler keeps each one as a single vector instruction.
#[inline(always)]
fn add(a: V8, b: V8) -> V8 {
    let mut o = a;
    for i in 0..8 {
        o[i] += b[i];
    }
    o
}
#[inline(always)]
fn sub(a: V8, b: V8) -> V8 {
    let mut o = a;
    for i in 0..8 {
        o[i] -= b[i];
    }
    o
}
#[inline(always)]
fn mul(a: V8, k: f32) -> V8 {
    let mut o = a;
    for v in &mut o {
        *v *= k;
    }
    o
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
#[inline(always)]
pub fn idct_fast(coeffs: &[i32], q: &[f32; 64], out: &mut [V8; 8]) {
    // Rows of coefficients: m[v][u].
    let c = &coeffs[..64];
    for v in 0..8 {
        for u in 0..8 {
            out[v][u] = c[v * 8 + u] as f32 * q[v * 8 + u];
        }
    }
    // Transform down the columns (along v) for all u at once, then along the rows.
    aan_1d(out);
    transpose(out);
    aan_1d(out);
    transpose(out);
}

#[cfg(target_arch = "x86_64")]
static HAS_AVX2: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// Is the AVX2 inverse DCT being used (the CPU has it, and it's allowed)?
pub fn avx2_in_use() -> bool {
    #[cfg(target_arch = "x86_64")]
    {
        avx2()
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        false
    }
}

/// Allow or forbid the AVX2 inverse DCT (for checking it against the portable
/// one). Forbidding always works; allowing only takes effect on a CPU with it.
pub fn set_avx2_allowed(allowed: bool) {
    #[cfg(target_arch = "x86_64")]
    {
        use std::sync::atomic::Ordering;
        let yes = allowed && std::is_x86_feature_detected!("avx2") && std::is_x86_feature_detected!("fma");
        HAS_AVX2.store(if yes { 2 } else { 1 }, Ordering::Relaxed);
    }
    #[cfg(not(target_arch = "x86_64"))]
    let _ = allowed;
}

/// Is AVX2 (with FMA) there? Checked once; the 8-lane maths above then runs
/// as single 256-bit instructions instead of pairs of 128-bit ones.
#[cfg(target_arch = "x86_64")]
#[inline(always)]
fn avx2() -> bool {
    use std::sync::atomic::Ordering;
    static HAS: &std::sync::atomic::AtomicU8 = &HAS_AVX2;
    match HAS.load(Ordering::Relaxed) {
        1 => false,
        2 => true,
        _ => {
            // REELFORGE_NO_AVX2 forces the portable path (for testing and comparing).
            let yes = std::is_x86_feature_detected!("avx2") && std::is_x86_feature_detected!("fma") && std::env::var_os("REELFORGE_NO_AVX2").is_none();
            HAS.store(if yes { 2 } else { 1 }, Ordering::Relaxed);
            yes
        }
    }
}

/// The AAN transform with AVX2: the block in eight 256-bit registers.
#[cfg(target_arch = "x86_64")]
mod avx2 {
    use std::arch::x86_64::*;

    /// One 8-point AAN inverse transform down `r` (lane-wise), as [`super::aan_1d`].
    #[inline(always)]
    unsafe fn aan(r: &mut [__m256; 8]) {
        let k = |v: f32| _mm256_set1_ps(v);
        let (add, sub, mul) = (|a, b| _mm256_add_ps(a, b), |a, b| _mm256_sub_ps(a, b), |a, b| _mm256_mul_ps(a, b));
        let tmp10 = add(r[0], r[4]);
        let tmp11 = sub(r[0], r[4]);
        let tmp13 = add(r[2], r[6]);
        let tmp12 = sub(mul(sub(r[2], r[6]), k(1.414_213_6)), tmp13);
        let e0 = add(tmp10, tmp13);
        let e3 = sub(tmp10, tmp13);
        let e1 = add(tmp11, tmp12);
        let e2 = sub(tmp11, tmp12);
        let z13 = add(r[5], r[3]);
        let z10 = sub(r[5], r[3]);
        let z11 = add(r[1], r[7]);
        let z12 = sub(r[1], r[7]);
        let o7 = add(z11, z13);
        let o11 = mul(sub(z11, z13), k(1.414_213_6));
        let z5 = mul(add(z10, z12), k(1.847_759));
        let o10 = sub(mul(z12, k(1.082_392_2)), z5);
        let o12 = add(mul(z10, k(-2.613_126)), z5);
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
    unsafe fn transpose(r: &mut [__m256; 8]) {
        let t0 = _mm256_unpacklo_ps(r[0], r[1]);
        let t1 = _mm256_unpackhi_ps(r[0], r[1]);
        let t2 = _mm256_unpacklo_ps(r[2], r[3]);
        let t3 = _mm256_unpackhi_ps(r[2], r[3]);
        let t4 = _mm256_unpacklo_ps(r[4], r[5]);
        let t5 = _mm256_unpackhi_ps(r[4], r[5]);
        let t6 = _mm256_unpacklo_ps(r[6], r[7]);
        let t7 = _mm256_unpackhi_ps(r[6], r[7]);
        let u0 = _mm256_shuffle_ps::<0x44>(t0, t2);
        let u1 = _mm256_shuffle_ps::<0xEE>(t0, t2);
        let u2 = _mm256_shuffle_ps::<0x44>(t1, t3);
        let u3 = _mm256_shuffle_ps::<0xEE>(t1, t3);
        let u4 = _mm256_shuffle_ps::<0x44>(t4, t6);
        let u5 = _mm256_shuffle_ps::<0xEE>(t4, t6);
        let u6 = _mm256_shuffle_ps::<0x44>(t5, t7);
        let u7 = _mm256_shuffle_ps::<0xEE>(t5, t7);
        r[0] = _mm256_permute2f128_ps::<0x20>(u0, u4);
        r[1] = _mm256_permute2f128_ps::<0x20>(u1, u5);
        r[2] = _mm256_permute2f128_ps::<0x20>(u2, u6);
        r[3] = _mm256_permute2f128_ps::<0x20>(u3, u7);
        r[4] = _mm256_permute2f128_ps::<0x31>(u0, u4);
        r[5] = _mm256_permute2f128_ps::<0x31>(u1, u5);
        r[6] = _mm256_permute2f128_ps::<0x31>(u2, u6);
        r[7] = _mm256_permute2f128_ps::<0x31>(u3, u7);
    }

    /// Dequantise and inverse-transform: `out[y]` is row y of samples.
    #[inline(always)]
    unsafe fn block(coeffs: &[i32], q: &[f32; 64]) -> [__m256; 8] {
        assert!(coeffs.len() >= 64);
        let mut r = [_mm256_setzero_ps(); 8];
        for (v, row) in r.iter_mut().enumerate() {
            let c = _mm256_cvtepi32_ps(_mm256_loadu_si256(coeffs.as_ptr().add(v * 8) as *const __m256i));
            *row = _mm256_mul_ps(c, _mm256_loadu_ps(q.as_ptr().add(v * 8)));
        }
        aan(&mut r);
        transpose(&mut r);
        aan(&mut r);
        transpose(&mut r);
        r
    }

    /// Rows as rounded, clamped 32-bit integers, packed to 16 bits (unsigned).
    #[inline(always)]
    unsafe fn rows_u16(r: &[__m256; 8], offset: f32, lo: f32, hi: f32) -> [__m128i; 8] {
        let off = _mm256_set1_ps(offset + 0.5);
        let (lo, hi) = (_mm256_set1_ps(lo + 0.5), _mm256_set1_ps(hi + 0.5));
        let mut out = [_mm_setzero_si128(); 8];
        for (o, row) in out.iter_mut().zip(r) {
            let v = _mm256_min_ps(_mm256_max_ps(_mm256_add_ps(*row, off), lo), hi);
            let i = _mm256_cvttps_epi32(v);
            *o = _mm_packus_epi32(_mm256_castsi256_si128(i), _mm256_extracti128_si256::<1>(i));
        }
        out
    }

    #[target_feature(enable = "avx2,fma")]
    pub unsafe fn put_u16(coeffs: &[i32], q: &[f32; 64], offset: f32, lo: f32, hi: f32, dst: &mut [u16], stride: usize) {
        assert!(dst.len() >= 7 * stride + 8);
        let rows = rows_u16(&block(coeffs, q), offset, lo, hi);
        for (y, row) in rows.iter().enumerate() {
            _mm_storeu_si128(dst.as_mut_ptr().add(y * stride) as *mut __m128i, *row);
        }
    }

    #[target_feature(enable = "avx2,fma")]
    pub unsafe fn put_u8(coeffs: &[i32], q: &[f32; 64], offset: f32, dst: &mut [u8], stride: usize) {
        assert!(dst.len() >= 7 * stride + 8);
        let rows = rows_u16(&block(coeffs, q), offset, 0.0, 255.0);
        for (y, row) in rows.iter().enumerate() {
            _mm_storel_epi64(dst.as_mut_ptr().add(y * stride) as *mut __m128i, _mm_packus_epi16(*row, *row));
        }
    }
}

#[cfg(target_arch = "x86_64")]
unsafe fn put_u16_avx2(coeffs: &[i32], q: &[f32; 64], offset: f32, lo: f32, hi: f32, dst: &mut [u16], stride: usize) {
    avx2::put_u16(coeffs, q, offset, lo, hi, dst, stride)
}

#[cfg(target_arch = "x86_64")]
unsafe fn put_u8_avx2(coeffs: &[i32], q: &[f32; 64], offset: f32, dst: &mut [u8], stride: usize) {
    avx2::put_u8(coeffs, q, offset, dst, stride)
}

/// Dequantise (`coeffs[i] × qmat[i]`), inverse-transform, and write 8×8
/// samples `out × scale + offset`, clamped to `lo..=hi`, into `dst` (`stride`
/// samples per row). The fast path the intra decoders use; `q` comes from
/// [`fast_qmat`] (which takes the scale).
#[inline]
pub fn idct_put_u16(coeffs: &[i32], q: &[f32; 64], offset: f32, lo: f32, hi: f32, dst: &mut [u16], stride: usize) {
    #[cfg(target_arch = "x86_64")]
    if avx2() {
        // SAFETY: the CPU has AVX2 and FMA (checked just above).
        return unsafe { put_u16_avx2(coeffs, q, offset, lo, hi, dst, stride) };
    }
    put_u16_any(coeffs, q, offset, lo, hi, dst, stride)
}

#[inline(always)]
fn put_u16_any(coeffs: &[i32], q: &[f32; 64], offset: f32, lo: f32, hi: f32, dst: &mut [u16], stride: usize) {
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
    #[cfg(target_arch = "x86_64")]
    if avx2() {
        // SAFETY: the CPU has AVX2 and FMA (checked just above).
        return unsafe { put_u8_avx2(coeffs, q, offset, dst, stride) };
    }
    put_u8_any(coeffs, q, offset, dst, stride)
}

#[inline(always)]
fn put_u8_any(coeffs: &[i32], q: &[f32; 64], offset: f32, dst: &mut [u8], stride: usize) {
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
    fn the_avx2_path_matches_the_portable_one() {
        #[cfg(target_arch = "x86_64")]
        if avx2() {
            let q = fast_qmat(&std::array::from_fn(|i| 3 + (i as i32 % 7)), 0.25);
            for seed in 0..200i32 {
                let c: Vec<i32> = (0..64).map(|i| if (i * 7 + seed) % 5 < 2 { ((i * 131 + seed * 977) % 801) - 400 } else { 0 }).collect();
                let (mut a, mut b) = (vec![0u16; 8 * 11], vec![0u16; 8 * 11]);
                put_u16_any(&c, &q, 512.0, 4.0, 1019.0, &mut a, 11);
                unsafe { put_u16_avx2(&c, &q, 512.0, 4.0, 1019.0, &mut b, 11) };
                for (x, y) in a.iter().zip(&b) {
                    assert!((*x as i32 - *y as i32).abs() <= 1, "seed {seed}: {x} vs {y}");
                }
                let (mut a, mut b) = (vec![0u8; 8 * 9], vec![0u8; 8 * 9]);
                put_u8_any(&c, &q, 128.0, &mut a, 9);
                unsafe { put_u8_avx2(&c, &q, 128.0, &mut b, 9) };
                for (x, y) in a.iter().zip(&b) {
                    assert!((*x as i32 - *y as i32).abs() <= 1, "seed {seed}: {x} vs {y}");
                }
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
