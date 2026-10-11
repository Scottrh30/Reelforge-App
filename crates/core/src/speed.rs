//! Speed changes: slow motion, fast motion, reverse and freeze frames.
//!
//! A clip's own time ("clip time", what keyframes are anchored to) always
//! runs at the timeline's pace. Its speed maps clip time to media time:
//! `media = anchor + rate × clip time`, where the rate is the speed, negative
//! when reversed, and 0 for a freeze frame. Normal clips have rate 1 and
//! anchor 0, so clip time *is* media time for them.

use crate::project::{scale, Clip, ClipId, Project};
use crate::time::Time;
use crate::undo::{split_group, AddClip, Batch, Command, MoveClip};

/// One clip's speed settings and timing, for undo.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Retime {
    pub speed: f32,
    pub reverse: bool,
    pub anchor: Time,
    pub duration: Time,
}

impl Retime {
    fn of(c: &Clip) -> Self {
        Self { speed: c.speed, reverse: c.reverse, anchor: c.anchor, duration: c.duration }
    }
}

pub struct SetRetime {
    pub seq: usize,
    pub id: ClipId,
    pub old: Retime,
    pub new: Retime,
}

impl Command for SetRetime {
    fn name(&self) -> &str {
        "Change speed"
    }
    fn apply(&mut self, p: &mut Project) {
        if let Some((_, c)) = p.sequences[self.seq].find_clip_mut(self.id) {
            (c.speed, c.reverse, c.anchor, c.duration) = (self.new.speed, self.new.reverse, self.new.anchor, self.new.duration);
        }
    }
    fn undo(&mut self, p: &mut Project) {
        if let Some((_, c)) = p.sequences[self.seq].find_clip_mut(self.id) {
            (c.speed, c.reverse, c.anchor, c.duration) = (self.old.speed, self.old.reverse, self.old.anchor, self.old.duration);
        }
    }
}

/// The smallest step of time, used to keep a reversed clip's first frame
/// inside its media (spans are end-exclusive).
const TICK: Time = Time(1);

/// The media range `[lo, hi)` a clip plays (lo == hi for a freeze frame).
pub fn media_span(c: &Clip) -> (Time, Time) {
    let (a, b) = (c.media_time(c.source_in), c.media_time(c.source_in + c.duration));
    match c.rate() {
        r if r > 0.0 => (a, b),
        r if r < 0.0 => (b + TICK, a + TICK),
        _ => (a, a),
    }
}

/// What a clip becomes with a new speed / direction: it keeps playing the
/// same stretch of media (so a slower clip gets longer), starting at the same
/// clip time, so its keyframes stay where they are on the timeline. A freeze
/// frame (speed 0) holds the frame at the clip's start, at the same length.
pub fn retimed(c: &Clip, speed: f32, reverse: bool, frame: Time) -> Retime {
    let speed = speed.max(0.0);
    let (lo, hi) = media_span(c);
    let old = c.speed.max(0.0);
    if speed == 0.0 {
        return Retime { speed, reverse: false, anchor: c.media_time(c.source_in), duration: c.duration };
    }
    // A freeze frame turning back into motion plays on from its frame for as long as it was.
    let (lo, hi) = if old == 0.0 { (lo, lo + scale(c.duration, speed as f64)) } else { (lo, hi) };
    let duration = scale(hi - lo, 1.0 / speed as f64).max(frame);
    let f = frame.0.max(1);
    let duration = Time((duration.0 + f - 1) / f * f); // whole frames
    let cs = c.source_in;
    let anchor = if reverse { hi - TICK + scale(cs, speed as f64) } else { lo - scale(cs, speed as f64) };
    Retime { speed, reverse, anchor, duration }
}

/// Change the speed of `id` and the clips linked to it. When the clips get
/// longer, the clips after them on their tracks move later to make room.
pub fn change_speed(p: &Project, seq: usize, id: ClipId, speed: f32, reverse: bool) -> Result<Box<dyn Command>, &'static str> {
    let s = &p.sequences[seq];
    if s.is_locked(id) {
        return Err("the clip is on a locked track");
    }
    let frame = s.frame_rate.frame_duration();
    let group = s.link_group(id);
    if group.iter().all(|(_, c)| p.is_still(c.media)) {
        return Err("still images and titles have no speed to change");
    }
    let mut cmds: Vec<Box<dyn Command>> = Vec::new();
    let mut ripple: Vec<(usize, Time, Time)> = Vec::new(); // (track, from, by)
    for (ti, c) in &group {
        if p.is_still(c.media) {
            continue;
        }
        let new = retimed(c, speed, reverse, frame);
        let grow = new.duration - c.duration;
        if grow > Time::ZERO {
            ripple.push((*ti, c.timeline_end(), grow));
        }
        cmds.push(Box::new(SetRetime { seq, id: c.id, old: Retime::of(c), new }));
    }
    let ids: Vec<ClipId> = group.iter().map(|(_, c)| c.id).collect();
    for (ti, from, by) in ripple {
        let mut later: Vec<&Clip> = s.tracks[ti].clips.iter().filter(|o| !ids.contains(&o.id) && o.timeline_start >= from).collect();
        later.sort_by_key(|o| std::cmp::Reverse(o.timeline_start));
        for o in later {
            cmds.push(Box::new(MoveClip { seq, id: o.id, from: (ti, o.timeline_start), to: (ti, o.timeline_start + by) }));
        }
    }
    Ok(Box::new(Batch { name: "Change speed".into(), cmds }))
}

