//! The start screen: new projects from presets or your templates, and your
//! recent projects (with a picture of each from when it was last saved).

use crate::{ed, Res};
use reelforge_core::FrameRate;
use reelforge_engine::editor::Editor;
use reelforge_engine::start::{self, UserTemplate, PRESETS};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use tauri::ipc::Response;

static TEMPLATES: OnceLock<PathBuf> = OnceLock::new();
/// Show the start screen once the window is up (set at startup).
static AT_LAUNCH: AtomicBool = AtomicBool::new(false);

/// At startup: the recent list and the templates folder, from the app's
/// settings folder; whether to open with the start screen.
pub fn startup(e: &mut Editor, config: &Path, opened_something: bool) {
    e.recent = start::Recent::load(config.join("recent.json"));
    let _ = TEMPLATES.set(config.join("templates"));
    AT_LAUNCH.store(e.prefs.start_screen && !opened_something, Ordering::Relaxed);
}

fn templates_dir() -> PathBuf {
    TEMPLATES.get().cloned().unwrap_or_else(|| std::env::temp_dir().join("reelforge-templates"))
}

#[derive(Serialize)]
pub struct PresetView {
    pub id: &'static str,
    pub group: &'static str,
    pub name: &'static str,
    pub detail: &'static str,
    pub width: u32,
    pub height: u32,
    pub summary: String,
    /// The frame rates it can start at: (numerator, denominator, "29.97"), and
    /// which one is its own.
    pub rates: Vec<(u32, u32, String)>,
    pub rate: (u32, u32),
    /// Its own pixel shape (width, height).
    pub aspect: (u32, u32),
}

#[derive(Serialize)]
pub struct RecentView {
    pub path: String,
    pub name: String,
    pub folder: String,
    pub summary: String,
    /// Seconds since 1970.
    pub at: u64,
    /// The file is still there.
    pub exists: bool,
    pub thumb: bool,
    /// It's the project open now.
    pub open: bool,
}

