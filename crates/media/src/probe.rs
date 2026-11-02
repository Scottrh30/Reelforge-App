use crate::{init, to_rational, AV_TIME_BASE};
use anyhow::{Context, Result};
use ffmpeg_next as ff;
use reelforge_core::{AudioStreamInfo, FrameRate, MediaInfo, Rational, Time, VideoStreamInfo};
use std::path::Path;

/// Open a file just long enough to read its streams and duration.
pub fn probe(path: &Path) -> Result<MediaInfo> {
    init();
    let ictx = crate::open_input(path).with_context(|| format!("cannot open {}", path.display()))?;

    let video = match ictx.streams().best(ff::media::Type::Video) {
        Some(st) => {
            let ctx = ff::codec::context::Context::from_parameters(st.parameters())?;
            let codec = ctx.id().name().to_owned();
            let dec = ctx.decoder().video()?;
            let mut rate = to_rational(st.avg_frame_rate());
            if rate.num <= 0 {
                rate = to_rational(st.rate());
            }
            if rate.num <= 0 {
                rate = Rational::new(30, 1); // still images etc.
            }
            Some(VideoStreamInfo {
                width: dec.width(),
                height: dec.height(),
                frame_rate: FrameRate(rate),
                codec,
                time_base: to_rational(st.time_base()),
                // SAFETY: reading a field of the stream's parameters.
                hdr: match unsafe { (*st.parameters().as_ptr()).color_trc } {
                    ff::ffi::AVColorTransferCharacteristic::AVCOL_TRC_SMPTE2084 => Some("PQ".into()),
                    ff::ffi::AVColorTransferCharacteristic::AVCOL_TRC_ARIB_STD_B67 => Some("HLG".into()),
                    _ => None,
                },
            })
        }
        None => None,
    };

    let audio = match ictx.streams().best(ff::media::Type::Audio) {
        Some(st) => {
            let ctx = ff::codec::context::Context::from_parameters(st.parameters())?;
            let codec = ctx.id().name().to_owned();
            let dec = ctx.decoder().audio()?;
            Some(AudioStreamInfo { sample_rate: dec.rate(), channels: dec.channels(), codec })
        }
        None => None,
    };

    let duration = if ictx.duration() > 0 {
        Time::from_pts(ictx.duration(), Rational::new(1, AV_TIME_BASE))
    } else {
        Time::ZERO
    };

    Ok(MediaInfo { duration, video, audio, container: ictx.format().name().to_owned() })
}
