//! Decoded pictures as they come out of the video decoder.
//!
//! Most video is stored as 4:2:0 YUV: full-resolution brightness (Y) and
//! half-resolution colour (U and V). Frames are kept that way, as the
//! decoder (or the graphics card's decoder) produced them, and turned into RGB
//! on the GPU as they're drawn. Converting on the CPU used to be the most
//! expensive step of playback after decoding itself, and the YUV planes are
//! less than half the size of RGBA, so there's less to copy, cache and upload.
//!
//! [`YuvPlanes::to_rgba`] does the same conversion on the CPU, for the few
//! things that need RGB in memory (thumbnails, proxies) and as the reference
//! the GPU shader is tested against.

use std::sync::Arc;

/// How finely colour (U and V) is sampled against brightness (Y).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Chroma {
    /// Half width, half height (most video).
    C420,
    /// Half width, full height (ProRes 422, DNxHR, XAVC Intra).
    C422,
    /// Full resolution (ProRes 4444, DNxHR 444).
    C444,
}

impl Chroma {
    /// log2 of the (horizontal, vertical) subsampling.
    pub fn shift(self) -> (u32, u32) {
        match self {
            Chroma::C420 => (1, 1),
            Chroma::C422 => (1, 0),
            Chroma::C444 => (0, 0),
        }
    }
}

/// How the planes are laid out.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum YuvLayout {
    /// 8-bit Y plane and interleaved 4:2:0 UV (`nv12`; hardware decoders).
    Nv12,
    /// 10-bit Y and interleaved 4:2:0 UV, value in the high bits of each word (`p010le`; hardware decoders).
    P010,
    /// Separate Y, U, V planes (and A, with `alpha`), of `bits` 8–16: one
    /// byte per sample at 8 bits, else little-endian 16-bit words with the
    /// value in the low bits (`yuv422p10le`, `yuva444p12le`…).
    Planar { bits: u8, chroma: Chroma, alpha: bool },
}

impl YuvLayout {
    /// 8-bit 4:2:0 planes (`yuv420p`; software decoders).
    pub const I420: YuvLayout = YuvLayout::Planar { bits: 8, chroma: Chroma::C420, alpha: false };
    /// 10-bit 4:2:0 planes (`yuv420p10le`).
    pub const I420P10: YuvLayout = YuvLayout::Planar { bits: 10, chroma: Chroma::C420, alpha: false };

    /// Bytes per sample.
    pub fn sample_bytes(self) -> usize {
        if self.bits() > 8 { 2 } else { 1 }
    }
    /// U and V share one interleaved plane.
    pub fn interleaved(self) -> bool {
        matches!(self, YuvLayout::Nv12 | YuvLayout::P010)
    }
    /// Significant bits per sample.
    pub fn bits(self) -> u32 {
        match self {
            YuvLayout::Nv12 => 8,
            YuvLayout::P010 => 10,
            YuvLayout::Planar { bits, .. } => bits as u32,
        }
    }
    /// How far the value is shifted up inside its word.
    pub fn shift(self) -> u32 {
        if self == YuvLayout::P010 { 6 } else { 0 }
    }
    pub fn chroma(self) -> Chroma {
        match self {
            YuvLayout::Planar { chroma, .. } => chroma,
            _ => Chroma::C420,
        }
    }
    /// There's an alpha plane (the last one).
    pub fn alpha(self) -> bool {
        matches!(self, YuvLayout::Planar { alpha: true, .. })
    }
    /// More than 8 bits: worth keeping in floating point all the way.
    pub fn deep(self) -> bool {
        self.bits() > 8
    }
    /// How many planes.
    pub fn planes(self) -> usize {
        if self.interleaved() { 2 } else { 3 + self.alpha() as usize }
    }
    /// Chroma plane size in samples, for a `width`×`height` picture.
    pub fn chroma_size(self, width: u32, height: u32) -> (u32, u32) {
        let (sx, sy) = self.chroma().shift();
        (width.div_ceil(1 << sx), height.div_ceil(1 << sy))
    }
}

/// The YUV → RGB matrix.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum YuvMatrix {
    Bt601,
    Bt709,
    Bt2020,
}

impl YuvMatrix {
    /// (Kr, Kb) luma coefficients.
    pub fn coefficients(self) -> (f32, f32) {
        match self {
            YuvMatrix::Bt601 => (0.299, 0.114),
            YuvMatrix::Bt709 => (0.2126, 0.0722),
            YuvMatrix::Bt2020 => (0.2627, 0.0593),
        }
    }
}

