//! Offline export: renders a sequence frame by frame through the same GPU
//! compositor the viewer uses, mixes its audio with the same mixer the
//! speakers hear, and encodes both with FFmpeg.
//!
//! Unlike the viewer, export never shows a "close enough" frame: it waits for
//! every layer's exact frame at full resolution. It runs on whatever thread
//! calls it (the app uses a background thread) and reports progress through a
//! callback that can also cancel.

use anyhow::{anyhow, Context as _, Result};
use reelforge_core::{ClipId, FrameRate, Layer, Project, Time, TrackKind};
use reelforge_media::{ColorOut, Fit, FrameSource, MixClip, Mixer, MovieWriter, Quality, VideoCodec, VideoFrame, WriterSettings};
use reelforge_render::{wgpu, Compositor, DeepCurve};
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub mod ofxhost;
pub mod scene;
pub mod zerocheck;
pub use scene::{frame_recipe, FrameProvider, Picture, Scene};

/// Audio is always exported at 48 kHz, the video standard.
pub const AUDIO_RATE: u32 = 48_000;
/// How long to wait for a decoder to produce a frame before giving up.
const FRAME_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Debug)]
pub struct ExportSettings {
    /// The output file. For an image sequence this is the first file's name
    /// without the number ("shot.png" becomes shot_00001.png, shot_00002.png, …).
    pub path: PathBuf,
    /// Output size; the sequence is rendered at its own size and scaled to this.
    pub width: u32,
    pub height: u32,
    pub quality: Quality,
    pub include_audio: bool,
    /// Range of the sequence to export, `[start, end)`.
    pub start: Time,
    pub end: Time,
    /// The video format, or `None` for audio only (WAV).
    pub video: Option<VideoCodec>,
    /// Use a GPU encoder (NVENC, Quick Sync, AMF…) when there is one.
    pub hardware: bool,
    /// What to do when the output's shape differs from the sequence's.
    pub fit: Fit,
    /// Which sequence of the project (0 is the main edit; others are compositions).
    pub sequence: usize,
    /// Dynamic range and bit depth (10-bit/HDR only where the codec supports it).
    pub color: ColorOut,
    /// Target bitrate in kbit/s instead of the quality setting (H.264/H.265).
    pub bitrate_kbps: Option<u32>,
    /// Keep the transparency: where no layer covers the frame it's see-through
    /// rather than black (ProRes 4444, CineForm 4444, PNG and TIFF only).
    pub alpha: bool,
    /// Frames a second written (the sequence's when `None`): every frame is
    /// drawn at its own moment, so titles and animation move smoothly at it.
    pub frame_rate: Option<FrameRate>,
    /// How footage fills the moments between its frames for this export,
    /// over what each clip says (`None` or Nearest: as each clip says) — so a
    /// 24p edit delivered at 60p can be blended or flowed throughout.
    pub between_frames: Option<reelforge_core::FrameBlend>,
}

impl ExportSettings {
    /// The path actually written: for image sequences, `name_%05d.ext`.
    pub fn output_path(&self) -> PathBuf {
        match self.video {
            Some(v) if v.is_sequence() => {
                let stem = self.path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "frame".into());
                self.path.with_file_name(format!("{stem}_%05d.{}", v.extension()))
            }
            _ => self.path.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Progress {
    /// Frames finished so far.
    pub done: u64,
    pub total: u64,
    pub elapsed: Duration,
}

impl Progress {
    pub fn fraction(&self) -> f32 {
        if self.total == 0 { 1.0 } else { self.done as f32 / self.total as f32 }
    }
    /// Estimated time left, once there's enough data to guess.
    pub fn remaining(&self) -> Option<Duration> {
        (self.done >= 5 && self.done < self.total)
            .then(|| self.elapsed.mul_f64((self.total - self.done) as f64 / self.done as f64))
    }
}

#[derive(Clone, Debug)]
pub struct Report {
    pub path: PathBuf,
    pub frames: u64,
    pub width: u32,
    pub height: u32,
    pub video_codec: String,
    /// The encoder ran on the GPU.
    pub hardware: bool,
    pub has_audio: bool,
    pub elapsed: Duration,
}

/// Returned (inside `anyhow::Error`) when the progress callback asked to stop.
#[derive(Debug)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("export cancelled")
    }
}
impl std::error::Error for Cancelled {}

