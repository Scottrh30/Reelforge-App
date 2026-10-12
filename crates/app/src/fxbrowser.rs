//! The effects & transitions browser (E), plus the editors for effects that
//! need more than numbers: the tone-curve editor and the LUT file picker.

use crate::app::EditorState;
use egui::{Color32, Pos2, Sense, Stroke, Vec2};
use reelforge_core::lut::{eval_curve, Curves};
use reelforge_core::{Effect, EffectCategory, EffectKind, Transition};
use std::path::PathBuf;

#[derive(Clone, Default)]
struct BrowserState {
    search: String,
    transitions: bool,
}

/// The browser window: search, pick a tab, and add an effect to the selected
/// clip or put a transition on the cut nearest the playhead.
pub fn window(ctx: &egui::Context, st: &mut EditorState, open: &mut bool) {
    let id = egui::Id::new("fx_browser_state");
    let mut bs: BrowserState = ctx.data_mut(|d| d.get_temp(id).unwrap_or_default());
    egui::Window::new("Effects & transitions").open(open).default_size([360.0, 480.0]).resizable(true).show(ctx, |ui| {
        ui.horizontal(|ui| {
            ui.selectable_value(&mut bs.transitions, false, "Effects");
            ui.selectable_value(&mut bs.transitions, true, "Transitions");
        });
        ui.horizontal(|ui| {
            ui.label("Search");
            ui.add(egui::TextEdit::singleline(&mut bs.search).hint_text("e.g. blur, wipe").desired_width(f32::INFINITY));
        });
        let target = st.selection.filter(|id| st.visual(*id).is_some());
        let q = bs.search.to_lowercase();
        let hit = |label: &str, desc: &str, cat: &str| q.is_empty() || label.to_lowercase().contains(&q) || desc.to_lowercase().contains(&q) || cat.to_lowercase().contains(&q);
        ui.separator();
        if bs.transitions {
            ui.weak("Apply puts it on the video cut nearest the playhead (1 s if there was no transition).");
        } else if target.is_none() {
            ui.colored_label(ui.visuals().warn_fg_color, "Select a video, image or title clip to add effects to it.");
        }
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            let mut any = false;
            if bs.transitions {
                for (group, kinds) in transition_groups() {
                    let shown: Vec<Transition> = kinds.into_iter().filter(|k| hit(k.label(), k.description(), group)).collect();
                    if shown.is_empty() {
                        continue;
                    }
                    any = true;
                    ui.strong(group);
                    for k in shown {
                        if entry(ui, k.label(), k.description(), "Apply", true) {
                            st.apply_transition(k);
                        }
                    }
                    ui.add_space(6.0);
                }
            } else {
                let plugins = reelforge_core::plugin::installed();
                for cat in EffectCategory::ALL {
                    let shown: Vec<EffectKind> = EffectKind::ALL.into_iter().filter(|k| k.category() == cat && hit(k.label(), k.description(), cat.label())).collect();
                    let shown_plugins: Vec<_> = plugins.iter().filter(|p| p.category == cat && (hit(&p.name, &p.description, cat.label()) || hit("plugin", &p.author, ""))).cloned().collect();
                    if shown.is_empty() && shown_plugins.is_empty() {
                        continue;
                    }
                    any = true;
                    ui.strong(cat.label());
                    for k in shown {
                        if entry(ui, k.label(), k.description(), "Add", target.is_some()) {
                            if let Some(id) = target {
                                add_effect(st, id, Effect::new(k));
                            }
                        }
                    }
                    for p in shown_plugins {
                        let by = if p.author.is_empty() { String::new() } else { format!(" · by {}", p.author) };
                        let desc = format!("{}{}Plugin{by}", p.description, if p.description.is_empty() { "" } else { "  ·  " });
                        if entry(ui, &p.name, &desc, "Add", target.is_some()) {
                            if let Some(id) = target {
                                add_effect(st, id, Effect::new_plugin(&p));
                            }
                        }
                    }
                    ui.add_space(6.0);
                }
                ui.horizontal(|ui| {
                    ui.weak(format!("{} plugin{} installed.", plugins.len(), if plugins.len() == 1 { "" } else { "s" }));
                    if ui.small_button("Manage plugins…").clicked() {
                        st.plugins.open = true;
                    }
                });
            }
            if !any {
                ui.weak("Nothing matches.");
            }
        });
    });
    ctx.data_mut(|d| d.insert_temp(id, bs));
}

