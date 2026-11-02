//! Audio effects on a clip's sound: EQ, compressor, limiter, reverb and
//! noise reduction. Each has a list of parameters that can be keyframed like
//! a visual effect's (see [`crate::effects::Param`]); the processing itself
//! is in the media crate's mixer, so playback and export sound the same.
//!
//! They work on the clip's own sound before its volume, pan and fades (like
//! inserts on a channel before the fader), so a compressor or noise
//! reduction behaves the same however the level is set.

use crate::effects::{Param, ParamSpec, ParamUi};
use crate::time::Time;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AudioFxKind {
    /// A low cut, low and high shelves and a bell in the middle.
    Eq,
    /// Evens out the level: loud passages are turned down past a threshold.
    Compressor,
    /// A brick wall: nothing goes over the ceiling (with a little look-ahead).
    Limiter,
    /// A room around the sound.
    Reverb,
    /// Takes steady background noise (hiss, hum, air conditioning) out of
    /// dialogue, learning what the noise sounds like as it plays.
    NoiseReduction,
}

const fn spec(name: &'static str, default: f32, min: f32, max: f32, suffix: &'static str, speed: f32) -> ParamSpec {
    ParamSpec { name, default, min, max, suffix, speed, ui: ParamUi::Number }
}

/// EQ parameter indices.
pub mod eq {
    pub const LOW_CUT: usize = 0;
    pub const LOW_FREQ: usize = 1;
    pub const LOW_GAIN: usize = 2;
    pub const MID_FREQ: usize = 3;
    pub const MID_GAIN: usize = 4;
    pub const MID_Q: usize = 5;
    pub const HIGH_FREQ: usize = 6;
    pub const HIGH_GAIN: usize = 7;
}
const EQ: [ParamSpec; 8] = [
    spec("Low cut", 20.0, 20.0, 1000.0, " Hz", 1.0),
    spec("Low shelf", 120.0, 30.0, 1000.0, " Hz", 1.0),
    spec("Low gain", 0.0, -24.0, 24.0, " dB", 0.1),
    spec("Mid frequency", 1000.0, 100.0, 10000.0, " Hz", 10.0),
    spec("Mid gain", 0.0, -24.0, 24.0, " dB", 0.1),
    spec("Mid width (Q)", 1.0, 0.1, 10.0, "", 0.01),
    spec("High shelf", 8000.0, 1000.0, 20000.0, " Hz", 20.0),
    spec("High gain", 0.0, -24.0, 24.0, " dB", 0.1),
];

/// Compressor parameter indices.
pub mod comp {
    pub const THRESHOLD: usize = 0;
    pub const RATIO: usize = 1;
    pub const ATTACK: usize = 2;
    pub const RELEASE: usize = 3;
    pub const KNEE: usize = 4;
    pub const MAKEUP: usize = 5;
}
const COMPRESSOR: [ParamSpec; 6] = [
    spec("Threshold", -18.0, -60.0, 0.0, " dB", 0.1),
    spec("Ratio", 4.0, 1.0, 20.0, ":1", 0.05),
    spec("Attack", 10.0, 0.1, 200.0, " ms", 0.2),
    spec("Release", 150.0, 5.0, 2000.0, " ms", 2.0),
    spec("Knee", 6.0, 0.0, 24.0, " dB", 0.1),
    spec("Make-up gain", 0.0, -12.0, 24.0, " dB", 0.1),
];

/// Limiter parameter indices.
pub mod limit {
    pub const GAIN: usize = 0;
    pub const CEILING: usize = 1;
    pub const RELEASE: usize = 2;
}
const LIMITER: [ParamSpec; 3] = [spec("Input gain", 0.0, 0.0, 24.0, " dB", 0.1), spec("Ceiling", -1.0, -24.0, 0.0, " dB", 0.1), spec("Release", 50.0, 1.0, 1000.0, " ms", 1.0)];

/// Reverb parameter indices.
pub mod reverb {
    pub const SIZE: usize = 0;
    pub const DAMPING: usize = 1;
    pub const WIDTH: usize = 2;
    pub const MIX: usize = 3;
    pub const PREDELAY: usize = 4;
}
const REVERB: [ParamSpec; 5] = [
    spec("Room size", 50.0, 0.0, 100.0, " %", 0.5),
    spec("Damping", 50.0, 0.0, 100.0, " %", 0.5),
    spec("Width", 100.0, 0.0, 100.0, " %", 0.5),
    spec("Mix", 25.0, 0.0, 100.0, " %", 0.5),
    spec("Pre-delay", 10.0, 0.0, 200.0, " ms", 0.5),
];

/// Noise reduction parameter indices.
pub mod denoise {
    pub const REDUCTION: usize = 0;
    pub const SENSITIVITY: usize = 1;
}
const NOISE_REDUCTION: [ParamSpec; 2] = [spec("Reduction", 12.0, 0.0, 40.0, " dB", 0.1), spec("Sensitivity", 50.0, 0.0, 100.0, " %", 0.5)];

impl AudioFxKind {
    pub const ALL: [AudioFxKind; 5] = [AudioFxKind::Eq, AudioFxKind::Compressor, AudioFxKind::Limiter, AudioFxKind::Reverb, AudioFxKind::NoiseReduction];

