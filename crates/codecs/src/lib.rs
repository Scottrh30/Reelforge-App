//! Reelforge's own video decoders.
//!
//! Each decoder takes the compressed packets of one video stream (as a
//! container gives them: one picture per packet) and hands back decoded
//! pictures as planar YUV. The media crate reads the containers (with
//! FFmpeg) and uses these decoders where one exists for the stream's codec,
//! falling back to FFmpeg's decoder if a native one isn't available or fails.
//!
//! | Codec | Module | Notes |
//! |---|---|---|
//! | Motion JPEG | [`mjpeg`] | baseline and progressive JPEG per frame (zune-jpeg), AVI-style frames without Huffman tables |
//! | DNxHD / DNxHR | [`dnxhd`] | every compression ID: DNxHD 8/10-bit, DNxHR LB/SQ/HQ/HQX/444, interlaced and adaptive-interlace; Reelforge's own |
//! | MPEG-2 | [`mpeg2`] | Main and 4:2:2 profiles: I/P/B, frame and field pictures, all motion types; Reelforge's own |
//! | H.264, HEVC, VP8, VP9, AV1 | `external` | pure-Rust decoder libraries: H.264 progressive and MBAFF 4:2:0/4:2:2 at 8–12 bits (Baseline/Main/High/High 10/High 4:2:2), HEVC Main/Main 10/Main 4:2:2 10/Main 4:4:4, VP8 versions 0–3, VP9 profiles 0–3, AV1 Main/High/Professional |
//! | Apple ProRes | [`prores`] | 422 Proxy/LT/Standard/HQ, 4444/XQ with alpha, progressive and interlaced; Reelforge's own |
//! | OpenEXR | [`exr`] | scanline and tiled, single and multi-part, none/RLE/ZIPS/ZIP/PIZ/PXR24/B44/B44A, half/float/uint; float output; Reelforge's own |
//! | GoPro CineForm | [`cineform`] | YUV 4:2:2 10-bit, RGB and RGBA 12-bit, RAW (Bayer mosaic), intra and two-frame groups, progressive and interlaced; Reelforge's own |
//!
//! Every decoder is checked against FFmpeg's output in the media crate's tests.

// Never crash: no unwrap, expect, panic, unreachable, todo or unimplemented
// outside tests (a damaged file or an odd setting is an error, not a crash).
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

// (as the other decoders: transforms and filters index several arrays in step,
// as the spec writes them)
#![allow(clippy::needless_range_loop, clippy::too_many_arguments)]

pub mod bits;
pub mod cineform;
mod cineform_tables;
pub mod dnxhd;
pub mod exr;
mod external;
pub use external::{Av1Decoder, H264Decoder, HevcDecoder, Vp8Decoder, Vp9Decoder};
mod dnxhd_tables;
pub mod idct;
pub mod mjpeg;
pub mod mpeg2;
pub mod prores;
pub mod vlc;

use std::collections::VecDeque;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};

/// The codecs Reelforge has (or will have) its own decoder for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Codec {
    Mjpeg,
    ProRes,
    Dnxhd,
    Mpeg2,
    H264,
    Hevc,
    Vp8,
    Vp9,
    Av1,
    Cineform,
    Exr,
}

impl Codec {
    pub const ALL: [Codec; 11] = [Codec::Mjpeg, Codec::ProRes, Codec::Dnxhd, Codec::Cineform, Codec::Exr, Codec::Mpeg2, Codec::H264, Codec::Hevc, Codec::Vp8, Codec::Vp9, Codec::Av1];

    pub fn label(self) -> &'static str {
        match self {
            Codec::Mjpeg => "Motion JPEG",
            Codec::ProRes => "Apple ProRes",
            Codec::Dnxhd => "DNxHD / DNxHR",
            Codec::Mpeg2 => "MPEG-2",
            Codec::H264 => "H.264",
            Codec::Hevc => "HEVC (H.265)",
            Codec::Vp8 => "VP8",
            Codec::Vp9 => "VP9",
            Codec::Av1 => "AV1",
            Codec::Cineform => "GoPro CineForm",
            Codec::Exr => "OpenEXR",
        }
    }

    /// Codecs graphics cards commonly decode: hardware decoding, when it's on and
    /// works, is preferred to the CPU decoder for these.
    pub fn gpu_friendly(self) -> bool {
        matches!(self, Codec::H264 | Codec::Hevc | Codec::Vp9 | Codec::Av1)
    }

    /// Does Reelforge have its own decoder for this yet?
    pub fn available(self) -> bool {
        true
    }
}

