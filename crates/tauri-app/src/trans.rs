//! Transitions and speed for the web interface: the transitions on the
//! timeline, the selected clip's transition in, the list of kinds, and speed
//! ramps.

use crate::{ed, Res};
use reelforge_core::{Clip, ClipId, Project, Time, TrackKind, Transition};
use reelforge_engine::editor::Editor;
use serde::Serialize;

/// A transition drawn on the timeline.
#[derive(Serialize)]
pub struct TransitionMark {
    pub track: usize,
    pub left: u64,
    pub right: u64,
    pub cut: i64,
    pub start: i64,
    pub end: i64,
    /// What was asked for (the drawn box can be shorter: spare media).
    pub requested: i64,
    pub label: &'static str,
    /// No spare media at all: it plays as a cut.
    pub empty: bool,
    pub audio: bool,
}

pub fn marks(p: &Project, seq: usize) -> Vec<TransitionMark> {
    let s = &p.sequences[seq];
    p.transitions(seq)
        .into_iter()
        .map(|x| TransitionMark {
            track: x.track,
            left: x.left.0,
            right: x.right.0,
            cut: x.cut.0,
            start: x.start().0,
            end: x.end().0,
            requested: x.requested.0,
            label: if x.kind == TrackKind::Audio { "Crossfade" } else { s.find_clip(x.right).map(|(_, c)| c.transition.label()).unwrap_or("Dissolve") },
            empty: x.is_empty(),
            audio: x.kind == TrackKind::Audio,
        })
        .collect()
}

/// The transition into the selected clip (when there's a clip right before it).
#[derive(Serialize)]
pub struct TransitionIn {
    /// None for an audio crossfade.
    pub kind: Option<Transition>,
    /// The length asked for, and what plays (less when there isn't enough spare media).
    pub length: i64,
    pub actual: i64,
    /// The longest it can be (the shorter of the two clips).
    pub limit: i64,
}

pub fn transition_in(e: &Editor, c: &Clip, track: TrackKind) -> Option<TransitionIn> {
    let left = e.project.clip_before(e.active, c.id)?;
    let left_dur = e.seq().find_clip(left)?.1.duration;
    let length = if track == TrackKind::Audio { c.audio.crossfade_in } else { c.dissolve_in };
    let actual = e.project.crossfade_into(e.active, c.id).map_or(Time::ZERO, |x| x.len());
    Some(TransitionIn { kind: (track == TrackKind::Video).then_some(c.transition), length: length.0, actual: actual.0, limit: left_dur.min(c.duration).0 })
}

#[derive(Serialize)]
pub struct TransitionKind {
    pub kind: Transition,
    pub label: &'static str,
    pub description: &'static str,
    pub group: &'static str,
}

/// Every kind of transition, by group.
#[tauri::command]
pub fn transition_list() -> Vec<TransitionKind> {
    Transition::ALL
        .into_iter()
        .map(|k| TransitionKind {
            kind: k,
            label: k.label(),
            description: k.description(),
            group: if k.is_wipe() {
                "Wipes"
            } else if k.is_slide() {
                "Slides"
            } else if k.is_push() {
                "Pushes"
            } else {
                "Dissolves"
            },
        })
        .collect()
}

/// Put a transition of `kind` into clip `id`, or (no id) on the video cut nearest the playhead.
#[tauri::command]
pub fn transition_apply(kind: Transition, id: Option<u64>) -> Res<()> {
    ed(|e| match id {
        Some(id) => e.apply_transition_to(ClipId(id), kind),
        None => e.apply_transition(kind),
    })
}

#[tauri::command]
pub fn transition_kind(id: u64, kind: Transition) -> Res<()> {
    ed(|e| e.set_transition_kind(ClipId(id), kind))
}

/// The length of the transition into clip `id` (live: dragging or typing; `commit` ends it).
#[tauri::command]
pub fn transition_length(id: u64, length: i64) -> Res<()> {
    ed(|e| {
        let id = ClipId(id);
        let rate = e.seq().frame_rate;
        let Some((ti, c)) = e.seq().find_clip(id) else { return };
        let Some(left) = e.project.clip_before(e.active, id).and_then(|l| e.seq().find_clip(l)).map(|(_, l)| l.duration) else { return };
        let d = Time(length).snap(rate).max(Time::ZERO).min(left.min(c.duration));
        if e.seq().tracks[ti].kind == TrackKind::Audio {
            let mut a = c.audio.clone();
            a.crossfade_in = d;
            e.set_clip_audio_live(id, a);
        } else {
            e.set_dissolve_live(id, d);
        }
    })
}

#[tauri::command]
pub fn transition_remove(id: u64) -> Res<()> {
    ed(|e| e.remove_transition(ClipId(id)))
}

/// Ramp the clip's speed from `from` to `to` (fractions) in `steps` parts.
#[tauri::command]
pub fn speed_ramp(id: u64, from: f32, to: f32, steps: usize, smooth: bool) -> Res<()> {
    ed(|e| e.speed_ramp(ClipId(id), from, to, steps, smooth))
}
