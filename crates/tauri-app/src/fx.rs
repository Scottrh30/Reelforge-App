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

pub(crate) fn changed() {
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
    /// The Hue curves effect's curves.
    pub hue_curves: Option<reelforge_core::lut::HueCurves>,
    /// The .cube file (LUT effect), and what's wrong with it, if anything.
    pub lut: Option<String>,
    pub lut_error: Option<String>,
    /// A difference key's clean plate: (file, where in it), or why it can't be read.
    pub plate: Option<(String, String)>,
    pub plate_error: Option<String>,
    /// An environment light's picture, and why it can't be read, if it can't.
    pub environment: Option<String>,
    pub environment_error: Option<String>,
    /// Object removal: how many frames have been filled (`None` if not yet,
    /// or the fill is missing).
    pub filled: Option<usize>,
    /// …only the one frame, as a preview.
    pub fill_preview: bool,
    /// Paint: how many strokes.
    pub strokes: usize,
    /// Face beauty: the faces found (`None`: not looked for yet): whether
    /// each is on, the frames it's in, and where it is first.
    pub faces: Option<Vec<FaceView>>,
    /// Sky replacement: the new sky's picture, and how many frames the sky
    /// has been found in (`None`: not yet).
    pub sky_image: Option<String>,
    pub sky_frames: Option<usize>,
    pub animated: bool,
    /// Positions to drag in the Viewer (an OpenFX plugin's): the first of
    /// each one's two parameters (x, y in % of the layer, y down) and its name.
    pub points: Vec<PointView>,
    /// Why it isn't rendering (an OpenFX plugin's error), if it isn't.
    pub error: Option<String>,
    /// An OpenFX plugin that reads other frames of the clip.
    pub temporal: bool,
    /// The ways an OpenFX plugin can render on the graphics card ("CUDA", "OpenCL"…).
    pub gpu: Vec<String>,
    /// What it rendered with last ("OpenCL on …", "the processor").
    pub rendered_with: Option<String>,
}

#[derive(Serialize)]
pub struct PointView {
    pub param: usize,
    pub label: String,
    /// Measured from another point (its first parameter), not from the
    /// layer's corner: a patch's source, which moves with its target.
    pub relative: Option<usize>,
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
    /// Its keyframes' timeline times.
    pub keys: Vec<i64>,
}

