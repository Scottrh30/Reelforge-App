//! Turning a sequence at a moment in time into compositor draw calls, shared
//! by the viewer and export so the two always agree.
//!
//! Where each layer's picture comes from is up to the caller (a
//! [`FrameProvider`]): the viewer wants whatever frame is ready right now from
//! a preview-sized decoder; export waits for the exact frame at full size.
//! Composite shots (clips that play another sequence) are handled here: the
//! inner sequence is rendered by its own compositor, recursively, and its
//! picture becomes one layer of the outer one.

use anyhow::Result;
use reelforge_core::mask::{self, MaskImage};
use reelforge_core::{Transition, BlendMode, ClipId, EffectValues, Layer, MediaId, Project, Time, Transform, VideoItem};
use reelforge_media::VideoFrame;
use reelforge_render::{wgpu, Compositor, DrawItem, LayerInput, MaskInput, Wipe};
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

/// Composite shots inside composite shots stop here (a cycle can't be built,
/// but a damaged project file could contain one).
pub const MAX_NESTING: usize = 8;

/// A layer's picture.
pub struct Picture {
    pub frame: VideoFrame,
    /// Its size at 100% scale, in frame pixels (can differ from the frame's
    /// pixel size, e.g. for proxies).
    pub size: [f32; 2],
    /// What it shows; part of the texture cache key.
    pub media: MediaId,
}

pub trait FrameProvider {
    /// The picture for a media or title layer of a sequence `frame_size` big,
    /// or `None` to leave the layer out (nothing ready, no video).
    fn picture(&mut self, project: &Project, layer: &Layer, frame_size: [u32; 2]) -> Result<Option<Picture>>;
}

struct Gathered {
    key: u64,
    picture: Picture,
    transform: Transform,
    blend: BlendMode,
    effects: Vec<EffectValues>,
    /// The layer's masks as an alpha image, and its hash.
    mask: Option<(u64, Arc<MaskImage>)>,
    /// Revealed by a wipe transition.
    wipe: Option<Wipe>,
}

/// Masks are rasterised at most this big (longer side); they're soft-edged
/// alpha, so the renderer's filtering covers the rest.
const MASK_MAX: u32 = 2048;

#[allow(clippy::large_enum_variant)] // short-lived, per frame
enum Item {
    Single(Gathered),
    /// A dissolve, or a dip through a colour.
    Dissolve(Gathered, Gathered, f32, Option<[f32; 3]>),
}

/// A composite shot's own renderer and its last picture.
struct Nested {
    compositor: Compositor,
    signature: Option<u64>,
    frame: Option<VideoFrame>,
    last_used: u64,
}

pub struct Scene {
    device: wgpu::Device,
    queue: wgpu::Queue,
    /// Per composite-shot clip.
    nested: HashMap<ClipId, Nested>,
    /// Rasterised masks per clip: (hash of the shape, image, last used tick).
    masks: HashMap<ClipId, (u64, Arc<MaskImage>, u64)>,
    /// Counts top-level renders, to let go of composite shots not seen for a while.
    tick: u64,
    generation: u64,
}

