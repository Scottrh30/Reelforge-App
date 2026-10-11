//! The playhead, driven by the audio clock when there is an output device.
//!
//! With audio: the playhead is whatever timeline sample is reaching the
//! speakers, so video follows sound. Without a device (or if the device
//! stalls), it falls back to wall-clock time since Play was pressed. Either
//! way speed is independent of the UI frame rate.

use crate::audio::AudioEngine;
use reelforge_core::{FrameRate, Time};
use std::time::{Duration, Instant};

/// How long to wait for the first audio before falling back to the wall clock.
const AUDIO_START_TIMEOUT: Duration = Duration::from_millis(500);

#[derive(Default)]
pub struct Playback {
    pub playing: bool,
    pub playhead: Time,
    anchor: Option<(Instant, Time)>,
    pub audio: Option<AudioEngine>,
    /// Set when the audio clock didn't start in time for this play.
    wall_clock: bool,
    /// Hear audio when the playhead moves while paused (dragging, frame stepping).
    pub scrub_audio: bool,
    last_heard: Time,
    /// Shuttle speed (J/K/L): 1 = normal play, 2/4/8 faster, negative backwards.
    /// Anything but 1 runs on the system clock, without sound.
    pub shuttle: f64,
}

impl Playback {
    pub fn with_audio(audio: Option<AudioEngine>) -> Self {
        Self { audio, scrub_audio: true, shuttle: 1.0, ..Default::default() }
    }

    /// Call once per UI frame: plays a scrub snippet if the paused playhead moved.
    pub fn scrub_if_moved(&mut self) {
        let moved = self.playhead != self.last_heard;
        self.last_heard = self.playhead;
        if moved && !self.playing && self.scrub_audio {
            if let Some(a) = &self.audio {
                a.scrub(self.playhead);
            }
        }
    }

    pub fn clock_name(&self) -> &'static str {
        match (&self.audio, self.wall_clock) {
            (Some(_), false) => "audio clock",
            _ => "system clock",
        }
    }

    pub fn play(&mut self) {
        self.playing = true;
        self.wall_clock = false;
        self.anchor = Some((Instant::now(), self.playhead));
        if let Some(a) = &mut self.audio {
            if self.shuttle == 1.0 {
                a.play(self.playhead);
            } else {
                a.stop();
            }
        }
    }

    /// J (`dir` = -1) or L (`dir` = 1): play that way; pressing again doubles
    /// the speed, up to 8×.
    pub fn shuttle(&mut self, dir: f64, end: Time) {
        let same_way = self.playing && self.shuttle.signum() == dir.signum();
        self.shuttle = if same_way { (self.shuttle * 2.0).clamp(-8.0, 8.0) } else { dir };
        if dir > 0.0 && self.playhead >= end {
            self.playhead = Time::ZERO;
        }
        self.play();
    }

    pub fn pause(&mut self) {
        if self.playing {
            if let Some(a) = &mut self.audio {
                a.stop();
            }
        }
        self.playing = false;
        self.anchor = None;
    }

    pub fn toggle(&mut self, end: Time) {
        if self.playing {
            self.pause();
        } else {
            self.shuttle = 1.0;
            if self.playhead >= end {
                self.playhead = Time::ZERO; // replay from start
            }
            self.play();
        }
    }

    pub fn seek(&mut self, t: Time) {
        self.playhead = t.max(Time::ZERO);
        if self.playing {
            self.play();
        }
    }

    pub fn step(&mut self, frames: i64, rate: FrameRate) {
        self.pause();
        let f = (self.playhead.to_frame(rate) + frames).max(0);
        self.playhead = Time::from_frame(f, rate);
    }

    /// Advance the playhead; stops at `end`.
    pub fn tick(&mut self, rate: FrameRate, end: Time) {
        let Some((started, from)) = self.anchor else { return };
        let wall = from + Time::from_secs_f64(started.elapsed().as_secs_f64() * self.shuttle);
        if self.shuttle != 1.0 {
            // Shuttling: system clock, either direction.
            if wall <= Time::ZERO || wall >= end {
                self.playhead = wall.max(Time::ZERO).min(end);
                self.pause();
            } else {
                self.playhead = wall.snap(rate);
            }
            return;
        }
        let t = match (&self.audio, self.wall_clock) {
            (Some(a), false) => match a.position() {
                Some(p) => p,
                // Audio hasn't started yet: hold still briefly rather than run ahead of it.
                None if started.elapsed() < AUDIO_START_TIMEOUT => from,
                None => {
                    log::warn!("audio clock didn't start; using system clock");
                    self.wall_clock = true;
                    wall
                }
            },
            _ => wall,
        };
        if t >= end {
            self.playhead = end;
            self.pause();
        } else {
            self.playhead = t.snap(rate);
        }
    }
}
