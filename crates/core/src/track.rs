//! Point tracking: follow a small feature (a corner, a mark, a light) through
//! a clip, then make another layer move with it, or hold the clip itself steady.
//!
//! The tracker compares a square patch around the feature with every position
//! in a search area of the next frame, using normalised cross-correlation (so
//! brightness changes don't throw it off), and refines the best match to a
//! fraction of a pixel. It works on luma only.

use crate::anim::TransformAnim;
use crate::project::Clip;
use crate::time::Time;
use serde::{Deserialize, Serialize};

/// One tracked position.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct TrackSample {
    /// Source time in the clip's media.
    pub at: Time,
    /// Where the feature is, in the media's pixels (top left = 0, 0).
    pub pos: [f32; 2],
    /// Match quality, 0–1 (1 = identical).
    pub score: f32,
}

/// A clip's tracker: where to start, how big the feature and the search area
/// are, and what has been tracked so far.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PointTrack {
    /// The feature's position (media pixels) at `start`, where tracking begins.
    pub point: [f32; 2],
    pub start: Time,
    /// Side of the square patch that is matched, in media pixels.
    pub size: f32,
    /// How far the feature may move between frames, in media pixels.
    pub search: f32,
    /// Tracked positions, sorted by time.
    pub path: Vec<TrackSample>,
}

impl Default for PointTrack {
    fn default() -> Self {
        Self { point: [0.0, 0.0], start: Time::ZERO, size: 31.0, search: 24.0, path: Vec::new() }
    }
}

/// Matches below this are treated as the feature being lost.
pub const MIN_SCORE: f32 = 0.6;

impl PointTrack {
    /// The tracked position at source time `t` (between samples it's interpolated).
    pub fn pos_at(&self, t: Time) -> Option<[f32; 2]> {
        let p = &self.path;
        let i = p.partition_point(|s| s.at <= t);
        match (i.checked_sub(1).map(|j| &p[j]), p.get(i)) {
            (Some(a), _) if a.at == t => Some(a.pos),
            (Some(a), Some(b)) => {
                let k = (t - a.at).0 as f32 / (b.at - a.at).0.max(1) as f32;
                Some([a.pos[0] + (b.pos[0] - a.pos[0]) * k, a.pos[1] + (b.pos[1] - a.pos[1]) * k])
            }
            _ => None,
        }
    }

    /// Where to show the tracker at `t`: on the path if it covers `t`, else the start point.
    pub fn shown_at(&self, t: Time) -> [f32; 2] {
        self.pos_at(t).unwrap_or(self.point)
    }

    /// Put new samples in, replacing any old ones in the time they cover.
    pub fn merge(&mut self, mut samples: Vec<TrackSample>) {
        if samples.is_empty() {
            return;
        }
        samples.sort_by_key(|s| s.at);
        let (a, b) = (samples[0].at, samples[samples.len() - 1].at);
        self.path.retain(|s| s.at < a || s.at > b);
        self.path.extend(samples);
        self.path.sort_by_key(|s| s.at);
    }
}

/// A greyscale image.
pub struct Luma<'a> {
    pub data: &'a [f32],
    pub width: usize,
    pub height: usize,
}

impl Luma<'_> {
    fn at(&self, x: isize, y: isize) -> f32 {
        let x = x.clamp(0, self.width as isize - 1) as usize;
        let y = y.clamp(0, self.height as isize - 1) as usize;
        self.data[y * self.width + x]
    }
}

/// Rec. 709 luma of tightly packed RGBA8.
pub fn luma_of_rgba(rgba: &[u8]) -> Vec<f32> {
    rgba.chunks_exact(4).map(|p| 0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32).collect()
}

/// A patch to look for, taken around `centre` (rounded to whole pixels).
pub struct Template {
    half: isize,
    /// Mean-removed values, row by row.
    values: Vec<f32>,
    norm: f32,
}

