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
    let frame_px = [st.seq().width as f32, st.seq().height as f32];

    // Letterbox the sequence frame into the panel.
    let scale = (avail.x / frame_px[0]).min(avail.y / frame_px[1]).max(0.01);
    let (area, resp) = ui.allocate_exact_size(Vec2::new(ui.available_width(), avail.y.max(1.0)), Sense::click_and_drag());
    let frame_rect = egui::Rect::from_center_size(area.center(), Vec2::new(frame_px[0], frame_px[1]) * scale);
    let painter = ui.painter_at(area);
    painter.rect_filled(area, 0.0, Color32::from_gray(18));
    painter.rect_filled(frame_rect, 0.0, Color32::BLACK);
    if let Some(tex) = st.preview.texture() {
        painter.image(tex, frame_rect, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
    }

    // Screen <-> frame pixel mapping.
    let to_screen = |[x, y]: [f32; 2]| frame_rect.min + Vec2::new(x, y) * scale;
    let to_frame = |p: egui::Pos2| {
        let v = (p - frame_rect.min) / scale;
        [v.x, v.y]
    };

    // Visible layers at the playhead, top first, with their native sizes.
    let t = st.playback.playhead;
    let layers: Vec<(reelforge_core::ClipId, [f32; 2], reelforge_core::Transform)> = st
        .seq()
        .video_layers_at(t)
        .into_iter()
        .rev()
        .filter_map(|(_, c)| {
            let v = st.project.media(c.media)?.info.video.as_ref()?;
            Some((c.id, [v.width as f32, v.height as f32], c.transform))
        })
        .collect();

    // Click a layer in the viewer to select it (topmost hit wins), like HitFilm.
    if resp.clicked() || resp.drag_started() {
        if let Some(p) = ui.input(|i| i.pointer.press_origin()).or(resp.interact_pointer_pos()) {
            let f = to_frame(p);
            let hit = layers.iter().find(|(_, size, tr)| tr.hit(*size, frame_px, f)).map(|(id, ..)| *id);
            let keep = st.selection.is_some_and(|sel| layers.iter().any(|(id, size, tr)| *id == sel && tr.hit(*size, frame_px, f)));
            if !keep {
                st.selection = hit;
            }
        }
    }

    // Drag the selected layer to move it.
    let selected = st.selection.and_then(|id| layers.iter().find(|(lid, ..)| *lid == id).copied());
    if let Some((id, size, tr)) = selected {
        if resp.dragged() && !st.playback.playing {
            let d = resp.drag_delta() / scale;
            if d != Vec2::ZERO {
                if let Some((mut t2, blend)) = st.visual(id) {
                    t2.position[0] += d.x;
                    t2.position[1] += d.y;
                    st.set_visual_live(id, (t2, blend));
                }
            }
            ui.ctx().set_cursor_icon(egui::CursorIcon::Move);
        }
        // Bounding box + centre handle.
        let corners = st.visual(id).map(|(t2, _)| t2).unwrap_or(tr).corners(size, frame_px).map(to_screen);
        let gold = Color32::from_rgb(255, 196, 64);
        let clip_painter = ui.painter_at(area);
        clip_painter.add(egui::Shape::closed_line(corners.to_vec(), egui::Stroke::new(1.5, gold)));
        for c in corners {
            clip_painter.rect_filled(egui::Rect::from_center_size(c, Vec2::splat(6.0)), 1.0, gold);
        }
        let centre = egui::pos2((corners[0].x + corners[2].x) / 2.0, (corners[0].y + corners[2].y) / 2.0);
        clip_painter.circle_stroke(centre, 5.0, egui::Stroke::new(1.5, gold));
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
            if !st.preview.exact {
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
            if st.seq().tracks[ti].kind == reelforge_core::TrackKind::Video {
                ui.add_space(10.0);
                transform_controls(ui, st, &clip);
            }
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

/// Position / scale / rotation / opacity / blend for a video clip.
/// Edits apply live and become one undo step when the mouse is released.
fn transform_controls(ui: &mut egui::Ui, st: &mut EditorState, clip: &reelforge_core::Clip) {
    use reelforge_core::{BlendMode, Transform};
    let Some((mut t, mut blend)) = st.visual(clip.id) else { return };
    let before = (t, blend);
    let native = st
        .project
        .media(clip.media)
        .and_then(|m| m.info.video.as_ref())
        .map(|v| [v.width as f32, v.height as f32])
        .unwrap_or([1.0, 1.0]);
    let frame = [st.seq().width as f32, st.seq().height as f32];
    let lock_id = egui::Id::new("scale_lock");
    let mut lock = ui.data_mut(|d| *d.get_persisted_mut_or(lock_id, true));

    ui.heading("Transform");
    egui::Grid::new("transform").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        ui.weak("Position");
        ui.horizontal(|ui| {
            ui.add(egui::DragValue::new(&mut t.position[0]).speed(1.0).prefix("X ").fixed_decimals(1));
            ui.add(egui::DragValue::new(&mut t.position[1]).speed(1.0).prefix("Y ").fixed_decimals(1));
        });
        ui.end_row();

        ui.weak("Scale");
        ui.horizontal(|ui| {
            let mut sx = t.scale[0] * 100.0;
            let mut sy = t.scale[1] * 100.0;
            let rx = ui.add(egui::DragValue::new(&mut sx).speed(0.5).suffix(" %").range(-10000.0..=10000.0).fixed_decimals(1));
            let ry = ui.add(egui::DragValue::new(&mut sy).speed(0.5).suffix(" %").range(-10000.0..=10000.0).fixed_decimals(1));
            if lock {
                if rx.changed() {
                    sy = sx;
                } else if ry.changed() {
                    sx = sy;
                }
            }
            t.scale = [sx / 100.0, sy / 100.0];
            ui.toggle_value(&mut lock, "🔒").on_hover_text("Keep X and Y scale equal");
        });
        ui.end_row();

        ui.weak("Rotation");
        ui.add(egui::DragValue::new(&mut t.rotation).speed(0.5).suffix("°").fixed_decimals(1));
        ui.end_row();

        ui.weak("Opacity");
        let mut o = t.opacity * 100.0;
        ui.add(egui::Slider::new(&mut o, 0.0..=100.0).suffix(" %").fixed_decimals(0));
        t.opacity = o / 100.0;
        ui.end_row();

        ui.weak("Blend");
        egui::ComboBox::from_id_salt("blend").selected_text(blend.label()).show_ui(ui, |ui| {
            for m in BlendMode::ALL {
                ui.selectable_value(&mut blend, m, m.label());
            }
        });
        ui.end_row();
    });
    ui.horizontal(|ui| {
        if ui.button("Reset").clicked() {
            t = Transform::default();
        }
        if ui.button("Fit").on_hover_text("Scale to fit inside the frame").clicked() {
            let k = Transform::fit_scale(native, frame, false);
            t.scale = [k, k];
            t.position = [0.0, 0.0];
        }
        if ui.button("Fill").on_hover_text("Scale to cover the whole frame").clicked() {
            let k = Transform::fit_scale(native, frame, true);
            t.scale = [k, k];
            t.position = [0.0, 0.0];
        }
    });
    ui.data_mut(|d| d.insert_persisted(lock_id, lock));

    if (t, blend) != before {
        st.set_visual_live(clip.id, (t, blend));
    }
}

fn format_duration(t: Time) -> String {
    let s = t.as_secs_f64();
    if s <= 0.0 {
        "still".into()
    } else {
        format!("{}:{:04.1}", (s / 60.0) as u64, s % 60.0)
    }
}
