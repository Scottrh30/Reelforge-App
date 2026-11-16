//! Converting R3D and ARRIRAW footage with their makers' tools (REDline, ARRI Reference Tool)
//! on a background thread, one clip at a time, so it can be edited (see
//! `reelforge_media::camraw`). Converted files are found by source file, so they carry over
//! between projects.

use crossbeam_channel::{unbounded, Receiver, Sender};
use reelforge_core::Project;
use reelforge_media::camraw;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq)]
pub enum ConvertState {
    Queued,
    Converting(f32),
    Done(PathBuf),
    Failed(String),
}

/// A conversion that just finished.
pub struct Finished {
    pub src: PathBuf,
    pub result: Result<PathBuf, String>,
    /// Asked for by an import: add the footage to the project now.
    pub import: bool,
}

pub struct Conversions {
    states: HashMap<PathBuf, ConvertState>,
    jobs: Sender<PathBuf>,
    updates: Receiver<(PathBuf, ConvertState)>,
    stop: Arc<AtomicBool>,
    import_when_done: HashSet<PathBuf>,
}

impl Default for Conversions {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Conversions {
    fn drop(&mut self) {
        // Stops the tool that's running (and the worker).
        self.stop.store(true, Ordering::Relaxed);
    }
}

impl Conversions {
    pub fn new() -> Self {
        let (jobs, job_rx) = unbounded::<PathBuf>();
        let (tx, updates) = unbounded();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_w = stop.clone();
        let spawned = std::thread::Builder::new().name("camera-raw".into()).spawn(move || {
            while let Ok(src) = job_rx.recv() {
                if stop_w.load(Ordering::Relaxed) {
                    break;
                }
                let _ = tx.send((src.clone(), ConvertState::Converting(0.0)));
                let mut last = Instant::now();
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    camraw::convert(&src, &stop_w, |p| {
                        if last.elapsed() > Duration::from_millis(500) {
                            last = Instant::now();
                            let _ = tx.send((src.clone(), ConvertState::Converting(p)));
                        }
                    })
                }));
                let state = match result {
                    Ok(Ok(path)) => ConvertState::Done(path),
                    Ok(Err(e)) => ConvertState::Failed(format!("{e:#}")),
                    Err(_) => ConvertState::Failed("the conversion stopped unexpectedly".into()),
                };
                let _ = tx.send((src, state));
            }
        });
        if let Err(e) = spawned {
            log::error!("couldn't start the camera RAW thread: {e}");
        }
        Self { states: HashMap::new(), jobs, updates, stop, import_when_done: HashSet::new() }
    }

    /// Queue `src` for converting (unless it's converted or on the way). With `import`, it's
    /// added to the project when done.
    pub fn request(&mut self, src: &Path, import: bool) {
        if import {
            self.import_when_done.insert(src.to_owned());
        }
        if matches!(self.states.get(src), Some(ConvertState::Queued | ConvertState::Converting(_))) {
            return;
        }
        self.states.insert(src.to_owned(), ConvertState::Queued);
        let _ = self.jobs.send(src.to_owned());
    }

    /// Queue the project's R3D and ARRIRAW footage that isn't converted (a project opened on
    /// another computer, or a cleared cache). Ones that failed aren't retried by themselves.
    pub fn refresh(&mut self, project: &Project) {
        for m in &project.media {
            if self.states.contains_key(&m.path) || !m.path.exists() {
                continue;
            }
            if let Some(kind) = camraw::needs_conversion(&m.path) {
                if camraw::tool_for(kind).is_some() {
                    self.request(&m.path, false);
                }
            }
        }
    }

    /// Take in progress reports; returns the conversions that finished.
    pub fn poll(&mut self) -> Vec<Finished> {
        let mut done = Vec::new();
        while let Ok((src, state)) = self.updates.try_recv() {
            let result = match &state {
                ConvertState::Done(p) => Some(Ok(p.clone())),
                ConvertState::Failed(e) => Some(Err(e.clone())),
                _ => None,
            };
            if let Some(result) = result {
                let import = self.import_when_done.remove(&src);
                done.push(Finished { src: src.clone(), result, import });
            }
            self.states.insert(src, state);
        }
        done
    }

    pub fn state(&self, src: &Path) -> Option<&ConvertState> {
        self.states.get(src)
    }

    /// The conversion running now: its source and progress.
    pub fn current(&self) -> Option<(&Path, f32)> {
        self.states.iter().find_map(|(k, s)| match s {
            ConvertState::Converting(p) => Some((k.as_path(), *p)),
            _ => None,
        })
    }

    /// Is any conversion queued or running (so the interface keeps updating)?
    pub fn busy(&self) -> bool {
        self.states.values().any(|s| matches!(s, ConvertState::Queued | ConvertState::Converting(_)))
    }

    /// Forget a file's state (to retry a failed conversion).
    pub fn forget(&mut self, src: &Path) {
        self.states.remove(src);
    }
}
