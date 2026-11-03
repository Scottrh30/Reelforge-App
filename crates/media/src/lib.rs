//! FFmpeg-backed media layer: probing files and decoding video on worker threads.
//!
//! The UI never touches FFmpeg directly. It asks a [`FrameSource`] for "the frame
//! at time t" every repaint, and the source keeps a decoder thread running ahead.

pub mod audio;
pub mod audiofx;
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
pub mod selfcheck;
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
pub use tracker::{track_outline, track_point, track_points, track_scene, TrackEnd};

use std::path::Path;
use std::sync::Once;

/// Initialise FFmpeg once per process. Safe to call repeatedly.
/// Open media for reading: a file, or an image sequence path (see
/// `reelforge_core::imageseq`), read with FFmpeg's image-sequence reader from
/// its first frame at its rate.
pub fn open_input(path: &std::path::Path) -> Result<ffmpeg_next::format::context::Input, ffmpeg_next::Error> {
    if reelforge_core::imageseq::is_sequence(path) {
        let Some(seq) = reelforge_core::imageseq::parse(path) else { return Err(ffmpeg_next::Error::Other { errno: 2 }) };
        let mut opts = ffmpeg_next::Dictionary::new();
        opts.set("start_number", &seq.first.to_string());
        opts.set("framerate", &format!("{}/{}", seq.rate.num, seq.rate.den));
        return ffmpeg_next::format::input_with_dictionary(&seq.pattern(), opts);
    }
    ffmpeg_next::format::input(&path)
}

pub fn init() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        ffmpeg_next::init().expect("failed to initialise FFmpeg");
        // One pool of worker threads, one per core, shared by every clip's
        // decoder (H.264, HEVC, AV1, ProRes, DNx, MPEG-2). Fails harmlessly if
        // something already started it.
        let _ = rayon::ThreadPoolBuilder::new().thread_name(|i| format!("decode-{i}")).build_global();
        ffmpeg_next::util::log::set_level(ffmpeg_next::util::log::Level::Error);
        reelforge_core::lut::set_plate_loader(load_plate);
    });
}

/// A difference key's clean plate: the frame of `path` at media time `at`, as RGBA8.
fn load_plate(path: &Path, at: reelforge_core::Time) -> Option<(u32, u32, Vec<u8>)> {
    let mut src = source::FrameSource::open(path, None).ok()?;
    let f = src.frame_at_blocking(at, std::time::Duration::from_secs(20))?;
    Some((f.width, f.height, f.pixels.to_rgba().to_vec()))
}

/// FFmpeg's version and licence, as the loaded libraries report them.
pub fn ffmpeg_version() -> String {
    init();
    // SAFETY: both return static NUL-terminated strings.
    unsafe {
        let v = std::ffi::CStr::from_ptr(ffmpeg_next::ffi::av_version_info()).to_string_lossy().into_owned();
        let l = std::ffi::CStr::from_ptr(ffmpeg_next::ffi::avcodec_license()).to_string_lossy().into_owned();
        let c = ffmpeg_next::ffi::avcodec_version();
        format!("{v} (libavcodec {}.{}.{}, {l})", c >> 16, (c >> 8) & 0xFF, c & 0xFF)
    }
}

/// How many worker threads the shared decode pool has.
pub fn decode_threads() -> usize {
    init();
    rayon::current_num_threads()
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