/// How a picture's signal maps to light.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Transfer {
    /// Standard dynamic range (BT.709/sRGB-style gamma).
    #[default]
    Sdr,
    /// HDR10's perceptual quantizer (SMPTE ST 2084), absolute: 0–10 000 nits.
    Pq,
    /// Hybrid log-gamma (ARIB STD-B67), relative.
    Hlg,
}

/// Reference (SDR) white in PQ, nits (ITU-R BT.2408): 1.0 in the compositor.
pub const SDR_WHITE_NITS: f32 = 203.0;
/// Scene light that HLG puts at 75% signal (BT.2408's reference white).
pub const HLG_SDR_WHITE: f32 = 0.264_962_56;
/// Linear BT.2020 RGB → linear BT.709 RGB (HDR footage's wide primaries into
/// the compositor's; colours outside BT.709 come out negative, and are cut).
pub const BT2020_TO_709: [[f32; 3]; 3] = [[1.660_491, -0.587_641, -0.072_850], [-0.124_551, 1.132_900, -0.008_349], [-0.018_151, -0.100_579, 1.118_730]];

impl Transfer {
    pub fn is_hdr(self) -> bool {
        self != Transfer::Sdr
    }

    /// What the GPU shader is told.
    pub fn code(self) -> u32 {
        match self {
            Transfer::Sdr => 0,
            Transfer::Pq => 1,
            Transfer::Hlg => 2,
        }
    }

    /// A signal value (0–1) → linear light, 1.0 being SDR white (so an HDR
    /// highlight comes out above 1). The inverse of what HDR export writes.
    pub fn linear(self, e: f32) -> f32 {
        let e = e.max(0.0);
        match self {
            Transfer::Sdr => {
                if e <= 0.04045 {
                    e / 12.92
                } else {
                    ((e + 0.055) / 1.055).powf(2.4)
                }
            }
            Transfer::Pq => {
                let (m1, m2, c1, c2, c3) = (0.159_301_76, 78.843_75, 0.835_937_5, 18.851_563, 18.6875);
                let p = e.min(1.0).powf(1.0 / m2);
                let y = ((p - c1).max(0.0) / (c2 - c3 * p)).powf(1.0 / m1);
                y * 10_000.0 / SDR_WHITE_NITS
            }
            Transfer::Hlg => {
                let (a, b, c) = (0.178_832_77, 0.284_668_92, 0.559_910_7);
                let s = if e <= 0.5 { e * e / 3.0 } else { (((e - c) / a).exp() + b) / 12.0 };
                s / HLG_SDR_WHITE
            }
        }
    }

    /// Linear R'G'B' at a pixel: through the transfer, and from BT.2020 to
    /// BT.709 primaries for HDR (negatives cut).
    pub fn rgb_linear(self, rgb: [f32; 3]) -> [f32; 3] {
        let l = rgb.map(|c| self.linear(c));
        if !self.is_hdr() {
            return l;
        }
        let m = BT2020_TO_709;
        [0, 1, 2].map(|i| (m[i][0] * l[0] + m[i][1] * l[1] + m[i][2] * l[2]).max(0.0))
    }
}

/// One decoded 4:2:0 picture, tightly packed (no row padding).
#[derive(Clone, PartialEq)]
pub struct YuvPlanes {
    pub layout: YuvLayout,
    /// Luma size in pixels (even).
    pub width: u32,
    pub height: u32,
    /// Y, then U and V (or the one interleaved UV plane).
    pub planes: Vec<Vec<u8>>,
    pub matrix: YuvMatrix,
    /// Full-range (0–255) rather than TV range (16–235).
    pub full_range: bool,
    /// SDR, or HDR (PQ, HLG; BT.2020 primaries).
    pub transfer: Transfer,
}

impl std::fmt::Debug for YuvPlanes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "YuvPlanes({:?} {}x{} {:?}{}{})", self.layout, self.width, self.height, self.matrix, if self.full_range { " full" } else { "" }, if self.transfer.is_hdr() { format!(" {:?}", self.transfer) } else { String::new() })
    }
}

/// The numbers a converter needs, shared by the CPU path and the GPU shader.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct YuvParams {
    /// Divide a sample word by `2^shift` to get the code value.
    pub shift: u32,
    /// Y' = (code − y_off) · y_scale; C' = (code − c_off) · c_scale.
    pub y_off: f32,
    pub y_scale: f32,
    pub c_off: f32,
    pub c_scale: f32,
    pub kr: f32,
    pub kb: f32,
    pub transfer: Transfer,
}

