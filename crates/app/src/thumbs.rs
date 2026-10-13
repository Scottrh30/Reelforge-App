//! Thumbnails for video clips on the timeline.
//!
//! The timeline asks for "a picture of this media near time t" for each tile
//! it draws. Pictures are decoded small on a background thread (from the proxy
//! when there is one), newest requests first so the part of the timeline you're
//! looking at fills in first, and kept as GPU textures per media, sorted by
//! time. Until the exact picture arrives the nearest one already decoded is
//! shown, so zooming and scrolling never leave holes.

use egui::{ColorImage, TextureHandle, TextureOptions};
use reelforge_core::{MediaId, Time};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

/// Decoded size limit (keeps the aspect ratio inside this box).
const DECODE_BOX: (u32, u32) = (192, 108);
/// Most pictures kept (about 80 KB each on the GPU).
const MAX_KEPT: usize = 1500;
/// Most requests waiting; older ones are dropped (they're usually scrolled away).
const MAX_QUEUED: usize = 96;
/// Time grids a picture can be requested on, in seconds (the coarsest that still
/// gives each tile its own picture is used).
const GRIDS: [f64; 11] = [0.04, 0.1, 0.25, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0, 300.0];

struct Request {
    media: MediaId,
    at: i64,
    path: PathBuf,
}

#[derive(Default)]
struct Queue {
    waiting: Vec<Request>,
    stop: bool,
}

type Done = (MediaId, i64, Option<ColorImage>);

struct Worker {
    queue: Arc<(Mutex<Queue>, Condvar)>,
    done: crossbeam_channel::Receiver<Done>,
}

#[derive(Default)]
pub struct Thumbnails {
    worker: Option<Worker>,
    pictures: HashMap<MediaId, BTreeMap<i64, TextureHandle>>,
    /// Insertion order, for forgetting the oldest.
    order: VecDeque<(MediaId, i64)>,
    pending: HashSet<(MediaId, i64)>,
    /// Media that couldn't be decoded (don't keep asking).
    failed: HashSet<MediaId>,
}

impl Drop for Thumbnails {
    fn drop(&mut self) {
        if let Some(w) = &self.worker {
            w.queue.0.lock().unwrap().stop = true;
            w.queue.1.notify_all();
        }
    }
}

/// The grid point (in ticks) to ask for, for media time `t` when each tile covers `span` of media.
pub fn grid_point(t: Time, span: f64) -> i64 {
    let g = GRIDS.iter().copied().find(|g| *g >= span.abs()).unwrap_or(GRIDS[GRIDS.len() - 1]);
    let step = (g * Time::TICKS_PER_SECOND as f64).round() as i64;
    (t.0.max(0) + step / 2) / step * step
}

fn worker_loop(queue: Arc<(Mutex<Queue>, Condvar)>, done: crossbeam_channel::Sender<Done>, ctx: egui::Context) {
    // A few decoders stay open, most recently used last.
    let mut open: Vec<(PathBuf, reelforge_media::FrameSource)> = Vec::new();
    loop {
        let req = {
            let (lock, cv) = &*queue;
            let mut q = lock.lock().unwrap();
            while q.waiting.is_empty() && !q.stop {
                q = cv.wait(q).unwrap();
            }
            if q.stop {
                return;
            }
            q.waiting.pop().unwrap() // newest first
        };
        let i = match open.iter().position(|(p, _)| *p == req.path) {
            Some(i) => i,
            None => match reelforge_media::FrameSource::open(&req.path, Some(DECODE_BOX)) {
                Ok(src) => {
                    if open.len() >= 3 {
                        open.remove(0);
                    }
                    open.push((req.path.clone(), src));
                    open.len() - 1
                }
                Err(e) => {
                    log::warn!("thumbnails for {}: {e:#}", req.path.display());
                    let _ = done.send((req.media, req.at, None));
                    ctx.request_repaint();
                    continue;
                }
            },
        };
        let entry = open.remove(i);
        let (path, mut src) = entry;
        let image = src.frame_at_blocking(Time(req.at), Duration::from_secs(5)).map(|f| ColorImage::from_rgba_unmultiplied([f.width as usize, f.height as usize], &f.rgba()));
        open.push((path, src));
        if done.send((req.media, req.at, image)).is_err() {
            return;
        }
        ctx.request_repaint();
    }
}

impl Thumbnails {
    /// Collect finished pictures. Call once per frame.
    pub fn poll(&mut self, ctx: &egui::Context) {
        let Some(w) = &self.worker else { return };
        let got: Vec<Done> = w.done.try_iter().collect();
        for (media, at, image) in got {
            self.pending.remove(&(media, at));
            let Some(image) = image else {
                self.failed.insert(media);
                continue;
            };
            let tex = ctx.load_texture(format!("thumb-{}-{at}", media.0), image, TextureOptions::LINEAR);
            self.pictures.entry(media).or_default().insert(at, tex);
            self.order.push_back((media, at));
        }
        while self.order.len() > MAX_KEPT {
            if let Some((m, at)) = self.order.pop_front() {
                if let Some(map) = self.pictures.get_mut(&m) {
                    map.remove(&at);
                }
            }
        }
    }

