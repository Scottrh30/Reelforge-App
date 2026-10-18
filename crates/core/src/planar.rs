//! Planar tracking: follow a whole flat surface (a screen, a sign, a wall, a
//! table top) rather than four separate details.
//!
//! The user draws the surface's outline as four corners. Inside it, many small
//! features are found (places with detail in two directions, by the smallest
//! eigenvalue of the local gradient structure) and each is followed with the
//! same patch matcher as the point tracker. Every frame, the perspective map
//! (homography) that best explains where they all went is fitted robustly:
//! random samples of four features propose a map, the one most features agree
//! with wins (RANSAC), and a least-squares fit over those agreeing refines it.
//! Features that disagree (a hand passing over, a reflection, something in
//! front) are put back where the surface says they should be, or dropped and
//! replaced by fresh ones. The four corners are moved by the map, so they can
//! sit on blank areas or even outside the picture.
//!
//! Drift is held down by comparing each feature, every frame, with how it
//! looked in the first frame, warped by the surface's motion since.

use crate::track::{local_linear, FeatureTracker, Luma, Pyramid, MIN_SCORE};
use crate::transform::Homography;

/// Why planar tracking stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanarError {
    /// Too little detail inside the shape to start.
    NoDetail,
    /// Too few features agree on where the surface went (covered, out of
    /// frame, too blurred).
    Lost,
}

/// One followed feature: its tracker and where it sits on the surface (in the
/// first frame's pixels).
struct Feature {
    t: FeatureTracker,
    on_plane: [f32; 2],
    /// Frames in a row it disagreed with the surface.
    misses: u32,
    /// Frames it has been followed for.
    age: u32,
}

/// Follows a flat surface from frame to frame.
pub struct PlanarTracker {
    features: Vec<Feature>,
    /// The surface's corners in the first frame.
    quad: [[f32; 2]; 4],
    /// First frame → current frame.
    h: Homography,
    /// …as it was one frame earlier (for predicting the next).
    prev: Homography,
    /// The first frame's luma, for re-checking features against their original look.
    first: Vec<f32>,
    width: usize,
    height: usize,
    size: f32,
    search: f32,
    /// How many features to keep.
    target: usize,
    /// Reprojection error (pixels) within which a feature agrees with the map.
    tol: f32,
    rng: u64,
    /// Where features that kept disagreeing were just dropped: new ones aren't
    /// put there (it's usually something moving across the surface).
    rejected: Vec<[f32; 2]>,
    /// Share of features that agreed in the last frame, 0–1.
    pub agreement: f32,
}

impl PlanarTracker {
    /// Start on `quad` (top-left, top-right, bottom-right, bottom-left; pixels
    /// of `pyr`'s full level). `search` is how far the surface may move
    /// between frames.
    pub fn new(pyr: &Pyramid, quad: [[f32; 2]; 4], search: f32) -> Result<Self, PlanarError> {
        let img = pyr.level(0);
        let (width, height) = (img.width, img.height);
        // Patches of about 1/90 of the frame width, odd, 15–31 px.
        let size = (((width as f32 / 90.0).round() as i32) | 1).clamp(15, 31) as f32;
        let mut p = PlanarTracker {
            features: Vec::new(),
            quad,
            h: Homography::IDENTITY,
            prev: Homography::IDENTITY,
            first: img.data.to_vec(),
            width,
            height,
            size,
            search: search.max(4.0),
            target: 120,
            tol: (width as f32 / 700.0).max(1.5),
            rng: 0x9e37_79b9_7f4a_7c15,
            rejected: Vec::new(),
            agreement: 1.0,
        };
        p.seed(pyr);
        if p.features.len() < 8 {
            return Err(PlanarError::NoDetail);
        }
        Ok(p)
    }

    /// The surface's corners now.
    pub fn corners(&self) -> [[f32; 2]; 4] {
        self.quad.map(|c| self.h.apply(c))
    }

    /// The map from the first frame to the current one.
    pub fn homography(&self) -> Homography {
        self.h
    }

    pub fn feature_count(&self) -> usize {
        self.features.len()
    }

