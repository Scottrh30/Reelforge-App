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
pub const MAX_PARAMS: usize = 20;
/// Most parameters a plugin may have (its shader sees params 0–15).
pub const PLUGIN_MAX_PARAMS: usize = 16;

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
    /// An installed plugin (which one is in `Effect::plugin`; see [`crate::plugin`]).
    Plugin,
    /// A glowing energy blade (a light sword) drawn from a hilt point to a tip
    /// point, with a hot core, coloured glow, flicker and motion blur.
    EnergyBlade,
    /// A camera clip's settings (not an effect you add: see [`crate::space3d`]).
    Camera,
    /// A light clip's settings (see [`crate::space3d`]).
    Light,
    /// A point clip's settings: an invisible point other layers can be
    /// parented to (see [`crate::space3d::Null`]).
    Point,
}

/// Where an effect is listed in the effects browser.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EffectCategory {
    Color,
    BlurSharpen,
    Keying,
    Stylize,
    /// (Plugins only, so far.)
    Distort,
    /// Plugins that didn't name a category.
    Plugins,
}

impl EffectCategory {
    pub const ALL: [EffectCategory; 6] = [EffectCategory::Color, EffectCategory::BlurSharpen, EffectCategory::Keying, EffectCategory::Stylize, EffectCategory::Distort, EffectCategory::Plugins];
    pub fn label(self) -> &'static str {
        match self {
            EffectCategory::Color => "Colour",
            EffectCategory::BlurSharpen => "Blur & sharpen",
            EffectCategory::Keying => "Keying",
            EffectCategory::Stylize => "Light & stylize",
            EffectCategory::Distort => "Distort",
            EffectCategory::Plugins => "Other plugins",
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

/// How a parameter is shown and edited.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ParamUi {
    /// A number to drag or type.
    Number,
    /// A checkbox: 0 or 1.
    Toggle,
    /// A menu: the index of the chosen option.
    Choice(&'static [&'static str]),
    /// A colour: the value is `0xRRGGBB` (sRGB) as a whole number. Between
    /// keyframes each channel is interpolated.
    Color,
}

/// `0xRRGGBB` → sRGB channels 0–1.
pub fn unpack_rgb(v: f32) -> [f32; 3] {
    let c = v.round().clamp(0.0, 16_777_215.0) as u32;
    [((c >> 16) & 255) as f32 / 255.0, ((c >> 8) & 255) as f32 / 255.0, (c & 255) as f32 / 255.0]
}

/// sRGB channels 0–1 → `0xRRGGBB`.
pub fn pack_rgb(c: [f32; 3]) -> f32 {
    let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
    ((b(c[0]) << 16) | (b(c[1]) << 8) | b(c[2])) as f32
}

/// Describes one parameter: its name, range and how the UI should show it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ParamSpec {
    pub name: &'static str,
    pub default: f32,
    pub min: f32,
    pub max: f32,
    /// Unit shown after the value.
    pub suffix: &'static str,
    /// Drag speed per pixel in the UI.
    pub speed: f32,
    pub ui: ParamUi,
}

