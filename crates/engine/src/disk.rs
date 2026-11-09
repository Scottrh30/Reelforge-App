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
    unused_proxies_in(&reelforge_media::proxy::media_cache_dir().join("proxies"), project)
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

/// Move the proxies and rendered previews kept in `from` to `to` (renamed
/// where they can be, copied then deleted across drives): (files moved,
/// bytes, files that couldn't be).
pub fn move_media_cache(from: &Path, to: &Path) -> (usize, u64, usize) {
    let (mut n, mut bytes, mut failed) = (0, 0, 0);
    for sub in ["proxies", "render"] {
        let (a, b) = (from.join(sub), to.join(sub));
        let Ok(rd) = std::fs::read_dir(&a) else { continue };
        if std::fs::create_dir_all(&b).is_err() {
            failed += 1;
            continue;
        }
        for f in rd.flatten() {
            let Ok(m) = f.metadata() else { continue };
            if !m.is_file() {
                continue;
            }
            let dst = b.join(f.file_name());
            let ok = std::fs::rename(f.path(), &dst).is_ok() || (std::fs::copy(f.path(), &dst).is_ok() && std::fs::remove_file(f.path()).is_ok());
            if ok {
                n += 1;
                bytes += m.len();
            } else {
                // A half-made copy goes; the original stays where it was.
                if f.path().exists() {
                    let _ = std::fs::remove_file(&dst);
                }
                failed += 1;
            }
        }
    }
    (n, bytes, failed)
}

/// Delete the proxies of media not in `project`: (files, bytes) deleted.
pub fn delete_unused_proxies(project: &Project) -> (usize, u64) {
    delete_unused_proxies_in(&reelforge_media::proxy::media_cache_dir().join("proxies"), project)
}

/// The same, in proxy folder `dir`.
pub fn delete_unused_proxies_in(dir: &Path, project: &Project) -> (usize, u64) {
    unused_proxies_in(dir, project).into_iter().filter(|(p, _)| std::fs::remove_file(p).is_ok()).fold((0, 0), |(n, b), (_, len)| (n + 1, b + len))
}

/// An Object removal fill folder in the cache.
#[derive(Clone, Debug, PartialEq)]
pub struct FillDir {
    pub path: PathBuf,
    pub bytes: u64,
    /// This project uses it, or the project that made it still does.
    pub used: bool,
}