    /// Add features inside the surface as it is now, away from existing ones.
    fn seed(&mut self, pyr: &Pyramid) {
        let img = pyr.level(0);
        let now = self.corners();
        let want = self.target.saturating_sub(self.features.len());
        if want == 0 {
            return;
        }
        let min_dist = self.size * 1.2;
        let mut taken: Vec<[f32; 2]> = self.features.iter().map(|f| f.t.pos).collect();
        taken.extend(&self.rejected);
        let found = good_features(&img, &now, want, min_dist, self.size * 0.5 + 2.0, &taken);
        let Some(back) = self.h.inverse() else { return };
        for pos in found {
            let t = FeatureTracker::new(pyr, pos, self.size, self.search);
            if t.is_flat() {
                continue;
            }
            self.features.push(Feature { t, on_plane: back.apply(pos), misses: 0, age: 0 });
        }
    }

    /// Follow the surface into the next frame. Returns its corners there.
    pub fn step(&mut self, pyr: &Pyramid) -> Result<[[f32; 2]; 4], PlanarError> {
        // Predict with the surface's last motion (constant velocity in the plane).
        let pred = match self.prev.inverse() {
            Some(pi) => self.h.then_after(&pi).then_after(&self.h),
            None => self.h,
        };
        for f in &mut self.features {
            f.t.aim(pred.apply(f.on_plane));
        }
        // Each feature searches on its own: spread them over the cores.
        let results: Vec<([f32; 2], f32)> = par_map(&mut self.features, |f| f.t.step(pyr));
        let found: Vec<usize> = (0..self.features.len()).filter(|&i| results[i].1 >= MIN_SCORE).collect();
        if found.len() < 6 {
            return Err(PlanarError::Lost);
        }
        let src: Vec<[f32; 2]> = found.iter().map(|&i| self.features[i].on_plane).collect();
        let dst: Vec<[f32; 2]> = found.iter().map(|&i| results[i].0).collect();
        let Some((h, inliers)) = ransac(&src, &dst, self.tol, &mut self.rng) else { return Err(PlanarError::Lost) };
        let agree = inliers.iter().filter(|&&b| b).count();
        // Judge by the features followed for a while: new ones may sit on
        // something that isn't the surface.
        let settled: Vec<bool> = found.iter().map(|&i| self.features[i].age >= 2).collect();
        let (n_settled, agree_settled) = (settled.iter().filter(|&&b| b).count(), settled.iter().zip(&inliers).filter(|(s, a)| **s && **a).count());
        let (pool, ok) = if n_settled >= 8 { (n_settled, agree_settled) } else { (found.len(), agree) };
        if agree < 6 || (ok as f32) < 0.3 * pool as f32 {
            return Err(PlanarError::Lost);
        }

        // Re-check features against their first-frame look, warped by the map;
        // put the ones that disagree back where the surface says they are.
        let first = Luma { data: &self.first, width: self.width, height: self.height };
        let mut agrees = vec![false; self.features.len()];
        for (k, &i) in found.iter().enumerate() {
            agrees[i] = inliers[k];
        }
        let h_now = h;
        let checked: Vec<bool> = par_map_enumerated(&mut self.features, |i, f| {
            let expected = h_now.apply(f.on_plane);
            f.age += 1;
            if !agrees[i] {
                f.misses += 1;
                f.t.place(expected);
            } else {
                f.misses = 0;
            }
            // Check against the first frame's look (features added later too: if their
            // spot was covered or off-picture then, the match fails and nothing changes).
            agrees[i] && f.t.correct(&first, f.on_plane, local_linear(&h_now, f.on_plane), pyr).is_some()
        });
        // Refit on the agreeing features at their checked positions: the ones that
        // also matched their first look if there are enough (a feature half under
        // something passing in front matches neither well), weighted so the
        // borderline ones count less.
        let n_checked = checked.iter().filter(|&&b| b).count();
        let use_it = |i: usize| if n_checked >= 12 { checked[i] } else { agrees[i] };
        let (s2, d2): (Vec<[f32; 2]>, Vec<[f32; 2]>) = self
            .features
            .iter()
            .enumerate()
            .filter(|(i, _)| use_it(*i))
            .map(|(_, f)| (f.on_plane, f.t.pos))
            .unzip();
        let h = robust_refit(h, &s2, &d2, self.tol * 2.0).filter(|r| reprojection_ok(r, &s2, &d2, self.tol * 2.0)).unwrap_or(h);
        if !plausible(&self.quad, &h) {
            return Err(PlanarError::Lost);
        }
        self.agreement = agree as f32 / self.features.len().max(1) as f32;
        self.prev = self.h;
        self.h = h;

        // Forget features that keep disagreeing (or wandered off the picture); top up.
        let (w, hh) = (self.width as f32, self.height as f32);
        let margin = self.size;
        self.rejected.clear();
        let rejected = &mut self.rejected;
        self.features.retain(|f| {
            let inside = f.t.pos[0] > margin && f.t.pos[1] > margin && f.t.pos[0] < w - margin && f.t.pos[1] < hh - margin;
            if f.misses > 2 {
                rejected.push(f.t.pos);
            }
            f.misses <= 2 && inside
        });
        if self.features.len() < self.target * 2 / 3 {
            self.seed(pyr);
        }
        Ok(self.corners())
    }
}