const fn spec(name: &'static str, default: f32, min: f32, max: f32, suffix: &'static str, speed: f32) -> ParamSpec {
    ParamSpec { name, default, min, max, suffix, speed, ui: ParamUi::Number }
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
const fn color(name: &'static str, rgb: u32) -> ParamSpec {
    ParamSpec { name, default: rgb as f32, min: 0.0, max: 16_777_215.0, suffix: "", speed: 1.0, ui: ParamUi::Color }
}
const fn choice(name: &'static str, options: &'static [&'static str]) -> ParamSpec {
    ParamSpec { name, default: 0.0, min: 0.0, max: (options.len() - 1) as f32, suffix: "", speed: 1.0, ui: ParamUi::Choice(options) }
}

/// Camera parameter indices (positions in pixels: X right, Y down, Z away
/// from the viewer, with the frame's centre at 0, 0, 0).
pub mod camera {
    pub const X: usize = 0;
    pub const Y: usize = 1;
    pub const Z: usize = 2;
    pub const TARGET_X: usize = 3;
    pub const TARGET_Y: usize = 4;
    pub const TARGET_Z: usize = 5;
    pub const ROLL: usize = 6;
    pub const FOV: usize = 7;
    pub const DOF: usize = 8;
    pub const FOCUS: usize = 9;
    pub const APERTURE: usize = 10;
}

/// Light parameter indices.
pub mod light {
    pub const KIND: usize = 0;
    pub const COLOR: usize = 1;
    pub const INTENSITY: usize = 2;
    pub const X: usize = 3;
    pub const Y: usize = 4;
    pub const Z: usize = 5;
    pub const TARGET_X: usize = 6;
    pub const TARGET_Y: usize = 7;
    pub const TARGET_Z: usize = 8;
    pub const CONE: usize = 9;
    pub const FEATHER: usize = 10;
    pub const FALLOFF: usize = 11;
    pub const RADIUS: usize = 12;
    /// What the light kinds are called (the Type menu).
    pub const KINDS: [&str; 4] = ["Point", "Spot", "Directional", "Ambient"];
}

/// Point parameter indices.
pub mod point {
    pub const X: usize = 0;
    pub const Y: usize = 1;
    pub const Z: usize = 2;
    pub const ROTATION: usize = 3;
    pub const SCALE: usize = 4;
}

const POINT: [ParamSpec; 5] = [
    spec("Position X", 0.0, -100000.0, 100000.0, " px", 1.0),
    spec("Position Y", 0.0, -100000.0, 100000.0, " px", 1.0),
    spec("Position Z", 0.0, -100000.0, 100000.0, " px", 1.0),
    spec("Rotation", 0.0, -3600.0, 3600.0, "°", 0.25),
    spec("Scale", 100.0, 0.0, 10000.0, " %", 0.5),
];

const CAMERA: [ParamSpec; 11] = [
    spec("Position X", 0.0, -100000.0, 100000.0, " px", 1.0),
    spec("Position Y", 0.0, -100000.0, 100000.0, " px", 1.0),
    spec("Position Z", -1500.0, -100000.0, 100000.0, " px", 1.0),
    spec("Point of interest X", 0.0, -100000.0, 100000.0, " px", 1.0),
    spec("Point of interest Y", 0.0, -100000.0, 100000.0, " px", 1.0),
    spec("Point of interest Z", 0.0, -100000.0, 100000.0, " px", 1.0),
    spec("Roll", 0.0, -3600.0, 3600.0, "°", 0.25),
    spec("Field of view", 39.6, 1.0, 170.0, "°", 0.1),
    ParamSpec { name: "Depth of field", default: 0.0, min: 0.0, max: 1.0, suffix: "", speed: 1.0, ui: ParamUi::Toggle },
    spec("Focus distance", 1500.0, 1.0, 100000.0, " px", 1.0),
    spec("Blur", 25.0, 0.0, 250.0, " px", 0.25),
];

const LIGHT: [ParamSpec; 13] = [
    choice("Type", &light::KINDS),
    color("Colour", 0xffffff),
    spec("Intensity", 100.0, 0.0, 1000.0, " %", 0.5),
    spec("Position X", -500.0, -100000.0, 100000.0, " px", 1.0),
    spec("Position Y", -400.0, -100000.0, 100000.0, " px", 1.0),
    spec("Position Z", -700.0, -100000.0, 100000.0, " px", 1.0),
    spec("Point of interest X", 0.0, -100000.0, 100000.0, " px", 1.0),
    spec("Point of interest Y", 0.0, -100000.0, 100000.0, " px", 1.0),
    spec("Point of interest Z", 0.0, -100000.0, 100000.0, " px", 1.0),
    spec("Cone angle", 60.0, 1.0, 179.0, "°", 0.25),
    spec("Cone feather", 50.0, 0.0, 100.0, " %", 0.5),
    choice("Falloff", &["None", "Smooth", "Inverse square"]),
    spec("Falloff distance", 1500.0, 1.0, 100000.0, " px", 1.0),
];

/// Energy blade parameter indices.
pub mod blade {
    pub const HILT_X: usize = 0;
    pub const HILT_Y: usize = 1;
    pub const TIP_X: usize = 2;
    pub const TIP_Y: usize = 3;
    pub const LENGTH: usize = 4;
    pub const BLADE_COLOR: usize = 5;
    pub const CORE_COLOR: usize = 6;
    pub const CORE_WIDTH: usize = 7;
    pub const CORE_SOFTNESS: usize = 8;
    pub const TAPER: usize = 9;
    pub const TIP_SHAPE: usize = 10;
    pub const GLOW_SIZE: usize = 11;
    pub const GLOW_INTENSITY: usize = 12;
    pub const HAZE: usize = 13;
    pub const OPACITY: usize = 14;
    pub const FLICKER: usize = 15;
    pub const FLICKER_SPEED: usize = 16;
    pub const WOBBLE: usize = 17;
    pub const MOTION_BLUR: usize = 18;
    pub const BLEND: usize = 19;
    /// Ready-made blade colours: name, blade colour, core colour.
    pub const PRESETS: [(&str, u32, u32); 8] = [
        ("Blue", 0x1f6bff, 0xf2f7ff),
        ("Green", 0x22e04a, 0xf2fff2),
        ("Red", 0xff1a12, 0xfff2ee),
        ("Purple", 0x9a3dff, 0xf8f0ff),
        ("Yellow", 0xffc21a, 0xfffbea),
        ("Orange", 0xff6a10, 0xfff4ea),
        ("Cyan", 0x18e0ff, 0xf0feff),
        ("White", 0xd8e4ff, 0xffffff),
    ];
}

const BLADE: [ParamSpec; 20] = [
    spec("Hilt X", 40.0, -100.0, 200.0, " %", 0.1),
    spec("Hilt Y", 78.0, -100.0, 200.0, " %", 0.1),
    spec("Tip X", 62.0, -100.0, 200.0, " %", 0.1),
    spec("Tip Y", 18.0, -100.0, 200.0, " %", 0.1),
    spec("Length", 100.0, 0.0, 100.0, " %", 0.5),
    color("Blade colour", 0x1f6bff),
    color("Core colour", 0xf2f7ff),
    spec("Core width", 9.0, 0.0, 200.0, " px", 0.1),
    spec("Core softness", 35.0, 0.0, 100.0, " %", 0.5),
    spec("Taper", 15.0, 0.0, 100.0, " %", 0.5),
    choice("Tip", &["Rounded", "Pointed", "Flat"]),
    spec("Glow size", 36.0, 0.0, 500.0, " px", 0.25),
    spec("Glow intensity", 100.0, 0.0, 500.0, " %", 0.5),
    spec("Outer haze", 35.0, 0.0, 200.0, " %", 0.5),
    spec("Opacity", 100.0, 0.0, 100.0, " %", 0.5),
    spec("Flicker", 12.0, 0.0, 100.0, " %", 0.5),
    spec("Flicker speed", 14.0, 0.0, 60.0, " Hz", 0.1),
    spec("Wobble", 8.0, 0.0, 100.0, " %", 0.5),
    spec("Motion blur", 50.0, 0.0, 100.0, " %", 0.5),
    choice("Blend", &["Add (light)", "Screen"]),
];

const VIGNETTE: [ParamSpec; 3] = [
    spec("Amount", 50.0, 0.0, 100.0, " %", 0.5),
    spec("Size", 70.0, 0.0, 150.0, " %", 0.5),
    spec("Softness", 50.0, 1.0, 100.0, " %", 0.5),
];

impl EffectKind {
    pub const ALL: [EffectKind; 11] = [
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
        EffectKind::EnergyBlade,
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
            EffectKind::Plugin => "Plugin",
            EffectKind::EnergyBlade => "Energy blade",
            EffectKind::Camera => "Camera",
            EffectKind::Light => "Light",
            EffectKind::Point => "Point",
        }
    }

    pub fn category(self) -> EffectCategory {
        match self {
            EffectKind::ColorCorrect | EffectKind::Levels | EffectKind::Curves | EffectKind::Lut => EffectCategory::Color,
            EffectKind::Blur | EffectKind::Sharpen => EffectCategory::BlurSharpen,
            EffectKind::ChromaKey => EffectCategory::Keying,
            EffectKind::Glow | EffectKind::DropShadow | EffectKind::Vignette | EffectKind::EnergyBlade | EffectKind::Camera | EffectKind::Light | EffectKind::Point => EffectCategory::Stylize,
            EffectKind::Plugin => EffectCategory::Plugins,
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
            EffectKind::Plugin => "An installed plugin.",
            EffectKind::EnergyBlade => "A glowing light-sword blade with a hot core, flicker and motion blur. Drag its hilt and tip in the Viewer, or attach it to a track.",
            EffectKind::Camera => "The 3D camera: where it is, what it looks at, its field of view and depth of field.",
            EffectKind::Light => "A light for 3D layers.",
            EffectKind::Point => "An invisible point: layers parented to it move, turn and scale with it.",
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
            EffectKind::EnergyBlade => &BLADE,
            EffectKind::Camera => &CAMERA,
            EffectKind::Light => &LIGHT,
            EffectKind::Point => &POINT,
            // A plugin's parameters depend on which plugin: see `Effect::param_specs`.
            EffectKind::Plugin => &[],
        }
    }
}

impl EffectKind {
    /// Headings to show above some parameters, as (first parameter, heading).
    pub fn sections(self) -> &'static [(usize, &'static str)] {
        match self {
            EffectKind::EnergyBlade => &[(0, "Blade"), (5, "Colour"), (7, "Core"), (11, "Glow"), (15, "Flicker"), (18, "Motion")],
            EffectKind::Camera => &[(0, "Position"), (3, "Point of interest"), (6, "Lens"), (8, "Depth of field")],
            EffectKind::Point => &[(0, "Position"), (3, "Rotation and scale")],
            EffectKind::Light => &[(0, "Light"), (3, "Position"), (6, "Point of interest (spot and directional)"), (9, "Spot cone"), (11, "Falloff")],
            _ => &[],
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
    /// For a plugin effect, which plugin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin: Option<crate::plugin::PluginRef>,
}

/// An effect's parameters at one moment, ready for the GPU.
#[derive(Clone, Debug, PartialEq)]
pub struct EffectValues {
    pub kind: EffectKind,
    pub values: [f32; MAX_PARAMS],
    /// The lookup table for Curves and LUT effects.
    pub table: Option<Arc<Table>>,
    /// The plugin to run, for a plugin effect (`None` if it isn't installed).
    pub plugin: Option<Arc<crate::plugin::PluginDef>>,
    /// The clip's source time, in seconds (plugins can animate with it).
    pub time: f32,
    /// For an energy blade with motion blur: where its hilt and tip were one
    /// frame earlier (hilt x, y, tip x, y, in % of the layer).
    pub prev: Option<[f32; 4]>,
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
            // Missing plugins are skipped.
            EffectKind::Plugin => self.plugin.is_none(),
            EffectKind::EnergyBlade => v[blade::OPACITY] == 0.0 || v[blade::LENGTH] == 0.0 || (v[blade::CORE_WIDTH] == 0.0 && v[blade::GLOW_INTENSITY] == 0.0),
            // Settings of a camera or light clip, not drawn as an effect.
            EffectKind::Camera | EffectKind::Light | EffectKind::Point => true,
        }
    }
}

