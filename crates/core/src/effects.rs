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
pub const MAX_PARAMS: usize = 24;
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
    /// Makes the dark (or light) parts transparent: a black or white background.
    LumaKey,
    /// Makes transparent whatever matches a clean plate (a picture of the
    /// background alone, in `Effect::data`).
    DifferenceKey,
    /// Tidies a keyed layer: takes the screen's colour out of what's left
    /// (spill), fills small holes in the matte and removes specks.
    KeyCleaner,
    /// Shrinks or grows a layer's matte, softens it and firms up its
    /// half-transparent parts.
    MatteChoker,
    /// Smooths a jagged matte, moves and feathers its edge, and takes the
    /// screen's colour out of the see-through edge by borrowing it from inside.
    RefineMatte,
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
    /// An installed OpenFX plugin (which one is in `Effect::plugin`; see
    /// [`crate::ofx`]). Run on the CPU before the other effects.
    OpenFx,
    /// A glowing energy blade (a light sword) drawn from a hilt point to a tip
    /// point, with a hot core, coloured glow, flicker and motion blur.
    EnergyBlade,
    /// A particle emitter: sparks, dust, snow, embers, magic, worked out and
    /// drawn on the GPU (see [`crate::particles`]).
    Particles,
    /// A lens flare from a bright light: glow, streak, halo and ghosts.
    LensFlare,
    /// Handheld-style shake: the picture wanders, turns and zooms a little.
    CameraShake,
    /// Colour fringes: red and blue pulled apart towards the edges or one way.
    ChromaticAberration,
    /// Film grain, moving from frame to frame.
    FilmGrain,
    /// Digital glitches: sliced, shifted blocks, colour splits and scan lines.
    Glitch,
    /// Soft drifting blooms of coloured light, like film light leaks.
    LightLeaks,
    /// Glowing neon tubes traced along the layer's edges (its shape, or the
    /// edges in the picture), with a hot core, a glow and an optional flicker.
    NeonEdges,
    /// A print look: the picture as a screen of dots (or lines), in black and
    /// white, four-colour CMYK or dots of the picture's own colour.
    Halftone,
    /// An old screen or tape: the curved glass of a CRT, scan lines, a
    /// phosphor mask, VHS colour bleed, tracking wobble and snow.
    Crt,
    /// Paints out a thin straight line (a wire or rig) by filling it from
    /// the picture either side.
    WireRemoval,
    /// A layer breaks into irregular pieces at a moment, thrown out from an
    /// impact point and falling (see [`crate::shatter`]).
    Shatter,
    /// A gun's muzzle flash: a brief star of flame at the barrel, lighting
    /// the scene round it, once or as a burst of shots.
    MuzzleFlash,
    /// A bullet hitting a surface: a flash, sparks and a puff of dust, and
    /// the hole (or the cracks in glass) it leaves.
    BulletHit,
    /// Coloured smoke or ink poured into still air: a 2D fluid simulation
    /// (see [`crate::fluid`]).
    Fluid,
    /// The layer breaks up into particles over time, a ragged glowing front
    /// sweeping across it (see [`crate::disintegrate`]).
    Disintegrate,
    /// Fractal noise: clouds, smoke, liquid or marble patterns that evolve
    /// over time, drawn over the layer or used as its transparency.
    FractalNoise,
    /// Lightning: jagged, branching bolts between two points or all round
    /// one, struck afresh several times a second (see [`crate::lightning`]).
    Lightning,
    /// Patch replacer: an area covered with a patch from elsewhere in the
    /// picture, its light matched to the surroundings (or cloned as it is,
    /// or filled in from the edges). The source moves with the target.
    PatchReplacer,
    /// Object removal (content-aware fill): what "object to remove" masks
    /// cover, filled from the other frames or made up (see `crate::inpaint`),
    /// worked out in the background and kept on disk.
    ObjectRemoval,
    /// Paint: clone-stamp, brush and erase strokes painted in the Viewer,
    /// each for a frame, from there on, or the whole clip (see `crate::paint`).
    Paint,
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
    /// Camera shake, chromatic aberration, glitch, and plugins that say so.
    Distort,
    /// Wire removal and other clean-up.
    Repair,
    /// Particles, fire, smoke, explosions, weather, gunfire and fluid.
    Simulation,
    /// Patterns made from nothing: fractal noise, lightning.
    Generate,
    /// Plugins that didn't name a category.
    Plugins,
    /// OpenFX plugins (Sapphire, Neat Video, BorisFX, the OpenFX Misc set…).
    OpenFx,
}

impl EffectCategory {
    pub const ALL: [EffectCategory; 10] = [EffectCategory::Color, EffectCategory::BlurSharpen, EffectCategory::Keying, EffectCategory::Stylize, EffectCategory::Simulation, EffectCategory::Generate, EffectCategory::Distort, EffectCategory::Repair, EffectCategory::Plugins, EffectCategory::OpenFx];
    pub fn label(self) -> &'static str {
        match self {
            EffectCategory::Color => "Colour",
            EffectCategory::BlurSharpen => "Blur & sharpen",
            EffectCategory::Keying => "Keying",
            EffectCategory::Stylize => "Light & stylize",
            EffectCategory::Distort => "Distort",
            EffectCategory::Repair => "Repair",
            EffectCategory::Simulation => "Particles & simulation",
            EffectCategory::Generate => "Generate",
            EffectCategory::Plugins => "Other plugins",
            EffectCategory::OpenFx => "OpenFX",
        }
    }
}

/// Extra settings some effects have beyond numbers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum EffectData {
    Curves(Curves),
    /// A .cube file.
    Lut(PathBuf),
    /// A difference key's clean plate: the frame of a picture or video file
    /// at media time `at`.
    Plate { path: PathBuf, at: crate::Time },
    /// An environment light's panorama (see `crate::environment`).
    Environment(PathBuf),
    /// Object removal's filled frames: a folder (see `crate::fillstore`).
    Fill(PathBuf),
    /// Paint's strokes (see `crate::paint`).
    Paint(Vec<crate::paint::PaintStroke>),
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
/// Chroma key parameter indices past the first four (key hue, threshold,
/// softness, spill).
pub mod key {
    pub const CLIP_BLACK: usize = 4;
    pub const CLIP_WHITE: usize = 5;
    pub const CHOKE: usize = 6;
    pub const EDGE_SOFTNESS: usize = 7;
    pub const VIEW: usize = 8;
    /// `VIEW` values.
    pub const VIEW_RESULT: f32 = 0.0;
    pub const VIEW_MATTE: f32 = 1.0;

    use super::EffectKind;
    /// Where a keyer's shared matte settings (clip black, clip white, choke,
    /// edge softness, view) start among its parameters.
    pub fn matte_base(kind: EffectKind) -> Option<usize> {
        match kind {
            EffectKind::ChromaKey => Some(CLIP_BLACK),
            EffectKind::LumaKey => Some(3),
            EffectKind::DifferenceKey => Some(2),
            _ => None,
        }
    }

    /// Luma key parameters.
    pub const LUMA_MODE: usize = 0;
    pub const LUMA_THRESHOLD: usize = 1;
    pub const LUMA_SOFTNESS: usize = 2;
    /// Difference key parameters.
    pub const DIFF_TOLERANCE: usize = 0;
    pub const DIFF_SOFTNESS: usize = 1;

    /// The chroma key's colour plane (as the shader's `chroma`): Cb and Cr of
    /// a display-encoded (gamma) colour, Rec. 709.
    pub(crate) fn chroma([r, g, b]: [f32; 3]) -> [f32; 2] {
        let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        [(b - y) / 1.8556, (r - y) / 1.5748]
    }

    /// The colour of key hue `h`° (as the shader's `hue_rgb`).
    pub(crate) fn hue_rgb(h: f32) -> [f32; 3] {
        let k = |n: f32| (n + h / 60.0).rem_euclid(6.0);
        let f = |n: f32| 1.0 - k(n).min(4.0 - k(n)).clamp(0.0, 1.0);
        [f(5.0), f(3.0), f(1.0)]
    }

    /// What the eyedropper sets from the screen colour it picked
    /// (display-encoded RGB, 0–1, averaged over the area): the key hue that
    /// points straight at it, and a lower threshold and softness (scaled
    /// down together) if the current ones (in %) wouldn't take it all out (a
    /// dull or dark screen). `None` for a grey, which has no hue to key.
    pub fn pick(rgb: [f32; 3], threshold: f32, softness: f32) -> Option<(f32, Option<(f32, f32)>)> {
        let c = chroma(rgb);
        let len = (c[0] * c[0] + c[1] * c[1]).sqrt();
        if len < 0.02 {
            return None;
        }
        // The hue whose direction in the colour plane is closest (to 0.1°).
        let dir = |h: f32| {
            let k = chroma(hue_rgb(h));
            let n = (k[0] * k[0] + k[1] * k[1]).sqrt().max(1e-6);
            (c[0] * k[0] + c[1] * k[1]) / (n * len)
        };
        let coarse = (0..720).map(|i| i as f32 * 0.5).max_by(|a, b| dir(*a).total_cmp(&dir(*b))).unwrap_or(0.0);
        let hue = (-5..=5).map(|i| coarse + i as f32 * 0.1).max_by(|a, b| dir(*a).total_cmp(&dir(*b))).unwrap_or(coarse).rem_euclid(360.0);
        // Pointing straight at it, its strength is its length in the plane; the
        // key is fully see-through above threshold + softness (each half a unit
        // per 100 %). Keep a margin so the screen's darker parts go too.
        let top = (threshold + softness) / 200.0;
        let k = 0.8 * len / top.max(1e-6);
        let lower = (k < 1.0).then(|| ((threshold * k * 10.0).round() / 10.0, (softness * k * 10.0).round() / 10.0));
        Some(((hue * 10.0).round() / 10.0, lower))
    }
}

