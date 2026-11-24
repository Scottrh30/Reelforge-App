//! Reelforge with a web interface: Tauri hosts the Svelte UI in `ui/` and
//! this file gives it the editor (`reelforge_engine::editor::Editor`)
//! through commands.
//!
//! The editor lives on the main thread (Tauri runs non-async commands
//! there), so no locking is needed: every command borrows it in turn. The UI
//! calls `tick` every animation frame, which moves playback along and renders
//! the viewer picture; it then fetches the full state when `rev` changed and
//! the picture when `picture_rev` changed.

// Never crash: no unwrap, expect, panic, unreachable, todo or unimplemented
// outside tests (a damaged file or an odd setting is an error, not a crash).
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

// (a few nested types read better spelled out)
#![allow(clippy::type_complexity)]

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(all(windows, target_env = "gnu"))]
mod webview2_shim;
mod diag;
mod export;
mod fx;
mod fx_presets;
mod start;
mod openfx;
mod keys;
mod ops;
mod multicam;
mod versions;
mod trans;
mod scene;
mod scopes;
mod viewer;
mod views;

use reelforge_core::undo::Edge;
use reelforge_core::{BlendMode, ClipAudio, ClipId, MediaId, Time, TitleSpec, TrackAudio, TrackKind, Transform, TrimMode};
use reelforge_engine::editor::{Editor, Prefs, MEDIA_EXTENSIONS, WAVEFORM_RATE};
use serde::Serialize;
use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use tauri::ipc::Response;

thread_local! {
    static EDITOR: RefCell<Option<Editor>> = const { RefCell::new(None) };
}

/// Where the settings file is (set at startup).
static SETTINGS_PATH: OnceLock<PathBuf> = OnceLock::new();

pub(crate) type Res<T> = Result<T, String>;

/// Run `f` on the editor.
pub(crate) fn ed<R>(f: impl FnOnce(&mut Editor) -> R) -> Res<R> {
    EDITOR.with(|cell| match cell.try_borrow_mut() {
        Ok(mut e) => e.as_mut().map(f).ok_or_else(|| "the editor isn't running on this thread".to_string()),
        Err(_) => Err("the editor is busy".into()),
    })
}

fn clip(id: u64) -> ClipId {
    ClipId(id)
}

// ---- State -----------------------------------------------------------------

#[derive(Serialize)]
struct Live {
    rev: u64,
    picture_rev: u64,
    /// Bumped when the Viewer's view changes.
    view_rev: u64,
    playhead: i64,
    playing: bool,
    /// Playback speed from J/K/L (1 = normal, negative = backwards).
    shuttle: f64,
    status: String,
    dirty: bool,
    clock: &'static str,
    exact: bool,
    from_cache: bool,
    master: MeterView,
    /// Peak dB (left, right) of each track of the sequence shown.
    tracks: Vec<[f32; 2]>,
    export: export::QueueView,
    waveforms: usize,
    /// Changes when the installed plugins change.
    plugins_rev: u64,
    /// Changes when any proxy's state does.
    proxies: u64,
    /// A Collect project in progress (or just finished).
    collect: Option<ops::CollectView>,
    /// A tracking run: the clip and how far along (0–1).
    tracking: Option<(u64, f32)>,
    /// An AI upscale being made: the media and how far along (0–1).
    upscaling: Option<(u64, f32)>,
    /// Match colour's progress (0–1), while it measures.
    matching: Option<f32>,
    /// A render-preview run: how far along (0–1).
    render: Option<f32>,
    /// The render-preview run going is Simulate now.
    simulating: bool,
    /// Rendering heavy parts in the background (while idle).
    background_render: bool,
    /// Bumped when the render cache gains or loses frames.
    render_gen: u64,
    /// What the Viewer is working from, for its performance readout.
    viewer: ViewerInfo,
    /// Disk space running short (until the warning is dismissed).
    disk: Option<reelforge_engine::disk::DiskLow>,
    /// The multicam clip at the playhead.
    multicam: Option<multicam::MulticamView>,
}

#[derive(Serialize)]
struct ViewerInfo {
    /// Video clips with footage showing at the playhead, and how many of them play from proxies.
    layers: usize,
    proxied: usize,
    proxies_on: bool,
    /// Hardware decoding: "on", "off", or why it isn't used.
    decode: String,
    /// The graphics card drawing the picture.
    gpu: Option<String>,
    /// The picture is drawn at 1/this of full resolution now.
    quality: u32,
}

/// What the Viewer works from at the playhead.
fn viewer_info(e: &reelforge_engine::editor::Editor) -> ViewerInfo {
    let t = e.playback.playhead;
    let s = e.seq();
    let paths = e.decode_paths();
    let mut layers = 0;
    let mut proxied = 0;
    for tr in s.tracks.iter().filter(|t| t.kind == TrackKind::Video && !t.muted) {
        for c in tr.clips.iter().filter(|c| c.contains(t)) {
            let Some(m) = e.project.media(c.media).filter(|m| m.info.video.is_some()) else { continue };
            layers += 1;
            if paths.get(&m.id).is_some_and(|p| *p != m.path) {
                proxied += 1;
            }
        }
    }
    let decode = if !e.prefs.hw_decode {
        "off".to_string()
    } else {
        let st = reelforge_media::hw::status();
        if st.starts_with("unavailable") { st } else { format!("on ({})", reelforge_media::hw::api_name()) }
    };
    ViewerInfo { layers, proxied, proxies_on: e.prefs.use_proxies, decode, gpu: reelforge_render::adapter_info(), quality: e.preview.active_quality() }
}

#[derive(Serialize)]
struct MeterView {
    peak: [f32; 2],
    rms: [f32; 2],
    hold: [f32; 2],
    clipped: [bool; 2],
}

/// Called every animation frame: moves playback along and renders the picture.
#[tauri::command]
fn tick() -> Res<Live> {
    ed(|e| {
        fx::poll(e);
        export::pump(e);
        e.tick();
        let collect = ops::view(e);
        let m = &e.meters;
        let n = e.seq().tracks.len();
        Live {
            rev: e.rev,
            picture_rev: e.preview.picture_rev,
            view_rev: e.view_rev,
            playhead: e.playback.playhead.0,
            playing: e.playback.playing,
            shuttle: e.playback.shuttle,
            status: e.status.clone(),
            dirty: e.is_dirty(),
            clock: e.playback.clock_name(),
            exact: e.preview.exact,
            from_cache: e.preview.from_cache,
            master: MeterView {
                peak: [m.ch[0].peak_db, m.ch[1].peak_db],
                rms: [m.ch[0].rms_db, m.ch[1].rms_db],
                hold: [m.ch[0].hold_db, m.ch[1].hold_db],
                clipped: [m.ch[0].clipped, m.ch[1].clipped],
            },
            tracks: m.tracks.iter().take(n).map(|t| [t[0].peak_db, t[1].peak_db]).collect(),
            export: export::view(),
            waveforms: e.waveforms.len(),
            plugins_rev: fx::PLUGINS_REV.load(std::sync::atomic::Ordering::Relaxed),
            proxies: e.proxies.signature(),
            collect,
            tracking: e.tracking.as_ref().map(|j| (j.clip.0, j.progress)),
            upscaling: e.upscale.as_ref().map(|j| (j.media.0, j.progress)),
            matching: e.color_match.as_ref().map(|j| j.progress),
            render: e.render_progress(),
            simulating: e.simulating(),
            background_render: e.background_rendering(),
            render_gen: e.render_cache.generation,
            viewer: viewer_info(e),
            disk: e.disk_low.clone().filter(|_| !e.disk_dismissed),
            multicam: multicam::view(e),
        }
    })
}

#[tauri::command]
fn get_state() -> Res<views::State> {
    ed(|e| views::state(e))
}

/// The viewer picture, if it changed since `have` (its `picture_rev`).
///
/// Shrunk by a whole factor to the smallest size still at least `max_w`×`max_h`.
/// Bytes: kind (0 = unchanged, 1 = RGBA, 2 = JPEG), then width and height
/// (u32, little-endian), then the picture.
#[tauri::command]
fn frame(max_w: u32, max_h: u32, have: i64, jpeg: bool) -> Res<Response> {
    ed(|e| {
        if e.preview.picture_rev as i64 == have {
            return Response::new(vec![0u8]);
        }
        let Some((rgba, [w, h])) = scopes::picture(e) else { return Response::new(vec![0u8]) };
        let k = (w / max_w.max(1)).min(h / max_h.max(1)).max(1);
        let (rgba, w, h) = if k > 1 { shrink(&rgba, w, h, k) } else { ((*rgba).clone(), w, h) };
        let mut out = Vec::with_capacity(9 + rgba.len());
        if jpeg {
            out.push(2);
            out.extend(w.to_le_bytes());
            out.extend(h.to_le_bytes());
            let enc = jpeg_encoder::Encoder::new(&mut out, 88);
            if enc.encode(&rgba, w as u16, h as u16, jpeg_encoder::ColorType::Rgba).is_err() {
                return Response::new(vec![0u8]);
            }
        } else {
            out.push(1);
            out.extend(w.to_le_bytes());
            out.extend(h.to_le_bytes());
            out.extend(rgba);
        }
        Response::new(out)
    })
}

/// Box-filter an RGBA picture down by `k` in each direction.
pub(crate) fn shrink(rgba: &[u8], w: u32, h: u32, k: u32) -> (Vec<u8>, u32, u32) {
    let (nw, nh) = (w / k, h / k);
    let mut out = Vec::with_capacity((nw * nh * 4) as usize);
    let n = k * k;
    for y in 0..nh {
        for x in 0..nw {
            let mut acc = [0u32; 4];
            for dy in 0..k {
                let row = ((y * k + dy) * w + x * k) as usize * 4;
                for px in rgba[row..row + (k * 4) as usize].chunks_exact(4) {
                    for c in 0..4 {
                        acc[c] += px[c] as u32;
                    }
                }
            }
            out.extend(acc.map(|a| ((a + n / 2) / n) as u8));
        }
    }
    (out, nw, nh)
}

/// A media item's sound as peaks, `rate` per second, 0–255 (empty until computed).
#[tauri::command]
fn waveform(media: u64) -> Res<Response> {
    ed(|e| {
        let bytes = match e.waveforms.get(&MediaId(media)) {
            Some(p) => p.iter().map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8).collect(),
            None => Vec::new(),
        };
        Response::new(bytes)
    })
}