/// Add an effect to a clip as one undo step.
pub fn add_effect(st: &mut EditorState, id: reelforge_core::ClipId, effect: Effect) {
    st.commit_live_edit();
    if let Some(mut v) = st.visual(id) {
        let name = effect.label();
        v.effects.push(effect);
        st.set_visual_live(id, v);
        st.commit_live_edit();
        st.status = format!("Added {name} to the clip");
    }
}

fn transition_groups() -> Vec<(&'static str, Vec<Transition>)> {
    let pick = |f: fn(Transition) -> bool| Transition::ALL.into_iter().filter(|t| f(*t)).collect::<Vec<_>>();
    vec![
        ("Dissolves", pick(|t| matches!(t, Transition::Dissolve | Transition::DipToBlack | Transition::DipToWhite))),
        ("Wipes", pick(Transition::is_wipe)),
        ("Slides", pick(Transition::is_slide)),
        ("Pushes", pick(Transition::is_push)),
    ]
}

/// One row: name, description and a button.
fn entry(ui: &mut egui::Ui, label: &str, desc: &str, button: &str, enabled: bool) -> bool {
    let mut go = false;
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            let text_w = (ui.available_width() - 64.0).max(80.0);
            ui.vertical(|ui| {
                ui.set_max_width(text_w);
                ui.label(label);
                ui.weak(desc);
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                go = ui.add_enabled(enabled, egui::Button::new(button)).clicked();
            });
        });
    });
    go
}

// ---- Tone-curve editor ---------------------------------------------------------------

const CHANNELS: [(&str, Color32); 4] = [
    ("Master", Color32::from_gray(220)),
    ("Red", Color32::from_rgb(235, 80, 80)),
    ("Green", Color32::from_rgb(80, 200, 90)),
    ("Blue", Color32::from_rgb(90, 140, 255)),
];

