//! Automatic ducking: lower the music while someone is speaking.
//!
//! The dialogue's loudness is read (peaks, in dBFS) and turned into the spans
//! where someone is talking; around each span the music's volume is brought
//! down by `amount` and back up afterwards. The result is written as ordinary
//! volume keyframes on the music clips, so it can be seen and fine-tuned, and
//! each clip remembers what was added (`ClipAudio::ducking`) so ducking again
//! replaces it instead of piling up, and it can be taken off again.

use crate::audio::{ClipAudio, VolumeKey, MAX_DB, MIN_DB};
use crate::time::Time;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DuckSettings {
    /// How far the music goes down (dB, negative).
    pub amount_db: f32,
    /// Dialogue louder than this (peak dBFS) counts as speech.
    pub threshold_db: f32,
    /// The music starts going down this long before the speech.
    pub fade_down: Time,
    /// … and comes back up over this long after it.
    pub fade_up: Time,
    /// Pauses shorter than this keep the music down.
    pub hold: Time,
    /// Sounds shorter than this (a click, a breath) don't count as speech.
    pub min_speech: Time,
}

impl Default for DuckSettings {
    fn default() -> Self {
        Self {
            amount_db: -12.0,
            threshold_db: -32.0,
            fade_down: Time::from_secs_f64(0.25),
            fade_up: Time::from_secs_f64(0.6),
            hold: Time::from_secs_f64(0.8),
            min_speech: Time::from_secs_f64(0.12),
        }
    }
}

/// Where someone is speaking between `from` and `to` (sequence times),
/// from `level(t)`: the dialogue's peak level (dBFS) around `t`, read every
/// `step`. Pauses shorter than `hold` are bridged and blips shorter than
/// `min_speech` dropped.
pub fn speech_spans(level: impl Fn(Time) -> f32, from: Time, to: Time, step: Time, s: &DuckSettings) -> Vec<(Time, Time)> {
    let step = Time(step.0.max(1));
    let mut spans: Vec<(Time, Time)> = Vec::new();
    let mut open: Option<Time> = None;
    let mut t = from;
    while t < to {
        let loud = level(t) >= s.threshold_db;
        match (loud, open) {
            (true, None) => open = Some(t),
            (false, Some(a)) => {
                spans.push((a, t));
                open = None;
            }
            _ => {}
        }
        t = t + step;
    }
    if let Some(a) = open {
        spans.push((a, to));
    }
    // Bridge short pauses, then drop blips.
    let mut merged: Vec<(Time, Time)> = Vec::new();
    for (a, b) in spans {
        match merged.last_mut() {
            Some(last) if a - last.1 < s.hold => last.1 = b,
            _ => merged.push((a, b)),
        }
    }
    merged.retain(|(a, b)| *b - *a >= s.min_speech);
    merged
}

/// The change in volume (dB, 0 or below) over time for those spans, as
/// breakpoints, linear between them: down to `amount_db` by the start of each
/// span (starting `fade_down` before) and back up over `fade_up` after it.
/// Spans close enough that the music wouldn't get back up are joined.
pub fn envelope(spans: &[(Time, Time)], s: &DuckSettings) -> Vec<(Time, f32)> {
    let amount = s.amount_db.min(0.0);
    if amount == 0.0 {
        return Vec::new();
    }
    let mut joined: Vec<(Time, Time)> = Vec::new();
    for &(a, b) in spans {
        match joined.last_mut() {
            Some(last) if a - s.fade_down <= last.1 + s.fade_up => last.1 = last.1.max(b),
            _ => joined.push((a, b)),
        }
    }
    let mut out = Vec::new();
    for (a, b) in joined {
        out.push((a - s.fade_down, 0.0));
        out.push((a, amount));
        out.push((b, amount));
        out.push((b + s.fade_up, 0.0));
    }
    out
}

/// A breakpoint list's value at `t` (linear between, held at the ends; 0 if empty).
fn env_at(env: &[(Time, f32)], t: Time) -> f32 {
    let (Some(first), Some(last)) = (env.first(), env.last()) else { return 0.0 };
    if t <= first.0 {
        return first.1;
    }
    if t >= last.0 {
        return last.1;
    }
    let i = env.partition_point(|k| k.0 <= t);
    let ((t0, v0), (t1, v1)) = (env[i - 1], env[i]);
    if t1 == t0 {
        return v1;
    }
    v0 + (v1 - v0) * ((t - t0).0 as f32 / (t1 - t0).0 as f32)
}

impl ClipAudio {
    /// Duck this clip by `env` (breakpoints in the clip's source time, in dB):
    /// any earlier ducking is taken off first, then `env` is added to its
    /// volume as keyframes. An empty `env` just takes ducking off. Only the
    /// part of `env` inside `range` (the clip's source times) is used.
    pub fn set_ducking(&mut self, env: Vec<(Time, f32)>, range: (Time, Time)) {
        let old: Vec<(Time, f32)> = self.ducking.iter().map(|k| (k.at, k.db)).collect();
        // Keep only what lands on the clip (with a breakpoint at each edge).
        let env: Vec<(Time, f32)> = if env.is_empty() {
            env
        } else {
            let mut v: Vec<(Time, f32)> = vec![(range.0, env_at(&env, range.0))];
            v.extend(env.iter().copied().filter(|(t, _)| *t > range.0 && *t < range.1));
            v.push((range.1, env_at(&env, range.1)));
            if v.iter().all(|(_, d)| *d == 0.0) { Vec::new() } else { v }
        };
        if old.is_empty() && env.is_empty() {
            return;
        }
        // Volume and both envelopes are linear between their breakpoints, so
        // their sum is exact at the union of them all.
        let mut times: Vec<Time> = self.volume_keys.iter().map(|k| k.at).chain(old.iter().map(|k| k.0)).chain(env.iter().map(|k| k.0)).collect();
        times.sort();
        times.dedup();
        let keys: Vec<VolumeKey> = times
            .iter()
            .map(|&t| VolumeKey { at: t, db: (self.volume_db_at(t) - env_at(&old, t) + env_at(&env, t)).clamp(MIN_DB, MAX_DB) })
            .collect();
        self.volume_keys = simplify(keys);
        // A flat line is just a volume.
        if let Some(first) = self.volume_keys.first().copied() {
            if self.volume_keys.iter().all(|k| (k.db - first.db).abs() < 0.01) {
                self.volume_db = first.db;
                self.volume_keys.clear();
            }
        }
        self.ducking = env.into_iter().map(|(at, db)| VolumeKey { at, db }).collect();
    }

