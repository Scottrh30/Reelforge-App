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

// ---- Planar tracking ------------------------------------------------------------

/// Smooth random texture (value noise) in 0–1 for the planar test's poster.
fn noise(x: f32, y: f32) -> f32 {
    let (xi, yi) = (x.floor() as i64, y.floor() as i64);
    let (fx, fy) = (x - x.floor(), y - y.floor());
    let r = |a: i64, b: i64| -> f32 {
        let mut s = (a.wrapping_mul(374_761_393) ^ b.wrapping_mul(668_265_263)) as u64;
        s = (s ^ (s >> 13)).wrapping_mul(1_274_126_177);
        (s >> 40) as f32 / (1u64 << 24) as f32
    };
    let (sx, sy) = (fx * fx * (3.0 - 2.0 * fx), fy * fy * (3.0 - 2.0 * fy));
    let top = r(xi, yi) * (1.0 - sx) + r(xi + 1, yi) * sx;
    let bot = r(xi, yi + 1) * (1.0 - sx) + r(xi + 1, yi + 1) * sx;
    top * (1.0 - sy) + bot * sy
}

/// A textured poster (with plain margins, so its corners have no detail of
/// their own) swinging in perspective, while a dark bar sweeps across it.
#[test]
fn planar_tracks_a_surface_through_perspective_and_a_passing_bar() {
    let path = std::env::temp_dir().join("rf_track_planar.mp4");
    let frames = 40;
    let child = Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error", "-f", "rawvideo", "-pix_fmt", "gray", "-s", &format!("{W}x{H}"), "-r", "25", "-i", "-", "-c:v", "libx264", "-crf", "16", "-pix_fmt", "yuv420p"])
        .arg(&path)
        .stdin(std::process::Stdio::piped())
        .spawn();
    let Ok(mut child) = child else {
        eprintln!("ffmpeg CLI not found; skipping");
        return;
    };
    let mut stdin = child.stdin.take().unwrap();
    for k in 0..frames {
        let inv = card_map(k, true).inverse().unwrap();
        let bar = 60.0 + 9.0 * k as f32;
        let mut img = vec![0u8; W * H];
        for y in 0..H {
            for x in 0..W {
                let [u, v] = inv.apply([x as f32 + 0.5, y as f32 + 0.5]);
                let mut g = 50.0 + 0.1 * x as f32;
                if (0.0..=1.0).contains(&u) && (0.0..=1.0).contains(&v) {
                    // Plain margin round the edge, print in the middle.
                    let inner = (0.12..=0.88).contains(&u) && (0.12..=0.88).contains(&v);
                    g = if inner { 40.0 + 150.0 * noise(u * 30.0, v * 22.0) + 50.0 * noise(u * 80.0 + 9.0, v * 60.0) } else { 200.0 };
                }
                if (x as f32 - bar).abs() < 14.0 {
                    g = 15.0;
                }
                img[y * W + x] = g.clamp(0.0, 255.0) as u8;
            }
        }
        stdin.write_all(&img).unwrap();
    }
    drop(stdin);
    assert!(child.wait().unwrap().success());

    let info = probe(&path).unwrap();
    let mut tr = Tracker::new(TrackMode::Planar, [240.0, 135.0], [W as f32, H as f32], Time::ZERO);
    let corners = quad(0, true);
    for (p, c) in tr.points.iter_mut().zip(corners) {
        p.point = c;
        p.search = 20.0;
    }
    let (samples, end) = reelforge_media::track_points(&path, &info, &tr, Time::ZERO, Time::from_secs_f64(1.56), &AtomicBool::new(false), |_| {}).unwrap();
    assert_eq!(end, TrackEnd::Done);
    let mut worst = 0f32;
    for (i, p) in samples.iter().enumerate() {
        assert_eq!(p.len(), frames);
        for (k, s) in p.iter().enumerate() {
            let truth = quad(k, true)[i];
            let e = ((s.pos[0] - truth[0]).powi(2) + (s.pos[1] - truth[1]).powi(2)).sqrt();
            if std::env::var("RF_DEBUG").is_ok() && e > 1.0 {
                eprintln!("corner {i} frame {k}: d=({:+.2},{:+.2}) agreement {:.2}", s.pos[0] - truth[0], s.pos[1] - truth[1], s.score);
            }
            worst = worst.max(e);
        }
    }
    eprintln!("planar on video: worst corner error {worst:.2} px");
    assert!(worst < 1.5, "corners off by up to {worst:.2} px");
}

