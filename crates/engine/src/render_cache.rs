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
//!
//! They're also kept **on disk** ([`DiskStore`]), compressed, in the cache
//! folder, named by their recipe: so reopening a project (or another one
//! using the same clips and effects) finds its rendered frames again. The
//! files are written in the background; a frame missing from memory is
//! looked up there, and comes back into memory when used. The disk has its
//! own budget, and the least recently used files go first.
//!
//! Water and fluid **simulations** keep their checkpoints (one a second of
//! simulated time) on disk the same way, in a `sims` folder beside the frames
//! (a second [`DiskStore`], `.rfs` files, half the frames' budget): see
//! `reelforge_render::simstore`.

use reelforge_core::Time;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use std::time::Instant;

/// Rendered frames on disk: `<recipe>.rfc` files (a small header, then the
/// RGBA deflated), with an index of what's there.
pub struct DiskStore {
    dir: PathBuf,
    /// Its files' extension: `rfc` (frames) or `rfs` (simulation checkpoints).
    ext: &'static str,
    /// recipe → (bytes on disk, last used).
    index: Mutex<HashMap<u64, (u64, u64)>>,
    budget: AtomicU64,
    clock: AtomicU64,
    /// Frames queued for the writer thread, and how many are still being written.
    queue: crossbeam_channel::Sender<(u64, [u32; 2], Arc<[u8]>)>,
    pending: Arc<AtomicU64>,
    /// Keep no new frames (the drive is nearly full).
    paused: std::sync::atomic::AtomicBool,
}

const MAGIC: &[u8; 4] = b"RFC1";

impl DiskStore {
    /// Use (and make) `dir`, keeping at most `budget` bytes there. What's
    /// already there is indexed (by when it was last used).
    pub fn open(dir: &Path, budget: u64) -> Arc<Self> {
        Self::open_kind(dir, budget, "rfc")
    }

    /// The same, for simulation checkpoints (`.rfs` files).
    pub fn open_simulations(dir: &Path, budget: u64) -> Arc<Self> {
        Self::open_kind(dir, budget, "rfs")
    }

