//! Collect project: copy a project and every file it uses into one folder
//! (the plan is `reelforge_core::collect`), with progress and a way to stop.

use reelforge_core::collect::{same_file, CollectPlan};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

/// What a finished collect did.
#[derive(Clone, Debug)]
pub struct Report {
    pub project_file: PathBuf,
    pub copied: usize,
    /// Already in the folder from an earlier collect (same size and date), so not copied again.
    pub reused: usize,
    pub bytes: u64,
    pub missing: Vec<PathBuf>,
    pub seconds: f64,
}

/// A name that's safe for a file on Windows too.
pub fn safe_name(s: &str) -> String {
    let t: String = s.chars().map(|c| if "<>:\"/\\|?*".contains(c) || c.is_control() { '_' } else { c }).collect();
    let t = t.trim().trim_end_matches('.').to_string();
    if t.is_empty() { "project".into() } else { t }
}

/// Copy one file, `on_bytes` told after every chunk. Returns false if an
/// identical copy was already there (nothing copied). The copy goes to a
/// temporary name first, so a stopped or failed copy never leaves a
/// half-written file under the real name; it keeps the source's date.
pub fn copy_file(from: &Path, to: &Path, cancel: &AtomicBool, mut on_bytes: impl FnMut(u64)) -> std::io::Result<bool> {
    if to.exists() && same_file(from, to) {
        on_bytes(std::fs::metadata(to)?.len());
        return Ok(false);
    }
    if let Some(dir) = to.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let part = to.with_file_name(format!("{}.part", to.file_name().unwrap_or_default().to_string_lossy()));
    let result = (|| -> std::io::Result<()> {
        let mut src = std::fs::File::open(from)?;
        let mut dst = std::fs::File::create(&part)?;
        let mut buf = vec![0u8; 4 << 20];
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(std::io::Error::new(std::io::ErrorKind::Interrupted, "stopped"));
            }
            let n = src.read(&mut buf)?;
            if n == 0 {
                break;
            }
            dst.write_all(&buf[..n])?;
            on_bytes(n as u64);
        }
        dst.sync_all()?;
        if let Ok(t) = std::fs::metadata(from).and_then(|m| m.modified()) {
            let _ = dst.set_modified(t);
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            if to.exists() {
                std::fs::remove_file(to)?;
            }
            std::fs::rename(&part, to)?;
            Ok(true)
        }
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            Err(e)
        }
    }
}

/// Carry out `plan`: copy every file, then write the project (with paths
/// relative to it). `progress` gets the bytes done so far, the file and its index.
pub fn run_collect(plan: &CollectPlan, cancel: &AtomicBool, mut progress: impl FnMut(u64, &str, usize)) -> Result<Report, String> {
    let started = Instant::now();
    std::fs::create_dir_all(&plan.dest).map_err(|e| format!("can't make the folder {}: {e}", plan.dest.display()))?;
    let (mut done, mut copied, mut reused) = (0u64, 0, 0);
    for (i, item) in plan.items.iter().enumerate() {
        let label = item.to.to_string_lossy().replace('\\', "/");
        progress(done, &label, i);
        let base = done;
        let mut moved = 0u64;
        let to = plan.dest.join(&item.to);
        match copy_file(&item.from, &to, cancel, |n| {
            moved += n;
            progress(base + moved, &label, i);
        }) {
            Ok(true) => copied += 1,
            Ok(false) => reused += 1,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => return Err("stopped: the files copied so far are in the folder, but no project was written".into()),
            Err(e) => return Err(format!("couldn't copy {}: {e}", item.from.display())),
        }
        done = base + item.size;
    }
    let json = serde_json::to_string_pretty(&plan.project.paths_relative_to(&plan.dest)).map_err(|e| e.to_string())?;
    crate::safety::write_atomic(&plan.project_file, json.as_bytes()).map_err(|e| format!("couldn't write {}: {e}", plan.project_file.display()))?;
    Ok(Report { project_file: plan.project_file.clone(), copied, reused, bytes: done, missing: plan.missing.clone(), seconds: started.elapsed().as_secs_f64() })
}


#[cfg(test)]
mod tests {
    use super::*;
    use reelforge_core::collect::{plan_collect, CollectOptions};
    use reelforge_core::{Clip, ClipId, MediaAsset, MediaId, MediaInfo, Project, Sequence, Time};
    use std::sync::atomic::AtomicBool;

    #[test]
    fn collects_copies_and_the_moved_folder_opens() {
        let root = std::env::temp_dir().join(format!("rf_collect_engine_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (src, dest, moved) = (root.join("src"), root.join("out"), root.join("moved"));
        std::fs::create_dir_all(&src).unwrap();
        let data: Vec<u8> = (0..9_000_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(src.join("big.mov"), &data).unwrap();
        let mut p = Project::new();
        let info = MediaInfo { duration: Time::ZERO, video: None, audio: None, container: String::new() };
        p.media = vec![MediaAsset { id: MediaId(1), path: src.join("big.mov"), name: "big.mov".into(), info, conform: None }];
        let mut seq = Sequence::new("S", 640, 360, reelforge_core::FrameRate::new(25, 1));
        seq.tracks[0].clips.push(Clip::new(ClipId(5), MediaId(1), Time::ZERO, Time::ZERO, Time::from_secs_f64(1.0), None));
        p.sequences = vec![seq];

        let plan = plan_collect(&p, &dest, &safe_name("My: film?"), CollectOptions::default());
        assert_eq!(plan.project_file, dest.join("My_ film_.rfproj"));
        let mut calls = 0;
        let rep = run_collect(&plan, &AtomicBool::new(false), |_, _, _| calls += 1).unwrap();
        assert_eq!((rep.copied, rep.reused, rep.bytes), (1, 0, 9_000_000));
        assert!(calls >= 3, "progress during the copy");
        assert_eq!(std::fs::read(dest.join("Media/big.mov")).unwrap(), data);
        assert!(!dest.join("Media/big.mov.part").exists());
        // Collecting again reuses the copy.
        let rep = run_collect(&plan_collect(&p, &dest, "My_ film_", CollectOptions::default()), &AtomicBool::new(false), |_, _, _| {}).unwrap();
        assert_eq!((rep.copied, rep.reused), (0, 1));
        // Move the whole folder, open the project: the media is found there.
        std::fs::rename(&dest, &moved).unwrap();
        let q = crate::editor::load_project(&moved.join("My_ film_.rfproj")).unwrap();
        assert_eq!(q.media[0].path, moved.join("Media").join("big.mov"));
        assert!(q.media[0].path.exists());
        // A stopped copy leaves no half-written file.
        let stop = AtomicBool::new(true);
        let err = copy_file(&src.join("big.mov"), &root.join("x/big.mov"), &stop, |_| {}).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::Interrupted);
        assert!(!root.join("x/big.mov").exists() && !root.join("x/big.mov.part").exists());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
