use crate::playback::Playback;
use crate::timeline::TimelineView;
use egui::{Key, KeyboardShortcut, Modifiers};
use egui_dock::{DockArea, DockState, NodeIndex, Style};
use crate::audio::AudioEngine;
use crossbeam_channel::{unbounded, Receiver, Sender};
use crate::preview::Preview;
use reelforge_core::undo::{
    split_group, AddClip, Batch, Edge, ImportMedia, SetClipAudio, SetClipTiming, SetClipVisual, SetDissolve,
    SetLink, SetTrackAudio, Timing, AddTrack, RemoveTrack, RenameTrack, SetTitle, SetTracker,
};
use reelforge_core::track::PointTrack;
use reelforge_render::text::FontEntry;
use reelforge_core::edit::{plan_trim, ripple_delete};
use reelforge_core::*;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;


#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tab {
    MediaBin,
    Viewer,
    Inspector,
    Timeline,
    Curves,
    Meters,
}

pub struct ReelforgeApp {
    dock: DockState<Tab>,
    state: EditorState,
    export: crate::export_ui::ExportUi,
}

/// Everything the panels read and edit. Separate from the dock layout so
/// `DockArea` can borrow the layout while panels borrow this mutably.
pub struct EditorState {
    pub project: Project,
    pub undo: UndoStack,
    pub playback: Playback,
    /// The clip the Controls panel shows (the last one clicked).
    pub selection: Option<ClipId>,
    /// Every selected clip (Ctrl+click, drag a box, Ctrl+A), including `selection`.
    pub selected: Vec<ClipId>,
    /// Drawing a freehand mask in the viewer: the clip, and the points so far (layer pixels).
    pub mask_draw: Option<(ClipId, Vec<[f32; 2]>)>,
    /// The mask being edited (clip, index), highlighted in the viewer and the Controls panel.
    pub active_mask: Option<(ClipId, usize)>,
    /// Clips copied with Ctrl+C / Ctrl+X.
    pub clipboard: reelforge_core::clipboard::Clipboard,
    pub timeline: TimelineView,
    pub curves: crate::curves::CurveView,
    pub status: String,
    pub project_path: Option<PathBuf>,
    /// Decoders + GPU compositor producing the viewer image.
    pub preview: Preview,
    /// A transform being changed interactively: the clip and its value before the gesture.
    /// Committed as one undo step when the mouse is released.
    live_edit: Option<LiveEdit>,
    pub meters: crate::meters::Meters,
    /// Set by the menu/shortcut; the app opens the export dialog.
    pub export_requested: bool,
    /// Show the export queue window.
    pub export_queue_requested: bool,
    /// The sequence shown in the timeline and viewer (0 = the main edit).
    pub active: usize,
    /// Switch to this sequence at the start of the next frame.
    pub open_request: Option<usize>,
    /// A point-tracking run in progress.
    pub tracking: Option<TrackJob>,
    /// Where the playhead was in each sequence, to come back to.
    playheads: HashMap<usize, Time>,
    /// Bumped on every project change so the audio mixer can be resynced.
    rev: u64,
    mixed_rev: u64,
    /// Audio peak envelopes for drawing, computed in the background (100 per second).
    pub waveforms: HashMap<MediaId, Arc<Vec<f32>>>,
    /// Installed fonts for titles, found in the background at startup (`None` until then).
    fonts: Arc<std::sync::Mutex<Option<Arc<Vec<FontEntry>>>>>,
    pub settings: crate::settings::Settings,
    pub settings_open: bool,
    pub proxies: crate::proxies::Proxies,
    /// Project revision when proxies were last looked for.
    proxied_rev: u64,
    /// This run's autosave/crash-detection session (moved along on New/Open).
    session: Option<crate::safety::Session>,
    /// Project revision at the last save (or open): anything newer is unsaved.
    saved_rev: u64,
    /// Crashed sessions found at startup, offered for recovery.
    recoveries: Vec<crate::safety::Recovery>,
    /// Something waiting on "save changes?".
    pending: Option<Pending>,
    /// The user agreed to close (after saving or discarding changes).
    quit_confirmed: bool,
    waveform_tx: Sender<(MediaId, Vec<f32>)>,
    waveform_rx: Receiver<(MediaId, Vec<f32>)>,
}

/// An action that would throw away unsaved changes, waiting for the user's answer.
#[derive(Clone, Debug, PartialEq)]
pub enum Pending {
    New,
    Open,
    Quit,
    Recover(usize),
    /// Open an autosave file as a new, unsaved project.
    OpenAutosave(PathBuf),
}

/// Waveform resolution: peaks per second of audio.
pub const WAVEFORM_RATE: u32 = 100;