/// Frame index range `[first, last)` covering `[start, end)` of a sequence.
pub fn frame_range(rate: FrameRate, start: Time, end: Time) -> (i64, i64) {
    let first = start.to_frame(rate);
    let mut last = end.to_frame(rate);
    if Time::from_frame(last, rate) < end {
        last += 1; // a partial final frame still gets rendered
    }
    (first, last.max(first))
}

/// The audio clips of sequence `seq` as the mixer wants them, at `rate`.
///
/// Folds clip and track volume/pan together, converts fades and volume
/// keyframes to samples, and resolves crossfades: the outgoing clip is extended
/// past the cut into its spare media and the incoming one starts early, each
/// with an equal-power ramp across the overlap. Compositions on audio
/// tracks contribute their own sequence's audio, flattened. Used for both
/// playback and export, so they always sound the same.
pub fn mix_clips(project: &Project, seq: usize, rate: u32) -> Vec<MixClip> {
    mix_sequence(project, seq, rate, 0)
}

fn mix_sequence(project: &Project, seq: usize, rate: u32, depth: usize) -> Vec<MixClip> {
    use reelforge_core::audio::{balance, db_to_gain};
    let Some(sequence) = project.sequences.get(seq) else { return Vec::new() };
    let sr = FrameRate::new(rate as i64, 1);
    let crossfades = project.crossfades(seq);
    let mut out = Vec::new();
    for (ti, t) in sequence.tracks.iter().enumerate().filter(|(_, t)| t.kind == TrackKind::Audio && !t.muted) {
        for c in &t.clips {
            // Time-remapped sound is silent (it would play at every speed at once).
            if c.remap.is_some() {
                continue;
            }
            let [cl, cr] = balance(c.audio.pan);
            let [tl, tr] = balance(t.audio.pan);
            let track_gain = db_to_gain(t.audio.volume_db);
            if let Some(inner) = c.nested {
                if depth >= scene::MAX_NESTING {
                    continue;
                }
                // The composition's own mix, moved to where the clip plays it and cut
                // to the clip's length. The clip's (static) volume, pan and fades apply on top.
                let (ws, we) = (c.timeline_start.to_frame(sr), c.timeline_end().to_frame(sr));
                let (fin, fout) = c.audio.clamped_fades(c.duration);
                let gain = db_to_gain(c.audio.volume_db) * track_gain;
                let r = c.rate();
                if r == 0.0 {
                    continue; // a frozen composition is silent
                }
                // Inner sample x plays at outer sample y where x = a + r·(cs + y − ws).
                let (a, cs) = (c.anchor.to_frame(sr) as f64, c.source_in.to_frame(sr) as f64);
                let outer = |x: f64| ws as f64 + (x - a) / r - cs;
                // A multicam clip plays only the angle it's heard on.
                let heard = c.angle.zip(project.sequences.get(inner).filter(|s| s.multicam)).map(|(a, s)| reelforge_core::multicam::audio_angles(s).get(a.heard() as usize).copied());
                for mut m in mix_sequence(project, inner, rate, depth + 1) {
                    if heard.is_some_and(|track| track != Some(m.track)) {
                        continue;
                    }
                    if !c.is_retimed() {
                        m.start += ws - c.source_in.to_frame(sr);
                    } else {
                        let (y0, y1) = (outer(m.start as f64), outer((m.start + m.len) as f64));
                        let start = y0.min(y1).round() as i64;
                        let len = (y0 - y1).abs().round() as i64;
                        // Media heard at the new start, and the combined rate.
                        let x_at_start = a + r * (cs + (start - ws) as f64);
                        m.source_in = (m.source_in as f64 + (x_at_start - m.start as f64) * m.rate).round() as i64;
                        // Effect keyframes run at the composition's speed too.
                        m.fx_src0 += (x_at_start - m.start as f64) * m.fx_scale;
                        m.fx_scale *= r;
                        m.rate *= r;
                        let k = 1.0 / r.abs();
                        let stretch = |v: i64| (v as f64 * k).round() as i64;
                        (m.fade_in, m.fade_out) = if r < 0.0 { (stretch(m.fade_out), stretch(m.fade_in)) } else { (stretch(m.fade_in), stretch(m.fade_out)) };
                        for key in m.volume_keys.iter_mut().chain(m.pan_keys.iter_mut()) {
                            key.0 = if r < 0.0 { len - stretch(key.0) } else { stretch(key.0) };
                        }
                        m.volume_keys.sort_by_key(|k| k.0);
                        m.pan_keys.sort_by_key(|k| k.0);
                        (m.start, m.len) = (start, len);
                    }
                    if m.start < ws {
                        let cut = ws - m.start;
                        m.start = ws;
                        m.source_in += (cut as f64 * m.rate).round() as i64;
                        m.fx_src0 += cut as f64 * m.fx_scale;
                        m.len -= cut;
                        m.fade_in = (m.fade_in - cut).max(0);
                        m.volume_keys.iter_mut().chain(m.pan_keys.iter_mut()).for_each(|k| k.0 -= cut);
                    }
                    if m.start + m.len > we {
                        m.len = we - m.start;
                        m.fade_out = 0;
                    }
                    if m.len <= 0 {
                        continue;
                    }
                    if m.start == ws {
                        m.fade_in = m.fade_in.max(fin.to_frame(sr));
                    }
                    if m.start + m.len == we {
                        m.fade_out = m.fade_out.max(fout.to_frame(sr));
                    }
                    let mut h = DefaultHasher::new();
                    (c.id, m.id).hash(&mut h);
                    m.id = ClipId(h.finish());
                    m.gain *= gain;
                    m.balance = [m.balance[0] * cl * tl, m.balance[1] * cr * tr];
                    m.track = ti;
                    if m.gain > 0.0 {
                        out.push(m);
                    }
                }
                continue;
            }
            let Some(m) = project.media(c.media) else { continue };
            let (mut fin, mut fout) = c.audio.clamped_fades(c.duration);
            let (mut start, mut source_in, mut end) = (c.timeline_start, c.source_in, c.timeline_end());
            // A crossfade replaces the plain fade on that edge.
            if let Some(x) = crossfades.iter().find(|x| x.right == c.id && !x.is_empty()) {
                start = start - x.before;
                source_in = source_in - x.before;
                fin = x.len();
            }
            if let Some(x) = crossfades.iter().find(|x| x.left == c.id && !x.is_empty()) {
                end = end + x.after;
                fout = x.len();
            }
            let (gain, volume_keys) = if c.audio.is_animated() {
                let s0 = source_in.to_frame(sr);
                let keys = c.audio.volume_keys.iter().map(|k| (k.at.to_frame(sr) - s0, k.db)).collect();
                (track_gain, keys)
            } else {
                (db_to_gain(c.audio.volume_db) * track_gain, Vec::new())
            };
            // Pan keys replace the clip's static pan; the track's pan stays in `balance`.
            let (balance_lr, pan_keys) = if c.audio.is_pan_animated() {
                let s0 = source_in.to_frame(sr);
                ([tl, tr], c.audio.pan_keys.iter().map(|k| (k.at.to_frame(sr) - s0, k.pan)).collect())
            } else {
                ([cl * tl, cr * tr], Vec::new())
            };
            let start_s = start.to_frame(sr);
            let clip = MixClip {
                id: c.id,
                path: m.path.clone(),
                start: start_s,
                // The media heard at the start (the clip's speed maps clip time to media time).
                source_in: c.media_time(source_in).to_frame(sr),
                rate: c.rate(),
                len: end.to_frame(sr) - start_s,
                gain,
                balance: balance_lr,
                fade_in: fin.to_frame(sr),
                fade_out: fout.to_frame(sr),
                volume_keys,
                pan_keys,
                track: ti,
                effects: c.audio.effects.clone(),
                // The clip's own time at the start (keyframes are in it).
                fx_src0: source_in.to_frame(sr) as f64,
                fx_scale: 1.0,
            };
            if clip.gain > 0.0 {
                out.push(clip);
            }
        }
    }
    out
}