/// A small JPEG of `path` at `at` (ticks), for the Media panel and timeline.
/// Runs off the main thread, so decoding doesn't hold up the interface.
#[tauri::command(async)]
fn thumbnail(path: String, at: i64) -> Res<Response> {
    static OPEN: Mutex<Vec<(PathBuf, reelforge_media::FrameSource)>> = Mutex::new(Vec::new());
    let path = PathBuf::from(path);
    let mut src = {
        let mut open = OPEN.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        match open.iter().position(|(p, _)| *p == path) {
            Some(i) => open.remove(i).1,
            None => reelforge_media::FrameSource::open(&path, Some((320, 180))).map_err(|e| format!("{e:#}"))?,
        }
    };
    let frame = src.frame_at_blocking(Time(at.max(0)), std::time::Duration::from_secs(5));
    {
        let mut open = OPEN.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if open.len() >= 4 {
            open.remove(0);
        }
        open.push((path, src));
    }
    let f = frame.ok_or("no picture there")?;
    let rgba = f.rgba();
    let mut out = Vec::new();
    jpeg_encoder::Encoder::new(&mut out, 80).encode(&rgba, f.width as u16, f.height as u16, jpeg_encoder::ColorType::Rgba).map_err(|e| e.to_string())?;
    Ok(Response::new(out))
}

/// A small JPEG of composition `seq` at its time `at` (ticks), for
/// composition clips' filmstrips on the timeline.
#[tauri::command]
fn comp_thumbnail(seq: usize, at: i64) -> Res<Response> {
    let jpg = ed(|e| e.comp_thumbnail(seq, Time(at.max(0)), 200))?.ok_or("no picture there")?;
    Ok(Response::new(jpg))
}

/// The font families installed on this computer (for Preferences > Fonts);
/// empty until the background font scan has finished.
#[tauri::command]
fn font_families() -> Res<Vec<String>> {
    ed(|e| {
        let mut v: Vec<String> = e.fonts().map(|f| f.iter().map(|f| f.family.clone()).collect()).unwrap_or_default();
        v.sort_by_key(|f| f.to_lowercase());
        v.dedup();
        v
    })
}

#[tauri::command]
fn waveform_rate() -> u32 {
    WAVEFORM_RATE
}

// ---- Files -----------------------------------------------------------------

#[tauri::command]
fn new_project() -> Res<()> {
    ed(|e| e.new_project())
}

#[tauri::command]
fn open_dialog() -> Res<()> {
    let Some(path) = rfd::FileDialog::new().add_filter("Reelforge project", &["rfproj"]).pick_file() else { return Ok(()) };
    ed(|e| e.open_project_path(path))
}

/// The renderers this computer has, for Preferences (GPU, CPU), and which draws now.
#[derive(Serialize)]
struct Renderers {
    available: Vec<reelforge_render::RendererInfo>,
    active: reelforge_render::RenderDevice,
    device: Option<String>,
}

#[tauri::command]
fn renderers() -> Res<Renderers> {
    let available = reelforge_render::renderers();
    ed(|e| Renderers { available, active: e.renderer, device: reelforge_render::adapter_info() })
}

/// File > Open a Premiere Pro, After Effects or HitFilm project…: it becomes a new, unsaved project.
#[tauri::command]
fn open_adobe_dialog() -> Res<()> {
    let Some(path) = rfd::FileDialog::new()
        .add_filter("Premiere Pro, After Effects or HitFilm project", &["prproj", "aep", "hfp", "hfcs"])
        .add_filter("Premiere Pro project", &["prproj"])
        .add_filter("After Effects project", &["aep"])
        .add_filter("HitFilm project or composite shot", &["hfp", "hfcs"])
        .pick_file()
    else {
        return Ok(());
    };
    ed(|e| e.import_adobe_project(&path))
}

/// Close the report of what an opened Adobe project brought across.
#[tauri::command]
fn dismiss_import_report() -> Res<()> {
    ed(|e| e.dismiss_import_report())
}

/// File > Open an autosave…: pick one from the autosave folder and open it unsaved.
#[tauri::command]
fn open_autosave_dialog() -> Res<()> {
    let dir = reelforge_engine::safety::autosave_dir();
    let Some(path) = rfd::FileDialog::new().set_directory(&dir).add_filter("Reelforge project", &["rfproj"]).pick_file() else { return Ok(()) };
    ed(|e| e.open_autosave(path))
}

/// Frame runs (first, end) the render preview has finished, checking every `step`th frame.
#[tauri::command]
fn rendered_runs(step: i64) -> Res<Vec<(i64, i64)>> {
    ed(|e| e.rendered_runs(step))
}

#[tauri::command]
fn open_path(path: String) -> Res<()> {
    ed(|e| e.open_project_path(PathBuf::from(path)))
}

/// Save (asking where if it's never been saved, or for Save As). Returns whether it was saved.
#[tauri::command]
fn save(save_as: bool) -> Res<bool> {
    let current = ed(|e| e.project_path.clone())?;
    let path = match (current, save_as) {
        (Some(p), false) => Some(p),
        _ => rfd::FileDialog::new().add_filter("Reelforge project", &["rfproj"]).set_file_name("project.rfproj").save_file(),
    };
    let Some(path) = path else { return Ok(false) };
    ed(|e| match e.save_to(&path) {
        Ok(()) => {
            start::save_thumbnail(e);
            true
        }
        Err(err) => {
            e.status = format!("Save failed: {err:#}");
            false
        }
    })
}

#[tauri::command]
fn import_dialog() -> Res<()> {
    let Some(paths) = rfd::FileDialog::new()
        .add_filter("Media and 3D models", &[&MEDIA_EXTENSIONS[..], &reelforge_core::MODEL_EXTENSIONS[..]].concat())
        .add_filter("3D models", &reelforge_core::MODEL_EXTENSIONS)
        .add_filter("All files", &["*"])
        .pick_files() else { return Ok(()) };
    ed(|e| e.import(&paths))
}

/// Import files (dropped on the window). Returns the ids of the media now in the project for them.
#[tauri::command]
fn import_paths(paths: Vec<String>) -> Res<Vec<u64>> {
    let paths: Vec<PathBuf> = paths.into_iter().map(PathBuf::from).collect();
    ed(|e| {
        let rf: Vec<&PathBuf> = paths.iter().filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("rfproj"))).collect();
        if let Some(p) = rf.first() {
            e.open_project_path((*p).clone());
            return Vec::new();
        }
        // A Premiere Pro, After Effects or HitFilm project dropped: opened as a new project.
        if let Some(p) = paths.iter().find(|p| reelforge_core::adobe::is_adobe_project(p)) {
            e.import_adobe_project(p);
            return Vec::new();
        }
        e.import(&paths);
        paths
            .iter()
            .filter_map(|p| {
                let abs = std::path::absolute(p).unwrap_or_else(|_| p.clone());
                e.project.media.iter().find(|m| m.path == abs).map(|m| m.id.0)
            })
            .collect()
    })
}

#[tauri::command]
fn recover(index: usize) -> Res<()> {
    ed(|e| e.recover(index))
}

#[tauri::command]
fn dismiss_recovery(index: usize) -> Res<()> {
    ed(|e| e.dismiss_recovery(index))
}

/// What the Settings dialog shows beside the choices: how the caches and
/// hardware decoding are doing, and where proxies and autosaves are kept.
#[derive(Serialize)]
struct SettingsInfo {
    hw_api: &'static str,
    hw_status: String,
    zero_copy_status: String,
    cache_frames: usize,
    cache_mb: usize,
    cache_hits: f32,
    render_frames: usize,
    render_mb: usize,
    /// Rendered frames kept on disk.
    render_disk_frames: usize,
    render_disk_mb: u64,
    proxies: usize,
    proxy_bytes: u64,
    proxy_dir: String,
    autosave_dir: String,
    /// Disk: free space where the app saves, and what could be cleared.
    free_bytes: u64,
    unused_proxies: usize,
    unused_proxy_bytes: u64,
    /// Object removal fills: all of them, and those nothing uses.
    fill_bytes: u64,
    unused_fills: usize,
    unused_fill_bytes: u64,
    autosaves: usize,
    autosave_bytes: u64,
    /// Where proxies and rendered previews go now.
    media_cache_dir: String,
    /// The codecs Reelforge decodes itself.
    native: Vec<&'static str>,
    /// What opens Blackmagic RAW, R3D and ARRIRAW here.
    camera_raw: Vec<String>,
}

#[tauri::command]
fn settings_info() -> Res<SettingsInfo> {
    ed(|e| {
        let (used, frames, hits) = reelforge_media::FrameCache::global().stats();
        let (rused, rframes) = e.render_cache.stats();
        let (dused, dframes) = e.render_cache.disk.as_ref().map_or((0, 0), |d| d.stats());
        let unused = reelforge_engine::disk::unused_proxies(&e.project);
        let fills = reelforge_engine::disk::fills(&e.project, e.project_path.as_deref());
        let (autosaves, autosave_bytes) = reelforge_engine::safety::autosave_usage();
        let dir = reelforge_media::proxy::media_cache_dir().join("proxies");
        let (proxy_bytes, proxies) = std::fs::read_dir(&dir)
            .map(|rd| rd.flatten().filter_map(|f| f.metadata().ok()).filter(|m| m.is_file()).fold((0, 0), |(b, n), m| (b + m.len(), n + 1)))
            .unwrap_or((0, 0));
        SettingsInfo {
            hw_api: reelforge_media::hw::api_name(),
            hw_status: reelforge_media::hw::status(),
            zero_copy_status: reelforge_core::frame::zero_copy::status(),
            cache_frames: frames,
            cache_mb: used >> 20,
            cache_hits: hits,
            render_frames: rframes,
            render_mb: rused >> 20,
            render_disk_frames: dframes,
            render_disk_mb: dused >> 20,
            proxies,
            proxy_bytes,
            proxy_dir: dir.display().to_string(),
            autosave_dir: reelforge_engine::safety::autosave_dir().display().to_string(),
            free_bytes: reelforge_engine::disk::free_bytes(&reelforge_media::proxy::media_cache_dir()).unwrap_or(0),
            unused_proxies: unused.len(),
            fill_bytes: fills.iter().map(|f| f.bytes).sum(),
            unused_fills: fills.iter().filter(|f| !f.used).count(),
            unused_fill_bytes: fills.iter().filter(|f| !f.used).map(|f| f.bytes).sum(),
            unused_proxy_bytes: unused.iter().map(|u| u.1).sum(),
            autosaves,
            autosave_bytes,
            media_cache_dir: reelforge_media::proxy::media_cache_dir().display().to_string(),
            native: reelforge_media::codecs::Codec::ALL.into_iter().filter(|c| c.available()).map(|c| c.label()).collect(),
            camera_raw: reelforge_media::camraw::status(),
        }
    })
}

