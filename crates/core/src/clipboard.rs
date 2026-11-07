//! Copy and paste of clips, within a sequence or between sequences (e.g.
//! from the main edit into a composition).
//!
//! Copied clips keep their positions relative to each other in time, and
//! which track they were on is remembered as "the n-th video track" or "the
//! n-th audio track", so pasting into a sequence with a different track
//! layout still lands them in the same stacking order (tracks are added if
//! there aren't enough). Pasted clips get new ids and new links, and every
//! property (keyframes, effects, masks, titles, trackers…) comes along.

use crate::project::{Clip, ClipId, MediaId, Project, TrackKind};
use crate::time::Time;
use crate::undo::{expand_links, AddClip, AddTrack, Batch, Command};
use std::collections::HashMap;

/// One copied clip: which track (by kind and rank among tracks of that kind),
/// and the clip with its start time relative to the earliest copied clip.
#[derive(Clone, Debug)]
pub struct Copied {
    pub kind: TrackKind,
    pub rank: usize,
    pub clip: Clip,
}

#[derive(Clone, Debug, Default)]
pub struct Clipboard {
    pub clips: Vec<Copied>,
}

impl Clipboard {
    pub fn is_empty(&self) -> bool {
        self.clips.is_empty()
    }
    pub fn len(&self) -> usize {
        self.clips.len()
    }
}

/// Copy `ids` (and what's linked to them) from sequence `seq`.
pub fn copy_clips(p: &Project, seq: usize, ids: &[ClipId]) -> Clipboard {
    let s = &p.sequences[seq];
    let all = expand_links(p, seq, ids);
    let Some(t0) = all.iter().filter_map(|id| s.find_clip(*id)).map(|(_, c)| c.timeline_start).min() else { return Clipboard::default() };
    let rank = |t: usize| (0..t).filter(|&i| s.tracks[i].kind == s.tracks[t].kind).count();
    let clips = all
        .iter()
        .filter_map(|id| s.find_clip(*id))
        .map(|(t, c)| {
            let mut clip = c.clone();
            clip.timeline_start = clip.timeline_start - t0;
            Copied { kind: s.tracks[t].kind, rank: rank(t), clip }
        })
        .collect();
    Clipboard { clips }
}

/// What a paste will do.
pub struct Paste {
    pub cmd: Box<dyn Command>,
    /// The new clips' ids.
    pub ids: Vec<ClipId>,
    /// Where they start (later than asked if something was in the way).
    pub at: Time,
    /// Clips left out (compositions that would contain themselves, missing media).
    pub skipped: usize,
}

/// Paste `cb` into sequence `seq` at `at`, or at the first time after it where
/// everything fits. Allocates ids, hence `&mut`.
pub fn paste_clips(p: &mut Project, seq: usize, cb: &Clipboard, at: Time) -> Option<Paste> {
    paste_clips_onto(p, seq, cb, at, None)
}

