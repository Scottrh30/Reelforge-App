//! Motion tracking: follow small features (corners, marks, lights) through a
//! clip, then make another layer move with them, hold the clip itself steady,
//! or pin a layer into a moving four-sided shape (a screen, a sign).
//!
//! A [`Tracker`] has one, two or four points ([`TrackMode`]):
//! - one point gives movement (position);
//! - two points also give rotation and scale (the line between them turning
//!   and changing length);
//! - four points give a perspective view of a flat surface, for a corner pin.
//!
//! Each point compares a square patch around its feature with every position
//! in a search area of the next frame, using normalised cross-correlation (so
//! brightness changes don't throw it off), and refines the best match to a
//! fraction of a pixel. Large search areas are searched coarse-to-fine on an
//! image pyramid, starting where the feature's last movement predicts it. It
//! works on luma only.

use crate::anim::TransformAnim;
use crate::project::Clip;
use crate::time::Time;
use crate::transform::{Homography, Pin};
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

/// The feature box and search area new trackers get on footage this size:
/// the defaults suit 720p, and grow with the picture (by its short side, up
/// to four times); on 4K a 31-pixel box sits inside a pupil, with nothing
/// to lock on to.
pub fn footage_sizes(footage: [f32; 2]) -> (f32, f32) {
    let k = (footage[0].min(footage[1]) / 720.0).clamp(1.0, 4.0);
    let d = PointTrack::default();
    ((d.size * k).round(), (d.search * k).round())
}

impl Tracker {
    /// Boxes still at the old defaults (31 and 24 px, as every tracker had
    /// before boxes were sized for the footage) grown to suit `footage`:
    /// returns whether they were. Boxes someone has set are left alone.
    pub fn fit_old_defaults(&mut self, footage: [f32; 2]) -> bool {
        let d = PointTrack::default();
        let (size, search) = footage_sizes(footage);
        if self.mode == TrackMode::Planar || self.mode == TrackMode::Mesh || size <= d.size || !self.points.iter().all(|p| p.size == d.size && p.search == d.search) {
            return false;
        }
        self.set_sizes(size, search);
        true
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

    /// Does the tracked path cover source time `t`?
    pub fn covers(&self, t: Time) -> bool {
        matches!((self.path.first(), self.path.last()), (Some(a), Some(b)) if a.at <= t && t <= b.at)
    }

    /// Move the feature at source time `t` to `pos`: on the tracked path, a
    /// correction at that frame (later frames stay as they are until tracked
    /// again from there); elsewhere, a new starting point.
    pub fn move_to(&mut self, t: Time, pos: [f32; 2]) {
        if self.covers(t) {
            match self.path.iter_mut().find(|s| s.at == t) {
                Some(s) => {
                    s.pos = pos;
                    s.score = 1.0;
                }
                None => {
                    let i = self.path.partition_point(|s| s.at < t);
                    self.path.insert(i, TrackSample { at: t, pos, score: 1.0 });
                }
            }
        } else {
            self.point = pos;
            self.start = t;
        }
    }

    /// [`Self::move_to`], with a correction on the tracked path eased into
    /// the `span` tracked frames on each side (fully at `t`, fading to
    /// nothing past `span`), so the path doesn't jump at that frame.
    pub fn move_blended(&mut self, t: Time, pos: [f32; 2], span: usize) {
        if span == 0 || !self.covers(t) {
            return self.move_to(t, pos);
        }
        let before = self.pos_at(t).unwrap_or(pos);
        self.move_to(t, pos);
        let Some(i) = self.path.iter().position(|s| s.at == t) else { return };
        let d = [pos[0] - before[0], pos[1] - before[1]];
        for k in 1..=span {
            // Raised cosine: 1 at the frame itself, 0 at span + 1 away.
            let w = 0.5 * (1.0 + (std::f32::consts::PI * k as f32 / (span + 1) as f32).cos());
            for j in [i.checked_sub(k), Some(i + k)].into_iter().flatten() {
                if let Some(s) = self.path.get_mut(j) {
                    s.pos = [s.pos[0] + d[0] * w, s.pos[1] + d[1] * w];
                }
            }
        }
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
#[derive(Clone, Copy)]
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

    /// Bilinear sample at a fractional position (pixel centres on whole numbers).
    fn sample(&self, x: f32, y: f32) -> f32 {
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        let (x0, y0) = (x0 as isize, y0 as isize);
        let top = self.at(x0, y0) * (1.0 - fx) + self.at(x0 + 1, y0) * fx;
        let bottom = self.at(x0, y0 + 1) * (1.0 - fx) + self.at(x0 + 1, y0 + 1) * fx;
        top * (1.0 - fy) + bottom * fy
    }
}

/// Rec. 709 luma of tightly packed RGBA8.
pub fn luma_of_rgba(rgba: &[u8]) -> Vec<f32> {
    rgba.chunks_exact(4).map(|p| 0.2126 * p[0] as f32 + 0.7152 * p[1] as f32 + 0.0722 * p[2] as f32).collect()
}

/// A patch to look for, taken around `centre` (rounded to whole pixels).
#[derive(Clone)]
pub struct Template {
    half: isize,
    /// Weight of each pixel: highest in the middle, falling off towards the
    /// edges, so the feature itself counts most and its surroundings (which
    /// shift most as it turns or changes size) least.
    weights: Vec<f32>,
    /// Weighted, mean-removed values, row by row: w·(t − mean).
    values: Vec<f32>,
    /// Sum of the weights.
    wsum: f32,
    norm: f32,
    /// How far the feature is from the whole pixel the patch is centred on, so
    /// a refreshed patch doesn't lose (and slowly drift by) the fraction.
    frac: [f32; 2],    /// The patch's own values (for template matching).
    raw: Vec<f32>,
    /// How it's compared with the picture.
    kind: TrackMatch,
}

impl Template {
    pub fn new(img: &Luma, centre: [f32; 2], size: f32) -> Self {
        let half = ((size.max(5.0) as isize) / 2).max(2);
        let (cx, cy) = (centre[0].round() as isize, centre[1].round() as isize);
        let n = ((2 * half + 1) * (2 * half + 1)) as usize;
        let sigma = 0.55 * half as f32;
        let mut raw = Vec::with_capacity(n);
        let mut weights = Vec::with_capacity(n);
        for dy in -half..=half {
            for dx in -half..=half {
                raw.push(img.at(cx + dx, cy + dy));
                weights.push((-((dx * dx + dy * dy) as f32) / (2.0 * sigma * sigma)).exp());
            }
        }
        let wsum: f32 = weights.iter().sum();
        let mean = raw.iter().zip(&weights).map(|(v, w)| v * w).sum::<f32>() / wsum;
        let values: Vec<f32> = raw.iter().zip(&weights).map(|(v, w)| w * (v - mean)).collect();
        // Σ w·(t − mean)²
        let norm = raw.iter().zip(&weights).map(|(v, w)| w * (v - mean) * (v - mean)).sum::<f32>().sqrt();
        Self { half, weights, values, wsum, norm, frac: [centre[0] - cx as f32, centre[1] - cy as f32], raw, kind: TrackMatch::Correlation }
    }

    /// The patch around `centre` of `img`, as it would look after the local
    /// linear map whose inverse is `inv` (rows of a 2×2: an offset in the new
    /// frame times `inv` is the offset in `img`). Used to compare a feature
    /// with how it looked at the start, turned, scaled and tilted the way the
    /// tracked surface has been since.
    pub fn warped(img: &Luma, centre: [f32; 2], size: f32, inv: [[f32; 2]; 2]) -> Self {
        let half = ((size.max(5.0) as isize) / 2).max(2);
        let sigma = 0.55 * half as f32;
        let n = ((2 * half + 1) * (2 * half + 1)) as usize;
        let (mut raw, mut weights) = (Vec::with_capacity(n), Vec::with_capacity(n));
        for dy in -half..=half {
            for dx in -half..=half {
                let (fx, fy) = (dx as f32, dy as f32);
                let ox = inv[0][0] * fx + inv[0][1] * fy;
                let oy = inv[1][0] * fx + inv[1][1] * fy;
                raw.push(img.sample(centre[0] + ox, centre[1] + oy));
                weights.push((-(fx * fx + fy * fy) / (2.0 * sigma * sigma)).exp());
            }
        }
        let wsum: f32 = weights.iter().sum();
        let mean = raw.iter().zip(&weights).map(|(v, w)| v * w).sum::<f32>() / wsum;
        let values: Vec<f32> = raw.iter().zip(&weights).map(|(v, w)| w * (v - mean)).collect();
        let norm = raw.iter().zip(&weights).map(|(v, w)| w * (v - mean) * (v - mean)).sum::<f32>().sqrt();
        // Sampled at the exact centre, so there's no fraction to carry.
        Self { half, weights, values, wsum, norm, frac: [0.0, 0.0], raw, kind: TrackMatch::Correlation }
    }

    /// Too flat to track (no texture to lock on to)?
    pub fn is_flat(&self) -> bool {
        self.norm / self.wsum.sqrt() < 2.0
    }

    /// Compared with the picture this way.
    pub fn matching(mut self, kind: TrackMatch) -> Self {
        self.kind = kind;
        self
    }

    /// How well the patch of `img` centred at `(cx, cy)` matches: 1 exactly,
    /// less the less alike.
    fn score(&self, img: &Luma, cx: isize, cy: isize) -> f32 {
        match self.kind {
            TrackMatch::Correlation => self.ncc(img, cx, cy),
            TrackMatch::Template => self.sad(img, cx, cy),
        }
    }

    /// Template matching: the weighted mean difference of the values
    /// themselves (not their pattern), as a score: 1 the same, 0 a mean
    /// difference of [`TEMPLATE_RANGE`] levels or more.
    fn sad(&self, img: &Luma, cx: isize, cy: isize) -> f32 {
        let h = self.half;
        let mut diff = 0.0;
        let mut i = 0;
        for dy in -h..=h {
            for dx in -h..=h {
                let (Some(w), Some(t)) = (self.weights.get(i), self.raw.get(i)) else { return 0.0 };
                diff += w * (img.at(cx + dx, cy + dy) - t).abs();
                i += 1;
            }
        }
        (1.0 - diff / self.wsum.max(1e-6) / TEMPLATE_RANGE).max(0.0)
    }

    /// Weighted normalised cross-correlation with the patch of `img` centred at `(cx, cy)`.
    fn ncc(&self, img: &Luma, cx: isize, cy: isize) -> f32 {
        let h = self.half;
        let mut sum = 0.0;
        let mut sum2 = 0.0;
        let mut cross = 0.0;
        let mut i = 0;
        let inside = cx - h >= 0 && cy - h >= 0 && cx + h < img.width as isize && cy + h < img.height as isize;
        for dy in -h..=h {
            if inside {
                // The fast path: a straight run of the row, no edge clamping.
                let row = (cy + dy) as usize * img.width + (cx - h) as usize;
                let len = (2 * h + 1) as usize;
                for (k, v) in img.data[row..row + len].iter().enumerate() {
                    let w = self.weights[i + k];
                    sum += w * v;
                    sum2 += w * v * v;
                    cross += v * self.values[i + k];
                }
                i += len;
            } else {
                for dx in -h..=h {
                    let v = img.at(cx + dx, cy + dy);
                    let w = self.weights[i];
                    sum += w * v;
                    sum2 += w * v * v;
                    cross += v * self.values[i];
                    i += 1;
                }
            }
        }
        // Σ w·t'·(v − mean v) = Σ w·t'·v, since Σ w·t' = 0.
        let var = (sum2 - sum * sum / self.wsum).max(0.0);
        let denom = self.norm * var.sqrt();
        if denom <= 1e-6 { 0.0 } else { cross / denom }
    }

    /// Find the patch in `img` within `search` pixels of `around`. Returns the
    /// sub-pixel position and the match score.
    pub fn find(&self, img: &Luma, around: [f32; 2], search: f32) -> ([f32; 2], f32) {
        let r = search.max(1.0) as isize;
        // Search for the patch's centre pixel; the feature is `frac` from it.
        let (cx, cy) = ((around[0] - self.frac[0]).round() as isize, (around[1] - self.frac[1]).round() as isize);
        let w = (2 * r + 1) as usize;
        let mut scores = vec![f32::MIN; w * w];
        let mut best = (0, 0, f32::MIN);
        for dy in -r..=r {
            for dx in -r..=r {
                let s = self.score(img, cx + dx, cy + dy);
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
        ([(cx + bx) as f32 + ox + self.frac[0], (cy + by) as f32 + oy + self.frac[1]], score)
    }
}


/// Template matching: a mean difference of this many levels (of 255) scores 0.
const TEMPLATE_RANGE: f32 = 40.0;

/// How a tracker compares a feature with each frame.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TrackMatch {
    /// Normalised correlation: the feature's pattern of light and dark,
    /// whatever its brightness and contrast, and its look refreshed as it
    /// goes, so it follows features that fade, flicker or slowly turn.
    #[default]
    Correlation,
    /// Template match: the feature's exact values, as first seen, never
    /// refreshed: no drift on a sharp, unchanging feature, but it's lost when
    /// the light on it changes.
    Template,
}

impl TrackMatch {
    pub const ALL: [TrackMatch; 2] = [TrackMatch::Correlation, TrackMatch::Template];

    pub fn label(self) -> &'static str {
        match self {
            TrackMatch::Correlation => "Correlation (copes with light changes)",
            TrackMatch::Template => "Template match (exact look, no drift)",
        }
    }
}

/// Below this search radius (pixels), search exhaustively at full resolution.
const DIRECT_SEARCH: f32 = 16.0;
/// Search radius at each finer pyramid level (around the coarser estimate).
const REFINE: f32 = 3.0;
/// The original patch is trusted over the latest one above this match score.
const ORIGINAL_SCORE: f32 = 0.85;
/// How much wider than its search area a feature carried along unseen is
/// looked for, at most.
const MAX_WIDEN: f32 = 3.0;
/// A feature found again after being lost matches at least this well (the
/// wider search finds more look-alikes).
pub const REFIND_SCORE: f32 = 0.75;
/// The patch is refreshed only where the original look also matches this
/// well…
const REFRESH_SCORE: f32 = 0.6;
/// …within this many pixels of where the refreshed patch says it is.
const REFRESH_DRIFT: f32 = 1.5;
/// …and the original warped by the group's motion above this one.
const WARPED_SCORE: f32 = 0.8;

/// A frame at full, half, quarter… resolution (box-filtered), for searching
/// big areas quickly.
pub struct Pyramid {
    levels: Vec<(Vec<f32>, usize, usize)>,
}

impl Pyramid {
    /// `levels` = how many halvings to keep (0 = just the frame).
    pub fn new(data: Vec<f32>, width: usize, height: usize, levels: usize) -> Self {
        let mut v = vec![(data, width, height)];
        for _ in 0..levels {
            let Some((prev, w, h)) = v.last() else { break };
            if *w < 16 || *h < 16 {
                break;
            }
            let (nw, nh) = (w / 2, h / 2);
            let mut d = vec![0.0; nw * nh];
            for y in 0..nh {
                for x in 0..nw {
                    let (a, b) = ((2 * y) * w + 2 * x, (2 * y + 1) * w + 2 * x);
                    d[y * nw + x] = 0.25 * (prev[a] + prev[a + 1] + prev[b] + prev[b + 1]);
                }
            }
            v.push((d, nw, nh));
        }
        Self { levels: v }
    }

    /// Halvings needed so a `search` radius is small at the coarsest level.
    pub fn levels_for(search: f32) -> usize {
        let mut l = 0;
        while search / (1 << l) as f32 > DIRECT_SEARCH * 0.75 && l < 3 {
            l += 1;
        }
        if search <= DIRECT_SEARCH { 0 } else { l }
    }

    pub fn depth(&self) -> usize {
        self.levels.len()
    }

    pub fn level(&self, i: usize) -> Luma<'_> {
        let (d, w, h) = &self.levels[i.min(self.levels.len() - 1)];
        Luma { data: d, width: *w, height: *h }
    }
}

/// Follows one feature from frame to frame: its patch at each pyramid level,
/// where it is, and how it moved last (to predict the next frame).
pub struct FeatureTracker {
    templates: Vec<Template>,
    /// The feature as it looked where tracking started (full resolution). While
    /// it still matches well it has the last word on position, so small errors
    /// don't pile up frame after frame.
    original: Template,
    /// The same at half, quarter… resolution, so it can be looked for over a
    /// wider area: a track that has drifted off it is pulled back once the
    /// feature looks as it did at the start again (after a blink, a turn).
    originals: Vec<Template>,
    size: f32,
    search: f32,
    pub pos: [f32; 2],
    velocity: [f32; 2],
    matching: TrackMatch,
    /// The search area grows while the feature is carried along unseen (see
    /// [`FeatureTracker::coast`]), so it can be found again where it's got to.
    widen: f32,
    /// Held to its original look (see [`FeatureTracker::anchored`]).
    anchor: bool,
}

impl FeatureTracker {
    /// Start following the feature at `pos` (pixels of `pyr`'s full level).
    pub fn new(pyr: &Pyramid, pos: [f32; 2], size: f32, search: f32) -> Self {
        Self::new_matching(pyr, pos, size, search, TrackMatch::Correlation)
    }

