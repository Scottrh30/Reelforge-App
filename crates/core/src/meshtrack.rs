//! Mesh tracking: follow a surface that bends (cloth, skin, a flag, a page),
//! as a grid of points that each move with what's under them.
//!
//! As in planar tracking ([`crate::planar`]), many small features inside the
//! surface are followed with the patch matcher. Each feature is tied to a spot
//! on the grid: a cell, and where in that cell it sits. Every frame the grid's
//! points are solved for together, by least squares: each feature pulls the
//! four corners of its cell so that its spot lands where the feature was
//! found, while neighbouring points are held to keep their shape relative to
//! each other (so the grid stays smooth, and cells with no detail of their own
//! move with their neighbours). Features that disagree with the rest (something
//! passing in front, a reflection) are weighted down to nothing (Tukey's
//! biweight), and a first robust fit of the overall motion catches sudden
//! moves. As in planar tracking, every feature is re-checked against how it
//! looked in the first frame, warped the way the grid has bent since, so small
//! errors don't pile up.

use crate::meshwarp::{MeshCoord, MeshShape};
use crate::planar::{good_features, par_map, par_map_enumerated, ransac, PlanarError};
use crate::track::{FeatureTracker, Luma, Pyramid, MIN_SCORE};

/// A feature outside the followed region starts each frame counting for
/// nothing; it counts fully once it's seen to move with what's inside (see
/// [`MeshTracker::step`]).
const OUTSIDE_WEIGHT: f64 = 0.0;
/// Frames in a row a feature outside the region must move with it to count.
const TRUST_FRAMES: u32 = 6;

struct Feature {
    t: FeatureTracker,
    /// Where on the grid it is.
    at: MeshCoord,
    /// Where it was in the first frame.
    on_ref: [f32; 2],
    misses: u32,
    age: u32,
    /// How much it counts in the solve (before the robust re-weighting).
    weight: f64,
    /// Outside a followed region: frames in a row it has moved with what's inside.
    with_region: u32,
}

/// Follows a bending surface from frame to frame.
pub struct MeshTracker {
    features: Vec<Feature>,
    /// The grid in the first frame.
    reference: MeshShape,
    /// …now, and one frame earlier.
    mesh: MeshShape,
    prev: MeshShape,
    first: Vec<f32>,
    width: usize,
    height: usize,
    size: f32,
    search: f32,
    target: usize,
    tol: f32,
    rng: u64,
    rejected: Vec<[f32; 2]>,
    /// What's being followed (an outline in the first frame's pixels, such as
    /// a mask's); features outside it count only once they've kept moving
    /// with it (see [`TRUST_FRAMES`]). The whole grid when `None`.
    region: Option<Vec<[f32; 2]>>,
    /// Share of features that agreed in the last frame, 0–1.
    pub agreement: f32,
}

impl MeshTracker {
    /// Start on `mesh` (pixels of `pyr`'s full level); `search` is how far the
    /// surface may move between frames.
    pub fn new(pyr: &Pyramid, mesh: MeshShape, search: f32) -> Result<Self, PlanarError> {
        Self::in_region(pyr, mesh, None, search)
    }

    /// Start on `mesh`, following what's inside `region` (an outline in the
    /// first frame, such as a mask's): what's outside it counts only where it
    /// keeps moving with what's inside (more of the same surface), never the
    /// background moving its own way.
    pub fn in_region(pyr: &Pyramid, mesh: MeshShape, region: Option<Vec<[f32; 2]>>, search: f32) -> Result<Self, PlanarError> {
        if !mesh.is_valid() || mesh.is_folded() {
            return Err(PlanarError::NoDetail);
        }
        let img = pyr.level(0);
        let (width, height) = (img.width, img.height);
        let size = (((width as f32 / 90.0).round() as i32) | 1).clamp(15, 31) as f32;
        let cells = mesh.cols as usize * mesh.rows as usize;
        let mut t = MeshTracker {
            features: Vec::new(),
            reference: mesh.clone(),
            prev: mesh.clone(),
            mesh,
            first: img.data.to_vec(),
            width,
            height,
            size,
            search: search.max(4.0),
            target: (cells * 8).clamp(120, 320),
            tol: (width as f32 / 700.0).max(1.5),
            rng: 0x2545_f491_4f6c_dd1d,
            rejected: Vec::new(),
            region,
            agreement: 1.0,
        };
        t.seed(pyr);
        if t.features.len() < 8 {
            return Err(PlanarError::NoDetail);
        }
        Ok(t)
    }

