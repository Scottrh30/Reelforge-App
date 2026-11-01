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
use reelforge_core::{BlendMode, ClipVisual, Time, Transform};
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
        let v = ClipVisual { transform: Transform::default(), blend: BlendMode::Normal, anim: TransformAnim::default(), effects: effects.to_vec(), masks: Vec::new(), mesh: None };
        Self::from_visual(name, &v, source_in, Time::ZERO, [0.0, 0.0], &SaveOptions::default())
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
            Motion { transform, anim: retime_motion(&v.anim, &rel) }
        });
        let blend = opts.blend.then_some(v.blend);
        if effects.is_empty() && masks.is_empty() && motion.is_none() && blend.is_none() {
            return None;
        }
        Some(Self { name: name.trim().to_string(), effects: retime_effects(&effects, &rel), masks, layer, motion, blend, timing: opts.timing, length })
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
        v.effects.extend(retime_effects(&self.effects, &f));
        if !self.masks.is_empty() {
            let mut masks = retime_masks(&self.masks, &f);
            scale_masks(&mut masks, self.layer, layer);
            if replace {
                v.masks.clear();
            }
            v.masks.extend(masks);
        }
        if let Some(m) = &self.motion {
            for p in MOTION_PROPS {
                v.transform.copy_prop(p, &m.transform);
            }
            v.transform.three_d = m.transform.three_d;
            v.transform.lit = m.transform.lit;
            let a = retime_motion(&m.anim, &f);
            (v.anim.position, v.anim.scale, v.anim.rotation, v.anim.opacity, v.anim.anchor, v.anim.depth, v.anim.orientation) = (a.position, a.scale, a.rotation, a.opacity, a.anchor, a.depth, a.orientation);
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
        fx + masks + motion
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
        if self.motion.is_some() {
            extra.push("motion".into());
        }
        if let Some(b) = self.blend {
            extra.push(format!("{} blend", b.label()));
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

/// The saved presets, in one file.
#[derive(Debug)]
pub struct PresetLibrary {
    file: PathBuf,
    pub list: Vec<EffectPreset>,
}

fn same(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

impl PresetLibrary {
    pub fn load(file: &Path) -> Self {
        let list = std::fs::read_to_string(file).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        Self { file: file.to_path_buf(), list }
    }

    fn save(&self) -> std::io::Result<()> {
        if let Some(d) = self.file.parent() {
            std::fs::create_dir_all(d)?;
        }
        let tmp = self.file.with_extension("tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(&self.list).unwrap_or_default())?;
        std::fs::rename(tmp, &self.file)
    }

    pub fn get(&self, name: &str) -> Option<&EffectPreset> {
        self.list.iter().find(|p| same(&p.name, name))
    }

    /// Add `p`, replacing one of the same name (case aside). Kept in name order.
    pub fn put(&mut self, p: EffectPreset) -> std::io::Result<()> {
        if p.name.is_empty() {
            return Err(std::io::Error::other("a preset needs a name"));
        }
        self.list.retain(|q| !same(&q.name, &p.name));
        self.list.push(p);
        self.list.sort_by_key(|p| p.name.to_lowercase());
        self.save()
    }

    pub fn rename(&mut self, from: &str, to: &str) -> std::io::Result<()> {
        let to = to.trim();
        if to.is_empty() {
            return Err(std::io::Error::other("a preset needs a name"));
        }
        if !same(from, to) && self.get(to).is_some() {
            return Err(std::io::Error::other(format!("there's already a preset called \"{to}\"")));
        }
        let Some(p) = self.list.iter_mut().find(|p| same(&p.name, from)) else { return Err(std::io::Error::other("no such preset")) };
        p.name = to.to_string();
        self.list.sort_by_key(|p| p.name.to_lowercase());
        self.save()
    }

    pub fn remove(&mut self, name: &str) -> std::io::Result<()> {
        self.list.retain(|p| !same(&p.name, name));
        self.save()
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
        let mut v = ClipVisual { transform: Transform::default(), blend: BlendMode::Screen, anim: TransformAnim::default(), effects: vec![keyed(10.0), Effect::new(EffectKind::Glow)], masks: Vec::new(), mesh: None };
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
        let all = SaveOptions { effects: Some(vec![0]), masks: true, motion: true, blend: true, timing: Timing::FromStart };
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
        let mut t = ClipVisual { transform: Transform::default(), blend: BlendMode::Normal, anim: TransformAnim::default(), effects: vec![Effect::new(EffectKind::Vignette)], masks: Vec::new(), mesh: None };
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
        let blank = || ClipVisual { transform: Transform::default(), blend: BlendMode::Normal, anim: TransformAnim::default(), effects: Vec::new(), masks: Vec::new(), mesh: None };
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
        let mut lib = lib2;
        assert!(lib.rename("blurry", "Warm").is_err());
        lib.rename("blurry", "Dreamy").unwrap();
        lib.remove("warm").unwrap();
        let lib3 = PresetLibrary::load(&file);
        assert_eq!(lib3.list.len(), 1);
        assert_eq!(lib3.list[0].name, "Dreamy");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
