//! Keyframe animation for video properties.
//!
//! Each property (position, scale, rotation, opacity) has its own curve, so
//! you can animate one without touching the others, as in HitFilm. Keys are
//! anchored to the clip's *source* time, like volume keys, so they stay with
//! the picture when the clip is moved, trimmed or split. A property with no
//! keys uses the clip's static value.

use crate::time::Time;
use crate::transform::Transform;
use serde::{Deserialize, Serialize};

/// How a key interpolates towards the next one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Interp {
    #[default]
    Linear,
    /// Smooth start and stop (cubic ease in-out).
    Ease,
    /// Jump: keep this value until the next key.
    Hold,
}

impl Interp {
    pub const ALL: [Interp; 3] = [Interp::Linear, Interp::Ease, Interp::Hold];
    pub fn label(self) -> &'static str {
        match self {
            Interp::Linear => "Linear",
            Interp::Ease => "Ease",
            Interp::Hold => "Hold",
        }
    }
    fn shape(self, u: f32) -> f32 {
        match self {
            Interp::Linear => u,
            Interp::Ease => u * u * (3.0 - 2.0 * u),
            Interp::Hold => 0.0,
        }
    }
}

pub trait Lerp: Copy + PartialEq {
    fn lerp(a: Self, b: Self, t: f32) -> Self;
}

impl Lerp for f32 {
    fn lerp(a: f32, b: f32, t: f32) -> f32 {
        a + (b - a) * t
    }
}

impl Lerp for [f32; 2] {
    fn lerp(a: [f32; 2], b: [f32; 2], t: f32) -> [f32; 2] {
        [f32::lerp(a[0], b[0], t), f32::lerp(a[1], b[1], t)]
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Key<T> {
    /// Source time of the clip's media.
    pub at: Time,
    pub value: T,
    #[serde(default)]
    pub interp: Interp,
}

/// Keys for one property, kept sorted by time.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Curve<T>(pub Vec<Key<T>>);

impl<T> Default for Curve<T> {
    fn default() -> Self {
        Self(Vec::new())
    }
}

impl<T: Lerp> Curve<T> {
    pub fn is_animated(&self) -> bool {
        !self.0.is_empty()
    }

    /// Value at source time `t`; `base` when there are no keys.
    pub fn value_at(&self, t: Time, base: T) -> T {
        let keys = &self.0;
        if keys.is_empty() {
            return base;
        }
        let i = keys.partition_point(|k| k.at <= t);
        if i == 0 {
            return keys[0].value;
        }
        if i == keys.len() {
            return keys[i - 1].value;
        }
        let (a, b) = (&keys[i - 1], &keys[i]);
        if b.at == a.at {
            return b.value;
        }
        let u = (t.0 - a.at.0) as f32 / (b.at.0 - a.at.0) as f32;
        T::lerp(a.value, b.value, a.interp.shape(u))
    }

    pub fn index_at(&self, t: Time) -> Option<usize> {
        self.0.iter().position(|k| k.at == t)
    }

    /// Add a key at `t`, or update the one already there (keeping its interpolation).
    pub fn set(&mut self, t: Time, value: T) {
        match self.0.iter_mut().find(|k| k.at == t) {
            Some(k) => k.value = value,
            None => {
                let i = self.0.partition_point(|k| k.at < t);
                // A new key inherits the interpolation of the key before it.
                let interp = if i > 0 { self.0[i - 1].interp } else { Interp::Linear };
                self.0.insert(i, Key { at: t, value, interp });
            }
        }
    }

    pub fn remove_at(&mut self, t: Time) -> bool {
        match self.index_at(t) {
            Some(i) => {
                self.0.remove(i);
                true
            }
            None => false,
        }
    }
}

/// The animatable properties of a video clip.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Prop {
    Position,
    Scale,
    Rotation,
    Opacity,
}

impl Prop {
    pub const ALL: [Prop; 4] = [Prop::Position, Prop::Scale, Prop::Rotation, Prop::Opacity];
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TransformAnim {
    pub position: Curve<[f32; 2]>,
    pub scale: Curve<[f32; 2]>,
    pub rotation: Curve<f32>,
    pub opacity: Curve<f32>,
}

impl TransformAnim {
    pub fn is_animated(&self) -> bool {
        self.position.is_animated() || self.scale.is_animated() || self.rotation.is_animated() || self.opacity.is_animated()
    }

    pub fn prop_animated(&self, p: Prop) -> bool {
        match p {
            Prop::Position => self.position.is_animated(),
            Prop::Scale => self.scale.is_animated(),
            Prop::Rotation => self.rotation.is_animated(),
            Prop::Opacity => self.opacity.is_animated(),
        }
    }

    /// The transform at source time `t`: animated properties from their curves,
    /// the rest from `base`.
    pub fn eval(&self, base: &Transform, t: Time) -> Transform {
        Transform {
            position: self.position.value_at(t, base.position),
            scale: self.scale.value_at(t, base.scale),
            rotation: self.rotation.value_at(t, base.rotation),
            opacity: self.opacity.value_at(t, base.opacity).clamp(0.0, 1.0),
        }
    }

    /// Key `p` at `t` with that property's value from `v`.
    pub fn set_key(&mut self, p: Prop, t: Time, v: &Transform) {
        match p {
            Prop::Position => self.position.set(t, v.position),
            Prop::Scale => self.scale.set(t, v.scale),
            Prop::Rotation => self.rotation.set(t, v.rotation),
            Prop::Opacity => self.opacity.set(t, v.opacity),
        }
    }

    pub fn has_key(&self, p: Prop, t: Time) -> bool {
        match p {
            Prop::Position => self.position.index_at(t).is_some(),
            Prop::Scale => self.scale.index_at(t).is_some(),
            Prop::Rotation => self.rotation.index_at(t).is_some(),
            Prop::Opacity => self.opacity.index_at(t).is_some(),
        }
    }

