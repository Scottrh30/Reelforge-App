//! Writing media files: RGBA frames + stereo f32 audio in; out come
//! H.264 or H.265 (MP4/MKV/MOV), ProRes and DNxHR masters (MOV), PNG/JPEG/TIFF
//! image sequences, or audio alone (WAV).
//!
//! For H.264/H.265 the encoder is chosen from a fallback list, because which
//! encoders exist depends on how FFmpeg was built and on the graphics card:
//! GPU encoders (NVIDIA NVENC, Intel Quick Sync, AMD AMF, Windows Media
//! Foundation, Apple VideoToolbox) are tried first when hardware encoding is
//! asked for, and silently skipped if they fail to open.

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

/// What kind of video to write.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum VideoCodec {
    #[default]
    H264,
    H265,
    ProRes422,
    ProRes422Hq,
    DnxhrSq,
    DnxhrHq,
    PngSequence,
    JpegSequence,
    TiffSequence,
}

impl VideoCodec {
    pub const ALL: [VideoCodec; 9] = [
        VideoCodec::H264,
        VideoCodec::H265,
        VideoCodec::ProRes422,
        VideoCodec::ProRes422Hq,
        VideoCodec::DnxhrSq,
        VideoCodec::DnxhrHq,
        VideoCodec::PngSequence,
        VideoCodec::JpegSequence,
        VideoCodec::TiffSequence,
    ];

    pub fn label(self) -> &'static str {
        match self {
            VideoCodec::H264 => "H.264 (MP4)",
            VideoCodec::H265 => "H.265 / HEVC (MP4)",
            VideoCodec::ProRes422 => "ProRes 422 (MOV)",
            VideoCodec::ProRes422Hq => "ProRes 422 HQ (MOV)",
            VideoCodec::DnxhrSq => "DNxHR SQ (MOV)",
            VideoCodec::DnxhrHq => "DNxHR HQ (MOV)",
            VideoCodec::PngSequence => "PNG image sequence",
            VideoCodec::JpegSequence => "JPEG image sequence",
            VideoCodec::TiffSequence => "TIFF image sequence",
        }
    }

    /// The usual file extension.
    pub fn extension(self) -> &'static str {
        match self {
            VideoCodec::H264 | VideoCodec::H265 => "mp4",
            VideoCodec::ProRes422 | VideoCodec::ProRes422Hq | VideoCodec::DnxhrSq | VideoCodec::DnxhrHq => "mov",
            VideoCodec::PngSequence => "png",
            VideoCodec::JpegSequence => "jpg",
            VideoCodec::TiffSequence => "tif",
        }
    }

    /// One image file per frame.
    pub fn is_sequence(self) -> bool {
        matches!(self, VideoCodec::PngSequence | VideoCodec::JpegSequence | VideoCodec::TiffSequence)
    }

    /// Has a quality setting (the others are fixed-quality by design).
    pub fn uses_quality(self) -> bool {
        matches!(self, VideoCodec::H264 | VideoCodec::H265 | VideoCodec::JpegSequence)
    }

    /// Can use a GPU encoder.
    pub fn has_hardware_encoders(self) -> bool {
        matches!(self, VideoCodec::H264 | VideoCodec::H265)
    }

    /// Intermediate/master codecs that keep audio uncompressed.
    fn wants_pcm_audio(self) -> bool {
        matches!(self, VideoCodec::ProRes422 | VideoCodec::ProRes422Hq | VideoCodec::DnxhrSq | VideoCodec::DnxhrHq)
    }

    /// Software and hardware encoders for this codec, in the order to try them.
    fn encoders(self, hardware_first: bool) -> Vec<&'static str> {
        let (sw, hw): (&[&str], &[&str]) = match self {
            VideoCodec::H264 => (&["libx264", "libopenh264"], &["h264_nvenc", "h264_qsv", "h264_amf", "h264_mf", "h264_videotoolbox"]),
            VideoCodec::H265 => (&["libx265"], &["hevc_nvenc", "hevc_qsv", "hevc_amf", "hevc_mf", "hevc_videotoolbox"]),
            VideoCodec::ProRes422 | VideoCodec::ProRes422Hq => (&["prores_ks", "prores_aw", "prores"], &[]),
            VideoCodec::DnxhrSq | VideoCodec::DnxhrHq => (&["dnxhd"], &[]),
            VideoCodec::PngSequence => (&["png"], &[]),
            VideoCodec::JpegSequence => (&["mjpeg"], &[]),
            VideoCodec::TiffSequence => (&["tiff"], &[]),
        };
        let mut v: Vec<&str> = if hardware_first { hw.iter().chain(sw).copied().collect() } else { sw.iter().chain(hw).copied().collect() };
        if self == VideoCodec::H264 {
            v.push("mpeg4"); // last resort: plays everywhere, if not efficiently
        }
        v
    }

    /// The pixel format to encode in.
    fn pixel(self) -> Option<ff::format::Pixel> {
        use ff::format::Pixel as P;
        match self {
            VideoCodec::ProRes422 | VideoCodec::ProRes422Hq => Some(P::YUV422P10LE),
            VideoCodec::DnxhrSq | VideoCodec::DnxhrHq => Some(P::YUV422P),
            VideoCodec::PngSequence | VideoCodec::TiffSequence => Some(P::RGB24),
            VideoCodec::JpegSequence => Some(P::YUVJ420P),
            VideoCodec::H264 | VideoCodec::H265 => None, // 4:2:0, or what a GPU encoder wants
        }
    }
}

