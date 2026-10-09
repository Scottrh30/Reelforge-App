//! Offline export: renders a sequence frame by frame through the same GPU
//! compositor the viewer uses, mixes its audio with the same mixer the
//! speakers hear, and encodes both with FFmpeg.
//!
//! Unlike the viewer, export never shows a "close enough" frame: it waits for
//! every layer's exact frame at full resolution. It runs on whatever thread
//! calls it (the app uses a background thread) and reports progress through a
//! callback that can also cancel.

use anyhow::{anyhow, Context as _, Result};
use reelforge_core::{ClipId, FrameRate, Project, Time, TrackKind};
use reelforge_media::{FrameSource, MixClip, Mixer, MovieWriter, Quality, WriterSettings};
use reelforge_render::{wgpu, Compositor, LayerInput};
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Audio is always exported at 48 kHz, the video standard.
pub const AUDIO_RATE: u32 = 48_000;
/// How long to wait for a decoder to produce a frame before giving up.
const FRAME_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Debug)]
pub struct ExportSettings {
    pub path: PathBuf,
    /// Output size; the sequence is rendered at its own size and scaled to this.
    pub width: u32,
    pub height: u32,
    pub quality: Quality,
    pub include_audio: bool,
    /// Range of the sequence to export, `[start, end)`.
    pub start: Time,
    pub end: Time,
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

/// The audio clips of sequence 0 as the mixer wants them, at `rate`.
pub fn mix_clips(project: &Project, rate: u32) -> Vec<MixClip> {
    let sr = FrameRate::new(rate as i64, 1);
    project.sequences[0]
        .tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Audio && !t.muted)
        .flat_map(|t| &t.clips)
        .filter_map(|c| {
            let m = project.media(c.media)?;
            Some(MixClip {
                id: c.id,
                path: m.path.clone(),
                start: c.timeline_start.to_frame(sr),
                source_in: c.source_in.to_frame(sr),
                len: c.duration.to_frame(sr),
                gain: 1.0,
            })
        })
        .collect()
}

/// Render sequence 0 of `project` to a movie file.
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
    let seq = project.sequences.first().ok_or_else(|| anyhow!("project has no sequence"))?;
    let rate = seq.frame_rate;
    let frame_size = [seq.width.max(2), seq.height.max(2)];
    let (first, last) = frame_range(rate, settings.start, settings.end);
    let total = (last - first) as u64;
    if total == 0 {
        return Err(anyhow!("nothing to export: the range is empty"));
    }

    let audio_clips = if settings.include_audio { mix_clips(project, AUDIO_RATE) } else { Vec::new() };
    let has_audio = !audio_clips.is_empty();
    let mut mixer = Mixer::new(AUDIO_RATE);
    mixer.set_clips(audio_clips);
    let sr = FrameRate::new(AUDIO_RATE as i64, 1);

    let mut writer = MovieWriter::create(&settings.path, &WriterSettings {
        width: settings.width,
        height: settings.height,
        input_size: Some(frame_size),
        frame_rate: rate,
        quality: settings.quality,
        audio_rate: has_audio.then_some(AUDIO_RATE),
    })?;
    let video_codec = writer.video_codec.clone();

    let mut compositor = Compositor::new(device, queue);
    let mut sources: HashMap<ClipId, FrameSource> = HashMap::new();
    let mut audio_pos = Time::from_frame(first, rate).to_frame(sr);
    let mut audio_buf = Vec::new();

    for (n, i) in (first..last).enumerate() {
        let t = Time::from_frame(i, rate);

        // Exact frame for every visible layer.
        let mut frames = Vec::new();
        for (_, clip) in seq.video_layers_at(t) {
            let Some(media) = project.media(clip.media) else { continue };
            let Some(v) = &media.info.video else { continue };
            let src = match sources.entry(clip.id) {
                std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
                std::collections::hash_map::Entry::Vacant(e) => e.insert(
                    FrameSource::open(&media.path, None).with_context(|| format!("opening {}", media.name))?,
                ),
            };
            let frame = src
                .frame_at_blocking(clip.source_time(t), FRAME_TIMEOUT)
                .ok_or_else(|| anyhow!("{} produced no frame at {}", media.name, clip.source_time(t)))?;
            frames.push((clip.id, media.id, [v.width as f32, v.height as f32], clip.transform, clip.blend, frame));
        }
        let inputs: Vec<LayerInput> = frames
            .iter()
            .map(|(id, media, size, transform, blend, f)| {
                let mut h = DefaultHasher::new();
                (media, f.pts, f.generation).hash(&mut h);
                LayerInput {
                    key: id.0,
                    content_id: h.finish(),
                    pixels: &f.rgba,
                    pixel_size: [f.width, f.height],
                    layer_size: *size,
                    transform: *transform,
                    blend: *blend,
                }
            })
            .collect();
        compositor.render(frame_size, &inputs);
        let rgba = compositor.read_output().ok_or_else(|| anyhow!("GPU readback failed"))?;
        writer.write_video(&rgba)?;

        // Exactly the audio that belongs to this frame, so A/V never drift apart.
        if has_audio {
            let next = Time::from_frame(i + 1, rate).to_frame(sr);
            audio_buf.resize((next - audio_pos).max(0) as usize * 2, 0.0);
            mixer.render(audio_pos, &mut audio_buf);
            writer.write_audio(&audio_buf)?;
            audio_pos = next;
        }

        // Close decoders for clips that are finished.
        sources.retain(|id, _| seq.find_clip(*id).is_some_and(|(_, c)| c.timeline_end() > t));
        compositor.retain_layers(|k| sources.contains_key(&ClipId(k)));

        let p = Progress { done: n as u64 + 1, total, elapsed: started.elapsed() };
        if !on_progress(p) {
            return Err(Cancelled.into()); // `writer` is dropped here and deletes the partial file
        }
    }

    let path = writer.finish()?;
    Ok(Report {
        path,
        frames: total,
        width: settings.width & !1,
        height: settings.height & !1,
        video_codec,
        has_audio,
        elapsed: started.elapsed(),
    })
}