impl ReelforgeApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        // Always dark, like HitFilm (not following the OS light/dark setting).
        cc.egui_ctx.set_theme(egui::ThemePreference::Dark);

        // HitFilm-like layout: media bin left, viewer centre, inspector right, timeline across the bottom.
        let mut dock = DockState::new(vec![Tab::Viewer]);
        let tree = dock.main_surface_mut();
        // `fraction` is the share kept by the original node (the viewer).
        let [top, timeline] = tree.split_below(NodeIndex::root(), 0.6, vec![Tab::Timeline, Tab::Curves]);
        tree.split_right(timeline, 0.93, vec![Tab::Meters]);
        let [viewer, _bin] = tree.split_left(top, 0.18, vec![Tab::MediaBin]);
        tree.split_right(viewer, 0.76, vec![Tab::Inspector]);

        let audio = match AudioEngine::new() {
            Ok(a) => Some(a),
            Err(e) => {
                log::warn!("audio output unavailable, playing silently: {e:#}");
                None
            }
        };
        let preview = Preview::new(cc.wgpu_render_state.clone());
        let mut app = Self { dock, state: EditorState::new(Playback::with_audio(audio), preview), export: Default::default() };
        if let Some(s) = cc.storage.and_then(|s| eframe::get_value::<crate::settings::Settings>(s, crate::settings::Settings::STORAGE_KEY)) {
            app.state.settings = s;
        }
        app.state.settings.apply();
        let (session, crashed) = crate::safety::Session::start();
        app.state.session = Some(session);
        app.state.recoveries = crashed;
        app.state.status = "Import media with File > Import, or drop files onto the window.".into();

        // `reelforge project.rfproj` opens a project (e.g. double-clicked in Explorer);
        // `reelforge clip1.mp4 clip2.mov …` imports the files and lays them out on the timeline.
        let args: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
        if let Some(proj) = args.iter().find(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("rfproj"))) {
            app.state.open_project_path(proj.clone());
        } else if !args.is_empty() {
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
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, crate::settings::Settings::STORAGE_KEY, &self.state.settings);
    }

    fn on_exit(&mut self) {
        // A clean exit: no crash to recover from next time.
        if let Some(s) = &self.state.session {
            s.end();
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let st = &mut self.state;

        // Closing the window with unsaved changes asks first.
        if ctx.input(|i| i.viewport().close_requested()) && st.is_dirty() && !st.quit_confirmed {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            st.pending = Some(Pending::Quit);
        }

        // Files dropped from Explorer.
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()).collect());
        if !dropped.is_empty() {
            st.import(&dropped);
        }

        st.handle_shortcuts(&ctx);
        st.sync_background();

        let seq = &st.project.sequences[st.active];
        let (rate, end) = (seq.frame_rate, seq.duration());
        st.playback.tick(rate, end);
        st.playback.scrub_if_moved();
        let sources = st.decode_paths();
        st.preview.update(&st.project, st.active, st.playback.playhead, st.playback.playing, &sources);
        if let Some(e) = st.preview.error.take() {
            st.status = e;
        }

        if std::mem::take(&mut st.export_requested) {
            self.export.open(st);
        }
        if let Some(i) = st.open_request.take() {
            st.open_sequence(i);
        }
        if std::mem::take(&mut st.export_queue_requested) {
            self.export.open_queue();
        }
        let export_note = self.export.summary();
        egui::Panel::top("menu").show(ui, |ui| st.menu_bar(ui));
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(&st.status);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if let Some(note) = &export_note {
                        if ui.link(note).on_hover_text("Open the export queue").clicked() {
                            st.export_queue_requested = true;
                        }
                        ui.separator();
                    }
                    let seq = &st.project.sequences[st.active];
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

        self.export.show(&ctx, st);
        if st.settings_open {
            let mut open = true;
            crate::settings::window(&ctx, st, &mut open);
            st.settings_open = open;
        }
        st.dialogs(&ctx);

        // A slider/gizmo gesture ends when the mouse is released and no text field is being typed in.
        if st.live_edit.is_some() && !ctx.input(|i| i.pointer.any_down()) && !ctx.egui_wants_keyboard_input() {
            st.commit_live_edit();
        }

        // Keep animating while playing or while waiting on a seek to land.
        if st.playback.playing {
            ctx.request_repaint();
        } else if !st.preview.exact {
            ctx.request_repaint_after(std::time::Duration::from_millis(15));
        } else if st.proxies.busy() || st.tracking.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
        // Wake up for autosave even when idle.
        ctx.request_repaint_after(std::time::Duration::from_secs(20));
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
            Tab::Curves => "Curves",
            Tab::Meters => "Meters",
        }
        .into()
    }

    fn ui(&mut self, ui: &mut egui::Ui, tab: &mut Tab) {
        match tab {
            Tab::MediaBin => crate::panels::media_bin(ui, self),
            Tab::Viewer => crate::panels::viewer(ui, self),
            Tab::Inspector => crate::panels::inspector(ui, self),
            Tab::Timeline => crate::timeline::show(ui, self),
            Tab::Curves => crate::curves::show(ui, self),
            Tab::Meters => crate::meters::show(ui, self),
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
    fn new(playback: Playback, preview: Preview) -> Self {
        let (waveform_tx, waveform_rx) = unbounded();
        Self {
            project: Project::new(),
            undo: UndoStack::default(),
            playback,
            selection: None,
            selected: Vec::new(),
            mask_draw: None,
            active_mask: None,
            clipboard: Default::default(),
            timeline: TimelineView::default(),
            curves: Default::default(),
            status: String::new(),
            project_path: None,
            preview,
            live_edit: None,
            meters: Default::default(),
            export_requested: false,
            export_queue_requested: false,
            active: 0,
            open_request: None,
            tracking: None,
            playheads: HashMap::new(),
            rev: 1,
            mixed_rev: 0,
            waveforms: HashMap::new(),
            fonts: {
                let slot = Arc::new(std::sync::Mutex::new(None));
                let fill = slot.clone();
                std::thread::spawn(move || {
                    let found = Arc::new(reelforge_render::text::system_fonts());
                    log::info!("found {} font faces", found.len());
                    *fill.lock().unwrap() = Some(found);
                });
                slot
            },
            waveform_tx,
            waveform_rx,
            settings: Default::default(),
            settings_open: false,
            proxies: Default::default(),
            proxied_rev: 0,
            session: None,
            saved_rev: 1,
            recoveries: Vec::new(),
            pending: None,
            quit_confirmed: false,
        }
    }

    /// A blank editor that keeps the audio device and viewer texture.
    fn fresh(&mut self) -> EditorState {
        self.playback.pause();
        let mut preview = std::mem::replace(&mut self.preview, Preview::new(None));
        preview.reset();
        let mut st = EditorState::new(Playback::with_audio(self.playback.audio.take()), preview);
        st.playback.scrub_audio = self.playback.scrub_audio;
        st.fonts = self.fonts.clone(); // no need to scan again
        st.settings = self.settings.clone();
        st.proxies = std::mem::take(&mut self.proxies);
        st.session = self.session.take();
        if let Some(s) = &mut st.session {
            s.set_project(None);
        }
        st.recoveries = std::mem::take(&mut self.recoveries);
        st.status = "New project".into();
        st
    }

    /// Mark the project as changed (resyncs audio).
    pub fn touch(&mut self) {
        self.rev += 1;
    }

    pub fn seq(&self) -> &Sequence {
        &self.project.sequences[self.active]
    }

    /// Show sequence `i` (the main edit, or a composite shot) in the timeline and viewer.
    pub fn open_sequence(&mut self, i: usize) {
        if i >= self.project.sequences.len() || i == self.active {
            return;
        }
        self.commit_live_edit();
        self.playback.pause();
        self.playheads.insert(self.active, self.playback.playhead);
        self.active = i;
        let t = self.playheads.get(&i).copied().unwrap_or(Time::ZERO);
        self.playback.seek(t);
        self.select_only(None);
        self.status = format!("Editing {}", self.seq().name);
        self.touch();
    }

    pub fn execute(&mut self, cmd: Box<dyn Command>) {
        self.status = cmd.name().to_owned();
        self.undo.execute(&mut self.project, cmd);
        self.touch();
    }

    pub fn undo(&mut self) {
        self.commit_live_edit();
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
        // Undoing "Make composite shot" can remove the sequence being shown.
        if self.active >= self.project.sequences.len() {
            self.active = 0;
            self.select_only(None);
        }
        if let Some(id) = self.selection {
            if self.project.sequences[self.active].find_clip_mut(id).is_none() {
                self.select_only(None);
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
        // Store full paths, so the project works wherever it's opened from.
        let paths: Vec<PathBuf> = paths.iter().map(|p| std::path::absolute(p).unwrap_or_else(|_| p.clone())).collect();
        for path in &paths {
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
                let s = &mut self.project.sequences[self.active];
                s.width = v.width;
                s.height = v.height;
                s.frame_rate = v.frame_rate;
                self.touch();
            }
        }

        let seq = self.seq();
        if track.is_some_and(|t| seq.tracks.get(t).is_some_and(|tr| tr.locked)) {
            self.status = LOCKED.into();
            return;
        }
        let pick = |kind: TrackKind| {
            track
                .filter(|&t| seq.tracks.get(t).is_some_and(|tr| tr.kind == kind))
                .or_else(|| seq.tracks.iter().position(|tr| tr.kind == kind && !tr.locked))
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
            let clip = Clip::new(id, media_id, start, Time::ZERO, duration, link);
            cmds.push(Box::new(AddClip { seq: self.active, track, clip }));
        }
        self.execute(Box::new(Batch { name: "Add clip".into(), cmds }));
        self.select_only(first);
    }

    pub fn split_at_playhead(&mut self) {
        let t = self.playback.playhead;
        let seq = self.seq();
        // A cut exactly on a clip's first frame would produce an empty clip.
        let splittable = |c: &&Clip| c.contains(t) && c.timeline_start < t && !seq.is_locked(c.id);
        let all = || seq.tracks.iter().flat_map(|tr| &tr.clips);
        let target = self
            .selection
            .filter(|id| all().filter(splittable).any(|c| c.id == *id))
            .or_else(|| seq.top_video_clip_at(t).filter(splittable).map(|c| c.id))
            .or_else(|| all().find(splittable).map(|c| c.id));
        match target.and_then(|id| split_group(&mut self.project, self.active, id, t)) {
            Some(cmd) => self.execute(cmd),
            None => self.status = "No clip under the playhead to split".into(),
        }
    }

    // ---- Selection ----------------------------------------------------------------

    /// Select just `id` (or nothing).
    pub fn select_only(&mut self, id: Option<ClipId>) {
        self.selection = id;
        self.selected = id.into_iter().collect();
    }

    /// Ctrl+click: add `id` to the selection, or take it out.
    pub fn toggle_selected(&mut self, id: ClipId) {
        let mut all = self.selected_ids();
        if let Some(i) = all.iter().position(|x| *x == id) {
            all.remove(i);
            self.selection = all.last().copied();
        } else {
            all.push(id);
            self.selection = Some(id);
        }
        self.selected = all;
    }

    /// Add clips to the selection (box select with Ctrl).
    pub fn add_selected(&mut self, ids: &[ClipId]) {
        let mut all = self.selected_ids();
        for id in ids {
            if !all.contains(id) {
                all.push(*id);
            }
        }
        if self.selection.is_none() {
            self.selection = all.first().copied();
        }
        self.selected = all;
    }

    /// All selected clips that still exist in the sequence shown.
    pub fn selected_ids(&self) -> Vec<ClipId> {
        let s = self.seq();
        let mut out: Vec<ClipId> = self.selected.iter().copied().filter(|id| s.find_clip(*id).is_some()).collect();
        if let Some(id) = self.selection.filter(|id| !out.contains(id) && s.find_clip(*id).is_some()) {
            out.push(id);
        }
        out
    }

    /// Is `id` selected (directly, or linked to a selected clip)?
    pub fn is_selected(&self, id: ClipId) -> bool {
        reelforge_core::undo::expand_links(&self.project, self.active, &self.selected_ids()).contains(&id)
    }

    pub fn select_all(&mut self) {
        let ids: Vec<ClipId> = self.seq().tracks.iter().flat_map(|t| &t.clips).map(|c| c.id).collect();
        self.selection = ids.first().copied();
        self.status = format!("Selected {} clip{}", ids.len(), if ids.len() == 1 { "" } else { "s" });
        self.selected = ids;
    }

    // ---- Masks --------------------------------------------------------------------

    /// The size a clip's masks are drawn in (its picture, or the frame for a title).
    pub fn layer_size(&self, clip: &Clip) -> [f32; 2] {
        match self.project.picture_size(clip) {
            Some([w, h]) => [w as f32, h as f32],
            None => [self.seq().width as f32, self.seq().height as f32],
        }
    }

    /// Add a mask to a clip as one undo step; it becomes the active mask.
    pub fn add_mask(&mut self, id: ClipId, mask: reelforge_core::mask::Mask) {
        self.commit_live_edit();
        let Some(mut v) = self.visual(id) else { return };
        let n = v.masks.len();
        let mut mask = mask;
        mask.name = format!("{} {}", mask.shape.label(), n + 1);
        v.masks.push(mask);
        self.set_visual_live(id, v);
        self.commit_live_edit();
        self.active_mask = Some((id, n));
        self.status = "Mask added: drag its corners in the Viewer, or inside it to move it".into();
    }

    /// Close the freehand shape being drawn and add it.
    pub fn finish_mask_drawing(&mut self) {
        let Some((id, pts)) = self.mask_draw.take() else { return };
        if pts.len() < 3 {
            self.status = "A freehand mask needs at least 3 points".into();
            return;
        }
        self.add_mask(id, reelforge_core::mask::Mask::freehand(pts));
    }

    // ---- Markers and navigation -------------------------------------------------------

    /// M: a marker at the playhead.
    pub fn add_marker(&mut self) {
        self.commit_live_edit();
        let s = self.seq();
        let at = self.playback.playhead.snap(s.frame_rate);
        if s.markers.iter().any(|m| m.at == at) {
            self.status = "There's already a marker here".into();
            return;
        }
        let old = s.markers.clone();
        let mut new = old.clone();
        new.push(reelforge_core::Marker { at, name: s.next_marker_name(), color: Default::default() });
        new.sort_by_key(|m| m.at);
        self.execute(Box::new(reelforge_core::undo::SetMarkers { seq: self.active, old, new }));
        self.status = format!("Marker at {} (double-click it on the ruler to name it)", at.timecode(self.seq().frame_rate));
    }

    /// Change the markers as part of a gesture (dragging one, typing its name).
    pub fn set_markers_live(&mut self, markers: Vec<reelforge_core::Marker>) {
        let before = self.seq().markers.clone();
        if before == markers {
            return;
        }
        self.begin_live(LiveEdit::Markers(before));
        self.project.sequences[self.active].markers = markers;
    }

    pub fn remove_marker(&mut self, index: usize) {
        self.commit_live_edit();
        let old = self.seq().markers.clone();
        if index >= old.len() {
            return;
        }
        let mut new = old.clone();
        new.remove(index);
        self.execute(Box::new(reelforge_core::undo::SetMarkers { seq: self.active, old, new }));
    }

    /// Up/Down: jump to the previous / next cut, marker or in/out point.
    pub fn jump(&mut self, forward: bool) {
        let s = self.seq();
        let rate = s.frame_rate;
        let now = self.playback.playhead;
        let half = Time(rate.frame_duration().0 / 2);
        let points = s.edit_points();
        let to = if forward { points.into_iter().find(|&p| p > now + half) } else { points.into_iter().rev().find(|&p| p + half < now) };
        if let Some(t) = to {
            let what = s.markers.iter().find(|m| m.at == t).map(|m| m.name.clone());
            self.playback.pause();
            self.playback.seek(t);
            self.status = match what {
                Some(name) => format!("{} · {name}", t.timecode(rate)),
                None => t.timecode(rate),
            };
        }
    }

    // ---- Speed --------------------------------------------------------------------

    /// Set the speed / direction of `id` (and its linked clips).
    pub fn set_speed(&mut self, id: ClipId, speed: f32, reverse: bool) {
        self.commit_live_edit();
        match reelforge_core::speed::change_speed(&self.project, self.active, id, speed, reverse) {
            Ok(cmd) => {
                self.execute(cmd);
                self.status = match (speed, reverse) {
                    (0.0, _) => "Freeze frame: holds the clip's first frame".into(),
                    (s, false) => format!("Speed {:.0}%", s * 100.0),
                    (s, true) => format!("Speed {:.0}%, reversed", s * 100.0),
                };
            }
            Err(e) => self.status = format!("Can't change the speed: {e}"),
        }
    }

    /// Hold the frame at the playhead for 2 s in the selected video clip (or
    /// the top one under the playhead), pushing the rest of it later.
    pub fn freeze_frame_at_playhead(&mut self) {
        self.commit_live_edit();
        let t = self.playback.playhead.snap(self.seq().frame_rate);
        let s = self.seq();
        let p = &self.project;
        let footage = |c: &Clip| c.contains(t) && !p.is_still(c.media) && c.speed != 0.0;
        let is_video = |id: ClipId| s.find_clip(id).is_some_and(|(ti, c)| s.tracks[ti].kind == TrackKind::Video && footage(c));
        // The selected clip if it's under the playhead, else the top moving clip there.
        let top = || s.tracks.iter().rev().filter(|tr| tr.kind == TrackKind::Video && !tr.muted).find_map(|tr| tr.clips.iter().find(|c| footage(c))).map(|c| c.id);
        let target = self.selection.filter(|id| is_video(*id)).or_else(top);
        let Some(id) = target else {
            self.status = "Put the playhead over a video clip to freeze a frame of it".into();
            return;
        };
        let len = Time::from_secs_f64(2.0).snap(s.frame_rate);
        match reelforge_core::speed::freeze_frame(&mut self.project, self.active, id, t, len) {
            Ok((cmd, fid)) => {
                self.execute(cmd);
                self.select_only(Some(fid));
                self.status = "Added a 2 s freeze frame: trim it to change its length".into();
            }
            Err(e) => self.status = format!("Can't freeze a frame: {e}"),
        }
    }

    // ---- Clipboard ----------------------------------------------------------------

    /// Copy the selected clips (and their linked clips). Returns how many.
    pub fn copy_selection(&mut self) -> usize {
        self.commit_live_edit();
        let ids = self.selected_ids();
        if ids.is_empty() {
            self.status = "Select clips to copy first".into();
            return 0;
        }
        self.clipboard = reelforge_core::clipboard::copy_clips(&self.project, self.active, &ids);
        let n = self.clipboard.len();
        self.status = format!("Copied {n} clip{}", if n == 1 { "" } else { "s" });
        n
    }

    pub fn cut_selection(&mut self) {
        if self.copy_selection() > 0 {
            let n = self.clipboard.len();
            if self.delete_selection() {
                self.status = format!("Cut {n} clip{}", if n == 1 { "" } else { "s" });
            }
        }
    }

    /// Paste at the playhead (or the first place after it with room).
    pub fn paste(&mut self) {
        self.commit_live_edit();
        if self.clipboard.is_empty() {
            self.status = "Nothing to paste: copy some clips first (Ctrl+C)".into();
            return;
        }
        let t = self.playback.playhead.snap(self.seq().frame_rate);
        let cb = self.clipboard.clone();
        match reelforge_core::clipboard::paste_clips(&mut self.project, self.active, &cb, t) {
            Some(p) => {
                let (n, at, skipped) = (p.ids.len(), p.at, p.skipped);
                self.execute(p.cmd);
                self.selection = p.ids.first().copied();
                self.selected = p.ids;
                let rate = self.seq().frame_rate;
                let mut msg = format!("Pasted {n} clip{}", if n == 1 { "" } else { "s" });
                if at != t {
                    msg += &format!(" at {} (the next place with room)", at.timecode(rate));
                }
                if skipped > 0 {
                    msg += &format!("; left out {skipped} that can't go here");
                }
                self.status = msg;
            }
            None => self.status = "Can't paste here (a locked track, or a composite shot inside itself)".into(),
        }
    }

    /// Delete every selected clip. Returns whether anything was deleted.
    pub fn delete_selection(&mut self) -> bool {
        let ids = self.selected_ids();
        if ids.is_empty() {
            return false;
        }
        if ids.iter().any(|id| self.seq().is_locked(*id)) {
            self.status = LOCKED.into();
            return false;
        }
        self.select_only(None);
        let cmd = reelforge_core::undo::delete_clips(&self.project, self.active, &ids);
        self.execute(cmd);
        true
    }

    /// Installed fonts, once the background scan has finished.
    pub fn fonts(&self) -> Option<Arc<Vec<FontEntry>>> {
        self.fonts.lock().unwrap().clone()
    }

    // ---- Tracks ---------------------------------------------------------------

    pub fn add_track(&mut self, kind: TrackKind) {
        self.commit_live_edit();
        let cmd = AddTrack::new(&self.project, self.active, kind);
        self.execute(Box::new(cmd));
    }

    pub fn delete_track(&mut self, index: usize) {
        self.commit_live_edit();
        match RemoveTrack::new(&self.project, self.active, index) {
            Ok(cmd) => {
                let name = self.seq().tracks[index].name.clone();
                let clips = self.seq().tracks[index].clips.len();
                self.execute(Box::new(cmd));
                self.validate_selection();
                self.status = match clips {
                    0 => format!("Deleted {name}"),
                    n => format!("Deleted {name} and its {n} clip{} (Ctrl+Z to undo)", if n == 1 { "" } else { "s" }),
                };
            }
            Err(why) => self.status = format!("Can't delete that track: {why}"),
        }
    }

    pub fn rename_track(&mut self, index: usize, name: String) {
        let Some(old) = self.seq().tracks.get(index).map(|t| t.name.clone()) else { return };
        if name.is_empty() || name == old {
            return;
        }
        self.execute(Box::new(RenameTrack { seq: self.active, index, old, new: name }));
    }

    // ---- Titles ---------------------------------------------------------------

    /// Add a 5 s title at the playhead on the top video track that has room
    /// (a new video track if none does), and select it.
    pub fn add_title(&mut self) {
        self.commit_live_edit();
        let s = self.seq();
        let rate = s.frame_rate;
        let start = self.playback.playhead.snap(rate);
        let dur = Time::from_secs_f64(5.0).snap(rate);
        let mut spec = TitleSpec::for_frame(s.height);
        if let Some(fonts) = self.fonts() {
            spec.font = reelforge_render::text::default_title_font(&fonts);
        }
        let free = (0..s.tracks.len())
            .rev()
            .find(|&t| s.tracks[t].kind == TrackKind::Video && !s.tracks[t].locked && !s.overlaps(t, start, dur, &[]));
        let mut cmds: Vec<Box<dyn Command>> = Vec::new();
        let track = match free {
            Some(t) => t,
            None => {
                let add = AddTrack::new(&self.project, self.active, TrackKind::Video);
                let t = add.index;
                cmds.push(Box::new(add));
                t
            }
        };
        let id = ClipId(self.project.alloc_id());
        cmds.push(Box::new(AddClip { seq: self.active, track, clip: Clip::new_title(id, start, dur, spec) }));
        self.execute(Box::new(Batch { name: "Add title".into(), cmds }));
        self.select_only(Some(id));
        self.status = "Added a title: type its text in the Controls panel".into();
    }

    // ---- Composite shots ---------------------------------------------------------

    /// A new, empty composite shot the size of the current sequence, opened for editing.
    pub fn new_composite(&mut self) {
        self.commit_live_edit();
        let s = self.seq();
        let seq = Sequence::new(reelforge_core::undo::next_composite_name(&self.project), s.width, s.height, s.frame_rate);
        let cmd = reelforge_core::undo::AddSequence::new(&self.project, seq);
        let index = cmd.index;
        self.execute(Box::new(cmd));
        self.open_sequence(index);
        self.status = "New composite shot: add clips to it, then put it in your edit from the Media panel".into();
    }

    /// The clips "Make composite shot" would take: everything entirely between
    /// the in and out marks (on unlocked tracks) if they're set, else the
    /// selected clip and what's linked to it.
    pub fn composite_candidates(&self) -> Vec<ClipId> {
        let s = self.seq();
        let selected = self.selected_ids();
        if selected.len() > 1 {
            return selected;
        }
        if s.in_point.is_some() || s.out_point.is_some() {
            if let Some((a, b)) = s.marked_range() {
                return s
                    .tracks
                    .iter()
                    .filter(|t| !t.locked)
                    .flat_map(|t| &t.clips)
                    .filter(|c| c.timeline_start >= a && c.timeline_end() <= b)
                    .map(|c| c.id)
                    .collect();
            }
        }
        self.selected_ids()
    }

    pub fn make_composite(&mut self) {
        self.commit_live_edit();
        let ids = self.composite_candidates();
        let name = reelforge_core::undo::next_composite_name(&self.project);
        match reelforge_core::undo::make_composite(&mut self.project, self.active, &ids, name) {
            Ok(cmd) => {
                let (clip, index) = (cmd.clip, cmd.index);
                self.execute(Box::new(cmd));
                self.select_only(Some(clip));
                self.status = format!("Made {}: double-click it to edit inside", self.project.sequences[index].name);
            }
            Err(e) => self.status = e,
        }
    }

    /// Put composite shot `index` into the current sequence at the playhead.
    pub fn add_composite_to_timeline(&mut self, index: usize) {
        self.commit_live_edit();
        if self.project.contains_sequence(index, self.active) {
            self.status = "A composite shot can't contain itself".into();
            return;
        }
        let Some(comp) = self.project.sequences.get(index) else { return };
        let dur = comp.duration();
        if dur == Time::ZERO {
            self.status = format!("{} is empty: open it and add some clips first", comp.name);
            return;
        }
        let has = |k: TrackKind| comp.tracks.iter().any(|t| t.kind == k && !t.clips.is_empty());
        let (video, audio) = (has(TrackKind::Video), has(TrackKind::Audio));
        let s = self.seq();
        let start = self.playback.playhead.snap(s.frame_rate);
        let dur = dur.snap(s.frame_rate).max(s.frame_rate.frame_duration());
        let free = |k: TrackKind| (0..s.tracks.len()).find(|&t| s.tracks[t].kind == k && !s.tracks[t].locked && !s.overlaps(t, start, dur, &[]));
        let mut cmds: Vec<Box<dyn Command>> = Vec::new();
        let mut targets = Vec::new();
        // Video first: a new video track goes below the audio tracks, shifting them up one.
        let mut shift = 0;
        if video {
            match free(TrackKind::Video) {
                Some(t) => targets.push(t),
                None => {
                    let add = AddTrack::new(&self.project, self.active, TrackKind::Video);
                    targets.push(add.index);
                    shift = 1;
                    cmds.push(Box::new(add));
                }
            }
        }
        if audio {
            match free(TrackKind::Audio) {
                Some(t) => targets.push(t + shift),
                None => {
                    let index = s.tracks.len() + shift;
                    targets.push(index);
                    let track = reelforge_core::Track::new(s.next_track_name(TrackKind::Audio), TrackKind::Audio);
                    cmds.push(Box::new(AddTrack { seq: self.active, index, track }));
                }
            }
        }
        let link = (targets.len() > 1).then(|| self.project.alloc_id());
        let mut first = None;
        for track in targets {
            let id = ClipId(self.project.alloc_id());
            first.get_or_insert(id);
            cmds.push(Box::new(AddClip { seq: self.active, track, clip: Clip::new_nested(id, index, start, dur, link) }));
        }
        self.execute(Box::new(Batch { name: "Add composite shot".into(), cmds }));
        self.select_only(first);
    }

    pub fn set_title_live(&mut self, id: ClipId, spec: TitleSpec) {
        let Some(before) = self.seq().find_clip(id).and_then(|(_, c)| c.title.clone()) else { return };
        if before == spec {
            return;
        }
        self.begin_live(LiveEdit::Title(id, before));
        if let Some((_, c)) = self.project.sequences[self.active].find_clip_mut(id) {
            c.title = Some(spec);
        }
    }

    // ---- Motion tracking -----------------------------------------------------------

    /// Change a clip's tracker as part of a gesture (dragging it, a slider).
    pub fn set_tracker_live(&mut self, id: ClipId, t: Option<PointTrack>) {
        let Some(before) = self.seq().find_clip(id).map(|(_, c)| c.tracker.clone()) else { return };
        if before == t {
            return;
        }
        self.begin_live(LiveEdit::Tracker(id, before));
        if let Some((_, c)) = self.project.sequences[self.active].find_clip_mut(id) {
            c.tracker = t;
        }
    }

    /// Change a clip's tracker as one undo step.
    pub fn set_tracker(&mut self, id: ClipId, t: Option<PointTrack>, label: &'static str) {
        self.commit_live_edit();
        let Some(old) = self.seq().find_clip(id).map(|(_, c)| c.tracker.clone()) else { return };
        if old != t {
            self.execute(Box::new(SetTracker { seq: self.active, id, old, new: t, label }));
        }
    }

    /// Track the selected clip's feature from the playhead to the end of the
    /// clip (or back to its start), in the background.
    pub fn start_tracking(&mut self, id: ClipId, forward: bool) {
        self.commit_live_edit();
        if self.tracking.is_some() {
            return;
        }
        let Some((_, c)) = self.seq().find_clip(id) else { return };
        let (Some(track), Some(media)) = (c.tracker.clone(), self.project.media(c.media).cloned()) else { return };
        let t = self.playback.playhead.max(c.timeline_start).min(c.timeline_end() - self.seq().frame_rate.frame_duration());
        // Tracking works in media time; for a reversed clip "forward" on the timeline is backwards in the media.
        let from = c.media_at(t);
        let until = if forward { c.media_at(c.timeline_end() - self.seq().frame_rate.frame_duration()) } else { c.media_at(c.timeline_start) };
        let (tx, rx) = unbounded();
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop = cancel.clone();
        std::thread::Builder::new()
            .name("tracker".into())
            .spawn(move || {
                let progress_tx = tx.clone();
                let r = reelforge_media::track_point(&media.path, &media.info, &track, from, until, &stop, |p| {
                    let _ = progress_tx.send(TrackMsg::Progress(p));
                });
                let _ = tx.send(TrackMsg::Done(r.map_err(|e| format!("{e:#}"))));
            })
            .expect("tracker thread");
        self.tracking = Some(TrackJob { seq: self.active, clip: id, rx, cancel, progress: 0.0 });
        self.status = "Tracking…".into();
    }

    pub fn cancel_tracking(&mut self) {
        if let Some(j) = &self.tracking {
            j.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    fn poll_tracking(&mut self) {
        let Some(job) = &mut self.tracking else { return };
        let mut done = None;
        while let Ok(m) = job.rx.try_recv() {
            match m {
                TrackMsg::Progress(p) => job.progress = p,
                TrackMsg::Done(r) => done = Some(r),
            }
        }
        let Some(result) = done else { return };
        let job = self.tracking.take().unwrap();
        match result {
            Err(e) => self.status = format!("Tracking failed: {e}"),
            Ok((samples, end)) => {
                let n = samples.len().saturating_sub(1);
                let Some(old) = self.project.sequences.get(job.seq).and_then(|s| s.find_clip(job.clip)).and_then(|(_, c)| c.tracker.clone()) else { return };
                let mut new = old.clone();
                new.merge(samples);
                let rate = self.project.sequences[job.seq].frame_rate;
                self.status = match end {
                    reelforge_media::TrackEnd::Done => format!("Tracked {n} frames"),
                    reelforge_media::TrackEnd::Cancelled => format!("Tracking stopped after {n} frames"),
                    reelforge_media::TrackEnd::Lost(t) => {
                        format!("Tracked {n} frames, then lost the feature at {} (source time): move the tracker there and track again", t.timecode(rate))
                    }
                };
                let status = self.status.clone();
                self.execute(Box::new(SetTracker { seq: job.seq, id: job.clip, old: Some(old), new: Some(new), label: "Track point" }));
                self.status = status;
            }
        }
    }

    /// Make mask `index` of the tracked clip move with its tracked feature.
    pub fn apply_track_to_mask(&mut self, tracked: ClipId, index: usize) {
        self.commit_live_edit();
        let Some((_, c)) = self.seq().find_clip(tracked) else { return };
        let (Some(track), Some(mask)) = (&c.tracker, c.masks.get(index)) else { return };
        let Some(m) = reelforge_core::track::follow_mask(c, mask, track) else { return };
        let name = m.name.clone();
        let mut v = c.visual();
        v.masks[index] = m;
        self.set_visual_live(tracked, v);
        self.commit_live_edit();
        self.status = format!("{name} now follows the tracked point");
    }

    /// Make `target` follow the tracked feature of `tracked` (or, if they're the
    /// same clip, hold it steady), as position keyframes.
    pub fn apply_track(&mut self, tracked: ClipId, target: ClipId) {
        self.commit_live_edit();
        let s = self.seq();
        let (Some((_, a)), Some((_, b))) = (s.find_clip(tracked), s.find_clip(target)) else { return };
        let Some(track) = &a.tracker else { return };
        let Some([w, h]) = self.project.picture_size(a) else { return };
        let frame = [s.width as f32, s.height as f32];
        let stabilise = tracked == target;
        let Some(anim) = reelforge_core::track::follow_keys(a, track, [w as f32, h as f32], frame, b, stabilise) else {
            self.status = "Nothing to apply: the track doesn't overlap that clip".into();
            return;
        };
        if s.is_locked(target) {
            self.status = LOCKED.into();
            return;
        }
        let old = b.visual();
        let new = ClipVisual { anim, ..old.clone() };
        let name = b.display_name(&self.project).into_owned();
        self.execute(Box::new(SetClipVisual { seq: self.active, id: target, old, new }));
        self.status = if stabilise { "Stabilised: the clip now moves against the tracked motion".into() } else { format!("{name} now follows the tracked point") };
    }

    pub fn toggle_lock(&mut self, track: usize) {
        self.commit_live_edit();
        let tr = &mut self.project.sequences[self.active].tracks[track];
        tr.locked = !tr.locked;
        self.status = if tr.locked {
            format!("{} locked: its clips can't be edited and ripple edits leave it alone", tr.name)
        } else {
            format!("{} unlocked", tr.name)
        };
        self.touch();
    }

    // ---- In/out marks ------------------------------------------------------------

    pub fn mark_in(&mut self) {
        let t = self.playback.playhead;
        let s = &mut self.project.sequences[self.active];
        s.in_point = Some(t);
        if s.out_point.is_some_and(|o| o <= t) {
            s.out_point = None;
        }
        self.status = format!("In point: {}", t.timecode(s.frame_rate));
        self.touch();
    }

    /// The out point is placed after the frame under the playhead, so that frame is included.
    pub fn mark_out(&mut self) {
        let s = &mut self.project.sequences[self.active];
        let rate = s.frame_rate;
        let t = self.playback.playhead.snap(rate) + rate.frame_duration();
        s.out_point = Some(t);
        if s.in_point.is_some_and(|i| i >= t) {
            s.in_point = None;
        }
        self.status = format!("Out point: {} (includes the frame at the playhead)", t.timecode(rate));
        self.touch();
    }

    pub fn clear_marks(&mut self) {
        let s = &mut self.project.sequences[self.active];
        (s.in_point, s.out_point) = (None, None);
        self.status = "In/out points cleared".into();
        self.touch();
    }

    pub fn selection_is_linked(&self) -> bool {
        self.selection.is_some_and(|id| self.seq().link_group(id).len() > 1)
    }

    pub fn unlink_selection(&mut self) {
        let Some(id) = self.selection else { return };
        let ids: Vec<ClipId> = self.seq().link_group(id).iter().map(|(_, c)| c.id).collect();
        if ids.len() > 1 {
            self.execute(Box::new(SetLink::new(self.active, ids, None)));
        }
    }

    /// Add (or remove) a 1 s transition at the cut nearest the playhead on tracks
    /// of `kind`: an audio crossfade or a video cross-dissolve. Prefers the
    /// selected clip's cut, otherwise any matching cut within 1 s.
    pub fn toggle_transition_at_playhead(&mut self, kind: TrackKind) {
        let t = self.playback.playhead;
        let seq = self.seq();
        let mut cuts: Vec<(Time, ClipId)> = Vec::new(); // (distance, incoming clip)
        for tr in seq.tracks.iter().filter(|t| t.kind == kind) {
            for pair in tr.clips.windows(2) {
                if pair[0].timeline_end() == pair[1].timeline_start {
                    let d = Time((pair[1].timeline_start.0 - t.0).abs());
                    cuts.push((d, pair[1].id));
                }
            }
        }
        let selected: Vec<ClipId> = self.selection.map(|id| seq.link_group(id).iter().map(|(_, c)| c.id).collect()).unwrap_or_default();
        let preferred = cuts.iter().filter(|(_, id)| {
            selected.contains(id) || self.project.clip_before(self.active, *id).is_some_and(|l| selected.contains(&l))
        });
        let pick = preferred
            .min_by_key(|(d, _)| *d)
            .or_else(|| cuts.iter().filter(|(d, _)| *d <= Time(Time::TICKS_PER_SECOND)).min_by_key(|(d, _)| *d))
            .map(|(_, id)| *id);
        let what = if kind == TrackKind::Audio { "Crossfade" } else { "Cross-dissolve" };
        let Some(id) = pick else {
            self.status = format!("No cut between two {} clips near the playhead", if kind == TrackKind::Audio { "audio" } else { "video" });
            return;
        };
        let one_sec = Time::from_secs_f64(1.0).snap(self.seq().frame_rate);
        match kind {
            TrackKind::Audio => {
                let Some(old) = self.clip_audio(id) else { return };
                let mut new = old.clone();
                new.crossfade_in = if old.crossfade_in > Time::ZERO { Time::ZERO } else { one_sec };
                self.execute(Box::new(SetClipAudio { seq: self.active, id, old, new }));
            }
            TrackKind::Video => {
                let Some(old) = self.seq().find_clip(id).map(|(_, c)| c.dissolve_in) else { return };
                let new = if old > Time::ZERO { Time::ZERO } else { one_sec };
                self.execute(Box::new(SetDissolve { seq: self.active, id, old, new }));
            }
        }
        self.select_only(Some(id));
        self.status = match self.project.crossfade_into(self.active, id) {
            Some(x) if x.is_empty() => format!("{what} added, but there's no spare media at the cut, so it plays as a plain cut"),
            Some(x) if x.len() < x.requested => format!("{what} added: {:.2} s (limited by spare media)", x.len().as_secs_f64()),
            Some(x) => format!("{what} added: {:.2} s", x.len().as_secs_f64()),
            None => format!("{what} removed"),
        };
    }

    pub fn toggle_mute(&mut self, track: usize) {
        let tr = &mut self.project.sequences[self.active].tracks[track];
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

    /// Per-frame housekeeping: push edits to the audio engine, collect finished waveforms.
    fn sync_background(&mut self) {
        if self.rev != self.proxied_rev {
            self.proxies.refresh(&self.project, self.settings.auto_proxies);
            self.proxied_rev = self.rev;
        }
        self.poll_tracking();
        if self.proxies.poll() {
            self.status = "A proxy is ready: the viewer uses it now".into();
        }
        if let Some(s) = &mut self.session {
            if let Some(msg) = s.tick(&self.project, self.rev, self.settings.autosave_minutes) {
                if msg.starts_with("Autosave failed") {
                    self.status = msg;
                }
            }
        }
        if self.rev != self.mixed_rev {
            if let Some(a) = &self.playback.audio {
                a.set_clips(reelforge_export::mix_clips(&self.project, self.active, a.rate));
            }
            self.mixed_rev = self.rev;
        }
        while let Ok((id, peaks)) = self.waveform_rx.try_recv() {
            self.waveforms.insert(id, Arc::new(peaks));
        }
    }

    // ---- Project files -------------------------------------------------------

    /// Save (asking for a file name if needed). Returns whether it was saved.
    fn save_project(&mut self, save_as: bool) -> bool {
        self.commit_live_edit();
        let path = match (&self.project_path, save_as) {
            (Some(p), false) => Some(p.clone()),
            _ => rfd::FileDialog::new().add_filter("Reelforge project", &["rfproj"]).set_file_name("project.rfproj").save_file(),
        };
        let Some(path) = path else { return false };
        let result = serde_json::to_string_pretty(&self.project)
            .map_err(anyhow::Error::from)
            .and_then(|json| crate::safety::write_atomic(&path, json.as_bytes()).map_err(Into::into));
        match result {
            Ok(()) => {
                self.status = format!("Saved {}", path.display());
                self.project_path = Some(path.clone());
                self.saved_rev = self.rev;
                if let Some(s) = &mut self.session {
                    s.set_project(Some(path));
                    s.autosaved_rev = self.rev;
                }
                true
            }
            Err(e) => {
                self.status = format!("Save failed: {e}");
                false
            }
        }
    }

    /// Are there changes since the last save?
    pub fn is_dirty(&self) -> bool {
        self.rev != self.saved_rev
    }

    /// Run `action` now, or ask about unsaved changes first.
    pub fn request(&mut self, action: Pending) {
        self.commit_live_edit();
        if self.is_dirty() {
            self.pending = Some(action);
        } else {
            self.perform(action);
        }
    }

    fn perform(&mut self, action: Pending) {
        match action {
            Pending::New => {
                *self = self.fresh();
                self.saved_rev = self.rev;
            }
            Pending::Open => {
                if let Some(path) = rfd::FileDialog::new().add_filter("Reelforge project", &["rfproj"]).pick_file() {
                    self.open_project_path(path);
                }
            }
            Pending::Quit => {
                self.quit_confirmed = true;
            }
            Pending::Recover(i) => self.recover(i),
            Pending::OpenAutosave(p) => {
                self.open_project_path(p);
                self.project_path = None; // save it somewhere real, not over the autosave
                self.saved_rev = self.rev.wrapping_sub(1);
                if let Some(s) = &mut self.session {
                    s.set_project(None);
                }
            }
        }
    }

    pub fn open_project_path(&mut self, path: PathBuf) {
        match load_project(&path) {
            Ok(p) => {
                *self = EditorState { project: p, project_path: Some(path.clone()), ..self.fresh() };
                self.saved_rev = self.rev;
                if let Some(s) = &mut self.session {
                    s.set_project(Some(path.clone()));
                    s.autosaved_rev = self.rev;
                }
                self.status = format!("Opened {}", path.display());
                self.after_load();
            }
            Err(e) => self.status = format!("Open failed: {e:#}"),
        }
    }

    /// After opening or recovering: waveforms, and a warning about missing files.
    fn after_load(&mut self) {
        let media: Vec<_> = self.project.media.clone();
        for m in &media {
            self.compute_waveform(m);
        }
        let missing = reelforge_core::undo::missing_media(&self.project);
        if !missing.is_empty() {
            self.status = format!(
                "{} media file{} can't be found: relink {} in the Media panel",
                missing.len(),
                if missing.len() == 1 { "" } else { "s" },
                if missing.len() == 1 { "it" } else { "them" }
            );
        }
    }

    /// Load crashed session `i`'s autosave as an unsaved project.
    fn recover(&mut self, i: usize) {
        if i >= self.recoveries.len() {
            return;
        }
        let r = self.recoveries.remove(i);
        match r.load() {
            Ok(p) => {
                let path = r.info.project.clone();
                *self = EditorState { project: p, project_path: path.clone(), ..self.fresh() };
                self.saved_rev = self.rev.wrapping_sub(1); // recovered work hasn't been saved
                if let Some(s) = &mut self.session {
                    s.set_project(path.clone());
                }
                self.after_load();
                self.status = match &path {
                    Some(p) => format!("Recovered your work on {}: save (Ctrl+S) to keep it", p.display()),
                    None => "Recovered your unsaved project: save it (Ctrl+S) to keep it".into(),
                };
                r.dismiss(true);
            }
            Err(e) => self.status = format!("Couldn't read the autosave: {e:#}"),
        }
    }

    /// The file to decode for each media item in the viewer: its proxy when
    /// proxies are on and one is ready, otherwise the original.
    pub fn decode_paths(&self) -> HashMap<MediaId, PathBuf> {
        self.project
            .media
            .iter()
            .map(|m| {
                let p = if self.settings.use_proxies { self.proxies.ready(&m.path).cloned() } else { None };
                (m.id, p.unwrap_or_else(|| m.path.clone()))
            })
            .collect()
    }

    // ---- Relinking -------------------------------------------------------------

    /// Point a missing media item at a file the user picks; then look in that
    /// folder for any other missing files with their original names.
    pub fn relink(&mut self, id: MediaId) {
        let Some(m) = self.project.media(id).cloned() else { return };
        let mut dlg = rfd::FileDialog::new().set_title(format!("Find {}", m.name)).add_filter("All files", &["*"]);
        if let Some(dir) = m.path.parent().filter(|d| d.is_dir()) {
            dlg = dlg.set_directory(dir);
        }
        let Some(new_path) = dlg.pick_file() else { return };
        let mut pairs = vec![(m.path.clone(), new_path.clone())];
        if let Some(dir) = new_path.parent() {
            let others: Vec<PathBuf> = reelforge_core::undo::missing_media(&self.project)
                .into_iter()
                .filter(|o| *o != id)
                .filter_map(|o| self.project.media(o).map(|m| m.path.clone()))
                .collect();
            pairs.extend(crate::safety::find_by_name(dir, &others));
        }
        self.apply_relinks(pairs);
    }

    /// Search a folder (and its subfolders) for every missing file by name.
    pub fn find_missing_in_folder(&mut self) {
        let Some(dir) = rfd::FileDialog::new().set_title("Folder to search for missing media").pick_folder() else { return };
        let missing: Vec<PathBuf> = reelforge_core::undo::missing_media(&self.project)
            .into_iter()
            .filter_map(|o| self.project.media(o).map(|m| m.path.clone()))
            .collect();
        let pairs = crate::safety::find_by_name(&dir, &missing);
        if pairs.is_empty() {
            self.status = format!("None of the missing files are in {}", dir.display());
        } else {
            self.apply_relinks(pairs);
        }
    }

    fn apply_relinks(&mut self, pairs: Vec<(PathBuf, PathBuf)>) {
        let mut cmds: Vec<Box<dyn Command>> = Vec::new();
        let mut problems = Vec::new();
        for (old_path, new_path) in pairs {
            let Some(old) = self.project.media.iter().find(|m| m.path == old_path).cloned() else { continue };
            match reelforge_media::probe(&new_path) {
                Ok(info) => {
                    if old.info.video.is_some() != info.video.is_some() || old.info.audio.is_some() != info.audio.is_some() {
                        problems.push(format!("{} has different streams from the original", new_path.display()));
                    }
                    let name = new_path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    let new = MediaAsset { id: old.id, path: new_path, name, info };
                    cmds.push(Box::new(reelforge_core::undo::RelinkMedia { id: old.id, old, new }));
                }
                Err(e) => problems.push(format!("{}: {e:#}", new_path.display())),
            }
        }
        let n = cmds.len();
        if n > 0 {
            self.execute(Box::new(Batch { name: "Relink media".into(), cmds }));
            self.preview.reset(); // reopen decoders on the new files
            let media: Vec<_> = self.project.media.clone();
            for m in &media {
                self.compute_waveform(m);
            }
        }
        let left = reelforge_core::undo::missing_media(&self.project).len();
        self.status = format!(
            "Relinked {n} file{}{}{}",
            if n == 1 { "" } else { "s" },
            if left > 0 { format!("; {left} still missing") } else { String::new() },
            if problems.is_empty() { String::new() } else { format!(" ({})", problems.join("; ")) }
        );
    }

    /// Modal dialogs: "save changes?" and crash recovery.
    pub fn dialogs(&mut self, ctx: &egui::Context) {
        if let Some(action) = self.pending.clone() {
            let name = self.project_path.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "the project".into());
            let mut choice = None;
            egui::Modal::new(egui::Id::new("unsaved")).show(ctx, |ui| {
                ui.set_width(380.0);
                ui.heading("Save changes?");
                ui.label(format!("{name} has changes that haven't been saved."));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button(egui::RichText::new("Save").strong()).clicked() {
                        choice = Some(0);
                    }
                    if ui.button("Don't save").clicked() {
                        choice = Some(1);
                    }
                    if ui.button("Cancel").clicked() {
                        choice = Some(2);
                    }
                });
            });
            match choice {
                Some(0) => {
                    self.pending = None;
                    if self.save_project(false) {
                        self.perform(action);
                    }
                }
                Some(1) => {
                    self.pending = None;
                    self.perform(action);
                }
                Some(2) => self.pending = None,
                _ => {}
            }
            if self.quit_confirmed {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        } else if !self.recoveries.is_empty() {
            let mut act: Option<(usize, bool)> = None;
            egui::Modal::new(egui::Id::new("recover")).show(ctx, |ui| {
                ui.set_width(460.0);
                ui.heading("Recover unsaved work?");
                ui.label("Reelforge didn't close properly last time. Its autosave can be opened:");
                ui.add_space(6.0);
                for (i, r) in self.recoveries.iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.label(r.describe());
                        if ui.button(egui::RichText::new("Recover").strong()).clicked() {
                            act = Some((i, true));
                        }
                        if ui.button("Discard").on_hover_text("Delete this autosave").clicked() {
                            act = Some((i, false));
                        }
                    });
                }
                ui.add_space(4.0);
                ui.weak("Autosaves are also in Edit > Settings > Open autosave folder.");
            });
            match act {
                Some((i, true)) => self.request(Pending::Recover(i)),
                Some((i, false)) => {
                    let r = self.recoveries.remove(i);
                    r.dismiss(false);
                }
                None => {}
            }
        }
    }

    fn menu_bar(&mut self, ui: &mut egui::Ui) {
        egui::MenuBar::new().ui(ui, |ui| {
            ui.menu_button("File", |ui| {
                if ui.button("New project").clicked() {
                    self.request(Pending::New);
                }
                if ui.button("Open project…").clicked() {
                    self.request(Pending::Open);
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
                ui.separator();
                if ui.button("Export…               Ctrl+M").clicked() {
                    self.export_requested = true;
                }
                if ui.button("Export queue…").clicked() {
                    self.export_queue_requested = true;
                }
                ui.separator();
                if ui.button("Open an autosave…").on_hover_text("Autosaves are made every few minutes (see Edit > Settings)").clicked() {
                    let dir = crate::safety::autosave_dir();
                    if let Some(p) = rfd::FileDialog::new().set_directory(&dir).add_filter("Reelforge project", &["rfproj"]).pick_file() {
                        self.request(Pending::OpenAutosave(p));
                    }
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
                let n = self.composite_candidates().len();
                let tip = if self.seq().in_point.is_some() || self.seq().out_point.is_some() {
                    "Moves every clip between the in and out marks into a new composite shot"
                } else {
                    "Moves the selected clip (and its linked audio) into a new composite shot. Set in/out marks to take several clips"
                };
                if ui.add_enabled(n > 0, egui::Button::new("Make composite shot   Ctrl+G")).on_hover_text(tip).on_disabled_hover_text(tip).clicked() {
                    self.make_composite();
                    ui.close();
                }
                if ui.button("New composite shot").clicked() {
                    self.new_composite();
                    ui.close();
                }
                ui.separator();
                if ui.button("Split at playhead     S").clicked() {
                    self.split_at_playhead();
                }
                let any = !self.selected_ids().is_empty();
                if ui.add_enabled(any, egui::Button::new("Cut                    Ctrl+X")).clicked() {
                    self.cut_selection();
                    ui.ctx().copy_text(format!("{CLIPBOARD_MARK} ({} clips)", self.clipboard.len()));
                }
                if ui.add_enabled(any, egui::Button::new("Copy                  Ctrl+C")).clicked() {
                    self.copy_selection();
                    ui.ctx().copy_text(format!("{CLIPBOARD_MARK} ({} clips)", self.clipboard.len()));
                }
                if ui.add_enabled(!self.clipboard.is_empty(), egui::Button::new("Paste at playhead  Ctrl+V")).clicked() {
                    self.paste();
                }
                if ui.button("Select all             Ctrl+A").clicked() {
                    self.select_all();
                }
                ui.separator();
                if ui.add_enabled(any, egui::Button::new("Delete clip          Del")).clicked() {
                    self.delete_selection();
                }
                if ui.add_enabled(self.selection.is_some(), egui::Button::new("Ripple delete   Shift+Del")).clicked() {
                    self.ripple_delete_selected();
                }
                if ui.add_enabled(self.selection_is_linked(), egui::Button::new("Unlink audio/video")).clicked() {
                    self.unlink_selection();
                }
                if ui.button("Freeze frame at playhead   Shift+F").on_hover_text("Holds the frame for 2 s and pushes the rest of the clip later").clicked() {
                    self.freeze_frame_at_playhead();
                    ui.close();
                }
                if ui.button("Add title at playhead        Ctrl+T").clicked() {
                    self.add_title();
                }
                ui.menu_button("Tracks", |ui| {
                    if ui.button("Add video track").clicked() {
                        self.add_track(TrackKind::Video);
                    }
                    if ui.button("Add audio track").clicked() {
                        self.add_track(TrackKind::Audio);
                    }
                    ui.weak("Right-click a track header to rename or delete it");
                });
                if ui.button("Cross-dissolve at playhead     Ctrl+D").clicked() {
                    self.toggle_transition_at_playhead(TrackKind::Video);
                }
                if ui.button("Audio crossfade at playhead   Ctrl+Shift+D").clicked() {
                    self.toggle_transition_at_playhead(TrackKind::Audio);
                }
                ui.separator();
                if ui.button("Mark in             I").clicked() {
                    self.mark_in();
                }
                if ui.button("Mark out            O").clicked() {
                    self.mark_out();
                }
                let marked = self.seq().in_point.is_some() || self.seq().out_point.is_some();
                if ui.add_enabled(marked, egui::Button::new("Clear in/out        Alt+X")).clicked() {
                    self.clear_marks();
                }
                ui.separator();
                if ui.button("Settings…").clicked() {
                    self.settings_open = true;
                }
                ui.checkbox(&mut self.playback.scrub_audio, "Audio scrubbing")
                    .on_hover_text("Hear audio while dragging the playhead or stepping frames");
            });
        });
    }

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        if ctx.egui_wants_keyboard_input() {
            return; // typing in a text field
        }
        let cmd = |k| KeyboardShortcut::new(Modifiers::COMMAND, k);
        let cmd_shift = |k| KeyboardShortcut::new(Modifiers::COMMAND | Modifiers::SHIFT, k);
        // Check Ctrl+Shift+D before Ctrl+D (egui matches Ctrl+D loosely otherwise).
        let crossfade = ctx.input_mut(|i| i.consume_shortcut(&cmd_shift(Key::D)));
        if crossfade {
            self.toggle_transition_at_playhead(TrackKind::Audio);
        }
        let dissolve = ctx.input_mut(|i| i.consume_shortcut(&cmd(Key::D)));
        if dissolve {
            self.toggle_transition_at_playhead(TrackKind::Video);
        }
        if ctx.input_mut(|i| i.consume_shortcut(&cmd(Key::T))) {
            self.add_title();
        }
        if ctx.input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(Modifiers::SHIFT, Key::F))) {
            self.freeze_frame_at_playhead();
        }
        if ctx.input_mut(|i| i.consume_shortcut(&cmd(Key::G))) {
            self.make_composite();
        }
        let (redo, undo, save, import, export) = ctx.input_mut(|i| {
            let redo = i.consume_shortcut(&cmd_shift(Key::Z)) || i.consume_shortcut(&cmd(Key::Y));
            (
                redo,
                i.consume_shortcut(&cmd(Key::Z)),
                i.consume_shortcut(&cmd(Key::S)),
                i.consume_shortcut(&cmd(Key::I)),
                i.consume_shortcut(&cmd(Key::M)),
            )
        });
        if export {
            self.export_requested = true;
        }
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
                Key::Delete | Key::Backspace => {
                    self.delete_selection();
                }
                Key::Escape if self.mask_draw.is_some() => {
                    self.mask_draw = None;
                    self.status = "Mask drawing cancelled".into();
                }
                Key::Enter if self.mask_draw.is_some() => self.finish_mask_drawing(),
                Key::Escape => self.select_only(None),
                Key::V => self.set_trim_mode(TrimMode::Normal),
                Key::B => self.set_trim_mode(TrimMode::Ripple),
                Key::N => self.set_trim_mode(TrimMode::Roll),
                Key::Y => self.set_trim_mode(TrimMode::Slip),
                Key::U => self.set_trim_mode(TrimMode::Slide),
                Key::J => self.playback.shuttle(-1.0, end),
                Key::K => {
                    self.playback.pause();
                    self.playback.shuttle = 1.0;
                }
                Key::L => self.playback.shuttle(1.0, end),
                Key::M => self.add_marker(),
                Key::ArrowUp => self.jump(false),
                Key::ArrowDown => self.jump(true),
                Key::I => self.mark_in(),
                Key::O => self.mark_out(),
                _ => {}
            }
        }
        let ripple_del = ctx.input(|i| {
            i.events.iter().any(|e| {
                matches!(e, egui::Event::Key { key: Key::Delete | Key::Backspace, pressed: true, modifiers, .. } if modifiers.shift && !modifiers.command && !modifiers.alt)
            })
        });
        // Clipboard keys arrive as Copy/Cut/Paste events, not key presses. (On
        // Windows, Shift+Del also arrives as Cut: that one is ripple delete.)
        let (copy, cut, paste, shift_del) = ctx.input(|i| {
            let mut r = (false, false, false, false);
            for e in &i.events {
                match e {
                    egui::Event::Copy => r.0 = true,
                    egui::Event::Cut if i.modifiers.shift && !i.modifiers.command => r.3 = true,
                    egui::Event::Cut => r.1 = true,
                    egui::Event::Paste(text) => r.2 = text.starts_with(CLIPBOARD_MARK),
                    _ => {}
                }
            }
            r
        });
        if ripple_del || shift_del {
            self.ripple_delete_selected();
        }
        if copy || cut {
            if cut {
                self.cut_selection();
            } else {
                self.copy_selection();
            }
            if !self.clipboard.is_empty() {
                // Paste only arrives when the system clipboard holds text, so leave a note there.
                ctx.copy_text(format!("{CLIPBOARD_MARK} ({} clips)", self.clipboard.len()));
            }
        }
        if paste {
            self.paste();
        }
        if ctx.input_mut(|i| i.consume_shortcut(&cmd(Key::A))) {
            self.select_all();
        }
        if ctx.input_mut(|i| i.consume_shortcut(&KeyboardShortcut::new(Modifiers::ALT, Key::X))) {
            self.clear_marks();
        }
    }

    pub fn set_trim_mode(&mut self, mode: TrimMode) {
        self.timeline.mode = mode;
        self.status = match mode {
            TrimMode::Normal => "Select tool (V): dragging an edge trims just that clip".into(),
            TrimMode::Ripple => "Ripple tool (B): trimming an edge moves everything after it".into(),
            TrimMode::Roll => "Roll tool (N): dragging a cut moves it between the two clips".into(),
            TrimMode::Slip => "Slip tool (Y): drag a clip to change which part of its media it shows".into(),
            TrimMode::Slide => "Slide tool (U): drag a clip to move it between its neighbours, which adjust to fit".into(),
        };
    }

    // ---- Viewer ----------------------------------------------------------------

    // ---- Interactive edits (sliders, gizmo, fade handles) ------------------------
    //
    // These change the project live while the mouse is held, then become a single
    // undo step when it's released (see `commit_live_edit`).

    /// Current transform, blend mode and keyframes of a clip.
    pub fn visual(&self, id: ClipId) -> Option<ClipVisual> {
        self.seq().find_clip(id).map(|(_, c)| c.visual())
    }

    pub fn clip_audio(&self, id: ClipId) -> Option<ClipAudio> {
        self.seq().find_clip(id).map(|(_, c)| c.audio.clone())
    }

    /// Start (or continue) a live edit; commits a different pending edit first.
    fn begin_live(&mut self, edit: LiveEdit) {
        match &self.live_edit {
            Some(pending) if pending.same_target(&edit) => {}
            Some(_) => {
                self.commit_live_edit();
                self.live_edit = Some(edit);
            }
            None => self.live_edit = Some(edit),
        }
    }

    pub fn set_visual_live(&mut self, id: ClipId, v: ClipVisual) {
        let Some(before) = self.visual(id) else { return };
        if before == v {
            return;
        }
        self.begin_live(LiveEdit::Visual(id, before));
        if let Some((_, c)) = self.project.sequences[self.active].find_clip_mut(id) {
            c.set_visual(v);
        }
    }

    /// Trim `edge` of `id` live in `mode` (normal, ripple or roll), `delta` from
    /// where the gesture started. `snapshot` holds the timings of every clip in
    /// the sequence from before the gesture: each call starts again from there,
    /// so the result never depends on the drag's history. Returns what was done.
    pub fn trim_live(&mut self, id: ClipId, edge: Edge, mode: TrimMode, delta: Time, snapshot: &[(ClipId, Timing)]) -> Option<TrimPlan> {
        self.begin_live(LiveEdit::Timing(id, snapshot.to_vec(), mode));
        self.restore_timings(snapshot);
        let plan = plan_trim(&self.project, self.active, id, edge, mode, delta);
        if let Some(plan) = &plan {
            plan.apply(&mut self.project, self.active);
        }
        self.touch();
        plan
    }

    /// Timings of every clip in the sequence (a snapshot to restore during a live trim).
    pub fn all_timings(&self) -> Vec<(ClipId, Timing)> {
        self.seq().tracks.iter().flat_map(|t| &t.clips).map(|c| (c.id, (c.timeline_start, c.source_in, c.duration))).collect()
    }

    /// Delete the selected clip (and its linked clips) and close the gap it leaves.
    pub fn ripple_delete_selected(&mut self) {
        self.commit_live_edit();
        let Some(id) = self.selection else { return };
        if self.seq().is_locked(id) {
            self.status = LOCKED.into();
            return;
        }
        match ripple_delete(&self.project, self.active, id) {
            Some((cmd, closed)) => {
                self.execute(cmd);
                self.select_only(None);
                let rate = self.seq().frame_rate;
                self.status = if closed > Time::ZERO {
                    format!("Ripple delete: closed a {} gap", closed.timecode(rate))
                } else {
                    "Ripple delete: deleted (another clip spans the gap, so nothing could move up)".into()
                };
            }
            None => self.status = "Nothing to delete".into(),
        }
    }

    fn restore_timings(&mut self, start: &[(ClipId, Timing)]) {
        for (cid, (a, b, c)) in start {
            if let Some((ti, clip)) = self.project.sequences[self.active].find_clip_mut(*cid) {
                (clip.timeline_start, clip.source_in, clip.duration) = (*a, *b, *c);
                self.project.sequences[self.active].tracks[ti].sort();
            }
        }
    }

    pub fn set_dissolve_live(&mut self, id: ClipId, d: Time) {
        let Some(before) = self.seq().find_clip(id).map(|(_, c)| c.dissolve_in) else { return };
        if before == d {
            return;
        }
        self.begin_live(LiveEdit::Dissolve(id, before));
        if let Some((_, c)) = self.project.sequences[self.active].find_clip_mut(id) {
            c.dissolve_in = d;
        }
    }

    pub fn set_clip_audio_live(&mut self, id: ClipId, a: ClipAudio) {
        let Some(before) = self.clip_audio(id) else { return };
        if before == a {
            return;
        }
        self.begin_live(LiveEdit::ClipAudio(id, before));
        if let Some((_, c)) = self.project.sequences[self.active].find_clip_mut(id) {
            c.audio = a;
        }
        self.touch(); // the mixer hears it immediately
    }

    pub fn set_track_audio_live(&mut self, track: usize, a: TrackAudio) {
        let before = self.seq().tracks[track].audio;
        if before == a {
            return;
        }
        self.begin_live(LiveEdit::TrackAudio(track, before));
        self.project.sequences[self.active].tracks[track].audio = a;
        self.touch();
    }

    /// Turn the pending live edit into one undoable command.
    pub fn commit_live_edit(&mut self) {
        let Some(edit) = self.live_edit.take() else { return };
        let cmd: Option<Box<dyn Command>> = match edit {
            LiveEdit::Visual(id, old) => self.visual(id).filter(|n| *n != old).map(|new| Box::new(SetClipVisual { seq: self.active, id, old, new }) as _),
            LiveEdit::Dissolve(id, old) => {
                let new = self.seq().find_clip(id).map(|(_, c)| c.dissolve_in).filter(|n| *n != old);
                new.map(|new| Box::new(SetDissolve { seq: self.active, id, old, new }) as _)
            }
            LiveEdit::Timing(_, start, mode) => {
                let cmds: Vec<Box<dyn Command>> = start
                    .into_iter()
                    .filter_map(|(cid, old)| {
                        let (_, c) = self.seq().find_clip(cid)?;
                        let new = (c.timeline_start, c.source_in, c.duration);
                        (new != old).then(|| Box::new(SetClipTiming { seq: self.active, id: cid, old, new }) as Box<dyn Command>)
                    })
                    .collect();
                if cmds.is_empty() {
                    None
                } else {
                    // The project already has the new timings: apply() re-sets them, which is harmless.
                    Some(Box::new(Batch { name: mode.undo_name().into(), cmds }) as _)
                }
            }
            LiveEdit::Title(id, old) => {
                let new = self.seq().find_clip(id).and_then(|(_, c)| c.title.clone()).filter(|n| *n != old);
                new.map(|new| Box::new(SetTitle { seq: self.active, id, old, new }) as _)
            }
            LiveEdit::Markers(old) => {
                let mut new = self.seq().markers.clone();
                new.sort_by_key(|m| m.at);
                self.project.sequences[self.active].markers = new.clone();
                (new != old).then(|| Box::new(reelforge_core::undo::SetMarkers { seq: self.active, old, new }) as _)
            }
            LiveEdit::Tracker(id, old) => {
                let new = self.seq().find_clip(id).map(|(_, c)| c.tracker.clone()).filter(|n| *n != old);
                new.map(|new| Box::new(SetTracker { seq: self.active, id, old, new, label: "Move tracker" }) as _)
            }
            LiveEdit::ClipAudio(id, old) => self.clip_audio(id).filter(|n| *n != old).map(|new| Box::new(SetClipAudio { seq: self.active, id, old, new }) as _),
            LiveEdit::TrackAudio(track, old) => {
                let new = self.seq().tracks.get(track).map(|t| t.audio).filter(|n| *n != old);
                new.map(|new| Box::new(SetTrackAudio { seq: self.active, track, old, new }) as _)
            }
        };
        if let Some(cmd) = cmd {
            self.execute(cmd);
        }
    }
}

