//! Disk space: how much is free where Reelforge writes (rendered previews,
//! proxies, autosaves, the project, exports), whether that's running short,
//! what can be cleared to make room, and roughly how big an export will be,
//! so a render or an export doesn't fail part-way with the drive full.

use reelforge_core::Project;
use reelforge_export::ExportSettings;
use reelforge_media::VideoCodec;
use serde::Serialize;
use std::path::{Path, PathBuf};

/// Below this much free space the app warns, stops keeping rendered frames
/// on disk and doesn't start proxies on its own.
pub const LOW: u64 = 2 << 30;
/// An export stops (cleanly, saying why) when its drive gets this full.
pub const EXPORT_STOP: u64 = 64 << 20;
/// Room left over after an export, on top of its estimated size.
pub const EXPORT_SPARE: u64 = 256 << 20;

/// Free space (bytes) on the drive holding `path` (or its nearest existing
/// folder), if it can be found out.
pub fn free_bytes(path: &Path) -> Option<u64> {
    let mut p = path;
    while !p.exists() {
        p = p.parent()?;
    }
    sys::free(p)
}

#[cfg(unix)]
mod sys {
    use std::os::unix::ffi::OsStrExt;
    pub fn free(p: &std::path::Path) -> Option<u64> {
        let c = std::ffi::CString::new(p.as_os_str().as_bytes()).ok()?;
        let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 {
            return None;
        }
        Some(s.f_bavail as u64 * s.f_frsize as u64)
    }
}

#[cfg(windows)]
mod sys {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    extern "system" {
        fn GetDiskFreeSpaceExW(dir: *const u16, avail: *mut u64, total: *mut u64, free: *mut u64) -> i32;
    }
    pub fn free(p: &std::path::Path) -> Option<u64> {
        let w: Vec<u16> = p.as_os_str().encode_wide().chain(Some(0)).collect();
        let (mut avail, mut total, mut free) = (0u64, 0u64, 0u64);
        (unsafe { GetDiskFreeSpaceExW(w.as_ptr(), &mut avail, &mut total, &mut free) } != 0).then_some(avail)
    }
}

#[cfg(not(any(unix, windows)))]
mod sys {
    pub fn free(_: &std::path::Path) -> Option<u64> {
        None
    }
}

/// Space running short somewhere Reelforge writes, and what could be cleared.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DiskLow {
    /// Bytes free on the fullest of those drives.
    pub free: u64,
    /// A folder on that drive (for the message).
    pub place: String,
    /// Rendered previews kept on disk, bytes (clearable).
    pub render_bytes: u64,
    /// Proxies of media not in the open project: how many, and bytes (deletable).
    pub old_proxies: usize,
    pub old_proxy_bytes: u64,
}

/// Proxy files not belonging to any media in `project` (in any proxy format):
/// left over from other projects, or from media since removed.
pub fn unused_proxies(project: &Project) -> Vec<(PathBuf, u64)> {
    unused_proxies_in(&reelforge_media::proxy::cache_dir().join("proxies"), project)
}

/// The same, in proxy folder `dir`.
pub fn unused_proxies_in(dir: &Path, project: &Project) -> Vec<(PathBuf, u64)> {
    let used: std::collections::HashSet<std::ffi::OsString> = project
        .media
        .iter()
        .flat_map(|m| reelforge_media::proxy::ProxyFormat::ALL.map(|f| reelforge_media::proxy::proxy_path_as(&m.path, f)))
        .filter_map(|p| p.file_name().map(|n| n.to_owned()))
        .collect();
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    rd.flatten()
        .filter_map(|f| {
            let m = f.metadata().ok()?;
            (m.is_file() && !used.contains(&f.file_name())).then(|| (f.path(), m.len()))
        })
        .collect()
}

/// Delete the proxies of media not in `project`: (files, bytes) deleted.
pub fn delete_unused_proxies(project: &Project) -> (usize, u64) {
    delete_unused_proxies_in(&reelforge_media::proxy::cache_dir().join("proxies"), project)
}

/// The same, in proxy folder `dir`.
pub fn delete_unused_proxies_in(dir: &Path, project: &Project) -> (usize, u64) {
    unused_proxies_in(dir, project).into_iter().filter(|(p, _)| std::fs::remove_file(p).is_ok()).fold((0, 0), |(n, b), (_, len)| (n + 1, b + len))
}

