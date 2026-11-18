//! The water surface and liquid effects simulated on the graphics card (see
//! `surface.wgsl`, `liquid.wgsl` and `reelforge_core::water`). Like the fluid
//! (`fluidgpu`): a simulation per effect (by what it depends on), stepped on to
//! the frame's moment, with a checkpoint every second of simulated time so
//! seeking back starts from the nearest one. Each step reads what's solid
//! from the layer's picture as it is now, so moving objects push the water
//! (and after a jump the scene steps it through the frames in between first:
//! `reelforge_export::scene`, `resume_step`).
//!
//! The picture goes to the effect pass as a texture: for the surface, its
//! height, slope and what's solid; for the liquid, how much water there is
//! (on a grid twice as fine as the simulation's) and its foam.

use crate::simstore;
use reelforge_core::effects::EffectKind;
use reelforge_core::fluid::FluidJob;
use reelforge_core::water::{self, LiquidStep, SurfaceStep};
use std::collections::HashMap;

/// Steps encoded per submission (and uniform slots).
const CHUNK: usize = 32;
/// Bytes per uniform slot (the usual dynamic-offset alignment).
const SLOT: u64 = 256;
const KEEP: usize = 3;
const CHECKPOINTS: usize = 40;
/// Liquid pressure-solve iterations a step (red-black over-relaxation, warm-started).
const ITERS: usize = 40;

/// How far back a simulation looks for a checkpoint on disk (in checkpoints:
/// ten minutes of simulated time).
const DISK_PROBES: usize = 600;

/// The shader a kind of simulation runs (part of its checkpoints' keys).
fn shader(kind: EffectKind) -> &'static str {
    if kind == EffectKind::Liquid { include_str!("liquid.wgsl") } else { include_str!("surface.wgsl") }
}

const SURFACE_ENTRIES: [&str; 5] = ["solids", "drive", "wave", "commit", "picture"];
const LIQUID_ENTRIES: [&str; 13] = ["solids", "fill", "emit", "advance", "p2g", "grid", "forces", "divergence", "red", "black", "project", "g2p", "splat"];

struct Kit {
    layout: wgpu::BindGroupLayout,
    pipelines: HashMap<&'static str, wgpu::ComputePipeline>,
}

/// Where each grid sits in a simulation's buffers (in 4-byte words).
#[derive(Clone, Copy, Default)]
struct Offsets {
    g: [u32; 12],
    a: [u32; 8],
    g_len: u64,
    a_len: u64,
}

struct Sim {
    kind: EffectKind,
    w: u32,
    h: u32,
    step: usize,
    /// What's checkpointed: the surface's grids, or the liquid's particles and counter.
    state: Vec<(wgpu::Buffer, u64)>,
    group: wgpu::BindGroup,
    out: wgpu::Texture,
    checkpoints: Vec<(usize, Vec<wgpu::Buffer>)>,
    used: u64,
    off: Offsets,
    /// The liquid's atomic buffer (cleared each step after its counter).
    atomics: Option<wgpu::Buffer>,
    cap: u32,
    /// The liquid's picture grid.
    rw: u32,
    rh: u32,
}

pub(crate) struct WaterGpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    surface: Kit,
    liquid: Kit,
    layer_layout: wgpu::BindGroupLayout,
    uniforms: wgpu::Buffer,
    sampler: wgpu::Sampler,
    blank: wgpu::TextureView,
    sims: HashMap<u64, Sim>,
    tick: u64,
}

fn words(v: &mut Vec<u8>, xs: &[u32]) {
    for x in xs {
        v.extend(x.to_le_bytes());
    }
}
fn floats(v: &mut Vec<u8>, xs: &[f32]) {
    for x in xs {
        v.extend(x.to_le_bytes());
    }
}

fn surface_bytes(w: u32, h: u32, s: &SurfaceStep, off: &Offsets) -> Vec<u8> {
    let mut v = Vec::with_capacity(256);
    words(&mut v, &[w, h, s.solids.kind, s.drops.len().min(water::MAX_DROPS) as u32]);
    floats(&mut v, &[s.c2, s.keep, s.wind, s.wake]);
    floats(&mut v, &s.wave);
    floats(&mut v, &[s.wave_dir[0], s.wave_dir[1], s.solids.tolerance, s.step as f32]);
    words(&mut v, &off.g[0..4]);
    words(&mut v, &[off.g[4], off.g[5], 0, 0]);
    for k in 0..water::MAX_DROPS {
        floats(&mut v, &s.drops.get(k).copied().unwrap_or([0.0; 4]));
    }
    v
}

