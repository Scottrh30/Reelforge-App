//! Audio spectrum and waveform visualiser: the sound at a moment, as bars,
//! a line, dots or a waveform drawn along a line across the layer. Which
//! sound: the clip's own (or its linked audio), or whatever plays on one of
//! the audio tracks. The samples come from the media reader, which the app
//! and export hand over (see [`set_audio_source`]); here they become a row
//! of band levels (0–1) in a small table the shader draws from.

use crate::effects::{audiovis as p, EffectValues};
use crate::lut::Table;
use crate::project::{Clip, Project, TrackKind};
use crate::time::Time;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

/// Samples read for each moment (a power of two, for the FFT).
pub const WINDOW: usize = 2048;
/// The rate they're read at.
pub const RATE: u32 = 22_050;
/// The most bands drawn.
pub const MAX_BANDS: usize = 256;

/// `n` mono samples at `rate` centred on source time `at` of the file at `path`.
pub type AudioSource = fn(&Path, Time, usize, u32) -> Option<Vec<f32>>;
static SOURCE: OnceLock<AudioSource> = OnceLock::new();

/// Set by whoever can decode audio (the editor, export).
pub fn set_audio_source(f: AudioSource) {
    let _ = SOURCE.set(f);
}

/// The sound an Audio spectrum effect on clip `c` shows at timeline time
/// `t`: the file, the moment in it, and its level (the clip's volume).
pub fn sound_for(project: &Project, seq: usize, c: &Clip, t: Time, source: usize) -> Option<(PathBuf, Time, f32)> {
    let s = project.sequences.get(seq)?;
    let gain = |a: &Clip| 10f32.powf(a.audio.volume_db / 20.0);
    let of = |a: &Clip| -> Option<(PathBuf, Time, f32)> {
        let m = project.media(a.media)?;
        m.info.audio.as_ref()?;
        Some((m.path.clone(), a.media_at(t), gain(a)))
    };
    if source == 0 {
        // Its own sound, else the audio linked to it.
        if let Some(x) = of(c) {
            return Some(x);
        }
        let link = c.link?;
        return s.tracks.iter().filter(|tr| tr.kind == TrackKind::Audio && !tr.muted).flat_map(|tr| &tr.clips).find(|a| a.link == Some(link) && a.contains(t)).and_then(of);
    }
    let tr = s.tracks.iter().filter(|tr| tr.kind == TrackKind::Audio).nth(source - 1)?;
    if tr.muted {
        return None;
    }
    tr.clips.iter().find(|a| a.contains(t)).and_then(of)
}

/// Fill an Audio spectrum effect's table: band levels for the sound at the
/// moment (silence: all 0).
pub fn fill(v: &mut EffectValues, project: &Project, seq: usize, c: &Clip, t: Time) {
    let vals = &v.values;
    let bands = (vals[p::BANDS].round() as usize).clamp(2, MAX_BANDS);
    let samples = SOURCE.get().and_then(|f| {
        let (path, at, gain) = sound_for(project, seq, c, t, vals[p::SOURCE].round().max(0.0) as usize)?;
        let mut s = f(&path, at, WINDOW, RATE)?;
        s.iter_mut().for_each(|x| *x *= gain);
        Some(s)
    });
    let levels = match samples {
        Some(s) if s.len() == WINDOW => {
            if vals[p::DISPLAY].round() as u32 == 3 {
                waveform(&s, bands)
            } else {
                spectrum(&s, bands, vals[p::LOW], vals[p::HIGH], vals[p::SENSITIVITY])
            }
        }
        // Nothing playing: flat (a waveform's middle line).
        _ => vec![if vals[p::DISPLAY].round() as u32 == 3 { 0.5 } else { 0.0 }; bands],
    };
    v.table = Some(Arc::new(table(&levels)));
    v.gpu = [bands as f32, 0.0, 0.0, 0.0];
}

