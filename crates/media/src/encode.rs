//! Writing media files: RGBA frames + stereo f32 audio in; out come
//! H.264 or H.265 (MP4/MKV/MOV), ProRes, DNxHR/DNxHD (MOV or MXF) and CineForm
//! masters (MOV) — with alpha where the codec has it — PNG/JPEG/TIFF
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
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

/// How hard a GPU encoder works (Export → Use the graphics card): quicker, or
/// smaller files at the same quality. NVIDIA's NVENC gets presets P2, P5 or P7, with
/// look-ahead, adaptive quantisation and two-pass at the better two.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize)]
pub enum GpuSpeed {
    Fast,
    #[default]
    Balanced,
    Best,
}

/// NVENC's options for an encode (`name` h264_nvenc, hevc_nvenc or av1_nvenc):
/// constant quality like the software encoders' CRF (unless `target`: a bitrate is
/// set), the preset for `speed`, and for an editing proxy the quickest settings.
/// `full`: with look-ahead, adaptive quantisation and two-pass (an older card may
/// refuse some of those; the plain set is tried next).
pub fn nvenc_options(name: &str, quality: Quality, speed: GpuSpeed, proxy: bool, target: bool, full: bool) -> Vec<(&'static str, String)> {
    let mut o: Vec<(&'static str, String)> = Vec::new();
    // Its constant-quality scale (0–51) runs a little above x264's CRF for the same
    // look; HEVC and AV1 need less again.
    let crf: u32 = quality.crf().parse().unwrap_or(21);
    let cq = if proxy {
        25
    } else if name.starts_with("h264") {
        crf + 1
    } else {
        crf + 4
    };
    if proxy {
        o.push(("preset", "p2".into()));
        o.push(("tune", "hq".into()));
        o.push(("rc", "vbr".into()));
        o.push(("cq", cq.to_string()));
        return o;
    }
    o.push(("preset", match speed {
        GpuSpeed::Fast => "p2",
        GpuSpeed::Balanced => "p5",
        GpuSpeed::Best => "p7",
    }
    .into()));
    o.push(("tune", "hq".into()));
    o.push(("rc", "vbr".into()));
    if !target {
        o.push(("cq", cq.to_string()));
    }
    if full && speed != GpuSpeed::Fast {
        o.push(("multipass", if speed == GpuSpeed::Best { "fullres" } else { "qres" }.into()));
        o.push(("rc-lookahead", if speed == GpuSpeed::Best { "32" } else { "16" }.into()));
        o.push(("spatial-aq", "1".into()));
        o.push(("temporal-aq", "1".into()));
        o.push(("b_ref_mode", "middle".into()));
    } else if full {
        o.push(("spatial-aq", "1".into()));
    }
    o
}

/// What kind of video to write.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize)]
pub enum VideoCodec {
    #[default]
    H264,
    H265,
    /// WebM (or MKV): royalty-free, for the web; keeps transparency.
    Vp9,
    /// WebM, MKV or MP4: the newest web codec, smallest files.
    Av1,
    ProRes422Proxy,
    ProRes422Lt,
    ProRes422,
    ProRes422Hq,
    /// 4:4:4, with alpha when exporting a transparent background.
    ProRes4444,
    ProRes4444Xq,
    DnxhrLb,
    DnxhrSq,
    DnxhrHq,
    /// 10-bit 4:2:2.
    DnxhrHqx,
    /// 10-bit 4:4:4.
    Dnxhr444,
    /// The fixed-size HD flavour: 1920×1080, 1440×1080, 1280×720 or 960×720, 8-bit.
    Dnxhd,
    /// DNxHD "x": 10-bit, 1920×1080 or 1280×720.
    Dnxhd10,
    /// 10-bit 4:2:2 YUV.
    Cineform,
    /// 12-bit RGB, with alpha when exporting a transparent background.
    Cineform4444,
    PngSequence,
    JpegSequence,
    TiffSequence,
}

/// The sizes DNxHD comes in (progressive) and the bitrates (Mbit/s) each allows,
/// for 8-bit and 10-bit. DNxHR has no such limits.
const DNXHD_RATES: [(u32, u32, &[u32], &[u32]); 4] = [
    (1920, 1080, &[36, 45, 75, 90, 115, 120, 145, 175, 185, 220, 240, 290, 365, 440], &[175, 185, 365, 440]),
    (1440, 1080, &[63, 84, 100, 110], &[]),
    (1280, 720, &[60, 75, 90, 110, 120, 145, 180, 220], &[90, 180, 220]),
    (960, 720, &[42, 60, 75, 115], &[]),
];

/// The DNxHD bitrate (Mbit/s) for a size, frame rate, quality and bit depth, or
/// why there isn't one. Quality sets the bits per pixel (High is DNxHD 220 at
/// 1080p 29.97, Medium 145, Low 45), and the nearest allowed bitrate is used,
/// which gives the usual pairings (175 at 23.976, 185 at 25, 220 at 29.97…).
pub fn dnxhd_bitrate(w: u32, h: u32, fps: f64, quality: Quality, ten_bit: bool) -> std::result::Result<u32, String> {
    let Some(&(_, _, eight, ten)) = DNXHD_RATES.iter().find(|r| r.0 == w && r.1 == h) else {
        return Err(if ten_bit {
            format!("DNxHD 10-bit only comes in 1920×1080 and 1280×720, not {w}×{h}. Use DNxHR HQX for other sizes.")
        } else {
            format!("DNxHD only comes in 1920×1080, 1440×1080, 1280×720 and 960×720, not {w}×{h}. Use DNxHR for other sizes.")
        });
    };
    let rates = if ten_bit { ten } else { eight };
    if rates.is_empty() {
        return Err(format!("DNxHD 10-bit only comes in 1920×1080 and 1280×720, not {w}×{h}. Use DNxHR HQX for other sizes."));
    }
    let bpp = match quality {
        Quality::High => 220.0,
        Quality::Medium => 145.0,
        Quality::Low => 45.0,
    } / (1920.0 * 1080.0 * 29.97);
    // Avid's 720p rates are about an eighth more per pixel (220 at 59.94, 90 at 23.976).
    let per_pixel = if h == 720 { 1.125 } else { 1.0 };
    let want = bpp * per_pixel * w as f64 * h as f64 * fps.max(1.0);
    rates.iter().min_by(|a, b| ((**a as f64 / want).ln().abs()).total_cmp(&(**b as f64 / want).ln().abs())).copied().ok_or_else(|| "no DNxHD bitrate for that size".to_string())
}

impl VideoCodec {
    pub const ALL: [VideoCodec; 22] = [
        VideoCodec::H264,
        VideoCodec::H265,
        VideoCodec::Vp9,
        VideoCodec::Av1,
        VideoCodec::ProRes422Proxy,
        VideoCodec::ProRes422Lt,
        VideoCodec::ProRes422,
        VideoCodec::ProRes422Hq,
        VideoCodec::ProRes4444,
        VideoCodec::ProRes4444Xq,
        VideoCodec::DnxhrLb,
        VideoCodec::DnxhrSq,
        VideoCodec::DnxhrHq,
        VideoCodec::DnxhrHqx,
        VideoCodec::Dnxhr444,
        VideoCodec::Dnxhd,
        VideoCodec::Dnxhd10,
        VideoCodec::Cineform,
        VideoCodec::Cineform4444,
        VideoCodec::PngSequence,
        VideoCodec::JpegSequence,
        VideoCodec::TiffSequence,
    ];

    pub fn label(self) -> &'static str {
        match self {
            VideoCodec::H264 => "H.264 (MP4)",
            VideoCodec::H265 => "H.265 / HEVC (MP4)",
            VideoCodec::Vp9 => "VP9 (WebM)",
            VideoCodec::Av1 => "AV1 (WebM)",
            VideoCodec::ProRes422Proxy => "ProRes 422 Proxy",
            VideoCodec::ProRes422Lt => "ProRes 422 LT",
            VideoCodec::ProRes422 => "ProRes 422",
            VideoCodec::ProRes422Hq => "ProRes 422 HQ",
            VideoCodec::ProRes4444 => "ProRes 4444",
            VideoCodec::ProRes4444Xq => "ProRes 4444 XQ",
            VideoCodec::DnxhrLb => "DNxHR LB",
            VideoCodec::DnxhrSq => "DNxHR SQ",
            VideoCodec::DnxhrHq => "DNxHR HQ",
            VideoCodec::DnxhrHqx => "DNxHR HQX (10-bit)",
            VideoCodec::Dnxhr444 => "DNxHR 444 (10-bit)",
            VideoCodec::Dnxhd => "DNxHD (HD sizes)",
            VideoCodec::Dnxhd10 => "DNxHD 10-bit (1080p, 720p)",
            VideoCodec::Cineform => "CineForm 422 (10-bit)",
            VideoCodec::Cineform4444 => "CineForm 4444 (12-bit RGB)",
            VideoCodec::PngSequence => "PNG image sequence",
            VideoCodec::JpegSequence => "JPEG image sequence",
            VideoCodec::TiffSequence => "TIFF image sequence",
        }
    }

    /// The heading it's listed under.
    pub fn group(self) -> &'static str {
        if self.is_prores() {
            "Apple ProRes"
        } else if self.is_dnx() {
            "Avid DNxHR / DNxHD"
        } else if self.is_cineform() {
            "GoPro CineForm"
        } else if self.is_sequence() {
            "Image sequences"
        } else {
            "Delivery"
        }
    }

    /// A line on what it's for.
    pub fn hint(self) -> &'static str {
        match self {
            VideoCodec::H264 => "Plays everywhere: the web, phones, TVs.",
            VideoCodec::H265 => "About half the size of H.264 at the same quality; 10-bit and HDR.",
            VideoCodec::Vp9 => "Royalty-free web video with Opus sound, for browsers and YouTube; 10-bit and HDR; keeps transparency if you ask for it (for overlays on web pages).",
            VideoCodec::Av1 => "The newest web codec: about a third smaller than HEVC, with Opus sound in WebM; 10-bit and HDR. Slower to encode without a recent graphics card.",
            VideoCodec::ProRes422Proxy => "Light ProRes for offline editing (about 45 Mbit/s at 1080p30).",
            VideoCodec::ProRes422Lt => "Smaller ProRes for editing and dailies (about 100 Mbit/s at 1080p30).",
            VideoCodec::ProRes422 => "Standard ProRes for editing and finishing (about 145 Mbit/s at 1080p30).",
            VideoCodec::ProRes422Hq => "High-quality ProRes master (about 220 Mbit/s at 1080p30).",
            VideoCodec::ProRes4444 => "Full-colour ProRes for graphics and VFX; keeps transparency if you ask for it.",
            VideoCodec::ProRes4444Xq => "The highest-quality ProRes, for grading and VFX; keeps transparency if you ask for it.",
            VideoCodec::DnxhrLb => "Light DNxHR for offline editing, at any size.",
            VideoCodec::DnxhrSq => "Standard DNxHR for editing, at any size.",
            VideoCodec::DnxhrHq => "High-quality 8-bit DNxHR master, at any size.",
            VideoCodec::DnxhrHqx => "10-bit DNxHR master for grading, at any size.",
            VideoCodec::Dnxhr444 => "10-bit 4:4:4 DNxHR for VFX and finishing, at any size.",
            VideoCodec::Dnxhd => "Classic DNxHD for Avid: 1920×1080, 1440×1080, 1280×720 or 960×720. Quality picks the bitrate (High is 175/185/220 by frame rate).",
            VideoCodec::Dnxhd10 => "10-bit DNxHD (175x/185x/220x) for Avid: 1920×1080 or 1280×720.",
            VideoCodec::Cineform => "GoPro CineForm: light to decode, 10-bit, for editing and masters.",
            VideoCodec::Cineform4444 => "12-bit RGB CineForm for graphics and VFX; keeps transparency if you ask for it.",
            VideoCodec::PngSequence => "One lossless PNG per frame; keeps transparency if you ask for it.",
            VideoCodec::JpegSequence => "One JPEG per frame.",
            VideoCodec::TiffSequence => "One TIFF per frame; keeps transparency if you ask for it.",
        }
    }

    pub fn is_prores(self) -> bool {
        matches!(
            self,
            VideoCodec::ProRes422Proxy | VideoCodec::ProRes422Lt | VideoCodec::ProRes422 | VideoCodec::ProRes422Hq | VideoCodec::ProRes4444 | VideoCodec::ProRes4444Xq
        )
    }

    pub fn is_dnx(self) -> bool {
        matches!(
            self,
            VideoCodec::DnxhrLb | VideoCodec::DnxhrSq | VideoCodec::DnxhrHq | VideoCodec::DnxhrHqx | VideoCodec::Dnxhr444 | VideoCodec::Dnxhd | VideoCodec::Dnxhd10
        )
    }

    pub fn is_cineform(self) -> bool {
        matches!(self, VideoCodec::Cineform | VideoCodec::Cineform4444)
    }

    /// The fixed-size DNxHD flavours (see [`dnxhd_bitrate`]).
    pub fn is_dnxhd(self) -> bool {
        matches!(self, VideoCodec::Dnxhd | VideoCodec::Dnxhd10)
    }

    /// An editing/mastering codec (every frame on its own, uncompressed audio).
    pub fn is_intermediate(self) -> bool {
        self.is_prores() || self.is_dnx() || self.is_cineform()
    }

    /// The usual file extension.
    pub fn extension(self) -> &'static str {
        self.extensions()[0]
    }

    /// The file types it can be written in, the usual one first: the
    /// extension picks the container.
    pub fn extensions(self) -> &'static [&'static str] {
        match self {
            VideoCodec::H264 | VideoCodec::H265 => &["mp4"],
            VideoCodec::Vp9 => &["webm", "mkv"],
            VideoCodec::Av1 => &["webm", "mkv", "mp4"],
            // MXF (OP1a) is what Avid Media Composer expects.
            v if v.is_dnx() => &["mov", "mxf"],
            v if v.is_intermediate() => &["mov"],
            VideoCodec::PngSequence => &["png"],
            VideoCodec::JpegSequence => &["jpg"],
            _ => &["tif"],
        }
    }

    /// One image file per frame.
    pub fn is_sequence(self) -> bool {
        matches!(self, VideoCodec::PngSequence | VideoCodec::JpegSequence | VideoCodec::TiffSequence)
    }

    /// Has a quality setting (the others are fixed-quality by design).
    pub fn uses_quality(self) -> bool {
        matches!(self, VideoCodec::H264 | VideoCodec::H265 | VideoCodec::Vp9 | VideoCodec::Av1 | VideoCodec::JpegSequence) || self.is_dnxhd() || self.is_cineform()
    }

    /// Can use a GPU encoder.
    pub fn has_hardware_encoders(self) -> bool {
        matches!(self, VideoCodec::H264 | VideoCodec::H265 | VideoCodec::Vp9 | VideoCodec::Av1)
    }

    /// Delivery codecs (long-GOP, compressed sound).
    pub fn is_delivery(self) -> bool {
        matches!(self, VideoCodec::H264 | VideoCodec::H265 | VideoCodec::Vp9 | VideoCodec::Av1)
    }

    /// Can keep a transparent background (an alpha channel).
    pub fn supports_alpha(self) -> bool {
        matches!(self, VideoCodec::Vp9 | VideoCodec::ProRes4444 | VideoCodec::ProRes4444Xq | VideoCodec::Cineform4444 | VideoCodec::PngSequence | VideoCodec::TiffSequence)
    }

    /// Intermediate/master codecs that keep audio uncompressed.
    fn wants_pcm_audio(self) -> bool {
        self.is_intermediate()
    }

    /// Software and hardware encoders for this codec, in the order to try them.
    fn encoders(self, hardware_first: bool) -> Vec<&'static str> {
        let (sw, hw): (&[&str], &[&str]) = match self {
            VideoCodec::H264 => (&["libx264", "libopenh264"], &["h264_nvenc", "h264_qsv", "h264_amf", "h264_mf", "h264_videotoolbox"]),
            VideoCodec::H265 => (&["libx265"], &["hevc_nvenc", "hevc_qsv", "hevc_amf", "hevc_mf", "hevc_videotoolbox"]),
            VideoCodec::Vp9 => (&["libvpx-vp9"], &["vp9_qsv", "vp9_vaapi"]),
            // SVT-AV1 is far the quickest of the three software ones.
            VideoCodec::Av1 => (&["libsvtav1", "libaom-av1", "librav1e"], &["av1_nvenc", "av1_qsv", "av1_amf", "av1_vaapi"]),
            v if v.is_prores() => (&["prores_ks", "prores_aw"], &[]),
            v if v.is_dnx() => (&["dnxhd"], &[]),
            v if v.is_cineform() => (&["cfhd"], &[]),
            VideoCodec::PngSequence => (&["png"], &[]),
            VideoCodec::JpegSequence => (&["mjpeg"], &[]),
            _ => (&["tiff"], &[]),
        };
        let mut v: Vec<&str> = if hardware_first { hw.iter().chain(sw).copied().collect() } else { sw.iter().chain(hw).copied().collect() };
        if self == VideoCodec::H264 {
            v.push("mpeg4"); // last resort: plays everywhere, if not efficiently
        }
        v
    }

    /// The pixel format to encode in (`alpha`: keep the transparency, where it can).
    fn pixel(self, alpha: bool) -> Option<ff::format::Pixel> {
        use ff::format::Pixel as P;
        let alpha = alpha && self.supports_alpha();
        Some(match self {
            VideoCodec::ProRes4444 | VideoCodec::ProRes4444Xq if alpha => P::YUVA444P10LE,
            VideoCodec::ProRes4444 | VideoCodec::ProRes4444Xq => P::YUV444P10LE,
            v if v.is_prores() => P::YUV422P10LE,
            VideoCodec::DnxhrHqx | VideoCodec::Dnxhd10 => P::YUV422P10LE,
            VideoCodec::Dnxhr444 => P::YUV444P10LE,
            v if v.is_dnx() => P::YUV422P,
            VideoCodec::Cineform => P::YUV422P10LE,
            VideoCodec::Cineform4444 if alpha => P::GBRAP12LE,
            VideoCodec::Cineform4444 => P::GBRP12LE,
            VideoCodec::PngSequence | VideoCodec::TiffSequence if alpha => P::RGBA,
            VideoCodec::PngSequence | VideoCodec::TiffSequence => P::RGB24,
            VideoCodec::JpegSequence => P::YUVJ420P,
            // VP9 carries alpha as a second, hidden picture (libvpx).
            VideoCodec::Vp9 if alpha => P::YUVA420P,
            _ => return None, // H.264/H.265/VP9/AV1: 4:2:0, or what a GPU encoder wants
        })
    }
}

