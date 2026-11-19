//! The fluid effect simulated on the graphics card (see `fluid.wgsl` and
//! `reelforge_core::fluid`): the grid lives on the card, each step's few
//! settings (where the source is, its colour…) come from the core crate,
//! and the picture goes straight to the effect pass as a texture.
//!
//! A simulation is kept per effect (by what it depends on), with a
//! checkpoint every second of simulated time, so playing forwards takes a
//! step a frame and seeking back starts from the nearest checkpoint.
//!
//! With obstacles on, each step reads the layer's picture as it is now (a new
//! one each frame of moving footage), so the fluid flows round an object as it
//! moves and is pushed by it. After a jump the scene first steps the
//! simulation through the frames in between (`reelforge_export::scene`,
//! [`FluidGpu::resume_step`]), as for the water.

use reelforge_core::fluid::{step_inputs, FluidJob, EVERY};
use std::collections::HashMap;

const ENTRIES: [&str; 14] = ["obstacles", "motion_sum", "motion", "seep", "inject", "curl", "confine", "advect_vel", "divergence", "jacobi_ab", "jacobi_ba", "project", "advect_dye", "to_texture"];
/// Pressure-solve iterations a step (an even number: the result ends in `pa`).
const ITERS: usize = 30;
/// Steps encoded per submission (and uniform slots).
const CHUNK: usize = 32;
/// Simulations and checkpoints kept.
const KEEP: usize = 4;
const CHECKPOINTS: usize = 60;
/// Bytes per uniform slot (the usual dynamic-offset alignment).
const SLOT: u64 = 256;
/// How far back a simulation looks for a checkpoint on disk (in checkpoints:
/// ten minutes of simulated time).
const DISK_PROBES: usize = 600;

