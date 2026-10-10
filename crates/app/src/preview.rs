//! Builds the viewer image each frame: finds the visible video layers at the
//! playhead, pulls their frames from per-clip decoders, and composites them on
//! the GPU (shared with egui) into a texture the viewer panel draws.

use eframe::egui_wgpu::{self, wgpu};
use reelforge_core::{ClipId, MediaId, Project, Time, VideoItem};
use reelforge_media::{FrameSource, VideoFrame};
use reelforge_render::{Compositor, DrawItem, LayerInput};
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
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
        frame: VideoFrame { pts: Time::ZERO, width: 0, height: 0, rgba: Arc::from(Vec::new()), generation: 0 },
        bounds: None,
        missing_font: false,
        last_used: Instant::now(),
    });
    if entry.hash != hash {
        let r = reelforge_render::text::render_title(spec, frame);
        entry.frame = VideoFrame { pts: Time::ZERO, width: r.width, height: r.height, rgba: r.rgba, generation: hash };
        (entry.hash, entry.bounds, entry.missing_font) = (hash, r.bounds, r.missing_font);
    }
    entry.last_used = Instant::now();
    entry.frame.clone()
}

struct Slot {
    source: Result<FrameSource, String>,
    last_used: Instant,
}

struct Gpu {
    render_state: egui_wgpu::RenderState,
    compositor: Compositor,
    texture: Option<egui::TextureId>,
    registered_generation: u64,
}

pub struct Preview {
    /// One decoder per clip (not per file), so two clips from the same file
    /// at different times don't fight over one decoder.
    slots: HashMap<ClipId, Slot>,
    titles: HashMap<ClipId, TitleCache>,
    gpu: Option<Gpu>,
    /// Hash of everything that went into the last render; skip identical frames.
    last_signature: u64,
    /// True when every layer shows its exact frame for the playhead.
    pub exact: bool,
    pub error: Option<String>,
}

impl Preview {
    pub fn new(render_state: Option<egui_wgpu::RenderState>) -> Self {
        let gpu = render_state.map(|rs| {
            let compositor = Compositor::new(&rs.device, &rs.queue);
            Gpu { render_state: rs, compositor, texture: None, registered_generation: 0 }
        });
        Self { slots: HashMap::new(), titles: HashMap::new(), gpu, last_signature: 0, exact: true, error: None }
    }

    /// The composited frame, ready to draw with `egui::Image`/`painter.image`.
    pub fn texture(&self) -> Option<egui::TextureId> {
        self.gpu.as_ref().and_then(|g| g.texture)
    }

    /// The GPU device/queue shared with egui, for offline work like export.
    pub fn gpu(&self) -> Option<(wgpu::Device, wgpu::Queue)> {
        self.gpu.as_ref().map(|g| (g.render_state.device.clone(), g.render_state.queue.clone()))
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
        self.last_signature = 0;
    }

