//! Paint and clone strokes on a layer (the Paint effect): brush strokes in a
//! colour, or clone strokes that copy the picture from somewhere else in the
//! frame (a clone stamp), each lasting one frame, from its frame to the end,
//! or the whole clip.
//!
//! The strokes showing at a moment go to the GPU as a small table: row 0 the
//! line segments (three texels each: start, end, which stroke), row 1 a box
//! round each run of up to [`CHUNK`] segments of one stroke (two texels, then
//! the stroke), row 2 each stroke's settings (four texels). Positions are
//! 16-bit fractions of twice the layer, so strokes can reach past its edges.

use crate::lut::Table;
use crate::time::Time;
use serde::{Deserialize, Serialize};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

pub const CHUNK: usize = 8;
/// At most this many segments are drawn at once.
pub const MAX_SEGMENTS: usize = 2600;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Tool {
    /// Copies the picture from `offset` away.
    #[default]
    Clone,
    /// Paints in a colour.
    Brush,
    /// Takes away earlier strokes, showing the picture as it was.
    Erase,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Lasts {
    /// Only the frame it was painted on.
    #[default]
    Frame,
    /// From that frame to the clip's end.
    OnFromHere,
    /// The whole clip.
    Always,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PaintStroke {
    pub tool: Tool,
    /// Layer pixels.
    pub points: Vec<[f32; 2]>,
    pub radius: f32,
    /// 0–100: how sharp the edge is.
    pub hardness: f32,
    /// 0–100.
    pub opacity: f32,
    /// 0xRRGGBB (brush).
    pub colour: u32,
    /// Where a clone copies from, relative to where it's painted (layer pixels).
    pub offset: [f32; 2],
    /// The source time it was painted at, how long it lasts, and a frame's
    /// length then (for "this frame only").
    pub at: Time,
    pub lasts: Lasts,
    pub frame: Time,
}

impl PaintStroke {
    /// Does it show at source time `t`?
    pub fn shows_at(&self, t: Time) -> bool {
        let frame = self.frame;
        match self.lasts {
            Lasts::Always => true,
            Lasts::OnFromHere => t.0 + frame.0 / 2 >= self.at.0,
            Lasts::Frame => (t.0 - self.at.0).abs() * 2 < frame.0.max(1),
        }
    }
}

/// The strokes showing at `t` as the GPU's table, and how many segments
/// and runs it holds (`None`: nothing to draw).
pub fn table(strokes: &[PaintStroke], t: Time, size: [f32; 2]) -> Option<(Arc<Table>, usize, usize)> {
    let shown: Vec<&PaintStroke> = strokes.iter().filter(|s| s.shows_at(t) && !s.points.is_empty()).collect();
    if shown.is_empty() {
        return None;
    }
    // Segments and runs, stroke by stroke (a single point is a dab).
    let mut segs: Vec<([f32; 2], [f32; 2], usize)> = Vec::new();
    let mut runs: Vec<(usize, usize, usize)> = Vec::new(); // (first segment, count, stroke)
    for (k, s) in shown.iter().enumerate().take(4000) {
        let pts: Vec<([f32; 2], [f32; 2])> = if s.points.len() == 1 { vec![(s.points[0], s.points[0])] } else { s.points.windows(2).map(|w| (w[0], w[1])).collect() };
        for chunk in pts.chunks(CHUNK) {
            if segs.len() + chunk.len() > MAX_SEGMENTS {
                break;
            }
            runs.push((segs.len(), chunk.len(), k));
            segs.extend(chunk.iter().map(|&(a, b)| (a, b, k)));
        }
    }
    let width = (3 * segs.len()).max(3 * runs.len()).max(4 * shown.len());
    let row = width * 4;
    let mut rgba = vec![0u8; row * 3];
    let q = |v: f32, s: f32| -> [u8; 2] { (((((v / s.max(1.0)) + 0.5) / 2.0).clamp(0.0, 1.0) * 65535.0).round() as u16).to_be_bytes() };
    let pt = |p: [f32; 2]| -> [u8; 4] {
        let (x, y) = (q(p[0], size[0]), q(p[1], size[1]));
        [x[0], x[1], y[0], y[1]]
    };
    let u16b = |v: usize| (v.min(65535) as u16).to_be_bytes();
    for (i, (a, b, k)) in segs.iter().enumerate() {
        let o = i * 12;
        rgba[o..o + 4].copy_from_slice(&pt(*a));
        rgba[o + 4..o + 8].copy_from_slice(&pt(*b));
        rgba[o + 8..o + 10].copy_from_slice(&u16b(*k));
    }
    for (r, (first, n, k)) in runs.iter().enumerate() {
        let s = shown[*k];
        let pad = s.radius.max(0.5) + 1.0;
        let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
        for (a, b, _) in &segs[*first..first + n] {
            for p in [a, b] {
                lo = [lo[0].min(p[0] - pad), lo[1].min(p[1] - pad)];
                hi = [hi[0].max(p[0] + pad), hi[1].max(p[1] + pad)];
            }
        }
        let o = row + r * 12;
        rgba[o..o + 4].copy_from_slice(&pt(lo));
        rgba[o + 4..o + 8].copy_from_slice(&pt(hi));
        // Which stroke (× 16) and how many segments; where they start.
        rgba[o + 8..o + 10].copy_from_slice(&u16b(k * 16 + n));
        rgba[o + 10..o + 12].copy_from_slice(&u16b(*first));
    }
    for (k, s) in shown.iter().enumerate() {
        let o = 2 * row + k * 16;
        rgba[o] = match s.tool {
            Tool::Clone => 0,
            Tool::Brush => 1,
            Tool::Erase => 2,
        };
        rgba[o + 1] = (s.opacity.clamp(0.0, 100.0) * 2.55).round() as u8;
        rgba[o + 2] = (s.hardness.clamp(0.0, 100.0) * 2.55).round() as u8;
        rgba[o + 4] = (s.colour >> 16) as u8;
        rgba[o + 5] = (s.colour >> 8) as u8;
        rgba[o + 6] = s.colour as u8;
        // The offset as a fraction of the layer, −1 to 1.
        let off = |v: f32, sz: f32| ((((v / sz.max(1.0)).clamp(-1.0, 1.0) + 1.0) / 2.0 * 65535.0).round() as u16).to_be_bytes();
        let (ox, oy) = (off(s.offset[0], size[0]), off(s.offset[1], size[1]));
        rgba[o + 8..o + 12].copy_from_slice(&[ox[0], ox[1], oy[0], oy[1]]);
        // Radius in quarter pixels.
        rgba[o + 12..o + 14].copy_from_slice(&((s.radius * 4.0).round().clamp(0.0, 65535.0) as u16).to_be_bytes());
    }
    let mut h = std::collections::hash_map::DefaultHasher::new();
    rgba.hash(&mut h);
    Some((Arc::new(Table { id: h.finish() | 1 << 61, width: width as u32, height: 3, rgba, cube_size: 0 }), segs.len(), runs.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stroke(lasts: Lasts, at: f64) -> PaintStroke {
        PaintStroke { tool: Tool::Brush, points: vec![[10.0, 10.0], [50.0, 10.0]], radius: 5.0, hardness: 80.0, opacity: 100.0, colour: 0xff0000, offset: [0.0, 0.0], at: Time::from_secs_f64(at), lasts, frame: Time::from_secs_f64(1.0 / 25.0) }
    }

    #[test]
    fn strokes_show_for_as_long_as_they_last() {
        let s = |x| Time::from_secs_f64(x);
        assert!(stroke(Lasts::Frame, 1.0).shows_at(s(1.0)) && !stroke(Lasts::Frame, 1.0).shows_at(s(1.04)));
        assert!(stroke(Lasts::OnFromHere, 1.0).shows_at(s(3.0)) && !stroke(Lasts::OnFromHere, 1.0).shows_at(s(0.9)));
        assert!(stroke(Lasts::Always, 1.0).shows_at(s(0.0)));
        let all = [stroke(Lasts::Frame, 1.0), stroke(Lasts::Always, 0.0)];
        let (t, segs, runs) = table(&all, s(1.0), [100.0, 50.0]).unwrap();
        assert_eq!((segs, runs, t.height), (2, 2, 3));
        let (_, segs, _) = table(&all, s(2.0), [100.0, 50.0]).unwrap();
        assert_eq!(segs, 1);
        assert!(table(&all[..1], s(2.0), [100.0, 50.0]).is_none());
    }
}
