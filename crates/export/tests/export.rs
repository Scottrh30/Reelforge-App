//! End-to-end export: build a two-layer project with offset audio, export it,
//! then decode the result and compare it with what the compositor renders.
//! Needs the `ffmpeg` CLI (to make test media) and a GPU adapter; skips otherwise.

use reelforge_core::{BlendMode, Clip, ClipId, FrameRate, MediaAsset, MediaId, Project, Sequence, Time, Transform};
use reelforge_export::{export_sequence, frame_range, Cancelled, ExportSettings};
use reelforge_media::{probe, AudioReader, Fit, FrameSource, Quality, VideoCodec};
use reelforge_render::{headless_device, Compositor, LayerInput};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

fn ffmpeg(name: &str, args: &[&str]) -> Option<PathBuf> {
    let path = std::env::temp_dir().join(name);
    if path.exists() {
        return Some(path);
    }
    let ok = Command::new("ffmpeg").args(["-y", "-loglevel", "error"]).args(args).arg(&path).status().ok()?.success();
    ok.then_some(path)
}

/// 640x360 test pattern with a 1 kHz beep for 0.1 s at the start of every second.
fn clip_a() -> Option<PathBuf> {
    ffmpeg("rf_export_a.mp4", &[
        "-f", "lavfi", "-i", "testsrc2=size=640x360:rate=25:duration=3",
        "-f", "lavfi", "-i", "aevalsrc='if(lt(mod(t,1),0.1),0.5*sin(2*PI*1000*t),0)':s=48000:d=3",
        "-c:v", "libx264", "-g", "25", "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest",
    ])
}

/// 320x180 solid red, no audio.
fn clip_b() -> Option<PathBuf> {
    ffmpeg("rf_export_b.mp4", &["-f", "lavfi", "-i", "color=c=red:s=320x180:r=25:d=2", "-c:v", "libx264", "-pix_fmt", "yuv420p"])
}

fn asset(id: u64, path: &Path) -> MediaAsset {
    MediaAsset { id: MediaId(id), path: path.to_owned(), name: path.file_name().unwrap().to_string_lossy().into(), info: probe(path).unwrap() }
}

const B_TRANSFORM: Transform = Transform { position: [100.0, 0.0], scale: [1.0, 1.0], rotation: 0.0, opacity: 0.5 };

/// V1: A at 0–2 s. V2: B at 1–2 s, offset and half transparent. A1: A's audio starting at 0.5 s.
fn project(a: &Path, b: &Path) -> Project {
    let mut p = Project::new();
    p.media.push(asset(1, a));
    p.media.push(asset(2, b));
    let mut seq = Sequence::new("Test", 640, 360, FrameRate::new(25, 1));
    let s = Time::from_secs_f64;
    seq.tracks[0].clips.push(Clip::new(ClipId(10), MediaId(1), s(0.0), s(0.0), s(2.0), None));
    let mut top = Clip::new(ClipId(11), MediaId(2), s(1.0), s(0.0), s(1.0), None);
    top.transform = B_TRANSFORM;
    seq.tracks[1].clips.push(top);
    seq.tracks[2].clips.push(Clip::new(ClipId(12), MediaId(1), s(0.5), s(0.0), s(1.5), None));
    p.sequences = vec![seq];
    p
}

fn settings(path: PathBuf) -> ExportSettings {
    ExportSettings {
        path,
        width: 640,
        height: 360,
        quality: Quality::High,
        include_audio: true,
        start: Time::ZERO,
        end: Time::from_secs_f64(2.0),
        video: Some(VideoCodec::H264),
        hardware: false,
        fit: Fit::Letterbox,
    }
}

/// What the compositor makes of the project at `t`, rendered independently of the exporter.
fn reference_frame(c: &mut Compositor, a: &Path, b: &Path, t: f64) -> Vec<u8> {
    let wait = Duration::from_secs(5);
    // Distinct per instant, so the compositor's texture cache re-uploads.
    let id = (t * 1000.0) as u64;
    let fa = FrameSource::open(a, None).unwrap().frame_at_blocking(Time::from_secs_f64(t), wait).unwrap();
    let mut layers = vec![LayerInput {
        key: 1, content_id: id, pixels: &fa.rgba, pixel_size: [fa.width, fa.height],
        layer_size: [640.0, 360.0], transform: Transform::default(), blend: BlendMode::Normal, effects: Vec::new(),
    }];
    let fb;
    if t >= 1.0 {
        fb = FrameSource::open(b, None).unwrap().frame_at_blocking(Time::from_secs_f64(t - 1.0), wait).unwrap();
        layers.push(LayerInput {
            key: 2, content_id: id, pixels: &fb.rgba, pixel_size: [fb.width, fb.height],
            layer_size: [320.0, 180.0], transform: B_TRANSFORM, blend: BlendMode::Normal, effects: Vec::new(),
        });
    }
    c.render([640, 360], &layers);
    c.read_output().unwrap()
}