/// The matte settings every keyer ends with (see [`key::matte_base`]).
const MATTE: [ParamSpec; 5] = [
    // What's below Clip black becomes fully see-through, above Clip white fully solid.
    spec("Clip black", 0.0, 0.0, 99.0, " %", 0.2),
    spec("Clip white", 100.0, 1.0, 100.0, " %", 0.2),
    // + shrinks the matte (eats into the edge), − grows it.
    spec("Choke", 0.0, -20.0, 20.0, " px", 0.05),
    spec("Edge softness", 0.0, 0.0, 30.0, " px", 0.05),
    choice("View", &["Result", "Matte (black and white)"]),
];
const LUMA_KEY: [ParamSpec; 8] = [
    choice("Key out", &["Dark parts", "Light parts"]),
    spec("Threshold", 10.0, 0.0, 100.0, " %", 0.2),
    spec("Softness", 10.0, 0.0, 100.0, " %", 0.2),
    MATTE[0],
    MATTE[1],
    MATTE[2],
    MATTE[3],
    MATTE[4],
];
const DIFFERENCE_KEY: [ParamSpec; 7] = [
    spec("Tolerance", 8.0, 0.0, 100.0, " %", 0.2),
    spec("Softness", 8.0, 0.0, 100.0, " %", 0.2),
    MATTE[0],
    MATTE[1],
    MATTE[2],
    MATTE[3],
    MATTE[4],
];

