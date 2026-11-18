//! AI upscaling: Real-ESRGAN's compact general model (`realesr-general-x4v3`,
//! SRVGGNetCompact, BSD-3-Clause; Wang et al. 2021), which makes a picture
//! four times the size with detail made up where the original has none —
//! sharper edges, cleaner textures, less blocking — rather than blurring it
//! as ordinary scaling does. 34 3 × 3 convolutions with PReLU at the
//! original size, then the result rearranged into a 4× larger picture (pixel
//! shuffle) and added to the original blown up.
//!
//! The picture is worked on in tiles (with a margin, so the joins don't
//! show), on the graphics card when there is one, otherwise on all CPU cores.

use super::{gemm_strided, load_weights};
use rayon::prelude::*;
use std::collections::HashMap;

/// How much bigger it makes a picture.
pub const SCALE: usize = 4;
/// Tile size (input pixels) and the margin round each.
const TILE: usize = 160;
const PAD: usize = 20;
const FEAT: usize = 64;
const CONVS: usize = 32;

pub struct Upscaler {
    pub(crate) w: HashMap<String, (Vec<usize>, Vec<f32>)>,
}

/// The model, from the weights built into the app.
pub fn model() -> Option<&'static Upscaler> {
    static M: std::sync::OnceLock<Option<Upscaler>> = std::sync::OnceLock::new();
    M.get_or_init(|| Some(Upscaler { w: load_weights(include_bytes!("../esrgan.bin"))? })).as_ref()
}

impl Upscaler {
    // (A weight the code asks for that the model file hasn't: the file doesn't
    // match. Runs go through `guard`, which turns this into "the model failed".)
    #[allow(clippy::panic)]
    fn t(&self, k: &str) -> &[f32] {
        &self.w.get(k).unwrap_or_else(|| panic!("no weight {k}")).1
    }

    /// One 3 × 3 convolution (padding 1) of `x`, `c` channels laid out with a
    /// one-pixel border of zeros ((h + 2) × (w + 2) each), into the same
    /// layout with `o` channels, then PReLU (`act`). Each tap is a matrix
    /// product straight on the bordered picture, shifted.
    fn conv(&self, x: &[f32], c: usize, h: usize, w: usize, layer: usize, act: bool) -> Vec<f32> {
        let wt = self.t(&format!("body.{layer}.weight"));
        let bias = self.t(&format!("body.{layer}.bias"));
        let o = bias.len();
        let (wp, hp) = (w + 2, h + 2);
        let plane = wp * hp;
        let mut y = vec![0.0f32; o * plane];
        // Positions p = row · wp + column over the h rows (two of each row's
        // are the borders, put right below).
        let n = h * wp - 2;
        let chunk = n.div_ceil(rayon::current_num_threads().max(1) * 2).max(256);
        let starts: Vec<usize> = (0..n).step_by(chunk).collect();
        let parts: Vec<(usize, Vec<f32>)> = starts
            .par_iter()
            .map(|&s0| {
                let len = chunk.min(n - s0);
                let mut out = vec![0.0f32; o * len];
                for tap in 0..9 {
                    let (dy, dx) = (tap / 3, tap % 3);
                    let off = dy * wp + dx + s0;
                    // a: weights (o × c) for this tap (row stride c·9, column stride 9).
                    gemm_strided(o, c, len, &wt[tap..], (c * 9) as isize, 9, &x[off..], plane as isize, 1, &mut out, len as isize, 1.0);
                }
                (s0, out)
            })
            .collect();
        for (s0, out) in parts {
            let len = out.len() / o;
            for ch in 0..o {
                let dst = &mut y[ch * plane + wp + 1 + s0..ch * plane + wp + 1 + s0 + len];
                dst.copy_from_slice(&out[ch * len..(ch + 1) * len]);
            }
        }
        let alpha = if act { Some(self.t(&format!("body.{}.weight", layer + 1))) } else { None };
        y.par_chunks_mut(plane).enumerate().for_each(|(ch, pl)| {
            let (b, a) = (bias[ch], alpha.map(|a| a[ch]));
            for r in 1..=h {
                let row = &mut pl[r * wp..(r + 1) * wp];
                for v in row[1..=w].iter_mut() {
                    let t = *v + b;
                    *v = match a {
                        Some(a) if t < 0.0 => a * t,
                        _ => t,
                    };
                }
                // The borders: positions that wrapped round the row edge.
                row[0] = 0.0;
                row[wp - 1] = 0.0;
            }
        });
        y
    }

