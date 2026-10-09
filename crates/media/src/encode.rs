//! Writing movie files: RGBA frames + stereo f32 audio in, H.264/AAC (MP4/MOV/MKV) out.
//!
//! The video encoder is chosen from a fallback list, because which H.264
//! encoders exist depends on how FFmpeg was built (GPL builds have libx264;
//! LGPL builds on Windows usually have Media Foundation or a GPU vendor's).

use crate::audio::CHANNELS;
use crate::init;
use anyhow::{anyhow, bail, Context as _, Result};
use ffmpeg_next as ff;
use ff::software::scaling::{context::Context as Scaler, flag::Flags};
use ff::util::color::{Primaries, Range, Space, TransferCharacteristic};
use ff::{ChannelLayout, Dictionary};
use reelforge_core::FrameRate;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Quality {
    /// Visually lossless; big files.
    High,
    #[default]
    Medium,
    /// Small files for sharing drafts.
    Low,
}

impl Quality {
    pub const ALL: [Quality; 3] = [Quality::High, Quality::Medium, Quality::Low];
    pub fn label(self) -> &'static str {
        match self {
            Quality::High => "High",
            Quality::Medium => "Medium",
            Quality::Low => "Low",
        }
    }
    fn crf(self) -> &'static str {
        match self {
            Quality::High => "17",
            Quality::Medium => "21",
            Quality::Low => "27",
        }
    }
    /// Bits per pixel per frame for encoders that need a bitrate instead of CRF.
    fn bits_per_pixel(self) -> f64 {
        match self {
            Quality::High => 0.20,
            Quality::Medium => 0.10,
            Quality::Low => 0.05,
        }
    }
}

#[derive(Clone, Debug)]
pub struct WriterSettings {
    /// Output size. Rounded down to even numbers (required by 4:2:0 video).
    pub width: u32,
    pub height: u32,
    /// Size of the RGBA frames passed to `write_video`, if different from the
    /// output size (they're rescaled, e.g. for a half-resolution export).
    pub input_size: Option<[u32; 2]>,
    pub frame_rate: FrameRate,
    pub quality: Quality,
    /// Audio sample rate, or `None` for a video-only file.
    pub audio_rate: Option<u32>,
}

/// H.264 encoders to try, best first. Hardware ones may be listed by FFmpeg but
/// fail to open without the right GPU/driver; we just move on to the next.
const VIDEO_ENCODERS: &[&str] = &["libx264", "h264_nvenc", "h264_qsv", "h264_amf", "h264_mf", "h264_videotoolbox", "libopenh264", "mpeg4"];

struct VideoOut {
    encoder: ff::encoder::Video,
    stream: usize,
    time_base: ff::Rational,
    scaler: Scaler,
    in_size: (u32, u32),
    frame: ff::frame::Video,
    next_pts: i64,
}

struct AudioOut {
    encoder: ff::encoder::Audio,
    stream: usize,
    time_base: ff::Rational,
    frame_size: usize,
    format: ff::format::Sample,
    pending: Vec<f32>, // interleaved stereo
    next_pts: i64,
}

pub struct MovieWriter {
    octx: ff::format::context::Output,
    video: VideoOut,
    audio: Option<AudioOut>,
    path: PathBuf,
    /// Which video encoder ended up being used (e.g. "libx264").
    pub video_codec: String,
    finished: bool,
}

impl MovieWriter {
    pub fn create(path: &Path, s: &WriterSettings) -> Result<Self> {
        init();
        if s.width < 2 || s.height < 2 {
            bail!("output size {}x{} is too small", s.width, s.height);
        }
        // 4:2:0 needs even dimensions.
        let (w, h) = (s.width & !1, s.height & !1);
        let mut octx = ff::format::output(path).with_context(|| format!("can't create {}", path.display()))?;
        let global_header = octx.format().flags().contains(ff::format::Flags::GLOBAL_HEADER);

        let (encoder, codec, video_codec) = open_video_encoder(w, h, s, global_header)?;
        let video_stream = {
            let mut st = octx.add_stream(codec)?;
            st.set_parameters(&encoder);
            st.set_time_base(encoder.time_base());
            st.index()
        };

        let audio = match s.audio_rate {
            Some(rate) => Some(open_audio_encoder(&mut octx, rate, global_header)?),
            None => None,
        };

        octx.write_header().context("writing container header")?;

        let pix = encoder.format();
        let [iw, ih] = s.input_size.unwrap_or([s.width, s.height]);
        let mut scaler = Scaler::get(ff::format::Pixel::RGBA, iw, ih, pix, w, h, Flags::BICUBIC)?;
        crate::color::configure(&mut scaler, Space::BT709, Range::MPEG, false);
        let time_base = encoder.time_base();
        Ok(Self {
            octx,
            video: VideoOut {
                encoder,
                stream: video_stream,
                time_base,
                scaler,
                in_size: (iw, ih),
                frame: ff::frame::Video::new(pix, w, h),
                next_pts: 0,
            },
            audio,
            path: path.to_owned(),
            video_codec,
            finished: false,
        })
    }

