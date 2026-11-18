//! Effect plugins: GPU shader effects you can write yourself and install
//! without rebuilding Reelforge.
//!
//! A plugin is a folder holding a `plugin.toml` (its name, category and
//! parameters) and a WGSL shader. Reelforge reads every plugin folder at
//! startup (see the app's plugin manager), checks the shader compiles, and
//! puts the good ones in the [`registry`], where they show up in the effects
//! browser next to the built-in effects. On a clip a plugin effect is an
//! [`crate::Effect`] of kind [`crate::EffectKind::Plugin`] naming the plugin
//! by id; its parameters are keyframed like any other effect's.
//!
//! A project using a plugin that isn't installed still opens: the effect keeps
//! its settings and is skipped until the plugin is installed again.
//!
//! This module only reads plugins; compiling and running the shader is the
//! render crate's job (`reelforge_render::plugin`). See PLUGINS.md for how to
//! write one.

use crate::effects::{EffectCategory, ParamSpec, ParamUi, PLUGIN_MAX_PARAMS as MAX_PARAMS};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

/// The manifest file in every plugin folder.
pub const MANIFEST: &str = "plugin.toml";

/// The manifest of a plugin pack: a folder of plugin folders installed and
/// removed together.
pub const PACK_MANIFEST: &str = "pack.toml";

/// Which plugin an effect uses (stored in the project).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PluginRef {
    pub id: String,
    /// Its name when the effect was added, to show if it's not installed.
    pub name: String,
}

/// A loaded, checked plugin.
#[derive(Clone, Debug, PartialEq)]
pub struct PluginDef {
    /// Unique, e.g. "scott.rgb-split". Projects refer to the plugin by it.
    pub id: String,
    pub name: String,
    pub version: String,
    pub author: String,
    pub description: String,
    pub category: EffectCategory,
    pub params: &'static [ParamSpec],
    /// The shader's functions to run, in order: one per pass.
    pub passes: Vec<String>,
    /// The functions return premultiplied colour (rather than straight).
    pub premultiplied: bool,
    /// The plugin's own WGSL code (without Reelforge's prelude).
    pub source: String,
    pub shader_file: PathBuf,
    pub folder: PathBuf,
    /// Unique per load: changes whenever the plugin is (re)loaded, so caches
    /// (compiled shaders, rendered frames) know to refresh.
    pub revision: u64,
    /// How far (layer pixels) the effect reaches past the layer's edges: the
    /// sum of these parameters' values, times `spread_scale`. Empty for an
    /// effect that stays inside the layer.
    pub spread: Vec<usize>,
    pub spread_scale: f32,
    /// Headings shown above some parameters, as (first parameter, heading).
    pub sections: &'static [(usize, &'static str)],
    /// Points dragged in the Viewer: each is two parameters (x, y in % of
    /// the layer, y down).
    pub points: Vec<PluginPoint>,
}

/// A point of a plugin that's dragged in the Viewer.
#[derive(Clone, Debug, PartialEq)]
pub struct PluginPoint {
    pub name: String,
    /// Its x parameter; y is the next one.
    pub x: usize,
    /// Draw a guide line to the point whose x parameter is this.
    pub line_to: Option<usize>,
}

impl PluginDef {
    /// How far the effect reaches past the layer's edges (layer pixels), for
    /// these parameter values.
    pub fn spread_px(&self, values: &[f32]) -> f32 {
        self.spread.iter().map(|&i| values.get(i).copied().unwrap_or(0.0).max(0.0)).sum::<f32>() * self.spread_scale.max(0.0)
    }
}

// ---- The manifest ------------------------------------------------------------

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    id: String,
    name: String,
    #[serde(default)]
    version: String,
    #[serde(default)]
    author: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    category: String,
    #[serde(default = "default_shader")]
    shader: String,
    #[serde(default)]
    passes: Vec<String>,
    #[serde(default)]
    premultiplied: bool,
    #[serde(default, rename = "param")]
    params: Vec<ManifestParam>,
    #[serde(default)]
    spread: Vec<usize>,
    #[serde(default = "one")]
    spread_scale: f32,
    #[serde(default, rename = "point")]
    points: Vec<ManifestPoint>,
}

