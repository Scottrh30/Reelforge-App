//! The Plugins window (Edit → Plugins…). The plugin manager itself is in
//! the engine (`reelforge_engine::plugins`), shared with the Tauri app.

use crate::app::EditorState;
use egui::{Color32, RichText};

pub use reelforge_engine::plugins::*;

// ---- The window --------------------------------------------------------------------

pub fn window(ctx: &egui::Context, st: &mut EditorState) {
    if !st.plugins.open {
        return;
    }
    let mut open = true;
    let mut action: Option<Box<dyn FnOnce(&mut EditorState)>> = None;
    egui::Window::new("Plugins").open(&mut open).default_size([520.0, 440.0]).resizable(true).show(ctx, |ui| {
        ui.label("Effect plugins are GPU shaders you can write yourself. They appear in the effects browser (E) with the built-in effects.");
        ui.horizontal_wrapped(|ui| {
            if ui.button("Install from folder…").on_hover_text("A folder with a plugin.toml in it").clicked() {
                action = Some(Box::new(|st| {
                    if let Some(d) = rfd::FileDialog::new().set_title("Plugin folder (with plugin.toml)").pick_folder() {
                        st.status = st.plugins.install_folder(&d).unwrap_or_else(|e| format!("Couldn't install: {e}"));
                    }
                }));
            }
            if ui.button("Install from .zip…").clicked() {
                action = Some(Box::new(|st| {
                    if let Some(z) = rfd::FileDialog::new().add_filter("Plugin zip", &["zip"]).pick_file() {
                        st.status = st.plugins.install_zip(&z).unwrap_or_else(|e| format!("Couldn't install: {e}"));
                    }
                }));
            }
            if ui.button("New plugin…").on_hover_text("Make a starter plugin in your plugin folder and open it, to edit").clicked() {
                action = Some(Box::new(|st| match st.plugins.new_from_template() {
                    Ok(dir) => {
                        crate::settings::open_in_file_manager(&dir);
                        st.status = format!("Made a new plugin in {}: edit effect.wgsl and plugin.toml; changes load as you save", dir.display());
                    }
                    Err(e) => st.status = format!("Couldn't make a plugin: {e}"),
                }));
            }
            if ui.button("Open plugin folder").clicked() {
                let d = user_dir();
                let _ = std::fs::create_dir_all(&d);
                crate::settings::open_in_file_manager(&d);
            }
            if ui.button("Reload").clicked() {
                action = Some(Box::new(|st| st.status = st.plugins.scan()));
            }
        });
        ui.checkbox(&mut st.plugins.live_reload, "Reload automatically when a plugin's files change")
            .on_hover_text("Edit a shader in any text editor and the Viewer updates when you save it");
        ui.separator();
        if st.plugins.entries.is_empty() {
            ui.weak(format!("No plugins yet. Put plugin folders in {}, or use Install / New plugin above.", user_dir().display()));
        }
        egui::ScrollArea::vertical().auto_shrink([false, true]).show(ui, |ui| {
            for e in st.plugins.entries.clone() {
                egui::Frame::group(ui.style()).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        match &e.result {
                            Ok(d) => {
                                ui.label(RichText::new(&d.name).strong());
                                ui.weak(format!("{} · {}", if d.version.is_empty() { "-" } else { &d.version }, d.category.label()));
                                ui.colored_label(Color32::from_rgb(110, 200, 120), "✔ loaded");
                            }
                            Err(_) => {
                                ui.label(RichText::new(e.title()).strong());
                                ui.colored_label(Color32::from_rgb(255, 160, 80), "⚠ not loaded");
                            }
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let removable = st.plugins.removable(&e);
                            if removable {
                                if st.plugins.confirm_remove.as_deref() == Some(e.folder.as_path()) {
                                    if ui.button(RichText::new("Really remove").color(Color32::from_rgb(255, 120, 100))).clicked() {
                                        let f = e.folder.clone();
                                        action = Some(Box::new(move |st| {
                                            st.status = st.plugins.remove(&f).unwrap_or_else(|e| format!("Couldn't remove: {e}"));
                                            st.plugins.confirm_remove = None;
                                        }));
                                    }
                                } else if ui.small_button("Remove").on_hover_text("Delete this plugin's folder").clicked() {
                                    st.plugins.confirm_remove = Some(e.folder.clone());
                                }
                            }
                            if ui.small_button("Show folder").clicked() {
                                crate::settings::open_in_file_manager(&e.folder);
                            }
                        });
                    });
                    match &e.result {
                        Ok(d) => {
                            if !d.description.is_empty() {
                                ui.label(&d.description);
                            }
                            let by = if d.author.is_empty() { String::new() } else { format!("by {} · ", d.author) };
                            ui.weak(format!("{by}id {} · {} parameter{}, {} pass{}", d.id, d.params.len(), if d.params.len() == 1 { "" } else { "s" }, d.passes.len(), if d.passes.len() == 1 { "" } else { "es" }));
                        }
                        Err(msg) => {
                            ui.label(RichText::new(msg).monospace().color(Color32::from_rgb(255, 190, 140)));
                        }
                    }
                    let place = match e.source {
                        Source::User => "your plugin folder",
                        Source::Bundled => "next to Reelforge",
                        Source::Extra => "REELFORGE_PLUGINS",
                    };
                    ui.weak(format!("{} ({place})", e.folder.display()));
                });
            }
        });
    });
    if let Some(a) = action {
        a(st);
    }
    if !open {
        st.plugins.open = false;
        st.plugins.confirm_remove = None;
    }
}