/// The map most pairs agree with (within `tol` pixels): (map, which ones agree).
pub(crate) fn ransac(src: &[[f32; 2]], dst: &[[f32; 2]], tol: f32, rng: &mut u64) -> Option<(Homography, Vec<bool>)> {
    let n = src.len();
    let tol2 = tol * tol;
    let count = |h: &Homography| -> (usize, Vec<bool>) {
        let v: Vec<bool> = src
            .iter()
            .zip(dst)
            .map(|(s, d)| {
                let p = h.apply(*s);
                let (dx, dy) = (p[0] - d[0], p[1] - d[1]);
                dx * dx + dy * dy <= tol2
            })
            .collect();
        (v.iter().filter(|&&b| b).count(), v)
    };
    // A fit to all of them is the first guess (right when nothing is in the way).
    let mut best = fit_homography(src, dst).map(|h| count(&h)).map(|(c, v)| (c, v, 0)).unwrap_or((0, vec![false; n], 0));
    for _ in 0..250 {
        let mut idx = [0usize; 4];
        let mut k = 0;
        let mut guard = 0;
        while k < 4 && guard < 50 {
            guard += 1;
            let r = (random(rng) % n as u64) as usize;
            if !idx[..k].contains(&r) {
                idx[k] = r;
                k += 1;
            }
        }
        if k < 4 {
            continue;
        }
        let f = idx.map(|i| src[i]);
        let t = idx.map(|i| dst[i]);
        let Some(h) = Homography::from_quads(&f, &t) else { continue };
        let (c, v) = count(&h);
        if c > best.0 {
            best = (c, v, 1);
            if c == n {
                break;
            }
        }
    }
    if best.0 < 4 {
        return None;
    }
    // Least squares over the agreeing ones, then once more with the new set.
    let pick = |v: &[bool]| -> (Vec<[f32; 2]>, Vec<[f32; 2]>) { src.iter().zip(dst).zip(v).filter(|(_, &b)| b).map(|((s, d), _)| (*s, *d)).unzip() };
    let (a, b) = pick(&best.1);
    let h = fit_homography(&a, &b)?;
    let (_, v) = count(&h);
    let (a, b) = pick(&v);
    let h2 = fit_homography(&a, &b).unwrap_or(h);
    let (_, v2) = count(&h2);
    Some((h2, v2))
}

/// xorshift64*: deterministic, so a track comes out the same every time.
pub(crate) fn random(state: &mut u64) -> u64 {
    let mut x = *state;
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    *state = x;
    x.wrapping_mul(0x2545_f491_4f6c_dd1d)
}


