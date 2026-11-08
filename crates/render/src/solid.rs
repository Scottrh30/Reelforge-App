//! Drawing solid objects (3D text) into a layer texture: the mesh in true
//! perspective with a depth buffer and 4× multisampling, lit by the scene's
//! lights or, in a scene without any, by a soft studio light from above left.
//!
//! The result is a frame-sized picture that the compositor then treats like
//! any other layer (opacity, blend mode and effects apply to it).

use crate::text3d::{TextMesh, Vertex};
use reelforge_core::space3d::{SolidSpace, MAX_LIGHTS, STUDIO_AMBIENT};
use std::collections::HashMap;
use std::sync::Arc;

/// A solid layer's picture: what to draw and where.
#[derive(Clone)]
pub struct SolidInput {
    pub mesh: Arc<TextMesh>,
    pub space: SolidSpace,
    /// sRGB colours (straight alpha) of the front/back faces and of the sides.
    pub face: [u8; 4],
    pub side: [u8; 4],
}

impl SolidInput {
    /// Changes whenever the picture would.
    pub fn content_id(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.mesh.id.hash(&mut h);
        (self.face, self.side).hash(&mut h);
        let s = &self.space;
        for v in s.mvp.iter().flatten().chain(s.axes.iter().flatten()).chain(&s.origin).chain(&s.camera).chain(&s.studio) {
            v.to_bits().hash(&mut h);
        }
        for l in &s.lights {
            for v in l.position.iter().chain(&l.direction).chain(&l.color).chain(&l.cone).chain([&l.radius, &l.shadow, &l.softness]) {
                v.to_bits().hash(&mut h);
            }
            (l.kind, l.falloff, l.env.map(|e| e.id)).hash(&mut h);
            for v in l.gpu().iter().flatten() {
                v.to_bits().hash(&mut h);
            }
        }
        h.finish()
    }
}

const SAMPLES: u32 = 4;
/// Outline uniforms: a slot per shadow-atlas tile (256-byte aligned for dynamic offsets).
const FLAT_STRIDE: u64 = 256;
const FLAT_SLOTS: u64 = crate::MAX_OCCLUDERS as u64;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
/// Shadow casters the text's shader takes (as `MAX_OCC` in solid.wgsl), six vec4 each, after a vec4 head.
pub(crate) const MAX_SHADOW_CASTERS: usize = crate::MAX_OCCLUDERS;
const SHADOW_SIZE: u64 = 16 + MAX_SHADOW_CASTERS as u64 * 6 * 16;
/// mat4, 4 + 3 vec4 of axes and normals, camera, studio, two colours, 32 vec4 of lights.
const LIGHTS_END: usize = 64 + 7 * 16 + 4 * 16 + 32 * 16;
/// …then the environment's reflection settings.
const UNIFORM_SIZE: usize = LIGHTS_END + 16;

struct Targets {
    size: [u32; 2],
    color: wgpu::TextureView,
    depth: wgpu::TextureView,
    /// Single-sampled, for the depth picture.
    depth1: wgpu::TextureView,
}

/// A layer's depth picture (R32Float, the layer's size) and its bind group
/// for the compositor (its "frame below" slot, group 2).
struct DepthPicture {
    size: [u32; 2],
    view: wgpu::TextureView,
    bind: wgpu::BindGroup,
}

struct MeshBuffers {
    id: u64,
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    count: u32,
    /// A model's texture atlas (group 3), or the plain white one.
    texture: Option<wgpu::BindGroup>,
}

