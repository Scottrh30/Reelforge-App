//! Saved presets for the web interface: save a clip's look (effects, masks,
//! motion, blend mode, title style, audio levels, transition in) under a
//! name, and apply, preview, file, star, rename, delete, export and import
//! presets from the effects browser's Presets tab and the Effects panel.

use crate::{ed, Res};
use reelforge_core::{ClipId, Time, TrackKind};
use reelforge_engine::fx_presets::{ClipParts, EffectPreset, PresetLibrary, SaveOptions, Timing, TransitionPreset};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use tauri::ipc::Response;

static FILE: OnceLock<PathBuf> = OnceLock::new();
static LIB: Mutex<Option<PresetLibrary>> = Mutex::new(None);

/// At startup: where the presets are kept (in the app's settings folder).
pub fn startup(config: &Path) {
    let _ = FILE.set(config.join("effect-presets.json"));
}

fn with<R>(f: impl FnOnce(&mut PresetLibrary) -> R) -> R {
    let mut g = LIB.lock().unwrap();
    let lib = g.get_or_insert_with(|| PresetLibrary::load(&FILE.get().cloned().unwrap_or_else(|| std::env::temp_dir().join("reelforge-effect-presets.json"))));
    f(lib)
}

fn find(name: &str) -> Res<EffectPreset> {
    with(|lib| lib.get(name)).ok_or_else(|| format!("There's no preset called \"{name}\""))
}

#[derive(Serialize)]
pub struct PresetView {
    pub name: String,
    /// "Glow, Blur + 1 mask · 2 keyframes".
    pub summary: String,
    pub effects: usize,
    pub masks: usize,
    pub motion: bool,
    /// Its blend mode's name, if it sets one.
    pub blend: Option<String>,
    pub timing: Timing,
    pub folder: String,
    pub favourite: bool,
    /// Comes with Reelforge (can't be renamed or deleted).
    pub builtin: bool,
    /// It has a picture (see `fx_preset_thumb`).
    pub thumb: bool,
    /// What it goes on: "look" (video clips), "title", "audio", "transition".
    pub parts: Vec<&'static str>,
    /// Plugins it uses that aren't installed now.
    pub missing: Vec<String>,
}

fn view(p: &EffectPreset) -> PresetView {
    let mut parts = Vec::new();
    if p.has_visual() {
        parts.push("look");
    }
    if p.title.is_some() {
        parts.push("title");
    }
    if p.audio.is_some() {
        parts.push("audio");
    }
    if p.transition.is_some() {
        parts.push("transition");
    }
    PresetView {
        name: p.name.clone(),
        summary: p.summary(),
        effects: p.effects.len(),
        masks: p.masks.len(),
        motion: p.motion.is_some(),
        blend: p.blend.map(|b| b.label().to_string()),
        timing: p.timing,
        folder: p.folder.clone(),
        favourite: p.favourite,
        builtin: p.builtin,
        thumb: p.thumb.is_some(),
        parts,
        missing: p.effects.iter().filter(|e| !e.plugin_installed()).map(|e| e.label()).collect(),
    }
}

#[tauri::command]
pub fn fx_preset_list() -> Vec<PresetView> {
    with(|lib| lib.all().iter().map(view).collect())
}

/// The folders your presets are filed in.
#[tauri::command]
pub fn fx_preset_folders() -> Vec<String> {
    with(|lib| lib.folders())
}

/// What a clip has besides its look: its title style, its audio (or the
/// audio linked to it), the transition into it.
#[derive(Serialize, Default)]
pub struct ClipExtras {
    pub title: bool,
    pub audio: bool,
    /// e.g. "Dissolve, 1.0 s".
    pub transition: Option<String>,
}