/// Make disk space: "render" (rendered previews) or "proxies" (those of media not in this project).
#[tauri::command]
fn disk_free(what: String) -> Res<()> {
    ed(|e| e.free_disk_space(&what))
}

/// Where proxies and rendered previews go: `choose` asks for a folder,
/// otherwise back to the cache folder. With `move_files` what's there now is
/// moved across in the background. Returns the folder now chosen (`None`:
/// the cache folder), or the same as before if the choice was cancelled.
#[tauri::command]
fn cache_folder_set(choose: bool, move_files: bool) -> Res<Option<String>> {
    let new = if choose {
        match rfd::FileDialog::new().set_title("Keep proxies and rendered previews in").pick_folder() {
            Some(p) => Some(p),
            None => return ed(|e| e.prefs.cache_folder.as_ref().map(|p| p.display().to_string())),
        }
    } else {
        None
    };
    let (from, to) = ed(|e| {
        let from = reelforge_media::proxy::media_cache_dir();
        e.prefs.cache_folder = new.clone();
        e.apply_prefs();
        save_settings(e);
        let to = reelforge_media::proxy::media_cache_dir();
        e.status = format!("Proxies and rendered previews now go in {}", to.display());
        e.touch();
        (from, to)
    })?;
    if move_files && from != to {
        std::thread::spawn(move || {
            let (n, bytes, failed) = reelforge_engine::disk::move_media_cache(&from, &to);
            let _ = ed(|e| {
                e.reopen_render_disk();
                e.proxies.forget_ready();
                e.status = format!(
                    "Moved {n} file{} ({}) to {}{}",
                    if n == 1 { "" } else { "s" },
                    reelforge_engine::disk::size_text(bytes),
                    to.display(),
                    if failed > 0 { format!("; {failed} couldn't be moved (in use?) and stay where they were") } else { String::new() }
                );
                e.touch();
            });
        });
    }
    Ok(new.map(|p| p.display().to_string()))
}

/// Hide the low disk space warning (until space runs short again after recovering).
#[tauri::command]
fn disk_dismiss() -> Res<()> {
    ed(|e| e.disk_dismissed = true)
}

/// Forget every rendered frame, in memory and on disk.
#[tauri::command]
fn clear_render_cache() -> Res<()> {
    ed(|e| {
        e.render_cache.clear_all(true);
        e.status = "Rendered previews cleared".into();
        e.touch();
    })
}

#[tauri::command]
fn set_prefs(prefs: Prefs) -> Res<()> {
    ed(|e| {
        let depth = (prefs.color_depth != e.prefs.color_depth).then_some(prefs.color_depth);
        e.set_prefs(prefs);
        // Say what it renders at now (32-bit float only where the graphics card can).
        if let Some(d) = depth {
            e.set_color_depth(d);
        }
        e.touch();
        save_settings(e);
    })
}

/// Settings → Renders → Default Color Bit Depth.
#[tauri::command]
fn set_color_depth(depth: reelforge_render::ColorDepth) -> Res<()> {
    ed(|e| {
        e.set_color_depth(depth);
        e.touch();
        save_settings(e);
    })
}

/// The Viewer's playback quality: 1 full, 2 half, 4 quarter, 8 eighth resolution (0 automatic).
#[tauri::command]
fn set_playback_quality(q: u32) -> Res<()> {
    ed(|e| {
        e.set_playback_quality(q);
        e.touch();
        save_settings(e);
    })
}

/// Show the paused picture at full resolution whatever the playback quality.
#[tauri::command]
fn set_full_when_paused(on: bool) -> Res<()> {
    ed(|e| {
        e.set_full_when_paused(on);
        e.touch();
        save_settings(e);
    })
}

pub(crate) fn save_settings(e: &Editor) {
    if let Some(p) = SETTINGS_PATH.get() {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_vec_pretty(&e.prefs) {
            let _ = reelforge_engine::safety::write_atomic(p, &json);
        }
    }
}

// ---- Media and sequences ------------------------------------------------------

#[tauri::command]
fn add_to_timeline(media: u64, track: Option<usize>, at: Option<i64>) -> Res<()> {
    ed(|e| e.add_to_timeline(MediaId(media), track, at.map(Time)))
}

#[tauri::command]
fn new_composition_from_media(media: u64) -> Res<()> {
    ed(|e| e.new_composition_from_media(MediaId(media)))
}

#[tauri::command]
fn new_composite() -> Res<()> {
    ed(|e| e.new_composite())
}

#[tauri::command]
fn add_composite(index: usize) -> Res<()> {
    ed(|e| e.add_composite_to_timeline(index))
}

#[tauri::command]
fn open_sequence(index: usize) -> Res<()> {
    ed(|e| e.open_sequence(index))
}

#[tauri::command]
fn add_title() -> Res<()> {
    ed(|e| e.add_title())
}

#[tauri::command]
fn add_3d_text() -> Res<()> {
    ed(|e| e.add_3d_text())
}

/// Pick a 3D model file and add it as a 3D layer at the playhead.
#[tauri::command]
fn add_3d_model() -> Res<()> {
    let Some(path) = rfd::FileDialog::new().add_filter("3D models", &reelforge_core::MODEL_EXTENSIONS).pick_file() else { return Ok(()) };
    ed(|e| e.add_3d_model(&path))
}

/// Pick an SVG logo and add it as a 3D shape.
#[tauri::command]
fn add_3d_logo() -> Res<()> {
    let Some(path) = rfd::FileDialog::new().add_filter("SVG logos", &["svg", "svgz"]).pick_file() else { return Ok(()) };
    ed(|e| e.import_svg_shape(&path))
}

/// Mask `index` of clip `id` as a new 3D shape.
#[tauri::command]
fn shape_from_mask(id: u64, index: usize) -> Res<()> {
    ed(|e| e.shape_from_mask(clip(id), index))
}

/// An energy blade on its own transparent layer, over the selected clip.
#[tauri::command]
fn add_blade_layer() -> Res<()> {
    ed(|e| e.add_blade_layer())
}

/// A particle emitter on its own transparent layer, over the selected clip.
#[tauri::command]
fn add_particles_layer() -> Res<()> {
    ed(|e| e.add_particles_layer())
}

/// Add a ready-made particles & simulation layer: fire, smoke, explosion,
/// rain, snow, fog, muzzle, bullet, gunshot, fluid; or shatter / atomic on
/// the selected layer.
#[tauri::command]
fn add_simulation(what: String) -> Res<()> {
    ed(|e| e.add_simulation(&what))
}

// ---- Selection and edits --------------------------------------------------------

/// `mode`: "only" (plain click), "toggle" (Ctrl+click) or "none" (click empty space).
#[tauri::command]
fn select(id: Option<u64>, mode: String) -> Res<()> {
    ed(|e| {
        e.commit_live_edit();
        match (mode.as_str(), id) {
            ("toggle", Some(id)) => e.toggle_selected(clip(id)),
            (_, id) => e.select_only(id.map(clip)),
        }
    })
}

#[tauri::command]
fn select_many(ids: Vec<u64>, add: bool) -> Res<()> {
    ed(|e| {
        let mut all: Vec<ClipId> = if add { e.selected_ids() } else { Vec::new() };
        for id in ids.into_iter().map(clip) {
            if !all.contains(&id) {
                all.push(id);
            }
        }
        e.select_many(all);
    })
}

#[tauri::command]
fn select_all() -> Res<()> {
    ed(|e| e.select_all())
}

#[tauri::command]
fn move_clips(lead: u64, track: usize, start: i64) -> Res<()> {
    ed(|e| e.move_clips(clip(lead), track, Time(start)))
}

/// A live trim (or slip/slide) of `id` by `delta` ticks from where the drag
/// started; `commit` ends it. Returns the delta actually applied.
#[tauri::command]
fn trim(id: u64, edge: String, mode: String, delta: i64) -> Res<Option<i64>> {
    let edge = if edge == "end" { Edge::End } else { Edge::Start };
    let mode = match mode.as_str() {
        "ripple" => TrimMode::Ripple,
        "roll" => TrimMode::Roll,
        "slip" => TrimMode::Slip,
        "slide" => TrimMode::Slide,
        _ => TrimMode::Normal,
    };
    ed(|e| e.trim_live(clip(id), edge, mode, Time(delta)).map(|p| p.applied.0))
}

/// End a gesture (a slider drag, a trim): it becomes one undo step.
#[tauri::command]
fn commit() -> Res<()> {
    ed(|e| e.commit_live_edit())
}

/// Simple editing actions by name (menus and shortcuts).
#[tauri::command]
fn action(name: String) -> Res<()> {
    ed(|e| match name.as_str() {
        "undo" => e.undo(),
        "redo" => e.redo(),
        "split" => e.split_at_playhead(),
        "delete" => {
            e.delete_selection();
        }
        "ripple_delete" => e.ripple_delete_selected(),
        "copy" => {
            e.copy_selection();
        }
        "cut" => e.cut_selection(),
        "paste" => e.paste(),
        "unlink" => e.unlink_selection(),
        "mark_in" => e.mark_in(),
        "mark_out" => e.mark_out(),
        "clear_marks" => e.clear_marks(),
        "select_all" => e.select_all(),
        "deselect" => e.select_only(None),
        "add_video_track" => e.add_track(TrackKind::Video),
        "add_audio_track" => e.add_track(TrackKind::Audio),
        "play" => e.toggle_play(),
        "stop" => e.playback.pause(),
        "start" => e.seek(Time::ZERO),
        "end" => {
            let end = e.seq().duration();
            e.seek(end);
        }
        "prev_frame" => {
            let rate = e.seq().frame_rate;
            e.playback.step(-1, rate);
        }
        "next_frame" => {
            let rate = e.seq().frame_rate;
            e.playback.step(1, rate);
        }
        "prev_edit" => e.jump(false),
        "next_edit" => e.jump(true),
        "prev_marker" => e.jump_marker(false),
        "next_marker" => e.jump_marker(true),
        "add_marker" => e.add_marker(),
        "shuttle_back" => {
            let end = e.seq().duration();
            e.playback.shuttle(-1.0, end);
        }
        "shuttle_stop" => {
            e.playback.pause();
            e.playback.shuttle = 1.0;
        }
        "shuttle_forward" => {
            let end = e.seq().duration();
            e.playback.shuttle(1.0, end);
        }
        "freeze_frame" => e.freeze_frame_at_playhead(),
        "dissolve" => e.toggle_transition_at_playhead(TrackKind::Video),
        "crossfade" => e.toggle_transition_at_playhead(TrackKind::Audio),
        "nudge_left" => e.nudge(-1),
        "nudge_right" => e.nudge(1),
        "nudge_left_10" => e.nudge(-10),
        "nudge_right_10" => e.nudge(10),
        "render_preview" => e.render_preview(),
        "simulate_now" => e.simulate_now(),
        "stop_render_preview" => e.stop_render_preview(),
        "make_composite" => e.make_composite(),
        other => e.status = format!("Unknown action {other}"),
    })
}