/// Band levels 0–1: the spectrum of `s` from `lo` to `hi` Hz, in bands
/// evenly spaced in pitch, each at its loudest bin; −60 dB is 0, 0 dB full
/// (`sensitivity` dB added).
pub fn spectrum(s: &[f32], bands: usize, lo: f32, hi: f32, sensitivity: f32) -> Vec<f32> {
    let n = s.len();
    let mut re: Vec<f32> = s.iter().enumerate().map(|(i, x)| x * (0.5 - 0.5 * (std::f32::consts::TAU * i as f32 / n as f32).cos())).collect();
    let mut im = vec![0.0f32; n];
    fft(&mut re, &mut im);
    let bin_hz = RATE as f32 / n as f32;
    let (lo, hi) = (lo.clamp(10.0, RATE as f32 / 2.0 - 10.0), hi.clamp(20.0, RATE as f32 / 2.0));
    let (lo, hi) = (lo.min(hi - 10.0), hi.max(lo + 10.0));
    // A full-scale sine through a Hann window peaks at n/4.
    let full = n as f32 / 4.0;
    (0..bands)
        .map(|b| {
            let f0 = lo * (hi / lo).powf(b as f32 / bands as f32);
            let f1 = lo * (hi / lo).powf((b + 1) as f32 / bands as f32);
            let (i0, i1) = ((f0 / bin_hz).floor() as usize, ((f1 / bin_hz).ceil() as usize).max((f0 / bin_hz).floor() as usize + 1));
            let m = (i0..i1.min(n / 2)).map(|i| (re[i] * re[i] + im[i] * im[i]).sqrt()).fold(0.0f32, f32::max);
            let db = 20.0 * (m / full).max(1e-9).log10() + sensitivity;
            ((db + 60.0) / 60.0).clamp(0.0, 1.0)
        })
        .collect()
}

/// The waveform of `s` as `points` levels (0.5 the middle, 0 and 1 full
/// scale down and up): the middle part of the window, each point the
/// sample furthest from silence in its stretch.
pub fn waveform(s: &[f32], points: usize) -> Vec<f32> {
    // About a frame's worth (the middle quarter of the window).
    let (a, b) = (s.len() * 3 / 8, s.len() * 5 / 8);
    let part = &s[a..b];
    let per = (part.len() as f32 / points as f32).max(1.0);
    (0..points)
        .map(|i| {
            let (x0, x1) = ((i as f32 * per) as usize, (((i + 1) as f32 * per) as usize).clamp(1, part.len()));
            let v = part[x0.min(part.len() - 1)..x1.max(x0 + 1).min(part.len())].iter().copied().fold(0.0f32, |m, x| if x.abs() > m.abs() { x } else { m });
            (0.5 + v.clamp(-1.0, 1.0) * 0.5).clamp(0.0, 1.0)
        })
        .collect()
}

/// Levels as a one-row table, 16 bits each (red high, green low).
pub fn table(levels: &[f32]) -> Table {
    let mut rgba = Vec::with_capacity(levels.len() * 4);
    for l in levels {
        let q = (l.clamp(0.0, 1.0) * 65535.0).round() as u32;
        rgba.extend([(q >> 8) as u8, (q & 255) as u8, 0, 255]);
    }
    let mut h = std::collections::hash_map::DefaultHasher::new();
    std::hash::Hash::hash(&rgba, &mut h);
    Table { id: std::hash::Hasher::finish(&h) ^ 0xa0d1_0000, width: levels.len() as u32, height: 1, rgba, cube_size: 0 }
}