#[test]
fn planar_refuses_a_blank_shape() {
    let path = std::env::temp_dir().join("rf_track_blank.mp4");
    let ok = Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i", "color=c=0x707070:s=320x240:r=25:d=0.4", "-c:v", "libx264", "-pix_fmt", "yuv420p"])
        .arg(&path)
        .status()
        .is_ok_and(|s| s.success());
    if !ok {
        return;
    }
    let info = probe(&path).unwrap();
    let tr = Tracker::new(TrackMode::Planar, [160.0, 120.0], [320.0, 240.0], Time::ZERO);
    let r = reelforge_media::track_points(&path, &info, &tr, Time::ZERO, Time::from_secs_f64(0.3), &AtomicBool::new(false), |_| {});
    assert!(r.unwrap_err().to_string().contains("too little detail"));
}

// ---- Mesh tracking --------------------------------------------------------------

/// A sheet rippling like a flag (a travelling wave, strongest at its free end)
/// while drifting: where (u, v) of the sheet is at frame k.
fn sheet(k: usize, u: f32, v: f32) -> [f32; 2] {
    let t = k as f32;
    [130.0 + 220.0 * u + 1.5 * t, 60.0 + 150.0 * v + 10.0 * (u * 5.0 - t * 0.25).sin() * u + 0.3 * t * u]
}

/// Write `frames` frames of the rippling sheet to `path`.
fn make_sheet_clip(path: &std::path::Path, frames: usize) -> bool {
    let child = Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error", "-f", "rawvideo", "-pix_fmt", "gray", "-s", &format!("{W}x{H}"), "-r", "25", "-i", "-", "-c:v", "libx264", "-crf", "16", "-pix_fmt", "yuv420p"])
        .arg(path)
        .stdin(std::process::Stdio::piped())
        .spawn();
    let Ok(mut child) = child else { return false };
    let mut stdin = child.stdin.take().unwrap();
    for k in 0..frames {
        let mut img = vec![0u8; W * H];
        for y in 0..H {
            for x in 0..W {
                // Invert the sheet's map with Newton steps.
                let (mut u, mut v) = ((x as f32 - 130.0) / 220.0, (y as f32 - 60.0) / 150.0);
                for _ in 0..8 {
                    let p = sheet(k, u, v);
                    let e = 1e-3;
                    let (pu, pv) = (sheet(k, u + e, v), sheet(k, u, v + e));
                    let (a, b, c, d) = ((pu[0] - p[0]) / e, (pv[0] - p[0]) / e, (pu[1] - p[1]) / e, (pv[1] - p[1]) / e);
                    let det = a * d - b * c;
                    let (rx, ry) = (x as f32 - p[0], y as f32 - p[1]);
                    u += (d * rx - b * ry) / det;
                    v += (-c * rx + a * ry) / det;
                }
                let g = if (0.0..=1.0).contains(&u) && (0.0..=1.0).contains(&v) { 30.0 + 160.0 * noise(u * 30.0, v * 20.0) + 50.0 * noise(u * 80.0 + 5.0, v * 55.0) } else { 70.0 + 0.05 * x as f32 };
                img[y * W + x] = g.clamp(0.0, 255.0) as u8;
            }
        }
        stdin.write_all(&img).unwrap();
    }
    drop(stdin);
    child.wait().is_ok_and(|s| s.success())
}