#[tauri::command]
fn history_go_to(pos: usize) -> Res<()> {
    ed(|e| e.history_go_to(pos))
}

#[tauri::command]
fn seek(t: i64) -> Res<()> {
    ed(|e| e.seek(Time(t)))
}

#[tauri::command]
fn track_op(index: usize, op: String, name: Option<String>, to: Option<usize>) -> Res<()> {
    ed(|e| match op.as_str() {
        "move" => e.move_track(index, to.unwrap_or(index)),
        "mute" => e.toggle_mute(index),
        "lock" => e.toggle_lock(index),
        "delete" => e.delete_track(index),
        "duplicate" => e.duplicate_track(index),
        "target" => e.set_target_track(Some(index)),
        "untarget" => e.set_target_track(None),
        "rename" => e.rename_track(index, name.unwrap_or_default()),
        "collapse" => e.toggle_collapse(index),
        "cycle" => e.cycle_track_view(index),
        "properties_on" => e.set_track_properties(index, true),
        "properties_off" => e.set_track_properties(index, false),
        "waveforms_on" => e.set_track_waveforms(index, Some(true)),
        "waveforms_off" => e.set_track_waveforms(index, Some(false)),
        "waveforms_default" => e.set_track_waveforms(index, None),
        "height" => e.set_track_height(index, to.unwrap_or(52).min(u16::MAX as usize) as u16),
        // Of the track's kind ("…_all") or every track ("…_every").
        "collapse_all" | "expand_all" | "reset_heights" => {
            let kind = e.seq().tracks.get(index).map(|t| t.kind);
            e.set_all_tracks(kind, op == "collapse_all", op == "reset_heights")
        }
        "collapse_every" | "expand_every" => e.set_all_tracks(None, op == "collapse_every", false),
        _ => {}
    })
}

#[tauri::command]
fn set_track_audio(index: usize, audio: TrackAudio) -> Res<()> {
    ed(|e| e.set_track_audio_live(index, audio))
}

// ---- Controls panel ------------------------------------------------------------

#[tauri::command]
fn set_transform(id: u64, transform: Transform) -> Res<()> {
    // As seen at the playhead: keyframed properties get a key there.
    ed(|e| e.set_transform_live(clip(id), transform))
}

#[tauri::command]
fn set_blend(id: u64, blend: BlendMode) -> Res<()> {
    ed(|e| {
        if let Some(mut v) = e.visual(clip(id)) {
            v.blend = blend;
            e.set_visual_live(clip(id), v);
            e.commit_live_edit();
        }
    })
}

/// Show clip `id` only through the layer on the video track above (or not, with `None`).
#[tauri::command]
fn set_track_matte(id: u64, matte: Option<reelforge_core::TrackMatte>) -> Res<()> {
    ed(|e| {
        if let Some(mut v) = e.visual(clip(id)) {
            v.track_matte = matte;
            e.set_visual_live(clip(id), v);
            e.commit_live_edit();
            e.status = match matte {
                None => "Track matte off".into(),
                Some(_) => "Track matte on: this layer shows through the layer on the video track above, which isn't drawn itself".into(),
            };
        }
    })
}

/// Make `what` of clip `id` a template control of the sequence being edited.
#[tauri::command]
fn expose_control(id: u64, what: reelforge_core::template::Target, label: Option<String>) -> Res<()> {
    ed(|e| e.expose_control(clip(id), what, label))
}

/// Rename (with a label) or remove (without) control `index` of the sequence being edited.
#[tauri::command]
fn edit_control(index: usize, label: Option<String>) -> Res<()> {
    ed(|e| e.edit_control(index, label))
}

/// Set control `index` of the composition clip `id` plays.
#[tauri::command]
fn set_control(id: u64, index: usize, value: reelforge_core::template::Value) -> Res<()> {
    ed(|e| e.set_control(clip(id), index, value))
}

/// Save the sequence being edited as a template (asks where).
#[tauri::command]
fn save_template() -> Res<()> {
    let name = ed(|e| e.seq().name.clone())?;
    let Some(path) = rfd::FileDialog::new().set_title("Save as a template").add_filter("Reelforge template", &[reelforge_core::template::EXTENSION]).set_file_name(format!("{name}.{}", reelforge_core::template::EXTENSION)).save_file() else { return Ok(()) };
    ed(|e| e.save_template(&path))
}

/// Add a template (asks which) at the playhead.
#[tauri::command]
fn add_template() -> Res<()> {
    let Some(path) = rfd::FileDialog::new().set_title("Add a template").add_filter("Reelforge template", &[reelforge_core::template::EXTENSION]).pick_file() else { return Ok(()) };
    ed(|e| e.add_template(&path))
}

/// Every installed font face for titles (empty until the background scan finishes).
#[tauri::command]
fn font_faces() -> Res<Vec<reelforge_core::FontChoice>> {
    ed(|e| {
        let mut v: Vec<reelforge_core::FontChoice> = e.fonts().map(|f| f.iter().map(|f| f.choice()).collect()).unwrap_or_default();
        v.sort_by_key(|f| f.name.to_lowercase());
        v.dedup_by(|a, b| a.name == b.name);
        v
    })
}

#[tauri::command]
fn place_title(id: u64, place: String) -> Res<()> {
    ed(|e| e.place_title(clip(id), &place))
}

#[tauri::command]
fn set_title(id: u64, spec: TitleSpec) -> Res<()> {
    ed(|e| e.set_title_live(clip(id), spec))
}

#[tauri::command]
fn set_clip_audio(id: u64, audio: ClipAudio) -> Res<()> {
    ed(|e| e.set_clip_audio_live(clip(id), audio))
}

/// The audio effects there are, with their parameters (for the Audio panel).
#[derive(Serialize)]
struct AudioFxInfo {
    kind: reelforge_core::audiofx::AudioFxKind,
    label: &'static str,
    params: Vec<AudioParamInfo>,
}
#[derive(Serialize)]
struct AudioParamInfo {
    name: &'static str,
    default: f32,
    min: f32,
    max: f32,
    suffix: &'static str,
    speed: f32,
}

#[tauri::command]
fn audio_fx_kinds() -> Vec<AudioFxInfo> {
    reelforge_core::audiofx::AudioFxKind::ALL
        .iter()
        .map(|&k| AudioFxInfo { kind: k, label: k.label(), params: k.specs().iter().map(|s| AudioParamInfo { name: s.name, default: s.default, min: s.min, max: s.max, suffix: s.suffix, speed: s.speed }).collect() })
        .collect()
}

#[tauri::command]
fn set_speed(id: u64, speed: f32, reverse: bool) -> Res<()> {
    ed(|e| e.set_speed(clip(id), speed, reverse))
}

/// Time remapping on or off.
#[tauri::command]
fn time_remap(id: u64, on: bool) -> Res<()> {
    ed(|e| e.time_remap(clip(id), on))
}

/// Show media time `secs` at the playhead.
#[tauri::command]
fn time_remap_value(id: u64, secs: f32) -> Res<()> {
    ed(|e| e.time_remap_value(clip(id), secs))
}

/// Add or remove a time remap key at the playhead.
#[tauri::command]
fn time_remap_key(id: u64) -> Res<()> {
    ed(|e| e.time_remap_key(clip(id)))
}

#[tauri::command]
fn time_remap_move_key(id: u64, index: usize, secs: f32) -> Res<()> {
    ed(|e| e.time_remap_move_key(clip(id), index, secs))
}

/// "Linear", "Ease" or "Hold" between all the keys.
#[tauri::command]
fn time_remap_interp(id: u64, interp: String) -> Res<()> {
    use reelforge_core::anim::Interp;
    let i = match interp.as_str() {
        "Ease" => Interp::Ease,
        "Hold" => Interp::Hold,
        _ => Interp::Linear,
    };
    ed(|e| e.time_remap_interp(clip(id), i))
}

/// "Nearest", "Blend" or "Flow".
#[tauri::command]
fn set_frame_blend(id: u64, blend: String) -> Res<()> {
    use reelforge_core::FrameBlend;
    let b = match blend.as_str() {
        "Blend" => FrameBlend::Blend,
        "Flow" => FrameBlend::Flow,
        _ => FrameBlend::Nearest,
    };
    ed(|e| e.set_frame_blend(clip(id), b))
}

/// How carefully optical flow works: "Fast", "Balanced" or "Best".
#[tauri::command]
fn set_flow_quality(id: u64, quality: String) -> Res<()> {
    use reelforge_core::FlowQuality;
    let q = match quality.as_str() {
        "Fast" => FlowQuality::Fast,
        "Best" => FlowQuality::Best,
        _ => FlowQuality::Balanced,
    };
    ed(|e| e.set_flow_quality(clip(id), q))
}

// ---- Masks ---------------------------------------------------------------------

/// Add a rectangle or ellipse mask in the middle of the clip. Returns its index.
#[tauri::command]
fn add_mask(id: u64, shape: String) -> Res<Option<usize>> {
    use reelforge_core::mask::{Mask, MaskShape};
    let shape = if shape == "ellipse" { MaskShape::Ellipse } else { MaskShape::Rectangle };
    ed(|e| {
        let size = e.seq().find_clip(clip(id)).map(|(_, c)| e.layer_size(c))?;
        e.add_mask(clip(id), Mask::centred(shape, size))
    })
}