impl YuvPlanes {
    /// Chroma plane size in samples.
    pub fn chroma_size(&self) -> (u32, u32) {
        self.layout.chroma_size(self.width, self.height)
    }

    /// Memory used.
    pub fn byte_len(&self) -> usize {
        self.planes.iter().map(Vec::len).sum()
    }

    pub fn params(&self) -> YuvParams {
        let bits = self.layout.bits();
        let k = (1u32 << (bits - 8)) as f32; // 1 for 8-bit, 4 for 10-bit
        let max = ((1u32 << bits) - 1) as f32;
        let (kr, kb) = self.matrix.coefficients();
        let (y_off, y_scale, c_off, c_scale) = if self.full_range {
            (0.0, 1.0 / max, 128.0 * k, 1.0 / max)
        } else {
            (16.0 * k, 1.0 / (219.0 * k), 128.0 * k, 1.0 / (224.0 * k))
        };
        YuvParams { shift: self.layout.shift(), y_off, y_scale, c_off, c_scale, kr, kb, transfer: self.transfer }
    }

    fn sample(&self, plane: usize, index: usize) -> f32 {
        let p = &self.planes[plane];
        let word = match self.layout.sample_bytes() {
            1 => p[index] as u32,
            _ => u16::from_le_bytes([p[index * 2], p[index * 2 + 1]]) as u32,
        };
        (word >> self.layout.shift()) as f32
    }

    /// Code value of U (`which` = 0) or V (1) at chroma sample (cx, cy).
    fn chroma(&self, which: usize, cx: u32, cy: u32) -> f32 {
        let (cw, _) = self.chroma_size();
        let i = (cy * cw + cx) as usize;
        if self.layout.interleaved() { self.sample(1, i * 2 + which) } else { self.sample(1 + which, i) }
    }

    /// Chroma at luma pixel (x, y), interpolated: subsampled chroma sits level
    /// with even columns, and for 4:2:0 halfway between rows (MPEG-2 siting).
    fn chroma_at(&self, which: usize, x: u32, y: u32) -> f32 {
        let (cw, ch) = self.chroma_size();
        let fx = match self.layout.chroma().shift().0 {
            0 => x as f32,
            _ => x as f32 / 2.0,
        }
        .min((cw - 1) as f32);
        let fy = match self.layout.chroma().shift().1 {
            0 => y as f32,
            _ => (y as f32 - 0.5) / 2.0,
        }
        .clamp(0.0, (ch - 1) as f32);
        let (x0, y0) = (fx.floor() as u32, fy.floor() as u32);
        let (x1, y1) = ((x0 + 1).min(cw - 1), (y0 + 1).min(ch - 1));
        let (ax, ay) = (fx - x0 as f32, fy - y0 as f32);
        let top = self.chroma(which, x0, y0) * (1.0 - ax) + self.chroma(which, x1, y0) * ax;
        let bottom = self.chroma(which, x0, y1) * (1.0 - ax) + self.chroma(which, x1, y1) * ax;
        top * (1.0 - ay) + bottom * ay
    }

    /// Gamma-encoded R'G'B' (0–1) at luma pixel (x, y).
    pub fn rgb_at(&self, x: u32, y: u32) -> [f32; 3] {
        self.rgb_with(&self.params(), x, y)
    }

    fn rgb_with(&self, p: &YuvParams, x: u32, y: u32) -> [f32; 3] {
        let yv = (self.sample(0, (y * self.width + x) as usize) - p.y_off) * p.y_scale;
        let cb = (self.chroma_at(0, x, y) - p.c_off) * p.c_scale;
        let cr = (self.chroma_at(1, x, y) - p.c_off) * p.c_scale;
        let r = yv + 2.0 * (1.0 - p.kr) * cr;
        let b = yv + 2.0 * (1.0 - p.kb) * cb;
        let g = (yv - p.kr * r - p.kb * b) / (1.0 - p.kr - p.kb);
        [r, g, b].map(|c| c.clamp(0.0, 1.0))
    }

    /// Alpha 0–1 at pixel (x, y) (1 without an alpha plane).
    pub fn alpha_at(&self, x: u32, y: u32) -> f32 {
        if !self.layout.alpha() {
            return 1.0;
        }
        let max = ((1u32 << self.layout.bits()) - 1) as f32;
        (self.sample(3, (y * self.width + x) as usize) / max).clamp(0.0, 1.0)
    }

