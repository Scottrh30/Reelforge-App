//! Trim tools: normal, ripple and roll, plus ripple delete.
//!
//! Every operation is first *planned* against the current project: the plan
//! says which clips move where, with the requested delta already clamped to
//! what's possible. Plans are applied live while dragging and turned into one
//! undoable command on release.
//!
//! - **Normal**: the edge moves; nothing else does (stops at neighbours).
//! - **Ripple**: the clip gets longer/shorter and everything after it on *all*
//!   tracks shifts to match, so later edits stay in sync. Trimming the start
//!   keeps the clip where it is and changes which part of the media it shows.
//! - **Roll**: moves a cut between two touching clips: one gets longer by
//!   exactly what the other loses, so nothing else on the timeline moves.
//! - **Slip**: the clip stays where it is and keeps its length, but shows an
//!   earlier or later part of its media.
//! - **Slide**: the clip keeps its content and length but moves along the
//!   timeline; the clips either side get longer/shorter to fill in.
//!
//! Clips on locked tracks (or linked to one) are never changed: they can't be
//! trimmed, and a ripple leaves them where they are.

use crate::project::*;
use crate::time::Time;
use crate::undo::{trim_range, trimmed, Batch, Command, Edge, RemoveClip, SetClipTiming, Timing};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum TrimMode {
    #[default]
    Normal,
    Ripple,
    Roll,
    /// Change which part of the media a clip shows (drag the clip body).
    Slip,
    /// Move a clip between its neighbours, adjusting them (drag the clip body).
    Slide,
}

impl TrimMode {
    pub const ALL: [TrimMode; 5] = [TrimMode::Normal, TrimMode::Ripple, TrimMode::Roll, TrimMode::Slip, TrimMode::Slide];

    pub fn label(self) -> &'static str {
        match self {
            TrimMode::Normal => "Trim",
            TrimMode::Ripple => "Ripple",
            TrimMode::Roll => "Roll",
            TrimMode::Slip => "Slip",
            TrimMode::Slide => "Slide",
        }
    }

    /// Slip and slide act on the whole clip rather than on an edge.
    pub fn acts_on_body(self) -> bool {
        matches!(self, TrimMode::Slip | TrimMode::Slide)
    }

    /// Name of the undo step.
    pub fn undo_name(self) -> &'static str {
        match self {
            TrimMode::Normal => "Trim clip",
            TrimMode::Ripple => "Ripple trim",
            TrimMode::Roll => "Roll edit",
            TrimMode::Slip => "Slip clip",
            TrimMode::Slide => "Slide clip",
        }
    }
}

/// The outcome of planning a trim.
#[derive(Clone, Debug, PartialEq)]
pub struct TrimPlan {
    /// The delta actually used (the request clamped to what's possible).
    pub applied: Time,
    /// `(clip, before, after)` for every clip that changes.
    pub changes: Vec<(ClipId, Timing, Timing)>,
    /// The mode used (a roll with no neighbouring clip falls back to Normal).
    pub mode: TrimMode,
    /// True when the request was clamped.
    pub limited: bool,
}

impl TrimPlan {
    pub fn command(&self, seq: usize, name: &str) -> Option<Box<dyn Command>> {
        if self.changes.is_empty() {
            return None;
        }
        let cmds = self
            .changes
            .iter()
            .map(|&(id, old, new)| Box::new(SetClipTiming { seq, id, old, new }) as Box<dyn Command>)
            .collect();
        Some(Box::new(Batch { name: name.into(), cmds }))
    }

    /// Write the new timings straight into the project (for live dragging).
    pub fn apply(&self, p: &mut Project, seq: usize) {
        let s = &mut p.sequences[seq];
        for &(id, _, (start, src, dur)) in &self.changes {
            if let Some((_, c)) = s.find_clip_mut(id) {
                (c.timeline_start, c.source_in, c.duration) = (start, src, dur);
            }
        }
        for tr in &mut s.tracks {
            tr.sort();
        }
    }
}

