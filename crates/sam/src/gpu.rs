//! The image encoder on the graphics card (wgpu compute shaders,
//! `encoder.wgsl`): the same network as [`Sam::encode`], step for step, ten
//! or so times quicker than the CPU on a typical graphics card. The prompt
//! decoder stays on the CPU (it takes a tenth of a second).

use super::{Sam, INPUT};
use std::collections::HashMap;
use std::sync::Arc;
use wgpu::util::DeviceExt;

pub struct GpuEncoder {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipes: HashMap<&'static str, wgpu::ComputePipeline>,
    /// Every weight, and each attention block's full bias table.
    w: HashMap<String, (Vec<usize>, wgpu::Buffer)>,
    dummy: wgpu::Buffer,
    /// The upscaler's weights, loaded when first needed.
    up: std::sync::OnceLock<HashMap<String, (Vec<usize>, wgpu::Buffer)>>,
    /// What it runs on, for the status line.
    pub adapter: String,
}

/// A buffer of f32s on the card, and how many.
#[derive(Clone)]
struct T {
    buf: Arc<wgpu::Buffer>,
    len: usize,
}

const ENTRIES: [&str; 14] = ["gemm", "im2col", "dwconv", "gelu", "add", "copy", "layernorm", "layernorm2d", "transpose", "windows", "unwindow", "attention", "prelu", "shuffle"];

impl GpuEncoder {
    /// A graphics card to run on, with the weights loaded onto it (`None`
    /// without a usable one).
    pub fn new(sam: &Sam) -> Option<GpuEncoder> {
        Self::open(sam, std::env::var_os("REELFORGE_SAM_GPU").is_some())
    }

