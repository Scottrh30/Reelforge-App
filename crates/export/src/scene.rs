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

    /// A 2D title layer laid out on a `frame_size` canvas, drawn `sharp`
    /// times as finely (it's shown that much bigger) and on only the part of
    /// the canvas round the middle that it reaches unless `whole` (see
    /// `reelforge_render::text::render_title_sharp`). The picture's `size`
    /// is what it covers, centred on the canvas.
    fn title(&mut self, project: &Project, layer: &Layer, frame_size: [u32; 2], sharp: f32, whole: bool) -> Result<Option<Picture>> {
        let _ = (sharp, whole);
        self.picture(project, layer, frame_size)
    }

    /// Wait for each exact frame from now on (`true`), or show what's ready
    /// (`false`). Returns what it was, to put it back. (Providers that always
    /// wait needn't do anything.)
    fn wait_for_frames(&mut self, wait: bool) -> bool {
        let _ = wait;
        true
    }
}

/// How much finer to draw a 2D title shown `scale` times its size: 1 up to
/// 100 %, then in steps of a quarter of an octave (about 19 %), so a zoom
/// redraws it now and then rather than every frame; at most 8.
pub fn title_sharpness(scale: [f32; 2]) -> f32 {
    let s = scale[0].abs().max(scale[1].abs());
    if !s.is_finite() || s <= 1.0 {
        return 1.0;
    }
    (2.0f32).powf((s.log2() * 4.0 - 1e-4).ceil() / 4.0).min(8.0)
}

struct Gathered {
    key: u64,
    picture: Picture,
    transform: Transform,
    blend: BlendMode,
    effects: Vec<EffectValues>,
    /// The layer's masks as an alpha image, and its hash.
    mask: Option<(u64, Arc<MaskImage>)>,
    /// Its masks' refined edge (radius, smoothness, shift).
    refine: Option<[f32; 4]>,
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
    /// Exporting at another frame rate: frames a second drawn (instead of the sequence's).
    pub out_fps: Option<f64>,
    /// How all footage fills in between its frames, over each clip's own choice.
    pub between: Option<reelforge_core::FrameBlend>,
    /// The pixel shape (width ÷ height) of the sequence being drawn.
    par: f32,
    /// How much finer than its own pixels the sequence being drawn is drawn:
    /// a composition scaled up in the one it's in is drawn that much finer,
    /// so the text and shapes in it stay crisp (1 at the top).
    zoom: f32,
    /// After a jump, at most this many frames are drawn to catch a simulation
    /// up before showing the frame; a longer jump spreads them over it and the
    /// simulation is approximate until filled in (when [`Self::idle`]).
    /// `None` (the default: exports, rendered previews): every frame, so
    /// it's always exact.
    pub max_catch_up: Option<usize>,
    /// Nothing's playing: a simulation left approximate by a long jump is
    /// filled in a few frames at a time, then the picture drawn again.
    pub idle: bool,
    /// The simulation being filled in, and how far.
    refining: Option<Refine>,
    /// Counts simulations filled in (part of the picture's signature: the
    /// same frame looks different afterwards).
    sim_rev: u64,
    /// A simulation was just filled in: the frame is drawn again.
    redraw: bool,
}

/// A simulation left approximate by a long jump, being filled in: a copy of
/// it stepped through its layer's frames from its last exact checkpoint up to
/// the frame shown, in order, then put in its place.
struct Refine {
    /// The frame shown when it started (moving on stops it).
    at: Time,
    /// The simulation's key and kind, and its copy's.
    key: u64,
    kind: reelforge_core::EffectKind,
    shadow: u64,
    track: usize,
    clip: reelforge_core::Clip,
    /// Timeline times to step it through, and the next.
    times: Vec<Time>,
    next: usize,
    /// Whose shape it flows round (see [`reelforge_core::fluid::FluidJob::obstacle_from`]).
    obstacle_from: u8,
}

/// Frames filled in per tick when idle.
const REFINE_FRAMES: usize = 6;
/// Marks a simulation copy's key apart from the simulation's.
const SHADOW: u64 = 0x5bd1_e995_9e37_79b9;

/// The most a composition is drawn finer than its own size (and the most
/// pixels across it's drawn at).
const MAX_ZOOM: f32 = 4.0;
const MAX_ZOOM_PX: f32 = 8192.0;

/// How much finer to draw a `size` composition seen at `scale` inside one
/// drawn `outer` times finer: as finely as it's scaled up (in quarter
/// steps of doubling, so a scale animating doesn't redraw at every frame).
pub fn nested_zoom(scale: [f32; 2], outer: f32, size: [u32; 2]) -> f32 {
    let want = title_sharpness(scale) * if outer.is_finite() { outer.max(1.0) } else { 1.0 };
    let fit = MAX_ZOOM_PX / (size[0].max(size[1]).max(1) as f32);
    want.min(MAX_ZOOM).min(fit).max(1.0)
}