/// Add a freehand mask through `points` (layer pixels). Returns its index.
#[tauri::command]
fn add_freehand_mask(id: u64, points: Vec<[f32; 2]>) -> Res<Option<usize>> {
    ed(|e| {
        if points.len() < 3 {
            e.status = "A freehand mask needs at least 3 points".into();
            return None;
        }
        e.add_mask(clip(id), reelforge_core::mask::Mask::freehand(points))
    })
}

/// Replace the clip's masks (the Masks panel's settings). `done` ends the gesture.
#[tauri::command]
fn set_masks(id: u64, masks: Vec<reelforge_core::mask::Mask>, done: bool) -> Res<()> {
    ed(|e| {
        e.set_masks_live(clip(id), masks);
        if done {
            e.commit_live_edit();
        }
    })
}

/// Move a mask's points (dragging in the Viewer); `commit` ends the drag.
#[tauri::command]
fn set_mask_points(id: u64, index: usize, points: Vec<[f32; 2]>) -> Res<()> {
    ed(|e| e.set_mask_points_live(clip(id), index, points))
}

/// Lower the music tracks under speech on the dialogue tracks (times in seconds). One undo step.
#[tauri::command]
fn auto_duck(dialogue: Vec<usize>, music: Vec<usize>, amount_db: f32, threshold_db: f32, fade_down: f64, fade_up: f64, hold: f64) -> Res<()> {
    let secs = |s: f64| Time::from_secs_f64(s.clamp(0.0, 30.0));
    let settings = reelforge_core::duck::DuckSettings {
        amount_db: amount_db.clamp(-60.0, 0.0),
        threshold_db: threshold_db.clamp(-90.0, 0.0),
        fade_down: secs(fade_down),
        fade_up: secs(fade_up),
        hold: secs(hold),
        ..Default::default()
    };
    ed(|e| e.auto_duck(&dialogue, &music, settings))
}

/// Take automatic ducking off the clips on these tracks. One undo step.
#[tauri::command]
fn remove_ducking(music: Vec<usize>) -> Res<()> {
    ed(|e| e.remove_ducking(&music))
}

/// Set a mask's Bézier handles (dragging them in the Viewer); `commit` ends the drag.
#[tauri::command]
fn set_mask_curves(id: u64, index: usize, curves: Vec<[f32; 4]>) -> Res<()> {
    ed(|e| e.set_mask_curves_live(clip(id), index, curves))
}

/// Add a point to a mask halfway along the side after (or before) `point`: the new point's index.
#[tauri::command]
fn mask_point_add(id: u64, index: usize, point: usize, before: bool) -> Res<Option<usize>> {
    ed(|e| e.add_mask_point(clip(id), index, point, before))
}

/// Take the jitter out of a tracked mask's keyframes, `frames` wide.
#[tauri::command]
fn mask_smooth(id: u64, index: usize, frames: f32) -> Res<()> {
    ed(|e| e.smooth_mask(clip(id), index, frames))
}

/// Trace what key effect `index` keeps into a new mask. Returns its index.
#[tauri::command]
fn mask_from_key(id: u64, index: usize) -> Res<Option<usize>> {
    ed(|e| e.mask_from_key(clip(id), index))
}

/// Paint a roto brush stroke (layer pixels) on clip `id` at the playhead, into
/// mask `mask` (a new one if none). Returns the mask's index.
#[tauri::command]
fn roto_stroke(id: u64, mask: Option<usize>, fg: bool, radius: f32, points: Vec<[f32; 2]>, removal: Option<bool>) -> Res<Option<usize>> {
    ed(|e| e.roto_stroke(clip(id), mask, reelforge_core::rotobrush::Stroke { fg, radius, points }, removal.unwrap_or(false)))
}

/// One click picks out the object at `point` (layer pixels) on clip `id` and carries it through the clip.
#[tauri::command]
fn roto_pick(id: u64, point: [f32; 2], removal: bool) -> Res<Option<usize>> {
    ed(|e| e.roto_pick(clip(id), point, removal))
}

/// One click: clip `id`'s background cut away through the whole clip (the subject found by itself).
#[tauri::command]
fn remove_background(id: u64) -> Res<Option<usize>> {
    ed(|e| e.remove_background(clip(id)))
}

/// What goes behind clip `id` once its background is cut away (a colour, a blur of the shot, or something from the project), on the track below.
#[tauri::command]
fn replace_background(id: u64, behind: reelforge_engine::editor::roto::Behind) -> Res<Option<u64>> {
    ed(|e| e.replace_background(clip(id), behind).map(|c| c.0))
}

/// Make an AI-upscaled copy (2× or 4×) of media `media`; `replace` puts it in the original's place.
#[tauri::command]
fn upscale_media(media: u64, factor: u32, replace: bool) -> Res<()> {
    ed(|e| e.start_upscale(MediaId(media), factor, replace))
}

/// Run NVIDIA's Video Effect `effect` (RTX cards) over media `media`; `replace` puts the
/// result in the original's place.
#[tauri::command]
fn nvidia_media(media: u64, effect: reelforge_media::nvfx::NvEffect, replace: bool) -> Res<()> {
    ed(|e| e.start_nvidia(MediaId(media), effect, replace))
}

/// Which of NVIDIA's AI effects are here (the Media panel offers them): the Video Effects
/// (installed by NVIDIA's redistributable) and RTX Video (shipped beside Reelforge).
#[derive(serde::Serialize)]
struct NvidiaHere {
    fx: bool,
    rtx: bool,
    /// The optical flow engine: whether it opened, and what became of it.
    flow: bool,
    flow_status: String,
}

#[tauri::command]
fn nvidia_available() -> NvidiaHere {
    NvidiaHere { fx: reelforge_media::nvfx::available(), rtx: reelforge_media::rtxvideo::available(), flow: reelforge_engine::hwflow::ready(), flow_status: reelforge_engine::hwflow::status() }
}

#[tauri::command]
fn upscale_cancel() -> Res<()> {
    ed(|e| e.cancel_upscale())
}

/// Match colour: give clips `targets` the look of a reference — `from`
/// "clip" (clip `id` on the timeline, as graded), "media" (Media panel item
/// `id`) or "file" (`path`) — matching brightness and contrast (`tone`)
/// and/or colour.
#[tauri::command]
fn color_match(targets: Vec<u64>, from: String, id: u64, path: Option<String>, tone: bool, color: bool) -> Res<()> {
    use reelforge_engine::editor::colormatch::MatchSource;
    let source = match from.as_str() {
        "clip" => MatchSource::Clip(ClipId(id)),
        "media" => MatchSource::Media(MediaId(id)),
        _ => MatchSource::File(path.map(std::path::PathBuf::from).ok_or("no file chosen")?),
    };
    let targets: Vec<ClipId> = targets.into_iter().map(ClipId).collect();
    ed(|e| e.start_color_match(&targets, source, reelforge_core::colormatch::MatchWhat { tone, color }))
}

#[tauri::command]
fn color_match_cancel() -> Res<()> {
    ed(|e| e.cancel_color_match())
}

/// A picture or video to take a look from.
#[tauri::command]
fn color_match_pick() -> Option<String> {
    rfd::FileDialog::new()
        .set_title("Match colour to… (a picture or a video)")
        .add_filter("Pictures and video", &["jpg", "jpeg", "png", "tif", "tiff", "exr", "bmp", "webp", "mp4", "mov", "mkv", "avi", "webm", "mxf", "m4v"])
        .pick_file()
        .map(|p| p.to_string_lossy().into_owned())
}

/// Get the frame at the playhead of clip `id` ready for the roto brush, in the background.
#[tauri::command]
fn roto_prepare(id: u64) -> Res<()> {
    ed(|e| e.roto_prepare(clip(id)))
}

/// Carry roto mask `mask` on from the playhead, forwards or backwards.
#[tauri::command]
fn roto_propagate(id: u64, mask: usize, forward: bool) -> Res<()> {
    ed(|e| e.start_roto_propagation(clip(id), mask, forward))
}

/// Add a mask point on its outline where it passes nearest `at` (layer pixels).
#[tauri::command]
fn mask_point_add_at(id: u64, index: usize, at: [f32; 2]) -> Res<Option<usize>> {
    ed(|e| e.add_mask_point_near(clip(id), index, at))
}

/// Delete a mask point (a mask keeps at least three).
#[tauri::command]
fn mask_point_delete(id: u64, index: usize, point: usize) -> Res<bool> {
    ed(|e| e.delete_mask_point(clip(id), index, point))
}

/// Turn a mask point into a smooth curve, or back into a corner.
#[tauri::command]
fn mask_point_curve(id: u64, index: usize, point: usize) -> Res<()> {
    ed(|e| e.toggle_mask_point_curve(clip(id), index, point))
}

/// Keyframing a mask's shape at the playhead: "animate" (on), "still" (off),
/// "key" (add a key here) or "unkey" (remove the key here). One undo step.
#[tauri::command]
fn mask_op(id: u64, index: usize, op: String) -> Res<()> {
    ed(|e| {
        let Some((_, c)) = e.seq().find_clip(clip(id)) else { return };
        let t = e.mask_time(c);
        let mut masks = c.masks.clone();
        let Some(m) = masks.get_mut(index) else { return };
        match op.as_str() {
            "animate" => m.set_animated(true, t),
            "still" => m.set_animated(false, t),
            "key" => {
                let pts = m.points_at(t);
                m.set_points_at(t, pts);
            }
            "unkey" if m.keys.len() > 1 => m.keys.retain(|k| k.at != t),
            _ => return,
        }
        e.set_masks_live(clip(id), masks);
        e.commit_live_edit();
    })
}

// ---- Motion tracking ------------------------------------------------------------

#[tauri::command]
fn tracker_add(id: u64, mode: reelforge_core::track::TrackMode) -> Res<()> {
    ed(|e| e.add_tracker(clip(id), mode))
}

/// Change how many points the tracker has.
#[tauri::command]
fn tracker_mode(id: u64, mode: reelforge_core::track::TrackMode) -> Res<()> {
    ed(|e| {
        let Some((_, c)) = e.seq().find_clip(clip(id)) else { return };
        let (Some(mut t), layer, src) = (c.tracker.clone(), e.layer_size(c), e.track_time(c)) else { return };
        t.set_mode(mode, layer, src);
        e.set_tracker(clip(id), Some(t), "Change tracker points");
    })
}

