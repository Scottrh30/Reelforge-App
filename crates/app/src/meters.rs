//! Drawing the audio meters (the levels themselves are in `reelforge_engine::meters`).

pub use reelforge_engine::meters::*;
use crate::app::EditorState;
use egui::{Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, Vec2};

/// A small horizontal stereo meter (one bar per channel), as in a track header.
pub fn paint_track_meter(painter: &egui::Painter, rect: Rect, ch: &[Channel; 2]) {
    painter.rect_filled(rect, 1.0, Color32::from_gray(22));
    let gap = 1.0;
    let h = ((rect.height() - gap) / 2.0).max(1.0);
    let x_of = |db: f32| rect.left() + y_frac(db) * rect.width();
    for (c, m) in ch.iter().enumerate() {
        let y = rect.top() + c as f32 * (h + gap);
        for (lo, hi) in [(FLOOR_DB, -18.0), (-18.0, -6.0), (-6.0, 0.0)] {
            let top_db = m.peak_db.min(hi);
            if top_db > lo {
                let r = Rect::from_min_max(Pos2::new(x_of(lo), y), Pos2::new(x_of(top_db), y + h));
                painter.rect_filled(r, 0.0, colour(hi - 0.1));
            }
        }
        if m.hold_db > FLOOR_DB {
            let x = x_of(m.hold_db);
            painter.line_segment([Pos2::new(x, y), Pos2::new(x, y + h)], Stroke::new(1.5, colour(m.hold_db)));
        }
    }
    if ch.iter().any(|m| m.clipped) {
        let led = Rect::from_min_max(Pos2::new(rect.right() - 3.0, rect.top()), rect.max);
        painter.rect_filled(led, 0.0, Color32::from_rgb(240, 40, 40));
    }
}

fn colour(db: f32) -> Color32 {
    if db > -6.0 {
        Color32::from_rgb(230, 70, 60)
    } else if db > -18.0 {
        Color32::from_rgb(230, 200, 60)
    } else {
        Color32::from_rgb(70, 200, 100)
    }
}

/// Fraction of the bar height for a level in dB.
fn y_frac(db: f32) -> f32 {
    ((db - FLOOR_DB) / -FLOOR_DB).clamp(0.0, 1.0)
}

pub fn show(ui: &mut egui::Ui, st: &mut EditorState) {
    // Levels are read once a frame by the app (`Meters::poll`), for the track meters too.
    if st.playback.audio.is_none() {
        ui.weak("No audio device");
        return;
    }
    let rect = ui.available_rect_before_wrap();
    let resp = ui.allocate_rect(rect, Sense::click());
    let painter = ui.painter_at(rect);
    let visuals = ui.visuals().clone();
    painter.rect_filled(rect, 0.0, visuals.extreme_bg_color);

    let led_h = 10.0;
    let label_h = 16.0;
    let scale_w = 22.0;
    let top = rect.top() + 6.0 + led_h + 4.0;
    let bottom = rect.bottom() - label_h - 4.0;
    let bars_w = (rect.width() - scale_w - 12.0).max(10.0);
    let bar_w = (bars_w - 4.0) / 2.0;
    let x0 = rect.left() + 6.0;
    let y_of = |db: f32| bottom - y_frac(db) * (bottom - top);

    for (c, m) in st.meters.ch.iter().enumerate() {
        let x = x0 + c as f32 * (bar_w + 4.0);
        let bar = Rect::from_min_max(Pos2::new(x, top), Pos2::new(x + bar_w, bottom));
        painter.rect_filled(bar, 1.0, Color32::from_gray(28));
        // Peak (dim) and RMS (bright) bars, each coloured by zone: green, yellow above -18, red above -6.
        for (level, inset, strength) in [(m.peak_db, 0.0, 0.5), (m.rms_db, 2.0, 1.0)] {
            for (lo, hi) in [(FLOOR_DB, -18.0), (-18.0, -6.0), (-6.0, 0.0)] {
                let top_db = level.min(hi);
                if top_db > lo {
                    let r = Rect::from_min_max(Pos2::new(x + inset, y_of(top_db)), Pos2::new(x + bar_w - inset, y_of(lo)));
                    painter.rect_filled(r, 0.0, colour(hi - 0.1).gamma_multiply(strength));
                }
            }
        }
        if m.hold_db > FLOOR_DB {
            let y = y_of(m.hold_db);
            painter.line_segment([Pos2::new(x, y), Pos2::new(x + bar_w, y)], Stroke::new(2.0, colour(m.hold_db)));
        }
        // Clip light: latches until clicked.
        let led = Rect::from_min_size(Pos2::new(x, rect.top() + 6.0), Vec2::new(bar_w, led_h));
        painter.rect_filled(led, 2.0, if m.clipped { Color32::from_rgb(240, 40, 40) } else { Color32::from_gray(45) });
        painter.text(
            Pos2::new(x + bar_w / 2.0, bottom + 3.0),
            Align2::CENTER_TOP,
            if m.hold_db > FLOOR_DB { format!("{:.0}", m.hold_db) } else { "-∞".into() },
            FontId::monospace(10.0),
            visuals.weak_text_color(),
        );
    }

    // dB scale.
    let sx = x0 + 2.0 * bar_w + 8.0;
    for db in [0.0, -6.0, -12.0, -18.0, -24.0, -36.0, -48.0, -60.0] {
        let y = y_of(db);
        painter.line_segment([Pos2::new(sx - 3.0, y), Pos2::new(sx, y)], Stroke::new(1.0, visuals.weak_text_color()));
        painter.text(Pos2::new(sx + 2.0, y), Align2::LEFT_CENTER, format!("{}", db as i32), FontId::monospace(9.0), visuals.weak_text_color());
    }

    if resp.clicked() {
        st.meters.reset_clips();
    }
    resp.on_hover_text("Master output. Bars: peak (dim) and RMS (bright); line: peak hold. Click to reset the clip lights.");

    if st.meters.any_active() {
        ui.ctx().request_repaint();
    }
}