/// The second frame of a blend has a key of its own (it's another layer for the compositor).
const NEXT_FRAME_KEY: u64 = 1 << 62;
/// A layer's Motion blur: where its content was at the shutter's opening and
/// closing (around `t`, as the effect's angle and phase say), as maps from
/// where it is now (layer pixels from its centre), written into the effect's
/// spare values for the GPU. A layer that doesn't move then, or one in 3D or
/// corner-pinned (not blurred this way), loses the effect.
fn motion_blur(project: &Project, seq: usize, l: &mut Layer, t: Time, frame_size: [u32; 2], fps: f64) {
    use reelforge_core::effects::motion_blur as mb;
    let Some(i) = l.effects.iter().position(|v| v.kind == reelforge_core::EffectKind::MotionBlur) else { return };
    let full = [frame_size[0] as f32, frame_size[1] as f32];
    let flat = |tr: &Transform| !tr.three_d && tr.valid_pin().is_none();
    let frame = project.sequences.get(seq).map_or(Time::from_secs_f64(1.0 / fps.max(1.0)), |s| s.frame_rate.frame_duration());
    let maps = (|| {
        if !flat(&l.transform) || !fps.is_finite() || fps <= 0.0 {
            return None;
        }
        let v = l.effects.get(i)?.values;
        let (open, phase) = ((v[mb::SHUTTER].clamp(0.0, 720.0) / 360.0) as f64, (v[mb::PHASE].clamp(-360.0, 360.0) / 360.0) as f64);
        let inv = l.transform.matrix(full).inverse()?;
        let at = |frames: f64| -> Option<[f32; 6]> {
            let when = Time::from_secs_f64((t.as_secs_f64() + frames / fps).max(0.0));
            let then = project.clip_layer(seq, l.track, l.clip, when, frame).transform;
            if !flat(&then) {
                return None;
            }
            let m = then.matrix(full);
            // inv ∘ m: where content now at q (layer pixels) was then, in today's layer pixels.
            let r = [inv.a * m.a + inv.c * m.b, inv.b * m.a + inv.d * m.b, inv.a * m.c + inv.c * m.d, inv.b * m.c + inv.d * m.d, inv.a * m.tx + inv.c * m.ty + inv.tx, inv.b * m.tx + inv.d * m.ty + inv.ty];
            r.iter().all(|x| x.is_finite()).then_some(r)
        };
        Some((at(phase)?, at(phase + open)?))
    })();
    let still = |m: &[f32; 6]| (m[0] - 1.0).abs() < 1e-4 && m[1].abs() < 1e-4 && m[2].abs() < 1e-4 && (m[3] - 1.0).abs() < 1e-4 && m[4].abs() < 0.05 && m[5].abs() < 0.05;
    match (maps, l.effects.get_mut(i)) {
        (Some((a, b)), Some(e)) if !(still(&a) && still(&b)) => {
            if let Some(slot) = e.values.get_mut(mb::FROM..mb::FROM + 12) {
                slot[..6].copy_from_slice(&a);
                slot[6..].copy_from_slice(&b);
            }
        }
        _ => {
            l.effects.remove(i);
        }
    }
}

/// A layer's Hold frame: between Hold from and Hold until (seconds into the
/// clip; until −1: to its end) the picture is the frame at Hold from, while
/// its transform and effects carry on. The effect itself then goes.
fn hold_frame(project: &Project, seq: usize, l: &mut Layer, t: Time, fps: f64) {
    use reelforge_core::effects::hold;
    let Some(i) = l.effects.iter().position(|v| v.kind == reelforge_core::EffectKind::HoldFrame) else { return };
    let v = l.effects.remove(i).values;
    let (from, until) = (v[hold::FROM].max(0.0) as f64, v[hold::UNTIL] as f64);
    let c = l.clip;
    let into = (t - c.timeline_start).as_secs_f64();
    if !from.is_finite() || into < from || (until >= 0.0 && into >= until) {
        return;
    }
    let frame = project.sequences.get(seq).map_or(Time::from_secs_f64(1.0 / fps.max(1.0)), |s| s.frame_rate.frame_duration());
    let at = c.timeline_start + Time::from_secs_f64(from);
    if at < c.timeline_end() {
        l.source_time = project.clip_layer(seq, l.track, c, at, frame).source_time;
    }
}

/// Time displacement bands' clip ids (with the band's number above bit 48).
const SLIT_KEY: u64 = 1 << 60;
/// Echo copies' clip ids (with the copy's number above bit 48).
const ECHO_KEY: u64 = 1 << 61;