/// Is the map sane for this surface: corners still a convex four-sided shape
/// of reasonable size (not flipped, folded or collapsed)?
fn plausible(quad: &[[f32; 2]; 4], h: &Homography) -> bool {
    let q = quad.map(|c| h.apply(c));
    if q.iter().any(|c| !c[0].is_finite() || !c[1].is_finite()) {
        return false;
    }
    let area = |q: &[[f32; 2]; 4]| -> f32 { 0.5 * (0..4).map(|i| q[i][0] * q[(i + 1) % 4][1] - q[(i + 1) % 4][0] * q[i][1]).sum::<f32>() };
    let (a0, a1) = (area(quad), area(&q));
    if a0.abs() < 1.0 || a0.signum() != a1.signum() || (a1 / a0).abs() < 0.02 || (a1 / a0).abs() > 50.0 {
        return false;
    }
    // Convex: every turn the same way.
    let turn = |a: [f32; 2], b: [f32; 2], c: [f32; 2]| (b[0] - a[0]) * (c[1] - b[1]) - (b[1] - a[1]) * (c[0] - b[0]);
    let t: Vec<f32> = (0..4).map(|i| turn(q[i], q[(i + 1) % 4], q[(i + 2) % 4])).collect();
    t.iter().all(|&v| v > 0.0) || t.iter().all(|&v| v < 0.0)
}

fn reprojection_ok(h: &Homography, src: &[[f32; 2]], dst: &[[f32; 2]], tol: f32) -> bool {
    if src.is_empty() {
        return false;
    }
    let mean = src
        .iter()
        .zip(dst)
        .map(|(s, d)| {
            let p = h.apply(*s);
            ((p[0] - d[0]).powi(2) + (p[1] - d[1]).powi(2)).sqrt()
        })
        .sum::<f32>()
        / src.len() as f32;
    mean <= tol
}

/// Run `f` on every item, spread over the cores.
pub(crate) fn par_map<T: Send, R: Send + Default + Clone>(items: &mut [T], f: impl Fn(&mut T) -> R + Sync) -> Vec<R> {
    let mut out = vec![R::default(); items.len()];
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(1, 16);
    if threads == 1 || items.len() < 8 {
        for (o, it) in out.iter_mut().zip(items.iter_mut()) {
            *o = f(it);
        }
        return out;
    }
    let per = items.len().div_ceil(threads);
    std::thread::scope(|s| {
        for (chunk, outs) in items.chunks_mut(per).zip(out.chunks_mut(per)) {
            let f = &f;
            s.spawn(move || {
                for (o, it) in outs.iter_mut().zip(chunk.iter_mut()) {
                    *o = f(it);
                }
            });
        }
    });
    out
}

/// Run `f(index, item)` on every item, spread over the cores.
pub(crate) fn par_map_enumerated<T: Send, R: Send + Default + Clone>(items: &mut [T], f: impl Fn(usize, &mut T) -> R + Sync) -> Vec<R> {
    let mut out = vec![R::default(); items.len()];
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(1, 16);
    let per = items.len().div_ceil(threads).max(1);
    std::thread::scope(|s| {
        for (c, (chunk, outs)) in items.chunks_mut(per).zip(out.chunks_mut(per)).enumerate() {
            let f = &f;
            s.spawn(move || {
                for (k, (it, o)) in chunk.iter_mut().zip(outs.iter_mut()).enumerate() {
                    *o = f(c * per + k, it);
                }
            });
        }
    });
    out
}

/// Refine `h` by iteratively re-weighted least squares (Tukey's biweight with
/// cut-off `c` pixels): pairs that fit badly count less, and past `c` not at all.
pub(crate) fn robust_refit(mut h: Homography, src: &[[f32; 2]], dst: &[[f32; 2]], c: f32) -> Option<Homography> {
    for _ in 0..4 {
        let w: Vec<f64> = src
            .iter()
            .zip(dst)
            .map(|(s, d)| {
                let p = h.apply(*s);
                let r = (((p[0] - d[0]).powi(2) + (p[1] - d[1]).powi(2)).sqrt() / c) as f64;
                if r >= 1.0 { 0.0 } else { (1.0 - r * r).powi(2) }
            })
            .collect();
        if w.iter().filter(|&&x| x > 0.0).count() < 4 {
            return None;
        }
        h = fit_homography_weighted(src, dst, Some(&w))?;
    }
    Some(h)
}

/// Is `p` inside the convex four-sided `q` (either winding)?
fn inside(q: &[[f32; 2]; 4], p: [f32; 2]) -> bool {
    let side = |a: [f32; 2], b: [f32; 2]| (b[0] - a[0]) * (p[1] - a[1]) - (b[1] - a[1]) * (p[0] - a[0]);
    let s: Vec<f32> = (0..4).map(|i| side(q[i], q[(i + 1) % 4])).collect();
    s.iter().all(|&v| v >= 0.0) || s.iter().all(|&v| v <= 0.0)
}

