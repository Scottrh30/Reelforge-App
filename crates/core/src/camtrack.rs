//! 3D camera tracking (camera solving, "match moving"): from many features
//! followed through a shot, work out where the camera was and which way it
//! pointed in every frame, its lens (field of view), and where the features
//! are in 3D, so 3D layers, text, points and lights can be put into the shot
//! and stay put as the camera moves.
//!
//! How it's solved:
//! 1. **Keyframes** are picked through the shot so that neighbours share most
//!    of their features.
//! 2. Two keyframes with plenty of **parallax** (motion a flat picture can't
//!    explain) start it: the essential matrix between them (eight-point
//!    method inside RANSAC) gives how the camera moved, and the features they
//!    share are triangulated.
//! 3. Each further keyframe is placed from the points already known
//!    (resection), new points are triangulated, and every few keyframes
//!    **bundle adjustment** (Levenberg–Marquardt over every camera and point
//!    at once, with the Schur complement, robust to bad tracks) tightens it.
//! 4. The **lens**, unless it's known: several fields of view are tried and
//!    the one that explains the tracks best is refined in a last adjustment.
//! 5. Every other frame is placed from the points; the points are refined
//!    from every frame, and the frames placed again.
//!
//! A shot with too little parallax (a camera on a tripod, only turning) is
//! solved as a turning camera: its features are directions, put at one
//! distance.
//!
//! The result is turned into the editor's world ([`CameraTrack`]): pixels,
//! X right, Y down, Z away, with the camera at the first frame where the
//! default camera is, so a 3D layer at Z = 0 sits on the features at about
//! the middle of the scene's depth.

use crate::space3d::Camera;
use crate::time::Time;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};

type V3 = [f64; 3];
type M3 = [[f64; 3]; 3];

// ---- Small linear algebra -------------------------------------------------------

fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn scale(a: V3, k: f64) -> V3 {
    [a[0] * k, a[1] * k, a[2] * k]
}
fn dot(a: V3, b: V3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: V3, b: V3) -> V3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
fn len(a: V3) -> f64 {
    dot(a, a).sqrt()
}
fn unit(a: V3) -> Option<V3> {
    let l = len(a);
    (l > 1e-12 && l.is_finite()).then(|| scale(a, 1.0 / l))
}
const I3: M3 = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
fn mv(m: &M3, v: V3) -> V3 {
    [dot(m[0], v), dot(m[1], v), dot(m[2], v)]
}
fn tr(m: &M3) -> M3 {
    [[m[0][0], m[1][0], m[2][0]], [m[0][1], m[1][1], m[2][1]], [m[0][2], m[1][2], m[2][2]]]
}
fn mm(a: &M3, b: &M3) -> M3 {
    let mut o = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            o[i][j] = a[i][0] * b[0][j] + a[i][1] * b[1][j] + a[i][2] * b[2][j];
        }
    }
    o
}
fn det(m: &M3) -> f64 {
    dot(m[0], cross(m[1], m[2]))
}
fn skew(v: V3) -> M3 {
    [[0.0, -v[2], v[1]], [v[2], 0.0, -v[0]], [-v[1], v[0], 0.0]]
}
/// The rotation by `w` (axis × angle in radians).
fn exp_so3(w: V3) -> M3 {
    let th = len(w);
    let k = skew(w);
    let k2 = mm(&k, &k);
    let (a, b) = if th < 1e-8 { (1.0, 0.5) } else { (th.sin() / th, (1.0 - th.cos()) / (th * th)) };
    let mut o = I3;
    for i in 0..3 {
        for j in 0..3 {
            o[i][j] += a * k[i][j] + b * k2[i][j];
        }
    }
    o
}
fn inv3(m: &[[f64; 3]; 3]) -> Option<M3> {
    let d = det(m);
    if d.abs() < 1e-300 || !d.is_finite() {
        return None;
    }
    let c = |r0: usize, r1: usize, c0: usize, c1: usize| m[r0][c0] * m[r1][c1] - m[r0][c1] * m[r1][c0];
    Some([
        [c(1, 2, 1, 2) / d, -c(0, 2, 1, 2) / d, c(0, 1, 1, 2) / d],
        [-c(1, 2, 0, 2) / d, c(0, 2, 0, 2) / d, -c(0, 1, 0, 2) / d],
        [c(1, 2, 0, 1) / d, -c(0, 2, 0, 1) / d, c(0, 1, 0, 1) / d],
    ])
}

/// Eigenvalues and eigenvectors (columns of the returned row-major matrix)
/// of symmetric `n` × `n` matrix `a` (row-major), by Jacobi rotations;
/// sorted from the smallest eigenvalue.
fn sym_eigen(a: &[f64], n: usize) -> (Vec<f64>, Vec<f64>) {
    let mut a = a.to_vec();
    let mut v = vec![0.0; n * n];
    for i in 0..n {
        v[i * n + i] = 1.0;
    }
    for _sweep in 0..60 {
        let off: f64 = (0..n).flat_map(|i| (0..n).filter(move |&j| j != i).map(move |j| (i, j))).map(|(i, j)| a[i * n + j] * a[i * n + j]).sum();
        let total: f64 = a.iter().map(|x| x * x).sum();
        if off <= 1e-30 * total.max(1e-300) {
            break;
        }
        for p in 0..n {
            for q in p + 1..n {
                let apq = a[p * n + q];
                if apq.abs() < 1e-300 {
                    continue;
                }
                let theta = (a[q * n + q] - a[p * n + p]) / (2.0 * apq);
                let sgn = if theta >= 0.0 { 1.0 } else { -1.0 };
                let t = sgn / (theta.abs() + (theta * theta + 1.0).sqrt());
                let c = 1.0 / (t * t + 1.0).sqrt();
                let s = t * c;
                for k in 0..n {
                    let (akp, akq) = (a[k * n + p], a[k * n + q]);
                    a[k * n + p] = c * akp - s * akq;
                    a[k * n + q] = s * akp + c * akq;
                }
                for k in 0..n {
                    let (apk, aqk) = (a[p * n + k], a[q * n + k]);
                    a[p * n + k] = c * apk - s * aqk;
                    a[q * n + k] = s * apk + c * aqk;
                }
                for k in 0..n {
                    let (vkp, vkq) = (v[k * n + p], v[k * n + q]);
                    v[k * n + p] = c * vkp - s * vkq;
                    v[k * n + q] = s * vkp + c * vkq;
                }
            }
        }
    }
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&i, &j| a[i * n + i].total_cmp(&a[j * n + j]));
    let vals = order.iter().map(|&i| a[i * n + i]).collect();
    let mut vecs = vec![0.0; n * n];
    for (c, &i) in order.iter().enumerate() {
        for r in 0..n {
            vecs[r * n + c] = v[r * n + i];
        }
    }
    (vals, vecs)
}

/// The unit vector `x` that makes `|A x|` smallest, from `AᵀA` (`n` × `n`).
fn null_vector(ata: &[f64], n: usize) -> Vec<f64> {
    let (_, v) = sym_eigen(ata, n);
    (0..n).map(|r| v[r * n]).collect()
}

/// Singular value decomposition of a 3 × 3 matrix: `m = U · diag(s) · Vᵀ`,
/// singular values largest first.
fn svd3(m: &M3) -> (M3, V3, M3) {
    let mtm = mm(&tr(m), m);
    let flat: Vec<f64> = mtm.iter().flatten().copied().collect();
    let (vals, vecs) = sym_eigen(&flat, 3);
    // Largest first.
    let col = |c: usize| [vecs[c], vecs[3 + c], vecs[6 + c]];
    let vcols = [col(2), col(1), col(0)];
    let s = [vals[2].max(0.0).sqrt(), vals[1].max(0.0).sqrt(), vals[0].max(0.0).sqrt()];
    let mut u = [[0.0; 3]; 3];
    let u0 = unit(mv(m, vcols[0])).unwrap_or([1.0, 0.0, 0.0]);
    let u1 = unit(sub(mv(m, vcols[1]), scale(u0, dot(u0, mv(m, vcols[1]))))).unwrap_or_else(|| unit(cross(u0, [0.0, 0.0, 1.0])).or_else(|| unit(cross(u0, [0.0, 1.0, 0.0]))).unwrap());
    let u2 = if s[2] > 1e-9 * s[0].max(1e-300) { unit(mv(m, vcols[2])).unwrap_or(cross(u0, u1)) } else { cross(u0, u1) };
    for (c, uc) in [u0, u1, u2].into_iter().enumerate() {
        for r in 0..3 {
            u[r][c] = uc[r];
        }
    }
    let mut vm = [[0.0; 3]; 3];
    for (c, vc) in vcols.into_iter().enumerate() {
        for r in 0..3 {
            vm[r][c] = vc[r];
        }
    }
    (u, s, vm)
}

/// The nearest rotation to `m`.
fn orthonormal(m: &M3) -> M3 {
    let (u, _, v) = svd3(m);
    let mut r = mm(&u, &tr(&v));
    if det(&r) < 0.0 {
        let mut u2 = u;
        for row in &mut u2 {
            row[2] = -row[2];
        }
        r = mm(&u2, &tr(&v));
    }
    r
}

/// Solve symmetric positive-definite `a` (`n` × `n`, row-major) · x = `b` in place (Cholesky).
fn cholesky_solve(a: &mut [f64], n: usize, b: &mut [f64]) -> bool {
    for j in 0..n {
        let mut d = a[j * n + j];
        for k in 0..j {
            d -= a[j * n + k] * a[j * n + k];
        }
        if d <= 0.0 || !d.is_finite() {
            return false;
        }
        let d = d.sqrt();
        a[j * n + j] = d;
        for i in j + 1..n {
            let mut s = a[i * n + j];
            for k in 0..j {
                s -= a[i * n + k] * a[j * n + k];
            }
            a[i * n + j] = s / d;
        }
    }
    for i in 0..n {
        let mut s = b[i];
        for k in 0..i {
            s -= a[i * n + k] * b[k];
        }
        b[i] = s / a[i * n + i];
    }
    for i in (0..n).rev() {
        let mut s = b[i];
        for k in i + 1..n {
            s -= a[k * n + i] * b[k];
        }
        b[i] = s / a[i * n + i];
    }
    true
}

/// A small fast random generator (the same every run).
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

// ---- The problem ----------------------------------------------------------------------

/// One feature followed through the shot: where it is (frame pixels) in
/// frames `start`, `start + 1`, … in a row.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Track2d {
    pub start: usize,
    pub pos: Vec<[f32; 2]>,
}

impl Track2d {
    /// One past its last frame.
    pub fn end(&self) -> usize {
        self.start + self.pos.len()
    }
    fn at(&self, f: usize) -> Option<[f64; 2]> {
        let p = self.pos.get(f.checked_sub(self.start)?)?;
        Some([p[0] as f64, p[1] as f64])
    }
}

