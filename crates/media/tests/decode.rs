//! Integration test: generates a clip with the `ffmpeg` CLI (skipped if it isn't
//! on PATH) and checks probing, sequential decode and random seeks.

use reelforge_core::{FrameRate, Time};
use reelforge_media::{probe, FrameSource};
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

fn make_clip() -> Option<PathBuf> {
    // Made once per test run: tests run in parallel, and one must not read the
    // file while another is still writing it.
    static CLIP: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    CLIP.get_or_init(write_clip).clone()
}

fn write_clip() -> Option<PathBuf> {
    // Written beside the final name, then renamed, so a half-written file is never seen.
    let path = std::env::temp_dir().join("reelforge_test_25fps.mp4");
    if path.exists() {
        return Some(path);
    }
    let part = std::env::temp_dir().join(format!("reelforge_test_25fps.{}.mp4", std::process::id()));
    // 6 s, 25 fps, keyframe every 2 s so seeks must decode forward from a keyframe.
    let status = Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i", "testsrc2=size=640x360:rate=25:duration=6"])
        .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=6"])
        .args(["-c:v", "libx264", "-g", "50", "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest"])
        .arg(&part)
        .status()
        .ok()?;
    (status.success() && std::fs::rename(&part, &path).is_ok()).then_some(path)
}

#[test]
fn probe_reports_streams() {
    let Some(path) = make_clip() else { eprintln!("ffmpeg CLI not found; skipping"); return };
    let info = probe(&path).unwrap();
    let v = info.video.expect("video stream");
    assert_eq!((v.width, v.height), (640, 360));
    assert_eq!(v.frame_rate, FrameRate::new(25, 1));
    assert_eq!(v.codec, "h264");
    let a = info.audio.expect("audio stream");
    assert_eq!(a.codec, "aac");
    assert!((info.duration.as_secs_f64() - 6.0).abs() < 0.1, "{}", info.duration);
}

#[test]
fn sequential_and_random_access_land_on_exact_frames() {
    let Some(path) = make_clip() else { eprintln!("ffmpeg CLI not found; skipping"); return };
    let rate = FrameRate::new(25, 1);
    let mut src = FrameSource::open(&path, None).unwrap();
    let wait = Duration::from_secs(5);

    // Sequential playback of the first second.
    for f in 0..25 {
        let t = Time::from_frame(f, rate);
        let frame = src.frame_at_blocking(t, wait).expect("frame");
        assert_eq!(frame.pts.to_frame(rate), f, "sequential frame {f}");
        assert_eq!((frame.width, frame.height), (640, 360));
        assert_eq!(frame.rgba.len(), 640 * 360 * 4);
    }

    // Random access, including backwards and mid-GOP targets.
    for f in [120, 37, 149, 0, 99, 51, 74, 10] {
        let t = Time::from_frame(f, rate) + Time::from_secs_f64(0.01); // mid-frame
        let frame = src.frame_at_blocking(t, wait).expect("frame");
        assert_eq!(frame.pts.to_frame(rate), f, "seek to frame {f}");
    }
}

#[test]
fn preview_downscale() {
    let Some(path) = make_clip() else { return };
    let mut src = FrameSource::open(&path, Some((320, 320))).unwrap();
    let frame = src.frame_at_blocking(Time::ZERO, Duration::from_secs(5)).unwrap();
    assert_eq!((frame.width, frame.height), (320, 180));
}

#[test]
fn seeking_past_the_end_returns_the_last_frame_promptly() {
    let Some(path) = make_clip() else { return };
    let rate = FrameRate::new(25, 1);
    let mut src = FrameSource::open(&path, Some((320, 320))).unwrap();
    let started = std::time::Instant::now();
    let frame = src.frame_at_blocking(Time::from_secs_f64(9.0), Duration::from_secs(10)).expect("frame");
    assert_eq!(frame.pts.to_frame(rate), 149, "should hold the final frame");
    assert!(started.elapsed() < Duration::from_secs(3), "took {:?}", started.elapsed());
}

