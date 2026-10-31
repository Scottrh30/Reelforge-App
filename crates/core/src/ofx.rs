//! OpenFX plugins as Reelforge sees them: which are installed, and how their
//! parameters map onto an effect's keyframable parameters.
//!
//! The `reelforge-ofx` crate loads and runs the plugins; the app fills this
//! registry from it at startup. On a clip an OpenFX effect is an
//! [`crate::Effect`] of kind [`crate::EffectKind::OpenFx`] whose
//! [`crate::plugin::PluginRef`] names the plugin by its OpenFX identifier. A
//! project using a plugin that isn't installed still opens; the effect keeps
//! its settings and is skipped.

use crate::effects::{EffectCategory, ParamSpec, ParamUi};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

/// How one OpenFX parameter appears among the effect's parameters.
#[derive(Clone, Debug, PartialEq)]
pub enum Slot {
    /// `n` numbers. `percent`: shown as % of the frame, given as a fraction.
    Numbers { n: usize, percent: bool },
    /// A checkbox.
    Toggle,
    /// A menu (the value is the option's position).
    Choice,
    /// A colour (one parameter, `0xRRGGBB`), and with alpha a second (%).
    Color { alpha: bool },
    /// A place in the picture: x and y in % of the frame, y down (as
    /// Reelforge's own effects and the Viewer's handles have them). The
    /// plugin gets a fraction of the frame (`normalised`) or pixels, y up.
    Point { normalised: bool },
}

#[derive(Clone, Debug, PartialEq)]
pub struct SlotMap {
    /// The OpenFX parameter's position in the plugin's list.
    pub param: usize,
    pub slot: Slot,
    /// Where its values start among the effect's parameters.
    pub first: usize,
}

/// An installed OpenFX plugin.
#[derive(Clone, Debug, PartialEq)]
pub struct OfxDef {
    /// The plugin's OpenFX identifier (unique across plugins).
    pub id: String,
    pub name: String,
    /// Its own grouping, e.g. "Sapphire Blurs".
    pub group: String,
    pub description: String,
    pub version: String,
    /// The bundle it came from.
    pub bundle: PathBuf,
    pub params: &'static [ParamSpec],
    pub slots: Vec<SlotMap>,
    /// How many parameters the plugin has in all (some aren't shown).
    pub plugin_params: usize,
    /// Unique per load, so caches know when a plugin was loaded again.
    pub revision: u64,
    /// It reads other frames of its source (noise reduction, retiming…).
    pub temporal: bool,
    /// The ways it can render on the graphics card ("CUDA", "OpenCL", "OpenGL", "Metal").
    pub gpu: Vec<String>,
    /// It can render on the processor (else only on the graphics card).
    pub cpu: bool,
}

impl OfxDef {
    pub fn category(&self) -> EffectCategory {
        EffectCategory::OpenFx
    }

    /// Its position parameters: (the first of their two effect parameters, the name).
    pub fn points(&self) -> Vec<(usize, String)> {
        self.slots
            .iter()
            .filter(|m| matches!(m.slot, Slot::Point { .. }))
            .map(|m| (m.first, self.params.get(m.first).map_or(String::new(), |p| p.name.trim_end_matches(" X").to_string())))
            .collect()
    }

    /// The plugin's parameter values from the effect's (`v`, one per
    /// [`ParamSpec`]) for a picture `size` pixels big: numbers per OpenFX
    /// parameter, `None` for the ones Reelforge doesn't show (they keep the
    /// plugin's default).
    pub fn plugin_values(&self, v: &[f32], size: [f32; 2]) -> Vec<Option<Vec<f64>>> {
        let mut out = vec![None; self.plugin_params];
        let at = |i: usize| v.get(i).copied().unwrap_or(0.0) as f64;
        for m in &self.slots {
            let vals = match m.slot {
                Slot::Numbers { n, percent } => (0..n).map(|k| if percent { at(m.first + k) / 100.0 } else { at(m.first + k) }).collect(),
                Slot::Toggle | Slot::Choice => vec![at(m.first).round()],
                Slot::Point { normalised } => {
                    let (x, y) = (at(m.first) / 100.0, 1.0 - at(m.first + 1) / 100.0);
                    if normalised { vec![x, y] } else { vec![x * size[0] as f64, y * size[1] as f64] }
                }
                Slot::Color { alpha } => {
                    let c = crate::effects::unpack_rgb(at(m.first) as f32);
                    let mut x: Vec<f64> = c.iter().map(|&c| c as f64).collect();
                    if alpha {
                        x.push(at(m.first + 1) / 100.0);
                    }
                    x
                }
            };
            if let Some(o) = out.get_mut(m.param) {
                *o = Some(vals);
            }
        }
        out
    }
}

