//! Segment Anything (MobileSAM) on the CPU: point, box and mask prompts
//! picking out an object in a picture, for the roto brush.
//!
//! MobileSAM (Zhang et al. 2023, Apache-2.0) is Segment Anything's prompt
//! encoder and mask decoder with a small TinyViT image encoder. Its weights
//! ship with the app (batch norms folded into their convolutions; the big
//! tensors as int8 with a scale per output channel, the rest as f16: about
//! 10 MB; see `load`); this module runs the network: the image encoder once per
//! frame ([`embed`], a second or two), then the decoder for each set of
//! prompts ([`cut`], a tenth of a second).

use rayon::prelude::*;

mod gpu;
pub use gpu::GpuEncoder;
use std::collections::HashMap;

/// The encoder's input size, and its output grid.
pub const INPUT: usize = 1024;
pub const GRID: usize = 64;
/// The decoder's mask resolution.
pub const MASK: usize = 256;

/// The network's weights.
pub struct Sam {
    w: HashMap<String, (Vec<usize>, Vec<f32>)>,
    /// Each attention block's position biases, (heads × N × N), by block.
    biases: HashMap<String, Vec<f32>>,
}

fn f16_to_f32(h: u16) -> f32 {
    let s = ((h >> 15) & 1) as u32;
    let e = ((h >> 10) & 0x1f) as u32;
    let m = (h & 0x3ff) as u32;
    let bits = if e == 0 {
        if m == 0 {
            s << 31
        } else {
            // Subnormal: normalise.
            let mut e = 127 - 15 + 1;
            let mut m = m;
            while m & 0x400 == 0 {
                m <<= 1;
                e -= 1;
            }
            (s << 31) | ((e as u32) << 23) | ((m & 0x3ff) << 13)
        }
    } else if e == 31 {
        (s << 31) | (0xff << 23) | (m << 13)
    } else {
        (s << 31) | ((e + 127 - 15) << 23) | (m << 13)
    };
    f32::from_bits(bits)
}

/// e^x, branch-free so loops over it vectorise (relative error about 2e-7).
#[inline(always)]
fn exp_fast(x: f32) -> f32 {
    let x = x.clamp(-87.0, 88.0);
    // Rounded by the float adder (no rounding instruction needed: SSE2 is enough).
    const MAGIC: f32 = 12_582_912.0;
    let n = (x * std::f32::consts::LOG2_E + MAGIC) - MAGIC;
    let r = x - n * 0.693_145_75 - n * 1.428_606_8e-6;
    let p = 1.0 + r * (1.0 + r * (0.5 + r * (0.166_666_67 + r * (0.041_666_668 + r * (0.008_333_334 + r * 0.001_388_889)))));
    p * f32::from_bits(((n as i32 + 127) << 23) as u32)
}

/// The error function (Abramowitz and Stegun 7.1.26: to within 1.5e-7).
#[inline(always)]
fn erf(x: f32) -> f32 {
    let a = x.abs();
    let t = 1.0 / (1.0 + 0.3275911 * a);
    let y = 1.0 - (((((1.061405429 * t - 1.453152027) * t) + 1.421413741) * t - 0.284496736) * t + 0.254829592) * t * exp_fast(-a * a);
    y.copysign(x)
}

fn softmax_rows(s: &mut [f32], n: usize) {
    for r in s.chunks_mut(n) {
        let mx = r.iter().fold(f32::MIN, |a, &b| a.max(b));
        let mut sum = 0.0;
        for v in r.iter_mut() {
            *v = exp_fast(*v - mx);
            sum += *v;
        }
        let k = 1.0 / sum;
        r.iter_mut().for_each(|v| *v *= k);
    }
}

fn gelu(v: &mut [f32]) {
    v.par_chunks_mut(8192).for_each(|c| {
        for x in c {
            *x = 0.5 * *x * (1.0 + erf(*x * std::f32::consts::FRAC_1_SQRT_2));
        }
    });
}

/// c (m × n) = a (m × k) · b (k × n), row-major; `bt` reads b as its transpose (n × k).
fn gemm(m: usize, k: usize, n: usize, a: &[f32], b: &[f32], bt: bool, c: &mut [f32]) {
    let (rsb, csb) = if bt { (1, k as isize) } else { (n as isize, 1) };
    // Split the rows across threads.
    let rows = m.div_ceil(rayon::current_num_threads().max(1)).max(16);
    c.par_chunks_mut(rows * n).enumerate().for_each(|(i, cc)| {
        let r0 = i * rows;
        let mm = cc.len() / n;
        unsafe {
            matrixmultiply::sgemm(mm, k, n, 1.0, a[r0 * k..].as_ptr(), k as isize, 1, b.as_ptr(), rsb, csb, 0.0, cc.as_mut_ptr(), n as isize, 1);
        }
    });
}

