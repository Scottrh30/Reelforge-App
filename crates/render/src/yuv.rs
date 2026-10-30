//! Turning decoded YUV planes into a layer's RGBA texture on the GPU.
//!
//! The planes are uploaded as unsigned-integer textures (8 or 16 bits per
//! sample, as decoded) and one full-screen pass converts them, with the
//! frame's matrix and range, into the layer texture the compositor samples:
//! 8-bit sRGB for 8-bit video, half float (linear, premultiplied) for deeper
//! video, so 10- and 12-bit pictures keep their precision.
//! See `yuv.wgsl` and `reelforge_core::frame` (the CPU reference).

use reelforge_core::frame::{YuvLayout, YuvParams, YuvPlanes};
use std::collections::HashMap;

/// Size of the shader's `Params` uniform.
const PARAMS_SIZE: u64 = 64;

struct PlaneSet {
    /// Y, U, V (or Y, UV) textures.
    textures: Vec<wgpu::Texture>,
    bind_group: wgpu::BindGroup,
    params: wgpu::Buffer,
    /// What these textures were made for.
    shape: (reelforge_core::frame::YuvLayout, u32, u32),
}

pub(crate) struct YuvConverter {
    layout: wgpu::BindGroupLayout,
    /// Into an 8-bit sRGB layer texture.
    pipeline: wgpu::RenderPipeline,
    /// Into a half-float layer texture (deep video).
    float_pipeline: wgpu::RenderPipeline,
    /// Per layer key.
    sets: HashMap<u64, PlaneSet>,
}

