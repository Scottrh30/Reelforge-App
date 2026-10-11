//! Video effects: a stack per clip, applied to the layer before it's
//! transformed and composited (so a blur or key moves and scales with it).
//!
//! An effect is a kind plus a list of numeric parameters. Every parameter can
//! be keyframed with the same [`Curve`] as the transform properties, anchored
//! to the clip's source time. The GPU side (render crate) only ever sees
//! [`EffectValues`]: the parameters evaluated at one moment.

use crate::anim::Curve;
use crate::lut::{Curves, Table};
use crate::time::Time;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;

/// Most parameters any effect has (the GPU uniform holds this many).
pub const MAX_PARAMS: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EffectKind {
    /// Exposure, contrast, saturation and white balance, in linear light.
    ColorCorrect,
    /// Gaussian blur.
    Blur,
    /// Makes one colour (by hue) transparent, for green/blue screen.
    ChromaKey,
    /// Darkens the edges of the layer.
    Vignette,
    /// Input and output black/white points and gamma.
    Levels,
    /// Tone curves, master and per channel (the points live in `Effect::data`).
    Curves,
    /// Unsharp-mask style sharpening.
    Sharpen,
    /// Bright parts bloom outwards.
    Glow,
    /// A soft shadow behind the layer's shape (titles, logos, keyed footage).
    DropShadow,
    /// A 3D colour lookup table from a .cube file (the path lives in `Effect::data`).
    Lut,
}

/// Where an effect is listed in the effects browser.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EffectCategory {
    Color,
    BlurSharpen,
    Keying,
    Stylize,
}

impl EffectCategory {
    pub const ALL: [EffectCategory; 4] = [EffectCategory::Color, EffectCategory::BlurSharpen, EffectCategory::Keying, EffectCategory::Stylize];
    pub fn label(self) -> &'static str {
        match self {
            EffectCategory::Color => "Colour",
            EffectCategory::BlurSharpen => "Blur & sharpen",
            EffectCategory::Keying => "Keying",
            EffectCategory::Stylize => "Light & stylize",
        }
    }
}

/// Extra settings some effects have beyond numbers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum EffectData {
    Curves(Curves),
    /// A .cube file.
    Lut(PathBuf),
}

/// Describes one parameter: its name, range and how the UI should show it.
#[derive(Clone, Copy, Debug)]
pub struct ParamSpec {
    pub name: &'static str,
    pub default: f32,
    pub min: f32,
    pub max: f32,
    /// Unit shown after the value.
    pub suffix: &'static str,
    /// Drag speed per pixel in the UI.
    pub speed: f32,
}

const fn spec(name: &'static str, default: f32, min: f32, max: f32, suffix: &'static str, speed: f32) -> ParamSpec {
    ParamSpec { name, default, min, max, suffix, speed }
}

const COLOR_CORRECT: [ParamSpec; 5] = [
    spec("Exposure", 0.0, -4.0, 4.0, " stops", 0.02),
    spec("Contrast", 100.0, 0.0, 200.0, " %", 0.5),
    spec("Saturation", 100.0, 0.0, 200.0, " %", 0.5),
    spec("Temperature", 0.0, -100.0, 100.0, "", 0.5),
    spec("Tint", 0.0, -100.0, 100.0, "", 0.5),
];
const BLUR: [ParamSpec; 1] = [spec("Radius", 10.0, 0.0, 250.0, " px", 0.25)];
const CHROMA_KEY: [ParamSpec; 4] = [
    spec("Key hue", 120.0, 0.0, 360.0, "°", 0.5),
    spec("Threshold", 12.0, 0.0, 100.0, " %", 0.2),
    spec("Softness", 10.0, 0.0, 100.0, " %", 0.2),
    spec("Spill", 60.0, 0.0, 100.0, " %", 0.5),
];
const LEVELS: [ParamSpec; 5] = [
    spec("Input black", 0.0, 0.0, 254.0, "", 0.5),
    spec("Input white", 255.0, 1.0, 255.0, "", 0.5),
    spec("Gamma", 1.0, 0.1, 5.0, "", 0.005),
    spec("Output black", 0.0, 0.0, 255.0, "", 0.5),
    spec("Output white", 255.0, 0.0, 255.0, "", 0.5),
];
const CURVES: [ParamSpec; 1] = [spec("Strength", 100.0, 0.0, 100.0, " %", 0.5)];
const SHARPEN: [ParamSpec; 2] = [spec("Amount", 50.0, 0.0, 500.0, " %", 0.5), spec("Radius", 1.5, 0.5, 10.0, " px", 0.02)];
const GLOW: [ParamSpec; 4] = [
    spec("Threshold", 70.0, 0.0, 100.0, " %", 0.5),
    spec("Radius", 20.0, 0.0, 200.0, " px", 0.25),
    spec("Intensity", 100.0, 0.0, 500.0, " %", 0.5),
    spec("Softness", 20.0, 0.0, 100.0, " %", 0.5),
];
const DROP_SHADOW: [ParamSpec; 4] = [
    spec("Distance", 10.0, 0.0, 500.0, " px", 0.25),
    spec("Angle", 135.0, -360.0, 360.0, "°", 0.5),
    spec("Softness", 8.0, 0.0, 200.0, " px", 0.25),
    spec("Opacity", 60.0, 0.0, 100.0, " %", 0.5),
];
const LUT: [ParamSpec; 1] = [spec("Strength", 100.0, 0.0, 100.0, " %", 0.5)];
const VIGNETTE: [ParamSpec; 3] = [
    spec("Amount", 50.0, 0.0, 100.0, " %", 0.5),
    spec("Size", 70.0, 0.0, 150.0, " %", 0.5),
    spec("Softness", 50.0, 1.0, 100.0, " %", 0.5),
];