fn mean_abs_diff(x: &[u8], y: &[u8]) -> f64 {
    let (mut sum, mut n) = (0u64, 0u64);
    for (a, b) in x.chunks(4).zip(y.chunks(4)) {
        for k in 0..3 {
            sum += (a[k] as i32 - b[k] as i32).unsigned_abs() as u64;
            n += 1;
        }
    }
    sum as f64 / n as f64
}

fn rms(s: &[f32]) -> f32 {
    (s.iter().map(|v| v * v).sum::<f32>() / s.len().max(1) as f32).sqrt()
}

#[test]
fn frame_range_rounds_partial_frames_up() {
    let r = FrameRate::new(25, 1);
    assert_eq!(frame_range(r, Time::ZERO, Time::from_secs_f64(2.0)), (0, 50));
    assert_eq!(frame_range(r, Time::ZERO, Time::from_secs_f64(2.01)), (0, 51));
    assert_eq!(frame_range(r, Time::from_secs_f64(1.0), Time::from_secs_f64(1.0)), (25, 25));
}

#[test]
fn exported_movie_matches_the_compositor_and_keeps_audio_in_sync() {
    let (Some(a), Some(b)) = (clip_a(), clip_b()) else { eprintln!("no ffmpeg CLI; skipping"); return };
    let Some((device, queue)) = headless_device() else { eprintln!("no GPU; skipping"); return };
    let out = std::env::temp_dir().join("rf_export_out.mp4");
    let p = project(&a, &b);

    let mut calls = 0;
    let report = export_sequence(&p, &settings(out.clone()), &device, &queue, |pr| {
        calls += 1;
        assert_eq!(pr.total, 50);
        true
    })
    .unwrap();
    assert_eq!((report.frames, calls), (50, 50));
    assert!(report.has_audio);
    eprintln!("exported {} frames with {} in {:?}", report.frames, report.video_codec, report.elapsed);

    // Container-level checks.
    let info = probe(&out).unwrap();
    let v = info.video.unwrap();
    assert_eq!((v.width, v.height, v.frame_rate), (640, 360, FrameRate::new(25, 1)));
    assert!((info.duration.as_secs_f64() - 2.0).abs() < 0.05, "duration {}", info.duration);
    assert_eq!(info.audio.unwrap().sample_rate, 48_000);

    // Pictures: exported frames should match an independent composite of the same instant.
    let mut c = Compositor::new(&device, &queue);
    let mut dec = FrameSource::open(&out, None).unwrap();
    let rate = FrameRate::new(25, 1);
    for frame_no in [5, 30, 45] {
        let t = frame_no as f64 / 25.0;
        let want = reference_frame(&mut c, &a, &b, t);
        let got = dec.frame_at_blocking(Time::from_frame(frame_no, rate), Duration::from_secs(5)).unwrap();
        assert_eq!(got.pts.to_frame(rate), frame_no);
        let err = mean_abs_diff(&got.rgba, &want);
        assert!(err < 2.5, "frame {frame_no}: mean error {err:.2} levels");

        // Spot check the overlay: centre of B is at (420, 180).
        let i = (180 * 640 + 420) * 4;
        // 50% red over anything gives at least 0.5 linear red = 188 in sRGB.
        if frame_no >= 25 {
            assert!(got.rgba[i] >= 180, "frame {frame_no}: half-transparent red layer missing ({:?})", &got.rgba[i..i + 3]);
        }
    }

    // Sound: A's audio starts at 0.5 s on the timeline, so its beeps land at 0.5 s and 1.5 s.
    let mut r = AudioReader::open(&out, 48_000).unwrap();
    let mut window = |from: f64, to: f64| {
        let mut buf = vec![0.0f32; ((to - from) * 48_000.0) as usize * 2];
        r.read((from * 48_000.0) as i64, &mut buf);
        rms(&buf)
    };
    assert!(window(0.0, 0.48) < 0.01, "silence before the audio clip");
    assert!(window(0.51, 0.59) > 0.2, "beep at 0.5 s");
    assert!(window(0.65, 1.45) < 0.01, "silence between beeps");
    assert!(window(1.51, 1.59) > 0.2, "beep at 1.5 s");
}

