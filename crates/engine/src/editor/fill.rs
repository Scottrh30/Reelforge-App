//! Object removal in the editor: work out a clip's filled frames in the
//! background (see `reelforge_core::inpaint`) and keep them on disk for its
//! Object removal effect (see `reelforge_core::fillstore`).

use super::{Editor, TrackJob, TrackMsg, LOCKED};
use crossbeam_channel::unbounded;
use reelforge_core::effects::{removal as rp, EffectData};
use reelforge_core::fillstore::{self, FillFrame, FillIndex};
use reelforge_core::inpaint::{self, Filler, FrameIn, Image, Method};
use reelforge_core::transform::Homography;
use reelforge_core::undo::SetClipVisual;
use reelforge_core::*;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

/// The longest side motion is worked out at.
const MOTION_SIDE: usize = 960;

/// Scale a pixel map made at `k` × the size up to full size.
fn scaled(h: &Homography, k: f64) -> Homography {
    let s = Homography([k, 0.0, 0.0, 0.0, k, 0.0, 0.0, 0.0, 1.0]);
    let si = Homography([1.0 / k, 0.0, 0.0, 0.0, 1.0 / k, 0.0, 0.0, 0.0, 1.0]);
    si.then_after(&h.then_after(&s))
}

/// Move a pixel map into a crop's pixels (the crop's corner at `o`).
fn cropped(h: &Homography, o: [f64; 2]) -> Homography {
    let to = Homography([1.0, 0.0, o[0], 0.0, 1.0, o[1], 0.0, 0.0, 1.0]);
    let back = Homography([1.0, 0.0, -o[0], 0.0, 1.0, -o[1], 0.0, 0.0, 1.0]);
    back.then_after(&h.then_after(&to))
}

/// A box blur of `a` (`w`×`h`) by `r` pixels, two passes.
fn blur(a: &[f32], w: usize, h: usize, r: usize) -> Vec<f32> {
    if r == 0 {
        return a.to_vec();
    }
    let n = (2 * r + 1) as f32;
    let mut b = vec![0.0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut s = 0.0;
            for k in x as isize - r as isize..=x as isize + r as isize {
                s += a[y * w + k.clamp(0, w as isize - 1) as usize];
            }
            b[y * w + x] = s / n;
        }
    }
    let mut c = vec![0.0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut s = 0.0;
            for k in y as isize - r as isize..=y as isize + r as isize {
                s += b[k.clamp(0, h as isize - 1) as usize * w + x];
            }
            c[y * w + x] = s / n;
        }
    }
    c
}

