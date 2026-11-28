//! Builds the viewer image: finds the visible video layers at the playhead,
//! pulls their frames from per-clip decoders, and composites them on the GPU
//! into a texture. The app reads it back with [`Preview::read_output`] and
//! sends it to the web interface.

use reelforge_render::wgpu;
use reelforge_core::{ClipId, Layer, MediaId, Project, Time};
use reelforge_export::{FrameProvider, Picture, Scene};
use reelforge_media::{FrameSource, VideoFrame};
use reelforge_render::Compositor;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use crate::render_cache::RenderCache;
use std::time::{Duration, Instant};

/// Preview frames are decoded at most at this size; export will decode at full resolution.
const PREVIEW_MAX: (u32, u32) = (1920, 1080);
/// Open decoders this far ahead of an upcoming cut while playing.
const PREFETCH: Time = Time(Time::TICKS_PER_SECOND * 3 / 2);
/// Close decoders for clips that haven't been needed for this long.
const IDLE_CLOSE: Duration = Duration::from_secs(4);

/// A rasterised title, kept until its look (or the frame size) changes.
struct TitleCache {
    hash: u64,
    frame: VideoFrame,
    /// The frame pixels it covers, centred on the frame's middle.
    size: [f32; 2],
    /// The width of the canvas it was laid out on (its bounds are in its pixels).
    layout_w: f32,
    bounds: Option<[f32; 4]>,
    missing_font: bool,
    last_used: Instant,
}

/// Rasterise `spec` (or reuse the cached image) for clip `id`.
fn title_frame(titles: &mut HashMap<ClipId, TitleCache>, id: ClipId, spec: &reelforge_core::TitleSpec, frame: [u32; 2], at: (f32, f32)) -> VideoFrame {
    // (A text animation's state at `at`: seconds into the clip, and its length.)
    let hash = spec.content_hash_at(frame, at.0, at.1);
    let entry = titles.entry(id).or_insert_with(|| TitleCache {
        hash: hash ^ 1,
        frame: VideoFrame::from_rgba(Time::ZERO, 0, 0, Arc::from(Vec::new()), 0),
        size: [0.0; 2],
        layout_w: 1.0,
        bounds: None,
        missing_font: false,
        last_used: Instant::now(),
    });
    if entry.hash != hash {
        let r = reelforge_render::text::render_title_at(spec, frame, at.0, at.1);
        entry.frame = VideoFrame::from_rgba(Time::ZERO, r.width, r.height, r.rgba, hash);
        (entry.hash, entry.bounds, entry.missing_font) = (hash, r.bounds, r.missing_font);
        (entry.size, entry.layout_w) = ([frame[0] as f32, frame[1] as f32], frame[0] as f32);
    }
    entry.last_used = Instant::now();
    entry.frame.clone()
}

/// Draw a 2D title as finely as it's shown (`sharp` times) on just the part
/// of the canvas it reaches (or reuse it): its picture and the frame pixels it covers.
#[allow(clippy::too_many_arguments)]
fn sharp_title_frame(titles: &mut HashMap<ClipId, TitleCache>, id: ClipId, spec: &reelforge_core::TitleSpec, frame: [u32; 2], at: (f32, f32), sharp: f32, whole: bool) -> (VideoFrame, [f32; 2]) {
    let hash = reelforge_export::sharp_hash(spec.content_hash_at(frame, at.0, at.1), sharp, whole);
    let entry = titles.entry(id).or_insert_with(|| TitleCache {
        hash: hash ^ 1,
        frame: VideoFrame::from_rgba(Time::ZERO, 0, 0, Arc::from(Vec::new()), 0),
        size: [0.0; 2],
        layout_w: 1.0,
        bounds: None,
        missing_font: false,
        last_used: Instant::now(),
    });
    if entry.hash != hash {
        let r = reelforge_render::text::render_title_sharp(spec, frame, at.0, at.1, sharp, whole);
        entry.frame = VideoFrame::from_rgba(Time::ZERO, r.title.width, r.title.height, r.title.rgba, hash);
        (entry.hash, entry.bounds, entry.missing_font) = (hash, r.title.bounds, r.title.missing_font);
        (entry.size, entry.layout_w) = (r.size, frame[0] as f32);
    }
    entry.last_used = Instant::now();
    (entry.frame.clone(), entry.size)
}