    /// [`FeatureTracker::new`], comparing it with each frame as `matching` says.
    pub fn new_matching(pyr: &Pyramid, pos: [f32; 2], size: f32, search: f32, matching: TrackMatch) -> Self {
        let original = Template::new(&pyr.level(0), pos, size.max(9.0)).matching(matching);
        let originals = (1..pyr.depth().min(3))
            .map(|l| {
                let k = (1 << l) as f32;
                Template::new(&pyr.level(l), [pos[0] / k, pos[1] / k], (size / k).max(9.0)).matching(matching)
            })
            .collect();
        let mut t = Self { templates: Vec::new(), original, originals, size, search, pos, velocity: [0.0, 0.0], matching, widen: 1.0, anchor: false };
        t.capture(pyr);
        t
    }

    fn levels(&self, pyr: &Pyramid) -> usize {
        (Pyramid::levels_for(self.search) + 1).min(pyr.depth())
    }

    /// Take the feature's patches from `pyr` around the current position.
    fn capture(&mut self, pyr: &Pyramid) {
        self.templates = (0..self.levels(pyr))
            .map(|l| {
                let k = (1 << l) as f32;
                Template::new(&pyr.level(l), [self.pos[0] / k, self.pos[1] / k], (self.size / k).max(9.0)).matching(self.matching)
            })
            .collect();
    }

    /// Too flat to track?
    pub fn is_flat(&self) -> bool {
        self.templates[0].is_flat()
    }

    /// Expect the feature at `p` in the next frame (the next search starts there).
    pub fn aim(&mut self, p: [f32; 2]) {
        self.velocity = [p[0] - self.pos[0], p[1] - self.pos[1]];
    }

    /// How far from where it's expected it's looked for now (pixels of the
    /// full level): its search area, wider while it's been unseen.
    pub fn reach(&self) -> f32 {
        self.search * self.widen
    }

    /// Found again after being carried along unseen: back to its usual search area.
    pub fn found(&mut self) {
        self.widen = 1.0;
    }

    /// How it moved last frame (pixels of the full level).
    pub fn velocity(&self) -> [f32; 2] {
        self.velocity
    }

    /// Carry the feature to `p`, moving by `d` a frame, without having found
    /// it there (it's hidden or blurred for a moment): its look is kept, and
    /// the next frame searches around where it's expected.
    pub fn coast(&mut self, p: [f32; 2], d: [f32; 2]) {
        self.pos = p;
        self.velocity = d;
        self.widen = (self.widen + 0.25).min(MAX_WIDEN);
    }

    /// Put the feature at `p` (e.g. where a surface it lies on says it is),
    /// forgetting its last movement.
    pub fn place(&mut self, p: [f32; 2]) {
        self.pos = p;
        self.velocity = [0.0, 0.0];
    }

    /// Search around `around`: coarse levels first, each finer one refining.
    fn search_from(&self, pyr: &Pyramid, around: [f32; 2]) -> ([f32; 2], f32) {
        let top = self.templates.len() - 1;
        let k = (1 << top) as f32;
        let (mut p, mut score) = self.templates[top].find(&pyr.level(top), [around[0] / k, around[1] / k], (self.search * self.widen / k).ceil());
        for l in (0..top).rev() {
            let up = [p[0] * 2.0, p[1] * 2.0];
            (p, score) = self.templates[l].find(&pyr.level(l), up, REFINE);
        }
        (p, score)
    }

    /// Held to its original look: searched for over a quarter of the
    /// feature's size each frame, it pulls back a track that's crept off it,
    /// and the patch is only refreshed where it agrees (drift correction).
    /// For features that keep their look (eyes, marks on a face or a prop)
    /// over long shots; not for ones that keep turning away from it.
    pub fn anchored(mut self) -> Self {
        self.anchor = true;
        self
    }

    /// The original look searched for round `p`, coarse to fine: within a
    /// quarter of the feature's size (at least [`REFINE`] pixels).
    fn find_original(&self, pyr: &Pyramid, p: [f32; 2]) -> ([f32; 2], f32) {
        let radius = (self.size * 0.25).max(REFINE);
        // The coarsest level that has the original, and where the radius is small.
        let mut top = 0;
        while top < self.originals.len() && top + 1 < pyr.depth() && radius / (1 << top) as f32 > REFINE * 2.0 {
            top += 1;
        }
        let k = (1 << top) as f32;
        let at = |l: usize| if l == 0 { Some(&self.original) } else { self.originals.get(l - 1) };
        let Some(t) = at(top) else { return (p, 0.0) };
        let (mut q, mut score) = t.find(&pyr.level(top), [p[0] / k, p[1] / k], (radius / k).ceil());
        for l in (0..top).rev() {
            let Some(t) = at(l) else { break };
            (q, score) = t.find(&pyr.level(l), [q[0] * 2.0, q[1] * 2.0], REFINE);
        }
        (q, score)
    }

    /// Re-find the feature with its original look warped by `local` (the
    /// surface's motion around it since the start, as a 2×2 linear map) taken
    /// from `start` (the first frame) at `start_pos`. If that matches well, it
    /// replaces this frame's position: with several points the group's motion
    /// is known, so the original stays a valid reference however far the
    /// feature turns, and small errors can't pile up. Returns the new score.
    pub fn correct(&mut self, start: &Luma, start_pos: [f32; 2], local: [[f32; 2]; 2], pyr: &Pyramid) -> Option<f32> {
        let det = local[0][0] * local[1][1] - local[0][1] * local[1][0];
        if det.abs() < 1e-4 {
            return None;
        }
        let inv = [[local[1][1] / det, -local[0][1] / det], [-local[1][0] / det, local[0][0] / det]];
        let t = Template::warped(start, start_pos, self.size.max(9.0), inv);
        let (q, score) = t.find(&pyr.level(0), self.pos, REFINE);
        if score < WARPED_SCORE || (q[0] - self.pos[0]).abs() >= REFINE || (q[1] - self.pos[1]).abs() >= REFINE {
            return None;
        }
        let d = [q[0] - self.pos[0], q[1] - self.pos[1]];
        self.velocity = [self.velocity[0] + d[0], self.velocity[1] + d[1]];
        self.pos = q;
        Some(score)
    }

    /// Find the feature in the next frame. Returns its position and match
    /// score; the tracker moves there unless the score is below `MIN_SCORE`.
    pub fn step(&mut self, pyr: &Pyramid) -> ([f32; 2], f32) {
        let predicted = [self.pos[0] + self.velocity[0], self.pos[1] + self.velocity[1]];
        let (mut p, mut score) = self.search_from(pyr, predicted);
        if score < 0.9 && self.velocity != [0.0, 0.0] {
            // The motion changed: look again around where it was.
            let (q, s) = self.search_from(pyr, self.pos);
            if s > score {
                (p, score) = (q, s);
            }
        }
        if score >= MIN_SCORE {
            if self.anchor {
                // Pin it down with the original look when that fits as well:
                // looked for round here (a quarter of the feature's size), so a
                // track that has crept off it comes back.
                let (q, s0) = self.find_original(pyr, p);
                if s0 >= ORIGINAL_SCORE && s0 + 0.03 >= score {
                    p = q;
                    score = score.max(s0);
                }
            } else {
                // Pin it down with the original look when that still fits closely.
                let (q, s0) = self.original.find(&pyr.level(0), p, REFINE);
                if s0 >= ORIGINAL_SCORE && (q[0] - p[0]).abs() < REFINE && (q[1] - p[1]).abs() < REFINE {
                    p = q;
                    score = score.max(s0);
                }
            }
            self.velocity = [p[0] - self.pos[0], p[1] - self.pos[1]];
            self.pos = p;
            // Follow gradual changes (turning, light) by refreshing the patch, but
            // only from good matches, so one poor frame doesn't make it wander
            // off; anchored, only where the original look, searched for from
            // here, lands on the same spot (drift correction): a patch refreshed
            // a pixel or two off would carry that error into every frame after.
            // (Template matching keeps the look it started with.)
            if score > 0.9 && self.matching == TrackMatch::Correlation {
                let refresh = !self.anchor || {
                    let (q0, s1) = self.original.find(&pyr.level(0), p, REFINE);
                    s1 >= REFRESH_SCORE && (q0[0] - p[0]).hypot(q0[1] - p[1]) <= REFRESH_DRIFT
                };
                if refresh {
                    self.capture(pyr);
                }
            }
        }
        (p, score)
    }
}

/// How many points a tracker has, and so what motion it can measure.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TrackMode {
    /// One point: movement.
    #[default]
    Position,
    /// Two points: movement, rotation and scale.
    Transform,
    /// Four points around a flat surface: its perspective, for a corner pin.
    CornerPin,
    /// The four corners of a flat surface, followed by tracking the whole area
    /// inside them (see [`crate::planar`]): the corners themselves needn't
    /// have any detail, or even be in the picture.
    Planar,
    /// A grid of points over a surface that bends (cloth, skin, a flag), see
    /// [`crate::meshtrack`]: the four outer corners are placed, the grid fills
    /// them, and every point moves with what's under it.
    Mesh,
}

impl TrackMode {
    pub const ALL: [TrackMode; 5] = [TrackMode::Position, TrackMode::Transform, TrackMode::CornerPin, TrackMode::Planar, TrackMode::Mesh];

    /// Points for this mode (a mesh: with the default 4 × 4 grid).
    pub fn points(self) -> usize {
        match self {
            TrackMode::Position => 1,
            TrackMode::Transform => 2,
            TrackMode::CornerPin | TrackMode::Planar => 4,
            TrackMode::Mesh => (DEFAULT_GRID as usize + 1) * (DEFAULT_GRID as usize + 1),
        }
    }

    /// Does it measure a surface's perspective (four corners, for a corner pin)?
    pub fn is_perspective(self) -> bool {
        matches!(self, TrackMode::CornerPin | TrackMode::Planar)
    }

    pub fn label(self) -> &'static str {
        match self {
            TrackMode::Position => "Position (1 point)",
            TrackMode::Transform => "Position, rotation and scale (2 points)",
            TrackMode::CornerPin => "Corner pin / perspective (4 points)",
            TrackMode::Planar => "Planar: a whole flat surface",
            TrackMode::Mesh => "Mesh: a surface that bends",
        }
    }
}

/// Cells across and down a new mesh tracker.
pub const DEFAULT_GRID: u8 = 4;

/// A clip's motion tracker: its points (each tracked on its own, but always
/// together) and what they measure.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(from = "TrackerRepr")]
pub struct Tracker {
    pub mode: TrackMode,
    /// `mode.points()` points. For a corner pin they're the top-left,
    /// top-right, bottom-right and bottom-left of the surface. For a mesh,
    /// the grid's points row by row from the top left.
    pub points: Vec<PointTrack>,
    /// A mesh's cells across and down.
    #[serde(default, skip_serializing_if = "is_no_grid")]
    pub grid: [u8; 2],
    /// How much the tracked paths are smoothed (Gaussian, in frames) wherever
    /// the motion is used. The tracked samples themselves are kept as they are.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub smooth: f32,
    /// How its points are compared with each frame.
    #[serde(default, skip_serializing_if = "is_correlation")]
    pub matching: TrackMatch,
}

fn is_correlation(m: &TrackMatch) -> bool {
    *m == TrackMatch::Correlation
}

fn is_zero(v: &f32) -> bool {
    *v == 0.0
}

fn is_no_grid(g: &[u8; 2]) -> bool {
    *g == [0, 0]
}

/// Projects saved before multi-point tracking had a single `PointTrack` here.
#[derive(Deserialize)]
#[serde(untagged)]
enum TrackerRepr {
    Multi {
        #[serde(default)]
        mode: TrackMode,
        points: Vec<PointTrack>,
        #[serde(default)]
        grid: [u8; 2],
        #[serde(default)]
        smooth: f32,
        #[serde(default)]
        matching: TrackMatch,
    },
    Single(PointTrack),
}

impl From<TrackerRepr> for Tracker {
    fn from(r: TrackerRepr) -> Self {
        let mut t = match r {
            TrackerRepr::Multi { mode, points, grid, smooth, matching } => Tracker { mode, points, grid, smooth: smooth.clamp(0.0, MAX_SMOOTH), matching },
            TrackerRepr::Single(p) => Tracker { mode: TrackMode::Position, points: vec![p], grid: [0, 0], smooth: 0.0, matching: TrackMatch::Correlation },
        };
        if t.points.is_empty() {
            t.points.push(PointTrack::default());
        }
        if t.mode == TrackMode::Mesh && t.grid[0] == 0 {
            t.grid = [DEFAULT_GRID, DEFAULT_GRID];
        }
        let n = t.point_count();
        let first = t.points[0].clone();
        t.points.resize(n, first);
        t
    }
}

impl From<PointTrack> for Tracker {
    fn from(p: PointTrack) -> Self {
        Tracker { mode: TrackMode::Position, points: vec![p], grid: [0, 0], smooth: 0.0, matching: TrackMatch::Correlation }
    }
}

impl Tracker {
    /// A new tracker at source time `start`, centred on `centre` (media pixels)
    /// of a `layer`-sized picture.
    pub fn new(mode: TrackMode, centre: [f32; 2], layer: [f32; 2], start: Time) -> Self {
        let (size, search) = footage_sizes(layer);
        let first = PointTrack { point: centre, start, size, search, ..Default::default() };
        // A frame's smoothing to start with: it takes out the tracking's
        // jitter without dragging behind real movement (and can be changed).
        let mut t = Tracker { mode: TrackMode::Position, points: vec![first], grid: [0, 0], smooth: 1.0, matching: TrackMatch::Correlation };
        t.set_mode(mode, layer, start);
        if mode == TrackMode::Mesh {
            // A grid filling the box around the centre.
            let q = Self::layout(TrackMode::Planar, centre, layer);
            t.fill_mesh([q[0], q[1], q[2], q[3]], start);
        } else if mode != TrackMode::Position {
            // Spread out around the centre, not with one point on it.
            let pts = Self::layout(mode, centre, layer);
            for (p, q) in t.points.iter_mut().zip(pts) {
                p.point = q;
            }
        }
        t
    }