impl Sam {
    /// The weights from the packed file (see the module notes).
    pub fn load(bytes: &[u8]) -> Option<Sam> {
        if bytes.len() < 12 {
            return None;
        }
        let hl = u32::from_le_bytes(bytes[8..12].try_into().ok()?) as usize;
        let mut w = HashMap::new();
        let f16 = |d: &[u8], off: usize, n: usize| -> Vec<f32> { (0..n).map(|i| f16_to_f32(u16::from_le_bytes([d[off + 2 * i], d[off + 2 * i + 1]]))).collect() };
        match &bytes[..6] {
            // f16 throughout: offsets in halves.
            b"RFSAM1" => {
                let head: HashMap<String, (usize, Vec<usize>)> = serde_json::from_slice(&bytes[12..12 + hl]).ok()?;
                let data = &bytes[12 + hl + (12 + hl) % 2..];
                for (k, (off, shape)) in head {
                    let n: usize = shape.iter().product::<usize>().max(1);
                    w.insert(k, (shape.clone(), f16(data, 2 * off, n)));
                }
            }
            // The big tensors int8 with a scale per output channel (f32,
            // before them), the rest f16: offsets in bytes.
            b"RFSAM2" => {
                let head: HashMap<String, (usize, Vec<usize>, String)> = serde_json::from_slice(&bytes[12..12 + hl]).ok()?;
                let data = &bytes[12 + hl + (4 - (12 + hl) % 4) % 4..];
                for (k, (off, shape, kind)) in head {
                    let n: usize = shape.iter().product::<usize>().max(1);
                    let v = if kind == "i8" {
                        let rows = shape[0].max(1);
                        let per = n / rows;
                        let scales: Vec<f32> = (0..rows).map(|r| f32::from_le_bytes(data[off + 4 * r..off + 4 * r + 4].try_into().unwrap())).collect();
                        let q = &data[off + 4 * rows..off + 4 * rows + n];
                        q.iter().enumerate().map(|(i, &b)| b as i8 as f32 * scales[i / per]).collect()
                    } else {
                        f16(data, off, n)
                    };
                    w.insert(k, (shape, v));
                }
            }
            _ => return None,
        }
        let mut sam = Sam { w, biases: HashMap::new() };
        // Attention biases expanded to full (heads × N × N) tables.
        let keys: Vec<String> = sam.w.keys().filter(|k| k.ends_with(".attn.attention_biases")).cloned().collect();
        for k in keys {
            let (shape, b) = sam.w[&k].clone();
            let (heads, offs) = (shape[0], shape[1]);
            let ws = (offs as f64).sqrt().round() as usize;
            let n = ws * ws;
            let mut table = vec![0.0; heads * n * n];
            // As the model numbers them: each offset (|dy|, |dx|) in order of first appearance.
            let mut ids: HashMap<(usize, usize), usize> = HashMap::new();
            for p1 in 0..n {
                for p2 in 0..n {
                    let o = ((p1 / ws).abs_diff(p2 / ws), (p1 % ws).abs_diff(p2 % ws));
                    let len = ids.len();
                    let id = *ids.entry(o).or_insert(len);
                    for h in 0..heads {
                        table[(h * n + p1) * n + p2] = b[h * offs + id];
                    }
                }
            }
            sam.biases.insert(k, table);
        }
        Some(sam)
    }

    fn t(&self, k: &str) -> &[f32] {
        &self.w.get(k).unwrap_or_else(|| panic!("no weight {k}")).1
    }
    fn shape(&self, k: &str) -> &[usize] {
        &self.w[k].0
    }

    /// A full convolution (im2col and a matrix product). x: C × H × W.
    fn conv(&self, x: &[f32], c: usize, h: usize, w: usize, pre: &str, stride: usize, pad: usize) -> (Vec<f32>, usize, usize) {
        let wt = self.t(&format!("{pre}.weight"));
        let s = self.shape(&format!("{pre}.weight")).to_vec();
        let (o, kh, kw) = (s[0], s[2], s[3]);
        let ho = (h + 2 * pad - kh) / stride + 1;
        let wo = (w + 2 * pad - kw) / stride + 1;
        let k = c * kh * kw;
        let n = ho * wo;
        let mut out = vec![0.0f32; o * n];
        if kh == 1 && kw == 1 && stride == 1 && pad == 0 {
            gemm(o, k, n, wt, x, false, &mut out);
        } else {
            let mut cols = vec![0.0f32; k * n];
            cols.par_chunks_mut(n).enumerate().for_each(|(row, dst)| {
                let (ci, ki, kj) = (row / (kh * kw), (row / kw) % kh, row % kw);
                for yo in 0..ho {
                    let yi = (yo * stride + ki) as isize - pad as isize;
                    for xo in 0..wo {
                        let xi = (xo * stride + kj) as isize - pad as isize;
                        dst[yo * wo + xo] = if yi >= 0 && xi >= 0 && (yi as usize) < h && (xi as usize) < w { x[(ci * h + yi as usize) * w + xi as usize] } else { 0.0 };
                    }
                }
            });
            gemm(o, k, n, wt, &cols, false, &mut out);
        }
        if let Some((_, b)) = self.w.get(&format!("{pre}.bias")) {
            out.par_chunks_mut(n).zip(b.par_iter()).for_each(|(row, b)| row.iter_mut().for_each(|v| *v += b));
        }
        (out, ho, wo)
    }