impl EffectKind {
    pub const ALL: [EffectKind; 10] = [
        EffectKind::ColorCorrect,
        EffectKind::Levels,
        EffectKind::Curves,
        EffectKind::Lut,
        EffectKind::Blur,
        EffectKind::Sharpen,
        EffectKind::ChromaKey,
        EffectKind::Glow,
        EffectKind::DropShadow,
        EffectKind::Vignette,
    ];

    pub fn label(self) -> &'static str {
        match self {
            EffectKind::ColorCorrect => "Colour correction",
            EffectKind::Blur => "Gaussian blur",
            EffectKind::ChromaKey => "Chroma key",
            EffectKind::Vignette => "Vignette",
            EffectKind::Levels => "Levels",
            EffectKind::Curves => "Curves",
            EffectKind::Sharpen => "Sharpen",
            EffectKind::Glow => "Glow",
            EffectKind::DropShadow => "Drop shadow",
            EffectKind::Lut => "LUT (.cube)",
        }
    }

    pub fn category(self) -> EffectCategory {
        match self {
            EffectKind::ColorCorrect | EffectKind::Levels | EffectKind::Curves | EffectKind::Lut => EffectCategory::Color,
            EffectKind::Blur | EffectKind::Sharpen => EffectCategory::BlurSharpen,
            EffectKind::ChromaKey => EffectCategory::Keying,
            EffectKind::Glow | EffectKind::DropShadow | EffectKind::Vignette => EffectCategory::Stylize,
        }
    }

    /// One line for the effects browser.
    pub fn description(self) -> &'static str {
        match self {
            EffectKind::ColorCorrect => "Exposure, contrast, saturation and white balance.",
            EffectKind::Levels => "Set the black and white points and the midtones (gamma), like Photoshop's Levels.",
            EffectKind::Curves => "Shape the tones with curves: an S-curve for contrast, or per-channel colour shifts.",
            EffectKind::Lut => "Apply a colour look or camera conversion from a .cube LUT file.",
            EffectKind::Blur => "Soft, even blur.",
            EffectKind::Sharpen => "Crisper edges and detail.",
            EffectKind::ChromaKey => "Remove a green or blue screen.",
            EffectKind::Glow => "Bright areas bloom out with a soft halo.",
            EffectKind::DropShadow => "A soft shadow behind a title, logo or keyed subject.",
            EffectKind::Vignette => "Darken the edges to draw the eye to the middle.",
        }
    }

    pub fn params(self) -> &'static [ParamSpec] {
        match self {
            EffectKind::ColorCorrect => &COLOR_CORRECT,
            EffectKind::Blur => &BLUR,
            EffectKind::ChromaKey => &CHROMA_KEY,
            EffectKind::Vignette => &VIGNETTE,
            EffectKind::Levels => &LEVELS,
            EffectKind::Curves => &CURVES,
            EffectKind::Sharpen => &SHARPEN,
            EffectKind::Glow => &GLOW,
            EffectKind::DropShadow => &DROP_SHADOW,
            EffectKind::Lut => &LUT,
        }
    }
}

/// One parameter: a static value, or keyframes when it's animated.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Param {
    pub value: f32,
    #[serde(default)]
    pub keys: Curve<f32>,
}

