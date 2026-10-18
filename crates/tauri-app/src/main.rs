//! Reelforge with a web interface: Tauri hosts the Svelte UI in `ui/` and
//! this file gives it the editor (`reelforge_engine::editor::Editor`)
//! through commands.
//!
//! The editor lives on the main thread (Tauri runs non-async commands
//! there), so no locking is needed: every command borrows it in turn. The UI
//! calls `tick` every animation frame, which moves playback along and renders
//! the viewer picture; it then fetches the full state when `rev` changed and
//! the picture when `picture_rev` changed.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(all(windows, target_env = "gnu"))]
mod webview2_shim;
mod export;
mod fx;
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
    playhead: i64,
    playing: bool,
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
    /// A tracking run: the clip and how far along (0–1).
    tracking: Option<(u64, f32)>,
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
        let m = &e.meters;
        let n = e.seq().tracks.len();
        Live {
            rev: e.rev,
            picture_rev: e.preview.picture_rev,
            playhead: e.playback.playhead.0,
            playing: e.playback.playing,
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
            tracking: e.tracking.as_ref().map(|j| (j.clip.0, j.progress)),
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
        let Some((rgba, [w, h])) = e.preview.read_output() else { return Response::new(vec![0u8]) };
        let k = (w / max_w.max(1)).min(h / max_h.max(1)).max(1);
        let (rgba, w, h) = if k > 1 { shrink(&rgba, w, h, k) } else { (rgba, w, h) };
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
fn shrink(rgba: &[u8], w: u32, h: u32, k: u32) -> (Vec<u8>, u32, u32) {
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
        let mut open = OPEN.lock().unwrap();
        match open.iter().position(|(p, _)| *p == path) {
            Some(i) => open.remove(i).1,
            None => reelforge_media::FrameSource::open(&path, Some((320, 180))).map_err(|e| format!("{e:#}"))?,
        }
    };
    let frame = src.frame_at_blocking(Time(at.max(0)), std::time::Duration::from_secs(5));
    {
        let mut open = OPEN.lock().unwrap();
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
        Ok(()) => true,
        Err(err) => {
            e.status = format!("Save failed: {err:#}");
            false
        }
    })
}

#[tauri::command]
fn import_dialog() -> Res<()> {
    let Some(paths) = rfd::FileDialog::new().add_filter("Media", &MEDIA_EXTENSIONS).add_filter("All files", &["*"]).pick_files() else { return Ok(()) };
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

#[tauri::command]
fn set_prefs(prefs: Prefs) -> Res<()> {
    ed(|e| {
        e.prefs = prefs;
        e.render_cache.set_budget_mb(e.prefs.render_cache_mb);
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
        "nudge_left" => e.nudge(-1),
        "nudge_right" => e.nudge(1),
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
fn track_op(index: usize, op: String, name: Option<String>) -> Res<()> {
    ed(|e| match op.as_str() {
        "mute" => e.toggle_mute(index),
        "lock" => e.toggle_lock(index),
        "delete" => e.delete_track(index),
        "rename" => e.rename_track(index, name.unwrap_or_default()),
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
    ed(|e| {
        if let Some(mut v) = e.visual(clip(id)) {
            v.transform = transform;
            e.set_visual_live(clip(id), v);
        }
    })
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

#[tauri::command]
fn set_title(id: u64, spec: TitleSpec) -> Res<()> {
    ed(|e| e.set_title_live(clip(id), spec))
}

#[tauri::command]
fn set_clip_audio(id: u64, audio: ClipAudio) -> Res<()> {
    ed(|e| e.set_clip_audio_live(clip(id), audio))
}

#[tauri::command]
fn set_speed(id: u64, speed: f32, reverse: bool) -> Res<()> {
    ed(|e| e.set_speed(clip(id), speed, reverse))
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

/// Move tracker point `point` to `pos` (layer pixels) at the playhead: on a
/// tracked frame this corrects the track; elsewhere it's where tracking starts.
#[tauri::command]
fn tracker_move(id: u64, point: usize, pos: [f32; 2]) -> Res<()> {
    ed(|e| {
        if e.tracking.is_some() {
            return;
        }
        let Some((_, c)) = e.seq().find_clip(clip(id)) else { return };
        let (Some(mut t), [w, h], src) = (c.tracker.clone(), e.layer_size(c), e.track_time(c)) else { return };
        let Some(p) = t.points.get_mut(point) else { return };
        p.move_to(src, [pos[0].clamp(0.0, w), pos[1].clamp(0.0, h)]);
        e.set_tracker_live(clip(id), Some(t));
    })
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

#[tauri::command]
fn track_cancel() -> Res<()> {
    ed(|e| e.cancel_tracking())
}

/// Apply the tracked motion of `id` to `target` (itself = stabilise) or to its mask `mask`.
#[tauri::command]
fn track_apply(id: u64, target: u64, mask: Option<usize>, smoothing: f32) -> Res<()> {
    ed(|e| match mask {
        Some(i) => e.apply_track_to_mask(clip(id), i),
        None => e.apply_track(clip(id), clip(target), smoothing.clamp(0.0, 120.0)),
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
                "Reelforge's new interface needs Microsoft Edge WebView2, which isn't installed on this PC.\n\n\
                 Install the \"Evergreen WebView2 Runtime\" from\nhttps://developer.microsoft.com/microsoft-edge/webview2/\n\n\
                 Meanwhile, reelforge.exe (the classic interface) works without it.",
            );
            false
        }
    }
}

#[cfg(not(all(windows, target_env = "gnu")))]
fn webview2_ready() -> bool {
    true
}

fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    if !webview2_ready() {
        return;
    }
    reelforge_media::init();
    let app = tauri::Builder::default()
        .setup(|app| {
            use tauri::Manager;
            let gpu = reelforge_render::headless_device();
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
            if let Ok(dir) = app.path().app_config_dir() {
                let path = dir.join("settings.json");
                if let Some(p) = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice::<Prefs>(&b).ok()) {
                    e.prefs = p;
                    e.render_cache.set_budget_mb(e.prefs.render_cache_mb);
                }
                let _ = SETTINGS_PATH.set(path);
            }
            fx::startup(&mut e);
            e.start_session();
            e.status = "Import media with File > Import, or drop files onto the window.".into();
            // `reelforge-tauri project.rfproj` opens it; media files are imported and laid out.
            let args: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
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
            frame,
            waveform,
            waveform_rate,
            font_families,
            thumbnail,
            new_project,
            open_dialog,
            open_path,
            save,
            import_dialog,
            import_paths,
            recover,
            dismiss_recovery,
            set_prefs,
            add_to_timeline,
            new_composition_from_media,
            new_composite,
            add_composite,
            open_sequence,
            add_title,
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
            set_title,
            set_clip_audio,
            set_speed,
            add_mask,
            add_freehand_mask,
            set_masks,
            set_mask_points,
            mask_op,
            tracker_add,
            tracker_mode,
            tracker_sizes,
            tracker_move,
            tracker_op,
            track_start,
            track_cancel,
            track_apply,
            show_in_folder,
            export::export_options,
            export::export_suggest,
            export::export_path_dialog,
            export::export_preset_save,
            export::export_preset_delete,
            export::export_add,
            export::export_job,
            export::export_run,
            export::export_clear,
            fx::effect_add,
            fx::effect_op,
            fx::effect_param,
            fx::effect_key,
            fx::effect_curves,
            fx::effect_lut,
            fx::effect_catalog,
            fx::plugins_list,
            fx::plugins_install,
            fx::plugins_new,
            fx::plugins_remove,
            fx::plugins_reload,
            fx::plugins_live,
            fx::open_folder,
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
