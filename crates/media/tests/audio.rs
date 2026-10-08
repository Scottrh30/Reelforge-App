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
    mixer.set_clips(vec![MixClip { id: ClipId(1), path: path.clone(), start: r, source_in: r / 2, len: 2 * r, gain: 1.0 }]);

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
        MixClip { id: ClipId(1), path: path.clone(), start: 0, source_in: 0, len: r, gain: 0.5 },
        MixClip { id: ClipId(2), path, start: 0, source_in: 0, len: r, gain: 0.5 },
    ]);
    let mut a = vec![0.0; 4096 * 2];
    mixer.render(r / 2, &mut a);
    let mut solo = Mixer::new(RATE);
    solo.set_clips(vec![MixClip { id: ClipId(9), path: make_clip().unwrap(), start: 0, source_in: 0, len: r, gain: 1.0 }]);
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