impl Template {
    pub fn new(img: &Luma, centre: [f32; 2], size: f32) -> Self {
        let half = ((size.max(5.0) as isize) / 2).max(2);
        let (cx, cy) = (centre[0].round() as isize, centre[1].round() as isize);
        let mut values = Vec::with_capacity(((2 * half + 1) * (2 * half + 1)) as usize);
        for dy in -half..=half {
            for dx in -half..=half {
                values.push(img.at(cx + dx, cy + dy));
            }
        }
        let mean = values.iter().sum::<f32>() / values.len() as f32;
        values.iter_mut().for_each(|v| *v -= mean);
        let norm = values.iter().map(|v| v * v).sum::<f32>().sqrt();
        Self { half, values, norm }
    }

    /// Too flat to track (no texture to lock on to)?
    pub fn is_flat(&self) -> bool {
        self.norm / (self.values.len() as f32).sqrt() < 2.0
    }

    /// Normalised cross-correlation with the patch of `img` centred at `(cx, cy)`.
    fn ncc(&self, img: &Luma, cx: isize, cy: isize) -> f32 {
        let h = self.half;
        let n = self.values.len() as f32;
        let mut sum = 0.0;
        let mut sum2 = 0.0;
        let mut cross = 0.0;
        let mut i = 0;
        for dy in -h..=h {
            for dx in -h..=h {
                let v = img.at(cx + dx, cy + dy);
                sum += v;
                sum2 += v * v;
                cross += v * self.values[i];
                i += 1;
            }
        }
        // Σ(t'·(v − mean v)) = Σ t'·v, since Σ t' = 0.
        let var = (sum2 - sum * sum / n).max(0.0);
        let denom = self.norm * var.sqrt();
        if denom <= 1e-6 { 0.0 } else { cross / denom }
    }

    /// Find the patch in `img` within `search` pixels of `around`. Returns the
    /// sub-pixel position and the match score.
    pub fn find(&self, img: &Luma, around: [f32; 2], search: f32) -> ([f32; 2], f32) {
        let r = search.max(1.0) as isize;
        let (cx, cy) = (around[0].round() as isize, around[1].round() as isize);
        let w = (2 * r + 1) as usize;
        let mut scores = vec![f32::MIN; w * w];
        let mut best = (0, 0, f32::MIN);
        for dy in -r..=r {
            for dx in -r..=r {
                let s = self.ncc(img, cx + dx, cy + dy);
                scores[((dy + r) as usize) * w + (dx + r) as usize] = s;
                if s > best.2 {
                    best = (dx, dy, s);
                }
            }
        }
        let (bx, by, score) = best;
        let at = |dx: isize, dy: isize| -> Option<f32> {
            let (x, y) = (dx + r, dy + r);
            (x >= 0 && y >= 0 && x < w as isize && y < w as isize).then(|| scores[y as usize * w + x as usize])
        };
        // Fit a parabola through the best score and its neighbours on each axis.
        let refine = |l: Option<f32>, c: f32, rr: Option<f32>| match (l, rr) {
            (Some(l), Some(rr)) => {
                let d = l - 2.0 * c + rr;
                if d.abs() > 1e-6 { (0.5 * (l - rr) / d).clamp(-0.5, 0.5) } else { 0.0 }
            }
            _ => 0.0,
        };
        let ox = refine(at(bx - 1, by), score, at(bx + 1, by));
        let oy = refine(at(bx, by - 1), score, at(bx, by + 1));
        ([(cx + bx) as f32 + ox, (cy + by) as f32 + oy], score)
    }
}