const CHROMA_KEY: [ParamSpec; 9] = [
    spec("Key hue", 120.0, 0.0, 360.0, "°", 0.5),
    spec("Threshold", 12.0, 0.0, 100.0, " %", 0.2),
    spec("Softness", 10.0, 0.0, 100.0, " %", 0.2),
    spec("Spill", 60.0, 0.0, 100.0, " %", 0.5),
    MATTE[0],
    MATTE[1],
    MATTE[2],
    MATTE[3],
    MATTE[4],
];
const KEY_CLEANER: [ParamSpec; 6] = [
    spec("Screen hue", 120.0, 0.0, 360.0, "°", 0.5),
    spec("Spill removal", 80.0, 0.0, 100.0, " %", 0.5),
    spec("Spill tolerance", 2.0, 0.0, 50.0, " %", 0.1),
    spec("Fill holes", 0.0, 0.0, 20.0, " px", 0.05),
    spec("Remove specks", 0.0, 0.0, 20.0, " px", 0.05),
    choice("View", &["Result", "Matte (black and white)"]),
];
const MATTE_CHOKER: [ParamSpec; 5] = [
    spec("Choke", 1.0, -30.0, 30.0, " px", 0.05),
    spec("Softness", 1.0, 0.0, 30.0, " px", 0.05),
    spec("Matte contrast", 0.0, 0.0, 100.0, " %", 0.5),
    spec("Midpoint", 50.0, 1.0, 99.0, " %", 0.5),
    choice("View", &["Result", "Matte (black and white)"]),
];
const REFINE_MATTE: [ParamSpec; 6] = [
    spec("Smooth", 2.0, 0.0, 20.0, " px", 0.05),
    spec("Shift edge", 0.0, -20.0, 20.0, " px", 0.05),
    spec("Feather", 0.0, 0.0, 30.0, " px", 0.05),
    spec("Decontaminate colours", 80.0, 0.0, 100.0, " %", 0.5),
    spec("Colour from within", 6.0, 1.0, 30.0, " px", 0.05),
    choice("View", &["Result", "Matte (black and white)"]),
];
const WIRE_REMOVAL: [ParamSpec; 8] = [
    spec("Start X", 50.0, -50.0, 150.0, " %", 0.1),
    spec("Start Y", 10.0, -50.0, 150.0, " %", 0.1),
    spec("End X", 50.0, -50.0, 150.0, " %", 0.1),
    spec("End Y", 90.0, -50.0, 150.0, " %", 0.1),
    spec("Width", 6.0, 0.5, 200.0, " px", 0.05),
    spec("Edge softness", 2.0, 0.0, 50.0, " px", 0.05),
    spec("Fill from", 1.0, 0.0, 50.0, " px past the edge", 0.05),
    choice("Show area", &["Off", "On (red)"]),
];
const SHATTER: [ParamSpec; 12] = [
    spec("Impact X", 50.0, -50.0, 150.0, " %", 0.1),
    spec("Impact Y", 50.0, -50.0, 150.0, " %", 0.1),
    spec("Pieces across", 12.0, 2.0, 60.0, "", 0.05),
    spec("Breaks at", 0.5, 0.0, 3600.0, " s", 0.01),
    spec("Force", 900.0, 0.0, 8000.0, " px/s", 2.0),
    spec("Reach", 60.0, 1.0, 400.0, " %", 0.2),
    spec("Gravity", 1500.0, -5000.0, 5000.0, " px/s²", 2.0),
    spec("Spin", 400.0, 0.0, 3000.0, "°/s", 1.0),
    spec("Randomness", 50.0, 0.0, 100.0, " %", 0.5),
    spec("Fade after", 1.5, 0.0, 30.0, " s", 0.01),
    spec("Irregularity", 70.0, 0.0, 100.0, " %", 0.5),
    spec("Seed", 0.0, 0.0, 9999.0, "", 0.2),
];
const MUZZLE_FLASH: [ParamSpec; 12] = [
    spec("Barrel X", 40.0, -50.0, 150.0, " %", 0.1),
    spec("Barrel Y", 50.0, -50.0, 150.0, " %", 0.1),
    spec("Direction", 0.0, -360.0, 360.0, "°", 0.5),
    spec("Size", 100.0, 1.0, 600.0, " %", 0.5),
    choice("Gun", &muzzle::STYLES),
    color("Colour", 0xffb54a),
    spec("Brightness", 150.0, 0.0, 600.0, " %", 0.5),
    spec("First shot at", 0.2, 0.0, 3600.0, " s", 0.01),
    spec("Shots per second", 0.0, 0.0, 30.0, "", 0.05),
    spec("Shots", 1.0, 1.0, 500.0, "", 0.1),
    spec("Light on the scene", 60.0, 0.0, 300.0, " %", 0.5),
    spec("Seed", 0.0, 0.0, 9999.0, "", 0.2),
];
const BULLET_HIT: [ParamSpec; 10] = [
    spec("Hit X", 60.0, -50.0, 150.0, " %", 0.1),
    spec("Hit Y", 45.0, -50.0, 150.0, " %", 0.1),
    spec("Hits at", 0.5, 0.0, 3600.0, " s", 0.01),
    choice("Surface", &bullet::SURFACES),
    spec("Size", 100.0, 5.0, 600.0, " %", 0.5),
    spec("Sparks", 60.0, 0.0, 200.0, " %", 0.5),
    spec("Dust", 70.0, 0.0, 200.0, " %", 0.5),
    spec("Hole", 100.0, 0.0, 100.0, " %", 0.5),
    spec("Flash", 100.0, 0.0, 300.0, " %", 0.5),
    spec("Seed", 0.0, 0.0, 9999.0, "", 0.2),
];
const DISINTEGRATE: [ParamSpec; 17] = [
    spec("Direction", 0.0, -360.0, 360.0, "°", 0.5),
    spec("Starts at", 0.5, 0.0, 3600.0, " s", 0.01),
    spec("Takes", 2.0, 0.0, 600.0, " s", 0.01),
    spec("Roughness", 40.0, 0.0, 100.0, " %", 0.5),
    color("Edge glow", 0xff9a30),
    spec("Edge glow width", 4.0, 0.0, 50.0, " %", 0.1),
    spec("Particle size", 3.0, 0.2, 100.0, " px", 0.05),
    spec("Particles", 20000.0, 0.0, 60000.0, "", 10.0),
    spec("Drift speed", 120.0, 0.0, 3000.0, " px/s", 1.0),
    spec("Drift direction", 30.0, -360.0, 360.0, "°", 0.5),
    spec("Spread", 60.0, 0.0, 360.0, "°", 0.5),
    spec("Gravity", -40.0, -5000.0, 5000.0, " px/s²", 1.0),
    spec("Turbulence", 40.0, 0.0, 1000.0, " px", 0.5),
    spec("Lifetime", 2.0, 0.05, 20.0, " s", 0.01),
    color("Fade to", 0xff6a20),
    choice("Look", &crate::particles::LOOKS),
    spec("Seed", 0.0, 0.0, 9999.0, "", 0.2),
];
const FLUID: [ParamSpec; 16] = [
    spec("Source X", 50.0, -50.0, 150.0, " %", 0.1),
    spec("Source Y", 85.0, -50.0, 150.0, " %", 0.1),
    spec("Direction", 90.0, -360.0, 360.0, "°", 0.5),
    spec("Force", 350.0, 0.0, 4000.0, " px/s", 1.0),
    spec("Source size", 70.0, 2.0, 1000.0, " px", 0.5),
    spec("Amount", 100.0, 0.0, 500.0, " %", 0.5),
    color("Colour", 0x2a8cff),
    color("Second colour", 0xff3c78),
    spec("Colour change", 0.15, 0.0, 5.0, " /s", 0.01),
    spec("Rise", 60.0, -500.0, 500.0, " %", 0.5),
    spec("Swirl", 45.0, 0.0, 300.0, " %", 0.5),
    spec("Fade", 8.0, 0.0, 100.0, " %/s", 0.1),
    choice("Detail", &fluid_params::DETAILS),
    spec("Opacity", 100.0, 0.0, 100.0, " %", 0.5),
    spec("Seed", 0.0, 0.0, 9999.0, "", 0.2),
    choice("Flow round the layer's shape", &["Off", "On"]),
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
    /// 3D layers between the light and another layer shade it (1) or not (0).
    pub const SHADOWS: usize = 13;
    pub const SHADOW_DARKNESS: usize = 14;
    /// How soft the shadows are (the light's size), 0–100 %.
    pub const SHADOW_SOFTNESS: usize = 15;
    /// An environment light: its picture turned about the up axis (degrees).
    pub const ENV_TURN: usize = 16;
    /// How much shiny 3D text, shapes and models reflect it, 0–100 %.
    pub const REFLECTIONS: usize = 17;
    /// How blurred those reflections are, 0–100 %.
    pub const REFLECTION_BLUR: usize = 18;
    /// The environment type (lit by a panorama picture, see `crate::environment`).
    pub const ENVIRONMENT: u32 = 4;
    /// What the light kinds are called (the Type menu).
    pub const KINDS: [&str; 5] = ["Point", "Spot", "Directional", "Ambient", "Environment"];
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

const LIGHT: [ParamSpec; 19] = [
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
    ParamSpec { name: "Casts shadows", default: 1.0, min: 0.0, max: 1.0, suffix: "", speed: 1.0, ui: ParamUi::Toggle },
    spec("Shadow darkness", 80.0, 0.0, 100.0, " %", 0.5),
    spec("Shadow softness", 0.0, 0.0, 100.0, " %", 0.5),
    spec("Environment turn", 0.0, -360.0, 360.0, "°", 0.5),
    spec("Reflections", 30.0, 0.0, 100.0, " %", 0.5),
    spec("Reflection blur", 20.0, 0.0, 100.0, " %", 0.5),
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

const PARTICLES: [ParamSpec; 24] = [
    spec("Emitter X", 50.0, -100.0, 200.0, " %", 0.1),
    spec("Emitter Y", 70.0, -100.0, 200.0, " %", 0.1),
    choice("Shape", &crate::particles::SHAPES),
    spec("Emitter size", 120.0, 0.0, 8000.0, " px", 1.0),
    spec("Birth rate", 80.0, 0.0, 5000.0, " /s", 0.5),
    spec("Lifetime", 2.0, 0.05, 20.0, " s", 0.01),
    spec("Randomness", 40.0, 0.0, 100.0, " %", 0.5),
    spec("Speed", 300.0, 0.0, 5000.0, " px/s", 1.0),
    spec("Direction", 90.0, -360.0, 360.0, "°", 0.5),
    spec("Spread", 40.0, 0.0, 360.0, "°", 0.5),
    spec("Gravity", 250.0, -5000.0, 5000.0, " px/s²", 1.0),
    spec("Wind", 0.0, -5000.0, 5000.0, " px/s²", 1.0),
    spec("Turbulence", 0.0, 0.0, 1000.0, " px", 0.5),
    spec("Size at birth", 10.0, 0.0, 2000.0, " px", 0.1),
    spec("Size at death", 2.0, 0.0, 2000.0, " px", 0.1),
    color("Colour at birth", 0xffd27a),
    color("Colour at death", 0xff4a12),
    spec("Opacity", 100.0, 0.0, 100.0, " %", 0.5),
    choice("Look", &crate::particles::LOOKS),
    spec("Seed", 0.0, 0.0, 9999.0, "", 0.2),
    choice("Collide with", &crate::particles::COLLIDE_WITH),
    spec("Floor", 90.0, -100.0, 200.0, " %", 0.1),
    spec("Bounce", 35.0, 0.0, 100.0, " %", 0.5),
    spec("Friction", 30.0, 0.0, 100.0, " %", 0.5),
];

/// Parameter indices of the stylise effects.
pub mod flare {
    pub const X: usize = 0;
    pub const Y: usize = 1;
    pub const BRIGHTNESS: usize = 2;
    pub const SIZE: usize = 3;
    pub const COLOR: usize = 4;
    pub const STYLE: usize = 5;
    pub const STREAK: usize = 6;
    pub const GHOSTS: usize = 7;
    pub const HALO: usize = 8;
}
pub mod shake {
    pub const AMOUNT: usize = 0;
    pub const ROTATION: usize = 1;
    pub const SPEED: usize = 2;
    pub const ROUGHNESS: usize = 3;
    pub const ZOOM: usize = 4;
    pub const EDGES: usize = 5;
    pub const SEED: usize = 6;
}
pub mod aberration {
    pub const AMOUNT: usize = 0;
    pub const MODE: usize = 1;
    pub const ANGLE: usize = 2;
    pub const FALLOFF: usize = 3;
}
pub mod grain {
    pub const AMOUNT: usize = 0;
    pub const SIZE: usize = 1;
    pub const COLOR: usize = 2;
    pub const HIGHLIGHTS: usize = 3;
    pub const SPEED: usize = 4;
    pub const SEED: usize = 5;
}
/// Neon edges parameter indices.
pub mod neon {
    pub const SOURCE: usize = 0;
    pub const COLOR: usize = 1;
    pub const WIDTH: usize = 2;
    pub const GLOW_RADIUS: usize = 3;
    pub const GLOW_INTENSITY: usize = 4;
    pub const CORE: usize = 5;
    pub const THRESHOLD: usize = 6;
    pub const ORIGINAL: usize = 7;
    pub const FLICKER: usize = 8;
    pub const FLICKER_SPEED: usize = 9;
    /// Where the edges come from (the Edges menu).
    pub const SOURCES: [&str; 2] = ["Shape (transparency)", "Picture (brightness)"];
}
/// Key cleaner parameter indices.
pub mod cleaner {
    /// The screen's hue (green 120°, blue 240°).
    pub const HUE: usize = 0;
    pub const SPILL: usize = 1;
    /// Spill weaker than this is left alone (protects greys and skin).
    pub const TOLERANCE: usize = 2;
    pub const HOLES: usize = 3;
    pub const SPECKS: usize = 4;
    pub const VIEW: usize = 5;
}
/// Matte choker parameter indices.
pub mod choker {
    /// + shrinks the matte, − grows it (px).
    pub const CHOKE: usize = 0;
    pub const SOFTNESS: usize = 1;
    /// 0 leaves half-transparent parts as they are, 100 makes them solid or gone.
    pub const CONTRAST: usize = 2;
    pub const MIDPOINT: usize = 3;
    pub const VIEW: usize = 4;
}
/// Refine matte parameter indices.
pub mod refine {
    /// Blur-and-resharpen radius that rounds off jagged steps (px).
    pub const SMOOTH: usize = 0;
    /// + pulls the edge in, − pushes it out (px).
    pub const SHIFT: usize = 1;
    pub const FEATHER: usize = 2;
    /// How much of a see-through edge's colour comes from the solid inside.
    pub const DECONTAMINATE: usize = 3;
    pub const RADIUS: usize = 4;
    pub const VIEW: usize = 5;
}
/// Shatter parameter indices.
pub mod shatter {
    pub const X: usize = 0;
    pub const Y: usize = 1;
    pub const PIECES: usize = 2;
    pub const AT: usize = 3;
    pub const FORCE: usize = 4;
    /// How far from the impact pieces are thrown, % of the layer's width.
    pub const REACH: usize = 5;
    pub const GRAVITY: usize = 6;
    pub const SPIN: usize = 7;
    pub const RANDOMNESS: usize = 8;
    /// Seconds after breaking that thrown pieces have faded out (0: never).
    pub const FADE: usize = 9;
    pub const IRREGULAR: usize = 10;
    pub const SEED: usize = 11;
}
/// Muzzle flash parameter indices.
pub mod muzzle {
    pub const X: usize = 0;
    pub const Y: usize = 1;
    pub const DIRECTION: usize = 2;
    pub const SIZE: usize = 3;
    pub const STYLE: usize = 4;
    pub const COLOR: usize = 5;
    pub const BRIGHTNESS: usize = 6;
    pub const FIRST: usize = 7;
    /// 0: a single shot.
    pub const RATE: usize = 8;
    pub const SHOTS: usize = 9;
    pub const LIGHT: usize = 10;
    pub const SEED: usize = 11;
    pub const STYLES: [&str; 4] = ["Rifle", "Pistol", "Shotgun", "Side vents"];
    /// How long one flash lasts (s): about a frame.
    pub const LENGTH: f32 = 0.05;
    /// When the shots are (s), given first-shot time, rate and count.
    pub fn shot_times(first: f32, rate: f32, shots: f32) -> Vec<f32> {
        let n = if rate <= 0.0 { 1 } else { shots.round().max(1.0) as usize };
        (0..n).map(|k| first + if rate > 0.0 { k as f32 / rate } else { 0.0 }).collect()
    }
}
/// Bullet hit parameter indices.
pub mod bullet {
    pub const X: usize = 0;
    pub const Y: usize = 1;
    pub const AT: usize = 2;
    pub const SURFACE: usize = 3;
    pub const SIZE: usize = 4;
    pub const SPARKS: usize = 5;
    pub const DUST: usize = 6;
    pub const HOLE: usize = 7;
    pub const FLASH: usize = 8;
    pub const SEED: usize = 9;
    pub const SURFACES: [&str; 5] = ["Wall (plaster)", "Concrete", "Metal", "Glass", "Wood"];
}
/// Disintegrate parameter indices.
pub mod disintegrate {
    pub const DIRECTION: usize = 0;
    pub const START: usize = 1;
    pub const DURATION: usize = 2;
    pub const ROUGH: usize = 3;
    pub const GLOW: usize = 4;
    pub const GLOW_WIDTH: usize = 5;
    pub const SIZE: usize = 6;
    pub const COUNT: usize = 7;
    pub const SPEED: usize = 8;
    pub const DRIFT: usize = 9;
    pub const SPREAD: usize = 10;
    pub const GRAVITY: usize = 11;
    pub const TURBULENCE: usize = 12;
    pub const LIFETIME: usize = 13;
    pub const FADE_TO: usize = 14;
    pub const LOOK: usize = 15;
    pub const SEED: usize = 16;
}
/// Fluid parameter indices.
pub mod fluid_params {
    pub const X: usize = 0;
    pub const Y: usize = 1;
    pub const DIRECTION: usize = 2;
    pub const FORCE: usize = 3;
    pub const SIZE: usize = 4;
    pub const AMOUNT: usize = 5;
    pub const COLOR_A: usize = 6;
    pub const COLOR_B: usize = 7;
    pub const CYCLE: usize = 8;
    pub const RISE: usize = 9;
    pub const SWIRL: usize = 10;
    pub const FADE: usize = 11;
    pub const DETAIL: usize = 12;
    pub const OPACITY: usize = 13;
    pub const SEED: usize = 14;
    /// Flow round the layer's own shape (a title's letters).
    pub const OBSTACLES: usize = 15;
    pub const DETAILS: [&str; 3] = ["Low (fast)", "Medium", "High"];
}
/// Wire removal parameter indices.
pub mod wire {
    /// The wire's two ends, in % of the layer.
    pub const START_X: usize = 0;
    pub const START_Y: usize = 1;
    pub const END_X: usize = 2;
    pub const END_Y: usize = 3;
    pub const WIDTH: usize = 4;
    pub const SOFTNESS: usize = 5;
    /// How far past the wire's edge the fill is taken from (px).
    pub const GAP: usize = 6;
    pub const SHOW: usize = 7;
}
/// Fractal noise parameter indices.
pub mod fractal {
    pub const TYPE: usize = 0;
    pub const SCALE: usize = 1;
    pub const DETAIL: usize = 2;
    pub const ROUGHNESS: usize = 3;
    pub const CONTRAST: usize = 4;
    pub const BRIGHTNESS: usize = 5;
    /// Where it is in its evolution (°: a turn is one full change).
    pub const EVOLUTION: usize = 6;
    /// How fast it evolves (° a second).
    pub const SPEED: usize = 7;
    pub const DRIFT_X: usize = 8;
    pub const DRIFT_Y: usize = 9;
    pub const DARK: usize = 10;
    pub const LIGHT: usize = 11;
    pub const BLEND: usize = 12;
    pub const OPACITY: usize = 13;
    pub const SEED: usize = 14;
    pub const TYPES: [&str; 6] = ["Smooth", "Turbulent", "Ridged", "Clouds", "Liquid", "Marble"];
    pub const BLENDS: [&str; 6] = ["Replace the layer", "Screen over it", "Multiply", "Overlay", "Add", "As the layer's transparency"];
}
/// Lightning parameter indices.
pub mod lightning {
    pub const START_X: usize = 0;
    pub const START_Y: usize = 1;
    pub const END_X: usize = 2;
    pub const END_Y: usize = 3;
    pub const TYPE: usize = 4;
    pub const COUNT: usize = 5;
    pub const JAGGED: usize = 6;
    pub const DETAIL: usize = 7;
    pub const BRANCHES: usize = 8;
    pub const BRANCH_LENGTH: usize = 9;
    pub const CORE: usize = 10;
    pub const GLOW: usize = 11;
    pub const GLOW_INTENSITY: usize = 12;
    pub const CORE_COLOUR: usize = 13;
    pub const GLOW_COLOUR: usize = 14;
    /// New bolts a second (0: the same bolt throughout).
    pub const RATE: usize = 15;
    pub const FLICKER: usize = 16;
    /// How much of the bolt has reached out from the start (%).
    pub const GROW: usize = 17;
    /// The flash lighting the layer itself (%).
    pub const LIGHT: usize = 18;
    pub const OPACITY: usize = 19;
    pub const SEED: usize = 20;
    pub const TYPES: [&str; 2] = ["Bolt (start to end)", "Ball (all round the start)"];
}
/// Patch replacer parameter indices.
pub mod patch {
    /// The area covered (its centre, % of the layer).
    pub const X: usize = 0;
    pub const Y: usize = 1;
    /// Where the patch comes from, from the target (% of the layer).
    pub const SOURCE_X: usize = 2;
    pub const SOURCE_Y: usize = 3;
    pub const WIDTH: usize = 4;
    pub const HEIGHT: usize = 5;
    pub const ROTATION: usize = 6;
    pub const SHAPE: usize = 7;
    pub const SOFTNESS: usize = 8;
    pub const FILL: usize = 9;
    /// Outline the target (red) and the source (green) in the picture.
    pub const SHOW: usize = 10;
    pub const OPACITY: usize = 11;
    pub const SHAPES: [&str; 2] = ["Ellipse", "Rectangle"];
    pub const FILLS: [&str; 3] = ["Adaptive blend (match the light)", "Clone (copy as it is)", "Fill from the edges"];
}
/// Object removal parameter indices.
pub mod removal {
    pub const METHOD: usize = 0;
    /// How many frames each way are searched for what's behind.
    pub const REACH: usize = 1;
    /// Grow the area to fill past the mask (px), to cover shadows and edges.
    pub const GROW: usize = 2;
    /// A soft edge outside that (px).
    pub const FEATHER: usize = 3;
    pub const OPACITY: usize = 4;
    pub const METHODS: [&str; 3] = ["Object (from other frames, the rest made up)", "Surface (made up once, carried with the camera)", "Edge blend (smooth, from the edges)"];
}
/// Paint parameter indices: the settings for the next strokes.
pub mod paint_params {
    pub const TOOL: usize = 0;
    pub const SIZE: usize = 1;
    pub const HARDNESS: usize = 2;
    pub const OPACITY: usize = 3;
    pub const COLOUR: usize = 4;
    pub const LASTS: usize = 5;
    /// Where clone strokes copy from, relative to where they're painted (px).
    pub const OFFSET_X: usize = 6;
    pub const OFFSET_Y: usize = 7;
    pub const TOOLS: [&str; 3] = ["Clone", "Brush", "Erase"];
    pub const LASTS_FOR: [&str; 3] = ["This frame", "From here on", "The whole clip"];
}
/// Halftone parameter indices.
pub mod halftone {
    pub const STYLE: usize = 0;
    pub const SIZE: usize = 1;
    pub const ANGLE: usize = 2;
    pub const SHAPE: usize = 3;
    pub const CONTRAST: usize = 4;
    pub const INK: usize = 5;
    pub const PAPER: usize = 6;
    pub const MIX: usize = 7;
    /// The Style menu.
    pub const STYLES: [&str; 3] = ["Black & white", "CMYK print", "Colour dots"];
    /// The Shape menu.
    pub const SHAPES: [&str; 3] = ["Round dots", "Square dots", "Lines"];
}
/// CRT / VHS parameter indices.
pub mod crt {
    pub const CURVATURE: usize = 0;
    pub const SCANLINES: usize = 1;
    pub const SPACING: usize = 2;
    pub const MASK: usize = 3;
    pub const BLEED: usize = 4;
    pub const WOBBLE: usize = 5;
    pub const NOISE: usize = 6;
    pub const TRACKING: usize = 7;
    pub const VIGNETTE: usize = 8;
    pub const BRIGHTNESS: usize = 9;
    pub const SPEED: usize = 10;
    pub const SEED: usize = 11;
}
pub mod glitch {
    pub const AMOUNT: usize = 0;
    pub const BLOCK: usize = 1;
    pub const SHIFT: usize = 2;
    pub const SPLIT: usize = 3;
    pub const SCANLINES: usize = 4;
    pub const SPEED: usize = 5;
    pub const SEED: usize = 6;
}
pub mod leaks {
    pub const COLOR_A: usize = 0;
    pub const COLOR_B: usize = 1;
    pub const INTENSITY: usize = 2;
    pub const SIZE: usize = 3;
    pub const SPEED: usize = 4;
    pub const FROM: usize = 5;
    pub const SEED: usize = 6;
}

const LENS_FLARE: [ParamSpec; 9] = [
    spec("Light X", 30.0, -100.0, 200.0, " %", 0.1),
    spec("Light Y", 30.0, -100.0, 200.0, " %", 0.1),
    spec("Brightness", 100.0, 0.0, 500.0, " %", 0.5),
    spec("Size", 100.0, 10.0, 400.0, " %", 0.5),
    color("Colour", 0xffc890),
    choice("Lens", &["Warm (spherical)", "Anamorphic (blue streak)", "Glow only"]),
    spec("Streak", 50.0, 0.0, 200.0, " %", 0.5),
    spec("Ghosts", 60.0, 0.0, 200.0, " %", 0.5),
    spec("Halo", 40.0, 0.0, 200.0, " %", 0.5),
];
const CAMERA_SHAKE: [ParamSpec; 7] = [
    spec("Amount", 12.0, 0.0, 500.0, " px", 0.25),
    spec("Rotation", 0.8, 0.0, 45.0, "°", 0.05),
    spec("Speed", 3.0, 0.05, 30.0, " Hz", 0.05),
    spec("Roughness", 40.0, 0.0, 100.0, " %", 0.5),
    spec("Zoom", 104.0, 100.0, 200.0, " %", 0.1),
    choice("Edges", &["Mirror", "Repeat the edge", "Transparent"]),
    spec("Seed", 0.0, 0.0, 9999.0, "", 0.2),
];
const CHROMATIC: [ParamSpec; 4] = [
    spec("Amount", 6.0, -100.0, 100.0, " px", 0.1),
    choice("Direction", &["From the centre (lens)", "One way"]),
    spec("Angle", 0.0, -360.0, 360.0, "°", 0.5),
    spec("Falloff", 60.0, 0.0, 100.0, " %", 0.5),
];
const FILM_GRAIN: [ParamSpec; 6] = [
    spec("Amount", 25.0, 0.0, 200.0, " %", 0.5),
    spec("Size", 1.2, 0.5, 8.0, " px", 0.01),
    spec("Colour", 20.0, 0.0, 100.0, " %", 0.5),
    spec("In highlights", 40.0, 0.0, 100.0, " %", 0.5),
    spec("Frame rate", 24.0, 0.0, 120.0, " fps", 0.1),
    spec("Seed", 0.0, 0.0, 9999.0, "", 0.2),
];
const NEON: [ParamSpec; 10] = [
    choice("Edges from", &neon::SOURCES),
    color("Colour", 0x2ae6ff),
    spec("Tube width", 4.0, 0.0, 100.0, " px", 0.1),
    spec("Glow radius", 24.0, 0.0, 300.0, " px", 0.25),
    spec("Glow intensity", 150.0, 0.0, 500.0, " %", 0.5),
    spec("Hot core", 60.0, 0.0, 100.0, " %", 0.5),
    spec("Edge threshold", 15.0, 0.0, 100.0, " %", 0.5),
    spec("Original layer", 100.0, 0.0, 100.0, " %", 0.5),
    spec("Flicker", 0.0, 0.0, 100.0, " %", 0.5),
    spec("Flicker speed", 6.0, 0.0, 60.0, " Hz", 0.1),
];
const HALFTONE: [ParamSpec; 8] = [
    choice("Style", &halftone::STYLES),
    spec("Dot size", 10.0, 2.0, 200.0, " px", 0.1),
    spec("Angle", 45.0, -90.0, 90.0, "°", 0.25),
    choice("Shape", &halftone::SHAPES),
    spec("Contrast", 100.0, 0.0, 400.0, " %", 0.5),
    color("Ink", 0x161616),
    color("Paper", 0xf6f1e6),
    spec("Mix", 100.0, 0.0, 100.0, " %", 0.5),
];
const FRACTAL: [ParamSpec; 15] = [
    choice("Type", &fractal::TYPES),
    spec("Scale", 250.0, 4.0, 5000.0, " px", 1.0),
    spec("Detail", 6.0, 1.0, 10.0, "", 0.02),
    spec("Roughness", 50.0, 0.0, 100.0, " %", 0.5),
    spec("Contrast", 100.0, 0.0, 500.0, " %", 0.5),
    spec("Brightness", 0.0, -100.0, 100.0, " %", 0.5),
    spec("Evolution", 0.0, -36000.0, 36000.0, "°", 1.0),
    spec("Evolution speed", 45.0, -3600.0, 3600.0, "°/s", 0.5),
    spec("Drift X", 0.0, -5000.0, 5000.0, " px/s", 0.5),
    spec("Drift Y", 0.0, -5000.0, 5000.0, " px/s", 0.5),
    color("Dark colour", 0x000000),
    color("Light colour", 0xffffff),
    choice("Blend", &fractal::BLENDS),
    spec("Opacity", 100.0, 0.0, 100.0, " %", 0.5),
    spec("Seed", 0.0, 0.0, 9999.0, "", 0.2),
];
const LIGHTNING: [ParamSpec; 21] = [
    spec("Start X", 50.0, -50.0, 150.0, " %", 0.1),
    spec("Start Y", 5.0, -50.0, 150.0, " %", 0.1),
    spec("End X", 50.0, -50.0, 150.0, " %", 0.1),
    spec("End Y", 95.0, -50.0, 150.0, " %", 0.1),
    choice("Type", &lightning::TYPES),
    spec("Bolts", 1.0, 1.0, 24.0, "", 0.02),
    spec("Jaggedness", 50.0, 0.0, 300.0, " %", 0.5),
    spec("Detail", 7.0, 2.0, 10.0, "", 0.02),
    spec("Branches", 40.0, 0.0, 300.0, " %", 0.5),
    spec("Branch length", 45.0, 0.0, 200.0, " %", 0.5),
    spec("Core width", 3.0, 0.0, 60.0, " px", 0.05),
    spec("Glow size", 14.0, 0.0, 400.0, " px", 0.2),
    spec("Glow intensity", 100.0, 0.0, 500.0, " %", 0.5),
    color("Core colour", 0xffffff),
    color("Glow colour", 0x6f9dff),
    spec("Strikes a second", 10.0, 0.0, 60.0, "", 0.05),
    spec("Flicker", 50.0, 0.0, 100.0, " %", 0.5),
    spec("Grow", 100.0, 0.0, 100.0, " %", 0.5),
    spec("Light the layer", 0.0, 0.0, 300.0, " %", 0.5),
    spec("Opacity", 100.0, 0.0, 100.0, " %", 0.5),
    spec("Seed", 0.0, 0.0, 9999.0, "", 0.2),
];
const PATCH: [ParamSpec; 12] = [
    spec("Target X", 50.0, -50.0, 150.0, " %", 0.1),
    spec("Target Y", 50.0, -50.0, 150.0, " %", 0.1),
    spec("Source offset X", 20.0, -150.0, 150.0, " %", 0.1),
    spec("Source offset Y", 0.0, -150.0, 150.0, " %", 0.1),
    spec("Width", 120.0, 2.0, 4000.0, " px", 0.5),
    spec("Height", 120.0, 2.0, 4000.0, " px", 0.5),
    spec("Rotation", 0.0, -360.0, 360.0, "°", 0.5),
    choice("Shape", &patch::SHAPES),
    spec("Softness", 12.0, 0.0, 500.0, " px", 0.25),
    choice("Fill", &patch::FILLS),
    ParamSpec { name: "Show areas", default: 0.0, min: 0.0, max: 1.0, suffix: "", speed: 1.0, ui: ParamUi::Toggle },
    spec("Opacity", 100.0, 0.0, 100.0, " %", 0.5),
];
const OBJECT_REMOVAL: [ParamSpec; 5] = [
    choice("Method", &removal::METHODS),
    spec("Frames each way", 15.0, 0.0, 60.0, "", 0.1),
    spec("Grow", 4.0, 0.0, 100.0, " px", 0.1),
    spec("Feather", 3.0, 0.0, 50.0, " px", 0.1),
    spec("Opacity", 100.0, 0.0, 100.0, " %", 0.5),
];
const PAINT: [ParamSpec; 8] = [
    choice("Tool", &paint_params::TOOLS),
    spec("Size", 20.0, 1.0, 1000.0, " px", 0.25),
    spec("Hardness", 60.0, 0.0, 100.0, " %", 0.5),
    spec("Opacity", 100.0, 0.0, 100.0, " %", 0.5),
    color("Colour", 0xffffff),
    choice("Strokes last", &paint_params::LASTS_FOR),
    spec("Clone from X", -60.0, -10000.0, 10000.0, " px", 0.5),
    spec("Clone from Y", 0.0, -10000.0, 10000.0, " px", 0.5),
];
const CRT: [ParamSpec; 12] = [
    spec("Curvature", 30.0, 0.0, 100.0, " %", 0.5),
    spec("Scan lines", 50.0, 0.0, 100.0, " %", 0.5),
    spec("Line spacing", 4.0, 1.0, 40.0, " px", 0.05),
    spec("Phosphor mask", 20.0, 0.0, 100.0, " %", 0.5),
    spec("Colour bleed", 3.0, 0.0, 60.0, " px", 0.05),
    spec("Tracking wobble", 1.5, 0.0, 60.0, " px", 0.05),
    spec("Noise", 12.0, 0.0, 100.0, " %", 0.5),
    spec("Tracking band", 0.0, 0.0, 100.0, " %", 0.5),
    spec("Vignette", 40.0, 0.0, 100.0, " %", 0.5),
    spec("Brightness", 115.0, 50.0, 200.0, " %", 0.5),
    spec("Speed", 10.0, 0.0, 60.0, "", 0.1),
    spec("Seed", 0.0, 0.0, 9999.0, "", 0.2),
];
const GLITCH: [ParamSpec; 7] = [
    spec("Amount", 40.0, 0.0, 100.0, " %", 0.5),
    spec("Block size", 40.0, 2.0, 1000.0, " px", 0.5),
    spec("Shift", 60.0, 0.0, 2000.0, " px", 0.5),
    spec("Colour split", 8.0, 0.0, 200.0, " px", 0.1),
    spec("Scan lines", 25.0, 0.0, 100.0, " %", 0.5),
    spec("Speed", 10.0, 0.0, 60.0, " Hz", 0.1),
    spec("Seed", 0.0, 0.0, 9999.0, "", 0.2),
];
const LIGHT_LEAKS: [ParamSpec; 7] = [
    color("Colour 1", 0xff7a2a),
    color("Colour 2", 0xff2f6a),
    spec("Intensity", 70.0, 0.0, 300.0, " %", 0.5),
    spec("Size", 60.0, 5.0, 200.0, " %", 0.5),
    spec("Drift", 0.25, 0.0, 5.0, " Hz", 0.01),
    choice("From", &["Left", "Right", "Top", "Bottom", "Corners", "Anywhere"]),
    spec("Seed", 0.0, 0.0, 9999.0, "", 0.2),
];

const VIGNETTE: [ParamSpec; 3] = [
    spec("Amount", 50.0, 0.0, 100.0, " %", 0.5),
    spec("Size", 70.0, 0.0, 150.0, " %", 0.5),
    spec("Softness", 50.0, 1.0, 100.0, " %", 0.5),
];

impl EffectKind {
    pub const ALL: [EffectKind; 37] = [
        EffectKind::ColorCorrect,
        EffectKind::Levels,
        EffectKind::Curves,
        EffectKind::Lut,
        EffectKind::Blur,
        EffectKind::Sharpen,
        EffectKind::ChromaKey,
        EffectKind::LumaKey,
        EffectKind::DifferenceKey,
        EffectKind::KeyCleaner,
        EffectKind::MatteChoker,
        EffectKind::RefineMatte,
        EffectKind::Glow,
        EffectKind::DropShadow,
        EffectKind::Vignette,
        EffectKind::EnergyBlade,
        EffectKind::Particles,
        EffectKind::LensFlare,
        EffectKind::LightLeaks,
        EffectKind::NeonEdges,
        EffectKind::Halftone,
        EffectKind::Crt,
        EffectKind::WireRemoval,
        EffectKind::Shatter,
        EffectKind::MuzzleFlash,
        EffectKind::BulletHit,
        EffectKind::Fluid,
        EffectKind::Disintegrate,
        EffectKind::FractalNoise,
        EffectKind::Lightning,
        EffectKind::PatchReplacer,
        EffectKind::ObjectRemoval,
        EffectKind::Paint,
        EffectKind::FilmGrain,
        EffectKind::Glitch,
        EffectKind::ChromaticAberration,
        EffectKind::CameraShake,
    ];

    pub fn label(self) -> &'static str {
        match self {
            EffectKind::ColorCorrect => "Colour correction",
            EffectKind::Blur => "Gaussian blur",
            EffectKind::ChromaKey => "Chroma key",
            EffectKind::LumaKey => "Luma key",
            EffectKind::DifferenceKey => "Difference key",
            EffectKind::KeyCleaner => "Key cleaner",
            EffectKind::MatteChoker => "Matte choker",
            EffectKind::RefineMatte => "Refine matte",
            EffectKind::Vignette => "Vignette",
            EffectKind::Levels => "Levels",
            EffectKind::Curves => "Curves",
            EffectKind::Sharpen => "Sharpen",
            EffectKind::Glow => "Glow",
            EffectKind::DropShadow => "Drop shadow",
            EffectKind::Lut => "LUT (.cube)",
            EffectKind::Plugin => "Plugin",
            EffectKind::OpenFx => "OpenFX plugin",
            EffectKind::EnergyBlade => "Energy blade",
            EffectKind::Particles => "Particles",
            EffectKind::LensFlare => "Lens flare",
            EffectKind::CameraShake => "Camera shake",
            EffectKind::ChromaticAberration => "Chromatic aberration",
            EffectKind::FilmGrain => "Film grain",
            EffectKind::Glitch => "Glitch",
            EffectKind::LightLeaks => "Light leaks",
            EffectKind::NeonEdges => "Neon edges",
            EffectKind::Halftone => "Halftone",
            EffectKind::Crt => "CRT / VHS",
            EffectKind::WireRemoval => "Wire removal",
            EffectKind::Shatter => "Shatter",
            EffectKind::MuzzleFlash => "Muzzle flash",
            EffectKind::BulletHit => "Bullet hit",
            EffectKind::Fluid => "Fluid",
            EffectKind::Disintegrate => "Disintegrate",
            EffectKind::FractalNoise => "Fractal noise",
            EffectKind::Lightning => "Lightning",
            EffectKind::PatchReplacer => "Patch replacer",
            EffectKind::ObjectRemoval => "Object removal",
            EffectKind::Paint => "Paint / clone",
            EffectKind::Camera => "Camera",
            EffectKind::Light => "Light",
            EffectKind::Point => "Point",
        }
    }

    pub fn category(self) -> EffectCategory {
        match self {
            EffectKind::ColorCorrect | EffectKind::Levels | EffectKind::Curves | EffectKind::Lut => EffectCategory::Color,
            EffectKind::Blur | EffectKind::Sharpen => EffectCategory::BlurSharpen,
            EffectKind::ChromaKey | EffectKind::LumaKey | EffectKind::DifferenceKey | EffectKind::KeyCleaner | EffectKind::MatteChoker | EffectKind::RefineMatte => EffectCategory::Keying,
            EffectKind::Glow | EffectKind::DropShadow | EffectKind::Vignette | EffectKind::EnergyBlade | EffectKind::Camera | EffectKind::Light | EffectKind::Point => EffectCategory::Stylize,
            EffectKind::Particles | EffectKind::Shatter | EffectKind::MuzzleFlash | EffectKind::BulletHit | EffectKind::Fluid | EffectKind::Disintegrate => EffectCategory::Simulation,
            EffectKind::LensFlare | EffectKind::LightLeaks | EffectKind::FilmGrain | EffectKind::NeonEdges | EffectKind::Halftone | EffectKind::Crt => EffectCategory::Stylize,
            EffectKind::WireRemoval | EffectKind::PatchReplacer | EffectKind::ObjectRemoval | EffectKind::Paint => EffectCategory::Repair,
            EffectKind::FractalNoise | EffectKind::Lightning => EffectCategory::Generate,
            EffectKind::CameraShake | EffectKind::ChromaticAberration | EffectKind::Glitch => EffectCategory::Distort,
            EffectKind::Plugin => EffectCategory::Plugins,
            EffectKind::OpenFx => EffectCategory::OpenFx,
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
            EffectKind::ChromaKey => "Remove a green or blue screen: spill suppression, matte clipping, choke and edge softness, and a black-and-white matte view.",
            EffectKind::LumaKey => "Remove a black (or white) background by brightness: fire, smoke, sparks and light shot on black.",
            EffectKind::DifferenceKey => "Remove whatever matches a clean plate (a frame of the background alone), for a locked-off shot with no screen.",
            EffectKind::Glow => "Bright areas bloom out with a soft halo.",
            EffectKind::DropShadow => "A soft shadow behind a title, logo or keyed subject.",
            EffectKind::Vignette => "Darken the edges to draw the eye to the middle.",
            EffectKind::Plugin => "An installed plugin.",
            EffectKind::OpenFx => "An installed OpenFX plugin.",
            EffectKind::EnergyBlade => "A glowing light-sword blade with a hot core, flicker and motion blur. Drag its hilt and tip in the Viewer, or attach it to a track.",
            EffectKind::Particles => "An emitter of sparks, fire, smoke, rain, snow, fog, debris or magic, or of the layer's own pixels (atomic particles), drawn on the GPU. Drag the emitter in the Viewer; every setting can be keyframed.",
            EffectKind::Shatter => "The layer breaks into irregular pieces at a moment you choose, thrown out from the impact point (drag it in the Viewer), tumbling and falling. Pieces beyond its reach stay put.",
            EffectKind::MuzzleFlash => "A gun's flash at the barrel (drag it in the Viewer): rifle, pistol, shotgun or side vents, one shot or a burst, lighting the scene round it. Put it on an adjustment layer to light the footage.",
            EffectKind::BulletHit => "A bullet striking a wall, concrete, metal, glass or wood: a flash, sparks and a puff of dust, and the hole or cracks it leaves. Put it on an adjustment layer over the footage.",
            EffectKind::Disintegrate => "The layer breaks up into particles of its own colours, a ragged glowing front sweeping across it in the direction you choose, the pieces drifting away and fading.",
            EffectKind::FractalNoise => "Clouds, smoke, liquid, marble or turbulent patterns that drift and evolve: in your colours over the layer, or as its transparency (a smoky reveal, a texture for other effects).",
            EffectKind::Lightning => "Jagged, branching bolts of lightning or electricity between two points (drag them in the Viewer) or all round one, struck afresh several times a second, flickering and lighting the layer.",
            EffectKind::PatchReplacer => "Covers a blemish, a logo or a sign with a patch of the picture from beside it (drag both in the Viewer), its light matched to the surroundings so the join doesn't show. Keyframe or track the target: the source moves with it.",
            EffectKind::ObjectRemoval => "Content-aware fill: removes whatever a mask set to \"Object to remove\" covers (a person, a sign, a boom), filling it from the other frames where what's behind shows, or making it up from the picture round it. Worked out in the background over the clip.",
            EffectKind::Paint => "Paint in the Viewer: clone the picture from somewhere else (Alt-click where to copy from), brush on a colour, or erase strokes. Each stroke lasts a frame, from there on, or the whole clip.",
            EffectKind::Fluid => "Coloured smoke or ink poured from a source into still air, swirling and rising: a fluid simulation, worked out from the clip's start.",
            EffectKind::LensFlare => "A flare from a bright light: glow, streak, halo and ghost reflections across the frame. Drag the light in the Viewer.",
            EffectKind::CameraShake => "Handheld or impact shake: the picture wanders, turns and zooms a little, smoothly and never the same twice.",
            EffectKind::ChromaticAberration => "Colour fringes: red and blue pulled apart towards the edges, like a cheap lens, or one way for a glitchy look.",
            EffectKind::FilmGrain => "Film grain that changes every frame: fine or coarse, mono or colour, kept out of the highlights.",
            EffectKind::Glitch => "Digital glitches: slices and blocks jump sideways, colours split and scan lines flicker.",
            EffectKind::LightLeaks => "Soft blooms of warm coloured light drifting in from the edges, like light leaking into a film camera.",
            EffectKind::WireRemoval => "Paints out a wire, a rig or a scratch: drag its two ends in the Viewer and set the width, and it's filled from the picture either side. Keyframe the ends to follow a moving wire.",
            EffectKind::Crt => "An old TV or a worn tape: the bulging glass and scan lines of a CRT, a phosphor mask, VHS colour bleed, tracking wobble, a rolling tracking band and snow.",
            EffectKind::KeyCleaner => "Tidies a keyed layer, after the key: takes the screen's green or blue out of hair and edges (spill), fills small holes in the matte and wipes out stray specks.",
            EffectKind::MatteChoker => "Works on any layer's transparency (after a key, or a cut-out picture): shrinks or grows its edge, softens it, and firms up what's half see-through so a ragged matte holds together.",
            EffectKind::RefineMatte => "For hair and soft edges after a key: smooths a jagged matte, moves and feathers its edge, and replaces the screen colour showing through see-through edges with colour from just inside.",
            EffectKind::Halftone => "The picture printed as a screen of dots or lines, like a newspaper photo or a comic: black and white, four-colour CMYK, or dots of its own colour.",
            EffectKind::NeonEdges => "Glowing neon tubes along the edges of a title, logo or keyed subject (or the edges in the picture): a hot core, a coloured glow, and a flicker if you like.",
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
            EffectKind::LumaKey => &LUMA_KEY,
            EffectKind::DifferenceKey => &DIFFERENCE_KEY,
            EffectKind::KeyCleaner => &KEY_CLEANER,
            EffectKind::MatteChoker => &MATTE_CHOKER,
            EffectKind::RefineMatte => &REFINE_MATTE,
            EffectKind::Vignette => &VIGNETTE,
            EffectKind::Levels => &LEVELS,
            EffectKind::Curves => &CURVES,
            EffectKind::Sharpen => &SHARPEN,
            EffectKind::Glow => &GLOW,
            EffectKind::DropShadow => &DROP_SHADOW,
            EffectKind::Lut => &LUT,
            EffectKind::EnergyBlade => &BLADE,
            EffectKind::Particles => &PARTICLES,
            EffectKind::LensFlare => &LENS_FLARE,
            EffectKind::CameraShake => &CAMERA_SHAKE,
            EffectKind::ChromaticAberration => &CHROMATIC,
            EffectKind::FilmGrain => &FILM_GRAIN,
            EffectKind::Glitch => &GLITCH,
            EffectKind::LightLeaks => &LIGHT_LEAKS,
            EffectKind::NeonEdges => &NEON,
            EffectKind::Halftone => &HALFTONE,
            EffectKind::Crt => &CRT,
            EffectKind::WireRemoval => &WIRE_REMOVAL,
            EffectKind::Shatter => &SHATTER,
            EffectKind::MuzzleFlash => &MUZZLE_FLASH,
            EffectKind::BulletHit => &BULLET_HIT,
            EffectKind::Fluid => &FLUID,
            EffectKind::Disintegrate => &DISINTEGRATE,
            EffectKind::FractalNoise => &FRACTAL,
            EffectKind::Lightning => &LIGHTNING,
            EffectKind::PatchReplacer => &PATCH,
            EffectKind::ObjectRemoval => &OBJECT_REMOVAL,
            EffectKind::Paint => &PAINT,
            EffectKind::Camera => &CAMERA,
            EffectKind::Light => &LIGHT,
            EffectKind::Point => &POINT,
            // A plugin's parameters depend on which plugin: see `Effect::param_specs`.
            EffectKind::Plugin | EffectKind::OpenFx => &[],
        }
    }
}

impl EffectKind {
    /// Does the picture change with time even when the settings don't
    /// (flicker, shake, grain…)? Then every frame has to be drawn.
    pub fn animates_with_time(self) -> bool {
        matches!(self, EffectKind::EnergyBlade | EffectKind::CameraShake | EffectKind::FilmGrain | EffectKind::Glitch | EffectKind::LightLeaks | EffectKind::OpenFx | EffectKind::Shatter | EffectKind::MuzzleFlash | EffectKind::BulletHit | EffectKind::Fluid | EffectKind::Disintegrate | EffectKind::Lightning)
    }

    /// Headings to show above some parameters, as (first parameter, heading).
    pub fn sections(self) -> &'static [(usize, &'static str)] {
        match self {
            EffectKind::EnergyBlade => &[(0, "Blade"), (5, "Colour"), (7, "Core"), (11, "Glow"), (15, "Flicker"), (18, "Motion")],
            EffectKind::Particles => &[(0, "Emitter"), (4, "Birth"), (7, "Motion"), (13, "Look"), (20, "Collisions")],
            EffectKind::Camera => &[(0, "Position"), (3, "Point of interest"), (6, "Lens"), (8, "Depth of field")],
            EffectKind::Point => &[(0, "Position"), (3, "Rotation and scale")],
            EffectKind::ChromaKey => &[(0, "Key"), (4, "Matte")],
            EffectKind::LumaKey => &[(0, "Key"), (3, "Matte")],
            EffectKind::DifferenceKey => &[(0, "Key"), (2, "Matte")],
            EffectKind::KeyCleaner => &[(0, "Spill"), (3, "Matte")],
            EffectKind::RefineMatte => &[(0, "Edge"), (3, "Colour")],
            EffectKind::WireRemoval => &[(0, "Wire"), (4, "Fill")],
            EffectKind::Shatter => &[(0, "Impact"), (2, "Pieces"), (4, "Motion")],
            EffectKind::MuzzleFlash => &[(0, "Gun"), (5, "Flash"), (7, "Shots")],
            EffectKind::BulletHit => &[(0, "Hit"), (5, "Look")],
            EffectKind::Fluid => &[(0, "Source"), (6, "Colour"), (9, "Motion"), (12, "Look"), (15, "Obstacles")],
            EffectKind::Disintegrate => &[(0, "Sweep"), (4, "Edge"), (6, "Particles")],
            EffectKind::FractalNoise => &[(0, "Noise"), (6, "Motion"), (10, "Colour")],
            EffectKind::Paint => &[(0, "Brush"), (5, "Strokes"), (6, "Clone")],
            EffectKind::Lightning => &[(0, "Path"), (4, "Shape"), (10, "Look"), (15, "Motion")],
            EffectKind::PatchReplacer => &[(0, "Target"), (2, "Source"), (4, "Shape"), (9, "Fill")],
            EffectKind::Light => &[(0, "Light"), (3, "Position"), (6, "Point of interest (spot and directional)"), (9, "Spot cone"), (11, "Falloff"), (13, "Shadows"), (16, "Environment")],
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
    /// For a particle emitter: the particles alive now.
    pub particles: Option<Arc<crate::particles::Particles>>,
    /// For a shatter: its pieces now.
    pub pieces: Option<Arc<crate::shatter::Pieces>>,
    /// For a fluid: what to simulate on the graphics card.
    pub fluid: Option<Arc<crate::fluid::FluidJob>>,
    /// Spare values for the GPU: for lightning, the strike's brightness
    /// (0–1) and how many segments and runs of them its table holds; for
    /// object removal, where its patch sits (left, top, width, height as
    /// fractions of the layer).
    pub gpu: [f32; 4],
}

impl EffectValues {
    /// True when the effect wouldn't change the picture (so it can be skipped).
    pub fn is_identity(&self) -> bool {
        let v = &self.values;
        match self.kind {
            EffectKind::ColorCorrect => v[0] == 0.0 && v[1] == 100.0 && v[2] == 100.0 && v[3] == 0.0 && v[4] == 0.0,
            EffectKind::Blur => v[0] < 0.05,
            EffectKind::ChromaKey | EffectKind::LumaKey => false,
            // Without its clean plate it does nothing.
            EffectKind::DifferenceKey => self.table.is_none(),
            EffectKind::Vignette => v[0] == 0.0,
            EffectKind::Levels => v[0] == 0.0 && v[1] == 255.0 && v[2] == 1.0 && v[3] == 0.0 && v[4] == 255.0,
            EffectKind::Curves | EffectKind::Lut => self.table.is_none() || v[0] == 0.0,
            EffectKind::Sharpen => v[0] == 0.0,
            EffectKind::Glow => v[2] == 0.0,
            EffectKind::DropShadow => v[3] == 0.0,
            // Missing plugins are skipped.
            EffectKind::Plugin => self.plugin.is_none(),
            // Run on the CPU before the GPU effects (see `crate::ofx`).
            EffectKind::OpenFx => true,
            EffectKind::LensFlare => v[flare::BRIGHTNESS] == 0.0,
            EffectKind::CameraShake => v[shake::AMOUNT] == 0.0 && v[shake::ROTATION] == 0.0 && v[shake::ZOOM] == 100.0,
            EffectKind::ChromaticAberration => v[aberration::AMOUNT] == 0.0,
            EffectKind::FilmGrain => v[grain::AMOUNT] == 0.0,
            EffectKind::Glitch => v[glitch::AMOUNT] == 0.0,
            EffectKind::LightLeaks => v[leaks::INTENSITY] == 0.0,
            EffectKind::Halftone => v[halftone::MIX] == 0.0,
            EffectKind::WireRemoval => false,
            EffectKind::Shatter => self.pieces.is_none(),
            EffectKind::Disintegrate => self.particles.is_none(),
            EffectKind::FractalNoise => v[fractal::OPACITY] == 0.0,
            EffectKind::Lightning => v[lightning::OPACITY] == 0.0 || self.table.is_none(),
            EffectKind::PatchReplacer => v[patch::OPACITY] == 0.0,
            EffectKind::ObjectRemoval => v[removal::OPACITY] == 0.0 || self.table.is_none(),
            EffectKind::Paint => self.table.is_none(),
            EffectKind::MuzzleFlash => v[muzzle::BRIGHTNESS] == 0.0 && v[muzzle::LIGHT] == 0.0,
            EffectKind::BulletHit => false,
            EffectKind::Fluid => v[fluid_params::OPACITY] == 0.0 || self.fluid.is_none(),
            EffectKind::RefineMatte => v[refine::SMOOTH] < 0.01 && v[refine::SHIFT].abs() < 0.01 && v[refine::FEATHER] < 0.01 && v[refine::DECONTAMINATE] == 0.0 && v[refine::VIEW] == 0.0,
            EffectKind::MatteChoker => v[choker::CHOKE].abs() < 0.01 && v[choker::SOFTNESS] < 0.01 && v[choker::CONTRAST] == 0.0 && v[choker::VIEW] == 0.0,
            EffectKind::KeyCleaner => v[cleaner::SPILL] == 0.0 && v[cleaner::HOLES] < 0.01 && v[cleaner::SPECKS] < 0.01 && v[cleaner::VIEW] == 0.0,
            EffectKind::Crt => {
                v[crt::CURVATURE] == 0.0
                    && v[crt::SCANLINES] == 0.0
                    && v[crt::MASK] == 0.0
                    && v[crt::BLEED] == 0.0
                    && v[crt::WOBBLE] == 0.0
                    && v[crt::NOISE] == 0.0
                    && v[crt::TRACKING] == 0.0
                    && v[crt::VIGNETTE] == 0.0
                    && v[crt::BRIGHTNESS] == 100.0
            }
            EffectKind::NeonEdges => v[neon::WIDTH] == 0.0 && v[neon::GLOW_INTENSITY] == 0.0 && v[neon::ORIGINAL] == 100.0,
            EffectKind::Particles => v[crate::particles::OPACITY] == 0.0 || self.particles.as_ref().is_none_or(|p| p.list.is_empty()),
            EffectKind::EnergyBlade => v[blade::OPACITY] == 0.0 || v[blade::LENGTH] == 0.0 || (v[blade::CORE_WIDTH] == 0.0 && v[blade::GLOW_INTENSITY] == 0.0),
            // Settings of a camera or light clip, not drawn as an effect.
            EffectKind::Camera | EffectKind::Light | EffectKind::Point => true,
        }
    }
}

impl EffectValues {
    /// Does it change from one moment to the next with its settings left
    /// alone (its kind always does, or it's set to flicker)?
    pub fn moves_with_time(&self) -> bool {
        let v = &self.values;
        self.kind.animates_with_time()
            || (self.kind == EffectKind::NeonEdges && v[neon::FLICKER] > 0.0)
            || (self.kind == EffectKind::FractalNoise && (v[fractal::SPEED] != 0.0 || v[fractal::DRIFT_X] != 0.0 || v[fractal::DRIFT_Y] != 0.0))
            || (self.kind == EffectKind::Crt && (v[crt::WOBBLE] > 0.0 || v[crt::NOISE] > 0.0 || v[crt::TRACKING] > 0.0))
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

    /// An effect running an installed OpenFX plugin, with its default settings.
    pub fn new_ofx(def: &crate::ofx::OfxDef) -> Self {
        Self {
            kind: EffectKind::OpenFx,
            enabled: true,
            params: def.params.iter().map(|s| Param { value: s.default, keys: Curve::default() }).collect(),
            data: None,
            plugin: Some(crate::plugin::PluginRef { id: def.id.clone(), name: def.name.clone() }),
        }
    }

    /// The installed plugin this effect runs (`None` for built-in effects, or
    /// a plugin that isn't installed).
    pub fn plugin_def(&self) -> Option<Arc<crate::plugin::PluginDef>> {
        if self.kind != EffectKind::Plugin {
            return None;
        }
        self.plugin.as_ref().and_then(|p| crate::plugin::find(&p.id))
    }

    /// The installed OpenFX plugin this effect runs.
    pub fn ofx_def(&self) -> Option<Arc<crate::ofx::OfxDef>> {
        if self.kind != EffectKind::OpenFx {
            return None;
        }
        self.plugin.as_ref().and_then(|p| crate::ofx::find(&p.id))
    }

    /// For a plugin effect (either kind): is its plugin installed? True for
    /// built-in effects.
    pub fn plugin_installed(&self) -> bool {
        match self.kind {
            EffectKind::Plugin => self.plugin_def().is_some(),
            EffectKind::OpenFx => self.ofx_def().is_some(),
            _ => true,
        }
    }

    /// A plugin effect whose plugin isn't installed.
    pub fn plugin_missing(&self) -> bool {
        !self.plugin_installed()
    }

    /// What to call it: the effect's name, or the plugin's.
    pub fn label(&self) -> String {
        match (self.kind, &self.plugin) {
            (EffectKind::Plugin, Some(p)) => self.plugin_def().map_or_else(|| p.name.clone(), |d| d.name.clone()),
            (EffectKind::OpenFx, Some(p)) => self.ofx_def().map_or_else(|| p.name.clone(), |d| d.name.clone()),
            _ => self.kind.label().to_string(),
        }
    }

    /// Its parameters (for a plugin, the installed plugin's; none if missing).
    pub fn param_specs(&self) -> &'static [ParamSpec] {
        match self.kind {
            EffectKind::Plugin => self.plugin_def().map_or(&[], |d| d.params),
            EffectKind::OpenFx => self.ofx_def().map_or(&[], |d| d.params),
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
                let keys: Curve<[f32; 3]> = Curve(p.keys.0.iter().map(|k| crate::anim::Key { at: k.at, value: unpack_rgb(k.value), interp: k.interp, ease_out: k.ease_out, ease_in: k.ease_in }).collect());
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
        let particles = (self.kind == EffectKind::Particles).then(|| Arc::new(crate::particles::simulate(self, t)));
        let (mut table, mut gpu) = (self.table(), [0.0; 4]);
        if let (Some(EffectData::Fill(dir)), EffectKind::ObjectRemoval) = (&self.data, self.kind) {
            if let Some((tb, place)) = crate::fillstore::load(dir, t) {
                (table, gpu) = (Some(tb), place);
            }
        }
        EffectValues { kind: self.kind, values, table, plugin, time: t.as_secs_f64() as f32, prev, particles, pieces: None, fluid: None, gpu }
    }

    /// [`Self::eval_moving`] for a layer `size` pixels across: what needs
    /// the layer's shape (a shatter's pieces, a fluid's grid) too.
    pub fn eval_sized(&self, t: Time, prev: Option<Time>, size: [f32; 2]) -> EffectValues {
        let mut v = self.eval_moving(t, prev);
        match self.kind {
            EffectKind::Shatter => v.pieces = Some(Arc::new(crate::shatter::simulate(self, t, size))),
            EffectKind::Fluid => v.fluid = crate::fluid::job(self, t, size).map(Arc::new),
            EffectKind::Disintegrate => v.particles = Some(Arc::new(crate::disintegrate::simulate(self, t, size))),
            EffectKind::Paint => {
                if let Some(EffectData::Paint(strokes)) = &self.data {
                    if let Some((tb, segs, runs)) = crate::paint::table(strokes, t, size) {
                        v.table = Some(tb);
                        v.gpu = [segs as f32, runs as f32, 0.0, 0.0];
                    } else {
                        v.table = None;
                    }
                }
            }
            EffectKind::Lightning => {
                let b = crate::lightning::bolts(self, t, size);
                v.table = crate::lightning::table(&b, size);
                // The strike's brightness and the table's size, for the GPU (spare slots).
                v.gpu = [b.flash, b.segments.len() as f32, b.segments.len().div_ceil(crate::lightning::CHUNK) as f32, 0.0];
            }
            _ => {}
        }
        v
    }

    /// The lookup table this effect needs, if any (a curve, or a loaded .cube file).
    pub fn table(&self) -> Option<Arc<Table>> {
        match (&self.data, self.kind) {
            (Some(EffectData::Curves(c)), EffectKind::Curves) if !c.is_identity() => Some(Arc::new(c.table())),
            (Some(EffectData::Lut(path)), EffectKind::Lut) => crate::lut::load_cube(path).ok(),
            (Some(EffectData::Plate { path, at }), EffectKind::DifferenceKey) => crate::lut::load_plate(path, *at),
            _ => None,
        }
    }

    pub fn is_animated(&self) -> bool {
        self.params.iter().any(|p| p.keys.is_animated())
    }

    /// Make sure there's a `Param` for every parameter the kind defines.
    pub fn normalize(&mut self) {
        // A plugin that isn't installed: keep what's there for when it is.
        if self.plugin_missing() {
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

/// [`eval_stack_moving`] for a layer `size` pixels across (see [`Effect::eval_sized`]).
pub fn eval_stack_sized(effects: &[Effect], t: Time, prev: Option<Time>, size: [f32; 2]) -> Vec<EffectValues> {
    effects.iter().filter(|e| e.enabled).map(|e| e.eval_sized(t, prev, size)).filter(|v| !v.is_identity()).collect()
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
                // …or need a file first (a LUT, a clean plate).
                EffectKind::ColorCorrect | EffectKind::Levels | EffectKind::Curves | EffectKind::Lut | EffectKind::DifferenceKey | EffectKind::ObjectRemoval | EffectKind::Paint => assert!(v.is_identity(), "{kind:?}"),
                // Nothing has been born yet at the start, but soon is.
                EffectKind::Particles => assert!(v.is_identity() && !e.eval(Time::from_secs_f64(0.5)).is_identity()),
                // These need the layer's size (a shatter's pieces, a fluid's grid).
                EffectKind::Shatter | EffectKind::Fluid | EffectKind::Disintegrate | EffectKind::Lightning => assert!(v.is_identity() && !e.eval_sized(Time::from_secs_f64(0.5), None, [640.0, 360.0]).is_identity(), "{kind:?}"),
                _ => assert!(!v.is_identity(), "{kind:?} should do something by default"),
            }
        }
    }

    #[test]
    fn the_eyedropper_aims_the_key_at_the_picked_colour() {
        // A typical green screen and a blue one.
        let (h, t) = key::pick([50.0 / 255.0, 180.0 / 255.0, 80.0 / 255.0], 12.0, 10.0).unwrap();
        assert!((100.0..150.0).contains(&h), "green: {h}");
        assert_eq!(t, None, "a strong screen keeps the threshold");
        let (h, _) = key::pick([0.1, 0.25, 0.85], 12.0, 10.0).unwrap();
        assert!((200.0..250.0).contains(&h), "blue: {h}");
        // A pure primary lands on its own hue.
        let (h, _) = key::pick([0.0, 1.0, 0.0], 12.0, 10.0).unwrap();
        assert!((h - 120.0).abs() < 1.0, "{h}");
        // A dull, dark screen: the threshold comes down so it goes.
        let (_, t) = key::pick([0.18, 0.26, 0.19], 12.0, 10.0).unwrap();
        let (t, s) = t.expect("lowered");
        assert!(t < 12.0 && s < 10.0 && (t / s - 1.2).abs() < 0.1, "{t} {s}");
        // Grey has no hue.
        assert!(key::pick([0.5, 0.5, 0.5], 12.0, 10.0).is_none());
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
