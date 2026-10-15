//! Master audio meters: peak and RMS per channel with standard ballistics
//! (instant attack, 20 dB/s fall, 1.5 s peak hold) and latching clip lights.

use std::time::{Duration, Instant};

pub const FLOOR_DB: f32 = -60.0;
const FALL_DB_PER_SEC: f32 = 20.0;
const HOLD: Duration = Duration::from_millis(1500);
/// A sample at or above this counts as clipped (the mixer limits output to ±1).
const CLIP: f32 = 0.999;

fn to_db(x: f32) -> f32 {
    if x <= 0.0 { FLOOR_DB } else { (20.0 * x.log10()).max(FLOOR_DB) }
}

#[derive(Clone, Copy)]
pub struct Channel {
    pub peak_db: f32,
    pub rms_db: f32,
    pub hold_db: f32,
    hold_until: Option<Instant>,
    pub clipped: bool,
}

impl Default for Channel {
    fn default() -> Self {
        Self { peak_db: FLOOR_DB, rms_db: FLOOR_DB, hold_db: FLOOR_DB, hold_until: None, clipped: false }
    }
}

impl Channel {
    /// Feed the latest linear peak/RMS, `dt` seconds after the previous update.
    pub fn update(&mut self, peak: f32, rms: f32, dt: f32, now: Instant) {
        let fall = FALL_DB_PER_SEC * dt;
        self.peak_db = to_db(peak).max(self.peak_db - fall);
        self.rms_db = to_db(rms).max(self.rms_db - fall);
        if peak >= CLIP {
            self.clipped = true;
        }
        let p = to_db(peak);
        if p >= self.hold_db || self.hold_until.is_none_or(|t| now > t) {
            if p >= self.hold_db {
                self.hold_db = p;
                self.hold_until = Some(now + HOLD);
            } else {
                // Hold expired: let the marker fall with the bar.
                self.hold_db = (self.hold_db - fall).max(self.peak_db);
            }
        }
    }

    pub fn active(&self) -> bool {
        self.peak_db > FLOOR_DB || self.hold_db > FLOOR_DB
    }
}

pub struct Meters {
    /// The master output.
    pub ch: [Channel; 2],
    /// Each track of the sequence being played (by track index), peak only.
    pub tracks: Vec<[Channel; 2]>,
    last: Instant,
}

impl Default for Meters {
    fn default() -> Self {
        Self { ch: [Channel::default(); 2], tracks: vec![[Channel::default(); 2]; reelforge_media::MAX_TRACK_METERS], last: Instant::now() }
    }
}

impl Meters {
    /// Feed the master levels and every track's peaks (once per frame).
    pub fn update_all(&mut self, peaks: [f32; 2], rms: [f32; 2], tracks: &reelforge_media::TrackPeaks) {
        let now = Instant::now();
        let dt = (now - self.last).as_secs_f32().min(0.25);
        self.last = now;
        for c in 0..2 {
            self.ch[c].update(peaks[c], rms[c], dt, now);
        }
        for (m, lv) in self.tracks.iter_mut().zip(tracks.iter()) {
            for c in 0..2 {
                m[c].update(lv[c], lv[c], dt, now);
            }
        }
    }

    /// Read the levels played since the last frame from the audio engine.
    pub fn poll(&mut self, audio: Option<&crate::audio::AudioEngine>) {
        if let Some(a) = audio {
            let (p, r) = a.take_levels();
            let t = a.take_track_levels();
            self.update_all(p, r, &t);
        }
    }

    /// Anything still moving (so the UI keeps repainting).
    pub fn any_active(&self) -> bool {
        self.ch.iter().any(Channel::active) || self.tracks.iter().flatten().any(Channel::active)
    }

    /// Reset every clip light.
    pub fn reset_clips(&mut self) {
        for c in self.ch.iter_mut().chain(self.tracks.iter_mut().flatten()) {
            c.clipped = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attack_is_instant_and_release_is_20_db_per_second() {
        let t0 = Instant::now();
        let mut c = Channel::default();
        c.update(0.5, 0.35, 0.016, t0);
        assert!((c.peak_db - -6.02).abs() < 0.05);
        // Silence for 0.5 s: falls 10 dB.
        c.update(0.0, 0.0, 0.5, t0 + Duration::from_millis(500));
        assert!((c.peak_db - -16.02).abs() < 0.05, "{}", c.peak_db);
    }

    #[test]
    fn peak_hold_lasts_then_falls() {
        let t0 = Instant::now();
        let mut c = Channel::default();
        c.update(1.0, 0.7, 0.016, t0);
        assert!(c.clipped, "full scale lights the clip LED");
        c.update(0.1, 0.07, 1.0, t0 + Duration::from_secs(1));
        assert!(c.hold_db.abs() < 0.01, "still holding 0 dB after 1 s");
        c.update(0.1, 0.07, 1.0, t0 + Duration::from_secs(2));
        assert!(c.hold_db < -10.0, "hold released after 1.5 s: {}", c.hold_db);
        assert!(c.clipped, "clip light latches");
    }

    #[test]
    fn track_meters_follow_their_own_levels() {
        let mut m = Meters::default();
        let mut tracks = [[0.0f32; 2]; reelforge_media::MAX_TRACK_METERS];
        tracks[2] = [0.5, 0.0];
        m.update_all([0.5, 0.5], [0.3, 0.3], &tracks);
        assert!((m.tracks[2][0].peak_db - -6.02).abs() < 0.05);
        assert!(!m.tracks[2][1].active() && !m.tracks[0][0].active());
        assert!(m.any_active());
        tracks[2] = [1.0, 0.0];
        m.update_all([1.0, 1.0], [0.7, 0.7], &tracks);
        assert!(m.tracks[2][0].clipped);
        m.reset_clips();
        assert!(!m.tracks[2][0].clipped && !m.ch[0].clipped);
    }

    #[test]
    fn silence_reads_as_floor() {
        let mut c = Channel::default();
        c.update(0.0, 0.0, 0.016, Instant::now());
        assert!(!c.active());
    }
}
