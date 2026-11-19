//! Saved effect presets: a stack of effects with their settings and
//! keyframes (and, if chosen, the clip's masks, its movement and opacity,
//! and its blend mode), kept under a name in the app's settings folder and
//! applied to other clips from the effects browser.
//!
//! Keyframes are stored relative to the start of the clip they came from
//! (its source in point). How they land on another clip is the preset's
//! [`Timing`]: the same time from its start (a fade-in over the first
//! second lands on the next clip's first second), the same time before its
//! end (a fade-out), or stretched to its length.

use reelforge_core::anim::{Curve, Prop, TransformAnim};
use reelforge_core::effects::{Effect, EffectKind};
use reelforge_core::mask::Mask;
use reelforge_core::{BlendMode, ClipAudio, ClipVisual, Time, TitleSpec, Transform, Transition};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Where a preset's keyframes land on the clip it's applied to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Timing {
    /// The same time after the clip's start.
    #[default]
    FromStart,
    /// The same time before the clip's end.
    FromEnd,
    /// Spread over the clip's length as over the original's.
    Stretch,
}

/// A clip's movement: its transform (position, scale, rotation, anchor,
/// opacity, and 3D depth and turn) and their keyframes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Motion {
    pub transform: Transform,
    pub anim: TransformAnim,
    /// Which properties it sets ("position", "scale", "rotation", "anchor",
    /// "opacity", "depth", "orientation"); all of them when empty. A fade
    /// sets only the opacity and leaves the clip where it is.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub props: Vec<String>,
}

impl Motion {
    fn sets(&self, p: Prop) -> bool {
        self.props.is_empty() || self.props.iter().any(|n| n == prop_name(p))
    }
}

fn prop_name(p: Prop) -> &'static str {
    match p {
        Prop::Position => "position",
        Prop::Scale => "scale",
        Prop::Rotation => "rotation",
        Prop::Anchor => "anchor",
        Prop::Opacity => "opacity",
        Prop::Depth => "depth",
        Prop::Orientation => "orientation",
        Prop::Crop => "crop",
        Prop::Pin => "pin",
    }
}

/// A transition into the clip: its kind and length.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct TransitionPreset {
    pub kind: Transition,
    pub length: Time,
}

/// What goes into a preset besides the chosen effects.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SaveOptions {
    /// Which of the clip's effects (by position); `None` for all of them.
    pub effects: Option<Vec<usize>>,
    pub masks: bool,
    pub motion: bool,
    pub blend: bool,
    pub timing: Timing,
    /// A title's style (font, size, colours, outline, shadow, background; not its words).
    pub title: bool,
    /// The clip's audio: volume, pan and fades, with their keyframes.
    pub audio: bool,
    /// The transition into the clip.
    pub transition: bool,
    /// The folder it's filed under ("" for none).
    pub folder: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EffectPreset {
    pub name: String,
    /// Keyframe times are from the clip's start.
    pub effects: Vec<Effect>,
    /// Masks, in the pixels of a layer of `layer` size (scaled to the clip they go on).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub masks: Vec<Mask>,
    #[serde(default)]
    pub layer: [f32; 2],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub motion: Option<Motion>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blend: Option<BlendMode>,
    #[serde(default)]
    pub timing: Timing,
    /// How long the clip it came from was (for [`Timing::FromEnd`] and [`Timing::Stretch`]).
    #[serde(default)]
    pub length: Time,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<TitleSpec>,
    /// Volume and pan keys are from the clip's start, like the rest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<ClipAudio>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transition: Option<TransitionPreset>,
    /// Filed under ("" for none).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub folder: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub favourite: bool,
    /// A small picture of the Viewer when it was saved (a file name in the
    /// presets' pictures folder).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumb: Option<String>,
    /// Comes with Reelforge (can't be renamed, deleted or replaced).
    #[serde(skip)]
    pub builtin: bool,
}

/// What else a clip has that a preset can take (see [`EffectPreset::from_clip_parts`]).
#[derive(Clone, Debug, Default)]
pub struct ClipParts {
    pub title: Option<TitleSpec>,
    pub audio: Option<ClipAudio>,
    pub transition: Option<TransitionPreset>,
}

fn retime_audio(a: &ClipAudio, f: &impl Fn(Time) -> Time) -> ClipAudio {
    let mut a = a.clone();
    for k in &mut a.volume_keys {
        k.at = f(k.at);
    }
    for k in &mut a.pan_keys {
        k.at = f(k.at);
    }
    // What ducking added belongs to that clip's dialogue, not a preset.
    a.ducking.clear();
    a.crossfade_in = Time::ZERO;
    a
}

/// The transform properties a preset's motion carries.
const MOTION_PROPS: [Prop; 7] = [Prop::Position, Prop::Scale, Prop::Rotation, Prop::Anchor, Prop::Opacity, Prop::Depth, Prop::Orientation];

fn retimed<T: Clone>(c: &Curve<T>, f: &impl Fn(Time) -> Time) -> Curve<T> {
    let mut c = c.clone();
    for k in &mut c.0 {
        k.at = f(k.at);
    }
    c
}

fn retime_effects(effects: &[Effect], f: &impl Fn(Time) -> Time) -> Vec<Effect> {
    effects
        .iter()
        .map(|e| {
            let mut e = e.clone();
            for p in &mut e.params {
                p.keys = retimed(&p.keys, f);
            }
            e
        })
        .collect()
}

fn retime_masks(masks: &[Mask], f: &impl Fn(Time) -> Time) -> Vec<Mask> {
    masks
        .iter()
        .map(|m| {
            let mut m = m.clone();
            for k in &mut m.keys {
                k.at = f(k.at);
            }
            m
        })
        .collect()
}

/// The motion properties' keyframes, retimed (crop and corner pin are left out).
fn retime_motion(a: &TransformAnim, f: &impl Fn(Time) -> Time) -> TransformAnim {
    TransformAnim {
        position: retimed(&a.position, f),
        scale: retimed(&a.scale, f),
        rotation: retimed(&a.rotation, f),
        opacity: retimed(&a.opacity, f),
        anchor: retimed(&a.anchor, f),
        depth: retimed(&a.depth, f),
        orientation: retimed(&a.orientation, f),
        ..TransformAnim::default()
    }
}

/// A camera's, light's or point's settings belong to that clip, not a preset.
fn savable(e: &Effect) -> bool {
    !matches!(e.kind, EffectKind::Camera | EffectKind::Light | EffectKind::Point)
}

/// Mask points scaled from one layer size to another.
fn scale_masks(masks: &mut [Mask], from: [f32; 2], to: [f32; 2]) {
    if from[0] <= 0.0 || from[1] <= 0.0 || from == to {
        return;
    }
    let (kx, ky) = (to[0] / from[0], to[1] / from[1]);
    let sc = |pts: &mut Vec<[f32; 2]>| pts.iter_mut().for_each(|q| *q = [q[0] * kx, q[1] * ky]);
    let sh = |cs: &mut Vec<[f32; 4]>| cs.iter_mut().for_each(|c| *c = [c[0] * kx, c[1] * ky, c[2] * kx, c[3] * ky]);
    for m in masks {
        sc(&mut m.points);
        sh(&mut m.curves);
        for k in &mut m.keys {
            sc(&mut k.points);
            sh(&mut k.curves);
        }
    }
}

impl EffectPreset {
    /// The effects of a clip whose media starts at `source_in` (all of them,
    /// nothing else, timed from its start).
    pub fn from_clip(name: &str, effects: &[Effect], source_in: Time) -> Option<Self> {
        let v = ClipVisual { transform: Transform::default(), blend: BlendMode::Normal, track_matte: None, anim: TransformAnim::default(), effects: effects.to_vec(), masks: Vec::new(), mesh: None };
        Self::from_visual(name, &v, source_in, Time::ZERO, [0.0, 0.0], &SaveOptions::default())
    }

