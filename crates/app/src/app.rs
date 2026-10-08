use crate::playback::Playback;
use crate::timeline::TimelineView;
use egui::{Key, KeyboardShortcut, Modifiers, TextureHandle, TextureOptions};
use egui_dock::{DockArea, DockState, NodeIndex, Style};
use reelforge_core::undo::{AddClip, ImportMedia, RemoveClip, SplitClip};
use reelforge_core::*;
use reelforge_media::FrameSource;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Preview frames are decoded at most at this size; the export path (later)
/// will decode at full resolution.
const PREVIEW_MAX: (u32, u32) = (1920, 1080);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tab {
    MediaBin,
    Viewer,
    Inspector,
    Timeline,
}

pub struct ReelforgeApp {
    dock: DockState<Tab>,
    state: EditorState,
}

/// Everything the panels read and edit. Separate from the dock layout so
/// `DockArea` can borrow the layout while panels borrow this mutably.
pub struct EditorState {
    pub project: Project,
    pub undo: UndoStack,
    pub playback: Playback,
    pub selection: Option<ClipId>,
    pub timeline: TimelineView,
    pub status: String,
    pub project_path: Option<PathBuf>,
    sources: HashMap<MediaId, Result<FrameSource, String>>,
    pub viewer_tex: Option<TextureHandle>,
    viewer_key: Option<(MediaId, Time, u64)>,
    /// Whether the viewer currently shows the exact frame for the playhead.
    pub viewer_exact: bool,
}

impl ReelforgeApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());

        // HitFilm-like layout: media bin left, viewer centre, inspector right, timeline across the bottom.
        let mut dock = DockState::new(vec![Tab::Viewer]);
        let tree = dock.main_surface_mut();
        // `fraction` is the share kept by the original node (the viewer).
        let [top, _timeline] = tree.split_below(NodeIndex::root(), 0.6, vec![Tab::Timeline]);
        let [viewer, _bin] = tree.split_left(top, 0.18, vec![Tab::MediaBin]);
        tree.split_right(viewer, 0.76, vec![Tab::Inspector]);

        let mut app = Self {
            dock,
            state: EditorState {
                project: Project::new(),
                undo: UndoStack::default(),
                playback: Playback::default(),
                selection: None,
                timeline: TimelineView::default(),
                status: "Import media with File → Import, or drop files onto the window.".into(),
                project_path: None,
                sources: HashMap::new(),
                viewer_tex: None,
                viewer_key: None,
                viewer_exact: true,
            },
        };

        // `reelforge clip1.mp4 clip2.mov …` imports the files and lays them out on the timeline.
        let args: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
        if !args.is_empty() {
            app.state.import(&args);
            let ids: Vec<MediaId> = app.state.project.media.iter().map(|m| m.id).collect();
            for id in ids {
                app.state.add_to_timeline(id, None, None);
            }
        }
        app
    }
}

impl eframe::App for ReelforgeApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let st = &mut self.state;

        // Files dropped from Explorer.
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()).collect());
        if !dropped.is_empty() {
            st.import(&dropped);
        }

        st.handle_shortcuts(&ctx);

        let seq = &st.project.sequences[0];
        let (rate, end) = (seq.frame_rate, seq.duration());
        st.playback.tick(rate, end);
        st.update_viewer_frame(&ctx);

        egui::Panel::top("menu").show(ui, |ui| st.menu_bar(ui));
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(&st.status);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let seq = &st.project.sequences[0];
                    ui.weak(format!("{}×{} @ {:.3} fps", seq.width, seq.height, seq.frame_rate.fps()));
                });
            });
        });
        egui::CentralPanel::default().show(ui, |ui| {
            DockArea::new(&mut self.dock)
                .style(Style::from_egui(ui.style().as_ref()))
                .show_close_buttons(false)
                .show_leaf_close_all_buttons(false)
                .show_leaf_collapse_buttons(false)
                .show_inside(ui, st);
        });

        // Keep animating while playing or while waiting on a seek to land.
        if st.playback.playing {
            ctx.request_repaint();
        } else if !st.viewer_exact {
            ctx.request_repaint_after(std::time::Duration::from_millis(15));
        }
    }
}