/// An interactive edit in progress: what's being changed and its value before the gesture.
pub enum TrackMsg {
    Progress(f32),
    Done(Result<(Vec<reelforge_core::track::TrackSample>, reelforge_media::TrackEnd), String>),
}

pub struct TrackJob {
    seq: usize,
    pub clip: ClipId,
    rx: Receiver<TrackMsg>,
    cancel: Arc<std::sync::atomic::AtomicBool>,
    pub progress: f32,
}

enum LiveEdit {
    Visual(ClipId, ClipVisual),
    /// Trimming a clip (keyed by the clip grabbed): timings of every clip before, and the trim mode.
    Timing(ClipId, Vec<(ClipId, Timing)>, TrimMode),
    Dissolve(ClipId, Time),
    Title(ClipId, TitleSpec),
    Tracker(ClipId, Option<PointTrack>),
    Markers(Vec<reelforge_core::Marker>),
    ClipAudio(ClipId, ClipAudio),
    TrackAudio(usize, TrackAudio),
}

impl LiveEdit {
    fn same_target(&self, other: &LiveEdit) -> bool {
        match (self, other) {
            (LiveEdit::Visual(a, _), LiveEdit::Visual(b, _)) => a == b,
            (LiveEdit::Timing(a, ..), LiveEdit::Timing(b, ..)) => a == b,
            (LiveEdit::Dissolve(a, _), LiveEdit::Dissolve(b, _)) => a == b,
            (LiveEdit::Title(a, _), LiveEdit::Title(b, _)) => a == b,
            (LiveEdit::Tracker(a, _), LiveEdit::Tracker(b, _)) => a == b,
            (LiveEdit::Markers(_), LiveEdit::Markers(_)) => true,
            (LiveEdit::ClipAudio(a, _), LiveEdit::ClipAudio(b, _)) => a == b,
            (LiveEdit::TrackAudio(a, _), LiveEdit::TrackAudio(b, _)) => a == b,
            _ => false,
        }
    }
}

