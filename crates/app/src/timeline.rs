//! Custom-painted editor timeline: ruler, tracks, clips, playhead, drag/move.

use crate::app::EditorState;
use egui::{Align2, Color32, CursorIcon, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};
use reelforge_core::undo::{Edge, Timing};
use reelforge_core::audio::{db_to_gain, fade_curve, MAX_DB, MIN_DB};
use reelforge_core::ClipAudio;
use reelforge_core::{ClipId, MediaId, Time, TrackKind, TrimMode};

const RULER_H: f32 = 26.0;
/// Height of the row under the tracks with the "add track" buttons.
const ADD_ROW_H: f32 = 34.0;
const TRACK_H: f32 = 52.0;
const HEADER_W: f32 = 124.0;

pub struct TimelineView {
    pub px_per_sec: f32,
    /// Seconds scrolled off the left edge.
    pub scroll: f32,
    /// What dragging a clip edge does: a plain trim, a ripple, or a roll.
    pub mode: TrimMode,
    /// Pixels scrolled off the top of the track area.
    pub vscroll: f32,
    /// A track name being edited: (track, text, focus requested yet).
    renaming: Option<(usize, String, bool)>,
    drag: Option<Drag>,
    /// The marker whose editor (name, colour) is open.
    pub marker_edit: Option<usize>,
    /// Zoom so the whole sequence fits, next time the timeline is drawn.
    pub fit_request: bool,
    /// ...without saying so in the status bar (a sequence opened for the first time).
    pub fit_quietly: bool,
}

/// Track-header actions, applied after drawing.
enum TrackAction {
    Add(TrackKind),
    Delete(usize),
    StartRename(usize),
    Rename(usize, String),
}

impl Default for TimelineView {
    fn default() -> Self {
        Self { px_per_sec: 60.0, scroll: 0.0, mode: TrimMode::Normal, vscroll: 0.0, renaming: None, drag: None, marker_edit: None, fit_request: false, fit_quietly: false }
    }
}

enum Drag {
    Scrub,
    Clip { id: ClipId, grab: Time, from: (usize, Time), to: (usize, Time) },
    /// Dragging an audio clip's fade-in (`out == false`) or fade-out handle.
    Fade { id: ClipId, out: bool },
    /// Dragging one volume keyframe (index into the clip's keys).
    VolKey { id: ClipId, index: usize },
    /// Dragging the volume line up/down: the clip's audio before the drag, and where it started.
    VolLine { id: ClipId, start_y: f32, orig: ClipAudio },
    /// Trimming an edge: the clip grabbed, which edge, where that edge was, the
    /// tool in use, and the timings of every clip before the drag.
    Trim { id: ClipId, edge: Edge, edge_at: Time, mode: TrimMode, start: Vec<(ClipId, Timing)> },
    /// Slipping or sliding a whole clip: where the pointer went down (timeline
    /// time), the tool, and every clip's timing before the drag.
    Body { id: ClipId, grab: Time, mode: TrimMode, start: Vec<(ClipId, Timing)> },
    /// Dragging a box over empty track space to select the clips it touches.
    Marquee { from: Pos2, add: bool },
    /// Dragging a marker along the ruler.
    Marker { index: usize },
}

/// How close (px) to a clip's edge counts as grabbing it for trimming.
const TRIM_PX: f32 = 6.0;

enum VolHit {
    Key(ClipId, usize),
    Line(ClipId),
}

/// Vertical band of an audio clip used by the volume line (below the name/handles).
fn vol_band(cr: Rect) -> (f32, f32) {
    (cr.top() + 18.0, cr.bottom() - 4.0)
}

fn db_to_y(db: f32, cr: Rect) -> f32 {
    let (top, bottom) = vol_band(cr);
    let f = ((db - MIN_DB) / (MAX_DB - MIN_DB)).clamp(0.0, 1.0);
    bottom - f * (bottom - top)
}

fn y_to_db(y: f32, cr: Rect) -> f32 {
    let (top, bottom) = vol_band(cr);
    (MIN_DB + (bottom - y) / (bottom - top).max(1.0) * (MAX_DB - MIN_DB)).clamp(MIN_DB, MAX_DB)
}

/// Snap to whole tenths of a dB, and to 0 dB when close, so values are easy to hit.
fn tidy_db(db: f32) -> f32 {
    if db.abs() < 0.6 { 0.0 } else { (db * 10.0).round() / 10.0 }
}

/// Fade handle size and how close the pointer must be to grab one.
const FADE_HANDLE: f32 = 7.0;

/// Maps between seconds and screen x for the current view.
struct Mapping {
    x0: f32,
    pps: f32,
    scroll: f32,
}

impl Mapping {
    fn x(&self, t: Time) -> f32 {
        self.x0 + (t.as_secs_f64() as f32 - self.scroll) * self.pps
    }
    fn t(&self, x: f32) -> Time {
        Time::from_secs_f64((((x - self.x0) / self.pps) + self.scroll).max(0.0) as f64)
    }
}

/// One of the two timeline tabs: the Editor Timeline (the main edit, `comp ==
/// false`) or the Composition Timeline (the composition being worked on).
///
/// One sequence is edited at a time (it's what the viewer shows), so a tab that
/// comes into view takes over. If both tabs are visible side by side, the one
/// not in use offers to switch with a click.
pub fn tab(ui: &mut egui::Ui, st: &mut EditorState, comp: bool) {
    let k = comp as usize;
    st.timeline_drawn[k] = true;
    if comp {
        if st.active != 0 {
            st.comp_open = st.active;
        }
        if st.comp_open == 0 || st.comp_open >= st.project.sequences.len() {
            st.comp_open = if st.project.sequences.len() > 1 { 1 } else { 0 };
        }
        composition_strip(ui, st);
        if st.comp_open == 0 {
            no_compositions(ui, st);
            return;
        }
    }
    let want = if comp { st.comp_open } else { 0 };
    if st.active != want {
        let just_shown = !st.timeline_drawn_before[k];
        let other_shown = st.timeline_drawn_before[1 - k];
        // Alone on screen (and not about to be swapped for the other tab), or just brought up.
        let other = if comp { crate::app::Tab::Timeline } else { crate::app::Tab::Compositions };
        if just_shown || (!other_shown && st.focus_tab != Some(other)) {
            st.open_sequence(want);
            ui.ctx().request_repaint();
        } else {
            // Both timeline tabs are on screen and the other one is in use.
            let name = st.project.sequences[want].name.clone();
            let rect = ui.available_rect_before_wrap();
            let r = ui.allocate_rect(rect, Sense::click());
            let painter = ui.painter_at(rect);
            painter.rect_filled(rect, 0.0, ui.visuals().extreme_bg_color);
            let text = if r.hovered() { ui.visuals().strong_text_color() } else { ui.visuals().weak_text_color() };
            painter.text(rect.center(), Align2::CENTER_CENTER, format!("Click to edit {name} here"), FontId::proportional(15.0), text);
            if r.clicked() {
                st.open_sequence(want);
            }
            return;
        }
    }
    show(ui, st);
}

/// The compositions as tabs along the top of the Composition Timeline, with "+".
fn composition_strip(ui: &mut egui::Ui, st: &mut EditorState) {
    ui.horizontal(|ui| {
        let mut open = None;
        for i in 1..st.project.sequences.len() {
            let s = &st.project.sequences[i];
            let r = ui
                .selectable_label(i == st.comp_open, &s.name)
                .on_hover_text(format!("{}×{} · {:.3} fps · {}", s.width, s.height, s.frame_rate.fps(), crate::panels::format_duration(s.duration())));
            if r.clicked() {
                open = Some(i);
            }
        }
        if ui.small_button("+").on_hover_text("New empty composition").clicked() {
            st.new_composite();
        }
        if let Some(i) = open {
            st.open_request = Some(i);
        }
        if st.comp_open > 0 && st.comp_open == st.active {
            let s = st.seq();
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.weak(format!("{}×{} · {:.3} fps", s.width, s.height, s.frame_rate.fps()));
            });
        }
    });
}

/// The Composition Timeline before there are any compositions.
fn no_compositions(ui: &mut egui::Ui, st: &mut EditorState) {
    ui.add_space(24.0);
    ui.vertical_centered(|ui| {
        ui.label(egui::RichText::new("No compositions yet").strong());
        ui.add_space(4.0);
        ui.weak("A composition is a shot built from layers: footage, titles, effects and masks,");
        ui.weak("stacked and timed on their own timeline, then used in your edit like one clip.");
        ui.add_space(8.0);
        ui.label("Right-click a clip in the Media panel and choose New composition from media, or:");
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let w = 330.0;
            ui.add_space(((ui.available_width() - w) / 2.0).max(0.0));
            if ui.button("+ New composition").clicked() {
                st.new_composite();
            }
            let n = st.composite_candidates().len();
            if ui
                .add_enabled(n > 0, egui::Button::new("Make from selection   Ctrl+G"))
                .on_disabled_hover_text("Select clips in the Editor Timeline first")
                .clicked()
            {
                st.make_composite();
            }
        });
    });
}