/// Where a camera is and which way it looks: a world point `x` is at
/// `r · (x − c)` in the camera's own space (right, down, forward).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pose {
    pub r: M3,
    pub c: V3,
}

impl Pose {
    pub const IDENTITY: Pose = Pose { r: I3, c: [0.0; 3] };
    fn view(&self, x: V3) -> V3 {
        mv(&self.r, sub(x, self.c))
    }
    fn t(&self) -> V3 {
        scale(mv(&self.r, self.c), -1.0)
    }
    fn from_rt(r: M3, t: V3) -> Pose {
        Pose { r, c: scale(mv(&tr(&r), t), -1.0) }
    }
}

/// What's known before solving.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct SolveOptions {
    /// The lens's vertical field of view in degrees, when known.
    pub fov: Option<f32>,
    /// The camera only turned (on a tripod): solve it as turning in place.
    pub tripod: bool,
}

/// A solved shot.
#[derive(Clone, Debug, PartialEq)]
pub struct Solution {
    /// The lens: focal length in frame pixels.
    pub focal: f64,
    /// The camera in each frame (`None` where it couldn't be placed).
    pub poses: Vec<Option<Pose>>,
    /// The 3D features: the track each came from, where it is, and how far
    /// (pixels, root mean square) its track is from where it projects.
    pub points: Vec<(usize, V3, f32)>,
    /// How well it all fits: root mean square, pixels.
    pub error: f32,
    pub tripod: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SolveError {
    /// Too few features were followed for long enough.
    TooFewFeatures,
    /// The shot is too short.
    TooShort,
    /// No two frames to start from (too little in common).
    NoStart,
    /// It didn't come together.
    Failed,
    Cancelled,
}

impl std::fmt::Display for SolveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            SolveError::TooFewFeatures => "too few details could be followed through the shot: it needs texture and detail that stays in view for a while",
            SolveError::TooShort => "the shot is too short to solve: it needs at least 10 frames",
            SolveError::NoStart => "no two frames share enough details to start from: the camera moves too fast, or the picture is too plain",
            SolveError::Failed => "the camera's motion couldn't be worked out from this shot (a lot of the picture moving on its own, or too little camera movement)",
            SolveError::Cancelled => "stopped",
        })
    }
}

/// How far (pixels) a projection may be from its track before it counts less (Huber).
const HUBER: f64 = 2.0;
/// Points that miss their tracks by more than this (pixels, rms) are dropped.
const BAD_POINT: f64 = 3.0;

#[derive(Clone)]
struct Recon {
    /// The frame held still in bundle adjustment (where the solve started).
    anchor: usize,
    poses: Vec<Option<Pose>>,
    pts: Vec<Option<V3>>,
    focal: f64,
    tripod: bool,
}

struct Ctx<'a> {
    tracks: &'a [Track2d],
    by_frame: Vec<Vec<(usize, [f64; 2])>>,
    cc: [f64; 2],
    frames: usize,
    cancel: &'a AtomicBool,
}

fn huber_w(e: f64) -> f64 {
    if e <= HUBER {
        1.0
    } else {
        HUBER / e
    }
}
fn huber_cost(e: f64) -> f64 {
    if e <= HUBER {
        e * e
    } else {
        2.0 * HUBER * e - HUBER * HUBER
    }
}

