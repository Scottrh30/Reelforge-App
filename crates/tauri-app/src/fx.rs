//! Effects and effect plugins for the web interface: the selected clip's
//! effect stack (add, reorder, switch off, edit parameters, keyframe them),
//! the effects browser's catalogue, and the Plugins window (install, remove,
//! reload).

use crate::{ed, Res};
use reelforge_core::lut::Curves;
use reelforge_core::{Clip, ClipId, Effect, EffectCategory, EffectData, EffectKind, ParamUi, Time};
use reelforge_engine::editor::Editor;
use reelforge_engine::plugins::{user_dir, PluginManager, Source};
use serde::Serialize;
use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

thread_local! {
    /// The plugin manager (on the main thread, like the editor).
    static PLUGINS: RefCell<PluginManager> = RefCell::new(PluginManager::default());
}

/// Bumped whenever the installed plugins change, so the interface fetches the
/// state again (an effect whose plugin was missing may now work, and the other way round).
pub static PLUGINS_REV: AtomicU64 = AtomicU64::new(1);

fn plugins<R>(f: impl FnOnce(&mut PluginManager) -> R) -> R {
    PLUGINS.with(|p| f(&mut p.borrow_mut()))
}

fn changed() {
    PLUGINS_REV.fetch_add(1, Ordering::Relaxed);
}

/// Load the installed plugins (at startup).
pub fn startup(e: &mut Editor) {
    let msg = plugins(|p| p.scan());
    log::info!("{msg}");
    if plugins(|p| p.entries.iter().any(|x| x.result.is_err())) {
        e.status = msg;
    }
}

/// Called every tick: reloads plugins whose files changed (live reload).
pub fn poll(e: &mut Editor) {
    if let Some(msg) = plugins(|p| p.poll()) {
        // The viewer re-renders by itself: a plugin's revision is part of a frame's signature.
        e.status = msg;
        changed();
    }
}

// ---- The selected clip's effects ---------------------------------------------------

#[derive(Serialize)]
pub struct EffectView {
    pub label: String,
    pub kind: EffectKind,
    pub enabled: bool,
    /// The plugin's id, for a plugin effect.
    pub plugin: Option<String>,
    /// A plugin effect whose plugin isn't installed (skipped; its settings are kept).
    pub missing: bool,
    pub params: Vec<ParamView>,
    /// Tone curves (Curves effect).
    pub curves: Option<Curves>,
    /// The .cube file (LUT effect), and what's wrong with it, if anything.
    pub lut: Option<String>,
    pub lut_error: Option<String>,
    pub animated: bool,
}

#[derive(Serialize)]
pub struct ParamView {
    pub name: &'static str,
    /// The value at the playhead.
    pub value: f32,
    pub default: f32,
    pub min: f32,
    pub max: f32,
    pub suffix: &'static str,
    /// Change per pixel dragged.
    pub step: f32,
    pub decimals: u8,
    /// "number", "toggle", "choice" or "color" (`value` is 0xRRGGBB).
    pub ui: &'static str,
    /// A heading to show above this parameter.
    pub section: Option<&'static str>,
    pub options: Vec<&'static str>,
    pub animated: bool,
    /// There's a keyframe at the playhead.
    pub key_here: bool,
}

/// The clip's source time at the playhead (held at the clip's ends), and
/// whether the playhead is on the clip (keyframes can only be set there).
fn effect_time(e: &Editor, c: &Clip) -> (Time, bool) {
    let ph = e.playback.playhead;
    (c.source_time(ph.max(c.timeline_start).min(c.timeline_end())), c.contains(ph))
}