/// The feature box and search sizes (live: a drag on the numbers).
#[tauri::command]
fn tracker_sizes(id: u64, size: f32, search: f32) -> Res<()> {
    ed(|e| {
        let Some(mut t) = e.seq().find_clip(clip(id)).and_then(|(_, c)| c.tracker.clone()) else { return };
        t.set_sizes(size.clamp(9.0, 201.0), search.clamp(2.0, 300.0));
        e.set_tracker_live(clip(id), Some(t));
    })
}

/// How much the tracked paths are smoothed, in frames (live: a drag on the number).
#[tauri::command]
fn tracker_smooth(id: u64, frames: f32) -> Res<()> {
    ed(|e| {
        let Some(mut t) = e.seq().find_clip(clip(id)).and_then(|(_, c)| c.tracker.clone()) else { return };
        t.smooth = frames.clamp(0.0, reelforge_core::track::MAX_SMOOTH);
        e.set_tracker_live(clip(id), Some(t));
    })
}

/// The most frames a correction is eased into on each side.
const MAX_BLEND: usize = 120;

/// Move tracker point `point` to `pos` (layer pixels) at the playhead: on a
/// tracked frame this corrects the track (eased into `blend` tracked frames
/// on each side); elsewhere it's where tracking starts.
#[tauri::command]
fn tracker_move(id: u64, point: usize, pos: [f32; 2], blend: Option<usize>) -> Res<()> {
    ed(|e| {
        if e.tracking.is_some() {
            return;
        }
        let Some((_, c)) = e.seq().find_clip(clip(id)) else { return };
        let (Some(mut t), [w, h], src) = (c.tracker.clone(), e.layer_size(c), e.track_time(c)) else { return };
        use reelforge_core::track::TrackMode;
        // A planar surface's corners may lie outside the picture (only the area inside is tracked).
        let (lo, hi) = if matches!(t.mode, TrackMode::Planar | TrackMode::Mesh) { ([-w, -h], [2.0 * w, 2.0 * h]) } else { ([0.0, 0.0], [w, h]) };
        let pos = [pos[0].clamp(lo[0], hi[0]), pos[1].clamp(lo[1], hi[1])];
        let corners = t.mesh_corners();
        if t.mode == TrackMode::Mesh && !t.is_tracked() && corners.contains(&point) {
            // Before tracking, the grid follows its four corners (keeping its shape).
            t.move_mesh_corner(point, pos, src);
        } else {
            let Some(p) = t.points.get_mut(point) else { return };
            p.move_blended(src, pos, blend.unwrap_or(0).min(MAX_BLEND));
        }
        e.set_tracker_live(clip(id), Some(t));
    })
}

/// Move several tracker points at once (a group of mesh points, with the
/// ones around them following softly): `moves` are (point, position in
/// layer pixels) at the playhead, as for `tracker_move`, but a corner moves
/// on its own here (the rest of the grid isn't carried along).
#[tauri::command]
fn tracker_move_many(id: u64, moves: Vec<(usize, [f32; 2])>, blend: Option<usize>) -> Res<()> {
    ed(|e| {
        if e.tracking.is_some() {
            return;
        }
        let Some((_, c)) = e.seq().find_clip(clip(id)) else { return };
        let (Some(mut t), [w, h], src) = (c.tracker.clone(), e.layer_size(c), e.track_time(c)) else { return };
        for (i, pos) in moves {
            let pos = [pos[0].clamp(-w, 2.0 * w), pos[1].clamp(-h, 2.0 * h)];
            if let Some(p) = t.points.get_mut(i) {
                p.move_blended(src, pos, blend.unwrap_or(0).min(MAX_BLEND));
            }
        }
        e.set_tracker_live(clip(id), Some(t));
    })
}

/// A mesh tracker's grid: cells across and down (its corners stay; tracked paths are cleared).
#[tauri::command]
fn tracker_grid(id: u64, cols: u8, rows: u8) -> Res<()> {
    ed(|e| {
        let Some((_, c)) = e.seq().find_clip(clip(id)) else { return };
        let (Some(mut t), src) = (c.tracker.clone(), e.track_time(c)) else { return };
        t.set_grid(cols, rows, src);
        e.set_tracker(clip(id), Some(t), "Change mesh grid");
    })
}

/// Take a clip's mesh warp off.
#[tauri::command]
fn mesh_remove(id: u64) -> Res<()> {
    ed(|e| e.remove_mesh_warp(clip(id)))
}

/// "clear" (forget the tracked paths) or "remove" the tracker.
#[tauri::command]
fn tracker_op(id: u64, op: String) -> Res<()> {
    ed(|e| {
        let Some(t) = e.seq().find_clip(clip(id)).map(|(_, c)| c.tracker.clone()) else { return };
        match op.as_str() {
            "clear" => {
                let mut t = t;
                if let Some(t) = &mut t {
                    t.clear_paths();
                }
                e.set_tracker(clip(id), t, "Clear track");
            }
            "remove" => e.set_tracker(clip(id), None, "Remove tracker"),
            _ => {}
        }
    })
}

#[tauri::command]
fn track_start(id: u64, forward: bool) -> Res<()> {
    ed(|e| e.start_tracking(clip(id), forward))
}

/// Rotoscoping: track what's inside mask `index` and key its shape.
#[tauri::command]
fn mask_track(id: u64, index: usize, forward: bool, motion: reelforge_core::track::MaskMotion) -> Res<()> {
    ed(|e| e.start_mask_tracking(clip(id), index, forward, motion))
}

/// The 3D camera tracker: analyse the clip's whole shot (lens `fov` degrees if known).
#[tauri::command]
fn camtrack_start(id: u64, fov: Option<f32>, tripod: bool) -> Res<()> {
    ed(|e| e.start_camera_tracking(clip(id), fov, tripod))
}

/// Make a camera (and a point or 3D layer on the `picked` points) from the solve.
#[tauri::command]
fn camtrack_make(id: u64, what: reelforge_engine::editor::CamMake, picked: Vec<usize>) -> Res<()> {
    ed(|e| e.create_from_camera_track(clip(id), what, &picked))
}

/// The `picked` points' surface becomes the ground.
#[tauri::command]
fn camtrack_ground(id: u64, picked: Vec<usize>) -> Res<()> {
    ed(|e| e.set_camera_ground(clip(id), &picked))
}

/// Export clip `id`'s solved camera (with its `picked` points, or all) for another application.
#[tauri::command]
fn camtrack_export(id: u64, format: reelforge_core::camtrack_io::CamFormat, picked: Vec<usize>) -> Res<()> {
    let name = ed(|e| e.seq().find_clip(clip(id)).map(|(_, c)| c.display_name(&e.project).into_owned()))?.unwrap_or_else(|| "camera".into());
    let stem: String = name.rsplit_once('.').map_or(name.as_str(), |(a, _)| a).chars().map(|c| if r#"<>:"/\|?*"#.contains(c) { '_' } else { c }).collect();
    let ext = format.extension();
    let file = match format {
        reelforge_core::camtrack_io::CamFormat::Json => format!("{stem} camera.json"),
        _ => format!("{stem} camera.{ext}"),
    };
    let Some(path) = rfd::FileDialog::new().add_filter(format.label(), &[ext]).set_file_name(&file).save_file() else { return Ok(()) };
    ed(|e| e.export_camera_track(clip(id), format, &path, &picked))
}

/// Stabilise clip `id` with its solved camera: keep camera moves slower than
/// `smoothing` frames, or (0) hold the camera still where it is at the playhead.
#[tauri::command]
fn camtrack_stabilise(id: u64, smoothing: f32, zoom: reelforge_core::track::StabZoom) -> Res<()> {
    ed(|e| {
        let path = if smoothing > 0.0 {
            reelforge_core::camtrack::StabPath::Smooth(smoothing)
        } else {
            let frame = e.seq().find_clip(clip(id)).and_then(|(_, c)| e.camera_track_frame(c)).unwrap_or(0);
            reelforge_core::camtrack::StabPath::Lock(frame)
        };
        e.stabilise_with_camera(clip(id), path, zoom)
    })
}

/// Import a camera: a `.chan` path becomes a camera clip; a Reelforge solve goes onto the selected clip.
#[tauri::command]
fn camtrack_import() -> Res<()> {
    let Some(path) = rfd::FileDialog::new().add_filter("Camera files", &["chan", "json"]).add_filter("All files", &["*"]).pick_file() else { return Ok(()) };
    ed(|e| e.import_camera_file(&path))
}

/// Move the playhead to frame `frame` of the camera solve.
#[tauri::command]
fn camtrack_seek(id: u64, frame: usize) -> Res<()> {
    ed(|e| e.seek_camera_frame(clip(id), frame))
}

/// Delete solved points (`points`, and any fitting worse than `max_error` px) and solve again.
#[tauri::command]
fn camtrack_resolve(id: u64, points: Vec<usize>, max_error: Option<f32>) -> Res<()> {
    ed(|e| e.resolve_camera(clip(id), &points, max_error))
}

#[tauri::command]
fn camtrack_remove(id: u64) -> Res<()> {
    ed(|e| e.remove_camera_track(clip(id)))
}

#[tauri::command]
fn track_cancel() -> Res<()> {
    ed(|e| e.cancel_tracking())
}

/// Apply the tracked motion of `id` to `target` (itself = stabilise) or to its mask `mask`.
#[tauri::command]
fn track_apply(id: u64, target: u64, mask: Option<usize>, blade: Option<usize>, smoothing: f32, axes: Option<reelforge_core::track::Axes>, zoom: Option<reelforge_core::track::StabZoom>) -> Res<()> {
    let axes = axes.unwrap_or(reelforge_core::track::Axes::ALL);
    ed(|e| match (mask, blade) {
        (Some(i), _) => e.apply_track_to_mask(clip(id), i),
        (None, Some(i)) => e.apply_track_to_blade(clip(id), clip(target), i, axes),
        (None, None) => e.apply_track_with(clip(id), clip(target), smoothing.clamp(0.0, 120.0), axes, zoom.unwrap_or_default()),
    })
}

// ---- Files on disk ------------------------------------------------------------

/// Open the folder holding `path` in the file manager.
#[tauri::command]
fn show_in_folder(path: String) -> Res<()> {
    let dir = PathBuf::from(&path).parent().map(PathBuf::from).unwrap_or_default();
    #[cfg(windows)]
    let r = std::process::Command::new("explorer").arg(&dir).spawn();
    #[cfg(not(windows))]
    let r = std::process::Command::new("xdg-open").arg(&dir).spawn();
    r.map(|_| ()).map_err(|e| e.to_string())
}

// ---- Startup -----------------------------------------------------------------

/// Windows: is the WebView2 runtime installed? If not, say what to install.
#[cfg(all(windows, target_env = "gnu"))]
fn webview2_ready() -> bool {
    match webview2_shim::runtime_version() {
        Some(v) => {
            log::info!("WebView2 {v}");
            true
        }
        None => {
            webview2_shim::message(
                "Reelforge needs Microsoft Edge WebView2, which isn't installed on this PC.\n\n\
                 Install the \"Evergreen WebView2 Runtime\" from\nhttps://developer.microsoft.com/microsoft-edge/webview2/\n\n\
                 (Windows 10 and 11 usually have it already; it comes with Microsoft Edge.)",
            );
            false
        }
    }
}

#[cfg(not(all(windows, target_env = "gnu")))]
fn webview2_ready() -> bool {
    true
}

// ---- About & diagnostics -------------------------------------------------------

/// Help → About & diagnostics → Diagnostics: the report to copy and paste.
#[tauri::command]
fn diagnostics() -> Res<String> {
    let mut extra: Vec<(&str, String)> = Vec::new();
    #[cfg(all(windows, target_env = "gnu"))]
    extra.push(("WebView2", webview2_shim::runtime_version().unwrap_or_else(|| "not found".into())));
    extra.push(("Decoder check", reelforge_media::selfcheck::last_summary().unwrap_or_else(|| "not run this session".into())));
    extra.push(("Zero-copy check", reelforge_export::zerocheck::last_summary().unwrap_or_else(|| "not run this session".into())));
    extra.push(("Performance check", reelforge_render::bench::last_summary().unwrap_or_else(|| "not run this session".into())));
    ed(|e| {
        let p = &e.prefs;
        extra.push((
            "Settings",
            format!(
                "hardware decoding {}, Reelforge's decoders {}, zero-copy {}, proxies {} (automatic {}), frame cache {} MB, render cache {} MB, autosave every {} min, {} saved export presets",
                p.hw_decode, p.native_decoders, p.zero_copy, p.use_proxies, p.auto_proxies, p.cache_mb, p.render_cache_mb, p.autosave_minutes, p.export_presets.len()
            ),
        ));
        diag::report(e, &extra)
    })
}

/// The third-party licence notices, as shipped (built into the program).
#[tauri::command]
fn notices() -> &'static str {
    include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../THIRD-PARTY-NOTICES.txt"))
}