pub fn show(ui: &mut egui::Ui, st: &mut EditorState) {
    let rate = st.seq().frame_rate;
    let rect = ui.available_rect_before_wrap();
    let resp = ui.allocate_rect(rect, Sense::click_and_drag());
    let painter = ui.painter_at(rect);

    // Display order: highest video track at the top, then audio tracks.
    let tracks = &st.seq().tracks;
    let mut rows: Vec<usize> = (0..tracks.len()).filter(|&i| tracks[i].kind == TrackKind::Video).rev().collect();
    rows.extend((0..tracks.len()).filter(|&i| tracks[i].kind == TrackKind::Audio));
    // The track area below the ruler scrolls vertically when the tracks don't fit.
    let body = Rect::from_min_max(Pos2::new(rect.left(), rect.top() + RULER_H), rect.max);
    let max_vscroll = (rows.len() as f32 * TRACK_H + ADD_ROW_H - body.height()).max(0.0);
    st.timeline.vscroll = st.timeline.vscroll.clamp(0.0, max_vscroll);
    let vscroll = st.timeline.vscroll;
    let row_rect = |row: usize| {
        let y = body.top() + row as f32 * TRACK_H - vscroll;
        Rect::from_min_size(Pos2::new(rect.left(), y), Vec2::new(rect.width(), TRACK_H))
    };
    let row_at = |y: f32| -> Option<usize> {
        if y < body.top() {
            return None;
        }
        let r = ((y - body.top() + vscroll) / TRACK_H).floor();
        (r >= 0.0 && (r as usize) < rows.len()).then(|| rows[r as usize])
    };

    // ---- Zoom / scroll ----
    if std::mem::take(&mut st.timeline.fit_request) {
        // The whole sequence across the track area, with a little room at the end.
        let width = (rect.width() - HEADER_W - 24.0).max(50.0);
        let secs = st.seq().duration().as_secs_f64().max(1.0) as f32;
        st.timeline.px_per_sec = (width / secs).clamp(2.0, 2000.0);
        st.timeline.scroll = 0.0;
        if !std::mem::take(&mut st.timeline.fit_quietly) {
            st.status = "Zoomed the timeline to fit the sequence".into();
        }
    }
    if resp.hovered() {
        let (zoom, scroll) = ui.input(|i| (i.zoom_delta(), i.smooth_scroll_delta));
        if zoom != 1.0 {
            // Zoom around the pointer.
            if let Some(p) = resp.hover_pos() {
                let m = Mapping { x0: rect.left() + HEADER_W, pps: st.timeline.px_per_sec, scroll: st.timeline.scroll };
                let anchor = m.t(p.x).as_secs_f64() as f32;
                st.timeline.px_per_sec = (st.timeline.px_per_sec * zoom).clamp(2.0, 2000.0);
                st.timeline.scroll = (anchor - (p.x - m.x0) / st.timeline.px_per_sec).max(0.0);
            }
        }
        // The wheel scrolls through the tracks when they don't all fit (Shift+wheel or a
        // sideways scroll moves along the timeline); otherwise it moves along the timeline.
        let shift = ui.input(|i| i.modifiers.shift);
        if scroll.y != 0.0 && scroll.x == 0.0 && max_vscroll > 0.0 && !shift {
            st.timeline.vscroll = (st.timeline.vscroll - scroll.y).clamp(0.0, max_vscroll);
        } else {
            let dx = if scroll.x != 0.0 { scroll.x } else { scroll.y };
            if dx != 0.0 {
                st.timeline.scroll = (st.timeline.scroll - dx / st.timeline.px_per_sec).max(0.0);
            }
        }
    }
    let m = Mapping { x0: rect.left() + HEADER_W, pps: st.timeline.px_per_sec, scroll: st.timeline.scroll };
    let lanes = Rect::from_min_max(Pos2::new(m.x0, rect.top()), rect.max);

    // ---- Background and time grid (the ruler itself is drawn last, on top) ----
    let visuals = ui.visuals().clone();
    painter.rect_filled(rect, 0.0, visuals.extreme_bg_color);
    let ruler = Rect::from_min_size(Pos2::new(m.x0, rect.top()), Vec2::new(lanes.width(), RULER_H));
    let step = [1.0 / rate.fps() as f32, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0, 300.0, 600.0]
        .into_iter()
        .find(|s| s * m.pps >= 80.0)
        .unwrap_or(1200.0);
    let ticks: Vec<(Time, f32)> = {
        let first = (m.scroll / step).floor() as i64;
        let last = ((m.scroll + lanes.width() / m.pps) / step).ceil() as i64;
        (first..=last).map(|i| Time::from_secs_f64((i as f32 * step) as f64)).map(|t| (t, m.x(t))).filter(|(_, x)| *x >= m.x0).collect()
    };
    for (_, x) in &ticks {
        painter.line_segment([Pos2::new(*x, ruler.bottom()), Pos2::new(*x, rect.bottom())], Stroke::new(1.0, visuals.faint_bg_color));
    }
    // Everything from here until the ruler is drawn stays inside the track area.
    let full_painter = painter.clone();
    let painter = painter.with_clip_rect(body);
    let mut track_action: Option<TrackAction> = None;

    // ---- Tracks and clips ----
    // Taken out while the tracks are drawn (they borrow the state).
    let mut thumbs = std::mem::take(&mut st.thumbs);
    thumbs.poll(ui.ctx());
    let selected_group: Vec<ClipId> = reelforge_core::undo::expand_links(&st.project, st.active, &st.selected_ids());
    // While dragging clips, the originals are drawn faded.
    let moving: Vec<ClipId> = match &st.timeline.drag {
        Some(Drag::Clip { .. }) => selected_group.clone(),
        _ => Vec::new(),
    };
    let mut mute_clicked = None;
    let mut lock_clicked = None;
    for (row, &ti) in rows.iter().enumerate() {
        let tr = &st.seq().tracks[ti];
        let r = row_rect(row);
        painter.line_segment([r.left_bottom(), r.right_bottom()], Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color));
        let header = Rect::from_min_size(r.min, Vec2::new(HEADER_W, TRACK_H));
        painter.rect_filled(header, 0.0, visuals.panel_fill);
        let name_rect = Rect::from_min_size(header.min + Vec2::new(4.0, 4.0), Vec2::new(HEADER_W - 8.0, 20.0));
        // Only fully visible headers take clicks (a half-hidden one sits under the ruler).
        let header_visible = body.contains_rect(header);
        if header_visible {
            // Right-click for the track menu; double-click the name to rename.
            let hresp = ui.interact(header, egui::Id::new(("track_header", ti)), Sense::click());
            if hresp.double_clicked() && name_rect.contains(hresp.interact_pointer_pos().unwrap_or_default()) {
                track_action = Some(TrackAction::StartRename(ti));
            }
            let kind = tr.kind;
            hresp.on_hover_text("Right-click for track options · double-click the name to rename").context_menu(|ui| {
                if ui.button("Rename track…").clicked() {
                    track_action = Some(TrackAction::StartRename(ti));
                    ui.close();
                }
                ui.separator();
                if ui.button("Add video track").clicked() {
                    track_action = Some(TrackAction::Add(TrackKind::Video));
                    ui.close();
                }
                if ui.button("Add audio track").clicked() {
                    track_action = Some(TrackAction::Add(TrackKind::Audio));
                    ui.close();
                }
                ui.separator();
                let what = if kind == TrackKind::Video { "Delete video track" } else { "Delete audio track" };
                if ui.button(what).clicked() {
                    track_action = Some(TrackAction::Delete(ti));
                    ui.close();
                }
            });
        }
        let renaming = matches!(&st.timeline.renaming, Some((t, ..)) if *t == ti);
        if renaming && header_visible {
            if let Some((_, text, focused)) = &mut st.timeline.renaming {
                let edit = ui.put(name_rect, egui::TextEdit::singleline(text).font(FontId::proportional(13.0)));
                if !*focused {
                    edit.request_focus();
                    *focused = true;
                }
                let (enter, escape) = ui.input(|i| (i.key_pressed(egui::Key::Enter), i.key_pressed(egui::Key::Escape)));
                if escape {
                    st.timeline.renaming = None;
                } else if edit.lost_focus() || enter {
                    track_action = Some(TrackAction::Rename(ti, text.trim().to_string()));
                }
            }
        } else {
            let name_color = if tr.muted { visuals.weak_text_color() } else { visuals.text_color() };
            painter.with_clip_rect(name_rect).text(header.left_center() + Vec2::new(8.0, -8.0), Align2::LEFT_CENTER, &tr.name, FontId::proportional(13.0), name_color);
        }
        let tr = &st.seq().tracks[ti];

        // Mute toggle ("M" for audio, eye for video), HitFilm-style.
        let btn = Rect::from_min_size(header.left_center() + Vec2::new(8.0, 3.0), Vec2::new(22.0, 16.0));
        let label = if tr.kind == TrackKind::Audio { "M" } else { "👁" };
        let bresp = ui.interact(btn, egui::Id::new(("mute", ti)), if header_visible { Sense::click() } else { Sense::hover() });
        let on = if tr.kind == TrackKind::Audio { tr.muted } else { !tr.muted };
        let fill = if on { Color32::from_rgb(200, 150, 40) } else { visuals.widgets.inactive.bg_fill };
        painter.rect_filled(btn, 3.0, if bresp.hovered() { fill.gamma_multiply(1.3) } else { fill });
        painter.text(btn.center(), Align2::CENTER_CENTER, label, FontId::proportional(11.0), if on { Color32::BLACK } else { visuals.text_color() });
        if bresp.on_hover_text(if tr.kind == TrackKind::Audio { "Mute track" } else { "Show/hide track" }).clicked() {
            mute_clicked = Some(ti);
        }
        // Lock toggle: a painted padlock (the font has no lock glyph).
        let lbtn = Rect::from_min_size(btn.right_top() + Vec2::new(4.0, 0.0), Vec2::new(22.0, 16.0));
        let lresp = ui.interact(lbtn, egui::Id::new(("lock", ti)), if header_visible { Sense::click() } else { Sense::hover() });
        let lfill = if tr.locked { Color32::from_rgb(220, 90, 70) } else { visuals.widgets.inactive.bg_fill };
        painter.rect_filled(lbtn, 3.0, if lresp.hovered() { lfill.gamma_multiply(1.3) } else { lfill });
        let ink = if tr.locked { Color32::BLACK } else { visuals.text_color() };
        let body = Rect::from_center_size(lbtn.center() + Vec2::new(0.0, 2.5), Vec2::new(9.0, 6.0));
        painter.rect_filled(body, 1.0, ink);
        let (sx0, sx1, sy) = (body.left() + 2.0, body.right() - 2.0, body.top());
        let open = if tr.locked { 0.0 } else { 2.5 };
        let shackle = vec![Pos2::new(sx0, sy), Pos2::new(sx0, sy - 3.5 - open), Pos2::new(sx0 + 1.0, sy - 4.5 - open), Pos2::new(sx1 - 1.0, sy - 4.5 - open), Pos2::new(sx1, sy - 3.5 - open), Pos2::new(sx1, sy - open)];
        painter.add(egui::Shape::line(shackle, Stroke::new(1.4, ink)));
        if lresp.on_hover_text(if tr.locked { "Unlock track" } else { "Lock track: no edits, and ripples leave it alone" }).clicked() {
            lock_clicked = Some(ti);
        }
        if tr.locked {
            // Hatch the lane so it's obvious nothing there can change.
            let lane = Rect::from_min_max(Pos2::new(m.x0, r.top()), r.max);
            let hp = painter.with_clip_rect(lane);
            let mut x = lane.left() - lane.height();
            while x < lane.right() {
                hp.line_segment([Pos2::new(x, lane.bottom()), Pos2::new(x + lane.height(), lane.top())], Stroke::new(1.0, Color32::from_white_alpha(14)));
                x += 10.0;
            }
        }

        for clip in &tr.clips {
            let dragging = moving.contains(&clip.id);
            let cr = clip_rect(&m, r, clip.timeline_start, clip.duration);
            if !cr.intersect(lanes).is_positive() {
                continue;
            }
            let vis = cr.intersect(lanes);
            let base = if clip.title.is_some() {
                Color32::from_rgb(122, 78, 170) // titles: purple, like generated media in HitFilm
            } else if clip.nested.is_some() {
                Color32::from_rgb(176, 108, 40) // compositions: amber
            } else if tr.kind == TrackKind::Video {
                Color32::from_rgb(54, 96, 160)
            } else {
                Color32::from_rgb(52, 130, 84)
            };
            let mut fill = if dragging { base.gamma_multiply(0.35) } else { base };
            if tr.muted {
                fill = fill.gamma_multiply(0.45);
            }
            if tr.locked {
                fill = fill.gamma_multiply(0.6);
            }
            painter.rect_filled(vis, 4.0, fill);

            // Picture thumbnails along video clips (not titles or compositions).
            let mut thumbs_drawn = false;
            if tr.kind == TrackKind::Video && clip.title.is_none() && clip.nested.is_none() && st.settings.timeline_thumbnails {
                thumbs_drawn = draw_thumbnails(ui.ctx(), &painter, &st.project, &mut thumbs, clip, cr, vis, &m, dragging || tr.muted || tr.locked);
            }

            // Waveform on audio clips: peaks mirrored around the centre line,
            // scaled by clip + track volume and the fades, so it shows what you'll hear.
            let (fade_in, fade_out) = clip.audio.clamped_fades(clip.duration);
            let track_gain = db_to_gain(tr.audio.volume_db);
            let envelope = |x: f32| {
                let into = ((x - cr.left()) / m.pps).max(0.0);
                let left = ((cr.right() - x) / m.pps).max(0.0);
                let src = clip.source_in + Time::from_secs_f64(into as f64);
                let mut g = track_gain * db_to_gain(clip.audio.volume_db_at(src));
                if fade_in > Time::ZERO {
                    g *= fade_curve(into / fade_in.as_secs_f64() as f32);
                }
                if fade_out > Time::ZERO {
                    g *= fade_curve(left / fade_out.as_secs_f64() as f32);
                }
                g
            };
            if tr.kind == TrackKind::Audio {
                if let Some(peaks) = st.waveforms.get(&clip.media) {
                    let wf = painter.with_clip_rect(vis.shrink(1.0));
                    let mid = vis.center().y + 6.0;
                    let half = (vis.height() - 18.0) / 2.0;
                    let color = Color32::from_rgba_unmultiplied(200, 255, 210, 150);
                    // The media under each pixel column (speed and reverse included).
                    let media_of = |x: f32| clip.media_time(clip.source_in + Time::from_secs_f64(((x - cr.left()) / m.pps) as f64)).as_secs_f64();
                    let mut x = vis.left().max(cr.left());
                    while x < vis.right() {
                        let (ta, tb) = (media_of(x), media_of(x + 1.0));
                        let (t0, t1) = (ta.min(tb), ta.max(tb));
                        let i0 = (t0.max(0.0) * crate::app::WAVEFORM_RATE as f64) as usize;
                        let i1 = (t1.max(0.0) * crate::app::WAVEFORM_RATE as f64).ceil() as usize;
                        let peak = peaks.get(i0..i1.max(i0 + 1).min(peaks.len())).map_or(0.0, |s| s.iter().cloned().fold(0.0, f32::max));
                        let h = ((peak * envelope(x)).min(1.0).sqrt() * half).max(0.5); // sqrt: quiet passages stay visible
                        wf.line_segment([Pos2::new(x, mid - h), Pos2::new(x, mid + h)], Stroke::new(1.0, color));
                        x += 1.0;
                    }
                }
            }

            // Fades: shade the faded-out area above the curve, plus a draggable handle.
            if tr.kind == TrackKind::Audio {
                let fp = painter.with_clip_rect(vis);
                let shade = Color32::from_black_alpha(110);
                for (len, out) in [(fade_in, false), (fade_out, true)] {
                    let w = len.as_secs_f64() as f32 * m.pps;
                    if w >= 1.0 {
                        let steps = (w / 3.0).clamp(4.0, 64.0) as usize;
                        let mut curve = Vec::with_capacity(steps + 1);
                        for k in 0..=steps {
                            let u = k as f32 / steps as f32;
                            let x = if out { cr.right() - w + u * w } else { cr.left() + u * w };
                            let g = if out { fade_curve(1.0 - u) } else { fade_curve(u) };
                            curve.push(Pos2::new(x, cr.bottom() - g * cr.height()));
                        }
                        // Shaded region between the curve and the clip's top edge.
                        for pair in curve.windows(2) {
                            let quad = vec![pair[0], pair[1], Pos2::new(pair[1].x, cr.top()), Pos2::new(pair[0].x, cr.top())];
                            fp.add(egui::Shape::convex_polygon(quad, shade, Stroke::NONE));
                        }
                        fp.add(egui::Shape::line(curve, Stroke::new(1.5, Color32::from_rgb(255, 230, 150))));
                    }
                    let hx = if out { cr.right() - w } else { cr.left() + w };
                    let handle = Rect::from_center_size(Pos2::new(hx, cr.top() + FADE_HANDLE / 2.0 + 1.0), Vec2::splat(FADE_HANDLE));
                    let hot = matches!(&st.timeline.drag, Some(Drag::Fade { id, out: o }) if *id == clip.id && *o == out);
                    fp.rect_filled(handle, 1.5, if hot { Color32::from_rgb(255, 196, 64) } else { Color32::from_white_alpha(200) });
                }
            }

            // Volume line ("rubber band"): flat at the clip volume, or through its keyframes.
            if tr.kind == TrackKind::Audio {
                let vp = painter.with_clip_rect(vis);
                let line_col = Color32::from_rgb(255, 210, 80);
                let x_of_src = |t: Time| cr.left() + ((t - clip.source_in).as_secs_f64() as f32) * m.pps;
                let mut pts = vec![Pos2::new(cr.left(), db_to_y(clip.audio.volume_db_at(clip.source_in), cr))];
                for k in &clip.audio.volume_keys {
                    let x = x_of_src(k.at);
                    if x > cr.left() && x < cr.right() {
                        pts.push(Pos2::new(x, db_to_y(k.db, cr)));
                    }
                }
                pts.push(Pos2::new(cr.right(), db_to_y(clip.audio.volume_db_at(clip.source_in + clip.duration), cr)));
                vp.add(egui::Shape::line(pts, Stroke::new(1.5, line_col)));
                for (i, k) in clip.audio.volume_keys.iter().enumerate() {
                    let c = Pos2::new(x_of_src(k.at), db_to_y(k.db, cr));
                    if c.x < cr.left() - 4.0 || c.x > cr.right() + 4.0 {
                        continue;
                    }
                    let hot = matches!(&st.timeline.drag, Some(Drag::VolKey { id, index }) if *id == clip.id && *index == i);
                    let r = 4.5;
                    let diamond = vec![c + Vec2::new(0.0, -r), c + Vec2::new(r, 0.0), c + Vec2::new(0.0, r), c + Vec2::new(-r, 0.0)];
                    vp.add(egui::Shape::convex_polygon(
                        diamond,
                        if hot { Color32::from_rgb(255, 196, 64) } else { line_col },
                        Stroke::new(1.0, Color32::from_black_alpha(160)),
                    ));
                }
            }

            // Transform keyframes: small diamonds along the bottom of video clips.
            if tr.kind == TrackKind::Video && clip.anim.is_animated() {
                let kp = painter.with_clip_rect(vis);
                for at in clip.anim.key_times() {
                    let x = cr.left() + ((at - clip.source_in).as_secs_f64() as f32) * m.pps;
                    let c = Pos2::new(x, cr.bottom() - 7.0);
                    let r = 4.0;
                    let d = vec![c + Vec2::new(0.0, -r), c + Vec2::new(r, 0.0), c + Vec2::new(0.0, r), c + Vec2::new(-r, 0.0)];
                    kp.add(egui::Shape::convex_polygon(d, Color32::from_rgb(255, 210, 80), Stroke::new(1.0, Color32::from_black_alpha(160))));
                }
            }

            if selected_group.contains(&clip.id) {
                let primary = st.selection == Some(clip.id);
                let w = if primary { 2.0 } else { 1.0 };
                painter.rect_stroke(vis, 4.0, Stroke::new(w, Color32::from_rgb(255, 196, 64)), StrokeKind::Inside);
            }
            let name = clip.display_name(&st.project);
            let name = if clip.title.is_some() {
                format!("T  {name}")
            } else if clip.nested.is_some() {
                format!("[C]  {name}")
            } else {
                name.into_owned()
            };
            let name = match (clip.speed, clip.reverse) {
                (0.0, _) => format!("{name}  (freeze)"),
                (s, false) if s != 1.0 => format!("{name}  ({:.0}%)", s * 100.0),
                (s, true) if s != 1.0 => format!("{name}  ({:.0}%, reversed)", s * 100.0),
                (_, true) => format!("{name}  (reversed)"),
                _ => name,
            };
            let text_painter = painter.with_clip_rect(vis.shrink(2.0));
            if thumbs_drawn {
                // A dark tab behind the name so it reads over the pictures.
                let galley = text_painter.layout_no_wrap(name.clone(), FontId::proportional(12.0), Color32::WHITE);
                let tab = Rect::from_min_size(vis.left_top() + Vec2::new(2.0, 2.0), galley.size() + Vec2::new(8.0, 4.0));
                text_painter.rect_filled(tab, 3.0, Color32::from_black_alpha(150));
            }
            text_painter.text(vis.left_top() + Vec2::new(6.0, 4.0), Align2::LEFT_TOP, name, FontId::proportional(12.0), Color32::WHITE);
        }
    }
    st.thumbs = thumbs;
    if let Some(ti) = mute_clicked {
        st.toggle_mute(ti);
    }
    if let Some(ti) = lock_clicked {
        st.toggle_lock(ti);
    }

    // "Add track" buttons under the last track.
    let add_top = row_rect(rows.len()).top();
    for (i, (kind, label, tip)) in [
        (TrackKind::Video, "+ Video", "Add a video track (on top of the other video tracks)"),
        (TrackKind::Audio, "+ Audio", "Add an audio track"),
    ]
    .into_iter()
    .enumerate()
    {
        let w = (HEADER_W - 12.0) / 2.0;
        let b = Rect::from_min_size(Pos2::new(rect.left() + 4.0 + i as f32 * (w + 4.0), add_top + 7.0), Vec2::new(w, 20.0));
        if !body.contains_rect(b) {
            continue;
        }
        let bresp = ui.interact(b, egui::Id::new(("add_track", i)), Sense::click());
        let fill = visuals.widgets.inactive.bg_fill;
        painter.rect_filled(b, 3.0, if bresp.hovered() { fill.gamma_multiply(1.4) } else { fill });
        painter.text(b.center(), Align2::CENTER_CENTER, label, FontId::proportional(11.5), visuals.text_color());
        if bresp.on_hover_text(tip).clicked() {
            track_action = Some(TrackAction::Add(kind));
        }
    }

    // Crossfades: a block with an X over the overlap; a red tick where one was
    // asked for but there's no spare media to make it.
    for x in st.project.transitions(st.active) {
        let Some(row) = rows.iter().position(|&t| t == x.track) else { continue };
        let r = row_rect(row);
        if x.is_empty() {
            let cx = m.x(x.cut);
            painter.with_clip_rect(lanes).line_segment([Pos2::new(cx, r.top() + 3.0), Pos2::new(cx, r.bottom() - 3.0)], Stroke::new(3.0, Color32::from_rgb(220, 60, 60)));
            continue;
        }
        let xr = Rect::from_min_max(Pos2::new(m.x(x.start()), r.top() + 3.0), Pos2::new(m.x(x.end()), r.bottom() - 3.0));
        let xp = painter.with_clip_rect(lanes);
        xp.rect_filled(xr, 3.0, Color32::from_rgba_unmultiplied(255, 255, 255, 40));
        xp.rect_stroke(xr, 3.0, Stroke::new(1.0, Color32::from_white_alpha(160)), StrokeKind::Inside);
        let s = Stroke::new(1.2, Color32::from_white_alpha(200));
        xp.line_segment([xr.left_top(), xr.right_bottom()], s);
        xp.line_segment([xr.left_bottom(), xr.right_top()], s);
        let kind = if x.kind == TrackKind::Video { st.seq().find_clip(x.right).map(|(_, c)| c.transition).unwrap_or_default() } else { reelforge_core::Transition::Dissolve };
        if xr.width() > 34.0 {
            let label = if x.kind == TrackKind::Video && xr.width() > 90.0 { format!("{} {:.1}s", kind.label(), x.len().as_secs_f64()) } else { format!("{:.1}s", x.len().as_secs_f64()) };
            xp.text(xr.center_bottom() - Vec2::new(0.0, 2.0), Align2::CENTER_BOTTOM, label, FontId::proportional(10.0), Color32::WHITE);
        }
    }

    // Ghosts of the clips being dragged, where they'd land.
    if let Some(Drag::Clip { id, to, .. }) = &st.timeline.drag {
        let sel = st.selected_ids();
        let plan = reelforge_core::undo::move_targets(&st.project, st.active, *id, &sel, *to);
        let ghosts: Vec<(usize, Time, Time)> = match &plan {
            Ok(moves) => moves.iter().filter_map(|mv| st.seq().find_clip(mv.id).map(|(_, c)| (mv.to_track, mv.start, c.duration))).collect(),
            Err(_) => st.seq().find_clip(*id).map(|(_, c)| vec![(to.0, to.1, c.duration)]).unwrap_or_default(),
        };
        let col = if plan.is_ok() { Color32::from_white_alpha(70) } else { Color32::from_rgba_unmultiplied(220, 60, 60, 90) };
        for (track, start, dur) in ghosts {
            if let Some(row) = rows.iter().position(|&t| t == track) {
                painter.rect_filled(clip_rect(&m, row_rect(row), start, dur).intersect(lanes), 4.0, col);
            }
        }
    }
    // The selection box.
    if let (Some(Drag::Marquee { from, .. }), Some(p)) = (&st.timeline.drag, resp.interact_pointer_pos()) {
        let r = Rect::from_two_pos(*from, p).intersect(lanes);
        painter.rect_filled(r, 2.0, Color32::from_rgba_unmultiplied(255, 196, 64, 24));
        painter.rect_stroke(r, 2.0, Stroke::new(1.0, Color32::from_rgb(255, 196, 64)), StrokeKind::Inside);
    }

    // ---- Interaction ----
    let pointer = resp.interact_pointer_pos().or(resp.hover_pos());
    let hit_clip = |st: &EditorState, p: Pos2| -> Option<(usize, ClipId, Time)> {
        if p.x < m.x0 || p.y < rect.top() + RULER_H {
            return None;
        }
        let ti = row_at(p.y)?;
        let t = m.t(p.x);
        st.seq().tracks[ti].clip_at(t).map(|c| (ti, c.id, c.timeline_start))
    };

    // A marker flag on the ruler under `p`.
    let marker_at = |st: &EditorState, p: Pos2| -> Option<usize> {
        if p.y > rect.top() + RULER_H || p.x < m.x0 - 6.0 {
            return None;
        }
        st.seq().markers.iter().enumerate().filter(|(_, mk)| (m.x(mk.at) - p.x).abs() <= 6.0).min_by(|a, b| (m.x(a.1.at) - p.x).abs().total_cmp(&(m.x(b.1.at) - p.x).abs())).map(|(i, _)| i)
    };

    // Fade handles sit on the top edge of audio clips, at the end of each fade.
    let hit_fade = |st: &EditorState, p: Pos2| -> Option<(ClipId, bool)> {
        let ti = row_at(p.y)?;
        let tr = &st.seq().tracks[ti];
        if tr.kind != TrackKind::Audio || p.x < m.x0 || tr.locked {
            return None;
        }
        let row = rows.iter().position(|&t| t == ti)?;
        let r = row_rect(row);
        let top = r.top() + 3.0;
        if (p.y - top) > FADE_HANDLE + 4.0 {
            return None;
        }
        tr.clips.iter().find_map(|c| {
            let (fi, fo) = c.audio.clamped_fades(c.duration);
            let hin = m.x(c.timeline_start + fi);
            let hout = m.x(c.timeline_end() - fo);
            let near = |x: f32| (p.x - x).abs() <= FADE_HANDLE;
            if near(hin) && p.x >= m.x(c.timeline_start) - 1.0 {
                Some((c.id, false))
            } else if near(hout) && p.x <= m.x(c.timeline_end()) + 1.0 {
                Some((c.id, true))
            } else {
                None
            }
        })
    };
    // Volume keys and line on audio clips.
    let hit_vol = |st: &EditorState, p: Pos2| -> Option<VolHit> {
        let ti = row_at(p.y)?;
        let tr = &st.seq().tracks[ti];
        if tr.kind != TrackKind::Audio || p.x < m.x0 || tr.locked {
            return None;
        }
        let row = rows.iter().position(|&t| t == ti)?;
        let c = tr.clip_at(m.t(p.x))?;
        let cr = clip_rect(&m, row_rect(row), c.timeline_start, c.duration);
        let x_of_src = |t: Time| cr.left() + ((t - c.source_in).as_secs_f64() as f32) * m.pps;
        if let Some(i) = c.audio.volume_keys.iter().position(|k| {
            (x_of_src(k.at) - p.x).abs() <= 6.0 && (db_to_y(k.db, cr) - p.y).abs() <= 6.0
        }) {
            return Some(VolHit::Key(c.id, i));
        }
        let src = c.source_time(m.t(p.x));
        ((db_to_y(c.audio.volume_db_at(src), cr) - p.y).abs() <= 4.0).then_some(VolHit::Line(c.id))
    };

    // Clip edges, for trimming. On audio clips the top strip belongs to the fade handles.
    // Slip and slide act on the clip body, so edges aren't grabbed in those modes.
    let hit_edge = |st: &EditorState, p: Pos2| -> Option<(ClipId, Edge)> {
        if p.x < m.x0 || st.timeline.mode.acts_on_body() {
            return None;
        }
        let ti = row_at(p.y)?;
        let row = rows.iter().position(|&t| t == ti)?;
        let r = row_rect(row);
        let tr = &st.seq().tracks[ti];
        if tr.kind == TrackKind::Audio && p.y < r.top() + 3.0 + FADE_HANDLE + 4.0 {
            return None;
        }
        let mut best: Option<(f32, ClipId, Edge)> = None;
        for c in tr.clips.iter().filter(|c| !st.seq().is_locked(c.id)) {
            for (x, edge) in [(m.x(c.timeline_start), Edge::Start), (m.x(c.timeline_end()), Edge::End)] {
                // Grab from inside the clip so two touching clips are both reachable.
                let inside = match edge {
                    Edge::Start => p.x >= x - 1.0,
                    Edge::End => p.x <= x + 1.0,
                };
                let d = (p.x - x).abs();
                if inside && d <= TRIM_PX && best.is_none_or(|(bd, ..)| d < bd) {
                    best = Some((d, c.id, edge));
                }
            }
        }
        best.map(|(_, id, e)| (id, e))
    };

    // Highlight the edge under the pointer (or being dragged) in the tool's colour.
    // A roll grabs the cut itself, so its marker straddles both clips.
    let hot_edge = match &st.timeline.drag {
        Some(Drag::Trim { id, edge, mode, .. }) => Some((*id, *edge, *mode)),
        None => resp.hover_pos().filter(|p| hit_fade(st, *p).is_none()).and_then(|p| hit_edge(st, p)).map(|(id, e)| (id, e, st.timeline.mode)),
        _ => None,
    };
    if let Some((id, edge, mode)) = hot_edge {
        if let Some((ti, c)) = st.seq().find_clip(id) {
            if let Some(row) = rows.iter().position(|&t| t == ti) {
                let cr = clip_rect(&m, row_rect(row), c.timeline_start, c.duration);
                let x = if edge == Edge::Start { cr.left() } else { cr.right() };
                let (x0, x1) = match (mode, edge) {
                    (TrimMode::Roll, _) => (x - 3.0, x + 3.0),
                    (_, Edge::Start) => (x, x + 4.0),
                    (_, Edge::End) => (x - 4.0, x),
                };
                let bar = Rect::from_x_y_ranges(x0..=x1, cr.y_range());
                let hp = painter.with_clip_rect(lanes);
                hp.rect_filled(bar, 1.0, mode_color(mode));
                // Ripple: a little arrow showing which way the rest of the timeline goes.
                if mode == TrimMode::Ripple {
                    let y = cr.center().y;
                    let tip = if edge == Edge::End { x + 9.0 } else { x - 9.0 };
                    let base = if edge == Edge::End { x + 2.0 } else { x - 2.0 };
                    hp.add(egui::Shape::convex_polygon(
                        vec![Pos2::new(base, y - 5.0), Pos2::new(tip, y), Pos2::new(base, y + 5.0)],
                        mode_color(mode),
                        Stroke::new(1.0, Color32::from_black_alpha(160)),
                    ));
                }
            }
        }
    }

    // Slip/slide: outline the clip (and its linked clips) in the tool's colour.
    // While slipping, also show the whole source media as a band, so you can see
    // how much is left either side of the part in use.
    let hot_body = match &st.timeline.drag {
        Some(Drag::Body { id, mode, .. }) => Some((*id, *mode, true)),
        None if st.timeline.mode.acts_on_body() => resp
            .hover_pos()
            .and_then(|p| hit_clip(st, p))
            .filter(|(_, id, _)| !st.seq().is_locked(*id))
            .map(|(_, id, _)| (id, st.timeline.mode, false)),
        _ => None,
    };
    if let Some((id, mode, dragging)) = hot_body {
        let hp = painter.with_clip_rect(lanes);
        let col = mode_color(mode);
        for (ti, c) in st.seq().link_group(id) {
            let Some(row) = rows.iter().position(|&t| t == ti) else { continue };
            let cr = clip_rect(&m, row_rect(row), c.timeline_start, c.duration);
            if dragging && mode == TrimMode::Slip && !st.project.is_still(c.media) && !c.is_retimed() {
                let len = st.project.source_len(c);
                let media_start = c.timeline_start - c.source_in;
                let band = Rect::from_x_y_ranges(m.x(media_start)..=m.x(media_start + len), cr.bottom() - 9.0..=cr.bottom() - 2.0);
                hp.rect_filled(band, 2.0, col.gamma_multiply(0.35));
                hp.rect_filled(Rect::from_x_y_ranges(cr.x_range(), band.y_range()), 0.0, col);
            }
            hp.rect_stroke(cr, 4.0, Stroke::new(2.0, col), StrokeKind::Inside);
            if c.id == id && cr.width() > 30.0 {
                // ◀ ▶ arrows in the middle: this clip moves (slide) or its content does (slip).
                let ctr = cr.center();
                for dir in [-1.0f32, 1.0] {
                    let tip = ctr + Vec2::new(11.0 * dir, 0.0);
                    let base = ctr + Vec2::new(4.0 * dir, 0.0);
                    hp.add(egui::Shape::convex_polygon(
                        vec![base + Vec2::new(0.0, -5.0), tip, base + Vec2::new(0.0, 5.0)],
                        col,
                        Stroke::new(1.0, Color32::from_black_alpha(160)),
                    ));
                }
            }
        }
    }

    if let Some(p) = resp.hover_pos() {
        if st.timeline.drag.is_none() {
            if hit_fade(st, p).is_none() && hit_edge(st, p).is_some() {
                ui.ctx().set_cursor_icon(CursorIcon::ResizeColumn);
            } else if hit_fade(st, p).is_some()
                || (st.timeline.mode.acts_on_body() && hit_clip(st, p).is_some_and(|(_, id, _)| !st.seq().is_locked(id)))
            {
                ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
            } else {
                match hit_vol(st, p) {
                    Some(VolHit::Key(..)) => ui.ctx().set_cursor_icon(CursorIcon::Grab),
                    Some(VolHit::Line(_)) => {
                        let ctrl = ui.input(|i| i.modifiers.command);
                        ui.ctx().set_cursor_icon(if ctrl { CursorIcon::Crosshair } else { CursorIcon::ResizeVertical });
                    }
                    None => {}
                }
            }
        }
    }

    if resp.drag_started() {
        // Hit-test where the button went down, not where the drag threshold was crossed.
        let origin = ui.input(|i| i.pointer.press_origin()).or(pointer);
        if let Some(i) = origin.and_then(|p| marker_at(st, p)) {
            st.timeline.drag = Some(Drag::Marker { index: i });
        } else if let Some(p) = origin {
            let edge = if hit_fade(st, p).is_none() { hit_edge(st, p) } else { None };
            let vol = if hit_fade(st, p).is_none() && edge.is_none() { hit_vol(st, p) } else { None };
            st.timeline.drag = match (hit_fade(st, p), vol) {
                (Some((id, out)), _) => {
                    st.select_only(Some(id));
                    Some(Drag::Fade { id, out })
                }
                (None, _) if edge.is_some() => {
                    let (id, edge) = edge.unwrap();
                    st.select_only(Some(id));
                    let c = st.seq().find_clip(id).map(|(_, c)| c.clone()).unwrap();
                    let edge_at = if edge == Edge::Start { c.timeline_start } else { c.timeline_end() };
                    let mode = st.timeline.mode;
                    Some(Drag::Trim { id, edge, edge_at, mode, start: st.all_timings() })
                }
                (None, Some(VolHit::Key(id, index))) => {
                    st.select_only(Some(id));
                    Some(Drag::VolKey { id, index })
                }
                (None, Some(VolHit::Line(id))) if !ui.input(|i| i.modifiers.command) => {
                    st.select_only(Some(id));
                    let orig = st.clip_audio(id).unwrap_or_default();
                    Some(Drag::VolLine { id, start_y: p.y, orig })
                }
                _ => match hit_clip(st, p) {
                Some((_, id, _)) if st.seq().is_locked(id) => {
                    st.select_only(Some(id));
                    st.status = crate::app::LOCKED.into();
                    None
                }
                Some((_, id, _)) if st.timeline.mode.acts_on_body() => {
                    st.select_only(Some(id));
                    let mode = st.timeline.mode;
                    Some(Drag::Body { id, grab: m.t(p.x), mode, start: st.all_timings() })
                }
                Some((ti, id, start)) => {
                    // Dragging a selected clip moves the whole selection; Ctrl/Shift adds to it.
                    let add = ui.input(|i| i.modifiers.command || i.modifiers.shift);
                    if st.is_selected(id) {
                        st.selection = Some(id);
                    } else if add {
                        st.add_selected(&[id]);
                        st.selection = Some(id);
                    } else {
                        st.select_only(Some(id));
                    }
                    Some(Drag::Clip { id, grab: m.t(p.x) - start, from: (ti, start), to: (ti, start) })
                }
                None if p.x >= m.x0 && p.y < rect.top() + RULER_H => Some(Drag::Scrub),
                None if p.x >= m.x0 => Some(Drag::Marquee { from: p, add: ui.input(|i| i.modifiers.command || i.modifiers.shift) }),
                None => None,
                },
            };
        }
    }

    if resp.dragged() {
        if let Some(p) = pointer {
            if let Some(Drag::Marker { index }) = st.timeline.drag {
                let mut markers = st.seq().markers.clone();
                if let Some(mk) = markers.get_mut(index) {
                    let mut t = m.t(p.x).max(Time::ZERO).snap(rate);
                    // Snap to the playhead and clip edges.
                    if !ui.input(|i| i.modifiers.alt) {
                        let targets = std::iter::once(st.playback.playhead).chain(st.seq().tracks.iter().flat_map(|t| &t.clips).flat_map(|c| [c.timeline_start, c.timeline_end()]));
                        if let Some(best) = targets.filter(|&x| (m.x(x) - m.x(t)).abs() <= SNAP_PX).min_by_key(|&x| (x - t).0.abs()) {
                            t = best;
                        }
                    }
                    mk.at = t;
                    st.status = format!("{} at {}", mk.name, t.timecode(rate));
                    st.set_markers_live(markers);
                }
                ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
            }
            if let Some(Drag::Fade { id, out }) = st.timeline.drag {
                if let Some((_, c)) = st.seq().find_clip(id) {
                    let mut a = c.audio.clone();
                    let (fi, fo) = a.clamped_fades(c.duration);
                    let t = m.t(p.x).snap(rate);
                    if out {
                        a.fade_out = (c.timeline_end() - t).max(Time::ZERO).min(c.duration - fi);
                    } else {
                        a.fade_in = (t - c.timeline_start).max(Time::ZERO).min(c.duration - fo);
                    }
                    let secs = if out { a.fade_out } else { a.fade_in }.as_secs_f64();
                    st.set_clip_audio_live(id, a);
                    st.status = format!("Fade {}: {:.2} s", if out { "out" } else { "in" }, secs);
                }
                ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
            }
            if let Some(Drag::VolKey { id, index }) = st.timeline.drag {
                if let Some((ti, c)) = st.seq().find_clip(id) {
                    if let Some(row) = rows.iter().position(|&t| t == ti) {
                        let cr = clip_rect(&m, row_rect(row), c.timeline_start, c.duration);
                        let mut a = c.audio.clone();
                        let frame = rate.frame_duration();
                        // Stay between the neighbouring keys and inside the clip.
                        let lo = if index > 0 { a.volume_keys[index - 1].at + frame } else { c.source_in };
                        let hi = a.volume_keys.get(index + 1).map(|k| k.at - frame).unwrap_or(c.source_in + c.duration);
                        let t = c.source_time(m.t(p.x).snap(rate)).max(lo).min(hi.max(lo));
                        let db = tidy_db(y_to_db(p.y, cr));
                        if let Some(k) = a.volume_keys.get_mut(index) {
                            (k.at, k.db) = (t, db);
                        }
                        st.status = format!("Volume key: {db:+.1} dB at {}", (c.timeline_start + (t - c.source_in)).timecode(rate));
                        st.set_clip_audio_live(id, a);
                    }
                }
                ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
            }
            if let Some(Drag::VolLine { id, start_y, orig }) = &st.timeline.drag {
                let (id, start_y, orig) = (*id, *start_y, orig.clone());
                if let Some((ti, c)) = st.seq().find_clip(id) {
                    if let Some(row) = rows.iter().position(|&t| t == ti) {
                        let cr = clip_rect(&m, row_rect(row), c.timeline_start, c.duration);
                        let delta = y_to_db(p.y, cr) - y_to_db(start_y, cr);
                        let mut a = orig.clone();
                        if a.is_animated() {
                            // Shift the whole automation curve.
                            for k in &mut a.volume_keys {
                                k.db = tidy_db((k.db + delta).clamp(MIN_DB, MAX_DB));
                            }
                            st.status = format!("Volume keys {delta:+.1} dB");
                        } else {
                            a.volume_db = tidy_db((orig.volume_db + delta).clamp(MIN_DB, MAX_DB));
                            st.status = format!("Clip volume: {:+.1} dB", a.volume_db);
                        }
                        st.set_clip_audio_live(id, a);
                    }
                }
                ui.ctx().set_cursor_icon(CursorIcon::ResizeVertical);
            }
            if let Some(Drag::Body { id, grab, mode, start }) = &st.timeline.drag {
                let (id, grab, mode, start) = (*id, *grab, *mode, start.clone());
                let mut delta = m.t(p.x).snap(rate) - grab.snap(rate);
                if mode == TrimMode::Slide && !ui.input(|i| i.modifiers.alt) {
                    delta = snap_slide(st, id, &start, delta, &m);
                }
                if mode == TrimMode::Slip {
                    // Dragging right reveals earlier media, like pulling the film strip along.
                    delta = Time(-delta.0);
                }
                if let Some(plan) = st.trim_live(id, Edge::Start, mode, delta, &start) {
                    if let Some((_, c)) = st.seq().find_clip(id) {
                        let (c_start, src, dur) = (c.timeline_start, c.source_in, c.duration);
                        // Show the clip's (new) first frame.
                        st.playback.seek(c_start);
                        let mut msg = format!(
                            "{} {:+.2} s  (in {} · out {} · at {})",
                            mode.label(),
                            plan.applied.as_secs_f64(),
                            src.timecode(rate),
                            (src + dur).timecode(rate),
                            c_start.timecode(rate)
                        );
                        if plan.limited {
                            msg += if mode == TrimMode::Slip { "  · no more media that way" } else { "  · at the limit" };
                        }
                        st.status = msg;
                    }
                }
                ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
            }
            if let Some(Drag::Trim { id, edge, edge_at, mode, start }) = &st.timeline.drag {
                let (id, edge, edge_at, mode, start) = (*id, *edge, *edge_at, *mode, start.clone());
                let mut to = m.t(p.x).snap(rate);
                if !ui.input(|i| i.modifiers.alt) {
                    to = snap_point(st, id, edge, edge_at, mode, &start, to, &m);
                }
                if let Some(plan) = st.trim_live(id, edge, mode, to - edge_at, &start) {
                    // Show the frame at the edit in the viewer: the last frame before an
                    // end edge, the first after a start edge. A ripple start keeps the
                    // clip where it is, so its (new) first frame stays at the old spot.
                    let at = if plan.mode == TrimMode::Ripple && edge == Edge::Start { edge_at } else { edge_at + plan.applied };
                    st.playback.seek(if edge == Edge::End { (at - rate.frame_duration()).max(Time::ZERO) } else { at });
                    if let Some((_, c)) = st.seq().find_clip(id) {
                        let what = plan.mode.label();
                        let moved = plan.changes.iter().filter(|(_, a, b)| a.2 == b.2 && a.0 != b.0).count();
                        let mut msg = format!(
                            "{what} {}: {:+.2} s  (in {} · out {} · length {})",
                            if edge == Edge::Start { "start" } else { "end" },
                            plan.applied.as_secs_f64(),
                            c.media_time(c.source_in).timecode(rate),
                            c.media_time(c.source_in + c.duration).timecode(rate),
                            c.duration.timecode(rate)
                        );
                        if plan.mode == TrimMode::Ripple && moved > 0 {
                            msg += &format!("  · {moved} clip{} shifted", if moved == 1 { "" } else { "s" });
                        }
                        if mode == TrimMode::Roll && plan.mode != TrimMode::Roll {
                            msg += "  · no clip touching this edge, so a plain trim";
                        }
                        if plan.limited {
                            msg += "  · at the limit";
                        }
                        st.status = msg;
                    }
                }
                ui.ctx().set_cursor_icon(CursorIcon::ResizeColumn);
            }
            let target = st.timeline.drag.as_ref().map(|d| match d {
                Drag::Scrub | Drag::Fade { .. } | Drag::VolKey { .. } | Drag::VolLine { .. } | Drag::Trim { .. } | Drag::Body { .. } | Drag::Marquee { .. } | Drag::Marker { .. } => None,
                Drag::Clip { id, from, grab, .. } => {
                    let kind = st.seq().tracks[from.0].kind;
                    let ti = row_at(p.y).filter(|&t| st.seq().tracks[t].kind == kind).unwrap_or(from.0);
                    let mut start = Time(m.t(p.x).0 - grab.0).max(Time::ZERO).snap(rate);
                    if !ui.input(|i| i.modifiers.alt) {
                        start = snap_clip(st, *id, start, &m);
                    }
                    Some((ti, start))
                }
            });
            match (&mut st.timeline.drag, target) {
                (Some(Drag::Scrub), _) => st.playback.seek(m.t(p.x).snap(rate)),
                (Some(Drag::Clip { to, .. }), Some(Some(t))) => *to = t,
                _ => {}
            }
            if matches!(st.timeline.drag, Some(Drag::Clip { .. })) {
                ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
            }
        }
    }

    if resp.drag_stopped() {
        match st.timeline.drag.take() {
            Some(Drag::Clip { id, from, to, .. }) if to != from => {
                let sel = st.selected_ids();
                match reelforge_core::undo::move_clips(&st.project, st.active, id, &sel, to) {
                    Ok(cmd) => st.execute(cmd),
                    Err(why) => st.status = format!("Can't move there: {why}"),
                }
            }
            Some(Drag::Marquee { from, add }) => {
                if let Some(p) = pointer {
                    let r = Rect::from_two_pos(from, p);
                    let mut hits = Vec::new();
                    for (row, &ti) in rows.iter().enumerate() {
                        for c in &st.seq().tracks[ti].clips {
                            if clip_rect(&m, row_rect(row), c.timeline_start, c.duration).intersects(r) {
                                hits.push(c.id);
                            }
                        }
                    }
                    if !add {
                        st.select_only(None);
                    }
                    st.add_selected(&hits);
                    let n = st.selected_ids().len();
                    st.status = format!("{n} clip{} selected", if n == 1 { "" } else { "s" });
                }
            }
            Some(Drag::Marker { .. }) => st.commit_live_edit(),
            _ => {}
        }
        st.timeline.drag = None;
    }

    // Ctrl+click the volume line to add a keyframe; Alt+click or double-click one to delete it.
    let (ctrl, alt) = ui.input(|i| (i.modifiers.command, i.modifiers.alt));
    let mut handled = false;
    if resp.clicked() || resp.double_clicked() {
        if let Some(p) = pointer {
            match hit_vol(st, p) {
                Some(VolHit::Key(id, index)) if alt || resp.double_clicked() => {
                    if let Some(mut a) = st.clip_audio(id) {
                        a.volume_keys.remove(index);
                        st.set_clip_audio_live(id, a);
                        st.commit_live_edit();
                    }
                    st.select_only(Some(id));
                    handled = true;
                }
                Some(VolHit::Line(id)) if ctrl => {
                    if let Some((_, c)) = st.seq().find_clip(id) {
                        let mut a = c.audio.clone();
                        let src = c.source_time(m.t(p.x).snap(rate));
                        if !a.is_animated() {
                            // First key: the curve starts from the current constant volume.
                            a.set_key(src, a.volume_db);
                        } else {
                            let db = a.volume_db_at(src);
                            a.set_key(src, db);
                        }
                        st.set_clip_audio_live(id, a);
                        st.commit_live_edit();
                        st.status = "Added volume keyframe".into();
                    }
                    st.select_only(Some(id));
                    handled = true;
                }
                _ => {}
            }
        }
    }

    // Double-click or right-click a marker to edit it.
    if resp.double_clicked() || resp.secondary_clicked() {
        if let Some(i) = pointer.and_then(|p| marker_at(st, p)) {
            st.timeline.marker_edit = Some(i);
        }
    }

    // Double-click a composition to go inside it.
    if resp.double_clicked() && !handled {
        if let Some(n) = pointer.and_then(|p| hit_clip(st, p)).and_then(|(_, id, _)| st.seq().find_clip(id).and_then(|(_, c)| c.nested)) {
            st.open_request = Some(n);
        }
    }

    if resp.clicked() && !handled {
        if let Some(p) = pointer {
            let on_ruler = p.y < rect.top() + RULER_H;
            match hit_clip(st, p) {
                Some((_, id, _)) => {
                    if ui.input(|i| i.modifiers.command || i.modifiers.shift) {
                        st.toggle_selected(id);
                    } else {
                        st.select_only(Some(id));
                    }
                }
                // The ruler only moves the playhead; empty track space also deselects.
                None if p.x >= m.x0 => {
                    if !on_ruler {
                        st.select_only(None);
                    }
                    // Clicking a marker jumps exactly to it.
                    let t = marker_at(st, p).map(|i| st.seq().markers[i].at).unwrap_or(m.t(p.x).snap(rate));
                    st.playback.seek(t);
                }
                None => {}
            }
        }
    }

    // Media dragged in from the bin.
    if let Some(media) = resp.dnd_release_payload::<MediaId>() {
        if let Some(p) = pointer {
            st.add_to_timeline(*media, row_at(p.y), Some(m.t(p.x)));
        }
    }
    if resp.dnd_hover_payload::<MediaId>().is_some() {
        if let Some(p) = pointer {
            painter.line_segment([Pos2::new(p.x, rect.top()), Pos2::new(p.x, rect.bottom())], Stroke::new(1.5, Color32::WHITE));
        }
    }

    // ---- Ruler (on top of tracks scrolled up under it) ----
    let painter = full_painter;
    painter.rect_filled(Rect::from_min_max(rect.min, Pos2::new(rect.right(), rect.top() + RULER_H)), 0.0, visuals.extreme_bg_color);
    painter.rect_filled(ruler, 0.0, visuals.faint_bg_color);
    for (t, x) in &ticks {
        painter.line_segment([Pos2::new(*x, ruler.bottom() - 8.0), Pos2::new(*x, ruler.bottom())], Stroke::new(1.0, visuals.weak_text_color()));
        painter.text(Pos2::new(x + 3.0, ruler.top() + 3.0), Align2::LEFT_TOP, t.timecode(rate), FontId::monospace(10.0), visuals.weak_text_color());
    }

    // ---- In/out marks: highlight the range on the ruler, dim outside it ----
    let (mark_in, mark_out) = (st.seq().in_point, st.seq().out_point);
    if mark_in.is_some() || mark_out.is_some() {
        let lp = painter.with_clip_rect(lanes);
        let lb = lp.with_clip_rect(Rect::from_min_max(Pos2::new(lanes.left(), body.top()), lanes.max));
        let xi = mark_in.map_or(lanes.left(), |t| m.x(t));
        let xo = mark_out.map_or(lanes.right(), |t| m.x(t));
        let blue = Color32::from_rgb(80, 150, 255);
        lp.rect_filled(Rect::from_x_y_ranges(xi..=xo, ruler.bottom() - 7.0..=ruler.bottom()), 0.0, blue.gamma_multiply(0.7));
        let shade = Color32::from_black_alpha(70);
        let below = ruler.bottom()..=rect.bottom();
        if xi > lanes.left() {
            lb.rect_filled(Rect::from_x_y_ranges(lanes.left()..=xi, below.clone()), 0.0, shade);
        }
        if xo < lanes.right() {
            lb.rect_filled(Rect::from_x_y_ranges(xo..=lanes.right(), below), 0.0, shade);
        }
        // Brackets: [ at the in point, ] at the out point.
        let bracket = |x: f32, dir: f32| {
            let s = Stroke::new(2.0, blue);
            let (top, bot) = (ruler.top() + 2.0, ruler.bottom());
            lp.line_segment([Pos2::new(x, top), Pos2::new(x, bot)], s);
            lp.line_segment([Pos2::new(x, top), Pos2::new(x + 5.0 * dir, top)], s);
            lp.line_segment([Pos2::new(x, bot), Pos2::new(x + 5.0 * dir, bot)], s);
        };
        if mark_in.is_some() {
            bracket(xi, 1.0);
        }
        if mark_out.is_some() {
            bracket(xo, -1.0);
        }
    }

    // ---- Tool buttons in the corner above the track headers ----
    let mut picked = None;
    for (i, mode) in TrimMode::ALL.into_iter().enumerate() {
        let (label, key, tip) = match mode {
            TrimMode::Normal => ("Sel", "V", "Select / trim: dragging an edge changes just that clip"),
            TrimMode::Ripple => ("Rip", "B", "Ripple: trimming an edge moves every later clip, on all unlocked tracks, to keep them butted up"),
            TrimMode::Roll => ("Roll", "N", "Roll: drag a cut between two clips; one gets longer, the other shorter, nothing else moves"),
            TrimMode::Slip => ("Slip", "Y", "Slip: drag a clip to change which part of its media it shows; it stays where it is"),
            TrimMode::Slide => ("Slide", "U", "Slide: drag a clip along the timeline; the clips either side get longer/shorter to fit"),
        };
        let w = (HEADER_W - 6.0) / TrimMode::ALL.len() as f32;
        let b = Rect::from_min_size(rect.left_top() + Vec2::new(3.0 + i as f32 * w, 4.0), Vec2::new(w - 2.0, RULER_H - 8.0));
        let bresp = ui.interact(b, egui::Id::new(("trim_tool", i)), Sense::click());
        let on = st.timeline.mode == mode;
        let fill = if on { mode_color(mode) } else { visuals.widgets.inactive.bg_fill };
        painter.rect_filled(b, 3.0, if bresp.hovered() { fill.gamma_multiply(1.25) } else { fill });
        painter.text(b.center(), Align2::CENTER_CENTER, label, FontId::proportional(10.0), if on { Color32::BLACK } else { visuals.text_color() });
        if bresp.on_hover_text(format!("{tip}\nShortcut: {key}")).clicked() {
            picked = Some(mode);
        }
    }
    if let Some(mode) = picked {
        st.set_trim_mode(mode);
    }

    // ---- Markers: a flag on the ruler and a faint line down through the tracks ----
    let lane_painter = painter.with_clip_rect(lanes);
    for mk in &st.seq().markers {
        let x = m.x(mk.at);
        if x < m.x0 - 1.0 || x > lanes.right() + 1.0 {
            continue;
        }
        let [r, g, b] = mk.color.rgb();
        let col = Color32::from_rgb(r, g, b);
        lane_painter.line_segment([Pos2::new(x, ruler.bottom()), Pos2::new(x, rect.bottom())], Stroke::new(1.0, col.gamma_multiply(0.35)));
        let (top, bot) = (ruler.bottom() - 15.0, ruler.bottom() - 1.0);
        let flag = vec![Pos2::new(x - 5.0, top), Pos2::new(x + 5.0, top), Pos2::new(x + 5.0, bot - 5.0), Pos2::new(x, bot), Pos2::new(x - 5.0, bot - 5.0)];
        lane_painter.add(egui::Shape::convex_polygon(flag, col, Stroke::new(1.0, Color32::from_black_alpha(160))));
        if !mk.name.is_empty() && m.pps > 20.0 {
            lane_painter.text(Pos2::new(x + 8.0, top - 1.0), Align2::LEFT_TOP, &mk.name, FontId::proportional(10.0), col);
        }
    }
    if st.timeline.drag.is_none() {
        if let Some(i) = resp.hover_pos().and_then(|p| marker_at(st, p)) {
            let mk = &st.seq().markers[i];
            let tip = format!("{} · {}\nDrag to move · double-click to rename", mk.name, mk.at.timecode(rate));
            resp.clone().on_hover_text_at_pointer(tip);
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
    }

    // ---- Playhead (drawn last, on top) ----
    let px = m.x(st.playback.playhead);
    if px >= m.x0 {
        let red = Color32::from_rgb(235, 64, 52);
        painter.line_segment([Pos2::new(px, rect.top()), Pos2::new(px, rect.bottom())], Stroke::new(1.5, red));
        let tri = vec![Pos2::new(px - 6.0, rect.top()), Pos2::new(px + 6.0, rect.top()), Pos2::new(px, rect.top() + 9.0)];
        painter.add(egui::Shape::convex_polygon(tri, red, Stroke::NONE));
    }

    marker_editor(ui.ctx(), st);

    // Track changes last, once nothing else this frame refers to the old track list.
    match track_action {
        Some(TrackAction::Add(kind)) => {
            st.add_track(kind);
            // Make sure the new track is in view.
            if kind == TrackKind::Audio {
                st.timeline.vscroll = f32::MAX;
            }
        }
        Some(TrackAction::Delete(ti)) => st.delete_track(ti),
        Some(TrackAction::StartRename(ti)) => {
            let name = st.seq().tracks.get(ti).map(|t| t.name.clone()).unwrap_or_default();
            st.timeline.renaming = Some((ti, name, false));
        }
        Some(TrackAction::Rename(ti, name)) => {
            st.timeline.renaming = None;
            st.rename_track(ti, name);
        }
        None => {}
    }

    // Keep the playhead visible while playing.
    if st.playback.playing {
        let right = m.x0 + lanes.width();
        if px > right - 20.0 {
            st.timeline.scroll = (st.playback.playhead.as_secs_f64() as f32) - 20.0 / m.pps;
        }
    }
}