/// Up to `max` well-spread features inside `region` of `img`, best first: the
/// smallest eigenvalue of the gradient structure tensor over a 5×5 window
/// (Shi–Tomasi), at least `min_dist` apart and from `taken`, and `margin`
/// pixels inside the picture.
pub fn good_features(img: &Luma, region: &[[f32; 2]; 4], max: usize, min_dist: f32, margin: f32, taken: &[[f32; 2]]) -> Vec<[f32; 2]> {
    let (w, h) = (img.width as isize, img.height as isize);
    let m = margin.ceil() as isize + 3;
    let xs = region.iter().map(|p| p[0]);
    let ys = region.iter().map(|p| p[1]);
    let x0 = (xs.clone().fold(f32::MAX, f32::min).floor() as isize).max(m);
    let x1 = (xs.fold(f32::MIN, f32::max).ceil() as isize).min(w - 1 - m);
    let y0 = (ys.clone().fold(f32::MAX, f32::min).floor() as isize).max(m);
    let y1 = (ys.fold(f32::MIN, f32::max).ceil() as isize).min(h - 1 - m);
    if x1 <= x0 || y1 <= y0 {
        return Vec::new();
    }
    let (bw, bh) = ((x1 - x0 + 1) as usize, (y1 - y0 + 1) as usize);
    let px = |x: isize, y: isize| img.data[(y * w + x) as usize];
    // Gradient products over the box (plus a 2-pixel border for the window).
    let (gw, gh) = (bw + 4, bh + 4);
    let mut gxx = vec![0f32; gw * gh];
    let mut gyy = vec![0f32; gw * gh];
    let mut gxy = vec![0f32; gw * gh];
    for j in 0..gh {
        let y = y0 - 2 + j as isize;
        for i in 0..gw {
            let x = x0 - 2 + i as isize;
            let dx = 0.5 * (px(x + 1, y) - px(x - 1, y));
            let dy = 0.5 * (px(x, y + 1) - px(x, y - 1));
            let k = j * gw + i;
            gxx[k] = dx * dx;
            gyy[k] = dy * dy;
            gxy[k] = dx * dy;
        }
    }
    // Box-sum each over 5×5 (separably), then the smaller eigenvalue.
    let boxsum = |g: &[f32]| -> Vec<f32> {
        let mut rows = vec![0f32; bw * gh];
        for j in 0..gh {
            for i in 0..bw {
                rows[j * bw + i] = (0..5).map(|d| g[j * gw + i + d]).sum();
            }
        }
        let mut out = vec![0f32; bw * bh];
        for j in 0..bh {
            for i in 0..bw {
                out[j * bw + i] = (0..5).map(|d| rows[(j + d) * bw + i]).sum();
            }
        }
        out
    };
    let (a, b, c) = (boxsum(&gxx), boxsum(&gyy), boxsum(&gxy));
    let mut resp: Vec<(f32, usize)> = Vec::new();
    let mut top = 0f32;
    for j in 0..bh {
        for i in 0..bw {
            let k = j * bw + i;
            let (sa, sb, sc) = (a[k], b[k], c[k]);
            let lmin = 0.5 * (sa + sb - ((sa - sb) * (sa - sb) + 4.0 * sc * sc).sqrt());
            let p = [(x0 + i as isize) as f32, (y0 + j as isize) as f32];
            if lmin > 0.0 && inside(region, p) {
                top = top.max(lmin);
                resp.push((lmin, k));
            }
        }
    }
    // Weak responses are noise; flat regions give none at all.
    // (λ is summed over the 25 pixels of the window: 100 ≈ gradients of 2
    // levels per pixel in both directions, above camera noise.)
    let floor = (top * 0.01).max(100.0);
    resp.retain(|r| r.0 >= floor);
    resp.sort_by(|p, q| q.0.total_cmp(&p.0));
    // Greedy pick, spread out (a coarse grid makes the distance checks cheap).
    let cell = min_dist.max(1.0);
    let gx = (bw as f32 / cell).ceil() as usize + 1;
    let gy = (bh as f32 / cell).ceil() as usize + 1;
    let mut grid: Vec<Vec<[f32; 2]>> = vec![Vec::new(); gx * gy];
    let cell_of = |p: [f32; 2]| -> Option<(usize, usize)> {
        let (cx, cy) = (((p[0] - x0 as f32) / cell).floor(), ((p[1] - y0 as f32) / cell).floor());
        (cx >= 0.0 && cy >= 0.0 && (cx as usize) < gx && (cy as usize) < gy).then_some((cx as usize, cy as usize))
    };
    for t in taken {
        if let Some((cx, cy)) = cell_of(*t) {
            grid[cy * gx + cx].push(*t);
        }
    }
    let d2 = min_dist * min_dist;
    let mut out = Vec::new();
    for (_, k) in resp {
        let p = [(x0 + (k % bw) as isize) as f32, (y0 + (k / bw) as isize) as f32];
        let (cx, cy) = cell_of(p).unwrap_or((0, 0));
        let mut ok = true;
        'n: for yy in cy.saturating_sub(1)..=(cy + 1).min(gy - 1) {
            for xx in cx.saturating_sub(1)..=(cx + 1).min(gx - 1) {
                for q in &grid[yy * gx + xx] {
                    if (q[0] - p[0]).powi(2) + (q[1] - p[1]).powi(2) < d2 {
                        ok = false;
                        break 'n;
                    }
                }
            }
        }
        if ok {
            grid[cy * gx + cx].push(p);
            out.push(p);
            if out.len() >= max {
                break;
            }
        }
    }
    out
}

