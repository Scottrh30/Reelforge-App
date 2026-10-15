//! Not losing work: autosave, crash recovery, safe saving, and relinking
//! media that has moved.
//!
//! **Autosave.** Every few minutes, if anything changed since the last save
//! or autosave, the project is written to the autosave folder (one file per
//! editing session, replaced each time). Writes go to a temporary file that
//! is then renamed over the old one, so a crash mid-write never leaves a
//! half-written file behind, for autosaves and normal saves alike.
//!
//! **Crash detection.** Each running Reelforge holds an OS lock on a small
//! file in the sessions folder and deletes it when it closes normally. On
//! start, a session file whose lock nobody holds belongs to a run that
//! crashed (or was killed), and its autosave is offered for recovery.

use reelforge_core::Project;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

pub fn autosave_dir() -> PathBuf {
    reelforge_media::proxy::cache_dir().join("autosave")
}

fn sessions_dir() -> PathBuf {
    reelforge_media::proxy::cache_dir().join("sessions")
}

/// Write `data` to `path` without ever leaving a partial file: write a
/// temporary file next to it, flush it to disk, then rename it into place.
pub fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let tmp = path.with_extension(format!("{}.saving", path.extension().and_then(|e| e.to_str()).unwrap_or("tmp")));
    {
        let mut f = File::create(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// What a session records about itself, for recovery after a crash.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionInfo {
    /// The autosave file of this session.
    pub autosave: PathBuf,
    /// The project file being edited, if it had been saved.
    pub project: Option<PathBuf>,
    pub started: SystemTime,
}

/// A crashed session found at startup.
#[derive(Clone, Debug)]
pub struct Recovery {
    pub info: SessionInfo,
    /// When its autosave was last written.
    pub saved: SystemTime,
    json: PathBuf,
    lock: PathBuf,
}

impl Recovery {
    /// Forget this session (after recovering it or declining).
    pub fn dismiss(&self, keep_autosave: bool) {
        let _ = std::fs::remove_file(&self.json);
        let _ = std::fs::remove_file(&self.lock);
        if !keep_autosave {
            let _ = std::fs::remove_file(&self.info.autosave);
        }
    }

    pub fn load(&self) -> anyhow::Result<Project> {
        Ok(serde_json::from_str(&std::fs::read_to_string(&self.info.autosave)?)?)
    }

    pub fn describe(&self) -> String {
        let name = self.info.project.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "an unsaved project".into());
        let ago = SystemTime::now().duration_since(self.saved).unwrap_or_default();
        format!("{name}, autosaved {} ago", human_duration(ago))
    }
}

pub fn human_duration(d: Duration) -> String {
    let s = d.as_secs();
    if s < 90 {
        format!("{s} s")
    } else if s < 90 * 60 {
        format!("{} min", (s + 30) / 60)
    } else if s < 48 * 3600 {
        format!("{} h", (s + 1800) / 3600)
    } else {
        format!("{} days", s / 86400)
    }
}

/// This run's session: its lock, its autosave file, and when to save next.
pub struct Session {
    id: String,
    _lock: Option<File>,
    pub info: SessionInfo,
    last_autosave: Instant,
    /// Project revision at the last save or autosave.
    pub autosaved_rev: u64,
}

impl Session {
    /// Register this run. Also returns sessions that crashed earlier and left an autosave.
    pub fn start() -> (Self, Vec<Recovery>) {
        let dir = sessions_dir();
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::create_dir_all(autosave_dir());
        let crashed = find_crashed(&dir);
        let started = SystemTime::now();
        let stamp = started.duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default().as_millis();
        let id = format!("{}-{stamp}", std::process::id());
        let lock = File::create(dir.join(format!("{id}.lock"))).ok().filter(|f| f.try_lock().is_ok());
        let info = SessionInfo { autosave: autosave_dir().join(format!("session-{id}.rfproj")), project: None, started };
        let s = Self { id, _lock: lock, info, last_autosave: Instant::now(), autosaved_rev: 0 };
        s.write_info();
        prune_autosaves(20);
        (s, crashed)
    }

    fn write_info(&self) {
        if let Ok(json) = serde_json::to_vec_pretty(&self.info) {
            let _ = write_atomic(&sessions_dir().join(format!("{}.json", self.id)), &json);
        }
    }

    /// Remember which project file this session is editing.
    pub fn set_project(&mut self, path: Option<PathBuf>) {
        if self.info.project != path {
            self.info.project = path;
            self.write_info();
        }
    }

    /// Autosave if it's time and something changed. Returns a status message when it saved.
    pub fn tick(&mut self, project: &Project, rev: u64, minutes: u32) -> Option<String> {
        if minutes == 0 || rev == self.autosaved_rev || self.last_autosave.elapsed() < Duration::from_secs(minutes as u64 * 60) {
            return None;
        }
        self.last_autosave = Instant::now();
        let json = serde_json::to_vec(project).ok()?;
        match write_atomic(&self.info.autosave, &json) {
            Ok(()) => {
                self.autosaved_rev = rev;
                Some("Autosaved".into())
            }
            Err(e) => Some(format!("Autosave failed: {e}")),
        }
    }