#[test]
fn mesh_tracks_a_rippling_sheet_on_video() {
    let path = std::env::temp_dir().join("rf_track_mesh.mp4");
    let frames = 30;
    if !make_sheet_clip(&path, frames) {
        eprintln!("ffmpeg CLI not found; skipping");
        return;
    }
    let info = probe(&path).unwrap();
    let mut tr = Tracker::new(TrackMode::Mesh, [240.0, 135.0], [W as f32, H as f32], Time::ZERO);
    tr.set_grid(4, 3, Time::ZERO);
    // Put the grid's points on the sheet (as a user would after dragging its corners).
    for (k, p) in tr.points.iter_mut().enumerate() {
        let (i, j) = (k % 5, k / 5);
        p.point = sheet(0, i as f32 / 4.0, j as f32 / 3.0);
        p.search = 20.0;
    }
    let (samples, end) = reelforge_media::track_points(&path, &info, &tr, Time::ZERO, Time::from_secs_f64((frames - 1) as f64 / 25.0), &AtomicBool::new(false), |_| {}).unwrap();
    assert_eq!(end, TrackEnd::Done);
    // How well the tracked grid lays the sheet out, against the best a 4 × 3 grid can do.
    let (mut sum, mut best_sum, mut worst, mut n) = (0f32, 0f32, 0f32, 0f32);
    for k in 0..frames {
        let tracked = reelforge_core::meshwarp::MeshShape { cols: 4, rows: 3, points: samples.iter().map(|s| s[k].pos).collect() };
        let ideal = reelforge_core::meshwarp::MeshShape { cols: 4, rows: 3, points: (0..20).map(|q| sheet(k, (q % 5) as f32 / 4.0, (q / 5) as f32 / 3.0)).collect() };
        for j in 0..=12 {
            for i in 0..=16 {
                let (gu, gv) = (i as f32 / 4.0, j as f32 / 4.0);
                let cell = [(gu.floor() as u8).min(3), (gv.floor() as u8).min(2)];
                let at = reelforge_core::meshwarp::MeshCoord { cell, f: [gu - cell[0] as f32, gv - cell[1] as f32] };
                let truth = sheet(k, gu / 4.0, gv / 3.0);
                let e = |p: [f32; 2]| ((p[0] - truth[0]).powi(2) + (p[1] - truth[1]).powi(2)).sqrt();
                let (et, eb) = (e(tracked.apply(at)), e(ideal.apply(at)));
                sum += et;
                best_sum += eb;
                worst = worst.max(et);
                n += 1.0;
            }
        }
    }
    let (mean, best) = (sum / n, best_sum / n);
    eprintln!("mesh on video: mean {mean:.2} px (best possible 4×3 grid {best:.2}), worst {worst:.2} px");
    assert!(mean < best + 0.6, "mean {mean:.2} vs {best:.2}");
}

// ---- Mask tracking (rotoscoping) ------------------------------------------------

/// Where point `p` of the disc (as it is at frame 0) is at frame `k`: it
/// drifts right and down, turns 1.2° a frame and grows 0.4 % a frame.
fn disc_at(k: usize, p: [f32; 2]) -> [f32; 2] {
    let c0 = [170.0f32, 130.0];
    let c = [c0[0] + 2.5 * k as f32, c0[1] + 0.8 * k as f32];
    let (s, a) = (1.0 + 0.004 * k as f32, (1.2 * k as f32).to_radians());
    let (dx, dy) = (p[0] - c0[0], p[1] - c0[1]);
    [c[0] + s * (a.cos() * dx - a.sin() * dy), c[1] + s * (a.sin() * dx + a.cos() * dy)]
}

