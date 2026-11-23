//! Optical-flow frame interpolation on the graphics card: the frame at a
//! moment between two frames of footage, each pushed along the motion
//! between them, aware of things passing in front of others (what one frame
//! sees and the other doesn't is taken from the frame that sees it). See
//! `flow.wgsl` for the steps.
//!
//! The motion is worked out at a working size set by the quality (and kept
//! for the frame pair, so slow motion's many in-between frames reuse it);
//! the in-between frame is then drawn at the footage's full size.

use reelforge_core::frame::{FloatPixels, Pixels};
use reelforge_core::FlowQuality;
use std::collections::HashMap;
use wgpu::util::DeviceExt;

/// Motion worked out by dedicated hardware rather than these shaders: NVIDIA's optical
/// flow engine (`reelforge_media::nvof`), handed over by the app at start-up.
pub trait HardwareFlow: Send {
    /// Grey frames `a`, `b` (`w` × `h`, a byte a pixel) → the motion A to B and B to A, a
    /// vector per pixel in pixels (`a[p] ≈ b[p + ab[p]]`). `None`: it couldn't (the
    /// shaders work it out instead).
    fn flow(&mut self, a: &[u8], b: &[u8], w: u32, h: u32, quality: FlowQuality) -> Option<(Vec<[f32; 2]>, Vec<[f32; 2]>)>;
}

static HARDWARE: std::sync::OnceLock<std::sync::Mutex<Box<dyn HardwareFlow>>> = std::sync::OnceLock::new();
static USE_HARDWARE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// Use `h` for optical flow's motion from now on (once; later calls are ignored).
pub fn set_hardware_flow(h: Box<dyn HardwareFlow>) {
    let _ = HARDWARE.set(std::sync::Mutex::new(h));
}

