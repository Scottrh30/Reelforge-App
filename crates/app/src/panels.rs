use crate::app::EditorState;
use egui::{Align, Color32, Layout, RichText, Sense, Vec2};
use reelforge_core::{MediaId, Time};

pub fn media_bin(ui: &mut egui::Ui, st: &mut EditorState) {
    ui.horizontal(|ui| {
        if ui.button("➕ Import…").clicked() {
            st.import_dialog();
        }
        ui.weak(format!("{} item(s)", st.project.media.len()));
    });
    ui.separator();

    if st.project.media.is_empty() {
        ui.add_space(20.0);
        ui.vertical_centered(|ui| ui.weak("Drop video, image or audio files here"));
        return;
    }

    let mut add: Option<MediaId> = None;
    for m in &st.project.media {
        let id = egui::Id::new(("media", m.id));
        let resp = ui
            .dnd_drag_source(id, m.id, |ui| {
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(RichText::new(&m.name).strong());
                    let mut parts = vec![format_duration(m.info.duration)];
                    if let Some(v) = &m.info.video {
                        parts.push(format!("{}×{}", v.width, v.height));
                        parts.push(format!("{:.3} fps", v.frame_rate.fps()));
                        parts.push(v.codec.clone());
                    }
                    if let Some(a) = &m.info.audio {
                        parts.push(format!("{} Hz {}ch {}", a.sample_rate, a.channels, a.codec));
                    }
                    ui.weak(parts.join(" · "));
                });
            })
            .response;
        if resp.double_clicked() {
            add = Some(m.id);
        }
        resp.on_hover_text("Drag onto the timeline, or double-click to append");
    }
    if let Some(id) = add {
        st.add_to_timeline(id, None, None);
    }
}

pub fn viewer(ui: &mut egui::Ui, st: &mut EditorState) {
    let transport_h = 34.0;
    let avail = ui.available_size() - Vec2::new(0.0, transport_h);
    let (seq_w, seq_h) = (st.seq().width as f32, st.seq().height as f32);

    // Letterbox the sequence frame into the panel.
    let scale = (avail.x / seq_w).min(avail.y / seq_h).max(0.01);
    let frame_size = Vec2::new(seq_w * scale, seq_h * scale);
    let (area, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), avail.y.max(1.0)), Sense::hover());
    let frame_rect = egui::Rect::from_center_size(area.center(), frame_size);
    let painter = ui.painter_at(area);
    painter.rect_filled(area, 0.0, Color32::from_gray(18));
    painter.rect_filled(frame_rect, 0.0, Color32::BLACK);

    if st.viewer_has_frame() {
        if let Some(tex) = &st.viewer_tex {
            // Fit the decoded frame inside the sequence frame (no scaling to fill yet).
            let ts = tex.size_vec2();
            let s = (frame_rect.width() / ts.x).min(frame_rect.height() / ts.y);
            let r = egui::Rect::from_center_size(frame_rect.center(), ts * s);
            painter.image(tex.id(), r, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
        }
    }

    transport(ui, st);
}

fn transport(ui: &mut egui::Ui, st: &mut EditorState) {
    let rate = st.seq().frame_rate;
    let end = st.seq().duration();
    ui.horizontal(|ui| {
        ui.spacing_mut().button_padding = Vec2::new(8.0, 4.0);
        if ui.button("⏮").on_hover_text("Go to start (Home)").clicked() {
            st.playback.seek(Time::ZERO);
        }
        if ui.button("⏴").on_hover_text("Previous frame (←)").clicked() {
            st.playback.step(-1, rate);
        }
        let label = if st.playback.playing { "⏸" } else { "▶" };
        if ui.button(label).on_hover_text("Play / pause (Space)").clicked() {
            st.playback.toggle(end);
        }
        if ui.button("⏵").on_hover_text("Next frame (→)").clicked() {
            st.playback.step(1, rate);
        }
        if ui.button("⏭").on_hover_text("Go to end (End)").clicked() {
            st.playback.seek(end);
        }
        ui.separator();
        ui.label(RichText::new(st.playback.playhead.timecode(rate)).monospace().size(15.0));
        ui.weak(format!("/ {}", end.timecode(rate)));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if !st.viewer_exact {
                ui.spinner();
            }
        });
    });
}

pub fn inspector(ui: &mut egui::Ui, st: &mut EditorState) {
    let rate = st.seq().frame_rate;

    ui.heading("Clip");
    let selected = st.selection.and_then(|id| {
        st.seq().tracks.iter().enumerate().find_map(|(ti, tr)| tr.clips.iter().find(|c| c.id == id).map(|c| (ti, c.clone())))
    });
    match selected {
        Some((ti, clip)) => {
            let media = st.project.media(clip.media);
            egui::Grid::new("clip_props").num_columns(2).spacing([12.0, 4.0]).show(ui, |ui| {
                ui.weak("Media");
                ui.label(media.map(|m| m.name.as_str()).unwrap_or("<missing>"));
                ui.end_row();
                ui.weak("Track");
                ui.label(&st.seq().tracks[ti].name);
                ui.end_row();
                ui.weak("Start");
                ui.monospace(clip.timeline_start.timecode(rate));
                ui.end_row();
                ui.weak("End");
                ui.monospace(clip.timeline_end().timecode(rate));
                ui.end_row();
                ui.weak("Duration");
                ui.monospace(clip.duration.timecode(rate));
                ui.end_row();
                ui.weak("Source in");
                ui.monospace(clip.source_in.timecode(rate));
                ui.end_row();
            });
            ui.add_space(6.0);
            ui.weak("Effects, transforms and keyframes will live here (roadmap phase 4).");
        }
        None => {
            ui.weak("Select a clip on the timeline.");
        }
    }

    ui.add_space(12.0);
    ui.separator();
    ui.heading("Sequence");
    let seq = &mut st.project.sequences[0];
    egui::Grid::new("seq_props").num_columns(2).spacing([12.0, 4.0]).show(ui, |ui| {
        ui.weak("Name");
        ui.text_edit_singleline(&mut seq.name);
        ui.end_row();
        ui.weak("Resolution");
        egui::ComboBox::from_id_salt("res")
            .selected_text(format!("{}×{}", seq.width, seq.height))
            .show_ui(ui, |ui| {
                for (w, h) in [(1280, 720), (1920, 1080), (2560, 1440), (3840, 2160), (1080, 1920), (1080, 1080)] {
                    if ui.selectable_label(seq.width == w && seq.height == h, format!("{w}×{h}")).clicked() {
                        seq.width = w;
                        seq.height = h;
                    }
                }
            });
        ui.end_row();
        ui.weak("Frame rate");
        egui::ComboBox::from_id_salt("fps")
            .selected_text(format!("{:.3}", seq.frame_rate.fps()))
            .show_ui(ui, |ui| {
                use reelforge_core::FrameRate as R;
                for r in [R::new(24000, 1001), R::new(24, 1), R::new(25, 1), R::new(30000, 1001), R::new(30, 1), R::new(50, 1), R::new(60000, 1001), R::new(60, 1)] {
                    if ui.selectable_label(seq.frame_rate == r, format!("{:.3}", r.fps())).clicked() {
                        seq.frame_rate = r;
                    }
                }
            });
        ui.end_row();
    });
}

fn format_duration(t: Time) -> String {
    let s = t.as_secs_f64();
    if s <= 0.0 {
        "still".into()
    } else {
        format!("{}:{:04.1}", (s / 60.0) as u64, s % 60.0)
    }
}
