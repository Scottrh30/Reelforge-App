//! The History panel (every edit, click one to go back to it) and the Undo / Redo
//! buttons in the menu bar.

use crate::app::EditorState;
use egui::{Color32, Pos2, Sense, Stroke, Vec2};
use std::time::{Duration, SystemTime};

/// Undo and Redo buttons with curved-arrow icons; the tooltip names the step.
pub fn undo_redo_buttons(ui: &mut egui::Ui, st: &mut EditorState) {
    let tip = |verb: &str, name: Option<&str>, keys: &str| match name {
        Some(n) => format!("{verb}: {n}   ({keys})"),
        None => format!("Nothing to {}", verb.to_lowercase()),
    };
    let undo_tip = tip("Undo", st.undo.undo_name(), "Ctrl+Z");
    let redo_tip = tip("Redo", st.undo.redo_name(), "Ctrl+Y");
    let u = icon_button(ui, "Undo", false, st.undo.can_undo());
    if u.on_hover_text(&undo_tip).on_disabled_hover_text(&undo_tip).clicked() {
        st.undo();
    }
    let r = icon_button(ui, "Redo", true, st.undo.can_redo());
    if r.on_hover_text(&redo_tip).on_disabled_hover_text(&redo_tip).clicked() {
        st.redo();
    }
}

/// A button with a painted curved arrow (the default font has no arrow glyphs).
/// `redo` mirrors the arrow.
fn icon_button(ui: &mut egui::Ui, label: &str, redo: bool, enabled: bool) -> egui::Response {
    let font = egui::TextStyle::Button.resolve(ui.style());
    let galley = ui.painter().layout_no_wrap(label.to_owned(), font, Color32::PLACEHOLDER);
    let pad = ui.spacing().button_padding;
    let icon = 14.0;
    let size = Vec2::new(pad.x * 2.0 + icon + 4.0 + galley.size().x, ui.spacing().interact_size.y.max(galley.size().y + pad.y * 2.0));
    let (rect, resp) = ui.allocate_exact_size(size, if enabled { Sense::click() } else { Sense::hover() });
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    let visuals = if enabled { ui.style().interact(&resp) } else { &ui.style().visuals.widgets.noninteractive };
    let col = if enabled { visuals.text_color() } else { ui.visuals().weak_text_color() };
    if enabled && (resp.hovered() || resp.has_focus() || resp.is_pointer_button_down_on()) {
        ui.painter().rect(rect, visuals.corner_radius, visuals.weak_bg_fill, visuals.bg_stroke, egui::StrokeKind::Inside);
    }
    // The arrow: the top of a circle, ending on the left (undo) or right (redo) with a head pointing down.
    let c = Pos2::new(rect.left() + pad.x + icon / 2.0, rect.center().y + 2.0);
    let r = 5.0;
    let flip = if redo { -1.0 } else { 1.0 };
    let pts: Vec<Pos2> = (0..=16)
        .map(|i| {
            let a = (-50.0f32 + 230.0 * i as f32 / 16.0).to_radians();
            Pos2::new(c.x + flip * r * a.cos(), c.y - r * a.sin())
        })
        .collect();
    let tip = *pts.last().unwrap();
    ui.painter().add(egui::Shape::line(pts, Stroke::new(1.6, col)));
    let head = vec![tip + Vec2::new(0.0, 4.0), tip + Vec2::new(-3.5, -1.0), tip + Vec2::new(3.5, -1.0)];
    ui.painter().add(egui::Shape::convex_polygon(head, col, Stroke::NONE));
    let text_pos = Pos2::new(rect.left() + pad.x + icon + 4.0, rect.center().y - galley.size().y / 2.0);
    ui.painter().galley(text_pos, galley, col);
    resp
}

/// "just now", "40 s ago", "5 min ago", "2 h ago".
fn ago(at: SystemTime, now: SystemTime) -> String {
    let s = now.duration_since(at).unwrap_or(Duration::ZERO).as_secs();
    match s {
        0..=4 => "just now".into(),
        5..=59 => format!("{s} s ago"),
        60..=3599 => format!("{} min ago", s / 60),
        _ => format!("{} h ago", s / 3600),
    }
}