#[test]
fn half_resolution_and_video_only() {
    let (Some(a), Some(b)) = (clip_a(), clip_b()) else { return };
    let Some((device, queue)) = headless_device() else { return };
    let out = std::env::temp_dir().join("rf_export_half.mp4");
    let mut s = settings(out.clone());
    (s.width, s.height, s.include_audio) = (320, 180, false);
    s.start = Time::from_secs_f64(1.0);
    export_sequence(&project(&a, &b), &s, &device, &queue, |_| true).unwrap();
    let info = probe(&out).unwrap();
    let v = info.video.unwrap();
    assert_eq!((v.width, v.height), (320, 180));
    assert!(info.audio.is_none());
    assert!((info.duration.as_secs_f64() - 1.0).abs() < 0.05, "duration {}", info.duration);
}

#[test]
fn cancelling_stops_and_removes_the_file() {
    let (Some(a), Some(b)) = (clip_a(), clip_b()) else { return };
    let Some((device, queue)) = headless_device() else { return };
    let out = std::env::temp_dir().join("rf_export_cancel.mp4");
    let err = export_sequence(&project(&a, &b), &settings(out.clone()), &device, &queue, |p| p.done < 5).unwrap_err();
    assert!(err.is::<Cancelled>(), "{err:#}");
    assert!(!out.exists(), "partial file should be deleted");
}

#[test]
fn exported_audio_follows_volume_pan_and_fades() {
    use reelforge_core::{ClipAudio, TrackAudio};
    let (Some(a), Some(b)) = (clip_a(), clip_b()) else { return };
    let Some((device, queue)) = headless_device() else { return };
    let out = std::env::temp_dir().join("rf_export_levels.mp4");
    let mut p = project(&a, &b);
    {
        let tr = &mut p.sequences[0].tracks[2];
        tr.audio = TrackAudio { volume_db: -6.0206, pan: 1.0 }; // half level, hard right
        // 0.5 s fade-in: the beep at 0.5 s (clip start) is almost silent, the one at 1.5 s isn't.
        tr.clips[0].audio = ClipAudio { fade_in: Time::from_secs_f64(0.5), ..Default::default() };
    }
    export_sequence(&p, &settings(out.clone()), &device, &queue, |_| true).unwrap();
    let mut r = AudioReader::open(&out, 48_000).unwrap();
    let mut beep = |from: f64| {
        let mut buf = vec![0.0f32; 4000 * 2];
        r.read((from * 48_000.0) as i64, &mut buf);
        let l: Vec<f32> = buf.chunks(2).map(|c| c[0]).collect();
        let rr: Vec<f32> = buf.chunks(2).map(|c| c[1]).collect();
        (rms(&l), rms(&rr))
    };
    let (l1, r1) = beep(0.505);
    let (l2, r2) = beep(1.505);
    assert!(l2 < 0.005, "hard right: left channel silent ({l2})");
    // Unfaded beep: 0.5 amplitude sine (rms ~0.354) at -6 dB -> ~0.177.
    assert!((r2 - 0.177).abs() < 0.03, "track volume halves the level ({r2})");
    assert!(r1 < 0.25 * r2, "beep inside the fade-in is much quieter ({r1} vs {r2})");
    let _ = l1;
}

/// 10 s pure tones as WAV, amplitude 1/8 (lavfi default).
fn tone(freq: u32) -> Option<PathBuf> {
    ffmpeg(&format!("rf_tone_{freq}.wav"), &["-f", "lavfi", "-i", &format!("sine=frequency={freq}:sample_rate=48000:duration=10")])
}

fn audio_asset(id: u64, path: &Path) -> MediaAsset {
    asset(id, path)
}