    /// Where new points go for `mode`, around `c`.
    fn layout(mode: TrackMode, c: [f32; 2], layer: [f32; 2]) -> Vec<[f32; 2]> {
        let (dx, dy) = (layer[0] / 8.0, layer[1] / 8.0);
        let clamp = |p: [f32; 2]| [p[0].clamp(0.0, layer[0]), p[1].clamp(0.0, layer[1])];
        match mode {
            TrackMode::Position => vec![c],
            TrackMode::Transform => vec![clamp([c[0] - dx, c[1]]), clamp([c[0] + dx, c[1]])],
            TrackMode::CornerPin | TrackMode::Planar | TrackMode::Mesh => vec![[c[0] - dx, c[1] - dy], [c[0] + dx, c[1] - dy], [c[0] + dx, c[1] + dy], [c[0] - dx, c[1] + dy]].into_iter().map(clamp).collect(),
        }
    }

    /// Switch mode, keeping the points there are and adding (or dropping) the
    /// rest. Added points sit around the first one, at source time `t`.
    pub fn set_mode(&mut self, mode: TrackMode, layer: [f32; 2], t: Time) {
        if self.mode == TrackMode::Mesh && mode != TrackMode::Mesh {
            // Keep the mesh's outer corners, in order, as the first points.
            let c = self.mesh_corners();
            let pts = std::mem::take(&mut self.points);
            self.points = c.iter().map(|&i| pts[i].clone()).collect();
            self.grid = [0, 0];
        }
        if mode == TrackMode::Mesh && self.mode != TrackMode::Mesh {
            // A grid filling the four corners (or a box around the first point).
            let shown = self.shown_at(t);
            let quad = if shown.len() >= 4 { [shown[0], shown[1], shown[2], shown[3]] } else {
                let q = Self::layout(TrackMode::Planar, shown[0], layer);
                [q[0], q[1], q[2], q[3]]
            };
            self.mode = mode;
            self.grid = [DEFAULT_GRID, DEFAULT_GRID];
            self.fill_mesh(quad, t);
            return;
        }
        self.mode = mode;
        let n = mode.points();
        if self.points.len() > n {
            self.points.truncate(n);
            return;
        }
        let c = self.points[0].shown_at(t);
        let spots = Self::layout(mode, c, layer);
        let template = self.points[0].clone();
        while self.points.len() < n {
            let i = self.points.len();
            self.points.push(PointTrack { point: spots[i], start: t, path: Vec::new(), ..template.clone() });
        }
    }

    /// How many points it has (a mesh's depend on its grid).
    pub fn point_count(&self) -> usize {
        match self.mode {
            TrackMode::Mesh => (self.grid[0].max(1) as usize + 1) * (self.grid[1].max(1) as usize + 1),
            m => m.points(),
        }
    }

    /// A mesh's outer corner points: top left, top right, bottom right, bottom left.
    pub fn mesh_corners(&self) -> [usize; 4] {
        let (c, r) = (self.grid[0].max(1) as usize, self.grid[1].max(1) as usize);
        [0, c, (r + 1) * (c + 1) - 1, r * (c + 1)]
    }

    /// Lay a mesh's points out as an even grid filling `quad`, starting at
    /// source time `t` (forgetting any tracked paths: a new grid is a new track).
    pub fn fill_mesh(&mut self, quad: [[f32; 2]; 4], t: Time) {
        let (c, r) = (self.grid[0].max(1), self.grid[1].max(1));
        let m = crate::meshwarp::MeshShape::filling(quad, c, r);
        let template = PointTrack { path: Vec::new(), start: t, ..self.points[0].clone() };
        self.points = m.points.iter().map(|&p| PointTrack { point: p, ..template.clone() }).collect();
    }

    /// Before tracking: move mesh corner `point` to `pos` at source time `t`,
    /// carrying the rest of the grid along with the four-sided shape its
    /// corners make, so inner points placed by hand keep their places in it
    /// (and clicking a corner without moving it changes nothing).
    pub fn move_mesh_corner(&mut self, point: usize, pos: [f32; 2], t: Time) {
        let corners = self.mesh_corners();
        let Some(k) = corners.iter().position(|&i| i == point) else { return };
        let shown = self.shown_at(t);
        if shown.len() != self.points.len() || corners.iter().any(|&i| i >= shown.len()) {
            return;
        }
        let old = corners.map(|i| shown[i]);
        let mut new = old;
        new[k] = pos;
        let cols = self.grid[0].max(1) as usize;
        let rows = self.grid[1].max(1) as usize;
        for (n, p) in self.points.iter_mut().enumerate() {
            let guess = [(n % (cols + 1)) as f32 / cols as f32, (n / (cols + 1)) as f32 / rows as f32];
            let to = match inverse_bilinear(old, shown[n], guess) {
                Some(uv) => bilinear(new, uv),
                // A folded shape: put it where an even grid would.
                None => bilinear(new, guess),
            };
            p.point = to;
            p.start = t;
        }
    }

    /// Change a mesh's grid (cells across and down), keeping its outer corners at `t`.
    pub fn set_grid(&mut self, cols: u8, rows: u8, t: Time) {
        if self.mode != TrackMode::Mesh {
            return;
        }
        let shown = self.shown_at(t);
        let quad = self.mesh_corners().map(|i| shown[i]);
        self.grid = [cols.clamp(1, crate::meshwarp::MAX_CELLS), rows.clamp(1, crate::meshwarp::MAX_CELLS)];
        self.fill_mesh(quad, t);
    }

    /// Where each point is shown at source time `t`.
    pub fn shown_at(&self, t: Time) -> Vec<[f32; 2]> {
        self.points.iter().map(|p| p.shown_at(t)).collect()
    }

    pub fn is_tracked(&self) -> bool {
        self.points.iter().any(|p| !p.path.is_empty())
    }

    /// The frames where every point has been tracked, with their positions
    /// (media pixels), in source time order, smoothed by [`Self::smooth`].
    pub fn frames(&self) -> Vec<(Time, Vec<[f32; 2]>)> {
        let mut f = self.raw_frames();
        smooth_paths(&mut f, self.smooth);
        f
    }

    /// [`Self::frames`] as tracked, without smoothing.
    pub fn raw_frames(&self) -> Vec<(Time, Vec<[f32; 2]>)> {
        let Some(first) = self.points.first() else { return Vec::new() };
        first
            .path
            .iter()
            .filter_map(|s| {
                let mut v = vec![s.pos];
                for p in &self.points[1..] {
                    v.push(p.pos_at(s.at)?);
                }
                Some((s.at, v))
            })
            .collect()
    }

    /// Set the size and search area of every point.
    pub fn set_sizes(&mut self, size: f32, search: f32) {
        for p in &mut self.points {
            p.size = size;
            p.search = search;
        }
    }

    pub fn clear_paths(&mut self) {
        for p in &mut self.points {
            p.path.clear();
        }
    }
}

/// The motion from the first tracked frame to frame `pts`, both given as the
/// points' positions: a shift for one point, a similarity (rotation, uniform
/// scale, shift) for two, a perspective map for four.
pub fn motion_between(mode: TrackMode, from: &[[f32; 2]], to: &[[f32; 2]]) -> Option<Homography> {
    match mode {
        TrackMode::Position => {
            let (a, b) = (from.first()?, to.first()?);
            Some(Homography([1.0, 0.0, (b[0] - a[0]) as f64, 0.0, 1.0, (b[1] - a[1]) as f64, 0.0, 0.0, 1.0]))
        }
        TrackMode::Transform => {
            let ([a0, b0], [a1, b1]) = ([from.first()?, from.get(1)?], [to.first()?, to.get(1)?]);
            let (u, v) = ([b0[0] - a0[0], b0[1] - a0[1]], [b1[0] - a1[0], b1[1] - a1[1]]);
            let len = (u[0] * u[0] + u[1] * u[1]).sqrt();
            if len < 1e-3 {
                return None;
            }
            let k = ((v[0] * v[0] + v[1] * v[1]).sqrt() / len) as f64;
            let angle = (v[1].atan2(v[0]) - u[1].atan2(u[0])) as f64;
            let (s, c) = angle.sin_cos();
            let (a, b) = (k * c, k * s);
            // Rotate and scale about the first point, then move it to its new place.
            let (x0, y0, x1, y1) = (a0[0] as f64, a0[1] as f64, a1[0] as f64, a1[1] as f64);
            Some(Homography([a, -b, x1 - (a * x0 - b * y0), b, a, y1 - (b * x0 + a * y0), 0.0, 0.0, 1.0]))
        }
        // A mesh bends: there's no one map for it (see `follow_mesh`).
        TrackMode::Mesh => None,
        TrackMode::CornerPin | TrackMode::Planar => {
            let f: [[f32; 2]; 4] = from.get(..4)?.try_into().ok()?;
            let t: [[f32; 2]; 4] = to.get(..4)?.try_into().ok()?;
            if !Pin(f).is_valid() || !Pin(t).is_valid() {
                return None;
            }
            Homography::from_quads(&f, &t)
        }
    }
}

/// Where an energy blade's hilt and tip (`ends`, in the tracked clip's media
/// pixels, as placed at media time `reference`) go at each tracked frame,
/// moving with the tracked points. Mesh tracks aren't supported (`None`).
///
/// One point moves the blade; two points also turn and stretch it (rotation
/// and scale about the points' middle); corner pins and planar tracks carry
/// it in perspective. Left-out `axes` keep the blade's own value for that
/// part. Returns (media time, [hilt, tip]) per tracked frame.
pub fn follow_blade(tracker: &Tracker, ends: [[f32; 2]; 2], reference: Time, axes: Axes) -> Option<Vec<(Time, [[f32; 2]; 2])>> {
    if tracker.mode == TrackMode::Mesh {
        return None;
    }
    let frames = tracker.frames();
    // The tracked frame nearest the reference time.
    let ref_pts = frames.iter().min_by_key(|(t, _)| (t.0 - reference.0).abs())?.1.clone();
    let mid = |p: &[[f32; 2]]| {
        let n = p.len().max(1) as f32;
        p.iter().fold([0.0, 0.0], |a, q| [a[0] + q[0] / n, a[1] + q[1] / n])
    };
    let c0 = mid(&ref_pts);
    let out: Vec<_> = frames
        .iter()
        .filter_map(|(t, pts)| {
            let h = motion_between(tracker.mode, &ref_pts, pts)?;
            let c1 = mid(pts);
            let d = axes.mask([c1[0] - c0[0], c1[1] - c0[1]]);
            let moved = match tracker.mode {
                TrackMode::CornerPin | TrackMode::Planar => {
                    // In perspective, then back by the left-out part of the surface's move.
                    let full = [c1[0] - c0[0], c1[1] - c0[1]];
                    ends.map(|p| {
                        let q = h.apply(p);
                        [q[0] - (full[0] - d[0]), q[1] - (full[1] - d[1])]
                    })
                }
                _ => {
                    let sim = Similar::of(&h);
                    let angle = if tracker.mode == TrackMode::Transform && axes.rotation { sim.angle.to_radians() as f32 } else { 0.0 };
                    let k = if tracker.mode == TrackMode::Transform && axes.scale { sim.scale as f32 } else { 1.0 };
                    let (sn, cs) = angle.sin_cos();
                    ends.map(|p| {
                        let (x, y) = (p[0] - c0[0], p[1] - c0[1]);
                        [c0[0] + k * (cs * x - sn * y) + d[0], c0[1] + k * (sn * x + cs * y) + d[1]]
                    })
                }
            };
            Some((*t, moved))
        })
        .collect();
    (!out.is_empty()).then_some(out)
}

/// The 2×2 linear map that `h` is close to around point `p` (its derivative).
pub fn local_linear(h: &Homography, p: [f32; 2]) -> [[f32; 2]; 2] {
    let e = 0.5;
    let (xp, xm) = (h.apply([p[0] + e, p[1]]), h.apply([p[0] - e, p[1]]));
    let (yp, ym) = (h.apply([p[0], p[1] + e]), h.apply([p[0], p[1] - e]));
    [[(xp[0] - xm[0]) / (2.0 * e), (yp[0] - ym[0]) / (2.0 * e)], [(xp[1] - xm[1]) / (2.0 * e), (yp[1] - ym[1]) / (2.0 * e)]]
}

/// A similarity map, as its parts: rotation (degrees), uniform scale and the
/// shift of the frame origin.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Similar {
    angle: f64,
    scale: f64,
    shift: [f64; 2],
}

impl Similar {
    fn of(h: &Homography) -> Similar {
        let m = &h.0;
        let (a, b) = (m[0] / m[8], m[3] / m[8]);
        Similar { angle: b.atan2(a).to_degrees(), scale: (a * a + b * b).sqrt(), shift: [m[2] / m[8], m[5] / m[8]] }
    }
    fn homography(&self) -> Homography {
        let (s, c) = self.angle.to_radians().sin_cos();
        let (a, b) = (self.scale * c, self.scale * s);
        Homography([a, -b, self.shift[0], b, a, self.shift[1], 0.0, 0.0, 1.0])
    }
}

/// The most smoothing a tracker can have, in frames.
pub const MAX_SMOOTH: f32 = 30.0;

/// Gaussian-weighted local straight-line fit (`sigma` in samples): shake is
/// averaged out, but a steady drift is kept exactly, also at the ends of the
/// run, where a plain average would pull the path back.
fn smooth_trend(v: &[f64], sigma: f64) -> Vec<f64> {
    let n = v.len() as isize;
    if sigma <= 0.0 || n < 3 {
        return v.to_vec();
    }
    let r = (3.0 * sigma).ceil() as isize;
    (0..n)
        .map(|i| {
            // Weighted least squares of v ≈ a + b·(k − i) over the window; the value is a.
            let (mut sw, mut sx, mut sxx, mut sy, mut sxy) = (0.0, 0.0, 0.0, 0.0, 0.0);
            for k in (i - r).max(0)..=(i + r).min(n - 1) {
                let x = (k - i) as f64;
                let w = (-(x * x) / (2.0 * sigma * sigma)).exp();
                let y = v[k as usize];
                sw += w;
                sx += w * x;
                sxx += w * x * x;
                sy += w * y;
                sxy += w * x * y;
            }
            let det = sw * sxx - sx * sx;
            if det.abs() < 1e-9 { sy / sw } else { (sy * sxx - sx * sxy) / det }
        })
        .collect()
}