    /// As [`Self::from_visual`], plus the title style, audio and transition
    /// `opts` chooses from `parts`.
    pub fn from_clip_parts(name: &str, v: &ClipVisual, parts: &ClipParts, source_in: Time, length: Time, layer: [f32; 2], opts: &SaveOptions) -> Option<Self> {
        let rel = |t: Time| t - source_in;
        let title = parts.title.clone().filter(|_| opts.title).map(|t| TitleSpec { text: String::new(), ..t });
        let audio = parts.audio.as_ref().filter(|_| opts.audio).map(|a| retime_audio(a, &rel));
        let transition = parts.transition.filter(|_| opts.transition);
        let mut p = match Self::from_visual(name, v, source_in, length, layer, opts) {
            Some(p) => p,
            None if title.is_some() || audio.is_some() || transition.is_some() => Self::empty(name, length, opts.timing),
            None => return None,
        };
        (p.title, p.audio, p.transition, p.folder) = (title, audio, transition, opts.folder.trim().to_string());
        Some(p)
    }

    fn empty(name: &str, length: Time, timing: Timing) -> Self {
        Self {
            name: name.trim().to_string(),
            effects: Vec::new(),
            masks: Vec::new(),
            layer: [0.0, 0.0],
            motion: None,
            blend: None,
            timing,
            length,
            title: None,
            audio: None,
            transition: None,
            folder: String::new(),
            favourite: false,
            thumb: None,
            builtin: false,
        }
    }

    /// Does it change a clip's picture (effects, masks, motion or blend mode)?
    pub fn has_visual(&self) -> bool {
        !self.effects.is_empty() || !self.masks.is_empty() || self.motion.is_some() || self.blend.is_some()
    }

    /// The audio to put on a clip (media from `source_in`, `length` long).
    pub fn audio_on(&self, source_in: Time, length: Time) -> Option<ClipAudio> {
        let a = self.audio.as_ref()?;
        let mut a = retime_audio(a, &|t| self.place(t, source_in, length));
        a.fade_in = a.fade_in.min(length);
        a.fade_out = a.fade_out.min(length - a.fade_in);
        Some(a)
    }

    /// A preset from a clip's look `v` (its media starts at `source_in`, it
    /// lasts `length`, its layer is `layer` pixels), with what `opts` chooses.
    /// `None` when that's nothing.
    pub fn from_visual(name: &str, v: &ClipVisual, source_in: Time, length: Time, layer: [f32; 2], opts: &SaveOptions) -> Option<Self> {
        let rel = |t: Time| t - source_in;
        let effects: Vec<Effect> = v
            .effects
            .iter()
            .enumerate()
            .filter(|(i, e)| savable(e) && opts.effects.as_ref().is_none_or(|w| w.contains(i)))
            .map(|(_, e)| e.clone())
            .collect();
        let masks = if opts.masks { retime_masks(&v.masks, &rel) } else { Vec::new() };
        let motion = opts.motion.then(|| {
            let mut transform = Transform::default();
            for p in MOTION_PROPS {
                transform.copy_prop(p, &v.transform);
            }
            transform.three_d = v.transform.three_d;
            transform.lit = v.transform.lit;
            Motion { transform, anim: retime_motion(&v.anim, &rel), props: Vec::new() }
        });
        let blend = opts.blend.then_some(v.blend);
        if effects.is_empty() && masks.is_empty() && motion.is_none() && blend.is_none() {
            return None;
        }
        Some(Self { effects: retime_effects(&effects, &rel), masks, layer, motion, blend, folder: opts.folder.trim().to_string(), ..Self::empty(name, length, opts.timing) })
    }

    /// Preset time `t` (from the original clip's start) → source time on a
    /// clip whose media starts at `source_in` and which lasts `length`.
    fn place(&self, t: Time, source_in: Time, length: Time) -> Time {
        let rel = match self.timing {
            Timing::FromStart => t,
            Timing::FromEnd => length - (self.length - t),
            Timing::Stretch if self.length > Time::ZERO => Time::from_secs_f64(t.as_secs_f64() * length.as_secs_f64() / self.length.as_secs_f64()),
            Timing::Stretch => t,
        };
        source_in + rel
    }

    /// The effects to put on a clip whose media starts at `source_in` (and
    /// which lasts `length`).
    pub fn effects_for(&self, source_in: Time) -> Vec<Effect> {
        self.effects_on(source_in, self.length)
    }

    pub fn effects_on(&self, source_in: Time, length: Time) -> Vec<Effect> {
        retime_effects(&self.effects, &|t| self.place(t, source_in, length))
    }

    /// Put the preset on look `v` of a clip (media from `source_in`, `length`
    /// long, a `layer`-pixel layer): its effects after the clip's own (or
    /// instead of them, with `replace`; a camera's, light's or point's
    /// settings stay), its masks likewise, and its motion and blend mode in
    /// place of the clip's.
    pub fn apply(&self, v: &mut ClipVisual, source_in: Time, length: Time, layer: [f32; 2], replace: bool) {
        let f = |t: Time| self.place(t, source_in, length);
        if replace {
            v.effects.retain(|e| !savable(e));
        }
        for e in retime_effects(&self.effects, &f) {
            // A water, liquid or fluid look on a clip that already has that
            // simulation: just its look, on the one there (where it is and
            // what it's poured onto stay).
            let look = look_params(e.kind);
            if let Some(there) = v.effects.iter_mut().find(|x| x.kind == e.kind).filter(|_| !look.is_empty()) {
                for &i in look {
                    if let (Some(to), Some(from)) = (there.params.get_mut(i), e.params.get(i)) {
                        *to = from.clone();
                    }
                }
                continue;
            }
            v.effects.push(e);
        }
        if !self.masks.is_empty() {
            let mut masks = retime_masks(&self.masks, &f);
            scale_masks(&mut masks, self.layer, layer);
            if replace {
                v.masks.clear();
            }
            v.masks.extend(masks);
        }
        if let Some(m) = &self.motion {
            let a = retime_motion(&m.anim, &f);
            for p in MOTION_PROPS.into_iter().filter(|p| m.sets(*p)) {
                v.transform.copy_prop(p, &m.transform);
                match p {
                    Prop::Position => v.anim.position = a.position.clone(),
                    Prop::Scale => v.anim.scale = a.scale.clone(),
                    Prop::Rotation => v.anim.rotation = a.rotation.clone(),
                    Prop::Opacity => v.anim.opacity = a.opacity.clone(),
                    Prop::Anchor => v.anim.anchor = a.anchor.clone(),
                    Prop::Depth => v.anim.depth = a.depth.clone(),
                    Prop::Orientation => v.anim.orientation = a.orientation.clone(),
                    Prop::Crop | Prop::Pin => {}
                }
            }
            if m.props.is_empty() {
                v.transform.three_d = m.transform.three_d;
                v.transform.lit = m.transform.lit;
            }
        }
        if let Some(b) = self.blend {
            v.blend = b;
        }
    }

    pub fn keyframes(&self) -> usize {
        let fx: usize = self.effects.iter().flat_map(|e| &e.params).map(|p| p.keys.0.len()).sum();
        let masks: usize = self.masks.iter().map(|m| m.keys.len()).sum();
        let motion = self.motion.as_ref().map_or(0, |m| {
            let a = &m.anim;
            a.position.0.len() + a.scale.0.len() + a.rotation.0.len() + a.opacity.0.len() + a.anchor.0.len() + a.depth.0.len() + a.orientation.0.len()
        });
        let audio = self.audio.as_ref().map_or(0, |a| a.volume_keys.len() + a.pan_keys.len());
        fx + masks + motion + audio
    }

    /// "Glow, Blur + 1 mask, motion, Screen · 4 keyframes from the end".
    pub fn summary(&self) -> String {
        let mut s: String = self.effects.iter().map(Effect::label).collect::<Vec<_>>().join(", ");
        let mut extra: Vec<String> = Vec::new();
        match self.masks.len() {
            0 => {}
            1 => extra.push("1 mask".into()),
            n => extra.push(format!("{n} masks")),
        }
        if let Some(m) = &self.motion {
            extra.push(if m.props.len() == 1 { m.props[0].clone() } else { "motion".into() });
        }
        if let Some(b) = self.blend {
            extra.push(format!("{} blend", b.label()));
        }
        if self.title.is_some() {
            extra.push("title style".into());
        }
        if self.audio.is_some() {
            extra.push("audio levels".into());
        }
        if let Some(t) = self.transition {
            extra.push(format!("{} in ({:.1} s)", t.kind.label(), t.length.as_secs_f64()));
        }
        if !extra.is_empty() {
            if !s.is_empty() {
                s.push_str(" + ");
            }
            s.push_str(&extra.join(", "));
        }
        let k = self.keyframes();
        if k > 0 {
            s.push_str(&format!(" · {k} keyframe{}", if k == 1 { "" } else { "s" }));
            s.push_str(match self.timing {
                Timing::FromStart => "",
                Timing::FromEnd => " from the end",
                Timing::Stretch => ", stretched to fit",
            });
        }
        s
    }
}