    pub fn is_ducked(&self) -> bool {
        !self.ducking.is_empty()
    }
}

/// Drop keys that sit on the straight line between their neighbours.
fn simplify(keys: Vec<VolumeKey>) -> Vec<VolumeKey> {
    let mut out: Vec<VolumeKey> = Vec::with_capacity(keys.len());
    for (i, k) in keys.iter().enumerate() {
        let Some(next) = keys.get(i + 1) else {
            out.push(*k);
            break;
        };
        match out.last() {
            Some(prev) if next.at > prev.at => {
                let u = (k.at - prev.at).0 as f32 / (next.at - prev.at).0 as f32;
                if (prev.db + (next.db - prev.db) * u - k.db).abs() < 0.01 {
                    continue;
                }
                out.push(*k);
            }
            _ => out.push(*k),
        }
    }
    // Volume is held flat before the first key and after the last: a repeat there adds nothing.
    while out.len() > 1 && (out[0].db - out[1].db).abs() < 0.01 {
        out.remove(0);
    }
    while out.len() > 1 && (out[out.len() - 1].db - out[out.len() - 2].db).abs() < 0.01 {
        out.pop();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(x: f64) -> Time {
        Time::from_secs_f64(x)
    }

    #[test]
    fn speech_is_found_with_pauses_bridged_and_blips_dropped() {
        let st = DuckSettings::default();
        // Talking 1–2 s and 2.3–3 s (a short pause), a click at 5 s, talking 7–8 s.
        let level = |t: Time| {
            let x = t.as_secs_f64();
            if (1.0..2.0).contains(&x) || (2.3..3.0).contains(&x) || (5.0..5.05).contains(&x) || (7.0..8.0).contains(&x) { -12.0 } else { -60.0 }
        };
        let spans = speech_spans(level, s(0.0), s(10.0), s(0.01), &st);
        assert_eq!(spans.len(), 2, "{spans:?}");
        assert!((spans[0].0.as_secs_f64() - 1.0).abs() < 0.011 && (spans[0].1.as_secs_f64() - 3.0).abs() < 0.011);
        assert!((spans[1].0.as_secs_f64() - 7.0).abs() < 0.011);
        let env = envelope(&spans, &st);
        assert_eq!(env.len(), 8);
        assert_eq!(env_at(&env, s(2.0)), -12.0);
        assert_eq!(env_at(&env, s(0.5)), 0.0);
        assert!((env_at(&env, s(0.875)) + 6.0).abs() < 0.05, "halfway down");
        // Close spans are joined: the music doesn't bob up between.
        let env = envelope(&[(s(1.0), s(2.0)), (s(2.5), s(3.0))], &st);
        assert_eq!(env.len(), 4);
        assert_eq!(env_at(&env, s(2.25)), -12.0);
    }

    #[test]
    fn ducking_adds_to_the_volume_and_comes_off_again() {
        let st = DuckSettings::default();
        let mut a = ClipAudio { volume_db: -3.0, ..Default::default() };
        let env = envelope(&[(s(2.0), s(4.0))], &st);
        a.set_ducking(env.clone(), (s(0.0), s(10.0)));
        assert!(a.is_ducked());
        assert_eq!(a.volume_db_at(s(1.0)), -3.0);
        assert_eq!(a.volume_db_at(s(3.0)), -15.0);
        assert_eq!(a.volume_keys.len(), 4, "{:?}", a.volume_keys);
        // Ducking again replaces it rather than doubling it.
        a.set_ducking(env, (s(0.0), s(10.0)));
        assert_eq!(a.volume_db_at(s(3.0)), -15.0);
        // A hand-made key in between is kept when ducking comes off.
        a.set_key(s(8.0), -9.0);
        a.set_ducking(Vec::new(), (s(0.0), s(10.0)));
        assert!(!a.is_ducked());
        assert!((a.volume_db_at(s(3.0)) - a.volume_db_at(s(1.0))).abs() < 0.01, "back to the line before");
        assert_eq!(a.volume_db_at(s(8.0)), -9.0);
        // Without the hand-made key it's back to a plain volume.
        let mut b = ClipAudio::default();
        b.set_ducking(envelope(&[(s(2.0), s(4.0))], &st), (s(0.0), s(10.0)));
        b.set_ducking(Vec::new(), (s(0.0), s(10.0)));
        assert!(b.volume_keys.is_empty() && b.volume_db == 0.0);
        // Speech past the clip's end: only the part on the clip is used.
        let mut c = ClipAudio::default();
        c.set_ducking(envelope(&[(s(4.0), s(9.0))], &st), (s(0.0), s(5.0)));
        assert_eq!(c.volume_db_at(s(5.0)), -12.0);
        assert!(c.volume_keys.iter().all(|k| k.at <= s(5.0)));
        // Old files load without ducking.
        let old: ClipAudio = serde_json::from_str(r#"{"volume_db":-2.0}"#).unwrap();
        assert!(!old.is_ducked());
    }
}
