//! GPU compositor: blends transformed layers into a frame with wgpu.
//!
//! Independent of any interface: the editor gives it a device and queue and
//! reads the output back for the viewer; export and the tests drive it
//! headless too.
//!
//! Pipeline per frame:
//! 1. upload each layer's RGBA8 frame to an `Rgba8UnormSrgb` texture (cached;
//!    only re-uploaded when its content changes),
//! 2. draw each layer as a transformed quad into an `Rgba16Float` target in
//!    linear light, bottom to top, using a blend state per [`BlendMode`],
//! 3. encode to sRGB into an `Rgba8Unorm` output texture.
//!
//! A cross-dissolve between two layers is drawn as a *group*: both layers are
//! summed into a separate transparent float target, weighted `1 − mix` and
//! `mix`, and that target is then composited over the frame like one layer.
//! This gives `(1 − u)·over(A, bg) + u·over(B, bg)` exactly, so there's no dip
//! in brightness halfway through, even for clips that don't fill the frame.
//!
//! A layer with effects is processed first, in its own pixel space: it's
//! converted to premultiplied linear float, run through one full-screen pass
//! per effect (two for a separable blur) ping-ponging between two scratch
//! targets kept per layer, and the result is drawn like a group. Because the
//! effects run before the transform, they move, scale and rotate with the layer.

// Never crash: no unwrap, expect, panic, unreachable, todo or unimplemented
// outside tests (a damaged file or an odd setting is an error, not a crash).
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

// (as reelforge-core; GPU dispatch helpers take their resources plainly)
#![allow(clippy::needless_range_loop, clippy::too_many_arguments, clippy::type_complexity)]

use reelforge_core::{BlendMode, EffectKind, EffectValues, Transform};
use std::collections::HashMap;

pub use wgpu;

mod yuv;
#[cfg(windows)]
mod interop;
pub use reelforge_core::frame::{GpuFrame, YuvPlanes};

/// A layer's pixels as the compositor receives them.
#[derive(Clone, Copy)]
pub enum PixelData<'a> {
    /// Tightly packed RGBA8, sRGB-encoded, straight alpha.
    Rgba(&'a [u8]),
    /// Decoded 4:2:0 YUV, converted to RGB on the GPU.
    Yuv(&'a YuvPlanes),
    /// A frame the hardware decoder left on the graphics card (zero-copy).
    Gpu(&'a GpuFrame),
    /// A solid object (3D text), drawn on the GPU into a frame-sized picture.
    Solid(&'a solid::SolidInput),
    /// Half-float RGBA, scene-linear and premultiplied (OpenEXR): kept as
    /// floats, so values above 1 and light without alpha survive.
    Float(&'a reelforge_core::frame::FloatPixels),
    /// An adjustment layer: the frame composited so far (everything below),
    /// put through the layer's effects and drawn back over it. `pixel_size`
    /// should be the frame size.
    Below,
}

impl PixelData<'_> {
    /// Drawn from a half-float layer texture: floating-point pictures, and
    /// video of more than 8 bits (kept at its precision).
    pub fn is_deep(&self) -> bool {
        match self {
            PixelData::Float(_) => true,
            PixelData::Yuv(y) => y.layout.deep(),
            PixelData::Gpu(g) => g.layout.deep(),
            _ => false,
        }
    }

    /// The data really is a picture of `size` (and not empty).
    fn matches(&self, size: [u32; 2]) -> bool {
        size[0] > 0
            && match self {
                PixelData::Rgba(px) => px.len() as u64 == size[0] as u64 * size[1] as u64 * 4,
                PixelData::Yuv(y) => [y.width, y.height] == size && y.planes.len() == y.layout.planes(),
                PixelData::Gpu(g) => [g.width, g.height] == size,
                PixelData::Float(f) => [f.width, f.height] == size && f.rgba.len() as u64 == size[0] as u64 * size[1] as u64 * 4,
                PixelData::Solid(_) => true,
                // Nothing to upload: it's read from the frame as it's drawn.
                PixelData::Below => false,
            }
    }
}

impl<'a> From<&'a reelforge_core::frame::Pixels> for PixelData<'a> {
    fn from(p: &'a reelforge_core::frame::Pixels) -> Self {
        match p {
            reelforge_core::frame::Pixels::Rgba(b) => PixelData::Rgba(b),
            reelforge_core::frame::Pixels::Yuv(y) => PixelData::Yuv(y),
            reelforge_core::frame::Pixels::Gpu(g) => PixelData::Gpu(g),
            reelforge_core::frame::Pixels::Float(f) => PixelData::Float(f),
        }
    }
}

mod particles;
mod fluidgpu;
mod liquid3dgpu;
mod watergpu;
pub mod simstore;
pub use simstore::{set_simulation_store, SimulationStore};
pub mod plugin;
pub mod solid;
pub mod text;
pub mod text3d;
pub mod model3d;
pub mod flowgpu;
pub mod bench;
pub use solid::SolidInput;

/// The standard working format (16-bit float); see [`Formats`] for the others.
const ACCUM_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
pub use reelforge_core::depth::ColorDepth;

/// The textures a colour bit depth works in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Formats {
    /// The depth worked at (the one asked for, or the nearest this graphics card can do).
    pub depth: ColorDepth,
    /// The frame being composited (and its backdrop copy).
    pub accum: wgpu::TextureFormat,
    /// Everything in between: effects, dissolves, mattes (always linear light).
    pub inter: wgpu::TextureFormat,
    /// The frame holds gamma-encoded values (layers blend in display values), else
    /// linear light.
    pub gamma: bool,
}

impl Formats {
    /// The formats for `depth` on a device with `features` (32-bit float needs it to
    /// filter and blend 32-bit float textures; without, 16-bit).
    pub fn for_depth(depth: ColorDepth, features: wgpu::Features) -> Formats {
        use wgpu::TextureFormat as F;
        let f32_ok = features.contains(wgpu::Features::FLOAT32_FILTERABLE | wgpu::Features::FLOAT32_BLENDABLE);
        let depth = if depth.bits() == 32 && !f32_ok { depth.at_most_half() } else { depth };
        let (accum, inter) = match depth.bits() {
            // The frame as 8-bit display values; what's in between kept linear in 8-bit
            // sRGB textures (so darks don't band).
            8 => (F::Rgba8Unorm, F::Rgba8UnormSrgb),
            32 => (F::Rgba32Float, F::Rgba32Float),
            _ => (F::Rgba16Float, F::Rgba16Float),
        };
        Formats { depth, accum, inter, gamma: !depth.linear() }
    }
}

static DEFAULT_DEPTH: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(2);

/// The colour bit depth compositors made from now on work at (Settings).
pub fn set_default_depth(d: ColorDepth) {
    let i = ColorDepth::ALL.iter().position(|x| *x == d).unwrap_or(2);
    DEFAULT_DEPTH.store(i as u8, std::sync::atomic::Ordering::Relaxed);
}

pub fn default_depth() -> ColorDepth {
    ColorDepth::ALL.get(DEFAULT_DEPTH.load(std::sync::atomic::Ordering::Relaxed) as usize).copied().unwrap_or_default()
}

/// The device features worth having when the graphics card offers them: 32-bit float
/// textures to filter and blend (for 32-bit float rendering).
fn wanted_features(adapter: &wgpu::Adapter) -> wgpu::Features {
    adapter.features() & (wgpu::Features::FLOAT32_FILTERABLE | wgpu::Features::FLOAT32_BLENDABLE)
}
const LAYER_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
/// For floating-point pictures (EXR).
const FLOAT_LAYER_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
/// Gamma-encoded 8-bit, which is what the viewer shows.
pub const OUTPUT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
/// The 16-bit export output (read back and converted to 16-bit integers).
const DEEP_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;

/// The curve for a 16-bit export frame ([`Compositor::read_output_deep`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeepCurve {
    /// sRGB-encoded BT.709 (SDR), as the 8-bit output but with more precision.
    Srgb,
    /// BT.2020 primaries, PQ (HDR10); SDR white at 203 nits.
    Pq,
    /// BT.2020 primaries, HLG; SDR white at 75% signal.
    Hlg,
}
/// Draw calls per frame (a dissolve uses three).
const MAX_DRAWS: usize = 128;
const UNIFORM_SIZE: u64 = 128 + 16 + 32 + MESH_VEC4S as u64 * 16; // mat4 + vec2 + 2×f32 + 3×vec4, the effect map and margin, then a mesh header and the points
/// vec4s holding a mesh's points, two to each: enough for the biggest grid
/// (33 × 33 points). Must match `pts` in composite.wgsl.
const MESH_VEC4S: usize = 545;
/// Depth for runs of 3D layers.
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
/// A 3D layer's pixels fainter than this don't hide what's behind them.
const DEPTH_CUT: f64 = 0.02;

/// 3D layers and text that can cast shadows in one frame (each gets a tile of the atlas).
pub const MAX_OCCLUDERS: usize = (ATLAS_GRID * ATLAS_GRID) as usize;
/// The shadow atlas: this many tiles across and down.
const ATLAS_GRID: u32 = 5;
/// Shadow quality: each caster's tile in the atlas, in pixels (sharper
/// shadows from big layers the bigger it is). Set for the whole app.
static SHADOW_TILE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(SHADOW_QUALITIES[1]);
/// Low, Standard and High shadow quality (tile sizes).
pub const SHADOW_QUALITIES: [u32; 3] = [256, 512, 1024];

/// Set the shadow quality (0 Low, 1 Standard, 2 High) for every compositor.
pub fn set_shadow_quality(q: usize) {
    SHADOW_TILE.store(SHADOW_QUALITIES[q.min(2)], std::sync::atomic::Ordering::Relaxed);
}

/// 3D text and models: anti-aliasing samples a pixel (1: off; else 4, the
/// most every graphics card offers). Set for the whole app.
static SOLID_SAMPLES: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(4);

/// Anti-aliasing for 3D text and models: `samples` 1 is off, anything more is 4.
pub fn set_solid_samples(samples: u32) {
    SOLID_SAMPLES.store(if samples > 1 { 4 } else { 1 }, std::sync::atomic::Ordering::Relaxed);
}

pub(crate) fn solid_samples() -> u32 {
    SOLID_SAMPLES.load(std::sync::atomic::Ordering::Relaxed)
}

/// Reflections on 3D text and models: the environment light's picture is
/// used at most this many pixels across (0: at its own size). Set for the whole app.
static REFLECTION_MAP: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(2048);

pub fn set_reflection_map_size(px: u32) {
    REFLECTION_MAP.store(px, std::sync::atomic::Ordering::Relaxed);
}

pub(crate) fn reflection_map_size() -> u32 {
    REFLECTION_MAP.load(std::sync::atomic::Ordering::Relaxed)
}

fn atlas_texture(device: &wgpu::Device, tile: u32) -> wgpu::TextureView {
    device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some("shadow atlas"),
            size: wgpu::Extent3d { width: tile * ATLAS_GRID, height: tile * ATLAS_GRID, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::R8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        })
        .create_view(&Default::default())
}
/// Where occluders start in a lit layer's point list (after the plane, the
/// camera and room for every light), in vec4s.
const OCCLUDER_BASE: usize = 4 + 4 * reelforge_core::space3d::MAX_LIGHTS;

/// Little quads per mesh cell, across and down (a cell is bilinear; this many
/// straight-edged pieces draw it smoothly): six for small grids, fewer for
/// fine ones, whose cells are small already.
fn mesh_subdiv(m: &reelforge_core::meshwarp::MeshShape) -> u32 {
    (48 / m.cols.max(m.rows).max(1) as u32).clamp(1, 6)
}
/// Effect passes per frame.
const MAX_FX_PASSES: usize = 256;
/// 10 × vec4: params 0–7, texel/direction, extra (aspect, time, layer size),
/// params 8–15, extra2 (texture size, pass, pass count), params 16–19, the
/// map from the target's uv to the layer's (the target can have a margin),
/// params 20–23 (the particle shader and plugins read those), then params 24–31
/// (plugins only).
const FX_UNIFORM_SIZE: u64 = 192;
/// Where params 20–23 sit.
const FX_P5: usize = 36;
/// Where params 24–31 sit.
const FX_P6: usize = 40;
/// Where `map` sits in an effect pass's uniform.
const FX_MAP: usize = 32;
/// The layer exactly fills its effect targets.
const NO_MARGIN: [f32; 4] = [1.0, 1.0, 0.0, 0.0];
const FX_FLOATS: usize = (FX_UNIFORM_SIZE / 4) as usize;

/// One layer to draw.
#[derive(Clone)]
pub struct LayerInput<'a> {
    /// Stable identity (e.g. the clip id) for texture caching.
    pub key: u64,
    /// Changes whenever `pixels` changes (e.g. a hash of media/pts/generation).
    pub content_id: u64,
    /// The picture: RGBA8 (sRGB-encoded) or YUV planes.
    pub pixels: PixelData<'a>,
    pub pixel_size: [u32; 2],
    /// Size of the layer in frame pixels at 100% scale (the media's native
    /// size, which can differ from `pixel_size` when decoding a proxy).
    pub layer_size: [f32; 2],
    pub transform: Transform,
    pub blend: BlendMode,
    /// Effects to apply to the layer first, top of the stack first.
    pub effects: Vec<EffectValues>,
    /// An alpha mask over the layer (applied before the effects).
    pub mask: Option<MaskInput<'a>>,
    /// Reveal the layer with a moving edge (wipe transitions).
    pub wipe: Option<Wipe>,
    /// Stretch the layer over this grid (frame pixels) instead of placing it by
    /// its transform (only opacity and crop still apply).
    pub mesh: Option<reelforge_core::meshwarp::MeshShape>,
    /// A 3D layer: placed by the camera instead (opacity and crop still
    /// apply), and lit by the scene's lights if it has any.
    pub space: Option<&'a reelforge_core::space3d::LayerSpace>,
}

/// A wipe: the layer shows where `q < progress`, with a soft edge. For a
/// straight wipe q = axis·(x/w, y/h, 1) over the frame; for an iris q is the
/// distance from the middle (0 at the centre, 1 in the corners).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Wipe {
    pub iris: bool,
    pub axis: [f32; 3],
    pub progress: f32,
    /// Width of the soft edge, as a fraction of the frame.
    pub softness: f32,
}

impl Wipe {
    /// The wipe for a transition moving in `dir` (x right, y down), `progress` of the way.
    pub fn moving(dir: [f32; 2], progress: f32) -> Self {
        // Revealed first where the edge starts: the side opposite the motion.
        let axis = match dir {
            [x, _] if x < 0.0 => [-1.0, 0.0, 1.0],
            [x, _] if x > 0.0 => [1.0, 0.0, 0.0],
            [_, y] if y < 0.0 => [0.0, -1.0, 1.0],
            _ => [0.0, 1.0, 0.0],
        };
        Self { iris: false, axis, progress, softness: 0.08 }
    }
    pub fn iris(progress: f32) -> Self {
        Self { iris: true, axis: [0.0; 3], progress, softness: 0.08 }
    }
}

/// A layer's mask: 8-bit alpha, stretched over the whole layer.
#[derive(Clone, Copy)]
pub struct MaskInput<'a> {
    /// Changes whenever `pixels` (or `keep`) changes.
    pub content_id: u64,
    pub pixels: &'a [u8],
    pub size: [u32; 2],
    /// Where keys must keep the layer (garbage mattes), the same size as
    /// `pixels`; empty when there are none.
    pub keep: &'a [u8],
    /// Refine edge: (radius in layer pixels, smoothness 0–100, shift −100 to 100, decontamination 0–100).
    pub refine: Option<[f32; 4]>,
}

/// Masks of a layer's key garbage mattes are kept under the layer's key with this bit flipped.
const KEEP_MASK: u64 = 1 << 62;

/// Something to draw, bottom first.
// Built per frame and consumed at once, so the size difference doesn't matter.
#[allow(clippy::large_enum_variant)]
pub enum DrawItem<'a> {
    Layer(LayerInput<'a>),
    /// Cross-dissolve from `from` to `to`; `mix` 0 → 1. The pair is composited
    /// with `to`'s blend mode. With `dip`, it fades out to that colour (linear
    /// RGB) and in from it instead.
    Dissolve { from: LayerInput<'a>, to: LayerInput<'a>, mix: f32, dip: Option<[f32; 3]> },
    /// `layer` seen only through `matte` (the layer above it, not drawn
    /// itself): `kind` 0 its alpha, 1 its alpha inverted, 2 its brightness,
    /// 3 its brightness inverted. Composited with `layer`'s blend mode.
    Matted { matte: LayerInput<'a>, layer: LayerInput<'a>, kind: u32 },
}

struct LayerTex {
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    size: [u32; 2],
    content_id: u64,
    /// A float texture of premultiplied linear light (an EXR), rather than
    /// sRGB with straight alpha.
    float: bool,
}

/// Two scratch targets the size of a layer's pixels, for its effect chain.
/// Three scratch targets the size of a layer's pixels, for its effect chain
/// (two for ping-pong, one more to keep the layer while glow or a shadow is built).
struct FxTargets {
    size: [u32; 2],
    /// Three, or four for a mask's refined edge.
    views: Vec<wgpu::TextureView>,
    binds: Vec<wgpu::BindGroup>,
}

struct Targets {
    size: [u32; 2],
    accum_view: wgpu::TextureView,
    /// Scratch target for dissolve groups, and its bind group for sampling it as a layer.
    group_view: wgpu::TextureView,
    group_bind: wgpu::BindGroup,
    output: wgpu::Texture,
    output_view: wgpu::TextureView,
    output_bind: wgpu::BindGroup,
    /// The frame so far, for adjustment layers to read.
    accum_bind: wgpu::BindGroup,
    accum: wgpu::Texture,
    /// A copy of the frame so far, for a layer whose blend mode reads it.
    backdrop: wgpu::Texture,
    backdrop_bind: wgpu::BindGroup,
    /// Depth, for runs of 3D layers (so layers that cut through each other do).
    depth_view: wgpu::TextureView,
    /// Track mattes: the matte drawn alone, the matted layer drawn alone
    /// (into the group target), and the two put together.
    matte_view: wgpu::TextureView,
    matte_load: wgpu::BindGroup,
    group_load: wgpu::BindGroup,
    matted_view: wgpu::TextureView,
    matted_bind: wgpu::BindGroup,
}

pub struct Compositor {
    /// The colour bit depth worked at, and its textures.
    formats: Formats,
    /// Dissolves and mattes mixed in display values, drawn onto a frame of them (only
    /// when the frame holds display values).
    encoded_group_pipelines: HashMap<BlendMode, wgpu::RenderPipeline>,
    /// A clear 1 × 1 texture, for an effect pass whose second texture is optional.
    blank_second: wgpu::BindGroup,
    device: wgpu::Device,
    queue: wgpu::Queue,
    layer_pipelines: HashMap<BlendMode, wgpu::RenderPipeline>,
    /// Draws a premultiplied group texture over the frame, per blend mode.
    group_pipelines: HashMap<BlendMode, wgpu::RenderPipeline>,
    /// Layers with Light wrap: the layer's edges take on the blurred frame under them, then blended (per mode).
    wrap_pipelines: HashMap<BlendMode, wgpu::RenderPipeline>,
    wrap_group_pipelines: HashMap<BlendMode, wgpu::RenderPipeline>,
    /// The same, depth-tested, for runs of 3D layers.
    layer_pipelines_3d: HashMap<BlendMode, wgpu::RenderPipeline>,
    group_pipelines_3d: HashMap<BlendMode, wgpu::RenderPipeline>,
    /// 3D text in a 3D run: its depth comes from its depth picture.
    solid_pipelines_3d: HashMap<BlendMode, wgpu::RenderPipeline>,
    solid_group_pipelines_3d: HashMap<BlendMode, wgpu::RenderPipeline>,
    /// Adds layers into a group target (colour and alpha both summed).
    sum_pipeline: wgpu::RenderPipeline,
    /// Same, for a layer whose effects already made it premultiplied.
    sum_premul_pipeline: wgpu::RenderPipeline,
    /// One per effect pass kind (fragment entry point), all writing float targets.
    fx_pipelines: HashMap<&'static str, wgpu::RenderPipeline>,
    fx_uniforms: wgpu::Buffer,
    fx_uniform_bind: wgpu::BindGroup,
    fx_uniform_stride: u64,
    fx_targets: HashMap<u64, FxTargets>,
    output_pipeline: wgpu::RenderPipeline,
    output_alpha_pipeline: wgpu::RenderPipeline,
    /// Track mattes, by kind (see [`DrawItem::Matted`]).
    matte_pipelines: [wgpu::RenderPipeline; 4],
    /// 16-bit export outputs: sRGB, PQ, HLG.
    /// (and the frame as linear half floats, last)
    deep_pipelines: [wgpu::RenderPipeline; 5],
    /// Render over transparency and keep the alpha in the output (for composite
    /// shots, which are layered again), instead of over opaque black.
    pub transparent: bool,
    /// Draw at this multiple of the frame size (the Viewer's playback
    /// quality: 1, ½ or ¼; up to 8 for a composition drawn finer); layers,
    /// effects and wipes keep their places.
    pub output_scale: f32,
    texture_layout: wgpu::BindGroupLayout,
    output_layout: wgpu::BindGroupLayout,
    uniforms: wgpu::Buffer,
    uniform_bind: wgpu::BindGroup,
    uniform_stride: u64,
    sampler: wgpu::Sampler,
    /// 3D layers' outlines, for the shadows they cast (see `occluder.wgsl`).
    shadow_atlas_view: wgpu::TextureView,
    /// Its tile size, and what the layers' uniforms are bound with (to swap the atlas).
    shadow_tile: u32,
    uniform_layout: wgpu::BindGroupLayout,
    occluder_pipeline: wgpu::RenderPipeline,
    layers: HashMap<u64, LayerTex>,
    /// Mask textures, by layer key.
    masks: HashMap<u64, LayerTex>,
    /// Effect passes that read a second texture (mask, lookup table, another target).
    fx2_pipelines: HashMap<&'static str, wgpu::RenderPipeline>,
    /// Lookup tables (curves, LUTs), by layer key and effect index; `content_id` is the table id.
    tables: HashMap<(u64, usize), LayerTex>,
    /// Layout for passes that read two textures (plugins use it: the picture and the original).
    fx2_layout: wgpu::PipelineLayout,
    /// Compiled plugin passes, by plugin revision (`None`: it failed to compile).
    plugin_pipelines: HashMap<u64, Option<Vec<wgpu::RenderPipeline>>>,
    /// The revision compiled for each plugin id, so a reloaded plugin replaces its old pipelines.
    plugin_revisions: HashMap<String, u64>,
    targets: Option<Targets>,
    /// Bumped when the output texture is recreated (resize), so a UI can re-register it.
    pub output_generation: u64,
    /// Converts YUV layers into their RGBA layer textures.
    yuv: yuv::YuvConverter,
    /// Draws solid objects (3D text) into their layer textures.
    solid: solid::SolidRenderer,
    particles: particles::ParticleRenderer,
    /// Fluid effects, simulated on the card.
    fluid: fluidgpu::FluidGpu,
    /// Water surfaces and liquids, simulated on the card.
    water: watergpu::WaterGpu,
    /// Liquids in 3D, simulated on the card.
    liquid3d: liquid3dgpu::Liquid3dGpu,
    /// Reads frames shared by the hardware decoder (Windows, Direct3D 12).
    #[cfg(windows)]
    interop: Option<interop::Importer>,
    /// Layouts whose shared frames have been checked against the usual copy.
    gpu_checked: std::collections::HashSet<reelforge_core::frame::YuvLayout>,
}