/// Smooth every point's path through `frames` over `sigma` frames. Runs of
/// frames with a gap between them (a stretch that wasn't tracked) are
/// smoothed separately, so nothing is averaged across the gap.
fn smooth_paths(frames: &mut [(Time, Vec<[f32; 2]>)], sigma: f32) {
    if sigma <= 0.0 || frames.len() < 3 {
        return;
    }
    // The usual step between frames; a step over 1.5× that starts a new run.
    let mut steps: Vec<i64> = frames.windows(2).map(|w| (w[1].0 .0 - w[0].0 .0).abs()).filter(|d| *d > 0).collect();
    steps.sort_unstable();
    let step = steps.get(steps.len() / 2).copied().unwrap_or(1).max(1);
    let mut start = 0;
    for i in 1..=frames.len() {
        let gap = i == frames.len() || (frames[i].0 .0 - frames[i - 1].0 .0).abs() * 2 > step * 3;
        if !gap {
            continue;
        }
        let run = &mut frames[start..i];
        let points = run.first().map_or(0, |f| f.1.len());
        for k in 0..points {
            for c in 0..2 {
                let raw: Vec<f64> = run.iter().map(|f| f.1[k][c] as f64).collect();
                for (f, v) in run.iter_mut().zip(smooth_trend(&raw, sigma as f64)) {
                    f.1[k][c] = v as f32;
                }
            }
        }
        start = i;
    }
}

/// Gaussian smoothing of a sequence (`sigma` in samples), with the ends held.
fn smooth(v: &[f64], sigma: f64) -> Vec<f64> {
    if sigma <= 0.0 || v.len() < 3 {
        return v.to_vec();
    }
    let r = (3.0 * sigma).ceil() as isize;
    let n = v.len() as isize;
    (0..n)
        .map(|i| {
            let (mut sum, mut wsum) = (0.0, 0.0);
            for j in -r..=r {
                let k = (i + j).clamp(0, n - 1) as usize;
                let w = (-(j * j) as f64 / (2.0 * sigma * sigma)).exp();
                sum += w * v[k];
                wsum += w;
            }
            sum / wsum
        })
        .collect()
}

/// Smooth a run of similarity motions (each from the reference frame) by
/// their parts: the camera's intended path, without the shake.
fn smooth_motions(m: &[Homography], frames: f32) -> Vec<Homography> {
    let parts: Vec<Similar> = m.iter().map(Similar::of).collect();
    // Unwrap the angles so a turn through ±180° doesn't average to nonsense.
    let mut angles: Vec<f64> = Vec::with_capacity(parts.len());
    for p in &parts {
        let a = match angles.last() {
            Some(&prev) => prev + ((p.angle - prev + 180.0).rem_euclid(360.0) - 180.0),
            None => p.angle,
        };
        angles.push(a);
    }
    let sigma = frames as f64 / 2.0;
    let a = smooth(&angles, sigma);
    let k = smooth(&parts.iter().map(|p| p.scale.max(1e-6).ln()).collect::<Vec<_>>(), sigma);
    let x = smooth(&parts.iter().map(|p| p.shift[0]).collect::<Vec<_>>(), sigma);
    let y = smooth(&parts.iter().map(|p| p.shift[1]).collect::<Vec<_>>(), sigma);
    (0..m.len()).map(|i| Similar { angle: a[i], scale: k[i].exp(), shift: [x[i], y[i]] }.homography()).collect()
}

/// How a tracker's motion is put on a layer.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ApplyOptions {
    /// Move the tracked clip itself against the motion (stabilise) rather than
    /// making another layer follow it.
    pub stabilise: bool,
    /// For stabilising with one or two points: keep slow, deliberate camera
    /// moves and only take out the shake, smoothing over about this many
    /// frames. 0 locks the shot completely.
    pub smoothing: f32,
    /// Which parts of the motion are applied. Left out, that part of the
    /// layer stays as it was.
    pub axes: Axes,
    /// Stabilising: zoom in to hide the edges the counter-motion uncovers.
    pub zoom: StabZoom,
}

impl Default for ApplyOptions {
    fn default() -> Self {
        Self { stabilise: false, smoothing: 0.0, axes: Axes::ALL, zoom: StabZoom::Off }
    }
}

/// How a stabilised clip hides the edges its counter-motion uncovers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum StabZoom {
    /// Not at all: the gaps show.
    #[default]
    Off,
    /// One zoom for the whole clip: just enough for its worst moment.
    Fixed,
    /// Zoom that changes with the motion (eased over about this many frames
    /// as the smoothing, a second at least): less zoom where the shake is
    /// small, never letting an edge show.
    Follow,
}

/// The least zoom (about the frame's centre, at least 1) at which `quad`
/// (where the picture lands on the frame) covers the whole `frame`.
pub fn cover_zoom(quad: &[[f32; 2]; 4], frame: [f32; 2]) -> f32 {
    let c = [frame[0] / 2.0, frame[1] / 2.0];
    // (A hair inside, so a picture exactly the frame's size counts as covering it.)
    let e = 0.01;
    let corners = [[e, e], [frame[0] - e, e], [frame[0] - e, frame[1] - e], [e, frame[1] - e]];
    // Zooming the picture by s about c covers corner F when c + (F − c)/s is inside it.
    let covers = |s: f32| corners.iter().all(|f| crate::planar::inside(quad, [c[0] + (f[0] - c[0]) / s, c[1] + (f[1] - c[1]) / s]));
    if covers(1.0) {
        return 1.0;
    }
    let (mut lo, mut hi) = (1.0f32, 2.0f32);
    while !covers(hi) {
        hi *= 2.0;
        if hi > 64.0 {
            return 64.0; // the picture doesn't reach the centre: nothing sensible
        }
    }
    for _ in 0..30 {
        let mid = (lo + hi) / 2.0;
        if covers(mid) {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    hi
}

/// Per-frame zooms `need` (each the least that frame needs) as zooms to use
/// over `radius` frames: a moving maximum, then a moving average no wider, so
/// every frame still gets at least what it needs.
fn eased_zoom(need: &[f32], radius: usize) -> Vec<f32> {
    let n = need.len();
    let max: Vec<f32> = (0..n).map(|i| need[i.saturating_sub(radius)..(i + radius + 1).min(n)].iter().fold(1.0f32, |a, &b| a.max(b))).collect();
    (0..n)
        .map(|i| {
            let w = &max[i.saturating_sub(radius)..(i + radius + 1).min(n)];
            // Near the ends the window is short on one side: never below this frame's own need.
            (w.iter().sum::<f32>() / w.len() as f32).max(need[i])
        })
        .collect()
}

/// The parts of a tracked motion to apply: movement across (X) and down (Y),
/// and, for two-point tracks, rotation and scale. For corner pins, planar
/// and mesh tracks, X and Y are the surface's movement as a whole (its shape
/// still follows).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Axes {
    pub x: bool,
    pub y: bool,
    pub rotation: bool,
    pub scale: bool,
}

impl Axes {
    pub const ALL: Axes = Axes { x: true, y: true, rotation: true, scale: true };

    /// The shift `d` with the parts left out set to 0.
    fn mask(&self, d: [f32; 2]) -> [f32; 2] {
        [if self.x { d[0] } else { 0.0 }, if self.y { d[1] } else { 0.0 }]
    }

    /// `pts` moved back by the left-out part of how far their middle has
    /// moved from `reference`'s.
    fn hold(&self, reference: &[[f32; 2]], pts: &mut [[f32; 2]]) {
        if self.x && self.y {
            return;
        }
        let mid = |p: &[[f32; 2]]| {
            let n = p.len().max(1) as f32;
            p.iter().fold([0.0, 0.0], |a, q| [a[0] + q[0] / n, a[1] + q[1] / n])
        };
        let (a, b) = (mid(reference), mid(pts));
        let d = [b[0] - a[0], b[1] - a[1]];
        let keep = self.mask(d);
        for p in pts.iter_mut() {
            p[0] -= d[0] - keep[0];
            p[1] -= d[1] - keep[1];
        }
    }
}

/// Keyframes that make `target` move with what `tracker` (on `tracked`)
/// followed, or, with `opts.stabilise`, that move `tracked` itself against
/// it. Returns the target's new animation.
///
/// `frame` is the sequence frame size; `layer` and `target_layer` the two
/// clips' picture sizes.
///
/// - One or two points, target not corner-pinned: position keys (and, with
///   two points, rotation and scale). The target keeps its own look at the
///   first tracked frame and moves (turns, grows) as the feature does since.
/// - Four points: corner-pin keys. A target without a pin of its own is
///   pinned straight into the four tracked points (screen replacement); a
///   target that already has one (say, adjusted by hand at the first tracked
///   frame) keeps that shape and moves in perspective with the surface.
/// - A corner-pinned target followed by one or two points has its pin moved.
///
/// Keys of the written properties inside the tracked span are replaced;
/// others are kept.
pub fn follow_keys(tracked: &Clip, tracker: &Tracker, layer: [f32; 2], frame: [f32; 2], target: &Clip, target_layer: [f32; 2], opts: ApplyOptions) -> Option<TransformAnim> {
    // Samples are in media time; where each shows on the timeline depends on the clip's speed.
    let to_seq = |src: Time| tracked.timeline_of_media(src).unwrap_or(tracked.timeline_start);
    // The points' positions on the frame (the tracked layer's own transform applies).
    let frames: Vec<(Time, Vec<[f32; 2]>)> = tracker
        .frames()
        .into_iter()
        .map(|(src, pts)| {
            let t = to_seq(src);
            let m = tracked.transform_at(t).layer_map(layer, frame);
            (t, pts.iter().map(|p| m.apply([p[0] - layer[0] / 2.0, p[1] - layer[1] / 2.0])).collect())
        })
        .filter(|(t, _)| *t >= target.timeline_start && *t <= target.timeline_end())
        .collect();
    let (t0, ref_pts) = frames.first().map(|(t, p)| (*t, p.clone()))?;
    // Motion since the reference frame, frame by frame (frames where it can't be
    // worked out, like a corner pin squashed flat, are left out).
    let motions: Vec<(Time, Vec<[f32; 2]>, Homography)> = frames.iter().filter_map(|(t, p)| Some((*t, p.clone(), motion_between(tracker.mode, &ref_pts, p)?))).collect();
    if motions.is_empty() {
        return None;
    }
    let raw: Vec<Homography> = motions.iter().map(|m| m.2).collect();
    // What to apply at each frame: the motion, or (stabilising) its undoing,
    // leaving any smoothed camera path in.
    let applied: Vec<Homography> = if !opts.stabilise {
        raw
    } else {
        let smoothed = (opts.smoothing > 0.0 && !tracker.mode.is_perspective()).then(|| smooth_motions(&raw, opts.smoothing));
        raw.iter()
            .enumerate()
            .map(|(i, m)| {
                let inv = m.inverse().unwrap_or(Homography::IDENTITY);
                match &smoothed {
                    Some(s) => s[i].then_after(&inv),
                    None => inv,
                }
            })
            .collect()
    };

    let clamp_t = |t: Time| t.max(target.timeline_start).min(target.timeline_end());
    // The look being moved: the target's at the reference frame, or (stabilising)
    // the clip's own at each frame.
    let base_at = |t: Time| if opts.stabilise { target.transform_at(clamp_t(t)) } else { target.transform_at(clamp_t(t0)) };
    let pinned = target.transform_at(clamp_t(t0)).valid_pin().is_some();
    let use_pin = pinned || tracker.mode.is_perspective();
    // Straight into the tracked points: a corner pin following onto an unpinned layer.
    let into_points = tracker.mode.is_perspective() && !opts.stabilise && !pinned;
    let c = [frame[0] / 2.0, frame[1] / 2.0];

    let mut anim = target.anim.clone();
    let mut pos_keys = Vec::new();
    let mut rot_keys = Vec::new();
    let mut scale_keys = Vec::new();
    let mut pin_keys = Vec::new();
    let mut last_angle: Option<f32> = None;
    let axes = opts.axes;
    for ((t, pts, _), h) in motions.iter().zip(&applied) {
        let at = target.source_time(*t);
        let base = base_at(*t);
        if use_pin {
            // Where the pin would be without any motion (to hold left-out axes against).
            let (still, mut pin) = if into_points {
                (Pin([ref_pts[0], ref_pts[1], ref_pts[2], ref_pts[3]]), Pin([pts[0], pts[1], pts[2], pts[3]]))
            } else {
                let q = base.valid_pin().unwrap_or_else(|| base.pin_of_transform(target_layer, frame));
                (q, q.map(|p| h.apply(p)))
            };
            axes.hold(&still.0, &mut pin.0);
            if pin.is_valid() {
                pin_keys.push((at, pin));
            }
            continue;
        }
        let sim = Similar::of(h);
        let anchor_at = [c[0] + base.position[0], c[1] + base.position[1]];
        let moved = h.apply(anchor_at);
        if axes.x || axes.y {
            // A left-out axis keeps the layer's own (animated) value.
            let own = target.transform_at(clamp_t(*t)).position;
            let p = [moved[0] - c[0], moved[1] - c[1]];
            pos_keys.push((at, [if axes.x { p[0] } else { own[0] }, if axes.y { p[1] } else { own[1] }]));
        }
        if tracker.mode == TrackMode::Transform {
            // Keep the angle continuous from frame to frame.
            let mut a = base.rotation + sim.angle as f32;
            if let Some(prev) = last_angle {
                a = prev + ((a - prev + 180.0).rem_euclid(360.0) - 180.0);
            }
            last_angle = Some(a);
            if axes.rotation {
                rot_keys.push((at, a));
            }
            if axes.scale {
                scale_keys.push((at, [base.scale[0] * sim.scale as f32, base.scale[1] * sim.scale as f32]));
            }
        }
    }

    fn put<T: crate::anim::Lerp>(curve: &mut crate::anim::Curve<T>, keys: Vec<(Time, T)>) {
        // (A reversed clip lists them newest first.)
        let (Some(a), Some(b)) = (keys.iter().map(|k| k.0).min(), keys.iter().map(|k| k.0).max()) else { return };
        curve.0.retain(|k| k.at < a || k.at > b);
        for (at, v) in keys {
            curve.set(at, v);
        }
    }
    if use_pin && pin_keys.is_empty() || !use_pin && pos_keys.is_empty() && rot_keys.is_empty() && scale_keys.is_empty() {
        return None;
    }
    put(&mut anim.position, pos_keys);
    put(&mut anim.rotation, rot_keys);
    put(&mut anim.scale, scale_keys);
    put(&mut anim.pin, pin_keys);
    if opts.stabilise && opts.zoom != StabZoom::Off {
        zoom_to_cover(&mut anim, target, target_layer, frame, &motions.iter().map(|m| m.0).collect::<Vec<_>>(), use_pin, opts);
    }
    Some(anim)
}

/// Zoom a stabilised clip's keys (about the frame's centre) so its picture
/// covers the frame at every tracked moment `times` (see [`StabZoom`]).
pub fn zoom_to_cover(anim: &mut TransformAnim, target: &Clip, layer: [f32; 2], frame: [f32; 2], times: &[Time], pinned: bool, opts: ApplyOptions) {
    let moved = Clip { anim: anim.clone(), ..target.clone() };
    let at = |t: Time| moved.transform_at(t.max(target.timeline_start).min(target.timeline_end()));
    let need: Vec<f32> = times.iter().map(|&t| cover_zoom(&at(t).corners(layer, frame), frame)).collect();
    let zoom: Vec<f32> = match opts.zoom {
        StabZoom::Off => return,
        StabZoom::Fixed => vec![need.iter().fold(1.0f32, |a, &b| a.max(b)); need.len()],
        StabZoom::Follow => eased_zoom(&need, (opts.smoothing.round() as usize).max(25)),
    };
    let c = [frame[0] / 2.0, frame[1] / 2.0];
    for (&t, &z) in times.iter().zip(&zoom) {
        let src = target.source_time(t);
        let tr = at(t);
        if pinned {
            if let Some(pin) = tr.valid_pin() {
                anim.pin.set(src, pin.map(|p| [c[0] + (p[0] - c[0]) * z, c[1] + (p[1] - c[1]) * z]));
            }
        } else {
            // About the frame's centre: the offset from it and the size both grow.
            anim.position.set(src, [tr.position[0] * z, tr.position[1] * z]);
            anim.scale.set(src, [tr.scale[0] * z, tr.scale[1] * z]);
        }
    }
}

/// A mesh warp laying `target` over a mesh tracker's grid at every tracked
/// frame it overlaps (its keys outside that range are kept). `layer` is the
/// tracked clip's picture size and `frame` the sequence's.
pub fn follow_mesh(tracked: &Clip, tracker: &Tracker, layer: [f32; 2], frame: [f32; 2], target: &Clip, axes: Axes) -> Option<crate::meshwarp::MeshWarp> {
    if tracker.mode != TrackMode::Mesh {
        return None;
    }
    let to_seq = |src: Time| tracked.timeline_of_media(src).unwrap_or(tracked.timeline_start);
    let mut reference: Option<Vec<[f32; 2]>> = None;
    let mut keys: Vec<(Time, crate::meshwarp::MeshShape)> = tracker
        .frames()
        .into_iter()
        .filter_map(|(src, pts)| {
            let t = to_seq(src);
            if t < target.timeline_start || t > target.timeline_end() {
                return None;
            }
            // From the tracked clip's pixels to the frame, through its own transform then.
            let m = tracked.transform_at(t).layer_map(layer, frame);
            let mut points: Vec<[f32; 2]> = pts.iter().map(|p| m.apply([p[0] - layer[0] / 2.0, p[1] - layer[1] / 2.0])).collect();
            let r = reference.get_or_insert_with(|| points.clone());
            axes.hold(r, &mut points);
            let shape = crate::meshwarp::MeshShape { cols: tracker.grid[0], rows: tracker.grid[1], points };
            (shape.is_valid() && !shape.is_folded()).then(|| (target.source_time(t), shape))
        })
        .collect();
    keys.sort_by_key(|k| k.0);
    keys.dedup_by_key(|k| k.0);
    let (a, b) = (keys.first()?.0, keys.last()?.0);
    let mut w = target.mesh.clone().unwrap_or_default();
    // A different grid can't be blended with: start again.
    if w.keys.first().is_some_and(|k| k.1.cols != tracker.grid[0] || k.1.rows != tracker.grid[1]) {
        w.keys.clear();
    }
    w.keys.retain(|k| k.0 < a || k.0 > b);
    w.keys.extend(keys);
    w.keys.sort_by_key(|k| k.0);
    Some(w)
}

/// Make a mask on the tracked clip itself move with the tracked points: shape
/// keys at every tracked frame, the shape as it is at the first tracked frame
/// moved (turned, scaled, put in perspective, by mode) as the points have
/// since. (Both are in the clip's own pixels, so the layer's transform doesn't
/// come into it.)
pub fn follow_mask(clip: &Clip, mask: &crate::mask::Mask, tracker: &Tracker) -> Option<crate::mask::Mask> {
    // Tracked samples are in media time; mask keys are in the clip's own time.
    let frames: Vec<(Time, Vec<[f32; 2]>)> = tracker.frames().into_iter().filter_map(|(src, p)| Some((clip.clip_time_of_media(src)?, p))).collect();
    let (t0, ref_pts) = frames.first().cloned()?;
    let base = mask.points_at(t0);
    let curves = mask.curves_at(t0);
    let curved = mask.is_curved(t0);
    // Each point and (for curves) both its handle ends move with the track; the
    // handles are worked out again from where they land.
    let mut anchors: Vec<[f32; 2]> = base.clone();
    if curved {
        for (p, c) in base.iter().zip(&curves) {
            anchors.push([p[0] + c[0], p[1] + c[1]]);
            anchors.push([p[0] + c[2], p[1] + c[3]]);
        }
    }
    let n = base.len();
    let split = |moved: Vec<[f32; 2]>| -> (Vec<[f32; 2]>, Vec<[f32; 4]>) {
        let pts: Vec<[f32; 2]> = moved[..n].to_vec();
        let cs = if curved {
            (0..n)
                .map(|i| {
                    let (p, a, b) = (pts[i], moved[n + 2 * i], moved[n + 2 * i + 1]);
                    [a[0] - p[0], a[1] - p[1], b[0] - p[0], b[1] - p[1]]
                })
                .collect()
        } else {
            Vec::new()
        };
        (pts, cs)
    };
    let mut m = mask.clone();
    let keys: Vec<(Time, Vec<[f32; 2]>)> = if tracker.mode == TrackMode::Mesh {
        // Each mask point keeps its place on the bending grid.
        let grid = |pts: &[[f32; 2]]| crate::meshwarp::MeshShape { cols: tracker.grid[0], rows: tracker.grid[1], points: pts.to_vec() };
        let reference = grid(&ref_pts);
        let at: Vec<crate::meshwarp::MeshCoord> = anchors.iter().map(|q| reference.locate(*q)).collect::<Option<_>>()?;
        frames.iter().map(|(t, p)| (*t, at.iter().map(|c| grid(p).apply(*c)).collect())).collect()
    } else {
        frames
            .iter()
            .filter_map(|(at, p)| {
                let h = motion_between(tracker.mode, &ref_pts, p)?;
                Some((*at, anchors.iter().map(|q| h.apply(*q)).collect()))
            })
            .collect()
    };
    let (a, b) = (keys.iter().map(|k| k.0).min()?, keys.iter().map(|k| k.0).max()?);
    m.keys.retain(|k| k.at < a || k.at > b);
    for (at, moved) in keys {
        let (points, curves) = split(moved);
        let i = m.keys.partition_point(|k| k.at < at);
        m.keys.insert(i, crate::mask::ShapeKey { at, points, curves });
    }
    Some(m)
}

/// A shape key from moved anchors: `n` points, then (when `curved`) both
/// handle ends of each, turned back into handles.
fn key_from(moved: &[[f32; 2]], n: usize, curved: bool, at: Time) -> crate::mask::ShapeKey {
    let points = moved[..n].to_vec();
    let curves = if curved {
        (0..n)
            .map(|k| {
                let (p, a, b) = (points[k], moved[n + 2 * k], moved[n + 2 * k + 1]);
                [a[0] - p[0], a[1] - p[1], b[0] - p[0], b[1] - p[1]]
            })
            .collect()
    } else {
        Vec::new()
    };
    crate::mask::ShapeKey { at, points, curves }
}

/// How a tracked mask's shape may move (see [`mask_from_region`]).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum MaskMotion {
    /// Moves only.
    Position,
    /// Moves, turns and grows or shrinks.
    #[default]
    Transform,
    /// Also tilts in perspective (a flat thing seen at a changing angle).
    Perspective,
    /// Bends: each part of the shape follows what's under it (cloth, skin,
    /// hands), tracked on a grid of [`MASK_GRID`] × [`MASK_GRID`] cells.
    Bend,
}