impl Ctx<'_> {
    fn project(&self, p: &Pose, f: f64, x: V3) -> Option<[f64; 2]> {
        let v = p.view(x);
        (v[2] > 1e-9).then(|| [self.cc[0] + f * v[0] / v[2], self.cc[1] + f * v[1] / v[2]])
    }
    fn err(&self, p: &Pose, f: f64, x: V3, obs: [f64; 2]) -> f64 {
        match self.project(p, f, x) {
            Some(q) => ((q[0] - obs[0]).powi(2) + (q[1] - obs[1]).powi(2)).sqrt(),
            None => 1e6,
        }
    }
    /// The ray (camera space, unnormalised) through pixel `u`.
    fn ray(&self, f: f64, u: [f64; 2]) -> V3 {
        [(u[0] - self.cc[0]) / f, (u[1] - self.cc[1]) / f, 1.0]
    }

    /// Keyframes: neighbours share most of their features, no more than
    /// about 40 of them through the shot.
    fn keyframes(&self) -> Vec<usize> {
        let n = self.frames;
        let max_gap = n.div_ceil(40).max(1);
        let mut out = vec![0];
        let mut last = 0;
        let mut f = 1;
        while f < n {
            let base = self.by_frame[last].len().max(1);
            let shared = self.by_frame[f].iter().filter(|(id, _)| self.tracks[*id].start <= last).count();
            if f - last >= max_gap || (shared as f64) < 0.7 * base as f64 {
                let k = if f - 1 > last && f - last < max_gap { f - 1 } else { f };
                out.push(k);
                last = k;
            }
            f += 1;
        }
        if *out.last().unwrap() != n - 1 {
            out.push(n - 1);
        }
        out
    }

    /// Features seen in both frames `a` < `b`: (track, where in a, where in b).
    fn shared(&self, a: usize, b: usize) -> Vec<(usize, [f64; 2], [f64; 2])> {
        self.by_frame[b].iter().filter_map(|&(id, pb)| Some((id, self.tracks[id].at(a)?, pb))).collect()
    }

    /// How much parallax between two frames: how far (pixels) the shared
    /// features are from where the best flat (homography) motion puts them
    /// (the twelfth furthest), and how many are more than 2 px off.
    fn parallax(&self, pairs: &[(usize, [f64; 2], [f64; 2])]) -> (f64, usize) {
        let src: Vec<[f32; 2]> = pairs.iter().map(|p| [p.1[0] as f32, p.1[1] as f32]).collect();
        let dst: Vec<[f32; 2]> = pairs.iter().map(|p| [p.2[0] as f32, p.2[1] as f32]).collect();
        let mut w = vec![1.0f64; src.len()];
        let mut res = vec![0.0f64; src.len()];
        for _ in 0..4 {
            let Some(h) = crate::planar::fit_homography_weighted(&src, &dst, Some(&w)) else { return (0.0, 0) };
            for i in 0..src.len() {
                let q = h.apply(src[i]);
                res[i] = (((q[0] - dst[i][0]) as f64).powi(2) + ((q[1] - dst[i][1]) as f64).powi(2)).sqrt();
                w[i] = 1.0 / (1.0 + (res[i] / 3.0).powi(2));
            }
        }
        // Not a median or a percentile: when most features lie on one
        // surface (a wall), a flat motion explains them however much depth
        // the rest have. A dozen features clearly off it is parallax.
        let mut r = res.clone();
        r.sort_by(|a, b| b.total_cmp(a));
        (r[11.min(r.len() - 1)], res.iter().filter(|e| **e > 2.0).count())
    }

    /// Triangulate from several views (camera, pixel); `None` behind a camera.
    fn triangulate(&self, f: f64, views: &[(Pose, [f64; 2])]) -> Option<V3> {
        let mut ata = [0.0f64; 16];
        for (p, u) in views {
            let x = self.ray(f, *u);
            let t = p.t();
            for k in 0..2 {
                let mut a = [0.0; 4];
                for j in 0..3 {
                    a[j] = x[k] * p.r[2][j] - p.r[k][j];
                }
                a[3] = x[k] * t[2] - t[k];
                // Each row normalised so near and far views count alike.
                let n = (a.iter().map(|v| v * v).sum::<f64>()).sqrt().max(1e-300);
                for i in 0..4 {
                    for j in 0..4 {
                        ata[i * 4 + j] += a[i] * a[j] / (n * n);
                    }
                }
            }
        }
        let h = null_vector(&ata, 4);
        if h[3].abs() < 1e-12 {
            return None;
        }
        let x = [h[0] / h[3], h[1] / h[3], h[2] / h[3]];
        views.iter().all(|(p, _)| p.view(x)[2] > 0.0).then_some(x)
    }

    /// The essential matrix between frames (eight-point method in RANSAC)
    /// and the camera motion it gives: frame b relative to frame a.
    fn relative_pose(&self, f: f64, pairs: &[(usize, [f64; 2], [f64; 2])]) -> Option<(Pose, Vec<usize>)> {
        let n = pairs.len();
        if n < 12 {
            return None;
        }
        let xa: Vec<V3> = pairs.iter().map(|p| self.ray(f, p.1)).collect();
        let xb: Vec<V3> = pairs.iter().map(|p| self.ray(f, p.2)).collect();
        let fit = |idx: &[usize]| -> Option<M3> {
            let mut ata = [0.0f64; 81];
            for &i in idx {
                let (a, b) = (xa[i], xb[i]);
                let r = [b[0] * a[0], b[0] * a[1], b[0], b[1] * a[0], b[1] * a[1], b[1], a[0], a[1], 1.0];
                for p in 0..9 {
                    for q in 0..9 {
                        ata[p * 9 + q] += r[p] * r[q];
                    }
                }
            }
            let e = null_vector(&ata, 9);
            let m = [[e[0], e[1], e[2]], [e[3], e[4], e[5]], [e[6], e[7], e[8]]];
            let (u, _, v) = svd3(&m);
            let mut d = [[0.0; 3]; 3];
            d[0][0] = 1.0;
            d[1][1] = 1.0;
            Some(mm(&mm(&u, &d), &tr(&v)))
        };
        // Sampson distance, in pixels squared.
        let sampson = |e: &M3, i: usize| -> f64 {
            let (a, b) = (xa[i], xb[i]);
            let ea = mv(e, a);
            let etb = mv(&tr(e), b);
            let num = dot(b, ea);
            let den = ea[0] * ea[0] + ea[1] * ea[1] + etb[0] * etb[0] + etb[1] * etb[1];
            num * num / den.max(1e-300) * f * f
        };
        let thresh = 1.5f64 * 1.5;
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15 ^ n as u64);
        let mut best: Option<(M3, usize)> = None;
        for _ in 0..400 {
            let mut idx = Vec::with_capacity(8);
            while idx.len() < 8 {
                let k = rng.below(n);
                if !idx.contains(&k) {
                    idx.push(k);
                }
            }
            let Some(e) = fit(&idx) else { continue };
            let count = (0..n).filter(|&i| sampson(&e, i) < thresh).count();
            if best.as_ref().is_none_or(|b| count > b.1) {
                best = Some((e, count));
            }
        }
        let (mut e, _) = best?;
        for _ in 0..2 {
            let inl: Vec<usize> = (0..n).filter(|&i| sampson(&e, i) < thresh * 2.0).collect();
            if inl.len() < 8 {
                return None;
            }
            e = fit(&inl)?;
        }
        let inliers: Vec<usize> = (0..n).filter(|&i| sampson(&e, i) < thresh * 2.0).collect();
        // The four ways the essential matrix splits into a turn and a move:
        // the one with the points in front of both cameras.
        let (mut u, _, mut v) = svd3(&e);
        if det(&u) < 0.0 {
            u = u.map(|r| r.map(|x| -x));
        }
        if det(&v) < 0.0 {
            v = v.map(|r| r.map(|x| -x));
        }
        let w = [[0.0, -1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]];
        let r1 = mm(&mm(&u, &w), &tr(&v));
        let r2 = mm(&mm(&u, &tr(&w)), &tr(&v));
        let t = [u[0][2], u[1][2], u[2][2]];
        let mut best_pose = None;
        let mut best_front = 0;
        for (r, t) in [(r1, t), (r1, scale(t, -1.0)), (r2, t), (r2, scale(t, -1.0))] {
            let pb = Pose::from_rt(r, t);
            let front = inliers.iter().filter(|&&i| self.triangulate(f, &[(Pose::IDENTITY, pairs[i].1), (pb, pairs[i].2)]).is_some()).count();
            if front > best_front {
                best_front = front;
                best_pose = Some(pb);
            }
        }
        let pb = best_pose?;
        (best_front * 2 > inliers.len()).then(|| (pb, inliers.iter().map(|&i| pairs[i].0).collect()))
    }

    /// Bundle adjustment of the cameras in `frames` (the first held still)
    /// and every point seen by two of them; the lens too with `free_focal`.
    /// Returns the rms error (pixels).
    fn bundle(&self, rc: &mut Recon, frames: &[usize], free_focal: bool, iters: usize) -> f64 {
        let rot_only = rc.tripod;
        let d = if rot_only { 3 } else { 6 };
        let mut cam_of = vec![usize::MAX; self.frames];
        for (k, &f) in frames.iter().enumerate().skip(1) {
            cam_of[f] = (k - 1) * d;
        }
        let in_ba = |f: usize| frames.contains(&f);
        let fidx = (frames.len() - 1) * d;
        let np = fidx + usize::from(free_focal);
        // Points and their observations among these cameras.
        let mut point_obs: Vec<(usize, Vec<(usize, [f64; 2])>)> = Vec::new();
        for (id, trk) in self.tracks.iter().enumerate() {
            if rc.pts[id].is_none() {
                continue;
            }
            let obs: Vec<(usize, [f64; 2])> = frames.iter().filter_map(|&f| Some((f, trk.at(f)?))).filter(|(f, _)| in_ba(*f) && rc.poses[*f].is_some()).collect();
            if obs.len() >= 2 {
                point_obs.push((id, obs));
            }
        }
        let cost_of = |rc: &Recon| -> (f64, f64, usize) {
            let (mut c, mut sq, mut n) = (0.0, 0.0, 0);
            for (id, obs) in &point_obs {
                let x = rc.pts[*id].unwrap();
                for (f, u) in obs {
                    let e = self.err(rc.poses[*f].as_ref().unwrap(), rc.focal, x, *u);
                    c += huber_cost(e);
                    if e < 5.0 {
                        sq += e * e;
                        n += 1;
                    }
                }
            }
            (c, sq, n)
        };
        let mut lambda = 1e-3;
        let (mut cost, ..) = cost_of(rc);
        for _ in 0..iters {
            if self.cancel.load(Ordering::Relaxed) {
                break;
            }
            // Normal equations: cameras (+ lens) dense; points as 3 × 3 blocks.
            let mut a = vec![0.0; np * np];
            let mut g = vec![0.0; np];
            struct Blk {
                id: usize,
                c: M3,
                g: V3,
                b: Vec<(usize, V3)>,
            }
            let mut blocks: Vec<Blk> = Vec::with_capacity(point_obs.len());
            for (id, obs) in &point_obs {
                let x = rc.pts[*id].unwrap();
                let mut blk = Blk { id: *id, c: [[0.0; 3]; 3], g: [0.0; 3], b: Vec::new() };
                for (f, u) in obs {
                    let pose = rc.poses[*f].as_ref().unwrap();
                    let rx = mv(&pose.r, x);
                    let xc = add(rx, pose.t());
                    if xc[2] <= 1e-9 {
                        continue;
                    }
                    let iz = 1.0 / xc[2];
                    let fo = rc.focal;
                    let pu = [self.cc[0] + fo * xc[0] * iz, self.cc[1] + fo * xc[1] * iz];
                    let r = [pu[0] - u[0], pu[1] - u[1]];
                    let w = huber_w((r[0] * r[0] + r[1] * r[1]).sqrt());
                    // d(pixel)/d(camera-space point).
                    let jx = [[fo * iz, 0.0, -fo * xc[0] * iz * iz], [0.0, fo * iz, -fo * xc[1] * iz * iz]];
                    let jp: [V3; 2] = [mv(&tr(&pose.r), jx[0]), mv(&tr(&pose.r), jx[1])];
                    // Camera-side columns: (parameter index, column).
                    let mut cols: Vec<(usize, [f64; 2])> = Vec::with_capacity(7);
                    if cam_of[*f] != usize::MAX {
                        let o = cam_of[*f];
                        // d xc / d w = −[R x]×.
                        let sk = skew(rx);
                        for k in 0..3 {
                            let colv = [-sk[0][k], -sk[1][k], -sk[2][k]];
                            cols.push((o + k, [dot(jx[0], colv), dot(jx[1], colv)]));
                        }
                        if !rot_only {
                            for k in 0..3 {
                                cols.push((o + 3 + k, [jx[0][k], jx[1][k]]));
                            }
                        }
                    }
                    if free_focal {
                        cols.push((fidx, [xc[0] * iz, xc[1] * iz]));
                    }
                    for (i, ci) in &cols {
                        g[*i] += w * (ci[0] * r[0] + ci[1] * r[1]);
                        for (j, cj) in &cols {
                            a[i * np + j] += w * (ci[0] * cj[0] + ci[1] * cj[1]);
                        }
                        let bv = [
                            w * (ci[0] * jp[0][0] + ci[1] * jp[1][0]),
                            w * (ci[0] * jp[0][1] + ci[1] * jp[1][1]),
                            w * (ci[0] * jp[0][2] + ci[1] * jp[1][2]),
                        ];
                        match blk.b.iter_mut().find(|(k, _)| k == i) {
                            Some((_, v)) => *v = add(*v, bv),
                            None => blk.b.push((*i, bv)),
                        }
                    }
                    for p in 0..3 {
                        blk.g[p] += w * (jp[0][p] * r[0] + jp[1][p] * r[1]);
                        for q in 0..3 {
                            blk.c[p][q] += w * (jp[0][p] * jp[0][q] + jp[1][p] * jp[1][q]);
                        }
                    }
                }
                blocks.push(blk);
            }
            let mut improved = false;
            for _try in 0..6 {
                let mut s = a.clone();
                for i in 0..np {
                    s[i * np + i] += lambda * a[i * np + i] + 1e-9;
                }
                let mut rhs: Vec<f64> = g.iter().map(|v| -v).collect();
                let mut cinv = Vec::with_capacity(blocks.len());
                for blk in &blocks {
                    let mut c = blk.c;
                    for k in 0..3 {
                        c[k][k] += lambda * c[k][k] + 1e-9;
                    }
                    let ci = inv3(&c).unwrap_or([[0.0; 3]; 3]);
                    let v = mv(&ci, blk.g);
                    for (i, bi) in &blk.b {
                        rhs[*i] += dot(*bi, v);
                        let cb = mv(&ci, *bi);
                        for (j, bj) in &blk.b {
                            s[i * np + j] -= dot(cb, *bj);
                        }
                    }
                    cinv.push(ci);
                }
                if np > 0 && !cholesky_solve(&mut s, np, &mut rhs) {
                    lambda *= 10.0;
                    continue;
                }
                let delta = rhs;
                let mut trial = rc.clone();
                for (k, &f) in frames.iter().enumerate().skip(1) {
                    let o = (k - 1) * d;
                    let p = trial.poses[f].as_mut().unwrap();
                    let t = p.t();
                    let r = mm(&exp_so3([delta[o], delta[o + 1], delta[o + 2]]), &p.r);
                    let t = if rot_only { [0.0; 3] } else { add(t, [delta[o + 3], delta[o + 4], delta[o + 5]]) };
                    *p = Pose::from_rt(r, t);
                    if rot_only {
                        p.c = [0.0; 3];
                    }
                }
                if free_focal {
                    trial.focal = (trial.focal + delta[fidx]).max(1.0);
                }
                for (blk, ci) in blocks.iter().zip(&cinv) {
                    let mut v = blk.g;
                    for (i, bi) in &blk.b {
                        v = add(v, scale(*bi, delta[*i]));
                    }
                    let dp = scale(mv(ci, v), -1.0);
                    if let Some(x) = trial.pts[blk.id].as_mut() {
                        *x = add(*x, dp);
                    }
                }
                let (c2, ..) = cost_of(&trial);
                if c2.is_finite() && c2 < cost {
                    let rel = (cost - c2) / cost.max(1e-300);
                    *rc = trial;
                    cost = c2;
                    lambda = (lambda * 0.3).max(1e-9);
                    improved = rel > 1e-7;
                    break;
                }
                lambda *= 8.0;
            }
            if !improved {
                break;
            }
        }
        let (_, sq, n) = cost_of(rc);
        (sq / n.max(1) as f64).sqrt()
    }

    /// Place frame `f`'s camera from the known points, starting from `init`.
    /// Returns it with the rms error of its good points and how many of its
    /// known points are good.
    fn place(&self, rc: &Recon, f: usize, init: Pose) -> Option<(Pose, f64, f64)> {
        let obs: Vec<(V3, [f64; 2])> = self.by_frame[f].iter().filter_map(|&(id, u)| Some((rc.pts[id]?, u))).collect();
        if obs.len() < 6 {
            return None;
        }
        let rot_only = rc.tripod;
        let d = if rot_only { 3 } else { 6 };
        let mut pose = init;
        let cost_of = |p: &Pose| obs.iter().map(|(x, u)| huber_cost(self.err(p, rc.focal, *x, *u))).sum::<f64>();
        let mut cost = cost_of(&pose);
        let mut lambda = 1e-3;
        for _ in 0..30 {
            let mut a = [0.0f64; 36];
            let mut g = [0.0f64; 6];
            for (x, u) in &obs {
                let rx = mv(&pose.r, *x);
                let xc = add(rx, pose.t());
                if xc[2] <= 1e-9 {
                    continue;
                }
                let iz = 1.0 / xc[2];
                let fo = rc.focal;
                let r = [self.cc[0] + fo * xc[0] * iz - u[0], self.cc[1] + fo * xc[1] * iz - u[1]];
                let w = huber_w((r[0] * r[0] + r[1] * r[1]).sqrt());
                let jx = [[fo * iz, 0.0, -fo * xc[0] * iz * iz], [0.0, fo * iz, -fo * xc[1] * iz * iz]];
                let sk = skew(rx);
                let mut cols = [[0.0f64; 2]; 6];
                for k in 0..3 {
                    let colv = [-sk[0][k], -sk[1][k], -sk[2][k]];
                    cols[k] = [dot(jx[0], colv), dot(jx[1], colv)];
                    cols[3 + k] = [jx[0][k], jx[1][k]];
                }
                for i in 0..d {
                    g[i] += w * (cols[i][0] * r[0] + cols[i][1] * r[1]);
                    for j in 0..d {
                        a[i * 6 + j] += w * (cols[i][0] * cols[j][0] + cols[i][1] * cols[j][1]);
                    }
                }
            }
            let mut improved = false;
            for _try in 0..6 {
                let mut s = vec![0.0; d * d];
                for i in 0..d {
                    for j in 0..d {
                        s[i * d + j] = a[i * 6 + j];
                    }
                    s[i * d + i] += lambda * a[i * 6 + i] + 1e-12;
                }
                let mut rhs: Vec<f64> = g[..d].iter().map(|v| -v).collect();
                if !cholesky_solve(&mut s, d, &mut rhs) {
                    lambda *= 10.0;
                    continue;
                }
                let r = mm(&exp_so3([rhs[0], rhs[1], rhs[2]]), &pose.r);
                let t = if rot_only { [0.0; 3] } else { add(pose.t(), [rhs[3], rhs[4], rhs[5]]) };
                let trial = Pose::from_rt(r, t);
                let c2 = cost_of(&trial);
                if c2 < cost {
                    let rel = (cost - c2) / cost.max(1e-300);
                    pose = trial;
                    cost = c2;
                    lambda = (lambda * 0.3).max(1e-9);
                    improved = rel > 1e-9;
                    break;
                }
                lambda *= 8.0;
            }
            if !improved {
                break;
            }
        }
        let errs: Vec<f64> = obs.iter().map(|(x, u)| self.err(&pose, rc.focal, *x, *u)).collect();
        let good: Vec<f64> = errs.iter().copied().filter(|e| *e < 4.0).collect();
        if good.len() < 6 {
            return None;
        }
        let rms = (good.iter().map(|e| e * e).sum::<f64>() / good.len() as f64).sqrt();
        Some((pose, rms, good.len() as f64 / errs.len() as f64))
    }

    /// Give a 3D point to every track seen by at least two of the placed
    /// `frames` (with enough angle between the views) that hasn't one yet.
    fn triangulate_new(&self, rc: &mut Recon, frames: &[usize]) {
        for (id, track) in self.tracks.iter().enumerate() {
            if rc.pts[id].is_some() {
                continue;
            }
            let views: Vec<(Pose, [f64; 2])> = frames.iter().filter_map(|&f| Some((rc.poses[f]?, track.at(f)?))).collect();
            if views.len() < 2 {
                continue;
            }
            if rc.tripod {
                // A direction: the average ray, at distance 1.
                let mut s = [0.0; 3];
                for (p, u) in &views {
                    s = add(s, unit(mv(&tr(&p.r), self.ray(rc.focal, *u))).unwrap());
                }
                let Some(x) = unit(s) else { continue };
                if views.iter().all(|(p, u)| self.err(p, rc.focal, x, *u) < 4.0) {
                    rc.pts[id] = Some(x);
                }
                continue;
            }
            // The widest angle between two of the rays.
            let dirs: Vec<(V3, V3)> = views.iter().map(|(p, u)| (p.c, unit(mv(&tr(&p.r), self.ray(rc.focal, *u))).unwrap())).collect();
            let Some(x) = self.triangulate(rc.focal, &views) else { continue };
            let mut widest = 0.0f64;
            for i in 0..dirs.len() {
                for j in i + 1..dirs.len() {
                    let (a, b) = (unit(sub(x, dirs[i].0)), unit(sub(x, dirs[j].0)));
                    if let (Some(a), Some(b)) = (a, b) {
                        widest = widest.max(dot(a, b).clamp(-1.0, 1.0).acos());
                    }
                }
            }
            if widest < 1.0f64.to_radians() {
                continue;
            }
            if views.iter().all(|(p, u)| self.err(p, rc.focal, x, *u) < 4.0) {
                rc.pts[id] = Some(x);
            }
        }
    }

    /// Each point's rms error over the placed frames that see it.
    fn point_error(&self, rc: &Recon, id: usize) -> Option<(f64, usize)> {
        let x = rc.pts[id]?;
        let trk = &self.tracks[id];
        let (mut sq, mut n) = (0.0, 0);
        for f in trk.start..trk.end().min(self.frames) {
            if let Some(p) = &rc.poses[f] {
                let e = self.err(p, rc.focal, x, trk.at(f).unwrap());
                sq += e * e;
                n += 1;
            }
        }
        (n > 0).then(|| ((sq / n as f64).sqrt(), n))
    }

    fn drop_bad_points(&self, rc: &mut Recon) {
        for id in 0..self.tracks.len() {
            if let Some((e, _)) = self.point_error(rc, id) {
                if e > BAD_POINT {
                    rc.pts[id] = None;
                }
            }
        }
    }

    /// How well a solve explains the shot at keyframes `kfs`: the root mean
    /// square of every observation's error, each capped at 3 px, with tracks
    /// left without a point and keyframes left unplaced counting the cap.
    /// (So a solve can't look good by throwing tracks away.)
    fn score(&self, rc: &Recon, kfs: &[usize]) -> f64 {
        const CAP: f64 = 2.25;
        let (mut sum, mut n) = (0.0, 0usize);
        for &f in kfs {
            for &(id, u) in &self.by_frame[f] {
                if self.tracks[id].pos.len() < 10 {
                    continue;
                }
                sum += match (&rc.poses[f], rc.pts[id]) {
                    (Some(p), Some(x)) => self.err(p, rc.focal, x, u).powi(2).min(CAP),
                    _ => CAP,
                };
                n += 1;
            }
        }
        let simpler = if rc.tripod { 0.85 } else { 1.0 };
        (sum / n.max(1) as f64).sqrt() * simpler
    }

    /// Refine one point from every placed frame that sees it.
    fn refine_point(&self, rc: &Recon, id: usize) -> Option<V3> {
        let mut x = rc.pts[id]?;
        let trk = &self.tracks[id];
        let views: Vec<(Pose, [f64; 2])> = (trk.start..trk.end().min(self.frames)).filter_map(|f| Some((rc.poses[f]?, trk.at(f)?))).collect();
        if views.len() < 2 {
            return Some(x);
        }
        for _ in 0..8 {
            let mut c = [[0.0f64; 3]; 3];
            let mut g = [0.0f64; 3];
            for (p, u) in &views {
                let xc = p.view(x);
                if xc[2] <= 1e-9 {
                    continue;
                }
                let iz = 1.0 / xc[2];
                let fo = rc.focal;
                let r = [self.cc[0] + fo * xc[0] * iz - u[0], self.cc[1] + fo * xc[1] * iz - u[1]];
                let w = huber_w((r[0] * r[0] + r[1] * r[1]).sqrt());
                let jx = [[fo * iz, 0.0, -fo * xc[0] * iz * iz], [0.0, fo * iz, -fo * xc[1] * iz * iz]];
                let jp = [mv(&tr(&p.r), jx[0]), mv(&tr(&p.r), jx[1])];
                for a in 0..3 {
                    g[a] += w * (jp[0][a] * r[0] + jp[1][a] * r[1]);
                    for b in 0..3 {
                        c[a][b] += w * (jp[0][a] * jp[0][b] + jp[1][a] * jp[1][b]);
                    }
                }
            }
            for k in 0..3 {
                c[k][k] += 1e-6 * c[k][k] + 1e-12;
            }
            let ci = inv3(&c)?;
            let dx = scale(mv(&ci, g), -1.0);
            x = add(x, dx);
            if len(dx) < 1e-9 * len(x).max(1.0) {
                break;
            }
        }
        if rc.tripod {
            x = unit(x)?;
        }
        Some(x)
    }

    /// The camera turn between frames `a` and `b` from a flat (homography)
    /// fit, for a camera turning in place.
    fn rotation_between(&self, f: f64, a: usize, b: usize) -> Option<M3> {
        let pairs = self.shared(a, b);
        if pairs.len() < 8 {
            return None;
        }
        let src: Vec<[f32; 2]> = pairs.iter().map(|p| [((p.1[0] - self.cc[0]) / f) as f32, ((p.1[1] - self.cc[1]) / f) as f32]).collect();
        let dst: Vec<[f32; 2]> = pairs.iter().map(|p| [((p.2[0] - self.cc[0]) / f) as f32, ((p.2[1] - self.cc[1]) / f) as f32]).collect();
        let h = crate::planar::fit_homography(&src, &dst)?;
        let m = [[h.0[0], h.0[1], h.0[2]], [h.0[3], h.0[4], h.0[5]], [h.0[6], h.0[7], h.0[8]]];
        Some(orthonormal(&m))
    }

    /// Solve the keyframes with lens `focal` (fixed), starting from keyframes
    /// `kfs[ia]` and `kfs[ib]`.
    fn solve_keyframes(&self, kfs: &[usize], ia: usize, ib: usize, focal: f64, tripod: bool) -> Option<Recon> {
        let (a, b) = (kfs[ia], kfs[ib]);
        let mut rc = Recon { anchor: a, poses: vec![None; self.frames], pts: vec![None; self.tracks.len()], focal, tripod };
        rc.poses[a] = Some(Pose::IDENTITY);
        if tripod {
            let r = self.rotation_between(focal, a, b)?;
            rc.poses[b] = Some(Pose { r, c: [0.0; 3] });
        } else {
            let pairs = self.shared(a, b);
            let (pb, _) = self.relative_pose(focal, &pairs)?;
            rc.poses[b] = Some(pb);
        }
        self.triangulate_new(&mut rc, &[a, b]);
        if rc.pts.iter().flatten().count() < 10 {
            return None;
        }
        let mut solved = vec![a, b];
        self.bundle(&mut rc, &solved, false, 10);
        self.drop_bad_points(&mut rc);
        // Then the keyframes between, and outwards both ways.
        let mut order: Vec<usize> = (ia + 1..ib).collect();
        let (mut fwd, mut back) = (ib + 1, ia as isize - 1);
        let (mut fwd_ok, mut back_ok) = (true, true);
        let mut since = 0;
        loop {
            let next = if let Some(k) = order.pop() {
                Some(k)
            } else if fwd_ok && fwd < kfs.len() {
                fwd += 1;
                Some(fwd - 1)
            } else if back_ok && back >= 0 {
                back -= 1;
                Some((back + 1) as usize)
            } else {
                None
            };
            let Some(k) = next else { break };
            if self.cancel.load(Ordering::Relaxed) {
                return None;
            }
            let f = kfs[k];
            // Start from the nearest placed keyframe.
            let near = solved.iter().copied().min_by_key(|&s| s.abs_diff(f)).unwrap();
            let init = rc.poses[near].unwrap();
            let placed = self.place(&rc, f, init).filter(|(_, rms, good)| *rms < 3.0 && *good > 0.5);
            let Some((pose, ..)) = placed else {
                if k > ib {
                    fwd_ok = false;
                } else if k < ia {
                    back_ok = false;
                }
                continue;
            };
            rc.poses[f] = Some(pose);
            solved.push(f);
            solved.sort_unstable();
            self.triangulate_new(&mut rc, &solved);
            since += 1;
            if since >= 3 {
                // Locally: the dozen placed keyframes nearest the new one, the
                // furthest of them held still (so long shots stay quick; the
                // whole lot is adjusted at the end).
                let mut near: Vec<usize> = solved.clone();
                near.sort_by_key(|&s| s.abs_diff(f));
                near.truncate(12);
                let hold = if near.contains(&a) { a } else { *near.last().unwrap() };
                let mut order_kf = vec![hold];
                order_kf.extend(near.iter().copied().filter(|&s| s != hold));
                self.bundle(&mut rc, &order_kf, false, 8);
                self.drop_bad_points(&mut rc);
                since = 0;
            }
        }
        let mut order_kf = vec![a];
        order_kf.extend(solved.iter().copied().filter(|&s| s != a));
        self.bundle(&mut rc, &order_kf, false, 25);
        self.drop_bad_points(&mut rc);
        Some(rc)
    }
}

