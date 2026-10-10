use crate::app::EditorState;
use egui::{Align, Color32, Layout, RichText, Sense, Vec2};
use reelforge_core::{ClipVisual, Prop, Transform};
use reelforge_core::{MediaId, Time};

pub fn media_bin(ui: &mut egui::Ui, st: &mut EditorState) {
    ui.horizontal(|ui| {
        if ui.button("➕ Import…").clicked() {
            st.import_dialog();
        }
        if ui.button("T  New title").on_hover_text("Add a text title at the playhead (Ctrl+T)").clicked() {
            st.add_title();
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
    // Each as the layer-local rectangle that counts as "the layer": the whole
    // picture for footage, just the text (and its box) for a title.
    let layers: Vec<(reelforge_core::ClipId, [f32; 4], reelforge_core::Transform)> = st
        .project
        .video_layers_at(0, t)
        .iter()
        .rev()
        .flat_map(|item| item.layers().into_iter().rev().map(|l| (l.clip, l.transform)).collect::<Vec<_>>())
        .filter_map(|(c, tr)| {
            if c.title.is_some() {
                let b = st.preview.title_bounds(c.id)?;
                let (hw, hh) = (frame_px[0] / 2.0, frame_px[1] / 2.0);
                return Some((c.id, [b[0] - hw, b[1] - hh, b[2] - hw, b[3] - hh], tr));
            }
            let v = st.project.media(c.media)?.info.video.as_ref()?;
            Some((c.id, Transform::full_rect([v.width as f32, v.height as f32]), tr))
        })
        .collect();

    // Click a layer in the viewer to select it (topmost hit wins), like HitFilm.
    if resp.clicked() || resp.drag_started() {
        if let Some(p) = ui.input(|i| i.pointer.press_origin()).or(resp.interact_pointer_pos()) {
            let f = to_frame(p);
            let hit = layers.iter().find(|(_, r, tr)| tr.hit_rect(*r, frame_px, f)).map(|(id, ..)| *id);
            let keep = st.selection.is_some_and(|sel| layers.iter().any(|(id, r, tr)| *id == sel && tr.hit_rect(*r, frame_px, f)));
            if !keep {
                st.selection = hit;
            }
        }
    }

    // Drag the selected layer to move it.
    let selected = st.selection.and_then(|id| layers.iter().find(|(lid, ..)| *lid == id).copied());
    if let Some((id, lrect, tr)) = selected {
        if resp.dragged() && !st.playback.playing {
            let d = resp.drag_delta() / scale;
            if d != Vec2::ZERO {
                let clip = st.seq().find_clip(id).map(|(_, c)| c.clone());
                if let (Some(mut v), Some(clip)) = (st.visual(id), clip) {
                    let shown = clip.transform_at(t);
                    let mut moved = shown;
                    moved.position[0] += d.x;
                    moved.position[1] += d.y;
                    // Keyed if position is animated (adds/updates a key at the playhead).
                    apply_transform_edit(&mut v, &shown, &moved, clip.source_time(t), clip.contains(t));
                    st.set_visual_live(id, v);
                }
            }
            ui.ctx().set_cursor_icon(egui::CursorIcon::Move);
        }
        // Bounding box + centre handle.
        let now = st.seq().find_clip(id).map(|(_, c)| c.transform_at(t)).unwrap_or(tr);
        let corners = now.rect_corners(lrect, frame_px).map(to_screen);
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
            let locked = st.seq().is_locked(clip.id);
            let media = st.project.media(clip.media);
            egui::Grid::new("clip_props").num_columns(2).spacing([12.0, 4.0]).show(ui, |ui| {
                ui.weak("Media");
                if clip.title.is_some() {
                    ui.label("Title (generated)");
                } else {
                    ui.label(media.map(|m| m.name.as_str()).unwrap_or("<missing>"));
                }
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
            if locked {
                ui.add_space(6.0);
                ui.colored_label(ui.visuals().warn_fg_color, "On a locked track: unlock it to make changes.");
            }
            ui.add_enabled_ui(!locked, |ui| {
                if clip.title.is_some() {
                    ui.add_space(10.0);
                    title_controls(ui, st, &clip);
                }
                if st.seq().tracks[ti].kind == reelforge_core::TrackKind::Video {
                    ui.add_space(10.0);
                    transform_controls(ui, st, &clip);
                    dissolve_control(ui, st, &clip);
                    ui.add_space(10.0);
                    effect_controls(ui, st, &clip);
                }
            });
            // Audio controls for this clip, or for the audio linked to a selected video clip.
            let audio_clip = st
                .seq()
                .link_group(clip.id)
                .into_iter()
                .find(|(t, _)| st.seq().tracks[*t].kind == reelforge_core::TrackKind::Audio)
                .map(|(t, c)| (t, c.clone()));
            if let Some((ati, aclip)) = audio_clip {
                ui.add_space(10.0);
                ui.add_enabled_ui(!locked, |ui| clip_audio_controls(ui, st, &aclip));
                ui.add_space(10.0);
                track_audio_controls(ui, st, ati);
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

/// Write an edit made to the *displayed* (evaluated) transform back to the clip:
/// animated properties get a keyframe at source time `src` (only if `can_key`,
/// i.e. the playhead is on the clip); static ones change the clip's base value.
pub fn apply_transform_edit(v: &mut ClipVisual, shown: &Transform, edited: &Transform, src: Time, can_key: bool) {
    for p in Prop::ALL {
        let changed = match p {
            Prop::Position => shown.position != edited.position,
            Prop::Scale => shown.scale != edited.scale,
            Prop::Rotation => shown.rotation != edited.rotation,
            Prop::Opacity => shown.opacity != edited.opacity,
        };
        if !changed {
            continue;
        }
        if v.anim.prop_animated(p) {
            if can_key {
                v.anim.set_key(p, src, edited);
            }
        } else {
            match p {
                Prop::Position => v.transform.position = edited.position,
                Prop::Scale => v.transform.scale = edited.scale,
                Prop::Rotation => v.transform.rotation = edited.rotation,
                Prop::Opacity => v.transform.opacity = edited.opacity,
            }
        }
    }
}

/// Position / scale / rotation / opacity / blend for a video clip, each with a
/// keyframe button. Edits apply live and become one undo step on release.
fn transform_controls(ui: &mut egui::Ui, st: &mut EditorState, clip: &reelforge_core::Clip) {
    use reelforge_core::{BlendMode, Interp};
    let Some(mut v) = st.visual(clip.id) else { return };
    let before = v.clone();
    let playhead = st.playback.playhead;
    let in_clip = clip.contains(playhead);
    let src = clip.source_time(playhead.max(clip.timeline_start).min(clip.timeline_end()));
    let to_timeline = |at: Time| clip.timeline_start + (at - clip.source_in);
    let shown = v.anim.eval(&v.transform, src);
    let mut t = shown;
    let native = st
        .project
        .media(clip.media)
        .and_then(|m| m.info.video.as_ref())
        .map(|v| [v.width as f32, v.height as f32])
        .unwrap_or([1.0, 1.0]);
    let frame = [st.seq().width as f32, st.seq().height as f32];
    let lock_id = egui::Id::new("scale_lock");
    let mut lock = ui.data_mut(|d| *d.get_persisted_mut_or(lock_id, true));
    let mut toggles: Vec<Prop> = Vec::new();
    let mut reset = false;
    let mut seek = None;
    let anim = v.anim.clone();
    // A keyframed property can only be edited with the playhead on the clip.
    let editable = |p: Prop| !anim.prop_animated(p) || in_clip;
    let key_btn = |ui: &mut egui::Ui, p: Prop, toggles: &mut Vec<Prop>| {
        let here = in_clip && anim.has_key(p, src);
        let tip = if here { "Remove this keyframe" } else if anim.prop_animated(p) { "Add a keyframe at the playhead" } else { "Start animating: add a keyframe at the playhead" };
        if key_button(ui, here, in_clip).on_hover_text(tip).clicked() {
            toggles.push(p);
        }
    };

    ui.heading("Transform");
    egui::Grid::new("transform").num_columns(3).spacing([8.0, 6.0]).show(ui, |ui| {
        ui.weak("Position");
        key_btn(ui, Prop::Position, &mut toggles);
        ui.add_enabled_ui(editable(Prop::Position), |ui| {
            ui.horizontal(|ui| {
                ui.add(egui::DragValue::new(&mut t.position[0]).speed(1.0).prefix("X ").fixed_decimals(1));
                ui.add(egui::DragValue::new(&mut t.position[1]).speed(1.0).prefix("Y ").fixed_decimals(1));
            });
        });
        ui.end_row();

        ui.weak("Scale");
        key_btn(ui, Prop::Scale, &mut toggles);
        ui.add_enabled_ui(editable(Prop::Scale), |ui| {
            ui.horizontal(|ui| {
                let mut sx = t.scale[0] * 100.0;
                let mut sy = t.scale[1] * 100.0;
                let rx = ui.add(egui::DragValue::new(&mut sx).speed(0.5).suffix(" %").range(-10000.0..=10000.0).fixed_decimals(1));
                let ry = ui.add(egui::DragValue::new(&mut sy).speed(0.5).suffix(" %").range(-10000.0..=10000.0).fixed_decimals(1));
                // Write back only on a real change: x*100/100 isn't always exact in f32,
                // and a phantom change would create an undo step every frame.
                if rx.changed() {
                    t.scale[0] = sx / 100.0;
                    if lock {
                        t.scale[1] = t.scale[0];
                    }
                } else if ry.changed() {
                    t.scale[1] = sy / 100.0;
                    if lock {
                        t.scale[0] = t.scale[1];
                    }
                }
                ui.toggle_value(&mut lock, "🔒").on_hover_text("Keep X and Y scale equal");
            });
        });
        ui.end_row();

        ui.weak("Rotation");
        key_btn(ui, Prop::Rotation, &mut toggles);
        ui.add_enabled_ui(editable(Prop::Rotation), |ui| {
            ui.add(egui::DragValue::new(&mut t.rotation).speed(0.5).suffix("°").fixed_decimals(1));
        });
        ui.end_row();

        ui.weak("Opacity");
        key_btn(ui, Prop::Opacity, &mut toggles);
        ui.add_enabled_ui(editable(Prop::Opacity), |ui| {
            let mut o = t.opacity * 100.0;
            if ui.add(egui::Slider::new(&mut o, 0.0..=100.0).suffix(" %").fixed_decimals(0)).changed() {
                t.opacity = o / 100.0;
            }
        });
        ui.end_row();

        ui.weak("Blend");
        ui.label("");
        egui::ComboBox::from_id_salt("blend").selected_text(v.blend.label()).show_ui(ui, |ui| {
            for m in BlendMode::ALL {
                ui.selectable_value(&mut v.blend, m, m.label());
            }
        });
        ui.end_row();

        // Keyframe navigation and interpolation.
        let times: Vec<Time> = v.anim.key_times().into_iter().map(to_timeline).collect();
        if !times.is_empty() {
            ui.weak("Keyframes");
            ui.label("");
            ui.horizontal(|ui| {
                let prev = times.iter().rev().copied().find(|k| *k < playhead);
                let next = times.iter().copied().find(|k| *k > playhead);
                if ui.add_enabled(prev.is_some(), egui::Button::new("◀")).on_hover_text("Previous keyframe").clicked() {
                    seek = prev;
                }
                if ui.add_enabled(next.is_some(), egui::Button::new("▶")).on_hover_text("Next keyframe").clicked() {
                    seek = next;
                }
                if let Some(mut interp) = v.anim.interp_at(src).filter(|_| in_clip) {
                    let old = interp;
                    egui::ComboBox::from_id_salt("interp").selected_text(interp.label()).width(70.0).show_ui(ui, |ui| {
                        for i in Interp::ALL {
                            ui.selectable_value(&mut interp, i, i.label());
                        }
                    })
                    .response
                    .on_hover_text("How the keyframe(s) here move towards the next one");
                    if interp != old {
                        v.anim.set_interp_at(src, interp);
                    }
                } else {
                    ui.weak(format!("{} keyframe time(s)", times.len()));
                }
            });
            ui.end_row();
        }
    });
    ui.horizontal(|ui| {
        if ui.button("Reset").on_hover_text("Default transform; removes transform keyframes").clicked() {
            reset = true;
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

    if reset {
        v.transform = Transform::default();
        v.anim = Default::default();
    } else {
        apply_transform_edit(&mut v, &shown, &t, src, in_clip);
        let now = v.anim.eval(&v.transform, src);
        for p in toggles {
            if v.anim.has_key(p, src) {
                // Removing the last key: keep the current value so nothing jumps.
                let last = match p {
                    Prop::Position => v.anim.position.0.len() == 1,
                    Prop::Scale => v.anim.scale.0.len() == 1,
                    Prop::Rotation => v.anim.rotation.0.len() == 1,
                    Prop::Opacity => v.anim.opacity.0.len() == 1,
                };
                if last {
                    let mut base = v.transform;
                    let cur = now;
                    match p {
                        Prop::Position => base.position = cur.position,
                        Prop::Scale => base.scale = cur.scale,
                        Prop::Rotation => base.rotation = cur.rotation,
                        Prop::Opacity => base.opacity = cur.opacity,
                    }
                    v.transform = base;
                }
                v.anim.remove_key(p, src);
            } else {
                v.anim.set_key(p, src, &now);
            }
        }
    }
    if v != before {
        st.set_visual_live(clip.id, v);
    }
    if let Some(t) = seek {
        st.playback.seek(t);
    }
}

/// An sRGBA colour swatch button; writes back only on a real change (the
/// round trip through egui's premultiplied colour isn't always exact).
fn color_button(ui: &mut egui::Ui, c: &mut [u8; 4]) -> bool {
    let mut col = Color32::from_rgba_unmultiplied(c[0], c[1], c[2], c[3]);
    let changed = egui::widgets::color_picker::color_edit_button_srgba(ui, &mut col, egui::widgets::color_picker::Alpha::OnlyBlend).changed();
    if changed {
        *c = col.to_srgba_unmultiplied();
    }
    changed
}

/// Text, font and styling of a title clip. Edits show immediately; a typing
/// session or a slider drag is one undo step.
fn title_controls(ui: &mut egui::Ui, st: &mut EditorState, clip: &reelforge_core::Clip) {
    use reelforge_core::TextAlign;
    let Some(before) = clip.title.clone() else { return };
    let mut t = before.clone();
    let fonts = st.fonts();

    ui.heading("Text");
    ui.add(egui::TextEdit::multiline(&mut t.text).desired_rows(3).desired_width(f32::INFINITY).hint_text("Type the title…"));

    egui::Grid::new("title_style").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
        ui.weak("Font");
        ui.horizontal(|ui| {
            let filter_id = egui::Id::new("font_filter");
            let mut filter: String = ui.data_mut(|d| d.get_temp(filter_id).unwrap_or_default());
            egui::ComboBox::from_id_salt("title_font").width(190.0).selected_text(t.font.name.clone()).height(360.0).show_ui(ui, |ui| {
                ui.add(egui::TextEdit::singleline(&mut filter).hint_text("Search fonts…").desired_width(170.0)).request_focus();
                let f = filter.to_lowercase();
                if "built-in".contains(&f) || f.is_empty() {
                    let b = reelforge_core::FontChoice::builtin();
                    if ui.selectable_label(t.font.path.is_none(), &b.name).clicked() {
                        t.font = b;
                    }
                }
                match &fonts {
                    Some(list) => {
                        for e in list.iter().filter(|e| f.is_empty() || e.name().to_lowercase().contains(&f)).take(400) {
                            let on = t.font.path.as_ref() == Some(&e.path) && t.font.index == e.index;
                            if ui.selectable_label(on, e.name()).clicked() {
                                t.font = e.choice();
                            }
                        }
                    }
                    None => {
                        ui.weak("Looking for installed fonts…");
                    }
                }
            });
            ui.data_mut(|d| d.insert_temp(filter_id, filter));
        });
        ui.end_row();
        if st.preview.title_missing_font(clip.id) {
            ui.label("");
            ui.colored_label(ui.visuals().warn_fg_color, "Font file not found: using the built-in font");
            ui.end_row();
        }

        ui.weak("Size");
        ui.horizontal(|ui| {
            ui.add(egui::DragValue::new(&mut t.size).speed(0.5).range(4.0..=2000.0).suffix(" px").fixed_decimals(0));
            color_button(ui, &mut t.color);
        });
        ui.end_row();

        ui.weak("Align");
        ui.horizontal(|ui| {
            for a in TextAlign::ALL {
                ui.selectable_value(&mut t.align, a, a.label());
            }
        });
        ui.end_row();

        ui.weak("Spacing");
        ui.horizontal(|ui| {
            ui.add(egui::DragValue::new(&mut t.line_spacing).speed(0.01).range(0.5..=4.0).prefix("lines ×").fixed_decimals(2));
            ui.add(egui::DragValue::new(&mut t.letter_spacing).speed(0.1).range(-50.0..=200.0).prefix("letters ").suffix(" px").fixed_decimals(1));
        });
        ui.end_row();

        ui.weak("Outline");
        ui.horizontal(|ui| {
            ui.add(egui::DragValue::new(&mut t.outline).speed(0.1).range(0.0..=100.0).suffix(" px").fixed_decimals(1));
            color_button(ui, &mut t.outline_color);
        });
        ui.end_row();

        ui.weak("Shadow");
        ui.horizontal(|ui| {
            ui.checkbox(&mut t.shadow, "");
            ui.add_enabled_ui(t.shadow, |ui| {
                ui.add(egui::DragValue::new(&mut t.shadow_distance).speed(0.2).range(0.0..=500.0).suffix(" px").fixed_decimals(0))
                    .on_hover_text("Distance");
                ui.add(egui::DragValue::new(&mut t.shadow_angle).speed(1.0).range(-360.0..=360.0).suffix("°").fixed_decimals(0))
                    .on_hover_text("Direction (0° = right, 90° = down)");
                ui.add(egui::DragValue::new(&mut t.shadow_softness).speed(0.2).range(0.0..=200.0).prefix("soft ").fixed_decimals(0));
                color_button(ui, &mut t.shadow_color);
            });
        });
        ui.end_row();

        ui.weak("Box");
        ui.horizontal(|ui| {
            ui.checkbox(&mut t.background, "").on_hover_text("A filled rectangle behind the text");
            ui.add_enabled_ui(t.background, |ui| {
                ui.add(egui::DragValue::new(&mut t.background_padding).speed(0.2).range(0.0..=500.0).prefix("padding ").suffix(" px").fixed_decimals(0));
                color_button(ui, &mut t.background_color);
            });
        });
        ui.end_row();
    });

    // Quick placement presets (they set the clip's position).
    let mut place: Option<[f32; 2]> = None;
    ui.horizontal(|ui| {
        ui.weak("Place:");
        if ui.button("Centre").clicked() {
            place = Some([0.0, 0.0]);
        }
        let frame = [st.seq().width as f32, st.seq().height as f32];
        let bounds = st.preview.title_bounds(clip.id);
        if ui.add_enabled(bounds.is_some(), egui::Button::new("Lower third")).on_hover_text("Bottom left, inside the title-safe area").clicked() {
            if let Some(b) = bounds {
                // Left edge at 10% of the width, bottom edge at 85% of the height.
                place = Some([frame[0] * 0.10 - b[0], frame[1] * 0.85 - b[3]]);
            }
        }
        if ui.add_enabled(bounds.is_some(), egui::Button::new("Top")).clicked() {
            if let Some(b) = bounds {
                place = Some([0.0, frame[1] * 0.10 - b[1]]);
            }
        }
    });

    if t != before {
        st.set_title_live(clip.id, t);
    }
    if let Some(pos) = place {
        if let Some(mut v) = st.visual(clip.id) {
            let shown = clip.transform_at(st.playback.playhead);
            let mut moved = shown;
            moved.position = pos;
            let src = clip.source_time(st.playback.playhead.max(clip.timeline_start).min(clip.timeline_end()));
            apply_transform_edit(&mut v, &shown, &moved, src, clip.contains(st.playback.playhead));
            st.set_visual_live(clip.id, v);
        }
    }
}

/// The clip's effect stack: add, remove, reorder, enable, and edit each
/// parameter (with keyframes, like the transform). One undo step per gesture.
fn effect_controls(ui: &mut egui::Ui, st: &mut EditorState, clip: &reelforge_core::Clip) {
    use reelforge_core::{Effect, EffectKind};
    let Some(mut v) = st.visual(clip.id) else { return };
    let before = v.clone();
    let playhead = st.playback.playhead;
    let in_clip = clip.contains(playhead);
    let src = clip.source_time(playhead.max(clip.timeline_start).min(clip.timeline_end()));
    enum Act {
        Up,
        Down,
        Remove,
    }
    let mut act: Option<(usize, Act)> = None;

    ui.horizontal(|ui| {
        ui.heading("Effects");
        ui.menu_button("Add effect…", |ui| {
            for kind in EffectKind::ALL {
                if ui.button(kind.label()).clicked() {
                    v.effects.push(Effect::new(kind));
                    ui.close();
                }
            }
        });
    });
    if v.effects.is_empty() {
        ui.weak("None. Effects are applied to the clip before its transform, top of the list first.");
    }
    let n = v.effects.len();
    for (i, e) in v.effects.iter_mut().enumerate() {
        e.normalize();
        ui.push_id(("effect", i), |ui| {
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.checkbox(&mut e.enabled, "").on_hover_text("Turn the effect on or off");
                    ui.strong(e.kind.label());
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.small_button("Remove").clicked() {
                            act = Some((i, Act::Remove));
                        }
                        if arrow_button(ui, false, i + 1 < n).on_hover_text("Move down (applied later)").clicked() {
                            act = Some((i, Act::Down));
                        }
                        if arrow_button(ui, true, i > 0).on_hover_text("Move up (applied earlier)").clicked() {
                            act = Some((i, Act::Up));
                        }
                    });
                });
                ui.add_enabled_ui(e.enabled, |ui| {
                    egui::Grid::new("params").num_columns(3).spacing([8.0, 4.0]).show(ui, |ui| {
                        for (pi, spec) in e.kind.params().iter().enumerate() {
                            let animated = e.params[pi].keys.is_animated();
                            let here = in_clip && e.params[pi].keys.index_at(src).is_some();
                            ui.weak(spec.name);
                            let tip = if here { "Remove this keyframe" } else if animated { "Add a keyframe at the playhead" } else { "Start animating: add a keyframe at the playhead" };
                            let toggle = key_button(ui, here, in_clip).on_hover_text(tip).clicked();
                            let shown = e.value_at(pi, src);
                            let mut val = shown;
                            let changed = ui
                                .add_enabled_ui(!animated || in_clip, |ui| {
                                    ui.horizontal(|ui| {
                                        let r = ui.add(
                                            egui::DragValue::new(&mut val)
                                                .speed(spec.speed)
                                                .range(spec.min..=spec.max)
                                                .suffix(spec.suffix)
                                                .fixed_decimals(if spec.speed < 0.1 { 2 } else { 1 }),
                                        );
                                        if e.kind == EffectKind::ChromaKey && pi == 0 {
                                            // Swatch of the key colour.
                                            let (rect, _) = ui.allocate_exact_size(Vec2::splat(14.0), Sense::hover());
                                            let c = egui::ecolor::Hsva::new(val / 360.0, 1.0, 1.0, 1.0);
                                            ui.painter().rect_filled(rect, 3.0, Color32::from(c));
                                        }
                                        r.changed()
                                    })
                                    .inner
                                })
                                .inner;
                            let p = &mut e.params[pi];
                            if changed {
                                if animated {
                                    p.keys.set(src, val);
                                } else {
                                    p.value = val;
                                }
                            }
                            if toggle {
                                if here {
                                    if p.keys.0.len() == 1 {
                                        p.value = shown; // last key gone: keep the value it had
                                    }
                                    p.keys.remove_at(src);
                                } else {
                                    p.keys.set(src, shown);
                                }
                            }
                            ui.end_row();
                        }
                    });
                });
            });
        });
    }
    match act {
        Some((i, Act::Up)) if i > 0 => v.effects.swap(i, i - 1),
        Some((i, Act::Down)) if i + 1 < v.effects.len() => v.effects.swap(i, i + 1),
        Some((i, Act::Remove)) => {
            v.effects.remove(i);
        }
        _ => {}
    }
    if v != before {
        st.set_visual_live(clip.id, v);
    }
}

/// "Dissolve in" length for a video clip that touches the one before it.
fn dissolve_control(ui: &mut egui::Ui, st: &mut EditorState, clip: &reelforge_core::Clip) {
    let Some(left_dur) = st.project.clip_before(0, clip.id).and_then(|id| st.seq().find_clip(id).map(|(_, c)| c.duration)) else { return };
    let rate = st.seq().frame_rate;
    let x = st.project.crossfade_into(0, clip.id);
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.weak("Dissolve in");
        let mut secs = clip.dissolve_in.as_secs_f64();
        let limit = left_dur.min(clip.duration).as_secs_f64();
        if ui
            .add(egui::DragValue::new(&mut secs).speed(0.01).range(0.0..=limit).suffix(" s").fixed_decimals(2))
            .on_hover_text("Cross-dissolve from the clip before this one (Ctrl+D)")
            .changed()
        {
            st.set_dissolve_live(clip.id, Time::from_secs_f64(secs).snap(rate));
        }
        if let Some(x) = x.filter(|x| x.len() < x.requested) {
            let msg = if x.is_empty() { "no spare media: plain cut".to_string() } else { format!("only {:.2} s of spare media", x.len().as_secs_f64()) };
            ui.colored_label(ui.visuals().warn_fg_color, msg)
                .on_hover_text("A dissolve shows the outgoing clip past its end and the incoming one before its start. Trim the clips to leave unused media at the cut.");
        }
    });
}

/// A small button with a painted up/down triangle (the font has no arrow glyphs).
fn arrow_button(ui: &mut egui::Ui, up: bool, enabled: bool) -> egui::Response {
    let size = Vec2::new(20.0, ui.spacing().interact_size.y - 4.0);
    let (rect, resp) = ui.allocate_exact_size(size, if enabled { Sense::click() } else { Sense::hover() });
    let visuals = if enabled { ui.style().interact(&resp) } else { &ui.style().visuals.widgets.noninteractive };
    ui.painter().rect(rect, visuals.corner_radius, visuals.weak_bg_fill, visuals.bg_stroke, egui::StrokeKind::Inside);
    let c = rect.center();
    let (dy, w) = (if up { -3.0 } else { 3.0 }, 4.5);
    let pts = vec![c + Vec2::new(0.0, dy), c + Vec2::new(w, -dy), c + Vec2::new(-w, -dy)];
    let col = if enabled { visuals.fg_stroke.color } else { ui.visuals().weak_text_color() };
    ui.painter().add(egui::Shape::convex_polygon(pts, col, egui::Stroke::NONE));
    resp
}

/// A button showing a keyframe diamond: filled when there's a key at the playhead.
/// (Painted rather than a text glyph, which the default font doesn't have.)
fn key_button(ui: &mut egui::Ui, filled: bool, enabled: bool) -> egui::Response {
    let size = Vec2::new(24.0, ui.spacing().interact_size.y);
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(size, sense);
    let visuals = if enabled { ui.style().interact(&resp) } else { &ui.style().visuals.widgets.noninteractive };
    ui.painter().rect(rect, visuals.corner_radius, visuals.weak_bg_fill, visuals.bg_stroke, egui::StrokeKind::Inside);
    let c = rect.center();
    let r = 5.0;
    let pts = vec![c + Vec2::new(0.0, -r), c + Vec2::new(r, 0.0), c + Vec2::new(0.0, r), c + Vec2::new(-r, 0.0)];
    let gold = Color32::from_rgb(255, 210, 80);
    let col = if !enabled { ui.visuals().weak_text_color() } else if filled { gold } else { visuals.fg_stroke.color };
    let fill = if filled && enabled { gold } else { Color32::TRANSPARENT };
    ui.painter().add(egui::Shape::convex_polygon(pts, fill, egui::Stroke::new(1.3, col)));
    resp
}

/// Volume slider in dB where the bottom of the range means silent.
fn volume_slider(ui: &mut egui::Ui, db: &mut f32) -> egui::Response {
    use reelforge_core::audio::{MAX_DB, MIN_DB};
    ui.add(
        egui::Slider::new(db, MIN_DB..=MAX_DB)
            .fixed_decimals(1)
            .custom_formatter(|v, _| if v <= MIN_DB as f64 { "-∞ dB".into() } else { format!("{v:+.1} dB") })
            .custom_parser(|s| {
                let t = s.trim().trim_end_matches("dB").trim();
                if t.starts_with("-∞") || t.eq_ignore_ascii_case("-inf") { Some(MIN_DB as f64) } else { t.parse().ok() }
            }),
    )
    .on_hover_text("Double-click the number to type a value")
}

/// Pan slider, -100 (left) .. +100 (right).
fn pan_slider(ui: &mut egui::Ui, pan: &mut f32) {
    let mut p = *pan * 100.0;
    let r = ui.add(egui::Slider::new(&mut p, -100.0..=100.0).fixed_decimals(0).custom_formatter(|v, _| match v.round() as i32 {
        0 => "C".into(),
        n if n < 0 => format!("L{}", -n),
        n => format!("R{n}"),
    }));
    if r.double_clicked() {
        *pan = 0.0;
    } else if r.changed() {
        *pan = p / 100.0;
    }
    r.on_hover_text("Double-click to centre");
}

fn clip_audio_controls(ui: &mut egui::Ui, st: &mut EditorState, clip: &reelforge_core::Clip) {
    let Some(mut a) = st.clip_audio(clip.id) else { return };
    let before = a.clone();
    let rate = st.seq().frame_rate;
    let playhead = st.playback.playhead;
    let in_clip = clip.contains(playhead);
    let src = clip.source_time(playhead.max(clip.timeline_start).min(clip.timeline_end()));
    let to_timeline = |at: Time| clip.timeline_start + (at - clip.source_in);
    let mut seek = None;
    // Crossfade with the clip that ends where this one starts, if any.
    let before_clip = st.project.clip_before(0, clip.id).and_then(|id| st.seq().find_clip(id).map(|(_, c)| c.duration));
    let xfade = st.project.crossfade_into(0, clip.id);

    ui.heading("Audio");
    egui::Grid::new("clip_audio").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        ui.weak("Volume");
        if a.is_animated() {
            // With keyframes, the slider shows/edits the value at the playhead (adding a key there).
            let mut db = a.volume_db_at(src);
            let r = ui.add_enabled_ui(in_clip, |ui| volume_slider(ui, &mut db)).inner;
            if r.changed() && in_clip {
                a.set_key(src, db);
            }
        } else {
            volume_slider(ui, &mut a.volume_db);
        }
        ui.end_row();

        ui.weak("Keyframes");
        ui.horizontal(|ui| {
            let prev = a.volume_keys.iter().rev().map(|k| to_timeline(k.at)).find(|t| *t < playhead);
            let next = a.volume_keys.iter().map(|k| to_timeline(k.at)).find(|t| *t > playhead);
            if ui.add_enabled(prev.is_some(), egui::Button::new("◀")).on_hover_text("Previous keyframe").clicked() {
                seek = prev;
            }
            let here = a.key_index_at(src).filter(|_| in_clip);
            let tip = if here.is_some() { "Remove the keyframe at the playhead" } else { "Add a volume keyframe at the playhead" };
            if key_button(ui, here.is_some(), in_clip).on_hover_text(tip).clicked() {
                match here {
                    Some(i) => {
                        a.volume_keys.remove(i);
                    }
                    None => {
                        let db = a.volume_db_at(src);
                        a.set_key(src, db);
                    }
                }
            }
            if ui.add_enabled(next.is_some(), egui::Button::new("▶")).on_hover_text("Next keyframe").clicked() {
                seek = next;
            }
            if a.is_animated() {
                ui.weak(format!("{}", a.volume_keys.len()));
                if ui.small_button("Clear").on_hover_text("Remove all keyframes, keeping the level at the playhead").clicked() {
                    a.volume_db = a.volume_db_at(src);
                    a.volume_keys.clear();
                }
            }
        });
        ui.end_row();
        ui.weak("Pan");
        pan_slider(ui, &mut a.pan);
        ui.end_row();
        let max = clip.duration.as_secs_f64();
        for (label, value, other) in [("Fade in", &mut a.fade_in, before.fade_out), ("Fade out", &mut a.fade_out, before.fade_in)] {
            ui.weak(label);
            let mut secs = value.as_secs_f64();
            let limit = (max - other.as_secs_f64()).max(0.0);
            // Only write back on a real change: a float round trip mustn't create phantom edits.
            if ui.add(egui::DragValue::new(&mut secs).speed(0.01).range(0.0..=limit).suffix(" s").fixed_decimals(2)).changed() {
                *value = Time::from_secs_f64(secs).snap(rate);
            }
            ui.end_row();
        }

        if let Some(left_dur) = before_clip {
            ui.weak("Crossfade in");
            ui.horizontal(|ui| {
                let mut secs = a.crossfade_in.as_secs_f64();
                let limit = left_dur.min(clip.duration).as_secs_f64();
                if ui
                    .add(egui::DragValue::new(&mut secs).speed(0.01).range(0.0..=limit).suffix(" s").fixed_decimals(2))
                    .on_hover_text("Equal-power crossfade with the clip before this one (Ctrl+Shift+D)")
                    .changed()
                {
                    a.crossfade_in = Time::from_secs_f64(secs).snap(rate);
                }
                if let Some(x) = xfade.filter(|x| x.len() < x.requested) {
                    let msg = if x.is_empty() { "no spare media: plain cut".to_string() } else { format!("only {:.2} s of spare media", x.len().as_secs_f64()) };
                    ui.colored_label(ui.visuals().warn_fg_color, msg).on_hover_text(
                        "A crossfade plays the outgoing clip past its end and the incoming one before its start. Trim the clips to leave unused media at the cut.",
                    );
                }
            });
            ui.end_row();
        }
    });
    if ui.button("Reset audio").clicked() {
        a = reelforge_core::ClipAudio::default();
    }
    if a != before {
        st.set_clip_audio_live(clip.id, a);
    }
    if let Some(t) = seek {
        st.playback.seek(t);
    }
}

fn track_audio_controls(ui: &mut egui::Ui, st: &mut EditorState, track: usize) {
    let tr = &st.seq().tracks[track];
    let mut a = tr.audio;
    let before = a;
    ui.heading(format!("Track: {}", tr.name));
    egui::Grid::new("track_audio").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
        ui.weak("Volume");
        volume_slider(ui, &mut a.volume_db);
        ui.end_row();
        ui.weak("Pan");
        pan_slider(ui, &mut a.pan);
        ui.end_row();
    });
    if a != before {
        st.set_track_audio_live(track, a);
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
