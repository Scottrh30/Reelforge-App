//! The roto brush in the editor: paint over a subject at the playhead to cut
//! a mask round it, then carry the mask on through the shot in the
//! background (see `reelforge_core::rotobrush`).

use super::{Editor, TrackJob, TrackMsg, LOCKED};
use crossbeam_channel::unbounded;
use reelforge_core::inpaint::{self, Image};
use reelforge_core::mask::{Mask, MaskShape, RotoStroke, ShapeKey};
use reelforge_core::rotobrush::{self, band_hints, paint, segment, stroke_hints, Hints, Stroke};
use reelforge_core::transform::Homography;
use reelforge_core::undo::SetClipVisual;
use reelforge_core::*;
use std::sync::atomic::Ordering;
use reelforge_sam as sam;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The longest side frames are cut at.
pub const ROTO_SIDE: u32 = 640;

/// What a propagation run found: the mask, and its shape at each source time.
pub struct RotoRun {
    pub mask: usize,
    pub shapes: Vec<(Time, Vec<[f32; 2]>)>,
    /// It lost the subject before the end.
    pub lost: bool,
}

/// `poly` (layer pixels) filled, on a `w`×`h` grid that's `k` × the layer.
fn fill_poly(poly: &[[f32; 2]], k: [f32; 2], w: usize, h: usize) -> Vec<bool> {
    let p: Vec<[f32; 2]> = poly.iter().map(|q| [q[0] * k[0], q[1] * k[1]]).collect();
    let mut out = vec![false; w * h];
    let mut xs: Vec<f32> = Vec::new();
    for y in 0..h {
        let cy = y as f32 + 0.5;
        xs.clear();
        for i in 0..p.len() {
            let (a, b) = (p[i], p[(i + 1) % p.len()]);
            if (a[1] <= cy) != (b[1] <= cy) {
                xs.push(a[0] + (cy - a[1]) / (b[1] - a[1]) * (b[0] - a[0]));
            }
        }
        xs.sort_by(f32::total_cmp);
        for pair in xs.chunks(2) {
            if let [x0, x1] = pair {
                let (a, b) = ((x0 - 0.5).ceil().max(0.0) as usize, ((x1 - 0.5).floor() + 1.0).clamp(0.0, w as f32) as usize);
                for x in a..b.min(w) {
                    out[y * w + x] = true;
                }
            }
        }
    }
    out
}

/// The last frame encoded for the object model (a second or two's work):
/// every stroke painted on one frame cuts from it.
type Encoded = Mutex<Option<((PathBuf, i64, usize, usize), Arc<sam::Embedding>)>>;
static ENCODED: Encoded = Mutex::new(None);

/// Frame `at` of `path` (`rgba`, `w`×`h`) encoded for the object model.
pub fn encoded(path: &Path, at: Time, rgba: &[u8], w: usize, h: usize) -> Option<Arc<sam::Embedding>> {
    let key = (path.to_path_buf(), at.0, w, h);
    // One at a time: a stroke made while the frame's being got ready waits for it.
    static BUSY: Mutex<()> = Mutex::new(());
    let _one = BUSY.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((k, e)) = ENCODED.lock().unwrap_or_else(std::sync::PoisonError::into_inner).as_ref() {
        if *k == key {
            return Some(e.clone());
        }
    }
    let e = Arc::new(sam::embed(rgba, w, h)?);
    *ENCODED.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some((key, e.clone()));
    Some(e)
}

/// A few points along each stroke (picture pixels) to prompt the model
/// with: on the subject for its strokes, off it for the others.
fn stroke_points(strokes: &[Stroke], side: f32) -> Vec<([f32; 2], bool)> {
    let mut out = Vec::new();
    for s in strokes {
        let Some(&first) = s.points.first() else { continue };
        let gap = (s.radius * 4.0).max(side / 16.0);
        let mut picked = vec![first];
        let mut run = 0.0;
        for w in s.points.windows(2) {
            run += ((w[1][0] - w[0][0]).powi(2) + (w[1][1] - w[0][1]).powi(2)).sqrt();
            if run >= gap {
                picked.push(w[1]);
                run = 0.0;
            }
        }
        // At most five along one stroke, spread over it.
        let n = picked.len();
        let keep = n.min(5);
        out.extend((0..keep).map(|i| (picked[if keep == 1 { 0 } else { i * (n - 1) / (keep - 1) }], s.fg)));
    }
    // The model takes a handful best: the latest strokes count most.
    let skip = out.len().saturating_sub(16);
    out.split_off(skip)
}

/// The point inside `mask` farthest from its edge.
fn deepest(mask: &[bool], w: usize, h: usize) -> Option<[f32; 2]> {
    // Two-pass chamfer distance.
    let big = (w + h) as u32 * 3;
    let mut d: Vec<u32> = mask.iter().map(|&m| if m { big } else { 0 }).collect();
    for y in 0..h {
        for x in 0..w {
            let i = y * w + x;
            if x > 0 {
                d[i] = d[i].min(d[i - 1] + 3);
            } else {
                d[i] = d[i].min(3);
            }
            if y > 0 {
                d[i] = d[i].min(d[i - w] + 3);
                if x > 0 {
                    d[i] = d[i].min(d[i - w - 1] + 4);
                }
                if x + 1 < w {
                    d[i] = d[i].min(d[i - w + 1] + 4);
                }
            } else {
                d[i] = d[i].min(3);
            }
        }
    }
    for y in (0..h).rev() {
        for x in (0..w).rev() {
            let i = y * w + x;
            d[i] = d[i].min(if x + 1 < w { d[i + 1] + 3 } else { 3 });
            d[i] = d[i].min(if y + 1 < h { d[i + w] + 3 } else { 3 });
            if y + 1 < h {
                if x + 1 < w {
                    d[i] = d[i].min(d[i + w + 1] + 4);
                }
                if x > 0 {
                    d[i] = d[i].min(d[i + w - 1] + 4);
                }
            }
        }
    }
    let (i, &v) = d.iter().enumerate().max_by_key(|(_, v)| **v)?;
    (v > 0).then(|| [(i % w) as f32 + 0.5, (i / w) as f32 + 0.5])
}