/// Renders the timeline audio exactly as playback/export do.
fn render_mix(p: &Project, secs: f64) -> Vec<f32> {
    let mut m = reelforge_media::Mixer::new(48_000);
    m.set_clips(reelforge_export::mix_clips(p, 48_000));
    let mut out = vec![0.0f32; (secs * 48_000.0) as usize * 2];
    for (i, chunk) in out.chunks_mut(1024 * 2).enumerate() {
        m.render(i as i64 * 1024, chunk);
    }
    out
}

/// Power of one frequency in the left channel (Goertzel).
fn tone_power(s: &[f32], hz: f32) -> f32 {
    let w = 2.0 * std::f32::consts::PI * hz / 48_000.0;
    let (mut s1, mut s2) = (0.0f32, 0.0f32);
    for x in s.chunks(2).map(|c| c[0]) {
        let s0 = x + 2.0 * w.cos() * s1 - s2;
        s2 = s1;
        s1 = s0;
    }
    s1 * s1 + s2 * s2 - 2.0 * w.cos() * s1 * s2
}

fn zero_cross_hz(s: &[f32]) -> f32 {
    let l: Vec<f32> = s.chunks(2).map(|c| c[0]).collect();
    l.windows(2).filter(|w| (w[0] < 0.0) != (w[1] < 0.0)).count() as f32 / 2.0 / (l.len() as f32 / 48_000.0)
}

#[test]
fn crossfade_overlaps_into_spare_media_with_constant_power() {
    let (Some(a), Some(b)) = (tone(440), tone(660)) else { return };
    let s = Time::from_secs_f64;
    let mut p = Project::new();
    p.media.push(audio_asset(1, &a));
    p.media.push(audio_asset(2, &b));
    let mut seq = Sequence::new("X", 640, 360, FrameRate::new(25, 1));
    // A: source 0–4 s at 0–4 s. B: source 2–6 s at 4–8 s. Both have plenty of spare media.
    let ca = Clip::new(ClipId(1), MediaId(1), s(0.0), s(0.0), s(4.0), None);
    let mut cb = Clip::new(ClipId(2), MediaId(2), s(4.0), s(2.0), s(4.0), None);
    seq.tracks[2].clips = vec![ca, cb.clone()];
    p.sequences = vec![seq];

    // Plain cut first: B is silent before 4 s.
    let cut = render_mix(&p, 6.0);
    let at = |buf: &[f32], a: f64, b: f64| buf[(a * 48_000.0) as usize * 2..(b * 48_000.0) as usize * 2].to_vec();
    assert!((zero_cross_hz(&at(&cut, 3.6, 3.9)) - 440.0).abs() < 5.0);

    // 1 s crossfade: overlap 3.5–4.5 s.
    cb.audio.crossfade_in = s(1.0);
    p.sequences[0].tracks[2].clips[1] = cb;
    let x = p.crossfade_into(0, ClipId(2)).unwrap();
    assert_eq!((x.start(), x.end()), (s(3.5), s(4.5)));
    let mix = render_mix(&p, 6.0);
    let steady = rms(&at(&mix, 1.0, 2.0));
    for k in 0..40 {
        let t = 3.0 + k as f64 * 0.05;
        let lvl = rms(&at(&mix, t, t + 0.05));
        let db = 20.0 * (lvl / steady).log10();
        assert!(db.abs() < 1.0, "level dips or bumps by {db:.2} dB at {t:.2} s");
    }
    assert!((zero_cross_hz(&at(&mix, 3.0, 3.4)) - 440.0).abs() < 5.0, "A alone before the overlap");
    assert!((zero_cross_hz(&at(&mix, 4.6, 5.0)) - 660.0).abs() < 5.0, "B alone after it");
    // Exactly at the cut both are at -3 dB (equal power), so their energies match.
    let mid = at(&mix, 3.95, 4.05);
    let (e440, e660) = (tone_power(&mid, 440.0), tone_power(&mid, 660.0));
    assert!((e440 / e660 - 1.0).abs() < 0.25, "both tones equally loud at the cut ({e440} vs {e660})");
    let early = at(&mix, 3.55, 3.65);
    assert!(tone_power(&early, 440.0) > 4.0 * tone_power(&early, 660.0), "outgoing dominates early in the fade");
}