    /// A depthwise 3 × 3 convolution, padding 1.
    fn dwconv(&self, x: &[f32], c: usize, h: usize, w: usize, pre: &str, stride: usize) -> (Vec<f32>, usize, usize) {
        let wt = self.t(&format!("{pre}.weight"));
        let b = self.t(&format!("{pre}.bias"));
        let ho = (h + 2 - 3) / stride + 1;
        let wo = (w + 2 - 3) / stride + 1;
        let mut out = vec![0.0f32; c * ho * wo];
        let (pw, ph) = (w + 2, h + 2);
        out.par_chunks_mut(ho * wo).enumerate().for_each(|(ci, dst)| {
            let k = &wt[ci * 9..ci * 9 + 9];
            let src = &x[ci * h * w..(ci + 1) * h * w];
            // The channel padded with a border of zeros, so the taps need no checks.
            let mut p = vec![0.0f32; pw * ph];
            for y in 0..h {
                p[(y + 1) * pw + 1..(y + 1) * pw + 1 + w].copy_from_slice(&src[y * w..(y + 1) * w]);
            }
            for yo in 0..ho {
                let row = &mut dst[yo * wo..(yo + 1) * wo];
                row.fill(b[ci]);
                for i in 0..3 {
                    let line = &p[(yo * stride + i) * pw..(yo * stride + i + 1) * pw];
                    let (k0, k1, k2) = (k[i * 3], k[i * 3 + 1], k[i * 3 + 2]);
                    if stride == 1 {
                        for (xo, o) in row.iter_mut().enumerate() {
                            *o += k0 * line[xo] + k1 * line[xo + 1] + k2 * line[xo + 2];
                        }
                    } else {
                        for (xo, o) in row.iter_mut().enumerate() {
                            let x0 = xo * stride;
                            *o += k0 * line[x0] + k1 * line[x0 + 1] + k2 * line[x0 + 2];
                        }
                    }
                }
            }
        });
        (out, ho, wo)
    }

    /// x (n × in) · weightᵀ + bias.
    fn linear(&self, x: &[f32], n: usize, pre: &str) -> Vec<f32> {
        let s = self.shape(&format!("{pre}.weight")).to_vec();
        let (o, i) = (s[0], s[1]);
        let mut out = vec![0.0f32; n * o];
        gemm(n, i, o, x, self.t(&format!("{pre}.weight")), true, &mut out);
        let b = self.t(&format!("{pre}.bias"));
        out.par_chunks_mut(o).for_each(|r| r.iter_mut().zip(b).for_each(|(v, b)| *v += b));
        out
    }

    fn layernorm(&self, x: &mut [f32], c: usize, pre: &str, eps: f32) {
        let (g, b) = (self.t(&format!("{pre}.weight")), self.t(&format!("{pre}.bias")));
        x.par_chunks_mut(c).for_each(|r| {
            let m = r.iter().sum::<f32>() / c as f32;
            let v = r.iter().map(|x| (x - m) * (x - m)).sum::<f32>() / c as f32;
            let k = 1.0 / (v + eps).sqrt();
            for (i, x) in r.iter_mut().enumerate() {
                *x = (*x - m) * k * g[i] + b[i];
            }
        });
    }

    /// LayerNorm over channels of a C × H × W map.
    fn layernorm2d(&self, x: &mut [f32], c: usize, hw: usize, pre: &str) {
        let (g, b) = (self.t(&format!("{pre}.weight")), self.t(&format!("{pre}.bias")));
        let mut mean = vec![0.0f32; hw];
        let mut var = vec![0.0f32; hw];
        for ci in 0..c {
            for p in 0..hw {
                mean[p] += x[ci * hw + p];
            }
        }
        mean.iter_mut().for_each(|m| *m /= c as f32);
        for ci in 0..c {
            for p in 0..hw {
                let d = x[ci * hw + p] - mean[p];
                var[p] += d * d;
            }
        }
        let inv: Vec<f32> = var.iter().map(|v| 1.0 / (v / c as f32 + 1e-6).sqrt()).collect();
        x.par_chunks_mut(hw).enumerate().for_each(|(ci, r)| {
            for p in 0..hw {
                r[p] = (r[p] - mean[p]) * inv[p] * g[ci] + b[ci];
            }
        });
    }

    fn transpose(x: &[f32], rows: usize, cols: usize) -> Vec<f32> {
        let mut o = vec![0.0f32; x.len()];
        o.par_chunks_mut(rows).enumerate().for_each(|(c, dst)| {
            for r in 0..rows {
                dst[r] = x[r * cols + c];
            }
        });
        o
    }

