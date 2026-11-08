//! The magnetic timeline: with it on (per sequence), the main track (the
//! first video track) keeps itself closed up. Deleting or cutting clips
//! from it closes the gaps they leave, and trimming its clips ripples: the
//! clips after them (on every track, so what's laid over them stays in
//! sync) move up or along. Moving clips keeps it closed too: a clip
//! dragged off it leaves no gap, one dropped onto it is inserted at the
//! nearest cut (pushing the rest along), and one dragged along it is
//! reordered. Other tracks edit as usual, and gaps that were already there stay.

use super::Editor;
use reelforge_core::edit::{ripple_delete, TrimMode};
use reelforge_core::undo::{delete_clips, Batch, Command};
use reelforge_core::{Clip, ClipId, Command as _, Sequence, Time, Track, TrackKind};

/// The main track: the first video track.
pub fn main_track(s: &Sequence) -> Option<usize> {
    s.tracks.iter().position(|t| t.kind == TrackKind::Video)
}

impl Editor {
    /// Turn the magnetic timeline on or off for the sequence being edited.
    pub fn set_magnetic(&mut self, on: bool) {
        let seq = self.active;
        if self.project.sequences[seq].magnetic != on {
            self.project.sequences[seq].magnetic = on;
            self.touch();
        }
        self.status = if on { "Magnetic timeline: trims and deletes on the main track close up the gaps".into() } else { "Magnetic timeline off".into() };
    }

    /// Is `id` on the main track of a magnetic sequence?
    pub(crate) fn magnetic_clip(&self, id: ClipId) -> bool {
        let s = self.seq();
        s.magnetic && main_track(s).is_some_and(|m| s.find_clip(id).is_some_and(|(t, _)| t == m))
    }

    /// The trim mode to use for `id`: a plain trim on the magnetic main track ripples.
    pub(crate) fn magnetic_mode(&self, id: ClipId, mode: TrimMode) -> TrimMode {
        if mode == TrimMode::Normal && self.magnetic_clip(id) { TrimMode::Ripple } else { mode }
    }