/// The vertical field of view (degrees) of focal length `f` (pixels) for a frame `h` pixels high.
pub fn fov_of(f: f64, h: f64) -> f64 {
    2.0 * (h * 0.5 / f).atan().to_degrees()
}
/// The focal length (pixels) of vertical field of view `fov` (degrees) for a frame `h` pixels high.
pub fn focal_of(fov: f64, h: f64) -> f64 {
    h * 0.5 / (fov.to_radians() * 0.5).tan()
}

/// Solve a shot from its feature tracks (frame pixels, `frames` frames of a
/// frame `size` pixels). `progress` gets 0–1.
pub fn solve(tracks: &[Track2d], frames: usize, size: [f32; 2], opts: SolveOptions, cancel: &AtomicBool, progress: &mut dyn FnMut(f32)) -> Result<Solution, SolveError> {
    if frames < 10 {
        return Err(SolveError::TooShort);
    }
    let mut by_frame: Vec<Vec<(usize, [f64; 2])>> = vec![Vec::new(); frames];
    for (id, t) in tracks.iter().enumerate() {
        if t.pos.len() < 3 {
            continue;
        }
        for f in t.start..t.end().min(frames) {
            by_frame[f].push((id, t.at(f).unwrap()));
        }
    }
    if tracks.iter().filter(|t| t.pos.len() >= 10).count() < 20 {
        return Err(SolveError::TooFewFeatures);
    }
    let ctx = Ctx { tracks, by_frame, cc: [size[0] as f64 * 0.5, size[1] as f64 * 0.5], frames, cancel };
    let h = size[1] as f64;
    let kfs = ctx.keyframes();
    progress(0.05);

    // The two keyframes to start from: plenty in common, plenty of features
    // off any one surface, the most parallax. The best two are tried.
    let mut pairs_ranked: Vec<(usize, usize, f64)> = Vec::new();
    let mut best_par = 0.0f64;
    for ia in 0..kfs.len() {
        for ib in ia + 1..kfs.len() {
            let pairs = ctx.shared(kfs[ia], kfs[ib]);
            if pairs.len() < 20 {
                continue;
            }
            let (par, off) = ctx.parallax(&pairs);
            best_par = best_par.max(par);
            let score = par.min(12.0) * (pairs.len() as f64).sqrt() * (off.min(40) as f64 / 40.0).max(0.05);
            pairs_ranked.push((ia, ib, score));
        }
    }
    if pairs_ranked.is_empty() {
        return Err(SolveError::NoStart);
    }
    pairs_ranked.sort_by(|a, b| b.2.total_cmp(&a.2));
    // The second start: a pair not sharing a keyframe with the first.
    let first = (pairs_ranked[0].0, pairs_ranked[0].1);
    let mut starts = vec![first];
    if let Some(p) = pairs_ranked.iter().find(|p| p.0 != first.0 && p.1 != first.1 && p.0 != first.1 && p.1 != first.0) {
        starts.push((p.0, p.1));
    }
    // Clearly turning in place, clearly moving, or unclear (then both are
    // tried, and the better fit wins).
    let modes: Vec<bool> = if opts.tripod || best_par < 0.7 {
        vec![true]
    } else if best_par < 4.0 {
        vec![false, true]
    } else {
        vec![false]
    };
    progress(0.1);

    // The lens: as given, or the best of several, refined.
    let fovs: Vec<f64> = match opts.fov {
        Some(f) => vec![f as f64],
        None => vec![20.0, 28.0, 36.0, 45.0, 55.0, 68.0, 85.0],
    };
    let mut cands: Vec<(f64, Recon)> = Vec::new();
    // (A camera turning in place needs no parallax: one start does.)
    let tries: Vec<(bool, f64, (usize, usize))> = modes
        .iter()
        .flat_map(|&m| {
            let st: Vec<(usize, usize)> = if m { starts[..1].to_vec() } else { starts.clone() };
            fovs.iter().flat_map(move |&f| st.clone().into_iter().map(move |p| (m, f, p)))
        })
        .collect();
    for (k, &(tripod, fov, (ia, ib))) in tries.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Err(SolveError::Cancelled);
        }
        if let Some(mut rc) = ctx.solve_keyframes(&kfs, ia, ib, focal_of(fov, h), tripod) {
            let placed: Vec<usize> = kfs.iter().copied().filter(|&f| rc.poses[f].is_some()).collect();
            let mut order = vec![rc.anchor];
            order.extend(placed.iter().copied().filter(|&f| f != rc.anchor));
            ctx.bundle(&mut rc, &order, false, 3);
            // (A camera turning in place is the simpler answer: it wins unless
            // moving explains the tracks clearly better.)
            cands.push((ctx.score(&rc, &kfs), rc));
        }
        progress(0.1 + 0.5 * (k + 1) as f32 / tries.len() as f32);
    }
    cands.sort_by(|a, b| a.0.total_cmp(&b.0));
    // The lens: refined from the three best starting guesses; the best fit wins.
    let free = |mut rc: Recon| -> (f64, Recon) {
        let placed: Vec<usize> = kfs.iter().copied().filter(|&f| rc.poses[f].is_some()).collect();
        let mut order = vec![rc.anchor];
        order.extend(placed.iter().copied().filter(|&f| f != rc.anchor));
        ctx.bundle(&mut rc, &order, true, 40);
        ctx.drop_bad_points(&mut rc);
        // Tracks dropped while the lens was still the guess get another chance.
        ctx.triangulate_new(&mut rc, &placed);
        ctx.bundle(&mut rc, &order, true, 40);
        ctx.drop_bad_points(&mut rc);
        ctx.bundle(&mut rc, &order, true, 20);
        (ctx.score(&rc, &kfs), rc)
    };
    let mut rc = if opts.fov.is_some() {
        cands.into_iter().next().ok_or(SolveError::Failed)?.1
    } else {
        let mut refined: Vec<(f64, Recon)> = cands.into_iter().take(3).map(|(_, rc)| free(rc)).collect();
        refined.sort_by(|a, b| a.0.total_cmp(&b.0));
        refined.into_iter().next().ok_or(SolveError::Failed)?.1
    };
    progress(0.7);
    if cancel.load(Ordering::Relaxed) {
        return Err(SolveError::Cancelled);
    }

    // Every frame: forwards from each placed one, then backwards.
    let place_all = |rc: &mut Recon, again: bool| {
        let is_kf = |f: usize| kfs.binary_search(&f).is_ok();
        let mut last: Option<Pose> = None;
        for f in 0..frames {
            if rc.poses[f].is_some() && (!again || is_kf(f)) {
                last = rc.poses[f];
                continue;
            }
            let init = if again { rc.poses[f].or(last) } else { last };
            let Some(init) = init else { continue };
            match ctx.place(rc, f, init) {
                Some((p, rms, good)) if rms < 4.0 && good > 0.4 => {
                    rc.poses[f] = Some(p);
                    last = Some(p);
                }
                _ => {}
            }
        }
        let mut next: Option<Pose> = None;
        for f in (0..frames).rev() {
            if rc.poses[f].is_some() {
                next = rc.poses[f];
                continue;
            }
            let Some(init) = next else { continue };
            if let Some((p, rms, good)) = ctx.place(rc, f, init) {
                if rms < 4.0 && good > 0.4 {
                    rc.poses[f] = Some(p);
                    next = Some(p);
                }
            }
        }
    };
    place_all(&mut rc, false);
    progress(0.8);
    let all: Vec<usize> = (0..frames).filter(|&f| rc.poses[f].is_some()).collect();
    ctx.triangulate_new(&mut rc, &all);
    for id in 0..tracks.len() {
        if rc.pts[id].is_some() {
            rc.pts[id] = ctx.refine_point(&rc, id);
        }
    }
    ctx.drop_bad_points(&mut rc);
    place_all(&mut rc, true);
    progress(0.95);

    let placed = rc.poses.iter().filter(|p| p.is_some()).count();
    if placed < frames / 2 || rc.pts.iter().flatten().count() < 8 {
        return Err(SolveError::Failed);
    }
    let mut points = Vec::new();
    let (mut sq, mut n) = (0.0, 0usize);
    for id in 0..tracks.len() {
        let Some(x) = rc.pts[id] else { continue };
        let Some((e, k)) = ctx.point_error(&rc, id) else { continue };
        if e > BAD_POINT {
            continue;
        }
        sq += e * e * k as f64;
        n += k;
        points.push((id, x, e as f32));
    }
    let error = (sq / n.max(1) as f64).sqrt() as f32;
    progress(1.0);
    Ok(Solution { focal: rc.focal, poses: rc.poses, points, error, tripod: rc.tripod })
}