    /// The grid now.
    pub fn mesh(&self) -> &MeshShape {
        &self.mesh
    }

    pub fn feature_count(&self) -> usize {
        self.features.len()
    }

    /// Add features inside the grid as it is now, away from existing ones.
    fn seed(&mut self, pyr: &Pyramid) {
        let want = self.target.saturating_sub(self.features.len());
        if want == 0 {
            return;
        }
        let img = pyr.level(0);
        let mut taken: Vec<[f32; 2]> = self.features.iter().map(|f| f.t.pos).collect();
        taken.extend(&self.rejected);
        // The grid's outline can bulge past its corners' four-sided shape; search a box around it.
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for p in &self.mesh.points {
            (x0, y0, x1, y1) = (x0.min(p[0]), y0.min(p[1]), x1.max(p[0]), y1.max(p[1]));
        }
        let region = [[x0, y0], [x1, y0], [x1, y1], [x0, y1]];
        for pos in good_features(&img, &region, want * 2, self.size * 0.8, self.size * 0.5 + 2.0, &taken) {
            if self.features.len() >= self.target {
                break;
            }
            let Some(at) = self.mesh.locate(pos) else { continue };
            // Inside the grid. (Features right at its outline matter: they're what
            // pins the outline down. Their patches may be partly on what's behind
            // the surface, which costs a little accuracy there, under a pixel.)
            let (gu, gv) = (at.cell[0] as f32 + at.f[0], at.cell[1] as f32 + at.f[1]);
            if gu < 0.0 || gv < 0.0 || gu > self.mesh.cols as f32 || gv > self.mesh.rows as f32 {
                continue;
            }
            let t = FeatureTracker::new(pyr, pos, self.size, self.search);
            if t.is_flat() {
                continue;
            }
            let on_ref = self.reference.apply(at);
            // Outside the region (behind a mask's subject) it still holds the grid
            // where there's nothing else, but counts for little next to what's inside.
            let weight = if self.region.as_ref().is_some_and(|r| !crate::planar::inside(r, on_ref)) { OUTSIDE_WEIGHT } else { 1.0 };
            self.features.push(Feature { t, at, on_ref, misses: 0, age: 0, weight, with_region: 0 });
        }
    }

