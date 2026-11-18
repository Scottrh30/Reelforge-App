//! Content-aware fill (object removal): filling the area of a clip that
//! "fill area" masks cover with what would be behind it.
//!
//! Each frame is filled from the other frames first: the camera's motion
//! between neighbouring frames is worked out from details outside the
//! masked area (a perspective map per pair, [`motion`]), so a pixel hidden
//! in one frame can be fetched from a frame where it shows ([`Filler`]).
//! What no frame shows is made up from the picture round it ([`synthesize`]:
//! PatchMatch-based image completion over a pyramid, after Wexler, Shechtman
//! and Irani), and carried on to the next frames the same way, so it holds
//! steady rather than boiling. [`smooth_fill`] fills from the edges alone.

use crate::planar::{fit_homography, good_features};
use crate::track::{FeatureTracker, Luma, Pyramid, MIN_SCORE};
use crate::transform::Homography;

/// A picture: sRGB values 0–255 as floats, row by row.
#[derive(Clone, Debug, PartialEq)]
pub struct Image {
    pub w: usize,
    pub h: usize,
    pub px: Vec<[f32; 3]>,
}

impl Image {
    pub fn from_rgba(rgba: &[u8], w: usize, h: usize) -> Self {
        Image { w, h, px: rgba.chunks_exact(4).take(w * h).map(|c| [c[0] as f32, c[1] as f32, c[2] as f32]).collect() }
    }
    pub fn to_rgba(&self) -> Vec<u8> {
        self.px.iter().flat_map(|p| [p[0].round().clamp(0.0, 255.0) as u8, p[1].round().clamp(0.0, 255.0) as u8, p[2].round().clamp(0.0, 255.0) as u8, 255]).collect()
    }
    /// Rec. 709 luma, 0–255 (as the motion tracker takes it).
    pub fn luma(&self) -> Vec<f32> {
        self.px.iter().map(|p| 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2]).collect()
    }
    /// Bilinear sample at pixel position `p` (centres at +0.5); `None` outside.
    pub fn sample(&self, p: [f32; 2]) -> Option<[f32; 3]> {
        let (x, y) = (p[0] - 0.5, p[1] - 0.5);
        if !(x >= -0.5 && y >= -0.5 && x <= self.w as f32 - 0.5 && y <= self.h as f32 - 0.5) {
            return None;
        }
        let (x, y) = (x.clamp(0.0, (self.w - 1) as f32), y.clamp(0.0, (self.h - 1) as f32));
        let (x0, y0) = (x.floor() as usize, y.floor() as usize);
        let (x1, y1) = ((x0 + 1).min(self.w - 1), (y0 + 1).min(self.h - 1));
        let (fx, fy) = (x - x0 as f32, y - y0 as f32);
        let g = |x: usize, y: usize| self.px[y * self.w + x];
        let (a, b, c, d) = (g(x0, y0), g(x1, y0), g(x0, y1), g(x1, y1));
        Some(std::array::from_fn(|k| (a[k] * (1.0 - fx) + b[k] * fx) * (1.0 - fy) + (c[k] * (1.0 - fx) + d[k] * fx) * fy))
    }
}

/// Grow a hole (true = unknown) by `r` pixels (a square neighbourhood, done as two passes).
pub fn dilate(hole: &[bool], w: usize, h: usize, r: usize) -> Vec<bool> {
    if r == 0 {
        return hole.to_vec();
    }
    let mut a = vec![false; w * h];
    for y in 0..h {
        let mut last: isize = -(r as isize) - 1;
        for x in 0..w {
            if hole[y * w + x] {
                last = x as isize;
            }
            if x as isize - last <= r as isize {
                a[y * w + x] = true;
            }
        }
        let mut next: isize = (w + r + 1) as isize;
        for x in (0..w).rev() {
            if hole[y * w + x] {
                next = x as isize;
            }
            if next - x as isize <= r as isize {
                a[y * w + x] = true;
            }
        }
    }
    let mut b = vec![false; w * h];
    for x in 0..w {
        let mut last: isize = -(r as isize) - 1;
        for y in 0..h {
            if a[y * w + x] {
                last = y as isize;
            }
            if y as isize - last <= r as isize {
                b[y * w + x] = true;
            }
        }
        let mut next: isize = (h + r + 1) as isize;
        for y in (0..h).rev() {
            if a[y * w + x] {
                next = y as isize;
            }
            if next - y as isize <= r as isize {
                b[y * w + x] = true;
            }
        }
    }
    b
}

