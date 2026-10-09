//! Master audio meters: peak and RMS per channel with standard ballistics
//! (instant attack, 20 dB/s fall, 1.5 s peak hold) and latching clip lights.

use crate::app::EditorState;
use egui::{Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, Vec2};
use std::time::{Duration, Instant};

pub const FLOOR_DB: f32 = -60.0;
const FALL_DB_PER_SEC: f32 = 20.0;
const HOLD: Duration = Duration::from_millis(1500);
/// A sample at or above this counts as clipped (the mixer limits output to ±1).
const CLIP: f32 = 0.999;

fn to_db(x: f32) -> f32 {
    if x <= 0.0 { FLOOR_DB } else { (20.0 * x.log10()).max(FLOOR_DB) }
}

#[derive(Clone, Copy)]
pub struct Channel {
    pub peak_db: f32,
    pub rms_db: f32,
    pub hold_db: f32,
    hold_until: Option<Instant>,
    pub clipped: bool,
}

impl Default for Channel {
    fn default() -> Self {
        Self { peak_db: FLOOR_DB, rms_db: FLOOR_DB, hold_db: FLOOR_DB, hold_until: None, clipped: false }
    }
}

impl Channel {
    /// Feed the latest linear peak/RMS, `dt` seconds after the previous update.
    pub fn update(&mut self, peak: f32, rms: f32, dt: f32, now: Instant) {
        let fall = FALL_DB_PER_SEC * dt;
        self.peak_db = to_db(peak).max(self.peak_db - fall);
        self.rms_db = to_db(rms).max(self.rms_db - fall);
        if peak >= CLIP {
            self.clipped = true;
        }
        let p = to_db(peak);
        if p >= self.hold_db || self.hold_until.is_none_or(|t| now > t) {
            if p >= self.hold_db {
                self.hold_db = p;
                self.hold_until = Some(now + HOLD);
            } else {
                // Hold expired: let the marker fall with the bar.
                self.hold_db = (self.hold_db - fall).max(self.peak_db);
            }
        }
    }

    pub fn active(&self) -> bool {
        self.peak_db > FLOOR_DB || self.hold_db > FLOOR_DB
    }
}

pub struct Meters {
    pub ch: [Channel; 2],
    last: Instant,
}

impl Default for Meters {
    fn default() -> Self {
        Self { ch: [Channel::default(); 2], last: Instant::now() }
    }
}

impl Meters {
    pub fn update(&mut self, peaks: [f32; 2], rms: [f32; 2]) {
        let now = Instant::now();
        let dt = (now - self.last).as_secs_f32().min(0.25);
        self.last = now;
        for c in 0..2 {
            self.ch[c].update(peaks[c], rms[c], dt, now);
        }
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
    if let Some(a) = &st.playback.audio {
        let (p, r) = a.take_levels();
        st.meters.update(p, r);
    } else {
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
        for c in &mut st.meters.ch {
            c.clipped = false;
        }
    }
    resp.on_hover_text("Master output. Bars: peak (dim) and RMS (bright); line: peak hold. Click to reset the clip lights.");

    if st.meters.ch.iter().any(Channel::active) {
        ui.ctx().request_repaint();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attack_is_instant_and_release_is_20_db_per_second() {
        let t0 = Instant::now();
        let mut c = Channel::default();
        c.update(0.5, 0.35, 0.016, t0);
        assert!((c.peak_db - -6.02).abs() < 0.05);
        // Silence for 0.5 s: falls 10 dB.
        c.update(0.0, 0.0, 0.5, t0 + Duration::from_millis(500));
        assert!((c.peak_db - -16.02).abs() < 0.05, "{}", c.peak_db);
    }

    #[test]
    fn peak_hold_lasts_then_falls() {
        let t0 = Instant::now();
        let mut c = Channel::default();
        c.update(1.0, 0.7, 0.016, t0);
        assert!(c.clipped, "full scale lights the clip LED");
        c.update(0.1, 0.07, 1.0, t0 + Duration::from_secs(1));
        assert!(c.hold_db.abs() < 0.01, "still holding 0 dB after 1 s");
        c.update(0.1, 0.07, 1.0, t0 + Duration::from_secs(2));
        assert!(c.hold_db < -10.0, "hold released after 1.5 s: {}", c.hold_db);
        assert!(c.clipped, "clip light latches");
    }

    #[test]
    fn silence_reads_as_floor() {
        let mut c = Channel::default();
        c.update(0.0, 0.0, 0.016, Instant::now());
        assert!(!c.active());
    }
}
