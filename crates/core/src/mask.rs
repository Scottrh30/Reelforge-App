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
    /// Any number of points, joined by straight lines.
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
                let i = self.keys.partition_point(|k| k.at < t);
                self.keys.insert(i, ShapeKey { at: t, points });
            }
        }
    }

    /// Start or stop animating the shape. Starting keys the current shape at
    /// `t`; stopping keeps the shape as it is at `t`.
    pub fn set_animated(&mut self, animated: bool, t: Time) {
        if animated && self.keys.is_empty() {
            self.keys.push(ShapeKey { at: t, points: self.points.clone() });
        } else if !animated && !self.keys.is_empty() {
            self.points = self.points_at(t);
            self.keys.clear();
        }
    }

    /// The outline as a polygon (an ellipse becomes many short sides).
    pub fn outline(&self, t: Time) -> Vec<[f32; 2]> {
        outline(self.shape, &self.points_at(t))
    }
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
        let poly: Vec<[f32; 2]> = m.outline(t).iter().map(|p| [p[0] * k, p[1] * k]).collect();
        if poly.len() < 3 {
            continue;
        }
        let a = shape_alpha(&poly, w, h, m.feather * k, m.expansion * k);
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
/// with a soft edge `feather` wide, the edge moved out by `expansion`.
fn shape_alpha(poly: &[[f32; 2]], w: usize, h: usize, feather: f32, expansion: f32) -> Vec<f32> {
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
    let soft = feather.max(1.0);
    let band = expansion.abs() + soft / 2.0 + 1.5;
    let mut dist = vec![f32::INFINITY; w * h];
    for i in 0..poly.len() {
        let (a, b) = (poly[i], poly[(i + 1) % poly.len()]);
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
                }
            }
        }
    }
    inside
        .iter()
        .zip(&dist)
        .map(|(&ins, &d)| {
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

#[cfg(test)]
mod tests {
    use super::*;

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