impl Scene {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        // (The audio visualiser hears sound through the media reader.)
        reelforge_core::audiovis::set_audio_source(reelforge_media::audio_window);
        Self { device: device.clone(), queue: queue.clone(), nested: HashMap::new(), masks: HashMap::new(), tick: 0, generation: 0, ofx: Default::default(), flow: None, flow_frames: HashMap::new(), par: 1.0, zoom: 1.0, out_fps: None, between: None, max_catch_up: None, idle: false, refining: None, sim_rev: 0, redraw: false }
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
        // (Stepping simulations through the frames in between draws them: the
        // picture must then be drawn again even if nothing else changed.)
        let caught_up = self.catch_up_simulations(project, seq, t, compositor, provider)?;
        // (the signature handed back carries the simulations filled in so far)
        let unchanged_from = unchanged_from.map(|u| u ^ self.sim_rev).filter(|_| !caught_up);
        let mut sig = self.render_at(project, seq, None, t, compositor, provider, unchanged_from, 0)?;
        if unchanged_from != Some(sig) {
            // Drawn afresh (something changed): a copy being filled in may no
            // longer fit; it's started again when next idle, if still needed.
            if let Some(r) = self.refining.take() {
                compositor.simulation_discard(r.kind, r.shadow);
            }
        } else if self.idle {
            self.refine_simulations(project, seq, t, compositor, provider)?;
            if std::mem::take(&mut self.redraw) {
                // Filled in: drawn again now, from the exact simulation.
                sig = self.render_at(project, seq, None, t, compositor, provider, None, 0)?;
            }
        }
        let sig = sig ^ self.sim_rev;
        let tick = self.tick;
        // Compositions off screen for a few seconds give their GPU memory back.
        self.nested.retain(|_, n| tick - n.last_used < 150);
        self.masks.retain(|_, m| tick - m.2 < 150);
        self.ofx.trim(600);
        Ok(sig)
    }

    /// Water or smoke that a layer's own picture pushes about (a keyed boat, say) reads
    /// it at every step. Played straight through, each frame's steps see that
    /// frame. After a jump (a scrub, a seek, starting an export part way in),
    /// the steps since where the simulation can start from would all see the
    /// frame jumped to, as if the object had been there all along: so first the
    /// layer is drawn at the frames in between (each stepping its simulation on
    /// with its own picture), waiting for each frame to decode. Returns whether
    /// anything was drawn. (Layers of the sequence shown, not ones inside
    /// nested sequences.)
    fn catch_up_simulations(&mut self, project: &Project, seq: usize, t: Time, compositor: &mut Compositor, provider: &mut dyn FrameProvider) -> Result<bool> {
        let Some(s) = project.sequences.get(seq) else { return Ok(false) };
        let frame = s.frame_rate.frame_duration();
        let fps = self.out_fps.unwrap_or(s.frame_rate.fps());
        if fps.is_nan() || fps <= 0.0 {
            return Ok(false);
        }
        let frame_size = [s.width.max(2), s.height.max(2)];
        let mut todo = Vec::new();
        for item in reelforge_core::multicam::layers_at(project, seq, t, None) {
            for l in item.layers() {
                for v in &l.effects {
                    if let Some(job) = v.fluid.as_ref().filter(|j| j.obstacles) {
                        if let Some(from) = compositor.simulation_resume(job) {
                            todo.push((l.track, l.clip, job.clone(), from));
                        }
                    }
                }
            }
        }
        let mut drew = false;
        for (track, clip, job, from) in todo {
            let rate = reelforge_core::water::rate(job.effect.kind);
            if job.step.saturating_sub(from) as f64 <= rate / fps * 1.5 {
                continue; // a frame or so on: this frame's picture will do
            }
            // Clip (keyframe) times from where it starts to where it's going.
            let (a, b) = (from as f64 / rate, job.step as f64 / rate);
            let needed = (((b - a) * fps).floor() as usize).saturating_sub(1);
            let n = self.max_catch_up.map_or(needed, |m| needed.min(m));
            if n == 0 {
                continue;
            }
            let scene3d = project.scene3d_at(seq, t);
            let was = provider.wait_for_frames(true);
            for k in 1..=n {
                let ct = Time::from_secs_f64(a + (b - a) * k as f64 / (n + 1) as f64);
                let at = clip.timeline_start + (ct - clip.source_in);
                let l = project.clip_layer(seq, track, clip, at, frame);
                if let Some(mut g) = self.gather(project, &l, at, frame_size, fps, &scene3d, provider, 0)? {
                    let other = self.gather_obstacle(project, seq, track, job.obstacle_from, at, frame_size, fps, &scene3d, provider)?;
                    link_obstacle(&mut g, other.as_ref(), [frame_size[0] as f32, frame_size[1] as f32]);
                    let mut draw: Vec<DrawItem> = other.iter().map(|o| DrawItem::Layer(input(o))).collect();
                    draw.push(DrawItem::Layer(input(&g)));
                    compositor.render_items(frame_size, &draw);
                    drew = true;
                }
            }
            provider.wait_for_frames(was);
            // (after: on a first jump the simulation is only made by the frames drawn)
            compositor.simulation_caught_up(&job, from, n >= needed);
        }
        Ok(drew)
    }

    /// Fill in a simulation a long jump left approximate, a few frames a
    /// tick (see [`Self::idle`]): its layer stepped through each frame from
    /// its last exact checkpoint, in order, as playing would, drawing nothing;
    /// then the frame is drawn again from it.
    fn refine_simulations(&mut self, project: &Project, seq: usize, t: Time, compositor: &mut Compositor, provider: &mut dyn FrameProvider) -> Result<()> {
        let Some(s) = project.sequences.get(seq) else { return Ok(()) };
        let frame = s.frame_rate.frame_duration();
        let fps = self.out_fps.unwrap_or(s.frame_rate.fps());
        if fps.is_nan() || fps <= 0.0 {
            return Ok(());
        }
        if let Some(r) = self.refining.take_if(|r| r.at != t) {
            compositor.simulation_discard(r.kind, r.shadow);
        }
        if self.refining.is_none() {
            'find: for item in reelforge_core::multicam::layers_at(project, seq, t, None) {
                for l in item.layers() {
                    for v in &l.effects {
                        let Some(job) = v.fluid.as_ref().filter(|j| j.obstacles) else { continue };
                        let shadow = job.key ^ SHADOW;
                        let Some(c) = compositor.simulation_fork(job, shadow) else { continue };
                        // The timeline frames between the checkpoint and now, as
                        // played (clip time: c / rate on).
                        let rate = reelforge_core::water::rate(job.effect.kind);
                        let start = l.clip.timeline_start + (Time::from_secs_f64(c as f64 / rate) - l.clip.source_in);
                        let first = start.to_frame(s.frame_rate) + 1;
                        let last = t.to_frame(s.frame_rate);
                        let times: Vec<Time> = (first.max(0)..last).map(|k| Time::from_frame(k, s.frame_rate)).collect();
                        self.refining = Some(Refine { at: t, key: job.key, kind: job.effect.kind, shadow, track: l.track, clip: l.clip.clone(), times, next: 0, obstacle_from: job.obstacle_from });
                        break 'find;
                    }
                }
            }
        }
        let Some(mut r) = self.refining.take() else { return Ok(()) };
        let frame_size = [s.width.max(2), s.height.max(2)];
        let scene3d = project.scene3d_at(seq, t);
        let was = provider.wait_for_frames(true);
        for _ in 0..REFINE_FRAMES {
            let Some(&at) = r.times.get(r.next) else { break };
            r.next += 1;
            let l = project.clip_layer(seq, r.track, &r.clip, at, frame);
            if let Some(mut g) = self.gather(project, &l, at, frame_size, fps, &scene3d, provider, 0)? {
                let other = self.gather_obstacle(project, seq, r.track, r.obstacle_from, at, frame_size, fps, &scene3d, provider)?;
                link_obstacle(&mut g, other.as_ref(), [frame_size[0] as f32, frame_size[1] as f32]);
                compositor.step_simulations(frame_size, &input(&g), other.as_ref().map(input).as_ref(), r.key, r.shadow);
            }
        }
        provider.wait_for_frames(was);
        if r.next >= r.times.len() {
            // Filled in: the copy takes the simulation's place and the frame
            // is drawn again (stepping it the last bit, from its own picture).
            compositor.simulation_adopt(r.kind, r.key, r.shadow);
            self.sim_rev = self.sim_rev.wrapping_add(1);
            self.redraw = true;
        } else {
            self.refining = Some(r);
        }
        Ok(())
    }

    /// For a fluid on `track` flowing round another layer's shape (`from`:
    /// 1 the layer above, 2 the one below; 0 none): that layer at `at`,
    /// gathered (see [`neighbour`]).
    #[allow(clippy::too_many_arguments)]
    fn gather_obstacle(&mut self, project: &Project, seq: usize, track: usize, from: u8, at: Time, frame_size: [u32; 2], fps: f64, scene3d: &reelforge_core::space3d::Scene3d, provider: &mut dyn FrameProvider) -> Result<Option<Gathered>> {
        if from == 0 {
            return Ok(None);
        }
        let singles: Vec<Layer> = reelforge_core::multicam::layers_at(project, seq, at, None)
            .into_iter()
            .filter_map(|i| match i {
                VideoItem::Single(l) if !l.clip.adjustment => Some(l),
                _ => None,
            })
            .collect();
        let tracks: Vec<usize> = singles.iter().map(|l| l.track).collect();
        let Some(l) = neighbour(&tracks, track, from).and_then(|j| singles.get(j)) else { return Ok(None) };
        Ok(self.gather(project, l, at, frame_size, fps, scene3d, provider, 0)?.filter(|g| !g.adjustment))
    }

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
        let fps = self.out_fps.unwrap_or(s.frame_rate.fps());
        self.par = s.par();
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
                VideoItem::Single(mut l) => {
                    motion_blur(project, seq, &mut l, t, frame_size, fps);
                    hold_frame(project, seq, &mut l, t, fps);
                    let echo = l.effects.iter().find(|v| v.kind == reelforge_core::EffectKind::Echo).cloned();
                    let slit = l.effects.iter().find(|v| v.kind == reelforge_core::EffectKind::TimeDisplacement).cloned();
                    match (slit, echo) {
                        (Some(slit), _) => self.gather_slit_scan(project, seq, &l, &slit, t, frame_size, fps, &scene3d, provider, depth, &mut items)?,
                        (None, Some(echo)) => self.gather_echoes(project, seq, &l, &echo, t, frame_size, fps, &scene3d, provider, depth, &mut items)?,
                        (None, None) => items.extend(self.gather_retimed(project, &l, t, frame_size, fps, &scene3d, provider, depth)?),
                    }
                }
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
        link_obstacles(&mut items, &tags, [frame_size[0] as f32, frame_size[1] as f32]);
        let items = track_mattes(items, &tags, &s.tracks);
        let mut items = items;

        sort_3d(&mut items);
        // (Drawn finer is another picture.)
        let signature = signature(frame_size, &items) ^ (self.zoom.to_bits() as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
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

    /// A layer with Time displacement (slit-scan): the picture cut into bands
    /// (rows or columns), each from a moment further back (or on) — band 0 now,
    /// the last `delay` seconds away — by copies of the layer, each cropped to
    /// its band and read through a decoder of its own. Moments outside the
    /// clip hold its first or last frame.
    #[allow(clippy::too_many_arguments)]
    fn gather_slit_scan(&mut self, project: &Project, seq: usize, l: &Layer, slit: &EffectValues, t: Time, frame_size: [u32; 2], fps: f64, scene3d: &reelforge_core::space3d::Scene3d, provider: &mut dyn FrameProvider, depth: usize, items: &mut Vec<Item>) -> Result<()> {
        use reelforge_core::effects::time_displace as td;
        let v = |i: usize| slit.values.get(i).copied().filter(|x| x.is_finite()).unwrap_or(0.0);
        let delay = v(td::DELAY).clamp(-10.0, 10.0) as f64;
        let bands = (v(td::BANDS).round().max(2.0) as usize).min(td::MAX);
        let dir = v(td::DIRECTION).round() as i32;
        let frame = project.sequences.get(seq).map_or(Time::from_secs_f64(1.0 / fps.max(1.0)), |s| s.frame_rate.frame_duration());
        let c = l.clip;
        let not_slit = |x: &EffectValues| x.kind != reelforge_core::EffectKind::TimeDisplacement;
        let clips: Vec<reelforge_core::Clip> = (0..bands).map(|b| reelforge_core::Clip { id: if b == 0 { c.id } else { ClipId(c.id.0 ^ SLIT_KEY ^ ((b as u64) << 48)) }, ..c.clone() }).collect();
        let last = c.timeline_end() - frame;
        for (b, cb) in clips.iter().enumerate() {
            let k = b as f64 / (bands - 1) as f64;
            let mut at = Time::from_secs_f64((t.as_secs_f64() - delay * k).max(0.0));
            at = at.max(c.timeline_start).min(last.max(c.timeline_start));
            let then = project.clip_layer(seq, l.track, cb, at, frame);
            let mut bl = l.clone();
            bl.clip = cb;
            bl.source_time = then.source_time;
            bl.effects.retain(not_slit);
            // This band only (with whatever the layer's own crop cuts too).
            let (lo, hi) = (b as f32 / bands as f32, (b + 1) as f32 / bands as f32);
            let cr = &mut bl.transform.crop;
            match dir {
                0 => { cr[1] = cr[1].max(lo); cr[3] = cr[3].max(1.0 - hi); }
                1 => { cr[1] = cr[1].max(1.0 - hi); cr[3] = cr[3].max(lo); }
                2 => { cr[0] = cr[0].max(lo); cr[2] = cr[2].max(1.0 - hi); }
                _ => { cr[0] = cr[0].max(1.0 - hi); cr[2] = cr[2].max(lo); }
            }
            if cr[0] + cr[2] >= 1.0 || cr[1] + cr[3] >= 1.0 {
                continue;
            }
            items.extend(self.gather_retimed(project, &bl, t, frame_size, fps, scene3d, provider, depth)?);
        }
        Ok(())
    }

    /// A layer with an Echo effect: copies of it from moments before (or
    /// after) `t`, each fainter than the last, behind it, in front of it or
    /// blended onto it, and the layer itself. Each copy is the layer as it was
    /// then (its picture, its transform and its other effects), read through a
    /// decoder of its own; there are no copies from outside the clip.
    #[allow(clippy::too_many_arguments)]
    fn gather_echoes(&mut self, project: &Project, seq: usize, l: &Layer, echo: &EffectValues, t: Time, frame_size: [u32; 2], fps: f64, scene3d: &reelforge_core::space3d::Scene3d, provider: &mut dyn FrameProvider, depth: usize, items: &mut Vec<Item>) -> Result<()> {
        use reelforge_core::effects::echo as e;
        let v = |i: usize| echo.values.get(i).copied().filter(|x| x.is_finite()).unwrap_or(0.0);
        let delay = v(e::DELAY) as f64;
        let count = (v(e::COUNT).round().max(0.0) as usize).min(e::MAX);
        let first = (v(e::FIRST) / 100.0).clamp(0.0, 1.0);
        let keep = 1.0 - (v(e::DECAY) / 100.0).clamp(0.0, 1.0);
        let mode = v(e::DRAW).round() as i32;
        let original = (v(e::ORIGINAL) / 100.0).clamp(0.0, 1.0);
        let blend = match mode {
            2 => BlendMode::Add,
            3 => BlendMode::Screen,
            4 => BlendMode::Lighten,
            _ => l.blend,
        };
        let not_echo = |x: &EffectValues| x.kind != reelforge_core::EffectKind::Echo;
        let frame = project.sequences.get(seq).map_or(Time::from_secs_f64(1.0 / fps), |s| s.frame_rate.frame_duration());
        let c = l.clip;
        // Each copy its own clip id, so its frames come from a decoder of its own.
        let clips: Vec<reelforge_core::Clip> = (1..=count).map(|k| reelforge_core::Clip { id: ClipId(c.id.0 ^ ECHO_KEY ^ ((k as u64) << 48)), ..c.clone() }).collect();
        let mut copies = Vec::new();
        // The farthest first, so nearer copies draw over it.
        for (k, ck) in clips.iter().enumerate().rev() {
            let opacity = first * keep.powi(k as i32);
            let at = t.as_secs_f64() - delay * (k + 1) as f64;
            if opacity < 0.002 || !at.is_finite() {
                continue;
            }
            let at = Time::from_secs_f64(at);
            if at < c.timeline_start || at >= c.timeline_end() {
                continue;
            }
            let mut el = project.clip_layer(seq, l.track, ck, at, frame);
            el.effects.retain(not_echo);
            motion_blur(project, seq, &mut el, at, frame_size, fps);
            el.transform.opacity *= opacity;
            el.blend = blend;
            copies.extend(self.gather_retimed(project, &el, at, frame_size, fps, scene3d, provider, depth)?);
        }
        let mut me = l.clone();
        me.effects.retain(not_echo);
        me.transform.opacity *= original;
        let me = if original > 0.0 { self.gather_retimed(project, &me, t, frame_size, fps, scene3d, provider, depth)? } else { None };
        if mode == 0 {
            items.extend(copies);
            items.extend(me);
        } else {
            items.extend(me);
            items.extend(copies);
        }
        Ok(())
    }

    /// A layer, and for retimed footage between two of its frames, those two
    /// frames cross-faded (frame blending), each first pushed along the
    /// motion between them (optical flow) if the clip asks for that.
    #[allow(clippy::too_many_arguments)]
    fn gather_retimed(&mut self, project: &Project, l: &Layer, t: Time, frame_size: [u32; 2], fps: f64, scene3d: &reelforge_core::space3d::Scene3d, provider: &mut dyn FrameProvider, depth: usize) -> Result<Option<Item>> {
        use reelforge_core::FrameBlend;
        let c = l.clip;
        // (An export can blend or flow all footage, over each clip's own choice.)
        let blend = self.between.filter(|b| *b != FrameBlend::Nearest).unwrap_or(c.frame_blend);
        // Retimed footage, or footage with fewer frames a second than the
        // sequence (24p in a 60p sequence): blended or flowed in between.
        let rate = (blend != FrameBlend::Nearest && c.nested.is_none() && c.title.is_none() && c.plane.is_none() && !c.adjustment)
            .then(|| project.media(c.media))
            .flatten()
            .filter(|m| !project.is_still(m.id))
            .and_then(|m| m.info.video.as_ref())
            .map(|v| v.frame_rate.fps())
            .filter(|r| r.is_finite() && *r > 0.0 && (c.is_retimed() || *r < fps * 0.999));
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
        if blend == FrameBlend::Flow && a.mesh.is_none() && a.space.is_none() && !l.transform.three_d {
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
            (Some(inner), _) => self.nested_picture(project, l.clip.id, inner, l.clip.angle, l.source_time, provider, depth, l.transform.scale)?,
            // A title is laid out on the frame as it's shown (as wide as its
            // pixels make it), then squeezed back into the frame's pixels;
            // drawn as finely as it's scaled up, so it stays crisp.
            (None, _) if l.clip.title.is_some() => {
                let canvas = if self.par != 1.0 { [((frame_size[0] as f32 * self.par).round() as u32).max(2), frame_size[1]] } else { frame_size };
                let tr = &l.transform;
                // Masks, effects (OpenFX too), corner pins, warps, crops and
                // 3D need the picture to cover the whole canvas.
                let whole = mask::any_active(&l.clip.masks) || l.effects.iter().any(|e| !e.is_identity()) || !reelforge_core::ofx::steps(&l.clip.effects, l.clip.source_time(t)).is_empty() || tr.pin.is_some() || tr.three_d || tr.crop.iter().any(|&c| c != 0.0) || l.clip.mesh_at(t).is_some();
                provider.title(project, l, canvas, (title_sharpness(tr.scale) * self.zoom).min(8.0), whole)?
            }
            (None, _) => provider.picture(project, l, frame_size)?,
        };
        let Some(mut picture) = picture else { return Ok(None) };
        // Pixel shapes: a picture (footage, a title, a composition) whose
        // pixels aren't this sequence's shape takes as many of this
        // sequence's pixels across as it shows wide.
        if l.clip.plane.is_none() && !l.clip.adjustment {
            let k = project.own_par(l.clip) / self.par;
            if k != 1.0 {
                picture.size[0] *= k;
            }
        }
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
            return Ok(Some(Gathered { key: l.clip.id.0, picture, transform, blend: l.blend, effects, mask, refine: mask::refine_of(&l.clip.masks).map(|r| [r.radius, r.smooth, r.shift, r.decontaminate]), wipe: None, mesh: None, space: None, adjustment: true, solid: None, sort_depth: None }));
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
                effects.push(EffectValues { kind: reelforge_core::EffectKind::Blur, values, table: None, plugin: None, time: 0.0, prev: None, particles: None, pieces: None, fluid: None, gpu: [0.0; 4], table2: None });
            }
        }
        Ok(Some(Gathered { key: l.clip.id.0, picture, transform: l.transform, blend: l.blend, effects, mask, refine: mask::refine_of(&l.clip.masks).map(|r| [r.radius, r.smooth, r.shift, r.decontaminate]), wipe: None, mesh, space, adjustment: false, solid: None, sort_depth: None }))
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
    #[allow(clippy::too_many_arguments)]
    fn nested_picture(&mut self, project: &Project, clip: ClipId, inner: usize, angle: Option<reelforge_core::multicam::Angle>, t: Time, provider: &mut dyn FrameProvider, depth: usize, scale: [f32; 2]) -> Result<Option<Picture>> {
        let Some(s) = project.sequences.get(inner) else { return Ok(None) };
        if depth >= MAX_NESTING {
            return Ok(None);
        }
        let size = [s.width.max(2), s.height.max(2)];
        let zoom = nested_zoom(scale, self.zoom, size);
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
        let (outer_par, outer_zoom) = (self.par, self.zoom);
        self.zoom = zoom;
        n.compositor.output_scale = zoom;
        let result = self.render_at(project, inner, angle, t, &mut n.compositor, provider, n.signature.filter(|_| n.frame.is_some()), depth + 1);
        self.par = outer_par;
        self.zoom = outer_zoom;
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
        effects.push(EffectValues { kind: reelforge_core::EffectKind::Blur, values, table: None, plugin: None, time: 0.0, prev: None, particles: None, pieces: None, fluid: None, gpu: [0.0; 4], table2: None });
    }
    let picture = Picture { frame: VideoFrame::from_rgba(Time::ZERO, 1, 1, Arc::from(vec![0u8; 4]), 0), size: full, media: MediaId::GENERATED };
    Gathered {
        key: l.clip.id.0,
        picture,
        transform: Transform { opacity: l.transform.opacity, ..Default::default() },
        blend: l.blend,
        effects,
        mask: None,
        refine: None,
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
    // Rendered at another colour bit depth, it's another picture (the standard one
    // leaves recipes as they always were, so rendered previews kept on disk still match).
    let depth = reelforge_render::default_depth();
    if depth != reelforge_render::ColorDepth::HalfLinear {
        std::hash::Hash::hash(&depth, &mut h);
    }
    h.finish()
}