impl VideoCodec {
    /// Can be written with 10 bits per channel (smoother gradients).
    pub fn supports_10bit(self) -> bool {
        matches!(self, VideoCodec::H265 | VideoCodec::Vp9 | VideoCodec::Av1)
    }

    /// Can carry HDR (BT.2020 colour with a PQ or HLG curve).
    pub fn supports_hdr(self) -> bool {
        matches!(self, VideoCodec::H265 | VideoCodec::Vp9 | VideoCodec::Av1)
    }

    /// Always encoded with more than 8 bits (fed from the 16-bit render).
    pub fn always_deep(self) -> bool {
        self.is_prores() || self.is_cineform() || matches!(self, VideoCodec::DnxhrHqx | VideoCodec::Dnxhr444 | VideoCodec::Dnxhd10)
    }

    /// Takes a target bitrate (the others are fixed-quality by design).
    pub fn uses_bitrate(self) -> bool {
        self.is_delivery()
    }
}

/// The output's dynamic range and colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default, serde::Serialize, serde::Deserialize)]
pub enum ColorOut {
    /// Standard dynamic range, BT.709, 8 bits.
    #[default]
    Sdr8,
    /// Standard dynamic range, BT.709, 10 bits: no banding in skies and fades.
    Sdr10,
    /// HDR10: BT.2020 with the PQ curve, 10 bits. SDR white sits at 203 nits
    /// (ITU-R BT.2408); brighter-than-white highlights (glows, exposure) go above it.
    HdrPq,
    /// HLG: BT.2020 with the hybrid log-gamma curve, 10 bits. Looks right on
    /// both SDR and HDR screens; SDR white at 75% signal (BT.2408).
    HdrHlg,
}