fn blend_state(mode: BlendMode) -> wgpu::BlendState {
    use wgpu::{BlendComponent as C, BlendFactor as F, BlendOperation as Op};
    let comp = |src, dst| C { src_factor: src, dst_factor: dst, operation: Op::Add };
    // Inputs are premultiplied; the destination is the opaque frame so far.
    let color = match mode {
        BlendMode::Normal => comp(F::One, F::OneMinusSrcAlpha),
        BlendMode::Add => comp(F::One, F::One),
        // src·dst + dst·(1 − αs)  ==  dst · lerp(1, src, α)
        BlendMode::Multiply => comp(F::Dst, F::OneMinusSrcAlpha),
        // src + dst·(1 − src)
        BlendMode::Screen => comp(F::One, F::OneMinusSrc),
        // The rest are worked out in the shader against a copy of the frame
        // (see `fs_blend`), and the result is drawn over it.
        _ => comp(F::One, F::OneMinusSrcAlpha),
    };
    wgpu::BlendState { color, alpha: comp(F::One, F::OneMinusSrcAlpha) }
}

impl Compositor {
    /// A compositor at the default colour bit depth ([`set_default_depth`]).
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        Self::with_depth(device, queue, default_depth())
    }

    /// The colour bit depth worked at (the one asked for, or the nearest the graphics
    /// card can do).
    pub fn depth(&self) -> ColorDepth {
        self.formats.depth
    }

    /// A compositor working at colour bit depth `depth`.
    pub fn with_depth(device: &wgpu::Device, queue: &wgpu::Queue, depth: ColorDepth) -> Self {
        let formats = Formats::for_depth(depth, device.features());
        // (Neon path can follow a title's letters, which only the renderer can lay out.)
        reelforge_core::neonpath::set_letter_outlines(text3d::letter_outlines);
        let layer_shader = device.create_shader_module(wgpu::include_wgsl!("composite.wgsl"));
        let output_shader = device.create_shader_module(wgpu::include_wgsl!("output.wgsl"));

        let align = device.limits().min_uniform_buffer_offset_alignment as u64;
        let uniform_stride = UNIFORM_SIZE.div_ceil(align) * align;

        let uniform_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("layer uniforms"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: true,
                        min_binding_size: wgpu::BufferSize::new(UNIFORM_SIZE),
                    },
                    count: None,
                },
                // The shadow atlas: 3D layers' outlines, for lit layers' shadows.
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
                    count: None,
                },
            ],
        });
        let shadow_tile = SHADOW_TILE.load(std::sync::atomic::Ordering::Relaxed);
        let shadow_atlas_view = atlas_texture(device, shadow_tile);
        let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("layer texture"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    // (Vertex: particles born from a layer's pixels.)
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let output_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("accum"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            }],
        });

        let layer_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("layer"),
            bind_group_layouts: &[Some(&uniform_layout), Some(&texture_layout)],
            immediate_size: 0,
        });
        // Blend modes that read the frame under the layer get it as a third texture.
        let blend_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("layer (blend mode)"),
            bind_group_layouts: &[Some(&uniform_layout), Some(&texture_layout), Some(&output_layout)],
            immediate_size: 0,
        });
        // `onto_frame`: draws onto the frame (else onto a dissolve's or matte's scratch):
        // at the frame's format, gamma-encoding what it draws when the frame holds
        // display values.
        // In display values (no linear colour), a dissolve's or matte's scratch is summed
        // encoded too (`onto_frame` false), and drawn onto the frame as it is (`encoded`).
        let make_enc = |label: &str, entry: &str, blend: wgpu::BlendState, mode: Option<usize>, depth: bool, third: bool, onto_frame: bool, encoded: bool| {
            let constants: Vec<(&str, f64)> = mode
                .map(|m| ("MODE", m as f64))
                .into_iter()
                .chain(depth.then_some(("DEPTH_CUT", DEPTH_CUT)))
                .chain(formats.gamma.then_some(("GAMMA", 1.0)))
                .chain(encoded.then_some(("SRC_ENCODED", 1.0)))
                .collect();
            let target = if onto_frame { formats.accum } else { formats.inter };
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(if mode.is_some() || third { &blend_pl } else { &layer_pl }),
                vertex: wgpu::VertexState {
                    module: &layer_shader,
                    entry_point: Some("vs_layer"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &layer_shader,
                    entry_point: Some(entry),
                    compilation_options: wgpu::PipelineCompilationOptions { constants: &constants, ..Default::default() },
                    targets: &[Some(wgpu::ColorTargetState { format: target, blend: Some(blend), write_mask: wgpu::ColorWrites::ALL })],
                }),
                primitive: wgpu::PrimitiveState::default(), // no culling: flips/negative scale are fine
                depth_stencil: depth.then(|| wgpu::DepthStencilState {
                    format: DEPTH_FORMAT,
                    depth_write_enabled: Some(true),
                    depth_compare: Some(wgpu::CompareFunction::Less),
                    stencil: Default::default(),
                    bias: Default::default(),
                }),
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        let make_full = |label: &str, entry: &str, blend: wgpu::BlendState, mode: Option<usize>, depth: bool, third: bool, onto_frame: bool| make_enc(label, entry, blend, mode, depth, third, onto_frame, false);
        let make_with = |label: &str, entry: &str, blend: wgpu::BlendState, mode: Option<usize>| make_full(label, entry, blend, mode, false, false, true);
        let make = |label: &str, entry: &str, blend: wgpu::BlendState| make_with(label, entry, blend, None);
        // 3D runs: depth-tested, for every blend mode that doesn't read the frame under it.
        // (3D text brings its own depth picture as the third texture.)
        let per_mode_3d = |entry: &str, third: bool| -> HashMap<BlendMode, wgpu::RenderPipeline> {
            BlendMode::ALL.into_iter().filter(|m| !m.reads_backdrop()).map(|m| (m, make_full(m.label(), entry, blend_state(m), None, true, third, true))).collect()
        };
        let per_mode = |plain: &str, blend_entry: &str| -> HashMap<BlendMode, wgpu::RenderPipeline> {
            BlendMode::ALL
                .into_iter()
                .enumerate()
                .map(|(i, m)| (m, if m.reads_backdrop() { make_with(m.label(), blend_entry, blend_state(m), Some(i)) } else { make(m.label(), plain, blend_state(m)) }))
                .collect()
        };
        let layer_pipelines = per_mode("fs_layer", "fs_blend");
        let per_mode_wrap = |entry: &str| -> HashMap<BlendMode, wgpu::RenderPipeline> { BlendMode::ALL.into_iter().enumerate().map(|(i, m)| (m, make_with(m.label(), entry, blend_state(m), Some(i)))).collect() };
        let wrap_pipelines = per_mode_wrap("fs_wrap");
        let wrap_group_pipelines = per_mode_wrap("fs_wrap_premul");
        let group_pipelines = per_mode("fs_layer_premul", "fs_blend_premul");
        // A dissolve or matte mixed in display values, drawn onto a frame of them.
        let encoded_group_pipelines: HashMap<BlendMode, wgpu::RenderPipeline> = if formats.gamma {
            BlendMode::ALL
                .into_iter()
                .enumerate()
                .map(|(i, m)| {
                    let (entry, mode) = if m.reads_backdrop() { ("fs_blend_premul", Some(i)) } else { ("fs_layer_premul", None) };
                    (m, make_enc(m.label(), entry, blend_state(m), mode, false, false, true, true))
                })
                .collect()
        } else {
            HashMap::new()
        };
        let layer_pipelines_3d = per_mode_3d("fs_layer", false);
        let group_pipelines_3d = per_mode_3d("fs_layer_premul", false);
        let solid_pipelines_3d = per_mode_3d("fs_solid", true);
        let solid_group_pipelines_3d = per_mode_3d("fs_solid_premul", true);
        let sum = wgpu::BlendComponent { src_factor: wgpu::BlendFactor::One, dst_factor: wgpu::BlendFactor::One, operation: wgpu::BlendOperation::Add };
        let sum_pipeline = make_full("dissolve sum", "fs_layer", wgpu::BlendState { color: sum, alpha: sum }, None, false, false, false);
        let sum_premul_pipeline = make_full("dissolve sum (premultiplied)", "fs_layer_premul", wgpu::BlendState { color: sum, alpha: sum }, None, false, false, false);

        // Effect passes.
        let fx_shader = device.create_shader_module(wgpu::include_wgsl!("effects.wgsl"));
        let fx_uniform_stride = FX_UNIFORM_SIZE.div_ceil(align) * align;
        let fx_uniform_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("effect uniforms"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT, // (vertex: particles)
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: wgpu::BufferSize::new(FX_UNIFORM_SIZE),
                },
                count: None,
            }],
        });
        let fx_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("effects"),
            bind_group_layouts: &[Some(&fx_uniform_layout), Some(&texture_layout)],
            immediate_size: 0,
        });
        let mask_shader = device.create_shader_module(wgpu::include_wgsl!("mask.wgsl"));
        let mask_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mask"),
            bind_group_layouts: &[Some(&fx_uniform_layout), Some(&texture_layout), Some(&texture_layout)],
            immediate_size: 0,
        });
        let mut fx2_pipelines: HashMap<&'static str, wgpu::RenderPipeline> = ["fs_mask", "fs_curves", "fs_lut", "fs_glow_add", "fs_under", "fs_diff_key", "fs_neon_mix", "fs_decon_mix", "fs_fluid", "fs_lightning", "fs_fill", "fs_refine_trimap", "fs_refine_in", "fs_refine_out", "fs_refine_line", "fs_refine_apply", "fs_paint", "fs_liquify", "fs_depth_map", "fs_relight", "fs_portrait", "fs_volumetric", "fs_sparkle", "fs_hud_text", "fs_beauty", "fs_sky_cut", "fs_sky_fill", "fs_streak_add", "fs_hue_curves", "fs_audio_vis", "fs_compound_mix", "fs_cross_mix", "fs_auto_apply"]
            .into_iter()
            .map(|entry| {
                let p = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some(entry),
                    layout: Some(&mask_pl),
                    vertex: wgpu::VertexState { module: &mask_shader, entry_point: Some("vs_fullscreen"), compilation_options: Default::default(), buffers: &[] },
                    fragment: Some(wgpu::FragmentState {
                        module: &mask_shader,
                        entry_point: Some(entry),
                        compilation_options: Default::default(),
                        targets: &[Some(wgpu::ColorTargetState { format: formats.inter, blend: None, write_mask: wgpu::ColorWrites::ALL })],
                    }),
                    primitive: wgpu::PrimitiveState::default(),
                    depth_stencil: None,
                    multisample: wgpu::MultisampleState::default(),
                    multiview_mask: None,
                    cache: None,
                });
                (entry, p)
            })
            .collect();
        let mut fx_pipelines: HashMap<&'static str, wgpu::RenderPipeline> = ["fs_import", "fs_import_float", "fs_from_gamma", "fs_color", "fs_blur", "fs_key", "fs_luma_key", "fs_key_choke", "fs_key_soft", "fs_key_out", "fs_key_clean", "fs_straight", "fs_matte_out", "fs_matte_steep", "fs_decon_blur", "fs_wire", "fs_vignette", "fs_levels", "fs_wheels", "fs_qualify", "fs_sharpen", "fs_bright", "fs_shadow", "fs_blade", "fs_copy", "fs_flare", "fs_shake", "fs_aberration", "fs_grain", "fs_glitch", "fs_leaks", "fs_neon_edge", "fs_halftone", "fs_crt", "fs_clear", "fs_muzzle", "fs_bullet", "fs_disintegrate", "fs_fractal", "fs_patch", "fs_rays", "fs_dirt", "fs_bulge", "fs_twirl", "fs_ripple", "fs_wave", "fs_polar", "fs_mirror", "fs_lens", "fs_film_look", "fs_film_damage", "fs_analogue", "fs_abstraction", "fs_prism", "fs_tilt_shift", "fs_scanlines", "fs_streak_src", "fs_strobe", "fs_ramp", "fs_smoke_distort", "fs_fluid_distort", "fs_energy_distort", "fs_derez", "fs_block_displace", "fs_blood", "fs_blaster", "fs_teleport", "fs_force_field", "fs_custom_flare", "fs_halation", "fs_gleam", "fs_spotlight", "fs_xray", "fs_infrared", "fs_thermal", "fs_night_vision", "fs_motion_blur", "fs_radial_blur", "fs_dir_blur", "fs_arcs", "fs_sweep", "fs_hud", "fs_portal", "fs_hologram", "fs_starfield", "fs_shockwave", "fs_heat_haze", "fs_turbulent", "fs_gradient_map", "fs_aurora", "fs_nebula", "fs_clouds", "fs_burn", "fs_deflicker", "fs_rotate3d", "fs_bend", "fs_bevel", "fs_extrude", "fs_extrusion3d", "fs_demultiply", "fs_stock_background", "fs_block_dissolve", "fs_gradient_wipe", "fs_iris_wipe", "fs_linear_wipe", "fs_radial_wipe", "fs_venetian_blinds", "fs_glass_wipe", "fs_grid_wipe", "fs_jaws", "fs_light_wipe", "fs_line_sweep", "fs_radial_scale_wipe", "fs_scale_wipe", "fs_twister", "fs_warpomatic", "fs_numbers", "fs_timecode", "fs_cineon", "fs_hdr_compress", "fs_ball_action", "fs_bubbles", "fs_drizzle", "fs_color_key", "fs_color_range", "fs_color_difference_key", "fs_linear_color_key", "fs_extract", "fs_spill_suppressor", "fs_brush_strokes", "fs_cartoon", "fs_emboss", "fs_color_emboss", "fs_find_edges", "fs_mosaic_blocks", "fs_posterize", "fs_roughen_edges", "fs_scatter", "fs_texturize", "fs_threshold", "fs_block_load", "fs_burn_film", "fs_hex_tile", "fs_kaleida", "fs_mr_smoothie", "fs_plastic", "fs_glass_cc", "fs_repe_tile", "fs_matte_view", "fs_four_color", "fs_beam", "fs_cell_pattern", "fs_checkerboard", "fs_circle", "fs_ellipse", "fs_fill_colour", "fs_grid", "fs_radio_waves", "fs_threads", "fs_light_burst", "fs_curl_noise", "fs_noise", "fs_noise_hls", "fs_median", "fs_dust_scratches", "fs_remove_grain", "fs_corner_pin", "fs_power_pin", "fs_displacement_map", "fs_magnify", "fs_mesh_warp", "fs_offset", "fs_optics", "fs_spherize", "fs_smear", "fs_warp", "fs_bender", "fs_blobbylize", "fs_flo_motion", "fs_lens_cc", "fs_page_turn", "fs_slant", "fs_split", "fs_tiler", "fs_griddler", "fs_ripple_pulse", "fs_motion_tile", "fs_glasses3d", "fs_radial_shadow", "fs_cylinder", "fs_sphere", "fs_smart_blur", "fs_lens_blur", "fs_vector_blur", "fs_arithmetic", "fs_blend_colour", "fs_calculations", "fs_channel_mixer", "fs_invert", "fs_shift_channels", "fs_set_channels", "fs_set_matte", "fs_solid_composite", "fs_change_color", "fs_change_to_color", "fs_color_balance", "fs_exposure", "fs_leave_color", "fs_photo_filter", "fs_selective_color", "fs_tint", "fs_black_white", "fs_color_neutralizer", "fs_color_offset", "fs_toner", "fs_threshold_rgb", "fs_kernel", "fs_video_limiter", "fs_box_blur", "fs_channel_blur", "fs_minimax", "fs_stats"]
            .into_iter()
            .map(|entry| {
                let p = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                    label: Some(entry),
                    layout: Some(&fx_pl),
                    vertex: wgpu::VertexState {
                        module: &fx_shader,
                        entry_point: Some("vs_fullscreen"),
                        compilation_options: Default::default(),
                        buffers: &[],
                    },
                    fragment: Some(wgpu::FragmentState {
                        module: &fx_shader,
                        entry_point: Some(entry),
                        compilation_options: Default::default(),
                        targets: &[Some(wgpu::ColorTargetState { format: formats.inter, blend: None, write_mask: wgpu::ColorWrites::ALL })],
                    }),
                    primitive: wgpu::PrimitiveState::default(),
                    depth_stencil: None,
                    multisample: wgpu::MultisampleState::default(),
                    multiview_mask: None,
                    cache: None,
                });
                (entry, p)
            })
            .collect();
        // Water (water.wgsl): the ocean on its own, the simulations' pictures with their state.
        let water_shader = device.create_shader_module(wgpu::include_wgsl!("water.wgsl"));
        // (the ocean reads a simulation round solid objects when it has one, a clear
        // texture when it hasn't: see `blank_second`)
        for (entry, layout, two) in [("fs_ocean", &mask_pl, true), ("fs_water", &mask_pl, true), ("fs_liquid", &mask_pl, true), ("fs_liquid3d", &mask_pl, true)] {
            let p = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(entry),
                layout: Some(layout),
                vertex: wgpu::VertexState { module: &water_shader, entry_point: Some("vs_fullscreen"), compilation_options: Default::default(), buffers: &[] },
                fragment: Some(wgpu::FragmentState {
                    module: &water_shader,
                    entry_point: Some(entry),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState { format: formats.inter, blend: None, write_mask: wgpu::ColorWrites::ALL })],
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            });
            if two {
                fx2_pipelines.insert(entry, p);
            } else {
                fx_pipelines.insert(entry, p);
            }
        }
        let fx_uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("effect uniforms"),
            size: fx_uniform_stride * MAX_FX_PASSES as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let fx_uniform_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("effect uniforms"),
            layout: &fx_uniform_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding { buffer: &fx_uniforms, offset: 0, size: wgpu::BufferSize::new(FX_UNIFORM_SIZE) }),
            }],
        });

        let output_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("output"),
            bind_group_layouts: &[Some(&output_layout)],
            immediate_size: 0,
        });
        // The frame's values decoded from gamma first, where it holds display values.
        let output_constants: Vec<(&str, f64)> = if formats.gamma { vec![("GAMMA", 1.0)] } else { vec![] };
        let output_pipeline_with = |entry: &str, format: wgpu::TextureFormat| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(entry),
                layout: Some(&output_pl),
                vertex: wgpu::VertexState {
                    module: &output_shader,
                    entry_point: Some("vs_fullscreen"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &output_shader,
                    entry_point: Some(entry),
                    compilation_options: wgpu::PipelineCompilationOptions { constants: &output_constants, ..Default::default() },
                    targets: &[Some(wgpu::ColorTargetState { format, blend: None, write_mask: wgpu::ColorWrites::ALL })],
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        let output_pipeline = output_pipeline_with("fs_output", OUTPUT_FORMAT);
        let output_alpha_pipeline = output_pipeline_with("fs_output_alpha", OUTPUT_FORMAT);
        let deep_pipelines = ["fs_deep_srgb", "fs_deep_pq", "fs_deep_hlg", "fs_deep_srgb_alpha", "fs_linear"].map(|e| output_pipeline_with(e, DEEP_FORMAT));
        let matte_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("track mattes"), source: wgpu::ShaderSource::Wgsl(include_str!("matte.wgsl").into()) });
        let matte_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("track mattes"), bind_group_layouts: &[Some(&output_layout), Some(&output_layout)], immediate_size: 0 });
        let matte_pipelines = ["fs_matte_alpha", "fs_matte_alpha_inv", "fs_matte_luma", "fs_matte_luma_inv"].map(|entry| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(entry),
                layout: Some(&matte_pl),
                vertex: wgpu::VertexState { module: &matte_shader, entry_point: Some("vs_fullscreen"), compilation_options: Default::default(), buffers: &[] },
                fragment: Some(wgpu::FragmentState { module: &matte_shader, entry_point: Some(entry), compilation_options: Default::default(), targets: &[Some(wgpu::ColorTargetState { format: formats.inter, blend: None, write_mask: wgpu::ColorWrites::ALL })] }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        });

        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("layer uniforms"),
            size: uniform_stride * MAX_DRAWS as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let uniform_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("layer uniforms"),
            layout: &uniform_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                        buffer: &uniforms,
                        offset: 0,
                        size: wgpu::BufferSize::new(UNIFORM_SIZE),
                    }),
                },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&shadow_atlas_view) },
            ],
        });
        // Draws a layer's alpha into its tile of the shadow atlas.
        let occluder_pipeline = {
            let shader = device.create_shader_module(wgpu::include_wgsl!("occluder.wgsl"));
            let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("occluder"), bind_group_layouts: &[Some(&texture_layout)], immediate_size: 0 });
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("occluder"),
                layout: Some(&pl),
                vertex: wgpu::VertexState { module: &shader, entry_point: Some("vs_tile"), compilation_options: Default::default(), buffers: &[] },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_alpha"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState { format: wgpu::TextureFormat::R8Unorm, blend: None, write_mask: wgpu::ColorWrites::ALL })],
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("layer"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let blank_second = {
            let texture = device.create_texture(&wgpu::TextureDescriptor {
                label: Some("no second texture"),
                size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            queue.write_texture(
                wgpu::TexelCopyTextureInfo { texture: &texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
                &[0, 0, 0, 0],
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4), rows_per_image: Some(1) },
                wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
            );
            let view = texture.create_view(&Default::default());
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("no second texture"),
                layout: &texture_layout,
                entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) }, wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&sampler) }],
            })
        };
        Self {
            blank_second,
            device: device.clone(),
            queue: queue.clone(),
            layer_pipelines,
            group_pipelines,
            wrap_pipelines,
            wrap_group_pipelines,
            layer_pipelines_3d,
            group_pipelines_3d,
            solid_pipelines_3d,
            solid_group_pipelines_3d,
            sum_pipeline,
            sum_premul_pipeline,
            fx_pipelines,
            fx_uniforms,
            fx_uniform_bind,
            fx_uniform_stride,
            fx_targets: HashMap::new(),
            output_pipeline,
            output_alpha_pipeline,
            matte_pipelines,
            deep_pipelines,
            yuv: yuv::YuvConverter::new(device, LAYER_FORMAT, FLOAT_LAYER_FORMAT),
            solid: solid::SolidRenderer::new(device, queue, LAYER_FORMAT),
            particles: particles::ParticleRenderer::new(device, &fx_pl, formats.inter),
            formats,
            encoded_group_pipelines,
            fluid: fluidgpu::FluidGpu::new(device, queue),
            water: watergpu::WaterGpu::new(device, queue),
            liquid3d: liquid3dgpu::Liquid3dGpu::new(device, queue),
            #[cfg(windows)]
            interop: {
                let i = interop::Importer::new(device);
                if let Some(i) = &i {
                    log::info!("zero-copy video: the compositor can read shared frames (adapter {:#x})", i.adapter());
                }
                i
            },
            gpu_checked: Default::default(),
            transparent: false,
            output_scale: 1.0,
            texture_layout,
            output_layout,
            uniforms,
            uniform_bind,
            uniform_stride,
            sampler,
            shadow_atlas_view,
            shadow_tile,
            uniform_layout,
            occluder_pipeline,
            layers: HashMap::new(),
            masks: HashMap::new(),
            fx2_pipelines,
            tables: HashMap::new(),
            fx2_layout: mask_pl,
            plugin_pipelines: HashMap::new(),
            plugin_revisions: HashMap::new(),
            targets: None,
            output_generation: 0,
        }
    }

    /// Compile a plugin's passes (once per revision). False if it can't run.
    fn ensure_plugin(&mut self, def: &reelforge_core::plugin::PluginDef) -> bool {
        if let Some(r) = self.plugin_pipelines.get(&def.revision) {
            return r.is_some();
        }
        // A reloaded plugin: forget the old version.
        if let Some(old) = self.plugin_revisions.insert(def.id.clone(), def.revision) {
            self.plugin_pipelines.remove(&old);
        }
        let compiled = match plugin::validate(def) {
            Err(e) => {
                log::warn!("plugin {} can't be used: {e}", def.id);
                None
            }
            Ok(source) => {
                // Anything the check above missed is caught here rather than crashing.
                let scope = self.device.push_error_scope(wgpu::ErrorFilter::Validation);
                let module = self.device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some(&def.id), source: wgpu::ShaderSource::Wgsl(source.into()) });
                let passes: Vec<wgpu::RenderPipeline> = (0..def.passes.len())
                    .map(|k| {
                        let entry = plugin::entry_point(k);
                        self.device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                            label: Some(&entry),
                            layout: Some(&self.fx2_layout),
                            vertex: wgpu::VertexState { module: &module, entry_point: Some("rf_vs"), compilation_options: Default::default(), buffers: &[] },
                            fragment: Some(wgpu::FragmentState {
                                module: &module,
                                entry_point: Some(&entry),
                                compilation_options: Default::default(),
                                targets: &[Some(wgpu::ColorTargetState { format: self.formats.inter, blend: None, write_mask: wgpu::ColorWrites::ALL })],
                            }),
                            primitive: wgpu::PrimitiveState::default(),
                            depth_stencil: None,
                            multisample: wgpu::MultisampleState::default(),
                            multiview_mask: None,
                            cache: None,
                        })
                    })
                    .collect();
                match pollster::block_on(scope.pop()) {
                    Some(e) => {
                        log::warn!("plugin {} failed on the GPU: {e}", def.id);
                        None
                    }
                    None => Some(passes),
                }
            }
        };
        let ok = compiled.is_some();
        self.plugin_pipelines.insert(def.revision, compiled);
        ok
    }

    pub fn output_view(&self) -> Option<&wgpu::TextureView> {
        self.targets.as_ref().map(|t| &t.output_view)
    }

    pub fn output_size(&self) -> Option<[u32; 2]> {
        self.targets.as_ref().map(|t| t.size)
    }

    fn ensure_targets(&mut self, size: [u32; 2]) {
        if self.targets.as_ref().is_some_and(|t| t.size == size) {
            return;
        }
        let extent = wgpu::Extent3d { width: size[0].max(1), height: size[1].max(1), depth_or_array_layers: 1 };
        let tex = |label, format, usage| {
            self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: extent,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage,
                view_formats: &[],
            })
        };
        use wgpu::TextureUsages as U;
        let accum = tex("accum", self.formats.accum, U::RENDER_ATTACHMENT | U::TEXTURE_BINDING | U::COPY_SRC);
        let backdrop = tex("backdrop", self.formats.accum, U::TEXTURE_BINDING | U::COPY_DST);
        let backdrop_bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("backdrop"),
            layout: &self.output_layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&backdrop.create_view(&Default::default())) }],
        });
        let group = tex("dissolve group", self.formats.inter, U::RENDER_ATTACHMENT | U::TEXTURE_BINDING);
        let group_view = group.create_view(&Default::default());
        let group_bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("dissolve group"),
            layout: &self.texture_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&group_view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.sampler) },
            ],
        });
        let output = tex("output", OUTPUT_FORMAT, U::RENDER_ATTACHMENT | U::TEXTURE_BINDING | U::COPY_SRC);
        let accum_view = accum.create_view(&Default::default());
        let output_view = output.create_view(&Default::default());
        let output_bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("accum"),
            layout: &self.output_layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&accum_view) }],
        });
        let accum_bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("frame so far"),
            layout: &self.texture_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&accum_view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.sampler) },
            ],
        });
        let depth_view = tex("3D depth", DEPTH_FORMAT, U::RENDER_ATTACHMENT).create_view(&Default::default());
        let load = |label, view: &wgpu::TextureView| {
            self.device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some(label), layout: &self.output_layout, entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(view) }] })
        };
        let matte_view = tex("track matte", self.formats.inter, U::RENDER_ATTACHMENT | U::TEXTURE_BINDING).create_view(&Default::default());
        let matte_load = load("track matte", &matte_view);
        let group_load = load("matted layer", &group_view);
        let matted_view = tex("matted", self.formats.inter, U::RENDER_ATTACHMENT | U::TEXTURE_BINDING).create_view(&Default::default());
        let matted_bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("matted"),
            layout: &self.texture_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&matted_view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.sampler) },
            ],
        });
        self.targets = Some(Targets { size, accum_view, group_view, group_bind, output, output_view, output_bind, accum_bind, accum, backdrop, backdrop_bind, depth_view, matte_view, matte_load, group_load, matted_view, matted_bind });
        self.output_generation += 1;
    }

    fn upload(&mut self, l: &LayerInput) {
        let [w, h] = l.pixel_size;
        if let Some(t) = self.layers.get(&l.key) {
            if t.size == l.pixel_size && t.content_id == l.content_id {
                return; // unchanged since last frame
            }
        }
        // Floating point, and deep (10-bit and up) video: kept in half floats.
        let float = l.pixels.is_deep();
        let needs_new = self.layers.get(&l.key).is_none_or(|t| t.size != l.pixel_size || t.float != float);
        if needs_new {
            let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("layer"),
                size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: if float { FLOAT_LAYER_FORMAT } else { LAYER_FORMAT },
                // Written by upload (RGBA) or by the YUV conversion pass.
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
                view_formats: &[],
            });
            let view = texture.create_view(&Default::default());
            let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("layer"),
                layout: &self.texture_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                ],
            });
            self.layers.insert(l.key, LayerTex { texture, bind_group, size: l.pixel_size, content_id: u64::MAX, float });
        }
        let Some(t) = self.layers.get_mut(&l.key) else { return };
        match l.pixels {
            PixelData::Below => {}
            PixelData::Rgba(px) => self.queue.write_texture(
                wgpu::TexelCopyTextureInfo { texture: &t.texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
                px,
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4 * w), rows_per_image: Some(h) },
                wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            ),
            PixelData::Float(f) => {
                // SAFETY: u16s are plain bytes; little-endian, as the GPU wants (and every target is).
                let bytes = unsafe { std::slice::from_raw_parts(f.rgba.as_ptr() as *const u8, f.rgba.len() * 2) };
                self.queue.write_texture(
                    wgpu::TexelCopyTextureInfo { texture: &t.texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
                    bytes,
                    wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(8 * w), rows_per_image: Some(h) },
                    wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                )
            }
            PixelData::Yuv(planes) => {
                let view = t.texture.create_view(&Default::default());
                self.yuv.convert(&self.device, &self.queue, l.key, planes, &view, float);
            }
            PixelData::Gpu(g) => {
                let texture = t.texture.clone();
                self.draw_gpu_frame(l.key, g, &texture, float);
            }
            PixelData::Solid(s) => {
                let view = t.texture.create_view(&Default::default());
                self.solid.draw(&self.device, &self.queue, l.key, s, &view, l.pixel_size, &self.output_layout);
            }
        }
        if let Some(t) = self.layers.get_mut(&l.key) {
            t.content_id = l.content_id;
        }
    }

    /// The layer texture is filled from a frame on the graphics card: read
    /// straight from the shared texture when that works, otherwise (other
    /// platforms, zero-copy off or broken) copied back and converted as usual.
    fn draw_gpu_frame(&mut self, key: u64, g: &GpuFrame, texture: &wgpu::Texture, float: bool) {
        use reelforge_core::frame::zero_copy;
        let view = texture.create_view(&Default::default());
        if zero_copy::enabled() && zero_copy::failure().is_none() {
            match self.convert_shared(g, &view) {
                // This compositor can't read shared frames: just copy it back.
                None => {}
                Some(Ok(())) => {
                    // The first frame of each kind: compare with the usual copy.
                    if let Some(check) = g.check.as_ref().filter(|_| !self.gpu_checked.contains(&g.layout)) {
                        let shared = self.read_layer8(texture, [g.width, g.height], float);
                        self.yuv.convert(&self.device, &self.queue, key, check, &view, float);
                        let copied = self.read_layer8(texture, [g.width, g.height], float);
                        match (shared, copied) {
                            (Some(a), Some(b)) => {
                                let worst = a.iter().zip(&b).map(|(x, y)| x.abs_diff(*y)).max().unwrap_or(0);
                                if worst <= 2 {
                                    log::info!("zero-copy video: {:?} frames match the usual copy (largest difference {worst})", g.layout);
                                    self.gpu_checked.insert(g.layout);
                                    zero_copy::set_checked();
                                } else {
                                    // The layer already shows the copied frame.
                                    zero_copy::fail(format!("a {:?} frame read from the graphics card didn't match the usual copy (differences up to {worst} levels)", g.layout));
                                }
                            }
                            _ => zero_copy::fail("couldn't read a frame back to check it"),
                        }
                        return;
                    }
                    zero_copy::count_frame();
                    return;
                }
                Some(Err(e)) => zero_copy::fail(e),
            }
        }
        if let Some(y) = g.check.clone().or_else(|| g.cpu()) {
            self.yuv.convert(&self.device, &self.queue, key, &y, &view, float);
        }
    }

    /// Read `g` straight from its shared texture: `None` if this compositor
    /// can't (not Direct3D 12), else whether it worked.
    #[cfg(windows)]
    fn convert_shared(&mut self, g: &GpuFrame, view: &wgpu::TextureView) -> Option<Result<(), String>> {
        let i = self.interop.as_mut()?;
        Some(i.convert(&self.device, &self.queue, &self.yuv, g, view))
    }

    #[cfg(not(windows))]
    fn convert_shared(&mut self, _g: &GpuFrame, _view: &wgpu::TextureView) -> Option<Result<(), String>> {
        None
    }

    /// The graphics adapter (LUID) whose shared frames this compositor can
    /// read directly, if it can (Windows, Direct3D 12).
    pub fn zero_copy_adapter(&self) -> Option<u64> {
        #[cfg(windows)]
        {
            self.interop.as_ref().map(|i| i.adapter())
        }
        #[cfg(not(windows))]
        {
            None
        }
    }

    fn upload_mask(&mut self, key: u64, m: &MaskInput) {
        let [w, h] = m.size;
        if self.masks.get(&key).is_some_and(|t| t.size == m.size && t.content_id == m.content_id) {
            return;
        }
        if self.masks.get(&key).is_none_or(|t| t.size != m.size) {
            let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("mask"),
                size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::R8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            });
            let view = texture.create_view(&Default::default());
            let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("mask"),
                layout: &self.texture_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                ],
            });
            self.masks.insert(key, LayerTex { texture, bind_group, size: m.size, content_id: u64::MAX, float: false });
        }
        let Some(t) = self.masks.get_mut(&key) else { return };
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture: &t.texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            m.pixels,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(w), rows_per_image: Some(h) },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        t.content_id = m.content_id;
    }

    fn upload_table(&mut self, key: (u64, usize), t: &reelforge_core::lut::Table) {
        if self.tables.get(&key).is_some_and(|x| x.content_id == t.id && x.size == [t.width, t.height]) {
            return;
        }
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("lookup table"),
            size: wgpu::Extent3d { width: t.width, height: t.height, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("lookup table"),
            layout: &self.texture_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.sampler) },
            ],
        });
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture: &texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            &t.rgba,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4 * t.width), rows_per_image: Some(t.height) },
            wgpu::Extent3d { width: t.width, height: t.height, depth_or_array_layers: 1 },
        );
        self.tables.insert(key, LayerTex { texture, bind_group, size: [t.width, t.height], content_id: t.id, float: false });
    }

    fn ensure_fx_targets(&mut self, key: u64, size: [u32; 2], count: usize) {
        if self.fx_targets.get(&key).is_some_and(|t| t.size == size && t.views.len() >= count) {
            return;
        }
        let make = || {
            let tex = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("effect scratch"),
                size: wgpu::Extent3d { width: size[0].max(1), height: size[1].max(1), depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: self.formats.inter,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            let view = tex.create_view(&Default::default());
            let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("effect scratch"),
                layout: &self.texture_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                ],
            });
            (view, bind)
        };
        let (views, binds) = (0..count.max(3)).map(|_| make()).unzip();
        self.fx_targets.insert(key, FxTargets { size, views, binds });
    }

    /// Composite plain `layers` (bottom first). See [`Self::render_items`].
    pub fn render(&mut self, frame_size: [u32; 2], layers: &[LayerInput]) {
        let items: Vec<DrawItem> = layers.iter().map(|l| DrawItem::Layer(l.clone())).collect();
        self.render_items(frame_size, &items);
    }

    /// Composite `items` (bottom first) into a `frame_size` frame and submit the GPU work.
    /// For a water surface, liquid or fluid: the step its simulation would
    /// start from to reach `job`'s (so a caller can feed it the frames in
    /// between first).
    pub fn simulation_resume(&self, job: &reelforge_core::fluid::FluidJob) -> Option<usize> {
        match job.effect.kind {
            EffectKind::Ocean | EffectKind::WaterSurface | EffectKind::Liquid => Some(self.water.resume_step(job)),
            EffectKind::Liquid3D => Some(self.liquid3d.resume_step(job)),
            EffectKind::Fluid => Some(self.fluid.resume_step(job)),
            _ => None,
        }
    }

    /// The scene stepped `job`'s simulation on from step `from` through the
    /// frames in between: every one (`exact`), or fewer spread over a long
    /// jump, leaving it approximate from `from` on until filled in (it's not
    /// kept on disk meanwhile). See [`Self::simulation_refine_from`].
    pub fn simulation_caught_up(&mut self, job: &reelforge_core::fluid::FluidJob, from: usize, exact: bool) {
        match job.effect.kind {
            EffectKind::Ocean | EffectKind::WaterSurface | EffectKind::Liquid => self.water.caught_up(job, from, exact),
            EffectKind::Liquid3D => self.liquid3d.caught_up(job, from, exact),
            EffectKind::Fluid => self.fluid.caught_up(job, from, exact),
            _ => {}
        }
    }

    /// For an approximate simulation: a copy of it, under `shadow`, from its
    /// last exact checkpoint, and the step that is. Stepping the copy through
    /// the frames after, in order ([`Self::step_simulations`] with `shadow`),
    /// then [`Self::simulation_adopt`] makes it exact; the simulation shown
    /// isn't touched meanwhile.
    pub fn simulation_fork(&mut self, job: &reelforge_core::fluid::FluidJob, shadow: u64) -> Option<usize> {
        match job.effect.kind {
            EffectKind::Ocean | EffectKind::WaterSurface | EffectKind::Liquid => self.water.fork(job, shadow),
            EffectKind::Liquid3D => self.liquid3d.fork(job, shadow),
            EffectKind::Fluid => self.fluid.fork(job, shadow),
            _ => None,
        }
    }

    /// The filled-in copy takes the simulation's place. See [`Self::simulation_fork`].
    pub fn simulation_adopt(&mut self, kind: EffectKind, key: u64, shadow: u64) {
        match kind {
            EffectKind::Ocean | EffectKind::WaterSurface | EffectKind::Liquid => self.water.adopt(key, shadow),
            EffectKind::Liquid3D => self.liquid3d.adopt(key, shadow),
            EffectKind::Fluid => self.fluid.adopt(key, shadow),
            _ => {}
        }
    }

    /// Let go of a copy that won't be adopted.
    pub fn simulation_discard(&mut self, kind: EffectKind, shadow: u64) {
        match kind {
            EffectKind::Ocean | EffectKind::WaterSurface | EffectKind::Liquid => self.water.discard(shadow),
            EffectKind::Liquid3D => self.liquid3d.discard(shadow),
            EffectKind::Fluid => self.fluid.discard(shadow),
            _ => {}
        }
    }

    /// The picture a simulation flows round: its own layer's (`own`), or the
    /// other layer its job names (none there: nothing).
    fn obstacle_view(&self, own: u64, job: &reelforge_core::fluid::FluidJob) -> Option<wgpu::TextureView> {
        let key = match (job.obstacle_from, &job.source) {
            (0, _) => own,
            (_, Some(s)) => s.key,
            (_, None) => return None,
        };
        self.layers.get(&key).filter(|t| !t.float).map(|t| t.texture.create_view(&Default::default()))
    }

    /// Step the simulation `key` of `layer`'s effects (water, liquid, fluid)
    /// on to where it says, as `shadow` (a copy: see
    /// [`Self::simulation_fork`]), with the layer's picture uploaded for
    /// what's solid, drawing nothing: the picture shown stays as it is.
    pub fn step_simulations(&mut self, frame_size: [u32; 2], layer: &LayerInput, obstacle: Option<&LayerInput>, key: u64, shadow: u64) {
        let Some((kind, job)) = layer.effects.iter().find_map(|e| e.fluid.as_ref().filter(|j| j.key == key).map(|j| (e.kind, j))) else { return };
        let mut job = reelforge_core::fluid::FluidJob::clone(job);
        job.key = shadow;
        if let Some(o) = obstacle.filter(|o| !o.effects.is_empty() && job.source.is_some_and(|s| s.key == o.key)) {
            // It flows round what the other layer's effects make of it: those run
            // first, as when the frame is drawn (the shadow simulation in its place).
            let mut fluid = layer.clone();
            for e in &mut fluid.effects {
                if e.fluid.as_ref().is_some_and(|j| j.key == key) {
                    e.fluid = Some(std::sync::Arc::new(job.clone()));
                }
            }
            self.run_effects(frame_size, &[DrawItem::Layer(o.clone()), DrawItem::Layer(fluid)]);
            return;
        }
        self.upload(layer);
        if let Some(o) = obstacle {
            self.upload(o);
        }
        let view = self.obstacle_view(layer.key, &job);
        let _ = match kind {
            EffectKind::Ocean | EffectKind::WaterSurface | EffectKind::Liquid => self.water.step_to(&job, view.as_ref()),
            EffectKind::Liquid3D => self.liquid3d.step_to(&job, view.as_ref()),
            _ => self.fluid.step_to(&job, view.as_ref()),
        };
    }

    pub fn render_items(&mut self, frame_size: [u32; 2], items: &[DrawItem]) {
        self.render_items_inner(frame_size, items, false);
    }

    /// Run `items`' effects (stepping their simulations) without drawing the frame: the
    /// picture shown stays as it is.
    fn run_effects(&mut self, frame_size: [u32; 2], items: &[DrawItem]) {
        self.render_items_inner(frame_size, items, true);
    }

    fn render_items_inner(&mut self, frame_size: [u32; 2], items: &[DrawItem], dry: bool) {
        // The shadow quality changed: a new atlas, bound in its place.
        let tile = SHADOW_TILE.load(std::sync::atomic::Ordering::Relaxed);
        if tile != self.shadow_tile {
            self.shadow_tile = tile;
            self.shadow_atlas_view = atlas_texture(&self.device, tile);
            self.uniform_bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("layer uniforms"),
                layout: &self.uniform_layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding { buffer: &self.uniforms, offset: 0, size: wgpu::BufferSize::new(UNIFORM_SIZE) }) },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&self.shadow_atlas_view) },
                ],
            });
        }
        // The picture's pixels: the frame, a fraction of it (playback quality)
        // or more (a composition drawn finer, as it's scaled up). Layers are
        // placed in frame units, so only the targets change.
        let k = if self.output_scale.is_finite() && self.output_scale > 0.0 { self.output_scale.min(8.0) } else { 1.0 };
        let target = frame_size.map(|v| ((v as f32 * k).round() as u32).clamp(2, 8192));
        self.ensure_targets(target);
        let frame = [frame_size[0] as f32, frame_size[1] as f32];
        let target_px = [target[0] as f32, target[1] as f32];
        let all_layers = || {
            items.iter().flat_map(|item| match item {
                DrawItem::Layer(l) => vec![l],
                DrawItem::Dissolve { from, to, .. } => vec![from, to],
                DrawItem::Matted { matte, layer, .. } => vec![matte, layer],
            })
        };

        for l in all_layers() {
            if l.pixels.matches(l.pixel_size) {
                self.upload(l);
            }
            if let Some(m) = l.mask.filter(|m| m.pixels.len() as u64 == m.size[0] as u64 * m.size[1] as u64 && m.size[0] > 0) {
                self.upload_mask(l.key, &m);
                if m.keep.len() == m.pixels.len() {
                    self.upload_mask(l.key ^ KEEP_MASK, &MaskInput { pixels: m.keep, ..m });
                }
            }
        }

        // ---- Effect chains: plan the passes for every layer that has effects ----
        #[derive(Clone, Copy)]
        enum Src {
            Layer,
            /// The frame composited so far (an adjustment layer).
            Below,
            Scratch(usize),
        }
        /// The second texture a two-input pass reads.
        #[derive(Clone, Copy)]
        enum Second {
            None,
            Mask,
            /// Where keys keep the layer (masks marked as keys' garbage mattes).
            KeepMask,
            Table(usize),
            Scratch(usize),
            /// A clear texture (an optional second texture that isn't there).
            Blank,
        }
        struct FxPass {
            key: u64,
            entry: &'static str,
            src: Src,
            second: Second,
            dst: usize,
            uniform: [f32; FX_FLOATS],
            /// A plugin pass: the plugin's revision and which pass.
            plugin: Option<(u64, usize)>,
            /// Particles to draw over the result (an index into `particle_sets`).
            particles: Option<usize>,
        }
        let mut fx_passes: Vec<FxPass> = Vec::new();
        // Which scratch target holds each processed layer's result.
        let mut processed: HashMap<u64, usize> = HashMap::new();
        // Each processed layer's map from its effect target's uv to its own.
        let mut margins: HashMap<u64, [f32; 4]> = HashMap::new();
        // Every emitter's particles this frame, and its look.
        let mut particle_sets: Vec<particles::Set> = Vec::new();
        // Layers whose effects run this frame, and fluids flowing round one of them
        // (another layer's shape after its effects): stepped once those have run.
        let with_effects: std::collections::HashSet<u64> = all_layers().filter(|l| !l.effects.is_empty()).map(|l| l.key).collect();
        let mut deferred: Vec<(u64, usize, reelforge_core::fluid::FluidJob)> = Vec::new();
        for l in all_layers() {
            let masked = l.mask.is_some() && self.masks.contains_key(&l.key);
            // Keys keep the layer inside its garbage mattes.
            let keep = l.mask.is_some_and(|m| !m.keep.is_empty()) && self.masks.contains_key(&(l.key ^ KEEP_MASK));
            let below = matches!(l.pixels, PixelData::Below);
            if (l.effects.is_empty() && !masked) || !(below || self.layers.contains_key(&l.key)) || processed.contains_key(&l.key) {
                continue;
            }
            let cost = |e: &EffectValues| match e.kind {
                EffectKind::Blur => 2,
                // At most: the key, two choke and two softness passes, putting it
                // together, and two for a garbage matte.
                EffectKind::ChromaKey | EffectKind::LumaKey | EffectKind::DifferenceKey => 8,
                // Spill, two passes each way for specks and for holes, putting it together.
                EffectKind::KeyCleaner => 10,
                // Straight colour, choke and softness each way, putting it together.
                EffectKind::MatteChoker => 6,
                // Straight colour; smooth (blur each way, steepen), shift and feather
                // each way; colour from within each way; putting it together.
                EffectKind::RefineMatte => 11,
                EffectKind::WireRemoval => 1,
                EffectKind::Glow | EffectKind::DropShadow => 4,
                EffectKind::Plugin => e.plugin.as_ref().map_or(0, |p| p.passes.len()),
                _ => 1,
            };
            let refine = l.mask.and_then(|m| m.refine).filter(|r| r[0] > 0.0 && masked);
            let needed = 1 + masked as usize + 8 * refine.is_some() as usize + l.effects.iter().map(cost).sum::<usize>();
            if fx_passes.len() + needed > MAX_FX_PASSES {
                log::warn!("too many effect passes; skipping effects on some layers");
                break;
            }
            // The resolution its effects and mask run at: its own pixels, except
            // for a tiny picture stretched over the layer (a solid colour's 1 × 1),
            // which gets the layer's size so a mask's edge or a blur has room.
            let fx_px = fx_resolution(l.pixel_size, l.layer_size);
            // Room round the layer for what glows and shadows spill past its edges.
            let margin = if below || l.mesh.is_some() { [0, 0] } else { effect_margin(&l.effects, fx_px, l.layer_size) };
            let padded = [fx_px[0] + 2 * margin[0], fx_px[1] + 2 * margin[1]];
            self.ensure_fx_targets(l.key, padded, if refine.is_some() { 4 } else { 3 });
            let map = {
                let (pw, ph) = (fx_px[0].max(1) as f32, fx_px[1].max(1) as f32);
                [padded[0] as f32 / pw, padded[1] as f32 / ph, -(margin[0] as f32) / pw, -(margin[1] as f32) / ph]
            };
            margins.insert(l.key, map);
            let first_pass = fx_passes.len();
            for (i, e) in l.effects.iter().enumerate() {
                if let Some(t) = &e.table {
                    self.upload_table((l.key, i), t);
                }
                if let Some(t) = &e.table2 {
                    self.upload_table((l.key, i | TABLE2), t);
                }
                if let Some(job) = &e.fluid {
                    if e.kind == EffectKind::Fluid && job.obstacle_from != 0 && job.source.is_some_and(|s| s.key != l.key && with_effects.contains(&s.key)) {
                        deferred.push((l.key, i, reelforge_core::fluid::FluidJob::clone(job)));
                        continue;
                    }
                    // Stepped on to now; its picture goes in as this effect's table.
                    let layer = self.obstacle_view(l.key, job);
                    let tex = match e.kind {
                        EffectKind::Ocean | EffectKind::WaterSurface | EffectKind::Liquid => self.water.step_to(job, layer.as_ref()),
                        EffectKind::Liquid3D => self.liquid3d.step_to(job, layer.as_ref()),
                        _ => self.fluid.step_to(job, layer.as_ref()),
                    };
                    if let Some(tex) = tex {
                        let view = tex.create_view(&Default::default());
                        let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                            label: Some("fluid picture"),
                            layout: &self.texture_layout,
                            entries: &[
                                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
                                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                            ],
                        });
                        let size = [tex.width(), tex.height()];
                        self.tables.insert((l.key, i), LayerTex { texture: tex, bind_group, size, content_id: job.key ^ job.step as u64, float: false });
                    }
                }
            }
            let [pw, ph] = [fx_px[0].max(1) as f32, fx_px[1].max(1) as f32];
            let aspect = l.layer_size[0] / l.layer_size[1].max(1.0);
            // Frame pixels at 100% scale → this layer's texels.
            let (kx, ky) = (pw / l.layer_size[0].max(1.0), ph / l.layer_size[1].max(1.0));
            let uniform = |v: &[f32; reelforge_core::effects::MAX_PARAMS], dir: [f32; 2]| {
                let mut u = [0.0f32; FX_FLOATS];
                u[..8].copy_from_slice(&v[..8]);
                u[16..24].copy_from_slice(&v[8..16]);
                u[8..12].copy_from_slice(&[1.0 / pw, 1.0 / ph, dir[0], dir[1]]);
                u[12] = aspect;
                u
            };
            let key = l.key;
            // The frame so far is premultiplied already: copied as it is.
            // A float layer is premultiplied already, so it's copied rather than imported.
            let float = self.layers.get(&l.key).is_some_and(|t| t.float);
            let (entry, src) = if below { (if self.formats.gamma { "fs_from_gamma" } else { "fs_copy" }, Src::Below) } else if float { ("fs_import_float", Src::Layer) } else { ("fs_import", Src::Layer) };
            fx_passes.push(FxPass { key, entry, src, second: Second::None, dst: 0, uniform: [0.0; FX_FLOATS], plugin: None, particles: None });
            let mut cur = 0usize;
            // Plugin passes pushed for this layer: (index in fx_passes, revision, pass).
            let mut plugin_marks: Vec<(usize, u64, usize)> = Vec::new();
            let mut particle_marks: Vec<(usize, usize)> = Vec::new();
            let mut push = |entry, src: usize, second, dst: usize, u| -> usize {
                fx_passes.push(FxPass { key, entry, src: Src::Scratch(src), second, dst, uniform: u, plugin: None, particles: None });
                fx_passes.len() - 1
            };
            // The two targets that aren't `cur`.
            let others = |cur: usize| ((cur + 1) % 3, (cur + 2) % 3);
            // A separable blur of `from` into `into`, using `tmp`.
            let blur = |push: &mut dyn FnMut(&'static str, usize, Second, usize, [f32; FX_FLOATS]) -> usize, radius: f32, from: usize, tmp: usize, into: usize| {
                let mut vx = [0.0; reelforge_core::effects::MAX_PARAMS];
                vx[0] = radius * 0.5 * kx;
                let mut vy = vx;
                vy[0] = radius * 0.5 * ky;
                push("fs_blur", from, Second::None, tmp, uniform(&vx, [1.0, 0.0]));
                push("fs_blur", tmp, Second::None, into, uniform(&vy, [0.0, 1.0]));
            };
            if let Some(r) = refine {
                // Matting in a band round the mask's edge (see mask.wgsl): the
                // layer with where it's surely in or out (3); the colours
                // surely in round each pixel (1) and surely out (2); the
                // line between them (0); the layer's alpha from where each
                // pixel sits on it (1).
                let mut v = [0.0; reelforge_core::effects::MAX_PARAMS];
                v[0] = r[0] / l.layer_size[0].max(1.0);
                v[1] = r[0] / l.layer_size[1].max(1.0);
                push("fs_refine_trimap", 0, Second::Mask, 3, uniform(&v, [0.0; 2]));
                push("fs_refine_in", 3, Second::Scratch(3), 1, [0.0; FX_FLOATS]);
                blur(&mut push, r[0] * 2.0, 1, 2, 1);
                push("fs_refine_out", 3, Second::Scratch(3), 2, [0.0; FX_FLOATS]);
                blur(&mut push, r[0] * 2.0, 2, 0, 2);
                push("fs_refine_line", 1, Second::Scratch(2), 0, [0.0; FX_FLOATS]);
                v[0] = r[1];
                v[1] = r[2];
                v[2] = r[3];
                push("fs_refine_apply", 3, Second::Scratch(0), 1, uniform(&v, [0.0; 2]));
                cur = 1;
            } else if masked {
                let (o, _) = others(cur);
                push("fs_mask", cur, Second::Mask, o, [0.0; FX_FLOATS]);
                cur = o;
            }
            for (i, e) in l.effects.iter().enumerate() {
                let (o1, o2) = others(cur);
                match e.kind {
                    EffectKind::Blur => {
                        blur(&mut push, e.values[0], cur, o1, o2);
                        cur = o2;
                    }
                    EffectKind::Glow => {
                        // Bright parts → blur → added back onto the untouched layer.
                        push("fs_bright", cur, Second::None, o1, uniform(&e.values, [0.0; 2]));
                        blur(&mut push, e.values[1], o1, o2, o1);
                        push("fs_glow_add", cur, Second::Scratch(o1), o2, uniform(&e.values, [0.0; 2]));
                        cur = o2;
                    }
                    EffectKind::NeonEdges => {
                        // The tube along the edges → blurred into the glow → the layer
                        // (as much as asked), the tube and its glow added up.
                        let mut u = uniform(&e.values, [i as f32 + 1.0, 0.0]);
                        u[13] = e.time;
                        u[14] = l.layer_size[0];
                        u[15] = l.layer_size[1];
                        push("fs_neon_edge", cur, Second::None, o1, u);
                        blur(&mut push, e.values[reelforge_core::effects::neon::GLOW_RADIUS], o1, o2, o1);
                        push("fs_neon_mix", cur, Second::Scratch(o1), o2, u);
                        cur = o2;
                    }
                    EffectKind::DropShadow => {
                        // The shape, moved and darkened → blurred → put under the layer.
                        let (d, a) = (e.values[0], e.values[1].to_radians());
                        let mut v = e.values;
                        (v[0], v[1], v[3]) = (-a.cos() * d * kx, a.sin() * d * ky, e.values[3]);
                        push("fs_shadow", cur, Second::None, o1, uniform(&v, [0.0; 2]));
                        blur(&mut push, e.values[2], o1, o2, o1);
                        push("fs_under", cur, Second::Scratch(o1), o2, uniform(&e.values, [0.0; 2]));
                        cur = o2;
                    }
                    EffectKind::Sharpen => {
                        let mut v = e.values;
                        v[1] = e.values[1] * kx.max(ky);
                        push("fs_sharpen", cur, Second::None, o1, uniform(&v, [0.0; 2]));
                        cur = o1;
                    }
                    EffectKind::Curves | EffectKind::Lut => {
                        if !self.tables.contains_key(&(key, i)) {
                            continue;
                        }
                        let mut v = e.values;
                        v[1] = e.table.as_ref().map_or(0.0, |t| t.cube_size as f32);
                        let entry = if e.kind == EffectKind::Curves { "fs_curves" } else { "fs_lut" };
                        push(entry, cur, Second::Table(i), o1, uniform(&v, [0.0; 2]));
                        cur = o1;
                    }
                    EffectKind::BoxBlur => {
                        use reelforge_core::effects::box_blur as bb;
                        let r = e.values[bb::RADIUS].max(0.0);
                        let dims = e.values[bb::DIMENSIONS].round() as i32;
                        let mut s = cur;
                        for _ in 0..(e.values[bb::ITERATIONS].round().clamp(1.0, 5.0) as usize) {
                            for (dir, k, on) in [([1.0, 0.0], kx, dims != 2), ([0.0, 1.0], ky, dims != 1)] {
                                if !on {
                                    continue;
                                }
                                let mut v = e.values;
                                v[0] = r * k;
                                v[1] = e.values[bb::REPEAT];
                                let d = if s == o1 { o2 } else { o1 };
                                push("fs_box_blur", s, Second::None, d, uniform(&v, dir));
                                s = d;
                            }
                        }
                        cur = s;
                    }
                    EffectKind::ChannelBlur => {
                        use reelforge_core::effects::channel_blur as cb;
                        let dims = e.values[cb::DIMENSIONS].round() as i32;
                        let mut s = cur;
                        for (dir, k, on) in [([1.0, 0.0], kx, dims != 2), ([0.0, 1.0], ky, dims != 1)] {
                            if !on {
                                continue;
                            }
                            let mut v = e.values;
                            for c in 0..4 {
                                v[c] = e.values[c].max(0.0) * 0.5 * k;
                            }
                            v[4] = e.values[cb::REPEAT];
                            let d = if s == o1 { o2 } else { o1 };
                            push("fs_channel_blur", s, Second::None, d, uniform(&v, dir));
                            s = d;
                        }
                        cur = s;
                    }
                    EffectKind::CompoundBlur => {
                        blur(&mut push, e.values[reelforge_core::effects::compound_blur::MAX].max(0.0), cur, o1, o2);
                        let v = e.values;
                        let mut u = uniform(&v, [0.0, 0.0]);
                        u[13] = e.time;
                        u[14] = l.layer_size[0];
                        u[15] = l.layer_size[1];
                        u[28..32].copy_from_slice(&v[16..20]);
                        push("fs_compound_mix", cur, Second::Scratch(o2), o1, u);
                        cur = o1;
                    }
                    EffectKind::CrossBlur => {
                        use reelforge_core::effects::cross_blur as xb;
                        let mut v = e.values;
                        v[0] = e.values[xb::ACROSS].max(0.0) * 0.5 * kx;
                        push("fs_blur", cur, Second::None, o1, uniform(&v, [1.0, 0.0]));
                        v[0] = e.values[xb::DOWN].max(0.0) * 0.5 * ky;
                        push("fs_blur", cur, Second::None, o2, uniform(&v, [0.0, 1.0]));
                        push("fs_cross_mix", o1, Second::Scratch(o2), cur, uniform(&e.values, [0.0, 0.0]));
                    }
                    EffectKind::Minimax => {
                        use reelforge_core::effects::minimax as mm;
                        let op = e.values[mm::OPERATION].round() as i32;
                        let dims = e.values[mm::DIRECTION].round() as i32;
                        let ops: &[f32] = match op {
                            1 => &[1.0],
                            2 => &[0.0, 1.0],
                            3 => &[1.0, 0.0],
                            _ => &[0.0],
                        };
                        let mut s = cur;
                        for &which in ops {
                            for (dir, k, on) in [([1.0, 0.0], kx, dims != 2), ([0.0, 1.0], ky, dims != 1)] {
                                if !on {
                                    continue;
                                }
                                let mut v = e.values;
                                v[0] = which;
                                v[1] = e.values[mm::RADIUS].max(0.0) * k;
                                v[2] = e.values[mm::CHANNEL];
                                let d = if s == o1 { o2 } else { o1 };
                                push("fs_minimax", s, Second::None, d, uniform(&v, dir));
                                s = d;
                            }
                        }
                        cur = s;
                    }
                    EffectKind::AutoLevels => {
                        let mut v = e.values;
                        v[2] = 0.0;
                        v[6] = e.values[2];
                        v[7] = 0.0;
                        push("fs_stats", cur, Second::None, o1, uniform(&v, [0.0, 0.0]));
                        push("fs_auto_apply", cur, Second::Scratch(o1), o2, uniform(&v, [0.0, 0.0]));
                        cur = o2;
                    }
                    EffectKind::AutoContrast => {
                        let mut v = e.values;
                        v[2] = 0.0;
                        v[6] = e.values[2];
                        v[7] = 1.0;
                        push("fs_stats", cur, Second::None, o1, uniform(&v, [0.0, 0.0]));
                        push("fs_auto_apply", cur, Second::Scratch(o1), o2, uniform(&v, [0.0, 0.0]));
                        cur = o2;
                    }
                    EffectKind::AutoColor => {
                        let mut v = e.values;
                        v[2] = 0.0;
                        v[6] = e.values[2];
                        v[7] = 2.0;
                        push("fs_stats", cur, Second::None, o1, uniform(&v, [0.0, 0.0]));
                        push("fs_auto_apply", cur, Second::Scratch(o1), o2, uniform(&v, [0.0, 0.0]));
                        cur = o2;
                    }
                    EffectKind::Equalize => {
                        let mut v = e.values;
                        v[2] = 1.0;
                        push("fs_stats", cur, Second::None, o1, uniform(&v, [0.0, 0.0]));
                        v[0] = e.values[0];
                        v[6] = 100.0 - e.values[1];
                        v[7] = 3.0;
                        push("fs_auto_apply", cur, Second::Scratch(o1), o2, uniform(&v, [0.0, 0.0]));
                        cur = o2;
                    }
                    EffectKind::RemoveGrain => {
                        use reelforge_core::effects::remove_grain as rg;
                        let passes = e.values[rg::PASSES].round().clamp(1.0, 4.0) as usize;
                        let mut s = cur;
                        for p in 0..passes {
                            let mut v = e.values;
                            // (The last pass sharpens and shows; the earlier ones only smooth.)
                            if p + 1 < passes {
                                v[rg::SHARPEN] = 0.0;
                                v[rg::VIEW] = 0.0;
                            }
                            let d = if s == o1 { o2 } else { o1 };
                            let mut u = uniform(&v, [0.0, 0.0]);
                            u[13] = e.time;
                            u[14] = l.layer_size[0];
                            u[15] = l.layer_size[1];
                            push("fs_remove_grain", s, Second::None, d, u);
                            s = d;
                        }
                        cur = s;
                    }
                    EffectKind::SimpleChoker => {
                        use reelforge_core::effects::simple_choker as sc;
                        let choke = e.values[sc::CHOKE];
                        let mut s = cur;
                        if choke != 0.0 {
                            for (dir, k) in [([1.0, 0.0], kx), ([0.0, 1.0], ky)] {
                                let mut v = e.values;
                                v[0] = if choke > 0.0 { 0.0 } else { 1.0 };
                                v[1] = choke.abs() * k;
                                v[2] = 2.0;
                                let d = if s == o1 { o2 } else { o1 };
                                push("fs_minimax", s, Second::None, d, uniform(&v, dir));
                                s = d;
                            }
                        }
                        if e.values[sc::VIEW] >= 0.5 {
                            let d = if s == o1 { o2 } else { o1 };
                            push("fs_matte_view", s, Second::None, d, uniform(&e.values, [0.0, 0.0]));
                            s = d;
                        }
                        cur = s;
                    }
                    EffectKind::PosterizeTime | EffectKind::TimeDifference => {}
                    EffectKind::ColorCorrect => {
                        push("fs_color", cur, Second::None, o1, uniform(&e.values, [0.0; 2]));
                        cur = o1;
                    }
                    EffectKind::ChromaKey | EffectKind::LumaKey | EffectKind::DifferenceKey => {
                        use reelforge_core::effects::key;
                        let Some(base) = key::matte_base(e.kind) else { continue };
                        // The shaders take the matte settings at 4–8 and the "straight" flag at 9.
                        let mut v = e.values;
                        v[base..].fill(0.0);
                        v[key::CLIP_BLACK..=key::VIEW].copy_from_slice(&e.values[base..base + 5]);
                        let (entry, second) = match e.kind {
                            EffectKind::ChromaKey => ("fs_key", Second::None),
                            EffectKind::LumaKey => ("fs_luma_key", Second::None),
                            _ => {
                                if !self.tables.contains_key(&(key, i)) {
                                    continue;
                                }
                                ("fs_diff_key", Second::Table(i))
                            }
                        };
                        let (choke, soft) = (v[key::CHOKE], v[key::EDGE_SOFTNESS]);
                        // The keyed layer ends up in `keyed`, with `spare` free; `cur`
                        // (the layer before the key) is left as it is.
                        let (keyed, spare) = if choke.abs() < 0.01 && soft < 0.01 {
                            push(entry, cur, second, o1, uniform(&v, [0.0; 2]));
                            (o1, o2)
                        } else {
                            // Straight colour with the matte in alpha, then the matte
                            // choked and softened (each across, then down, back into
                            // `o1`), then put together into `o2`.
                            let mut first = v;
                            first[9] = 1.0;
                            push(entry, cur, second, o1, uniform(&first, [0.0; 2]));
                            let mut pass = |entry: &'static str, reach: f32, sign: f32| {
                                let mut vx = [0.0; reelforge_core::effects::MAX_PARAMS];
                                (vx[0], vx[1]) = (reach * kx, sign);
                                let mut vy = vx;
                                vy[0] = reach * ky;
                                push(entry, o1, Second::None, o2, uniform(&vx, [1.0, 0.0]));
                                push(entry, o2, Second::None, o1, uniform(&vy, [0.0, 1.0]));
                            };
                            if choke.abs() >= 0.01 {
                                pass("fs_key_choke", choke.abs(), choke.signum());
                            }
                            if soft >= 0.01 {
                                pass("fs_key_soft", soft * 0.5, 0.0);
                            }
                            push("fs_key_out", o1, Second::None, o2, uniform(&v, [0.0; 2]));
                            (o2, o1)
                        };
                        if keep {
                            // Garbage matte: inside it the layer as it was before the
                            // key, over the keyed one.
                            push("fs_mask", cur, Second::KeepMask, spare, [0.0; FX_FLOATS]);
                            push("fs_under", spare, Second::Scratch(keyed), cur, [0.0; FX_FLOATS]);
                        } else {
                            cur = keyed;
                        }
                    }
                    EffectKind::KeyCleaner => {
                        use reelforge_core::effects::cleaner;
                        let v = e.values;
                        let (holes, specks) = (v[cleaner::HOLES], v[cleaner::SPECKS]);
                        if holes < 0.01 && specks < 0.01 {
                            push("fs_key_clean", cur, Second::None, o1, uniform(&v, [0.0; 2]));
                            cur = o1;
                            continue;
                        }
                        // Straight colour with the matte in alpha into `o1`, then specks
                        // removed (shrink, then grow back) and holes filled (grow, then
                        // shrink back), each across and down through `o2`.
                        let mut first = v;
                        first[6] = 1.0;
                        push("fs_key_clean", cur, Second::None, o1, uniform(&first, [0.0; 2]));
                        let mut pass = |reach: f32, sign: f32| {
                            let mut vx = [0.0; reelforge_core::effects::MAX_PARAMS];
                            (vx[0], vx[1]) = (reach * kx, sign);
                            let mut vy = vx;
                            vy[0] = reach * ky;
                            push("fs_key_choke", o1, Second::None, o2, uniform(&vx, [1.0, 0.0]));
                            push("fs_key_choke", o2, Second::None, o1, uniform(&vy, [0.0, 1.0]));
                        };
                        if specks >= 0.01 {
                            pass(specks, 1.0);
                            pass(specks, -1.0);
                        }
                        if holes >= 0.01 {
                            pass(holes, -1.0);
                            pass(holes, 1.0);
                        }
                        let mut out = [0.0; reelforge_core::effects::MAX_PARAMS];
                        out[8] = v[cleaner::VIEW];
                        push("fs_key_out", o1, Second::None, o2, uniform(&out, [0.0; 2]));
                        cur = o2;
                    }
                    EffectKind::MatteChoker => {
                        use reelforge_core::effects::choker;
                        let v = e.values;
                        let (choke, soft) = (v[choker::CHOKE], v[choker::SOFTNESS]);
                        // Straight colour into `o1`, choked and softened (across and down
                        // through `o2`), then the contrast and put together into `o2`.
                        push("fs_straight", cur, Second::None, o1, [0.0; FX_FLOATS]);
                        let mut pass = |entry: &'static str, reach: f32, sign: f32| {
                            let mut vx = [0.0; reelforge_core::effects::MAX_PARAMS];
                            (vx[0], vx[1]) = (reach * kx, sign);
                            let mut vy = vx;
                            vy[0] = reach * ky;
                            push(entry, o1, Second::None, o2, uniform(&vx, [1.0, 0.0]));
                            push(entry, o2, Second::None, o1, uniform(&vy, [0.0, 1.0]));
                        };
                        if choke.abs() >= 0.01 {
                            pass("fs_key_choke", choke.abs(), choke.signum());
                        }
                        if soft >= 0.01 {
                            pass("fs_key_soft", soft * 0.5, 0.0);
                        }
                        let mut out = [0.0; reelforge_core::effects::MAX_PARAMS];
                        out[..3].copy_from_slice(&[v[choker::CONTRAST], v[choker::MIDPOINT], v[choker::VIEW]]);
                        push("fs_matte_out", o1, Second::None, o2, uniform(&out, [0.0; 2]));
                        cur = o2;
                    }
                    EffectKind::RefineMatte => {
                        use reelforge_core::effects::refine;
                        let v = e.values;
                        let (smooth, shift, feather) = (v[refine::SMOOTH], v[refine::SHIFT], v[refine::FEATHER]);
                        // The matte, in straight colour, moves between `o1` and `o2`
                        // (`a` holds it after each step); `cur` is free once read.
                        let (mut a, mut b) = (o1, o2);
                        push("fs_straight", cur, Second::None, a, [0.0; FX_FLOATS]);
                        macro_rules! both {
                            ($entry:expr, $reach:expr, $sign:expr) => {{
                                let mut vx = [0.0; reelforge_core::effects::MAX_PARAMS];
                                (vx[0], vx[1]) = ($reach * kx, $sign);
                                let mut vy = vx;
                                vy[0] = $reach * ky;
                                push($entry, a, Second::None, b, uniform(&vx, [1.0, 0.0]));
                                push($entry, b, Second::None, a, uniform(&vy, [0.0, 1.0]));
                            }};
                        }
                        if smooth >= 0.01 {
                            both!("fs_key_soft", smooth * 0.5, 0.0);
                            // Steep enough to bring back an edge about a pixel wide.
                            let mut st = [0.0; reelforge_core::effects::MAX_PARAMS];
                            st[0] = (smooth * 1.2).max(2.0);
                            push("fs_matte_steep", a, Second::None, b, uniform(&st, [0.0; 2]));
                            std::mem::swap(&mut a, &mut b);
                        }
                        if shift.abs() >= 0.01 {
                            both!("fs_key_choke", shift.abs(), shift.signum());
                        }
                        if feather >= 0.01 {
                            both!("fs_key_soft", feather * 0.5, 0.0);
                        }
                        // Colour from within into `cur` (through `b`).
                        let mut dv = [0.0; reelforge_core::effects::MAX_PARAMS];
                        dv[0] = v[refine::RADIUS] * kx;
                        push("fs_decon_blur", a, Second::None, b, uniform(&dv, [1.0, 0.0]));
                        (dv[0], dv[1]) = (v[refine::RADIUS] * ky, 1.0);
                        push("fs_decon_blur", b, Second::None, cur, uniform(&dv, [0.0, 1.0]));
                        let mut out = [0.0; reelforge_core::effects::MAX_PARAMS];
                        (out[0], out[1]) = (v[refine::DECONTAMINATE], v[refine::VIEW]);
                        push("fs_decon_mix", a, Second::Scratch(cur), b, uniform(&out, [0.0; 2]));
                        cur = b;
                    }
                    EffectKind::WireRemoval => {
                        use reelforge_core::effects::wire;
                        let mut v = e.values;
                        // Frame pixels into this layer's texels.
                        let k = (kx + ky) * 0.5;
                        for i in [wire::WIDTH, wire::SOFTNESS, wire::GAP] {
                            v[i] *= k;
                        }
                        push("fs_wire", cur, Second::None, o1, uniform(&v, [0.0; 2]));
                        cur = o1;
                    }
                    EffectKind::Vignette => {
                        push("fs_vignette", cur, Second::None, o1, uniform(&e.values, [0.0; 2]));
                        cur = o1;
                    }
                    EffectKind::Levels => {
                        push("fs_levels", cur, Second::None, o1, uniform(&e.values, [0.0; 2]));
                        cur = o1;
                    }
                    EffectKind::ColorWheels => {
                        push("fs_wheels", cur, Second::None, o1, uniform(&e.values, [0.0; 2]));
                        cur = o1;
                    }
                    EffectKind::HslQualifier => {
                        // Its grade is params 16–19.
                        let mut u = uniform(&e.values, [0.0; 2]);
                        u[28..32].copy_from_slice(&e.values[16..20]);
                        push("fs_qualify", cur, Second::None, o1, u);
                        cur = o1;
                    }
                    EffectKind::HueCurves => {
                        if !self.tables.contains_key(&(key, i)) {
                            continue;
                        }
                        push("fs_hue_curves", cur, Second::Table(i), o1, uniform(&e.values, [0.0; 2]));
                        cur = o1;
                    }
                    // Camera and light settings are never drawn (they're filtered out earlier).
                    // (Echo is drawn as copies of the layer, by the scene.)
                    EffectKind::Camera | EffectKind::Light | EffectKind::Point | EffectKind::OpenFx | EffectKind::Echo | EffectKind::LightWrap | EffectKind::TimeDisplacement | EffectKind::HoldFrame => {}
                    EffectKind::EnergyBlade => {
                        // A seed per blade (its place in the stack) so two blades flicker apart.
                        let mut u = uniform(&e.values, [i as f32 + 1.0, 0.0]);
                        u[13] = e.time;
                        u[14] = l.layer_size[0];
                        u[15] = l.layer_size[1];
                        u[24..28].copy_from_slice(&e.prev.unwrap_or([e.values[0], e.values[1], e.values[2], e.values[3]]));
                        u[28..32].copy_from_slice(&e.values[16..20]);
                        push("fs_blade", cur, Second::None, o1, u);
                        cur = o1;
                    }
                    EffectKind::Particles => {
                        let Some(ps) = &e.particles else { continue };
                        // The layer as it is, then the particles drawn over it.
                        let mut u = uniform(&e.values, [0.0; 2]);
                        u[13] = e.time;
                        u[14] = l.layer_size[0];
                        u[15] = l.layer_size[1];
                        u[28..32].copy_from_slice(&e.values[16..20]);
                        u[FX_P5..FX_P5 + 4].copy_from_slice(&e.values[20..24]);
                        // Only the particles, for those born from a hidden layer's pixels.
                        let hidden = e.values[reelforge_core::particles::SHAPE].round() as u32 == reelforge_core::particles::SHAPE_PIXELS_ONLY;
                        let at = push(if hidden { "fs_clear" } else { "fs_copy" }, cur, Second::None, o1, u);
                        particle_marks.push((at, particle_sets.len()));
                        particle_sets.push(particles::Set::Particles(ps, e.values[reelforge_core::particles::LOOK]));
                        cur = o1;
                    }
                    EffectKind::Shatter => {
                        let Some(pieces) = &e.pieces else { continue };
                        let mut u = uniform(&e.values, [0.0; 2]);
                        u[13] = e.time;
                        u[14] = l.layer_size[0];
                        u[15] = l.layer_size[1];
                        if !pieces.broken {
                            continue; // whole still
                        }
                        // The layer gone, then its pieces drawn where they are.
                        let at = push("fs_clear", cur, Second::None, o1, u);
                        particle_marks.push((at, particle_sets.len()));
                        particle_sets.push(particles::Set::Pieces(pieces));
                        cur = o1;
                    }
                    EffectKind::Disintegrate => {
                        use reelforge_core::effects::disintegrate as d;
                        let Some(ps) = &e.particles else { continue };
                        let v = &e.values;
                        // The particles' settings where the particle shader reads them,
                        // and the sweep in the slots it doesn't use (see `fs_disintegrate`).
                        let mut pv = [0.0f32; reelforge_core::effects::MAX_PARAMS];
                        pv[0] = v[d::DIRECTION];
                        pv[1] = v[d::START];
                        pv[2] = reelforge_core::particles::SHAPE_PIXELS as f32;
                        pv[3] = v[d::DURATION];
                        pv[4] = v[d::ROUGH];
                        pv[5] = v[d::GLOW];
                        pv[6] = v[d::GLOW_WIDTH];
                        pv[7] = v[d::SEED];
                        pv[reelforge_core::particles::GRAVITY] = v[d::GRAVITY];
                        pv[reelforge_core::particles::TURBULENCE] = v[d::TURBULENCE];
                        pv[reelforge_core::particles::SIZE_START] = v[d::SIZE];
                        pv[reelforge_core::particles::SIZE_END] = v[d::SIZE] * 0.4;
                        pv[reelforge_core::particles::COLOR_END] = v[d::FADE_TO];
                        pv[reelforge_core::particles::OPACITY] = 100.0;
                        pv[reelforge_core::particles::LOOK] = v[d::LOOK];
                        pv[reelforge_core::particles::SEED] = v[d::SEED];
                        let mut u = uniform(&pv, [0.0; 2]);
                        u[13] = e.time;
                        u[14] = l.layer_size[0];
                        u[15] = l.layer_size[1];
                        u[28..32].copy_from_slice(&pv[16..20]);
                        let at = push("fs_disintegrate", cur, Second::None, o1, u);
                        particle_marks.push((at, particle_sets.len()));
                        particle_sets.push(particles::Set::Particles(ps, v[d::LOOK]));
                        cur = o1;
                    }
                    EffectKind::Fluid => {
                        if !self.tables.contains_key(&(key, i)) {
                            continue;
                        }
                        push("fs_fluid", cur, Second::Table(i), o1, uniform(&e.values, [0.0; 2]));
                        cur = o1;
                    }
                    EffectKind::Ocean | EffectKind::WaterSurface | EffectKind::Liquid | EffectKind::Liquid3D => {
                        // (an ocean has a simulation only with objects in it)
                        let second = if self.tables.contains_key(&(key, i)) {
                            Second::Table(i)
                        } else if e.kind == EffectKind::Ocean {
                            Second::Blank
                        } else {
                            continue;
                        };
                        // All 32 parameters, the time and the layer's size (see water.wgsl).
                        let mut u = uniform(&e.values, [0.0; 2]);
                        u[13] = e.time;
                        u[14] = l.layer_size[0];
                        u[15] = l.layer_size[1];
                        u[28..32].copy_from_slice(&e.values[16..20]);
                        u[FX_P5..FX_P5 + 4].copy_from_slice(&e.values[20..24]);
                        u[FX_P6..FX_P6 + 8].copy_from_slice(&e.values[24..32]);
                        // The simulation's grid (cells across, down).
                        if let Some(job) = &e.fluid {
                            u[24..26].copy_from_slice(&[job.w as f32, job.h as f32]);
                        }
                        // A liquid with depth: where it's seen from (texel_dir; see
                        // fs_liquid): a 3D layer's camera in its own pixels, or
                        // for a 2D layer the slant of the view from above.
                        if e.kind == EffectKind::Liquid && e.values[reelforge_core::effects::liquid::DEPTH] > 0.0 {
                            let cam = l.space.and_then(|s| s.local_camera()).filter(|c| c[2] > 1.0);
                            let look = match cam {
                                Some(c) => [c[0], c[1], c[2], 1.0],
                                None => [0.0, e.values[reelforge_core::effects::liquid::VIEW].clamp(-60.0, 60.0).to_radians().tan(), 0.0, 2.0],
                            };
                            u[8..12].copy_from_slice(&look);
                        } else {
                            u[8..12].copy_from_slice(&[0.0; 4]);
                        }
                        let entry = match e.kind {
                            EffectKind::Ocean => "fs_ocean",
                            EffectKind::WaterSurface => "fs_water",
                            EffectKind::Liquid3D => "fs_liquid3d",
                            _ => "fs_liquid",
                        };
                        push(entry, cur, second, o1, u);
                        cur = o1;
                    }
                    EffectKind::Paint => {
                        if !self.tables.contains_key(&(key, i)) {
                            continue;
                        }
                        let mut u = uniform(&e.values, [0.0; 2]);
                        u[14] = l.layer_size[0];
                        u[15] = l.layer_size[1];
                        u[24..28].copy_from_slice(&e.gpu);
                        push("fs_paint", cur, Second::Table(i), o1, u);
                        cur = o1;
                    }
                    EffectKind::Liquify => {
                        if !self.tables.contains_key(&(key, i)) {
                            continue;
                        }
                        let mut u = uniform(&e.values, [0.0; 2]);
                        u[14] = l.layer_size[0];
                        u[15] = l.layer_size[1];
                        u[24..28].copy_from_slice(&e.gpu);
                        push("fs_liquify", cur, Second::Table(i), o1, u);
                        cur = o1;
                    }
                    EffectKind::Hud => {
                        // The element, then its readout (when it has one) beside it.
                        let mut u = uniform(&e.values, [i as f32 + 1.0, 0.0]);
                        u[13] = e.time;
                        u[14] = l.layer_size[0];
                        u[15] = l.layer_size[1];
                        u[28..32].copy_from_slice(&e.values[16..20]);
                        u[FX_P5..FX_P5 + 4].copy_from_slice(&e.values[20..24]);
                        push("fs_hud", cur, Second::None, o1, u);
                        cur = o1;
                        if e.table.is_some() && self.tables.contains_key(&(key, i)) {
                            let (o1, _) = others(cur);
                            u[24..28].copy_from_slice(&e.gpu);
                            push("fs_hud_text", cur, Second::Table(i), o1, u);
                            cur = o1;
                        }
                    }
                    EffectKind::SparkleTrail => {
                        if !self.tables.contains_key(&(key, i)) {
                            continue;
                        }
                        let mut u = uniform(&e.values, [0.0; 2]);
                        u[13] = e.time;
                        u[14] = l.layer_size[0];
                        u[15] = l.layer_size[1];
                        u[24..28].copy_from_slice(&e.gpu);
                        push("fs_sparkle", cur, Second::Table(i), o1, u);
                        cur = o1;
                    }
                    EffectKind::DepthMap | EffectKind::Relight | EffectKind::PortraitBlur | EffectKind::VolumetricLight => {
                        if !self.tables.contains_key(&(key, i)) {
                            continue;
                        }
                        let mut u = uniform(&e.values, [0.0; 2]);
                        u[14] = l.layer_size[0];
                        u[15] = l.layer_size[1];
                        let entry = match e.kind {
                            EffectKind::DepthMap => "fs_depth_map",
                            EffectKind::Relight => "fs_relight",
                            EffectKind::VolumetricLight => "fs_volumetric",
                            _ => "fs_portrait",
                        };
                        u[24..28].copy_from_slice(&e.gpu);
                        push(entry, cur, Second::Table(i), o1, u);
                        cur = o1;
                    }
                    EffectKind::FaceBeauty => {
                        if !self.tables.contains_key(&(key, i)) {
                            continue;
                        }
                        let mut u = uniform(&e.values, [0.0; 2]);
                        u[14] = l.layer_size[0];
                        u[15] = l.layer_size[1];
                        u[24..28].copy_from_slice(&e.gpu);
                        push("fs_beauty", cur, Second::Table(i), o1, u);
                        cur = o1;
                    }
                    EffectKind::SkyReplace => {
                        if !self.tables.contains_key(&(key, i)) {
                            continue;
                        }
                        let mut u = uniform(&e.values, [0.0; 2]);
                        u[14] = l.layer_size[0];
                        u[15] = l.layer_size[1];
                        u[24..28].copy_from_slice(&e.gpu);
                        push("fs_sky_cut", cur, Second::Table(i), o1, u);
                        let sky = if self.tables.contains_key(&(key, i | TABLE2)) { i | TABLE2 } else { i };
                        push("fs_sky_fill", o1, Second::Table(sky), o2, u);
                        cur = o2;
                    }
                    EffectKind::ObjectRemoval => {
                        if !self.tables.contains_key(&(key, i)) {
                            continue;
                        }
                        let mut u = uniform(&e.values, [0.0; 2]);
                        u[24..28].copy_from_slice(&e.gpu);
                        push("fs_fill", cur, Second::Table(i), o1, u);
                        cur = o1;
                    }
                    EffectKind::AnamorphicFlare => {
                        // Bright parts → stretched sideways (long), then a little
                        // up and down (thickness) → tinted and added back as light.
                        use reelforge_core::effects::streak;
                        let mut u = uniform(&e.values, [0.0; 2]);
                        u[14] = l.layer_size[0];
                        u[15] = l.layer_size[1];
                        push("fs_streak_src", cur, Second::None, o1, u);
                        let mut vx = [0.0; reelforge_core::effects::MAX_PARAMS];
                        vx[0] = e.values[streak::LENGTH] * 0.5 * kx;
                        push("fs_blur", o1, Second::None, o2, uniform(&vx, [1.0, 0.0]));
                        vx[0] = e.values[streak::LENGTH] * 0.12 * kx;
                        push("fs_blur", o2, Second::None, o1, uniform(&vx, [1.0, 0.0]));
                        let mut vy = [0.0; reelforge_core::effects::MAX_PARAMS];
                        vy[0] = e.values[streak::THICKNESS].max(0.01) * 0.5 * ky;
                        push("fs_blur", o1, Second::None, o2, uniform(&vy, [0.0, 1.0]));
                        push("fs_streak_add", cur, Second::Scratch(o2), o1, uniform(&e.values, [0.0; 2]));
                        cur = o1;
                    }
                    EffectKind::Lightning | EffectKind::NeonPath => {
                        if !self.tables.contains_key(&(key, i)) {
                            continue;
                        }
                        let mut u = uniform(&e.values, [0.0; 2]);
                        u[13] = e.time;
                        u[14] = l.layer_size[0];
                        u[15] = l.layer_size[1];
                        u[24..28].copy_from_slice(&e.gpu);
                        u[28..32].copy_from_slice(&e.values[16..20]);
                        push("fs_lightning", cur, Second::Table(i), o1, u);
                        cur = o1;
                    }
                    EffectKind::AudioSpectrum => {
                        if !self.tables.contains_key(&(key, i)) {
                            continue;
                        }
                        let mut u = uniform(&e.values, [0.0; 2]);
                        u[13] = e.time;
                        u[14] = l.layer_size[0];
                        u[15] = l.layer_size[1];
                        u[24..28].copy_from_slice(&e.gpu);
                        u[28..32].copy_from_slice(&e.values[16..20]);
                        push("fs_audio_vis", cur, Second::Table(i), o1, u);
                        cur = o1;
                    }
                    EffectKind::MuzzleFlash | EffectKind::BulletHit => {
                        let mut u = uniform(&e.values, [i as f32 + 1.0, 0.0]);
                        u[13] = e.time;
                        u[14] = l.layer_size[0];
                        u[15] = l.layer_size[1];
                        push(if e.kind == EffectKind::MuzzleFlash { "fs_muzzle" } else { "fs_bullet" }, cur, Second::None, o1, u);
                        cur = o1;
                    }
                    EffectKind::LensFlare
                    | EffectKind::CameraShake
                    | EffectKind::ChromaticAberration
                    | EffectKind::FilmGrain
                    | EffectKind::Glitch
                    | EffectKind::LightLeaks
                    | EffectKind::Halftone
                    | EffectKind::Crt
                    | EffectKind::FractalNoise
                    | EffectKind::PatchReplacer
                    | EffectKind::LightRays
                    | EffectKind::LensDirt
                    | EffectKind::Bulge
                    | EffectKind::Twirl
                    | EffectKind::Ripple
                    | EffectKind::WaveWarp
                    | EffectKind::PolarCoordinates
                    | EffectKind::Mirror
                    | EffectKind::LensDistortion
                    | EffectKind::FilmLook
                    | EffectKind::FilmDamage
                    | EffectKind::AnalogueDamage
                    | EffectKind::Abstraction
                    | EffectKind::PrismBlur
                    | EffectKind::TiltShift
                    | EffectKind::TurbulentDisplace
                    | EffectKind::GradientMap
                    | EffectKind::Aurora
                    | EffectKind::Nebula
                    | EffectKind::Clouds
                    | EffectKind::BurnAway
                    | EffectKind::Deflicker
                    | EffectKind::Rotate3d
                    | EffectKind::Bend
                    | EffectKind::Bevel
                    | EffectKind::Extrude
                    | EffectKind::Extrusion3d
                    | EffectKind::Demultiply
                    | EffectKind::RemoveStockBackground
                    | EffectKind::BlockDissolve
                    | EffectKind::GradientWipe
                    | EffectKind::IrisWipe
                    | EffectKind::LinearWipe
                    | EffectKind::RadialWipe
                    | EffectKind::VenetianBlinds
                    | EffectKind::GlassWipe
                    | EffectKind::GridWipe
                    | EffectKind::Jaws
                    | EffectKind::LightWipe
                    | EffectKind::LineSweep
                    | EffectKind::RadialScaleWipe
                    | EffectKind::ScaleWipe
                    | EffectKind::Twister
                    | EffectKind::WarpoMatic
                    | EffectKind::Numbers
                    | EffectKind::Timecode
                    | EffectKind::CineonConverter
                    | EffectKind::HdrCompress
                    | EffectKind::BallAction
                    | EffectKind::Bubbles
                    | EffectKind::Drizzle
                    | EffectKind::ColorKey
                    | EffectKind::ColorRange
                    | EffectKind::ColorDifferenceKey
                    | EffectKind::LinearColorKey
                    | EffectKind::Extract
                    | EffectKind::SpillSuppressor
                    | EffectKind::BrushStrokes
                    | EffectKind::Cartoon
                    | EffectKind::Emboss
                    | EffectKind::ColorEmboss
                    | EffectKind::FindEdges
                    | EffectKind::Mosaic
                    | EffectKind::Posterize
                    | EffectKind::RoughenEdges
                    | EffectKind::Scatter
                    | EffectKind::Texturize
                    | EffectKind::Threshold
                    | EffectKind::BlockLoad
                    | EffectKind::BurnFilm
                    | EffectKind::HexTile
                    | EffectKind::Kaleida
                    | EffectKind::MrSmoothie
                    | EffectKind::Plastic
                    | EffectKind::GlassCc
                    | EffectKind::RepeTile
                    | EffectKind::FourColorGradient
                    | EffectKind::Beam
                    | EffectKind::CellPattern
                    | EffectKind::Checkerboard
                    | EffectKind::Circle
                    | EffectKind::Ellipse
                    | EffectKind::Fill
                    | EffectKind::Grid
                    | EffectKind::RadioWaves
                    | EffectKind::Threads
                    | EffectKind::LightBurst
                    | EffectKind::CurlNoise
                    | EffectKind::Noise
                    | EffectKind::NoiseHls
                    | EffectKind::Median
                    | EffectKind::DustScratches
                    | EffectKind::CornerPin
                    | EffectKind::PowerPin
                    | EffectKind::DisplacementMap
                    | EffectKind::Magnify
                    | EffectKind::MeshWarp
                    | EffectKind::Offset
                    | EffectKind::OpticsCompensation
                    | EffectKind::Spherize
                    | EffectKind::Smear
                    | EffectKind::Warp
                    | EffectKind::Bender
                    | EffectKind::Blobbylize
                    | EffectKind::FloMotion
                    | EffectKind::LensCc
                    | EffectKind::PageTurn
                    | EffectKind::Slant
                    | EffectKind::Split
                    | EffectKind::Tiler
                    | EffectKind::Griddler
                    | EffectKind::RipplePulse
                    | EffectKind::MotionTile
                    | EffectKind::Glasses3d
                    | EffectKind::RadialShadow
                    | EffectKind::Cylinder
                    | EffectKind::Sphere
                    | EffectKind::SmartBlur
                    | EffectKind::CameraLensBlur
                    | EffectKind::VectorBlur
                    | EffectKind::Arithmetic
                    | EffectKind::BlendColour
                    | EffectKind::Calculations
                    | EffectKind::ChannelMixer
                    | EffectKind::Invert
                    | EffectKind::ShiftChannels
                    | EffectKind::SetChannels
                    | EffectKind::SetMatte
                    | EffectKind::SolidComposite
                    | EffectKind::ChangeColor
                    | EffectKind::ChangeToColor
                    | EffectKind::ColorBalance
                    | EffectKind::Exposure
                    | EffectKind::LeaveColor
                    | EffectKind::PhotoFilter
                    | EffectKind::SelectiveColor
                    | EffectKind::Tint
                    | EffectKind::BlackWhite
                    | EffectKind::ColorNeutralizer
                    | EffectKind::ColorOffset
                    | EffectKind::Toner
                    | EffectKind::ThresholdRgb
                    | EffectKind::Kernel
                    | EffectKind::VideoLimiter
                    | EffectKind::HeatHaze
                    | EffectKind::Shockwave
                    | EffectKind::Starfield
                    | EffectKind::Hologram
                    | EffectKind::Portal
                    | EffectKind::LightSweep
                    | EffectKind::ElectricArcs
                    | EffectKind::DirectionalBlur
                    | EffectKind::RadialBlur
                    | EffectKind::MotionBlur
                    | EffectKind::NightVision
                    | EffectKind::ThermalCamera
                    | EffectKind::Infrared
                    | EffectKind::XRay
                    | EffectKind::Spotlight
                    | EffectKind::Gleam
                    | EffectKind::Halation
                    | EffectKind::CustomFlare
                    | EffectKind::ForceField
                    | EffectKind::Teleport
                    | EffectKind::BlasterFire
                    | EffectKind::BloodSpatter
                    | EffectKind::BlockDisplacement
                    | EffectKind::Derez
                    | EffectKind::EnergyDistortion
                    | EffectKind::FluidDistortion
                    | EffectKind::SmokeDistortion
                    | EffectKind::GradientRamp
                    | EffectKind::Strobe
                    | EffectKind::Scanlines => {
                        // A number of its own (its place in the stack), the time and the layer's size.
                        let mut u = uniform(&e.values, [i as f32 + 1.0, 0.0]);
                        u[13] = e.time;
                        u[14] = l.layer_size[0];
                        u[15] = l.layer_size[1];
                        // Params 16–19 (for effects with more than 16).
                        u[28..32].copy_from_slice(&e.values[16..20]);
                        let entry = match e.kind {
                            EffectKind::LensFlare => "fs_flare",
                            EffectKind::CameraShake => "fs_shake",
                            EffectKind::ChromaticAberration => "fs_aberration",
                            EffectKind::FilmGrain => "fs_grain",
                            EffectKind::Glitch => "fs_glitch",
                            EffectKind::Halftone => "fs_halftone",
                            EffectKind::Crt => "fs_crt",
                            EffectKind::FractalNoise => "fs_fractal",
                            EffectKind::PatchReplacer => "fs_patch",
                            EffectKind::LightRays => "fs_rays",
                            EffectKind::LensDirt => "fs_dirt",
                            EffectKind::Bulge => "fs_bulge",
                            EffectKind::Twirl => "fs_twirl",
                            EffectKind::Ripple => "fs_ripple",
                            EffectKind::WaveWarp => "fs_wave",
                            EffectKind::PolarCoordinates => "fs_polar",
                            EffectKind::Mirror => "fs_mirror",
                            EffectKind::LensDistortion => "fs_lens",
                            EffectKind::FilmLook => "fs_film_look",
                            EffectKind::FilmDamage => "fs_film_damage",
                            EffectKind::AnalogueDamage => "fs_analogue",
                            EffectKind::Abstraction => "fs_abstraction",
                            EffectKind::PrismBlur => "fs_prism",
                            EffectKind::TiltShift => "fs_tilt_shift",
                            EffectKind::Scanlines => "fs_scanlines",
                            EffectKind::Strobe => "fs_strobe",
                            EffectKind::GradientRamp => "fs_ramp",
                            EffectKind::SmokeDistortion => "fs_smoke_distort",
                            EffectKind::FluidDistortion => "fs_fluid_distort",
                            EffectKind::EnergyDistortion => "fs_energy_distort",
                            EffectKind::Derez => "fs_derez",
                            EffectKind::BlockDisplacement => "fs_block_displace",
                            EffectKind::BloodSpatter => "fs_blood",
                            EffectKind::BlasterFire => "fs_blaster",
                            EffectKind::Teleport => "fs_teleport",
                            EffectKind::ForceField => "fs_force_field",
                            EffectKind::CustomFlare => "fs_custom_flare",
                            EffectKind::Halation => "fs_halation",
                            EffectKind::Gleam => "fs_gleam",
                            EffectKind::Spotlight => "fs_spotlight",
                            EffectKind::XRay => "fs_xray",
                            EffectKind::Infrared => "fs_infrared",
                            EffectKind::ThermalCamera => "fs_thermal",
                            EffectKind::NightVision => "fs_night_vision",
                            EffectKind::MotionBlur => "fs_motion_blur",
                            EffectKind::RadialBlur => "fs_radial_blur",
                            EffectKind::DirectionalBlur => "fs_dir_blur",
                            EffectKind::ElectricArcs => "fs_arcs",
                            EffectKind::LightSweep => "fs_sweep",
                            EffectKind::Portal => "fs_portal",
                            EffectKind::Hologram => "fs_hologram",
                            EffectKind::Starfield => "fs_starfield",
                            EffectKind::Shockwave => "fs_shockwave",
                            EffectKind::HeatHaze => "fs_heat_haze",
                            EffectKind::TurbulentDisplace => "fs_turbulent",
                            EffectKind::GradientMap => "fs_gradient_map",
                            EffectKind::Aurora => "fs_aurora",
                            EffectKind::Nebula => "fs_nebula",
                            EffectKind::Clouds => "fs_clouds",
                            EffectKind::BurnAway => "fs_burn",
                            EffectKind::Deflicker => "fs_deflicker",
                            EffectKind::Rotate3d => "fs_rotate3d",
                            EffectKind::Bend => "fs_bend",
                            EffectKind::Bevel => "fs_bevel",
                            EffectKind::Extrude => "fs_extrude",
                            EffectKind::Extrusion3d => "fs_extrusion3d",
                            EffectKind::Demultiply => "fs_demultiply",
                            EffectKind::RemoveStockBackground => "fs_stock_background",
                            EffectKind::BlockDissolve => "fs_block_dissolve",
                            EffectKind::GradientWipe => "fs_gradient_wipe",
                            EffectKind::IrisWipe => "fs_iris_wipe",
                            EffectKind::LinearWipe => "fs_linear_wipe",
                            EffectKind::RadialWipe => "fs_radial_wipe",
                            EffectKind::VenetianBlinds => "fs_venetian_blinds",
                            EffectKind::GlassWipe => "fs_glass_wipe",
                            EffectKind::GridWipe => "fs_grid_wipe",
                            EffectKind::Jaws => "fs_jaws",
                            EffectKind::LightWipe => "fs_light_wipe",
                            EffectKind::LineSweep => "fs_line_sweep",
                            EffectKind::RadialScaleWipe => "fs_radial_scale_wipe",
                            EffectKind::ScaleWipe => "fs_scale_wipe",
                            EffectKind::Twister => "fs_twister",
                            EffectKind::WarpoMatic => "fs_warpomatic",
                            EffectKind::Numbers => "fs_numbers",
                            EffectKind::Timecode => "fs_timecode",
                            EffectKind::CineonConverter => "fs_cineon",
                            EffectKind::HdrCompress => "fs_hdr_compress",
                            EffectKind::BallAction => "fs_ball_action",
                            EffectKind::Bubbles => "fs_bubbles",
                            EffectKind::Drizzle => "fs_drizzle",
                            EffectKind::ColorKey => "fs_color_key",
                            EffectKind::ColorRange => "fs_color_range",
                            EffectKind::ColorDifferenceKey => "fs_color_difference_key",
                            EffectKind::LinearColorKey => "fs_linear_color_key",
                            EffectKind::Extract => "fs_extract",
                            EffectKind::SpillSuppressor => "fs_spill_suppressor",
                            EffectKind::BrushStrokes => "fs_brush_strokes",
                            EffectKind::Cartoon => "fs_cartoon",
                            EffectKind::Emboss => "fs_emboss",
                            EffectKind::ColorEmboss => "fs_color_emboss",
                            EffectKind::FindEdges => "fs_find_edges",
                            EffectKind::Mosaic => "fs_mosaic_blocks",
                            EffectKind::Posterize => "fs_posterize",
                            EffectKind::RoughenEdges => "fs_roughen_edges",
                            EffectKind::Scatter => "fs_scatter",
                            EffectKind::Texturize => "fs_texturize",
                            EffectKind::Threshold => "fs_threshold",
                            EffectKind::BlockLoad => "fs_block_load",
                            EffectKind::BurnFilm => "fs_burn_film",
                            EffectKind::HexTile => "fs_hex_tile",
                            EffectKind::Kaleida => "fs_kaleida",
                            EffectKind::MrSmoothie => "fs_mr_smoothie",
                            EffectKind::Plastic => "fs_plastic",
                            EffectKind::GlassCc => "fs_glass_cc",
                            EffectKind::RepeTile => "fs_repe_tile",
                            EffectKind::FourColorGradient => "fs_four_color",
                            EffectKind::Beam => "fs_beam",
                            EffectKind::CellPattern => "fs_cell_pattern",
                            EffectKind::Checkerboard => "fs_checkerboard",
                            EffectKind::Circle => "fs_circle",
                            EffectKind::Ellipse => "fs_ellipse",
                            EffectKind::Fill => "fs_fill_colour",
                            EffectKind::Grid => "fs_grid",
                            EffectKind::RadioWaves => "fs_radio_waves",
                            EffectKind::Threads => "fs_threads",
                            EffectKind::LightBurst => "fs_light_burst",
                            EffectKind::CurlNoise => "fs_curl_noise",
                            EffectKind::Noise => "fs_noise",
                            EffectKind::NoiseHls => "fs_noise_hls",
                            EffectKind::Median => "fs_median",
                            EffectKind::DustScratches => "fs_dust_scratches",
                            EffectKind::CornerPin => "fs_corner_pin",
                            EffectKind::PowerPin => "fs_power_pin",
                            EffectKind::DisplacementMap => "fs_displacement_map",
                            EffectKind::Magnify => "fs_magnify",
                            EffectKind::MeshWarp => "fs_mesh_warp",
                            EffectKind::Offset => "fs_offset",
                            EffectKind::OpticsCompensation => "fs_optics",
                            EffectKind::Spherize => "fs_spherize",
                            EffectKind::Smear => "fs_smear",
                            EffectKind::Warp => "fs_warp",
                            EffectKind::Bender => "fs_bender",
                            EffectKind::Blobbylize => "fs_blobbylize",
                            EffectKind::FloMotion => "fs_flo_motion",
                            EffectKind::LensCc => "fs_lens_cc",
                            EffectKind::PageTurn => "fs_page_turn",
                            EffectKind::Slant => "fs_slant",
                            EffectKind::Split => "fs_split",
                            EffectKind::Tiler => "fs_tiler",
                            EffectKind::Griddler => "fs_griddler",
                            EffectKind::RipplePulse => "fs_ripple_pulse",
                            EffectKind::MotionTile => "fs_motion_tile",
                            EffectKind::Glasses3d => "fs_glasses3d",
                            EffectKind::RadialShadow => "fs_radial_shadow",
                            EffectKind::Cylinder => "fs_cylinder",
                            EffectKind::Sphere => "fs_sphere",
                            EffectKind::SmartBlur => "fs_smart_blur",
                            EffectKind::CameraLensBlur => "fs_lens_blur",
                            EffectKind::VectorBlur => "fs_vector_blur",
                            EffectKind::Arithmetic => "fs_arithmetic",
                            EffectKind::BlendColour => "fs_blend_colour",
                            EffectKind::Calculations => "fs_calculations",
                            EffectKind::ChannelMixer => "fs_channel_mixer",
                            EffectKind::Invert => "fs_invert",
                            EffectKind::ShiftChannels => "fs_shift_channels",
                            EffectKind::SetChannels => "fs_set_channels",
                            EffectKind::SetMatte => "fs_set_matte",
                            EffectKind::SolidComposite => "fs_solid_composite",
                            EffectKind::ChangeColor => "fs_change_color",
                            EffectKind::ChangeToColor => "fs_change_to_color",
                            EffectKind::ColorBalance => "fs_color_balance",
                            EffectKind::Exposure => "fs_exposure",
                            EffectKind::LeaveColor => "fs_leave_color",
                            EffectKind::PhotoFilter => "fs_photo_filter",
                            EffectKind::SelectiveColor => "fs_selective_color",
                            EffectKind::Tint => "fs_tint",
                            EffectKind::BlackWhite => "fs_black_white",
                            EffectKind::ColorNeutralizer => "fs_color_neutralizer",
                            EffectKind::ColorOffset => "fs_color_offset",
                            EffectKind::Toner => "fs_toner",
                            EffectKind::ThresholdRgb => "fs_threshold_rgb",
                            EffectKind::Kernel => "fs_kernel",
                            EffectKind::VideoLimiter => "fs_video_limiter",
                            _ => "fs_leaks",
                        };
                        push(entry, cur, Second::None, o1, u);
                        cur = o1;
                    }
                    EffectKind::Plugin => {
                        let Some(def) = &e.plugin else { continue };
                        if !self.ensure_plugin(def) {
                            continue;
                        }
                        // Each pass reads the last one's result (the first reads `cur`) and
                        // can read `cur` as "the original"; results alternate between the other two.
                        let n = def.passes.len();
                        let (mut read, mut write) = (cur, o1);
                        for k in 0..n {
                            let mut u = uniform(&e.values, [0.0; 2]);
                            u[28..32].copy_from_slice(&e.values[16..20]);
                            u[FX_P5..FX_P5 + 4].copy_from_slice(&e.values[20..24]);
                            u[FX_P6..FX_P6 + 8].copy_from_slice(&e.values[24..32]);
                            u[13] = e.time;
                            u[14] = l.layer_size[0];
                            u[15] = l.layer_size[1];
                            u[24..28].copy_from_slice(&[pw, ph, k as f32, n as f32]);
                            let at = push("", read, Second::Scratch(cur), write, u);
                            plugin_marks.push((at, def.revision, k));
                            read = write;
                            write = if write == o1 { o2 } else { o1 };
                        }
                        cur = read;
                    }
                }
            }
            for (at, rev, k) in plugin_marks {
                fx_passes[at].plugin = Some((rev, k));
            }
            for (at, set) in particle_marks {
                fx_passes[at].particles = Some(set);
            }
            for p in &mut fx_passes[first_pass..] {
                p.uniform[FX_MAP..FX_MAP + 4].copy_from_slice(&map);
            }
            processed.insert(l.key, cur);
        }
        let particle_batches = if particle_sets.is_empty() { Vec::new() } else { self.particles.upload(&self.device, &self.queue, &particle_sets) };
        if !fx_passes.is_empty() {
            let mut data = vec![0u8; self.fx_uniform_stride as usize * fx_passes.len()];
            for (i, p) in fx_passes.iter().enumerate() {
                let bytes: Vec<u8> = p.uniform.iter().flat_map(|f| f.to_ne_bytes()).collect();
                let off = i * self.fx_uniform_stride as usize;
                data[off..off + bytes.len()].copy_from_slice(&bytes);
            }
            self.queue.write_buffer(&self.fx_uniforms, 0, &data);
        }

        // Build the draw list: (uniform slot, what to draw). `fx` is the scratch
        // target holding the layer's processed (premultiplied) pixels, if any.
        struct Draw {
            slot: usize,
            key: u64,
            fx: Option<usize>,
        }
        enum Op {
            /// Draw a layer into the frame.
            Frame { draw: Draw, blend: BlendMode },
            /// An adjustment layer: its effects run on the frame so far (drawn
            /// at this point, not up front), and the result is drawn over it.
            Adjust { draw: Draw, blend: BlendMode },
            /// Draw a dissolve group: sum the layers into the group target (cleared
            /// to `clear`, premultiplied, for a dip), then composite the group into the frame.
            Group { a: Option<Draw>, b: Option<Draw>, over: usize, blend: BlendMode, clear: wgpu::Color },
            /// A track matte: the matte and the layer each drawn alone, the
            /// layer kept where the matte says, the result composited.
            Matte { matte: Draw, layer: Draw, kind: u32, over: usize, blend: BlendMode },
        }
        type Uniform<'m> = (Transform, [f32; 2], Option<Wipe>, Option<&'m reelforge_core::meshwarp::MeshShape>, Option<&'m reelforge_core::space3d::LayerSpace>, [f32; 4]);
        let mut uniforms: Vec<Uniform> = Vec::new();
        fn slot<'m>(u: &mut Vec<Uniform<'m>>, l: (Transform, [f32; 2], Option<Wipe>), mesh: Option<&'m reelforge_core::meshwarp::MeshShape>, space: Option<&'m reelforge_core::space3d::LayerSpace>) -> usize {
            let mesh = mesh.filter(|m| m.is_valid());
            u.push((l.0, l.1, l.2, mesh, space.filter(|_| mesh.is_none()), NO_MARGIN));
            u.len() - 1
        }
        // A processed layer drawn with its margin (texture map; the quad grows to show it).
        let with_margin = |u: &mut Vec<Uniform>, slot: usize, fx: Option<usize>, key: u64| {
            if fx.is_some() {
                if let Some(m) = margins.get(&key) {
                    u[slot].5 = *m;
                }
            }
        };
        let mut ops = Vec::new();
        // Light wrap, by the draw's uniform slot: (width px, amount, background blur px, mode).
        let mut wraps: HashMap<usize, [f32; 4]> = HashMap::new();
        for item in items {
            match item {
                DrawItem::Layer(l) if matches!(l.pixels, PixelData::Below) => {
                    // No effects (or too many passes): it changes nothing.
                    let Some(fx) = processed.get(&l.key).copied() else { continue };
                    let draw = Draw { slot: slot(&mut uniforms, (l.transform, l.layer_size, l.wipe), None, None), key: l.key, fx: Some(fx) };
                    ops.push(Op::Adjust { draw, blend: l.blend })
                }
                DrawItem::Layer(l) => {
                    let draw = Draw { slot: slot(&mut uniforms, (l.transform, l.layer_size, l.wipe), l.mesh.as_ref(), l.space), key: l.key, fx: processed.get(&l.key).copied() };
                    with_margin(&mut uniforms, draw.slot, draw.fx, draw.key);
                    // (flat layers only: a mesh or 3D lighting has the uniform's room)
                    if l.space.is_none() && uniforms[draw.slot].3.is_none() {
                        if let Some(w) = l.effects.iter().find(|e| e.kind == EffectKind::LightWrap && !e.is_identity()) {
                            use reelforge_core::effects::light_wrap as lw;
                            let v = &w.values;
                            wraps.insert(draw.slot, [v[lw::WIDTH].max(0.0), v[lw::AMOUNT].max(0.0) / 100.0, v[lw::SOFTNESS].max(0.0), v[lw::MODE]]);
                        }
                    }
                    ops.push(Op::Frame { draw, blend: l.blend })
                }
                DrawItem::Dissolve { from, to, mix, dip } => {
                    let m = mix.clamp(0.0, 1.0);
                    let (mut ta, mut tb) = (from.transform, to.transform);
                    let (draw_a, draw_b, clear) = match dip {
                        // A dip: first half fades `from` into the colour, second half `to` out of it.
                        Some(c) => {
                            let (w, first) = if m < 0.5 { (1.0 - 2.0 * m, true) } else { (2.0 * m - 1.0, false) };
                            if first {
                                ta.opacity *= w;
                            } else {
                                tb.opacity *= w;
                            }
                            let k = (1.0 - w) as f64;
                            (first, !first, wgpu::Color { r: c[0] as f64 * k, g: c[1] as f64 * k, b: c[2] as f64 * k, a: k })
                        }
                        None => {
                            ta.opacity *= 1.0 - m;
                            tb.opacity *= m;
                            (true, true, wgpu::Color::TRANSPARENT)
                        }
                    };
                    let a = draw_a.then(|| Draw { slot: slot(&mut uniforms, (ta, from.layer_size, from.wipe), from.mesh.as_ref(), from.space), key: from.key, fx: processed.get(&from.key).copied() });
                    let b = draw_b.then(|| Draw { slot: slot(&mut uniforms, (tb, to.layer_size, to.wipe), to.mesh.as_ref(), to.space), key: to.key, fx: processed.get(&to.key).copied() });
                    for d in [&a, &b].into_iter().flatten() {
                        with_margin(&mut uniforms, d.slot, d.fx, d.key);
                    }
                    let over = slot(&mut uniforms, (Transform::default(), frame, None), None, None);
                    ops.push(Op::Group { a, b, over, blend: to.blend, clear });
                }
                DrawItem::Matted { matte, layer, kind } => {
                    let m = Draw { slot: slot(&mut uniforms, (matte.transform, matte.layer_size, matte.wipe), matte.mesh.as_ref(), matte.space), key: matte.key, fx: processed.get(&matte.key).copied() };
                    let d = Draw { slot: slot(&mut uniforms, (layer.transform, layer.layer_size, layer.wipe), layer.mesh.as_ref(), layer.space), key: layer.key, fx: processed.get(&layer.key).copied() };
                    for x in [&m, &d] {
                        with_margin(&mut uniforms, x.slot, x.fx, x.key);
                    }
                    let over = slot(&mut uniforms, (Transform::default(), frame, None), None, None);
                    ops.push(Op::Matte { matte: m, layer: d, kind: (*kind).min(3), over, blend: layer.blend });
                }
            }
            if uniforms.len() > MAX_DRAWS - 3 {
                log::warn!("too many layers; dropping the rest");
                break;
            }
        }

        // Shadows: when a light casts them, the 3D layers being drawn (up to
        // MAX_OCCLUDERS) each get a tile of the shadow atlas with their outline,
        // and lit layers trace their light past them (see composite.wgsl).
        let shadowing = |l: &reelforge_core::space3d::Light| l.shadow > 0.0 && l.kind != 3;
        let layers_cast = uniforms.iter().any(|u| u.4.and_then(|s| s.lighting.as_ref()).is_some_and(|l| l.lights.iter().any(shadowing)));
        // 3D text casts too: its letters' outline, on the plane halfway through its depth.
        // (and 3D runs take it in: it has a depth picture).
        let mut solids: HashMap<u64, &SolidInput> = HashMap::new();
        {
            for item in items {
                let ls: Vec<&LayerInput> = match item {
                    DrawItem::Layer(l) => vec![l],
                    DrawItem::Dissolve { from, to, .. } => vec![from, to],
                    DrawItem::Matted { matte, layer, .. } => vec![matte, layer],
                };
                for l in ls {
                    if let PixelData::Solid(s) = &l.pixels {
                        solids.insert(l.key, s);
                    }
                }
            }
        }
        // (and is lit by the same lights, so it takes shadows as well.)
        let casts = layers_cast || solids.values().any(|s| s.space.lights.iter().any(shadowing));
        let mut occluders: Vec<(usize, u64, Option<usize>)> = Vec::new();
        if casts {
            let mut add = |d: &Draw| {
                let solid = occluders.len() < MAX_OCCLUDERS && solids.get(&d.key).is_some_and(|s| self.solid.prepare_outline(&self.queue, d.key, s, occluders.len()));
                if occluders.len() < MAX_OCCLUDERS && (uniforms[d.slot].4.is_some() || solid) && uniforms[d.slot].0.opacity > 0.0 {
                    occluders.push((d.slot, d.key, d.fx));
                }
            };
            for op in &ops {
                match op {
                    Op::Frame { draw, .. } => add(draw),
                    Op::Group { a, b, .. } => {
                        for d in [a, b].into_iter().flatten() {
                            add(d);
                        }
                    }
                    Op::Matte { layer, .. } => add(layer),
                    Op::Adjust { .. } => {}
                }
            }
        }
        let occluder_floats: Vec<f32> = occluders
            .iter()
            .enumerate()
            .flat_map(|(k, &(slot, key, _))| {
                let (t, size, _, _, space, fxmap) = &uniforms[slot];
                if let Some(s) = solids.get(&key) {
                    let b = s.mesh.bounds;
                    let sp = &s.space;
                    let o = sp.world([(b[0] + b[2]) * 0.5, (b[1] + b[3]) * 0.5, s.mesh.mid_z]);
                    let (u, v) = (sp.axes[0], sp.axes[1]);
                    let g = 1.0 / ATLAS_GRID as f32;
                    let tile = [(k as u32 % ATLAS_GRID) as f32 * g, (k as u32 / ATLAS_GRID) as f32 * g, g, 0.0];
                    return [
                        [o[0], o[1], o[2], (b[2] - b[0]) * 0.5],
                        [u[0], u[1], u[2], (b[3] - b[1]) * 0.5],
                        [v[0], v[1], v[2], t.opacity.clamp(0.0, 1.0)],
                        [0.0, 0.0, 1.0, 1.0],
                        [1.0, 1.0, 0.0, 0.0],
                        tile,
                    ]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<f32>>();
                }
                let [o, u, v] = space.map_or([[0.0; 3]; 3], |s| s.plane);
                let g = 1.0 / ATLAS_GRID as f32;
                    let tile = [(k as u32 % ATLAS_GRID) as f32 * g, (k as u32 / ATLAS_GRID) as f32 * g, g, 0.0];
                [
                    [o[0], o[1], o[2], size[0] * 0.5],
                    [u[0], u[1], u[2], size[1] * 0.5],
                    [v[0], v[1], v[2], t.opacity.clamp(0.0, 1.0)],
                    t.crop_uv(),
                    *fxmap,
                    tile,
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<f32>>()
            })
            .collect();

        // Uniforms for every draw in one buffer, addressed by dynamic offset.
        let mut data = vec![0u8; self.uniform_stride as usize * uniforms.len().max(1)];
        for (i, (t, size, wipe, mesh, space, fxmap)) in uniforms.iter().enumerate() {
            // (Corner-pinned and 3D layers get a perspective matrix.)
            let mvp = match space {
                Some(s) => s.mvp,
                None => t.layer_map(*size, frame).to_clip_space(frame),
            };
            let lighting = space.and_then(|s| s.lighting.as_ref());
            let mut floats: Vec<f32> = mvp.iter().flatten().copied().collect();
            let mode = wipe.map_or(0.0, |w| if w.iris { 2.0 } else { 1.0 });
            floats.extend([size[0], size[1], t.opacity.clamp(0.0, 1.0), mode]);
            let w = wipe.unwrap_or(Wipe { iris: false, axis: [0.0; 3], progress: 1.0, softness: 0.1 });
            floats.extend([w.axis[0], w.axis[1], w.axis[2], w.progress.clamp(0.0, 1.0)]);
            // The wipe works in the target's pixels (the fragment's position).
            floats.extend([w.softness, target_px[0], target_px[1], if lighting.is_some() { 1.0 } else { 0.0 }]);
            let crop = t.crop_uv();
            floats.extend(crop);
            // The effect target's map, and how far the quad reaches past the
            // crop: the whole margin, on the sides the crop hasn't cut.
            floats.extend(*fxmap);
            let (mx, my) = (-fxmap[2], -fxmap[3]);
            let edge = |c: f32, at: f32| (c - at).abs() < 1e-4;
            let grow = if mesh.is_some() { [0.0; 4] } else { [if edge(crop[0], 0.0) { mx } else { 0.0 }, if edge(crop[1], 0.0) { my } else { 0.0 }, if edge(crop[2], 1.0) { mx } else { 0.0 }, if edge(crop[3], 1.0) { my } else { 0.0 }] };
            floats.extend(grow);
            match mesh {
                Some(m) => {
                    floats.extend([m.cols as f32, m.rows as f32, 1.0, mesh_subdiv(m) as f32]);
                    let mut pts: Vec<f32> = m.points.iter().flatten().copied().collect();
                    pts.resize(MESH_VEC4S * 4, 0.0);
                    floats.extend(pts);
                }
                None => {
                    // (mesh.w = 1: Light wrap, its numbers in the first point)
                    let wrap = wraps.get(&i).filter(|_| lighting.is_none());
                    floats.extend([0.0, 0.0, 0.0, if wrap.is_some() { 1.0 } else { 0.0 }]);
                    if let Some(w) = wrap {
                        floats.extend(w);
                    }
                    // A lit 3D layer: its plane, the camera and the lights where the mesh would go.
                    if let Some(l) = lighting {
                        let n = l.lights.len().min(reelforge_core::space3d::MAX_LIGHTS);
                        let pts0 = floats.len();
                        floats.extend([l.origin[0], l.origin[1], l.origin[2], n as f32]);
                        floats.extend([l.u[0], l.u[1], l.u[2], 0.0]);
                        floats.extend([l.v[0], l.v[1], l.v[2], 0.0]);
                        floats.extend([l.camera[0], l.camera[1], l.camera[2], occluders.len() as f32]);
                        for g in &l.lights[..n] {
                            floats.extend(g.gpu().iter().flatten());
                        }
                        if !occluders.is_empty() {
                            floats.resize(pts0 + OCCLUDER_BASE * 4, 0.0);
                            floats.extend_from_slice(&occluder_floats);
                        }
                    }
                }
            }
            let bytes: Vec<u8> = floats.iter().flat_map(|f| f.to_ne_bytes()).collect();
            let off = i * self.uniform_stride as usize;
            data[off..off + bytes.len()].copy_from_slice(&bytes);
        }
        self.queue.write_buffer(&self.uniforms, 0, &data);

        let Some(targets) = self.targets.as_ref() else { return };
        let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("composite") });
        let pass_on = |enc: &mut wgpu::CommandEncoder, view: &wgpu::TextureView, load: wgpu::LoadOp<wgpu::Color>| {
            enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("composite"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations { load, store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            })
            .forget_lifetime()
        };

        // One effect pass: its own render pass on a layer's scratch target.
        // Encoding one effect pass (a macro, as it's needed twice: for the layers some
        // fluids flow round, then for the rest).
        macro_rules! encode_fx {
            () => {
                |enc: &mut wgpu::CommandEncoder, i: usize, p: &FxPass| {
            let Some(fx) = self.fx_targets.get(&p.key) else { return };
            let source = match p.src {
                Src::Layer => match self.layers.get(&p.key) {
                    Some(t) => &t.bind_group,
                    None => return,
                },
                Src::Below => &targets.accum_bind,
                Src::Scratch(s) => &fx.binds[s],
            };
            let second = match p.second {
                Second::None => None,
                Second::Mask => Some(self.masks.get(&p.key).map(|m| &m.bind_group)),
                Second::KeepMask => Some(self.masks.get(&(p.key ^ KEEP_MASK)).map(|m| &m.bind_group)),
                Second::Table(e) => Some(self.tables.get(&(p.key, e)).map(|t| &t.bind_group)),
                Second::Scratch(s) => Some(Some(&fx.binds[s])),
                Second::Blank => Some(Some(&self.blank_second)),
            };
            if matches!(second, Some(None)) {
                return; // its texture is missing
            }
            let pipeline = match (p.plugin, second) {
                (Some((rev, k)), Some(Some(_))) => match self.plugin_pipelines.get(&rev) {
                    Some(Some(passes)) => &passes[k],
                    _ => return,
                },
                (Some(_), _) => return,
                (None, Some(Some(_))) => &self.fx2_pipelines[p.entry],
                (None, _) => &self.fx_pipelines[p.entry],
            };
            let mut pass = pass_on(enc, &fx.views[p.dst], wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT));
            pass.set_pipeline(pipeline);
            if let Some(Some(b)) = second {
                pass.set_bind_group(2, b, &[]);
            }
            pass.set_bind_group(0, &self.fx_uniform_bind, &[(i as u64 * self.fx_uniform_stride) as u32]);
            pass.set_bind_group(1, source, &[]);
            pass.draw(0..3, 0..1);
            if let Some(b) = p.particles.and_then(|i| particle_batches.get(i)) {
                self.particles.draw(&mut pass, b);
            }
        }
            };
        }
        // Fluids flowing round another layer after its effects: that layer's passes
        // first (submitted), then the fluids stepped on with what they made.
        let mut early: std::collections::HashSet<u64> = std::collections::HashSet::new();
        if !deferred.is_empty() {
            early = deferred.iter().filter_map(|(_, _, j)| j.source.map(|s| s.key)).filter(|k| processed.contains_key(k)).collect();
            let mut enc = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("obstacle effects") });
            {
                let encode_fx = encode_fx!();
                for (i, p) in fx_passes.iter().enumerate() {
                    if early.contains(&p.key) {
                        encode_fx(&mut enc, i, p);
                    }
                }
            }
            self.queue.submit([enc.finish()]);
            for (key, i, mut job) in deferred {
                let Some(src) = job.source else { continue };
                let made = processed.get(&src.key).copied().zip(margins.get(&src.key).copied());
                let tex = match made {
                    Some((t, [sx, sy, ox, oy])) => {
                        // (the processed picture has room round it: its uv, from the layer's)
                        let m = src.map;
                        let (ax, ay) = (1.0 / sx.max(1e-6), 1.0 / sy.max(1e-6));
                        let to_target = [ax * m[0] - ax * ox * m[6], ax * m[1] - ax * ox * m[7], ax * m[2] - ax * ox * m[8], ay * m[3] - ay * oy * m[6], ay * m[4] - ay * oy * m[7], ay * m[5] - ay * oy * m[8], m[6], m[7], m[8]];
                        job.source = Some(reelforge_core::fluid::ObstacleSource { key: src.key, map: to_target });
                        let view = self.fx_targets.get(&src.key).and_then(|f| f.views.get(t));
                        self.fluid.step_to(&job, view)
                    }
                    None => {
                        let view = self.obstacle_view(key, &job);
                        self.fluid.step_to(&job, view.as_ref())
                    }
                };
                if let Some(tex) = tex {
                    let view = tex.create_view(&Default::default());
                    let bind_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("fluid picture"),
                        layout: &self.texture_layout,
                        entries: &[
                            wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
                            wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                        ],
                    });
                    let size = [tex.width(), tex.height()];
                    self.tables.insert((key, i), LayerTex { texture: tex, bind_group, size, content_id: job.key ^ job.step as u64, float: false });
                }
            }
        }
        let encode_fx = encode_fx!();
        // Adjustment layers' passes wait for the frame below them to be drawn.
        let adjust: std::collections::HashSet<u64> = ops.iter().filter_map(|o| if let Op::Adjust { draw, .. } = o { Some(draw.key) } else { None }).collect();
        // 3D text in shadow is drawn again once the atlas is ready (below), so
        // its effects wait for that too.
        let shaded: Vec<u64> = if occluders.is_empty() { Vec::new() } else { solids.iter().filter(|(_, s)| s.space.lights.iter().any(shadowing)).map(|(k, _)| *k).collect() };
        for (i, p) in fx_passes.iter().enumerate() {
            if !adjust.contains(&p.key) && !shaded.contains(&p.key) && !early.contains(&p.key) {
                encode_fx(&mut enc, i, p);
            }
        }
        if dry {
            // (only the effects: the frame isn't drawn)
            self.queue.submit([enc.finish()]);
            return;
        }
        // The shadow casters' outlines into the atlas (from what they'll be drawn with).
        if !occluders.is_empty() {
            let tile_px = self.shadow_tile;
            let mut pass = pass_on(&mut enc, &self.shadow_atlas_view, wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT));
            pass.set_pipeline(&self.occluder_pipeline);
            for (k, &(_, key, fx)) in occluders.iter().enumerate() {
                if solids.contains_key(&key) {
                    let (x, y) = ((k as u32 % ATLAS_GRID) * tile_px, (k as u32 / ATLAS_GRID) * tile_px);
                    pass.set_viewport(x as f32, y as f32, tile_px as f32, tile_px as f32, 0.0, 1.0);
                    self.solid.draw_outline(&mut pass, key, k);
                    pass.set_pipeline(&self.occluder_pipeline);
                    continue;
                }
                let bind = match fx {
                    Some(i) => self.fx_targets.get(&key).map(|f| &f.binds[i]),
                    None => self.layers.get(&key).map(|t| &t.bind_group),
                };
                let Some(bind) = bind else { continue };
                let (x, y) = ((k as u32 % ATLAS_GRID) * tile_px, (k as u32 / ATLAS_GRID) * tile_px);
                pass.set_viewport(x as f32, y as f32, tile_px as f32, tile_px as f32, 0.0, 1.0);
                pass.set_bind_group(0, bind, &[]);
                pass.draw(0..3, 0..1);
            }
        }
        if !shaded.is_empty() {
            // The atlas has to be drawn before the text is shaded with it.
            let done = std::mem::replace(&mut enc, self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("composite") }));
            self.queue.submit([done.finish()]);
            let sh = solid::Shadows { casters: &occluder_floats, count: occluders.len(), atlas: &self.shadow_atlas_view };
            for key in &shaded {
                if let (Some(s), Some(t)) = (solids.get(key), self.layers.get(key)) {
                    let view = t.texture.create_view(&Default::default());
                    self.solid.draw_shadowed(&self.device, &self.queue, *key, s, &view, &sh);
                }
            }

            for (i, p) in fx_passes.iter().enumerate() {
                if !adjust.contains(&p.key) && shaded.contains(&p.key) {
                    encode_fx(&mut enc, i, p);
                }
            }
        }

        let offset = |slot: usize| (slot as u64 * self.uniform_stride) as u32;
        let is_solid = |key: u64| solids.contains_key(&key);
        // Vertices for a draw: the plain quad, or every little quad of a mesh.
        let verts: Vec<u32> = uniforms.iter().map(|u| u.3.map_or(6, |m| m.cols as u32 * m.rows as u32 * mesh_subdiv(m) * mesh_subdiv(m) * 6)).collect();
        // The texture to sample for a draw: the processed result, or the plain upload.
        let source = |d: &Draw| -> Option<&wgpu::BindGroup> {
            match d.fx {
                Some(i) => self.fx_targets.get(&d.key).map(|f| &f.binds[i]),
                None => self.layers.get(&d.key).map(|t| &t.bind_group),
            }
        };

        // Float layers hold premultiplied light: drawn like processed layers.
        let premul = |d: &Draw| d.fx.is_none() && self.layers.get(&d.key).is_some_and(|t| t.float);
        // The frame is drawn in runs of plain layers; each dissolve group needs
        // its own pass on the scratch target in between.
        let background = if self.transparent { wgpu::Color::TRANSPARENT } else { wgpu::Color::BLACK };
        let mut frame_pass = Some(pass_on(&mut enc, &targets.accum_view, wgpu::LoadOp::Clear(background)));
        // For a blend mode that reads the frame under the layer: copy the frame so far
        // (the pass drawing it has to end first). Returns whether to bind the copy.
        let copy_backdrop = |enc: &mut wgpu::CommandEncoder, blend: BlendMode| -> bool {
            if !blend.reads_backdrop() {
                return false;
            }
            let extent = wgpu::Extent3d { width: targets.size[0].max(1), height: targets.size[1].max(1), depth_or_array_layers: 1 };
            enc.copy_texture_to_texture(targets.accum.as_image_copy(), targets.backdrop.as_image_copy(), extent);
            true
        };
        // Consecutive 3D layers are drawn in one pass with a depth buffer (cleared
        // as the run starts), so where they cut through each other the nearer
        // one shows; still back to front, for what's see-through.
        let mut depth_run = false;
        let depth_pass = |enc: &mut wgpu::CommandEncoder| {
            enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("3D layers"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &targets.accum_view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Load, store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                    view: &targets.depth_view,
                    depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Discard }),
                    stencil_ops: None,
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            })
            .forget_lifetime()
        };
        for op in &ops {
            if !matches!(op, Op::Frame { .. }) && depth_run {
                drop(frame_pass.take());
                depth_run = false;
            }
            match op {
                Op::Frame { draw, blend } if (uniforms[draw.slot].4.is_some() || self.solid.depth_bind(draw.key).is_some() && is_solid(draw.key)) && !blend.reads_backdrop() => {
                    let Some(bind) = source(draw) else { continue };
                    let solid_depth = if uniforms[draw.slot].4.is_none() { self.solid.depth_bind(draw.key) } else { None };
                    if !depth_run {
                        drop(frame_pass.take());
                        frame_pass = Some(depth_pass(&mut enc));
                        depth_run = true;
                    }
                    let Some(pass) = frame_pass.as_mut() else { continue };
                    let pre = draw.fx.is_some() || premul(draw);
                    if let Some(d) = solid_depth {
                        pass.set_pipeline(if pre { &self.solid_group_pipelines_3d[blend] } else { &self.solid_pipelines_3d[blend] });
                        pass.set_bind_group(2, d, &[]);
                    } else {
                        pass.set_pipeline(if pre { &self.group_pipelines_3d[blend] } else { &self.layer_pipelines_3d[blend] });
                    }
                    pass.set_bind_group(0, &self.uniform_bind, &[offset(draw.slot)]);
                    pass.set_bind_group(1, bind, &[]);
                    pass.draw(0..verts[draw.slot], 0..1);
                }
                Op::Frame { draw, blend } => {
                    let Some(bind) = source(draw) else { continue };
                    let wrap = wraps.contains_key(&draw.slot);
                    if blend.reads_backdrop() || depth_run || wrap {
                        drop(frame_pass.take());
                        depth_run = false;
                    }
                    let read = if wrap {
                        let extent = wgpu::Extent3d { width: targets.size[0].max(1), height: targets.size[1].max(1), depth_or_array_layers: 1 };
                        enc.copy_texture_to_texture(targets.accum.as_image_copy(), targets.backdrop.as_image_copy(), extent);
                        true
                    } else {
                        copy_backdrop(&mut enc, *blend)
                    };
                    let pass = frame_pass.get_or_insert_with(|| pass_on(&mut enc, &targets.accum_view, wgpu::LoadOp::Load));
                    if read {
                        pass.set_bind_group(2, &targets.backdrop_bind, &[]);
                    }
                    // Processed layers are already premultiplied: draw them like a group.
                    let pre = draw.fx.is_some() || premul(draw);
                    pass.set_pipeline(match (wrap, pre) {
                        (true, true) => &self.wrap_group_pipelines[blend],
                        (true, false) => &self.wrap_pipelines[blend],
                        (false, true) => &self.group_pipelines[blend],
                        (false, false) => &self.layer_pipelines[blend],
                    });
                    pass.set_bind_group(0, &self.uniform_bind, &[offset(draw.slot)]);
                    pass.set_bind_group(1, bind, &[]);
                    pass.draw(0..verts[draw.slot], 0..1);
                }
                Op::Adjust { draw, blend } => {
                    // Finish the frame so far, run the effects on it, and draw the result over it.
                    drop(frame_pass.take());
                    for (i, p) in fx_passes.iter().enumerate().filter(|(_, p)| p.key == draw.key) {
                        encode_fx(&mut enc, i, p);
                    }
                    let Some(bind) = source(draw) else { continue };
                    let read = copy_backdrop(&mut enc, *blend);
                    let pass = frame_pass.insert(pass_on(&mut enc, &targets.accum_view, wgpu::LoadOp::Load));
                    if read {
                        pass.set_bind_group(2, &targets.backdrop_bind, &[]);
                    }
                    pass.set_pipeline(&self.group_pipelines[blend]);
                    pass.set_bind_group(0, &self.uniform_bind, &[offset(draw.slot)]);
                    pass.set_bind_group(1, bind, &[]);
                    pass.draw(0..6, 0..1);
                }
                Op::Matte { matte, layer, kind, over, blend } => {
                    drop(frame_pass.take());
                    depth_run = false;
                    for (d, view) in [(matte, &targets.matte_view), (layer, &targets.group_view)] {
                        let mut gp = pass_on(&mut enc, view, wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT));
                        let Some(bind) = source(d) else { continue };
                        gp.set_pipeline(if d.fx.is_some() || premul(d) { &self.sum_premul_pipeline } else { &self.sum_pipeline });
                        gp.set_bind_group(0, &self.uniform_bind, &[offset(d.slot)]);
                        gp.set_bind_group(1, bind, &[]);
                        gp.draw(0..verts[d.slot], 0..1);
                    }
                    {
                        let mut mp = pass_on(&mut enc, &targets.matted_view, wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT));
                        mp.set_pipeline(&self.matte_pipelines[*kind as usize]);
                        mp.set_bind_group(0, &targets.group_load, &[]);
                        mp.set_bind_group(1, &targets.matte_load, &[]);
                        mp.draw(0..3, 0..1);
                    }
                    let read = copy_backdrop(&mut enc, *blend);
                    let pass = frame_pass.insert(pass_on(&mut enc, &targets.accum_view, wgpu::LoadOp::Load));
                    if read {
                        pass.set_bind_group(2, &targets.backdrop_bind, &[]);
                    }
                    pass.set_pipeline(self.encoded_group_pipelines.get(blend).unwrap_or(&self.group_pipelines[blend]));
                    pass.set_bind_group(0, &self.uniform_bind, &[offset(*over)]);
                    pass.set_bind_group(1, &targets.matted_bind, &[]);
                    pass.draw(0..6, 0..1);
                }
                Op::Group { a, b, over, blend, clear } => {
                    drop(frame_pass.take());
                    {
                        let mut gp = pass_on(&mut enc, &targets.group_view, wgpu::LoadOp::Clear(*clear));
                        for d in [a, b].into_iter().flatten() {
                            let Some(bind) = source(d) else { continue };
                            gp.set_pipeline(if d.fx.is_some() || premul(d) { &self.sum_premul_pipeline } else { &self.sum_pipeline });
                            gp.set_bind_group(0, &self.uniform_bind, &[offset(d.slot)]);
                            gp.set_bind_group(1, bind, &[]);
                            gp.draw(0..verts[d.slot], 0..1);
                        }
                    }
                    let read = copy_backdrop(&mut enc, *blend);
                    let pass = frame_pass.insert(pass_on(&mut enc, &targets.accum_view, wgpu::LoadOp::Load));
                    if read {
                        pass.set_bind_group(2, &targets.backdrop_bind, &[]);
                    }
                    pass.set_pipeline(self.encoded_group_pipelines.get(blend).unwrap_or(&self.group_pipelines[blend]));
                    pass.set_bind_group(0, &self.uniform_bind, &[offset(*over)]);
                    pass.set_bind_group(1, &targets.group_bind, &[]);
                    pass.draw(0..6, 0..1);
                }
            }
        }
        drop(frame_pass);
        {
            let mut pass = pass_on(&mut enc, &targets.output_view, wgpu::LoadOp::Clear(background));
            pass.set_pipeline(if self.transparent { &self.output_alpha_pipeline } else { &self.output_pipeline });
            pass.set_bind_group(0, &targets.output_bind, &[]);
            pass.draw(0..3, 0..1);
        }
        self.queue.submit([enc.finish()]);
        // Shaded text's picture depends on the other layers: draw it afresh next time.
        for key in shaded {
            if let Some(t) = self.layers.get_mut(&key) {
                t.content_id = u64::MAX;
            }
        }
    }

    /// Drop cached textures for layers not in `keep`.
    pub fn retain_layers(&mut self, keep: impl Fn(u64) -> bool) {
        self.layers.retain(|k, _| keep(*k));
        self.yuv.retain(&keep);
        self.solid.retain(&keep);
        self.fx_targets.retain(|k, _| keep(*k));
        self.masks.retain(|k, _| keep(*k & !KEEP_MASK));
        self.tables.retain(|k, _| keep(k.0));
    }

    /// Read the last rendered frame back as tightly packed RGBA8 (sRGB). Blocks on the GPU.
    pub fn read_output(&self) -> Option<Vec<u8>> {
        let t = self.targets.as_ref()?;
        self.read_rgba8(&t.output, t.size)
    }

    /// Read the last rendered frame back as it was composited: half floats,
    /// linear light, premultiplied, nothing clipped (for compositions: their
    /// picture loses nothing on the way into the outer sequence). Blocks on the GPU.
    pub fn read_output_linear(&self) -> Option<reelforge_core::frame::FloatPixels> {
        let t = self.targets.as_ref()?;
        let raw = if self.formats.accum == ACCUM_FORMAT && !self.formats.gamma {
            self.read_texture(&t.accum, t.size, 8)?
        } else {
            // Another format, or display values: through the output pass to half floats.
            self.read_through(4)?
        };
        let rgba = raw.chunks_exact(2).map(|b| u16::from_le_bytes([b[0], b[1]])).collect();
        Some(reelforge_core::frame::FloatPixels { width: t.size[0], height: t.size[1], rgba })
    }

    /// The frame drawn by deep pipeline `i` into a half-float texture and read back
    /// (8 bytes a pixel, rows packed). Blocks on the GPU.
    fn read_through(&self, i: usize) -> Option<Vec<u8>> {
        let t = self.targets.as_ref()?;
        let [w, h] = t.size;
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("frame readout"),
            size: wgpu::Extent3d { width: w.max(1), height: h.max(1), depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: DEEP_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        let mut enc = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("frame readout"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT), store: wgpu::StoreOp::Store },
                })],
                ..Default::default()
            });
            pass.set_pipeline(self.deep_pipelines.get(i)?);
            pass.set_bind_group(0, &t.output_bind, &[]);
            pass.draw(0..3, 0..1);
        }
        self.queue.submit([enc.finish()]);
        self.read_texture(&texture, t.size, 8)
    }

    /// A layer texture as 8 bits per channel (a half-float one scaled to
    /// 0–255 linear: for comparing two conversions). Blocks on the GPU.
    fn read_layer8(&self, texture: &wgpu::Texture, size: [u32; 2], float: bool) -> Option<Vec<u8>> {
        if !float {
            return self.read_rgba8(texture, size);
        }
        let raw = self.read_texture(texture, size, 8)?;
        Some(raw.chunks_exact(2).map(|b| (reelforge_core::frame::f16_to_f32(u16::from_le_bytes([b[0], b[1]])).clamp(0.0, 1.0) * 255.0).round() as u8).collect())
    }

    /// Read the top-left `size` of an RGBA8 texture back to memory. Blocks on the GPU.
    fn read_rgba8(&self, texture: &wgpu::Texture, size: [u32; 2]) -> Option<Vec<u8>> {
        self.read_texture(texture, size, 4)
    }

    /// Read the top-left `size` of a texture of `bpp` bytes per pixel back to
    /// memory, rows tightly packed. Blocks on the GPU.
    fn read_texture(&self, texture: &wgpu::Texture, size: [u32; 2], bpp: u32) -> Option<Vec<u8>> {
        read_texture_bytes(&self.device, &self.queue, texture, size, bpp)
    }
}

