//! Keeps track of editing proxies and makes them on a background thread, one
//! at a time, so the editor stays responsive. Proxies are found by source
//! file, so they carry over between projects.

use crossbeam_channel::{unbounded, Receiver, Sender};
use reelforge_core::{MediaInfo, Project};
use reelforge_media::proxy;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq)]
pub enum ProxyState {
    Queued,
    Building(f32),
    Ready(PathBuf),
    Failed(String),
}

struct Job {
    src: PathBuf,
    info: MediaInfo,
}

pub struct Proxies {
    states: HashMap<PathBuf, ProxyState>,
    jobs: Sender<Job>,
    updates: Receiver<(PathBuf, ProxyState)>,
    /// Set to stop the job in progress (and the worker) when the app closes.
    stop: Arc<AtomicBool>,
    /// Files whose proxy was deleted by hand: not made again automatically.
    declined: std::collections::HashSet<PathBuf>,
}

impl Default for Proxies {
    fn default() -> Self {
        Self::new()
    }
}

impl Proxies {
    pub fn new() -> Self {
        let (jobs, job_rx) = unbounded::<Job>();
        let (tx, updates) = unbounded();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_w = stop.clone();
        std::thread::Builder::new()
            .name("proxies".into())
            .spawn(move || {
                while let Ok(job) = job_rx.recv() {
                    if stop_w.load(Ordering::Relaxed) {
                        break;
                    }
                    let _ = tx.send((job.src.clone(), ProxyState::Building(0.0)));
                    let mut last = Instant::now();
                    let result = proxy::make_proxy(&job.src, &job.info, &stop_w, |p| {
                        if last.elapsed() > Duration::from_millis(250) {
                            last = Instant::now();
                            let _ = tx.send((job.src.clone(), ProxyState::Building(p)));
                        }
                    });
                    let state = match result {
                        Ok(path) => ProxyState::Ready(path),
                        Err(e) => ProxyState::Failed(format!("{e:#}")),
                    };
                    let _ = tx.send((job.src, state));
                }
            })
            .expect("proxy thread");
        Self { states: HashMap::new(), jobs, updates, stop, declined: Default::default() }
    }

    /// Note which files already have proxies, and queue new ones for heavy
    /// footage if `auto`. Cheap to call often.
    pub fn refresh(&mut self, project: &Project, auto: bool) {
        for m in &project.media {
            if m.info.video.is_none() || self.states.contains_key(&m.path) || !m.path.exists() {
                continue;
            }
            if let Some(p) = proxy::existing_proxy(&m.path) {
                self.states.insert(m.path.clone(), ProxyState::Ready(p));
            } else if auto && proxy::worth_a_proxy(&m.info) && !self.declined.contains(&m.path) {
                self.request(&m.path, &m.info);
            }
        }
    }

    /// Queue a proxy for `src` (unless it has one or one is on the way).
    pub fn request(&mut self, src: &Path, info: &MediaInfo) {
        if matches!(self.states.get(src), Some(ProxyState::Queued | ProxyState::Building(_) | ProxyState::Ready(_))) {
            return;
        }
        self.declined.remove(src);
        self.states.insert(src.to_owned(), ProxyState::Queued);
        let _ = self.jobs.send(Job { src: src.to_owned(), info: info.clone() });
    }

    /// Take in progress reports. Returns true when a proxy has just become ready.
    pub fn poll(&mut self) -> bool {
        let mut ready = false;
        while let Ok((src, state)) = self.updates.try_recv() {
            ready |= matches!(state, ProxyState::Ready(_));
            self.states.insert(src, state);
        }
        ready
    }

    pub fn state(&self, src: &Path) -> Option<&ProxyState> {
        self.states.get(src)
    }

    /// The proxy file for `src`, if it's ready.
    /// How many proxies are ready (changes when one finishes).
    pub fn ready_count(&self) -> usize {
        self.states.values().filter(|s| matches!(s, ProxyState::Ready(_))).count()
    }

    pub fn ready(&self, src: &Path) -> Option<&PathBuf> {
        match self.states.get(src) {
            Some(ProxyState::Ready(p)) => Some(p),
            _ => None,
        }
    }

    /// Is any proxy being made right now (so the UI keeps repainting)?
    pub fn busy(&self) -> bool {
        self.states.values().any(|s| matches!(s, ProxyState::Queued | ProxyState::Building(_)))
    }