/// Cells across and down the grid a bending mask is tracked on.
pub const MASK_GRID: u8 = 4;

/// The grid a bending mask is tracked on: its bounds (`quad`) grown by a
/// tenth each way, so the shape's edge isn't on the grid's.
pub fn mask_grid(quad: [[f32; 2]; 4]) -> crate::meshwarp::MeshShape {
    let (w, h) = (quad[1][0] - quad[0][0], quad[3][1] - quad[0][1]);
    let (dx, dy) = (w * 0.1, h * 0.1);
    let grown = [[quad[0][0] - dx, quad[0][1] - dy], [quad[1][0] + dx, quad[1][1] - dy], [quad[2][0] + dx, quad[2][1] + dy], [quad[3][0] - dx, quad[3][1] + dy]];
    crate::meshwarp::MeshShape::filling(grown, MASK_GRID, MASK_GRID)
}

/// The four corners of a mask's outline's bounds at clip time `t`, for
/// tracking what's inside it.
pub fn mask_bounds(mask: &crate::mask::Mask, t: Time) -> Option<[[f32; 2]; 4]> {
    let o = mask.outline(t);
    if o.len() < 3 {
        return None;
    }
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for p in &o {
        x0 = x0.min(p[0]);
        y0 = y0.min(p[1]);
        x1 = x1.max(p[0]);
        y1 = y1.max(p[1]);
    }
    (x1 - x0 >= 4.0 && y1 - y0 >= 4.0).then_some([[x0, y0], [x1, y0], [x1, y1], [x0, y1]])
}

/// Rotoscoping: shape keys for `mask` (on `clip`) at every frame of a
/// tracked region. `corners` are the region's four corners as tracked (one
/// list of samples per corner, in media time, all on the same frames; the
/// first sample is where tracking started, with the mask as it is then) or,
/// for [`MaskMotion::Bend`], the points of its [`mask_grid`].
/// The shape there is carried along, moved as `motion` allows. Keys the mask
/// already had inside the tracked span are replaced; others are kept.
pub fn mask_from_region(clip: &Clip, mask: &crate::mask::Mask, corners: &[Vec<TrackSample>], motion: MaskMotion) -> Option<crate::mask::Mask> {
    let n_grid = (MASK_GRID as usize + 1) * (MASK_GRID as usize + 1);
    if corners.len() != if motion == MaskMotion::Bend { n_grid } else { 4 } {
        return None;
    }
    let frames = corners[0].len();
    if frames < 2 || corners.iter().any(|c| c.len() != frames) {
        return None;
    }
    let quad = |i: usize| -> [[f32; 2]; 4] { std::array::from_fn(|k| corners[k][i].pos) };
    let grid = |i: usize| crate::meshwarp::MeshShape { cols: MASK_GRID, rows: MASK_GRID, points: corners.iter().map(|c| c[i].pos).collect() };
    let t0 = clip.clip_time_of_media(corners[0][0].at)?;
    let base = mask.points_at(t0);
    let curves = mask.curves_at(t0);
    let curved = mask.is_curved(t0);
    let n = base.len();
    let mut anchors = base.clone();
    if curved {
        for (p, c) in base.iter().zip(&curves) {
            anchors.push([p[0] + c[0], p[1] + c[1]]);
            anchors.push([p[0] + c[2], p[1] + c[3]]);
        }
    }
    // Turned and scaled about the shape's middle.
    let mid = {
        let (sx, sy) = base.iter().fold((0.0, 0.0), |(x, y), p| (x + p[0], y + p[1]));
        [sx / n.max(1) as f32, sy / n.max(1) as f32]
    };
    // Where each point and handle end sits on the grid (bending only).
    let on_grid: Vec<crate::meshwarp::MeshCoord> = if motion == MaskMotion::Bend {
        let g0 = grid(0);
        anchors.iter().map(|&q| g0.locate(q)).collect::<Option<_>>()?
    } else {
        Vec::new()
    };
    let q0 = if motion == MaskMotion::Bend { [[0.0; 2]; 4] } else { quad(0) };
    let reach = ((q0[1][0] - q0[0][0]).abs().max((q0[3][1] - q0[0][1]).abs()) / 2.0).max(8.0);
    let mut keys: Vec<crate::mask::ShapeKey> = Vec::new();
    for i in 0..frames {
        let Some(at) = clip.clip_time_of_media(corners[0][i].at) else { continue };
        if motion == MaskMotion::Bend {
            let g = grid(i);
            let moved: Vec<[f32; 2]> = on_grid.iter().map(|&m| g.apply(m)).collect();
            keys.push(key_from(&moved, n, curved, at));
            continue;
        }
        let h = if i == 0 { Homography::IDENTITY } else { Homography::from_quads(&q0, &quad(i))? };
        let h = match motion {
            MaskMotion::Perspective | MaskMotion::Bend => h,
            MaskMotion::Position => motion_between(TrackMode::Position, &[mid], &[h.apply(mid)])?,
            MaskMotion::Transform => {
                let side = [mid[0] + reach, mid[1]];
                motion_between(TrackMode::Transform, &[mid, side], &[h.apply(mid), h.apply(side)])?
            }
        };
        let moved: Vec<[f32; 2]> = anchors.iter().map(|&q| h.apply(q)).collect();
        keys.push(key_from(&moved, n, curved, at));
    }
    let (a, b) = (keys.iter().map(|k| k.at).min()?, keys.iter().map(|k| k.at).max()?);
    let mut m = mask.clone();
    if m.keys.is_empty() {
        // Not animated before: the shape outside the span is the one tracking started from.
        m.points = base;
        if curved {
            m.curves = curves;
        }
    }
    m.keys.retain(|k| k.at < a || k.at > b);
    for k in keys {
        let i = m.keys.partition_point(|x| x.at < k.at);
        m.keys.insert(i, k);
    }
    Some(m)
}

/// The point at `uv` (0–1 each way) of the four-sided `q` (top left, top
/// right, bottom right, bottom left), stretched bilinearly.
fn bilinear(q: [[f32; 2]; 4], uv: [f32; 2]) -> [f32; 2] {
    let [u, v] = uv;
    let w = [(1.0 - u) * (1.0 - v), u * (1.0 - v), u * v, (1.0 - u) * v];
    [0, 1].map(|c| w[0] * q[0][c] + w[1] * q[1][c] + w[2] * q[2][c] + w[3] * q[3][c])
}

/// Where `p` is in the four-sided `q` (the `uv` that [`bilinear`] maps to
/// it), found by Newton's method from `guess`; `None` if the shape is folded there.
fn inverse_bilinear(q: [[f32; 2]; 4], p: [f32; 2], guess: [f32; 2]) -> Option<[f32; 2]> {
    let [mut u, mut v] = guess.map(|x| x as f64);
    let q = q.map(|a| a.map(|x| x as f64));
    let (px, py) = (p[0] as f64, p[1] as f64);
    for _ in 0..30 {
        let w = [(1.0 - u) * (1.0 - v), u * (1.0 - v), u * v, (1.0 - u) * v];
        let f = [0, 1].map(|c| w[0] * q[0][c] + w[1] * q[1][c] + w[2] * q[2][c] + w[3] * q[3][c]);
        let (rx, ry) = (f[0] - px, f[1] - py);
        let du = [0, 1].map(|c| (1.0 - v) * (q[1][c] - q[0][c]) + v * (q[2][c] - q[3][c]));
        let dv = [0, 1].map(|c| (1.0 - u) * (q[3][c] - q[0][c]) + u * (q[2][c] - q[1][c]));
        let det = du[0] * dv[1] - du[1] * dv[0];
        if det.abs() < 1e-12 {
            return None;
        }
        let su = (rx * dv[1] - ry * dv[0]) / det;
        let sv = (du[0] * ry - du[1] * rx) / det;
        u -= su;
        v -= sv;
        if su.abs() + sv.abs() < 1e-10 {
            break;
        }
    }
    (u.is_finite() && v.is_finite()).then_some([u as f32, v as f32])
}