impl egui_dock::TabViewer for EditorState {
    type Tab = Tab;

    fn id(&mut self, tab: &mut Tab) -> egui::Id {
        egui::Id::new(*tab)
    }

    fn title(&mut self, tab: &mut Tab) -> egui::WidgetText {
        match tab {
            Tab::MediaBin => "Media",
            Tab::Viewer => "Viewer",
            Tab::Inspector => "Controls",
            Tab::Timeline => "Editor Timeline",
        }
        .into()
    }

    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut Tab) {
        match tab {
            Tab::MediaBin => crate::panels::media_bin(ui, self),
            Tab::Viewer => crate::panels::viewer(ui, self),
            Tab::Inspector => crate::panels::inspector(ui, self),
            Tab::Timeline => crate::timeline::show(ui, self),
        }
    }

    fn scroll_bars(&self, tab: &Tab) -> [bool; 2] {
        match tab {
            Tab::MediaBin | Tab::Inspector => [false, true],
            _ => [false, false],
        }
    }
}

impl EditorState {
    pub fn seq(&self) -> &Sequence {
        &self.project.sequences[0]
    }

    pub fn execute(&mut self, cmd: Box<dyn Command>) {
        self.status = cmd.name().to_owned();
        self.undo.execute(&mut self.project, cmd);
    }

    pub fn undo(&mut self) {
        match self.undo.undo(&mut self.project) {
            Some(n) => self.status = format!("Undo: {n}"),
            None => self.status = "Nothing to undo".into(),
        }
        self.validate_selection();
    }

    pub fn redo(&mut self) {
        match self.undo.redo(&mut self.project) {
            Some(n) => self.status = format!("Redo: {n}"),
            None => self.status = "Nothing to redo".into(),
        }
        self.validate_selection();
    }

    fn validate_selection(&mut self) {
        if let Some(id) = self.selection {
            if self.project.sequences[0].find_clip_mut(id).is_none() {
                self.selection = None;
            }
        }
        self.viewer_key = None; // force a texture refresh
    }

    // ---- Media ----------------------------------------------------------------

    pub fn import_dialog(&mut self) {
        if let Some(paths) = rfd::FileDialog::new()
            .add_filter("Media", &["mp4", "mov", "mkv", "avi", "webm", "m4v", "mxf", "mts", "png", "jpg", "jpeg", "gif", "wav", "mp3", "aac", "flac", "ogg"])
            .add_filter("All files", &["*"])
            .pick_files()
        {
            self.import(&paths);
        }
    }

    pub fn import(&mut self, paths: &[PathBuf]) {
        let mut ok = 0;
        for path in paths {
            if self.project.media.iter().any(|m| m.path == *path) {
                continue;
            }
            match reelforge_media::probe(path) {
                Ok(info) => {
                    let id = MediaId(self.project.alloc_id());
                    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    self.execute(Box::new(ImportMedia(MediaAsset { id, path: path.clone(), name, info })));
                    ok += 1;
                }
                Err(e) => self.status = format!("Could not import {}: {e:#}", path.display()),
            }
        }
        if ok > 0 {
            self.status = format!("Imported {ok} file(s)");
        }
    }

    /// Default length for media without a duration (still images).
    fn clip_duration(media: &MediaAsset) -> Time {
        if media.info.duration > Time::ZERO { media.info.duration } else { Time::from_secs_f64(5.0) }
    }

