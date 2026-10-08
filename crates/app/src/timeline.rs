//! Custom-painted editor timeline: ruler, tracks, clips, playhead, drag/move.

use crate::app::EditorState;
use egui::{Align2, Color32, CursorIcon, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};
use reelforge_core::undo::MoveClip;
use reelforge_core::{ClipId, MediaId, Time, TrackKind};

const RULER_H: f32 = 26.0;
const TRACK_H: f32 = 52.0;
const HEADER_W: f32 = 96.0;

pub struct TimelineView {
    pub px_per_sec: f32,
    /// Seconds scrolled off the left edge.
    pub scroll: f32,
    drag: Option<Drag>,
}

impl Default for TimelineView {
    fn default() -> Self {
        Self { px_per_sec: 60.0, scroll: 0.0, drag: None }
    }
}

enum Drag {
    Scrub,
    Clip { id: ClipId, grab: Time, from: (usize, Time), to: (usize, Time) },
}

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

pub fn show(ui: &mut egui::Ui, st: &mut EditorState) {
    let rate = st.seq().frame_rate;
    let rect = ui.available_rect_before_wrap();
    let resp = ui.allocate_rect(rect, Sense::click_and_drag());
    let painter = ui.painter_at(rect);

    // Display order: highest video track at the top, then audio tracks.
    let tracks = &st.seq().tracks;
    let mut rows: Vec<usize> = (0..tracks.len()).filter(|&i| tracks[i].kind == TrackKind::Video).rev().collect();
    rows.extend((0..tracks.len()).filter(|&i| tracks[i].kind == TrackKind::Audio));
    let row_rect = |row: usize| {
        let y = rect.top() + RULER_H + row as f32 * TRACK_H;
        Rect::from_min_size(Pos2::new(rect.left(), y), Vec2::new(rect.width(), TRACK_H))
    };
    let row_at = |y: f32| -> Option<usize> {
        let r = ((y - rect.top() - RULER_H) / TRACK_H).floor();
        (r >= 0.0 && (r as usize) < rows.len()).then(|| rows[r as usize])
    };

    // ---- Zoom / scroll ----
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
        let dx = if scroll.x != 0.0 { scroll.x } else { scroll.y };
        if dx != 0.0 {
            st.timeline.scroll = (st.timeline.scroll - dx / st.timeline.px_per_sec).max(0.0);
        }
    }
    let m = Mapping { x0: rect.left() + HEADER_W, pps: st.timeline.px_per_sec, scroll: st.timeline.scroll };
    let lanes = Rect::from_min_max(Pos2::new(m.x0, rect.top()), rect.max);

    // ---- Background, ruler ----
    let visuals = ui.visuals().clone();
    painter.rect_filled(rect, 0.0, visuals.extreme_bg_color);
    let ruler = Rect::from_min_size(Pos2::new(m.x0, rect.top()), Vec2::new(lanes.width(), RULER_H));
    painter.rect_filled(ruler, 0.0, visuals.faint_bg_color);

    let step = [1.0 / rate.fps() as f32, 0.5, 1.0, 2.0, 5.0, 10.0, 30.0, 60.0, 300.0, 600.0]
        .into_iter()
        .find(|s| s * m.pps >= 80.0)
        .unwrap_or(1200.0);
    let first = (m.scroll / step).floor() as i64;
    let last = ((m.scroll + lanes.width() / m.pps) / step).ceil() as i64;
    for i in first..=last {
        let t = Time::from_secs_f64((i as f32 * step) as f64);
        let x = m.x(t);
        if x < m.x0 {
            continue;
        }
        painter.line_segment([Pos2::new(x, ruler.bottom() - 8.0), Pos2::new(x, ruler.bottom())], Stroke::new(1.0, visuals.weak_text_color()));
        painter.line_segment([Pos2::new(x, ruler.bottom()), Pos2::new(x, rect.bottom())], Stroke::new(1.0, visuals.faint_bg_color));
        painter.text(Pos2::new(x + 3.0, ruler.top() + 3.0), Align2::LEFT_TOP, t.timecode(rate), FontId::monospace(10.0), visuals.weak_text_color());
    }

    // ---- Tracks and clips ----
    for (row, &ti) in rows.iter().enumerate() {
        let tr = &st.seq().tracks[ti];
        let r = row_rect(row);
        painter.line_segment([r.left_bottom(), r.right_bottom()], Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color));
        let header = Rect::from_min_size(r.min, Vec2::new(HEADER_W, TRACK_H));
        painter.rect_filled(header, 0.0, visuals.panel_fill);
        painter.text(header.left_center() + Vec2::new(8.0, 0.0), Align2::LEFT_CENTER, &tr.name, FontId::proportional(13.0), visuals.text_color());

        for clip in &tr.clips {
            let dragging = matches!(&st.timeline.drag, Some(Drag::Clip { id, .. }) if *id == clip.id);
            let cr = clip_rect(&m, r, clip.timeline_start, clip.duration);
            let Some(vis) = cr.intersect(lanes).is_positive().then(|| cr.intersect(lanes)) else { continue };
            let base = if tr.kind == TrackKind::Video { Color32::from_rgb(54, 96, 160) } else { Color32::from_rgb(52, 130, 84) };
            let fill = if dragging { base.gamma_multiply(0.35) } else { base };
            painter.rect_filled(vis, 4.0, fill);
            if st.selection == Some(clip.id) {
                painter.rect_stroke(vis, 4.0, Stroke::new(2.0, Color32::from_rgb(255, 196, 64)), StrokeKind::Inside);
            }
            let name = st.project.media(clip.media).map(|m| m.name.as_str()).unwrap_or("?");
            let text_painter = painter.with_clip_rect(vis.shrink(2.0));
            text_painter.text(vis.left_top() + Vec2::new(6.0, 4.0), Align2::LEFT_TOP, name, FontId::proportional(12.0), Color32::WHITE);
        }
    }

    // Ghost of the clip being dragged.
    if let Some(Drag::Clip { id, to, .. }) = &st.timeline.drag {
        if let Some(clip) = st.seq().tracks.iter().flat_map(|t| &t.clips).find(|c| c.id == *id) {
            if let Some(row) = rows.iter().position(|&t| t == to.0) {
                let gr = clip_rect(&m, row_rect(row), to.1, clip.duration).intersect(lanes);
                let ok = !overlaps(st, to.0, to.1, clip.duration, Some(*id));
                let c = if ok { Color32::from_white_alpha(70) } else { Color32::from_rgba_unmultiplied(220, 60, 60, 90) };
                painter.rect_filled(gr, 4.0, c);
            }
        }
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

    if resp.drag_started() {
        // Hit-test where the button went down, not where the drag threshold was crossed.
        let origin = ui.input(|i| i.pointer.press_origin()).or(pointer);
        if let Some(p) = origin {
            st.timeline.drag = match hit_clip(st, p) {
                Some((ti, id, start)) => {
                    st.selection = Some(id);
                    Some(Drag::Clip { id, grab: m.t(p.x) - start, from: (ti, start), to: (ti, start) })
                }
                None if p.x >= m.x0 => Some(Drag::Scrub),
                None => None,
            };
        }
    }

    if resp.dragged() {
        if let Some(p) = pointer {
            let target = st.timeline.drag.as_ref().map(|d| match d {
                Drag::Scrub => None,
                Drag::Clip { from, grab, .. } => {
                    let kind = st.seq().tracks[from.0].kind;
                    let ti = row_at(p.y).filter(|&t| st.seq().tracks[t].kind == kind).unwrap_or(from.0);
                    let start = Time(m.t(p.x).0 - grab.0).max(Time::ZERO).snap(rate);
                    Some((ti, start))
                }
            });
            match (&mut st.timeline.drag, target) {
                (Some(Drag::Scrub), _) => st.playback.seek(m.t(p.x).snap(rate)),
                (Some(Drag::Clip { to, .. }), Some(Some(t))) => *to = t,
                _ => {}
            }
            ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
        }
    }

    if resp.drag_stopped() {
        if let Some(Drag::Clip { id, from, to, .. }) = st.timeline.drag.take() {
            let dur = st.seq().tracks[from.0].clips.iter().find(|c| c.id == id).map(|c| c.duration).unwrap_or_default();
            if to != from {
                if overlaps(st, to.0, to.1, dur, Some(id)) {
                    st.status = "Can't move there: it would overlap another clip".into();
                } else {
                    st.execute(Box::new(MoveClip { seq: 0, id, from, to }));
                }
            }
        }
        st.timeline.drag = None;
    }

    if resp.clicked() {
        if let Some(p) = pointer {
            match hit_clip(st, p) {
                Some((_, id, _)) => st.selection = Some(id),
                None if p.x >= m.x0 => {
                    st.selection = None;
                    st.playback.seek(m.t(p.x).snap(rate));
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

    // ---- Playhead (drawn last, on top) ----
    let px = m.x(st.playback.playhead);
    if px >= m.x0 {
        let red = Color32::from_rgb(235, 64, 52);
        painter.line_segment([Pos2::new(px, rect.top()), Pos2::new(px, rect.bottom())], Stroke::new(1.5, red));
        let tri = vec![Pos2::new(px - 6.0, rect.top()), Pos2::new(px + 6.0, rect.top()), Pos2::new(px, rect.top() + 9.0)];
        painter.add(egui::Shape::convex_polygon(tri, red, Stroke::NONE));
    }

    // Keep the playhead visible while playing.
    if st.playback.playing {
        let right = m.x0 + lanes.width();
        if px > right - 20.0 {
            st.timeline.scroll = (st.playback.playhead.as_secs_f64() as f32) - 20.0 / m.pps;
        }
    }
}

fn clip_rect(m: &Mapping, row: Rect, start: Time, dur: Time) -> Rect {
    Rect::from_min_max(Pos2::new(m.x(start), row.top() + 3.0), Pos2::new(m.x(start + dur), row.bottom() - 3.0))
}

fn overlaps(st: &EditorState, track: usize, start: Time, dur: Time, ignore: Option<ClipId>) -> bool {
    st.seq().tracks[track]
        .clips
        .iter()
        .filter(|c| Some(c.id) != ignore)
        .any(|c| start < c.timeline_end() && c.timeline_start < start + dur)
}