/// A textured disc turning, growing and drifting over a textured background
/// sliding the other way: a mask drawn inside the disc follows the disc (the
/// background, outside the mask, is left out).
#[test]
fn a_mask_follows_what_is_inside_it() {
    use reelforge_core::mask::Mask;
    use reelforge_core::track::{mask_from_region, mask_bounds, MaskMotion};
    use reelforge_core::{Clip, ClipId, MediaId};
    let path = std::env::temp_dir().join("rf_track_mask.mp4");
    let frames = 30;
    let child = Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error", "-f", "rawvideo", "-pix_fmt", "gray", "-s", &format!("{W}x{H}"), "-r", "25", "-i", "-", "-c:v", "libx264", "-crf", "14", "-pix_fmt", "yuv420p"])
        .arg(&path)
        .stdin(std::process::Stdio::piped())
        .spawn();
    let Ok(mut child) = child else {
        eprintln!("ffmpeg CLI not found; skipping");
        return;
    };
    let mut stdin = child.stdin.take().unwrap();
    let radius = 70.0f32;
    for k in 0..frames {
        let (c, s, a) = (disc_at(k, [170.0, 130.0]), 1.0 + 0.004 * k as f32, (1.2 * k as f32).to_radians());
        let mut img = vec![0u8; W * H];
        for y in 0..H {
            for x in 0..W {
                let (dx, dy) = (x as f32 + 0.5 - c[0], y as f32 + 0.5 - c[1]);
                // Back to the disc's frame-0 coordinates.
                let (u, v) = ((a.cos() * dx + a.sin() * dy) / s, (-a.sin() * dx + a.cos() * dy) / s);
                let g = if u * u + v * v < radius * radius {
                    40.0 + 160.0 * noise(u / 6.0 + 50.0, v / 6.0 + 50.0) + 40.0 * noise(u / 2.5, v / 2.5)
                } else {
                    // The background slides left.
                    let bx = x as f32 + 3.0 * k as f32;
                    30.0 + 180.0 * noise(bx / 7.0, y as f32 / 7.0)
                };
                img[y * W + x] = g.clamp(0.0, 255.0) as u8;
            }
        }
        stdin.write_all(&img).unwrap();
    }
    drop(stdin);
    assert!(child.wait().unwrap().success());
    let info = probe(&path).unwrap();

    // A pentagon inside the disc.
    let pts: Vec<[f32; 2]> = (0..5).map(|i| {
        let t = (i as f32 * 72.0 - 90.0).to_radians();
        [170.0 + 55.0 * t.cos(), 130.0 + 55.0 * t.sin()]
    }).collect();
    let mask = Mask { points: pts.clone(), shape: reelforge_core::mask::MaskShape::Freehand, ..Mask::default() };
    let quad = mask_bounds(&mask, Time::ZERO).unwrap();
    let (corners, end) = reelforge_media::track_outline(&path, &info, quad, None, mask.outline(Time::ZERO), 30.0, Time::ZERO, Time::from_secs_f64(1.16), &AtomicBool::new(false), |_| {}).unwrap();
    assert_eq!(end, TrackEnd::Done);
    assert_eq!(corners[0].len(), frames);
    let clip = Clip::new(ClipId(1), MediaId(1), Time::ZERO, Time::ZERO, Time::from_secs_f64(1.2), None);
    for motion in [MaskMotion::Transform, MaskMotion::Perspective] {
        let m = mask_from_region(&clip, &mask, &corners, motion).unwrap();
        assert_eq!(m.keys.len(), frames);
        let mut worst = 0f32;
        for (k, key) in m.keys.iter().enumerate() {
            for (p, q) in pts.iter().zip(&key.points) {
                let truth = disc_at(k, *p);
                worst = worst.max(((q[0] - truth[0]).powi(2) + (q[1] - truth[1]).powi(2)).sqrt());
            }
        }
        eprintln!("mask tracking ({motion:?}): worst point error {worst:.2} px");
        assert!(worst < 2.0, "{motion:?}: mask points off by up to {worst:.2} px");
    }
    // Bending (a grid over the mask) follows the disc too, with the mask right
    // up to the disc's edge, so the background (sliding the other way) fills
    // the grid's outer cells.
    let edge: Vec<[f32; 2]> = (0..12)
        .map(|i| {
            let t = (i as f32 * 30.0).to_radians();
            [170.0 + 68.0 * t.cos(), 130.0 + 68.0 * t.sin()]
        })
        .collect();
    let edge_mask = Mask { points: edge.clone(), shape: reelforge_core::mask::MaskShape::Freehand, ..Mask::default() };
    let eq = mask_bounds(&edge_mask, Time::ZERO).unwrap();
    let (bent, end) = reelforge_media::track_outline(&path, &info, eq, Some(reelforge_core::track::mask_grid(eq)), edge_mask.outline(Time::ZERO), 30.0, Time::ZERO, Time::from_secs_f64(1.16), &AtomicBool::new(false), |_| {}).unwrap();
    assert_eq!(end, TrackEnd::Done);
    let m = mask_from_region(&clip, &edge_mask, &bent, MaskMotion::Bend).unwrap();
    let mut worst = 0f32;
    for (k, key) in m.keys.iter().enumerate() {
        for (p, q) in edge.iter().zip(&key.points) {
            let truth = disc_at(k, *p);
            worst = worst.max(((q[0] - truth[0]).powi(2) + (q[1] - truth[1]).powi(2)).sqrt());
        }
    }
    if std::env::var("RF_DEBUG").is_ok() {
        for (k, key) in m.keys.iter().enumerate() {
            let e: Vec<String> = edge.iter().zip(&key.points).map(|(p, q)| { let t = disc_at(k, *p); format!("{:.1}", ((q[0] - t[0]).powi(2) + (q[1] - t[1]).powi(2)).sqrt()) }).collect();
            eprintln!("frame {k}: {}", e.join(" "));
        }
    }
    eprintln!("mask tracking (Bend) over a sliding background: worst point error {worst:.2} px");
    // Right on the subject's edge half of each feature patch is background, so
    // this is the hard case; a few pixels after 30 frames.
    assert!(worst < 4.5, "Bend: mask points off by up to {worst:.2} px");
    // Position only: the middle follows, the shape doesn't turn.
    let m = mask_from_region(&clip, &mask, &corners, MaskMotion::Position).unwrap();
    let last = &m.keys[frames - 1].points;
    let mid = |p: &[[f32; 2]]| [p.iter().map(|q| q[0]).sum::<f32>() / 5.0, p.iter().map(|q| q[1]).sum::<f32>() / 5.0];
    let (a, b) = (mid(last), disc_at(frames - 1, mid(&pts)));
    assert!((a[0] - b[0]).abs() < 2.0 && (a[1] - b[1]).abs() < 2.0, "{a:?} vs {b:?}");
    assert!((last[0][0] - last[1][0] - (pts[0][0] - pts[1][0])).abs() < 0.01, "kept its shape");
}