pub(crate) struct SolidRenderer {
    pipeline: wgpu::RenderPipeline,
    uniforms: wgpu::Buffer,
    bind: wgpu::BindGroup,
    /// Outlines into the shadow atlas: the pipeline, and a uniform slot per tile.
    flat_pipeline: wgpu::RenderPipeline,
    flat_uniforms: wgpu::Buffer,
    flat_bind: wgpu::BindGroup,
    targets: Option<Targets>,
    /// Per layer key.
    meshes: HashMap<u64, MeshBuffers>,
    depth_pipeline: wgpu::RenderPipeline,
    depths: HashMap<u64, DepthPicture>,
    format: wgpu::TextureFormat,
    /// Shadows on the text: their layout, the casters' buffer, a sampler, and "no shadows".
    shadow_layout: wgpu::BindGroupLayout,
    shadow_buffer: wgpu::Buffer,
    shadow_sampler: wgpu::Sampler,
    no_shadows: wgpu::BindGroup,
    /// The environment light's picture (group 2), and which one it is (0: none, black).
    env_layout: wgpu::BindGroupLayout,
    env_sampler: wgpu::Sampler,
    env_bind: wgpu::BindGroup,
    env_id: u64,
    /// A model's textures (group 3): the layout, a sampler, and a white one for everything else.
    tex_layout: wgpu::BindGroupLayout,
    tex_sampler: wgpu::Sampler,
    white_tex: wgpu::BindGroup,
}

/// Shadows to draw text with: the casters (as the compositor lays them out,
/// six vec4s each) and the atlas of their outlines.
pub(crate) struct Shadows<'a> {
    pub casters: &'a [f32],
    pub count: usize,
    pub atlas: &'a wgpu::TextureView,
}