    /// Moving clip `lead` (and what's linked to it) to `track` at `start` in a
    /// magnetic sequence, when the main track is involved: lifted off it
    /// with the gap closed, and/or inserted into it at the nearest cut.
    /// `None` when it's an ordinary move (neither from nor to the main
    /// track, other clips selected too, or not magnetic).
    pub(crate) fn magnetic_move(&self, lead: ClipId, selection: &[ClipId], track: usize, start: Time) -> Option<Result<(Box<dyn Command>, &'static str), &'static str>> {
        let seq = self.active;
        let s = self.seq();
        let main = main_track(s)?;
        if !s.magnetic {
            return None;
        }
        let (from, _) = s.find_clip(lead)?;
        let group: Vec<ClipId> = s.link_group(lead).iter().map(|(_, c)| c.id).collect();
        if selection.iter().any(|id| !group.contains(id)) || s.tracks[from].kind != TrackKind::Video {
            return None;
        }
        if from != main && track != main {
            return None;
        }
        if group.iter().any(|id| s.is_locked(*id)) || s.tracks[track].locked {
            return Some(Err("the track is locked"));
        }
        let mut probe = self.project.clone();
        // Where each clip of the group sits relative to the lead.
        let lead_start = s.find_clip(lead)?.1.timeline_start;
        let mut lifted: Vec<(usize, Clip)> = group.iter().filter_map(|id| s.find_clip(*id).map(|(t, c)| (t, c.clone()))).collect();
        // Off the main track: remove it there, closing the gap.
        if from == main {
            let (mut cmd, _) = ripple_delete(&probe, seq, lead)?;
            cmd.apply(&mut probe);
        } else {
            for id in &group {
                for t in &mut probe.sequences[seq].tracks {
                    t.clips.retain(|c| c.id != *id);
                }
            }
        }
        let ps = &mut probe.sequences[seq];
        let len = lifted.iter().find(|(_, c)| c.id == lead).map(|(_, c)| c.duration)?;
        let what: &'static str;
        let at = if track == main {
            // Into the main track at the nearest cut (never past its end).
            let clips = &ps.tracks[main].clips;
            let end = clips.iter().map(|c| c.timeline_end()).max().unwrap_or(Time::ZERO);
            // A drop later along the same track counts in the timeline as it
            // was while dragging: the lifted clip's own length further on.
            let start = if from == main && start > lead_start { start - len } else { start };
            let mut t = start.min(end).max(Time::ZERO);
            if let Some(c) = clips.iter().find(|c| c.timeline_start < t && t < c.timeline_end()) {
                t = if (t - c.timeline_start).0 < (c.timeline_end() - t).0 { c.timeline_start } else { c.timeline_end() };
            }
            // Everything from there on moves along to make room.
            for (ti, tr) in ps.tracks.iter_mut().enumerate() {
                if tr.locked {
                    continue;
                }
                for c in &mut tr.clips {
                    if c.timeline_start >= t {
                        c.timeline_start = c.timeline_start + len;
                    }
                }
                let _ = ti;
            }
            what = if from == main { "Reordered on the main track" } else { "Inserted into the main track" };
            t
        } else {
            what = "Lifted off the main track, closing the gap";
            start.max(Time::ZERO)
        };
        // Put the group back: the lead on its new track, linked clips on theirs, keeping their offsets.
        for (t, c) in &mut lifted {
            let dest = if c.id == lead { track } else { *t };
            c.timeline_start = at + (c.timeline_start - lead_start);
            if c.timeline_start < Time::ZERO || ps.overlaps(dest, c.timeline_start, c.duration, &group) {
                return Some(Err("there's no room there"));
            }
            *t = dest;
        }
        for (t, c) in lifted {
            ps.tracks[t].clips.push(c);
            ps.tracks[t].sort();
        }
        let new = ps.tracks.clone();
        let old = self.seq().tracks.clone();
        Some(Ok((Box::new(ReplaceTracks { seq, old, new, name: "Move clip (magnetic)" }), what)))
    }

    /// Deleting `ids` in a magnetic sequence: the main track's clips go with
    /// their gaps closed (latest first, so each gap is where it was), the
    /// rest plainly. `None` when none of them are on the main track (or it
    /// isn't magnetic). Also returns how much time was closed up.
    pub(crate) fn magnetic_delete(&self, ids: &[ClipId]) -> Option<(Box<dyn Command>, Time)> {
        let seq = self.active;
        let mut main: Vec<(Time, ClipId)> = ids.iter().filter(|id| self.magnetic_clip(**id)).filter_map(|id| self.seq().find_clip(*id).map(|(_, c)| (c.timeline_start, c.id))).collect();
        if main.is_empty() {
            return None;
        }
        main.sort_by(|a, b| b.cmp(a));
        let mut probe = self.project.clone();
        let mut cmds: Vec<Box<dyn Command>> = Vec::new();
        let mut closed = Time::ZERO;
        for (_, id) in main {
            if probe.sequences[seq].find_clip(id).is_none() {
                continue; // went with a clip linked to it
            }
            if let Some((mut cmd, by)) = ripple_delete(&probe, seq, id) {
                cmd.apply(&mut probe);
                cmds.push(cmd);
                closed = closed + by;
            }
        }
        let rest: Vec<ClipId> = ids.iter().copied().filter(|id| probe.sequences[seq].find_clip(*id).is_some()).collect();
        if !rest.is_empty() {
            let mut cmd = delete_clips(&probe, seq, &rest);
            cmd.apply(&mut probe);
            cmds.push(cmd);
        }
        Some((Box::new(Batch { name: "Delete (magnetic)".into(), cmds }), closed))
    }
}

/// The whole of a sequence's tracks before and after an edit (for the
/// magnetic moves, which shift many clips at once).
struct ReplaceTracks {
    seq: usize,
    old: Vec<Track>,
    new: Vec<Track>,
    name: &'static str,
}

impl Command for ReplaceTracks {
    fn name(&self) -> &str {
        self.name
    }
    fn apply(&mut self, p: &mut reelforge_core::Project) {
        p.sequences[self.seq].tracks = self.new.clone();
    }
    fn undo(&mut self, p: &mut reelforge_core::Project) {
        p.sequences[self.seq].tracks = self.old.clone();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reelforge_core::undo::Edge;
    use reelforge_core::{Clip, MediaId};

    fn s(x: f64) -> Time {
        Time::from_secs_f64(x)
    }

    /// Three 2 s clips back to back on the main track, a gap, a fourth;
    /// a title over the third on Video 2 and a sound under the first.
    fn editor() -> (Editor, [ClipId; 6]) {
        let mut e = Editor::new(None, None);
        e.project.media.push(reelforge_core::MediaAsset {
            id: MediaId(1),
            path: "/a.mp4".into(),
            name: "a.mp4".into(),
            info: reelforge_core::MediaInfo { duration: s(100.0), video: None, audio: None, container: String::new() },
            conform: None,
        });
        let ids = [1, 2, 3, 4, 5, 6].map(ClipId);
        let seq = &mut e.project.sequences[0];
        seq.tracks[0].clips = vec![
            Clip::new(ids[0], MediaId(1), s(0.0), s(10.0), s(2.0), None),
            Clip::new(ids[1], MediaId(1), s(2.0), s(20.0), s(2.0), None),
            Clip::new(ids[2], MediaId(1), s(4.0), s(30.0), s(2.0), None),
            Clip::new(ids[3], MediaId(1), s(8.0), s(40.0), s(2.0), None),
        ];
        seq.tracks[1].clips = vec![Clip::new(ids[4], MediaId(1), s(4.5), s(50.0), s(1.0), None)];
        seq.tracks[2].clips = vec![Clip::new(ids[5], MediaId(1), s(0.0), s(60.0), s(2.0), None)];
        (e, ids)
    }

    fn starts(e: &Editor) -> Vec<Vec<f64>> {
        e.seq().tracks.iter().map(|t| t.clips.iter().map(|c| (c.timeline_start.as_secs_f64() * 100.0).round() / 100.0).collect()).collect()
    }

    #[test]
    fn deletes_and_trims_close_up_the_main_track() {
        let (mut e, ids) = editor();
        // Off: a delete leaves its gap.
        e.select_many(vec![ids[1]]);
        e.delete_selection();
        assert_eq!(starts(&e)[0], [0.0, 4.0, 8.0]);
        e.undo();

        e.set_magnetic(true);
        // Deleting two main-track clips (and one elsewhere) closes both gaps;
        // what's over them moves along, the gap that was there stays.
        e.select_many(vec![ids[0], ids[1], ids[5]]);
        assert!(e.delete_selection());
        assert_eq!(starts(&e), [vec![0.0, 4.0], vec![0.5], vec![]]);
        assert!(e.status.contains("closed"), "{}", e.status);
        e.undo();
        assert_eq!(starts(&e)[0], [0.0, 2.0, 4.0, 8.0]);

        // A trim on the main track ripples, both ways.
        e.trim_live(ids[0], Edge::End, TrimMode::Normal, s(-1.0));
        e.commit_live_edit();
        assert_eq!(starts(&e), [vec![0.0, 1.0, 3.0, 7.0], vec![3.5], vec![0.0]]);
        e.trim_live(ids[1], Edge::End, TrimMode::Normal, s(1.0));
        e.commit_live_edit();
        assert_eq!(starts(&e)[0], [0.0, 1.0, 4.0, 8.0], "lengthening pushes the rest along");
        // Elsewhere a trim is a trim.
        e.trim_live(ids[4], Edge::End, TrimMode::Normal, s(-0.5));
        e.commit_live_edit();
        assert_eq!(starts(&e)[0], [0.0, 1.0, 4.0, 8.0]);
    }

    /// Moving on a magnetic main track: reorder (dropped mid-clip, it goes
    /// to the nearer cut), lift off (the gap closes), drop on (inserted,
    /// the rest pushed along). Off the main track, a move is a move.
    #[test]
    fn moves_keep_the_main_track_closed() {
        let (mut e, ids) = editor();
        e.set_magnetic(true);
        let main_order = |e: &Editor| e.seq().tracks[0].clips.iter().map(|c| (c.id.0, (c.timeline_start.as_secs_f64() * 100.0).round() / 100.0)).collect::<Vec<_>>();
        // The first clip dropped at 3.1 s, past the middle of the second: after it.
        e.move_clips(ids[0], 0, s(3.1));
        assert_eq!(main_order(&e), [(2, 0.0), (1, 2.0), (3, 4.0), (4, 8.0)]);
        e.undo();
        assert_eq!(main_order(&e), [(1, 0.0), (2, 2.0), (3, 4.0), (4, 8.0)]);
        // Lifted up onto Video 2 at 9 s: the main track closes up behind it.
        e.move_clips(ids[1], 1, s(9.0));
        assert_eq!(main_order(&e), [(1, 0.0), (3, 2.0), (4, 6.0)]);
        assert!(e.seq().tracks[1].clips.iter().any(|c| c.id == ids[1] && c.timeline_start == s(9.0)));
        // …and dropped back in at 1.2 s: inserted at the cut at 2 s, the rest pushed along.
        e.move_clips(ids[1], 0, s(2.3));
        assert_eq!(main_order(&e), [(1, 0.0), (2, 2.0), (3, 4.0), (4, 8.0)]);
        // A drop past the end lands at the end, with no gap.
        e.move_clips(ids[0], 0, s(30.0));
        assert_eq!(main_order(&e).last(), Some(&(1, 8.0)));
        // Off the main track nothing magnetic happens.
        let before = main_order(&e);
        e.move_clips(ids[4], 1, s(12.0));
        assert_eq!(main_order(&e), before);
    }
}