    /// A TinyViT block over an h × w grid of tokens (n × c, row-major).
    fn tvit_block(&self, x: &mut Vec<f32>, h: usize, w: usize, c: usize, pre: &str, heads: usize, ws: usize) {
        let kd = c / heads;
        let (nh, nw) = (h.div_ceil(ws), w.div_ceil(ws));
        let nwin = nh * nw;
        let n = ws * ws;
        // Windows of tokens (padding with zeros), normed, then q, k, v.
        let mut win = vec![0.0f32; nwin * n * c];
        let mut real = vec![false; nwin * n];
        for wy in 0..nh {
            for wx in 0..nw {
                for i in 0..ws {
                    for j in 0..ws {
                        let (y, xx) = (wy * ws + i, wx * ws + j);
                        if y < h && xx < w {
                            let t = (wy * nw + wx) * n + i * ws + j;
                            win[t * c..(t + 1) * c].copy_from_slice(&x[(y * w + xx) * c..(y * w + xx + 1) * c]);
                            real[t] = true;
                        }
                    }
                }
            }
        }
        // (Padding goes through the norm as zeros, as in the model: it pads before attention.)
        self.layernorm(&mut win, c, &format!("{pre}.attn.norm"), 1e-5);
        let qkv = self.linear(&win, nwin * n, &format!("{pre}.attn.qkv"));
        let bias = &self.biases[&format!("{pre}.attn.attention_biases")];
        let scale = (kd as f32).powf(-0.5);
        let mut att = vec![0.0f32; nwin * n * c];
        att.par_chunks_mut(n * c).enumerate().for_each(|(wi, out)| {
            let base = wi * n * 3 * c;
            let mut s = vec![0.0f32; n * n];
            let rs = 3 * c as isize;
            for hh in 0..heads {
                let q = base + hh * 3 * kd;
                // Scores: q · kᵀ, scaled, plus the position biases.
                s.copy_from_slice(&bias[hh * n * n..(hh + 1) * n * n]);
                unsafe {
                    matrixmultiply::sgemm(n, kd, n, scale, qkv[q..].as_ptr(), rs, 1, qkv[q + kd..].as_ptr(), 1, rs, 1.0, s.as_mut_ptr(), n as isize, 1);
                }
                softmax_rows(&mut s, n);
                unsafe {
                    matrixmultiply::sgemm(n, n, kd, 1.0, s.as_ptr(), n as isize, 1, qkv[q + 2 * kd..].as_ptr(), rs, 1, 0.0, out[hh * kd..].as_mut_ptr(), c as isize, 1);
                }
            }
        });
        let proj = self.linear(&att, nwin * n, &format!("{pre}.attn.proj"));
        // Back onto the grid, added to what was there.
        for wy in 0..nh {
            for wx in 0..nw {
                for i in 0..ws {
                    for j in 0..ws {
                        let (y, xx) = (wy * ws + i, wx * ws + j);
                        if y < h && xx < w {
                            let t = (wy * nw + wx) * n + i * ws + j;
                            for e in 0..c {
                                x[(y * w + xx) * c + e] += proj[t * c + e];
                            }
                        }
                    }
                }
            }
        }
        let _ = real;
        // The local convolution, on the grid.
        let chw = Self::transpose(x, h * w, c);
        let (lc, _, _) = self.dwconv(&chw, c, h, w, &format!("{pre}.local_conv"), 1);
        *x = Self::transpose(&lc, c, h * w);
        // The MLP.
        let mut m = x.clone();
        self.layernorm(&mut m, c, &format!("{pre}.mlp.norm"), 1e-5);
        let mut m = self.linear(&m, h * w, &format!("{pre}.mlp.fc1"));
        gelu(&mut m);
        let m = self.linear(&m, h * w, &format!("{pre}.mlp.fc2"));
        x.par_iter_mut().zip(m.par_iter()).for_each(|(a, b)| *a += b);
    }

    fn patch_merge(&self, x: &[f32], c: usize, h: usize, w: usize, pre: &str, out: usize) -> (Vec<f32>, usize, usize) {
        let (mut y, h1, w1) = self.conv(x, c, h, w, &format!("{pre}.conv1"), 1, 0);
        gelu(&mut y);
        let stride = if out == 320 { 1 } else { 2 };
        let (mut y, h2, w2) = self.dwconv(&y, out, h1, w1, &format!("{pre}.conv2"), stride);
        gelu(&mut y);
        let (y, _, _) = self.conv(&y, out, h2, w2, &format!("{pre}.conv3"), 1, 0);
        (y, h2, w2)
    }

    /// The image encoder: a 3 × 1024 × 1024 normalised picture → 256 × 64 × 64.
    pub fn encode(&self, img: &[f32]) -> Vec<f32> {
        let p = "image_encoder";
        let (mut x, h, w) = self.conv(img, 3, INPUT, INPUT, &format!("{p}.patch_embed.seq.0"), 2, 1);
        gelu(&mut x);
        let (mut x, h, w) = self.conv(&x, 32, h, w, &format!("{p}.patch_embed.seq.2"), 2, 1);
        for b in 0..2 {
            let q = format!("{p}.layers.0.blocks.{b}");
            let (mut y, _, _) = self.conv(&x, 64, h, w, &format!("{q}.conv1"), 1, 0);
            gelu(&mut y);
            let (mut y, _, _) = self.dwconv(&y, 256, h, w, &format!("{q}.conv2"), 1);
            gelu(&mut y);
            let (y, _, _) = self.conv(&y, 256, h, w, &format!("{q}.conv3"), 1, 0);
            x.par_iter_mut().zip(y.par_iter()).for_each(|(a, b)| *a += b);
            gelu(&mut x);
        }
        let (mut x, mut h, mut w) = self.patch_merge(&x, 64, h, w, &format!("{p}.layers.0.downsample"), 128);
        let dims = [64, 128, 160, 320];
        let heads = [2, 4, 5, 10];
        let wss = [7, 7, 14, 7];
        let depths = [2, 2, 6, 2];
        for li in 1..4 {
            let c = dims[li];
            let mut t = Self::transpose(&x, c, h * w);
            for b in 0..depths[li] {
                self.tvit_block(&mut t, h, w, c, &format!("{p}.layers.{li}.blocks.{b}"), heads[li], wss[li]);
            }
            x = Self::transpose(&t, h * w, c);
            if li < 3 {
                let (y, h2, w2) = self.patch_merge(&x, c, h, w, &format!("{p}.layers.{li}.downsample"), dims[li + 1]);
                (x, h, w) = (y, h2, w2);
            }
        }
        let (mut x, _, _) = self.conv(&x, 320, h, w, &format!("{p}.neck.0"), 1, 0);
        self.layernorm2d(&mut x, 256, h * w, &format!("{p}.neck.1"));
        let (mut x, _, _) = self.conv(&x, 256, h, w, &format!("{p}.neck.2"), 1, 1);
        self.layernorm2d(&mut x, 256, h * w, &format!("{p}.neck.3"));
        x
    }