/// Snap a sliding clip so its start or end lands on the edge of a clip that
/// isn't affected by the slide (not its own links or the neighbours it adjusts).
fn snap_slide(st: &EditorState, id: ClipId, before: &[(ClipId, Timing)], delta: Time, m: &Mapping) -> Time {
    let seq = st.seq();
    let group: Vec<ClipId> = seq.link_group(id).iter().map(|(_, c)| c.id).collect();
    let Some(&(_, (start, _, dur))) = before.iter().find(|(c, _)| *c == id) else { return delta };
    let end = start + dur;
    let affected = |&(cid, (s0, _, d0)): &(ClipId, Timing)| group.contains(&cid) || s0 + d0 == start || s0 == end;
    let mut best: Option<(f32, Time)> = None;
    for &(_, (s0, _, d0)) in before.iter().filter(|c| !affected(c)) {
        for target in [s0, s0 + d0] {
            for edge in [start, end] {
                let d = (m.x(edge + delta) - m.x(target)).abs();
                if d <= SNAP_PX && best.is_none_or(|(bd, _)| d < bd) {
                    best = Some((d, target - edge));
                }
            }
        }
    }
    best.map_or(delta, |(_, d)| d)
}

/// Colour for each trim tool: its button and the edge marker.
fn mode_color(mode: TrimMode) -> Color32 {
    match mode {
        TrimMode::Normal => Color32::from_rgb(235, 235, 235),
        TrimMode::Ripple => Color32::from_rgb(255, 196, 64),
        TrimMode::Roll => Color32::from_rgb(90, 210, 255),
        TrimMode::Slip => Color32::from_rgb(190, 140, 255),
        TrimMode::Slide => Color32::from_rgb(120, 230, 140),
    }
}