/// Position keyframes that make `target` move with the feature `tracked`
/// follows (or, with `stabilise`, that move `tracked` itself against it, so the
/// feature stays still on screen). `frame` is the sequence frame size and
/// `layer` the tracked clip's picture size.
///
/// Motion is measured from the first tracked sample: the target keeps its
/// current position there and moves by however far the feature moved.
/// Position keys inside the tracked span are replaced; others are kept.
pub fn follow_keys(tracked: &Clip, track: &PointTrack, layer: [f32; 2], frame: [f32; 2], target: &Clip, stabilise: bool) -> Option<TransformAnim> {
    let first = track.path.first()?;
    let to_seq = |src: Time| tracked.timeline_start + (src - tracked.source_in);
    // The feature's position on the frame (the tracked layer's transform applies).
    let on_frame = |s: &TrackSample| {
        let tr = tracked.transform_at(to_seq(s.at));
        tr.matrix(frame).apply([s.pos[0] - layer[0] / 2.0, s.pos[1] - layer[1] / 2.0])
    };
    let origin = on_frame(first);
    let t0 = to_seq(first.at);
    let base = target.transform_at(t0.max(target.timeline_start).min(target.timeline_end())).position;
    let sign = if stabilise { -1.0 } else { 1.0 };

    let mut anim = target.anim.clone();
    let mut keys = Vec::new();
    for s in &track.path {
        let t = to_seq(s.at);
        if t < target.timeline_start || t > target.timeline_end() {
            continue;
        }
        let p = on_frame(s);
        let v = [base[0] + sign * (p[0] - origin[0]), base[1] + sign * (p[1] - origin[1])];
        keys.push((target.source_time(t), v));
    }
    let (a, b) = (keys.first()?.0, keys.last()?.0);
    anim.position.0.retain(|k| k.at < a || k.at > b);
    for (at, v) in keys {
        anim.position.set(at, v);
    }
    Some(anim)
}

