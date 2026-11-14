//! The analysis behind Face beauty (Find faces: faces found in every frame
//! and followed through the clip) and Sky replacement (Find the sky: the sky
//! in every frame picked out by the object model, its edge fitted to the
//! picture, and the camera's movement worked out), run in the background.

use super::{Editor, TrackJob, TrackMsg, LOCKED};
use crossbeam_channel::unbounded;
use reelforge_core::effects::EffectData;
use reelforge_core::faces::{FaceKey, FaceTrack};
use reelforge_core::fillstore::{self, FillFrame, FillIndex};
use reelforge_core::undo::SetClipVisual;
use reelforge_core::*;
use reelforge_sam as sam;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// Frames are looked at this big at most (longer side).
const SIDE: u32 = 640;

/// What an analysis found.
pub enum AiResult {
    Faces(Vec<FaceTrack>),
    /// The sky's folder.
    Sky(PathBuf),
}

impl Editor {
    /// The clip's frames to analyse (source time for keys, media time to
    /// decode), every `every`th one, and its media.
    fn ai_frames(&mut self, id: ClipId, index: usize, kind: EffectKind, every: usize) -> Option<(Vec<(Time, Time)>, MediaAsset)> {
        self.commit_live_edit();
        if self.tracking.is_some() {
            self.status = "Wait for the tracking or analysis already running to finish (or stop it)".into();
            return None;
        }
        if self.seq().is_locked(id) {
            self.status = LOCKED.into();
            return None;
        }
        let (_, c) = self.seq().find_clip(id)?;
        c.effects.get(index).filter(|f| f.kind == kind)?;
        let footage = c.title.is_none() && c.nested.is_none() && c.scene.is_none();
        let Some(media) = self.project.media(c.media).filter(|m| footage && m.info.video.is_some()).cloned() else {
            self.status = format!("{} works on footage or a picture: put it on a video clip", kind.label());
            return None;
        };
        let step = self.seq().frame_rate.frame_duration();
        let mut times = Vec::new();
        let mut t = c.timeline_start;
        let mut i = 0;
        while t < c.timeline_end() {
            if i % every == 0 {
                times.push((c.source_time(t), c.media_at(t)));
            }
            t = t + step;
            i += 1;
        }
        if self.project.is_still(c.media) {
            times.truncate(1);
        }
        self.playback.pause();
        Some((times, media))
    }

    /// Face beauty `index` of clip `id`: find the faces in every frame and
    /// follow them through the clip, in the background.
    pub fn start_find_faces(&mut self, id: ClipId, index: usize) {
        // Every frame up to 30 a second; every other one above.
        let every = if self.seq().frame_rate.fps() > 31.0 { 2 } else { 1 };
        let Some((times, media)) = self.ai_frames(id, index, EffectKind::FaceBeauty, every) else { return };
        let (tx, rx) = unbounded();
        let cancel = Arc::new(AtomicBool::new(false));
        let stop = cancel.clone();
        std::thread::Builder::new()
            .name("find faces".into())
            .spawn(move || {
                let r = (|| -> Result<Option<AiResult>, String> {
                    let mut src = reelforge_media::FrameSource::open(&media.path, Some((SIDE, SIDE))).map_err(|e| format!("{e:#}"))?;
                    let mut found: Vec<Vec<sam::faces::Rect>> = Vec::new();
                    let mut size = [1.0f32, 1.0];
                    for (i, (_, media_t)) in times.iter().enumerate() {
                        if stop.load(Ordering::Relaxed) {
                            return Ok(None);
                        }
                        let f = src.frame_at_blocking(*media_t, Duration::from_secs(20)).ok_or_else(|| format!("no frame at {media_t}"))?;
                        let (w, h) = (f.width as usize, f.height as usize);
                        size = [w as f32, h as f32];
                        let g = sam::faces::gray(&f.pixels.to_rgba(), w, h);
                        found.push(sam::faces::detect(&g, w, h, (h as f32 / 18.0).max(20.0)));
                        let _ = tx.send(TrackMsg::Progress((i + 1) as f32 / times.len() as f32));
                    }
                    // Seen in at least a third of a second's frames (or the only frame).
                    let min = if times.len() == 1 { 1 } else { 6.min(times.len()) };
                    let tracks = sam::faces::track(&found, min, 6);
                    let faces = tracks
                        .into_iter()
                        .map(|t| FaceTrack {
                            on: true,
                            keys: t.iter().enumerate().filter_map(|(i, r)| r.map(|r| FaceKey { at: times[i].0, rect: [(r[0] + r[2] / 2.0) / size[0], (r[1] + r[3] / 2.0) / size[1], r[2] / size[0], r[3] / size[1]] })).collect(),
                        })
                        .collect();
                    Ok(Some(AiResult::Faces(faces)))
                })();
                let _ = tx.send(TrackMsg::AiDone(r));
            })
            .expect("find faces thread");
        self.tracking = Some(TrackJob { seq: self.active, clip: id, rx, cancel, progress: 0.0, mask: None, through: None, camera: false, fill: None, roto: None, ai: Some(index), live: Vec::new(), live_shape: None });
        self.status = "Finding faces…".into();
    }