fn srgb_to_linear(c: u8) -> f32 {
    let c = c as f32 / 255.0;
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

/// The colour as linear RGB, and its alpha.
fn linear(c: [u8; 4]) -> [f32; 4] {
    [srgb_to_linear(c[0]), srgb_to_linear(c[1]), srgb_to_linear(c[2]), c[3] as f32 / 255.0]
}

impl SolidRenderer {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue, format: wgpu::TextureFormat) -> Self {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("solid"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: wgpu::BufferSize::new(UNIFORM_SIZE as u64) },
                count: None,
            }],
        });
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor { label: Some("solid"), size: UNIFORM_SIZE as u64, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("solid"), layout: &layout, entries: &[wgpu::BindGroupEntry { binding: 0, resource: uniforms.as_entire_binding() }] });
        let shader = device.create_shader_module(wgpu::include_wgsl!("solid.wgsl"));
        let shadow_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("solid shadows"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: wgpu::BufferSize::new(SHADOW_SIZE) },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry { binding: 2, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering), count: None },
            ],
        });
        let shadow_buffer = device.create_buffer(&wgpu::BufferDescriptor { label: Some("solid shadows"), size: SHADOW_SIZE, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let none_buffer = device.create_buffer(&wgpu::BufferDescriptor { label: Some("solid no shadows"), size: SHADOW_SIZE, usage: wgpu::BufferUsages::UNIFORM, mapped_at_creation: false });
        let shadow_sampler = device.create_sampler(&wgpu::SamplerDescriptor { label: Some("solid shadows"), mag_filter: wgpu::FilterMode::Linear, min_filter: wgpu::FilterMode::Linear, ..Default::default() });
        let blank = device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("solid no shadows"),
                size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::R8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
            .create_view(&Default::default());
        let no_shadows = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("solid no shadows"),
            layout: &shadow_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: none_buffer.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(&blank) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&shadow_sampler) },
            ],
        });
        let env_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("solid environment"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry { binding: 1, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering), count: None },
            ],
        });
        // Round and round across the picture, clamped at the poles.
        let env_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("solid environment"),
            address_mode_u: wgpu::AddressMode::Repeat,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Linear,
            ..Default::default()
        });
        let env_bind = env_bind_group(device, black_picture(device), &env_layout, &env_sampler);
        let tex_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("solid texture"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry { binding: 1, visibility: wgpu::ShaderStages::FRAGMENT, ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering), count: None },
            ],
        });
        let tex_sampler = device.create_sampler(&wgpu::SamplerDescriptor { label: Some("solid texture"), mag_filter: wgpu::FilterMode::Linear, min_filter: wgpu::FilterMode::Linear, ..Default::default() });
        let white_tex = texture_bind_group(device, queue, &tex_layout, &tex_sampler, 1, 1, &[255; 4]);
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("solid"), bind_group_layouts: &[Some(&layout), Some(&shadow_layout), Some(&env_layout), Some(&tex_layout)], immediate_size: 0 });
        let attrs = wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32, 3 => Float32x4, 4 => Float32x2, 5 => Float32x4];
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("solid"),
            layout: Some(&pl),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_solid"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout { array_stride: std::mem::size_of::<Vertex>() as u64, step_mode: wgpu::VertexStepMode::Vertex, attributes: &attrs })],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_solid"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState { format, blend: None, write_mask: wgpu::ColorWrites::ALL })],
            }),
            // Both sides: the faces' winding isn't kept consistent, the depth test sorts them out.
            primitive: wgpu::PrimitiveState { cull_mode: None, ..Default::default() },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: wgpu::MultisampleState { count: SAMPLES, ..Default::default() },
            multiview_mask: None,
            cache: None,
        });
        // The depth picture: same mesh and transform, nearest face's depth into R32Float.
        let depth_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("solid depth"),
            layout: Some(&pl),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_solid"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout { array_stride: std::mem::size_of::<Vertex>() as u64, step_mode: wgpu::VertexStepMode::Vertex, attributes: &attrs })],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_depth"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState { format: wgpu::TextureFormat::R32Float, blend: None, write_mask: wgpu::ColorWrites::ALL })],
            }),
            primitive: wgpu::PrimitiveState { cull_mode: None, ..Default::default() },
            depth_stencil: Some(wgpu::DepthStencilState {
                format: DEPTH_FORMAT,
                depth_write_enabled: Some(true),
                depth_compare: Some(wgpu::CompareFunction::Less),
                stencil: Default::default(),
                bias: Default::default(),
            }),
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        // The outline (for shadows): flat, into the R8 shadow atlas, one uniform slot per tile.
        let flat_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("solid outline"),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX,
                ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: true, min_binding_size: wgpu::BufferSize::new(16) },
                count: None,
            }],
        });
        let flat_uniforms = device.create_buffer(&wgpu::BufferDescriptor { label: Some("solid outline"), size: FLAT_STRIDE * FLAT_SLOTS, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let flat_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("solid outline"),
            layout: &flat_layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding { buffer: &flat_uniforms, offset: 0, size: wgpu::BufferSize::new(16) }) }],
        });
        let flat_shader = device.create_shader_module(wgpu::include_wgsl!("solid_flat.wgsl"));
        let flat_pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("solid outline"), bind_group_layouts: &[Some(&flat_layout)], immediate_size: 0 });
        let flat_attrs = wgpu::vertex_attr_array![0 => Float32x3];
        let flat_pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("solid outline"),
            layout: Some(&flat_pl),
            vertex: wgpu::VertexState {
                module: &flat_shader,
                entry_point: Some("vs_flat"),
                compilation_options: Default::default(),
                buffers: &[Some(wgpu::VertexBufferLayout { array_stride: std::mem::size_of::<Vertex>() as u64, step_mode: wgpu::VertexStepMode::Vertex, attributes: &flat_attrs })],
            },
            fragment: Some(wgpu::FragmentState {
                module: &flat_shader,
                entry_point: Some("fs_flat"),
                compilation_options: Default::default(),
                targets: &[Some(wgpu::ColorTargetState { format: wgpu::TextureFormat::R8Unorm, blend: None, write_mask: wgpu::ColorWrites::ALL })],
            }),
            primitive: wgpu::PrimitiveState { cull_mode: None, ..Default::default() },
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        Self { pipeline, uniforms, bind, flat_pipeline, flat_uniforms, flat_bind, targets: None, meshes: HashMap::new(), depth_pipeline, depths: HashMap::new(), format, shadow_layout, shadow_buffer, shadow_sampler, no_shadows, env_layout, env_sampler, env_bind, env_id: 0, tex_layout, tex_sampler, white_tex }
    }

    /// Bind the environment `id` (`crate::environment`; 0 or unknown: black)
    /// for reflections, uploading its picture with mipmaps when it changes.
    fn use_environment(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, id: u64) {
        if id == self.env_id {
            return;
        }
        let view = match reelforge_core::environment::get(id) {
            Some(e) if e.width > 0 && e.height > 0 => env_texture(device, queue, &e),
            _ => black_picture(device),
        };
        self.env_bind = env_bind_group(device, view, &self.env_layout, &self.env_sampler);
        self.env_id = id;
    }

    /// Get layer `key`'s outline ready for shadow-atlas tile `slot`: false if
    /// it has no mesh yet (draw it first) or nothing to cast.
    pub fn prepare_outline(&self, queue: &wgpu::Queue, key: u64, s: &SolidInput, slot: usize) -> bool {
        let Some(m) = self.meshes.get(&key) else { return false };
        let b = s.mesh.bounds;
        if m.count == 0 || m.id != s.mesh.id || slot as u64 >= FLAT_SLOTS || b[2] <= b[0] || b[3] <= b[1] {
            return false;
        }
        let v = [b[0], b[1], b[2] - b[0], b[3] - b[1]];
        let bytes: Vec<u8> = v.iter().flat_map(|f| f.to_le_bytes()).collect();
        queue.write_buffer(&self.flat_uniforms, slot as u64 * FLAT_STRIDE, &bytes);
        true
    }

    /// Draw layer `key`'s outline (prepared for `slot`) into `pass`, whose
    /// viewport is already the tile.
    pub fn draw_outline<'p>(&'p self, pass: &mut wgpu::RenderPass<'p>, key: u64, slot: usize) {
        let Some(m) = self.meshes.get(&key) else { return };
        pass.set_pipeline(&self.flat_pipeline);
        pass.set_bind_group(0, &self.flat_bind, &[(slot as u64 * FLAT_STRIDE) as u32]);
        pass.set_vertex_buffer(0, m.vertices.slice(..));
        pass.set_index_buffer(m.indices.slice(..), wgpu::IndexFormat::Uint32);
        pass.draw_indexed(0..m.count, 0, 0..1);
    }

    /// The text's picture into `target`, lit with `shadows` (a group-1 bind group).
    fn colour_pass(&self, enc: &mut wgpu::CommandEncoder, key: u64, s: &SolidInput, target: &wgpu::TextureView, shadows: &wgpu::BindGroup) {
        let face = linear(s.face);
        let (Some(t), Some(m)) = (self.targets.as_ref(), self.meshes.get(&key)) else { return };
        {
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("solid"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &t.color,
                    depth_slice: None,
                    resolve_target: Some(target),
                    // Transparent, but in the text's colour, so the soft edges
                    // (averaged with the background) keep the right colour.
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color { r: face[0] as f64, g: face[1] as f64, b: face[2] as f64, a: 0.0 }), store: wgpu::StoreOp::Discard },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment { view: &t.depth, depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Discard }), stencil_ops: None }),
                ..Default::default()
            });
            if m.count > 0 {
                pass.set_pipeline(&self.pipeline);
                pass.set_bind_group(0, &self.bind, &[]);
                pass.set_bind_group(1, shadows, &[]);
                pass.set_bind_group(2, &self.env_bind, &[]);
                pass.set_bind_group(3, m.texture.as_ref().unwrap_or(&self.white_tex), &[]);
                pass.set_vertex_buffer(0, m.vertices.slice(..));
                pass.set_index_buffer(m.indices.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..m.count, 0, 0..1);
            }
        }
    }

    /// Draw layer `key`'s picture again (after `draw` this frame), now with
    /// the shadows other layers and the text itself cast on it.
    pub fn draw_shadowed(&self, device: &wgpu::Device, queue: &wgpu::Queue, key: u64, s: &SolidInput, target: &wgpu::TextureView, sh: &Shadows) {
        if !self.meshes.contains_key(&key) || self.targets.is_none() {
            return;
        }
        let mut head = vec![sh.count.min(MAX_SHADOW_CASTERS) as f32, 0.0, 0.0, 0.0];
        head.extend(sh.casters.iter().take(MAX_SHADOW_CASTERS * 24));
        head.resize(SHADOW_SIZE as usize / 4, 0.0);
        queue.write_buffer(&self.shadow_buffer, 0, &head.iter().flat_map(|f| f.to_le_bytes()).collect::<Vec<u8>>());
        queue.write_buffer(&self.uniforms, 0, &uniform_bytes(s));
        let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("solid shadows"),
            layout: &self.shadow_layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: self.shadow_buffer.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::TextureView(sh.atlas) },
                wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::Sampler(&self.shadow_sampler) },
            ],
        });
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("solid shadowed") });
        self.colour_pass(&mut enc, key, s, target, &bind);
        queue.submit([enc.finish()]);
    }

    /// Forget the meshes of layers that are gone.
    pub fn retain(&mut self, keep: impl Fn(u64) -> bool) {
        self.meshes.retain(|k, _| keep(*k));
    }

    /// Draw `s` (layer `key`) into `target`, a `size` texture, replacing what's there.
    /// Layer `key`'s depth picture, bound for the compositor (if it's been drawn).
    pub fn depth_bind(&self, key: u64) -> Option<&wgpu::BindGroup> {
        self.depths.get(&key).map(|d| &d.bind)
    }

    /// Draw layer `key`'s picture into `target`, and its depth picture (bound
    /// with `depth_layout`, a single non-filterable float texture).
    pub fn draw(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, key: u64, s: &SolidInput, target: &wgpu::TextureView, size: [u32; 2], depth_layout: &wgpu::BindGroupLayout) {
        if self.depths.get(&key).is_none_or(|d| d.size != size) {
            let view = device
                .create_texture(&wgpu::TextureDescriptor {
                    label: Some("solid depth picture"),
                    size: wgpu::Extent3d { width: size[0].max(1), height: size[1].max(1), depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::R32Float,
                    usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
                    view_formats: &[],
                })
                .create_view(&Default::default());
            let bind = device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("solid depth picture"), layout: depth_layout, entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) }] });
            self.depths.insert(key, DepthPicture { size, view, bind });
        }
        if self.targets.as_ref().is_none_or(|t| t.size != size) {
            let tex = |label, format, samples| {
                device
                    .create_texture(&wgpu::TextureDescriptor {
                        label: Some(label),
                        size: wgpu::Extent3d { width: size[0].max(1), height: size[1].max(1), depth_or_array_layers: 1 },
                        mip_level_count: 1,
                        sample_count: samples,
                        dimension: wgpu::TextureDimension::D2,
                        format,
                        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                        view_formats: &[],
                    })
                    .create_view(&Default::default())
            };
            self.targets = Some(Targets { size, color: tex("solid colour", self.format, SAMPLES), depth: tex("solid depth", DEPTH_FORMAT, SAMPLES), depth1: tex("solid depth (single)", DEPTH_FORMAT, 1) });
        }
        let mesh = &s.mesh;
        if self.meshes.get(&key).is_none_or(|m| m.id != mesh.id) {
            use wgpu::util::DeviceExt;
            let bytes = |v: &[f32]| v.iter().flat_map(|f| f.to_le_bytes()).collect::<Vec<u8>>();
            let flat: Vec<f32> = mesh
                .vertices
                .iter()
                .flat_map(|v| [v.pos[0], v.pos[1], v.pos[2], v.normal[0], v.normal[1], v.normal[2], v.part, v.color[0], v.color[1], v.color[2], v.color[3], v.uv[0], v.uv[1], v.rect[0], v.rect[1], v.rect[2], v.rect[3]])
                .collect();
            let idx: Vec<u8> = mesh.indices.iter().flat_map(|i| i.to_le_bytes()).collect();
            // Buffers can't be empty.
            let vdata = if flat.is_empty() { vec![0u8; std::mem::size_of::<Vertex>()] } else { bytes(&flat) };
            let idata = if idx.is_empty() { vec![0u8; 4] } else { idx };
            let vertices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("solid vertices"), contents: &vdata, usage: wgpu::BufferUsages::VERTEX });
            let indices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("solid indices"), contents: &idata, usage: wgpu::BufferUsages::INDEX });
            let texture = mesh.atlas.as_ref().map(|a| texture_bind_group(device, queue, &self.tex_layout, &self.tex_sampler, a.width, a.height, &a.rgba));
            self.meshes.insert(key, MeshBuffers { id: mesh.id, vertices, indices, count: mesh.indices.len() as u32, texture });
        }
        self.use_environment(device, queue, environment_of(s).map_or(0, |e| e.id));
        queue.write_buffer(&self.uniforms, 0, &uniform_bytes(s));
        let t = self.targets.as_ref().unwrap();
        let m = &self.meshes[&key];
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("solid") });
        self.colour_pass(&mut enc, key, s, target, &self.no_shadows);
        // The depth picture. Where there's no face it holds the depth of the
        // text's middle, so a glow or blur spilling past the letters sits there too.
        let mid = {
            let b = mesh.bounds;
            let p = [(b[0] + b[2]) * 0.5, (b[1] + b[3]) * 0.5, mesh.mid_z, 1.0];
            let m = &s.space.mvp;
            let c = |i: usize| (0..4).map(|j| m[j][i] * p[j]).sum::<f32>();
            let (z, w) = (c(2), c(3));
            if w > 1e-6 { (z / w).clamp(0.0, 1.0) } else { 1.0 }
        };
        {
            let d = &self.depths[&key];
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("solid depth picture"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &d.view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color { r: mid as f64, g: 0.0, b: 0.0, a: 1.0 }), store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment { view: &t.depth1, depth_ops: Some(wgpu::Operations { load: wgpu::LoadOp::Clear(1.0), store: wgpu::StoreOp::Discard }), stencil_ops: None }),
                ..Default::default()
            });
            if m.count > 0 {
                pass.set_pipeline(&self.depth_pipeline);
                pass.set_bind_group(0, &self.bind, &[]);
                pass.set_bind_group(1, &self.no_shadows, &[]);
                pass.set_bind_group(2, &self.env_bind, &[]);
                pass.set_bind_group(3, m.texture.as_ref().unwrap_or(&self.white_tex), &[]);
                pass.set_vertex_buffer(0, m.vertices.slice(..));
                pass.set_index_buffer(m.indices.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..m.count, 0, 0..1);
            }
        }
        queue.submit([enc.finish()]);
    }
}