/// Every Object removal fill folder in `dir`, and whether it's still used:
/// by `project` (saved at `path`, if it is), or by the project that made it
/// (its saved file still names the folder). One whose project no longer
/// exists or no longer names it is unused.
pub fn fills_in(dir: &Path, project: &Project, path: Option<&Path>) -> Vec<FillDir> {
    let here: std::collections::HashSet<PathBuf> = project
        .sequences
        .iter()
        .flat_map(|s| s.tracks.iter().flat_map(|t| t.clips.iter()))
        .flat_map(|c| c.effects.iter())
        .filter_map(|e| match &e.data {
            Some(reelforge_core::effects::EffectData::Fill(d)) => Some(d.clone()),
            Some(reelforge_core::effects::EffectData::Sky { matte: Some(d), .. }) => Some(d.clone()),
            _ => None,
        })
        .collect();
    let mut texts: std::collections::HashMap<String, Option<String>> = Default::default();
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<FillDir> = rd
        .flatten()
        .filter(|f| f.metadata().is_ok_and(|m| m.is_dir()))
        .map(|f| {
            let p = f.path();
            let bytes = std::fs::read_dir(&p).map(|rd| rd.flatten().filter_map(|x| x.metadata().ok()).map(|m| m.len()).sum()).unwrap_or(0);
            let name = f.file_name().to_string_lossy().to_string();
            let owner = std::fs::read_to_string(p.join("owner.txt")).unwrap_or_default();
            let mine = path.is_some_and(|q| q.display().to_string() == owner) || (path.is_none() && owner.is_empty());
            let used = here.contains(&p)
                || (!mine && !owner.is_empty() && texts.entry(owner.clone()).or_insert_with(|| std::fs::read_to_string(&owner).ok()).as_ref().is_some_and(|t| t.contains(&name)));
            FillDir { path: p, bytes, used }
        })
        .collect();
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

pub fn fills(project: &Project, path: Option<&Path>) -> Vec<FillDir> {
    fills_in(&reelforge_media::proxy::media_cache_dir().join("fills"), project, path)
}

/// Delete the unused fill folders: (folders, bytes) deleted.
pub fn delete_unused_fills(project: &Project, path: Option<&Path>) -> (usize, u64) {
    fills(project, path).into_iter().filter(|f| !f.used && std::fs::remove_dir_all(&f.path).is_ok()).fold((0, 0), |(n, b), f| (n + 1, b + f.bytes))
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
    fn the_media_cache_moves_to_another_folder() {
        let base = std::env::temp_dir().join(format!("rf_move_cache_{}", std::process::id()));
        let (from, to) = (base.join("a"), base.join("b"));
        for (sub, name, len) in [("proxies", "x-1.mp4", 100), ("render", "r1", 30), ("render", "r2", 20)] {
            std::fs::create_dir_all(from.join(sub)).unwrap();
            std::fs::write(from.join(sub).join(name), vec![1u8; len]).unwrap();
        }
        std::fs::write(from.join("keep.txt"), b"other things stay").unwrap();
        assert_eq!(move_media_cache(&from, &to), (3, 150, 0));
        assert_eq!(std::fs::read(to.join("proxies/x-1.mp4")).unwrap().len(), 100);
        assert!(to.join("render/r2").exists() && !from.join("render/r1").exists() && from.join("keep.txt").exists());
        // Nothing there: nothing to do.
        assert_eq!(move_media_cache(&base.join("none"), &to), (0, 0, 0));
        let _ = std::fs::remove_dir_all(&base);
    }

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
        p.media.push(reelforge_core::project::MediaAsset { id: reelforge_core::MediaId(1), path: src.into(), name: "a.mov".into(), info, conform: None });
        let old = unused_proxies_in(&dir, &p);
        assert_eq!(old.len(), 1);
        assert_eq!(old[0].1, 300);
        assert_eq!(delete_unused_proxies_in(&dir, &p), (1, 300));
        assert!(unused_proxies_in(&dir, &p).is_empty());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2, "its own proxies kept");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod fill_tests {
    use super::*;
    use reelforge_core::effects::{Effect, EffectData, EffectKind};
    use reelforge_core::*;

    #[test]
    fn unused_fills_are_found_and_others_projects_kept() {
        let root = std::env::temp_dir().join(format!("rf-fills-{}", std::process::id()));
        let dir = root.join("fills");
        let made = |name: &str, owner: &str| {
            let p = dir.join(name);
            std::fs::create_dir_all(&p).unwrap();
            std::fs::write(p.join("000000.rff"), [0u8; 100]).unwrap();
            std::fs::write(p.join("owner.txt"), owner).unwrap();
            p
        };
        let here = root.join("here.rfproj");
        let other = root.join("other.rfproj");
        let used = made("1-0-a", &here.display().to_string());
        let replaced = made("1-0-b", &here.display().to_string());
        let others = made("2-0-c", &other.display().to_string());
        let gone = made("3-0-d", &root.join("deleted.rfproj").display().to_string());
        std::fs::write(&other, "{ \"fill\": \"…/fills/2-0-c\" }").unwrap();
        let mut p = Project::new();
        let mut seq = Sequence::new("S", 64, 32, FrameRate::new(25, 1));
        let mut c = Clip::new_plane(ClipId(1), Time::ZERO, Time::from_secs_f64(1.0), [0, 0, 0, 255]);
        let mut fx = Effect::new(EffectKind::ObjectRemoval);
        fx.data = Some(EffectData::Fill(used.clone()));
        c.effects.push(fx);
        seq.tracks[0].clips = vec![c];
        p.sequences = vec![seq];
        let list = fills_in(&dir, &p, Some(&here));
        let is_used = |q: &Path| list.iter().find(|f| f.path == q).unwrap().used;
        assert!(is_used(&used) && is_used(&others));
        assert!(!is_used(&replaced) && !is_used(&gone));
        assert!(list.iter().all(|f| f.bytes >= 100));
        std::fs::remove_dir_all(&root).unwrap();
    }
}
