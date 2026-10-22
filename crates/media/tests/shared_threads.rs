//! Clips decoding together share one pool of worker threads. This drives a
//! dozen clips of every codec with Reelforge's own decoder at once, from their
//! own threads, and fails if any of them stalls (a deadlock in the shared pool).
//! Run it with a tiny pool too: RAYON_NUM_THREADS=1 (and 2) is the hardest case.
//! Needs the `ffmpeg` CLI to make the clips; skips without it.

use reelforge_core::{FrameRate, Time};
use reelforge_media::{probe, FrameSource};
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

fn clip(name: &str, args: &[&str]) -> Option<PathBuf> {
    let dir = std::env::temp_dir().join(format!("rf-shared-{}", std::process::id()));
    std::fs::create_dir_all(&dir).ok()?;
    let p = dir.join(name);
    let ok = Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i", "testsrc2=size=640x360:rate=25:duration=2"])
        .args(args)
        .arg(&p)
        .status()
        .is_ok_and(|s| s.success());
    ok.then_some(p)
}

fn threads_now() -> usize {
    std::fs::read_dir("/proc/self/task").map_or(0, |d| d.count())
}

#[test]
fn many_clips_share_the_workers_without_stalling() {
    let kinds: [(&str, &[&str]); 7] = [
        ("a.mp4", &["-c:v", "libx264", "-g", "12", "-bf", "2", "-pix_fmt", "yuv420p"]),
        ("b.mp4", &["-c:v", "libx265", "-g", "12", "-bf", "2", "-pix_fmt", "yuv420p", "-x265-params", "log-level=error"]),
        ("c.mkv", &["-c:v", "libaom-av1", "-cpu-used", "8", "-g", "12", "-pix_fmt", "yuv420p"]),
        ("d.webm", &["-c:v", "libvpx-vp9", "-deadline", "realtime", "-cpu-used", "8", "-g", "12"]),
        ("e.mov", &["-c:v", "prores_ks", "-profile:v", "3"]),
        ("f.mov", &["-c:v", "dnxhd", "-profile:v", "dnxhr_hq", "-pix_fmt", "yuv422p"]),
        ("g.mpg", &["-c:v", "mpeg2video", "-g", "12", "-bf", "2", "-b:v", "8M"]),
    ];
    let mut files = Vec::new();
    for (name, args) in kinds {
        match clip(name, args) {
            Some(p) => files.push(p),
            None => eprintln!("couldn't make {name} (no encoder?); left out"),
        }
    }
    if files.len() < 3 {
        eprintln!("ffmpeg CLI not found; skipping");
        return;
    }
    let before = threads_now();
    let pool = rayon::current_num_threads();
    // Each clip twice (12-14 decoders), every one from its own thread, each
    // seeking about so whole GOPs decode at once.
    let start = Instant::now();
    let handles: Vec<_> = files
        .iter()
        .chain(files.iter())
        .enumerate()
        .map(|(k, path)| {
            let path = path.clone();
            std::thread::spawn(move || {
                let rate = FrameRate::new(25, 1);
                let mut src = FrameSource::open(&path, None).unwrap();
                let order: Vec<i64> = if k % 2 == 0 { (0..50).collect() } else { [40, 3, 47, 11, 29, 0, 35, 18].into_iter().collect() };
                for f in order {
                    let fr = src.frame_at_blocking(Time::from_frame(f, rate), Duration::from_secs(60)).unwrap_or_else(|| panic!("{}: no frame {f}", path.display()));
                    assert_eq!(fr.pts.to_frame(rate), f, "{}", path.display());
                }
                threads_now()
            })
        })
        .collect();
    // A watchdog: everything should finish in well under this, even on one worker.
    let deadline = Instant::now() + Duration::from_secs(300);
    let mut peak = 0;
    for h in handles {
        while !h.is_finished() {
            assert!(Instant::now() < deadline, "decoding stalled: a deadlock in the shared worker pool?");
            std::thread::sleep(Duration::from_millis(20));
        }
        peak = peak.max(h.join().expect("a decoding thread failed"));
    }
    for p in &files {
        assert!(probe(p).is_ok());
    }
    eprintln!(
        "{} decoders on a pool of {pool}: {:.1} s; threads: {before} before, at most {peak} while decoding",
        files.len() * 2,
        start.elapsed().as_secs_f64()
    );
    std::fs::remove_dir_all(files[0].parent().unwrap()).ok();
}