// ---- Following features through the shot ---------------------------------------------

/// Follows a few hundred features through a shot, frame by frame, for
/// solving: features that are lost or leave the picture end their tracks,
/// and new ones are found where there's room.
pub struct SceneFollower {
    target: usize,
    size: f32,
    search: f32,
    min_dist: f32,
    width: f32,
    height: f32,
    /// Working pixels → output pixels.
    scale: f32,
    /// Each followed feature: its track, its follower, and how it looked when first found.
    active: Vec<(usize, crate::track::FeatureTracker, crate::track::Template)>,
    tracks: Vec<Track2d>,
    frame: usize,
}

/// A feature that no longer matches its first look this well has changed
/// too much (turned, foreshortened) to follow without drifting: its track
/// ends, and a fresh feature takes over.
const SAME_LOOK: f32 = 0.8;

impl SceneFollower {
    /// For frames `width` × `height` (working pixels), with positions put
    /// out multiplied by `scale`.
    pub fn new(width: usize, height: usize, scale: f32) -> Self {
        let w = width as f32;
        SceneFollower {
            target: 260,
            size: 15.0,
            search: (w / 24.0).max(16.0),
            min_dist: (w / 40.0).max(10.0),
            width: w,
            height: height as f32,
            scale,
            active: Vec::new(),
            tracks: Vec::new(),
            frame: 0,
        }
    }