fn bounds(mask: &[bool], w: usize) -> Option<[f32; 4]> {
    let mut b = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
    for (i, _) in mask.iter().enumerate().filter(|(_, m)| **m) {
        let (x, y) = ((i % w) as f32, (i / w) as f32);
        b = [b[0].min(x), b[1].min(y), b[2].max(x + 1.0), b[3].max(y + 1.0)];
    }
    (b[0] <= b[2]).then_some(b)
}

/// What the object model makes of the strokes (`strokes`, picture pixels;
/// `fg`, `bg` painted out) and where the subject was expected (`was`): the
/// subject as a mask, or `None` if it found nothing that fits the strokes.
fn model_cut(e: &sam::Embedding, strokes: &[Stroke], was: Option<&[bool]>, fg: &[bool], bg: &[bool]) -> Option<Vec<bool>> {
    let (w, h) = (e.w, e.h);
    let side = w.max(h) as f32;
    let mut points = stroke_points(strokes, side);
    // Where it was expected: a box round that and its strokes, a point deep in it.
    let bbox = was.and_then(|m| {
        let mut b = bounds(m, w)?;
        if let Some(f) = bounds(fg, w) {
            b = [b[0].min(f[0]), b[1].min(f[1]), b[2].max(f[2]), b[3].max(f[3])];
        }
        let pad = side / 50.0;
        Some([(b[0] - pad).max(0.0), (b[1] - pad).max(0.0), (b[2] + pad).min(w as f32), (b[3] + pad).min(h as f32)])
    });
    if let Some(m) = was {
        if !points.iter().any(|p| p.1) {
            if let Some(p) = deepest(m, w, h) {
                points.push((p, true));
            }
        }
    }
    let cut = sam::cut(e, &sam::Prompt { points: &points, bbox, mask: None })?;
    let mask: Vec<bool> = cut.logits.iter().map(|&v| v > 0.0).collect();
    let mask = rotobrush::largest(&mask, w, h);
    let n = mask.iter().filter(|&&m| m).count();
    if n < 9 {
        return None;
    }
    // It must agree with the strokes: most of the subject's inside it, most of the rest not.
    let share = |s: &[bool]| {
        let all = s.iter().filter(|&&b| b).count();
        (all > 0).then(|| s.iter().zip(&mask).filter(|(a, b)| **a && **b).count() as f32 / all as f32)
    };
    if share(fg).is_some_and(|f| f < 0.6) || share(bg).is_some_and(|b| b > 0.4) {
        return None;
    }
    Some(mask)
}

/// Cut one frame (`img`, `k` × the layer's pixels): from the strokes painted
/// on it, and where the subject was expected (`prior`, layer pixels) with
/// the cut free `band` pixels either side of it. With the frame encoded for
/// the object model (`model`), it picks out the object and the edge is then
/// fitted to the colours there; otherwise (or if it finds nothing that fits
/// the strokes) the cut goes by colours and edges alone. The outline in
/// layer pixels.
pub fn cut_frame(img: &Image, k: [f32; 2], strokes: &[Stroke], prior: Option<&[[f32; 2]]>, band: usize, model: Option<&sam::Embedding>) -> Option<Vec<[f32; 2]>> {
    let (w, h) = (img.w, img.h);
    let scaled: Vec<Stroke> = strokes.iter().map(|s| Stroke { fg: s.fg, radius: s.radius * k[0], points: s.points.iter().map(|p| [p[0] * k[0], p[1] * k[1]]).collect() }).collect();
    let fg = paint(&scaled, 1.0, w, h, true);
    let bg = paint(&scaled, 1.0, w, h, false);
    let was = prior.filter(|p| p.len() >= 3).map(|poly| fill_poly(poly, k, w, h));
    if was.is_none() && !fg.iter().any(|&b| b) {
        return None;
    }
    let seen = model.filter(|e| (e.w, e.h) == (w, h)).and_then(|e| model_cut(e, &scaled, was.as_deref(), &fg, &bg));
    let cut = match (seen, was) {
        (Some(m), _) => {
            // Its edge (a quarter of the model's resolution) fitted to the
            // picture, in a narrow band; kept as it was if that goes astray.
            let (f, b, p) = band_hints(&m, w, h, (w.max(h) / 160).max(2), &fg, &bg);
            let fitted = segment(img, &Hints { fg: &f, bg: &b, prior: Some(&p) });
            let (a0, a1) = (m.iter().filter(|&&v| v).count() as f32, fitted.iter().filter(|&&v| v).count() as f32);
            if a1 > a0 * 0.85 && a1 < a0 * 1.15 {
                fitted
            } else {
                m
            }
        }
        (None, Some(was)) => {
            let (f, b, p) = band_hints(&was, w, h, band, &fg, &bg);
            segment(img, &Hints { fg: &f, bg: &b, prior: Some(&p) })
        }
        (None, None) => {
            let (f, b) = stroke_hints(&fg, &bg, w, h);
            segment(img, &Hints { fg: &f, bg: &b, prior: None })
        }
    };
    let line = rotobrush::outline(&cut, w, h, 0.8);
    (line.len() >= 3).then(|| line.iter().map(|p| [p[0] / k[0], p[1] / k[1]]).collect())
}