    /// The picture as tightly packed RGBA8 (straight alpha).
    pub fn to_rgba(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity((self.width * self.height * 4) as usize);
        let p = self.params();
        let srgb = |c: f32| {
            let c = c.clamp(0.0, 1.0);
            if c <= 0.003_130_8 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 }
        };
        for y in 0..self.height {
            for x in 0..self.width {
                let mut rgb = self.rgb_with(&p, x, y);
                if self.transfer.is_hdr() {
                    // HDR into an 8-bit SDR picture: SDR white at white, highlights cut.
                    rgb = self.transfer.rgb_linear(rgb).map(srgb);
                }
                let [r, g, b] = rgb;
                let a = self.alpha_at(x, y);
                out.extend_from_slice(&[(r * 255.0).round() as u8, (g * 255.0).round() as u8, (b * 255.0).round() as u8, (a * 255.0).round() as u8]);
            }
        }
        out
    }

    /// The picture as half floats: linear light, premultiplied (for deep
    /// video that has to be worked on in memory, at its precision).
    pub fn to_float(&self) -> FloatPixels {
        let p = self.params();
        let mut rgba = Vec::with_capacity((self.width * self.height * 4) as usize);
        for y in 0..self.height {
            for x in 0..self.width {
                let a = self.alpha_at(x, y);
                for c in self.transfer.rgb_linear(self.rgb_with(&p, x, y)) {
                    rgba.push(f32_to_f16(c * a));
                }
                rgba.push(f32_to_f16(a));
            }
        }
        FloatPixels { width: self.width, height: self.height, rgba }
    }

    /// Brightness 0–255 per pixel (for the motion tracker), straight from Y.
    pub fn luma(&self) -> Vec<f32> {
        let p = self.params();
        let n = (self.width * self.height) as usize;
        (0..n).map(|i| ((self.sample(0, i) - p.y_off) * p.y_scale * 255.0).clamp(0.0, 255.0)).collect()
    }
}

/// A floating-point picture (OpenEXR): RGBA as half floats (IEEE binary16),
/// scene-linear light and premultiplied alpha, as the files store them, so
/// values above 1 (highlights) and light with no alpha (fire, glows) survive
/// into the compositor.
#[derive(Clone, Debug, PartialEq)]
pub struct FloatPixels {
    pub width: u32,
    pub height: u32,
    /// R, G, B, A per pixel, tightly packed.
    pub rgba: Vec<u16>,
}

impl FloatPixels {
    pub fn byte_len(&self) -> usize {
        self.rgba.len() * 2
    }

    /// As 8-bit sRGB with straight alpha (for thumbnails and the tracker):
    /// highlights above 1 are clipped.
    pub fn to_rgba8(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.rgba.len());
        for p in self.rgba.chunks_exact(4) {
            let a = f16_to_f32(p[3]).clamp(0.0, 1.0);
            let un = |v: u16| {
                let v = f16_to_f32(v);
                if a > 1e-6 { v / a } else { v }
            };
            for c in 0..3 {
                out.push((linear_to_srgb(un(p[c])) * 255.0).round() as u8);
            }
            out.push((a * 255.0).round() as u8);
        }
        out
    }
}

/// Linear light 0–1 to the sRGB curve (clamped).
pub fn linear_to_srgb(v: f32) -> f32 {
    let v = if v.is_nan() { 0.0 } else { v.clamp(0.0, 1.0) };
    if v <= 0.003_130_8 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 }
}

/// A half float's value.
pub fn f16_to_f32(h: u16) -> f32 {
    let sign = ((h as u32) & 0x8000) << 16;
    let exp = ((h >> 10) & 0x1f) as u32;
    let man = (h & 0x3ff) as u32;
    let bits = match exp {
        0 if man == 0 => sign,
        0 => {
            let (mut e, mut m) = (113u32, man);
            while m & 0x400 == 0 {
                m <<= 1;
                e -= 1;
            }
            sign | (e << 23) | ((m & 0x3ff) << 13)
        }
        31 => sign | 0x7f80_0000 | (man << 13),
        _ => sign | ((exp + 112) << 23) | (man << 13),
    };
    f32::from_bits(bits)
}

