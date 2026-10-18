//! The transform gizmo in the Viewer, without any drawing: the selected
//! layer's bounding box with handles to scale (corners and edges), rotate (the
//! stem above the top edge, or just outside a corner), crop (Ctrl + an edge
//! handle) and move the anchor point (Ctrl + the cross). Dragging inside the
//! box moves the layer, snapping to the frame and the other layers.
//!
//! A corner-pinned layer shows its four pin corners instead: drag one to move
//! that corner (Shift: straight along the frame's axes).
//!
//! Every drag is worked out from the transform at the start of the drag and
//! the pointer now, so the result never drifts. Points are frame pixels;
//! `scale` is screen pixels per frame pixel (for the grab distances).

use crate::editor::Editor;
use crate::snap;
use reelforge_core::{ClipId, Time, Transform};
use serde::Serialize;

/// Screen-pixel sizes.
pub const GRAB: f32 = 8.0;
pub const STEM: f32 = 26.0;

/// A layer the gizmo can be on.
#[derive(Clone, Copy, Debug)]
pub struct GizmoLayer {
    pub id: ClipId,
    /// Layer-local rectangle that counts as the picture (the text, for a title), before cropping.
    pub rect: [f32; 4],
    /// The layer's size at 100% (for the crop fractions).
    pub size: [f32; 2],
}

/// What a press grabbed.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", content = "index", rename_all = "lowercase")]
pub enum Handle {
    /// A corner, by index (top-left, top-right, bottom-right, bottom-left).
    Corner(usize),
    /// An edge, by index (top, right, bottom, left).
    Edge(usize),
    Rotate,
    Anchor,
}

/// A handle drag under way.
#[derive(Clone, Copy, Debug)]
pub struct DragState {
    pub handle: Handle,
    pub crop: bool,
    pub start: Transform,
    /// Pointer at the start, in frame pixels.
    pub from: [f32; 2],
    /// For rotation: the pointer angle last time and the total turned so far.
    last_angle: f32,
    turned: f32,
}

impl DragState {
    pub fn new(handle: Handle, crop: bool, start: Transform, frame: [f32; 2], from: [f32; 2]) -> Self {
        DragState { handle, crop, start, from, last_angle: angle(&start, frame, from), turned: 0.0 }
    }
}

/// Where everything grabbable is, in frame pixels.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct Geometry {
    pub corners: [[f32; 2]; 4],
    pub edges: [[f32; 2]; 4],
    /// The rotation stem: from the middle of the top edge, `STEM` screen pixels out.
    pub stem_base: [f32; 2],
    /// Unit direction of the stem (on screen and in the frame alike).
    pub stem_dir: [f32; 2],
    pub anchor: [f32; 2],
    /// The outline before cropping (the same as `corners` when not cropped).
    pub full: [[f32; 2]; 4],
    pub pinned: bool,
    pub cropped: bool,
}

