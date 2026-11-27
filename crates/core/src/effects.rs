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
pub const MAX_PARAMS: usize = 32;
/// Most parameters a plugin may have (its shader sees all of them).
pub const PLUGIN_MAX_PARAMS: usize = MAX_PARAMS;

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
    /// Grade only a colour range: pick it by hue, saturation and luminance
    /// (or with the eyedropper), then shift its hue, saturation, brightness
    /// and contrast or tint it. See [`qualify`].
    HslQualifier,
    /// Hue vs hue, hue vs saturation, hue vs luminance, luminance vs
    /// saturation and saturation vs saturation curves (the points live in
    /// `Effect::data`; see [`crate::lut::HueCurves`]).
    HueCurves,
    /// Lift, gamma, gain and offset colour wheels (shadows, midtones,
    /// highlights, everything), with contrast, pivot and saturation, as in
    /// DaVinci Resolve's primaries. See [`wheels`].
    ColorWheels,
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
    /// A round area swelled out (or pinched in: a dent), or wrapped on a sphere.
    Bulge,
    /// The picture turned round a point, more in the middle (a vortex).
    Twirl,
    /// Rings spreading out from a point, as on water.
    Ripple,
    /// Waves across the picture (sine, square, triangle, sawtooth or wobbly).
    WaveWarp,
    /// The picture bent into a circle (a "tiny planet") or a circle unrolled.
    PolarCoordinates,
    /// One side of a line reflected onto the other.
    Mirror,
    /// Barrel or pincushion distortion: add a lens's bend, or take it out.
    LensDistortion,
    /// The sound as bars, a line, dots or a waveform (an audio visualiser).
    AudioSpectrum,
    /// A film look in one place, as Resolve's Film Look Creator.
    FilmLook,
    /// Old film.
    FilmDamage,
    /// Worn videotape.
    AnalogueDamage,
    /// Turn footage into a painted, simplified picture.
    Abstraction,
    /// A blur that splits light into its colours as a prism does.
    PrismBlur,
    /// The miniature look.
    TiltShift,
    /// Lines across the picture as on a screen.
    Scanlines,
    Strobe,
    GradientRamp,
    SmokeDistortion,
    FluidDistortion,
    EnergyDistortion,
    Derez,
    BlockDisplacement,
    BloodSpatter,
    BlasterFire,
    Teleport,
    ForceField,
    CustomFlare,
    Halation,
    Gleam,
    Spotlight,
    XRay,
    Infrared,
    ThermalCamera,
    NightVision,
    MotionBlur,
    RadialBlur,
    DirectionalBlur,
    /// Copies of the layer from moments before (or after), drawn with it: trails.
    Echo,
    /// The picture pushed, twirled, pinched and swelled with a brush in the Viewer.
    Liquify,
    /// The depth of the footage (worked out by the depth model): shown, as fog, or as a key.
    DepthMap,
    /// A light added to footage after it was shot, shading it by its depth.
    Relight,
    /// The background (or anything out of focus) blurred by its depth, as a portrait mode.
    PortraitBlur,
    /// Shafts of light through the air from a source, blocked by what's in front of it (by depth, or the dark parts).
    VolumetricLight,
    /// A magic trail of sparkles behind a moving point.
    SparkleTrail,
    /// A keyed subject picking up the background's light round its edges (applied as it's composited).
    LightWrap,
    /// Each part of the picture from a different moment (slit-scan), drawn as bands of copies by the scene.
    TimeDisplacement,
    /// The picture held on one frame for a while, its effects still moving (by the scene).
    HoldFrame,
    ElectricArcs,
    LightSweep,
    Hud,
    Portal,
    Hologram,
    Starfield,
    Shockwave,
    HeatHaze,
    TurbulentDisplace,
    /// Duotone / gradient map: the picture redrawn in two or three colours by its brightness.
    GradientMap,
    /// The northern lights: glowing curtains rippling across the sky.
    Aurora,
    /// Coloured clouds of gas and dust in space, with stars.
    Nebula,
    /// Soft clouds: a sky drifting by, or flying through them.
    Clouds,
    /// The layer burns away with glowing, charred edges and embers.
    BurnAway,
    /// The layer's picture made solid: its shape extruded into 3D, bevelled,
    /// turned and lit.
    Extrusion3d,
    /// Bends the layer like a sheet, round a vertical or horizontal axis.
    Bend,
    /// Bevelled edges: the picture's outline (or the layer's frame) lit as if raised.
    Bevel,
    /// Flat-style extrusion: the layer's shape drawn out in a direction or towards
    /// a point, its sides shaded.
    Extrude,
    /// Turns the layer like a card in 3D, with perspective.
    Rotate3d,
    /// A fast blur that averages each pixel with its neighbours in a box, as many times as you
    /// like: once is blocky, three times is nearly Gaussian.
    BoxBlur,
    /// Blurs the red, green, blue and alpha channels each by their own amount: soft colour
    /// fringes, a blurred matte under a sharp picture, or a dreamy red bloom.
    ChannelBlur,
    /// Blurs some places more than others, by a map: the layer's own brightness or opacity, or
    /// a gradient sharp in the middle, at the top, at the bottom or in a band (a quick depth of
    /// field).
    CompoundBlur,
    /// Blurs within areas of like colour but keeps the edges between them sharp: smooths skin,
    /// noise and texture without softening outlines.
    SmartBlur,
    /// A real lens's out-of-focus blur: the iris's shape (round or with blades) shows in bright
    /// spots, which bloom into bokeh, rather than a smooth Gaussian smear.
    CameraLensBlur,
    /// Blurs across and down separately and combines the two, as an average or added as light:
    /// a soft cross-shaped glow on highlights and text.
    CrossBlur,
    /// Smears the picture along its own contours (or across them), following the shapes of its
    /// light and dark: brushed, painterly and flowing looks.
    VectorBlur,
    /// Simple sums on each channel's 0–255 value with a number of its own: and, or, exclusive
    /// or, add, subtract, difference, minimum, maximum, block above or below, slice, multiply
    /// and screen.
    Arithmetic,
    /// Blends the layer with a colour: cross-faded, the colour only (keeping the layer's light
    /// and dark), a tint, or only where it darkens or lightens.
    BlendColour,
    /// Combines two of the layer's own channels with a blend mode — say its red multiplied by
    /// its luminance — into a new picture: for mattes, high-contrast looks and channel tricks.
    Calculations,
    /// Makes each output channel from a mix of the red, green and blue, plus a constant: swap
    /// channels, a custom black and white (monochrome), or an infrared look.
    ChannelMixer,
    /// Inverts the colours (a negative), or just one channel: red, green, blue, alpha,
    /// luminance, hue, lightness or saturation; blended with the original as much as you like.
    Invert,
    /// Takes each channel from another: red from blue, alpha from luminance, and so on (or
    /// full, half or off) — swapped colours, or a matte from the picture's own brightness.
    ShiftChannels,
    /// Sets each channel from one of the layer's own (or full, half or off), each inverted if
    /// you like, then put back together: build a matte, isolate a channel, or reverse one.
    SetChannels,
    /// Replaces the layer's alpha with one of its own channels — its luminance, red,
    /// saturation… — inverted if you like: a quick luma matte, or cut a layer out by its
    /// brightness.
    SetMatte,
    /// Puts the layer over a solid colour, each with an opacity of its own, with a blend mode:
    /// fills its see-through parts, or tints it by blending.
    SolidComposite,
    /// Each pixel takes the darkest (minimum) or brightest (maximum) value within a radius —
    /// spreading or shrinking light areas and mattes — or one then the other to close holes or
    /// remove specks.
    Minimax,
    /// Sets each channel's darkest and brightest points automatically (ignoring a few stray
    /// pixels at each end, the clip): full contrast and, as each channel is stretched on its
    /// own, a colour cast often taken out.
    AutoLevels,
    /// Stretches the picture's contrast automatically, all channels alike, so its darkest part
    /// is black and its brightest white — the colours stay as they are.
    AutoContrast,
    /// Takes out a colour cast automatically: each channel stretched to full range and the
    /// midtones made neutral grey.
    AutoColor,
    /// Spreads the picture's brightness levels evenly from black to white (histogram
    /// equalisation), each channel on its own or by brightness alone: hidden detail in flat,
    /// murky pictures.
    Equalize,
    /// Changes one colour: pick it, and shift its hue, lightness and saturation — the matching
    /// area found by a tolerance and softness, in RGB, by hue or by chroma; or show the mask it
    /// makes.
    ChangeColor,
    /// Changes one colour to another: everything near the From colour (by hue, lightness and
    /// saturation tolerances, softened) takes the To colour's hue — and its lightness and
    /// saturation too, if you choose.
    ChangeToColor,
    /// Shifts the balance of red, green and blue in the shadows, midtones and highlights
    /// separately, keeping the brightness if you like: warm the highlights, cool the shadows.
    ColorBalance,
    /// Adjusts the picture as a camera's exposure would, in stops of light (each doubling or
    /// halving it), with an offset for the blacks and a gamma correction for the midtones.
    Exposure,
    /// Takes the colour out of everything except one colour you pick (the red dress in a
    /// black-and-white world), with a tolerance and soft edges, matched by RGB or by hue.
    LeaveColor,
    /// A coloured filter over the lens: warming, cooling, the colours of the rainbow, sepia,
    /// deep tones or underwater — or a colour of your own — at a density, keeping the
    /// brightness if you like.
    PhotoFilter,
    /// Adjusts the amounts of cyan, magenta, yellow and black in particular colour ranges — the
    /// reds, yellows, greens, cyans, blues, magentas, whites, neutrals or blacks — as printers
    /// do: three ranges at once, relative or absolute.
    SelectiveColor,
    /// Maps the picture's blacks and whites to two colours of your choice (everything between,
    /// between them), blended in by an amount: a quick monochrome in any two colours.
    Tint,
    /// Turns the picture black and white with control over how light each colour becomes —
    /// reds, yellows, greens, cyans, blues and magentas — and an optional tint, as a
    /// photographer's coloured filters would.
    BlackWhite,
    /// Takes colour casts out of the shadows, midtones and highlights separately: pick the
    /// colour each should be neutral grey in (the cast), and it's balanced away.
    ColorNeutralizer,
    /// Cycles the red, green and blue values round by a phase each, wrapping over, solarising
    /// or folding back where they pass the end: psychedelic colour shifts that loop as you
    /// animate them.
    ColorOffset,
    /// Tones the picture in two, three or five colours by its brightness — highlights, brights,
    /// midtones, darktones and shadows — or solarised: duotones, sepia and split tones in a
    /// step.
    Toner,
    /// Turns each of red, green and blue fully on or off at a threshold of its own, giving at
    /// most eight flat colours: graphic, pop-art and stencil looks.
    ThresholdRgb,
    /// Each pixel becomes a weighted sum of itself and its eight neighbours — nine numbers of
    /// your choosing, divided and offset — the building block of sharpening, embossing, edge
    /// finding and blurs.
    Kernel,
    /// Keeps the picture within broadcast-safe limits: brightness held between a floor and a
    /// ceiling and saturation held below a maximum, by pulling down brightness, saturation or
    /// both.
    VideoLimiter,
    /// Evens out flicker: brightness and colour gains, keyed frame by frame by
    /// Deflicker's analysis (Clip menu), or set by hand.
    Deflicker,
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
    /// An open sea to the horizon: waves from the wind and a swell, foam on
    /// the crests, the sky and the sun on the water (see [`crate::water`]).
    Ocean,
    /// A water surface seen from above: ripples from rain and drops, waves
    /// rolling in, bouncing off and pushed by solid objects, caustics on the
    /// floor (a wave simulation, see [`crate::water`]).
    WaterSurface,
    /// Water poured, flowing and splashing (seen from the side): a liquid
    /// simulation that fills, falls and splashes off solid objects (see
    /// [`crate::water`]).
    Liquid,
    /// Fractal noise: clouds, smoke, liquid or marble patterns that evolve
    /// over time, drawn over the layer or used as its transparency.
    FractalNoise,
    /// Lightning: jagged, branching bolts between two points or all round
    /// one, struck afresh several times a second (see [`crate::lightning`]).
    Lightning,
    /// Light rays (god rays) streaming from a point through the bright parts
    /// of the layer, or the whole of it (HitFilm's Light Rays).
    LightRays,
    /// Long horizontal streaks from the bright parts of the layer, as an
    /// anamorphic lens draws them.
    AnamorphicFlare,
    /// Smudges and specks on the lens, showing where light hits them.
    LensDirt,
    /// A glowing neon tube along one of the layer's masks, written on and
    /// flickering as wanted. See [`crate::neonpath`].
    NeonPath,
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
    /// Smooths the skin of faces found and followed through the clip (or of
    /// all the skin in the picture), keeping their detail; or blurs or
    /// pixelates the faces (see `crate::faces`).
    FaceBeauty,
    /// Puts a new sky behind the footage: the sky found in each frame (in the
    /// background, by the object model), its edge softened, the new sky moved
    /// with the camera and the footage's light matched to it.
    SkyReplace,
    /// A camera clip's settings (not an effect you add: see [`crate::space3d`]).
    Camera,
    /// A light clip's settings (see [`crate::space3d`]).
    Light,
    /// A point clip's settings: an invisible point other layers can be
    /// parented to (see [`crate::space3d::Null`]).
    Point,
    /// Water poured in three dimensions: a liquid simulation in a box behind
    /// the layer, seen through a camera, so it can splash towards the camera
    /// and out of the picture (see [`crate::water::liquid3d_step`]).
    Liquid3D,
}

/// Where an effect is listed in the effects browser.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EffectCategory {
    Color,
    BlurSharpen,
    /// Channels moved, mixed, inverted and combined.
    Channel,
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
    /// The layer as a shape in 3D: bent, bevelled, extruded, turned.
    Geometry,
    /// Plugins that didn't name a category.
    Plugins,
    /// OpenFX plugins (Sapphire, Neat Video, BorisFX, the OpenFX Misc set…).
    OpenFx,
}

impl EffectCategory {
    pub const ALL: [EffectCategory; 12] = [EffectCategory::Color, EffectCategory::BlurSharpen, EffectCategory::Channel, EffectCategory::Keying, EffectCategory::Stylize, EffectCategory::Simulation, EffectCategory::Generate, EffectCategory::Geometry, EffectCategory::Distort, EffectCategory::Repair, EffectCategory::Plugins, EffectCategory::OpenFx];
    pub fn label(self) -> &'static str {
        match self {
            EffectCategory::Color => "Colour",
            EffectCategory::BlurSharpen => "Blur & sharpen",
            EffectCategory::Channel => "Channel",
            EffectCategory::Keying => "Keying",
            EffectCategory::Stylize => "Light & stylize",
            EffectCategory::Distort => "Distort",
            EffectCategory::Repair => "Repair",
            EffectCategory::Simulation => "Particles & simulation",
            EffectCategory::Generate => "Generate",
            EffectCategory::Geometry => "Geometry",
            EffectCategory::Plugins => "Other plugins",
            EffectCategory::OpenFx => "OpenFX",
        }
    }
}

