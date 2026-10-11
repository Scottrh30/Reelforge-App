//! The Paste attributes dialog (Edit → Paste attributes…, Ctrl+Alt+V): pick
//! which parts of the copied clip go onto the selected clips.

use crate::app::EditorState;
use reelforge_core::attrs::{AttrSet, Source};

pub fn window(ctx: &egui::Context, st: &mut EditorState, open: &mut bool) {
    let cb = st.clipboard.clone();
    let src = Source::of(&cb);
    let offers = src.offers();
    let targets = st.selected_ids();
    let from = src.video.or(src.audio).map(|c| c.display_name(&st.project).into_owned()).unwrap_or_default();
    let mut set = st.paste_attrs;
    let mut paste = false;
    egui::Window::new("Paste attributes").open(open).collapsible(false).resizable(false).show(ctx, |ui| {
        ui.label(format!("From: {from}"));
        let n = targets.len();
        if n == 0 {
            ui.colored_label(egui::Color32::from_rgb(255, 170, 80), "Select the clips to paste onto (in the timeline).");
        } else {
            ui.label(format!("To: {n} selected clip{}", if n == 1 { "" } else { "s" }));
        }
        ui.add_space(6.0);
        let visual = offers.visual;
        let row = |ui: &mut egui::Ui, on: &mut bool, enabled: bool, label: &str, tip: &str| {
            if !enabled {
                *on = false;
            }
            ui.add_enabled(enabled, egui::Checkbox::new(on, label)).on_hover_text(tip).on_disabled_hover_text("The copied clip doesn't have this");
        };
        row(ui, &mut set.transform, visual, "Position, scale, rotation, anchor and opacity", "With their keyframes, at the same distance from the clip's start");
        row(ui, &mut set.crop, visual, "Crop", "The crop amounts (and keyframes)");
        row(ui, &mut set.corner_pin, offers.corner_pin, "Corner pin", "Where the four corners go (and keyframes)");
        row(ui, &mut set.blend, visual, "Blend mode", "Normal, Add, Multiply or Screen");
        let fx = offers.effects;
        row(ui, &mut set.effects, fx > 0, &format!("Effects ({fx})"), "The whole effect stack, with keyframes: the same colour grade on every shot, say");
        if set.effects {
            ui.indent("fx_mode", |ui| {
                ui.radio_value(&mut set.replace_effects, true, "Replace their effects");
                ui.radio_value(&mut set.replace_effects, false, "Add after their effects");
            });
        }
        let masks = offers.masks;
        row(ui, &mut set.masks, masks > 0, &format!("Masks ({masks})"), "Replaces their masks; shapes are scaled to each clip's picture size");
        row(ui, &mut set.audio, offers.audio, "Audio levels: volume, pan and fades", "Onto audio clips (including those linked to the selected video clips), with volume keyframes");
        row(ui, &mut set.text_style, offers.text_style, "Text style", "Onto titles: font, size, colour, outline, shadow and box; each keeps its own text");
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.small_button("All").clicked() {
                set = AttrSet {
                    transform: visual,
                    crop: visual,
                    corner_pin: offers.corner_pin,
                    blend: visual,
                    effects: fx > 0,
                    replace_effects: set.replace_effects,
                    masks: masks > 0,
                    audio: offers.audio,
                    text_style: offers.text_style,
                };
            }
            if ui.small_button("None").clicked() {
                set = AttrSet { transform: false, crop: false, corner_pin: false, blend: false, effects: false, masks: false, audio: false, text_style: false, ..set };
            }
        });
        ui.separator();
        ui.horizontal(|ui| {
            let ok = n > 0 && set.any();
            if ui.add_enabled(ok, egui::Button::new(egui::RichText::new("Paste").strong())).clicked() {
                paste = true;
            }
            if ui.button("Cancel").clicked() {
                st.paste_attrs_cancel = true;
            }
        });
    });
    st.paste_attrs = set;
    if std::mem::take(&mut st.paste_attrs_cancel) {
        *open = false;
    }
    if paste {
        st.paste_attributes(set);
        *open = false;
    }
}
