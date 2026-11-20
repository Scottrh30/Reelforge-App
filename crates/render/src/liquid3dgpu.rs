//! The 3D liquid simulated on the graphics card (see `liquid3d.wgsl` and
//! `reelforge_core::water::liquid3d_step`): particles in a box behind the
//! layer, stepped on the card; each step's settings (where it pours from,
//! how hard…) come from the core crate, and the picture (thickness, foam and
//! the surface's normals, seen through the camera) goes to the effect pass as
//! a texture for `fs_liquid3d` to light.
//!
//! A simulation is kept per effect (by what it depends on), with a checkpoint
//! every second of simulated time in memory (not on disk: a box of particles
//! is tens of megabytes a second), so playing forwards takes a step or two a
//! frame and seeking back starts from the nearest checkpoint. Long jumps are
//! caught up and filled in as for the other simulations
//! (`reelforge_export::scene`).

use reelforge_core::effects::liquid3d as l3;
use reelforge_core::fluid::FluidJob;
use reelforge_core::water::{self, Liquid3dStep};
use reelforge_core::Time;
use std::collections::HashMap;

const ENTRIES: [&str; 18] = ["solids", "fill", "emit", "advance", "clear_sums", "p2g", "grid", "forces", "divergence", "red", "black", "project", "g2p", "splat", "depth_in", "smooth_ab", "smooth_ba", "picture"];
/// Pressure-solve sweeps a step (red and black each).
const ITERS: usize = 30;
/// Depth-smoothing passes for the picture (pairs: the result ends where it began).
const SMOOTHING: usize = 4;
/// Steps encoded per submission (and uniform slots).
const CHUNK: usize = 16;
/// Simulations and checkpoints kept.
const KEEP: usize = 2;
const CHECKPOINTS: usize = 12;
/// Bytes per uniform slot (the usual dynamic-offset alignment; the step's settings fit in it).
const SLOT: u64 = 256;
/// The camera's field of view down the picture (degrees).
const FOV: f32 = 40.0;
/// Particles nearer the camera than this (cells) aren't drawn, and are gone.
const NEAR: f32 = 0.4;
const DEAD: f32 = -1.0e9;

/// Where each grid sits in the float (`g`) and atomic (`a`) buffers, in words.
#[derive(Clone, Copy)]
struct Offsets {
    og: [[u32; 4]; 3],
    oa: [[u32; 4]; 3],
    g_len: u64,
    a_len: u64,
    /// Where the pressure is, and how many cells.
    pressure: u32,
    cells: u32,
}

impl Offsets {
    fn new(w: u32, h: u32, d: u32, rw: u32, rh: u32) -> Self {
        let nu = (w + 1) * h * d;
        let nv = w * (h + 1) * d;
        let nw = w * h * (d + 1);
        let nc = w * h * d;
        let np = rw * rh;
        let mut at = 0u32;
        let mut take = |n: u32| {
            let o = at;
            at += n;
            o
        };
        // u, v, w; u, v, w before forces; pressure, divergence; solid, kind; smoothed depth ×2
        let (u, v, ww, u0, v0, w0) = (take(nu), take(nv), take(nw), take(nu), take(nv), take(nw));
        let (p, dv, s, k) = (take(nc), take(nc), take(nc), take(nc));
        let (za, zb) = (take(np), take(np));
        let g_len = at as u64 * 4;
        // (the ring's head first)
        let mut at = 4u32;
        let mut take = |n: u32| {
            let o = at;
            at += n;
            o
        };
        let (us, uw, vs, vw, ws, www) = (take(nu), take(nu), take(nv), take(nv), take(nw), take(nw));
        let (count, depth, thick, foam) = (take(nc), take(np), take(np), take(np));
        let a_len = at as u64 * 4;
        Offsets {
            og: [[u, v, ww, u0], [v0, w0, p, dv], [s, k, za, zb]],
            oa: [[us, uw, vs, vw], [ws, www, count, depth], [thick, foam, 0, 0]],
            g_len,
            a_len,
            pressure: p,
            cells: nc,
        }
    }
}

struct Sim {
    w: u32,
    h: u32,
    d: u32,
    rw: u32,
    rh: u32,
    cap: u32,
    step: usize,
    parts: wgpu::Buffer,
    a: wgpu::Buffer,
    g: wgpu::Buffer,
    out: wgpu::Texture,
    out_view: wgpu::TextureView,
    off: Offsets,
    /// (step, particles, pressure, the ring's head)
    checkpoints: Vec<(usize, wgpu::Buffer, wgpu::Buffer, wgpu::Buffer)>,
    /// Past this step its state is approximate (see `WaterGpu::caught_up`).
    approx_after: Option<usize>,
    /// A copy being filled in (see `WaterGpu::fork`).
    forked_at: Option<usize>,
    used: u64,
}