/// The best-fitting move, turn and uniform scale taking points `from` to
/// `to` (least squares; at least two points that aren't all in one place).
fn fit_similar(from: &[[f32; 2]], to: &[[f32; 2]]) -> Option<Homography> {
    let n = from.len().min(to.len());
    if n == 0 {
        return None;
    }
    let mid = |p: &[[f32; 2]]| p[..n].iter().fold([0.0f64; 2], |a, q| [a[0] + q[0] as f64 / n as f64, a[1] + q[1] as f64 / n as f64]);
    let (cf, ct) = (mid(from), mid(to));
    let (mut dot, mut cross, mut len) = (0.0, 0.0, 0.0);
    for (f, t) in from[..n].iter().zip(&to[..n]) {
        let u = [f[0] as f64 - cf[0], f[1] as f64 - cf[1]];
        let v = [t[0] as f64 - ct[0], t[1] as f64 - ct[1]];
        dot += u[0] * v[0] + u[1] * v[1];
        cross += u[0] * v[1] - u[1] * v[0];
        len += u[0] * u[0] + u[1] * u[1];
    }
    if len < 1e-6 {
        return None;
    }
    let (a, b) = (dot / len, cross / len);
    Some(Homography([a, -b, ct[0] - (a * cf[0] - b * cf[1]), b, a, ct[1] - (b * cf[0] + a * cf[1]), 0.0, 0.0, 1.0]))
}