    fn pe(&self, xy: [f32; 2]) -> Vec<f32> {
        let g = self.t("prompt_encoder.pe_layer.positional_encoding_gaussian_matrix");
        let (x, y) = (2.0 * xy[0] - 1.0, 2.0 * xy[1] - 1.0);
        let mut out = vec![0.0f32; 256];
        for i in 0..128 {
            let v = 2.0 * std::f32::consts::PI * (x * g[i] + y * g[128 + i]);
            out[i] = v.sin();
            out[128 + i] = v.cos();
        }
        out
    }

    fn attn(&self, q: &[f32], nq: usize, k: &[f32], v: &[f32], nk: usize, pre: &str) -> Vec<f32> {
        let heads = 8;
        let qp = self.linear(q, nq, &format!("{pre}.q_proj"));
        let kp = self.linear(k, nk, &format!("{pre}.k_proj"));
        let vp = self.linear(v, nk, &format!("{pre}.v_proj"));
        let ci = qp.len() / nq;
        let hd = ci / heads;
        let scale = 1.0 / (hd as f32).sqrt();
        let mut out = vec![0.0f32; nq * ci];
        // Heads in parallel, each into its own columns.
        let parts: Vec<Vec<f32>> = (0..heads)
            .into_par_iter()
            .map(|h| {
                let mut s = vec![0.0f32; nq * nk];
                let mut o = vec![0.0f32; nq * hd];
                unsafe {
                    matrixmultiply::sgemm(nq, hd, nk, scale, qp[h * hd..].as_ptr(), ci as isize, 1, kp[h * hd..].as_ptr(), 1, ci as isize, 0.0, s.as_mut_ptr(), nk as isize, 1);
                }
                softmax_rows(&mut s, nk);
                unsafe {
                    matrixmultiply::sgemm(nq, nk, hd, 1.0, s.as_ptr(), nk as isize, 1, vp[h * hd..].as_ptr(), ci as isize, 1, 0.0, o.as_mut_ptr(), hd as isize, 1);
                }
                o
            })
            .collect();
        for (h, o) in parts.iter().enumerate() {
            for i in 0..nq {
                out[i * ci + h * hd..i * ci + (h + 1) * hd].copy_from_slice(&o[i * hd..(i + 1) * hd]);
            }
        }
        self.linear(&out, nq, &format!("{pre}.out_proj"))
    }