    /// A working position (pixel centres at whole numbers, as the matcher
    /// gives them) → output pixels (pixel centres at halves, as the frame
    /// and the camera have them).
    fn out(&self, p: [f32; 2]) -> [f32; 2] {
        [(p[0] + 0.5) * self.scale, (p[1] + 0.5) * self.scale]
    }

    /// Pyramid levels the frames need.
    pub fn levels(&self) -> usize {
        crate::track::Pyramid::levels_for(self.search) + 1
    }

    /// The next frame.
    pub fn add_frame(&mut self, pyr: &crate::track::Pyramid) {
        let margin = self.size;
        let (w, h) = (self.width, self.height);
        let inside = |p: [f32; 2]| p[0] >= margin && p[1] >= margin && p[0] < w - margin && p[1] < h - margin;
        if self.frame > 0 {
            let mut keep = Vec::with_capacity(self.active.len());
            for (id, mut f, first) in std::mem::take(&mut self.active) {
                let (p, score) = f.step(pyr);
                if score < crate::track::MIN_SCORE || !inside(p) {
                    continue;
                }
                let (_, same) = first.find(&pyr.level(0), p, 1.0);
                if same < SAME_LOOK {
                    continue;
                }
                let q = self.out(p);
                self.tracks[id].pos.push(q);
                keep.push((id, f, first));
            }
            self.active = keep;
        }
        if self.active.len() < self.target * 4 / 5 {
            let taken: Vec<[f32; 2]> = self.active.iter().map(|(_, f, _)| f.pos).collect();
            let region = [[0.0, 0.0], [w, 0.0], [w, h], [0.0, h]];
            let fresh = crate::planar::good_features(&pyr.level(0), &region, self.target - self.active.len(), self.min_dist, margin + 2.0, &taken);
            for p in fresh {
                let f = crate::track::FeatureTracker::new(pyr, p, self.size, self.search);
                if f.is_flat() {
                    continue;
                }
                let first = crate::track::Template::new(&pyr.level(0), p, self.size);
                let q = self.out(p);
                self.tracks.push(Track2d { start: self.frame, pos: vec![q] });
                self.active.push((self.tracks.len() - 1, f, first));
            }
        }
        self.frame += 1;
    }

    /// Every track (output pixels), short ones left out.
    pub fn tracks(self) -> Vec<Track2d> {
        self.tracks.into_iter().filter(|t| t.pos.len() >= 4).collect()
    }
}

// ---- In the editor's world -------------------------------------------------------------

/// A camera in one frame: as a camera clip's settings.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct CamPose {
    pub position: [f32; 3],
    pub target: [f32; 3],
    pub roll: f32,
}

/// A solved 3D feature: where it is, how well it fits (pixels), and the
/// frames it was seen in.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ScenePoint {
    pub pos: [f32; 3],
    pub error: f32,
    pub first: u32,
    pub last: u32,
}

/// A solved shot in the editor's world (frame pixels, X right, Y down, Z
/// away), kept on the footage clip.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CameraTrack {
    /// The media time of each frame, in order.
    pub times: Vec<Time>,
    /// The camera in each frame.
    pub cameras: Vec<CamPose>,
    /// Vertical field of view, degrees.
    pub fov: f32,
    /// The frame size it was solved in (the sequence's).
    pub frame: [f32; 2],
    pub points: Vec<ScenePoint>,
    /// Root mean square error, pixels.
    pub error: f32,
    /// Solved as a camera turning in place.
    #[serde(default)]
    pub tripod: bool,
    /// How many frames could be placed from the features (the rest are filled in).
    #[serde(default)]
    pub placed: u32,
    /// The camera clip made from it, so later layers use the same one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera: Option<crate::project::ClipId>,
}

fn to3(v: V3) -> [f32; 3] {
    [v[0] as f32, v[1] as f32, v[2] as f32]
}
fn to64(v: [f32; 3]) -> V3 {
    [v[0] as f64, v[1] as f64, v[2] as f64]
}

/// Camera settings (position, point of interest at `dist`, roll) for a pose.
pub(crate) fn cam_pose(p: &Pose, dist: f64) -> CamPose {
    let (right, fwd) = (p.r[0], p.r[2]);
    // The camera's right with no roll (see `space3d::Camera::basis`).
    let r0 = unit(cross([0.0, 1.0, 0.0], fwd)).or_else(|| unit(cross(fwd, [0.0, 0.0, 1.0]))).unwrap_or([1.0, 0.0, 0.0]);
    let roll = dot(right, cross(fwd, r0)).atan2(dot(right, r0)).to_degrees();
    CamPose { position: to3(p.c), target: to3(add(p.c, scale(fwd, dist))), roll: roll as f32 }
}

/// The nearest rotation between two poses, a fraction `k` of the way.
fn blend(a: &Pose, b: &Pose, k: f64) -> Pose {
    let mut r = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            r[i][j] = a.r[i][j] * (1.0 - k) + b.r[i][j] * k;
        }
    }
    Pose { r: orthonormal(&r), c: add(scale(a.c, 1.0 - k), scale(b.c, k)) }
}

impl Solution {
    /// Into the editor's world, for frame size `frame`: the camera in frame
    /// `reference` (the first placed one if that wasn't) where the default
    /// camera is, and scaled so the features at the middle of the scene's
    /// depth then are at Z = 0. Frames that couldn't be placed are filled in
    /// from their neighbours. `times` are the frames' media times.
    pub fn into_track(self, tracks: &[Track2d], times: Vec<Time>, frame: [f32; 2], reference: usize) -> Option<CameraTrack> {
        let n = self.poses.len();
        let placed: Vec<usize> = (0..n).filter(|&f| self.poses[f].is_some()).collect();
        let reference = if self.poses.get(reference).is_some_and(|p| p.is_some()) { reference } else { *placed.first()? };
        let ref_pose = self.poses[reference]?;
        // The lens in frame pixels (the tracks are in frame pixels already).
        let focal = self.focal;
        let big_f = focal_of(fov_of(focal, frame[1] as f64), frame[1] as f64);
        // Scale: the median depth of the features seen at the reference frame → the focal distance.
        let mut depths: Vec<f64> = self.points.iter().filter(|(id, ..)| tracks[*id].at(reference).is_some()).map(|(_, x, _)| ref_pose.view(*x)[2]).filter(|z| *z > 0.0).collect();
        if depths.is_empty() {
            depths = self.points.iter().map(|(_, x, _)| ref_pose.view(*x)[2]).filter(|z| *z > 0.0).collect();
        }
        depths.sort_by(f64::total_cmp);
        let median = *depths.get(depths.len() / 2)?;
        let s = big_f / median;
        let origin = [0.0, 0.0, -big_f];
        let rr = ref_pose.r;
        let world = |x: V3| add(scale(mv(&rr, sub(x, ref_pose.c)), s), origin);
        let pose_of = |p: &Pose| Pose { r: mm(&p.r, &tr(&rr)), c: add(origin, scale(mv(&rr, sub(p.c, ref_pose.c)), s)) };
        // Fill the gaps: blend between placed neighbours, or hold the nearest.
        let mut cameras = Vec::with_capacity(n);
        for f in 0..n {
            let p = match self.poses[f] {
                Some(p) => p,
                None => {
                    let before = placed.iter().rev().find(|&&k| k < f).copied();
                    let after = placed.iter().find(|&&k| k > f).copied();
                    match (before, after) {
                        (Some(a), Some(b)) => blend(&self.poses[a].unwrap(), &self.poses[b].unwrap(), (f - a) as f64 / (b - a) as f64),
                        (Some(a), None) => self.poses[a].unwrap(),
                        (None, Some(b)) => self.poses[b].unwrap(),
                        (None, None) => return None,
                    }
                }
            };
            cameras.push(cam_pose(&pose_of(&p), big_f));
        }
        let points = self
            .points
            .iter()
            .map(|(id, x, e)| {
                let t = &tracks[*id];
                ScenePoint { pos: to3(world(*x)), error: *e, first: t.start as u32, last: (t.end().min(n) - 1) as u32 }
            })
            .collect();
        Some(CameraTrack { times, cameras, fov: fov_of(focal, frame[1] as f64) as f32, frame, points, error: self.error, tripod: self.tripod, placed: placed.len() as u32, camera: None })
    }
}

impl CameraTrack {
    /// The frame nearest media time `t`, if it's within the track (half a frame either side).
    pub fn frame_at(&self, t: Time) -> Option<usize> {
        let i = self.times.partition_point(|&x| x < t);
        let cands = [i.checked_sub(1), (i < self.times.len()).then_some(i)];
        let best = cands.into_iter().flatten().min_by_key(|&k| (self.times[k] - t).0.abs())?;
        let step = if self.times.len() > 1 { (self.times[1] - self.times[0]).0.abs() } else { i64::MAX };
        ((self.times[best] - t).0.abs() <= step / 2 + 1).then_some(best)
    }

    /// The camera at frame `i`.
    pub fn camera(&self, i: usize) -> Camera {
        let c = &self.cameras[i.min(self.cameras.len() - 1)];
        Camera { position: c.position, target: c.target, roll: c.roll, fov: self.fov, dof: None, frame: self.frame }
    }

    /// Where world point `p` is in frame `i` (frame pixels), if in front of the camera.
    pub fn project(&self, i: usize, p: [f32; 3]) -> Option<[f32; 2]> {
        self.camera(i).project(p)
    }