impl ColorOut {
    pub const ALL: [ColorOut; 4] = [ColorOut::Sdr8, ColorOut::Sdr10, ColorOut::HdrPq, ColorOut::HdrHlg];
    pub fn label(self) -> &'static str {
        match self {
            ColorOut::Sdr8 => "SDR, 8-bit",
            ColorOut::Sdr10 => "SDR, 10-bit",
            ColorOut::HdrPq => "HDR10 (PQ), 10-bit",
            ColorOut::HdrHlg => "HDR (HLG), 10-bit",
        }
    }
    pub fn hint(self) -> &'static str {
        match self {
            ColorOut::Sdr8 => "Plays everywhere.",
            ColorOut::Sdr10 => "Smoother gradients (skies, fades, vignettes). Needs a player with 10-bit HEVC support.",
            ColorOut::HdrPq => "For HDR TVs and YouTube HDR. White in the edit is 203 nits; highlights pushed above white (glow, exposure) get brighter.",
            ColorOut::HdrHlg => "Broadcast HDR that also looks right on ordinary screens.",
        }
    }
    pub fn ten_bit(self) -> bool {
        self != ColorOut::Sdr8
    }
    pub fn is_hdr(self) -> bool {
        matches!(self, ColorOut::HdrPq | ColorOut::HdrHlg)
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
    /// How hard a GPU encoder works.
    pub gpu_speed: GpuSpeed,
    /// What to do when the input's shape differs from the output's.
    pub fit: Fit,
    /// Dynamic range and bit depth (10-bit and HDR only for codecs that support them).
    pub color: ColorOut,
    /// A target average bitrate in kbit/s, instead of the quality setting
    /// (H.264/H.265 only). Peaks may go to 1.5× it.
    pub bitrate_kbps: Option<u32>,
    /// Keep the frames' alpha (a transparent background), for codecs that can
    /// ([`VideoCodec::supports_alpha`]); otherwise ignored.
    pub alpha: bool,
    /// The shape of the input frames' pixels (width, height; 1, 1 square).
    /// Written at the same size, the file keeps them (players stretch it as
    /// it should be shown); resized, it's made of square pixels, fitted as
    /// it's shown.
    pub pixel_aspect: (u32, u32),
}

/// How a picture goes into a frame of a different shape.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
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