/// Extra settings some effects have beyond numbers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum EffectData {
    Curves(Curves),
    /// The Hue curves effect's five curves.
    HueCurves(crate::lut::HueCurves),
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
    /// Liquify's strokes (see `crate::liquify`).
    Liquify(Vec<crate::liquify::LiquifyStroke>),
    /// Depth map and Relight: the footage's depth in each frame (a folder, see
    /// `crate::fillstore`; red and green hold it, 16 bits, bigger nearer).
    Depth(PathBuf),
    /// A HUD element's readout text (with live values in braces, see `crate::hud_text`).
    Text(String),
    /// Face beauty's faces, followed through the clip.
    Faces(Vec<crate::faces::FaceTrack>),
    /// Sky replacement: the new sky (a picture), and the sky found in each
    /// frame (a folder, see `crate::fillstore`; alpha is the sky).
    Sky { image: Option<PathBuf>, matte: Option<PathBuf> },
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
const FLUID: [ParamSpec; 20] = [
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
    choice("Shape to flow round", &fluid_params::SHAPES),
    choice("Body", &["Flat", "Billowing"]),
    spec("Light from", 60.0, -360.0, 360.0, "°", 0.5),
    spec("Shading", 60.0, 0.0, 100.0, " %", 0.5),
];
const OCEAN: [ParamSpec; 31] = [
    spec("Wind speed", 10.0, 0.5, 40.0, " m/s", 0.05),
    spec("Wind direction", 20.0, -360.0, 360.0, "°", 0.5),
    spec("Wave height", 100.0, 0.0, 400.0, " %", 0.5),
    spec("Choppiness", 70.0, 0.0, 150.0, " %", 0.5),
    spec("Swell", 40.0, 0.0, 300.0, " %", 0.5),
    spec("Swell direction", -30.0, -360.0, 360.0, "°", 0.5),
    spec("Ripples", 60.0, 0.0, 200.0, " %", 0.5),
    spec("Speed", 100.0, 0.0, 400.0, " %", 0.5),
    spec("Seed", 0.0, 0.0, 9999.0, "", 0.2),
    spec("Camera height", 4.0, 0.2, 300.0, " m", 0.05),
    spec("Look down", 7.0, -20.0, 89.0, "°", 0.1),
    spec("Field of view", 55.0, 10.0, 120.0, "°", 0.2),
    spec("Camera speed", 0.0, -60.0, 60.0, " m/s", 0.1),
    color("Deep water", 0x06283c),
    color("Light through the waves", 0x1f8a8c),
    spec("Foam", 60.0, 0.0, 200.0, " %", 0.5),
    spec("Sun height", 18.0, -10.0, 90.0, "°", 0.2),
    spec("Sun direction", 15.0, -180.0, 180.0, "°", 0.5),
    spec("Sun strength", 100.0, 0.0, 400.0, " %", 0.5),
    color("Sky", 0x4d86c6),
    color("Horizon", 0xc8d6e2),
    spec("Haze", 35.0, 0.0, 100.0, " %", 0.5),
    choice("Show", &ocean::SHOWS),
    spec("Opacity", 100.0, 0.0, 100.0, " %", 0.5),
    spec("Horizon softness", 30.0, 0.0, 100.0, " %", 0.5),
    choice("Solid objects", &water::SOLID_KINDS),
    spec("Key tolerance", 30.0, 0.0, 100.0, " %", 0.5),
    spec("White water", 100.0, 0.0, 400.0, " %", 0.5),
    spec("Spray", 100.0, 0.0, 400.0, " %", 0.5),
    spec("Wake", 100.0, 0.0, 400.0, " %", 0.5),
    spec("Waves bouncing off", 100.0, 0.0, 300.0, " %", 0.5),
];
const WATER_SURFACE: [ParamSpec; 31] = [
    spec("Rain", 6.0, 0.0, 400.0, " drops/s", 0.1),
    spec("Drop size", 14.0, 1.0, 300.0, " px", 0.2),
    spec("Drop strength", 100.0, 0.0, 400.0, " %", 0.5),
    spec("Drop X", 50.0, -50.0, 150.0, " %", 0.1),
    spec("Drop Y", 50.0, -50.0, 150.0, " %", 0.1),
    spec("A drop there every", 0.0, 0.0, 30.0, " s", 0.01),
    spec("Wave height", 0.0, 0.0, 400.0, " %", 0.5),
    spec("Wavelength", 120.0, 10.0, 2000.0, " px", 1.0),
    spec("Wave direction", 90.0, -360.0, 360.0, "°", 0.5),
    spec("Wind ripples", 15.0, 0.0, 200.0, " %", 0.5),
    spec("Wave speed", 100.0, 10.0, 100.0, " %", 0.5),
    spec("Calming", 15.0, 0.0, 100.0, " %", 0.5),
    choice("Detail", &water::DETAILS),
    spec("Wave scale", 100.0, 10.0, 400.0, " %", 0.5),
    spec("Seed", 0.0, 0.0, 9999.0, "", 0.2),
    choice("Solid objects", &water::SOLID_KINDS),
    spec("Key tolerance", 30.0, 0.0, 100.0, " %", 0.5),
    spec("Wake", 100.0, 0.0, 400.0, " %", 0.5),
    choice("The layer", &water::LAYER_KINDS),
    color("Water colour", 0x17607a),
    spec("Depth", 45.0, 0.0, 100.0, " %", 0.5),
    spec("Refraction", 100.0, 0.0, 400.0, " %", 0.5),
    spec("Caustics", 100.0, 0.0, 400.0, " %", 0.5),
    spec("Reflections", 60.0, 0.0, 200.0, " %", 0.5),
    spec("Light direction", 135.0, -360.0, 360.0, "°", 0.5),
    spec("Light height", 50.0, 5.0, 90.0, "°", 0.2),
    spec("Highlights", 100.0, 0.0, 400.0, " %", 0.5),
    color("Sky", 0xa8cbe8),
    color("Floor", 0xcdb98e),
    choice("Output", &water::OUTPUTS),
    spec("Opacity", 100.0, 0.0, 100.0, " %", 0.5),
];
const LIQUID: [ParamSpec; 30] = [
    spec("Source X", 20.0, -50.0, 150.0, " %", 0.1),
    spec("Source Y", 12.0, -50.0, 150.0, " %", 0.1),
    spec("Direction", -15.0, -360.0, 360.0, "°", 0.5),
    spec("Speed", 250.0, 0.0, 4000.0, " px/s", 1.0),
    spec("Width", 36.0, 2.0, 1000.0, " px", 0.5),
    spec("Flow", 100.0, 0.0, 400.0, " %", 0.5),
    spec("Pours from", 0.0, 0.0, 3600.0, " s", 0.01),
    spec("Pours for", 0.0, 0.0, 3600.0, " s", 0.01),
    spec("Fill", 0.0, 0.0, 95.0, " %", 0.5),
    spec("Gravity", 980.0, -5000.0, 5000.0, " px/s²", 1.0),
    spec("Thickness", 0.0, 0.0, 100.0, " %", 0.5),
    spec("Splashiness", 85.0, 0.0, 100.0, " %", 0.5),
    spec("Droplets", 100.0, 0.0, 300.0, " %", 0.5),
    choice("Frame edges", &liquid::EDGE_KINDS),
    choice("Detail", &liquid::DETAILS),
    spec("Seed", 0.0, 0.0, 9999.0, "", 0.2),
    choice("Solid objects", &water::SOLID_KINDS),
    spec("Key tolerance", 30.0, 0.0, 100.0, " %", 0.5),
    spec("Bounce", 20.0, 0.0, 100.0, " %", 0.5),
    color("Water colour", 0x2f8fd0),
    spec("Clearness", 65.0, 0.0, 100.0, " %", 0.5),
    spec("Refraction", 100.0, 0.0, 400.0, " %", 0.5),
    spec("Highlights", 100.0, 0.0, 400.0, " %", 0.5),
    spec("Foam", 60.0, 0.0, 200.0, " %", 0.5),
    spec("Smoothness", 50.0, 0.0, 100.0, " %", 0.5),
    spec("Light direction", 120.0, -360.0, 360.0, "°", 0.5),
    choice("Show", &liquid::SHOWS),
    spec("Opacity", 100.0, 0.0, 100.0, " %", 0.5),
    spec("Depth", 0.0, 0.0, 4000.0, " px", 1.0),
    spec("View from above", 25.0, -60.0, 60.0, "°", 0.2),
];
const LIQUID3D: [ParamSpec; 31] = [
    spec("Source X", 30.0, -50.0, 150.0, " %", 0.1),
    spec("Source Y", 20.0, -50.0, 150.0, " %", 0.1),
    spec("Source depth", 50.0, 0.0, 100.0, " %", 0.2),
    spec("Direction", -20.0, -360.0, 360.0, "°", 0.5),
    spec("Towards the camera", 35.0, -89.0, 89.0, "°", 0.2),
    spec("Speed", 400.0, 0.0, 4000.0, " px/s", 1.0),
    spec("Width", 50.0, 2.0, 1000.0, " px", 0.5),
    spec("Flow", 100.0, 0.0, 400.0, " %", 0.5),
    spec("Pours from", 0.0, 0.0, 3600.0, " s", 0.01),
    spec("Pours for", 0.0, 0.0, 3600.0, " s", 0.01),
    spec("Fill", 0.0, 0.0, 95.0, " %", 0.5),
    spec("Gravity", 980.0, -5000.0, 5000.0, " px/s²", 1.0),
    spec("Thickness", 0.0, 0.0, 100.0, " %", 0.5),
    spec("Splashiness", 85.0, 0.0, 100.0, " %", 0.5),
    spec("Droplets", 100.0, 0.0, 300.0, " %", 0.5),
    choice("Container", &liquid3d::CONTAINERS),
    choice("Detail", &liquid3d::DETAILS),
    spec("Seed", 0.0, 0.0, 9999.0, "", 0.2),
    choice("Solid objects", &water::SOLID_KINDS),
    spec("Key tolerance", 30.0, 0.0, 100.0, " %", 0.5),
    spec("Object thickness", 120.0, 0.0, 4000.0, " px", 1.0),
    spec("Bounce", 20.0, 0.0, 100.0, " %", 0.5),
    color("Water colour", 0x2f8fd0),
    spec("Clearness", 65.0, 0.0, 100.0, " %", 0.5),
    spec("Refraction", 100.0, 0.0, 400.0, " %", 0.5),
    spec("Highlights", 100.0, 0.0, 400.0, " %", 0.5),
    spec("Foam", 60.0, 0.0, 200.0, " %", 0.5),
    spec("Light direction", 120.0, -360.0, 360.0, "°", 0.5),
    spec("View from above", 12.0, -30.0, 60.0, "°", 0.2),
    choice("Show", &liquid3d::SHOWS),
    spec("Opacity", 100.0, 0.0, 100.0, " %", 0.5),
];
const LEVELS: [ParamSpec; 5] = [
    spec("Input black", 0.0, 0.0, 254.0, "", 0.5),
    spec("Input white", 255.0, 1.0, 255.0, "", 0.5),
    spec("Gamma", 1.0, 0.1, 5.0, "", 0.005),
    spec("Output black", 0.0, 0.0, 255.0, "", 0.5),
    spec("Output white", 255.0, 0.0, 255.0, "", 0.5),
];
const QUALIFIER: [ParamSpec; 20] = [
    spec("Hue", 30.0, 0.0, 360.0, "°", 0.5),
    spec("Hue width", 60.0, 0.0, 360.0, "°", 0.5),
    spec("Hue softness", 20.0, 0.0, 180.0, "°", 0.5),
    spec("Saturation low", 10.0, 0.0, 100.0, " %", 0.2),
    spec("Saturation high", 100.0, 0.0, 100.0, " %", 0.2),
    spec("Saturation softness", 10.0, 0.0, 100.0, " %", 0.2),
    spec("Luminance low", 5.0, 0.0, 100.0, " %", 0.2),
    spec("Luminance high", 100.0, 0.0, 100.0, " %", 0.2),
    spec("Luminance softness", 10.0, 0.0, 100.0, " %", 0.2),
    ParamSpec { name: "Use hue", default: 1.0, min: 0.0, max: 1.0, suffix: "", speed: 1.0, ui: ParamUi::Toggle },
    ParamSpec { name: "Use saturation", default: 1.0, min: 0.0, max: 1.0, suffix: "", speed: 1.0, ui: ParamUi::Toggle },
    ParamSpec { name: "Use luminance", default: 1.0, min: 0.0, max: 1.0, suffix: "", speed: 1.0, ui: ParamUi::Toggle },
    ParamSpec { name: "Invert", default: 0.0, min: 0.0, max: 1.0, suffix: "", speed: 1.0, ui: ParamUi::Toggle },
    choice("Show", &["The graded picture", "The matte (white is chosen)", "The chosen colours on grey"]),
    spec("Hue shift", 0.0, -180.0, 180.0, "°", 0.5),
    spec("Saturation", 100.0, 0.0, 300.0, " %", 0.5),
    spec("Brightness", 0.0, -100.0, 100.0, " %", 0.2),
    spec("Contrast", 100.0, 0.0, 300.0, " %", 0.5),
    color("Tint", 0xff8a3c),
    spec("Tint amount", 0.0, 0.0, 100.0, " %", 0.5),
];
const HUE_CURVES: [ParamSpec; 1] = [spec("Strength", 100.0, 0.0, 100.0, " %", 0.5)];
const WHEELS: [ParamSpec; 16] = [
    spec("Lift X", 0.0, -100.0, 100.0, " %", 0.25),
    spec("Lift Y", 0.0, -100.0, 100.0, " %", 0.25),
    spec("Lift", 0.0, -100.0, 100.0, " %", 0.2),
    spec("Gamma X", 0.0, -100.0, 100.0, " %", 0.25),
    spec("Gamma Y", 0.0, -100.0, 100.0, " %", 0.25),
    spec("Gamma", 0.0, -100.0, 100.0, " %", 0.2),
    spec("Gain X", 0.0, -100.0, 100.0, " %", 0.25),
    spec("Gain Y", 0.0, -100.0, 100.0, " %", 0.25),
    spec("Gain", 0.0, -100.0, 100.0, " %", 0.2),
    spec("Offset X", 0.0, -100.0, 100.0, " %", 0.25),
    spec("Offset Y", 0.0, -100.0, 100.0, " %", 0.25),
    spec("Offset", 0.0, -100.0, 100.0, " %", 0.2),
    spec("Saturation", 100.0, 0.0, 200.0, " %", 0.5),
    spec("Contrast", 100.0, 0.0, 200.0, " %", 0.5),
    spec("Pivot", 43.5, 0.0, 100.0, " %", 0.2),
    spec("Hue", 0.0, -180.0, 180.0, "°", 0.5),
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
    /// The lens library (the first three are the original lenses).
    pub const LENSES: [&str; 12] = [
        "Warm (spherical)",
        "Anamorphic (blue streak)",
        "Glow only",
        "Cinematic sun",
        "Clean prime",
        "Hexagon iris",
        "Sci-fi rainbow",
        "Red anamorphic",
        "Spotlight",
        "Vintage rings",
        "Green tech",
        "Starburst",
    ];
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
pub mod rays {
    pub const X: usize = 0;
    pub const Y: usize = 1;
    pub const LENGTH: usize = 2;
    pub const BRIGHTNESS: usize = 3;
    pub const THRESHOLD: usize = 4;
    pub const SOFTNESS: usize = 5;
    pub const COLOR: usize = 6;
    pub const FROM: usize = 7;
    pub const SHOW: usize = 8;
}
pub mod streak {
    pub const THRESHOLD: usize = 0;
    pub const LENGTH: usize = 1;
    pub const BRIGHTNESS: usize = 2;
    pub const SOFTNESS: usize = 3;
    pub const COLOR: usize = 4;
    pub const THICKNESS: usize = 5;
    pub const SHOW: usize = 6;
    pub const FROM: usize = 7;
    pub const X: usize = 8;
    pub const Y: usize = 9;
    pub const LIGHT_SIZE: usize = 10;
}
pub mod dirt {
    pub const X: usize = 0;
    pub const Y: usize = 1;
    pub const LIT_BY: usize = 2;
    pub const AMOUNT: usize = 3;
    pub const SPREAD: usize = 4;
    pub const THRESHOLD: usize = 5;
    pub const SMUDGE: usize = 6;
    pub const SPECKS: usize = 7;
    pub const COLOR: usize = 8;
    pub const SEED: usize = 9;
}
/// Neon path's parameters (10–14 and 18–19 sit where Lightning's do: the
/// same shader draws both).
pub mod neonpath {
    pub const MASK: usize = 0;
    pub const START: usize = 1;
    pub const END: usize = 2;
    pub const OFFSET: usize = 3;
    pub const CLOSED: usize = 4;
    pub const FLICKER: usize = 5;
    pub const FLICKER_SPEED: usize = 6;
    pub const TAPER: usize = 7;
    pub const SEED: usize = 8;
    pub const BRIGHTNESS: usize = 9;
    pub const CORE_WIDTH: usize = 10;
    pub const GLOW_SIZE: usize = 11;
    pub const GLOW: usize = 12;
    pub const CORE_COLOR: usize = 13;
    pub const GLOW_COLOR: usize = 14;
    pub const DASH: usize = 15;
    pub const GAP: usize = 16;
    pub const DASH_SPEED: usize = 17;
    pub const LIGHT_LAYER: usize = 18;
    pub const OPACITY: usize = 19;
    /// How much of the layer itself shows under the tube (0: only the tube, a sign at night).
    pub const SHOW_LAYER: usize = 20;
    /// Switch on: seconds from the clip's start until the last letter lights (0: always on).
    pub const SWITCH_ON: usize = 21;
    /// How much each letter stutters as it comes on (0: a quick fade up).
    pub const STUTTER: usize = 22;
    /// The order the letters light in.
    pub const ORDER: usize = 23;
    /// The Path menu: a title's letters, then the layer's masks.
    pub const PATHS: [&str; 17] = ["The letters (titles)", "Mask 1", "Mask 2", "Mask 3", "Mask 4", "Mask 5", "Mask 6", "Mask 7", "Mask 8", "Mask 9", "Mask 10", "Mask 11", "Mask 12", "Mask 13", "Mask 14", "Mask 15", "Mask 16"];
}
pub mod shake {
    pub const AMOUNT: usize = 0;
    pub const ROTATION: usize = 1;
    pub const SPEED: usize = 2;
    pub const ROUGHNESS: usize = 3;
    pub const ZOOM: usize = 4;
    pub const EDGES: usize = 5;
    pub const SEED: usize = 6;
    pub const STYLE: usize = 7;
    pub const SIDEWAYS: usize = 8;
    pub const UP_DOWN: usize = 9;
    pub const JOLT_AT: usize = 10;
    pub const JOLT: usize = 11;
    pub const BLUR: usize = 12;
}
pub mod film {
    pub const STOCK: usize = 0;
    pub const STRENGTH: usize = 1;
    pub const CONTRAST: usize = 2;
    pub const BLACKS: usize = 3;
    pub const ROLLOFF: usize = 4;
    pub const SATURATION: usize = 5;
    pub const HALATION: usize = 6;
    pub const HALATION_SIZE: usize = 7;
    pub const BLOOM: usize = 8;
    pub const GRAIN: usize = 9;
    pub const GRAIN_SIZE: usize = 10;
    pub const VIGNETTE: usize = 11;
    pub const WEAVE: usize = 12;
    pub const SEED: usize = 13;
    pub const STOCKS: [&str; 7] = ["Neutral print", "Warm print", "Cool print", "Bleach bypass", "Teal and orange", "Faded seventies", "Black and white"];
}
pub mod damage {
    pub const SCRATCHES: usize = 0;
    pub const SCRATCH_WIDTH: usize = 1;
    pub const DUST: usize = 2;
    pub const DUST_SIZE: usize = 3;
    pub const HAIRS: usize = 4;
    pub const FLICKER: usize = 5;
    pub const JITTER: usize = 6;
    pub const TINT: usize = 7;
    pub const VIGNETTE: usize = 8;
    pub const FRAME_RATE: usize = 9;
    pub const SEED: usize = 10;
}
pub mod analogue {
    pub const AMOUNT: usize = 0;
    pub const BLEED: usize = 1;
    pub const SHIFT: usize = 2;
    pub const TRACKING: usize = 3;
    pub const HEAD: usize = 4;
    pub const WOBBLE: usize = 5;
    pub const NOISE: usize = 6;
    pub const DROPOUTS: usize = 7;
    pub const SATURATION: usize = 8;
    pub const SOFTNESS: usize = 9;
    pub const SPEED: usize = 10;
    pub const SEED: usize = 11;
}
pub mod abstraction {
    pub const SIZE: usize = 0;
    pub const EDGES: usize = 1;
    pub const EDGE_WIDTH: usize = 2;
    pub const LEVELS: usize = 3;
    pub const SATURATION: usize = 4;
}
pub mod prism {
    pub const AMOUNT: usize = 0;
    pub const MODE: usize = 1;
    pub const ANGLE: usize = 2;
    pub const X: usize = 3;
    pub const Y: usize = 4;
    pub const SPREAD: usize = 5;
}
pub mod tilt {
    pub const FOCUS: usize = 0;
    pub const ANGLE: usize = 1;
    pub const BAND: usize = 2;
    pub const FALLOFF: usize = 3;
    pub const BLUR: usize = 4;
    pub const SATURATION: usize = 5;
    pub const CONTRAST: usize = 6;
}
pub mod scan {
    pub const AMOUNT: usize = 0;
    pub const SPACING: usize = 1;
    pub const THICKNESS: usize = 2;
    pub const SOFTNESS: usize = 3;
    pub const ANGLE: usize = 4;
    pub const ROLL: usize = 5;
    pub const MODE: usize = 6;
    pub const BRIGHTEN: usize = 7;
}
/// Audio spectrum: which sound, how it's drawn, along which line.
pub mod audiovis {
    pub const SOURCE: usize = 0;
    /// Bars, Line, Dots or Waveform.
    pub const DISPLAY: usize = 1;
    pub const BANDS: usize = 2;
    pub const START_X: usize = 3;
    pub const START_Y: usize = 4;
    pub const END_X: usize = 5;
    pub const END_Y: usize = 6;
    /// The tallest a band rises (% of the layer's height).
    pub const HEIGHT: usize = 7;
    /// How much of each band's width its bar fills (%).
    pub const THICKNESS: usize = 8;
    pub const LOW: usize = 9;
    pub const HIGH: usize = 10;
    pub const INSIDE: usize = 11;
    pub const OUTSIDE: usize = 12;
    pub const SENSITIVITY: usize = 13;
    pub const GLOW: usize = 14;
    /// Both sides of the line.
    pub const MIRROR: usize = 15;
    pub const SHOW_LAYER: usize = 16;
    pub const SOURCES: [&str; 9] = ["The clip's own sound", "Audio 1", "Audio 2", "Audio 3", "Audio 4", "Audio 5", "Audio 6", "Audio 7", "Audio 8"];
}
/// Bulge and Twirl: centre (% of the layer), radius (% of its height) and amount.
pub mod distort {
    pub const X: usize = 0;
    pub const Y: usize = 1;
    pub const RADIUS: usize = 2;
    pub const AMOUNT: usize = 3;
    /// Bulge: Bulge or Sphere. Twirl: how it fades out (Smooth or Even).
    pub const SHAPE: usize = 4;
    /// Bulge: the circle's width (an ellipse below or above 100 %).
    pub const WIDTH: usize = 5;
}
/// How a distortion fills in where it reaches past the layer.
pub const EDGES: [&str; 4] = ["Transparent", "Stretch", "Mirror", "Repeat"];
pub mod turbulent {
    pub const AMOUNT: usize = 0;
    pub const SIZE: usize = 1;
    pub const COMPLEXITY: usize = 2;
    pub const EVOLUTION: usize = 3;
    pub const SPEED: usize = 4;
    pub const OFFSET_X: usize = 5;
    pub const OFFSET_Y: usize = 6;
    pub const TYPE: usize = 7;
    pub const PIN: usize = 8;
    pub const EDGES: usize = 9;
}
pub mod haze {
    pub const STRENGTH: usize = 0;
    pub const SIZE: usize = 1;
    pub const RISE: usize = 2;
    pub const SHIMMER: usize = 3;
    pub const AREA: usize = 4;
    pub const HEIGHT: usize = 5;
    pub const X: usize = 6;
    pub const Y: usize = 7;
    pub const RADIUS: usize = 8;
    pub const DIRECTION: usize = 9;
    pub const EDGES: usize = 10;
}
pub mod shockwave {
    pub const X: usize = 0;
    pub const Y: usize = 1;
    pub const START: usize = 2;
    pub const SPEED: usize = 3;
    pub const WIDTH: usize = 4;
    pub const STRENGTH: usize = 5;
    pub const FADE: usize = 6;
    pub const REPEAT: usize = 7;
    pub const SPLIT: usize = 8;
    pub const BRIGHTNESS: usize = 9;
    pub const COLOR: usize = 10;
    pub const EDGES: usize = 11;
}
pub mod stars {
    pub const MODE: usize = 0;
    pub const AMOUNT: usize = 1;
    pub const SIZE: usize = 2;
    pub const SPEED: usize = 3;
    pub const DIRECTION: usize = 4;
    pub const X: usize = 5;
    pub const Y: usize = 6;
    pub const STREAK: usize = 7;
    pub const TWINKLE: usize = 8;
    pub const BRIGHTNESS: usize = 9;
    pub const COLOR: usize = 10;
    pub const VARIETY: usize = 11;
    pub const DRAW: usize = 12;
    pub const SEED: usize = 13;
}
pub mod gradient_map {
    pub const MODE: usize = 0;
    pub const SHADOWS: usize = 1;
    pub const MIDTONES: usize = 2;
    pub const HIGHLIGHTS: usize = 3;
    pub const BALANCE: usize = 4;
    pub const CONTRAST: usize = 5;
    pub const MIX: usize = 6;
}
pub mod aurora {
    pub const LOW: usize = 0;
    pub const HIGH: usize = 1;
    pub const HEIGHT: usize = 2;
    pub const SIZE: usize = 3;
    pub const BRIGHTNESS: usize = 4;
    pub const SPEED: usize = 5;
    pub const WAVES: usize = 6;
    pub const RAYS: usize = 7;
    pub const CURTAINS: usize = 8;
    pub const DRAW: usize = 9;
    pub const SEED: usize = 10;
}
pub mod nebula {
    pub const COLOR1: usize = 0;
    pub const COLOR2: usize = 1;
    pub const GLOW: usize = 2;
    pub const DENSITY: usize = 3;
    pub const SCALE: usize = 4;
    pub const DETAIL: usize = 5;
    pub const BRIGHTNESS: usize = 6;
    pub const SWIRL: usize = 7;
    pub const STARS: usize = 8;
    pub const DUST: usize = 9;
    pub const DRAW: usize = 10;
    pub const SEED: usize = 11;
}
pub mod clouds {
    pub const MODE: usize = 0;
    pub const COVERAGE: usize = 1;
    pub const SOFTNESS: usize = 2;
    pub const SCALE: usize = 3;
    pub const SPEED: usize = 4;
    pub const DIRECTION: usize = 5;
    pub const COLOR: usize = 6;
    pub const SHADE: usize = 7;
    pub const SKY_TOP: usize = 8;
    pub const SKY_LOW: usize = 9;
    pub const DRAW: usize = 10;
    pub const BRIGHTNESS: usize = 11;
    pub const SEED: usize = 12;
}
pub mod burn {
    pub const PROGRESS: usize = 0;
    pub const FROM: usize = 1;
    pub const X: usize = 2;
    pub const Y: usize = 3;
    pub const EDGE: usize = 4;
    pub const CHAR: usize = 5;
    pub const COLOR: usize = 6;
    pub const GLOW: usize = 7;
    pub const RAGGED: usize = 8;
    pub const EMBERS: usize = 9;
    pub const SEED: usize = 10;
}
pub mod extrusion {
    pub const DEPTH: usize = 0;
    pub const BEVEL: usize = 1;
    pub const BEVEL_SHAPE: usize = 2;
    pub const TURN_X: usize = 3;
    pub const TURN_Y: usize = 4;
    pub const TURN_Z: usize = 5;
    pub const SIZE: usize = 6;
    pub const FOV: usize = 7;
    pub const SIDES: usize = 8;
    pub const SIDE_COLOR: usize = 9;
    pub const LIGHT_ANGLE: usize = 10;
    pub const LIGHT_HEIGHT: usize = 11;
    pub const LIGHT: usize = 12;
    pub const AMBIENT: usize = 13;
    pub const SHINE: usize = 14;
    pub const BACK: usize = 15;
    pub const THRESHOLD: usize = 16;
}
pub mod bend {
    pub const AMOUNT: usize = 0;
    pub const AXIS: usize = 1;
    pub const CENTRE: usize = 2;
    pub const TOWARDS: usize = 3;
    pub const FOV: usize = 4;
    pub const SHADE: usize = 5;
    pub const BACK: usize = 6;
    pub const SIZE: usize = 7;
}
pub mod bevel {
    pub const WIDTH: usize = 0;
    pub const EDGES: usize = 1;
    pub const SHAPE: usize = 2;
    pub const LIGHT_ANGLE: usize = 3;
    pub const LIGHT_COLOR: usize = 4;
    pub const INTENSITY: usize = 5;
}
pub mod extrude {
    pub const LENGTH: usize = 0;
    pub const TOWARDS: usize = 1;
    pub const ANGLE: usize = 2;
    pub const X: usize = 3;
    pub const Y: usize = 4;
    pub const SIDES: usize = 5;
    pub const SIDE_COLOR: usize = 6;
    pub const SHADE: usize = 7;
    pub const FADE: usize = 8;
}
pub mod rotate3d {
    pub const TURN_X: usize = 0;
    pub const TURN_Y: usize = 1;
    pub const TURN_Z: usize = 2;
    pub const FOV: usize = 3;
    pub const SIZE: usize = 4;
    pub const X: usize = 5;
    pub const Y: usize = 6;
    pub const PUSH: usize = 7;
    pub const BACK: usize = 8;
}
pub mod box_blur {
    pub const RADIUS: usize = 0;
    pub const ITERATIONS: usize = 1;
    pub const DIMENSIONS: usize = 2;
    pub const REPEAT: usize = 3;
}
pub mod channel_blur {
    pub const RED: usize = 0;
    pub const GREEN: usize = 1;
    pub const BLUE: usize = 2;
    pub const ALPHA: usize = 3;
    pub const DIMENSIONS: usize = 4;
    pub const REPEAT: usize = 5;
}
pub mod compound_blur {
    pub const MAX: usize = 0;
    pub const MAP: usize = 1;
    pub const INVERT: usize = 2;
    pub const BAND: usize = 3;
}
pub mod smart_blur {
    pub const RADIUS: usize = 0;
    pub const THRESHOLD: usize = 1;
    pub const MODE: usize = 2;
}
pub mod lens_blur {
    pub const RADIUS: usize = 0;
    pub const IRIS: usize = 1;
    pub const ROTATION: usize = 2;
    pub const GAIN: usize = 3;
    pub const THRESHOLD: usize = 4;
    pub const REPEAT: usize = 5;
}
pub mod cross_blur {
    pub const ACROSS: usize = 0;
    pub const DOWN: usize = 1;
    pub const MODE: usize = 2;
}
pub mod vector_blur {
    pub const AMOUNT: usize = 0;
    pub const TYPE: usize = 1;
    pub const ANGLE: usize = 2;
    pub const SMOOTHNESS: usize = 3;
    pub const MAP: usize = 4;
}
pub mod arithmetic {
    pub const OPERATOR: usize = 0;
    pub const RED: usize = 1;
    pub const GREEN: usize = 2;
    pub const BLUE: usize = 3;
    pub const CLIP: usize = 4;
}
pub mod blend_colour {
    pub const COLOR: usize = 0;
    pub const MODE: usize = 1;
    pub const AMOUNT: usize = 2;
}
pub mod calculations {
    pub const INPUT: usize = 0;
    pub const INVERT_INPUT: usize = 1;
    pub const SECOND: usize = 2;
    pub const INVERT_SECOND: usize = 3;
    pub const OPACITY: usize = 4;
    pub const MODE: usize = 5;
    pub const PRESERVE: usize = 6;
}
pub mod channel_mixer {
    pub const RR: usize = 0;
    pub const RG: usize = 1;
    pub const RB: usize = 2;
    pub const RC: usize = 3;
    pub const GR: usize = 4;
    pub const GG: usize = 5;
    pub const GB: usize = 6;
    pub const GC: usize = 7;
    pub const BR: usize = 8;
    pub const BG: usize = 9;
    pub const BB: usize = 10;
    pub const BC: usize = 11;
    pub const MONO: usize = 12;
}
pub mod invert {
    pub const CHANNEL: usize = 0;
    pub const BLEND: usize = 1;
}
pub mod shift_channels {
    pub const RED: usize = 0;
    pub const GREEN: usize = 1;
    pub const BLUE: usize = 2;
    pub const ALPHA: usize = 3;
}
pub mod set_channels {
    pub const RED: usize = 0;
    pub const GREEN: usize = 1;
    pub const BLUE: usize = 2;
    pub const ALPHA: usize = 3;
    pub const INV_R: usize = 4;
    pub const INV_G: usize = 5;
    pub const INV_B: usize = 6;
    pub const INV_A: usize = 7;
}
pub mod set_matte {
    pub const FROM: usize = 0;
    pub const INVERT: usize = 1;
    pub const KEEP: usize = 2;
}
pub mod solid_composite {
    pub const SOURCE: usize = 0;
    pub const COLOR: usize = 1;
    pub const OPACITY: usize = 2;
    pub const MODE: usize = 3;
}
pub mod minimax {
    pub const OPERATION: usize = 0;
    pub const RADIUS: usize = 1;
    pub const CHANNEL: usize = 2;
    pub const DIRECTION: usize = 3;
}
pub mod auto_levels {
    pub const BLACK_CLIP: usize = 0;
    pub const WHITE_CLIP: usize = 1;
    pub const BLEND: usize = 2;
}
pub mod auto_contrast {
    pub const BLACK_CLIP: usize = 0;
    pub const WHITE_CLIP: usize = 1;
    pub const BLEND: usize = 2;
}
pub mod auto_color {
    pub const BLACK_CLIP: usize = 0;
    pub const WHITE_CLIP: usize = 1;
    pub const BLEND: usize = 2;
}
pub mod equalize {
    pub const STYLE: usize = 0;
    pub const AMOUNT: usize = 1;
}
pub mod change_color {
    pub const VIEW: usize = 0;
    pub const HUE: usize = 1;
    pub const LIGHTNESS: usize = 2;
    pub const SATURATION: usize = 3;
    pub const COLOR: usize = 4;
    pub const TOLERANCE: usize = 5;
    pub const SOFTNESS: usize = 6;
    pub const MATCH: usize = 7;
    pub const INVERT: usize = 8;
}
pub mod change_to_color {
    pub const FROM: usize = 0;
    pub const TO: usize = 1;
    pub const CHANGE: usize = 2;
    pub const HUE_TOL: usize = 3;
    pub const LIGHT_TOL: usize = 4;
    pub const SAT_TOL: usize = 5;
    pub const SOFTNESS: usize = 6;
    pub const VIEW: usize = 7;
}
pub mod color_balance {
    pub const SR: usize = 0;
    pub const SG: usize = 1;
    pub const SB: usize = 2;
    pub const MR: usize = 3;
    pub const MG: usize = 4;
    pub const MB: usize = 5;
    pub const HR: usize = 6;
    pub const HG: usize = 7;
    pub const HB: usize = 8;
    pub const PRESERVE: usize = 9;
}
pub mod exposure {
    pub const EXPOSURE: usize = 0;
    pub const OFFSET: usize = 1;
    pub const GAMMA: usize = 2;
}
pub mod leave_color {
    pub const AMOUNT: usize = 0;
    pub const COLOR: usize = 1;
    pub const TOLERANCE: usize = 2;
    pub const SOFTNESS: usize = 3;
    pub const MATCH: usize = 4;
}
pub mod photo_filter {
    pub const FILTER: usize = 0;
    pub const COLOR: usize = 1;
    pub const DENSITY: usize = 2;
    pub const PRESERVE: usize = 3;
}
pub mod selective_color {
    pub const METHOD: usize = 0;
    pub const R1: usize = 1;
    pub const C1: usize = 2;
    pub const M1: usize = 3;
    pub const Y1: usize = 4;
    pub const K1: usize = 5;
    pub const R2: usize = 6;
    pub const C2: usize = 7;
    pub const M2: usize = 8;
    pub const Y2: usize = 9;
    pub const K2: usize = 10;
    pub const R3: usize = 11;
    pub const C3: usize = 12;
    pub const M3: usize = 13;
    pub const Y3: usize = 14;
    pub const K3: usize = 15;
}
pub mod tint {
    pub const BLACK: usize = 0;
    pub const WHITE: usize = 1;
    pub const AMOUNT: usize = 2;
}
pub mod black_white {
    pub const REDS: usize = 0;
    pub const YELLOWS: usize = 1;
    pub const GREENS: usize = 2;
    pub const CYANS: usize = 3;
    pub const BLUES: usize = 4;
    pub const MAGENTAS: usize = 5;
    pub const TINT: usize = 6;
    pub const TINT_COLOR: usize = 7;
}
pub mod color_neutralizer {
    pub const SHADOWS: usize = 0;
    pub const MIDTONES: usize = 1;
    pub const HIGHLIGHTS: usize = 2;
    pub const BLEND: usize = 3;
}
pub mod color_offset {
    pub const RED: usize = 0;
    pub const GREEN: usize = 1;
    pub const BLUE: usize = 2;
    pub const OVERFLOW: usize = 3;
}
pub mod toner {
    pub const TONES: usize = 0;
    pub const HIGHLIGHTS: usize = 1;
    pub const BRIGHTS: usize = 2;
    pub const MIDTONES: usize = 3;
    pub const DARKTONES: usize = 4;
    pub const SHADOWS: usize = 5;
    pub const BLEND: usize = 6;
}
pub mod threshold_rgb {
    pub const RED: usize = 0;
    pub const GREEN: usize = 1;
    pub const BLUE: usize = 2;
    pub const INVERT_R: usize = 3;
    pub const INVERT_G: usize = 4;
    pub const INVERT_B: usize = 5;
    pub const BLEND: usize = 6;
}
pub mod kernel {
    pub const K11: usize = 0;
    pub const K12: usize = 1;
    pub const K13: usize = 2;
    pub const K21: usize = 3;
    pub const K22: usize = 4;
    pub const K23: usize = 5;
    pub const K31: usize = 6;
    pub const K32: usize = 7;
    pub const K33: usize = 8;
    pub const DIVIDE: usize = 9;
    pub const OFFSET: usize = 10;
    pub const ALPHA: usize = 11;
    pub const BLEND: usize = 12;
}
pub mod video_limiter {
    pub const REDUCE: usize = 0;
    pub const MAX_LUMA: usize = 1;
    pub const MIN_LUMA: usize = 2;
    pub const MAX_CHROMA: usize = 3;
}
pub mod deflicker {
    pub const BRIGHTNESS: usize = 0;
    pub const RED: usize = 1;
    pub const GREEN: usize = 2;
    pub const BLUE: usize = 3;
    pub const SMOOTHING: usize = 4;
    pub const COLOUR: usize = 5;
}
pub mod hologram {
    pub const COLOR: usize = 0;
    pub const OPACITY: usize = 1;
    pub const LINES: usize = 2;
    pub const SPACING: usize = 3;
    pub const LINE_SPEED: usize = 4;
    pub const FLICKER: usize = 5;
    pub const GLITCH: usize = 6;
    pub const EDGES: usize = 7;
    pub const SPLIT: usize = 8;
    pub const KEEP: usize = 9;
    pub const SEED: usize = 10;
}
pub mod portal {
    pub const X: usize = 0;
    pub const Y: usize = 1;
    pub const RADIUS: usize = 2;
    pub const OPEN: usize = 3;
    pub const RIM: usize = 4;
    pub const COLOR_A: usize = 5;
    pub const COLOR_B: usize = 6;
    pub const SPIN: usize = 7;
    pub const SPARKS: usize = 8;
    pub const INSIDE: usize = 9;
    pub const SWIRL: usize = 10;
    pub const RIPPLE: usize = 11;
    pub const BRIGHTNESS: usize = 12;
    pub const STRETCH: usize = 13;
    pub const SEED: usize = 14;
}
pub mod hud {
    pub const ELEMENT: usize = 0;
    pub const X: usize = 1;
    pub const Y: usize = 2;
    pub const SIZE: usize = 3;
    pub const COLOR: usize = 4;
    pub const WIDTH: usize = 5;
    pub const SPIN: usize = 6;
    pub const DETAIL: usize = 7;
    pub const DRAW_ON: usize = 8;
    pub const OPACITY: usize = 9;
    pub const FLICKER: usize = 10;
    pub const SEED: usize = 11;
    /// A soft glow round the lines.
    pub const GLOW: usize = 12;
    /// Faint scanlines inside the reticle styles.
    pub const FILL: usize = 13;
    /// How far the glow reaches, as a share of the element's radius.
    pub const GLOW_SIZE: usize = 14;
    /// The glow's own colour (when Own glow colour is on; otherwise the line colour).
    pub const GLOW_COLOR: usize = 15;
    pub const OWN_GLOW_COLOR: usize = 16;
    /// Lock-on: the element closes in on its target, turning and shrinking from
    /// big, over Lock-on time from Lock-on at (clip time), then blinks and turns the
    /// Locked colour.
    pub const LOCK_ON: usize = 17;
    pub const LOCK_AT: usize = 18;
    pub const LOCK_TIME: usize = 19;
    pub const LOCKED_COLOR: usize = 20;
    /// The readout text (the effect's data): where by the element, and how big
    /// (% of the layer's height).
    pub const TEXT_POS: usize = 21;
    pub const TEXT_SIZE: usize = 22;
}
pub mod sweep {
    pub const ANGLE: usize = 0;
    pub const WIDTH: usize = 1;
    pub const SOFTNESS: usize = 2;
    pub const START: usize = 3;
    pub const DURATION: usize = 4;
    pub const REPEAT: usize = 5;
    pub const POSITION: usize = 6;
    pub const BRIGHTNESS: usize = 7;
    pub const COLOR: usize = 8;
    pub const ON: usize = 9;
    pub const SECOND: usize = 10;
}
pub mod arcs {
    pub const COLOR: usize = 0;
    pub const AMOUNT: usize = 1;
    pub const REACH: usize = 2;
    pub const THICKNESS: usize = 3;
    pub const SPEED: usize = 4;
    pub const FLICKER: usize = 5;
    pub const GLOW: usize = 6;
    pub const BRIGHTNESS: usize = 7;
    pub const INSIDE: usize = 8;
    pub const SCALE: usize = 9;
    pub const SEED: usize = 10;
}
/// Echo: the layer drawn again from moments before (a positive delay) or after.
pub mod echo {
    pub const DELAY: usize = 0;
    pub const COUNT: usize = 1;
    pub const FIRST: usize = 2;
    pub const DECAY: usize = 3;
    pub const DRAW: usize = 4;
    pub const ORIGINAL: usize = 5;
    /// Most copies drawn.
    pub const MAX: usize = 16;
}
/// Liquify's parameters: the brush (used as strokes are painted) and how the
/// warp shows.
pub mod liquify_params {
    pub const TOOL: usize = 0;
    pub const SIZE: usize = 1;
    pub const PRESSURE: usize = 2;
    pub const AMOUNT: usize = 3;
    pub const EDGES: usize = 4;
    pub const MESH: usize = 5;
}
/// Depth map's parameters.
pub mod depth_map {
    pub const SHOW: usize = 0;
    pub const NEAR: usize = 1;
    pub const FAR: usize = 2;
    pub const INVERT: usize = 3;
    pub const FOG_COLOR: usize = 4;
    pub const FOG: usize = 5;
    pub const KEY: usize = 6;
    pub const SOFTNESS: usize = 7;
}
/// Relight's parameters (the light's position in % of the layer).
pub mod relight {
    pub const X: usize = 0;
    pub const Y: usize = 1;
    pub const HEIGHT: usize = 2;
    pub const COLOR: usize = 3;
    pub const INTENSITY: usize = 4;
    pub const DARKEN: usize = 5;
    pub const REACH: usize = 6;
    pub const BUMPS: usize = 7;
    pub const KIND: usize = 8;
}
pub mod dir_blur {
    pub const ANGLE: usize = 0;
    pub const LENGTH: usize = 1;
    pub const SHAPE: usize = 2;
}
pub mod radial_blur {
    pub const X: usize = 0;
    pub const Y: usize = 1;
    pub const TYPE: usize = 2;
    pub const AMOUNT: usize = 3;
    pub const QUALITY: usize = 4;
}
pub mod motion_blur {
    pub const SHUTTER: usize = 0;
    pub const PHASE: usize = 1;
    pub const SAMPLES: usize = 2;
    /// Values from here (not parameters): the two moments' maps (a, b, c, d,
    /// tx, ty each), filled in by the scene from the layer's movement.
    pub const FROM: usize = 3;
}
/// Portrait blur's parameters.
pub mod portrait {
    pub const FOCUS: usize = 0;
    pub const RANGE: usize = 1;
    pub const BLUR: usize = 2;
    pub const FRONT: usize = 3;
    pub const HIGHLIGHTS: usize = 4;
    pub const SHOW: usize = 5;
}
pub mod night_vision {
    pub const GAIN: usize = 0;
    pub const COLOR: usize = 1;
    pub const NOISE: usize = 2;
    pub const GLOW: usize = 3;
    pub const GOGGLES: usize = 4;
    pub const SOFTNESS: usize = 5;
    pub const SCANLINES: usize = 6;
    pub const FLICKER: usize = 7;
    pub const SEED: usize = 8;
}
pub mod thermal {
    pub const PALETTE: usize = 0;
    pub const HEAT_FROM: usize = 1;
    pub const COLDEST: usize = 2;
    pub const HOTTEST: usize = 3;
    pub const SMOOTH: usize = 4;
    pub const NOISE: usize = 5;
    pub const SCALE: usize = 6;
    pub const SEED: usize = 7;
}
pub mod infrared {
    pub const MODE: usize = 0;
    pub const FOLIAGE: usize = 1;
    pub const SKY: usize = 2;
    pub const GLOW: usize = 3;
    pub const CONTRAST: usize = 4;
    pub const GRAIN: usize = 5;
    pub const MIX: usize = 6;
    pub const SEED: usize = 7;
}
pub mod xray {
    pub const COLOR: usize = 0;
    pub const INVERT: usize = 1;
    pub const EDGES: usize = 2;
    pub const CONTRAST: usize = 3;
    pub const GLOW: usize = 4;
    pub const NOISE: usize = 5;
    pub const BRIGHTNESS: usize = 6;
    pub const SEED: usize = 7;
}
pub mod spotlight {
    pub const X: usize = 0;
    pub const Y: usize = 1;
    pub const SIZE: usize = 2;
    pub const SQUASH: usize = 3;
    pub const ANGLE: usize = 4;
    pub const SOFTNESS: usize = 5;
    pub const INTENSITY: usize = 6;
    pub const COLOR: usize = 7;
    pub const DARKEN: usize = 8;
    pub const BEAM: usize = 9;
    pub const SOURCE_X: usize = 10;
    pub const SOURCE_Y: usize = 11;
    pub const GOBO: usize = 12;
    pub const GOBO_SIZE: usize = 13;
    pub const GOBO_SPIN: usize = 14;
}
pub mod gleam {
    pub const THRESHOLD: usize = 0;
    pub const LENGTH: usize = 1;
    pub const POINTS: usize = 2;
    pub const ANGLE: usize = 3;
    pub const BRIGHTNESS: usize = 4;
    pub const COLOR: usize = 5;
    pub const OWN_COLOUR: usize = 6;
    pub const RAINBOW: usize = 7;
    pub const TWINKLE: usize = 8;
    pub const SHARPNESS: usize = 9;
    pub const SHOW: usize = 10;
}
pub mod halation {
    pub const THRESHOLD: usize = 0;
    pub const SIZE: usize = 1;
    pub const AMOUNT: usize = 2;
    pub const COLOR: usize = 3;
    pub const EDGES_ONLY: usize = 4;
}
pub mod custom_flare {
    pub const X: usize = 0;
    pub const Y: usize = 1;
    pub const BRIGHTNESS: usize = 2;
    pub const COLOR: usize = 3;
    pub const GLOW_SIZE: usize = 4;
    pub const GLOW: usize = 5;
    pub const RAYS: usize = 6;
    pub const RAYS_LENGTH: usize = 7;
    pub const RAYS_BRIGHT: usize = 8;
    pub const STREAK_LENGTH: usize = 9;
    pub const STREAK_BRIGHT: usize = 10;
    pub const STREAK_COLOR: usize = 11;
    pub const HALO_RADIUS: usize = 12;
    pub const HALO_BRIGHT: usize = 13;
    pub const RAINBOW: usize = 14;
    pub const GHOSTS: usize = 15;
    pub const GHOST_SIZE: usize = 16;
    pub const GHOST_SHAPE: usize = 17;
    pub const GHOST_BRIGHT: usize = 18;
    pub const SEED: usize = 19;
}
/// Volumetric light's parameters.
pub mod volumetric {
    pub const X: usize = 0;
    pub const Y: usize = 1;
    pub const LENGTH: usize = 2;
    pub const INTENSITY: usize = 3;
    pub const COLOR: usize = 4;
    pub const DEPTH: usize = 5;
    pub const THRESHOLD: usize = 6;
    pub const SOFTNESS: usize = 7;
    pub const HAZE: usize = 8;
    pub const DECAY: usize = 9;
    pub const SHOW: usize = 10;
}
pub mod force_field {
    pub const X: usize = 0;
    pub const Y: usize = 1;
    pub const RADIUS: usize = 2;
    pub const ROUNDNESS: usize = 3;
    pub const COLOR: usize = 4;
    pub const RIM: usize = 5;
    pub const FILL: usize = 6;
    pub const PATTERN: usize = 7;
    pub const PATTERN_SIZE: usize = 8;
    pub const PATTERN_BRIGHT: usize = 9;
    pub const BEND: usize = 10;
    pub const SHIMMER: usize = 11;
    pub const POWER: usize = 12;
    pub const HIT_AT: usize = 13;
    pub const HIT_ANGLE: usize = 14;
    pub const HIT_SIZE: usize = 15;
}
pub mod teleport {
    pub const DIRECTION: usize = 0;
    pub const START: usize = 1;
    pub const DURATION: usize = 2;
    pub const STYLE: usize = 3;
    pub const COLOR: usize = 4;
    pub const GLOW: usize = 5;
    pub const SPARKLES: usize = 6;
    pub const COLUMN: usize = 7;
    pub const SEED: usize = 8;
}
/// Sparkle trail's parameters (see `crate::sparkle`).
pub mod sparkle {
    pub const HEAD_X: usize = 0;
    pub const HEAD_Y: usize = 1;
    pub const PATH: usize = 2;
    pub const PROGRESS: usize = 3;
    pub const TRAIL: usize = 4;
    pub const AMOUNT: usize = 5;
    pub const SIZE: usize = 6;
    pub const SPREAD: usize = 7;
    pub const FALL: usize = 8;
    pub const TWINKLE: usize = 9;
    pub const COLOR: usize = 10;
    pub const COLOR2: usize = 11;
    pub const CORE: usize = 12;
    pub const CORE_BRIGHT: usize = 13;
    pub const BRIGHTNESS: usize = 14;
    pub const SEED: usize = 15;
}
pub mod blaster {
    pub const FROM_X: usize = 0;
    pub const FROM_Y: usize = 1;
    pub const TO_X: usize = 2;
    pub const TO_Y: usize = 3;
    pub const FIRE_AT: usize = 4;
    pub const SHOTS: usize = 5;
    pub const EVERY: usize = 6;
    pub const SPEED: usize = 7;
    pub const LENGTH: usize = 8;
    pub const WIDTH: usize = 9;
    pub const COLOR: usize = 10;
    pub const CORE: usize = 11;
    pub const GLOW: usize = 12;
    pub const SPREAD: usize = 13;
    pub const MUZZLE: usize = 14;
    pub const IMPACT: usize = 15;
    pub const SCORCH: usize = 16;
    pub const SEED: usize = 17;
}
pub mod blood {
    pub const X: usize = 0;
    pub const Y: usize = 1;
    pub const AT: usize = 2;
    pub const DIRECTION: usize = 3;
    pub const SPREAD: usize = 4;
    pub const AMOUNT: usize = 5;
    pub const SIZE: usize = 6;
    pub const REACH: usize = 7;
    pub const COLOR: usize = 8;
    pub const SHINE: usize = 9;
    pub const DRIP: usize = 10;
    pub const SEED: usize = 11;
}
pub mod block_displace {
    pub const WIDTH: usize = 0;
    pub const HEIGHT: usize = 1;
    pub const AMOUNT: usize = 2;
    pub const DIRECTION: usize = 3;
    pub const DENSITY: usize = 4;
    pub const SPEED: usize = 5;
    pub const VARIETY: usize = 6;
    pub const SPLIT: usize = 7;
    pub const EDGES: usize = 8;
    pub const SEED: usize = 9;
}
pub mod derez {
    pub const PROGRESS: usize = 0;
    pub const BLOCK: usize = 1;
    pub const RANDOM: usize = 2;
    pub const FADE: usize = 3;
    pub const DIRECTION: usize = 4;
    pub const SPREAD: usize = 5;
    pub const SEED: usize = 6;
}
pub mod energy_distort {
    pub const AMOUNT: usize = 0;
    pub const SIZE: usize = 1;
    pub const SPEED: usize = 2;
    pub const DETAIL: usize = 3;
    pub const GLOW: usize = 4;
    pub const COLOR: usize = 5;
    pub const AREA: usize = 6;
    pub const X: usize = 7;
    pub const Y: usize = 8;
    pub const RADIUS: usize = 9;
    pub const FEATHER: usize = 10;
    pub const EDGES: usize = 11;
    pub const SEED: usize = 12;
}
pub mod fluid_distort {
    pub const AMOUNT: usize = 0;
    pub const SIZE: usize = 1;
    pub const SPEED: usize = 2;
    pub const SWIRL: usize = 3;
    pub const FLOW_ANGLE: usize = 4;
    pub const FLOW: usize = 5;
    pub const AREA: usize = 6;
    pub const X: usize = 7;
    pub const Y: usize = 8;
    pub const RADIUS: usize = 9;
    pub const FEATHER: usize = 10;
    pub const EDGES: usize = 11;
    pub const SEED: usize = 12;
}
pub mod smoke_distort {
    pub const AMOUNT: usize = 0;
    pub const SIZE: usize = 1;
    pub const RISE: usize = 2;
    pub const TURBULENCE: usize = 3;
    pub const TINT: usize = 4;
    pub const COLOR: usize = 5;
    pub const AREA: usize = 6;
    pub const X: usize = 7;
    pub const Y: usize = 8;
    pub const RADIUS: usize = 9;
    pub const FEATHER: usize = 10;
    pub const EDGES: usize = 11;
    pub const SEED: usize = 12;
    pub const HEIGHT: usize = 13;
}
pub mod ramp {
    pub const SHAPE: usize = 0;
    pub const X1: usize = 1;
    pub const Y1: usize = 2;
    pub const X2: usize = 3;
    pub const Y2: usize = 4;
    pub const COLOR1: usize = 5;
    pub const COLOR2: usize = 6;
    pub const X3: usize = 7;
    pub const Y3: usize = 8;
    pub const X4: usize = 9;
    pub const Y4: usize = 10;
    pub const COLOR3: usize = 11;
    pub const COLOR4: usize = 12;
    pub const BLEND: usize = 13;
    pub const OPACITY: usize = 14;
    pub const MODE: usize = 15;
    pub const DITHER: usize = 16;
}
pub mod strobe {
    pub const RATE: usize = 0;
    pub const ON: usize = 1;
    pub const MODE: usize = 2;
    pub const COLOR: usize = 3;
    pub const START: usize = 4;
    pub const RANDOM: usize = 5;
    pub const STRENGTH: usize = 6;
    pub const SEED: usize = 7;
}
/// Light wrap's parameters (in frame pixels).
pub mod light_wrap {
    pub const WIDTH: usize = 0;
    pub const AMOUNT: usize = 1;
    pub const SOFTNESS: usize = 2;
    pub const MODE: usize = 3;
}
/// Time displacement's parameters.
pub mod time_displace {
    pub const DELAY: usize = 0;
    pub const DIRECTION: usize = 1;
    pub const BANDS: usize = 2;
    /// Most bands (copies of the layer) drawn.
    pub const MAX: usize = 48;
}
/// Hold frame's parameters (seconds into the clip).
pub mod hold {
    pub const FROM: usize = 0;
    pub const UNTIL: usize = 1;
}
pub mod ripple {
    pub const X: usize = 0;
    pub const Y: usize = 1;
    pub const HEIGHT: usize = 2;
    pub const SPACING: usize = 3;
    pub const SPEED: usize = 4;
    pub const PHASE: usize = 5;
    pub const RADIUS: usize = 6;
    pub const DIRECTION: usize = 7;
    pub const EDGES: usize = 8;
}
pub mod wave {
    pub const SHAPE: usize = 0;
    pub const HEIGHT: usize = 1;
    pub const WIDTH: usize = 2;
    pub const DIRECTION: usize = 3;
    pub const SPEED: usize = 4;
    pub const PHASE: usize = 5;
    pub const PIN: usize = 6;
    pub const EDGES: usize = 7;
}
pub mod polar {
    pub const AMOUNT: usize = 0;
    pub const WAY: usize = 1;
}
pub mod mirror {
    pub const X: usize = 0;
    pub const Y: usize = 1;
    pub const ANGLE: usize = 2;
}
pub mod lens {
    pub const X: usize = 0;
    pub const Y: usize = 1;
    pub const DISTORTION: usize = 2;
    pub const CUBIC: usize = 3;
    pub const ZOOM: usize = 4;
    pub const EDGES: usize = 5;
}
pub mod aberration {
    pub const AMOUNT: usize = 0;
    pub const MODE: usize = 1;
    pub const ANGLE: usize = 2;
    pub const FALLOFF: usize = 3;
}
/// Colour wheels parameter indices and the grade they make (the GPU shader
/// `fs_wheels` does the same sums).
pub mod wheels {
    /// The wheels in order; each has X, Y (where its puck is, -100 to 100 %
    /// of the rim: right is blue, up is red, as on a vectorscope) and its
    /// master (the ring under it, -100 to 100 %).
    pub const NAMES: [&str; 4] = ["Lift", "Gamma", "Gain", "Offset"];
    pub const LIFT: usize = 0;
    pub const GAMMA: usize = 3;
    pub const GAIN: usize = 6;
    pub const OFFSET: usize = 9;
    pub const SATURATION: usize = 12;
    pub const CONTRAST: usize = 13;
    pub const PIVOT: usize = 14;
    pub const HUE: usize = 15;

    /// A puck at (x, y) (-1 to 1 each) as a colour push with no brightness of
    /// its own: the Rec. 709 R′G′B′ of chroma (Cb, Cr) = (x, y) × 0.35.
    pub fn tint(x: f32, y: f32) -> [f32; 3] {
        let (cb, cr) = (x * 0.35, y * 0.35);
        [1.5748 * cr, -0.1873 * cb - 0.4681 * cr, 1.8556 * cb]
    }

    /// The grade of display-referred R′G′B′ `c` (0–1) by params `v`.
    pub fn apply(v: &[f32], c: [f32; 3]) -> [f32; 3] {
        let w = |i: usize| (tint(v[i] / 100.0, v[i + 1] / 100.0), v[i + 2] / 100.0);
        let ((tl, ml), (tg, mg), (tn, mn), (to, mo)) = (w(LIFT), w(GAMMA), w(GAIN), w(OFFSET));
        let (sat, con, piv) = (v[SATURATION] / 100.0, v[CONTRAST] / 100.0, v[PIVOT] / 100.0);
        let mut out = [0.0; 3];
        for k in 0..3 {
            let lift = ml * 0.5 + tl[k] * 0.4;
            let gain = (mn + tn[k]).exp2();
            let gamma = (-(mg + tg[k])).exp2();
            let offset = mo * 0.5 + to[k] * 0.4;
            let x = c[k] + lift * (1.0 - c[k]);
            let x = (x * gain).max(0.0).powf(gamma) + offset;
            out[k] = (x - piv) * con + piv;
        }
        let luma = 0.2126 * out[0] + 0.7152 * out[1] + 0.0722 * out[2];
        // Hue: turn the colour round the grey axis (in Cb/Cr).
        let (cb, cr) = ((out[2] - luma) / 1.8556, (out[0] - luma) / 1.5748);
        let (s, co) = v[HUE].to_radians().sin_cos();
        let (cb, cr) = ((cb * co - cr * s) * sat, (cb * s + cr * co) * sat);
        let r = luma + 1.5748 * cr;
        let b = luma + 1.8556 * cb;
        let g = (luma - 0.2126 * r - 0.0722 * b) / 0.7152;
        [r.max(0.0), g.max(0.0), b.max(0.0)]
    }
}
/// HSL qualifier parameter indices, and the selection and grade it makes
/// (the GPU shader `fs_qualify` does the same sums).
pub mod qualify {
    pub const HUE: usize = 0;
    pub const HUE_WIDTH: usize = 1;
    pub const HUE_SOFT: usize = 2;
    pub const SAT_LOW: usize = 3;
    pub const SAT_HIGH: usize = 4;
    pub const SAT_SOFT: usize = 5;
    pub const LUM_LOW: usize = 6;
    pub const LUM_HIGH: usize = 7;
    pub const LUM_SOFT: usize = 8;
    pub const USE_HUE: usize = 9;
    pub const USE_SAT: usize = 10;
    pub const USE_LUM: usize = 11;
    pub const INVERT: usize = 12;
    pub const SHOW: usize = 13;
    pub const HUE_SHIFT: usize = 14;
    pub const SATURATION: usize = 15;
    pub const BRIGHTNESS: usize = 16;
    pub const CONTRAST: usize = 17;
    pub const TINT: usize = 18;
    pub const TINT_AMOUNT: usize = 19;

    /// Hue (0–360°), saturation (0–1, HSV) and Rec. 709 luminance of display R′G′B′.
    pub fn hsl(c: [f32; 3]) -> (f32, f32, f32) {
        let (mx, mn) = (c[0].max(c[1]).max(c[2]), c[0].min(c[1]).min(c[2]));
        let d = mx - mn;
        let h = if d <= 1e-6 {
            0.0
        } else if mx == c[0] {
            60.0 * ((c[1] - c[2]) / d).rem_euclid(6.0)
        } else if mx == c[1] {
            60.0 * ((c[2] - c[0]) / d + 2.0)
        } else {
            60.0 * ((c[0] - c[1]) / d + 4.0)
        };
        let s = if mx <= 1e-6 { 0.0 } else { d / mx };
        (h, s, 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2])
    }

    fn smooth(e0: f32, e1: f32, x: f32) -> f32 {
        if e1 <= e0 {
            return if x >= e0 { 1.0 } else { 0.0 };
        }
        let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    }

    /// Inside a range `lo`–`hi` (0–1) with `soft` either side.
    fn band(x: f32, lo: f32, hi: f32, soft: f32) -> f32 {
        let (lo, hi) = (lo.min(hi), lo.max(hi));
        smooth(lo - soft, lo, x) * (1.0 - smooth(hi, hi + soft, x))
    }

    /// How much colour `c` is chosen (0–1), by params `v`.
    pub fn matte(v: &[f32], c: [f32; 3]) -> f32 {
        let (h, s, l) = hsl(c);
        let mut m = 1.0;
        if v[USE_HUE] >= 0.5 {
            // Distance round the wheel from the centre hue; colourless pixels have no hue.
            let d = ((h - v[HUE]).rem_euclid(360.0)).min((v[HUE] - h).rem_euclid(360.0));
            let half = v[HUE_WIDTH].max(0.0) / 2.0;
            m *= 1.0 - smooth(half, half + v[HUE_SOFT].max(0.0), d);
            m *= smooth(0.0, 0.08, s);
        }
        if v[USE_SAT] >= 0.5 {
            m *= band(s, v[SAT_LOW] / 100.0, v[SAT_HIGH] / 100.0, v[SAT_SOFT].max(0.0) / 100.0);
        }
        if v[USE_LUM] >= 0.5 {
            m *= band(l, v[LUM_LOW] / 100.0, v[LUM_HIGH] / 100.0, v[LUM_SOFT].max(0.0) / 100.0);
        }
        if v[INVERT] >= 0.5 {
            1.0 - m
        } else {
            m
        }
    }

    /// The grade given to the chosen colours (display R′G′B′), before mixing by the matte.
    pub fn grade(v: &[f32], c: [f32; 3]) -> [f32; 3] {
        let y = 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
        let (cb, cr) = ((c[2] - y) / 1.8556, (c[0] - y) / 1.5748);
        let (sn, cs) = v[HUE_SHIFT].to_radians().sin_cos();
        let sat = v[SATURATION].max(0.0) / 100.0;
        let (cb, cr) = ((cb * cs - cr * sn) * sat, (cb * sn + cr * cs) * sat);
        // Brightness ±100 % is a gain of ×2 or ×½; contrast about mid-grey.
        let y = (y * (v[BRIGHTNESS] / 100.0).exp2() - 0.5) * (v[CONTRAST].max(0.0) / 100.0) + 0.5;
        let mut out = [y + 1.5748 * cr, 0.0, y + 1.8556 * cb];
        out[1] = (y - 0.2126 * out[0] - 0.0722 * out[2]) / 0.7152;
        // Tint: towards the colour, keeping the brightness.
        let t = (v[TINT_AMOUNT] / 100.0).clamp(0.0, 1.0);
        if t > 0.0 {
            let tc = v[TINT] as u32;
            let tint = [((tc >> 16) & 255) as f32 / 255.0, ((tc >> 8) & 255) as f32 / 255.0, (tc & 255) as f32 / 255.0];
            let ty = (0.2126 * tint[0] + 0.7152 * tint[1] + 0.0722 * tint[2]).max(1e-3);
            for k in 0..3 {
                out[k] += (tint[k] * y / ty - out[k]) * t * 0.6;
            }
        }
        out.map(|x| x.max(0.0))
    }

    /// The whole effect on display colour `c`: graded where chosen (or what Show asks for).
    pub fn apply(v: &[f32], c: [f32; 3]) -> [f32; 3] {
        let m = matte(v, c);
        match v[SHOW].round() as i32 {
            1 => [m; 3],
            2 => {
                let y = 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
                let g = 0.35 + y * 0.3;
                [0, 1, 2].map(|k| g + (c[k] - g) * m)
            }
            _ => {
                let g = grade(v, c);
                [0, 1, 2].map(|k| c[k] + (g[k] - c[k]) * m)
            }
        }
    }

    /// The eyedropper: hue, saturation and luminance ranges round colour `c`.
    pub fn pick(c: [f32; 3]) -> [(usize, f32); 5] {
        let (h, s, l) = hsl(c);
        [(HUE, h), (SAT_LOW, ((s - 0.25) * 100.0).max(0.0)), (SAT_HIGH, ((s + 0.25) * 100.0).min(100.0)), (LUM_LOW, ((l - 0.3) * 100.0).max(0.0)), (LUM_HIGH, ((l + 0.3) * 100.0).min(100.0))]
    }
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
    /// Flow round the layer's own shape (a title's letters, or a keyed subject:
    /// read at every step, so it can move and push the fluid about).
    pub const OBSTACLES: usize = 15;
    /// Whose shape: this layer's, or the layer on the track above or below
    /// (smoke on its own layer flowing round a subject on another).
    pub const OBSTACLE_LAYER: usize = 16;
    pub const SHAPES: [&str; 3] = ["This layer's", "The layer above", "The layer below"];
    /// Flat (an even fill where it's thick), or billowing: poured in puffs, piling up
    /// thicker than covers, lit as a surface and wispy where thin.
    pub const BODY: usize = 17;
    /// Where billowing smoke's light comes from (90°: above) and how strongly it shades.
    pub const LIGHT: usize = 18;
    pub const SHADING: usize = 19;
    pub const DETAILS: [&str; 3] = ["Low (fast)", "Medium", "High"];
}
/// Ocean parameter indices.
pub mod ocean {
    pub const WIND: usize = 0;
    pub const WIND_DIR: usize = 1;
    pub const HEIGHT: usize = 2;
    pub const CHOP: usize = 3;
    pub const SWELL: usize = 4;
    pub const SWELL_DIR: usize = 5;
    pub const RIPPLES: usize = 6;
    pub const SPEED: usize = 7;
    pub const SEED: usize = 8;
    pub const CAM_HEIGHT: usize = 9;
    pub const TILT: usize = 10;
    pub const FOV: usize = 11;
    pub const CAM_SPEED: usize = 12;
    pub const DEEP: usize = 13;
    pub const SCATTER: usize = 14;
    pub const FOAM: usize = 15;
    pub const SUN_HEIGHT: usize = 16;
    pub const SUN_DIR: usize = 17;
    pub const SUN: usize = 18;
    pub const SKY: usize = 19;
    pub const HORIZON: usize = 20;
    pub const HAZE: usize = 21;
    pub const SHOW: usize = 22;
    pub const OPACITY: usize = 23;
    pub const SOFT: usize = 24;
    /// Solid objects in the sea, keyed from the layer (as the water surface's).
    pub const SOLIDS: usize = 25;
    pub const TOLERANCE: usize = 26;
    /// White water where the waves meet the objects.
    pub const SURF: usize = 27;
    pub const SPRAY: usize = 28;
    pub const WAKE: usize = 29;
    /// The waves bouncing back off the objects.
    pub const REFLECT: usize = 30;
    pub const SHOWS: [&str; 4] = ["Full scene", "Water only (no sky)", "Water under the layer", "The sea round the solid objects (keyed)"];
}
/// Water surface parameter indices.
pub mod water {
    pub const RAIN: usize = 0;
    pub const DROP_SIZE: usize = 1;
    pub const DROP_STRENGTH: usize = 2;
    pub const DROP_X: usize = 3;
    pub const DROP_Y: usize = 4;
    pub const DROP_EVERY: usize = 5;
    pub const WAVE_HEIGHT: usize = 6;
    pub const WAVELENGTH: usize = 7;
    pub const WAVE_DIR: usize = 8;
    pub const WIND: usize = 9;
    pub const SPEED: usize = 10;
    pub const CALM: usize = 11;
    pub const DETAIL: usize = 12;
    pub const SCALE: usize = 13;
    pub const SEED: usize = 14;
    pub const SOLIDS: usize = 15;
    pub const TOLERANCE: usize = 16;
    pub const WAKE: usize = 17;
    pub const LAYER: usize = 18;
    pub const COLOR: usize = 19;
    pub const DEPTH: usize = 20;
    pub const REFRACTION: usize = 21;
    pub const CAUSTICS: usize = 22;
    pub const REFLECTIONS: usize = 23;
    pub const LIGHT_DIR: usize = 24;
    pub const LIGHT_HEIGHT: usize = 25;
    pub const HIGHLIGHTS: usize = 26;
    pub const SKY: usize = 27;
    pub const FLOOR: usize = 28;
    pub const OUTPUT: usize = 29;
    pub const OPACITY: usize = 30;
    pub const DETAILS: [&str; 3] = ["Low (fast)", "Medium", "High"];
    /// What's solid (the water goes round it). Shared with the liquid.
    pub const SOLID_KINDS: [&str; 6] = ["None", "The layer's shape", "Green screen (what's in front)", "Blue screen (what's in front)", "Dark areas", "Light areas"];
    pub const LAYER_KINDS: [&str; 3] = ["Under the water", "Solid objects shown", "Solid objects hidden (keyed out)"];
    pub const OUTPUTS: [&str; 2] = ["Full scene", "Water only (transparent)"];
}
/// Liquid parameter indices.
pub mod liquid {
    pub const X: usize = 0;
    pub const Y: usize = 1;
    pub const DIRECTION: usize = 2;
    pub const SPEED: usize = 3;
    pub const WIDTH: usize = 4;
    pub const FLOW: usize = 5;
    pub const FROM: usize = 6;
    pub const FOR: usize = 7;
    pub const FILL: usize = 8;
    pub const GRAVITY: usize = 9;
    pub const THICK: usize = 10;
    pub const SPLASH: usize = 11;
    pub const DROPLETS: usize = 12;
    pub const EDGES: usize = 13;
    pub const DETAIL: usize = 14;
    pub const SEED: usize = 15;
    pub const SOLIDS: usize = 16;
    pub const TOLERANCE: usize = 17;
    pub const BOUNCE: usize = 18;
    pub const COLOR: usize = 19;
    pub const CLEAR: usize = 20;
    pub const REFRACTION: usize = 21;
    pub const HIGHLIGHTS: usize = 22;
    pub const FOAM: usize = 23;
    pub const SMOOTH: usize = 24;
    pub const LIGHT_DIR: usize = 25;
    pub const SHOW: usize = 26;
    pub const OPACITY: usize = 27;
    /// The water as a body this deep behind the layer (layer pixels; 0: flat),
    /// seen in perspective by a 3D layer's camera.
    pub const DEPTH: usize = 28;
    /// On a 2D layer: how far above the water it's seen from.
    pub const VIEW: usize = 29;
    pub const DETAILS: [&str; 3] = ["Low (fast)", "Medium", "High"];
    pub const EDGE_KINDS: [&str; 4] = ["Floor and walls", "Floor only", "Walls only", "Open (it falls out)"];
    pub const SHOWS: [&str; 3] = ["Water over the layer", "Water and the solid objects (keyed)", "Water only (transparent)"];
}
/// Liquid (3D) parameter indices. The settings before [`liquid3d::COLOR`]
/// change the simulation; the rest, how it's drawn.
pub mod liquid3d {
    pub const X: usize = 0;
    pub const Y: usize = 1;
    /// How far back the source is: 0 the box's front, 50 the layer, 100 its back.
    pub const Z: usize = 2;
    pub const DIRECTION: usize = 3;
    /// How much the source points towards the camera (degrees; negative: away).
    pub const TOWARD: usize = 4;
    pub const SPEED: usize = 5;
    pub const WIDTH: usize = 6;
    pub const FLOW: usize = 7;
    pub const FROM: usize = 8;
    pub const FOR: usize = 9;
    pub const FILL: usize = 10;
    pub const GRAVITY: usize = 11;
    pub const THICK: usize = 12;
    pub const SPLASH: usize = 13;
    pub const DROPLETS: usize = 14;
    pub const CONTAINER: usize = 15;
    pub const DETAIL: usize = 16;
    pub const SEED: usize = 17;
    pub const SOLIDS: usize = 18;
    pub const TOLERANCE: usize = 19;
    /// How thick the solid objects are, front to back (layer pixels).
    pub const OBJECT_DEPTH: usize = 20;
    pub const BOUNCE: usize = 21;
    pub const COLOR: usize = 22;
    pub const CLEAR: usize = 23;
    pub const REFRACTION: usize = 24;
    pub const HIGHLIGHTS: usize = 25;
    pub const FOAM: usize = 26;
    pub const LIGHT_DIR: usize = 27;
    /// How far above the water it's seen from (degrees down).
    pub const VIEW: usize = 28;
    pub const SHOW: usize = 29;
    pub const OPACITY: usize = 30;
    pub const DETAILS: [&str; 3] = ["Low (fast)", "Medium", "High"];
    pub const CONTAINERS: [&str; 3] = ["Floor, walls and back", "Floor only", "Open (it falls away)"];
    pub const SHOWS: [&str; 2] = ["Water over the layer", "Water only (transparent)"];
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
/// Face beauty parameter indices.
pub mod beauty {
    pub const MODE: usize = 0;
    pub const SMOOTH: usize = 1;
    /// How much of the skin's fine texture is kept.
    pub const DETAIL: usize = 2;
    pub const BRIGHTEN: usize = 3;
    /// Blotches and redness evened out.
    pub const EVEN: usize = 4;
    /// The faces found, or all the skin in the picture.
    pub const AREA: usize = 5;
    /// How much bigger than the face found the area worked on is (%).
    pub const SIZE: usize = 6;
    pub const SOFTNESS: usize = 7;
    /// Outline the faces found, to check them.
    pub const SHOW: usize = 8;
    pub const MODES: [&str; 3] = ["Smooth skin", "Blur faces", "Pixelate faces"];
    pub const AREAS: [&str; 2] = ["The faces found", "All skin in the picture"];
}
/// Sky replacement parameter indices.
pub mod sky {
    pub const SOURCE: usize = 0;
    pub const TOP: usize = 1;
    pub const HORIZON: usize = 2;
    /// Where the new sky sits (% of the frame) and how big it is (%).
    pub const X: usize = 3;
    pub const Y: usize = 4;
    pub const SCALE: usize = 5;
    /// How much it moves with the camera (%).
    pub const FOLLOW: usize = 6;
    pub const BRIGHTNESS: usize = 7;
    pub const BLUR: usize = 8;
    /// The sky's edge: softened (px) and moved out (+) or in (px).
    pub const SOFTNESS: usize = 9;
    pub const SHIFT: usize = 10;
    /// The footage tinted towards the new sky's colour (%).
    pub const MATCH: usize = 11;
    pub const OPACITY: usize = 12;
    pub const VIEW: usize = 13;
    pub const SOURCES: [&str; 2] = ["Picture", "Gradient"];
    pub const VIEWS: [&str; 3] = ["Result", "Sky matte", "Sky only"];
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
    choice("Lens", &flare::LENSES),
    spec("Streak", 50.0, 0.0, 200.0, " %", 0.5),
    spec("Ghosts", 60.0, 0.0, 200.0, " %", 0.5),
    spec("Halo", 40.0, 0.0, 200.0, " %", 0.5),
];
const LIGHT_RAYS: [ParamSpec; 9] = [
    spec("Source X", 50.0, -100.0, 200.0, " %", 0.1),
    spec("Source Y", 25.0, -100.0, 200.0, " %", 0.1),
    spec("Length", 50.0, 0.0, 100.0, " %", 0.2),
    spec("Brightness", 100.0, 0.0, 400.0, " %", 0.5),
    spec("Threshold", 60.0, 0.0, 100.0, " %", 0.2),
    spec("Softness", 25.0, 1.0, 100.0, " %", 0.2),
    color("Colour", 0xfff4e0),
    choice("Rays from", &["Bright parts of the layer", "The whole layer"]),
    choice("Show", &["The layer and its rays", "Rays only"]),
];
const ANAMORPHIC: [ParamSpec; 11] = [
    spec("Threshold", 85.0, 0.0, 100.0, " %", 0.2),
    spec("Length", 400.0, 0.0, 3000.0, " px", 1.0),
    spec("Brightness", 100.0, 0.0, 500.0, " %", 0.5),
    spec("Softness", 10.0, 1.0, 100.0, " %", 0.2),
    color("Colour", 0x5a8cff),
    spec("Thickness", 2.0, 0.0, 60.0, " px", 0.05),
    choice("Show", &["The layer and its streaks", "Streaks only"]),
    choice("Streaks from", &["Bright parts of the layer", "A light (X, Y)", "Both"]),
    spec("Light X", 50.0, -100.0, 200.0, " %", 0.1),
    spec("Light Y", 40.0, -100.0, 200.0, " %", 0.1),
    spec("Light size", 12.0, 1.0, 400.0, " px", 0.1),
];
const LENS_DIRT: [ParamSpec; 10] = [
    spec("Light X", 50.0, -100.0, 200.0, " %", 0.1),
    spec("Light Y", 40.0, -100.0, 200.0, " %", 0.1),
    choice("Lit by", &["Bright parts of the layer", "The light (X, Y)"]),
    spec("Amount", 100.0, 0.0, 300.0, " %", 0.5),
    spec("Spread", 35.0, 5.0, 100.0, " %", 0.2),
    spec("Threshold", 60.0, 0.0, 100.0, " %", 0.2),
    spec("Smudge size", 100.0, 20.0, 400.0, " %", 0.5),
    spec("Specks", 50.0, 0.0, 200.0, " %", 0.5),
    color("Colour", 0xfff0dc),
    spec("Seed", 0.0, 0.0, 9999.0, "", 0.2),
];
const NEON_PATH: [ParamSpec; 24] = [
    ParamSpec { name: "Path", default: 1.0, min: 0.0, max: 16.0, suffix: "", speed: 1.0, ui: ParamUi::Choice(&neonpath::PATHS) },
    spec("Start", 0.0, 0.0, 100.0, " %", 0.2),
    spec("End", 100.0, 0.0, 100.0, " %", 0.2),
    spec("Offset", 0.0, -1000.0, 1000.0, " %", 0.2),
    ParamSpec { name: "Closed", default: 1.0, min: 0.0, max: 1.0, suffix: "", speed: 1.0, ui: ParamUi::Toggle },
    spec("Flicker", 0.0, 0.0, 100.0, " %", 0.5),
    spec("Flicker speed", 6.0, 0.1, 30.0, " Hz", 0.05),
    spec("Taper", 0.0, 0.0, 100.0, " %", 0.5),
    spec("Seed", 0.0, 0.0, 9999.0, "", 0.2),
    spec("Brightness", 100.0, 0.0, 400.0, " %", 0.5),
    spec("Core width", 4.0, 0.0, 60.0, " px", 0.05),
    spec("Glow size", 18.0, 0.0, 400.0, " px", 0.2),
    spec("Glow intensity", 120.0, 0.0, 500.0, " %", 0.5),
    color("Core colour", 0xfff0fa),
    color("Glow colour", 0xff2bd6),
    spec("Dash length", 0.0, 0.0, 2000.0, " px", 0.5),
    spec("Gap", 12.0, 0.0, 2000.0, " px", 0.5),
    spec("Dash speed", 0.0, -2000.0, 2000.0, " px/s", 1.0),
    spec("Light the layer", 0.0, 0.0, 300.0, " %", 0.5),
    spec("Opacity", 100.0, 0.0, 100.0, " %", 0.5),
    spec("Show the layer", 100.0, 0.0, 100.0, " %", 0.5),
    spec("Switch on over", 0.0, 0.0, 60.0, " s", 0.02),
    spec("Stutter", 70.0, 0.0, 100.0, " %", 0.5),
    choice("Order", &["Left to right", "Random", "All together"]),
];
const CAMERA_SHAKE: [ParamSpec; 13] = [
    spec("Amount", 12.0, 0.0, 500.0, " px", 0.25),
    spec("Rotation", 0.8, 0.0, 45.0, "°", 0.05),
    spec("Speed", 3.0, 0.05, 30.0, " Hz", 0.05),
    spec("Roughness", 40.0, 0.0, 100.0, " %", 0.5),
    spec("Zoom", 104.0, 100.0, 200.0, " %", 0.1),
    choice("Edges", &["Mirror", "Repeat the edge", "Transparent"]),
    spec("Seed", 0.0, 0.0, 9999.0, "", 0.2),
    choice("Style", &["Handheld", "Walking", "Vehicle", "Earthquake"]),
    spec("Sideways", 100.0, 0.0, 400.0, " %", 0.5),
    spec("Up and down", 100.0, 0.0, 400.0, " %", 0.5),
    spec("Jolt at", -1.0, -1.0, 36000.0, " s", 0.01),
    spec("Jolt", 300.0, 0.0, 2000.0, " %", 1.0),
    spec("Motion blur", 0.0, 0.0, 100.0, " %", 0.5),
];
const FILM_LOOK: [ParamSpec; 14] = [
    choice("Stock", &film::STOCKS),
    spec("Strength", 100.0, 0.0, 100.0, " %", 0.5),
    spec("Contrast", 30.0, -100.0, 100.0, " %", 0.5),
    spec("Lift blacks", 3.0, 0.0, 30.0, " %", 0.1),
    spec("Highlight roll-off", 40.0, 0.0, 100.0, " %", 0.5),
    spec("Saturation", 100.0, 0.0, 200.0, " %", 0.5),
    spec("Halation", 25.0, 0.0, 200.0, " %", 0.5),
    spec("Halation size", 10.0, 1.0, 100.0, " px", 0.1),
    spec("Bloom", 15.0, 0.0, 200.0, " %", 0.5),
    spec("Grain", 15.0, 0.0, 200.0, " %", 0.5),
    spec("Grain size", 1.2, 0.5, 8.0, " px", 0.01),
    spec("Vignette", 20.0, 0.0, 100.0, " %", 0.5),
    spec("Gate weave", 0.0, 0.0, 20.0, " px", 0.05),
    spec("Seed", 0.0, 0.0, 9999.0, "", 0.2),
];
const FILM_DAMAGE: [ParamSpec; 11] = [
    spec("Scratches", 40.0, 0.0, 100.0, " %", 0.5),
    spec("Scratch width", 1.5, 0.5, 20.0, " px", 0.05),
    spec("Dust", 40.0, 0.0, 100.0, " %", 0.5),
    spec("Dust size", 3.0, 0.5, 40.0, " px", 0.05),
    spec("Hairs", 15.0, 0.0, 100.0, " %", 0.5),
    spec("Flicker", 25.0, 0.0, 100.0, " %", 0.5),
    spec("Jitter", 1.5, 0.0, 40.0, " px", 0.05),
    spec("Sepia", 0.0, 0.0, 100.0, " %", 0.5),
    spec("Vignette", 30.0, 0.0, 100.0, " %", 0.5),
    spec("Frame rate", 18.0, 1.0, 60.0, " fps", 0.1),
    spec("Seed", 0.0, 0.0, 9999.0, "", 0.2),
];
const ANALOGUE: [ParamSpec; 12] = [
    spec("Amount", 75.0, 0.0, 100.0, " %", 0.5),
    spec("Colour bleed", 8.0, 0.0, 60.0, " px", 0.05),
    spec("Colour shift", 3.0, -60.0, 60.0, " px", 0.05),
    spec("Tracking lines", 30.0, 0.0, 100.0, " %", 0.5),
    spec("Head switching", 40.0, 0.0, 100.0, " %", 0.5),
    spec("Wobble", 2.0, 0.0, 40.0, " px", 0.05),
    spec("Noise", 30.0, 0.0, 100.0, " %", 0.5),
    spec("Dropouts", 25.0, 0.0, 100.0, " %", 0.5),
    spec("Saturation", 80.0, 0.0, 200.0, " %", 0.5),
    spec("Softness", 1.5, 0.0, 20.0, " px", 0.05),
    spec("Speed", 1.0, 0.0, 10.0, "", 0.02),
    spec("Seed", 0.0, 0.0, 9999.0, "", 0.2),
];
const ABSTRACTION: [ParamSpec; 5] = [
    spec("Brush size", 6.0, 0.0, 40.0, " px", 0.05),
    spec("Outlines", 40.0, 0.0, 100.0, " %", 0.5),
    spec("Outline width", 1.0, 0.5, 8.0, " px", 0.02),
    spec("Colour levels", 0.0, 0.0, 32.0, "", 0.1),
    spec("Saturation", 110.0, 0.0, 200.0, " %", 0.5),
];
const PRISM: [ParamSpec; 6] = [
    spec("Amount", 20.0, 0.0, 300.0, " px", 0.2),
    choice("Direction", &["Out from the centre", "One way"]),
    spec("Angle", 0.0, -360.0, 360.0, "°", 0.5),
    spec("Centre X", 50.0, -100.0, 200.0, " %", 0.2),
    spec("Centre Y", 50.0, -100.0, 200.0, " %", 0.2),
    spec("Colour spread", 100.0, 0.0, 200.0, " %", 0.5),
];
const TILT_SHIFT: [ParamSpec; 7] = [
    spec("Focus at", 55.0, -50.0, 150.0, " %", 0.2),
    spec("Angle", 0.0, -180.0, 180.0, "°", 0.5),
    spec("Sharp band", 12.0, 0.0, 100.0, " %", 0.2),
    spec("Falloff", 25.0, 1.0, 100.0, " %", 0.2),
    spec("Blur", 14.0, 0.0, 100.0, " px", 0.1),
    spec("Saturation", 120.0, 0.0, 200.0, " %", 0.5),
    spec("Contrast", 108.0, 50.0, 200.0, " %", 0.5),
];
const SCANLINES: [ParamSpec; 8] = [
    spec("Amount", 50.0, 0.0, 100.0, " %", 0.5),
    spec("Spacing", 3.0, 1.0, 100.0, " px", 0.05),
    spec("Thickness", 50.0, 1.0, 100.0, " %", 0.5),
    spec("Softness", 50.0, 0.0, 100.0, " %", 0.5),
    spec("Angle", 0.0, -180.0, 180.0, "°", 0.5),
    spec("Roll", 0.0, -500.0, 500.0, " px/s", 0.5),
    choice("Kind", &["Dark lines", "Red, green and blue stripes"]),
    spec("Brighten", 10.0, 0.0, 100.0, " %", 0.5),
];
const AUDIO_VIS: [ParamSpec; 17] = [
    choice("Sound", &audiovis::SOURCES),
    choice("Display", &["Bars", "Line", "Dots", "Waveform"]),
    spec("Bands", 48.0, 2.0, 256.0, "", 0.2),
    spec("Start X", 8.0, -100.0, 200.0, " %", 0.2),
    spec("Start Y", 85.0, -100.0, 200.0, " %", 0.2),
    spec("End X", 92.0, -100.0, 200.0, " %", 0.2),
    spec("End Y", 85.0, -100.0, 200.0, " %", 0.2),
    spec("Height", 35.0, 0.0, 200.0, " %", 0.2),
    spec("Thickness", 65.0, 1.0, 100.0, " %", 0.5),
    spec("Lowest", 40.0, 10.0, 11000.0, " Hz", 1.0),
    spec("Highest", 12000.0, 20.0, 11025.0, " Hz", 10.0),
    color("Inside colour", 0x4fd1ff),
    color("Outside colour", 0xff4fd8),
    spec("Sensitivity", 0.0, -30.0, 30.0, " dB", 0.1),
    spec("Glow", 40.0, 0.0, 200.0, " %", 0.5),
    ParamSpec { name: "Mirror", default: 0.0, min: 0.0, max: 1.0, suffix: "", speed: 1.0, ui: ParamUi::Toggle },
    spec("Show the layer", 100.0, 0.0, 100.0, " %", 0.5),
];
/// Edges, starting at Stretch (no gaps).
const EDGES_STRETCH: ParamSpec = ParamSpec { name: "Edges", default: 1.0, min: 0.0, max: 3.0, suffix: "", speed: 1.0, ui: ParamUi::Choice(&EDGES) };
const BULGE: [ParamSpec; 6] = [
    spec("Centre X", 50.0, -100.0, 200.0, " %", 0.2),
    spec("Centre Y", 50.0, -100.0, 200.0, " %", 0.2),
    spec("Radius", 30.0, 0.0, 200.0, " %", 0.2),
    spec("Amount", 50.0, -100.0, 100.0, " %", 0.5),
    choice("Shape", &["Bulge", "Sphere"]),
    spec("Width", 100.0, 10.0, 1000.0, " %", 0.5),
];
const TWIRL: [ParamSpec; 5] = [
    spec("Centre X", 50.0, -100.0, 200.0, " %", 0.2),
    spec("Centre Y", 50.0, -100.0, 200.0, " %", 0.2),
    spec("Radius", 40.0, 0.0, 200.0, " %", 0.2),
    spec("Angle", 180.0, -3600.0, 3600.0, "°", 1.0),
    choice("Falloff", &["Smooth", "Even"]),
];
const TURBULENTDISPLACE: [ParamSpec; 10] = [
    spec("Amount", 20.0, -500.0, 500.0, " px", 0.1),
    spec("Size", 80.0, 2.0, 4000.0, " px", 0.2),
    spec("Complexity", 3.0, 1.0, 6.0, "", 0.02),
    spec("Evolution", 0.0, -36000.0, 36000.0, "°", 1.0),
    spec("Evolution speed", 0.0, -20.0, 20.0, " turns/s", 0.01),
    spec("Offset X", 0.0, -10000.0, 10000.0, " px", 0.5),
    spec("Offset Y", 0.0, -10000.0, 10000.0, " px", 0.5),
    choice("Displacement", &["Turbulent", "Smooth", "Sideways only", "Up and down only"]),
    choice("Pin", &["Nothing", "All edges"]),
    EDGES_STRETCH,
];
const HEATHAZE: [ParamSpec; 11] = [
    spec("Strength", 4.0, 0.0, 200.0, " px", 0.05),
    spec("Size", 24.0, 2.0, 1000.0, " px", 0.2),
    spec("Rise speed", 80.0, -2000.0, 2000.0, " px/s", 1.0),
    spec("Shimmer", 1.5, 0.0, 20.0, " /s", 0.02),
    choice("Area", &["Whole picture", "Rising from the bottom", "In a circle"]),
    spec("Height", 50.0, 1.0, 100.0, " %", 0.5),
    spec("Centre X", 50.0, -100.0, 200.0, " %", 0.2),
    spec("Centre Y", 70.0, -100.0, 200.0, " %", 0.2),
    spec("Radius", 30.0, 1.0, 200.0, " %", 0.2),
    spec("Rises towards", 90.0, -360.0, 360.0, "°", 0.5),
    EDGES_STRETCH,
];
const SHOCKWAVE: [ParamSpec; 12] = [
    spec("Centre X", 50.0, -100.0, 200.0, " %", 0.2),
    spec("Centre Y", 50.0, -100.0, 200.0, " %", 0.2),
    spec("Start at", 0.0, -3600.0, 3600.0, " s", 0.01),
    spec("Speed", 700.0, 1.0, 20000.0, " px/s", 2.0),
    spec("Width", 60.0, 2.0, 2000.0, " px", 0.5),
    spec("Strength", 25.0, -500.0, 500.0, " px", 0.2),
    spec("Fade out at", 100.0, 1.0, 400.0, " %", 0.5),
    spec("Repeat every", 0.0, 0.0, 60.0, " s", 0.01),
    spec("Colour split", 20.0, 0.0, 100.0, " %", 0.5),
    spec("Ring brightness", 0.0, 0.0, 400.0, " %", 0.5),
    color("Ring colour", 0xdff4ff),
    EDGES_STRETCH,
];
const STARFIELD: [ParamSpec; 14] = [
    choice("Stars", &["Night sky", "Drifting past", "Warp speed"]),
    spec("Amount", 50.0, 0.0, 100.0, " %", 0.5),
    spec("Size", 1.6, 0.3, 20.0, " px", 0.02),
    spec("Speed", 40.0, -5000.0, 5000.0, " px/s", 0.5),
    spec("Direction", 180.0, -360.0, 360.0, "°", 0.5),
    spec("Centre X", 50.0, -100.0, 200.0, " %", 0.2),
    spec("Centre Y", 50.0, -100.0, 200.0, " %", 0.2),
    spec("Streaks", 50.0, 0.0, 100.0, " %", 0.5),
    spec("Twinkle", 40.0, 0.0, 100.0, " %", 0.5),
    spec("Brightness", 100.0, 0.0, 400.0, " %", 0.5),
    color("Colour", 0xeef3ff),
    spec("Colour variety", 40.0, 0.0, 100.0, " %", 0.5),
    choice("Draw", &["Over the picture", "On black", "On its own (see-through)"]),
    spec("Seed", 1.0, 0.0, 9999.0, "", 1.0),
];
const GRADIENT_MAP: [ParamSpec; 7] = [
    choice("Colours", &["Two (duotone)", "Three (tritone)"]),
    color("Shadows", 0x1b2a4a),
    color("Midtones", 0xd0566b),
    color("Highlights", 0xf6e7c8),
    spec("Balance", 50.0, 1.0, 99.0, " %", 0.3),
    spec("Contrast", 100.0, 0.0, 400.0, " %", 0.5),
    spec("Mix", 100.0, 0.0, 100.0, " %", 0.5),
];
const AURORA: [ParamSpec; 11] = [
    color("Colour low", 0x39ff8f),
    color("Colour high", 0xa64dff),
    spec("Height", 60.0, -50.0, 150.0, " %", 0.2),
    spec("Size", 35.0, 1.0, 150.0, " %", 0.2),
    spec("Brightness", 100.0, 0.0, 500.0, " %", 0.5),
    spec("Speed", 30.0, -500.0, 500.0, " %", 0.5),
    spec("Waves", 50.0, 0.0, 200.0, " %", 0.5),
    spec("Rays", 60.0, 0.0, 100.0, " %", 0.5),
    spec("Curtains", 2.0, 1.0, 4.0, "", 1.0),
    choice("Draw", &["Over the picture", "On black", "On its own (see-through)"]),
    spec("Seed", 1.0, 0.0, 9999.0, "", 1.0),
];
const NEBULA: [ParamSpec; 12] = [
    color("Colour", 0x6a2fa0),
    color("Second colour", 0x2c8ef0),
    color("Glow", 0xff6fa8),
    spec("Density", 60.0, 0.0, 100.0, " %", 0.5),
    spec("Scale", 100.0, 5.0, 1000.0, " %", 0.5),
    spec("Detail", 60.0, 0.0, 100.0, " %", 0.5),
    spec("Brightness", 100.0, 0.0, 500.0, " %", 0.5),
    spec("Swirl speed", 10.0, -500.0, 500.0, " %", 0.5),
    spec("Stars", 40.0, 0.0, 100.0, " %", 0.5),
    spec("Dust lanes", 40.0, 0.0, 100.0, " %", 0.5),
    choice("Draw", &["Over the picture", "On black", "On its own (see-through)"]),
    spec("Seed", 1.0, 0.0, 9999.0, "", 1.0),
];
const CLOUDS: [ParamSpec; 13] = [
    choice("Clouds", &["Drifting sky", "Flying through"]),
    spec("Coverage", 55.0, 0.0, 100.0, " %", 0.5),
    spec("Softness", 50.0, 0.0, 100.0, " %", 0.5),
    spec("Scale", 100.0, 5.0, 1000.0, " %", 0.5),
    spec("Speed", 30.0, -1000.0, 1000.0, " %", 0.5),
    spec("Direction", 0.0, -360.0, 360.0, "°", 0.5),
    color("Cloud colour", 0xffffff),
    color("Shade colour", 0x8c98ab),
    color("Sky top", 0x2f66b3),
    color("Sky horizon", 0xa8c8ea),
    choice("Draw", &["Over the picture", "With a sky", "On their own (see-through)"]),
    spec("Brightness", 100.0, 0.0, 300.0, " %", 0.5),
    spec("Seed", 1.0, 0.0, 9999.0, "", 1.0),
];
const BURN: [ParamSpec; 11] = [
    spec("Burnt", 50.0, 0.0, 100.0, " %", 0.3),
    choice("From", &["Everywhere at once", "The left", "The right", "The top", "The bottom", "The middle", "A point"]),
    spec("Point X", 50.0, -100.0, 200.0, " %", 0.2),
    spec("Point Y", 50.0, -100.0, 200.0, " %", 0.2),
    spec("Glowing edge", 6.0, 0.0, 400.0, " px", 0.2),
    spec("Charred edge", 18.0, 0.0, 400.0, " px", 0.2),
    color("Glow colour", 0xff7a1c),
    spec("Glow", 150.0, 0.0, 1000.0, " %", 0.5),
    spec("Raggedness", 100.0, 0.0, 400.0, " %", 0.5),
    spec("Embers", 50.0, 0.0, 100.0, " %", 0.5),
    spec("Seed", 1.0, 0.0, 9999.0, "", 1.0),
];
const EXTRUSION: [ParamSpec; 17] = [
    spec("Depth", 60.0, 0.0, 4000.0, " px", 0.5),
    spec("Bevel", 6.0, 0.0, 400.0, " px", 0.2),
    choice("Bevel shape", &["Flat", "Round"]),
    spec("Turn X", -20.0, -3600.0, 3600.0, " °", 0.3),
    spec("Turn Y", 30.0, -3600.0, 3600.0, " °", 0.3),
    spec("Turn Z", 0.0, -3600.0, 3600.0, " °", 0.3),
    spec("Size", 70.0, 1.0, 1000.0, " %", 0.3),
    spec("Perspective", 35.0, 0.0, 120.0, " °", 0.2),
    choice("Sides", &["The picture's edge colours", "A colour"]),
    color("Side colour", 0x8a8f99),
    spec("Light angle", 135.0, -3600.0, 3600.0, " °", 0.5),
    spec("Light height", 40.0, 0.0, 90.0, " °", 0.3),
    spec("Light", 100.0, 0.0, 400.0, " %", 0.5),
    spec("Ambient", 35.0, 0.0, 100.0, " %", 0.3),
    spec("Shine", 10.0, 0.0, 400.0, " %", 0.5),
    choice("Back", &["The picture", "Side colour"]),
    spec("Solid above", 50.0, 1.0, 99.0, " % opacity", 0.3),
];
const BEND: [ParamSpec; 8] = [
    spec("Bend", 90.0, -720.0, 720.0, " °", 0.5),
    choice("Bends", &["Left and right edges", "Top and bottom edges"]),
    spec("Centre", 50.0, -100.0, 200.0, " %", 0.2),
    choice("Edges go", &["Away", "Towards you"]),
    spec("Perspective", 35.0, 0.0, 120.0, " °", 0.2),
    spec("Shading", 40.0, 0.0, 100.0, " %", 0.3),
    choice("Back", &["The picture", "Darkened", "Nothing"]),
    spec("Size", 100.0, 1.0, 1000.0, " %", 0.3),
];
const BEVEL: [ParamSpec; 6] = [
    spec("Width", 8.0, 0.0, 400.0, " px", 0.2),
    choice("Edges", &["The picture's outline", "The layer's frame"]),
    choice("Shape", &["Flat", "Round"]),
    spec("Light angle", 135.0, -3600.0, 3600.0, " °", 0.5),
    color("Light colour", 0xffffff),
    spec("Intensity", 60.0, 0.0, 400.0, " %", 0.5),
];
const EXTRUDE: [ParamSpec; 9] = [
    spec("Length", 30.0, 0.0, 4000.0, " px", 0.5),
    choice("Towards", &["A direction", "A point"]),
    spec("Direction", 45.0, -3600.0, 3600.0, " °", 0.5),
    spec("Point X", 50.0, -200.0, 300.0, " %", 0.2),
    spec("Point Y", 50.0, -200.0, 300.0, " %", 0.2),
    choice("Sides", &["A darker picture", "A colour"]),
    color("Side colour", 0x30343c),
    spec("Shading", 50.0, 0.0, 100.0, " %", 0.3),
    spec("Fade", 0.0, 0.0, 100.0, " %", 0.3),
];
const ROTATE3D: [ParamSpec; 9] = [
    spec("Turn X", 0.0, -3600.0, 3600.0, " °", 0.3),
    spec("Turn Y", 35.0, -3600.0, 3600.0, " °", 0.3),
    spec("Turn Z", 0.0, -3600.0, 3600.0, " °", 0.3),
    spec("Perspective", 35.0, 0.0, 120.0, " °", 0.2),
    spec("Size", 100.0, 1.0, 1000.0, " %", 0.3),
    spec("Pivot X", 50.0, -200.0, 300.0, " %", 0.2),
    spec("Pivot Y", 50.0, -200.0, 300.0, " %", 0.2),
    spec("Push back", 0.0, -5000.0, 20000.0, " px", 0.5),
    choice("Back", &["The picture", "Darkened", "Nothing"]),
];
const BOX_BLUR: [ParamSpec; 4] = [
    spec("Radius", 10.0, 0.0, 1000.0, " px", 0.2),
    spec("Iterations", 3.0, 1.0, 5.0, "", 0.05),
    choice("Blur", &["Across and down", "Across", "Down"]),
    ParamSpec { name: "Repeat edge pixels", default: 0.0, min: 0.0, max: 1.0, suffix: "", speed: 1.0, ui: ParamUi::Toggle },
];
const CHANNEL_BLUR: [ParamSpec; 6] = [
    spec("Red", 0.0, 0.0, 500.0, " px", 0.2),
    spec("Green", 0.0, 0.0, 500.0, " px", 0.2),
    spec("Blue", 10.0, 0.0, 500.0, " px", 0.2),
    spec("Alpha", 0.0, 0.0, 500.0, " px", 0.2),
    choice("Blur", &["Across and down", "Across", "Down"]),
    ParamSpec { name: "Repeat edge pixels", default: 0.0, min: 0.0, max: 1.0, suffix: "", speed: 1.0, ui: ParamUi::Toggle },
];
const COMPOUND_BLUR: [ParamSpec; 4] = [
    spec("Maximum blur", 20.0, 0.0, 500.0, " px", 0.2),
    choice("Blur by", &["Its brightness", "Its opacity", "Sharp in the middle", "Sharp at the top", "Sharp at the bottom", "Sharp in a band across"]),
    ParamSpec { name: "Invert the map", default: 0.0, min: 0.0, max: 1.0, suffix: "", speed: 1.0, ui: ParamUi::Toggle },
    spec("Band", 50.0, 0.0, 100.0, " %", 0.3),
];
const SMART_BLUR: [ParamSpec; 3] = [
    spec("Radius", 4.0, 0.0, 40.0, " px", 0.1),
    spec("Threshold", 25.0, 0.0, 100.0, " %", 0.3),
    choice("Mode", &["Normal", "Edges only", "Edges over it"]),
];
const LENS_BLUR: [ParamSpec; 6] = [
    spec("Blur radius", 12.0, 0.0, 300.0, " px", 0.2),
    choice("Iris shape", &["Round", "Triangle", "Square", "Pentagon", "Hexagon", "Heptagon", "Octagon"]),
    spec("Iris rotation", 0.0, -3600.0, 3600.0, " °", 0.5),
    spec("Highlight gain", 100.0, 0.0, 1000.0, " %", 0.5),
    spec("Highlight threshold", 75.0, 0.0, 100.0, " %", 0.3),
    ParamSpec { name: "Repeat edge pixels", default: 1.0, min: 0.0, max: 1.0, suffix: "", speed: 1.0, ui: ParamUi::Toggle },
];
const CROSS_BLUR: [ParamSpec; 3] = [
    spec("Radius across", 10.0, 0.0, 500.0, " px", 0.2),
    spec("Radius down", 10.0, 0.0, 500.0, " px", 0.2),
    choice("Combine", &["Average", "Add", "Screen", "Lighten"]),
];
const VECTOR_BLUR: [ParamSpec; 5] = [
    spec("Amount", 10.0, 0.0, 300.0, " px", 0.2),
    choice("Direction", &["Along the contours", "Across the contours", "One angle"]),
    spec("Angle", 0.0, -3600.0, 3600.0, " °", 0.5),
    spec("Ridge smoothness", 4.0, 0.5, 50.0, " px", 0.1),
    choice("Shapes from", &["Lightness", "Red", "Green", "Blue", "Alpha"]),
];
const ARITHMETIC: [ParamSpec; 5] = [
    choice("Operator", &["And", "Or", "Xor", "Add", "Subtract", "Difference", "Minimum", "Maximum", "Block above", "Block below", "Slice", "Multiply", "Screen"]),
    spec("Red value", 255.0, 0.0, 255.0, "", 0.5),
    spec("Green value", 255.0, 0.0, 255.0, "", 0.5),
    spec("Blue value", 255.0, 0.0, 255.0, "", 0.5),
    ParamSpec { name: "Clip result values", default: 1.0, min: 0.0, max: 1.0, suffix: "", speed: 1.0, ui: ParamUi::Toggle },
];
const BLEND_COLOUR: [ParamSpec; 3] = [
    color("Colour", 0x2060c0),
    choice("Mode", &["Cross-fade", "Colour only", "Tint only", "Darken only", "Lighten only"]),
    spec("Blend with original", 50.0, 0.0, 100.0, " %", 0.3),
];
const CALCULATIONS: [ParamSpec; 7] = [
    choice("Input channel", &["RGBA", "Grey", "Red", "Green", "Blue", "Alpha"]),
    ParamSpec { name: "Invert input", default: 0.0, min: 0.0, max: 1.0, suffix: "", speed: 1.0, ui: ParamUi::Toggle },
    choice("Second channel", &["Luminance", "Red", "Green", "Blue", "Alpha"]),
    ParamSpec { name: "Invert second", default: 0.0, min: 0.0, max: 1.0, suffix: "", speed: 1.0, ui: ParamUi::Toggle },
    spec("Second opacity", 100.0, 0.0, 100.0, " %", 0.3),
    choice("Blend mode", &["Normal", "Multiply", "Screen", "Overlay", "Soft light", "Hard light", "Colour dodge", "Colour burn", "Darken", "Lighten", "Difference", "Add", "Subtract"]),
    ParamSpec { name: "Preserve transparency", default: 1.0, min: 0.0, max: 1.0, suffix: "", speed: 1.0, ui: ParamUi::Toggle },
];
const CHANNEL_MIXER: [ParamSpec; 13] = [
    spec("Red–Red", 100.0, -200.0, 200.0, " %", 0.5),
    spec("Red–Green", 0.0, -200.0, 200.0, " %", 0.5),
    spec("Red–Blue", 0.0, -200.0, 200.0, " %", 0.5),
    spec("Red–Constant", 0.0, -200.0, 200.0, " %", 0.5),
    spec("Green–Red", 0.0, -200.0, 200.0, " %", 0.5),
    spec("Green–Green", 100.0, -200.0, 200.0, " %", 0.5),
    spec("Green–Blue", 0.0, -200.0, 200.0, " %", 0.5),
    spec("Green–Constant", 0.0, -200.0, 200.0, " %", 0.5),
    spec("Blue–Red", 0.0, -200.0, 200.0, " %", 0.5),
    spec("Blue–Green", 0.0, -200.0, 200.0, " %", 0.5),
    spec("Blue–Blue", 100.0, -200.0, 200.0, " %", 0.5),
    spec("Blue–Constant", 0.0, -200.0, 200.0, " %", 0.5),
    ParamSpec { name: "Monochrome", default: 0.0, min: 0.0, max: 1.0, suffix: "", speed: 1.0, ui: ParamUi::Toggle },
];
const INVERT: [ParamSpec; 2] = [
    choice("Channel", &["RGB", "Red", "Green", "Blue", "Alpha", "Luminance", "Hue", "Lightness", "Saturation"]),
    spec("Blend with original", 0.0, 0.0, 100.0, " %", 0.3),
];
const SHIFT_CHANNELS: [ParamSpec; 4] = [
    ParamSpec { name: "Take red from", default: 0.0, min: 0.0, max: 10.0, suffix: "", speed: 1.0, ui: ParamUi::Choice(&["Red", "Green", "Blue", "Alpha", "Luminance", "Hue", "Lightness", "Saturation", "Full", "Half", "Off"]) },
    ParamSpec { name: "Take green from", default: 1.0, min: 0.0, max: 10.0, suffix: "", speed: 1.0, ui: ParamUi::Choice(&["Red", "Green", "Blue", "Alpha", "Luminance", "Hue", "Lightness", "Saturation", "Full", "Half", "Off"]) },
    ParamSpec { name: "Take blue from", default: 2.0, min: 0.0, max: 10.0, suffix: "", speed: 1.0, ui: ParamUi::Choice(&["Red", "Green", "Blue", "Alpha", "Luminance", "Hue", "Lightness", "Saturation", "Full", "Half", "Off"]) },
    ParamSpec { name: "Take alpha from", default: 3.0, min: 0.0, max: 10.0, suffix: "", speed: 1.0, ui: ParamUi::Choice(&["Red", "Green", "Blue", "Alpha", "Luminance", "Hue", "Lightness", "Saturation", "Full", "Half", "Off"]) },
];
const SET_CHANNELS: [ParamSpec; 8] = [
    ParamSpec { name: "Red from", default: 0.0, min: 0.0, max: 10.0, suffix: "", speed: 1.0, ui: ParamUi::Choice(&["Red", "Green", "Blue", "Alpha", "Luminance", "Hue", "Lightness", "Saturation", "Full", "Half", "Off"]) },
    ParamSpec { name: "Green from", default: 1.0, min: 0.0, max: 10.0, suffix: "", speed: 1.0, ui: ParamUi::Choice(&["Red", "Green", "Blue", "Alpha", "Luminance", "Hue", "Lightness", "Saturation", "Full", "Half", "Off"]) },
    ParamSpec { name: "Blue from", default: 2.0, min: 0.0, max: 10.0, suffix: "", speed: 1.0, ui: ParamUi::Choice(&["Red", "Green", "Blue", "Alpha", "Luminance", "Hue", "Lightness", "Saturation", "Full", "Half", "Off"]) },
    ParamSpec { name: "Alpha from", default: 3.0, min: 0.0, max: 10.0, suffix: "", speed: 1.0, ui: ParamUi::Choice(&["Red", "Green", "Blue", "Alpha", "Luminance", "Hue", "Lightness", "Saturation", "Full", "Half", "Off"]) },
    ParamSpec { name: "Invert red", default: 0.0, min: 0.0, max: 1.0, suffix: "", speed: 1.0, ui: ParamUi::Toggle },
    ParamSpec { name: "Invert green", default: 0.0, min: 0.0, max: 1.0, suffix: "", speed: 1.0, ui: ParamUi::Toggle },
    ParamSpec { name: "Invert blue", default: 0.0, min: 0.0, max: 1.0, suffix: "", speed: 1.0, ui: ParamUi::Toggle },
    ParamSpec { name: "Invert alpha", default: 0.0, min: 0.0, max: 1.0, suffix: "", speed: 1.0, ui: ParamUi::Toggle },
];
const SET_MATTE: [ParamSpec; 3] = [
    ParamSpec { name: "Take matte from", default: 4.0, min: 0.0, max: 10.0, suffix: "", speed: 1.0, ui: ParamUi::Choice(&["Red", "Green", "Blue", "Alpha", "Luminance", "Hue", "Lightness", "Saturation", "Full", "Half", "Off"]) },
    ParamSpec { name: "Invert matte", default: 0.0, min: 0.0, max: 1.0, suffix: "", speed: 1.0, ui: ParamUi::Toggle },
    ParamSpec { name: "Keep its own transparency", default: 1.0, min: 0.0, max: 1.0, suffix: "", speed: 1.0, ui: ParamUi::Toggle },
];
const SOLID_COMPOSITE: [ParamSpec; 4] = [
    spec("Source opacity", 100.0, 0.0, 100.0, " %", 0.3),
    color("Colour", 0xffffff),
    spec("Opacity", 100.0, 0.0, 100.0, " %", 0.3),
    choice("Blend mode", &["Normal", "Multiply", "Screen", "Overlay", "Soft light", "Hard light", "Colour dodge", "Colour burn", "Darken", "Lighten", "Difference", "Add", "Subtract"]),
];
const MINIMAX: [ParamSpec; 4] = [
    choice("Operation", &["Minimum", "Maximum", "Minimum, then maximum", "Maximum, then minimum"]),
    spec("Radius", 3.0, 0.0, 200.0, " px", 0.1),
    choice("Channel", &["Colour", "Alpha and colour", "Alpha", "Red", "Green", "Blue"]),
    choice("Direction", &["Across and down", "Across", "Down"]),
];
const AUTO_LEVELS: [ParamSpec; 3] = [
    spec("Black clip", 0.1, 0.0, 10.0, " %", 0.02),
    spec("White clip", 0.1, 0.0, 10.0, " %", 0.02),
    spec("Blend with original", 0.0, 0.0, 100.0, " %", 0.3),
];
const AUTO_CONTRAST: [ParamSpec; 3] = [
    spec("Black clip", 0.1, 0.0, 10.0, " %", 0.02),
    spec("White clip", 0.1, 0.0, 10.0, " %", 0.02),
    spec("Blend with original", 0.0, 0.0, 100.0, " %", 0.3),
];
const AUTO_COLOR: [ParamSpec; 3] = [
    spec("Black clip", 0.1, 0.0, 10.0, " %", 0.02),
    spec("White clip", 0.1, 0.0, 10.0, " %", 0.02),
    spec("Blend with original", 0.0, 0.0, 100.0, " %", 0.3),
];
const EQUALIZE: [ParamSpec; 2] = [
    choice("Equalise", &["Each channel (RGB)", "Brightness"]),
    spec("Amount", 100.0, 0.0, 100.0, " %", 0.3),
];
const CHANGE_COLOR: [ParamSpec; 9] = [
    choice("View", &["Corrected layer", "Colour correction mask"]),
    spec("Hue transform", 0.0, -180.0, 180.0, " °", 0.5),
    spec("Lightness transform", 0.0, -100.0, 100.0, "", 0.3),
    spec("Saturation transform", 0.0, -100.0, 100.0, "", 0.3),
    color("Colour to change", 0xd02020),
    spec("Matching tolerance", 15.0, 0.0, 100.0, " %", 0.3),
    spec("Matching softness", 0.0, 0.0, 100.0, " %", 0.3),
    choice("Match colours", &["Using RGB", "Using hue", "Using chroma"]),
    ParamSpec { name: "Invert colour correction mask", default: 0.0, min: 0.0, max: 1.0, suffix: "", speed: 1.0, ui: ParamUi::Toggle },
];
const CHANGE_TO_COLOR: [ParamSpec; 8] = [
    color("From", 0xd02020),
    color("To", 0x2060e0),
    choice("Change", &["Hue", "Hue and lightness", "Hue, lightness and saturation", "Hue and saturation"]),
    spec("Hue tolerance", 10.0, 0.0, 100.0, " %", 0.3),
    spec("Lightness tolerance", 50.0, 0.0, 100.0, " %", 0.3),
    spec("Saturation tolerance", 50.0, 0.0, 100.0, " %", 0.3),
    spec("Softness", 50.0, 0.0, 100.0, " %", 0.3),
    ParamSpec { name: "View the correction mask", default: 0.0, min: 0.0, max: 1.0, suffix: "", speed: 1.0, ui: ParamUi::Toggle },
];
const COLOR_BALANCE: [ParamSpec; 10] = [
    spec("Shadow red", 0.0, -100.0, 100.0, "", 0.3),
    spec("Shadow green", 0.0, -100.0, 100.0, "", 0.3),
    spec("Shadow blue", 0.0, -100.0, 100.0, "", 0.3),
    spec("Midtone red", 0.0, -100.0, 100.0, "", 0.3),
    spec("Midtone green", 0.0, -100.0, 100.0, "", 0.3),
    spec("Midtone blue", 0.0, -100.0, 100.0, "", 0.3),
    spec("Highlight red", 0.0, -100.0, 100.0, "", 0.3),
    spec("Highlight green", 0.0, -100.0, 100.0, "", 0.3),
    spec("Highlight blue", 0.0, -100.0, 100.0, "", 0.3),
    ParamSpec { name: "Preserve luminosity", default: 0.0, min: 0.0, max: 1.0, suffix: "", speed: 1.0, ui: ParamUi::Toggle },
];
const EXPOSURE: [ParamSpec; 3] = [
    spec("Exposure", 0.0, -20.0, 20.0, " stops", 0.02),
    spec("Offset", 0.0, -1.0, 1.0, "", 0.002),
    spec("Gamma correction", 1.0, 0.1, 10.0, "", 0.01),
];
const LEAVE_COLOR: [ParamSpec; 5] = [
    spec("Amount to decolour", 100.0, 0.0, 100.0, " %", 0.3),
    color("Colour to leave", 0xd02020),
    spec("Tolerance", 20.0, 0.0, 100.0, " %", 0.3),
    spec("Edge softness", 10.0, 0.0, 100.0, " %", 0.3),
    choice("Match colours", &["Using RGB", "Using hue"]),
];
const PHOTO_FILTER: [ParamSpec; 4] = [
    choice("Filter", &["Warming (85)", "Warming (LBA)", "Warming (81)", "Cooling (80)", "Cooling (LBB)", "Cooling (82)", "Red", "Orange", "Yellow", "Green", "Cyan", "Blue", "Violet", "Magenta", "Sepia", "Deep red", "Deep blue", "Deep emerald", "Deep yellow", "Underwater", "Custom"]),
    color("Colour", 0xec8a00),
    spec("Density", 25.0, 0.0, 100.0, " %", 0.3),
    ParamSpec { name: "Preserve luminosity", default: 1.0, min: 0.0, max: 1.0, suffix: "", speed: 1.0, ui: ParamUi::Toggle },
];
const SELECTIVE_COLOR: [ParamSpec; 16] = [
    choice("Method", &["Relative", "Absolute"]),
    choice("Colours 1", &["Reds", "Yellows", "Greens", "Cyans", "Blues", "Magentas", "Whites", "Neutrals", "Blacks"]),
    spec("Cyan 1", 0.0, -100.0, 100.0, " %", 0.3),
    spec("Magenta 1", 0.0, -100.0, 100.0, " %", 0.3),
    spec("Yellow 1", 0.0, -100.0, 100.0, " %", 0.3),
    spec("Black 1", 0.0, -100.0, 100.0, " %", 0.3),
    ParamSpec { name: "Colours 2", default: 6.0, min: 0.0, max: 8.0, suffix: "", speed: 1.0, ui: ParamUi::Choice(&["Reds", "Yellows", "Greens", "Cyans", "Blues", "Magentas", "Whites", "Neutrals", "Blacks"]) },
    spec("Cyan 2", 0.0, -100.0, 100.0, " %", 0.3),
    spec("Magenta 2", 0.0, -100.0, 100.0, " %", 0.3),
    spec("Yellow 2", 0.0, -100.0, 100.0, " %", 0.3),
    spec("Black 2", 0.0, -100.0, 100.0, " %", 0.3),
    ParamSpec { name: "Colours 3", default: 8.0, min: 0.0, max: 8.0, suffix: "", speed: 1.0, ui: ParamUi::Choice(&["Reds", "Yellows", "Greens", "Cyans", "Blues", "Magentas", "Whites", "Neutrals", "Blacks"]) },
    spec("Cyan 3", 0.0, -100.0, 100.0, " %", 0.3),
    spec("Magenta 3", 0.0, -100.0, 100.0, " %", 0.3),
    spec("Yellow 3", 0.0, -100.0, 100.0, " %", 0.3),
    spec("Black 3", 0.0, -100.0, 100.0, " %", 0.3),
];
const TINT: [ParamSpec; 3] = [
    color("Map black to", 0x000000),
    color("Map white to", 0xffffff),
    spec("Amount to tint", 100.0, 0.0, 100.0, " %", 0.3),
];
const BLACK_WHITE: [ParamSpec; 8] = [
    spec("Reds", 40.0, -200.0, 300.0, " %", 0.5),
    spec("Yellows", 60.0, -200.0, 300.0, " %", 0.5),
    spec("Greens", 40.0, -200.0, 300.0, " %", 0.5),
    spec("Cyans", 60.0, -200.0, 300.0, " %", 0.5),
    spec("Blues", 20.0, -200.0, 300.0, " %", 0.5),
    spec("Magentas", 80.0, -200.0, 300.0, " %", 0.5),
    ParamSpec { name: "Tint", default: 0.0, min: 0.0, max: 1.0, suffix: "", speed: 1.0, ui: ParamUi::Toggle },
    color("Tint colour", 0xe1d3b3),
];
const COLOR_NEUTRALIZER: [ParamSpec; 4] = [
    color("Shadows cast", 0x000000),
    color("Midtones cast", 0x808080),
    color("Highlights cast", 0xffffff),
    spec("Blend with original", 0.0, 0.0, 100.0, " %", 0.3),
];
const COLOR_OFFSET: [ParamSpec; 4] = [
    spec("Red phase", 0.0, -3600.0, 3600.0, " °", 0.5),
    spec("Green phase", 0.0, -3600.0, 3600.0, " °", 0.5),
    spec("Blue phase", 0.0, -3600.0, 3600.0, " °", 0.5),
    choice("Overflow", &["Wrap", "Solarise", "Polarise"]),
];
const TONER: [ParamSpec; 7] = [
    choice("Tones", &["Duotone", "Tritone", "Pentone", "Solarise"]),
    color("Highlights", 0xffffff),
    color("Brights", 0xe5cc99),
    color("Midtones", 0x9a7a4c),
    color("Darktones", 0x4c3420),
    color("Shadows", 0x000000),
    spec("Blend with original", 0.0, 0.0, 100.0, " %", 0.3),
];
const THRESHOLD_RGB: [ParamSpec; 7] = [
    spec("Red threshold", 128.0, 0.0, 255.0, "", 0.5),
    spec("Green threshold", 128.0, 0.0, 255.0, "", 0.5),
    spec("Blue threshold", 128.0, 0.0, 255.0, "", 0.5),
    ParamSpec { name: "Invert red", default: 0.0, min: 0.0, max: 1.0, suffix: "", speed: 1.0, ui: ParamUi::Toggle },
    ParamSpec { name: "Invert green", default: 0.0, min: 0.0, max: 1.0, suffix: "", speed: 1.0, ui: ParamUi::Toggle },
    ParamSpec { name: "Invert blue", default: 0.0, min: 0.0, max: 1.0, suffix: "", speed: 1.0, ui: ParamUi::Toggle },
    spec("Blend with original", 0.0, 0.0, 100.0, " %", 0.3),
];
const KERNEL: [ParamSpec; 13] = [
    spec("Top left", 0.0, -999.0, 999.0, "", 0.1),
    spec("Top", 0.0, -999.0, 999.0, "", 0.1),
    spec("Top right", 0.0, -999.0, 999.0, "", 0.1),
    spec("Left", 0.0, -999.0, 999.0, "", 0.1),
    spec("Middle", 1.0, -999.0, 999.0, "", 0.1),
    spec("Right", 0.0, -999.0, 999.0, "", 0.1),
    spec("Bottom left", 0.0, -999.0, 999.0, "", 0.1),
    spec("Bottom", 0.0, -999.0, 999.0, "", 0.1),
    spec("Bottom right", 0.0, -999.0, 999.0, "", 0.1),
    spec("Divide by", 1.0, -999.0, 999.0, "", 0.1),
    spec("Offset", 0.0, -255.0, 255.0, "", 0.5),
    ParamSpec { name: "Alpha too", default: 0.0, min: 0.0, max: 1.0, suffix: "", speed: 1.0, ui: ParamUi::Toggle },
    spec("Blend with original", 0.0, 0.0, 100.0, " %", 0.3),
];
const VIDEO_LIMITER: [ParamSpec; 4] = [
    choice("Reduce", &["Brightness", "Saturation", "Both"]),
    spec("Highest brightness", 100.0, 50.0, 110.0, " %", 0.2),
    spec("Lowest brightness", 0.0, -10.0, 50.0, " %", 0.2),
    spec("Highest saturation", 100.0, 0.0, 150.0, " %", 0.3),
];
const DEFLICKER: [ParamSpec; 6] = [
    spec("Brightness", 100.0, 0.0, 400.0, " %", 0.2),
    spec("Red", 100.0, 0.0, 400.0, " %", 0.2),
    spec("Green", 100.0, 0.0, 400.0, " %", 0.2),
    spec("Blue", 100.0, 0.0, 400.0, " %", 0.2),
    spec("Smoothing", 15.0, 1.0, 250.0, " frames", 0.5),
    ParamSpec { name: "Even out colour too", default: 1.0, min: 0.0, max: 1.0, suffix: "", speed: 1.0, ui: ParamUi::Toggle },
];
const HOLOGRAM: [ParamSpec; 11] = [
    color("Colour", 0x4fd8ff),
    spec("Opacity", 70.0, 0.0, 100.0, " %", 0.5),
    spec("Scanlines", 50.0, 0.0, 100.0, " %", 0.5),
    spec("Line spacing", 4.0, 1.0, 100.0, " px", 0.05),
    spec("Line speed", 20.0, -2000.0, 2000.0, " px/s", 0.5),
    spec("Flicker", 25.0, 0.0, 100.0, " %", 0.5),
    spec("Glitches", 25.0, 0.0, 100.0, " %", 0.5),
    spec("Edge glow", 60.0, 0.0, 300.0, " %", 0.5),
    spec("Colour split", 2.0, 0.0, 50.0, " px", 0.05),
    spec("Keep its colours", 0.0, 0.0, 100.0, " %", 0.5),
    spec("Seed", 1.0, 0.0, 9999.0, "", 1.0),
];
const PORTAL: [ParamSpec; 15] = [
    spec("Centre X", 50.0, -100.0, 200.0, " %", 0.2),
    spec("Centre Y", 50.0, -100.0, 200.0, " %", 0.2),
    spec("Radius", 30.0, 1.0, 200.0, " %", 0.2),
    spec("Open", 100.0, 0.0, 100.0, " %", 0.5),
    spec("Rim width", 18.0, 1.0, 400.0, " px", 0.2),
    color("Colour", 0xff9a2e),
    color("Second colour", 0xffe48a),
    spec("Spin", 0.5, -10.0, 10.0, " turns/s", 0.01),
    spec("Sparks", 60.0, 0.0, 100.0, " %", 0.5),
    choice("Inside", &["A swirl of the picture", "Dark", "See-through (shows the track below)"]),
    spec("Swirl", 180.0, -1440.0, 1440.0, "°", 1.0),
    spec("Ripple round it", 8.0, 0.0, 200.0, " px", 0.1),
    spec("Brightness", 150.0, 0.0, 500.0, " %", 0.5),
    spec("Stretch", 100.0, 10.0, 400.0, " %", 0.5),
    spec("Seed", 1.0, 0.0, 9999.0, "", 1.0),
];
const HUD: [ParamSpec; 23] = [
    choice(
        "Element",
        &[
            "Targeting reticle",
            "Radar sweep",
            "Turning rings",
            "Readout bars",
            "Corner brackets",
            "Scope",
            "Double-ring scope",
            "Sniper scope",
            "Chevron dial",
            "Star compass",
            "Diamond compass",
            "Notched square",
            "Framed square",
            "Bracketed square",
            "Hexagon in pieces",
            "Hexagon",
            "Double hexagon",
            "Side arcs",
            "Bow tie",
            "Hooked arcs",
            "Swept X",
            "Bracketed X",
            "Long X",
            "Wireframe callouts",
            "Wireframe hexagon",
            "Scanner panel",
            "Data scope",
            "Data X",
            "Data cross",
            "Bullseye",
            "Heavy ring",
            "Tick dial",
            "Segment gauge",
            "Lock-on",
            "Segmented shield",
        ],
    ),
    spec("Centre X", 50.0, -100.0, 200.0, " %", 0.2),
    spec("Centre Y", 50.0, -100.0, 200.0, " %", 0.2),
    spec("Size", 30.0, 1.0, 200.0, " %", 0.2),
    color("Colour", 0x5ce1ff),
    spec("Line width", 2.0, 0.3, 40.0, " px", 0.05),
    spec("Spin", 0.25, -10.0, 10.0, " turns/s", 0.01),
    spec("Detail", 50.0, 0.0, 100.0, " %", 0.5),
    spec("Draw on", 100.0, 0.0, 100.0, " %", 0.5),
    spec("Opacity", 100.0, 0.0, 100.0, " %", 0.5),
    spec("Flicker", 0.0, 0.0, 100.0, " %", 0.5),
    spec("Seed", 1.0, 0.0, 9999.0, "", 1.0),
    spec("Glow", 0.0, 0.0, 200.0, " %", 0.5),
    spec("Fill", 0.0, 0.0, 100.0, " %", 0.5),
    spec("Glow size", 12.0, 0.5, 200.0, " %", 0.2),
    color("Glow colour", 0x5ce1ff),
    ParamSpec { name: "Own glow colour", default: 0.0, min: 0.0, max: 1.0, suffix: "", speed: 1.0, ui: ParamUi::Toggle },
    ParamSpec { name: "Lock-on", default: 0.0, min: 0.0, max: 1.0, suffix: "", speed: 1.0, ui: ParamUi::Toggle },
    spec("Lock-on at", 0.0, 0.0, 36000.0, " s", 0.01),
    spec("Lock-on time", 1.0, 0.05, 30.0, " s", 0.01),
    color("Locked colour", 0xff3b30),
    choice("Text position", &["Right", "Left", "Above", "Below", "Centre"]),
    spec("Text size", 4.0, 0.5, 30.0, " %", 0.05),
];
const LIGHTSWEEP: [ParamSpec; 11] = [
    spec("Angle", 60.0, -360.0, 360.0, "°", 0.5),
    spec("Width", 60.0, 1.0, 4000.0, " px", 0.5),
    spec("Softness", 60.0, 0.0, 100.0, " %", 0.5),
    spec("Start at", 0.0, -3600.0, 3600.0, " s", 0.01),
    spec("Takes", 1.0, 0.0, 60.0, " s", 0.01),
    spec("Repeat every", 0.0, 0.0, 60.0, " s", 0.01),
    spec("Position", -1.0, -1.0, 200.0, " %", 0.5),
    spec("Brightness", 150.0, 0.0, 1000.0, " %", 0.5),
    color("Colour", 0xffffff),
    choice("Lights", &["The shape", "Its bright parts", "Everything"]),
    spec("Second sweep", 0.0, 0.0, 100.0, " %", 0.5),
];
const ELECTRICARCS: [ParamSpec; 11] = [
    color("Colour", 0x8fc8ff),
    spec("Amount", 50.0, 0.0, 100.0, " %", 0.5),
    spec("Reach", 30.0, 1.0, 400.0, " px", 0.5),
    spec("Thickness", 1.5, 0.3, 20.0, " px", 0.05),
    spec("Speed", 4.0, 0.0, 50.0, " /s", 0.05),
    spec("Flicker", 40.0, 0.0, 100.0, " %", 0.5),
    spec("Glow", 60.0, 0.0, 300.0, " %", 0.5),
    spec("Brightness", 150.0, 0.0, 1000.0, " %", 0.5),
    spec("Over the shape", 30.0, 0.0, 100.0, " %", 0.5),
    spec("Scale", 40.0, 4.0, 1000.0, " px", 0.5),
    spec("Seed", 1.0, 0.0, 9999.0, "", 1.0),
];
const ECHO: [ParamSpec; 6] = [
    spec("Echo time", 0.1, -5.0, 5.0, " s", 0.005),
    spec("Number of echoes", 4.0, 0.0, 16.0, "", 0.05),
    spec("First echo", 60.0, 0.0, 100.0, " %", 0.5),
    spec("Each fades by", 30.0, 0.0, 100.0, " %", 0.5),
    choice("Draw echoes", &["Behind the layer", "In front of it", "Added (light trails)", "Screen", "Lighten"]),
    spec("Original", 100.0, 0.0, 100.0, " %", 0.5),
];
const LIQUIFY: [ParamSpec; 6] = [
    choice("Tool", &["Push", "Twirl clockwise", "Twirl anticlockwise", "Pucker (pinch in)", "Bloat (swell out)", "Reconstruct"]),
    spec("Brush size", 120.0, 2.0, 4000.0, " px", 0.5),
    spec("Pressure", 50.0, 1.0, 100.0, " %", 0.5),
    spec("Amount", 100.0, 0.0, 400.0, " %", 0.5),
    choice("Edges", &["Transparent", "Stretch"]),
    ParamSpec { name: "Show mesh", default: 0.0, min: 0.0, max: 1.0, suffix: "", speed: 1.0, ui: ParamUi::Toggle },
];
const DEPTH_MAP: [ParamSpec; 8] = [
    choice("Show", &["The depth (near is white)", "Fog by depth", "Keep what's near (depth key)"]),
    spec("Nearest at", 100.0, 0.0, 100.0, " %", 0.5),
    spec("Farthest at", 0.0, 0.0, 100.0, " %", 0.5),
    ParamSpec { name: "Invert (near is black)", default: 0.0, min: 0.0, max: 1.0, suffix: "", speed: 1.0, ui: ParamUi::Toggle },
    color("Fog colour", 0xc9d2dc),
    spec("Fog", 80.0, 0.0, 100.0, " %", 0.5),
    spec("Keep nearer than", 50.0, 0.0, 100.0, " %", 0.5),
    spec("Key softness", 10.0, 0.0, 100.0, " %", 0.5),
];
const RELIGHT: [ParamSpec; 9] = [
    spec("Light X", 30.0, -100.0, 200.0, " %", 0.2),
    spec("Light Y", 30.0, -100.0, 200.0, " %", 0.2),
    spec("Height", 40.0, 0.0, 400.0, " %", 0.5),
    color("Colour", 0xffd9a8),
    spec("Intensity", 100.0, 0.0, 1000.0, " %", 0.5),
    spec("Darken the rest", 30.0, 0.0, 100.0, " %", 0.5),
    spec("Reach", 60.0, 1.0, 500.0, " %", 0.5),
    spec("Surface detail", 50.0, 0.0, 400.0, " %", 0.5),
    choice("Light", &["A lamp (from a point)", "The sun (from a direction)"]),
];
const DIRECTIONALBLUR: [ParamSpec; 3] = [
    spec("Angle", 0.0, -360.0, 360.0, "°", 0.5),
    spec("Length", 30.0, 0.0, 2000.0, " px", 0.5),
    choice("Shape", &["Even", "Smooth", "Trail one way"]),
];
const RADIALBLUR: [ParamSpec; 5] = [
    spec("Centre X", 50.0, -100.0, 200.0, " %", 0.2),
    spec("Centre Y", 50.0, -100.0, 200.0, " %", 0.2),
    choice("Type", &["Spin", "Zoom"]),
    spec("Amount", 10.0, 0.0, 360.0, "", 0.1),
    spec("Quality", 48.0, 4.0, 128.0, " samples", 0.5),
];
const MOTIONBLUR: [ParamSpec; 3] = [
    spec("Shutter angle", 180.0, 0.0, 720.0, "°", 1.0),
    spec("Shutter phase", -90.0, -360.0, 360.0, "°", 1.0),
    spec("Samples", 16.0, 2.0, 64.0, "", 0.2),
];
const PORTRAIT: [ParamSpec; 6] = [
    spec("Focus at", 100.0, 0.0, 100.0, " %", 0.5),
    spec("In focus range", 15.0, 0.0, 100.0, " %", 0.5),
    spec("Blur", 16.0, 0.0, 80.0, " px", 0.2),
    ParamSpec { name: "Blur what's nearer too", default: 1.0, min: 0.0, max: 1.0, suffix: "", speed: 1.0, ui: ParamUi::Toggle },
    spec("Bright spots", 30.0, 0.0, 200.0, " %", 0.5),
    ParamSpec { name: "Show what's in focus", default: 0.0, min: 0.0, max: 1.0, suffix: "", speed: 1.0, ui: ParamUi::Toggle },
];
const NIGHTVISION: [ParamSpec; 9] = [
    spec("Gain", 250.0, 0.0, 2000.0, " %", 1.0),
    color("Colour", 0x7dff6a),
    spec("Noise", 35.0, 0.0, 100.0, " %", 0.5),
    spec("Glow", 50.0, 0.0, 300.0, " %", 0.5),
    choice("Goggles", &["None", "One eyepiece", "Binoculars"]),
    spec("Edge softness", 40.0, 0.0, 100.0, " %", 0.5),
    spec("Scan lines", 0.0, 0.0, 100.0, " %", 0.5),
    spec("Flicker", 10.0, 0.0, 100.0, " %", 0.5),
    spec("Seed", 1.0, 0.0, 9999.0, "", 1.0),
];
const THERMALCAMERA: [ParamSpec; 8] = [
    choice("Palette", &["Iron", "Rainbow", "White hot", "Black hot", "Lava"]),
    choice("Heat from", &["Both", "Brightness", "Warm colours (skin, fire)"]),
    spec("Coldest at", 5.0, 0.0, 100.0, " %", 0.5),
    spec("Hottest at", 95.0, 0.0, 100.0, " %", 0.5),
    spec("Smoothing", 3.0, 0.0, 40.0, " px", 0.1),
    spec("Noise", 8.0, 0.0, 100.0, " %", 0.5),
    ParamSpec { name: "Scale bar", default: 0.0, min: 0.0, max: 1.0, suffix: "", speed: 1.0, ui: ParamUi::Toggle },
    spec("Seed", 1.0, 0.0, 9999.0, "", 1.0),
];
const INFRARED: [ParamSpec; 8] = [
    choice("Look", &["Black and white infrared", "Colour infrared (false colour)", "Golden (warm foliage)"]),
    spec("Foliage brightness", 100.0, 0.0, 300.0, " %", 0.5),
    spec("Sky darkness", 60.0, 0.0, 100.0, " %", 0.5),
    spec("Glow", 40.0, 0.0, 200.0, " %", 0.5),
    spec("Contrast", 110.0, 0.0, 300.0, " %", 0.5),
    spec("Grain", 15.0, 0.0, 100.0, " %", 0.5),
    spec("Mix", 100.0, 0.0, 100.0, " %", 0.5),
    spec("Seed", 1.0, 0.0, 9999.0, "", 1.0),
];
const XRAY: [ParamSpec; 8] = [
    color("Colour", 0xbfe4ff),
    ParamSpec { name: "Invert", default: 1.0, min: 0.0, max: 1.0, suffix: "", speed: 1.0, ui: ParamUi::Toggle },
    spec("Edges", 60.0, 0.0, 300.0, " %", 0.5),
    spec("Contrast", 130.0, 0.0, 400.0, " %", 0.5),
    spec("Glow", 50.0, 0.0, 300.0, " %", 0.5),
    spec("Noise", 10.0, 0.0, 100.0, " %", 0.5),
    spec("Brightness", 100.0, 0.0, 300.0, " %", 0.5),
    spec("Seed", 1.0, 0.0, 9999.0, "", 1.0),
];
const SPOTLIGHT: [ParamSpec; 15] = [
    spec("Centre X", 50.0, -100.0, 200.0, " %", 0.2),
    spec("Centre Y", 55.0, -100.0, 200.0, " %", 0.2),
    spec("Size", 50.0, 0.0, 300.0, " %", 0.2),
    spec("Roundness", 70.0, 5.0, 300.0, " %", 0.5),
    spec("Angle", 0.0, -360.0, 360.0, "°", 0.5),
    spec("Softness", 35.0, 0.0, 100.0, " %", 0.5),
    spec("Intensity", 120.0, 0.0, 1000.0, " %", 0.5),
    color("Colour", 0xfff1d6),
    spec("Darken the rest", 60.0, 0.0, 100.0, " %", 0.5),
    spec("Beam in the air", 25.0, 0.0, 200.0, " %", 0.5),
    spec("Lamp X", 50.0, -100.0, 200.0, " %", 0.2),
    spec("Lamp Y", -10.0, -100.0, 200.0, " %", 0.2),
    choice("Pattern", &["None", "Window", "Leaves", "Stripes", "Dots", "Rings"]),
    spec("Pattern size", 100.0, 10.0, 500.0, " %", 0.5),
    spec("Pattern turn", 0.0, -360.0, 360.0, "°", 0.5),
];
const GLEAM: [ParamSpec; 11] = [
    spec("Threshold", 80.0, 0.0, 100.0, " %", 0.5),
    spec("Length", 60.0, 0.0, 1000.0, " px", 0.5),
    choice("Points", &["4", "6", "8"]),
    spec("Angle", 45.0, -360.0, 360.0, "°", 0.5),
    spec("Brightness", 100.0, 0.0, 1000.0, " %", 0.5),
    color("Tint", 0xffffff),
    spec("Their own colour", 70.0, 0.0, 100.0, " %", 0.5),
    spec("Rainbow", 0.0, 0.0, 100.0, " %", 0.5),
    spec("Twinkle", 0.0, 0.0, 100.0, " %", 0.5),
    spec("Sharpness", 60.0, 0.0, 100.0, " %", 0.5),
    choice("Show", &["The picture and its glints", "The glints only"]),
];
const HALATION: [ParamSpec; 5] = [
    spec("Threshold", 70.0, 0.0, 100.0, " %", 0.5),
    spec("Size", 12.0, 0.0, 200.0, " px", 0.2),
    spec("Amount", 60.0, 0.0, 400.0, " %", 0.5),
    color("Colour", 0xff4a14),
    spec("Round the edges only", 70.0, 0.0, 100.0, " %", 0.5),
];
const CUSTOMFLARE: [ParamSpec; 20] = [
    spec("Light X", 30.0, -100.0, 200.0, " %", 0.2),
    spec("Light Y", 30.0, -100.0, 200.0, " %", 0.2),
    spec("Brightness", 100.0, 0.0, 1000.0, " %", 0.5),
    color("Colour", 0xfff0dc),
    spec("Glow size", 25.0, 0.0, 300.0, " %", 0.5),
    spec("Glow", 80.0, 0.0, 500.0, " %", 0.5),
    spec("Rays", 12.0, 0.0, 64.0, "", 0.1),
    spec("Ray length", 40.0, 0.0, 300.0, " %", 0.5),
    spec("Ray brightness", 20.0, 0.0, 500.0, " %", 0.5),
    spec("Streak length", 80.0, 0.0, 400.0, " %", 0.5),
    spec("Streak brightness", 60.0, 0.0, 500.0, " %", 0.5),
    color("Streak colour", 0x7fb6ff),
    spec("Halo radius", 30.0, 0.0, 200.0, " %", 0.5),
    spec("Halo brightness", 25.0, 0.0, 500.0, " %", 0.5),
    ParamSpec { name: "Rainbow halo", default: 1.0, min: 0.0, max: 1.0, suffix: "", speed: 1.0, ui: ParamUi::Toggle },
    spec("Ghosts", 6.0, 0.0, 24.0, "", 0.1),
    spec("Ghost size", 14.0, 0.0, 100.0, " %", 0.2),
    choice("Ghost shape", &["Round", "Hexagon", "Octagon"]),
    spec("Ghost brightness", 40.0, 0.0, 500.0, " %", 0.5),
    spec("Seed", 1.0, 0.0, 9999.0, "", 1.0),
];
const VOLUMETRIC: [ParamSpec; 11] = [
    spec("Light X", 50.0, -100.0, 200.0, " %", 0.2),
    spec("Light Y", 20.0, -100.0, 200.0, " %", 0.2),
    spec("Length", 70.0, 0.0, 100.0, " %", 0.5),
    spec("Intensity", 100.0, 0.0, 1000.0, " %", 0.5),
    color("Colour", 0xffe6b8),
    spec("Light depth", 0.0, 0.0, 100.0, " %", 0.5),
    spec("Threshold", 60.0, 0.0, 100.0, " %", 0.5),
    spec("Softness", 15.0, 0.0, 100.0, " %", 0.5),
    spec("Haze round the light", 40.0, 0.0, 300.0, " %", 0.5),
    spec("Fall-off", 50.0, 0.0, 100.0, " %", 0.5),
    choice("Show", &["The picture and its light", "The light only"]),
];
const FORCEFIELD: [ParamSpec; 16] = [
    spec("Centre X", 50.0, -100.0, 200.0, " %", 0.2),
    spec("Centre Y", 50.0, -100.0, 200.0, " %", 0.2),
    spec("Radius", 40.0, 0.0, 300.0, " %", 0.2),
    spec("Roundness", 100.0, 10.0, 300.0, " %", 0.5),
    color("Colour", 0x4fc3ff),
    spec("Rim", 100.0, 0.0, 500.0, " %", 0.5),
    spec("Fill", 12.0, 0.0, 100.0, " %", 0.5),
    choice("Pattern", &["None", "Hexagons", "Grid", "Ripples"]),
    spec("Pattern size", 30.0, 4.0, 400.0, " px", 0.2),
    spec("Pattern brightness", 40.0, 0.0, 300.0, " %", 0.5),
    spec("Bend the picture", 12.0, 0.0, 200.0, " px", 0.2),
    spec("Shimmer", 50.0, 0.0, 300.0, " %", 0.5),
    spec("Power", 100.0, 0.0, 100.0, " %", 0.5),
    spec("Hit at", -1.0, -1.0, 36000.0, " s", 0.01),
    spec("Hit angle", -45.0, -360.0, 360.0, "°", 0.5),
    spec("Hit size", 100.0, 10.0, 400.0, " %", 0.5),
];
const TELEPORT: [ParamSpec; 9] = [
    choice("Beam", &["In", "Out"]),
    spec("Start at", 0.0, 0.0, 36000.0, " s", 0.01),
    spec("Takes", 2.0, 0.05, 60.0, " s", 0.01),
    choice("Style", &["Sparkle column", "Scan line", "Pixel dissolve", "Pillar of light"]),
    color("Colour", 0x9fd8ff),
    spec("Glow", 100.0, 0.0, 400.0, " %", 0.5),
    spec("Sparkles", 60.0, 0.0, 200.0, " %", 0.5),
    spec("Column width", 30.0, 1.0, 200.0, " %", 0.5),
    spec("Seed", 1.0, 0.0, 9999.0, "", 1.0),
];
const SPARKLE: [ParamSpec; 16] = [
    spec("Head X", 50.0, -100.0, 200.0, " %", 0.2),
    spec("Head Y", 50.0, -100.0, 200.0, " %", 0.2),
    choice("Path", &["The head (keyframe Head X and Y)", "Mask 1", "Mask 2", "Mask 3", "Mask 4"]),
    spec("Progress along the mask", 0.0, 0.0, 100.0, " %", 0.2),
    spec("Trail length", 1.0, 0.05, 30.0, " s", 0.01),
    spec("Sparkles", 85.0, 0.0, 100.0, " %", 0.5),
    spec("Sparkle size", 12.0, 0.5, 200.0, " px", 0.1),
    spec("Spread", 40.0, 0.0, 2000.0, " px/s", 0.5),
    spec("Fall", 60.0, -2000.0, 2000.0, " px/s²", 0.5),
    spec("Twinkle", 60.0, 0.0, 100.0, " %", 0.5),
    color("Colour", 0xffd76a),
    color("Second colour", 0xa8e6ff),
    spec("Streak width", 3.0, 0.0, 100.0, " px", 0.1),
    spec("Streak brightness", 80.0, 0.0, 500.0, " %", 0.5),
    spec("Brightness", 100.0, 0.0, 1000.0, " %", 0.5),
    spec("Seed", 1.0, 0.0, 9999.0, "", 1.0),
];
const BLASTERFIRE: [ParamSpec; 18] = [
    spec("Gun X", 15.0, -100.0, 200.0, " %", 0.2),
    spec("Gun Y", 60.0, -100.0, 200.0, " %", 0.2),
    spec("Target X", 85.0, -100.0, 200.0, " %", 0.2),
    spec("Target Y", 45.0, -100.0, 200.0, " %", 0.2),
    spec("Fire at", 0.0, 0.0, 36000.0, " s", 0.01),
    spec("Shots", 3.0, 1.0, 50.0, "", 0.05),
    spec("Every", 0.25, 0.02, 10.0, " s", 0.005),
    spec("Speed", 2500.0, 50.0, 50000.0, " px/s", 5.0),
    spec("Bolt length", 90.0, 2.0, 2000.0, " px", 0.5),
    spec("Thickness", 4.0, 0.5, 100.0, " px", 0.1),
    color("Colour", 0xff2a2a),
    spec("White core", 70.0, 0.0, 100.0, " %", 0.5),
    spec("Glow", 16.0, 0.0, 300.0, " px", 0.2),
    spec("Spread", 1.5, 0.0, 45.0, "°", 0.05),
    spec("Muzzle flash", 100.0, 0.0, 500.0, " %", 0.5),
    spec("Impact flash", 100.0, 0.0, 500.0, " %", 0.5),
    spec("Scorch marks", 60.0, 0.0, 300.0, " %", 0.5),
    spec("Seed", 1.0, 0.0, 9999.0, "", 1.0),
];
const BLOODSPATTER: [ParamSpec; 12] = [
    spec("Hit X", 50.0, -100.0, 200.0, " %", 0.2),
    spec("Hit Y", 45.0, -100.0, 200.0, " %", 0.2),
    spec("At", 0.0, 0.0, 36000.0, " s", 0.01),
    spec("Direction", 0.0, -360.0, 360.0, "°", 0.5),
    spec("Spread", 70.0, 0.0, 360.0, "°", 0.5),
    spec("Amount", 60.0, 0.0, 100.0, " %", 0.5),
    spec("Drop size", 6.0, 0.5, 100.0, " px", 0.1),
    spec("Reach", 140.0, 1.0, 3000.0, " px", 0.5),
    color("Colour", 0x6e0606),
    spec("Shine", 40.0, 0.0, 100.0, " %", 0.5),
    spec("Run down", 0.0, 0.0, 100.0, " %", 0.5),
    spec("Seed", 1.0, 0.0, 9999.0, "", 1.0),
];
const BLOCKDISPLACEMENT: [ParamSpec; 10] = [
    spec("Block width", 80.0, 2.0, 4000.0, " px", 0.5),
    spec("Block height", 24.0, 2.0, 4000.0, " px", 0.5),
    spec("Amount", 40.0, 0.0, 2000.0, " px", 0.5),
    choice("Direction", &["Sideways", "Up and down", "Both"]),
    spec("Blocks moved", 30.0, 0.0, 100.0, " %", 0.5),
    spec("Changes a second", 8.0, 0.0, 60.0, "", 0.1),
    spec("Size variety", 50.0, 0.0, 100.0, " %", 0.5),
    spec("Colour split", 0.0, 0.0, 200.0, " px", 0.2),
    EDGES_STRETCH,
    spec("Seed", 1.0, 0.0, 9999.0, "", 1.0),
];
const DEREZ: [ParamSpec; 7] = [
    spec("Progress", 50.0, 0.0, 100.0, " %", 0.5),
    spec("Biggest blocks", 64.0, 2.0, 1000.0, " px", 0.5),
    spec("Randomness", 50.0, 0.0, 100.0, " %", 0.5),
    spec("Fade out at the end", 100.0, 0.0, 100.0, " %", 0.5),
    choice("Direction", &["Everywhere at once", "Left to right", "Right to left", "Top down", "Bottom up", "From the middle"]),
    spec("Sweep softness", 40.0, 0.0, 100.0, " %", 0.5),
    spec("Seed", 1.0, 0.0, 9999.0, "", 1.0),
];
const ENERGYDISTORTION: [ParamSpec; 13] = [
    spec("Amount", 15.0, 0.0, 500.0, " px", 0.2),
    spec("Size", 60.0, 2.0, 2000.0, " px", 0.5),
    spec("Speed", 2.0, 0.0, 30.0, "", 0.05),
    spec("Detail", 4.0, 1.0, 6.0, "", 0.05),
    spec("Glow", 40.0, 0.0, 400.0, " %", 0.5),
    color("Glow colour", 0x66ccff),
    choice("Area", &["Everywhere", "In a circle"]),
    spec("Area X", 50.0, -100.0, 200.0, " %", 0.2),
    spec("Area Y", 50.0, -100.0, 200.0, " %", 0.2),
    spec("Area radius", 30.0, 0.0, 300.0, " %", 0.2),
    spec("Area feather", 50.0, 0.0, 100.0, " %", 0.5),
    EDGES_STRETCH,
    spec("Seed", 1.0, 0.0, 9999.0, "", 1.0),
];
const FLUIDDISTORTION: [ParamSpec; 13] = [
    spec("Amount", 20.0, 0.0, 500.0, " px", 0.2),
    spec("Size", 150.0, 2.0, 4000.0, " px", 0.5),
    spec("Speed", 0.5, 0.0, 10.0, "", 0.02),
    spec("Swirl", 60.0, 0.0, 100.0, " %", 0.5),
    spec("Flow direction", 90.0, -360.0, 360.0, "°", 0.5),
    spec("Flow speed", 20.0, 0.0, 2000.0, " px/s", 0.5),
    choice("Area", &["Everywhere", "In a circle"]),
    spec("Area X", 50.0, -100.0, 200.0, " %", 0.2),
    spec("Area Y", 50.0, -100.0, 200.0, " %", 0.2),
    spec("Area radius", 30.0, 0.0, 300.0, " %", 0.2),
    spec("Area feather", 50.0, 0.0, 100.0, " %", 0.5),
    EDGES_STRETCH,
    spec("Seed", 1.0, 0.0, 9999.0, "", 1.0),
];
const SMOKEDISTORTION: [ParamSpec; 14] = [
    spec("Amount", 12.0, 0.0, 500.0, " px", 0.2),
    spec("Size", 90.0, 2.0, 4000.0, " px", 0.5),
    spec("Rise", 80.0, -2000.0, 2000.0, " px/s", 0.5),
    spec("Turbulence", 50.0, 0.0, 100.0, " %", 0.5),
    spec("Smoke tint", 15.0, 0.0, 100.0, " %", 0.5),
    color("Smoke colour", 0x9a9a9a),
    choice("Area", &["Everywhere", "In a circle", "From the bottom"]),
    spec("Area X", 50.0, -100.0, 200.0, " %", 0.2),
    spec("Area Y", 50.0, -100.0, 200.0, " %", 0.2),
    spec("Area radius", 30.0, 0.0, 300.0, " %", 0.2),
    spec("Area feather", 50.0, 0.0, 100.0, " %", 0.5),
    EDGES_STRETCH,
    spec("Seed", 1.0, 0.0, 9999.0, "", 1.0),
    spec("Height from the bottom", 50.0, 1.0, 100.0, " %", 0.5),
];
const GRADIENTRAMP: [ParamSpec; 17] = [
    choice("Shape", &["Linear", "Radial", "Four colours"]),
    spec("Point 1 X", 0.0, -100.0, 200.0, " %", 0.2),
    spec("Point 1 Y", 0.0, -100.0, 200.0, " %", 0.2),
    spec("Point 2 X", 100.0, -100.0, 200.0, " %", 0.2),
    spec("Point 2 Y", 100.0, -100.0, 200.0, " %", 0.2),
    color("Colour 1", 0x1b2a6b),
    color("Colour 2", 0xf2a65a),
    spec("Point 3 X", 100.0, -100.0, 200.0, " %", 0.2),
    spec("Point 3 Y", 0.0, -100.0, 200.0, " %", 0.2),
    spec("Point 4 X", 0.0, -100.0, 200.0, " %", 0.2),
    spec("Point 4 Y", 100.0, -100.0, 200.0, " %", 0.2),
    color("Colour 3", 0xd04a8a),
    color("Colour 4", 0x2fb5a8),
    spec("Blend", 50.0, 0.0, 100.0, " %", 0.5),
    spec("Opacity", 100.0, 0.0, 100.0, " %", 0.5),
    choice("Mix with the picture", &["Over it", "Multiply", "Screen", "Overlay", "Colour only"]),
    spec("Dither", 50.0, 0.0, 100.0, " %", 0.5),
];
const STROBE: [ParamSpec; 8] = [
    spec("Flashes a second", 4.0, 0.05, 60.0, "", 0.05),
    spec("Flash length", 30.0, 1.0, 100.0, " % of each", 0.5),
    choice("Flash", &["To a colour", "Blank (see-through)", "Invert", "Brighten (a strobe light)"]),
    color("Colour", 0xffffff),
    spec("Start at", 0.0, 0.0, 36000.0, " s", 0.01),
    spec("Random", 0.0, 0.0, 100.0, " %", 0.5),
    spec("Strength", 100.0, 0.0, 400.0, " %", 0.5),
    spec("Seed", 1.0, 0.0, 9999.0, "", 1.0),
];
const LIGHT_WRAP: [ParamSpec; 4] = [
    spec("Width", 12.0, 0.0, 500.0, " px", 0.2),
    spec("Amount", 60.0, 0.0, 200.0, " %", 0.5),
    spec("Background blur", 20.0, 0.0, 500.0, " px", 0.2),
    choice("Wrap", &["Over the edges", "Screen (brighten only)"]),
];
const TIME_DISPLACE: [ParamSpec; 3] = [
    spec("Most delay", 1.0, -10.0, 10.0, " s", 0.01),
    choice("Direction", &["Top now, bottom earlier", "Bottom now, top earlier", "Left now, right earlier", "Right now, left earlier"]),
    spec("Bands", 24.0, 2.0, 48.0, "", 0.1),
];
const HOLD_FRAME: [ParamSpec; 2] = [
    spec("Hold from", 1.0, 0.0, 36000.0, " s", 0.01),
    spec("Hold until", 3.0, -1.0, 36000.0, " s", 0.01),
];
const RIPPLE: [ParamSpec; 9] = [
    spec("Centre X", 50.0, -100.0, 200.0, " %", 0.2),
    spec("Centre Y", 50.0, -100.0, 200.0, " %", 0.2),
    spec("Height", 6.0, -200.0, 200.0, " px", 0.1),
    spec("Spacing", 60.0, 2.0, 2000.0, " px", 0.2),
    spec("Speed", 1.0, -20.0, 20.0, " rings/s", 0.02),
    spec("Phase", 0.0, -3600.0, 3600.0, "°", 1.0),
    spec("Fade out at", 0.0, 0.0, 300.0, " %", 0.5),
    choice("Push", &["Out and in (water)", "Round (sideways)"]),
    EDGES_STRETCH,
];
const WAVE_WARP: [ParamSpec; 8] = [
    choice("Wave", &["Sine", "Square", "Triangle", "Sawtooth", "Wobbly noise"]),
    spec("Height", 10.0, -500.0, 500.0, " px", 0.1),
    spec("Width", 60.0, 2.0, 4000.0, " px", 0.2),
    spec("Direction", 0.0, -360.0, 360.0, "°", 0.5),
    spec("Speed", 1.0, -20.0, 20.0, " waves/s", 0.02),
    spec("Phase", 0.0, -3600.0, 3600.0, "°", 1.0),
    choice("Pin", &["Nothing", "All edges", "Left and right", "Top and bottom"]),
    EDGES_STRETCH,
];
const POLAR: [ParamSpec; 2] = [spec("Amount", 100.0, 0.0, 100.0, " %", 0.5), choice("Conversion", &["Rectangular to polar", "Polar to rectangular"])];
const MIRROR: [ParamSpec; 3] = [
    spec("Centre X", 50.0, -100.0, 200.0, " %", 0.2),
    spec("Centre Y", 50.0, -100.0, 200.0, " %", 0.2),
    spec("Angle", 0.0, -360.0, 360.0, "°", 0.5),
];
const LENS_DISTORTION: [ParamSpec; 6] = [
    spec("Centre X", 50.0, -100.0, 200.0, " %", 0.2),
    spec("Centre Y", 50.0, -100.0, 200.0, " %", 0.2),
    spec("Distortion", 25.0, -100.0, 100.0, " %", 0.2),
    spec("Cubic", 0.0, -100.0, 100.0, " %", 0.2),
    spec("Zoom", 100.0, 10.0, 400.0, " %", 0.2),
    choice("Edges", &EDGES),
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
const BEAUTY: [ParamSpec; 9] = [
    choice("Mode", &beauty::MODES),
    spec("Smoothing", 50.0, 0.0, 100.0, " %", 0.5),
    spec("Detail kept", 35.0, 0.0, 100.0, " %", 0.5),
    spec("Brighten", 10.0, 0.0, 100.0, " %", 0.5),
    spec("Even out tone", 30.0, 0.0, 100.0, " %", 0.5),
    choice("Where", &beauty::AREAS),
    spec("Face area", 130.0, 60.0, 300.0, " %", 0.5),
    spec("Softness", 40.0, 0.0, 100.0, " %", 0.5),
    ParamSpec { name: "Show faces", default: 0.0, min: 0.0, max: 1.0, suffix: "", speed: 1.0, ui: ParamUi::Toggle },
];
const SKY: [ParamSpec; 14] = [
    choice("New sky", &sky::SOURCES),
    color("Top colour", 0x2f6fc4),
    color("Horizon colour", 0xcfe2f3),
    spec("Position X", 0.0, -200.0, 200.0, " %", 0.2),
    spec("Position Y", 0.0, -200.0, 200.0, " %", 0.2),
    spec("Scale", 100.0, 20.0, 500.0, " %", 0.5),
    spec("Follow the camera", 100.0, 0.0, 200.0, " %", 0.5),
    spec("Brightness", 0.0, -100.0, 100.0, " %", 0.5),
    spec("Blur", 0.0, 0.0, 100.0, " px", 0.2),
    spec("Edge softness", 2.0, 0.0, 50.0, " px", 0.1),
    spec("Edge shift", 0.0, -30.0, 30.0, " px", 0.1),
    spec("Match lighting", 40.0, 0.0, 100.0, " %", 0.5),
    spec("Opacity", 100.0, 0.0, 100.0, " %", 0.5),
    choice("View", &sky::VIEWS),
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
    pub const ALL: [EffectKind; 153] = [
        EffectKind::ColorCorrect,
        EffectKind::Levels,
        EffectKind::Curves,
        EffectKind::ColorWheels,
        EffectKind::HslQualifier,
        EffectKind::HueCurves,
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
        EffectKind::AnamorphicFlare,
        EffectKind::LightRays,
        EffectKind::LensDirt,
        EffectKind::NeonPath,
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
        EffectKind::Ocean,
        EffectKind::WaterSurface,
        EffectKind::Liquid,
        EffectKind::Liquid3D,
        EffectKind::FractalNoise,
        EffectKind::Lightning,
        EffectKind::PatchReplacer,
        EffectKind::ObjectRemoval,
        EffectKind::Paint,
        EffectKind::FaceBeauty,
        EffectKind::SkyReplace,
        EffectKind::FilmGrain,
        EffectKind::Glitch,
        EffectKind::ChromaticAberration,
        EffectKind::CameraShake,
        EffectKind::Bulge,
        EffectKind::Twirl,
        EffectKind::Ripple,
        EffectKind::WaveWarp,
        EffectKind::PolarCoordinates,
        EffectKind::Mirror,
        EffectKind::LensDistortion,
        EffectKind::AudioSpectrum,
        EffectKind::FilmLook,
        EffectKind::FilmDamage,
        EffectKind::AnalogueDamage,
        EffectKind::Abstraction,
        EffectKind::PrismBlur,
        EffectKind::TiltShift,
        EffectKind::Scanlines,
        EffectKind::Strobe,
        EffectKind::GradientRamp,
        EffectKind::SmokeDistortion,
        EffectKind::FluidDistortion,
        EffectKind::EnergyDistortion,
        EffectKind::Derez,
        EffectKind::BlockDisplacement,
        EffectKind::BloodSpatter,
        EffectKind::BlasterFire,
        EffectKind::Teleport,
        EffectKind::ForceField,
        EffectKind::CustomFlare,
        EffectKind::Halation,
        EffectKind::Gleam,
        EffectKind::Spotlight,
        EffectKind::XRay,
        EffectKind::Infrared,
        EffectKind::ThermalCamera,
        EffectKind::NightVision,
        EffectKind::MotionBlur,
        EffectKind::RadialBlur,
        EffectKind::DirectionalBlur,
        EffectKind::Echo,
        EffectKind::Liquify,
        EffectKind::DepthMap,
        EffectKind::Relight,
        EffectKind::PortraitBlur,
        EffectKind::VolumetricLight,
        EffectKind::SparkleTrail,
        EffectKind::LightWrap,
        EffectKind::TimeDisplacement,
        EffectKind::HoldFrame,
        EffectKind::ElectricArcs,
        EffectKind::LightSweep,
        EffectKind::Hud,
        EffectKind::Portal,
        EffectKind::Hologram,
        EffectKind::Starfield,
        EffectKind::Shockwave,
        EffectKind::HeatHaze,
        EffectKind::TurbulentDisplace,
        EffectKind::GradientMap,
        EffectKind::Aurora,
        EffectKind::Nebula,
        EffectKind::Clouds,
        EffectKind::BurnAway,
        EffectKind::Deflicker,
        EffectKind::Extrusion3d,
        EffectKind::Bend,
        EffectKind::Bevel,
        EffectKind::Extrude,
        EffectKind::Rotate3d,
        EffectKind::BoxBlur,
        EffectKind::ChannelBlur,
        EffectKind::CompoundBlur,
        EffectKind::SmartBlur,
        EffectKind::CameraLensBlur,
        EffectKind::CrossBlur,
        EffectKind::VectorBlur,
        EffectKind::Arithmetic,
        EffectKind::BlendColour,
        EffectKind::Calculations,
        EffectKind::ChannelMixer,
        EffectKind::Invert,
        EffectKind::ShiftChannels,
        EffectKind::SetChannels,
        EffectKind::SetMatte,
        EffectKind::SolidComposite,
        EffectKind::Minimax,
        EffectKind::AutoLevels,
        EffectKind::AutoContrast,
        EffectKind::AutoColor,
        EffectKind::Equalize,
        EffectKind::ChangeColor,
        EffectKind::ChangeToColor,
        EffectKind::ColorBalance,
        EffectKind::Exposure,
        EffectKind::LeaveColor,
        EffectKind::PhotoFilter,
        EffectKind::SelectiveColor,
        EffectKind::Tint,
        EffectKind::BlackWhite,
        EffectKind::ColorNeutralizer,
        EffectKind::ColorOffset,
        EffectKind::Toner,
        EffectKind::ThresholdRgb,
        EffectKind::Kernel,
        EffectKind::VideoLimiter,
    ];

    /// The one point of an effect that a motion tracker can carry (its x
    /// and y params, % of the layer) and what it is: a patch's target, or
    /// where a light is.
    pub fn tracked_point(self) -> Option<(usize, usize, &'static str)> {
        Some(match self {
            EffectKind::PatchReplacer => (patch::X, patch::Y, "target"),
            EffectKind::LensFlare => (flare::X, flare::Y, "light"),
            EffectKind::LightRays => (rays::X, rays::Y, "source"),
            EffectKind::LensDirt => (dirt::X, dirt::Y, "light"),
            EffectKind::AnamorphicFlare => (streak::X, streak::Y, "light"),
            EffectKind::Hud => (hud::X, hud::Y, "centre"),
            EffectKind::BurnAway => (burn::X, burn::Y, "burn point"),
            EffectKind::Extrude => (extrude::X, extrude::Y, "vanishing point"),
            _ => return None,
        })
    }

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
            EffectKind::ColorWheels => "Colour wheels",
            EffectKind::HslQualifier => "HSL qualifier",
            EffectKind::HueCurves => "Hue curves",
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
            EffectKind::LightRays => "Light rays",
            EffectKind::AnamorphicFlare => "Anamorphic flare",
            EffectKind::LensDirt => "Lens dirt",
            EffectKind::NeonPath => "Neon path",
            EffectKind::CameraShake => "Camera shake",
            EffectKind::Bulge => "Bulge",
            EffectKind::Twirl => "Twirl",
            EffectKind::Ripple => "Ripple",
            EffectKind::WaveWarp => "Wave warp",
            EffectKind::PolarCoordinates => "Polar coordinates",
            EffectKind::Mirror => "Mirror",
            EffectKind::LensDistortion => "Lens distortion",
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
            EffectKind::Ocean => "Ocean",
            EffectKind::WaterSurface => "Water surface",
            EffectKind::Liquid => "Liquid (pouring water)",
            EffectKind::Liquid3D => "Liquid (3D)",
            EffectKind::Disintegrate => "Disintegrate",
            EffectKind::FractalNoise => "Fractal noise",
            EffectKind::AudioSpectrum => "Audio spectrum",
            EffectKind::FilmLook => "Film look",
            EffectKind::FilmDamage => "Film damage",
            EffectKind::AnalogueDamage => "Analogue damage",
            EffectKind::Abstraction => "Abstraction",
            EffectKind::PrismBlur => "Prism blur",
            EffectKind::TiltShift => "Tilt-shift",
            EffectKind::TurbulentDisplace => "Turbulent displace",
            EffectKind::HeatHaze => "Heat haze",
            EffectKind::Shockwave => "Shockwave",
            EffectKind::Starfield => "Starfield",
            EffectKind::GradientMap => "Gradient map",
            EffectKind::Aurora => "Aurora",
            EffectKind::Nebula => "Nebula",
            EffectKind::Clouds => "Clouds",
            EffectKind::BurnAway => "Burn away",
            EffectKind::Extrusion3d => "3D Extrusion",
            EffectKind::Bend => "Bend",
            EffectKind::Bevel => "Bevel",
            EffectKind::Extrude => "Extrude",
            EffectKind::Rotate3d => "Rotate",
            EffectKind::BoxBlur => "Box blur",
            EffectKind::ChannelBlur => "Channel blur",
            EffectKind::CompoundBlur => "Compound blur",
            EffectKind::SmartBlur => "Smart blur",
            EffectKind::CameraLensBlur => "Camera lens blur",
            EffectKind::CrossBlur => "Cross blur",
            EffectKind::VectorBlur => "Vector blur",
            EffectKind::Arithmetic => "Arithmetic",
            EffectKind::BlendColour => "Blend",
            EffectKind::Calculations => "Calculations",
            EffectKind::ChannelMixer => "Channel mixer",
            EffectKind::Invert => "Invert",
            EffectKind::ShiftChannels => "Shift channels",
            EffectKind::SetChannels => "Set channels",
            EffectKind::SetMatte => "Set matte",
            EffectKind::SolidComposite => "Solid composite",
            EffectKind::Minimax => "Minimax",
            EffectKind::AutoLevels => "Auto levels",
            EffectKind::AutoContrast => "Auto contrast",
            EffectKind::AutoColor => "Auto colour",
            EffectKind::Equalize => "Equalize",
            EffectKind::ChangeColor => "Change colour",
            EffectKind::ChangeToColor => "Change to colour",
            EffectKind::ColorBalance => "Colour balance",
            EffectKind::Exposure => "Exposure",
            EffectKind::LeaveColor => "Leave colour",
            EffectKind::PhotoFilter => "Photo filter",
            EffectKind::SelectiveColor => "Selective colour",
            EffectKind::Tint => "Tint",
            EffectKind::BlackWhite => "Black & white",
            EffectKind::ColorNeutralizer => "Colour neutraliser",
            EffectKind::ColorOffset => "Colour offset",
            EffectKind::Toner => "Toner",
            EffectKind::ThresholdRgb => "Threshold RGB",
            EffectKind::Kernel => "Convolution kernel",
            EffectKind::VideoLimiter => "Video limiter",
            EffectKind::Deflicker => "Deflicker",
            EffectKind::Hologram => "Hologram",
            EffectKind::Portal => "Portal",
            EffectKind::Hud => "HUD element",
            EffectKind::LightSweep => "Light sweep",
            EffectKind::ElectricArcs => "Electric arcs",
            EffectKind::DepthMap => "Depth map",
            EffectKind::LightWrap => "Light wrap",
            EffectKind::TimeDisplacement => "Time displacement",
            EffectKind::HoldFrame => "Hold frame",
            EffectKind::SparkleTrail => "Sparkle trail",
            EffectKind::VolumetricLight => "Volumetric light",
            EffectKind::PortraitBlur => "Portrait blur",
            EffectKind::Relight => "Relight",
            EffectKind::Liquify => "Liquify",
            EffectKind::Echo => "Echo",
            EffectKind::DirectionalBlur => "Directional blur",
            EffectKind::RadialBlur => "Radial blur",
            EffectKind::MotionBlur => "Motion blur",
            EffectKind::NightVision => "Night vision",
            EffectKind::ThermalCamera => "Thermal camera",
            EffectKind::Infrared => "Infrared",
            EffectKind::XRay => "X-ray",
            EffectKind::Spotlight => "Spotlight",
            EffectKind::Gleam => "Gleam",
            EffectKind::Halation => "Halation",
            EffectKind::CustomFlare => "Custom flare",
            EffectKind::ForceField => "Force field",
            EffectKind::Teleport => "Teleport",
            EffectKind::BlasterFire => "Blaster fire",
            EffectKind::BloodSpatter => "Blood spatter",
            EffectKind::BlockDisplacement => "Block displacement",
            EffectKind::Derez => "Derez",
            EffectKind::EnergyDistortion => "Energy distortion",
            EffectKind::FluidDistortion => "Fluid distortion",
            EffectKind::SmokeDistortion => "Smoke distortion",
            EffectKind::GradientRamp => "Gradient ramp",
            EffectKind::Strobe => "Strobe",
            EffectKind::Scanlines => "Scanlines",

            EffectKind::Lightning => "Lightning",
            EffectKind::PatchReplacer => "Patch replacer",
            EffectKind::ObjectRemoval => "Object removal",
            EffectKind::Paint => "Paint / clone",
            EffectKind::FaceBeauty => "Face beauty",
            EffectKind::SkyReplace => "Sky replacement",
            EffectKind::Camera => "Camera",
            EffectKind::Light => "Light",
            EffectKind::Point => "Point",
        }
    }

