//! A clip's sound as keyframe channels, like a picture's (see `channels`):
//! its volume, its pan and each audio effect's settings. Used by an expanded
//! audio track's rows (a stopwatch and a diamond on each) and its lane, where
//! the diamonds are picked, dragged, copied, pasted, eased and deleted.
//! Times are the clip's source time, as everywhere keys are stored.

use crate::anim::{Interp, Key};
use crate::audio::{ClipAudio, PanKey, VolumeKey, MAX_DB, MIN_DB};
use crate::time::Time;

/// One of a clip's sound channels.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AudioChannel {
    /// dB.
    Volume,
    /// −1 (left) to 1 (right).
    Pan,
    /// Audio effect index, setting index.
    Effect(usize, usize),
}

/// A key as the channels see it.
trait K {
    fn at(&self) -> Time;
    fn set_at(&mut self, t: Time);
    fn set_val(&mut self, v: f32);
    fn make(at: Time, v: f32) -> Self;
}

impl K for VolumeKey {
    fn at(&self) -> Time {
        self.at
    }
    fn set_at(&mut self, t: Time) {
        self.at = t;
    }
    fn set_val(&mut self, v: f32) {
        self.db = v;
    }
    fn make(at: Time, v: f32) -> Self {
        VolumeKey { at, db: v }
    }
}

impl K for PanKey {
    fn at(&self) -> Time {
        self.at
    }
    fn set_at(&mut self, t: Time) {
        self.at = t;
    }
    fn set_val(&mut self, v: f32) {
        self.pan = v;
    }
    fn make(at: Time, v: f32) -> Self {
        PanKey { at, pan: v }
    }
}

impl K for Key<f32> {
    fn at(&self) -> Time {
        self.at
    }
    fn set_at(&mut self, t: Time) {
        self.at = t;
    }
    fn set_val(&mut self, v: f32) {
        self.value = v;
    }
    fn make(at: Time, v: f32) -> Self {
        Key::new(at, v, Interp::Linear)
    }
}

