//! Project housekeeping for the web interface: markers, proxies, relinking
//! missing media, and Collect project.

use crate::{ed, Res};
use reelforge_core::collect::{plan_collect, CollectOptions};
use reelforge_core::{MarkerColor, MediaId, Time};
use reelforge_engine::collect::{run_collect, safe_name, Report};
use serde::Serialize;
use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

// ---- Markers --------------------------------------------------------------------

#[derive(Serialize)]
pub struct MarkerView {
    pub at: i64,
    pub name: String,
    pub color: MarkerColor,
    /// "#rrggbb".
    pub rgb: String,
}

pub fn markers(s: &reelforge_core::Sequence) -> Vec<MarkerView> {
    s.markers
        .iter()
        .map(|m| {
            let [r, g, b] = m.color.rgb();
            MarkerView { at: m.at.0, name: m.name.clone(), color: m.color, rgb: format!("#{r:02x}{g:02x}{b:02x}") }
        })
        .collect()
}

/// The marker colours, with their swatches.
#[tauri::command]
pub fn marker_colors() -> Vec<(MarkerColor, String)> {
    MarkerColor::ALL
        .into_iter()
        .map(|c| {
            let [r, g, b] = c.rgb();
            (c, format!("#{r:02x}{g:02x}{b:02x}"))
        })
        .collect()
}

/// A marker at the playhead (M).
#[tauri::command]
pub fn marker_add() -> Res<()> {
    ed(|e| e.add_marker())
}

/// Change marker `index` (move, rename, recolour): live, `commit` ends it.
/// `at` is snapped to a frame.
#[tauri::command]
pub fn marker_set(index: usize, at: i64, name: String, color: MarkerColor) -> Res<()> {
    ed(|e| {
        let rate = e.seq().frame_rate;
        let mut list = e.seq().markers.clone();
        let Some(m) = list.get_mut(index) else { return };
        *m = reelforge_core::Marker { at: Time(at).max(Time::ZERO).snap(rate), name, color };
        e.set_markers_live(list);
    })
}

#[tauri::command]
pub fn marker_remove(index: usize) -> Res<()> {
    ed(|e| e.remove_marker(index))
}

// ---- Proxies --------------------------------------------------------------------

/// Make (or retry) a proxy for a media item.
#[tauri::command]
pub fn proxy_make(media: u64) -> Res<()> {
    ed(|e| {
        let Some(m) = e.project.media(MediaId(media)).cloned() else { return };
        if m.info.video.is_none() || m.info.duration == Time::ZERO {
            e.status = "Only video gets proxies".into();
            return;
        }
        if matches!(e.proxies.state(&m.path), Some(reelforge_engine::proxies::ProxyState::Failed(_))) {
            e.proxies.forget(&m.path);
        }
        e.proxies.request(&m.path, &m.info);
        e.status = format!("Making a proxy for {}", m.name);
    })
}

/// Delete a media item's proxy file (the original is used from now on).
#[tauri::command]
pub fn proxy_delete(media: u64) -> Res<()> {
    ed(|e| {
        let Some(m) = e.project.media(MediaId(media)).cloned() else { return };
        e.preview.reset(); // close the proxy file first (Windows can't delete an open file)
        let r = e.proxies.delete(&m.path);
        e.status = match r {
            Ok(()) => format!("Deleted the proxy of {}", m.name),
            Err(err) => format!("Couldn't delete the proxy: {err}"),
        };
    })
}

/// Delete every proxy file.
#[tauri::command]
pub fn proxies_delete_all() -> Res<()> {
    ed(|e| {
        e.preview.reset();
        e.proxies.delete_all();
        e.status = "Deleted all proxies".into();
    })
}

/// Make new proxies as `format` (saved in the settings), and remake the
/// project's existing ones in it. Returns how many are being remade.
#[tauri::command]
pub fn proxies_remake(format: reelforge_media::proxy::ProxyFormat) -> Res<usize> {
    ed(|e| {
        e.prefs.proxy_format = format;
        e.apply_prefs();
        crate::save_settings(e);
        e.remake_proxies()
    })
}

/// Show proxies in the viewer (or the original files).
#[tauri::command]
pub fn proxies_use(on: bool) -> Res<()> {
    ed(|e| {
        e.prefs.use_proxies = on;
        e.preview.reset();
        crate::save_settings(e);
        e.status = if on { "Proxies on: the viewer plays the small copies (export always uses the originals)" } else { "Proxies off: the viewer plays the original files" }.into();
    })
}

