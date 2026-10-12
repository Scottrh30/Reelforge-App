//! App settings (Edit → Settings…), remembered between runs.

use crate::app::EditorState;
use reelforge_media::{hw, proxy, FrameCache};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Windows turned off in the Tabs menu.
    pub hidden_tabs: Vec<crate::app::Tab>,
    /// Decode video on the GPU when possible.
    pub hw_decode: bool,
    /// Show proxies (instead of the original files) in the viewer when they exist.
    pub use_proxies: bool,
    /// Make proxies automatically for heavy footage (above 1080p, or 720p+ HEVC/AV1/…).
    pub auto_proxies: bool,
    /// Memory for keeping decoded frames, in MB.
    pub cache_mb: usize,
    /// Minutes between autosaves (0 = off).
    pub autosave_minutes: u32,
    /// Show picture thumbnails on video clips in the timeline.
    pub timeline_thumbnails: bool,
    /// Layers dragged in the Viewer snap to the frame and to each other.
    pub viewer_snapping: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self { hidden_tabs: Vec::new(), hw_decode: true, use_proxies: true, auto_proxies: true, cache_mb: reelforge_media::cache::DEFAULT_BUDGET_MB, autosave_minutes: 2, timeline_thumbnails: true, viewer_snapping: true }
    }
}

impl Settings {
    pub const STORAGE_KEY: &'static str = "reelforge_settings";

    /// Push the settings into the parts of the engine that use them.
    pub fn apply(&self) {
        hw::set_enabled(self.hw_decode);
        FrameCache::global().set_budget_mb(self.cache_mb);
    }
}

fn folder_size(dir: &std::path::Path) -> (u64, usize) {
    let Ok(rd) = std::fs::read_dir(dir) else { return (0, 0) };
    rd.flatten().filter_map(|e| e.metadata().ok()).filter(|m| m.is_file()).fold((0, 0), |(b, n), m| (b + m.len(), n + 1))
}

/// The settings window. `open` is cleared when it's closed.
pub fn window(ctx: &egui::Context, st: &mut EditorState, open: &mut bool) {
    let before = st.settings.clone();
    let mut clear_proxies = false;
    egui::Window::new("Settings").open(open).collapsible(false).resizable(false).show(ctx, |ui| {
        let s = &mut st.settings;
        ui.heading("Playback");
        ui.checkbox(&mut s.hw_decode, format!("Hardware video decoding ({})", hw::api_name()))
            .on_hover_text("Decode on the graphics card. Falls back to software automatically if it doesn't work. Applies to clips opened from now on.");
        ui.weak(format!("Status: {}", hw::status()));
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label("Frame cache");
            ui.add(egui::Slider::new(&mut s.cache_mb, 128..=16384).logarithmic(true).suffix(" MB"));
        });
        let (used, frames, hit) = FrameCache::global().stats();
        ui.weak(format!("Holding {frames} frames ({} MB) · {:.0}% of requests served from memory", used >> 20, hit * 100.0));
        ui.add_space(4.0);
        ui.checkbox(&mut s.timeline_thumbnails, "Picture thumbnails on timeline clips")
            .on_hover_text("Small frames along each video clip. They're decoded in the background (from proxies when there are some).");
        ui.checkbox(&mut s.viewer_snapping, "Snap layers in the Viewer")
            .on_hover_text("A layer you drag lines up with the frame's centre and edges and with other layers, showing guide lines. Hold Alt while dragging to place it freely.");

        ui.add_space(8.0);
        ui.heading("Proxies");
        ui.checkbox(&mut s.use_proxies, "Use proxies in the viewer").on_hover_text("Small all-keyframe copies of heavy footage. Export always uses the original files.");
        ui.checkbox(&mut s.auto_proxies, "Make proxies automatically for heavy footage")
            .on_hover_text("Above 1080p, and 720p or more in HEVC, AV1, VP9, ProRes or DNxHD. You can make one for any clip from the Media panel.");
        let dir = proxy::cache_dir().join("proxies");
        let (bytes, n) = folder_size(&dir);
        ui.horizontal(|ui| {
            let size = if bytes >= 1_000_000_000 { format!("{:.1} GB", bytes as f64 / 1e9) } else { format!("{:.0} MB", bytes as f64 / 1e6) };
            ui.weak(format!("{n} prox{}, {size}, in {}", if n == 1 { "y" } else { "ies" }, dir.display()));
        });
        ui.horizontal(|ui| {
            if ui.button("Open folder").clicked() {
                let _ = std::fs::create_dir_all(&dir);
                open_in_file_manager(&dir);
            }
            if ui.add_enabled(n > 0, egui::Button::new("Delete all proxies")).clicked() {
                clear_proxies = true;
            }
        });

        ui.add_space(8.0);
        ui.heading("Safety");
        ui.horizontal(|ui| {
            ui.label("Autosave every");
            ui.add(egui::DragValue::new(&mut s.autosave_minutes).range(0..=60).suffix(" min"));
            if s.autosave_minutes == 0 {
                ui.colored_label(ui.visuals().warn_fg_color, "off");
            }
        });
        let adir = crate::safety::autosave_dir();
        ui.weak(format!("Autosaves are kept in {}", adir.display()));
        if ui.button("Open autosave folder").clicked() {
            let _ = std::fs::create_dir_all(&adir);
            open_in_file_manager(&adir);
        }
    });
    if st.settings != before {
        st.settings.apply();
    }
    if clear_proxies {
        st.proxies.delete_all();
        st.status = "Deleted all proxies".into();
    }
}

/// Open the folder holding `file`, with the file selected where the system can.
pub fn reveal_in_file_manager(file: &std::path::Path) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // Explorer wants exactly /select,"path" (its own quoting rules).
        let _ = std::process::Command::new("explorer").raw_arg(format!("/select,\"{}\"", file.display())).spawn();
    }
    #[cfg(not(windows))]
    if let Some(dir) = file.parent() {
        open_in_file_manager(dir);
    }
}

pub fn open_in_file_manager(dir: &std::path::Path) {
    let cmd = if cfg!(windows) {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let _ = std::process::Command::new(cmd).arg(dir).spawn();
}