    /// Forget a file's state (e.g. to retry a failed proxy).
    pub fn forget(&mut self, src: &Path) {
        self.states.remove(src);
    }

    /// Delete `src`'s proxy file (close decoders using it first); it isn't
    /// made again automatically until asked for.
    pub fn delete(&mut self, src: &Path) -> std::io::Result<()> {
        let file = match self.states.get(src) {
            Some(ProxyState::Ready(p)) => Some(p.clone()),
            _ => proxy::existing_proxy(src),
        };
        self.states.remove(src);
        self.declined.insert(src.to_owned());
        match file {
            Some(f) => std::fs::remove_file(f),
            None => Ok(()),
        }
    }

    /// Changes whenever any proxy's state does (for interfaces that redraw on change).
    pub fn signature(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        let mut v: Vec<(&PathBuf, String)> = self
            .states
            .iter()
            .map(|(k, s)| {
                let s = match s {
                    ProxyState::Building(f) => format!("b{}", (f * 100.0) as u32),
                    other => format!("{other:?}"),
                };
                (k, s)
            })
            .collect();
        v.sort();
        v.hash(&mut h);
        h.finish()
    }

    /// Make the project's proxies that are in another format again in the
    /// current one (deleting the old files). Returns how many were queued.
    pub fn remake_all(&mut self, project: &Project) -> usize {
        let now = proxy::format();
        let mut n = 0;
        for m in &project.media {
            let Some(old) = proxy::existing_proxy(&m.path) else { continue };
            if proxy::format_of(&old) == Some(now) || m.info.video.is_none() {
                continue;
            }
            let _ = std::fs::remove_file(&old);
            self.states.remove(&m.path);
            self.request(&m.path, &m.info);
            n += 1;
        }
        n
    }

    /// Delete every proxy file (in use ones may stay until they're closed).
    pub fn delete_all(&mut self) {
        let dir = proxy::cache_dir().join("proxies");
        if let Ok(rd) = std::fs::read_dir(&dir) {
            for e in rd.flatten() {
                let _ = std::fs::remove_file(e.path());
            }
        }
        // Not made again automatically (until asked for).
        let gone: Vec<PathBuf> = self.states.iter().filter(|(_, s)| matches!(s, ProxyState::Ready(_))).map(|(k, _)| k.clone()).collect();
        self.declined.extend(gone);
        self.states.retain(|_, s| matches!(s, ProxyState::Queued | ProxyState::Building(_)));
    }
}

impl Drop for Proxies {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reelforge_core::{FrameRate, MediaAsset, MediaId, Rational, Time, VideoStreamInfo};

    #[test]
    fn remaking_replaces_proxies_in_another_format() {
        let dir = std::env::temp_dir().join(format!("rf-remake-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("clip.mov");
        std::fs::write(&src, b"not really video").unwrap();
        // An H.264 proxy made earlier.
        let old = proxy::proxy_path_as(&src, proxy::ProxyFormat::H264);
        std::fs::create_dir_all(old.parent().unwrap()).unwrap();
        std::fs::write(&old, b"old proxy").unwrap();
        let mut p = Project::new();
        let info = MediaInfo {
            duration: Time::from_secs_f64(1.0),
            video: Some(VideoStreamInfo { width: 3840, height: 2160, frame_rate: FrameRate::new(25, 1), codec: "h264".into(), time_base: Rational::new(1, 25) }),
            audio: None,
            container: String::new(),
        };
        p.media.push(MediaAsset { id: MediaId(1), path: src.clone(), name: "clip.mov".into(), info });
        let mut px = Proxies::new();
        px.refresh(&p, false);
        assert!(matches!(px.state(&src), Some(ProxyState::Ready(f)) if *f == old), "the old proxy is used");
        // Still H.264: nothing to do.
        assert_eq!(px.remake_all(&p), 0);
        assert!(old.exists());
        proxy::set_format(proxy::ProxyFormat::DnxhrLb);
        let n = px.remake_all(&p);
        proxy::set_format(proxy::ProxyFormat::H264);
        assert_eq!(n, 1);
        assert!(!old.exists(), "the old one is deleted");
        assert!(!matches!(px.state(&src), Some(ProxyState::Ready(_))), "and a new one is on its way");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