fn recipe_into(project: &Project, seq: usize, angle: Option<reelforge_core::multicam::Angle>, t: Time, sources: &HashMap<MediaId, std::path::PathBuf>, depth: usize, h: &mut DefaultHasher) {
    let Some(s) = project.sequences.get(seq) else { return };
    let frame_size = [s.width.max(2), s.height.max(2)];
    frame_size.hash(h);
    s.pixel_aspect.hash(h);
    let par = s.par();
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
            (None, Some(_)) => {
                2u8.hash(h);
                let at = crate::title_clock(l);
                if let Some(spec) = clip.title_at(l.source_time) {
                    spec.content_hash_at([((frame_size[0] as f32 * par).round() as u32).max(2), frame_size[1]], at.0, at.1).hash(h);
                }
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
                        // Missing media shows a stand-in picture; a file written
                        // over shows its new pictures.
                        reelforge_core::imageseq::media_version(path).hash(h);
                        m.layer_size().map_or([0.0; 2], |s| [s[0] as f32, s[1] as f32])
                    }
                    None => [0.0; 2],
                }
            }
        };
        let layer_size = if clip.plane.is_none() && !clip.adjustment && clip.title.is_none() {
            [layer_size[0] * project.own_par(clip) / par, layer_size[1]]
        } else {
            layer_size
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
        mask: g.mask.as_ref().map(|(hash, img)| MaskInput { content_id: *hash, pixels: &img.data, size: [img.width, img.height], keep: &img.keep, refine: g.refine }),
        wipe: g.wipe,
        mesh: g.mesh.clone(),
        space: g.space.as_ref(),
    }
}

