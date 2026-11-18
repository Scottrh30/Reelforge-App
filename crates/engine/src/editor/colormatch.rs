//! Match colour (clip context menu, or Controls): measure the look of a
//! reference — a picture or video file, something in the Media panel, or
//! another clip as it's graded on the timeline — and of each chosen clip
//! (with its effects), and give each clip the grade that brings it to the
//! reference: a LUT effect (a .cube made for it, see
//! `reelforge_core::colormatch`), at the end of its stack, whose Strength
//! sets how far. Matching again replaces that LUT. One undo step.

use super::Editor;
use crossbeam_channel::{unbounded, Receiver};
use reelforge_core::colormatch::{ColorStats, MatchWhat, Transfer};
use reelforge_core::effects::{Effect, EffectData, EffectKind};
use reelforge_core::{Clip, ClipId, MediaId, Project, Sequence, Time, Transform};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// What the look is taken from.
#[derive(Clone, Debug, PartialEq, serde::Deserialize)]
pub enum MatchSource {
    /// A picture or video file.
    File(PathBuf),
    /// Footage or a picture in the Media panel.
    Media(MediaId),
    /// A clip on the timeline, as it's graded (its effects included).
    Clip(ClipId),
}

/// Frames taken from each clip and from a video reference.
const FRAMES: usize = 8;

/// The file name every Match colour LUT starts with (how a clip's earlier match is found).
const PREFIX: &str = "Colour match";

pub struct MatchJob {
    /// 0–1.
    pub progress: f32,
    rx: Receiver<MatchMsg>,
    cancel: Arc<AtomicBool>,
    seq: usize,
}

enum MatchMsg {
    Progress(f32),
    /// Each clip's new LUT file (`None`: stopped).
    Done(Result<Option<Vec<(ClipId, PathBuf)>>, String>),
}

/// Something to measure: frames of a lone clip rendered with its effects
/// (a sequence of its own in a copy of the project), or of a file.
enum Sample {
    Rendered { project: Arc<Project>, seq: usize, times: Vec<Time>, fallback: Option<(PathBuf, Vec<Time>)> },
    File { path: PathBuf, times: Vec<Time> },
}

/// `n` times spread through `[start, start + len)`, in the middle of each part.
fn spread(start: Time, len: Time, n: usize) -> Vec<Time> {
    let n = n.max(1);
    (0..n).map(|i| Time(start.0 + (len.0 as f64 * (i as f64 + 0.5) / n as f64) as i64)).collect()
}

/// Where Match colour LUTs go: a `Looks` folder beside the project, or in
/// Reelforge's own folder when it hasn't been saved.
fn looks_dir(project_path: Option<&Path>) -> PathBuf {
    match project_path.and_then(|p| p.parent()) {
        Some(d) => d.join("Looks"),
        None => crate::plugins::user_dir().parent().map(|p| p.join("looks")).unwrap_or_else(|| std::env::temp_dir().join("reelforge-looks")),
    }
}

fn safe_name(s: &str) -> String {
    let cleaned: String = s.chars().map(|c| if c.is_alphanumeric() || " -_.".contains(c) { c } else { '_' }).collect();
    cleaned.trim().chars().take(40).collect()
}

/// Is this LUT effect one that Match colour made?
pub fn is_match_lut(e: &Effect) -> bool {
    e.kind == EffectKind::Lut && matches!(&e.data, Some(EffectData::Lut(p)) if p.file_name().is_some_and(|n| n.to_string_lossy().starts_with(PREFIX)))
}