impl Effect {
    pub fn new(kind: EffectKind) -> Self {
        let data = match kind {
            EffectKind::Curves => Some(EffectData::Curves(Curves::default())),
            _ => None,
        };
        Self { kind, enabled: true, params: kind.params().iter().map(|s| Param { value: s.default, keys: Curve::default() }).collect(), data, plugin: None }
    }

    /// An effect running an installed plugin, with its default settings.
    pub fn new_plugin(def: &crate::plugin::PluginDef) -> Self {
        Self {
            kind: EffectKind::Plugin,
            enabled: true,
            params: def.params.iter().map(|s| Param { value: s.default, keys: Curve::default() }).collect(),
            data: None,
            plugin: Some(crate::plugin::PluginRef { id: def.id.clone(), name: def.name.clone() }),
        }
    }

    /// The installed plugin this effect runs (`None` for built-in effects, or
    /// a plugin that isn't installed).
    pub fn plugin_def(&self) -> Option<Arc<crate::plugin::PluginDef>> {
        self.plugin.as_ref().and_then(|p| crate::plugin::find(&p.id))
    }

    /// For a plugin effect: is its plugin installed?
    pub fn plugin_installed(&self) -> bool {
        self.plugin_def().is_some()
    }

    /// What to call it: the effect's name, or the plugin's.
    pub fn label(&self) -> String {
        match (self.kind, &self.plugin) {
            (EffectKind::Plugin, Some(p)) => self.plugin_def().map_or_else(|| p.name.clone(), |d| d.name.clone()),
            _ => self.kind.label().to_string(),
        }
    }