/// Of layers on `tracks`, the one on the nearest track above `track`
/// (`from` 1) or below it (`from` 2), the first of any on that track.
fn neighbour(tracks: &[usize], track: usize, from: u8) -> Option<usize> {
    let pick = match from {
        1 => tracks.iter().copied().filter(|&t| t > track).min(),
        2 => tracks.iter().copied().filter(|&t| t < track).max(),
        _ => None,
    }?;
    tracks.iter().position(|&t| t == pick)
}

/// Point each fluid in the frame that flows round another layer's shape at
/// that layer (none there: nothing to flow round).
fn link_obstacles(items: &mut [Item], tags: &[(usize, Option<reelforge_core::TrackMatte>)], frame: [f32; 2]) {
    let wants = |g: &Gathered| g.effects.iter().filter_map(|e| e.fluid.as_ref()).find(|j| j.obstacles && j.obstacle_from != 0).map(|j| j.obstacle_from);
    // Only whole layers can be shapes (not adjustments, or dissolves part way).
    let shape = |i: &Item| matches!(i, Item::Single(g) if !g.adjustment);
    let tracks: Vec<(usize, usize)> = items.iter().enumerate().filter(|(_, i)| shape(i)).filter_map(|(k, _)| tags.get(k).map(|t| (k, t.0))).collect();
    let only: Vec<usize> = tracks.iter().map(|t| t.1).collect();
    for k in 0..items.len() {
        let (Some(Item::Single(g)), Some(tag)) = (items.get(k), tags.get(k)) else { continue };
        let Some(from) = wants(g) else { continue };
        let source = neighbour(&only, tag.0, from)
            .and_then(|j| tracks.get(j))
            .and_then(|&(j, _)| match items.get(j) {
                Some(Item::Single(o)) => obstacle_source(g, o, frame),
                _ => None,
            });
        if let Some(Item::Single(g)) = items.get_mut(k) {
            set_obstacle(g, source);
        }
    }
}