/// How a picture's samples are laid out. All planar; `P10`/`P12` store each
/// sample as a little-endian u16.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Gray8,
    Yuv420P8,
    Yuv422P8,
    Yuv444P8,
    Yuv420P10,
    Yuv422P10,
    Yuv444P10,
    Yuv420P12,
    Yuv422P12,
    Yuv444P12,
    /// 4:4:4 with an alpha plane (ProRes 4444).
    Yuva444P10,
    Yuva444P12,
    /// Planar RGB, stored green, blue, red (DNxHR 444).
    Gbrp10,
    Gbrp12,
    /// Planar RGB with alpha: green, blue, red, alpha (CineForm RGBA).
    Gbrap12,
    /// A raw camera sensor picture: one plane, a red-green / green-blue
    /// mosaic of 16-bit samples (CineForm RAW), for demosaicing later.
    BayerRggb16,
    /// Planar 32-bit float RGB (green, blue, red), scene-linear (OpenEXR).
    Gbrpf32,
    /// The same with alpha (premultiplied, as EXR stores it).
    Gbrapf32,
    /// Planar half-float RGB / RGBA (IEEE binary16), scene-linear (half-float EXR).
    Gbrpf16,
    Gbrapf16,
}

impl Format {
    pub fn bytes_per_sample(self) -> usize {
        match self {
            Format::Gray8 | Format::Yuv420P8 | Format::Yuv422P8 | Format::Yuv444P8 => 1,
            Format::Gbrpf32 | Format::Gbrapf32 => 4,
            _ => 2,
        }
    }
    pub fn planes(self) -> usize {
        match self {
            Format::Gray8 | Format::BayerRggb16 => 1,
            Format::Yuva444P10 | Format::Yuva444P12 | Format::Gbrap12 | Format::Gbrapf32 | Format::Gbrapf16 => 4,
            _ => 3,
        }
    }
    /// Chroma subsampling: (horizontal, vertical) shift.
    pub fn chroma_shift(self) -> (u32, u32) {
        match self {
            Format::Yuv420P8 | Format::Yuv420P10 | Format::Yuv420P12 => (1, 1),
            Format::Yuv422P8 | Format::Yuv422P10 | Format::Yuv422P12 => (1, 0),
            _ => (0, 0),
        }
    }
    /// Width and height of plane `i` for a `w`×`h` picture.
    pub fn plane_size(self, i: usize, w: u32, h: u32) -> (u32, u32) {
        let (sx, sy) = self.chroma_shift();
        if i == 0 || i == 3 {
            (w, h)
        } else {
            ((w + (1 << sx) - 1) >> sx, (h + (1 << sy) - 1) >> sy)
        }
    }
}

/// The colour matrix the YUV values use.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Matrix {
    #[default]
    Unknown,
    Bt601,
    Bt709,
    Bt2020,
}

/// One plane: `stride` bytes per row.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Plane {
    pub data: Vec<u8>,
    pub stride: usize,
}

impl Plane {
    /// Copy `rows` rows of `cols` 16-bit samples from `src` (`src_stride`
    /// samples apart) in as little-endian, a row at a time.
    pub fn put_u16_rows(&mut self, src: &[u16], src_stride: usize, cols: usize, rows: usize) {
        for r in 0..rows {
            let s = &src[r * src_stride..r * src_stride + cols];
            let d = &mut self.data[r * self.stride..r * self.stride + cols * 2];
            if cfg!(target_endian = "little") {
                // SAFETY: u16 has no padding or invalid bit patterns, and in
                // memory it's already little-endian here.
                d.copy_from_slice(unsafe { std::slice::from_raw_parts(s.as_ptr() as *const u8, cols * 2) });
            } else {
                for (c, px) in s.iter().enumerate() {
                    d[c * 2..c * 2 + 2].copy_from_slice(&px.to_le_bytes());
                }
            }
        }
    }

    /// The same into 8-bit samples (values already within 0–255).
    pub fn put_u16_rows_as_u8(&mut self, src: &[u16], src_stride: usize, cols: usize, rows: usize) {
        for r in 0..rows {
            let s = &src[r * src_stride..r * src_stride + cols];
            let d = &mut self.data[r * self.stride..r * self.stride + cols];
            for (o, v) in d.iter_mut().zip(s) {
                *o = *v as u8;
            }
        }
    }
}

/// A decoded picture.
#[derive(Clone, Debug, PartialEq)]
pub struct Picture {
    pub width: u32,
    pub height: u32,
    pub format: Format,
    pub planes: Vec<Plane>,
    pub matrix: Matrix,
    /// 0–255 (JPEG) levels rather than 16–235.
    pub full_range: bool,
    /// The timestamp of the packet this picture came from (container units).
    pub pts: i64,
    pub interlaced: bool,
}