/// The smallest box (x0, y0, x1, y1; exclusive ends) round the hole, if any.
pub fn bounds(hole: &[bool], w: usize, h: usize) -> Option<[usize; 4]> {
    let mut b = [usize::MAX, usize::MAX, 0, 0];
    for y in 0..h {
        for x in 0..w {
            if hole[y * w + x] {
                b = [b[0].min(x), b[1].min(y), b[2].max(x + 1), b[3].max(y + 1)];
            }
        }
    }
    (b[0] != usize::MAX).then_some(b)
}

/// Fill the hole smoothly from its edges (push–pull over a pyramid): every
/// unknown pixel takes a blend of the known ones round it, nearer counting more.
pub fn smooth_fill(img: &Image, hole: &[bool]) -> Image {
    let (w, h) = (img.w, img.h);
    // (colour × weight, weight) at each level.
    let mut levels: Vec<(usize, usize, Vec<[f32; 4]>)> = vec![(w, h, img.px.iter().zip(hole).map(|(p, &u)| if u { [0.0; 4] } else { [p[0], p[1], p[2], 1.0] }).collect())];
    while levels.last().is_some_and(|(lw, lh, _)| *lw > 1 || *lh > 1) {
        let Some((lw, lh, v)) = levels.last() else { break };
        let (nw, nh) = (lw.div_ceil(2), lh.div_ceil(2));
        let mut n = vec![[0.0f32; 4]; nw * nh];
        for y in 0..*lh {
            for x in 0..*lw {
                let s = v[y * lw + x];
                let o = &mut n[(y / 2) * nw + x / 2];
                for k in 0..4 {
                    o[k] += s[k];
                }
            }
        }
        // Keep the weights at most 1, so known pixels always win when pulling back up.
        for o in &mut n {
            if o[3] > 1.0 {
                let k = 1.0 / o[3];
                *o = [o[0] * k, o[1] * k, o[2] * k, 1.0];
            }
        }
        levels.push((nw, nh, n));
    }
    // Pull: each level's gaps from the coarser one.
    for i in (0..levels.len() - 1).rev() {
        let (cw, ch, coarse) = levels[i + 1].clone();
        let (lw, lh, fine) = &mut levels[i];
        for y in 0..*lh {
            for x in 0..*lw {
                // Bilinear from the coarse level.
                let fx = ((x as f32 + 0.5) / 2.0 - 0.5).clamp(0.0, (cw - 1) as f32);
                let fy = ((y as f32 + 0.5) / 2.0 - 0.5).clamp(0.0, (ch - 1) as f32);
                let (x0, y0) = (fx.floor() as usize, fy.floor() as usize);
                let (x1, y1) = ((x0 + 1).min(cw - 1), (y0 + 1).min(ch - 1));
                let (ax, ay) = (fx - x0 as f32, fy - y0 as f32);
                let g = |x: usize, y: usize| {
                    let c = coarse[y * cw + x];
                    if c[3] > 0.0 {
                        [c[0] / c[3], c[1] / c[3], c[2] / c[3]]
                    } else {
                        [0.0; 3]
                    }
                };
                let (a, b, c, d) = (g(x0, y0), g(x1, y0), g(x0, y1), g(x1, y1));
                let up: [f32; 3] = std::array::from_fn(|k| (a[k] * (1.0 - ax) + b[k] * ax) * (1.0 - ay) + (c[k] * (1.0 - ax) + d[k] * ax) * ay);
                let f = &mut fine[y * *lw + x];
                let wgt = f[3].min(1.0);
                let own = if f[3] > 0.0 { [f[0] / f[3], f[1] / f[3], f[2] / f[3]] } else { [0.0; 3] };
                *f = [own[0] * wgt + up[0] * (1.0 - wgt), own[1] * wgt + up[1] * (1.0 - wgt), own[2] * wgt + up[2] * (1.0 - wgt), 1.0];
            }
        }
    }
    let (_, _, top) = &levels[0];
    Image { w, h, px: img.px.iter().zip(hole).zip(top).map(|((p, &u), t)| if u { [t[0], t[1], t[2]] } else { *p }).collect() }
}

