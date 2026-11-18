//! Finding faces and following them through a clip, for the Face beauty
//! effect.
//!
//! Faces are found in each frame with Viola–Jones boosted Haar cascades
//! (OpenCV's `haarcascade_frontalface_alt2` and `haarcascade_profileface`,
//! the latter run on the picture mirrored too, for faces turned either way;
//! Intel License Agreement, see the third-party notices): an image pyramid,
//! every 20 × 20 window tested stage by stage, rejected as early as possible,
//! and windows found many times over grouped into one face. Then the faces
//! are followed from frame to frame: joined up by overlap, short gaps filled
//! in, faces seen only for a moment (most false finds) dropped, and the
//! boxes smoothed so they don't jitter.

use rayon::prelude::*;
use serde::Deserialize;

#[derive(Deserialize)]
struct Raw {
    w: usize,
    h: usize,
    stages: Vec<(f32, Vec<(Vec<f64>, Vec<f32>)>)>,
    features: Vec<Vec<(i32, i32, i32, i32, f32)>>,
}

struct Tree {
    /// (left, right, feature, threshold) per node; ≤ 0 children are leaves (−index).
    nodes: Vec<(i32, i32, usize, f32)>,
    leaves: Vec<f32>,
}

struct Stage {
    threshold: f32,
    trees: Vec<Tree>,
}

pub struct Cascade {
    w: usize,
    h: usize,
    stages: Vec<Stage>,
    features: Vec<Vec<(i32, i32, i32, i32, f32)>>,
}

impl Cascade {
    fn parse(json: &str) -> Option<Cascade> {
        let r: Raw = serde_json::from_str(json).ok()?;
        let stages = r
            .stages
            .into_iter()
            .map(|(threshold, trees)| Stage {
                threshold,
                trees: trees.into_iter().map(|(n, leaves)| Tree { nodes: n.chunks(4).map(|c| (c[0] as i32, c[1] as i32, c[2] as usize, c[3] as f32)).collect(), leaves }).collect(),
            })
            .collect();
        Some(Cascade { w: r.w, h: r.h, stages, features: r.features })
    }

    /// Does the window at (x, y) of integral image `ii` (and squared `sq`), `iw` across, hold a face?
    fn test(&self, ii: &[f64], sq: &[f64], iw: usize, x: usize, y: usize) -> bool {
        let rect = |a: &[f64], x0: usize, y0: usize, w: usize, h: usize| a[(y0 + h) * iw + x0 + w] - a[y0 * iw + x0 + w] - a[(y0 + h) * iw + x0] + a[y0 * iw + x0];
        // Normalised by the window's contrast (inside a one-pixel border).
        let (nw, nh) = (self.w - 2, self.h - 2);
        let area = (nw * nh) as f64;
        let s = rect(ii, x + 1, y + 1, nw, nh);
        let s2 = rect(sq, x + 1, y + 1, nw, nh);
        let nf = area * s2 - s * s;
        let inv = if nf > 0.0 { 1.0 / nf.sqrt() } else { 1.0 };
        for st in &self.stages {
            let mut sum = 0.0f32;
            for t in &st.trees {
                let mut idx = 0i32;
                let leaf = loop {
                    let (l, r, f, th) = t.nodes[idx as usize];
                    let mut v = 0.0f64;
                    for &(rx, ry, rw, rh, wt) in &self.features[f] {
                        v += wt as f64 * rect(ii, x + rx as usize, y + ry as usize, rw as usize, rh as usize);
                    }
                    let next = if ((v * inv) as f32) < th { l } else { r };
                    if next <= 0 {
                        break t.leaves[(-next) as usize];
                    }
                    idx = next;
                };
                sum += leaf;
            }
            if sum < st.threshold {
                return false;
            }
        }
        true
    }
}

fn cascades() -> &'static [Cascade; 2] {
    static C: std::sync::OnceLock<[Cascade; 2]> = std::sync::OnceLock::new();
    C.get_or_init(|| [Cascade::parse(include_str!("../faces_frontal.json")).expect("frontal cascade"), Cascade::parse(include_str!("../faces_profile.json")).expect("profile cascade")])
}