#[test]
fn crossfade_without_spare_media_falls_back_to_a_cut() {
    let (Some(a), Some(b)) = (tone(440), tone(660)) else { return };
    let s = Time::from_secs_f64;
    let mut p = Project::new();
    p.media.push(audio_asset(1, &a));
    p.media.push(audio_asset(2, &b));
    let mut seq = Sequence::new("X", 640, 360, FrameRate::new(25, 1));
    // A uses its media to the very end; B starts at its very beginning: no handles at all.
    let ca = Clip::new(ClipId(1), MediaId(1), s(0.0), s(6.0), s(4.0), None);
    let mut cb = Clip::new(ClipId(2), MediaId(2), s(4.0), s(0.0), s(4.0), None);
    cb.audio.crossfade_in = s(1.0);
    seq.tracks[2].clips = vec![ca, cb];
    p.sequences = vec![seq];
    let x = p.crossfade_into(0, ClipId(2)).unwrap();
    assert!(x.is_empty(), "no spare media: {x:?}");
    let mix = render_mix(&p, 6.0);
    let steady = rms(&mix[48_000 * 2..96_000 * 2]);
    let around = rms(&mix[(3.8 * 48_000.0) as usize * 2..(4.2 * 48_000.0) as usize * 2]);
    assert!((around / steady - 1.0).abs() < 0.05, "plain cut keeps the level");
}

fn solid_clip(name: &str, colour: &str, size: &str) -> Option<PathBuf> {
    ffmpeg(name, &["-f", "lavfi", "-i", &format!("color=c={colour}:s={size}:r=25:d=4"), "-c:v", "libx264", "-pix_fmt", "yuv420p", "-g", "25"])
}

#[test]
fn exported_dissolve_and_keyframed_motion() {
    let (Some(red), Some(blue), Some(white)) = (
        solid_clip("rf_red.mp4", "red", "640x360"),
        solid_clip("rf_blue.mp4", "blue", "640x360"),
        solid_clip("rf_white_small.mp4", "white", "64x64"),
    ) else {
        return;
    };
    let Some((device, queue)) = headless_device() else { return };
    let s = Time::from_secs_f64;
    let mut p = Project::new();
    p.media.push(asset(1, &red));
    p.media.push(asset(2, &blue));
    p.media.push(asset(3, &white));
    let mut seq = Sequence::new("T", 640, 360, FrameRate::new(25, 1));
    // V1: red (source 0–2) then blue (source 1–3), 1 s dissolve centred on the 2 s cut.
    let a = Clip::new(ClipId(1), MediaId(1), s(0.0), s(0.0), s(2.0), None);
    let mut b = Clip::new(ClipId(2), MediaId(2), s(2.0), s(1.0), s(2.0), None);
    b.dissolve_in = s(1.0);
    // V2: a white square sliding from x = -200 to +200 over the first 2 s.
    let mut w = Clip::new(ClipId(3), MediaId(3), s(0.0), s(0.0), s(4.0), None);
    w.anim.position.set(s(0.0), [-200.0, 0.0]);
    w.anim.position.set(s(2.0), [200.0, 0.0]);
    seq.tracks[0].clips = vec![a, b];
    seq.tracks[1].clips = vec![w];
    p.sequences = vec![seq];

    let out = std::env::temp_dir().join("rf_export_dissolve.mp4");
    let mut st = settings(out.clone());
    st.end = s(4.0);
    st.include_audio = false;
    export_sequence(&p, &st, &device, &queue, |_| true).unwrap();

    let rate = FrameRate::new(25, 1);
    let mut dec = FrameSource::open(&out, None).unwrap();
    let mut pixel = |frame: i64, x: u32, y: u32| {
        let f = dec.frame_at_blocking(Time::from_frame(frame, rate), Duration::from_secs(5)).unwrap();
        let i = ((y * 640 + x) * 4) as usize;
        [f.rgba[i] as i32, f.rgba[i + 1] as i32, f.rgba[i + 2] as i32]
    };
    let close = |got: [i32; 3], want: [i32; 3]| got.iter().zip(want).all(|(g, w)| (g - w).abs() <= 6);
    // Corner (away from the square): red, mid-dissolve purple (linear-light 50/50), then blue.
    assert!(close(pixel(25, 10, 10), [254, 0, 0]), "{:?}", pixel(25, 10, 10));
    assert!(close(pixel(50, 10, 10), [188, 0, 188]), "mid-dissolve: {:?}", pixel(50, 10, 10));
    assert!(close(pixel(75, 10, 10), [0, 0, 254]), "{:?}", pixel(75, 10, 10));
    // Square centre at t = 1 s is x = 320 (frame centre); at t = 0.5 s it's 320 - 100.
    assert!(close(pixel(25, 320, 180), [255, 255, 255]), "square at centre at 1 s");
    assert!(close(pixel(12, 320 - 100 + 3, 180), [255, 255, 255]), "square at x-100 at 0.5 s");
    assert!(!close(pixel(12, 320, 180), [255, 255, 255]), "and not yet at the centre");
}

