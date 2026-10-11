//! The transform gizmo in the Viewer: the selected layer's bounding box with
//! handles to scale (corners and edges), rotate (the stem above the top edge,
//! or just outside a corner), crop (Ctrl + an edge handle) and move the anchor
//! point (Ctrl + the cross). Dragging inside the box moves the layer (in panels.rs).
//!
//! A corner-pinned layer shows its four pin corners instead: drag one to move
//! that corner (Shift: straight along the frame's axes).
//!
//! Every drag is computed from the transform at the start of the drag and the
//! pointer now, so the result never drifts, and is one undo step on release.

use crate::app::EditorState;
use egui::{Color32, CursorIcon, Pos2, Stroke, Vec2};
use reelforge_core::{ClipId, Transform};

/// Screen-pixel sizes.
const HANDLE: f32 = 7.0;
const GRAB: f32 = 8.0;
const STEM: f32 = 26.0;

/// The layer the gizmo is on.
#[derive(Clone, Copy, Debug)]
pub struct GizmoLayer {
    pub id: ClipId,
    /// Layer-local rectangle that counts as the picture (the text, for a title), before cropping.
    pub rect: [f32; 4],
    /// The layer's size at 100% (for the crop fractions).
    pub size: [f32; 2],
}

/// What a press grabbed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Handle {
    /// A corner, by index (top-left, top-right, bottom-right, bottom-left).
    Corner(usize),
    /// An edge, by index (top, right, bottom, left).
    Edge(usize),
    Rotate,
    Anchor,
}

#[derive(Clone, Copy, Debug)]
struct DragState {
    handle: Handle,
    crop: bool,
    start: Transform,
    /// Pointer at the start, in frame pixels.
    from: [f32; 2],
    /// For rotation: the pointer angle last frame and the total turned so far.
    last_angle: f32,
    turned: f32,
}

/// The view's mapping between frame pixels and the screen.
#[derive(Clone, Copy)]
pub struct View {
    pub frame: [f32; 2],
    pub scale: f32,
    pub origin: Pos2,
}

impl View {
    fn to_screen(self, [x, y]: [f32; 2]) -> Pos2 {
        self.origin + Vec2::new(x, y) * self.scale
    }
    fn to_frame(self, p: Pos2) -> [f32; 2] {
        let v = (p - self.origin) / self.scale;
        [v.x, v.y]
    }
}

/// Screen positions of everything grabbable for transform `t`.
struct Geometry {
    corners: [Pos2; 4],
    edges: [Pos2; 4],
    stem_base: Pos2,
    stem_tip: Pos2,
    anchor: Pos2,
}

fn geometry(t: &Transform, layer: &GizmoLayer, view: &View) -> Geometry {
    let r = t.clip_to_crop(layer.rect, layer.size);
    let corners = match t.valid_pin() {
        // A pinned layer's handles are its pin corners.
        Some(pin) => pin.0.map(|p| view.to_screen(p)),
        None => t.rect_corners(r, view.frame).map(|p| view.to_screen(p)),
    };
    let mid = |a: Pos2, b: Pos2| a + (b - a) / 2.0;
    let edges = [mid(corners[0], corners[1]), mid(corners[1], corners[2]), mid(corners[2], corners[3]), mid(corners[3], corners[0])];
    let centre = mid(corners[0], corners[2]);
    // The stem points away from the middle of the box, through the top edge.
    let out = (edges[0] - centre).normalized();
    let out = if out.is_finite() && out != Vec2::ZERO { out } else { -Vec2::Y };
    let anchor = view.to_screen(t.matrix(view.frame).apply(t.anchor));
    Geometry { corners, edges, stem_base: edges[0], stem_tip: edges[0] + out * STEM, anchor }
}