/// [`link_obstacles`] for one layer, given the other one.
fn link_obstacle(g: &mut Gathered, other: Option<&Gathered>, frame: [f32; 2]) {
    let source = other.and_then(|o| obstacle_source(g, o, frame));
    set_obstacle(g, source);
}

fn set_obstacle(g: &mut Gathered, source: Option<reelforge_core::fluid::ObstacleSource>) {
    for e in &mut g.effects {
        if let Some(job) = e.fluid.as_ref().filter(|j| j.obstacle_from != 0 && j.source != source) {
            let mut job = reelforge_core::fluid::FluidJob::clone(job);
            job.source = source;
            e.fluid = Some(Arc::new(job));
        }
    }
}

/// How `other`'s picture lies under `fluid`'s: the homography from the
/// fluid layer's 0–1 picture coordinates to `other`'s (both through the
/// frame, corner pins included). 3D layers aren't linked.
fn obstacle_source(fluid: &Gathered, other: &Gathered, frame: [f32; 2]) -> Option<reelforge_core::fluid::ObstacleSource> {
    use reelforge_core::transform::Homography;
    if fluid.space.is_some() || other.space.is_some() || fluid.solid.is_some() || other.solid.is_some() {
        return None;
    }
    let (fs, os) = (fluid.picture.size, other.picture.size);
    if !(fs[0] >= 1.0 && fs[1] >= 1.0 && os[0] >= 1.0 && os[1] >= 1.0) {
        return None;
    }
    let to_frame = fluid.transform.layer_map(fs, frame);
    let from_frame = other.transform.layer_map(os, frame).inverse()?;
    let unit = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]];
    let to = unit.map(|[u, v]| {
        let [x, y] = from_frame.apply(to_frame.apply([(u - 0.5) * fs[0], (v - 0.5) * fs[1]]));
        [x / os[0] + 0.5, y / os[1] + 0.5]
    });
    let h = Homography::from_quads(&unit, &to)?;
    let map = h.0.map(|x| x as f32);
    map.iter().all(|x| x.is_finite()).then_some(reelforge_core::fluid::ObstacleSource { key: other.key, map })
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
                match (slots[i].take(), slots[i + 1].take()) {
                    (Some(Item::Single(layer)), Some(Item::Single(m))) => {
                        out.push(Item::Matted { matte: m, layer, kind });
                        i += 2;
                        continue;
                    }
                    // (checked just above: never so, but put back as they were)
                    (a, b) => {
                        slots[i] = a;
                        slots[i + 1] = b;
                    }
                }
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
                video: Some(VideoStreamInfo { width: 640, height: 360, frame_rate: FrameRate::new(25, 1), codec: "h264".into(), time_base: reelforge_core::Rational::new(1, 25), hdr: None, pixel_aspect: (1, 1) }),
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
    fn a_file_written_over_gets_new_recipes() {
        // (so finished frames kept of the old file, in memory or on disk, aren't shown)
        let dir = std::env::temp_dir().join(format!("reelforge-recipe-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("a.mp4");
        std::fs::write(&file, b"one").unwrap();
        let mut p = project();
        p.media[0].path = file.clone();
        let none = HashMap::new();
        let before = frame_recipe(&p, 0, Time::from_secs_f64(1.0), &none);
        assert_eq!(frame_recipe(&p, 0, Time::from_secs_f64(1.0), &none), before, "nothing changed");
        std::fs::write(&file, b"two, longer").unwrap();
        let after = frame_recipe(&p, 0, Time::from_secs_f64(1.0), &none);
        let _ = std::fs::remove_dir_all(&dir);
        assert_ne!(after, before);
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
    fn titles_are_drawn_finer_in_steps_as_they_grow() {
        assert_eq!(title_sharpness([1.0, 1.0]), 1.0);
        assert_eq!(title_sharpness([0.3, -0.5]), 1.0);
        assert_eq!(title_sharpness([2.0, 1.0]), 2.0);
        assert_eq!(title_sharpness([-4.0, 1.0]), 4.0);
        let k = title_sharpness([1.5, 1.5]);
        assert!((1.5..1.7).contains(&k), "{k}");
        // Nearby sizes share a step (no redraw every frame of a slow zoom).
        assert_eq!(title_sharpness([1.42, 1.0]), title_sharpness([1.6, 1.0]));
        assert_eq!(title_sharpness([50.0, 1.0]), 8.0);
        assert_eq!(title_sharpness([f32::NAN, 1.0]), 1.0);
    }

    #[test]
    fn scaled_up_compositions_are_drawn_finer() {
        assert_eq!(nested_zoom([1.0, 1.0], 1.0, [1920, 1080]), 1.0);
        assert_eq!(nested_zoom([0.5, 0.5], 1.0, [1920, 1080]), 1.0);
        assert_eq!(nested_zoom([2.0, 2.0], 1.0, [1920, 1080]), 2.0);
        assert_eq!(nested_zoom([2.0, 2.0], 2.0, [640, 360]), 4.0, "inside one drawn finer, finer still");
        assert_eq!(nested_zoom([20.0, 1.0], 1.0, [640, 360]), MAX_ZOOM);
        assert_eq!(nested_zoom([4.0, 4.0], 1.0, [3840, 2160]), MAX_ZOOM_PX / 3840.0, "kept to a size the GPU takes");
        assert_eq!(nested_zoom([f32::NAN, 1.0], f32::INFINITY, [0, 0]), 1.0);

        let Some((device, queue)) = reelforge_render::headless_device() else {
            eprintln!("no GPU adapter; skipping");
            return;
        };
        let secs = Time::from_secs_f64;
        // A 32×16 composition with a blue square, its edges on pixels.
        let mut inner = Sequence::new("Inner", 32, 16, FrameRate::new(25, 1));
        let mut sq = Clip::new_plane(ClipId(1), Time::ZERO, secs(5.0), [0, 0, 255, 255]);
        sq.transform.scale = [0.25, 0.25];
        inner.tracks[0].clips = vec![sq];
        // Scaled up 4× in a 128×64 one: its edges stay one pixel sharp.
        let mut main = Sequence::new("Main", 128, 64, FrameRate::new(25, 1));
        let mut c = Clip::new_nested(ClipId(2), 1, Time::ZERO, secs(5.0), None);
        c.transform.scale = [4.0, 4.0];
        main.tracks[0].clips = vec![c];
        let mut p = Project::new();
        p.sequences = vec![main, inner];
        let mut scene = Scene::new(&device, &queue);
        let mut comp = Compositor::new(&device, &queue);
        scene.render(&p, 0, secs(1.0), &mut comp, &mut Nothing, None).unwrap();
        let img = comp.read_output().unwrap();
        let row: Vec<u8> = (0..128).map(|x| img[(32 * 128 + x) * 4 + 2]).collect();
        let soft = row.iter().filter(|&&b| b > 30 && b < 225).count();
        assert!(row[64] > 240 && row[10] < 15, "{row:?}");
        assert!(soft <= 2, "the square's edges are crisp, not 4 px ramps: {soft} soft pixels in {row:?}");
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