/// splitmix64 as a small random source.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

/// Patch radius (patches are 9 × 9).
const R: isize = 4;

/// Make up the hole's contents from the picture round it (exemplar-based
/// inpainting, after Criminisi, Pérez and Toyama): patch by patch from the
/// edge inwards, the place where lines run into the hole and most is known
/// first, each copied from the patch elsewhere that best matches what's
/// known round it (found by sampling, following where the neighbours were
/// copied from, and refining). Only the part of the picture round the hole
/// is searched. Returns the picture with the hole filled.
pub fn synthesize(img: &Image, hole: &[bool], seed: u64) -> Image {
    let Some(b) = bounds(hole, img.w, img.h) else { return img.clone() };
    // Search round the hole: twice as far again as it's big, at least 32 px.
    let m = ((b[2] - b[0]).max(b[3] - b[1]) * 2).max(32);
    let (x0, y0) = (b[0].saturating_sub(m), b[1].saturating_sub(m));
    let (x1, y1) = ((b[2] + m).min(img.w), (b[3] + m).min(img.h));
    let (w, h) = (x1 - x0, y1 - y0);
    let (wi, hi) = (w as isize, h as isize);
    let mut px: Vec<[f32; 3]> = Vec::with_capacity(w * h);
    let mut unknown: Vec<bool> = Vec::with_capacity(w * h);
    for y in y0..y1 {
        px.extend_from_slice(&img.px[y * img.w + x0..y * img.w + x1]);
        unknown.extend_from_slice(&hole[y * img.w + x0..y * img.w + x1]);
    }
    // Patches that can be copied from: wholly known and inside.
    let near = dilate(&unknown, w, h, R as usize);
    let ok_src = |x: isize, y: isize| x >= R && y >= R && x < wi - R && y < hi - R && !near[(y * wi + x) as usize];
    let sources: Vec<(isize, isize)> = (R..hi - R).flat_map(|y| (R..wi - R).map(move |x| (x, y))).filter(|&(x, y)| ok_src(x, y)).collect();
    if sources.is_empty() {
        return smooth_fill(img, hole);
    }
    let lum = |p: [f32; 3]| 0.2126 * p[0] + 0.7152 * p[1] + 0.0722 * p[2];
    let mut conf: Vec<f32> = unknown.iter().map(|&u| if u { 0.0 } else { 1.0 }).collect();
    // Where each pixel was copied from (an offset), for following neighbours.
    let mut from: Vec<Option<(isize, isize)>> = vec![None; w * h];
    let mut rng = Rng(seed ^ 0x5eed_c0de);
    let mut left = unknown.iter().filter(|&&u| u).count();
    let inside = |x: isize, y: isize| x >= 0 && y >= 0 && x < wi && y < hi;
    let mut guard = 0;
    while left > 0 && guard < w * h {
        guard += 1;
        // The front: unknown pixels next to known ones.
        let mut best: Option<((isize, isize), f32)> = None;
        // (The front is always inside the hole's box.)
        for y in (b[1] - y0) as isize..(b[3] - y0) as isize {
            for x in (b[0] - x0) as isize..(b[2] - x0) as isize {
                let i = (y * wi + x) as usize;
                if !unknown[i] {
                    continue;
                }
                let edge = [(1, 0), (-1, 0), (0, 1), (0, -1)].iter().any(|&(dx, dy)| inside(x + dx, y + dy) && !unknown[((y + dy) * wi + x + dx) as usize]);
                if !edge {
                    continue;
                }
                // How much round it is known (and how sure).
                let (mut c, mut n) = (0.0f32, 0.0f32);
                for dy in -R..=R {
                    for dx in -R..=R {
                        if inside(x + dx, y + dy) {
                            c += conf[((y + dy) * wi + x + dx) as usize];
                            n += 1.0;
                        }
                    }
                }
                let c = c / n.max(1.0);
                // Lines running into the hole: the picture's gradient turned
                // along its edge, against the front's normal.
                let k = |x: isize, y: isize| -> Option<f32> { (inside(x, y) && !unknown[(y * wi + x) as usize]).then(|| lum(px[(y * wi + x) as usize])) };
                let (mut gx, mut gy) = (0.0f32, 0.0f32);
                let mut best_g = 0.0;
                for dy in -1..=1 {
                    for dx in -1..=1 {
                        let (qx, qy) = (x + dx, y + dy);
                        if let (Some(l), Some(r), Some(u), Some(d)) = (k(qx - 1, qy), k(qx + 1, qy), k(qx, qy - 1), k(qx, qy + 1)) {
                            let (ax, ay) = ((r - l) * 0.5, (d - u) * 0.5);
                            let g = ax * ax + ay * ay;
                            if g > best_g {
                                best_g = g;
                                (gx, gy) = (ax, ay);
                            }
                        }
                    }
                }
                let u = |x: isize, y: isize| if inside(x, y) && unknown[(y * wi + x) as usize] { 1.0f32 } else { 0.0 };
                let (nx, ny) = (u(x + 1, y) - u(x - 1, y), u(x, y + 1) - u(x, y - 1));
                let nl = (nx * nx + ny * ny).sqrt().max(1e-3);
                // Isophote (the gradient turned) against the normal.
                let d = ((-gy) * nx / nl + gx * ny / nl).abs() / 255.0;
                let pr = c * (d + 0.05);
                if best.is_none_or(|(_, b)| pr > b) {
                    best = Some(((x, y), pr));
                }
            }
        }
        let Some(((tx, ty), _)) = best else { break };
        // The best source for the patch round (tx, ty), by what's known in it.
        let dist = |s: (isize, isize), limit: f32| -> f32 {
            let mut d = 0.0;
            let mut n = 0;
            for dy in -R..=R {
                for dx in -R..=R {
                    let (qx, qy) = (tx + dx, ty + dy);
                    if !inside(qx, qy) || unknown[(qy * wi + qx) as usize] {
                        continue;
                    }
                    let a = px[(qy * wi + qx) as usize];
                    let c = px[((s.1 + dy) * wi + s.0 + dx) as usize];
                    d += (a[0] - c[0]).powi(2) + (a[1] - c[1]).powi(2) + (a[2] - c[2]).powi(2);
                    n += 1;
                }
                if d > limit {
                    return f32::MAX;
                }
            }
            if n == 0 { f32::MAX } else { d }
        };
        let mut cand: Vec<(isize, isize)> = Vec::with_capacity(900);
        // Where the known neighbours were copied from, carried over.
        for dy in -R..=R {
            for dx in -R..=R {
                let (qx, qy) = (tx + dx, ty + dy);
                if inside(qx, qy) {
                    if let Some(o) = from[(qy * wi + qx) as usize] {
                        cand.push((tx + o.0, ty + o.1));
                    }
                }
            }
        }
        for _ in 0..700 {
            cand.push(sources[rng.below(sources.len())]);
        }
        let (mut bs, mut bd) = ((0, 0), f32::MAX);
        for c in cand {
            if ok_src(c.0, c.1) {
                let d = dist(c, bd);
                if d < bd {
                    (bs, bd) = (c, d);
                }
            }
        }
        // Refine round the best.
        for r in [6isize, 3, 1] {
            let centre = bs;
            for dy in -r..=r {
                for dx in -r..=r {
                    let c = (centre.0 + dx, centre.1 + dy);
                    if ok_src(c.0, c.1) {
                        let d = dist(c, bd);
                        if d < bd {
                            (bs, bd) = (c, d);
                        }
                    }
                }
            }
        }
        if bd == f32::MAX {
            bs = sources[rng.below(sources.len())];
        }
        // Copy the unknown part of the patch.
        let c_here = {
            let (mut c, mut n) = (0.0f32, 0.0f32);
            for dy in -R..=R {
                for dx in -R..=R {
                    if inside(tx + dx, ty + dy) {
                        c += conf[((ty + dy) * wi + tx + dx) as usize];
                        n += 1.0;
                    }
                }
            }
            c / n.max(1.0)
        };
        for dy in -R..=R {
            for dx in -R..=R {
                let (qx, qy) = (tx + dx, ty + dy);
                if !inside(qx, qy) {
                    continue;
                }
                let i = (qy * wi + qx) as usize;
                if unknown[i] {
                    px[i] = px[((bs.1 + dy) * wi + bs.0 + dx) as usize];
                    unknown[i] = false;
                    conf[i] = c_here;
                    from[i] = Some((bs.0 - tx, bs.1 - ty));
                    left -= 1;
                }
            }
        }
    }
    let mut out = img.clone();
    for y in 0..h {
        for x in 0..w {
            if hole[(y + y0) * img.w + x + x0] {
                out.px[(y + y0) * img.w + x + x0] = px[y * w + x];
            }
        }
    }
    out
}

