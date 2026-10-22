//! Reelforge's own decoders (`reelforge-codecs`) plugged in where FFmpeg's
//! decoder would be: packets still come from FFmpeg's container readers, and
//! the decoded pictures are handed on as FFmpeg frames, so scaling, colour
//! handling and caching are the same whichever decoder made them.

use ffmpeg_next as ff;
use reelforge_codecs::{Codec, Decoder, Format, Matrix, Picture, StreamInfo};

/// Reelforge's codec for an FFmpeg codec id.
pub fn codec_of(id: ff::codec::Id) -> Option<Codec> {
    use ff::codec::Id;
    Some(match id {
        Id::MJPEG => Codec::Mjpeg,
        Id::PRORES => Codec::ProRes,
        Id::DNXHD => Codec::Dnxhd,
        Id::MPEG2VIDEO => Codec::Mpeg2,
        Id::H264 => Codec::H264,
        Id::HEVC => Codec::Hevc,
        Id::VP9 => Codec::Vp9,
        Id::AV1 => Codec::Av1,
        _ => return None,
    })
}

/// A native decoder for a stream, if there is one (and they're switched on).
pub fn open(params: &ff::codec::Parameters) -> Option<NativeVideo> {
    let codec = codec_of(params.id())?;
    // SAFETY: reading plain fields of valid codec parameters.
    let info = unsafe {
        let p = &*params.as_ptr();
        let extradata = if p.extradata.is_null() || p.extradata_size <= 0 { Vec::new() } else { std::slice::from_raw_parts(p.extradata, p.extradata_size as usize).to_vec() };
        StreamInfo { width: p.width.max(0) as u32, height: p.height.max(0) as u32, extradata }
    };
    let dec = reelforge_codecs::open(codec, &info)?;
    log::info!("decoding {} with Reelforge's own decoder", codec.label());
    Some(NativeVideo { dec, codec })
}

#[cfg(test)]
fn open_codec(codec: Codec) -> Option<NativeVideo> {
    reelforge_codecs::open(codec, &StreamInfo::default()).map(|dec| NativeVideo { dec, codec })
}

pub struct NativeVideo {
    dec: Box<dyn Decoder>,
    pub codec: Codec,
}

impl NativeVideo {
    pub fn send_packet(&mut self, packet: &ff::Packet) -> Result<(), reelforge_codecs::Error> {
        let pts = packet.pts().or(packet.dts()).unwrap_or(ff::ffi::AV_NOPTS_VALUE);
        match packet.data() {
            // A panic in a decoder (here or on a pool thread) is an error like any
            // other: the caller falls back to FFmpeg instead of losing the clip.
            Some(d) if !d.is_empty() => std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.dec.send(d, pts)))
                .unwrap_or_else(|_| Err(reelforge_codecs::Error::Invalid(format!("{} decoder panicked", self.codec.label())))),
            _ => Ok(()),
        }
    }

    /// The next picture as an FFmpeg frame.
    pub fn receive_frame(&mut self, frame: &mut ff::frame::Video) -> bool {
        match self.dec.receive() {
            Some(p) => {
                *frame = to_frame(&p);
                true
            }
            None => false,
        }
    }

    pub fn flush(&mut self) {
        self.dec.flush();
    }

    pub fn send_eof(&mut self) {
        self.dec.finish();
    }
}

fn pixel(f: Format) -> ff::format::Pixel {
    use ff::format::Pixel as P;
    match f {
        Format::Gray8 => P::GRAY8,
        Format::Yuv420P8 => P::YUV420P,
        Format::Yuv422P8 => P::YUV422P,
        Format::Yuv444P8 => P::YUV444P,
        Format::Yuv420P10 => P::YUV420P10LE,
        Format::Yuv422P10 => P::YUV422P10LE,
        Format::Yuv444P10 => P::YUV444P10LE,
        Format::Yuv420P12 => P::YUV420P12LE,
        Format::Yuv422P12 => P::YUV422P12LE,
        Format::Yuv444P12 => P::YUV444P12LE,
        Format::Yuva444P10 => P::YUVA444P10LE,
        Format::Yuva444P12 => P::YUVA444P12LE,
        Format::Gbrp10 => P::GBRP10LE,
        Format::Gbrp12 => P::GBRP12LE,
    }
}