/// Is this encoder name a GPU one?
pub fn is_hardware_encoder(name: &str) -> bool {
    ["nvenc", "_qsv", "_amf", "_mf", "videotoolbox", "_vaapi"].iter().any(|k| name.contains(k))
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
    /// Make an editing proxy: every frame a keyframe (instant seeking) and the
    /// fastest encoder settings, at the expense of file size.
    pub proxy: bool,
    /// The video to write, or `None` for an audio-only file (WAV).
    pub video: Option<VideoCodec>,
    /// Try GPU encoders before software ones.
    pub hardware: bool,
    /// What to do when the input's shape differs from the output's.
    pub fit: Fit,
}

/// How a picture goes into a frame of a different shape.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Fit {
    /// Show all of it, with black bars at the sides or top and bottom.
    #[default]
    Letterbox,
    /// Fill the frame, cutting off the edges that don't fit.
    Crop,
    /// Squash or stretch it to the new shape.
    Stretch,
}

impl Fit {
    pub const ALL: [Fit; 3] = [Fit::Letterbox, Fit::Crop, Fit::Stretch];
    pub fn label(self) -> &'static str {
        match self {
            Fit::Letterbox => "Fit (black bars)",
            Fit::Crop => "Fill (crop edges)",
            Fit::Stretch => "Stretch",
        }
    }
}

/// Where the input sits on the canvas that gets scaled to the output: the
/// canvas size and the input's offset in it (negative when cropping).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Canvas {
    w: u32,
    h: u32,
    x: i32,
    y: i32,
}

fn canvas(fit: Fit, iw: u32, ih: u32, w: u32, h: u32) -> Option<Canvas> {
    let (in_aspect, out_aspect) = (iw as f64 / ih.max(1) as f64, w as f64 / h.max(1) as f64);
    // Leave it alone when the shapes differ by less than half a percent.
    if fit == Fit::Stretch || (in_aspect / out_aspect - 1.0).abs() <= 0.005 {
        return None;
    }
    // Letterbox grows the canvas around the input; crop shrinks it inside.
    let wider = (out_aspect > in_aspect) == (fit == Fit::Letterbox);
    let c = if wider {
        let cw = (((ih as f64 * out_aspect).round() as u32 + 1) & !1).max(2);
        Canvas { w: cw, h: ih, x: (cw as i32 - iw as i32) / 2, y: 0 }
    } else {
        let ch = (((iw as f64 / out_aspect).round() as u32 + 1) & !1).max(2);
        Canvas { w: iw, h: ch, x: 0, y: (ch as i32 - ih as i32) / 2 }
    };
    Some(c)
}

impl Default for WriterSettings {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            input_size: None,
            frame_rate: FrameRate::new(30, 1),
            quality: Quality::Medium,
            audio_rate: None,
            proxy: false,
            video: Some(VideoCodec::H264),
            hardware: false,
            fit: Fit::Letterbox,
        }
    }
}