/// An effect's settings as seen at source time `src` (`fx` normalised).
pub fn param_views(fx: &reelforge_core::effects::Effect, c: &Clip, src: Time, in_clip: bool) -> Vec<ParamView> {
    fx.param_specs()
        .iter()
        .enumerate()
        .filter_map(|(i, s)| {
            let keys = &fx.params.get(i)?.keys;
            let (ui, options) = match s.ui {
                ParamUi::Number => ("number", Vec::new()),
                ParamUi::Toggle => ("toggle", Vec::new()),
                ParamUi::Choice(o) => ("choice", o.to_vec()),
                ParamUi::Color => ("color", Vec::new()),
            };
            let section = fx.kind.sections().iter().find(|(at, _)| *at == i).map(|(_, h)| *h);
            Some(ParamView {
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
                keys: keys.0.iter().map(|k| (c.timeline_start + (k.at - c.source_in)).0).collect(),
            })
        })
        .collect()
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
        .enumerate()
        .map(|(index, fx)| {
            let ofx = fx.ofx_def();
            let mut fx = fx.clone();
            fx.normalize();
            let params = param_views(&fx, c, src, in_clip);
            let (curves, lut) = match (&fx.data, fx.kind) {
                (Some(EffectData::Curves(c)), _) => (Some(c.clone()), None),
                (_, EffectKind::Curves) => (Some(Curves::default()), None),
                (Some(EffectData::Lut(p)), _) => (None, Some(p.clone())),
                _ => (None, None),
            };
            let lut_error = lut.as_ref().and_then(|p| reelforge_core::lut::load_cube(p).err());
            let rate = e.seq().frame_rate;
            let (plate, plate_error) = match &fx.data {
                Some(EffectData::Plate { path, at }) => (
                    Some((path.display().to_string(), at.timecode(rate))),
                    if !path.exists() {
                        Some("The clean plate's file is missing: choose it again".to_string())
                    } else if reelforge_core::lut::load_plate(path, *at).is_none() {
                        Some("The clean plate couldn't be read from that file".to_string())
                    } else {
                        None
                    },
                ),
                _ => (None, None),
            };
            let (environment, environment_error) = match &fx.data {
                Some(EffectData::Environment(path)) => (
                    Some(path.display().to_string()),
                    if !path.exists() {
                        Some("The environment picture is missing: choose it again".to_string())
                    } else if reelforge_core::environment::load(path).is_none() {
                        Some("That picture couldn't be read (use an .hdr, .exr, .png or .jpg panorama)".to_string())
                    } else {
                        None
                    },
                ),
                _ => (None, None),
            };
            EffectView {
                label: fx.label(),
                kind: fx.kind,
                enabled: fx.enabled,
                plugin: fx.plugin.as_ref().map(|p| p.id.clone()),
                missing: fx.plugin_missing(),
                animated: fx.is_animated(),
                params,
                curves,
                hue_curves: match (&fx.data, fx.kind) {
                    (Some(EffectData::HueCurves(c)), _) => Some(c.clone()),
                    (_, EffectKind::HueCurves) => Some(Default::default()),
                    _ => None,
                },
                lut: lut.map(|p| p.display().to_string()),
                lut_error,
                plate,
                plate_error,
                environment,
                environment_error,
                strokes: match &fx.data {
                    Some(EffectData::Paint(s)) => s.len(),
                    _ => 0,
                },
                faces: match &fx.data {
                    Some(EffectData::Faces(f)) => Some(f.iter().map(|t| FaceView { on: t.on, frames: t.keys.len(), at: t.keys.first().map_or(0, |k| k.at.0), rect: t.keys.first().map_or([0.0; 4], |k| k.rect) }).collect()),
                    _ => None,
                },
                sky_image: match &fx.data {
                    Some(EffectData::Sky { image: Some(p), .. }) => Some(p.display().to_string()),
                    _ => None,
                },
                sky_frames: match &fx.data {
                    Some(EffectData::Sky { matte: Some(d), .. }) if reelforge_core::fillstore::exists(d) => Some(reelforge_core::fillstore::frame_count(d)),
                    _ => None,
                },
                fill_preview: matches!(&fx.data, Some(EffectData::Fill(dir)) if reelforge_core::fillstore::is_preview(dir)),
                filled: match &fx.data {
                    Some(EffectData::Fill(dir)) if reelforge_core::fillstore::exists(dir) => Some(reelforge_core::fillstore::frame_count(dir)),
                    _ => None,
                },
                points: if matches!(fx.kind, EffectKind::WireRemoval | EffectKind::Lightning) {
                    vec![PointView { param: 0, label: "Start".into(), relative: None }, PointView { param: 2, label: "End".into(), relative: None }]
                } else if fx.kind == EffectKind::PatchReplacer {
                    vec![PointView { param: 0, label: "Target".into(), relative: None }, PointView { param: 2, label: "Source".into(), relative: Some(0) }]
                } else if let Some(label) = match fx.kind {
                    EffectKind::Shatter => Some("Impact"),
                    EffectKind::MuzzleFlash => Some("Barrel"),
                    EffectKind::BulletHit => Some("Hit"),
                    EffectKind::Fluid => Some("Source"),
                    EffectKind::LightRays => Some("Source"),
                    EffectKind::Bulge | EffectKind::Twirl | EffectKind::Ripple | EffectKind::Mirror | EffectKind::LensDistortion => Some("Centre"),
                    EffectKind::LensDirt if fx.params.get(reelforge_core::effects::dirt::LIT_BY).is_some_and(|p| p.value >= 0.5) => Some("Light"),
                    _ => None,
                } {
                    vec![PointView { param: 0, label: label.into(), relative: None }]
                } else {
                    ofx.as_ref().map_or_else(Vec::new, |d| d.points().into_iter().map(|(param, label)| PointView { param, label, relative: None }).collect())
                },
                error: ofx.as_ref().and_then(|_| reelforge_export::ofxhost::error(c.id, index)).filter(|_| fx.enabled),
                temporal: ofx.as_ref().is_some_and(|d| d.temporal),
                gpu: ofx.as_ref().map_or_else(Vec::new, |d| d.gpu.clone()),
                rendered_with: ofx.as_ref().and_then(|_| reelforge_export::ofxhost::rendered_with(c.id, index)),
            }
        })
        .collect()
}