/// Make a mask on the tracked clip itself move with the feature: shape keys
/// at every tracked frame, the shape as it is at the first tracked frame
/// shifted by how far the feature has moved since. (Both are in the clip's own
/// pixels, so the layer's transform doesn't come into it.)
pub fn follow_mask(mask: &crate::mask::Mask, track: &PointTrack) -> Option<crate::mask::Mask> {
    let first = track.path.first()?;
    let base = mask.points_at(first.at);
    let mut m = mask.clone();
    let (a, b) = (first.at, track.path.last()?.at);
    m.keys.retain(|k| k.at < a || k.at > b);
    for s in &track.path {
        let (dx, dy) = (s.pos[0] - first.pos[0], s.pos[1] - first.pos[1]);
        let pts = base.iter().map(|p| [p[0] + dx, p[1] + dy]).collect();
        let i = m.keys.partition_point(|k| k.at < s.at);
        m.keys.insert(i, crate::mask::ShapeKey { at: s.at, points: pts });
    }
    Some(m)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::{ClipId, MediaId};

    /// A smooth blob (a "feature") at `c` on a gently varying background.
    fn scene(w: usize, h: usize, c: [f32; 2], brightness: f32) -> Vec<f32> {
        let mut v = vec![0.0; w * h];
        for y in 0..h {
            for x in 0..w {
                let (dx, dy) = (x as f32 - c[0], y as f32 - c[1]);
                let blob = 200.0 * (-(dx * dx + dy * dy) / 18.0).exp();
                let corner = if dx > 0.0 && dy > 0.0 && dx < 6.0 && dy < 3.0 { 60.0 } else { 0.0 };
                v[y * w + x] = (20.0 + 0.1 * x as f32 + blob + corner) * brightness;
            }
        }
        v
    }

    #[test]
    fn finds_a_moved_feature_to_a_fraction_of_a_pixel() {
        let (w, h) = (120, 90);
        let a = scene(w, h, [40.0, 30.0], 1.0);
        let t = Template::new(&Luma { data: &a, width: w, height: h }, [40.0, 30.0], 21.0);
        assert!(!t.is_flat());
        // Moved by (7.3, -4.6) and 30% darker: still found.
        let b = scene(w, h, [47.3, 25.4], 0.7);
        let (p, score) = t.find(&Luma { data: &b, width: w, height: h }, [40.0, 30.0], 12.0);
        assert!(score > 0.9, "{score}");
        assert!((p[0] - 47.3).abs() < 0.35 && (p[1] - 25.4).abs() < 0.35, "{p:?}");
        // A flat patch can't be tracked.
        let flat = vec![50.0; w * h];
        assert!(Template::new(&Luma { data: &flat, width: w, height: h }, [60.0, 45.0], 21.0).is_flat());
    }

    #[test]
    fn masks_follow_the_track() {
        let s = Time::from_secs_f64;
        let track = PointTrack {
            path: vec![TrackSample { at: s(1.0), pos: [10.0, 10.0], score: 1.0 }, TrackSample { at: s(2.0), pos: [30.0, 5.0], score: 1.0 }],
            ..Default::default()
        };
        let mask = crate::mask::Mask { points: vec![[0.0, 0.0], [4.0, 0.0], [4.0, 4.0]], ..Default::default() };
        let m = follow_mask(&mask, &track).unwrap();
        assert_eq!(m.keys.len(), 2);
        assert_eq!(m.points_at(s(2.0))[1], [24.0, -5.0]);
        assert_eq!(m.points_at(s(1.5))[0], [10.0, -2.5]);
    }

    #[test]
    fn interpolates_and_merges_paths() {
        let s = |t: f64, x: f32| TrackSample { at: Time::from_secs_f64(t), pos: [x, 0.0], score: 1.0 };
        let mut tr = PointTrack { path: vec![s(0.0, 0.0), s(1.0, 10.0)], ..Default::default() };
        assert_eq!(tr.pos_at(Time::from_secs_f64(0.25)), Some([2.5, 0.0]));
        assert_eq!(tr.pos_at(Time::from_secs_f64(2.0)), None);
        tr.merge(vec![s(1.0, 20.0), s(2.0, 30.0)]);
        assert_eq!(tr.path.iter().map(|s| s.pos[0]).collect::<Vec<_>>(), [0.0, 20.0, 30.0]);
    }

    #[test]
    fn follow_and_stabilise_keys() {
        let secs = Time::from_secs_f64;
        // Tracked clip at 2 s on the timeline, from 1 s into its media, unscaled.
        let tracked = Clip::new(ClipId(1), MediaId(1), secs(2.0), secs(1.0), secs(4.0), None);
        let track = PointTrack {
            path: vec![
                TrackSample { at: secs(1.0), pos: [100.0, 50.0], score: 1.0 },
                TrackSample { at: secs(2.0), pos: [130.0, 40.0], score: 1.0 },
            ],
            ..Default::default()
        };
        let (layer, frame) = ([640.0, 360.0], [640.0, 360.0]);
        // A title from 0 s at position (5, 5) follows: +30, -10 a second later (3 s on the timeline).
        let mut target = Clip::new(ClipId(2), MediaId::GENERATED, secs(0.0), secs(0.0), secs(10.0), None);
        target.transform.position = [5.0, 5.0];
        let anim = follow_keys(&tracked, &track, layer, frame, &target, false).unwrap();
        let keys: Vec<(Time, [f32; 2])> = anim.position.0.iter().map(|k| (k.at, k.value)).collect();
        assert_eq!(keys, vec![(secs(2.0), [5.0, 5.0]), (secs(3.0), [35.0, -5.0])]);
        // Stabilising the tracked clip moves it the other way, keyed in its own source time.
        let anim = follow_keys(&tracked, &track, layer, frame, &tracked, true).unwrap();
        let keys: Vec<(Time, [f32; 2])> = anim.position.0.iter().map(|k| (k.at, k.value)).collect();
        assert_eq!(keys, vec![(secs(1.0), [0.0, 0.0]), (secs(2.0), [-30.0, 10.0])]);
        // At double scale the motion on screen is doubled too.
        let mut big = tracked.clone();
        big.transform.scale = [2.0, 2.0];
        let anim = follow_keys(&big, &track, layer, frame, &target, false).unwrap();
        assert_eq!(anim.position.0[1].value, [65.0, -15.0]);
    }
}