struct VideoOut {
    encoder: ff::encoder::Video,
    stream: usize,
    time_base: ff::Rational,
    scaler: Scaler,
    in_size: (u32, u32),
    /// Letterboxing or cropping (see [`Canvas`]).
    pad: Option<Canvas>,
    frame: ff::frame::Video,
    next_pts: i64,
    range: Range,
    yuv: bool,
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
    video: Option<VideoOut>,
    audio: Option<AudioOut>,
    path: PathBuf,
    /// Which video encoder ended up being used (e.g. "libx264", "h264_nvenc"),
    /// or the audio one for an audio-only file.
    pub video_codec: String,
    sequence: bool,
    finished: bool,
}

/// The file name of frame `n` (from 1) of an image sequence written to `pattern` ("…%05d.png").
pub fn sequence_file(pattern: &Path, n: u64) -> PathBuf {
    PathBuf::from(pattern.to_string_lossy().replace("%05d", &format!("{n:05}")))
}

impl MovieWriter {
    pub fn create(path: &Path, s: &WriterSettings) -> Result<Self> {
        init();
        if s.video.is_some() && (s.width < 2 || s.height < 2) {
            bail!("output size {}x{} is too small", s.width, s.height);
        }
        let sequence = s.video.is_some_and(|v| v.is_sequence());
        if s.video.is_none() && s.audio_rate.is_none() {
            bail!("nothing to write: no video and no audio");
        }
        // 4:2:0 needs even dimensions.
        let (w, h) = (s.width & !1, s.height & !1);
        let mut octx = ff::format::output(path).with_context(|| format!("can't create {}", path.display()))?;
        let global_header = octx.format().flags().contains(ff::format::Flags::GLOBAL_HEADER);
        if sequence {
            // The image-sequence muxer writes each numbered file itself; the file opened
            // for the pattern name ("…%05d.png") isn't used, so close and remove it.
            // SAFETY: the context is valid and nothing has been written through `pb`.
            unsafe {
                ff::ffi::avio_closep(&mut (*octx.as_mut_ptr()).pb);
            }
            let _ = std::fs::remove_file(path);
        }

        let mut video_codec = String::new();
        let video = match s.video {
            Some(kind) => {
                let (encoder, codec, name) = open_video_encoder(kind, w, h, s, global_header)?;
                video_codec = name;
                let stream = {
                    let mut st = octx.add_stream(codec)?;
                    st.set_parameters(&encoder);
                    st.set_time_base(encoder.time_base());
                    st.index()
                };
                let pix = encoder.format();
                let yuv = !matches!(pix, ff::format::Pixel::RGB24 | ff::format::Pixel::RGBA);
                let range = if pix == ff::format::Pixel::YUVJ420P { Range::JPEG } else { Range::MPEG };
                let [iw, ih] = s.input_size.unwrap_or([s.width, s.height]);
                let pad = canvas(s.fit, iw, ih, w, h);
                let (sw, sh) = pad.map_or((iw, ih), |c| (c.w, c.h));
                let mut scaler = Scaler::get(ff::format::Pixel::RGBA, sw, sh, pix, w, h, Flags::BICUBIC)?;
                if yuv {
                    crate::color::configure(&mut scaler, Space::BT709, range, false);
                }
                let time_base = encoder.time_base();
                Some(VideoOut { encoder, stream, time_base, scaler, in_size: (iw, ih), pad, frame: ff::frame::Video::new(pix, w, h), next_pts: 0, range, yuv })
            }
            None => None,
        };

        let pcm = s.video.is_none_or(|v| v.wants_pcm_audio());
        let audio = match s.audio_rate {
            Some(rate) if !sequence => Some(open_audio_encoder(&mut octx, rate, global_header, pcm)?),
            _ => None,
        };
        if video.is_none() {
            video_codec = audio.as_ref().map(|a| a.encoder.codec().map(|c| c.name().to_string()).unwrap_or_default()).unwrap_or_default();
        }

        octx.write_header().context("writing container header")?;
        Ok(Self { octx, video, audio, path: path.to_owned(), video_codec, sequence, finished: false })
    }