/// The camera's motion from one frame to the next, as a perspective map of
/// pixel positions, worked out from up to 300 details outside the masked
/// area (`hole_a`, true = left out) that are followed into frame `b` and
/// fitted robustly (RANSAC). `None` if too few details could be followed.
pub fn motion(a: &[f32], b: &[f32], w: usize, h: usize, hole_a: &[bool]) -> Option<Homography> {
    let levels = 4;
    let pa = Pyramid::new(a.to_vec(), w, h, levels);
    let pb = Pyramid::new(b.to_vec(), w, h, levels);
    let region = [[0.0, 0.0], [w as f32, 0.0], [w as f32, h as f32], [0.0, h as f32]];
    let near = dilate(hole_a, w, h, 12);
    let la = Luma { data: a, width: w, height: h };
    let feats: Vec<[f32; 2]> = good_features(&la, &region, 600, (w.max(h) as f32 / 60.0).max(6.0), 10.0, &[]).into_iter().filter(|p| !near[(p[1] as usize).min(h - 1) * w + (p[0] as usize).min(w - 1)]).take(300).collect();
    let search = (w.max(h) as f32 / 12.0).max(24.0);
    let mut from = Vec::new();
    let mut to = Vec::new();
    for f in feats {
        let mut t = FeatureTracker::new(&pa, f, 11.0, search);
        if t.is_flat() {
            continue;
        }
        let (p, score) = t.step(&pb);
        if score >= MIN_SCORE {
            from.push(f);
            to.push(p);
        }
    }
    ransac(&from, &to, 1.5 + w.max(h) as f32 / 800.0)
}