    /// A clean exit: nothing to recover.
    pub fn end(&self) {
        let dir = sessions_dir();
        let _ = std::fs::remove_file(dir.join(format!("{}.json", self.id)));
        let _ = std::fs::remove_file(dir.join(format!("{}.lock", self.id)));
        let _ = std::fs::remove_file(&self.info.autosave);
    }
}

/// Sessions whose lock file isn't held any more but which didn't clean up.
fn find_crashed(dir: &Path) -> Vec<Recovery> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out = Vec::new();
    for e in rd.flatten() {
        let json = e.path();
        if json.extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        let lock = json.with_extension("lock");
        // If we can lock it, its owner is gone.
        let alive = File::options().write(true).create(true).truncate(false).open(&lock).ok().is_some_and(|f| f.try_lock().is_err());
        if alive {
            continue;
        }
        let Some(info) = std::fs::read_to_string(&json).ok().and_then(|s| serde_json::from_str::<SessionInfo>(&s).ok()) else {
            let _ = std::fs::remove_file(&json);
            let _ = std::fs::remove_file(&lock);
            continue;
        };
        match std::fs::metadata(&info.autosave).and_then(|m| m.modified()) {
            Ok(saved) => out.push(Recovery { info, saved, json, lock }),
            Err(_) => {
                // Crashed before its first autosave: nothing to offer.
                let _ = std::fs::remove_file(&json);
                let _ = std::fs::remove_file(&lock);
            }
        }
    }
    out.sort_by_key(|r| std::cmp::Reverse(r.saved));
    out
}

/// Keep only the newest `keep` autosave files.
fn prune_autosaves(keep: usize) {
    let Ok(rd) = std::fs::read_dir(autosave_dir()) else { return };
    let mut files: Vec<(SystemTime, PathBuf)> = rd
        .flatten()
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .filter(|(_, p)| p.extension().and_then(|x| x.to_str()) == Some("rfproj"))
        .collect();
    files.sort_by_key(|(t, _)| std::cmp::Reverse(*t));
    for (_, p) in files.into_iter().skip(keep) {
        let _ = std::fs::remove_file(p);
    }
}

// ---- Relinking --------------------------------------------------------------

/// Look for files named like each missing one in `dir` (and its subfolders,
/// a few levels deep). Returns `(missing path, found path)` pairs.
pub fn find_by_name(dir: &Path, missing: &[PathBuf]) -> Vec<(PathBuf, PathBuf)> {
    let wanted: Vec<(String, &PathBuf)> = missing
        .iter()
        .filter_map(|p| Some((p.file_name()?.to_string_lossy().to_lowercase(), p)))
        .collect();
    let mut found = Vec::new();
    let mut stack = vec![(dir.to_path_buf(), 0)];
    let mut visited = 0;
    while let Some((d, depth)) = stack.pop() {
        visited += 1;
        if visited > 5000 {
            break; // don't wander a whole drive
        }
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                if depth < 4 {
                    stack.push((p, depth + 1));
                }
            } else if let Some(name) = p.file_name().map(|n| n.to_string_lossy().to_lowercase()) {
                for (w, orig) in &wanted {
                    if *w == name && !found.iter().any(|(o, _): &(PathBuf, PathBuf)| o == *orig) {
                        found.push(((*orig).clone(), p.clone()));
                    }
                }
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_replaces_whole_files() {
        let dir = std::env::temp_dir().join(format!("rf-safety-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("p.rfproj");
        write_atomic(&p, b"first").unwrap();
        write_atomic(&p, b"second").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"second");
        assert!(std::fs::read_dir(&dir).unwrap().count() == 1, "no temporary files left");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn finds_moved_media_by_name_in_subfolders() {
        let dir = std::env::temp_dir().join(format!("rf-relink-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("a/b")).unwrap();
        std::fs::write(dir.join("a/b/Clip One.MP4"), b"x").unwrap();
        std::fs::write(dir.join("other.mp4"), b"x").unwrap();
        let missing = vec![PathBuf::from("/old/place/clip one.mp4"), PathBuf::from("/old/gone.mov")];
        let found = find_by_name(&dir, &missing);
        assert_eq!(found, vec![(missing[0].clone(), dir.join("a/b/Clip One.MP4"))]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn crashed_sessions_are_found_and_live_ones_are_not() {
        // A fake session in a private folder: no lock holder, with an autosave → crashed.
        let dir = std::env::temp_dir().join(format!("rf-sessions-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let autosave = dir.join("s.rfproj");
        std::fs::write(&autosave, serde_json::to_vec(&Project::new()).unwrap()).unwrap();
        let info = SessionInfo { autosave: autosave.clone(), project: Some("/x/My film.rfproj".into()), started: SystemTime::now() };
        std::fs::write(dir.join("1-1.json"), serde_json::to_vec(&info).unwrap()).unwrap();
        // A live one: its lock is held.
        std::fs::write(dir.join("2-2.json"), serde_json::to_vec(&info).unwrap()).unwrap();
        let held = File::create(dir.join("2-2.lock")).unwrap();
        held.try_lock().unwrap();
        let found = find_crashed(&dir);
        assert_eq!(found.len(), 1);
        assert!(found[0].describe().starts_with("My film.rfproj, autosaved"));
        assert!(found[0].load().is_ok());
        found[0].dismiss(false);
        assert!(!autosave.exists() && !dir.join("1-1.json").exists());
        drop(held);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
