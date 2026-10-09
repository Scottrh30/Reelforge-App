//! Integration test: generates a clip with the `ffmpeg` CLI (skipped if it isn't
//! on PATH) and checks probing, sequential decode and random seeks.

use reelforge_core::{FrameRate, Time};
use reelforge_media::{probe, FrameSource};
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

fn make_clip() -> Option<PathBuf> {
    let path = std::env::temp_dir().join("reelforge_test_25fps.mp4");
    if path.exists() {
        return Some(path);
    }
    // 6 s, 25 fps, keyframe every 2 s so seeks must decode forward from a keyframe.
    let status = Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i", "testsrc2=size=640x360:rate=25:duration=6"])
        .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=6"])
        .args(["-c:v", "libx264", "-g", "50", "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest"])
        .arg(&path)
        .status()
        .ok()?;
    status.success().then_some(path)
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