    fn open_kind(dir: &Path, budget: u64, ext: &'static str) -> Arc<Self> {
        let _ = std::fs::create_dir_all(dir);
        let mut found: Vec<(u64, u64, u64)> = Vec::new();
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                let Some(hex) = name.strip_suffix(ext).and_then(|n| n.strip_suffix('.')) else { continue };
                let (Ok(recipe), Ok(meta)) = (u64::from_str_radix(hex, 16), e.metadata()) else { continue };
                let when = meta.modified().ok().and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_millis() as u64);
                found.push((recipe, meta.len(), when));
            }
        }
        // Last used order → stamps 1, 2, …
        found.sort_by_key(|f| f.2);
        let index: HashMap<u64, (u64, u64)> = found.iter().enumerate().map(|(i, f)| (f.0, (f.1, i as u64 + 1))).collect();
        let (tx, rx) = crossbeam_channel::unbounded::<(u64, [u32; 2], Arc<[u8]>)>();
        let pending = Arc::new(AtomicU64::new(0));
        let store = Arc::new(DiskStore { dir: dir.to_owned(), ext, index: Mutex::new(index), budget: AtomicU64::new(budget), clock: AtomicU64::new(found.len() as u64 + 1), queue: tx, pending: pending.clone(), paused: Default::default() });
        let weak = Arc::downgrade(&store);
        let _ = std::thread::Builder::new().name("render cache writer".into()).spawn(move || {
            for (recipe, size, rgba) in rx {
                if let Some(s) = weak.upgrade() {
                    s.write(recipe, size, &rgba);
                }
                pending.fetch_sub(1, Ordering::SeqCst);
            }
        });
        store
    }

    /// Stop (or start again) keeping new frames, when the drive is nearly full.
    pub fn set_paused(&self, on: bool) {
        self.paused.store(on, Ordering::Relaxed);
    }

    pub fn paused(&self) -> bool {
        self.paused.load(Ordering::Relaxed)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn path(&self, recipe: u64) -> PathBuf {
        self.dir.join(format!("{recipe:016x}.{}", self.ext))
    }

    pub fn set_budget(&self, bytes: u64) {
        self.budget.store(bytes, Ordering::Relaxed);
        self.evict();
    }

    pub fn has(&self, recipe: u64) -> bool {
        self.index.lock().unwrap_or_else(std::sync::PoisonError::into_inner).contains_key(&recipe)
    }

    /// (bytes, frames) on disk.
    pub fn stats(&self) -> (u64, usize) {
        let i = self.index.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        (i.values().map(|v| v.0).sum(), i.len())
    }

    /// Keep a frame (written in the background).
    pub fn store(&self, recipe: u64, size: [u32; 2], rgba: Arc<[u8]>) {
        if self.has(recipe) || self.paused.load(Ordering::Relaxed) {
            return;
        }
        self.pending.fetch_add(1, Ordering::SeqCst);
        if self.queue.send((recipe, size, rgba)).is_err() {
            self.pending.fetch_sub(1, Ordering::SeqCst);
        }
    }

    /// Wait until everything queued is written (for tests, and on quitting).
    pub fn flush(&self) {
        while self.pending.load(Ordering::SeqCst) > 0 {
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
    }

    fn write(&self, recipe: u64, size: [u32; 2], rgba: &[u8]) {
        let packed = miniz_oxide::deflate::compress_to_vec(rgba, 1);
        let mut data = Vec::with_capacity(16 + packed.len());
        data.extend_from_slice(MAGIC);
        data.extend_from_slice(&size[0].to_le_bytes());
        data.extend_from_slice(&size[1].to_le_bytes());
        data.extend_from_slice(&(rgba.len() as u32).to_le_bytes());
        data.extend_from_slice(&packed);
        let path = self.path(recipe);
        let tmp = path.with_extension("tmp");
        if std::fs::write(&tmp, &data).is_err() || std::fs::rename(&tmp, &path).is_err() {
            let _ = std::fs::remove_file(&tmp);
            return;
        }
        let stamp = self.clock.fetch_add(1, Ordering::Relaxed);
        self.index.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(recipe, (data.len() as u64, stamp));
        self.evict();
    }

    /// A kept frame, if it's on disk (and readable).
    pub fn load(&self, recipe: u64) -> Option<([u32; 2], Vec<u8>)> {
        self.load_with(recipe, |size, len| len == size[0] as usize * size[1] as usize * 4)
    }

    /// Kept bytes (a simulation checkpoint), if they're on disk (and readable).
    pub fn load_bytes(&self, key: u64) -> Option<Vec<u8>> {
        self.load_with(key, |size, _| size == [0, 0]).map(|(_, b)| b)
    }

    /// Keep bytes (written in the background).
    pub fn store_bytes(&self, key: u64, bytes: Arc<[u8]>) {
        self.store(key, [0, 0], bytes);
    }

    fn load_with(&self, recipe: u64, fits: impl Fn([u32; 2], usize) -> bool) -> Option<([u32; 2], Vec<u8>)> {
        if !self.has(recipe) {
            return None;
        }
        let path = self.path(recipe);
        let read = || -> Option<([u32; 2], Vec<u8>)> {
            let data = std::fs::read(&path).ok()?;
            if data.len() < 16 || data.get(..4)? != MAGIC {
                return None;
            }
            let u = |i: usize| data.get(i..i + 4).and_then(|b| b.try_into().ok()).map(u32::from_le_bytes);
            let (size, len) = ([u(4)?, u(8)?], u(12)? as usize);
            let rgba = miniz_oxide::inflate::decompress_to_vec_with_limit(data.get(16..)?, len).ok()?;
            (rgba.len() == len && fits(size, len)).then_some((size, rgba))
        };
        match read() {
            Some(f) => {
                let stamp = self.clock.fetch_add(1, Ordering::Relaxed);
                if let Some(e) = self.index.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get_mut(&recipe) {
                    e.1 = stamp;
                }
                // Remembered as used, for next time.
                if let Ok(file) = std::fs::File::options().append(true).open(&path) {
                    let _ = file.set_modified(std::time::SystemTime::now());
                }
                Some(f)
            }
            None => {
                // Unreadable: forget it.
                self.index.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&recipe);
                let _ = std::fs::remove_file(&path);
                None
            }
        }
    }

    /// Delete every kept frame.
    pub fn clear(&self) {
        self.flush();
        let all: Vec<u64> = self.index.lock().unwrap_or_else(std::sync::PoisonError::into_inner).drain().map(|(k, _)| k).collect();
        for r in all {
            let _ = std::fs::remove_file(self.path(r));
        }
    }

    fn evict(&self) {
        let budget = self.budget.load(Ordering::Relaxed);
        let mut gone = Vec::new();
        {
            let mut i = self.index.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut used: u64 = i.values().map(|v| v.0).sum();
            if used <= budget {
                return;
            }
            let mut by_age: Vec<(u64, u64, u64)> = i.iter().map(|(k, v)| (v.1, *k, v.0)).collect();
            by_age.sort();
            for (_, k, bytes) in by_age {
                if used <= budget {
                    break;
                }
                i.remove(&k);
                used -= bytes;
                gone.push(k);
            }
        }
        for k in gone {
            let _ = std::fs::remove_file(self.path(k));
        }
    }
}