/// Which handle (if any) is at screen point `p`. The anchor can only be
/// grabbed with Ctrl held (`ctrl`), so dragging the middle of a layer moves it.
pub fn handle_at(t: &Transform, layer: &GizmoLayer, view: &View, p: Pos2, ctrl: bool) -> Option<Handle> {
    let g = geometry(t, layer, view);
    let near = |q: Pos2| q.distance(p) <= GRAB;
    if t.valid_pin().is_some() {
        return g.corners.iter().position(|c| near(*c)).map(Handle::Corner);
    }
    if ctrl && near(g.anchor) {
        return Some(Handle::Anchor);
    }
    if near(g.stem_tip) {
        return Some(Handle::Rotate);
    }
    if let Some(i) = g.corners.iter().position(|c| near(*c)) {
        return Some(Handle::Corner(i));
    }
    if small(&g) {
        // Too small for edge handles to be told apart from the corners.
    } else if let Some(i) = g.edges.iter().position(|c| near(*c)) {
        return Some(Handle::Edge(i));
    }
    // Just outside a corner: rotate.
    let inside = t.hit_rect(t.clip_to_crop(layer.rect, layer.size), view.frame, view.to_frame(p));
    if !inside && g.corners.iter().any(|c| c.distance(p) <= GRAB + 16.0) {
        return Some(Handle::Rotate);
    }
    None
}

fn small(g: &Geometry) -> bool {
    g.corners[0].distance(g.corners[1]) < 4.0 * GRAB || g.corners[1].distance(g.corners[2]) < 4.0 * GRAB
}

