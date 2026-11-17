// What a project or media file goes through once it's read in. Shared with
// crates/export/tests/fuzz_regressions.rs (by include!), which replays what the
// fuzzer found.

use reelforge_core::{Project, Time};

/// Do with a project what the app does once it's open: every sequence's
/// pictures worked out at a few moments (nested sequences included), every
/// clip's timing, transform, effects, title and mesh at those moments, its
/// edit points, and saving it again. A project read from a damaged file
/// must survive all of it.
pub fn exercise(p: &Project) {
    let sources = std::collections::HashMap::new();
    for (si, seq) in p.sequences.iter().enumerate().take(16) {
        let end = seq.tracks.iter().map(|t| t.end()).max().unwrap_or(Time::ZERO);
        let size = [seq.width as f32, seq.height as f32];
        let frame = seq.frame_rate.frame_duration();
        let _ = seq.edit_points();
        for k in 0..4u32 {
            let t = Time::from_secs_f64(end.as_secs_f64() * f64::from(k) / 3.0);
            let _ = reelforge_export::scene::frame_recipe(p, si, t, &sources);
            for track in seq.tracks.iter().take(32) {
                let _ = track.clip_at(t);
                for clip in track.clips.iter().take(64) {
                    let _ = clip.timeline_end();
                    let ct = clip.source_time(t);
                    let _ = (clip.media_time(ct), clip.rate_at(ct), clip.media_at(t), clip.clip_time_of_media(ct), clip.timeline_of_media(ct));
                    let _ = clip.transform_at(ct);
                    let _ = clip.effects_sized(ct, frame, size);
                    let _ = clip.title_at(ct);
                    let _ = clip.mesh_at(ct);
                    let _ = clip.display_name(p);
                }
            }
        }
    }
    let _ = serde_json::to_vec(p);
}

/// A file of `ext` holding `data`, for readers that only take paths (named for
/// this process, so parallel fuzzers don't trip over each other).
pub fn temp_file(ext: &str, data: &[u8]) -> std::path::PathBuf {
    let dir = if std::path::Path::new("/dev/shm").is_dir() { std::path::PathBuf::from("/dev/shm") } else { std::env::temp_dir() };
    let path = dir.join(format!("reelforge-fuzz-{}.{ext}", std::process::id()));
    let _ = std::fs::write(&path, data);
    path
}

/// What a media file goes through on import and in the Viewer: probing,
/// camera RAW detection, the first and a middle picture (with Reelforge's
/// own decoders where they apply), the sound, and its waveform. The first
/// byte picks the file's extension (MP4, MOV, MXF, WebM, MKV, R3D, BRAW, ARI).
pub fn run_media(data: &[u8]) {
    const EXTS: [&str; 8] = ["mp4", "mov", "mxf", "webm", "mkv", "r3d", "braw", "ari"];
    let Some((&pick, rest)) = data.split_first() else { return };
    let path = temp_file(EXTS[pick as usize % EXTS.len()], rest);
    let _ = reelforge_media::camraw::kind(&path);
    let _ = reelforge_media::camraw::needs_conversion(&path);
    if let Ok(info) = reelforge_media::probe::probe(&path) {
        if info.video.is_some() {
            if let Ok(mut src) = reelforge_media::source::FrameSource::open(&path, Some((256, 256))) {
                let wait = std::time::Duration::from_secs(3);
                let _ = src.frame_at_blocking(Time::ZERO, wait);
                let _ = src.frame_at_blocking(Time::from_secs_f64(info.duration.as_secs_f64() * 0.5), wait);
            }
        }
        if info.audio.is_some() {
            if let Ok(mut a) = reelforge_media::AudioReader::open(&path, 48000) {
                let mut buf = vec![0.0f32; 4096];
                a.read(0, &mut buf);
                a.read(48000, &mut buf);
            }
            let _ = reelforge_media::waveform_peaks(&path, 64);
        }
    }
    let _ = std::fs::remove_file(&path);
}

/// A Premiere Pro project (gzipped or plain XML), as File → Import reads it.
pub fn run_prproj(data: &[u8]) {
    if let Ok(im) = reelforge_core::adobe::prproj::read(data, None) {
        exercise(&im.project);
    }
}

/// An After Effects project, as File → Import reads it.
pub fn run_aep(data: &[u8]) {
    if let Ok(im) = reelforge_core::adobe::aep::read(data, None) {
        exercise(&im.project);
    }
}

/// A Reelforge project (JSON), as File → Open reads it.
pub fn run_project(data: &[u8]) {
    if let Ok(mut p) = serde_json::from_slice::<Project>(data) {
        p.resolve_paths(std::path::Path::new("/nowhere"));
        exercise(&p);
    }
}