/// Switch the hardware on or off (a setting).
pub fn use_hardware_flow(on: bool) {
    USE_HARDWARE.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// Optical flow's motion comes from the hardware.
pub fn hardware_flow_on() -> bool {
    HARDWARE.get().is_some() && USE_HARDWARE.load(std::sync::atomic::Ordering::Relaxed)
}

/// The working size (longer side) with the hardware: it's fast enough to look at more.
fn hardware_long(q: FlowQuality) -> u32 {
    match q {
        FlowQuality::Fast => 960,
        FlowQuality::Balanced => 1920,
        FlowQuality::Best => 3840,
    }
}

/// A frame as grey bytes at `out` (area-averaged down from `size`), for the hardware.
pub fn grey_at(f: &Pixels, size: [u32; 2], out: [u32; 2]) -> Vec<u8> {
    let rgba = f.to_rgba();
    let [w, h] = size;
    let [ow, oh] = out;
    let mut g = Vec::with_capacity((ow as usize).saturating_mul(oh as usize));
    if w == 0 || h == 0 || ow == 0 || oh == 0 {
        return g;
    }
    for y in 0..oh {
        let y0 = (y as u64 * h as u64 / oh as u64) as u32;
        let y1 = (((y + 1) as u64 * h as u64).div_ceil(oh as u64) as u32).clamp(y0 + 1, h);
        for x in 0..ow {
            let x0 = (x as u64 * w as u64 / ow as u64) as u32;
            let x1 = (((x + 1) as u64 * w as u64).div_ceil(ow as u64) as u32).clamp(x0 + 1, w);
            let (mut sum, mut n) = (0u64, 0u64);
            for yy in y0..y1 {
                for xx in x0..x1 {
                    let i = (yy as usize * w as usize + xx as usize) * 4;
                    if let Some(p) = rgba.get(i..i + 3) {
                        sum += (p[0] as u64 * 54 + p[1] as u64 * 183 + p[2] as u64 * 19) >> 8;
                        n += 1;
                    }
                }
            }
            g.push(sum.checked_div(n).unwrap_or(0).min(255) as u8);
        }
    }
    g
}

/// Settings per quality: working size (longer side), match radius, iterations per level.
fn settings(q: FlowQuality) -> (u32, i32, u32) {
    match q {
        FlowQuality::Fast => (480, 2, 1),
        FlowQuality::Balanced => (960, 3, 2),
        FlowQuality::Best => (2048, 3, 3),
    }
}

const ENTRIES: [&str; 12] = ["grey_tex", "grey_down", "flow_zero", "flow_up", "search", "smooth_flow", "occlusion", "fill", "proj_clear", "project", "resolve", "synth"];

#[derive(Clone, Copy)]
struct Params {
    w: u32,
    h: u32,
    cw: u32,
    ch: u32,
    ow: u32,
    oh: u32,
    radius: i32,
    wide: i32,
    t: f32,
    s: f32,
    flags: u32,
    lambda: f32,
}

impl Params {
    /// As the shader's uniform block lays it out (twelve 4-byte fields).
    fn bytes(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(48);
        for x in [self.w, self.h, self.cw, self.ch, self.ow, self.oh] {
            v.extend(x.to_le_bytes());
        }
        v.extend(self.radius.to_le_bytes());
        v.extend(self.wide.to_le_bytes());
        v.extend(self.t.to_le_bytes());
        v.extend(self.s.to_le_bytes());
        v.extend(self.flags.to_le_bytes());
        v.extend(self.lambda.to_le_bytes());
        v
    }

    fn level(w: u32, h: u32) -> Params {
        Params { w, h, cw: w, ch: h, ow: w, oh: h, radius: 3, wide: 0, t: 0.5, s: 0.0, flags: 0, lambda: 0.0 }
    }
}

/// A frame as a texture: its view, and whether its alpha is straight (8-bit) or premultiplied (float).
struct Input {
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    straight: bool,
    size: [u32; 2],
    float: bool,
}

/// One frame pair's motion, kept on the card.
struct Pair {
    /// What it was worked out for (clip, frame, quality…).
    key: u64,
    out: [u32; 2],
    levels: Vec<[u32; 2]>,
    grey_a: Vec<wgpu::Buffer>,
    grey_b: Vec<wgpu::Buffer>,
    ab: Vec<wgpu::Buffer>,
    ba: Vec<wgpu::Buffer>,
    tmp: wgpu::Buffer,
    proj_a: wgpu::Buffer,
    proj_b: wgpu::Buffer,
    tflow: wgpu::Buffer,
    vis: wgpu::Buffer,
    a: Option<Input>,
    b: Option<Input>,
    target: wgpu::Texture,
    estimated: bool,
    /// The motion came from the hardware.
    hw: bool,
    used: u64,
}

pub struct FlowInterp {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipelines: HashMap<&'static str, wgpu::ComputePipeline>,
    sampler: wgpu::Sampler,
    pairs: HashMap<u64, Pair>,
    tick: u64,
}

/// How many clips' frame pairs are kept at once.
const KEEP: usize = 4;

impl FlowInterp {
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("flow"), source: wgpu::ShaderSource::Wgsl(include_str!("flow.wgsl").into()) });
        let pipelines = ENTRIES
            .iter()
            .map(|e| {
                let p = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                    label: Some(e),
                    layout: None,
                    module: &module,
                    entry_point: Some(e),
                    compilation_options: Default::default(),
                    cache: None,
                });
                (*e, p)
            })
            .collect();
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("flow"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });
        Self { device: device.clone(), queue: queue.clone(), pipelines, sampler, pairs: HashMap::new(), tick: 0 }
    }

    /// Forget every kept frame pair.
    pub fn clear(&mut self) {
        self.pairs.clear();
    }

    /// The frame `t` (0–1) of the way from `a` to `b` (the same size), as
    /// linear premultiplied half floats. `slot` names the clip (one pair
    /// kept per clip), `key` the frame pair and settings: the motion is
    /// worked out again only when it changes.
    pub fn interpolate(&mut self, slot: u64, key: u64, a: &Pixels, b: &Pixels, size: [u32; 2], t: f32, quality: FlowQuality) -> Option<FloatPixels> {
        let [w, h] = size;
        if w < 8 || h < 8 {
            return None;
        }
        self.tick += 1;
        let tick = self.tick;
        if !self.pairs.contains_key(&slot) && self.pairs.len() >= KEEP {
            if let Some(old) = self.pairs.iter().min_by_key(|(_, p)| p.used).map(|(k, _)| *k) {
                self.pairs.remove(&old);
            }
        }
        let (soft_long, radius, iters) = settings(quality);
        let hw = hardware_flow_on();
        let long = if hw { hardware_long(quality) } else { soft_long };
        let k = (w.max(h) as f32 / long as f32).max(1.0);
        let lw = ((w as f32 / k).round() as u32).max(8);
        let lh = ((h as f32 / k).round() as u32).max(8);
        let fresh = !matches!(self.pairs.get(&slot), Some(p) if p.out == [w, h] && p.levels[0] == [lw, lh]);
        if fresh {
            let pair = self.make_pair([w, h], [lw, lh]);
            self.pairs.insert(slot, pair);
        }
        let mut pair = self.pairs.remove(&slot)?;
        pair.used = tick;
        if pair.key != key || !pair.estimated || pair.hw != hw {
            pair.a = Some(self.upload(pair.a.take(), a, size));
            pair.b = Some(self.upload(pair.b.take(), b, size));
            let by_hardware = hw && self.estimate_hw(&pair, a, b, size, quality);
            if !by_hardware {
                // (The working size may be the hardware's: the shaders manage any size.)
                self.estimate(&pair, radius, iters);
            }
            pair.key = key;
            pair.estimated = true;
            pair.hw = hw;
        }
        let result = self.synthesise(&pair, t);
        self.pairs.insert(slot, pair);
        result
    }

    /// The motion worked out for `slot` at the working size, A to B and B to
    /// A (x, y, match cost, how hidden), for checking it.
    pub fn motion(&self, slot: u64, level: usize) -> Option<([u32; 2], Vec<[f32; 4]>, Vec<[f32; 4]>)> {
        let p = self.pairs.get(&slot)?;
        let level = level.min(p.levels.len() - 1);
        let l0 = p.levels[level];
        let read = |b: &wgpu::Buffer| -> Option<Vec<[f32; 4]>> {
            let n = l0[0] as u64 * l0[1] as u64 * 16;
            let staging = self.device.create_buffer(&wgpu::BufferDescriptor { label: Some("flow read"), size: n, usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ, mapped_at_creation: false });
            let mut enc = self.device.create_command_encoder(&Default::default());
            enc.copy_buffer_to_buffer(b, 0, &staging, 0, n);
            self.queue.submit([enc.finish()]);
            let slice = staging.slice(..);
            let (tx, rx) = std::sync::mpsc::channel();
            slice.map_async(wgpu::MapMode::Read, move |r| {
                let _ = tx.send(r);
            });
            self.device.poll(wgpu::PollType::wait_indefinitely()).ok()?;
            rx.recv().ok()?.ok()?;
            let data = slice.get_mapped_range().ok()?;
            let v = data.chunks_exact(16).map(|c| [0, 1, 2, 3].map(|k| f32::from_le_bytes([c[k * 4], c[k * 4 + 1], c[k * 4 + 2], c[k * 4 + 3]]))).collect();
            drop(data);
            staging.unmap();
            Some(v)
        };
        Some((l0, read(&p.ab[level])?, read(&p.ba[level])?))
    }

    fn buffer(&self, label: &str, bytes: u64) -> wgpu::Buffer {
        self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: bytes.max(16),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    fn make_pair(&self, out: [u32; 2], l0: [u32; 2]) -> Pair {
        let mut levels = vec![l0];
        while levels.len() < 7 {
            let Some(&[w, h]) = levels.last() else { break };
            if w.min(h) < 24 {
                break;
            }
            levels.push([w.div_ceil(2), h.div_ceil(2)]);
        }
        let px = |l: &[u32; 2]| l[0] as u64 * l[1] as u64;
        let n0 = px(&l0);
        let target = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("flow out"),
            size: wgpu::Extent3d { width: out[0], height: out[1], depth_or_array_layers: 1 },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::STORAGE_BINDING | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        Pair {
            key: 0,
            out,
            grey_a: levels.iter().map(|l| self.buffer("grey a", px(l) * 4)).collect(),
            grey_b: levels.iter().map(|l| self.buffer("grey b", px(l) * 4)).collect(),
            ab: levels.iter().map(|l| self.buffer("flow ab", px(l) * 16)).collect(),
            ba: levels.iter().map(|l| self.buffer("flow ba", px(l) * 16)).collect(),
            tmp: self.buffer("flow tmp", n0 * 16),
            proj_a: self.buffer("proj a", n0 * 4),
            proj_b: self.buffer("proj b", n0 * 4),
            tflow: self.buffer("tflow", n0 * 16),
            vis: self.buffer("vis", n0 * 16),
            levels,
            a: None,
            b: None,
            target,
            estimated: false,
            hw: false,
            used: 0,
        }
    }

    /// A frame onto the card: 8-bit as sRGB (read back as linear light, alpha
    /// straight), anything deeper as linear half floats, premultiplied.
    fn upload(&self, reuse: Option<Input>, f: &Pixels, size: [u32; 2]) -> Input {
        let float_px: Option<std::sync::Arc<FloatPixels>> = match f {
            Pixels::Rgba(_) => None,
            Pixels::Float(p) => Some(p.clone()),
            Pixels::Yuv(y) => Some(std::sync::Arc::new(y.to_float())),
            Pixels::Gpu(g) => g.cpu().map(|y| std::sync::Arc::new(y.to_float())),
        };
        let float = float_px.is_some();
        let input = match reuse {
            Some(i) if i.size == size && i.float == float => i,
            _ => {
                let texture = self.device.create_texture(&wgpu::TextureDescriptor {
                    label: Some("flow frame"),
                    size: wgpu::Extent3d { width: size[0], height: size[1], depth_or_array_layers: 1 },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: if float { wgpu::TextureFormat::Rgba16Float } else { wgpu::TextureFormat::Rgba8UnormSrgb },
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                });
                let view = texture.create_view(&Default::default());
                Input { texture, view, straight: !float, size, float }
            }
        };
        let (bytes, bpp): (std::borrow::Cow<[u8]>, u32) = match &float_px {
            Some(p) => (std::borrow::Cow::Owned(p.rgba.iter().flat_map(|v| v.to_le_bytes()).collect()), 8),
            None => (std::borrow::Cow::Owned(f.to_rgba().to_vec()), 4),
        };
        self.queue.write_texture(
            wgpu::TexelCopyTextureInfo { texture: &input.texture, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            &bytes,
            wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(size[0] * bpp), rows_per_image: Some(size[1]) },
            wgpu::Extent3d { width: size[0], height: size[1], depth_or_array_layers: 1 },
        );
        input
    }

    /// One compute dispatch of `entry` over `size`, with these bindings.
    fn dispatch(&self, enc: &mut wgpu::CommandEncoder, entry: &str, prm: Params, size: [u32; 2], bind: &[(u32, wgpu::BindingResource)]) {
        let pipeline = &self.pipelines[entry];
        let ubuf = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some("flow params"), contents: &prm.bytes(), usage: wgpu::BufferUsages::UNIFORM });
        let mut entries = vec![wgpu::BindGroupEntry { binding: 0, resource: ubuf.as_entire_binding() }];
        entries.extend(bind.iter().map(|(b, r)| wgpu::BindGroupEntry { binding: *b, resource: r.clone() }));
        let group = self.device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some(entry), layout: &pipeline.get_bind_group_layout(0), entries: &entries });
        let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some(entry), timestamp_writes: None });
        pass.set_pipeline(pipeline);
        pass.set_bind_group(0, &group, &[]);
        pass.dispatch_workgroups(size[0].div_ceil(8), size[1].div_ceil(8), 1);
    }

    fn estimate(&self, p: &Pair, radius: i32, iters: u32) {
        let (Some(ia), Some(ib)) = (&p.a, &p.b) else { return };
        let mut enc = self.device.create_command_encoder(&Default::default());
        let samp = wgpu::BindingResource::Sampler(&self.sampler);
        let l0 = p.levels[0];
        // The grey pyramids.
        for (tex, grey) in [(&ia.view, &p.grey_a), (&ib.view, &p.grey_b)] {
            self.dispatch(&mut enc, "grey_tex", Params::level(l0[0], l0[1]), l0, &[(1, wgpu::BindingResource::TextureView(tex)), (3, samp.clone()), (6, grey[0].as_entire_binding())]);
            for l in 1..p.levels.len() {
                let [w, h] = p.levels[l];
                let [cw, ch] = p.levels[l - 1];
                self.dispatch(&mut enc, "grey_down", Params { cw, ch, ..Params::level(w, h) }, [w, h], &[(4, grey[l - 1].as_entire_binding()), (6, grey[l].as_entire_binding())]);
            }
        }
        // Motion both ways, coarse to fine.
        let top = p.levels.len() - 1;
        for l in (0..p.levels.len()).rev() {
            let [w, h] = p.levels[l];
            let n = w as u64 * h as u64 * 16;
            for (from, to, flow) in [(&p.grey_a, &p.grey_b, &p.ab), (&p.grey_b, &p.grey_a, &p.ba)] {
                if l == top {
                    self.dispatch(&mut enc, "flow_zero", Params::level(w, h), [w, h], &[(8, flow[l].as_entire_binding())]);
                } else {
                    let [cw, ch] = p.levels[l + 1];
                    self.dispatch(&mut enc, "flow_up", Params { cw, ch, ..Params::level(w, h) }, [w, h], &[(9, flow[l + 1].as_entire_binding()), (8, flow[l].as_entire_binding())]);
                }
                for it in 0..iters {
                    // Coarse levels match small patches (a big one there takes in
                    // whatever else is moving nearby) and search widely; finer
                    // ones look a little round what came down.
                    let coarse = l + 2 >= p.levels.len();
                    let wide = if l == top && it == 0 { 6 } else if it == 0 && w.max(h) <= 600 { 2 } else { 0 };
                    // Large working sizes match sparse patches.
                    let flags = if w.max(h) > 600 { 4 } else { 0 };
                    let prm = Params { radius: if coarse { 1 } else { radius }, wide, flags, lambda: if l == top { 0.0 } else { 0.3 }, ..Params::level(w, h) };
                    self.dispatch(&mut enc, "search", prm, [w, h], &[(4, from[l].as_entire_binding()), (5, to[l].as_entire_binding()), (7, flow[l].as_entire_binding()), (8, p.tmp.as_entire_binding())]);
                    self.dispatch(&mut enc, "smooth_flow", prm, [w, h], &[(4, from[l].as_entire_binding()), (7, wgpu::BindingResource::Buffer(wgpu::BufferBinding { buffer: &p.tmp, offset: 0, size: std::num::NonZeroU64::new(n) })), (8, flow[l].as_entire_binding())]);
                }
            }
        }
        self.finish(&mut enc, p);
        self.queue.submit([enc.finish()]);
    }

    /// The motion from the hardware: grey frames to it at the working size, its vectors
    /// into the finest level, then the same visibility check and filling as the shaders'
    /// own. `false`: it couldn't (nothing changed).
    fn estimate_hw(&self, p: &Pair, a: &Pixels, b: &Pixels, size: [u32; 2], quality: FlowQuality) -> bool {
        let Some(ia) = &p.a else { return false };
        let Some(ib) = &p.b else { return false };
        let Some(&l0) = p.levels.first() else { return false };
        let (ga, gb) = (grey_at(a, size, l0), grey_at(b, size, l0));
        let Some(hw) = HARDWARE.get() else { return false };
        let got = hw.lock().unwrap_or_else(std::sync::PoisonError::into_inner).flow(&ga, &gb, l0[0], l0[1], quality);
        let Some((ab, ba)) = got else { return false };
        let n = l0[0] as usize * l0[1] as usize;
        if ab.len() != n || ba.len() != n {
            return false;
        }
        let (Some(fab), Some(fba), Some(gra), Some(grb)) = (p.ab.first(), p.ba.first(), p.grey_a.first(), p.grey_b.first()) else { return false };
        let bytes = |v: &[[f32; 2]]| -> Vec<u8> { v.iter().flat_map(|f| [f[0], f[1], 0.0, 0.0]).flat_map(|x| x.to_le_bytes()).collect() };
        self.queue.write_buffer(fab, 0, &bytes(&ab));
        self.queue.write_buffer(fba, 0, &bytes(&ba));
        let mut enc = self.device.create_command_encoder(&Default::default());
        let samp = wgpu::BindingResource::Sampler(&self.sampler);
        for (tex, grey) in [(&ia.view, gra), (&ib.view, grb)] {
            self.dispatch(&mut enc, "grey_tex", Params::level(l0[0], l0[1]), l0, &[(1, wgpu::BindingResource::TextureView(tex)), (3, samp.clone()), (6, grey.as_entire_binding())]);
        }
        self.finish(&mut enc, p);
        self.queue.submit([enc.finish()]);
        true
    }

    /// After the motion: what each frame sees that the other doesn't, and motion for
    /// what's hidden.
    fn finish(&self, enc: &mut wgpu::CommandEncoder, p: &Pair) {
        let l0 = p.levels[0];
        // What each frame sees that the other doesn't.
        let n0 = l0[0] as u64 * l0[1] as u64 * 16;
        for (this, other) in [(&p.ab[0], &p.ba[0]), (&p.ba[0], &p.ab[0])] {
            self.dispatch(enc, "occlusion", Params::level(l0[0], l0[1]), l0, &[(7, this.as_entire_binding()), (9, other.as_entire_binding()), (8, p.tmp.as_entire_binding())]);
            enc.copy_buffer_to_buffer(&p.tmp, 0, this, 0, n0);
        }
        // Motion for what's hidden, from what's around it.
        // Strides doubling up to the picture's size, alike neighbours only, then anything.
        let mut passes: Vec<(i32, i32)> = Vec::new();
        let mut stride = 1;
        while stride < l0[0].max(l0[1]) as i32 / 4 {
            passes.push((stride, 0));
            stride *= 2;
        }
        passes.extend([(1, 0), (1, 0), (1, 1), (2, 1), (4, 1), (8, 1), (16, 1), (64, 1)]);
        for (grey, flow) in [(&p.grey_a[0], &p.ab[0]), (&p.grey_b[0], &p.ba[0])] {
            for &(stride, any) in &passes {
                let prm = Params { radius: stride, wide: any, ..Params::level(l0[0], l0[1]) };
                self.dispatch(enc, "fill", prm, l0, &[(4, grey.as_entire_binding()), (7, flow.as_entire_binding()), (8, p.tmp.as_entire_binding())]);
                enc.copy_buffer_to_buffer(&p.tmp, 0, flow, 0, n0);
            }
        }
    }

    fn synthesise(&self, p: &Pair, t: f32) -> Option<FloatPixels> {
        let (Some(ia), Some(ib)) = (&p.a, &p.b) else { return None };
        let l0 = p.levels[0];
        let base = Params { t, ..Params::level(l0[0], l0[1]) };
        let mut enc = self.device.create_command_encoder(&Default::default());
        self.dispatch(&mut enc, "proj_clear", base, l0, &[(10, p.proj_a.as_entire_binding()), (11, p.proj_b.as_entire_binding())]);
        self.dispatch(&mut enc, "project", Params { s: t, ..base }, l0, &[(7, p.ab[0].as_entire_binding()), (10, p.proj_a.as_entire_binding())]);
        self.dispatch(&mut enc, "project", Params { s: 1.0 - t, ..base }, l0, &[(7, p.ba[0].as_entire_binding()), (10, p.proj_b.as_entire_binding())]);
        self.dispatch(
            &mut enc,
            "resolve",
            base,
            l0,
            &[
                (7, p.ab[0].as_entire_binding()),
                (9, p.ba[0].as_entire_binding()),
                (10, p.proj_a.as_entire_binding()),
                (11, p.proj_b.as_entire_binding()),
                (12, p.tflow.as_entire_binding()),
                (13, p.vis.as_entire_binding()),
            ],
        );
        let out_view = p.target.create_view(&Default::default());
        let flags = u32::from(ia.straight) | (u32::from(ib.straight) << 1);
        self.dispatch(
            &mut enc,
            "synth",
            Params { ow: p.out[0], oh: p.out[1], flags, ..base },
            p.out,
            &[
                (1, wgpu::BindingResource::TextureView(&ia.view)),
                (2, wgpu::BindingResource::TextureView(&ib.view)),
                (3, wgpu::BindingResource::Sampler(&self.sampler)),
                (12, p.tflow.as_entire_binding()),
                (13, p.vis.as_entire_binding()),
                (14, wgpu::BindingResource::TextureView(&out_view)),
            ],
        );
        self.queue.submit([enc.finish()]);
        let raw = crate::read_texture_bytes(&self.device, &self.queue, &p.target, p.out, 8)?;
        Some(FloatPixels { width: p.out[0], height: p.out[1], rgba: raw.chunks_exact(2).map(|b| u16::from_le_bytes([b[0], b[1]])).collect() })
    }
}