impl reelforge_render::SimulationStore for DiskStore {
    fn has(&self, key: u64) -> bool {
        DiskStore::has(self, key)
    }
    fn load(&self, key: u64) -> Option<Vec<u8>> {
        self.load_bytes(key)
    }
    fn save(&self, key: u64, bytes: Vec<u8>) {
        self.store_bytes(key, bytes.into());
    }
}

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
    /// Run to work simulations out (Simulate now): said so as it goes.
    pub simulating: bool,
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
    /// Where frames are also kept on disk (none: memory only).
    pub disk: Option<Arc<DiskStore>>,
    /// Where simulation checkpoints are kept on disk (with the frames).
    pub sims: Option<Arc<DiskStore>>,
}

impl RenderCache {
    pub fn new(budget_mb: usize) -> Self {
        Self { frames: HashMap::new(), lru: BTreeMap::new(), used: 0, budget: budget_mb << 20, clock: 0, generation: 0, job: None, disk: None, sims: None }
    }

    pub fn set_budget_mb(&mut self, mb: usize) {
        self.budget = mb << 20;
        self.evict();
    }

    /// The frame kept for `frame` of `seq`, if it's still right for `recipe`
    /// (from memory, else from disk, then kept in memory again).
    pub fn get(&mut self, seq: usize, frame: i64, recipe: u64) -> Option<&CachedFrame> {
        if !self.frames.get(&(seq, frame)).is_some_and(|e| e.recipe == recipe) {
            let (size, rgba) = self.disk.as_ref()?.load(recipe)?;
            self.keep(seq, frame, recipe, size, rgba.into());
        }
        self.clock += 1;
        let stamp = self.clock;
        let e = self.frames.get_mut(&(seq, frame)).filter(|e| e.recipe == recipe)?;
        self.lru.remove(&e.stamp);
        e.stamp = stamp;
        self.lru.insert(stamp, (seq, frame));
        Some(e)
    }

    /// Is a valid frame kept, in memory or on disk (without counting as a use)?
    pub fn has(&self, seq: usize, frame: i64, recipe: u64) -> bool {
        self.frames.get(&(seq, frame)).is_some_and(|e| e.recipe == recipe) || self.disk.as_ref().is_some_and(|d| d.has(recipe))
    }

    /// Keep a frame, in memory and on disk.
    pub fn insert(&mut self, seq: usize, frame: i64, recipe: u64, size: [u32; 2], rgba: Arc<[u8]>) {
        if let Some(d) = &self.disk {
            d.store(recipe, size, rgba.clone());
        }
        self.keep(seq, frame, recipe, size, rgba);
    }

