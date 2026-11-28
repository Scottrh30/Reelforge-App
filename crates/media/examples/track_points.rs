//! Runs Reelforge's point tracker over a clip from the command line, for
//! measuring how well it follows features (`cargo run -p reelforge-media
//! --example track_points -- CLIP OUT.json x,y [x,y…] [--size N] [--search N]
//! [--template] [--from SECS] [--until SECS]`). Writes every point's tracked
//! path (media pixels, with each frame's match score) and how the run ended.
use reelforge_core::track::{TrackMatch, TrackMode, Tracker};
use reelforge_core::Time;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (Some(clip), Some(out)) = (args.first(), args.get(1)) else {
        anyhow::bail!("usage: track_points CLIP OUT.json x,y [x,y…] [--size N] [--search N] [--template] [--from S] [--until S]");
    };
    let mut points = Vec::new();
    let (mut size, mut search, mut template) = (31.0f32, 24.0f32, false);
    let (mut from, mut until) = (0.0f64, f64::MAX);
    let mut i = 2;
    while let Some(a) = args.get(i) {
        let next = || args.get(i + 1).and_then(|v| v.parse::<f64>().ok());
        match a.as_str() {
            "--size" => (size, i) = (next().unwrap_or(31.0) as f32, i + 1),
            "--search" => (search, i) = (next().unwrap_or(24.0) as f32, i + 1),
            "--from" => (from, i) = (next().unwrap_or(0.0), i + 1),
            "--until" => (until, i) = (next().unwrap_or(f64::MAX), i + 1),
            "--template" => template = true,
            p => {
                let v: Vec<f32> = p.split(',').filter_map(|s| s.parse().ok()).collect();
                if let [x, y] = v[..] {
                    points.push([x, y]);
                }
            }
        }
        i += 1;
    }
    let path = PathBuf::from(clip);
    let info = reelforge_media::probe(&path)?;
    let mode = match points.len() {
        1 => TrackMode::Position,
        2 => TrackMode::Transform,
        4 => TrackMode::CornerPin,
        n => anyhow::bail!("{n} points: give 1, 2 or 4"),
    };
    let start = Time::from_secs_f64(from);
    let mut t = Tracker::new(mode, points[0], [1.0, 1.0], start);
    for (p, q) in t.points.iter_mut().zip(&points) {
        p.point = *q;
    }
    t.set_sizes(size, search);
    if template {
        t.matching = TrackMatch::Template;
    }
    let end = Time::from_secs_f64(until.min(info.duration.as_secs_f64()));
    let started = std::time::Instant::now();
    let (paths, ended) = reelforge_media::track_points(&path, &info, &t, start, end, &AtomicBool::new(false), |_| {})?;
    let secs = started.elapsed().as_secs_f64();
    let json = serde_json::json!({
        "ended": format!("{ended:?}"),
        "seconds": secs,
        "points": paths.iter().map(|p| p.iter().map(|s| serde_json::json!([s.at.as_secs_f64(), s.pos[0], s.pos[1], s.score])).collect::<Vec<_>>()).collect::<Vec<_>>(),
    });
    std::fs::write(out, serde_json::to_string(&json)?)?;
    eprintln!("{ended:?} after {} frames in {secs:.1} s", paths.first().map_or(0, |p| p.len()));
    Ok(())
}
