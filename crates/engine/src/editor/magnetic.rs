//! The magnetic timeline: with it on (per sequence), the main track (the
//! first video track) keeps itself closed up. Deleting or cutting clips
//! from it closes the gaps they leave, and trimming its clips ripples: the
//! clips after them (on every track, so what's laid over them stays in
//! sync) move up or along. Other tracks edit as usual, and gaps that were
//! already there stay.

use super::Editor;
use reelforge_core::edit::{ripple_delete, TrimMode};
use reelforge_core::undo::{delete_clips, Batch, Command};
use reelforge_core::{ClipId, Sequence, Time, TrackKind};

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
}