/// Read the top-left `size` of a texture of `bpp` bytes per pixel back to
/// memory, rows tightly packed. Blocks on the GPU.
pub(crate) fn read_texture_bytes(device: &wgpu::Device, queue: &wgpu::Queue, texture: &wgpu::Texture, size: [u32; 2], bpp: u32) -> Option<Vec<u8>> {
    {
        let [w, h] = size;
        let row = bpp * w;
        let padded = row.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let buf = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: padded as u64 * h as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo { texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            wgpu::TexelCopyBufferInfo { buffer: &buf, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(padded), rows_per_image: Some(h) } },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        queue.submit([enc.finish()]);
        let slice = buf.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        device.poll(wgpu::PollType::wait_indefinitely()).ok()?;
        rx.recv().ok()?.ok()?;
        let mapped = slice.get_mapped_range().ok()?;
        let mut out = Vec::with_capacity((row * h) as usize);
        for y in 0..h as usize {
            out.extend_from_slice(&mapped[y * padded as usize..][..row as usize]);
        }
        drop(mapped);
        buf.unmap();
        Some(out)
    }
}

impl Compositor {
    /// Read the last rendered frame back as tightly packed 16-bit RGBA in
    /// `curve` (for 10-bit and HDR export). Blocks on the GPU.
    pub fn read_output_deep(&self, curve: DeepCurve) -> Option<Vec<u16>> {
        let t = self.targets.as_ref()?;
        let [w, h] = t.size;
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("deep output"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: DEEP_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let view = texture.create_view(&Default::default());
        let row = 8 * w;
        let padded = row.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("deep readback"),
            size: padded as u64 * h as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("deep output"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
                })],
                ..Default::default()
            });
            let i = match curve {
                // A transparent compositor keeps its coverage (SDR only).
                DeepCurve::Srgb if self.transparent => 3,
                DeepCurve::Srgb => 0,
                DeepCurve::Pq => 1,
                DeepCurve::Hlg => 2,
            };
            pass.set_pipeline(&self.deep_pipelines[i]);
            pass.set_bind_group(0, &t.output_bind, &[]);
            pass.draw(0..3, 0..1);
        }
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo { texture: &texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            wgpu::TexelCopyBufferInfo { buffer: &buf, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(padded), rows_per_image: Some(h) } },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        self.queue.submit([enc.finish()]);
        let slice = buf.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.device.poll(wgpu::PollType::wait_indefinitely()).ok()?;
        rx.recv().ok()?.ok()?;
        let mapped = slice.get_mapped_range().ok()?;
        let lut = half_to_unorm16();
        let mut out = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h as usize {
            for px in mapped[y * padded as usize..][..row as usize].chunks_exact(2) {
                out.push(lut[u16::from_le_bytes([px[0], px[1]]) as usize]);
            }
        }
        drop(mapped);
        buf.unmap();
        Some(out)
    }
}

