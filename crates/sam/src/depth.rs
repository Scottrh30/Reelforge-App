//! Depth from a single picture: Depth Anything V2 Small (Yang et al. 2024,
//! Apache-2.0) on the CPU, for the Depth map and Relight effects.
//!
//! The network is a DINOv2 ViT-S/14 encoder (12 blocks, 384 wide, 6 heads)
//! read at four depths by a DPT head, which puts them back together into a
//! map as big as its input. The picture is squeezed to 518 × 518 (37 × 37
//! patches), and the map comes back the same size: relative inverse depth,
//! bigger nearer, 0 at the far distance (sky). Its weights ship with the app
//! (`depth.bin`: the big tensors int8 with a scale per output channel, the
//! rest f16; about 25 MB). On a graphics card (the same compute kernels as the
//! roto brush's, `gpu.rs`) a frame takes a fraction of a second; on the CPU, a
//! second or more.

use super::*;

/// The network's input (and output) size.
pub const SIDE: usize = 518;
/// Patches across.
const P: usize = 37;
const DIM: usize = 384;
const HEADS: usize = 6;
const HEAD: usize = 64;
/// Most samples a convolution's unrolled patches may hold at once (the big
/// maps are done a band of rows at a time).
const COLS: usize = 1 << 22;

/// The network's weights (held as a [`Sam`]'s, for its kernels).
pub struct DepthModel(Sam);

/// The model, from the weights built into the app.
pub fn model() -> Option<&'static DepthModel> {
    static M: std::sync::OnceLock<Option<DepthModel>> = std::sync::OnceLock::new();
    M.get_or_init(|| load_weights(include_bytes!("../depth.bin")).map(|w| DepthModel(Sam { w, biases: HashMap::new() }))).as_ref()
}

/// The depth of a picture (RGBA, `w` × `h`): [`SIDE`] × [`SIDE`] values,
/// row by row, bigger nearer (`None` if it couldn't be worked out). On the
/// graphics card when there's one that can, else on the CPU.
pub fn depth(rgba: &[u8], w: usize, h: usize) -> Option<Vec<f32>> {
    guard(|| {
        let m = model()?;
        if w == 0 || h == 0 || rgba.len() < w.checked_mul(h)?.checked_mul(4)? {
            return None;
        }
        let img = prepare_depth(rgba, w, h);
        if let Some(d) = gpu().and_then(|g| guard(|| g.depth(&img))) {
            return Some(d);
        }
        Some(m.run(&img))
    })
}

/// The depth model on the graphics card (`None` without one that can run it, or
/// with REELFORGE_DEPTH_CPU set).
pub fn gpu() -> Option<&'static crate::GpuEncoder> {
    static G: std::sync::OnceLock<Option<crate::GpuEncoder>> = std::sync::OnceLock::new();
    G.get_or_init(|| if std::env::var_os("REELFORGE_DEPTH_CPU").is_some() { None } else { crate::GpuEncoder::open_depth(model()?, false) }).as_ref()
}

impl DepthModel {
    /// Its weights (for the graphics card's copy).
    pub(crate) fn weights(&self) -> &Sam {
        &self.0
    }
}

/// The picture squeezed to the input and normalised as the network was
/// trained (ImageNet's mean and spread): 3 × SIDE × SIDE.
pub fn prepare_depth(rgba: &[u8], w: usize, h: usize) -> Vec<f32> {
    const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
    const STD: [f32; 3] = [0.229, 0.224, 0.225];
    let mut out = vec![0.0f32; 3 * SIDE * SIDE];
    let px = |x: usize, y: usize, c: usize| rgba.get((y.min(h - 1) * w + x.min(w - 1)) * 4 + c).copied().unwrap_or(0) as f32 / 255.0;
    for y in 0..SIDE {
        // Bilinear, pixel centres lined up.
        let sy = ((y as f32 + 0.5) * h as f32 / SIDE as f32 - 0.5).max(0.0);
        let (y0, fy) = (sy.floor() as usize, sy - sy.floor());
        for x in 0..SIDE {
            let sx = ((x as f32 + 0.5) * w as f32 / SIDE as f32 - 0.5).max(0.0);
            let (x0, fx) = (sx.floor() as usize, sx - sx.floor());
            for c in 0..3 {
                let top = px(x0, y0, c) * (1.0 - fx) + px(x0 + 1, y0, c) * fx;
                let bottom = px(x0, y0 + 1, c) * (1.0 - fx) + px(x0 + 1, y0 + 1, c) * fx;
                let v = top * (1.0 - fy) + bottom * fy;
                out[(c * SIDE + y) * SIDE + x] = (v - MEAN[c]) / STD[c];
            }
        }
    }
    out
}