/// What [`Editor::replace_background`] puts behind a clip.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Behind {
    /// A solid colour (RGB).
    Color([u8; 3]),
    /// The shot itself, blurred.
    Blur,
    /// Footage or a picture from the project.
    Media(MediaId),
}

/// Where the main subject of a frame is (frame pixels), with the frame
/// encoded for the object model: of a few points round the middle, the one
/// whose cut looks most like a subject — not tiny, not most of the frame,
/// not reaching round three sides (that's the background), and near the
/// middle.
pub fn find_subject(img: &Image, model: &sam::Embedding) -> Option<[f32; 2]> {
    let (w, h) = (img.w as f32, img.h as f32);
    let tries = [[0.5, 0.5], [0.5, 0.38], [0.5, 0.62], [0.38, 0.5], [0.62, 0.5], [0.5, 0.26], [0.32, 0.42], [0.68, 0.42]];
    let band = (img.w.max(img.h) / 40).max(5);
    let mut best: Option<([f32; 2], f32)> = None;
    for t in tries {
        let p = [t[0] * w, t[1] * h];
        let st = [Stroke { fg: true, radius: 2.0, points: vec![p] }];
        let Some(line) = cut_frame(img, [1.0, 1.0], &st, None, band, Some(model)) else { continue };
        let area = (0..line.len()).map(|j| { let (a, b) = (line[j], line[(j + 1) % line.len()]); a[0] * b[1] - b[0] * a[1] }).sum::<f32>().abs() / 2.0 / (w * h);
        if !(0.015..0.85).contains(&area) {
            continue;
        }
        let (x0, x1) = line.iter().fold((f32::MAX, f32::MIN), |m, q| (m.0.min(q[0]), m.1.max(q[0])));
        let (y0, y1) = line.iter().fold((f32::MAX, f32::MIN), |m, q| (m.0.min(q[1]), m.1.max(q[1])));
        let (mx, my) = (w * 0.015, h * 0.015);
        let sides = [x0 <= mx, x1 >= w - mx, y0 <= my, y1 >= h - my].iter().filter(|&&b| b).count();
        if sides >= 3 {
            continue;
        }
        let n = line.len() as f32;
        let c = line.iter().fold([0.0, 0.0], |s, q| [s[0] + q[0] / n, s[1] + q[1] / n]);
        let off = (((c[0] / w - 0.5).powi(2) + (c[1] / h - 0.5).powi(2)).sqrt() / 0.7).min(1.0);
        let score = area.sqrt() * (1.0 - off) * if sides == 2 { 0.7 } else { 1.0 };
        if best.is_none_or(|b| score > b.1) {
            best = Some((p, score));
        }
    }
    best.map(|b| b.0)
}

impl Editor {
    /// Get the frame at the playhead of clip `id` ready for the roto brush
    /// (encoded for the object model) in the background, so the first
    /// stroke on it doesn't wait.
    pub fn roto_prepare(&mut self, id: ClipId) {
        let Some((_, c)) = self.seq().find_clip(id) else { return };
        let Some(media) = self.project.media(c.media).filter(|m| m.info.video.is_some() && c.title.is_none() && c.nested.is_none() && c.scene.is_none()).cloned() else { return };
        let at = self.track_time(c);
        let _ = std::thread::Builder::new().name("roto brush prepare".into()).spawn(move || {
            let Ok(mut s) = reelforge_media::FrameSource::open(&media.path, Some((ROTO_SIDE, ROTO_SIDE))) else { return };
            if let Some(f) = s.frame_at_blocking(at, Duration::from_secs(10)) {
                encoded(&media.path, at, &f.pixels.to_rgba(), f.width as usize, f.height as usize);
            }
        });
    }

    /// Paint roto brush stroke `stroke` (layer pixels) over clip `id` at the
    /// playhead: the subject's outline there is cut from every stroke
    /// painted on this frame, into mask `mask` (a new one if `None`). One
    /// undo step. A new mask is an Object to remove (for the Object removal
    /// effect) when `removal`. Returns the mask's index.
    pub fn roto_stroke(&mut self, id: ClipId, mask: Option<usize>, stroke: Stroke, removal: bool) -> Option<usize> {
        self.commit_live_edit();
        if self.seq().is_locked(id) {
            self.status = LOCKED.into();
            return None;
        }
        let (_, c) = self.seq().find_clip(id)?;
        let Some(media) = self.project.media(c.media).filter(|m| m.info.video.is_some() && c.title.is_none() && c.nested.is_none() && c.scene.is_none()).cloned() else {
            self.status = "The roto brush works on footage or a picture".into();
            return None;
        };
        let layer = self.layer_size(c);
        let src = self.mask_time(c);
        let at = self.track_time(c);
        let frame = reelforge_media::FrameSource::open(&media.path, Some((ROTO_SIDE, ROTO_SIDE))).ok().and_then(|mut s| s.frame_at_blocking(at, Duration::from_secs(10)));
        let Some(f) = frame else {
            self.status = "Couldn't read the frame at the playhead".into();
            return None;
        };
        let (w, h) = (f.width as usize, f.height as usize);
        let rgba = f.pixels.to_rgba();
        let img = Image::from_rgba(&rgba, w, h);
        let model = encoded(&media.path, at, &rgba, w, h);
        let k = [w as f32 / layer[0].max(1.0), h as f32 / layer[1].max(1.0)];
        let mut v = self.visual(id)?;
        let index = match mask.filter(|&i| i < v.masks.len()) {
            Some(i) => i,
            None => {
                let name = if removal { "Object to remove" } else { "Roto brush" };
                v.masks.push(Mask { name: name.into(), shape: MaskShape::Freehand, fill_area: removal, ..Mask::default() });
                v.masks.len() - 1
            }
        };
        let m = &mut v.masks[index];
        let mut here: Vec<Stroke> = m.roto.iter().filter(|r| r.at == src).map(|r| r.stroke.clone()).collect();
        here.push(stroke.clone());
        // A shape already here (carried on to this frame, or cut before): the
        // strokes correct it, the cut free well round it.
        let shape = m.points_at(src);
        let prior = (shape.len() >= 3 && m.shape == MaskShape::Freehand).then_some(shape);
        let band = (w.max(h) / 6).max(8);
        let Some(line) = cut_frame(&img, k, &here, prior.as_deref(), band, model.as_deref()) else {
            self.status = "Paint over the subject to pick it out (hold Alt to paint over what isn't it)".into();
            return mask;
        };
        let n = line.len();
        m.shape = MaskShape::Freehand;
        m.curves.clear();
        m.feathers.clear();
        if m.keys.is_empty() {
            m.points = line.clone();
            m.keys = vec![ShapeKey::new(src, line)];
        } else {
            m.set_points_at(src, line);
            if let Some(k) = m.keys.iter_mut().find(|k| k.at == src) {
                k.curves.clear();
            }
        }
        m.roto.push(RotoStroke { at: src, stroke });
        let name = m.name.clone();
        self.set_visual_live(id, v);
        self.commit_live_edit();
        self.status = format!("{name}: {n} points round the subject. Paint more to correct it (Alt: what isn't it), then carry it on through the shot");
        Some(index)
    }

