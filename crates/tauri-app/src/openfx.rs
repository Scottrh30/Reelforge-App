//! OpenFX plugins for the web interface: which folders are searched, scanning
//! them (in the background at startup), and the OpenFX part of the Plugins
//! window. Loading and running them is `reelforge_export::ofxhost`'s job.

use crate::fx::changed;
use crate::Res;
use reelforge_export::ofxhost::{self, BundleReport};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Where Reelforge keeps its OpenFX settings and crash markers.
fn state_dir() -> PathBuf {
    let plugins = reelforge_engine::plugins::user_dir();
    plugins.parent().map_or_else(|| plugins.join("openfx"), |p| p.join("openfx"))
}

/// A folder of the user's own to drop bundles in.
fn own_folder() -> PathBuf {
    state_dir().join("Plugins")
}

fn read_lines(p: &Path) -> Vec<PathBuf> {
    std::fs::read_to_string(p).map(|s| s.lines().map(str::trim).filter(|l| !l.is_empty()).map(PathBuf::from).collect()).unwrap_or_default()
}

/// Folders added in the Plugins window.
fn extra_folders() -> Vec<PathBuf> {
    read_lines(&state_dir().join("folders.txt"))
}

fn save_extra(list: &[PathBuf]) -> Res<()> {
    let _ = std::fs::create_dir_all(state_dir());
    let text: String = list.iter().map(|p| format!("{}\n", p.display())).collect();
    std::fs::write(state_dir().join("folders.txt"), text).map_err(|e| e.to_string())
}

fn enabled() -> bool {
    !state_dir().join("off").exists()
}

/// OpenGL plugins may use the graphics card (else the processor, if they can).
fn gpu_enabled() -> bool {
    !state_dir().join("gpu-off").exists()
}