/// Every half-float bit pattern clamped to 0..1 and scaled to 0..65535.
fn half_to_unorm16() -> &'static [u16] {
    static LUT: std::sync::OnceLock<Vec<u16>> = std::sync::OnceLock::new();
    LUT.get_or_init(|| (0..=u16::MAX).map(|h| (half_to_f32(h).clamp(0.0, 1.0) * 65535.0).round() as u16).collect())
}

/// IEEE 754 half → f32 (NaN reads as 0).
fn half_to_f32(h: u16) -> f32 {
    let sign = if h & 0x8000 != 0 { -1.0 } else { 1.0 };
    let exp = ((h >> 10) & 0x1f) as i32;
    let frac = (h & 0x3ff) as f32;
    match exp {
        0 => sign * frac * 2f32.powi(-24),
        31 => if frac == 0.0 { sign * f32::INFINITY } else { 0.0 },
        e => sign * (1.0 + frac / 1024.0) * 2f32.powi(e - 15),
    }
}

#[cfg(test)]
mod half_tests {
    use super::*;

    #[test]
    fn half_floats_decode() {
        assert_eq!(half_to_f32(0x3c00), 1.0);
        assert_eq!(half_to_f32(0x3800), 0.5);
        assert_eq!(half_to_f32(0xc000), -2.0);
        assert_eq!(half_to_f32(0x0001), 2f32.powi(-24));
        let lut = half_to_unorm16();
        assert_eq!((lut[0x3c00], lut[0x3800], lut[0xbc00], lut[0x4000]), (65535, 32768, 0, 65535));
    }
}

