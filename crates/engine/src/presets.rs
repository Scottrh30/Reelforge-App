//! Export presets: the built-in ones, and the shape of the ones users save.

use reelforge_media::{ColorOut, Fit, Quality, VideoCodec};

/// The output size.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Size {
    /// The sequence's size divided by 1, 2 or 4.
    Scaled(u32),
    /// A fixed size (presets, and "Custom").
    Fixed(u32, u32),
}

impl Size {
    pub fn resolve(self, seq_w: u32, seq_h: u32) -> (u32, u32) {
        match self {
            Size::Scaled(d) => (((seq_w / d) & !1).max(2), ((seq_h / d) & !1).max(2)),
            Size::Fixed(w, h) => (w.max(2), h.max(2)),
        }
    }
}

pub const COMMON_SIZES: [(u32, u32, &str); 6] = [
    (3840, 2160, "4K UHD"),
    (1920, 1080, "1080p"),
    (1280, 720, "720p"),
    (1080, 1920, "Vertical 9:16"),
    (1080, 1350, "Portrait 4:5"),
    (1080, 1080, "Square 1:1"),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preset {
    YouTube1080,
    YouTube4k,
    YouTubeHdr,
    Vertical,
    Portrait,
    Square,
    MasterProRes,
    MasterDnxhr,
    MasterCineform,
    GraphicsAlpha,
    WebVp9,
    WebAv1,
    WebOverlay,
    Draft,
    PngSequence,
    JpegSequence,
    AudioWav,
    Custom,
}

/// What a preset sets.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Spec {
    pub video: Option<VideoCodec>,
    pub size: Size,
    pub quality: Quality,
    pub fit: Fit,
    pub hardware: bool,
    /// How hard the GPU encoder works.
    #[serde(default)]
    pub gpu_speed: reelforge_media::encode::GpuSpeed,
    #[serde(default)]
    pub color: ColorOut,
    /// Target bitrate (kbit/s) instead of the quality setting.
    #[serde(default)]
    pub bitrate_kbps: Option<u32>,
    /// A transparent background, for codecs with an alpha channel.
    #[serde(default)]
    pub alpha: bool,
    /// Frames a second to write (the sequence's when `None`).
    #[serde(default)]
    pub frame_rate: Option<reelforge_core::FrameRate>,
    /// How footage fills in between its frames throughout (`None`: as each clip says).
    #[serde(default)]
    pub between: Option<reelforge_core::FrameBlend>,
}

/// Export settings you saved under a name (kept with the app's settings).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct UserPreset {
    pub name: String,
    pub spec: Spec,
    pub include_audio: bool,
}

