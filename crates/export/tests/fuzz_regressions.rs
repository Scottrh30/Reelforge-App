//! Project and media files the fuzzer found that once crashed, hung or ran
//! away with memory (`fuzz/regressions/{prproj,aep,project,media}/*`). Each is
//! read and walked the way `fuzz/src/files.rs` does; the test fails if any
//! panics or takes more than 20 s (the hangs). Run the fuzzers with
//! `fuzz/run.sh` (see `fuzz/README.md`).

#![allow(dead_code)]

include!("../../../fuzz/src/files.rs");

use std::sync::atomic::{AtomicUsize, Ordering};

static PANICS: AtomicUsize = AtomicUsize::new(0);

/// Resident memory (MB), where the system says (Linux).
fn resident_mb() -> Option<u64> {
    let s = std::fs::read_to_string("/proc/self/statm").ok()?;
    Some(s.split_whitespace().nth(1)?.parse::<u64>().ok()? * 4 / 1024)
}

#[test]
fn damaged_project_and_media_files_the_fuzzer_found_are_survived() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/regressions");
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        PANICS.fetch_add(1, Ordering::SeqCst);
        prev(info);
    }));
    let mut failed = Vec::new();
    let mut ran = 0;
    for target in ["prproj", "aep", "project", "media"] {
        let Ok(dir) = std::fs::read_dir(root.join(target)) else { continue };
        let mut files: Vec<_> = dir.flatten().map(|e| e.path()).collect();
        files.sort();
        for path in files {
            let Ok(data) = std::fs::read(&path) else { continue };
            let before = PANICS.load(Ordering::SeqCst);
            let start = std::time::Instant::now();
            let ok = std::panic::catch_unwind(|| match target {
                "prproj" => run_prproj(&data),
                "aep" => run_aep(&data),
                "project" => run_project(&data),
                _ => run_media(&data),
            })
            .is_ok();
            let took = start.elapsed();
            ran += 1;
            // Media files are read again and again, as the app does (every
            // open of a clip, every scrub), so memory kept per read shows.
            if target == "media" && ok {
                let before = resident_mb();
                for _ in 0..10 {
                    run_media(&data);
                }
                if let (Some(a), Some(b)) = (before, resident_mb()) {
                    if b > a + 200 {
                        failed.push(format!("{target}/{}: memory grew {} MB over 10 reads", path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), b - a));
                    }
                }
            }
            if !ok || PANICS.load(Ordering::SeqCst) != before || took.as_secs() > 20 {
                failed.push(format!("{target}/{} ({took:?})", path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()));
            }
        }
    }
    assert!(ran > 0, "no regression files under {}", root.display());
    assert!(failed.is_empty(), "these still crash or hang: {failed:#?}");
}