/// Change clip `id`'s effects with `f` (given the source time at the playhead
/// and whether it's on the clip) as a live edit. Returns what `f` returned.
pub(crate) fn edit<R>(e: &mut Editor, id: u64, f: impl FnOnce(&mut Vec<Effect>, Time, bool) -> R) -> Option<R> {
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
    // As favourites name it: the kind, or the plugin.
    let key = match &plugin {
        Some(p) => format!("plugin:{p}"),
        None => serde_json::to_value(kind).ok().and_then(|v| v.as_str().map(String::from)).unwrap_or_default(),
    };
    let fx = match plugin {
        Some(pid) if kind == EffectKind::OpenFx => Effect::new_ofx(&*reelforge_core::ofx::find(&pid).ok_or_else(|| format!("The OpenFX plugin \"{pid}\" isn't installed"))?),
        Some(pid) => Effect::new_plugin(&*reelforge_core::plugin::find(&pid).ok_or_else(|| format!("The plugin \"{pid}\" isn't installed"))?),
        None if matches!(kind, EffectKind::Plugin | EffectKind::OpenFx) => return Err("no plugin given".into()),
        None => Effect::new(kind),
    };
    ed(|e| {
        e.commit_live_edit();
        let name = fx.label();
        match e.seq().find_clip(ClipId(id)) {
            None => {
                e.status = "Select a video, picture or title clip first".into();
                return;
            }
            Some((_, c)) if c.scene.is_some() => {
                e.status = "Cameras, lights and points have no picture to add effects to: add them to a layer, or to an adjustment layer to change everything below".into();
                return;
            }
            _ => {}
        }
        edit(e, id, |list, _, _| list.push(fx));
        e.commit_live_edit();
        e.status = format!("Added {name}");
        if !key.is_empty() {
            e.note_recent_effect(&key);
            crate::save_settings(e);
        }
    })
}

/// A small picture of the frame at the playhead with effect `kind` (or
/// plugin `plugin`) added to clip `id` at its defaults, for the effects browser.
#[tauri::command]
pub fn effect_thumb(id: u64, kind: EffectKind, plugin: Option<String>, step: Option<u32>) -> Res<tauri::ipc::Response> {
    let fx = match plugin {
        Some(pid) if kind == EffectKind::OpenFx => Effect::new_ofx(&*reelforge_core::ofx::find(&pid).ok_or("not installed")?),
        Some(pid) => Effect::new_plugin(&*reelforge_core::plugin::find(&pid).ok_or("not installed")?),
        None if matches!(kind, EffectKind::Plugin | EffectKind::OpenFx) => return Err("no plugin given".into()),
        None => Effect::new(kind),
    };
    let jpg = ed(|e| e.effect_thumbnail_at(ClipId(id), fx, 192, step.unwrap_or(0)))?.ok_or("no picture")?;
    Ok(tauri::ipc::Response::new(jpg))
}

