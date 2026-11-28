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
fn mixer_follows_pan_keyframes_and_meters_each_track() {
    let Some(path) = make_clip() else { return };
    let r = RATE as i64;
    let mut m = Mixer::new(RATE);
    // Track 2: hard left at 0 s sweeping to hard right at 2 s. Track 4: centred, half volume.
    m.set_clips(vec![
        MixClip { pan_keys: vec![(0, -1.0), (2 * r, 1.0)], track: 2, ..MixClip::plain(ClipId(1), path.clone(), 0, 0, 3 * r) },
        MixClip { gain: 0.5, track: 4, ..MixClip::plain(ClipId(2), path, 0, 0, 3 * r) },
    ]);
    let mut block = vec![0.0; 1024 * 2];
    m.render(0, &mut block);
    let p = *m.track_peaks();
    assert!(p[2][0] > 0.1 && p[2][1] < 0.01, "track 2 starts hard left: {:?}", p[2]);
    assert!((p[4][0] - p[4][1]).abs() < 1e-4 && p[4][0] > 0.05, "track 4 centred: {:?}", p[4]);
    assert!(p[4][0] < p[2][0], "track 4 is at half volume");
    assert_eq!(p[0], [0.0, 0.0], "an empty track reads silent");
    m.render(r, &mut block); // 1 s: centred
    let p = *m.track_peaks();
    assert!((p[2][0] - p[2][1]).abs() < 0.01, "centred halfway through the sweep: {:?}", p[2]);
    m.render(2 * r + 100, &mut block); // past the last key: held hard right
    let p = *m.track_peaks();
    assert!(p[2][0] < 0.01 && p[2][1] > 0.1, "held hard right: {:?}", p[2]);
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

#[test]
fn varispeed_slows_reverses_and_pitches() {
    let Some(path) = make_clip() else { return };
    let r = RATE as i64;
    let render = |c: MixClip, from: f64, to: f64| {
        let mut m = Mixer::new(RATE);
        m.set_clips(vec![c]);
        let mut buf = vec![0.0f32; ((to - from) * RATE as f64) as usize * 2];
        m.render((from * RATE as f64) as i64, &mut buf);
        buf
    };
    // Half speed: an octave down (220 Hz), and 2 s of timeline cover 1 s of media.
    let slow = MixClip { rate: 0.5, ..MixClip::plain(ClipId(1), path.clone(), 0, 0, 2 * r) };
    let f = freq(&render(slow, 0.5, 1.5));
    assert!((f - 220.0).abs() < 6.0, "{f}");
    // Double speed: an octave up.
    let fast = MixClip { rate: 2.0, ..MixClip::plain(ClipId(1), path.clone(), 0, 0, r) };
    let f = freq(&render(fast, 0.2, 0.8));
    assert!((f - 880.0).abs() < 15.0, "{f}");
    // Reversed from 3.5 s back towards 0.5 s: same pitch, and the tone is there.
    let back = MixClip { rate: -1.0, ..MixClip::plain(ClipId(1), path.clone(), 0, 3 * r + r / 2, 3 * r) };
    let b = render(back, 0.5, 1.5);
    assert!((freq(&b) - 440.0).abs() < 6.0 && rms(&b) > 0.05);
    // A freeze frame is silent.
    let frozen = MixClip { rate: 0.0, ..MixClip::plain(ClipId(1), path, 0, r, r) };
    assert_eq!(rms(&render(frozen, 0.0, 1.0)), 0.0);
}

/// Clip effects in the mix: in sync (a limiter's look-ahead is made up for),
/// the same in any block size, after a jump the same as played through, and
/// keyframes followed.
#[test]
fn mixer_runs_clip_effects_in_sync() {
    use reelforge_core::audiofx::{comp, limit, AudioEffect, AudioFxKind};
    use reelforge_core::anim::{Curve, Interp, Key};
    use reelforge_core::Time;
    let Some(path) = make_clip() else { return };
    let r = RATE as i64;
    let base = MixClip::plain(ClipId(1), path.clone(), 0, 0, 3 * r);
    let render = |clip: &MixClip, block: usize, from: f64, to: f64| {
        let mut m = Mixer::new(RATE);
        m.set_clips(vec![clip.clone()]);
        let (a, b) = ((from * RATE as f64) as i64, (to * RATE as f64) as i64);
        let mut out = vec![0.0; (b - a) as usize * 2];
        for (i, chunk) in out.chunks_mut(block * 2).enumerate() {
            m.render(a + (i * block) as i64, chunk);
        }
        out
    };
    let max_diff = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(x, y)| (x - y).abs()).fold(0.0f32, f32::max);

    // A limiter that never has to act (ceiling 0 dB, the tone peaks at −18):
    // exactly the plain sound, not 5 ms late.
    let mut lim = AudioEffect::new(AudioFxKind::Limiter);
    lim.params[limit::CEILING].value = 0.0;
    let limited = MixClip { effects: vec![lim], ..base.clone() };
    let plain = render(&base, 1024, 0.5, 1.5);
    let d = max_diff(&render(&limited, 1024, 0.5, 1.5), &plain);
    assert!(d < 1e-4, "limiter at rest changes nothing: {d}");

    // A compressor: the same whatever the block size, and starting in the middle
    // (a jump) matches having played from the start once it has settled.
    let mut c = AudioEffect::new(AudioFxKind::Compressor);
    c.params[comp::THRESHOLD].value = -30.0;
    c.params[comp::RATIO].value = 8.0;
    let squashed = MixClip { effects: vec![c.clone()], ..base.clone() };
    let whole = render(&squashed, 4800, 0.0, 3.0);
    let small = render(&squashed, 333, 0.0, 3.0);
    assert!(max_diff(&whole, &small) < 1e-5, "block size doesn't matter");
    let jumped = render(&squashed, 1024, 2.0, 2.5);
    let d = max_diff(&jumped, &whole[2 * r as usize * 2..(2.5 * RATE as f64) as usize * 2]);
    assert!(d < 1e-3, "after a jump: {d}");
    let level = |b: &[f32]| rms(&b[(RATE as usize) * 2..(2 * RATE as usize) * 2]);
    let k = 20.0 * (level(&whole) / level(&render(&base, 4800, 0.0, 3.0))).log10();
    assert!(k < -6.0, "compressed: {k:.1} dB");

    // Keyframed: the threshold rises out of the way over the clip, so the end is
    // louder than the start.
    let mut keyed = c;
    keyed.params[comp::THRESHOLD].keys = Curve(vec![Key::new(Time::ZERO, -40.0, Interp::Linear), Key::new(Time::from_secs_f64(3.0), 0.0, Interp::Linear)]);
    let out = render(&MixClip { effects: vec![keyed], ..base.clone() }, 1024, 0.0, 3.0);
    let at = |s: f64| rms(&out[(s * RATE as f64) as usize * 2..((s + 0.2) * RATE as f64) as usize * 2]);
    assert!(at(2.6) > at(0.4) * 2.0, "{} vs {}", at(2.6), at(0.4));
}

