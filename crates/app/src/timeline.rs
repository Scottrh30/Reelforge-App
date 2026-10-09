//! Custom-painted editor timeline: ruler, tracks, clips, playhead, drag/move.

use crate::app::EditorState;
use egui::{Align2, Color32, CursorIcon, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};
use reelforge_core::undo::{move_group, Edge, Timing};
use reelforge_core::audio::{db_to_gain, fade_curve, MAX_DB, MIN_DB};
use reelforge_core::ClipAudio;
use reelforge_core::{ClipId, MediaId, Time, TrackKind, TrimMode};

const RULER_H: f32 = 26.0;
const TRACK_H: f32 = 52.0;
const HEADER_W: f32 = 96.0;

pub struct TimelineView {
    pub px_per_sec: f32,
    /// Seconds scrolled off the left edge.
    pub scroll: f32,
    /// What dragging a clip edge does: a plain trim, a ripple, or a roll.
    pub mode: TrimMode,
    drag: Option<Drag>,
}

impl Default for TimelineView {
    fn default() -> Self {
        Self { px_per_sec: 60.0, scroll: 0.0, mode: TrimMode::Normal, drag: None }
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

    // ---- Tool buttons in the corner above the track headers ----
    let mut picked = None;
    for (i, (mode, label, key, tip)) in [
        (TrimMode::Normal, "Sel", "V", "Select / trim (V): dragging an edge changes just that clip"),
        (TrimMode::Ripple, "Rip", "B", "Ripple (B): trimming an edge moves every later clip, on all tracks, to keep them butted up"),
        (TrimMode::Roll, "Roll", "N", "Roll (N): drag a cut between two clips; one gets longer, the other shorter, nothing else moves"),
    ]
    .into_iter()
    .enumerate()
    {
        let w = (HEADER_W - 8.0) / 3.0;
        let b = Rect::from_min_size(rect.left_top() + Vec2::new(4.0 + i as f32 * w, 4.0), Vec2::new(w - 2.0, RULER_H - 8.0));
        let bresp = ui.interact(b, egui::Id::new(("trim_tool", i)), Sense::click());
        let on = st.timeline.mode == mode;
        let fill = if on { mode_color(mode) } else { visuals.widgets.inactive.bg_fill };
        painter.rect_filled(b, 3.0, if bresp.hovered() { fill.gamma_multiply(1.25) } else { fill });
        painter.text(b.center(), Align2::CENTER_CENTER, label, FontId::proportional(11.0), if on { Color32::BLACK } else { visuals.text_color() });
        if bresp.on_hover_text(format!("{tip}\nShortcut: {key}")).clicked() {
            picked = Some(mode);
        }
    }
    if let Some(mode) = picked {
        st.set_trim_mode(mode);
    }

    // ---- Tracks and clips ----
    let selected_group: Vec<ClipId> = st.selection.map(|id| st.seq().link_group(id).iter().map(|(_, c)| c.id).collect()).unwrap_or_default();
    let mut mute_clicked = None;
    for (row, &ti) in rows.iter().enumerate() {
        let tr = &st.seq().tracks[ti];
        let r = row_rect(row);
        painter.line_segment([r.left_bottom(), r.right_bottom()], Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color));
        let header = Rect::from_min_size(r.min, Vec2::new(HEADER_W, TRACK_H));
        painter.rect_filled(header, 0.0, visuals.panel_fill);
        let name_color = if tr.muted { visuals.weak_text_color() } else { visuals.text_color() };
        painter.text(header.left_center() + Vec2::new(8.0, -8.0), Align2::LEFT_CENTER, &tr.name, FontId::proportional(13.0), name_color);

        // Mute toggle ("M" for audio, eye for video), HitFilm-style.
        let btn = Rect::from_min_size(header.left_center() + Vec2::new(8.0, 3.0), Vec2::new(22.0, 16.0));
        let label = if tr.kind == TrackKind::Audio { "M" } else { "👁" };
        let bresp = ui.interact(btn, egui::Id::new(("mute", ti)), Sense::click());
        let on = if tr.kind == TrackKind::Audio { tr.muted } else { !tr.muted };
        let fill = if on { Color32::from_rgb(200, 150, 40) } else { visuals.widgets.inactive.bg_fill };
        painter.rect_filled(btn, 3.0, if bresp.hovered() { fill.gamma_multiply(1.3) } else { fill });
        painter.text(btn.center(), Align2::CENTER_CENTER, label, FontId::proportional(11.0), if on { Color32::BLACK } else { visuals.text_color() });
        if bresp.on_hover_text(if tr.kind == TrackKind::Audio { "Mute track" } else { "Show/hide track" }).clicked() {
            mute_clicked = Some(ti);
        }

        for clip in &tr.clips {
            let dragging = matches!(&st.timeline.drag, Some(Drag::Clip { id, .. }) if *id == clip.id);
            let cr = clip_rect(&m, r, clip.timeline_start, clip.duration);
            if !cr.intersect(lanes).is_positive() {
                continue;
            }
            let vis = cr.intersect(lanes);
            let base = if tr.kind == TrackKind::Video { Color32::from_rgb(54, 96, 160) } else { Color32::from_rgb(52, 130, 84) };
            let mut fill = if dragging { base.gamma_multiply(0.35) } else { base };
            if tr.muted {
                fill = fill.gamma_multiply(0.45);
            }
            painter.rect_filled(vis, 4.0, fill);

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
                    let src0 = clip.source_in.as_secs_f64();
                    let color = Color32::from_rgba_unmultiplied(200, 255, 210, 150);
                    let mut x = vis.left().max(cr.left());
                    while x < vis.right() {
                        let t = src0 + ((x - cr.left()) / m.pps) as f64;
                        let i0 = (t * crate::app::WAVEFORM_RATE as f64) as usize;
                        let i1 = ((t + 1.0 / m.pps as f64) * crate::app::WAVEFORM_RATE as f64).ceil() as usize;
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
            let name = st.project.media(clip.media).map(|m| m.name.as_str()).unwrap_or("?");
            let text_painter = painter.with_clip_rect(vis.shrink(2.0));
            text_painter.text(vis.left_top() + Vec2::new(6.0, 4.0), Align2::LEFT_TOP, name, FontId::proportional(12.0), Color32::WHITE);
        }
    }
    if let Some(ti) = mute_clicked {
        st.toggle_mute(ti);
    }

    // Crossfades: a block with an X over the overlap; a red tick where one was
    // asked for but there's no spare media to make it.
    for x in st.project.transitions(0) {
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
        if xr.width() > 34.0 {
            xp.text(xr.center_bottom() - Vec2::new(0.0, 2.0), Align2::CENTER_BOTTOM, format!("{:.1}s", x.len().as_secs_f64()), FontId::proportional(10.0), Color32::WHITE);
        }
    }

    // Ghost of the clip being dragged.
    if let Some(Drag::Clip { id, to, .. }) = &st.timeline.drag {
        if let Some(clip) = st.seq().tracks.iter().flat_map(|t| &t.clips).find(|c| c.id == *id) {
            if let Some(row) = rows.iter().position(|&t| t == to.0) {
                let gr = clip_rect(&m, row_rect(row), to.1, clip.duration).intersect(lanes);
                let ok = move_group(&st.project, 0, *id, *to).is_ok();
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

    // Fade handles sit on the top edge of audio clips, at the end of each fade.
    let hit_fade = |st: &EditorState, p: Pos2| -> Option<(ClipId, bool)> {
        let ti = row_at(p.y)?;
        let tr = &st.seq().tracks[ti];
        if tr.kind != TrackKind::Audio || p.x < m.x0 {
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
        if tr.kind != TrackKind::Audio || p.x < m.x0 {
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
    let hit_edge = |st: &EditorState, p: Pos2| -> Option<(ClipId, Edge)> {
        if p.x < m.x0 {
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
        for c in &tr.clips {
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

    if let Some(p) = resp.hover_pos() {
        if st.timeline.drag.is_none() {
            if hit_fade(st, p).is_none() && hit_edge(st, p).is_some() {
                ui.ctx().set_cursor_icon(CursorIcon::ResizeColumn);
            } else if hit_fade(st, p).is_some() {
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
        if let Some(p) = origin {
            let edge = if hit_fade(st, p).is_none() { hit_edge(st, p) } else { None };
            let vol = if hit_fade(st, p).is_none() && edge.is_none() { hit_vol(st, p) } else { None };
            st.timeline.drag = match (hit_fade(st, p), vol) {
                (Some((id, out)), _) => {
                    st.selection = Some(id);
                    Some(Drag::Fade { id, out })
                }
                (None, _) if edge.is_some() => {
                    let (id, edge) = edge.unwrap();
                    st.selection = Some(id);
                    let c = st.seq().find_clip(id).map(|(_, c)| c.clone()).unwrap();
                    let edge_at = if edge == Edge::Start { c.timeline_start } else { c.timeline_end() };
                    let mode = st.timeline.mode;
                    Some(Drag::Trim { id, edge, edge_at, mode, start: st.all_timings() })
                }
                (None, Some(VolHit::Key(id, index))) => {
                    st.selection = Some(id);
                    Some(Drag::VolKey { id, index })
                }
                (None, Some(VolHit::Line(id))) if !ui.input(|i| i.modifiers.command) => {
                    st.selection = Some(id);
                    let orig = st.clip_audio(id).unwrap_or_default();
                    Some(Drag::VolLine { id, start_y: p.y, orig })
                }
                _ => match hit_clip(st, p) {
                Some((ti, id, start)) => {
                    st.selection = Some(id);
                    Some(Drag::Clip { id, grab: m.t(p.x) - start, from: (ti, start), to: (ti, start) })
                }
                None if p.x >= m.x0 => Some(Drag::Scrub),
                None => None,
                },
            };
        }
    }

    if resp.dragged() {
        if let Some(p) = pointer {
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
                        let what = match plan.mode {
                            TrimMode::Normal => "Trim",
                            TrimMode::Ripple => "Ripple",
                            TrimMode::Roll => "Roll",
                        };
                        let moved = plan.changes.iter().filter(|(_, a, b)| a.2 == b.2 && a.0 != b.0).count();
                        let mut msg = format!(
                            "{what} {}: {:+.2} s  (in {} · out {} · length {})",
                            if edge == Edge::Start { "start" } else { "end" },
                            plan.applied.as_secs_f64(),
                            c.source_in.timecode(rate),
                            (c.source_in + c.duration).timecode(rate),
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
                Drag::Scrub | Drag::Fade { .. } | Drag::VolKey { .. } | Drag::VolLine { .. } | Drag::Trim { .. } => None,
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
        if let Some(Drag::Clip { id, from, to, .. }) = st.timeline.drag.take() {
            if to != from {
                match move_group(&st.project, 0, id, to) {
                    Ok(cmd) => st.execute(cmd),
                    Err(why) => st.status = format!("Can't move there: {why}"),
                }
            }
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
                    st.selection = Some(id);
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
                    st.selection = Some(id);
                    handled = true;
                }
                _ => {}
            }
        }
    }

    if resp.clicked() && !handled {
        if let Some(p) = pointer {
            let on_ruler = p.y < rect.top() + RULER_H;
            match hit_clip(st, p) {
                Some((_, id, _)) => st.selection = Some(id),
                // The ruler only moves the playhead; empty track space also deselects.
                None if p.x >= m.x0 => {
                    if !on_ruler {
                        st.selection = None;
                    }
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

/// Colour for each trim tool: its button and the edge marker.
fn mode_color(mode: TrimMode) -> Color32 {
    match mode {
        TrimMode::Normal => Color32::from_rgb(235, 235, 235),
        TrimMode::Ripple => Color32::from_rgb(255, 196, 64),
        TrimMode::Roll => Color32::from_rgb(90, 210, 255),
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

fn clip_rect(m: &Mapping, row: Rect, start: Time, dur: Time) -> Rect {
    Rect::from_min_max(Pos2::new(m.x(start), row.top() + 3.0), Pos2::new(m.x(start + dur), row.bottom() - 3.0))
}