    /// One click on clip `id` at `point` (layer pixels): the object there is
    /// picked out into a new roto mask (an Object to remove when `removal`)
    /// and carried on through the rest of the clip in the background (back
    /// to its start, from its last frame). Returns the mask's index.
    pub fn roto_pick(&mut self, id: ClipId, point: [f32; 2], removal: bool) -> Option<usize> {
        if self.tracking.is_some() {
            self.status = "Wait for the tracking or analysis already running to finish (or stop it)".into();
            return None;
        }
        let index = self.roto_stroke(id, None, Stroke { fg: true, radius: 2.0, points: vec![point] }, removal)?;
        let (_, c) = self.seq().find_clip(id)?;
        let step = self.seq().frame_rate.frame_duration();
        let forward = self.playback.playhead + step < c.timeline_end();
        if c.duration > step {
            self.start_roto_propagation(id, index, forward);
        }
        Some(index)
    }

    /// One click: the background of clip `id` cut away through the whole
    /// clip. The main subject at the playhead (on the clip's first frame if
    /// the playhead isn't on it) is found by itself — of a few points round
    /// the middle, the one the object model cuts out most like a subject:
    /// a good size, near the middle, not reaching right round the frame —
    /// cut into a mask with a refined (soft) edge, and carried through the
    /// clip in the background, forwards then back. Returns the mask's index.
    pub fn remove_background(&mut self, id: ClipId) -> Option<usize> {
        if self.tracking.is_some() {
            self.status = "Wait for the tracking or analysis already running to finish (or stop it)".into();
            return None;
        }
        let (_, c) = self.seq().find_clip(id)?;
        let c = c.clone();
        let Some(media) = self.project.media(c.media).filter(|m| m.info.video.is_some() && c.title.is_none() && c.nested.is_none() && c.scene.is_none()).cloned() else {
            self.status = "Background removal works on footage or a picture".into();
            return None;
        };
        if !(c.timeline_start..c.timeline_end()).contains(&self.playback.playhead) {
            self.seek(c.timeline_start);
        }
        let layer = self.layer_size(&c);
        let at = self.track_time(&c);
        let Some(f) = reelforge_media::FrameSource::open(&media.path, Some((ROTO_SIDE, ROTO_SIDE))).ok().and_then(|mut s| s.frame_at_blocking(at, Duration::from_secs(10))) else {
            self.status = "Couldn't read the frame at the playhead".into();
            return None;
        };
        let (w, h) = (f.width as usize, f.height as usize);
        let rgba = f.pixels.to_rgba();
        let img = Image::from_rgba(&rgba, w, h);
        let Some(model) = encoded(&media.path, at, &rgba, w, h) else {
            self.status = "Couldn't look at the frame for a subject".into();
            return None;
        };
        let Some(point) = find_subject(&img, &model) else {
            self.status = "No clear subject found here: try another frame, or + Pick object and click it".into();
            return None;
        };
        let k = [w as f32 / layer[0].max(1.0), h as f32 / layer[1].max(1.0)];
        let index = self.roto_stroke(id, None, Stroke { fg: true, radius: 2.0, points: vec![[point[0] / k[0], point[1] / k[1]]] }, false)?;
        // Named for what it does, with a soft edge for hair.
        if let Some(mut v) = self.visual(id) {
            if let Some(m) = v.masks.get_mut(index) {
                m.name = "Background removal".into();
                m.refine = Some(reelforge_core::mask::RefineEdge { decontaminate: 100.0, ..Default::default() });
            }
            self.set_visual_live(id, v);
            self.commit_live_edit();
        }
        let step = self.seq().frame_rate.frame_duration();
        let here = self.playback.playhead;
        if here + step < c.timeline_end() {
            self.roto_then_back = Some((id, index, here));
            self.start_roto_propagation(id, index, true);
        } else if here > c.timeline_start {
            self.start_roto_propagation(id, index, false);
        }
        if self.tracking.is_some() {
            self.status = "Background removal: the subject is picked out; following it through the clip…".into();
        }
        Some(index)
    }