fn cursor_for(h: Handle, crop: bool, t: &Transform) -> CursorIcon {
    // Pick the resize cursor closest to the handle's direction on screen.
    let dir = match h {
        Handle::Corner(i) => [-45.0, 45.0, 135.0, 225.0][i],
        Handle::Edge(i) => [-90.0, 0.0, 90.0, 180.0][i],
        Handle::Rotate => return CursorIcon::Grab,
        Handle::Anchor => return CursorIcon::Crosshair,
    };
    let a = (dir + t.rotation).rem_euclid(180.0);
    let _ = crop;
    match ((a + 22.5) / 45.0) as i32 % 4 {
        0 => CursorIcon::ResizeHorizontal,
        1 => CursorIcon::ResizeNwSe,
        2 => CursorIcon::ResizeVertical,
        _ => CursorIcon::ResizeNeSw,
    }
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

/// The transform a drag gives, from where it started and the pointer now.
fn dragged(d: &mut DragState, layer: &GizmoLayer, view: &View, p: [f32; 2], shift: bool) -> Transform {
    let s = d.start;
    let mut t = s;
    if let (Some(pin), Handle::Corner(i)) = (s.valid_pin(), d.handle) {
        let mut delta = [p[0] - d.from[0], p[1] - d.from[1]];
        if shift {
            // Only along whichever axis it has moved further.
            if delta[0].abs() > delta[1].abs() { delta[1] = 0.0 } else { delta[0] = 0.0 }
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
            if let (Handle::Edge(e), Some(inv)) = (d.handle, s.matrix(view.frame).inverse()) {
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
            let v0 = local_offset(&s, view.frame, d.from);
            let v = local_offset(&s, view.frame, p);
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
            let v0 = local_offset(&s, view.frame, d.from);
            let v = local_offset(&s, view.frame, p);
            if v0[i].abs() > 0.5 {
                let f = v[i] / v0[i];
                t.scale[i] = s.scale[i] * f;
                if shift {
                    t.scale[1 - i] = s.scale[1 - i] * f;
                }
            }
        }
        Handle::Rotate => {
            let a = angle(&s, view.frame, p);
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
            if let Some(inv) = s.matrix(view.frame).inverse() {
                let mut a = inv.apply(p);
                // Snap to the middle, corners and edge midpoints of the picture (8 screen px).
                let r = s.clip_to_crop(layer.rect, layer.size);
                let (cx, cy) = ((r[0] + r[2]) / 2.0, (r[1] + r[3]) / 2.0);
                let snaps = [[cx, cy], [r[0], r[1]], [r[2], r[1]], [r[2], r[3]], [r[0], r[3]], [cx, r[1]], [r[2], cy], [cx, r[3]], [r[0], cy]];
                let m = s.matrix(view.frame);
                let here = view.to_screen(p);
                if !shift {
                    if let Some(q) = snaps.iter().find(|q| view.to_screen(m.apply(**q)).distance(here) <= 8.0) {
                        a = *q;
                    }
                }
                t = s.with_anchor_kept_in_place(a, view.frame);
            }
        }
    }
    t
}

/// A short readout of what a drag is changing.
fn readout(d: &DragState, t: &Transform) -> String {
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

/// Run the gizmo for the selected layer: hover cursors, handle drags, and drawing.
/// `grabbed` is the handle under the press that started this drag (if any).
/// Returns true while a handle is being dragged (so the layer isn't also moved).
pub fn show(ui: &mut egui::Ui, st: &mut EditorState, resp: &egui::Response, area: egui::Rect, layer: GizmoLayer, view: View, grabbed: Option<Handle>) -> bool {
    let id = egui::Id::new("gizmo_drag");
    let t_now = st.playback.playhead;
    let Some(clip) = st.seq().find_clip(layer.id).map(|(_, c)| c.clone()) else { return false };
    let shown = clip.transform_at(t_now);
    let (shift, ctrl) = ui.input(|i| (i.modifiers.shift, i.modifiers.command));
    let can_edit = !st.playback.playing;

    let mut active = false;
    if resp.drag_started() {
        let state = grabbed.filter(|_| can_edit).map(|h| {
            let from = view.to_frame(ui.input(|i| i.pointer.press_origin()).unwrap_or_default());
            let crop = ctrl && matches!(h, Handle::Edge(_));
            DragState { handle: h, crop, start: shown, from, last_angle: angle(&shown, view.frame, from), turned: 0.0 }
        });
        ui.data_mut(|d| match state {
            Some(s) => {
                d.insert_temp(id, s);
            }
            None => d.remove::<DragState>(id),
        });
    }
    let mut state: Option<DragState> = if resp.dragged() || resp.drag_stopped() { ui.data(|d| d.get_temp(id)) } else { None };
    if !resp.dragged() && !resp.drag_stopped() {
        ui.data_mut(|d| d.remove::<DragState>(id));
    }
    let mut label = None;
    if let (Some(d), Some(p)) = (state.as_mut(), resp.interact_pointer_pos()) {
        active = true;
        let moved = dragged(d, &layer, &view, view.to_frame(p), shift);
        label = Some((p, readout(d, &moved)));
        ui.ctx().set_cursor_icon(if d.handle == Handle::Rotate { CursorIcon::Grabbing } else { cursor_for(d.handle, d.crop, &shown) });
        if moved != shown {
            if let Some(mut v) = st.visual(layer.id) {
                let src = clip.source_time(t_now.max(clip.timeline_start).min(clip.timeline_end()));
                crate::panels::apply_transform_edit(&mut v, &shown, &moved, src, clip.contains(t_now));
                st.set_visual_live(layer.id, v);
            }
        }
        let d = *d;
        ui.data_mut(|m| m.insert_temp(id, d));
    } else if let Some(h) = resp.hover_pos().filter(|_| can_edit).and_then(|p| handle_at(&shown, &layer, &view, p, ctrl)) {
        let crop = ctrl && matches!(h, Handle::Edge(_));
        ui.ctx().set_cursor_icon(cursor_for(h, crop, &shown));
        let tip = match h {
            Handle::Corner(_) if shown.valid_pin().is_some() => "Drag to move this corner of the corner pin (Shift: straight across or down)",
            Handle::Corner(_) => "Drag to scale (Shift: X and Y separately)",
            Handle::Edge(_) if crop => "Drag to crop this edge",
            Handle::Edge(_) => "Drag to stretch (Shift: both ways). Ctrl+drag crops this edge",
            Handle::Rotate => "Drag to rotate around the anchor (Shift: 15° steps)",
            Handle::Anchor => "Drag to move the anchor point, the pivot for scale and rotation (snaps to the middle, corners and edges; Shift: no snapping)",
        };
        resp.clone().on_hover_text_at_pointer(tip);
    }

    // Draw (with the transform as it is now, after any edit this frame).
    let now = st.seq().find_clip(layer.id).map(|(_, c)| c.transform_at(t_now)).unwrap_or(shown);
    let g = geometry(&now, &layer, &view);
    let gold = Color32::from_rgb(255, 196, 64);
    let p = ui.painter_at(area);
    if now.valid_pin().is_some() {
        // Corner pin: the four corners and the shape between them (with its
        // diagonals, which show the perspective), nothing else.
        let pink = Color32::from_rgb(255, 120, 200);
        p.add(egui::Shape::closed_line(g.corners.to_vec(), Stroke::new(1.5, pink)));
        p.line_segment([g.corners[0], g.corners[2]], Stroke::new(1.0, pink.gamma_multiply(0.35)));
        p.line_segment([g.corners[1], g.corners[3]], Stroke::new(1.0, pink.gamma_multiply(0.35)));
        for c in g.corners {
            p.circle_filled(c, 5.0, Color32::from_gray(20));
            p.circle_stroke(c, 5.0, Stroke::new(2.0, pink));
        }
        if let Some((at, text)) = label {
            let font = egui::FontId::proportional(12.0);
            let galley = p.layout_no_wrap(text, font, Color32::WHITE);
            let r = egui::Rect::from_min_size(at + Vec2::new(16.0, 16.0), galley.size() + Vec2::splat(8.0));
            p.rect_filled(r, 3.0, Color32::from_black_alpha(200));
            p.galley(r.min + Vec2::splat(4.0), galley, Color32::WHITE);
        }
        return active;
    }
    p.add(egui::Shape::closed_line(g.corners.to_vec(), Stroke::new(1.5, gold)));
    if now.is_cropped() {
        // The uncropped outline, faintly, so you can see what's been cut.
        let full = now.rect_corners(layer.rect, view.frame).map(|q| view.to_screen(q));
        p.add(egui::Shape::dashed_line(&[full[0], full[1], full[2], full[3], full[0]], Stroke::new(1.0, gold.gamma_multiply(0.45)), 4.0, 4.0));
    }
    let crop_mode = ctrl || state.is_some_and(|d| d.crop);
    let handle = |c: Pos2, filled: bool| {
        let r = egui::Rect::from_center_size(c, Vec2::splat(HANDLE));
        if filled {
            p.rect_filled(r, 1.0, gold);
        } else {
            p.rect_filled(r, 1.0, Color32::from_gray(20));
            p.rect_stroke(r, 1.0, Stroke::new(1.5, gold), egui::StrokeKind::Inside);
        }
    };
    for c in g.corners {
        handle(c, true);
    }
    if !small(&g) {
        for e in g.edges {
            if crop_mode {
                // Crop handles: short bars along the edge.
                p.circle_filled(e, 3.5, Color32::from_rgb(120, 200, 255));
            } else {
                handle(e, false);
            }
        }
    }
    p.line_segment([g.stem_base, g.stem_tip], Stroke::new(1.2, gold));
    p.circle_filled(g.stem_tip, 4.5, Color32::from_gray(20));
    p.circle_stroke(g.stem_tip, 4.5, Stroke::new(1.5, gold));
    // The anchor: a cross in a circle.
    let a = g.anchor;
    if crop_mode {
        p.circle_filled(a, 7.0, Color32::from_rgb(120, 200, 255).gamma_multiply(0.5));
    }
    p.circle_stroke(a, 5.0, Stroke::new(1.5, gold));
    p.line_segment([a - Vec2::X * 9.0, a + Vec2::X * 9.0], Stroke::new(1.2, gold));
    p.line_segment([a - Vec2::Y * 9.0, a + Vec2::Y * 9.0], Stroke::new(1.2, gold));
    if let Some((at, text)) = label {
        let font = egui::FontId::proportional(12.0);
        let galley = p.layout_no_wrap(text, font, Color32::WHITE);
        let r = egui::Rect::from_min_size(at + Vec2::new(16.0, 16.0), galley.size() + Vec2::splat(8.0));
        p.rect_filled(r, 3.0, Color32::from_black_alpha(200));
        p.galley(r.min + Vec2::splat(4.0), galley, Color32::WHITE);
    }
    active
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view() -> View {
        View { frame: [200.0, 100.0], scale: 1.0, origin: Pos2::ZERO }
    }
    fn layer() -> GizmoLayer {
        GizmoLayer { id: ClipId(1), rect: Transform::full_rect([40.0, 20.0]), size: [40.0, 20.0] }
    }
    fn drag(h: Handle, crop: bool, start: Transform, from: [f32; 2], to: [f32; 2], shift: bool) -> Transform {
        let v = view();
        let mut d = DragState { handle: h, crop, start, from, last_angle: angle(&start, v.frame, from), turned: 0.0 };
        dragged(&mut d, &layer(), &v, to, shift)
    }

    #[test]
    fn pinned_layers_drag_their_corners() {
        use reelforge_core::transform::Pin;
        let pin = Pin([[60.0, 30.0], [140.0, 35.0], [130.0, 80.0], [70.0, 70.0]]);
        let t = Transform { pin: Some(pin), ..Default::default() };
        let (l, v) = (layer(), view());
        assert_eq!(handle_at(&t, &l, &v, Pos2::new(131.0, 79.0), false), Some(Handle::Corner(2)));
        assert_eq!(handle_at(&t, &l, &v, Pos2::new(100.0, 50.0), true), None, "no anchor or rotation on a pinned layer");
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
        let (l, v) = (layer(), view());
        // The 40×20 layer spans 80..120 × 40..60 in the 200×100 frame.
        assert_eq!(handle_at(&t, &l, &v, Pos2::new(80.0, 40.0), false), Some(Handle::Corner(0)));
        assert_eq!(handle_at(&t, &l, &v, Pos2::new(121.0, 61.0), false), Some(Handle::Corner(2)));
        assert_eq!(handle_at(&t, &l, &v, Pos2::new(100.0, 50.0), true), Some(Handle::Anchor));
        assert_eq!(handle_at(&t, &l, &v, Pos2::new(100.0, 50.0), false), None, "without Ctrl the middle moves the layer");
        assert_eq!(handle_at(&t, &l, &v, Pos2::new(100.0, 40.0 - STEM), false), Some(Handle::Rotate));
        assert_eq!(handle_at(&t, &l, &v, Pos2::new(70.0, 30.0), false), Some(Handle::Rotate), "just outside a corner");
        assert_eq!(handle_at(&t, &l, &v, Pos2::new(110.0, 45.0), false), None, "inside: that's a move");
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
        let c = r.rect_corners(layer().rect, view().frame)[2];
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
        let corner = start.rect_corners(layer().rect, view().frame)[0];
        let t = drag(Handle::Anchor, false, start, [100.0, 50.0], [corner[0] + 3.0, corner[1] + 2.0], false);
        assert_eq!(t.anchor, [-20.0, -10.0], "snapped to the corner");
        let before = start.rect_corners(layer().rect, view().frame);
        let after = t.rect_corners(layer().rect, view().frame);
        for i in 0..4 {
            assert!((before[i][0] - after[i][0]).abs() < 1e-3 && (before[i][1] - after[i][1]).abs() < 1e-3);
        }
    }
}
