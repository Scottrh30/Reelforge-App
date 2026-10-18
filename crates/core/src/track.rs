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
    frac: [f32; 2],
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
        Self { half, weights, values, wsum, norm, frac: [centre[0] - cx as f32, centre[1] - cy as f32] }
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
        Self { half, weights, values, wsum, norm, frac: [0.0, 0.0] }
    }

    /// Too flat to track (no texture to lock on to)?
    pub fn is_flat(&self) -> bool {
        self.norm / self.wsum.sqrt() < 2.0
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
        ([(cx + bx) as f32 + ox + self.frac[0], (cy + by) as f32 + oy + self.frac[1]], score)
    }
}


/// Below this search radius (pixels), search exhaustively at full resolution.
const DIRECT_SEARCH: f32 = 16.0;
/// Search radius at each finer pyramid level (around the coarser estimate).
const REFINE: f32 = 3.0;
/// The original patch is trusted over the latest one above this match score.
const ORIGINAL_SCORE: f32 = 0.85;
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
            let (prev, w, h) = v.last().unwrap();
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
    size: f32,
    search: f32,
    pub pos: [f32; 2],
    velocity: [f32; 2],
}

impl FeatureTracker {
    /// Start following the feature at `pos` (pixels of `pyr`'s full level).
    pub fn new(pyr: &Pyramid, pos: [f32; 2], size: f32, search: f32) -> Self {
        let original = Template::new(&pyr.level(0), pos, size.max(9.0));
        let mut t = Self { templates: Vec::new(), original, size, search, pos, velocity: [0.0, 0.0] };
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
                Template::new(&pyr.level(l), [self.pos[0] / k, self.pos[1] / k], (self.size / k).max(9.0))
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
        let (mut p, mut score) = self.templates[top].find(&pyr.level(top), [around[0] / k, around[1] / k], (self.search / k).ceil());
        for l in (0..top).rev() {
            let up = [p[0] * 2.0, p[1] * 2.0];
            (p, score) = self.templates[l].find(&pyr.level(l), up, REFINE);
        }
        (p, score)
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
            // Pin it down with the original look when that still fits closely.
            let (q, s0) = self.original.find(&pyr.level(0), p, REFINE);
            if s0 >= ORIGINAL_SCORE && (q[0] - p[0]).abs() < REFINE && (q[1] - p[1]).abs() < REFINE {
                p = q;
                score = score.max(s0);
            }
            self.velocity = [p[0] - self.pos[0], p[1] - self.pos[1]];
            self.pos = p;
            // Follow gradual changes (turning, light) by refreshing the patch, but
            // only from good matches, so one poor frame doesn't make it wander off.
            if score > 0.9 {
                self.capture(pyr);
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
    },
    Single(PointTrack),
}

impl From<TrackerRepr> for Tracker {
    fn from(r: TrackerRepr) -> Self {
        let mut t = match r {
            TrackerRepr::Multi { mode, points, grid } => Tracker { mode, points, grid },
            TrackerRepr::Single(p) => Tracker { mode: TrackMode::Position, points: vec![p], grid: [0, 0] },
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
        Tracker { mode: TrackMode::Position, points: vec![p], grid: [0, 0] }
    }
}

impl Tracker {
    /// A new tracker at source time `start`, centred on `centre` (media pixels)
    /// of a `layer`-sized picture.
    pub fn new(mode: TrackMode, centre: [f32; 2], layer: [f32; 2], start: Time) -> Self {
        let first = PointTrack { point: centre, start, ..Default::default() };
        let mut t = Tracker { mode: TrackMode::Position, points: vec![first], grid: [0, 0] };
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
    /// (media pixels), in source time order.
    pub fn frames(&self) -> Vec<(Time, Vec<[f32; 2]>)> {
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
}

impl Default for ApplyOptions {
    fn default() -> Self {
        Self { stabilise: false, smoothing: 0.0 }
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
    for ((t, pts, _), h) in motions.iter().zip(&applied) {
        let at = target.source_time(*t);
        let base = base_at(*t);
        if use_pin {
            let pin = if into_points {
                Pin([pts[0], pts[1], pts[2], pts[3]])
            } else {
                let q = base.valid_pin().unwrap_or_else(|| base.pin_of_transform(target_layer, frame));
                q.map(|p| h.apply(p))
            };
            if pin.is_valid() {
                pin_keys.push((at, pin));
            }
            continue;
        }
        let sim = Similar::of(h);
        let anchor_at = [c[0] + base.position[0], c[1] + base.position[1]];
        let moved = h.apply(anchor_at);
        pos_keys.push((at, [moved[0] - c[0], moved[1] - c[1]]));
        if tracker.mode == TrackMode::Transform {
            // Keep the angle continuous from frame to frame.
            let mut a = base.rotation + sim.angle as f32;
            if let Some(prev) = last_angle {
                a = prev + ((a - prev + 180.0).rem_euclid(360.0) - 180.0);
            }
            last_angle = Some(a);
            rot_keys.push((at, a));
            scale_keys.push((at, [base.scale[0] * sim.scale as f32, base.scale[1] * sim.scale as f32]));
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
    if use_pin && pin_keys.is_empty() {
        return None;
    }
    put(&mut anim.position, pos_keys);
    put(&mut anim.rotation, rot_keys);
    put(&mut anim.scale, scale_keys);
    put(&mut anim.pin, pin_keys);
    Some(anim)
}

/// A mesh warp laying `target` over a mesh tracker's grid at every tracked
/// frame it overlaps (its keys outside that range are kept). `layer` is the
/// tracked clip's picture size and `frame` the sequence's.
pub fn follow_mesh(tracked: &Clip, tracker: &Tracker, layer: [f32; 2], frame: [f32; 2], target: &Clip) -> Option<crate::meshwarp::MeshWarp> {
    if tracker.mode != TrackMode::Mesh {
        return None;
    }
    let to_seq = |src: Time| tracked.timeline_of_media(src).unwrap_or(tracked.timeline_start);
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
            let points = pts.iter().map(|p| m.apply([p[0] - layer[0] / 2.0, p[1] - layer[1] / 2.0])).collect();
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
    let mut m = mask.clone();
    let keys: Vec<(Time, Vec<[f32; 2]>)> = if tracker.mode == TrackMode::Mesh {
        // Each mask point keeps its place on the bending grid.
        let grid = |pts: &[[f32; 2]]| crate::meshwarp::MeshShape { cols: tracker.grid[0], rows: tracker.grid[1], points: pts.to_vec() };
        let reference = grid(&ref_pts);
        let at: Vec<crate::meshwarp::MeshCoord> = base.iter().map(|q| reference.locate(*q)).collect::<Option<_>>()?;
        frames.iter().map(|(t, p)| (*t, at.iter().map(|c| grid(p).apply(*c)).collect())).collect()
    } else {
        frames
            .iter()
            .filter_map(|(at, p)| {
                let h = motion_between(tracker.mode, &ref_pts, p)?;
                Some((*at, base.iter().map(|q| h.apply(*q)).collect()))
            })
            .collect()
    };
    let (a, b) = (keys.iter().map(|k| k.0).min()?, keys.iter().map(|k| k.0).max()?);
    m.keys.retain(|k| k.at < a || k.at > b);
    for (at, points) in keys {
        let i = m.keys.partition_point(|k| k.at < at);
        m.keys.insert(i, crate::mask::ShapeKey { at, points });
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

    fn keys<T: Copy>(c: &crate::anim::Curve<T>) -> Vec<(Time, T)> {
        c.0.iter().map(|k| (k.at, k.value)).collect()
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
        let stab = ApplyOptions { stabilise: true, smoothing: 0.0 };
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
        Tracker { mode: TrackMode::Transform, points: vec![a, b], grid: [0, 0] }
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
        let st = follow_keys(&tracked, &tr, layer, frame, &tracked, layer, ApplyOptions { stabilise: true, smoothing: 0.0 }).unwrap();
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
        Tracker { mode: TrackMode::CornerPin, points, grid: [0, 0] }
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
        let tr = Tracker { mode: TrackMode::Mesh, points, grid: [2, 1] };
        let w = follow_mesh(&tracked, &tr, layer, frame, &target).unwrap();
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
        for opts in [ApplyOptions::default(), ApplyOptions { stabilise: true, smoothing: 10.0 }] {
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
        let st = follow_keys(&tracked, &tr, layer, frame, &tracked, layer, ApplyOptions { stabilise: true, smoothing: 0.0 }).unwrap();
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
        let anim = follow_keys(&clip, &tr, layer, frame, &clip, layer, ApplyOptions { stabilise: true, smoothing: 10.0 }).unwrap();
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
