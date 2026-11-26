//! One-click fixes worked out in the background: stabilising a shaky shot with
//! no track to set up (`reelforge_core::autostab`), and deflickering
//! (`reelforge_core::deflicker`).

use super::{Editor, TrackJob, TrackMsg, LOCKED};
use crossbeam_channel::unbounded;
use reelforge_core::anim::{Curve, Interp, Key};
use reelforge_core::camtrack::{StabPath, Track2d};
use reelforge_core::effects::{deflicker as df, Effect, EffectKind};
use reelforge_core::track::StabZoom;
use reelforge_core::transform::Homography;
use reelforge_core::undo::SetClipVisual;
use reelforge_core::*;
use std::sync::Arc;

/// An automatic stabilise worked out: each frame's map (frame pixels), at
/// these media times.
pub(super) struct AutoStab {
    maps: Vec<Homography>,
    times: Vec<Time>,
    path: StabPath,
    zoom: StabZoom,
}

/// A deflicker worked out: each frame's gains (brightness, red, green, blue %)
/// at these media times.
pub(super) struct Deflickered {
    gains: Vec<[f32; 4]>,
    times: Vec<Time>,
    smoothing: f32,
    colour: bool,
}

impl Editor {
    /// Is clip `id` moving footage a background job can work on? (The clip,
    /// or why not, in the status.)
    fn footage_for_job(&mut self, id: ClipId, what: &str) -> Option<Clip> {
        self.commit_live_edit();
        if self.tracking.is_some() {
            self.status = "Wait for the job running to finish (or stop it) first".into();
            return None;
        }
        let c = self.seq().find_clip(id).map(|(_, c)| c.clone())?;
        if self.seq().is_locked(id) {
            self.status = LOCKED.into();
            return None;
        }
        let footage = c.title.is_none() && c.nested.is_none() && c.scene.is_none() && self.project.media(c.media).is_some_and(|m| m.info.video.is_some()) && !self.project.is_still(c.media);
        if !footage {
            self.status = format!("{what} works on moving footage: select a video clip");
            return None;
        }
        Some(c)
    }

    /// The media times clip `c` shows (earliest, latest).
    fn media_span(&self, c: &Clip) -> (Time, Time) {
        let frame = self.seq().frame_rate.frame_duration();
        let (a, b) = (c.media_at(c.timeline_start), c.media_at(c.timeline_end() - frame));
        (a.min(b), a.max(b))
    }

    fn start_job(&mut self, id: ClipId, rx: crossbeam_channel::Receiver<TrackMsg>, cancel: Arc<std::sync::atomic::AtomicBool>) {
        self.tracking = Some(TrackJob { seq: self.active, clip: id, rx, cancel, progress: 0.0, mask: None, through: None, camera: false, fill: None, roto: None, ai: None, live: Vec::new(), live_shape: None });
    }

