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

/// How the planes are laid out. All are 4:2:0.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum YuvLayout {
    /// 8-bit Y, U, V planes (`yuv420p`; software decoders).
    I420,
    /// 8-bit Y plane and interleaved UV (`nv12`; hardware decoders).
    Nv12,
    /// 10-bit Y, U, V planes, little-endian 16-bit words, value in the low bits (`yuv420p10le`).
    I420P10,
    /// 10-bit Y and interleaved UV, value in the high bits of each word (`p010le`; hardware decoders).
    P010,
}

impl YuvLayout {
    /// Bytes per sample.
    pub fn sample_bytes(self) -> usize {
        match self {
            YuvLayout::I420 | YuvLayout::Nv12 => 1,
            YuvLayout::I420P10 | YuvLayout::P010 => 2,
        }
    }
    /// U and V share one interleaved plane.
    pub fn interleaved(self) -> bool {
        matches!(self, YuvLayout::Nv12 | YuvLayout::P010)
    }
    /// Significant bits per sample.
    pub fn bits(self) -> u32 {
        match self {
            YuvLayout::I420 | YuvLayout::Nv12 => 8,
            YuvLayout::I420P10 | YuvLayout::P010 => 10,
        }
    }
    /// How far the value is shifted up inside its word.
    pub fn shift(self) -> u32 {
        if self == YuvLayout::P010 { 6 } else { 0 }
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
}

impl std::fmt::Debug for YuvPlanes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "YuvPlanes({:?} {}x{} {:?}{})", self.layout, self.width, self.height, self.matrix, if self.full_range { " full" } else { "" })
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
}

impl YuvPlanes {
    /// Chroma plane size in samples.
    pub fn chroma_size(&self) -> (u32, u32) {
        (self.width.div_ceil(2), self.height.div_ceil(2))
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
        YuvParams { shift: self.layout.shift(), y_off, y_scale, c_off, c_scale, kr, kb }
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

    /// Chroma at luma pixel (x, y), interpolated: 4:2:0 chroma sits level with
    /// even columns and halfway between rows (MPEG-2 siting).
    fn chroma_at(&self, which: usize, x: u32, y: u32) -> f32 {
        let (cw, ch) = self.chroma_size();
        let fx = (x as f32 / 2.0).min((cw - 1) as f32);
        let fy = ((y as f32 - 0.5) / 2.0).clamp(0.0, (ch - 1) as f32);
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

    /// The picture as tightly packed RGBA8 (opaque).
    pub fn to_rgba(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity((self.width * self.height * 4) as usize);
        let p = self.params();
        for y in 0..self.height {
            for x in 0..self.width {
                let [r, g, b] = self.rgb_with(&p, x, y);
                out.extend_from_slice(&[(r * 255.0).round() as u8, (g * 255.0).round() as u8, (b * 255.0).round() as u8, 255]);
            }
        }
        out
    }

    /// Brightness 0–255 per pixel (for the motion tracker), straight from Y.
    pub fn luma(&self) -> Vec<f32> {
        let p = self.params();
        let n = (self.width * self.height) as usize;
        (0..n).map(|i| ((self.sample(0, i) - p.y_off) * p.y_scale * 255.0).clamp(0.0, 255.0)).collect()
    }
}

/// A picture's pixels: RGBA, YUV planes to be converted when drawn, or a
/// frame still on the graphics card.
#[derive(Clone, Debug)]
pub enum Pixels {
    /// Tightly packed RGBA8, sRGB-encoded, straight alpha.
    Rgba(Arc<[u8]>),
    Yuv(Arc<YuvPlanes>),
    /// Decoded by the graphics card and left there ("zero-copy").
    Gpu(Arc<GpuFrame>),
}

impl Pixels {
    /// Memory used in main memory (a frame on the graphics card uses next to none).
    pub fn byte_len(&self) -> usize {
        match self {
            Pixels::Rgba(b) => b.len(),
            Pixels::Yuv(y) => y.byte_len(),
            Pixels::Gpu(_) => 256,
        }
    }

    /// As RGBA8 (converted on the CPU if it's YUV; copied back first if it's
    /// on the graphics card, black if that fails).
    pub fn to_rgba(&self) -> Arc<[u8]> {
        match self {
            Pixels::Rgba(b) => b.clone(),
            Pixels::Yuv(y) => Arc::from(y.to_rgba()),
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
            Pixels::Rgba(_) => None,
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
        self.cpu.get_or_init(|| (self.download)().map(Arc::new)).clone()
    }

    /// The same numbers for the conversion as the CPU path uses.
    pub fn params(&self) -> YuvParams {
        YuvPlanes { layout: self.layout, width: self.width, height: self.height, planes: Vec::new(), matrix: self.matrix, full_range: self.full_range }.params()
    }

    pub fn chroma_size(&self) -> (u32, u32) {
        (self.width.div_ceil(2), self.height.div_ceil(2))
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
    use super::*;

    /// A flat 4×2 picture with the given 8-bit Y, U, V.
    fn flat(layout: YuvLayout, y: u16, u: u16, v: u16, matrix: YuvMatrix, full_range: bool) -> YuvPlanes {
        let (w, h) = (4u32, 2u32);
        let put = |v: u16, n: usize| -> Vec<u8> {
            let word = (v as u32) << layout.shift();
            (0..n).flat_map(|_| if layout.sample_bytes() == 1 { vec![word as u8] } else { (word as u16).to_le_bytes().to_vec() }).collect()
        };
        let c = (w / 2 * h / 2) as usize;
        let planes = if layout.interleaved() {
            let uv: Vec<u8> = (0..c).flat_map(|_| [put(u, 1), put(v, 1)].concat()).collect();
            vec![put(y, (w * h) as usize), uv]
        } else {
            vec![put(y, (w * h) as usize), put(u, c), put(v, c)]
        };
        YuvPlanes { layout, width: w, height: h, planes, matrix, full_range }
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
    fn chroma_is_interpolated_between_samples() {
        // Left chroma column blue-ish, right column red-ish: the middle pixels blend.
        let mut p = flat(YuvLayout::I420, 128, 128, 128, YuvMatrix::Bt709, true);
        p.planes[2] = vec![0, 255]; // V: left 0, right 255
        let row: Vec<u8> = (0..4).map(|x| p.to_rgba()[x * 4]).collect();
        assert!(row[0] < row[1] && row[1] < row[2] && row[2] <= row[3], "red rises left to right: {row:?}");
    }
}