impl Preset {
    pub const ALL: [Preset; 17] = [
        Preset::YouTube1080,
        Preset::YouTube4k,
        Preset::YouTubeHdr,
        Preset::Vertical,
        Preset::Portrait,
        Preset::Square,
        Preset::MasterProRes,
        Preset::MasterDnxhr,
        Preset::MasterCineform,
        Preset::GraphicsAlpha,
        Preset::WebVp9,
        Preset::WebAv1,
        Preset::WebOverlay,
        Preset::Draft,
        Preset::PngSequence,
        Preset::JpegSequence,
        Preset::AudioWav,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Preset::YouTube1080 => "YouTube 1080p",
            Preset::YouTube4k => "YouTube 4K (H.265)",
            Preset::YouTubeHdr => "YouTube 4K HDR (H.265, HLG)",
            Preset::Vertical => "Shorts / Reels / TikTok (9:16)",
            Preset::Portrait => "Instagram portrait (4:5)",
            Preset::Square => "Square (1:1)",
            Preset::MasterProRes => "Master: ProRes 422 HQ",
            Preset::MasterDnxhr => "Master: DNxHR HQ",
            Preset::MasterCineform => "Master: CineForm 422",
            Preset::GraphicsAlpha => "Graphics with transparency: ProRes 4444",
            Preset::WebVp9 => "Web: VP9 WebM",
            Preset::WebAv1 => "Web: AV1 WebM",
            Preset::WebOverlay => "Web overlay with transparency: VP9 WebM",
            Preset::Draft => "Quick draft (half size, GPU)",
            Preset::PngSequence => "PNG image sequence",
            Preset::JpegSequence => "JPEG image sequence",
            Preset::AudioWav => "Audio only (WAV)",
            Preset::Custom => "Custom",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Preset::YouTube1080 => "H.264 + AAC at 1920×1080, high quality. Plays everywhere.",
            Preset::YouTube4k => "H.265 + AAC at 3840×2160. Smaller files than H.264 at the same quality.",
            Preset::YouTubeHdr => "10-bit HDR (HLG) H.265 at 3840×2160. Looks right on HDR and ordinary screens; glows and highlights above white get extra brightness on HDR ones.",
            Preset::Vertical => "1080×1920 for phone-first platforms. Wide footage is cropped to fill the frame.",
            Preset::Portrait => "1080×1350 for feeds. Wide footage is cropped to fill the frame.",
            Preset::Square => "1080×1080. Wide footage is cropped to fill the frame.",
            Preset::MasterProRes => "Near-lossless 10-bit master at the sequence size, with uncompressed audio. Big files.",
            Preset::MasterDnxhr => "Avid's master codec at the sequence size, with uncompressed audio. Big files.",
            Preset::MasterCineform => "GoPro CineForm, 10-bit, at the sequence size: quick to decode, with uncompressed audio.",
            Preset::GraphicsAlpha => "Titles and graphics over a see-through background, to lay over footage in another editor.",
            Preset::WebVp9 => "VP9 + Opus at 1920×1080: royalty-free, plays in every browser.",
            Preset::WebAv1 => "AV1 + Opus at 1920×1080: the smallest files for the web, at the same quality.",
            Preset::WebOverlay => "VP9 with a see-through background, for animations laid over a web page (Chrome, Firefox, Edge).",
            Preset::Draft => "Fast, small H.264 for checking a cut. Uses the graphics card's encoder if there is one.",
            Preset::PngSequence => "One lossless PNG per frame, for VFX and colour work. No audio.",
            Preset::JpegSequence => "One JPEG per frame, for thumbnails and contact sheets. No audio.",
            Preset::AudioWav => "The mixed soundtrack as 24-bit, 48 kHz stereo WAV.",
            Preset::Custom => "Your own choice of settings.",
        }
    }

    pub fn spec(self) -> Option<Spec> {
        let s = |video, size, quality, fit, hardware| Some(Spec { video, size, quality, fit, hardware, gpu_speed: Default::default(), color: ColorOut::Sdr8, bitrate_kbps: None, alpha: false, frame_rate: None, between: None });
        use VideoCodec as V;
        match self {
            Preset::YouTube1080 => s(Some(V::H264), Size::Fixed(1920, 1080), Quality::High, Fit::Letterbox, false),
            Preset::YouTube4k => s(Some(V::H265), Size::Fixed(3840, 2160), Quality::High, Fit::Letterbox, false),
            Preset::YouTubeHdr => Some(Spec { color: ColorOut::HdrHlg, ..s(Some(V::H265), Size::Fixed(3840, 2160), Quality::High, Fit::Letterbox, false)? }),
            Preset::Vertical => s(Some(V::H264), Size::Fixed(1080, 1920), Quality::High, Fit::Crop, false),
            Preset::Portrait => s(Some(V::H264), Size::Fixed(1080, 1350), Quality::High, Fit::Crop, false),
            Preset::Square => s(Some(V::H264), Size::Fixed(1080, 1080), Quality::High, Fit::Crop, false),
            Preset::MasterProRes => s(Some(V::ProRes422Hq), Size::Scaled(1), Quality::High, Fit::Letterbox, false),
            Preset::MasterDnxhr => s(Some(V::DnxhrHq), Size::Scaled(1), Quality::High, Fit::Letterbox, false),
            Preset::MasterCineform => s(Some(V::Cineform), Size::Scaled(1), Quality::High, Fit::Letterbox, false),
            Preset::GraphicsAlpha => Some(Spec { alpha: true, ..s(Some(V::ProRes4444), Size::Scaled(1), Quality::High, Fit::Letterbox, false)? }),
            Preset::WebVp9 => s(Some(V::Vp9), Size::Fixed(1920, 1080), Quality::High, Fit::Letterbox, false),
            Preset::WebAv1 => s(Some(V::Av1), Size::Fixed(1920, 1080), Quality::High, Fit::Letterbox, false),
            Preset::WebOverlay => Some(Spec { alpha: true, ..s(Some(V::Vp9), Size::Scaled(1), Quality::High, Fit::Letterbox, false)? }),
            Preset::Draft => s(Some(V::H264), Size::Scaled(2), Quality::Low, Fit::Letterbox, true),
            Preset::PngSequence => s(Some(V::PngSequence), Size::Scaled(1), Quality::High, Fit::Letterbox, false),
            Preset::JpegSequence => s(Some(V::JpegSequence), Size::Scaled(1), Quality::High, Fit::Letterbox, false),
            Preset::AudioWav => s(None, Size::Scaled(1), Quality::High, Fit::Letterbox, false),
            Preset::Custom => None,
        }
    }
}