/// A mask drawn on the rippling sheet follows it as it bends (each point
/// with what's under it), far closer than any one perspective map can.
#[test]
fn a_bending_mask_follows_a_rippling_sheet() {
    use reelforge_core::mask::{Mask, MaskShape};
    use reelforge_core::track::{mask_bounds, mask_from_region, mask_grid, MaskMotion};
    use reelforge_core::{Clip, ClipId, MediaId};
    let path = std::env::temp_dir().join("rf_track_mesh_mask.mp4");
    let frames = 30;
    if !make_sheet_clip(&path, frames) {
        eprintln!("ffmpeg CLI not found; skipping");
        return;
    }
    let info = probe(&path).unwrap();
    // Eight points round the sheet's middle, in its own (u, v).
    let uv: Vec<[f32; 2]> = (0..8).map(|i| {
        let t = (i as f32 * 45.0).to_radians();
        [0.5 + 0.38 * t.cos(), 0.5 + 0.36 * t.sin()]
    }).collect();
    let pts: Vec<[f32; 2]> = uv.iter().map(|p| sheet(0, p[0], p[1])).collect();
    let mask = Mask { points: pts, shape: MaskShape::Freehand, ..Mask::default() };
    let quad = mask_bounds(&mask, Time::ZERO).unwrap();
    let clip = Clip::new(ClipId(1), MediaId(1), Time::ZERO, Time::ZERO, Time::from_secs_f64(1.2), None);
    let until = Time::from_secs_f64((frames - 1) as f64 / 25.0);
    let err = |m: &Mask| -> (f32, f32) {
        let (mut sum, mut worst, mut n) = (0f32, 0f32, 0f32);
        for (k, key) in m.keys.iter().enumerate() {
            for (q, p) in key.points.iter().zip(&uv) {
                let t = sheet(k, p[0], p[1]);
                let e = ((q[0] - t[0]).powi(2) + (q[1] - t[1]).powi(2)).sqrt();
                (sum, worst, n) = (sum + e, worst.max(e), n + 1.0);
            }
        }
        (sum / n, worst)
    };
    let (bent, end) = reelforge_media::track_outline(&path, &info, quad, Some(mask_grid(quad)), mask.outline(Time::ZERO), 20.0, Time::ZERO, until, &AtomicBool::new(false), |_| {}).unwrap();
    assert_eq!(end, TrackEnd::Done);
    let m = mask_from_region(&clip, &mask, &bent, MaskMotion::Bend).unwrap();
    assert_eq!(m.keys.len(), frames);
    let (mean, worst) = err(&m);
    let (flat, _) = reelforge_media::track_outline(&path, &info, quad, None, mask.outline(Time::ZERO), 20.0, Time::ZERO, until, &AtomicBool::new(false), |_| {}).unwrap();
    let (pmean, pworst) = err(&mask_from_region(&clip, &mask, &flat, MaskMotion::Perspective).unwrap());
    // The best a grid this size can do: its points exactly on the sheet.
    let g0 = mask_grid(quad);
    let inv = |p: [f32; 2]| {
        let (mut u, mut v) = ((p[0] - 130.0) / 220.0, (p[1] - 60.0) / 150.0);
        for _ in 0..20 {
            let q = sheet(0, u, v);
            let e = 1e-3;
            let (pu, pv) = (sheet(0, u + e, v), sheet(0, u, v + e));
            let (a, b, c, d) = ((pu[0] - q[0]) / e, (pv[0] - q[0]) / e, (pu[1] - q[1]) / e, (pv[1] - q[1]) / e);
            let det = a * d - b * c;
            let (rx, ry) = (p[0] - q[0], p[1] - q[1]);
            u += (d * rx - b * ry) / det;
            v += (-c * rx + a * ry) / det;
        }
        [u, v]
    };
    let guv: Vec<[f32; 2]> = g0.points.iter().map(|&p| inv(p)).collect();
    let ideal: Vec<Vec<reelforge_core::track::TrackSample>> =
        guv.iter().map(|q| (0..frames).map(|k| reelforge_core::track::TrackSample { at: Time::from_secs_f64(k as f64 / 25.0), pos: sheet(k, q[0], q[1]), score: 1.0 }).collect()).collect();
    let (imean, iworst) = err(&mask_from_region(&clip, &mask, &ideal, MaskMotion::Bend).unwrap());
    eprintln!("bending mask: mean {mean:.2} px, worst {worst:.2} px (best possible grid {imean:.2}, {iworst:.2}); perspective: mean {pmean:.2}, worst {pworst:.2}");
    assert!(mean < imean + 0.4 && worst < 3.0, "bending mask off by {mean:.2} on average, {worst:.2} at worst");
    assert!(worst < pworst, "bending beats one perspective map");
}

