//! Turning a sequence at a moment in time into compositor draw calls, shared
//! by the viewer and export so the two always agree.
//!
//! Where each layer's picture comes from is up to the caller (a
//! [`FrameProvider`]): the viewer wants whatever frame is ready right now from
//! a preview-sized decoder; export waits for the exact frame at full size.
//! Compositions (clips that play another sequence) are handled here: the
//! inner sequence is rendered by its own compositor, recursively, and its
//! picture becomes one layer of the outer one.

use anyhow::Result;
use reelforge_core::mask::{self, MaskImage};
use reelforge_core::{Transition, BlendMode, ClipId, EffectValues, Layer, MediaId, Project, Time, Transform, VideoItem};
use reelforge_media::VideoFrame;
use reelforge_render::{wgpu, Compositor, DrawItem, LayerInput, MaskInput, PixelData, Wipe};
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

/// Compositions inside compositions stop here (a cycle can't be built,
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
    /// Stretched over a bending grid (a mesh warp).
    mesh: Option<reelforge_core::meshwarp::MeshShape>,
    /// A 3D layer, as the camera sees it.
    space: Option<reelforge_core::space3d::LayerSpace>,
    /// An adjustment layer: its effects apply to the frame drawn so far.
    adjustment: bool,
    /// 3D text: a solid drawn into a frame-sized picture.
    solid: Option<reelforge_render::SolidInput>,
    /// Where a solid sits in front of the camera (drawing order among 3D layers).
    sort_depth: Option<f32>,
}

/// Masks are rasterised at most this big (longer side); they're soft-edged
/// alpha, so the renderer's filtering covers the rest.
const MASK_MAX: u32 = 2048;

#[allow(clippy::large_enum_variant)] // short-lived, per frame
enum Item {
    Single(Gathered),
    /// A dissolve, or a dip through a colour.
    Dissolve(Gathered, Gathered, f32, Option<[f32; 3]>),
    /// A layer seen only through a track matte (the layer above, not drawn itself).
    Matted { matte: Gathered, layer: Gathered, kind: reelforge_core::TrackMatte },
}

/// A composition's own renderer and its last picture.
struct Nested {
    compositor: Compositor,
    signature: Option<u64>,
    frame: Option<VideoFrame>,
    last_used: u64,
}

pub struct Scene {
    device: wgpu::Device,
    queue: wgpu::Queue,
    /// Per composition clip.
    nested: HashMap<ClipId, Nested>,
    /// Rasterised masks per clip: (hash of the shape, image, last used tick).
    masks: HashMap<ClipId, (u64, Arc<MaskImage>, u64)>,
    /// Counts top-level renders, to let go of compositions not seen for a while.
    tick: u64,
    generation: u64,
    /// Runs clips' OpenFX effects.
    ofx: crate::ofxhost::OfxRunner,
    /// Optical flow for retimed clips (made when first needed).
    flow: Option<reelforge_render::flowgpu::FlowInterp>,
    /// Each clip's last in-between frame, by what it was made from.
    flow_frames: HashMap<ClipId, (u64, VideoFrame)>,
}

/// The second frame of a blend has a key of its own (it's another layer for the compositor).
const NEXT_FRAME_KEY: u64 = 1 << 62;

