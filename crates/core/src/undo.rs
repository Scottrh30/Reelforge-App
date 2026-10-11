//! Command pattern undo/redo. Every edit to the project goes through here.

use crate::project::*;
use crate::audio::{ClipAudio, TrackAudio};
use crate::time::Time;

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
            // Transform keys are source-anchored too; a dissolve stays on the left half.
            anim: clip.anim.clone(),
            dissolve_in: Time::ZERO,
            // Effect keys are source-anchored as well.
            effects: clip.effects.clone(),
            title: clip.title.clone(),
            nested: clip.nested,
            masks: clip.masks.clone(),
            tracker: clip.tracker.clone(),
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
    pub old: ClipVisual,
    pub new: ClipVisual,
}

impl SetClipVisual {
    fn set(&self, p: &mut Project, v: &ClipVisual) {
        if let Some((_, c)) = p.sequences[self.seq].find_clip_mut(self.id) {
            c.set_visual(v.clone());
        }
    }
}

impl Command for SetClipVisual {
    fn name(&self) -> &str {
        if self.old.blend != self.new.blend {
            "Change blend mode"
        } else if self.old.effects.len() != self.new.effects.len() {
            if self.old.effects.len() < self.new.effects.len() { "Add effect" } else { "Remove effect" }
        } else if self.old.effects != self.new.effects {
            "Change effect"
        } else if self.old.masks.len() != self.new.masks.len() {
            if self.old.masks.len() < self.new.masks.len() { "Add mask" } else { "Remove mask" }
        } else if self.old.masks != self.new.masks {
            "Edit mask"
        } else if self.old.anim != self.new.anim {
            "Change keyframes"
        } else {
            "Change transform"
        }
    }
    fn apply(&mut self, p: &mut Project) {
        self.set(p, &self.new);
    }
    fn undo(&mut self, p: &mut Project) {
        self.set(p, &self.old);
    }
}

/// Set the cross-dissolve into a video clip.
pub struct SetDissolve {
    pub seq: usize,
    pub id: ClipId,
    pub old: Time,
    pub new: Time,
}

impl Command for SetDissolve {
    fn name(&self) -> &str {
        "Change cross-dissolve"
    }
    fn apply(&mut self, p: &mut Project) {
        if let Some((_, c)) = p.sequences[self.seq].find_clip_mut(self.id) {
            c.dissolve_in = self.new;
        }
    }
    fn undo(&mut self, p: &mut Project) {
        if let Some((_, c)) = p.sequences[self.seq].find_clip_mut(self.id) {
            c.dissolve_in = self.old;
        }
    }
}

/// Where a clip sits: `(timeline_start, source_in, duration)`.
pub type Timing = (Time, Time, Time);

/// Set a clip's position, source in-point and length (trimming).
pub struct SetClipTiming {
    pub seq: usize,
    pub id: ClipId,
    pub old: Timing,
    pub new: Timing,
}

impl SetClipTiming {
    fn set(&self, p: &mut Project, (start, src, dur): Timing) {
        let s = &mut p.sequences[self.seq];
        if let Some((ti, c)) = s.find_clip_mut(self.id) {
            (c.timeline_start, c.source_in, c.duration) = (start, src, dur);
            s.tracks[ti].sort();
        }
    }
}

