//! Command pattern undo/redo. Every edit to the project goes through here.

use crate::project::*;
use crate::audio::{ClipAudio, TrackAudio};
use crate::time::Time;
use crate::transform::{BlendMode, Transform};

pub trait Command: Send {
    fn name(&self) -> &str;
    fn apply(&mut self, project: &mut Project);
    fn undo(&mut self, project: &mut Project);
}

#[derive(Default)]
pub struct UndoStack {
    done: Vec<Box<dyn Command>>,
    undone: Vec<Box<dyn Command>>,
}

impl UndoStack {
    pub fn execute(&mut self, project: &mut Project, mut cmd: Box<dyn Command>) {
        cmd.apply(project);
        self.done.push(cmd);
        self.undone.clear();
    }
    pub fn undo(&mut self, project: &mut Project) -> Option<String> {
        let mut cmd = self.done.pop()?;
        cmd.undo(project);
        let name = cmd.name().to_owned();
        self.undone.push(cmd);
        Some(name)
    }
    pub fn redo(&mut self, project: &mut Project) -> Option<String> {
        let mut cmd = self.undone.pop()?;
        cmd.apply(project);
        let name = cmd.name().to_owned();
        self.done.push(cmd);
        Some(name)
    }
    pub fn can_undo(&self) -> bool {
        !self.done.is_empty()
    }
    pub fn can_redo(&self) -> bool {
        !self.undone.is_empty()
    }
}

// ---- Concrete commands -------------------------------------------------------

/// Several commands that undo/redo as one step.
pub struct Batch {
    pub name: String,
    pub cmds: Vec<Box<dyn Command>>,
}

impl Command for Batch {
    fn name(&self) -> &str {
        &self.name
    }
    fn apply(&mut self, p: &mut Project) {
        for c in &mut self.cmds {
            c.apply(p);
        }
    }
    fn undo(&mut self, p: &mut Project) {
        for c in self.cmds.iter_mut().rev() {
            c.undo(p);
        }
    }
}

pub struct ImportMedia(pub MediaAsset);

impl Command for ImportMedia {
    fn name(&self) -> &str {
        "Import media"
    }
    fn apply(&mut self, p: &mut Project) {
        p.media.push(self.0.clone());
    }
    fn undo(&mut self, p: &mut Project) {
        p.media.retain(|m| m.id != self.0.id);
    }
}

pub struct AddClip {
    pub seq: usize,
    pub track: usize,
    pub clip: Clip,
}

impl Command for AddClip {
    fn name(&self) -> &str {
        "Add clip"
    }
    fn apply(&mut self, p: &mut Project) {
        let tr = &mut p.sequences[self.seq].tracks[self.track];
        tr.clips.push(self.clip.clone());
        tr.sort();
    }
    fn undo(&mut self, p: &mut Project) {
        p.sequences[self.seq].tracks[self.track].clips.retain(|c| c.id != self.clip.id);
    }
}

pub struct RemoveClip {
    pub seq: usize,
    pub id: ClipId,
    removed: Option<(usize, Clip)>,
}

impl RemoveClip {
    pub fn new(seq: usize, id: ClipId) -> Self {
        Self { seq, id, removed: None }
    }
}

impl Command for RemoveClip {
    fn name(&self) -> &str {
        "Delete clip"
    }
    fn apply(&mut self, p: &mut Project) {
        for (ti, tr) in p.sequences[self.seq].tracks.iter_mut().enumerate() {
            if let Some(i) = tr.clips.iter().position(|c| c.id == self.id) {
                self.removed = Some((ti, tr.clips.remove(i)));
                return;
            }
        }
    }
    fn undo(&mut self, p: &mut Project) {
        if let Some((ti, clip)) = self.removed.take() {
            let tr = &mut p.sequences[self.seq].tracks[ti];
            tr.clips.push(clip);
            tr.sort();
        }
    }
}