    /// Keep a frame in memory.
    fn keep(&mut self, seq: usize, frame: i64, recipe: u64, size: [u32; 2], rgba: Arc<[u8]>) {
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

    /// Forget every kept frame (in memory; and on disk with `disk`).
    pub fn clear_all(&mut self, disk_too: bool) {
        if disk_too {
            if let Some(d) = &self.disk {
                d.clear();
            }
            if let Some(d) = &self.sims {
                d.clear();
            }
        }
        self.clear();
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
    let n = k * k;
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
    fn simulation_checkpoints_kept_on_disk_come_back_after_a_restart() {
        use reelforge_render::SimulationStore;
        let dir = std::env::temp_dir().join(format!("rf_sim_disk_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let blob: Vec<u8> = (0..5000u32).flat_map(|i| (i as f32 * 0.25).to_le_bytes()).collect();
        let store = DiskStore::open_simulations(&dir, 1 << 30);
        store.save(42, blob.clone());
        store.flush();
        assert!(store.has(42));
        // Its own files: frames in the same folder don't mix with them.
        assert!(std::fs::read_dir(&dir).unwrap().flatten().all(|e| e.path().extension().is_some_and(|x| x == "rfs")));
        assert_eq!(store.load_bytes(42).unwrap(), blob);
        drop(store);
        let again = DiskStore::open_simulations(&dir, 1 << 30);
        assert_eq!(SimulationStore::load(&*again, 42).unwrap(), blob);
        assert!(again.load(42).is_none(), "not a frame");
        let frames = DiskStore::open(&dir, 1 << 30);
        assert!(!frames.has(42), "the frame store doesn't see them");
        again.clear();
        assert!(!again.has(42));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn frames_kept_on_disk_come_back_after_a_restart() {
        let dir = std::env::temp_dir().join(format!("rf_render_disk_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let frame = |v: u8| -> Arc<[u8]> { (0..64 * 32 * 4).map(|i| (i as u8).wrapping_mul(v)).collect::<Vec<u8>>().into() };
        let store = DiskStore::open(&dir, 1 << 30);
        store.store(0xabc, [64, 32], frame(3));
        store.store(0xdef, [64, 32], frame(5));
        store.flush();
        assert_eq!(store.stats().1, 2);
        // "Restart": a new store finds them.
        drop(store);
        let store = DiskStore::open(&dir, 1 << 30);
        assert!(store.has(0xabc) && store.has(0xdef) && !store.has(1));
        let (size, rgba) = store.load(0xabc).unwrap();
        assert_eq!((size, &rgba[..]), ([64, 32], &frame(3)[..]));
        // A damaged file is dropped, not shown.
        std::fs::write(dir.join(format!("{:016x}.rfc", 0xdefu64)), b"RFC1garbage").unwrap();
        assert!(store.load(0xdef).is_none() && !store.has(0xdef));
        // Over budget: the least recently used goes.
        store.store(0x111, [64, 32], frame(7));
        store.flush();
        let one = std::fs::metadata(dir.join(format!("{:016x}.rfc", 0x111u64))).unwrap().len();
        store.load(0xabc); // used: now the newest
        store.store(0x222, [64, 32], frame(9));
        store.flush();
        store.set_budget(one * 2 + 10);
        assert!(store.has(0xabc) && store.has(0x222) && !store.has(0x111), "{:?}", store.stats());
        assert!(!dir.join(format!("{:016x}.rfc", 0x111u64)).exists());
        // The render cache looks there when memory hasn't got it.
        let mut c = RenderCache::new(64);
        c.disk = Some(store.clone());
        assert!(c.has(0, 5, 0xabc), "on disk counts");
        assert_eq!(c.get(0, 5, 0xabc).unwrap().size, [64, 32]);
        assert_eq!(c.stats().1, 1, "back in memory");
        // New frames go to disk too.
        c.insert(0, 6, 0x333, [64, 32], frame(11));
        store.flush();
        assert!(store.has(0x333));
        c.clear_all(true);
        assert_eq!((c.stats().1, store.stats().1), (0, 0));
        let _ = std::fs::remove_dir_all(&dir);
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
