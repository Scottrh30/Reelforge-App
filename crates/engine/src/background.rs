//! Rendering in the background when idle: after a few seconds of no editing, playing or
//! moving the playhead, the stretches of the sequence that are heavy to play are rendered
//! into the render cache by themselves, a little each tick, as **Render preview** would.
//! Anything the user does stops it at once; it starts again (on what's still to do) once
//! things have been quiet for long enough.
//!
//! What counts as heavy ([`heavy_ranges`]): any clip with an effect switched on, a mask,
//! a mesh warp, a blend mode, a track matte, 3D, frame blending or optical flow, or that is
//! a composition; dissolves and other transitions; and wherever two or more pictures are
//! stacked. A plain clip on its own plays straight from its decoder and isn't rendered.

use reelforge_core::project::{FrameBlend, Sequence, TrackKind};
use reelforge_core::transform::BlendMode;
use reelforge_core::Time;
use std::time::{Duration, Instant};

/// The stretches of `seq` that are heavy to play, in time order and merged.
pub fn heavy_ranges(seq: &Sequence) -> Vec<(Time, Time)> {
    let mut heavy = Vec::new();
    // Where each picture starts and ends, to find where two or more are stacked.
    let mut edges: Vec<(Time, i32)> = Vec::new();
    for track in seq.tracks.iter().filter(|t| t.kind == TrackKind::Video && !t.muted) {
        for c in &track.clips {
            let (start, end) = (c.timeline_start, c.timeline_end());
            if end <= start {
                continue;
            }
            edges.push((start, 1));
            edges.push((end, -1));
            let busy = c.effects.iter().any(|e| e.enabled)
                || c.nested.is_some()
                || !c.masks.is_empty()
                || c.mesh.is_some()
                || c.scene.is_some()
                || c.track_matte.is_some()
                || c.blend != BlendMode::Normal
                || c.frame_blend != FrameBlend::Nearest;
            if busy {
                heavy.push((start, end));
            } else if c.dissolve_in > Time::ZERO {
                heavy.push((start, (start + c.dissolve_in).min(end)));
            }
        }
    }
    // Ends before starts at the same moment (a cut isn't two pictures at once).
    edges.sort_by_key(|&(t, d)| (t, d));
    let mut depth = 0;
    let mut from = None;
    for (t, d) in edges {
        depth += d;
        match (depth >= 2, from) {
            (true, None) => from = Some(t),
            (false, Some(f)) => {
                if t > f {
                    heavy.push((f, t));
                }
                from = None;
            }
            _ => {}
        }
    }
    merge(heavy)
}

fn merge(mut v: Vec<(Time, Time)>) -> Vec<(Time, Time)> {
    v.retain(|(a, b)| b > a);
    v.sort_by_key(|&(a, b)| (a, b));
    let mut out: Vec<(Time, Time)> = Vec::new();
    for (a, b) in v {
        match out.last_mut() {
            Some(last) if a <= last.1 => last.1 = last.1.max(b),
            _ => out.push((a, b)),
        }
    }
    out
}

/// The frame ranges (`first..end`, by frame index) to render for `ranges`, starting with
/// those at or after frame `from` (so what's just ahead of the playhead comes first),
/// then the ones before it.
pub fn frame_order(ranges: &[(Time, Time)], rate: reelforge_core::FrameRate, from: i64) -> Vec<(i64, i64)> {
    let frames: Vec<(i64, i64)> = ranges.iter().map(|&(a, b)| reelforge_export::frame_range(rate, a.max(Time::ZERO), b)).filter(|(a, b)| b > a).collect();
    let mut ahead = Vec::new();
    let mut behind = Vec::new();
    for (a, b) in frames {
        if b <= from {
            behind.push((a, b));
        } else if a >= from {
            ahead.push((a, b));
        } else {
            // The playhead is inside it: from there on first, the part before last.
            ahead.insert(0, (from, b));
            behind.push((a, from));
        }
    }
    ahead.sort_by_key(|r| r.0);
    ahead.extend(behind);
    ahead
}

/// Watches for the editor going quiet.
#[derive(Debug)]
pub struct Idle {
    /// What was last seen (edits, the playhead, playing, the sequence open).
    seen: Option<(u64, i64, bool, usize)>,
    since: Instant,
    /// The edit and sequence everything heavy was last rendered for (nothing to do again
    /// until something changes).
    pub done: Option<(u64, usize)>,
}