/// Insert a freeze frame of `len` at `at` in video clip `id`: the clip is
/// split there, the frame at `at` is held for `len`, and the rest of the clip
/// (and everything after it on its tracks, linked audio included) moves later.
pub fn freeze_frame(p: &mut Project, seq: usize, id: ClipId, at: Time, len: Time) -> Result<(Box<dyn Command>, ClipId), &'static str> {
    let s = &p.sequences[seq];
    if s.is_locked(id) {
        return Err("the clip is on a locked track");
    }
    let Some((track, c)) = s.find_clip(id) else { return Err("clip not found") };
    if !(c.timeline_start <= at && at < c.timeline_end()) {
        return Err("put the playhead over the clip first");
    }
    if p.is_still(c.media) {
        return Err("it's already a still");
    }
    let c = c.clone();
    let tracks: Vec<usize> = s.link_group(id).iter().map(|(t, _)| *t).collect();

    // Work on a copy to see the clips after the split.
    let mut sim = p.clone();
    let mut cmds: Vec<Box<dyn Command>> = Vec::new();
    if at > c.timeline_start {
        let mut split = split_group(p, seq, id, at).ok_or("can't split the clip there")?;
        split.apply(&mut sim);
        cmds.push(split);
    }
    let q = &sim.sequences[seq];
    for &ti in &tracks {
        let mut later: Vec<&Clip> = q.tracks[ti].clips.iter().filter(|o| o.timeline_start >= at).collect();
        later.sort_by_key(|o| std::cmp::Reverse(o.timeline_start));
        for o in later {
            cmds.push(Box::new(MoveClip { seq, id: o.id, from: (ti, o.timeline_start), to: (ti, o.timeline_start + len) }));
        }
    }
    let fid = ClipId(p.alloc_id());
    let mut freeze = c.clone();
    freeze.id = fid;
    freeze.timeline_start = at;
    freeze.duration = len;
    freeze.source_in = c.source_time(at);
    (freeze.speed, freeze.reverse, freeze.anchor) = (0.0, false, c.media_at(at));
    freeze.link = None;
    freeze.dissolve_in = Time::ZERO;
    freeze.tracker = None;
    cmds.push(Box::new(AddClip { seq, track, clip: freeze }));
    Ok((Box::new(Batch { name: "Freeze frame".into(), cmds }), fid))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::{MediaAsset, MediaId, MediaInfo};
    use crate::undo::UndoStack;

    fn s(x: f64) -> Time {
        Time::from_secs_f64(x)
    }

    fn project() -> Project {
        let mut p = Project::new(); // V1 V2 A1, 30 fps
        for _ in 0..100 {
            p.alloc_id();
        }
        p.media.push(MediaAsset { id: MediaId(1), path: "a.mp4".into(), name: "a".into(), info: MediaInfo { duration: s(20.0), video: None, audio: None, container: String::new() } });
        let q = &mut p.sequences[0];
        // a: 0-4 s on V1 from 2 s into the media, linked to its audio; b: 4-6 s after it.
        q.tracks[0].clips.push(Clip::new(ClipId(1), MediaId(1), s(0.0), s(2.0), s(4.0), Some(9)));
        q.tracks[2].clips.push(Clip::new(ClipId(2), MediaId(1), s(0.0), s(2.0), s(4.0), Some(9)));
        q.tracks[0].clips.push(Clip::new(ClipId(3), MediaId(1), s(4.0), s(10.0), s(2.0), None));
        p
    }

    fn close(a: Time, b: Time) -> bool {
        (a.0 - b.0).abs() <= 2
    }

    #[test]
    fn slow_fast_and_reverse_keep_the_same_media() {
        let mut p = project();
        let mut u = UndoStack::default();
        // Half speed: twice as long, same media (2-6 s), and b moves 4 s later.
        let cmd = change_speed(&p, 0, ClipId(1), 0.5, false).unwrap();
        u.execute(&mut p, cmd);
        let q = &p.sequences[0];
        let a = q.find_clip(ClipId(1)).unwrap().1;
        assert_eq!(a.duration, s(8.0));
        assert!(close(a.media_at(s(0.0)), s(2.0)) && close(a.media_at(s(4.0)), s(4.0)) && close(a.media_at(s(8.0)), s(6.0)));
        assert_eq!(q.find_clip(ClipId(2)).unwrap().1.duration, s(8.0), "the linked audio too");
        assert_eq!(q.find_clip(ClipId(3)).unwrap().1.timeline_start, s(8.0), "b moved out of the way");
        // Reverse at double speed: 2 s long, from the end of the media back.
        let cmd = change_speed(&p, 0, ClipId(1), 2.0, true).unwrap();
        u.execute(&mut p, cmd);
        let a = p.sequences[0].find_clip(ClipId(1)).unwrap().1.clone();
        assert_eq!(a.duration, s(2.0));
        let first = a.media_at(s(0.0));
        assert!(first < s(6.0) && first > s(5.99), "starts at the last frame: {first}");
        assert!(close(a.media_at(s(1.0)), s(4.0)));
        assert_eq!(media_span(&a), (s(2.0), s(6.0)));
        // It can be trimmed out to the start of the media (14 s of extra media at the
        // far end, which plays first: 2 s of it per timeline second).
        let (lo, hi) = p.clip_limits(&a);
        assert!(close(a.source_in - lo, s(7.0)), "{lo} {hi}");
        assert!(close(hi - (a.source_in + a.duration), s(1.0)));
        // A timeline time for a media time, and back.
        assert!(close(a.timeline_of_media(s(4.0)).unwrap(), s(1.0)));
        // Undo all the way.
        u.undo(&mut p);
        u.undo(&mut p);
        let a = p.sequences[0].find_clip(ClipId(1)).unwrap().1;
        assert_eq!((a.speed, a.reverse, a.duration, a.media_at(s(1.0))), (1.0, false, s(4.0), s(3.0)));
        assert_eq!(p.sequences[0].find_clip(ClipId(3)).unwrap().1.timeline_start, s(4.0));
    }

    #[test]
    fn splitting_a_reversed_clip_keeps_playing_on() {
        let mut p = project();
        let mut u = UndoStack::default();
        let cmd = change_speed(&p, 0, ClipId(1), 1.0, true).unwrap();
        u.execute(&mut p, cmd);
        let before: Vec<Time> = (0..4).map(|i| p.sequences[0].find_clip(ClipId(1)).unwrap().1.media_at(s(i as f64 + 0.5))).collect();
        let cmd = split_group(&mut p, 0, ClipId(1), s(2.0)).unwrap();
        u.execute(&mut p, cmd);
        let q = &p.sequences[0];
        let after: Vec<Time> = (0..4).map(|i| q.tracks[0].clip_at(s(i as f64 + 0.5)).unwrap().media_at(s(i as f64 + 0.5))).collect();
        assert_eq!(before, after);
    }

    #[test]
    fn freeze_frames_hold_and_push_the_rest_later() {
        let mut p = project();
        let mut u = UndoStack::default();
        let (cmd, fid) = freeze_frame(&mut p, 0, ClipId(1), s(1.0), s(2.0)).unwrap();
        u.execute(&mut p, cmd);
        let q = &p.sequences[0];
        let f = q.find_clip(fid).unwrap().1;
        assert_eq!((f.timeline_start, f.duration, f.speed), (s(1.0), s(2.0), 0.0));
        assert_eq!((f.media_at(s(1.0)), f.media_at(s(2.9))), (s(3.0), s(3.0)), "holds the frame at 1 s (3 s into the media)");
        // The rest of a starts after the freeze and carries on from 3 s.
        let rest = q.tracks[0].clip_at(s(3.0)).unwrap();
        assert_eq!((rest.timeline_start, rest.media_at(s(3.0))), (s(3.0), s(3.0)));
        assert_eq!(q.find_clip(ClipId(3)).unwrap().1.timeline_start, s(6.0));
        // The audio is split and moved the same way; silence under the freeze.
        assert!(q.tracks[2].clip_at(s(2.0)).is_none());
        assert_eq!(q.tracks[2].clip_at(s(3.5)).unwrap().media_at(s(3.5)), s(3.5));
        // Trimming a freeze frame is unlimited (it's a still now).
        assert_eq!(p.clip_limits(f).0, Time(-(i64::MAX / 4)));
        u.undo(&mut p);
        assert_eq!(p.sequences[0].tracks.iter().map(|t| t.clips.len()).sum::<usize>(), 3);
    }
}