fn one() -> f32 {
    1.0
}

fn default_shader() -> String {
    "effect.wgsl".into()
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(deny_unknown_fields)]
struct ManifestParam {
    name: String,
    /// "number" (the default), "toggle", "choice" or "color".
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    default: f32,
    min: Option<f32>,
    max: Option<f32>,
    #[serde(default)]
    suffix: String,
    /// Drag speed: how much the value changes per pixel dragged.
    step: Option<f32>,
    #[serde(default)]
    options: Vec<String>,
    /// A heading shown above this parameter.
    #[serde(default)]
    section: String,
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestPoint {
    name: String,
    x: usize,
    #[serde(default)]
    line_to: Option<usize>,
}

/// The headings above parameters, kept for the life of the program (like
/// parameter names: reloading the same plugin reuses them).
fn intern_sections(params: &[ManifestParam]) -> &'static [(usize, &'static str)] {
    static CACHE: Mutex<Vec<(Vec<(usize, String)>, &'static [(usize, &'static str)])>> = Mutex::new(Vec::new());
    let want: Vec<(usize, String)> = params.iter().enumerate().filter(|(_, p)| !p.section.trim().is_empty()).map(|(i, p)| (i, p.section.trim().to_string())).collect();
    if want.is_empty() {
        return &[];
    }
    let mut cache = CACHE.lock().unwrap();
    if let Some((_, s)) = cache.iter().find(|(w, _)| *w == want) {
        return s;
    }
    let made: &'static [(usize, &'static str)] = Box::leak(want.iter().map(|(i, h)| (*i, &*Box::leak(h.clone().into_boxed_str()))).collect::<Vec<_>>().into_boxed_slice());
    cache.push((want, made));
    made
}

/// Category names a manifest may use (case and spacing don't matter).
fn category_of(s: &str) -> EffectCategory {
    let s: String = s.to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect();
    match s.as_str() {
        "colour" | "color" | "colorcorrection" | "colourcorrection" | "grade" | "grading" => EffectCategory::Color,
        "blur" | "sharpen" | "blursharpen" | "blurandsharpen" => EffectCategory::BlurSharpen,
        "key" | "keying" => EffectCategory::Keying,
        "stylize" | "stylise" | "light" | "lightstylize" | "lightandstylize" => EffectCategory::Stylize,
        "distort" | "distortion" | "warp" => EffectCategory::Distort,
        "repair" | "restoration" | "cleanup" => EffectCategory::Repair,
        "generate" | "generator" | "render" | "noise" => EffectCategory::Generate,
        _ => EffectCategory::Plugins,
    }
}