pub fn views(e: &Editor, c: &Clip) -> Vec<EffectView> {
    let (src, in_clip) = effect_time(e, c);
    c.effects
        .iter()
        .map(|fx| {
            let mut fx = fx.clone();
            fx.normalize();
            let params = fx
                .param_specs()
                .iter()
                .enumerate()
                .map(|(i, s)| {
                    let keys = &fx.params[i].keys;
                    let (ui, options) = match s.ui {
                        ParamUi::Number => ("number", Vec::new()),
                        ParamUi::Toggle => ("toggle", Vec::new()),
                        ParamUi::Choice(o) => ("choice", o.to_vec()),
                        ParamUi::Color => ("color", Vec::new()),
                    };
                    let section = fx.kind.sections().iter().find(|(at, _)| *at == i).map(|(_, h)| *h);
                    ParamView {
                        name: s.name,
                        value: fx.value_at(i, src),
                        default: s.default,
                        min: s.min,
                        max: s.max,
                        suffix: s.suffix,
                        step: s.speed,
                        decimals: if s.speed < 0.1 { 2 } else { 1 },
                        ui,
                        section,
                        options,
                        animated: keys.is_animated(),
                        key_here: in_clip && keys.index_at(src).is_some(),
                    }
                })
                .collect();
            let (curves, lut) = match (&fx.data, fx.kind) {
                (Some(EffectData::Curves(c)), _) => (Some(c.clone()), None),
                (_, EffectKind::Curves) => (Some(Curves::default()), None),
                (Some(EffectData::Lut(p)), _) => (None, Some(p.clone())),
                _ => (None, None),
            };
            let lut_error = lut.as_ref().and_then(|p| reelforge_core::lut::load_cube(p).err());
            EffectView {
                label: fx.label(),
                kind: fx.kind,
                enabled: fx.enabled,
                plugin: fx.plugin.as_ref().map(|p| p.id.clone()),
                missing: fx.kind == EffectKind::Plugin && !fx.plugin_installed(),
                animated: fx.is_animated(),
                params,
                curves,
                lut: lut.map(|p| p.display().to_string()),
                lut_error,
            }
        })
        .collect()
}

/// Change clip `id`'s effects with `f` (given the source time at the playhead
/// and whether it's on the clip) as a live edit. Returns what `f` returned.
fn edit<R>(e: &mut Editor, id: u64, f: impl FnOnce(&mut Vec<Effect>, Time, bool) -> R) -> Option<R> {
    let id = ClipId(id);
    let (_, c) = e.seq().find_clip(id)?;
    let (src, in_clip) = effect_time(e, c);
    let mut v = e.visual(id)?;
    let r = f(&mut v.effects, src, in_clip);
    e.set_visual_live(id, v);
    Some(r)
}

/// Add an effect (built-in `kind`, or the installed plugin `plugin`) to the
/// end of the clip's stack: one undo step.
#[tauri::command]
pub fn effect_add(id: u64, kind: EffectKind, plugin: Option<String>) -> Res<()> {
    let fx = match plugin {
        Some(pid) => Effect::new_plugin(&*reelforge_core::plugin::find(&pid).ok_or_else(|| format!("The plugin \"{pid}\" isn't installed"))?),
        None if kind == EffectKind::Plugin => return Err("no plugin given".into()),
        None => Effect::new(kind),
    };
    ed(|e| {
        e.commit_live_edit();
        let name = fx.label();
        if e.seq().find_clip(ClipId(id)).is_none() {
            e.status = "Select a video, picture or title clip first".into();
            return;
        }
        edit(e, id, |list, _, _| list.push(fx));
        e.commit_live_edit();
        e.status = format!("Added {name}");
    })
}

/// One-step changes to effect `index`: "up", "down", "remove", "toggle",
/// "duplicate" or "reset" (defaults, no keyframes).
#[tauri::command]
pub fn effect_op(id: u64, index: usize, op: String) -> Res<()> {
    ed(|e| {
        e.commit_live_edit();
        edit(e, id, |list, _, _| {
            if index >= list.len() {
                return;
            }
            match op.as_str() {
                "up" if index > 0 => list.swap(index, index - 1),
                "down" if index + 1 < list.len() => list.swap(index, index + 1),
                "remove" => {
                    list.remove(index);
                }
                "toggle" => list[index].enabled = !list[index].enabled,
                "duplicate" => {
                    let copy = list[index].clone();
                    list.insert(index + 1, copy);
                }
                "reset" => {
                    let fx = &mut list[index];
                    if fx.kind != EffectKind::Plugin || fx.plugin_installed() {
                        let fresh = match fx.plugin_def() {
                            Some(d) => Effect::new_plugin(&d),
                            None => Effect::new(fx.kind),
                        };
                        let data = if fx.kind == EffectKind::Lut { fx.data.take() } else { fresh.data.clone() };
                        *fx = Effect { enabled: fx.enabled, data, ..fresh };
                    }
                }
                _ => {}
            }
        });
        e.commit_live_edit();
    })
}