    /// As [`Self::new`]; `software`: a software renderer will do (for tests).
    pub fn open(sam: &Sam, software: bool) -> Option<GpuEncoder> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions { power_preference: wgpu::PowerPreference::HighPerformance, ..Default::default() })).ok()?;
        let info = adapter.get_info();
        // A software renderer is slower than the CPU code: not worth it.
        if info.device_type == wgpu::DeviceType::Cpu && !software {
            return None;
        }
        let want = adapter.limits();
        let limits = wgpu::Limits { max_storage_buffer_binding_size: want.max_storage_buffer_binding_size.min(1 << 30), max_buffer_size: want.max_buffer_size.min(1 << 30), ..wgpu::Limits::default() };
        if limits.max_storage_buffer_binding_size < 80 << 20 {
            return None;
        }
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor { label: Some("roto brush model"), required_limits: limits, ..Default::default() })).ok()?;
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("sam encoder"), source: wgpu::ShaderSource::Wgsl(include_str!("encoder.wgsl").into()) });
        let pipes = ENTRIES
            .iter()
            .map(|e| {
                let p = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor { label: Some(e), layout: None, module: &module, entry_point: Some(e), compilation_options: Default::default(), cache: None });
                (*e, p)
            })
            .collect();
        let up = |label: &str, v: &[f32]| device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some(label), contents: bytes(v), usage: wgpu::BufferUsages::STORAGE });
        let mut w = HashMap::new();
        for (k, (shape, v)) in &sam.w {
            if k.starts_with("image_encoder.") {
                w.insert(k.clone(), (shape.clone(), up(k, v)));
            }
        }
        for (k, v) in &sam.biases {
            w.insert(format!("{k}.table"), (vec![v.len()], up(k, v)));
        }
        let dummy = up("dummy", &[0.0; 4]);
        Some(GpuEncoder { device, queue, pipes, w, dummy, up: Default::default(), adapter: format!("{} ({:?})", info.name, info.backend) })
    }

    /// One upscaler tile (see `crate::upscale`): `rgb` three planes h × w,
    /// 0–1 → three planes four times the size.
    pub fn upscale_tile(&self, m: &crate::upscale::Upscaler, rgb: &[f32], w: usize, h: usize) -> Option<Vec<f32>> {
        let up = self.up.get_or_init(|| m.w.iter().map(|(k, (shape, v))| (k.clone(), (shape.clone(), self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: Some(k), contents: bytes(v), usage: wgpu::BufferUsages::STORAGE })))).collect());
        let mut r = Run { g: self, enc: self.device.create_command_encoder(&Default::default()), keep: Vec::new() };
        let input = r.upload(rgb);
        let hw = w * h;
        let mut x = input.clone();
        let mut c = 3usize;
        for layer in (0..=66).step_by(2) {
            let wt = &up.get(&format!("body.{layer}.weight"))?.1;
            let (bias_shape, bias) = up.get(&format!("body.{layer}.bias"))?;
            let o = bias_shape[0];
            let cols = self.new_t(c * 9 * hw);
            let mut u = [0u32; 16];
            u[..8].copy_from_slice(&[c as u32, h as u32, w as u32, 3, 1, 1, h as u32, w as u32]);
            r.go("im2col", u, [hw.div_ceil(256) as u32, (c * 9) as u32, 1], &[(1, &x.buf), (3, &cols.buf)]);
            let y = r.gemm(o, c * 9, hw, wt, &cols.buf.clone(), false, Some((bias, true)));
            if layer < 66 {
                let a = &up.get(&format!("body.{}.weight", layer + 1))?.1;
                let mut u = [0u32; 16];
                u[..2].copy_from_slice(&[(o * hw) as u32, hw as u32]);
                r.go("prelu", u, Run::flat(o * hw), &[(2, a), (3, &y.buf)]);
            }
            r.flush();
            x = y;
            c = o;
        }
        let out = self.new_t(3 * hw * 16);
        let mut u = [0u32; 16];
        u[..2].copy_from_slice(&[h as u32, w as u32]);
        r.go("shuffle", u, [(w * 4).div_ceil(256) as u32, (3 * h * 4) as u32, 1], &[(1, &x.buf), (2, &input.buf), (3, &out.buf)]);
        self.read(r, &out)
    }

    /// Submit what `r` recorded and read `out` back.
    fn read(&self, mut r: Run<'_>, out: &T) -> Option<Vec<f32>> {
        let size = (out.len * 4) as u64;
        let staging = self.device.create_buffer(&wgpu::BufferDescriptor { label: Some("read back"), size, usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        r.enc.copy_buffer_to_buffer(&out.buf, 0, &staging, 0, size);
        self.queue.submit([r.enc.finish()]);
        let slice = staging.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |res| {
            let _ = tx.send(res);
        });
        self.device.poll(wgpu::PollType::wait_indefinitely()).ok()?;
        rx.recv().ok()?.ok()?;
        let data = slice.get_mapped_range().ok()?;
        let v: Vec<f32> = data.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
        drop(data);
        staging.unmap();
        v.iter().all(|x| x.is_finite()).then_some(v)
    }

    fn new_t(&self, len: usize) -> T {
        let buf = self.device.create_buffer(&wgpu::BufferDescriptor { label: None, size: (len.max(1) * 4) as u64, usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        T { buf: Arc::new(buf), len }
    }

    fn wt(&self, k: &str) -> &wgpu::Buffer {
        &self.w.get(k).unwrap_or_else(|| panic!("no weight {k}")).1
    }
    fn shape(&self, k: &str) -> &[usize] {
        &self.w[k].0
    }

    /// The image encoder: a 3 × 1024 × 1024 normalised picture → 256 × 64 × 64.
    pub fn encode(&self, img: &[f32]) -> Option<Vec<f32>> {
        if img.len() != 3 * INPUT * INPUT {
            return None;
        }
        let mut r = Run { g: self, enc: self.device.create_command_encoder(&Default::default()), keep: Vec::new() };
        let x = r.upload(img);
        let out = r.encoder(x);
        // Read it back.
        let size = (out.len * 4) as u64;
        let staging = self.device.create_buffer(&wgpu::BufferDescriptor { label: Some("sam out"), size, usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        r.enc.copy_buffer_to_buffer(&out.buf, 0, &staging, 0, size);
        self.queue.submit([r.enc.finish()]);
        let slice = staging.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |res| {
            let _ = tx.send(res);
        });
        self.device.poll(wgpu::PollType::wait_indefinitely()).ok()?;
        rx.recv().ok()?.ok()?;
        let data = slice.get_mapped_range().ok()?;
        let v: Vec<f32> = data.chunks_exact(4).map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]])).collect();
        drop(data);
        staging.unmap();
        v.iter().all(|x| x.is_finite()).then_some(v)
    }
}

fn bytes(v: &[f32]) -> &[u8] {
    unsafe { std::slice::from_raw_parts(v.as_ptr() as *const u8, std::mem::size_of_val(v)) }
}

/// One encoding being recorded.
struct Run<'a> {
    g: &'a GpuEncoder,
    enc: wgpu::CommandEncoder,
    /// Uniform buffers in use until it's submitted.
    keep: Vec<wgpu::Buffer>,
}

