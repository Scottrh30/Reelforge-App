//! Mesh warps: a layer stretched over a grid of points that can bend, for
//! pinning a picture onto something that isn't flat or doesn't stay flat
//! (cloth, skin, a flag, paper). The grid usually comes from a mesh tracker
//! (see [`crate::meshtrack`]).
//!
//! A grid of `cols` × `rows` cells has `(cols + 1) × (rows + 1)` points, row by
//! row from the top left. The layer's picture is spread evenly over the cells
//! and stretched bilinearly inside each one.

use crate::time::Time;
use serde::{Deserialize, Serialize};

/// The most cells across or down a mesh can have.
pub const MAX_CELLS: u8 = 32;

/// A grid of points (frame pixels), row by row from the top left.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MeshShape {
    pub cols: u8,
    pub rows: u8,
    pub points: Vec<[f32; 2]>,
}

impl MeshShape {
    /// A grid filling the four-sided `quad` (top left, top right, bottom right,
    /// bottom left) evenly.
    pub fn filling(quad: [[f32; 2]; 4], cols: u8, rows: u8) -> Self {
        let (cols, rows) = (cols.clamp(1, MAX_CELLS), rows.clamp(1, MAX_CELLS));
        let mut points = Vec::with_capacity((cols as usize + 1) * (rows as usize + 1));
        for j in 0..=rows {
            for i in 0..=cols {
                points.push(bilinear(&quad, i as f32 / cols as f32, j as f32 / rows as f32));
            }
        }
        MeshShape { cols, rows, points }
    }

    pub fn is_valid(&self) -> bool {
        (1..=MAX_CELLS).contains(&self.cols)
            && (1..=MAX_CELLS).contains(&self.rows)
            && self.points.len() == (self.cols as usize + 1) * (self.rows as usize + 1)
            && self.points.iter().flatten().all(|v| v.is_finite())
    }

    pub fn index(&self, i: usize, j: usize) -> usize {
        j * (self.cols as usize + 1) + i
    }

    /// The four corners of cell (i, j): top left, top right, bottom right, bottom left.
    pub fn cell(&self, i: usize, j: usize) -> [[f32; 2]; 4] {
        [self.points[self.index(i, j)], self.points[self.index(i + 1, j)], self.points[self.index(i + 1, j + 1)], self.points[self.index(i, j + 1)]]
    }

    /// The four outer corners.
    pub fn corners(&self) -> [[f32; 2]; 4] {
        let (c, r) = (self.cols as usize, self.rows as usize);
        [self.points[0], self.points[self.index(c, 0)], self.points[self.index(c, r)], self.points[self.index(0, r)]]
    }

    /// Where mesh coordinates `m` (see [`MeshCoord`]) are on this mesh.
    pub fn apply(&self, m: MeshCoord) -> [f32; 2] {
        bilinear(&self.cell(m.cell[0] as usize, m.cell[1] as usize), m.f[0], m.f[1])
    }

    /// The mesh coordinates of frame point `p`: the cell it's in and where in
    /// it. Points outside the mesh get the nearest edge cell, extended.
    pub fn locate(&self, p: [f32; 2]) -> Option<MeshCoord> {
        let mut best: Option<(f32, MeshCoord)> = None;
        for j in 0..self.rows as usize {
            for i in 0..self.cols as usize {
                let Some(f) = inverse_bilinear(&self.cell(i, j), p) else { continue };
                // How far outside the cell (0 inside).
                let out = (-f[0]).max(f[0] - 1.0).max(0.0) + (-f[1]).max(f[1] - 1.0).max(0.0);
                if best.as_ref().is_none_or(|b| out < b.0) {
                    best = Some((out, MeshCoord { cell: [i as u8, j as u8], f }));
                }
                if out == 0.0 {
                    return best.map(|b| b.1);
                }
            }
        }
        best.map(|b| b.1)
    }

    /// Each point moved by `f`.
    pub fn map(&self, f: impl Fn([f32; 2]) -> [f32; 2]) -> MeshShape {
        MeshShape { cols: self.cols, rows: self.rows, points: self.points.iter().map(|&p| f(p)).collect() }
    }

    /// `k` of the way from `self` to `other` (same grid), else `self`.
    pub fn lerp(&self, other: &MeshShape, k: f32) -> MeshShape {
        if other.cols != self.cols || other.rows != self.rows || other.points.len() != self.points.len() {
            return self.clone();
        }
        MeshShape { cols: self.cols, rows: self.rows, points: self.points.iter().zip(&other.points).map(|(a, b)| [a[0] + (b[0] - a[0]) * k, a[1] + (b[1] - a[1]) * k]).collect() }
    }

    /// Does any cell fold over (turn inside out)? Such a mesh can't be drawn sensibly.
    pub fn is_folded(&self) -> bool {
        let area = |q: &[[f32; 2]; 4]| -> f32 { 0.5 * (0..4).map(|i| q[i][0] * q[(i + 1) % 4][1] - q[(i + 1) % 4][0] * q[i][1]).sum::<f32>() };
        let mut sign = 0.0f32;
        for j in 0..self.rows as usize {
            for i in 0..self.cols as usize {
                let a = area(&self.cell(i, j));
                if a.abs() < 1e-3 {
                    return true;
                }
                if sign == 0.0 {
                    sign = a.signum();
                } else if a.signum() != sign {
                    return true;
                }
            }
        }
        false
    }
}