    /// Stabilise clip `id` with nothing to set up: follow the details through
    /// the shot in the background, find the camera's shake from them, and take
    /// it out (keeping a smoothed move, or holding still), zooming by `zoom` to
    /// hide the edges: camera moves slower than `smoothing` frames are kept,
    /// or (0) the camera is held still where it is at the playhead. One undo
    /// step, when it's done.
    pub fn start_auto_stabilise(&mut self, id: ClipId, smoothing: f32, zoom: StabZoom) {
        let Some(c) = self.footage_for_job(id, "Stabilising") else { return };
        let Some(media) = self.project.media(c.media).cloned() else { return };
        let (from, until) = self.media_span(&c);
        let path = if smoothing.is_finite() && smoothing > 0.0 {
            StabPath::Smooth(smoothing.min(1000.0))
        } else {
            // (The playhead's frame among those followed, from `from`.)
            let step = media.info.video.as_ref().map_or(1, |v| v.frame_rate.frame_duration().0.max(1));
            let t = self.playback.playhead.max(c.timeline_start).min(c.timeline_end() - self.seq().frame_rate.frame_duration());
            let k = (c.media_at(t) - from).0.max(0) / step;
            StabPath::Lock(usize::try_from(k).unwrap_or(0))
        };
        let mid = c.timeline_start + Time((c.duration.0 / 2).max(0));
        let map = self.layer_frame_map(&c, mid);
        let layer = self.layer_size(&c);
        let px = self.frame_px();
        let (tx, rx) = unbounded();
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop = cancel.clone();
        self.playback.pause();
        let spawned = std::thread::Builder::new()
            .name("stabiliser".into())
            .spawn(move || {
                let ptx = tx.clone();
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<Option<AutoStab>, String> {
                    let followed = reelforge_media::track_scene(&media.path, &media.info, from, until, &stop, |p| {
                        let _ = ptx.send(TrackMsg::Progress(p * 0.95));
                    })
                    .map_err(|e| format!("{e:#}"))?;
                    let Some((tracks, times)) = followed else { return Ok(None) };
                    if tracks.len() < 4 {
                        return Err("too few details to follow in this shot (is it very dark, blurred or plain?)".into());
                    }
                    let to_frame = |p: [f32; 2]| map.apply([p[0] - layer[0] / 2.0, p[1] - layer[1] / 2.0]);
                    let tracks: Vec<Track2d> = tracks.into_iter().map(|t| Track2d { start: t.start, pos: t.pos.into_iter().map(to_frame).collect() }).collect();
                    let cam = reelforge_core::autostab::camera_path(&tracks, times.len());
                    let centre = [px[0] as f64 / 2.0, px[1] as f64 / 2.0];
                    let maps = reelforge_core::autostab::stabilising_maps(&cam, path, centre);
                    Ok(Some(AutoStab { maps, times, path, zoom }))
                }))
                .unwrap_or_else(|_| Err("the stabiliser hit a problem with this shot".into()));
                let _ = tx.send(TrackMsg::StabDone(r));
            });
        if let Err(e) = spawned {
            self.status = format!("Couldn't start stabilising: {e}");
            return;
        }
        self.start_job(id, rx, cancel);
        self.status = "Stabilising: following the details through the shot…".into();
    }

    pub(super) fn finish_auto_stabilise(&mut self, job: &TrackJob, r: Result<Option<AutoStab>, String>) {
        match r {
            Err(e) => self.status = format!("Stabilising failed: {e}"),
            Ok(None) => self.status = "Stabilising stopped (nothing was changed)".into(),
            Ok(Some(s)) => {
                if job.seq != self.active {
                    self.status = "Stabilising finished in another sequence: open it and stabilise again".into();
                    return;
                }
                match self.pin_stabilising_maps(job.clip, &s.maps, &s.times, s.path, s.zoom) {
                    Err(e) => self.status = e.into(),
                    Ok(()) => {
                        self.status = match s.path {
                            StabPath::Lock(_) => "Stabilised, held still: the shot looks locked off (corner-pinned each frame; undo to take it off)".into(),
                            StabPath::Smooth(_) => "Stabilised: the shake is gone, the move stays (corner-pinned each frame; undo to take it off)".into(),
                        }
                    }
                }
            }
        }
    }

    /// Corner-pin clip `id` each frame by `maps` (frame pixels; frame `k` at
    /// media time `media_times[k]`), replacing its corner-pin keys over that
    /// span, then zoom (`zoom`) to hide the uncovered edges. One undo step.
    pub(super) fn pin_stabilising_maps(&mut self, id: ClipId, maps: &[Homography], media_times: &[Time], path: StabPath, zoom: StabZoom) -> Result<(), &'static str> {
        let Some((_, c)) = self.seq().find_clip(id) else { return Err("The clip is gone") };
        if self.seq().is_locked(id) {
            return Err(LOCKED);
        }
        let (layer, frame) = (self.layer_size(c), self.frame_px());
        let mut anim = c.anim.clone();
        let mut keys = Vec::new();
        let mut times = Vec::new();
        for (h, &mt) in maps.iter().zip(media_times) {
            let Some(t) = c.timeline_of_media(mt) else { continue };
            if t < c.timeline_start || t >= c.timeline_end() {
                continue;
            }
            let base = c.transform_at(t);
            let still = base.valid_pin().unwrap_or_else(|| base.pin_of_transform(layer, frame));
            let pin = still.map(|p| h.apply(p));
            if pin.is_valid() {
                keys.push((c.source_time(t), pin));
                times.push(t);
            }
        }
        let (Some(a), Some(b)) = (keys.iter().map(|k| k.0).min(), keys.iter().map(|k| k.0).max()) else {
            return Err("Nothing to stabilise: no frames of the clip were worked out");
        };
        anim.pin.0.retain(|k| k.at < a || k.at > b);
        for (at, pin) in keys {
            anim.pin.set(at, pin);
        }
        if zoom != StabZoom::Off {
            let smoothing = match path {
                StabPath::Smooth(s) => s,
                StabPath::Lock(_) => 0.0,
            };
            let opts = reelforge_core::track::ApplyOptions { stabilise: true, smoothing, axes: reelforge_core::track::Axes::ALL, zoom };
            reelforge_core::track::zoom_to_cover(&mut anim, c, layer, frame, &times, true, opts);
        }
        let old = c.visual();
        let new = ClipVisual { anim, ..old.clone() };
        self.execute(Box::new(SetClipVisual { seq: self.active, id, old, new }));
        Ok(())
    }

    /// Deflicker clip `id`: measure each frame's brightness (and colour) in
    /// the background, and key a Deflicker effect (added if the clip has none;
    /// its Smoothing and colour settings used) to even it out. One undo step.
    pub fn start_deflicker(&mut self, id: ClipId) {
        let Some(c) = self.footage_for_job(id, "Deflicker") else { return };
        let Some(media) = self.project.media(c.media).cloned() else { return };
        let (from, until) = self.media_span(&c);
        let (smoothing, colour) = match c.effects.iter().find(|e| e.kind == EffectKind::Deflicker) {
            Some(e) => (e.params.get(df::SMOOTHING).map_or(15.0, |p| p.value), e.params.get(df::COLOUR).is_none_or(|p| p.value >= 0.5)),
            None => (15.0, true),
        };
        let (tx, rx) = unbounded();
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop = cancel.clone();
        self.playback.pause();
        let spawned = std::thread::Builder::new()
            .name("deflicker".into())
            .spawn(move || {
                let ptx = tx.clone();
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<Option<Deflickered>, String> {
                    let measured = reelforge_media::frame_means(&media.path, &media.info, from, until, &stop, |p| {
                        let _ = ptx.send(TrackMsg::Progress(p));
                    })
                    .map_err(|e| format!("{e:#}"))?;
                    let Some((means, times)) = measured else { return Ok(None) };
                    let gains = reelforge_core::deflicker::gains(&means, smoothing, colour);
                    Ok(Some(Deflickered { gains, times, smoothing, colour }))
                }))
                .unwrap_or_else(|_| Err("deflickering hit a problem with this shot".into()));
                let _ = tx.send(TrackMsg::DeflickerDone(r));
            });
        if let Err(e) = spawned {
            self.status = format!("Couldn't start deflickering: {e}");
            return;
        }
        self.start_job(id, rx, cancel);
        self.status = "Deflickering: measuring each frame's brightness…".into();
    }

    pub(super) fn finish_deflicker(&mut self, job: &TrackJob, r: Result<Option<Deflickered>, String>) {
        let d = match r {
            Err(e) => {
                self.status = format!("Deflickering failed: {e}");
                return;
            }
            Ok(None) => {
                self.status = "Deflickering stopped (nothing was changed)".into();
                return;
            }
            Ok(Some(d)) => d,
        };
        let Some(c) = self.project.sequences.get(job.seq).and_then(|s| s.find_clip(job.clip)).map(|(_, c)| c.clone()) else { return };
        let old = c.visual();
        let mut new = old.clone();
        let index = match new.effects.iter().position(|e| e.kind == EffectKind::Deflicker) {
            Some(i) => i,
            None => {
                let mut fx = Effect::new(EffectKind::Deflicker);
                if let Some(p) = fx.params.get_mut(df::SMOOTHING) {
                    p.value = d.smoothing;
                }
                if let Some(p) = fx.params.get_mut(df::COLOUR) {
                    p.value = if d.colour { 1.0 } else { 0.0 };
                }
                new.effects.push(fx);
                new.effects.len() - 1
            }
        };
        let Some(fx) = new.effects.get_mut(index) else { return };
        let mut keys: [Vec<Key<f32>>; 4] = Default::default();
        for (g, &mt) in d.gains.iter().zip(&d.times) {
            let Some(t) = c.timeline_of_media(mt) else { continue };
            if t < c.timeline_start || t >= c.timeline_end() {
                continue;
            }
            let at = c.source_time(t);
            for (k, v) in keys.iter_mut().zip(g) {
                if k.last().is_some_and(|l| l.at == at) {
                    continue;
                }
                k.push(Key::new(at, *v, Interp::Linear));
            }
        }
        let n = keys.first().map_or(0, |k| k.len());
        if n == 0 {
            self.status = "Nothing to deflicker: no frames of the clip were measured".into();
            return;
        }
        let params = [df::BRIGHTNESS, df::RED, df::GREEN, df::BLUE];
        for (p, mut ks) in params.into_iter().zip(keys) {
            ks.sort_by_key(|k| k.at);
            let Some(param) = fx.params.get_mut(p) else { continue };
            if let Some(first) = ks.first() {
                param.value = first.value;
            }
            param.keys = Curve(ks);
        }
        // The largest correction, to say how much flicker there was.
        let most = d.gains.iter().map(|g| (g[0] / 100.0).max(1e-3).ln().abs()).fold(0.0f32, f32::max);
        self.execute(Box::new(SetClipVisual { seq: job.seq, id: job.clip, old, new }));
        self.status = format!("Deflickered {n} frames (the most any frame needed: {:.0} %); change Smoothing and deflicker again to even out slower or faster changes", (most.exp() - 1.0) * 100.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reelforge_core::project::MediaAsset;

    /// A clip made with ffmpeg (`filter`, at `w`×`h`), on the timeline for
    /// `secs`, in an editor with a sequence its size. `None` without ffmpeg.
    fn editor_with(name: &str, filter: &str, w: u32, h: u32, secs: f64) -> Option<(Editor, ClipId, std::path::PathBuf)> {
        let dir = std::env::temp_dir().join(format!("rf-engine-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).ok()?;
        let f = dir.join("a.mp4");
        let ok = std::process::Command::new("ffmpeg")
            .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i", filter, "-pix_fmt", "yuv420p", "-crf", "12"])
            .arg(&f)
            .status()
            .is_ok_and(|s| s.success());
        if !ok {
            return None;
        }
        let mut e = Editor::new(None, None);
        let info = reelforge_media::probe(&f).unwrap();
        e.project.media.push(MediaAsset { id: MediaId(1), path: f, name: "a.mp4".into(), info, conform: None });
        (e.project.sequences[0].width, e.project.sequences[0].height) = (w, h);
        let id = ClipId(e.project.alloc_id());
        let c = Clip::new(id, MediaId(1), Time::ZERO, Time::ZERO, Time::from_secs_f64(secs), None);
        let v = e.seq().tracks.iter().position(|tr| tr.kind == reelforge_core::project::TrackKind::Video).unwrap();
        e.project.sequences[0].tracks[v].clips.push(c);
        Some((e, id, dir))
    }

    fn wait(e: &mut Editor) {
        let t0 = std::time::Instant::now();
        while e.tracking.is_some() && t0.elapsed().as_secs() < 180 {
            e.poll_tracking();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    /// A detailed picture seen through a shaking window: held still, each
    /// frame is corner-pinned back by the shake (to within a pixel or so).
    #[test]
    fn a_shaky_shot_is_held_still_with_nothing_set_up() {
        let shake = |t: f64| (20.0 * (t * 13.0).sin(), 15.0 * (t * 17.0).cos());
        // (A still of the test picture, whose own pattern moves.)
        let filter = "testsrc2=size=480x270:rate=25:duration=0.04,tpad=stop=60:stop_mode=clone,crop=320:180:x='80+20*sin(t*13)':y='45+15*cos(t*17)'";
        let Some((mut e, id, dir)) = editor_with("autostab", filter, 320, 180, 2.0) else { return };
        e.start_auto_stabilise(id, 0.0, StabZoom::Off);
        wait(&mut e);
        assert!(e.status.starts_with("Stabilised, held still"), "{}", e.status);
        let (_, c) = e.seq().find_clip(id).unwrap();
        let keys = &c.anim.pin.0;
        assert!(keys.len() >= 45, "{} keys", keys.len());
        let s0 = shake(0.0);
        let mut worst = 0.0f64;
        for k in keys {
            let t = k.at.as_secs_f64();
            let s = shake(t);
            let mid = k.value.0.iter().fold([0.0f64; 2], |m, p| [m[0] + p[0] as f64 / 4.0, m[1] + p[1] as f64 / 4.0]);
            // The picture shifted by the crop's offset since frame 0.
            let (dx, dy) = (mid[0] - 160.0 - (s.0 - s0.0), mid[1] - 90.0 - (s.1 - s0.1));
            worst = worst.max(dx.hypot(dy));
        }
        assert!(worst < 2.0, "off by up to {worst:.2} px");
        // One undo step.
        e.undo();
        assert!(e.seq().find_clip(id).unwrap().1.anim.pin.0.is_empty());
        // Smoothed, with the zoom: still keyed, and scaled up to hide the edges.
        e.start_auto_stabilise(id, 15.0, StabZoom::Fixed);
        wait(&mut e);
        assert!(e.status.starts_with("Stabilised:"), "{}", e.status);
        assert!(!e.seq().find_clip(id).unwrap().1.anim.pin.0.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Every other frame brighter: deflickered, the effect is added and keyed
    /// to darken those frames and lighten the others, evening them out.
    #[test]
    fn flicker_is_measured_and_keyed() {
        let filter = "testsrc2=size=160x90:rate=25:duration=2,eq=eval=frame:brightness='if(mod(n\\,2)\\,0.12\\,0)'";
        let Some((mut e, id, dir)) = editor_with("deflicker", filter, 160, 90, 2.0) else { return };
        e.start_deflicker(id);
        wait(&mut e);
        assert!(e.status.starts_with("Deflickered"), "{}", e.status);
        let (_, c) = e.seq().find_clip(id).unwrap();
        let fx = c.effects.iter().find(|f| f.kind == EffectKind::Deflicker).expect("a Deflicker effect");
        let keys = &fx.params[df::BRIGHTNESS].keys.0;
        assert!(keys.len() >= 45, "{} keys", keys.len());
        for w in keys.windows(2).skip(10).take(20) {
            let (a, b) = (w[0].value, w[1].value);
            assert!((a - b).abs() > 3.0, "{a} then {b}: not evening out");
        }
        let at = |i: usize| keys[i].value;
        // (Frame 1, 3… are the bright ones.)
        assert!(at(21) < 100.0 && at(20) > 100.0, "{} {}", at(20), at(21));
        e.undo();
        assert!(!e.seq().find_clip(id).unwrap().1.effects.iter().any(|f| f.kind == EffectKind::Deflicker));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