fn parts_of(e: &reelforge_engine::editor::Editor, id: ClipId) -> ClipParts {
    let s = e.seq();
    let Some((ti, c)) = s.find_clip(id) else { return ClipParts::default() };
    let audio = if s.tracks[ti].kind == TrackKind::Audio {
        Some(c.audio.clone())
    } else {
        reelforge_core::undo::expand_links(&e.project, e.active, &[id]).into_iter().filter_map(|a| s.find_clip(a)).find(|(t, _)| s.tracks[*t].kind == TrackKind::Audio).map(|(_, a)| a.audio.clone())
    };
    let transition = (s.tracks[ti].kind == TrackKind::Video && c.dissolve_in > Time::ZERO && e.project.clip_before(e.active, id).is_some()).then_some(TransitionPreset { kind: c.transition, length: c.dissolve_in });
    ClipParts { title: c.title.clone(), audio, transition }
}

#[tauri::command]
pub fn fx_preset_clip_extras(id: u64) -> Res<ClipExtras> {
    ed(|e| {
        let p = parts_of(e, ClipId(id));
        ClipExtras { title: p.title.is_some(), audio: p.audio.is_some(), transition: p.transition.map(|t| format!("{}, {:.1} s", t.kind.label(), t.length.as_secs_f64())) }
    })
}

/// A small picture of the Viewer as it is now, saved for preset `name`;
/// returns its file name.
fn save_picture(e: &reelforge_engine::editor::Editor, dir: &Path, name: &str) -> Option<String> {
    let (rgba, [w, h]) = e.preview.read_output()?;
    if w == 0 || h == 0 {
        return None;
    }
    let k = (w / 240).max(1);
    let (rgba, w, h) = if k > 1 { crate::shrink(&rgba, w, h, k) } else { (rgba, w, h) };
    let mut jpg = Vec::new();
    jpeg_encoder::Encoder::new(&mut jpg, 80).encode(&rgba, w as u16, h as u16, jpeg_encoder::ColorType::Rgba).ok()?;
    let clean: String = name.chars().map(|c| if c.is_alphanumeric() { c } else { '-' }).collect();
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis());
    let file = format!("{}-{stamp}.jpg", clean.chars().take(40).collect::<String>());
    let _ = std::fs::create_dir_all(dir);
    reelforge_engine::safety::write_atomic(&dir.join(&file), &jpg).ok()?;
    Some(file)
}

/// Save clip `id`'s look as preset `name` (replacing one of that name):
/// what `opts` chooses (all its effects and nothing else when `None`), with
/// a picture of the Viewer.
#[tauri::command]
pub fn fx_preset_save(id: u64, name: String, opts: Option<SaveOptions>) -> Res<String> {
    let opts = opts.unwrap_or_default();
    let pictures = with(|lib| lib.pictures());
    let (v, parts, source_in, length, layer, thumb) = ed(|e| {
        e.commit_live_edit();
        let id = ClipId(id);
        let parts = parts_of(e, id);
        let thumb = save_picture(e, &pictures, &name);
        e.seq().find_clip(id).map(|(_, c)| (c.visual(), parts, c.source_in, c.duration, e.layer_size(c), thumb))
    })?
    .ok_or("Select the clip to save from first")?;
    let Some(mut p) = EffectPreset::from_clip_parts(&name, &v, &parts, source_in, length, layer, &opts) else {
        if let Some(t) = thumb {
            let _ = std::fs::remove_file(pictures.join(t));
        }
        return Err("Nothing to save: choose at least one thing to go in the preset".into());
    };
    if p.name.is_empty() {
        return Err("A preset needs a name".into());
    }
    p.thumb = thumb;
    let replaced = with(|lib| lib.list.iter().any(|q| q.name.eq_ignore_ascii_case(&p.name)));
    let msg = format!("{} the preset \"{}\" ({})", if replaced { "Replaced" } else { "Saved" }, p.name, p.summary());
    if let Err(err) = with(|lib| lib.put(p.clone())) {
        if let Some(t) = &p.thumb {
            let _ = std::fs::remove_file(pictures.join(t));
        }
        return Err(format!("Couldn't save the preset: {err}"));
    }
    let _ = ed(|e| e.status = msg.clone());
    Ok(msg)
}