/// The History tab.
pub fn show(ui: &mut egui::Ui, st: &mut EditorState) {
    let entries = st.undo.entries();
    let pos = st.undo.position();
    ui.horizontal(|ui| {
        undo_redo_buttons(ui, st);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let undone = entries.len() - pos;
            let text = if undone > 0 { format!("{pos} of {} steps", entries.len()) } else { format!("{} steps", entries.len()) };
            ui.weak(text);
        });
    });
    ui.separator();

    // Scroll to the current step when it changes (undo, redo, or a new edit).
    let mem = ui.id().with("history_pos");
    let last: Option<(usize, usize)> = ui.data(|d| d.get_temp(mem));
    let changed = last != Some((pos, entries.len()));
    ui.data_mut(|d| d.insert_temp(mem, (pos, entries.len())));

    let now = SystemTime::now();
    let mut go: Option<usize> = None;
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = 1.0;
        // Row 0 is the state before any step; row i is the state after step i.
        for row in 0..=entries.len() {
            let (name, when) = if row == 0 { ("Start".to_string(), None) } else { (entries[row - 1].name.clone(), Some(entries[row - 1].at)) };
            let current = row == pos;
            let undone = row > pos;
            let r = history_row(ui, row, &name, when.map(|t| ago(t, now)), current, undone);
            let tip = if current {
                "The project is here now".to_string()
            } else if undone {
                "Undone. Click to redo up to here".to_string()
            } else if row == 0 {
                "Click to undo everything back to the start".to_string()
            } else {
                "Click to go back to just after this step".to_string()
            };
            let r = r.on_hover_text(tip);
            if r.clicked() && !current {
                go = Some(row);
            }
            if current && changed {
                r.scroll_to_me(Some(egui::Align::Center));
            }
        }
        if entries.is_empty() {
            ui.add_space(6.0);
            ui.weak("Edits appear here as you make them. Click any step to go back to it; steps after it stay listed (greyed) until you make a new edit, so you can jump forward again.");
        }
    });
    if let Some(row) = go {
        st.history_go_to(row);
    }
    // Keep "n min ago" fresh.
    ui.ctx().request_repaint_after(Duration::from_secs(5));
}

fn history_row(ui: &mut egui::Ui, index: usize, name: &str, when: Option<String>, current: bool, undone: bool) -> egui::Response {
    let h = ui.spacing().interact_size.y + 2.0;
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), h), Sense::click());
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    let v = ui.visuals();
    let p = ui.painter();
    if current {
        p.rect_filled(rect, 3.0, v.selection.bg_fill);
    } else if resp.hovered() {
        p.rect_filled(rect, 3.0, v.widgets.hovered.weak_bg_fill);
    }
    let strong = if current { v.selection.stroke.color } else { v.text_color() };
    let col = if undone { v.weak_text_color().gamma_multiply(0.8) } else { strong };
    let font = egui::FontId::proportional(13.0);
    let small = egui::FontId::proportional(11.0);
    // Step number, name, time.
    let num = if index == 0 { String::new() } else { index.to_string() };
    p.text(Pos2::new(rect.left() + 26.0, rect.center().y), egui::Align2::RIGHT_CENTER, num, small.clone(), if undone { col } else { v.weak_text_color() });
    if index == 0 {
        // A small dot marks the starting state.
        p.circle_filled(Pos2::new(rect.left() + 20.0, rect.center().y), 3.0, col);
    }
    let name_rect = p.text(Pos2::new(rect.left() + 34.0, rect.center().y), egui::Align2::LEFT_CENTER, name, font, col);
    if undone {
        // Struck through lightly, so undone steps read as "not in effect".
        let y = name_rect.center().y;
        p.line_segment([Pos2::new(name_rect.left(), y), Pos2::new(name_rect.right(), y)], Stroke::new(1.0, col.gamma_multiply(0.6)));
    }
    if let Some(w) = when {
        p.text(Pos2::new(rect.right() - 6.0, rect.center().y), egui::Align2::RIGHT_CENTER, w, small, if current { strong } else { v.weak_text_color() });
    }
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_times() {
        let now = SystemTime::now();
        assert_eq!(ago(now, now), "just now");
        assert_eq!(ago(now - Duration::from_secs(42), now), "42 s ago");
        assert_eq!(ago(now - Duration::from_secs(150), now), "2 min ago");
        assert_eq!(ago(now - Duration::from_secs(7300), now), "2 h ago");
        assert_eq!(ago(now + Duration::from_secs(5), now), "just now", "clock went backwards");
    }
}