/// Effects' second tables are kept beside their first, by effect index with this bit set.
const TABLE2: usize = 1 << 20;

/// What draws the pictures: the graphics card, or the processor running
/// the same shaders in software (WARP on Windows, lavapipe or llvmpipe on
/// Linux), so a machine with a missing, old or misbehaving graphics card
/// still renders the same picture, only slower.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RenderDevice {
    #[default]
    Gpu,
    Cpu,
}

impl RenderDevice {
    pub fn label(self) -> &'static str {
        match self {
            RenderDevice::Gpu => "GPU",
            RenderDevice::Cpu => "CPU",
        }
    }
}

/// One of the renderers on this computer.
#[derive(Clone, Debug, serde::Serialize)]
pub struct RendererInfo {
    pub kind: RenderDevice,
    /// "NVIDIA GeForce RTX 4070 (Vulkan)", "Microsoft Basic Render Driver (Dx12)".
    pub name: String,
}

fn instance() -> wgpu::Instance {
    wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env())
}

fn describe(a: &wgpu::Adapter) -> String {
    let i = a.get_info();
    format!("{} ({:?}, {:?}; driver {} {})", i.name, i.backend, i.device_type, i.driver, i.driver_info).replace(" ; driver  )", ")").replace("; driver  )", ")")
}