impl Default for Idle {
    fn default() -> Self {
        Idle { seen: None, since: Instant::now(), done: None }
    }
}

impl Idle {
    /// Look at what's going on now; returns whether anything changed since last time
    /// (the user is busy: a background run should stop).
    pub fn observe(&mut self, rev: u64, playhead: Time, playing: bool, seq: usize, now: Instant) -> bool {
        let key = (rev, playhead.0, playing, seq);
        let changed = self.seen.is_some_and(|k| k != key) || playing;
        if self.seen != Some(key) || playing {
            self.since = now;
        }
        self.seen = Some(key);
        changed
    }

    /// How long it's been quiet.
    pub fn quiet_for(&self, now: Instant) -> Duration {
        now.saturating_duration_since(self.since)
    }

    /// Pretend it's been quiet `d` longer (tests).
    #[cfg(test)]
    pub fn backdate(&mut self, d: Duration) {
        self.since = self.since.checked_sub(d).unwrap_or(self.since);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reelforge_core::effects::{Effect, EffectKind};
    use reelforge_core::{Clip, ClipId, FrameRate, MediaId};

    fn s(x: f64) -> Time {
        Time::from_secs_f64(x)
    }

    fn clip(id: u64, start: f64, len: f64) -> Clip {
        Clip::new(ClipId(id), MediaId(1), s(start), s(0.0), s(len), None)
    }

    #[test]
    fn finds_what_is_heavy_to_play() {
        let mut seq = Sequence::new("S", 1920, 1080, FrameRate::new(25, 1));
        // Plain clips one after another: nothing to render.
        seq.tracks[0].clips = vec![clip(1, 0.0, 2.0), clip(2, 2.0, 2.0)];
        assert!(heavy_ranges(&seq).is_empty());
        // An effect on the second; a dissolve into a third.
        seq.tracks[0].clips[1].effects.push(Effect::new(EffectKind::Glow));
        let mut third = clip(3, 4.0, 2.0);
        third.dissolve_in = s(0.5);
        seq.tracks[0].clips.push(third);
        assert_eq!(heavy_ranges(&seq), vec![(s(2.0), s(4.5))], "merged where they touch");
        // Switched off, the effect doesn't count.
        seq.tracks[0].clips[1].effects[0].enabled = false;
        assert_eq!(heavy_ranges(&seq), vec![(s(4.0), s(4.5))]);
        // A picture over another on a second video track: where they overlap.
        let mut over = Sequence::new("S", 1920, 1080, FrameRate::new(25, 1));
        over.tracks[0].clips = vec![clip(1, 0.0, 6.0)];
        let mut top = over.tracks[0].clone();
        top.clips = vec![clip(2, 1.0, 1.0), clip(3, 5.0, 3.0)];
        over.tracks.insert(1, top);
        assert_eq!(heavy_ranges(&over), vec![(s(1.0), s(2.0)), (s(5.0), s(6.0))]);
        // A hidden (muted) track doesn't show, so doesn't count.
        over.tracks[1].muted = true;
        assert!(heavy_ranges(&over).is_empty());
    }

    #[test]
    fn renders_from_the_playhead_on_first() {
        let rate = FrameRate::new(25, 1);
        let r = [(s(0.0), s(1.0)), (s(2.0), s(4.0)), (s(6.0), s(7.0))];
        // The playhead at 3 s (frame 75), inside the second stretch.
        assert_eq!(frame_order(&r, rate, 75), vec![(75, 100), (150, 175), (0, 25), (50, 75)]);
        assert_eq!(frame_order(&r, rate, 0), vec![(0, 25), (50, 100), (150, 175)]);
        assert!(frame_order(&[], rate, 10).is_empty());
    }

    #[test]
    fn notices_going_quiet_and_waking_up() {
        let t0 = Instant::now();
        let mut idle = Idle::default();
        assert!(!idle.observe(1, s(0.0), false, 0, t0), "the first look isn't a change");
        let later = t0 + Duration::from_secs(6);
        assert!(!idle.observe(1, s(0.0), false, 0, later));
        assert!(idle.quiet_for(later) >= Duration::from_secs(6));
        // An edit, a move of the playhead, playing: all wake it.
        assert!(idle.observe(2, s(0.0), false, 0, later));
        assert_eq!(idle.quiet_for(later), Duration::ZERO);
        assert!(idle.observe(2, s(1.0), false, 0, later + Duration::from_secs(9)));
        assert!(idle.observe(2, s(1.0), true, 0, later + Duration::from_secs(10)));
    }
}
