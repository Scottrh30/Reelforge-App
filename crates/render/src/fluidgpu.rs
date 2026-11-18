//! The fluid effect simulated on the graphics card (see `fluid.wgsl` and
//! `reelforge_core::fluid`): the grid lives on the card, each step's few
//! settings (where the source is, its colour…) come from the core crate,
//! and the picture goes straight to the effect pass as a texture.
//!
//! A simulation is kept per effect (by what it depends on), with a
//! checkpoint every second of simulated time, so playing forwards takes a
//! step a frame and seeking back starts from the nearest checkpoint.

use reelforge_core::fluid::{step_inputs, FluidJob, EVERY};
use std::collections::HashMap;

const ENTRIES: [&str; 11] = ["obstacles", "inject", "curl", "confine", "advect_vel", "divergence", "jacobi_ab", "jacobi_ba", "project", "advect_dye", "to_texture"];
/// Pressure-solve iterations a step (an even number: the result ends in `pa`).
const ITERS: usize = 30;
/// Steps encoded per submission (and uniform slots).
const CHUNK: usize = 32;
/// Simulations and checkpoints kept.
const KEEP: usize = 4;
const CHECKPOINTS: usize = 60;
/// Bytes per uniform slot (the usual dynamic-offset alignment).
const SLOT: u64 = 256;

struct Sim {
    w: u32,
    h: u32,
    step: usize,
    vel: wgpu::Buffer,
    dye: wgpu::Buffer,
    group: wgpu::BindGroup,
    out: wgpu::Texture,
    checkpoints: Vec<(usize, wgpu::Buffer, wgpu::Buffer)>,
    /// Which layer texture the obstacle pass reads (0: none).
    layer_id: u64,
    used: u64,
    // Kept alive for the bind group.
    _bufs: Vec<wgpu::Buffer>,
}

pub(crate) struct FluidGpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    layout: wgpu::BindGroupLayout,
    pipelines: HashMap<&'static str, wgpu::ComputePipeline>,
    uniforms: wgpu::Buffer,
    sampler: wgpu::Sampler,
    blank: wgpu::TextureView,
    sims: HashMap<u64, Sim>,
    tick: u64,
}

fn step_bytes(w: u32, h: u32, obstacles: bool, i: &reelforge_core::fluid::Inputs, keep: f32) -> Vec<u8> {
    let mut v = Vec::with_capacity(64);
    for x in [w, h, u32::from(obstacles), 0] {
        v.extend(x.to_le_bytes());
    }
    for x in [i.src[0], i.src[1], i.radius, i.amount, i.push[0], i.push[1], i.rise, i.swirl, i.color[0], i.color[1], i.color[2], keep] {
        v.extend(x.to_le_bytes());
    }
    v
}

