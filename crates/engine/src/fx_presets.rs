//! Saved effect presets: a stack of effects with their settings and
//! keyframes, kept under a name in the app's settings folder and applied to
//! other clips from the effects browser.
//!
//! Keyframes are stored relative to the start of the clip they came from
//! (its source in point), so a fade-in made over a clip's first second lands
//! on the first second of whatever clip the preset goes onto.

use reelforge_core::anim::Curve;
use reelforge_core::effects::{Effect, EffectKind};
use reelforge_core::Time;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EffectPreset {
    pub name: String,
    /// Keyframe times are from the clip's start.
    pub effects: Vec<Effect>,
}

fn shifted<T: Clone>(c: &Curve<T>, by: Time) -> Curve<T> {
    let mut c = c.clone();
    for k in &mut c.0 {
        k.at = k.at + by;
    }
    c
}

fn shift_all(effects: &[Effect], by: Time) -> Vec<Effect> {
    effects
        .iter()
        .map(|e| {
            let mut e = e.clone();
            for p in &mut e.params {
                p.keys = shifted(&p.keys, by);
            }
            e
        })
        .collect()
}

/// A camera's, light's or point's settings belong to that clip, not a preset.
fn savable(e: &Effect) -> bool {
    !matches!(e.kind, EffectKind::Camera | EffectKind::Light | EffectKind::Point)
}

impl EffectPreset {
    /// The effects of a clip whose media starts at `source_in`.
    pub fn from_clip(name: &str, effects: &[Effect], source_in: Time) -> Option<Self> {
        let list: Vec<Effect> = effects.iter().filter(|e| savable(e)).cloned().collect();
        (!list.is_empty()).then(|| Self { name: name.trim().to_string(), effects: shift_all(&list, Time::ZERO - source_in) })
    }

    /// The effects to put on a clip whose media starts at `source_in`.
    pub fn effects_for(&self, source_in: Time) -> Vec<Effect> {
        shift_all(&self.effects, source_in)
    }

    pub fn keyframes(&self) -> usize {
        self.effects.iter().flat_map(|e| &e.params).map(|p| p.keys.0.len()).sum()
    }

    /// "Glow, Blur · 4 keyframes".
    pub fn summary(&self) -> String {
        let names: Vec<String> = self.effects.iter().map(Effect::label).collect();
        let k = self.keyframes();
        match k {
            0 => names.join(", "),
            1 => format!("{} · 1 keyframe", names.join(", ")),
            _ => format!("{} · {k} keyframes", names.join(", ")),
        }
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

    #[test]
    fn the_library_saves_renames_and_removes() {
        let dir = std::env::temp_dir().join(format!("rf_fxpresets_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("effect-presets.json");
        let mut lib = PresetLibrary::load(&file);
        assert!(lib.list.is_empty());
        let p = |n: &str, k| EffectPreset { name: n.into(), effects: vec![Effect::new(k)] };
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