// ---- 3D camera tracking on rendered footage -----------------------------------------

/// Encodes a camera moving past a textured floor and wall (known path and
/// lens), then follows and solves it: the lens and the path come back.
#[test]
fn solves_the_camera_of_encoded_footage() {
    use reelforge_core::camtrack::{fov_of, solve, SolveOptions};
    let (w, h) = (1440usize, 810usize);
    let frames = 30;
    let fov = 45.0f64;
    let f = h as f64 * 0.5 / (fov.to_radians() * 0.5).tan();
    let hash = |x: i64, y: i64| -> f64 {
        let mut v = (x as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (y as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
        v ^= v >> 29;
        v = v.wrapping_mul(0xBF58_476D_1CE4_E5B9);
        v ^= v >> 32;
        (v >> 11) as f64 / (1u64 << 53) as f64
    };
    let noise = |x: f64, y: f64| -> f64 {
        let (mut s, mut amp, mut k) = (0.0, 1.0, 1.0 / 40.0);
        for _ in 0..4 {
            let (fx, fy) = (x * k, y * k);
            let (ix, iy) = (fx.floor(), fy.floor());
            let (tx, ty) = (fx - ix, fy - iy);
            let (sx, sy) = (tx * tx * (3.0 - 2.0 * tx), ty * ty * (3.0 - 2.0 * ty));
            let (ix, iy) = (ix as i64, iy as i64);
            let a = hash(ix, iy) * (1.0 - sx) + hash(ix + 1, iy) * sx;
            let b = hash(ix, iy + 1) * (1.0 - sx) + hash(ix + 1, iy + 1) * sx;
            s += amp * (a * (1.0 - sy) + b * sy);
            amp *= 0.5;
            k *= 2.0;
        }
        s / 1.875
    };
    // Camera centre and the right, down and forward axes for frame k.
    let pose = |k: usize| -> ([f64; 3], [[f64; 3]; 3]) {
        let s = k as f64 / (frames - 1) as f64;
        let c = [-250.0 + 500.0 * s, -150.0, -100.0 + 100.0 * s];
        let t = [60.0 * s, 0.0, 1500.0];
        let n = |v: [f64; 3]| {
            let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
            [v[0] / l, v[1] / l, v[2] / l]
        };
        let fw = n([t[0] - c[0], t[1] - c[1], t[2] - c[2]]);
        let r = n([fw[2], 0.0, -fw[0]]);
        let d = [fw[1] * r[2] - fw[2] * r[1], fw[2] * r[0] - fw[0] * r[2], fw[0] * r[1] - fw[1] * r[0]];
        (c, [r, d, fw])
    };
    let path = std::env::temp_dir().join("rf_camtrack_scene.mp4");
    let child = Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error", "-f", "rawvideo", "-pix_fmt", "gray", "-s", &format!("{w}x{h}"), "-r", "25", "-i", "-", "-c:v", "libx264", "-crf", "16", "-pix_fmt", "yuv420p"])
        .arg(&path)
        .stdin(std::process::Stdio::piped())
        .spawn();
    let Ok(mut child) = child else {
        eprintln!("ffmpeg CLI not found; skipping");
        return;
    };
    let mut stdin = child.stdin.take().unwrap();
    for k in 0..frames {
        let (c, [r, d, fw]) = pose(k);
        let mut img = vec![0u8; w * h];
        for y in 0..h {
            for x in 0..w {
                let (u, v) = ((x as f64 + 0.5 - w as f64 / 2.0) / f, (y as f64 + 0.5 - h as f64 / 2.0) / f);
                let dir = [r[0] * u + d[0] * v + fw[0], r[1] * u + d[1] * v + fw[1], r[2] * u + d[2] * v + fw[2]];
                let mut best = f64::INFINITY;
                let mut g = 0.5;
                if dir[1] > 1e-9 {
                    let t = (200.0 - c[1]) / dir[1];
                    let z = c[2] + dir[2] * t;
                    if t > 0.0 && z < 2500.0 {
                        best = t;
                        g = noise(c[0] + dir[0] * t, z);
                    }
                }
                if dir[2] > 1e-9 {
                    let t = (2500.0 - c[2]) / dir[2];
                    if t > 0.0 && t < best {
                        g = noise(c[0] + dir[0] * t + 5000.0, c[1] + dir[1] * t);
                    }
                }
                img[y * w + x] = (g * 255.0) as u8;
            }
        }
        stdin.write_all(&img).unwrap();
    }
    drop(stdin);
    if !child.wait().is_ok_and(|s| s.success()) {
        eprintln!("ffmpeg failed; skipping");
        return;
    }
    let info = probe(&path).unwrap();
    let mut last = 0.0;
    let until = Time::from_secs_f64((frames - 1) as f64 / 25.0);
    let (tracks, times) = reelforge_media::track_scene(&path, &info, Time::ZERO, until, &AtomicBool::new(false), |p| last = p).unwrap().unwrap();
    assert_eq!(times.len(), frames);
    assert!(last > 0.99);
    assert!(tracks.len() > 100, "{}", tracks.len());
    // Full-size pixels: the features cover the whole frame.
    // Followed at half size, given back at full size (beyond the half-size frame).
    let max = |k: usize| tracks.iter().map(|t| t.pos[0][k]).fold(0.0f32, f32::max);
    assert!(max(0) > 1100.0 && max(1) > 500.0, "{} {}", max(0), max(1));
    let turn = |a: [[f64; 3]; 3], b: [[f64; 3]; 3]| (a[2][0] * b[2][0] + a[2][1] * b[2][1] + a[2][2] * b[2][2]).clamp(-1.0, 1.0).acos().to_degrees();
    let truth = turn(pose(0).1, pose(frames - 1).1);
    let sol = solve(&tracks, frames, [w as f32, h as f32], SolveOptions::default(), &AtomicBool::new(false), &mut |_| {}).unwrap();
    let got = fov_of(sol.focal, h as f64);
    assert!(sol.error < 0.6, "error {}", sol.error);
    // A short sideways move only roughly fixes the lens (nearby lenses with
    // the camera a little nearer or further fit almost as well)…
    assert!((got - fov).abs() < 4.0, "lens {got}° (truth {fov}°)");
    let solved = turn(sol.poses[0].unwrap().r, sol.poses[frames - 1].unwrap().r);
    assert!((truth - solved).abs() < 1.5, "turned {solved}° (truth {truth}°)");
    // …and given the lens, the camera's turn through the shot is exact.
    let known = solve(&tracks, frames, [w as f32, h as f32], SolveOptions { fov: Some(fov as f32), tripod: false }, &AtomicBool::new(false), &mut |_| {}).unwrap();
    let solved = turn(known.poses[0].unwrap().r, known.poses[frames - 1].unwrap().r);
    assert!(known.error < 0.6 && (truth - solved).abs() < 0.2, "known lens: error {}, turned {solved}° (truth {truth}°)", known.error);
    // Cancelling stops early.
    let stop = AtomicBool::new(true);
    assert!(reelforge_media::track_scene(&path, &info, Time::ZERO, until, &stop, |_| {}).unwrap().is_none());
}

/// Two textured spots moving together (a pair of eyes); one is covered over
/// for a few frames (a blink): it's carried along with the other and picked
/// up again, rather than the whole track stopping at the first frame it
/// can't be found.
#[test]
fn a_point_hidden_for_a_few_frames_is_carried_and_found_again() {
    let path = std::env::temp_dir().join("rf_track_blink.mp4");
    let frames = 40usize;
    let child = Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error", "-f", "rawvideo", "-pix_fmt", "gray", "-s", &format!("{W}x{H}"), "-r", "25", "-i", "-", "-c:v", "libx264", "-crf", "12", "-pix_fmt", "yuv420p"])
        .arg(&path)
        .stdin(std::process::Stdio::piped())
        .spawn();
    let Ok(mut child) = child else {
        eprintln!("ffmpeg CLI not found; skipping");
        return;
    };
    // The spots' middles at frame k: moving right 3 px and down 1 px a frame.
    let at = |k: usize, x: f32| [x + 3.0 * k as f32, 110.0 + k as f32];
    let mut stdin = child.stdin.take().unwrap();
    for k in 0..frames {
        let mut img = vec![0u8; W * H];
        for y in 0..H {
            for x in 0..W {
                let mut g = 150.0;
                for (i, cx) in [150.0f32, 290.0].into_iter().enumerate() {
                    let c = at(k, cx);
                    let (u, v) = (x as f32 + 0.5 - c[0], y as f32 + 0.5 - c[1]);
                    if u.abs() < 22.0 && v.abs() < 16.0 {
                        // The left one is shut (plain) on frames 15 to 19.
                        let shut = i == 0 && (15..20).contains(&k);
                        g = if shut { 140.0 } else { 40.0 + 170.0 * noise(u / 4.0 + 10.0 * i as f32, v / 4.0) };
                    }
                }
                img[y * W + x] = g.clamp(0.0, 255.0) as u8;
            }
        }
        stdin.write_all(&img).unwrap();
    }
    drop(stdin);
    assert!(child.wait().unwrap().success());
    let info = probe(&path).unwrap();
    let mut tr = Tracker::new(TrackMode::Transform, [220.0, 110.0], [W as f32, H as f32], Time::ZERO);
    tr.points[0].point = at(0, 150.0);
    tr.points[1].point = at(0, 290.0);
    tr.set_sizes(41.0, 12.0);
    let (paths, end) = reelforge_media::track_points(&path, &info, &tr, Time::ZERO, Time::from_secs_f64(1.56), &AtomicBool::new(false), |_| {}).unwrap();
    assert_eq!(end, TrackEnd::Done, "carried through the blink");
    assert_eq!(paths[0].len(), frames);
    for (k, (a, b)) in paths[0].iter().zip(&paths[1]).enumerate() {
        let (ta, tb) = (at(k, 150.0), at(k, 290.0));
        let ea = (a.pos[0] - ta[0]).hypot(a.pos[1] - ta[1]);
        let eb = (b.pos[0] - tb[0]).hypot(b.pos[1] - tb[1]);
        assert!(eb < 1.0, "frame {k}: the open one {eb:.2} px off");
        // While shut it's carried with the other (to within a pixel or two), and
        // found again exactly once it's back.
        assert!(ea < if (15..20).contains(&k) { 2.5 } else { 1.0 }, "frame {k}: the blinking one {ea:.2} px off");
    }
}
