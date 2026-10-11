//! Mask editing: the outlines and handles in the Viewer, freehand drawing,
//! and the Masks section of the Controls panel.

use crate::app::EditorState;
use egui::{Color32, Pos2, RichText, Stroke, Vec2};
use reelforge_core::mask::{Mask, MaskMode, MaskShape};
use reelforge_core::{Affine2, Clip, Time};

const MASK_COLOR: Color32 = Color32::from_rgb(255, 110, 200);

/// Mapping between a clip's layer pixels and the screen.
struct Geo {
    m: Affine2,
    inv: Affine2,
    half: [f32; 2],
    origin: Pos2,
    scale: f32,
}

impl Geo {
    fn to_screen(&self, p: [f32; 2]) -> Pos2 {
        let f = self.m.apply([p[0] - self.half[0], p[1] - self.half[1]]);
        self.origin + Vec2::new(f[0], f[1]) * self.scale
    }
    fn to_layer(&self, s: Pos2) -> [f32; 2] {
        let f = (s - self.origin) / self.scale;
        let l = self.inv.apply([f.x, f.y]);
        [l[0] + self.half[0], l[1] + self.half[1]]
    }
}

/// A mask drag in progress: which mask, which point (`None` = the whole
/// shape), where it started (layer pixels), and the points before.
#[derive(Clone)]
struct MaskDrag {
    mask: usize,
    point: Option<usize>,
    start: [f32; 2],
    orig: Vec<[f32; 2]>,
}

fn inside(poly: &[[f32; 2]], p: [f32; 2]) -> bool {
    let mut c = false;
    for i in 0..poly.len() {
        let (a, b) = (poly[i], poly[(i + 1) % poly.len()]);
        if (a[1] > p[1]) != (b[1] > p[1]) && p[0] < a[0] + (p[1] - a[1]) / (b[1] - a[1]) * (b[0] - a[0]) {
            c = !c;
        }
    }
    c
}

/// Would a press at `press` grab a mask handle or the active mask? (Checked
/// before the viewer's own click-to-select, so grabbing a handle outside the
/// layer doesn't select something else.)
pub fn wants_press(st: &EditorState, frame_px: [f32; 2], scale: f32, origin: Pos2, press: Pos2) -> bool {
    if st.mask_draw.is_some() {
        return true;
    }
    hit(st, frame_px, scale, origin, press).is_some()
}

fn geo(st: &EditorState, clip: &Clip, frame_px: [f32; 2], scale: f32, origin: Pos2) -> Option<Geo> {
    let m = clip.transform_at(st.playback.playhead).matrix(frame_px);
    let size = st.layer_size(clip);
    Some(Geo { inv: m.inverse()?, m, half: [size[0] / 2.0, size[1] / 2.0], origin, scale })
}

/// The mask and point under `press`, for the selected clip.
fn hit(st: &EditorState, frame_px: [f32; 2], scale: f32, origin: Pos2, press: Pos2) -> Option<(usize, Option<usize>)> {
    let id = st.selection?;
    let (_, clip) = st.seq().find_clip(id)?;
    if clip.masks.is_empty() {
        return None;
    }
    let g = geo(st, clip, frame_px, scale, origin)?;
    let src = src_time(st, clip);
    for (i, m) in clip.masks.iter().enumerate().filter(|(_, m)| m.enabled) {
        if let Some(j) = m.points_at(src).iter().position(|p| g.to_screen(*p).distance(press) <= 7.0) {
            return Some((i, Some(j)));
        }
    }
    let active = st.active_mask.filter(|(c, _)| *c == id).map(|(_, i)| i)?;
    let m = clip.masks.get(active)?;
    inside(&m.outline(src), g.to_layer(press)).then_some((active, None))
}

fn src_time(st: &EditorState, clip: &Clip) -> Time {
    clip.source_time(st.playback.playhead.max(clip.timeline_start).min(clip.timeline_end()))
}