    /// Sky replacement `index` of clip `id`: find the sky in every other
    /// frame (and how the camera moves), in the background.
    pub fn start_find_sky(&mut self, id: ClipId, index: usize) {
        let Some((times, media)) = self.ai_frames(id, index, EffectKind::SkyReplace, 2) else { return };
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis());
        let dir: PathBuf = reelforge_media::proxy::media_cache_dir().join("fills").join(format!("sky-{}-{index}-{stamp:x}", id.0));
        let owner = self.project_path.as_ref().map(|p| p.display().to_string()).unwrap_or_default();
        let (tx, rx) = unbounded();
        let cancel = Arc::new(AtomicBool::new(false));
        let stop = cancel.clone();
        std::thread::Builder::new()
            .name("find the sky".into())
            .spawn(move || {
                let r = (|| -> Result<Option<AiResult>, String> {
                    std::fs::create_dir_all(&dir).map_err(|e| format!("couldn't make {}: {e}", dir.display()))?;
                    let _ = std::fs::write(dir.join("owner.txt"), &owner);
                    let mut src = reelforge_media::FrameSource::open(&media.path, Some((SIDE, SIDE))).map_err(|e| format!("{e:#}"))?;
                    let mut index = FillIndex::default();
                    let mut prev: Option<(Vec<f32>, Vec<f32>, Vec<f32>)> = None; // luma, matte, low-res logits
                    let mut shift = [0.0f32, 0.0, 1.0];
                    for (i, (at, media_t)) in times.iter().enumerate() {
                        if stop.load(Ordering::Relaxed) {
                            return Ok(None);
                        }
                        let f = src.frame_at_blocking(*media_t, Duration::from_secs(20)).ok_or_else(|| format!("no frame at {media_t}"))?;
                        let (w, h) = (f.width as usize, f.height as usize);
                        index.size = [w as u32, h as u32];
                        let rgba = f.pixels.to_rgba();
                        let luma = reelforge_core::track::luma_of_rgba(&rgba);
                        // How the picture moved since the last frame (the ground, not the sky).
                        if let Some((pl, pm, _)) = &prev {
                            let skyish: Vec<bool> = pm.iter().map(|&m| m > 0.5).collect();
                            if let Some(hm) = reelforge_core::inpaint::motion(pl, &luma, w, h, &skyish) {
                                let c = [w as f32 / 2.0, h as f32 / 2.0];
                                let m = hm.apply(c);
                                let e = hm.apply([c[0] + 100.0, c[1]]);
                                let s = ((e[0] - m[0]).powi(2) + (e[1] - m[1]).powi(2)).sqrt() / 100.0;
                                shift = [shift[0] + (m[0] - c[0]) / w as f32, shift[1] + (m[1] - c[1]) / h as f32, shift[2] * s.clamp(0.8, 1.25)];
                            }
                        }
                        let (matte, low) = sky_matte(&rgba, &luma, w, h, prev.as_ref().map(|p| &p.2[..])).ok_or_else(|| if i == 0 { "no sky found: the top of the frame doesn't look like sky".to_string() } else { "the object model didn't load".into() })?;
                        let bytes: Vec<u8> = matte.iter().flat_map(|&m| [0, 0, 0, (m.clamp(0.0, 1.0) * 255.0).round() as u8]).collect();
                        let file = fillstore::write_frame(&dir, i, [0, 0, w as u32, h as u32], &bytes).map_err(|e| e.to_string())?;
                        index.frames.push(FillFrame { at: at.0, file, rect: [0, 0, w as u32, h as u32], shift: Some(shift) });
                        prev = Some((luma, matte, low));
                        let _ = tx.send(TrackMsg::Progress((i + 1) as f32 / times.len() as f32));
                    }
                    fillstore::write_index(&dir, &index).map_err(|e| e.to_string())?;
                    Ok(Some(AiResult::Sky(dir.clone())))
                })();
                if !matches!(r, Ok(Some(_))) {
                    let _ = std::fs::remove_dir_all(&dir);
                }
                let _ = tx.send(TrackMsg::AiDone(r));
            })
            .expect("find the sky thread");
        self.tracking = Some(TrackJob { seq: self.active, clip: id, rx, cancel, progress: 0.0, mask: None, through: None, camera: false, fill: None, roto: None, ai: Some(index), live: Vec::new(), live_shape: None });
        self.status = if sam::gpu().is_some() { "Finding the sky…" } else { "Finding the sky (on the CPU: a second or two a frame)…" }.into();
    }

    pub(super) fn finish_ai(&mut self, job: &TrackJob, r: Result<Option<AiResult>, String>) {
        let Some(index) = job.ai else { return };
        let Some(c) = self.project.sequences.get(job.seq).and_then(|s| s.find_clip(job.clip)).map(|(_, c)| c.clone()) else { return };
        let old = c.visual();
        let mut new = old.clone();
        let Some(fx) = new.effects.get_mut(index) else { return };
        match r {
            Err(e) => self.status = format!("{} failed: {e}", fx.kind.label()),
            Ok(None) => self.status = "Stopped".into(),
            Ok(Some(AiResult::Faces(faces))) => {
                let n = faces.len();
                fx.data = Some(EffectData::Faces(faces));
                self.execute(Box::new(SetClipVisual { seq: job.seq, id: job.clip, old, new }));
                self.status = match n {
                    0 => "No faces found (faces turned well away, very small or in shadow aren't): try All skin in the picture".into(),
                    1 => "Found 1 face, followed through the clip".into(),
                    n => format!("Found {n} faces, followed through the clip: untick any you don't want touched"),
                };
            }
            Ok(Some(AiResult::Sky(dir))) => {
                let image = match &fx.data {
                    Some(EffectData::Sky { image, .. }) => image.clone(),
                    _ => None,
                };
                fx.data = Some(EffectData::Sky { image, matte: Some(dir.clone()) });
                self.execute(Box::new(SetClipVisual { seq: job.seq, id: job.clip, old, new }));
                self.status = format!("Found the sky in {} frames", fillstore::frame_count(&dir));
            }
        }
    }
}