#[test]
fn waveform_detail_has_each_sides_peak_and_loudness() {
    // A 440 Hz sine on the left only, at half level, for 2 s (stereo WAV).
    let path = std::env::temp_dir().join(format!("reelforge_left_tone_{}.wav", std::process::id()));
    let ok = Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=2"])
        .args(["-af", "volume=0.5,pan=stereo|c0=c0|c1=0*c0", "-c:a", "pcm_s16le"])
        .arg(&path)
        .status()
        .is_ok_and(|s| s.success());
    if !ok {
        eprintln!("ffmpeg CLI not found; skipping");
        return;
    }
    let (peaks, detail) = reelforge_media::waveform_detail(&path, 100).unwrap();
    let _ = std::fs::remove_file(&path);
    assert_eq!(detail.len(), peaks.len() * 4);
    assert!((195..=205).contains(&peaks.len()), "{} buckets for 2 s", peaks.len());
    let level = |b: u8| (b as f32 / 255.0).powi(2);
    let mid = &detail[100 * 4..100 * 4 + 4];
    // Left: peak ½ (ffmpeg's sine is at ⅛ of full scale, then halved: 1/16), RMS peak/√2.
    let (pl, pr, rl, rr) = (level(mid[0]), level(mid[1]), level(mid[2]), level(mid[3]));
    assert!(pl > 0.04 && pl < 0.09, "left peak {pl}");
    assert!((rl / pl - std::f32::consts::FRAC_1_SQRT_2).abs() < 0.08, "left RMS {rl} of peak {pl}");
    assert!(pr < 0.002 && rr < 0.002, "right is silent: {pr} {rr}");
    assert!((peaks[100] - pl).abs() < 0.01, "the combined peak is the louder side's");
}