    /// Follow the surface into the next frame. Returns the grid there.
    pub fn step(&mut self, pyr: &Pyramid) -> Result<MeshShape, PlanarError> {
        // Predict: each point carries on as it last moved.
        let mut pred = self.mesh.clone();
        for (p, (a, b)) in pred.points.iter_mut().zip(self.mesh.points.iter().zip(&self.prev.points)) {
            *p = [2.0 * a[0] - b[0], 2.0 * a[1] - b[1]];
        }
        for f in &mut self.features {
            f.t.aim(pred.apply(f.at));
        }
        let results: Vec<([f32; 2], f32)> = par_map(&mut self.features, |f| f.t.step(pyr));
        let found: Vec<usize> = (0..self.features.len()).filter(|&i| results[i].1 >= MIN_SCORE).collect();
        if found.len() < 8 {
            return Err(PlanarError::Lost);
        }

        // The overall motion since the last frame, fitted robustly (it also
        // catches sudden moves the prediction missed). Bending since the last
        // frame is small.
        // The shape the grid is held to where features are few: the last grid,
        // moved by the overall motion (not each point carried on by its own last
        // movement, which overshoots when a ripple turns).
        let src: Vec<[f32; 2]> = found.iter().map(|&i| self.mesh.apply(self.features[i].at)).collect();
        let dst: Vec<[f32; 2]> = found.iter().map(|&i| results[i].0).collect();
        if let Some((g, _)) = ransac(&src, &dst, self.tol * 3.0, &mut self.rng) {
            pred = self.mesh.map(|p| g.apply(p));
        }

        // Solve the grid from the features, weighting down the ones that disagree.
        let pairs: Vec<(MeshCoord, [f32; 2], f64)> = found.iter().map(|&i| (self.features[i].at, results[i].0, self.features[i].weight)).collect();
        let cell = cell_size(&pred);
        let (mut mesh, mut w) = solve_robust(&pred, &pairs, &[(4.0 * self.tol).max(cell / 4.0), 3.0 * self.tol, 2.0 * self.tol]);
        // Following a region: that solve used only what's inside it. Features
        // outside that moved with it (more of the same surface) now count
        // fully, holding the grid's outer points; the rest (the background,
        // moving its own way) stay out. Solve again with them.
        let mut prior: Vec<f64> = self.features.iter().map(|f| f.weight).collect();
        if self.region.is_some() {
            for &i in &found {
                if prior[i] == 0.0 {
                    let f = &mut self.features[i];
                    let (q, p) = (mesh.apply(f.at), results[i].0);
                    // It has to keep up for a few frames running (the background can
                    // happen to move the same way for a moment).
                    f.with_region = if ((q[0] - p[0]).powi(2) + (q[1] - p[1]).powi(2)).sqrt() < 2.0 * self.tol { f.with_region + 1 } else { 0 };
                    if f.with_region >= TRUST_FRAMES {
                        prior[i] = 1.0;
                    }
                }
            }
            let pairs: Vec<(MeshCoord, [f32; 2], f64)> = found.iter().map(|&i| (self.features[i].at, results[i].0, prior[i])).collect();
            (mesh, w) = solve_robust(&mesh, &pairs, &[3.0 * self.tol, 2.0 * self.tol]);
        }

        // Re-check features against their first look, bent the way the grid has
        // bent around them, then solve again from the checked positions.
        let first = Luma { data: &self.first, width: self.width, height: self.height };
        let mut agrees = vec![false; self.features.len()];
        for (k, &i) in found.iter().enumerate() {
            agrees[i] = w[k] > 0.0;
        }
        let (reference, now) = (&self.reference, &mesh);
        let _ = par_map_enumerated(&mut self.features, |i, f| {
            f.age += 1;
            if agrees[i] {
                f.misses = 0;
                if let Some(local) = local_map(reference, now, f.at) {
                    let _ = f.t.correct(&first, f.on_ref, local, pyr);
                }
            } else {
                f.misses += 1;
                f.t.place(now.apply(f.at));
            }
        });
        let pairs: Vec<(MeshCoord, [f32; 2], f64)> = found.iter().filter(|&&i| agrees[i]).map(|&i| (self.features[i].at, self.features[i].t.pos, prior[i])).collect();
        if pairs.len() >= 8 {
            (mesh, w) = solve_robust(&mesh, &pairs, &[2.0 * self.tol, 2.0 * self.tol]);
        }
        let agree = w.iter().filter(|&&x| x > 0.0).count();
        // Judge by features followed a while (new ones may be on something else).
        let settled = found.iter().filter(|&&i| self.features[i].age >= 2).count();
        let pool = if settled >= 8 { settled } else { found.len() };
        if agree < 8 || (agree as f32) < 0.3 * pool.min(found.len()) as f32 || mesh.is_folded() {
            return Err(PlanarError::Lost);
        }
        self.agreement = agree as f32 / self.features.len().max(1) as f32;
        self.prev = std::mem::replace(&mut self.mesh, mesh);

        // Drop features that keep disagreeing or leave the picture; top up.
        let (wd, hd, margin) = (self.width as f32, self.height as f32, self.size);
        self.rejected.clear();
        let rejected = &mut self.rejected;
        self.features.retain(|f| {
            let inside = f.t.pos[0] > margin && f.t.pos[1] > margin && f.t.pos[0] < wd - margin && f.t.pos[1] < hd - margin;
            if f.misses > 2 {
                rejected.push(f.t.pos);
            }
            f.misses <= 2 && inside
        });
        if self.features.len() < self.target * 2 / 3 {
            self.seed(pyr);
        }
        Ok(self.mesh.clone())
    }
}

