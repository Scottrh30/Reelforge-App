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
//! | Apple ProRes | [`prores`] | 422 Proxy/LT/Standard/HQ, 4444/XQ with alpha, progressive and interlaced; Reelforge's own |
//!
//! Every decoder is checked against FFmpeg's output in the media crate's tests.

pub mod bits;
pub mod idct;
pub mod mjpeg;
pub mod prores;

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
    Vp9,
    Av1,
}

impl Codec {
    pub const ALL: [Codec; 8] = [Codec::Mjpeg, Codec::ProRes, Codec::Dnxhd, Codec::Mpeg2, Codec::H264, Codec::Hevc, Codec::Vp9, Codec::Av1];

    pub fn label(self) -> &'static str {
        match self {
            Codec::Mjpeg => "Motion JPEG",
            Codec::ProRes => "Apple ProRes",
            Codec::Dnxhd => "DNxHD / DNxHR",
            Codec::Mpeg2 => "MPEG-2",
            Codec::H264 => "H.264",
            Codec::Hevc => "HEVC (H.265)",
            Codec::Vp9 => "VP9",
            Codec::Av1 => "AV1",
        }
    }

    /// Does Reelforge have its own decoder for this yet?
    pub fn available(self) -> bool {
        matches!(self, Codec::Mjpeg | Codec::ProRes)
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
    Yuv422P12,
    Yuv444P12,
    /// 4:4:4 with an alpha plane (ProRes 4444).
    Yuva444P10,
    Yuva444P12,
}

impl Format {
    pub fn bytes_per_sample(self) -> usize {
        match self {
            Format::Gray8 | Format::Yuv420P8 | Format::Yuv422P8 | Format::Yuv444P8 => 1,
            _ => 2,
        }
    }
    pub fn planes(self) -> usize {
        match self {
            Format::Gray8 => 1,
            Format::Yuva444P10 | Format::Yuva444P12 => 4,
            _ => 3,
        }
    }
    /// Chroma subsampling: (horizontal, vertical) shift.
    pub fn chroma_shift(self) -> (u32, u32) {
        match self {
            Format::Yuv420P8 | Format::Yuv420P10 => (1, 1),
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
#[derive(Clone, Debug, PartialEq)]
pub struct Plane {
    pub data: Vec<u8>,
    pub stride: usize,
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
        _ => None,
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