fn short(a: &wgpu::Adapter) -> String {
    let i = a.get_info();
    format!("{} ({:?})", i.name, i.backend)
}

/// The adapter for `kind`: for the GPU the fastest graphics card (or, with
/// none, whatever there is); for the CPU a software one, or none.
fn adapter_for(kind: RenderDevice) -> Option<wgpu::Adapter> {
    let inst = instance();
    match kind {
        RenderDevice::Gpu => {
            let all = pollster::block_on(inst.enumerate_adapters(wgpu::Backends::all()));
            let rank = |a: &wgpu::Adapter| match a.get_info().device_type {
                wgpu::DeviceType::DiscreteGpu => 0,
                wgpu::DeviceType::IntegratedGpu => 1,
                wgpu::DeviceType::VirtualGpu => 2,
                wgpu::DeviceType::Other => 3,
                wgpu::DeviceType::Cpu => 4,
            };
            // The system's own choice first (it knows the backend that works), else the best kind.
            let preferred = pollster::block_on(inst.request_adapter(&wgpu::RequestAdapterOptions { power_preference: wgpu::PowerPreference::HighPerformance, ..Default::default() })).ok();
            match preferred {
                Some(a) if a.get_info().device_type != wgpu::DeviceType::Cpu || all.iter().all(|b| b.get_info().device_type == wgpu::DeviceType::Cpu) => Some(a),
                p => all.into_iter().filter(|a| a.get_info().device_type != wgpu::DeviceType::Cpu).min_by_key(rank).or(p),
            }
        }
        RenderDevice::Cpu => {
            let all = pollster::block_on(inst.enumerate_adapters(wgpu::Backends::all()));
            all.into_iter().find(|a| a.get_info().device_type == wgpu::DeviceType::Cpu).or_else(|| pollster::block_on(inst.request_adapter(&wgpu::RequestAdapterOptions { force_fallback_adapter: true, ..Default::default() })).ok().filter(|a| a.get_info().device_type == wgpu::DeviceType::Cpu))
        }
    }
}

