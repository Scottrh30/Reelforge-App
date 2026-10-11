//! Tracks a box moving across a generated clip (needs the `ffmpeg` CLI; skipped otherwise).

use reelforge_core::track::PointTrack;
use reelforge_core::Time;
use reelforge_media::{probe, track_point, TrackEnd};
use std::process::Command;
use std::sync::atomic::AtomicBool;

#[test]
fn follows_a_moving_box_both_ways() {
    let path = std::env::temp_dir().join("rf_track_box.mp4");
    // A 16 px white box with a dark notch, moving 60 px/s right and 20 px/s down over noise-free grey.
    let filter = "color=c=0x606060:s=320x240:r=25:d=2[bg];color=c=white:s=16x16:r=25[b];[b]drawbox=x=4:y=4:w=4:h=4:c=black:t=fill[box];[bg][box]overlay=x='40+t*60':y='60+t*20':shortest=1";
    let ok = Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i", filter, "-c:v", "libx264", "-crf", "12", "-pix_fmt", "yuv420p"])
        .arg(&path)
        .status()
        .is_ok_and(|s| s.success());
    if !ok {
        eprintln!("ffmpeg CLI not found; skipping");
        return;
    }
    let info = probe(&path).unwrap();
    let s = Time::from_secs_f64;
    // The box's centre at t = 0 is (48, 68).
    let track = PointTrack { point: [48.0, 68.0], start: Time::ZERO, size: 25.0, search: 12.0, path: Vec::new() };
    let mut last = 0.0;
    let (samples, end) = track_point(&path, &info, &track, Time::ZERO, s(1.5), &AtomicBool::new(false), |p| last = p).unwrap();
    assert_eq!(end, TrackEnd::Done);
    assert!(last > 0.99);
    assert_eq!(samples.len(), 38, "0 to 1.5 s at 25 fps");
    let at = |samples: &[reelforge_core::track::TrackSample], t: f64| samples.iter().find(|x| (x.at.as_secs_f64() - t).abs() < 0.01).unwrap().pos;
    let p = at(&samples, 1.0);
    // 40 + 60 = 100 -> centre 108; 60 + 20 = 80 -> 88.
    assert!((p[0] - 108.0).abs() < 1.0 && (p[1] - 88.0).abs() < 1.0, "{p:?}");

    // Backwards from 1.48 s (a frame time), starting where the forward run had it.
    let back = PointTrack { point: at(&samples, 1.48), start: s(1.48), ..track.clone() };
    let (samples, end) = track_point(&path, &info, &back, s(1.48), s(0.6), &AtomicBool::new(false), |_| {}).unwrap();
    assert_eq!(end, TrackEnd::Done);
    assert_eq!(samples.len(), 23);
    // 40 + 36 = 76 -> 84; 60 + 12 = 72 -> 80.
    let p = at(&samples, 0.6);
    assert!((p[0] - 84.0).abs() < 1.0 && (p[1] - 80.0).abs() < 1.0, "{p:?}");
}