pub fn geometry(t: &Transform, layer: &GizmoLayer, frame: [f32; 2]) -> Geometry {
    let r = t.clip_to_crop(layer.rect, layer.size);
    let pin = t.valid_pin();
    let corners = match pin {
        // A pinned layer's handles are its pin corners.
        Some(pin) => pin.0,
        None => t.rect_corners(r, frame),
    };
    let mid = |a: [f32; 2], b: [f32; 2]| [(a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0];
    let edges = [mid(corners[0], corners[1]), mid(corners[1], corners[2]), mid(corners[2], corners[3]), mid(corners[3], corners[0])];
    let centre = mid(corners[0], corners[2]);
    // The stem points away from the middle of the box, through the top edge.
    let (dx, dy) = (edges[0][0] - centre[0], edges[0][1] - centre[1]);
    let len = (dx * dx + dy * dy).sqrt();
    let stem_dir = if len > 1e-4 && len.is_finite() { [dx / len, dy / len] } else { [0.0, -1.0] };
    let anchor = t.matrix(frame).apply(t.anchor);
    let full = if pin.is_some() { corners } else { t.rect_corners(layer.rect, frame) };
    Geometry { corners, edges, stem_base: edges[0], stem_dir, anchor, full, pinned: pin.is_some(), cropped: pin.is_none() && t.is_cropped() }
}

fn dist(a: [f32; 2], b: [f32; 2]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
}

impl Geometry {
    pub fn stem_tip(&self, scale: f32) -> [f32; 2] {
        let k = STEM / scale.max(1e-4);
        [self.stem_base[0] + self.stem_dir[0] * k, self.stem_base[1] + self.stem_dir[1] * k]
    }
    /// Too small on screen for edge handles to be told apart from the corners.
    pub fn small(&self, scale: f32) -> bool {
        let c = &self.corners;
        dist(c[0], c[1]) * scale < 4.0 * GRAB || dist(c[1], c[2]) * scale < 4.0 * GRAB
    }
}

/// Which handle (if any) is at frame point `p`. The anchor can only be
/// grabbed with Ctrl held (`ctrl`), so dragging the middle of a layer moves it.
pub fn handle_at(t: &Transform, layer: &GizmoLayer, frame: [f32; 2], scale: f32, p: [f32; 2], ctrl: bool) -> Option<Handle> {
    let g = geometry(t, layer, frame);
    let grab = GRAB / scale.max(1e-4);
    let near = |q: [f32; 2]| dist(q, p) <= grab;
    if g.pinned {
        return g.corners.iter().position(|c| near(*c)).map(Handle::Corner);
    }
    if ctrl && near(g.anchor) {
        return Some(Handle::Anchor);
    }
    if near(g.stem_tip(scale)) {
        return Some(Handle::Rotate);
    }
    if let Some(i) = g.corners.iter().position(|c| near(*c)) {
        return Some(Handle::Corner(i));
    }
    if !g.small(scale) {
        if let Some(i) = g.edges.iter().position(|c| near(*c)) {
            return Some(Handle::Edge(i));
        }
    }
    // Just outside a corner: rotate.
    let inside = t.hit_rect(t.clip_to_crop(layer.rect, layer.size), frame, p);
    if !inside && g.corners.iter().any(|c| dist(*c, p) <= grab + 16.0 / scale.max(1e-4)) {
        return Some(Handle::Rotate);
    }
    None
}

/// The pointer relative to the anchor, in the layer's (rotated) axes, in frame pixels.
fn local_offset(t: &Transform, frame: [f32; 2], p: [f32; 2]) -> [f32; 2] {
    let a = t.matrix(frame).apply(t.anchor);
    let (s, c) = t.rotation.to_radians().sin_cos();
    let (dx, dy) = (p[0] - a[0], p[1] - a[1]);
    [dx * c + dy * s, -dx * s + dy * c]
}

fn angle(t: &Transform, frame: [f32; 2], p: [f32; 2]) -> f32 {
    let a = t.matrix(frame).apply(t.anchor);
    (p[1] - a[1]).atan2(p[0] - a[0]).to_degrees()
}

/// The transform a handle drag gives, from where it started and the pointer now (`p`).
pub fn dragged(d: &mut DragState, layer: &GizmoLayer, frame: [f32; 2], scale: f32, p: [f32; 2], shift: bool) -> Transform {
    let s = d.start;
    let mut t = s;
    if let (Some(pin), Handle::Corner(i)) = (s.valid_pin(), d.handle) {
        let mut delta = [p[0] - d.from[0], p[1] - d.from[1]];
        if shift {
            // Only along whichever axis it has moved further.
            if delta[0].abs() > delta[1].abs() {
                delta[1] = 0.0
            } else {
                delta[0] = 0.0
            }
        }
        let mut q = pin;
        q.0[i] = [pin.0[i][0] + delta[0], pin.0[i][1] + delta[1]];
        // A corner dragged across the shape would twist it: keep the last good one.
        if q.is_valid() {
            t.pin = Some(q);
        }
        return t;
    }
    match d.handle {
        Handle::Corner(_) | Handle::Edge(_) if d.crop => {
            if let (Handle::Edge(e), Some(inv)) = (d.handle, s.matrix(frame).inverse()) {
                let [x, y] = inv.apply(p);
                let [w, h] = [layer.size[0].max(1.0), layer.size[1].max(1.0)];
                let mut c = s.crop_clamped();
                match e {
                    0 => c[1] = ((y + h / 2.0) / h).clamp(0.0, 1.0 - c[3] - 0.001),
                    1 => c[2] = ((w / 2.0 - x) / w).clamp(0.0, 1.0 - c[0] - 0.001),
                    2 => c[3] = ((h / 2.0 - y) / h).clamp(0.0, 1.0 - c[1] - 0.001),
                    _ => c[0] = ((x + w / 2.0) / w).clamp(0.0, 1.0 - c[2] - 0.001),
                }
                t.crop = c;
            }
        }
        Handle::Corner(_) => {
            let v0 = local_offset(&s, frame, d.from);
            let v = local_offset(&s, frame, p);
            if shift {
                // Free: each axis on its own.
                for i in 0..2 {
                    if v0[i].abs() > 0.5 {
                        t.scale[i] = s.scale[i] * v[i] / v0[i];
                    }
                }
            } else {
                // Uniform: how far along the line from the anchor through the grabbed point.
                let len2 = v0[0] * v0[0] + v0[1] * v0[1];
                if len2 > 0.25 {
                    let f = (v[0] * v0[0] + v[1] * v0[1]) / len2;
                    t.scale = [s.scale[0] * f, s.scale[1] * f];
                }
            }
        }
        Handle::Edge(e) => {
            let i = if e % 2 == 0 { 1 } else { 0 };
            let v0 = local_offset(&s, frame, d.from);
            let v = local_offset(&s, frame, p);
            if v0[i].abs() > 0.5 {
                let f = v[i] / v0[i];
                t.scale[i] = s.scale[i] * f;
                if shift {
                    t.scale[1 - i] = s.scale[1 - i] * f;
                }
            }
        }
        Handle::Rotate => {
            let a = angle(&s, frame, p);
            let mut step = a - d.last_angle;
            step = (step + 180.0).rem_euclid(360.0) - 180.0;
            d.last_angle = a;
            d.turned += step;
            t.rotation = s.rotation + d.turned;
            if shift {
                t.rotation = (t.rotation / 15.0).round() * 15.0;
            }
        }
        Handle::Anchor => {
            if let Some(inv) = s.matrix(frame).inverse() {
                let mut a = inv.apply(p);
                // Snap to the middle, corners and edge midpoints of the picture (8 screen px).
                let r = s.clip_to_crop(layer.rect, layer.size);
                let (cx, cy) = ((r[0] + r[2]) / 2.0, (r[1] + r[3]) / 2.0);
                let snaps = [[cx, cy], [r[0], r[1]], [r[2], r[1]], [r[2], r[3]], [r[0], r[3]], [cx, r[1]], [r[2], cy], [cx, r[3]], [r[0], cy]];
                let m = s.matrix(frame);
                if !shift {
                    if let Some(q) = snaps.iter().find(|q| dist(m.apply(**q), p) * scale <= 8.0) {
                        a = *q;
                    }
                }
                t = s.with_anchor_kept_in_place(a, frame);
            }
        }
    }
    t
}

/// A short readout of what a drag is changing.
pub fn readout(d: &DragState, t: &Transform) -> String {
    if let (Some(pin), Handle::Corner(i)) = (t.valid_pin(), d.handle) {
        let names = ["Top left", "Top right", "Bottom right", "Bottom left"];
        return format!("Corner pin: {}  {:.1}, {:.1}", names[i], pin.0[i][0], pin.0[i][1]);
    }
    match d.handle {
        Handle::Edge(_) if d.crop => {
            let c = t.crop_clamped().map(|v| v * 100.0);
            format!("Crop  L {:.1}%  T {:.1}%  R {:.1}%  B {:.1}%", c[0], c[1], c[2], c[3])
        }
        Handle::Corner(_) | Handle::Edge(_) => format!("Scale {:.1}% × {:.1}%", t.scale[0] * 100.0, t.scale[1] * 100.0),
        Handle::Rotate => format!("Rotation {:.1}°", t.rotation),
        Handle::Anchor => format!("Anchor {:.0}, {:.0}", t.anchor[0], t.anchor[1]),
    }
}

/// What the pointer would do over a handle: a hint for the cursor (the
/// handle's direction on screen, in degrees; None = rotate or anchor) and a tooltip.
pub fn hint(h: Handle, crop: bool, t: &Transform) -> (Option<f32>, &'static str) {
    let dir = match h {
        Handle::Corner(i) => Some([-45.0, 45.0, 135.0, 225.0][i]),
        Handle::Edge(i) => Some([-90.0, 0.0, 90.0, 180.0][i]),
        _ => None,
    };
    let tip = match h {
        Handle::Corner(_) if t.valid_pin().is_some() => "Drag to move this corner of the corner pin (Shift: straight across or down)",
        Handle::Corner(_) => "Drag to scale (Shift: X and Y separately)",
        Handle::Edge(_) if crop => "Drag to crop this edge",
        Handle::Edge(_) => "Drag to stretch (Shift: both ways). Ctrl+drag crops this edge",
        Handle::Rotate => "Drag to rotate around the anchor (Shift: 15° steps)",
        Handle::Anchor => "Drag to move the anchor point, the pivot for scale and rotation (snaps to the middle, corners and edges; Shift: no snapping)",
    };
    (dir.map(|d: f32| (d + t.rotation).rem_euclid(360.0)), tip)
}

/// A layer being moved by dragging inside it.
#[derive(Clone, Copy, Debug)]
pub struct MoveState {
    pub id: ClipId,
    pub start: Transform,
    pub from: [f32; 2],
}

/// The move a drag of `d` (frame pixels) gives, snapped (unless `free`) to the
/// frame and the other layers, with the guide lines that lined up.
pub fn moved(m: &MoveState, layers: &[(GizmoLayer, Transform)], frame: [f32; 2], scale: f32, d: [f32; 2], free: bool) -> (Transform, snap::Snapped) {
    let mut d = d;
    let mut guides = snap::Snapped::default();
    if !free {
        let outline = |l: &GizmoLayer, tr: &Transform| {
            let pts = if tr.valid_pin().is_some() { tr.corners(l.size, frame) } else { tr.rect_corners(tr.clip_to_crop(l.rect, l.size), frame) };
            snap::bounds(&pts)
        };
        if let Some((layer, _)) = layers.iter().find(|(l, _)| l.id == m.id) {
            let b = outline(layer, &m.start);
            let others: Vec<snap::Bounds> = layers.iter().filter(|(l, _)| l.id != m.id).map(|(l, tr)| outline(l, tr)).collect();
            let targets = snap::Targets::new(frame, &others);
            guides = snap::snap([b[0] + d[0], b[1] + d[1], b[2] + d[0], b[3] + d[1]], &targets, 8.0 / scale.max(1e-4));
            d = [d[0] + guides.offset[0], d[1] + guides.offset[1]];
        }
    }
    let mut t = m.start;
    match m.start.valid_pin() {
        // A pinned layer moves by moving all four corners.
        Some(pin) => t.pin = Some(pin.map(|p| [p[0] + d[0], p[1] + d[1]])),
        None => t.position = [m.start.position[0] + d[0], m.start.position[1] + d[1]],
    }
    (t, guides)
}

impl Editor {
    /// The frame size of the active sequence, in pixels.
    pub fn frame_px(&self) -> [f32; 2] {
        [self.seq().width as f32, self.seq().height as f32]
    }

    /// The visible video layers at the playhead, topmost first, each as the
    /// rectangle that counts as the layer (the whole picture for footage,
    /// just the text and its box for a title) with its transform there.
    pub fn viewer_layers(&self) -> Vec<(GizmoLayer, Transform)> {
        let t: Time = self.playback.playhead;
        let frame = self.frame_px();
        self.project
            .video_layers_at(self.active, t)
            .iter()
            .rev()
            .flat_map(|item| item.layers().into_iter().rev().map(|l| (l.clip, l.transform)).collect::<Vec<_>>())
            .filter_map(|(c, tr)| {
                if c.title.is_some() {
                    let b = self.preview.title_bounds(c.id)?;
                    let (hw, hh) = (frame[0] / 2.0, frame[1] / 2.0);
                    return Some((GizmoLayer { id: c.id, rect: [b[0] - hw, b[1] - hh, b[2] - hw, b[3] - hh], size: frame }, tr));
                }
                let [w, h] = self.project.picture_size(c)?;
                let size = [w as f32, h as f32];
                Some((GizmoLayer { id: c.id, rect: Transform::full_rect(size), size }, tr))
            })
            .collect()
    }

    /// Is frame point `f` on layer `l` (shown with `tr`)?
    pub fn layer_shows(l: &GizmoLayer, tr: &Transform, frame: [f32; 2], f: [f32; 2]) -> bool {
        if tr.valid_pin().is_some() {
            tr.hit(l.size, frame, f)
        } else {
            tr.hit_rect(tr.clip_to_crop(l.rect, l.size), frame, f)
        }
    }

    /// The selected layer (if it's visible at the playhead) with its transform now.
    pub fn gizmo_layer(&self) -> Option<(GizmoLayer, Transform)> {
        let id = self.selection?;
        let (layer, _) = self.viewer_layers().into_iter().find(|(l, _)| l.id == id)?;
        let (_, c) = self.seq().find_clip(id)?;
        // A mesh-warped layer isn't placed by its transform: no handles for it.
        if c.mesh.is_some() {
            return None;
        }
        Some((layer, c.transform_at(self.playback.playhead)))
    }

    /// Apply a transform worked out by the gizmo to the selected layer (live).
    pub fn gizmo_apply(&mut self, id: ClipId, t: Transform) {
        self.set_transform_live(id, t);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: [f32; 2] = [200.0, 100.0];
    fn layer() -> GizmoLayer {
        GizmoLayer { id: ClipId(1), rect: Transform::full_rect([40.0, 20.0]), size: [40.0, 20.0] }
    }
    fn drag(h: Handle, crop: bool, start: Transform, from: [f32; 2], to: [f32; 2], shift: bool) -> Transform {
        let mut d = DragState::new(h, crop, start, FRAME, from);
        dragged(&mut d, &layer(), FRAME, 1.0, to, shift)
    }

    #[test]
    fn pinned_layers_drag_their_corners() {
        use reelforge_core::transform::Pin;
        let pin = Pin([[60.0, 30.0], [140.0, 35.0], [130.0, 80.0], [70.0, 70.0]]);
        let t = Transform { pin: Some(pin), ..Default::default() };
        let l = layer();
        assert_eq!(handle_at(&t, &l, FRAME, 1.0, [131.0, 79.0], false), Some(Handle::Corner(2)));
        assert_eq!(handle_at(&t, &l, FRAME, 1.0, [100.0, 50.0], true), None, "no anchor or rotation on a pinned layer");
        let moved = drag(Handle::Corner(2), false, t, [130.0, 80.0], [150.0, 90.0], false);
        assert_eq!(moved.pin.unwrap().0[2], [150.0, 90.0]);
        assert_eq!(moved.pin.unwrap().0[0], [60.0, 30.0], "the others stay");
        // Dragging a corner across the shape is refused (it would twist).
        let twisted = drag(Handle::Corner(2), false, t, [130.0, 80.0], [20.0, 10.0], false);
        assert_eq!(twisted.pin, Some(pin));
    }

    #[test]
    fn handles_are_found_where_they_are_drawn() {
        let t = Transform::default();
        let l = layer();
        let at = |p: [f32; 2], ctrl| handle_at(&t, &l, FRAME, 1.0, p, ctrl);
        // The 40×20 layer spans 80..120 × 40..60 in the 200×100 frame.
        assert_eq!(at([80.0, 40.0], false), Some(Handle::Corner(0)));
        assert_eq!(at([121.0, 61.0], false), Some(Handle::Corner(2)));
        assert_eq!(at([100.0, 50.0], true), Some(Handle::Anchor));
        assert_eq!(at([100.0, 50.0], false), None, "without Ctrl the middle moves the layer");
        assert_eq!(at([100.0, 40.0 - STEM], false), Some(Handle::Rotate));
        assert_eq!(at([70.0, 30.0], false), Some(Handle::Rotate), "just outside a corner");
        assert_eq!(at([110.0, 45.0], false), None, "inside: that's a move");
        // Zoomed out 2×: the grab distance stays the same on screen.
        assert_eq!(handle_at(&t, &l, FRAME, 0.5, [100.0, 40.0 - 2.0 * STEM], false), Some(Handle::Rotate));
    }

    #[test]
    fn corner_scales_uniformly_about_the_anchor() {
        let t = drag(Handle::Corner(2), false, Transform::default(), [120.0, 60.0], [140.0, 70.0], false);
        assert!((t.scale[0] - 2.0).abs() < 1e-4 && (t.scale[1] - 2.0).abs() < 1e-4, "{:?}", t.scale);
        assert_eq!(t.position, [0.0, 0.0], "the anchor (the middle) stays put");
        let free = drag(Handle::Corner(2), false, Transform::default(), [120.0, 60.0], [140.0, 60.0], true);
        assert!((free.scale[0] - 2.0).abs() < 1e-4 && (free.scale[1] - 1.0).abs() < 1e-4, "{:?}", free.scale);
        // Rotated 90°: dragging the corner straight down the screen still scales it.
        let r = Transform { rotation: 90.0, ..Default::default() };
        let c = r.rect_corners(layer().rect, FRAME)[2];
        let t = drag(Handle::Corner(2), false, r, c, [100.0 + (c[0] - 100.0) * 3.0, 50.0 + (c[1] - 50.0) * 3.0], false);
        assert!((t.scale[0] - 3.0).abs() < 1e-3, "{:?}", t.scale);
    }

    #[test]
    fn edges_stretch_or_crop() {
        let t = drag(Handle::Edge(1), false, Transform::default(), [120.0, 50.0], [130.0, 50.0], false);
        assert!((t.scale[0] - 1.5).abs() < 1e-4 && t.scale[1] == 1.0);
        let c = drag(Handle::Edge(3), true, Transform::default(), [80.0, 50.0], [90.0, 50.0], false);
        assert!((c.crop[0] - 0.25).abs() < 1e-4, "{:?}", c.crop);
        assert_eq!(c.scale, [1.0, 1.0]);
    }

    #[test]
    fn rotation_follows_the_pointer_and_snaps() {
        let t = drag(Handle::Rotate, false, Transform::default(), [100.0, 20.0], [130.0, 50.0], false);
        assert!((t.rotation - 90.0).abs() < 1e-3, "{}", t.rotation);
        let s = drag(Handle::Rotate, false, Transform::default(), [100.0, 20.0], [131.0, 41.0], true);
        assert_eq!(s.rotation % 15.0, 0.0);
    }

    #[test]
    fn anchor_moves_without_moving_the_picture_and_snaps() {
        let start = Transform { rotation: 30.0, ..Default::default() };
        let corner = start.rect_corners(layer().rect, FRAME)[0];
        let t = drag(Handle::Anchor, false, start, [100.0, 50.0], [corner[0] + 3.0, corner[1] + 2.0], false);
        assert_eq!(t.anchor, [-20.0, -10.0], "snapped to the corner");
        let before = start.rect_corners(layer().rect, FRAME);
        let after = t.rect_corners(layer().rect, FRAME);
        for i in 0..4 {
            assert!((before[i][0] - after[i][0]).abs() < 1e-3 && (before[i][1] - after[i][1]).abs() < 1e-3);
        }
    }

    #[test]
    fn moving_snaps_to_the_frame_middle_unless_free() {
        let l = layer();
        let start = Transform { position: [3.0, 0.0], ..Default::default() };
        let m = MoveState { id: l.id, start, from: [0.0, 0.0] };
        let layers = vec![(l, start)];
        let (t, g) = moved(&m, &layers, FRAME, 1.0, [-1.0, 0.0], false);
        assert_eq!(t.position[0], 0.0, "lined up with the centre line");
        assert!(g.guides_x.contains(&100.0));
        let (t, _) = moved(&m, &layers, FRAME, 1.0, [-1.0, 0.0], true);
        assert_eq!(t.position[0], 2.0);
    }
}
