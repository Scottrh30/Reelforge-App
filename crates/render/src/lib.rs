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
    /// An adjustment layer: the frame composited so far (everything below),
    /// put through the layer's effects and drawn back over it. `pixel_size`
    /// should be the frame size.
    Below,
}

impl PixelData<'_> {
    /// The data really is a picture of `size` (and not empty).
    fn matches(&self, size: [u32; 2]) -> bool {
        size[0] > 0
            && match self {
                PixelData::Rgba(px) => px.len() as u64 == size[0] as u64 * size[1] as u64 * 4,
                PixelData::Yuv(y) => [y.width, y.height] == size && y.planes.len() == if y.layout.interleaved() { 2 } else { 3 },
                PixelData::Gpu(g) => [g.width, g.height] == size,
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
        }
    }
}

pub mod plugin;
pub mod text;

const ACCUM_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
const LAYER_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
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
const UNIFORM_SIZE: u64 = 128 + 16 + 41 * 16; // mat4 + vec2 + 2×f32 + 3×vec4, then a mesh header and 41 vec4 of points
/// Little quads per mesh cell, across and down (a cell is bilinear; this many
/// straight-edged pieces draw it smoothly).
const MESH_SUBDIV: u32 = 6;
/// Effect passes per frame.
const MAX_FX_PASSES: usize = 256;
/// 8 × vec4: params 0–7, texel/direction, extra (aspect, time, layer size),
/// params 8–15, extra2 (texture size, pass, pass count), spare. Built-in
/// effect shaders only declare the first four; plugins see them all.
const FX_UNIFORM_SIZE: u64 = 128;
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
    /// Changes whenever `pixels` changes.
    pub content_id: u64,
    pub pixels: &'a [u8],
    pub size: [u32; 2],
}

/// Something to draw, bottom first.
// Built per frame and consumed at once, so the size difference doesn't matter.
#[allow(clippy::large_enum_variant)]
pub enum DrawItem<'a> {
    Layer(LayerInput<'a>),
    /// Cross-dissolve from `from` to `to`; `mix` 0 → 1. The pair is composited
    /// with `to`'s blend mode. With `dip`, it fades out to that colour (linear
    /// RGB) and in from it instead.
    Dissolve { from: LayerInput<'a>, to: LayerInput<'a>, mix: f32, dip: Option<[f32; 3]> },
}

struct LayerTex {
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    size: [u32; 2],
    content_id: u64,
}

/// Two scratch targets the size of a layer's pixels, for its effect chain.
/// Three scratch targets the size of a layer's pixels, for its effect chain
/// (two for ping-pong, one more to keep the layer while glow or a shadow is built).
struct FxTargets {
    size: [u32; 2],
    views: [wgpu::TextureView; 3],
    binds: [wgpu::BindGroup; 3],
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
}

pub struct Compositor {
    device: wgpu::Device,
    queue: wgpu::Queue,
    layer_pipelines: HashMap<BlendMode, wgpu::RenderPipeline>,
    /// Draws a premultiplied group texture over the frame, per blend mode.
    group_pipelines: HashMap<BlendMode, wgpu::RenderPipeline>,
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
    /// 16-bit export outputs: sRGB, PQ, HLG.
    deep_pipelines: [wgpu::RenderPipeline; 3],
    /// Render over transparency and keep the alpha in the output (for composite
    /// shots, which are layered again), instead of over opaque black.
    pub transparent: bool,
    texture_layout: wgpu::BindGroupLayout,
    output_layout: wgpu::BindGroupLayout,
    uniforms: wgpu::Buffer,
    uniform_bind: wgpu::BindGroup,
    uniform_stride: u64,
    sampler: wgpu::Sampler,
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
    };
    wgpu::BlendState { color, alpha: comp(F::One, F::OneMinusSrcAlpha) }
}

impl Compositor {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let layer_shader = device.create_shader_module(wgpu::include_wgsl!("composite.wgsl"));
        let output_shader = device.create_shader_module(wgpu::include_wgsl!("output.wgsl"));