/// For a title's text animation: seconds into its clip at layer `l`, and the clip's length.
pub fn title_clock(l: &Layer) -> (f32, f32) {
    ((l.source_time - l.clip.source_in).as_secs_f64().max(0.0) as f32, l.clip.duration.as_secs_f64() as f32)
}

/// One exact, full-size frame of sequence `seq` at `t` (as export would
/// draw it), as RGBA8 with its size: for snapshots of the Viewer.
pub fn render_still(project: &Project, seq: usize, t: Time, device: &wgpu::Device, queue: &wgpu::Queue) -> Result<(Vec<u8>, [u32; 2])> {
    let mut compositor = Compositor::new(device, queue);
    let mut scene = Scene::new(device, queue);
    let mut frames = ExactFrames { sources: HashMap::new(), titles: HashMap::new(), sharp_titles: HashMap::new(), tick: 1 };
    scene.render(project, seq, t, &mut compositor, &mut frames, None)?;
    let size = compositor.output_size().ok_or_else(|| anyhow!("nothing was drawn"))?;
    let rgba = compositor.read_output().ok_or_else(|| anyhow!("GPU readback failed"))?;
    Ok((rgba, size))
}

/// A title's content hash with how it's drawn for its size mixed in.
pub fn sharp_hash(content: u64, sharp: f32, whole: bool) -> u64 {
    content ^ (sharp.to_bits() as u64).rotate_left(17) ^ if whole { 0x5bd1_e995 } else { 0 }
}

