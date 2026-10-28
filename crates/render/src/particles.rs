//! Drawing particles (see `reelforge_core::particles`) into a layer's effect
//! target: one instanced draw of a quad per particle, which the vertex shader
//! moves along the particle's path. The birth states of every emitter in a
//! frame go up in one instance buffer.

use reelforge_core::particles::{Particles, LOOKS};

/// Bytes per particle (12 floats).
const STRIDE: u64 = 48;

pub(crate) struct ParticleRenderer {
    /// Additive light, and covering (premultiplied "over").
    add: wgpu::RenderPipeline,
    over: wgpu::RenderPipeline,
    buffer: wgpu::Buffer,
    capacity: u64,
}

/// Where one emitter's particles are in the frame's instance buffer.
#[derive(Clone)]
pub(crate) struct Batch {
    pub range: std::ops::Range<u32>,
    pub additive: bool,
}

impl ParticleRenderer {
    /// Pipelines with `layout` (the effect passes' uniform at group 0) drawing into `format`.
    pub fn new(device: &wgpu::Device, layout: &wgpu::PipelineLayout, format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::include_wgsl!("particles.wgsl"));
        let attrs = wgpu::vertex_attr_array![0 => Float32x4, 1 => Float32x4, 2 => Float32x4];
        let make = |label, blend| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some(label),
                layout: Some(layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_particle"),
                    compilation_options: Default::default(),
                    buffers: &[Some(wgpu::VertexBufferLayout { array_stride: STRIDE, step_mode: wgpu::VertexStepMode::Instance, attributes: &attrs })],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_particle"),
                    compilation_options: Default::default(),
                    targets: &[Some(wgpu::ColorTargetState { format, blend: Some(blend), write_mask: wgpu::ColorWrites::ALL })],
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        use wgpu::{BlendComponent as C, BlendFactor as F, BlendOperation as O};
        let add = make("particles (add)", wgpu::BlendState { color: C { src_factor: F::One, dst_factor: F::One, operation: O::Add }, alpha: C { src_factor: F::One, dst_factor: F::One, operation: O::Max } });
        let over = make("particles (over)", wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING);
        let capacity = 1024 * STRIDE;
        let buffer = Self::make_buffer(device, capacity);
        Self { add, over, buffer, capacity }
    }

    fn make_buffer(device: &wgpu::Device, size: u64) -> wgpu::Buffer {
        device.create_buffer(&wgpu::BufferDescriptor { label: Some("particles"), size, usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false })
    }

    /// Upload the frame's emitters (with their look), returning where each one is.
    pub fn upload(&mut self, device: &wgpu::Device, queue: &wgpu::Queue, sets: &[(&Particles, f32)]) -> Vec<Batch> {
        let total: u64 = sets.iter().map(|(p, _)| p.list.len() as u64).sum();
        if total * STRIDE > self.capacity {
            self.capacity = (total * STRIDE).next_power_of_two();
            self.buffer = Self::make_buffer(device, self.capacity);
        }
        let mut at = 0u32;
        let mut out = Vec::with_capacity(sets.len());
        for (p, look) in sets {
            let n = p.list.len() as u32;
            if n > 0 {
                let bytes: &[u8] = bytemuck_floats(p.as_floats());
                queue.write_buffer(&self.buffer, at as u64 * STRIDE, bytes);
            }
            let look = (look.round().max(0.0) as usize).min(LOOKS.len() - 1);
            out.push(Batch { range: at..at + n, additive: look <= 1 });
            at += n;
        }
        out
    }

    /// Draw `b` in `pass` (whose group 0 is already set to the pass's uniform).
    pub fn draw(&self, pass: &mut wgpu::RenderPass<'_>, b: &Batch) {
        if b.range.is_empty() {
            return;
        }
        pass.set_pipeline(if b.additive { &self.add } else { &self.over });
        pass.set_vertex_buffer(0, self.buffer.slice(..));
        pass.draw(0..6, b.range.clone());
    }
}

fn bytemuck_floats(f: &[f32]) -> &[u8] {
    // Safety: any f32 is 4 plain bytes.
    unsafe { std::slice::from_raw_parts(f.as_ptr() as *const u8, std::mem::size_of_val(f)) }
}