/// Move a clip in time and/or to another track. Built from a drag's start and end.
pub struct MoveClip {
    pub seq: usize,
    pub id: ClipId,
    pub from: (usize, Time),
    pub to: (usize, Time),
}

impl MoveClip {
    fn place(p: &mut Project, seq: usize, id: ClipId, (track, start): (usize, Time)) {
        let s = &mut p.sequences[seq];
        let Some((cur, _)) = s.find_clip_mut(id) else { return };
        let i = s.tracks[cur].clips.iter().position(|c| c.id == id).unwrap();
        let mut clip = s.tracks[cur].clips.remove(i);
        clip.timeline_start = start;
        s.tracks[track].clips.push(clip);
        s.tracks[track].sort();
    }
}

impl Command for MoveClip {
    fn name(&self) -> &str {
        "Move clip"
    }
    fn apply(&mut self, p: &mut Project) {
        Self::place(p, self.seq, self.id, self.to);
    }
    fn undo(&mut self, p: &mut Project) {
        Self::place(p, self.seq, self.id, self.from);
    }
}

/// Razor: split one clip into two at sequence time `at`.
pub struct SplitClip {
    pub seq: usize,
    pub id: ClipId,
    pub at: Time,
    pub new_id: ClipId,
    /// Link id for the right-hand half, so the halves of linked clips stay linked to each other.
    pub new_link: Option<u64>,
    original: Option<Clip>,
}

impl SplitClip {
    pub fn new(seq: usize, id: ClipId, at: Time, new_id: ClipId) -> Self {
        Self { seq, id, at, new_id, new_link: None, original: None }
    }
}

impl Command for SplitClip {
    fn name(&self) -> &str {
        "Split clip"
    }
    fn apply(&mut self, p: &mut Project) {
        let s = &mut p.sequences[self.seq];
        let Some((ti, clip)) = s.find_clip_mut(self.id) else { return };
        if !clip.contains(self.at) || clip.timeline_start == self.at {
            return;
        }
        self.original = Some(clip.clone());
        let left = self.at - clip.timeline_start;
        let mut right = Clip {
            id: self.new_id,
            media: clip.media,
            timeline_start: self.at,
            source_in: clip.source_in + left,
            duration: clip.duration - left,
            link: clip.link.and(self.new_link),
            transform: clip.transform,
            blend: clip.blend,
            // The fade-in and any crossfade stay on the left half, the fade-out goes
            // right. Volume keys are anchored to source time, so both halves keep them.
            audio: ClipAudio { fade_in: Time::ZERO, crossfade_in: Time::ZERO, ..clip.audio.clone() },
        };
        clip.duration = left;
        clip.audio.fade_out = Time::ZERO;
        clip.audio.fade_in = clip.audio.fade_in.min(left);
        right.audio.fade_out = right.audio.fade_out.min(right.duration);
        s.tracks[ti].clips.push(right);
        s.tracks[ti].sort();
    }
    fn undo(&mut self, p: &mut Project) {
        let Some(orig) = self.original.take() else { return };
        let s = &mut p.sequences[self.seq];
        for tr in &mut s.tracks {
            tr.clips.retain(|c| c.id != self.new_id);
        }
        if let Some((_, c)) = s.find_clip_mut(self.id) {
            *c = orig;
        }
    }
}

/// Change a clip's transform and/or blend mode.
///
/// Interactive edits (dragging a slider or the layer in the viewer) change the
/// clip live, then push one of these with the pre-drag value as `old` when the
/// drag ends, so the whole gesture is a single undo step.
pub struct SetClipVisual {
    pub seq: usize,
    pub id: ClipId,
    pub old: (Transform, BlendMode),
    pub new: (Transform, BlendMode),
}

impl SetClipVisual {
    fn set(&self, p: &mut Project, v: (Transform, BlendMode)) {
        if let Some((_, c)) = p.sequences[self.seq].find_clip_mut(self.id) {
            (c.transform, c.blend) = v;
        }
    }
}

