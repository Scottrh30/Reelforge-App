//! Timeline audio mixer: renders any range of the sequence to interleaved stereo.
//!
//! Positions are in output-rate samples on the sequence timeline. Rendering is
//! pull-based and stateless apart from the per-clip readers, so the same code
//! serves realtime playback (via the engine thread) and offline export.

use crate::audio::{AudioReader, CHANNELS};
use crate::audiofx::FxChain;
use reelforge_core::audiofx::AudioEffect;
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
    /// Effects on the clip's sound (before gain, fades and pan).
    pub effects: Vec<AudioEffect>,
    /// For the effects' keyframes: the clip's source time at `start`, in
    /// samples, and how many source samples pass per timeline sample.
    pub fx_src0: f64,
    pub fx_scale: f64,
}

/// Tracks that get a level meter (later ones still play, unmetered).
pub const MAX_TRACK_METERS: usize = 32;

/// Peak level (linear, per channel) of each track's contribution to a block.
pub type TrackPeaks = [[f32; 2]; MAX_TRACK_METERS];

impl MixClip {
    /// A clip at unity gain, centred, without fades.
    pub fn plain(id: ClipId, path: PathBuf, start: i64, source_in: i64, len: i64) -> Self {
        Self { id, path, start, source_in, len, gain: 1.0, balance: [1.0, 1.0], fade_in: 0, fade_out: 0, volume_keys: Vec::new(), rate: 1.0, pan_keys: Vec::new(), track: 0, effects: Vec::new(), fx_src0: source_in as f64, fx_scale: 1.0 }
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
    /// Clips' effect chains, and the timeline sample each expects next (any
    /// other start is a jump: the chain starts afresh and warms up first).
    fx: HashMap<ClipId, (FxChain, i64)>,
    /// Effect parameters for one sub-block, per effect.
    fx_params: Vec<Vec<f32>>,
    /// Scrubbing: effects skip their warm-up (snippets come many times a second).
    quick: bool,
}

/// Effects' parameters are looked up this often (frames).
const FX_STEP: usize = 256;

/// The sound of `clip` for timeline samples `s..e` (into `out`, interleaved),
/// before its gain, fades and pan; silence outside the clip.
fn read_source(clip: &MixClip, reader: &mut AudioReader, span: &mut Vec<f32>, s: i64, e: i64, out: &mut [f32]) {
    out.fill(0.0);
    let (a, b) = (s.max(clip.start), e.min(clip.start + clip.len));
    if a >= b || clip.rate == 0.0 {
        return;
    }
    let dst = &mut out[(a - s) as usize * CHANNELS..(b - s) as usize * CHANNELS];
    if clip.rate == 1.0 {
        reader.read(clip.source_in + (a - clip.start), dst);
        return;
    }
    // Varispeed, like tape: read the stretch of media this block covers
    // and resample it linearly (backwards when the rate is negative).
    let m = |k: i64| clip.source_in as f64 + (a - clip.start + k) as f64 * clip.rate;
    let (m0, m1) = (m(0), m(b - a));
    let lo = m0.min(m1).floor() as i64 - 1;
    let hi = m0.max(m1).ceil() as i64 + 2;
    span.resize((hi - lo) as usize * CHANNELS, 0.0);
    reader.read(lo, span);
    for k in 0..(b - a) {
        let x = (m(k) - lo as f64).max(0.0);
        let i = (x as usize).min((hi - lo) as usize - 2);
        let f = (x - i as f64) as f32;
        for ch in 0..CHANNELS {
            let (p, q) = (span[i * CHANNELS + ch], span[(i + 1) * CHANNELS + ch]);
            dst[k as usize * CHANNELS + ch] = p + (q - p) * f;
        }
    }
}

/// Run `buf` (the chain's input for timeline samples from `s`) through
/// `chain`, looking the parameters up every [`FX_STEP`] frames.
fn run_chain(clip: &MixClip, chain: &mut FxChain, params: &mut Vec<Vec<f32>>, rate: u32, s: i64, buf: &mut [f32]) {
    let active: Vec<&AudioEffect> = clip.effects.iter().filter(|e| !e.is_identity()).collect();
    params.resize(active.len(), Vec::new());
    for (k, block) in buf.chunks_mut(FX_STEP * CHANNELS).enumerate() {
        let offset = s + (k * FX_STEP) as i64 - clip.start;
        let src = clip.fx_src0 + offset as f64 * clip.fx_scale;
        let t = reelforge_core::Time::from_secs_f64(src / rate as f64);
        for (e, p) in active.iter().zip(params.iter_mut()) {
            e.values_at(t, p);
        }
        chain.process(block, params);
    }
}

impl Mixer {
    pub fn new(rate: u32) -> Self {
        Self { rate, clips: Vec::new(), readers: HashMap::new(), scratch: Vec::new(), span: Vec::new(), track_peaks: [[0.0; 2]; MAX_TRACK_METERS], fx: HashMap::new(), fx_params: Vec::new(), quick: false }
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
        // Effect chains carry on through an edit when the clip keeps the same
        // effects (a parameter change); otherwise they start afresh.
        self.fx.retain(|id, (chain, _)| clips.iter().any(|c| c.id == *id && fx_kinds(c) == chain.kinds()));
        self.clips = clips;
    }

    /// Render a scrubbing snippet: like [`Self::render`], but effects skip
    /// their warm-up (snippets come many times a second, from anywhere).
    pub fn render_quick(&mut self, pos: i64, out: &mut [f32]) {
        self.quick = true;
        self.render(pos, out);
        self.quick = false;
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
            let kinds = fx_kinds(clip);
            if kinds.is_empty() {
                read_source(clip, reader, &mut self.span, s, e, &mut self.scratch);
            } else {
                // Effects: their chain runs ahead by its latency, so what comes out
                // lines up with the picture; after a jump it starts afresh and
                // hears a little of what came before first (to settle).
                let (chain, next) = self.fx.entry(clip.id).or_insert_with(|| (FxChain::new(&kinds, self.rate), i64::MIN));
                let lat = chain.latency() as i64;
                if *next != s {
                    chain.reset();
                    let warm = if self.quick { 0 } else { chain.settle() as i64 };
                    // The chain is fed timeline sound from `from` on (silence before the
                    // clip starts), `lat` ahead of what comes out.
                    let from = (s - warm).max(clip.start).min(s);
                    let mut pre = vec![0.0f32; (s + lat - from).max(0) as usize * CHANNELS];
                    read_source(clip, reader, &mut self.span, from, s + lat, &mut pre);
                    run_chain(clip, chain, &mut self.fx_params, self.rate, from - lat, &mut pre);
                }
                read_source(clip, reader, &mut self.span, s + lat, e + lat, &mut self.scratch);
                run_chain(clip, chain, &mut self.fx_params, self.rate, s, &mut self.scratch);
                *next = e;
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

/// The effects of `clip` that change its sound, in order.
fn fx_kinds(clip: &MixClip) -> Vec<reelforge_core::audiofx::AudioFxKind> {
    clip.effects.iter().filter(|e| !e.is_identity()).map(|e| e.kind).collect()
}