    /// Its parameters (for a plugin, the installed plugin's; none if missing).
    pub fn param_specs(&self) -> &'static [ParamSpec] {
        match self.kind {
            EffectKind::Plugin => self.plugin_def().map_or(&[], |d| d.params),
            k => k.params(),
        }
    }

    pub fn spec(&self, i: usize) -> Option<&'static ParamSpec> {
        self.param_specs().get(i)
    }

    /// Parameter `i` at source time `t`, clamped to its range. Missing
    /// parameters (an older project file) read as their default. Toggles and
    /// choices are whole numbers, even between keyframes.
    pub fn value_at(&self, i: usize, t: Time) -> f32 {
        let Some(s) = self.spec(i) else { return 0.0 };
        let Some(p) = self.params.get(i) else { return s.default };
        match s.ui {
            ParamUi::Number => p.keys.value_at(t, p.value).clamp(s.min, s.max),
            ParamUi::Color => {
                let keys: Curve<[f32; 3]> = Curve(p.keys.0.iter().map(|k| crate::anim::Key { at: k.at, value: unpack_rgb(k.value), interp: k.interp }).collect());
                pack_rgb(keys.value_at(t, unpack_rgb(p.value)))
            }
            // A checkbox or menu changes at its keyframes (holding the last key's
            // value until the next), never sliding through values in between.
            _ => {
                let k = &p.keys.0;
                let v = match k.partition_point(|k| k.at <= t) {
                    _ if k.is_empty() => p.value,
                    0 => k[0].value,
                    n => k[n - 1].value,
                };
                v.round().clamp(s.min, s.max)
            }
        }
    }

    pub fn eval(&self, t: Time) -> EffectValues {
        self.eval_moving(t, None)
    }

    /// [`Self::eval`], also noting where an energy blade was at `prev` (one
    /// frame earlier) for its motion blur.
    pub fn eval_moving(&self, t: Time, prev: Option<Time>) -> EffectValues {
        let prev = match (self.kind, prev) {
            (EffectKind::EnergyBlade, Some(p)) => Some([0, 1, 2, 3].map(|i| self.value_at(i, p))),
            _ => None,
        };
        let mut values = [0.0; MAX_PARAMS];
        for (i, v) in values.iter_mut().enumerate().take(self.param_specs().len()) {
            *v = self.value_at(i, t);
        }
        let plugin = if self.kind == EffectKind::Plugin { self.plugin_def() } else { None };
        EffectValues { kind: self.kind, values, table: self.table(), plugin, time: t.as_secs_f64() as f32, prev }
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
        // A plugin that isn't installed: keep what's there for when it is.
        if self.kind == EffectKind::Plugin && !self.plugin_installed() {
            return;
        }
        let specs = self.param_specs();
        self.params.truncate(specs.len());
        for s in &specs[self.params.len()..] {
            self.params.push(Param { value: s.default, keys: Curve::default() });
        }
    }
}