    /// One tile: `rgb` (h × w, 0–1, three planes) → four times the size, 0–1, three planes.
    pub fn run_cpu(&self, rgb: &[f32], w: usize, h: usize) -> Vec<f32> {
        let (wp, hp) = (w + 2, h + 2);
        let plane = wp * hp;
        let mut x = vec![0.0f32; 3 * plane];
        for ch in 0..3 {
            for r in 0..h {
                x[ch * plane + (r + 1) * wp + 1..ch * plane + (r + 1) * wp + 1 + w].copy_from_slice(&rgb[(ch * h + r) * w..(ch * h + r + 1) * w]);
            }
        }
        let mut x = self.conv(&x, 3, h, w, 0, true);
        for k in 0..CONVS {
            x = self.conv(&x, FEAT, h, w, 2 + 2 * k, true);
        }
        let x = self.conv(&x, FEAT, h, w, 2 + 2 * CONVS, false);
        shuffle(&x, rgb, w, h, |ch, r, c| x[ch * plane + (r + 1) * wp + c + 1])
    }
}

/// The last layer's 48 channels as a picture four times the size, plus the
/// original blown up (nearest), clamped. `at(channel, row, column)` reads them.
fn shuffle(_x: &[f32], rgb: &[f32], w: usize, h: usize, at: impl Fn(usize, usize, usize) -> f32 + Sync) -> Vec<f32> {
    let (ow, oh) = (w * SCALE, h * SCALE);
    let mut out = vec![0.0f32; 3 * ow * oh];
    out.par_chunks_mut(ow).enumerate().for_each(|(row, line)| {
        let ch = row / oh;
        let y = row % oh;
        let (r, i) = (y / SCALE, y % SCALE);
        for (xx, o) in line.iter_mut().enumerate() {
            let (c, j) = (xx / SCALE, xx % SCALE);
            let v = at(ch * SCALE * SCALE + i * SCALE + j, r, c) + rgb[(ch * h + r) * w + c];
            *o = v.clamp(0.0, 1.0);
        }
    });
    out
}

/// A picture (RGBA, `w` × `h`) four times the size (RGBA; alpha blown up as
/// it is), tile by tile: on the graphics card when there is one.
/// `progress` is told how far it's got (0–1).
pub fn upscale(rgba: &[u8], w: usize, h: usize, progress: impl FnMut(f32)) -> Option<Vec<u8>> {
    crate::guard(|| upscale_now(rgba, w, h, progress))
}

fn upscale_now(rgba: &[u8], w: usize, h: usize, mut progress: impl FnMut(f32)) -> Option<Vec<u8>> {
    let m = model()?;
    let gpu = super::gpu();
    let (ow, oh) = (w * SCALE, h * SCALE);
    let mut out = vec![0u8; ow * oh * 4];
    let tiles_x = w.div_ceil(TILE);
    let tiles_y = h.div_ceil(TILE);
    let total = tiles_x * tiles_y;
    for ty in 0..tiles_y {
        for tx in 0..tiles_x {
            // The tile and its margin (clipped to the picture).
            let (x0, y0) = (tx * TILE, ty * TILE);
            let (x1, y1) = ((x0 + TILE).min(w), (y0 + TILE).min(h));
            let (px0, py0) = (x0.saturating_sub(PAD), y0.saturating_sub(PAD));
            let (px1, py1) = ((x1 + PAD).min(w), (y1 + PAD).min(h));
            let (tw, th) = (px1 - px0, py1 - py0);
            let mut rgb = vec![0.0f32; 3 * tw * th];
            for r in 0..th {
                for c in 0..tw {
                    let s = ((py0 + r) * w + px0 + c) * 4;
                    for ch in 0..3 {
                        rgb[(ch * th + r) * tw + c] = rgba[s + ch] as f32 / 255.0;
                    }
                }
            }
            let big = gpu.and_then(|g| g.upscale_tile(m, &rgb, tw, th)).unwrap_or_else(|| m.run_cpu(&rgb, tw, th));
            let (bw, bh) = (tw * SCALE, th * SCALE);
            // Keep the middle, without the margin.
            for r in (y0 - py0) * SCALE..(y1 - py0) * SCALE {
                for c in (x0 - px0) * SCALE..(x1 - px0) * SCALE {
                    let (oy, ox) = (py0 * SCALE + r, px0 * SCALE + c);
                    let o = (oy * ow + ox) * 4;
                    for ch in 0..3 {
                        out[o + ch] = (big[(ch * bh + r) * bw + c] * 255.0).round() as u8;
                    }
                    out[o + 3] = rgba[((oy / SCALE) * w + ox / SCALE) * 4 + 3];
                }
            }
            progress((ty * tiles_x + tx + 1) as f32 / total as f32);
        }
    }
    Some(out)
}