impl Run<'_> {
    /// Send what's recorded and wait for it, so the buffers it used are let go.
    fn flush(&mut self) {
        let enc = std::mem::replace(&mut self.enc, self.g.device.create_command_encoder(&Default::default()));
        self.g.queue.submit([enc.finish()]);
        let _ = self.g.device.poll(wgpu::PollType::wait_indefinitely());
        self.keep.clear();
    }

    fn upload(&mut self, v: &[f32]) -> T {
        let buf = self.g.device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: None, contents: bytes(v), usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC });
        T { buf: Arc::new(buf), len: v.len() }
    }

    /// Run kernel `entry` with sizes `u` over `groups`, its buffers bound
    /// as (binding, buffer).
    fn go(&mut self, entry: &'static str, mut u: [u32; 16], groups: [u32; 3], bind: &[(u32, &wgpu::Buffer)]) {
        if u[15] == 0 {
            u[15] = groups[0];
        }
        let ub = self.g.device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: None, contents: unsafe { std::slice::from_raw_parts(u.as_ptr() as *const u8, 64) }, usage: wgpu::BufferUsages::UNIFORM });
        let pipe = &self.g.pipes[entry];
        let mut entries = vec![wgpu::BindGroupEntry { binding: 0, resource: ub.as_entire_binding() }];
        entries.extend(bind.iter().map(|(b, buf)| wgpu::BindGroupEntry { binding: *b, resource: buf.as_entire_binding() }));
        let group = self.g.device.create_bind_group(&wgpu::BindGroupDescriptor { label: Some(entry), layout: &pipe.get_bind_group_layout(0), entries: &entries });
        {
            let mut pass = self.enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some(entry), timestamp_writes: None });
            pass.set_pipeline(pipe);
            pass.set_bind_group(0, &group, &[]);
            pass.dispatch_workgroups(groups[0], groups[1], groups[2]);
        }
        self.keep.push(ub);
    }

    /// Groups for `n` items, 256 to a group, spread over two dimensions.
    fn flat(n: usize) -> [u32; 3] {
        let groups = n.div_ceil(256).max(1) as u32;
        let gx = groups.min(32768);
        [gx, groups.div_ceil(gx), 1]
    }

    /// c = a (m × k) · b (k × n, or n × k when `bt`) + bias.
    fn gemm(&mut self, m: usize, k: usize, n: usize, a: &wgpu::Buffer, b: &wgpu::Buffer, bt: bool, bias: Option<(&wgpu::Buffer, bool)>) -> T {
        let out = self.g.new_t(m * n);
        let mode = match bias {
            None => 0,
            Some((_, true)) => 1,
            Some((_, false)) => 2,
        };
        let d = bias.map_or(&self.g.dummy, |b| b.0);
        let gx = n.div_ceil(64) as u32;
        let gy = m.div_ceil(64) as u32;
        let mut u = [0u32; 16];
        u[..5].copy_from_slice(&[m as u32, k as u32, n as u32, bt as u32, mode]);
        self.go("gemm", u, [gx, gy, 1], &[(1, a), (2, b), (3, &out.buf), (4, d)]);
        out
    }

    /// A convolution (bias if it has one): x is C × H × W.
    fn conv(&mut self, x: &T, c: usize, h: usize, w: usize, pre: &str, stride: usize, pad: usize) -> (T, usize, usize) {
        let g = self.g;
        let s = g.shape(&format!("{pre}.weight")).to_vec();
        let (o, kh) = (s[0], s[2]);
        let ho = (h + 2 * pad - kh) / stride + 1;
        let wo = (w + 2 * pad - kh) / stride + 1;
        let k = c * kh * kh;
        let n = ho * wo;
        let wt = g.wt(&format!("{pre}.weight"));
        let bias = g.w.get(&format!("{pre}.bias")).map(|(_, b)| (b, true));
        let out = if kh == 1 && stride == 1 && pad == 0 {
            self.gemm(o, k, n, wt, &x.buf.clone(), false, bias)
        } else {
            let cols = g.new_t(k * n);
            let mut u = [0u32; 16];
            u[..8].copy_from_slice(&[c as u32, h as u32, w as u32, kh as u32, stride as u32, pad as u32, ho as u32, wo as u32]);
            self.go("im2col", u, [n.div_ceil(256) as u32, k as u32, 1], &[(1, &x.buf), (3, &cols.buf)]);
            self.gemm(o, k, n, wt, &cols.buf.clone(), false, bias)
        };
        (out, ho, wo)
    }

    fn dwconv(&mut self, x: &T, c: usize, h: usize, w: usize, pre: &str, stride: usize) -> (T, usize, usize) {
        let ho = (h + 2 - 3) / stride + 1;
        let wo = (w + 2 - 3) / stride + 1;
        let out = self.g.new_t(c * ho * wo);
        let mut u = [0u32; 16];
        u[..6].copy_from_slice(&[c as u32, h as u32, w as u32, stride as u32, ho as u32, wo as u32]);
        let (wt, b) = (self.g.wt(&format!("{pre}.weight")), self.g.wt(&format!("{pre}.bias")));
        self.go("dwconv", u, [(ho * wo).div_ceil(256) as u32, c as u32, 1], &[(1, &x.buf), (2, wt), (3, &out.buf), (4, b)]);
        (out, ho, wo)
    }

    fn gelu(&mut self, x: &T) {
        let mut u = [0u32; 16];
        u[0] = x.len as u32;
        self.go("gelu", u, Self::flat(x.len), &[(3, &x.buf)]);
    }

    /// x += y.
    fn add(&mut self, x: &T, y: &T) {
        let mut u = [0u32; 16];
        u[0] = x.len as u32;
        self.go("add", u, Self::flat(x.len), &[(1, &y.buf), (3, &x.buf)]);
    }

    fn copy(&mut self, x: &T) -> T {
        let out = self.g.new_t(x.len);
        let mut u = [0u32; 16];
        u[0] = x.len as u32;
        self.go("copy", u, Self::flat(x.len), &[(1, &x.buf), (3, &out.buf)]);
        out
    }

    fn layernorm(&mut self, x: &T, c: usize, pre: &str, eps: f32) {
        let rows = x.len / c;
        let mut u = [0u32; 16];
        u[..3].copy_from_slice(&[rows as u32, c as u32, eps.to_bits()]);
        let (g, b) = (self.g.wt(&format!("{pre}.weight")), self.g.wt(&format!("{pre}.bias")));
        self.go("layernorm", u, Self::flat(rows), &[(2, g), (3, &x.buf), (4, b)]);
    }

    fn layernorm2d(&mut self, x: &T, c: usize, hw: usize, pre: &str) {
        let mut u = [0u32; 16];
        u[..3].copy_from_slice(&[hw as u32, c as u32, 1e-6f32.to_bits()]);
        let (g, b) = (self.g.wt(&format!("{pre}.weight")), self.g.wt(&format!("{pre}.bias")));
        self.go("layernorm2d", u, Self::flat(hw), &[(2, g), (3, &x.buf), (4, b)]);
    }

    fn transpose(&mut self, x: &T, rows: usize, cols: usize) -> T {
        let out = self.g.new_t(x.len);
        let mut u = [0u32; 16];
        u[..2].copy_from_slice(&[rows as u32, cols as u32]);
        self.go("transpose", u, [cols.div_ceil(256) as u32, rows as u32, 1], &[(1, &x.buf), (3, &out.buf)]);
        out
    }

    /// x (n × in) · weightᵀ + bias.
    fn linear(&mut self, x: &T, n: usize, pre: &str) -> T {
        let s = self.g.shape(&format!("{pre}.weight")).to_vec();
        let (o, i) = (s[0], s[1]);
        let g = self.g;
        let (wt, b) = (g.wt(&format!("{pre}.weight")), g.wt(&format!("{pre}.bias")));
        self.gemm(n, i, o, &x.buf.clone(), wt, true, Some((b, false)))
    }

    fn tvit_block(&mut self, x: &T, h: usize, w: usize, c: usize, pre: &str, heads: usize, ws: usize) -> T {
        let kd = c / heads;
        let (nh, nw) = (h.div_ceil(ws), w.div_ceil(ws));
        let nwin = nh * nw;
        let n = ws * ws;
        let tokens = nwin * n;
        let win = self.g.new_t(tokens * c);
        let mut u = [0u32; 16];
        u[..5].copy_from_slice(&[h as u32, w as u32, c as u32, ws as u32, nw as u32]);
        self.go("windows", u, [c.div_ceil(256) as u32, tokens as u32, 1], &[(1, &x.buf), (3, &win.buf)]);
        self.layernorm(&win, c, &format!("{pre}.attn.norm"), 1e-5);
        let qkv = self.linear(&win, tokens, &format!("{pre}.attn.qkv"));
        let att = self.g.new_t(tokens * c);
        let mut u = [0u32; 16];
        u[..5].copy_from_slice(&[nwin as u32, n as u32, heads as u32, kd as u32, (kd as f32).powf(-0.5).to_bits()]);
        let table = self.g.wt(&format!("{pre}.attn.attention_biases.table"));
        self.go("attention", u, [n.div_ceil(64) as u32, (nwin * heads) as u32, 1], &[(1, &qkv.buf), (2, table), (3, &att.buf)]);
        let proj = self.linear(&att, tokens, &format!("{pre}.attn.proj"));
        let mut u = [0u32; 16];
        u[..5].copy_from_slice(&[h as u32, w as u32, c as u32, ws as u32, nw as u32]);
        self.go("unwindow", u, [c.div_ceil(256) as u32, (h * w) as u32, 1], &[(1, &proj.buf), (3, &x.buf)]);
        // The local convolution, on the grid (it replaces x).
        let chw = self.transpose(x, h * w, c);
        let (lc, _, _) = self.dwconv(&chw, c, h, w, &format!("{pre}.local_conv"), 1);
        let x = self.transpose(&lc, c, h * w);
        // The MLP.
        let m = self.copy(&x);
        self.layernorm(&m, c, &format!("{pre}.mlp.norm"), 1e-5);
        let m = self.linear(&m, h * w, &format!("{pre}.mlp.fc1"));
        self.gelu(&m);
        let m = self.linear(&m, h * w, &format!("{pre}.mlp.fc2"));
        self.add(&x, &m);
        x
    }

    fn patch_merge(&mut self, x: &T, c: usize, h: usize, w: usize, pre: &str, out: usize) -> (T, usize, usize) {
        let (y, h1, w1) = self.conv(x, c, h, w, &format!("{pre}.conv1"), 1, 0);
        self.gelu(&y);
        let stride = if out == 320 { 1 } else { 2 };
        let (y, h2, w2) = self.dwconv(&y, out, h1, w1, &format!("{pre}.conv2"), stride);
        self.gelu(&y);
        let (y, _, _) = self.conv(&y, out, h2, w2, &format!("{pre}.conv3"), 1, 0);
        (y, h2, w2)
    }

    /// As [`Sam::encode`].
    fn encoder(&mut self, img: T) -> T {
        let p = "image_encoder";
        let (x, h, w) = self.conv(&img, 3, INPUT, INPUT, &format!("{p}.patch_embed.seq.0"), 2, 1);
        self.gelu(&x);
        let (mut x, h, w) = self.conv(&x, 32, h, w, &format!("{p}.patch_embed.seq.2"), 2, 1);
        self.flush();
        for b in 0..2 {
            let q = format!("{p}.layers.0.blocks.{b}");
            let (y, _, _) = self.conv(&x, 64, h, w, &format!("{q}.conv1"), 1, 0);
            self.gelu(&y);
            let (y, _, _) = self.dwconv(&y, 256, h, w, &format!("{q}.conv2"), 1);
            self.gelu(&y);
            let (y, _, _) = self.conv(&y, 256, h, w, &format!("{q}.conv3"), 1, 0);
            self.add(&y, &x);
            self.gelu(&y);
            x = y;
            self.flush();
        }
        let (mut x, mut h, mut w) = self.patch_merge(&x, 64, h, w, &format!("{p}.layers.0.downsample"), 128);
        let dims = [64, 128, 160, 320];
        let heads = [2, 4, 5, 10];
        let wss = [7, 7, 14, 7];
        let depths = [2, 2, 6, 2];
        for li in 1..4 {
            let c = dims[li];
            let mut t = self.transpose(&x, c, h * w);
            for b in 0..depths[li] {
                t = self.tvit_block(&t, h, w, c, &format!("{p}.layers.{li}.blocks.{b}"), heads[li], wss[li]);
                self.flush();
            }
            x = self.transpose(&t, h * w, c);
            if li < 3 {
                let (y, h2, w2) = self.patch_merge(&x, c, h, w, &format!("{p}.layers.{li}.downsample"), dims[li + 1]);
                (x, h, w) = (y, h2, w2);
            }
        }
        let (x, _, _) = self.conv(&x, 320, h, w, &format!("{p}.neck.0"), 1, 0);
        self.layernorm2d(&x, 256, h * w, &format!("{p}.neck.1"));
        let (x, _, _) = self.conv(&x, 256, h, w, &format!("{p}.neck.2"), 1, 1);
        self.layernorm2d(&x, 256, h * w, &format!("{p}.neck.3"));
        x
    }
}