fn timing(c: &Clip) -> Timing {
    (c.timeline_start, c.source_in, c.duration)
}

/// Most negative shift allowed for every clip starting at or after `after`
/// (on all tracks, except `exclude`), so none of them runs into a clip that
/// stays put, or before 0. Always ≤ 0.
pub fn shift_limit(p: &Project, seq: usize, after: Time, exclude: &[ClipId]) -> Time {
    let s = &p.sequences[seq];
    let mut limit = Time(i64::MIN / 4);
    for tr in &s.tracks {
        let (moving, fixed): (Vec<&Clip>, Vec<&Clip>) =
            tr.clips.iter().filter(|c| !exclude.contains(&c.id)).partition(|c| shifts(s, c, after));
        let Some(first) = moving.iter().map(|c| c.timeline_start).min() else { continue };
        let floor = fixed.iter().map(|c| c.timeline_end()).max().unwrap_or(Time::ZERO).max(Time::ZERO);
        limit = limit.max(floor - first);
    }
    limit.min(Time::ZERO)
}

/// Does a ripple at `after` move `c`? Clips on (or linked to) a locked track stay put.
fn shifts(s: &Sequence, c: &Clip, after: Time) -> bool {
    c.timeline_start >= after && !s.is_locked(c.id)
}

/// Shift every clip starting at or after `after` (except `exclude`) by `by`.
fn shift_changes(p: &Project, seq: usize, after: Time, by: Time, exclude: &[ClipId]) -> Vec<(ClipId, Timing, Timing)> {
    if by == Time::ZERO {
        return Vec::new();
    }
    p.sequences[seq]
        .tracks
        .iter()
        .flat_map(|t| &t.clips)
        .filter(|c| shifts(&p.sequences[seq], c, after) && !exclude.contains(&c.id))
        .map(|c| (c.id, timing(c), (c.timeline_start + by, c.source_in, c.duration)))
        .collect()
}

/// Media-length and minimum-length limits for changing a clip's length from
/// one `edge` (not neighbours: ripple moves them, roll handles them itself).
fn length_range(p: &Project, seq: usize, c: &Clip, edge: Edge) -> (Time, Time) {
    let frame = p.sequences[seq].frame_rate.frame_duration();
    let still = p.is_still(c.media);
    let big = Time(i64::MAX / 4);
    match edge {
        Edge::Start => {
            // delta moves the in-point: + shortens, − lengthens.
            let lo = if still { -big } else { p.clip_limits(c).0 - c.source_in };
            (lo, c.duration - frame)
        }
        Edge::End => {
            let hi = if still { big } else { p.clip_limits(c).1 - (c.source_in + c.duration) };
            (frame - c.duration, hi)
        }
    }
}

/// The clip touching `c`'s `edge` on the same track, if any.
fn neighbour<'a>(s: &'a Sequence, track: usize, c: &Clip, edge: Edge) -> Option<&'a Clip> {
    s.tracks[track].clips.iter().find(|o| match edge {
        Edge::End => o.timeline_start == c.timeline_end() && o.id != c.id,
        Edge::Start => o.timeline_end() == c.timeline_start && o.id != c.id,
    })
}

