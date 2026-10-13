//! The render cache: finished viewer frames, kept so heavy parts of the edit
//! (stacks of effects, compositions inside compositions) play back smoothly.
//!
//! **Render preview** (Enter, or Timeline → Render preview) renders every
//! frame between the in and out marks (or the whole sequence) exactly, once,
//! and keeps it here. Playback then shows the kept frame instead of decoding
//! and compositing again, for as long as that frame's *recipe* (a fingerprint
//! of everything that decides how it looks, see `reelforge_export::frame_recipe`)
//! hasn't changed. Edit a clip and only the frames it touches go stale; undo
//! the edit and they're valid again.
//!
//! Frames are kept in memory, at most viewer size (1920×1080), within a size
//! budget set in Settings; the oldest-used go first.

use reelforge_core::Time;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::Instant;

/// Cached frames are kept at most this big (the viewer's size).
pub const MAX_SIZE: [u32; 2] = [1920, 1080];

pub struct CachedFrame {
    pub recipe: u64,
    pub size: [u32; 2],
    pub rgba: Arc<[u8]>,
    stamp: u64,
}

/// A render-preview run in progress.
#[derive(Clone, Debug)]
pub struct RenderJob {
    pub seq: usize,
    /// Frame indexes still to look at, and the end (exclusive).
    pub next: i64,
    pub end: i64,
    pub first: i64,
    /// Frames actually rendered (the rest were already in the cache).
    pub rendered: u64,
    pub started: Instant,
}

impl RenderJob {
    pub fn fraction(&self) -> f32 {
        let total = (self.end - self.first).max(1) as f32;
        ((self.next - self.first) as f32 / total).clamp(0.0, 1.0)
    }
}

pub struct RenderCache {
    /// (sequence, frame index) → frame.
    frames: HashMap<(usize, i64), CachedFrame>,
    /// Least recently used first: stamp → key.
    lru: BTreeMap<u64, (usize, i64)>,
    used: usize,
    budget: usize,
    clock: u64,
    /// Bumped on every change, so views of the cache know to look again.
    pub generation: u64,
    pub job: Option<RenderJob>,
}

impl RenderCache {
    pub fn new(budget_mb: usize) -> Self {
        Self { frames: HashMap::new(), lru: BTreeMap::new(), used: 0, budget: budget_mb << 20, clock: 0, generation: 0, job: None }
    }

    pub fn set_budget_mb(&mut self, mb: usize) {
        self.budget = mb << 20;
        self.evict();
    }

    /// The frame kept for `frame` of `seq`, if it's still right for `recipe`.
    pub fn get(&mut self, seq: usize, frame: i64, recipe: u64) -> Option<&CachedFrame> {
        self.clock += 1;
        let stamp = self.clock;
        let e = self.frames.get_mut(&(seq, frame)).filter(|e| e.recipe == recipe)?;
        self.lru.remove(&e.stamp);
        e.stamp = stamp;
        self.lru.insert(stamp, (seq, frame));
        Some(e)
    }

    /// Is a valid frame kept (without counting as a use)?
    pub fn has(&self, seq: usize, frame: i64, recipe: u64) -> bool {
        self.frames.get(&(seq, frame)).is_some_and(|e| e.recipe == recipe)
    }

    pub fn insert(&mut self, seq: usize, frame: i64, recipe: u64, size: [u32; 2], rgba: Arc<[u8]>) {
        if rgba.len() > self.budget {
            return;
        }
        self.clock += 1;
        let stamp = self.clock;
        if let Some(old) = self.frames.remove(&(seq, frame)) {
            self.lru.remove(&old.stamp);
            self.used -= old.rgba.len();
        }
        self.used += rgba.len();
        self.frames.insert((seq, frame), CachedFrame { recipe, size, rgba, stamp });
        self.lru.insert(stamp, (seq, frame));
        self.generation += 1;
        self.evict();
    }

    pub fn clear(&mut self) {
        self.frames.clear();
        self.lru.clear();
        self.used = 0;
        self.generation += 1;
    }