    /// What goes behind clip `id` once its background is cut away: a layer
    /// over the same stretch of time on the nearest free video track below
    /// it (a new track there if none is free) — a solid colour, a blur of
    /// the shot itself (its masks off, so the whole picture, softened and a
    /// little bigger to hide the blurred edges), or footage or a picture from
    /// the project, scaled to fill the frame. One undo step.
    pub fn replace_background(&mut self, id: ClipId, behind: Behind) -> Option<ClipId> {
        use reelforge_core::undo::{AddClip, AddTrack, Batch};
        self.commit_live_edit();
        let seq = self.active;
        let s = self.seq();
        let (ti, c) = s.find_clip(id)?;
        let c = c.clone();
        let (start, end) = (c.timeline_start, c.timeline_end());
        let free = |t: &reelforge_core::project::Track| t.kind == TrackKind::Video && !t.locked && t.clips.iter().all(|o| o.timeline_end() <= start || o.timeline_start >= end);
        let below = (0..ti).rev().find(|&j| s.tracks.get(j).is_some_and(free));
        let new_id = ClipId(self.project.alloc_id());
        let (w, h) = (self.seq().width as f32, self.seq().height as f32);
        let mut layer = match behind {
            Behind::Color(rgb) => Clip::new_plane(new_id, start, end - start, [rgb[0], rgb[1], rgb[2], 255]),
            Behind::Blur => {
                let mut b = c.clone();
                b.id = new_id;
                b.link = None;
                b.masks.clear();
                b.effects = vec![{
                    let mut e = reelforge_core::Effect::new(reelforge_core::EffectKind::Blur);
                    e.params[0].value = (h / 25.0).clamp(8.0, 120.0);
                    e
                }];
                b.transform.scale = b.transform.scale.map(|k| k * 1.06);
                b
            }
            Behind::Media(m) => {
                let Some(asset) = self.project.media(m).cloned() else {
                    self.status = "That's not in the project any more".into();
                    return None;
                };
                let Some(v) = asset.info.video.as_ref() else {
                    self.status = format!("{} has no picture to put behind", asset.name);
                    return None;
                };
                let len = if self.project.is_still(m) { end - start } else { (end - start).min(asset.info.duration.max(Time(1))) };
                let mut l = Clip::new(new_id, m, start, Time::ZERO, len, None);
                // Filling the frame.
                let k = (w / v.width.max(1) as f32).max(h / v.height.max(1) as f32);
                l.transform.scale = [k, k];
                l
            }
        };
        layer.label = None;
        let mut cmds: Vec<Box<dyn reelforge_core::undo::Command>> = Vec::new();
        let track = match below {
            Some(j) => j,
            None => {
                // A new video track just under the clip's.
                let t = AddTrack { seq, index: ti, track: reelforge_core::project::Track::new(self.seq().next_track_name(TrackKind::Video), TrackKind::Video) };
                cmds.push(Box::new(t));
                ti
            }
        };
        cmds.push(Box::new(AddClip { seq, track, clip: layer }));
        let what = match behind {
            Behind::Color(_) => "a colour",
            Behind::Blur => "a blur of the shot",
            Behind::Media(_) => "the picture",
        };
        self.execute(Box::new(Batch { name: "Replace background".into(), cmds }));
        let name = self.seq().tracks.get(track).map_or(String::new(), |t| t.name.clone());
        self.status = format!("Behind it now: {what}, on {name}");
        Some(new_id)
    }

