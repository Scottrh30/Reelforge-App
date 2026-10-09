//! Audio reader and mixer tests against a generated clip with a 440 Hz tone.

use reelforge_core::ClipId;
use reelforge_media::{waveform_peaks, AudioReader, MixClip, Mixer};
use std::path::PathBuf;
use std::process::Command;

const RATE: u32 = 48_000;

fn make_clip() -> Option<PathBuf> {
    let path = std::env::temp_dir().join("reelforge_test_tone.mp4");
    if path.exists() {
        return Some(path);
    }
    // 4 s of video + 440 Hz tone at 44.1 kHz mono AAC (so resampling and upmixing are exercised).
    let status = Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i", "testsrc2=size=320x180:rate=25:duration=4"])
        .args(["-f", "lavfi", "-i", "sine=frequency=440:sample_rate=44100:duration=4"])
        .args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac", "-ac", "1", "-shortest"])
        .arg(&path)
        .status()
        .ok()?;
    status.success().then_some(path)
}

fn rms(s: &[f32]) -> f32 {
    (s.iter().map(|v| v * v).sum::<f32>() / s.len().max(1) as f32).sqrt()
}

/// Frequency estimate from zero crossings of the left channel.
fn freq(stereo: &[f32]) -> f32 {
    let left: Vec<f32> = stereo.chunks(2).map(|c| c[0]).collect();
    let crossings = left.windows(2).filter(|w| (w[0] < 0.0) != (w[1] < 0.0)).count();
    crossings as f32 / 2.0 / (left.len() as f32 / RATE as f32)
}

#[test]
fn reader_produces_tone_and_seeks_consistently() {
    let Some(path) = make_clip() else { eprintln!("ffmpeg CLI not found; skipping"); return };
    let mut seq = AudioReader::open(&path, RATE).unwrap();

    // Read the first 3 s sequentially in 1024-frame blocks.
    let block = 1024;
    let mut all = Vec::new();
    let mut buf = vec![0.0; block * 2];
    let mut at = 0;
    while at < 3 * RATE as i64 {
        seq.read(at, &mut buf);
        all.extend_from_slice(&buf);
        at += block as i64;
    }
    let f = freq(&all[(RATE as usize / 2) * 2..]);
    assert!((f - 440.0).abs() < 3.0, "tone is {f} Hz");
    assert!(rms(&all) > 0.05, "too quiet: {}", rms(&all));

    // A fresh reader jumping straight to 2.25 s must match the sequential samples.
    let target = (2.25 * RATE as f64) as usize;
    let mut jump = AudioReader::open(&path, RATE).unwrap();
    let mut out = vec![0.0; 2048 * 2];
    jump.read(target as i64, &mut out);
    let expect = &all[target * 2..target * 2 + out.len()];
    let err = out.iter().zip(expect).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
    assert!(err < 0.02, "seek mismatch, max error {err}");

    // Backwards seek on the same reader also matches.
    seq.read(target as i64, &mut out);
    let err = out.iter().zip(expect).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
    assert!(err < 0.02, "backward seek mismatch, max error {err}");
}

#[test]
fn mixer_places_clips_on_timeline() {
    let Some(path) = make_clip() else { return };
    let mut mixer = Mixer::new(RATE);
    let r = RATE as i64;
    // Clip plays source 0.5 s..2.5 s at timeline 1 s..3 s.
    mixer.set_clips(vec![MixClip::plain(ClipId(1), path.clone(), r, r / 2, 2 * r)]);

    let mut out = vec![0.0; 4 * RATE as usize * 2];
    // Render in blocks like the engine does.
    for (i, chunk) in out.chunks_mut(512 * 2).enumerate() {
        mixer.render(i as i64 * 512, chunk);
    }
    let sec = |a: f64, b: f64| &out[(a * RATE as f64) as usize * 2..(b * RATE as f64) as usize * 2];
    assert!(rms(sec(0.0, 0.99)) < 1e-4, "should be silent before the clip");
    assert!(rms(sec(1.01, 2.99)) > 0.05, "should play during the clip");
    assert!(rms(sec(3.01, 4.0)) < 1e-4, "should be silent after the clip");
    let f = freq(sec(1.1, 2.9));
    assert!((f - 440.0).abs() < 3.0, "tone is {f} Hz");

    // Gain 0 and a second overlapping clip sum correctly.
    mixer.set_clips(vec![
        MixClip { gain: 0.5, ..MixClip::plain(ClipId(1), path.clone(), 0, 0, r) },
        MixClip { gain: 0.5, ..MixClip::plain(ClipId(2), path, 0, 0, r) },
    ]);
    let mut a = vec![0.0; 4096 * 2];
    mixer.render(r / 2, &mut a);
    let mut solo = Mixer::new(RATE);
    solo.set_clips(vec![MixClip::plain(ClipId(9), make_clip().unwrap(), 0, 0, r)]);
    let mut b = vec![0.0; 4096 * 2];
    solo.render(r / 2, &mut b);
    let err = a.iter().zip(&b).map(|(x, y)| (x - y).abs()).fold(0.0f32, f32::max);
    assert!(err < 1e-3, "two half-gain copies should equal one full-gain copy (err {err})");
}