fn canvas(fit: Fit, iw: u32, ih: u32, w: u32, h: u32, par: f64) -> Option<Canvas> {
    // The input as it's shown (its pixels may not be square).
    let (in_aspect, out_aspect) = (iw as f64 * par / ih.max(1) as f64, w as f64 / h.max(1) as f64);
    // Leave it alone when the shapes differ by less than half a percent.
    if fit == Fit::Stretch || (in_aspect / out_aspect - 1.0).abs() <= 0.005 {
        return None;
    }
    // Letterbox grows the canvas around the input; crop shrinks it inside.
    let wider = (out_aspect > in_aspect) == (fit == Fit::Letterbox);
    let c = if wider {
        let cw = (((ih as f64 * out_aspect / par).round() as u32 + 1) & !1).max(2);
        Canvas { w: cw, h: ih, x: (cw as i32 - iw as i32) / 2, y: 0 }
    } else {
        let ch = (((iw as f64 * par / out_aspect).round() as u32 + 1) & !1).max(2);
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
            gpu_speed: GpuSpeed::Balanced,
            fit: Fit::Letterbox,
            color: ColorOut::Sdr8,
            bitrate_kbps: None,
            alpha: false,
            pixel_aspect: (1, 1),
        }
    }
}

impl WriterSettings {
    /// The colour actually written: 10-bit and HDR fall back to SDR 8-bit for
    /// codecs that can't carry them.
    pub fn effective_color(&self) -> ColorOut {
        match self.video {
            Some(v) if self.color.is_hdr() && !v.supports_hdr() => ColorOut::Sdr8,
            Some(v) if self.color == ColorOut::Sdr10 && !v.supports_10bit() => ColorOut::Sdr8,
            _ => self.color,
        }
    }

    /// The alpha channel is written.
    pub fn keeps_alpha(&self) -> bool {
        self.alpha && self.video.is_some_and(|v| v.supports_alpha())
    }