#[test]
fn exports_the_marked_range_with_keyframed_effects() {
    use reelforge_core::{Effect, EffectKind};
    let (Some(red), Some(green)) = (solid_clip("rf_red.mp4", "red", "640x360"), solid_clip("rf_lime.mp4", "lime", "640x360")) else { return };
    let Some((device, queue)) = headless_device() else { return };
    let s = Time::from_secs_f64;
    let mut p = Project::new();
    p.media.push(asset(1, &red));
    p.media.push(asset(2, &green));
    let mut seq = Sequence::new("T", 640, 360, FrameRate::new(25, 1));
    // V1: red, desaturating from 100% to 0% over its 2 s.
    let mut a = Clip::new(ClipId(1), MediaId(1), s(0.0), s(0.0), s(2.0), None);
    let mut cc = Effect::new(EffectKind::ColorCorrect);
    cc.params[2].keys.set(s(0.0), 100.0);
    cc.params[2].keys.set(s(2.0), 0.0);
    a.effects.push(cc);
    // V2: a green screen over it, keyed out completely.
    let mut g = Clip::new(ClipId(2), MediaId(2), s(0.0), s(0.0), s(2.0), None);
    g.effects.push(Effect::new(EffectKind::ChromaKey));
    seq.tracks[0].clips = vec![a];
    seq.tracks[1].clips = vec![g];
    // Export only from 1 s to the end.
    seq.in_point = Some(s(1.0));
    let (start, end) = seq.marked_range().unwrap();
    assert_eq!((start, end), (s(1.0), s(2.0)));
    p.sequences = vec![seq];

    let out = std::env::temp_dir().join("rf_export_range_fx.mp4");
    let mut st = settings(out.clone());
    (st.start, st.end, st.include_audio) = (start, end, false);
    let report = export_sequence(&p, &st, &device, &queue, |_| true).unwrap();
    assert_eq!(report.frames, 25, "one second at 25 fps");
    let info = probe(&out).unwrap();
    assert!((info.duration.as_secs_f64() - 1.0).abs() < 0.05, "duration {}", info.duration);

    let rate = FrameRate::new(25, 1);
    let mut dec = FrameSource::open(&out, None).unwrap();
    let mut pixel = |frame: i64| {
        let f = dec.frame_at_blocking(Time::from_frame(frame, rate), Duration::from_secs(5)).unwrap();
        let i = ((180 * 640 + 320) * 4) as usize;
        [f.rgba[i] as i32, f.rgba[i + 1] as i32, f.rgba[i + 2] as i32]
    };
    let close = |got: [i32; 3], want: [i32; 3]| got.iter().zip(want).all(|(g, w)| (g - w).abs() <= 8);
    // The movie's first frame is the sequence at 1 s: red at 50% saturation, green keyed away.
    assert!(close(pixel(0), [204, 92, 92]), "half saturation at 1 s: {:?}", pixel(0));
    // Its last frame (1.96 s) is almost grey.
    assert!(close(pixel(24), [131, 126, 126]), "nearly grey at the end: {:?}", pixel(24));
}

