//! Saved effect presets for the web interface: save a clip's effect stack
//! under a name (the Effects section's "Save as preset…"), and apply, rename
//! or delete presets from the effects browser's Presets tab.

use crate::{ed, Res};
use reelforge_core::ClipId;
use reelforge_engine::fx_presets::{EffectPreset, PresetLibrary, SaveOptions};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

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

#[derive(Serialize)]
pub struct PresetView {
    pub name: String,
    /// "Glow, Blur · 2 keyframes".
    pub summary: String,
    pub effects: usize,
    pub masks: usize,
    pub motion: bool,
    pub blend: Option<String>,
    /// "FromStart", "FromEnd" or "Stretch".
    pub timing: reelforge_engine::fx_presets::Timing,
    /// Plugins it uses that aren't installed now.
    pub missing: Vec<String>,
}

#[tauri::command]
pub fn fx_preset_list() -> Vec<PresetView> {
    with(|lib| {
        lib.list
            .iter()
            .map(|p| PresetView {
                name: p.name.clone(),
                summary: p.summary(),
                effects: p.effects.len(),
                masks: p.masks.len(),
                motion: p.motion.is_some(),
                blend: p.blend.map(|b| b.label().to_string()),
                timing: p.timing,
                missing: p.effects.iter().filter(|e| !e.plugin_installed()).map(|e| e.label()).collect(),
            })
            .collect()
    })
}

/// Save clip `id`'s look as preset `name` (replacing one of that name):
/// what `opts` chooses (all its effects and nothing else when `None`).
#[tauri::command]
pub fn fx_preset_save(id: u64, name: String, opts: Option<SaveOptions>) -> Res<String> {
    let opts = opts.unwrap_or_default();
    let (v, source_in, length, layer) = ed(|e| {
        e.commit_live_edit();
        e.seq().find_clip(ClipId(id)).map(|(_, c)| (c.visual(), c.source_in, c.duration, e.layer_size(c)))
    })?
    .ok_or("Select the clip whose effects to save first")?;
    let Some(p) = EffectPreset::from_visual(&name, &v, source_in, length, layer, &opts) else {
        return Err("Nothing to save: choose at least one effect, the masks, the motion or the blend mode".into());
    };
    if p.name.is_empty() {
        return Err("A preset needs a name".into());
    }
    let replaced = with(|lib| lib.get(&p.name).is_some());
    let msg = format!("{} the effect preset \"{}\" ({})", if replaced { "Replaced" } else { "Saved" }, p.name, p.summary());
    with(|lib| lib.put(p)).map_err(|e| format!("Couldn't save the preset: {e}"))?;
    let _ = ed(|e| e.status = msg.clone());
    Ok(msg)
}

/// Put preset `name`'s effects on clip `id`: after its own, or instead of them.
#[tauri::command]
pub fn fx_preset_apply(id: u64, name: String, replace: bool) -> Res<()> {
    let preset = with(|lib| lib.get(&name).cloned()).ok_or_else(|| format!("There's no preset called \"{name}\""))?;
    ed(|e| {
        e.commit_live_edit();
        let Some((scene, source_in, length, layer, locked)) = e.seq().find_clip(ClipId(id)).map(|(_, c)| (c.scene.is_some(), c.source_in, c.duration, e.layer_size(c), e.seq().is_locked(c.id))) else {
            e.status = "Select a video, picture or title clip first".into();
            return;
        };
        if scene {
            e.status = "Cameras, lights and points have no picture to put effects on".into();
            return;
        }
        if locked {
            e.status = "That clip's track is locked".into();
            return;
        }
        let Some(mut v) = e.visual(ClipId(id)) else { return };
        preset.apply(&mut v, source_in, length, layer, replace);
        e.set_visual_live(ClipId(id), v);
        e.commit_live_edit();
        e.status = format!("Applied the effect preset \"{}\"", preset.name);
    })
}

#[tauri::command]
pub fn fx_preset_rename(from: String, to: String) -> Res<()> {
    with(|lib| lib.rename(&from, &to)).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn fx_preset_delete(name: String) -> Res<()> {
    with(|lib| lib.remove(&name)).map_err(|e| e.to_string())?;
    let _ = ed(|e| e.status = format!("Deleted the effect preset \"{name}\""));
    Ok(())
}