/// Exact, full-size frames for export: one blocking decoder per clip, and titles.
struct ExactFrames {
    sources: HashMap<ClipId, (FrameSource, u64)>,
    titles: HashMap<ClipId, (u64, VideoFrame, u64)>,
    /// 2D titles drawn for the size they're shown at: (hash, picture, the frame pixels it covers, last used).
    sharp_titles: HashMap<ClipId, (u64, VideoFrame, [f32; 2], u64)>,
    /// Counts frames, to close decoders that haven't been needed for a while.
    tick: u64,
}

impl FrameProvider for ExactFrames {
    fn title(&mut self, _: &Project, l: &Layer, frame_size: [u32; 2], sharp: f32, whole: bool) -> Result<Option<Picture>> {
        let Some(spec) = l.clip.title_at(l.source_time) else { return Ok(None) };
        let at = title_clock(l);
        let hash = sharp_hash(spec.content_hash_at(frame_size, at.0, at.1), sharp, whole);
        let tick = self.tick;
        let (frame, size) = match self.sharp_titles.get_mut(&l.clip.id) {
            Some((h, f, size, used)) if *h == hash => {
                *used = tick;
                (f.clone(), *size)
            }
            _ => {
                let r = reelforge_render::text::render_title_sharp(&spec, frame_size, at.0, at.1, sharp, whole);
                let f = VideoFrame::from_rgba(Time::ZERO, r.title.width, r.title.height, r.title.rgba, hash);
                self.sharp_titles.insert(l.clip.id, (hash, f.clone(), r.size, tick));
                (f, r.size)
            }
        };
        Ok(Some(Picture { frame, size, media: reelforge_core::MediaId::GENERATED }))
    }