/// One-step changes to effect `index`: "up", "down", "remove", "toggle",
/// "duplicate" or "reset" (defaults, no keyframes).
#[tauri::command]
pub fn effect_op(id: u64, index: usize, op: String) -> Res<()> {
    ed(|e| {
        e.commit_live_edit();
        let frame = e.frame_px();
        edit(e, id, |list, _, _| {
            if index >= list.len() {
                return;
            }
            // A camera's, light's or point's settings stay put (they can be reset or switched off).
            let fixed = matches!(list[index].kind, EffectKind::Camera | EffectKind::Light | EffectKind::Point);
            if fixed && !matches!(op.as_str(), "reset" | "toggle") {
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
                    if fx.plugin_installed() {
                        let fresh = match (fx.plugin_def(), fx.ofx_def()) {
                            (Some(d), _) => Effect::new_plugin(&d),
                            (_, Some(d)) => Effect::new_ofx(&d),
                            _ => Effect::new(fx.kind),
                        };
                        let data = if matches!(fx.kind, EffectKind::Lut | EffectKind::Light) { fx.data.take() } else { fresh.data.clone() };
                        let light_kind = fx.value_at(reelforge_core::effects::light::KIND, Time::ZERO);
                        *fx = Effect { enabled: fx.enabled, data, ..fresh };
                        // A camera back to where 2D layers look unchanged; a light keeps its type.
                        if fx.kind == EffectKind::Camera {
                            let d = reelforge_core::space3d::Camera::default_for(frame).focal();
                            fx.params[reelforge_core::effects::camera::Z].value = -d;
                            fx.params[reelforge_core::effects::camera::FOCUS].value = d;
                        } else if fx.kind == EffectKind::Light {
                            fx.params[reelforge_core::effects::light::KIND].value = light_kind;
                        }
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
    effect_params(id, index, vec![(param, value)])
}

/// Several of an effect's parameters at once (a colour wheel's puck moves
/// X and Y together), as one live edit.
#[tauri::command]
pub fn effect_params(id: u64, index: usize, values: Vec<(usize, f32)>) -> Res<()> {
    ed(|e| {
        edit(e, id, |list, src, in_clip| {
            let Some(fx) = list.get_mut(index) else { return };
            fx.normalize();
            for &(param, value) in &values {
                let Some(s) = fx.spec(param) else { continue };
                let v = value.clamp(s.min, s.max);
                let p = &mut fx.params[param];
                if p.keys.is_animated() {
                    if in_clip {
                        p.keys.set(src, v);
                    }
                } else {
                    p.value = v;
                }
            }
        });
    })
}

/// The eyedropper: aim chroma key `index` at the colour of the clip's own
/// picture (before effects) at the playhead, averaged over the box from `a`
/// to `b` (layer pixels; a click averages a small square round it). Sets the
/// key hue, and lowers the threshold if the screen wouldn't all go. One undo step.
#[tauri::command]
pub fn effect_key_pick(id: u64, index: usize, a: [f32; 2], b: [f32; 2]) -> Res<String> {
    use reelforge_core::effects::key;
    let (path, at, layer, threshold, softness) = ed(|e| {
        let (_, c) = e.seq().find_clip(ClipId(id))?;
        let fx = c.effects.get(index).filter(|f| f.kind == EffectKind::ChromaKey || f.kind == EffectKind::HslQualifier)?;
        let (src, _) = effect_time(e, c);
        let m = e.project.media(c.media)?;
        let size = e.project.layer_dims(c, e.seq())?;
        Some((m.path.clone(), e.track_time(c), size, fx.value_at(1, src), fx.value_at(2, src)))
    })?
    .ok_or("The eyedropper picks from a video or picture clip's own footage")?;
    let mut src = reelforge_media::FrameSource::open_cached(&path, None).map_err(|e| format!("{e:#}"))?;
    let f = src.frame_at_blocking(at, std::time::Duration::from_secs(10)).ok_or("Couldn't read the frame at the playhead")?;
    let rgba = f.pixels.to_rgba();
    // Layer pixels → the frame's (a proxy may be smaller).
    let (sx, sy) = (f.width as f32 / layer[0].max(1.0), f.height as f32 / layer[1].max(1.0));
    let small = (a[0] - b[0]).abs() < 3.0 && (a[1] - b[1]).abs() < 3.0;
    let r = if small { 4.0 } else { 0.0 };
    let clampx = |v: f32| (v * sx).round().clamp(0.0, f.width as f32 - 1.0) as u32;
    let clampy = |v: f32| (v * sy).round().clamp(0.0, f.height as f32 - 1.0) as u32;
    let (x0, x1) = (clampx(a[0].min(b[0]) - r), clampx(a[0].max(b[0]) + r));
    let (y0, y1) = (clampy(a[1].min(b[1]) - r), clampy(a[1].max(b[1]) + r));
    let mut sum = [0f64; 3];
    let mut n = 0f64;
    for y in y0..=y1 {
        for x in x0..=x1 {
            let p = &rgba[((y * f.width + x) * 4) as usize..][..4];
            if p[3] > 0 {
                for k in 0..3 {
                    sum[k] += p[k] as f64;
                }
                n += 1.0;
            }
        }
    }
    if n == 0.0 {
        return Err("Nothing to pick there: the picture is see-through".into());
    }
    let rgb = sum.map(|v| (v / n / 255.0) as f32);
    let hex = format!("#{:02x}{:02x}{:02x}", (rgb[0] * 255.0).round() as u8, (rgb[1] * 255.0).round() as u8, (rgb[2] * 255.0).round() as u8);
    // The HSL qualifier: its ranges round the picked colour.
    let qualifier = ed(|e| e.seq().find_clip(ClipId(id)).and_then(|(_, c)| c.effects.get(index)).is_some_and(|f| f.kind == EffectKind::HslQualifier))?;
    if qualifier {
        let set = reelforge_core::effects::qualify::pick(rgb);
        let (h, s, l) = reelforge_core::effects::qualify::hsl(rgb);
        return ed(|e| {
            e.commit_live_edit();
            edit(e, id, |list, src, in_clip| {
                let Some(fx) = list.get_mut(index).filter(|f| f.kind == EffectKind::HslQualifier) else { return };
                fx.normalize();
                for (param, v) in set {
                    let p = &mut fx.params[param];
                    if p.keys.is_animated() {
                        if in_clip {
                            p.keys.set(src, v);
                        }
                    } else {
                        p.value = v;
                    }
                }
            });
            e.commit_live_edit();
            let msg = format!("Colour picked: {hex} (hue {h:.0}°, saturation {:.0} %, luminance {:.0} %). Turn on Show → The matte to check what's chosen", s * 100.0, l * 100.0);
            e.status = msg.clone();
            msg
        });
    }
    let Some((hue, lower)) = key::pick(rgb, threshold, softness) else {
        let msg = format!("That's grey ({hex}): there's no colour to key. Pick the screen, or use the luma key for black or white");
        let _ = ed(|e| e.status = msg.clone());
        return Ok(msg);
    };
    ed(|e| {
        e.commit_live_edit();
        edit(e, id, |list, src, in_clip| {
            let Some(fx) = list.get_mut(index).filter(|f| f.kind == EffectKind::ChromaKey) else { return };
            fx.normalize();
            let mut set = |param: usize, v: f32| {
                let p = &mut fx.params[param];
                if p.keys.is_animated() {
                    if in_clip {
                        p.keys.set(src, v);
                    }
                } else {
                    p.value = v;
                }
            };
            set(0, hue);
            if let Some((t, s)) = lower {
                set(1, t);
                set(2, s);
            }
        });
        e.commit_live_edit();
        let msg = match lower {
            Some((t, s)) => format!("Key colour picked: {hex}, hue {hue:.1}°; threshold and softness lowered to {t:.1} % and {s:.1} % so all of it goes"),
            None => format!("Key colour picked: {hex}, hue {hue:.1}°"),
        };
        e.status = msg.clone();
        msg
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

/// Set particle emitter `index` to ready-made preset `preset` (see
/// [`reelforge_core::particles::PRESETS`]; its position and seed are kept):
/// one undo step. Animated settings get a key at the playhead.
#[tauri::command]
pub fn particles_preset(id: u64, index: usize, preset: usize) -> Res<()> {
    use reelforge_core::particles;
    let Some(&(name, values)) = particles::PRESETS.get(preset) else { return Err("no such preset".into()) };
    ed(|e| {
        e.commit_live_edit();
        edit(e, id, |list, src, in_clip| {
            let Some(fx) = list.get_mut(index).filter(|f| f.kind == EffectKind::Particles) else { return };
            fx.normalize();
            for (k, v) in values.into_iter().enumerate() {
                let p = &mut fx.params[particles::SHAPE + k];
                if p.keys.is_animated() {
                    if in_clip {
                        p.keys.set(src, v);
                    }
                } else {
                    p.value = v;
                }
            }
        });
        e.commit_live_edit();
        e.status = format!("{name} particles");
    })
}

#[derive(Serialize, Clone)]
pub struct FaceView {
    pub on: bool,
    pub frames: usize,
    /// Where it's first seen (source ticks), and its box there (middle, size; fractions of the layer).
    pub at: i64,
    pub rect: [f32; 4],
}

/// Face beauty `index` of clip `id`: find the faces and follow them, in the background.
#[tauri::command]
pub fn find_faces(id: u64, index: usize) -> Res<()> {
    ed(|e| e.start_find_faces(reelforge_core::ClipId(id), index))
}

/// Face beauty: work on face `face` or leave it be.
#[tauri::command]
pub fn face_on(id: u64, index: usize, face: usize, on: bool) -> Res<()> {
    ed(|e| {
        e.commit_live_edit();
        edit(e, id, |list, _, _| {
            if let Some(EffectData::Faces(f)) = list.get_mut(index).and_then(|fx| fx.data.as_mut()) {
                if let Some(t) = f.get_mut(face) {
                    t.on = on;
                }
            }
        });
        e.commit_live_edit();
    })
}

/// Sky replacement `index` of clip `id`: find the sky in every frame, in the background.
#[tauri::command]
pub fn find_sky(id: u64, index: usize) -> Res<()> {
    ed(|e| e.start_find_sky(reelforge_core::ClipId(id), index))
}

/// Sky replacement: choose the new sky's picture.
#[tauri::command]
pub fn sky_image(id: u64, index: usize) -> Res<()> {
    let Some(path) = rfd::FileDialog::new().set_title("The new sky (a picture)").add_filter("Pictures", &["jpg", "jpeg", "png", "tif", "tiff", "exr", "bmp", "webp"]).pick_file() else { return Ok(()) };
    ed(|e| {
        e.commit_live_edit();
        edit(e, id, |list, _, _| {
            if let Some(fx) = list.get_mut(index).filter(|f| f.kind == EffectKind::SkyReplace) {
                let matte = match &fx.data {
                    Some(EffectData::Sky { matte, .. }) => matte.clone(),
                    _ => None,
                };
                fx.data = Some(EffectData::Sky { image: Some(path.clone()), matte });
                fx.params[reelforge_core::effects::sky::SOURCE].value = 0.0;
            }
        });
        e.commit_live_edit();
        e.status = format!("New sky: {}", path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
    })
}

/// Work out object removal `index` on clip `id` in the background.
#[tauri::command]
pub fn object_removal(id: u64, index: usize, preview: Option<bool>) -> Res<()> {
    ed(|e| e.start_object_removal(reelforge_core::ClipId(id), index, preview.unwrap_or(false)))
}

/// Paint a stroke (layer pixels) with Paint effect `index` of clip `id`;
/// `source` sets where clone strokes copy from (an Alt-click).
#[tauri::command]
pub fn paint_stroke(id: u64, index: usize, points: Vec<[f32; 2]>, source: Option<[f32; 2]>) -> Res<()> {
    ed(|e| e.paint_stroke(reelforge_core::ClipId(id), index, points, source))
}

/// Take every stroke off Paint effect `index` of clip `id`.
#[tauri::command]
pub fn paint_clear(id: u64, index: usize) -> Res<()> {
    ed(|e| e.paint_clear(reelforge_core::ClipId(id), index))
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

/// Set several parameters of effect `index` (live, like dragging a number):
/// keyed at the playhead where a parameter is animated.
pub(crate) fn set_params(e: &mut Editor, id: u64, index: usize, values: &[(usize, f32)]) {
    edit(e, id, |list, src, in_clip| {
        let Some(fx) = list.get_mut(index) else { return };
        fx.normalize();
        for &(param, value) in values {
            let Some(s) = fx.spec(param) else { continue };
            let v = value.clamp(s.min, s.max);
            let p = &mut fx.params[param];
            if p.keys.is_animated() {
                if in_clip {
                    p.keys.set(src, v);
                }
            } else {
                p.value = v;
            }
        }
    });
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

/// An effect setting's stopwatch: on, a keyframe at the playhead (it's
/// animated from then on); off, every keyframe gone, keeping the value it
/// has at the playhead. One undo step.
#[tauri::command]
pub fn effect_stopwatch(id: u64, index: usize, param: usize) -> Res<()> {
    ed(|e| {
        e.commit_live_edit();
        let r = edit(e, id, |list, src, in_clip| {
            let Some(fx) = list.get_mut(index) else { return Ok(()) };
            fx.normalize();
            if fx.spec(param).is_none() {
                return Ok(());
            }
            let shown = fx.value_at(param, src);
            let Some(p) = fx.params.get_mut(param) else { return Ok(()) };
            if p.keys.is_animated() {
                p.value = shown;
                p.keys.0.clear();
            } else if in_clip {
                p.keys.set(src, shown);
            } else {
                return Err("Move the playhead onto the clip to start keyframing");
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

/// The Hue curves effect's points (live while dragging; `commit` ends it).
#[tauri::command]
pub fn effect_hue_curves(id: u64, index: usize, curves: reelforge_core::lut::HueCurves) -> Res<()> {
    // Points in 0–1, sorted, at most 32 a curve.
    let clean = |mut pts: Vec<[f32; 2]>| {
        pts.retain(|p| p[0].is_finite() && p[1].is_finite());
        for p in &mut pts {
            *p = [p[0].clamp(0.0, 1.0), p[1].clamp(0.0, 1.0)];
        }
        pts.sort_by(|a, b| a[0].total_cmp(&b[0]));
        pts.truncate(32);
        pts
    };
    let curves = reelforge_core::lut::HueCurves {
        hue_hue: clean(curves.hue_hue),
        hue_sat: clean(curves.hue_sat),
        hue_lum: clean(curves.hue_lum),
        lum_sat: clean(curves.lum_sat),
        sat_sat: clean(curves.sat_sat),
    };
    ed(|e| {
        edit(e, id, |list, _, _| {
            if let Some(fx) = list.get_mut(index).filter(|f| f.kind == EffectKind::HueCurves) {
                fx.data = Some(EffectData::HueCurves(curves));
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

/// Set a difference key's clean plate: the clip's own frame at the playhead
/// (`file` false), or a frame of a picture or video file you choose.
#[tauri::command]
pub fn effect_plate(id: u64, index: usize, file: bool) -> Res<()> {
    let chosen = if file {
        let Some(p) = rfd::FileDialog::new()
            .set_title("A clean plate: a picture (or video) of the background alone")
            .add_filter("Pictures and video", &["png", "jpg", "jpeg", "tif", "tiff", "exr", "bmp", "webp", "mp4", "mov", "mkv", "mxf", "avi", "webm"])
            .pick_file()
        else {
            return Ok(());
        };
        Some((p, Time::ZERO))
    } else {
        None
    };
    ed(|e| {
        e.commit_live_edit();
        let Some((_, c)) = e.seq().find_clip(ClipId(id)) else { return };
        let (path, at) = match chosen {
            Some(x) => x,
            None => {
                let Some(m) = e.project.media(c.media) else {
                    e.status = "This clip has no footage of its own to take a clean plate from: choose a file".into();
                    return;
                };
                (m.path.clone(), e.track_time(c))
            }
        };
        let ok = reelforge_core::lut::load_plate(&path, at).is_some();
        let rate = e.seq().frame_rate;
        edit(e, id, |list, _, _| {
            if let Some(fx) = list.get_mut(index).filter(|f| f.kind == EffectKind::DifferenceKey) {
                fx.data = Some(EffectData::Plate { path: path.clone(), at });
            }
        });
        e.commit_live_edit();
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        e.status = if ok { format!("Clean plate: {name} at {}", at.timecode(rate)) } else { format!("Couldn't read a frame from {name}") };
    })
}

/// Ask for an environment light's panorama.
fn pick_environment() -> Option<std::path::PathBuf> {
    rfd::FileDialog::new()
        .set_title("An environment: a panorama picture (equirectangular, 2:1), best an HDR")
        .add_filter("Panoramas", &["hdr", "exr", "png", "jpg", "jpeg", "tif", "tiff", "webp"])
        .pick_file()
}

/// Choose the picture of light clip `id`'s environment (and make it an environment light).
#[tauri::command]
pub fn light_environment(id: u64, index: usize) -> Res<()> {
    let Some(path) = pick_environment() else { return Ok(()) };
    ed(|e| set_environment(e, id, index, path))
}

fn set_environment(e: &mut Editor, id: u64, index: usize, path: std::path::PathBuf) {
    e.commit_live_edit();
    edit(e, id, |list, _, _| {
        if let Some(fx) = list.get_mut(index).filter(|f| f.kind == EffectKind::Light) {
            fx.normalize();
            fx.data = Some(EffectData::Environment(path.clone()));
            fx.params[reelforge_core::effects::light::KIND].value = reelforge_core::effects::light::ENVIRONMENT as f32;
        }
    });
    e.commit_live_edit();
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    e.status = if reelforge_core::environment::load(&path).is_some() {
        format!("Lit by {name}: 3D layers take its light, and shiny 3D text, shapes and models reflect it (Reflections)")
    } else {
        format!("Couldn't read {name} as a panorama")
    };
}

/// Timeline → Add environment light: choose a panorama, then a light clip lit by it.
#[tauri::command]
pub fn add_environment_light() -> Res<()> {
    let Some(path) = pick_environment() else { return Ok(()) };
    ed(|e| {
        e.add_scene_object(reelforge_core::space3d::SceneObject::Light, reelforge_core::effects::light::ENVIRONMENT as usize);
        if let Some(id) = e.selection {
            set_environment(e, id.0, 0, path);
        }
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
    let mut ofx = reelforge_core::ofx::installed();
    ofx.sort_by(|a, b| (a.group.to_lowercase(), a.name.to_lowercase()).cmp(&(b.group.to_lowercase(), b.name.to_lowercase())));
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
            if cat == EffectCategory::OpenFx {
                items.extend(ofx.iter().map(|d| CatalogItem {
                    kind: EffectKind::OpenFx,
                    plugin: Some(d.id.clone()),
                    label: d.name.clone(),
                    description: if d.description.is_empty() { format!("An OpenFX plugin ({})", d.group) } else { d.description.clone() },
                    author: d.group.clone(),
                    version: d.version.clone(),
                }));
            }
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