impl Editor {
    /// A lone clip with its effects, masks and speed (not its place, size,
    /// opacity or parent), in a sequence the size of its picture, in a copy
    /// of the project; and times spread through it. `None` for clips with no
    /// picture of their own (titles, planes, adjustment layers, cameras).
    fn solo(&self, c: &Clip) -> Option<Sample> {
        if c.title.is_some() || c.plane.is_some() || c.adjustment || c.scene.is_some() {
            return None;
        }
        let [w, h] = self.project.picture_size(c)?;
        let mut p = self.project.clone();
        let mut clip = c.clone();
        clip.timeline_start = Time::ZERO;
        clip.transform = Transform::default();
        clip.anim = Default::default();
        clip.parent = None;
        clip.track_matte = None;
        clip.dissolve_in = Time::ZERO;
        let mut seq = Sequence::new("Match colour", w.max(2) & !1, h.max(2) & !1, self.seq().frame_rate);
        let times = spread(Time::ZERO, clip.duration, FRAMES);
        seq.tracks[0].clips = vec![clip];
        p.sequences.push(seq);
        let seq = p.sequences.len() - 1;
        // Without a graphics card: the clip's own frames, before its effects.
        let fallback = (c.nested.is_none()).then(|| self.project.media(c.media)).flatten().map(|m| (m.path.clone(), times.iter().map(|&t| c.source_time(c.timeline_start + t)).collect()));
        Some(Sample::Rendered { project: Arc::new(p), seq, times, fallback })
    }

