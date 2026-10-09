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

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClipAudio {
    pub volume_db: f32,
    pub pan: f32,
    pub fade_in: Time,
    pub fade_out: Time,
}

impl Default for ClipAudio {
    fn default() -> Self {
        Self { volume_db: 0.0, pan: 0.0, fade_in: Time::ZERO, fade_out: Time::ZERO }
    }
}

impl ClipAudio {
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
    fn fades_are_clamped_to_duration() {
        let a = ClipAudio { fade_in: Time::from_secs_f64(3.0), fade_out: Time::from_secs_f64(3.0), ..Default::default() };
        let (i, o) = a.clamped_fades(Time::from_secs_f64(4.0));
        assert_eq!(i, Time::from_secs_f64(3.0));
        assert_eq!(o, Time::from_secs_f64(1.0));
    }
}