pub(crate) struct Liquid3dGpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    layout: wgpu::BindGroupLayout,
    layer_layout: wgpu::BindGroupLayout,
    pipelines: HashMap<&'static str, wgpu::ComputePipeline>,
    uniforms: wgpu::Buffer,
    sampler: wgpu::Sampler,
    blank: wgpu::TextureView,
    sims: HashMap<u64, Sim>,
    tick: u64,
}

/// The camera for a box `w`×`h`×`d` cells drawn `rh` pixels down, looking down
/// by `view` degrees: (position, focal length in picture pixels), (cos, sin of
/// looking down, the layer's plane, the nearest drawn).
fn camera(w: u32, h: u32, d: u32, rh: u32, view: f32) -> ([f32; 4], [f32; 4]) {
    let half = (FOV / 2.0).to_radians().tan();
    let dist = h as f32 / 2.0 / half;
    let (s, c) = view.clamp(-30.0, 60.0).to_radians().sin_cos();
    let plane = d as f32 * water::PLANE;
    let centre = [w as f32 / 2.0, h as f32 / 2.0, plane];
    // (back from the middle of the layer along where it looks: forward (0, s, c))
    let pos = [centre[0], centre[1] - s * dist, centre[2] - c * dist];
    let focal = rh as f32 / 2.0 / half;
    ([pos[0], pos[1], pos[2], focal], [c, s, plane, NEAR])
}

fn step_bytes(sim: &Sim, st: &Liquid3dStep, emit: u32, cam: ([f32; 4], [f32; 4])) -> Vec<u8> {
    let mut v = Vec::with_capacity(SLOT as usize);
    let u = |v: &mut Vec<u8>, xs: &[u32]| xs.iter().for_each(|x| v.extend(x.to_le_bytes()));
    let f = |v: &mut Vec<u8>, xs: &[f32]| xs.iter().for_each(|x| v.extend(x.to_le_bytes()));
    u(&mut v, &[sim.w, sim.h, sim.d, sim.cap, emit, sim.rw, sim.rh, st.step, st.seed, u32::from(st.floor), if st.walls { 3 } else { 0 }, st.solids.kind]);
    f(&mut v, &[st.src[0], st.src[1], st.src[2], st.width]);
    f(&mut v, &[st.dir[0], st.dir[1], st.dir[2], st.speed]);
    f(&mut v, &[st.gravity, st.flip, st.bounce, st.damp]);
    f(&mut v, &[st.solids.tolerance, st.fill_to, st.droplets, st.slab_half]);
    f(&mut v, &cam.0);
    f(&mut v, &cam.1);
    for row in sim.off.og.iter().chain(&sim.off.oa) {
        u(&mut v, row);
    }
    v
}