struct Slot {
    source: Result<FrameSource, String>,
    /// The file being decoded (the original or its proxy).
    path: std::path::PathBuf,
    /// The size it decodes at most at.
    max: (u32, u32),
    last_used: Instant,
    /// Which version of the file it opened (see `media_version`), and when
    /// that was last looked at: a file written over is opened again.
    version: Option<(u64, u64)>,
    checked: Instant,
}

/// How often an open file is looked at for being written over.
const RECHECK: Duration = Duration::from_secs(1);

/// A stand-in picture for media whose file is missing: dark red, with the file's name.
fn offline_frame(name: &str, size: [u32; 2]) -> VideoFrame {
    // Draw it small; the compositor scales it to the clip's real size.
    let k = (960.0 / size[0].max(1) as f32).min(540.0 / size[1].max(1) as f32).min(1.0);
    let (w, h) = (((size[0] as f32 * k) as u32).max(16), ((size[1] as f32 * k) as u32).max(16));
    let spec = reelforge_core::TitleSpec {
        text: format!("MEDIA OFFLINE\n{name}"),
        size: h as f32 / 12.0,
        shadow: false,
        ..Default::default()
    };
    let text = reelforge_render::text::render_title(&spec, [w, h]);
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    for (o, t) in rgba.chunks_exact_mut(4).zip(text.rgba.chunks_exact(4)) {
        let a = t[3] as f32 / 255.0;
        let bg = [110.0, 16.0, 24.0];
        for k in 0..3 {
            o[k] = (t[k] as f32 * a + bg[k] * (1.0 - a)) as u8;
        }
        o[3] = 255;
    }
    VideoFrame::from_rgba(Time::ZERO, w, h, Arc::from(rgba), 0)
}

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    compositor: Compositor,
    scene: Scene,
    /// Renders frames for the render cache (off screen, read back).
    cache_render: Option<(Compositor, Scene)>,
}

/// Texture key for a frame shown from the render cache.
const CACHED_KEY: u64 = u64::MAX - 7;

pub struct Preview {
    /// One decoder per clip (not per file), so two clips from the same file
    /// at different times don't fight over one decoder.
    slots: HashMap<ClipId, Slot>,
    titles: HashMap<ClipId, TitleCache>,
    offline: HashMap<MediaId, VideoFrame>,
    gpu: Option<Gpu>,
    /// Hash of everything that went into the last render; skip identical frames.
    last_signature: u64,
    /// True when every layer shows its exact frame for the playhead.
    pub exact: bool,
    pub error: Option<String>,
    /// The cached frame on screen (its content id), while showing one.
    shown_cached: Option<u64>,
    /// The last frame came from the render cache.
    pub from_cache: bool,
    /// Bumped whenever the picture changes (so a viewer knows to fetch it again).
    pub picture_rev: u64,
    /// Playback quality: the Viewer's picture at 1/this of full resolution (1, 2, 4 or 8).
    quality: u32,
    /// Show the paused picture at full resolution whatever the playback quality.
    pub full_when_paused: bool,
    /// The resolution divisor drawing now (the quality, or 1 while paused).
    active: u32,
    /// Quality 0: chosen by itself while playing.
    pub auto: AutoQuality,
}

impl Preview {
    /// A preview drawing with `gpu` (the device and queue), or a blank one without.
    pub fn new(gpu: Option<(wgpu::Device, wgpu::Queue)>) -> Self {
        let gpu = gpu.map(|(device, queue)| {
            let compositor = Compositor::new(&device, &queue);
            // Hardware-decoded frames can stay on the graphics card when the
            // compositor can read them there (Windows, Direct3D 12).
            reelforge_core::frame::zero_copy::set_renderer(compositor.zero_copy_adapter());
            let mut scene = Scene::new(&device, &queue);
            // The viewer answers at once after a long jump (simulations
            // caught up on at most 250 frames, 10 s at 25 fps) and fills
            // the rest in while nothing plays.
            scene.max_catch_up = Some(250);
            Gpu { device, queue, compositor, scene, cache_render: None }
        });
        Self { slots: HashMap::new(), titles: HashMap::new(), offline: HashMap::new(), gpu, last_signature: 0, exact: true, error: None, shown_cached: None, from_cache: false, picture_rev: 0, quality: 1, full_when_paused: false, active: 1, auto: AutoQuality::default() }
    }