/// The perspective map that best takes `src` to `dst` (least squares, at
/// least four pairs), with the points normalised for numerical stability.
pub fn fit_homography(src: &[[f32; 2]], dst: &[[f32; 2]]) -> Option<Homography> {
    fit_homography_weighted(src, dst, None)
}

/// [`fit_homography`] with a weight per pair.
pub(crate) fn fit_homography_weighted(src: &[[f32; 2]], dst: &[[f32; 2]], weights: Option<&[f64]>) -> Option<Homography> {
    let n = src.len().min(dst.len());
    if n < 4 {
        return None;
    }
    // Move each set to its centroid and scale to mean distance √2.
    let norm = |pts: &[[f32; 2]]| -> [f64; 3] {
        let (mut cx, mut cy) = (0.0, 0.0);
        for p in pts {
            cx += p[0] as f64;
            cy += p[1] as f64;
        }
        cx /= pts.len() as f64;
        cy /= pts.len() as f64;
        let d = pts.iter().map(|p| ((p[0] as f64 - cx).powi(2) + (p[1] as f64 - cy).powi(2)).sqrt()).sum::<f64>() / pts.len() as f64;
        let s = if d > 1e-9 { std::f64::consts::SQRT_2 / d } else { 1.0 };
        [s, cx, cy]
    };
    let ([ss, sx, sy], [ds, dx, dy]) = (norm(&src[..n]), norm(&dst[..n]));
    // Normal equations of the 8-unknown linear system (h8 = 1).
    let mut ata = [[0.0f64; 8]; 8];
    let mut atb = [0.0f64; 8];
    for i in 0..n {
        let (x, y) = ((src[i][0] as f64 - sx) * ss, (src[i][1] as f64 - sy) * ss);
        let (u, v) = ((dst[i][0] as f64 - dx) * ds, (dst[i][1] as f64 - dy) * ds);
        let wt = weights.map_or(1.0, |w| w[i]);
        if wt <= 0.0 {
            continue;
        }
        let rows = [([x, y, 1.0, 0.0, 0.0, 0.0, -u * x, -u * y], u), ([0.0, 0.0, 0.0, x, y, 1.0, -v * x, -v * y], v)];
        for (r, b) in rows {
            for a in 0..8 {
                atb[a] += wt * r[a] * b;
                for c in 0..8 {
                    ata[a][c] += wt * r[a] * r[c];
                }
            }
        }
    }
    let h = solve(ata, atb)?;
    let hn = Homography([h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7], 1.0]);
    // Undo the normalisations: H = T_dst⁻¹ · Hn · T_src.
    let t_src = Homography([ss, 0.0, -ss * sx, 0.0, ss, -ss * sy, 0.0, 0.0, 1.0]);
    let t_dst_inv = Homography([1.0 / ds, 0.0, dx, 0.0, 1.0 / ds, dy, 0.0, 0.0, 1.0]);
    let r = t_dst_inv.then_after(&hn.then_after(&t_src));
    let k = r.0[8];
    if k.abs() < 1e-12 {
        return None;
    }
    Some(Homography(r.0.map(|v| v / k)))
}