/// What an OpenFX parameter is, as the host crate reports it (kept free of
/// that crate, so core doesn't load native code).
#[derive(Clone, Debug)]
pub enum ParamDesc {
    /// `point`: a position (2 numbers: fractions of the frame if
    /// `normalised`, else pixels; y up).
    Double { label: String, dims: usize, normalised: bool, point: bool, default: Vec<f64>, min: Vec<f64>, max: Vec<f64> },
    Int { label: String, dims: usize, default: Vec<f64>, min: Vec<f64>, max: Vec<f64> },
    Bool { label: String, default: bool },
    Choice { label: String, options: Vec<String>, default: usize },
    Color { label: String, alpha: bool, default: Vec<f64> },
    /// Not shown (strings, buttons, pages, custom data, secret ones…).
    Hidden,
}

fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

/// A position given in pixels is turned into % of the frame against this
/// size for its default (OpenFX's own reference frame when a plugin describes itself).
pub const REFERENCE_SIZE: [f64; 2] = [1920.0, 1080.0];

/// Turn a plugin's parameters into effect parameters (the strings are kept
/// for the life of the program, as plugins are).
pub fn map_params(descs: &[ParamDesc]) -> (&'static [ParamSpec], Vec<SlotMap>) {
    let mut specs: Vec<ParamSpec> = Vec::new();
    let mut slots = Vec::new();
    let axes = ["X", "Y", "Z"];
    for (i, d) in descs.iter().enumerate() {
        let first = specs.len();
        let slot = match d {
            ParamDesc::Double { label, dims: 2, normalised, point: true, default, .. } => {
                // % of the frame, y down, like Reelforge's own positions.
                let d = |k: usize| default.get(k).copied().unwrap_or(0.5);
                let (x, y) = if *normalised { (d(0), d(1)) } else { (d(0) / REFERENCE_SIZE[0], d(1) / REFERENCE_SIZE[1]) };
                for (axis, value) in [("X", x * 100.0), ("Y", (1.0 - y) * 100.0)] {
                    specs.push(ParamSpec { name: leak(format!("{label} {axis}")), default: value as f32, min: -1000.0, max: 1100.0, suffix: " %", speed: 0.25, ui: ParamUi::Number });
                }
                Slot::Point { normalised: *normalised }
            }
            ParamDesc::Double { label, dims, normalised, default, min, max, .. } => {
                for k in 0..*dims {
                    let name = if *dims > 1 { format!("{label} {}", axes[k.min(2)]) } else { label.clone() };
                    let (scale, suffix) = if *normalised { (100.0, " %") } else { (1.0, "") };
                    let (lo, hi) = (min[k] * scale, max[k] * scale);
                    let (lo, hi) = (lo.min(hi), hi.max(lo));
                    specs.push(ParamSpec {
                        name: leak(name),
                        default: (default[k] * scale) as f32,
                        min: if *normalised { -1000.0 } else { lo.min(default[k] * scale) as f32 },
                        max: if *normalised { 1100.0 } else { hi.max(default[k] * scale) as f32 },
                        suffix,
                        speed: (((hi - lo) / 400.0).max(0.001)) as f32,
                        ui: ParamUi::Number,
                    });
                }
                Slot::Numbers { n: *dims, percent: *normalised }
            }
            ParamDesc::Int { label, dims, default, min, max } => {
                for k in 0..*dims {
                    let name = if *dims > 1 { format!("{label} {}", axes[k.min(2)]) } else { label.clone() };
                    specs.push(ParamSpec {
                        name: leak(name),
                        default: default[k] as f32,
                        min: min[k].min(default[k]) as f32,
                        max: max[k].max(default[k]) as f32,
                        suffix: "",
                        speed: (((max[k] - min[k]) / 300.0).max(0.05)) as f32,
                        ui: ParamUi::Number,
                    });
                }
                Slot::Numbers { n: *dims, percent: false }
            }
            ParamDesc::Bool { label, default } => {
                specs.push(ParamSpec { name: leak(label.clone()), default: *default as u8 as f32, min: 0.0, max: 1.0, suffix: "", speed: 1.0, ui: ParamUi::Toggle });
                Slot::Toggle
            }
            ParamDesc::Choice { label, options, default } => {
                let opts: &'static [&'static str] = Box::leak(options.iter().map(|o| leak(o.clone())).collect::<Vec<_>>().into_boxed_slice());
                specs.push(ParamSpec { name: leak(label.clone()), default: *default as f32, min: 0.0, max: (options.len().max(1) - 1) as f32, suffix: "", speed: 1.0, ui: ParamUi::Choice(opts) });
                Slot::Choice
            }
            ParamDesc::Color { label, alpha, default } => {
                let c = [0, 1, 2].map(|k| default.get(k).copied().unwrap_or(0.0).clamp(0.0, 1.0) as f32);
                specs.push(ParamSpec { name: leak(label.clone()), default: crate::effects::pack_rgb(c), min: 0.0, max: 16_777_215.0, suffix: "", speed: 1.0, ui: ParamUi::Color });
                if *alpha {
                    let a = default.get(3).copied().unwrap_or(1.0) * 100.0;
                    specs.push(ParamSpec { name: leak(format!("{label} opacity")), default: a as f32, min: 0.0, max: 100.0, suffix: " %", speed: 0.5, ui: ParamUi::Number });
                }
                Slot::Color { alpha: *alpha }
            }
            ParamDesc::Hidden => continue,
        };
        slots.push(SlotMap { param: i, slot, first });
    }
    (Box::leak(specs.into_boxed_slice()), slots)
}