/// Plan a trim of `edge` of clip `id` (and the clips linked to it) by `delta`.
pub fn plan_trim(p: &Project, seq: usize, id: ClipId, edge: Edge, mode: TrimMode, delta: Time) -> Option<TrimPlan> {
    let s = &p.sequences[seq];
    if s.is_locked(id) {
        return None;
    }
    let group: Vec<(usize, &Clip)> = s.link_group(id);
    let (_, grabbed) = *group.iter().find(|(_, c)| c.id == id)?;
    let clamp = |lo: Time, hi: Time| -> Option<(Time, bool)> {
        (lo <= hi).then(|| {
            let d = delta.max(lo).min(hi);
            (d, d != delta)
        })
    };

    match mode {
        TrimMode::Slip => plan_slip(p, seq, id, delta),
        TrimMode::Slide => plan_slide(p, seq, id, delta),
        TrimMode::Normal => {
            let (lo, hi) = trim_range(p, seq, id, edge)?;
            let (d, limited) = clamp(lo, hi)?;
            let changes = group.iter().map(|(_, c)| (c.id, timing(c), trimmed(p, c, edge, d))).filter(|(_, a, b)| a != b).collect();
            Some(TrimPlan { applied: d, changes, mode, limited })
        }

        TrimMode::Ripple => {
            let ids: Vec<ClipId> = group.iter().map(|(_, c)| c.id).collect();
            let mut lo = Time(i64::MIN / 4);
            let mut hi = Time(i64::MAX / 4);
            for (_, c) in &group {
                let (a, b) = length_range(p, seq, c, edge);
                lo = lo.max(a);
                hi = hi.min(b);
            }
            // Later clips shift by `shift`: +d for the end edge, −d for the start edge.
            let after = grabbed.timeline_end();
            let min_shift = shift_limit(p, seq, after, &ids);
            match edge {
                Edge::End => lo = lo.max(min_shift),
                Edge::Start => hi = hi.min(Time(-min_shift.0)),
            }
            let (d, limited) = clamp(lo, hi)?;
            let shift = if edge == Edge::End { d } else { Time(-d.0) };
            let mut changes: Vec<(ClipId, Timing, Timing)> = group
                .iter()
                .map(|(_, c)| {
                    let new = match edge {
                        Edge::End => (c.timeline_start, c.source_in, c.duration + d),
                        // The clip stays put; only the media it shows changes.
                        Edge::Start => {
                            let src = if p.is_still(c.media) { c.source_in } else { c.source_in + d };
                            (c.timeline_start, src, c.duration - d)
                        }
                    };
                    (c.id, timing(c), new)
                })
                .collect();
            changes.extend(shift_changes(p, seq, after, shift, &ids));
            changes.retain(|(_, a, b)| a != b);
            Some(TrimPlan { applied: d, changes, mode, limited })
        }

        TrimMode::Roll => {
            // Pairs of (left, right) clips meeting at the cut, for the grabbed clip and its links.
            let mut pairs: Vec<(&Clip, &Clip)> = Vec::new();
            let mut lone: Vec<&Clip> = Vec::new();
            for (ti, c) in &group {
                match (edge, neighbour(s, *ti, c, edge)) {
                    (Edge::End, Some(r)) => pairs.push((c, r)),
                    (Edge::Start, Some(l)) => pairs.push((l, c)),
                    (_, None) => lone.push(c),
                }
            }
            if !pairs.iter().any(|(l, r)| l.id == id || r.id == id) {
                // Nothing to roll against: behave like a normal trim.
                return plan_trim(p, seq, id, edge, TrimMode::Normal, delta);
            }
            // `delta` moves the cut: + later, − earlier.
            let mut lo = Time(i64::MIN / 4);
            let mut hi = Time(i64::MAX / 4);
            for (l, r) in &pairs {
                let (_, l_hi) = length_range(p, seq, l, Edge::End);
                let (l_lo, _) = length_range(p, seq, l, Edge::End);
                let (r_lo, r_hi) = length_range(p, seq, r, Edge::Start);
                lo = lo.max(l_lo).max(r_lo);
                hi = hi.min(l_hi).min(r_hi);
            }
            // Linked clips with no neighbour at the cut are trimmed normally alongside.
            for c in &lone {
                let (ti, _) = s.find_clip(c.id)?;
                let others = s.tracks[ti].clips.iter().filter(|o| o.id != c.id);
                let (a, b) = length_range(p, seq, c, edge);
                lo = lo.max(a);
                hi = hi.min(b);
                for o in others {
                    match edge {
                        Edge::End if o.timeline_start >= c.timeline_end() => hi = hi.min(o.timeline_start - c.timeline_end()),
                        Edge::Start if o.timeline_end() <= c.timeline_start => lo = lo.max(o.timeline_end() - c.timeline_start),
                        _ => {}
                    }
                }
            }
            let (d, limited) = clamp(lo, hi)?;
            let mut changes = Vec::new();
            for (l, r) in &pairs {
                changes.push((l.id, timing(l), (l.timeline_start, l.source_in, l.duration + d)));
                let src = if p.is_still(r.media) { r.source_in } else { r.source_in + d };
                changes.push((r.id, timing(r), (r.timeline_start + d, src, r.duration - d)));
            }
            for c in &lone {
                changes.push((c.id, timing(c), trimmed(p, c, edge, d)));
            }
            changes.retain(|(_, a, b)| a != b);
            Some(TrimPlan { applied: d, changes, mode, limited })
        }
    }
}