struct Sim {
    w: u32,
    h: u32,
    step: usize,
    vel: wgpu::Buffer,
    dye: wgpu::Buffer,
    out: wgpu::Texture,
    out_view: wgpu::TextureView,
    checkpoints: Vec<(usize, wgpu::Buffer, wgpu::Buffer)>,
    /// Past this step its state is approximate (a long jump caught up on
    /// fewer frames than it spans): see [`FluidGpu::caught_up`].
    approx_after: Option<usize>,
    /// A copy being filled in (see [`FluidGpu::fork`]).
    forked_at: Option<usize>,
    /// The step whose obstacles were read last (`None`: none since the start
    /// or a seek, so the next read has nothing to compare with).
    obs_at: Option<usize>,
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

fn step_bytes(w: u32, h: u32, obstacles: bool, fresh: bool, i: &reelforge_core::fluid::Inputs, keep: f32) -> Vec<u8> {
    let mut v = Vec::with_capacity(64);
    for x in [w, h, u32::from(obstacles), u32::from(fresh)] {
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

    fn make(&self, w: u32, h: u32) -> Sim {
        let n = w as u64 * h as u64;
        let vel = self.buffer("fluid velocity", n * 8);
        let vel_tmp = self.buffer("fluid velocity'", n * 8);
        let dye = self.buffer("fluid colour", n * 16);
        let dye_tmp = self.buffer("fluid colour'", n * 16);
        let scratch = self.buffer("fluid scratch", n * 4);
        let pa = self.buffer("fluid pressure", n * 4);
        let pb = self.buffer("fluid pressure'", n * 4);
        // (and the solid's motion after the cells)
        let obs = self.buffer("fluid obstacles", (n + 1) * 8);
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
        let mut enc = self.device.create_command_encoder(&Default::default());
        enc.clear_buffer(&vel, 0, None);
        enc.clear_buffer(&dye, 0, None);
        enc.clear_buffer(&obs, 0, None);
        self.queue.submit([enc.finish()]);
        Sim { w, h, step: 0, vel: vel.clone(), dye: dye.clone(), out, out_view, checkpoints: Vec::new(), approx_after: None, forked_at: None, obs_at: None, used: 0, _bufs: vec![vel, vel_tmp, dye, dye_tmp, scratch, pa, pb, obs] }
    }

    /// See [`crate::watergpu::WaterGpu::caught_up`].
    pub fn caught_up(&mut self, job: &FluidJob, from: usize, exact: bool) {
        let Some(sim) = self.sims.get_mut(&job.key) else { return };
        sim.approx_after = match (sim.approx_after, exact) {
            (Some(a), true) if from <= a => None,
            (a, true) => a,
            (Some(a), false) => Some(a.min(from)),
            (None, false) => Some(from),
        };
    }

    /// See [`crate::watergpu::WaterGpu::fork`].
    pub fn fork(&mut self, job: &FluidJob, shadow: u64) -> Option<usize> {
        let real = self.sims.get(&job.key)?;
        let a = real.approx_after?;
        let (w, h) = (real.w, real.h);
        let from = real.checkpoints.iter().filter(|c| c.0 <= a).max_by_key(|c| c.0).map(|(s, v, d)| (*s, v.clone(), d.clone()));
        let mut copy = self.make(w, h);
        if let Some((step, v, d)) = from {
            let mut enc = self.device.create_command_encoder(&Default::default());
            enc.copy_buffer_to_buffer(&v, 0, &copy.vel, 0, v.size());
            enc.copy_buffer_to_buffer(&d, 0, &copy.dye, 0, d.size());
            self.queue.submit([enc.finish()]);
            copy.step = step;
        }
        let at = copy.step;
        copy.forked_at = Some(at);
        copy.used = self.tick;
        self.sims.insert(shadow, copy);
        Some(at)
    }

    /// See [`crate::watergpu::WaterGpu::adopt`].
    pub fn adopt(&mut self, key: u64, shadow: u64) {
        let Some(mut copy) = self.sims.remove(&shadow) else { return };
        let from = copy.forked_at.take().unwrap_or(0);
        if let Some(real) = self.sims.remove(&key) {
            let mut kept: Vec<_> = real.checkpoints.into_iter().filter(|c| c.0 <= from).collect();
            kept.append(&mut copy.checkpoints);
            kept.sort_by_key(|c| c.0);
            kept.dedup_by_key(|c| c.0);
            while kept.len() > CHECKPOINTS {
                kept.remove(1);
            }
            copy.checkpoints = kept;
        }
        copy.approx_after = None;
        if let Some(store) = crate::simstore::store() {
            let n = copy.w as u64 * copy.h as u64;
            let lens = [n * 8, n * 16];
            for (at, v, d) in copy.checkpoints.iter().filter(|c| c.0 > from) {
                let k = crate::simstore::checkpoint_key(key, *at, &lens, include_str!("fluid.wgsl"));
                if !store.has(k) {
                    if let Some(parts) = crate::simstore::read_back(&self.device, &self.queue, &[(v, lens[0]), (d, lens[1])]) {
                        store.save(k, crate::simstore::pack(&parts));
                    }
                }
            }
        }
        self.sims.insert(key, copy);
    }

    /// See [`crate::watergpu::WaterGpu::discard`].
    pub fn discard(&mut self, shadow: u64) {
        self.sims.remove(&shadow);
    }

    /// `sim`'s bind group, reading `layer` (the layer's picture now) for obstacles.
    fn group(&self, sim: &Sim, layer: Option<&wgpu::TextureView>) -> wgpu::BindGroup {
        let mut entries = vec![wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding { buffer: &self.uniforms, offset: 0, size: wgpu::BufferSize::new(64) }) }];
        entries.extend(sim._bufs.iter().enumerate().map(|(k, b)| wgpu::BindGroupEntry { binding: k as u32 + 1, resource: b.as_entire_binding() }));
        entries.push(wgpu::BindGroupEntry { binding: 9, resource: wgpu::BindingResource::TextureView(&sim.out_view) });
        entries.push(wgpu::BindGroupEntry { binding: 10, resource: wgpu::BindingResource::TextureView(layer.unwrap_or(&self.blank)) });
        entries.push(wgpu::BindGroupEntry { binding: 11, resource: wgpu::BindingResource::Sampler(&self.sampler) });
        self.device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some("fluid"), layout: &self.layout, entries: &entries })
    }

    /// The step `job` would start from: where its simulation is if that's not
    /// past it, else the nearest checkpoint before it (in memory or on disk),
    /// else the beginning. (So the scene can step it through the frames in
    /// between first.)
    pub fn resume_step(&self, job: &FluidJob) -> usize {
        let (w, h) = (job.w as u32, job.h as u32);
        let mem = match self.sims.get(&job.key) {
            Some(s) if s.w == w && s.h == h => {
                if s.step <= job.step {
                    s.step
                } else {
                    s.checkpoints.iter().map(|c| c.0).filter(|&c| c <= job.step).max().unwrap_or(0)
                }
            }
            _ => 0,
        };
        let Some(store) = crate::simstore::store() else { return mem };
        let n = u64::from(w) * u64::from(h);
        let lens = [n * 8, n * 16];
        let mut at = job.step / EVERY * EVERY;
        for _ in 0..DISK_PROBES {
            if at <= mem || at == 0 {
                break;
            }
            if store.has(crate::simstore::checkpoint_key(job.key, at, &lens, include_str!("fluid.wgsl"))) {
                return at;
            }
            at -= EVERY;
        }
        mem
    }

    /// Simulate job `job` up to its step and return its picture (a
    /// `w`×`h` texture: straight sRGB colour and coverage). `layer` is the
    /// layer's own picture now, for flowing round its shape.
    pub fn step_to(&mut self, job: &FluidJob, layer: Option<&wgpu::TextureView>) -> Option<wgpu::Texture> {
        let (w, h) = (job.w as u32, job.h as u32);
        if w < 4 || h < 4 {
            return None;
        }
        self.tick += 1;
        let layer = layer.filter(|_| job.obstacles);
        let fresh = !matches!(self.sims.get(&job.key), Some(s) if s.w == w && s.h == h);
        if fresh {
            if self.sims.len() >= KEEP {
                if let Some(old) = self.sims.iter().min_by_key(|(_, s)| s.used).map(|(k, _)| *k) {
                    self.sims.remove(&old);
                }
            }
            let sim = self.make(w, h);
            self.sims.insert(job.key, sim);
        }
        let mut sim = self.sims.remove(&job.key)?;
        sim.used = self.tick;
        let group = self.group(&sim, layer);
        let target = job.step;
        if target < sim.step {
            sim.obs_at = None;
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
            // Back at or before where it stopped being exact: exact again, and
            // the approximate checkpoints after go.
            if sim.approx_after.is_some_and(|a| sim.step <= a) {
                let at = sim.step;
                sim.checkpoints.retain(|c| c.0 <= at);
                sim.approx_after = None;
            }
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
                    if let Some([v, d]) = store.load(key).as_deref().and_then(|b| crate::simstore::unpack(b, &lens).map(|p| p.iter().map(|x| x.to_vec()).collect::<Vec<_>>())).and_then(|p| <[Vec<u8>; 2]>::try_from(p).ok()) {
                        self.queue.write_buffer(&sim.vel, 0, &v);
                        self.queue.write_buffer(&sim.dye, 0, &d);
                        // (what's on disk is exact: the approximate tail in memory goes)
                        if let Some(a) = sim.approx_after.take() {
                            sim.checkpoints.retain(|c| c.0 <= a);
                        }
                        // (no obstacles read for the step before it)
                        sim.obs_at = None;
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
                let step = sim.step + k;
                let inp = step_inputs(&job.effect, step, job.w, job.h, size);
                let keep = (1.0 - inp.fade / reelforge_core::fluid::RATE as f32).clamp(0.0, 1.0);
                // The obstacles read the step before are what this step's are compared with.
                let fresh = k == 0 && (step == 0 || sim.obs_at != Some(step - 1));
                self.queue.write_buffer(&self.uniforms, k as u64 * SLOT, &step_bytes(w, h, layer.is_some(), fresh, &inp, keep));
            }
            if layer.is_some() {
                sim.obs_at = Some(sim.step + todo - 1);
            }
            let mut enc = self.device.create_command_encoder(&Default::default());
            let mut fresh = Vec::new();
            for k in 0..todo {
                let off = (k as u64 * SLOT) as u32;
                let run_on = |enc: &mut wgpu::CommandEncoder, entry: &str, groups: (u32, u32)| {
                    let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some(entry), timestamp_writes: None });
                    pass.set_pipeline(&self.pipelines[entry]);
                    pass.set_bind_group(0, &group, &[off]);
                    pass.dispatch_workgroups(groups.0, groups.1, 1);
                };
                let run = |enc: &mut wgpu::CommandEncoder, entry: &str| run_on(enc, entry, groups);
                if layer.is_some() {
                    run(&mut enc, "obstacles");
                    run(&mut enc, "motion_sum");
                    run_on(&mut enc, "motion", (1, 1));
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
                if layer.is_some() {
                    run(&mut enc, "seep");
                    enc.copy_buffer_to_buffer(&sim._bufs[3], 0, &sim.dye, 0, n * 16);
                }
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
                    // (approximate ones are never kept as if they were the real thing)
                    if sim.approx_after.is_some_and(|a| at > a) || sim.forked_at.is_some() {
                        continue;
                    }
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
        self.queue.write_buffer(&self.uniforms, 0, &step_bytes(w, h, layer.is_some(), true, &inp, 1.0));
        let mut enc = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("to_texture"), timestamp_writes: None });
            pass.set_pipeline(&self.pipelines["to_texture"]);
            pass.set_bind_group(0, &group, &[0]);
            pass.dispatch_workgroups(groups.0, groups.1, 1);
        }
        self.queue.submit([enc.finish()]);
        let out = sim.out.clone();
        self.sims.insert(job.key, sim);
        Some(out)
    }
}