#[test]
fn bt709_colours_decode_accurately() {
    let path = std::env::temp_dir().join("reelforge_test_709.mp4");
    if !path.exists() {
        let ok = Command::new("ffmpeg")
            .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i", "color=c=0x3080C0:s=1280x720:r=25:d=1"])
            .args(["-vf", "scale=out_color_matrix=bt709:out_range=tv,format=yuv420p"])
            .args(["-colorspace", "bt709", "-color_primaries", "bt709", "-color_trc", "bt709", "-color_range", "tv"])
            .args(["-c:v", "libx264", "-qp", "0"])
            .arg(&path)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            return;
        }
    }
    let mut src = FrameSource::open(&path, None).unwrap();
    let f = src.frame_at_blocking(Time::ZERO, Duration::from_secs(5)).unwrap();
    let i = ((360 * 1280 + 640) * 4) as usize;
    let got = [f.rgba[i], f.rgba[i + 1], f.rgba[i + 2]];
    for (g, w) in got.iter().zip([0x30, 0x80, 0xC0]) {
        assert!((*g as i32 - w).abs() <= 3, "got {got:?}, want [48, 128, 192]");
    }
}

#[test]
fn a_second_decoder_gets_already_seen_frames_from_the_cache_instantly() {
    let Some(path) = make_clip() else { return };
    let rate = FrameRate::new(25, 1);
    let size = Some((320, 180));
    // The first viewer of the file decodes 0–1 s.
    let mut a = FrameSource::open_cached(&path, size).unwrap();
    for i in 0..25 {
        a.frame_at_blocking(Time::from_frame(i, rate), Duration::from_secs(5)).unwrap();
    }
    // A fresh decoder (e.g. another clip of the same file) asked for 0.6 s answers
    // straight away with the right frame, before its own decoder has produced anything.
    let mut b = FrameSource::open_cached(&path, size).unwrap();
    let t = Time::from_frame(15, rate);
    let f = b.frame_at(t).cloned().expect("served from the cache");
    assert_eq!(f.pts.to_frame(rate), 15);
    assert!(b.is_exact(t));
    // An uncached source has to wait for its decoder.
    let mut c = FrameSource::open(&path, size).unwrap();
    assert!(!c.is_exact(t) || c.frame_at(t).is_some_and(|f| f.pts.to_frame(rate) == 15));
}

#[test]
fn proxies_are_small_all_keyframe_and_line_up_with_the_original() {
    use reelforge_media::proxy;
    use std::sync::atomic::AtomicBool;
    // 1280x720 HEVC is "heavy": proxies are made for it automatically.
    let path = std::env::temp_dir().join("reelforge_proxy_src.mp4");
    if !path.exists() {
        let ok = Command::new("ffmpeg")
            .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i", "testsrc2=size=1280x720:rate=25:duration=3"])
            .args(["-c:v", "libx265", "-x265-params", "log-level=error", "-pix_fmt", "yuv420p"])
            .arg(&path)
            .status()
            .is_ok_and(|s| s.success());
        if !ok {
            return;
        }
    }
    let info = probe(&path).unwrap();
    assert!(proxy::worth_a_proxy(&info));
    let _ = std::fs::remove_file(proxy::proxy_path(&path));
    let mut last = 0.0;
    let out = proxy::make_proxy(&path, &info, &AtomicBool::new(false), |p| last = p).unwrap();
    assert_eq!(last, 1.0);
    assert_eq!(proxy::existing_proxy(&path), Some(out.clone()));
    let pi = probe(&out).unwrap();
    let v = pi.video.unwrap();
    assert_eq!((v.width, v.height), (960, 540));
    assert!((pi.duration.as_secs_f64() - 3.0).abs() < 0.1);
    // Frame 40 of the proxy is frame 40 of the original (testsrc2 draws the frame number).
    let rate = FrameRate::new(25, 1);
    let t = Time::from_frame(40, rate);
    let a = FrameSource::open(&path, Some((960, 540))).unwrap().frame_at_blocking(t, Duration::from_secs(5)).unwrap();
    let b = FrameSource::open(&out, None).unwrap().frame_at_blocking(t, Duration::from_secs(5)).unwrap();
    assert_eq!(b.pts.to_frame(rate), 40);
    let diff: f64 = a.rgba.iter().zip(b.rgba.iter()).map(|(x, y)| (*x as i32 - *y as i32).unsigned_abs() as f64).sum::<f64>() / a.rgba.len() as f64;
    assert!(diff < 4.0, "proxy frame differs from the original: {diff}");
}