/// The enabled effects of a stack at source time `t`, skipping ones that do nothing.
pub fn eval_stack(effects: &[Effect], t: Time) -> Vec<EffectValues> {
    eval_stack_moving(effects, t, None)
}

/// [`eval_stack`], with `prev` the source time one frame earlier (for motion blur).
pub fn eval_stack_moving(effects: &[Effect], t: Time, prev: Option<Time>) -> Vec<EffectValues> {
    effects.iter().filter(|e| e.enabled).map(|e| e.eval_moving(t, prev)).filter(|v| !v.is_identity()).collect()
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
    fn colours_pack_and_interpolate_per_channel() {
        assert_eq!(unpack_rgb(pack_rgb([1.0, 0.0, 0.5])), [1.0, 0.0, 128.0 / 255.0]);
        assert_eq!(pack_rgb([1.0, 1.0, 1.0]), 16_777_215.0);
        let mut e = Effect::new(EffectKind::EnergyBlade);
        let p = &mut e.params[blade::BLADE_COLOR];
        p.keys.set(Time::from_secs_f64(0.0), 0xff0000 as f32);
        p.keys.set(Time::from_secs_f64(1.0), 0x0000ff as f32);
        // Halfway: half red, half blue (not the number halfway between the two codes).
        let mid = unpack_rgb(e.value_at(blade::BLADE_COLOR, Time::from_secs_f64(0.5)));
        assert!((mid[0] - 0.5).abs() < 0.01 && mid[1] == 0.0 && (mid[2] - 0.5).abs() < 0.01, "{mid:?}");
    }

    #[test]
    fn blade_notes_where_it_was_a_frame_ago() {
        let mut e = Effect::new(EffectKind::EnergyBlade);
        e.params[blade::TIP_X].keys.set(Time::from_secs_f64(0.0), 10.0);
        e.params[blade::TIP_X].keys.set(Time::from_secs_f64(1.0), 90.0);
        let v = &eval_stack_moving(std::slice::from_ref(&e), Time::from_secs_f64(0.5), Some(Time::from_secs_f64(0.25)))[0];
        assert_eq!(v.values[blade::TIP_X], 50.0);
        assert_eq!(v.prev.unwrap()[2], 30.0);
        assert!(Effect::new(EffectKind::Blur).eval_moving(Time::ZERO, Some(Time::ZERO)).prev.is_none());
        e.params[blade::LENGTH].value = 0.0;
        assert!(eval_stack(&[e], Time::ZERO).is_empty(), "a retracted blade draws nothing");
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