// ---- Starter looks ---------------------------------------------------------------

/// The folder the starter looks are in.
pub const STARTERS: &str = "Starter looks";
/// Neon signs: Neon path round a title's letters.
pub const NEON_SIGNS: &str = "Neon signs";
/// Ready-made lighting: flares, rays, dirt and glow together.
pub const LIGHT_LOOKS: &str = "Light looks";
/// Water, liquid and fluid looks: seas, surfaces, honey and paint, ink and smoke.
pub const WATER_LOOKS: &str = "Water and fluid looks";
/// The folders the built-in presets are in, in order.
pub const BUILTIN_FOLDERS: [&str; 4] = [STARTERS, NEON_SIGNS, LIGHT_LOOKS, WATER_LOOKS];

/// The settings that make a water, liquid or fluid simulation's look (its
/// colours, how runny or wild it is, the light on it), as opposed to where it
/// is and what it's poured onto (its source, direction, camera, solid objects,
/// what's shown). A look put on a clip that already has that simulation sets
/// just these on it (see [`EffectPreset::apply`]).
pub fn look_params(kind: EffectKind) -> &'static [usize] {
    use reelforge_core::effects::{fluid_params as fl, liquid as lq, ocean as oc, water as ws};
    match kind {
        EffectKind::Ocean => &[oc::WIND, oc::HEIGHT, oc::CHOP, oc::SWELL, oc::RIPPLES, oc::SPEED, oc::DEEP, oc::SCATTER, oc::FOAM, oc::SUN_HEIGHT, oc::SUN, oc::SKY, oc::HORIZON, oc::HAZE, oc::SURF, oc::SPRAY],
        EffectKind::WaterSurface => &[ws::RAIN, ws::DROP_SIZE, ws::DROP_STRENGTH, ws::WAVE_HEIGHT, ws::WAVELENGTH, ws::WIND, ws::SPEED, ws::CALM, ws::COLOR, ws::DEPTH, ws::REFRACTION, ws::CAUSTICS, ws::REFLECTIONS, ws::HIGHLIGHTS, ws::SKY, ws::FLOOR],
        EffectKind::Liquid => &[lq::THICK, lq::SPLASH, lq::DROPLETS, lq::BOUNCE, lq::COLOR, lq::CLEAR, lq::REFRACTION, lq::HIGHLIGHTS, lq::FOAM, lq::SMOOTH],
        EffectKind::Fluid => &[fl::FORCE, fl::AMOUNT, fl::COLOR_A, fl::COLOR_B, fl::CYCLE, fl::RISE, fl::SWIRL, fl::FADE],
        _ => &[],
    }
}

fn fx(kind: EffectKind, set: &[(usize, f32)]) -> Effect {
    let mut e = Effect::new(kind);
    for &(i, v) in set {
        e.params[i].value = v;
    }
    e
}

/// An opacity-only move: keys (time from the start, opacity 0–1).
fn opacity_move(keys: &[(f64, f32)]) -> Motion {
    let mut anim = TransformAnim::default();
    anim.opacity = Curve(keys.iter().map(|&(t, v)| reelforge_core::anim::Key::new(Time::from_secs_f64(t), v, reelforge_core::anim::Interp::Ease)).collect());
    Motion { transform: Transform::default(), anim, props: vec!["opacity".into()] }
}

/// The looks that come with Reelforge, made of built-in effects only.
pub fn starters() -> Vec<EffectPreset> {
    use reelforge_core::effects::{aberration, glitch, grain, crt, key, halftone, neon};
    let one = Time::from_secs_f64(1.0);
    let look = |name: &str, effects: Vec<Effect>| EffectPreset { effects, folder: STARTERS.into(), builtin: true, ..EffectPreset::empty(name, Time::ZERO, Timing::FromStart) };
    let mut teal = Effect::new(EffectKind::Curves);
    teal.data = Some(reelforge_core::EffectData::Curves(reelforge_core::lut::Curves {
        master: vec![[0.0, 0.0], [0.25, 0.22], [0.75, 0.79], [1.0, 1.0]],
        red: vec![[0.0, 0.0], [0.5, 0.5], [0.8, 0.86], [1.0, 1.0]],
        green: vec![[0.0, 0.0], [1.0, 1.0]],
        blue: vec![[0.0, 0.06], [0.3, 0.33], [0.7, 0.64], [1.0, 0.92]],
    }));
    let mut out = vec![
        look("Teal and orange", vec![teal, fx(EffectKind::ColorCorrect, &[(2, 112.0)])]),
        look("Black and white", vec![fx(EffectKind::ColorCorrect, &[(1, 115.0), (2, 0.0)])]),
        look("Film look", vec![fx(EffectKind::ColorCorrect, &[(1, 108.0), (2, 88.0), (3, 8.0)]), fx(EffectKind::FilmGrain, &[(grain::AMOUNT, 18.0)]), fx(EffectKind::Vignette, &[(0, 30.0)])]),
        look("Dreamy glow", vec![fx(EffectKind::Glow, &[(0, 55.0), (1, 40.0), (2, 70.0)]), fx(EffectKind::ColorCorrect, &[(0, 0.15), (1, 92.0)])]),
        look("Warm and soft", vec![fx(EffectKind::ColorCorrect, &[(1, 95.0), (3, 25.0)]), fx(EffectKind::Glow, &[(0, 75.0), (1, 25.0), (2, 35.0)])]),
        look("Old video (VHS)", vec![
            fx(EffectKind::ChromaticAberration, &[(aberration::AMOUNT, 4.0), (1, 1.0)]),
            fx(EffectKind::Glitch, &[(glitch::AMOUNT, 8.0)]),
            fx(EffectKind::FilmGrain, &[(grain::AMOUNT, 30.0)]),
            fx(EffectKind::ColorCorrect, &[(2, 80.0)]),
        ]),
        look("Neon sign", vec![fx(EffectKind::NeonEdges, &[(neon::ORIGINAL, 15.0), (neon::WIDTH, 5.0), (neon::GLOW_RADIUS, 30.0), (neon::FLICKER, 25.0)])]),
        look("Comic print", vec![fx(EffectKind::Halftone, &[(halftone::STYLE, 1.0), (halftone::SIZE, 7.0), (halftone::CONTRAST, 130.0)])]),
        look("Old TV", vec![fx(EffectKind::Crt, &[(crt::BLEED, 1.0), (crt::WOBBLE, 0.5), (crt::NOISE, 6.0)])]),
        look("VHS tape", vec![fx(EffectKind::Crt, &[(crt::CURVATURE, 0.0), (crt::SCANLINES, 20.0), (crt::MASK, 0.0), (crt::BLEED, 8.0), (crt::WOBBLE, 3.0), (crt::NOISE, 20.0), (crt::TRACKING, 50.0), (crt::VIGNETTE, 20.0), (crt::BRIGHTNESS, 105.0)])]),
        look("Green screen", vec![fx(EffectKind::ChromaKey, &[(key::CHOKE, 1.0), (key::EDGE_SOFTNESS, 1.5)])]),
        look("Blue screen", vec![fx(EffectKind::ChromaKey, &[(0, 230.0), (key::CHOKE, 1.0), (key::EDGE_SOFTNESS, 1.5)])]),
    ];
    out.push(EffectPreset { motion: Some(opacity_move(&[(0.0, 0.0), (1.0, 1.0)])), ..look("Fade in", Vec::new()) });
    out.push(EffectPreset { motion: Some(opacity_move(&[(0.0, 1.0), (1.0, 0.0)])), timing: Timing::FromEnd, length: one, ..look("Fade out", Vec::new()) });
    let push = TransformAnim { scale: Curve(vec![reelforge_core::anim::Key::new(Time::ZERO, [1.0, 1.0], reelforge_core::anim::Interp::Linear), reelforge_core::anim::Key::new(one, [1.1, 1.1], reelforge_core::anim::Interp::Linear)]), ..TransformAnim::default() };
    out.push(EffectPreset { motion: Some(Motion { transform: Transform::default(), anim: push, props: vec!["scale".into()] }), timing: Timing::Stretch, length: one, ..look("Slow push in", Vec::new()) });
    out.extend(neon_signs());
    out.extend(light_looks());
    out.extend(water_looks());
    out
}

