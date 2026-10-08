use crate::playback::Playback;
use crate::timeline::TimelineView;
use egui::{Key, KeyboardShortcut, Modifiers, TextureHandle, TextureOptions};
use egui_dock::{DockArea, DockState, NodeIndex, Style};
use crate::audio::AudioEngine;
use crossbeam_channel::{unbounded, Receiver, Sender};
use reelforge_core::undo::{delete_group, split_group, AddClip, Batch, ImportMedia, SetLink};
use reelforge_core::*;
use reelforge_media::{FrameSource, MixClip};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

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
    /// Bumped on every project change so the audio mixer can be resynced.
    rev: u64,
    mixed_rev: u64,
    /// Audio peak envelopes for drawing, computed in the background (100 per second).
    pub waveforms: HashMap<MediaId, Arc<Vec<f32>>>,
    waveform_tx: Sender<(MediaId, Vec<f32>)>,
    waveform_rx: Receiver<(MediaId, Vec<f32>)>,
}

/// Waveform resolution: peaks per second of audio.
pub const WAVEFORM_RATE: u32 = 100;

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

        let audio = match AudioEngine::new() {
            Ok(a) => Some(a),
            Err(e) => {
                log::warn!("audio output unavailable, playing silently: {e:#}");
                None
            }
        };
        let mut app = Self { dock, state: EditorState::new(Playback::with_audio(audio)) };
        app.state.status = "Import media with File → Import, or drop files onto the window.".into();

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
        st.sync_background();

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
                    ui.separator();
                    match &st.playback.audio {
                        Some(a) => ui.weak(format!("🔊 {} Hz · {}", a.rate, st.playback.clock_name())),
                        None => ui.weak("🔇 no audio device"),
                    }
                    .on_hover_text(st.playback.audio.as_ref().map(|a| a.device_name.clone()).unwrap_or_default());
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
    fn new(playback: Playback) -> Self {
        let (waveform_tx, waveform_rx) = unbounded();
        Self {
            project: Project::new(),
            undo: UndoStack::default(),
            playback,
            selection: None,
            timeline: TimelineView::default(),
            status: String::new(),
            project_path: None,
            sources: HashMap::new(),
            viewer_tex: None,
            viewer_key: None,
            viewer_exact: true,
            rev: 1,
            mixed_rev: 0,
            waveforms: HashMap::new(),
            waveform_tx,
            waveform_rx,
        }
    }

    /// A blank editor that keeps the audio device and viewer texture.
    fn fresh(&mut self) -> EditorState {
        self.playback.pause();
        let mut st = EditorState::new(Playback::with_audio(self.playback.audio.take()));
        st.viewer_tex = self.viewer_tex.take();
        st.status = "New project".into();
        st
    }

    /// Mark the project as changed (resyncs audio).
    pub fn touch(&mut self) {
        self.rev += 1;
        self.viewer_key = None;
    }

    pub fn seq(&self) -> &Sequence {
        &self.project.sequences[0]
    }

    pub fn execute(&mut self, cmd: Box<dyn Command>) {
        self.status = cmd.name().to_owned();
        self.undo.execute(&mut self.project, cmd);
        self.touch();
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
        self.touch();
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
                    let asset = MediaAsset { id, path: path.clone(), name, info };
                    self.compute_waveform(&asset);
                    self.execute(Box::new(ImportMedia(asset)));
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

    /// Place media on the timeline at `at` (or after existing clips if that overlaps).
    ///
    /// Footage with sound becomes a linked pair: video on a video track, audio
    /// on an audio track, moving and splitting together (unlink via Edit menu).
    pub fn add_to_timeline(&mut self, media_id: MediaId, track: Option<usize>, at: Option<Time>) {
        let Some(media) = self.project.media(media_id).cloned() else { return };

        // First clip in an empty sequence: match the sequence to the footage, like HitFilm does.
        if self.seq().duration() == Time::ZERO {
            if let Some(v) = &media.info.video {
                let s = &mut self.project.sequences[0];
                s.width = v.width;
                s.height = v.height;
                s.frame_rate = v.frame_rate;
                self.touch();
            }
        }

        let seq = self.seq();
        let pick = |kind: TrackKind| {
            track
                .filter(|&t| seq.tracks.get(t).is_some_and(|tr| tr.kind == kind))
                .or_else(|| seq.tracks.iter().position(|tr| tr.kind == kind))
        };
        let mut targets = Vec::new();
        if media.info.video.is_some() {
            targets.extend(pick(TrackKind::Video));
        }
        if media.info.audio.is_some() {
            targets.extend(pick(TrackKind::Audio));
        }
        if targets.is_empty() {
            self.status = format!("{} has no video or audio track to place", media.name);
            return;
        }

        let rate = seq.frame_rate;
        let duration = Self::clip_duration(&media).snap(rate);
        let free_end = targets.iter().map(|&t| seq.tracks[t].end()).max().unwrap_or_default();
        let mut start = at.unwrap_or(free_end).max(Time::ZERO).snap(rate);
        if targets.iter().any(|&t| seq.overlaps(t, start, duration, &[])) {
            start = free_end;
        }

        let link = (targets.len() > 1).then(|| self.project.alloc_id());
        let mut cmds: Vec<Box<dyn Command>> = Vec::new();
        let mut first = None;
        for track in targets {
            let id = ClipId(self.project.alloc_id());
            first.get_or_insert(id);
            let clip = Clip { id, media: media_id, timeline_start: start, source_in: Time::ZERO, duration, link };
            cmds.push(Box::new(AddClip { seq: 0, track, clip }));
        }
        self.execute(Box::new(Batch { name: "Add clip".into(), cmds }));
        self.selection = first;
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
        match target.and_then(|id| split_group(&mut self.project, 0, id, t)) {
            Some(cmd) => self.execute(cmd),
            None => self.status = "No clip under the playhead to split".into(),
        }
    }

    pub fn delete_selection(&mut self) {
        if let Some(id) = self.selection.take() {
            let cmd = delete_group(&self.project, 0, id);
            self.execute(cmd);
        }
    }

    pub fn selection_is_linked(&self) -> bool {
        self.selection.is_some_and(|id| self.seq().link_group(id).len() > 1)
    }

    pub fn unlink_selection(&mut self) {
        let Some(id) = self.selection else { return };
        let ids: Vec<ClipId> = self.seq().link_group(id).iter().map(|(_, c)| c.id).collect();
        if ids.len() > 1 {
            self.execute(Box::new(SetLink::new(0, ids, None)));
        }
    }

    pub fn toggle_mute(&mut self, track: usize) {
        let tr = &mut self.project.sequences[0].tracks[track];
        tr.muted = !tr.muted;
        self.status = format!("{} {}", tr.name, if tr.muted { "muted" } else { "unmuted" });
        self.touch();
    }

    /// Decode a peak envelope for drawing, off the UI thread.
    fn compute_waveform(&self, media: &MediaAsset) {
        if media.info.audio.is_none() || self.waveforms.contains_key(&media.id) {
            return;
        }
        let (id, path, tx) = (media.id, media.path.clone(), self.waveform_tx.clone());
        std::thread::spawn(move || match reelforge_media::waveform_peaks(&path, WAVEFORM_RATE) {
            Ok(p) => {
                let _ = tx.send((id, p));
            }
            Err(e) => log::warn!("waveform for {}: {e:#}", path.display()),
        });
    }

    /// Audio clips for the mixer, in the engine's sample rate.
    fn mix_clips(&self, rate: u32) -> Vec<MixClip> {
        let sr = FrameRate::new(rate as i64, 1);
        self.seq()
            .tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Audio && !t.muted)
            .flat_map(|t| &t.clips)
            .filter_map(|c| {
                let m = self.project.media(c.media)?;
                Some(MixClip {
                    id: c.id,
                    path: m.path.clone(),
                    start: c.timeline_start.to_frame(sr),
                    source_in: c.source_in.to_frame(sr),
                    len: c.duration.to_frame(sr),
                    gain: 1.0,
                })
            })
            .collect()
    }

    /// Per-frame housekeeping: push edits to the audio engine, collect finished waveforms.
    fn sync_background(&mut self) {
        if self.rev != self.mixed_rev {
            if let Some(a) = &self.playback.audio {
                a.set_clips(self.mix_clips(a.rate));
            }
            self.mixed_rev = self.rev;
        }
        while let Ok((id, peaks)) = self.waveform_rx.try_recv() {
            self.waveforms.insert(id, Arc::new(peaks));
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
                let media: Vec<_> = self.project.media.clone();
                for m in &media {
                    self.compute_waveform(m);
                }
            }
            Err(e) => self.status = format!("Open failed: {e:#}"),
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
                if ui.add_enabled(self.selection_is_linked(), egui::Button::new("Unlink audio/video")).clicked() {
                    self.unlink_selection();
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