/// Guided filter (He et al.): `p` smoothed by `r` pixels, keeping the edges
/// in `guide` (both `w` × `h`, 0–1), so a coarse matte follows the
/// picture's edges (branches, roof lines).
fn guided(guide: &[f32], p: &[f32], w: usize, h: usize, r: usize, eps: f32) -> Vec<f32> {
    let boxf = |a: &[f32]| -> Vec<f32> {
        // Summed-area table.
        let mut s = vec![0.0f64; (w + 1) * (h + 1)];
        for y in 0..h {
            let mut row = 0.0f64;
            for x in 0..w {
                row += a[y * w + x] as f64;
                s[(y + 1) * (w + 1) + x + 1] = s[y * (w + 1) + x + 1] + row;
            }
        }
        let mut out = vec![0.0f32; w * h];
        for y in 0..h {
            let (y0, y1) = (y.saturating_sub(r), (y + r + 1).min(h));
            for x in 0..w {
                let (x0, x1) = (x.saturating_sub(r), (x + r + 1).min(w));
                let sum = s[y1 * (w + 1) + x1] - s[y0 * (w + 1) + x1] - s[y1 * (w + 1) + x0] + s[y0 * (w + 1) + x0];
                out[y * w + x] = (sum / ((y1 - y0) * (x1 - x0)) as f64) as f32;
            }
        }
        out
    };
    let mi = boxf(guide);
    let mp = boxf(p);
    let ip: Vec<f32> = guide.iter().zip(p).map(|(a, b)| a * b).collect();
    let ii: Vec<f32> = guide.iter().map(|a| a * a).collect();
    let (mip, mii) = (boxf(&ip), boxf(&ii));
    let a: Vec<f32> = (0..w * h).map(|i| (mip[i] - mi[i] * mp[i]) / (mii[i] - mi[i] * mi[i] + eps)).collect();
    let b: Vec<f32> = (0..w * h).map(|i| mp[i] - a[i] * mi[i]).collect();
    let (ma, mb) = (boxf(&a), boxf(&b));
    (0..w * h).map(|i| (ma[i] * guide[i] + mb[i]).clamp(0.0, 1.0)).collect()
}