    pub fn label(self) -> &'static str {
        match self {
            AudioFxKind::Eq => "EQ",
            AudioFxKind::Compressor => "Compressor",
            AudioFxKind::Limiter => "Limiter",
            AudioFxKind::Reverb => "Reverb",
            AudioFxKind::NoiseReduction => "Noise reduction",
        }
    }

    pub fn specs(self) -> &'static [ParamSpec] {
        match self {
            AudioFxKind::Eq => &EQ,
            AudioFxKind::Compressor => &COMPRESSOR,
            AudioFxKind::Limiter => &LIMITER,
            AudioFxKind::Reverb => &REVERB,
            AudioFxKind::NoiseReduction => &NOISE_REDUCTION,
        }
    }
}

fn yes() -> bool {
    true
}

/// One audio effect on a clip.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AudioEffect {
    pub kind: AudioFxKind,
    #[serde(default = "yes")]
    pub enabled: bool,
    pub params: Vec<Param>,
}

impl AudioEffect {
    /// A new effect with every parameter at its default.
    pub fn new(kind: AudioFxKind) -> Self {
        AudioEffect { kind, enabled: true, params: kind.specs().iter().map(|s| Param { value: s.default, keys: Default::default() }).collect() }
    }

    /// Parameter `i` at source time `t`, clamped to its range (its default if
    /// missing, as in an older file).
    pub fn value_at(&self, i: usize, t: Time) -> f32 {
        let Some(s) = self.kind.specs().get(i) else { return 0.0 };
        match self.params.get(i) {
            Some(p) => p.keys.value_at(t, p.value).clamp(s.min, s.max),
            None => s.default,
        }
    }

    /// Every parameter at source time `t`.
    pub fn values_at(&self, t: Time, out: &mut Vec<f32>) {
        out.clear();
        out.extend((0..self.kind.specs().len()).map(|i| self.value_at(i, t)));
    }

    pub fn is_animated(&self) -> bool {
        self.params.iter().any(|p| p.keys.is_animated())
    }

    /// Does it change the sound at all? (An EQ left flat, a compressor at 1:1
    /// and so on are skipped.)
    pub fn is_identity(&self) -> bool {
        if !self.enabled {
            return true;
        }
        if self.is_animated() {
            return false;
        }
        let v = |i: usize| self.value_at(i, Time::ZERO);
        match self.kind {
            AudioFxKind::Eq => v(eq::LOW_CUT) <= 20.0 && v(eq::LOW_GAIN) == 0.0 && v(eq::MID_GAIN) == 0.0 && v(eq::HIGH_GAIN) == 0.0,
            AudioFxKind::Compressor => v(comp::RATIO) <= 1.0 && v(comp::MAKEUP) == 0.0,
            AudioFxKind::Reverb => v(reverb::MIX) <= 0.0,
            AudioFxKind::NoiseReduction => v(denoise::REDUCTION) <= 0.0,
            AudioFxKind::Limiter => false,
        }
    }

    /// A short description: "EQ", "Compressor 4:1 at −18 dB".
    pub fn summary(&self) -> String {
        let v = |i: usize| self.value_at(i, Time::ZERO);
        match self.kind {
            AudioFxKind::Compressor => format!("Compressor {:.1}:1 at {:.0} dB", v(comp::RATIO), v(comp::THRESHOLD)),
            AudioFxKind::Limiter => format!("Limiter at {:.1} dB", v(limit::CEILING)),
            AudioFxKind::Reverb => format!("Reverb {:.0}% wet", v(reverb::MIX)),
            AudioFxKind::NoiseReduction => format!("Noise reduction {:.0} dB", v(denoise::REDUCTION)),
            AudioFxKind::Eq => "EQ".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anim::{Curve, Interp, Key};

    #[test]
    fn defaults_keys_and_identity() {
        for k in AudioFxKind::ALL {
            let e = AudioEffect::new(k);
            assert_eq!(e.params.len(), k.specs().len());
            for (i, s) in k.specs().iter().enumerate() {
                assert_eq!(e.value_at(i, Time::ZERO), s.default, "{} {}", k.label(), s.name);
            }
        }
        // Flat EQ and a 1:1 compressor do nothing; a limiter always does.
        assert!(AudioEffect::new(AudioFxKind::Eq).is_identity());
        assert!(!AudioEffect::new(AudioFxKind::Limiter).is_identity());
        let mut c = AudioEffect::new(AudioFxKind::Compressor);
        assert!(!c.is_identity());
        c.params[comp::RATIO].value = 1.0;
        assert!(c.is_identity());
        // Keyframes, clamped to the range.
        let mut r = AudioEffect::new(AudioFxKind::Reverb);
        r.params[reverb::MIX].keys = Curve(vec![Key::new(Time(0), 0.0, Interp::Linear), Key::new(Time(1000), 200.0, Interp::Linear)]);
        assert_eq!(r.value_at(reverb::MIX, Time(500)), 100.0);
        assert!(!r.is_identity(), "animated: not skipped");
        // An older file with fewer parameters reads the rest as defaults.
        let mut old = AudioEffect::new(AudioFxKind::Eq);
        old.params.truncate(2);
        assert_eq!(old.value_at(eq::HIGH_FREQ, Time::ZERO), 8000.0);
        // Round trip.
        let j = serde_json::to_string(&r).unwrap();
        assert_eq!(serde_json::from_str::<AudioEffect>(&j).unwrap(), r);
    }
}
