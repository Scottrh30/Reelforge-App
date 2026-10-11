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

// ---- Multi-point tracking on synthesised footage with known motion ----------

use reelforge_core::track::{TrackMode, Tracker};
use reelforge_core::transform::{Homography, Pin};
use std::io::Write;

const W: usize = 480;
const H: usize = 270;

/// The card's picture at (u, v) in 0–1: mid grey with fine texture and four
/// bold ring markers near its corners (the features to track).
fn card(u: f32, v: f32) -> f32 {
    let mut g = 120.0 + 25.0 * ((u * 40.0).sin() * (v * 31.0).cos());
    for (mu, mv) in MARKERS {
        let d = ((u - mu) * (u - mu) + (v - mv) * (v - mv)).sqrt();
        if d < 0.035 {
            g = 20.0;
        } else if d < 0.06 {
            g = 240.0;
        }
    }
    // A notch so each marker looks different from the others (no look-alikes).
    if u > 0.45 && u < 0.55 && v > 0.1 && v < 0.2 {
        g = 30.0;
    }
    g
}
const MARKERS: [(f32, f32); 4] = [(0.15, 0.15), (0.85, 0.15), (0.85, 0.85), (0.15, 0.85)];

/// Card corners on the frame at frame `k` for each kind of motion.
fn quad(k: usize, persp: bool) -> [[f32; 2]; 4] {
    let t = k as f32;
    let (cx, cy) = (240.0 + 1.5 * t, 135.0 - 0.6 * t);
    if persp {
        // A card swinging away on the right, while drifting.
        let s = 0.9 * t;
        [[cx - 110.0, cy - 75.0 + 0.2 * s], [cx + 110.0 - s, cy - 75.0 + 0.6 * s], [cx + 110.0 - s, cy + 75.0 - 0.6 * s], [cx - 110.0, cy + 75.0 - 0.2 * s]]
    } else {
        // Turning 1.2° and growing 0.5% a frame.
        let (a, k) = ((1.2 * t).to_radians(), 1.0 + 0.005 * t);
        let (sn, cs) = a.sin_cos();
        [[-100.0, -65.0], [100.0, -65.0], [100.0, 65.0], [-100.0, 65.0]].map(|[x, y]| [cx + k * (cs * x - sn * y), cy + k * (sn * x + cs * y)])
    }
}

fn card_map(k: usize, persp: bool) -> Homography {
    Homography::from_quads(&[[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]], &quad(k, persp)).unwrap()
}

/// Encode `frames` frames of the moving card (25 fps) to `path`.
fn make_clip(path: &std::path::Path, frames: usize, persp: bool) -> bool {
    let child = Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error", "-f", "rawvideo", "-pix_fmt", "gray", "-s", &format!("{W}x{H}"), "-r", "25", "-i", "-", "-c:v", "libx264", "-crf", "14", "-pix_fmt", "yuv420p"])
        .arg(path)
        .stdin(std::process::Stdio::piped())
        .spawn();
    let Ok(mut child) = child else { return false };
    let mut stdin = child.stdin.take().unwrap();
    for k in 0..frames {
        let inv = card_map(k, persp).inverse().unwrap();
        let mut img = vec![0u8; W * H];
        for y in 0..H {
            for x in 0..W {
                let [u, v] = inv.apply([x as f32 + 0.5, y as f32 + 0.5]);
                let bg = 60.0 + 0.1 * x as f32 + 15.0 * ((x as f32 * 0.07).sin() * (y as f32 * 0.05).cos());
                let g = if (0.0..=1.0).contains(&u) && (0.0..=1.0).contains(&v) { card(u, v) } else { bg };
                img[y * W + x] = g.clamp(0.0, 255.0) as u8;
            }
        }
        stdin.write_all(&img).unwrap();
    }
    drop(stdin);
    child.wait().is_ok_and(|s| s.success())
}

fn marker_at(k: usize, persp: bool, i: usize) -> [f32; 2] {
    let (u, v) = MARKERS[i];
    card_map(k, persp).apply([u, v])
}

fn run(mode: TrackMode, persp: bool, name: &str) -> Option<(Tracker, Vec<Vec<reelforge_core::track::TrackSample>>)> {
    let path = std::env::temp_dir().join(name);
    if !make_clip(&path, 40, persp) {
        eprintln!("ffmpeg CLI not found; skipping");
        return None;
    }
    let info = probe(&path).unwrap();
    let mut tr = Tracker::new(mode, [240.0, 135.0], [W as f32, H as f32], Time::ZERO);
    let which: Vec<usize> = if mode == TrackMode::Transform { vec![0, 2] } else { vec![0, 1, 2, 3] };
    for (p, &i) in tr.points.iter_mut().zip(&which) {
        p.point = marker_at(0, persp, i);
        p.size = 41.0;
        p.search = 20.0;
    }
    let (samples, end) = reelforge_media::track_points(&path, &info, &tr, Time::ZERO, Time::from_secs_f64(1.56), &AtomicBool::new(false), |_| {}).unwrap();
    assert_eq!(end, TrackEnd::Done, "{name}");
    assert_eq!(samples.len(), which.len());
    for (p, s) in samples.iter().zip(&which) {
        assert_eq!(p.len(), 40);
        for (k, smp) in p.iter().enumerate() {
            let truth = marker_at(k, persp, *s);
            let err = ((smp.pos[0] - truth[0]).powi(2) + (smp.pos[1] - truth[1]).powi(2)).sqrt();
            if std::env::var("RF_DEBUG").is_ok() && k % 4 == 0 { eprintln!("{name} m{s} f{k}: d=({:+.2},{:+.2}) score {:.3}", smp.pos[0] - truth[0], smp.pos[1] - truth[1], smp.score); }
            assert!(std::env::var("RF_DEBUG").is_ok() || err < 1.0, "{name}: marker {s} frame {k}: {:?} vs {truth:?} ({err:.2} px)", smp.pos);
        }
    }
    for (p, s) in tr.points.iter_mut().zip(samples.clone()) {
        p.merge(s);
    }
    Some((tr, samples))
}

#[test]
fn two_points_track_rotation_and_scale() {
    let Some((tr, _)) = run(TrackMode::Transform, false, "rf_track_rot.mp4") else { return };
    let f = tr.frames();
    let h = reelforge_core::track::motion_between(TrackMode::Transform, &f[0].1, &f[39].1).unwrap();
    // 39 frames: 46.8° and ×1.195.
    let m = &h.0;
    let angle = m[3].atan2(m[0]).to_degrees();
    let scale = (m[0] * m[0] + m[3] * m[3]).sqrt();
    assert!((angle - 46.8).abs() < 0.5, "{angle}");
    assert!((scale - 1.195).abs() < 0.01, "{scale}");
}

#[test]
fn four_points_track_perspective() {
    let Some((tr, _)) = run(TrackMode::CornerPin, true, "rf_track_persp.mp4") else { return };
    let f = tr.frames();
    let h = reelforge_core::track::motion_between(TrackMode::CornerPin, &f[0].1, &f[39].1).unwrap();
    // The card's corners (not tracked themselves) land where they really are.
    let (q0, q1) = (quad(0, true), quad(39, true));
    for i in 0..4 {
        let p = h.apply(q0[i]);
        assert!((p[0] - q1[i][0]).abs() < 2.0 && (p[1] - q1[i][1]).abs() < 2.0, "corner {i}: {p:?} vs {:?}", q1[i]);
    }
    assert!(Pin(q1).is_valid());
}