/// Parameter specs are `&'static` like the built-in effects' tables, so each
/// distinct set is kept for the life of the program (and reused when a plugin
/// is reloaded with the same parameters, so editing its shader leaks nothing).
fn intern_params(params: &[ManifestParam]) -> Result<&'static [ParamSpec], String> {
    static CACHE: Mutex<Vec<(Vec<ManifestParam>, &'static [ParamSpec])>> = Mutex::new(Vec::new());
    let mut cache = CACHE.lock().unwrap();
    if let Some((_, specs)) = cache.iter().find(|(p, _)| p.as_slice() == params) {
        return Ok(specs);
    }
    let leak = |s: &str| -> &'static str { Box::leak(s.to_string().into_boxed_str()) };
    let mut out = Vec::new();
    for (i, p) in params.iter().enumerate() {
        let what = format!("parameter {} (\"{}\")", i + 1, p.name);
        let spec = match p.kind.to_lowercase().as_str() {
            "" | "number" | "slider" | "float" => {
                let (min, max) = (p.min.unwrap_or(0.0), p.max.unwrap_or(100.0));
                if min.is_nan() || max.is_nan() || min >= max {
                    return Err(format!("{what}: min ({min}) must be less than max ({max})"));
                }
                let speed = p.step.unwrap_or(((max - min) / 400.0).clamp(0.001, 10.0));
                ParamSpec { name: leak(&p.name), default: p.default.clamp(min, max), min, max, suffix: leak(&p.suffix), speed, ui: ParamUi::Number }
            }
            "toggle" | "checkbox" | "bool" => ParamSpec { name: leak(&p.name), default: if p.default >= 0.5 { 1.0 } else { 0.0 }, min: 0.0, max: 1.0, suffix: "", speed: 1.0, ui: ParamUi::Toggle },
            "choice" | "menu" | "list" => {
                if p.options.is_empty() {
                    return Err(format!("{what}: a choice needs options = [\"…\", \"…\"]"));
                }
                let opts: Vec<&'static str> = p.options.iter().map(|o| leak(o)).collect();
                let n = opts.len() as f32;
                ParamSpec { name: leak(&p.name), default: p.default.round().clamp(0.0, n - 1.0), min: 0.0, max: n - 1.0, suffix: "", speed: 1.0, ui: ParamUi::Choice(Box::leak(opts.into_boxed_slice())) }
            }
            "color" | "colour" => ParamSpec { name: leak(&p.name), default: p.default.round().clamp(0.0, 16_777_215.0), min: 0.0, max: 16_777_215.0, suffix: "", speed: 1.0, ui: ParamUi::Color },
            other => return Err(format!("{what}: unknown type \"{other}\" (use number, toggle, choice or color)")),
        };
        out.push(spec);
    }
    let specs: &'static [ParamSpec] = Box::leak(out.into_boxed_slice());
    cache.push((params.to_vec(), specs));
    Ok(specs)
}

/// Is `s` usable as a WGSL function name?
fn is_ident(s: &str) -> bool {
    let mut c = s.chars();
    c.next().is_some_and(|f| f.is_ascii_alphabetic() || f == '_') && c.all(|x| x.is_ascii_alphanumeric() || x == '_') && !s.starts_with("rf_")
}

/// A fresh revision number (never repeats within a run).
pub fn next_revision() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// Read a plugin from its manifest text and shader source (no files involved).
pub fn parse(manifest: &str, source: &str, folder: &Path) -> Result<PluginDef, String> {
    let m: Manifest = toml::from_str(manifest).map_err(|e| format!("{MANIFEST}: {}", e.to_string().trim()))?;
    let id = m.id.trim().to_string();
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c)) {
        return Err(format!("{MANIFEST}: id \"{}\" should be letters, digits, dots, dashes or underscores, like \"yourname.effect-name\"", m.id));
    }
    if m.name.trim().is_empty() {
        return Err(format!("{MANIFEST}: name is empty"));
    }
    if m.params.len() > MAX_PARAMS {
        return Err(format!("{MANIFEST}: {} parameters; a plugin can have at most {MAX_PARAMS}", m.params.len()));
    }
    let passes = if m.passes.is_empty() { vec!["effect".to_string()] } else { m.passes };
    if passes.len() > 8 {
        return Err(format!("{MANIFEST}: {} passes; at most 8", passes.len()));
    }
    if let Some(bad) = passes.iter().find(|p| !is_ident(p)) {
        return Err(format!("{MANIFEST}: pass \"{bad}\" isn't a usable function name (letters, digits and _, not starting with rf_)"));
    }
    if let Some(bad) = m.spread.iter().find(|&&i| i >= m.params.len()) {
        return Err(format!("{MANIFEST}: spread names parameter {bad}, but there are only {} (numbered from 0)", m.params.len()));
    }
    for p in &m.points {
        let ok = |x: usize| x + 1 < m.params.len();
        if !ok(p.x) {
            return Err(format!("{MANIFEST}: point \"{}\" uses parameters {} and {}, but there are only {} (numbered from 0)", p.name, p.x, p.x + 1, m.params.len()));
        }
        if let Some(t) = p.line_to.filter(|&t| !m.points.iter().any(|q| q.x == t)) {
            return Err(format!("{MANIFEST}: point \"{}\" has line_to = {t}, but no point has x = {t}", p.name));
        }
    }
    if !m.spread_scale.is_finite() || m.spread_scale < 0.0 {
        return Err(format!("{MANIFEST}: spread_scale should be 0 or more"));
    }
    let params = intern_params(&m.params).map_err(|e| format!("{MANIFEST}: {e}"))?;
    Ok(PluginDef {
        id,
        name: m.name.trim().to_string(),
        version: m.version,
        author: m.author,
        description: m.description,
        category: category_of(&m.category),
        params,
        passes,
        premultiplied: m.premultiplied,
        source: source.to_string(),
        shader_file: folder.join(&m.shader),
        folder: folder.to_path_buf(),
        revision: next_revision(),
        sections: intern_sections(&m.params),
        points: m.points.into_iter().map(|p| PluginPoint { name: p.name, x: p.x, line_to: p.line_to }).collect(),
        spread: m.spread,
        spread_scale: m.spread_scale,
    })
}