#[derive(Serialize)]
pub struct StartView {
    pub presets: Vec<PresetView>,
    /// Every pixel shape a new project can have: name, width, height.
    pub aspects: Vec<(&'static str, u32, u32)>,
    pub recent: Vec<RecentView>,
    pub templates: Vec<UserTemplate>,
    pub templates_dir: String,
    /// Shown when Reelforge opens.
    pub at_startup: bool,
    /// The project open now has unsaved changes.
    pub dirty: bool,
    /// The project open now has something in it (else starting anew loses nothing).
    pub has_project: bool,
}

#[tauri::command]
pub fn start_view() -> Res<StartView> {
    ed(|e| {
        let open = e.project_path.as_ref().map(|p| std::fs::canonicalize(p).unwrap_or_else(|_| p.clone()));
        StartView {
            presets: PRESETS
                .iter()
                .map(|p| PresetView {
                    id: p.id,
                    group: p.group,
                    name: p.name,
                    detail: p.detail,
                    width: p.width,
                    height: p.height,
                    summary: p.summary(),
                    rates: p.rates().into_iter().map(|(n, d)| (n, d, start::rate_label(FrameRate::new(n as i64, d as i64)))).collect(),
                    rate: p.rate,
                    aspect: p.aspect,
                })
                .collect(),
            aspects: reelforge_core::project::PIXEL_ASPECTS.to_vec(),
            recent: e
                .recent
                .list
                .iter()
                .map(|r| RecentView {
                    path: r.path.display().to_string(),
                    name: r.path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                    folder: r.path.parent().map(|d| d.display().to_string()).unwrap_or_default(),
                    summary: r.summary.clone(),
                    at: r.at,
                    exists: r.path.is_file(),
                    thumb: e.recent.thumbnail(&r.path).is_some_and(|t| t.is_file()),
                    open: open.as_ref() == Some(&r.path),
                })
                .collect(),
            templates: start::user_templates(&templates_dir()),
            templates_dir: templates_dir().display().to_string(),
            at_startup: e.prefs.start_screen,
            dirty: e.is_dirty(),
            has_project: e.project_path.is_some() || !e.project.media.is_empty() || e.project.sequences.iter().any(|s| s.tracks.iter().any(|t| !t.clips.is_empty())),
        }
    })
}

/// Show the start screen now? True once, right after launch.
#[tauri::command]
pub fn start_at_launch() -> bool {
    AT_LAUNCH.swap(false, Ordering::Relaxed)
}

#[tauri::command]
pub fn start_new_preset(id: String, rate: Option<(u32, u32)>, aspect: Option<(u32, u32)>) -> Res<()> {
    let p = start::preset(&id).ok_or_else(|| format!("no preset \"{id}\""))?;
    // One of the rates it offers (its own when none is given).
    let (n, d) = rate.filter(|r| p.rates().contains(r)).unwrap_or(p.rate);
    // One of the pixel shapes (its own when none is given).
    let aspect = aspect.filter(|a| reelforge_core::project::PIXEL_ASPECTS.iter().any(|x| (x.1, x.2) == *a)).unwrap_or(p.aspect);
    ed(|e| e.new_project_with_aspect(p.width, p.height, FrameRate::new(n as i64, d as i64), aspect))
}

/// A new project of any size: `rate` as numerator and denominator (30/1, 30000/1001…).
/// Every pixel shape a sequence can have: name, width, height.
#[tauri::command]
pub fn pixel_aspects() -> Vec<(&'static str, u32, u32)> {
    reelforge_core::project::PIXEL_ASPECTS.to_vec()
}

/// Timeline → Sequence settings…: the open sequence's name, frame size,
/// frame rate and pixel shape (one undo step).
#[tauri::command]
pub fn set_sequence_settings(name: String, width: u32, height: u32, num: u32, den: u32, aspect: (u32, u32)) -> Res<()> {
    ed(|e| e.set_sequence_settings(&name, width, height, FrameRate::new(num as i64, den.max(1) as i64), aspect))
}

#[tauri::command]
pub fn start_new_custom(width: u32, height: u32, num: u32, den: u32, aspect: Option<(u32, u32)>) -> Res<()> {
    if width < 16 || height < 16 || width > 16384 || height > 16384 {
        return Err("Width and height go from 16 to 16384".into());
    }
    if num == 0 || den == 0 || num as f64 / den as f64 > 240.0 {
        return Err("The frame rate goes up to 240 fps".into());
    }
    let aspect = aspect.filter(|a| reelforge_core::project::PIXEL_ASPECTS.iter().any(|x| (x.1, x.2) == *a)).unwrap_or((1, 1));
    ed(|e| e.new_project_with_aspect(width, height, FrameRate::new(num as i64, den as i64), aspect))
}

#[tauri::command]
pub fn start_new_template(path: String) -> Res<()> {
    ed(|e| e.new_from_template(Path::new(&path)))
}

#[tauri::command]
pub fn start_open_recent(path: String) -> Res<bool> {
    let p = PathBuf::from(&path);
    if !p.is_file() {
        let _ = ed(|e| e.status = format!("{path} isn't there any more"));
        return Ok(false);
    }
    ed(|e| {
        e.open_project_path(p.clone());
        e.project_path.is_some()
    })
}

#[tauri::command]
pub fn start_forget(path: Option<String>) -> Res<()> {
    ed(|e| match path {
        Some(p) => {
            if let Some(t) = e.recent.thumbnail(Path::new(&p)) {
                let _ = std::fs::remove_file(t);
            }
            e.recent.remove(Path::new(&p));
        }
        None => {
            for r in e.recent.list.clone() {
                if let Some(t) = e.recent.thumbnail(&r.path) {
                    let _ = std::fs::remove_file(t);
                }
            }
            e.recent.clear();
        }
    })
}

/// A recent project's picture (JPEG; empty if it has none).
#[tauri::command]
pub fn start_thumb(path: String) -> Res<Response> {
    ed(|e| {
        let bytes = e.recent.thumbnail(Path::new(&path)).and_then(|t| std::fs::read(t).ok()).unwrap_or_default();
        Response::new(bytes)
    })
}

/// File > Save as template…: the project as it is now, under `name`.
#[tauri::command]
pub fn save_as_template(name: String) -> Res<String> {
    ed(|e| match e.save_as_template(&templates_dir(), &name) {
        Ok(p) => Ok(p.display().to_string()),
        Err(err) => {
            e.status = format!("Couldn't save the template: {err:#}");
            Err(format!("{err:#}"))
        }
    })?
}

#[tauri::command]
pub fn template_remove(path: String) -> Res<()> {
    let p = PathBuf::from(&path);
    // Only templates in the templates folder.
    if p.parent() != Some(templates_dir().as_path()) {
        return Err("not a template".into());
    }
    std::fs::remove_file(&p).map_err(|e| e.to_string())?;
    let _ = ed(|e| e.status = format!("Removed the template \"{}\"", p.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()));
    Ok(())
}

#[tauri::command]
pub fn start_screen_setting(on: bool) -> Res<()> {
    ed(|e| {
        e.prefs.start_screen = on;
        crate::save_settings(e);
    })
}

/// After a save: a small picture of the viewer, for the start screen.
pub fn save_thumbnail(e: &Editor) {
    let Some(path) = e.recent.list.first().map(|r| r.path.clone()) else { return };
    let Some(out) = e.recent.thumbnail(&path) else { return };
    let Some((rgba, [w, h])) = e.preview.read_output() else { return };
    if w == 0 || h == 0 {
        return;
    }
    let k = (w / 320).max(1);
    let (rgba, w, h) = if k > 1 { crate::shrink(&rgba, w, h, k) } else { (rgba, w, h) };
    let mut jpg = Vec::new();
    if jpeg_encoder::Encoder::new(&mut jpg, 82).encode(&rgba, w as u16, h as u16, jpeg_encoder::ColorType::Rgba).is_ok() {
        if let Some(dir) = out.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = reelforge_engine::safety::write_atomic(&out, &jpg);
    }
}