/// The keys of a channel, to change.
enum Keys<'a> {
    Volume(&'a mut Vec<VolumeKey>),
    Pan(&'a mut Vec<PanKey>),
    Fx(&'a mut Vec<Key<f32>>),
}

/// Apply `$f` to whichever list of keys it is.
macro_rules! with_keys {
    ($k:expr, |$v:ident| $body:expr) => {
        match $k {
            Keys::Volume($v) => $body,
            Keys::Pan($v) => $body,
            Keys::Fx($v) => $body,
        }
    };
}

/// Move key `index` to `at` (kept `gap` from its neighbours, so the order
/// never changes) with `value`. Returns the time used.
fn place<T: K>(keys: &mut [T], index: usize, at: Time, value: f32, gap: Time) -> Time {
    let lo = match index.checked_sub(1).and_then(|i| keys.get(i)) {
        Some(k) => k.at() + gap,
        None => Time(i64::MIN / 4),
    };
    let hi = keys.get(index.saturating_add(1)).map_or(Time(i64::MAX / 4), |k| k.at() - gap);
    let Some(k) = keys.get_mut(index) else { return at };
    k.set_at(at.max(lo).min(hi.max(lo)));
    k.set_val(value);
    k.at()
}

/// Add (or, at a time with a key, replace) a key; returns its index.
fn insert<T: K>(keys: &mut Vec<T>, at: Time, value: f32) -> usize {
    if let Some(i) = keys.iter().position(|k| k.at() == at) {
        if let Some(k) = keys.get_mut(i) {
            k.set_val(value);
        }
        return i;
    }
    let i = keys.partition_point(|k| k.at() < at);
    keys.insert(i, T::make(at, value));
    i
}

impl ClipAudio {
    /// Every channel the clip's sound has, in the rows' order.
    pub fn audio_channels(&self) -> Vec<AudioChannel> {
        let mut v = vec![AudioChannel::Volume, AudioChannel::Pan];
        for (i, fx) in self.effects.iter().enumerate() {
            v.extend((0..fx.kind.specs().len()).map(|p| AudioChannel::Effect(i, p)));
        }
        v
    }

    fn keys_mut(&mut self, ch: AudioChannel) -> Option<Keys<'_>> {
        Some(match ch {
            AudioChannel::Volume => Keys::Volume(&mut self.volume_keys),
            AudioChannel::Pan => Keys::Pan(&mut self.pan_keys),
            AudioChannel::Effect(i, p) => {
                let fx = self.effects.get_mut(i)?;
                if fx.params.len() < fx.kind.specs().len() {
                    let specs = fx.kind.specs();
                    let have = fx.params.len();
                    fx.params.extend(specs.iter().skip(have).map(|s| crate::effects::Param { value: s.default, keys: Default::default() }));
                }
                Keys::Fx(&mut fx.params.get_mut(p)?.keys.0)
            }
        })
    }

    /// The range a channel's values keep to.
    pub fn audio_range(&self, ch: AudioChannel) -> (f32, f32) {
        match ch {
            AudioChannel::Volume => (MIN_DB, MAX_DB),
            AudioChannel::Pan => (-1.0, 1.0),
            AudioChannel::Effect(i, p) => self.effects.get(i).and_then(|f| f.kind.specs().get(p)).map_or((f32::MIN, f32::MAX), |s| (s.min, s.max)),
        }
    }

    /// A channel's keys (source time, value, easing).
    pub fn audio_keys(&self, ch: AudioChannel) -> Vec<(Time, f32, Interp)> {
        match ch {
            AudioChannel::Volume => self.volume_keys.iter().map(|k| (k.at, k.db, Interp::Linear)).collect(),
            AudioChannel::Pan => self.pan_keys.iter().map(|k| (k.at, k.pan, Interp::Linear)).collect(),
            AudioChannel::Effect(i, p) => self.effects.get(i).and_then(|f| f.params.get(p)).map_or_else(Vec::new, |q| q.keys.0.iter().map(|k| (k.at, k.value, k.interp)).collect()),
        }
    }

    pub fn audio_animated(&self, ch: AudioChannel) -> bool {
        !self.audio_keys(ch).is_empty()
    }

    /// A channel's value at source time `t`.
    pub fn audio_value(&self, ch: AudioChannel, t: Time) -> f32 {
        match ch {
            AudioChannel::Volume => self.volume_db_at(t),
            AudioChannel::Pan => self.pan_at(t),
            AudioChannel::Effect(i, p) => self.effects.get(i).map_or(0.0, |f| f.value_at(p, t)),
        }
    }

    /// Set a channel's value: a key at `t` when it's keyframed, else its one value.
    pub fn audio_set_value(&mut self, ch: AudioChannel, t: Time, v: f32) {
        let (lo, hi) = self.audio_range(ch);
        let v = if v.is_finite() { v.clamp(lo, hi) } else { lo.max(0.0).min(hi) };
        if self.audio_animated(ch) {
            if let Some(k) = self.keys_mut(ch) {
                with_keys!(k, |keys| {
                    insert(keys, t, v);
                });
            }
            return;
        }
        match ch {
            AudioChannel::Volume => self.volume_db = v,
            AudioChannel::Pan => self.pan = v,
            AudioChannel::Effect(i, p) => {
                if self.keys_mut(ch).is_some() {
                    if let Some(q) = self.effects.get_mut(i).and_then(|f| f.params.get_mut(p)) {
                        q.value = v;
                    }
                }
            }
        }
    }

    /// The stopwatch: start keyframing (a key at `t` with the value there),
    /// or stop (the keys go, the value at `t` kept).
    pub fn audio_stopwatch(&mut self, ch: AudioChannel, t: Time) {
        let v = self.audio_value(ch, t);
        if self.audio_animated(ch) {
            if let Some(k) = self.keys_mut(ch) {
                with_keys!(k, |keys| keys.clear());
            }
            self.audio_set_value(ch, t, v);
        } else if let Some(k) = self.keys_mut(ch) {
            with_keys!(k, |keys| {
                insert(keys, t, v);
            });
        }
    }

    /// The diamond: a key at `t` with the value there, or take away the one there.
    pub fn audio_toggle_key(&mut self, ch: AudioChannel, t: Time) {
        if self.audio_keys(ch).iter().any(|k| k.0 == t) {
            if let Some(k) = self.keys_mut(ch) {
                with_keys!(k, |keys| keys.retain(|k| k.at() != t));
            }
        } else {
            self.audio_add_key(ch, t);
        }
    }

    /// Add a key at `at` holding the value there; its index.
    pub fn audio_add_key(&mut self, ch: AudioChannel, at: Time) -> Option<usize> {
        let v = self.audio_value(ch, at);
        let k = self.keys_mut(ch)?;
        Some(with_keys!(k, |keys| insert(keys, at, v)))
    }

    /// Move key `index` to `at` with `value` (see [`place`]); the time used.
    pub fn audio_set_key(&mut self, ch: AudioChannel, index: usize, at: Time, value: f32, gap: Time) -> Time {
        let (lo, hi) = self.audio_range(ch);
        let value = value.clamp(lo, hi);
        match self.keys_mut(ch) {
            Some(k) => with_keys!(k, |keys| place(keys, index, at, value, gap)),
            None => at,
        }
    }

    pub fn audio_remove_key(&mut self, ch: AudioChannel, index: usize) {
        if let Some(k) = self.keys_mut(ch) {
            with_keys!(k, |keys| {
                if index < keys.len() {
                    keys.remove(index);
                }
            });
        }
    }

    /// Easing: effect settings only (volume and pan go straight from key to key).
    pub fn audio_set_interp(&mut self, ch: AudioChannel, index: usize, interp: Interp) {
        if let Some(Keys::Fx(keys)) = self.keys_mut(ch) {
            if let Some(k) = keys.get_mut(index) {
                k.interp = interp;
            }
        }
    }

    /// Slide several keys (channel, index, source time each started at) by
    /// `dt` together, as [`crate::ClipVisual::channel_shift`] does. Returns
    /// how far they went.
    pub fn audio_shift(&mut self, keys: &[(AudioChannel, usize, Time)], dt: Time, lo: Time, hi: Time, gap: Time) -> Time {
        let target = |from: Time, d: Time| (from + d).max(lo).min(hi);
        let now = |a: &Self, k: &(AudioChannel, usize, Time)| a.audio_keys(k.0).get(k.1).map(|x| x.0);
        let later = keys.first().and_then(|k| now(self, k)).is_some_and(|t| keys.first().is_some_and(|k| target(k.2, dt) > t));
        let mut order: Vec<&(AudioChannel, usize, Time)> = keys.iter().collect();
        order.sort_by_key(|k| (k.2, k.1));
        if later {
            order.reverse();
        }
        let mut moved = dt;
        for k in order {
            let Some((_, value, _)) = self.audio_keys(k.0).get(k.1).copied() else { continue };
            let got = self.audio_set_key(k.0, k.1, target(k.2, dt), value, gap);
            if (got - k.2).0.abs() < moved.0.abs() {
                moved = got - k.2;
            }
        }
        if moved != dt && (moved.0 == 0 || moved.0.signum() == dt.0.signum()) {
            for k in keys {
                if let Some((_, value, _)) = self.audio_keys(k.0).get(k.1).copied() {
                    self.audio_set_key(k.0, k.1, target(k.2, moved), value, gap);
                }
            }
        }
        moved
    }

    /// Paste keys (channel, source time, value, easing). Returns how many went in.
    pub fn audio_paste(&mut self, keys: &[(AudioChannel, Time, f32, Interp)], gap: Time) -> usize {
        let mut n = 0;
        for &(ch, at, value, interp) in keys {
            let Some(i) = self.audio_add_key(ch, at) else { continue };
            self.audio_set_key(ch, i, at, value, gap);
            self.audio_set_interp(ch, i, interp);
            n += 1;
        }
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audiofx::{AudioEffect, AudioFxKind};

    #[test]
    fn stopwatch_diamond_and_value_on_volume_pan_and_effects() {
        let mut a = ClipAudio::default();
        let (t1, t2) = (Time(100), Time(300));
        // Not keyframed: a value is the value.
        a.audio_set_value(AudioChannel::Volume, t1, -6.0);
        assert_eq!((a.volume_db, a.volume_keys.len()), (-6.0, 0));
        // Stopwatch on: a key with it; changing elsewhere adds keys.
        a.audio_stopwatch(AudioChannel::Volume, t1);
        a.audio_set_value(AudioChannel::Volume, t2, -20.0);
        assert_eq!(a.audio_keys(AudioChannel::Volume).iter().map(|k| (k.0, k.1)).collect::<Vec<_>>(), vec![(t1, -6.0), (t2, -20.0)]);
        assert_eq!(a.audio_value(AudioChannel::Volume, Time(200)), -13.0);
        // The diamond takes one away and puts one back.
        a.audio_toggle_key(AudioChannel::Volume, t2);
        assert_eq!(a.volume_keys.len(), 1);
        a.audio_toggle_key(AudioChannel::Volume, Time(200));
        assert_eq!(a.volume_keys.len(), 2);
        // Stopwatch off: keys gone, the value at the playhead kept.
        a.audio_stopwatch(AudioChannel::Volume, t1);
        assert!(a.volume_keys.is_empty() && a.volume_db == -6.0);
        // Pan is kept to −1…1.
        a.audio_set_value(AudioChannel::Pan, t1, 7.0);
        assert_eq!(a.pan, 1.0);
        // An effect's setting.
        a.effects.push(AudioEffect::new(AudioFxKind::ALL[0]));
        let ch = AudioChannel::Effect(0, 0);
        a.audio_stopwatch(ch, t1);
        assert!(a.audio_animated(ch) && a.audio_channels().contains(&ch));
        a.audio_set_interp(ch, 0, Interp::Hold);
        assert_eq!(a.audio_keys(ch)[0].2, Interp::Hold);
        // Hostile indexes do nothing.
        a.audio_remove_key(AudioChannel::Effect(9, 9), 3);
        a.audio_set_key(AudioChannel::Pan, 99, t1, 0.0, Time(1));
    }

    #[test]
    fn keys_slide_together_and_stop_short_of_others() {
        let mut a = ClipAudio::default();
        for (t, v) in [(0, 0.0), (100, -3.0), (200, -6.0), (400, -9.0)] {
            a.volume_keys.push(VolumeKey { at: Time(t), db: v });
        }
        a.pan_keys.push(PanKey { at: Time(100), pan: 0.5 });
        let picked = [(AudioChannel::Volume, 1, Time(100)), (AudioChannel::Volume, 2, Time(200)), (AudioChannel::Pan, 0, Time(100))];
        let went = a.audio_shift(&picked, Time(150), Time(0), Time(1000), Time(10));
        // Room for all of it (the one at 200 goes to 350, short of 400).
        assert_eq!(went, Time(150));
        assert_eq!(a.audio_keys(AudioChannel::Volume).iter().map(|k| k.0 .0).collect::<Vec<_>>(), vec![0, 250, 350, 400]);
        assert_eq!(a.pan_keys[0].at, Time(250));
        // Further than there's room for: they keep their spacing.
        let picked = [(AudioChannel::Volume, 1, Time(250)), (AudioChannel::Volume, 2, Time(350))];
        let went = a.audio_shift(&picked, Time(200), Time(0), Time(1000), Time(10));
        assert_eq!(went, Time(40));
        assert_eq!(a.audio_keys(AudioChannel::Volume).iter().map(|k| k.0 .0).collect::<Vec<_>>(), vec![0, 290, 390, 400]);
        // Paste back in.
        let n = a.audio_paste(&[(AudioChannel::Pan, Time(500), -0.25, Interp::Linear)], Time(10));
        assert_eq!((n, a.pan_at(Time(600))), (1, -0.25));
    }
}
