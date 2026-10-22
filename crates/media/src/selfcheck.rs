//! Check decoders (Help → About & diagnostics): on this PC, encode a short
//! sample in each format Reelforge decodes itself, decode it with Reelforge's
//! decoder (with AVX2 and with the portable code) and with FFmpeg's, and
//! compare every frame. A processor- or build-specific fault shows up as a
//! mismatch here instead of as glitches in someone's edit.

use ffmpeg_next as ff;
use ff::software::scaling::{context::Context as Scaler, flag::Flags};
use std::path::Path;
use std::time::Instant;

/// One format's result.
#[derive(Clone, Debug, serde::Serialize)]
pub struct DecoderCheck {
    pub format: &'static str,
    /// "ok", "mismatch", "error" or "skipped".
    pub status: &'static str,
    /// Worst frame's PSNR against FFmpeg (dB) with AVX2 (or the default path).
    pub psnr: Option<f64>,
    /// The same with the portable code, when the CPU has AVX2.
    pub psnr_portable: Option<f64>,
    pub frames: usize,
    pub detail: String,
}

/// Below this the pictures visibly differ; Reelforge's decoders score 55–99 dB.
pub const MIN_PSNR: f64 = 50.0;

struct Sample {
    format: &'static str,
    /// Encoders to try, in order.
    encoders: &'static [&'static str],
    pix: ff::format::Pixel,
    ext: &'static str,
    opts: &'static [(&'static str, &'static str)],
    b_frames: usize,
    alpha: bool,
}

const W: u32 = 640;
const H: u32 = 360;
const FRAMES: usize = 12;

fn samples() -> Vec<Sample> {
    use ff::format::Pixel as P;
    let s = |format, encoders, pix, ext, opts, b_frames| Sample { format, encoders, pix, ext, opts, b_frames, alpha: false };
    vec![
        s("H.264", &["libx264"], P::YUV420P, "mp4", &[("preset", "veryfast")], 2),
        s("HEVC", &["libx265"], P::YUV420P, "mp4", &[("preset", "ultrafast"), ("x265-params", "log-level=error")], 2),
        s("HEVC 10-bit", &["libx265"], P::YUV420P10LE, "mp4", &[("preset", "ultrafast"), ("x265-params", "log-level=error")], 2),
        s("VP9", &["libvpx-vp9"], P::YUV420P, "webm", &[("deadline", "realtime"), ("cpu-used", "8")], 0),
        s("AV1", &["libaom-av1", "libsvtav1", "librav1e"], P::YUV420P, "mkv", &[("cpu-used", "8"), ("preset", "12"), ("speed", "10")], 0),
        s("MPEG-2", &["mpeg2video"], P::YUV420P, "mpg", &[("b", "6M")], 2),
        s("ProRes 422 HQ", &["prores_ks"], P::YUV422P10LE, "mov", &[("profile", "3")], 0),
        Sample { alpha: true, ..s("ProRes 4444 with alpha", &["prores_ks"], P::YUVA444P10LE, "mov", &[("profile", "4")], 0) },
        s("DNxHR HQ", &["dnxhd"], P::YUV422P, "mov", &[("profile", "dnxhr_hq")], 0),
        s("DNxHR HQX (10-bit)", &["dnxhd"], P::YUV422P10LE, "mov", &[("profile", "dnxhr_hqx")], 0),
        s("Motion JPEG", &["mjpeg"], P::YUVJ422P, "avi", &[("q:v", "3")], 0),
    ]
}