    /// Append one frame of tightly packed sRGB RGBA at the size given in the settings.
    pub fn write_video(&mut self, rgba: &[u8]) -> Result<()> {
        let (w, h) = self.video.in_size;
        if rgba.len() != (w * h * 4) as usize {
            bail!("frame is {} bytes, expected {}x{} RGBA", rgba.len(), w, h);
        }
        let mut src = ff::frame::Video::new(ff::format::Pixel::RGBA, w, h);
        let stride = src.stride(0);
        let row = w as usize * 4;
        let data = src.data_mut(0);
        for y in 0..h as usize {
            data[y * stride..y * stride + row].copy_from_slice(&rgba[y * row..(y + 1) * row]);
        }
        let v = &mut self.video;
        v.scaler.run(&src, &mut v.frame)?;
        v.frame.set_pts(Some(v.next_pts));
        v.frame.set_color_space(Space::BT709);
        v.frame.set_color_range(Range::MPEG);
        v.frame.set_color_primaries(Primaries::BT709);
        v.frame.set_color_transfer_characteristic(TransferCharacteristic::BT709);
        v.next_pts += 1;
        v.encoder.send_frame(&v.frame)?;
        self.drain_video()
    }

    /// Append interleaved stereo samples at the audio rate given in the settings.
    pub fn write_audio(&mut self, interleaved: &[f32]) -> Result<()> {
        let Some(a) = &mut self.audio else { return Ok(()) };
        a.pending.extend_from_slice(interleaved);
        loop {
            let a = self.audio.as_mut().unwrap();
            let n = a.frame_size * CHANNELS;
            if a.pending.len() < n {
                return Ok(());
            }
            let chunk: Vec<f32> = a.pending.drain(..n).collect();
            self.encode_audio_chunk(&chunk)?;
        }
    }

    fn encode_audio_chunk(&mut self, chunk: &[f32]) -> Result<()> {
        let a = self.audio.as_mut().unwrap();
        let n = chunk.len() / CHANNELS;
        let mut frame = ff::frame::Audio::new(a.format, n, ChannelLayout::default(CHANNELS as i32));
        frame.set_rate(a.encoder.rate());
        match a.format {
            ff::format::Sample::F32(ff::format::sample::Type::Planar) => {
                for c in 0..CHANNELS {
                    let plane = frame.plane_mut::<f32>(c);
                    for (i, s) in plane.iter_mut().take(n).enumerate() {
                        *s = chunk[i * CHANNELS + c];
                    }
                }
            }
            ff::format::Sample::F32(ff::format::sample::Type::Packed) => {
                let bytes = frame.data_mut(0);
                for (i, v) in chunk.iter().enumerate() {
                    bytes[i * 4..i * 4 + 4].copy_from_slice(&v.to_ne_bytes());
                }
            }
            ff::format::Sample::I16(ff::format::sample::Type::Planar) => {
                for c in 0..CHANNELS {
                    let plane = frame.plane_mut::<i16>(c);
                    for (i, s) in plane.iter_mut().take(n).enumerate() {
                        *s = (chunk[i * CHANNELS + c].clamp(-1.0, 1.0) * 32767.0) as i16;
                    }
                }
            }
            other => bail!("audio encoder wants unsupported sample format {other:?}"),
        }
        frame.set_pts(Some(a.next_pts));
        a.next_pts += n as i64;
        a.encoder.send_frame(&frame)?;
        self.drain_audio()
    }

    fn drain_video(&mut self) -> Result<()> {
        let v = &mut self.video;
        let ost_tb = self.octx.stream(v.stream).unwrap().time_base();
        let mut packet = ff::Packet::empty();
        while v.encoder.receive_packet(&mut packet).is_ok() {
            // One packet per frame. Without an explicit duration the muxer can't know how
            // long the last frame lasts, and the file's reported frame rate comes out wrong.
            if packet.duration() <= 0 {
                packet.set_duration(1);
            }
            packet.set_stream(v.stream);
            packet.rescale_ts(v.time_base, ost_tb);
            packet.write_interleaved(&mut self.octx)?;
        }
        Ok(())
    }

    fn drain_audio(&mut self) -> Result<()> {
        let Some(a) = &mut self.audio else { return Ok(()) };
        let ost_tb = self.octx.stream(a.stream).unwrap().time_base();
        let mut packet = ff::Packet::empty();
        while a.encoder.receive_packet(&mut packet).is_ok() {
            packet.set_stream(a.stream);
            packet.rescale_ts(a.time_base, ost_tb);
            packet.write_interleaved(&mut self.octx)?;
        }
        Ok(())
    }

    /// Flush encoders and finalise the file. Without this the file is unplayable.
    pub fn finish(mut self) -> Result<PathBuf> {
        self.video.encoder.send_eof()?;
        self.drain_video()?;
        if let Some(a) = &mut self.audio {
            if !a.pending.is_empty() {
                // Pad the last partial frame with silence (< 1 frame, ~21 ms at 48 kHz).
                let mut last = std::mem::take(&mut a.pending);
                last.resize(a.frame_size * CHANNELS, 0.0);
                self.encode_audio_chunk(&last)?;
            }
            self.audio.as_mut().unwrap().encoder.send_eof()?;
            self.drain_audio()?;
        }
        self.octx.write_trailer().context("finalising the file")?;
        self.finished = true;
        Ok(self.path.clone())
    }

