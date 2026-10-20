//! Every keyframeable number on a video clip, addressed uniformly, for the
//! curve editor: the transform components (position X/Y, scale X/Y, rotation,
//! opacity) and each effect parameter.
//!
//! Position and scale are stored as one curve of `[x, y]` keys, so a key on
//! "Position X" is the same key as on "Position Y": moving it in time moves
//! both, while changing its value only changes that component.

use crate::anim::{Curve, Interp, Key, Prop};
use crate::project::ClipVisual;
use crate::time::Time;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Channel {
    /// A transform property and component (0 = x, 1 = y; 0 for scalars).
    Transform(Prop, u8),
    /// Effect index in the stack, parameter index.
    Effect(usize, usize),
}

enum Ref<'a> {
    Scalar(&'a Curve<f32>, f32),
    Pair(&'a Curve<[f32; 2]>, usize, f32),
    Quad(&'a Curve<[f32; 4]>, usize, f32),
}

enum Mut<'a> {
    Scalar(&'a mut Curve<f32>),
    Pair(&'a mut Curve<[f32; 2]>, usize),
    Quad(&'a mut Curve<[f32; 4]>, usize),
}

const CROP_SIDES: [&str; 4] = ["left", "top", "right", "bottom"];

/// Keys of a channel as plain `(time, value, interpolation)`.
pub type ChannelKey = (Time, f32, Interp);

impl ClipVisual {
    /// All channels this clip has, in display order.
    pub fn channels(&self) -> Vec<Channel> {
        let mut v = vec![
            Channel::Transform(Prop::Position, 0),
            Channel::Transform(Prop::Position, 1),
            Channel::Transform(Prop::Scale, 0),
            Channel::Transform(Prop::Scale, 1),
            Channel::Transform(Prop::Rotation, 0),
            Channel::Transform(Prop::Opacity, 0),
            Channel::Transform(Prop::Anchor, 0),
            Channel::Transform(Prop::Anchor, 1),
            Channel::Transform(Prop::Crop, 0),
            Channel::Transform(Prop::Crop, 1),
            Channel::Transform(Prop::Crop, 2),
            Channel::Transform(Prop::Crop, 3),
        ];
        if self.transform.three_d {
            v.splice(5..5, [Channel::Transform(Prop::Depth, 0), Channel::Transform(Prop::Orientation, 0), Channel::Transform(Prop::Orientation, 1)]);
        }
        for (i, e) in self.effects.iter().enumerate() {
            // Colours have no curve to draw.
            v.extend((0..e.param_specs().len()).filter(|&p| e.spec(p).is_some_and(|s| s.ui != crate::effects::ParamUi::Color)).map(|p| Channel::Effect(i, p)));
        }
        v
    }

    pub fn channel_name(&self, ch: Channel) -> String {
        match ch {
            Channel::Transform(Prop::Position, c) => format!("Position {}", ["X", "Y"][c as usize & 1]),
            Channel::Transform(Prop::Scale, c) => format!("Scale {}", ["X", "Y"][c as usize & 1]),
            Channel::Transform(Prop::Rotation, _) => "Rotation".into(),
            Channel::Transform(Prop::Opacity, _) => "Opacity".into(),
            Channel::Transform(Prop::Anchor, c) => format!("Anchor {}", ["X", "Y"][c as usize & 1]),
            Channel::Transform(Prop::Crop, c) => format!("Crop {}", CROP_SIDES[c as usize & 3]),
            Channel::Transform(Prop::Pin, _) => "Corner pin".into(),
            Channel::Transform(Prop::Depth, _) => "Position Z".into(),
            Channel::Transform(Prop::Orientation, c) => format!("Rotation {}", ["X", "Y"][c as usize & 1]),
            Channel::Effect(e, p) => match self.effects.get(e) {
                Some(fx) => format!("{}: {}", fx.label(), fx.spec(p).map_or("?", |s| s.name)),
                None => "?".into(),
            },
        }
    }

    /// Display multiplier and unit: opacity and scale are stored as fractions but shown as %.
    pub fn channel_display(&self, ch: Channel) -> (f32, &'static str) {
        match ch {
            Channel::Transform(Prop::Scale | Prop::Opacity | Prop::Crop, _) => (100.0, " %"),
            Channel::Transform(Prop::Rotation | Prop::Orientation, _) => (1.0, "°"),
            Channel::Transform(Prop::Position | Prop::Anchor | Prop::Pin | Prop::Depth, _) => (1.0, " px"),
            Channel::Effect(e, p) => (1.0, self.effects.get(e).and_then(|fx| fx.spec(p)).map_or("", |s| s.suffix)),
        }
    }

    /// Allowed value range (stored units), if the channel has one.
    pub fn channel_range(&self, ch: Channel) -> Option<(f32, f32)> {
        match ch {
            Channel::Transform(Prop::Opacity | Prop::Crop, _) => Some((0.0, 1.0)),
            Channel::Effect(e, p) => self.effects.get(e).and_then(|fx| fx.spec(p)).map(|s| (s.min, s.max)),
            _ => None,
        }
    }

    fn get(&self, ch: Channel) -> Option<Ref<'_>> {
        let t = &self.transform;
        Some(match ch {
            Channel::Transform(Prop::Position, c) => Ref::Pair(&self.anim.position, c as usize & 1, t.position[c as usize & 1]),
            Channel::Transform(Prop::Scale, c) => Ref::Pair(&self.anim.scale, c as usize & 1, t.scale[c as usize & 1]),
            Channel::Transform(Prop::Rotation, _) => Ref::Scalar(&self.anim.rotation, t.rotation),
            Channel::Transform(Prop::Opacity, _) => Ref::Scalar(&self.anim.opacity, t.opacity),
            Channel::Transform(Prop::Anchor, c) => Ref::Pair(&self.anim.anchor, c as usize & 1, t.anchor[c as usize & 1]),
            Channel::Transform(Prop::Crop, c) => Ref::Quad(&self.anim.crop, c as usize & 3, t.crop[c as usize & 3]),
            // Corner pins are edited in the Viewer, not as curves.
            Channel::Transform(Prop::Pin, _) => return None,
            Channel::Transform(Prop::Depth, _) => Ref::Scalar(&self.anim.depth, t.depth),
            Channel::Transform(Prop::Orientation, c) => Ref::Pair(&self.anim.orientation, c as usize & 1, t.orientation[c as usize & 1]),
            Channel::Effect(e, p) => {
                let param = self.effects.get(e)?.params.get(p)?;
                Ref::Scalar(&param.keys, param.value)
            }
        })
    }

    fn get_mut(&mut self, ch: Channel) -> Option<Mut<'_>> {
        Some(match ch {
            Channel::Transform(Prop::Position, c) => Mut::Pair(&mut self.anim.position, c as usize & 1),
            Channel::Transform(Prop::Scale, c) => Mut::Pair(&mut self.anim.scale, c as usize & 1),
            Channel::Transform(Prop::Rotation, _) => Mut::Scalar(&mut self.anim.rotation),
            Channel::Transform(Prop::Opacity, _) => Mut::Scalar(&mut self.anim.opacity),
            Channel::Transform(Prop::Anchor, c) => Mut::Pair(&mut self.anim.anchor, c as usize & 1),
            Channel::Transform(Prop::Crop, c) => Mut::Quad(&mut self.anim.crop, c as usize & 3),
            Channel::Transform(Prop::Pin, _) => return None,
            Channel::Transform(Prop::Depth, _) => Mut::Scalar(&mut self.anim.depth),
            Channel::Transform(Prop::Orientation, c) => Mut::Pair(&mut self.anim.orientation, c as usize & 1),
            Channel::Effect(e, p) => Mut::Scalar(&mut self.effects.get_mut(e)?.params.get_mut(p)?.keys),
        })
    }

    pub fn channel_animated(&self, ch: Channel) -> bool {
        match self.get(ch) {
            Some(Ref::Scalar(c, _)) => c.is_animated(),
            Some(Ref::Pair(c, ..)) => c.is_animated(),
            Some(Ref::Quad(c, ..)) => c.is_animated(),
            None => false,
        }
    }

    pub fn channel_keys(&self, ch: Channel) -> Vec<ChannelKey> {
        match self.get(ch) {
            Some(Ref::Scalar(c, _)) => c.0.iter().map(|k| (k.at, k.value, k.interp)).collect(),
            Some(Ref::Pair(c, i, _)) => c.0.iter().map(|k| (k.at, k.value[i], k.interp)).collect(),
            Some(Ref::Quad(c, i, _)) => c.0.iter().map(|k| (k.at, k.value[i], k.interp)).collect(),
            None => Vec::new(),
        }
    }

    /// Value at source time `t` (not clamped).
    pub fn channel_value(&self, ch: Channel, t: Time) -> f32 {
        match self.get(ch) {
            Some(Ref::Scalar(c, base)) => c.value_at(t, base),
            Some(Ref::Pair(c, i, base)) => {
                let mut b = [0.0; 2];
                b[i] = base;
                c.value_at(t, b)[i]
            }
            Some(Ref::Quad(c, i, base)) => {
                let mut b = [0.0; 4];
                b[i] = base;
                c.value_at(t, b)[i]
            }
            None => 0.0,
        }
    }

    /// Move key `index` to time `at` and set its value. Neighbouring keys bound
    /// the move (it keeps at least `gap` from them), so the order never
    /// changes. Returns the time actually used.
    pub fn channel_set_key(&mut self, ch: Channel, index: usize, at: Time, value: f32, gap: Time) -> Time {
        let value = match self.channel_range(ch) {
            Some((lo, hi)) => value.clamp(lo, hi),
            None => value,
        };
        fn place<T>(keys: &mut [Key<T>], index: usize, at: Time, gap: Time) -> Option<Time> {
            let lo = if index > 0 { keys[index - 1].at + gap } else { Time(i64::MIN / 4) };
            let hi = keys.get(index + 1).map_or(Time(i64::MAX / 4), |k| k.at - gap);
            let k = keys.get_mut(index)?;
            k.at = at.max(lo).min(hi.max(lo));
            Some(k.at)
        }
        match self.get_mut(ch) {
            Some(Mut::Scalar(c)) => {
                let t = place(&mut c.0, index, at, gap).unwrap_or(at);
                if let Some(k) = c.0.get_mut(index) {
                    k.value = value;
                }
                t
            }
            Some(Mut::Pair(c, i)) => {
                let t = place(&mut c.0, index, at, gap).unwrap_or(at);
                if let Some(k) = c.0.get_mut(index) {
                    k.value[i] = value;
                }
                t
            }
            Some(Mut::Quad(c, i)) => {
                let t = place(&mut c.0, index, at, gap).unwrap_or(at);
                if let Some(k) = c.0.get_mut(index) {
                    k.value[i] = value;
                }
                t
            }
            None => at,
        }
    }

    pub fn channel_set_interp(&mut self, ch: Channel, index: usize, interp: Interp) {
        match self.get_mut(ch) {
            Some(Mut::Scalar(c)) => c.0.get_mut(index).into_iter().for_each(|k| k.interp = interp),
            Some(Mut::Pair(c, _)) => c.0.get_mut(index).into_iter().for_each(|k| k.interp = interp),
            Some(Mut::Quad(c, _)) => c.0.get_mut(index).into_iter().for_each(|k| k.interp = interp),
            None => {}
        }
    }

    /// Delete key `index` (for position/scale, that removes the key for both components).
    pub fn channel_remove_key(&mut self, ch: Channel, index: usize) {
        match self.get_mut(ch) {
            Some(Mut::Scalar(c)) if index < c.0.len() => {
                c.0.remove(index);
            }
            Some(Mut::Pair(c, _)) if index < c.0.len() => {
                c.0.remove(index);
            }
            Some(Mut::Quad(c, _)) if index < c.0.len() => {
                c.0.remove(index);
            }
            _ => {}
        }
    }

    /// Add a key at `at` holding the channel's current value there (for
    /// position/scale, both components keep their current values).
    /// Returns its index.
    pub fn channel_add_key(&mut self, ch: Channel, at: Time) -> Option<usize> {
        let base = match self.get(ch)? {
            Ref::Scalar(c, base) => KeyBase::Scalar(c.value_at(at, base)),
            Ref::Pair(c, _, _) => {
                let b = match ch {
                    Channel::Transform(Prop::Scale, _) => self.transform.scale,
                    Channel::Transform(Prop::Anchor, _) => self.transform.anchor,
                    _ => self.transform.position,
                };
                KeyBase::Pair(c.value_at(at, b))
            }
            Ref::Quad(c, _, _) => KeyBase::Quad(c.value_at(at, self.transform.crop)),
        };
        match (self.get_mut(ch)?, base) {
            (Mut::Scalar(c), KeyBase::Scalar(v)) => {
                c.set(at, v);
                c.index_at(at)
            }
            (Mut::Pair(c, _), KeyBase::Pair(v)) => {
                c.set(at, v);
                c.index_at(at)
            }
            (Mut::Quad(c, _), KeyBase::Quad(v)) => {
                c.set(at, v);
                c.index_at(at)
            }
            _ => None,
        }
    }
}

enum KeyBase {
    Scalar(f32),
    Pair([f32; 2]),
    Quad([f32; 4]),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::{Effect, EffectKind};

    fn s(x: f64) -> Time {
        Time::from_secs_f64(x)
    }

    #[test]
    fn transform_components_share_keys() {
        let mut v = ClipVisual::default();
        v.transform.position = [10.0, 20.0];
        let x = Channel::Transform(Prop::Position, 0);
        let y = Channel::Transform(Prop::Position, 1);
        assert!(!v.channel_animated(x));
        assert_eq!(v.channel_value(y, s(1.0)), 20.0, "static value without keys");
        v.channel_add_key(x, s(0.0));
        v.channel_add_key(x, s(2.0));
        assert_eq!(v.channel_keys(y).len(), 2, "a position key covers both components");
        v.channel_set_key(y, 1, s(2.0), 120.0, Time(1));
        assert_eq!(v.channel_value(y, s(1.0)), 70.0);
        assert_eq!(v.channel_value(x, s(1.0)), 10.0, "x untouched");
    }

    #[test]
    fn anchor_and_crop_are_channels() {
        let mut v = ClipVisual::default();
        v.transform.crop = [0.1, 0.2, 0.3, 0.4];
        let right = Channel::Transform(Prop::Crop, 2);
        assert_eq!(v.channel_name(right), "Crop right");
        assert_eq!(v.channel_value(right, s(0.0)), 0.3);
        let i = v.channel_add_key(right, s(1.0)).unwrap();
        v.channel_set_key(right, i, s(1.0), 2.0, Time(1));
        assert_eq!(v.anim.crop.0[0].value, [0.1, 0.2, 1.0, 0.4], "clamped; other sides kept");
        let ax = Channel::Transform(Prop::Anchor, 0);
        v.channel_add_key(ax, s(0.0));
        v.channel_set_key(ax, 0, s(0.0), 25.0, Time(1));
        assert_eq!(v.anim.eval(&v.transform, s(0.0)).anchor, [25.0, 0.0]);
    }

    #[test]
    fn keys_stay_in_order_and_ranges_hold() {
        let mut v = ClipVisual::default();
        let op = Channel::Transform(Prop::Opacity, 0);
        for t in [0.0, 1.0, 2.0] {
            v.channel_add_key(op, s(t));
        }
        let gap = s(0.1);
        let t = v.channel_set_key(op, 1, s(5.0), 3.0, gap);
        assert_eq!(t, s(1.9), "stops short of the next key");
        assert_eq!(v.channel_keys(op)[1].1, 1.0, "opacity clamped to 100%");
        v.channel_set_interp(op, 0, Interp::Hold);
        assert_eq!(v.channel_keys(op)[0].2, Interp::Hold);
        v.channel_remove_key(op, 1);
        assert_eq!(v.channel_keys(op).len(), 2);
    }

    #[test]
    fn effect_params_are_channels() {
        let mut v = ClipVisual::default();
        v.effects.push(Effect::new(EffectKind::Blur));
        let ch = Channel::Effect(0, 0);
        assert!(v.channels().contains(&ch));
        assert_eq!(v.channel_name(ch), "Gaussian blur: Radius");
        let i = v.channel_add_key(ch, s(1.0)).unwrap();
        v.channel_set_key(ch, i, s(1.0), 999.0, Time(1));
        assert_eq!(v.channel_keys(ch)[0].1, 250.0, "clamped to the parameter's range");
        assert_eq!(v.effects[0].value_at(0, s(3.0)), 250.0);
    }
}
