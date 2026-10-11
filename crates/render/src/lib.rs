//! GPU compositor: blends transformed layers into a frame with wgpu.
//!
//! Independent of egui: the app hands it eframe's device/queue and shows the
//! output texture in the viewer; export (next phase) and the tests drive it
//! headless and read the pixels back.
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

pub mod text;

const ACCUM_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
const LAYER_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
/// Gamma-encoded 8-bit, which is what egui expects for user textures.
pub const OUTPUT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
/// Draw calls per frame (a dissolve uses three).
const MAX_DRAWS: usize = 128;
const UNIFORM_SIZE: u64 = 128; // mat4 + vec2 + 2×f32 + 3×vec4
/// Effect passes per frame.
const MAX_FX_PASSES: usize = 256;
const FX_UNIFORM_SIZE: u64 = 64; // 4 × vec4

/// One layer to draw.
#[derive(Clone)]
pub struct LayerInput<'a> {
    /// Stable identity (e.g. the clip id) for texture caching.
    pub key: u64,
    /// Changes whenever `pixels` changes (e.g. a hash of media/pts/generation).
    pub content_id: u64,
    /// Tightly packed RGBA8, sRGB-encoded.
    pub pixels: &'a [u8],
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
    targets: Option<Targets>,
    /// Bumped when the output texture is recreated (resize), so a UI can re-register it.
    pub output_generation: u64,
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
        let fx_pipelines = ["fs_import", "fs_color", "fs_blur", "fs_key", "fs_vignette", "fs_levels", "fs_sharpen", "fs_bright", "fs_shadow"]
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
        let output_pipeline_for = |entry: &str| {
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
                    targets: &[Some(wgpu::ColorTargetState { format: OUTPUT_FORMAT, blend: None, write_mask: wgpu::ColorWrites::ALL })],
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        let output_pipeline = output_pipeline_for("fs_output");
        let output_alpha_pipeline = output_pipeline_for("fs_output_alpha");

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
            targets: None,
            output_generation: 0,
        }
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
        self.targets = Some(Targets { size, accum_view, group_view, group_bind, output, output_view, output_bind });
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
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
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
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture: &t.texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            l.pixels,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(4 * w), rows_per_image: Some(h) },
            wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        );
        t.content_id = l.content_id;
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
            if l.pixels.len() as u64 == l.pixel_size[0] as u64 * l.pixel_size[1] as u64 * 4 && l.pixel_size[0] > 0 {
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
            uniform: [f32; 16],
        }
        let mut fx_passes: Vec<FxPass> = Vec::new();
        // Which scratch target holds each processed layer's result.
        let mut processed: HashMap<u64, usize> = HashMap::new();
        for l in all_layers() {
            let masked = l.mask.is_some() && self.masks.contains_key(&l.key);
            if (l.effects.is_empty() && !masked) || !self.layers.contains_key(&l.key) || processed.contains_key(&l.key) {
                continue;
            }
            let cost = |e: &EffectValues| match e.kind {
                EffectKind::Blur => 2,
                EffectKind::Glow | EffectKind::DropShadow => 4,
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
                let mut u = [0.0f32; 16];
                u[..8].copy_from_slice(v);
                u[8..12].copy_from_slice(&[1.0 / pw, 1.0 / ph, dir[0], dir[1]]);
                u[12] = aspect;
                u
            };
            let key = l.key;
            fx_passes.push(FxPass { key, entry: "fs_import", src: Src::Layer, second: Second::None, dst: 0, uniform: [0.0; 16] });
            let mut cur = 0usize;
            let mut push = |entry, src: usize, second, dst: usize, u| fx_passes.push(FxPass { key, entry, src: Src::Scratch(src), second, dst, uniform: u });
            // The two targets that aren't `cur`.
            let others = |cur: usize| ((cur + 1) % 3, (cur + 2) % 3);
            // A separable blur of `from` into `into`, using `tmp`.
            let blur = |push: &mut dyn FnMut(&'static str, usize, Second, usize, [f32; 16]), radius: f32, from: usize, tmp: usize, into: usize| {
                let mut vx = [0.0; reelforge_core::effects::MAX_PARAMS];
                vx[0] = radius * 0.5 * kx;
                let mut vy = vx;
                vy[0] = radius * 0.5 * ky;
                push("fs_blur", from, Second::None, tmp, uniform(&vx, [1.0, 0.0]));
                push("fs_blur", tmp, Second::None, into, uniform(&vy, [0.0, 1.0]));
            };
            if masked {
                let (o, _) = others(cur);
                push("fs_mask", cur, Second::Mask, o, [0.0; 16]);
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
                }
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
            /// Draw a dissolve group: sum the layers into the group target (cleared
            /// to `clear`, premultiplied, for a dip), then composite the group into the frame.
            Group { a: Option<Draw>, b: Option<Draw>, over: usize, blend: BlendMode, clear: wgpu::Color },
        }
        let mut uniforms: Vec<(Transform, [f32; 2], Option<Wipe>)> = Vec::new();
        fn slot(u: &mut Vec<(Transform, [f32; 2], Option<Wipe>)>, t: Transform, size: [f32; 2], wipe: Option<Wipe>) -> usize {
            u.push((t, size, wipe));
            u.len() - 1
        }
        let mut ops = Vec::new();
        for item in items {
            match item {
                DrawItem::Layer(l) => {
                    let draw = Draw { slot: slot(&mut uniforms, l.transform, l.layer_size, l.wipe), key: l.key, fx: processed.get(&l.key).copied() };
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
                    let a = draw_a.then(|| Draw { slot: slot(&mut uniforms, ta, from.layer_size, from.wipe), key: from.key, fx: processed.get(&from.key).copied() });
                    let b = draw_b.then(|| Draw { slot: slot(&mut uniforms, tb, to.layer_size, to.wipe), key: to.key, fx: processed.get(&to.key).copied() });
                    let over = slot(&mut uniforms, Transform::default(), frame, None);
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
        for (i, (t, size, wipe)) in uniforms.iter().enumerate() {
            // (Corner-pinned layers get a perspective matrix.)
            let mvp = t.layer_map(*size, frame).to_clip_space(frame);
            let mut floats: Vec<f32> = mvp.iter().flatten().copied().collect();
            let mode = wipe.map_or(0.0, |w| if w.iris { 2.0 } else { 1.0 });
            floats.extend([size[0], size[1], t.opacity.clamp(0.0, 1.0), mode]);
            let w = wipe.unwrap_or(Wipe { iris: false, axis: [0.0; 3], progress: 1.0, softness: 0.1 });
            floats.extend([w.axis[0], w.axis[1], w.axis[2], w.progress.clamp(0.0, 1.0)]);
            floats.extend([w.softness, frame[0], frame[1], 0.0]);
            floats.extend(t.crop_uv());
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

        // Effects first: each pass is its own render pass on a layer's scratch target.
        for (i, p) in fx_passes.iter().enumerate() {
            let (Some(fx), Some(tex)) = (self.fx_targets.get(&p.key), self.layers.get(&p.key)) else { continue };
            let second = match p.second {
                Second::None => None,
                Second::Mask => Some(self.masks.get(&p.key).map(|m| &m.bind_group)),
                Second::Table(e) => Some(self.tables.get(&(p.key, e)).map(|t| &t.bind_group)),
                Second::Scratch(s) => Some(Some(&fx.binds[s])),
            };
            if matches!(second, Some(None)) {
                continue; // its texture is missing
            }
            let mut pass = pass_on(&mut enc, &fx.views[p.dst], wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT));
            match second {
                Some(Some(b)) => {
                    pass.set_pipeline(&self.fx2_pipelines[p.entry]);
                    pass.set_bind_group(2, b, &[]);
                }
                _ => pass.set_pipeline(&self.fx_pipelines[p.entry]),
            }
            pass.set_bind_group(0, &self.fx_uniform_bind, &[(i as u64 * self.fx_uniform_stride) as u32]);
            match p.src {
                Src::Layer => pass.set_bind_group(1, &tex.bind_group, &[]),
                Src::Scratch(s) => pass.set_bind_group(1, &fx.binds[s], &[]),
            }
            pass.draw(0..3, 0..1);
        }

        let offset = |slot: usize| (slot as u64 * self.uniform_stride) as u32;
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
                            gp.draw(0..6, 0..1);
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
        self.fx_targets.retain(|k, _| keep(*k));
        self.masks.retain(|k, _| keep(*k));
        self.tables.retain(|k, _| keep(k.0));
    }

    /// Read the last rendered frame back as tightly packed RGBA8 (sRGB). Blocks on the GPU.
    pub fn read_output(&self) -> Option<Vec<u8>> {
        let t = self.targets.as_ref()?;
        let [w, h] = t.size;
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
            wgpu::TexelCopyTextureInfo { texture: &t.output, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
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

/// A device/queue without a window, for offline rendering and tests.
pub fn headless_device() -> Option<(wgpu::Device, wgpu::Queue)> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())).ok()?;
    pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).ok()
}