    pub fn frames_written(&self) -> i64 {
        self.video.next_pts
    }
}

impl Drop for MovieWriter {
    fn drop(&mut self) {
        if !self.finished {
            // Cancelled or failed: don't leave a broken file behind.
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

fn open_video_encoder(w: u32, h: u32, s: &WriterSettings, global_header: bool) -> Result<(ff::encoder::Video, ff::Codec, String)> {
    let tb = ff::Rational::new(s.frame_rate.0.den as i32, s.frame_rate.0.num as i32);
    let fps = ff::Rational::new(s.frame_rate.0.num as i32, s.frame_rate.0.den as i32);
    let mut errors = Vec::new();
    for name in VIDEO_ENCODERS {
        let Some(codec) = ff::encoder::find_by_name(name) else { continue };
        let Ok(vcodec) = codec.video() else { continue };
        // Prefer yuv420p (plays everywhere); otherwise the encoder's first format (e.g. nv12 for hardware).
        let formats: Vec<ff::format::Pixel> = vcodec.formats().map(|f| f.collect()).unwrap_or_default();
        let pix = if formats.is_empty() || formats.contains(&ff::format::Pixel::YUV420P) {
            ff::format::Pixel::YUV420P
        } else {
            formats[0]
        };

        let mut enc = ff::codec::context::Context::new_with_codec(codec).encoder().video()?;
        enc.set_width(w);
        enc.set_height(h);
        enc.set_format(pix);
        enc.set_time_base(tb);
        enc.set_frame_rate(Some(fps));
        enc.set_gop((s.frame_rate.fps() * 2.0).round().max(1.0) as u32); // keyframe every ~2 s: easy scrubbing
        enc.set_colorspace(Space::BT709);
        enc.set_color_range(Range::MPEG);
        enc.set_color_primaries(Primaries::BT709);
        enc.set_color_transfer_characteristic(TransferCharacteristic::BT709);
        // Closed GOPs: every keyframe is a clean random-access point, so files
        // seek cleanly in editors (including this one) and play everywhere.
        let mut flags = ff::codec::Flags::CLOSED_GOP;
        if global_header {
            flags |= ff::codec::Flags::GLOBAL_HEADER;
        }
        enc.set_flags(flags);

        let mut opts = Dictionary::new();
        if *name == "libx264" {
            opts.set("crf", s.quality.crf());
            opts.set("preset", "medium");
            opts.set("profile", "high");
        } else {
            let bitrate = (w as f64 * h as f64 * s.frame_rate.fps() * s.quality.bits_per_pixel()) as usize;
            enc.set_bit_rate(bitrate.max(500_000));
        }
        match enc.open_with(opts) {
            Ok(opened) => {
                log::info!("video encoder: {name} ({pix:?})");
                return Ok((opened, codec, name.to_string()));
            }
            Err(e) => errors.push(format!("{name}: {e}")),
        }
    }
    Err(anyhow!(
        "no usable H.264/MPEG-4 video encoder in this FFmpeg build ({})",
        if errors.is_empty() { "none found".into() } else { errors.join("; ") }
    ))
}

fn open_audio_encoder(octx: &mut ff::format::context::Output, rate: u32, global_header: bool) -> Result<AudioOut> {
    let codec = ff::encoder::find(ff::codec::Id::AAC).ok_or_else(|| anyhow!("no AAC encoder in this FFmpeg build"))?;
    let acodec = codec.audio()?;
    let formats: Vec<ff::format::Sample> = acodec.formats().map(|f| f.collect()).unwrap_or_default();
    let fltp = ff::format::Sample::F32(ff::format::sample::Type::Planar);
    let format = if formats.is_empty() || formats.contains(&fltp) { fltp } else { formats[0] };

    let mut enc = ff::codec::context::Context::new_with_codec(codec).encoder().audio()?;
    enc.set_rate(rate as i32);
    enc.set_format(format);
    enc.set_channel_layout(ChannelLayout::default(CHANNELS as i32));
    enc.set_time_base(ff::Rational::new(1, rate as i32));
    enc.set_bit_rate(192_000);
    if global_header {
        enc.set_flags(ff::codec::Flags::GLOBAL_HEADER);
    }
    let encoder = enc.open_as(codec).context("opening AAC encoder")?;
    let mut st = octx.add_stream(codec)?;
    st.set_parameters(&encoder);
    st.set_time_base(ff::Rational::new(1, rate as i32));
    // 0 means "any size" (e.g. PCM); use the AAC-standard 1024 then.
    let frame_size = match encoder.frame_size() as usize {
        0 => 1024,
        n => n.min(1 << 16),
    };
    Ok(AudioOut {
        stream: st.index(),
        time_base: ff::Rational::new(1, rate as i32),
        frame_size,
        format,
        pending: Vec::new(),
        next_pts: 0,
        encoder,
    })
}