#[test]
fn exports_titles_with_their_transform_and_effects() {
    use reelforge_core::{Effect, EffectKind, TitleSpec};
    let Some(red) = solid_clip("rf_red.mp4", "red", "640x360") else { return };
    let Some((device, queue)) = headless_device() else { return };
    let s = Time::from_secs_f64;
    let mut p = Project::new();
    p.media.push(asset(1, &red));
    let mut seq = Sequence::new("T", 640, 360, FrameRate::new(25, 1));
    seq.tracks[0].clips = vec![Clip::new(ClipId(1), MediaId(1), s(0.0), s(0.0), s(2.0), None)];
    // A white box title (no text) moved 100 px right and fading out over its second second.
    let spec = TitleSpec { text: "".into(), background: true, background_color: [255, 255, 255, 255], background_padding: 30.0, shadow: false, ..TitleSpec::default() };
    let mut t = Clip::new_title(ClipId(2), s(0.0), s(2.0), TitleSpec { text: "I".into(), color: [255, 255, 255, 255], size: 40.0, ..spec });
    t.transform.position = [100.0, 0.0];
    t.anim.opacity.set(s(1.0), 1.0);
    t.anim.opacity.set(s(2.0), 0.0);
    t.effects.push(Effect::new(EffectKind::Vignette)); // effects apply to titles too (centre unaffected)
    seq.tracks[1].clips = vec![t];
    p.sequences = vec![seq];

    let out = std::env::temp_dir().join("rf_export_title.mp4");
    let mut st = settings(out.clone());
    st.include_audio = false;
    export_sequence(&p, &st, &device, &queue, |_| true).unwrap();
    let rate = FrameRate::new(25, 1);
    let mut dec = FrameSource::open(&out, None).unwrap();
    let mut pixel = |frame: i64, x: u32, y: u32| {
        let f = dec.frame_at_blocking(Time::from_frame(frame, rate), Duration::from_secs(5)).unwrap();
        let i = ((y * 640 + x) * 4) as usize;
        [f.rgba[i] as i32, f.rgba[i + 1] as i32, f.rgba[i + 2] as i32]
    };
    let close = |got: [i32; 3], want: [i32; 3]| got.iter().zip(want).all(|(g, w)| (g - w).abs() <= 10);
    // The white box is centred 100 px right of the frame centre, and only there.
    assert!(close(pixel(10, 420, 180), [255, 255, 255]), "{:?}", pixel(10, 420, 180));
    assert!(close(pixel(10, 200, 180), [254, 0, 0]), "{:?}", pixel(10, 200, 180));
    // Half-way through the fade (t = 1.5 s) it's 50% over red, in linear light.
    assert!(close(pixel(37, 420, 180), [255, 188, 188]), "{:?}", pixel(37, 420, 180));
}

/// Red video with a 440 Hz tone, 1 s, as a one-clip project (video + linked audio).
fn red_with_tone() -> Option<(Project, PathBuf)> {
    let src = ffmpeg("rf_red_tone.mp4", &[
        "-f", "lavfi", "-i", "color=c=red:s=640x360:r=25:d=1",
        "-f", "lavfi", "-i", "sine=frequency=440:sample_rate=48000:duration=1",
        "-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest",
    ])?;
    let s = Time::from_secs_f64;
    let mut p = Project::new();
    p.media.push(asset(1, &src));
    let mut seq = Sequence::new("T", 640, 360, FrameRate::new(25, 1));
    seq.tracks[0].clips = vec![Clip::new(ClipId(1), MediaId(1), s(0.0), s(0.0), s(1.0), None)];
    seq.tracks[2].clips = vec![Clip::new(ClipId(2), MediaId(1), s(0.0), s(0.0), s(1.0), None)];
    p.sequences = vec![seq];
    Some((p, src))
}

fn first_frame_centre(path: &Path) -> [i32; 3] {
    let mut dec = FrameSource::open(path, None).unwrap();
    let f = dec.frame_at_blocking(Time::ZERO, Duration::from_secs(5)).unwrap();
    let i = (((f.height / 2) * f.width + f.width / 2) * 4) as usize;
    [f.rgba[i] as i32, f.rgba[i + 1] as i32, f.rgba[i + 2] as i32]
}