    /// Prompts → (four masks' logits, 256 × 256 each, and their predicted quality).
    /// `points` are in the 1024 input's pixels; `label` true for on the
    /// object; `bbox` a box round it (corners); `mask` a 256 × 256 logit map
    /// from before.
    pub fn decode(&self, emb: &[f32], points: &[([f32; 2], bool)], bbox: Option<[f32; 4]>, mask: Option<&[f32]>) -> (Vec<Vec<f32>>, Vec<f32>) {
        let d = "mask_decoder";
        let c = 256;
        let mut sparse: Vec<f32> = Vec::new();
        for (p, on) in points {
            let mut e = self.pe([(p[0] + 0.5) / INPUT as f32, (p[1] + 0.5) / INPUT as f32]);
            let pe = self.t(&format!("prompt_encoder.point_embeddings.{}.weight", *on as usize));
            e.iter_mut().zip(pe).for_each(|(a, b)| *a += b);
            sparse.extend(e);
        }
        match bbox {
            Some(b) => {
                for (k, xy) in [[b[0], b[1]], [b[2], b[3]]].iter().enumerate() {
                    let mut e = self.pe([(xy[0] + 0.5) / INPUT as f32, (xy[1] + 0.5) / INPUT as f32]);
                    let pe = self.t(&format!("prompt_encoder.point_embeddings.{}.weight", 2 + k));
                    e.iter_mut().zip(pe).for_each(|(a, b)| *a += b);
                    sparse.extend(e);
                }
            }
            None => sparse.extend_from_slice(self.t("prompt_encoder.not_a_point_embed.weight")),
        }
        let ns = sparse.len() / c;
        // The dense prompt: the mask from before, or "no mask".
        let hw = GRID * GRID;
        let dense: Vec<f32> = match mask {
            Some(m) => {
                let p = "prompt_encoder.mask_downscaling";
                let (mut x, h, w) = self.conv(m, 1, MASK, MASK, &format!("{p}.0"), 2, 0);
                self.layernorm2d(&mut x, 4, h * w, &format!("{p}.1"));
                gelu(&mut x);
                let (mut x, h, w) = self.conv(&x, 4, h, w, &format!("{p}.3"), 2, 0);
                self.layernorm2d(&mut x, 16, h * w, &format!("{p}.4"));
                gelu(&mut x);
                self.conv(&x, 16, h, w, &format!("{p}.6"), 1, 0).0
            }
            None => {
                let e = self.t("prompt_encoder.no_mask_embed.weight");
                (0..c).flat_map(|ci| std::iter::repeat_n(e[ci], hw)).collect()
            }
        };
        let mut tokens: Vec<f32> = Vec::new();
        tokens.extend_from_slice(self.t(&format!("{d}.iou_token.weight")));
        tokens.extend_from_slice(self.t(&format!("{d}.mask_tokens.weight")));
        tokens.extend(sparse);
        let nt = tokens.len() / c;
        let _ = ns;
        let src: Vec<f32> = emb.iter().zip(&dense).map(|(a, b)| a + b).collect();
        let mut keys = Self::transpose(&src, c, hw);
        // The image's positions.
        let mut kpe = vec![0.0f32; hw * c];
        for y in 0..GRID {
            for x in 0..GRID {
                let e = self.pe([(x as f32 + 0.5) / GRID as f32, (y as f32 + 0.5) / GRID as f32]);
                kpe[(y * GRID + x) * c..(y * GRID + x + 1) * c].copy_from_slice(&e);
            }
        }
        let qpe = tokens.clone();
        let mut queries = tokens;
        let add = |a: &[f32], b: &[f32]| -> Vec<f32> { a.iter().zip(b).map(|(x, y)| x + y).collect() };
        for i in 0..2 {
            let t = format!("{d}.transformer.layers.{i}");
            if i == 0 {
                queries = self.attn(&queries, nt, &queries.clone(), &queries.clone(), nt, &format!("{t}.self_attn"));
            } else {
                let qq = add(&queries, &qpe);
                let a = self.attn(&qq, nt, &qq, &queries, nt, &format!("{t}.self_attn"));
                queries = add(&queries, &a);
            }
            self.layernorm(&mut queries, c, &format!("{t}.norm1"), 1e-5);
            let qq = add(&queries, &qpe);
            let kk = add(&keys, &kpe);
            let a = self.attn(&qq, nt, &kk, &keys, hw, &format!("{t}.cross_attn_token_to_image"));
            queries = add(&queries, &a);
            self.layernorm(&mut queries, c, &format!("{t}.norm2"), 1e-5);
            let mut m = self.linear(&queries, nt, &format!("{t}.mlp.lin1"));
            m.iter_mut().for_each(|v| *v = v.max(0.0));
            let m = self.linear(&m, nt, &format!("{t}.mlp.lin2"));
            queries = add(&queries, &m);
            self.layernorm(&mut queries, c, &format!("{t}.norm3"), 1e-5);
            let qq = add(&queries, &qpe);
            let kk = add(&keys, &kpe);
            let a = self.attn(&kk, hw, &qq, &queries, nt, &format!("{t}.cross_attn_image_to_token"));
            keys = add(&keys, &a);
            self.layernorm(&mut keys, c, &format!("{t}.norm4"), 1e-5);
        }
        let qq = add(&queries, &qpe);
        let kk = add(&keys, &kpe);
        let a = self.attn(&qq, nt, &kk, &keys, hw, &format!("{d}.transformer.final_attn_token_to_image"));
        queries = add(&queries, &a);
        self.layernorm(&mut queries, c, &format!("{d}.transformer.norm_final_attn"), 1e-5);
        // Upscale the image side 4×.
        let srcm = Self::transpose(&keys, hw, c);
        let conv_t = |x: &[f32], cin: usize, h: usize, w: usize, pre: &str| -> (Vec<f32>, usize) {
            let wt = self.t(&format!("{pre}.weight"));
            let cout = self.shape(&format!("{pre}.weight"))[1];
            let b = self.t(&format!("{pre}.bias"));
            let mut out = vec![0.0f32; cout * 4 * h * w];
            for i in 0..2 {
                for j in 0..2 {
                    // (cout × cin) for this tap.
                    let tap: Vec<f32> = (0..cout).flat_map(|o| (0..cin).map(move |ci| (o, ci))).map(|(o, ci)| wt[((ci * cout + o) * 2 + i) * 2 + j]).collect();
                    let mut r = vec![0.0f32; cout * h * w];
                    gemm(cout, cin, h * w, &tap, x, false, &mut r);
                    for o in 0..cout {
                        for y in 0..h {
                            for xx in 0..w {
                                out[(o * 2 * h + 2 * y + i) * 2 * w + 2 * xx + j] = r[(o * h + y) * w + xx] + b[o];
                            }
                        }
                    }
                }
            }
            (out, cout)
        };
        let (mut u, c1) = conv_t(&srcm, c, GRID, GRID, &format!("{d}.output_upscaling.0"));
        self.layernorm2d(&mut u, c1, 4 * hw, &format!("{d}.output_upscaling.1"));
        gelu(&mut u);
        let (mut u, c2) = conv_t(&u, c1, 2 * GRID, 2 * GRID, &format!("{d}.output_upscaling.3"));
        gelu(&mut u);
        let mlp = |x: &[f32], pre: &str| -> Vec<f32> {
            let mut x = x.to_vec();
            for l in 0..3 {
                x = self.linear(&x, 1, &format!("{pre}.layers.{l}"));
                if l < 2 {
                    x.iter_mut().for_each(|v| *v = v.max(0.0));
                }
            }
            x
        };
        let px = MASK * MASK;
        let masks: Vec<Vec<f32>> = (0..4)
            .map(|i| {
                let hyper = mlp(&queries[(1 + i) * c..(2 + i) * c], &format!("{d}.output_hypernetworks_mlps.{i}"));
                let mut m = vec![0.0f32; px];
                gemm(1, c2, px, &hyper, &u, false, &mut m);
                m
            })
            .collect();
        let iou = mlp(&queries[..c], &format!("{d}.iou_prediction_head"));
        (masks, iou)
    }
}