/// Draw the selected clip's masks and handle dragging them (and freehand
/// drawing). Returns true when it used the pointer this frame.
pub fn overlay(ui: &mut egui::Ui, st: &mut EditorState, resp: &egui::Response, area: egui::Rect, frame_px: [f32; 2], scale: f32, origin: Pos2) -> bool {
    let Some(id) = st.selection else {
        st.mask_draw = None;
        return false;
    };
    if st.mask_draw.as_ref().is_some_and(|(c, _)| *c != id) {
        st.mask_draw = None;
    }
    let Some((_, clip)) = st.seq().find_clip(id) else { return false };
    let clip = clip.clone();
    let Some(g) = geo(st, &clip, frame_px, scale, origin) else { return false };
    let src = src_time(st, &clip);
    let painter = ui.painter_at(area);
    let active = st.active_mask.filter(|(c, _)| *c == id).map(|(_, i)| i);

    for (i, m) in clip.masks.iter().enumerate() {
        let outline: Vec<Pos2> = m.outline(src).iter().map(|p| g.to_screen(*p)).collect();
        if outline.len() < 2 {
            continue;
        }
        let on = Some(i) == active;
        let col = if m.enabled { MASK_COLOR } else { MASK_COLOR.gamma_multiply(0.35) };
        painter.add(egui::Shape::closed_line(outline, Stroke::new(if on { 1.8 } else { 1.0 }, col)));
        if m.enabled {
            for p in m.points_at(src) {
                let s = g.to_screen(p);
                let r = egui::Rect::from_center_size(s, Vec2::splat(if on { 7.0 } else { 5.0 }));
                painter.rect_filled(r, 1.0, if on { col } else { Color32::from_black_alpha(160) });
                painter.rect_stroke(r, 1.0, Stroke::new(1.0, col), egui::StrokeKind::Inside);
            }
        }
    }

    // Freehand drawing: each click adds a point; the first point (or Enter) closes it.
    if let Some((_, pts)) = &st.mask_draw {
        let screen: Vec<Pos2> = pts.iter().map(|p| g.to_screen(*p)).collect();
        if screen.len() > 1 {
            painter.add(egui::Shape::line(screen.clone(), Stroke::new(1.5, MASK_COLOR)));
        }
        if let (Some(last), Some(hover)) = (screen.last(), resp.hover_pos()) {
            painter.line_segment([*last, hover], Stroke::new(1.0, MASK_COLOR.gamma_multiply(0.6)));
        }
        for (k, s) in screen.iter().enumerate() {
            let big = k == 0 && screen.len() >= 3;
            painter.circle_filled(*s, if big { 5.0 } else { 3.0 }, MASK_COLOR);
        }
        ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
        if resp.clicked() || resp.double_clicked() {
            if let Some(p) = resp.interact_pointer_pos() {
                let close = screen.len() >= 3 && (resp.double_clicked() || screen[0].distance(p) <= 8.0);
                if close {
                    st.finish_mask_drawing();
                } else if let Some((_, pts)) = &mut st.mask_draw {
                    pts.push(g.to_layer(p));
                    st.status = format!("{} points: click the first point or press Enter to close the mask (Esc cancels)", pts.len());
                }
            }
        }
        return true;
    }

    // Dragging a point, or the whole active mask.
    let drag_id = egui::Id::new("mask_drag");
    if resp.drag_started() {
        let grab = ui.input(|i| i.pointer.press_origin()).and_then(|p| hit(st, frame_px, scale, origin, p).map(|h| (h, p)));
        let state = grab.map(|((mask, point), p)| {
            st.active_mask = Some((id, mask));
            MaskDrag { mask, point, start: g.to_layer(p), orig: clip.masks[mask].points_at(src) }
        });
        ui.data_mut(|d| d.insert_temp(drag_id, state));
    }
    let state: Option<MaskDrag> = ui.data(|d| d.get_temp::<Option<MaskDrag>>(drag_id)).flatten();
    let mut used = false;
    if let (Some(d), true) = (&state, resp.dragged() && !st.playback.playing) {
        if let (Some(pos), Some(mut v)) = (resp.interact_pointer_pos(), st.visual(id)) {
            let now = g.to_layer(pos);
            let (dx, dy) = (now[0] - d.start[0], now[1] - d.start[1]);
            let pts: Vec<[f32; 2]> = d
                .orig
                .iter()
                .enumerate()
                .map(|(j, p)| if d.point.is_none_or(|k| k == j) { [p[0] + dx, p[1] + dy] } else { *p })
                .collect();
            if let Some(m) = v.masks.get_mut(d.mask) {
                m.set_points_at(src, pts);
                st.set_visual_live(id, v);
            }
        }
        ui.ctx().set_cursor_icon(if d.point.is_some() { egui::CursorIcon::Crosshair } else { egui::CursorIcon::Move });
        used = true;
    }
    if resp.drag_stopped() {
        ui.data_mut(|d| d.insert_temp::<Option<MaskDrag>>(drag_id, None));
    }
    used
}

