//! Automatic stabilising: the camera's shake found from the details followed
//! through the shot (see `camtrack::Track2d`, from the scene follower), with no
//! track to set up. Between each frame and the next, the details that carry
//! on are fitted with one movement — a shift, turn and zoom (a similarity),
//! robustly, so things moving on their own are outvoted — and the movements
//! chained into the camera's path. That path is smoothed (or held at a frame),
//! and each frame is mapped from where the shaky camera put it to where the
//! steady one would have.

use crate::camtrack::{StabPath, Track2d};
use crate::transform::Homography;

/// A shift, turn and zoom: `p → [a −b; b a] p + t`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sim {
    pub a: f64,
    pub b: f64,
    pub tx: f64,
    pub ty: f64,
}

impl Sim {
    pub const IDENTITY: Sim = Sim { a: 1.0, b: 0.0, tx: 0.0, ty: 0.0 };

    pub fn apply(&self, p: [f64; 2]) -> [f64; 2] {
        [self.a * p[0] - self.b * p[1] + self.tx, self.b * p[0] + self.a * p[1] + self.ty]
    }

    /// `self` after `first`.
    pub fn after(&self, first: &Sim) -> Sim {
        let t = self.apply([first.tx, first.ty]);
        Sim { a: self.a * first.a - self.b * first.b, b: self.b * first.a + self.a * first.b, tx: t[0], ty: t[1] }
    }

    pub fn inverse(&self) -> Option<Sim> {
        let d = self.a * self.a + self.b * self.b;
        if d <= 1e-12 || !d.is_finite() {
            return None;
        }
        let (a, b) = (self.a / d, -self.b / d);
        let i = Sim { a, b, tx: 0.0, ty: 0.0 };
        let t = i.apply([self.tx, self.ty]);
        Some(Sim { a, b, tx: -t[0], ty: -t[1] })
    }

    /// As shift (of the point `c`), turn (radians) and zoom (its log), about `c`.
    fn params(&self, c: [f64; 2]) -> [f64; 4] {
        let m = self.apply(c);
        [m[0] - c[0], m[1] - c[1], self.b.atan2(self.a), (self.a * self.a + self.b * self.b).sqrt().max(1e-9).ln()]
    }

    fn from_params(p: [f64; 4], c: [f64; 2]) -> Sim {
        let s = p[3].exp();
        let (a, b) = (s * p[2].cos(), s * p[2].sin());
        // c → c + shift.
        Sim { a, b, tx: c[0] + p[0] - (a * c[0] - b * c[1]), ty: c[1] + p[1] - (b * c[0] + a * c[1]) }
    }

    pub fn homography(&self) -> Homography {
        Homography([self.a, -self.b, self.tx, self.b, self.a, self.ty, 0.0, 0.0, 1.0])
    }
}

/// The similarity best fitting `pairs` (from → to), by least squares.
pub fn fit(pairs: &[([f64; 2], [f64; 2])]) -> Option<Sim> {
    let n = pairs.len();
    if n < 2 {
        return None;
    }
    let k = 1.0 / n as f64;
    let (mut mp, mut mq) = ([0.0; 2], [0.0; 2]);
    for (p, q) in pairs {
        mp = [mp[0] + p[0] * k, mp[1] + p[1] * k];
        mq = [mq[0] + q[0] * k, mq[1] + q[1] * k];
    }
    let (mut sxx, mut sab, mut sba) = (0.0, 0.0, 0.0);
    for (p, q) in pairs {
        let (u, v) = ([p[0] - mp[0], p[1] - mp[1]], [q[0] - mq[0], q[1] - mq[1]]);
        sxx += u[0] * u[0] + u[1] * u[1];
        sab += u[0] * v[0] + u[1] * v[1];
        sba += u[0] * v[1] - u[1] * v[0];
    }
    if sxx < 1e-9 {
        return None;
    }
    let (a, b) = (sab / sxx, sba / sxx);
    let r = Sim { a, b, tx: 0.0, ty: 0.0 }.apply(mp);
    let s = Sim { a, b, tx: mq[0] - r[0], ty: mq[1] - r[1] };
    (s.a.is_finite() && s.b.is_finite() && s.tx.is_finite() && s.ty.is_finite()).then_some(s)
}