/// The nearest half float (ties to even). For drawing, NaN becomes 0 and
/// anything beyond the half range the largest finite half.
pub fn f32_to_f16(v: f32) -> u16 {
    let x = v.to_bits();
    let sign = ((x >> 16) & 0x8000) as u16;
    let exp = ((x >> 23) & 0xff) as i32;
    let man = x & 0x7f_ffff;
    if exp == 255 {
        return if man != 0 { 0 } else { sign | 0x7bff };
    }
    let e = exp - 127 + 15;
    if e >= 31 {
        return sign | 0x7bff;
    }
    if e <= 0 {
        if e < -10 {
            return sign;
        }
        let m = man | 0x80_0000;
        let shift = (14 - e) as u32;
        let (mut r, rem, half) = (m >> shift, m & ((1 << shift) - 1), 1u32 << (shift - 1));
        if rem > half || (rem == half && r & 1 == 1) {
            r += 1;
        }
        return sign | r as u16;
    }
    let mut h = ((e as u32) << 10) | (man >> 13);
    let rem = man & 0x1fff;
    if rem > 0x1000 || (rem == 0x1000 && h & 1 == 1) {
        h += 1;
    }
    if h >= 0x7c00 {
        h = 0x7bff;
    }
    sign | h as u16
}

/// A picture's pixels: RGBA, YUV planes to be converted when drawn, a
/// floating-point picture, or a frame still on the graphics card.
#[derive(Clone, Debug)]
pub enum Pixels {
    /// Tightly packed RGBA8, sRGB-encoded, straight alpha.
    Rgba(Arc<[u8]>),
    Yuv(Arc<YuvPlanes>),
    /// Half-float RGBA, scene-linear, premultiplied (OpenEXR).
    Float(Arc<FloatPixels>),
    /// Decoded by the graphics card and left there ("zero-copy").
    Gpu(Arc<GpuFrame>),
}

impl Pixels {
    /// Memory used in main memory (a frame on the graphics card uses next to none).
    pub fn byte_len(&self) -> usize {
        match self {
            Pixels::Rgba(b) => b.len(),
            Pixels::Yuv(y) => y.byte_len(),
            Pixels::Float(f) => f.byte_len(),
            Pixels::Gpu(_) => 256,
        }
    }

    /// As RGBA8 (converted on the CPU if it's YUV; copied back first if it's
    /// on the graphics card, black if that fails).
    pub fn to_rgba(&self) -> Arc<[u8]> {
        match self {
            Pixels::Rgba(b) => b.clone(),
            Pixels::Yuv(y) => Arc::from(y.to_rgba()),
            Pixels::Float(f) => Arc::from(f.to_rgba8()),
            Pixels::Gpu(g) => match g.cpu() {
                Some(y) => Arc::from(y.to_rgba()),
                None => Arc::from(vec![0u8; g.width as usize * g.height as usize * 4]),
            },
        }
    }

    /// The YUV planes in main memory, if the pixels are YUV (copied back from
    /// the graphics card if need be).
    pub fn yuv(&self) -> Option<Arc<YuvPlanes>> {
        match self {
            Pixels::Rgba(_) | Pixels::Float(_) => None,
            Pixels::Yuv(y) => Some(y.clone()),
            Pixels::Gpu(g) => g.cpu(),
        }
    }
}

/// A decoded 4:2:0 frame left on the graphics card: a texture the video
/// decoder copied it into, shared with the compositor so the picture never
/// travels through main memory. The handles are the platform's (on Windows,
/// NT handles of a shared D3D11 texture and fence), so this crate needn't know
/// any graphics API.
pub struct GpuFrame {
    /// `Nv12` or `P010`.
    pub layout: YuvLayout,
    /// The picture's size (the texture can be bigger).
    pub width: u32,
    pub height: u32,
    pub matrix: YuvMatrix,
    pub full_range: bool,
    /// SDR or HDR (set after [`GpuFrame::new`] when known).
    pub transfer: Transfer,
    /// The shared texture's handle, and a number that identifies the texture
    /// for as long as it exists (so the compositor can keep it open).
    pub texture: u64,
    pub texture_id: u64,
    /// The texture's allocated size.
    pub alloc: [u32; 2],
    /// A shared fence, and the value it reaches once the frame is in the texture.
    pub fence: u64,
    pub fence_value: u64,
    /// The graphics adapter the texture lives on (its LUID).
    pub adapter: u64,
    /// Keeps the texture from being reused while this frame (or a copy of it)
    /// is held: the compositor clones it until the GPU has finished reading.
    pub hold: Arc<dyn std::any::Any + Send + Sync>,
    /// Copies the frame back to main memory, for anything that needs the pixels there.
    pub download: Arc<dyn Fn() -> Option<YuvPlanes> + Send + Sync>,
    cpu: std::sync::OnceLock<Option<Arc<YuvPlanes>>>,
    /// For the first frames from a decoder: the same frame copied back the
    /// usual way, for the compositor to check its own reading against.
    pub check: Option<Arc<YuvPlanes>>,
}

