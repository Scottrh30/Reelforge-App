//! Timeline audio mixer: renders any range of the sequence to interleaved stereo.
//!
//! Positions are in output-rate samples on the sequence timeline. Rendering is
//! pull-based and stateless apart from the per-clip readers, so the same code
//! serves realtime playback (via the engine thread) and offline export.

use crate::audio::{AudioReader, CHANNELS};
use reelforge_core::audio::{balance, db_to_gain, interpolate, interpolate_db};
use reelforge_core::{ClipAudio, ClipId};
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
    /// Linear gain (clip volume × track volume).
    pub gain: f32,
    /// Per-channel multipliers from clip pan × track pan (see `reelforge_core::audio::balance`).
    pub balance: [f32; 2],
    /// Fade lengths, in samples (equal-power; also used for crossfades).
    pub fade_in: i64,
    pub fade_out: i64,
    /// Volume automation as `(offset from start, dB)`, sorted. When non-empty,
    /// the clip's level follows it (multiplied by `gain`, which then carries
    /// only the track volume).
    pub volume_keys: Vec<(i64, f32)>,
    /// Media samples per timeline sample: 1 normally, 0.5 for half speed,
    /// negative when reversed, 0 for a freeze frame (silent). `source_in` is
    /// the media sample heard at `start`.
    pub rate: f64,
    /// Pan automation as `(offset from start, pan -1…+1)`, sorted. When
    /// non-empty, `balance` carries only the track's (and any enclosing
    /// composition's) pan and the clip's own pan follows these keys.
    pub pan_keys: Vec<(i64, f32)>,
    /// Which track of the sequence being played this clip is heard on (for the
    /// per-track meters). Clips inside a composition report the track the
    /// composition sits on.
    pub track: usize,
}

/// Tracks that get a level meter (later ones still play, unmetered).
pub const MAX_TRACK_METERS: usize = 32;

/// Peak level (linear, per channel) of each track's contribution to a block.
pub type TrackPeaks = [[f32; 2]; MAX_TRACK_METERS];

impl MixClip {
    /// A clip at unity gain, centred, without fades.
    pub fn plain(id: ClipId, path: PathBuf, start: i64, source_in: i64, len: i64) -> Self {
        Self { id, path, start, source_in, len, gain: 1.0, balance: [1.0, 1.0], fade_in: 0, fade_out: 0, volume_keys: Vec::new(), rate: 1.0, pan_keys: Vec::new(), track: 0 }
    }
}

pub struct Mixer {
    rate: u32,
    clips: Vec<MixClip>,
    readers: HashMap<ClipId, Option<AudioReader>>,
    scratch: Vec<f32>,
    /// Media read for a clip played at another speed, before resampling.
    span: Vec<f32>,
    /// Each track's peak level in the last block rendered.
    track_peaks: TrackPeaks,
}

impl Mixer {
    pub fn new(rate: u32) -> Self {
        Self { rate, clips: Vec::new(), readers: HashMap::new(), scratch: Vec::new(), span: Vec::new(), track_peaks: [[0.0; 2]; MAX_TRACK_METERS] }
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

    /// Peak level of each track in the last [`render`](Self::render), after its
    /// volume and pan (before the master limiter).
    pub fn track_peaks(&self) -> &TrackPeaks {
        &self.track_peaks
    }

    /// Render `out.len() / 2` stereo frames starting at timeline sample `pos`.
    pub fn render(&mut self, pos: i64, out: &mut [f32]) {
        out.fill(0.0);
        self.track_peaks = [[0.0; 2]; MAX_TRACK_METERS];
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
            if clip.rate == 0.0 {
                continue; // a freeze frame has no sound
            }
            let n = (e - s) as usize * CHANNELS;
            self.scratch.resize(n, 0.0);
            if clip.rate == 1.0 {
                reader.read(clip.source_in + (s - clip.start), &mut self.scratch);
            } else {
                // Varispeed, like tape: read the stretch of media this block covers
                // and resample it linearly (backwards when the rate is negative).
                let m = |k: i64| clip.source_in as f64 + (s - clip.start + k) as f64 * clip.rate;
                let (m0, m1) = (m(0), m(e - s));
                let lo = m0.min(m1).floor() as i64 - 1;
                let hi = m0.max(m1).ceil() as i64 + 2;
                self.span.resize((hi - lo) as usize * CHANNELS, 0.0);
                reader.read(lo, &mut self.span);
                for k in 0..(e - s) {
                    let x = (m(k) - lo as f64).max(0.0);
                    let i = (x as usize).min((hi - lo) as usize - 2);
                    let f = (x - i as f64) as f32;
                    for ch in 0..CHANNELS {
                        let (a, b) = (self.span[i * CHANNELS + ch], self.span[(i + 1) * CHANNELS + ch]);
                        self.scratch[k as usize * CHANNELS + ch] = a + (b - a) * f;
                    }
                }
            }
            let dst = &mut out[(s - pos) as usize * CHANNELS..][..n];
            let [bl, br] = clip.balance;
            let fading = clip.fade_in > 0 || clip.fade_out > 0;
            let animated = !clip.volume_keys.is_empty();
            let panning = !clip.pan_keys.is_empty();
            let mut peak = [0.0f32; 2];
            for (k, (d, v)) in dst.chunks_exact_mut(CHANNELS).zip(self.scratch.chunks_exact(CHANNELS)).enumerate() {
                let mut g = clip.gain;
                let offset = s - clip.start + k as i64;
                if fading {
                    g *= ClipAudio::fade_at(offset, clip.len, clip.fade_in, clip.fade_out);
                }
                if animated {
                    g *= db_to_gain(interpolate_db(&clip.volume_keys, offset));
                }
                let (mut l, mut r) = (bl, br);
                if panning {
                    let [pl, pr] = balance(interpolate(&clip.pan_keys, offset));
                    (l, r) = (l * pl, r * pr);
                }
                let (a, b) = (v[0] * g * l, v[1] * g * r);
                d[0] += a;
                d[1] += b;
                peak = [peak[0].max(a.abs()), peak[1].max(b.abs())];
            }
            if let Some(t) = self.track_peaks.get_mut(clip.track) {
                // Clips on one track don't overlap, except across a crossfade, where
                // the louder side is a fair reading.
                *t = [t[0].max(peak[0]), t[1].max(peak[1])];
            }
        }
        for v in out.iter_mut() {
            *v = v.clamp(-1.0, 1.0);
        }
    }
}
