//! Optical flow with its motion from hardware (as NVIDIA's optical flow engine gives
//! it): a stand-in engine that reports the true motion of a square crossing a textured
//! background; the in-between frame should then have the square where it truly is,
//! and with the engine failing the shaders take over.

use reelforge_core::frame::{f16_to_f32, Pixels};
use reelforge_core::FlowQuality;
use reelforge_render::flowgpu::{self, FlowInterp, HardwareFlow};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

const W: usize = 320;
const H: usize = 180;
const SHIFT: f32 = 24.0;

static CALLS: AtomicU32 = AtomicU32::new(0);
static FAIL: AtomicBool = AtomicBool::new(false);

/// Reports the square's motion (and none for the background), at whatever working size.
struct Truth;

impl HardwareFlow for Truth {
    fn flow(&mut self, a: &[u8], _b: &[u8], w: u32, h: u32, _q: FlowQuality) -> Option<(Vec<[f32; 2]>, Vec<[f32; 2]>)> {
        CALLS.fetch_add(1, Ordering::Relaxed);
        if FAIL.load(Ordering::Relaxed) || a.len() != (w * h) as usize {
            return None;
        }
        let k = w as f32 / W as f32;
        let (x0, x1, y0, y1) = (60.0 * k, (60.0 + 50.0) * k, 60.0 * k, 110.0 * k);
        let inside = |x: f32, y: f32, dx: f32| x >= x0 + dx && x < x1 + dx && y >= y0 && y < y1;
        let mut ab = Vec::new();
        let mut ba = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let (x, y) = (x as f32 + 0.5, y as f32 + 0.5);
                ab.push(if inside(x, y, 0.0) { [SHIFT * k, 0.0] } else { [0.0, 0.0] });
                ba.push(if inside(x, y, SHIFT * k) { [-SHIFT * k, 0.0] } else { [0.0, 0.0] });
            }
        }
        Some((ab, ba))
    }
}

fn hash(x: usize, y: usize) -> u32 {
    let mut h = (x as u32).wrapping_mul(0x9E37_79B1) ^ (y as u32).wrapping_mul(0x85EB_CA77);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    h ^ (h >> 12)
}

/// A bright square (left edge `sx`) over a textured background.
fn picture(sx: f32) -> Vec<u8> {
    let mut v = vec![0u8; W * H * 4];
    for y in 0..H {
        for x in 0..W {
            let i = (y * W + x) * 4;
            let g = 40 + (hash(x / 3, y / 3) % 60) as u8;
            let inside = (x as f32) >= sx && (x as f32) < sx + 50.0 && (60..110).contains(&y);
            let px = if inside { [230, 200 - (hash(x - sx as usize, y) % 40) as u8, 60, 255] } else { [g, g + 10, g, 255] };
            v[i..i + 4].copy_from_slice(&px);
        }
    }
    v
}

/// How far (0–255) the picture's red channel is from the truth over the square's path.
fn error(out: &reelforge_core::frame::FloatPixels, truth: &[u8]) -> f32 {
    let mut s = 0.0;
    let mut n = 0.0;
    for y in 55..115 {
        for x in 55..140 {
            let i = (y * W + x) * 4;
            let a = f16_to_f32(out.rgba[i + 3]).max(1e-6);
            let c = (f16_to_f32(out.rgba[i]) / a).clamp(0.0, 1.0);
            let enc = (if c <= 0.003_130_8 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 }) * 255.0;
            s += (enc - truth[i] as f32).abs();
            n += 1.0;
        }
    }
    s / n
}

#[test]
fn motion_from_hardware_places_the_in_between_frame() {
    let Some((device, queue)) = reelforge_render::headless_device() else { return };
    assert!(!flowgpu::hardware_flow_on());
    flowgpu::set_hardware_flow(Box::new(Truth));
    assert!(flowgpu::hardware_flow_on());
    let a = Pixels::Rgba(Arc::from(picture(60.0)));
    let b = Pixels::Rgba(Arc::from(picture(60.0 + SHIFT)));
    let truth = picture(60.0 + SHIFT / 2.0);
    let mut f = FlowInterp::new(&device, &queue);
    let out = f.interpolate(1, 1, &a, &b, [W as u32, H as u32], 0.5, FlowQuality::Balanced).expect("interpolated");
    assert_eq!(CALLS.load(Ordering::Relaxed), 1, "the hardware was asked");
    let hw_err = error(&out, &truth);
    // A plain cross-fade, for comparison: two ghost squares.
    let fade: Vec<u8> = picture(60.0).iter().zip(picture(60.0 + SHIFT).iter()).map(|(p, q)| ((*p as u16 + *q as u16) / 2) as u8).collect();
    let fade_err = {
        let mut s = 0.0;
        let mut n = 0.0;
        for y in 55..115 {
            for x in 55..140 {
                let i = (y * W + x) * 4;
                s += (fade[i] as f32 - truth[i] as f32).abs();
                n += 1.0;
            }
        }
        s / n
    };
    eprintln!("hardware motion {hw_err:.1}, cross-fade {fade_err:.1}");
    assert!(hw_err < fade_err * 0.4, "with the true motion: {hw_err:.1} vs cross-fade {fade_err:.1}");
    // The same pair again: the motion is kept, not asked for again.
    f.interpolate(1, 1, &a, &b, [W as u32, H as u32], 0.25, FlowQuality::Balanced).expect("again");
    assert_eq!(CALLS.load(Ordering::Relaxed), 1);
    // The engine failing: the shaders work it out (still a picture, still decent).
    FAIL.store(true, Ordering::Relaxed);
    let out = f.interpolate(1, 2, &a, &b, [W as u32, H as u32], 0.5, FlowQuality::Balanced).expect("by the shaders");
    assert_eq!(CALLS.load(Ordering::Relaxed), 2);
    assert!(error(&out, &truth) < fade_err, "the shaders' own motion");
    // Switched off in Settings: not asked at all.
    flowgpu::use_hardware_flow(false);
    assert!(!flowgpu::hardware_flow_on());
    f.interpolate(1, 3, &a, &b, [W as u32, H as u32], 0.5, FlowQuality::Balanced).expect("shaders");
    assert_eq!(CALLS.load(Ordering::Relaxed), 2);
}

#[test]
fn grey_frames_for_the_hardware_are_averaged_down() {
    // 4×2 RGBA: left half white, right half black → 2×1 grey.
    let mut px = Vec::new();
    for _y in 0..2 {
        for x in 0..4 {
            px.extend(if x < 2 { [255u8, 255, 255, 255] } else { [0, 0, 0, 255] });
        }
    }
    let g = flowgpu::grey_at(&Pixels::Rgba(Arc::from(px)), [4, 2], [2, 1]);
    assert_eq!(g.len(), 2);
    assert!(g[0] >= 254 && g[1] == 0, "{g:?}");
    assert!(flowgpu::grey_at(&Pixels::Rgba(Arc::from(vec![0u8; 4])), [1, 1], [0, 0]).is_empty());
}