impl Command for SetClipVisual {
    fn name(&self) -> &str {
        if self.old.1 != self.new.1 { "Change blend mode" } else { "Change transform" }
    }
    fn apply(&mut self, p: &mut Project) {
        self.set(p, self.new);
    }
    fn undo(&mut self, p: &mut Project) {
        self.set(p, self.old);
    }
}

/// Change a clip's volume, pan or fades (live-edited like [`SetClipVisual`]).
pub struct SetClipAudio {
    pub seq: usize,
    pub id: ClipId,
    pub old: ClipAudio,
    pub new: ClipAudio,
}

impl Command for SetClipAudio {
    fn name(&self) -> &str {
        if self.old.crossfade_in != self.new.crossfade_in {
            "Change crossfade"
        } else if self.old.volume_keys != self.new.volume_keys {
            "Change volume keyframes"
        } else if (self.old.fade_in, self.old.fade_out) != (self.new.fade_in, self.new.fade_out) {
            "Change fade"
        } else {
            "Change clip audio"
        }
    }
    fn apply(&mut self, p: &mut Project) {
        if let Some((_, c)) = p.sequences[self.seq].find_clip_mut(self.id) {
            c.audio = self.new.clone();
        }
    }
    fn undo(&mut self, p: &mut Project) {
        if let Some((_, c)) = p.sequences[self.seq].find_clip_mut(self.id) {
            c.audio = self.old.clone();
        }
    }
}

/// Change a track's volume or pan.
pub struct SetTrackAudio {
    pub seq: usize,
    pub track: usize,
    pub old: TrackAudio,
    pub new: TrackAudio,
}

impl Command for SetTrackAudio {
    fn name(&self) -> &str {
        "Change track audio"
    }
    fn apply(&mut self, p: &mut Project) {
        p.sequences[self.seq].tracks[self.track].audio = self.new;
    }
    fn undo(&mut self, p: &mut Project) {
        p.sequences[self.seq].tracks[self.track].audio = self.old;
    }
}

/// Set (or clear) the link id of a set of clips.
pub struct SetLink {
    pub seq: usize,
    pub ids: Vec<ClipId>,
    pub link: Option<u64>,
    old: Vec<Option<u64>>,
}

impl SetLink {
    pub fn new(seq: usize, ids: Vec<ClipId>, link: Option<u64>) -> Self {
        Self { seq, ids, link, old: Vec::new() }
    }
}

impl Command for SetLink {
    fn name(&self) -> &str {
        if self.link.is_some() { "Link clips" } else { "Unlink clips" }
    }
    fn apply(&mut self, p: &mut Project) {
        let s = &mut p.sequences[self.seq];
        self.old = self.ids.iter().map(|id| s.find_clip_mut(*id).and_then(|(_, c)| std::mem::replace(&mut c.link, self.link))).collect();
    }
    fn undo(&mut self, p: &mut Project) {
        let s = &mut p.sequences[self.seq];
        for (id, old) in self.ids.iter().zip(&self.old) {
            if let Some((_, c)) = s.find_clip_mut(*id) {
                c.link = *old;
            }
        }
    }
}

// ---- Group operations (respecting links) -------------------------------------

/// Split `id` and everything linked to it at `at`. `None` if nothing can be split there.
pub fn split_group(p: &mut Project, seq: usize, id: ClipId, at: Time) -> Option<Box<dyn Command>> {
    let targets: Vec<ClipId> = p.sequences[seq]
        .link_group(id)
        .into_iter()
        .filter(|(_, c)| c.contains(at) && c.timeline_start < at)
        .map(|(_, c)| c.id)
        .collect();
    if targets.is_empty() {
        return None;
    }
    let new_link = Some(p.alloc_id());
    let cmds = targets
        .into_iter()
        .map(|cid| {
            let mut c = SplitClip::new(seq, cid, at, ClipId(p.alloc_id()));
            c.new_link = new_link;
            Box::new(c) as Box<dyn Command>
        })
        .collect();
    Some(Box::new(Batch { name: "Split clip".into(), cmds }))
}

