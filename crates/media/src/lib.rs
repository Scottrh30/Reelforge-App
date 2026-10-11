//! FFmpeg-backed media layer: probing files and decoding video on worker threads.
//!
//! The UI never touches FFmpeg directly. It asks a [`FrameSource`] for "the frame
//! at time t" every repaint, and the source keeps a decoder thread running ahead.

pub mod audio;
pub mod cache;
pub mod color;
pub mod decoder;
pub mod encode;
pub mod hw;
pub mod mixer;
pub mod probe;
pub mod proxy;
pub mod source;
pub mod tracker;

pub use audio::{waveform_peaks, AudioReader};
pub use cache::{CacheKey, FrameCache};
pub use decoder::{DecoderHandle, VideoFrame};
pub use encode::{is_hardware_encoder, sequence_file, Fit, MovieWriter, Quality, VideoCodec, WriterSettings};
pub use mixer::{MixClip, Mixer};
pub use probe::probe;
pub use source::FrameSource;
pub use tracker::{track_point, TrackEnd};

use std::sync::Once;

/// Initialise FFmpeg once per process. Safe to call repeatedly.
pub fn init() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        ffmpeg_next::init().expect("failed to initialise FFmpeg");
        ffmpeg_next::util::log::set_level(ffmpeg_next::util::log::Level::Error);
    });
}

pub(crate) fn to_rational(r: ffmpeg_next::Rational) -> reelforge_core::Rational {
    if r.denominator() == 0 {
        reelforge_core::Rational::new(0, 1)
    } else {
        reelforge_core::Rational::new(r.numerator() as i64, r.denominator() as i64)
    }
}

/// FFmpeg's container-level time unit (microseconds).
pub(crate) const AV_TIME_BASE: i64 = 1_000_000;