/// Read the plugin in `folder` (its manifest and shader).
pub fn load_folder(folder: &Path) -> Result<PluginDef, String> {
    let manifest = std::fs::read_to_string(folder.join(MANIFEST)).map_err(|e| format!("can't read {MANIFEST}: {e}"))?;
    // The shader's name comes from the manifest; read that first (default effect.wgsl).
    let shader = toml::from_str::<toml::Table>(&manifest).ok().and_then(|t| t.get("shader").and_then(|s| s.as_str().map(String::from))).unwrap_or_else(default_shader);
    let source = std::fs::read_to_string(folder.join(&shader)).map_err(|e| format!("can't read the shader {shader}: {e}"))?;
    parse(&manifest, &source, folder)
}

// ---- Packs ---------------------------------------------------------------------

/// A plugin pack's own details (its pack.toml).
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackDef {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub description: String,
}

/// Read a pack.toml's text.
pub fn parse_pack(manifest: &str) -> Result<PackDef, String> {
    let mut p: PackDef = toml::from_str(manifest).map_err(|e| format!("{PACK_MANIFEST}: {}", e.to_string().trim()))?;
    p.id = p.id.trim().to_string();
    p.name = p.name.trim().to_string();
    if p.id.is_empty() || !p.id.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c)) {
        return Err(format!("{PACK_MANIFEST}: id \"{}\" should be letters, digits, dots, dashes or underscores", p.id));
    }
    if p.name.is_empty() {
        return Err(format!("{PACK_MANIFEST}: name is empty"));
    }
    Ok(p)
}

/// Read the pack in `folder` (its pack.toml).
pub fn load_pack(folder: &Path) -> Result<PackDef, String> {
    let text = std::fs::read_to_string(folder.join(PACK_MANIFEST)).map_err(|e| format!("can't read {PACK_MANIFEST}: {e}"))?;
    parse_pack(&text)
}

// ---- The registry --------------------------------------------------------------

/// The installed plugins everyone sees (the effects browser, rendering, export).
#[derive(Default)]
struct Registry {
    by_id: HashMap<String, Arc<PluginDef>>,
    order: Vec<String>,
}

static REGISTRY: RwLock<Option<Registry>> = RwLock::new(None);

/// Replace the installed plugins (after a scan or reload). Later duplicates of
/// an id are ignored (the caller reports them).
pub fn set_installed(defs: Vec<Arc<PluginDef>>) {
    let mut r = Registry::default();
    for d in defs {
        if !r.by_id.contains_key(&d.id) {
            r.order.push(d.id.clone());
            r.by_id.insert(d.id.clone(), d);
        }
    }
    *REGISTRY.write().unwrap() = Some(r);
}