/// The typical side of a cell, in pixels.
fn cell_size(m: &MeshShape) -> f32 {
    let c = m.corners();
    let w = ((c[1][0] - c[0][0]).hypot(c[1][1] - c[0][1]) + (c[2][0] - c[3][0]).hypot(c[2][1] - c[3][1])) / 2.0 / m.cols as f32;
    let h = ((c[3][0] - c[0][0]).hypot(c[3][1] - c[0][1]) + (c[2][0] - c[1][0]).hypot(c[2][1] - c[1][1])) / 2.0 / m.rows as f32;
    w.min(h).max(1.0)
}

/// The 2×2 map taking small offsets around `at` in the first frame to the same
/// offsets now (the grid's local bend there).
fn local_map(reference: &MeshShape, now: &MeshShape, at: MeshCoord) -> Option<[[f32; 2]; 2]> {
    let d = |m: &MeshShape| -> [[f32; 2]; 2] {
        let e = 0.01;
        let a = m.apply(MeshCoord { f: [at.f[0] + e, at.f[1]], ..at });
        let b = m.apply(MeshCoord { f: [at.f[0] - e, at.f[1]], ..at });
        let c = m.apply(MeshCoord { f: [at.f[0], at.f[1] + e], ..at });
        let dd = m.apply(MeshCoord { f: [at.f[0], at.f[1] - e], ..at });
        // Columns: ∂/∂u, ∂/∂v.
        [[(a[0] - b[0]) / (2.0 * e), (c[0] - dd[0]) / (2.0 * e)], [(a[1] - b[1]) / (2.0 * e), (c[1] - dd[1]) / (2.0 * e)]]
    };
    let (r, n) = (d(reference), d(now));
    let det = r[0][0] * r[1][1] - r[0][1] * r[1][0];
    if det.abs() < 1e-6 {
        return None;
    }
    let ri = [[r[1][1] / det, -r[0][1] / det], [-r[1][0] / det, r[0][0] / det]];
    // now · ref⁻¹
    Some([
        [n[0][0] * ri[0][0] + n[0][1] * ri[1][0], n[0][0] * ri[0][1] + n[0][1] * ri[1][1]],
        [n[1][0] * ri[0][0] + n[1][1] * ri[1][0], n[1][0] * ri[0][1] + n[1][1] * ri[1][1]],
    ])
}

/// Solve the grid from `pairs` (a spot on the grid, where it was found),
/// starting from `start`, re-weighting with Tukey's biweight at each cut-off
/// in turn. Returns the grid and each pair's final weight.
fn solve_robust(start: &MeshShape, pairs: &[(MeshCoord, [f32; 2], f64)], cutoffs: &[f32]) -> (MeshShape, Vec<f64>) {
    let mut mesh = start.clone();
    let mut w = vec![1.0f64; pairs.len()];
    for &c in cutoffs {
        for (k, (at, p, prior)) in pairs.iter().enumerate() {
            let q = mesh.apply(*at);
            let r = (((q[0] - p[0]).powi(2) + (q[1] - p[1]).powi(2)).sqrt() / c) as f64;
            w[k] = if r >= 1.0 { 0.0 } else { prior * (1.0 - r * r).powi(2) };
        }
        if w.iter().filter(|&&x| x > 0.0).count() < 4 {
            break;
        }
        if let Some(m) = solve_mesh(&mesh, start, pairs, &w) {
            mesh = m;
        }
    }
    // Final weights against the solved grid.
    let c = *cutoffs.last().unwrap_or(&2.0);
    for (k, (at, p, _)) in pairs.iter().enumerate() {
        let q = mesh.apply(*at);
        let r = (((q[0] - p[0]).powi(2) + (q[1] - p[1]).powi(2)).sqrt() / c) as f64;
        w[k] = if r >= 1.0 { 0.0 } else { (1.0 - r * r).powi(2) };
    }
    (mesh, w)
}

