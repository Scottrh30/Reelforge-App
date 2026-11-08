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
    if let Some((k, e)) = ENCODED.lock().unwrap().as_ref() {
        if *k == key {
            return Some(e.clone());
        }
    }
    let e = Arc::new(sam::embed(rgba, w, h)?);
    *ENCODED.lock().unwrap() = Some((key, e.clone()));
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
    /// undo step. Returns the mask's index.
    pub fn roto_stroke(&mut self, id: ClipId, mask: Option<usize>, stroke: Stroke) -> Option<usize> {
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
                v.masks.push(Mask { name: "Roto brush".into(), shape: MaskShape::Freehand, ..Mask::default() });
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
                            let (pl, pw, ph) = prev.as_ref().unwrap();
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
                        let _ = tx.send(TrackMsg::Progress((i + 1) as f32 / n as f32));
                    }
                    Ok(Some(RotoRun { mask: index, shapes, lost }))
                })();
                let _ = tx.send(TrackMsg::RotoDone(r));
            })
            .expect("roto brush thread");
        self.tracking = Some(TrackJob { seq: self.active, clip: id, rx, cancel, progress: 0.0, mask: None, through: None, camera: false, fill: None, roto: Some(index) });
        self.status = format!("Carrying {} {}…", m.name, if forward { "forwards" } else { "backwards" });
    }

    pub(super) fn finish_roto(&mut self, job: &TrackJob, r: Result<Option<RotoRun>, String>) {
        match r {
            Err(e) => self.status = format!("Roto brush failed: {e}"),
            Ok(None) => self.status = "Roto brush stopped".into(),
            Ok(Some(run)) => {
                let Some(c) = self.project.sequences.get(job.seq).and_then(|s| s.find_clip(job.clip)).map(|(_, c)| c.clone()) else { return };
                let old = c.visual();
                let mut new = old.clone();
                let Some(m) = new.masks.get_mut(run.mask) else { return };
                let (Some(lo), Some(hi)) = (run.shapes.iter().map(|s| s.0).min(), run.shapes.iter().map(|s| s.0).max()) else { return };
                // The run's shapes replace the keys over its span.
                m.keys.retain(|k| k.at < lo || k.at > hi);
                for (at, pts) in &run.shapes {
                    m.keys.push(ShapeKey::new(*at, pts.clone()));
                }
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

    /// A ball moving across a textured background: a stroke on it in the
    /// first frame outlines it, and carrying it forwards keeps the outline on it.
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
        let i = e.roto_stroke(id, None, Stroke { fg: true, radius: 3.0, points: vec![[38.0, 45.0], [52.0, 45.0]] }).expect("a mask");
        let xs = |pts: &[[f32; 2]]| (pts.iter().map(|p| p[0]).fold(f32::MAX, f32::min), pts.iter().map(|p| p[0]).fold(f32::MIN, f32::max));
        let m = e.seq().find_clip(id).unwrap().1.masks[i].clone();
        let (lo, hi) = xs(&m.points_at(Time::ZERO));
        assert!((lo - 30.0).abs() < 3.0 && (hi - 60.0).abs() < 3.0, "{lo}..{hi}: {}", e.status);
        e.start_roto_propagation(id, i, true);
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