/// One OpenFX effect of a clip at one moment: what to run it with.
#[derive(Clone, Debug)]
pub struct OfxStep {
    pub def: Arc<OfxDef>,
    /// Its position in the clip's effect stack.
    pub index: usize,
    /// One per [`OfxDef::params`].
    pub values: Vec<f32>,
    /// The clip's source time, in seconds.
    pub time: f64,
}

impl OfxStep {
    pub fn hash_into(&self, h: &mut impl std::hash::Hasher) {
        use std::hash::Hash;
        (self.def.id.as_str(), self.def.revision, self.index, self.time.to_bits()).hash(h);
        for v in &self.values {
            v.to_bits().hash(h);
        }
    }
}

/// The enabled, installed OpenFX effects of a stack at source time `src`, in order.
pub fn steps(effects: &[crate::Effect], src: crate::Time) -> Vec<OfxStep> {
    effects
        .iter()
        .enumerate()
        .filter(|(_, e)| e.enabled && e.kind == crate::EffectKind::OpenFx)
        .filter_map(|(index, e)| {
            let def = e.ofx_def()?;
            let values = (0..def.params.len()).map(|i| e.value_at(i, src)).collect();
            Some(OfxStep { def, index, values, time: src.as_secs_f64() })
        })
        .collect()
}

static REGISTRY: RwLock<Vec<Arc<OfxDef>>> = RwLock::new(Vec::new());

/// Replace the list of installed plugins.
pub fn set_installed(defs: Vec<Arc<OfxDef>>) {
    *REGISTRY.write().unwrap() = defs;
}

pub fn find(id: &str) -> Option<Arc<OfxDef>> {
    REGISTRY.read().unwrap().iter().find(|d| d.id == id).cloned()
}

