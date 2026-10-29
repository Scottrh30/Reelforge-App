//! End-to-end export: build a two-layer project with offset audio, export it,
//! then decode the result and compare it with what the compositor renders.
//! Needs the `ffmpeg` CLI (to make test media) and a GPU adapter; skips otherwise.

use reelforge_core::{BlendMode, Clip, ClipId, FrameRate, MediaAsset, MediaId, Project, Sequence, Time, Transform};
use reelforge_export::{export_sequence, frame_range, Cancelled, ExportSettings};
use reelforge_media::{probe, AudioReader, ColorOut, Fit, FrameSource, Quality, VideoCodec};
use reelforge_render::{headless_device, Compositor, LayerInput, PixelData};
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

const B_TRANSFORM: Transform = Transform { position: [100.0, 0.0], scale: [1.0, 1.0], rotation: 0.0, opacity: 0.5, anchor: [0.0, 0.0], crop: [0.0; 4], pin: None, three_d: false, depth: 0.0, orientation: [0.0, 0.0], lit: true };

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
        sequence: 0,
        color: ColorOut::Sdr8,
        bitrate_kbps: None,
        alpha: false,
    }
}

/// What the compositor makes of the project at `t`, rendered independently of the exporter.
fn reference_frame(c: &mut Compositor, a: &Path, b: &Path, t: f64) -> Vec<u8> {
    let wait = Duration::from_secs(5);
    // Distinct per instant, so the compositor's texture cache re-uploads.
    let id = (t * 1000.0) as u64;
    let fa = FrameSource::open(a, None).unwrap().frame_at_blocking(Time::from_secs_f64(t), wait).unwrap();
    let mut layers = vec![LayerInput {
        key: 1, content_id: id, pixels: (&fa.pixels).into(), pixel_size: [fa.width, fa.height],
        layer_size: [640.0, 360.0], transform: Transform::default(), blend: BlendMode::Normal, effects: Vec::new(), mask: None, wipe: None, mesh: None, space: None
    }];
    let fb;
    if t >= 1.0 {
        fb = FrameSource::open(b, None).unwrap().frame_at_blocking(Time::from_secs_f64(t - 1.0), wait).unwrap();
        layers.push(LayerInput {
            key: 2, content_id: id, pixels: (&fb.pixels).into(), pixel_size: [fb.width, fb.height],
            layer_size: [320.0, 180.0], transform: B_TRANSFORM, blend: BlendMode::Normal, effects: Vec::new(), mask: None, wipe: None, mesh: None, space: None
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
        let err = mean_abs_diff(&got.rgba(), &want);
        assert!(err < 2.5, "frame {frame_no}: mean error {err:.2} levels");

        // Spot check the overlay: centre of B is at (420, 180).
        let i = (180 * 640 + 420) * 4;
        // 50% red over anything gives at least 0.5 linear red = 188 in sRGB.
        if frame_no >= 25 {
            assert!(got.rgba()[i] >= 180, "frame {frame_no}: half-transparent red layer missing ({:?})", &got.rgba()[i..i + 3]);
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
    m.set_clips(reelforge_export::mix_clips(p, 0, 48_000));
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
        [f.rgba()[i] as i32, f.rgba()[i + 1] as i32, f.rgba()[i + 2] as i32]
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
        [f.rgba()[i] as i32, f.rgba()[i + 1] as i32, f.rgba()[i + 2] as i32]
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
        [f.rgba()[i] as i32, f.rgba()[i + 1] as i32, f.rgba()[i + 2] as i32]
    };
    let close = |got: [i32; 3], want: [i32; 3]| got.iter().zip(want).all(|(g, w)| (g - w).abs() <= 10);
    // The white box is centred 100 px right of the frame centre, and only there.
    assert!(close(pixel(10, 420, 180), [255, 255, 255]), "{:?}", pixel(10, 420, 180));
    assert!(close(pixel(10, 200, 180), [254, 0, 0]), "{:?}", pixel(10, 200, 180));
    // Half-way through the fade (t = 1.5 s) it's 50% over red, in linear light.
    assert!(close(pixel(37, 420, 180), [255, 188, 188]), "{:?}", pixel(37, 420, 180));
}

/// A mesh warp on a title (a still) follows its keys through the clip.
#[test]
fn exports_mesh_warped_titles_over_time() {
    use reelforge_core::meshwarp::{MeshShape, MeshWarp};
    use reelforge_core::TitleSpec;
    let Some(red) = solid_clip("rf_red.mp4", "red", "640x360") else { return };
    let Some((device, queue)) = headless_device() else { return };
    let s = Time::from_secs_f64;
    let mut p = Project::new();
    p.media.push(asset(1, &red));
    let mut seq = Sequence::new("T", 640, 360, FrameRate::new(25, 1));
    seq.tracks[0].clips = vec![Clip::new(ClipId(1), MediaId(1), s(0.0), s(0.0), s(2.0), None)];
    // A white box in the middle of a frame-sized title, laid over the left half of
    // the frame at 0 s and the right half from 1 s.
    let spec = TitleSpec { text: "I".into(), color: [255, 255, 255, 255], size: 40.0, background: true, background_color: [255, 255, 255, 255], background_padding: 30.0, shadow: false, ..TitleSpec::default() };
    let mut t = Clip::new_title(ClipId(2), s(0.0), s(2.0), spec);
    let left = MeshShape::filling([[0.0, 0.0], [320.0, 0.0], [320.0, 360.0], [0.0, 360.0]], 2, 2);
    let right = left.map(|q| [q[0] + 320.0, q[1]]);
    t.mesh = Some(MeshWarp { keys: vec![(s(0.0), left), (s(1.0), right)] });
    seq.tracks[1].clips = vec![t];
    p.sequences = vec![seq];

    let out = std::env::temp_dir().join("rf_export_mesh.mp4");
    let mut st = settings(out.clone());
    st.include_audio = false;
    export_sequence(&p, &st, &device, &queue, |_| true).unwrap();
    let rate = FrameRate::new(25, 1);
    let mut dec = FrameSource::open(&out, None).unwrap();
    let mut pixel = |frame: i64, x: u32, y: u32| {
        let f = dec.frame_at_blocking(Time::from_frame(frame, rate), Duration::from_secs(5)).unwrap();
        let i = ((y * 640 + x) * 4) as usize;
        [f.rgba()[i] as i32, f.rgba()[i + 1] as i32, f.rgba()[i + 2] as i32]
    };
    let close = |got: [i32; 3], want: [i32; 3]| got.iter().zip(want).all(|(g, w)| (g - w).abs() <= 12);
    assert!(close(pixel(0, 160, 180), [255, 255, 255]), "box in the left half at 0 s: {:?}", pixel(0, 160, 180));
    assert!(close(pixel(0, 480, 180), [254, 0, 0]), "{:?}", pixel(0, 480, 180));
    assert!(close(pixel(37, 480, 180), [255, 255, 255]), "box in the right half at 1.5 s: {:?}", pixel(37, 480, 180));
    assert!(close(pixel(37, 160, 180), [254, 0, 0]), "{:?}", pixel(37, 160, 180));
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
    [f.rgba()[i] as i32, f.rgba()[i + 1] as i32, f.rgba()[i + 2] as i32]
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

/// What ffprobe says about a file's video: (codec, profile, pixel format).
fn ffprobe_video(path: &Path) -> Option<(String, String, String)> {
    let out = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=codec_name,profile,pix_fmt", "-of", "default=nw=1"])
        .arg(path)
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let get = |k: &str| text.lines().find_map(|l| l.strip_prefix(&format!("{k}="))).unwrap_or("").to_string();
    Some((get("codec_name"), get("profile"), get("pix_fmt")))
}

#[test]
fn exports_every_master_profile() {
    let Some((p, _)) = red_with_tone() else { return };
    let Some((device, queue)) = headless_device() else { return };
    let red = |c: [i32; 3]| c[0] > 215 && c[1] < 30 && c[2] < 30;
    use VideoCodec as V;
    // (format, file type, size, codec, profile, pixel format)
    let cases: &[(V, &str, (u32, u32), &str, &str, &str)] = &[
        (V::ProRes422Proxy, "mov", (640, 360), "prores", "Proxy", "yuv422p10le"),
        (V::ProRes422Lt, "mov", (640, 360), "prores", "LT", "yuv422p10le"),
        (V::ProRes422, "mov", (640, 360), "prores", "Standard", "yuv422p10le"),
        (V::ProRes4444, "mov", (640, 360), "prores", "4444", "yuv444p12le"), // FFmpeg decodes 4444 as 12-bit
        (V::ProRes4444Xq, "mov", (640, 360), "prores", "XQ", "yuv444p12le"),
        (V::DnxhrLb, "mov", (640, 360), "dnxhd", "DNXHR LB", "yuv422p"),
        (V::DnxhrSq, "mov", (640, 360), "dnxhd", "DNXHR SQ", "yuv422p"),
        (V::DnxhrHqx, "mov", (640, 360), "dnxhd", "DNXHR HQX", "yuv422p10le"),
        (V::Dnxhr444, "mov", (640, 360), "dnxhd", "DNXHR 444", "yuv444p10le"),
        (V::DnxhrHq, "mxf", (640, 360), "dnxhd", "DNXHR HQ", "yuv422p"),
        (V::Dnxhd, "mov", (1280, 720), "dnxhd", "DNXHD", "yuv422p"),
        (V::Dnxhd10, "mxf", (1920, 1080), "dnxhd", "DNXHD", "yuv422p10le"),
        (V::Cineform, "mov", (640, 360), "cfhd", "", "yuv422p10le"),
        (V::Cineform4444, "mov", (640, 360), "cfhd", "", "gbrp12le"),
    ];
    for &(codec, ext, (w, h), want_codec, want_profile, want_pix) in cases {
        let out = std::env::temp_dir().join(format!("rf_master_{codec:?}.{ext}"));
        let mut st = settings(out.clone());
        (st.end, st.video, st.width, st.height) = (Time::from_secs_f64(0.2), Some(codec), w, h);
        let r = export_sequence(&p, &st, &device, &queue, |_| true).unwrap_or_else(|e| panic!("{codec:?} {ext}: {e:#}"));
        assert_eq!(r.frames, 5);
        let info = probe(&out).unwrap();
        assert_eq!(info.audio.as_ref().map(|a| a.codec.as_str()), Some("pcm_s24le"), "{codec:?}");
        if let Some((c, prof, pix)) = ffprobe_video(&out) {
            assert_eq!((c.as_str(), pix.as_str()), (want_codec, want_pix), "{codec:?} via {}", r.video_codec);
            assert!(prof.contains(want_profile), "{codec:?}: profile {prof}");
        }
        assert!(red(first_frame_centre(&out)), "{codec:?}: {:?}", first_frame_centre(&out));
    }
    // DNxHD only comes in its own sizes, and says so.
    let mut st = settings(std::env::temp_dir().join("rf_master_bad.mov"));
    (st.end, st.video) = (Time::from_secs_f64(0.2), Some(V::Dnxhd));
    let e = export_sequence(&p, &st, &device, &queue, |_| true).unwrap_err();
    assert!(format!("{e:#}").contains("Use DNxHR"), "{e:#}");
}

/// A half-size red layer on nothing: exported with a transparent background,
/// the corners are see-through and the middle solid.
#[test]
fn exports_a_transparent_background() {
    let Some((mut p, _)) = red_with_tone() else { return };
    let Some((device, queue)) = headless_device() else { return };
    let seq = &mut p.sequences[0];
    seq.tracks[2].clips.clear();
    seq.tracks[0].clips[0].transform.scale = [0.5, 0.5];
    use VideoCodec as V;
    for (codec, ext, alpha) in [(V::ProRes4444, "mov", true), (V::ProRes4444Xq, "mov", true), (V::Cineform4444, "mov", true), (V::PngSequence, "png", true), (V::TiffSequence, "tif", true), (V::ProRes4444, "mov", false)] {
        let dir = std::env::temp_dir().join(format!("rf_alpha_{codec:?}_{alpha}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut st = settings(dir.join(format!("a.{ext}")));
        (st.end, st.video, st.alpha, st.include_audio) = (Time::from_secs_f64(0.2), Some(codec), alpha, false);
        export_sequence(&p, &st, &device, &queue, |_| true).unwrap_or_else(|e| panic!("{codec:?}: {e:#}"));
        let file = if codec.is_sequence() { dir.join(format!("a_00001.{ext}")) } else { st.path.clone() };
        let mut dec = FrameSource::open(&file, None).unwrap();
        let f = dec.frame_at_blocking(Time::ZERO, Duration::from_secs(5)).unwrap();
        let px = |x: u32, y: u32| -> [u8; 4] { f.rgba()[((y * f.width + x) * 4) as usize..][..4].try_into().unwrap() };
        let (corner, middle) = (px(10, 10), px(320, 180));
        assert!(middle[0] > 215 && middle[1] < 30 && middle[3] >= 253, "{codec:?}: middle {middle:?}");
        if alpha {
            assert!(corner[3] <= 2, "{codec:?}: corner should be see-through: {corner:?}");
        } else {
            assert!(corner[3] >= 253 && corner[0] < 20, "{codec:?}: corner should be opaque black: {corner:?}");
        }
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
    let at = |x: u32, y: u32| f.rgba()[((y * 360 + x) * 4) as usize] as i32;
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
    let at = |x: u32, y: u32| f.rgba()[((y * 360 + x) * 4) as usize] as i32;
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

#[test]
fn exports_composite_shots_with_their_transform_and_audio() {
    let Some((mut p, _)) = red_with_tone() else { return };
    let Some((device, queue)) = headless_device() else { return };
    let s = Time::from_secs_f64;
    // The red+tone sequence becomes a composition (sequence 1) inside a new main
    // sequence, from 0.5 s to 1.5 s, at half size and -6 dB.
    let inner = p.sequences.remove(0);
    let mut main = Sequence::new("Main", 640, 360, FrameRate::new(25, 1));
    let mut v = Clip::new_nested(ClipId(10), 1, s(0.5), s(1.0), Some(7));
    v.transform.scale = [0.5, 0.5];
    let mut a = Clip::new_nested(ClipId(11), 1, s(0.5), s(1.0), Some(7));
    a.audio.volume_db = -6.0206;
    main.tracks[1].clips = vec![v];
    main.tracks[2].clips = vec![a];
    p.sequences = vec![main, inner];

    let mix = reelforge_export::mix_clips(&p, 0, 48_000);
    assert_eq!(mix.len(), 1);
    assert_eq!((mix[0].start, mix[0].len), (24_000, 48_000), "moved to where the shot plays");
    assert!((mix[0].gain - 0.5).abs() < 0.01);

    let out = std::env::temp_dir().join("rf_nested.mp4");
    let st = settings(out.clone());
    export_sequence(&p, &st, &device, &queue, |_| true).unwrap();

    let mut dec = FrameSource::open(&out, None).unwrap();
    let wait = Duration::from_secs(5);
    let px = |f: &reelforge_media::VideoFrame, x: u32, y: u32| {
        let i = ((y * f.width + x) * 4) as usize;
        [f.rgba()[i] as i32, f.rgba()[i + 1] as i32, f.rgba()[i + 2] as i32]
    };
    let before = dec.frame_at_blocking(s(0.2), wait).unwrap();
    assert!(px(&before, 320, 180)[0] < 30, "nothing before the shot starts");
    let during = dec.frame_at_blocking(s(1.0), wait).unwrap();
    let c = px(&during, 320, 180);
    assert!(c[0] > 215 && c[1] < 30, "the shot's picture in the middle: {c:?}");
    assert!(px(&during, 40, 40)[0] < 30, "scaled to half size, so the corners are empty");
    assert!(px(&during, 320 + 150, 180)[0] > 200 && px(&during, 320 + 170, 180)[0] < 40, "edge of the half-size picture at x = 160 from the centre");

    let mut rd = AudioReader::open(&out, 48_000).unwrap();
    let mut buf = vec![0.0f32; 9_600];
    rd.read(4_800, &mut buf);
    assert!(rms(&buf) < 0.005, "silent before the shot");
    rd.read(36_000, &mut buf);
    assert!((rms(&buf) - 0.044).abs() < 0.008, "the shot's tone at -6 dB: {}", rms(&buf));
    rd.read(79_000, &mut buf);
    assert!(rms(&buf) < 0.005, "silent after the shot");
}

#[test]
fn transparent_compositor_keeps_coverage() {
    let Some((device, queue)) = headless_device() else { return };
    let mut c = Compositor::new(&device, &queue);
    c.transparent = true;
    // A 50%-opaque red layer at half size in the middle of a 64×64 frame.
    let px: Vec<u8> = [255u8, 0, 0, 255].repeat(16 * 16);
    let t = Transform { scale: [0.5, 0.5], opacity: 0.5, ..Default::default() };
    let layer = LayerInput {
        key: 1, content_id: 1, pixels: PixelData::Rgba(&px), pixel_size: [16, 16], layer_size: [64.0, 64.0],
        transform: t, blend: BlendMode::Normal, effects: Vec::new(), mask: None, wipe: None, mesh: None, space: None
    };
    c.render([64, 64], &[layer]);
    let out = c.read_output().unwrap();
    let at = |x: usize, y: usize| &out[(y * 64 + x) * 4..][..4];
    assert_eq!(at(2, 2), [0, 0, 0, 0], "transparent where nothing is drawn");
    let m = at(32, 32);
    assert!(m[0] > 250 && m[1] < 5 && (m[3] as i32 - 128).abs() <= 2, "straight colour, half coverage: {m:?}");
}

#[test]
fn exports_masked_layers() {
    use reelforge_core::mask::{Mask, MaskMode};
    let Some((mut p, _)) = red_with_tone() else { return };
    let Some((device, queue)) = headless_device() else { return };
    // Keep the left half of the red layer, with a 40 px feather, minus a hole in the middle of it.
    let keep = Mask { points: vec![[0.0, 0.0], [320.0, 0.0], [320.0, 360.0], [0.0, 360.0]], feather: 40.0, ..Default::default() };
    let hole = Mask { mode: MaskMode::Subtract, points: vec![[100.0, 150.0], [140.0, 150.0], [140.0, 210.0], [100.0, 210.0]], ..Default::default() };
    p.sequences[0].tracks[0].clips[0].masks = vec![keep, hole];
    let out = std::env::temp_dir().join("rf_masked.mp4");
    let mut st = settings(out.clone());
    (st.end, st.include_audio) = (Time::from_secs_f64(0.2), false);
    export_sequence(&p, &st, &device, &queue, |_| true).unwrap();
    let mut dec = FrameSource::open(&out, None).unwrap();
    let f = dec.frame_at_blocking(Time::ZERO, Duration::from_secs(5)).unwrap();
    let r = |x: u32, y: u32| f.rgba()[((y * 640 + x) * 4) as usize] as i32;
    assert!(r(40, 40) > 220, "kept: {}", r(40, 40));
    assert!(r(600, 40) < 20, "masked out: {}", r(600, 40));
    assert!(r(120, 180) < 20, "the subtracted hole: {}", r(120, 180));
    // On the feathered edge the layer is half there: half-way in linear light is about 188 in sRGB.
    let edge = r(320, 40);
    assert!((150..215).contains(&edge), "soft edge: {edge}");
    assert!(r(300, 40) > edge && r(340, 40) < edge, "fading out to the right");
}

#[test]
fn exports_speed_changes_and_reverse() {
    // 2 s that get steadily brighter, so each moment has its own grey level.
    let Some(src) = ffmpeg("rf_ramp.mp4", &[
        "-f", "lavfi", "-i", "color=c=gray:s=64x64:r=25:d=2", "-vf", "geq=lum='clip(20+T*100,0,255)':cb=128:cr=128",
        "-c:v", "libx264", "-g", "5", "-crf", "8", "-pix_fmt", "yuv420p",
    ]) else { return };
    let Some((device, queue)) = headless_device() else { return };
    let s = Time::from_secs_f64;
    let mut p = Project::new();
    p.media.push(asset(1, &src));
    let mut seq = Sequence::new("S", 64, 64, FrameRate::new(25, 1));
    seq.tracks[0].clips = vec![Clip::new(ClipId(1), MediaId(1), s(0.0), s(0.0), s(2.0), None)];
    p.sequences = vec![seq];
    for _ in 0..10 {
        p.alloc_id();
    }
    let grey = |path: &Path, t: f64| {
        let mut d = FrameSource::open(path, None).unwrap();
        let f = d.frame_at_blocking(s(t), Duration::from_secs(5)).unwrap();
        f.rgba()[((32 * 64 + 32) * 4) as usize] as i32
    };
    for (speed, reverse, checks) in [(2.0f32, false, [(0.1, 0.2), (0.6, 1.2)]), (1.0, true, [(0.1, 1.88), (1.5, 0.48)])] {
        let mut q = p.clone();
        let cmd = reelforge_core::speed::change_speed(&q, 0, ClipId(1), speed, reverse).unwrap();
        let mut u = reelforge_core::UndoStack::default();
        u.execute(&mut q, cmd);
        let out = std::env::temp_dir().join(format!("rf_speed_{speed}_{reverse}.mp4"));
        let mut st = settings(out.clone());
        (st.width, st.height, st.include_audio, st.end) = (64, 64, false, q.sequences[0].duration());
        export_sequence(&q, &st, &device, &queue, |_| true).unwrap();
        for (t, media) in checks {
            let (got, want) = (grey(&out, t), grey(&src, media));
            assert!((got - want).abs() <= 6, "speed {speed} reverse {reverse}: at {t} s got {got}, the media at {media} s is {want}");
        }
    }
}

#[test]
fn exports_plugin_effects_that_animate_with_time() {
    let Some(red) = solid_clip("rf_red.mp4", "red", "640x360") else { return };
    let Some((device, queue)) = headless_device() else { return };
    // A plugin: swap red and green, then fade to black as time goes on (1 s → 0).
    let manifest = "id = \"test.export-plugin\"\nname = \"Swap\"\n[[param]]\nname = \"Mix\"\ndefault = 100\nmax = 100\n";
    let source = "fn effect(uv: vec2<f32>) -> vec4<f32> {\n  let c = sample(uv);\n  let k = clamp(1.0 - time(), 0.0, 1.0) * param(0) / 100.0;\n  return vec4(c.g * k, c.r * k, c.b, c.a);\n}";
    let def = std::sync::Arc::new(reelforge_core::plugin::parse(manifest, source, Path::new("/p")).unwrap());
    reelforge_core::plugin::set_installed(vec![def.clone()]);
    let s = Time::from_secs_f64;
    let mut p = Project::new();
    p.media.push(asset(1, &red));
    let mut seq = Sequence::new("T", 640, 360, FrameRate::new(25, 1));
    let mut c = Clip::new(ClipId(1), MediaId(1), s(0.0), s(0.0), s(2.0), None);
    c.effects.push(reelforge_core::Effect::new_plugin(&def));
    seq.tracks[0].clips = vec![c];
    p.sequences = vec![seq];
    let out = std::env::temp_dir().join("rf_export_plugin.mp4");
    let mut st = settings(out.clone());
    st.end = s(2.0);
    st.include_audio = false;
    export_sequence(&p, &st, &device, &queue, |_| true).unwrap();
    let rate = FrameRate::new(25, 1);
    let mut dec = FrameSource::open(&out, None).unwrap();
    let mut pixel = |frame: i64| {
        let f = dec.frame_at_blocking(Time::from_frame(frame, rate), Duration::from_secs(5)).unwrap();
        let i = ((180 * 640 + 320) * 4) as usize;
        [f.rgba()[i] as i32, f.rgba()[i + 1] as i32, f.rgba()[i + 2] as i32]
    };
    let close = |got: [i32; 3], want: [i32; 3]| got.iter().zip(want).all(|(g, w)| (g - w).abs() <= 6);
    // Frame 0: red became green. Half a second in: linear 0.5 -> sRGB 188. After 1 s: black.
    assert!(close(pixel(0), [0, 254, 0]), "{:?}", pixel(0));
    assert!(close(pixel(12), [0, 190, 0]) || close(pixel(12), [0, 186, 0]), "{:?}", pixel(12));
    assert!(close(pixel(40), [0, 0, 0]), "{:?}", pixel(40));
}


/// `ffprobe` stream fields of the first video stream, as "key=value" lines.
fn video_stream_info(path: &Path) -> String {
    let out = std::process::Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=pix_fmt,color_transfer,color_primaries,color_space,bit_rate", "-of", "default=nw=1"])
        .arg(path)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// The 10-bit luma code at the centre of the first frame (limited range: 64 black, 940 white).
fn centre_luma_10bit(path: &Path, w: usize, h: usize) -> u16 {
    let out = std::process::Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(path)
        .args(["-frames:v", "1", "-f", "rawvideo", "-pix_fmt", "yuv420p10le", "-"])
        .output()
        .unwrap();
    let i = ((h / 2) * w + w / 2) * 2;
    u16::from_le_bytes([out.stdout[i], out.stdout[i + 1]])
}

#[test]
fn exports_10_bit_sdr_and_hdr_with_the_right_curves_and_tags() {
    let Some(src) = ffmpeg("rf_white.mp4", &["-f", "lavfi", "-i", "color=c=white:s=320x180:r=25:d=1", "-c:v", "libx264", "-pix_fmt", "yuv420p"]) else { return };
    let Some((device, queue)) = headless_device() else { return };
    let s = Time::from_secs_f64;
    let mut p = Project::new();
    p.media.push(asset(1, &src));
    let mut seq = Sequence::new("T", 320, 180, FrameRate::new(25, 1));
    seq.tracks[0].clips = vec![Clip::new(ClipId(1), MediaId(1), s(0.0), s(0.0), s(1.0), None)];
    p.sequences = vec![seq];
    // White in the edit: 940 in 10-bit SDR, 203 nits in PQ (code ≈ 573), 75% in HLG (≈ 721).
    for (color, pix, trc, want) in [
        (ColorOut::Sdr10, "yuv420p10le", "bt709", 940),
        (ColorOut::HdrPq, "yuv420p10le", "smpte2084", 573),
        (ColorOut::HdrHlg, "yuv420p10le", "arib-std-b67", 721),
    ] {
        let out = std::env::temp_dir().join(format!("rf_deep_{color:?}.mp4"));
        let mut st = settings(out.clone());
        (st.width, st.height, st.end, st.video, st.color, st.include_audio) = (320, 180, s(0.4), Some(VideoCodec::H265), color, false);
        export_sequence(&p, &st, &device, &queue, |_| true).unwrap();
        let info = video_stream_info(&out);
        assert!(info.contains(&format!("pix_fmt={pix}")), "{color:?}: {info}");
        assert!(info.contains(&format!("color_transfer={trc}")), "{color:?}: {info}");
        if color.is_hdr() {
            assert!(info.contains("color_primaries=bt2020"), "{color:?}: {info}");
        }
        let y = centre_luma_10bit(&out, 320, 180) as i32;
        assert!((y - want).abs() <= 6, "{color:?}: white is {y}, want {want}");
    }
    // H.264 can't do 10-bit/HDR: it falls back to 8-bit SDR rather than failing.
    let out = std::env::temp_dir().join("rf_deep_h264.mp4");
    let mut st = settings(out.clone());
    (st.width, st.height, st.end, st.color, st.include_audio) = (320, 180, s(0.4), ColorOut::HdrPq, false);
    export_sequence(&p, &st, &device, &queue, |_| true).unwrap();
    assert!(video_stream_info(&out).contains("pix_fmt=yuv420p\n"), "{}", video_stream_info(&out));
}

#[test]
fn a_target_bitrate_replaces_the_quality_setting() {
    // Busy, noisy video, so the encoder needs every bit it's given.
    let Some(src) = ffmpeg("rf_noise.mp4", &["-f", "lavfi", "-i", "testsrc2=s=640x360:r=25:d=2,noise=alls=60:allf=t", "-c:v", "libx264", "-crf", "10", "-pix_fmt", "yuv420p"]) else { return };
    let Some((device, queue)) = headless_device() else { return };
    let s = Time::from_secs_f64;
    let mut p = Project::new();
    p.media.push(asset(1, &src));
    let mut seq = Sequence::new("T", 640, 360, FrameRate::new(25, 1));
    seq.tracks[0].clips = vec![Clip::new(ClipId(1), MediaId(1), s(0.0), s(0.0), s(2.0), None)];
    p.sequences = vec![seq];
    let size_at = |kbps: Option<u32>| {
        let out = std::env::temp_dir().join(format!("rf_rate_{}.mp4", kbps.unwrap_or(0)));
        let mut st = settings(out.clone());
        (st.include_audio, st.bitrate_kbps) = (false, kbps);
        export_sequence(&p, &st, &device, &queue, |_| true).unwrap();
        std::fs::metadata(&out).unwrap().len() as f64 * 8.0 / 2.0 / 1000.0 // kbit/s over 2 s
    };
    let low = size_at(Some(500));
    let high = size_at(Some(3000));
    assert!(low > 300.0 && low < 750.0, "500 kbit/s target came out at {low:.0}");
    assert!(high > 1800.0 && high < 4500.0, "3000 kbit/s target came out at {high:.0}");
}

/// 3D text: a solid in the camera's view, its sides in their own colour,
/// shaded by the studio light without lights and by the scene's lights with them.
#[test]
fn exports_3d_text_lit_by_the_scene() {
    use reelforge_core::space3d::SceneObject;
    use reelforge_core::TitleSpec;
    let Some((device, queue)) = headless_device() else { return };
    let s = Time::from_secs_f64;
    let make = |light: Option<f32>| {
        let mut p = Project::new();
        let mut seq = Sequence::new("T", 640, 360, FrameRate::new(25, 1));
        let spec = TitleSpec { text: "H".into(), size: 220.0, color: [230, 30, 30, 255], shadow: false, extrude: 120.0, side_color: Some([30, 60, 230, 255]), ..TitleSpec::default() };
        let mut t = Clip::new_title(ClipId(2), s(0.0), s(1.0), spec);
        t.transform.three_d = true;
        t.transform.orientation = [0.0, 40.0];
        seq.tracks[0].clips = vec![t];
        if let Some(k) = light {
            // An ambient light at `k`% lights everything evenly.
            let mut l = Clip::new_scene(ClipId(3), s(0.0), s(1.0), SceneObject::Light, [640.0, 360.0]);
            l.effects[0].params[reelforge_core::effects::light::KIND].value = 3.0;
            l.effects[0].params[reelforge_core::effects::light::INTENSITY].value = k;
            seq.tracks[1].clips = vec![l];
        }
        p.sequences = vec![seq];
        p
    };
    let frame = |p: &Project, name: &str| {
        let out = std::env::temp_dir().join(name);
        let mut st = settings(out.clone());
        st.include_audio = false;
        st.end = s(0.2);
        st.video = Some(VideoCodec::ProRes422Hq);
        export_sequence(p, &st, &device, &queue, |_| true).unwrap();
        let mut dec = FrameSource::open(&out, None).unwrap();
        dec.frame_at_blocking(Time::ZERO, Duration::from_secs(5)).unwrap().rgba().to_vec()
    };
    let count = |img: &[u8], f: &dyn Fn(i32, i32, i32) -> bool| img.chunks_exact(4).filter(|p| f(p[0] as i32, p[1] as i32, p[2] as i32)).count();
    let studio = frame(&make(None), "rf_3dtext.mov");
    let faces = count(&studio, &|r, _, b| r > 150 && b < 90);
    let sides = count(&studio, &|r, _, b| b > 120 && r < 90);
    assert!(faces > 3000 && sides > 3000, "faces {faces}, sides {sides}");
    // Half an ambient light: everything at half its colour (in linear light).
    let dim = frame(&make(Some(50.0)), "rf_3dtext_lit.mov");
    let dim_faces = count(&dim, &|r, _, b| r > 140 && r < 200 && b < 70);
    assert!(dim_faces > faces / 2, "{dim_faces} of {faces}");
    assert_eq!(count(&dim, &|r, _, _| r > 215), 0, "nothing at full brightness");
}

#[test]
fn exports_particles_that_build_up_over_time() {
    use reelforge_core::particles as pt;
    use reelforge_core::{Effect, EffectKind, TitleSpec};
    let Some((device, queue)) = headless_device() else { return };
    let s = Time::from_secs_f64;
    let mut p = Project::new();
    let mut seq = Sequence::new("P", 320, 180, FrameRate::new(25, 1));
    let mut spec = TitleSpec::for_frame(180);
    spec.text = String::new();
    let mut c = Clip::new_title(ClipId(2), s(0.0), s(2.0), spec);
    let mut fx = Effect::new(EffectKind::Particles);
    // The "Sparks" preset, thrown up from the bottom middle.
    for (k, v) in pt::PRESETS[0].1.into_iter().enumerate() {
        fx.params[pt::SHAPE + k].value = v;
    }
    fx.params[pt::EMIT_Y].value = 90.0;
    fx.params[pt::SPEED].value = 250.0;
    fx.params[pt::GRAVITY].value = 300.0;
    c.effects.push(fx);
    seq.tracks[0].clips = vec![c];
    p.sequences = vec![seq];
    let out = std::env::temp_dir().join("rf_particles.mov");
    let mut st = settings(out.clone());
    st.include_audio = false;
    st.end = s(1.2);
    st.video = Some(VideoCodec::ProRes422Hq);
    export_sequence(&p, &st, &device, &queue, |_| true).unwrap();
    let mut dec = FrameSource::open(&out, None).unwrap();
    let mut lit = |t: f64| {
        let f = dec.frame_at_blocking(s(t), Duration::from_secs(5)).unwrap();
        f.rgba().chunks_exact(4).filter(|p| p[0] > 120).count()
    };
    let (first, early, later) = (lit(0.0), lit(0.2), lit(1.0));
    assert_eq!(first, 0, "nothing born at the start");
    assert!(early > 20 && later > early, "{early} then {later}");
}

#[test]
fn exports_an_exr_sequence_like_its_frames() {
    use reelforge_core::imageseq;
    let Some((device, queue)) = headless_device() else { return };
    let dir = std::env::temp_dir().join(format!("rf_export_exr_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let ok = std::process::Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i", "testsrc2=size=320x180:rate=25:duration=0.4", "-vf", "format=gbrpf32le", "-c:v", "exr", "-compression", "rle", "-format", "half", "-start_number", "0"])
        .arg(dir.join("bg_%03d.exr"))
        .status()
        .is_ok_and(|s| s.success());
    if !ok {
        return;
    }
    let s = Time::from_secs_f64;
    let seq = imageseq::detect(&dir.join("bg_000.exr"), reelforge_core::Rational::new(25, 1)).unwrap();
    let path = seq.media_path();
    let info = reelforge_media::probe(&path).unwrap();
    let mut p = Project::new();
    p.media = vec![reelforge_core::MediaAsset { id: reelforge_core::MediaId(1), path: path.clone(), name: seq.name(), info }];
    let mut sq = Sequence::new("E", 320, 180, FrameRate::new(25, 1));
    sq.tracks[0].clips = vec![Clip::new(ClipId(2), reelforge_core::MediaId(1), s(0.0), s(0.0), s(0.4), None)];
    p.sequences = vec![sq];
    let out = std::env::temp_dir().join("rf_exrseq.mov");
    let mut st = settings(out.clone());
    st.include_audio = false;
    st.end = s(0.4);
    (st.width, st.height) = (320, 180);
    st.video = Some(VideoCodec::ProRes422Hq);
    export_sequence(&p, &st, &device, &queue, |_| true).unwrap();
    let mut exported = FrameSource::open(&out, None).unwrap();
    let mut frames = FrameSource::open(&path, None).unwrap();
    for t in [0.0, 0.2] {
        let a = exported.frame_at_blocking(s(t), Duration::from_secs(5)).unwrap().rgba();
        let b = frames.frame_at_blocking(s(t), Duration::from_secs(5)).unwrap().rgba();
        let diff: f64 = a.chunks_exact(4).zip(b.chunks_exact(4)).map(|(x, y)| (0..3).map(|i| (x[i] as f64 - y[i] as f64).abs()).sum::<f64>()).sum::<f64>() / (320.0 * 180.0 * 3.0);
        // (ProRes is 4:2:2: some colour is lost on the test pattern's sharp coloured edges.)
        assert!(diff < 5.0, "at {t}s the export differs from the frame by {diff:.2} levels on average");
        let next = frames.frame_at_blocking(s(t + 0.04), Duration::from_secs(5)).unwrap().rgba();
        let off: f64 = a.chunks_exact(4).zip(next.chunks_exact(4)).map(|(x, y)| (0..3).map(|i| (x[i] as f64 - y[i] as f64).abs()).sum::<f64>()).sum::<f64>() / (320.0 * 180.0 * 3.0);
        assert!(diff < off, "at {t}s the export looks more like the next frame ({diff:.2} vs {off:.2})");
    }
    std::fs::remove_dir_all(&dir).ok();
}

/// Frame-threaded encoders (ProRes, DNxHR, CineForm) once got pictures that
/// had been overwritten by the next one: exports repeated and skipped frames.
/// Every exported frame must be its own source frame.
#[test]
fn intermediate_codec_exports_keep_every_frame_in_order() {
    let Some((device, queue)) = headless_device() else { return };
    let src = std::env::temp_dir().join(format!("rf_order_{}.mp4", std::process::id()));
    let ok = Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i", "testsrc2=size=320x180:rate=25:duration=0.4", "-pix_fmt", "yuv420p"])
        .arg(&src)
        .status()
        .is_ok_and(|s| s.success());
    if !ok {
        return;
    }
    let s = Time::from_secs_f64;
    let mut p = Project::new();
    p.media = vec![MediaAsset { id: MediaId(1), path: src.clone(), name: "src".into(), info: probe(&src).unwrap() }];
    let mut sq = Sequence::new("O", 320, 180, FrameRate::new(25, 1));
    sq.tracks[0].clips = vec![Clip::new(ClipId(2), MediaId(1), s(0.0), s(0.0), s(0.4), None)];
    p.sequences = vec![sq];
    let mut source = FrameSource::open(&src, None).unwrap();
    let originals: Vec<Vec<u8>> = (0..10).map(|k| source.frame_at_blocking(s(k as f64 * 0.04), Duration::from_secs(5)).unwrap().rgba().to_vec()).collect();
    let diff = |a: &[u8], b: &[u8]| a.iter().zip(b).map(|(x, y)| (*x as f64 - *y as f64).abs()).sum::<f64>() / a.len() as f64;
    for codec in [VideoCodec::ProRes422Hq, VideoCodec::ProRes4444, VideoCodec::DnxhrHq, VideoCodec::Cineform] {
        let out = std::env::temp_dir().join(format!("rf_order_{}_{codec:?}.mov", std::process::id()));
        let mut st = settings(out.clone());
        (st.width, st.height, st.include_audio, st.end, st.video) = (320, 180, false, s(0.4), Some(codec));
        export_sequence(&p, &st, &device, &queue, |_| true).unwrap();
        let mut exported = FrameSource::open(&out, None).unwrap();
        for k in 0..10usize {
            let got = exported.frame_at_blocking(s(k as f64 * 0.04), Duration::from_secs(5)).unwrap().rgba();
            let best = (0..10).min_by(|&a, &b| diff(&got, &originals[a]).total_cmp(&diff(&got, &originals[b]))).unwrap();
            assert_eq!(best, k, "{codec:?}: exported frame {k} looks like source frame {best}");
        }
        std::fs::remove_file(&out).ok();
    }
    std::fs::remove_file(&src).ok();
}

/// Grain changes every frame even when nothing is keyframed.
#[test]
fn film_grain_changes_every_exported_frame() {
    use reelforge_core::{Effect, EffectKind};
    let Some((device, queue)) = headless_device() else { return };
    let s = Time::from_secs_f64;
    let mut p = Project::new();
    let mut sq = Sequence::new("G", 160, 90, FrameRate::new(24, 1));
    let mut plane = Clip::new_plane(ClipId(2), s(0.0), s(1.0), [120, 120, 120, 255]);
    let mut grain = Effect::new(EffectKind::FilmGrain);
    grain.params[reelforge_core::effects::grain::AMOUNT].value = 100.0;
    plane.effects.push(grain);
    sq.tracks[0].clips = vec![plane];
    p.sequences = vec![sq];
    let out = std::env::temp_dir().join(format!("rf_grain_{}.mov", std::process::id()));
    let mut st = settings(out.clone());
    (st.width, st.height, st.include_audio, st.end, st.video) = (160, 90, false, s(0.2), Some(VideoCodec::ProRes422Hq));
    export_sequence(&p, &st, &device, &queue, |_| true).unwrap();
    let mut src = FrameSource::open(&out, None).unwrap();
    let f = |src: &mut FrameSource, k: f64| src.frame_at_blocking(s(k / 24.0), Duration::from_secs(5)).unwrap().rgba().to_vec();
    let (a, b, c) = (f(&mut src, 0.0), f(&mut src, 1.0), f(&mut src, 2.0));
    let differ = |x: &[u8], y: &[u8]| x.iter().zip(y).filter(|(p, q)| p.abs_diff(**q) > 4).count();
    assert!(differ(&a, &b) > 1000 && differ(&b, &c) > 1000, "{} {}", differ(&a, &b), differ(&b, &c));
    std::fs::remove_file(&out).ok();
}