impl Editor {
    /// Work out object removal `index` of clip `id` in the background: every
    /// frame of the clip has what its "object to remove" masks cover filled.
    pub fn start_object_removal(&mut self, id: ClipId, index: usize) {
        self.commit_live_edit();
        if self.tracking.is_some() {
            self.status = "Wait for the tracking or analysis already running to finish (or stop it)".into();
            return;
        }
        let Some((_, c)) = self.seq().find_clip(id) else { return };
        if self.seq().is_locked(id) {
            self.status = LOCKED.into();
            return;
        }
        let Some(fx) = c.effects.get(index).filter(|f| f.kind == EffectKind::ObjectRemoval) else { return };
        let footage = c.title.is_none() && c.nested.is_none() && c.scene.is_none() && self.project.media(c.media).is_some_and(|m| m.info.video.is_some());
        if !footage {
            self.status = "Object removal works on footage or a picture: put it on a video clip".into();
            return;
        }
        let masks: Vec<mask::Mask> = c.masks.iter().filter(|m| m.enabled && m.fill_area).cloned().collect();
        if masks.is_empty() {
            self.status = "Draw a mask round what to remove, and set its Use to \"Object to remove\" (Masks panel); animate or track it to follow the object".into();
            return;
        }
        let Some(media) = self.project.media(c.media).cloned() else { return };
        let Some(v) = media.info.video.as_ref() else { return };
        let media_size = [v.width as usize, v.height as usize];
        let layer = self.layer_size(c);
        // Each frame of the clip: the time its effects and masks see, and the media frame shown.
        let step = self.seq().frame_rate.frame_duration();
        let mut times = Vec::new();
        let mut t = c.timeline_start;
        while t < c.timeline_end() {
            times.push((c.source_time(t), c.media_at(t)));
            t = t + step;
        }
        if self.project.is_still(c.media) {
            times.truncate(1);
        }
        let method = match fx.value_at(rp::METHOD, Time::ZERO).round() as i32 {
            1 => Method::Surface,
            2 => Method::EdgeBlend,
            _ => Method::Object,
        };
        let reach = fx.value_at(rp::REACH, Time::ZERO).round().clamp(0.0, 60.0) as usize;
        let grow = fx.value_at(rp::GROW, Time::ZERO).round().max(0.0) as usize;
        let feather = fx.value_at(rp::FEATHER, Time::ZERO).round().max(0.0) as usize;
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis());
        let dir: PathBuf = reelforge_media::proxy::media_cache_dir().join("fills").join(format!("{}-{index}-{stamp:x}", id.0));
        let (tx, rx) = unbounded();
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop = cancel.clone();
        self.playback.pause();
        std::thread::Builder::new()
            .name("object removal".into())
            .spawn(move || {
                let ptx = tx.clone();
                let r = (|| -> Result<Option<PathBuf>, String> {
                    std::fs::create_dir_all(&dir).map_err(|e| format!("couldn't make {}: {e}", dir.display()))?;
                    let (w, h) = (media_size[0], media_size[1]);
                    // Media pixels → layer pixels, for the masks.
                    let to_layer = [layer[0] / w as f32, layer[1] / h as f32];
                    let hole_at = |src: Time| -> Vec<bool> {
                        let img = mask::fill_area_image(&masks, src, layer, w.max(h) as u32);
                        let mut out = vec![false; w * h];
                        if let Some(img) = img {
                            for y in 0..h {
                                for x in 0..w {
                                    out[y * w + x] = img.at([(x as f32 + 0.5) * to_layer[0], (y as f32 + 0.5) * to_layer[1]], layer) >= 128;
                                }
                            }
                        }
                        inpaint::dilate(&out, w, h, grow)
                    };
                    // The part of the frame worth keeping: round every frame's hole,
                    // with room to find what's behind it and to make it up.
                    let mut union: Option<[usize; 4]> = None;
                    for (src, _) in &times {
                        let small = mask::fill_area_image(&masks, *src, layer, 320);
                        if let Some(img) = small {
                            let (iw, ih) = (img.width as usize, img.height as usize);
                            let hb: Vec<bool> = img.data.iter().map(|v| *v >= 128).collect();
                            if let Some(b) = inpaint::bounds(&hb, iw, ih) {
                                let b = [b[0] * w / iw, b[1] * h / ih, (b[2] * w).div_ceil(iw), (b[3] * h).div_ceil(ih)];
                                union = Some(union.map_or(b, |u| [u[0].min(b[0]), u[1].min(b[1]), u[2].max(b[2]), u[3].max(b[3])]));
                            }
                        }
                    }
                    let Some(u) = union else { return Err("the masks to remove don't cover anything in this clip".into()) };
                    let margin = (w.max(h) / 4).max((u[2] - u[0]).max(u[3] - u[1])) + grow + feather + 8;
                    let crop = [u[0].saturating_sub(margin), u[1].saturating_sub(margin), (u[2] + margin).min(w), (u[3] + margin).min(h)];
                    let (cw, ch) = (crop[2] - crop[0], crop[3] - crop[1]);
                    // At most about 1.5 GB of frames kept.
                    let per = cw * ch * 16;
                    let reach = reach.min((1_500_000_000 / per.max(1)).saturating_sub(1) / 2).max(if method == Method::Object { 1 } else { 0 });
                    let mut src = reelforge_media::FrameSource::open(&media.path, None).map_err(|e| format!("{e:#}"))?;
                    let wait = Duration::from_secs(20);
                    let k = (MOTION_SIDE as f64 / w.max(h) as f64).min(1.0);
                    let (mw, mh) = (((w as f64 * k).round() as usize).max(1), ((h as f64 * k).round() as usize).max(1));
                    let small_luma = |rgba: &[u8]| -> Vec<f32> {
                        let full = reelforge_core::track::luma_of_rgba(rgba);
                        if k >= 1.0 {
                            return full;
                        }
                        (0..mh).flat_map(|y| (0..mw).map(move |x| (y, x))).map(|(y, x)| full[((y as f64 / k) as usize).min(h - 1) * w + ((x as f64 / k) as usize).min(w - 1)]).collect()
                    };
                    let small_hole = |hole: &[bool]| -> Vec<bool> {
                        (0..mh).flat_map(|y| (0..mw).map(move |x| (y, x))).map(|(y, x)| hole[((y as f64 / k) as usize).min(h - 1) * w + ((x as f64 / k) as usize).min(w - 1)]).collect()
                    };
                    let mut filler = Filler::new(method, reach, 0x0b1e_c7 ^ times.len() as u64);
                    let mut prev: Option<(Vec<f32>, Vec<bool>)> = None;
                    let mut index = FillIndex { size: [w as u32, h as u32], frames: Vec::new() };
                    let mut holes: Vec<Vec<bool>> = Vec::new();
                    let n = times.len();
                    let mut next_fill = 0usize;
                    let fill_one = |filler: &mut Filler, i: usize, hole: &[bool], index: &mut FillIndex| -> Result<(), String> {
                        let img = filler.fill(i);
                        // How much of the patch covers the picture: all of the hole,
                        // fading over `feather` beyond it.
                        let soft = if feather > 0 {
                            let wide: Vec<f32> = inpaint::dilate(hole, cw, ch, feather / 2).iter().map(|&b| if b { 1.0 } else { 0.0 }).collect();
                            blur(&wide, cw, ch, feather.div_ceil(2))
                        } else {
                            hole.iter().map(|&b| if b { 1.0 } else { 0.0 }).collect()
                        };
                        let cover: Vec<bool> = soft.iter().map(|&a| a > 0.0).collect();
                        let Some(b) = inpaint::bounds(&cover, cw, ch) else { return Ok(()) };
                        let (bw, bh) = (b[2] - b[0], b[3] - b[1]);
                        let mut rgba = Vec::with_capacity(bw * bh * 4);
                        for y in b[1]..b[3] {
                            for x in b[0]..b[2] {
                                let p = img.px[y * cw + x];
                                rgba.extend_from_slice(&[p[0].round().clamp(0.0, 255.0) as u8, p[1].round().clamp(0.0, 255.0) as u8, p[2].round().clamp(0.0, 255.0) as u8, (soft[y * cw + x].clamp(0.0, 1.0) * 255.0).round() as u8]);
                            }
                        }
                        let rect = [(b[0] + crop[0]) as u32, (b[1] + crop[1]) as u32, bw as u32, bh as u32];
                        let file = fillstore::write_frame(&dir, i, rect, &rgba).map_err(|e| e.to_string())?;
                        index.frames.push(FillFrame { at: times[i].0 .0, file, rect });
                        Ok(())
                    };
                    for (i, (src_t, media_t)) in times.iter().enumerate() {
                        if stop.load(Ordering::Relaxed) {
                            return Ok(None);
                        }
                        let f = src.frame_at_blocking(*media_t, wait).ok_or_else(|| format!("no frame at {media_t}"))?;
                        let rgba = f.pixels.to_rgba();
                        let (fw, fh) = (f.width as usize, f.height as usize);
                        // (Frames decode at the media's size; if not, they're stretched to it.)
                        let full: Vec<u8> = if (fw, fh) == (w, h) {
                            rgba.to_vec()
                        } else {
                            (0..h).flat_map(|y| (0..w).map(move |x| (y, x))).flat_map(|(y, x)| {
                                let o = ((y * fh / h) * fw + x * fw / w) * 4;
                                [rgba[o], rgba[o + 1], rgba[o + 2], rgba[o + 3]]
                            }).collect()
                        };
                        let hole = hole_at(*src_t);
                        let luma = small_luma(&full);
                        let sh = small_hole(&hole);
                        let from_prev = match &prev {
                            Some((pl, ph)) if method != Method::EdgeBlend => inpaint::motion(pl, &luma, mw, mh, ph).map(|m| cropped(&scaled(&m, 1.0 / k), [crop[0] as f64, crop[1] as f64])),
                            _ => None,
                        };
                        prev = Some((luma, sh));
                        let mut img = Image { w: cw, h: ch, px: Vec::with_capacity(cw * ch) };
                        let mut ch_hole = Vec::with_capacity(cw * ch);
                        for y in crop[1]..crop[3] {
                            for x in crop[0]..crop[2] {
                                let o = (y * w + x) * 4;
                                img.px.push([full[o] as f32, full[o + 1] as f32, full[o + 2] as f32]);
                                ch_hole.push(hole[y * w + x]);
                            }
                        }
                        // The area filled reaches the feather's soft edge too.
                        let fill_hole = inpaint::dilate(&ch_hole, cw, ch, feather);
                        holes.push(ch_hole);
                        // Where the motion can't be found, the camera's taken to be still.
                        filler.push(FrameIn { img, hole: fill_hole, from_prev: from_prev.or(if i > 0 { Some(Homography::IDENTITY) } else { None }) });
                        // Fill each frame once the frames after it it may need are in.
                        while next_fill < n && (next_fill + reach <= i) {
                            fill_one(&mut filler, next_fill, &holes[next_fill], &mut index)?;
                            holes[next_fill] = Vec::new();
                            next_fill += 1;
                            let _ = ptx.send(TrackMsg::Progress(next_fill as f32 / n as f32));
                        }
                    }
                    while next_fill < n {
                        if stop.load(Ordering::Relaxed) {
                            return Ok(None);
                        }
                        fill_one(&mut filler, next_fill, &holes[next_fill], &mut index)?;
                        holes[next_fill] = Vec::new();
                        next_fill += 1;
                        let _ = ptx.send(TrackMsg::Progress(next_fill as f32 / n as f32));
                    }
                    fillstore::write_index(&dir, &index).map_err(|e| e.to_string())?;
                    Ok(Some(dir.clone()))
                })();
                if !matches!(r, Ok(Some(_))) {
                    let _ = std::fs::remove_dir_all(&dir);
                }
                let _ = tx.send(TrackMsg::FillDone(r));
            })
            .expect("object removal thread");
        self.tracking = Some(TrackJob { seq: self.active, clip: id, rx, cancel, progress: 0.0, mask: None, through: None, camera: false, fill: Some(index), roto: None });
        self.status = "Removing the object: filling every frame…".into();
    }

    pub(super) fn finish_object_removal(&mut self, job: &TrackJob, r: Result<Option<PathBuf>, String>) {
        let Some(index) = job.fill else { return };
        match r {
            Err(e) => self.status = format!("Object removal failed: {e}"),
            Ok(None) => self.status = "Object removal stopped".into(),
            Ok(Some(dir)) => {
                let Some(c) = self.project.sequences.get(job.seq).and_then(|s| s.find_clip(job.clip)).map(|(_, c)| c.clone()) else { return };
                let old = c.visual();
                let mut new = old.clone();
                let Some(fx) = new.effects.get_mut(index).filter(|f| f.kind == EffectKind::ObjectRemoval) else { return };
                // The fill it replaces goes (unless an undo step still points at it, kept till then).
                fx.data = Some(EffectData::Fill(dir.clone()));
                self.execute(Box::new(SetClipVisual { seq: job.seq, id: job.clip, old, new }));
                self.status = format!("Object removed: {} frames filled", fillstore::frame_count(&dir));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reelforge_core::project::MediaAsset;

    /// A one-second clip with a red square stuck over the moving test picture:
    /// removed, every frame gets a patch with no red left in it, and the
    /// effect points at it (one undo step).
    #[test]
    fn a_masked_object_is_filled_in_every_frame() {
        let dir = std::env::temp_dir().join(format!("rf-engine-fill-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("a.mp4");
        let ok = std::process::Command::new("ffmpeg")
            .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i", "testsrc2=size=160x90:rate=25:duration=1,drawbox=x=70:y=35:w=20:h=20:color=red:t=fill", "-pix_fmt", "yuv420p"])
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
        let mut c = Clip::new(id, MediaId(1), Time::ZERO, Time::ZERO, Time::from_secs_f64(1.0), None);
        c.masks.push(mask::Mask { points: vec![[68.0, 33.0], [92.0, 33.0], [92.0, 57.0], [68.0, 57.0]], fill_area: true, ..Default::default() });
        c.effects.push(Effect::new(EffectKind::ObjectRemoval));
        let v = e.seq().tracks.iter().position(|tr| tr.kind == TrackKind::Video).unwrap();
        e.project.sequences[0].tracks[v].clips.push(c);
        e.start_object_removal(id, 0);
        let t0 = std::time::Instant::now();
        while e.tracking.is_some() && t0.elapsed().as_secs() < 120 {
            e.poll_tracking();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(e.status.starts_with("Object removed"), "{}", e.status);
        let (_, c) = e.seq().find_clip(id).unwrap();
        let Some(EffectData::Fill(fill)) = c.effects[0].data.clone() else { panic!("no fill") };
        // A frame for each of the sequence's frames over the clip.
        assert_eq!(fillstore::frame_count(&fill), e.seq().frame_rate.fps().round() as usize);
        for s in [0.0, 0.5, 0.96] {
            let (t, place) = fillstore::load(&fill, Time::from_secs_f64(s)).unwrap();
            assert!(place[0] < 70.0 / 160.0 && place[0] + place[2] > 90.0 / 160.0, "{place:?}");
            // Where the square was (the test picture has red of its own elsewhere, which may be copied a little).
            let (x0, y0) = ((place[0] * 160.0).round() as usize, (place[1] * 90.0).round() as usize);
            let red = (35..55)
                .flat_map(|y| (70..90).map(move |x| (x, y)))
                .filter(|&(x, y)| {
                    let i = ((y - y0) * t.width as usize + x - x0) * 4;
                    let p = &t.rgba[i..i + 4];
                    p[3] > 200 && p[0] > 180 && p[1] < 80 && p[2] < 80
                })
                .count();
            assert!(red < 20, "red left at {s}: {red} of 400");
        }
        // The effect draws it.
        assert!(!c.effects[0].eval(Time::from_secs_f64(0.5)).is_identity());
        e.undo();
        assert!(e.seq().find_clip(id).unwrap().1.effects[0].data.is_none());
        let _ = std::fs::remove_dir_all(&fill);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