/// x ← max(x, 0).
fn relu(x: &mut [f32]) {
    x.par_chunks_mut(8192).for_each(|c| c.iter_mut().for_each(|v| *v = v.max(0.0)));
}

/// Bilinear resize of a C × H × W map to `ho` × `wo`, corners lined up
/// (PyTorch's align_corners).
fn resize(x: &[f32], c: usize, h: usize, w: usize, ho: usize, wo: usize) -> Vec<f32> {
    let map = |o: usize, n_out: usize, n_in: usize| -> (usize, usize, f32) {
        let s = if n_out > 1 { o as f32 * (n_in - 1) as f32 / (n_out - 1) as f32 } else { 0.0 };
        let i0 = (s.floor() as usize).min(n_in - 1);
        (i0, (i0 + 1).min(n_in - 1), s - i0 as f32)
    };
    let ys: Vec<_> = (0..ho).map(|y| map(y, ho, h)).collect();
    let xs: Vec<_> = (0..wo).map(|x| map(x, wo, w)).collect();
    let mut out = vec![0.0f32; c * ho * wo];
    out.par_chunks_mut(ho * wo).enumerate().for_each(|(ci, dst)| {
        let src = &x[ci * h * w..(ci + 1) * h * w];
        for (yo, &(y0, y1, fy)) in ys.iter().enumerate() {
            for (xo, &(x0, x1, fx)) in xs.iter().enumerate() {
                let top = src[y0 * w + x0] * (1.0 - fx) + src[y0 * w + x1] * fx;
                let bottom = src[y1 * w + x0] * (1.0 - fx) + src[y1 * w + x1] * fx;
                dst[yo * wo + xo] = top * (1.0 - fy) + bottom * fy;
            }
        }
    });
    out
}

impl DepthModel {
    fn t(&self, k: &str) -> &[f32] {
        self.0.t(k)
    }

    /// A convolution (stride 1 or 2, square kernel) a band of output rows at a time.
    fn conv(&self, x: &[f32], c: usize, h: usize, w: usize, pre: &str, stride: usize, pad: usize) -> (Vec<f32>, usize, usize) {
        let wt = self.t(&format!("{pre}.weight"));
        let s = self.0.shape(&format!("{pre}.weight")).to_vec();
        let (o, kh, kw) = (s[0], s[2], s[3]);
        let ho = (h + 2 * pad - kh) / stride + 1;
        let wo = (w + 2 * pad - kw) / stride + 1;
        let k = c * kh * kw;
        let mut out = vec![0.0f32; o * ho * wo];
        if kh == 1 && kw == 1 && stride == 1 && pad == 0 {
            gemm(o, k, ho * wo, wt, x, false, &mut out);
        } else {
            let band = (COLS / (k * wo).max(1)).clamp(1, ho);
            let mut cols = vec![0.0f32; k * band * wo];
            let mut part = vec![0.0f32; o * band * wo];
            let mut y0 = 0;
            while y0 < ho {
                let rows = band.min(ho - y0);
                let n = rows * wo;
                cols[..k * n].par_chunks_mut(n).enumerate().for_each(|(row, dst)| {
                    let (ci, ki, kj) = (row / (kh * kw), (row / kw) % kh, row % kw);
                    for r in 0..rows {
                        let yi = ((y0 + r) * stride + ki) as isize - pad as isize;
                        for xo in 0..wo {
                            let xi = (xo * stride + kj) as isize - pad as isize;
                            dst[r * wo + xo] = if yi >= 0 && xi >= 0 && (yi as usize) < h && (xi as usize) < w { x[(ci * h + yi as usize) * w + xi as usize] } else { 0.0 };
                        }
                    }
                });
                gemm(o, k, n, wt, &cols[..k * n], false, &mut part[..o * n]);
                for oc in 0..o {
                    out[oc * ho * wo + y0 * wo..oc * ho * wo + (y0 + rows) * wo].copy_from_slice(&part[oc * n..(oc + 1) * n]);
                }
                y0 += rows;
            }
        }
        if let Some((_, b)) = self.0.w.get(&format!("{pre}.bias")) {
            out.par_chunks_mut(ho * wo).zip(b.par_iter()).for_each(|(row, b)| row.iter_mut().for_each(|v| *v += b));
        }
        (out, ho, wo)
    }