/// A test picture: gradients, a moving disc and film-like grain (the same every run).
fn picture(n: usize, alpha: bool) -> Vec<u8> {
    let mut px = vec![0u8; (W * H * 4) as usize];
    let mut seed = 0x9E37_79B9u32 ^ (n as u32).wrapping_mul(2_654_435_761);
    let (cx, cy) = (100.0 + 30.0 * n as f32, 180.0 + 8.0 * n as f32);
    for y in 0..H {
        for x in 0..W {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let grain = (seed & 15) as i32 - 8;
            let d = ((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)).sqrt();
            let disc = d < 60.0;
            let r = if disc { 230 } else { (x * 255 / W) as i32 };
            let g = if disc { 60 } else { (y * 255 / H) as i32 };
            let b = if disc { 40 } else { ((x + y + 8 * n as u32) % 256) as i32 };
            let i = ((y * W + x) * 4) as usize;
            px[i] = (r + grain).clamp(0, 255) as u8;
            px[i + 1] = (g + grain).clamp(0, 255) as u8;
            px[i + 2] = (b + grain).clamp(0, 255) as u8;
            px[i + 3] = if alpha { (x * 255 / W) as u8 } else { 255 };
        }
    }
    px
}

/// Encode the sample to `path` with the first encoder that exists; its name.
fn encode(s: &Sample, path: &Path) -> Result<&'static str, String> {
    let Some((name, codec)) = s.encoders.iter().find_map(|n| ff::encoder::find_by_name(n).map(|c| (*n, c))) else {
        return Err(format!("this FFmpeg has no {} encoder", s.encoders.join(" or ")));
    };
    let mut octx = ff::format::output(path).map_err(|e| e.to_string())?;
    let global = octx.format().flags().contains(ff::format::Flags::GLOBAL_HEADER);
    let mut enc = ff::codec::context::Context::new_with_codec(codec).encoder().video().map_err(|e| e.to_string())?;
    let tb = ff::Rational::new(1, 25);
    enc.set_width(W);
    enc.set_height(H);
    enc.set_format(s.pix);
    enc.set_time_base(tb);
    enc.set_frame_rate(Some(ff::Rational::new(25, 1)));
    enc.set_gop(6);
    enc.set_max_b_frames(s.b_frames);
    enc.set_colorspace(ff::util::color::Space::BT709);
    if global {
        enc.set_flags(ff::codec::Flags::GLOBAL_HEADER);
    }
    let mut opts = ff::Dictionary::new();
    for (k, v) in s.opts {
        if *k == "q:v" {
            enc.set_flags(ff::codec::Flags::QSCALE);
            enc.set_global_quality(v.parse::<i32>().unwrap_or(3) * 118);
        } else if *k == "b" {
            enc.set_bit_rate(6_000_000);
        } else {
            opts.set(k, v);
        }
    }
    let mut enc = enc.open_with(opts).map_err(|e| format!("{name} didn't open: {e}"))?;
    let index = {
        let mut st = octx.add_stream(codec).map_err(|e| e.to_string())?;
        st.set_parameters(&enc);
        st.set_time_base(tb);
        st.set_rate(ff::Rational::new(25, 1));
        st.set_avg_frame_rate(ff::Rational::new(25, 1));
        st.index()
    };
    octx.write_header().map_err(|e| e.to_string())?;
    let mut scaler = Scaler::get(ff::format::Pixel::RGBA, W, H, s.pix, W, H, Flags::BICUBIC).map_err(|e| e.to_string())?;
    let st_tb = octx.stream(index).map(|s| s.time_base()).unwrap_or(tb);
    let write = |enc: &mut ff::encoder::Video, octx: &mut ff::format::context::Output| -> Result<(), String> {
        let mut p = ff::Packet::empty();
        while enc.receive_packet(&mut p).is_ok() {
            p.set_stream(index);
            p.rescale_ts(tb, st_tb);
            p.write_interleaved(octx).map_err(|e| e.to_string())?;
        }
        Ok(())
    };
    for n in 0..FRAMES {
        let mut src = ff::frame::Video::new(ff::format::Pixel::RGBA, W, H);
        let stride = src.stride(0);
        let px = picture(n, s.alpha);
        for y in 0..H as usize {
            src.data_mut(0)[y * stride..y * stride + W as usize * 4].copy_from_slice(&px[y * W as usize * 4..(y + 1) * W as usize * 4]);
        }
        let mut out = ff::frame::Video::new(s.pix, W, H);
        scaler.run(&src, &mut out).map_err(|e| e.to_string())?;
        out.set_pts(Some(n as i64));
        out.set_color_space(ff::util::color::Space::BT709);
        enc.send_frame(&out).map_err(|e| e.to_string())?;
        write(&mut enc, &mut octx)?;
    }
    enc.send_eof().map_err(|e| e.to_string())?;
    write(&mut enc, &mut octx)?;
    octx.write_trailer().map_err(|e| e.to_string())?;
    Ok(name)
}