#[test]
fn waveform_has_expected_length_and_level() {
    let Some(path) = make_clip() else { return };
    let peaks = waveform_peaks(&path, 50).unwrap();
    assert!((peaks.len() as i32 - 200).abs() <= 3, "{} buckets for 4 s at 50/s", peaks.len());
    assert!(peaks[100] > 0.05);
}

#[test]
fn mixer_applies_gain_balance_and_fades() {
    let Some(path) = make_clip() else { return };
    let r = RATE as i64;
    let render = |clip: MixClip, from: f64, to: f64| {
        let mut m = Mixer::new(RATE);
        m.set_clips(vec![clip]);
        let mut out = vec![0.0; ((to - from) * RATE as f64) as usize * 2];
        m.render((from * RATE as f64) as i64, &mut out);
        out
    };
    let base = || MixClip::plain(ClipId(1), path.clone(), 0, 0, 3 * r);
    let left = |s: &[f32]| s.chunks(2).map(|c| c[0]).collect::<Vec<_>>();
    let right = |s: &[f32]| s.chunks(2).map(|c| c[1]).collect::<Vec<_>>();

    let unity = rms(&render(base(), 1.0, 1.5));
    let half = rms(&render(MixClip { gain: 0.5, ..base() }, 1.0, 1.5));
    assert!((half / unity - 0.5).abs() < 0.01, "gain 0.5 halves the level ({half} vs {unity})");

    let panned = render(MixClip { balance: [1.0, 0.0], ..base() }, 1.0, 1.5);
    assert!(rms(&right(&panned)) < 1e-6, "hard left: right channel silent");
    assert!((rms(&left(&panned)) - unity).abs() < 0.01, "left channel untouched");

    // 1 s fade-in and 1 s fade-out on a 3 s clip.
    let faded = MixClip { fade_in: r, fade_out: r, ..base() };
    let lvl = |a: f64, b: f64| rms(&render(faded.clone(), a, b));
    assert!(lvl(0.0, 0.1) < 0.2 * unity, "starts near silence");
    assert!(lvl(0.0, 0.1) < lvl(0.45, 0.55) && lvl(0.45, 0.55) < lvl(0.9, 1.0), "rises");
    assert!((lvl(1.2, 1.8) - unity).abs() < 0.01, "full level in the middle");
    assert!(lvl(2.9, 3.0) < 0.2 * unity, "ends near silence");
}

#[test]
fn mono_sources_play_at_full_level_on_both_sides() {
    let Some(path) = make_clip() else { return }; // mono 440 Hz, amplitude 1/8 (lavfi sine default)
    let mut r = AudioReader::open(&path, RATE).unwrap();
    let mut buf = vec![0.0f32; 48_000 * 2];
    r.read(RATE as i64, &mut buf);
    let l: Vec<f32> = buf.chunks(2).map(|c| c[0]).collect();
    let rr: Vec<f32> = buf.chunks(2).map(|c| c[1]).collect();
    assert_eq!(l, rr, "both channels identical");
    let want = 0.125 / 2f32.sqrt();
    assert!((rms(&l) - want).abs() < 0.01, "rms {} want {want}", rms(&l));
}

#[test]
fn mixer_follows_volume_keyframes() {
    let Some(path) = make_clip() else { return };
    let r = RATE as i64;
    let mut m = Mixer::new(RATE);
    // 0 dB at 0 s ramping to -20 dB at 1 s, then held.
    m.set_clips(vec![MixClip { volume_keys: vec![(0, 0.0), (r, -20.0)], ..MixClip::plain(ClipId(1), path, 0, 0, 3 * r) }]);
    let mut out = vec![0.0; 3 * RATE as usize * 2];
    for (i, chunk) in out.chunks_mut(1024 * 2).enumerate() {
        m.render(i as i64 * 1024, chunk);
    }
    let level = |a: f64, b: f64| rms(&out[(a * RATE as f64) as usize * 2..(b * RATE as f64) as usize * 2]);
    let db = |x: f32| 20.0 * (x / (0.125 / 2f32.sqrt())).log10(); // relative to the unprocessed tone
    assert!((db(level(0.475, 0.525)) - -10.0).abs() < 1.0, "halfway in dB at 0.5 s: {}", db(level(0.475, 0.525)));
    assert!((db(level(2.0, 2.5)) - -20.0).abs() < 0.5, "held at the last key: {}", db(level(2.0, 2.5)));
}

#[test]
fn wav_without_channel_mask_is_not_silent() {
    // ffmpeg writes mono WAVs from lavfi with an unknown channel layout.
    let path = std::env::temp_dir().join("reelforge_nomask.wav");
    let ok = Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=2"])
        .arg(&path)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        return;
    }
    let mut r = AudioReader::open(&path, RATE).unwrap();
    let mut buf = vec![0.0f32; 4800 * 2];
    r.read(24_000, &mut buf);
    assert!((rms(&buf) - 0.125 / 2f32.sqrt()).abs() < 0.005, "rms {}", rms(&buf));
}