    /// The plane through points `picked` (indices into `points`): its
    /// centre and its unit normal, facing the camera of frame `i`. Needs
    /// three points not in a line; two give a plane facing the camera.
    pub fn plane(&self, picked: &[usize], i: usize) -> Option<([f32; 3], [f32; 3])> {
        let pts: Vec<V3> = picked.iter().filter_map(|&k| self.points.get(k)).map(|p| to64(p.pos)).collect();
        if pts.is_empty() {
            return None;
        }
        let c = scale(pts.iter().fold([0.0; 3], |a, p| add(a, *p)), 1.0 / pts.len() as f64);
        let cam = to64(self.cameras[i.min(self.cameras.len() - 1)].position);
        let to_cam = unit(sub(cam, c)).unwrap_or([0.0, 0.0, -1.0]);
        let mut n = to_cam;
        if pts.len() >= 3 {
            let mut cov = [0.0f64; 9];
            for p in &pts {
                let d = sub(*p, c);
                for a in 0..3 {
                    for b in 0..3 {
                        cov[a * 3 + b] += d[a] * d[b];
                    }
                }
            }
            let (vals, vecs) = sym_eigen(&cov, 3);
            // Flat enough to be a plane: the thinnest spread well below the next.
            if vals[1] > 1e-9 && vals[0] < vals[1] * 0.5 {
                n = [vecs[0], vecs[3], vecs[6]];
            }
        }
        if dot(n, to_cam) < 0.0 {
            n = scale(n, -1.0);
        }
        Some((to3(c), to3(n)))
    }

    /// The same shot turned and moved so the plane through `picked` is the
    /// floor (its normal pointing up, −Y) and its centre is the origin; or
    /// with only an origin when fewer than three points are picked.
    pub fn with_ground(&self, picked: &[usize], reference: usize) -> Option<CameraTrack> {
        let (c, n) = self.plane(picked, reference)?;
        let (c, n) = (to64(c), to64(n));
        let rot = if picked.len() >= 3 {
            // The smallest turn taking n to up (0, −1, 0).
            let up = [0.0, -1.0, 0.0];
            let axis = cross(n, up);
            let s = len(axis);
            let cth = dot(n, up).clamp(-1.0, 1.0);
            if s < 1e-9 {
                if cth > 0.0 {
                    I3
                } else {
                    exp_so3([std::f64::consts::PI, 0.0, 0.0])
                }
            } else {
                exp_so3(scale(axis, s.atan2(cth) / s))
            }
        } else {
            I3
        };
        let map = |p: [f32; 3]| to3(mv(&rot, sub(to64(p), c)));
        let mut out = self.clone();
        // Each camera moved and turned with the world (its roll is measured
        // from the world's down, so it's worked out again).
        for (k, cam) in out.cameras.iter_mut().enumerate() {
            let (r, d, f) = self.camera(k).basis();
            let rows = [mv(&rot, to64(r)), mv(&rot, to64(d)), mv(&rot, to64(f))];
            let dist = len(sub(to64(cam.target), to64(cam.position)));
            let pose = Pose { r: [rows[0], rows[1], rows[2]], c: to64(map(cam.position)) };
            *cam = cam_pose(&pose, dist);
        }
        for p in &mut out.points {
            p.pos = map(p.pos);
        }
        Some(out)
    }
}