    /// Composite at colour bit depth `d` (from now on: the compositors are made
    /// afresh when it changes). Returns the depth worked at (32-bit float only where
    /// the graphics card can).
    pub fn set_depth(&mut self, d: reelforge_render::ColorDepth) -> Option<reelforge_render::ColorDepth> {
        let gpu = self.gpu.as_mut()?;
        let now = gpu.compositor.depth();
        let want = reelforge_render::Formats::for_depth(d, gpu.device.features()).depth;
        if want != now {
            gpu.compositor = Compositor::with_depth(&gpu.device, &gpu.queue, d);
            gpu.cache_render = None;
            self.last_signature = 0;
            self.picture_rev = self.picture_rev.wrapping_add(1);
        }
        Some(gpu.compositor.depth())
    }

    /// Draw with another device (the GPU or the CPU): its compositor and
    /// scene are made afresh, and what was drawn or decoded on the old one
    /// is let go; the playback settings stay.
    pub fn replace_gpu(&mut self, gpu: Option<(wgpu::Device, wgpu::Queue)>) {
        let fresh = Preview::new(gpu);
        self.gpu = fresh.gpu;
        self.slots.clear();
        self.titles.clear();
        self.last_signature = 0;
        self.shown_cached = None;
        self.picture_rev = self.picture_rev.wrapping_add(1);
        self.error = None;
    }

    /// Playback quality: draw the Viewer's picture at 1/`q` of full
    /// resolution (1, 2, 4 or 8), decoding footage that much smaller too, so big
    /// or heavy projects play smoothly. Renders for the render cache stay full.
    pub fn set_quality(&mut self, q: u32) {
        let q = match q {
            // Automatic.
            0 => 0,
            1 => 1,
            2..=3 => 2,
            4..=7 => 4,
            _ => 8,
        };
        self.quality = q;
    }

    pub fn quality(&self) -> u32 {
        self.quality
    }

    /// Whether there's a graphics device to draw with.
    pub fn has_gpu(&self) -> bool {
        self.gpu.is_some()
    }

    /// The resolution divisor the picture was last drawn at.
    pub fn active_quality(&self) -> u32 {
        self.active
    }

    /// Draw at 1/`q` from now on (when it changes, the next frame is drawn again).
    fn activate(&mut self, q: u32) {
        if q == self.active {
            return;
        }
        self.active = q;
        if let Some(g) = &mut self.gpu {
            g.compositor.output_scale = 1.0 / q as f32;
        }
        self.last_signature = 0;
        self.shown_cached = None;
    }

    /// The size the Viewer's footage is decoded at most at.
    fn decode_max(&self) -> (u32, u32) {
        (PREVIEW_MAX.0 / self.active, PREVIEW_MAX.1 / self.active)
    }

    /// The GPU device/queue, for offline work like export.
    pub fn gpu(&self) -> Option<(wgpu::Device, wgpu::Queue)> {
        self.gpu.as_ref().map(|g| (g.device.clone(), g.queue.clone()))
    }

    /// The compositor holding the picture (its output texture is what's shown).
    pub fn compositor(&self) -> Option<&Compositor> {
        self.gpu.as_ref().map(|g| &g.compositor)
    }

    /// The picture as RGBA bytes, with its size (read back from the GPU).
    pub fn read_output(&self) -> Option<(Vec<u8>, [u32; 2])> {
        let c = &self.gpu.as_ref()?.compositor;
        let size = c.output_size()?;
        Some((c.read_output()?, size))
    }

    /// Where a title's text is on its frame-sized canvas (`[x0, y0, x1, y1]`),
    /// once it has been drawn. Used to select titles by clicking their text.
    /// Where a title's text is, in the pixels of a frame `frame_width` across
    /// (it may be laid out wider, on a frame of non-square pixels as shown).
    pub fn title_bounds(&self, id: ClipId, frame_width: f32) -> Option<[f32; 4]> {
        let t = self.titles.get(&id)?;
        let k = if t.layout_w > 0.0 { frame_width / t.layout_w } else { 1.0 };
        t.bounds.map(|b| [b[0] * k, b[1], b[2] * k, b[3]])
    }