impl Scene {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        Self { device: device.clone(), queue: queue.clone(), nested: HashMap::new(), masks: HashMap::new(), tick: 0, generation: 0 }
    }

    /// Forget composite-shot renders (e.g. a different project was opened).
    pub fn reset(&mut self) {
        self.nested.clear();
        self.masks.clear();
    }

    /// Render sequence `seq` at time `t` into `compositor`. Returns a
    /// signature of everything that went into the picture; when it equals
    /// `unchanged_from`, the compositor isn't touched (the picture would be the same).
    pub fn render(
        &mut self,
        project: &Project,
        seq: usize,
        t: Time,
        compositor: &mut Compositor,
        provider: &mut dyn FrameProvider,
        unchanged_from: Option<u64>,
    ) -> Result<u64> {
        self.tick += 1;
        let sig = self.render_at(project, seq, t, compositor, provider, unchanged_from, 0)?;
        let tick = self.tick;
        // Composite shots off screen for a few seconds give their GPU memory back.
        self.nested.retain(|_, n| tick - n.last_used < 150);
        self.masks.retain(|_, m| tick - m.2 < 150);
        Ok(sig)
    }

    #[allow(clippy::too_many_arguments)]
    fn render_at(
        &mut self,
        project: &Project,
        seq: usize,
        t: Time,
        compositor: &mut Compositor,
        provider: &mut dyn FrameProvider,
        unchanged_from: Option<u64>,
        depth: usize,
    ) -> Result<u64> {
        let Some(s) = project.sequences.get(seq) else { return Ok(0) };
        let frame_size = [s.width.max(2), s.height.max(2)];
        let mut items = Vec::new();
        for item in project.video_layers_at(seq, t) {
            match item {
                VideoItem::Single(l) => items.extend(self.gather(project, &l, t, frame_size, provider, depth)?.map(Item::Single)),
                VideoItem::Dissolve { from, to, mix, kind } => {
                    match (self.gather(project, &from, t, frame_size, provider, depth)?, self.gather(project, &to, t, frame_size, provider, depth)?) {
                        (Some(mut a), Some(mut b)) => {
                            if kind.is_slide() || kind.is_push() {
                                // Moves only: the incoming clip over the outgoing one.
                                let (to_off, from_off) = kind.offsets(mix, [frame_size[0] as f32, frame_size[1] as f32]);
                                for (g, o) in [(&mut a, from_off), (&mut b, to_off)] {
                                    g.transform.position[0] += o[0];
                                    g.transform.position[1] += o[1];
                                }
                                items.push(Item::Single(a));
                                items.push(Item::Single(b));
                            } else if kind.is_wipe() {
                                b.wipe = Some(if kind == Transition::Iris { Wipe::iris(mix) } else { Wipe::moving(kind.direction(), mix) });
                                items.push(Item::Single(a));
                                items.push(Item::Single(b));
                            } else {
                                items.push(Item::Dissolve(a, b, mix, kind.dip_color()));
                            }
                        }
                        (Some(one), None) | (None, Some(one)) => items.push(Item::Single(one)),
                        (None, None) => {}
                    }
                }
            }
        }

        let signature = signature(frame_size, &items);
        if unchanged_from == Some(signature) && compositor.output_size().is_some() {
            return Ok(signature);
        }
        let draw: Vec<DrawItem> = items
            .iter()
            .map(|i| match i {
                Item::Single(g) => DrawItem::Layer(input(g)),
                Item::Dissolve(a, b, mix, dip) => DrawItem::Dissolve { from: input(a), to: input(b), mix: *mix, dip: *dip },
            })
            .collect();
        compositor.render_items(frame_size, &draw);
        let keys: HashSet<u64> = items
            .iter()
            .flat_map(|i| match i {
                Item::Single(g) => vec![g.key],
                Item::Dissolve(a, b, ..) => vec![a.key, b.key],
            })
            .collect();
        compositor.retain_layers(|k| keys.contains(&k));
        Ok(signature)
    }

    #[allow(clippy::too_many_arguments)]
    fn gather(&mut self, project: &Project, l: &Layer, t: Time, frame_size: [u32; 2], provider: &mut dyn FrameProvider, depth: usize) -> Result<Option<Gathered>> {
        let picture = match l.clip.nested {
            Some(inner) => self.nested_picture(project, l.clip.id, inner, l.source_time, provider, depth)?,
            None => provider.picture(project, l, frame_size)?,
        };
        let Some(picture) = picture else { return Ok(None) };
        let mask = self.mask_for(l.clip, l.clip.source_time(t), picture.size);
        Ok(Some(Gathered { key: l.clip.id.0, picture, transform: l.transform, blend: l.blend, effects: l.effects.clone(), mask, wipe: None }))
    }

    /// The clip's masks at source time `src` as an image (made again only when they change).
    fn mask_for(&mut self, clip: &reelforge_core::Clip, src: Time, layer: [f32; 2]) -> Option<(u64, Arc<MaskImage>)> {
        if !mask::any_active(&clip.masks) {
            return None;
        }
        let hash = mask::mask_hash(&clip.masks, src, layer);
        let tick = self.tick;
        if let Some(e) = self.masks.get_mut(&clip.id).filter(|e| e.0 == hash) {
            e.2 = tick;
            return Some((hash, e.1.clone()));
        }
        let img = Arc::new(mask::rasterize(&clip.masks, src, layer, MASK_MAX)?);
        self.masks.insert(clip.id, (hash, img.clone(), tick));
        Some((hash, img))
    }

    /// Render composite shot `inner` at its time `t` and read the picture back.
    fn nested_picture(&mut self, project: &Project, clip: ClipId, inner: usize, t: Time, provider: &mut dyn FrameProvider, depth: usize) -> Result<Option<Picture>> {
        let Some(s) = project.sequences.get(inner) else { return Ok(None) };
        if depth >= MAX_NESTING {
            return Ok(None);
        }
        let size = [s.width.max(2), s.height.max(2)];
        let mut n = match self.nested.remove(&clip) {
            Some(n) => n,
            None => {
                let mut compositor = Compositor::new(&self.device, &self.queue);
                // Where the shot has no layers, what's underneath shows through.
                compositor.transparent = true;
                Nested { compositor, signature: None, frame: None, last_used: 0 }
            }
        };
        n.last_used = self.tick;
        let result = self.render_at(project, inner, t, &mut n.compositor, provider, n.signature.filter(|_| n.frame.is_some()), depth + 1);
        let sig = match result {
            Ok(sig) => sig,
            Err(e) => {
                self.nested.insert(clip, n);
                return Err(e);
            }
        };
        if n.signature != Some(sig) || n.frame.is_none() {
            n.frame = n.compositor.read_output().map(|rgba| {
                self.generation += 1;
                VideoFrame { pts: t, width: size[0], height: size[1], rgba: Arc::from(rgba), generation: self.generation }
            });
            n.signature = Some(sig);
        }
        let picture = n.frame.clone().map(|frame| Picture { frame, size: [size[0] as f32, size[1] as f32], media: MediaId::NESTED });
        self.nested.insert(clip, n);
        Ok(picture)
    }
}

