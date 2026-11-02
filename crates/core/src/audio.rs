//! Audio levels: per-clip volume, pan and fades, per-track volume and pan,
//! and the maths the mixer uses to apply them.

use crate::time::Time;
use serde::{Deserialize, Serialize};

/// Below this, a volume slider means "silent".
pub const MIN_DB: f32 = -60.0;
pub const MAX_DB: f32 = 12.0;

/// Decibels to linear amplitude. `MIN_DB` and below are silence.
pub fn db_to_gain(db: f32) -> f32 {
    if db <= MIN_DB { 0.0 } else { 10f32.powf(db / 20.0) }
}

/// Stereo balance: 0 = centre (both channels untouched), -1 = left only,
/// +1 = right only. Turning towards one side attenuates the other, so a
/// centred stereo source keeps its level and nothing is ever boosted.
pub fn balance(pan: f32) -> [f32; 2] {
    let p = pan.clamp(-1.0, 1.0);
    [(1.0 - p).min(1.0), (1.0 + p).min(1.0)]
}

/// Fade curve for `x` in 0..=1 (0 = silent end, 1 = full level).
/// Equal-power (quarter sine): sounds even to the ear, and a fade-out
/// overlapping a fade-in of the same length keeps constant power.
pub fn fade_curve(x: f32) -> f32 {
    (x.clamp(0.0, 1.0) * std::f32::consts::FRAC_PI_2).sin()
}

/// A volume keyframe. Anchored to the clip's *source* time, so keys stay on the
/// same sound when the clip is moved, trimmed or split.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct VolumeKey {
    pub at: Time,
    pub db: f32,
}