/// Set parameter `param` of effect `index` (live: a drag or typing; `commit`
/// ends it). On an animated parameter it sets the keyframe at the playhead.
#[tauri::command]
pub fn effect_param(id: u64, index: usize, param: usize, value: f32) -> Res<()> {
    ed(|e| {
        edit(e, id, |list, src, in_clip| {
            let Some(fx) = list.get_mut(index) else { return };
            fx.normalize();
            let Some(s) = fx.spec(param) else { return };
            let v = value.clamp(s.min, s.max);
            let p = &mut fx.params[param];
            if p.keys.is_animated() {
                if in_clip {
                    p.keys.set(src, v);
                }
            } else {
                p.value = v;
            }
        });
    })
}

/// Set energy blade `index`'s colours to ready-made preset `preset`
/// (see [`reelforge_core::effects::blade::PRESETS`]): one undo step. Animated
/// colours get a key at the playhead.
#[tauri::command]
pub fn blade_preset(id: u64, index: usize, preset: usize) -> Res<()> {
    use reelforge_core::effects::blade;
    let Some(&(name, glow, core)) = blade::PRESETS.get(preset) else { return Err("no such colour".into()) };
    ed(|e| {
        e.commit_live_edit();
        edit(e, id, |list, src, in_clip| {
            let Some(fx) = list.get_mut(index).filter(|f| f.kind == EffectKind::EnergyBlade) else { return };
            fx.normalize();
            for (i, v) in [(blade::BLADE_COLOR, glow), (blade::CORE_COLOR, core)] {
                let p = &mut fx.params[i];
                if p.keys.is_animated() {
                    if in_clip {
                        p.keys.set(src, v as f32);
                    }
                } else {
                    p.value = v as f32;
                }
            }
        });
        e.commit_live_edit();
        e.status = format!("{name} blade");
    })
}

/// Keyframe the blade's Length so it switches on (0 → 100 %) or off
/// (`retract`, 100 → 0 %) over `seconds` from the playhead: one undo step.
#[tauri::command]
pub fn blade_ignite(id: u64, index: usize, retract: bool, seconds: f32) -> Res<()> {
    use reelforge_core::effects::blade;
    ed(|e| {
        e.commit_live_edit();
        let r = edit(e, id, |list, src, in_clip| {
            let Some(fx) = list.get_mut(index).filter(|f| f.kind == EffectKind::EnergyBlade) else { return Ok(()) };
            if !in_clip {
                return Err("Move the playhead onto the clip first");
            }
            fx.normalize();
            let end = src + Time::from_secs_f64(seconds.clamp(0.02, 10.0) as f64);
            let (a, b) = if retract { (100.0, 0.0) } else { (0.0, 100.0) };
            let p = &mut fx.params[blade::LENGTH];
            p.keys.0.retain(|k| k.at < src || k.at > end);
            p.keys.set(src, a);
            p.keys.set(end, b);
            // Fast at first, settling at the end: the classic snap-out.
            if let Some(k) = p.keys.0.iter_mut().find(|k| k.at == src) {
                k.interp = reelforge_core::anim::Interp::Ease;
            }
            Ok(())
        });
        match r {
            Some(Err(m)) => e.status = m.into(),
            _ => {
                e.commit_live_edit();
                e.status = if retract { "The blade retracts from the playhead".into() } else { "The blade ignites from the playhead".into() };
            }
        }
    })
}

/// Keyframe button of a parameter: removes the key at the playhead if there
/// is one, otherwise adds one there (which starts animating it). One step.
#[tauri::command]
pub fn effect_key(id: u64, index: usize, param: usize) -> Res<()> {
    ed(|e| {
        e.commit_live_edit();
        let r = edit(e, id, |list, src, in_clip| {
            let Some(fx) = list.get_mut(index) else { return Ok(()) };
            if !in_clip {
                return Err("Move the playhead onto the clip to set a keyframe");
            }
            fx.normalize();
            if fx.spec(param).is_none() {
                return Ok(());
            }
            let shown = fx.value_at(param, src);
            let p = &mut fx.params[param];
            if p.keys.index_at(src).is_some() {
                if p.keys.0.len() == 1 {
                    p.value = shown; // the last key gone: keep the value it had
                }
                p.keys.remove_at(src);
            } else {
                p.keys.set(src, shown);
            }
            Ok(())
        });
        if let Some(Err(msg)) = r {
            e.status = msg.into();
        }
        e.commit_live_edit();
    })
}