/// The proxy folder (to open it).
#[tauri::command]
pub fn proxy_folder() -> String {
    reelforge_media::proxy::cache_dir().join("proxies").display().to_string()
}

// ---- Relinking --------------------------------------------------------------------

/// Find a missing media file (asks which file), then look for the other
/// missing ones in the same folder.
#[tauri::command]
pub fn relink_media(media: u64) -> Res<()> {
    let (name, dir) = ed(|e| e.project.media(MediaId(media)).map(|m| (m.name.clone(), m.path.parent().map(PathBuf::from))))?.ok_or("no such media")?;
    let mut dlg = rfd::FileDialog::new().set_title(format!("Find {name}")).add_filter("All files", &["*"]);
    if let Some(d) = dir.filter(|d| d.is_dir()) {
        dlg = dlg.set_directory(d);
    }
    let Some(path) = dlg.pick_file() else { return Ok(()) };
    ed(|e| e.relink(MediaId(media), path))
}

/// Search a folder (and its subfolders) for every missing file by name.
#[tauri::command]
pub fn relink_search() -> Res<()> {
    let Some(dir) = rfd::FileDialog::new().set_title("Folder to search for missing media").pick_folder() else { return Ok(()) };
    ed(|e| e.find_missing_in(&dir))
}

// ---- Collect project ----------------------------------------------------------------

#[derive(Serialize)]
pub struct CollectPlanView {
    pub files: usize,
    pub bytes: u64,
    /// Already in the folder from an earlier collect (won't be copied again).
    pub already: usize,
    pub missing: Vec<String>,
    pub project_file: String,
    /// A project file of that name is there and will be replaced.
    pub replaces: bool,
    pub unsaved: bool,
}

/// What collecting into `dest` as `name` would do.
#[tauri::command]
pub fn collect_plan(dest: String, name: String, include_unused: bool, include_fonts: bool) -> Res<CollectPlanView> {
    let dest = PathBuf::from(dest);
    ed(|e| {
        e.commit_live_edit();
        let plan = plan_collect(&e.project, &dest, &safe_name(&name), CollectOptions { include_unused, include_fonts });
        CollectPlanView {
            files: plan.items.len(),
            bytes: plan.total_bytes,
            already: plan.items.iter().filter(|i| dest.join(&i.to).exists()).count(),
            missing: plan.missing.iter().map(|p| p.display().to_string()).collect(),
            project_file: plan.project_file.display().to_string(),
            replaces: plan.project_file.exists(),
            unsaved: e.project_path.is_none() || e.is_dirty(),
        }
    })
}

/// Ask for the folder to collect into.
#[tauri::command]
pub fn collect_pick_folder() -> Option<String> {
    rfd::FileDialog::new().set_title("Folder to collect the project into").pick_folder().map(|p| p.display().to_string())
}

#[derive(Default)]
struct Progress {
    bytes: u64,
    file: String,
    index: usize,
    result: Option<Result<Report, String>>,
}

struct Job {
    cancel: Arc<AtomicBool>,
    progress: Arc<Mutex<Progress>>,
    total: u64,
    count: usize,
}

thread_local! {
    static COLLECT: RefCell<Option<Job>> = const { RefCell::new(None) };
}

#[derive(Serialize, Clone)]
pub struct CollectView {
    pub running: bool,
    pub bytes: u64,
    pub total: u64,
    pub file: String,
    pub index: usize,
    pub count: usize,
    pub stopping: bool,
    /// When it's over: what happened.
    pub done: Option<CollectDone>,
}

#[derive(Serialize, Clone)]
pub struct CollectDone {
    pub ok: bool,
    pub error: String,
    pub project_file: String,
    pub folder: String,
    pub copied: usize,
    pub reused: usize,
    pub bytes: u64,
    pub missing: Vec<String>,
    pub seconds: f64,
}