    /// Place media on a track at `at` (or after the last clip if that overlaps).
    pub fn add_to_timeline(&mut self, media_id: MediaId, track: Option<usize>, at: Option<Time>) {
        let Some(media) = self.project.media(media_id).cloned() else { return };
        let is_video = media.info.video.is_some();
        let want = if is_video { TrackKind::Video } else { TrackKind::Audio };

        // First clip in an empty sequence: match the sequence to the footage, like HitFilm does.
        if self.seq().duration() == Time::ZERO {
            if let Some(v) = &media.info.video {
                let s = &mut self.project.sequences[0];
                s.width = v.width;
                s.height = v.height;
                s.frame_rate = v.frame_rate;
            }
        }

        let seq = self.seq();
        let track = track
            .filter(|&t| seq.tracks.get(t).is_some_and(|tr| tr.kind == want))
            .or_else(|| seq.tracks.iter().position(|tr| tr.kind == want));
        let Some(track) = track else { return };

        let rate = seq.frame_rate;
        let duration = Self::clip_duration(&media).snap(rate);
        let tr = &seq.tracks[track];
        let mut start = at.unwrap_or_else(|| tr.end()).max(Time::ZERO).snap(rate);
        let overlaps = |s: Time| tr.clips.iter().any(|c| s < c.timeline_end() && c.timeline_start < s + duration);
        if overlaps(start) {
            start = tr.end();
        }

        let id = ClipId(self.project.alloc_id());
        let clip = Clip { id, media: media_id, timeline_start: start, source_in: Time::ZERO, duration };
        self.execute(Box::new(AddClip { seq: 0, track, clip }));
        self.selection = Some(id);
    }

    pub fn split_at_playhead(&mut self) {
        let t = self.playback.playhead;
        let seq = self.seq();
        // A cut exactly on a clip's first frame would produce an empty clip.
        let splittable = |c: &&Clip| c.contains(t) && c.timeline_start < t;
        let all = || seq.tracks.iter().flat_map(|tr| &tr.clips);
        let target = self
            .selection
            .filter(|id| all().filter(splittable).any(|c| c.id == *id))
            .or_else(|| seq.top_video_clip_at(t).filter(splittable).map(|c| c.id))
            .or_else(|| all().find(splittable).map(|c| c.id));
        match target {
            Some(id) => {
                let new_id = ClipId(self.project.alloc_id());
                self.execute(Box::new(SplitClip::new(0, id, t, new_id)));
            }
            None => self.status = "No clip under the playhead to split".into(),
        }
    }

    pub fn delete_selection(&mut self) {
        if let Some(id) = self.selection.take() {
            self.execute(Box::new(RemoveClip::new(0, id)));
        }
    }

    // ---- Project files -------------------------------------------------------

    fn save_project(&mut self, save_as: bool) {
        let path = match (&self.project_path, save_as) {
            (Some(p), false) => Some(p.clone()),
            _ => rfd::FileDialog::new().add_filter("Reelforge project", &["rfproj"]).set_file_name("project.rfproj").save_file(),
        };
        let Some(path) = path else { return };
        let result = serde_json::to_string_pretty(&self.project)
            .map_err(anyhow::Error::from)
            .and_then(|json| std::fs::write(&path, json).map_err(Into::into));
        self.status = match result {
            Ok(()) => {
                let msg = format!("Saved {}", path.display());
                self.project_path = Some(path);
                msg
            }
            Err(e) => format!("Save failed: {e}"),
        };
    }

    fn open_project(&mut self) {
        let Some(path) = rfd::FileDialog::new().add_filter("Reelforge project", &["rfproj"]).pick_file() else { return };
        match load_project(&path) {
            Ok(p) => {
                *self = EditorState { project: p, project_path: Some(path.clone()), ..self.fresh() };
                self.status = format!("Opened {}", path.display());
            }
            Err(e) => self.status = format!("Open failed: {e:#}"),
        }
    }