#[test]
fn exports_every_video_format() {
    let Some((p, _)) = red_with_tone() else { return };
    let Some((device, queue)) = headless_device() else { return };
    let red = |c: [i32; 3]| c[0] > 215 && c[1] < 30 && c[2] < 30; // lossy codecs drift a little
    for (codec, want_codec, want_audio) in [
        (VideoCodec::H265, "hevc", Some("aac")),
        (VideoCodec::ProRes422Hq, "prores", Some("pcm_s24le")),
        (VideoCodec::DnxhrHq, "dnxhd", Some("pcm_s24le")),
    ] {
        let out = std::env::temp_dir().join(format!("rf_fmt_{}.{}", want_codec, codec.extension()));
        let mut st = settings(out.clone());
        (st.end, st.video) = (Time::from_secs_f64(0.4), Some(codec));
        let r = export_sequence(&p, &st, &device, &queue, |_| true).unwrap();
        let info = probe(&out).unwrap();
        assert_eq!(info.video.as_ref().unwrap().codec, want_codec, "{codec:?} via {}", r.video_codec);
        assert_eq!(info.audio.as_ref().map(|a| a.codec.as_str()), want_audio, "{codec:?}");
        assert!(red(first_frame_centre(&out)), "{codec:?}: {:?}", first_frame_centre(&out));
    }
    // Image sequences: one numbered file per frame, no audio.
    for codec in [VideoCodec::PngSequence, VideoCodec::JpegSequence, VideoCodec::TiffSequence] {
        let dir = std::env::temp_dir().join(format!("rf_seq_{}", codec.extension()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut st = settings(dir.join(format!("shot.{}", codec.extension())));
        (st.end, st.video) = (Time::from_secs_f64(0.2), Some(codec));
        let r = export_sequence(&p, &st, &device, &queue, |_| true).unwrap();
        assert!(!r.has_audio);
        let mut files: Vec<String> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        files.sort();
        let ext = codec.extension();
        assert_eq!(files, (1..=5).map(|i| format!("shot_{i:05}.{ext}")).collect::<Vec<_>>());
        let c = first_frame_centre(&dir.join(format!("shot_00001.{ext}"))); assert!(red(c), "{codec:?} {c:?}");
    }
}

#[test]
fn letterboxes_into_a_different_shape_and_exports_audio_alone() {
    let Some((p, _)) = red_with_tone() else { return };
    let Some((device, queue)) = headless_device() else { return };
    // A 16:9 sequence into a 9:16 frame: bars above and below, picture in the middle.
    let out = std::env::temp_dir().join("rf_vertical.mp4");
    let mut st = settings(out.clone());
    (st.width, st.height, st.end, st.include_audio) = (360, 640, Time::from_secs_f64(0.2), false);
    // Asking for the GPU falls back to software quietly when there isn't one.
    st.hardware = true;
    let r = export_sequence(&p, &st, &device, &queue, |_| true).unwrap();
    eprintln!("encoder used: {} (hardware: {})", r.video_codec, r.hardware);
    let mut dec = FrameSource::open(&out, None).unwrap();
    let f = dec.frame_at_blocking(Time::ZERO, Duration::from_secs(5)).unwrap();
    assert_eq!((f.width, f.height), (360, 640));
    let at = |x: u32, y: u32| f.rgba[((y * 360 + x) * 4) as usize] as i32;
    assert!(at(180, 40) < 20, "black bar at the top");
    assert!(at(180, 320) > 230, "picture in the middle");
    assert!(at(180, 600) < 20, "black bar at the bottom");
    // The picture is 360 × 202 (16:9), so it ends about 101 px either side of the middle.
    assert!(at(180, 320 - 95) > 200 && at(180, 320 - 108) < 40);

    // Fill instead: the picture covers the whole frame, edges cut off.
    st.fit = Fit::Crop;
    export_sequence(&p, &st, &device, &queue, |_| true).unwrap();
    let mut dec = FrameSource::open(&out, None).unwrap();
    let f = dec.frame_at_blocking(Time::ZERO, Duration::from_secs(5)).unwrap();
    let at = |x: u32, y: u32| f.rgba[((y * 360 + x) * 4) as usize] as i32;
    assert!(at(180, 10) > 200 && at(5, 320) > 200 && at(180, 630) > 200, "no bars when cropping");

    // Audio only.
    let wav = std::env::temp_dir().join("rf_audio_only.wav");
    let mut st = settings(wav.clone());
    (st.video, st.end) = (None, Time::from_secs_f64(1.0));
    let r = export_sequence(&p, &st, &device, &queue, |_| true).unwrap();
    assert!(r.has_audio);
    let info = probe(&wav).unwrap();
    assert!(info.video.is_none());
    assert_eq!(info.audio.as_ref().unwrap().codec, "pcm_s24le");
    assert!((info.duration.as_secs_f64() - 1.0).abs() < 0.05, "{}", info.duration);
    let mut rd = AudioReader::open(&wav, 48_000).unwrap();
    let mut buf = vec![0.0f32; 48_000];
    rd.read(12_000, &mut buf);
    // lavfi's sine is 1/8 amplitude: RMS 0.088.
    assert!((rms(&buf) - 0.088).abs() < 0.01, "the tone is there: {}", rms(&buf));
}