    /// Carry roto mask `index` of clip `id` on from the playhead through the
    /// rest of the clip (`forward`) or back to its start, in the background:
    /// at each frame the outline moves with what's in it and is cut again
    /// round there.
    pub fn start_roto_propagation(&mut self, id: ClipId, index: usize, forward: bool) {
        self.commit_live_edit();
        if self.tracking.is_some() {
            self.status = "Wait for the tracking or analysis already running to finish (or stop it)".into();
            return;
        }
        if self.seq().is_locked(id) {
            self.status = LOCKED.into();
            return;
        }
        let Some((_, c)) = self.seq().find_clip(id) else { return };
        let Some(m) = c.masks.get(index).cloned() else { return };
        let Some(media) = self.project.media(c.media).filter(|m| m.info.video.is_some()).cloned() else { return };
        let start = m.points_at(self.mask_time(c));
        if start.len() < 3 {
            self.status = "Paint over the subject with the roto brush first".into();
            return;
        }
        let layer = self.layer_size(c);
        let step = self.seq().frame_rate.frame_duration();
        let mut times = Vec::new();
        let mut t = self.playback.playhead.max(c.timeline_start).min(c.timeline_end() - step);
        loop {
            times.push((c.source_time(t), c.media_at(t)));
            t = if forward { t + step } else { t - step };
            if t < c.timeline_start || t >= c.timeline_end() {
                break;
            }
        }
        if times.len() < 2 {
            self.status = if forward { "Already at the clip's end" } else { "Already at the clip's start" }.into();
            return;
        }
        let strokes = m.roto.clone();
        let (tx, rx) = unbounded();
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop = cancel.clone();
        self.playback.pause();
        std::thread::Builder::new()
            .name("roto brush".into())
            .spawn(move || {
                let r = (|| -> Result<Option<RotoRun>, String> {
                    let mut src = reelforge_media::FrameSource::open(&media.path, Some((ROTO_SIDE, ROTO_SIDE))).map_err(|e| format!("{e:#}"))?;
                    let wait = Duration::from_secs(20);
                    let mut shapes = vec![(times[0].0, start.clone())];
                    let mut prev: Option<(Vec<f32>, usize, usize)> = None;
                    let mut poly = start;
                    let mut lost = false;
                    let n = times.len();
                    for (i, (s_t, m_t)) in times.iter().enumerate() {
                        if stop.load(Ordering::Relaxed) {
                            return Ok(None);
                        }
                        let f = src.frame_at_blocking(*m_t, wait).ok_or_else(|| format!("no frame at {m_t}"))?;
                        let (w, h) = (f.width as usize, f.height as usize);
                        let rgba = f.pixels.to_rgba();
                        let img = Image::from_rgba(&rgba, w, h);
                        let luma = img.luma();
                        let k = [w as f32 / layer[0].max(1.0), h as f32 / layer[1].max(1.0)];
                        if i > 0 {
                            // The outline moved with what's in and round it.
                            let Some((pl, pw, ph)) = prev.as_ref() else { continue };
                            let moved = if (*pw, *ph) == (w, h) {
                                let near = inpaint::dilate(&fill_poly(&poly, k, w, h), w, h, (w.max(h) / 30).max(4));
                                let leave: Vec<bool> = near.iter().map(|&b| !b).collect();
                                inpaint::motion(pl, &luma, w, h, &leave)
                            } else {
                                None
                            };
                            let hm = moved.unwrap_or(Homography::IDENTITY);
                            let guess: Vec<[f32; 2]> = poly
                                .iter()
                                .map(|p| {
                                    let q = hm.apply([p[0] * k[0], p[1] * k[1]]);
                                    [q[0] / k[0], q[1] / k[1]]
                                })
                                .collect();
                            let here: Vec<Stroke> = strokes.iter().filter(|r| r.at == *s_t).map(|r| r.stroke.clone()).collect();
                            let band = (w.max(h) / 40).max(5);
                            let model = sam::embed(&rgba, w, h);
                            match cut_frame(&img, k, &here, Some(&guess), band, model.as_ref()) {
                                Some(line) => {
                                    // Lost if it shrinks to almost nothing or balloons.
                                    let area = |p: &[[f32; 2]]| (0..p.len()).map(|j| { let (a, b) = (p[j], p[(j + 1) % p.len()]); a[0] * b[1] - b[0] * a[1] }).sum::<f32>().abs() / 2.0;
                                    let (a0, a1) = (area(&poly), area(&line));
                                    if a1 < a0 * 0.3 || a1 > a0 * 3.0 {
                                        lost = true;
                                        break;
                                    }
                                    poly = line;
                                }
                                None => {
                                    lost = true;
                                    break;
                                }
                            }
                            shapes.push((*s_t, poly.clone()));
                        }
                        prev = Some((luma, w, h));
                        let _ = tx.send(TrackMsg::LiveShape((i + 1) as f32 / n as f32, *s_t, poly.clone()));
                    }
                    Ok(Some(RotoRun { mask: index, shapes, lost }))
                })();
                let _ = tx.send(TrackMsg::RotoDone(r));
            })
            .map_err(|e| log::error!("couldn't start the roto brush thread: {e}")).ok();
        self.tracking = Some(TrackJob { seq: self.active, clip: id, rx, cancel, progress: 0.0, mask: None, through: None, camera: false, fill: None, roto: Some(index), ai: None, live: Vec::new(), live_shape: None });
        self.status = format!("Carrying {} {}…", m.name, if forward { "forwards" } else { "backwards" });
    }

    pub(super) fn finish_roto(&mut self, job: &TrackJob, r: Result<Option<RotoRun>, String>) {
        let back = self.roto_then_back.take();
        let carry_back = matches!(&r, Ok(Some(_)));
        self.finish_roto_run(job, r);
        // Background removal: having carried it forwards, carry it back to the start.
        if let Some((id, mask, from)) = back.filter(|b| carry_back && b.0 == job.clip) {
            let start = self.seq().find_clip(id).map(|(_, c)| c.timeline_start);
            if start.is_some_and(|s| from > s) {
                self.playback.playhead = from;
                self.start_roto_propagation(id, mask, false);
            }
        }
    }