/// One weighted least-squares solve for the grid's points: features pull
/// their cells into place; each edge between neighbouring points is held to
/// its vector in `shape` (smoothness); and every point is tied, very weakly,
/// to where it is in `shape` (so a point with nothing pulling it stays put).
fn solve_mesh(init: &MeshShape, shape: &MeshShape, pairs: &[(MeshCoord, [f32; 2], f64)], w: &[f64]) -> Option<MeshShape> {
    let (cols, rows) = (init.cols as usize, init.rows as usize);
    let n = (cols + 1) * (rows + 1);
    let idx = |i: usize, j: usize| j * (cols + 1) + i;
    let mut a = vec![0.0f64; n * n];
    let mut bx = vec![0.0f64; n];
    let mut by = vec![0.0f64; n];
    let mut total = 0.0;
    let mut data = vec![0.0f64; n];
    for ((at, p, _), &wk) in pairs.iter().zip(w) {
        if wk <= 0.0 {
            continue;
        }
        let (i, j) = (at.cell[0] as usize, at.cell[1] as usize);
        let (u, v) = (at.f[0] as f64, at.f[1] as f64);
        let verts = [idx(i, j), idx(i + 1, j), idx(i + 1, j + 1), idx(i, j + 1)];
        let bw = [(1.0 - u) * (1.0 - v), u * (1.0 - v), u * v, (1.0 - u) * v];
        for r in 0..4 {
            data[verts[r]] += wk * bw[r] * bw[r];
            bx[verts[r]] += wk * bw[r] * p[0] as f64;
            by[verts[r]] += wk * bw[r] * p[1] as f64;
            for c in 0..4 {
                a[verts[r] * n + verts[c]] += wk * bw[r] * bw[c];
            }
        }
        total += wk;
    }
    // Smoothness, scaled to the data so it means the same however many features
    // there are: light where both points have plenty of features pulling them,
    // firmer where they don't (so they move with their neighbours, not wildly).
    let m = (total / n as f64).max(0.05);
    let mu = 1e-3 * m;
    let mut edge = |p: usize, q: usize| {
        let lambda = m * (0.1 + 0.6 * (-(data[p] + data[q]) / m).exp());
        let (sp, sq) = (shape.points[p], shape.points[q]);
        let (dx, dy) = ((sp[0] - sq[0]) as f64, (sp[1] - sq[1]) as f64);
        a[p * n + p] += lambda;
        a[q * n + q] += lambda;
        a[p * n + q] -= lambda;
        a[q * n + p] -= lambda;
        bx[p] += lambda * dx;
        bx[q] -= lambda * dx;
        by[p] += lambda * dy;
        by[q] -= lambda * dy;
    };
    for j in 0..=rows {
        for i in 0..=cols {
            if i < cols {
                edge(idx(i, j), idx(i + 1, j));
            }
            if j < rows {
                edge(idx(i, j), idx(i, j + 1));
            }
        }
    }
    for k in 0..n {
        a[k * n + k] += mu;
        bx[k] += mu * shape.points[k][0] as f64;
        by[k] += mu * shape.points[k][1] as f64;
    }
    let x = solve_dense(a.clone(), bx, n)?;
    let y = solve_dense(a, by, n)?;
    Some(MeshShape { cols: init.cols, rows: init.rows, points: (0..n).map(|k| [x[k] as f32, y[k] as f32]).collect() })
}