    fn picture(&mut self, project: &Project, l: &Layer, frame_size: [u32; 2]) -> Result<Option<Picture>> {
        let tick = self.tick;
        if let Some(spec) = l.clip.title_at(l.source_time) {
            let spec = &*spec;
            let at = title_clock(l);
            let hash = spec.content_hash_at(frame_size, at.0, at.1);
            let frame = match self.titles.get_mut(&l.clip.id) {
                Some((h, f, used)) if *h == hash => {
                    *used = tick;
                    f.clone()
                }
                _ => {
                    let r = reelforge_render::text::render_title_at(spec, frame_size, at.0, at.1);
                    let f = VideoFrame::from_rgba(Time::ZERO, r.width, r.height, r.rgba, hash);
                    self.titles.insert(l.clip.id, (hash, f.clone(), tick));
                    f
                }
            };
            let size = [frame_size[0] as f32, frame_size[1] as f32];
            return Ok(Some(Picture { frame, size, media: reelforge_core::MediaId::GENERATED }));
        }
        let Some(media) = project.media(l.clip.media) else { return Ok(None) };
        let Some(v) = &media.info.video else { return Ok(None) };
        let (src, used) = match self.sources.entry(l.clip.id) {
            std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::hash_map::Entry::Vacant(e) => {
                e.insert((FrameSource::open(&media.path, None).with_context(|| format!("opening {}", media.name))?, tick))
            }
        };
        *used = tick;
        let frame = src
            .frame_at_blocking(l.source_time, FRAME_TIMEOUT)
            .ok_or_else(|| anyhow!("{} produced no frame at {}", media.name, l.source_time))?;
        let size = media.layer_size().unwrap_or([v.width, v.height]);
        Ok(Some(Picture { frame, size: [size[0] as f32, size[1] as f32], media: media.id }))
    }
}