fn uniform_bytes(s: &SolidInput) -> Vec<u8> {
    let sp = &s.space;
    let mut f: Vec<f32> = Vec::with_capacity(UNIFORM_SIZE / 4);
    f.extend(sp.mvp.iter().flatten());
    for a in sp.axes.iter().chain([&sp.origin]) {
        f.extend([a[0], a[1], a[2], 0.0]);
    }
    for n in &sp.normals {
        f.extend([n[0], n[1], n[2], 0.0]);
    }
    let lights = &sp.lights[..sp.lights.len().min(MAX_LIGHTS)];
    f.extend([sp.camera[0], sp.camera[1], sp.camera[2], lights.len() as f32]);
    f.extend([sp.studio[0], sp.studio[1], sp.studio[2], STUDIO_AMBIENT]);
    f.extend(linear(s.face));
    f.extend(linear(s.side));
    for l in lights {
        f.extend(l.gpu().iter().flatten());
    }
    f.resize(LIGHTS_END / 4, 0.0);
    match environment_of(s) {
        Some(e) => {
            let l = lights.iter().find(|l| l.env.is_some()).unwrap();
            f.extend([e.turn, e.reflect, e.blur, l.color.iter().sum::<f32>() / 3.0]);
        }
        None => f.extend([0.0; 4]),
    }
    f.resize(UNIFORM_SIZE / 4, 0.0);
    f.iter().flat_map(|v| v.to_le_bytes()).collect()
}