fn input(g: &Gathered) -> LayerInput<'_> {
    let mut h = DefaultHasher::new();
    (g.picture.media, g.picture.frame.pts, g.picture.frame.generation).hash(&mut h);
    LayerInput {
        key: g.key,
        content_id: h.finish(),
        pixels: &g.picture.frame.rgba,
        pixel_size: [g.picture.frame.width, g.picture.frame.height],
        layer_size: g.picture.size,
        transform: g.transform,
        blend: g.blend,
        effects: g.effects.clone(),
        mask: g.mask.as_ref().map(|(hash, img)| MaskInput { content_id: *hash, pixels: &img.data, size: [img.width, img.height] }),
        wipe: g.wipe,
    }
}

/// A hash of everything that affects the picture.
fn signature(frame_size: [u32; 2], items: &[Item]) -> u64 {
    let mut h = DefaultHasher::new();
    frame_size.hash(&mut h);
    let layer = |l: &Gathered, h: &mut DefaultHasher| {
        (l.key, l.picture.media, l.picture.frame.pts, l.picture.frame.generation, l.picture.frame.width).hash(h);
        for f in l.transform.position.iter().chain(&l.transform.scale).chain([&l.transform.rotation, &l.transform.opacity]).chain(&l.transform.anchor).chain(&l.transform.crop).chain(&l.picture.size) {
            f.to_bits().hash(h);
        }
        l.transform.pin.map(|p| p.0.map(|c| c.map(f32::to_bits))).hash(h);
        l.blend.hash(h);
        l.mask.as_ref().map(|m| m.0).hash(h);
        l.wipe.map(|w| (w.iris, w.progress.to_bits(), w.axis.map(f32::to_bits))).hash(h);
        for e in &l.effects {
            e.kind.hash(h);
            for v in e.values {
                v.to_bits().hash(h);
            }
            e.table.as_ref().map(|t| t.id).hash(h);
            // A reloaded plugin (new revision) or one that animates with time changes the picture.
            if let Some(p) = &e.plugin {
                p.revision.hash(h);
                e.time.to_bits().hash(h);
            }
        }
    };
    for item in items {
        match item {
            Item::Single(l) => layer(l, &mut h),
            Item::Dissolve(a, b, mix, dip) => {
                layer(a, &mut h);
                layer(b, &mut h);
                mix.to_bits().hash(&mut h);
                dip.map(|c| c.map(f32::to_bits)).hash(&mut h);
            }
        }
    }
    h.finish()
}
