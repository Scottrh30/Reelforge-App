//! FFmpeg-backed media layer: probing files and decoding video on worker threads.
//!
//! The UI never touches FFmpeg directly. It asks a [`FrameSource`] for "the frame
//! at time t" every repaint, and the source keeps a decoder thread running ahead.

// Never crash: no unwrap, expect, panic, unreachable, todo or unimplemented
// outside tests (a damaged file or an odd setting is an error, not a crash).
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

// (as reelforge-core: numeric code indexing several arrays in step, plain
// argument lists, nested types spelled out)
#![allow(clippy::needless_range_loop, clippy::too_many_arguments, clippy::type_complexity)]

pub mod audio;
pub mod audiofx;
pub mod cache;
pub mod camraw;
pub mod color;
pub mod decoder;
pub mod encode;
pub mod hw;
#[cfg(windows)]
mod zerocopy;
pub mod mixer;
pub mod native;
pub mod nvof;
pub mod nvfx;
pub mod probe;
pub mod prores_raw;
pub mod selfcheck;
pub mod proxy;
pub mod rtxvideo;
pub mod source;
pub mod tracker;

pub use audio::{audio_window, waveform_peaks, AudioReader};
pub use cache::{CacheKey, FrameCache};
pub use decoder::{DecoderHandle, VideoFrame};
pub use encode::{dnxhd_bitrate, is_hardware_encoder, sequence_file, ColorOut, Fit, MovieWriter, Quality, VideoCodec, WriterSettings};
pub use mixer::{MixClip, Mixer, TrackPeaks, MAX_TRACK_METERS};
pub use probe::probe;
pub use reelforge_codecs as codecs;
pub use source::FrameSource;
pub use tracker::{frame_means, track_outline, track_outline_live, track_point, track_points, track_points_live, track_scene, TrackEnd};

use std::path::Path;
use std::sync::Once;

/// Initialise FFmpeg once per process. Safe to call repeatedly.
/// Read the next packet of `input` into `packet`, which is emptied first.
///
/// (`ff::Packet::read` hands the packet straight to `av_read_frame`, which
/// expects an empty one: reading into a packet still holding the last one
/// loses that one's memory. Reused packets go through here.)
pub(crate) fn read_packet(packet: &mut ffmpeg_next::Packet, input: &mut ffmpeg_next::format::context::Input) -> Result<(), ffmpeg_next::Error> {
    *packet = ffmpeg_next::Packet::empty();
    packet.read(input)
}

/// Open media for reading: a file, or an image sequence path (see
/// `reelforge_core::imageseq`), read with FFmpeg's image-sequence reader from
/// its first frame at its rate.
pub fn open_input(path: &std::path::Path) -> Result<ffmpeg_next::format::context::Input, ffmpeg_next::Error> {
    // R3D and ARRIRAW are read through the file their maker's tool converted them to.
    if let Some(converted) = camraw::stand_in(path) {
        return ffmpeg_next::format::input(&converted);
    }
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
        // (if it can't, decoding and encoding fail with their own errors later)
        if let Err(e) = ffmpeg_next::init() {
            log::error!("FFmpeg didn't start: {e}");
        }
        // One pool of worker threads, one per core, shared by every clip's
        // decoder (H.264, HEVC, AV1, ProRes, DNx, MPEG-2). Fails harmlessly if
        // something already started it.
        let _ = rayon::ThreadPoolBuilder::new().thread_name(|i| format!("decode-{i}")).build_global();
        ffmpeg_next::util::log::set_level(ffmpeg_next::util::log::Level::Error);
        reelforge_core::lut::set_plate_loader(load_plate);
        reelforge_core::environment::set_env_loader(load_environment);
    });
}

/// A difference key's clean plate: the frame of `path` at media time `at`, as RGBA8.
fn load_plate(path: &Path, at: reelforge_core::Time) -> Option<(u32, u32, Vec<u8>)> {
    let mut src = source::FrameSource::open(path, None).ok()?;
    let f = src.frame_at_blocking(at, std::time::Duration::from_secs(20))?;
    Some((f.width, f.height, f.pixels.to_rgba().to_vec()))
}

/// An environment light's picture (OpenEXR, or an ordinary picture) as linear-light RGB.
fn load_environment(path: &Path) -> Option<(u32, u32, Vec<f32>)> {
    use reelforge_core::frame::{f16_to_f32, Pixels};
    let mut src = source::FrameSource::open(path, None).ok()?;
    let f = src.frame_at_blocking(reelforge_core::Time::ZERO, std::time::Duration::from_secs(30))?;
    let rgb = match &f.pixels {
        Pixels::Float(p) => p
            .rgba
            .chunks_exact(4)
            .flat_map(|q| {
                let a = f16_to_f32(q[3]);
                let un = |v: u16| if a > 1e-6 && a < 1.0 { f16_to_f32(v) / a } else { f16_to_f32(v) };
                [un(q[0]), un(q[1]), un(q[2])]
            })
            .collect(),
        other => {
            let lin = |c: u8| {
                let c = c as f32 / 255.0;
                if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
            };
            other.to_rgba().chunks_exact(4).flat_map(|q| [lin(q[0]), lin(q[1]), lin(q[2])]).collect()
        }
    };
    Some((f.width, f.height, rgb))
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