    /// The picture for media time `t` (asking for it if needed), or the nearest one
    /// decoded so far while it's on its way. `span` is how much media one tile covers.
    pub fn get(&mut self, ctx: &egui::Context, media: MediaId, path: &std::path::Path, t: Time, span: f64) -> Option<TextureHandle> {
        if self.failed.contains(&media) {
            return None;
        }
        let at = grid_point(t, span);
        if let Some(tex) = self.pictures.get(&media).and_then(|m| m.get(&at)) {
            return Some(tex.clone());
        }
        if self.pending.insert((media, at)) {
            let w = self.worker.get_or_insert_with(|| {
                let queue = Arc::new((Mutex::new(Queue::default()), Condvar::new()));
                let (tx, rx) = crossbeam_channel::unbounded();
                let (q, c) = (queue.clone(), ctx.clone());
                std::thread::Builder::new().name("thumbnails".into()).spawn(move || worker_loop(q, tx, c)).expect("spawn thumbnail thread");
                Worker { queue, done: rx }
            });
            // Decode from the proxy when there is one: much faster for big files.
            let path = reelforge_media::proxy::existing_proxy(path).unwrap_or_else(|| path.to_owned());
            let mut q = w.queue.0.lock().unwrap();
            q.waiting.push(Request { media, at, path });
            if q.waiting.len() > MAX_QUEUED {
                let extra = q.waiting.len() - MAX_QUEUED;
                for r in q.waiting.drain(..extra) {
                    self.pending.remove(&(r.media, r.at));
                }
            }
            w.queue.1.notify_one();
        }
        // Meanwhile: the nearest picture we have.
        let map = self.pictures.get(&media)?;
        let before = map.range(..=at).next_back();
        let after = map.range(at..).next();
        match (before, after) {
            (Some(b), Some(a)) => Some(if at - b.0 <= a.0 - at { b.1 } else { a.1 }.clone()),
            (Some(x), None) | (None, Some(x)) => Some(x.1.clone()),
            (None, None) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_snap_to_a_grid_that_suits_the_zoom() {
        let s = |x: f64| Time::from_secs_f64(x);
        let tps = Time::TICKS_PER_SECOND;
        // Zoomed out (each tile ~3 s of media): 5 s grid.
        assert_eq!(grid_point(s(12.4), 3.0), 10 * tps);
        assert_eq!(grid_point(s(12.6), 3.0), 15 * tps);
        // Zoomed in (each tile 0.3 s): 0.5 s grid.
        assert_eq!(grid_point(s(12.6), 0.3), (12.5 * tps as f64) as i64);
        // Nearby times share a picture; negative times clamp to the start.
        assert_eq!(grid_point(s(12.55), 0.3), grid_point(s(12.6), 0.3));
        assert_eq!(grid_point(Time(-5), 1.0), 0);
    }

    #[test]
    fn decodes_pictures_in_the_background() {
        // Make a short test video with FFmpeg's own lavfi source if the ffmpeg CLI is around.
        let dir = std::env::temp_dir().join(format!("rf-thumbs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("t.mp4");
        let ok = std::process::Command::new("ffmpeg")
            .args(["-y", "-v", "error", "-f", "lavfi", "-i", "testsrc=size=320x180:rate=10:duration=2", "-pix_fmt", "yuv420p"])
            .arg(&file)
            .status()
            .is_ok_and(|s| s.success());
        if !ok {
            eprintln!("no ffmpeg CLI; skipping");
            return;
        }
        let ctx = egui::Context::default();
        let mut th = Thumbnails::default();
        let m = MediaId(1);
        assert!(th.get(&ctx, m, &file, Time::from_secs_f64(1.0), 0.5).is_none(), "nothing yet");
        let deadline = std::time::Instant::now() + Duration::from_secs(20);
        let tex = loop {
            th.poll(&ctx);
            if let Some(t) = th.get(&ctx, m, &file, Time::from_secs_f64(1.0), 0.5) {
                break t;
            }
            assert!(std::time::Instant::now() < deadline, "timed out");
            std::thread::sleep(Duration::from_millis(20));
        };
        assert_eq!(tex.size(), [192, 108], "decoded small, keeping the aspect ratio");
        // A nearby time on the same grid gets the same picture without decoding again.
        assert!(th.get(&ctx, m, &file, Time::from_secs_f64(1.1), 0.5).is_some());
        // A missing file is given up on.
        let gone = MediaId(2);
        th.get(&ctx, gone, &dir.join("nope.mp4"), Time::ZERO, 1.0);
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !th.failed.contains(&gone) {
            th.poll(&ctx);
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(20));
        }
        drop(th);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
