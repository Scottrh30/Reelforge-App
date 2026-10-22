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
    /// Shaped by this key's out-handle and the next key's in-handle
    /// (see [`Key::ease_out`]).
    Bezier,
}

/// Where a handle sits when it hasn't been dragged: a gentle ease, close to
/// [`Interp::Ease`].
pub const DEFAULT_HANDLE: [f32; 2] = [1.0 / 3.0, 0.0];

impl Interp {
    pub const ALL: [Interp; 4] = [Interp::Linear, Interp::Ease, Interp::Hold, Interp::Bezier];
    pub fn label(self) -> &'static str {
        match self {
            Interp::Linear => "Linear",
            Interp::Ease => "Ease",
            Interp::Hold => "Hold",
            Interp::Bezier => "Bézier",
        }
    }
    fn shape(self, u: f32) -> f32 {
        match self {
            Interp::Linear => u,
            Interp::Ease => u * u * (3.0 - 2.0 * u),
            Interp::Hold => 0.0,
            Interp::Bezier => bezier_progress(DEFAULT_HANDLE, DEFAULT_HANDLE, u),
        }
    }
}

/// Progress (0 at the first key, 1 at the next; it may overshoot) at time
/// fraction `u` of a Bézier segment. `out` is the first key's handle and
/// `inn` the next key's, each as (fraction of the segment's time, fraction of
/// its change in value) measured from its own key, so a key's handles stay put
/// relative to it when keys move. Time fractions are kept within 0–1, so the
/// curve never goes back in time.
pub fn bezier_progress(out: [f32; 2], inn: [f32; 2], u: f32) -> f32 {
    let u = u.clamp(0.0, 1.0) as f64;
    let (x1, y1) = (out[0].clamp(0.0, 1.0) as f64, out[1] as f64);
    let (x2, y2) = (1.0 - inn[0].clamp(0.0, 1.0) as f64, 1.0 - inn[1] as f64);
    let bez = |a: f64, b: f64, s: f64| {
        let r = 1.0 - s;
        3.0 * r * r * s * a + 3.0 * r * s * s * b + s * s * s
    };
    // x(s) rises from 0 to 1: solve x(s) = u by bisection (exact enough, never fails).
    let (mut lo, mut hi) = (0.0f64, 1.0f64);
    for _ in 0..40 {
        let mid = 0.5 * (lo + hi);
        if bez(x1, x2, mid) < u {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    bez(y1, y2, 0.5 * (lo + hi)) as f32
}

pub trait Lerp: Copy + PartialEq {
    fn lerp(a: Self, b: Self, t: f32) -> Self;
}

impl Lerp for f32 {
    fn lerp(a: f32, b: f32, t: f32) -> f32 {
        a + (b - a) * t
    }
}

impl Lerp for [f32; 3] {
    fn lerp(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
        [f32::lerp(a[0], b[0], t), f32::lerp(a[1], b[1], t), f32::lerp(a[2], b[2], t)]
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
    /// Bézier handle leaving this key (used when `interp` is
    /// [`Interp::Bezier`]): (fraction of the time to the next key, fraction of
    /// the change in value). `None` is [`DEFAULT_HANDLE`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ease_out: Option<[f32; 2]>,
    /// Bézier handle arriving at this key from the previous one (used when
    /// that key is [`Interp::Bezier`]), measured back from this key the same way.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ease_in: Option<[f32; 2]>,
}

impl<T> Key<T> {
    pub fn new(at: Time, value: T, interp: Interp) -> Self {
        Key { at, value, interp, ease_out: None, ease_in: None }
    }
    pub fn out_handle(&self) -> [f32; 2] {
        self.ease_out.unwrap_or(DEFAULT_HANDLE)
    }
    pub fn in_handle(&self) -> [f32; 2] {
        self.ease_in.unwrap_or(DEFAULT_HANDLE)
    }
}

/// How far (0–1, may overshoot) a segment from key `a` to key `b` has got at time fraction `u`.
pub fn segment_progress<T>(a: &Key<T>, b: &Key<T>, u: f32) -> f32 {
    match a.interp {
        Interp::Bezier => bezier_progress(a.out_handle(), b.in_handle(), u),
        i => i.shape(u),
    }
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
        T::lerp(a.value, b.value, segment_progress(a, b, u))
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
                self.0.insert(i, Key::new(t, value, interp));
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
    Anchor,
    Crop,
    /// Corner pin (see [`crate::transform::Pin`]).
    Pin,
    /// Z position of a 3D layer.
    Depth,
    /// X and Y rotation of a 3D layer.
    Orientation,
}

impl Prop {
    pub const ALL: [Prop; 9] = [Prop::Position, Prop::Scale, Prop::Rotation, Prop::Opacity, Prop::Anchor, Prop::Crop, Prop::Pin, Prop::Depth, Prop::Orientation];
}

impl Lerp for crate::transform::Pin {
    fn lerp(a: Self, b: Self, t: f32) -> Self {
        crate::transform::Pin(std::array::from_fn(|i| <[f32; 2]>::lerp(a.0[i], b.0[i], t)))
    }
}

impl Lerp for [f32; 4] {
    fn lerp(a: [f32; 4], b: [f32; 4], t: f32) -> [f32; 4] {
        std::array::from_fn(|i| f32::lerp(a[i], b[i], t))
    }
}

impl Transform {
    /// Copy property `p` from `from`.
    pub fn copy_prop(&mut self, p: Prop, from: &Transform) {
        match p {
            Prop::Position => self.position = from.position,
            Prop::Scale => self.scale = from.scale,
            Prop::Rotation => self.rotation = from.rotation,
            Prop::Opacity => self.opacity = from.opacity,
            Prop::Anchor => self.anchor = from.anchor,
            Prop::Crop => self.crop = from.crop,
            Prop::Pin => self.pin = from.pin,
            Prop::Depth => self.depth = from.depth,
            Prop::Orientation => self.orientation = from.orientation,
        }
    }
    /// Does property `p` differ between the two?
    pub fn prop_differs(&self, p: Prop, other: &Transform) -> bool {
        match p {
            Prop::Position => self.position != other.position,
            Prop::Scale => self.scale != other.scale,
            Prop::Rotation => self.rotation != other.rotation,
            Prop::Opacity => self.opacity != other.opacity,
            Prop::Anchor => self.anchor != other.anchor,
            Prop::Crop => self.crop != other.crop,
            Prop::Pin => self.pin != other.pin,
            Prop::Depth => self.depth != other.depth,
            Prop::Orientation => self.orientation != other.orientation,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TransformAnim {
    pub position: Curve<[f32; 2]>,
    pub scale: Curve<[f32; 2]>,
    pub rotation: Curve<f32>,
    pub opacity: Curve<f32>,
    #[serde(skip_serializing_if = "Curve::is_empty")]
    pub anchor: Curve<[f32; 2]>,
    #[serde(skip_serializing_if = "Curve::is_empty")]
    pub crop: Curve<[f32; 4]>,
    #[serde(skip_serializing_if = "Curve::is_empty")]
    pub pin: Curve<crate::transform::Pin>,
    #[serde(skip_serializing_if = "Curve::is_empty")]
    pub depth: Curve<f32>,
    #[serde(skip_serializing_if = "Curve::is_empty")]
    pub orientation: Curve<[f32; 2]>,
}

impl<T> Curve<T> {
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Key `index`'s (in, out) Bézier handles.
    pub fn handles(&self) -> Vec<([f32; 2], [f32; 2])> {
        self.0.iter().map(|k| (k.in_handle(), k.out_handle())).collect()
    }

    /// Set key `index`'s out-handle (`out`) or in-handle. The time part is
    /// kept within 0–1; dragging a handle makes the segment it shapes Bézier.
    pub fn set_handle(&mut self, index: usize, out: bool, h: [f32; 2]) {
        let h = [h[0].clamp(0.0, 1.0), if h[1].is_finite() { h[1].clamp(-10.0, 10.0) } else { 0.0 }];
        if out {
            if let Some(k) = self.0.get_mut(index) {
                k.ease_out = Some(h);
                k.interp = Interp::Bezier;
            }
        } else if index > 0 && index < self.0.len() {
            self.0[index].ease_in = Some(h);
            self.0[index - 1].interp = Interp::Bezier;
        }
    }
}

impl TransformAnim {
    pub fn is_animated(&self) -> bool {
        Prop::ALL.iter().any(|p| self.prop_animated(*p))
    }

    pub fn prop_animated(&self, p: Prop) -> bool {
        self.key_count(p) > 0
    }

    /// How many keys property `p` has.
    pub fn key_count(&self, p: Prop) -> usize {
        match p {
            Prop::Position => self.position.0.len(),
            Prop::Scale => self.scale.0.len(),
            Prop::Rotation => self.rotation.0.len(),
            Prop::Opacity => self.opacity.0.len(),
            Prop::Anchor => self.anchor.0.len(),
            Prop::Crop => self.crop.0.len(),
            Prop::Pin => self.pin.0.len(),
            Prop::Depth => self.depth.0.len(),
            Prop::Orientation => self.orientation.0.len(),
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
            anchor: self.anchor.value_at(t, base.anchor),
            crop: self.crop.value_at(t, base.crop),
            // Keys make it pinned; without them, the clip's own setting.
            pin: match self.pin.0.first() {
                Some(k) => Some(self.pin.value_at(t, k.value)),
                None => base.pin,
            },
            three_d: base.three_d,
            depth: self.depth.value_at(t, base.depth),
            orientation: self.orientation.value_at(t, base.orientation),
            lit: base.lit,
        }
    }

    /// Key `p` at `t` with that property's value from `v`.
    pub fn set_key(&mut self, p: Prop, t: Time, v: &Transform) {
        match p {
            Prop::Position => self.position.set(t, v.position),
            Prop::Scale => self.scale.set(t, v.scale),
            Prop::Rotation => self.rotation.set(t, v.rotation),
            Prop::Opacity => self.opacity.set(t, v.opacity),
            Prop::Anchor => self.anchor.set(t, v.anchor),
            Prop::Crop => self.crop.set(t, v.crop),
            Prop::Pin => {
                if let Some(p) = v.pin {
                    self.pin.set(t, p)
                }
            }
            Prop::Depth => self.depth.set(t, v.depth),
            Prop::Orientation => self.orientation.set(t, v.orientation),
        }
    }

    pub fn has_key(&self, p: Prop, t: Time) -> bool {
        self.key_times_of(p).contains(&t)
    }

    pub fn remove_key(&mut self, p: Prop, t: Time) -> bool {
        match p {
            Prop::Position => self.position.remove_at(t),
            Prop::Scale => self.scale.remove_at(t),
            Prop::Rotation => self.rotation.remove_at(t),
            Prop::Opacity => self.opacity.remove_at(t),
            Prop::Anchor => self.anchor.remove_at(t),
            Prop::Crop => self.crop.remove_at(t),
            Prop::Pin => self.pin.remove_at(t),
            Prop::Depth => self.depth.remove_at(t),
            Prop::Orientation => self.orientation.remove_at(t),
        }
    }

    pub fn clear(&mut self, p: Prop) {
        match p {
            Prop::Position => self.position.0.clear(),
            Prop::Scale => self.scale.0.clear(),
            Prop::Rotation => self.rotation.0.clear(),
            Prop::Opacity => self.opacity.0.clear(),
            Prop::Anchor => self.anchor.0.clear(),
            Prop::Crop => self.crop.0.clear(),
            Prop::Pin => self.pin.0.clear(),
            Prop::Depth => self.depth.0.clear(),
            Prop::Orientation => self.orientation.0.clear(),
        }
    }

    fn key_times_of(&self, p: Prop) -> Vec<Time> {
        match p {
            Prop::Position => self.position.0.iter().map(|k| k.at).collect(),
            Prop::Scale => self.scale.0.iter().map(|k| k.at).collect(),
            Prop::Rotation => self.rotation.0.iter().map(|k| k.at).collect(),
            Prop::Opacity => self.opacity.0.iter().map(|k| k.at).collect(),
            Prop::Anchor => self.anchor.0.iter().map(|k| k.at).collect(),
            Prop::Crop => self.crop.0.iter().map(|k| k.at).collect(),
            Prop::Pin => self.pin.0.iter().map(|k| k.at).collect(),
            Prop::Depth => self.depth.0.iter().map(|k| k.at).collect(),
            Prop::Orientation => self.orientation.0.iter().map(|k| k.at).collect(),
        }
    }

    /// Times of all keys on any property, sorted and de-duplicated.
    pub fn key_times(&self) -> Vec<Time> {
        let mut v: Vec<Time> = Prop::ALL.iter().flat_map(|p| self.key_times_of(*p)).collect();
        v.sort();
        v.dedup();
        v
    }

    fn interp_of(&self, p: Prop, t: Time) -> Option<Interp> {
        fn find<T>(c: &[Key<T>], t: Time) -> Option<Interp> {
            c.iter().find(|k| k.at == t).map(|k| k.interp)
        }
        match p {
            Prop::Position => find(&self.position.0, t),
            Prop::Scale => find(&self.scale.0, t),
            Prop::Rotation => find(&self.rotation.0, t),
            Prop::Opacity => find(&self.opacity.0, t),
            Prop::Anchor => find(&self.anchor.0, t),
            Prop::Crop => find(&self.crop.0, t),
            Prop::Pin => find(&self.pin.0, t),
            Prop::Depth => find(&self.depth.0, t),
            Prop::Orientation => find(&self.orientation.0, t),
        }
    }

    /// Interpolation of the key(s) at `t` (the first found), if any.
    pub fn interp_at(&self, t: Time) -> Option<Interp> {
        Prop::ALL.iter().find_map(|p| self.interp_of(*p, t))
    }

    /// Set the interpolation of every key at `t`.
    pub fn set_interp_at(&mut self, t: Time, interp: Interp) {
        fn set<T>(c: &mut [Key<T>], t: Time, interp: Interp) {
            c.iter_mut().filter(|k| k.at == t).for_each(|k| k.interp = interp);
        }
        set(&mut self.position.0, t, interp);
        set(&mut self.scale.0, t, interp);
        set(&mut self.rotation.0, t, interp);
        set(&mut self.opacity.0, t, interp);
        set(&mut self.anchor.0, t, interp);
        set(&mut self.crop.0, t, interp);
        set(&mut self.pin.0, t, interp);
        set(&mut self.depth.0, t, interp);
        set(&mut self.orientation.0, t, interp);
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
        let base = Transform { position: [10.0, 20.0], scale: [1.0, 1.0], rotation: 5.0, ..Default::default() };
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

#[cfg(test)]
mod bezier_tests {
    use super::*;

    #[test]
    fn bezier_handles_shape_the_timing() {
        let s = Time::from_secs_f64;
        let mut c: Curve<f32> = Curve::default();
        c.set(s(0.0), 0.0);
        c.set(s(1.0), 100.0);
        // Handles lying along the line: straight.
        c.set_handle(0, true, [0.0, 0.0]);
        c.set_handle(1, false, [0.0, 0.0]);
        assert_eq!(c.0[0].interp, Interp::Bezier);
        for k in 0..=10 {
            let u = k as f64 / 10.0;
            assert!((c.value_at(s(u), 0.0) - 100.0 * u as f32).abs() < 0.01, "{u}");
        }
        // The default handles ease in and out like Ease, symmetrically.
        c.0[0].ease_out = None;
        c.0[1].ease_in = None;
        assert!((c.value_at(s(0.5), 0.0) - 50.0).abs() < 0.01);
        assert!(c.value_at(s(0.1), 0.0) < 10.0 && c.value_at(s(0.9), 0.0) > 90.0);
        // A handle pulled above the next key's value overshoots, then settles.
        c.set_handle(0, true, [0.4, 1.8]);
        let peak = (1..20).map(|k| c.value_at(s(k as f64 / 20.0), 0.0)).fold(f32::MIN, f32::max);
        assert!(peak > 100.0, "{peak}");
        assert!((c.value_at(s(1.0), 0.0) - 100.0).abs() < 1e-3);
        // Time fractions are kept within 0–1; in-handles on the first key do nothing.
        c.set_handle(0, true, [3.0, 0.0]);
        assert_eq!(c.0[0].ease_out, Some([1.0, 0.0]));
        c.set_handle(0, false, [0.5, 0.5]);
        assert_eq!(c.0[0].ease_in, None);
        // Handles go with their keys in saved files; old files load as before.
        let back: Curve<f32> = serde_json::from_str(&serde_json::to_string(&c).unwrap()).unwrap();
        assert_eq!(back, c);
        let old: Curve<f32> = serde_json::from_str(r#"[{"at":0,"value":1.0,"interp":"Ease"}]"#).unwrap();
        assert_eq!(old.0[0], Key::new(Time(0), 1.0, Interp::Ease));
    }
}