        let align = device.limits().min_uniform_buffer_offset_alignment as u64;
        let uniform_stride = UNIFORM_SIZE.div_ceil(align) * align;

        let uniform_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("layer uniforms"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: true,
                    min_binding_size: wgpu::BufferSize::new(UNIFORM_SIZE),
                },
                count: None,
            }],
        });
        let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("layer texture"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
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
        let make = |label: &str, entry: &str, blend: wgpu::BlendState| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(&layer_pl),
                vertex: wgpu::VertexState {
                    module: &layer_shader,
                    entry_point: Some("vs_layer"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &layer_shader,
                    entry_point: Some(entry),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState { format: ACCUM_FORMAT, blend: Some(blend), write_mask: wgpu::ColorWrites::ALL })],
                }),
                primitive: wgpu::PrimitiveState::default(), // no culling: flips/negative scale are fine
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        let layer_pipelines = BlendMode::ALL.into_iter().map(|m| (m, make(m.label(), "fs_layer", blend_state(m)))).collect();
        let group_pipelines = BlendMode::ALL.into_iter().map(|m| (m, make(m.label(), "fs_layer_premul", blend_state(m)))).collect();
        let sum = wgpu::BlendComponent { src_factor: wgpu::BlendFactor::One, dst_factor: wgpu::BlendFactor::One, operation: wgpu::BlendOperation::Add };
        let sum_pipeline = make("dissolve sum", "fs_layer", wgpu::BlendState { color: sum, alpha: sum });
        let sum_premul_pipeline = make("dissolve sum (premultiplied)", "fs_layer_premul", wgpu::BlendState { color: sum, alpha: sum });

        // Effect passes.
        let fx_shader = device.create_shader_module(wgpu::include_wgsl!("effects.wgsl"));
        let fx_uniform_stride = FX_UNIFORM_SIZE.div_ceil(align) * align;
        let fx_uniform_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("effect uniforms"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
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
        let fx2_pipelines = ["fs_mask", "fs_curves", "fs_lut", "fs_glow_add", "fs_under"]
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
                        targets: &[Some(wgpu::ColorTargetState { format: ACCUM_FORMAT, blend: None, write_mask: wgpu::ColorWrites::ALL })],
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
        let fx_pipelines = ["fs_import", "fs_color", "fs_blur", "fs_key", "fs_vignette", "fs_levels", "fs_sharpen", "fs_bright", "fs_shadow", "fs_blade", "fs_copy"]
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
                        targets: &[Some(wgpu::ColorTargetState { format: ACCUM_FORMAT, blend: None, write_mask: wgpu::ColorWrites::ALL })],
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
                    compilation_options: Default::default(),
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
        let deep_pipelines = ["fs_deep_srgb", "fs_deep_pq", "fs_deep_hlg"].map(|e| output_pipeline_with(e, DEEP_FORMAT));

        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("layer uniforms"),
            size: uniform_stride * MAX_DRAWS as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let uniform_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("layer uniforms"),
            layout: &uniform_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                    buffer: &uniforms,
                    offset: 0,
                    size: wgpu::BufferSize::new(UNIFORM_SIZE),
                }),
            }],
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("layer"),
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        Self {
            device: device.clone(),
            queue: queue.clone(),
            layer_pipelines,
            group_pipelines,
            sum_pipeline,
            sum_premul_pipeline,
            fx_pipelines,
            fx_uniforms,
            fx_uniform_bind,
            fx_uniform_stride,
            fx_targets: HashMap::new(),
            output_pipeline,
            output_alpha_pipeline,
            deep_pipelines,
            yuv: yuv::YuvConverter::new(device, LAYER_FORMAT),
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
            texture_layout,
            output_layout,
            uniforms,
            uniform_bind,
            uniform_stride,
            sampler,
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
                                targets: &[Some(wgpu::ColorTargetState { format: ACCUM_FORMAT, blend: None, write_mask: wgpu::ColorWrites::ALL })],
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
        let accum = tex("accum", ACCUM_FORMAT, U::RENDER_ATTACHMENT | U::TEXTURE_BINDING);
        let group = tex("dissolve group", ACCUM_FORMAT, U::RENDER_ATTACHMENT | U::TEXTURE_BINDING);
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
        self.targets = Some(Targets { size, accum_view, group_view, group_bind, output, output_view, output_bind, accum_bind });
        self.output_generation += 1;
    }

    fn upload(&mut self, l: &LayerInput) {
        let [w, h] = l.pixel_size;
        if let Some(t) = self.layers.get(&l.key) {
            if t.size == l.pixel_size && t.content_id == l.content_id {
                return; // unchanged since last frame
            }
        }
        let needs_new = self.layers.get(&l.key).is_none_or(|t| t.size != l.pixel_size);
        if needs_new {
            let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("layer"),
                size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: LAYER_FORMAT,
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
            self.layers.insert(l.key, LayerTex { texture, bind_group, size: l.pixel_size, content_id: u64::MAX });
        }
        let t = self.layers.get_mut(&l.key).unwrap();
        match l.pixels {
            PixelData::Below => {}
            PixelData::Rgba(px) => self.queue.write_texture(
                wgpu::TexelCopyTextureInfo { texture: &t.texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
                px,
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4 * w), rows_per_image: Some(h) },
                wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            ),
            PixelData::Yuv(planes) => {
                let view = t.texture.create_view(&Default::default());
                self.yuv.convert(&self.device, &self.queue, l.key, planes, &view);
            }
            PixelData::Gpu(g) => {
                let texture = t.texture.clone();
                self.draw_gpu_frame(l.key, g, &texture);
            }
        }
        if let Some(t) = self.layers.get_mut(&l.key) {
            t.content_id = l.content_id;
        }
    }

    /// The layer texture is filled from a frame on the graphics card: read
    /// straight from the shared texture when that works, otherwise (other
    /// platforms, zero-copy off or broken) copied back and converted as usual.
    fn draw_gpu_frame(&mut self, key: u64, g: &GpuFrame, texture: &wgpu::Texture) {
        use reelforge_core::frame::zero_copy;
        let view = texture.create_view(&Default::default());
        if zero_copy::enabled() && zero_copy::failure().is_none() {
            match self.convert_shared(g, &view) {
                // This compositor can't read shared frames: just copy it back.
                None => {}
                Some(Ok(())) => {
                    // The first frame of each kind: compare with the usual copy.
                    if let Some(check) = g.check.as_ref().filter(|_| !self.gpu_checked.contains(&g.layout)) {
                        let shared = self.read_rgba8(texture, [g.width, g.height]);
                        self.yuv.convert(&self.device, &self.queue, key, check, &view);
                        let copied = self.read_rgba8(texture, [g.width, g.height]);
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
            self.yuv.convert(&self.device, &self.queue, key, &y, &view);
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
            self.masks.insert(key, LayerTex { texture, bind_group, size: m.size, content_id: u64::MAX });
        }
        let t = self.masks.get_mut(&key).unwrap();
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
        self.tables.insert(key, LayerTex { texture, bind_group, size: [t.width, t.height], content_id: t.id });
    }

    fn ensure_fx_targets(&mut self, key: u64, size: [u32; 2]) {
        if self.fx_targets.get(&key).is_some_and(|t| t.size == size) {
            return;
        }
        let make = || {
            let tex = self.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("effect scratch"),
                size: wgpu::Extent3d { width: size[0].max(1), height: size[1].max(1), depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: ACCUM_FORMAT,
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
        let (va, ba) = make();
        let (vb, bb) = make();
        let (vc, bc) = make();
        self.fx_targets.insert(key, FxTargets { size, views: [va, vb, vc], binds: [ba, bb, bc] });
    }

    /// Composite plain `layers` (bottom first). See [`Self::render_items`].
    pub fn render(&mut self, frame_size: [u32; 2], layers: &[LayerInput]) {
        let items: Vec<DrawItem> = layers.iter().map(|l| DrawItem::Layer(l.clone())).collect();
        self.render_items(frame_size, &items);
    }

    /// Composite `items` (bottom first) into a `frame_size` frame and submit the GPU work.
    pub fn render_items(&mut self, frame_size: [u32; 2], items: &[DrawItem]) {
        self.ensure_targets(frame_size);
        let frame = [frame_size[0] as f32, frame_size[1] as f32];
        let all_layers = || {
            items.iter().flat_map(|item| match item {
                DrawItem::Layer(l) => vec![l],
                DrawItem::Dissolve { from, to, .. } => vec![from, to],
            })
        };

        for l in all_layers() {
            if l.pixels.matches(l.pixel_size) {
                self.upload(l);
            }
            if let Some(m) = l.mask.filter(|m| m.pixels.len() as u64 == m.size[0] as u64 * m.size[1] as u64 && m.size[0] > 0) {
                self.upload_mask(l.key, &m);
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
            Table(usize),
            Scratch(usize),
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
        }
        let mut fx_passes: Vec<FxPass> = Vec::new();
        // Which scratch target holds each processed layer's result.
        let mut processed: HashMap<u64, usize> = HashMap::new();
        for l in all_layers() {
            let masked = l.mask.is_some() && self.masks.contains_key(&l.key);
            let below = matches!(l.pixels, PixelData::Below);
            if (l.effects.is_empty() && !masked) || !(below || self.layers.contains_key(&l.key)) || processed.contains_key(&l.key) {
                continue;
            }
            let cost = |e: &EffectValues| match e.kind {
                EffectKind::Blur => 2,
                EffectKind::Glow | EffectKind::DropShadow => 4,
                EffectKind::Plugin => e.plugin.as_ref().map_or(0, |p| p.passes.len()),
                _ => 1,
            };
            let needed = 1 + masked as usize + l.effects.iter().map(cost).sum::<usize>();
            if fx_passes.len() + needed > MAX_FX_PASSES {
                log::warn!("too many effect passes; skipping effects on some layers");
                break;
            }
            self.ensure_fx_targets(l.key, l.pixel_size);
            for (i, e) in l.effects.iter().enumerate() {
                if let Some(t) = &e.table {
                    self.upload_table((l.key, i), t);
                }
            }
            let [pw, ph] = [l.pixel_size[0].max(1) as f32, l.pixel_size[1].max(1) as f32];
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
            let (entry, src) = if below { ("fs_copy", Src::Below) } else { ("fs_import", Src::Layer) };
            fx_passes.push(FxPass { key, entry, src, second: Second::None, dst: 0, uniform: [0.0; FX_FLOATS], plugin: None });
            let mut cur = 0usize;
            // Plugin passes pushed for this layer: (index in fx_passes, revision, pass).
            let mut plugin_marks: Vec<(usize, u64, usize)> = Vec::new();
            let mut push = |entry, src: usize, second, dst: usize, u| -> usize {
                fx_passes.push(FxPass { key, entry, src: Src::Scratch(src), second, dst, uniform: u, plugin: None });
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
            if masked {
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
                    EffectKind::ColorCorrect => {
                        push("fs_color", cur, Second::None, o1, uniform(&e.values, [0.0; 2]));
                        cur = o1;
                    }
                    EffectKind::ChromaKey => {
                        push("fs_key", cur, Second::None, o1, uniform(&e.values, [0.0; 2]));
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
                    // Camera and light settings are never drawn (they're filtered out earlier).
                    EffectKind::Camera | EffectKind::Light | EffectKind::Point => {}
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
            processed.insert(l.key, cur);
        }
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
        }
        type Uniform<'m> = (Transform, [f32; 2], Option<Wipe>, Option<&'m reelforge_core::meshwarp::MeshShape>, Option<&'m reelforge_core::space3d::LayerSpace>);
        let mut uniforms: Vec<Uniform> = Vec::new();
        fn slot<'m>(u: &mut Vec<Uniform<'m>>, l: (Transform, [f32; 2], Option<Wipe>), mesh: Option<&'m reelforge_core::meshwarp::MeshShape>, space: Option<&'m reelforge_core::space3d::LayerSpace>) -> usize {
            let mesh = mesh.filter(|m| m.is_valid());
            u.push((l.0, l.1, l.2, mesh, space.filter(|_| mesh.is_none())));
            u.len() - 1
        }
        let mut ops = Vec::new();
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
                    let over = slot(&mut uniforms, (Transform::default(), frame, None), None, None);
                    ops.push(Op::Group { a, b, over, blend: to.blend, clear });
                }
            }
            if uniforms.len() > MAX_DRAWS - 3 {
                log::warn!("too many layers; dropping the rest");
                break;
            }
        }

        // Uniforms for every draw in one buffer, addressed by dynamic offset.
        let mut data = vec![0u8; self.uniform_stride as usize * uniforms.len().max(1)];
        for (i, (t, size, wipe, mesh, space)) in uniforms.iter().enumerate() {
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
            floats.extend([w.softness, frame[0], frame[1], if lighting.is_some() { 1.0 } else { 0.0 }]);
            floats.extend(t.crop_uv());
            match mesh {
                Some(m) => {
                    floats.extend([m.cols as f32, m.rows as f32, 1.0, MESH_SUBDIV as f32]);
                    let mut pts: Vec<f32> = m.points.iter().flatten().copied().collect();
                    pts.resize(41 * 4, 0.0);
                    floats.extend(pts);
                }
                None => {
                    floats.extend([0.0; 4]);
                    // A lit 3D layer: its plane, the camera and the lights where the mesh would go.
                    if let Some(l) = lighting {
                        let n = l.lights.len().min(reelforge_core::space3d::MAX_LIGHTS);
                        floats.extend([l.origin[0], l.origin[1], l.origin[2], n as f32]);
                        floats.extend([l.u[0], l.u[1], l.u[2], 0.0]);
                        floats.extend([l.v[0], l.v[1], l.v[2], 0.0]);
                        floats.extend([l.camera[0], l.camera[1], l.camera[2], 0.0]);
                        for g in &l.lights[..n] {
                            floats.extend([g.position[0], g.position[1], g.position[2], g.kind as f32]);
                            floats.extend([g.direction[0], g.direction[1], g.direction[2], g.falloff as f32]);
                            floats.extend([g.color[0], g.color[1], g.color[2], g.radius]);
                            floats.extend([g.cone[0], g.cone[1], 0.0, 0.0]);
                        }
                    }
                }
            }
            let bytes: Vec<u8> = floats.iter().flat_map(|f| f.to_ne_bytes()).collect();
            let off = i * self.uniform_stride as usize;
            data[off..off + bytes.len()].copy_from_slice(&bytes);
        }
        self.queue.write_buffer(&self.uniforms, 0, &data);

        let targets = self.targets.as_ref().unwrap();
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
        let encode_fx = |enc: &mut wgpu::CommandEncoder, i: usize, p: &FxPass| {
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
                Second::Table(e) => Some(self.tables.get(&(p.key, e)).map(|t| &t.bind_group)),
                Second::Scratch(s) => Some(Some(&fx.binds[s])),
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
        };
        // Adjustment layers' passes wait for the frame below them to be drawn.
        let adjust: std::collections::HashSet<u64> = ops.iter().filter_map(|o| if let Op::Adjust { draw, .. } = o { Some(draw.key) } else { None }).collect();
        for (i, p) in fx_passes.iter().enumerate() {
            if !adjust.contains(&p.key) {
                encode_fx(&mut enc, i, p);
            }
        }

        let offset = |slot: usize| (slot as u64 * self.uniform_stride) as u32;
        // Vertices for a draw: the plain quad, or every little quad of a mesh.
        let verts: Vec<u32> = uniforms.iter().map(|u| u.3.map_or(6, |m| m.cols as u32 * m.rows as u32 * MESH_SUBDIV * MESH_SUBDIV * 6)).collect();
        // The texture to sample for a draw: the processed result, or the plain upload.
        let source = |d: &Draw| -> Option<&wgpu::BindGroup> {
            match d.fx {
                Some(i) => self.fx_targets.get(&d.key).map(|f| &f.binds[i]),
                None => self.layers.get(&d.key).map(|t| &t.bind_group),
            }
        };

        // The frame is drawn in runs of plain layers; each dissolve group needs
        // its own pass on the scratch target in between.
        let background = if self.transparent { wgpu::Color::TRANSPARENT } else { wgpu::Color::BLACK };
        let mut frame_pass = Some(pass_on(&mut enc, &targets.accum_view, wgpu::LoadOp::Clear(background)));
        for op in &ops {
            match op {
                Op::Frame { draw, blend } => {
                    let Some(bind) = source(draw) else { continue };
                    let pass = frame_pass.get_or_insert_with(|| pass_on(&mut enc, &targets.accum_view, wgpu::LoadOp::Load));
                    // Processed layers are already premultiplied: draw them like a group.
                    pass.set_pipeline(if draw.fx.is_some() { &self.group_pipelines[blend] } else { &self.layer_pipelines[blend] });
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
                    let pass = frame_pass.insert(pass_on(&mut enc, &targets.accum_view, wgpu::LoadOp::Load));
                    pass.set_pipeline(&self.group_pipelines[blend]);
                    pass.set_bind_group(0, &self.uniform_bind, &[offset(draw.slot)]);
                    pass.set_bind_group(1, bind, &[]);
                    pass.draw(0..6, 0..1);
                }
                Op::Group { a, b, over, blend, clear } => {
                    drop(frame_pass.take());
                    {
                        let mut gp = pass_on(&mut enc, &targets.group_view, wgpu::LoadOp::Clear(*clear));
                        for d in [a, b].into_iter().flatten() {
                            let Some(bind) = source(d) else { continue };
                            gp.set_pipeline(if d.fx.is_some() { &self.sum_premul_pipeline } else { &self.sum_pipeline });
                            gp.set_bind_group(0, &self.uniform_bind, &[offset(d.slot)]);
                            gp.set_bind_group(1, bind, &[]);
                            gp.draw(0..verts[d.slot], 0..1);
                        }
                    }
                    let pass = frame_pass.insert(pass_on(&mut enc, &targets.accum_view, wgpu::LoadOp::Load));
                    pass.set_pipeline(&self.group_pipelines[blend]);
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
    }

    /// Drop cached textures for layers not in `keep`.
    pub fn retain_layers(&mut self, keep: impl Fn(u64) -> bool) {
        self.layers.retain(|k, _| keep(*k));
        self.yuv.retain(&keep);
        self.fx_targets.retain(|k, _| keep(*k));
        self.masks.retain(|k, _| keep(*k));
        self.tables.retain(|k, _| keep(k.0));
    }

    /// Read the last rendered frame back as tightly packed RGBA8 (sRGB). Blocks on the GPU.
    pub fn read_output(&self) -> Option<Vec<u8>> {
        let t = self.targets.as_ref()?;
        self.read_rgba8(&t.output, t.size)
    }

    /// Read the top-left `size` of an RGBA8 texture back to memory. Blocks on the GPU.
    fn read_rgba8(&self, texture: &wgpu::Texture, size: [u32; 2]) -> Option<Vec<u8>> {
        let [w, h] = size;
        let row = 4 * w;
        let padded = row.div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT) * wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let buf = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("readback"),
            size: padded as u64 * h as u64,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });
        let mut enc = self.device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo { texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
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

/// A device/queue without a window, for offline rendering and tests.
pub fn headless_device() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())).ok()?;
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()
}