/// Layer orientation (rotation about X, about Y, degrees) that turns a 3D
/// layer to face along `normal` (pointing out of its front, towards the viewer).
pub fn orientation_facing(normal: [f32; 3]) -> [f32; 2] {
    let n = unit(to64(normal)).unwrap_or([0.0, 0.0, -1.0]);
    // A layer's front faces −Z; after X then Y rotation it faces
    // (−cos a · sin b, sin a, −cos a · cos b) with b = −(Y rotation).
    let a = n[1].clamp(-1.0, 1.0).asin();
    let b = (-n[0]).atan2(-n[2]);
    [a.to_degrees() as f32, (-b).to_degrees() as f32]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn look_at(c: V3, target: V3) -> Pose {
        let f = unit(sub(target, c)).unwrap();
        let r = unit(cross([0.0, 1.0, 0.0], f)).unwrap();
        let d = cross(f, r);
        Pose { r: [r, d, f], c }
    }

    /// A synthetic shot: points on a floor and a back wall, a camera moving
    /// along a path, its tracks with a little noise (and some bad ones).
    fn shot(frames: usize, fov: f64, path: impl Fn(f64) -> (V3, V3), noise: f64, bad: usize) -> (Vec<Track2d>, Vec<Pose>, f64) {
        let size = [1280.0f64, 720.0];
        let f = focal_of(fov, size[1]);
        let mut rng = Rng(12345);
        let mut uni = move || (rng.next() >> 11) as f64 / (1u64 << 53) as f64;
        let mut pts = Vec::new();
        for _ in 0..300 {
            // Floor y = 200 (below), z 300..2500; wall z = 2500.
            if uni() < 0.6 {
                pts.push([uni() * 3000.0 - 1500.0, 200.0, 300.0 + uni() * 2200.0]);
            } else {
                pts.push([uni() * 3000.0 - 1500.0, 200.0 - uni() * 1500.0, 2500.0]);
            }
        }
        let poses: Vec<Pose> = (0..frames).map(|i| {
            let (c, t) = path(i as f64 / (frames - 1) as f64);
            look_at(c, t)
        }).collect();
        let mut tracks = Vec::new();
        for x in &pts {
            let mut cur: Option<Track2d> = None;
            for (i, p) in poses.iter().enumerate() {
                let v = p.view(*x);
                let q = (v[2] > 10.0).then(|| [size[0] / 2.0 + f * v[0] / v[2] + (uni() - 0.5) * 2.0 * noise, size[1] / 2.0 + f * v[1] / v[2] + (uni() - 0.5) * 2.0 * noise]);
                match q.filter(|q| q[0] > 5.0 && q[1] > 5.0 && q[0] < size[0] - 5.0 && q[1] < size[1] - 5.0) {
                    Some(q) => cur.get_or_insert_with(|| Track2d { start: i, pos: vec![] }).pos.push([q[0] as f32, q[1] as f32]),
                    None => {
                        if let Some(t) = cur.take() {
                            tracks.push(t);
                        }
                    }
                }
            }
            if let Some(t) = cur.take() {
                tracks.push(t);
            }
        }
        // Tracks that wander off on their own (something moving in the shot).
        for k in 0..bad {
            let start = [100.0 + (k * 37 % 1000) as f32, 100.0 + (k * 53 % 500) as f32];
            tracks.push(Track2d { start: 0, pos: (0..frames).map(|i| [start[0] + i as f32 * 2.5, start[1] - i as f32 * 1.0]).collect() });
        }
        (tracks, poses, f)
    }

    /// Compare a solution with the true cameras (up to the scene's scale):
    /// the largest angle (degrees) between their viewing directions, and
    /// the largest gap between their paths as a fraction of the path's length.
    fn compare(sol: &Solution, truth: &[Pose]) -> (f64, f64) {
        let first = (0..truth.len()).find(|&i| sol.poses[i].is_some()).unwrap();
        let (p0, t0) = (sol.poses[first].unwrap(), truth[first]);
        // Bring the solution into the truth's frame: rotate by t0ᵀ p0.
        let rot = mm(&tr(&t0.r), &p0.r);
        let path_len = len(sub(truth[truth.len() - 1].c, truth[0].c)).max(1e-9);
        let sol_len = len(sub(sol.poses[truth.len() - 1].unwrap().c, sol.poses[first].unwrap().c)).max(1e-9);
        let k = path_len / sol_len;
        let mut ang: f64 = 0.0;
        let mut gap: f64 = 0.0;
        for (i, t) in truth.iter().enumerate() {
            let Some(p) = sol.poses[i] else { continue };
            let r = mm(&p.r, &tr(&rot));
            let fa = r[2];
            ang = ang.max(dot(fa, t.r[2]).clamp(-1.0, 1.0).acos().to_degrees());
            let c = add(t0.c, scale(mv(&rot, sub(p.c, p0.c)), k));
            gap = gap.max(len(sub(c, t.c)) / path_len);
        }
        (ang, gap)
    }

    #[test]
    fn solves_a_moving_camera_and_its_lens() {
        // A dolly to the right, turning a little to keep looking at the scene.
        let (tracks, truth, f) = shot(60, 50.0, |s| ([-400.0 + 800.0 * s, -150.0, -200.0 + 150.0 * s], [100.0 * s, 0.0, 1500.0]), 0.3, 12);
        let stop = AtomicBool::new(false);
        let sol = solve(&tracks, 60, [1280.0, 720.0], SolveOptions::default(), &stop, &mut |_| {}).unwrap();
        assert!(!sol.tripod);
        assert!(sol.error < 1.0, "error {}", sol.error);
        let fov = fov_of(sol.focal, 720.0);
        assert!((fov - 50.0).abs() < 1.0, "fov {fov} (focal {} vs {f})", sol.focal);
        assert!(sol.poses.iter().all(|p| p.is_some()), "every frame placed");
        let (ang, gap) = compare(&sol, &truth);
        assert!(ang < 0.2 && gap < 0.01, "angle {ang}°, path {gap}");
        // The wandering tracks aren't points.
        let n = tracks.len();
        assert!(sol.points.iter().all(|(id, ..)| *id < n - 12), "bad tracks dropped");
        assert!(sol.points.len() > 100);
    }

    #[test]
    fn solves_with_a_known_lens_and_a_forward_move() {
        let (tracks, truth, _) = shot(50, 40.0, |s| ([50.0 * s, -120.0, -600.0 + 700.0 * s], [50.0 * s + 200.0 * s, -50.0, 2000.0]), 0.3, 0);
        let stop = AtomicBool::new(false);
        let sol = solve(&tracks, 50, [1280.0, 720.0], SolveOptions { fov: Some(40.0), tripod: false }, &stop, &mut |_| {}).unwrap();
        assert!(sol.error < 1.0, "{}", sol.error);
        assert!((fov_of(sol.focal, 720.0) - 40.0).abs() < 1e-3, "kept the lens");
        let (ang, gap) = compare(&sol, &truth);
        assert!(ang < 0.2 && gap < 0.01, "angle {ang}°, path {gap}");
    }

    #[test]
    fn a_camera_only_turning_is_a_tripod_shot() {
        let (tracks, truth, _) = shot(40, 45.0, |s| ([0.0, -100.0, -300.0], [-500.0 + 900.0 * s, -100.0 + 200.0 * s, 1500.0]), 0.2, 0);
        let stop = AtomicBool::new(false);
        let sol = solve(&tracks, 40, [1280.0, 720.0], SolveOptions::default(), &stop, &mut |_| {}).unwrap();
        assert!(sol.tripod);
        assert!(sol.error < 1.0, "{}", sol.error);
        let (ang, _) = compare(&sol, &truth);
        assert!(ang < 0.5, "{ang}");
        assert!((fov_of(sol.focal, 720.0) - 45.0).abs() < 3.0, "{}", fov_of(sol.focal, 720.0));
    }

    #[test]
    fn into_the_editors_world() {
        let (tracks, _, _) = shot(40, 50.0, |s| ([-300.0 + 600.0 * s, -150.0, -200.0], [0.0, 0.0, 1500.0]), 0.2, 0);
        let stop = AtomicBool::new(false);
        let sol = solve(&tracks, 40, [1280.0, 720.0], SolveOptions::default(), &stop, &mut |_| {}).unwrap();
        let times: Vec<Time> = (0..40).map(|i| Time::from_secs_f64(i as f64 / 25.0)).collect();
        let ct = sol.clone().into_track(&tracks, times.clone(), [1280.0, 720.0], 0).unwrap();
        // Frame 0's camera is the default one.
        let d = Camera::default_for([1280.0, 720.0]);
        let c0 = ct.camera(0);
        let fov_cam = Camera { fov: ct.fov, ..d };
        let c0d = Camera { position: [0.0, 0.0, -fov_cam.focal()], ..fov_cam };
        for k in 0..3 {
            assert!((c0.position[k] - c0d.position[k]).abs() < 1e-2, "{:?} {:?}", c0.position, c0d.position);
        }
        assert!(ct.cameras[0].roll.abs() < 1e-3);
        // Every point projects onto its track, in every frame (through the editor's camera).
        let mut worst = 0.0f32;
        for (k, (id, ..)) in sol.points.iter().enumerate() {
            let t = &tracks[*id];
            for f in t.start..t.end() {
                let q = ct.project(f, ct.points[k].pos).unwrap();
                let p = t.pos[f - t.start];
                worst = worst.max(((q[0] - p[0]).powi(2) + (q[1] - p[1]).powi(2)).sqrt());
            }
        }
        assert!(worst < 4.0, "worst {worst}");
        // Features at the middle depth sit near Z = 0 in the first frame.
        let mut z: Vec<f32> = ct.points.iter().filter(|p| p.first == 0).map(|p| p.pos[2]).collect();
        z.sort_by(f32::total_cmp);
        assert!(z[z.len() / 2].abs() < 1.0, "{}", z[z.len() / 2]);
        assert_eq!(ct.frame_at(times[7]), Some(7));
        assert_eq!(ct.frame_at(Time::from_secs_f64(10.0)), None);
        // Turn so three floor-ish points (the lowest in frame 0) are the ground.
        let mut low: Vec<usize> = (0..ct.points.len()).filter(|&k| ct.points[k].first == 0).collect();
        low.sort_by(|&a, &b| ct.project(0, ct.points[b].pos).unwrap()[1].total_cmp(&ct.project(0, ct.points[a].pos).unwrap()[1]));
        let picked: Vec<usize> = low.into_iter().take(12).collect();
        let (_, n) = ct.plane(&picked, 0).unwrap();
        assert!(n[1] < -0.8, "floor faces up towards the camera: {n:?}");
        let g = ct.with_ground(&picked, 0).unwrap();
        let (c, n2) = g.plane(&picked, 0).unwrap();
        assert!(n2[1] < -0.999 && c.iter().all(|v| v.abs() < 1e-2), "{c:?} {n2:?}");
        // Same pictures from the turned world.
        for k in [0usize, 5, 20] {
            for f in [0usize, 10, 39] {
                if let (Some(a), Some(b)) = (ct.project(f, ct.points[k].pos), g.project(f, g.points[k].pos)) {
                    assert!((a[0] - b[0]).abs() < 0.05 && (a[1] - b[1]).abs() < 0.05, "{a:?} {b:?}");
                }
            }
        }
    }

    #[test]
    fn layer_orientation_faces_the_normal() {
        use crate::transform::Transform;
        for n in [[0.0f32, 0.0, -1.0], [0.0, -1.0, 0.0], [0.6, -0.48, -0.64], [-0.3, 0.2, 0.93]] {
            let l = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            let n = [n[0] / l, n[1] / l, n[2] / l];
            let o = orientation_facing(n);
            let t = Transform { three_d: true, orientation: o, ..Transform::default() };
            let scene = crate::space3d::Scene3d { camera: Camera::default_for([1920.0, 1080.0]), lights: vec![] };
            let ls = crate::space3d::layer_space(&t, [100.0, 100.0], &scene);
            let [_, u, v] = ls.plane;
            let front = cross(to64(v), to64(u));
            let front = unit(front).unwrap();
            assert!(dot(front, to64(n)) > 0.999, "{n:?} → {o:?}: {front:?}");
        }
    }

    #[test]
    fn linear_algebra() {
        let r = exp_so3([0.3, -0.2, 0.5]);
        let (u, s, v) = svd3(&r);
        assert!(s.iter().all(|x| (x - 1.0).abs() < 1e-9));
        let back = mm(&mm(&u, &[[s[0], 0.0, 0.0], [0.0, s[1], 0.0], [0.0, 0.0, s[2]]]), &tr(&v));
        for i in 0..3 {
            for j in 0..3 {
                assert!((back[i][j] - r[i][j]).abs() < 1e-9);
            }
        }
        let m = [[2.0, 1.0, 0.0], [0.5, 3.0, 1.0], [0.0, 0.2, 1.0]];
        let (u, s, v) = svd3(&m);
        let back = mm(&mm(&u, &[[s[0], 0.0, 0.0], [0.0, s[1], 0.0], [0.0, 0.0, s[2]]]), &tr(&v));
        for i in 0..3 {
            for j in 0..3 {
                assert!((back[i][j] - m[i][j]).abs() < 1e-9, "{back:?}");
            }
        }
        let mut a = vec![4.0, 1.0, 0.5, 1.0, 3.0, 0.2, 0.5, 0.2, 2.0];
        let mut b = vec![1.0, 2.0, 3.0];
        assert!(cholesky_solve(&mut a, 3, &mut b));
        let a0 = [4.0, 1.0, 0.5, 1.0, 3.0, 0.2, 0.5, 0.2, 2.0];
        for i in 0..3 {
            let s: f64 = (0..3).map(|j| a0[i * 3 + j] * b[j]).sum();
            assert!((s - [1.0, 2.0, 3.0][i]).abs() < 1e-9);
        }
    }

    /// Rendered footage of a textured floor and wall, followed and solved.
    #[test]
    fn follows_and_solves_rendered_footage() {
        use crate::track::Pyramid;
        let (w, h) = (640usize, 360usize);
        let fov = 50.0;
        let f = focal_of(fov, h as f64);
        // Value noise, a few octaves, in world units.
        let hash = |x: i64, y: i64| -> f64 {
            let mut v = (x as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (y as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
            v ^= v >> 29;
            v = v.wrapping_mul(0xBF58_476D_1CE4_E5B9);
            v ^= v >> 32;
            (v >> 11) as f64 / (1u64 << 53) as f64
        };
        let noise = |x: f64, y: f64| -> f64 {
            let mut s = 0.0;
            let mut amp = 1.0;
            let mut k = 1.0 / 40.0;
            for _ in 0..4 {
                let (fx, fy) = (x * k, y * k);
                let (ix, iy) = (fx.floor(), fy.floor());
                let (tx, ty) = (fx - ix, fy - iy);
                let (sx, sy) = (tx * tx * (3.0 - 2.0 * tx), ty * ty * (3.0 - 2.0 * ty));
                let (ix, iy) = (ix as i64, iy as i64);
                let a = hash(ix, iy) * (1.0 - sx) + hash(ix + 1, iy) * sx;
                let b = hash(ix, iy + 1) * (1.0 - sx) + hash(ix + 1, iy + 1) * sx;
                s += amp * (a * (1.0 - sy) + b * sy);
                amp *= 0.5;
                k *= 2.0;
            }
            s / 1.875
        };
        let frames = 36;
        let path = |s: f64| ([-250.0 + 500.0 * s, -150.0, -100.0 + 100.0 * s], [60.0 * s, 0.0, 1500.0]);
        let truth: Vec<Pose> = (0..frames).map(|i| {
            let (c, t) = path(i as f64 / (frames - 1) as f64);
            look_at(c, t)
        }).collect();
        let mut fol = SceneFollower::new(w, h, 1.0);
        for p in &truth {
            let rt = tr(&p.r);
            let mut img = vec![0.0f32; w * h];
            // Four samples a pixel, as a camera's sensor averages over it.
            let shade = |sx: f64, sy: f64| -> f64 {
                let d = mv(&rt, [(sx - w as f64 / 2.0) / f, (sy - h as f64 / 2.0) / f, 1.0]);
                // Floor y = 200, wall z = 2500: the nearer hit.
                let mut best = f64::INFINITY;
                let mut v = 0.5;
                if d[1] > 1e-9 {
                    let t = (200.0 - p.c[1]) / d[1];
                    let q = add(p.c, scale(d, t));
                    if t > 0.0 && q[2] < 2500.0 {
                        best = t;
                        v = noise(q[0], q[2]);
                    }
                }
                if d[2] > 1e-9 {
                    let t = (2500.0 - p.c[2]) / d[2];
                    if t > 0.0 && t < best {
                        let q = add(p.c, scale(d, t));
                        v = noise(q[0] + 5000.0, q[1]);
                    }
                }
                v
            };
            for y in 0..h {
                for x in 0..w {
                    let (fx, fy) = (x as f64, y as f64);
                    let v = (shade(fx + 0.25, fy + 0.25) + shade(fx + 0.75, fy + 0.25) + shade(fx + 0.25, fy + 0.75) + shade(fx + 0.75, fy + 0.75)) * 0.25;
                    img[y * w + x] = (v * 255.0) as f32;
                }
            }
            let pyr = Pyramid::new(img, w, h, fol.levels());
            fol.add_frame(&pyr);
        }
        let tracks = fol.tracks();
        assert!(tracks.len() > 100, "{} tracks", tracks.len());
        let stop = AtomicBool::new(false);
        let sol = solve(&tracks, frames, [w as f32, h as f32], SolveOptions::default(), &stop, &mut |_| {}).unwrap();
        let (ang, gap) = compare(&sol, &truth);
        let got = fov_of(sol.focal, h as f64);
        // A short sideways move can't tell nearby lenses apart: the true
        // lens fits these tracks no better than the one found. So the lens
        // and the camera come out close, and given the lens, the camera is exact.
        assert!(sol.error < 0.5, "error {}", sol.error);
        assert!((got - fov).abs() < 3.0, "fov {got}");
        assert!(ang < 1.0 && gap < 0.025, "angle {ang}°, path {gap}");
        let fixed = solve(&tracks, frames, [w as f32, h as f32], SolveOptions { fov: Some(fov as f32), tripod: false }, &stop, &mut |_| {}).unwrap();
        let (ang, gap) = compare(&fixed, &truth);
        assert!(fixed.error < 0.5 && ang < 0.2 && gap < 0.01, "known lens: error {}, angle {ang}°, path {gap}", fixed.error);
    }
}