    fn slot<'a>(slots: &'a mut HashMap<ClipId, Slot>, id: ClipId, path: &std::path::Path) -> &'a mut Slot {
        let slot = slots.entry(id).or_insert_with(|| Slot {
            source: FrameSource::open(path, Some(PREVIEW_MAX)).map_err(|e| format!("{e:#}")),
            last_used: Instant::now(),
        });
        slot.last_used = Instant::now();
        slot
    }

    pub fn update(&mut self, project: &Project, t: Time, playing: bool) {
        let seq = &project.sequences[0];
        let frame_size = [seq.width.max(2), seq.height.max(2)];

        // Gather the frame for every visible layer (two per dissolve).
        struct Gathered {
            key: u64,
            frame: VideoFrame,
            media: MediaId,
            layer_size: [f32; 2],
            transform: reelforge_core::Transform,
            blend: reelforge_core::BlendMode,
            effects: Vec<reelforge_core::EffectValues>,
        }
        enum Item {
            Single(Gathered),
            Dissolve(Gathered, Gathered, f32),
        }
        let mut items = Vec::new();
        let mut exact = true;
        self.error = None;
        let mut gather = |l: &reelforge_core::Layer, slots: &mut HashMap<ClipId, Slot>, titles: &mut HashMap<ClipId, TitleCache>, error: &mut Option<String>| -> Option<Gathered> {
            if let Some(spec) = &l.clip.title {
                // Titles are drawn on a canvas the size of the frame, so 100% is their natural size.
                return Some(Gathered {
                    key: l.clip.id.0,
                    frame: title_frame(titles, l.clip.id, spec, frame_size),
                    media: MediaId::GENERATED,
                    layer_size: [frame_size[0] as f32, frame_size[1] as f32],
                    transform: l.transform,
                    blend: l.blend,
                    effects: l.effects.clone(),
                });
            }
            let media = project.media(l.clip.media)?;
            let v = media.info.video.as_ref()?;
            let src = match &mut Self::slot(slots, l.clip.id, &media.path).source {
                Ok(s) => s,
                Err(e) => {
                    *error = Some(format!("Can't decode {}: {e}", media.name));
                    return None;
                }
            };
            let frame = src.frame_at(l.source_time).cloned();
            exact &= src.is_exact(l.source_time);
            Some(Gathered {
                key: l.clip.id.0,
                frame: frame?,
                media: media.id,
                layer_size: [v.width as f32, v.height as f32],
                transform: l.transform,
                blend: l.blend,
                effects: l.effects.clone(),
            })
        };
        for item in project.video_layers_at(0, t) {
            match item {
                VideoItem::Single(l) => items.extend(gather(&l, &mut self.slots, &mut self.titles, &mut self.error).map(Item::Single)),
                VideoItem::Dissolve { from, to, mix } => {
                    match (
                        gather(&from, &mut self.slots, &mut self.titles, &mut self.error),
                        gather(&to, &mut self.slots, &mut self.titles, &mut self.error),
                    ) {
                        (Some(a), Some(b)) => items.push(Item::Dissolve(a, b, mix)),
                        (Some(one), None) | (None, Some(one)) => items.push(Item::Single(one)),
                        (None, None) => {}
                    }
                }
            }
        }
        self.exact = exact;

        // Warm up decoders for cuts coming up soon (including the early start of an
        // incoming dissolve), so the first frame is ready on time.
        if playing {
            for tr in seq.tracks.iter().filter(|tr| tr.kind == reelforge_core::TrackKind::Video && !tr.muted) {
                for clip in tr.clips.iter() {
                    let shows_at = clip.timeline_start - clip.dissolve_in;
                    if shows_at > t && shows_at <= t + PREFETCH {
                        if let Some(media) = project.media(clip.media) {
                            if let Ok(src) = &mut Self::slot(&mut self.slots, clip.id, &media.path).source {
                                src.frame_at(clip.source_time(shows_at).max(Time::ZERO));
                            }
                        }
                    }
                }
            }
        }
        self.slots.retain(|_, s| s.last_used.elapsed() < IDLE_CLOSE);
        self.titles.retain(|_, t| t.last_used.elapsed() < IDLE_CLOSE);

        let Some(gpu) = &mut self.gpu else { return };

        // Skip the GPU work if nothing that affects the picture changed.
        let mut h = DefaultHasher::new();
        frame_size.hash(&mut h);
        let hash_layer = |l: &Gathered, h: &mut DefaultHasher| {
            (l.key, l.media, l.frame.pts, l.frame.generation).hash(h);
            for f in l.transform.position.iter().chain(&l.transform.scale).chain([&l.transform.rotation, &l.transform.opacity]) {
                f.to_bits().hash(h);
            }
            l.blend.hash(h);
            for e in &l.effects {
                e.kind.hash(h);
                for v in e.values {
                    v.to_bits().hash(h);
                }
            }
        };
        for item in &items {
            match item {
                Item::Single(l) => hash_layer(l, &mut h),
                Item::Dissolve(a, b, mix) => {
                    hash_layer(a, &mut h);
                    hash_layer(b, &mut h);
                    mix.to_bits().hash(&mut h);
                }
            }
        }
        let signature = h.finish();
        if signature == self.last_signature && gpu.texture.is_some() {
            return;
        }
        self.last_signature = signature;

        fn input(l: &Gathered) -> LayerInput<'_> {
            let mut ch = DefaultHasher::new();
            (l.media, l.frame.pts, l.frame.generation).hash(&mut ch);
            LayerInput {
                key: l.key,
                content_id: ch.finish(),
                pixels: &l.frame.rgba,
                pixel_size: [l.frame.width, l.frame.height],
                layer_size: l.layer_size,
                transform: l.transform,
                blend: l.blend,
                effects: l.effects.clone(),
            }
        }
        let draw: Vec<DrawItem> = items
            .iter()
            .map(|i| match i {
                Item::Single(l) => DrawItem::Layer(input(l)),
                Item::Dissolve(a, b, mix) => DrawItem::Dissolve { from: input(a), to: input(b), mix: *mix },
            })
            .collect();
        gpu.compositor.render_items(frame_size, &draw);
        let (slots, titles) = (&self.slots, &self.titles);
        gpu.compositor.retain_layers(|k| slots.contains_key(&ClipId(k)) || titles.contains_key(&ClipId(k)));

        // Hand the output texture to egui (re-register after a resize).
        let view = gpu.compositor.output_view().expect("rendered");
        let mut renderer = gpu.render_state.renderer.write();
        match gpu.texture {
            None => {
                gpu.texture = Some(renderer.register_native_texture(&gpu.render_state.device, view, wgpu::FilterMode::Linear));
                gpu.registered_generation = gpu.compositor.output_generation;
            }
            Some(id) if gpu.registered_generation != gpu.compositor.output_generation => {
                renderer.update_egui_texture_from_wgpu_texture(&gpu.render_state.device, view, wgpu::FilterMode::Linear, id);
                gpu.registered_generation = gpu.compositor.output_generation;
            }
            Some(_) => {}
        }
    }
}
