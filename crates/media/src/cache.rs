//! A memory cache of decoded preview frames, shared by every decoder.
//!
//! Every frame a preview decoder produces is kept here (up to a memory
//! budget, least recently used dropped first), keyed by file, decode size and
//! timestamp. Scrubbing back over footage that has already been seen, or
//! replaying a section, is then served straight from memory instead of
//! seeking and decoding again. Frames are reference-counted, so caching one
//! costs no copy.

use crate::decoder::VideoFrame;
use reelforge_core::Time;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

/// Which stream of frames: the file and the size it was decoded at.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CacheKey {
    pub path: PathBuf,
    pub max_size: Option<(u32, u32)>,
}

struct Entry {
    frame: VideoFrame,
    dur: Time,
    stamp: u64,
}

#[derive(Default)]
struct Inner {
    budget: usize,
    used: usize,
    files: HashMap<CacheKey, BTreeMap<i64, Entry>>,
    /// Use order: stamp → (file, pts).
    lru: BTreeMap<u64, (CacheKey, i64)>,
    clock: u64,
    hits: u64,
    misses: u64,
}

pub struct FrameCache {
    inner: Mutex<Inner>,
}

/// Default budget: 1 GB.
pub const DEFAULT_BUDGET_MB: usize = 1024;

impl FrameCache {
    pub fn new(budget_bytes: usize) -> Self {
        Self { inner: Mutex::new(Inner { budget: budget_bytes, ..Default::default() }) }
    }

    /// The cache all preview decoders share.
    pub fn global() -> &'static FrameCache {
        static C: OnceLock<FrameCache> = OnceLock::new();
        C.get_or_init(|| FrameCache::new(DEFAULT_BUDGET_MB << 20))
    }

    pub fn set_budget_mb(&self, mb: usize) {
        let mut g = self.inner.lock().unwrap();
        g.budget = mb << 20;
        g.evict();
    }

    /// Keep `frame` (which lasts `dur`).
    pub fn insert(&self, key: &CacheKey, frame: &VideoFrame, dur: Time) {
        let size = frame.byte_len();
        let mut g = self.inner.lock().unwrap();
        if size > g.budget {
            return;
        }
        g.clock += 1;
        let stamp = g.clock;
        let pts = frame.pts.0;
        let old = g.files.entry(key.clone()).or_default().insert(pts, Entry { frame: frame.clone(), dur, stamp });
        if let Some(o) = old {
            g.lru.remove(&o.stamp);
            g.used -= o.frame.byte_len();
        }
        g.used += size;
        g.lru.insert(stamp, (key.clone(), pts));
        g.evict();
    }

    /// The cached frame showing at `t`, if there is one.
    pub fn get(&self, key: &CacheKey, t: Time) -> Option<VideoFrame> {
        let mut g = self.inner.lock().unwrap();
        g.clock += 1;
        let stamp = g.clock;
        let found = g.files.get_mut(key).and_then(|m| {
            let (_, e) = m.range_mut(..=t.0).next_back()?;
            (t < e.frame.pts + e.dur).then(|| {
                let old = std::mem::replace(&mut e.stamp, stamp);
                (old, e.frame.clone())
            })
        });
        match found {
            Some((old, frame)) => {
                if let Some(v) = g.lru.remove(&old) {
                    g.lru.insert(stamp, v);
                }
                g.hits += 1;
                Some(frame)
            }
            None => {
                g.misses += 1;
                None
            }
        }
    }

    /// (bytes used, frames held, hit rate 0–1).
    pub fn stats(&self) -> (usize, usize, f32) {
        let g = self.inner.lock().unwrap();
        let frames = g.files.values().map(|m| m.len()).sum();
        let total = (g.hits + g.misses).max(1);
        (g.used, frames, g.hits as f32 / total as f32)
    }

    pub fn clear(&self) {
        let mut g = self.inner.lock().unwrap();
        g.files.clear();
        g.lru.clear();
        g.used = 0;
    }
}

impl Inner {
    fn evict(&mut self) {
        while self.used > self.budget {
            let Some((_, (key, pts))) = self.lru.pop_first() else { break };
            if let Some(m) = self.files.get_mut(&key) {
                if let Some(e) = m.remove(&pts) {
                    self.used -= e.frame.byte_len();
                }
                if m.is_empty() {
                    self.files.remove(&key);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn frame(pts: f64, bytes: usize) -> VideoFrame {
        VideoFrame::from_rgba(Time::from_secs_f64(pts), 1, 1, Arc::from(vec![0u8; bytes]), 0)
    }

    #[test]
    fn finds_the_frame_covering_a_time_and_evicts_oldest() {
        let c = FrameCache::new(300);
        let k = CacheKey { path: "a.mp4".into(), max_size: None };
        let dur = Time::from_secs_f64(0.04);
        for i in 0..3 {
            c.insert(&k, &frame(i as f64 * 0.04, 100), dur);
        }
        assert_eq!(c.get(&k, Time::from_secs_f64(0.05)).unwrap().pts, Time::from_secs_f64(0.04));
        assert!(c.get(&k, Time::from_secs_f64(0.2)).is_none(), "past the last frame");
        // Touch frame 0 so frame 1 (just used too) … frame 2 is the least recent now? Use 0, then add one more.
        c.get(&k, Time::ZERO);
        c.insert(&k, &frame(0.12, 100), dur);
        // Over budget by one frame: the least recently used (frame 2 at 0.08) went.
        assert!(c.get(&k, Time::from_secs_f64(0.09)).is_none());
        assert!(c.get(&k, Time::ZERO).is_some());
        let (used, frames, _) = c.stats();
        assert_eq!((used, frames), (300, 3));
        // Another decode size of the same file is a different stream.
        assert!(c.get(&CacheKey { path: "a.mp4".into(), max_size: Some((10, 10)) }, Time::ZERO).is_none());
    }
}
