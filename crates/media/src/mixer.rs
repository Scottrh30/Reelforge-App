//! Timeline audio mixer: renders any range of the sequence to interleaved stereo.
//!
//! Positions are in output-rate samples on the sequence timeline. Rendering is
//! pull-based and stateless apart from the per-clip readers, so the same code
//! serves realtime playback (via the engine thread) and offline export.

use crate::audio::{AudioReader, CHANNELS};
use reelforge_core::ClipId;
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq)]
pub struct MixClip {
    pub id: ClipId,
    pub path: PathBuf,
    /// Timeline position, in samples.
    pub start: i64,
    /// Offset into the source, in samples.
    pub source_in: i64,
    pub len: i64,
    pub gain: f32,
}

pub struct Mixer {
    rate: u32,
    clips: Vec<MixClip>,
    readers: HashMap<ClipId, Option<AudioReader>>,
    scratch: Vec<f32>,
}

impl Mixer {
    pub fn new(rate: u32) -> Self {
        Self { rate, clips: Vec::new(), readers: HashMap::new(), scratch: Vec::new() }
    }

    pub fn rate(&self) -> u32 {
        self.rate
    }

    /// Replace the clip list (after an edit). Readers for clips that still exist are kept.
    pub fn set_clips(&mut self, clips: Vec<MixClip>) {
        self.readers.retain(|id, _| clips.iter().any(|c| c.id == *id));
        // A clip whose media changed needs a fresh reader.
        for c in &clips {
            if let Some(old) = self.clips.iter().find(|o| o.id == c.id) {
                if old.path != c.path {
                    self.readers.remove(&c.id);
                }
            }
        }
        self.clips = clips;
    }

    /// Render `out.len() / 2` stereo frames starting at timeline sample `pos`.
    pub fn render(&mut self, pos: i64, out: &mut [f32]) {
        out.fill(0.0);
        let frames = (out.len() / CHANNELS) as i64;
        let end = pos + frames;
        for clip in &self.clips {
            let (s, e) = (clip.start.max(pos), (clip.start + clip.len).min(end));
            if s >= e {
                continue;
            }
            let reader = self.readers.entry(clip.id).or_insert_with(|| match AudioReader::open(&clip.path, self.rate) {
                Ok(r) => Some(r),
                Err(e) => {
                    log::warn!("no audio for {}: {e:#}", clip.path.display());
                    None
                }
            });
            let Some(reader) = reader else { continue };
            let n = (e - s) as usize * CHANNELS;
            self.scratch.resize(n, 0.0);
            reader.read(clip.source_in + (s - clip.start), &mut self.scratch);
            let dst = &mut out[(s - pos) as usize * CHANNELS..][..n];
            for (d, v) in dst.iter_mut().zip(&self.scratch) {
                *d += v * clip.gain;
            }
        }
        for v in out.iter_mut() {
            *v = v.clamp(-1.0, 1.0);
        }
    }
}