/// Render a sequence of `project` (`settings.sequence`) to a movie file.
///
/// `on_progress` is called after every frame; return `false` to cancel, in
/// which case the partial file is deleted and the error is [`Cancelled`].
pub fn export_sequence(
    project: &Project,
    settings: &ExportSettings,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    mut on_progress: impl FnMut(Progress) -> bool,
) -> Result<Report> {
    let started = Instant::now();
    let seq = project.sequences.get(settings.sequence).ok_or_else(|| anyhow!("the project has no sequence {}", settings.sequence))?;
    let rate = settings.frame_rate.filter(|r| r.fps() > 0.0 && r.fps() <= 240.0).unwrap_or(seq.frame_rate);
    let frame_size = [seq.width.max(2), seq.height.max(2)];
    let (first, last) = frame_range(rate, settings.start, settings.end);
    let total = (last - first) as u64;
    if total == 0 {
        return Err(anyhow!("nothing to export: the range is empty"));
    }

    let audio_clips = if settings.include_audio { mix_clips(project, settings.sequence, AUDIO_RATE) } else { Vec::new() };
    // Image sequences carry no sound.
    let has_audio = !audio_clips.is_empty() && !settings.video.is_some_and(|v| v.is_sequence());
    if settings.video.is_none() && !has_audio {
        return Err(anyhow!("nothing to export: audio only, but the range has no audio"));
    }
    let out_path = settings.output_path();
    let mut mixer = Mixer::new(AUDIO_RATE);
    mixer.set_clips(audio_clips);
    let sr = FrameRate::new(AUDIO_RATE as i64, 1);

    let mut writer = MovieWriter::create(&out_path, &WriterSettings {
        width: settings.width,
        height: settings.height,
        input_size: Some(frame_size),
        frame_rate: rate,
        quality: settings.quality,
        audio_rate: has_audio.then_some(AUDIO_RATE),
        proxy: false,
        video: settings.video,
        hardware: settings.hardware,
        fit: settings.fit,
        color: settings.color,
        bitrate_kbps: settings.bitrate_kbps,
        alpha: settings.alpha,
        pixel_aspect: seq.pixel_aspect,
    })?;
    // 10-bit, HDR and ProRes get the 16-bit render in the output's curve.
    let deep = writer.wants_deep().then(|| match writer.color() {
        ColorOut::HdrPq => DeepCurve::Pq,
        ColorOut::HdrHlg => DeepCurve::Hlg,
        ColorOut::Sdr8 | ColorOut::Sdr10 => DeepCurve::Srgb,
    });
    let video_codec = writer.video_codec.clone();

    let mut compositor = Compositor::new(device, queue);
    // A transparent background: the frame starts clear and keeps its coverage.
    compositor.transparent = writer.keeps_alpha();
    let mut scene = Scene::new(device, queue);
    if rate != seq.frame_rate {
        scene.out_fps = Some(rate.fps());
    }
    scene.between = settings.between_frames;
    let mut frames = ExactFrames { sources: HashMap::new(), titles: HashMap::new(), sharp_titles: HashMap::new(), tick: 0 };
    // Decoders not needed for this many frames are closed.
    let idle = (rate.fps() * 2.0).ceil() as u64;
    let mut audio_pos = Time::from_frame(first, rate).to_frame(sr);
    let mut audio_buf = Vec::new();

    for (n, i) in (first..last).enumerate() {
        let t = Time::from_frame(i, rate);
        if settings.video.is_none() {
            // Audio only: just the mix for this frame's span.
            let next = Time::from_frame(i + 1, rate).to_frame(sr);
            audio_buf.resize((next - audio_pos).max(0) as usize * 2, 0.0);
            mixer.render(audio_pos, &mut audio_buf);
            writer.write_audio(&audio_buf)?;
            audio_pos = next;
            if !on_progress(Progress { done: n as u64 + 1, total, elapsed: started.elapsed() }) {
                return Err(Cancelled.into());
            }
            continue;
        }

        // Exact frame for every visible layer (two per dissolve), with keyframes applied.
        frames.tick += 1;
        scene.render(project, settings.sequence, t, &mut compositor, &mut frames, None)?;
        match deep {
            Some(curve) => {
                let px = compositor.read_output_deep(curve).ok_or_else(|| anyhow!("GPU readback failed"))?;
                writer.write_video_deep(&px)?;
            }
            None => {
                let rgba = compositor.read_output().ok_or_else(|| anyhow!("GPU readback failed"))?;
                writer.write_video(&rgba)?;
            }
        }

        // Exactly the audio that belongs to this frame, so A/V never drift apart.
        if has_audio {
            let next = Time::from_frame(i + 1, rate).to_frame(sr);
            audio_buf.resize((next - audio_pos).max(0) as usize * 2, 0.0);
            mixer.render(audio_pos, &mut audio_buf);
            writer.write_audio(&audio_buf)?;
            audio_pos = next;
        }

        // Close decoders (and forget titles) not needed lately.
        let tick = frames.tick;
        frames.sources.retain(|_, (_, used)| tick - *used < idle);
        frames.titles.retain(|_, (_, _, used)| tick - *used < idle);
        frames.sharp_titles.retain(|_, (_, _, _, used)| tick - *used < idle);

        let p = Progress { done: n as u64 + 1, total, elapsed: started.elapsed() };
        if !on_progress(p) {
            return Err(Cancelled.into()); // `writer` is dropped here and deletes the partial file
        }
    }

    let path = writer.finish()?;
    let hardware = reelforge_media::is_hardware_encoder(&video_codec);
    Ok(Report {
        path,
        hardware,
        frames: total,
        width: settings.width & !1,
        height: settings.height & !1,
        video_codec,
        has_audio,
        elapsed: started.elapsed(),
    })
}