/// Delete `id` and its linked clips, then close the gap by pulling everything
/// after it (on all tracks) earlier, as far as other clips allow.
/// Returns the command and how much of the gap was closed.
pub fn ripple_delete(p: &Project, seq: usize, id: ClipId) -> Option<(Box<dyn Command>, Time)> {
    let s = &p.sequences[seq];
    let group = s.link_group(id);
    if group.is_empty() || s.is_locked(id) {
        return None;
    }
    let ids: Vec<ClipId> = group.iter().map(|(_, c)| c.id).collect();
    let start = group.iter().map(|(_, c)| c.timeline_start).min()?;
    let end = group.iter().map(|(_, c)| c.timeline_end()).max()?;
    let gap = end - start;
    let shift = Time(-gap.0).max(shift_limit(p, seq, end, &ids));
    let mut cmds: Vec<Box<dyn Command>> = ids.iter().map(|&cid| Box::new(RemoveClip::new(seq, cid)) as Box<dyn Command>).collect();
    for (cid, old, new) in shift_changes(p, seq, end, shift, &ids) {
        cmds.push(Box::new(SetClipTiming { seq, id: cid, old, new }));
    }
    Some((Box::new(Batch { name: "Ripple delete".into(), cmds }), Time(-shift.0)))
}

/// Slip `id` (and its linked clips) by `delta`: + shows later media, − earlier.
/// Position and length don't change. Stills have nothing to slip.
pub fn plan_slip(p: &Project, seq: usize, id: ClipId, delta: Time) -> Option<TrimPlan> {
    let s = &p.sequences[seq];
    if s.is_locked(id) {
        return None;
    }
    let group = s.link_group(id);
    let (mut lo, mut hi) = (Time(i64::MIN / 4), Time(i64::MAX / 4));
    let mut movable = Vec::new();
    for (_, c) in &group {
        if p.is_still(c.media) {
            continue;
        }
        let (a, b) = p.clip_limits(c);
        lo = lo.max(a - c.source_in);
        hi = hi.min(b - (c.source_in + c.duration));
        movable.push(*c);
    }
    if movable.is_empty() {
        return Some(TrimPlan { applied: Time::ZERO, changes: Vec::new(), mode: TrimMode::Slip, limited: delta != Time::ZERO });
    }
    if lo > hi {
        return None;
    }
    let d = delta.max(lo).min(hi);
    let changes = movable
        .iter()
        .map(|c| (c.id, timing(c), (c.timeline_start, c.source_in + d, c.duration)))
        .filter(|(_, a, b)| a != b)
        .collect();
    Some(TrimPlan { applied: d, changes, mode: TrimMode::Slip, limited: d != delta })
}

