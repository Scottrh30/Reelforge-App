//! Editing proxies: small, all-keyframe copies of heavy footage.
//!
//! 4K and HEVC files are slow to decode, and long-GOP codecs make seeking
//! slower still (every seek decodes forward from a keyframe). A proxy is the
//! same footage at no more than 960×540, with every frame a keyframe, so the
//! viewer can jump anywhere instantly. Proxies are only for the viewer:
//! export always reads the original files.
//!
//! Proxies live in the user's cache folder, named after the source file and
//! a hash of its path, size and modification time, so they're found again for
//! any project that uses the same file and are rebuilt if the file changes.

use crate::encode::{MovieWriter, Quality, WriterSettings};
use crate::source::FrameSource;
use anyhow::{anyhow, bail, Context as _, Result};
use reelforge_core::{MediaInfo, Time};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Proxies fit inside this size.
pub const PROXY_MAX: (u32, u32) = (960, 540);

/// Where proxies (and other caches) are kept.
pub fn cache_dir() -> PathBuf {
    let env = |k: &str| std::env::var_os(k).map(PathBuf::from);
    let base = if cfg!(windows) {
        env("LOCALAPPDATA").map(|p| p.join("Reelforge"))
    } else if cfg!(target_os = "macos") {
        env("HOME").map(|h| h.join("Library/Caches/Reelforge"))
    } else {
        env("XDG_CACHE_HOME").or_else(|| env("HOME").map(|h| h.join(".cache"))).map(|p| p.join("reelforge"))
    };
    base.unwrap_or_else(|| std::env::temp_dir().join("reelforge"))
}

/// The proxy file for `src` (whether or not it exists yet).
pub fn proxy_path(src: &Path) -> PathBuf {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    std::path::absolute(src).unwrap_or_else(|_| src.to_owned()).hash(&mut h);
    if let Ok(m) = std::fs::metadata(src) {
        m.len().hash(&mut h);
        if let Ok(t) = m.modified() {
            t.hash(&mut h);
        }
    }
    let stem = src.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let stem: String = stem.chars().filter(|c| c.is_alphanumeric() || "-_ ".contains(*c)).take(40).collect();
    cache_dir().join("proxies").join(format!("{stem}-{:016x}.mp4", h.finish()))
}

/// The proxy for `src`, if one has been made.
pub fn existing_proxy(src: &Path) -> Option<PathBuf> {
    let p = proxy_path(src);
    p.is_file().then_some(p)
}

/// Is this footage heavy enough that a proxy is worth making automatically?
/// Anything above 1080p, and 720p+ in codecs that are slow to decode.
pub fn worth_a_proxy(info: &MediaInfo) -> bool {
    let Some(v) = &info.video else { return false };
    if info.duration <= Time::ZERO {
        return false; // still image
    }
    let px = v.width as u64 * v.height as u64;
    let heavy_codec = matches!(v.codec.as_str(), "hevc" | "av1" | "vp9" | "prores" | "dnxhd");
    px > 1920 * 1080 || (heavy_codec && px >= 1280 * 720)
}

/// Make the proxy for `src`. Calls `progress` with 0–1 as it goes; set
/// `cancel` to stop (the partial file is removed). Returns the proxy's path.
pub fn make_proxy(src: &Path, info: &MediaInfo, cancel: &AtomicBool, mut progress: impl FnMut(f32)) -> Result<PathBuf> {
    let dst = proxy_path(src);
    if dst.is_file() {
        return Ok(dst);
    }
    let v = info.video.as_ref().ok_or_else(|| anyhow!("no video to make a proxy of"))?;
    let rate = v.frame_rate;
    let frames = info.duration.to_frame(rate).max(1);
    std::fs::create_dir_all(dst.parent().unwrap()).context("creating the proxy folder")?;
    let tmp = dst.with_extension("partial.mp4");

    let mut source = FrameSource::open(src, Some(PROXY_MAX))?;
    let first = source.frame_at_blocking(Time::ZERO, Duration::from_secs(30)).ok_or_else(|| anyhow!("no frames"))?;
    let mut writer = MovieWriter::create(&tmp, &WriterSettings {
        width: first.width,
        height: first.height,
        input_size: None,
        frame_rate: rate,
        quality: Quality::Medium,
        audio_rate: None,
        proxy: true,
        ..Default::default()
    })?;
    let blank = vec![0u8; (first.width * first.height * 4) as usize];
    // One proxy frame per nominal frame time, so the proxy lines up with the
    // original even when the camera recorded a variable frame rate.
    for i in 0..frames {
        if cancel.load(Ordering::Relaxed) {
            drop(writer);
            let _ = std::fs::remove_file(&tmp);
            bail!("cancelled");
        }
        let t = Time::from_frame(i, rate);
        let f = source.frame_at_blocking(t, Duration::from_secs(30));
        match &f {
            Some(f) if f.width == first.width && f.height == first.height => writer.write_video(&f.rgba)?,
            _ => writer.write_video(&blank)?,
        }
        progress((i + 1) as f32 / frames as f32);
    }
    writer.finish()?;
    std::fs::rename(&tmp, &dst).context("saving the proxy")?;
    Ok(dst)
}

#[cfg(test)]
mod tests {
    use super::*;
    use reelforge_core::{FrameRate, Rational, VideoStreamInfo};

    fn info(w: u32, h: u32, codec: &str, secs: f64) -> MediaInfo {
        MediaInfo {
            duration: Time::from_secs_f64(secs),
            video: Some(VideoStreamInfo { width: w, height: h, frame_rate: FrameRate::new(30, 1), codec: codec.into(), time_base: Rational::new(1, 30) }),
            audio: None,
            container: String::new(),
        }
    }

    #[test]
    fn which_footage_gets_a_proxy() {
        assert!(worth_a_proxy(&info(3840, 2160, "h264", 10.0)));
        assert!(!worth_a_proxy(&info(1920, 1080, "h264", 10.0)));
        assert!(worth_a_proxy(&info(1920, 1080, "hevc", 10.0)));
        assert!(!worth_a_proxy(&info(3840, 2160, "png", 0.0)), "stills never");
    }

    #[test]
    fn proxy_names_are_stable_and_distinct() {
        let a = proxy_path(Path::new("/x/My Clip (1).mp4"));
        assert_eq!(a, proxy_path(Path::new("/x/My Clip (1).mp4")));
        assert_ne!(a, proxy_path(Path::new("/y/My Clip (1).mp4")));
        assert!(a.file_name().unwrap().to_string_lossy().starts_with("My Clip 1-"));
    }
}