/// The renderers this computer has (a graphics card, a software one), for
/// choosing between them.
pub fn renderers() -> Vec<RendererInfo> {
    [RenderDevice::Gpu, RenderDevice::Cpu]
        .into_iter()
        .filter_map(|kind| {
            let a = adapter_for(kind)?;
            // "GPU" that's really software (no graphics card) isn't a second choice.
            (kind == RenderDevice::Cpu || a.get_info().device_type != wgpu::DeviceType::Cpu).then(|| RendererInfo { kind, name: short(&a) })
        })
        .collect()
}

/// A device and queue on `kind`, without a window. None when this computer
/// hasn't one of that kind.
pub fn device_for(kind: RenderDevice) -> Option<(wgpu::Device, wgpu::Queue)> {
    let adapter = adapter_for(kind)?;
    let (d, q) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor { required_features: wanted_features(&adapter), ..Default::default() })).ok()?;
    if let Ok(mut g) = ADAPTER.lock() {
        *g = Some(describe(&adapter));
    }
    Some((d, q))
}

/// A device/queue without a window, for offline rendering and tests.
pub fn headless_device() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = instance();
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())).ok()?;
    let dev = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor { required_features: wanted_features(&adapter), ..Default::default() })).ok()?;
    if let Ok(mut g) = ADAPTER.lock() {
        if g.is_none() {
            *g = Some(describe(&adapter));
        }
    }
    Some(dev)
}