    /// A transposed convolution whose kernel is its stride (each input pixel
    /// spread over a `k` × `k` block). Its weight is Cin × Cout × k × k.
    fn conv_t(&self, x: &[f32], c: usize, h: usize, w: usize, pre: &str) -> (Vec<f32>, usize, usize) {
        let wt = self.t(&format!("{pre}.weight"));
        let s = self.0.shape(&format!("{pre}.weight")).to_vec();
        let (co, k) = (s[1], s[2]);
        let m = co * k * k;
        // (Cout·k·k) × (H·W): the weight read transposed.
        let mut blocks = vec![0.0f32; m * h * w];
        gemm_strided(m, c, h * w, wt, 1, m as isize, x, (h * w) as isize, 1, &mut blocks, (h * w) as isize, 0.0);
        let b = self.t(&format!("{pre}.bias"));
        let (ho, wo) = (h * k, w * k);
        let mut out = vec![0.0f32; co * ho * wo];
        out.par_chunks_mut(ho * wo).enumerate().for_each(|(o, dst)| {
            for a in 0..k {
                for bb in 0..k {
                    let src = &blocks[((o * k + a) * k + bb) * h * w..][..h * w];
                    for y in 0..h {
                        for xx in 0..w {
                            dst[(y * k + a) * wo + xx * k + bb] = src[y * w + xx] + b[o];
                        }
                    }
                }
            }
        });
        (out, ho, wo)
    }

    /// The residual unit: x + conv2(relu(conv1(relu(x)))).
    fn rcu(&self, x: &[f32], h: usize, w: usize, pre: &str) -> Vec<f32> {
        let mut o = x.to_vec();
        relu(&mut o);
        let (mut o, ..) = self.conv(&o, 64, h, w, &format!("{pre}.conv1"), 1, 1);
        relu(&mut o);
        let (mut o, ..) = self.conv(&o, 64, h, w, &format!("{pre}.conv2"), 1, 1);
        o.iter_mut().zip(x).for_each(|(a, b)| *a += b);
        o
    }

    /// A fusion block: (x + rcu1(skip)) through rcu2, resized to `size` (or
    /// twice as big), then a 1 × 1 convolution.
    fn fusion(&self, i: usize, x: &[f32], skip: Option<&[f32]>, h: usize, w: usize, size: Option<(usize, usize)>) -> (Vec<f32>, usize, usize) {
        let pre = format!("depth_head.scratch.refinenet{i}");
        let mut o = x.to_vec();
        if let Some(s) = skip {
            let r = self.rcu(s, h, w, &format!("{pre}.resConfUnit1"));
            o.iter_mut().zip(&r).for_each(|(a, b)| *a += b);
        }
        let o = self.rcu(&o, h, w, &format!("{pre}.resConfUnit2"));
        let (ho, wo) = size.unwrap_or((h * 2, w * 2));
        let o = resize(&o, 64, h, w, ho, wo);
        self.conv(&o, 64, ho, wo, &format!("{pre}.out_conv"), 1, 0)
    }