/// Gaussian elimination with partial pivoting.
pub(crate) fn solve(mut a: [[f64; 8]; 8], mut b: [f64; 8]) -> Option<[f64; 8]> {
    for col in 0..8 {
        let piv = (col..8).max_by(|&p, &q| a[p][col].abs().total_cmp(&a[q][col].abs()))?;
        if a[piv][col].abs() < 1e-12 {
            return None;
        }
        a.swap(col, piv);
        b.swap(col, piv);
        for r in col + 1..8 {
            let f = a[r][col] / a[col][col];
            for c in col..8 {
                a[r][c] -= f * a[col][c];
            }
            b[r] -= f * b[col];
        }
    }
    let mut x = [0.0; 8];
    for r in (0..8).rev() {
        let s: f64 = (r + 1..8).map(|c| a[r][c] * x[c]).sum();
        x[r] = (b[r] - s) / a[r][r];
    }
    Some(x)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fits_a_known_perspective_map() {
        let h = Homography([1.1, 0.05, 12.0, -0.03, 0.95, -7.0, 0.0004, -0.0002, 1.0]);
        let src: Vec<[f32; 2]> = (0..30).map(|i| [(i * 37 % 400) as f32, (i * 53 % 300) as f32]).collect();
        let dst: Vec<[f32; 2]> = src.iter().map(|p| h.apply(*p)).collect();
        let g = fit_homography(&src, &dst).unwrap();
        for p in &src {
            let (a, b) = (h.apply(*p), g.apply(*p));
            assert!((a[0] - b[0]).abs() < 0.01 && (a[1] - b[1]).abs() < 0.01, "{a:?} vs {b:?}");
        }
    }

    /// A textured plane under a perspective that changes every frame, with a
    /// bright square sweeping across part of it (an occluder): the corners
    /// must follow within a pixel or so.
    #[test]
    fn follows_a_textured_plane_through_perspective_and_occlusion() {
        let (w, h) = (320usize, 240usize);
        // Texture: smooth random blobs (value noise), plus some lines.
        let tex = |u: f32, v: f32| -> f32 {
            let n = |x: f32, y: f32| -> f32 {
                let (xi, yi) = (x.floor() as i64, y.floor() as i64);
                let (fx, fy) = (x - x.floor(), y - y.floor());
                let r = |a: i64, b: i64| -> f32 {
                    let mut s = (a.wrapping_mul(374_761_393) ^ b.wrapping_mul(668_265_263)) as u64;
                    s = (s ^ (s >> 13)).wrapping_mul(1_274_126_177);
                    (s >> 40) as f32 / (1u64 << 24) as f32
                };
                let (sx, sy) = (fx * fx * (3.0 - 2.0 * fx), fy * fy * (3.0 - 2.0 * fy));
                let top = r(xi, yi) * (1.0 - sx) + r(xi + 1, yi) * sx;
                let bot = r(xi, yi + 1) * (1.0 - sx) + r(xi + 1, yi + 1) * sx;
                top * (1.0 - sy) + bot * sy
            };
            40.0 + 120.0 * n(u / 9.0, v / 9.0) + 60.0 * n(u / 3.5 + 50.0, v / 3.5)
        };
        let quad0 = [[90.0, 70.0], [230.0, 70.0], [230.0, 170.0], [90.0, 170.0]];
        // The plane drifts, turns a little and tilts over 30 frames.
        let motion = |k: usize| -> Homography {
            let t = k as f64 / 29.0;
            let a = 0.15 * t;
            let (s, c) = a.sin_cos();
            Homography([c * (1.0 + 0.1 * t), -s, 18.0 * t + 8.0 * (3.0 * t).sin(), s, c, -10.0 * t, 0.0006 * t, 0.0002 * t, 1.0])
        };
        let render = |k: usize| -> Vec<f32> {
            let m = motion(k);
            // Turn about the frame centre: m is applied to centred coordinates.
            let c = [w as f32 / 2.0, h as f32 / 2.0];
            let inv = m.inverse().unwrap();
            let mut img = vec![0f32; w * h];
            for y in 0..h {
                for x in 0..w {
                    let q = inv.apply([x as f32 - c[0], y as f32 - c[1]]);
                    let mut v = tex(q[0] + c[0], q[1] + c[1]);
                    // The occluder: a flat square crossing the right part of the plane.
                    let ox = 150.0 + 3.0 * k as f32;
                    if (x as f32) > ox && (x as f32) < ox + 30.0 && (y as f32) > 90.0 && (y as f32) < 130.0 {
                        v = 250.0;
                    }
                    img[y * w + x] = v;
                }
            }
            img
        };
        let truth = |k: usize, p: [f32; 2]| -> [f32; 2] {
            let c = [w as f32 / 2.0, h as f32 / 2.0];
            let q = motion(k).apply([p[0] - c[0], p[1] - c[1]]);
            [q[0] + c[0], q[1] + c[1]]
        };
        let start = quad0.map(|p| truth(0, p));
        let pyr0 = Pyramid::new(render(0), w, h, 1);
        let mut pt = PlanarTracker::new(&pyr0, start, 12.0).expect("detail to track");
        assert!(pt.feature_count() >= 15, "{} features", pt.feature_count());
        let mut worst = 0f32;
        for k in 1..30 {
            let pyr = Pyramid::new(render(k), w, h, 1);
            let corners = pt.step(&pyr).unwrap_or_else(|e| panic!("lost at frame {k}: {e:?}"));
            for (c, q) in corners.iter().zip(quad0) {
                let t = truth(k, q);
                worst = worst.max(((c[0] - t[0]).powi(2) + (c[1] - t[1]).powi(2)).sqrt());
            }
        }
        eprintln!("planar: worst corner error {worst:.3} px, {} features at the end", pt.feature_count());
        assert!(worst < 1.5, "corners off by up to {worst:.2} px");
    }

    #[test]
    fn a_blank_shape_is_refused() {
        let (w, h) = (200usize, 150usize);
        let pyr = Pyramid::new(vec![100.0; w * h], w, h, 1);
        let r = PlanarTracker::new(&pyr, [[50.0, 40.0], [150.0, 40.0], [150.0, 110.0], [50.0, 110.0]], 10.0);
        assert!(matches!(r, Err(PlanarError::NoDetail)));
    }
}