    pub fn category(self) -> EffectCategory {
        match self {
            EffectKind::ColorCorrect | EffectKind::Levels | EffectKind::Curves | EffectKind::ColorWheels | EffectKind::HslQualifier | EffectKind::HueCurves | EffectKind::Lut => EffectCategory::Color,
            EffectKind::Blur | EffectKind::Sharpen => EffectCategory::BlurSharpen,
            EffectKind::ChromaKey | EffectKind::LumaKey | EffectKind::DifferenceKey | EffectKind::KeyCleaner | EffectKind::MatteChoker | EffectKind::RefineMatte => EffectCategory::Keying,
            EffectKind::Glow | EffectKind::DropShadow | EffectKind::Vignette | EffectKind::EnergyBlade | EffectKind::Camera | EffectKind::Light | EffectKind::Point => EffectCategory::Stylize,
            EffectKind::Particles | EffectKind::Shatter | EffectKind::MuzzleFlash | EffectKind::BulletHit | EffectKind::Fluid | EffectKind::Disintegrate | EffectKind::Ocean | EffectKind::WaterSurface | EffectKind::Liquid | EffectKind::Liquid3D => EffectCategory::Simulation,
            EffectKind::LensFlare | EffectKind::LightRays | EffectKind::AnamorphicFlare | EffectKind::LensDirt | EffectKind::NeonPath | EffectKind::LightLeaks | EffectKind::FilmGrain | EffectKind::NeonEdges | EffectKind::Halftone | EffectKind::Crt => EffectCategory::Stylize,
            EffectKind::WireRemoval | EffectKind::PatchReplacer | EffectKind::ObjectRemoval | EffectKind::Paint | EffectKind::FaceBeauty => EffectCategory::Repair,
            EffectKind::SkyReplace => EffectCategory::Keying,
            EffectKind::FractalNoise | EffectKind::Lightning | EffectKind::AudioSpectrum => EffectCategory::Generate,
            EffectKind::FilmLook | EffectKind::FilmDamage | EffectKind::AnalogueDamage | EffectKind::Abstraction | EffectKind::Scanlines => EffectCategory::Stylize,
            EffectKind::PrismBlur | EffectKind::TiltShift => EffectCategory::BlurSharpen,
            EffectKind::CameraShake
            | EffectKind::ChromaticAberration
            | EffectKind::Glitch
            | EffectKind::Bulge
            | EffectKind::Twirl
            | EffectKind::Ripple
            | EffectKind::WaveWarp
            | EffectKind::PolarCoordinates
            | EffectKind::Mirror
            | EffectKind::LensDistortion => EffectCategory::Distort,
            EffectKind::TurbulentDisplace => EffectCategory::Distort,
            EffectKind::HeatHaze => EffectCategory::Distort,
            EffectKind::Shockwave => EffectCategory::Distort,
            EffectKind::Starfield => EffectCategory::Generate,
            EffectKind::Aurora | EffectKind::Nebula | EffectKind::Clouds => EffectCategory::Generate,
            EffectKind::GradientMap => EffectCategory::Color,
            EffectKind::BurnAway => EffectCategory::Stylize,
            EffectKind::Extrusion3d => EffectCategory::Generate,
            EffectKind::Bend | EffectKind::Bevel | EffectKind::Extrude | EffectKind::Rotate3d => EffectCategory::Geometry,
            EffectKind::BoxBlur => EffectCategory::BlurSharpen,
            EffectKind::ChannelBlur => EffectCategory::BlurSharpen,
            EffectKind::CompoundBlur => EffectCategory::BlurSharpen,
            EffectKind::SmartBlur => EffectCategory::BlurSharpen,
            EffectKind::CameraLensBlur => EffectCategory::BlurSharpen,
            EffectKind::CrossBlur => EffectCategory::BlurSharpen,
            EffectKind::VectorBlur => EffectCategory::BlurSharpen,
            EffectKind::Arithmetic => EffectCategory::Channel,
            EffectKind::BlendColour => EffectCategory::Channel,
            EffectKind::Calculations => EffectCategory::Channel,
            EffectKind::ChannelMixer => EffectCategory::Channel,
            EffectKind::Invert => EffectCategory::Channel,
            EffectKind::ShiftChannels => EffectCategory::Channel,
            EffectKind::SetChannels => EffectCategory::Channel,
            EffectKind::SetMatte => EffectCategory::Channel,
            EffectKind::SolidComposite => EffectCategory::Channel,
            EffectKind::Minimax => EffectCategory::Channel,
            EffectKind::AutoLevels => EffectCategory::Color,
            EffectKind::AutoContrast => EffectCategory::Color,
            EffectKind::AutoColor => EffectCategory::Color,
            EffectKind::Equalize => EffectCategory::Color,
            EffectKind::ChangeColor => EffectCategory::Color,
            EffectKind::ChangeToColor => EffectCategory::Color,
            EffectKind::ColorBalance => EffectCategory::Color,
            EffectKind::Exposure => EffectCategory::Color,
            EffectKind::LeaveColor => EffectCategory::Color,
            EffectKind::PhotoFilter => EffectCategory::Color,
            EffectKind::SelectiveColor => EffectCategory::Color,
            EffectKind::Tint => EffectCategory::Color,
            EffectKind::BlackWhite => EffectCategory::Color,
            EffectKind::ColorNeutralizer => EffectCategory::Color,
            EffectKind::ColorOffset => EffectCategory::Color,
            EffectKind::Toner => EffectCategory::Color,
            EffectKind::ThresholdRgb => EffectCategory::Color,
            EffectKind::Kernel => EffectCategory::Color,
            EffectKind::VideoLimiter => EffectCategory::Color,
            EffectKind::Deflicker => EffectCategory::Repair,
            EffectKind::Hologram => EffectCategory::Stylize,
            EffectKind::Portal => EffectCategory::Generate,
            EffectKind::Hud => EffectCategory::Generate,
            EffectKind::LightSweep => EffectCategory::Stylize,
            EffectKind::ElectricArcs => EffectCategory::Generate,
            EffectKind::Echo => EffectCategory::Stylize,
            EffectKind::Liquify => EffectCategory::Distort,
            EffectKind::DepthMap | EffectKind::Relight => EffectCategory::Stylize,
            EffectKind::PortraitBlur => EffectCategory::BlurSharpen,
            EffectKind::VolumetricLight => EffectCategory::Stylize,
            EffectKind::SparkleTrail => EffectCategory::Generate,
            EffectKind::LightWrap => EffectCategory::Keying,
            EffectKind::TimeDisplacement | EffectKind::HoldFrame => EffectCategory::Stylize,
            EffectKind::DirectionalBlur => EffectCategory::BlurSharpen,
            EffectKind::RadialBlur => EffectCategory::BlurSharpen,
            EffectKind::MotionBlur => EffectCategory::BlurSharpen,
            EffectKind::NightVision => EffectCategory::Stylize,
            EffectKind::ThermalCamera => EffectCategory::Stylize,
            EffectKind::Infrared => EffectCategory::Stylize,
            EffectKind::XRay => EffectCategory::Stylize,
            EffectKind::Spotlight => EffectCategory::Stylize,
            EffectKind::Gleam => EffectCategory::Stylize,
            EffectKind::Halation => EffectCategory::Stylize,
            EffectKind::CustomFlare => EffectCategory::Stylize,
            EffectKind::ForceField => EffectCategory::Generate,
            EffectKind::Teleport => EffectCategory::Stylize,
            EffectKind::BlasterFire => EffectCategory::Generate,
            EffectKind::BloodSpatter => EffectCategory::Generate,
            EffectKind::BlockDisplacement => EffectCategory::Distort,
            EffectKind::Derez => EffectCategory::Distort,
            EffectKind::EnergyDistortion => EffectCategory::Distort,
            EffectKind::FluidDistortion => EffectCategory::Distort,
            EffectKind::SmokeDistortion => EffectCategory::Distort,
            EffectKind::GradientRamp => EffectCategory::Generate,
            EffectKind::Strobe => EffectCategory::Stylize,
            EffectKind::Plugin => EffectCategory::Plugins,
            EffectKind::OpenFx => EffectCategory::OpenFx,
        }
    }