/// [`fit`], robustly: the movement most of the pairs agree on (within `tol`
/// pixels), from tries with two pairs at a time, then fitted to those that agree.
pub fn fit_robust(pairs: &[([f64; 2], [f64; 2])], tol: f64) -> Option<Sim> {
    let n = pairs.len();
    if n < 3 {
        return fit(pairs);
    }
    let mut rng: u64 = 0x9E37_79B9_7F4A_7C15 ^ n as u64;
    let mut next = |m: usize| {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        (rng % m.max(1) as u64) as usize
    };
    fn agree<'a>(pairs: &'a [([f64; 2], [f64; 2])], s: Sim, tol: f64) -> impl Iterator<Item = &'a ([f64; 2], [f64; 2])> + 'a {
        pairs.iter().filter(move |(p, q)| {
            let r = s.apply(*p);
            (r[0] - q[0]).hypot(r[1] - q[1]) <= tol
        })
    }
    let mut best: Option<(usize, Sim)> = None;
    for _ in 0..(n * 4).clamp(30, 200) {
        let (i, j) = (next(n), next(n));
        let (Some(a), Some(b)) = (pairs.get(i), pairs.get(j)) else { continue };
        if i == j {
            continue;
        }
        let Some(s) = fit(&[*a, *b]) else { continue };
        let k = agree(pairs, s, tol).count();
        if best.as_ref().is_none_or(|(bk, _)| k > *bk) {
            best = Some((k, s));
        }
    }
    let (k, s) = best?;
    if k < 3 {
        return fit(pairs);
    }
    let inliers: Vec<([f64; 2], [f64; 2])> = agree(pairs, s, tol).copied().collect();
    fit(&inliers).or(Some(s))
}

/// The camera's path through `n` frames: for each, the movement taking frame 0's
/// picture to that frame's (a frame without enough details to go on is taken as
/// not having moved from the one before).
pub fn camera_path(tracks: &[Track2d], n: usize) -> Vec<Sim> {
    let mut path = Vec::with_capacity(n);
    let mut now = Sim::IDENTITY;
    path.push(now);
    for i in 0..n.saturating_sub(1) {
        let pairs: Vec<([f64; 2], [f64; 2])> = tracks
            .iter()
            .filter_map(|t| {
                let k = i.checked_sub(t.start)?;
                let (a, b) = (t.pos.get(k)?, t.pos.get(k + 1)?);
                Some(([a[0] as f64, a[1] as f64], [b[0] as f64, b[1] as f64]))
            })
            .collect();
        let step = fit_robust(&pairs, 1.5).filter(|s| {
            // A wild jump (a cut, or a bad fit) is taken as no movement.
            let z = (s.a * s.a + s.b * s.b).sqrt();
            (0.8..1.25).contains(&z) && s.b.atan2(s.a).abs() < 0.35
        });
        now = step.unwrap_or(Sim::IDENTITY).after(&now);
        path.push(now);
    }
    path
}