/// Edit tone curves: drag points, double-click to add one, right-click to remove.
/// Returns whether anything changed.
pub fn curves_editor(ui: &mut egui::Ui, curves: &mut Curves) -> bool {
    let ch_id = ui.id().with("curve_channel");
    let drag_id = ui.id().with("curve_drag");
    let mut ch: usize = ui.data(|d| d.get_temp(ch_id).unwrap_or(0));
    let mut changed = false;
    ui.horizontal(|ui| {
        for (i, (name, _)) in CHANNELS.iter().enumerate() {
            ui.selectable_value(&mut ch, i, *name);
        }
        if ui.small_button("Reset").on_hover_text("Straighten this channel's curve").clicked() {
            *curves.channel_mut(ch) = vec![[0.0, 0.0], [1.0, 1.0]];
            changed = true;
        }
    });
    ui.data_mut(|d| d.insert_temp(ch_id, ch));

    let side = ui.available_width().clamp(120.0, 260.0);
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(side), Sense::click_and_drag());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 2.0, Color32::from_gray(24));
    let grid = Stroke::new(1.0, Color32::from_gray(48));
    for i in 1..4 {
        let f = i as f32 / 4.0;
        p.line_segment([Pos2::new(rect.left() + f * side, rect.top()), Pos2::new(rect.left() + f * side, rect.bottom())], grid);
        p.line_segment([Pos2::new(rect.left(), rect.top() + f * side), Pos2::new(rect.right(), rect.top() + f * side)], grid);
    }
    p.line_segment([rect.left_bottom(), rect.right_top()], Stroke::new(1.0, Color32::from_gray(64)));
    let to_screen = |v: [f32; 2]| Pos2::new(rect.left() + v[0] * side, rect.bottom() - v[1] * side);
    let from_screen = |s: Pos2| [((s.x - rect.left()) / side).clamp(0.0, 1.0), ((rect.bottom() - s.y) / side).clamp(0.0, 1.0)];

    // Interaction on the active channel.
    let pts = curves.channel_mut(ch);
    pts.sort_by(|a, b| a[0].total_cmp(&b[0]));
    let near = |pts: &Vec<[f32; 2]>, s: Pos2| pts.iter().enumerate().map(|(i, v)| (i, to_screen(*v).distance(s))).filter(|(_, d)| *d < 8.0).min_by(|a, b| a.1.total_cmp(&b.1)).map(|(i, _)| i);
    let mut dragging: Option<usize> = ui.data(|d| d.get_temp(drag_id));
    if let Some(pos) = resp.interact_pointer_pos() {
        if resp.drag_started() {
            // Pick the point under where the press began, not where the pointer is now.
            let origin = ui.input(|i| i.pointer.press_origin()).unwrap_or(pos);
            dragging = near(pts, origin);
        }
        if resp.double_clicked() && near(pts, pos).is_none() {
            let v = from_screen(pos);
            pts.push([v[0], eval_curve(pts, v[0])]);
            changed = true;
        }
        if resp.secondary_clicked() {
            if let Some(i) = near(pts, pos) {
                if pts.len() > 2 {
                    pts.remove(i);
                    changed = true;
                }
            }
        }
        if let (Some(i), true) = (dragging, resp.dragged()) {
            if i < pts.len() {
                let mut v = from_screen(pos);
                // Keep points in order: an inner point stays between its neighbours.
                let lo = if i > 0 { pts[i - 1][0] + 0.01 } else { 0.0 };
                let hi = if i + 1 < pts.len() { pts[i + 1][0] - 0.01 } else { 1.0 };
                v[0] = v[0].clamp(lo, hi.max(lo));
                if pts[i] != v {
                    pts[i] = v;
                    changed = true;
                }
            }
        }
    }
    if resp.drag_stopped() || !resp.dragged() && !resp.drag_started() {
        dragging = None;
    }
    ui.data_mut(|d| match dragging {
        Some(i) => {
            d.insert_temp(drag_id, i);
        }
        None => d.remove::<usize>(drag_id),
    });

    // Draw the inactive channels faintly, then the active one with its points.
    for (i, c) in curves.channels().iter().enumerate() {
        if i == ch {
            continue;
        }
        let line: Vec<Pos2> = (0..=64).map(|k| to_screen([k as f32 / 64.0, eval_curve(c, k as f32 / 64.0)])).collect();
        p.add(egui::Shape::line(line, Stroke::new(1.0, CHANNELS[i].1.gamma_multiply(0.35))));
    }
    let c = curves.channels()[ch];
    let line: Vec<Pos2> = (0..=128).map(|k| to_screen([k as f32 / 128.0, eval_curve(c, k as f32 / 128.0)])).collect();
    p.add(egui::Shape::line(line, Stroke::new(2.0, CHANNELS[ch].1)));
    let hover = resp.hover_pos().and_then(|s| near(c, s));
    for (i, v) in c.iter().enumerate() {
        let hot = hover == Some(i) || dragging == Some(i);
        p.circle(to_screen(*v), if hot { 5.5 } else { 4.0 }, Color32::from_gray(20), Stroke::new(1.5, CHANNELS[ch].1));
    }
    resp.on_hover_text("Drag a point to bend the curve. Double-click to add a point, right-click a point to remove it.");
    changed
}

// ---- LUT picker -----------------------------------------------------------------------

/// Choose a .cube file. Returns whether the path changed.
pub fn lut_picker(ui: &mut egui::Ui, path: &mut Option<PathBuf>) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        let name = path.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "No LUT chosen".into());
        ui.label(name).on_hover_text(path.as_ref().map(|p| p.display().to_string()).unwrap_or_default());
        if ui.button("Choose .cube…").clicked() {
            if let Some(p) = rfd::FileDialog::new().add_filter("3D LUT", &["cube", "CUBE"]).pick_file() {
                *path = Some(p);
                changed = true;
            }
        }
    });
    if let Some(p) = path {
        if let Err(e) = reelforge_core::lut::load_cube(p) {
            ui.colored_label(ui.visuals().error_fg_color, e);
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_transition_is_in_one_group() {
        let all: Vec<Transition> = transition_groups().into_iter().flat_map(|(_, k)| k).collect();
        assert_eq!(all.len(), Transition::ALL.len());
        for t in Transition::ALL {
            assert!(all.contains(&t), "{t:?}");
        }
    }
}