/// Make point clip `target` (its settings are `target.effects[0]`) follow the
/// tracked motion, so every layer parented to it follows too. The point keeps
/// where it is at the start of the tracked span and moves, turns and scales
/// with the tracked points from there, as keyframes over the span (its keys
/// outside it are kept). One-point tracks move it; two-point tracks also turn
/// and scale it; corner-pin, planar and mesh tracks give it the surface's
/// best-fitting move, turn and scale (a point can't take perspective).
///
/// `parent_at` is where the point's own parent puts it at a sequence time,
/// if it has one: the point's settings are inside that.
pub fn follow_point(
    tracked: &Clip,
    tracker: &Tracker,
    layer: [f32; 2],
    frame: [f32; 2],
    target: &Clip,
    parent_at: impl Fn(Time) -> Option<crate::space3d::Null>,
    axes: Axes,
) -> Option<crate::effects::Effect> {
    use crate::effects::point as pt;
    use crate::space3d::Null;
    let fx = target.effects.first().filter(|f| f.kind == crate::effects::EffectKind::Point)?;
    let to_seq = |src: Time| tracked.timeline_of_media(src).unwrap_or(tracked.timeline_start);
    let frames: Vec<(Time, Vec<[f32; 2]>)> = tracker
        .frames()
        .into_iter()
        .map(|(src, pts)| {
            let t = to_seq(src);
            let m = tracked.transform_at(t).layer_map(layer, frame);
            (t, pts.iter().map(|p| m.apply([p[0] - layer[0] / 2.0, p[1] - layer[1] / 2.0])).collect())
        })
        .filter(|(t, _)| *t >= target.timeline_start && *t <= target.timeline_end())
        .collect();
    let (t0, ref_pts) = frames.first().map(|(t, p)| (*t, p.clone()))?;
    let turns = tracker.mode != TrackMode::Position;
    let clamp_t = |t: Time| t.max(target.timeline_start).min(target.timeline_end());
    // The point as placed in the frame (inside its parent) at sequence time `t`.
    let world = |t: Time| {
        let own = Null::from_effect(fx, target.source_time(clamp_t(t)));
        match parent_at(t) {
            Some(p) => own.within(&p),
            None => own,
        }
    };
    let base = world(t0);
    let c = [frame[0] as f64 / 2.0, frame[1] as f64 / 2.0];
    let mut keys: Vec<(Time, Null)> = Vec::new();
    let mut last_angle: Option<f32> = None;
    for (t, pts) in &frames {
        let h = match tracker.mode {
            TrackMode::Position | TrackMode::Transform => motion_between(tracker.mode, &ref_pts, pts),
            _ => fit_similar(&ref_pts, pts),
        };
        let Some(h) = h else { continue };
        let sim = Similar::of(&h);
        let now = world(*t);
        // Where the point's centre goes (frame pixels, from the frame's centre).
        let m = h.apply([(c[0] + base.position[0] as f64) as f32, (c[1] + base.position[1] as f64) as f32]);
        let moved = [m[0] - c[0] as f32, m[1] - c[1] as f32];
        let mut angle = base.rotation + sim.angle as f32;
        if let Some(prev) = last_angle {
            angle = prev + ((angle - prev + 180.0).rem_euclid(360.0) - 180.0);
        }
        last_angle = Some(angle);
        // Parts left out keep the point's own (animated) value.
        let desired = Null {
            position: [if axes.x { moved[0] } else { now.position[0] }, if axes.y { moved[1] } else { now.position[1] }, now.position[2]],
            rotation: if turns && axes.rotation { angle } else { now.rotation },
            scale: if turns && axes.scale { base.scale * sim.scale as f32 } else { now.scale },
        };
        // Back into the parent's space.
        let own = match parent_at(*t) {
            Some(p) => desired.within(&p.inverse()),
            None => desired,
        };
        keys.push((target.source_time(*t), own));
    }
    let (Some(lo), Some(hi)) = (keys.iter().map(|k| k.0).min(), keys.iter().map(|k| k.0).max()) else { return None };
    let mut out = fx.clone();
    out.normalize();
    let mut write = |i: usize, get: &dyn Fn(&Null) -> f32| {
        let p = &mut out.params[i];
        p.keys.0.retain(|k| k.at < lo || k.at > hi);
        for (at, n) in &keys {
            p.keys.set(*at, get(n));
        }
    };
    if axes.x {
        write(pt::X, &|n| n.position[0]);
    }
    if axes.y {
        write(pt::Y, &|n| n.position[1]);
    }
    if turns && axes.rotation {
        write(pt::ROTATION, &|n| n.rotation);
    }
    if turns && axes.scale {
        write(pt::SCALE, &|n| n.scale * 100.0);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    #[test]
    fn smoothing_takes_out_jitter_but_keeps_the_motion() {
        let s = |k: i64| Time::from_secs_f64(k as f64 / 25.0);
        let mut t = Tracker::new(TrackMode::Position, [0.0, 0.0], [400.0, 300.0], Time::ZERO);
        // Moving 3 px a frame to the right, with ±2 px of shake; then (after a gap) standing still.
        let mut path: Vec<TrackSample> = (0..60).map(|k| TrackSample { at: s(k), pos: [3.0 * k as f32 + if k % 2 == 0 { 2.0 } else { -2.0 }, 50.0], score: 1.0 }).collect();
        path.extend((80..100).map(|k| TrackSample { at: s(k), pos: [500.0, 50.0], score: 1.0 }));
        t.points[0].path = path;
        // A new tracker starts with a frame's smoothing; at 0 it's the raw track.
        assert_eq!(t.smooth, 1.0);
        let light = t.frames();
        assert!((light[30].1[0][0] - 90.0).abs() < 1.6 && light[30].1[0][0] != t.raw_frames()[30].1[0][0], "lightly smoothed: {:?}", light[30]);
        t.smooth = 0.0;
        assert_eq!(t.frames(), t.raw_frames(), "no smoothing at 0");
        t.smooth = 4.0;
        let f = t.frames();
        let x = |k: usize| f[k].1[0][0];
        for k in 0..60 {
            // ±2 px of shake: almost gone in the middle, mostly gone at the ends; the line is kept.
            let tol = if (6..54).contains(&k) { 0.2 } else { 0.6 };
            assert!((x(k) - 3.0 * k as f32).abs() < tol, "frame {k}: {}", x(k));
        }
        assert!((60..80).all(|k| (x(k) - 500.0).abs() < 1e-3), "nothing smeared across the gap");
        // The samples themselves are untouched, and old files load as unsmoothed.
        assert_eq!(t.points[0].path[1].pos[0], 1.0);
        let back: Tracker = serde_json::from_str(&serde_json::to_string(&Tracker { smooth: 0.0, ..t.clone() }).unwrap()).unwrap();
        assert_eq!(back.smooth, 0.0);
        let back: Tracker = serde_json::from_str(&serde_json::to_string(&t).unwrap()).unwrap();
        assert_eq!(back.smooth, 4.0);
    }

    #[test]
    fn blade_follows_two_points_with_rotation_and_chosen_axes() {
        let s = |x: f64| Time::from_secs_f64(x);
        let mut t = Tracker::new(TrackMode::Transform, [100.0, 100.0], [400.0, 300.0], s(0.0));
        // Two markers on a prop: they turn 90° about their middle (150, 100) and move 20 px right.
        let path = |a: [f32; 2], b: [f32; 2]| vec![TrackSample { at: s(0.0), pos: a, score: 1.0 }, TrackSample { at: s(1.0), pos: b, score: 1.0 }];
        t.points[0].path = path([100.0, 100.0], [170.0, 50.0]);
        t.points[1].path = path([200.0, 100.0], [170.0, 150.0]);
        let ends = [[150.0, 100.0], [150.0, 0.0]];
        let f = follow_blade(&t, ends, s(0.0), Axes::ALL).unwrap();
        let [h, tip] = f.last().unwrap().1;
        assert!((h[0] - 170.0).abs() < 0.01 && (h[1] - 100.0).abs() < 0.01, "hilt moved with the middle: {h:?}");
        assert!((tip[0] - 270.0).abs() < 0.01 && (tip[1] - 100.0).abs() < 0.01, "tip turned 90°: {tip:?}");
        // Rotation only: no move, still turned.
        let f = follow_blade(&t, ends, s(0.0), Axes { x: false, y: false, rotation: true, scale: false }).unwrap();
        let [h, tip] = f.last().unwrap().1;
        assert!((h[0] - 150.0).abs() < 0.01 && (tip[0] - 250.0).abs() < 0.01, "{h:?} {tip:?}");
        // A mesh can't carry a blade.
        t.mode = TrackMode::Mesh;
        assert!(follow_blade(&t, ends, s(0.0), Axes::ALL).is_none());
    }

    use super::*;

    #[test]
    fn new_trackers_have_boxes_sized_for_the_footage() {
        let sizes = |w: f32, h: f32| {
            let t = Tracker::new(TrackMode::Transform, [w / 2.0, h / 2.0], [w, h], Time::ZERO);
            assert!(t.points.iter().all(|p| p.size == t.points[0].size));
            (t.points[0].size, t.points[0].search)
        };
        assert_eq!(sizes(640.0, 360.0), (31.0, 24.0));
        assert_eq!(sizes(1920.0, 1080.0), (47.0, 36.0));
        assert_eq!(sizes(4096.0, 2160.0), (93.0, 72.0));
        assert_eq!(sizes(1080.0, 1920.0), (47.0, 36.0), "portrait: by the short side");
        // A tracker made before (31 and 24 px whatever the footage) is grown
        // to suit 4K; one whose boxes were set by hand isn't, nor on 720p.
        let mut old = Tracker::new(TrackMode::Transform, [50.0, 50.0], [640.0, 360.0], Time::ZERO);
        assert!(!old.fit_old_defaults([1280.0, 720.0]));
        assert!(old.fit_old_defaults([4096.0, 2160.0]));
        assert!(old.points.iter().all(|p| p.size == 93.0 && p.search == 72.0));
        let mut set = Tracker::new(TrackMode::Position, [50.0, 50.0], [640.0, 360.0], Time::ZERO);
        set.set_sizes(41.0, 24.0);
        assert!(!set.fit_old_defaults([4096.0, 2160.0]));
        assert_eq!(set.points[0].size, 41.0);
    }

    #[test]
    fn template_matching_holds_exact_looks_and_correlation_copes_with_light() {
        let (w, h) = (128usize, 96usize);
        // A textured mark moving 2 px a frame right, on a plain ground; `gain` dims everything.
        let frame = |k: usize, gain: f32| {
            let (cx, cy) = (30.0 + 2.0 * k as f32, 48.0);
            let data: Vec<f32> = (0..w * h)
                .map(|i| {
                    let (dx, dy) = ((i % w) as f32 - cx, (i / w) as f32 - cy);
                    let blob = (-(dx * dx + dy * dy) / 60.0).exp() * (1.0 + 0.5 * (dx * 0.7).sin() * (dy * 0.5).cos());
                    gain * (60.0 + 120.0 * blob)
                })
                .collect();
            Pyramid::new(data, w, h, 1)
        };
        let run = |m: TrackMatch, dim: f32| -> (usize, f32) {
            let mut f = FeatureTracker::new_matching(&frame(0, 1.0), [30.0, 48.0], 21.0, 12.0, m);
            for k in 1..=20 {
                let (p, score) = f.step(&frame(k, dim.powi(k as i32)));
                if score < MIN_SCORE {
                    return (k, 0.0);
                }
                let err = (p[0] - (30.0 + 2.0 * k as f32)).abs() + (p[1] - 48.0).abs();
                if k == 20 {
                    return (21, err);
                }
            }
            (21, 0.0)
        };
        // Steady light: both follow it all the way, on the spot.
        for m in TrackMatch::ALL {
            let (done, err) = run(m, 1.0);
            assert!(done == 21 && err < 0.3, "{m:?}: {done} frames, {err} px off");
        }
        // Fading 4% a frame: correlation keeps it, template matching loses it.
        let (done, err) = run(TrackMatch::Correlation, 0.96);
        assert!(done == 21 && err < 0.5, "correlation: {done} frames, {err} px off");
        let (lost, _) = run(TrackMatch::Template, 0.96);
        assert!(lost < 20, "template match lost it as the light changed (frame {lost})");
        // Saved only when not the default.
        let mut t = Tracker::new(TrackMode::Position, [0.0, 0.0], [400.0, 300.0], Time::ZERO);
        assert!(!serde_json::to_string(&t).unwrap().contains("matching"));
        t.matching = TrackMatch::Template;
        let back: Tracker = serde_json::from_str(&serde_json::to_string(&t).unwrap()).unwrap();
        assert_eq!(back.matching, TrackMatch::Template);
    }

    #[test]
    fn corrections_ease_into_the_frames_around() {
        let f = |k: i64| Time(k * 1000);
        let mut p = PointTrack { path: (0..10).map(|k| TrackSample { at: f(k), pos: [k as f32 * 10.0, 0.0], score: 1.0 }).collect(), ..Default::default() };
        // 3 frames each side: frame 5 moves 8 px down, its neighbours less and less.
        p.move_blended(f(5), [50.0, 8.0], 3);
        let ys: Vec<f32> = p.path.iter().map(|s| s.pos[1]).collect();
        assert_eq!(ys[5], 8.0);
        assert!((ys[4] - 8.0 * 0.853_553).abs() < 1e-3 && (ys[6] - ys[4]).abs() < 1e-6, "{ys:?}");
        assert!((ys[3] - 4.0).abs() < 1e-3 && (ys[2] - 8.0 * 0.146_447).abs() < 1e-3, "{ys:?}");
        assert_eq!((ys[1], ys[9]), (0.0, 0.0));
        // x untouched (the move was straight down).
        assert!(p.path.iter().enumerate().all(|(k, s)| s.pos[0] == k as f32 * 10.0));
        // Dragged on in small steps (as a drag sends them), the same result.
        let mut q = PointTrack { path: (0..10).map(|k| TrackSample { at: f(k), pos: [k as f32 * 10.0, 0.0], score: 1.0 }).collect(), ..Default::default() };
        for y in [2.0, 5.0, 8.0] {
            q.move_blended(f(5), [50.0, y], 3);
        }
        for (a, b) in p.path.iter().zip(&q.path) {
            assert!((a.pos[1] - b.pos[1]).abs() < 1e-4);
        }
        // Span 0, or off the path: as `move_to`.
        q.move_blended(f(5), [50.0, 0.0], 0);
        assert_eq!(q.path[4].pos[1], p.path[4].pos[1]);
        q.move_blended(f(20), [1.0, 2.0], 3);
        assert_eq!((q.point, q.start), ([1.0, 2.0], f(20)));
    }

    #[test]
    fn moving_a_mesh_corner_keeps_the_grids_shape() {
        let mut t = Tracker { mode: TrackMode::Mesh, points: vec![PointTrack::default()], grid: [4, 4], smooth: 0.0, matching: TrackMatch::Correlation };
        t.fill_mesh([[0.0, 0.0], [400.0, 0.0], [400.0, 400.0], [0.0, 400.0]], Time::ZERO);
        // An inner point dragged by hand off the even grid.
        t.points[6].point = [130.0, 80.0];
        let before = t.shown_at(Time::ZERO);
        // Clicking a corner without moving it changes nothing.
        let c = t.mesh_corners()[2];
        t.move_mesh_corner(c, before[c], Time::ZERO);
        let same = t.shown_at(Time::ZERO);
        for (a, b) in before.iter().zip(&same) {
            assert!((a[0] - b[0]).abs() < 1e-3 && (a[1] - b[1]).abs() < 1e-3, "{a:?} became {b:?}");
        }
        // Moving it carries the grid along, the hand-placed point included.
        t.move_mesh_corner(c, [600.0, 400.0], Time::ZERO);
        let after = t.shown_at(Time::ZERO);
        assert_eq!(after[c], [600.0, 400.0]);
        assert_eq!(after[0], [0.0, 0.0]);
        // At the same place in the stretched square: (0.325, 0.2) of the way across and down.
        assert!((after[6][0] - 143.0).abs() < 0.01 && (after[6][1] - 80.0).abs() < 0.01, "{:?}", after[6]);
        // An untouched inner point follows the stretch as an even grid would.
        assert!((after[12][0] - 250.0).abs() < 0.01 && (after[12][1] - 200.0).abs() < 0.01, "{:?}", after[12]);
        // A different number of cells.
        t.set_grid(32, 32, Time::ZERO);
        assert_eq!(t.points.len(), 33 * 33);
        // Any size, not just the listed ones; 32 at most.
        t.set_grid(12, 5, Time::ZERO);
        assert_eq!((t.grid, t.points.len()), ([12, 5], 13 * 6));
        t.set_grid(40, 0, Time::ZERO);
        assert_eq!(t.grid, [32, 1]);
    }
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
        let clip = Clip::new(ClipId(1), MediaId(1), Time::ZERO, Time::ZERO, s(10.0), None);
        let m = follow_mask(&clip, &mask, &Tracker::from(track)).unwrap();
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
    fn axes_hold_surfaces_in_place() {
        let reference = [[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]];
        // Moved right 5 and down 3, and stretched.
        let mut pts = [[5.0, 3.0], [17.0, 3.0], [17.0, 13.0], [5.0, 13.0]];
        Axes { x: false, ..Axes::ALL }.hold(&reference, &mut pts);
        assert_eq!(pts, [[-1.0, 3.0], [11.0, 3.0], [11.0, 13.0], [-1.0, 13.0]], "the middle stays across, the shape still changes");
        let mut same = pts;
        Axes::ALL.hold(&reference, &mut same);
        assert_eq!(same, pts);
    }

    fn keys<T: Copy>(c: &crate::anim::Curve<T>) -> Vec<(Time, T)> {
        c.0.iter().map(|k| (k.at, k.value)).collect()
    }

    #[test]
    fn stabilising_can_zoom_to_hide_the_edges() {
        let secs = Time::from_secs_f64;
        let tracked = Clip::new(ClipId(1), MediaId(1), secs(0.0), secs(0.0), secs(4.0), None);
        // A point shaking about: 100 frames, big jolts in the first half, small in the second.
        let fr = |k: i64| Time(k * Time::TICKS_PER_SECOND / 25);
        let path = (0..100)
            .map(|k| {
                let amp = if k < 50 { 24.0 } else { 4.0 };
                let x = 300.0 + amp * ((k as f32) * 1.7).sin();
                let y = 150.0 + amp * ((k as f32) * 2.3).cos();
                TrackSample { at: fr(k), pos: [x, y], score: 1.0 }
            })
            .collect();
        let track = Tracker::from(PointTrack { path, ..Default::default() });
        let (layer, frame) = ([640.0, 360.0], [640.0, 360.0]);
        let covered = |anim: &TransformAnim| {
            let c = Clip { anim: anim.clone(), ..tracked.clone() };
            (0..100).map(|k| cover_zoom(&c.transform_at(fr(k)).corners(layer, frame), frame)).fold(1.0f32, f32::max)
        };
        let zoom_at = |anim: &TransformAnim, k: i64| Clip { anim: anim.clone(), ..tracked.clone() }.transform_at(fr(k)).scale[0];
        // Without zoom the edges show.
        let plain = follow_keys(&tracked, &track, layer, frame, &tracked, layer, ApplyOptions { stabilise: true, ..Default::default() }).unwrap();
        assert!(covered(&plain) > 1.05);
        // One zoom: every frame covered, the same zoom throughout.
        let fixed = follow_keys(&tracked, &track, layer, frame, &tracked, layer, ApplyOptions { stabilise: true, zoom: StabZoom::Fixed, ..Default::default() }).unwrap();
        assert!(covered(&fixed) < 1.0001, "{}", covered(&fixed));
        assert!((zoom_at(&fixed, 10) - zoom_at(&fixed, 90)).abs() < 1e-5);
        let z = zoom_at(&fixed, 10);
        // Measured from the first frame (where the vertical shake is at its top),
        // the picture moves up to 48 px down: 360 / (360 − 2·48) ≈ 1.364.
        assert!((z - 360.0 / 264.0).abs() < 0.01, "{z}");
        // Following: every frame covered, less zoom where the shake is small.
        let follow = follow_keys(&tracked, &track, layer, frame, &tracked, layer, ApplyOptions { stabilise: true, zoom: StabZoom::Follow, ..Default::default() }).unwrap();
        assert!(covered(&follow) < 1.0001, "{}", covered(&follow));
        assert!(zoom_at(&follow, 95) < zoom_at(&follow, 10) - 0.05, "{} vs {}", zoom_at(&follow, 95), zoom_at(&follow, 10));
        assert!(zoom_at(&follow, 10) <= z + 1e-4);
        // The cover test itself: a picture shifted 10 px needs 1 + 20/360 vertically.
        let shifted = [[0.0, 10.0], [640.0, 10.0], [640.0, 370.0], [0.0, 370.0]];
        assert!((cover_zoom(&shifted, frame) - 360.0 / 340.0).abs() < 1e-3);
        assert_eq!(cover_zoom(&[[0.0, 0.0], [640.0, 0.0], [640.0, 360.0], [0.0, 360.0]], frame), 1.0);
    }

    #[test]
    fn follow_and_stabilise_keys() {
        let secs = Time::from_secs_f64;
        // Tracked clip at 2 s on the timeline, from 1 s into its media, unscaled.
        let tracked = Clip::new(ClipId(1), MediaId(1), secs(2.0), secs(1.0), secs(4.0), None);
        let track = Tracker::from(PointTrack {
            path: vec![
                TrackSample { at: secs(1.0), pos: [100.0, 50.0], score: 1.0 },
                TrackSample { at: secs(2.0), pos: [130.0, 40.0], score: 1.0 },
            ],
            ..Default::default()
        });
        let (layer, frame) = ([640.0, 360.0], [640.0, 360.0]);
        let follow = ApplyOptions::default();
        let stab = ApplyOptions { stabilise: true, smoothing: 0.0, ..Default::default() };
        // A title from 0 s at position (5, 5) follows: +30, -10 a second later (3 s on the timeline).
        let mut target = Clip::new(ClipId(2), MediaId::GENERATED, secs(0.0), secs(0.0), secs(10.0), None);
        target.transform.position = [5.0, 5.0];
        let anim = follow_keys(&tracked, &track, layer, frame, &target, layer, follow).unwrap();
        assert_eq!(keys(&anim.position), vec![(secs(2.0), [5.0, 5.0]), (secs(3.0), [35.0, -5.0])]);
        assert!(anim.rotation.0.is_empty() && anim.scale.0.is_empty(), "one point only moves it");
        // Stabilising the tracked clip moves it the other way, keyed in its own source time.
        let anim = follow_keys(&tracked, &track, layer, frame, &tracked, layer, stab).unwrap();
        assert_eq!(keys(&anim.position), vec![(secs(1.0), [0.0, 0.0]), (secs(2.0), [-30.0, 10.0])]);
        // At double scale the motion on screen is doubled too.
        let mut big = tracked.clone();
        big.transform.scale = [2.0, 2.0];
        let anim = follow_keys(&big, &track, layer, frame, &target, layer, follow).unwrap();
        assert_eq!(anim.position.0[1].value, [65.0, -15.0]);
        // X only: the layer moves across but keeps its own height.
        let x_only = ApplyOptions { axes: Axes { y: false, ..Axes::ALL }, ..Default::default() };
        let anim = follow_keys(&tracked, &track, layer, frame, &target, layer, x_only).unwrap();
        assert_eq!(keys(&anim.position), vec![(secs(2.0), [5.0, 5.0]), (secs(3.0), [35.0, 5.0])]);
        // Y only, stabilising: only the up-and-down shake is taken out.
        let y_only = ApplyOptions { axes: Axes { x: false, ..Axes::ALL }, ..stab };
        let anim = follow_keys(&tracked, &track, layer, frame, &tracked, layer, y_only).unwrap();
        assert_eq!(keys(&anim.position), vec![(secs(1.0), [0.0, 0.0]), (secs(2.0), [0.0, 10.0])]);
        // Neither: nothing to apply.
        let none = ApplyOptions { axes: Axes { x: false, y: false, ..Axes::ALL }, ..Default::default() };
        assert!(follow_keys(&tracked, &track, layer, frame, &target, layer, none).is_none());
    }

    fn close2(a: [f32; 2], b: [f32; 2], tol: f32) -> bool {
        (a[0] - b[0]).abs() < tol && (a[1] - b[1]).abs() < tol
    }

    /// Two points that turn 90° clockwise and double their spacing about (100, 100).
    fn two_point_track() -> Tracker {
        let secs = Time::from_secs_f64;
        let s = |t: f64, p: [f32; 2]| TrackSample { at: secs(t), pos: p, score: 1.0 };
        let a = PointTrack { path: vec![s(0.0, [100.0, 100.0]), s(1.0, [100.0, 100.0])], ..Default::default() };
        let b = PointTrack { path: vec![s(0.0, [120.0, 100.0]), s(1.0, [100.0, 140.0])], ..Default::default() };
        Tracker { mode: TrackMode::Transform, points: vec![a, b], grid: [0, 0], smooth: 0.0, matching: TrackMatch::Correlation }
    }

    #[test]
    fn two_points_give_rotation_and_scale() {
        let secs = Time::from_secs_f64;
        let tracked = Clip::new(ClipId(1), MediaId(1), secs(0.0), secs(0.0), secs(4.0), None);
        let (layer, frame) = ([200.0, 200.0], [200.0, 200.0]);
        let tr = two_point_track();
        let h = motion_between(TrackMode::Transform, &[[100.0, 100.0], [120.0, 100.0]], &[[100.0, 100.0], [100.0, 140.0]]).unwrap();
        assert!(close2(h.apply([120.0, 100.0]), [100.0, 140.0], 1e-3));
        // A 20×10 title sitting 10 px right of the first point (frame (110, 100) = position (10, 0)).
        let mut target = Clip::new(ClipId(2), MediaId::GENERATED, secs(0.0), secs(0.0), secs(4.0), None);
        target.transform.position = [10.0, 0.0];
        target.transform.rotation = 5.0;
        let anim = follow_keys(&tracked, &tr, layer, frame, &target, [20.0, 10.0], ApplyOptions::default()).unwrap();
        let end = anim.eval(&target.transform, secs(1.0));
        assert!((end.rotation - 95.0).abs() < 1e-3, "{}", end.rotation);
        assert!((end.scale[0] - 2.0).abs() < 1e-3 && (end.scale[1] - 2.0).abs() < 1e-3);
        // It swung round the first point: 10 px right becomes 20 px below.
        assert!(close2(end.position, [0.0, 20.0], 1e-3), "{:?}", end.position);
        // Stabilising undoes it: the clip turns back and shrinks.
        let st = follow_keys(&tracked, &tr, layer, frame, &tracked, layer, ApplyOptions { stabilise: true, smoothing: 0.0, ..Default::default() }).unwrap();
        let t = st.eval(&tracked.transform, secs(1.0));
        assert!((t.rotation + 90.0).abs() < 1e-3 && (t.scale[0] - 0.5).abs() < 1e-4, "{t:?}");
        // The tracked point now stays where it was on the frame.
        let m = t.layer_map(layer, frame);
        assert!(close2(m.apply([120.0 - 100.0, 100.0 - 100.0]), [120.0, 100.0], 1e-2) || close2(m.apply([0.0, 40.0]), [120.0, 100.0], 1e-2));
        assert!(close2(m.apply([0.0, 40.0]), [120.0, 100.0], 1e-2), "{:?}", m.apply([0.0, 40.0]));
    }

    fn quad_track(quads: &[[[f32; 2]; 4]]) -> Tracker {
        let points = (0..4)
            .map(|i| PointTrack {
                path: quads.iter().enumerate().map(|(k, q)| TrackSample { at: Time::from_secs_f64(k as f64), pos: q[i], score: 1.0 }).collect(),
                ..Default::default()
            })
            .collect();
        Tracker { mode: TrackMode::CornerPin, points, grid: [0, 0], smooth: 0.0, matching: TrackMatch::Correlation }
    }

    /// A mesh track warps another layer over its grid (on the frame, through the
    /// tracked clip's own transform), and masks keep their place on the grid.
    #[test]
    fn mesh_tracks_warp_layers_and_bend_masks() {
        let secs = Time::from_secs_f64;
        let mut tracked = Clip::new(ClipId(1), MediaId(1), secs(0.0), secs(0.0), secs(4.0), None);
        tracked.transform.position = [10.0, 0.0]; // moved 10 px right on the frame
        let target = Clip::new(ClipId(2), MediaId(2), secs(1.0), secs(0.0), secs(4.0), None);
        let (layer, frame) = ([400.0, 300.0], [400.0, 300.0]);
        // A 2 × 1 grid whose middle column bows down 20 px by the second frame.
        let g0 = crate::meshwarp::MeshShape::filling([[100.0, 100.0], [300.0, 100.0], [300.0, 200.0], [100.0, 200.0]], 2, 1);
        let mut g1 = g0.clone();
        g1.points[1][1] += 20.0;
        g1.points[4][1] += 20.0;
        let points = (0..6)
            .map(|i| PointTrack {
                path: [(&g0, 1.0), (&g1, 2.0)].iter().map(|(g, t)| TrackSample { at: secs(*t), pos: g.points[i], score: 1.0 }).collect(),
                ..Default::default()
            })
            .collect();
        let tr = Tracker { mode: TrackMode::Mesh, points, grid: [2, 1], smooth: 0.0, matching: TrackMatch::Correlation };
        let w = follow_mesh(&tracked, &tr, layer, frame, &target, Axes::ALL).unwrap();
        // Keys at the target's source times (it starts at 1 s), shifted by the tracked clip's position.
        assert_eq!(w.keys.len(), 2);
        assert_eq!(w.keys[0].0, secs(0.0));
        assert_eq!(w.keys[1].0, secs(1.0));
        assert!(close2(w.keys[1].1.points[1], [210.0, 120.0], 1e-3), "{:?}", w.keys[1].1.points[1]);
        // A mask point halfway along the top edge bows with it.
        let mask = crate::mask::Mask { points: vec![[150.0, 100.0], [200.0, 150.0]], ..Default::default() };
        let mm = follow_mask(&tracked, &mask, &tr).unwrap();
        let last = &mm.keys.last().unwrap().points;
        assert!(close2(last[0], [150.0, 110.0], 1e-3), "{:?}", last[0]);
        assert!(close2(last[1], [200.0, 170.0], 1e-3), "{:?}", last[1]);
        // Switching away from a mesh keeps its outer corners.
        let mut t2 = tr.clone();
        t2.set_mode(TrackMode::Planar, layer, secs(1.0));
        assert_eq!(t2.shown_at(secs(1.0)), vec![[100.0, 100.0], [300.0, 100.0], [300.0, 200.0], [100.0, 200.0]]);
        // And a new mesh fills a planar tracker's corners.
        t2.clear_paths();
        t2.set_mode(TrackMode::Mesh, layer, secs(1.0));
        assert_eq!((t2.grid, t2.points.len()), ([DEFAULT_GRID, DEFAULT_GRID], 25));
        t2.set_grid(3, 2, secs(1.0));
        assert_eq!(t2.points.len(), 12);
        let back: Tracker = serde_json::from_str(&serde_json::to_string(&t2).unwrap()).unwrap();
        assert_eq!(back, t2);
    }

    #[test]
    fn points_follow_tracks_and_carry_their_children() {
        use crate::effects::point as pt;
        use crate::space3d::{Null, SceneObject};
        let secs = Time::from_secs_f64;
        let tracked = Clip::new(ClipId(1), MediaId(1), secs(0.0), secs(0.0), secs(4.0), None);
        let (layer, frame) = ([200.0, 200.0], [200.0, 200.0]);
        let mut point = Clip::new_scene(ClipId(2), secs(0.0), secs(4.0), SceneObject::Point, frame);
        point.effects[0].params[pt::X].value = 10.0;
        point.effects[0].params[pt::ROTATION].value = 5.0;
        let at = |fx: &crate::effects::Effect, t: f64| Null::from_effect(fx, secs(t));
        // Two points turning 90° clockwise and doubling about (100, 100): the point
        // 10 px right of the first one swings to 20 px below it, turns and doubles.
        let fx = follow_point(&tracked, &two_point_track(), layer, frame, &point, |_| None, Axes::ALL).unwrap();
        let end = at(&fx, 1.0);
        assert!((end.position[0]).abs() < 1e-3 && (end.position[1] - 20.0).abs() < 1e-3, "{end:?}");
        assert!((end.rotation - 95.0).abs() < 1e-3 && (end.scale - 2.0).abs() < 1e-4, "{end:?}");
        assert_eq!(at(&fx, 0.0), Null { position: [10.0, 0.0, 0.0], rotation: 5.0, scale: 1.0 });
        // A layer parented to it, sitting on the first tracked point (which doesn't move), stays on it.
        let child = at(&point.effects[0], 0.0).inverse().apply([0.0, 0.0, 0.0]);
        let p = end.apply(child);
        assert!(p[0].abs() < 1e-3 && p[1].abs() < 1e-3, "{p:?}");
        // Only X: it moves across, nothing else is keyed.
        let fx = follow_point(&tracked, &two_point_track(), layer, frame, &point, |_| None, Axes { y: false, rotation: false, scale: false, ..Axes::ALL }).unwrap();
        assert!(fx.params[pt::Y].keys.0.is_empty() && fx.params[pt::ROTATION].keys.0.is_empty() && fx.params[pt::SCALE].keys.0.is_empty());
        assert_eq!(fx.params[pt::X].keys.0.len(), 2);
        // Inside a parent turned 90° and moved: the point still lands in the same place on the frame.
        let parent = Null { position: [30.0, -5.0, 0.0], rotation: 90.0, scale: 0.5 };
        let mut inside = point.clone();
        let own0 = Null { position: [10.0, 0.0, 0.0], rotation: 5.0, scale: 1.0 }.within(&parent.inverse());
        inside.effects[0].params[pt::X].value = own0.position[0];
        inside.effects[0].params[pt::Y].value = own0.position[1];
        inside.effects[0].params[pt::ROTATION].value = own0.rotation;
        inside.effects[0].params[pt::SCALE].value = own0.scale * 100.0;
        let fx = follow_point(&tracked, &two_point_track(), layer, frame, &inside, |_| Some(parent), Axes::ALL).unwrap();
        let w = at(&fx, 1.0).within(&parent);
        assert!((w.position[0]).abs() < 1e-2 && (w.position[1] - 20.0).abs() < 1e-2, "{w:?}");
        assert!((w.rotation - 95.0).abs() < 1e-2 && (w.scale - 2.0).abs() < 1e-3, "{w:?}");
        // A planar track that moves 20, 10 and doubles about its middle: the best fit has no turn.
        let q0 = [[50.0, 50.0], [150.0, 50.0], [150.0, 150.0], [50.0, 150.0]];
        let q1 = [[20.0, 10.0], [220.0, 10.0], [220.0, 210.0], [20.0, 210.0]];
        let planar = Tracker { mode: TrackMode::Planar, ..quad_track(&[q0, q1]) };
        let fx = follow_point(&tracked, &planar, layer, frame, &point, |_| None, Axes::ALL).unwrap();
        let end = at(&fx, 1.0);
        // (110, 100) on the frame, from the middle (100, 100): +20, +10, then 2× → (140, 110).
        assert!((end.position[0] - 40.0).abs() < 1e-3 && (end.position[1] - 10.0).abs() < 1e-3, "{end:?}");
        assert!((end.rotation - 5.0).abs() < 1e-3 && (end.scale - 2.0).abs() < 1e-4, "{end:?}");
        // Not a point, or no overlap: nothing.
        assert!(follow_point(&tracked, &planar, layer, frame, &tracked, |_| None, Axes::ALL).is_none());
    }

    #[test]
    fn curved_masks_follow_turning_tracks_handles_and_all() {
        let secs = Time::from_secs_f64;
        let clip = Clip::new(ClipId(1), MediaId(1), secs(0.0), secs(0.0), secs(4.0), None);
        let pts = vec![[110.0, 100.0], [130.0, 100.0], [130.0, 120.0]];
        let mut mask = crate::mask::Mask::freehand(pts.clone());
        mask.curves = vec![[0.0, 0.0, 10.0, 0.0], [0.0; 4], [0.0; 4]];
        // Turned 90° clockwise and doubled about (100, 100): (10, 0) becomes (0, 20).
        let m = follow_mask(&clip, &mask, &two_point_track()).unwrap();
        let c = m.curves_at(secs(1.0));
        assert!(close2([c[0][2], c[0][3]], [0.0, 20.0], 1e-3), "{:?}", c[0]);
        assert!(close2(m.points_at(secs(1.0))[0], [100.0, 120.0], 1e-3));
        assert_eq!(m.curves_at(secs(0.0))[0], [0.0, 0.0, 10.0, 0.0]);
    }

    /// A planar track's four corners apply exactly as a four-point track's do.
    #[test]
    fn planar_tracks_apply_like_corner_pins() {
        let secs = Time::from_secs_f64;
        let tracked = Clip::new(ClipId(1), MediaId(1), secs(0.0), secs(0.0), secs(4.0), None);
        let target = Clip::new(ClipId(2), MediaId(2), secs(0.0), secs(0.0), secs(4.0), None);
        let (layer, frame) = ([400.0, 300.0], [400.0, 300.0]);
        let q0 = [[100.0, 100.0], [300.0, 100.0], [300.0, 200.0], [100.0, 200.0]];
        let q1 = [[120.0, 90.0], [310.0, 120.0], [290.0, 230.0], [110.0, 210.0]];
        let pin = quad_track(&[q0, q1]);
        let planar = Tracker { mode: TrackMode::Planar, ..pin.clone() };
        assert!(TrackMode::Planar.is_perspective() && TrackMode::Planar.points() == 4);
        for opts in [ApplyOptions::default(), ApplyOptions { stabilise: true, smoothing: 10.0, ..Default::default() }] {
            let t = if opts.stabilise { &tracked } else { &target };
            let a = follow_keys(&tracked, &pin, layer, frame, t, layer, opts).unwrap();
            let b = follow_keys(&tracked, &planar, layer, frame, t, layer, opts).unwrap();
            assert_eq!(keys(&a.pin), keys(&b.pin));
        }
        let back: Tracker = serde_json::from_str(&serde_json::to_string(&planar).unwrap()).unwrap();
        assert_eq!(back, planar);
    }

    #[test]
    fn four_points_corner_pin_a_layer() {
        let secs = Time::from_secs_f64;
        let tracked = Clip::new(ClipId(1), MediaId(1), secs(0.0), secs(0.0), secs(4.0), None);
        let (layer, frame) = ([400.0, 300.0], [400.0, 300.0]);
        let q0 = [[100.0, 100.0], [300.0, 100.0], [300.0, 200.0], [100.0, 200.0]];
        let q1 = [[120.0, 90.0], [310.0, 120.0], [290.0, 230.0], [110.0, 210.0]];
        let tr = quad_track(&[q0, q1]);
        // An unpinned layer is pinned straight into the four points.
        let target = Clip::new(ClipId(2), MediaId(2), secs(0.0), secs(0.0), secs(4.0), None);
        let anim = follow_keys(&tracked, &tr, layer, frame, &target, [640.0, 360.0], ApplyOptions::default()).unwrap();
        assert_eq!(keys(&anim.pin), vec![(secs(0.0), Pin(q0)), (secs(1.0), Pin(q1))]);
        let t = anim.eval(&target.transform, secs(1.0));
        assert_eq!(t.corners([640.0, 360.0], frame), q1);
        // A layer already pinned (by hand, inside the surface) moves with it in perspective.
        let mut inner = target.clone();
        let small = [[150.0, 125.0], [250.0, 125.0], [250.0, 175.0], [150.0, 175.0]];
        inner.transform.pin = Some(Pin(small));
        let anim = follow_keys(&tracked, &tr, layer, frame, &inner, [640.0, 360.0], ApplyOptions::default()).unwrap();
        let h = Homography::from_quads(&q0, &q1).unwrap();
        let moved = anim.eval(&inner.transform, secs(1.0)).pin.unwrap();
        for i in 0..4 {
            assert!(close2(moved.0[i], h.apply(small[i]), 1e-2));
        }
        // Stabilising takes the perspective change back out.
        let st = follow_keys(&tracked, &tr, layer, frame, &tracked, layer, ApplyOptions { stabilise: true, smoothing: 0.0, ..Default::default() }).unwrap();
        let m = st.eval(&tracked.transform, secs(1.0)).layer_map(layer, frame);
        for i in 0..4 {
            let p = m.apply([q1[i][0] - 200.0, q1[i][1] - 150.0]);
            assert!(close2(p, q0[i], 0.05), "{p:?} vs {:?}", q0[i]);
        }
        // So does a mask on the clip.
        let mask = crate::mask::Mask { points: small.to_vec(), ..Default::default() };
        let mm = follow_mask(&tracked, &mask, &tr).unwrap();
        assert!(close2(mm.points_at(secs(1.0))[2], h.apply(small[2]), 1e-2));
    }

    #[test]
    fn smoothed_stabilising_keeps_the_pan_and_removes_the_shake() {
        let secs = Time::from_secs_f64;
        let fps = 25.0;
        // The camera pans steadily (2 px a frame) with ±3 px of shake.
        let path: Vec<TrackSample> = (0..100)
            .map(|i| {
                let shake = if i % 2 == 0 { 3.0 } else { -3.0 };
                TrackSample { at: secs(i as f64 / fps), pos: [100.0 + 2.0 * i as f32 + shake, 100.0], score: 1.0 }
            })
            .collect();
        let tr = Tracker::from(PointTrack { path, ..Default::default() });
        let clip = Clip::new(ClipId(1), MediaId(1), secs(0.0), secs(0.0), secs(4.0), None);
        let (layer, frame) = ([400.0, 300.0], [400.0, 300.0]);
        let anim = follow_keys(&clip, &tr, layer, frame, &clip, layer, ApplyOptions { stabilise: true, smoothing: 10.0, ..Default::default() }).unwrap();
        // Where the feature ends up on screen, frame by frame (well inside the run).
        let shown: Vec<f32> = (20..80).map(|i| tr.points[0].path[i].pos[0] - 200.0 + anim.position.0[i].value[0] + 200.0).collect();
        let steps: Vec<f32> = shown.windows(2).map(|w| w[1] - w[0]).collect();
        let mean = steps.iter().sum::<f32>() / steps.len() as f32;
        assert!((mean - 2.0).abs() < 0.2, "the pan stays: {mean}");
        assert!(steps.iter().all(|s| (s - 2.0).abs() < 0.6), "the shake is gone: {steps:?}");
    }

    #[test]
    fn pyramid_search_follows_fast_motion() {
        let (w, h) = (320, 240);
        let a = scene(w, h, [80.0, 60.0], 1.0);
        let pa = Pyramid::new(a, w, h, Pyramid::levels_for(70.0));
        let mut ft = FeatureTracker::new(&pa, [80.0, 60.0], 25.0, 70.0);
        // 45 px right and 25 down in one frame, then the same again (predicted).
        for (k, c) in [[125.3, 85.4], [170.6, 110.8]].into_iter().enumerate() {
            let b = scene(w, h, c, 0.8);
            let pb = Pyramid::new(b, w, h, Pyramid::levels_for(70.0));
            let (p, score) = ft.step(&pb);
            assert!(score > 0.9, "step {k}: {score}");
            assert!(close2(p, c, 0.4), "step {k}: {p:?} vs {c:?}");
        }
    }

    #[test]
    fn modes_and_old_projects() {
        // A project saved with a single point tracker still loads.
        let old = r#"{"point":[10.0,20.0],"start":0,"size":31.0,"search":24.0,"path":[]}"#;
        let t: Tracker = serde_json::from_str(old).unwrap();
        assert_eq!(t.mode, TrackMode::Position);
        assert_eq!(t.points[0].point, [10.0, 20.0]);
        // Round trip.
        let mut t = Tracker::new(TrackMode::CornerPin, [200.0, 150.0], [400.0, 300.0], Time::ZERO);
        assert_eq!(t.points.len(), 4);
        assert!(Pin([t.points[0].point, t.points[1].point, t.points[2].point, t.points[3].point]).is_valid());
        let back: Tracker = serde_json::from_str(&serde_json::to_string(&t).unwrap()).unwrap();
        assert_eq!(back, t);
        t.set_mode(TrackMode::Transform, [400.0, 300.0], Time::ZERO);
        assert_eq!(t.points.len(), 2);
        // Correcting a tracked point edits that frame; elsewhere it's a new start.
        let s = Time::from_secs_f64;
        let mut p = PointTrack { path: vec![TrackSample { at: s(0.0), pos: [0.0, 0.0], score: 1.0 }, TrackSample { at: s(1.0), pos: [10.0, 0.0], score: 1.0 }], ..Default::default() };
        p.move_to(s(0.5), [7.0, 1.0]);
        assert_eq!(p.path.len(), 3);
        assert_eq!(p.pos_at(s(0.5)), Some([7.0, 1.0]));
        p.move_to(s(3.0), [1.0, 2.0]);
        assert_eq!((p.point, p.start, p.path.len()), ([1.0, 2.0], s(3.0), 3));
    }
}