/// A sensible starting bitrate for a `w`×`h` export (about YouTube's
/// recommendations: ~8 Mbit/s for 1080p H.264, less for H.265).
pub fn suggested_kbps(w: u32, h: u32, codec: VideoCodec) -> u32 {
    let bpp = match codec {
        VideoCodec::H265 | VideoCodec::Vp9 => 0.08,
        VideoCodec::Av1 => 0.06,
        _ => 0.13,
    };
    let kbps = w as f64 * h as f64 * 30.0 * bpp / 1000.0;
    ((kbps / 100.0).round() * 100.0).max(500.0) as u32
}

pub fn extension(video: Option<VideoCodec>) -> &'static str {
    video.map_or("wav", |v| v.extension())
}

pub fn format_label(video: Option<VideoCodec>) -> &'static str {
    video.map_or("Audio only (WAV)", |v| v.label())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_set_sizes() {
        let master = Preset::MasterProRes.spec().unwrap();
        assert_eq!(master.size.resolve(2560, 1440), (2560, 1440), "masters keep the sequence size");
        let v = Preset::Vertical.spec().unwrap();
        assert_eq!((v.size.resolve(1920, 1080), v.fit), ((1080, 1920), Fit::Crop));
        assert!(Preset::AudioWav.spec().unwrap().video.is_none());
        assert_eq!(extension(None), "wav");
        assert_eq!(Preset::Draft.spec().unwrap().size.resolve(1920, 1080), (960, 540));
        assert!(Preset::Custom.spec().is_none());
        let web = Preset::WebOverlay.spec().unwrap();
        assert!(web.alpha && web.video == Some(VideoCodec::Vp9) && web.video.unwrap().supports_alpha());
        assert_eq!(extension(Preset::WebAv1.spec().unwrap().video), "webm");
    }

    #[test]
    fn saved_presets_round_trip() {
        let mut spec = Preset::YouTube4k.spec().unwrap();
        spec.color = ColorOut::Sdr10;
        spec.bitrate_kbps = Some(25_000);
        let saved = vec![UserPreset { name: "Client 4K".into(), spec, include_audio: false }];
        let json = serde_json::to_string(&saved).unwrap();
        let back: Vec<UserPreset> = serde_json::from_str(&json).unwrap();
        assert_eq!(back, saved);
        // Presets saved before colour/bitrate existed still load.
        let old: Spec = serde_json::from_str(r#"{"video":"H264","size":{"Scaled":1},"quality":"High","fit":"Letterbox","hardware":false}"#).unwrap();
        assert_eq!((old.color, old.bitrate_kbps, old.alpha), (ColorOut::Sdr8, None, false));
    }

    #[test]
    fn suggested_bitrates_are_sensible() {
        assert_eq!(suggested_kbps(1920, 1080, VideoCodec::H264), 8100);
        assert!(suggested_kbps(3840, 2160, VideoCodec::H265) > 15_000);
        assert_eq!(suggested_kbps(64, 64, VideoCodec::H264), 500);
    }

    #[test]
    fn odd_sequence_sizes_scale_to_even_ones() {
        assert_eq!(Size::Scaled(2).resolve(1918, 1078), (958, 538));
        assert_eq!(Size::Scaled(4).resolve(6, 6), (2, 2));
    }
}