    pub fn remove_key(&mut self, p: Prop, t: Time) -> bool {
        match p {
            Prop::Position => self.position.remove_at(t),
            Prop::Scale => self.scale.remove_at(t),
            Prop::Rotation => self.rotation.remove_at(t),
            Prop::Opacity => self.opacity.remove_at(t),
        }
    }

    pub fn clear(&mut self, p: Prop) {
        match p {
            Prop::Position => self.position.0.clear(),
            Prop::Scale => self.scale.0.clear(),
            Prop::Rotation => self.rotation.0.clear(),
            Prop::Opacity => self.opacity.0.clear(),
        }
    }

    /// Times of all keys on any property, sorted and de-duplicated.
    pub fn key_times(&self) -> Vec<Time> {
        let mut v: Vec<Time> = self
            .position
            .0
            .iter()
            .map(|k| k.at)
            .chain(self.scale.0.iter().map(|k| k.at))
            .chain(self.rotation.0.iter().map(|k| k.at))
            .chain(self.opacity.0.iter().map(|k| k.at))
            .collect();
        v.sort();
        v.dedup();
        v
    }

    /// Interpolation of the key(s) at `t` (the first found), if any.
    pub fn interp_at(&self, t: Time) -> Option<Interp> {
        let find = |c: &[Key<f32>]| c.iter().find(|k| k.at == t).map(|k| k.interp);
        let find2 = |c: &[Key<[f32; 2]>]| c.iter().find(|k| k.at == t).map(|k| k.interp);
        find2(&self.position.0).or_else(|| find2(&self.scale.0)).or_else(|| find(&self.rotation.0)).or_else(|| find(&self.opacity.0))
    }

    /// Set the interpolation of every key at `t`.
    pub fn set_interp_at(&mut self, t: Time, interp: Interp) {
        for k in self.position.0.iter_mut().chain(self.scale.0.iter_mut()).filter(|k| k.at == t) {
            k.interp = interp;
        }
        for k in self.rotation.0.iter_mut().chain(self.opacity.0.iter_mut()).filter(|k| k.at == t) {
            k.interp = interp;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: f64) -> Time {
        Time::from_secs_f64(s)
    }

    #[test]
    fn linear_ease_and_hold() {
        let mut c: Curve<f32> = Curve::default();
        assert_eq!(c.value_at(t(1.0), 7.0), 7.0, "no keys: base value");
        c.set(t(1.0), 0.0);
        c.set(t(3.0), 100.0);
        assert_eq!(c.value_at(t(0.0), 7.0), 0.0, "held before the first key");
        assert_eq!(c.value_at(t(2.0), 7.0), 50.0);
        assert_eq!(c.value_at(t(9.0), 7.0), 100.0, "held after the last key");
        c.0[0].interp = Interp::Ease;
        assert_eq!(c.value_at(t(2.0), 7.0), 50.0, "ease is symmetric at the midpoint");
        assert!(c.value_at(t(1.2), 7.0) < 10.0 * 0.5, "ease starts slowly");
        c.0[0].interp = Interp::Hold;
        assert_eq!(c.value_at(t(2.9), 7.0), 0.0, "hold keeps the value until the next key");
        assert_eq!(c.value_at(t(3.0), 7.0), 100.0);
    }

    #[test]
    fn new_keys_stay_sorted_and_inherit_interpolation() {
        let mut c: Curve<f32> = Curve::default();
        c.set(t(2.0), 1.0);
        c.0[0].interp = Interp::Ease;
        c.set(t(4.0), 2.0);
        c.set(t(1.0), 0.0);
        let times: Vec<f64> = c.0.iter().map(|k| k.at.as_secs_f64()).collect();
        assert_eq!(times, vec![1.0, 2.0, 4.0]);
        assert_eq!(c.0[2].interp, Interp::Ease, "inherits from the key before");
        assert_eq!(c.0[0].interp, Interp::Linear);
        c.set(t(2.0), 9.0);
        assert_eq!((c.0.len(), c.0[1].value), (3, 9.0), "same time updates");
        assert!(c.remove_at(t(2.0)));
        assert!(!c.remove_at(t(2.0)));
    }

    #[test]
    fn transform_eval_mixes_animated_and_static_properties() {
        let base = Transform { position: [10.0, 20.0], scale: [1.0, 1.0], rotation: 5.0, opacity: 1.0 };
        let mut a = TransformAnim::default();
        let mut v = base;
        v.position = [0.0, 0.0];
        a.set_key(Prop::Position, t(0.0), &v);
        v.position = [100.0, -50.0];
        a.set_key(Prop::Position, t(2.0), &v);
        a.opacity.set(t(0.0), 1.0);
        a.opacity.set(t(2.0), 3.0); // out of range: clamped on evaluation
        let mid = a.eval(&base, t(1.0));
        assert_eq!(mid.position, [50.0, -25.0]);
        assert_eq!(mid.rotation, 5.0, "not animated: static value");
        assert_eq!(a.eval(&base, t(2.0)).opacity, 1.0);
        assert_eq!(a.key_times(), vec![t(0.0), t(2.0)]);
        assert!(a.prop_animated(Prop::Position) && !a.prop_animated(Prop::Rotation));
    }

    #[test]
    fn serialises_compactly() {
        let mut a = TransformAnim::default();
        a.rotation.set(Time(10), 45.0);
        let json = serde_json::to_string(&a).unwrap();
        let back: TransformAnim = serde_json::from_str(&json).unwrap();
        assert_eq!(a, back);
        let empty: TransformAnim = serde_json::from_str("{}").unwrap();
        assert!(!empty.is_animated());
    }
}