    fn fresh(&mut self) -> EditorState {
        EditorState {
            project: Project::new(),
            undo: UndoStack::default(),
            playback: Playback::default(),
            selection: None,
            timeline: TimelineView::default(),
            status: "New project".into(),
            project_path: None,
            sources: HashMap::new(),
            viewer_tex: self.viewer_tex.take(),
            viewer_key: None,
            viewer_exact: true,
        }
    }

    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui.button("New project").clicked() {
                    *self = self.fresh();
                }
                if ui.button("Open project…").clicked() {
                    self.open_project();
                }
                if ui.button("Save project        Ctrl+S").clicked() {
                    self.save_project(false);
                }
                if ui.button("Save project as…").clicked() {
                    self.save_project(true);
                }
                ui.separator();
                if ui.button("Import media…      Ctrl+I").clicked() {
                    self.import_dialog();
                }
            });
            ui.menu_button("Edit", |ui| {
                if ui.add_enabled(self.undo.can_undo(), egui::Button::new("Undo          Ctrl+Z")).clicked() {
                    self.undo();
                }
                if ui.add_enabled(self.undo.can_redo(), egui::Button::new("Redo          Ctrl+Y")).clicked() {
                    self.redo();
                }
                ui.separator();
                if ui.button("Split at playhead     S").clicked() {
                    self.split_at_playhead();
                }
                if ui.add_enabled(self.selection.is_some(), egui::Button::new("Delete clip          Del")).clicked() {
                    self.delete_selection();
                }
            });
        });
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        if ctx.egui_wants_keyboard_input() {
            return; // typing in a text field
        }
        let cmd = |k| KeyboardShortcut::new(Modifiers::COMMAND, k);
        let cmd_shift = |k| KeyboardShortcut::new(Modifiers::COMMAND | Modifiers::SHIFT, k);
        let (redo, undo, save, import) = ctx.input_mut(|i| {
            let redo = i.consume_shortcut(&cmd_shift(Key::Z)) || i.consume_shortcut(&cmd(Key::Y));
            (redo, i.consume_shortcut(&cmd(Key::Z)), i.consume_shortcut(&cmd(Key::S)), i.consume_shortcut(&cmd(Key::I)))
        });
        if redo {
            self.redo();
        }
        if undo {
            self.undo();
        }
        if save {
            self.save_project(false);
        }
        if import {
            self.import_dialog();
        }

        // Plain (unmodified) keys. Read per event, so a quick "Space then Ctrl+Z"
        // in one frame doesn't lose the Space to the frame's modifier state.
        let keys: Vec<Key> = ctx.input(|i| {
            i.events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::Key { key, pressed: true, modifiers, .. } if modifiers.is_none() => Some(*key),
                    _ => None,
                })
                .collect()
        });
        let rate = self.seq().frame_rate;
        let end = self.seq().duration();
        for key in keys {
            match key {
                Key::Space => self.playback.toggle(end),
                Key::ArrowLeft => self.playback.step(-1, rate),
                Key::ArrowRight => self.playback.step(1, rate),
                Key::Home => self.playback.seek(Time::ZERO),
                Key::End => self.playback.seek(end),
                Key::S => self.split_at_playhead(),
                Key::Delete | Key::Backspace => self.delete_selection(),
                _ => {}
            }
        }
    }

    // ---- Viewer ----------------------------------------------------------------

    /// Find the frame for the playhead and upload it to the viewer texture if it changed.
    fn update_viewer_frame(&mut self, ctx: &egui::Context) {
        let seq = &self.project.sequences[0];
        let t = self.playback.playhead;
        let Some(clip) = seq.top_video_clip_at(t) else {
            self.viewer_key = None;
            self.viewer_exact = true;
            return;
        };
        let Some(media) = self.project.media.iter().find(|m| m.id == clip.media) else { return };
        let src_t = clip.source_time(t);

        let slot = self
            .sources
            .entry(media.id)
            .or_insert_with(|| FrameSource::open(&media.path, Some(PREVIEW_MAX)).map_err(|e| format!("{e:#}")));
        let src = match slot {
            Ok(s) => s,
            Err(e) => {
                self.status = format!("Can't decode {}: {e}", media.name);
                self.viewer_key = None;
                return;
            }
        };
        if let Some(frame) = src.frame_at(src_t) {
            let key = (media.id, frame.pts, frame.generation);
            if self.viewer_key != Some(key) {
                let img = egui::ColorImage::from_rgba_unmultiplied([frame.width as usize, frame.height as usize], &frame.rgba);
                match &mut self.viewer_tex {
                    Some(tex) => tex.set(img, TextureOptions::LINEAR),
                    None => self.viewer_tex = Some(ctx.load_texture("viewer", img, TextureOptions::LINEAR)),
                }
                self.viewer_key = Some(key);
            }
        }
        self.viewer_exact = src.is_exact(src_t);
    }

    pub fn viewer_has_frame(&self) -> bool {
        self.viewer_key.is_some() && self.viewer_tex.is_some()
    }
}

fn load_project(path: &Path) -> anyhow::Result<Project> {
    Ok(serde_json::from_str(&std::fs::read_to_string(path)?)?)
}