#[allow(clippy::too_many_arguments)]
fn liquid_bytes(sim: &Sim, s: &LiquidStep, emit: u32, droplets: f32) -> Vec<u8> {
    let o = &sim.off;
    let mut v = Vec::with_capacity(256);
    words(&mut v, &[sim.w, sim.h, sim.cap, emit, sim.rw, sim.rh, s.step, s.seed]);
    floats(&mut v, &[s.src[0], s.src[1], s.dir[0], s.dir[1], s.speed, s.width, s.gravity, s.flip, s.bounce, s.solids.tolerance, s.fill_to, droplets]);
    words(&mut v, &[u32::from(s.floor), u32::from(s.walls), s.solids.kind]);
    floats(&mut v, &[1.0 - s.viscosity * 0.08]);
    words(&mut v, &o.g[0..8]);
    words(&mut v, &[o.g[8], 0, 0, 0]);
    words(&mut v, &o.a[0..8]);
    v
}

impl WaterGpu {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        use wgpu::{BindGroupLayoutEntry as E, BindingType as B, ShaderStages as S};
        let storage = |binding| E { binding, visibility: S::COMPUTE, ty: B::Buffer { ty: wgpu::BufferBindingType::Storage { read_only: false }, has_dynamic_offset: false, min_binding_size: None }, count: None };
        let uniform = E { binding: 0, visibility: S::COMPUTE, ty: B::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: true, min_binding_size: wgpu::BufferSize::new(SLOT) }, count: None };
        let picture = |binding| E { binding, visibility: S::COMPUTE, ty: B::StorageTexture { access: wgpu::StorageTextureAccess::WriteOnly, format: wgpu::TextureFormat::Rgba16Float, view_dimension: wgpu::TextureViewDimension::D2 }, count: None };
        let layer_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("water layer"),
            entries: &[
                E { binding: 0, visibility: S::COMPUTE, ty: B::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false }, count: None },
                E { binding: 1, visibility: S::COMPUTE, ty: B::Sampler(wgpu::SamplerBindingType::Filtering), count: None },
            ],
        });
        let kit = |label: &str, entries: Vec<E>, source: &str, names: &[&'static str]| {
            let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some(label), entries: &entries });
            let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some(label), bind_group_layouts: &[Some(&layout), Some(&layer_layout)], immediate_size: 0 });
            let module = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some(label), source: wgpu::ShaderSource::Wgsl(source.into()) });
            let pipelines = names
                .iter()
                .map(|e| (*e, device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor { label: Some(e), layout: Some(&pl), module: &module, entry_point: Some(e), compilation_options: Default::default(), cache: None })))
                .collect();
            Kit { layout, pipelines }
        };
        let surface = kit("water surface", vec![uniform, storage(1), picture(2)], include_str!("surface.wgsl"), &SURFACE_ENTRIES);
        let mut liquid_names = LIQUID_ENTRIES.to_vec();
        liquid_names.push("picture");
        let liquid = kit("liquid", vec![uniform, storage(1), storage(2), storage(3), picture(4)], include_str!("liquid.wgsl"), &liquid_names);
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor { label: Some("water steps"), size: SLOT * CHUNK as u64, usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor { label: Some("water"), mag_filter: wgpu::FilterMode::Linear, min_filter: wgpu::FilterMode::Linear, ..Default::default() });
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
        Self { device: device.clone(), queue: queue.clone(), surface, liquid, layer_layout, uniforms, sampler, blank, sims: HashMap::new(), tick: 0 }
    }

    fn buffer(&self, label: &str, bytes: u64) -> wgpu::Buffer {
        self.device.create_buffer(&wgpu::BufferDescriptor { label: Some(label), size: bytes.max(16).next_multiple_of(4), usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false })
    }

    fn picture_texture(&self, w: u32, h: u32) -> wgpu::Texture {
        self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("water picture"),
            size: wgpu::Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        })
    }

    fn make(&self, kind: EffectKind, w: u32, h: u32) -> Option<Sim> {
        let n = w as u64 * h as u64;
        let limit = self.device.limits().max_storage_buffer_binding_size as u64;
        let uniform = wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding { buffer: &self.uniforms, offset: 0, size: wgpu::BufferSize::new(SLOT) }) };
        let mut enc = self.device.create_command_encoder(&Default::default());
        let sim = if kind == EffectKind::WaterSurface {
            // height, rate, next height, next rate, solid, change in solid
            let mut off = Offsets::default();
            for k in 0..6 {
                off.g[k] = (n * k as u64) as u32;
            }
            off.g_len = n * 6 * 4;
            if off.g_len > limit {
                return None;
            }
            let grids = self.buffer("water surface", off.g_len);
            let out = self.picture_texture(w, h);
            let view = out.create_view(&Default::default());
            let group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("water surface"),
                layout: &self.surface.layout,
                entries: &[uniform, wgpu::BindGroupEntry { binding: 1, resource: grids.as_entire_binding() }, wgpu::BindGroupEntry { binding: 2, resource: wgpu::BindingResource::TextureView(&view) }],
            });
            enc.clear_buffer(&grids, 0, None);
            Sim { kind, w, h, step: 0, state: vec![(grids, off.g_len)], group, out, checkpoints: Vec::new(), used: 0, off, atomics: None, cap: 0, rw: w, rh: h }
        } else {
            let cap = water::capacity(w as usize, h as usize) as u32;
            let (rw, rh) = (w * 2, h * 2);
            let faces_u = (w as u64 + 1) * h as u64;
            let faces_v = w as u64 * (h as u64 + 1);
            let r = rw as u64 * rh as u64;
            let mut off = Offsets::default();
            // floats: u, v, u before, v before, pressure a, pressure b, divergence, solid, kind
            let sizes_g = [faces_u, faces_v, faces_u, faces_v, n, n, n, n, n];
            let mut at = 0u64;
            for (k, s) in sizes_g.iter().enumerate() {
                off.g[k] = at as u32;
                at += s;
            }
            off.g_len = at * 4;
            // ints: (counter: 16 words) u sum, u weight, v sum, v weight, count, density, foam
            let sizes_a = [faces_u, faces_u, faces_v, faces_v, n, r, r];
            let mut at = 16u64;
            for (k, s) in sizes_a.iter().enumerate() {
                off.a[k] = at as u32;
                at += s;
            }
            off.a_len = at * 4;
            let parts_len = cap as u64 * 16;
            if off.g_len > limit || off.a_len > limit || parts_len > limit {
                return None;
            }
            let parts = self.buffer("liquid particles", parts_len);
            let atomics = self.buffer("liquid sums", off.a_len);
            let grids = self.buffer("liquid grids", off.g_len);
            let out = self.picture_texture(rw, rh);
            let view = out.create_view(&Default::default());
            let group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("liquid"),
                layout: &self.liquid.layout,
                entries: &[
                    uniform,
                    wgpu::BindGroupEntry { binding: 1, resource: parts.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 2, resource: atomics.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 3, resource: grids.as_entire_binding() },
                    wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(&view) },
                ],
            });
            // Every particle starts out dead (x far off to the left).
            let dead: Vec<u8> = (0..cap).flat_map(|_| [-1.0e9f32, -1.0e9, 0.0, 0.0]).flat_map(f32::to_le_bytes).collect();
            self.queue.write_buffer(&parts, 0, &dead);
            enc.clear_buffer(&atomics, 0, None);
            enc.clear_buffer(&grids, 0, None);
            // (the grids too: the pressure carries over from step to step)
            Sim { kind, w, h, step: 0, state: vec![(parts, parts_len), (atomics.clone(), 64), (grids, off.g_len)], group, out, checkpoints: Vec::new(), used: 0, off, atomics: Some(atomics), cap, rw, rh }
        };
        self.queue.submit([enc.finish()]);
        Some(sim)
    }

    /// Back to the start (a fresh simulation).
    fn reset(&self, sim: &mut Sim) {
        let mut enc = self.device.create_command_encoder(&Default::default());
        if sim.kind == EffectKind::Liquid {
            let dead: Vec<u8> = (0..sim.cap).flat_map(|_| [-1.0e9f32, -1.0e9, 0.0, 0.0]).flat_map(f32::to_le_bytes).collect();
            self.queue.write_buffer(&sim.state[0].0, 0, &dead);
            enc.clear_buffer(&sim.state[1].0, 0, None);
            enc.clear_buffer(&sim.state[2].0, 0, None);
        } else {
            enc.clear_buffer(&sim.state[0].0, 0, None);
        }
        self.queue.submit([enc.finish()]);
        sim.step = 0;
    }

    /// The step `job` would start from: where its simulation is if that's not
    /// past it, else the nearest checkpoint before it (in memory or on disk),
    /// else the beginning.
    pub fn resume_step(&self, job: &FluidJob) -> usize {
        let mem = match self.sims.get(&job.key) {
            Some(s) if s.w == job.w as u32 && s.h == job.h as u32 && s.kind == job.effect.kind => {
                if s.step <= job.step {
                    s.step
                } else {
                    s.checkpoints.iter().map(|c| c.0).filter(|&c| c <= job.step).max().unwrap_or(0)
                }
            }
            _ => 0,
        };
        let lens = match self.sims.get(&job.key) {
            Some(s) => s.state.iter().map(|b| b.1).collect(),
            None => match self.state_lens(job.effect.kind, job.w as u32, job.h as u32) {
                Some(l) => l,
                None => return mem,
            },
        };
        self.on_disk(job, &lens, mem).unwrap_or(mem)
    }

    /// The sizes of a fresh simulation's checkpointed buffers.
    fn state_lens(&self, kind: EffectKind, w: u32, h: u32) -> Option<Vec<u64>> {
        self.make(kind, w, h).map(|s| s.state.iter().map(|b| b.1).collect())
    }

    /// The latest checkpoint on disk after step `after` and no later than the
    /// job's.
    fn on_disk(&self, job: &FluidJob, lens: &[u64], after: usize) -> Option<usize> {
        let store = simstore::store()?;
        let every = (water::rate(job.effect.kind).round() as usize).max(1);
        let mut at = job.step / every * every;
        for _ in 0..DISK_PROBES {
            if at <= after || at == 0 {
                return None;
            }
            if store.has(simstore::checkpoint_key(job.key, at, lens, shader(job.effect.kind))) {
                return Some(at);
            }
            at -= every;
        }
        None
    }

    /// Take the checkpoint at `step` from disk into `sim` (and keep it in
    /// memory too). False if it can't be read.
    fn load_from_disk(&self, sim: &mut Sim, key: u64, step: usize) -> bool {
        let Some(store) = simstore::store() else { return false };
        let lens: Vec<u64> = sim.state.iter().map(|b| b.1).collect();
        let Some(bytes) = store.load(simstore::checkpoint_key(key, step, &lens, shader(sim.kind))) else { return false };
        let Some(parts) = simstore::unpack(&bytes, &lens) else { return false };
        let mut enc = self.device.create_command_encoder(&Default::default());
        let mut copies = Vec::with_capacity(parts.len());
        for ((buf, len), part) in sim.state.iter().zip(&parts) {
            self.queue.write_buffer(buf, 0, part);
            let c = self.buffer("water checkpoint", *len);
            enc.copy_buffer_to_buffer(buf, 0, &c, 0, *len);
            copies.push(c);
        }
        self.queue.submit([enc.finish()]);
        sim.step = step;
        if !sim.checkpoints.iter().any(|(s, _)| *s == step) {
            sim.checkpoints.push((step, copies));
            sim.checkpoints.sort_by_key(|(s, _)| *s);
            if sim.checkpoints.len() > CHECKPOINTS {
                sim.checkpoints.remove(1);
            }
        }
        true
    }

    /// Simulate `job` (a water surface or liquid) up to its step and return its
    /// picture. `layer` is the layer's picture now, for what's solid.
    pub fn step_to(&mut self, job: &FluidJob, layer: Option<&wgpu::TextureView>) -> Option<wgpu::Texture> {
        let (w, h) = (job.w as u32, job.h as u32);
        let kind = job.effect.kind;
        if w < 4 || h < 4 || !matches!(kind, EffectKind::WaterSurface | EffectKind::Liquid) {
            return None;
        }
        self.tick += 1;
        let fresh = !matches!(self.sims.get(&job.key), Some(s) if s.w == w && s.h == h && s.kind == kind);
        if fresh {
            if self.sims.len() >= KEEP {
                if let Some(old) = self.sims.iter().min_by_key(|(_, s)| s.used).map(|(k, _)| *k) {
                    self.sims.remove(&old);
                }
            }
            let sim = self.make(kind, w, h)?;
            self.sims.insert(job.key, sim);
        }
        let mut sim = self.sims.remove(&job.key)?;
        sim.used = self.tick;
        let target = job.step;
        if target < sim.step {
            let best = sim.checkpoints.iter().filter(|(s, _)| *s <= target).max_by_key(|(s, _)| *s);
            match best {
                Some((s, bufs)) => {
                    let mut enc = self.device.create_command_encoder(&Default::default());
                    for ((dst, len), src) in sim.state.iter().zip(bufs) {
                        enc.copy_buffer_to_buffer(src, 0, dst, 0, *len);
                    }
                    self.queue.submit([enc.finish()]);
                    sim.step = *s;
                }
                None => self.reset(&mut sim),
            }
        }
        // Further on from a checkpoint kept on disk?
        let lens: Vec<u64> = sim.state.iter().map(|b| b.1).collect();
        if let Some(at) = self.on_disk(job, &lens, sim.step) {
            if !self.load_from_disk(&mut sim, job.key, at) {
                log::warn!("couldn't read a saved simulation checkpoint; working it out again");
            }
        }
        let store = simstore::store();
        let layer_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("water layer"),
            layout: &self.layer_layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(layer.unwrap_or(&self.blank)) }, wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.sampler) }],
        });
        let every = water::rate(kind).round() as usize;
        let droplets = (job.effect.value_at(reelforge_core::effects::liquid::DROPLETS, reelforge_core::Time::ZERO) / 100.0).clamp(0.0, 3.0);
        while sim.step < target {
            let todo = (target - sim.step).min(CHUNK);
            let mut emits = Vec::with_capacity(todo);
            for k in 0..todo {
                let bytes = if kind == EffectKind::WaterSurface {
                    surface_bytes(w, h, &water::surface_step(&job.effect, sim.step + k, job.w, job.h, job.size), &sim.off)
                } else {
                    let st = water::liquid_step(&job.effect, sim.step + k, job.w, job.h, job.size);
                    let emit = st.emit.min(sim.cap);
                    emits.push((emit, sim.step + k == 0 && st.fill_to < h as f32 - 0.5, st.fill_to));
                    liquid_bytes(&sim, &st, emit, droplets)
                };
                self.queue.write_buffer(&self.uniforms, k as u64 * SLOT, &bytes);
            }
            let mut enc = self.device.create_command_encoder(&Default::default());
            let mut fresh_checkpoints = Vec::new();
            for k in 0..todo {
                let off = (k as u64 * SLOT) as u32;
                if kind == EffectKind::WaterSurface {
                    self.surface_step(&mut enc, &sim, &layer_group, off);
                } else {
                    let (emit, fill, fill_to) = emits[k];
                    self.liquid_step(&mut enc, &sim, &layer_group, off, emit, fill.then_some(fill_to));
                }
                let now = sim.step + k + 1;
                if now % every == 0 && !sim.checkpoints.iter().any(|(s, _)| *s == now) {
                    let copies: Vec<wgpu::Buffer> = sim
                        .state
                        .iter()
                        .map(|(b, len)| {
                            let c = self.buffer("water checkpoint", *len);
                            enc.copy_buffer_to_buffer(b, 0, &c, 0, *len);
                            c
                        })
                        .collect();
                    sim.checkpoints.push((now, copies));
                    fresh_checkpoints.push(now);
                    if sim.checkpoints.len() > CHECKPOINTS {
                        // Keep the first and thin out the rest.
                        sim.checkpoints.remove(1);
                    }
                }
            }
            self.queue.submit([enc.finish()]);
            sim.step += todo;
            // New checkpoints go to disk too (read back, written in the background).
            if let Some(store) = &store {
                for at in fresh_checkpoints {
                    let key = simstore::checkpoint_key(job.key, at, &lens, shader(kind));
                    if store.has(key) {
                        continue;
                    }
                    let Some((_, bufs)) = sim.checkpoints.iter().find(|(s, _)| *s == at) else { continue };
                    let with_lens: Vec<(&wgpu::Buffer, u64)> = bufs.iter().zip(&lens).map(|(b, l)| (b, *l)).collect();
                    if let Some(parts) = simstore::read_back(&self.device, &self.queue, &with_lens) {
                        store.save(key, simstore::pack(&parts));
                    }
                }
            }
        }
        // The picture (slot 0 told this simulation's sizes: another may have used it last).
        let mut enc = self.device.create_command_encoder(&Default::default());
        if kind == EffectKind::WaterSurface {
            let st = water::surface_step(&job.effect, target, job.w, job.h, job.size);
            self.queue.write_buffer(&self.uniforms, 0, &surface_bytes(w, h, &st, &sim.off));
            self.run(&mut enc, &self.surface, "picture", &sim.group, &layer_group, 0, (w.div_ceil(8), h.div_ceil(8)));
        } else {
            let st = water::liquid_step(&job.effect, target, job.w, job.h, job.size);
            self.queue.write_buffer(&self.uniforms, 0, &liquid_bytes(&sim, &st, 0, droplets));
            if let Some(a) = &sim.atomics {
                let from = sim.off.a[5] as u64 * 4;
                enc.clear_buffer(a, from, Some(sim.off.a_len - from));
            }
            self.run(&mut enc, &self.liquid, "splat", &sim.group, &layer_group, 0, (sim.cap.div_ceil(256), 1));
            self.run(&mut enc, &self.liquid, "picture", &sim.group, &layer_group, 0, (sim.rw.div_ceil(8), sim.rh.div_ceil(8)));
        }
        self.queue.submit([enc.finish()]);
        let out = sim.out.clone();
        self.sims.insert(job.key, sim);
        Some(out)
    }

    #[allow(clippy::too_many_arguments)]
    fn run(&self, enc: &mut wgpu::CommandEncoder, kit: &Kit, entry: &str, group: &wgpu::BindGroup, layer: &wgpu::BindGroup, off: u32, groups: (u32, u32)) {
        let Some(p) = kit.pipelines.get(entry) else { return };
        if groups.0 == 0 || groups.1 == 0 {
            return;
        }
        let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some(entry), timestamp_writes: None });
        pass.set_pipeline(p);
        pass.set_bind_group(0, group, &[off]);
        pass.set_bind_group(1, layer, &[]);
        pass.dispatch_workgroups(groups.0, groups.1.min(65535), 1);
    }

    fn surface_step(&self, enc: &mut wgpu::CommandEncoder, sim: &Sim, layer: &wgpu::BindGroup, off: u32) {
        let cells = (sim.w.div_ceil(8), sim.h.div_ceil(8));
        self.run(enc, &self.surface, "solids", &sim.group, layer, off, cells);
        self.run(enc, &self.surface, "drive", &sim.group, layer, off, cells);
        self.run(enc, &self.surface, "wave", &sim.group, layer, off, cells);
        self.run(enc, &self.surface, "commit", &sim.group, layer, off, cells);
    }

    fn liquid_step(&self, enc: &mut wgpu::CommandEncoder, sim: &Sim, layer: &wgpu::BindGroup, off: u32, emit: u32, fill: Option<f32>) {
        let cells = (sim.w.div_ceil(8), sim.h.div_ceil(8));
        let faces = ((sim.w + 1).div_ceil(8), (sim.h + 1).div_ceil(8));
        let parts = (sim.cap.div_ceil(256), 1);
        let Some(atomics) = &sim.atomics else { return };
        // The sums start from nothing each step (the counter before them stays).
        let from = sim.off.a[0] as u64 * 4;
        enc.clear_buffer(atomics, from, Some(sim.off.a_len - from));
        self.run(enc, &self.liquid, "solids", &sim.group, layer, off, cells);
        if let Some(to) = fill {
            let rows = (sim.h as f32 - to.ceil().clamp(0.0, sim.h as f32)) as u32;
            self.run(enc, &self.liquid, "fill", &sim.group, layer, off, (sim.w.div_ceil(8), rows.div_ceil(8)));
        }
        if emit > 0 {
            self.run(enc, &self.liquid, "emit", &sim.group, layer, off, (emit.div_ceil(64), 1));
            self.run(enc, &self.liquid, "advance", &sim.group, layer, off, (1, 1));
        }
        self.run(enc, &self.liquid, "p2g", &sim.group, layer, off, parts);
        self.run(enc, &self.liquid, "grid", &sim.group, layer, off, faces);
        self.run(enc, &self.liquid, "forces", &sim.group, layer, off, faces);
        self.run(enc, &self.liquid, "divergence", &sim.group, layer, off, cells);
        for _ in 0..ITERS {
            self.run(enc, &self.liquid, "red", &sim.group, layer, off, cells);
            self.run(enc, &self.liquid, "black", &sim.group, layer, off, cells);
        }
        self.run(enc, &self.liquid, "project", &sim.group, layer, off, faces);
        self.run(enc, &self.liquid, "g2p", &sim.group, layer, off, parts);
    }
}