/// The perspective map that most of the pairs agree with (within `tol` px), refined on those.
pub fn ransac(from: &[[f32; 2]], to: &[[f32; 2]], tol: f32) -> Option<Homography> {
    let n = from.len();
    if n < 6 {
        return None;
    }
    let mut rng = Rng(n as u64 * 7919 + 17);
    let mut best: Vec<usize> = Vec::new();
    for _ in 0..300 {
        let mut pick = [0usize; 4];
        for k in 0..4 {
            pick[k] = rng.below(n);
        }
        if (0..4).any(|i| (i + 1..4).any(|j| pick[i] == pick[j])) {
            continue;
        }
        let f: [[f32; 2]; 4] = pick.map(|i| from[i]);
        let t: [[f32; 2]; 4] = pick.map(|i| to[i]);
        let Some(hm) = Homography::from_quads(&f, &t) else { continue };
        let inl: Vec<usize> = (0..n)
            .filter(|&i| {
                let q = hm.apply(from[i]);
                (q[0] - to[i][0]).powi(2) + (q[1] - to[i][1]).powi(2) < tol * tol
            })
            .collect();
        if inl.len() > best.len() {
            best = inl;
        }
    }
    if best.len() < 6.max(n / 4) {
        return None;
    }
    let f: Vec<[f32; 2]> = best.iter().map(|&i| from[i]).collect();
    let t: Vec<[f32; 2]> = best.iter().map(|&i| to[i]).collect();
    fit_homography(&f, &t)
}

/// How each frame is filled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    /// From the other frames where they show what's behind, the rest made up
    /// and carried on (moving objects, moving cameras).
    Object,
    /// Made up once in the first frame and carried through the shot with
    /// the camera's motion (a wall, a floor, the sky).
    Surface,
    /// Smoothly from the edges (small or plain areas).
    EdgeBlend,
}