/// In-place radix-2 FFT (`re.len()` a power of two).
fn fft(re: &mut [f32], im: &mut [f32]) {
    let n = re.len();
    if n < 2 || !n.is_power_of_two() || im.len() != n {
        return;
    }
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let a = -std::f32::consts::TAU / len as f32;
        for start in (0..n).step_by(len) {
            for k in 0..len / 2 {
                let (wr, wi) = ((a * k as f32).cos(), (a * k as f32).sin());
                let (u, v) = (start + k, start + k + len / 2);
                let (tr, ti) = (re[v] * wr - im[v] * wi, re[v] * wi + im[v] * wr);
                re[v] = re[u] - tr;
                im[v] = im[u] - ti;
                re[u] += tr;
                im[u] += ti;
            }
        }
        len <<= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(hz: f32, amp: f32) -> Vec<f32> {
        (0..WINDOW).map(|i| amp * (std::f32::consts::TAU * hz * i as f32 / RATE as f32).sin()).collect()
    }

    #[test]
    fn a_tone_lights_its_own_band() {
        let bands = 32;
        let (lo, hi) = (40.0, 12000.0);
        let l = spectrum(&tone(1000.0, 0.5), bands, lo, hi, 0.0);
        let peak = l.iter().enumerate().max_by(|a, b| a.1.total_cmp(b.1)).unwrap().0;
        // 1 kHz in 40–12000 Hz, evenly in pitch.
        let want = ((1000.0f32 / lo).ln() / (hi / lo).ln() * bands as f32) as usize;
        assert!(peak.abs_diff(want) <= 1, "peak band {peak}, want {want}: {l:?}");
        assert!((l[peak] - (1.0 - 6.0 / 60.0)).abs() < 0.06, "half scale is −6 dB: {}", l[peak]);
        assert!(l[0] < 0.2 && l[bands - 1] < 0.2, "quiet far away");
        // Louder by sensitivity; silence is 0.
        assert!(spectrum(&tone(1000.0, 0.5), bands, lo, hi, 12.0)[peak] > l[peak]);
        assert!(spectrum(&vec![0.0; WINDOW], bands, lo, hi, 0.0).iter().all(|&v| v == 0.0));
    }

    #[test]
    fn a_waveform_follows_the_samples() {
        let w = waveform(&tone(100.0, 0.8), 64);
        assert_eq!(w.len(), 64);
        assert!(w.iter().all(|v| (0.0..=1.0).contains(v)));
        assert!(w.iter().cloned().fold(0.0, f32::max) > 0.85 && w.iter().cloned().fold(1.0, f32::min) < 0.15, "swings up and down");
        assert!(waveform(&vec![0.0; WINDOW], 16).iter().all(|&v| v == 0.5), "silence: the middle");
    }

    #[test]
    fn the_sound_comes_from_the_clip_or_a_track() {
        use crate::project::{MediaAsset, MediaId, MediaInfo, Sequence};
        let mut proj = Project::new();
        let info: MediaInfo = serde_json::from_value(serde_json::json!({"duration": 254016000i64 * 10, "video": null, "audio": {"sample_rate": 48000, "channels": 2, "codec": "aac"}, "container": "wav"})).unwrap();
        proj.media.push(MediaAsset { id: MediaId(5), path: "/music.wav".into(), name: "music.wav".into(), info, conform: None });
        let mut s = Sequence::new("S", 640, 360, crate::time::FrameRate::new(25, 1));
        let a = s.tracks.iter().position(|t| t.kind == TrackKind::Audio).unwrap();
        let sec = Time::from_secs_f64;
        let mut music = Clip::new(crate::ClipId(9), MediaId(5), sec(1.0), sec(2.0), sec(5.0), None);
        music.audio.volume_db = -6.0;
        s.tracks[a].clips.push(music);
        proj.sequences = vec![s];
        let solid = Clip::new_plane(crate::ClipId(1), Time::ZERO, sec(10.0), [0, 0, 0, 255]);
        // A solid has no sound of its own; "Audio 1" is the music, 2 s in at 3 s.
        assert!(sound_for(&proj, 0, &solid, sec(3.0), 0).is_none());
        let (path, at, gain) = sound_for(&proj, 0, &solid, sec(3.0), 1).unwrap();
        assert_eq!((path, at), (PathBuf::from("/music.wav"), sec(4.0)));
        assert!((gain - 0.501).abs() < 0.01);
        assert!(sound_for(&proj, 0, &solid, sec(0.5), 1).is_none(), "before the music");
    }
}