impl GpuFrame {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        layout: YuvLayout,
        size: [u32; 2],
        matrix: YuvMatrix,
        full_range: bool,
        texture: (u64, u64, [u32; 2]),
        fence: (u64, u64),
        adapter: u64,
        hold: Arc<dyn std::any::Any + Send + Sync>,
        download: Arc<dyn Fn() -> Option<YuvPlanes> + Send + Sync>,
    ) -> Self {
        Self {
            layout,
            width: size[0],
            height: size[1],
            matrix,
            full_range,
            transfer: Transfer::Sdr,
            texture: texture.0,
            texture_id: texture.1,
            alloc: texture.2,
            fence: fence.0,
            fence_value: fence.1,
            adapter,
            hold,
            download,
            cpu: std::sync::OnceLock::new(),
            check: None,
        }
    }

    /// The frame in main memory (copied back once, on first use).
    pub fn cpu(&self) -> Option<Arc<YuvPlanes>> {
        self.cpu
            .get_or_init(|| {
                (self.download)().map(|mut y| {
                    y.transfer = self.transfer;
                    Arc::new(y)
                })
            })
            .clone()
    }

    /// The same numbers for the conversion as the CPU path uses.
    pub fn params(&self) -> YuvParams {
        YuvPlanes { layout: self.layout, width: self.width, height: self.height, planes: Vec::new(), matrix: self.matrix, full_range: self.full_range, transfer: self.transfer }.params()
    }

    pub fn chroma_size(&self) -> (u32, u32) {
        self.layout.chroma_size(self.width, self.height)
    }
}

impl std::fmt::Debug for GpuFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "GpuFrame({:?} {}x{} texture #{} fence {})", self.layout, self.width, self.height, self.texture_id, self.fence_value)
    }
}

/// Zero-copy decoding's switch and state, shared by the decoders (which make
/// [`GpuFrame`]s) and the compositor (which reads them).
pub mod zero_copy {
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::Mutex;

    static ENABLED: AtomicBool = AtomicBool::new(true);
    /// The compositor's graphics adapter (LUID), when it can read shared textures.
    static RENDERER: AtomicU64 = AtomicU64::new(0);
    static CAN_IMPORT: AtomicBool = AtomicBool::new(false);
    static FAILED: Mutex<Option<String>> = Mutex::new(None);
    static FRAMES: AtomicU64 = AtomicU64::new(0);
    static CHECKED: AtomicBool = AtomicBool::new(false);
    static DECODER_NOTE: Mutex<Option<String>> = Mutex::new(None);

    /// The setting (on by default).
    pub fn set_enabled(on: bool) {
        ENABLED.store(on, Ordering::Relaxed);
    }
    pub fn enabled() -> bool {
        ENABLED.load(Ordering::Relaxed)
    }

    /// The compositor can read shared textures made on adapter `luid` (or not: `None`).
    pub fn set_renderer(luid: Option<u64>) {
        RENDERER.store(luid.unwrap_or(0), Ordering::Relaxed);
        CAN_IMPORT.store(luid.is_some(), Ordering::Relaxed);
    }
    pub fn renderer() -> Option<u64> {
        CAN_IMPORT.load(Ordering::Relaxed).then(|| RENDERER.load(Ordering::Relaxed))
    }

    /// Something went wrong (the check didn't match, a texture wouldn't open):
    /// zero-copy is off until Reelforge restarts.
    pub fn fail(reason: impl Into<String>) {
        let reason = reason.into();
        log::warn!("zero-copy decoding switched off: {reason}");
        let mut f = FAILED.lock().unwrap();
        if f.is_none() {
            *f = Some(reason);
        }
    }
    pub fn failure() -> Option<String> {
        FAILED.lock().unwrap().clone()
    }

    /// Should decoders hand over frames on the graphics card?
    pub fn usable() -> bool {
        enabled() && renderer().is_some() && FAILED.lock().unwrap().is_none()
    }

    /// Why the decoders aren't using it, when it's their side (say, a different adapter).
    pub fn note_decoder(reason: impl Into<String>) {
        *DECODER_NOTE.lock().unwrap() = Some(reason.into());
    }

    /// A frame was drawn straight from the graphics card.
    pub fn count_frame() {
        FRAMES.fetch_add(1, Ordering::Relaxed);
    }
    pub fn frames() -> u64 {
        FRAMES.load(Ordering::Relaxed)
    }
    /// The compositor's check of its reading against the usual copy passed.
    pub fn set_checked() {
        CHECKED.store(true, Ordering::Relaxed);
    }
    pub fn checked() -> bool {
        CHECKED.load(Ordering::Relaxed)
    }