/// Does this colour look like sky (bright, and blue, grey or white)?
fn skyish(p: &[u8]) -> bool {
    let (r, g, b) = (p[0] as f32, p[1] as f32, p[2] as f32);
    let y = 0.299 * r + 0.587 * g + 0.114 * b;
    y > 90.0 && b >= r - 12.0 && b >= g - 20.0
}

/// The sky in a frame (`rgba`, `w` × `h`): its matte (0–1 a pixel) and the
/// model's low-resolution logits (to prompt the next frame with). The
/// model is pointed at what looks like sky along the top of the frame (and
/// away from the bottom), or given the last frame's sky.
fn sky_matte(rgba: &[u8], luma: &[f32], w: usize, h: usize, prev: Option<&[f32]>) -> Option<(Vec<f32>, Vec<f32>)> {
    // Places along the top that look like sky (smooth, and sky-coloured).
    let mut points: Vec<([f32; 2], bool)> = Vec::new();
    for row in [0.04f32, 0.12, 0.2] {
        for k in 0..9 {
            let (x, y) = (((k as f32 + 0.5) / 9.0 * w as f32) as usize, (row * h as f32) as usize);
            let o = (y * w + x) * 4;
            let flat = {
                let at = |dx: isize, dy: isize| luma[(y as isize + dy).clamp(0, h as isize - 1) as usize * w + (x as isize + dx).clamp(0, w as isize - 1) as usize];
                (at(3, 0) - at(-3, 0)).abs() + (at(0, 3) - at(0, -3)).abs() < 20.0
            };
            if skyish(&rgba[o..o + 4]) && flat {
                points.push(([x as f32, y as f32], true));
            }
        }
    }
    if points.is_empty() && prev.is_none() {
        return None;
    }
    // Keep the prompt short: the model works best with a handful of points.
    if points.len() > 8 {
        let step = points.len() as f32 / 8.0;
        points = (0..8).map(|i| points[(i as f32 * step) as usize]).collect();
    }
    for k in 0..3 {
        points.push(([(k as f32 + 0.5) / 3.0 * w as f32, h as f32 * 0.97], false));
    }
    let e = sam::embed(rgba, w, h)?;
    let cut = sam::cut(&e, &sam::Prompt { points: &points, bbox: None, mask: prev })?;
    let soft: Vec<f32> = cut.logits.iter().map(|&l| 1.0 / (1.0 + (-l * 2.0).exp())).collect();
    // The edge fitted to the picture: branches, wires and roof lines.
    let guide: Vec<f32> = luma.iter().map(|v| v / 255.0).collect();
    let matte = guided(&guide, &soft, w, h, (w.max(h) / 160).max(3), 1e-3);
    Some((matte, cut.low))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The guided filter keeps a sharp edge in the guide sharp in the matte.
    #[test]
    fn guided_filter_follows_edges() {
        let (w, h) = (40usize, 10usize);
        let guide: Vec<f32> = (0..w * h).map(|i| if i % w < 20 { 0.9 } else { 0.1 }).collect();
        // A coarse matte whose edge is 4 px off.
        let p: Vec<f32> = (0..w * h).map(|i| if i % w < 24 { 1.0 } else { 0.0 }).collect();
        let m = guided(&guide, &p, w, h, 6, 1e-3);
        // The matte steps down where the guide does, not 4 px off where the coarse one did.
        let step = m[5 * w + 19] - m[5 * w + 20];
        let later = m[5 * w + 23] - m[5 * w + 24];
        assert!(m[5 * w + 16] > 0.9 && step > 0.25 && step > later * 2.0, "{:?}", &m[5 * w + 14..5 * w + 26]);
    }

    #[test]
    fn sky_colours() {
        assert!(skyish(&[130, 170, 230, 255]) && skyish(&[220, 220, 225, 255]));
        assert!(!skyish(&[90, 120, 40, 255]) && !skyish(&[40, 40, 60, 255]));
    }
}

