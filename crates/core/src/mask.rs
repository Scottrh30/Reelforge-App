//! Masks: shapes that cut a layer down to part of its picture.
//!
//! A clip can have several masks, combined in order: each one adds to,
//! subtracts from, or intersects with what the ones before it let through.
//! Each mask is a closed shape in the layer's own pixels (so it moves with the
//! layer), softened by a feather, grown or shrunk by an expansion, and can be
//! inverted. Its shape can be animated with keyframes.
//!
//! Masks are turned into an alpha image here, on the CPU (`rasterize`); the
//! renderer multiplies the layer by it before the layer's effects.

use crate::time::Time;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MaskShape {
    /// Four corners (drag them into any four-sided shape).
    #[default]
    Rectangle,
    /// The ellipse inside four corners.
    Ellipse,
    /// Any number of points, joined by straight lines or (with handles) curves.
    Freehand,
}

impl MaskShape {
    pub fn label(self) -> &'static str {
        match self {
            MaskShape::Rectangle => "Rectangle",
            MaskShape::Ellipse => "Ellipse",
            MaskShape::Freehand => "Freehand",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MaskMode {
    /// Show what's inside (added to the masks before it).
    #[default]
    Add,
    /// Cut what's inside out of the masks before it.
    Subtract,
    /// Keep only where this and the masks before it overlap.
    Intersect,
}

impl MaskMode {
    pub const ALL: [MaskMode; 3] = [MaskMode::Add, MaskMode::Subtract, MaskMode::Intersect];
    pub fn label(self) -> &'static str {
        match self {
            MaskMode::Add => "Add",
            MaskMode::Subtract => "Subtract",
            MaskMode::Intersect => "Intersect",
        }
    }
}

/// The shape at one moment (source time of the clip's media).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ShapeKey {
    pub at: Time,
    pub points: Vec<[f32; 2]>,
    /// Bézier handles of each point at this moment (see [`Mask::curves`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub curves: Vec<[f32; 4]>,
}

