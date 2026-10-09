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

use reelforge_core::{BlendMode, Transform};
use std::collections::HashMap;

pub use wgpu;

const ACCUM_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba16Float;
const LAYER_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;
/// Gamma-encoded 8-bit, which is what egui expects for user textures.
pub const OUTPUT_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;
/// Draw calls per frame (a dissolve uses three).
const MAX_DRAWS: usize = 128;
const UNIFORM_SIZE: u64 = 80; // mat4 + vec2 + f32 + pad

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
}

/// Something to draw, bottom first.
pub enum DrawItem<'a> {
    Layer(LayerInput<'a>),
    /// Cross-dissolve from `from` to `to`; `mix` 0 → 1. The pair is composited
    /// with `to`'s blend mode.
    Dissolve { from: LayerInput<'a>, to: LayerInput<'a>, mix: f32 },
}

struct LayerTex {
    texture: wgpu::Texture,
    bind_group: wgpu::BindGroup,
    size: [u32; 2],
    content_id: u64,
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
    output_pipeline: wgpu::RenderPipeline,
    texture_layout: wgpu::BindGroupLayout,
    output_layout: wgpu::BindGroupLayout,
    uniforms: wgpu::Buffer,
    uniform_bind: wgpu::BindGroup,
    uniform_stride: u64,
    sampler: wgpu::Sampler,
    layers: HashMap<u64, LayerTex>,
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

        let output_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("output"),
            bind_group_layouts: &[Some(&output_layout)],
            immediate_size: 0,
        });
        let output_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("output"),
            layout: Some(&output_pl),
            vertex: wgpu::VertexState {
                module: &output_shader,
                entry_point: Some("vs_fullscreen"),
                compilation_options: Default::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &output_shader,
                entry_point: Some("fs_output"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState { format: OUTPUT_FORMAT, blend: None, write_mask: wgpu::ColorWrites::ALL })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
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
            output_pipeline,
            texture_layout,
            output_layout,
            uniforms,
            uniform_bind,
            uniform_stride,
            sampler,
            layers: HashMap::new(),
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

    /// Composite plain `layers` (bottom first). See [`Self::render_items`].
    pub fn render(&mut self, frame_size: [u32; 2], layers: &[LayerInput]) {
        let items: Vec<DrawItem> = layers.iter().map(|l| DrawItem::Layer(l.clone())).collect();
        self.render_items(frame_size, &items);
    }

    /// Composite `items` (bottom first) into a `frame_size` frame and submit the GPU work.
    pub fn render_items(&mut self, frame_size: [u32; 2], items: &[DrawItem]) {
        self.ensure_targets(frame_size);
        let frame = [frame_size[0] as f32, frame_size[1] as f32];

        for item in items {
            let layers: &[&LayerInput] = match item {
                DrawItem::Layer(l) => &[l],
                DrawItem::Dissolve { from, to, .. } => &[from, to],
            };
            for l in layers {
                if l.pixels.len() as u64 == l.pixel_size[0] as u64 * l.pixel_size[1] as u64 * 4 && l.pixel_size[0] > 0 {
                    self.upload(l);
                }
            }
        }

        // Build the draw list: (uniform slot, what to draw).
        enum Op {
            /// Draw a layer into the frame.
            Frame { slot: usize, key: u64, blend: BlendMode },
            /// Draw a dissolve group: sum two layers into the group target, then
            /// composite the group into the frame.
            Group { a: (usize, u64), b: (usize, u64), over: usize, blend: BlendMode },
        }
        let mut uniforms: Vec<(Transform, [f32; 2])> = Vec::new();
        fn slot(u: &mut Vec<(Transform, [f32; 2])>, t: Transform, size: [f32; 2]) -> usize {
            u.push((t, size));
            u.len() - 1
        }
        let mut ops = Vec::new();
        for item in items {
            match item {
                DrawItem::Layer(l) => ops.push(Op::Frame { slot: slot(&mut uniforms, l.transform, l.layer_size), key: l.key, blend: l.blend }),
                DrawItem::Dissolve { from, to, mix } => {
                    let m = mix.clamp(0.0, 1.0);
                    let mut ta = from.transform;
                    ta.opacity *= 1.0 - m;
                    let mut tb = to.transform;
                    tb.opacity *= m;
                    let a = (slot(&mut uniforms, ta, from.layer_size), from.key);
                    let b = (slot(&mut uniforms, tb, to.layer_size), to.key);
                    let over = slot(&mut uniforms, Transform::default(), frame);
                    ops.push(Op::Group { a, b, over, blend: to.blend });
                }
            }
            if uniforms.len() > MAX_DRAWS - 3 {
                log::warn!("too many layers; dropping the rest");
                break;
            }
        }

        // Uniforms for every draw in one buffer, addressed by dynamic offset.
        let mut data = vec![0u8; self.uniform_stride as usize * uniforms.len().max(1)];
        for (i, (t, size)) in uniforms.iter().enumerate() {
            let mvp = t.matrix(frame).to_clip_space(frame);
            let mut floats: Vec<f32> = mvp.iter().flatten().copied().collect();
            floats.extend([size[0], size[1], t.opacity.clamp(0.0, 1.0), 0.0]);
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
        let offset = |slot: usize| (slot as u64 * self.uniform_stride) as u32;

        // The frame is drawn in runs of plain layers; each dissolve group needs
        // its own pass on the scratch target in between.
        let mut frame_pass = Some(pass_on(&mut enc, &targets.accum_view, wgpu::LoadOp::Clear(wgpu::Color::BLACK)));
        for op in &ops {
            match op {
                Op::Frame { slot, key, blend } => {
                    let Some(tex) = self.layers.get(key) else { continue };
                    let pass = frame_pass.get_or_insert_with(|| pass_on(&mut enc, &targets.accum_view, wgpu::LoadOp::Load));
                    pass.set_pipeline(&self.layer_pipelines[blend]);
                    pass.set_bind_group(0, &self.uniform_bind, &[offset(*slot)]);
                    pass.set_bind_group(1, &tex.bind_group, &[]);
                    pass.draw(0..6, 0..1);
                }
                Op::Group { a, b, over, blend } => {
                    drop(frame_pass.take());
                    {
                        let mut gp = pass_on(&mut enc, &targets.group_view, wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT));
                        gp.set_pipeline(&self.sum_pipeline);
                        for (slot, key) in [a, b] {
                            let Some(tex) = self.layers.get(key) else { continue };
                            gp.set_bind_group(0, &self.uniform_bind, &[offset(*slot)]);
                            gp.set_bind_group(1, &tex.bind_group, &[]);
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
            let mut pass = pass_on(&mut enc, &targets.output_view, wgpu::LoadOp::Clear(wgpu::Color::BLACK));
            pass.set_pipeline(&self.output_pipeline);
            pass.set_bind_group(0, &targets.output_bind, &[]);
            pass.draw(0..3, 0..1);
        }
        self.queue.submit([enc.finish()]);
    }

    /// Drop cached textures for layers not in `keep`.
    pub fn retain_layers(&mut self, keep: impl Fn(u64) -> bool) {
        self.layers.retain(|k, _| keep(*k));
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