/// The installed plugin with this id.
pub fn find(id: &str) -> Option<Arc<PluginDef>> {
    REGISTRY.read().unwrap().as_ref().and_then(|r| r.by_id.get(id).cloned())
}

/// Every installed plugin, sorted by name.
pub fn installed() -> Vec<Arc<PluginDef>> {
    let r = REGISTRY.read().unwrap();
    let mut v: Vec<Arc<PluginDef>> = r.as_ref().map(|r| r.order.iter().filter_map(|id| r.by_id.get(id).cloned()).collect()).unwrap_or_default();
    v.sort_by_key(|d| d.name.to_lowercase());
    v
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::effects::{eval_stack, Effect, EffectKind};
    use crate::time::Time;

    pub const MANIFEST_TEXT: &str = r#"
id = "test.rgb-split"
name = "RGB Split"
version = "1.0"
author = "Me"
category = "Stylize"
description = "Shifts red and blue apart."

[[param]]
name = "Amount"
default = 8
min = 0
max = 100
suffix = " px"

[[param]]
name = "Direction"
type = "choice"
options = ["Horizontal", "Vertical", "Diagonal"]
default = 1

[[param]]
name = "Animate"
type = "toggle"
default = 1
"#;

    #[test]
    fn reads_a_manifest() {
        let d = parse(MANIFEST_TEXT, "fn effect(uv: vec2<f32>) -> vec4<f32> { return sample(uv); }", Path::new("/p")).unwrap();
        assert_eq!((d.id.as_str(), d.name.as_str(), d.category), ("test.rgb-split", "RGB Split", EffectCategory::Stylize));
        assert_eq!(d.passes, ["effect"]);
        assert_eq!(d.shader_file, Path::new("/p/effect.wgsl"));
        assert_eq!(d.params.len(), 3);
        assert_eq!((d.params[0].name, d.params[0].default, d.params[0].max, d.params[0].suffix), ("Amount", 8.0, 100.0, " px"));
        assert!(matches!(d.params[1].ui, ParamUi::Choice(o) if o == ["Horizontal", "Vertical", "Diagonal"]));
        assert_eq!((d.params[1].max, d.params[1].default), (2.0, 1.0));
        assert_eq!((d.params[2].ui, d.params[2].default), (ParamUi::Toggle, 1.0));
        // Reloading the same manifest reuses the parameter table; the revision changes.
        let again = parse(MANIFEST_TEXT, "", Path::new("/p")).unwrap();
        assert!(std::ptr::eq(again.params, d.params));
        assert_ne!(again.revision, d.revision);
    }

    #[test]
    fn explains_bad_manifests() {
        let p = |m: &str| parse(m, "", Path::new("/p")).unwrap_err();
        assert!(p("name = \"x\"").contains("missing field `id`"), "{}", p("name = \"x\""));
        assert!(p("id = \"has space\"\nname = \"x\"").contains("id \"has space\""));
        assert!(p("id = \"a\"\nname = \"x\"\n[[param]]\nname = \"n\"\nmin = 5\nmax = 1").contains("min (5) must be less than max (1)"));
        assert!(p("id = \"a\"\nname = \"x\"\n[[param]]\nname = \"n\"\ntype = \"choice\"").contains("needs options"));
        assert!(p("id = \"a\"\nname = \"x\"\n[[param]]\nname = \"n\"\ntype = \"colr\"").contains("unknown type \"colr\""));
        assert!(p("id = \"a\"\nname = \"x\"\npasses = [\"rf_x\"]").contains("pass \"rf_x\""));
        assert!(p("id = \"a\"\nname = \"x\"\nsize = 3").contains("unknown field `size`"));
        assert!(p("id = \"a\"\nname = \"x\"\nname = \"y\"").contains("plugin.toml"));
    }

    #[test]
    fn plugin_effects_use_the_registry_and_survive_without_it() {
        let d = Arc::new(parse(MANIFEST_TEXT.replace("test.rgb-split", "test.reg").as_str(), "", Path::new("/p")).unwrap());
        set_installed(vec![d.clone()]);
        assert!(installed().iter().any(|p| p.id == "test.reg"));
        let mut e = Effect::new_plugin(&d);
        assert_eq!(e.kind, EffectKind::Plugin);
        assert_eq!(e.label(), "RGB Split");
        assert_eq!(e.params.len(), 3);
        e.params[0].value = 30.0;
        // Choices and toggles switch at their keyframes, holding until the next.
        e.params[1].keys.set(Time::ZERO, 0.0);
        e.params[1].keys.set(Time::from_secs_f64(1.0), 2.0);
        assert_eq!(e.value_at(1, Time::from_secs_f64(0.9)), 0.0);
        assert_eq!(e.value_at(1, Time::from_secs_f64(1.0)), 2.0);
        let v = &eval_stack(std::slice::from_ref(&e), Time::from_secs_f64(2.5))[0];
        assert_eq!((v.values[0], v.values[1], v.time), (30.0, 2.0, 2.5));
        assert_eq!(v.plugin.as_ref().map(|p| p.id.as_str()), Some("test.reg"));
        // Saved and loaded.
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.contains("\"plugin\":{\"id\":\"test.reg\",\"name\":\"RGB Split\"}"), "{json}");
        let back: Effect = serde_json::from_str(&json).unwrap();
        assert_eq!(back, e);
        // Uninstalled: still there, settings kept, nothing rendered.
        set_installed(Vec::new());
        let mut gone = back.clone();
        gone.normalize();
        assert_eq!(gone.params.len(), 3, "settings kept");
        assert_eq!(gone.label(), "RGB Split");
        assert!(!gone.plugin_installed());
        assert!(eval_stack(&[gone], Time::ZERO).is_empty());
        // Built-in effects aren't affected.
        assert_eq!(Effect::new(EffectKind::Blur).label(), "Gaussian blur");
    }

    #[test]
    fn colours_points_sections_and_32_parameters() {
        let src = "fn effect(uv: vec2<f32>) -> vec4<f32> { return sample(uv); }";
        let mut m = String::from("id = \"t.full\"\nname = \"Full\"\n[[point]]\nname = \"Start\"\nx = 0\nline_to = 2\n[[point]]\nname = \"End\"\nx = 2\n");
        for i in 0..32 {
            m += &format!("[[param]]\nname = \"p{i}\"\n");
            if i == 4 {
                m += "type = \"color\"\ndefault = 0x1f6bff\nsection = \"Look\"\n";
            }
        }
        let d = parse(&m, src, Path::new("/x")).unwrap();
        assert_eq!(d.params.len(), 32);
        assert_eq!(d.params[4].ui, ParamUi::Color);
        assert_eq!(d.params[4].default, 0x1f6bff as f32);
        assert_eq!(d.sections, &[(4, "Look")]);
        assert_eq!(d.points, vec![PluginPoint { name: "Start".into(), x: 0, line_to: Some(2) }, PluginPoint { name: "End".into(), x: 2, line_to: None }]);
        let bad = m.replace("line_to = 2", "line_to = 7");
        assert!(parse(&bad, src, Path::new("/x")).unwrap_err().contains("no point has x = 7"));
        let bad = m.replace("x = 2\n", "x = 31\n").replace("line_to = 2", "line_to = 31");
        assert!(parse(&bad, src, Path::new("/x")).unwrap_err().contains("only 32"));
        // One more than the most is refused.
        let over = format!("{m}[[param]]\nname = \"p32\"\n");
        assert!(parse(&over, src, Path::new("/x")).unwrap_err().contains("32"));
    }
}