/// Start of the text left on the system clipboard when clips are copied
/// (egui only reports Ctrl+V when the clipboard holds text).
pub const CLIPBOARD_MARK: &str = "Reelforge clips";

/// Status message when an edit is refused because of a locked track.
pub const LOCKED: &str = "That clip is on a locked track (or linked to one). Unlock the track to edit it";

fn load_project(path: &Path) -> anyhow::Result<Project> {
    Ok(serde_json::from_str(&std::fs::read_to_string(path)?)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn editor() -> EditorState {
        EditorState::new(Playback::with_audio(None), Preview::new(None))
    }

    /// A small real video file made with the ffmpeg CLI (tests skip without it).
    fn clip(dir: &Path, name: &str) -> Option<PathBuf> {
        let p = dir.join(name);
        let ok = std::process::Command::new("ffmpeg")
            .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i", "testsrc2=size=160x90:rate=25:duration=1", "-pix_fmt", "yuv420p"])
            .arg(&p)
            .status()
            .is_ok_and(|s| s.success());
        ok.then_some(p)
    }

    #[test]
    fn relinking_points_media_at_the_moved_files_and_undoes() {
        let dir = std::env::temp_dir().join(format!("rf-app-relink-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("new/sub")).unwrap();
        let (Some(a), Some(b)) = (clip(&dir.join("new"), "a.mp4"), clip(&dir.join("new/sub"), "b.mp4")) else { return };
        let mut st = editor();
        let info = reelforge_media::probe(&a).unwrap();
        for (id, name) in [(1, "a.mp4"), (2, "b.mp4")] {
            st.project.media.push(MediaAsset { id: MediaId(id), path: dir.join("old").join(name), name: name.into(), info: info.clone() });
        }
        assert_eq!(reelforge_core::undo::missing_media(&st.project).len(), 2);
        // Relinking one file finds the other in the same folder tree by name.
        let mut pairs = vec![(dir.join("old/a.mp4"), a.clone())];
        pairs.extend(crate::safety::find_by_name(&dir.join("new"), &[dir.join("old/b.mp4")]));
        st.apply_relinks(pairs);
        assert!(reelforge_core::undo::missing_media(&st.project).is_empty(), "{}", st.status);
        assert_eq!(st.project.media(MediaId(2)).unwrap().path, b);
        assert!(st.status.starts_with("Relinked 2 files"));
        st.undo();
        assert_eq!(reelforge_core::undo::missing_media(&st.project).len(), 2, "one undo step");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn edits_are_unsaved_until_saved() {
        let dir = std::env::temp_dir().join(format!("rf-app-dirty-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut st = editor();
        assert!(!st.is_dirty(), "a new project has nothing to save");
        st.add_track(TrackKind::Video);
        assert!(st.is_dirty());
        st.project_path = Some(dir.join("p.rfproj"));
        assert!(st.save_project(false));
        assert!(!st.is_dirty());
        let saved: Project = serde_json::from_str(&std::fs::read_to_string(dir.join("p.rfproj")).unwrap()).unwrap();
        assert_eq!(saved.sequences[0].tracks.len(), 4);
        // Asking for a new project with unsaved changes waits for an answer.
        st.add_title();
        st.request(Pending::New);
        assert_eq!(st.pending, Some(Pending::New));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