/// For each frame, the map from where the shaky camera put the picture to where
/// the steady camera of `path` would have (about the frame's middle `centre`).
pub fn stabilising_maps(cam: &[Sim], path: StabPath, centre: [f64; 2]) -> Vec<Homography> {
    let n = cam.len();
    let params: Vec<[f64; 4]> = cam.iter().map(|s| s.params(centre)).collect();
    // (The turn unwrapped, so smoothing doesn't cross ±180°.)
    let mut params = params;
    for i in 1..n {
        let (Some(prev), Some(cur)) = (params.get(i - 1).map(|p| p[2]), params.get(i).map(|p| p[2])) else { continue };
        let mut a = cur;
        while a - prev > std::f64::consts::PI {
            a -= std::f64::consts::TAU;
        }
        while prev - a > std::f64::consts::PI {
            a += std::f64::consts::TAU;
        }
        if let Some(p) = params.get_mut(i) {
            p[2] = a;
        }
    }
    let steady: Vec<[f64; 4]> = match path {
        StabPath::Lock(k) => {
            let held = params.get(k.min(n.saturating_sub(1))).copied().unwrap_or([0.0; 4]);
            vec![held; n]
        }
        StabPath::Smooth(frames) => {
            let sigma = (frames as f64).clamp(0.5, 10_000.0);
            let reach = (sigma * 3.0).ceil() as i64;
            (0..n as i64)
                .map(|i| {
                    let (mut sum, mut wsum) = ([0.0; 4], 0.0);
                    for j in (i - reach)..=(i + reach) {
                        // (Past the ends, the end frames, so the path doesn't bend there.)
                        let Some(p) = params.get(j.clamp(0, n as i64 - 1) as usize) else { continue };
                        let w = (-((j - i) as f64).powi(2) / (2.0 * sigma * sigma)).exp();
                        for k in 0..4 {
                            sum[k] += p[k] * w;
                        }
                        wsum += w;
                    }
                    sum.map(|v| v / wsum.max(1e-12))
                })
                .collect()
        }
    };
    cam.iter()
        .zip(&steady)
        .map(|(real, s)| match real.inverse() {
            Some(inv) => Sim::from_params(*s, centre).after(&inv).homography(),
            None => Homography::IDENTITY,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shaky(n: usize) -> (Vec<Sim>, Vec<Track2d>) {
        // A camera panning steadily right with a jitter, turning and zooming a little.
        let cam: Vec<Sim> = (0..n)
            .map(|i| {
                let f = i as f64;
                let jitter = ((f * 1.7).sin() * 6.0, (f * 2.3).cos() * 5.0);
                let p = [-(f * 2.0) + jitter.0, jitter.1, (f * 0.9).sin() * 0.01, (f * 1.3).sin() * 0.005];
                Sim::from_params(p, [320.0, 180.0])
            })
            .collect();
        // Details on a grid, seen moving with the camera (and one moving on its own).
        let mut tracks: Vec<Track2d> = Vec::new();
        for gy in 0..6 {
            for gx in 0..8 {
                let p = [40.0 + gx as f64 * 80.0, 30.0 + gy as f64 * 60.0];
                tracks.push(Track2d { start: 0, pos: cam.iter().map(|c| c.apply(p)).map(|q| [q[0] as f32, q[1] as f32]).collect() });
            }
        }
        tracks.push(Track2d { start: 0, pos: (0..n).map(|i| [100.0 + i as f32 * 15.0, 200.0]).collect() });
        (cam, tracks)
    }

    #[test]
    fn finds_the_camera_and_takes_the_shake_out() {
        let n = 60;
        let (cam, tracks) = shaky(n);
        let found = camera_path(&tracks, n);
        // (Relative to frame 0's picture.)
        let first = cam[0].inverse().unwrap();
        for (a, b) in found.iter().zip(&cam) {
            let (p, q) = (a.apply([320.0, 180.0]), b.after(&first).apply([320.0, 180.0]));
            assert!((p[0] - q[0]).abs() < 0.05 && (p[1] - q[1]).abs() < 0.05, "{p:?} vs {q:?}");
        }
        // Held still: every frame's picture goes back where frame 10's was.
        let held = stabilising_maps(&found, StabPath::Lock(10), [320.0, 180.0]);
        let corner = [40.0, 30.0];
        let at10 = cam[10].apply(corner);
        for (i, h) in held.iter().enumerate() {
            let seen = cam[i].apply(corner);
            let fixed = h.apply([seen[0] as f32, seen[1] as f32]);
            assert!((fixed[0] as f64 - at10[0]).abs() < 0.1 && (fixed[1] as f64 - at10[1]).abs() < 0.1, "frame {i}: {fixed:?}");
        }
        // Smoothed: the pan stays, the jitter goes (the picture's movement from frame
        // to frame is near the steady 2 px).
        let smooth = stabilising_maps(&found, StabPath::Smooth(8.0), [320.0, 180.0]);
        let at = |i: usize| {
            let s = cam[i].apply(corner);
            smooth[i].apply([s[0] as f32, s[1] as f32])
        };
        for i in 20..40 {
            let (a, b) = (at(i), at(i + 1));
            assert!((b[0] - a[0] + 2.0).abs() < 1.0 && (b[1] - a[1]).abs() < 1.0, "frame {i}: moved {:?}", [b[0] - a[0], b[1] - a[1]]);
        }
    }

    #[test]
    fn fits_are_robust_and_odd_input_does_no_harm() {
        let s = Sim::from_params([5.0, -3.0, 0.1, 0.05], [0.0, 0.0]);
        let mut pairs: Vec<([f64; 2], [f64; 2])> = (0..20).map(|i| [i as f64 * 7.0, (i * i % 13) as f64 * 5.0]).map(|p| (p, s.apply(p))).collect();
        pairs.push(([0.0, 0.0], [500.0, 500.0]));
        pairs.push(([10.0, 0.0], [-300.0, 40.0]));
        let f = fit_robust(&pairs, 1.0).unwrap();
        assert!((f.a - s.a).abs() < 1e-6 && (f.tx - s.tx).abs() < 1e-4);
        assert!(fit(&[]).is_none() && fit(&[([1.0, 1.0], [2.0, 2.0])]).is_none());
        assert_eq!(camera_path(&[], 3), vec![Sim::IDENTITY; 3]);
        assert!(stabilising_maps(&[], StabPath::Smooth(10.0), [0.0, 0.0]).is_empty());
        assert_eq!(stabilising_maps(&[Sim::IDENTITY], StabPath::Lock(99), [1.0, 1.0]).len(), 1);
        let nan = Sim { a: f64::NAN, b: 0.0, tx: 0.0, ty: 0.0 };
        assert!(nan.inverse().is_none());
    }
}