/// Seas, water seen from above, liquids and fluids: each one simulation
/// effect with its look set (put on a clip that has that simulation, it
/// changes just the look of the one there).
fn water_looks() -> Vec<EffectPreset> {
    use reelforge_core::effects::{fluid_params as fl, liquid as lq, ocean as oc, water as ws};
    let look = |name: &str, kind: EffectKind, set: &[(usize, f32)]| EffectPreset { effects: vec![fx(kind, set)], folder: WATER_LOOKS.into(), builtin: true, ..EffectPreset::empty(name, Time::ZERO, Timing::FromStart) };
    let c = |rgb: u32| rgb as f32;
    use EffectKind::{Fluid, Liquid, Ocean, WaterSurface};
    vec![
        // Seas, to the horizon.
        look("Calm lake at dawn", Ocean, &[(oc::WIND, 2.0), (oc::HEIGHT, 15.0), (oc::CHOP, 10.0), (oc::SWELL, 0.0), (oc::RIPPLES, 40.0), (oc::SPEED, 70.0), (oc::DEEP, c(0x0e2a30)), (oc::SCATTER, c(0x2f6f6a)), (oc::FOAM, 0.0), (oc::SUN_HEIGHT, 6.0), (oc::SUN, 140.0), (oc::SKY, c(0xe9b48c)), (oc::HORIZON, c(0xffd9b4)), (oc::HAZE, 55.0), (oc::SURF, 40.0), (oc::SPRAY, 0.0)]),
        look("Harbour chop", Ocean, &[(oc::WIND, 14.0), (oc::HEIGHT, 45.0), (oc::CHOP, 120.0), (oc::SWELL, 10.0), (oc::RIPPLES, 140.0), (oc::SPEED, 110.0), (oc::DEEP, c(0x10303a)), (oc::SCATTER, c(0x2d6e64)), (oc::FOAM, 40.0), (oc::SUN_HEIGHT, 30.0), (oc::SUN, 70.0), (oc::SKY, c(0x8fa8bc)), (oc::HORIZON, c(0xd0d8dc)), (oc::HAZE, 25.0), (oc::SURF, 120.0), (oc::SPRAY, 60.0)]),
        look("Open sea swell", Ocean, &[(oc::WIND, 6.0), (oc::HEIGHT, 160.0), (oc::CHOP, 40.0), (oc::SWELL, 220.0), (oc::RIPPLES, 50.0), (oc::SPEED, 90.0), (oc::DEEP, c(0x062a44)), (oc::SCATTER, c(0x1d7f92)), (oc::FOAM, 25.0), (oc::SUN_HEIGHT, 35.0), (oc::SUN, 110.0), (oc::SKY, c(0x4a87c8)), (oc::HORIZON, c(0xc4d8ea)), (oc::HAZE, 30.0), (oc::SURF, 100.0), (oc::SPRAY, 80.0)]),
        look("Storm at sea", Ocean, &[(oc::WIND, 32.0), (oc::HEIGHT, 300.0), (oc::CHOP, 140.0), (oc::SWELL, 180.0), (oc::RIPPLES, 160.0), (oc::SPEED, 120.0), (oc::DEEP, c(0x0b1a20)), (oc::SCATTER, c(0x24524e)), (oc::FOAM, 180.0), (oc::SUN_HEIGHT, 12.0), (oc::SUN, 30.0), (oc::SKY, c(0x47505a)), (oc::HORIZON, c(0x7a858c)), (oc::HAZE, 70.0), (oc::SURF, 220.0), (oc::SPRAY, 250.0)]),
        // Water from above.
        look("Tropical lagoon", WaterSurface, &[(ws::RAIN, 0.0), (ws::WAVE_HEIGHT, 80.0), (ws::WAVELENGTH, 260.0), (ws::WIND, 40.0), (ws::CALM, 10.0), (ws::COLOR, c(0x0fb2c8)), (ws::DEPTH, 72.0), (ws::REFRACTION, 120.0), (ws::CAUSTICS, 220.0), (ws::REFLECTIONS, 40.0), (ws::HIGHLIGHTS, 140.0), (ws::SKY, c(0xcfe8f5)), (ws::FLOOR, c(0xf1dfb0))]),
        look("Swimming pool", WaterSurface, &[(ws::RAIN, 3.0), (ws::DROP_SIZE, 8.0), (ws::DROP_STRENGTH, 40.0), (ws::WAVE_HEIGHT, 60.0), (ws::WAVELENGTH, 140.0), (ws::WIND, 50.0), (ws::CALM, 10.0), (ws::COLOR, c(0x2fa8e0)), (ws::DEPTH, 60.0), (ws::REFRACTION, 140.0), (ws::CAUSTICS, 300.0), (ws::REFLECTIONS, 50.0), (ws::HIGHLIGHTS, 160.0), (ws::SKY, c(0xdff0fa)), (ws::FLOOR, c(0xd8eef5))]),
        look("Rain on a pond", WaterSurface, &[(ws::RAIN, 40.0), (ws::DROP_SIZE, 10.0), (ws::DROP_STRENGTH, 90.0), (ws::WIND, 10.0), (ws::CALM, 20.0), (ws::COLOR, c(0x24453a)), (ws::DEPTH, 70.0), (ws::CAUSTICS, 30.0), (ws::REFLECTIONS, 110.0), (ws::HIGHLIGHTS, 70.0), (ws::SKY, c(0x9aa8b0)), (ws::FLOOR, c(0x4a4030))]),
        // Liquids poured.
        look("Clear water", Liquid, &[(lq::THICK, 0.0), (lq::SPLASH, 90.0), (lq::DROPLETS, 120.0), (lq::BOUNCE, 20.0), (lq::COLOR, c(0x6fb8e8)), (lq::CLEAR, 85.0), (lq::REFRACTION, 120.0), (lq::HIGHLIGHTS, 120.0), (lq::FOAM, 70.0), (lq::SMOOTH, 50.0)]),
        look("Muddy water", Liquid, &[(lq::THICK, 15.0), (lq::SPLASH, 75.0), (lq::DROPLETS, 80.0), (lq::BOUNCE, 15.0), (lq::COLOR, c(0x6b5233)), (lq::CLEAR, 5.0), (lq::REFRACTION, 30.0), (lq::HIGHLIGHTS, 70.0), (lq::FOAM, 30.0), (lq::SMOOTH, 55.0)]),
        look("Milk", Liquid, &[(lq::THICK, 20.0), (lq::SPLASH, 70.0), (lq::DROPLETS, 70.0), (lq::BOUNCE, 15.0), (lq::COLOR, c(0xf4f1ea)), (lq::CLEAR, 0.0), (lq::REFRACTION, 0.0), (lq::HIGHLIGHTS, 80.0), (lq::FOAM, 20.0), (lq::SMOOTH, 75.0)]),
        look("Honey", Liquid, &[(lq::THICK, 85.0), (lq::SPLASH, 10.0), (lq::DROPLETS, 15.0), (lq::BOUNCE, 5.0), (lq::COLOR, c(0xf2b632)), (lq::CLEAR, 30.0), (lq::REFRACTION, 90.0), (lq::HIGHLIGHTS, 140.0), (lq::FOAM, 0.0), (lq::SMOOTH, 90.0)]),
        look("Paint", Liquid, &[(lq::THICK, 70.0), (lq::SPLASH, 20.0), (lq::DROPLETS, 25.0), (lq::BOUNCE, 5.0), (lq::COLOR, c(0xd8302a)), (lq::CLEAR, 0.0), (lq::REFRACTION, 0.0), (lq::HIGHLIGHTS, 80.0), (lq::FOAM, 0.0), (lq::SMOOTH, 85.0)]),
        look("Oil", Liquid, &[(lq::THICK, 55.0), (lq::SPLASH, 30.0), (lq::DROPLETS, 30.0), (lq::BOUNCE, 10.0), (lq::COLOR, c(0x2a2418)), (lq::CLEAR, 10.0), (lq::REFRACTION, 40.0), (lq::HIGHLIGHTS, 220.0), (lq::FOAM, 0.0), (lq::SMOOTH, 85.0)]),
        // Fluids: smoke and ink.
        look("Ink in water", Fluid, &[(fl::FORCE, 150.0), (fl::AMOUNT, 160.0), (fl::COLOR_A, c(0x1e3a8a)), (fl::COLOR_B, c(0x3a1a70)), (fl::CYCLE, 0.05), (fl::RISE, -20.0), (fl::SWIRL, 90.0), (fl::FADE, 1.0)]),
        look("Coloured smoke", Fluid, &[(fl::FORCE, 350.0), (fl::AMOUNT, 120.0), (fl::COLOR_A, c(0xff4fa0)), (fl::COLOR_B, c(0x6a5cff)), (fl::CYCLE, 0.3), (fl::RISE, 120.0), (fl::SWIRL, 60.0), (fl::FADE, 10.0)]),
        look("Steam", Fluid, &[(fl::FORCE, 250.0), (fl::AMOUNT, 70.0), (fl::COLOR_A, c(0xe8eef2)), (fl::COLOR_B, c(0xf6f8fa)), (fl::CYCLE, 0.0), (fl::RISE, 200.0), (fl::SWIRL, 70.0), (fl::FADE, 25.0)]),
        look("Thick smoke", Fluid, &[(fl::FORCE, 300.0), (fl::AMOUNT, 250.0), (fl::COLOR_A, c(0x6a6a6a)), (fl::COLOR_B, c(0x3c3c3c)), (fl::CYCLE, 0.1), (fl::RISE, 90.0), (fl::SWIRL, 120.0), (fl::FADE, 5.0)]),
    ]
}

