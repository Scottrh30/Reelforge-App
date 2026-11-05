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
            for v in l.position.iter().chain(&l.direction).chain(&l.color).chain(&l.cone).chain([&l.radius]) {
                v.to_bits().hash(&mut h);
            }
            (l.kind, l.falloff).hash(&mut h);
        }
        h.finish()
    }
}

const SAMPLES: u32 = 4;
/// Outline uniforms: a slot per shadow-atlas tile (256-byte aligned for dynamic offsets).
const FLAT_STRIDE: u64 = 256;
const FLAT_SLOTS: u64 = 16;
const DEPTH_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Depth32Float;
/// mat4, 4 + 3 vec4 of axes and normals, camera, studio, two colours, 32 vec4 of lights.
const UNIFORM_SIZE: usize = 64 + 7 * 16 + 4 * 16 + 32 * 16;

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
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
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
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("solid"), bind_group_layouts: &[Some(&layout)], immediate_size: 0 });
        let attrs = wgpu::vertex_attr_array![0 => Float32x3, 1 => Float32x3, 2 => Float32];
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
        Self { pipeline, uniforms, bind, flat_pipeline, flat_uniforms, flat_bind, targets: None, meshes: HashMap::new(), depth_pipeline, depths: HashMap::new(), format }
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
            let flat: Vec<f32> = mesh.vertices.iter().flat_map(|v| [v.pos[0], v.pos[1], v.pos[2], v.normal[0], v.normal[1], v.normal[2], v.part]).collect();
            let idx: Vec<u8> = mesh.indices.iter().flat_map(|i| i.to_le_bytes()).collect();
            // Buffers can't be empty.
            let vdata = if flat.is_empty() { vec![0u8; 28] } else { bytes(&flat) };
            let idata = if idx.is_empty() { vec![0u8; 4] } else { idx };
            let vertices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("solid vertices"), contents: &vdata, usage: wgpu::BufferUsages::VERTEX });
            let indices = device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("solid indices"), contents: &idata, usage: wgpu::BufferUsages::INDEX });
            self.meshes.insert(key, MeshBuffers { id: mesh.id, vertices, indices, count: mesh.indices.len() as u32 });
        }
        queue.write_buffer(&self.uniforms, 0, &uniform_bytes(s));
        let face = linear(s.face);
        let t = self.targets.as_ref().unwrap();
        let m = &self.meshes[&key];
        let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("solid") });
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
                pass.set_vertex_buffer(0, m.vertices.slice(..));
                pass.set_index_buffer(m.indices.slice(..), wgpu::IndexFormat::Uint32);
                pass.draw_indexed(0..m.count, 0, 0..1);
            }
        }
        // The depth picture. Where there's no face it holds the depth of the
        // text's middle, so a glow or blur spilling past the letters sits there too.
        let mid = {
            let b = mesh.bounds;
            let p = [(b[0] + b[2]) * 0.5, (b[1] + b[3]) * 0.5, mesh.depth * 0.5, 1.0];
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
        f.extend([l.position[0], l.position[1], l.position[2], l.kind as f32]);
        f.extend([l.direction[0], l.direction[1], l.direction[2], l.falloff as f32]);
        f.extend([l.color[0], l.color[1], l.color[2], l.radius]);
        f.extend([l.cone[0], l.cone[1], 0.0, 0.0]);
    }
    f.resize(UNIFORM_SIZE / 4, 0.0);
    f.iter().flat_map(|v| v.to_le_bytes()).collect()
}