impl Liquid3dGpu {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        use wgpu::{BindGroupLayoutEntry as E, BindingType as B, ShaderStages as S};
        let storage = |binding| E { binding, visibility: S::COMPUTE, ty: B::Buffer { ty: wgpu::BufferBindingType::Storage { read_only: false }, has_dynamic_offset: false, min_binding_size: None }, count: None };
        let entries = [
            E { binding: 0, visibility: S::COMPUTE, ty: B::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: true, min_binding_size: wgpu::BufferSize::new(240) }, count: None },
            storage(1),
            storage(2),
            storage(3),
            E { binding: 4, visibility: S::COMPUTE, ty: B::StorageTexture { access: wgpu::StorageTextureAccess::WriteOnly, format: wgpu::TextureFormat::Rgba16Float, view_dimension: wgpu::TextureViewDimension::D2 }, count: None },
        ];
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("liquid 3d"), entries: &entries });
        let layer_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("liquid 3d layer"),
            entries: &[
                E { binding: 0, visibility: S::COMPUTE, ty: B::Texture { sample_type: wgpu::TextureSampleType::Float { filterable: true }, view_dimension: wgpu::TextureViewDimension::D2, multisampled: false }, count: None },
                E { binding: 1, visibility: S::COMPUTE, ty: B::Sampler(wgpu::SamplerBindingType::Filtering), count: None },
            ],
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("liquid 3d"), bind_group_layouts: &[Some(&layout), Some(&layer_layout)], immediate_size: 0 });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("liquid 3d"), source: wgpu::ShaderSource::Wgsl(include_str!("liquid3d.wgsl").into()) });
        let pipelines = ENTRIES
            .iter()
            .map(|e| (*e, device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor { label: Some(e), layout: Some(&pl), module: &module, entry_point: Some(e), compilation_options: Default::default(), cache: None })))
            .collect();
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor { label: Some("liquid 3d steps"), size: SLOT * (CHUNK as u64 + 1), usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor { label: Some("liquid 3d"), mag_filter: wgpu::FilterMode::Linear, min_filter: wgpu::FilterMode::Linear, ..Default::default() });
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
        Self { device: device.clone(), queue: queue.clone(), layout, layer_layout, pipelines, uniforms, sampler, blank, sims: HashMap::new(), tick: 0 }
    }

    fn buffer(&self, label: &str, bytes: u64) -> wgpu::Buffer {
        self.device.create_buffer(&wgpu::BufferDescriptor { label: Some(label), size: bytes.max(16), usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false })
    }

    /// The picture's size for a `w`×`h` grid on a `size` layer: half the
    /// layer's pixels, at most eight a cell.
    fn picture_size(w: u32, h: u32, size: [f32; 2]) -> (u32, u32) {
        let rw = ((size[0] * 0.5) as u32).clamp(64, w * 8).min(2048);
        let rh = ((rw as f32 * h as f32 / w.max(1) as f32).round() as u32).clamp(16, 2048);
        (rw, rh)
    }

    /// A new simulation, empty, or `None` if the card can't hold it.
    fn make(&self, w: u32, h: u32, d: u32, size: [f32; 2]) -> Option<Sim> {
        let (rw, rh) = Self::picture_size(w, h, size);
        let off = Offsets::new(w, h, d, rw, rh);
        let cap = water::capacity3d(w as usize, h as usize, d as usize) as u32;
        let limit = self.device.limits().max_storage_buffer_binding_size;
        let parts_len = cap as u64 * 32;
        if off.g_len > limit || off.a_len > limit || parts_len > limit {
            log::warn!("the 3D liquid's grid is too big for this graphics card; try a lower Detail");
            return None;
        }
        let parts = self.buffer("liquid 3d particles", parts_len);
        let a = self.buffer("liquid 3d sums", off.a_len);
        let g = self.buffer("liquid 3d grids", off.g_len);
        let out = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("liquid 3d picture"),
            size: wgpu::Extent3d { width: rw, height: rh, depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let out_view = out.create_view(&Default::default());
        let mut sim = Sim { w, h, d, rw, rh, cap, step: 0, parts, a, g, out, out_view, off, checkpoints: Vec::new(), approx_after: None, forked_at: None, used: 0 };
        self.reset(&mut sim);
        Some(sim)
    }

    /// Back to the start: no particles, still grids.
    fn reset(&self, sim: &mut Sim) {
        let dead: Vec<u8> = (0..sim.cap as usize * 2).flat_map(|k| if k % 2 == 0 { [DEAD, DEAD, DEAD, 0.0] } else { [0.0; 4] }).flat_map(f32::to_le_bytes).collect();
        self.queue.write_buffer(&sim.parts, 0, &dead);
        let mut enc = self.device.create_command_encoder(&Default::default());
        enc.clear_buffer(&sim.a, 0, None);
        enc.clear_buffer(&sim.g, 0, None);
        self.queue.submit([enc.finish()]);
        sim.step = 0;
    }

    fn group(&self, sim: &Sim) -> wgpu::BindGroup {
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("liquid 3d"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding { buffer: &self.uniforms, offset: 0, size: wgpu::BufferSize::new(240) }) },
                wgpu::BindGroupEntry { binding: 1, resource: sim.parts.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 2, resource: sim.a.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 3, resource: sim.g.as_entire_binding() },
                wgpu::BindGroupEntry { binding: 4, resource: wgpu::BindingResource::TextureView(&sim.out_view) },
            ],
        })
    }

    fn run(&self, enc: &mut wgpu::CommandEncoder, entry: &str, group: &wgpu::BindGroup, layer: &wgpu::BindGroup, off: u32, groups: (u32, u32, u32)) {
        let Some(p) = self.pipelines.get(entry) else { return };
        let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some(entry), timestamp_writes: None });
        pass.set_pipeline(p);
        pass.set_bind_group(0, group, &[off]);
        pass.set_bind_group(1, layer, &[]);
        pass.dispatch_workgroups(groups.0.max(1), groups.1.max(1), groups.2.max(1));
    }

    /// The step `job` would start from: where its simulation is if that's not
    /// past it, else the nearest checkpoint before it, else the start.
    pub fn resume_step(&self, job: &FluidJob) -> usize {
        match self.sims.get(&job.key) {
            Some(s) if s.step <= job.step => s.step,
            Some(s) => s.checkpoints.iter().map(|c| c.0).filter(|&c| c <= job.step).max().unwrap_or(0),
            None => 0,
        }
    }

    /// See `WaterGpu::caught_up`.
    pub fn caught_up(&mut self, job: &FluidJob, from: usize, exact: bool) {
        let Some(sim) = self.sims.get_mut(&job.key) else { return };
        sim.approx_after = match (sim.approx_after, exact) {
            (Some(a), true) if from <= a => None,
            (a, true) => a,
            (Some(a), false) => Some(a.min(from)),
            (None, false) => Some(from),
        };
    }

    fn copy_state(&self, enc: &mut wgpu::CommandEncoder, from: (&wgpu::Buffer, &wgpu::Buffer, &wgpu::Buffer), to: &Sim) {
        enc.copy_buffer_to_buffer(from.0, 0, &to.parts, 0, to.parts.size());
        enc.copy_buffer_to_buffer(from.1, 0, &to.g, to.off.pressure as u64 * 4, to.off.cells as u64 * 4);
        enc.copy_buffer_to_buffer(from.2, 0, &to.a, 0, 16);
    }

    fn checkpoint(&self, enc: &mut wgpu::CommandEncoder, sim: &Sim) -> (wgpu::Buffer, wgpu::Buffer, wgpu::Buffer) {
        let parts = self.buffer("liquid 3d checkpoint", sim.parts.size());
        let p = self.buffer("liquid 3d checkpoint pressure", sim.off.cells as u64 * 4);
        let head = self.buffer("liquid 3d checkpoint head", 16);
        enc.copy_buffer_to_buffer(&sim.parts, 0, &parts, 0, sim.parts.size());
        enc.copy_buffer_to_buffer(&sim.g, sim.off.pressure as u64 * 4, &p, 0, sim.off.cells as u64 * 4);
        enc.copy_buffer_to_buffer(&sim.a, 0, &head, 0, 16);
        (parts, p, head)
    }

    /// See `WaterGpu::fork`.
    pub fn fork(&mut self, job: &FluidJob, shadow: u64) -> Option<usize> {
        let real = self.sims.get(&job.key)?;
        let a = real.approx_after?;
        let mut copy = self.make(real.w, real.h, real.d, job.size)?;
        if let Some((step, p, pr, hd)) = real.checkpoints.iter().filter(|c| c.0 <= a).max_by_key(|c| c.0) {
            let mut enc = self.device.create_command_encoder(&Default::default());
            self.copy_state(&mut enc, (p, pr, hd), &copy);
            self.queue.submit([enc.finish()]);
            copy.step = *step;
        }
        let at = copy.step;
        copy.forked_at = Some(at);
        copy.used = self.tick;
        self.sims.insert(shadow, copy);
        Some(at)
    }

    /// See `WaterGpu::adopt`.
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
        self.sims.insert(key, copy);
    }

    /// See `WaterGpu::discard`.
    pub fn discard(&mut self, shadow: u64) {
        self.sims.remove(&shadow);
    }

    /// Simulate `job` up to its step and return its picture (thickness, foam,
    /// the surface's normal across and down). `layer` is the layer's picture
    /// now, for what's solid.
    pub fn step_to(&mut self, job: &FluidJob, layer: Option<&wgpu::TextureView>) -> Option<wgpu::Texture> {
        let (w, h, d) = (job.w as u32, job.h as u32, job.d as u32);
        if w < 4 || h < 4 || d < 4 || job.effect.kind != reelforge_core::EffectKind::Liquid3D {
            return None;
        }
        self.tick += 1;
        let fresh = !matches!(self.sims.get(&job.key), Some(s) if s.w == w && s.h == h && s.d == d && (s.rw, s.rh) == Self::picture_size(w, h, job.size));
        if fresh {
            // (the copies being filled in don't count against the rest)
            while self.sims.iter().filter(|(_, s)| s.forked_at.is_none()).count() >= KEEP {
                let Some(old) = self.sims.iter().filter(|(_, s)| s.forked_at.is_none()).min_by_key(|(_, s)| s.used).map(|(k, _)| *k) else { break };
                self.sims.remove(&old);
            }
            let sim = self.make(w, h, d, job.size)?;
            self.sims.insert(job.key, sim);
        }
        let mut sim = self.sims.remove(&job.key)?;
        sim.used = self.tick;
        let target = job.step;
        if target < sim.step {
            let best = sim.checkpoints.iter().filter(|c| c.0 <= target).max_by_key(|c| c.0);
            match best {
                Some((s, p, pr, hd)) => {
                    let mut enc = self.device.create_command_encoder(&Default::default());
                    self.copy_state(&mut enc, (p, pr, hd), &sim);
                    self.queue.submit([enc.finish()]);
                    sim.step = *s;
                }
                None => self.reset(&mut sim),
            }
            if sim.approx_after.is_some_and(|a| sim.step <= a) {
                let at = sim.step;
                sim.checkpoints.retain(|c| c.0 <= at);
                sim.approx_after = None;
            }
        }
        let group = self.group(&sim);
        let layer_group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("liquid 3d layer"),
            layout: &self.layer_layout,
            entries: &[wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(layer.unwrap_or(&self.blank)) }, wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.sampler) }],
        });
        let every = water::LIQUID3D_RATE.round() as usize;
        let view_at = |step: usize| job.effect.value_at(l3::VIEW, Time::from_secs_f64(step as f64 / water::LIQUID3D_RATE));
        let cells = (w.div_ceil(4), h.div_ceil(4), d.div_ceil(4));
        let faces = ((w + 1).div_ceil(4), (h + 1).div_ceil(4), (d + 1).div_ceil(4));
        let parts = (sim.cap.div_ceil(256), 1, 1);
        while sim.step < target {
            let todo = (target - sim.step).min(CHUNK);
            let mut steps = Vec::with_capacity(todo);
            for k in 0..todo {
                let st = water::liquid3d_step(&job.effect, sim.step + k, job.w, job.h, job.d, job.size);
                let emit = st.emit.min(sim.cap);
                let cam = camera(w, h, d, sim.rh, view_at(sim.step + k));
                self.queue.write_buffer(&self.uniforms, k as u64 * SLOT, &step_bytes(&sim, &st, emit, cam));
                steps.push((emit, sim.step + k == 0 && st.fill_to < h as f32 - 0.5, st.fill_to));
            }
            let mut enc = self.device.create_command_encoder(&Default::default());
            let mut fresh_checkpoints = Vec::new();
            for (k, &(emit, fill, fill_to)) in steps.iter().enumerate() {
                let off = (k as u64 * SLOT) as u32;
                // The sums start from nothing each step (the ring's head before them stays).
                enc.clear_buffer(&sim.a, 16, Some(sim.off.a_len - 16));
                self.run(&mut enc, "solids", &group, &layer_group, off, cells);
                if fill {
                    let rows = (h as f32 - fill_to.ceil().clamp(0.0, h as f32)) as u32;
                    self.run(&mut enc, "fill", &group, &layer_group, off, (w.div_ceil(4), rows.div_ceil(4), d.div_ceil(4)));
                }
                if emit > 0 {
                    self.run(&mut enc, "emit", &group, &layer_group, off, (emit.div_ceil(64), 1, 1));
                    self.run(&mut enc, "advance", &group, &layer_group, off, (1, 1, 1));
                }
                self.run(&mut enc, "p2g", &group, &layer_group, off, parts);
                self.run(&mut enc, "grid", &group, &layer_group, off, faces);
                self.run(&mut enc, "forces", &group, &layer_group, off, faces);
                self.run(&mut enc, "divergence", &group, &layer_group, off, cells);
                for _ in 0..ITERS {
                    self.run(&mut enc, "red", &group, &layer_group, off, cells);
                    self.run(&mut enc, "black", &group, &layer_group, off, cells);
                }
                self.run(&mut enc, "project", &group, &layer_group, off, faces);
                self.run(&mut enc, "g2p", &group, &layer_group, off, parts);
                let now = sim.step + k + 1;
                if now % every == 0 && !sim.checkpoints.iter().any(|c| c.0 == now) {
                    let (p, pr, hd) = self.checkpoint(&mut enc, &sim);
                    sim.checkpoints.push((now, p, pr, hd));
                    fresh_checkpoints.push(now);
                    if sim.checkpoints.len() > CHECKPOINTS {
                        // Keep the first and thin out the rest.
                        sim.checkpoints.remove(1);
                    }
                }
            }
            self.queue.submit([enc.finish()]);
            sim.step += todo;
        }
        // The picture, through the camera as it is now.
        let st = water::liquid3d_step(&job.effect, target, job.w, job.h, job.d, job.size);
        let slot = CHUNK as u64;
        self.queue.write_buffer(&self.uniforms, slot * SLOT, &step_bytes(&sim, &st, 0, camera(w, h, d, sim.rh, view_at(target))));
        let off = (slot * SLOT) as u32;
        let pix = (sim.rw.div_ceil(8), sim.rh.div_ceil(8), 1);
        let mut enc = self.device.create_command_encoder(&Default::default());
        // (the picture's sums from nothing; the particle counts a cell, for
        // telling spray, from the last step)
        let thick = sim.off.oa[2][0] as u64 * 4;
        enc.clear_buffer(&sim.a, thick, Some(sim.off.a_len - thick));
        self.run(&mut enc, "clear_sums", &group, &layer_group, off, ((sim.rw * sim.rh).div_ceil(256), 1, 1));
        self.run(&mut enc, "splat", &group, &layer_group, off, parts);
        self.run(&mut enc, "depth_in", &group, &layer_group, off, pix);
        for _ in 0..SMOOTHING {
            self.run(&mut enc, "smooth_ab", &group, &layer_group, off, pix);
            self.run(&mut enc, "smooth_ba", &group, &layer_group, off, pix);
        }
        self.run(&mut enc, "picture", &group, &layer_group, off, pix);
        self.queue.submit([enc.finish()]);
        let out = sim.out.clone();
        self.sims.insert(job.key, sim);
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The grids don't overlap, and fit the buffers made for them.
    #[test]
    fn grids_are_laid_out_apart() {
        let (w, h, d, rw, rh) = (10u32, 6, 6, 80, 48);
        let o = Offsets::new(w, h, d, rw, rh);
        let nu = (w + 1) * h * d;
        let nv = w * (h + 1) * d;
        let nw = w * h * (d + 1);
        let nc = w * h * d;
        let np = rw * rh;
        let g = [(o.og[0][0], nu), (o.og[0][1], nv), (o.og[0][2], nw), (o.og[0][3], nu), (o.og[1][0], nv), (o.og[1][1], nw), (o.og[1][2], nc), (o.og[1][3], nc), (o.og[2][0], nc), (o.og[2][1], nc), (o.og[2][2], np), (o.og[2][3], np)];
        let a = [(0, 4), (o.oa[0][0], nu), (o.oa[0][1], nu), (o.oa[0][2], nv), (o.oa[0][3], nv), (o.oa[1][0], nw), (o.oa[1][1], nw), (o.oa[1][2], nc), (o.oa[1][3], np), (o.oa[2][0], np), (o.oa[2][1], np)];
        for (list, len) in [(&g[..], o.g_len), (&a[..], o.a_len)] {
            let mut spans: Vec<(u32, u32)> = list.iter().map(|&(at, n)| (at, at + n)).collect();
            spans.sort();
            assert!(spans.windows(2).all(|p| p[0].1 <= p[1].0), "{spans:?}");
            assert!(spans.last().unwrap().1 as u64 * 4 <= len);
        }
        assert_eq!(o.pressure, o.og[1][2]);
    }

    /// The camera sees the layer's plane fill the picture: the box's middle
    /// corners land on the picture's corners.
    #[test]
    fn the_layer_fills_the_picture() {
        let (w, h, d, rw, rh) = (64u32, 36, 36, 512, 288);
        let (cam, view) = camera(w, h, d, rh, 0.0);
        for corner in [[0.0, 0.0], [w as f32, h as f32], [w as f32, 0.0]] {
            let r = [corner[0] - cam[0], corner[1] - cam[1], view[2] - cam[2]];
            let (c, s) = (view[0], view[1]);
            let p = [r[0], r[1] * c - r[2] * s, r[1] * s + r[2] * c];
            let px = [rw as f32 / 2.0 + p[0] * cam[3] / p[2], rh as f32 / 2.0 + p[1] * cam[3] / p[2]];
            assert!((px[0] - corner[0] / w as f32 * rw as f32).abs() < 0.5 && (px[1] - corner[1] / h as f32 * rh as f32).abs() < 0.5, "{corner:?} → {px:?}");
        }
    }
}
