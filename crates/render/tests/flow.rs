//! Optical-flow frame interpolation on the graphics card against the true
//! in-between frame: a square passing over a textured background, and two
//! squares crossing, one in front of the other.

use reelforge_core::frame::{f16_to_f32, Pixels};
use reelforge_core::FlowQuality;
use reelforge_render::flowgpu::FlowInterp;
use std::sync::Arc;

const W: usize = 256;
const H: usize = 144;

/// Textured squares (left edge x, top y, size, brightness, texture seed) over a textured background; later ones in front.
fn picture(squares: &[(f32, usize, usize, u8, usize)]) -> Vec<u8> {
    let mut v = vec![0u8; W * H * 4];
    for y in 0..H {
        for x in 0..W {
            let i = (y * W + x) * 4;
            let g = 40 + (hash(x / 3, y / 3) % 60) as u8;
            v[i..i + 4].copy_from_slice(&[g, g + 10, g, 255]);
            for &(sx, sy, n, base, seed) in squares {
                let lx = x as f32 - sx;
                if lx >= 0.0 && lx < n as f32 && y >= sy && y < sy + n {
                    let (lx, ly) = (lx as usize, y - sy);
                    let tex = (hash(lx / 3 + 1000 * seed, ly / 3) % 70) as u8;
                    let c = base.saturating_add(tex);
                    let px = if seed == 2 { [c / 4, c / 4, c.saturating_add(60)] } else { [c, c / 2 + seed as u8 * 20, 255 - c] };
                    v[i..i + 4].copy_from_slice(&[px[0], px[1], px[2], 255]);
                }
            }
        }
    }
    v
}

/// A repeatable scramble of a block's position: a texture with no pattern.
fn hash(x: usize, y: usize) -> u32 {
    let mut h = (x as u32).wrapping_mul(0x9E37_79B1) ^ (y as u32).wrapping_mul(0x85EB_CA77);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    h ^ (h >> 12)
}

fn srgb8(px: &reelforge_core::frame::FloatPixels) -> Vec<u8> {
    px.rgba
        .chunks_exact(4)
        .flat_map(|p| {
            let a = f16_to_f32(p[3]).max(1e-6);
            let enc = |c: f32| {
                let c = (c / a).clamp(0.0, 1.0);
                ((if c <= 0.003_130_8 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 }) * 255.0).round() as u8
            };
            [enc(f16_to_f32(p[0])), enc(f16_to_f32(p[1])), enc(f16_to_f32(p[2]))]
        })
        .collect()
}

/// Mean difference (0–255) over the rgb of the region `x0..x1`.
fn error(a: &[u8], truth: &[u8], x0: usize, x1: usize) -> f32 {
    let mut s = 0.0;
    let mut n = 0.0;
    for y in 0..H {
        for x in x0..x1 {
            for c in 0..3 {
                s += (a[(y * W + x) * 3 + c] as f32 - truth[(y * W + x) * 4 + c] as f32).abs();
                n += 1.0;
            }
        }
    }
    s / n
}

fn rgb(v: &[u8]) -> Vec<u8> {
    v.chunks_exact(4).flat_map(|p| [p[0], p[1], p[2]]).collect()
}

/// The plain cross-fade of two frames, as rgb.
fn blend(a: &[u8], b: &[u8]) -> Vec<u8> {
    a.chunks_exact(4).zip(b.chunks_exact(4)).flat_map(|(p, q)| [0, 1, 2].map(|c| ((p[c] as u16 + q[c] as u16) / 2) as u8)).collect()
}

fn check(name: &str, a: Vec<u8>, b: Vec<u8>, truth: Vec<u8>, region: (usize, usize)) {
    let Some((device, queue)) = reelforge_render::headless_device() else { return };
    let mut f = FlowInterp::new(&device, &queue);
    let pa = Pixels::Rgba(Arc::from(a.clone()));
    let pb = Pixels::Rgba(Arc::from(b.clone()));
    let base = error(&blend(&a, &b), &truth, region.0, region.1);
    for q in [FlowQuality::Fast, FlowQuality::Balanced, FlowQuality::Best] {
        let out = f.interpolate(1, q as u64, &pa, &pb, [W as u32, H as u32], 0.5, q).expect("interpolated");
        assert_eq!((out.width, out.height), (W as u32, H as u32));
        let got = srgb8(&out);
        let e = error(&got, &truth, region.0, region.1);
        let whole = error(&got, &truth, 0, W);
        eprintln!("{name} {q:?}: {e:.2} where it moves ({whole:.2} overall) against {base:.2} for a cross-fade");
        assert!(e < base * 0.35, "{name} {q:?}: {e} against a cross-fade's {base}");
    }
    // Ends: t = 0 is the first frame, t = 1 the second.
    let first = srgb8(&f.interpolate(1, 99, &pa, &pb, [W as u32, H as u32], 0.0, FlowQuality::Fast).unwrap());
    assert!(error(&first, &a, 0, W) < 1.0);
}

