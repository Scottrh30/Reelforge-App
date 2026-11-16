//! Pure-Rust VP8 decoder, written from the VP8 specification (RFC 6386).
//!
//! Supported: everything a VP8 stream can use. Versions (profiles) 0-3 (6-tap and bilinear
//! interpolation, whole-pixel chroma), key and inter frames, golden and alternate reference
//! frames with sign bias and buffer copies, hidden frames, segmentation (quantiser and loop
//! filter per segment, persistent segment maps), loop filter deltas by reference frame and
//! mode, normal and simple loop filters with sharpness, multiple token partitions, frame-local
//! probability updates and odd frame sizes. Output is 8-bit 4:2:0.
//!
//! ```no_run
//! let mut dec = reelforge_vp8::Decoder::new();
//! let frame: Vec<u8> = Vec::new(); // one IVF / WebM frame
//! if let Some(pic) = dec.decode(&frame, 0).unwrap() {
//!     println!("{}x{}", pic.width, pic.height);
//! }
//! ```

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]
#![allow(clippy::needless_range_loop, clippy::too_many_arguments)]

mod boolcoder;
mod decoder;
mod header;
mod loopfilter;
mod modes;
mod recon;
mod residual;
#[rustfmt::skip]
mod tables;

pub use decoder::Decoder;

/// Why a frame couldn't be decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The data isn't a valid VP8 frame.
    Invalid(&'static str),
    /// Valid, but beyond what this decoder accepts.
    Unsupported(&'static str),
    /// An inter frame came before any key frame.
    NoKeyframe,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Invalid(s) => write!(f, "invalid VP8 data: {s}"),
            Error::Unsupported(s) => write!(f, "unsupported VP8 stream: {s}"),
            Error::NoKeyframe => write!(f, "VP8 inter frame before the first key frame"),
        }
    }
}

impl std::error::Error for Error {}

/// A decoded picture: 8-bit 4:2:0, BT.601 limited range (VP8 has no other).
#[derive(Clone, Debug)]
pub struct Picture {
    pub width: u32,
    pub height: u32,
    pub chroma_width: u32,
    pub chroma_height: u32,
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
    pub y_stride: usize,
    pub uv_stride: usize,
    /// `pts` passed to [`Decoder::decode`] with the frame.
    pub pts: i64,
    pub key: bool,
}

/// The size of a key frame, None for inter frames and anything that isn't VP8.
pub fn keyframe_size(frame: &[u8]) -> Option<(u32, u32)> {
    let t = header::frame_tag(frame).ok()?;
    t.key.then_some((u32::from(t.width), u32::from(t.height)))
}
