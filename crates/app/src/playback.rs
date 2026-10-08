//! Wall-clock driven playhead.
//!
//! The playhead is derived from "time since play was pressed" rather than
//! incremented per repaint, so playback speed is correct regardless of UI
//! frame rate or dropped repaints.

use reelforge_core::{FrameRate, Time};
use std::time::Instant;

#[derive(Default)]
pub struct Playback {
    pub playing: bool,
    pub playhead: Time,
    anchor: Option<(Instant, Time)>,
}

impl Playback {
    pub fn play(&mut self) {
        self.playing = true;
        self.anchor = Some((Instant::now(), self.playhead));
    }

    pub fn pause(&mut self) {
        self.playing = false;
        self.anchor = None;
    }

    pub fn toggle(&mut self, end: Time) {
        if self.playing {
            self.pause();
        } else {
            if self.playhead >= end {
                self.playhead = Time::ZERO; // replay from start
            }
            self.play();
        }
    }

    pub fn seek(&mut self, t: Time) {
        self.playhead = t.max(Time::ZERO);
        if self.playing {
            self.anchor = Some((Instant::now(), self.playhead));
        }
    }

    pub fn step(&mut self, frames: i64, rate: FrameRate) {
        self.pause();
        let f = (self.playhead.to_frame(rate) + frames).max(0);
        self.playhead = Time::from_frame(f, rate);
    }

    /// Advance the playhead; stops at `end`.
    pub fn tick(&mut self, rate: FrameRate, end: Time) {
        let Some((start, from)) = self.anchor else { return };
        let t = from + Time::from_secs_f64(start.elapsed().as_secs_f64());
        if t >= end {
            self.playhead = end;
            self.pause();
        } else {
            self.playhead = t.snap(rate);
        }
    }
}