/// A picture (RGB, `w` × `h`) made ready for the encoder: its longer side
/// scaled to 1024 (filtered when shrinking), normalised, padded. Returns it
/// and the scale.
pub fn prepare(rgb: &[u8], w: usize, h: usize) -> (Vec<f32>, f32) {
    let s = INPUT as f32 / w.max(h) as f32;
    let (nw, nh) = (((w as f32 * s) + 0.5) as usize, ((h as f32 * s) + 0.5) as usize);
    let mean = [123.675f32, 116.28, 103.53];
    let std = [58.395f32, 57.12, 57.375];
    // Separable triangle filter, widened when shrinking (as PIL's bilinear).
    let resample = |src: &[f32], sw: usize, sh: usize, dw: usize, horizontal: bool| -> Vec<f32> {
        let (n_in, n_out, other) = if horizontal { (sw, dw, sh) } else { (sh, dw, sw) };
        let scale = n_in as f32 / n_out as f32;
        let support = scale.max(1.0);
        let mut out = vec![0.0f32; if horizontal { dw * sh * 3 } else { sw * dw * 3 }];
        for o in 0..n_out {
            let centre = (o as f32 + 0.5) * scale;
            let lo = ((centre - support).floor().max(0.0)) as usize;
            let hi = ((centre + support).ceil() as usize).min(n_in);
            let ws: Vec<(usize, f32)> = (lo..hi).map(|i| (i, (1.0 - ((i as f32 + 0.5 - centre) / support).abs()).max(0.0))).filter(|(_, w)| *w > 0.0).collect();
            let total: f32 = ws.iter().map(|(_, w)| w).sum::<f32>().max(1e-6);
            for t in 0..other {
                for ch in 0..3 {
                    let mut acc = 0.0;
                    for &(i, wgt) in &ws {
                        let idx = if horizontal { (t * sw + i) * 3 + ch } else { (i * sw + t) * 3 + ch };
                        acc += src[idx] * wgt;
                    }
                    let oi = if horizontal { (t * dw + o) * 3 + ch } else { (o * sw + t) * 3 + ch };
                    out[oi] = acc / total;
                }
            }
        }
        out
    };
    let f: Vec<f32> = rgb.iter().map(|&v| v as f32).collect();
    let a = resample(&f, w, h, nw, true);
    let b = resample(&a, nw, h, nh, false);
    let mut out = vec![0.0f32; 3 * INPUT * INPUT];
    for y in 0..nh {
        for x in 0..nw {
            for ch in 0..3 {
                out[(ch * INPUT + y) * INPUT + x] = (b[(y * nw + x) * 3 + ch].round() - mean[ch]) / std[ch];
            }
        }
    }
    (out, s)
}

/// A 256 × 256 logit map → a `w` × `h` picture's (bilinear, as the model's postprocessing).
pub fn upscale(m: &[f32], w: usize, h: usize, scale: f32) -> Vec<f32> {
    let mut out = vec![0.0f32; w * h];
    let k = MASK as f32 / INPUT as f32;
    out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, o) in row.iter_mut().enumerate() {
            // Picture pixel → input pixel → mask grid.
            let ix = (x as f32 + 0.5) * scale;
            let iy = (y as f32 + 0.5) * scale;
            let gx = (ix * k - 0.5).clamp(0.0, (MASK - 1) as f32);
            let gy = (iy * k - 0.5).clamp(0.0, (MASK - 1) as f32);
            let (x0, y0) = (gx.floor() as usize, gy.floor() as usize);
            let (x1, y1) = ((x0 + 1).min(MASK - 1), (y0 + 1).min(MASK - 1));
            let (fx, fy) = (gx - x0 as f32, gy - y0 as f32);
            let g = |x: usize, y: usize| m[y * MASK + x];
            *o = (g(x0, y0) * (1.0 - fx) + g(x1, y0) * fx) * (1.0 - fy) + (g(x0, y1) * (1.0 - fx) + g(x1, y1) * fx) * fy;
        }
    });
    out
}

/// The model, from the weights built into the app.
pub fn model() -> Option<&'static Sam> {
    static M: std::sync::OnceLock<Option<Sam>> = std::sync::OnceLock::new();
    M.get_or_init(|| Sam::load(include_bytes!("../mobilesam.bin"))).as_ref()
}

/// A picture's encoding, kept to cut from as often as needed.
pub struct Embedding {
    pub data: Vec<f32>,
    pub w: usize,
    pub h: usize,
    /// Picture pixels → the encoder's.
    pub scale: f32,
}

/// The image encoder on the graphics card, if there's a usable one (and
/// `REELFORGE_SAM_CPU` isn't set).
pub fn gpu() -> Option<&'static GpuEncoder> {
    static G: std::sync::OnceLock<Option<GpuEncoder>> = std::sync::OnceLock::new();
    G.get_or_init(|| if std::env::var_os("REELFORGE_SAM_CPU").is_some() { None } else { GpuEncoder::new(model()?) }).as_ref()
}

/// Encode a picture (RGBA, `w` × `h`): on the graphics card when there is
/// one, otherwise (or if that fails) on the CPU.
pub fn embed(rgba: &[u8], w: usize, h: usize) -> Option<Embedding> {
    let sam = model()?;
    if w == 0 || h == 0 || rgba.len() < w * h * 4 {
        return None;
    }
    let rgb: Vec<u8> = rgba.chunks_exact(4).take(w * h).flat_map(|c| [c[0], c[1], c[2]]).collect();
    let (img, scale) = prepare(&rgb, w, h);
    let data = gpu().and_then(|g| g.encode(&img)).unwrap_or_else(|| sam.encode(&img));
    Some(Embedding { data, w, h, scale })
}