impl Scene {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        Self { device: device.clone(), queue: queue.clone(), nested: HashMap::new(), masks: HashMap::new(), tick: 0, generation: 0, ofx: Default::default(), flow: None, flow_frames: HashMap::new() }
    }

    /// Forget composition renders (e.g. a different project was opened).
    pub fn reset(&mut self) {
        self.nested.clear();
        self.masks.clear();
        self.ofx.clear();
        if let Some(f) = &mut self.flow {
            f.clear();
        }
        self.flow_frames.clear();
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
        let sig = self.render_at(project, seq, None, t, compositor, provider, unchanged_from, 0)?;
        let tick = self.tick;
        // Compositions off screen for a few seconds give their GPU memory back.
        self.nested.retain(|_, n| tick - n.last_used < 150);
        self.masks.retain(|_, m| tick - m.2 < 150);
        self.ofx.trim(600);
        Ok(sig)
    }

    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_arguments)]
    fn render_at(
        &mut self,
        project: &Project,
        seq: usize,
        angle: Option<reelforge_core::multicam::Angle>,
        t: Time,
        compositor: &mut Compositor,
        provider: &mut dyn FrameProvider,
        unchanged_from: Option<u64>,
        depth: usize,
    ) -> Result<u64> {
        let Some(s) = project.sequences.get(seq) else { return Ok(0) };
        let frame_size = [s.width.max(2), s.height.max(2)];
        let fps = s.frame_rate.fps();
        // The camera and lights, for 3D layers.
        let scene3d = project.scene3d_at(seq, t);
        let mut items = Vec::new();
        // Each item's track, and its clip's track matte.
        let mut tags: Vec<(usize, Option<reelforge_core::TrackMatte>)> = Vec::new();
        for item in reelforge_core::multicam::layers_at(project, seq, t, angle) {
            let tag = match &item {
                VideoItem::Single(l) => (l.track, l.clip.track_matte),
                VideoItem::Dissolve { to, .. } => (to.track, None),
            };
            match item {
                VideoItem::Single(l) => items.extend(self.gather_retimed(project, &l, t, frame_size, fps, &scene3d, provider, depth)?),
                VideoItem::Dissolve { from, to, mix, kind } => {
                    match (self.gather(project, &from, t, frame_size, fps, &scene3d, provider, depth)?, self.gather(project, &to, t, frame_size, fps, &scene3d, provider, depth)?) {
                        (Some(mut a), Some(mut b)) => {
                            if a.adjustment || b.adjustment {
                                // An adjustment fades in or out: one after the other, by opacity.
                                a.transform.opacity *= 1.0 - mix;
                                b.transform.opacity *= mix;
                                items.push(Item::Single(a));
                                items.push(Item::Single(b));
                            } else if kind.is_slide() || kind.is_push() {
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
            tags.resize(items.len(), tag);
        }
        let items = track_mattes(items, &tags, &s.tracks);
        let mut items = items;

        sort_3d(&mut items);
        let signature = signature(frame_size, &items);
        if unchanged_from == Some(signature) && compositor.output_size().is_some() {
            return Ok(signature);
        }
        let draw: Vec<DrawItem> = items
            .iter()
            .map(|i| match i {
                Item::Single(g) => DrawItem::Layer(input(g)),
                Item::Dissolve(a, b, mix, dip) => DrawItem::Dissolve { from: input(a), to: input(b), mix: *mix, dip: *dip },
                Item::Matted { matte, layer, kind } => DrawItem::Matted { matte: input(matte), layer: input(layer), kind: kind.index() },
            })
            .collect();
        compositor.render_items(frame_size, &draw);
        let keys: HashSet<u64> = items
            .iter()
            .flat_map(|i| match i {
                Item::Single(g) => vec![g.key],
                Item::Dissolve(a, b, ..) => vec![a.key, b.key],
                Item::Matted { matte, layer, .. } => vec![matte.key, layer.key],
            })
            .collect();
        compositor.retain_layers(|k| keys.contains(&k));
        Ok(signature)
    }

    /// A layer, and for retimed footage between two of its frames, those two
    /// frames cross-faded (frame blending), each first pushed along the
    /// motion between them (optical flow) if the clip asks for that.
    #[allow(clippy::too_many_arguments)]
    fn gather_retimed(&mut self, project: &Project, l: &Layer, t: Time, frame_size: [u32; 2], fps: f64, scene3d: &reelforge_core::space3d::Scene3d, provider: &mut dyn FrameProvider, depth: usize) -> Result<Option<Item>> {
        use reelforge_core::FrameBlend;
        let c = l.clip;
        let rate = (c.frame_blend != FrameBlend::Nearest && c.is_retimed() && c.nested.is_none() && c.title.is_none() && c.plane.is_none() && !c.adjustment)
            .then(|| project.media(c.media))
            .flatten()
            .filter(|m| !project.is_still(m.id))
            .and_then(|m| m.info.video.as_ref())
            .map(|v| v.frame_rate.fps())
            .filter(|r| *r > 0.0);
        let Some(media_fps) = rate else { return Ok(self.gather(project, l, t, frame_size, fps, scene3d, provider, depth)?.map(Item::Single)) };
        // Which two frames, and how far from the first to the second.
        let f = l.source_time.as_secs_f64() * media_fps;
        let f0 = (f + 1e-3).floor();
        let mix = (f - f0) as f32;
        if !(0.02..=0.98).contains(&mix) {
            let near = Layer { source_time: Time::from_secs_f64((f.round() + 0.25) / media_fps), ..l.clone() };
            return Ok(self.gather(project, &near, t, frame_size, fps, scene3d, provider, depth)?.map(Item::Single));
        }
        let first = Layer { source_time: Time::from_secs_f64((f0 + 0.25) / media_fps), ..l.clone() };
        // The next frame through a decoder of its own (another clip id), so each reads forwards.
        let next_clip = reelforge_core::Clip { id: ClipId(c.id.0 ^ NEXT_FRAME_KEY), ..c.clone() };
        let second = Layer { clip: &next_clip, source_time: Time::from_secs_f64((f0 + 1.25) / media_fps), ..l.clone() };
        let (Some(mut a), Some(mut b)) = (self.gather(project, &first, t, frame_size, fps, scene3d, provider, depth)?, self.gather(project, &second, t, frame_size, fps, scene3d, provider, depth)?) else {
            return Ok(self.gather(project, l, t, frame_size, fps, scene3d, provider, depth)?.map(Item::Single));
        };
        b.key = c.id.0 ^ NEXT_FRAME_KEY;
        if c.frame_blend == FrameBlend::Flow && a.mesh.is_none() && a.space.is_none() && !l.transform.three_d {
            // The frame in between, worked out on the graphics card.
            let mut h = DefaultHasher::new();
            (c.id, f0 as i64, c.flow_quality, a.picture.frame.generation, b.picture.frame.generation, a.picture.frame.pts, b.picture.frame.pts).hash(&mut h);
            let pair = h.finish();
            let key = pair ^ (mix.to_bits() as u64).rotate_left(17);
            let cached = self.flow_frames.get(&c.id).filter(|(k, _)| *k == key).map(|(_, f)| f.clone());
            let frame = match cached {
                Some(f) => Some(f),
                None => {
                    let interp = self.flow.get_or_insert_with(|| reelforge_render::flowgpu::FlowInterp::new(&self.device, &self.queue));
                    (a.picture.frame.width == b.picture.frame.width && a.picture.frame.height == b.picture.frame.height).then_some(()).and_then(|_| interp.interpolate(c.id.0, pair, &a.picture.frame.pixels, &b.picture.frame.pixels, [a.picture.frame.width, a.picture.frame.height], mix, c.flow_quality)).map(|px| {
                        self.generation += 1;
                        let f = VideoFrame { pts: a.picture.frame.pts, width: px.width, height: px.height, pixels: reelforge_core::frame::Pixels::Float(Arc::new(px)), generation: self.generation };
                        if self.flow_frames.len() > 16 {
                            self.flow_frames.clear();
                        }
                        self.flow_frames.insert(c.id, (key, f.clone()));
                        f
                    })
                }
            };
            if let Some(frame) = frame {
                a.picture.frame = frame;
                return Ok(Some(Item::Single(a)));
            }
        }
        Ok(Some(Item::Dissolve(a, b, mix, None)))
    }

    #[allow(clippy::too_many_arguments)]
    fn gather(&mut self, project: &Project, l: &Layer, t: Time, frame_size: [u32; 2], fps: f64, scene3d: &reelforge_core::space3d::Scene3d, provider: &mut dyn FrameProvider, depth: usize) -> Result<Option<Gathered>> {
        let full = [frame_size[0] as f32, frame_size[1] as f32];
        if let Some(spec) = l.clip.title.as_ref().filter(|s| s.is_3d() && l.clip.nested.is_none() && !l.clip.adjustment) {
            if l.transform.pin.is_none() && l.clip.mesh_at(t).is_none() {
                // The flat title is still laid out (cached): the viewer uses
                // its bounds to select and outline the text's front face.
                let _ = provider.picture(project, l, frame_size)?;
                return Ok(Some(solid_layer(l, spec, frame_size, scene3d)));
            }
        }
        let picture = match (l.clip.nested, l.clip.plane) {
            // A plane: one pixel of its colour, stretched over the frame.
            (_, Some(c)) if !l.clip.adjustment => Some(Picture { frame: VideoFrame::from_rgba(Time::ZERO, 1, 1, Arc::from(c.to_vec()), u32::from_le_bytes(c) as u64), size: full, media: MediaId::GENERATED }),
            // An adjustment layer has no picture: it's drawn from the frame below.
            _ if l.clip.adjustment => Some(Picture { frame: VideoFrame::from_rgba(Time::ZERO, 1, 1, Arc::from(vec![0u8; 4]), 0), size: full, media: MediaId::GENERATED }),
            (Some(inner), _) => self.nested_picture(project, l.clip.id, inner, l.clip.angle, l.source_time, provider, depth)?,
            (None, _) => provider.picture(project, l, frame_size)?,
        };
        let Some(mut picture) = picture else { return Ok(None) };
        // OpenFX effects run on the picture first, on the CPU (not on an
        // adjustment layer, which has no picture of its own).
        if !l.clip.adjustment {
            let steps = reelforge_core::ofx::steps(&l.clip.effects, l.clip.source_time(t));
            if !steps.is_empty() {
                let mut mh = DefaultHasher::new();
                picture.media.hash(&mut mh);
                // A plane is one pixel stretched: plugins get it at full size.
                if picture.frame.width * picture.frame.height == 1 {
                    let [w, h] = picture.size.map(|v| (v.round() as u32).clamp(1, 8192));
                    let one = picture.frame.rgba();
                    let rgba: Vec<u8> = one.iter().copied().cycle().take(w as usize * h as usize * 4).collect();
                    picture.frame = VideoFrame::from_rgba(picture.frame.pts, w, h, Arc::from(rgba), picture.frame.generation);
                }
                // Other frames of the clip (for plugins that read them): its
                // media's, from the provider; anything else is the same picture.
                let media = (l.clip.nested.is_none() && l.clip.plane.is_none() && l.clip.title.is_none()).then(|| project.media(l.clip.media)).flatten();
                let range = media.map_or((0.0, 1e6), |m| (0.0, (m.info.duration.as_secs_f64() * fps - 1.0).max(0.0)));
                let here = picture.frame.clone();
                let mut fetch = |f: f64| -> Option<VideoFrame> {
                    if media.is_none() {
                        return Some(here.clone());
                    }
                    let mut other = l.clone();
                    other.source_time = Time::from_secs_f64((f / fps).max(0.0));
                    provider.picture(project, &other, frame_size).ok().flatten().map(|p| p.frame)
                };
                picture.frame = self.ofx.apply(l.clip.id, &steps, &picture.frame, mh.finish(), fps, range, &mut fetch);
            }
        }
        let mask = self.mask_for(l.clip, l.clip.source_time(t), picture.size);
        // Keyed to the clip's own time, like its transform (a still's media time is always 0).
        if l.clip.adjustment {
            // Covers the frame: only opacity and crop of its transform apply.
            let transform = Transform { opacity: l.transform.opacity, crop: l.transform.crop, ..Default::default() };
            let effects = l.effects.iter().filter(|e| !e.is_identity()).cloned().collect();
            return Ok(Some(Gathered { key: l.clip.id.0, picture, transform, blend: l.blend, effects, mask, wipe: None, mesh: None, space: None, adjustment: true, solid: None, sort_depth: None }));
        }
        let mesh = l.clip.mesh_at(t);
        let mut effects = l.effects.clone();
        // A 3D layer (not mesh-warped or corner-pinned): seen through the camera,
        // blurred by its depth of field.
        let space = (l.transform.three_d && mesh.is_none() && l.transform.pin.is_none()).then(|| reelforge_core::space3d::layer_space(&l.transform, picture.size, scene3d));
        if let Some(sp) = &space {
            let blur = sp.blur(&scene3d.camera);
            if blur > 0.25 {
                let mut values = [0.0; reelforge_core::effects::MAX_PARAMS];
                values[0] = blur;
                effects.push(EffectValues { kind: reelforge_core::EffectKind::Blur, values, table: None, plugin: None, time: 0.0, prev: None, particles: None, pieces: None, fluid: None, gpu: [0.0; 4] });
            }
        }
        Ok(Some(Gathered { key: l.clip.id.0, picture, transform: l.transform, blend: l.blend, effects, mask, wipe: None, mesh, space, adjustment: false, solid: None, sort_depth: None }))
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

    /// Render composition `inner` at its time `t` and read the picture back.
    #[allow(clippy::too_many_arguments)]
    fn nested_picture(&mut self, project: &Project, clip: ClipId, inner: usize, angle: Option<reelforge_core::multicam::Angle>, t: Time, provider: &mut dyn FrameProvider, depth: usize) -> Result<Option<Picture>> {
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
        let result = self.render_at(project, inner, angle, t, &mut n.compositor, provider, n.signature.filter(|_| n.frame.is_some()), depth + 1);
        let sig = match result {
            Ok(sig) => sig,
            Err(e) => {
                self.nested.insert(clip, n);
                return Err(e);
            }
        };
        if n.signature != Some(sig) || n.frame.is_none() {
            // Read back in floating point: the composition's own precision
            // (10-bit footage, highlights above white) carries on outside it.
            n.frame = n.compositor.read_output_linear().map(|f| {
                self.generation += 1;
                VideoFrame { pts: t, width: f.width, height: f.height, pixels: reelforge_core::frame::Pixels::Float(Arc::new(f)), generation: self.generation }
            });
            n.signature = Some(sig);
        }
        let picture = n.frame.clone().map(|frame| Picture { frame, size: [size[0] as f32, size[1] as f32], media: MediaId::NESTED });
        self.nested.insert(clip, n);
        Ok(picture)
    }
}

/// 3D text: the letters as a solid, seen through the camera and lit, drawn
/// into a frame-sized picture that's then layered like any other (opacity,
/// blend mode and effects apply; depth of field blurs it by its distance).
fn solid_layer(l: &Layer, spec: &reelforge_core::TitleSpec, frame_size: [u32; 2], scene3d: &reelforge_core::space3d::Scene3d) -> Gathered {
    let full = [frame_size[0] as f32, frame_size[1] as f32];
    // Always in 3D: at Z 0 with no turn it sits where the flat title would.
    let tr = Transform { three_d: true, ..l.transform };
    let space = reelforge_core::space3d::solid_space(&tr, scene3d);
    let solid = reelforge_render::SolidInput {
        mesh: reelforge_render::text3d::text_mesh(spec, frame_size),
        space,
        face: spec.color,
        side: spec.side_color.unwrap_or(spec.color),
    };
    let flat = reelforge_core::space3d::layer_space(&tr, full, scene3d);
    let mut effects = l.effects.clone();
    let blur = flat.blur(&scene3d.camera);
    if blur > 0.25 {
        let mut values = [0.0; reelforge_core::effects::MAX_PARAMS];
        // The picture is frame pixels, the blur was worked out in layer pixels.
        values[0] = blur * flat.scale;
        effects.push(EffectValues { kind: reelforge_core::EffectKind::Blur, values, table: None, plugin: None, time: 0.0, prev: None, particles: None, pieces: None, fluid: None, gpu: [0.0; 4] });
    }
    let picture = Picture { frame: VideoFrame::from_rgba(Time::ZERO, 1, 1, Arc::from(vec![0u8; 4]), 0), size: full, media: MediaId::GENERATED };
    Gathered {
        key: l.clip.id.0,
        picture,
        transform: Transform { opacity: l.transform.opacity, ..Default::default() },
        blend: l.blend,
        effects,
        mask: None,
        wipe: None,
        mesh: None,
        space: None,
        adjustment: false,
        solid: Some(solid),
        sort_depth: Some(flat.depth),
    }
}

/// A fingerprint of everything that decides what sequence `seq` looks like at
/// `t`, worked out from the project alone (nothing is decoded or drawn): which
/// media frames show (from which file: original or proxy, per `sources`),
/// titles, compositions (recursively), transforms, blend modes, effects,
/// masks and transitions. Two moments with the same recipe look identical, so
/// a rendered frame can be reused while its recipe is unchanged.
pub fn frame_recipe(project: &Project, seq: usize, t: Time, sources: &HashMap<MediaId, std::path::PathBuf>) -> u64 {
    let mut h = DefaultHasher::new();
    recipe_into(project, seq, None, t, sources, 0, &mut h);
    h.finish()
}

fn recipe_into(project: &Project, seq: usize, angle: Option<reelforge_core::multicam::Angle>, t: Time, sources: &HashMap<MediaId, std::path::PathBuf>, depth: usize, h: &mut DefaultHasher) {
    let Some(s) = project.sequences.get(seq) else { return };
    let frame_size = [s.width.max(2), s.height.max(2)];
    frame_size.hash(h);
    let scene3d = project.scene3d_at(seq, t);
    let layer = |l: &Layer, h: &mut DefaultHasher| {
        let clip = l.clip;
        clip.id.hash(h);
        // Retimed footage blended between frames looks different for the same time.
        if clip.is_retimed() {
            clip.frame_blend.hash(h);
        }
        let layer_size = match (clip.nested, &clip.title) {
            _ if clip.adjustment => {
                4u8.hash(h);
                [frame_size[0] as f32, frame_size[1] as f32]
            }
            _ if clip.plane.is_some() => {
                5u8.hash(h);
                clip.plane.hash(h);
                [frame_size[0] as f32, frame_size[1] as f32]
            }
            (Some(inner), _) => {
                1u8.hash(h);
                if depth < MAX_NESTING {
                    clip.angle.hash(h);
                    reelforge_core::multicam::grid().hash(h);
                    recipe_into(project, inner, clip.angle, l.source_time, sources, depth + 1, h);
                }
                project.sequences.get(inner).map_or([0.0; 2], |s| [s.width as f32, s.height as f32])
            }
            (None, Some(spec)) => {
                2u8.hash(h);
                let at = crate::title_clock(l);
                spec.content_hash_at(frame_size, at.0, at.1).hash(h);
                [frame_size[0] as f32, frame_size[1] as f32]
            }
            (None, None) => {
                3u8.hash(h);
                match project.media(clip.media) {
                    Some(m) => {
                        let path = sources.get(&m.id).unwrap_or(&m.path);
                        path.hash(h);
                        // The media frame showing, not the exact time inside it.
                        match &m.info.video {
                            Some(v) if m.info.duration > Time::ZERO && v.frame_rate.fps() > 0.0 => l.source_time.to_frame(v.frame_rate).hash(h),
                            _ => {} // a still: the same picture throughout
                        }
                        // Missing media shows a stand-in picture.
                        reelforge_core::imageseq::media_exists(path).hash(h);
                        m.layer_size().map_or([0.0; 2], |s| [s[0] as f32, s[1] as f32])
                    }
                    None => [0.0; 2],
                }
            }
        };
        let tr = &l.transform;
        for f in tr.position.iter().chain(&tr.scale).chain([&tr.rotation, &tr.opacity]).chain(&tr.anchor).chain(&tr.crop) {
            f.to_bits().hash(h);
        }
        tr.pin.map(|p| p.0.map(|c| c.map(f32::to_bits))).hash(h);
        hash_mesh(clip.mesh_at(t).as_ref(), h);
        if clip.title.as_ref().is_some_and(|s| s.is_3d()) {
            // 3D text: where the camera sees it and how it's lit.
            let sp = reelforge_core::space3d::solid_space(&Transform { three_d: true, ..*tr }, &scene3d);
            hash_solid(&sp, h);
            scene3d.camera.dof.map(|(f, a)| (f.to_bits(), a.to_bits())).hash(h);
            tr.depth.to_bits().hash(h);
        } else if tr.three_d {
            // Where it is and how it's lit depends on the camera and lights too.
            hash_space(Some(&reelforge_core::space3d::layer_space(tr, layer_size, &scene3d)), h);
            scene3d.camera.dof.map(|(f, a)| (f.to_bits(), a.to_bits())).hash(h);
        }
        l.blend.hash(h);
        clip.track_matte.hash(h);
        for e in &l.effects {
            e.kind.hash(h);
            for v in e.values {
                v.to_bits().hash(h);
            }
            e.table.as_ref().map(|t| t.id).hash(h);
            e.particles.as_ref().map(|p| p.id).hash(h);
            // Flicker, shake, grain… change with time.
            if e.moves_with_time() {
                e.time.to_bits().hash(h);
            }
            if let Some(p) = &e.plugin {
                p.revision.hash(h);
                e.time.to_bits().hash(h);
            }
        }
        if !clip.adjustment {
            for st in reelforge_core::ofx::steps(&clip.effects, clip.source_time(t)) {
                st.hash_into(h);
            }
        }
        if mask::any_active(&clip.masks) {
            mask::mask_hash(&clip.masks, clip.source_time(t), layer_size).hash(h);
        }
    };
    for item in reelforge_core::multicam::layers_at(project, seq, t, angle) {
        match item {
            VideoItem::Single(l) => {
                0u8.hash(h);
                layer(&l, h);
            }
            VideoItem::Dissolve { from, to, mix, kind } => {
                1u8.hash(h);
                layer(&from, h);
                layer(&to, h);
                mix.to_bits().hash(h);
                kind.hash(h);
            }
        }
    }
}

fn input(g: &Gathered) -> LayerInput<'_> {
    let mut h = DefaultHasher::new();
    (g.picture.media, g.picture.frame.pts, g.picture.frame.generation).hash(&mut h);
    if let Some(s) = &g.solid {
        return LayerInput {
            key: g.key,
            content_id: s.content_id(),
            pixels: PixelData::Solid(s),
            pixel_size: [g.picture.size[0] as u32, g.picture.size[1] as u32],
            layer_size: g.picture.size,
            transform: g.transform,
            blend: g.blend,
            effects: g.effects.clone(),
            mask: None,
            wipe: g.wipe,
            mesh: None,
            space: None,
        };
    }
    LayerInput {
        key: g.key,
        content_id: h.finish(),
        pixels: if g.adjustment { PixelData::Below } else { (&g.picture.frame.pixels).into() },
        pixel_size: if g.adjustment { [g.picture.size[0] as u32, g.picture.size[1] as u32] } else { [g.picture.frame.width, g.picture.frame.height] },
        layer_size: g.picture.size,
        transform: g.transform,
        blend: g.blend,
        effects: g.effects.clone(),
        mask: g.mask.as_ref().map(|(hash, img)| MaskInput { content_id: *hash, pixels: &img.data, size: [img.width, img.height], keep: &img.keep }),
        wipe: g.wipe,
        mesh: g.mesh.clone(),
        space: g.space.as_ref(),
    }
}

/// 3D layers next to each other in the stack are drawn furthest first (2D
/// layers between them keep their places, as in HitFilm).
/// Pair each layer with a track matte with the layer on the video track
/// directly above (drawn into its matte, not by itself). With nothing there,
/// an alpha or luma matte hides the layer and an inverted one shows it all.
fn track_mattes(items: Vec<Item>, tags: &[(usize, Option<reelforge_core::TrackMatte>)], tracks: &[reelforge_core::Track]) -> Vec<Item> {
    use reelforge_core::TrackMatte;
    let above = |ti: usize| (ti + 1..tracks.len()).find(|&j| tracks[j].kind == reelforge_core::TrackKind::Video && !tracks[j].muted);
    let mut slots: Vec<Option<Item>> = items.into_iter().map(Some).collect();
    let mut out = Vec::with_capacity(slots.len());
    let mut i = 0;
    while i < slots.len() {
        let (track, matte) = tags[i];
        if let (Some(kind), Some(Item::Single(_))) = (matte, &slots[i]) {
            let next = i + 1 < slots.len() && Some(tags[i + 1].0) == above(track) && (i + 2 >= slots.len() || tags[i + 2].0 != tags[i + 1].0);
            if next && matches!(slots[i + 1], Some(Item::Single(_))) {
                let (Some(Item::Single(layer)), Some(Item::Single(m))) = (slots[i].take(), slots[i + 1].take()) else { unreachable!() };
                out.push(Item::Matted { matte: m, layer, kind });
                i += 2;
                continue;
            }
            if !next && matches!(kind, TrackMatte::Alpha | TrackMatte::Luma) {
                i += 1;
                continue;
            }
        }
        if let Some(it) = slots[i].take() {
            out.push(it);
        }
        i += 1;
    }
    out
}

fn sort_3d(items: &mut [Item]) {
    let is_3d = |i: &Item| matches!(i, Item::Single(g) if g.space.is_some() || g.sort_depth.is_some());
    let mut start = 0;
    while start < items.len() {
        if !is_3d(&items[start]) {
            start += 1;
            continue;
        }
        let mut end = start;
        while end < items.len() && is_3d(&items[end]) {
            end += 1;
        }
        // Stable, so layers at the same depth keep their track order.
        items[start..end].sort_by(|a, b| {
            let d = |i: &Item| match i {
                Item::Single(g) => g.space.as_ref().map(|s| s.depth).or(g.sort_depth).unwrap_or(0.0),
                _ => 0.0,
            };
            d(b).total_cmp(&d(a))
        });
        start = end;
    }
}

fn hash_space(s: Option<&reelforge_core::space3d::LayerSpace>, h: &mut DefaultHasher) {
    if let Some(s) = s {
        for v in s.mvp.iter().flatten() {
            v.to_bits().hash(h);
        }
        if let Some(l) = &s.lighting {
            for v in l.origin.iter().chain(&l.u).chain(&l.v).chain(&l.camera) {
                v.to_bits().hash(h);
            }
            for g in &l.lights {
                for v in g.position.iter().chain(&g.direction).chain(&g.color).chain(&g.cone).chain([&g.radius]) {
                    v.to_bits().hash(h);
                }
                (g.kind, g.falloff, g.env.map(|e| e.id)).hash(h);
                g.gpu().iter().flatten().for_each(|v| v.to_bits().hash(h));
            }
        }
    }
}

fn hash_solid(s: &reelforge_core::space3d::SolidSpace, h: &mut DefaultHasher) {
    for v in s.mvp.iter().flatten().chain(&s.camera).chain(&s.studio) {
        v.to_bits().hash(h);
    }
    for g in &s.lights {
        for v in g.position.iter().chain(&g.direction).chain(&g.color).chain(&g.cone).chain([&g.radius]) {
            v.to_bits().hash(h);
        }
        (g.kind, g.falloff, g.env.map(|e| e.id)).hash(h);
        g.gpu().iter().flatten().for_each(|v| v.to_bits().hash(h));
    }
}

fn hash_mesh(m: Option<&reelforge_core::meshwarp::MeshShape>, h: &mut DefaultHasher) {
    if let Some(m) = m {
        (m.cols, m.rows).hash(h);
        for p in &m.points {
            p.map(f32::to_bits).hash(h);
        }
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
        hash_mesh(l.mesh.as_ref(), h);
        hash_space(l.space.as_ref(), h);
        l.solid.as_ref().map(|s| s.content_id()).hash(h);
        l.blend.hash(h);
        l.mask.as_ref().map(|m| m.0).hash(h);
        l.wipe.map(|w| (w.iris, w.progress.to_bits(), w.axis.map(f32::to_bits))).hash(h);
        for e in &l.effects {
            e.kind.hash(h);
            for v in e.values {
                v.to_bits().hash(h);
            }
            e.table.as_ref().map(|t| t.id).hash(h);
            e.particles.as_ref().map(|p| p.id).hash(h);
            if e.moves_with_time() {
                e.time.to_bits().hash(h);
            }
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
            Item::Matted { matte, layer: l, kind } => {
                2u8.hash(&mut h);
                layer(matte, &mut h);
                layer(l, &mut h);
                kind.hash(&mut h);
            }
        }
    }
    h.finish()
}


#[cfg(test)]
mod recipe_tests {
    use super::*;
    use reelforge_core::{Clip, Effect, EffectKind, FrameRate, MediaAsset, MediaInfo, Sequence, VideoStreamInfo};

    fn project() -> Project {
        let mut p = Project::new();
        p.media.push(MediaAsset {
            id: MediaId(1),
            path: "/nowhere/a.mp4".into(),
            name: "a.mp4".into(),
            info: MediaInfo {
                duration: Time::from_secs_f64(10.0),
                video: Some(VideoStreamInfo { width: 640, height: 360, frame_rate: FrameRate::new(25, 1), codec: "h264".into(), time_base: reelforge_core::Rational::new(1, 25), hdr: None }),
                audio: None,
                container: String::new(),
            },
            conform: None,
        });
        let mut seq = Sequence::new("T", 640, 360, FrameRate::new(25, 1));
        seq.tracks[0].clips = vec![Clip::new(ClipId(1), MediaId(1), Time::ZERO, Time::ZERO, Time::from_secs_f64(10.0), None)];
        p.sequences = vec![seq];
        p
    }

    #[test]
    fn recipes_change_with_what_shows_and_nothing_else() {
        let mut p = project();
        let none = HashMap::new();
        let at = |p: &Project, s: f64| frame_recipe(p, 0, Time::from_secs_f64(s), &none);
        // Two moments inside one media frame (25 fps): same picture.
        assert_eq!(at(&p, 1.0), at(&p, 1.01));
        assert_ne!(at(&p, 1.0), at(&p, 1.04), "the next media frame");
        let before = at(&p, 1.0);
        // An edit that changes the picture changes the recipe…
        p.sequences[0].tracks[0].clips[0].transform.opacity = 0.5;
        assert_ne!(at(&p, 1.0), before);
        p.sequences[0].tracks[0].clips[0].transform.opacity = 1.0;
        assert_eq!(at(&p, 1.0), before, "…and undoing it brings the same recipe back");
        // Playing a proxy instead of the original is a different picture.
        let proxy = HashMap::from([(MediaId(1), std::path::PathBuf::from("/cache/a-proxy.mp4"))]);
        assert_ne!(frame_recipe(&p, 0, Time::from_secs_f64(1.0), &proxy), before);
        // An empty stretch of timeline.
        assert_eq!(at(&p, 20.0), at(&p, 30.0));
    }

    #[test]
    fn effects_that_move_by_themselves_make_every_moment_a_new_picture() {
        let none = HashMap::new();
        let mut p = Project::new();
        let mut seq = Sequence::new("S", 64, 32, FrameRate::new(25, 1));
        seq.tracks[0].clips = vec![Clip::new_plane(ClipId(1), Time::ZERO, Time::from_secs_f64(5.0), [90, 90, 90, 255])];
        p.sequences = vec![seq];
        let at = |p: &Project, s: f64| frame_recipe(p, 0, Time::from_secs_f64(s), &none);
        assert_eq!(at(&p, 1.0), at(&p, 2.0), "a plain plane is the same picture throughout");
        for kind in [EffectKind::FilmGrain, EffectKind::CameraShake, EffectKind::Glitch, EffectKind::LightLeaks, EffectKind::Crt] {
            p.sequences[0].tracks[0].clips[0].effects = vec![Effect::new(kind)];
            assert_ne!(at(&p, 1.0), at(&p, 1.04), "{kind:?}");
        }
        // A lens flare stays put unless it's keyframed.
        p.sequences[0].tracks[0].clips[0].effects = vec![Effect::new(EffectKind::LensFlare)];
        assert_eq!(at(&p, 1.0), at(&p, 2.0));
    }
}

#[cfg(test)]
mod layer_tests {
    use super::*;
    use reelforge_core::{Clip, Effect, EffectKind, FrameRate, Sequence, TrackKind};

    struct Nothing;
    impl FrameProvider for Nothing {
        fn picture(&mut self, _: &Project, _: &Layer, _: [u32; 2]) -> Result<Option<Picture>> {
            Ok(None)
        }
    }

    #[test]
    fn planes_points_and_adjustment_layers_render() {
        let Some((device, queue)) = reelforge_render::headless_device() else {
            eprintln!("no GPU adapter; skipping");
            return;
        };
        let mut p = Project::new();
        let mut seq = Sequence::new("T", 64, 32, FrameRate::new(25, 1));
        let secs = Time::from_secs_f64;
        let red = Clip::new_plane(ClipId(1), Time::ZERO, secs(5.0), [255, 0, 0, 255]);
        let mut grey = Clip::new_adjustment(ClipId(2), Time::ZERO, secs(5.0));
        let mut fx = Effect::new(EffectKind::ColorCorrect);
        fx.params[2].value = 0.0; // no saturation
        grey.effects.push(fx);
        // A small blue plane above, moved right by a point it's parented to.
        let mut blue = Clip::new_plane(ClipId(3), Time::ZERO, secs(5.0), [0, 0, 255, 255]);
        blue.transform.scale = [0.25, 0.25];
        blue.parent = Some(ClipId(4));
        let mut point = Clip::new_scene(ClipId(4), Time::ZERO, secs(5.0), reelforge_core::space3d::SceneObject::Point, [64.0, 32.0]);
        point.effects[0].params[reelforge_core::effects::point::X].value = 20.0;
        while seq.tracks.iter().filter(|t| t.kind == TrackKind::Video).count() < 4 {
            seq.tracks.insert(0, reelforge_core::Track::new("V", TrackKind::Video));
        }
        for (i, c) in [red, grey, blue, point].into_iter().enumerate() {
            seq.tracks[i].clips = vec![c];
        }
        p.sequences = vec![seq];
        let mut scene = Scene::new(&device, &queue);
        let mut comp = Compositor::new(&device, &queue);
        scene.render(&p, 0, secs(1.0), &mut comp, &mut Nothing, None).unwrap();
        let img = comp.read_output().unwrap();
        let px = |x: usize, y: usize| &img[(y * 64 + x) * 4..][..3];
        // Red with no saturation is grey; the blue square above it isn't touched.
        let g = px(4, 4);
        assert!(g[0] == g[1] && g[1] == g[2] && g[0] > 60 && g[0] < 160, "{g:?}");
        assert_eq!(px(32 + 20, 16), [0, 0, 255], "the blue plane follows its point");
        assert_ne!(px(32, 16), [0, 0, 255]);
        // Recipes see the plane's colour and the adjustment layer.
        let none = HashMap::new();
        let before = frame_recipe(&p, 0, secs(1.0), &none);
        p.sequences[0].tracks[0].clips[0].plane = Some([0, 255, 0, 255]);
        assert_ne!(frame_recipe(&p, 0, secs(1.0), &none), before);
    }

    #[test]
    fn a_track_matte_shows_a_layer_through_the_one_above() {
        let Some((device, queue)) = reelforge_render::headless_device() else { return };
        let secs = Time::from_secs_f64;
        let mut seq = Sequence::new("T", 64, 32, FrameRate::new(25, 1));
        while seq.tracks.iter().filter(|t| t.kind == TrackKind::Video).count() < 2 {
            seq.tracks.insert(0, reelforge_core::Track::new("V", TrackKind::Video));
        }
        let mut red = Clip::new_plane(ClipId(1), Time::ZERO, secs(5.0), [255, 0, 0, 255]);
        red.track_matte = Some(reelforge_core::TrackMatte::Alpha);
        let mut white = Clip::new_plane(ClipId(2), Time::ZERO, secs(2.0), [255, 255, 255, 255]);
        white.transform.scale = [0.25, 0.25];
        seq.tracks[0].clips = vec![red];
        seq.tracks[1].clips = vec![white];
        let mut p = Project::new();
        p.sequences = vec![seq];
        let mut scene = Scene::new(&device, &queue);
        let mut comp = Compositor::new(&device, &queue);
        let mut shot = |p: &Project, s: f64| {
            scene.render(p, 0, secs(s), &mut comp, &mut Nothing, None).unwrap();
            comp.read_output().unwrap()
        };
        let px = |img: &[u8], x: usize, y: usize| [img[(y * 64 + x) * 4], img[(y * 64 + x) * 4 + 1], img[(y * 64 + x) * 4 + 2]];
        let img = shot(&p, 1.0);
        assert_eq!(px(&img, 32, 16), [255, 0, 0], "red through the square (and the square itself not drawn)");
        assert_eq!(px(&img, 2, 2), [0, 0, 0]);
        // Nothing above (after 2 s): an alpha matte hides it, an inverted one shows it.
        assert_eq!(px(&shot(&p, 3.0), 32, 16), [0, 0, 0]);
        p.sequences[0].tracks[0].clips[0].track_matte = Some(reelforge_core::TrackMatte::AlphaInverted);
        assert_eq!(px(&shot(&p, 3.0), 32, 16), [255, 0, 0]);
        let img = shot(&p, 1.0);
        assert_eq!((px(&img, 32, 16), px(&img, 2, 2)), ([0, 0, 0], [255, 0, 0]));
    }
}
