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
#[cfg(windows)]
mod zerocopy;
pub mod mixer;
pub mod native;
pub mod probe;
pub mod proxy;
pub mod source;
pub mod tracker;

pub use audio::{waveform_peaks, AudioReader};
pub use cache::{CacheKey, FrameCache};
pub use decoder::{DecoderHandle, VideoFrame};
pub use encode::{dnxhd_bitrate, is_hardware_encoder, sequence_file, ColorOut, Fit, MovieWriter, Quality, VideoCodec, WriterSettings};
pub use mixer::{MixClip, Mixer, TrackPeaks, MAX_TRACK_METERS};
pub use probe::probe;
pub use reelforge_codecs as codecs;
pub use source::FrameSource;
pub use tracker::{track_point, track_points, TrackEnd};

use std::sync::Once;

/// Initialise FFmpeg once per process. Safe to call repeatedly.
pub fn init() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        ffmpeg_next::init().expect("failed to initialise FFmpeg");
        // One pool of worker threads, one per core, shared by every clip's
        // decoder (H.264, HEVC, AV1, ProRes, DNx, MPEG-2). Fails harmlessly if
        // something already started it.
        let _ = rayon::ThreadPoolBuilder::new().thread_name(|i| format!("decode-{i}")).build_global();
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