/// Snap distance in pixels for clip moves.
const SNAP_PX: f32 = 8.0;

/// Snap a dragged clip so its start or end lands on another clip's edge, the
/// playhead, or zero, if one is within `SNAP_PX`. Clips linked to it are ignored.
fn snap_clip(st: &EditorState, id: ClipId, start: Time, m: &Mapping) -> Time {
    let seq = st.seq();
    let Some((_, clip)) = seq.find_clip(id) else { return start };
    let group: Vec<ClipId> = seq.link_group(id).iter().map(|(_, c)| c.id).collect();
    let mut targets = vec![Time::ZERO, st.playback.playhead];
    targets.extend(seq.markers.iter().map(|mk| mk.at));
    for c in seq.tracks.iter().flat_map(|t| &t.clips).filter(|c| !group.contains(&c.id)) {
        targets.push(c.timeline_start);
        targets.push(c.timeline_end());
    }
    let mut best: Option<(f32, Time)> = None;
    for t in targets {
        for (edge, new_start) in [(start, t), (start + clip.duration, Time(t.0 - clip.duration.0))] {
            let d = (m.x(edge) - m.x(t)).abs();
            if d <= SNAP_PX && best.is_none_or(|(bd, _)| d < bd) && new_start >= Time::ZERO {
                best = Some((d, new_start));
            }
        }
    }
    best.map_or(start, |(_, s)| s)
}