/// A face: its box (left, top, width, height) in the picture's pixels.
pub type Rect = [f32; 4];

fn integral(gray: &[f32], w: usize, h: usize) -> (Vec<f64>, Vec<f64>) {
    let iw = w + 1;
    let mut ii = vec![0.0f64; iw * (h + 1)];
    let mut sq = vec![0.0f64; iw * (h + 1)];
    for y in 0..h {
        let (mut s, mut s2) = (0.0f64, 0.0f64);
        for x in 0..w {
            let v = gray[y * w + x] as f64;
            s += v;
            s2 += v * v;
            ii[(y + 1) * iw + x + 1] = ii[y * iw + x + 1] + s;
            sq[(y + 1) * iw + x + 1] = sq[y * iw + x + 1] + s2;
        }
    }
    (ii, sq)
}

/// `gray` (0–255) shrunk by `k` (area average... a bilinear sample is enough here).
fn shrink(gray: &[f32], w: usize, h: usize, k: f32) -> (Vec<f32>, usize, usize) {
    let (nw, nh) = (((w as f32) / k) as usize, ((h as f32) / k) as usize);
    let mut out = vec![0.0f32; nw * nh];
    for y in 0..nh {
        for x in 0..nw {
            let (sx, sy) = (((x as f32 + 0.5) * k - 0.5).max(0.0), ((y as f32 + 0.5) * k - 0.5).max(0.0));
            let (x0, y0) = (sx as usize, sy as usize);
            let (x1, y1) = ((x0 + 1).min(w - 1), (y0 + 1).min(h - 1));
            let (fx, fy) = (sx - x0 as f32, sy - y0 as f32);
            let g = |x: usize, y: usize| gray[y * w + x];
            out[y * nw + x] = (g(x0, y0) * (1.0 - fx) + g(x1, y0) * fx) * (1.0 - fy) + (g(x0, y1) * (1.0 - fx) + g(x1, y1) * fx) * fy;
        }
    }
    (out, nw, nh)
}

/// Every window cascade `c` says is a face, at every scale (boxes in `gray`'s pixels).
fn windows(c: &Cascade, gray: &[f32], w: usize, h: usize, mirrored: bool, min_size: f32) -> Vec<Rect> {
    let mut scales = Vec::new();
    let mut k = (min_size / c.w as f32).max(1.0);
    while (w as f32 / k) as usize > c.w && (h as f32 / k) as usize > c.h {
        scales.push(k);
        k *= 1.1;
    }
    scales
        .par_iter()
        .flat_map_iter(|&k| {
            let (img, sw, sh) = shrink(gray, w, h, k);
            let img = if mirrored { (0..sh).flat_map(|y| (0..sw).rev().map(move |x| (y, x))).map(|(y, x)| img[y * sw + x]).collect() } else { img };
            let (ii, sq) = integral(&img, sw, sh);
            let step = if k > 2.0 { 1 } else { 2 };
            let mut found = Vec::new();
            for y in (0..sh - c.h).step_by(step) {
                for x in (0..sw - c.w).step_by(step) {
                    if c.test(&ii, &sq, sw + 1, x, y) {
                        let x = if mirrored { sw - c.w - x } else { x };
                        found.push([x as f32 * k, y as f32 * k, c.w as f32 * k, c.h as f32 * k]);
                    }
                }
            }
            found
        })
        .collect()
}

fn iou(a: &Rect, b: &Rect) -> f32 {
    let ix = (a[0] + a[2]).min(b[0] + b[2]) - a[0].max(b[0]);
    let iy = (a[1] + a[3]).min(b[1] + b[3]) - a[1].max(b[1]);
    if ix <= 0.0 || iy <= 0.0 {
        return 0.0;
    }
    let i = ix * iy;
    i / (a[2] * a[3] + b[2] * b[3] - i)
}