/// The Curves effect's points (live while dragging; `commit` ends it).
#[tauri::command]
pub fn effect_curves(id: u64, index: usize, curves: Curves) -> Res<()> {
    let clean = |mut pts: Vec<[f32; 2]>| {
        for p in &mut pts {
            *p = [p[0].clamp(0.0, 1.0), p[1].clamp(0.0, 1.0)];
        }
        pts.sort_by(|a, b| a[0].total_cmp(&b[0]));
        if pts.len() < 2 {
            pts = vec![[0.0, 0.0], [1.0, 1.0]];
        }
        pts
    };
    let curves = Curves { master: clean(curves.master), red: clean(curves.red), green: clean(curves.green), blue: clean(curves.blue) };
    ed(|e| {
        edit(e, id, |list, _, _| {
            if let Some(fx) = list.get_mut(index).filter(|f| f.kind == EffectKind::Curves) {
                fx.data = Some(EffectData::Curves(curves));
            }
        });
    })
}

/// Choose the LUT effect's .cube file.
#[tauri::command]
pub fn effect_lut(id: u64, index: usize) -> Res<()> {
    let Some(path) = rfd::FileDialog::new().set_title("3D LUT (.cube)").add_filter("3D LUT", &["cube", "CUBE"]).pick_file() else { return Ok(()) };
    ed(|e| {
        e.commit_live_edit();
        let err = reelforge_core::lut::load_cube(&path).err();
        edit(e, id, |list, _, _| {
            if let Some(fx) = list.get_mut(index).filter(|f| f.kind == EffectKind::Lut) {
                fx.data = Some(EffectData::Lut(path.clone()));
            }
        });
        e.commit_live_edit();
        e.status = match err {
            Some(msg) => format!("That LUT can't be used: {msg}"),
            None => format!("LUT: {}", path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()),
        };
    })
}

// ---- The effects browser -----------------------------------------------------------

#[derive(Serialize)]
pub struct CatalogGroup {
    pub label: &'static str,
    pub items: Vec<CatalogItem>,
}

#[derive(Serialize)]
pub struct CatalogItem {
    pub kind: EffectKind,
    /// The plugin's id, for a plugin.
    pub plugin: Option<String>,
    pub label: String,
    pub description: String,
    pub author: String,
    pub version: String,
}

/// Every effect that can be added, built-in and plugins, by category.
#[tauri::command]
pub fn effect_catalog() -> Vec<CatalogGroup> {
    let installed = reelforge_core::plugin::installed();
    EffectCategory::ALL
        .into_iter()
        .map(|cat| {
            let mut items: Vec<CatalogItem> = EffectKind::ALL
                .into_iter()
                .filter(|k| k.category() == cat)
                .map(|k| CatalogItem { kind: k, plugin: None, label: k.label().into(), description: k.description().into(), author: String::new(), version: String::new() })
                .collect();
            items.extend(installed.iter().filter(|p| p.category == cat).map(|p| CatalogItem {
                kind: EffectKind::Plugin,
                plugin: Some(p.id.clone()),
                label: p.name.clone(),
                description: p.description.clone(),
                author: p.author.clone(),
                version: p.version.clone(),
            }));
            CatalogGroup { label: cat.label(), items }
        })
        .filter(|g| !g.items.is_empty())
        .collect()
}

// ---- The Plugins window --------------------------------------------------------------

#[derive(Serialize)]
pub struct PluginsView {
    pub entries: Vec<PluginView>,
    pub live_reload: bool,
    /// Where Install puts plugins.
    pub folder: String,
}

#[derive(Serialize)]
pub struct PluginView {
    pub folder: String,
    /// "your plugin folder", "next to Reelforge" or "REELFORGE_PLUGINS".
    pub place: &'static str,
    pub ok: bool,
    pub name: String,
    pub id: String,
    pub version: String,
    pub author: String,
    pub category: &'static str,
    pub description: String,
    pub params: usize,
    pub passes: usize,
    /// Why it didn't load.
    pub error: Option<String>,
    pub removable: bool,
}