/// Open a folder in the system's file browser (made first if need be).
fn open_folder(dir: &std::path::Path) -> Res<()> {
    let _ = std::fs::create_dir_all(dir);
    #[cfg(windows)]
    let r = std::process::Command::new("explorer").arg(dir).spawn();
    #[cfg(not(windows))]
    let r = std::process::Command::new("xdg-open").arg(dir).spawn();
    r.map(|_| ()).map_err(|e| e.to_string())
}

/// The Viewer's snapshot button: save the frame at the playhead, full size,
/// in the snapshots folder (`save_as`: ask where, PNG or JPEG). Returns where it went.
#[tauri::command]
fn snapshot(save_as: bool) -> Res<Option<String>> {
    let to = if save_as {
        let (dir, name) = ed(|e| (e.snapshot_folder(), format!("{}.png", e.seq().name)))?;
        let mut dlg = rfd::FileDialog::new().set_title("Save the frame as").add_filter("PNG", &["png"]).add_filter("JPEG", &["jpg", "jpeg"]).set_file_name(name);
        if dir.is_dir() {
            dlg = dlg.set_directory(dir);
        }
        match dlg.save_file() {
            Some(p) => Some(if p.extension().is_none() { p.with_extension("png") } else { p }),
            None => return Ok(None),
        }
    } else {
        None
    };
    ed(|e| match e.snapshot(to) {
        Ok(p) => Ok(Some(p.display().to_string())),
        Err(err) => {
            e.status = format!("Couldn't take the snapshot: {err}");
            Ok(None)
        }
    })?
}

/// The magnetic timeline on or off for the sequence being edited.
#[tauri::command]
fn set_magnetic(on: bool) -> Res<()> {
    ed(|e| e.set_magnetic(on))
}

/// File > Export edit as: the sequence being edited as an EDL ("edl"),
/// FCPXML ("fcpxml"), Final Cut Pro 7 XML ("xml") or OpenTimelineIO
/// ("otio"), to a file chosen here. Returns where it went.
#[tauri::command]
fn export_edit(format: String) -> Res<Option<String>> {
    use reelforge_core::interchange::Format;
    let Some(f) = Format::parse(&format) else { return Err(format!("unknown format {format}")) };
    let (dir, name) = ed(|e| (e.project_path.as_ref().and_then(|p| p.parent().map(|d| d.to_path_buf())), format!("{}.{}", e.seq().name, f.extension())))?;
    let mut dlg = rfd::FileDialog::new().set_title(format!("Export the edit as {}", f.label())).add_filter(f.label(), &[f.extension()]).set_file_name(name);
    if let Some(d) = dir.filter(|d| d.is_dir()) {
        dlg = dlg.set_directory(d);
    }
    let Some(mut path) = dlg.save_file() else { return Ok(None) };
    if path.extension().is_none() {
        path.set_extension(f.extension());
    }
    ed(|e| match e.export_edit(f, &path) {
        Ok(_) => Some(path.display().to_string()),
        Err(err) => {
            e.status = err;
            None
        }
    })
}

/// Snapshot settings: choose their folder (`choose`; else back to the
/// default), or open it.
#[tauri::command]
fn snapshot_folder(choose: bool, open: bool) -> Res<()> {
    if open {
        let dir = ed(|e| e.snapshot_folder())?;
        return open_folder(&dir);
    }
    let picked = if choose {
        match rfd::FileDialog::new().set_title("Folder for snapshots").pick_folder() {
            Some(d) => Some(d),
            None => return Ok(()),
        }
    } else {
        None
    };
    ed(|e| {
        e.prefs.snapshot_folder = picked;
        e.status = format!("Snapshots go to {}", e.snapshot_folder().display());
        e.touch();
        save_settings(e);
    })
}

/// Add snapshots to the Media panel too.
#[tauri::command]
fn snapshot_import(on: bool) -> Res<()> {
    ed(|e| {
        e.prefs.snapshot_import = on;
        e.touch();
        save_settings(e);
    })
}

/// Open the folder with the log files.
#[tauri::command]
fn open_log_folder() -> Res<()> {
    let dir = diag::log_dir();
    let _ = std::fs::create_dir_all(&dir);
    #[cfg(windows)]
    let r = std::process::Command::new("explorer").arg(&dir).spawn();
    #[cfg(not(windows))]
    let r = std::process::Command::new("xdg-open").arg(&dir).spawn();
    r.map(|_| ()).map_err(|e| e.to_string())
}

/// Help → Check decoders: compare every Reelforge decoder with FFmpeg's on
/// samples made here (takes 10–30 seconds; runs off the main thread).
#[tauri::command]
async fn check_decoders() -> Result<Vec<reelforge_media::selfcheck::DecoderCheck>, String> {
    tauri::async_runtime::spawn_blocking(|| reelforge_media::selfcheck::check_decoders(|_, _, _| {})).await.map_err(|e| e.to_string())
}

/// Help → Check decoders → Check zero-copy: decode samples on the graphics
/// card and compare drawing them straight from there with the usual copy.
#[tauri::command]
async fn check_zero_copy() -> Result<reelforge_export::zerocheck::ZeroCopyReport, String> {
    let gpu = ed(|e| e.preview.gpu())?.ok_or("there's no graphics device to check with")?;
    tauri::async_runtime::spawn_blocking(move || reelforge_export::zerocheck::check_zero_copy(&gpu.0, &gpu.1)).await.map_err(|e| e.to_string())
}

/// Help → About → Check → Performance: time the heavy work (optical flow,
/// fluid, particles, a layered frame) on this graphics card at 1080p.
#[tauri::command]
async fn check_performance() -> Result<reelforge_render::bench::BenchReport, String> {
    let gpu = ed(|e| e.preview.gpu())?.ok_or("there's no graphics device to check with")?;
    tauri::async_runtime::spawn_blocking(move || {
        let mut r = reelforge_render::bench::run(&gpu.0, &gpu.1);
        r.lines.extend(roto_model_bench());
        r
    })
    .await
    .map_err(|e| e.to_string())
}

/// The roto brush's object model: encoding one frame on the graphics card
/// and on the CPU (what the first stroke on a frame waits for).
fn roto_model_bench() -> Vec<reelforge_render::bench::BenchLine> {
    use reelforge_render::bench::BenchLine;
    let Some(sam) = reelforge_sam::model() else { return Vec::new() };
    let rgb: Vec<u8> = (0..1280 * 720 * 3).map(|i| ((i * 37 + (i / 3841) * 11) % 251) as u8).collect();
    let (img, _) = reelforge_sam::prepare(&rgb, 1280, 720);
    let line = |what: &str, ms: f64, note: String| BenchLine { what: what.into(), ms: (ms * 10.0).round() / 10.0, verdict: if ms <= 1000.0 { "realtime" } else if ms <= 3000.0 { "slow" } else { "heavy" }, note };
    let time = |f: &dyn Fn()| {
        let t = std::time::Instant::now();
        f();
        t.elapsed().as_secs_f64() * 1000.0
    };
    let mut out = Vec::new();
    if let Some(g) = reelforge_sam::gpu() {
        // (a model that doesn't match its file fails here rather than crashing)
        if reelforge_sam::guard(|| g.encode(&img)).is_none() {
            return out;
        }
        let ms = time(&|| {
            let _ = reelforge_sam::guard(|| g.encode(&img));
        });
        out.push(line("Roto brush: one frame for the object model, on the graphics card", ms, format!("{} (under a second: strokes and carrying feel quick)", g.adapter)));
    }
    let ms = time(&|| {
        let _ = reelforge_sam::guard(|| Some(sam.encode(&img)));
    });
    let gpu = if reelforge_sam::gpu().is_some() { "" } else { "; no suitable graphics card, so the CPU does it" };
    out.push(line("Roto brush: one frame for the object model, on the CPU", ms, format!("{} threads{gpu}", std::thread::available_parallelism().map_or(1, |n| n.get()))));
    out
}