/// Every folder searched, with where it comes from.
fn folders() -> Vec<(PathBuf, &'static str)> {
    let mut out: Vec<(PathBuf, &'static str)> = Vec::new();
    let env: Vec<PathBuf> = std::env::var_os("OFX_PLUGIN_PATH").map(|v| std::env::split_paths(&v).collect()).unwrap_or_default();
    for f in reelforge_ofx::standard_folders() {
        let place = if env.contains(&f) { "OFX_PLUGIN_PATH" } else { "the standard place" };
        out.push((f, place));
    }
    out.push((own_folder(), "your OpenFX folder"));
    for f in extra_folders() {
        if !out.iter().any(|(p, _)| *p == f) {
            out.push((f, "added by you"));
        }
    }
    out
}

static LAST: Mutex<(Vec<BundleReport>, bool)> = Mutex::new((Vec::new(), false));

/// Scan every folder, replacing the installed OpenFX plugins (none when
/// switched off). Returns a summary.
fn scan() -> String {
    LAST.lock().unwrap().1 = true;
    let reports = if enabled() {
        let st = state_dir();
        let _ = std::fs::create_dir_all(&st);
        let dirs: Vec<PathBuf> = folders().into_iter().map(|f| f.0).collect();
        ofxhost::scan(&dirs, Some(&st))
    } else {
        ofxhost::scan(&[], None)
    };
    let effects: usize = reports.iter().map(|r| r.plugins.len()).sum();
    let bad = reports.iter().filter(|r| r.plugins.is_empty() && (r.blocked || !r.errors.is_empty())).count();
    *LAST.lock().unwrap() = (reports, false);
    changed();
    if bad > 0 {
        format!("OpenFX: {effects} effect{} loaded; {bad} bundle{} didn't load (see Edit > Plugins…)", if effects == 1 { "" } else { "s" }, if bad == 1 { "" } else { "s" })
    } else {
        format!("OpenFX: {effects} effect{} loaded", if effects == 1 { "" } else { "s" })
    }
}

/// At startup: scan in the background (a big plugin set takes a while to load).
pub fn startup() {
    reelforge_ofx::set_gpu(gpu_enabled());
    if !enabled() {
        return;
    }
    LAST.lock().unwrap().1 = true;
    let _ = std::thread::Builder::new().name("openfx-scan".into()).spawn(|| log::info!("{}", scan()));
}

#[derive(Serialize)]
pub struct OfxView {
    pub enabled: bool,
    /// OpenGL plugins may use the graphics card.
    pub gpu: bool,
    /// What they'd run on, or why they can't ("" until it's been looked at).
    pub gpu_status: String,
    pub gpu_ok: bool,
    /// Each way of rendering on the graphics card: (name, what on or why not, works).
    pub gpus: Vec<(String, String, bool)>,
    pub scanning: bool,
    pub folders: Vec<OfxFolder>,
    pub bundles: Vec<OfxBundle>,
    /// Where "Open OpenFX folder" goes.
    pub own_folder: String,
}

#[derive(Serialize)]
pub struct OfxFolder {
    pub path: String,
    pub place: &'static str,
    pub exists: bool,
    pub removable: bool,
}

#[derive(Serialize)]
pub struct OfxBundle {
    pub path: String,
    pub name: String,
    pub effects: Vec<OfxEffect>,
    pub errors: Vec<String>,
    pub blocked: bool,
}

#[derive(Serialize)]
pub struct OfxEffect {
    pub name: String,
    pub group: String,
    pub version: String,
    pub params: usize,
    /// Reads other frames of the clip.
    pub temporal: bool,
    /// The ways it can render on the graphics card ("CUDA", "OpenCL", "OpenGL", "Metal").
    pub gpu: Vec<String>,
    /// It can't render on the processor.
    pub gpu_only: bool,
}

#[tauri::command]
pub fn ofx_list() -> OfxView {
    let (reports, scanning) = LAST.lock().unwrap().clone();
    let installed = reelforge_core::ofx::installed();
    let bundles = reports
        .iter()
        .map(|r| OfxBundle {
            path: r.path.display().to_string(),
            name: r.path.file_name().map_or_else(String::new, |n| n.to_string_lossy().trim_end_matches(".bundle").trim_end_matches(".ofx").to_string()),
            effects: installed
                .iter()
                .filter(|d| d.bundle == r.path)
                .map(|d| OfxEffect {
                    name: d.name.clone(),
                    group: d.group.clone(),
                    version: d.version.clone(),
                    params: d.params.len(),
                    temporal: d.temporal,
                    gpu: d.gpu.clone(),
                    gpu_only: !d.cpu,
                })
                .collect(),
            errors: r.errors.clone(),
            blocked: r.blocked,
        })
        .collect();
    // Only once a scan has loaded plugins (it makes the OpenGL context).
    let gpus = if reports.is_empty() {
        Vec::new()
    } else {
        reelforge_ofx::gpu_report().into_iter().map(|(g, r)| (g.label().to_string(), r.clone().unwrap_or_else(|e| e), r.is_ok())).collect()
    };
    let (gpu_ok, gpu_status) = if reports.is_empty() {
        (false, String::new())
    } else {
        match reelforge_ofx::gpu_status() {
            Ok(r) => (true, r),
            Err(e) => (false, e),
        }
    };
    OfxView {
        enabled: enabled(),
        gpu: gpu_enabled(),
        gpu_status,
        gpu_ok,
        gpus,
        scanning,
        folders: folders()
            .into_iter()
            .map(|(p, place)| OfxFolder { exists: p.is_dir(), removable: place == "added by you", path: p.display().to_string(), place })
            .collect(),
        bundles,
        own_folder: own_folder().display().to_string(),
    }
}

#[tauri::command]
pub fn ofx_rescan() -> String {
    let msg = scan();
    let _ = crate::ed(|e| e.status = msg.clone());
    msg
}

#[tauri::command]
pub fn ofx_enable(on: bool) -> String {
    let _ = std::fs::create_dir_all(state_dir());
    let flag = state_dir().join("off");
    if on {
        let _ = std::fs::remove_file(flag);
    } else {
        let _ = std::fs::write(flag, "OpenFX plugins switched off in Edit > Plugins\n");
    }
    ofx_rescan()
}

/// Add a folder to search (asks which).
#[tauri::command]
pub fn ofx_add_folder() -> Res<Option<String>> {
    let Some(dir) = rfd::FileDialog::new().set_title("A folder with OpenFX plugins (.ofx.bundle) in it").pick_folder() else { return Ok(None) };
    let mut list = extra_folders();
    if !list.contains(&dir) {
        list.push(dir);
        save_extra(&list)?;
    }
    Ok(Some(ofx_rescan()))
}

#[tauri::command]
pub fn ofx_remove_folder(path: String) -> Res<String> {
    let mut list = extra_folders();
    list.retain(|p| *p != PathBuf::from(&path));
    save_extra(&list)?;
    Ok(ofx_rescan())
}

/// Try a bundle that crashed Reelforge again (at the next scan, which this starts).
#[tauri::command]
pub fn ofx_unblock(path: String) -> String {
    ofxhost::unblock(&state_dir(), Path::new(&path));
    ofx_rescan()
}

#[tauri::command]
pub fn ofx_open_folder() -> Res<()> {
    let dir = own_folder();
    let _ = std::fs::create_dir_all(&dir);
    crate::fx::open_folder(dir.display().to_string())
}

/// Let OpenGL plugins use the graphics card, or keep them on the processor.
#[tauri::command]
pub fn ofx_gpu(on: bool) -> String {
    let _ = std::fs::create_dir_all(state_dir());
    let flag = state_dir().join("gpu-off");
    if on {
        let _ = std::fs::remove_file(flag);
    } else {
        let _ = std::fs::write(flag, "OpenGL for OpenFX plugins switched off in Edit > Plugins\n");
    }
    reelforge_ofx::set_gpu(on);
    changed();
    let msg = if on { "OpenFX plugins that can will render on the graphics card" } else { "OpenFX plugins will render on the processor (ones that only render on the graphics card stop)" };
    let _ = crate::ed(|e| e.status = msg.into());
    msg.into()
}