/// A native picture as an FFmpeg frame (rows copied to FFmpeg's padded layout).
pub fn to_frame(p: &Picture) -> ff::frame::Video {
    let mut f = ff::frame::Video::new(pixel(p.format), p.width, p.height);
    let b = p.format.bytes_per_sample();
    for (i, pl) in p.planes.iter().enumerate() {
        let (pw, ph) = p.format.plane_size(i, p.width, p.height);
        let row = pw as usize * b;
        let stride = f.stride(i);
        let dst = f.data_mut(i);
        for y in 0..ph as usize {
            let s = &pl.data[y * pl.stride..y * pl.stride + row];
            dst[y * stride..y * stride + row].copy_from_slice(s);
        }
    }
    f.set_color_space(match p.matrix {
        Matrix::Bt709 => ff::util::color::Space::BT709,
        Matrix::Bt2020 => ff::util::color::Space::BT2020NCL,
        Matrix::Bt601 => ff::util::color::Space::BT470BG,
        Matrix::Unknown => ff::util::color::Space::Unspecified,
    });
    f.set_color_range(if p.full_range { ff::util::color::Range::JPEG } else { ff::util::color::Range::MPEG });
    if p.pts != ff::ffi::AV_NOPTS_VALUE {
        f.set_pts(Some(p.pts));
    }
    f
}

/// Test support: decode every frame of `path`'s video with both FFmpeg and the
/// native decoder and compare them as RGBA. Returns (frames compared, worst PSNR in dB).
#[cfg(test)]
pub(crate) fn compare_with_ffmpeg(path: &std::path::Path) -> (usize, f64) {
    compare(path).unwrap_or_else(|e| panic!("{e}"))
}

