//! End-to-end export: build a two-layer project with offset audio, export it,
//! then decode the result and compare it with what the compositor renders.
//! Needs the `ffmpeg` CLI (to make test media) and a GPU adapter; skips otherwise.

use reelforge_core::{BlendMode, Clip, ClipId, FrameRate, MediaAsset, MediaId, Project, Sequence, Time, Transform};
use reelforge_export::{export_sequence, frame_range, Cancelled, ExportSettings};
use reelforge_media::{probe, AudioReader, FrameSource, Quality};
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
        layer_size: [640.0, 360.0], transform: Transform::default(), blend: BlendMode::Normal,
    }];
    let fb;
    if t >= 1.0 {
        fb = FrameSource::open(b, None).unwrap().frame_at_blocking(Time::from_secs_f64(t - 1.0), wait).unwrap();
        layers.push(LayerInput {
            key: 2, content_id: id, pixels: &fb.rgba, pixel_size: [fb.width, fb.height],
            layer_size: [320.0, 180.0], transform: B_TRANSFORM, blend: BlendMode::Normal,
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