/// The first environment light shining on `s` (what its reflections show).
fn environment_of(s: &SolidInput) -> Option<reelforge_core::space3d::EnvLight> {
    s.space.lights.iter().take(MAX_LIGHTS).find_map(|l| l.env)
}

/// A 1 × 1 black picture (no environment).
fn black_picture(device: &wgpu::Device) -> wgpu::TextureView {
    device
        .create_texture(&wgpu::TextureDescriptor {
            label: Some("solid no environment"),
            size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        })
        .create_view(&Default::default())
}

fn env_bind_group(device: &wgpu::Device, view: wgpu::TextureView, layout: &wgpu::BindGroupLayout, sampler: &wgpu::Sampler) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("solid environment"),
        layout,
        entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) }, wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(sampler) }],
    })
}

/// The environment's picture as half-float RGBA with mipmaps (each level
/// the 2 × 2 average of the one before), for sharp to blurred reflections.
fn env_texture(device: &wgpu::Device, queue: &wgpu::Queue, e: &reelforge_core::environment::Environment) -> wgpu::TextureView {
    use reelforge_core::frame::f32_to_f16;
    let (w, h) = (e.width, e.height);
    let levels = 32 - w.max(h).leading_zeros();
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("solid environment"),
        size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        mip_level_count: levels,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba16Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    let (mut lw, mut lh, mut rgb) = (w as usize, h as usize, e.rgb.clone());
    for level in 0..levels {
        let bytes: Vec<u8> = rgb.chunks_exact(3).flat_map(|p| [f32_to_f16(p[0]), f32_to_f16(p[1]), f32_to_f16(p[2]), f32_to_f16(1.0)]).flat_map(|v| v.to_le_bytes()).collect();
        queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture: &tex, mip_level: level, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            &bytes,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(lw as u32 * 8), rows_per_image: Some(lh as u32) },
            wgpu::Extent3d { width: lw as u32, height: lh as u32, depth_or_array_layers: 1 },
        );
        let (nw, nh) = ((lw / 2).max(1), (lh / 2).max(1));
        let mut next = vec![0.0f32; nw * nh * 3];
        for y in 0..nh {
            for x in 0..nw {
                for c in 0..3 {
                    let at = |xx: usize, yy: usize| rgb[(yy.min(lh - 1) * lw + xx.min(lw - 1)) * 3 + c];
                    next[(y * nw + x) * 3 + c] = (at(2 * x, 2 * y) + at(2 * x + 1, 2 * y) + at(2 * x, 2 * y + 1) + at(2 * x + 1, 2 * y + 1)) * 0.25;
                }
            }
        }
        (lw, lh, rgb) = (nw, nh, next);
    }
    tex.create_view(&Default::default())
}

/// An RGBA8 (sRGB) picture as a group-3 texture.
fn texture_bind_group(device: &wgpu::Device, queue: &wgpu::Queue, layout: &wgpu::BindGroupLayout, sampler: &wgpu::Sampler, w: u32, h: u32, rgba: &[u8]) -> wgpu::BindGroup {
    let tex = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("solid texture"),
        size: wgpu::Extent3d { width: w.max(1), height: h.max(1), depth_or_array_layers: 1 },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo { texture: &tex, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
        rgba,
        wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(w.max(1) * 4), rows_per_image: Some(h.max(1)) },
        wgpu::Extent3d { width: w.max(1), height: h.max(1), depth_or_array_layers: 1 },
    );
    let view = tex.create_view(&Default::default());
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("solid texture"),
        layout,
        entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) }, wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(sampler) }],
    })
}