/// A place on a mesh: a cell and the position inside it (0–1 each way; a
/// little outside for points beyond the mesh's edge).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MeshCoord {
    pub cell: [u8; 2],
    pub f: [f32; 2],
}

/// The point `u` across and `v` down the four-sided `q` (top left, top right,
/// bottom right, bottom left), stretched bilinearly.
pub fn bilinear(q: &[[f32; 2]; 4], u: f32, v: f32) -> [f32; 2] {
    let top = [q[0][0] + (q[1][0] - q[0][0]) * u, q[0][1] + (q[1][1] - q[0][1]) * u];
    let bot = [q[3][0] + (q[2][0] - q[3][0]) * u, q[3][1] + (q[2][1] - q[3][1]) * u];
    [top[0] + (bot[0] - top[0]) * v, top[1] + (bot[1] - top[1]) * v]
}

/// The (u, v) with `bilinear(q, u, v) == p` (Newton's method from the middle).
pub fn inverse_bilinear(q: &[[f32; 2]; 4], p: [f32; 2]) -> Option<[f32; 2]> {
    let (mut u, mut v) = (0.5f32, 0.5f32);
    for _ in 0..12 {
        let b = bilinear(q, u, v);
        let (ex, ey) = (b[0] - p[0], b[1] - p[1]);
        // ∂/∂u and ∂/∂v.
        let du = [(q[1][0] - q[0][0]) * (1.0 - v) + (q[2][0] - q[3][0]) * v, (q[1][1] - q[0][1]) * (1.0 - v) + (q[2][1] - q[3][1]) * v];
        let dv = [(q[3][0] - q[0][0]) * (1.0 - u) + (q[2][0] - q[1][0]) * u, (q[3][1] - q[0][1]) * (1.0 - u) + (q[2][1] - q[1][1]) * u];
        let det = du[0] * dv[1] - du[1] * dv[0];
        if det.abs() < 1e-9 {
            return None;
        }
        let su = (ex * dv[1] - ey * dv[0]) / det;
        let sv = (du[0] * ey - du[1] * ex) / det;
        u -= su;
        v -= sv;
        if su.abs() < 1e-6 && sv.abs() < 1e-6 {
            break;
        }
    }
    (u.is_finite() && v.is_finite()).then_some([u, v])
}

/// A clip's mesh warp: the grid at moments of its source time, in between
/// moved evenly from one to the next. Before the first and after the last it
/// holds still.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MeshWarp {
    /// Sorted by time.
    pub keys: Vec<(Time, MeshShape)>,
}

impl MeshWarp {
    /// The grid at source time `t`.
    pub fn at(&self, t: Time) -> Option<MeshShape> {
        let k = &self.keys;
        let first = k.first()?;
        if t <= first.0 {
            return Some(first.1.clone());
        }
        let last = k.last()?;
        if t >= last.0 {
            return Some(last.1.clone());
        }
        let i = k.partition_point(|(at, _)| *at <= t);
        let (a, b) = (&k[i - 1], &k[i]);
        let span = (b.0 - a.0).0.max(1) as f64;
        Some(a.1.lerp(&b.1, ((t - a.0).0 as f64 / span) as f32))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bilinear_round_trip_and_locating_points() {
        let q = [[10.0, 10.0], [110.0, 20.0], [100.0, 90.0], [5.0, 70.0]];
        for (u, v) in [(0.0, 0.0), (0.3, 0.7), (1.0, 1.0), (0.5, 0.2)] {
            let p = bilinear(&q, u, v);
            let f = inverse_bilinear(&q, p).unwrap();
            assert!((f[0] - u).abs() < 1e-4 && (f[1] - v).abs() < 1e-4, "{f:?}");
        }
        let m = MeshShape::filling(q, 4, 3);
        assert_eq!(m.points.len(), 20);
        assert_eq!(m.corners(), q);
        let p = bilinear(&q, 0.62, 0.4);
        let c = m.locate(p).unwrap();
        assert_eq!(c.cell, [2, 1]);
        let back = m.apply(c);
        assert!((back[0] - p[0]).abs() < 1e-3 && (back[1] - p[1]).abs() < 1e-3);
        assert!(!m.is_folded());
        // Turning one cell inside out folds it.
        let mut bad = m.clone();
        let i = bad.index(1, 1);
        bad.points[i] = bad.points[bad.index(2, 2)];
        bad.points[i][0] += 20.0;
        assert!(bad.is_folded());
    }

    #[test]
    fn keys_interpolate_and_hold() {
        let a = MeshShape::filling([[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]], 2, 2);
        let b = a.map(|p| [p[0] + 4.0, p[1]]);
        let s = Time::from_secs_f64;
        let w = MeshWarp { keys: vec![(s(1.0), a.clone()), (s(2.0), b.clone())] };
        assert_eq!(w.at(s(0.0)).unwrap(), a);
        assert_eq!(w.at(s(3.0)).unwrap(), b);
        assert!((w.at(s(1.5)).unwrap().points[0][0] - 2.0).abs() < 1e-4);
    }
}
