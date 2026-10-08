//! Random-access "give me the frame at time t" on top of a forward-only decoder.

use crate::decoder::{DecoderHandle, VideoFrame};
use anyhow::Result;
use reelforge_core::Time;
use std::collections::VecDeque;
use std::path::Path;
use std::time::{Duration, Instant};

/// If the wanted time is further than this past what the decoder has produced,
/// seek instead of decoding forward to it.
const SEEK_THRESHOLD: Time = Time(Time::TICKS_PER_SECOND);

pub struct FrameSource {
    decoder: DecoderHandle,
    generation: u64,
    /// Frames received but not yet due, in pts order.
    ahead: VecDeque<VideoFrame>,
    /// The frame currently on screen.
    current: Option<VideoFrame>,
    /// Target of an in-flight seek; cleared when its first frame arrives.
    pending_seek: Option<Time>,
}

impl FrameSource {
    pub fn open(path: &Path, max_size: Option<(u32, u32)>) -> Result<Self> {
        let decoder = DecoderHandle::spawn(path, max_size, 8)?;
        // A fresh decoder starts at the beginning, which behaves like a seek to 0.
        Ok(Self { decoder, generation: 0, ahead: VecDeque::new(), current: None, pending_seek: Some(Time::ZERO) })
    }

    pub fn path(&self) -> &Path {
        &self.decoder.path
    }

    pub fn frame_duration(&self) -> Time {
        self.decoder.frame_duration
    }

    fn pull(&mut self) {
        while let Ok(f) = self.decoder.frames.try_recv() {
            if f.generation == self.generation {
                self.ahead.push_back(f);
            }
        }
    }

    fn needs_seek(&self, t: Time) -> bool {
        match self.pending_seek {
            Some(s) => t < s || t - s > SEEK_THRESHOLD,
            None => {
                let behind = self.current.as_ref().is_some_and(|c| t < c.pts);
                let newest = self.ahead.back().or(self.current.as_ref()).map(|f| f.pts);
                let too_far = newest.is_none_or(|n| t - n > SEEK_THRESHOLD);
                behind || too_far
            }
        }
    }

    /// Advance to the newest frame with `pts <= t` and return it.
    ///
    /// Never blocks: if the decoder hasn't produced the frame yet this returns
    /// the best frame available and catches up on later calls. Call it every repaint.
    pub fn frame_at(&mut self, t: Time) -> Option<&VideoFrame> {
        self.pull();
        if self.needs_seek(t) {
            self.generation += 1;
            self.ahead.clear();
            self.pending_seek = Some(t);
            self.decoder.seek(t, self.generation);
        }
        while self.ahead.front().is_some_and(|f| f.pts <= t) {
            self.current = self.ahead.pop_front();
            self.pending_seek = None;
        }
        // After a seek, the first frame can start after `t` (t before the first
        // frame of the file); show it rather than nothing.
        if self.pending_seek.is_some() {
            if let Some(f) = self.ahead.pop_front() {
                self.current = Some(f);
                self.pending_seek = None;
            }
        }
        self.current.as_ref()
    }

    /// True when the frame `frame_at(t)` returned is the right one for `t`.
    pub fn is_exact(&self, t: Time) -> bool {
        self.pending_seek.is_none()
            && self.current.as_ref().is_some_and(|c| {
                c.pts <= t + self.frame_duration() && t < c.pts + self.frame_duration()
            })
    }

    /// Wait (up to `timeout`) for the exact frame at `t`. For offline rendering
    /// and tests — never call this on the UI thread.
    pub fn frame_at_blocking(&mut self, t: Time, timeout: Duration) -> Option<VideoFrame> {
        let deadline = Instant::now() + timeout;
        loop {
            self.frame_at(t);
            if self.is_exact(t) || Instant::now() >= deadline {
                return self.current.clone();
            }
            if let Ok(f) = self.decoder.frames.recv_timeout(Duration::from_millis(20)) {
                if f.generation == self.generation {
                    self.ahead.push_back(f);
                }
            }
        }
    }
}
