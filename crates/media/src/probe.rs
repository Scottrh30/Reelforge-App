use crate::{init, to_rational, AV_TIME_BASE};
use anyhow::{Context, Result};
use ffmpeg_next as ff;
use reelforge_core::{AudioStreamInfo, FrameRate, MediaInfo, Rational, Time, VideoStreamInfo};
use std::path::Path;

/// A stream's pixel shape, reduced (square when it doesn't say or says 1:1);
/// the codec's own, else the container's.
fn pixel_aspect(codec: ff::ffi::AVRational, stream: ff::ffi::AVRational) -> (u32, u32) {
    let pick = |n: i32, d: i32| (n > 0 && d > 0 && n != d).then_some((n as u32, d as u32));
    let Some((n, d)) = pick(codec.num, codec.den).or_else(|| pick(stream.num, stream.den)) else { return (1, 1) };
    let g = gcd(n, d);
    (n / g, d / g)
}

fn gcd(a: u32, b: u32) -> u32 {
    if b == 0 { a.max(1) } else { gcd(b, a % b) }
}

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
                // SAFETY: reading a field of the stream's parameters.
                pixel_aspect: pixel_aspect(unsafe { (*st.parameters().as_ptr()).sample_aspect_ratio }, unsafe { (*st.as_ptr()).sample_aspect_ratio }),
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

    let mut duration = if ictx.duration() > 0 {
        Time::from_pts(ictx.duration(), Rational::new(1, AV_TIME_BASE))
    } else {
        Time::ZERO
    };
    // A single picture (a one-frame GIF, an AVIF or HEIC still) that comes
    // with a one-frame "duration" is a still: no length of its own, so its
    // clips can be made as long as wanted.
    if let (Some(v), None) = (&video, &audio) {
        let one_frame = ictx.streams().best(ff::media::Type::Video).is_some_and(|st| st.frames() == 1);
        let frame = v.frame_rate.frame_duration();
        if duration > Time::ZERO && (one_frame || duration.0 <= frame.0 + frame.0 / 2) {
            duration = Time::ZERO;
        }
    }

    Ok(MediaInfo { duration, video, audio, container: ictx.format().name().to_owned() })
}

#[cfg(test)]
mod tests {
    /// A one-frame GIF (which FFmpeg gives a one-frame length) is a still;
    /// a moving GIF isn't.
    #[test]
    fn single_pictures_are_stills() {
        let dir = std::env::temp_dir().join(format!("rf-probe-still-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let make = |name: &str, frames: &str| {
            let p = dir.join(name);
            let ok = std::process::Command::new("ffmpeg")
                .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i", "testsrc2=size=64x48:rate=25", "-frames:v", frames])
                .arg(&p)
                .status()
                .is_ok_and(|s| s.success());
            ok.then_some(p)
        };
        let (Some(one), Some(many), Some(png)) = (make("one.gif", "1"), make("many.gif", "10"), make("one.png", "1")) else { return };
        assert_eq!(super::probe(&one).unwrap().duration, reelforge_core::Time::ZERO);
        assert_eq!(super::probe(&png).unwrap().duration, reelforge_core::Time::ZERO);
        assert!(super::probe(&many).unwrap().duration > reelforge_core::Time::from_secs_f64(0.3));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