/// A pan keyframe (-1 left … +1 right), anchored to source time like [`VolumeKey`].
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct PanKey {
    pub at: Time,
    pub pan: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClipAudio {
    /// Constant volume, used when there are no keyframes.
    pub volume_db: f32,
    pub pan: f32,
    pub fade_in: Time,
    pub fade_out: Time,
    /// Volume automation, sorted by `at`. When non-empty it replaces `volume_db`.
    pub volume_keys: Vec<VolumeKey>,
    /// Length of an equal-power crossfade with the clip that ends exactly where
    /// this one starts on the same track (0 = a plain cut).
    pub crossfade_in: Time,
    /// Pan automation, sorted by `at`. When non-empty it replaces `pan`.
    pub pan_keys: Vec<PanKey>,
    /// What automatic ducking added to the volume (breakpoints in source
    /// time, dB), so it can be replaced or taken off. See [`crate::duck`].
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub ducking: Vec<VolumeKey>,
    /// Effects on the clip's sound, in order, before its volume, pan and fades.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub effects: Vec<crate::audiofx::AudioEffect>,
}

impl Default for ClipAudio {
    fn default() -> Self {
        Self { volume_db: 0.0, pan: 0.0, fade_in: Time::ZERO, fade_out: Time::ZERO, volume_keys: Vec::new(), crossfade_in: Time::ZERO, pan_keys: Vec::new(), ducking: Vec::new(), effects: Vec::new() }
    }
}

/// Volume in dB at `x` along a sorted keyframe list: linear in dB between keys
/// (sounds even), held flat before the first and after the last.
pub fn interpolate_db(keys: &[(i64, f32)], x: i64) -> f32 {
    interpolate(keys, x)
}

/// Linear interpolation along sorted `(position, value)` keys, held flat
/// before the first and after the last (0 with no keys).
pub fn interpolate(keys: &[(i64, f32)], x: i64) -> f32 {
    match keys {
        [] => 0.0,
        [(_, db)] => *db,
        _ => {
            let i = keys.partition_point(|(k, _)| *k <= x);
            if i == 0 {
                return keys[0].1;
            }
            if i == keys.len() {
                return keys[i - 1].1;
            }
            let ((x0, d0), (x1, d1)) = (keys[i - 1], keys[i]);
            if x1 == x0 {
                return d1;
            }
            d0 + (d1 - d0) * ((x - x0) as f32 / (x1 - x0) as f32)
        }
    }
}

impl ClipAudio {
    pub fn is_animated(&self) -> bool {
        !self.volume_keys.is_empty()
    }

    /// Volume (dB) at source time `t`.
    pub fn volume_db_at(&self, t: Time) -> f32 {
        let keys = &self.volume_keys;
        if keys.is_empty() {
            return self.volume_db;
        }
        // Same maths as `interpolate_db`, without building a temporary list.
        let i = keys.partition_point(|k| k.at <= t);
        if i == 0 {
            return keys[0].db;
        }
        if i == keys.len() {
            return keys[i - 1].db;
        }
        let (a, b) = (keys[i - 1], keys[i]);
        if b.at == a.at {
            return b.db;
        }
        a.db + (b.db - a.db) * ((t.0 - a.at.0) as f32 / (b.at.0 - a.at.0) as f32)
    }

    /// Add a key at source time `at`, replacing one already there.
    pub fn set_key(&mut self, at: Time, db: f32) {
        let db = db.clamp(MIN_DB, MAX_DB);
        match self.volume_keys.iter_mut().find(|k| k.at == at) {
            Some(k) => k.db = db,
            None => {
                let i = self.volume_keys.partition_point(|k| k.at < at);
                self.volume_keys.insert(i, VolumeKey { at, db });
            }
        }
    }

    pub fn key_index_at(&self, at: Time) -> Option<usize> {
        self.volume_keys.iter().position(|k| k.at == at)
    }

    pub fn is_pan_animated(&self) -> bool {
        !self.pan_keys.is_empty()
    }

    /// Pan (-1…+1) at source time `t`: linear between keys, held at the ends.
    pub fn pan_at(&self, t: Time) -> f32 {
        let keys = &self.pan_keys;
        if keys.is_empty() {
            return self.pan;
        }
        let i = keys.partition_point(|k| k.at <= t);
        if i == 0 {
            return keys[0].pan;
        }
        if i == keys.len() {
            return keys[i - 1].pan;
        }
        let (a, b) = (keys[i - 1], keys[i]);
        if b.at == a.at {
            return b.pan;
        }
        a.pan + (b.pan - a.pan) * ((t.0 - a.at.0) as f32 / (b.at.0 - a.at.0) as f32)
    }

    /// Add a pan key at source time `at`, replacing one already there.
    pub fn set_pan_key(&mut self, at: Time, pan: f32) {
        let pan = pan.clamp(-1.0, 1.0);
        match self.pan_keys.iter_mut().find(|k| k.at == at) {
            Some(k) => k.pan = pan,
            None => {
                let i = self.pan_keys.partition_point(|k| k.at < at);
                self.pan_keys.insert(i, PanKey { at, pan });
            }
        }
    }

    pub fn pan_key_index_at(&self, at: Time) -> Option<usize> {
        self.pan_keys.iter().position(|k| k.at == at)
    }

    /// Fades limited so they fit in a clip of `duration` (and can't overlap).
    pub fn clamped_fades(&self, duration: Time) -> (Time, Time) {
        let d = duration.max(Time::ZERO);
        let fin = self.fade_in.max(Time::ZERO).min(d);
        let fout = self.fade_out.max(Time::ZERO).min(d - fin);
        (fin, fout)
    }

    /// Fade multiplier at `offset` into a clip of `duration` (both in any one unit).
    pub fn fade_at(offset: i64, duration: i64, fade_in: i64, fade_out: i64) -> f32 {
        let mut g = 1.0;
        if fade_in > 0 && offset < fade_in {
            g *= fade_curve((offset as f32 + 0.5) / fade_in as f32);
        }
        let from_end = duration - offset;
        if fade_out > 0 && from_end <= fade_out {
            g *= fade_curve((from_end as f32 - 0.5) / fade_out as f32);
        }
        g
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TrackAudio {
    pub volume_db: f32,
    pub pan: f32,
}

impl Default for TrackAudio {
    fn default() -> Self {
        Self { volume_db: 0.0, pan: 0.0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decibels() {
        assert_eq!(db_to_gain(0.0), 1.0);
        assert!((db_to_gain(-6.0206) - 0.5).abs() < 1e-4);
        assert!((db_to_gain(6.0206) - 2.0).abs() < 1e-3);
        assert_eq!(db_to_gain(MIN_DB), 0.0);
    }

    #[test]
    fn balance_law() {
        assert_eq!(balance(0.0), [1.0, 1.0]);
        assert_eq!(balance(-1.0), [1.0, 0.0]);
        assert_eq!(balance(1.0), [0.0, 1.0]);
        assert_eq!(balance(0.5), [0.5, 1.0]);
        assert_eq!(balance(7.0), [0.0, 1.0], "clamped");
    }

    #[test]
    fn fades_ramp_and_stay_within_the_clip() {
        // 100-sample clip, 10 in, 20 out.
        let f = |o| ClipAudio::fade_at(o, 100, 10, 20);
        assert!(f(0) < 0.1);
        assert!(f(5) > 0.6 && f(5) < 0.8);
        assert_eq!(f(10), 1.0);
        assert_eq!(f(79), 1.0);
        assert!(f(99) < 0.1);
        for o in 1..10 {
            assert!(f(o) > f(o - 1), "fade-in rises");
        }
        for o in 81..100 {
            assert!(f(o) < f(o - 1), "fade-out falls");
        }
        // Equal power: a fade-out and fade-in crossing keep sum of squares at 1.
        for i in 0..20 {
            let out = ClipAudio::fade_at(80 + i, 100, 0, 20);
            let inn = ClipAudio::fade_at(i, 100, 20, 0);
            assert!((out * out + inn * inn - 1.0).abs() < 0.02);
        }
    }

    #[test]
    fn keyframes_interpolate_in_db_and_hold_at_the_ends() {
        let mut a = ClipAudio::default();
        assert_eq!(a.volume_db_at(Time(5)), 0.0);
        a.volume_db = -3.0;
        assert_eq!(a.volume_db_at(Time(5)), -3.0, "no keys: constant volume");
        a.set_key(Time(200), -20.0);
        a.set_key(Time(100), 0.0);
        a.set_key(Time(300), -20.0);
        assert_eq!(a.volume_keys.iter().map(|k| k.at.0).collect::<Vec<_>>(), vec![100, 200, 300], "kept sorted");
        assert_eq!(a.volume_db_at(Time(0)), 0.0, "held before the first key");
        assert_eq!(a.volume_db_at(Time(150)), -10.0, "linear in dB");
        assert_eq!(a.volume_db_at(Time(250)), -20.0);
        assert_eq!(a.volume_db_at(Time(9999)), -20.0, "held after the last key");
        a.set_key(Time(200), -6.0);
        assert_eq!(a.volume_keys.len(), 3, "same time replaces");
        a.set_key(Time(400), 99.0);
        assert_eq!(a.volume_keys[3].db, MAX_DB, "clamped");
    }

    #[test]
    fn pan_keys_interpolate_linearly_and_hold_at_the_ends() {
        let mut a = ClipAudio { pan: 0.25, ..Default::default() };
        assert_eq!(a.pan_at(Time(50)), 0.25, "no keys: constant pan");
        a.set_pan_key(Time(300), 1.0);
        a.set_pan_key(Time(100), -1.0);
        assert!(a.is_pan_animated());
        assert_eq!(a.pan_at(Time(0)), -1.0);
        assert_eq!(a.pan_at(Time(200)), 0.0);
        assert_eq!(a.pan_at(Time(250)), 0.5);
        assert_eq!(a.pan_at(Time(9999)), 1.0);
        a.set_pan_key(Time(100), -9.0);
        assert_eq!((a.pan_keys.len(), a.pan_keys[0].pan), (2, -1.0), "replaced and clamped");
        assert_eq!(a.pan_key_index_at(Time(300)), Some(1));
        // Older projects (no pan_keys) still load.
        let old: ClipAudio = serde_json::from_str(r#"{"volume_db":0.0,"pan":0.5}"#).unwrap();
        assert!(old.pan_keys.is_empty() && old.pan == 0.5);
    }

    #[test]
    fn old_clip_audio_loads_without_new_fields() {
        let a: ClipAudio = serde_json::from_str(r#"{"volume_db":-3.0,"pan":0.0,"fade_in":0,"fade_out":0}"#).unwrap();
        assert!(a.volume_keys.is_empty());
        assert_eq!(a.crossfade_in, Time::ZERO);
    }

    #[test]
    fn fades_are_clamped_to_duration() {
        let a = ClipAudio { fade_in: Time::from_secs_f64(3.0), fade_out: Time::from_secs_f64(3.0), ..Default::default() };
        let (i, o) = a.clamped_fades(Time::from_secs_f64(4.0));
        assert_eq!(i, Time::from_secs_f64(3.0));
        assert_eq!(o, Time::from_secs_f64(1.0));
    }
}
