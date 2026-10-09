//! Builds the viewer image each frame: finds the visible video layers at the
//! playhead, pulls their frames from per-clip decoders, and composites them on
//! the GPU (shared with egui) into a texture the viewer panel draws.

use eframe::egui_wgpu::{self, wgpu};
use reelforge_core::{ClipId, MediaId, Project, Time};
use reelforge_media::{FrameSource, VideoFrame};
use reelforge_render::{Compositor, LayerInput};
use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

/// Preview frames are decoded at most at this size; export will decode at full resolution.
const PREVIEW_MAX: (u32, u32) = (1920, 1080);
/// Open decoders this far ahead of an upcoming cut while playing.
const PREFETCH: Time = Time(Time::TICKS_PER_SECOND * 3 / 2);
/// Close decoders for clips that haven't been needed for this long.
const IDLE_CLOSE: Duration = Duration::from_secs(4);

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
        Self { slots: HashMap::new(), gpu, last_signature: 0, exact: true, error: None }
    }

    /// The composited frame, ready to draw with `egui::Image`/`painter.image`.
    pub fn texture(&self) -> Option<egui::TextureId> {
        self.gpu.as_ref().and_then(|g| g.texture)
    }

    /// The GPU device/queue shared with egui, for offline work like export.
    pub fn gpu(&self) -> Option<(wgpu::Device, wgpu::Queue)> {
        self.gpu.as_ref().map(|g| (g.render_state.device.clone(), g.render_state.queue.clone()))
    }

    /// Forget all decoders (new/opened project). Keeps the GPU side.
    pub fn reset(&mut self) {
        self.slots.clear();
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

        // Gather the frame for every visible layer.
        struct Gathered {
            key: u64,
            frame: VideoFrame,
            media: MediaId,
            layer_size: [f32; 2],
            transform: reelforge_core::Transform,
            blend: reelforge_core::BlendMode,
        }
        let mut layers = Vec::new();
        let mut exact = true;
        self.error = None;
        for (_, clip) in seq.video_layers_at(t) {
            let Some(media) = project.media(clip.media) else { continue };
            let Some(v) = &media.info.video else { continue };
            let src_t = clip.source_time(t);
            let slot = Self::slot(&mut self.slots, clip.id, &media.path);
            let src = match &mut slot.source {
                Ok(s) => s,
                Err(e) => {
                    self.error = Some(format!("Can't decode {}: {e}", media.name));
                    continue;
                }
            };
            let frame = src.frame_at(src_t).cloned();
            exact &= src.is_exact(src_t);
            if let Some(frame) = frame {
                layers.push(Gathered {
                    key: clip.id.0,
                    frame,
                    media: media.id,
                    layer_size: [v.width as f32, v.height as f32],
                    transform: clip.transform,
                    blend: clip.blend,
                });
            }
        }
        self.exact = exact;

        // Warm up decoders for cuts coming up soon, so the first frame is ready on time.
        if playing {
            for tr in seq.tracks.iter().filter(|tr| tr.kind == reelforge_core::TrackKind::Video && !tr.muted) {
                for clip in tr.clips.iter().filter(|c| c.timeline_start > t && c.timeline_start <= t + PREFETCH) {
                    if let Some(media) = project.media(clip.media) {
                        if let Ok(src) = &mut Self::slot(&mut self.slots, clip.id, &media.path).source {
                            src.frame_at(clip.source_in);
                        }
                    }
                }
            }
        }
        self.slots.retain(|_, s| s.last_used.elapsed() < IDLE_CLOSE);

        let Some(gpu) = &mut self.gpu else { return };

        // Skip the GPU work if nothing that affects the picture changed.
        let mut h = DefaultHasher::new();
        frame_size.hash(&mut h);
        for l in &layers {
            (l.key, l.media, l.frame.pts, l.frame.generation).hash(&mut h);
            for f in l.transform.position.iter().chain(&l.transform.scale).chain([&l.transform.rotation, &l.transform.opacity]) {
                f.to_bits().hash(&mut h);
            }
            l.blend.hash(&mut h);
        }
        let signature = h.finish();
        if signature == self.last_signature && gpu.texture.is_some() {
            return;
        }
        self.last_signature = signature;

        let inputs: Vec<LayerInput> = layers
            .iter()
            .map(|l| {
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
                }
            })
            .collect();
        gpu.compositor.render(frame_size, &inputs);
        let slots = &self.slots;
        gpu.compositor.retain_layers(|k| slots.contains_key(&ClipId(k)));

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