pub fn delete_group(p: &Project, seq: usize, id: ClipId) -> Box<dyn Command> {
    let cmds = p.sequences[seq]
        .link_group(id)
        .into_iter()
        .map(|(_, c)| Box::new(RemoveClip::new(seq, c.id)) as Box<dyn Command>)
        .collect();
    Box::new(Batch { name: "Delete clip".into(), cmds })
}

/// Move `id` to `(track, start)`; linked clips shift by the same time but stay on their tracks.
pub fn move_group(p: &Project, seq: usize, id: ClipId, to: (usize, Time)) -> Result<Box<dyn Command>, &'static str> {
    let s = &p.sequences[seq];
    let group = s.link_group(id);
    let Some(&(_, lead)) = group.iter().find(|(_, c)| c.id == id) else { return Err("clip not found") };
    let delta = to.1.0 - lead.timeline_start.0;
    let ignore: Vec<ClipId> = group.iter().map(|(_, c)| c.id).collect();
    let mut cmds: Vec<Box<dyn Command>> = Vec::new();
    for (ti, c) in &group {
        let (track, start) = if c.id == id { to } else { (*ti, Time(c.timeline_start.0 + delta)) };
        if start < Time::ZERO {
            return Err("can't move before the start of the timeline");
        }
        if s.tracks[track].kind != s.tracks[*ti].kind {
            return Err("clips can only move to a track of the same kind");
        }
        if s.overlaps(track, start, c.duration, &ignore) {
            return Err("it would overlap another clip");
        }
        if (track, start) != (*ti, c.timeline_start) {
            cmds.push(Box::new(MoveClip { seq, id: c.id, from: (*ti, c.timeline_start), to: (track, start) }));
        }
    }
    Ok(Box::new(Batch { name: "Move clip".into(), cmds }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::Time;
    use crate::audio::{ClipAudio, TrackAudio};
    use crate::transform::{BlendMode, Transform};

    fn clip(id: u64, start: f64, dur: f64) -> Clip {
        Clip::new(ClipId(id), MediaId(1), Time::from_secs_f64(start), Time::ZERO, Time::from_secs_f64(dur), None)
    }

    fn linked_pair(p: &mut Project, u: &mut UndoStack) {
        let mut v = clip(1001, 0.0, 10.0);
        let mut a = clip(1002, 0.0, 10.0);
        v.link = Some(99);
        a.link = Some(99);
        u.execute(p, Box::new(AddClip { seq: 0, track: 0, clip: v }));
        u.execute(p, Box::new(AddClip { seq: 0, track: 2, clip: a }));
    }

    #[test]
    fn linked_split_move_delete() {
        let mut p = Project::new();
        let mut u = UndoStack::default();
        linked_pair(&mut p, &mut u);

        let cmd = split_group(&mut p, 0, ClipId(1001), Time::from_secs_f64(4.0)).unwrap();
        u.execute(&mut p, cmd);
        let s = &p.sequences[0];
        assert_eq!(s.tracks[0].clips.len(), 2);
        assert_eq!(s.tracks[2].clips.len(), 2);
        // Right halves are linked to each other, not to the left halves.
        let (rv, ra) = (&s.tracks[0].clips[1], &s.tracks[2].clips[1]);
        assert_eq!(rv.link, ra.link);
        assert_ne!(rv.link, Some(99));
        let right_video = rv.id;

        // Moving the right video half drags its audio along.
        let cmd = move_group(&p, 0, right_video, (1, Time::from_secs_f64(20.0))).unwrap();
        u.execute(&mut p, cmd);
        let s = &p.sequences[0];
        assert_eq!(s.tracks[1].clips[0].timeline_start, Time::from_secs_f64(20.0));
        assert_eq!(s.tracks[2].clips[1].timeline_start, Time::from_secs_f64(20.0));

        // Video can't go on an audio track.
        assert!(move_group(&p, 0, right_video, (2, Time::from_secs_f64(30.0))).is_err());

        let cmd = delete_group(&p, 0, ClipId(1001));
        u.execute(&mut p, cmd);
        assert_eq!(p.sequences[0].tracks[0].clips.len(), 0);
        assert_eq!(p.sequences[0].tracks[2].clips.len(), 1);

        // Undo everything back to the original pair.
        while u.can_undo() {
            u.undo(&mut p);
        }
        assert!(p.sequences[0].duration() == Time::ZERO);
    }

    #[test]
    fn move_rejects_overlap_of_linked_partner() {
        let mut p = Project::new();
        let mut u = UndoStack::default();
        linked_pair(&mut p, &mut u);
        u.execute(&mut p, Box::new(AddClip { seq: 0, track: 2, clip: clip(1003, 12.0, 5.0) }));
        // Video lands on empty space, but its audio would hit clip 3.
        assert!(move_group(&p, 0, ClipId(1001), (0, Time::from_secs_f64(5.0))).is_err());
        assert!(move_group(&p, 0, ClipId(1001), (0, Time::from_secs_f64(17.0))).is_ok());
    }

    #[test]
    fn split_then_undo_restores() {
        let mut p = Project::new();
        let mut u = UndoStack::default();
        u.execute(&mut p, Box::new(AddClip { seq: 0, track: 0, clip: clip(10, 0.0, 10.0) }));
        u.execute(&mut p, Box::new(SplitClip::new(0, ClipId(10), Time::from_secs_f64(4.0), ClipId(11))));

        let clips = &p.sequences[0].tracks[0].clips;
        assert_eq!(clips.len(), 2);
        assert_eq!(clips[1].source_in, Time::from_secs_f64(4.0));
        assert_eq!(clips[0].duration + clips[1].duration, Time::from_secs_f64(10.0));

        u.undo(&mut p);
        assert_eq!(p.sequences[0].tracks[0].clips, vec![clip(10, 0.0, 10.0)]);
        u.redo(&mut p);
        assert_eq!(p.sequences[0].tracks[0].clips.len(), 2);
    }

    #[test]
    fn move_across_tracks_and_undo() {
        let mut p = Project::new();
        let mut u = UndoStack::default();
        u.execute(&mut p, Box::new(AddClip { seq: 0, track: 0, clip: clip(1, 0.0, 2.0) }));
        let to = (1, Time::from_secs_f64(5.0));
        u.execute(&mut p, Box::new(MoveClip { seq: 0, id: ClipId(1), from: (0, Time::ZERO), to }));
        assert!(p.sequences[0].tracks[0].clips.is_empty());
        assert_eq!(p.sequences[0].tracks[1].clips[0].timeline_start, to.1);
        u.undo(&mut p);
        assert_eq!(p.sequences[0].tracks[0].clips[0].timeline_start, Time::ZERO);
    }

    #[test]
    fn visual_change_undo_and_split_inherits() {
        let mut p = Project::new();
        let mut u = UndoStack::default();
        u.execute(&mut p, Box::new(AddClip { seq: 0, track: 0, clip: clip(1001, 0.0, 4.0) }));
        let t = Transform { position: [10.0, 5.0], scale: [0.5, 0.5], rotation: 15.0, opacity: 0.4 };
        let old = (Transform::default(), BlendMode::Normal);
        u.execute(&mut p, Box::new(SetClipVisual { seq: 0, id: ClipId(1001), old, new: (t, BlendMode::Screen) }));
        let cmd = split_group(&mut p, 0, ClipId(1001), Time::from_secs_f64(2.0)).unwrap();
        u.execute(&mut p, cmd);
        for c in &p.sequences[0].tracks[0].clips {
            assert_eq!((c.transform, c.blend), (t, BlendMode::Screen), "both halves keep the look");
        }
        u.undo(&mut p);
        u.undo(&mut p);
        assert_eq!(p.sequences[0].tracks[0].clips[0].transform, Transform::default());
    }

    #[test]
    fn split_keeps_fade_in_left_and_fade_out_right() {
        let mut p = Project::new();
        let mut u = UndoStack::default();
        let mut c = clip(1001, 0.0, 10.0);
        c.audio = ClipAudio {
            volume_db: -6.0,
            pan: 0.3,
            fade_in: Time::from_secs_f64(1.0),
            fade_out: Time::from_secs_f64(2.0),
            crossfade_in: Time::from_secs_f64(0.5),
            ..Default::default()
        };
        c.audio.set_key(Time::from_secs_f64(3.0), -10.0);
        u.execute(&mut p, Box::new(AddClip { seq: 0, track: 2, clip: c }));
        let cmd = split_group(&mut p, 0, ClipId(1001), Time::from_secs_f64(5.0)).unwrap();
        u.execute(&mut p, cmd);
        let clips = &p.sequences[0].tracks[2].clips;
        assert_eq!((clips[0].audio.fade_in, clips[0].audio.fade_out), (Time::from_secs_f64(1.0), Time::ZERO));
        assert_eq!((clips[1].audio.fade_in, clips[1].audio.fade_out), (Time::ZERO, Time::from_secs_f64(2.0)));
        assert!(clips.iter().all(|c| c.audio.volume_db == -6.0 && c.audio.pan == 0.3), "levels carry over");
        assert!(clips.iter().all(|c| c.audio.volume_keys.len() == 1), "both halves keep the keys");
        assert_eq!((clips[0].audio.crossfade_in, clips[1].audio.crossfade_in), (Time::from_secs_f64(0.5), Time::ZERO));
        u.undo(&mut p);
        assert_eq!(p.sequences[0].tracks[2].clips[0].audio.fade_out, Time::from_secs_f64(2.0));
    }

    #[test]
    fn clip_and_track_audio_commands_undo() {
        let mut p = Project::new();
        let mut u = UndoStack::default();
        u.execute(&mut p, Box::new(AddClip { seq: 0, track: 2, clip: clip(1001, 0.0, 4.0) }));
        let new = ClipAudio { volume_db: -12.0, ..Default::default() };
        u.execute(&mut p, Box::new(SetClipAudio { seq: 0, id: ClipId(1001), old: ClipAudio::default(), new: new.clone() }));
        let t = TrackAudio { volume_db: 3.0, pan: -1.0 };
        u.execute(&mut p, Box::new(SetTrackAudio { seq: 0, track: 2, old: TrackAudio::default(), new: t }));
        assert_eq!(p.sequences[0].tracks[2].audio, t);
        assert_eq!(p.sequences[0].tracks[2].clips[0].audio.volume_db, -12.0);
        u.undo(&mut p);
        u.undo(&mut p);
        assert_eq!(p.sequences[0].tracks[2].audio, TrackAudio::default());
        assert_eq!(p.sequences[0].tracks[2].clips[0].audio, ClipAudio::default());
    }

    #[test]
    fn old_projects_load_with_default_transform() {
        let json = r#"{"id":5,"media":1,"timeline_start":0,"source_in":0,"duration":100}"#;
        let c: Clip = serde_json::from_str(json).unwrap();
        assert_eq!(c.transform, Transform::default());
        assert_eq!(c.blend, BlendMode::Normal);
        assert_eq!(c.audio, ClipAudio::default());
        let t: Track = serde_json::from_str(r#"{"name":"A1","kind":"Audio","clips":[],"muted":false}"#).unwrap();
        assert_eq!(t.audio, TrackAudio::default());
    }

    #[test]
    fn remove_and_undo() {
        let mut p = Project::new();
        let mut u = UndoStack::default();
        u.execute(&mut p, Box::new(AddClip { seq: 0, track: 1, clip: clip(7, 1.0, 1.0) }));
        u.execute(&mut p, Box::new(RemoveClip::new(0, ClipId(7))));
        assert!(p.sequences[0].tracks[1].clips.is_empty());
        u.undo(&mut p);
        assert_eq!(p.sequences[0].tracks[1].clips.len(), 1);
    }
}