/// Run the check on every format, calling `progress(done, total, format)` as it goes.
pub fn check_decoders(mut progress: impl FnMut(usize, usize, &str)) -> Vec<DecoderCheck> {
    crate::init();
    let dir = std::env::temp_dir().join(format!("reelforge-check-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let had_avx2 = reelforge_codecs::idct::avx2_in_use();
    let list = samples();
    let mut out = Vec::new();
    for (i, s) in list.iter().enumerate() {
        progress(i, list.len(), s.format);
        let t = Instant::now();
        let path = dir.join(format!("sample{i}.{}", s.ext));
        let r = match encode(s, &path) {
            Err(e) => DecoderCheck { format: s.format, status: "skipped", psnr: None, psnr_portable: None, frames: 0, detail: e },
            Ok(encoder) => {
                reelforge_codecs::idct::set_avx2_allowed(had_avx2);
                let fast = crate::native::compare(&path);
                let portable = if had_avx2 {
                    reelforge_codecs::idct::set_avx2_allowed(false);
                    let p = crate::native::compare(&path);
                    reelforge_codecs::idct::set_avx2_allowed(true);
                    Some(p)
                } else {
                    None
                };
                match (fast, portable) {
                    (Err(e), _) | (_, Some(Err(e))) => DecoderCheck { format: s.format, status: "error", psnr: None, psnr_portable: None, frames: 0, detail: e },
                    (Ok((frames, psnr)), p) => {
                        let pp = p.and_then(|r| r.ok()).map(|(_, v)| v);
                        let worst = pp.map_or(psnr, |v| v.min(psnr));
                        let ok = frames >= FRAMES && worst >= MIN_PSNR;
                        let detail = format!(
                            "made with {encoder}; {frames} frames; {}{} ({:.1} s)",
                            if psnr >= 98.0 { "identical to FFmpeg".to_string() } else { format!("worst frame {psnr:.1} dB from FFmpeg") },
                            pp.map(|v| if v >= 98.0 { ", portable code identical".to_string() } else { format!(", portable code {v:.1} dB") }).unwrap_or_default(),
                            t.elapsed().as_secs_f64()
                        );
                        DecoderCheck { format: s.format, status: if ok { "ok" } else { "mismatch" }, psnr: Some(psnr), psnr_portable: pp, frames, detail }
                    }
                }
            }
        };
        log::info!("decoder check: {} {}: {}", r.format, r.status, r.detail);
        out.push(r);
    }
    reelforge_codecs::idct::set_avx2_allowed(had_avx2);
    progress(list.len(), list.len(), "");
    let _ = std::fs::remove_dir_all(&dir);
    *LAST.lock().unwrap_or_else(|e| e.into_inner()) = Some(out.clone());
    out
}

static LAST: std::sync::Mutex<Option<Vec<DecoderCheck>>> = std::sync::Mutex::new(None);

/// The last check's results this session, one line each, for the diagnostics report.
pub fn last_summary() -> Option<String> {
    let last = LAST.lock().ok()?.clone()?;
    Some(last.iter().map(|c| format!("{} {}: {}", c.format, c.status, c.detail)).collect::<Vec<_>>().join("; "))
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_decoder_matches_ffmpeg_here() {
        let r = super::check_decoders(|_, _, _| {});
        for c in &r {
            eprintln!("{:24} {:8} {}", c.format, c.status, c.detail);
        }
        assert!(r.iter().all(|c| c.status == "ok" || c.status == "skipped"), "{r:#?}");
        assert!(r.iter().filter(|c| c.status == "ok").count() >= 8);
    }
}