/// Slide `id` (and its linked clips) along the timeline by `delta`, keeping
/// its content. A clip touching its start gets longer/shorter to stay butted
/// up to it, and one touching its end has its start trimmed to match. Where
/// there's a gap instead, the clip can move into the gap but not past it.
pub fn plan_slide(p: &Project, seq: usize, id: ClipId, delta: Time) -> Option<TrimPlan> {
    let s = &p.sequences[seq];
    if s.is_locked(id) {
        return None;
    }
    let group = s.link_group(id);
    let ids: Vec<ClipId> = group.iter().map(|(_, c)| c.id).collect();
    let (mut lo, mut hi) = (Time(i64::MIN / 4), Time(i64::MAX / 4));
    let mut sides: Vec<(&Clip, Option<&Clip>, Option<&Clip>)> = Vec::new();
    for (ti, c) in &group {
        let left = neighbour(s, *ti, c, Edge::Start).filter(|o| !ids.contains(&o.id));
        let right = neighbour(s, *ti, c, Edge::End).filter(|o| !ids.contains(&o.id));
        let others = || s.tracks[*ti].clips.iter().filter(|o| !ids.contains(&o.id));
        match left {
            Some(l) => {
                // The left clip changes length at its end.
                let (a, b) = length_range(p, seq, l, Edge::End);
                lo = lo.max(a);
                hi = hi.min(b);
            }
            None => {
                let floor = others().filter(|o| o.timeline_end() <= c.timeline_start).map(|o| o.timeline_end()).max().unwrap_or(Time::ZERO);
                lo = lo.max(floor - c.timeline_start);
            }
        }
        match right {
            Some(r) => {
                // The right clip's start moves with the slid clip's end.
                let (a, b) = length_range(p, seq, r, Edge::Start);
                lo = lo.max(a);
                hi = hi.min(b);
            }
            None => {
                if let Some(ceil) = others().filter(|o| o.timeline_start >= c.timeline_end()).map(|o| o.timeline_start).min() {
                    hi = hi.min(ceil - c.timeline_end());
                }
            }
        }
        sides.push((c, left, right));
    }
    if lo > hi {
        return None;
    }
    let d = delta.max(lo).min(hi);
    let mut changes = Vec::new();
    for (c, left, right) in sides {
        changes.push((c.id, timing(c), (c.timeline_start + d, c.source_in, c.duration)));
        if let Some(l) = left {
            changes.push((l.id, timing(l), (l.timeline_start, l.source_in, l.duration + d)));
        }
        if let Some(r) = right {
            let src = if p.is_still(r.media) { r.source_in } else { r.source_in + d };
            changes.push((r.id, timing(r), (r.timeline_start + d, src, r.duration - d)));
        }
    }
    changes.retain(|(_, a, b)| a != b);
    changes.dedup_by_key(|(id, ..)| *id);
    Some(TrimPlan { applied: d, changes, mode: TrimMode::Slide, limited: d != delta })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::undo::{AddClip, UndoStack};
    use crate::{FrameRate, Rational};

    fn s(x: f64) -> Time {
        Time::from_secs_f64(x)
    }

    fn media(id: u64, secs: f64) -> MediaAsset {
        MediaAsset {
            id: MediaId(id),
            path: format!("{id}.mp4").into(),
            name: String::new(),
            info: MediaInfo {
                duration: s(secs),
                video: Some(VideoStreamInfo { width: 8, height: 8, frame_rate: FrameRate::new(30, 1), codec: String::new(), time_base: Rational::new(1, 30), hdr: None }),
                audio: None,
                container: String::new(),
            },
            conform: None,
        }
    }

    fn clip(id: u64, start: f64, src: f64, dur: f64) -> Clip {
        Clip::new(ClipId(id), MediaId(1), s(start), s(src), s(dur), None)
    }

    /// V1: A [0–4) src 1–5, B [4–8) src 2–6. V2: title [6–7). A1: A's audio, linked.
    fn project() -> Project {
        let mut p = Project::new();
        p.media.push(media(1, 10.0));
        let mut a = clip(1, 0.0, 1.0, 4.0);
        a.link = Some(100);
        let mut a_audio = a.clone();
        a_audio.id = ClipId(11);
        let b = clip(2, 4.0, 2.0, 4.0);
        let title = clip(3, 6.0, 0.0, 1.0);
        let seq = &mut p.sequences[0];
        seq.tracks[0].clips = vec![a, b];
        seq.tracks[1].clips = vec![title];
        seq.tracks[2].clips = vec![a_audio];
        p
    }

    fn at(p: &Project, id: u64) -> Timing {
        let (_, c) = p.sequences[0].find_clip(ClipId(id)).unwrap();
        (c.timeline_start, c.source_in, c.duration)
    }

    #[test]
    fn ripple_end_pushes_everything_after_on_all_tracks() {
        let mut p = project();
        let plan = plan_trim(&p, 0, ClipId(1), Edge::End, TrimMode::Ripple, s(1.0)).unwrap();
        assert!(!plan.limited);
        plan.apply(&mut p, 0);
        assert_eq!(at(&p, 1), (s(0.0), s(1.0), s(5.0)), "A longer");
        assert_eq!(at(&p, 11), (s(0.0), s(1.0), s(5.0)), "its linked audio too");
        assert_eq!(at(&p, 2).0, s(5.0), "B pushed later");
        assert_eq!(at(&p, 3).0, s(7.0), "title on V2 stays in sync");
    }

    #[test]
    fn ripple_start_keeps_the_clip_in_place_and_pulls_the_rest() {
        let mut p = project();
        let plan = plan_trim(&p, 0, ClipId(1), Edge::Start, TrimMode::Ripple, s(0.5)).unwrap();
        plan.apply(&mut p, 0);
        assert_eq!(at(&p, 1), (s(0.0), s(1.5), s(3.5)), "same start, later in-point, shorter");
        assert_eq!(at(&p, 2).0, s(3.5));
        assert_eq!(at(&p, 3).0, s(5.5));
    }

    #[test]
    fn ripple_shrink_stops_at_a_clip_that_spans_the_edit() {
        let mut p = project();
        // A title on V2 from 3.5 to 4.5 straddles A's end (4 s): later clips on V2 can't slide under it.
        p.sequences[0].tracks[1].clips.insert(0, clip(4, 3.5, 0.0, 1.0));
        // The V2 title at 6 s can move back to 4.5 at most: shift ≥ −1.5 s.
        let plan = plan_trim(&p, 0, ClipId(1), Edge::End, TrimMode::Ripple, s(-3.0)).unwrap();
        assert!(plan.limited);
        assert_eq!(plan.applied, s(-1.5));
    }

    #[test]
    fn ripple_respects_media_limits() {
        let p = project();
        // A shows source 1–5 of a 10 s file: it can grow by at most 5 s at the end, 1 s at the start.
        assert_eq!(plan_trim(&p, 0, ClipId(1), Edge::End, TrimMode::Ripple, s(9.0)).unwrap().applied, s(5.0));
        assert_eq!(plan_trim(&p, 0, ClipId(1), Edge::Start, TrimMode::Ripple, s(-9.0)).unwrap().applied, s(-1.0));
    }

    #[test]
    fn roll_moves_the_cut_and_nothing_else() {
        let mut p = project();
        let before_total = at(&p, 1).2 + at(&p, 2).2;
        let plan = plan_trim(&p, 0, ClipId(1), Edge::End, TrimMode::Roll, s(1.0)).unwrap();
        assert_eq!(plan.mode, TrimMode::Roll);
        plan.apply(&mut p, 0);
        assert_eq!(at(&p, 1), (s(0.0), s(1.0), s(5.0)));
        assert_eq!(at(&p, 2), (s(5.0), s(3.0), s(3.0)), "B starts later and later in its media");
        assert_eq!(at(&p, 1).2 + at(&p, 2).2, before_total, "total length unchanged");
        assert_eq!(at(&p, 3).0, s(6.0), "other tracks untouched");
        assert_eq!(at(&p, 11), (s(0.0), s(1.0), s(5.0)), "A's lone audio is trimmed normally, staying in sync");
    }

    #[test]
    fn roll_is_limited_by_spare_media_on_both_sides() {
        let p = project();
        // Cut earlier: B's in-point is 2 s, so the cut can move back at most 2 s (A can shrink 4 s).
        assert_eq!(plan_trim(&p, 0, ClipId(2), Edge::Start, TrimMode::Roll, s(-3.0)).unwrap().applied, s(-2.0));
        // Cut later: A has 5 s spare after its out-point, B is only 4 s long (keeps one frame).
        let d = plan_trim(&p, 0, ClipId(2), Edge::Start, TrimMode::Roll, s(10.0)).unwrap().applied;
        assert_eq!(d, s(4.0) - p.sequences[0].frame_rate.frame_duration());
    }

    #[test]
    fn roll_without_a_neighbour_is_a_normal_trim() {
        let p = project();
        let plan = plan_trim(&p, 0, ClipId(2), Edge::End, TrimMode::Roll, s(1.0)).unwrap();
        assert_eq!(plan.mode, TrimMode::Normal);
    }

    #[test]
    fn slip_changes_the_media_but_not_the_position() {
        let mut p = project();
        // A shows 1–5 of a 10 s file: it can slip 1 s earlier or 5 s later.
        let plan = plan_slip(&p, 0, ClipId(1), s(2.0)).unwrap();
        assert!(!plan.limited);
        plan.apply(&mut p, 0);
        assert_eq!(at(&p, 1), (s(0.0), s(3.0), s(4.0)));
        assert_eq!(at(&p, 11), (s(0.0), s(3.0), s(4.0)), "linked audio slips with it");
        assert_eq!(at(&p, 2).0, s(4.0), "neighbours untouched");
        let back = plan_slip(&p, 0, ClipId(1), s(-9.0)).unwrap();
        assert!(back.limited);
        assert_eq!(back.applied, s(-3.0), "can't show media before its start");
        assert_eq!(plan_slip(&p, 0, ClipId(1), s(9.0)).unwrap().applied, s(3.0), "or past its end");
    }

    #[test]
    fn slide_moves_the_clip_and_adjusts_both_neighbours() {
        let mut p = project();
        // V1: A [0–4) src 1, B [4–8) src 2, C [8–10) src 5 (all from a 10 s file).
        p.sequences[0].tracks[0].clips.push(clip(4, 8.0, 5.0, 2.0));
        let plan = plan_slide(&p, 0, ClipId(2), s(1.0)).unwrap();
        assert!(!plan.limited);
        plan.apply(&mut p, 0);
        assert_eq!(at(&p, 2), (s(5.0), s(2.0), s(4.0)), "B moves, same content and length");
        assert_eq!(at(&p, 1), (s(0.0), s(1.0), s(5.0)), "A gets longer");
        assert_eq!(at(&p, 4), (s(9.0), s(6.0), s(1.0)), "C starts later and shorter");
        assert_eq!(at(&p, 3).0, s(6.0), "other tracks untouched");
    }

    #[test]
    fn slide_limits() {
        let mut p = project();
        p.sequences[0].tracks[0].clips.push(clip(4, 8.0, 5.0, 2.0));
        let frame = p.sequences[0].frame_rate.frame_duration();
        // Right: C can lose all but a frame of its 2 s; A has 5 s spare media.
        assert_eq!(plan_slide(&p, 0, ClipId(2), s(5.0)).unwrap().applied, s(2.0) - frame);
        // Left: C's in-point (5 s) allows 5 s, A can shrink to a frame (4 s − 1 frame).
        assert_eq!(plan_slide(&p, 0, ClipId(2), s(-9.0)).unwrap().applied, frame - s(4.0));
        // With a gap after it instead of a neighbour, B can slide into the gap but not past.
        p.sequences[0].tracks[0].clips.retain(|c| c.id != ClipId(4));
        p.sequences[0].tracks[0].clips.push(clip(5, 9.0, 0.0, 1.0));
        assert_eq!(plan_slide(&p, 0, ClipId(2), s(5.0)).unwrap().applied, s(1.0));
    }

    #[test]
    fn locked_tracks_are_left_alone() {
        let mut p = project();
        p.sequences[0].tracks[1].locked = true; // V2 (the title)
        let plan = plan_trim(&p, 0, ClipId(1), Edge::End, TrimMode::Ripple, s(1.0)).unwrap();
        plan.apply(&mut p, 0);
        assert_eq!(at(&p, 2).0, s(5.0), "B on V1 still ripples");
        assert_eq!(at(&p, 3).0, s(6.0), "the title on the locked track stays");
        assert!(plan_trim(&p, 0, ClipId(3), Edge::End, TrimMode::Normal, s(0.5)).is_none(), "can't trim a locked clip");
        assert!(plan_slip(&p, 0, ClipId(3), s(0.5)).is_none());
        assert!(ripple_delete(&p, 0, ClipId(3)).is_none());
        // Locking A's audio track locks A too (they're linked).
        p.sequences[0].tracks[2].locked = true;
        assert!(p.sequences[0].is_locked(ClipId(1)));
        assert!(plan_trim(&p, 0, ClipId(1), Edge::End, TrimMode::Normal, s(-0.5)).is_none());
        assert!(crate::undo::move_group(&p, 0, ClipId(1), (0, s(20.0))).is_err());
    }

    #[test]
    fn a_ripple_is_stopped_by_a_locked_clip_in_the_way() {
        let mut p = project();
        // The title on V2 (6–7 s) is linked to a clip on a locked audio track, so it stays put;
        // another clip on V2 at 8 s ripples back, but only until it meets the title.
        let mut a2 = Track::new("Audio 2", TrackKind::Audio);
        a2.locked = true;
        let mut sound = clip(12, 6.0, 0.0, 1.0);
        sound.link = Some(300);
        a2.clips.push(sound);
        p.sequences[0].tracks.push(a2);
        p.sequences[0].tracks[1].clips[0].link = Some(300);
        p.sequences[0].tracks[1].clips.push(clip(13, 8.0, 0.0, 1.0));
        let plan = plan_trim(&p, 0, ClipId(1), Edge::End, TrimMode::Ripple, s(-3.0)).unwrap();
        assert!(plan.limited);
        assert_eq!(plan.applied, s(-1.0));
        plan.apply(&mut p, 0);
        assert_eq!(at(&p, 3).0, s(6.0), "locked title stays");
        assert_eq!(at(&p, 13).0, s(7.0), "the clip after it closes up to it");
    }

    #[test]
    fn ripple_delete_closes_the_gap_and_undoes() {
        let mut p = project();
        let mut u = UndoStack::default();
        u.execute(&mut p, Box::new(AddClip { seq: 0, track: 0, clip: clip(5, 9.0, 0.0, 1.0) }));
        let (cmd, closed) = ripple_delete(&p, 0, ClipId(2)).unwrap();
        // B (4–8) goes; the V2 title at 6 s starts inside the deleted range so it stays,
        // and it blocks the clip at 9 s on V1? No: different track. V1's clip at 9 can move to 4.
        // But the title (6–7) is "after" B's end? It starts at 6 < 8, so it doesn't move.
        assert_eq!(closed, s(4.0));
        u.execute(&mut p, cmd);
        assert!(p.sequences[0].find_clip(ClipId(2)).is_none());
        assert_eq!(at(&p, 5).0, s(5.0), "clip after the gap pulled back by 4 s");
        assert_eq!(at(&p, 3).0, s(6.0), "title that overlapped the deleted clip stays");
        u.undo(&mut p);
        assert_eq!(at(&p, 5).0, s(9.0));
        assert!(p.sequences[0].find_clip(ClipId(2)).is_some());
    }
}