    /// One line for the settings window.
    pub fn status() -> String {
        if !enabled() {
            return "off".into();
        }
        if let Some(f) = failure() {
            return format!("off: {f}");
        }
        if renderer().is_none() {
            return "not available here (Windows with Direct3D 12 and hardware decoding)".into();
        }
        if let Some(n) = DECODER_NOTE.lock().unwrap().clone() {
            if frames() == 0 {
                return format!("not used: {n}");
            }
        }
        match (frames(), checked()) {
            (0, _) => "ready (used for hardware-decoded video in the viewer)".into(),
            (n, true) => format!("working: {n} frames shown straight from the graphics card, checked against the usual copy"),
            (n, false) => format!("working: {n} frames shown straight from the graphics card"),
        }
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn half_floats_round_trip_and_round_to_nearest() {
        for h in (0u16..=0xffff).step_by(7) {
            let v = f16_to_f32(h);
            if v.is_finite() {
                assert_eq!(f32_to_f16(v), h, "{h:#06x} = {v}");
            }
        }
        assert_eq!(f32_to_f16(1.0), 0x3c00);
        assert_eq!(f32_to_f16(1.0 + 1.0 / 2048.0), 0x3c00, "a tie rounds to even");
        assert_eq!(f32_to_f16(1.0 + 3.0 / 2048.0), 0x3c02);
        assert_eq!(f32_to_f16(1e9), 0x7bff);
        assert_eq!(f32_to_f16(f32::NAN), 0);
        assert_eq!(f32_to_f16(3e-8), 0x0001);
        let f = FloatPixels { width: 2, height: 1, rgba: [0.5f32, 0.5, 0.5, 0.5, 4.0, 0.0, 0.0, 1.0].map(f32_to_f16).to_vec() };
        // Premultiplied 0.5 at alpha 0.5 is linear 1 (white); 4.0 clips.
        assert_eq!(f.to_rgba8(), vec![255, 255, 255, 128, 255, 0, 0, 255]);
    }

    use super::*;

    /// A flat 4×2 picture with the given 8-bit Y, U, V.
    fn flat(layout: YuvLayout, y: u16, u: u16, v: u16, matrix: YuvMatrix, full_range: bool) -> YuvPlanes {
        let (w, h) = (4u32, 2u32);
        let put = |v: u16, n: usize| -> Vec<u8> {
            let word = (v as u32) << layout.shift();
            (0..n).flat_map(|_| if layout.sample_bytes() == 1 { vec![word as u8] } else { (word as u16).to_le_bytes().to_vec() }).collect()
        };
        let (cw, ch) = layout.chroma_size(w, h);
        let c = (cw * ch) as usize;
        let mut planes = if layout.interleaved() {
            let uv: Vec<u8> = (0..c).flat_map(|_| [put(u, 1), put(v, 1)].concat()).collect();
            vec![put(y, (w * h) as usize), uv]
        } else {
            vec![put(y, (w * h) as usize), put(u, c), put(v, c)]
        };
        if layout.alpha() {
            planes.push(put(((1u32 << layout.bits()) - 1) as u16, (w * h) as usize));
        }
        YuvPlanes { layout, width: w, height: h, planes, matrix, full_range, transfer: Transfer::Sdr }
    }

    #[test]
    fn tv_range_black_white_and_primaries() {
        let px = |y, u, v| flat(YuvLayout::I420, y, u, v, YuvMatrix::Bt709, false).to_rgba()[..3].to_vec();
        assert_eq!(px(16, 128, 128), [0, 0, 0]);
        assert_eq!(px(235, 128, 128), [255, 255, 255]);
        // BT.709 pure red is Y 63, Cb 102, Cr 240.
        let red = px(63, 102, 240);
        assert!(red[0] >= 253 && red[1] <= 2 && red[2] <= 2, "{red:?}");
        // Full range: 0 and 255.
        let full = flat(YuvLayout::Nv12, 255, 128, 128, YuvMatrix::Bt601, true).to_rgba();
        assert_eq!(&full[..3], &[255, 255, 255]);
    }

    #[test]
    fn ten_bit_layouts_match_eight_bit() {
        // The same colour at 10 bits (×4) reads the same in both 10-bit layouts.
        let eight = flat(YuvLayout::I420, 81, 90, 240, YuvMatrix::Bt709, false).to_rgba();
        for layout in [YuvLayout::I420P10, YuvLayout::P010] {
            let ten = flat(layout, 81 * 4, 90 * 4, 240 * 4, YuvMatrix::Bt709, false).to_rgba();
            for (a, b) in eight.iter().zip(&ten) {
                assert!((*a as i32 - *b as i32).abs() <= 1, "{layout:?}: {eight:?} vs {ten:?}");
            }
        }
        let p = flat(YuvLayout::P010, 64, 512, 512, YuvMatrix::Bt2020, false);
        assert_eq!(p.params().shift, 6);
        assert_eq!(&p.to_rgba()[..3], &[0, 0, 0], "10-bit TV black");
        assert_eq!(p.luma()[0], 0.0);
        assert_eq!(p.byte_len(), 4 * 2 * 2 + 2 * 2 * 2);
    }

    #[test]
    fn deeper_and_finer_layouts_match_eight_bit() {
        let eight = flat(YuvLayout::I420, 81, 90, 240, YuvMatrix::Bt709, false).to_rgba();
        for bits in [10u8, 12, 16] {
            for chroma in [Chroma::C420, Chroma::C422, Chroma::C444] {
                for alpha in [false, true] {
                    let layout = YuvLayout::Planar { bits, chroma, alpha };
                    let k = 1u16 << (bits - 8);
                    let p = flat(layout, 81 * k, 90 * k, 240 * k, YuvMatrix::Bt709, false);
                    assert_eq!(p.planes.len(), layout.planes());
                    let deep = p.to_rgba();
                    for (a, b) in eight.iter().zip(&deep) {
                        assert!((*a as i32 - *b as i32).abs() <= 1, "{layout:?}: {eight:?} vs {deep:?}");
                    }
                }
            }
        }
        // 4:2:2 chroma is full height, 4:4:4 full size.
        assert_eq!(YuvLayout::Planar { bits: 10, chroma: Chroma::C422, alpha: false }.chroma_size(1920, 1080), (960, 1080));
        assert_eq!(YuvLayout::Planar { bits: 12, chroma: Chroma::C444, alpha: true }.chroma_size(1921, 1080), (1921, 1080));
        // Alpha comes through.
        let mut p = flat(YuvLayout::Planar { bits: 12, chroma: Chroma::C444, alpha: true }, 2048, 2048, 2048, YuvMatrix::Bt709, true);
        p.planes[3][0..2].copy_from_slice(&2048u16.to_le_bytes());
        assert_eq!(p.to_rgba()[3], 128);
    }

    #[test]
    fn deep_video_converts_to_float_without_eight_bit_steps() {
        let layout = YuvLayout::Planar { bits: 10, chroma: Chroma::C444, alpha: false };
        let mut p = flat(layout, 64, 512, 512, YuvMatrix::Bt709, false);
        // Y 500, 501, 502, 503: four neighbours an 8-bit copy would merge.
        p.planes[0] = (500u16..504).chain(500..504).flat_map(|v| v.to_le_bytes()).collect();
        let f = p.to_float();
        let g: Vec<f32> = (0..4).map(|x| f16_to_f32(f.rgba[x * 4 + 1])).collect();
        assert!(g.windows(2).all(|w| w[1] > w[0]), "{g:?}");
        // (500 − 64) / 876 = 0.4977 encoded → 0.2119 linear.
        assert!((g[0] - 0.2119).abs() < 0.001, "{}", g[0]);
        assert_eq!(f16_to_f32(f.rgba[3]), 1.0);
    }

    #[test]
    fn four_four_four_chroma_is_per_pixel() {
        // Every column its own colour: no blending between neighbours.
        let mut p = flat(YuvLayout::Planar { bits: 8, chroma: Chroma::C444, alpha: false }, 128, 128, 128, YuvMatrix::Bt709, true);
        p.planes[2] = vec![0, 255, 0, 255, 0, 255, 0, 255];
        let row: Vec<u8> = (0..4).map(|x| p.to_rgba()[x * 4]).collect();
        assert!(row[0] < 30 && row[1] > 225 && row[2] < 30 && row[3] > 225, "{row:?}");
    }

    #[test]
    fn chroma_is_interpolated_between_samples() {
        // Left chroma column blue-ish, right column red-ish: the middle pixels blend.
        let mut p = flat(YuvLayout::I420, 128, 128, 128, YuvMatrix::Bt709, true);
        p.planes[2] = vec![0, 255]; // V: left 0, right 255
        let row: Vec<u8> = (0..4).map(|x| p.to_rgba()[x * 4]).collect();
        assert!(row[0] < row[1] && row[1] < row[2] && row[2] <= row[3], "red rises left to right: {row:?}");
    }
}