    /// Append one frame of tightly packed sRGB RGBA at the size given in the settings.
    pub fn write_video(&mut self, rgba: &[u8]) -> Result<()> {
        let Some(v) = &mut self.video else { return Ok(()) };
        let (w, h) = v.in_size;
        if rgba.len() != (w * h * 4) as usize {
            bail!("frame is {} bytes, expected {}x{} RGBA", rgba.len(), w, h);
        }
        let c = v.pad.unwrap_or(Canvas { w, h, x: 0, y: 0 });
        let mut src = ff::frame::Video::new(ff::format::Pixel::RGBA, c.w, c.h);
        let stride = src.stride(0);
        let data = src.data_mut(0);
        if c.x > 0 || c.y > 0 {
            // Black bars, opaque.
            for y in 0..c.h as usize {
                for px in data[y * stride..y * stride + c.w as usize * 4].chunks_exact_mut(4) {
                    px.copy_from_slice(&[0, 0, 0, 255]);
                }
            }
        }
        // The part of the input that lands on the canvas.
        let (x0, x1) = ((-c.x).max(0) as usize, (c.w as i32 - c.x).min(w as i32) as usize);
        let in_row = w as usize * 4;
        for cy in 0..c.h as i32 {
            let y = cy - c.y;
            if y < 0 || y >= h as i32 {
                continue;
            }
            let at = cy as usize * stride + (x0 as i32 + c.x) as usize * 4;
            let from = y as usize * in_row;
            data[at..at + (x1 - x0) * 4].copy_from_slice(&rgba[from + x0 * 4..from + x1 * 4]);
        }
        v.scaler.run(&src, &mut v.frame)?;
        v.frame.set_pts(Some(v.next_pts));
        if v.yuv {
            v.frame.set_color_space(Space::BT709);
            v.frame.set_color_range(v.range);
            v.frame.set_color_primaries(Primaries::BT709);
            v.frame.set_color_transfer_characteristic(TransferCharacteristic::BT709);
        }
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
        use ff::format::sample::Type::{Packed, Planar};
        use ff::format::Sample as S;
        let i16s = |v: f32| (v.clamp(-1.0, 1.0) * 32767.0).round() as i16;
        // 24-bit PCM is carried in the top 24 bits of 32-bit samples.
        let i32s = |v: f32| ((v.clamp(-1.0, 1.0) as f64 * 8_388_607.0).round() as i32) << 8;
        match a.format {
            S::F32(Planar) => {
                for c in 0..CHANNELS {
                    let plane = frame.plane_mut::<f32>(c);
                    for (i, s) in plane.iter_mut().take(n).enumerate() {
                        *s = chunk[i * CHANNELS + c];
                    }
                }
            }
            S::F32(Packed) => {
                let bytes = frame.data_mut(0);
                for (i, v) in chunk.iter().enumerate() {
                    bytes[i * 4..i * 4 + 4].copy_from_slice(&v.to_ne_bytes());
                }
            }
            S::I16(Planar) => {
                for c in 0..CHANNELS {
                    let plane = frame.plane_mut::<i16>(c);
                    for (i, s) in plane.iter_mut().take(n).enumerate() {
                        *s = i16s(chunk[i * CHANNELS + c]);
                    }
                }
            }
            S::I16(Packed) => {
                let bytes = frame.data_mut(0);
                for (i, v) in chunk.iter().enumerate() {
                    bytes[i * 2..i * 2 + 2].copy_from_slice(&i16s(*v).to_ne_bytes());
                }
            }
            S::I32(Packed) => {
                let bytes = frame.data_mut(0);
                for (i, v) in chunk.iter().enumerate() {
                    bytes[i * 4..i * 4 + 4].copy_from_slice(&i32s(*v).to_ne_bytes());
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
        let Some(v) = &mut self.video else { return Ok(()) };
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
        if let Some(v) = &mut self.video {
            v.encoder.send_eof()?;
            self.drain_video()?;
        }
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
        self.video.as_ref().map_or(0, |v| v.next_pts)
    }
}

impl Drop for MovieWriter {
    fn drop(&mut self) {
        if !self.finished {
            // Cancelled or failed: don't leave a broken file behind. Close the file
            // first: Windows can't delete a file that's still open. (The context's
            // own drop later sees a null handle and skips closing it again.)
            unsafe {
                let ctx = self.octx.as_mut_ptr();
                if !ctx.is_null() {
                    ff::ffi::avio_closep(&mut (*ctx).pb);
                }
            }
            if self.sequence {
                for n in 1..=self.frames_written().max(0) as u64 + 1 {
                    let _ = std::fs::remove_file(sequence_file(&self.path, n));
                }
            } else {
                let _ = std::fs::remove_file(&self.path);
            }
        }
    }
}

fn open_video_encoder(kind: VideoCodec, w: u32, h: u32, s: &WriterSettings, global_header: bool) -> Result<(ff::encoder::Video, ff::Codec, String)> {
    let tb = ff::Rational::new(s.frame_rate.0.den as i32, s.frame_rate.0.num as i32);
    let fps = ff::Rational::new(s.frame_rate.0.num as i32, s.frame_rate.0.den as i32);
    let mut errors = Vec::new();
    for name in kind.encoders(s.hardware && !s.proxy) {
        let Some(codec) = ff::encoder::find_by_name(name) else { continue };
        let Ok(vcodec) = codec.video() else { continue };
        let formats: Vec<ff::format::Pixel> = vcodec.formats().map(|f| f.collect()).unwrap_or_default();
        let pix = match kind.pixel() {
            Some(p) => p,
            // Prefer yuv420p (plays everywhere); otherwise the encoder's first format (e.g. nv12 for hardware).
            None if formats.is_empty() || formats.contains(&ff::format::Pixel::YUV420P) => ff::format::Pixel::YUV420P,
            None => formats[0],
        };
        if !formats.is_empty() && !formats.contains(&pix) {
            errors.push(format!("{name}: no {pix:?} support"));
            continue;
        }

        let mut enc = ff::codec::context::Context::new_with_codec(codec).encoder().video()?;
        enc.set_width(w);
        enc.set_height(h);
        enc.set_format(pix);
        enc.set_time_base(tb);
        enc.set_frame_rate(Some(fps));
        let mut flags = ff::codec::Flags::empty();
        if matches!(kind, VideoCodec::H264 | VideoCodec::H265) {
            // Keyframe every ~2 s for easy scrubbing; every frame for proxies.
            enc.set_gop(if s.proxy { 1 } else { (s.frame_rate.fps() * 2.0).round().max(1.0) as u32 });
            if s.proxy {
                enc.set_max_b_frames(0);
            }
            // Closed GOPs: every keyframe is a clean random-access point, so files
            // seek cleanly in editors (including this one) and play everywhere.
            flags |= ff::codec::Flags::CLOSED_GOP;
        }
        if !matches!(pix, ff::format::Pixel::RGB24) {
            enc.set_colorspace(Space::BT709);
            enc.set_color_range(if pix == ff::format::Pixel::YUVJ420P { Range::JPEG } else { Range::MPEG });
            enc.set_color_primaries(Primaries::BT709);
            enc.set_color_transfer_characteristic(TransferCharacteristic::BT709);
        }
        if global_header {
            flags |= ff::codec::Flags::GLOBAL_HEADER;
        }

        let mut opts = Dictionary::new();
        let bitrate = |bpp: f64| ((w as f64 * h as f64 * s.frame_rate.fps() * bpp) as usize).max(500_000);
        match (kind, name) {
            (_, "libx264") if s.proxy => {
                opts.set("crf", "23");
                opts.set("preset", "veryfast");
                opts.set("tune", "fastdecode");
            }
            (_, "libx264") => {
                opts.set("crf", s.quality.crf());
                opts.set("preset", "medium");
                opts.set("profile", "high");
            }
            (_, "libx265") => {
                // HEVC needs ~4 less CRF for the same look as H.264, at about half the size.
                let crf: u32 = s.quality.crf().parse::<u32>().unwrap_or(21) + 4;
                opts.set("crf", &crf.to_string());
                opts.set("preset", "medium");
                opts.set("x265-params", "log-level=error");
                opts.set("tag", "hvc1");
            }
            (VideoCodec::ProRes422 | VideoCodec::ProRes422Hq, _) => {
                // prores_ks profiles: 2 = 422, 3 = 422 HQ.
                opts.set("profile", if kind == VideoCodec::ProRes422Hq { "3" } else { "2" });
                opts.set("vendor", "apl0");
            }
            (VideoCodec::DnxhrSq | VideoCodec::DnxhrHq, _) => {
                opts.set("profile", if kind == VideoCodec::DnxhrHq { "dnxhr_hq" } else { "dnxhr_sq" });
            }
            (VideoCodec::JpegSequence, _) => {
                // qscale 2 (best) … 31 (worst), in FFmpeg's lambda units.
                let q = match s.quality {
                    Quality::High => 2,
                    Quality::Medium => 4,
                    Quality::Low => 8,
                };
                flags |= ff::codec::Flags::QSCALE;
                enc.set_global_quality(q * 118);
            }
            (VideoCodec::PngSequence | VideoCodec::TiffSequence, _) => {}
            (VideoCodec::H265, _) => enc.set_bit_rate(bitrate(s.quality.bits_per_pixel() * 0.6)),
            _ => enc.set_bit_rate(bitrate(s.quality.bits_per_pixel())),
        }
        if kind == VideoCodec::H265 && name != "libx265" {
            opts.set("tag", "hvc1"); // so Apple players accept the MP4
        }
        enc.set_flags(flags);
        match enc.open_with(opts) {
            Ok(opened) => {
                log::info!("video encoder: {name} ({pix:?})");
                return Ok((opened, codec, name.to_string()));
            }
            Err(e) => errors.push(format!("{name}: {e}")),
        }
    }
    Err(anyhow!(
        "no usable {} encoder in this FFmpeg build ({})",
        kind.label(),
        if errors.is_empty() { "none found".into() } else { errors.join("; ") }
    ))
}

fn open_audio_encoder(octx: &mut ff::format::context::Output, rate: u32, global_header: bool, pcm: bool) -> Result<AudioOut> {
    let id = if pcm { ff::codec::Id::PCM_S24LE } else { ff::codec::Id::AAC };
    let codec = ff::encoder::find(id).ok_or_else(|| anyhow!("no {id:?} encoder in this FFmpeg build"))?;
    let acodec = codec.audio()?;
    let formats: Vec<ff::format::Sample> = acodec.formats().map(|f| f.collect()).unwrap_or_default();
    let fltp = ff::format::Sample::F32(ff::format::sample::Type::Planar);
    let format = if formats.is_empty() || formats.contains(&fltp) { fltp } else { formats[0] };

    let mut enc = ff::codec::context::Context::new_with_codec(codec).encoder().audio()?;
    enc.set_rate(rate as i32);
    enc.set_format(format);
    enc.set_channel_layout(ChannelLayout::default(CHANNELS as i32));
    enc.set_time_base(ff::Rational::new(1, rate as i32));
    if !pcm {
        enc.set_bit_rate(192_000);
    }
    if global_header {
        enc.set_flags(ff::codec::Flags::GLOBAL_HEADER);
    }
    let encoder = enc.open_as(codec).with_context(|| format!("opening the {id:?} audio encoder"))?;
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

#[cfg(test)]
mod fit_tests {
    use super::*;

    #[test]
    fn canvases_for_each_fit() {
        // 16:9 into 9:16.
        let lb = canvas(Fit::Letterbox, 1920, 1080, 1080, 1920).unwrap();
        assert_eq!((lb.w, lb.h, lb.x, lb.y), (1920, 3414, 0, 1167));
        let cr = canvas(Fit::Crop, 1920, 1080, 1080, 1920).unwrap();
        assert_eq!((cr.w, cr.h, cr.x, cr.y), (608, 1080, -656, 0));
        assert_eq!(canvas(Fit::Stretch, 1920, 1080, 1080, 1920), None);
        assert_eq!(canvas(Fit::Crop, 1920, 1080, 1280, 720), None, "same shape");
        // 4:3 into 16:9: bars at the sides, or crop top and bottom.
        let lb = canvas(Fit::Letterbox, 640, 480, 1920, 1080).unwrap();
        assert_eq!((lb.w, lb.h, lb.x, lb.y), (854, 480, 107, 0));
        let cr = canvas(Fit::Crop, 640, 480, 1920, 1080).unwrap();
        assert_eq!((cr.w, cr.h, cr.x, cr.y), (640, 360, 0, -60));
    }
}