/// Put preset `name` on clip `id` (and, when `id` is one of several selected
/// clips, on all of them): after their own effects, or instead of them.
#[tauri::command]
pub fn fx_preset_apply(id: u64, name: String, replace: bool) -> Res<usize> {
    let preset = find(&name)?;
    ed(|e| {
        e.end_preset_preview();
        let id = ClipId(id);
        let sel = e.selected_ids();
        let ids = if sel.len() > 1 && sel.contains(&id) { sel } else { vec![id] };
        e.apply_preset(&ids, &preset, replace)
    })
}

/// Show preset `name` on clip `id` while it's hovered (nothing changes for good).
#[tauri::command]
pub fn fx_preset_preview(id: u64, name: String) -> Res<()> {
    let preset = find(&name)?;
    ed(|e| {
        e.end_preset_preview();
        e.preview_preset(ClipId(id), &preset);
    })
}

#[tauri::command]
pub fn fx_preset_preview_end() -> Res<()> {
    ed(|e| e.end_preset_preview())
}

#[tauri::command]
pub fn fx_preset_thumb(name: String) -> Response {
    let bytes = with(|lib| lib.get(&name).and_then(|p| p.thumb).and_then(|t| std::fs::read(lib.pictures().join(t)).ok())).unwrap_or_default();
    Response::new(bytes)
}

#[tauri::command]
pub fn fx_preset_favourite(name: String, on: bool) -> Res<()> {
    with(|lib| lib.set_favourite(&name, on)).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn fx_preset_folder(name: String, folder: String) -> Res<()> {
    with(|lib| lib.set_folder(&name, &folder)).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn fx_preset_rename(from: String, to: String) -> Res<()> {
    with(|lib| lib.rename(&from, &to)).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn fx_preset_delete(name: String) -> Res<()> {
    with(|lib| lib.remove(&name)).map_err(|e| e.to_string())?;
    let _ = ed(|e| e.status = format!("Deleted the preset \"{name}\""));
    Ok(())
}

/// Save presets `names` to a .rfpreset file (asks where).
#[tauri::command]
pub fn fx_preset_export(names: Vec<String>) -> Res<Option<String>> {
    if names.is_empty() {
        return Err("Choose the presets to export".into());
    }
    let suggested = if names.len() == 1 { format!("{}.rfpreset", names[0]) } else { "Reelforge presets.rfpreset".into() };
    let Some(path) = rfd::FileDialog::new().set_title("Export presets").set_file_name(&suggested).add_filter("Reelforge presets", &["rfpreset"]).save_file() else { return Ok(None) };
    let text = with(|lib| lib.export(&names));
    std::fs::write(&path, text).map_err(|e| format!("Couldn't write {}: {e}", path.display()))?;
    let msg = format!("Exported {} preset{} to {}", names.len(), if names.len() == 1 { "" } else { "s" }, path.display());
    let _ = ed(|e| e.status = msg.clone());
    Ok(Some(msg))
}

/// Add the presets in .rfpreset files (asks which).
#[tauri::command]
pub fn fx_preset_import() -> Res<Option<String>> {
    let Some(files) = rfd::FileDialog::new().set_title("Import presets").add_filter("Reelforge presets", &["rfpreset", "json"]).pick_files() else { return Ok(None) };
    let mut added = Vec::new();
    for f in files {
        let text = std::fs::read_to_string(&f).map_err(|e| format!("Couldn't read {}: {e}", f.display()))?;
        added.extend(with(|lib| lib.import(&text)).map_err(|e| format!("{}: {e}", f.display()))?);
    }
    let msg = match added.len() {
        0 => "No presets in that file".to_string(),
        1 => format!("Imported the preset \"{}\"", added[0]),
        n => format!("Imported {n} presets"),
    };
    let _ = ed(|e| e.status = msg.clone());
    Ok(Some(msg))
}