/// Start collecting in the background.
#[tauri::command]
pub fn collect_start(dest: String, name: String, include_unused: bool, include_fonts: bool) -> Res<()> {
    if COLLECT.with(|c| c.borrow().as_ref().is_some_and(|j| j.progress.lock().unwrap().result.is_none())) {
        return Err("A collect is already running".into());
    }
    let dest = PathBuf::from(dest);
    let plan = ed(|e| {
        e.commit_live_edit();
        e.status = "Collecting the project…".into();
        plan_collect(&e.project, &dest, &safe_name(&name), CollectOptions { include_unused, include_fonts })
    })?;
    let cancel = Arc::new(AtomicBool::new(false));
    let progress = Arc::new(Mutex::new(Progress::default()));
    let (stop, p) = (cancel.clone(), progress.clone());
    let (total, count) = (plan.total_bytes, plan.items.len());
    std::thread::Builder::new()
        .name("collect".into())
        .spawn(move || {
            let r = run_collect(&plan, &stop, |bytes, file, index| {
                let mut g = p.lock().unwrap();
                (g.bytes, g.index) = (bytes, index);
                if g.file != file {
                    g.file = file.to_string();
                }
            });
            p.lock().unwrap().result = Some(r);
        })
        .map_err(|e| e.to_string())?;
    COLLECT.with(|c| *c.borrow_mut() = Some(Job { cancel, progress, total, count }));
    Ok(())
}

#[tauri::command]
pub fn collect_stop() {
    COLLECT.with(|c| {
        if let Some(j) = c.borrow().as_ref() {
            j.cancel.store(true, Ordering::Relaxed);
        }
    });
}

/// Forget a finished collect (the window was closed).
#[tauri::command]
pub fn collect_dismiss() {
    COLLECT.with(|c| {
        let mut c = c.borrow_mut();
        if c.as_ref().is_some_and(|j| j.progress.lock().unwrap().result.is_some()) {
            *c = None;
        }
    });
}

/// For `tick`: how a collect is going (and the status line when it ends).
pub fn view(e: &mut reelforge_engine::editor::Editor) -> Option<CollectView> {
    COLLECT.with(|c| {
        let c = c.borrow();
        let j = c.as_ref()?;
        let p = j.progress.lock().unwrap();
        let done = p.result.as_ref().map(|r| match r {
            Ok(rep) => CollectDone {
                ok: true,
                error: String::new(),
                project_file: rep.project_file.display().to_string(),
                folder: rep.project_file.parent().map(|d| d.display().to_string()).unwrap_or_default(),
                copied: rep.copied,
                reused: rep.reused,
                bytes: rep.bytes,
                missing: rep.missing.iter().map(|m| m.display().to_string()).collect(),
                seconds: rep.seconds,
            },
            Err(err) => CollectDone { ok: false, error: err.clone(), project_file: String::new(), folder: String::new(), copied: 0, reused: 0, bytes: 0, missing: Vec::new(), seconds: 0.0 },
        });
        if let Some(d) = &done {
            let msg = if d.ok { format!("Collected the project into {}", d.folder) } else { format!("Collect project: {}", d.error) };
            if e.status == "Collecting the project…" {
                e.status = msg;
            }
        }
        Some(CollectView {
            running: done.is_none(),
            bytes: p.bytes,
            total: j.total,
            file: p.file.clone(),
            index: p.index,
            count: j.count,
            stopping: j.cancel.load(Ordering::Relaxed),
            done,
        })
    })
}

// ---- Paste attributes ----------------------------------------------------------

/// What the copied clip can give (for the Paste attributes dialog).
#[derive(Serialize)]
pub struct AttrOffers {
    pub from: String,
    pub targets: usize,
    pub visual: bool,
    pub corner_pin: bool,
    pub effects: usize,
    pub masks: usize,
    pub audio: bool,
    pub text_style: bool,
}

/// None when nothing has been copied.
#[tauri::command]
pub fn attr_offers() -> Res<Option<AttrOffers>> {
    ed(|e| {
        if e.clipboard.is_empty() {
            return None;
        }
        let src = reelforge_core::attrs::Source::of(&e.clipboard);
        let o = src.offers();
        let from = src.video.or(src.audio).map(|c| c.display_name(&e.project).into_owned()).unwrap_or_default();
        Some(AttrOffers { from, targets: e.selected_ids().len(), visual: o.visual, corner_pin: o.corner_pin, effects: o.effects, masks: o.masks, audio: o.audio, text_style: o.text_style })
    })
}

#[tauri::command]
pub fn paste_attributes(set: reelforge_core::attrs::AttrSet) -> Res<()> {
    ed(|e| e.paste_attributes(set))
}