    /// Multi-head self-attention of `x` (n × 384, after its norm): the heads'
    /// outputs side by side (n × 384), before the projection.
    fn attention(&self, x: &[f32], n: usize, pre: &str) -> Vec<f32> {
        let qkv = self.0.linear(x, n, &format!("{pre}.qkv"));
        let scale = 1.0 / (HEAD as f32).sqrt();
        const ROWS: usize = 128;
        let chunks = n.div_ceil(ROWS);
        let parts: Vec<(usize, usize, Vec<f32>)> = (0..HEADS * chunks)
            .into_par_iter()
            .map(|job| {
                let (hd, r0) = (job / chunks, (job % chunks) * ROWS);
                let rows = ROWS.min(n - r0);
                let stride = (3 * DIM) as isize;
                let mut s = vec![0.0f32; rows * n];
                // q (rows × 64) · kᵀ (64 × n)
                gemm_strided(rows, HEAD, n, &qkv[r0 * 3 * DIM + hd * HEAD..], stride, 1, &qkv[DIM + hd * HEAD..], 1, stride, &mut s, n as isize, 0.0);
                s.iter_mut().for_each(|v| *v *= scale);
                softmax_rows(&mut s, n);
                let mut o = vec![0.0f32; rows * HEAD];
                gemm_strided(rows, n, HEAD, &s, n as isize, 1, &qkv[2 * DIM + hd * HEAD..], stride, 1, &mut o, HEAD as isize, 0.0);
                (hd, r0, o)
            })
            .collect();
        let mut out = vec![0.0f32; n * DIM];
        for (hd, r0, o) in parts {
            for (r, row) in o.chunks_exact(HEAD).enumerate() {
                out[(r0 + r) * DIM + hd * HEAD..][..HEAD].copy_from_slice(row);
            }
        }
        out
    }