#[cfg(test)]
mod job_tests {
    use super::*;
    use reelforge_core::project::MediaAsset;

    fn clip_of(e: &mut Editor, f: &std::path::Path, secs: f64, kind: EffectKind) -> ClipId {
        let info = reelforge_media::probe(f).unwrap();
        let (w, h) = info.video.as_ref().map(|v| (v.width, v.height)).unwrap();
        e.project.media.push(MediaAsset { id: MediaId(1), path: f.to_path_buf(), name: "a.mp4".into(), info, conform: None });
        (e.project.sequences[0].width, e.project.sequences[0].height) = (w, h);
        let id = ClipId(e.project.alloc_id());
        let mut c = Clip::new(id, MediaId(1), Time::ZERO, Time::ZERO, Time::from_secs_f64(secs), None);
        c.effects.push(Effect::new(kind));
        let v = e.seq().tracks.iter().position(|tr| tr.kind == TrackKind::Video).unwrap();
        e.project.sequences[0].tracks[v].clips.push(c);
        id
    }

    fn wait(e: &mut Editor) {
        let t0 = std::time::Instant::now();
        while e.tracking.is_some() && t0.elapsed().as_secs() < 300 {
            e.poll_tracking();
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// A blue sky over noisy ground: the sky is found in each frame (the top
    /// all sky, the ground none), and the effect then draws.
    #[test]
    fn the_sky_is_found() {
        let dir = std::env::temp_dir().join(format!("rf-engine-sky-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a.mp4");
        let ok = std::process::Command::new("ffmpeg")
            .args(["-y", "-loglevel", "error", "-filter_complex", "color=c=0x8cbcec:s=160x90:r=10:d=0.4[sky];color=c=0x4a5a2a:s=160x40:r=10:d=0.4,noise=alls=60:allf=t[ground];[sky][ground]overlay=x=0:y=50", "-pix_fmt", "yuv420p"])
            .arg(&f)
            .status()
            .is_ok_and(|s| s.success());
        if !ok {
            return;
        }
        let mut e = Editor::new(None, None);
        let id = clip_of(&mut e, &f, 0.4, EffectKind::SkyReplace);
        e.start_find_sky(id, 0);
        wait(&mut e);
        let fx = e.seq().find_clip(id).unwrap().1.effects[0].clone();
        let Some(EffectData::Sky { matte: Some(m), .. }) = &fx.data else { panic!("{}", e.status) };
        let frames = (0.4 * e.seq().frame_rate.fps()).round() as usize;
        assert_eq!(fillstore::frame_count(m), frames.div_ceil(2), "every other frame: {}", e.status);
        let (t, _, _) = fillstore::load_moved(m, Time::ZERO).unwrap();
        let a = |x: usize, y: usize| t.rgba[(y * t.width as usize + x) * 4 + 3];
        assert!(a(80, 10) > 200 && a(80, 75) < 50, "{} {}", a(80, 10), a(80, 75));
        assert!(!fx.eval(Time::ZERO).is_identity());
        let _ = std::fs::remove_dir_all(m);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// No faces in a test pattern: found none, said so.
    #[test]
    fn no_faces_in_a_test_pattern() {
        let dir = std::env::temp_dir().join(format!("rf-engine-faces-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a.mp4");
        let ok = std::process::Command::new("ffmpeg").args(["-y", "-loglevel", "error", "-f", "lavfi", "-i", "testsrc2=s=160x90:r=10:d=0.3", "-pix_fmt", "yuv420p"]).arg(&f).status().is_ok_and(|s| s.success());
        if !ok {
            return;
        }
        let mut e = Editor::new(None, None);
        let id = clip_of(&mut e, &f, 0.3, EffectKind::FaceBeauty);
        e.start_find_faces(id, 0);
        wait(&mut e);
        let fx = e.seq().find_clip(id).unwrap().1.effects[0].clone();
        assert!(matches!(&fx.data, Some(EffectData::Faces(v)) if v.is_empty()), "{}", e.status);
        assert!(e.status.starts_with("No faces"), "{}", e.status);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