fn main() {
    diag::init_logging();
    if !webview2_ready() {
        return;
    }
    reelforge_media::init();
    reelforge_engine::hwflow::start();
    let app = tauri::Builder::default()
        .setup(|app| {
            use tauri::Manager;
            // The graphics card (the settings may then ask for the CPU instead).
            let gpu = reelforge_render::device_for(reelforge_render::RenderDevice::Gpu).or_else(reelforge_render::headless_device);
            if gpu.is_none() {
                log::warn!("no graphics device: the viewer stays blank");
            }
            let audio = match reelforge_engine::audio::AudioEngine::new() {
                Ok(a) => Some(a),
                Err(err) => {
                    log::warn!("audio output unavailable, playing silently: {err:#}");
                    None
                }
            };
            let mut e = Editor::new(gpu, audio);
            let args: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
            if let Ok(dir) = app.path().app_config_dir() {
                let path = dir.join("settings.json");
                match std::fs::read(&path).ok().and_then(|b| serde_json::from_slice::<Prefs>(&b).ok()) {
                    Some(p) => e.set_prefs(p),
                    // First run: the defaults (rendered frames kept on disk, and so on).
                    None => e.apply_prefs(),
                }
                let _ = SETTINGS_PATH.set(path);
                start::startup(&mut e, &dir, !args.is_empty());
                fx_presets::startup(&dir);
            }
            fx::startup(&mut e);
            openfx::startup();
            e.start_session();
            e.status = "Import media with File > Import, or drop files onto the window.".into();
            // `reelforge project.rfproj` opens it; media files are imported and laid out.
            if let Some(p) = args.iter().find(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("rfproj"))) {
                e.open_project_path(p.clone());
            } else if !args.is_empty() {
                e.import(&args);
                let ids: Vec<MediaId> = e.project.media.iter().map(|m| m.id).collect();
                for id in ids {
                    e.add_to_timeline(id, None, None);
                }
            }
            EDITOR.with(|c| *c.borrow_mut() = Some(e));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            tick,
            get_state,
            diagnostics,
            notices,
            open_log_folder,
            check_decoders,
            check_zero_copy,
            check_performance,
            snapshot,
            snapshot_folder,
            snapshot_import,
            export_edit,
            set_magnetic,
            frame,
            scopes::scope,
            waveform,
            waveform_rate,
            font_families,
            thumbnail,
            comp_thumbnail,
            audio_fx_kinds,
            set_playback_quality,
            set_full_when_paused,
            set_color_depth,
            new_project,
            open_dialog,
            open_path,
            save,
            import_dialog,
            import_paths,
            recover,
            dismiss_recovery,
            set_prefs,
            clear_render_cache,
            disk_free,
            disk_dismiss,
            cache_folder_set,
            settings_info,
            add_to_timeline,
            new_composition_from_media,
            new_composite,
            add_composite,
            open_sequence,
            add_title,
            add_3d_text,
            add_3d_logo,
            add_3d_model,
            shape_from_mask,
            add_blade_layer,
            add_particles_layer,
            add_simulation,
            scene::add_scene,
            scene::add_new,
            scene::set_plane_color,
            scene::set_clip_label,
            scene::set_parent,
            scene::scene_point_drag,
            scene::view_set,
            multicam::create_multicam,
            versions::versions_list,
            versions::save_version,
            versions::restore_version,
            versions::open_version_copy,
            versions::rename_version,
            versions::delete_version,
            versions::set_versions_on_save,
            multicam::switch_angle,
            multicam::switch_angle_at,
            multicam::set_audio_angle,
            multicam::set_multicam_grid,
            fx::light_environment,
            fx::add_environment_light,
            scene::camera_tool_drag,
            scene::camera_tool_wheel,
            select,
            select_many,
            select_all,
            move_clips,
            trim,
            commit,
            action,
            history_go_to,
            seek,
            track_op,
            set_track_audio,
            set_transform,
            set_blend,
            set_track_matte,
            expose_control,
            edit_control,
            set_control,
            save_template,
            add_template,
            set_title,
            font_faces,
            place_title,
            set_clip_audio,
            set_speed,
            time_remap,
            time_remap_value,
            time_remap_key,
            time_remap_move_key,
            time_remap_interp,
            set_frame_blend,
            set_flow_quality,
            add_mask,
            add_freehand_mask,
            set_masks,
            set_mask_points,
            set_mask_curves,
            auto_duck,
            remove_ducking,
            mask_point_curve,
            mask_point_add,
            mask_point_add_at,
            mask_from_key,
            roto_stroke,
            roto_prepare,
            upscale_media,
            upscale_cancel,
            nvidia_media,
            nvidia_available,
            color_match,
            color_match_cancel,
            color_match_pick,
            roto_pick,
            remove_background,
            replace_background,
            roto_propagate,
            mask_smooth,
            mask_point_delete,
            mask_op,
            tracker_add,
            tracker_mode,
            tracker_sizes,
            tracker_smooth,
            tracker_grid,
            tracker_move_many,
            mesh_remove,
            tracker_move,
            tracker_op,
            track_start,
            camtrack_start,
            camtrack_make,
            camtrack_ground,
            camtrack_remove,
            camtrack_resolve,
            camtrack_seek,
            camtrack_export,
            camtrack_import,
            camtrack_stabilise,
            track_cancel,
            mask_track,
            track_apply,
            show_in_folder,
            export::export_options,
            export::export_suggest,
            export::export_path_dialog,
            export::export_preset_save,
            export::export_preset_delete,
            export::export_add,
            export::water_element_path,
            export::export_water_element,
            export::export_estimate,
            export::export_job,
            export::export_run,
            export::export_clear,
            fx::effect_add,
            fx::effect_thumb,
            fx::effect_plate,
            fx::effect_key_pick,
            fx_presets::fx_preset_list,
            fx_presets::fx_preset_save,
            fx_presets::fx_preset_apply,
            fx_presets::fx_preset_rename,
            fx_presets::fx_preset_delete,
            fx_presets::fx_preset_folders,
            fx_presets::fx_preset_clip_extras,
            fx_presets::fx_preset_preview,
            fx_presets::fx_preset_preview_end,
            fx_presets::fx_preset_thumb,
            fx_presets::fx_preset_favourite,
            fx_presets::fx_preset_folder,
            fx_presets::fx_preset_export,
            fx_presets::fx_preset_import,
            fx::effect_op,
            fx::effect_param,
            fx::effect_params,
            fx::effect_key,
            fx::effect_stopwatch,
            fx::blade_preset,
            fx::particles_preset,
            fx::blade_ignite,
            fx::object_removal,
            fx::find_faces,
            fx::face_on,
            fx::find_sky,
            fx::find_depth,
            fx::sky_image,
            fx::paint_stroke,
            fx::paint_clear,
            fx::set_hud_text,
            fx::effect_curves,
            fx::effect_hue_curves,
            fx::effect_lut,
            fx::effect_catalog,
            fx::plugins_list,
            fx::plugins_install,
            fx::plugins_new,
            fx::plugins_remove,
            fx::plugins_reload,
            fx::plugins_live,
            openfx::ofx_list,
            openfx::ofx_rescan,
            openfx::ofx_enable,
            openfx::ofx_add_folder,
            openfx::ofx_remove_folder,
            openfx::ofx_unblock,
            openfx::ofx_open_folder,
            openfx::ofx_gpu,
            start::start_view,
            start::start_at_launch,
            start::start_new_preset,
            start::start_new_custom,
            start::pixel_aspects,
            start::set_sequence_settings,
            start::start_new_template,
            start::start_open_recent,
            start::start_forget,
            start::start_thumb,
            start::save_as_template,
            start::template_remove,
            start::start_screen_setting,
            fx::open_folder,
            keys::transform_key,
            keys::transform_stopwatch,
            keys::key_move,
            keys::key_retime,
            keys::keys_shift,
            keys::keys_interp,
            keys::keys_delete,
            keys::keys_copy,
            keys::keys_paste,
            keys::audio_channel_value,
            keys::audio_stopwatch,
            keys::audio_key,
            keys::key_interp,
            keys::key_handle,
            keys::key_delete,
            keys::key_add,
            ops::marker_colors,
            ops::attr_offers,
            keys::transform_op,
            keys::transform_interp,
            viewer::viewer_hover,
            viewer::viewer_press,
            viewer::viewer_drag,
            viewer::viewer_release,
            open_autosave_dialog,
            open_adobe_dialog,
            renderers,
            dismiss_import_report,
            rendered_runs,
            ops::paste_attributes,
            ops::marker_add,
            ops::marker_set,
            ops::marker_remove,
            ops::proxy_make,
            ops::proxy_delete,
            ops::proxies_delete_all,
            ops::proxies_remake,
            ops::proxies_use,
            ops::proxy_folder,
            ops::relink_media,
            ops::relink_search,
            ops::relink_model,
            ops::find_model_textures,
            ops::replace_footage,
            ops::replace_clip_footage,
            ops::collect_plan,
            ops::collect_pick_folder,
            ops::collect_start,
            ops::collect_stop,
            ops::collect_dismiss,
            trans::transition_list,
            trans::transition_apply,
            trans::transition_kind,
            trans::transition_length,
            trans::transition_remove,
            trans::speed_ramp,
        ])
        .build(tauri::generate_context!());
    let app = match app {
        Ok(a) => a,
        Err(err) => {
            log::error!("couldn't open the window: {err}");
            #[cfg(all(windows, target_env = "gnu"))]
            webview2_shim::message(&format!("Reelforge couldn't open its window:\n\n{err}"));
            return;
        }
    };
    app.run(|_, event| {
        if let tauri::RunEvent::Exit = event {
            // A clean exit: this session's autosave isn't offered for recovery next time.
            let _ = ed(|e| {
                e.playback.pause();
                if let Some(s) = &e.session {
                    s.end();
                }
                // Rendered frames still being written to disk are finished.
                if let Some(d) = &e.render_cache.disk {
                    d.flush();
                }
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shrinking_averages_blocks() {
        // 4×2 → 2×1: each output pixel is the mean of a 2×2 block.
        let mut px = vec![0u8; 4 * 2 * 4];
        px[0..4].copy_from_slice(&[200, 100, 0, 255]);
        let (out, w, h) = shrink(&px, 4, 2, 2);
        assert_eq!((w, h), (2, 1));
        assert_eq!(&out[0..4], &[50, 25, 0, 64]);
        assert_eq!(&out[4..8], &[0, 0, 0, 0]);
    }
}
