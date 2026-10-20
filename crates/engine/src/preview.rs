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
    bounds: Option<[f32; 4]>,
    missing_font: bool,
    last_used: Instant,
}

/// Rasterise `spec` (or reuse the cached image) for clip `id`.
fn title_frame(titles: &mut HashMap<ClipId, TitleCache>, id: ClipId, spec: &reelforge_core::TitleSpec, frame: [u32; 2]) -> VideoFrame {
    let hash = spec.content_hash(frame);
    let entry = titles.entry(id).or_insert_with(|| TitleCache {
        hash: hash ^ 1,
        frame: VideoFrame::from_rgba(Time::ZERO, 0, 0, Arc::from(Vec::new()), 0),
        bounds: None,
        missing_font: false,
        last_used: Instant::now(),
    });
    if entry.hash != hash {
        let r = reelforge_render::text::render_title(spec, frame);
        entry.frame = VideoFrame::from_rgba(Time::ZERO, r.width, r.height, r.rgba, hash);
        (entry.hash, entry.bounds, entry.missing_font) = (hash, r.bounds, r.missing_font);
    }
    entry.last_used = Instant::now();
    entry.frame.clone()
}

struct Slot {
    source: Result<FrameSource, String>,
    /// The file being decoded (the original or its proxy).
    path: std::path::PathBuf,
    last_used: Instant,
}

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
}

impl Preview {
    /// A preview drawing with `gpu` (the device and queue), or a blank one without.
    pub fn new(gpu: Option<(wgpu::Device, wgpu::Queue)>) -> Self {
        let gpu = gpu.map(|(device, queue)| {
            let compositor = Compositor::new(&device, &queue);
            // Hardware-decoded frames can stay on the graphics card when the
            // compositor can read them there (Windows, Direct3D 12).
            reelforge_core::frame::zero_copy::set_renderer(compositor.zero_copy_adapter());
            let scene = Scene::new(&device, &queue);
            Gpu { device, queue, compositor, scene, cache_render: None }
        });
        Self { slots: HashMap::new(), titles: HashMap::new(), offline: HashMap::new(), gpu, last_signature: 0, exact: true, error: None, shown_cached: None, from_cache: false, picture_rev: 0 }
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
    pub fn title_bounds(&self, id: ClipId) -> Option<[f32; 4]> {
        self.titles.get(&id).and_then(|t| t.bounds)
    }

    /// True if a title's font file couldn't be loaded (the built-in font was used).
    pub fn title_missing_font(&self, id: ClipId) -> bool {
        self.titles.get(&id).is_some_and(|t| t.missing_font)
    }

    /// Forget all decoders (new/opened project). Keeps the GPU side.
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

    fn slot<'a>(slots: &'a mut HashMap<ClipId, Slot>, id: ClipId, path: &std::path::Path) -> &'a mut Slot {
        let open = || Slot {
            source: FrameSource::open_cached(path, Some(PREVIEW_MAX)).map_err(|e| format!("{e:#}")),
            path: path.to_owned(),
            last_used: Instant::now(),
        };
        let slot = slots.entry(id).or_insert_with(open);
        if slot.path != path {
            // Switched between the original and its proxy.
            *slot = open();
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
                    mesh: None,
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
            for tr in sequence.tracks.iter().filter(|tr| tr.kind == reelforge_core::TrackKind::Video && !tr.muted) {
                for clip in tr.clips.iter() {
                    let shows_at = clip.timeline_start - clip.dissolve_in;
                    if shows_at > t && shows_at <= t + PREFETCH {
                        if let Some(media) = project.media(clip.media) {
                            let path = sources.get(&media.id).unwrap_or(&media.path);
                            if let Ok(src) = &mut Self::slot(&mut self.slots, clip.id, path).source {
                                src.frame_at(clip.media_at(shows_at).max(Time::ZERO));
                            }
                        }
                    }
                }
            }
        }

        let Some(gpu) = &mut self.gpu else { return };
        let mut frames = PreviewFrames {
            slots: &mut self.slots,
            titles: &mut self.titles,
            offline: &mut self.offline,
            sources,
            exact: true,
            error: None,
            blocking: false,
        };
        let unchanged = gpu.compositor.output_view().is_some().then_some(self.last_signature);
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
        self.slots.retain(|_, s| s.last_used.elapsed() < IDLE_CLOSE);
        self.titles.retain(|_, t| t.last_used.elapsed() < IDLE_CLOSE);
    }

    /// Render `seq` at `t` exactly (waiting for every decoder), off screen, and
    /// read it back as RGBA with its size: a frame for the render cache.
    pub fn render_for_cache(&mut self, project: &Project, seq: usize, t: Time, sources: &HashMap<MediaId, PathBuf>) -> Option<(Vec<u8>, [u32; 2])> {
        let gpu = self.gpu.as_mut()?;
        let (compositor, scene) = gpu.cache_render.get_or_insert_with(|| (Compositor::new(&gpu.device, &gpu.queue), Scene::new(&gpu.device, &gpu.queue)));
        let mut frames = PreviewFrames {
            slots: &mut self.slots,
            titles: &mut self.titles,
            offline: &mut self.offline,
            sources,
            exact: true,
            error: None,
            blocking: true,
        };
        if let Err(e) = scene.render(project, seq, t, compositor, &mut frames, None) {
            self.error = Some(format!("{e:#}"));
            return None;
        }
        if let Some(e) = frames.error {
            self.error = Some(e);
            return None;
        }
        let size = compositor.output_size()?;
        Some((compositor.read_output()?, size))
    }
}

/// The viewer's frames: whatever each clip's preview-size decoder has ready.
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
}

impl FrameProvider for PreviewFrames<'_> {
    fn picture(&mut self, project: &Project, l: &Layer, frame_size: [u32; 2]) -> anyhow::Result<Option<Picture>> {
        if let Some(spec) = &l.clip.title {
            // Titles are drawn on a canvas the size of the frame, so 100% is their natural size.
            let frame = title_frame(self.titles, l.clip.id, spec, frame_size);
            return Ok(Some(Picture { frame, size: [frame_size[0] as f32, frame_size[1] as f32], media: MediaId::GENERATED }));
        }
        let Some(media) = project.media(l.clip.media) else { return Ok(None) };
        let Some(v) = media.info.video.as_ref() else { return Ok(None) };
        let size = [v.width as f32, v.height as f32];
        let path = self.sources.get(&media.id).unwrap_or(&media.path);
        if !path.exists() {
            let frame = self.offline.entry(media.id).or_insert_with(|| offline_frame(&media.name, [v.width, v.height])).clone();
            return Ok(Some(Picture { frame, size, media: media.id }));
        }
        let src = match &mut Preview::slot(self.slots, l.clip.id, path).source {
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