    /// True if a title's font file couldn't be loaded (the built-in font was used).
    pub fn title_missing_font(&self, id: ClipId) -> bool {
        self.titles.get(&id).is_some_and(|t| t.missing_font)
    }

    /// Forget all decoders (new/opened project). Keeps the GPU side.
    /// Close every decoder (and so its file); the picture shown stays, and
    /// they reopen when a frame is next needed.
    pub fn release_sources(&mut self) {
        self.slots.clear();
    }

    pub fn reset(&mut self) {
        self.slots.clear();
        self.titles.clear();
        self.offline.clear();
        self.last_signature = 0;
        self.shown_cached = None;
        if let Some(g) = &mut self.gpu {
            g.scene.reset();
            g.cache_render = None;
        }
    }

    fn slot<'a>(slots: &'a mut HashMap<ClipId, Slot>, id: ClipId, path: &std::path::Path, max: (u32, u32)) -> &'a mut Slot {
        let open = || Slot {
            // (the version first: anything decoded after is at least as new)
            version: reelforge_core::imageseq::media_version(path),
            checked: Instant::now(),
            source: FrameSource::open_cached(path, Some(max)).map_err(|e| format!("{e:#}")),
            path: path.to_owned(),
            max,
            last_used: Instant::now(),
        };
        let slot = slots.entry(id).or_insert_with(open);
        if slot.path != path || slot.max != max {
            // Switched between the original and its proxy, or to another playback quality.
            *slot = open();
        } else if slot.checked.elapsed() >= RECHECK {
            slot.checked = Instant::now();
            if reelforge_core::imageseq::media_version(path) != slot.version {
                // Written over (re-exported, re-rendered) or gone or back: start again.
                *slot = open();
            }
        }
        slot.last_used = Instant::now();
        slot
    }

    /// Render sequence `seq` at `t`. `sources` says which file to decode for
    /// each media item (the original, or its proxy). A frame kept in `cache`
    /// that's still right is shown as it is, without decoding anything.
    pub fn update(&mut self, project: &Project, seq: usize, t: Time, playing: bool, sources: &HashMap<MediaId, PathBuf>, cache: &mut RenderCache) {
        let Some(sequence) = project.sequences.get(seq) else { return };
        self.error = None;
        let chosen = if self.quality == 0 { self.auto.level } else { self.quality };
        self.activate(if self.full_when_paused && !playing { 1 } else { chosen });
        if !playing {
            self.auto.pause();
        }
        let (began, rev0) = (Instant::now(), self.picture_rev);

        let frame = crate::render_cache::frame_index(t, sequence.frame_rate);
        let recipe = reelforge_export::frame_recipe(project, seq, t, sources);
        if let (Some(hit), Some(gpu)) = (cache.get(seq, frame, recipe), &mut self.gpu) {
            let content = recipe ^ (frame as u64).rotate_left(32) ^ seq as u64;
            if self.shown_cached != Some(content) || gpu.compositor.output_view().is_none() {
                let frame_size = [sequence.width.max(2), sequence.height.max(2)];
                let layer = reelforge_render::LayerInput {
                    key: CACHED_KEY,
                    content_id: content,
                    pixels: reelforge_render::PixelData::Rgba(&hit.rgba),
                    pixel_size: hit.size,
                    layer_size: [frame_size[0] as f32, frame_size[1] as f32],
                    transform: reelforge_core::Transform::default(),
                    blend: reelforge_core::BlendMode::Normal,
                    effects: Vec::new(),
                    mask: None,
                    wipe: None,
                    mesh: None, space: None
                };
                gpu.compositor.render(frame_size, &[layer]);
                gpu.compositor.retain_layers(|k| k == CACHED_KEY);
                self.shown_cached = Some(content);
                self.picture_rev += 1;
                // What's on screen no longer matches the scene's last picture.
                self.last_signature = 0;
            }
            self.exact = true;
            self.from_cache = true;
            return;
        }
        self.shown_cached = None;
        self.from_cache = false;

        // Warm up decoders for cuts coming up soon (including the early start of an
        // incoming dissolve), so the first frame is ready on time.
        if playing {
            let max = self.decode_max();
            for tr in sequence.tracks.iter().filter(|tr| tr.kind == reelforge_core::TrackKind::Video && !tr.muted) {
                for clip in tr.clips.iter() {
                    let shows_at = clip.timeline_start - clip.dissolve_in;
                    if shows_at > t && shows_at <= t + PREFETCH {
                        if let Some(media) = project.media(clip.media) {
                            let path = sources.get(&media.id).unwrap_or(&media.path);
                            if let Ok(src) = &mut Self::slot(&mut self.slots, clip.id, path, max).source {
                                src.frame_at(clip.media_at(shows_at).max(Time::ZERO));
                            }
                        }
                    }
                }
            }
        }

        let max = self.decode_max();
        let Some(gpu) = &mut self.gpu else { return };
        let mut frames = PreviewFrames {
            slots: &mut self.slots,
            titles: &mut self.titles,
            offline: &mut self.offline,
            sources,
            exact: true,
            error: None,
            blocking: false,
            max,
        };
        let unchanged = gpu.compositor.output_view().is_some().then_some(self.last_signature);
        gpu.scene.idle = !playing;
        match gpu.scene.render(project, seq, t, &mut gpu.compositor, &mut frames, unchanged) {
            Ok(sig) => {
                if sig != self.last_signature {
                    self.picture_rev += 1;
                }
                self.last_signature = sig;
            }
            Err(e) => self.error = Some(format!("{e:#}")),
        }
        self.exact = frames.exact;
        if let Some(e) = frames.error {
            self.error = Some(e);
        }
        // Judged on frames actually drawn (ticks between frames draw nothing).
        if playing && self.quality == 0 && (self.picture_rev != rev0 || !self.exact) {
            let frame = Duration::from_secs_f64(1.0 / sequence.frame_rate.fps().max(1.0));
            self.auto.observe(Instant::now(), began.elapsed(), !self.exact, frame);
        }
        self.slots.retain(|_, s| s.last_used.elapsed() < IDLE_CLOSE);
        self.titles.retain(|_, t| t.last_used.elapsed() < IDLE_CLOSE);
    }

    /// Render `seq` at `t` exactly (waiting for every decoder), off screen, and
    /// read it back as RGBA with its size: a frame for the render cache.
    pub fn render_for_cache(&mut self, project: &Project, seq: usize, t: Time, sources: &HashMap<MediaId, PathBuf>) -> Option<(Vec<u8>, [u32; 2])> {
        self.render_scaled(project, seq, t, sources, 1.0)
    }

    /// As [`Self::render_for_cache`], drawn at `scale` (0–1) of full
    /// resolution (for small pictures: much quicker for big frames).
    pub fn render_scaled(&mut self, project: &Project, seq: usize, t: Time, sources: &HashMap<MediaId, PathBuf>, scale: f32) -> Option<(Vec<u8>, [u32; 2])> {
        let gpu = self.gpu.as_mut()?;
        let (compositor, scene) = gpu.cache_render.get_or_insert_with(|| (Compositor::new(&gpu.device, &gpu.queue), Scene::new(&gpu.device, &gpu.queue)));
        compositor.output_scale = scale.clamp(1.0 / 16.0, 1.0);
        let out = Self::render_with(compositor, scene, &mut self.slots, &mut self.titles, &mut self.offline, &mut self.error, project, seq, t, sources);
        compositor.output_scale = 1.0;
        out
    }

    #[allow(clippy::too_many_arguments)]
    fn render_with(
        compositor: &mut Compositor,
        scene: &mut Scene,
        slots: &mut HashMap<ClipId, Slot>,
        titles: &mut HashMap<ClipId, TitleCache>,
        offline: &mut HashMap<MediaId, VideoFrame>,
        error: &mut Option<String>,
        project: &Project,
        seq: usize,
        t: Time,
        sources: &HashMap<MediaId, PathBuf>,
    ) -> Option<(Vec<u8>, [u32; 2])> {
        let mut frames = PreviewFrames { slots, titles, offline, sources, exact: true, error: None, blocking: true, max: PREVIEW_MAX };
        if let Err(e) = scene.render(project, seq, t, compositor, &mut frames, None) {
            *error = Some(format!("{e:#}"));
            return None;
        }
        if let Some(e) = frames.error {
            *error = Some(e);
            return None;
        }
        let size = compositor.output_size()?;
        Some((compositor.read_output()?, size))
    }
}

