//! The Viewer's mouse: clicking a layer selects it (topmost wins), dragging
//! inside the selected one moves it (snapping), and its gizmo handles scale,
//! stretch, crop, rotate, move the anchor or move corner-pin corners. Points
//! come in frame pixels, with `scale` = screen pixels per frame pixel.

use crate::{ed, Res};
use reelforge_engine::editor::Editor;
use reelforge_engine::gizmo::{self, DragState, Geometry, Handle, MoveState};
use serde::Serialize;
use std::cell::RefCell;

enum Drag {
    Handle(DragState, gizmo::GizmoLayer, reelforge_core::ClipId),
    Move(MoveState),
}

thread_local! {
    static DRAG: RefCell<Option<(Drag, bool)>> = const { RefCell::new(None) };
}

/// The selected layer's gizmo, for drawing.
pub fn geometry(e: &Editor) -> Option<Geometry> {
    let (layer, t) = e.gizmo_layer()?;
    Some(gizmo::geometry(&t, &layer, e.frame_px()))
}

#[derive(Serialize)]
pub struct Hover {
    pub handle: Handle,
    /// The handle's direction on screen in degrees (None: rotate or anchor).
    pub dir: Option<f32>,
    pub tip: &'static str,
}

/// The selected layer's handle under the pointer, if any.
#[tauri::command]
pub fn viewer_hover(at: [f32; 2], ctrl: bool, scale: f32) -> Res<Option<Hover>> {
    ed(|e| {
        if e.playback.playing {
            return None;
        }
        let (layer, t) = e.gizmo_layer()?;
        let h = gizmo::handle_at(&t, &layer, e.frame_px(), scale, at, ctrl)?;
        let (dir, tip) = gizmo::hint(h, ctrl && matches!(h, Handle::Edge(_)), &t);
        Some(Hover { handle: h, dir, tip })
    })
}

/// A press in the viewer: grab a handle of the selected layer, or select the
/// layer under the pointer and start moving it. Returns "handle", "move" or "none".
#[tauri::command]
pub fn viewer_press(at: [f32; 2], ctrl: bool, scale: f32) -> Res<&'static str> {
    ed(|e| {
        e.commit_live_edit();
        DRAG.with(|d| *d.borrow_mut() = None);
        let frame = e.frame_px();
        if !e.playback.playing {
            if let Some((layer, t)) = e.gizmo_layer() {
                if let Some(h) = gizmo::handle_at(&t, &layer, frame, scale, at, ctrl) {
                    let crop = ctrl && matches!(h, Handle::Edge(_));
                    let st = DragState::new(h, crop, t, frame, at);
                    DRAG.with(|d| *d.borrow_mut() = Some((Drag::Handle(st, layer, layer.id), false)));
                    return "handle";
                }
            }
        }
        let layers = e.viewer_layers();
        let keep = e.selection.is_some_and(|sel| layers.iter().any(|(l, tr)| l.id == sel && Editor::layer_shows(l, tr, frame, at)));
        if !keep {
            let hit = layers.iter().find(|(l, tr)| Editor::layer_shows(l, tr, frame, at)).map(|(l, _)| l.id);
            e.select_only(hit);
        }
        if e.playback.playing {
            return "none";
        }
        match e.gizmo_layer() {
            Some((layer, t)) if Editor::layer_shows(&layer, &t, frame, at) || keep => {
                DRAG.with(|d| *d.borrow_mut() = Some((Drag::Move(MoveState { id: layer.id, start: t, from: at }), false)));
                "move"
            }
            _ => "none",
        }
    })
}

#[derive(Serialize, Default)]
pub struct Feedback {
    pub readout: String,
    pub guides_x: Vec<f32>,
    pub guides_y: Vec<f32>,
}

/// The pointer moved during a viewer drag (live; `viewer_release` ends it).
#[tauri::command]
pub fn viewer_drag(at: [f32; 2], shift: bool, alt: bool, scale: f32) -> Res<Feedback> {
    ed(|e| {
        let frame = e.frame_px();
        let mut out = Feedback::default();
        let layers = e.viewer_layers();
        let snapping = e.prefs.viewer_snapping;
        let edit = DRAG.with(|d| {
            let mut d = d.borrow_mut();
            let (drag, moved) = d.as_mut()?;
            *moved = true;
            match drag {
                Drag::Handle(st, layer, id) => {
                    let t = gizmo::dragged(st, layer, frame, scale, at, shift);
                    out.readout = gizmo::readout(st, &t);
                    Some((*id, t))
                }
                Drag::Move(m) => {
                    let mut dlt = [at[0] - m.from[0], at[1] - m.from[1]];
                    if shift {
                        // Straight across or down.
                        if dlt[0].abs() > dlt[1].abs() {
                            dlt[1] = 0.0
                        } else {
                            dlt[0] = 0.0
                        }
                    }
                    let (t, g) = gizmo::moved(m, &layers, frame, scale, dlt, alt || !snapping);
                    out.readout = format!("Position {:.0}, {:.0}", t.position[0], t.position[1]);
                    out.guides_x = g.guides_x;
                    out.guides_y = g.guides_y;
                    Some((m.id, t))
                }
            }
        });
        if let Some((id, t)) = edit {
            e.gizmo_apply(id, t);
        }
        out
    })
}

/// The end of a viewer drag: one undo step.
#[tauri::command]
pub fn viewer_release() -> Res<()> {
    ed(|e| {
        let moved = DRAG.with(|d| d.borrow_mut().take()).is_some_and(|(_, m)| m);
        if moved {
            e.commit_live_edit();
        }
    })
}