/// Solve the n×n system `a x = b` (row-major), by elimination with partial pivoting.
fn solve_dense(mut a: Vec<f64>, mut b: Vec<f64>, n: usize) -> Option<Vec<f64>> {
    for col in 0..n {
        let piv = (col..n).max_by(|&p, &q| a[p * n + col].abs().total_cmp(&a[q * n + col].abs()))?;
        if a[piv * n + col].abs() < 1e-12 {
            return None;
        }
        if piv != col {
            for c in 0..n {
                a.swap(col * n + c, piv * n + c);
            }
            b.swap(col, piv);
        }
        for r in col + 1..n {
            let f = a[r * n + col] / a[col * n + col];
            if f == 0.0 {
                continue;
            }
            for c in col..n {
                a[r * n + c] -= f * a[col * n + c];
            }
            b[r] -= f * b[col];
        }
    }
    let mut x = vec![0.0; n];
    for r in (0..n).rev() {
        let s: f64 = (r + 1..n).map(|c| a[r * n + c] * x[c]).sum();
        x[r] = (b[r] - s) / a[r * n + r];
    }
    Some(x)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noise(x: f32, y: f32) -> f32 {
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
    }

    /// The sheet's point (u, v) in 0–1 at frame `k`: it drifts, shears, and
    /// ripples like a flag (a travelling wave, strongest at the free end).
    fn place(k: usize, u: f32, v: f32, amp: f32) -> [f32; 2] {
        let t = k as f32;
        let wave = amp * (u * 5.0 - t * 0.25).sin() * u;
        [70.0 + 180.0 * u + 1.2 * t, 50.0 + 140.0 * v + wave + 0.4 * t * u]
    }

    /// The sheet's (u, v) at frame-0 position `p`.
    fn sheet_at(p: [f32; 2], amp: f32) -> (f32, f32) {
        let (mut u, mut v) = ((p[0] - 70.0) / 180.0, (p[1] - 50.0) / 140.0);
        for _ in 0..12 {
            let q = place(0, u, v, amp);
            let e = 1e-3;
            let (pu, pv) = (place(0, u + e, v, amp), place(0, u, v + e, amp));
            let (a, b, c, d) = ((pu[0] - q[0]) / e, (pv[0] - q[0]) / e, (pu[1] - q[1]) / e, (pv[1] - q[1]) / e);
            let det = a * d - b * c;
            let (rx, ry) = (p[0] - q[0], p[1] - q[1]);
            u += (d * rx - b * ry) / det;
            v += (-c * rx + a * ry) / det;
        }
        (u, v)
    }

    /// Track the sheet with a `cols` × `rows` grid over 24 frames. Returns the
    /// worst and mean error of where the tracked grid puts points of the
    /// surface, and the same for the best possible grid of that size (its
    /// points exactly on the surface): a grid's straight-edged cells can't
    /// follow a curve inside them, so that's the floor.
    fn ripple(cols: u8, rows: u8, amp: f32) -> ((f32, f32), (f32, f32)) {
        let (w, h) = (320usize, 240usize);
        let tex = |u: f32, v: f32| 30.0 + 160.0 * noise(u * 26.0, v * 20.0) + 50.0 * noise(u * 70.0 + 5.0, v * 55.0);
        let render = |k: usize| -> Vec<f32> {
            let mut img = vec![60.0f32; w * h];
            for y in 0..h {
                for x in 0..w {
                    // Invert the frame-k map with Newton steps.
                    let (mut u, mut v) = (((x as f32) - 70.0) / 180.0, ((y as f32) - 50.0) / 140.0);
                    for _ in 0..8 {
                        let p = place(k, u, v, amp);
                        let e = 1e-3;
                        let (pu, pv) = (place(k, u + e, v, amp), place(k, u, v + e, amp));
                        let (a, b, c, d) = ((pu[0] - p[0]) / e, (pv[0] - p[0]) / e, (pu[1] - p[1]) / e, (pv[1] - p[1]) / e);
                        let det = a * d - b * c;
                        let (rx, ry) = (x as f32 - p[0], y as f32 - p[1]);
                        u += (d * rx - b * ry) / det;
                        v += (-c * rx + a * ry) / det;
                    }
                    if (0.0..=1.0).contains(&u) && (0.0..=1.0).contains(&v) {
                        img[y * w + x] = tex(u, v);
                    }
                }
            }
            img
        };
        let grid_at = |k: usize| MeshShape {
            cols,
            rows,
            points: (0..=rows).flat_map(|j| (0..=cols).map(move |i| (i, j))).map(|(i, j)| place(k, i as f32 / cols as f32, j as f32 / rows as f32, amp)).collect(),
        };
        let start = grid_at(0);
        // Surface points to check, spread over the grid (frame-0 positions).
        let mut probes = Vec::new();
        for j in 0..=12 {
            for i in 0..=16 {
                let (gu, gv) = (i as f32 / 16.0 * cols as f32, j as f32 / 12.0 * rows as f32);
                let cell = [(gu.floor() as u8).min(cols - 1), (gv.floor() as u8).min(rows - 1)];
                let at = MeshCoord { cell, f: [gu - cell[0] as f32, gv - cell[1] as f32] };
                let p0 = start.apply(at);
                probes.push((at, sheet_at(p0, amp)));
            }
        }
        let pyr = Pyramid::new(render(0), w, h, 1);
        let mut mt = MeshTracker::new(&pyr, start, 14.0).expect("detail");
        let (mut worst, mut sum, mut n) = (0f32, 0f32, 0f32);
        let (mut iworst, mut isum) = (0f32, 0f32);
        for k in 1..24 {
            let pyr = Pyramid::new(render(k), w, h, 1);
            let m = mt.step(&pyr).unwrap_or_else(|e| panic!("lost at {k}: {e:?}"));
            let ideal = grid_at(k);
            let (mut fw, mut fiw) = (0f32, 0f32);
            for (at, (u, v)) in &probes {
                let truth = place(k, *u, *v, amp);
                let e = |q: [f32; 2]| ((q[0] - truth[0]).powi(2) + (q[1] - truth[1]).powi(2)).sqrt();
                let (et, ei) = (e(m.apply(*at)), e(ideal.apply(*at)));
                (worst, sum, iworst, isum, n) = (worst.max(et), sum + et, iworst.max(ei), isum + ei, n + 1.0);
                (fw, fiw) = (fw.max(et), fiw.max(ei));
            }
            if std::env::var_os("RF_DEBUG").is_some() {
                eprintln!("frame {k}: worst {fw:.2} (ideal {fiw:.2}), agreement {:.2}, {} features", mt.agreement, mt.feature_count());
            }
        }
        ((worst, sum / n), (iworst, isum / n))
    }

    #[test]
    fn follows_a_drifting_shearing_sheet_exactly() {
        let ((worst, mean), _) = ripple(4, 3, 0.0);
        eprintln!("mesh, no ripple: worst {worst:.2} px, mean {mean:.2} px");
        // (The worst is at the outer corners: see the note on features at the outline.)
        assert!(worst < 1.5 && mean < 0.3, "worst {worst:.2}, mean {mean:.2}");
    }

    /// A rippling sheet: the tracked grid must warp the surface about as well
    /// as the best grid of its size could.
    #[test]
    fn follows_a_rippling_sheet() {
        for (c, r) in [(4u8, 3u8), (8, 4)] {
            let ((worst, mean), (iworst, imean)) = ripple(c, r, 9.0);
            eprintln!("mesh {c}x{r}, ripple: worst {worst:.2} px, mean {mean:.2} px (best possible grid: {iworst:.2}, {imean:.2})");
            assert!(mean < imean + 0.6 && worst < iworst + 2.0, "{c}x{r}: worst {worst:.2} vs {iworst:.2}, mean {mean:.2} vs {imean:.2}");
        }
    }
}