    /// One line for the effects browser.
    pub fn description(self) -> &'static str {
        match self {
            EffectKind::ColorCorrect => "Exposure, contrast, saturation and white balance.",
            EffectKind::HslQualifier => "Grade just one colour range — skin, sky, a red car: pick it with the eyedropper or by hue, saturation and luminance, check it with Show matte, then shift its hue, saturation, brightness and contrast, or tint it.",
            EffectKind::HueCurves => "Curves against hue, luminance or saturation: turn one hue into another, make some colours more or less saturated or brighter, or take colour out of the shadows or highlights.",
            EffectKind::ColorWheels => "Grade with lift, gamma, gain and offset wheels: push the shadows, midtones, highlights or the whole picture towards a colour and brighten or darken each, then set contrast, saturation and hue. Watch the scopes as you go.",
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
            EffectKind::FilmLook => "A film look in one place, as Resolve's Film Look Creator: a print stock's colour and contrast (or bleach bypass, teal and orange, faded, black and white), soft highlight roll-off and lifted blacks, red halation round bright edges, bloom, grain, vignette and a little gate weave.",
            EffectKind::FilmDamage => "Old film: scratches running down the frame, dust and specks, a stray hair, flicker and a jittering frame, sepia toning and a vignette, changing frame to frame at the film's own rate.",
            EffectKind::AnalogueDamage => "Worn videotape: colour bleeding and shifted off the picture, tracking lines rolling through, the head-switching tear along the bottom, wobbling lines, tape noise, dropouts and washed-out colour.",
            EffectKind::Abstraction => "Turn footage into a painted, simplified picture: flat areas of colour that keep their edges (a Kuwahara filter), with dark outlines and fewer colours if wanted.",
            EffectKind::PrismBlur => "A blur that splits light into its colours as a prism does: out from a centre or one way, red one side and blue the other.",
            EffectKind::TiltShift => "The miniature look: a band of the picture sharp and the rest blurred more and more away from it, at any angle, with a little extra saturation and contrast.",
            EffectKind::TurbulentDisplace => "Bends the picture with rolling, cloudy noise, like the Turbulent Displace in After Effects: how far and how big the swirls are, how detailed, and how they change over time. For heat, water, flags, dreams and magic.",
            EffectKind::HeatHaze => "The shimmer over a hot road, a fire or an engine: the picture wobbles as rising air bends the light. Over the whole picture, rising from the bottom, or in a circle you place.",
            EffectKind::Shockwave => "A ring of bent air bursting out from a point, as from an explosion, a landing or a power-up: it travels out at the speed you set from the moment you set, fading as it goes, with a split of colour and a glint along the ring. Repeat it for a pulse. Drag its centre in the Viewer.",
            EffectKind::GradientMap => "Duotone and tritone: the picture redrawn in two or three colours of your choosing by how bright each part is (shadows, midtones, highlights), with where the middle falls, contrast, and how much of it to mix in.",
            EffectKind::Aurora => "The northern lights: glowing curtains of colour rippling across the sky, green at the foot fading to purple above, with fine rays, as many curtains as you like. Over the picture, on black, or on its own to put behind something.",
            EffectKind::Nebula => "Space: swirling clouds of coloured gas with a glowing heart, dark lanes of dust and stars, slowly turning if you like. Over the picture, on black, or on its own.",
            EffectKind::Clouds => "Soft clouds: a sky of them drifting by (choose the wind's direction and speed), or flying forwards through them. Over the picture, with a sky of their own, or on their own to put behind something.",
            EffectKind::Extrusion3d => "Makes the layer's picture solid: its shape (where it isn't see-through) extruded into 3D to a Depth, with bevelled edges, its sides in the picture's edge colours or a colour of their own, turned in 3D with perspective and lit by a light you place. Logos, titles and cut-outs become solid objects; key the turns to spin them.",
            EffectKind::Bend => "Bends the layer like a sheet of paper round a vertical or horizontal axis, curling its edges away (or towards you) with perspective and shading. Key Bend to roll it up or unroll it.",
            EffectKind::Bevel => "Bevelled edges: the picture's outline (a logo or a cut-out) or the layer's whole frame lit as if raised, bright on the side towards the light and shaded on the other.",
            EffectKind::Extrude => "Flat-style extrusion: the layer's shape drawn out in a direction (a long-shadow look) or towards a vanishing point, with its sides in a darker shade of the picture or a colour of their own.",
            EffectKind::Rotate3d => "Turns the layer like a card in 3D, round X, Y and Z about a point you choose, with perspective; its back can show the picture, darkened, or nothing. Key the turns to flip or spin it.",
            EffectKind::BoxBlur => "A fast blur that averages each pixel with its neighbours in a box, as many times as you like: once is blocky, three times is nearly Gaussian. Just across, just down, or both; the edges' own pixels repeated so they don't fade.",
            EffectKind::ChannelBlur => "Blurs the red, green, blue and alpha channels each by their own amount: soft colour fringes, a blurred matte under a sharp picture, or a dreamy red bloom.",
            EffectKind::CompoundBlur => "Blurs some places more than others, by a map: the layer's own brightness or opacity, or a gradient sharp in the middle, at the top, at the bottom or in a band (a quick depth of field).",
            EffectKind::SmartBlur => "Blurs within areas of like colour but keeps the edges between them sharp: smooths skin, noise and texture without softening outlines. Or shows just the edges it finds, in white on black or over the picture.",
            EffectKind::CameraLensBlur => "A real lens's out-of-focus blur: the iris's shape (round or with blades) shows in bright spots, which bloom into bokeh, rather than a smooth Gaussian smear.",
            EffectKind::CrossBlur => "Blurs across and down separately and combines the two, as an average or added as light: a soft cross-shaped glow on highlights and text.",
            EffectKind::VectorBlur => "Smears the picture along its own contours (or across them), following the shapes of its light and dark: brushed, painterly and flowing looks.",
            EffectKind::Arithmetic => "Simple sums on each channel's 0–255 value with a number of its own: and, or, exclusive or, add, subtract, difference, minimum, maximum, block above or below, slice, multiply and screen. Clip the result or let it wrap round.",
            EffectKind::BlendColour => "Blends the layer with a colour: cross-faded, the colour only (keeping the layer's light and dark), a tint, or only where it darkens or lightens.",
            EffectKind::Calculations => "Combines two of the layer's own channels with a blend mode — say its red multiplied by its luminance — into a new picture: for mattes, high-contrast looks and channel tricks.",
            EffectKind::ChannelMixer => "Makes each output channel from a mix of the red, green and blue, plus a constant: swap channels, a custom black and white (monochrome), or an infrared look.",
            EffectKind::Invert => "Inverts the colours (a negative), or just one channel: red, green, blue, alpha, luminance, hue, lightness or saturation; blended with the original as much as you like.",
            EffectKind::ShiftChannels => "Takes each channel from another: red from blue, alpha from luminance, and so on (or full, half or off) — swapped colours, or a matte from the picture's own brightness.",
            EffectKind::SetChannels => "Sets each channel from one of the layer's own (or full, half or off), each inverted if you like, then put back together: build a matte, isolate a channel, or reverse one.",
            EffectKind::SetMatte => "Replaces the layer's alpha with one of its own channels — its luminance, red, saturation… — inverted if you like: a quick luma matte, or cut a layer out by its brightness.",
            EffectKind::SolidComposite => "Puts the layer over a solid colour, each with an opacity of its own, with a blend mode: fills its see-through parts, or tints it by blending.",
            EffectKind::Minimax => "Each pixel takes the darkest (minimum) or brightest (maximum) value within a radius — spreading or shrinking light areas and mattes — or one then the other to close holes or remove specks.",
            EffectKind::AutoLevels => "Sets each channel's darkest and brightest points automatically (ignoring a few stray pixels at each end, the clip): full contrast and, as each channel is stretched on its own, a colour cast often taken out.",
            EffectKind::AutoContrast => "Stretches the picture's contrast automatically, all channels alike, so its darkest part is black and its brightest white — the colours stay as they are.",
            EffectKind::AutoColor => "Takes out a colour cast automatically: each channel stretched to full range and the midtones made neutral grey.",
            EffectKind::Equalize => "Spreads the picture's brightness levels evenly from black to white (histogram equalisation), each channel on its own or by brightness alone: hidden detail in flat, murky pictures.",
            EffectKind::ChangeColor => "Changes one colour: pick it, and shift its hue, lightness and saturation — the matching area found by a tolerance and softness, in RGB, by hue or by chroma; or show the mask it makes.",
            EffectKind::ChangeToColor => "Changes one colour to another: everything near the From colour (by hue, lightness and saturation tolerances, softened) takes the To colour's hue — and its lightness and saturation too, if you choose.",
            EffectKind::ColorBalance => "Shifts the balance of red, green and blue in the shadows, midtones and highlights separately, keeping the brightness if you like: warm the highlights, cool the shadows.",
            EffectKind::Exposure => "Adjusts the picture as a camera's exposure would, in stops of light (each doubling or halving it), with an offset for the blacks and a gamma correction for the midtones.",
            EffectKind::LeaveColor => "Takes the colour out of everything except one colour you pick (the red dress in a black-and-white world), with a tolerance and soft edges, matched by RGB or by hue.",
            EffectKind::PhotoFilter => "A coloured filter over the lens: warming, cooling, the colours of the rainbow, sepia, deep tones or underwater — or a colour of your own — at a density, keeping the brightness if you like.",
            EffectKind::SelectiveColor => "Adjusts the amounts of cyan, magenta, yellow and black in particular colour ranges — the reds, yellows, greens, cyans, blues, magentas, whites, neutrals or blacks — as printers do: three ranges at once, relative or absolute.",
            EffectKind::Tint => "Maps the picture's blacks and whites to two colours of your choice (everything between, between them), blended in by an amount: a quick monochrome in any two colours.",
            EffectKind::BlackWhite => "Turns the picture black and white with control over how light each colour becomes — reds, yellows, greens, cyans, blues and magentas — and an optional tint, as a photographer's coloured filters would.",
            EffectKind::ColorNeutralizer => "Takes colour casts out of the shadows, midtones and highlights separately: pick the colour each should be neutral grey in (the cast), and it's balanced away.",
            EffectKind::ColorOffset => "Cycles the red, green and blue values round by a phase each, wrapping over, solarising or folding back where they pass the end: psychedelic colour shifts that loop as you animate them.",
            EffectKind::Toner => "Tones the picture in two, three or five colours by its brightness — highlights, brights, midtones, darktones and shadows — or solarised: duotones, sepia and split tones in a step.",
            EffectKind::ThresholdRgb => "Turns each of red, green and blue fully on or off at a threshold of its own, giving at most eight flat colours: graphic, pop-art and stencil looks.",
            EffectKind::Kernel => "Each pixel becomes a weighted sum of itself and its eight neighbours — nine numbers of your choosing, divided and offset — the building block of sharpening, embossing, edge finding and blurs.",
            EffectKind::VideoLimiter => "Keeps the picture within broadcast-safe limits: brightness held between a floor and a ceiling and saturation held below a maximum, by pulling down brightness, saturation or both.",
            EffectKind::BurnAway => "The layer burns away like paper: a ragged front of glowing embers and charred edges sweeps across it (everywhere at once, from a side, the middle or a point you place) as Burnt goes from 0 to 100 %. Key Burnt to animate it.",
            EffectKind::Deflicker => "Evens out flicker in timelapses and under flickering lights. To analyse the clip, right-click it and choose Deflicker (or Timeline → Deflicker selected clip): it keys Brightness (and Red, Green and Blue) frame by frame to follow its brightness, smoothed over the frames you choose. Or set them by hand.",
            EffectKind::Starfield => "Stars: a night sky that twinkles, stars drifting past in layers (nearer ones faster), or the jump to warp speed, stars streaking out from a point you place. Over the picture, on black, or on their own to put behind something.",
            EffectKind::Hologram => "Turns the layer into a projected hologram: one glowing colour, see-through, with scanlines rolling over it, edges that glow, flicker, and glitches that tear across it now and then.",
            EffectKind::Portal => "A ring of energy that opens in the air: sparks spinning round its rim in two colours, the air rippling round it, and inside a swirl of the picture, darkness, or a hole you can see another shot through (put it on the track below). Keyframe Open to open and close it. Drag its centre in the Viewer.",
            EffectKind::Hud => "Sci-fi screen graphics drawn over the picture: a targeting reticle, a radar sweep, rings of ticks turning at different speeds, a readout of moving bars, corner brackets, or one of 30 reticle styles (scopes, compasses, squares, hexagons, arcs, X sights, wireframes with readouts, rings and gauges). Drag it in the Viewer, keyframe Draw on to bring it in. Turn up Glow for the lit look (its Glow size and, with Own glow colour, its own Glow colour) and Fill for scanlines inside. Lock-on closes it in on its target, then it blinks and turns the Locked colour. Add a readout beside it in your own words, with live values ({x}, {y}, {time}, {rand}, {lock}). Attach it to a track (Track panel → Apply to) to follow a face or object.",
            EffectKind::LightSweep => "A sheen of light passing over a title or logo, like light catching chrome or glass: its angle, width and softness, when it passes and how long it takes (or keyframe Position), and whether it lights only the shape, only its bright parts, or everything.",
            EffectKind::ElectricArcs => "Crackling electricity crawling over the edges of the layer’s shape — a keyed person, a title, a logo — and jumping off them: thin, branching, flickering arcs with a glow, in the colour you choose.",
            EffectKind::DepthMap => "How far away each part of the footage is, worked out by the depth model (Work out depth): shown as a picture (near is white), as fog that thickens with distance, or as a key that keeps what's near",
            EffectKind::LightWrap => "For a keyed or cut-out subject: the light of whatever is behind it spills round its edges, so it sits in the shot instead of looking pasted on",
            EffectKind::TimeDisplacement => "Slit-scan: each band of the picture from a different moment, so moving things stretch, bend and smear across it (rows or columns, earlier towards one side)",
            EffectKind::HoldFrame => "Freezes the picture on one frame from a moment until another, while the clip goes on: its other effects keep moving, and nothing is cut or moved on the timeline",
            EffectKind::SparkleTrail => "A magic trail behind a moving point: a glowing streak and sparkles shed along it that drift, fall, twinkle and fade. Keyframe the Head (drag it in the Viewer) or send it along one of the layer's masks",
            EffectKind::VolumetricLight => "Shafts of light through hazy air from a light you place, shadowed by what stands in front of it: by the footage's depth (Work out depth), or by its dark parts",
            EffectKind::PortraitBlur => "A portrait mode: what's in focus stays sharp and everything nearer or farther blurs like a wide-open lens, round highlights and all, by the footage's depth (Work out depth)",
            EffectKind::Relight => "A light added after the shot: the depth model works out the footage's shape (Work out depth), and the light shades it, brighter where surfaces face it and nearer to it",
            EffectKind::Liquify => "Push, twirl, pinch and swell the picture with a brush in the Viewer",
            EffectKind::Echo => "Copies of the layer from a moment before, fading behind it: trails, ghosting and stutter",
            EffectKind::DirectionalBlur => "A blur one way only, as if the picture moved in a straight line: at an angle, so long, evenly, smoothly or as a trail behind",
            EffectKind::RadialBlur => "A blur round a point (Spin, as if turning) or out from it (Zoom, as if rushing towards it)",
            EffectKind::MotionBlur => "Blur from the layer's own movement (its position, scale and rotation keyframes, and its parent's), as a camera's shutter would: longer with a wider Shutter angle",
            EffectKind::NightVision => "The look through night-vision goggles: the dark lifted and tinted green, glowing highlights, crawling noise, and the round eyepiece if you like",
            EffectKind::ThermalCamera => "A heat camera: the picture as heat, from its brightness or its warm colours, in a thermal palette (iron, rainbow, white hot, black hot, lava), with a scale bar if you like",
            EffectKind::Infrared => "Infrared photography: leaves and grass turn white (or pink and red in colour infrared), skies go dark, with the soft glow infrared film has",
            EffectKind::XRay => "An X-ray film look: the picture inverted and tinted cold, edges and dense parts glowing, with soft bloom and film noise",
            EffectKind::Spotlight => "A spotlight thrown onto the scene: a pool of light (round or slanted, soft or hard), the rest darkened, a beam through the air from where the lamp is, and a pattern in it if you like (window, leaves, stripes, dots, rings)",
            EffectKind::Gleam => "Star-shaped glints on the bright parts of the picture: 4, 6 or 8 points, in their own colour or a tint, rainbow-fringed if you like, twinkling",
            EffectKind::Halation => "The red-orange glow film gets round bright things: light spreading out from highlights into the darker parts next to them",
            EffectKind::CustomFlare => "A lens flare built from its parts: glow, starburst rays, a streak, a halo ring (rainbow if you like) and ghosts (round or many-sided) along the line through the middle, each part sized and as bright as you want",
            EffectKind::ForceField => "A shield bubble: a glowing rim, a pattern of hexagons, a grid or ripples flowing over it, the picture behind bent as through glass, and ripples spreading from where it is hit",
            EffectKind::Teleport => "Beaming in or out: the layer (a cut-out person or thing) appears or vanishes in a column of sparkles, a scan line, a pixel dissolve or a pillar of light",
            EffectKind::BlasterFire => "Blaster bolts that fire on their own, from a gun to a target you drag in the Viewer: when to fire, how many shots and how often, their speed, length, colour and glow, a little spread, muzzle and impact flashes, and scorch marks that glow and fade",
            EffectKind::BloodSpatter => "A spray of blood from a hit: a burst of drops flung out in a direction, landing as splats and streaks round the spot, glistening, and running down if you like",
            EffectKind::BlockDisplacement => "Rectangular blocks of the picture shoved out of place, as a broken signal does: how big the blocks are, how far and which way they move, how many of them, how often they change, with their colours split apart if you like",
            EffectKind::Derez => "The picture breaking down into ever bigger blocks and falling apart (or building back up, keyframed the other way): everywhere at once, sweeping across, or from the middle",
            EffectKind::EnergyDistortion => "The air rippling with energy: a fast, crackling distortion that bends the picture, with an electric glow along its ridges if you like, everywhere or in a circle",
            EffectKind::FluidDistortion => "The picture seen through moving liquid: slow swirls and flowing ripples that bend it, drifting in a direction, everywhere or in a circle",
            EffectKind::SmokeDistortion => "Rising, curling smoke bending the picture behind it — the wavering air above a fire or out of a vent — with a faint smoky tint if you like, everywhere, from the bottom or in a circle",
            EffectKind::GradientRamp => "A gradient: from one colour to another in a line or a circle, or four colours each at a point and blended between, over the picture or blended with it",
            EffectKind::Strobe => "A strobe: the picture flashing to a colour, blanking out, inverting or lit by bright flashes, so many times a second, each flash so long, regularly or at random",
            EffectKind::Scanlines => "Lines across the picture as on a screen: their spacing, thickness, softness and angle, rolling if wanted, darkening or as red, green and blue stripes.",
            EffectKind::AudioSpectrum => "An audio visualiser: the sound as bars, a line, dots or a waveform, drawn along a line across the layer (its start and end), rising with the sound. The sound is the clip's own (or its linked audio), or whatever plays on an audio track — put it on a solid or an adjustment layer over a music track. Bands are spaced by pitch between the lowest and highest notes shown.",
            EffectKind::FractalNoise => "Clouds, smoke, liquid, marble or turbulent patterns that drift and evolve: in your colours over the layer, or as its transparency (a smoky reveal, a texture for other effects).",
            EffectKind::Lightning => "Jagged, branching bolts of lightning or electricity between two points (drag them in the Viewer) or all round one, struck afresh several times a second, flickering and lighting the layer.",
            EffectKind::PatchReplacer => "Covers a blemish, a logo or a sign with a patch of the picture from beside it (drag both in the Viewer), its light matched to the surroundings so the join doesn't show. Keyframe or track the target: the source moves with it.",
            EffectKind::ObjectRemoval => "Content-aware fill: removes whatever a mask set to \"Object to remove\" covers (a person, a sign, a boom), filling it from the other frames where what's behind shows, or making it up from the picture round it. Worked out in the background over the clip.",
            EffectKind::FaceBeauty => "Finds the faces in the clip and follows them (Find faces), then smooths their skin while keeping its detail, evens out its tone and brightens it a touch; or blurs or pixelates them to hide who they are. Can smooth all the skin in the picture instead.",
            EffectKind::SkyReplace => "A new sky behind the footage: the sky in every frame is found by the object model (Find the sky), its edge softened round trees and buildings, the new sky (a picture, or a gradient) moved with the camera, and the footage's light matched to it.",
            EffectKind::Paint => "Paint in the Viewer: clone the picture from somewhere else (Alt-click where to copy from), brush on a colour, or erase strokes. Each stroke lasts a frame, from there on, or the whole clip.",
            EffectKind::Fluid => "Coloured smoke or ink poured from a source into still air, swirling and rising, flowing round the layer's own shape if you like (pushed about by it as it moves): a fluid simulation, worked out from the clip's start.",
            EffectKind::Ocean => "An open sea out to the horizon: waves raised by the wind and a swell, choppy crests with foam, the sky and the sun on the water, seen from a camera you place. Full scene, the water alone (transparent sky), or the water under the layer. With Solid objects keyed from the layer (rocks, a pier, a boat on a green screen), they sit in the sea: waves bounce off them, white water where the sea meets them, spray where the waves hit, and a wake and foam behind them as they move.",
            EffectKind::WaterSurface => "A water surface from above: ripples from rain and drops, waves rolling in, bouncing off solid objects (the layer's shape or a green screen) and pushed by them as they move, with caustics on the floor. A wave simulation, worked out from the clip's start.",
            EffectKind::Liquid3D => "Water poured in three dimensions: a liquid simulation in a box behind the layer, seen through a camera, so a pour pointed towards the camera splashes out of the picture, the drops growing as they come. It fills and spreads over the floor, flows round and splashes off solid objects (the layer's shape or what's in front of a green screen, given a thickness), and is drawn as a surface: the picture behind bent through it, the sky in it, highlights, its colour deeper where it's thicker, foam where it sprays.",
            EffectKind::Liquid => "Water poured from a source, flowing, filling and splashing off solid objects (the layer's shape, or what's in front of a green or blue screen) and the frame's floor and walls. A liquid simulation, worked out from the clip's start; the water alone comes out with transparency.",
            EffectKind::LensFlare => "A flare from a bright light: glow, streak, halo and ghost reflections across the frame. Choose a lens from the library (cinematic sun, anamorphic, hexagon iris, sci-fi and more) and drag the light in the Viewer. To keep it on a moving lamp or the sun, track that in the Track panel and apply the track to the flare.",
            EffectKind::LightRays => "Rays of light streaming out from a point through the bright parts of the layer (or all of it), like sun through trees or a logo's god rays. Drag the source in the Viewer, or apply a motion track to it (Track panel) so it stays on what it shines from.",
            EffectKind::AnamorphicFlare => "Long horizontal streaks out of the bright parts of the picture (lamps, headlights, highlights), as an anamorphic lens draws them, or from a light you place (and can apply a motion track to, in the Track panel).",
            EffectKind::LensDirt => "Smudges and specks on the lens that light up where bright light hits them: from the bright parts of the picture, or a light you place (which a motion track can carry, from the Track panel).",
            EffectKind::NeonPath => "A glowing neon tube along one of the layer's masks: draw a mask (set its Use to A path) and pick it as the Path, or on a title pick The letters to outline the text itself; then write it on, dash it or make it flicker. Show the layer at 0 % leaves only the tube.",
            EffectKind::Bulge => "Swell a round area out like a magnifying glass, or pinch it in (a dent) with a negative amount; Sphere wraps it on a ball instead. Drag its centre in the Viewer.",
            EffectKind::Twirl => "Turn the picture round a point, most in the middle and less towards the edge of the circle: a twirl, a vortex or a whirlpool. Drag its centre in the Viewer.",
            EffectKind::Ripple => "Rings spreading out from a point, as when a stone drops in water: how high and far apart they are, how fast they travel, and where they fade out. Drag its centre in the Viewer.",
            EffectKind::WaveWarp => "Waves across the whole picture: sine, square, triangle, sawtooth or a wobbly noise, pushed whichever way, moving at a speed you set; pin the edges to keep the frame still.",
            EffectKind::PolarCoordinates => "Bend the picture round into a circle (rectangular to polar: a \"tiny planet\" from a wide shot), or unroll a circle into a strip; Amount goes between the two.",
            EffectKind::Mirror => "Reflect one side of a line onto the other: set where the line goes through and its angle (180° more mirrors the other side). Drag its centre in the Viewer.",
            EffectKind::LensDistortion => "Barrel (bulging out) or pincushion (pinched in) distortion, as a wide or long lens gives: add it for a look, or take a lens's out with the opposite amount. Cubic shapes the edges; Zoom fills the frame again.",
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
            EffectKind::ColorWheels => &WHEELS,
            EffectKind::HslQualifier => &QUALIFIER,
            EffectKind::HueCurves => &HUE_CURVES,
            EffectKind::Curves => &CURVES,
            EffectKind::Sharpen => &SHARPEN,
            EffectKind::Glow => &GLOW,
            EffectKind::DropShadow => &DROP_SHADOW,
            EffectKind::Lut => &LUT,
            EffectKind::EnergyBlade => &BLADE,
            EffectKind::Particles => &PARTICLES,
            EffectKind::LensFlare => &LENS_FLARE,
            EffectKind::LightRays => &LIGHT_RAYS,
            EffectKind::AnamorphicFlare => &ANAMORPHIC,
            EffectKind::LensDirt => &LENS_DIRT,
            EffectKind::NeonPath => &NEON_PATH,
            EffectKind::CameraShake => &CAMERA_SHAKE,
            EffectKind::Bulge => &BULGE,
            EffectKind::Twirl => &TWIRL,
            EffectKind::Ripple => &RIPPLE,
            EffectKind::WaveWarp => &WAVE_WARP,
            EffectKind::PolarCoordinates => &POLAR,
            EffectKind::Mirror => &MIRROR,
            EffectKind::LensDistortion => &LENS_DISTORTION,
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
            EffectKind::Ocean => &OCEAN,
            EffectKind::WaterSurface => &WATER_SURFACE,
            EffectKind::Liquid => &LIQUID,
            EffectKind::Liquid3D => &LIQUID3D,
            EffectKind::Disintegrate => &DISINTEGRATE,
            EffectKind::FractalNoise => &FRACTAL,
            EffectKind::AudioSpectrum => &AUDIO_VIS,
            EffectKind::FilmLook => &FILM_LOOK,
            EffectKind::FilmDamage => &FILM_DAMAGE,
            EffectKind::AnalogueDamage => &ANALOGUE,
            EffectKind::Abstraction => &ABSTRACTION,
            EffectKind::PrismBlur => &PRISM,
            EffectKind::TiltShift => &TILT_SHIFT,
            EffectKind::TurbulentDisplace => &TURBULENTDISPLACE,
            EffectKind::HeatHaze => &HEATHAZE,
            EffectKind::Shockwave => &SHOCKWAVE,
            EffectKind::Starfield => &STARFIELD,
            EffectKind::GradientMap => &GRADIENT_MAP,
            EffectKind::Aurora => &AURORA,
            EffectKind::Nebula => &NEBULA,
            EffectKind::Clouds => &CLOUDS,
            EffectKind::BurnAway => &BURN,
            EffectKind::Extrusion3d => &EXTRUSION,
            EffectKind::Bend => &BEND,
            EffectKind::Bevel => &BEVEL,
            EffectKind::Extrude => &EXTRUDE,
            EffectKind::Rotate3d => &ROTATE3D,
            EffectKind::BoxBlur => &BOX_BLUR,
            EffectKind::ChannelBlur => &CHANNEL_BLUR,
            EffectKind::CompoundBlur => &COMPOUND_BLUR,
            EffectKind::SmartBlur => &SMART_BLUR,
            EffectKind::CameraLensBlur => &LENS_BLUR,
            EffectKind::CrossBlur => &CROSS_BLUR,
            EffectKind::VectorBlur => &VECTOR_BLUR,
            EffectKind::Arithmetic => &ARITHMETIC,
            EffectKind::BlendColour => &BLEND_COLOUR,
            EffectKind::Calculations => &CALCULATIONS,
            EffectKind::ChannelMixer => &CHANNEL_MIXER,
            EffectKind::Invert => &INVERT,
            EffectKind::ShiftChannels => &SHIFT_CHANNELS,
            EffectKind::SetChannels => &SET_CHANNELS,
            EffectKind::SetMatte => &SET_MATTE,
            EffectKind::SolidComposite => &SOLID_COMPOSITE,
            EffectKind::Minimax => &MINIMAX,
            EffectKind::AutoLevels => &AUTO_LEVELS,
            EffectKind::AutoContrast => &AUTO_CONTRAST,
            EffectKind::AutoColor => &AUTO_COLOR,
            EffectKind::Equalize => &EQUALIZE,
            EffectKind::ChangeColor => &CHANGE_COLOR,
            EffectKind::ChangeToColor => &CHANGE_TO_COLOR,
            EffectKind::ColorBalance => &COLOR_BALANCE,
            EffectKind::Exposure => &EXPOSURE,
            EffectKind::LeaveColor => &LEAVE_COLOR,
            EffectKind::PhotoFilter => &PHOTO_FILTER,
            EffectKind::SelectiveColor => &SELECTIVE_COLOR,
            EffectKind::Tint => &TINT,
            EffectKind::BlackWhite => &BLACK_WHITE,
            EffectKind::ColorNeutralizer => &COLOR_NEUTRALIZER,
            EffectKind::ColorOffset => &COLOR_OFFSET,
            EffectKind::Toner => &TONER,
            EffectKind::ThresholdRgb => &THRESHOLD_RGB,
            EffectKind::Kernel => &KERNEL,
            EffectKind::VideoLimiter => &VIDEO_LIMITER,
            EffectKind::Deflicker => &DEFLICKER,
            EffectKind::Hologram => &HOLOGRAM,
            EffectKind::Portal => &PORTAL,
            EffectKind::Hud => &HUD,
            EffectKind::LightSweep => &LIGHTSWEEP,
            EffectKind::ElectricArcs => &ELECTRICARCS,
            EffectKind::Echo => &ECHO,
            EffectKind::Liquify => &LIQUIFY,
            EffectKind::DepthMap => &DEPTH_MAP,
            EffectKind::Relight => &RELIGHT,
            EffectKind::PortraitBlur => &PORTRAIT,
            EffectKind::VolumetricLight => &VOLUMETRIC,
            EffectKind::SparkleTrail => &SPARKLE,
            EffectKind::LightWrap => &LIGHT_WRAP,
            EffectKind::TimeDisplacement => &TIME_DISPLACE,
            EffectKind::HoldFrame => &HOLD_FRAME,
            EffectKind::DirectionalBlur => &DIRECTIONALBLUR,
            EffectKind::RadialBlur => &RADIALBLUR,
            EffectKind::MotionBlur => &MOTIONBLUR,
            EffectKind::NightVision => &NIGHTVISION,
            EffectKind::ThermalCamera => &THERMALCAMERA,
            EffectKind::Infrared => &INFRARED,
            EffectKind::XRay => &XRAY,
            EffectKind::Spotlight => &SPOTLIGHT,
            EffectKind::Gleam => &GLEAM,
            EffectKind::Halation => &HALATION,
            EffectKind::CustomFlare => &CUSTOMFLARE,
            EffectKind::ForceField => &FORCEFIELD,
            EffectKind::Teleport => &TELEPORT,
            EffectKind::BlasterFire => &BLASTERFIRE,
            EffectKind::BloodSpatter => &BLOODSPATTER,
            EffectKind::BlockDisplacement => &BLOCKDISPLACEMENT,
            EffectKind::Derez => &DEREZ,
            EffectKind::EnergyDistortion => &ENERGYDISTORTION,
            EffectKind::FluidDistortion => &FLUIDDISTORTION,
            EffectKind::SmokeDistortion => &SMOKEDISTORTION,
            EffectKind::GradientRamp => &GRADIENTRAMP,
            EffectKind::Strobe => &STROBE,
            EffectKind::Scanlines => &SCANLINES,
            EffectKind::Lightning => &LIGHTNING,
            EffectKind::PatchReplacer => &PATCH,
            EffectKind::ObjectRemoval => &OBJECT_REMOVAL,
            EffectKind::Paint => &PAINT,
            EffectKind::FaceBeauty => &BEAUTY,
            EffectKind::SkyReplace => &SKY,
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
        matches!(self, EffectKind::EnergyBlade | EffectKind::CameraShake | EffectKind::FilmGrain | EffectKind::Glitch | EffectKind::LightLeaks | EffectKind::OpenFx | EffectKind::Shatter | EffectKind::MuzzleFlash | EffectKind::BulletHit | EffectKind::Fluid | EffectKind::Disintegrate | EffectKind::Lightning | EffectKind::Ocean | EffectKind::WaterSurface | EffectKind::Liquid)
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
            EffectKind::Fluid => &[(0, "Source"), (6, "Colour"), (9, "Motion"), (12, "Look"), (15, "Obstacles"), (17, "Body")],
            EffectKind::Ocean => &[(0, "Waves"), (9, "Camera"), (13, "Water"), (16, "Sky and sun"), (22, "Output"), (25, "Solid objects")],
            EffectKind::WaterSurface => &[(0, "Rain and drops"), (6, "Waves"), (10, "Water"), (15, "Solid objects"), (19, "Look"), (29, "Output")],
            EffectKind::Liquid => &[(0, "Source"), (8, "Water"), (16, "Solid objects"), (19, "Look"), (26, "Output"), (28, "Depth (3D)")],
            EffectKind::Liquid3D => &[(0, "Source"), (10, "Water"), (18, "Solid objects"), (22, "Look"), (28, "Camera and output")],
            EffectKind::Disintegrate => &[(0, "Sweep"), (4, "Edge"), (6, "Particles")],
            EffectKind::FractalNoise => &[(0, "Noise"), (6, "Motion"), (10, "Colour")],
            EffectKind::AudioSpectrum => &[(0, "Sound"), (3, "Where"), (7, "Shape"), (11, "Colour")],
            EffectKind::FilmLook => &[(0, "Stock"), (2, "Tone"), (6, "Light"), (9, "Texture")],
            EffectKind::FilmDamage => &[(0, "Scratches and dirt"), (5, "Projector"), (7, "Look")],
            EffectKind::AnalogueDamage => &[(0, "Colour"), (3, "Tape"), (8, "Picture")],
            EffectKind::Paint => &[(0, "Brush"), (5, "Strokes"), (6, "Clone")],
            EffectKind::FaceBeauty => &[(0, "Look"), (5, "Faces")],
            EffectKind::HslQualifier => &[(0, "Hue"), (3, "Saturation"), (6, "Luminance"), (9, "Matte"), (14, "Grade inside")],
            EffectKind::ColorWheels => &[(0, "Lift (shadows)"), (3, "Gamma (midtones)"), (6, "Gain (highlights)"), (9, "Offset (everything)"), (12, "Overall")],
            EffectKind::SkyReplace => &[(0, "Sky"), (3, "Placing"), (7, "Look"), (9, "Edge"), (11, "Blend")],
            EffectKind::Lightning => &[(0, "Path"), (4, "Shape"), (10, "Look"), (15, "Motion")],
            EffectKind::NeonPath => &[(0, "Path"), (5, "Flicker and taper"), (9, "Look"), (15, "Dashes"), (18, "Mix"), (21, "Switch on")],
            EffectKind::LensDirt => &[(0, "Light"), (3, "Dirt")],
            EffectKind::LightRays => &[(0, "Source"), (2, "Rays")],
            EffectKind::AnamorphicFlare => &[(0, "Streaks"), (7, "Light")],
            EffectKind::Clouds => &[(0, "Clouds"), (6, "Colours"), (10, "Draw")],
            EffectKind::BurnAway => &[(0, "Burn"), (4, "Edge"), (9, "Embers")],
            EffectKind::Extrusion3d => &[(0, "Shape"), (3, "Turn"), (8, "Surface"), (10, "Light")],
            EffectKind::Rotate3d => &[(0, "Turn"), (3, "View"), (5, "Pivot")],
            EffectKind::ChannelMixer => &[(0, "Red"), (4, "Green"), (8, "Blue"), (12, "Output")],
            EffectKind::ColorBalance => &[(0, "Shadows"), (3, "Midtones"), (6, "Highlights")],
            EffectKind::SelectiveColor => &[(0, "Method"), (1, "Range 1"), (6, "Range 2"), (11, "Range 3")],
            EffectKind::Kernel => &[(0, "Kernel"), (9, "Scale")],
            EffectKind::Deflicker => &[(0, "Gains"), (4, "Analysis")],
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
    /// A second table some effects need (sky replacement's new sky).
    pub table2: Option<Arc<Table>>,
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
            EffectKind::HslQualifier => v[qualify::SHOW] == 0.0 && v[qualify::HUE_SHIFT] == 0.0 && v[qualify::SATURATION] == 100.0 && v[qualify::BRIGHTNESS] == 0.0 && v[qualify::CONTRAST] == 100.0 && v[qualify::TINT_AMOUNT] == 0.0,
            EffectKind::HueCurves => v[0] == 0.0 || self.table.is_none(),
            EffectKind::ColorWheels => v[..12].iter().all(|&x| x == 0.0) && v[wheels::SATURATION] == 100.0 && v[wheels::CONTRAST] == 100.0 && v[wheels::HUE] == 0.0,
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
            EffectKind::LightRays => v[rays::BRIGHTNESS] == 0.0 || v[rays::LENGTH] == 0.0,
            EffectKind::AnamorphicFlare => v[streak::BRIGHTNESS] == 0.0 || v[streak::LENGTH] == 0.0,
            EffectKind::LensDirt => v[dirt::AMOUNT] == 0.0,
            EffectKind::NeonPath => v[neonpath::OPACITY] == 0.0 || self.table.is_none(),
            EffectKind::CameraShake => v[shake::AMOUNT] == 0.0 && v[shake::ROTATION] == 0.0 && v[shake::ZOOM] == 100.0,
            EffectKind::Bulge => v[distort::AMOUNT] == 0.0 || v[distort::RADIUS] <= 0.0,
            EffectKind::Twirl => v[distort::AMOUNT] == 0.0 || v[distort::RADIUS] <= 0.0,
            EffectKind::Ripple => v[ripple::HEIGHT] == 0.0,
            EffectKind::WaveWarp => v[wave::HEIGHT] == 0.0,
            EffectKind::PolarCoordinates => v[polar::AMOUNT] == 0.0,
            EffectKind::Mirror => false,
            EffectKind::LensDistortion => v[lens::DISTORTION] == 0.0 && v[lens::CUBIC] == 0.0 && v[lens::ZOOM] == 100.0,
            EffectKind::ChromaticAberration => v[aberration::AMOUNT] == 0.0,
            EffectKind::FilmGrain => v[grain::AMOUNT] == 0.0,
            EffectKind::Glitch => v[glitch::AMOUNT] == 0.0,
            EffectKind::LightLeaks => v[leaks::INTENSITY] == 0.0,
            EffectKind::Halftone => v[halftone::MIX] == 0.0,
            EffectKind::WireRemoval => false,
            EffectKind::Shatter => self.pieces.is_none(),
            EffectKind::Disintegrate => self.particles.is_none(),
            EffectKind::FractalNoise => v[fractal::OPACITY] == 0.0,
            EffectKind::AudioSpectrum => false,
            EffectKind::FilmLook => v[film::STRENGTH] == 0.0,
            EffectKind::FilmDamage => v[damage::SCRATCHES] == 0.0 && v[damage::DUST] == 0.0 && v[damage::HAIRS] == 0.0 && v[damage::FLICKER] == 0.0 && v[damage::JITTER] == 0.0 && v[damage::TINT] == 0.0 && v[damage::VIGNETTE] == 0.0,
            EffectKind::AnalogueDamage => v[analogue::AMOUNT] == 0.0,
            EffectKind::Abstraction => v[abstraction::SIZE] <= 0.0 && v[abstraction::EDGES] == 0.0 && v[abstraction::LEVELS] == 0.0,
            EffectKind::PrismBlur => v[prism::AMOUNT] == 0.0,
            EffectKind::TiltShift => v[tilt::BLUR] == 0.0 && v[tilt::SATURATION] == 100.0 && v[tilt::CONTRAST] == 100.0,
            EffectKind::TurbulentDisplace => v[turbulent::AMOUNT] == 0.0,
            EffectKind::HeatHaze => v[haze::STRENGTH] == 0.0,
            EffectKind::Shockwave => v[shockwave::STRENGTH] == 0.0 && v[shockwave::BRIGHTNESS] == 0.0,
            EffectKind::Starfield => v[stars::BRIGHTNESS] == 0.0 && v[stars::DRAW] == 0.0,
            EffectKind::GradientMap => v[gradient_map::MIX] == 0.0,
            EffectKind::Aurora => v[aurora::BRIGHTNESS] == 0.0 && v[aurora::DRAW] == 0.0,
            EffectKind::Nebula => v[nebula::BRIGHTNESS] == 0.0 && v[nebula::DRAW] == 0.0,
            EffectKind::Clouds => (v[clouds::COVERAGE] == 0.0 || v[clouds::BRIGHTNESS] == 0.0) && v[clouds::DRAW] == 0.0,
            EffectKind::BurnAway => v[burn::PROGRESS] <= 0.0 && v[burn::FROM] >= 1.0,
            EffectKind::Extrusion3d => false,
            EffectKind::Bend => v[bend::AMOUNT] == 0.0 && v[bend::SIZE] == 100.0,
            EffectKind::Bevel => v[bevel::WIDTH] <= 0.0 || v[bevel::INTENSITY] == 0.0,
            EffectKind::Extrude => v[extrude::LENGTH] <= 0.0,
            EffectKind::Rotate3d => v[rotate3d::TURN_X] == 0.0 && v[rotate3d::TURN_Y] == 0.0 && v[rotate3d::TURN_Z] == 0.0 && v[rotate3d::SIZE] == 100.0 && v[rotate3d::PUSH] == 0.0,
            EffectKind::BoxBlur => v[box_blur::RADIUS] <= 0.0,
            EffectKind::ChannelBlur => v[channel_blur::RED] <= 0.0 && v[channel_blur::GREEN] <= 0.0 && v[channel_blur::BLUE] <= 0.0 && v[channel_blur::ALPHA] <= 0.0,
            EffectKind::CompoundBlur => v[compound_blur::MAX] <= 0.0,
            EffectKind::SmartBlur => v[smart_blur::RADIUS] <= 0.0 && v[smart_blur::MODE] < 0.5,
            EffectKind::CameraLensBlur => v[lens_blur::RADIUS] <= 0.0,
            EffectKind::CrossBlur => v[cross_blur::ACROSS] <= 0.0 && v[cross_blur::DOWN] <= 0.0,
            EffectKind::VectorBlur => v[vector_blur::AMOUNT] <= 0.0,
            EffectKind::Arithmetic => false,
            EffectKind::BlendColour => v[blend_colour::AMOUNT] >= 100.0,
            EffectKind::Calculations => false,
            EffectKind::ChannelMixer => v[channel_mixer::RR] == 100.0 && v[channel_mixer::GG] == 100.0 && v[channel_mixer::BB] == 100.0 && [1, 2, 3, 4, 6, 7, 8, 9, 11].iter().all(|&i| v[i] == 0.0) && v[channel_mixer::MONO] < 0.5,
            EffectKind::Invert => v[invert::BLEND] >= 100.0,
            EffectKind::ShiftChannels => v[shift_channels::RED] == 0.0 && v[shift_channels::GREEN] == 1.0 && v[shift_channels::BLUE] == 2.0 && v[shift_channels::ALPHA] == 3.0,
            EffectKind::SetChannels => v[set_channels::RED] == 0.0 && v[set_channels::GREEN] == 1.0 && v[set_channels::BLUE] == 2.0 && v[set_channels::ALPHA] == 3.0 && v[4..8].iter().all(|x| *x < 0.5),
            EffectKind::SetMatte => v[set_matte::FROM] == 3.0 && v[set_matte::INVERT] < 0.5,
            EffectKind::SolidComposite => v[solid_composite::OPACITY] <= 0.0 && v[solid_composite::SOURCE] >= 100.0,
            EffectKind::Minimax => v[minimax::RADIUS] <= 0.0,
            EffectKind::AutoLevels => v[auto_levels::BLEND] >= 100.0,
            EffectKind::AutoContrast => v[auto_contrast::BLEND] >= 100.0,
            EffectKind::AutoColor => v[auto_color::BLEND] >= 100.0,
            EffectKind::Equalize => v[equalize::AMOUNT] <= 0.0,
            EffectKind::ChangeColor => v[change_color::VIEW] < 0.5 && v[change_color::HUE] == 0.0 && v[change_color::LIGHTNESS] == 0.0 && v[change_color::SATURATION] == 0.0,
            EffectKind::ChangeToColor => v[change_to_color::FROM] == v[change_to_color::TO] && v[change_to_color::VIEW] < 0.5,
            EffectKind::ColorBalance => v[0..9].iter().all(|x| *x == 0.0),
            EffectKind::Exposure => v[exposure::EXPOSURE] == 0.0 && v[exposure::OFFSET] == 0.0 && v[exposure::GAMMA] == 1.0,
            EffectKind::LeaveColor => v[leave_color::AMOUNT] <= 0.0,
            EffectKind::PhotoFilter => v[photo_filter::DENSITY] <= 0.0,
            EffectKind::SelectiveColor => [2, 3, 4, 5, 7, 8, 9, 10, 12, 13, 14, 15].iter().all(|&i| v[i] == 0.0),
            EffectKind::Tint => v[tint::AMOUNT] <= 0.0,
            EffectKind::BlackWhite => false,
            EffectKind::ColorNeutralizer => v[color_neutralizer::BLEND] >= 100.0,
            EffectKind::ColorOffset => v[color_offset::RED] == 0.0 && v[color_offset::GREEN] == 0.0 && v[color_offset::BLUE] == 0.0,
            EffectKind::Toner => v[toner::BLEND] >= 100.0,
            EffectKind::ThresholdRgb => v[threshold_rgb::BLEND] >= 100.0,
            EffectKind::Kernel => v[kernel::BLEND] >= 100.0 || (v[kernel::K22] == v[kernel::DIVIDE] && v[kernel::OFFSET] == 0.0 && [0, 1, 2, 3, 5, 6, 7, 8].iter().all(|&i| v[i] == 0.0)),
            EffectKind::VideoLimiter => false,
            EffectKind::Deflicker => v[deflicker::BRIGHTNESS] == 100.0 && v[deflicker::RED] == 100.0 && v[deflicker::GREEN] == 100.0 && v[deflicker::BLUE] == 100.0,
            EffectKind::Hologram => false,
            EffectKind::Portal => v[portal::OPEN] == 0.0,
            EffectKind::Hud => v[hud::OPACITY] == 0.0 || v[hud::DRAW_ON] == 0.0,
            EffectKind::LightSweep => v[sweep::BRIGHTNESS] == 0.0,
            EffectKind::ElectricArcs => v[arcs::AMOUNT] == 0.0 || v[arcs::BRIGHTNESS] == 0.0,
            EffectKind::DepthMap => self.table.is_none(),
            EffectKind::VolumetricLight => v[volumetric::INTENSITY] == 0.0,
            EffectKind::LightWrap => v[light_wrap::AMOUNT] == 0.0 || v[light_wrap::WIDTH] < 0.25,
            EffectKind::TimeDisplacement => v[time_displace::DELAY] == 0.0,
            EffectKind::HoldFrame => false,
            EffectKind::SparkleTrail => self.table.is_none() || v[sparkle::BRIGHTNESS] == 0.0,
            EffectKind::PortraitBlur => self.table.is_none() || (v[portrait::BLUR] < 0.5 && v[portrait::SHOW] == 0.0),
            EffectKind::Relight => self.table.is_none() || (v[relight::INTENSITY] == 0.0 && v[relight::DARKEN] == 0.0),
            EffectKind::Liquify => self.table.is_none() || (v[liquify_params::AMOUNT] == 0.0 && v[liquify_params::MESH] == 0.0),
            EffectKind::Echo => v[echo::COUNT] < 0.5 || v[echo::FIRST] == 0.0 || v[echo::DELAY] == 0.0,
            EffectKind::DirectionalBlur => v[dir_blur::LENGTH] < 0.5,
            EffectKind::RadialBlur => v[radial_blur::AMOUNT] == 0.0,
            EffectKind::MotionBlur => v[motion_blur::SHUTTER] == 0.0,
            EffectKind::NightVision => false,
            EffectKind::ThermalCamera => false,
            EffectKind::Infrared => v[infrared::MIX] == 0.0,
            EffectKind::XRay => false,
            EffectKind::Spotlight => false,
            EffectKind::Gleam => v[gleam::BRIGHTNESS] == 0.0 || v[gleam::LENGTH] < 0.5,
            EffectKind::Halation => v[halation::AMOUNT] == 0.0 || v[halation::SIZE] < 0.25,
            EffectKind::CustomFlare => v[custom_flare::BRIGHTNESS] == 0.0,
            EffectKind::ForceField => v[force_field::POWER] == 0.0,
            EffectKind::Teleport => false,
            EffectKind::BlasterFire => false,
            EffectKind::BloodSpatter => v[blood::AMOUNT] == 0.0,
            EffectKind::BlockDisplacement => v[block_displace::AMOUNT] == 0.0 || v[block_displace::DENSITY] == 0.0,
            EffectKind::Derez => v[derez::PROGRESS] == 0.0,
            EffectKind::EnergyDistortion => v[energy_distort::AMOUNT] == 0.0 && v[energy_distort::GLOW] == 0.0,
            EffectKind::FluidDistortion => v[fluid_distort::AMOUNT] == 0.0,
            EffectKind::SmokeDistortion => v[smoke_distort::AMOUNT] == 0.0 && v[smoke_distort::TINT] == 0.0,
            EffectKind::GradientRamp => v[ramp::OPACITY] == 0.0,
            EffectKind::Strobe => v[strobe::STRENGTH] == 0.0,
            EffectKind::Scanlines => v[scan::AMOUNT] == 0.0,

            EffectKind::Lightning => v[lightning::OPACITY] == 0.0 || self.table.is_none(),
            EffectKind::PatchReplacer => v[patch::OPACITY] == 0.0,
            EffectKind::ObjectRemoval => v[removal::OPACITY] == 0.0 || self.table.is_none(),
            EffectKind::Paint => self.table.is_none(),
            EffectKind::FaceBeauty => {
                let faces = v[beauty::AREA] == 0.0;
                (faces && self.gpu[0] == 0.0) || (v[beauty::MODE] == 0.0 && v[beauty::SMOOTH] == 0.0 && v[beauty::BRIGHTEN] == 0.0 && v[beauty::EVEN] == 0.0 && v[beauty::SHOW] == 0.0)
            }
            EffectKind::SkyReplace => self.table.is_none() || v[sky::OPACITY] == 0.0,
            EffectKind::MuzzleFlash => v[muzzle::BRIGHTNESS] == 0.0 && v[muzzle::LIGHT] == 0.0,
            EffectKind::BulletHit => false,
            EffectKind::Fluid => v[fluid_params::OPACITY] == 0.0 || self.fluid.is_none(),
            EffectKind::Ocean => v[ocean::OPACITY] == 0.0,
            EffectKind::WaterSurface => v[water::OPACITY] == 0.0 || self.fluid.is_none(),
            EffectKind::Liquid => v[liquid::OPACITY] == 0.0 || self.fluid.is_none(),
            EffectKind::Liquid3D => v[liquid3d::OPACITY] == 0.0 || self.fluid.is_none(),
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
            || self.kind == EffectKind::AudioSpectrum
            || (self.kind == EffectKind::FilmLook && (v[film::GRAIN] > 0.0 || v[film::WEAVE] > 0.0))
            || self.kind == EffectKind::FilmDamage
            || self.kind == EffectKind::AnalogueDamage
            || (self.kind == EffectKind::Scanlines && v[scan::ROLL] != 0.0)
            || (self.kind == EffectKind::FractalNoise && (v[fractal::SPEED] != 0.0 || v[fractal::DRIFT_X] != 0.0 || v[fractal::DRIFT_Y] != 0.0))
            || (self.kind == EffectKind::Crt && (v[crt::WOBBLE] > 0.0 || v[crt::NOISE] > 0.0 || v[crt::TRACKING] > 0.0))
            || (self.kind == EffectKind::TurbulentDisplace && (v[turbulent::SPEED] != 0.0))
            || (self.kind == EffectKind::HeatHaze && (v[haze::RISE] != 0.0 || v[haze::SHIMMER] != 0.0))
            || (self.kind == EffectKind::Shockwave && (true))
            || (self.kind == EffectKind::Starfield && (v[stars::SPEED] != 0.0 || v[stars::TWINKLE] > 0.0))
            || (self.kind == EffectKind::Aurora && v[aurora::SPEED] != 0.0)
            || (self.kind == EffectKind::Nebula && (v[nebula::SWIRL] != 0.0 || v[nebula::STARS] > 0.0))
            || (self.kind == EffectKind::Clouds && v[clouds::SPEED] != 0.0)
            || (self.kind == EffectKind::BurnAway && v[burn::EMBERS] > 0.0 && v[burn::PROGRESS] > 0.0)
            || (self.kind == EffectKind::Hologram && (v[hologram::LINE_SPEED] != 0.0 || v[hologram::FLICKER] > 0.0 || v[hologram::GLITCH] > 0.0))
            || (self.kind == EffectKind::Portal && (v[portal::OPEN] > 0.0))
            || (self.kind == EffectKind::Hud && (true))
            || (self.kind == EffectKind::LightSweep && (v[sweep::POSITION] < 0.0))
            || (self.kind == EffectKind::ElectricArcs && (true))
            || (self.kind == EffectKind::NightVision && (v[night_vision::NOISE] > 0.0 || v[night_vision::FLICKER] > 0.0))
            || (self.kind == EffectKind::ThermalCamera && (v[thermal::NOISE] > 0.0))
            || (self.kind == EffectKind::Infrared && (v[infrared::GRAIN] > 0.0))
            || (self.kind == EffectKind::XRay && (v[xray::NOISE] > 0.0))
            || (self.kind == EffectKind::Gleam && (v[gleam::TWINKLE] > 0.0))
            || (self.kind == EffectKind::ForceField && (v[force_field::SHIMMER] > 0.0 || v[force_field::HIT_AT] >= 0.0))
            || (self.kind == EffectKind::Teleport && (true))
            || self.kind == EffectKind::SparkleTrail
            || (self.kind == EffectKind::BlasterFire && (true))
            || (self.kind == EffectKind::BloodSpatter && (true))
            || (self.kind == EffectKind::BlockDisplacement && (v[block_displace::SPEED] > 0.0))
            || (self.kind == EffectKind::EnergyDistortion && (v[energy_distort::SPEED] > 0.0))
            || (self.kind == EffectKind::FluidDistortion && (v[fluid_distort::SPEED] > 0.0 || v[fluid_distort::FLOW] > 0.0))
            || (self.kind == EffectKind::SmokeDistortion && (v[smoke_distort::RISE] != 0.0 || v[smoke_distort::TURBULENCE] > 0.0))
            || (self.kind == EffectKind::Strobe && (true))
            || (self.kind == EffectKind::Ripple && v[ripple::SPEED] != 0.0)
            || (self.kind == EffectKind::WaveWarp && v[wave::SPEED] != 0.0)
            || (self.kind == EffectKind::NeonPath && (v[neonpath::FLICKER] > 0.0 || v[neonpath::DASH_SPEED] != 0.0 || v[neonpath::SWITCH_ON] > 0.0))
    }
}