/// Snap a trimmed edge to other clips' edges or the start of the timeline. Not
/// the playhead: it follows the edge while trimming. Targets come from the
/// timings before the drag, leaving out clips that move with the edit: the
/// clips being trimmed, everything after a ripple, and both sides of a roll.
#[allow(clippy::too_many_arguments)]
fn snap_point(st: &EditorState, id: ClipId, edge: Edge, edge_at: Time, mode: TrimMode, before: &[(ClipId, Timing)], t: Time, m: &Mapping) -> Time {
    let group: Vec<ClipId> = st.seq().link_group(id).iter().map(|(_, c)| c.id).collect();
    let moves = |&(cid, (start, _, dur)): &(ClipId, Timing)| {
        group.contains(&cid)
            || match mode {
                TrimMode::Normal => false,
                // A ripple start keeps the edge where it was; the rest slides under it.
                TrimMode::Ripple => start >= edge_at || (edge == Edge::Start && start + dur > edge_at),
                TrimMode::Roll => start == edge_at || start + dur == edge_at,
                TrimMode::Slip | TrimMode::Slide => false,
            }
    };
    let mut best: Option<(f32, Time)> = None;
    let targets = before.iter().filter(|c| !moves(c)).flat_map(|&(_, (start, _, dur))| [start, start + dur]).chain([Time::ZERO]);
    for target in targets {
        let d = (m.x(target) - m.x(t)).abs();
        if d <= SNAP_PX && best.is_none_or(|(bd, _)| d < bd) {
            best = Some((d, target));
        }
    }
    best.map_or(t, |(_, x)| x)
}