impl Command for SetClipTiming {
    fn name(&self) -> &str {
        "Trim clip"
    }
    fn apply(&mut self, p: &mut Project) {
        self.set(p, self.new);
    }
    fn undo(&mut self, p: &mut Project) {
        self.set(p, self.old);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    Start,
    End,
}

/// How far `edge` of `id` (and the clips linked to it) can move, as
/// `(earliest, latest)` deltas. Limited by the media available, a minimum
/// length of one frame, and the neighbouring clips on each track.
pub fn trim_range(p: &Project, seq: usize, id: ClipId, edge: Edge) -> Option<(Time, Time)> {
    let s = &p.sequences[seq];
    let frame = s.frame_rate.frame_duration();
    let group = s.link_group(id);
    if group.is_empty() || s.is_locked(id) {
        return None;
    }
    let ids: Vec<ClipId> = group.iter().map(|(_, c)| c.id).collect();
    let (mut lo, mut hi) = (Time(i64::MIN / 4), Time(i64::MAX / 4));
    for (ti, c) in &group {
        let still = p.is_still(c.media);
        let others = s.tracks[*ti].clips.iter().filter(|o| !ids.contains(&o.id));
        match edge {
            Edge::Start => {
                if !still {
                    lo = lo.max(Time(-c.source_in.0));
                }
                lo = lo.max(Time(-c.timeline_start.0)); // not before 0
                hi = hi.min(c.duration - frame);
                for o in others.filter(|o| o.timeline_end() <= c.timeline_start) {
                    lo = lo.max(o.timeline_end() - c.timeline_start);
                }
            }
            Edge::End => {
                lo = lo.max(frame - c.duration);
                if !still {
                    let media_len = p.source_len(c);
                    hi = hi.min(media_len - (c.source_in + c.duration));
                }
                for o in others.filter(|o| o.timeline_start >= c.timeline_end()) {
                    hi = hi.min(o.timeline_start - c.timeline_end());
                }
            }
        }
    }
    (lo <= hi).then_some((lo, hi))
}

/// New timing for a clip after moving `edge` by `delta` (already within range).
pub fn trimmed(p: &Project, c: &Clip, edge: Edge, delta: Time) -> Timing {
    match edge {
        Edge::Start => {
            let src = if p.is_still(c.media) { c.source_in } else { c.source_in + delta };
            (c.timeline_start + delta, src, c.duration - delta)
        }
        Edge::End => (c.timeline_start, c.source_in, c.duration + delta),
    }
}

/// Trim `id` and its linked clips by `delta` (clamped to what's allowed).
pub fn trim_group(p: &Project, seq: usize, id: ClipId, edge: Edge, delta: Time) -> Option<Box<dyn Command>> {
    let (lo, hi) = trim_range(p, seq, id, edge)?;
    let d = delta.max(lo).min(hi);
    if d == Time::ZERO {
        return None;
    }
    let s = &p.sequences[seq];
    let cmds = s
        .link_group(id)
        .into_iter()
        .map(|(_, c)| {
            let old = (c.timeline_start, c.source_in, c.duration);
            Box::new(SetClipTiming { seq, id: c.id, old, new: trimmed(p, c, edge, d) }) as Box<dyn Command>
        })
        .collect();
    Some(Box::new(Batch { name: "Trim clip".into(), cmds }))
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

/// Point a media item at a different file (relinking missing media).
pub struct RelinkMedia {
    pub id: MediaId,
    pub old: MediaAsset,
    pub new: MediaAsset,
}

impl Command for RelinkMedia {
    fn name(&self) -> &str {
        "Relink media"
    }
    fn apply(&mut self, p: &mut Project) {
        if let Some(m) = p.media.iter_mut().find(|m| m.id == self.id) {
            *m = self.new.clone();
        }
    }
    fn undo(&mut self, p: &mut Project) {
        if let Some(m) = p.media.iter_mut().find(|m| m.id == self.id) {
            *m = self.old.clone();
        }
    }
}

/// Media whose file isn't where the project says (moved, renamed, or on a drive that isn't connected).
pub fn missing_media(p: &Project) -> Vec<MediaId> {
    p.media.iter().filter(|m| !m.path.exists()).map(|m| m.id).collect()
}

// ---- Composite shots ----------------------------------------------------------

/// Add a new, empty composite shot (a sequence that can be placed in others).
pub struct AddSequence {
    pub seq: Sequence,
    /// Where it lands: always at the end.
    pub index: usize,
}

impl AddSequence {
    pub fn new(p: &Project, seq: Sequence) -> Self {
        Self { seq, index: p.sequences.len() }
    }
}

impl Command for AddSequence {
    fn name(&self) -> &str {
        "New composite shot"
    }
    fn apply(&mut self, p: &mut Project) {
        p.sequences.truncate(self.index);
        p.sequences.push(self.seq.clone());
    }
    fn undo(&mut self, p: &mut Project) {
        if p.sequences.len() == self.index + 1 {
            self.seq = p.sequences.pop().unwrap();
        }
    }
}

/// "Make composite shot": move clips into a new sequence and put one clip
/// playing that sequence where they were (plus a linked audio one if any
/// audio moved).
pub struct MakeComposite {
    seq: usize,
    before: Sequence,
    after: Sequence,
    comp: Sequence,
    /// Index of the new sequence.
    pub index: usize,
    /// The new composite-shot clip (the video one if there is one).
    pub clip: ClipId,
}

impl Command for MakeComposite {
    fn name(&self) -> &str {
        "Make composite shot"
    }
    fn apply(&mut self, p: &mut Project) {
        p.sequences[self.seq] = self.after.clone();
        p.sequences.truncate(self.index);
        p.sequences.push(self.comp.clone());
    }
    fn undo(&mut self, p: &mut Project) {
        if p.sequences.len() == self.index + 1 {
            p.sequences.pop();
        }
        p.sequences[self.seq] = self.before.clone();
    }
}

/// A name like "Composite shot 3" not used by another sequence.
pub fn next_composite_name(p: &Project) -> String {
    (1..).map(|n| format!("Composite shot {n}")).find(|name| !p.sequences.iter().any(|s| &s.name == name)).unwrap()
}

/// Plan "Make composite shot" for clips `ids` (and everything linked to them)
/// in sequence `seq`. Allocates ids, hence `&mut`.
pub fn make_composite(p: &mut Project, seq: usize, ids: &[ClipId], name: String) -> Result<MakeComposite, String> {
    let s = &p.sequences[seq];
    let mut moved: Vec<ClipId> = Vec::new();
    for id in ids {
        for (_, c) in s.link_group(*id) {
            if !moved.contains(&c.id) {
                moved.push(c.id);
            }
        }
    }
    if moved.is_empty() {
        return Err("Select the clips to put in the composite shot first".into());
    }
    if moved.iter().any(|id| s.is_locked(*id)) {
        return Err("Some of those clips are on a locked track".into());
    }
    let clips: Vec<(usize, &Clip)> = moved.iter().filter_map(|id| s.find_clip(*id)).collect();
    let start = clips.iter().map(|(_, c)| c.timeline_start).min().unwrap();
    let end = clips.iter().map(|(_, c)| c.timeline_end()).max().unwrap();

    // The new sequence: same format, one track per track that had a selected clip.
    let mut comp = Sequence { tracks: Vec::new(), in_point: None, out_point: None, ..Sequence::new(name, s.width, s.height, s.frame_rate) };
    let mut track_map = Vec::new(); // outer track -> inner track
    for (ti, tr) in s.tracks.iter().enumerate() {
        if clips.iter().any(|(t, _)| *t == ti) {
            track_map.push((ti, comp.tracks.len()));
            comp.tracks.push(Track { clips: Vec::new(), locked: false, ..tr.clone() });
        }
    }
    for (ti, c) in &clips {
        let inner = track_map.iter().find(|(o, _)| o == ti).unwrap().1;
        let mut c = (*c).clone();
        // A transition from a clip that stays outside can't come along.
        let from_outside = s.tracks[*ti].clips.iter().any(|o| o.timeline_end() == c.timeline_start && !moved.contains(&o.id));
        if from_outside {
            c.dissolve_in = Time::ZERO;
            c.audio.crossfade_in = Time::ZERO;
        }
        c.timeline_start = c.timeline_start - start;
        comp.tracks[inner].clips.push(c);
    }
    for tr in &mut comp.tracks {
        tr.sort();
    }

    let mut after = s.clone();
    for tr in &mut after.tracks {
        let gone: Vec<Time> = tr.clips.iter().filter(|c| moved.contains(&c.id)).map(|c| c.timeline_end()).collect();
        tr.clips.retain(|c| !moved.contains(&c.id));
        // Nor can a transition into a clip that stays from one that left.
        for c in &mut tr.clips {
            if gone.contains(&c.timeline_start) {
                c.dissolve_in = Time::ZERO;
                c.audio.crossfade_in = Time::ZERO;
            }
        }
    }
    let has = |k: TrackKind| clips.iter().any(|(t, _)| s.tracks[*t].kind == k);
    let (has_video, has_audio) = (has(TrackKind::Video), has(TrackKind::Audio));
    // A free track for the new clip: one of the selection's own, lowest first.
    let free_track = |after: &Sequence, kind: TrackKind| -> Result<usize, String> {
        track_map
            .iter()
            .map(|(o, _)| *o)
            .filter(|t| after.tracks[*t].kind == kind)
            .find(|t| !after.overlaps(*t, start, end - start, &[]))
            .ok_or_else(|| "Other clips sit between the selected ones; select them too, or move them out of the way".to_string())
    };
    let link = (has_video && has_audio).then(|| p.alloc_id());
    let mut clip_id = None;
    let s = &p.sequences[seq];
    let before = s.clone();
    let index = p.sequences.len();
    for kind in [TrackKind::Video, TrackKind::Audio] {
        if (kind == TrackKind::Video && !has_video) || (kind == TrackKind::Audio && !has_audio) {
            continue;
        }
        let t = free_track(&after, kind)?;
        let id = ClipId(p.alloc_id());
        clip_id.get_or_insert(id);
        after.tracks[t].clips.push(Clip::new_nested(id, index, start, end - start, link));
        after.tracks[t].sort();
    }
    Ok(MakeComposite { seq, before, after, comp, index, clip: clip_id.unwrap() })
}

// ---- Tracks -------------------------------------------------------------------

/// Insert a new track at `index`.
pub struct AddTrack {
    pub seq: usize,
    pub index: usize,
    pub track: Track,
}

impl AddTrack {
    /// A new empty track of `kind`, named and placed like an editor would.
    pub fn new(p: &Project, seq: usize, kind: TrackKind) -> Self {
        let s = &p.sequences[seq];
        Self { seq, index: s.new_track_index(kind), track: Track::new(s.next_track_name(kind), kind) }
    }
}

impl Command for AddTrack {
    fn name(&self) -> &str {
        if self.track.kind == TrackKind::Video { "Add video track" } else { "Add audio track" }
    }
    fn apply(&mut self, p: &mut Project) {
        let tracks = &mut p.sequences[self.seq].tracks;
        tracks.insert(self.index.min(tracks.len()), self.track.clone());
    }
    fn undo(&mut self, p: &mut Project) {
        let tracks = &mut p.sequences[self.seq].tracks;
        if self.index < tracks.len() {
            tracks.remove(self.index);
        }
    }
}

/// Delete a track and everything on it.
pub struct RemoveTrack {
    pub seq: usize,
    pub index: usize,
    removed: Option<Track>,
}

impl RemoveTrack {
    /// Refuses to remove the last video or the last audio track.
    pub fn new(p: &Project, seq: usize, index: usize) -> Result<Self, &'static str> {
        let s = &p.sequences[seq];
        let Some(t) = s.tracks.get(index) else { return Err("no such track") };
        if s.tracks.iter().filter(|o| o.kind == t.kind).count() <= 1 {
            return Err(if t.kind == TrackKind::Video { "a sequence needs at least one video track" } else { "a sequence needs at least one audio track" });
        }
        if t.locked {
            return Err("the track is locked");
        }
        // Clips linked to ones on this track would be left half-deleted; unlink first.
        let ids: Vec<ClipId> = t.clips.iter().map(|c| c.id).collect();
        let linked_elsewhere = t.clips.iter().any(|c| s.link_group(c.id).iter().any(|(ti, o)| *ti != index && !ids.contains(&o.id)));
        if linked_elsewhere {
            return Err("it has clips linked to clips on other tracks (delete or unlink those first)");
        }
        Ok(Self { seq, index, removed: None })
    }
}

impl Command for RemoveTrack {
    fn name(&self) -> &str {
        "Delete track"
    }
    fn apply(&mut self, p: &mut Project) {
        let tracks = &mut p.sequences[self.seq].tracks;
        if self.index < tracks.len() {
            self.removed = Some(tracks.remove(self.index));
        }
    }
    fn undo(&mut self, p: &mut Project) {
        if let Some(t) = self.removed.take() {
            let tracks = &mut p.sequences[self.seq].tracks;
            tracks.insert(self.index.min(tracks.len()), t);
        }
    }
}

pub struct RenameTrack {
    pub seq: usize,
    pub index: usize,
    pub old: String,
    pub new: String,
}

impl Command for RenameTrack {
    fn name(&self) -> &str {
        "Rename track"
    }
    fn apply(&mut self, p: &mut Project) {
        if let Some(t) = p.sequences[self.seq].tracks.get_mut(self.index) {
            t.name = self.new.clone();
        }
    }
    fn undo(&mut self, p: &mut Project) {
        if let Some(t) = p.sequences[self.seq].tracks.get_mut(self.index) {
            t.name = self.old.clone();
        }
    }
}

/// Add, change or remove a clip's point tracker.
pub struct SetTracker {
    pub seq: usize,
    pub id: ClipId,
    pub old: Option<crate::track::PointTrack>,
    pub new: Option<crate::track::PointTrack>,
    pub label: &'static str,
}

impl Command for SetTracker {
    fn name(&self) -> &str {
        self.label
    }
    fn apply(&mut self, p: &mut Project) {
        if let Some((_, c)) = p.sequences[self.seq].find_clip_mut(self.id) {
            c.tracker = self.new.clone();
        }
    }
    fn undo(&mut self, p: &mut Project) {
        if let Some((_, c)) = p.sequences[self.seq].find_clip_mut(self.id) {
            c.tracker = self.old.clone();
        }
    }
}

/// Change a title clip's text and styling (live-edited like [`SetClipVisual`]).
pub struct SetTitle {
    pub seq: usize,
    pub id: ClipId,
    pub old: crate::title::TitleSpec,
    pub new: crate::title::TitleSpec,
}

impl Command for SetTitle {
    fn name(&self) -> &str {
        if self.old.text != self.new.text { "Edit title text" } else { "Change title style" }
    }
    fn apply(&mut self, p: &mut Project) {
        if let Some((_, c)) = p.sequences[self.seq].find_clip_mut(self.id) {
            c.title = Some(self.new.clone());
        }
    }
    fn undo(&mut self, p: &mut Project) {
        if let Some((_, c)) = p.sequences[self.seq].find_clip_mut(self.id) {
            c.title = Some(self.old.clone());
        }
    }
}

// ---- Group operations (respecting links) -------------------------------------

/// Split `id` and everything linked to it at `at`. `None` if nothing can be split there.
pub fn split_group(p: &mut Project, seq: usize, id: ClipId, at: Time) -> Option<Box<dyn Command>> {
    if p.sequences[seq].is_locked(id) {
        return None;
    }
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

/// Delete `ids` and everything linked to them, as one undo step.
pub fn delete_clips(p: &Project, seq: usize, ids: &[ClipId]) -> Box<dyn Command> {
    let all = expand_links(p, seq, ids);
    let name = if all.len() > 1 { "Delete clips" } else { "Delete clip" };
    let cmds = all.into_iter().map(|id| Box::new(RemoveClip::new(seq, id)) as Box<dyn Command>).collect();
    Box::new(Batch { name: name.into(), cmds })
}

/// `ids` plus every clip linked to any of them, without repeats, in timeline order.
pub fn expand_links(p: &Project, seq: usize, ids: &[ClipId]) -> Vec<ClipId> {
    let s = &p.sequences[seq];
    let mut out: Vec<ClipId> = Vec::new();
    for id in ids {
        for (_, c) in s.link_group(*id) {
            if !out.contains(&c.id) {
                out.push(c.id);
            }
        }
    }
    out.sort_by_key(|id| s.find_clip(*id).map(|(t, c)| (c.timeline_start, t)));
    out
}

/// Move `id` to `(track, start)`; linked clips shift by the same time but stay on their tracks.
pub fn move_group(p: &Project, seq: usize, id: ClipId, to: (usize, Time)) -> Result<Box<dyn Command>, &'static str> {
    move_clips(p, seq, id, &[id], to)
}

/// Move a selection: clip `lead` (one of `selection`) goes to `(track, start)`
/// and every selected clip, plus what's linked to them, shifts by the same
/// time. Clips of the lead's kind also shift by the same number of tracks of
/// that kind; linked clips of the other kind stay on their tracks.
pub fn move_clips(p: &Project, seq: usize, lead: ClipId, selection: &[ClipId], to: (usize, Time)) -> Result<Box<dyn Command>, &'static str> {
    let moves = move_targets(p, seq, lead, selection, to)?;
    let cmds: Vec<Box<dyn Command>> = moves
        .iter()
        .filter(|m| (m.to_track, m.start) != (m.from_track, m.from_start))
        .map(|m| Box::new(MoveClip { seq, id: m.id, from: (m.from_track, m.from_start), to: (m.to_track, m.start) }) as Box<dyn Command>)
        .collect();
    let groups = {
        let s = &p.sequences[seq];
        let mut links: Vec<Option<u64>> = moves.iter().map(|m| s.find_clip(m.id).and_then(|(_, c)| c.link).or(Some(u64::MAX - m.id.0))).collect();
        links.sort();
        links.dedup();
        links.len()
    };
    Ok(Box::new(Batch { name: if groups > 1 { "Move clips" } else { "Move clip" }.into(), cmds }))
}