/// `rgba` (`w` × `h`) shrunk by a whole factor `k` (each output pixel the
/// average of a k × k block), e.g. a 4× upscale brought to 2×.
pub fn shrink(rgba: &[u8], w: usize, h: usize, k: usize) -> Vec<u8> {
    if k <= 1 {
        return rgba.to_vec();
    }
    let (ow, oh) = (w / k, h / k);
    let mut out = vec![0u8; ow * oh * 4];
    out.par_chunks_mut(ow * 4).enumerate().for_each(|(y, line)| {
        for x in 0..ow {
            for ch in 0..4 {
                let mut s = 0u32;
                for dy in 0..k {
                    for dx in 0..k {
                        s += rgba[((y * k + dy) * w + x * k + dx) * 4 + ch] as u32;
                    }
                }
                line[x * 4 + ch] = ((s + (k * k / 2) as u32) / (k * k) as u32) as u8;
            }
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The CPU code gives what the reference (numpy, from the published
    /// model) does on a small test picture, and the tiles join seamlessly.
    #[test]
    fn upscales_as_the_reference_does() {
        let m = model().unwrap();
        // The same picture as the reference script: stripes, a checker, a ramp.
        let (w, h) = (32usize, 24usize);
        let mut rgb = vec![0.0f32; 3 * w * h];
        for y in 0..h {
            for x in 0..w {
                let v = [((x as f32 / 3.0).sin() + 1.0) / 2.0, (((y / 4 + x / 4) % 2) as f32) * 0.8 + 0.1, (x + y) as f32 / (h + w) as f32];
                for ch in 0..3 {
                    rgb[(ch * h + y) * w + x] = ((v[ch] * 255.0).round()) / 255.0;
                }
            }
        }
        let out = m.run_cpu(&rgb, w, h);
        assert_eq!(out.len(), 3 * w * h * 16);
        // The reference's mean (its values agree to within 0.002 everywhere).
        let mean = out.iter().sum::<f32>() / out.len() as f32;
        assert!((mean - 0.4652).abs() < 0.001, "mean {mean}");
        // The graphics card's code agrees with the CPU's.
        if let Some(g) = crate::GpuEncoder::open(crate::model().unwrap(), true) {
            let o2 = g.upscale_tile(m, &rgb, w, h).unwrap();
            let d = o2.iter().zip(&out).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
            assert!(d < 1e-3, "{d}");
        }
        // In tiles, a picture bigger than one comes out as it does whole.
        let (w, h) = (200usize, 30usize);
        let rgba: Vec<u8> = (0..w * h)
            .flat_map(|i| {
                let (x, y) = (i % w, i / w);
                [((x * 7) % 255) as u8, ((y * 13 + x) % 255) as u8, (((x / 9 + y / 7) % 2) * 200) as u8, 255]
            })
            .collect();
        let tiled = upscale(&rgba, w, h, |_| {}).unwrap();
        let mut planes = vec![0.0f32; 3 * w * h];
        for i in 0..w * h {
            for ch in 0..3 {
                planes[ch * w * h + i] = rgba[i * 4 + ch] as f32 / 255.0;
            }
        }
        let whole = m.run_cpu(&planes, w, h);
        let mut worst = 0i32;
        for i in 0..w * h * 16 {
            for ch in 0..3 {
                worst = worst.max((tiled[i * 4 + ch] as i32 - (whole[ch * w * h * 16 + i] * 255.0).round() as i32).abs());
            }
        }
        assert!(worst <= 1, "{worst}");
        assert_eq!(shrink(&tiled, w * 4, h * 4, 2).len(), w * h * 4 * 4);
    }
}