    /// The network: a normalised 3 × SIDE × SIDE picture in, SIDE × SIDE depth out.
    pub fn run(&self, img: &[f32]) -> Vec<f32> {
        let (patches, ..) = self.conv(img, 3, SIDE, SIDE, "pretrained.patch_embed.proj", 14, 0);
        let n = P * P + 1;
        let pos = self.t("pretrained.pos_embed");
        let cls = self.t("pretrained.cls_token");
        let mut x = vec![0.0f32; n * DIM];
        for d in 0..DIM {
            x[d] = cls[d] + pos[d];
            for p in 0..P * P {
                x[(p + 1) * DIM + d] = patches[d * P * P + p] + pos[(p + 1) * DIM + d];
            }
        }
        let mut feats: Vec<Vec<f32>> = Vec::with_capacity(4);
        for k in 0..12 {
            let pre = format!("pretrained.blocks.{k}");
            let mut h = x.clone();
            self.0.layernorm(&mut h, DIM, &format!("{pre}.norm1"), 1e-6);
            let a = self.attention(&h, n, &format!("{pre}.attn"));
            let a = self.0.linear(&a, n, &format!("{pre}.attn.proj"));
            let g = self.t(&format!("{pre}.ls1.gamma"));
            x.par_chunks_mut(DIM).zip(a.par_chunks(DIM)).for_each(|(xr, ar)| xr.iter_mut().zip(ar).zip(g).for_each(|((x, a), g)| *x += a * g));
            let mut h = x.clone();
            self.0.layernorm(&mut h, DIM, &format!("{pre}.norm2"), 1e-6);
            let mut h = self.0.linear(&h, n, &format!("{pre}.mlp.fc1"));
            gelu(&mut h);
            let h = self.0.linear(&h, n, &format!("{pre}.mlp.fc2"));
            let g = self.t(&format!("{pre}.ls2.gamma"));
            x.par_chunks_mut(DIM).zip(h.par_chunks(DIM)).for_each(|(xr, hr)| xr.iter_mut().zip(hr).zip(g).for_each(|((x, h), g)| *x += h * g));
            if matches!(k, 2 | 5 | 8 | 11) {
                let mut f = x.clone();
                self.0.layernorm(&mut f, DIM, "pretrained.norm", 1e-6);
                // Patches only, as a 384 × 37 × 37 map.
                let mut map = vec![0.0f32; DIM * P * P];
                for p in 0..P * P {
                    for d in 0..DIM {
                        map[d * P * P + p] = f[(p + 1) * DIM + d];
                    }
                }
                feats.push(map);
            }
        }
        // The DPT head: each depth's features to its own scale.
        let mut layers = Vec::with_capacity(4);
        for (i, f) in feats.iter().enumerate() {
            let (t, ..) = self.conv(f, DIM, P, P, &format!("depth_head.projects.{i}"), 1, 0);
            let c = self.0.shape(&format!("depth_head.projects.{i}.weight"))[0];
            let (t, h, w) = match i {
                0 | 1 => self.conv_t(&t, c, P, P, &format!("depth_head.resize_layers.{i}")),
                3 => self.conv(&t, c, P, P, "depth_head.resize_layers.3", 2, 1),
                _ => (t, P, P),
            };
            let (t, h, w) = self.conv(&t, c, h, w, &format!("depth_head.scratch.layer{}_rn", i + 1), 1, 1);
            layers.push((t, h, w));
        }
        let [l1, l2, l3, l4] = <[(Vec<f32>, usize, usize); 4]>::try_from(layers).unwrap_or_else(|_| unreachable_layers());
        let p4 = self.fusion(4, &l4.0, None, l4.1, l4.2, Some((l3.1, l3.2)));
        let p3 = self.fusion(3, &p4.0, Some(&l3.0), l3.1, l3.2, Some((l2.1, l2.2)));
        let p2 = self.fusion(2, &p3.0, Some(&l2.0), l2.1, l2.2, Some((l1.1, l1.2)));
        let (p1, h, w) = self.fusion(1, &p2.0, Some(&l1.0), l1.1, l1.2, None);
        let (o, h, w) = self.conv(&p1, 64, h, w, "depth_head.scratch.output_conv1", 1, 1);
        let o = resize(&o, 32, h, w, SIDE, SIDE);
        let (mut o, ..) = self.conv(&o, 32, SIDE, SIDE, "depth_head.scratch.output_conv2.0", 1, 1);
        relu(&mut o);
        let (mut o, ..) = self.conv(&o, 32, SIDE, SIDE, "depth_head.scratch.output_conv2.2", 1, 0);
        relu(&mut o);
        o
    }
}