/// [`paste_clips`], with the copied clips of `target`'s kind moved up or down
/// the stack together so the lowest of them lands on track `target` (the
/// clips keep their places relative to each other; the other kind stays put).
pub fn paste_clips_onto(p: &mut Project, seq: usize, cb: &Clipboard, at: Time, target: Option<usize>) -> Option<Paste> {
    let shifted;
    let cb = match target.and_then(|t| p.sequences.get(seq)?.tracks.get(t).map(|tr| (t, tr.kind))) {
        Some((t, kind)) => {
            let s = &p.sequences[seq];
            let target_rank = (0..t).filter(|&i| s.tracks[i].kind == kind).count() as isize;
            match cb.clips.iter().filter(|c| c.kind == kind).map(|c| c.rank).min() {
                Some(lowest) => {
                    let by = target_rank - lowest as isize;
                    let mut c = cb.clone();
                    for x in c.clips.iter_mut().filter(|x| x.kind == kind) {
                        x.rank = (x.rank as isize + by).max(0) as usize;
                    }
                    shifted = c;
                    &shifted
                }
                None => cb,
            }
        }
        None => cb,
    };
    // Leave out what can't go here.
    let usable = |c: &Clip| match c.nested {
        Some(n) => n < p.sequences.len() && !p.contains_sequence(n, seq),
        None => c.media == MediaId::GENERATED || p.media(c.media).is_some(),
    };
    let items: Vec<&Copied> = cb.clips.iter().filter(|c| usable(&c.clip)).collect();
    let skipped = cb.clips.len() - items.len();
    if items.is_empty() {
        return None;
    }

    // Work out the track layout on a copy, adding tracks where needed.
    let mut sim = p.clone();
    let mut cmds: Vec<Box<dyn Command>> = Vec::new();
    for kind in [TrackKind::Video, TrackKind::Audio] {
        let need = items.iter().filter(|c| c.kind == kind).map(|c| c.rank + 1).max().unwrap_or(0);
        while sim.sequences[seq].tracks.iter().filter(|t| t.kind == kind).count() < need {
            let mut add: Box<dyn Command> = Box::new(AddTrack::new(&sim, seq, kind));
            add.apply(&mut sim);
            cmds.push(add);
        }
    }
    let s = &sim.sequences[seq];
    let track_of = |kind: TrackKind, rank: usize| (0..s.tracks.len()).filter(|&i| s.tracks[i].kind == kind).nth(rank).unwrap();
    let placed: Vec<(usize, &Clip)> = items.iter().map(|c| (track_of(c.kind, c.rank), &c.clip)).collect();
    if placed.iter().any(|(t, _)| s.tracks[*t].locked) {
        return None;
    }

    // The first start at or after `at` where nothing overlaps: try `at`, then the
    // end of every clip on the tracks involved.
    let fits = |start: Time| placed.iter().all(|(t, c)| !s.overlaps(*t, start + c.timeline_start, c.duration, &[]));
    let mut candidates: Vec<Time> = placed
        .iter()
        .flat_map(|(t, c)| s.tracks[*t].clips.iter().map(move |o| o.timeline_end() - c.timeline_start))
        .filter(|&x| x > at)
        .collect();
    candidates.sort();
    let start = std::iter::once(at).chain(candidates).find(|&x| fits(x))?;

    // New ids, and new link ids shared the same way as the originals.
    let mut links: HashMap<u64, u64> = HashMap::new();
    let mut ids = Vec::new();
    let mut adds: Vec<Box<dyn Command>> = Vec::new();
    for (track, c) in &placed {
        let mut clip = (*c).clone();
        clip.id = ClipId(p.alloc_id());
        clip.timeline_start = start + c.timeline_start;
        clip.link = c.link.map(|l| *links.entry(l).or_insert_with(|| p.alloc_id()));
        ids.push(clip.id);
        adds.push(Box::new(AddClip { seq, track: *track, clip }));
    }
    cmds.extend(adds);
    let name = if ids.len() > 1 { "Paste clips" } else { "Paste clip" };
    Some(Paste { cmd: Box::new(Batch { name: name.into(), cmds }), ids, at: start, skipped })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::Sequence;
    use crate::time::FrameRate;
    use crate::undo::{move_clips, UndoStack};

    fn s(x: f64) -> Time {
        Time::from_secs_f64(x)
    }

    fn project() -> Project {
        let mut p = Project::new(); // V1 V2 A1
        for _ in 0..100 {
            p.alloc_id();
        }
        p.media.push(crate::project::MediaAsset {
            id: MediaId(1),
            path: "a.mp4".into(),
            name: "a".into(),
            info: crate::project::MediaInfo { duration: s(100.0), video: None, audio: None, container: String::new() },
            conform: None,
        });
        let q = &mut p.sequences[0];
        // a: V1 0-2 linked to its audio on A1; b: V2 1-3.
        q.tracks[0].clips.push(Clip::new(ClipId(1), MediaId(1), s(0.0), s(0.0), s(2.0), Some(9)));
        q.tracks[2].clips.push(Clip::new(ClipId(2), MediaId(1), s(0.0), s(0.0), s(2.0), Some(9)));
        q.tracks[1].clips.push(Clip::new(ClipId(3), MediaId(1), s(1.0), s(0.0), s(2.0), None));
        p
    }

    #[test]
    fn copy_and_paste_keeps_layout_and_relinks() {
        let mut p = project();
        let cb = copy_clips(&p, 0, &[ClipId(1), ClipId(3)]);
        assert_eq!(cb.len(), 3, "the linked audio comes along");
        let paste = paste_clips(&mut p, 0, &cb, s(10.0)).unwrap();
        assert_eq!((paste.at, paste.ids.len(), paste.skipped), (s(10.0), 3, 0));
        let mut u = UndoStack::default();
        u.execute(&mut p, paste.cmd);
        let q = &p.sequences[0];
        let (t, v) = q.find_clip(paste.ids[0]).unwrap();
        assert_eq!((t, v.timeline_start), (0, s(10.0)));
        let (t, b) = q.find_clip(paste.ids[2]).unwrap();
        assert_eq!((t, b.timeline_start), (1, s(11.0)), "same track and offset as the original");
        // The pasted video and audio are linked to each other, not to the originals.
        let group: Vec<ClipId> = q.link_group(v.id).iter().map(|(_, c)| c.id).collect();
        assert_eq!(group.len(), 2);
        assert!(!group.contains(&ClipId(1)));

        // Pasting where it doesn't fit moves on to the first free spot.
        let paste = paste_clips(&mut p, 0, &cb, s(0.5)).unwrap();
        // At 2 s, a fits after the original a, and the copy of b (1 s later) after the original b.
        assert_eq!(paste.at, s(2.0));
        u.undo(&mut p);
        assert_eq!(p.sequences[0].tracks.iter().map(|t| t.clips.len()).sum::<usize>(), 3);
    }

    #[test]
    fn pastes_into_another_sequence_adding_tracks() {
        let mut p = project();
        p.sequences.push(Sequence { tracks: vec![crate::project::Track::new("Video 1", TrackKind::Video)], ..Sequence::new("C", 640, 360, FrameRate::new(25, 1)) });
        let cb = copy_clips(&p, 0, &[ClipId(1), ClipId(3)]);
        let paste = paste_clips(&mut p, 1, &cb, Time::ZERO).unwrap();
        let mut u = UndoStack::default();
        u.execute(&mut p, paste.cmd);
        let kinds: Vec<TrackKind> = p.sequences[1].tracks.iter().map(|t| t.kind).collect();
        assert_eq!(kinds, [TrackKind::Video, TrackKind::Video, TrackKind::Audio]);
        assert_eq!(p.sequences[1].tracks.iter().map(|t| t.clips.len()).collect::<Vec<_>>(), [1, 1, 1]);
        u.undo(&mut p);
        assert_eq!(p.sequences[1].tracks.len(), 1, "the added tracks go too");
        // A composition can't be pasted into itself.
        p.sequences[0].tracks[1].clips.push(Clip::new_nested(ClipId(50), 1, s(20.0), s(1.0), None));
        let cb = copy_clips(&p, 0, &[ClipId(50)]);
        assert!(paste_clips(&mut p, 1, &cb, Time::ZERO).is_none());
    }

    #[test]
    fn moves_a_selection_together() {
        let mut p = project();
        let mut u = UndoStack::default();
        // Drag b (V2) right by 4 s with a selected too: a and its audio follow.
        let cmd = move_clips(&p, 0, ClipId(3), &[ClipId(1), ClipId(3)], (1, s(5.0))).unwrap();
        u.execute(&mut p, cmd);
        let q = &p.sequences[0];
        assert_eq!(q.find_clip(ClipId(1)).map(|(t, c)| (t, c.timeline_start)), Some((0, s(4.0))));
        assert_eq!(q.find_clip(ClipId(2)).map(|(t, c)| (t, c.timeline_start)), Some((2, s(4.0))));
        // Moving a down one video track: there's no track below V1.
        assert!(move_clips(&p, 0, ClipId(3), &[ClipId(1), ClipId(3)], (0, s(5.0))).is_err());
        // Moving a up to V2 alone works when b isn't in the way.
        let cmd = move_clips(&p, 0, ClipId(1), &[ClipId(1)], (1, s(10.0))).unwrap();
        u.execute(&mut p, cmd);
        assert_eq!(p.sequences[0].find_clip(ClipId(1)).map(|(t, _)| t), Some(1));
        assert_eq!(p.sequences[0].find_clip(ClipId(2)).map(|(t, _)| t), Some(2), "the audio stays on its track");
    }
}