pub fn installed() -> Vec<Arc<OfxDef>> {
    REGISTRY.read().unwrap().clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parameters_map_onto_effect_parameters_and_back() {
        let descs = vec![
            ParamDesc::Double { label: "Gain".into(), dims: 1, normalised: false, point: false, default: vec![1.0], min: vec![0.0], max: vec![4.0] },
            ParamDesc::Hidden,
            ParamDesc::Double { label: "Centre".into(), dims: 2, normalised: true, point: false, default: vec![0.5, 0.25], min: vec![0.0, 0.0], max: vec![1.0, 1.0] },
            ParamDesc::Bool { label: "Invert".into(), default: true },
            ParamDesc::Choice { label: "Mode".into(), options: vec!["A".into(), "B".into()], default: 1 },
            ParamDesc::Color { label: "Tint".into(), alpha: true, default: vec![1.0, 0.0, 0.0, 0.5] },
            ParamDesc::Int { label: "Steps".into(), dims: 1, default: vec![3.0], min: vec![1.0], max: vec![10.0] },
        ];
        let (specs, slots) = map_params(&descs);
        let names: Vec<&str> = specs.iter().map(|s| s.name).collect();
        assert_eq!(names, ["Gain", "Centre X", "Centre Y", "Invert", "Mode", "Tint", "Tint opacity", "Steps"]);
        assert_eq!((specs[1].default, specs[1].suffix), (50.0, " %"), "fractions of the frame shown as %");
        assert_eq!(specs[3].ui, ParamUi::Toggle);
        assert_eq!(specs[5].default, 0xff0000 as f32);
        let def = OfxDef { id: "x".into(), name: "X".into(), group: String::new(), description: String::new(), version: String::new(), bundle: PathBuf::new(), params: specs, slots, plugin_params: descs.len(), revision: 1, temporal: false, gpu: Vec::new(), cpu: true };
        let v: Vec<f32> = specs.iter().map(|s| s.default).collect();
        let back = def.plugin_values(&v, [1920.0, 1080.0]);
        assert_eq!(back[0], Some(vec![1.0]));
        assert_eq!(back[1], None, "hidden: the plugin's own default");
        assert_eq!(back[2], Some(vec![0.5, 0.25]));
        assert_eq!(back[3], Some(vec![1.0]));
        assert_eq!(back[4], Some(vec![1.0]));
        assert_eq!(back[5], Some(vec![1.0, 0.0, 0.0, 0.5]));
        assert_eq!(back[6], Some(vec![3.0]));
    }

    #[test]
    fn positions_are_percent_of_the_frame_y_down_and_go_back_in_the_plugins_units() {
        let descs = vec![
            ParamDesc::Double { label: "Centre".into(), dims: 2, normalised: true, point: true, default: vec![0.25, 0.75], min: vec![0.0; 2], max: vec![1.0; 2] },
            ParamDesc::Double { label: "Corner".into(), dims: 2, normalised: false, point: true, default: vec![480.0, 270.0], min: vec![0.0; 2], max: vec![1920.0; 2] },
        ];
        let (specs, slots) = map_params(&descs);
        let names: Vec<&str> = specs.iter().map(|s| s.name).collect();
        assert_eq!(names, ["Centre X", "Centre Y", "Corner X", "Corner Y"]);
        // 75% up from the bottom is 25% down from the top.
        assert_eq!(specs.iter().map(|s| s.default).collect::<Vec<_>>(), vec![25.0, 25.0, 25.0, 75.0]);
        let def = OfxDef { id: "p".into(), name: "P".into(), group: String::new(), description: String::new(), version: String::new(), bundle: PathBuf::new(), params: specs, slots, plugin_params: 2, revision: 1, temporal: false, gpu: Vec::new(), cpu: true };
        assert_eq!(def.points(), vec![(0, "Centre".to_string()), (2, "Corner".to_string())]);
        // Dragged to the middle-left, three quarters down, on a 200×100 picture.
        let back = def.plugin_values(&[10.0, 75.0, 10.0, 75.0], [200.0, 100.0]);
        assert_eq!(back[0], Some(vec![0.1, 0.25]), "a fraction, y up");
        assert_eq!(back[1], Some(vec![20.0, 25.0]), "pixels, y up");
    }

    #[test]
    fn openfx_effects_on_a_clip() {
        use crate::{Effect, EffectKind, Time};
        let (params, slots) = map_params(&[ParamDesc::Double { label: "Amount".into(), dims: 1, normalised: false, point: false, default: vec![0.5], min: vec![0.0], max: vec![1.0] }]);
        let def = Arc::new(OfxDef { id: "test.ofx.amount".into(), name: "Amount".into(), group: "Test".into(), description: String::new(), version: "1.0".into(), bundle: PathBuf::new(), params, slots, plugin_params: 1, revision: 7, temporal: false, gpu: Vec::new(), cpu: true });
        set_installed(vec![def.clone()]);
        let mut e = Effect::new_ofx(&def);
        assert_eq!((e.kind, e.label(), e.param_specs().len()), (EffectKind::OpenFx, "Amount".to_string(), 1));
        assert!(e.plugin_installed());
        // The GPU never draws it…
        assert!(crate::effects::eval_stack(std::slice::from_ref(&e), Time::ZERO).is_empty());
        // …it runs as a step.
        e.params[0].value = 0.75;
        let s = steps(&[Effect::new(EffectKind::Blur), e.clone()], Time::from_secs_f64(2.0));
        assert_eq!((s.len(), s[0].index, s[0].values.clone(), s[0].time), (1, 1, vec![0.75], 2.0));
        // Not installed: kept, skipped, its name remembered.
        set_installed(vec![]);
        assert!(e.plugin_missing() && steps(std::slice::from_ref(&e), Time::ZERO).is_empty());
        assert_eq!(e.label(), "Amount");
        e.normalize();
        assert_eq!(e.params.len(), 1);
    }
}