/// (Four features always come out of the encoder; if not, the run is
/// abandoned through `guard`.)
#[allow(clippy::panic)]
fn unreachable_layers() -> [(Vec<f32>, usize, usize); 4] {
    panic!("depth: the encoder gave the wrong number of features")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The scene `ref.py` makes: sky over a tiled floor going away, a box
    /// standing on it near the camera.
    fn scene() -> Vec<u8> {
        let mut px = Vec::with_capacity(SIDE * SIDE * 4);
        for y in 0..SIDE {
            for x in 0..SIDE {
                let (yy, xx) = (y as f64 / (SIDE - 1) as f64, x as f64 / (SIDE - 1) as f64);
                let sky = yy < 0.45;
                let mut c = if sky { [0.55 + 0.2 * yy, 0.7 + 0.1 * yy, 0.95] } else { [0.35 + 0.3 * yy, 0.3 + 0.25 * yy, 0.25 + 0.1 * yy] };
                let z = 1.0 / (yy - 0.45).max(1e-3);
                if !sky && ((z * 0.5).floor() + ((xx - 0.5) * z * 0.6).floor()).rem_euclid(2.0) == 0.0 {
                    c = c.map(|v| v * 0.6);
                }
                if xx > 0.3 && xx < 0.6 && yy > 0.55 && yy < 0.9 {
                    c = if xx > 0.52 { [0.55, 0.25, 0.12] } else { [0.8, 0.4, 0.2] };
                }
                px.extend(c.map(|v| (v * 255.0).round().clamp(0.0, 255.0) as u8));
                px.push(255);
            }
        }
        px
    }

    /// The reference network's depth (onnxruntime, float weights), as means of
    /// 14 × 14 blocks of 37 × 37.
    const REFERENCE: [f32; 196] = [
        0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.459, 0.458, 0.456, 0.452, 0.451, 0.466, 0.466, 0.459, 0.457, 0.446, 0.444, 0.45, 0.459, 0.457, 1.757, 1.771, 1.772, 1.773, 2.447, 2.744, 2.796, 2.792, 2.154, 1.778, 1.766, 1.768, 1.772, 1.764, 3.003, 3.004, 3.009, 3.022, 5.405, 6.094, 6.146, 6.186, 4.253, 3.031, 3.021, 3.011, 3.003, 2.999, 4.154, 4.154, 4.161, 4.169, 5.893, 6.375, 6.438, 6.489, 5.066, 4.169, 4.183, 4.197, 4.166, 4.129, 5.278, 5.256, 5.224, 5.219, 6.297, 6.604, 6.658, 6.696, 5.792, 5.218, 5.239, 5.271, 5.277, 5.259, 6.216, 6.17, 6.125, 6.103, 6.607, 6.771, 6.805, 6.839, 6.372, 6.087, 6.129, 6.172, 6.197, 6.202, 6.956, 6.926, 6.889, 6.857, 6.935, 6.958, 6.951, 6.962, 6.866, 6.825, 6.872, 6.922, 6.948, 6.944, 7.788, 7.757, 7.733, 7.717, 7.697, 7.684, 7.675, 7.675, 7.692, 7.691, 7.706, 7.741, 7.771, 7.78,
    ];

    #[test]
    fn depth_matches_the_reference_network() {
        let d = depth(&scene(), SIDE, SIDE).expect("depth");
        assert_eq!(d.len(), SIDE * SIDE);
        let mut worst = 0.0f32;
        for by in 0..14 {
            for bx in 0..14 {
                let mut s = 0.0;
                for y in by * 37..by * 37 + 37 {
                    for x in bx * 37..bx * 37 + 37 {
                        s += d[y * SIDE + x];
                    }
                }
                let m = s / (37.0 * 37.0);
                worst = worst.max((m - REFERENCE[by * 14 + bx]).abs());
            }
        }
        // (int8 weights: within a few per cent of the range)
        assert!(worst < 0.3, "worst block off by {worst}");
    }

    /// The graphics card's run (a software one will do here) matches the reference
    /// as the CPU's does, and the CPU's own result closely.
    #[test]
    fn depth_on_the_graphics_card_matches() {
        let Some(g) = crate::GpuEncoder::open_depth(model().expect("model"), true) else {
            eprintln!("no graphics device; skipping");
            return;
        };
        let img = prepare_depth(&scene(), SIDE, SIDE);
        let d = g.depth(&img).expect("depth on the graphics card");
        assert_eq!(d.len(), SIDE * SIDE);
        let cpu = model().unwrap().run(&img);
        let worst = d.iter().zip(&cpu).map(|(a, b)| (a - b).abs()).fold(0.0f32, f32::max);
        assert!(worst < 0.05, "GPU and CPU differ by up to {worst}");
        let mut off = 0.0f32;
        for by in 0..14 {
            for bx in 0..14 {
                let mut s = 0.0;
                for y in by * 37..by * 37 + 37 {
                    for x in bx * 37..bx * 37 + 37 {
                        s += d[y * SIDE + x];
                    }
                }
                off = off.max((s / (37.0 * 37.0) - REFERENCE[by * 14 + bx]).abs());
            }
        }
        assert!(off < 0.3, "worst block off by {off}");
        // The wrong input size is refused.
        assert!(g.depth(&[0.0; 12]).is_none());
    }

    #[test]
    fn odd_pictures_are_refused_not_crashed_on() {
        assert!(depth(&[], 0, 0).is_none());
        assert!(depth(&[1, 2, 3], 10, 10).is_none());
        assert!(depth(&[0; 4], 1, 1).is_some_and(|d| d.len() == SIDE * SIDE));
    }
}