#[test]
fn a_square_passing_over_the_background() {
    // 40 px right; halfway it's 20 px on. What it uncovers and covers is background.
    check("passing", picture(&[(60.0, 50, 40, 160, 1)]), picture(&[(100.0, 50, 40, 160, 1)]), picture(&[(80.0, 50, 40, 160, 1)]), (50, 150));
}

#[test]
fn two_squares_crossing() {
    // A dark one going left, a bright one going right in front of it: they overlap halfway.
    let back = |x: f32| (x, 26, 40, 20, 2);
    let front = |x: f32| (x, 52, 40, 170, 0);
    check("crossing", picture(&[back(140.0), front(70.0)]), picture(&[back(116.0), front(118.0)]), picture(&[back(128.0), front(94.0)]), (60, 190));
}

#[test]
fn still_pictures_stay_still() {
    let Some((device, queue)) = reelforge_render::headless_device() else { return };
    let mut f = FlowInterp::new(&device, &queue);
    let a = picture(&[(60.0, 50, 40, 160, 1)]);
    let pa = Pixels::Rgba(Arc::from(a.clone()));
    for level in [3, 2, 1, 0] {
        f.interpolate(1, 7, &pa, &pa, [W as u32, H as u32], 0.5, FlowQuality::Fast).unwrap();
        let (size, ab, _) = f.motion(1, level).unwrap();
        let worst = ab.iter().map(|m| m[0].abs().max(m[1].abs())).fold(0.0f32, f32::max);
        assert!(worst < 0.5, "level {level} {size:?}: moves up to {worst}");
    }
}

#[test]
fn a_dark_square_going_left() {
    let back = |x: f32| (x, 26, 40, 20, 2);
    check("leftwards", picture(&[back(140.0)]), picture(&[back(116.0)]), picture(&[back(128.0)]), (60, 190));
}

/// Four times the size (1024 × 576, where the motion is matched on sparse
/// patches): still close to the true in-between frame.
#[test]
fn large_frames() {
    let Some((device, queue)) = reelforge_render::headless_device() else { return };
    let up = |v: Vec<u8>| -> Vec<u8> {
        let mut o = vec![0u8; W * H * 16 * 4];
        for y in 0..H * 4 {
            for x in 0..W * 4 {
                let s = ((y / 4) * W + x / 4) * 4;
                o[(y * W * 4 + x) * 4..][..4].copy_from_slice(&v[s..s + 4]);
            }
        }
        o
    };
    let a = up(picture(&[(60.0, 50, 40, 160, 1)]));
    let b = up(picture(&[(100.0, 50, 40, 160, 1)]));
    let truth = up(picture(&[(80.0, 50, 40, 160, 1)]));
    let mut f = FlowInterp::new(&device, &queue);
    let (pa, pb) = (Pixels::Rgba(Arc::from(a.clone())), Pixels::Rgba(Arc::from(b.clone())));
    for q in [FlowQuality::Balanced] {
        let started = std::time::Instant::now();
        let out = srgb8(&f.interpolate(1, q as u64 + 10, &pa, &pb, [W as u32 * 4, H as u32 * 4], 0.5, q).unwrap());
        let (mut e, mut base, mut n) = (0.0, 0.0, 0.0);
        for i in 0..W * H * 16 {
            for c in 0..3 {
                e += (out[i * 3 + c] as f32 - truth[i * 4 + c] as f32).abs();
                base += ((a[i * 4 + c] as f32 + b[i * 4 + c] as f32) / 2.0 - truth[i * 4 + c] as f32).abs();
                n += 1.0;
            }
        }
        eprintln!("large {q:?}: {:.2} against {:.2} for a cross-fade, in {:?}", e / n, base / n, started.elapsed());
        assert!(e < base * 0.35, "{q:?}");
    }
}
