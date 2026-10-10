//! Curves panel: a value graph of the selected clip's keyframes.
//!
//! Lists every keyframeable channel of the clip (transform components and
//! effect parameters); the ones ticked are drawn over the clip's time span.
//! Keys can be dragged in time and value, given an interpolation, added
//! (double-click a curve) and deleted (double-click or Alt+click a key, or the
//! right-click menu). Each drag is one undo step, like every other edit.

use crate::app::EditorState;
use egui::{Align2, Color32, CursorIcon, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};
use reelforge_core::{Channel, ClipId, ClipVisual, Interp, Time};

const LIST_W: f32 = 200.0;
const KEY_R: f32 = 5.0;

#[derive(Default)]
pub struct CurveView {
    /// Channels drawn, per clip (remembered when switching clips).
    shown: Vec<(ClipId, Channel)>,
    /// Clips whose shown list has been initialised (all animated channels at first).
    seeded: Vec<ClipId>,
    drag: Option<KeyDrag>,
    /// The key last clicked: `(channel, index)`.
    selected: Option<(Channel, usize)>,
    /// The key the context menu is for.
    menu_key: Option<(Channel, usize)>,
}

struct KeyDrag {
    clip: ClipId,
    ch: Channel,
    index: usize,
    /// The clip's look before the drag (each frame re-applies the move to it).
    orig: ClipVisual,
    /// Value range frozen for the drag, so the graph doesn't rescale under the pointer.
    range: (f32, f32),
}

fn channel_color(i: usize) -> Color32 {
    const C: [Color32; 8] = [
        Color32::from_rgb(240, 90, 90),
        Color32::from_rgb(110, 210, 110),
        Color32::from_rgb(100, 160, 255),
        Color32::from_rgb(255, 200, 70),
        Color32::from_rgb(210, 120, 255),
        Color32::from_rgb(80, 220, 220),
        Color32::from_rgb(255, 150, 60),
        Color32::from_rgb(220, 220, 220),
    ];
    C[i % C.len()]
}

/// A step for grid lines that gives roughly `target` lines over `span`.
fn nice_step(span: f32, target: f32) -> f32 {
    let raw = (span / target).max(1e-6);
    let mag = 10f32.powf(raw.log10().floor());
    [1.0, 2.0, 5.0, 10.0].into_iter().map(|m| m * mag).find(|s| *s >= raw).unwrap_or(10.0 * mag)
}