    /// (bytes used, frames kept).
    pub fn stats(&self) -> (usize, usize) {
        (self.used, self.frames.len())
    }

    fn evict(&mut self) {
        while self.used > self.budget {
            let Some((_, key)) = self.lru.pop_first() else { break };
            if let Some(e) = self.frames.remove(&key) {
                self.used -= e.rgba.len();
            }
            self.generation += 1;
        }
    }
}

/// Shrink an RGBA frame to fit `MAX_SIZE` (box filter), or keep it.
pub fn fit_for_cache(rgba: Vec<u8>, size: [u32; 2]) -> (Vec<u8>, [u32; 2]) {
    let k = size[0].div_ceil(MAX_SIZE[0]).max(size[1].div_ceil(MAX_SIZE[1])).max(1);
    if k == 1 {
        return (rgba, size);
    }
    let (w, h) = (size[0] / k, size[1] / k);
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    let n = (k * k) as u32;
    for y in 0..h {
        for x in 0..w {
            let mut acc = [0u32; 4];
            for dy in 0..k {
                let row = ((y * k + dy) * size[0] + x * k) as usize * 4;
                for px in rgba[row..row + (k * 4) as usize].chunks_exact(4) {
                    for c in 0..4 {
                        acc[c] += px[c] as u32;
                    }
                }
            }
            out.extend(acc.map(|a| ((a + n / 2) / n) as u8));
        }
    }
    (out, [w, h])
}

/// The frame of a `rate` sequence showing at `t`.
pub fn frame_index(t: Time, rate: reelforge_core::FrameRate) -> i64 {
    t.to_frame(rate)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn px(n: usize) -> Arc<[u8]> {
        Arc::from(vec![7u8; n])
    }

    #[test]
    fn frames_are_valid_only_for_their_recipe() {
        let mut c = RenderCache::new(1);
        c.insert(0, 10, 111, [2, 2], px(16));
        assert!(c.get(0, 10, 111).is_some());
        assert!(c.get(0, 10, 222).is_none(), "the edit changed this frame");
        assert!(c.get(1, 10, 111).is_none(), "another sequence");
        assert!(c.has(0, 10, 111) && !c.has(0, 11, 111));
        c.insert(0, 10, 222, [2, 2], px(16));
        assert_eq!(c.stats(), (16, 1), "replaced, not added");
    }

    #[test]
    fn the_least_recently_used_frames_go_first() {
        let mut c = RenderCache::new(0);
        c.budget = 300;
        for f in 0..3 {
            c.insert(0, f, 1, [1, 1], px(100));
        }
        c.get(0, 0, 1); // frame 0 used again: frame 1 is now the oldest
        c.insert(0, 3, 1, [1, 1], px(100));
        assert!(c.has(0, 0, 1) && !c.has(0, 1, 1) && c.has(0, 2, 1) && c.has(0, 3, 1));
        assert_eq!(c.stats(), (300, 3));
        let g = c.generation;
        c.clear();
        assert_eq!(c.stats(), (0, 0));
        assert!(c.generation > g);
    }

    #[test]
    fn big_frames_shrink_to_viewer_size() {
        // 4K → 1080p by averaging 2×2 blocks.
        let size = [3840, 2160];
        let mut rgba = vec![0u8; (3840 * 2160 * 4) as usize];
        // Top-left 2×2 block: two white pixels, two black.
        for i in [0usize, 1] {
            rgba[i * 4..i * 4 + 4].copy_from_slice(&[255, 255, 255, 255]);
        }
        let (small, s) = fit_for_cache(rgba, size);
        assert_eq!(s, [1920, 1080]);
        assert_eq!(small.len(), 1920 * 1080 * 4);
        assert_eq!(&small[..4], &[128, 128, 128, 128]);
        let (same, s) = fit_for_cache(vec![1; 16], [2, 2]);
        assert_eq!((same.len(), s), (16, [2, 2]));
    }
}