impl Picture {
    /// An empty (black-filled for luma 0) picture with tight planes.
    pub fn new(width: u32, height: u32, format: Format) -> Self {
        let b = format.bytes_per_sample();
        let planes = (0..format.planes())
            .map(|i| {
                let (pw, ph) = format.plane_size(i, width, height);
                let stride = pw as usize * b;
                Plane { data: vec![0; stride * ph as usize], stride }
            })
            .collect();
        Picture { width, height, format, planes, matrix: Matrix::Unknown, full_range: false, pts: 0, interlaced: false }
    }

    /// Sample (x, y) of plane `p` (u16 for high bit depths, else u8 widened).
    pub fn sample(&self, p: usize, x: usize, y: usize) -> u16 {
        let pl = &self.planes[p];
        if self.format.bytes_per_sample() == 1 {
            pl.data[y * pl.stride + x] as u16
        } else {
            let i = y * pl.stride + x * 2;
            u16::from_le_bytes([pl.data[i], pl.data[i + 1]])
        }
    }

    pub fn set_u16(&mut self, p: usize, x: usize, y: usize, v: u16) {
        let pl = &mut self.planes[p];
        let i = y * pl.stride + x * 2;
        pl.data[i..i + 2].copy_from_slice(&v.to_le_bytes());
    }
}

/// Why a packet couldn't be decoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// The data breaks the format's rules (or is cut short).
    Invalid(String),
    /// Valid, but uses a feature this decoder doesn't handle yet.
    Unsupported(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Invalid(s) => write!(f, "invalid data: {s}"),
            Error::Unsupported(s) => write!(f, "not supported yet: {s}"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn invalid<T>(s: impl Into<String>) -> Result<T> {
    Err(Error::Invalid(s.into()))
}

/// A video decoder: packets in (decode order), pictures out (display order).
pub trait Decoder: Send {
    /// Decode one packet. Pictures it completes become available from [`Decoder::receive`].
    fn send(&mut self, packet: &[u8], pts: i64) -> Result<()>;
    /// The next finished picture, if any.
    fn receive(&mut self) -> Option<Picture>;
    /// Forget everything in flight (before decoding from a new keyframe).
    fn flush(&mut self);
    /// No more packets: release any pictures held back for reordering.
    fn finish(&mut self) {}
}

/// What the stream says about itself (from the container).
#[derive(Clone, Debug, Default)]
pub struct StreamInfo {
    pub width: u32,
    pub height: u32,
    /// Codec-specific setup data (avcC, hvcC, sequence headers…).
    pub extradata: Vec<u8>,
}

static ENABLED: AtomicBool = AtomicBool::new(true);

/// Use the native decoders (Settings). When off, FFmpeg decodes everything.
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// A decoder for `codec`, if Reelforge has one and they're switched on.
pub fn open(codec: Codec, info: &StreamInfo) -> Option<Box<dyn Decoder>> {
    if !enabled() {
        return None;
    }
    match codec {
        Codec::Mjpeg => Some(Box::new(mjpeg::MjpegDecoder::new(info))),
        Codec::ProRes => Some(Box::new(prores::ProResDecoder::new(info))),
        Codec::Dnxhd => Some(Box::new(dnxhd::DnxhdDecoder::new(info))),
        Codec::Mpeg2 => Some(Box::new(mpeg2::Mpeg2Decoder::new(info))),
        Codec::H264 => Some(Box::new(H264Decoder::new(info))),
        Codec::Hevc => Some(Box::new(HevcDecoder::new(info))),
        Codec::Vp8 => Some(Box::new(Vp8Decoder::new(info))),
        Codec::Vp9 => Some(Box::new(Vp9Decoder::new(info))),
        Codec::Av1 => Some(Box::new(Av1Decoder::new(info))),
        Codec::Cineform => Some(Box::new(cineform::CineformDecoder::new(info))),
        Codec::Exr => Some(Box::new(exr::ExrDecoder::new(info))),
    }
}

/// Pictures waiting to be collected.
#[derive(Default)]
pub(crate) struct Output(VecDeque<Picture>);

impl Output {
    pub fn push(&mut self, p: Picture) {
        self.0.push_back(p);
    }
    pub fn pop(&mut self) -> Option<Picture> {
        self.0.pop_front()
    }
    pub fn clear(&mut self) {
        self.0.clear();
    }
}