/// Where each clip goes in a [`move_clips`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ClipMove {
    pub id: ClipId,
    pub from_track: usize,
    pub from_start: Time,
    pub to_track: usize,
    pub start: Time,
}

/// Plan a [`move_clips`] without making it (e.g. to draw where clips would land).
pub fn move_targets(p: &Project, seq: usize, lead: ClipId, selection: &[ClipId], to: (usize, Time)) -> Result<Vec<ClipMove>, &'static str> {
    let s = &p.sequences[seq];
    let Some((lead_track, lead_clip)) = s.find_clip(lead) else { return Err("clip not found") };
    let mut sel = selection.to_vec();
    if !sel.contains(&lead) {
        sel.push(lead);
    }
    let moving = expand_links(p, seq, &sel);
    if moving.iter().any(|id| s.is_locked(*id)) || s.tracks.get(to.0).is_none_or(|t| t.locked) {
        return Err("the track is locked");
    }
    let kind = s.tracks[lead_track].kind;
    if s.tracks[to.0].kind != kind {
        return Err("clips can only move to a track of the same kind");
    }
    // Tracks of the lead's kind, bottom first, so "up one video track" skips audio tracks.
    let same_kind: Vec<usize> = (0..s.tracks.len()).filter(|&t| s.tracks[t].kind == kind).collect();
    let rank = |t: usize| same_kind.iter().position(|&x| x == t);
    let shift = rank(to.0).unwrap() as isize - rank(lead_track).unwrap() as isize;
    let delta = to.1.0 - lead_clip.timeline_start.0;
    let mut out = Vec::new();
    for id in &moving {
        let (ti, c) = s.find_clip(*id).unwrap();
        let start = Time(c.timeline_start.0 + delta);
        if start < Time::ZERO {
            return Err("can't move before the start of the timeline");
        }
        // Only selected clips change track; linked partners keep theirs.
        let track = if s.tracks[ti].kind == kind && sel.contains(id) {
            let r = rank(ti).unwrap() as isize + shift;
            *same_kind.get(usize::try_from(r).map_err(|_| "there's no track there")?).ok_or("there's no track there")?
        } else {
            ti
        };
        if s.tracks[track].locked {
            return Err("the track is locked");
        }
        if s.overlaps(track, start, c.duration, &moving) {
            return Err("it would overlap another clip");
        }
        out.push(ClipMove { id: c.id, from_track: ti, from_start: c.timeline_start, to_track: track, start });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::Time;
    use crate::audio::{ClipAudio, TrackAudio};
    use crate::transform::{BlendMode, Transform};
    use crate::{MediaInfo, Prop, VideoStreamInfo, FrameRate, Rational};

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
        let old = ClipVisual::default();
        let effects = vec![crate::Effect::new(crate::EffectKind::Blur)];
        let new = ClipVisual { transform: t, blend: BlendMode::Screen, anim: Default::default(), effects: effects.clone(), masks: Vec::new() };
        u.execute(&mut p, Box::new(SetClipVisual { seq: 0, id: ClipId(1001), old, new }));
        let cmd = split_group(&mut p, 0, ClipId(1001), Time::from_secs_f64(2.0)).unwrap();
        u.execute(&mut p, cmd);
        for c in &p.sequences[0].tracks[0].clips {
            assert_eq!((c.transform, c.blend), (t, BlendMode::Screen), "both halves keep the look");
            assert_eq!(c.effects, effects, "and the effects");
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

    fn video_media(id: u64, secs: f64) -> MediaAsset {
        MediaAsset {
            id: MediaId(id),
            path: format!("{id}.mp4").into(),
            name: String::new(),
            info: MediaInfo {
                duration: Time::from_secs_f64(secs),
                video: Some(VideoStreamInfo { width: 64, height: 64, frame_rate: FrameRate::new(30, 1), codec: "h264".into(), time_base: Rational::new(1, 30) }),
                audio: None,
                container: String::new(),
            },
        }
    }

    #[test]
    fn trimming_respects_media_neighbours_and_minimum_length() {
        let s = Time::from_secs_f64;
        let mut p = Project::new(); // 30 fps sequence
        p.media.push(video_media(1, 10.0));
        let mut a = Clip::new(ClipId(1), MediaId(1), s(2.0), s(1.0), s(3.0), None); // source 1–4 at 2–5
        a.link = Some(7);
        let mut a_audio = a.clone();
        a_audio.id = ClipId(2);
        let b = Clip::new(ClipId(3), MediaId(1), s(6.0), s(0.0), s(2.0), None); // next clip at 6
        p.sequences[0].tracks[0].clips = vec![a, b];
        p.sequences[0].tracks[2].clips = vec![a_audio];

        let frame = Time::from_frame(1, FrameRate::new(30, 1));
        // Start edge: back to source 0 (−1 s), forward to leave one frame.
        assert_eq!(trim_range(&p, 0, ClipId(1), Edge::Start), Some((s(-1.0), s(3.0) - frame)));
        // End edge: up to the next clip at 6 s (+1 s) on the video track.
        assert_eq!(trim_range(&p, 0, ClipId(1), Edge::End).map(|r| r.1), Some(s(1.0)));

        // Trimming either linked clip trims both, clamped at the limit.
        let mut u = UndoStack::default();
        let cmd = trim_group(&p, 0, ClipId(2), Edge::End, s(5.0)).unwrap();
        u.execute(&mut p, cmd);
        for (track, id) in [(0, ClipId(1)), (2, ClipId(2))] {
            let c = p.sequences[0].tracks[track].clips.iter().find(|c| c.id == id).unwrap();
            assert_eq!(c.timeline_end(), s(6.0), "clamped to the neighbour");
        }
        let cmd = trim_group(&p, 0, ClipId(1), Edge::Start, s(-0.5)).unwrap();
        u.execute(&mut p, cmd);
        let c = &p.sequences[0].tracks[0].clips[0];
        assert_eq!((c.timeline_start, c.source_in, c.duration), (s(1.5), s(0.5), s(4.5)), "start trim moves the in-point too");
        u.undo(&mut p);
        u.undo(&mut p);
        let c = &p.sequences[0].tracks[0].clips[0];
        assert_eq!((c.timeline_start, c.source_in, c.duration), (s(2.0), s(1.0), s(3.0)));
    }

    #[test]
    fn stills_can_be_extended_freely() {
        let s = Time::from_secs_f64;
        let mut p = Project::new();
        p.media.push(video_media(1, 0.0)); // a still image
        p.sequences[0].tracks[0].clips = vec![Clip::new(ClipId(1), MediaId(1), s(1.0), s(0.0), s(5.0), None)];
        let (lo, hi) = trim_range(&p, 0, ClipId(1), Edge::End).unwrap();
        assert!(hi > s(1000.0) && lo < Time::ZERO);
        let (lo, _) = trim_range(&p, 0, ClipId(1), Edge::Start).unwrap();
        assert_eq!(lo, s(-1.0), "only limited by the start of the timeline");
    }

    #[test]
    fn split_keeps_keyframes_and_dissolve_on_the_left() {
        let s = Time::from_secs_f64;
        let mut p = Project::new();
        let mut u = UndoStack::default();
        let mut c = clip(1001, 0.0, 4.0);
        c.anim.rotation.set(s(1.0), 10.0);
        c.dissolve_in = s(0.5);
        u.execute(&mut p, Box::new(AddClip { seq: 0, track: 0, clip: c }));
        let _ = Prop::Rotation;
        let cmd = split_group(&mut p, 0, ClipId(1001), s(2.0)).unwrap();
        u.execute(&mut p, cmd);
        let cl = &p.sequences[0].tracks[0].clips;
        assert!(cl.iter().all(|c| c.anim.rotation.0.len() == 1));
        assert_eq!((cl[0].dissolve_in, cl[1].dissolve_in), (s(0.5), Time::ZERO));
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

#[cfg(test)]
mod track_tests {
    use super::*;
    use crate::title::TitleSpec;

    fn s(x: f64) -> Time {
        Time::from_secs_f64(x)
    }

    #[test]
    fn add_rename_remove_tracks_with_undo() {
        let mut p = Project::new(); // V1, V2, A1
        let mut u = UndoStack::default();
        let v = AddTrack::new(&p, 0, TrackKind::Video);
        assert_eq!((v.index, v.track.name.as_str()), (2, "Video 3"), "new video tracks go on top of the video tracks");
        u.execute(&mut p, Box::new(v));
        let a = AddTrack::new(&p, 0, TrackKind::Audio);
        u.execute(&mut p, Box::new(a));
        let names: Vec<&str> = p.sequences[0].tracks.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["Video 1", "Video 2", "Video 3", "Audio 1", "Audio 2"]);

        u.execute(&mut p, Box::new(RenameTrack { seq: 0, index: 2, old: "Video 3".into(), new: "Titles".into() }));
        assert_eq!(p.sequences[0].tracks[2].name, "Titles");
        assert_eq!(p.sequences[0].next_track_name(TrackKind::Video), "Video 3", "the freed name is reused");

        // Delete V2 with a clip on it; undo brings both back.
        p.sequences[0].tracks[1].clips.push(Clip::new(ClipId(7), MediaId(1), s(0.0), s(0.0), s(1.0), None));
        let cmd = RemoveTrack::new(&p, 0, 1).unwrap();
        u.execute(&mut p, Box::new(cmd));
        assert_eq!(p.sequences[0].tracks.len(), 4);
        assert!(p.sequences[0].find_clip(ClipId(7)).is_none());
        u.undo(&mut p);
        assert_eq!(p.sequences[0].tracks[1].name, "Video 2");
        assert!(p.sequences[0].find_clip(ClipId(7)).is_some());
        u.undo(&mut p);
        assert_eq!(p.sequences[0].tracks[2].name, "Video 3");
        u.undo(&mut p);
        u.undo(&mut p);
        assert_eq!(p.sequences[0].tracks.len(), 3);
    }

    #[test]
    fn track_removal_guards() {
        let mut p = Project::new();
        assert!(RemoveTrack::new(&p, 0, 2).is_err(), "last audio track");
        assert!(RemoveTrack::new(&p, 0, 0).is_ok());
        p.sequences[0].tracks[0].locked = true;
        assert!(RemoveTrack::new(&p, 0, 0).is_err(), "locked");
        p.sequences[0].tracks[0].locked = false;
        // A linked pair across V1 and A1: deleting V1 would orphan the audio.
        let mut v = Clip::new(ClipId(1), MediaId(1), s(0.0), s(0.0), s(1.0), Some(5));
        p.sequences[0].tracks[0].clips.push(v.clone());
        v.id = ClipId(2);
        p.sequences[0].tracks[2].clips.push(v);
        assert!(RemoveTrack::new(&p, 0, 0).is_err());
    }

    #[test]
    fn titles_behave_like_stills() {
        let mut p = Project::new();
        let mut u = UndoStack::default();
        let t = Clip::new_title(ClipId(3), s(1.0), s(2.0), TitleSpec::default());
        assert!(p.is_still(t.media));
        p.sequences[0].tracks[1].clips.push(t);
        // Can be stretched to any length (no media limit) and split, keeping the text.
        let (_, hi) = trim_range(&p, 0, ClipId(3), Edge::End).unwrap();
        assert!(hi > s(1000.0));
        let cmd = split_group(&mut p, 0, ClipId(3), s(2.0)).unwrap();
        u.execute(&mut p, cmd);
        assert!(p.sequences[0].tracks[1].clips.iter().all(|c| c.title.as_ref().is_some_and(|t| t.text == "Title")));
        // Editing the text is undoable.
        let old = TitleSpec::default();
        let new = TitleSpec { text: "Hello".into(), ..old.clone() };
        u.execute(&mut p, Box::new(SetTitle { seq: 0, id: ClipId(3), old, new }));
        assert_eq!(p.sequences[0].find_clip(ClipId(3)).unwrap().1.display_name(&p), "Hello");
        u.undo(&mut p);
        assert_eq!(p.sequences[0].find_clip(ClipId(3)).unwrap().1.title.as_ref().unwrap().text, "Title");
        // Titles show up as layers like any video clip.
        assert_eq!(p.video_layers_at(0, s(1.5)).len(), 1);
    }
}

#[cfg(test)]
mod composite_tests {
    use super::*;

    fn s(x: f64) -> Time {
        Time::from_secs_f64(x)
    }

    #[test]
    fn makes_a_composite_shot_and_undoes_it() {
        let mut p = Project::new(); // V1, V2, A1
        // V1: a [0,2) linked to audio, b [2,4) (stays); V2: c [1,3) on top.
        let seq = &mut p.sequences[0];
        seq.tracks[0].clips.push(Clip::new(ClipId(1), MediaId(1), s(0.0), s(0.0), s(2.0), Some(50)));
        seq.tracks[2].clips.push(Clip::new(ClipId(2), MediaId(1), s(0.0), s(0.0), s(2.0), Some(50)));
        let mut b = Clip::new(ClipId(3), MediaId(1), s(2.0), s(2.0), s(2.0), None);
        b.dissolve_in = s(0.5);
        seq.tracks[0].clips.push(b);
        seq.tracks[1].clips.push(Clip::new(ClipId(4), MediaId(1), s(1.0), s(0.0), s(2.0), None));
        for _ in 0..100 {
            p.alloc_id(); // past the hand-made ids
        }
        let before = p.sequences[0].clone();

        let mut u = UndoStack::default();
        // Selecting the video of `a` brings its audio along.
        let name = next_composite_name(&p);
        let cmd = make_composite(&mut p, 0, &[ClipId(1), ClipId(4)], name).unwrap();
        let clip = cmd.clip;
        u.execute(&mut p, Box::new(cmd));
        assert_eq!(p.sequences.len(), 2);
        let comp = &p.sequences[1];
        assert_eq!(comp.name, "Composite shot 1");
        let names: Vec<&str> = comp.tracks.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["Video 1", "Video 2", "Audio 1"]);
        assert_eq!(comp.tracks[1].clips[0].timeline_start, s(1.0), "times are relative to the shot's start");
        assert_eq!(comp.duration(), s(3.0));

        let outer = &p.sequences[0];
        let (t, c) = outer.find_clip(clip).unwrap();
        // V1 still has b at 2-4 s in the way, so the shot goes on V2.
        assert_eq!((t, c.nested, c.media, c.timeline_start, c.duration), (1, Some(1), MediaId::NESTED, s(0.0), s(3.0)));
        assert!(outer.find_clip(ClipId(1)).is_none() && outer.find_clip(ClipId(4)).is_none());
        // b no longer dissolves from a clip that left.
        assert_eq!(outer.find_clip(ClipId(3)).unwrap().1.dissolve_in, Time::ZERO);
        // The audio composite clip is linked to the video one.
        let group = outer.link_group(clip);
        assert_eq!(group.len(), 2);
        assert_eq!(group[1].0, 2);
        assert_eq!(p.source_len(c), s(3.0));
        assert!(p.contains_sequence(0, 1) && !p.contains_sequence(1, 0));
        assert_eq!(p.nested_in(0), vec![1]);
        assert_eq!(c.display_name(&p), "Composite shot 1");

        u.undo(&mut p);
        assert_eq!(p.sequences.len(), 1);
        assert_eq!(p.sequences[0].tracks[0].clips, before.tracks[0].clips);
        u.redo(&mut p);
        assert_eq!(p.sequences.len(), 2);
    }

    #[test]
    fn refuses_when_other_clips_are_in_the_way() {
        let mut p = Project::new();
        let seq = &mut p.sequences[0];
        seq.tracks[0].clips.push(Clip::new(ClipId(1), MediaId(1), s(0.0), s(0.0), s(1.0), None));
        seq.tracks[0].clips.push(Clip::new(ClipId(2), MediaId(1), s(1.0), s(0.0), s(1.0), None));
        seq.tracks[0].clips.push(Clip::new(ClipId(3), MediaId(1), s(2.0), s(0.0), s(1.0), None));
        assert!(make_composite(&mut p, 0, &[ClipId(1), ClipId(3)], "x".into()).is_err());
        assert!(make_composite(&mut p, 0, &[], "x".into()).is_err());
        assert!(make_composite(&mut p, 0, &[ClipId(1), ClipId(2), ClipId(3)], "x".into()).is_ok());
    }
}