/// A Neon path round a title's letters, with only the tube showing.
pub fn neon_sign(glow: u32, core: u32, set: &[(usize, f32)]) -> Effect {
    use reelforge_core::effects::neonpath as n;
    let mut e = fx(EffectKind::NeonPath, &[(n::MASK, 0.0), (n::SHOW_LAYER, 0.0), (n::GLOW_COLOR, glow as f32), (n::CORE_COLOR, core as f32), (n::GLOW_SIZE, 20.0), (n::GLOW, 140.0), (n::CORE_WIDTH, 4.0)]);
    for &(i, v) in set {
        e.params[i].value = v;
    }
    e
}

/// Neon signs for titles: the letters' outlines as glowing tubes.
fn neon_signs() -> Vec<EffectPreset> {
    use reelforge_core::anim::{Interp, Key};
    use reelforge_core::effects::neonpath as n;
    let sign = |name: &str, e: Effect| EffectPreset { effects: vec![e], folder: NEON_SIGNS.into(), builtin: true, ..EffectPreset::empty(name, Time::ZERO, Timing::FromStart) };
    let mut on = neon_sign(0xff2633, 0xffe9ea, &[]);
    on.params[n::END].keys = Curve(vec![Key::new(Time::ZERO, 0.0, Interp::Linear), Key::new(Time::from_secs_f64(1.5), 100.0, Interp::Linear)]);
    vec![
        sign("Neon pink", neon_sign(0xff2bd6, 0xfff0fa, &[])),
        sign("Neon ice blue", neon_sign(0x26b7ff, 0xeaf8ff, &[])),
        sign("Neon warm white", neon_sign(0xffa640, 0xfff7e6, &[(n::GLOW, 110.0)])),
        sign("Neon green, failing tube", neon_sign(0x2dff6e, 0xeaffef, &[(n::FLICKER, 55.0), (n::FLICKER_SPEED, 8.0)])),
        sign("Neon red, writing on", on),
        sign("Neon switching on", neon_sign(0xff2bd6, 0xfff0fa, &[(n::SWITCH_ON, 2.5), (n::STUTTER, 80.0)])),
        sign("Neon blue, warming up at random", neon_sign(0x26b7ff, 0xeaf8ff, &[(n::SWITCH_ON, 3.0), (n::STUTTER, 100.0), (n::ORDER, 1.0)])),
        sign("Neon purple marquee", neon_sign(0xa64bff, 0xf3e8ff, &[(n::DASH, 14.0), (n::GAP, 10.0), (n::DASH_SPEED, 40.0)])),
        sign("Neon over the letters", neon_sign(0xff2bd6, 0xfff0fa, &[(n::SHOW_LAYER, 35.0), (n::LIGHT_LAYER, 60.0)])),
    ]
}

/// Lighting looks made of the light effects together.
fn light_looks() -> Vec<EffectPreset> {
    use reelforge_core::effects::{dirt, flare, rays};
    let light = |name: &str, effects: Vec<Effect>| EffectPreset { effects, folder: LIGHT_LOOKS.into(), builtin: true, ..EffectPreset::empty(name, Time::ZERO, Timing::FromStart) };
    let sun = |x: f32, y: f32| [(flare::X, x), (flare::Y, y)];
    vec![
        light("Sunset", vec![
            fx(EffectKind::ColorCorrect, &[(2, 108.0), (3, 30.0)]),
            fx(EffectKind::LightRays, &[(rays::X, 75.0), (rays::Y, 30.0), (2, 45.0), (3, 90.0), (4, 55.0), (6, 0xffb070 as f32)]),
            fx(EffectKind::LensFlare, &[sun(75.0, 30.0)[0], sun(75.0, 30.0)[1], (flare::BRIGHTNESS, 120.0), (4, 0xffa060 as f32), (5, 3.0)]),
            fx(EffectKind::LensDirt, &[(dirt::X, 75.0), (dirt::Y, 30.0), (2, 1.0), (3, 70.0), (8, 0xffd6a8 as f32)]),
        ]),
        light("Sci-fi corridor", vec![
            fx(EffectKind::ColorCorrect, &[(1, 112.0), (3, -25.0)]),
            fx(EffectKind::Glow, &[(0, 65.0), (1, 30.0), (2, 120.0)]),
            fx(EffectKind::AnamorphicFlare, &[(0, 75.0), (1, 900.0), (2, 140.0), (4, 0x4aa8ff as f32)]),
            fx(EffectKind::LensFlare, &[(flare::X, 50.0), (flare::Y, 45.0), (flare::BRIGHTNESS, 70.0), (4, 0x8fc8ff as f32), (5, 6.0)]),
        ]),
        light("Night city", vec![
            fx(EffectKind::ColorCorrect, &[(1, 110.0), (2, 115.0), (3, -10.0), (4, 8.0)]),
            fx(EffectKind::Glow, &[(0, 60.0), (1, 40.0), (2, 90.0)]),
            fx(EffectKind::AnamorphicFlare, &[(0, 88.0), (1, 500.0), (2, 70.0), (4, 0x6f8cff as f32)]),
            fx(EffectKind::LensDirt, &[(3, 90.0), (5, 55.0), (7, 80.0)]),
        ]),
        light("Golden hour haze", vec![
            fx(EffectKind::ColorCorrect, &[(1, 92.0), (3, 22.0)]),
            fx(EffectKind::LightLeaks, &[(0, 0xffb347 as f32), (1, 0xff7a4a as f32), (2, 40.0)]),
            fx(EffectKind::LightRays, &[(rays::X, 85.0), (rays::Y, 15.0), (2, 60.0), (3, 70.0), (6, 0xffd9a0 as f32)]),
            fx(EffectKind::Glow, &[(0, 70.0), (1, 30.0), (2, 45.0)]),
        ]),
        light("Stage spotlight", vec![
            fx(EffectKind::LightRays, &[(rays::X, 50.0), (rays::Y, -5.0), (2, 70.0), (3, 110.0), (4, 45.0)]),
            fx(EffectKind::LensFlare, &[(flare::X, 50.0), (flare::Y, 5.0), (flare::BRIGHTNESS, 90.0), (5, 8.0)]),
            fx(EffectKind::Vignette, &[(0, 45.0)]),
        ]),
        light("Summer sun", vec![
            fx(EffectKind::LensFlare, &[(flare::X, 82.0), (flare::Y, 14.0), (flare::BRIGHTNESS, 110.0), (5, 0.0)]),
            fx(EffectKind::LensDirt, &[(dirt::X, 82.0), (dirt::Y, 14.0), (2, 1.0), (3, 60.0)]),
            fx(EffectKind::ColorCorrect, &[(2, 110.0), (3, 12.0)]),
        ]),
    ]
}