fn yes() -> bool {
    true
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Effect {
    pub kind: EffectKind,
    #[serde(default = "yes")]
    pub enabled: bool,
    pub params: Vec<Param>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<EffectData>,
}

/// An effect's parameters at one moment, ready for the GPU.
#[derive(Clone, Debug, PartialEq)]
pub struct EffectValues {
    pub kind: EffectKind,
    pub values: [f32; MAX_PARAMS],
    /// The lookup table for Curves and LUT effects.
    pub table: Option<Arc<Table>>,
}

impl EffectValues {
    /// True when the effect wouldn't change the picture (so it can be skipped).
    pub fn is_identity(&self) -> bool {
        let v = &self.values;
        match self.kind {
            EffectKind::ColorCorrect => v[0] == 0.0 && v[1] == 100.0 && v[2] == 100.0 && v[3] == 0.0 && v[4] == 0.0,
            EffectKind::Blur => v[0] < 0.05,
            EffectKind::ChromaKey => false,
            EffectKind::Vignette => v[0] == 0.0,
            EffectKind::Levels => v[0] == 0.0 && v[1] == 255.0 && v[2] == 1.0 && v[3] == 0.0 && v[4] == 255.0,
            EffectKind::Curves | EffectKind::Lut => self.table.is_none() || v[0] == 0.0,
            EffectKind::Sharpen => v[0] == 0.0,
            EffectKind::Glow => v[2] == 0.0,
            EffectKind::DropShadow => v[3] == 0.0,
        }
    }
}

impl Effect {
    pub fn new(kind: EffectKind) -> Self {
        let data = match kind {
            EffectKind::Curves => Some(EffectData::Curves(Curves::default())),
            _ => None,
        };
        Self { kind, enabled: true, params: kind.params().iter().map(|s| Param { value: s.default, keys: Curve::default() }).collect(), data }
    }

    pub fn spec(&self, i: usize) -> Option<&'static ParamSpec> {
        self.kind.params().get(i)
    }

    /// Parameter `i` at source time `t`, clamped to its range. Missing
    /// parameters (an older project file) read as their default.
    pub fn value_at(&self, i: usize, t: Time) -> f32 {
        let Some(s) = self.spec(i) else { return 0.0 };
        match self.params.get(i) {
            Some(p) => p.keys.value_at(t, p.value).clamp(s.min, s.max),
            None => s.default,
        }
    }

    pub fn eval(&self, t: Time) -> EffectValues {
        let mut values = [0.0; MAX_PARAMS];
        for (i, v) in values.iter_mut().enumerate().take(self.kind.params().len()) {
            *v = self.value_at(i, t);
        }
        EffectValues { kind: self.kind, values, table: self.table() }
    }

    /// The lookup table this effect needs, if any (a curve, or a loaded .cube file).
    pub fn table(&self) -> Option<Arc<Table>> {
        match (&self.data, self.kind) {
            (Some(EffectData::Curves(c)), EffectKind::Curves) if !c.is_identity() => Some(Arc::new(c.table())),
            (Some(EffectData::Lut(path)), EffectKind::Lut) => crate::lut::load_cube(path).ok(),
            _ => None,
        }
    }

    pub fn is_animated(&self) -> bool {
        self.params.iter().any(|p| p.keys.is_animated())
    }

    /// Make sure there's a `Param` for every parameter the kind defines.
    pub fn normalize(&mut self) {
        let specs = self.kind.params();
        self.params.truncate(specs.len());
        for s in &specs[self.params.len()..] {
            self.params.push(Param { value: s.default, keys: Curve::default() });
        }
    }
}

/// The enabled effects of a stack at source time `t`, skipping ones that do nothing.
pub fn eval_stack(effects: &[Effect], t: Time) -> Vec<EffectValues> {
    effects.iter().filter(|e| e.enabled).map(|e| e.eval(t)).filter(|v| !v.is_identity()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_identity_except_keys() {
        for kind in EffectKind::ALL {
            let e = Effect::new(kind);
            assert_eq!(e.params.len(), kind.params().len());
            assert!(kind.params().len() <= MAX_PARAMS);
            let v = e.eval(Time::ZERO);
            match kind {
                // These start neutral, for you to adjust.
                EffectKind::ColorCorrect | EffectKind::Levels | EffectKind::Curves | EffectKind::Lut => assert!(v.is_identity(), "{kind:?}"),
                _ => assert!(!v.is_identity(), "{kind:?} should do something by default"),
            }
        }
    }

    #[test]
    fn params_animate_clamp_and_disable() {
        let mut e = Effect::new(EffectKind::Blur);
        e.params[0].keys.set(Time::from_secs_f64(0.0), 0.0);
        e.params[0].keys.set(Time::from_secs_f64(2.0), 1000.0);
        assert_eq!(e.value_at(0, Time::from_secs_f64(0.1)), 50.0);
        assert_eq!(e.value_at(0, Time::from_secs_f64(2.0)), 250.0, "clamped to the maximum");
        assert!(eval_stack(std::slice::from_ref(&e), Time::ZERO).is_empty(), "radius 0 is skipped");
        assert_eq!(eval_stack(std::slice::from_ref(&e), Time::from_secs_f64(1.0)).len(), 1);
        e.enabled = false;
        assert!(eval_stack(&[e], Time::from_secs_f64(1.0)).is_empty());
    }

    #[test]
    fn loads_old_files_with_missing_params() {
        let mut e: Effect = serde_json::from_str(r#"{"kind":"Vignette","params":[{"value":30.0}]}"#).unwrap();
        assert!(e.enabled);
        assert_eq!(e.value_at(0, Time::ZERO), 30.0);
        assert_eq!(e.value_at(2, Time::ZERO), 50.0, "default for the missing one");
        e.normalize();
        assert_eq!(e.params.len(), 3);
    }
}