    fn finish_roto_run(&mut self, job: &TrackJob, r: Result<Option<RotoRun>, String>) {
        match r {
            Err(e) => self.status = format!("Roto brush failed: {e}"),
            Ok(None) => self.status = "Roto brush stopped".into(),
            Ok(Some(run)) => {
                let Some(c) = self.project.sequences.get(job.seq).and_then(|s| s.find_clip(job.clip)).map(|(_, c)| c.clone()) else { return };
                let old = c.visual();
                let mut new = old.clone();
                let Some(m) = new.masks.get_mut(run.mask) else { return };
                let (Some(lo), Some(hi)) = (run.shapes.iter().map(|s| s.0).min(), run.shapes.iter().map(|s| s.0).max()) else { return };
                // Each frame's outline is cut afresh, so it boils (edges
                // creeping frame to frame): line their points up and smooth
                // them over time (about a frame and a half either way), the
                // frame it started from kept as painted.
                let mut shapes = run.shapes.clone();
                let n = shapes.iter().map(|s| s.1.len()).max().unwrap_or(0).clamp(48, 160);
                reelforge_core::mask::conform_outlines(&mut shapes, n);
                let mut smooth = Mask { keys: shapes.iter().map(|(at, p)| ShapeKey::new(*at, p.clone())).collect(), ..Mask::default() };
                let fps = self.project.sequences.get(job.seq).map_or(25.0, |s| s.frame_rate.fps());
                smooth.smooth_keys(1.5 / fps.max(1.0));
                if let (Some(k), Some(first)) = (smooth.keys.iter_mut().find(|k| k.at == run.shapes[0].0), shapes.first()) {
                    k.points = first.1.clone();
                }
                // The run's shapes replace the keys over its span.
                m.keys.retain(|k| k.at < lo || k.at > hi);
                m.keys.extend(smooth.keys);
                m.keys.sort_by_key(|k| k.at);
                m.curves.clear();
                let (name, n) = (m.name.clone(), run.shapes.len() - 1);
                self.execute(Box::new(SetClipVisual { seq: job.seq, id: job.clip, old, new }));
                self.status = if run.lost {
                    format!("{name} carried over {n} frames, then lost the subject: go to the next frame, paint to correct it and carry it on from there")
                } else {
                    format!("{name} carried over {n} frames: scrub through, paint over any frame to correct it, and carry it on from there")
                };
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reelforge_core::project::MediaAsset;

    /// A box moving across a noisy background: a click on it in the first
    /// frame outlines it, and carrying it forwards keeps the outline on it.
    #[test]
    fn a_painted_subject_is_followed_through_the_shot() {
        let dir = std::env::temp_dir().join(format!("rf-engine-roto-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a.mp4");
        let ok = std::process::Command::new("ffmpeg")
            .args(["-y", "-loglevel", "error", "-filter_complex", "color=c=0x3050a0:s=160x90:r=25:d=0.6,noise=alls=20:allf=t[bg];color=c=orange:s=30x30:r=25:d=0.6[fg];[bg][fg]overlay=x='30+t*60':y=30", "-pix_fmt", "yuv420p"])
            .arg(&f)
            .status()
            .is_ok_and(|s| s.success());
        if !ok {
            return;
        }
        let mut e = Editor::new(None, None);
        let info = reelforge_media::probe(&f).unwrap();
        e.project.media.push(MediaAsset { id: MediaId(1), path: f, name: "a.mp4".into(), info, conform: None });
        (e.project.sequences[0].width, e.project.sequences[0].height) = (160, 90);
        let id = ClipId(e.project.alloc_id());
        let c = Clip::new(id, MediaId(1), Time::ZERO, Time::ZERO, Time::from_secs_f64(0.6), None);
        let v = e.seq().tracks.iter().position(|tr| tr.kind == TrackKind::Video).unwrap();
        e.project.sequences[0].tracks[v].clips.push(c);
        e.playback.playhead = Time::ZERO;
        // One click on it: picked out (as an Object to remove) and carried on by itself.
        let i = e.roto_pick(id, [45.0, 45.0], true).expect("a mask");
        let xs = |pts: &[[f32; 2]]| (pts.iter().map(|p| p[0]).fold(f32::MAX, f32::min), pts.iter().map(|p| p[0]).fold(f32::MIN, f32::max));
        let m = e.seq().find_clip(id).unwrap().1.masks[i].clone();
        assert!(m.fill_area && m.name == "Object to remove");
        let (lo, hi) = xs(&m.points_at(Time::ZERO));
        assert!((lo - 30.0).abs() < 3.0 && (hi - 60.0).abs() < 3.0, "{lo}..{hi}: {}", e.status);
        assert!(e.tracking.is_some(), "carrying it on: {}", e.status);
        let t0 = std::time::Instant::now();
        while e.tracking.is_some() && t0.elapsed().as_secs() < 120 {
            e.poll_tracking();
            std::thread::sleep(Duration::from_millis(20));
        }
        let m = e.seq().find_clip(id).unwrap().1.masks[i].clone();
        assert!(m.keys.len() > 12, "{} keys: {}", m.keys.len(), e.status);
        // At 0.4 s the box is 24 px further on.
        let (lo, hi) = xs(&m.points_at(Time::from_secs_f64(0.4)));
        assert!((lo - 54.0).abs() < 4.0 && (hi - 84.0).abs() < 4.0, "{lo}..{hi}: {}", e.status);
        e.undo();
        assert_eq!(e.seq().find_clip(id).unwrap().1.masks[i].keys.len(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// One click, no pointing: the box (the subject) is found by itself,
    /// cut out with a soft edge, and followed both ways from the middle of
    /// the clip to cover all of it.
    #[test]
    fn the_background_is_removed_across_the_whole_clip() {
        let dir = std::env::temp_dir().join(format!("rf-engine-bg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("b.mp4");
        let ok = std::process::Command::new("ffmpeg")
            .args(["-y", "-loglevel", "error", "-filter_complex", "color=c=0x3050a0:s=160x90:r=25:d=0.6,noise=alls=20:allf=t[bg];color=c=orange:s=36x40:r=25:d=0.6[fg];[bg][fg]overlay=x='50+t*40':y=25", "-pix_fmt", "yuv420p"])
            .arg(&f)
            .status()
            .is_ok_and(|s| s.success());
        if !ok {
            return;
        }
        let mut e = Editor::new(None, None);
        let info = reelforge_media::probe(&f).unwrap();
        e.project.media.push(MediaAsset { id: MediaId(1), path: f, name: "b.mp4".into(), info, conform: None });
        (e.project.sequences[0].width, e.project.sequences[0].height) = (160, 90);
        let id = ClipId(e.project.alloc_id());
        let c = Clip::new(id, MediaId(1), Time::ZERO, Time::ZERO, Time::from_secs_f64(0.6), None);
        let v = e.seq().tracks.iter().position(|tr| tr.kind == TrackKind::Video).unwrap();
        e.project.sequences[0].tracks[v].clips.push(c);
        e.playback.playhead = Time::from_secs_f64(0.3);
        let i = e.remove_background(id).expect("a mask");
        let xs = |pts: &[[f32; 2]]| (pts.iter().map(|p| p[0]).fold(f32::MAX, f32::min), pts.iter().map(|p| p[0]).fold(f32::MIN, f32::max));
        let m = e.seq().find_clip(id).unwrap().1.masks[i].clone();
        assert!(m.name == "Background removal" && m.refine.is_some() && !m.fill_area && m.mode == reelforge_core::mask::MaskMode::Add);
        // At 0.3 s the box is at 62..98.
        let (lo, hi) = xs(&m.points_at(Time::from_secs_f64(0.3)));
        assert!((lo - 62.0).abs() < 3.5 && (hi - 98.0).abs() < 3.5, "{lo}..{hi}: {}", e.status);
        let t0 = std::time::Instant::now();
        while (e.tracking.is_some() || e.roto_then_back.is_some()) && t0.elapsed().as_secs() < 240 {
            e.poll_tracking();
            std::thread::sleep(Duration::from_millis(20));
        }
        let m = e.seq().find_clip(id).unwrap().1.masks[i].clone();
        let (first, last) = (m.keys.first().unwrap().at, m.keys.last().unwrap().at);
        assert!(first == Time::ZERO && last >= Time::from_secs_f64(0.55), "covers the clip: {first:?}..{last:?} ({} keys): {}", m.keys.len(), e.status);
        // At the start the box is at 50..86, at 0.5 s at 70..106.
        let (lo, hi) = xs(&m.points_at(Time::ZERO));
        assert!((lo - 50.0).abs() < 4.0 && (hi - 86.0).abs() < 4.0, "start {lo}..{hi}");
        let (lo, hi) = xs(&m.points_at(Time::from_secs_f64(0.5)));
        assert!((lo - 70.0).abs() < 4.0 && (hi - 106.0).abs() < 4.0, "0.5 s {lo}..{hi}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The subject finder ignores the background (the cut that reaches round the frame).
    #[test]
    fn the_subject_is_found_off_centre_too() {
        let (w, h) = (192usize, 108usize);
        let mut rgba = vec![0u8; w * h * 4];
        let mut seed = 9u32;
        for y in 0..h {
            for x in 0..w {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                let n = (seed >> 26) as u8;
                let inside = ((x as f32 - 120.0) / 26.0).powi(2) + ((y as f32 - 52.0) / 34.0).powi(2) < 1.0;
                let p = if inside { [230, 200, 60] } else { [40 + n, 90 + n, 140 + n] };
                rgba[(y * w + x) * 4..(y * w + x) * 4 + 4].copy_from_slice(&[p[0], p[1], p[2], 255]);
            }
        }
        let img = Image::from_rgba(&rgba, w, h);
        let m = sam::embed(&rgba, w, h).unwrap();
        let p = find_subject(&img, &m).expect("found");
        assert!(((p[0] - 120.0) / 26.0).powi(2) + ((p[1] - 52.0) / 34.0).powi(2) < 1.0, "on the subject: {p:?}");
    }
}
#[cfg(test)]
mod model_tests {
    use super::*;

    /// A face outlined in dark on a ground of the same mottled grey: a
    /// stroke inside it cuts round the whole face, outline and all, with
    /// the object model, and round its inside without.
    #[test]
    fn the_object_model_cuts_round_the_whole_object() {
        let (w, h) = (320usize, 200usize);
        let mut rgba = vec![0u8; w * h * 4];
        let mut seed = 3u32;
        for y in 0..h {
            for x in 0..w {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                let n = (seed >> 24) as i32 / 6;
                let g = (100 + n + ((x / 9) % 3) as i32 * 25) as u8;
                let d = ((x as f32 - 170.0).powi(2) + (y as f32 - 100.0).powi(2)).sqrt();
                let eye = ((x as f32 - 155.0).powi(2) + (y as f32 - 85.0).powi(2)).sqrt() < 7.0 || ((x as f32 - 185.0).powi(2) + (y as f32 - 85.0).powi(2)).sqrt() < 7.0;
                let p = if (48.0..53.0).contains(&d) || (d < 48.0 && eye) { [30, 30, 30] } else if d < 48.0 { [g, g, g] } else { [g, g, (g as i32 - 10).max(0) as u8] };
                rgba[(y * w + x) * 4..(y * w + x) * 4 + 4].copy_from_slice(&[p[0], p[1], p[2], 255]);
            }
        }
        let img = Image::from_rgba(&rgba, w, h);
        let e = sam::embed(&rgba, w, h).unwrap();
        let st = vec![Stroke { fg: true, radius: 6.0, points: vec![[160.0, 110.0], [180.0, 112.0]] }];
        let xs = |l: &Option<Vec<[f32; 2]>>| l.as_ref().map(|l| (l.iter().map(|p| p[0]).fold(f32::MAX, f32::min), l.iter().map(|p| p[0]).fold(f32::MIN, f32::max))).unwrap();
        let (lo, hi) = xs(&cut_frame(&img, [1.0, 1.0], &st, None, 10, Some(&e)));
        assert!((lo - 117.0).abs() < 3.0 && (hi - 223.0).abs() < 3.0, "{lo}..{hi}");
        let (lo, hi) = xs(&cut_frame(&img, [1.0, 1.0], &st, None, 10, None));
        assert!((lo - 122.0).abs() < 3.0 && (hi - 218.0).abs() < 3.0, "{lo}..{hi}");
        // A stroke off the face (and nothing on it) finds nothing that fits.
        let off = vec![Stroke { fg: false, radius: 6.0, points: vec![[160.0, 110.0], [180.0, 112.0]] }];
        assert!(cut_frame(&img, [1.0, 1.0], &off, None, 10, Some(&e)).is_none());
    }
}