    /// Frames should come in as 16-bit RGBA ([`MovieWriter::write_video_deep`]).
    pub fn wants_deep(&self) -> bool {
        self.video.is_some_and(|v| v.always_deep()) || self.effective_color().ten_bit()
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
    /// Input frames are RGBA64 (16 bits per channel).
    deep: bool,
    color: ColorOut,
    /// The alpha channel is written (letterbox bars are then transparent).
    alpha: bool,
    /// Each frame carries the HDR10 metadata (for encoders that write it from there).
    hdr10_frames: bool,
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
                    // The pixels' shape on the stream too (MP4 and MOV write it as `pasp`).
                    // SAFETY: a plain field of the stream just added, before the header is written.
                    unsafe {
                        let sar = (*encoder.as_ptr()).sample_aspect_ratio;
                        if sar.num > 0 && sar.den > 0 {
                            (*st.as_mut_ptr()).sample_aspect_ratio = sar;
                        }
                    }
                    // MXF needs the frame rate on the stream itself.
                    let fps = ff::Rational::new(s.frame_rate.0.num as i32, s.frame_rate.0.den as i32);
                    st.set_rate(fps);
                    st.set_avg_frame_rate(fps);
                    st.index()
                };
                if s.effective_color() == ColorOut::HdrPq {
                    add_stream_hdr10(&mut octx, stream);
                }
                let pix = encoder.format();
                let yuv = !is_rgb(pix);
                let range = if pix == ff::format::Pixel::YUVJ420P { Range::JPEG } else { Range::MPEG };
                let [iw, ih] = s.input_size.unwrap_or([s.width, s.height]);
                let (pn, pd) = if s.pixel_aspect.0 == 0 || s.pixel_aspect.1 == 0 { (1, 1) } else { s.pixel_aspect };
                // The same size: the pixels keep their shape (written into the file); resized: square.
                let same = (iw, ih) == (w, h) || s.input_size.is_none();
                let pad = canvas(s.fit, iw, ih, w, h, if same { 1.0 } else { pn as f64 / pd as f64 });
                let (sw, sh) = pad.map_or((iw, ih), |c| (c.w, c.h));
                let deep = s.wants_deep();
                let color = s.effective_color();
                let input = if deep { ff::format::Pixel::RGBA64LE } else { ff::format::Pixel::RGBA };
                let mut scaler = Scaler::get(input, sw, sh, pix, w, h, Flags::BICUBIC)?;
                if yuv {
                    crate::color::configure(&mut scaler, if color.is_hdr() { Space::BT2020NCL } else { Space::BT709 }, range, false);
                }
                let time_base = encoder.time_base();
                // x265 writes HDR10's SEI from its own settings; other encoders from the frames.
                let hdr10_frames = color == ColorOut::HdrPq && video_codec != "libx265";
                Some(VideoOut { encoder, stream, time_base, scaler, in_size: (iw, ih), pad, frame: ff::frame::Video::new(pix, w, h), next_pts: 0, range, yuv, deep, color, alpha: s.keeps_alpha(), hdr10_frames })
            }
            None => None,
        };

        let ext = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
        let sound = if s.video.is_none_or(|v| v.wants_pcm_audio()) {
            Sound::Pcm
        } else if ext == "webm" || (ext == "mkv" && s.video.is_some_and(|v| matches!(v, VideoCodec::Vp9 | VideoCodec::Av1))) {
            // WebM takes only Opus (or Vorbis).
            Sound::Opus
        } else {
            Sound::Aac
        };
        let audio = match s.audio_rate {
            Some(rate) if !sequence => Some(open_audio_encoder(&mut octx, rate, global_header, sound)?),
            _ => None,
        };
        if video.is_none() {
            video_codec = audio.as_ref().map(|a| a.encoder.codec().map(|c| c.name().to_string()).unwrap_or_default()).unwrap_or_default();
        }

        octx.write_header().context("writing container header")?;
        Ok(Self { octx, video, audio, path: path.to_owned(), video_codec, sequence, finished: false })
    }

    /// The frames to pass: 16-bit ([`write_video_deep`](Self::write_video_deep))
    /// rather than 8-bit ([`write_video`](Self::write_video)).
    pub fn wants_deep(&self) -> bool {
        self.video.as_ref().is_some_and(|v| v.deep)
    }

    /// The colour being written (after falling back for codecs that can't do 10-bit/HDR).
    pub fn color(&self) -> ColorOut {
        self.video.as_ref().map_or(ColorOut::Sdr8, |v| v.color)
    }

    /// Append one frame of tightly packed sRGB RGBA at the size given in the settings.
    pub fn write_video(&mut self, rgba: &[u8]) -> Result<()> {
        if self.wants_deep() {
            // Widen to 16 bits (x·257 maps 0…255 exactly onto 0…65535).
            let wide: Vec<u16> = rgba.iter().map(|&b| b as u16 * 257).collect();
            return self.write_video_deep(&wide);
        }
        let black: &[u8] = if self.keeps_alpha() { &[0, 0, 0, 0] } else { &[0, 0, 0, 255] };
        self.put_frame(rgba, 4, ff::format::Pixel::RGBA, black)
    }

    /// The alpha channel is being written.
    pub fn keeps_alpha(&self) -> bool {
        self.video.as_ref().is_some_and(|v| v.alpha)
    }

    /// Append one frame of tightly packed 16-bit RGBA, already in the output's
    /// curve (sRGB, PQ or HLG; see `ColorOut`).
    pub fn write_video_deep(&mut self, rgba: &[u16]) -> Result<()> {
        if !self.wants_deep() {
            let narrow: Vec<u8> = rgba.iter().map(|&v| ((v as u32 + 128) / 257) as u8).collect();
            let black: &[u8] = if self.keeps_alpha() { &[0, 0, 0, 0] } else { &[0, 0, 0, 255] };
            return self.put_frame(&narrow, 4, ff::format::Pixel::RGBA, black);
        }
        // SAFETY: u16 has no padding or invalid bit patterns; viewed as bytes, little-endian
        // on every platform we build for (RGBA64LE).
        let bytes = unsafe { std::slice::from_raw_parts(rgba.as_ptr() as *const u8, rgba.len() * 2) };
        // Black in each curve is zero; opaque alpha is all ones.
        let black: &[u8] = if self.keeps_alpha() { &[0; 8] } else { &[0, 0, 0, 0, 0, 0, 255, 255] };
        self.put_frame(bytes, 8, ff::format::Pixel::RGBA64LE, black)
    }

    /// Place a packed frame (`bpp` bytes per pixel) on the canvas, scale/convert, encode.
    fn put_frame(&mut self, pixels: &[u8], bpp: usize, format: ff::format::Pixel, black: &[u8]) -> Result<()> {
        let Some(v) = &mut self.video else { return Ok(()) };
        let (w, h) = v.in_size;
        if pixels.len() != w as usize * h as usize * bpp {
            bail!("frame is {} bytes, expected {}x{} at {} bytes per pixel", pixels.len(), w, h, bpp);
        }
        let c = v.pad.unwrap_or(Canvas { w, h, x: 0, y: 0 });
        let mut src = ff::frame::Video::new(format, c.w, c.h);
        let stride = src.stride(0);
        let data = src.data_mut(0);
        if c.x > 0 || c.y > 0 {
            // Black bars (opaque, or clear when keeping alpha).
            for y in 0..c.h as usize {
                for px in data[y * stride..y * stride + c.w as usize * bpp].chunks_exact_mut(bpp) {
                    px.copy_from_slice(black);
                }
            }
        }
        // The part of the input that lands on the canvas.
        let (x0, x1) = ((-c.x).max(0) as usize, (c.w as i32 - c.x).min(w as i32) as usize);
        let in_row = w as usize * bpp;
        for cy in 0..c.h as i32 {
            let y = cy - c.y;
            if y < 0 || y >= h as i32 {
                continue;
            }
            let at = cy as usize * stride + (x0 as i32 + c.x) as usize * bpp;
            let from = y as usize * in_row;
            data[at..at + (x1 - x0) * bpp].copy_from_slice(&pixels[from + x0 * bpp..from + x1 * bpp]);
        }
        // Frame-threaded encoders (ProRes, DNxHR…) keep a reference to the last
        // frame and encode it later: writing the next picture into that same
        // buffer would change it under them (frames came out repeated and
        // skipped). A new buffer if the encoder still holds this one.
        // SAFETY: `v.frame` is a valid, allocated video frame.
        if unsafe { ff::ffi::av_frame_make_writable(v.frame.as_mut_ptr()) } < 0 {
            bail!("out of memory for a video frame");
        }
        v.scaler.run(&src, &mut v.frame)?;
        v.frame.set_pts(Some(v.next_pts));
        if v.yuv {
            let (space, primaries, trc) = color_tags(v.color);
            v.frame.set_color_space(space);
            v.frame.set_color_range(v.range);
            v.frame.set_color_primaries(primaries);
            v.frame.set_color_transfer_characteristic(trc);
        }
        if v.hdr10_frames {
            add_frame_hdr10(&mut v.frame);
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
            let Some(a) = self.audio.as_mut() else { return Ok(()) };
            let n = a.frame_size * CHANNELS;
            if a.pending.len() < n {
                return Ok(());
            }
            let chunk: Vec<f32> = a.pending.drain(..n).collect();
            self.encode_audio_chunk(&chunk)?;
        }
    }

    fn encode_audio_chunk(&mut self, chunk: &[f32]) -> Result<()> {
        let Some(a) = self.audio.as_mut() else { return Ok(()) };
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
        let ost_tb = self.octx.stream(v.stream).ok_or_else(|| anyhow!("the video stream went missing"))?.time_base();
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
        let ost_tb = self.octx.stream(a.stream).ok_or_else(|| anyhow!("the audio stream went missing"))?.time_base();
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
            if let Some(a) = self.audio.as_mut() {
                a.encoder.send_eof()?;
            }
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

// ---- HDR10 metadata -------------------------------------------------------------

/// libavutil's `AVMasteringDisplayMetadata` (SMPTE ST 2086), not in the
/// generated bindings: chromaticities and luminances as rationals.
#[repr(C)]
#[derive(Clone, Copy)]
struct MasteringDisplay {
    /// R, G, B, each (x, y).
    display_primaries: [[ff::ffi::AVRational; 2]; 3],
    white_point: [ff::ffi::AVRational; 2],
    min_luminance: ff::ffi::AVRational,
    max_luminance: ff::ffi::AVRational,
    has_primaries: i32,
    has_luminance: i32,
}

/// libavutil's `AVContentLightMetadata` (CTA-861.3): MaxCLL and MaxFALL, nits.
#[repr(C)]
#[derive(Clone, Copy)]
struct ContentLight {
    max_cll: u32,
    max_fall: u32,
}

/// The HDR10 metadata exports carry (the same as given to x265): mastered on
/// a P3-D65 display of 0.0001–1000 nits, brightest pixel 1000, brightest
/// frame on average 400.
fn hdr10_metadata() -> (MasteringDisplay, ContentLight) {
    let q = |n: i32, d: i32| ff::ffi::AVRational { num: n, den: d };
    let c = |v: i32| q(v, 50_000);
    let md = MasteringDisplay {
        display_primaries: [[c(34_000), c(16_000)], [c(13_250), c(34_500)], [c(7_500), c(3_000)]],
        white_point: [c(15_635), c(16_450)],
        min_luminance: q(1, 10_000),
        max_luminance: q(1_000, 1),
        has_primaries: 1,
        has_luminance: 1,
    };
    (md, ContentLight { max_cll: 1_000, max_fall: 400 })
}

fn bytes_of<T: Copy>(v: &T) -> &[u8] {
    // SAFETY: a plain-old-data `repr(C)` value, read as its bytes.
    unsafe { std::slice::from_raw_parts(v as *const T as *const u8, std::mem::size_of::<T>()) }
}

/// Put the HDR10 metadata on stream `index` of `octx` (so MP4 and MOV carry
/// it in `mdcv`/`clli` boxes, whatever the encoder).
fn add_stream_hdr10(octx: &mut ff::format::context::Output, index: usize) {
    let (md, cl) = hdr10_metadata();
    // SAFETY: the stream exists; its codec parameters are valid and owned by the context.
    unsafe {
        let st = *(*octx.as_mut_ptr()).streams.add(index);
        let par = (*st).codecpar;
        use ff::ffi::AVPacketSideDataType as T;
        for (kind, data) in [(T::AV_PKT_DATA_MASTERING_DISPLAY_METADATA, bytes_of(&md)), (T::AV_PKT_DATA_CONTENT_LIGHT_LEVEL, bytes_of(&cl))] {
            let sd = ff::ffi::av_packet_side_data_new(&mut (*par).coded_side_data, &mut (*par).nb_coded_side_data, kind, data.len(), 0);
            if !sd.is_null() {
                std::ptr::copy_nonoverlapping(data.as_ptr(), (*sd).data, data.len());
            }
        }
    }
}

/// Put the HDR10 metadata on `frame`, for encoders that write it into the
/// bitstream from there (hardware HEVC encoders: as SEI messages players read).
fn add_frame_hdr10(frame: &mut ff::frame::Video) {
    let (md, cl) = hdr10_metadata();
    use ff::ffi::AVFrameSideDataType as T;
    for (kind, data) in [(T::AV_FRAME_DATA_MASTERING_DISPLAY_METADATA, bytes_of(&md)), (T::AV_FRAME_DATA_CONTENT_LIGHT_LEVEL, bytes_of(&cl))] {
        // SAFETY: a valid frame; the new side data is `data.len()` bytes.
        unsafe {
            ff::ffi::av_frame_remove_side_data(frame.as_mut_ptr(), kind);
            let sd = ff::ffi::av_frame_new_side_data(frame.as_mut_ptr(), kind, data.len());
            if !sd.is_null() {
                std::ptr::copy_nonoverlapping(data.as_ptr(), (*sd).data, data.len());
            }
        }
    }
}

/// A quality's constant-quality value for the web encoders: [high, medium, low].
fn web_crf(q: Quality, v: [u32; 3]) -> String {
    match q {
        Quality::High => v[0],
        Quality::Medium => v[1],
        Quality::Low => v[2],
    }
    .to_string()
}

/// Options a hardware HEVC encoder needs for `color`: the Main 10 profile
/// for 10-bit (most pick 8-bit Main otherwise, or refuse P010).
fn hardware_hevc_options(name: &str, color: ColorOut) -> Vec<(&'static str, &'static str)> {
    if !color.ten_bit() || !name.starts_with("hevc_") {
        return Vec::new();
    }
    match name {
        "hevc_nvenc" | "hevc_qsv" | "hevc_amf" | "hevc_videotoolbox" | "hevc_vaapi" => vec![("profile", "main10")],
        _ => Vec::new(),
    }
}

/// Colour tags for the stream and frames.
/// An RGB (not YUV) pixel format: no colour matrix.
fn is_rgb(p: ff::format::Pixel) -> bool {
    use ff::format::Pixel as P;
    matches!(p, P::RGB24 | P::RGBA | P::GBRP12LE | P::GBRAP12LE | P::GBRP10LE)
}

fn color_tags(c: ColorOut) -> (Space, Primaries, TransferCharacteristic) {
    match c {
        ColorOut::Sdr8 | ColorOut::Sdr10 => (Space::BT709, Primaries::BT709, TransferCharacteristic::BT709),
        ColorOut::HdrPq => (Space::BT2020NCL, Primaries::BT2020, TransferCharacteristic::SMPTE2084),
        ColorOut::HdrHlg => (Space::BT2020NCL, Primaries::BT2020, TransferCharacteristic::ARIB_STD_B67),
    }
}

fn open_video_encoder(kind: VideoCodec, w: u32, h: u32, s: &WriterSettings, global_header: bool) -> Result<(ff::encoder::Video, ff::Codec, String)> {
    let color = s.effective_color();
    let tb = ff::Rational::new(s.frame_rate.0.den as i32, s.frame_rate.0.num as i32);
    let fps = ff::Rational::new(s.frame_rate.0.num as i32, s.frame_rate.0.den as i32);
    let mut errors = Vec::new();
    // NVENC first with its full set of options, then plain (an older card may not
    // take them all), before moving on to the next encoder.
    let tries = kind.encoders(s.hardware).into_iter().flat_map(|n| if n.contains("_nvenc") { vec![(n, true), (n, false)] } else { vec![(n, false)] });
    for (name, full) in tries {
        // Proxies on the graphics card only with the encoders that do all-keyframe video well.
        if s.proxy && is_hardware_encoder(name) && !["_nvenc", "_qsv", "_amf"].iter().any(|k| name.contains(k)) {
            continue;
        }
        let Some(codec) = ff::encoder::find_by_name(name) else { continue };
        let Ok(vcodec) = codec.video() else { continue };
        let formats: Vec<ff::format::Pixel> = vcodec.formats().map(|f| f.collect()).unwrap_or_default();
        use ff::format::Pixel as P;
        let pix = match kind.pixel(s.alpha) {
            Some(p) => p,
            // 10-bit: planar for software encoders, P010 for GPU ones.
            None if color.ten_bit() => match [P::YUV420P10LE, P::P010LE].into_iter().find(|p| formats.is_empty() || formats.contains(p)) {
                Some(p) => p,
                None => {
                    errors.push(format!("{name}: no 10-bit support"));
                    continue;
                }
            },
            // Prefer yuv420p (plays everywhere); otherwise the encoder's first format (e.g. nv12 for hardware).
            None if formats.is_empty() || formats.contains(&P::YUV420P) => P::YUV420P,
            None => formats[0],
        };
        if !formats.is_empty() && !formats.contains(&pix) {
            errors.push(format!("{name}: no {pix:?} support"));
            continue;
        }

        let mut enc = ff::codec::context::Context::new_with_codec(codec).encoder().video()?;
        enc.set_width(w);
        enc.set_height(h);
        // Non-square pixels kept (an export at the sequence's own size): written into the file.
        let (pn, pd) = s.pixel_aspect;
        if pn != pd && pn > 0 && pd > 0 && s.input_size.is_none_or(|[iw, ih]| (iw, ih) == (w, h)) {
            enc.set_aspect_ratio(ff::Rational::new(pn as i32, pd as i32));
        }
        enc.set_format(pix);
        enc.set_time_base(tb);
        enc.set_frame_rate(Some(fps));
        let mut flags = ff::codec::Flags::empty();
        if kind.is_delivery() {
            // Keyframe every ~2 s for easy scrubbing; every frame for proxies.
            enc.set_gop(if s.proxy { 1 } else { (s.frame_rate.fps() * 2.0).round().max(1.0) as u32 });
            if s.proxy {
                enc.set_max_b_frames(0);
            } else if name.contains("_nvenc") && full && s.gpu_speed != GpuSpeed::Fast {
                enc.set_max_b_frames(3);
            }
            // Closed GOPs: every keyframe is a clean random-access point, so files
            // seek cleanly in editors (including this one) and play everywhere.
            flags |= ff::codec::Flags::CLOSED_GOP;
        }
        if kind.is_intermediate() {
            // Every frame stands alone: let the encoder use every core, by frames
            // (CineForm) or slices (ProRes, DNx), whichever it has (FFmpeg's default type).
            // SAFETY: setting a plain field before the encoder is opened.
            unsafe {
                (*enc.as_mut_ptr()).thread_count = 0;
            }
        }
        if !matches!(pix, ff::format::Pixel::RGB24 | ff::format::Pixel::RGBA) {
            let (space, primaries, trc) = color_tags(color);
            enc.set_colorspace(if is_rgb(pix) { Space::RGB } else { space });
            enc.set_color_range(if pix == ff::format::Pixel::YUVJ420P { Range::JPEG } else { Range::MPEG });
            enc.set_color_primaries(primaries);
            enc.set_color_transfer_characteristic(trc);
        }
        if global_header {
            flags |= ff::codec::Flags::GLOBAL_HEADER;
        }

        let mut opts = Dictionary::new();
        // A target bitrate replaces the quality setting (CRF).
        let target = s.bitrate_kbps.filter(|k| *k > 0 && kind.uses_bitrate() && !s.proxy);
        let bitrate = |bpp: f64| ((w as f64 * h as f64 * s.frame_rate.fps() * bpp) as usize).max(500_000);
        match (kind, name) {
            (_, "libx264") if s.proxy => {
                opts.set("crf", "23");
                opts.set("preset", "veryfast");
                opts.set("tune", "fastdecode");
            }
            (_, "libx264") => {
                if target.is_none() {
                    opts.set("crf", s.quality.crf());
                }
                opts.set("preset", "medium");
                opts.set("profile", "high");
            }
            (_, "libx265") => {
                // HEVC needs ~4 less CRF for the same look as H.264, at about half the size.
                let crf: u32 = s.quality.crf().parse::<u32>().unwrap_or(21) + 4;
                if target.is_none() {
                    opts.set("crf", &crf.to_string());
                }
                opts.set("preset", "medium");
                let mut params = String::from("log-level=error");
                match color {
                    // Signal HDR10 with typical mastering metadata (P3 primaries, 1000 nits),
                    // in every keyframe so players switch modes when seeking.
                    ColorOut::HdrPq => params.push_str(
                        ":hdr10=1:repeat-headers=1:colorprim=bt2020:transfer=smpte2084:colormatrix=bt2020nc\
                         :master-display=G(13250,34500)B(7500,3000)R(34000,16000)WP(15635,16450)L(10000000,1):max-cll=1000,400",
                    ),
                    ColorOut::HdrHlg => params.push_str(":repeat-headers=1:colorprim=bt2020:transfer=arib-std-b67:colormatrix=bt2020nc"),
                    _ => {}
                }
                opts.set("x265-params", &params);
                opts.set("tag", "hvc1");
            }
            (v, _) if v.is_prores() => {
                // prores_ks/prores_aw profiles: 0 Proxy, 1 LT, 2 422, 3 HQ, 4 4444, 5 4444 XQ.
                let profile = match v {
                    VideoCodec::ProRes422Proxy => "0",
                    VideoCodec::ProRes422Lt => "1",
                    VideoCodec::ProRes422 => "2",
                    VideoCodec::ProRes422Hq => "3",
                    VideoCodec::ProRes4444 => "4",
                    _ => "5",
                };
                opts.set("profile", profile);
                opts.set("vendor", "apl0");
            }
            (v, _) if v.is_dnxhd() => {
                let mbps = dnxhd_bitrate(w, h, s.frame_rate.fps(), s.quality, v == VideoCodec::Dnxhd10).map_err(|e| anyhow!(e))?;
                opts.set("profile", "dnxhd");
                enc.set_bit_rate(mbps as usize * 1_000_000);
            }
            (v, _) if v.is_dnx() => {
                let profile = match v {
                    VideoCodec::DnxhrLb => "dnxhr_lb",
                    VideoCodec::DnxhrSq => "dnxhr_sq",
                    VideoCodec::DnxhrHq => "dnxhr_hq",
                    VideoCodec::DnxhrHqx => "dnxhr_hqx",
                    _ => "dnxhr_444",
                };
                opts.set("profile", profile);
            }
            (v, _) if v.is_cineform() => {
                // 0 (film3+, the best) … 12 (low).
                let q = match s.quality {
                    Quality::High => "film3+",
                    Quality::Medium => "film1",
                    Quality::Low => "high",
                };
                opts.set("quality", q);
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
            (_, "libvpx-vp9") => {
                // Constant quality (a bitrate of 0) unless a target is set; CRF 0–63.
                if target.is_none() {
                    opts.set("crf", &web_crf(s.quality, [24, 31, 38]));
                    enc.set_bit_rate(0);
                }
                opts.set("deadline", "good");
                opts.set("cpu-used", "2");
                opts.set("row-mt", "1");
                opts.set("tile-columns", "2");
                if color.ten_bit() {
                    opts.set("profile", "2");
                }
                if pix == ff::format::Pixel::YUVA420P {
                    // The alpha picture needs no alt-ref frames to stay in step.
                    opts.set("auto-alt-ref", "0");
                }
            }
            (_, "libsvtav1") => {
                if target.is_none() {
                    opts.set("crf", &web_crf(s.quality, [27, 33, 41]));
                }
                opts.set("preset", "8");
                opts.set("svtav1-params", "log=0");
            }
            (_, "libaom-av1") => {
                if target.is_none() {
                    opts.set("crf", &web_crf(s.quality, [27, 33, 41]));
                    enc.set_bit_rate(0);
                }
                opts.set("cpu-used", "6");
                opts.set("row-mt", "1");
            }
            (_, "librav1e") => {
                if target.is_none() {
                    // Quantiser 0–255.
                    opts.set("qp", &web_crf(s.quality, [70, 100, 140]));
                }
                opts.set("speed", "6");
            }
            // NVENC: constant quality and its presets (see `nvenc_options`).
            (_, n) if n.contains("_nvenc") => {
                for (k, v) in nvenc_options(n, s.quality, s.gpu_speed, s.proxy, target.is_some(), full) {
                    opts.set(k, &v);
                }
                if target.is_none() {
                    enc.set_bit_rate(0);
                }
            }
            // GPU AV1 and VP9: a bitrate, a little under HEVC's.
            (VideoCodec::Av1, _) => enc.set_bit_rate(bitrate(s.quality.bits_per_pixel() * 0.5)),
            (VideoCodec::Vp9, _) => enc.set_bit_rate(bitrate(s.quality.bits_per_pixel() * 0.6)),
            (VideoCodec::H265, _) => enc.set_bit_rate(bitrate(s.quality.bits_per_pixel() * 0.6)),
            _ => enc.set_bit_rate(bitrate(s.quality.bits_per_pixel())),
        }
        if kind == VideoCodec::H265 && name != "libx265" {
            opts.set("tag", "hvc1"); // so Apple players accept the MP4
            for (k, v) in hardware_hevc_options(name, color) {
                opts.set(k, v);
            }
        }
        if let Some(kbps) = target {
            // Average `kbps`, peaks to 1.5×.
            let bps = kbps as usize * 1000;
            enc.set_bit_rate(bps);
            enc.set_max_bit_rate(bps * 3 / 2);
            opts.set("bufsize", &(bps * 2).to_string());
            if name.contains("nvenc") {
                opts.set("rc", "vbr");
            }
        }
        enc.set_flags(flags);
        match enc.open_with(opts) {
            Ok(opened) => {
                log::info!("video encoder: {name} ({pix:?}{})", if name.contains("_nvenc") && !full { ", plain settings" } else { "" });
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

/// What the sound is written as.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sound {
    Aac,
    Pcm,
    Opus,
}

fn open_audio_encoder(octx: &mut ff::format::context::Output, rate: u32, global_header: bool, sound: Sound) -> Result<AudioOut> {
    let pcm = sound == Sound::Pcm;
    let id = match sound {
        Sound::Pcm => ff::codec::Id::PCM_S24LE,
        Sound::Aac => ff::codec::Id::AAC,
        Sound::Opus => ff::codec::Id::OPUS,
    };
    // libopus over FFmpeg's own (still experimental) Opus encoder.
    let codec = (if sound == Sound::Opus { ff::encoder::find_by_name("libopus") } else { None }).or_else(|| ff::encoder::find(id)).ok_or_else(|| anyhow!("no {id:?} encoder in this FFmpeg build"))?;
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
    fn nvenc_gets_constant_quality_and_its_presets() {
        let get = |o: &[(&str, String)], k: &str| o.iter().find(|(n, _)| *n == k).map(|(_, v)| v.clone());
        let o = nvenc_options("h264_nvenc", Quality::Medium, GpuSpeed::Balanced, false, false, true);
        assert_eq!((get(&o, "preset"), get(&o, "rc"), get(&o, "cq")), (Some("p5".into()), Some("vbr".into()), Some("22".into())));
        assert_eq!((get(&o, "multipass"), get(&o, "rc-lookahead"), get(&o, "temporal-aq")), (Some("qres".into()), Some("16".into()), Some("1".into())));
        // HEVC a little lower on the scale; Best: full two-pass; Fast: none of the extras.
        let o = nvenc_options("hevc_nvenc", Quality::High, GpuSpeed::Best, false, false, true);
        assert_eq!((get(&o, "preset"), get(&o, "cq"), get(&o, "multipass")), (Some("p7".into()), Some("21".into()), Some("fullres".into())));
        let o = nvenc_options("h264_nvenc", Quality::Low, GpuSpeed::Fast, false, false, true);
        assert_eq!((get(&o, "preset"), get(&o, "multipass"), get(&o, "b_ref_mode")), (Some("p2".into()), None, None));
        // A target bitrate: no constant quality; the plain set: no extras.
        assert_eq!(get(&nvenc_options("h264_nvenc", Quality::Medium, GpuSpeed::Best, false, true, true), "cq"), None);
        let plain = nvenc_options("hevc_nvenc", Quality::Medium, GpuSpeed::Best, false, false, false);
        assert!(get(&plain, "rc-lookahead").is_none() && get(&plain, "spatial-aq").is_none() && get(&plain, "cq").is_some());
        // Proxies: quick, constant quality.
        let o = nvenc_options("h264_nvenc", Quality::High, GpuSpeed::Best, true, false, true);
        assert_eq!((get(&o, "preset"), get(&o, "cq"), get(&o, "multipass")), (Some("p2".into()), Some("25".into()), None));
    }

    #[test]
    fn hardware_goes_first_for_proxies_too_when_asked() {
        assert_eq!(VideoCodec::H264.encoders(true).first(), Some(&"h264_nvenc"));
        assert_eq!(VideoCodec::H264.encoders(false).first(), Some(&"libx264"));
    }

    #[test]
    fn dnxhd_picks_avids_usual_bitrates() {
        let r = |w, h, fps, q, ten| dnxhd_bitrate(w, h, fps, q, ten);
        // DNxHD 175/185/220 (and the "x" 10-bit ones) by frame rate at 1080p.
        assert_eq!(r(1920, 1080, 23.976, Quality::High, false), Ok(175));
        assert_eq!(r(1920, 1080, 25.0, Quality::High, false), Ok(185));
        assert_eq!(r(1920, 1080, 29.97, Quality::High, false), Ok(220));
        assert_eq!(r(1920, 1080, 50.0, Quality::High, false), Ok(365));
        assert_eq!(r(1920, 1080, 59.94, Quality::High, false), Ok(440));
        assert_eq!(r(1920, 1080, 25.0, Quality::High, true), Ok(185));
        assert_eq!(r(1920, 1080, 23.976, Quality::Medium, false), Ok(115));
        assert_eq!(r(1920, 1080, 29.97, Quality::Medium, false), Ok(145));
        assert_eq!(r(1920, 1080, 29.97, Quality::Low, false), Ok(45));
        assert_eq!(r(1280, 720, 59.94, Quality::High, true), Ok(220));
        assert_eq!(r(1280, 720, 23.976, Quality::High, false), Ok(90));
        assert_eq!(r(1280, 720, 29.97, Quality::High, false), Ok(110));
        assert_eq!(r(1280, 720, 59.94, Quality::Medium, false), Ok(145));
        assert!(r(1440, 1080, 25.0, Quality::High, false).is_ok());
        assert!(r(1920, 800, 25.0, Quality::High, false).unwrap_err().contains("DNxHR"));
        assert!(r(1440, 1080, 25.0, Quality::High, true).unwrap_err().contains("HQX"));
    }

    #[test]
    fn canvases_for_each_fit() {
        // 16:9 into 9:16.
        let lb = canvas(Fit::Letterbox, 1920, 1080, 1080, 1920, 1.0).unwrap();
        assert_eq!((lb.w, lb.h, lb.x, lb.y), (1920, 3414, 0, 1167));
        let cr = canvas(Fit::Crop, 1920, 1080, 1080, 1920, 1.0).unwrap();
        assert_eq!((cr.w, cr.h, cr.x, cr.y), (608, 1080, -656, 0));
        assert_eq!(canvas(Fit::Stretch, 1920, 1080, 1080, 1920, 1.0), None);
        assert_eq!(canvas(Fit::Crop, 1920, 1080, 1280, 720, 1.0), None, "same shape");
        // 4:3 into 16:9: bars at the sides, or crop top and bottom.
        let lb = canvas(Fit::Letterbox, 640, 480, 1920, 1080, 1.0).unwrap();
        assert_eq!((lb.w, lb.h, lb.x, lb.y), (854, 480, 107, 0));
        let cr = canvas(Fit::Crop, 640, 480, 1920, 1080, 1.0).unwrap();
        assert_eq!((cr.w, cr.h, cr.x, cr.y), (640, 360, 0, -60));
        // DV NTSC (720 × 480 of 10:11 pixels, shown about 4:3) into square 16:9: bars at the sides.
        let lb = canvas(Fit::Letterbox, 720, 480, 1920, 1080, 10.0 / 11.0).unwrap();
        assert_eq!((lb.w, lb.h), (940, 480));
        // 2:1 anamorphic 1440 × 1080 (shown 2.67:1) into 16:9: bars above and below.
        let lb = canvas(Fit::Letterbox, 1440, 1080, 1920, 1080, 2.0).unwrap();
        assert_eq!((lb.w, lb.h), (1440, 1620));
    }
}

#[cfg(test)]
mod hdr_tests {
    use super::*;

    #[test]
    fn hdr10_metadata_matches_libavutil_and_x265() {
        // The C structs' sizes (6 + 2 + 2 rationals and two ints; two unsigned).
        assert_eq!(std::mem::size_of::<MasteringDisplay>(), 88);
        assert_eq!(std::mem::size_of::<ContentLight>(), 8);
        let (md, cl) = hdr10_metadata();
        // What x265 is given: G(13250,34500) B(7500,3000) R(34000,16000) WP(15635,16450) L(10000000,1).
        let v = |r: ff::ffi::AVRational| r.num as f64 / r.den as f64;
        assert_eq!((v(md.display_primaries[0][0]), v(md.display_primaries[1][1]), v(md.display_primaries[2][0])), (0.68, 0.69, 0.15));
        assert_eq!((v(md.white_point[0]), v(md.white_point[1])), (0.3127, 0.329));
        assert_eq!((v(md.max_luminance), v(md.min_luminance)), (1000.0, 0.0001));
        assert_eq!((cl.max_cll, cl.max_fall), (1000, 400));
        // On a frame, readable back.
        let mut f = ff::frame::Video::new(ff::format::Pixel::P010LE, 16, 16);
        add_frame_hdr10(&mut f);
        add_frame_hdr10(&mut f); // again: replaced, not doubled
        unsafe {
            let sd = ff::ffi::av_frame_get_side_data(f.as_ptr(), ff::ffi::AVFrameSideDataType::AV_FRAME_DATA_CONTENT_LIGHT_LEVEL);
            assert!(!sd.is_null());
            let back = *((*sd).data as *const ContentLight);
            assert_eq!((back.max_cll, back.max_fall), (1000, 400));
            assert_eq!((*f.as_ptr()).nb_side_data, 2);
        }
    }

    #[test]
    fn hardware_hevc_gets_main10_for_ten_bit_only() {
        assert_eq!(hardware_hevc_options("hevc_nvenc", ColorOut::HdrPq), vec![("profile", "main10")]);
        assert_eq!(hardware_hevc_options("hevc_qsv", ColorOut::Sdr10), vec![("profile", "main10")]);
        assert_eq!(hardware_hevc_options("hevc_amf", ColorOut::HdrHlg), vec![("profile", "main10")]);
        assert!(hardware_hevc_options("hevc_nvenc", ColorOut::Sdr8).is_empty());
        assert!(hardware_hevc_options("h264_nvenc", ColorOut::HdrPq).is_empty());
        assert!(hardware_hevc_options("hevc_mf", ColorOut::HdrPq).is_empty(), "Media Foundation takes no profile option");
    }
}