/// One frame given to the [`Filler`].
pub struct FrameIn {
    pub img: Image,
    /// True where it's to be filled.
    pub hole: Vec<bool>,
    /// The motion from the previous frame to this one, if known.
    pub from_prev: Option<Homography>,
}

/// Fills frame after frame, keeping the frames round the current one.
pub struct Filler {
    pub method: Method,
    /// How many frames each way are looked at.
    pub reach: usize,
    pub seed: u64,
    frames: Vec<FrameIn>,
    /// The index (in the whole shot) of `frames[0]`.
    first: usize,
    /// The last frame filled (its index and picture).
    last: Option<(usize, Image)>,
}

impl Filler {
    pub fn new(method: Method, reach: usize, seed: u64) -> Self {
        Filler { method, reach, seed, frames: Vec::new(), first: 0, last: None }
    }

    /// Add the next frame of the shot.
    pub fn push(&mut self, f: FrameIn) {
        self.frames.push(f);
    }

    /// How many frames have been pushed.
    pub fn pushed(&self) -> usize {
        self.first + self.frames.len()
    }

    /// Forget frames too far back to be needed for frame `next`.
    fn trim(&mut self, next: usize) {
        while self.first + self.reach < next && !self.frames.is_empty() {
            self.frames.remove(0);
            self.first += 1;
        }
    }

    /// The map from frame `i` to frame `j` (both still kept), chaining the steps between.
    fn map(&self, i: usize, j: usize) -> Option<Homography> {
        let mut m = Homography::IDENTITY;
        if j > i {
            for k in i + 1..=j {
                m = self.frames[k - self.first].from_prev.as_ref()?.then_after(&m);
            }
        } else {
            for k in (j + 1..=i).rev() {
                m = self.frames[k - self.first].from_prev.as_ref()?.inverse()?.then_after(&m);
            }
        }
        Some(m)
    }