/// Roughly how big an export will be (bytes), erring on the big side.
pub fn estimate_export_bytes(s: &ExportSettings, fps: f64) -> u64 {
    let secs = (s.end - s.start).as_secs_f64().max(0.0);
    let pixels = s.width as f64 * s.height as f64;
    // Bits per pixel per frame, from each format's usual data rates.
    let video_bits = match s.video {
        None => 0.0,
        Some(_) if s.bitrate_kbps.is_some() => s.bitrate_kbps.unwrap_or(0) as f64 * 1000.0 * secs,
        Some(v) => {
            let bpp = match v {
                VideoCodec::H264 | VideoCodec::H265 => 0.15,
                VideoCodec::ProRes422Proxy => 0.8,
                VideoCodec::ProRes422Lt => 1.7,
                VideoCodec::ProRes422 => 2.4,
                VideoCodec::ProRes422Hq => 3.6,
                VideoCodec::ProRes4444 => 5.4,
                VideoCodec::ProRes4444Xq => 8.1,
                VideoCodec::DnxhrLb => 0.7,
                VideoCodec::DnxhrSq => 2.0,
                VideoCodec::DnxhrHq | VideoCodec::DnxhrHqx => 3.2,
                VideoCodec::Dnxhr444 => 6.4,
                VideoCodec::Dnxhd | VideoCodec::Dnxhd10 => 3.6,
                VideoCodec::Cineform => 3.2,
                VideoCodec::Cineform4444 => 6.4,
                VideoCodec::PngSequence => 16.0,
                VideoCodec::JpegSequence => 2.5,
                VideoCodec::TiffSequence => if s.alpha { 32.0 } else { 24.0 },
            };
            bpp * pixels * fps * secs
        }
    };
    // Audio: AAC/PCM alongside video, or a 24-bit stereo WAV.
    let audio_bits = if s.video.is_none() {
        48_000.0 * 2.0 * 24.0 * secs
    } else if s.include_audio {
        48_000.0 * 2.0 * 24.0 * secs
    } else {
        0.0
    };
    ((video_bits + audio_bits) / 8.0 * 1.2) as u64
}

/// "1.4 GB", "350 MB".
pub fn size_text(bytes: u64) -> String {
    let gb = bytes as f64 / (1u64 << 30) as f64;
    if gb >= 1.0 {
        format!("{gb:.1} GB")
    } else {
        format!("{} MB", (bytes >> 20).max(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reelforge_core::Time;

    #[test]
    fn free_space_is_found_for_paths_that_dont_exist_yet() {
        let here = free_bytes(&std::env::temp_dir()).expect("free space");
        assert!(here > 0);
        let deeper = free_bytes(&std::env::temp_dir().join("rf_not_there/a/b/c.mp4")).unwrap();
        // The same drive (allowing for other writes in between).
        assert!((here as i64 - deeper as i64).abs() < 1 << 30);
    }

    #[test]
    fn export_sizes_are_estimated_on_the_big_side() {
        let s = |video: Option<VideoCodec>, bitrate: Option<u32>| ExportSettings {
            path: "x.mov".into(),
            width: 1920,
            height: 1080,
            quality: reelforge_media::Quality::High,
            include_audio: true,
            start: Time::ZERO,
            end: Time::from_secs_f64(60.0),
            video,
            hardware: false,
            fit: Default::default(),
            sequence: 0,
            color: Default::default(),
            bitrate_kbps: bitrate,
            alpha: false,
        };
        let mb = |b: u64| b as f64 / 1e6;
        // ProRes 422 HQ at 1080p25 is about 184 Mbit/s: a minute is about 1.4 GB.
        let hq = mb(estimate_export_bytes(&s(Some(VideoCodec::ProRes422Hq), None), 25.0));
        assert!((1400.0..2200.0).contains(&hq), "{hq} MB");
        // A given bitrate: 10 Mbit/s for a minute is 75 MB, plus audio and the margin.
        let h264 = mb(estimate_export_bytes(&s(Some(VideoCodec::H264), Some(10_000)), 25.0));
        assert!((90.0..120.0).contains(&h264), "{h264} MB");
        // A WAV: 24-bit stereo 48 kHz is about 17 MB a minute.
        let wav = mb(estimate_export_bytes(&s(None, None), 25.0));
        assert!((17.0..25.0).contains(&wav), "{wav} MB");
        assert_eq!(size_text(3 << 29), "1.5 GB");
        assert_eq!(size_text(300 << 20), "300 MB");
    }

    #[test]
    fn unused_proxies_are_those_of_media_not_in_the_project() {
        let dir = std::env::temp_dir().join(format!("rf_disk_proxies_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut p = Project::default();
        let src = Path::new("/footage/a.mov");
        // Its proxies in two formats are kept; another file's goes.
        for f in [reelforge_media::proxy::ProxyFormat::H264, reelforge_media::proxy::ProxyFormat::DnxhrLb] {
            let name = reelforge_media::proxy::proxy_path_as(src, f).file_name().unwrap().to_owned();
            std::fs::write(dir.join(name), vec![0u8; 100]).unwrap();
        }
        std::fs::write(dir.join("old-0000000000000001.mp4"), vec![0u8; 300]).unwrap();
        let info = reelforge_core::project::MediaInfo { duration: Time::ZERO, video: None, audio: None, container: String::new() };
        p.media.push(reelforge_core::project::MediaAsset { id: reelforge_core::MediaId(1), path: src.into(), name: "a.mov".into(), info });
        let old = unused_proxies_in(&dir, &p);
        assert_eq!(old.len(), 1);
        assert_eq!(old[0].1, 300);
        assert_eq!(delete_unused_proxies_in(&dir, &p), (1, 300));
        assert!(unused_proxies_in(&dir, &p).is_empty());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2, "its own proxies kept");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