    /// Give `targets` the look of `reference` (`what`: tone, colour or both)
    /// in the background; see the module notes.
    pub fn start_color_match(&mut self, targets: &[ClipId], reference: MatchSource, what: MatchWhat) {
        self.commit_live_edit();
        if self.color_match.is_some() {
            self.status = "Match colour is already running".into();
            return;
        }
        if !what.tone && !what.color {
            self.status = "Match colour: choose colour, brightness or both".into();
            return;
        }
        let s = self.seq();
        let mut jobs: Vec<(ClipId, String, Sample)> = Vec::new();
        for &id in targets {
            if reference == MatchSource::Clip(id) {
                continue;
            }
            let Some((_, c)) = s.find_clip(id) else { continue };
            if let Some(sample) = self.solo(c) {
                let name = if let Some(n) = c.nested { self.project.sequences.get(n).map(|s| s.name.clone()).unwrap_or_default() } else { self.project.media(c.media).map(|m| m.name.clone()).unwrap_or_default() };
                jobs.push((id, name, sample));
            }
        }
        if jobs.is_empty() {
            self.status = "Match colour: select the video or picture clips to change (titles, planes and adjustment layers have no picture of their own)".into();
            return;
        }
        let (reference, ref_name) = match &reference {
            MatchSource::File(path) => {
                let Ok(info) = reelforge_media::probe(path) else {
                    self.status = format!("Match colour: can't read {}", path.display());
                    return;
                };
                if info.video.is_none() {
                    self.status = format!("Match colour: {} has no picture", path.display());
                    return;
                }
                let still = info.duration <= Time::from_secs_f64(0.1);
                let times = if still { vec![Time::ZERO] } else { spread(Time::ZERO, info.duration, FRAMES) };
                (Sample::File { path: path.clone(), times }, path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default())
            }
            MatchSource::Media(id) => {
                let Some(m) = self.project.media(*id).filter(|m| m.info.video.is_some()) else {
                    self.status = "Match colour: that has no picture".into();
                    return;
                };
                let times = if self.project.is_still(*id) { vec![Time::ZERO] } else { spread(Time::ZERO, m.info.duration, FRAMES) };
                (Sample::File { path: m.path.clone(), times }, m.name.clone())
            }
            MatchSource::Clip(id) => {
                let Some((_, c)) = s.find_clip(*id) else { return };
                let Some(sample) = self.solo(c) else {
                    self.status = "Match colour: the reference clip has no picture of its own".into();
                    return;
                };
                let name = self.project.media(c.media).map(|m| m.name.clone()).unwrap_or_else(|| "clip".into());
                (sample, name)
            }
        };
        let dir = looks_dir(self.project_path.as_deref());
        let (tx, rx) = unbounded();
        let cancel = Arc::new(AtomicBool::new(false));
        let stop = cancel.clone();
        let n = jobs.len();
        let shown_name = ref_name.clone();
        let spawned = std::thread::Builder::new().name("match colour".into()).spawn(move || {
            let r = (|| -> Result<Option<Vec<(ClipId, PathBuf)>>, String> {
                let gpu = reelforge_render::headless_device();
                let total = (n + 1) as f32;
                let to = measure(&reference, gpu.as_ref(), &stop)?;
                let Some(to) = to else { return Ok(None) };
                if to.samples < 50 {
                    return Err("the reference has no picture to match".into());
                }
                let _ = tx.send(MatchMsg::Progress(1.0 / total));
                std::fs::create_dir_all(&dir).map_err(|e| format!("couldn't make {}: {e}", dir.display()))?;
                let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() % 1_000_000_000);
                let mut out = Vec::new();
                for (i, (id, name, sample)) in jobs.iter().enumerate() {
                    let Some(from) = measure(sample, gpu.as_ref(), &stop)? else { return Ok(None) };
                    if from.samples < 50 {
                        continue;
                    }
                    let t = Transfer::new(&from, &to, what);
                    let file = dir.join(format!("{PREFIX} - {} to {} {stamp}-{}.cube", safe_name(name), safe_name(&ref_name), id.0));
                    std::fs::write(&file, t.cube(33, &format!("{name} matched to {ref_name}"))).map_err(|e| format!("couldn't write {}: {e}", file.display()))?;
                    out.push((*id, file));
                    let _ = tx.send(MatchMsg::Progress((i + 2) as f32 / total));
                }
                Ok(Some(out))
            })();
            let _ = tx.send(MatchMsg::Done(r));
        });
        if spawned.is_err() {
            self.status = "Couldn't start Match colour".into();
            return;
        }
        self.status = format!("Matching {} to {shown_name}…", if n == 1 { "the clip".to_string() } else { format!("{n} clips") });
        self.color_match = Some(MatchJob { progress: 0.0, rx, cancel, seq: self.active });
    }

    pub fn cancel_color_match(&mut self) {
        if let Some(j) = &self.color_match {
            j.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Take in the running match's news; at the end, give the clips their LUTs.
    pub(crate) fn poll_color_match(&mut self) {
        let Some(job) = self.color_match.as_mut() else { return };
        let mut done = None;
        while let Ok(m) = job.rx.try_recv() {
            match m {
                MatchMsg::Progress(p) => job.progress = p,
                MatchMsg::Done(r) => done = Some(r),
            }
        }
        let Some(result) = done else { return };
        let job = self.color_match.take().unwrap();
        match result {
            Ok(Some(luts)) if !luts.is_empty() => self.apply_match_luts(job.seq, &luts),
            Ok(Some(_)) => self.status = "Match colour: nothing to measure in those clips".into(),
            Ok(None) => self.status = "Match colour stopped".into(),
            Err(e) => self.status = format!("Match colour failed: {e}"),
        }
    }

    /// Each clip's LUT in place of its earlier match (or added at the end), as one undo step.
    pub fn apply_match_luts(&mut self, seq: usize, luts: &[(ClipId, PathBuf)]) {
        use reelforge_core::undo::{Batch, SetClipVisual};
        self.commit_live_edit();
        let mut cmds: Vec<Box<dyn reelforge_core::undo::Command>> = Vec::new();
        for (id, path) in luts {
            let Some((_, c)) = self.project.sequences.get(seq).and_then(|s| s.find_clip(*id)) else { continue };
            let old = c.visual();
            let mut new = old.clone();
            match new.effects.iter_mut().rev().find(|e| is_match_lut(e)) {
                Some(e) => e.data = Some(EffectData::Lut(path.clone())),
                None => {
                    let mut e = Effect::new(EffectKind::Lut);
                    e.data = Some(EffectData::Lut(path.clone()));
                    new.effects.push(e);
                }
            }
            cmds.push(Box::new(SetClipVisual { seq, id: *id, old, new }));
        }
        let n = cmds.len();
        if n == 0 {
            return;
        }
        self.execute(Box::new(Batch { name: "Match colour".into(), cmds }));
        self.status = format!("Matched colour on {}: a LUT effect at the end of {} (its Strength sets how far)", if n == 1 { "the clip".to_string() } else { format!("{n} clips") }, if n == 1 { "its effects" } else { "each one's effects" });
    }
}

/// The look of a sample (`None`: stopped).
fn measure(s: &Sample, gpu: Option<&(reelforge_render::wgpu::Device, reelforge_render::wgpu::Queue)>, stop: &AtomicBool) -> Result<Option<ColorStats>, String> {
    let mut stats = ColorStats::new();
    let from_file = |path: &Path, times: &[Time], stats: &mut ColorStats| -> Result<bool, String> {
        let mut src = reelforge_media::FrameSource::open(path, Some((640, 640))).map_err(|e| format!("{e:#}"))?;
        for &t in times {
            if stop.load(Ordering::Relaxed) {
                return Ok(false);
            }
            if let Some(f) = src.frame_at_blocking(t, Duration::from_secs(20)) {
                stats.add_rgba(&f.rgba(), f.width as usize, f.height as usize);
            }
        }
        Ok(true)
    };
    match s {
        Sample::File { path, times } => {
            if !from_file(path, times, &mut stats)? {
                return Ok(None);
            }
        }
        Sample::Rendered { project, seq, times, fallback } => match gpu {
            Some((device, queue)) => {
                for &t in times {
                    if stop.load(Ordering::Relaxed) {
                        return Ok(None);
                    }
                    let (rgba, [w, h]) = reelforge_export::render_still(project, *seq, t, device, queue).map_err(|e| format!("{e:#}"))?;
                    stats.add_rgba(&rgba, w as usize, h as usize);
                }
            }
            None => match fallback {
                Some((path, times)) => {
                    if !from_file(path, times, &mut stats)? {
                        return Ok(None);
                    }
                }
                None => return Err("compositions need the graphics card to be measured".into()),
            },
        },
    }
    Ok(Some(stats))
}

#[cfg(test)]
mod tests {
    use super::*;
    use reelforge_core::project::MediaAsset;

    fn clip_of(dir: &Path, name: &str, filter: &str) -> Option<PathBuf> {
        let f = dir.join(name);
        let ok = std::process::Command::new("ffmpeg")
            .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i", &format!("testsrc2=s=160x90:r=10:d=1,{filter}"), "-pix_fmt", "yuv420p"])
            .arg(&f)
            .status()
            .is_ok_and(|s| s.success());
        ok.then_some(f)
    }

    fn mean_rgb(path: &Path, lut: Option<&Path>) -> [f32; 3] {
        let mut src = reelforge_media::FrameSource::open(path, None).unwrap();
        let f = src.frame_at_blocking(Time::from_secs_f64(0.5), Duration::from_secs(20)).unwrap();
        let rgba = f.rgba();
        let t = lut.map(|p| reelforge_core::lut::load_cube(p).unwrap());
        let mut sum = [0.0f32; 3];
        let n = (f.width * f.height) as f32;
        for p in rgba.chunks_exact(4) {
            let mut c = [p[0] as f32 / 255.0, p[1] as f32 / 255.0, p[2] as f32 / 255.0];
            if let Some(t) = &t {
                // Nearest LUT point is plenty for a mean.
                let n = t.cube_size as usize;
                let idx = c.map(|v| (v * (n - 1) as f32).round() as usize);
                let texel = (idx[1] * t.width as usize + idx[2] * n + idx[0]) * 4;
                c = [t.rgba[texel] as f32 / 255.0, t.rgba[texel + 1] as f32 / 255.0, t.rgba[texel + 2] as f32 / 255.0];
            }
            for k in 0..3 {
                sum[k] += c[k];
            }
        }
        sum.map(|s| s / n)
    }

    /// A neutral clip matched to a warm, dark version of the same picture:
    /// it gets a LUT effect whose look is warm and dark; matching again
    /// replaces it; undo takes it off.
    #[test]
    fn a_clip_takes_the_look_of_another() {
        let dir = std::env::temp_dir().join(format!("rf-match-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let Some(plain) = clip_of(&dir, "plain.mp4", "null") else { return };
        let Some(warm) = clip_of(&dir, "warm.mp4", "colorchannelmixer=rr=1.0:gg=0.8:bb=0.55,eq=brightness=-0.08") else { return };
        let project_file = dir.join("p.rfproj");
        let mut e = Editor::new(None, None);
        e.project_path = Some(project_file);
        for (i, f) in [&plain, &warm].iter().enumerate() {
            let info = reelforge_media::probe(f).unwrap();
            e.project.media.push(MediaAsset { id: MediaId(i as u64 + 1), path: (*f).clone(), name: f.file_name().unwrap().to_string_lossy().into(), info, conform: None });
        }
        let s = Time::from_secs_f64;
        e.project.sequences[0].tracks[0].clips = vec![Clip::new(ClipId(10), MediaId(1), s(0.0), s(0.0), s(1.0), None), Clip::new(ClipId(11), MediaId(2), s(1.0), s(0.0), s(1.0), None)];
        let run = |e: &mut Editor| {
            let t0 = std::time::Instant::now();
            while e.color_match.is_some() && t0.elapsed().as_secs() < 120 {
                e.poll_color_match();
                std::thread::sleep(Duration::from_millis(20));
            }
        };
        e.start_color_match(&[ClipId(10)], MatchSource::Clip(ClipId(11)), MatchWhat { tone: true, color: true });
        run(&mut e);
        let fx = &e.seq().find_clip(ClipId(10)).unwrap().1.effects;
        assert_eq!(fx.len(), 1, "{}", e.status);
        assert!(is_match_lut(&fx[0]));
        let Some(EffectData::Lut(lut)) = fx[0].data.clone() else { panic!() };
        assert!(lut.starts_with(dir.join("Looks")), "{}", lut.display());
        let before = mean_rgb(&plain, None);
        let target = mean_rgb(&warm, None);
        let after = mean_rgb(&plain, Some(&lut));
        // Much closer to the reference overall (and no channel further off), and warm like it.
        let off = |c: [f32; 3]| (0..3).map(|k| (c[k] - target[k]).abs()).sum::<f32>();
        assert!(off(after) < off(before) * 0.5, "{before:?} → {after:?}, reference {target:?}");
        for k in 0..3 {
            assert!((after[k] - target[k]).abs() <= (before[k] - target[k]).abs() + 0.01, "channel {k}: {before:?} → {after:?}, reference {target:?}");
        }
        assert!(after[0] - after[2] > (before[0] - before[2]) + 0.1, "warmer: {before:?} → {after:?}");
        // Again, from a file (a still frame of it would do as well): the same LUT effect, new file.
        e.start_color_match(&[ClipId(10)], MatchSource::File(warm.clone()), MatchWhat { tone: false, color: true });
        run(&mut e);
        let fx = &e.seq().find_clip(ClipId(10)).unwrap().1.effects;
        assert_eq!(fx.len(), 1);
        assert!(matches!(&fx[0].data, Some(EffectData::Lut(p)) if *p != lut));
        e.undo();
        e.undo();
        assert!(e.seq().find_clip(ClipId(10)).unwrap().1.effects.is_empty());
        // A title can't be matched.
        e.start_color_match(&[ClipId(99)], MatchSource::Media(MediaId(2)), MatchWhat { tone: true, color: true });
        assert!(e.color_match.is_none() && e.status.contains("select the video"), "{}", e.status);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