/// Windows found many times over, grouped (as OpenCV's `groupRectangles`):
/// clusters of similar boxes with more than `min` members, averaged.
fn group(rects: &[Rect], min: usize) -> Vec<(Rect, usize)> {
    let n = rects.len();
    let mut label: Vec<usize> = (0..n).collect();
    fn find(l: &mut [usize], i: usize) -> usize {
        let mut r = i;
        while l[r] != r {
            r = l[r];
        }
        let mut i = i;
        while l[i] != r {
            let next = l[i];
            l[i] = r;
            i = next;
        }
        r
    }
    for i in 0..n {
        for j in i + 1..n {
            let (a, b) = (&rects[i], &rects[j]);
            let d = 0.2 * (a[2].min(b[2]) + a[3].min(b[3])) * 0.5;
            if (a[0] - b[0]).abs() <= d && (a[1] - b[1]).abs() <= d && (a[0] + a[2] - b[0] - b[2]).abs() <= d && (a[1] + a[3] - b[1] - b[3]).abs() <= d {
                let (ri, rj) = (find(&mut label, i), find(&mut label, j));
                if ri != rj {
                    label[ri] = rj;
                }
            }
        }
    }
    let mut sums: std::collections::HashMap<usize, ([f32; 4], usize)> = Default::default();
    for i in 0..n {
        let r = find(&mut label, i);
        let e = sums.entry(r).or_insert(([0.0; 4], 0));
        for k in 0..4 {
            e.0[k] += rects[i][k];
        }
        e.1 += 1;
    }
    let mut out: Vec<(Rect, usize)> = sums.into_values().filter(|(_, c)| *c > min).map(|(s, c)| (s.map(|v| v / c as f32), c)).collect();
    out.sort_by_key(|o| std::cmp::Reverse(o.1));
    out
}

/// The faces in a picture (`gray`, 0–255, `w` × `h`), at least `min_size` pixels across.
pub fn detect(gray: &[f32], w: usize, h: usize, min_size: f32) -> Vec<Rect> {
    let [frontal, profile] = cascades();
    let mut found: Vec<(Rect, usize)> = group(&windows(frontal, gray, w, h, false, min_size), 3);
    // Faces turned to the side, each way; where a frontal face was found already, it's that.
    for mirrored in [false, true] {
        for (r, n) in group(&windows(profile, gray, w, h, mirrored, min_size), 3) {
            if !found.iter().any(|(f, _)| iou(f, &r) > 0.3) {
                found.push((r, n));
            }
        }
    }
    // A box inside a bigger, surer one is part of it.
    let mut keep: Vec<(Rect, usize)> = Vec::new();
    found.sort_by_key(|f| std::cmp::Reverse(f.1));
    for (r, n) in found {
        let inside = keep.iter().any(|(k, _)| r[0] >= k[0] - k[2] * 0.2 && r[1] >= k[1] - k[3] * 0.2 && r[0] + r[2] <= k[0] + k[2] * 1.2 && r[1] + r[3] <= k[1] + k[3] * 1.2);
        if !inside {
            keep.push((r, n));
        }
    }
    keep.into_iter().map(|(r, _)| r).collect()
}

/// RGBA → grey (0–255).
pub fn gray(rgba: &[u8], w: usize, h: usize) -> Vec<f32> {
    rgba.chunks_exact(4).take(w * h).map(|p| 0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32).collect()
}