/// The viewer's frames: whatever each clip's preview-size decoder has ready.
/// Automatic playback quality: steps the Viewer's resolution down (full →
/// ½ → ¼ → ⅛) when playing falls behind, and back up once it's had room to
/// spare for a while.
#[derive(Debug, Clone)]
pub struct AutoQuality {
    /// The divisor now (1, 2, 4 or 8).
    pub level: u32,
    window_start: Option<Instant>,
    frames: u32,
    late: u32,
    busy: Duration,
    /// Windows in a row with room to spare.
    calm: u32,
    /// When it last stepped down (stepping back up then waits longer).
    last_down: Option<Instant>,
}

impl Default for AutoQuality {
    fn default() -> Self {
        AutoQuality { level: 1, window_start: None, frames: 0, late: 0, busy: Duration::ZERO, calm: 0, last_down: None }
    }
}

/// Seconds of playing each judgement covers.
const AUTO_WINDOW: f64 = 0.75;

impl AutoQuality {
    /// One frame drawn while playing at `now`: it took `took`, and `late`
    /// when a layer wasn't ready with its frame. `frame` is the sequence's
    /// frame duration. Returns the level to draw at.
    pub fn observe(&mut self, now: Instant, took: Duration, late: bool, frame: Duration) -> u32 {
        let start = *self.window_start.get_or_insert(now);
        self.frames += 1;
        self.late += late as u32;
        self.busy += took;
        let span = now.duration_since(start).as_secs_f64();
        if span < AUTO_WINDOW || self.frames < 4 {
            return self.level;
        }
        let late_share = self.late as f64 / self.frames as f64;
        // Drawing time against the time a frame has.
        let load = self.busy.as_secs_f64() / self.frames as f64 / frame.as_secs_f64().max(1e-3);
        if (late_share > 0.25 || load > 0.85) && self.level < 8 {
            self.level *= 2;
            self.calm = 0;
            self.last_down = Some(now);
        } else if late_share < 0.05 && load < 0.3 && self.level > 1 {
            self.calm += 1;
            // About 3 s of room to spare; 8 s soon after stepping down (so it doesn't see-saw).
            let need = if self.last_down.is_some_and(|t| now.duration_since(t).as_secs_f64() < 10.0) { 11 } else { 4 };
            if self.calm >= need {
                self.level /= 2;
                self.calm = 0;
            }
        } else {
            self.calm = 0;
        }
        self.window_start = Some(now);
        (self.frames, self.late, self.busy) = (0, 0, Duration::ZERO);
        self.level
    }