    /// Fill frame `t` (it, and `reach` frames after it, must have been
    /// pushed, or every frame of the shot). Returns the filled picture.
    pub fn fill(&mut self, t: usize) -> Image {
        self.trim(t);
        let cur = &self.frames[t - self.first];
        let (w, h) = (cur.img.w, cur.img.h);
        let mut img = cur.img.clone();
        let mut todo = cur.hole.clone();
        if self.method == Method::EdgeBlend {
            let out = smooth_fill(&img, &todo);
            self.last = Some((t, out.clone()));
            return out;
        }
        let ring = {
            let outer = dilate(&todo, w, h, 10);
            let inner = dilate(&todo, w, h, 2);
            outer.iter().zip(&inner).map(|(o, i)| *o && !*i).collect::<Vec<bool>>()
        };
        let mut sources: Vec<(usize, bool)> = Vec::new();
        if self.method == Method::Object {
            // The nearest frames first, then further out.
            for d in 1..=self.reach {
                for s in [t.checked_sub(d), Some(t + d)].into_iter().flatten() {
                    if s >= self.first && s < self.first + self.frames.len() {
                        sources.push((s, false));
                    }
                }
            }
        }
        // Last of all, the frame filled before this one.
        if let Some((s, _)) = &self.last {
            if *s + 1 == t || (*s >= self.first && *s < t) {
                sources.push((*s, true));
            }
        }
        for (s, filled) in sources {
            if !todo.iter().any(|&u| u) {
                break;
            }
            let Some(to_s) = self.map(t, s) else { continue };
            let src_img = match (filled, &self.last) {
                (true, Some(last)) => &last.1,
                (true, None) => continue,
                (false, _) => &self.frames[s - self.first].img,
            };
            let src_hole = if filled { None } else { Some(&self.frames[s - self.first].hole) };
            let at = |p: [f32; 2]| -> Option<[f32; 3]> {
                let q = to_s.apply(p);
                if let Some(hs) = src_hole {
                    let (x, y) = (q[0].floor(), q[1].floor());
                    if x < 0.0 || y < 0.0 || x >= w as f32 || y >= h as f32 || hs[y as usize * w + x as usize] {
                        return None;
                    }
                }
                src_img.sample(q)
            };
            // Match the light: the ring round the hole here against the same place there.
            let mut sum = [[0.0f64; 2]; 3];
            for (i, _) in ring.iter().enumerate().filter(|(_, r)| **r) {
                let p = [(i % w) as f32 + 0.5, (i / w) as f32 + 0.5];
                if let Some(c) = at(p) {
                    for k in 0..3 {
                        sum[k][0] += img.px[i][k] as f64;
                        sum[k][1] += c[k] as f64;
                    }
                }
            }
            let gain: [f32; 3] = std::array::from_fn(|k| if sum[k][1] > 1.0 { ((sum[k][0] / sum[k][1]) as f32).clamp(0.8, 1.25) } else { 1.0 });
            for i in 0..w * h {
                if !todo[i] {
                    continue;
                }
                let p = [(i % w) as f32 + 0.5, (i / w) as f32 + 0.5];
                if let Some(c) = at(p) {
                    img.px[i] = [c[0] * gain[0], c[1] * gain[1], c[2] * gain[2]];
                    todo[i] = false;
                }
            }
        }
        if todo.iter().any(|&u| u) {
            img = synthesize(&img, &todo, self.seed ^ t as u64);
        }
        self.last = Some((t, img.clone()));
        img
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A textured picture: stripes and a smooth gradient, moved by `dx`.
    fn scene(w: usize, h: usize, dx: f32) -> Image {
        let px = (0..h)
            .flat_map(|y| {
                (0..w).map(move |x| {
                    let u = x as f32 + dx;
                    let s = if ((u / 6.0).floor() as i32 + (y as i32 / 9)) % 2 == 0 { 60.0 } else { 0.0 };
                    let n = ((u * 0.37).sin() * 20.0 + (y as f32 * 0.21).cos() * 20.0).abs();
                    [80.0 + s + n, 90.0 + u * 0.3, 120.0 - y as f32 * 0.2 + s * 0.5]
                })
            })
            .collect();
        Image { w, h, px }
    }

    /// Blotchy random detail (nothing repeats), moved by `dx`.
    fn blotches(w: usize, h: usize, dx: f32) -> Image {
        let hash = |x: i32, y: i32| -> f32 {
            let mut z = (x as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (y as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
            z = (z ^ (z >> 31)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            (z >> 40) as f32 / (1u64 << 24) as f32
        };
        let noise = |u: f32, v: f32| {
            let (i, j) = (u.floor(), v.floor());
            let (fx, fy) = (u - i, v - j);
            let (i, j) = (i as i32, j as i32);
            let a = hash(i, j) * (1.0 - fx) + hash(i + 1, j) * fx;
            let b = hash(i, j + 1) * (1.0 - fx) + hash(i + 1, j + 1) * fx;
            a * (1.0 - fy) + b * fy
        };
        let px = (0..h)
            .flat_map(|y| {
                (0..w).map(move |x| {
                    let u = x as f32 + dx;
                    let v = noise(u / 7.0, y as f32 / 7.0) * 160.0 + noise(u / 3.0 + 50.0, y as f32 / 3.0) * 60.0;
                    [v + 20.0, v * 0.8 + 30.0, 200.0 - v * 0.5]
                })
            })
            .collect();
        Image { w, h, px }
    }

    fn disc(w: usize, h: usize, c: [f32; 2], r: f32) -> Vec<bool> {
        (0..h).flat_map(|y| (0..w).map(move |x| (x as f32 + 0.5 - c[0]).powi(2) + (y as f32 + 0.5 - c[1]).powi(2) < r * r)).collect()
    }

    fn err(a: &Image, b: &Image, hole: &[bool]) -> f32 {
        let (mut s, mut n) = (0.0, 0.0);
        for i in 0..hole.len() {
            if hole[i] {
                s += (0..3).map(|k| (a.px[i][k] - b.px[i][k]).abs()).sum::<f32>() / 3.0;
                n += 1.0;
            }
        }
        s / n
    }

    #[test]
    fn smooth_fill_blends_from_the_edges() {
        let (w, h) = (64, 48);
        let img = Image { w, h, px: (0..w * h).map(|i| [(i % w) as f32 * 4.0, 100.0, 50.0]).collect() };
        let hole = disc(w, h, [32.0, 24.0], 10.0);
        let mut dirty = img.clone();
        for (p, &u) in dirty.px.iter_mut().zip(&hole) {
            if u {
                *p = [255.0, 0.0, 255.0];
            }
        }
        let out = smooth_fill(&dirty, &hole);
        assert!(err(&out, &img, &hole) < 12.0, "{}", err(&out, &img, &hole));
        // Outside the hole nothing changes.
        assert!(out.px.iter().zip(&dirty.px).zip(&hole).all(|((a, b), &u)| u || a == b));
    }

    #[test]
    fn synthesis_continues_a_texture_better_than_blurring() {
        let (w, h) = (96, 72);
        let img = scene(w, h, 0.0);
        let hole = disc(w, h, [48.0, 36.0], 11.0);
        let mut dirty = img.clone();
        for (p, &u) in dirty.px.iter_mut().zip(&hole) {
            if u {
                *p = [0.0, 255.0, 0.0];
            }
        }
        let synth = synthesize(&dirty, &hole, 1);
        let smooth = smooth_fill(&dirty, &hole);
        let (es, eb) = (err(&synth, &img, &hole), err(&smooth, &img, &hole));
        assert!(es < 30.0, "synthesised: {es}, smooth: {eb}");
        // No trace of the green left.
        assert!(synth.px.iter().zip(&hole).all(|(p, &u)| !u || p[1] < 200.0));
    }

    #[test]
    fn motion_is_found_from_details_outside_the_hole() {
        let (w, h) = (160, 120);
        let a = blotches(w, h, 0.0);
        let b = blotches(w, h, 5.0); // the picture slides 5 px left
        let hole = disc(w, h, [80.0, 60.0], 15.0);
        let m = motion(&a.luma(), &b.luma(), w, h, &hole).expect("a motion");
        let q = m.apply([40.0, 40.0]);
        assert!((q[0] - 35.0).abs() < 0.6 && (q[1] - 40.0).abs() < 0.6, "{q:?}");
    }

    #[test]
    fn a_hidden_area_is_fetched_from_frames_where_it_shows() {
        // A panning shot with a blob stuck over the middle of the frame: the
        // background behind it shows in the frames before and after.
        let (w, h) = (128, 96);
        let hole = disc(w, h, [64.0, 48.0], 10.0);
        let mut f = Filler::new(Method::Object, 8, 3);
        let truth: Vec<Image> = (0..12).map(|k| scene(w, h, k as f32 * 3.0)).collect();
        for (k, t) in truth.iter().enumerate() {
            let mut img = t.clone();
            for (p, &u) in img.px.iter_mut().zip(&hole) {
                if u {
                    *p = [255.0, 0.0, 0.0];
                }
            }
            let from_prev = (k > 0).then(|| Homography([1.0, 0.0, -3.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]));
            f.push(FrameIn { img, hole: hole.clone(), from_prev });
        }
        for k in 0..12 {
            let out = f.fill(k);
            let e = err(&out, &truth[k], &hole);
            assert!(e < 6.0, "frame {k}: {e}");
        }
    }

    #[test]
    fn edge_blend_and_surface_fill_everything() {
        let (w, h) = (64, 48);
        let hole = disc(w, h, [32.0, 24.0], 8.0);
        for method in [Method::EdgeBlend, Method::Surface] {
            let mut f = Filler::new(method, 4, 1);
            for k in 0..3 {
                let mut img = scene(w, h, 0.0);
                for (p, &u) in img.px.iter_mut().zip(&hole) {
                    if u {
                        *p = [255.0, 0.0, 255.0];
                    }
                }
                f.push(FrameIn { img, hole: hole.clone(), from_prev: (k > 0).then_some(Homography::IDENTITY) });
            }
            let a = f.fill(0);
            let b = f.fill(1);
            assert!(a.px.iter().zip(&hole).all(|(p, &u)| !u || p[1] > 20.0), "{method:?}");
            if method == Method::Surface {
                // Carried on unchanged with a still camera.
                assert!(err(&a, &b, &hole) < 0.5);
            }
        }
    }
}