/// Faces found frame by frame (`frames[i]`), followed through: each track
/// is a face's box in every frame from where it starts to where it ends
/// (`None` before and after). Faces seen in fewer than `min_frames` frames
/// are dropped; gaps of up to `max_gap` frames are filled in; boxes are
/// smoothed over a few frames.
pub fn track(frames: &[Vec<Rect>], min_frames: usize, max_gap: usize) -> Vec<Vec<Option<Rect>>> {
    let n = frames.len();
    // (last frame seen, boxes by frame)
    let mut tracks: Vec<(usize, Vec<Option<Rect>>)> = Vec::new();
    for (i, found) in frames.iter().enumerate() {
        let mut used = vec![false; found.len()];
        // Each live track takes the best-matching face (overlap, or a short move).
        let mut order: Vec<usize> = (0..tracks.len()).filter(|&t| i - tracks[t].0 <= max_gap + 1).collect();
        order.sort_by_key(|&t| i - tracks[t].0);
        for t in order {
            let last = tracks[t].1[tracks[t].0].unwrap();
            let best = found
                .iter()
                .enumerate()
                .filter(|(k, _)| !used[*k])
                .map(|(k, r)| {
                    let (cx, cy) = (r[0] + r[2] / 2.0 - last[0] - last[2] / 2.0, r[1] + r[3] / 2.0 - last[1] - last[3] / 2.0);
                    let near = (cx * cx + cy * cy).sqrt() / last[2].max(1.0);
                    let size = (r[2] / last[2].max(1.0)).ln().abs();
                    (k, iou(r, &last).max(if near < 0.5 && size < 0.4 { 0.3 } else { 0.0 }))
                })
                .filter(|(_, s)| *s >= 0.2)
                .max_by(|a, b| a.1.total_cmp(&b.1));
            if let Some((k, _)) = best {
                used[k] = true;
                tracks[t].1[i] = Some(found[k]);
                tracks[t].0 = i;
            }
        }
        for (k, r) in found.iter().enumerate() {
            if !used[k] {
                let mut v = vec![None; n];
                v[i] = Some(*r);
                tracks.push((i, v));
            }
        }
    }
    tracks
        .into_iter()
        .filter_map(|(_, mut v)| {
            let seen: Vec<usize> = (0..n).filter(|&i| v[i].is_some()).collect();
            if seen.len() < min_frames.min(n) {
                return None;
            }
            // Fill the gaps between sightings.
            for w in seen.windows(2) {
                let (a, b) = (w[0], w[1]);
                let (ra, rb) = (v[a].unwrap(), v[b].unwrap());
                for (i, slot) in v.iter_mut().enumerate().take(b).skip(a + 1) {
                    let u = (i - a) as f32 / (b - a) as f32;
                    *slot = Some([0, 1, 2, 3].map(|k| ra[k] + (rb[k] - ra[k]) * u));
                }
            }
            // Smooth over ±3 frames.
            let raw = v.clone();
            for i in 0..n {
                if raw[i].is_none() {
                    continue;
                }
                let mut s = [0.0f32; 4];
                let mut c = 0.0;
                for j in i.saturating_sub(3)..(i + 4).min(n) {
                    if let Some(r) = raw[j] {
                        let wgt = 1.0 / (1.0 + (j as f32 - i as f32).abs());
                        for k in 0..4 {
                            s[k] += r[k] * wgt;
                        }
                        c += wgt;
                    }
                }
                v[i] = Some(s.map(|x| x / c));
            }
            Some(v)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Following: a face seen in most frames is one track, its gap filled in;
    /// a face seen for a frame is dropped.
    #[test]
    fn faces_are_followed_and_flukes_dropped() {
        let mut frames: Vec<Vec<Rect>> = (0..20).map(|i| vec![[100.0 + i as f32 * 3.0, 50.0, 60.0, 60.0]]).collect();
        frames[7].clear();
        frames[8].clear();
        frames[12].push([400.0, 300.0, 40.0, 40.0]);
        let t = track(&frames, 6, 4);
        assert_eq!(t.len(), 1);
        let r = t[0][7].unwrap();
        assert!((r[0] - 121.0).abs() < 2.0, "{r:?}");
        assert!(t[0].iter().all(|r| r.is_some()));
    }

    /// The cascades load, and find nothing in noise.
    #[test]
    fn no_faces_in_noise() {
        let (w, h) = (160usize, 120usize);
        let mut s = 1u32;
        let g: Vec<f32> = (0..w * h)
            .map(|_| {
                s = s.wrapping_mul(1664525).wrapping_add(1013904223);
                (s >> 24) as f32
            })
            .collect();
        assert!(detect(&g, w, h, 24.0).is_empty());
    }
}