/// Tile a video clip with thumbnails of the frames under each tile. Returns
/// whether any were drawn.
#[allow(clippy::too_many_arguments)]
fn draw_thumbnails(ctx: &egui::Context, painter: &egui::Painter, project: &reelforge_core::Project, thumbs: &mut crate::thumbs::Thumbnails, clip: &reelforge_core::Clip, cr: Rect, vis: Rect, m: &Mapping, dim: bool) -> bool {
    let Some(media) = project.media(clip.media) else { return false };
    let Some(v) = media.info.video.as_ref() else { return false };
    if !media.path.exists() {
        return false;
    }
    let (path, duration) = (media.path.clone(), media.info.duration);
    let area = Rect::from_min_max(cr.min + Vec2::new(1.0, 1.0), cr.max - Vec2::new(1.0, 1.0));
    let h = area.height();
    if h < 8.0 {
        return false;
    }
    let aspect = (v.width as f32 / v.height.max(1) as f32).clamp(0.25, 4.0);
    let w = (h * aspect).max(8.0);
    let p = painter.with_clip_rect(vis.shrink(1.0));
    // Media covered by one tile (speed included), to pick how finely to sample.
    let span = (w / m.pps) as f64 * clip.speed.abs().max(1e-3) as f64;
    let first = ((vis.left() - area.left()) / w).floor().max(0.0) as i32;
    let last = ((vis.right() - area.left()) / w).ceil() as i32;
    let tint = if dim { Color32::from_gray(110) } else { Color32::from_gray(225) };
    let mut any = false;
    for k in first..last.max(first) {
        let x = area.left() + k as f32 * w;
        if x >= area.right() {
            break;
        }
        // The frame at the middle of the tile (or of what's left of the clip).
        let mid = ((x + w / 2.0).min((x + area.right()) / 2.0) - cr.left()) / m.pps;
        let src = clip.source_in + Time::from_secs_f64(mid.max(0.0) as f64);
        let at = clip.media_time(src).max(Time::ZERO).min(duration);
        if let Some(tex) = thumbs.get(ctx, clip.media, &path, at, span) {
            let tile = Rect::from_min_size(Pos2::new(x, area.top()), Vec2::new(w, h));
            p.image(tex.id(), tile, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), tint);
            any = true;
        }
    }
    if any {
        // Rounded corners like the clip, and a thin line between the pictures and the edge.
        p.rect_stroke(area, 3.0, Stroke::new(1.0, Color32::from_black_alpha(90)), StrokeKind::Inside);
    }
    any
}