impl FluidGpu {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        use wgpu::{BindGroupLayoutEntry as E, BindingType as B, ShaderStages as S};
        let storage = |binding| E { binding, visibility: S::COMPUTE, ty: B::Buffer { ty: wgpu::BufferBindingType::Storage { read_only: false }, has_dynamic_offset: false, min_binding_size: None }, count: None };
        let mut entries = vec![E { binding: 0, visibility: S::COMPUTE, ty: B::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: true, min_binding_size: wgpu::BufferSize::new(64) }, count: None }];
        entries.extend((1..=8).map(storage));
        entries.push(E { binding: 9, visibility: S::COMPUTE, ty: B::StorageTexture { access: wgpu::StorageTextureAccess::WriteOnly, format: wgpu::TextureFormat::Rgba8Unorm, view_dimension: wgpu::TextureViewDimension::D2 }, count: None });
        entries.push(E { binding: 10, visibility: S::COMPUTE, ty: B::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false }, count: None });
        entries.push(E { binding: 11, visibility: S::COMPUTE, ty: B::Sampler(wgpu::SamplerBindingType::Filtering), count: None });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("fluid"), entries: &entries });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("fluid"), bind_group_layouts: &[Some(&layout)], immediate_size: 0 });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("fluid"), source: wgpu::ShaderSource::Wgsl(include_str!("fluid.wgsl").into()) });
        let pipelines = ENTRIES
            .iter()
            .map(|e| {
                (*e, device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor { label: Some(e), layout: Some(&pl), module: &module, entry_point: Some(e), compilation_options: Default::default(), cache: None }))
            })
            .collect();
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor { label: Some("fluid steps"), size: SLOT * CHUNK as u64, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor { label: Some("fluid"), mag_filter: wgpu::FilterMode::Linear, min_filter: wgpu::FilterMode::Linear, ..Default::default() });
        let blank = device
            .create_texture(&wgpu::TextureDescriptor {
                label: Some("no layer"),
                size: wgpu::Extent3d { width: 1, height: 1, depth_or_array_layers: 1 },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            })
            .create_view(&Default::default());
        Self { device: device.clone(), queue: queue.clone(), layout, pipelines, uniforms, sampler, blank, sims: HashMap::new(), tick: 0 }
    }

    fn buffer(&self, label: &str, bytes: u64) -> wgpu::Buffer {
        self.device.create_buffer(&wgpu::BufferDescriptor { label: Some(label), size: bytes.max(16), usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false })
    }

    fn make(&self, w: u32, h: u32, layer: Option<&wgpu::TextureView>, layer_id: u64) -> Sim {
        let n = w as u64 * h as u64;
        let vel = self.buffer("fluid velocity", n * 8);
        let vel_tmp = self.buffer("fluid velocity'", n * 8);
        let dye = self.buffer("fluid colour", n * 16);
        let dye_tmp = self.buffer("fluid colour'", n * 16);
        let scratch = self.buffer("fluid scratch", n * 4);
        let pa = self.buffer("fluid pressure", n * 4);
        let pb = self.buffer("fluid pressure'", n * 4);
        let obs = self.buffer("fluid obstacles", n * 4);
        let out = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("fluid picture"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let out_view = out.create_view(&Default::default());
        let bufs = [&vel, &vel_tmp, &dye, &dye_tmp, &scratch, &pa, &pb, &obs];
        let mut entries = vec![wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding { buffer: &self.uniforms, offset: 0, size: wgpu::BufferSize::new(64) }) }];
        entries.extend(bufs.iter().enumerate().map(|(k, b)| wgpu::BindGroupEntry { binding: k as u32 + 1, resource: b.as_entire_binding() }));
        entries.push(wgpu::BindGroupEntry { binding: 9, resource: wgpu::BindingResource::TextureView(&out_view) });
        entries.push(wgpu::BindGroupEntry { binding: 10, resource: wgpu::BindingResource::TextureView(layer.unwrap_or(&self.blank)) });
        entries.push(wgpu::BindGroupEntry { binding: 11, resource: wgpu::BindingResource::Sampler(&self.sampler) });
        let group = self.device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("fluid"), layout: &self.layout, entries: &entries });
        let mut enc = self.device.create_command_encoder(&Default::default());
        enc.clear_buffer(&vel, 0, None);
        enc.clear_buffer(&dye, 0, None);
        enc.clear_buffer(&obs, 0, None);
        self.queue.submit([enc.finish()]);
        Sim { w, h, step: 0, vel: vel.clone(), dye: dye.clone(), group, out, checkpoints: Vec::new(), layer_id, used: 0, _bufs: vec![vel, vel_tmp, dye, dye_tmp, scratch, pa, pb, obs] }
    }

    /// Simulate job `job` up to its step and return its picture (a
    /// `w`×`h` texture: straight sRGB colour and coverage). `layer` is the
    /// layer's own picture, for flowing round its shape.
    pub fn step_to(&mut self, job: &FluidJob, layer: Option<(&wgpu::TextureView, u64)>) -> Option<wgpu::Texture> {
        let (w, h) = (job.w as u32, job.h as u32);
        if w < 4 || h < 4 {
            return None;
        }
        self.tick += 1;
        let layer = layer.filter(|_| job.obstacles);
        let layer_id = layer.map_or(0, |l| l.1);
        let fresh = !matches!(self.sims.get(&job.key), Some(s) if s.w == w && s.h == h && s.layer_id == layer_id);
        if fresh {
            if self.sims.len() >= KEEP {
                if let Some(old) = self.sims.iter().min_by_key(|(_, s)| s.used).map(|(k, _)| *k) {
                    self.sims.remove(&old);
                }
            }
            let sim = self.make(w, h, layer.map(|l| l.0), layer_id);
            self.sims.insert(job.key, sim);
        }
        let mut sim = self.sims.remove(&job.key)?;
        sim.used = self.tick;
        let target = job.step;
        if target < sim.step {
            // Back to the nearest checkpoint (or the start).
            let best = sim.checkpoints.iter().filter(|(s, _, _)| *s <= target).max_by_key(|(s, _, _)| *s);
            let mut enc = self.device.create_command_encoder(&Default::default());
            match best {
                Some((s, v, d)) => {
                    enc.copy_buffer_to_buffer(v, 0, &sim.vel, 0, v.size());
                    enc.copy_buffer_to_buffer(d, 0, &sim.dye, 0, d.size());
                    sim.step = *s;
                }
                None => {
                    enc.clear_buffer(&sim.vel, 0, None);
                    enc.clear_buffer(&sim.dye, 0, None);
                    sim.step = 0;
                }
            }
            self.queue.submit([enc.finish()]);
        }
        let n = w as u64 * h as u64;
        let groups = (w.div_ceil(8), h.div_ceil(8));
        let size = job.size;
        // Further on from a checkpoint kept on disk? (see `simstore`)
        let store = crate::simstore::store();
        let lens = [n * 8, n * 16];
        let shader = include_str!("fluid.wgsl");
        if let Some(store) = &store {
            let mut at = target / EVERY * EVERY;
            for _ in 0..600 {
                if at <= sim.step || at == 0 {
                    break;
                }
                let key = crate::simstore::checkpoint_key(job.key, at, &lens, shader);
                if store.has(key) {
                    if let Some(parts) = store.load(key).as_deref().and_then(|b| crate::simstore::unpack(b, &lens).map(|p| p.iter().map(|x| x.to_vec()).collect::<Vec<_>>())) {
                        self.queue.write_buffer(&sim.vel, 0, &parts[0]);
                        self.queue.write_buffer(&sim.dye, 0, &parts[1]);
                        let mut enc = self.device.create_command_encoder(&Default::default());
                        if layer.is_some() {
                            // (what's solid is worked out at the start, which was skipped)
                            let inp = step_inputs(&job.effect, at, job.w, job.h, size);
                            self.queue.write_buffer(&self.uniforms, 0, &step_bytes(w, h, true, &inp, 1.0));
                            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("obstacles"), timestamp_writes: None });
                            pass.set_pipeline(&self.pipelines["obstacles"]);
                            pass.set_bind_group(0, &sim.group, &[0]);
                            pass.dispatch_workgroups(groups.0, groups.1, 1);
                        }
                        self.queue.submit([enc.finish()]);
                        sim.step = at;
                    }
                    break;
                }
                at -= EVERY;
            }
        }
        while sim.step < target {
            let todo = (target - sim.step).min(CHUNK);
            for k in 0..todo {
                let inp = step_inputs(&job.effect, sim.step + k, job.w, job.h, size);
                let keep = (1.0 - inp.fade / reelforge_core::fluid::RATE as f32).clamp(0.0, 1.0);
                self.queue.write_buffer(&self.uniforms, k as u64 * SLOT, &step_bytes(w, h, layer.is_some(), &inp, keep));
            }
            let mut enc = self.device.create_command_encoder(&Default::default());
            let mut fresh = Vec::new();
            for k in 0..todo {
                let off = (k as u64 * SLOT) as u32;
                let run = |enc: &mut wgpu::CommandEncoder, entry: &str| {
                    let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some(entry), timestamp_writes: None });
                    pass.set_pipeline(&self.pipelines[entry]);
                    pass.set_bind_group(0, &sim.group, &[off]);
                    pass.dispatch_workgroups(groups.0, groups.1, 1);
                };
                if layer.is_some() && sim.step + k == 0 {
                    run(&mut enc, "obstacles");
                }
                run(&mut enc, "inject");
                run(&mut enc, "curl");
                run(&mut enc, "confine");
                enc.copy_buffer_to_buffer(&sim._bufs[1], 0, &sim.vel, 0, n * 8);
                run(&mut enc, "advect_vel");
                enc.copy_buffer_to_buffer(&sim._bufs[1], 0, &sim.vel, 0, n * 8);
                run(&mut enc, "divergence");
                for it in 0..ITERS {
                    run(&mut enc, if it % 2 == 0 { "jacobi_ab" } else { "jacobi_ba" });
                }
                run(&mut enc, "project");
                run(&mut enc, "advect_dye");
                enc.copy_buffer_to_buffer(&sim._bufs[3], 0, &sim.dye, 0, n * 16);
                let now = sim.step + k + 1;
                if now % EVERY == 0 && !sim.checkpoints.iter().any(|(s, _, _)| *s == now) {
                    let v = self.buffer("fluid checkpoint", n * 8);
                    let d = self.buffer("fluid checkpoint", n * 16);
                    enc.copy_buffer_to_buffer(&sim.vel, 0, &v, 0, n * 8);
                    enc.copy_buffer_to_buffer(&sim.dye, 0, &d, 0, n * 16);
                    sim.checkpoints.push((now, v, d));
                    fresh.push(now);
                    if sim.checkpoints.len() > CHECKPOINTS {
                        // Keep the first and thin out the rest.
                        sim.checkpoints.remove(1);
                    }
                }
            }
            self.queue.submit([enc.finish()]);
            sim.step += todo;
            if let Some(store) = &store {
                for at in fresh {
                    let key = crate::simstore::checkpoint_key(job.key, at, &lens, shader);
                    let Some((_, v, d)) = sim.checkpoints.iter().find(|c| c.0 == at) else { continue };
                    if !store.has(key) {
                        if let Some(parts) = crate::simstore::read_back(&self.device, &self.queue, &[(v, lens[0]), (d, lens[1])]) {
                            store.save(key, crate::simstore::pack(&parts));
                        }
                    }
                }
            }
        }
        // The picture (slot 0 told this grid's size: another may have used it last).
        let inp = step_inputs(&job.effect, target, job.w, job.h, size);
        self.queue.write_buffer(&self.uniforms, 0, &step_bytes(w, h, layer.is_some(), &inp, 1.0));
        let mut enc = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("to_texture"), timestamp_writes: None });
            pass.set_pipeline(&self.pipelines["to_texture"]);
            pass.set_bind_group(0, &sim.group, &[0]);
            pass.dispatch_workgroups(groups.0, groups.1, 1);
        }
        self.queue.submit([enc.finish()]);
        let out = sim.out.clone();
        self.sims.insert(job.key, sim);
        Some(out)
    }
}