pub fn show(ui: &mut egui::Ui, st: &mut EditorState) {
    let Some(id) = st.selection else {
        ui.centered_and_justified(|ui| ui.weak("Select a video clip to see its keyframe curves."));
        return;
    };
    let Some((ti, clip)) = st.seq().find_clip(id).map(|(t, c)| (t, c.clone())) else { return };
    if st.seq().tracks[ti].kind != reelforge_core::TrackKind::Video {
        ui.centered_and_justified(|ui| ui.weak("Curves are for video clips. Audio volume keyframes are edited on the clip in the timeline."));
        return;
    }
    let locked = st.seq().is_locked(id);
    let rate = st.seq().frame_rate;
    let v = clip.visual();
    let all = v.channels();
    let cv = &mut st.curves;
    if !cv.seeded.contains(&id) {
        cv.seeded.push(id);
        cv.shown.extend(all.iter().filter(|c| v.channel_animated(**c)).map(|c| (id, *c)));
    }
    // Drop channels that no longer exist (an effect was removed).
    cv.shown.retain(|(cid, ch)| *cid != id || all.contains(ch));

    let full = ui.available_rect_before_wrap();
    let list_rect = Rect::from_min_size(full.min, Vec2::new(LIST_W.min(full.width() * 0.4), full.height()));
    let graph_rect = Rect::from_min_max(Pos2::new(list_rect.right() + 6.0, full.top()), full.max);

    // ---- Channel list ----
    let mut toggled = None;
    ui.scope_builder(egui::UiBuilder::new().max_rect(list_rect), |ui| {
        egui::ScrollArea::vertical().id_salt("curve_channels").show(ui, |ui| {
            ui.set_width(list_rect.width() - 8.0);
            for (i, ch) in all.iter().enumerate() {
                let on = cv.shown.contains(&(id, *ch));
                let keys = v.channel_keys(*ch).len();
                let mut text = egui::RichText::new(v.channel_name(*ch)).size(12.0);
                if on {
                    text = text.color(channel_color(i));
                } else if keys == 0 {
                    text = text.weak();
                }
                let label = if keys > 0 { format!("{keys} key{}", if keys == 1 { "" } else { "s" }) } else { String::new() };
                ui.horizontal(|ui| {
                    if ui.selectable_label(on, text).on_hover_text("Show or hide this curve").clicked() {
                        toggled = Some(*ch);
                    }
                    ui.weak(label);
                });
            }
        });
    });
    if let Some(ch) = toggled {
        match cv.shown.iter().position(|s| *s == (id, ch)) {
            Some(i) => {
                cv.shown.remove(i);
            }
            None => cv.shown.push((id, ch)),
        }
    }
    let shown: Vec<(usize, Channel)> = all.iter().enumerate().filter(|(_, c)| cv.shown.contains(&(id, **c))).map(|(i, c)| (i, *c)).collect();

    // ---- Graph ----
    let resp = ui.interact(graph_rect, egui::Id::new("curve_graph"), Sense::click_and_drag());
    let painter = ui.painter_at(graph_rect);
    let visuals = ui.visuals().clone();
    painter.rect_filled(graph_rect, 2.0, visuals.extreme_bg_color);
    let plot = graph_rect.shrink2(Vec2::new(46.0, 14.0)).translate(Vec2::new(20.0, 0.0));

    // Time axis: the clip's span on the timeline, a little either side.
    let pad = Time((clip.duration.0 / 20).max(rate.frame_duration().0));
    let (t0, t1) = (clip.timeline_start - pad, clip.timeline_end() + pad);
    let span = (t1 - t0).as_secs_f64().max(1e-6) as f32;
    let x_of = |t: Time| plot.left() + (t - t0).as_secs_f64() as f32 / span * plot.width();
    let t_of = |x: f32| t0 + Time::from_secs_f64(((x - plot.left()) / plot.width() * span) as f64);
    let src_of = |t: Time| clip.source_time(t);
    let tl_of = |src: Time| clip.timeline_start + (src - clip.source_in);

    // Value axis: fit everything shown (frozen while dragging).
    let range = match &cv.drag {
        Some(d) if d.clip == id => d.range,
        _ => {
            let (mut lo, mut hi) = (f32::INFINITY, f32::NEG_INFINITY);
            for (_, ch) in &shown {
                let (mul, _) = v.channel_display(*ch);
                let mut x = plot.left();
                while x <= plot.right() {
                    let val = v.channel_value(*ch, src_of(t_of(x))) * mul;
                    lo = lo.min(val);
                    hi = hi.max(val);
                    x += 4.0;
                }
                for (_, val, _) in v.channel_keys(*ch) {
                    lo = lo.min(val * mul);
                    hi = hi.max(val * mul);
                }
            }
            if !lo.is_finite() {
                (0.0, 1.0)
            } else {
                let extra = ((hi - lo) * 0.1).max(1.0f32.max(hi.abs().max(lo.abs()) * 0.05));
                (lo - extra, hi + extra)
            }
        }
    };
    let y_of = |val: f32| plot.bottom() - (val - range.0) / (range.1 - range.0) * plot.height();
    let val_of = |y: f32| range.0 + (plot.bottom() - y) / plot.height() * (range.1 - range.0);

    // Grid: values on the left, timecodes along the bottom, the clip's extent shaded.
    painter.rect_filled(Rect::from_x_y_ranges(x_of(clip.timeline_start)..=x_of(clip.timeline_end()), plot.y_range()), 0.0, visuals.faint_bg_color);
    let vstep = nice_step(range.1 - range.0, (plot.height() / 28.0).max(2.0));
    let mut gv = (range.0 / vstep).ceil() * vstep;
    while gv <= range.1 {
        let y = y_of(gv);
        painter.line_segment([Pos2::new(plot.left(), y), Pos2::new(plot.right(), y)], Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color.gamma_multiply(0.6)));
        let label = if vstep < 1.0 { format!("{gv:.2}") } else { format!("{gv:.0}") };
        painter.text(Pos2::new(graph_rect.left() + 4.0, y), Align2::LEFT_CENTER, label, FontId::monospace(9.5), visuals.weak_text_color());
        gv += vstep;
    }
    let tstep = nice_step(span, (plot.width() / 110.0).max(2.0)) as f64;
    let mut gt = (t0.as_secs_f64() / tstep).ceil() * tstep;
    while gt <= t1.as_secs_f64() {
        let t = Time::from_secs_f64(gt);
        let x = x_of(t);
        painter.line_segment([Pos2::new(x, plot.top()), Pos2::new(x, plot.bottom())], Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color.gamma_multiply(0.4)));
        painter.text(Pos2::new(x + 2.0, graph_rect.bottom() - 1.0), Align2::LEFT_BOTTOM, t.timecode(rate), FontId::monospace(9.0), visuals.weak_text_color());
        gt += tstep;
    }
    if shown.is_empty() {
        painter.text(plot.center(), Align2::CENTER_CENTER, "Pick channels on the left to see their curves", FontId::proportional(13.0), visuals.weak_text_color());
    }

    // Curves and keys.
    let pointer = resp.interact_pointer_pos().or(resp.hover_pos());
    let mut spots: Vec<(Channel, usize, Pos2)> = Vec::new(); // every key drawn, for hit-testing
    for &(ci, ch) in &shown {
        let col = channel_color(ci);
        let (mul, _) = v.channel_display(ch);
        let mut pts = Vec::new();
        let mut x = plot.left();
        while x <= plot.right() + 0.5 {
            pts.push(Pos2::new(x, y_of(v.channel_value(ch, src_of(t_of(x))) * mul)));
            x += 2.0;
        }
        painter.add(egui::Shape::line(pts, Stroke::new(1.6, col)));
        for (k, (at, val, interp)) in v.channel_keys(ch).into_iter().enumerate() {
            let c = Pos2::new(x_of(tl_of(at)), y_of(val * mul));
            if !plot.expand(KEY_R).contains(c) {
                continue;
            }
            let sel = cv.selected == Some((ch, k));
            let shape: Vec<Pos2> = match interp {
                // Diamond for smooth keys, square for hold, as in most editors.
                Interp::Hold => vec![c + Vec2::new(-KEY_R, -KEY_R), c + Vec2::new(KEY_R, -KEY_R), c + Vec2::new(KEY_R, KEY_R), c + Vec2::new(-KEY_R, KEY_R)],
                _ => vec![c + Vec2::new(0.0, -KEY_R - 1.0), c + Vec2::new(KEY_R + 1.0, 0.0), c + Vec2::new(0.0, KEY_R + 1.0), c + Vec2::new(-KEY_R - 1.0, 0.0)],
            };
            let fill = if interp == Interp::Ease { visuals.extreme_bg_color } else { col };
            painter.add(egui::Shape::convex_polygon(shape, fill, Stroke::new(if sel { 2.5 } else { 1.5 }, if sel { Color32::WHITE } else { col })));
            spots.push((ch, k, c));
        }
    }
    // Playhead.
    let px = x_of(st.playback.playhead);
    if px >= plot.left() && px <= plot.right() {
        painter.line_segment([Pos2::new(px, graph_rect.top()), Pos2::new(px, graph_rect.bottom())], Stroke::new(1.5, Color32::from_rgb(235, 64, 52)));
    }
    painter.rect_stroke(plot, 0.0, Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color), StrokeKind::Outside);

    // ---- Interaction ----
    let key_at = |p: Pos2| -> Option<(Channel, usize, f32)> {
        spots
            .iter()
            .map(|&(ch, k, c)| (ch, k, p.distance(c)))
            .filter(|(.., d)| *d <= KEY_R + 3.0)
            .min_by(|a, b| a.2.total_cmp(&b.2))
    };
    let hover_key = pointer.and_then(key_at);
    let frame = rate.frame_duration();
    let alt = ui.input(|i| i.modifiers.alt);
    if hover_key.is_some() && cv.drag.is_none() {
        ui.ctx().set_cursor_icon(if locked { CursorIcon::NotAllowed } else { CursorIcon::Grab });
    }
    if resp.drag_started() && !locked {
        // Hit-test where the button went down, not where the drag threshold was crossed.
        if let Some((ch, index, _)) = ui.input(|i| i.pointer.press_origin()).or(pointer).and_then(key_at) {
            cv.drag = Some(KeyDrag { clip: id, ch, index, orig: v.clone(), range });
            cv.selected = Some((ch, index));
        }
    }
    let mut new_visual: Option<ClipVisual> = None;
    let mut seek = None;
    let mut status = None;
    if resp.dragged() {
        match (&cv.drag, pointer) {
            (Some(d), Some(p)) if d.clip == id => {
                let shift = ui.input(|i| i.modifiers.shift);
                let mut nv = d.orig.clone();
                let (mul, unit) = nv.channel_display(d.ch);
                let (orig_at, _, _) = d.orig.channel_keys(d.ch)[d.index];
                // Shift: change only the value, keep the time.
                let at = if shift { orig_at } else { src_of(t_of(p.x).snap(rate)) };
                let at = at.max(clip.source_in).min(clip.source_in + clip.duration);
                let val = val_of(p.y) / mul;
                let used = nv.channel_set_key(d.ch, d.index, at, val, frame);
                let shown_val = nv.channel_keys(d.ch)[d.index].1 * mul;
                status = Some(format!("{}: {:.2}{unit} at {}", nv.channel_name(d.ch), shown_val, tl_of(used).timecode(rate)));
                seek = Some(tl_of(used));
                new_visual = Some(nv);
                ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
            }
            (None, Some(p)) if plot.contains(p) => seek = Some(t_of(p.x).snap(rate).max(Time::ZERO)),
            _ => {}
        }
    }
    if resp.drag_stopped() {
        cv.drag = None;
    }

    if resp.secondary_clicked() {
        cv.menu_key = hover_key.map(|(c, k, _)| (c, k));
        if let Some(k) = cv.menu_key {
            cv.selected = Some(k);
        }
    }
    let menu_key = cv.menu_key;
    resp.context_menu(|ui| {
        let Some((ch, k)) = menu_key else {
            ui.weak("Right-click a keyframe for its options");
            return;
        };
        if locked {
            ui.weak("The track is locked");
            return;
        }
        let mut nv = v.clone();
        let cur = nv.channel_keys(ch).get(k).map(|k| k.2);
        for i in Interp::ALL {
            if ui.radio(cur == Some(i), i.label()).clicked() {
                nv.channel_set_interp(ch, k, i);
                new_visual = Some(nv.clone());
                ui.close();
            }
        }
        ui.separator();
        if ui.button("Delete keyframe").clicked() {
            nv.channel_remove_key(ch, k);
            new_visual = Some(nv.clone());
            ui.close();
        }
    });

    if (resp.double_clicked() || (resp.clicked() && alt)) && !locked {
        if let Some(p) = pointer {
            match hover_key {
                // Double-click or Alt+click a key: delete it.
                Some((ch, k, _)) => {
                    let mut nv = v.clone();
                    nv.channel_remove_key(ch, k);
                    status = Some(format!("Deleted a {} keyframe", nv.channel_name(ch)));
                    new_visual = Some(nv);
                    cv.selected = None;
                }
                // Double-click near a curve: add a key on it there.
                None if resp.double_clicked() && plot.contains(p) => {
                    let at = src_of(t_of(p.x).snap(rate)).max(clip.source_in).min(clip.source_in + clip.duration);
                    let near = shown
                        .iter()
                        .map(|&(_, ch)| {
                            let (mul, _) = v.channel_display(ch);
                            (ch, (y_of(v.channel_value(ch, at) * mul) - p.y).abs())
                        })
                        .filter(|(_, d)| *d < 24.0)
                        .min_by(|a, b| a.1.total_cmp(&b.1));
                    if let Some((ch, _)) = near {
                        let mut nv = v.clone();
                        if let Some(k) = nv.channel_add_key(ch, at) {
                            cv.selected = Some((ch, k));
                        }
                        status = Some(format!("Added a {} keyframe", nv.channel_name(ch)));
                        new_visual = Some(nv);
                    } else {
                        status = Some("Double-click on a curve to add a keyframe there".into());
                    }
                }
                None => {}
            }
        }
    } else if resp.clicked() {
        match hover_key {
            Some((ch, k, _)) => {
                cv.selected = Some((ch, k));
                if let Some((at, ..)) = v.channel_keys(ch).get(k) {
                    seek = Some(tl_of(*at));
                }
            }
            None => {
                cv.selected = None;
                if let Some(p) = pointer.filter(|p| plot.contains(*p)) {
                    seek = Some(t_of(p.x).snap(rate).max(Time::ZERO));
                }
            }
        }
    }

    // Hint line at the top of the graph.
    let hint = if locked {
        "Locked track: unlock it to edit keyframes".to_string()
    } else if let Some((ch, k)) = cv.selected.filter(|(ch, k)| v.channel_keys(*ch).len() > *k) {
        let (at, val, interp) = v.channel_keys(ch)[k];
        let (mul, unit) = v.channel_display(ch);
        format!("{} · key at {} = {:.2}{unit} · {}   (drag to move, Shift: value only, right-click for options)", v.channel_name(ch), tl_of(at).timecode(rate), val * mul, interp.label())
    } else {
        "Drag keys to move them · double-click a curve to add a key · double-click a key to delete it".into()
    };
    painter.text(Pos2::new(plot.left() + 4.0, graph_rect.top() + 2.0), Align2::LEFT_TOP, hint, FontId::proportional(11.0), visuals.weak_text_color());

    if let Some(nv) = new_visual {
        if nv != v {
            st.set_visual_live(id, nv);
        }
    }
    if let Some(t) = seek {
        st.playback.seek(t);
    }
    if let Some(s) = status {
        st.status = s;
    }
}