/// The saved presets, in one file.
#[derive(Debug)]
pub struct PresetLibrary {
    file: PathBuf,
    pub list: Vec<EffectPreset>,
    /// Starter looks marked as favourites (by name).
    starred: Vec<String>,
}

/// The file: the presets, and which starter looks are favourites. (Older
/// files are just the list.)
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum LibraryFile {
    Full {
        presets: Vec<EffectPreset>,
        #[serde(default)]
        starred: Vec<String>,
    },
    List(Vec<EffectPreset>),
}

fn same(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

fn err(msg: impl Into<String>) -> std::io::Error {
    std::io::Error::other(msg.into())
}

/// Presets exported to a file (.rfpreset).
#[derive(Serialize, Deserialize)]
struct Exported {
    reelforge_presets: u32,
    presets: Vec<EffectPreset>,
}

impl PresetLibrary {
    pub fn load(file: &Path) -> Self {
        let parsed = std::fs::read_to_string(file).ok().and_then(|s| serde_json::from_str::<LibraryFile>(&s).ok());
        let (list, starred) = match parsed {
            Some(LibraryFile::Full { presets, starred }) => (presets, starred),
            Some(LibraryFile::List(l)) => (l, Vec::new()),
            None => (Vec::new(), Vec::new()),
        };
        Self { file: file.to_path_buf(), list, starred }
    }

    fn save(&self) -> std::io::Result<()> {
        if let Some(d) = self.file.parent() {
            std::fs::create_dir_all(d)?;
        }
        let tmp = self.file.with_extension("tmp");
        let body = LibraryFile::Full { presets: self.list.clone(), starred: self.starred.clone() };
        std::fs::write(&tmp, serde_json::to_string_pretty(&body).unwrap_or_default())?;
        std::fs::rename(tmp, &self.file)
    }

    /// Where presets' pictures are kept.
    pub fn pictures(&self) -> PathBuf {
        self.file.parent().map_or_else(|| PathBuf::from("preset-pictures"), |d| d.join("preset-pictures"))
    }

    /// Everything: your presets, then the starter looks; favourites marked.
    pub fn all(&self) -> Vec<EffectPreset> {
        let mut v = self.list.clone();
        v.extend(starters().into_iter().map(|mut p| {
            p.favourite = self.starred.iter().any(|n| same(n, &p.name));
            p
        }));
        v
    }

    pub fn get(&self, name: &str) -> Option<EffectPreset> {
        self.all().into_iter().find(|p| same(&p.name, name))
    }

    fn is_starter(name: &str) -> bool {
        starters().iter().any(|p| same(&p.name, name))
    }

    /// Add `p`, replacing one of the same name (case aside). Kept in name order.
    pub fn put(&mut self, p: EffectPreset) -> std::io::Result<()> {
        if p.name.is_empty() {
            return Err(err("a preset needs a name"));
        }
        if Self::is_starter(&p.name) {
            return Err(err(format!("\"{}\" is one of the starter looks: choose another name", p.name)));
        }
        // Keep the old one's favourite mark and folder if it's replaced without one.
        let old = self.list.iter().find(|q| same(&q.name, &p.name)).cloned();
        let mut p = p;
        if let Some(o) = &old {
            p.favourite |= o.favourite;
            if o.thumb != p.thumb {
                self.drop_picture(o.thumb.as_deref());
            }
        }
        self.list.retain(|q| !same(&q.name, &p.name));
        self.list.push(p);
        self.list.sort_by_key(|p| p.name.to_lowercase());
        self.save()
    }

    pub fn rename(&mut self, from: &str, to: &str) -> std::io::Result<()> {
        let to = to.trim();
        if to.is_empty() {
            return Err(err("a preset needs a name"));
        }
        if Self::is_starter(from) {
            return Err(err("the starter looks can't be renamed"));
        }
        if !same(from, to) && (self.list.iter().any(|p| same(&p.name, to)) || Self::is_starter(to)) {
            return Err(err(format!("there's already a preset called \"{to}\"")));
        }
        let Some(p) = self.list.iter_mut().find(|p| same(&p.name, from)) else { return Err(err("no such preset")) };
        p.name = to.to_string();
        self.list.sort_by_key(|p| p.name.to_lowercase());
        self.save()
    }

    fn drop_picture(&self, thumb: Option<&str>) {
        if let Some(t) = thumb {
            let _ = std::fs::remove_file(self.pictures().join(t));
        }
    }

    pub fn remove(&mut self, name: &str) -> std::io::Result<()> {
        if Self::is_starter(name) {
            return Err(err("the starter looks can't be deleted"));
        }
        if let Some(p) = self.list.iter().find(|p| same(&p.name, name)) {
            self.drop_picture(p.thumb.as_deref());
        }
        self.list.retain(|p| !same(&p.name, name));
        self.save()
    }

    pub fn set_favourite(&mut self, name: &str, on: bool) -> std::io::Result<()> {
        if Self::is_starter(name) {
            self.starred.retain(|n| !same(n, name));
            if on {
                self.starred.push(name.to_string());
            }
        } else {
            let Some(p) = self.list.iter_mut().find(|p| same(&p.name, name)) else { return Err(err("no such preset")) };
            p.favourite = on;
        }
        self.save()
    }

    /// File preset `name` under `folder` ("" for none).
    pub fn set_folder(&mut self, name: &str, folder: &str) -> std::io::Result<()> {
        if Self::is_starter(name) {
            return Err(err("the starter looks stay in their folder"));
        }
        let Some(p) = self.list.iter_mut().find(|p| same(&p.name, name)) else { return Err(err("no such preset")) };
        p.folder = folder.trim().to_string();
        self.save()
    }

    /// The folders your presets are in, by name.
    pub fn folders(&self) -> Vec<String> {
        let mut v: Vec<String> = self.list.iter().map(|p| p.folder.clone()).filter(|f| !f.is_empty()).collect();
        v.sort_by_key(|f| f.to_lowercase());
        v.dedup_by(|a, b| same(a, b));
        v
    }

    /// Presets `names` as the contents of a .rfpreset file.
    pub fn export(&self, names: &[String]) -> String {
        let presets: Vec<EffectPreset> = self.all().into_iter().filter(|p| names.iter().any(|n| same(n, &p.name))).map(|p| EffectPreset { thumb: None, favourite: false, ..p }).collect();
        serde_json::to_string_pretty(&Exported { reelforge_presets: 1, presets }).unwrap_or_default()
    }

    /// Add the presets in a .rfpreset file's contents. A name that's taken
    /// gets a number ("Look (2)"). Returns the names added.
    pub fn import(&mut self, text: &str) -> std::io::Result<Vec<String>> {
        let e: Exported = serde_json::from_str(text).map_err(|e| err(format!("that isn't a Reelforge presets file ({e})")))?;
        let mut added = Vec::new();
        for mut p in e.presets {
            p.builtin = false;
            p.thumb = None;
            let base = p.name.trim().to_string();
            let taken = |n: &str, l: &[EffectPreset]| l.iter().any(|q| same(&q.name, n)) || Self::is_starter(n);
            let mut name = base.clone();
            let mut k = 2;
            while taken(&name, &self.list) {
                name = format!("{base} ({k})");
                k += 1;
            }
            p.name = name.clone();
            if same(&p.folder, STARTERS) {
                p.folder.clear();
            }
            self.list.push(p);
            added.push(name);
        }
        self.list.sort_by_key(|p| p.name.to_lowercase());
        self.save()?;
        Ok(added)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reelforge_core::anim::{Interp, Key};

    fn keyed(at: f64) -> Effect {
        let mut e = Effect::new(EffectKind::Blur);
        e.params[0].keys = Curve(vec![Key::new(Time::from_secs_f64(at), 0.0, Interp::Linear), Key::new(Time::from_secs_f64(at + 1.0), 8.0, Interp::Linear)]);
        e
    }

    /// The water and fluid looks: each is one simulation with its look set;
    /// put on a clip that has that simulation, only the look changes on the
    /// one there; on one that hasn't, it's added.
    #[test]
    fn water_looks_change_just_the_look() {
        use reelforge_core::effects::liquid as lq;
        let all = starters();
        let looks: Vec<_> = all.iter().filter(|p| p.folder == WATER_LOOKS).collect();
        assert!(looks.len() >= 12);
        for p in &looks {
            assert_eq!(p.effects.len(), 1, "{}", p.name);
            let e = &p.effects[0];
            assert!(!look_params(e.kind).is_empty(), "{}", p.name);
            // Each sets something other than its effect's defaults, and only look settings.
            let plain = Effect::new(e.kind);
            let changed: Vec<usize> = (0..e.params.len()).filter(|&i| e.params[i].value != plain.params[i].value).collect();
            assert!(!changed.is_empty(), "{}", p.name);
            assert!(changed.iter().all(|i| look_params(e.kind).contains(i)), "{}: {changed:?}", p.name);
        }
        let honey = looks.iter().find(|p| p.name == "Honey").unwrap();
        let mut v = ClipVisual { transform: Transform::default(), blend: BlendMode::Normal, track_matte: None, anim: TransformAnim::default(), effects: Vec::new(), masks: Vec::new(), mesh: None };
        // On a clip with a liquid poured from the right: thick and golden, still poured from there.
        let mut poured = Effect::new(EffectKind::Liquid);
        poured.params[lq::X].value = 80.0;
        poured.params[lq::THICK].keys = Curve(vec![Key::new(secs(1.0), 10.0, Interp::Linear)]);
        v.effects = vec![Effect::new(EffectKind::Blur), poured];
        honey.apply(&mut v, Time::ZERO, secs(5.0), [100.0, 100.0], false);
        assert_eq!(v.effects.len(), 2);
        let l = &v.effects[1];
        assert_eq!((l.params[lq::X].value, l.params[lq::THICK].value, l.params[lq::COLOR].value), (80.0, 85.0, 0xf2b632 as f32));
        assert!(l.params[lq::THICK].keys.is_empty(), "the look's own value, not the old keys");
        // On a clip without one: added.
        v.effects = vec![Effect::new(EffectKind::Blur)];
        honey.apply(&mut v, Time::ZERO, secs(5.0), [100.0, 100.0], false);
        assert_eq!(v.effects.iter().map(|e| e.kind).collect::<Vec<_>>(), [EffectKind::Blur, EffectKind::Liquid]);
    }

    #[test]
    fn keyframes_follow_the_clip_start() {
        // Keys at 10 s and 11 s on a clip whose media starts at 10 s…
        let p = EffectPreset::from_clip(" Soft in ", &[keyed(10.0), Effect::new(EffectKind::Camera), Effect::new(EffectKind::Glow)], Time::from_secs_f64(10.0)).unwrap();
        assert_eq!(p.name, "Soft in");
        // …the camera's settings stay behind.
        assert_eq!(p.effects.len(), 2);
        assert_eq!(p.effects[0].params[0].keys.0[0].at, Time::ZERO);
        assert_eq!(p.keyframes(), 2);
        assert!(p.summary().ends_with("2 keyframes"), "{}", p.summary());
        // …land at 3 s and 4 s on a clip starting at 3 s.
        let fx = p.effects_for(Time::from_secs_f64(3.0));
        assert_eq!(fx[0].params[0].keys.0[0].at, Time::from_secs_f64(3.0));
        assert_eq!(fx[0].params[0].keys.0[1].at, Time::from_secs_f64(4.0));
        assert_eq!(fx[1], Effect::new(EffectKind::Glow));
        // Only cameras: nothing to save.
        assert!(EffectPreset::from_clip("x", &[Effect::new(EffectKind::Light)], Time::ZERO).is_none());
    }

    fn secs(v: f64) -> Time {
        Time::from_secs_f64(v)
    }

    /// A 4 s clip (media from 10 s, 200 × 100 layer) with a blur faded up
    /// over its first second, a glow, a mask, a slide in and Screen.
    fn source() -> ClipVisual {
        let mut v = ClipVisual { transform: Transform::default(), blend: BlendMode::Screen, track_matte: None, anim: TransformAnim::default(), effects: vec![keyed(10.0), Effect::new(EffectKind::Glow)], masks: Vec::new(), mesh: None };
        let mut m = Mask { points: vec![[0.0, 0.0], [100.0, 0.0], [100.0, 50.0]], ..Mask::default() };
        m.keys = vec![reelforge_core::mask::ShapeKey::new(secs(13.0), m.points.clone())];
        v.masks.push(m);
        v.transform.scale = [0.5, 0.5];
        v.anim.position = Curve(vec![Key::new(secs(10.0), [-300.0, 0.0], Interp::Linear), Key::new(secs(11.0), [0.0, 0.0], Interp::Linear)]);
        v.anim.crop = Curve(vec![Key::new(secs(10.0), [0.0; 4], Interp::Linear)]);
        v
    }

    #[test]
    fn presets_carry_masks_motion_and_blend_when_chosen() {
        let v = source();
        let all = SaveOptions { effects: Some(vec![0]), masks: true, motion: true, blend: true, timing: Timing::FromStart, ..SaveOptions::default() };
        let p = EffectPreset::from_visual("Slide in", &v, secs(10.0), secs(4.0), [200.0, 100.0], &all).unwrap();
        assert_eq!(p.effects.len(), 1, "only the chosen effect");
        assert_eq!((p.masks.len(), p.blend), (1, Some(BlendMode::Screen)));
        assert_eq!(p.masks[0].keys[0].at, secs(3.0), "mask keys from the clip's start");
        let m = p.motion.as_ref().unwrap();
        assert_eq!(m.transform.scale, [0.5, 0.5]);
        assert_eq!(m.anim.position.0[0].at, Time::ZERO);
        assert!(m.anim.crop.is_empty(), "crop isn't motion");
        assert!(p.summary().starts_with("Gaussian blur + 1 mask, motion, Screen blend · 5 keyframes"), "{}", p.summary());
        // Onto a 2 s clip from 0 s, twice the size: masks scale, keys land from its start.
        let mut t = ClipVisual { transform: Transform::default(), blend: BlendMode::Normal, track_matte: None, anim: TransformAnim::default(), effects: vec![Effect::new(EffectKind::Vignette)], masks: Vec::new(), mesh: None };
        p.apply(&mut t, Time::ZERO, secs(2.0), [400.0, 200.0], false);
        assert_eq!(t.effects.len(), 2, "added after its own");
        assert_eq!(t.effects[1].params[0].keys.0[1].at, secs(1.0));
        assert_eq!(t.masks[0].points[1], [200.0, 0.0]);
        assert_eq!((t.transform.scale, t.blend), ([0.5, 0.5], BlendMode::Screen));
        assert_eq!(t.anim.position.0[1].at, secs(1.0));
        // Replacing: its own effects go.
        let mut r = t.clone();
        p.apply(&mut r, Time::ZERO, secs(2.0), [400.0, 200.0], true);
        assert_eq!((r.effects.len(), r.masks.len()), (1, 1));
        // Nothing chosen: no preset.
        let none = SaveOptions { effects: Some(vec![]), ..SaveOptions::default() };
        assert!(EffectPreset::from_visual("x", &v, secs(10.0), secs(4.0), [200.0, 100.0], &none).is_none());
    }

    #[test]
    fn keyframes_can_count_from_the_end_or_stretch() {
        let v = source();
        let opts = |timing| SaveOptions { effects: Some(vec![0]), motion: true, timing, ..SaveOptions::default() };
        let blank = || ClipVisual { transform: Transform::default(), blend: BlendMode::Normal, track_matte: None, anim: TransformAnim::default(), effects: Vec::new(), masks: Vec::new(), mesh: None };
        // The blur's keys are at 0 s and 1 s of a 4 s clip.
        let p = EffectPreset::from_visual("x", &v, secs(10.0), secs(4.0), [200.0, 100.0], &opts(Timing::FromEnd)).unwrap();
        let mut t = blank();
        p.apply(&mut t, secs(5.0), secs(8.0), [200.0, 100.0], false);
        // 4 s and 3 s before the end of an 8 s clip from 5 s: 9 s and 10 s.
        assert_eq!(t.effects[0].params[0].keys.0[0].at, secs(9.0));
        assert_eq!(t.effects[0].params[0].keys.0[1].at, secs(10.0));
        assert!(p.summary().ends_with("from the end"), "{}", p.summary());
        let p = EffectPreset::from_visual("x", &v, secs(10.0), secs(4.0), [200.0, 100.0], &opts(Timing::Stretch)).unwrap();
        let mut t = blank();
        p.apply(&mut t, Time::ZERO, secs(8.0), [200.0, 100.0], false);
        assert_eq!(t.effects[0].params[0].keys.0[1].at, secs(2.0), "twice as long");
        assert_eq!(t.anim.position.0[1].at, secs(2.0));
        // Presets saved before these settings load as effects-only, from the start.
        let old: EffectPreset = serde_json::from_str(r#"{"name":"Old","effects":[]}"#).unwrap();
        assert_eq!((old.timing, old.masks.len(), old.motion.is_none(), old.blend), (Timing::FromStart, 0, true, None));
    }

    #[test]
    fn starter_looks_are_sound() {
        let all = starters();
        assert!(all.len() >= 10);
        for p in &all {
            assert!(p.builtin && BUILTIN_FOLDERS.contains(&p.folder.as_str()) && p.has_visual(), "{}", p.name);
            for e in &p.effects {
                assert_eq!(e.params.len(), e.kind.params().len(), "{}: {:?}", p.name, e.kind);
            }
        }
        // Every look does something (Neon signs need a title's letters, so are checked on one elsewhere).
        for p in all.iter().filter(|p| p.folder == LIGHT_LOOKS) {
            assert!(p.effects.len() >= 3, "{}", p.name);
            for e in &p.effects {
                assert!(!e.eval_sized(Time::ZERO, None, [640.0, 360.0]).is_identity(), "{}: {:?}", p.name, e.kind);
            }
        }
        let signs: Vec<_> = all.iter().filter(|p| p.folder == NEON_SIGNS).collect();
        assert!(signs.len() >= 5 && signs.iter().all(|p| p.effects.iter().all(|e| e.kind == EffectKind::NeonPath && reelforge_core::neonpath::on_letters(&e.params.iter().map(|q| q.value).collect::<Vec<_>>()))));
        // A fade sets only the opacity: the clip stays where it is.
        let fade = all.iter().find(|p| p.name == "Fade out").unwrap();
        let mut v = ClipVisual { transform: Transform::default(), blend: BlendMode::Normal, track_matte: None, anim: TransformAnim::default(), effects: Vec::new(), masks: Vec::new(), mesh: None };
        v.transform.position = [50.0, 20.0];
        v.transform.scale = [0.5, 0.5];
        fade.apply(&mut v, secs(2.0), secs(6.0), [100.0, 100.0], false);
        assert_eq!((v.transform.position, v.transform.scale), ([50.0, 20.0], [0.5, 0.5]));
        assert_eq!(v.anim.opacity.0.iter().map(|k| k.at).collect::<Vec<_>>(), [secs(7.0), secs(8.0)], "over the last second");
        assert!(v.anim.position.is_empty());
    }

    #[test]
    fn titles_audio_and_transitions_go_in_when_chosen() {
        let v = ClipVisual { transform: Transform::default(), blend: BlendMode::Normal, track_matte: None, anim: TransformAnim::default(), effects: Vec::new(), masks: Vec::new(), mesh: None };
        let mut audio = ClipAudio { volume_db: -6.0, fade_in: secs(0.5), ..ClipAudio::default() };
        audio.volume_keys.push(reelforge_core::VolumeKey { at: secs(11.0), db: -20.0 });
        let parts = ClipParts {
            title: Some(TitleSpec { text: "Hello".into(), size: 90.0, ..TitleSpec::default() }),
            audio: Some(audio),
            transition: Some(TransitionPreset { kind: Transition::default(), length: secs(0.5) }),
        };
        let opts = SaveOptions { effects: Some(vec![]), title: true, audio: true, transition: true, folder: "Lower thirds".into(), ..SaveOptions::default() };
        let p = EffectPreset::from_clip_parts("Big title", &v, &parts, secs(10.0), secs(4.0), [100.0, 100.0], &opts).unwrap();
        assert!(!p.has_visual());
        let t = p.title.as_ref().unwrap();
        assert_eq!((t.text.as_str(), t.size), ("", 90.0), "the style, not the words");
        assert_eq!(p.audio.as_ref().unwrap().volume_keys[0].at, secs(1.0));
        assert_eq!(p.folder, "Lower thirds");
        let a = p.audio_on(secs(3.0), secs(0.25)).unwrap();
        assert_eq!((a.volume_keys[0].at, a.fade_in), (secs(4.0), secs(0.25)), "keys placed; fades fit the clip");
        assert!(p.summary().contains("title style, audio levels"), "{}", p.summary());
        // None of it chosen: nothing to save.
        let none = SaveOptions { effects: Some(vec![]), ..SaveOptions::default() };
        assert!(EffectPreset::from_clip_parts("x", &v, &parts, secs(10.0), secs(4.0), [100.0, 100.0], &none).is_none());
    }

    #[test]
    fn the_library_saves_renames_and_removes() {
        let dir = std::env::temp_dir().join(format!("rf_fxpresets_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("effect-presets.json");
        let mut lib = PresetLibrary::load(&file);
        assert!(lib.list.is_empty());
        let p = |n: &str, k| EffectPreset::from_clip(n, &[Effect::new(k)], Time::ZERO).unwrap();
        lib.put(p("Warm", EffectKind::ColorCorrect)).unwrap();
        lib.put(p("blurry", EffectKind::Blur)).unwrap();
        // Same name: replaced.
        lib.put(p("warm", EffectKind::Glow)).unwrap();
        let lib2 = PresetLibrary::load(&file);
        assert_eq!(lib2.list.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["blurry", "warm"]);
        assert_eq!(lib2.get("WARM").unwrap().effects[0].kind, EffectKind::Glow);
        // The starter looks are there too, and can't be renamed, deleted or replaced.
        assert!(lib2.get("Film look").is_some_and(|p| p.builtin && p.folder == STARTERS));
        let mut lib = lib2;
        assert!(lib.rename("blurry", "Warm").is_err());
        lib.rename("blurry", "Dreamy").unwrap();
        lib.remove("warm").unwrap();
        let mut lib3 = PresetLibrary::load(&file);
        assert_eq!(lib3.list.len(), 1);
        assert_eq!(lib3.list[0].name, "Dreamy");
        assert!(lib3.rename("Film look", "Mine").is_err() && lib3.remove("Film look").is_err());
        assert!(lib3.put(EffectPreset::from_clip("film LOOK", &[Effect::new(EffectKind::Blur)], Time::ZERO).unwrap()).is_err());
        // Favourites (yours and the starters') and folders are kept.
        lib3.set_favourite("Dreamy", true).unwrap();
        lib3.set_favourite("Film look", true).unwrap();
        lib3.set_folder("Dreamy", " Looks ").unwrap();
        let lib4 = PresetLibrary::load(&file);
        assert!(lib4.get("dreamy").is_some_and(|p| p.favourite && p.folder == "Looks"));
        assert!(lib4.get("Film look").unwrap().favourite);
        assert_eq!(lib4.folders(), ["Looks"]);
        // Export and import: a taken name gets a number; starters come in as yours.
        let text = lib4.export(&["Dreamy".into(), "Film look".into()]);
        let mut lib5 = PresetLibrary::load(&file);
        assert_eq!(lib5.import(&text).unwrap(), ["Dreamy (2)", "Film look (2)"]);
        let fl = lib5.get("Film look (2)").unwrap();
        assert!(!fl.builtin && fl.folder.is_empty() && !fl.favourite && fl.effects.len() == 3);
        assert!(lib5.import("{\"x\":1}").is_err());
        // Files from before favourites (a plain list) still load.
        std::fs::write(&file, serde_json::to_string(&vec![EffectPreset::from_clip("Old", &[Effect::new(EffectKind::Blur)], Time::ZERO).unwrap()]).unwrap()).unwrap();
        assert_eq!(PresetLibrary::load(&file).list[0].name, "Old");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