impl ShapeKey {
    pub fn new(at: Time, points: Vec<[f32; 2]>) -> Self {
        Self { at, points, curves: Vec::new() }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Mask {
    pub name: String,
    pub shape: MaskShape,
    /// The shape's points in layer pixels (top left = 0, 0), used when it isn't animated.
    pub points: Vec<[f32; 2]>,
    /// Shape keyframes; when there are any they decide the shape.
    pub keys: Vec<ShapeKey>,
    pub mode: MaskMode,
    pub inverted: bool,
    /// Width of the soft edge, in layer pixels.
    pub feather: f32,
    /// Grow (+) or shrink (−) the shape, in layer pixels.
    pub expansion: f32,
    /// 0–1.
    pub opacity: f32,
    pub enabled: bool,
    /// Bézier handles of each point, used when the shape isn't animated:
    /// `[in x, in y, out x, out y]`, offsets in layer pixels from the point.
    /// The side from a point to the next is a curve shaped by that point's
    /// out-handle and the next one's in-handle; a side whose two handles are
    /// both zero (or missing) is straight. Not used by ellipses.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub curves: Vec<[f32; 4]>,
    /// The feather at each point, in layer pixels (it blends along the sides
    /// between them); a negative value, or a missing one, uses `feather`.
    /// Not used by ellipses.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub feathers: Vec<f32>,
}

impl Default for Mask {
    fn default() -> Self {
        Self {
            name: "Mask".into(),
            shape: MaskShape::Rectangle,
            points: Vec::new(),
            keys: Vec::new(),
            mode: MaskMode::Add,
            inverted: false,
            feather: 0.0,
            expansion: 0.0,
            opacity: 1.0,
            enabled: true,
            curves: Vec::new(),
            feathers: Vec::new(),
        }
    }
}

/// Points per ellipse when it's turned into a polygon.
const ELLIPSE_SEGMENTS: usize = 96;

impl Mask {
    /// A rectangle or ellipse filling the middle half of a `layer`-sized picture.
    pub fn centred(shape: MaskShape, layer: [f32; 2]) -> Self {
        let [w, h] = layer;
        let (x0, y0, x1, y1) = (w * 0.25, h * 0.25, w * 0.75, h * 0.75);
        Self { name: shape.label().into(), shape, points: vec![[x0, y0], [x1, y0], [x1, y1], [x0, y1]], ..Default::default() }
    }

    pub fn freehand(points: Vec<[f32; 2]>) -> Self {
        Self { name: "Freehand".into(), shape: MaskShape::Freehand, points, ..Default::default() }
    }

    pub fn is_animated(&self) -> bool {
        !self.keys.is_empty()
    }

    /// The shape's points at source time `t`.
    pub fn points_at(&self, t: Time) -> Vec<[f32; 2]> {
        let k = &self.keys;
        if k.is_empty() {
            return self.points.clone();
        }
        let i = k.partition_point(|key| key.at <= t);
        if i == 0 {
            return k[0].points.clone();
        }
        if i == k.len() {
            return k[i - 1].points.clone();
        }
        let (a, b) = (&k[i - 1], &k[i]);
        if a.points.len() != b.points.len() || b.at == a.at {
            return a.points.clone();
        }
        let u = (t - a.at).0 as f32 / (b.at - a.at).0 as f32;
        a.points.iter().zip(&b.points).map(|(p, q)| [p[0] + (q[0] - p[0]) * u, p[1] + (q[1] - p[1]) * u]).collect()
    }

    /// Change the shape as seen at `t`: a key there if it's animated, else the shape itself.
    pub fn set_points_at(&mut self, t: Time, points: Vec<[f32; 2]>) {
        if self.keys.is_empty() {
            self.points = points;
            return;
        }
        match self.keys.iter_mut().find(|k| k.at == t) {
            Some(k) => k.points = points,
            None => {
                let curves = self.curves_at(t);
                let i = self.keys.partition_point(|k| k.at < t);
                self.keys.insert(i, ShapeKey { at: t, points, curves });
            }
        }
    }

    /// Each point's Bézier handles at source time `t` (one per point; zeros
    /// for straight sides).
    pub fn curves_at(&self, t: Time) -> Vec<[f32; 4]> {
        let n = self.points_at(t).len();
        let fit = |c: &[[f32; 4]]| (0..n).map(|i| c.get(i).copied().unwrap_or([0.0; 4])).collect::<Vec<_>>();
        let k = &self.keys;
        if k.is_empty() {
            return fit(&self.curves);
        }
        let i = k.partition_point(|key| key.at <= t);
        if i == 0 {
            return fit(&k[0].curves);
        }
        if i == k.len() {
            return fit(&k[i - 1].curves);
        }
        let (a, b) = (&k[i - 1], &k[i]);
        let (ca, cb) = (fit(&a.curves), fit(&b.curves));
        if a.points.len() != b.points.len() || b.at == a.at {
            return ca;
        }
        let u = (t - a.at).0 as f32 / (b.at - a.at).0 as f32;
        ca.iter().zip(&cb).map(|(p, q)| std::array::from_fn(|j| p[j] + (q[j] - p[j]) * u)).collect()
    }

    /// Change the shape's handles as seen at `t` (a key there if it's animated).
    pub fn set_curves_at(&mut self, t: Time, curves: Vec<[f32; 4]>) {
        if self.keys.is_empty() {
            self.curves = curves;
            return;
        }
        match self.keys.iter_mut().find(|k| k.at == t) {
            Some(k) => k.curves = curves,
            None => {
                let points = self.points_at(t);
                let i = self.keys.partition_point(|k| k.at < t);
                self.keys.insert(i, ShapeKey { at: t, points, curves });
            }
        }
    }

    /// Add a point halfway along the side from point `i` to the next one
    /// (on the curve, if the side is curved, which keeps its shape), in the
    /// shape and in every shape keyframe so they stay alike. A rectangle
    /// becomes a freehand shape. Returns the new point's index, or `None`
    /// for an ellipse or a bad index.
    pub fn insert_point_after(&mut self, i: usize) -> Option<usize> {
        self.insert_point_on(i, 0.5)
    }

    /// Add a point on side `i` (from point `i` to the next), `s` (0–1) of the
    /// way along it, in the shape and every keyframe (each keyframe's side is
    /// cut at the same place, and curves keep their shape). As
    /// [`Self::insert_point_after`] otherwise.
    pub fn insert_point_on(&mut self, i: usize, s: f32) -> Option<usize> {
        let n = self.points.len().max(self.keys.first().map_or(0, |k| k.points.len()));
        if self.shape == MaskShape::Ellipse || i >= n || n < 2 {
            return None;
        }
        let s = s.clamp(0.01, 0.99);
        split_side_at(&mut self.points, &mut self.curves, i, s);
        for k in &mut self.keys {
            split_side_at(&mut k.points, &mut k.curves, i, s);
        }
        if self.feathers.iter().any(|&f| f >= 0.0) {
            // Halfway between its neighbours' feathers (a missing one is the mask's).
            let own = |j: usize| self.feathers.get(j % n).copied().filter(|f| *f >= 0.0);
            let mid = match (own(i), own(i + 1)) {
                (None, None) => -1.0,
                (a, b) => {
                    let (a, b) = (a.unwrap_or(self.feather), b.unwrap_or(self.feather));
                    a + (b - a) * s
                }
            };
            self.feathers.resize(n, -1.0);
            self.feathers.insert(i + 1, mid);
        }
        self.shape = MaskShape::Freehand;
        Some(i + 1)
    }

    /// The side of the shape at time `t` nearest to `p` (layer pixels): (side
    /// `i`, from point `i` to the next; how far along it, 0–1; distance).
    /// `None` for an ellipse.
    pub fn nearest_side(&self, t: Time, p: [f32; 2]) -> Option<(usize, f32, f32)> {
        if self.shape == MaskShape::Ellipse {
            return None;
        }
        let pts = self.points_at(t);
        let curves = self.curves_at(t);
        let n = pts.len();
        if n < 2 {
            return None;
        }
        let handle = |k: usize, slot: usize| curves.get(k).map_or([0.0, 0.0], |c| [c[slot], c[slot + 1]]);
        let mut best: Option<(usize, f32, f32)> = None;
        const STEPS: usize = 64;
        for i in 0..n {
            let j = (i + 1) % n;
            let (a, b) = (pts[i], pts[j]);
            let (o, inn) = (handle(i, 2), handle(j, 0));
            let (c1, c2) = ([a[0] + o[0], a[1] + o[1]], [b[0] + inn[0], b[1] + inn[1]]);
            let straight = o == [0.0, 0.0] && inn == [0.0, 0.0];
            for k in 0..=STEPS {
                let s = k as f32 / STEPS as f32;
                // A straight side is cut proportionally (see `split_side_at`).
                let q = if straight { [a[0] + (b[0] - a[0]) * s, a[1] + (b[1] - a[1]) * s] } else { cubic(a, c1, c2, b, s) };
                let d = ((q[0] - p[0]).powi(2) + (q[1] - p[1]).powi(2)).sqrt();
                if best.is_none_or(|x| d < x.2) {
                    best = Some((i, s, d));
                }
            }
        }
        best
    }

    /// Take point `i` out of the shape and every keyframe. A shape keeps at
    /// least three points (returns false then, and for an ellipse).
    pub fn remove_point(&mut self, i: usize) -> bool {
        let n = self.points.len().max(self.keys.first().map_or(0, |k| k.points.len()));
        if self.shape == MaskShape::Ellipse || i >= n || n <= 3 {
            return false;
        }
        let drop = |pts: &mut Vec<[f32; 2]>, curves: &mut Vec<[f32; 4]>| {
            if i < pts.len() {
                pts.remove(i);
            }
            if i < curves.len() {
                curves.remove(i);
            }
        };
        drop(&mut self.points, &mut self.curves);
        for k in &mut self.keys {
            drop(&mut k.points, &mut k.curves);
        }
        if i < self.feathers.len() {
            self.feathers.remove(i);
        }
        self.shape = MaskShape::Freehand;
        true
    }

    /// Does the shape have any curved sides at `t`?
    pub fn is_curved(&self, t: Time) -> bool {
        self.shape != MaskShape::Ellipse && self.curves_at(t).iter().any(|c| c.iter().any(|v| *v != 0.0))
    }

    /// The feather at point `i` (layer pixels).
    pub fn feather_at_point(&self, i: usize) -> f32 {
        match self.feathers.get(i) {
            Some(&f) if f >= 0.0 => f,
            _ => self.feather,
        }
    }

    /// Does any point have its own feather?
    pub fn has_point_feathers(&self) -> bool {
        self.shape != MaskShape::Ellipse && self.feathers.iter().any(|&f| f >= 0.0)
    }

    /// Start or stop animating the shape. Starting keys the current shape at
    /// `t`; stopping keeps the shape as it is at `t`.
    pub fn set_animated(&mut self, animated: bool, t: Time) {
        if animated && self.keys.is_empty() {
            self.keys.push(ShapeKey { at: t, points: self.points.clone(), curves: self.curves.clone() });
        } else if !animated && !self.keys.is_empty() {
            self.points = self.points_at(t);
            self.curves = self.curves_at(t);
            self.keys.clear();
        }
    }

    /// The outline as a polygon (an ellipse, and curved sides, become many short sides).
    pub fn outline(&self, t: Time) -> Vec<[f32; 2]> {
        self.outline_feathered(t).into_iter().map(|(p, _)| p).collect()
    }

    /// The outline with the feather at each of its corners.
    pub fn outline_feathered(&self, t: Time) -> Vec<([f32; 2], f32)> {
        let pts = self.points_at(t);
        if self.shape == MaskShape::Ellipse {
            return outline(self.shape, &pts).into_iter().map(|p| (p, self.feather)).collect();
        }
        let curves = self.curves_at(t);
        curved_outline(&pts, &curves, |i| self.feather_at_point(i))
    }
}

/// Points per curved side (more for longer ones), at most.
const CURVE_STEPS: usize = 48;

/// A closed shape through `pts` whose sides are cubic curves shaped by
/// `curves` (straight where a side's handles are zero), each corner with the
/// feather `feather(i)` blended along the sides.
pub fn curved_outline(pts: &[[f32; 2]], curves: &[[f32; 4]], feather: impl Fn(usize) -> f32) -> Vec<([f32; 2], f32)> {
    let n = pts.len();
    let mut out = Vec::with_capacity(n * 4);
    for i in 0..n {
        let j = (i + 1) % n;
        let (a, b) = (pts[i], pts[j]);
        let (fa, fb) = (feather(i), feather(j));
        let ca = curves.get(i).copied().unwrap_or([0.0; 4]);
        let cb = curves.get(j).copied().unwrap_or([0.0; 4]);
        let c1 = [a[0] + ca[2], a[1] + ca[3]];
        let c2 = [b[0] + cb[0], b[1] + cb[1]];
        out.push((a, fa));
        let straight = ca[2] == 0.0 && ca[3] == 0.0 && cb[0] == 0.0 && cb[1] == 0.0;
        // Enough steps to look smooth: about one per 4 px of the control polygon.
        let len = dist(a, c1) + dist(c1, c2) + dist(c2, b);
        let steps = if straight { if fa != fb { 8 } else { 1 } } else { ((len / 4.0).ceil() as usize).clamp(4, CURVE_STEPS) };
        for k in 1..steps {
            let s = k as f32 / steps as f32;
            let p = if straight { [a[0] + (b[0] - a[0]) * s, a[1] + (b[1] - a[1]) * s] } else { cubic(a, c1, c2, b, s) };
            out.push((p, fa + (fb - fa) * s));
        }
    }
    out
}

fn dist(a: [f32; 2], b: [f32; 2]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
}

/// A point on the cubic Bézier `a`, `c1`, `c2`, `b` at `s` (0–1).
pub fn cubic(a: [f32; 2], c1: [f32; 2], c2: [f32; 2], b: [f32; 2], s: f32) -> [f32; 2] {
    let r = 1.0 - s;
    let (w0, w1, w2, w3) = (r * r * r, 3.0 * r * r * s, 3.0 * r * s * s, s * s * s);
    [w0 * a[0] + w1 * c1[0] + w2 * c2[0] + w3 * b[0], w0 * a[1] + w1 * c1[1] + w2 * c2[1] + w3 * b[1]]
}

/// Smooth handles for point `i` of a closed shape, pointing along the line
/// between its neighbours (a third of the way to each), for turning a corner
/// into a curve.
pub fn smooth_handles(pts: &[[f32; 2]], i: usize) -> [f32; 4] {
    let n = pts.len();
    if n < 3 {
        return [0.0; 4];
    }
    let (prev, p, next) = (pts[(i + n - 1) % n], pts[i], pts[(i + 1) % n]);
    let d = [next[0] - prev[0], next[1] - prev[1]];
    let len = (d[0] * d[0] + d[1] * d[1]).sqrt().max(1e-6);
    let u = [d[0] / len, d[1] / len];
    let (lp, ln) = (dist(prev, p) / 3.0, dist(p, next) / 3.0);
    [-u[0] * lp, -u[1] * lp, u[0] * ln, u[1] * ln]
}

/// The polygon for a shape's points.
pub fn outline(shape: MaskShape, pts: &[[f32; 2]]) -> Vec<[f32; 2]> {
    match shape {
        MaskShape::Ellipse if pts.len() == 4 => {
            // The ellipse inside the four corners: a circle mapped bilinearly into the quad.
            let [a, b, c, d] = [pts[0], pts[1], pts[2], pts[3]];
            (0..ELLIPSE_SEGMENTS)
                .map(|i| {
                    let th = i as f32 / ELLIPSE_SEGMENTS as f32 * std::f32::consts::TAU;
                    let (s, u) = (0.5 + 0.5 * th.cos(), 0.5 + 0.5 * th.sin());
                    let top = [a[0] + (b[0] - a[0]) * s, a[1] + (b[1] - a[1]) * s];
                    let bottom = [d[0] + (c[0] - d[0]) * s, d[1] + (c[1] - d[1]) * s];
                    [top[0] + (bottom[0] - top[0]) * u, top[1] + (bottom[1] - top[1]) * u]
                })
                .collect()
        }
        _ => pts.to_vec(),
    }
}

/// An 8-bit alpha image.
#[derive(Clone, Debug, PartialEq)]
pub struct MaskImage {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

/// Do these masks do anything (any enabled, with a shape)?
pub fn any_active(masks: &[Mask]) -> bool {
    masks.iter().any(|m| m.enabled && m.points.len().max(m.keys.first().map_or(0, |k| k.points.len())) >= 3)
}

/// A hash of what the masks look like at `t` on a `layer`-sized picture, to
/// know when the image needs making again.
pub fn mask_hash(masks: &[Mask], t: Time, layer: [f32; 2]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    layer[0].to_bits().hash(&mut h);
    layer[1].to_bits().hash(&mut h);
    for m in masks.iter().filter(|m| m.enabled) {
        (m.shape, m.mode, m.inverted).hash(&mut h);
        for f in [m.feather, m.expansion, m.opacity] {
            f.to_bits().hash(&mut h);
        }
        for p in m.points_at(t) {
            p[0].to_bits().hash(&mut h);
            p[1].to_bits().hash(&mut h);
        }
        for c in m.curves_at(t) {
            c.iter().for_each(|v| v.to_bits().hash(&mut h));
        }
        m.feathers.iter().for_each(|v| v.to_bits().hash(&mut h));
    }
    h.finish()
}

/// Turn the enabled masks at source time `t` into an alpha image for a
/// `layer`-sized picture, at most `max_side` pixels on its longer side (the
/// renderer stretches it over the layer). `None` if no mask is active.
pub fn rasterize(masks: &[Mask], t: Time, layer: [f32; 2], max_side: u32) -> Option<MaskImage> {
    if !any_active(masks) {
        return None;
    }
    let k = (max_side as f32 / layer[0].max(layer[1]).max(1.0)).min(1.0);
    let (w, h) = (((layer[0] * k).ceil() as usize).max(1), ((layer[1] * k).ceil() as usize).max(1));
    let mut acc: Option<Vec<f32>> = None;
    for m in masks.iter().filter(|m| m.enabled) {
        let line = m.outline_feathered(t);
        let poly: Vec<[f32; 2]> = line.iter().map(|(p, _)| [p[0] * k, p[1] * k]).collect();
        if poly.len() < 3 {
            continue;
        }
        let feathers: Vec<f32> = line.iter().map(|(_, f)| f.max(0.0) * k).collect();
        let a = shape_alpha(&poly, &feathers, w, h, m.expansion * k);
        let acc = acc.get_or_insert_with(|| vec![if m.mode == MaskMode::Add { 0.0 } else { 1.0 }; w * h]);
        for (o, v) in acc.iter_mut().zip(a) {
            let v = if m.inverted { 1.0 - v } else { v } * m.opacity.clamp(0.0, 1.0);
            *o = match m.mode {
                MaskMode::Add => *o + v - *o * v,
                MaskMode::Subtract => *o * (1.0 - v),
                MaskMode::Intersect => *o * v,
            };
        }
    }
    let acc = acc?;
    Some(MaskImage { width: w as u32, height: h as u32, data: acc.iter().map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8).collect() })
}

/// Coverage of one closed polygon on a `w`×`h` grid (pixel centres at +0.5),
/// with a soft edge as wide as `feathers` says at each corner (blended along
/// the sides), the edge moved out by `expansion`.
fn shape_alpha(poly: &[[f32; 2]], feathers: &[f32], w: usize, h: usize, expansion: f32) -> Vec<f32> {
    // Inside or out: even-odd fill, one scanline per pixel row.
    let mut inside = vec![false; w * h];
    let mut xs: Vec<f32> = Vec::new();
    for y in 0..h {
        let cy = y as f32 + 0.5;
        xs.clear();
        for i in 0..poly.len() {
            let (a, b) = (poly[i], poly[(i + 1) % poly.len()]);
            if (a[1] <= cy) != (b[1] <= cy) {
                xs.push(a[0] + (cy - a[1]) / (b[1] - a[1]) * (b[0] - a[0]));
            }
        }
        xs.sort_by(|a, b| a.total_cmp(b));
        for span in xs.chunks_exact(2) {
            let x0 = (span[0] - 0.5).ceil().max(0.0) as usize;
            let x1 = ((span[1] - 0.5).floor() + 1.0).clamp(0.0, w as f32) as usize;
            for v in &mut inside[y * w + x0.min(w)..y * w + x1.max(x0.min(w))] {
                *v = true;
            }
        }
    }
    // Distance to the outline, only in a band around it (that's where alpha is in between).
    let widest = feathers.iter().copied().fold(0.0f32, f32::max);
    let soft = widest.max(1.0);
    let band = expansion.abs() + soft / 2.0 + 1.5;
    let mut dist = vec![f32::INFINITY; w * h];
    // The feather where the nearest point of the outline is.
    let mut soft_at = vec![widest; w * h];
    let n = poly.len();
    for i in 0..n {
        let (a, b) = (poly[i], poly[(i + 1) % n]);
        let (fa, fb) = (feathers.get(i).copied().unwrap_or(0.0), feathers.get((i + 1) % n).copied().unwrap_or(0.0));
        // This side's own band (a narrower feather needs less).
        let band = expansion.abs() + fa.max(fb).max(1.0) / 2.0 + 1.5;
        let x0 = (a[0].min(b[0]) - band).floor().max(0.0) as usize;
        let x1 = ((a[0].max(b[0]) + band).ceil().max(0.0) as usize).min(w);
        let y0 = (a[1].min(b[1]) - band).floor().max(0.0) as usize;
        let y1 = ((a[1].max(b[1]) + band).ceil().max(0.0) as usize).min(h);
        let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
        let len2 = (dx * dx + dy * dy).max(1e-12);
        for y in y0..y1 {
            for x in x0..x1 {
                let (px, py) = (x as f32 + 0.5 - a[0], y as f32 + 0.5 - a[1]);
                let u = ((px * dx + py * dy) / len2).clamp(0.0, 1.0);
                let (ex, ey) = (px - u * dx, py - u * dy);
                let d = (ex * ex + ey * ey).sqrt();
                let o = &mut dist[y * w + x];
                if d < *o {
                    *o = d;
                    soft_at[y * w + x] = fa + (fb - fa) * u;
                }
            }
        }
    }
    inside
        .iter()
        .zip(&dist)
        .zip(&soft_at)
        .map(|((&ins, &d), &feather)| {
            // Signed distance, positive inside, after expansion.
            let sd = if d.is_finite() { if ins { d } else { -d } } else if ins { band * 2.0 } else { -band * 2.0 };
            let v = sd + expansion;
            if feather <= 1.0 {
                (v + 0.5).clamp(0.0, 1.0) // just anti-aliasing
            } else {
                let x = ((v + feather / 2.0) / feather).clamp(0.0, 1.0);
                x * x * (3.0 - 2.0 * x)
            }
        })
        .collect()
}

/// Split the side from point `i` to the next of a closed shape in two at its
/// middle: a straight side at its midpoint, a curved one on the curve (de
/// Casteljau), with the handles either side shortened so the shape is unchanged.
/// Split side `i` (from point `i` to the next) at `s` (0–1 along it): a new
/// point there, and a curved side's handles cut so its shape stays the same
/// (de Casteljau).
fn split_side_at(points: &mut Vec<[f32; 2]>, curves: &mut Vec<[f32; 4]>, i: usize, s: f32) {
    let n = points.len();
    if i >= n || n < 2 {
        return;
    }
    curves.resize(n, [0.0; 4]);
    let j = (i + 1) % n;
    let (p0, p3) = (points[i], points[j]);
    let out = [curves[i][2], curves[i][3]];
    let inn = [curves[j][0], curves[j][1]];
    let lerp = |a: [f32; 2], b: [f32; 2]| [a[0] + (b[0] - a[0]) * s, a[1] + (b[1] - a[1]) * s];
    let sub = |a: [f32; 2], b: [f32; 2]| [a[0] - b[0], a[1] - b[1]];
    if out == [0.0, 0.0] && inn == [0.0, 0.0] {
        points.insert(i + 1, lerp(p0, p3));
        curves.insert(i + 1, [0.0; 4]);
        return;
    }
    let p1 = [p0[0] + out[0], p0[1] + out[1]];
    let p2 = [p3[0] + inn[0], p3[1] + inn[1]];
    let (p01, p12, p23) = (lerp(p0, p1), lerp(p1, p2), lerp(p2, p3));
    let (p012, p123) = (lerp(p01, p12), lerp(p12, p23));
    let m = lerp(p012, p123);
    let o = sub(p01, p0);
    curves[i][2] = o[0];
    curves[i][3] = o[1];
    let ni = sub(p23, p3);
    curves[j][0] = ni[0];
    curves[j][1] = ni[1];
    let (mi, mo) = (sub(p012, m), sub(p123, m));
    points.insert(i + 1, m);
    curves.insert(i + 1, [mi[0], mi[1], mo[0], mo[1]]);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Area inside a polygon (shoelace).
    fn area(p: &[[f32; 2]]) -> f32 {
        (0..p.len()).map(|i| { let (a, b) = (p[i], p[(i + 1) % p.len()]); a[0] * b[1] - b[0] * a[1] }).sum::<f32>().abs() / 2.0
    }

    #[test]
    fn a_point_goes_where_the_side_was_clicked_and_curves_keep_their_shape() {
        // A square, 0–100; click a quarter of the way along its top side.
        let mut m = Mask { shape: MaskShape::Rectangle, points: vec![[0.0, 0.0], [100.0, 0.0], [100.0, 100.0], [0.0, 100.0]], ..Mask::default() };
        let (i, s, d) = m.nearest_side(Time::ZERO, [25.0, 3.0]).unwrap();
        assert_eq!(i, 0);
        assert!((s - 0.25).abs() < 0.02 && (d - 3.0).abs() < 0.1, "{s} {d}");
        assert_eq!(m.insert_point_on(i, s), Some(1));
        assert!((m.points[1][0] - 25.0).abs() < 2.0 && m.points[1][1] == 0.0);
        assert_eq!(m.shape, MaskShape::Freehand);
        // The bottom side, now from point 3 (100, 100) to point 4 (0, 100).
        assert_eq!(m.nearest_side(Time::ZERO, [70.0, 98.0]).unwrap().0, 3);
        // A curved side: the outline is the same after the cut, and the new
        // point is on the curve.
        let mut c = Mask { shape: MaskShape::Freehand, points: vec![[0.0, 0.0], [100.0, 0.0], [50.0, 80.0]], curves: vec![[0.0, 0.0, 30.0, -40.0], [-30.0, -40.0, 0.0, 0.0], [0.0; 4]], ..Mask::default() };
        let before = c.outline(Time::ZERO);
        let (i, s, _) = c.nearest_side(Time::ZERO, [70.0, -25.0]).unwrap();
        assert_eq!(i, 0);
        c.insert_point_on(i, s).unwrap();
        // Distance from `q` to the old outline (a closed polyline).
        let to_old = |q: [f32; 2]| {
            (0..before.len())
                .map(|k| {
                    let (a, b) = (before[k], before[(k + 1) % before.len()]);
                    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
                    let t = (((q[0] - a[0]) * dx + (q[1] - a[1]) * dy) / (dx * dx + dy * dy).max(1e-9)).clamp(0.0, 1.0);
                    ((a[0] + dx * t - q[0]).powi(2) + (a[1] + dy * t - q[1]).powi(2)).sqrt()
                })
                .fold(f32::MAX, f32::min)
        };
        let near = to_old(c.points[1]);
        assert!(near < 0.5, "the new point is on the old curve ({near})");
        for q in c.outline(Time::ZERO) {
            let d = to_old(q);
            assert!(d < 0.5, "the outline keeps its shape ({d} at {q:?})");
        }
        // Keyframes are cut at the same place.
        let mut k = Mask { shape: MaskShape::Freehand, points: vec![[0.0, 0.0], [100.0, 0.0], [50.0, 80.0]], ..Mask::default() };
        k.keys = vec![ShapeKey::new(Time::ZERO, k.points.clone()), ShapeKey::new(Time::from_secs_f64(1.0), vec![[0.0, 0.0], [200.0, 0.0], [50.0, 80.0]])];
        k.insert_point_on(0, 0.25).unwrap();
        assert_eq!(k.keys[1].points[1], [50.0, 0.0]);
    }

    #[test]
    fn adding_a_point_keeps_the_shape_and_its_keyframes_alike() {
        let square = vec![[0.0, 0.0], [100.0, 0.0], [100.0, 100.0], [0.0, 100.0]];
        let mut m = Mask { shape: MaskShape::Rectangle, points: square.clone(), ..Default::default() };
        // Straight side: the midpoint; the rectangle becomes freehand.
        assert_eq!(m.insert_point_after(1), Some(2));
        assert_eq!(m.points[2], [100.0, 50.0]);
        assert_eq!(m.shape, MaskShape::Freehand);
        assert_eq!(m.curves_at(Time::ZERO).len(), 5);
        // The last side wraps round to the first point.
        assert_eq!(m.insert_point_after(4), Some(5));
        assert_eq!(m.points[5], [0.0, 50.0]);
        // A curved side: the new point is on the curve and the outline is unchanged.
        let mut c = Mask { shape: MaskShape::Freehand, points: square.clone(), curves: vec![[0.0; 4], [0.0, 0.0, 40.0, 30.0], [20.0, -30.0, 0.0, 0.0], [0.0; 4]], ..Default::default() };
        let before = area(&c.outline(Time::ZERO));
        c.insert_point_after(1);
        assert_eq!(c.points.len(), 5);
        let after = area(&c.outline(Time::ZERO));
        assert!((before - after).abs() < before * 0.01, "{before} {after}");
        assert!(c.points[2][0] > 100.0, "out on the bulge: {:?}", c.points[2]);
        // Animated: every key gets the point, so the shape still blends between them.
        let mut a = Mask { shape: MaskShape::Freehand, points: square.clone(), ..Default::default() };
        a.keys = vec![ShapeKey::new(Time::ZERO, square.clone()), ShapeKey::new(Time::from_secs_f64(1.0), square.iter().map(|p| [p[0] * 2.0, p[1]]).collect())];
        a.insert_point_after(0);
        assert!(a.keys.iter().all(|k| k.points.len() == 5));
        assert_eq!(a.points_at(Time::from_secs_f64(0.5))[1], [75.0, 0.0]);
        // Per-point feathers: the new point's is halfway between its neighbours'.
        let mut f = Mask { shape: MaskShape::Freehand, points: square.clone(), feather: 10.0, feathers: vec![-1.0, 30.0, -1.0, -1.0], ..Default::default() };
        f.insert_point_after(0);
        assert_eq!(f.feathers, vec![-1.0, 20.0, 30.0, -1.0, -1.0]);
        // Ellipses have no points to add.
        assert_eq!(Mask { shape: MaskShape::Ellipse, points: square.clone(), ..Default::default() }.insert_point_after(0), None);
    }

    #[test]
    fn removing_points_leaves_at_least_three() {
        let mut m = Mask { shape: MaskShape::Freehand, points: vec![[0.0, 0.0], [50.0, 0.0], [100.0, 0.0], [100.0, 100.0], [0.0, 100.0]], curves: vec![[0.0; 4], [1.0, 2.0, 3.0, 4.0], [0.0; 4], [0.0; 4], [0.0; 4]], ..Default::default() };
        m.keys = vec![ShapeKey::new(Time::ZERO, m.points.clone())];
        assert!(m.remove_point(1));
        assert_eq!(m.points, vec![[0.0, 0.0], [100.0, 0.0], [100.0, 100.0], [0.0, 100.0]]);
        assert_eq!(m.curves.len(), 4);
        assert_eq!(m.keys[0].points.len(), 4);
        assert!(m.remove_point(0));
        assert!(!m.remove_point(0), "three points stay");
        assert_eq!(m.points.len(), 3);
    }

    fn at(img: &MaskImage, x: u32, y: u32) -> u8 {
        img.data[(y * img.width + x) as usize]
    }

    fn rect(x0: f32, y0: f32, x1: f32, y1: f32) -> Mask {
        Mask { points: vec![[x0, y0], [x1, y0], [x1, y1], [x0, y1]], ..Default::default() }
    }

    #[test]
    fn rectangle_with_hard_and_feathered_edges() {
        let layer = [100.0, 50.0];
        let img = rasterize(&[rect(20.0, 10.0, 60.0, 40.0)], Time::ZERO, layer, 4096).unwrap();
        assert_eq!((img.width, img.height), (100, 50));
        assert_eq!((at(&img, 30, 20), at(&img, 10, 20), at(&img, 70, 20)), (255, 0, 0));
        // Pixel 20 (centre 20.5) is inside, pixel 19 isn't.
        assert_eq!(at(&img, 19, 20), 0);
        assert_eq!(at(&img, 20, 20), 255);
        // A 20 px feather: half at the edge, full 10 px in, none 10 px out.
        let mut m = rect(20.0, 10.0, 60.0, 40.0);
        m.feather = 20.0;
        let img = rasterize(&[m.clone()], Time::ZERO, layer, 4096).unwrap();
        let edge = at(&img, 40, 10) as i32; // centre 10.5: 0.5 px inside
        assert!((edge - 140).abs() < 12, "{edge}");
        assert_eq!(at(&img, 40, 25), 255);
        // Expanded by 5 px, the edge moves from 20 to 15.
        m.feather = 0.0;
        m.expansion = 5.0;
        let img = rasterize(&[m], Time::ZERO, layer, 4096).unwrap();
        assert_eq!((at(&img, 14, 20), at(&img, 15, 20)), (0, 255));
    }

    #[test]
    fn combining_inverting_and_ellipses() {
        let layer = [100.0, 100.0];
        let a = rect(0.0, 0.0, 60.0, 100.0);
        let b = Mask { mode: MaskMode::Subtract, ..rect(40.0, 0.0, 100.0, 100.0) };
        let img = rasterize(&[a.clone(), b], Time::ZERO, layer, 4096).unwrap();
        assert_eq!((at(&img, 20, 50), at(&img, 50, 50), at(&img, 80, 50)), (255, 0, 0));
        let c = Mask { mode: MaskMode::Intersect, ..rect(40.0, 0.0, 100.0, 100.0) };
        let img = rasterize(&[a.clone(), c], Time::ZERO, layer, 4096).unwrap();
        assert_eq!((at(&img, 20, 50), at(&img, 50, 50), at(&img, 80, 50)), (0, 255, 0));
        let inv = Mask { inverted: true, opacity: 0.5, ..a };
        let img = rasterize(&[inv], Time::ZERO, layer, 4096).unwrap();
        assert_eq!((at(&img, 20, 50), at(&img, 80, 50)), (0, 128));
        // An ellipse inside a 100×100 square covers about π/4 of it.
        let e = Mask::centred(MaskShape::Ellipse, [200.0, 200.0]);
        let e = Mask { points: vec![[0.0, 0.0], [100.0, 0.0], [100.0, 100.0], [0.0, 100.0]], ..e };
        let img = rasterize(&[e], Time::ZERO, layer, 4096).unwrap();
        let area: f32 = img.data.iter().map(|&v| v as f32 / 255.0).sum();
        assert!((area / 10_000.0 - std::f32::consts::FRAC_PI_4).abs() < 0.01, "{area}");
        // Nothing enabled: no image.
        assert!(rasterize(&[Mask { enabled: false, ..rect(0.0, 0.0, 1.0, 1.0) }], Time::ZERO, layer, 4096).is_none());
    }

    #[test]
    fn curved_sides_and_feather_per_point() {
        let layer = [200.0, 200.0];
        // A diamond with smooth handles at every point bulges out towards a circle.
        let pts = vec![[100.0, 20.0], [180.0, 100.0], [100.0, 180.0], [20.0, 100.0]];
        let straight = Mask::freehand(pts.clone());
        let mut round = straight.clone();
        round.curves = (0..4).map(|i| smooth_handles(&pts, i)).collect();
        assert!(round.is_curved(Time::ZERO) && !straight.is_curved(Time::ZERO));
        let area = |m: &Mask| rasterize(&[m.clone()], Time::ZERO, layer, 4096).unwrap().data.iter().map(|&v| v as f32 / 255.0).sum::<f32>();
        let (a0, a1) = (area(&straight), area(&round));
        assert!((a0 - 12_800.0).abs() < 60.0, "{a0}");
        // Between the diamond (12 800) and the circle through its points (≈20 100).
        assert!(a1 > 17_000.0 && a1 < 20_200.0, "{a1}");
        // The bulge: the middle of a side was outside the diamond, now inside.
        let img = rasterize(&[round.clone()], Time::ZERO, layer, 4096).unwrap();
        assert_eq!(at(&img, 150, 50), 255);
        assert!(round.outline(Time::ZERO).len() > 16, "curves become many short sides");
        // Feather per point: soft at the top, hard at the bottom.
        let mut m = rect(40.0, 40.0, 160.0, 160.0);
        m.feather = 0.0;
        m.feathers = vec![40.0, 40.0, -1.0, -1.0];
        assert!(m.has_point_feathers());
        assert_eq!((m.feather_at_point(0), m.feather_at_point(3), m.feather_at_point(9)), (40.0, 0.0, 0.0));
        let img = rasterize(&[m.clone()], Time::ZERO, layer, 4096).unwrap();
        let top = at(&img, 100, 40) as i32; // on the top edge: about half
        let bottom_in = at(&img, 100, 158); // just inside the hard bottom edge
        assert!((top - 128).abs() < 25, "{top}");
        assert_eq!((bottom_in, at(&img, 100, 161)), (255, 0));
        assert!(at(&img, 100, 30) > 0, "the soft top reaches outside");
        // Halfway down the left side, the feather is halfway between.
        let mid_out = at(&img, 35, 100);
        assert!(mid_out > 0 && mid_out < at(&img, 35, 45).max(1) + 60, "{mid_out}");
        // Curves are keyed with the shape, and blend between keys.
        let s = Time::from_secs_f64;
        let mut k = round.clone();
        k.set_animated(true, s(0.0));
        k.set_curves_at(s(2.0), vec![[0.0; 4]; 4]);
        assert_eq!(k.keys.len(), 2);
        let half = k.curves_at(s(1.0));
        assert!((half[0][2] - round.curves[0][2] / 2.0).abs() < 1e-4);
        assert_ne!(mask_hash(&[k.clone()], s(0.0), layer), mask_hash(&[k.clone()], s(2.0), layer));
        k.set_animated(false, s(0.0));
        assert_eq!(k.curves, round.curves, "stopping keeps the handles at that moment");
        // Saved and loaded, including old masks without handles.
        let back: Mask = serde_json::from_str(&serde_json::to_string(&m).unwrap()).unwrap();
        assert_eq!(back, m);
        let old: Mask = serde_json::from_str(r#"{"points":[[0,0],[1,0],[1,1]],"keys":[{"at":0,"points":[[0,0],[1,0],[1,1]]}]}"#).unwrap();
        assert!(old.curves.is_empty() && old.keys[0].curves.is_empty() && old.feathers.is_empty());
    }

    #[test]
    fn animated_shapes_and_reduced_resolution() {
        let s = Time::from_secs_f64;
        let mut m = rect(0.0, 0.0, 10.0, 10.0);
        m.set_animated(true, s(0.0));
        m.set_points_at(s(2.0), vec![[10.0, 0.0], [20.0, 0.0], [20.0, 10.0], [10.0, 10.0]]);
        assert_eq!(m.points_at(s(1.0))[0], [5.0, 0.0]);
        assert_eq!(m.points_at(s(5.0))[0], [10.0, 0.0]);
        assert_ne!(mask_hash(&[m.clone()], s(0.5), [20.0, 10.0]), mask_hash(&[m.clone()], s(1.0), [20.0, 10.0]));
        m.set_animated(false, s(1.0));
        assert_eq!((m.keys.len(), m.points[0]), (0, [5.0, 0.0]), "stopping keeps the shape at that moment");
        // A 4000-wide layer is masked at 1000 px.
        let img = rasterize(&[rect(0.0, 0.0, 2000.0, 1000.0)], Time::ZERO, [4000.0, 1000.0], 1000).unwrap();
        assert_eq!((img.width, img.height), (1000, 250));
        assert_eq!((at(&img, 400, 100), at(&img, 600, 100)), (255, 0));
    }
}