/// The Masks section of the Controls panel.
pub fn controls(ui: &mut egui::Ui, st: &mut EditorState, clip: &Clip) {
    let id = clip.id;
    let size = st.layer_size(clip);
    ui.horizontal(|ui| {
        ui.heading("Masks");
        if ui.small_button("+ Rectangle").clicked() {
            st.add_mask(id, Mask::centred(MaskShape::Rectangle, size));
        }
        if ui.small_button("+ Ellipse").clicked() {
            st.add_mask(id, Mask::centred(MaskShape::Ellipse, size));
        }
        let drawing = st.mask_draw.as_ref().is_some_and(|(c, _)| *c == id);
        if ui.add(egui::Button::new("+ Freehand").small().selected(drawing)).on_hover_text("Click points in the Viewer; click the first point or press Enter to close").clicked() {
            if drawing {
                st.mask_draw = None;
            } else {
                st.mask_draw = Some((id, Vec::new()));
                st.status = "Click in the Viewer to place the mask's points; click the first point or press Enter to close it (Esc cancels)".into();
            }
        }
    });
    if clip.masks.is_empty() {
        ui.weak("Cut the layer down to a shape. Masks are applied before effects, and move with the layer.");
        return;
    }
    let Some(mut v) = st.visual(id) else { return };
    let before = v.clone();
    let src = src_time(st, clip);
    let active = st.active_mask.filter(|(c, _)| *c == id).map(|(_, i)| i);
    let mut remove = None;
    let mut make_active = None;
    for (i, m) in v.masks.iter_mut().enumerate() {
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.checkbox(&mut m.enabled, "").on_hover_text("On / off");
                let name = RichText::new(&m.name).color(if Some(i) == active { MASK_COLOR } else { ui.visuals().text_color() });
                if ui.selectable_label(Some(i) == active, name).on_hover_text("Edit this mask in the Viewer (drag inside it to move it)").clicked() {
                    make_active = Some(i);
                }
                egui::ComboBox::from_id_salt(("mask_mode", id, i)).width(80.0).selected_text(m.mode.label()).show_ui(ui, |ui| {
                    for mode in MaskMode::ALL {
                        ui.selectable_value(&mut m.mode, mode, mode.label());
                    }
                });
                ui.checkbox(&mut m.inverted, "Invert");
            });
            ui.horizontal(|ui| {
                ui.weak("Feather");
                ui.add(egui::DragValue::new(&mut m.feather).range(0.0..=1000.0).speed(0.5).suffix(" px").fixed_decimals(0));
                ui.weak("Expand");
                ui.add(egui::DragValue::new(&mut m.expansion).range(-1000.0..=1000.0).speed(0.5).suffix(" px").fixed_decimals(0));
                ui.weak("Opacity");
                let mut o = m.opacity * 100.0;
                if ui.add(egui::DragValue::new(&mut o).range(0.0..=100.0).speed(0.5).suffix(" %").fixed_decimals(0)).changed() {
                    m.opacity = o / 100.0;
                }
            });
            ui.horizontal(|ui| {
                let mut animated = m.is_animated();
                if ui.checkbox(&mut animated, "Animate shape").on_hover_text("Moving the shape's points then sets a keyframe at the playhead").changed() {
                    m.set_animated(animated, src);
                }
                if m.is_animated() {
                    let here = m.keys.iter().any(|k| k.at == src);
                    ui.weak(format!("{} key{}", m.keys.len(), if m.keys.len() == 1 { "" } else { "s" }));
                    if !here && ui.small_button("Key here").clicked() {
                        let pts = m.points_at(src);
                        m.set_points_at(src, pts);
                    }
                    if here && m.keys.len() > 1 && ui.small_button("Delete key").clicked() {
                        m.keys.retain(|k| k.at != src);
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("Remove").clicked() {
                        remove = Some(i);
                    }
                });
            });
        });
    }
    if let Some(i) = remove {
        v.masks.remove(i);
        st.active_mask = None;
    }
    if v != before {
        st.set_visual_live(id, v);
        if remove.is_some() {
            st.commit_live_edit();
        }
    }
    if let Some(i) = make_active {
        st.active_mask = if active == Some(i) { None } else { Some((id, i)) };
    }
}