impl YuvConverter {
    /// `target`: the 8-bit layer format; `float_target`: the deep one.
    pub fn new(device: &wgpu::Device, target: wgpu::TextureFormat, float_target: wgpu::TextureFormat) -> Self {
        let tex = |binding| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Uint, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("yuv planes"),
            entries: &[
                tex(0),
                tex(1),
                tex(2),
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: wgpu::BufferSize::new(PARAMS_SIZE) },
                    count: None,
                },
                tex(4),
            ],
        });
        let shader = device.create_shader_module(wgpu::include_wgsl!("yuv.wgsl"));
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("yuv"), bind_group_layouts: &[Some(&layout)], immediate_size: 0 });
        let make = |entry: &str, format| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("yuv to rgb"),
                layout: Some(&pl),
                vertex: wgpu::VertexState { module: &shader, entry_point: Some("vs_fullscreen"), compilation_options: Default::default(), buffers: &[] },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
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
        Self { pipeline: make("fs_yuv", target), float_pipeline: make("fs_yuv_float", float_target), layout, sets: HashMap::new() }
    }

    /// Upload `yuv` and convert it into `target` (a texture of the same size;
    /// `float`: a half-float one).
    pub fn convert(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, key: u64, yuv: &YuvPlanes, target: &wgpu::TextureView, float: bool) {
        let shape = (yuv.layout, yuv.width, yuv.height);
        if self.sets.get(&key).is_none_or(|s| s.shape != shape) {
            self.sets.insert(key, self.make_set(device, yuv));
        }
        let set = &self.sets[&key];
        let (cw, ch) = yuv.chroma_size();
        let b = yuv.layout.sample_bytes() as u32;
        let interleaved = yuv.layout.interleaved();
        let mut sizes: Vec<(u32, u32, u32)> =
            if interleaved { vec![(yuv.width, yuv.height, b), (cw, ch, 2 * b)] } else { vec![(yuv.width, yuv.height, b), (cw, ch, b), (cw, ch, b)] };
        if yuv.layout.alpha() {
            sizes.push((yuv.width, yuv.height, b));
        }
        for ((t, data), (w, h, bpp)) in set.textures.iter().zip(&yuv.planes).zip(sizes) {
            queue.write_texture(
                wgpu::TexelCopyTextureInfo { texture: t, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
                data,
                wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(w * bpp), rows_per_image: Some(h) },
                wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            );
        }
        queue.write_buffer(&set.params, 0, &params_bytes(&yuv.params(), yuv.layout, [yuv.width, yuv.height], [cw, ch]));
        self.draw(device, queue, &set.bind_group, target, float);
    }

    /// Convert planes that are already on the GPU (a frame shared by the video
    /// decoder; see `interop`) into `target`. `planes` are the luma plane and
    /// the interleaved chroma plane, as unsigned-integer textures.
    #[allow(dead_code)]
    pub fn bind_planes(&self, device: &wgpu::Device, y: &wgpu::TextureView, uv: &wgpu::TextureView) -> (wgpu::BindGroup, wgpu::Buffer) {
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("yuv params (shared frame)"),
            size: PARAMS_SIZE,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("yuv planes (shared frame)"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(y) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(uv) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(uv) },
                wgpu::BindGroupEntry { binding: 3, resource: params.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(y) },
            ],
        });
        (bind_group, params)
    }

    /// Run the conversion with a bind group from [`Self::bind_planes`].
    #[allow(dead_code)]
    #[allow(clippy::too_many_arguments)]
    pub fn convert_bound(&self, device: &wgpu::Device, queue: &wgpu::Queue, group: &(wgpu::BindGroup, wgpu::Buffer), p: &YuvParams, layout: YuvLayout, size: [u32; 2], target: &wgpu::TextureView, float: bool) {
        let (cw, ch) = layout.chroma_size(size[0], size[1]);
        queue.write_buffer(&group.1, 0, &params_bytes(p, layout, size, [cw, ch]));
        self.draw(device, queue, &group.0, target, float);
    }

    fn draw(&self, device: &wgpu::Device, queue: &wgpu::Queue, bind_group: &wgpu::BindGroup, target: &wgpu::TextureView, float: bool) {
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("yuv to rgb") });
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("yuv to rgb"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
                })],
                ..Default::default()
            });
            pass.set_pipeline(if float { &self.float_pipeline } else { &self.pipeline });
            pass.set_bind_group(0, bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        queue.submit([enc.finish()]);
    }

    fn make_set(&self, device: &wgpu::Device, yuv: &YuvPlanes) -> PlaneSet {
        let (cw, ch) = yuv.chroma_size();
        let wide = yuv.layout.sample_bytes() == 2;
        let (one, two) = if wide { (wgpu::TextureFormat::R16Uint, wgpu::TextureFormat::Rg16Uint) } else { (wgpu::TextureFormat::R8Uint, wgpu::TextureFormat::Rg8Uint) };
        let make = |label, w: u32, h: u32, format| {
            device.create_texture(&wgpu::TextureDescriptor {
                label: Some(label),
                size: wgpu::Extent3d { width: w.max(1), height: h.max(1), depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            })
        };
        let mut textures = if yuv.layout.interleaved() {
            vec![make("y plane", yuv.width, yuv.height, one), make("uv plane", cw, ch, two)]
        } else {
            vec![make("y plane", yuv.width, yuv.height, one), make("u plane", cw, ch, one), make("v plane", cw, ch, one)]
        };
        if yuv.layout.alpha() {
            textures.push(make("a plane", yuv.width, yuv.height, one));
        }
        let views: Vec<wgpu::TextureView> = textures.iter().map(|t| t.create_view(&Default::default())).collect();
        // With interleaved chroma the shader reads U and V from one texture; bind it twice.
        let v_view = if yuv.layout.interleaved() { &views[1] } else { &views[2] };
        // Without alpha the shader doesn't read binding 4; something must be bound.
        let a_view = if yuv.layout.alpha() { &views[3] } else { &views[0] };
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("yuv params"),
            size: PARAMS_SIZE,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("yuv planes"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&views[0]) },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&views[1]) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(v_view) },
                wgpu::BindGroupEntry { binding: 3, resource: params.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(a_view) },
            ],
        });
        PlaneSet { textures, bind_group, params, shape: (yuv.layout, yuv.width, yuv.height) }
    }

    pub fn retain(&mut self, keep: impl Fn(u64) -> bool) {
        self.sets.retain(|k, _| keep(*k));
    }
}

/// The shader's `Params`.
fn params_bytes(p: &YuvParams, layout: YuvLayout, size: [u32; 2], csize: [u32; 2]) -> [u8; PARAMS_SIZE as usize] {
    let mut u = [0u8; PARAMS_SIZE as usize];
    let (sx, sy) = layout.chroma().shift();
    let amax = if layout.alpha() { ((1u32 << layout.bits()) - 1) as f32 } else { 0.0 };
    let words: [u32; 16] = [
        p.shift,
        layout.interleaved() as u32,
        p.y_off.to_bits(),
        p.y_scale.to_bits(),
        p.c_off.to_bits(),
        p.c_scale.to_bits(),
        p.kr.to_bits(),
        p.kb.to_bits(),
        size[0],
        size[1],
        csize[0],
        csize[1],
        sx,
        sy,
        amax.to_bits(),
        0,
    ];
    for (i, w) in words.iter().enumerate() {
        u[i * 4..i * 4 + 4].copy_from_slice(&w.to_le_bytes());
    }
    u
}