/// What to cut out, in picture pixels.
#[derive(Default)]
pub struct Prompt<'a> {
    /// Points on the object (`true`) or off it.
    pub points: &'a [([f32; 2], bool)],
    /// A box round it: left, top, right, bottom.
    pub bbox: Option<[f32; 4]>,
    /// A low-resolution logit map from an earlier cut (256 × 256).
    pub mask: Option<&'a [f32]>,
}

/// The cut: logits over the picture (above 0: the object), the
/// low-resolution logits (to prompt the next cut with), and how good the
/// model thinks it is (0–1).
pub struct Cut {
    pub logits: Vec<f32>,
    pub low: Vec<f32>,
    pub quality: f32,
}

pub fn cut(e: &Embedding, p: &Prompt) -> Option<Cut> {
    let sam = model()?;
    let s = e.scale;
    let points: Vec<([f32; 2], bool)> = p.points.iter().map(|(q, on)| ([q[0] * s, q[1] * s], *on)).collect();
    let bbox = p.bbox.map(|b| [b[0] * s, b[1] * s, b[2] * s, b[3] * s]);
    if points.is_empty() && bbox.is_none() {
        return None;
    }
    let (mut masks, iou) = sam.decode(&e.data, &points, bbox, p.mask);
    // One point is ambiguous (a part, the whole, …): the model offers three
    // and says which it thinks best; with more to go on, its single answer.
    let best = if points.len() == 1 && bbox.is_none() && p.mask.is_none() { (1..4).max_by(|&a, &b| iou[a].total_cmp(&iou[b])).unwrap() } else { 0 };
    let low = masks.swap_remove(best);
    Some(Cut { logits: upscale(&low, e.w, e.h, s), low, quality: iou[best].clamp(0.0, 1.0) })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A red disc on a mottled grey ground: a click on it cuts out the disc,
    /// and a box round it does too.
    #[test]
    fn a_click_picks_out_the_object_under_it() {
        let (w, h) = (320usize, 200usize);
        let mut rgba = vec![0u8; w * h * 4];
        let mut seed = 7u32;
        for y in 0..h {
            for x in 0..w {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                let n = (seed >> 24) as i32 / 8;
                let g = (110 + n + ((x / 23 + y / 17) % 2) as i32 * 20) as u8;
                let d = ((x as f32 - 200.0).powi(2) + (y as f32 - 90.0).powi(2)).sqrt();
                let p = if d < 40.0 { [210, 40, 30] } else { [g, g, g] };
                rgba[(y * w + x) * 4..(y * w + x) * 4 + 4].copy_from_slice(&[p[0], p[1], p[2], 255]);
            }
        }
        let e = embed(&rgba, w, h).expect("the model loads");
        let bounds = |c: &Cut| {
            let on: Vec<usize> = (0..w * h).filter(|&i| c.logits[i] > 0.0).collect();
            let xs = on.iter().map(|i| i % w);
            let ys = on.iter().map(|i| i / w);
            (xs.clone().min().unwrap(), xs.max().unwrap(), ys.clone().min().unwrap(), ys.max().unwrap(), on.len())
        };
        let click = cut(&e, &Prompt { points: &[([200.0, 90.0], true)], ..Default::default() }).unwrap();
        let (x0, x1, y0, y1, n) = bounds(&click);
        assert!(x0.abs_diff(160) <= 3 && x1.abs_diff(239) <= 3 && y0.abs_diff(50) <= 3 && y1.abs_diff(129) <= 3, "{x0}..{x1} × {y0}..{y1}");
        let disc = std::f32::consts::PI * 40.0 * 40.0;
        assert!((n as f32 - disc).abs() < disc * 0.06, "{n} vs {disc}");
        let boxed = cut(&e, &Prompt { bbox: Some([150.0, 40.0, 250.0, 140.0]), ..Default::default() }).unwrap();
        let (bx0, bx1, ..) = bounds(&boxed);
        assert!(bx0.abs_diff(160) <= 3 && bx1.abs_diff(239) <= 3, "{bx0}..{bx1}");
        // A point off it, with a box over the ground only, picks out no disc.
        let off = cut(&e, &Prompt { points: &[([200.0, 90.0], false), ([40.0, 160.0], true)], ..Default::default() }).unwrap();
        assert!(off.logits[90 * w + 200] < 0.0);
    }

    /// The graphics card's encoder gives what the CPU's does.
    #[test]
    fn the_graphics_card_encodes_as_the_cpu_does() {
        let sam = model().unwrap();
        let Some(g) = GpuEncoder::open(sam, true) else {
            eprintln!("no graphics adapter: skipped");
            return;
        };
        let (w, h) = (96usize, 64usize);
        let rgb: Vec<u8> = (0..w * h * 3).map(|i| ((i * 37 + (i / 97) * 11) % 251) as u8).collect();
        let (img, _) = prepare(&rgb, w, h);
        let t = std::time::Instant::now();
        let a = sam.encode(&img);
        let cpu = t.elapsed();
        let t = std::time::Instant::now();
        let b = g.encode(&img).expect("encoded");
        eprintln!("cpu {cpu:?}, {} {:?}", g.adapter, t.elapsed());
        let diff = a.iter().zip(&b).map(|(x, y)| (x - y).abs()).fold(0.0f32, f32::max);
        let mean = a.iter().zip(&b).map(|(x, y)| (x - y).abs()).sum::<f32>() / a.len() as f32;
        assert!(diff < 0.02 && mean < 1e-3, "max {diff}, mean {mean}");
    }
}