#[tauri::command]
pub fn plugins_list() -> PluginsView {
    plugins(|m| PluginsView {
        entries: m
            .entries
            .iter()
            .map(|e| {
                let place = match e.source {
                    Source::User => "your plugin folder",
                    Source::Bundled => "next to Reelforge",
                    Source::Extra => "REELFORGE_PLUGINS",
                };
                let base = PluginView {
                    folder: e.folder.display().to_string(),
                    place,
                    ok: e.result.is_ok(),
                    name: e.title(),
                    id: String::new(),
                    version: String::new(),
                    author: String::new(),
                    category: "",
                    description: String::new(),
                    params: 0,
                    passes: 0,
                    error: e.result.as_ref().err().cloned(),
                    removable: m.removable(e),
                };
                match &e.result {
                    Ok(d) => PluginView {
                        id: d.id.clone(),
                        version: d.version.clone(),
                        author: d.author.clone(),
                        category: d.category.label(),
                        description: d.description.clone(),
                        params: d.params.len(),
                        passes: d.passes.len(),
                        ..base
                    },
                    Err(_) => base,
                }
            })
            .collect(),
        live_reload: m.live_reload,
        folder: user_dir().display().to_string(),
    })
}

/// Install a plugin from a folder (`zip` false) or a .zip file, asking which.
/// Returns what happened (`None` if nothing was chosen).
#[tauri::command]
pub fn plugins_install(zip: bool) -> Res<Option<String>> {
    let picked = if zip {
        rfd::FileDialog::new().set_title("Install a plugin (.zip)").add_filter("Plugin zip", &["zip"]).pick_file()
    } else {
        rfd::FileDialog::new().set_title("Install a plugin: its folder (with plugin.toml)").pick_folder()
    };
    let Some(path) = picked else { return Ok(None) };
    let r = plugins(|m| if zip { m.install_zip(&path) } else { m.install_folder(&path) }).map_err(|e| format!("Couldn't install: {e}"));
    changed();
    tell(r.map(Some))
}

/// Make a starter plugin in the user plugin folder and show it.
#[tauri::command]
pub fn plugins_new() -> Res<String> {
    let dir = plugins(|m| m.new_from_template()).map_err(|e| format!("Couldn't make a plugin: {e}"))?;
    changed();
    let _ = open_dir(&dir);
    tell(Ok(format!("Made a new plugin in {}: edit effect.wgsl and plugin.toml; changes load as you save", dir.display())))
}

#[tauri::command]
pub fn plugins_remove(folder: String) -> Res<String> {
    let r = plugins(|m| m.remove(&PathBuf::from(folder))).map_err(|e| format!("Couldn't remove: {e}"));
    changed();
    tell(r)
}

#[tauri::command]
pub fn plugins_reload() -> String {
    let msg = plugins(|m| m.scan());
    changed();
    let _ = ed(|e| e.status = msg.clone());
    msg
}

/// Put what happened in the status bar too (so an older message doesn't linger there).
fn tell<T: Clone + Into<Option<String>>>(r: Res<T>) -> Res<T> {
    let msg = match &r {
        Ok(m) => m.clone().into(),
        Err(e) => Some(e.clone()),
    };
    if let Some(msg) = msg {
        let _ = ed(|e| e.status = msg);
    }
    r
}

#[tauri::command]
pub fn plugins_live(on: bool) {
    plugins(|m| m.live_reload = on);
}

/// Open a folder (making it first if it's the plugin folder) in the file manager.
#[tauri::command]
pub fn open_folder(path: String) -> Res<()> {
    let dir = PathBuf::from(path);
    if dir == user_dir() {
        let _ = std::fs::create_dir_all(&dir);
    }
    open_dir(&dir)
}

fn open_dir(dir: &std::path::Path) -> Res<()> {
    #[cfg(windows)]
    let r = std::process::Command::new("explorer").arg(dir).spawn();
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("open").arg(dir).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let r = std::process::Command::new("xdg-open").arg(dir).spawn();
    r.map(|_| ()).map_err(|e| e.to_string())
}