/// Decode every frame of `path`'s video with both FFmpeg and Reelforge's own
/// decoder and compare them as RGBA: (frames compared, worst PSNR in dB).
pub(crate) fn compare(path: &std::path::Path) -> Result<(usize, f64), String> {
    use ff::software::scaling::{context::Context as Scaler, flag::Flags};
    crate::init();
    let mut ictx = ff::format::input(path).map_err(|e| format!("can't read the sample: {e}"))?;
    let st = ictx.streams().best(ff::media::Type::Video).ok_or("the sample has no video")?;
    let index = st.index();
    let params = st.parameters();
    let mut ffd = ff::codec::context::Context::from_parameters(params.clone()).and_then(|c| c.decoder().video()).map_err(|e| format!("FFmpeg's decoder didn't open: {e}"))?;
    let mut nat = open(&params).ok_or("Reelforge has no decoder for this")?;
    let rgba = |f: &ff::frame::Video| -> Vec<u8> {
        let Ok(mut s) = Scaler::get(f.format(), f.width(), f.height(), ff::format::Pixel::RGBA, f.width(), f.height(), Flags::BILINEAR | Flags::ACCURATE_RND | Flags::FULL_CHR_H_INT) else { return Vec::new() };
        crate::color::configure(&mut s, crate::color::effective_space(f.color_space(), f.height()), f.color_range(), true);
        let mut o = ff::frame::Video::empty();
        if s.run(f, &mut o).is_err() {
            return Vec::new();
        }
        let (w, h) = (f.width() as usize, f.height() as usize);
        (0..h).flat_map(|y| o.data(0)[y * o.stride(0)..y * o.stride(0) + w * 4].to_vec()).collect()
    };
    let (mut a, mut b) = (Vec::new(), Vec::new());
    let (mut ya, mut yb): (Vec<ff::frame::Video>, Vec<ff::frame::Video>) = (Vec::new(), Vec::new());
    let mut packets = Vec::new();
    for (s, p) in ictx.packets() {
        if s.index() == index {
            packets.push(p);
        }
    }
    for p in &packets {
        ffd.send_packet(p).map_err(|e| format!("FFmpeg's decoder: {e}"))?;
        let mut f = ff::frame::Video::empty();
        while ffd.receive_frame(&mut f).is_ok() {
            a.push(rgba(&f));
            ya.push(f.clone());
        }
        nat.send_packet(p).map_err(|e| format!("Reelforge's decoder: {e}"))?;
        let mut g = ff::frame::Video::empty();
        while nat.receive_frame(&mut g) {
            b.push(rgba(&g));
            yb.push(g.clone());
        }
    }
    ffd.send_eof().map_err(|e| format!("FFmpeg's decoder: {e}"))?;
    let mut f = ff::frame::Video::empty();
    while ffd.receive_frame(&mut f).is_ok() {
        a.push(rgba(&f));
        ya.push(f.clone());
    }
    nat.send_eof();
    let mut g = ff::frame::Video::empty();
    while nat.receive_frame(&mut g) {
        b.push(rgba(&g));
        yb.push(g.clone());
    }
    if std::env::var_os("RF_VERBOSE").is_some() {
        for (k, (fa, fb)) in ya.iter().zip(&yb).enumerate() {
            for p in 0..3 {
                if fa.format() != fb.format() || fa.planes() <= p {
                    continue;
                }
                let (w, h) = if p == 0 { (fa.width() as usize, fa.height() as usize) } else { (fa.plane_width(p) as usize, fa.plane_height(p) as usize) };
                let mut diffs = Vec::new();
                for yy in 0..h {
                    for xx in 0..w {
                        let va = fa.data(p)[yy * fa.stride(p) + xx];
                        let vb = fb.data(p)[yy * fb.stride(p) + xx];
                        if (va as i32 - vb as i32).abs() > 2 {
                            diffs.push((xx, yy, va, vb));
                        }
                    }
                }
                if !diffs.is_empty() {
                    eprintln!("  yuv frame {k} plane {p}: {} samples differ, e.g. {:?}", diffs.len(), &diffs[..diffs.len().min(6)]);
                }
            }
        }
    }
    if a.len() != b.len() {
        return Err(format!("frame counts differ: FFmpeg {}, Reelforge {}", a.len(), b.len()));
    }
    let mut worst = f64::INFINITY;
    for (x, y) in a.iter().zip(&b) {
        if x.len() != y.len() || x.is_empty() {
            return Err("frame sizes differ".into());
        }
        let mse = x.iter().zip(y).map(|(p, q)| { let d = *p as f64 - *q as f64; d * d }).sum::<f64>() / x.len() as f64;
        let psnr = if mse == 0.0 { 99.0 } else { 10.0 * (255.0f64 * 255.0 / mse).log10() };
        if std::env::var_os("RF_VERBOSE").is_some() {
            // Which 16×16 blocks differ noticeably.
            let w = (x.len() / 4) as f64;
            let width = std::env::var("RF_W").ok().and_then(|v| v.parse::<usize>().ok()).unwrap_or(128);
            let mut mbs = std::collections::BTreeSet::new();
            for (i, (p, q)) in x.chunks(4).zip(y.chunks(4)).enumerate() {
                if (p[0] as i32 - q[0] as i32).abs() > 8 {
                    mbs.insert(((i % width) / 16, (i / width) / 16));
                }
            }
            let _ = w;
            eprintln!("  frame: {psnr:.1} dB, differing MBs {:?}", mbs.iter().take(12).collect::<Vec<_>>());
            if let Some(&(mx, my)) = mbs.iter().next() {
                for row in [0usize, 1, 2, 3, 8, 15] {
                    let yy = my * 16 + row;
                    let a: Vec<u8> = (0..16).map(|k| x[(yy * width + mx * 16 + k) * 4 + 1]).collect();
                    let b: Vec<u8> = (0..16).map(|k| y[(yy * width + mx * 16 + k) * 4 + 1]).collect();
                    eprintln!("    row {row}: ffmpeg {a:?}\n           native {b:?}");
                }
            }
        }
        worst = worst.min(psnr);
    }
    Ok((a.len(), worst))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    /// A test clip made with the ffmpeg CLI; None (test skipped) without it.
    pub(crate) fn clip(name: &str, args: &[&str]) -> Option<PathBuf> {
        let dir = std::env::temp_dir().join(format!("rf-native-{}", std::process::id()));
        std::fs::create_dir_all(&dir).ok()?;
        let p = dir.join(name);
        let ok = std::process::Command::new("ffmpeg")
            .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i", "testsrc2=size=320x180:rate=25:duration=0.4"])
            .args(args)
            .arg(&p)
            .status()
            .is_ok_and(|s| s.success());
        ok.then_some(p)
    }

    fn check(path: &Path, min_psnr: f64) {
        let (n, psnr) = compare_with_ffmpeg(path);
        eprintln!("{}: {n} frames, worst {psnr:.1} dB", path.file_name().unwrap().to_string_lossy());
        assert!(n >= 5, "only {n} frames");
        assert!(psnr >= min_psnr, "{}: worst frame {psnr:.1} dB", path.display());
    }

    #[test]
    fn prores_matches_ffmpeg() {
        for (name, args) in [
            ("p422.mov", &["-c:v", "prores_ks", "-profile:v", "2"][..]),
            ("p422hq.mov", &["-c:v", "prores_ks", "-profile:v", "3"][..]),
            ("pproxy.mov", &["-c:v", "prores_ks", "-profile:v", "0"][..]),
            ("p4444.mov", &["-c:v", "prores_ks", "-profile:v", "4", "-pix_fmt", "yuv444p10le"][..]),
            ("p4444a.mov", &["-c:v", "prores_ks", "-profile:v", "4", "-pix_fmt", "yuva444p10le"][..]),
            ("paw.mov", &["-c:v", "prores_aw", "-profile:v", "2"][..]),
            ("pint.mov", &["-c:v", "prores_ks", "-profile:v", "2", "-flags", "+ildct", "-top", "1"][..]),
            ("podd.mov", &["-vf", "scale=302:170", "-c:v", "prores_ks", "-profile:v", "1"][..]),
        ] {
            let Some(p) = clip(name, args) else { return };
            check(&p, 45.0);
        }
    }

    /// A clip at a given size and rate (DNxHD's fixed profiles need exact ones).
    fn clip_at(name: &str, size: &str, rate: u32, args: &[&str]) -> Option<PathBuf> {
        let dir = std::env::temp_dir().join(format!("rf-native-{}", std::process::id()));
        std::fs::create_dir_all(&dir).ok()?;
        let p = dir.join(name);
        let frames = if name.ends_with(".mov") { 6.0 } else { 30.0 };
        let src = format!("testsrc2=size={size}:rate={rate}:duration={}", frames / rate as f64);
        let ok = std::process::Command::new("ffmpeg")
            .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i", &src])
            .args(args)
            .arg(&p)
            .status()
            .is_ok_and(|s| s.success());
        ok.then_some(p)
    }

    #[test]
    fn dnxhd_matches_ffmpeg() {
        let hr = |p: &'static str| -> Vec<&'static str> { vec!["-c:v", "dnxhd", "-profile:v", p] };
        let cases: Vec<(&str, &str, u32, Vec<&str>)> = vec![
            ("lb.mov", "320x180", 25, hr("dnxhr_lb")),
            ("sq.mov", "320x180", 25, hr("dnxhr_sq")),
            ("hq.mov", "320x180", 25, hr("dnxhr_hq")),
            ("odd.mov", "302x170", 25, hr("dnxhr_hq")),
            ("hqx.mov", "320x180", 25, [hr("dnxhr_hqx"), vec!["-pix_fmt", "yuv422p10le"]].concat()),
            ("hr444.mov", "320x180", 25, [hr("dnxhr_444"), vec!["-pix_fmt", "yuv444p10le"]].concat()),
            ("hr444rgb.mov", "320x180", 25, [hr("dnxhr_444"), vec!["-pix_fmt", "gbrp10le"]].concat()),
            ("d36.mov", "1920x1080", 25, vec!["-c:v", "dnxhd", "-b:v", "36M"]),
            ("d185x.mov", "1920x1080", 25, vec!["-c:v", "dnxhd", "-b:v", "185M", "-pix_fmt", "yuv422p10le"]),
            ("d90.mov", "1280x720", 25, vec!["-c:v", "dnxhd", "-b:v", "90M"]),
            ("dint.mov", "1920x1080", 25, vec!["-c:v", "dnxhd", "-b:v", "120M", "-flags", "+ildct"]),
        ];
        let mut ran = 0;
        for (name, size, rate, args) in cases {
            let Some(p) = clip_at(name, size, rate, &args) else {
                eprintln!("{name}: ffmpeg couldn't make it, skipped");
                continue;
            };
            check(&p, 45.0);
            ran += 1;
        }
        assert!(ran >= 7 || std::process::Command::new("ffmpeg").arg("-version").output().is_err());
    }

    #[test]
    fn mpeg2_matches_ffmpeg() {
        let m = |extra: &[&'static str]| -> Vec<&'static str> { [vec!["-c:v", "mpeg2video", "-q:v", "4"], extra.to_vec()].concat() };
        let cases: Vec<(&str, &str, u32, Vec<&str>)> = vec![
            ("intra.m2v", "320x180", 25, m(&["-g", "1"])),
            ("ip.mpg", "320x180", 25, m(&["-bf", "0", "-g", "12"])),
            ("ipb.ts", "320x180", 25, m(&["-bf", "2", "-g", "12"])),
            ("ipb.mp4", "352x288", 25, m(&["-bf", "2", "-g", "15"])),
            ("ildct.ts", "720x576", 25, m(&["-bf", "2", "-flags", "+ildct+ilme", "-top", "1"])),
            ("p422.mxf", "320x180", 25, m(&["-pix_fmt", "yuv422p", "-bf", "2"])),
            ("alt.m2v", "320x180", 25, m(&["-alternate_scan", "1", "-intra_vlc", "1", "-non_linear_quant", "1", "-qmax", "28", "-dc", "10", "-bf", "2"])),
            ("hq.m2v", "1280x720", 25, vec!["-c:v", "mpeg2video", "-b:v", "8M", "-bf", "2", "-intra_matrix", "8,16,19,22,26,27,29,34,16,16,22,24,27,29,34,37,19,22,26,27,29,34,34,38,22,22,26,27,29,34,37,40,22,26,27,29,32,35,40,48,26,27,29,32,35,40,48,58,26,27,29,34,38,46,56,69,27,29,35,38,46,56,69,90"]),
        ];
        let mut ran = 0;
        for (name, size, rate, args) in cases {
            let Some(p) = clip_at(name, size, rate, &args) else {
                eprintln!("{name}: ffmpeg couldn't make it, skipped");
                continue;
            };
            check(&p, 40.0);
            ran += 1;
        }
        assert!(ran >= 6 || std::process::Command::new("ffmpeg").arg("-version").output().is_err());
    }

    /// H.264, HEVC, VP9 and AV1 through the pure-Rust decoder libraries.
    #[test]
    fn h264_hevc_vp9_av1_match_ffmpeg() {
        let x264 = |extra: &[&'static str]| -> Vec<&'static str> { [vec!["-c:v", "libx264", "-preset", "fast", "-crf", "20"], extra.to_vec()].concat() };
        let x265 = |extra: &[&'static str]| -> Vec<&'static str> { [vec!["-c:v", "libx265", "-preset", "fast", "-crf", "24", "-x265-params", "log-level=error"], extra.to_vec()].concat() };
        let vp9 = |extra: &[&'static str]| -> Vec<&'static str> { [vec!["-c:v", "libvpx-vp9", "-deadline", "realtime", "-cpu-used", "8", "-b:v", "1M"], extra.to_vec()].concat() };
        let cases: Vec<(&str, &str, u32, Vec<&str>)> = vec![
            ("baseline.mp4", "320x180", 25, x264(&["-profile:v", "baseline"])),
            ("main.mp4", "320x180", 25, x264(&["-profile:v", "main", "-bf", "3"])),
            ("high.mkv", "352x288", 25, x264(&["-profile:v", "high", "-bf", "3", "-x264-params", "weightp=2:b-pyramid=normal:slices=3"])),
            ("annexb.ts", "320x180", 25, x264(&["-bf", "2"])),
            ("odd264.mp4", "302x170", 25, x264(&[])),
            ("hevc.mp4", "320x180", 25, x265(&["-bf", "3"])),
            ("hevc10.mkv", "320x180", 25, x265(&["-pix_fmt", "yuv420p10le"])),
            ("hevc.ts", "320x180", 25, x265(&[])),
            ("vp9.webm", "320x180", 25, vp9(&[])),
            ("vp9_10.webm", "320x180", 25, vp9(&["-pix_fmt", "yuv420p10le"])),
            ("vp9_444.mkv", "320x180", 25, vp9(&["-pix_fmt", "yuv444p"])),
            ("av1.mp4", "320x180", 25, vec!["-c:v", "libsvtav1", "-preset", "12", "-crf", "35", "-svtav1-params", "log=0"]),
            ("av1_10.mkv", "320x180", 25, vec!["-c:v", "libsvtav1", "-preset", "12", "-crf", "35", "-pix_fmt", "yuv420p10le"]),
            ("aom.webm", "320x180", 25, vec!["-c:v", "libaom-av1", "-cpu-used", "8", "-crf", "35", "-row-mt", "1"]),
        ];
        let mut ran = 0;
        let mut bad = Vec::new();
        for (name, size, rate, args) in cases {
            let Some(p) = clip_at(name, size, rate, &args) else {
                eprintln!("{name}: ffmpeg couldn't make it, skipped");
                continue;
            };
            let (n, psnr) = compare_with_ffmpeg(&p);
            eprintln!("{name}: {n} frames, worst {psnr:.1} dB");
            if n < 25 || psnr < 50.0 {
                bad.push(format!("{name}: {n} frames, {psnr:.1} dB"));
            }
            ran += 1;
        }
        assert!(bad.is_empty(), "{bad:#?}");
        assert!(ran >= 8 || std::process::Command::new("ffmpeg").arg("-version").output().is_err());
    }

    /// What the libraries don't decode (interlaced or 10-bit H.264) is reported as an
    /// error, which hands the file to FFmpeg.
    #[test]
    fn unsupported_h264_is_reported() {
        for (name, args) in [
            ("interlaced264.mp4", vec!["-c:v", "libx264", "-flags", "+ildct+ilme", "-x264-params", "interlaced=1"]),
            ("hi10.mp4", vec!["-c:v", "libx264", "-pix_fmt", "yuv420p10le"]),
        ] {
            let Some(p) = clip_at(name, "320x180", 25, &args) else { continue };
            crate::init();
            let mut ictx = ff::format::input(&p).unwrap();
            let st = ictx.streams().best(ff::media::Type::Video).unwrap();
            let index = st.index();
            let mut nat = open(&st.parameters()).unwrap();
            let mut failed = false;
            for (s, packet) in ictx.packets() {
                if s.index() == index && nat.send_packet(&packet).is_err() {
                    failed = true;
                    break;
                }
            }
            assert!(failed, "{name} should be refused");
        }
    }

    /// Field pictures, same-frame field references, 16×8 and dual-prime motion
    /// and skipped macroblocks: FFmpeg's encoder never makes these, so random
    /// streams that use them are generated and both decoders must agree.
    #[test]
    fn mpeg2_field_pictures_and_dual_prime_match_ffmpeg() {
        use reelforge_codecs::mpeg2::testgen::{generate, Options};
        let dir = std::env::temp_dir().join(format!("rf-native-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut bad = Vec::new();
        for (i, (field_pictures, chroma_format)) in [(true, 1), (false, 1), (true, 2), (false, 2)].into_iter().enumerate() {
            for seed in 0..3u64 {
                let data = generate(Options { field_pictures, chroma_format, seed: seed * 7 + i as u64, b_only: None }, 10);
                let p = dir.join(format!("gen{i}_{seed}.m2v"));
                std::fs::write(&p, &data).unwrap();
                let (n, psnr) = compare_with_ffmpeg(&p);
                eprintln!("generated fields={field_pictures} chroma={chroma_format} seed={seed}: {n} frames, worst {psnr:.1} dB");
                if n < 9 || psnr < 40.0 {
                    bad.push(format!("fields={field_pictures} chroma={chroma_format} seed={seed}: {n} frames, {psnr:.1} dB"));
                }
            }
        }
        assert!(bad.is_empty(), "{bad:#?}");
    }

    /// Every B-picture prediction direction × motion type, coded and skipped.
    #[test]
    fn mpeg2_b_picture_modes_match_ffmpeg() {
        use reelforge_codecs::mpeg2::testgen::{generate, Options};
        let dir = std::env::temp_dir().join(format!("rf-native-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for dirs in 1..=3 {
            for mt in 1..=2 {
                for coded in [false, true] {
                    let data = generate(Options { field_pictures: false, chroma_format: 1, seed: 3, b_only: Some((dirs, mt, coded)) }, 4);
                    let p = dir.join(format!("b{dirs}{mt}{coded}.m2v"));
                    std::fs::write(&p, &data).unwrap();
                    let (n, psnr) = compare_with_ffmpeg(&p);
                    eprintln!("B dirs={dirs} mt={mt} coded={coded}: {n} frames, worst {psnr:.1} dB");
                    assert!(n >= 4 && psnr >= 40.0, "B dirs={dirs} mt={mt} coded={coded}: {n} frames, {psnr:.1} dB");
                }
            }
        }
    }

    #[test]
    fn the_decoder_thread_uses_it_and_seeks() {
        let Some(p) = clip("thread.mov", &["-c:v", "prores_ks", "-profile:v", "2"]) else { return };
        let d = crate::DecoderHandle::spawn(&p, None, 4).unwrap();
        let f = d.frames.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
        assert_eq!((f.width, f.height, f.pts), (320, 180, reelforge_core::Time::ZERO));
        d.seek(reelforge_core::Time::from_secs_f64(0.2), 1);
        let f = loop {
            let f = d.frames.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
            if f.generation == 1 {
                break f;
            }
        };
        assert_eq!(f.pts, reelforge_core::Time::from_secs_f64(0.2));
    }

    /// Transport streams are searched by timestamp, so a seek can land mid-GOP or
    /// past the last keyframe; frames must still be exact (right time, real picture).
    /// Indexed containers and every library decoder are checked the same way.
    #[test]
    fn seeking_in_transport_streams_lands_on_the_frame() {
        let cases: Vec<(&str, Vec<&str>)> = vec![
            ("seek264.ts", vec!["-c:v", "libx264", "-bf", "3", "-x264-params", "open-gop=1:keyint=50"]),
            ("seek_m2.ts", vec!["-c:v", "mpeg2video", "-bf", "2", "-g", "12"]),
            ("seek265.ts", vec!["-c:v", "libx265", "-x265-params", "log-level=error:open-gop=1:keyint=50"]),
            ("seek264.mp4", vec!["-c:v", "libx264", "-bf", "3", "-g", "50"]),
            ("seek265.mkv", vec!["-c:v", "libx265", "-x265-params", "log-level=error:open-gop=1:keyint=50"]),
            ("seekvp9.webm", vec!["-c:v", "libvpx-vp9", "-deadline", "realtime", "-cpu-used", "8", "-g", "50"]),
            ("seekav1.mkv", vec!["-c:v", "libsvtav1", "-preset", "12", "-g", "50", "-svtav1-params", "log=0"]),
        ];
        for (name, args) in cases {
            let dir = std::env::temp_dir().join(format!("rf-native-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let p = dir.join(name);
            let ok = std::process::Command::new("ffmpeg")
                .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i", "testsrc2=size=640x360:rate=25:duration=8"])
                .args(&args)
                .arg(&p)
                .status()
                .is_ok_and(|s| s.success());
            if !ok {
                continue;
            }
            // One source, seeking back and forth (the decoder is flushed, not rebuilt).
            let mut src = crate::FrameSource::open(&p, Some((320, 180))).unwrap();
            for (t, frame) in [(5.0, 125), (2.3, 57), (7.7, 192), (7.95, 198), (0.5, 12), (6.1, 152)] {
                let f = src.frame_at_blocking(reelforge_core::Time::from_secs_f64(t), std::time::Duration::from_secs(10)).unwrap();
                assert_eq!(f.pts, reelforge_core::Time::from_secs_f64(frame as f64 / 25.0), "{name} at {t}s");
                // testsrc2's left bar is red: a frame decoded without its references isn't.
                let rgba = f.rgba();
                let mut red = 0;
                let mut all = 0;
                for y in (30..160).step_by(10) {
                    for x in (5..40).step_by(5) {
                        let px = &rgba[(y * f.width as usize + x) * 4..][..3];
                        red += usize::from(px[0] > 180 && px[1] < 80 && px[2] < 80);
                        all += 1;
                    }
                }
                assert!(red * 10 >= all * 7, "{name} at {t}s: only {red}/{all} red");
            }
        }
    }

    #[test]
    fn a_broken_packet_falls_back_to_ffmpeg() {
        // Garbage instead of a ProRes frame: the native decoder refuses it.
        let mut n = open_codec(Codec::ProRes).unwrap();
        assert!(n.dec.send(&[0, 0, 0, 8, b'x', b'x', b'x', b'x'], 0).is_err());
    }

    #[test]
    fn mjpeg_matches_ffmpeg() {
        for (name, pix) in [("m420.avi", "yuvj420p"), ("m422.mov", "yuvj422p"), ("m444.mkv", "yuvj444p")] {
            let Some(p) = clip(name, &["-c:v", "mjpeg", "-q:v", "3", "-pix_fmt", pix]) else { return };
            check(&p, 38.0);
        }
    }
}

#[cfg(test)]
mod bench {
    use super::*;
    /// RF_CLIP=file cargo test --release -p reelforge-media --lib native_speed -- --ignored --nocapture
    #[test]
    #[ignore]
    fn native_speed() {
        let p = std::path::PathBuf::from(std::env::var("RF_CLIP").unwrap());
        crate::init();
        let mut ictx = ff::format::input(&p).unwrap();
        let st = ictx.streams().best(ff::media::Type::Video).unwrap();
        let idx = st.index();
        let params = st.parameters();
        let pkts: Vec<ff::Packet> = ictx.packets().filter(|(s, _)| s.index() == idx).map(|(_, p)| p).collect();
        let mut nat = open(&params).unwrap();
        let t = std::time::Instant::now();
        let mut f = ff::frame::Video::empty();
        let mut n = 0;
        for p in &pkts { nat.send_packet(p).unwrap(); while nat.receive_frame(&mut f) { n += 1; } }
        let nt = t.elapsed().as_secs_f64();
        let mut ffd = ff::codec::context::Context::from_parameters(params.clone()).unwrap().decoder().video().unwrap();
        let t = std::time::Instant::now();
        let mut m = 0;
        for p in &pkts { ffd.send_packet(p).unwrap(); while ffd.receive_frame(&mut f).is_ok() { m += 1; } }
        let ft = t.elapsed().as_secs_f64();
        eprintln!("native: {n} frames {:.1} fps; ffmpeg (1 thread): {m} frames {:.1} fps", n as f64 / nt, m as f64 / ft);
    }
}


/// Decode speed, Reelforge's decoder against FFmpeg's (one thread), on the
/// files in RF_BENCH (comma-separated). Pin to one core (taskset -c 0) for
/// per-core figures. Run with --ignored --nocapture.
#[cfg(test)]
#[test]
#[ignore]
fn bench_native_vs_ffmpeg() {
    use std::time::Instant;
    crate::init();
    for path in std::env::var("RF_BENCH").unwrap().split(',') {
        let mut ictx = ff::format::input(path).unwrap();
        let st = ictx.streams().best(ff::media::Type::Video).unwrap();
        let index = st.index();
        let params = st.parameters();
        let packets: Vec<ff::Packet> = ictx.packets().filter(|(s, _)| s.index() == index).map(|(_, p)| p).collect();
        let reps = 3;
        let mut nat = open(&params).expect("native decoder");
        let mut f = ff::frame::Video::empty();
        let t = Instant::now();
        for _ in 0..reps {
            for p in &packets {
                nat.send_packet(p).unwrap();
                while nat.receive_frame(&mut f) {}
            }
        }
        let native = t.elapsed().as_secs_f64() * 1000.0 / (reps * packets.len()) as f64;
        let mut ctx = ff::codec::context::Context::from_parameters(params.clone()).unwrap();
        ctx.set_threading(ff::codec::threading::Config { kind: ff::codec::threading::Type::Frame, count: 1, ..Default::default() });
        let mut ffd = ctx.decoder().video().unwrap();
        let t = Instant::now();
        for _ in 0..reps {
            for p in &packets {
                ffd.send_packet(p).unwrap();
                while ffd.receive_frame(&mut f).is_ok() {}
            }
        }
        let ffm = t.elapsed().as_secs_f64() * 1000.0 / (reps * packets.len()) as f64;
        let name = std::path::Path::new(path).file_name().unwrap().to_string_lossy();
        println!("BENCH {name}: Reelforge {native:.1} ms/frame, FFmpeg (1 thread) {ffm:.1} ms/frame, ratio {:.2}", native / ffm);
    }
}