impl Effect {
    /// Multiply its settings measured in pixels (blur radius, glow size,
    /// outline width, shake amount, dash speed…) and their keyframes by `k`,
    /// kept within each setting's range; for a layer drawn `k` times as big.
    /// A camera's, light's or point's positions, and plugins' settings, are
    /// left alone. Whether anything changed.
    pub fn scale_pixels(&mut self, k: f32) -> bool {
        if !k.is_finite() || k <= 0.0 || (k - 1.0).abs() < 1e-6 || matches!(self.kind, EffectKind::Camera | EffectKind::Light | EffectKind::Point | EffectKind::Plugin | EffectKind::OpenFx) {
            return false;
        }
        let mut changed = false;
        for (p, spec) in self.params.iter_mut().zip(self.kind.params()) {
            if !matches!(spec.ui, ParamUi::Number) || !spec.suffix.trim_start().starts_with("px") {
                continue;
            }
            let fit = |v: f32| (v * k).clamp(spec.min.min(v), spec.max.max(v));
            let v = fit(p.value);
            changed |= v != p.value;
            p.value = v;
            for key in &mut p.keys.0 {
                let v = fit(key.value);
                changed |= v != key.value;
                key.value = v;
            }
        }
        changed
    }

    pub fn new(kind: EffectKind) -> Self {
        let data = match kind {
            EffectKind::Curves => Some(EffectData::Curves(Curves::default())),
            EffectKind::HueCurves => Some(EffectData::HueCurves(crate::lut::HueCurves::default())),
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
        if self.kind == EffectKind::MotionBlur {
            // Until the scene says how the layer moves: standing still.
            for m in [motion_blur::FROM, motion_blur::FROM + 6] {
                if let Some(v) = values.get_mut(m..m + 6) {
                    v.copy_from_slice(&[1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
                }
            }
        }
        let plugin = if self.kind == EffectKind::Plugin { self.plugin_def() } else { None };
        let particles = (self.kind == EffectKind::Particles).then(|| Arc::new(crate::particles::simulate(self, t)));
        let (mut table, mut gpu, mut table2) = (self.table(), [0.0; 4], None);
        match (&self.data, self.kind) {
            (Some(EffectData::Depth(dir)), EffectKind::DepthMap | EffectKind::Relight | EffectKind::PortraitBlur | EffectKind::VolumetricLight) => {
                if let Some((tb, _)) = crate::fillstore::load(dir, t) {
                    table = Some(tb);
                }
            }
            (Some(EffectData::Fill(dir)), EffectKind::ObjectRemoval) => {
                if let Some((tb, place)) = crate::fillstore::load(dir, t) {
                    (table, gpu) = (Some(tb), place);
                }
            }
            (data, EffectKind::FaceBeauty) => {
                let tracks = match data {
                    Some(EffectData::Faces(f)) => &f[..],
                    _ => &[],
                };
                let (tb, n) = crate::faces::table(tracks, t);
                (table, gpu) = (Some(tb), [n as f32, 0.0, 0.0, 0.0]);
            }
            (Some(EffectData::Sky { image, matte: Some(dir) }), EffectKind::SkyReplace) => {
                if let Some((tb, _, shift)) = crate::fillstore::load_moved(dir, t) {
                    table = Some(tb);
                    gpu = [shift[0], shift[1], shift[2], 0.0];
                    // The new sky, and its average colour (for matching the light).
                    table2 = image.as_ref().filter(|_| values[sky::SOURCE] == 0.0).and_then(|p| crate::lut::load_plate(p, Time::ZERO));
                    gpu[3] = table2.as_ref().map_or(0.0, |t| crate::lut::average_rgb(t) as f32);
                }
            }
            _ => {}
        }
        if self.kind == EffectKind::VolumetricLight {
            // Depth when it's been worked out (gpu.x = 1); otherwise a blank table and the
            // picture's brightness decides what blocks the light.
            gpu[0] = if table.is_some() { 1.0 } else { 0.0 };
            if table.is_none() {
                table = Some(std::sync::Arc::new(crate::lut::Table { id: u64::MAX - 7, width: 1, height: 1, rgba: vec![0, 0, 0, 255], cube_size: 0 }));
            }
        }
        EffectValues { kind: self.kind, values, table, plugin, time: t.as_secs_f64() as f32, prev, particles, pieces: None, fluid: None, gpu, table2 }
    }

    /// [`Self::eval_moving`] for a layer `size` pixels across: what needs
    /// the layer's shape (a shatter's pieces, a fluid's grid) too.
    pub fn eval_sized(&self, t: Time, prev: Option<Time>, size: [f32; 2]) -> EffectValues {
        let mut v = self.eval_moving(t, prev);
        match self.kind {
            EffectKind::Shatter => v.pieces = Some(Arc::new(crate::shatter::simulate(self, t, size))),
            EffectKind::Fluid => v.fluid = crate::fluid::job(self, t, size).map(Arc::new),
            EffectKind::Ocean | EffectKind::WaterSurface | EffectKind::Liquid | EffectKind::Liquid3D => v.fluid = crate::water::job(self, t, size).map(Arc::new),
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
            EffectKind::Liquify => {
                let strokes = match &self.data {
                    Some(EffectData::Liquify(s)) => s.as_slice(),
                    _ => &[],
                };
                match crate::liquify::table(strokes, size) {
                    Some((tb, cell)) => {
                        v.gpu = [cell, tb.width as f32, tb.height as f32, 0.0];
                        v.table = Some(tb);
                    }
                    None => v.table = None,
                }
            }
            EffectKind::Hud => {
                if let Some(EffectData::Text(s)) = &self.data {
                    crate::hud_text::fill(&mut v, s, size);
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
            (Some(EffectData::HueCurves(c)), EffectKind::HueCurves) if !c.is_identity() => Some(Arc::new(c.table())),
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
                EffectKind::ColorCorrect | EffectKind::Levels | EffectKind::Curves | EffectKind::ColorWheels | EffectKind::HslQualifier | EffectKind::HueCurves | EffectKind::Lut | EffectKind::DifferenceKey | EffectKind::ObjectRemoval | EffectKind::Paint | EffectKind::Liquify | EffectKind::DepthMap | EffectKind::Relight | EffectKind::PortraitBlur | EffectKind::FaceBeauty | EffectKind::SkyReplace | EffectKind::Deflicker | EffectKind::ChannelMixer | EffectKind::ShiftChannels | EffectKind::SetChannels | EffectKind::ColorBalance | EffectKind::ChangeColor | EffectKind::ColorOffset | EffectKind::Kernel | EffectKind::Exposure | EffectKind::SelectiveColor => assert!(v.is_identity(), "{kind:?}"),
                // Needs a mask to run along (see neonpath's tests).
                EffectKind::NeonPath => assert!(v.is_identity()),
                // Its trail is worked out with the clip (see sparkle's tests).
                EffectKind::SparkleTrail => assert!(v.is_identity()),
                // Nothing has been born yet at the start, but soon is.
                EffectKind::Particles => assert!(v.is_identity() && !e.eval(Time::from_secs_f64(0.5)).is_identity()),
                // These need the layer's size (a shatter's pieces, a fluid's grid).
                EffectKind::Shatter | EffectKind::Fluid | EffectKind::Disintegrate | EffectKind::Lightning | EffectKind::WaterSurface | EffectKind::Liquid | EffectKind::Liquid3D => assert!(v.is_identity() && !e.eval_sized(Time::from_secs_f64(0.5), None, [640.0, 360.0]).is_identity(), "{kind:?}"),
                _ => assert!(!v.is_identity(), "{kind:?} should do something by default"),
            }
        }
    }

    #[test]
    fn colour_wheels_grade_as_a_colourist_expects() {
        let base: Vec<f32> = EffectKind::ColorWheels.params().iter().map(|p| p.default).collect();
        let with = |pairs: &[(usize, f32)]| {
            let mut v = base.clone();
            for &(i, x) in pairs {
                v[i] = x;
            }
            v
        };
        let near = |a: [f32; 3], b: [f32; 3]| a.iter().zip(&b).all(|(x, y)| (x - y).abs() < 1e-4);
        // At rest nothing changes.
        for c in [[0.0, 0.0, 0.0], [0.2, 0.5, 0.9], [1.0, 1.0, 1.0]] {
            assert!(near(wheels::apply(&base, c), c), "{c:?}");
        }
        // The pucks push towards the colour they point at, with no change of
        // brightness: right is blue, up is red (as on a vectorscope).
        let t = wheels::tint(1.0, 0.0);
        assert!(t[2] > 0.5 && t[0].abs() < 1e-6 && (0.2126 * t[0] + 0.7152 * t[1] + 0.0722 * t[2]).abs() < 1e-5);
        let t = wheels::tint(0.0, 1.0);
        assert!(t[0] > 0.5 && (0.2126 * t[0] + 0.7152 * t[1] + 0.0722 * t[2]).abs() < 1e-5);
        // Lift moves black and leaves white; gain moves white and leaves black.
        let lift = with(&[(wheels::LIFT + 2, 20.0)]);
        assert!(near(wheels::apply(&lift, [0.0; 3]), [0.1; 3]));
        assert!(near(wheels::apply(&lift, [1.0; 3]), [1.0; 3]));
        let gain = with(&[(wheels::GAIN + 2, 100.0)]);
        assert!(near(wheels::apply(&gain, [0.0; 3]), [0.0; 3]));
        assert!(near(wheels::apply(&gain, [0.4; 3]), [0.8; 3]));
        // Gamma lifts the middle and leaves both ends.
        let gamma = with(&[(wheels::GAMMA + 2, 50.0)]);
        let mid = wheels::apply(&gamma, [0.5; 3]);
        assert!(mid[0] > 0.55 && near(wheels::apply(&gamma, [1.0; 3]), [1.0; 3]) && near(wheels::apply(&gamma, [0.0; 3]), [0.0; 3]));
        // Gain towards orange warms the highlights more than the shadows.
        let warm = with(&[(wheels::GAIN, -40.0), (wheels::GAIN + 1, 40.0)]);
        let hi = wheels::apply(&warm, [0.8; 3]);
        let lo = wheels::apply(&warm, [0.1; 3]);
        assert!(hi[0] > hi[2] && hi[0] - hi[2] > lo[0] - lo[2]);
        // Offset moves everything the same.
        let off = with(&[(wheels::OFFSET + 2, -20.0)]);
        assert!(near(wheels::apply(&off, [0.5; 3]), [0.4; 3]));
        // Saturation 0 is grey of the same luma; contrast pivots.
        let grey = wheels::apply(&with(&[(wheels::SATURATION, 0.0)]), [0.9, 0.3, 0.1]);
        assert!((grey[0] - grey[1]).abs() < 1e-5 && (grey[0] - (0.2126 * 0.9 + 0.7152 * 0.3 + 0.0722 * 0.1)).abs() < 1e-5);
        let con = with(&[(wheels::CONTRAST, 200.0), (wheels::PIVOT, 50.0)]);
        assert!(near(wheels::apply(&con, [0.5; 3]), [0.5; 3]) && near(wheels::apply(&con, [0.6; 3]), [0.7; 3]));
        // Hue turns a colour and keeps its luma; 360° is back where it was.
        let c = [0.6, 0.4, 0.3];
        let h = wheels::apply(&with(&[(wheels::HUE, 120.0)]), c);
        let y = |c: [f32; 3]| 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
        assert!((y(h) - y(c)).abs() < 1e-4 && h[0] < c[0]);
        assert!(near(wheels::apply(&with(&[(wheels::HUE, 180.0)]), wheels::apply(&with(&[(wheels::HUE, 180.0)]), c)), c));
        let e = Effect::new(EffectKind::ColorWheels);
        assert!(e.eval(Time::ZERO).is_identity());
    }

    #[test]
    fn the_qualifier_chooses_a_colour_range_and_grades_only_that() {
        let base: Vec<f32> = EffectKind::HslQualifier.params().iter().map(|p| p.default).collect();
        let with = |set: &[(usize, f32)]| {
            let mut v = base.clone();
            for &(i, x) in set {
                v[i] = x;
            }
            v
        };
        // Aimed at red (hue 0, 60° wide): red is chosen, green and grey aren't.
        let red_only = with(&[(qualify::HUE, 0.0)]);
        let (red, green, grey) = ([0.8, 0.15, 0.1], [0.2, 0.7, 0.2], [0.5, 0.5, 0.5]);
        assert!(qualify::matte(&red_only, red) > 0.95);
        assert!(qualify::matte(&red_only, green) < 0.01 && qualify::matte(&red_only, grey) < 0.01);
        // It wraps round the wheel: hue 350 is near red too.
        assert!(qualify::matte(&with(&[(qualify::HUE, 350.0)]), red) > 0.9);
        // Softness blends at the edge; Invert flips it.
        let edge = qualify::matte(&with(&[(qualify::HUE, 0.0), (qualify::HUE_WIDTH, 20.0), (qualify::HUE_SOFT, 30.0)]), [0.8, 0.4, 0.1]);
        assert!(edge > 0.05 && edge < 0.95, "{edge}");
        assert!(qualify::matte(&with(&[(qualify::HUE, 0.0), (qualify::INVERT, 1.0)]), green) > 0.95);
        // Luminance only: the dark reds left out.
        let bright_reds = with(&[(qualify::HUE, 0.0), (qualify::LUM_LOW, 25.0), (qualify::LUM_SOFT, 2.0)]);
        assert!(qualify::matte(&bright_reds, [0.25, 0.03, 0.02]) < 0.05);
        // The grade: red turned towards orange-yellow, greens untouched.
        let turned = with(&[(qualify::HUE, 0.0), (qualify::HUE_SHIFT, 40.0)]);
        let r2 = qualify::apply(&turned, red);
        assert!(r2[1] > red[1] + 0.1, "{r2:?}");
        assert_eq!(qualify::apply(&turned, green), green);
        // Desaturate the chosen ones to grey of the same luminance.
        let grey_reds = qualify::apply(&with(&[(qualify::HUE, 0.0), (qualify::SATURATION, 0.0)]), red);
        assert!((grey_reds[0] - grey_reds[2]).abs() < 1e-4);
        // Show the matte.
        assert_eq!(qualify::apply(&with(&[(qualify::HUE, 0.0), (qualify::SHOW, 1.0)]), green), [0.0; 3]);
        // At rest (no grade, showing the picture) it's nothing.
        assert!(Effect::new(EffectKind::HslQualifier).eval(Time::ZERO).is_identity());
        // The eyedropper centres it on the picked colour.
        let picked = qualify::pick([0.2, 0.4, 0.85]);
        assert!((picked[0].1 - 221.0).abs() < 2.0, "{picked:?}");
        let mut v = base.clone();
        for (i, x) in picked {
            v[i] = x;
        }
        assert!(qualify::matte(&v, [0.2, 0.4, 0.85]) > 0.95);
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