    /// Playing stopped: the next judgement starts afresh (the level stays).
    pub fn pause(&mut self) {
        self.window_start = None;
        (self.frames, self.late, self.busy) = (0, 0, Duration::ZERO);
    }
}

struct PreviewFrames<'a> {
    slots: &'a mut HashMap<ClipId, Slot>,
    titles: &'a mut HashMap<ClipId, TitleCache>,
    offline: &'a mut HashMap<MediaId, VideoFrame>,
    sources: &'a HashMap<MediaId, PathBuf>,
    /// Every layer showed its exact frame.
    exact: bool,
    error: Option<String>,
    /// Wait for each exact frame (rendering for the cache) instead of showing what's ready.
    blocking: bool,
    /// The size frames are decoded at most at.
    max: (u32, u32),
}

impl FrameProvider for PreviewFrames<'_> {
    fn wait_for_frames(&mut self, wait: bool) -> bool {
        std::mem::replace(&mut self.blocking, wait)
    }

    fn title(&mut self, _: &Project, l: &Layer, frame_size: [u32; 2], sharp: f32, whole: bool) -> anyhow::Result<Option<Picture>> {
        let Some(spec) = l.clip.title_at(l.source_time) else { return Ok(None) };
        let (frame, size) = sharp_title_frame(self.titles, l.clip.id, &spec, frame_size, reelforge_export::title_clock(l), sharp, whole);
        Ok(Some(Picture { frame, size, media: MediaId::GENERATED }))
    }

    fn picture(&mut self, project: &Project, l: &Layer, frame_size: [u32; 2]) -> anyhow::Result<Option<Picture>> {
        if let Some(spec) = l.clip.title_at(l.source_time) {
            // Titles are drawn on a canvas the size of the frame, so 100% is their natural size.
            let frame = title_frame(self.titles, l.clip.id, &spec, frame_size, reelforge_export::title_clock(l));
            return Ok(Some(Picture { frame, size: [frame_size[0] as f32, frame_size[1] as f32], media: MediaId::GENERATED }));
        }
        let Some(media) = project.media(l.clip.media) else { return Ok(None) };
        let Some(v) = media.info.video.as_ref() else { return Ok(None) };
        let laid = media.layer_size().unwrap_or([v.width, v.height]);
        let size = [laid[0] as f32, laid[1] as f32];
        let path = self.sources.get(&media.id).unwrap_or(&media.path);
        if !reelforge_core::imageseq::media_exists(path) {
            let frame = self.offline.entry(media.id).or_insert_with(|| offline_frame(&media.name, [v.width, v.height])).clone();
            return Ok(Some(Picture { frame, size, media: media.id }));
        }
        let src = match &mut Preview::slot(self.slots, l.clip.id, path, self.max).source {
            Ok(s) => s,
            Err(e) => {
                self.error = Some(format!("Can't decode {}: {e}", media.name));
                return Ok(None);
            }
        };
        let frame = if self.blocking {
            src.frame_at_blocking(l.source_time, Duration::from_secs(10))
        } else {
            let f = src.frame_at(l.source_time).cloned();
            self.exact &= src.is_exact(l.source_time);
            f
        };
        Ok(frame.map(|frame| Picture { frame, size, media: media.id }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Play `secs` at 25 fps where each frame takes `took` ms and every
    /// `late_every`th is late (0: none). Returns the level after.
    fn play(a: &mut AutoQuality, t: &mut Instant, secs: f64, took: f64, late_every: u32) -> u32 {
        let frame = Duration::from_millis(40);
        for i in 0..(secs * 25.0) as u32 {
            *t += frame;
            a.observe(*t, Duration::from_secs_f64(took / 1000.0), late_every > 0 && i % late_every == 0, frame);
        }
        a.level
    }

    #[test]
    fn automatic_quality_steps_down_when_behind_and_back_up_when_easy() {
        let mut a = AutoQuality::default();
        let mut t = Instant::now();
        assert_eq!(play(&mut a, &mut t, 3.0, 10.0, 0), 1, "keeping up: full");
        // Slow drawing (38 of 40 ms): half within a second or so, then quarter, then eighth.
        assert!(play(&mut a, &mut t, 1.6, 38.0, 0) >= 2);
        assert_eq!(play(&mut a, &mut t, 3.0, 38.0, 0), 8, "no lower than an eighth");
        // Easy again, soon after stepping down: it waits about 8 s.
        assert_eq!(play(&mut a, &mut t, 5.0, 5.0, 0), 8);
        assert_eq!(play(&mut a, &mut t, 4.0, 5.0, 0), 4);
        // Later, about 3 s does, a step at a time.
        assert_eq!(play(&mut a, &mut t, 3.5, 5.0, 0), 2);
        assert_eq!(play(&mut a, &mut t, 3.5, 5.0, 0), 1);
        // Frames not ready in time (the decoder behind) count too.
        let l = play(&mut a, &mut t, 1.6, 5.0, 2);
        assert!(l >= 2);
        // Middling: stays put.
        assert_eq!(play(&mut a, &mut t, 6.0, 20.0, 0), l);
        // A pause doesn't count as time with room to spare.
        a.pause();
        t += Duration::from_secs(30);
        assert_eq!(play(&mut a, &mut t, 0.5, 5.0, 0), l);
    }
}