static ADAPTER: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// The graphics card (or software renderer) the compositor got last (name,
/// API, kind, driver), once one was opened.
pub fn adapter_info() -> Option<String> {
    ADAPTER.lock().ok().and_then(|g| g.clone())
}

/// The resolution a layer's effects and mask run at (see the call).
fn fx_resolution(pixel_size: [u32; 2], layer_size: [f32; 2]) -> [u32; 2] {
    if pixel_size[0] >= 16 || pixel_size[1] >= 16 {
        return pixel_size;
    }
    let side = |v: f32| (v.round() as u32).clamp(1, 4096);
    [side(layer_size[0]), side(layer_size[1])]
}

/// How far (in texels, x and y) a layer's effects spread past its edges:
/// a drop shadow by its distance and blur, a glow by its blur. Rounded up to
/// steps of 32 so an animated shadow doesn't remake its targets every frame,
/// and capped at the layer's size (and 2048).
fn effect_margin(effects: &[EffectValues], px: [u32; 2], layer: [f32; 2]) -> [u32; 2] {
    let (kx, ky) = (px[0] as f32 / layer[0].max(1.0), px[1] as f32 / layer[1].max(1.0));
    let (mut x, mut y) = (0.0f32, 0.0f32);
    for e in effects {
        match e.kind {
            // The streak reaches about 1.5 lengths each side.
            EffectKind::AnamorphicFlare => {
                use reelforge_core::effects::streak;
                x += 1.5 * e.values[streak::LENGTH].max(0.0) * kx;
                y += 1.5 * e.values[streak::THICKNESS].max(0.0) * ky;
            }
            EffectKind::CornerPin | EffectKind::PowerPin => {
                let (mut lo, mut hi) = ([0.0f32; 2], [100.0f32; 2]);
                for k in 0..4 {
                    for a in 0..2 {
                        let c = e.values[k * 2 + a];
                        if c.is_finite() {
                            lo[a] = lo[a].min(c);
                            hi[a] = hi[a].max(c);
                        }
                    }
                }
                x += layer[0] * ((-lo[0]).max(hi[0] - 100.0).max(0.0) / 100.0) * kx;
                y += layer[1] * ((-lo[1]).max(hi[1] - 100.0).max(0.0) / 100.0) * ky;
            }
            EffectKind::Warp => {
                x += layer[0] * 0.3 * kx;
                y += layer[1] * 0.3 * ky;
            }
            EffectKind::Bender => {
                let a = (e.values[reelforge_core::effects::bender::AMOUNT].abs() / 100.0).min(2.0);
                x += layer[0] * 0.5 * a * kx;
                y += layer[1] * 0.5 * a * ky;
            }
            EffectKind::Slant => {
                let t = e.values[reelforge_core::effects::slant::AMOUNT].to_radians().tan().abs().min(4.0);
                x += layer[1] * t * kx;
            }
            EffectKind::MotionTile => {
                use reelforge_core::effects::motion_tile as mt;
                x += layer[0] * ((e.values[mt::OUT_W] / 100.0 - 1.0).clamp(0.0, 3.0) * 0.5) * kx;
                y += layer[1] * ((e.values[mt::OUT_H] / 100.0 - 1.0).clamp(0.0, 3.0) * 0.5) * ky;
            }
            EffectKind::RadialShadow => {
                let d = (e.values[reelforge_core::effects::radial_shadow::DISTANCE] / 100.0).clamp(0.0, 4.0);
                x += layer[0] * 0.5 * d * kx + e.values[reelforge_core::effects::radial_shadow::SOFTNESS].max(0.0) * kx;
                y += layer[1] * 0.5 * d * ky + e.values[reelforge_core::effects::radial_shadow::SOFTNESS].max(0.0) * ky;
            }
            EffectKind::RepeTile => {
                use reelforge_core::effects::repe_tile as rt;
                let v = |i: usize| e.values[i].max(0.0);
                x += v(rt::LEFT).max(v(rt::RIGHT)) * kx;
                y += v(rt::UP).max(v(rt::DOWN)) * ky;
            }
            // Turned, bent or made solid, it can reach past its edges.
            EffectKind::Rotate3d => {
                let k = (e.values[reelforge_core::effects::rotate3d::SIZE] / 100.0).clamp(0.0, 4.0);
                x += layer[0] * (0.5 * k).max(0.25) * kx;
                y += layer[1] * (0.5 * k).max(0.25) * ky;
            }
            EffectKind::Extrusion3d => {
                use reelforge_core::effects::extrusion as ex;
                let k = (e.values[ex::SIZE] / 100.0).clamp(0.0, 4.0);
                let d = e.values[ex::DEPTH].max(0.0) * k;
                x += (layer[0] * 0.5 * k + d) * kx;
                y += (layer[1] * 0.5 * k + d) * ky;
            }
            EffectKind::Bend => {
                let k = (e.values[reelforge_core::effects::bend::SIZE] / 100.0).clamp(0.0, 4.0);
                x += layer[0] * 0.25 * k.max(1.0) * kx;
                y += layer[1] * 0.25 * k.max(1.0) * ky;
            }
            EffectKind::Extrude if e.values[reelforge_core::effects::extrude::TOWARDS] < 0.5 => {
                use reelforge_core::effects::extrude as xt;
                let (l, a) = (e.values[xt::LENGTH].max(0.0), e.values[xt::ANGLE].to_radians());
                x += (a.cos() * l).abs() * kx;
                y += (a.sin() * l).abs() * ky;
            }
            // The blur reaches 3 sigma, and sigma is half the radius (see `blur`).
            EffectKind::Glow => {
                x += 1.5 * e.values[1].max(0.0) * kx;
                y += 1.5 * e.values[1].max(0.0) * ky;
            }
            EffectKind::NeonEdges => {
                use reelforge_core::effects::neon;
                let r = 1.5 * e.values[neon::GLOW_RADIUS].max(0.0) + e.values[neon::WIDTH].max(0.0);
                x += r * kx;
                y += r * ky;
            }
            // A plugin says how far it reaches (its manifest's `spread`).
            EffectKind::Plugin => {
                if let Some(p) = &e.plugin {
                    let r = p.spread_px(&e.values);
                    x += r * kx;
                    y += r * ky;
                }
            }
            EffectKind::ElectricArcs => {
                use reelforge_core::effects::arcs;
                let r = e.values[arcs::REACH].max(0.0) * 1.2 + e.values[arcs::THICKNESS].max(0.0) * 4.0;
                x += r * kx;
                y += r * ky;
            }
            EffectKind::DirectionalBlur => {
                use reelforge_core::effects::dir_blur;
                let (l, a) = (e.values[dir_blur::LENGTH].max(0.0), e.values[dir_blur::ANGLE].to_radians());
                x += (a.cos() * l).abs() * kx;
                y += (a.sin() * l).abs() * ky;
            }
            EffectKind::RadialBlur => {
                use reelforge_core::effects::radial_blur;
                let amt = e.values[radial_blur::AMOUNT].abs();
                let reach = if e.values[radial_blur::TYPE] >= 0.5 { amt / 100.0 * 0.5 } else { (amt / 90.0).min(1.0) * 0.3 };
                x += reach * layer[0] * kx;
                y += reach * layer[1] * ky;
            }
            EffectKind::MotionBlur => {
                use reelforge_core::effects::motion_blur as mb;
                let v = &e.values;
                let mut reach = [0.0f32; 2];
                for m in [&v[mb::FROM..mb::FROM + 6], &v[mb::FROM + 6..mb::FROM + 12]] {
                    for q in [[-0.5f32, -0.5], [0.5, -0.5], [0.5, 0.5], [-0.5, 0.5]] {
                        let q = [q[0] * layer[0], q[1] * layer[1]];
                        let p = [m[0] * q[0] + m[2] * q[1] + m[4], m[1] * q[0] + m[3] * q[1] + m[5]];
                        reach = [reach[0].max((p[0] - q[0]).abs()), reach[1].max((p[1] - q[1]).abs())];
                    }
                }
                x += reach[0].min(4.0 * layer[0]) * kx;
                y += reach[1].min(4.0 * layer[1]) * ky;
            }
            EffectKind::PortraitBlur => {
                use reelforge_core::effects::portrait;
                let r = e.values[portrait::BLUR].max(0.0);
                x += r * kx;
                y += r * ky;
            }
            EffectKind::Gleam => {
                use reelforge_core::effects::gleam;
                let l = e.values[gleam::LENGTH].max(0.0);
                x += l * kx;
                y += l * ky;
            }
            EffectKind::Halation => {
                use reelforge_core::effects::halation;
                let s = e.values[halation::SIZE].max(0.0) * 1.5;
                x += s * kx;
                y += s * ky;
            }
            EffectKind::DropShadow => {
                let (d, a) = (e.values[0].max(0.0), e.values[1].to_radians());
                x += (a.cos() * d).abs() * kx + 1.5 * e.values[2].max(0.0) * kx;
                y += (a.sin() * d).abs() * ky + 1.5 * e.values[2].max(0.0) * ky;
            }
            _ => {}
        }
    }
    let step = |v: f32, size: u32| -> u32 {
        if v <= 0.5 {
            return 0;
        }
        let v = (v.ceil() as u32).min(size).min(2048);
        v.div_ceil(32) * 32
    };
    [step(x, px[0]), step(y, px[1])]
}