fn clip_rect(m: &Mapping, row: Rect, start: Time, dur: Time) -> Rect {
    Rect::from_min_max(Pos2::new(m.x(start), row.top() + 3.0), Pos2::new(m.x(start + dur), row.bottom() - 3.0))
}

/// The small window for naming, recolouring or deleting a marker.
fn marker_editor(ctx: &egui::Context, st: &mut EditorState) {
    let Some(i) = st.timeline.marker_edit else { return };
    let Some(mk) = st.seq().markers.get(i).cloned() else {
        st.timeline.marker_edit = None;
        return;
    };
    let mut open = true;
    let mut close = false;
    let mut delete = false;
    let mut edited = mk.clone();
    egui::Window::new("Marker").id(egui::Id::new("marker_editor")).open(&mut open).collapsible(false).resizable(false).default_pos(ctx.pointer_hover_pos().unwrap_or_default()).show(ctx, |ui| {
        ui.weak(edited.at.timecode(st.seq().frame_rate));
        let r = ui.add(egui::TextEdit::singleline(&mut edited.name).desired_width(200.0).hint_text("Name"));
        if !r.has_focus() && ui.memory(|m| m.focused().is_none()) {
            r.request_focus();
        }
        ui.horizontal(|ui| {
            for c in reelforge_core::MarkerColor::ALL {
                let [r, g, b] = c.rgb();
                let (rect, resp) = ui.allocate_exact_size(Vec2::splat(18.0), Sense::click());
                ui.painter().rect_filled(rect.shrink(2.0), 3.0, Color32::from_rgb(r, g, b));
                if edited.color == c {
                    ui.painter().rect_stroke(rect, 3.0, Stroke::new(1.5, Color32::WHITE), StrokeKind::Inside);
                }
                if resp.clicked() {
                    edited.color = c;
                }
            }
        });
        ui.horizontal(|ui| {
            if ui.button("Delete").clicked() {
                delete = true;
            }
            if ui.button("Done").clicked() || ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                close = true;
            }
        });
    });
    if edited != mk {
        let mut all = st.seq().markers.clone();
        all[i] = edited;
        st.set_markers_live(all);
    }
    if delete {
        st.timeline.marker_edit = None;
        st.remove_marker(i);
    } else if close || !open {
        st.timeline.marker_edit = None;
        st.commit_live_edit();
    }
}