#[cfg(test)]
mod bench {
    use super::*;
    /// cargo test --release -p reelforge-core planar_speed -- --ignored --nocapture
    #[test]
    #[ignore]
    fn planar_speed() {
        let (w, h) = (1920usize, 1080usize);
        let frame = |dx: f32| -> Vec<f32> {
            (0..w * h)
                .map(|i| {
                    let (x, y) = ((i % w) as f32 - dx, (i / w) as f32);
                    let v = ((x * 0.37).sin() * (y * 0.23).cos() + (x * 0.051 + y * 0.077).sin() * 2.0 + ((x * 0.013) * (y * 0.017)).sin()) * 40.0;
                    128.0 + v
                })
                .collect()
        };
        let quad = [[600.0, 300.0], [1300.0, 300.0], [1300.0, 800.0], [600.0, 800.0]];
        let p0 = Pyramid::new(frame(0.0), w, h, Pyramid::levels_for(24.0));
        let mut pt = PlanarTracker::new(&p0, quad, 24.0).unwrap();
        let frames: Vec<Pyramid> = (1..11).map(|k| Pyramid::new(frame(3.0 * k as f32), w, h, Pyramid::levels_for(24.0))).collect();
        let t = std::time::Instant::now();
        for p in &frames {
            pt.step(p).unwrap();
        }
        let ms = t.elapsed().as_secs_f64() * 100.0;
        eprintln!("planar 1080p: {ms:.1} ms per frame with {} features, corner 0 at {:?}", pt.feature_count(), pt.corners()[0]);
    }
}
