//! The editor model: a project being edited, with its undo history,
//! selection, playback and viewer picture, and the edits a user interface
//! asks for. Nothing here draws anything: an interface reads the state and
//! calls these methods (the Tauri app does so through its commands).
//!
//! Edits made while a mouse button is held (dragging a slider, trimming a
//! clip) change the project live and become a single undo step when the
//! gesture ends: see [`Editor::commit_live_edit`].

use crate::audio::AudioEngine;
use crate::playback::Playback;
use crate::preview::Preview;
use crate::render_cache::RenderCache;
use crossbeam_channel::{unbounded, Receiver, Sender};
use reelforge_core::edit::{plan_trim, ripple_delete, TrimPlan};
use reelforge_core::mask::Mask;
use reelforge_core::track::{ApplyOptions, TrackMode, Tracker};
use reelforge_core::undo::{
    split_group, AddClip, AddTrack, Batch, Edge, ImportMedia, RemoveTrack, RenameTrack, SetClipAudio, SetClipTiming, SetClipVisual, SetLink,
    SetTitle, SetTrackAudio, SetTracker, Timing,
};
use reelforge_core::*;
use reelforge_render::text::FontEntry;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Waveform resolution: peaks per second of audio.
pub const WAVEFORM_RATE: u32 = 100;

/// Status message when an edit is refused because of a locked track.
pub const LOCKED: &str = "That clip is on a locked track (or linked to one). Unlock the track to edit it";

/// File types offered when importing.
pub const MEDIA_EXTENSIONS: [&str; 17] = ["mp4", "mov", "mkv", "avi", "webm", "m4v", "mxf", "mts", "png", "jpg", "jpeg", "gif", "wav", "mp3", "aac", "flac", "ogg"];

/// Choices that change how the editor behaves (kept with the app's settings).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Prefs {
    /// Decode proxies in the viewer when they're ready.
    pub use_proxies: bool,
    /// Make proxies for big footage by themselves.
    pub auto_proxies: bool,
    /// Autosave every this many minutes (0 = never).
    pub autosave_minutes: u32,
    /// Size limit of the render cache.
    pub render_cache_mb: usize,
    /// Export settings saved under a name.
    pub export_presets: Vec<crate::presets::UserPreset>,
}

impl Default for Prefs {
    fn default() -> Self {
        Self { use_proxies: true, auto_proxies: true, autosave_minutes: 2, render_cache_mb: 2048, export_presets: Vec::new() }
    }
}

/// Messages from a tracking run.
enum TrackMsg {
    Progress(f32),
    Done(Result<(Vec<Vec<reelforge_core::track::TrackSample>>, reelforge_media::TrackEnd), String>),
}

/// A motion-tracking run going on in the background.
pub struct TrackJob {
    seq: usize,
    pub clip: ClipId,
    rx: Receiver<TrackMsg>,
    cancel: Arc<std::sync::atomic::AtomicBool>,
    /// 0–1.
    pub progress: f32,
}

/// An interactive edit in progress: what's being changed and its value before the gesture.
enum LiveEdit {
    Tracker(ClipId, Option<Tracker>),
    Visual(ClipId, ClipVisual),
    /// Trimming a clip (keyed by the clip grabbed): timings of every clip before, and the trim mode.
    Timing(ClipId, Vec<(ClipId, Timing)>, TrimMode),
    Title(ClipId, TitleSpec),
    ClipAudio(ClipId, ClipAudio),
    TrackAudio(usize, TrackAudio),
    /// The sequence's markers (dragging one, typing its name).
    Markers(Vec<Marker>),
}

impl LiveEdit {
    fn same_target(&self, other: &LiveEdit) -> bool {
        match (self, other) {
            (LiveEdit::Visual(a, _), LiveEdit::Visual(b, _)) => a == b,
            (LiveEdit::Timing(a, ..), LiveEdit::Timing(b, ..)) => a == b,
            (LiveEdit::Title(a, _), LiveEdit::Title(b, _)) => a == b,
            (LiveEdit::ClipAudio(a, _), LiveEdit::ClipAudio(b, _)) => a == b,
            (LiveEdit::TrackAudio(a, _), LiveEdit::TrackAudio(b, _)) => a == b,
            (LiveEdit::Tracker(a, _), LiveEdit::Tracker(b, _)) => a == b,
            (LiveEdit::Markers(_), LiveEdit::Markers(_)) => true,
            _ => false,
        }
    }
}

pub struct Editor {
    pub project: Project,
    pub undo: UndoStack,
    pub playback: Playback,
    /// The clip the Controls panel shows (the last one clicked).
    pub selection: Option<ClipId>,
    /// Every selected clip, including `selection`.
    pub selected: Vec<ClipId>,
    pub clipboard: reelforge_core::clipboard::Clipboard,
    /// The last thing that happened, for the status bar.
    pub status: String,
    pub project_path: Option<PathBuf>,
    /// Decoders + GPU compositor producing the viewer picture.
    pub preview: Preview,
    /// Finished frames from Render preview.
    pub render_cache: RenderCache,
    live_edit: Option<LiveEdit>,
    pub meters: crate::meters::Meters,
    /// The sequence shown in the timeline and viewer (0 = the main edit).
    pub active: usize,
    /// The composition last opened (0 = none yet).
    pub comp_open: usize,
    /// Where the playhead was in each sequence, to come back to.
    playheads: HashMap<usize, Time>,
    /// Bumped on every project change.
    pub rev: u64,
    mixed_rev: u64,
    /// Project revision at the last save (or open): anything newer is unsaved.
    pub saved_rev: u64,
    /// Audio peak envelopes for drawing (`WAVEFORM_RATE` per second), computed in the background.
    pub waveforms: HashMap<MediaId, Arc<Vec<f32>>>,
    waveform_tx: Sender<(MediaId, Vec<f32>)>,
    waveform_rx: Receiver<(MediaId, Vec<f32>)>,
    /// Installed fonts for titles, found in the background (`None` until then).
    fonts: Arc<Mutex<Option<Arc<Vec<FontEntry>>>>>,
    pub prefs: Prefs,
    pub proxies: crate::proxies::Proxies,
    proxied_rev: u64,
    /// This run's autosave/crash-detection session.
    pub session: Option<crate::safety::Session>,
    /// Crashed sessions found at startup, offered for recovery.
    pub recoveries: Vec<crate::safety::Recovery>,
    /// A motion-tracking run in progress.
    pub tracking: Option<TrackJob>,
}

impl Editor {
    /// An editor with an empty project. `gpu` draws the viewer picture; `audio` plays sound.
    pub fn new(gpu: Option<(reelforge_render::wgpu::Device, reelforge_render::wgpu::Queue)>, audio: Option<AudioEngine>) -> Self {
        let fonts = Arc::new(Mutex::new(None));
        let fill = fonts.clone();
        std::thread::spawn(move || {
            let found = Arc::new(reelforge_render::text::system_fonts());
            log::info!("found {} font faces", found.len());
            *fill.lock().unwrap() = Some(found);
        });
        Self::blank(Playback::with_audio(audio), Preview::new(gpu), fonts)
    }

    fn blank(playback: Playback, preview: Preview, fonts: Arc<Mutex<Option<Arc<Vec<FontEntry>>>>>) -> Self {
        let (waveform_tx, waveform_rx) = unbounded();
        let prefs = Prefs::default();
        Self {
            project: Project::new(),
            undo: UndoStack::default(),
            playback,
            selection: None,
            selected: Vec::new(),
            clipboard: Default::default(),
            status: String::new(),
            project_path: None,
            preview,
            render_cache: RenderCache::new(prefs.render_cache_mb),
            live_edit: None,
            meters: Default::default(),
            active: 0,
            comp_open: 0,
            playheads: HashMap::new(),
            rev: 1,
            mixed_rev: 0,
            saved_rev: 1,
            waveforms: HashMap::new(),
            waveform_tx,
            waveform_rx,
            fonts,
            prefs,
            proxies: Default::default(),
            proxied_rev: 0,
            session: None,
            recoveries: Vec::new(),
            tracking: None,
        }
    }

    /// Start the autosave / crash-detection session; returns crashed sessions found.
    pub fn start_session(&mut self) {
        let (session, crashed) = crate::safety::Session::start();
        self.session = Some(session);
        self.recoveries = crashed;
    }

    /// Replace the project with `project` (or a new one), keeping the audio
    /// device, GPU, fonts, settings and session.
    fn replace_project(&mut self, project: Project, path: Option<PathBuf>) {
        self.playback.pause();
        let mut preview = std::mem::replace(&mut self.preview, Preview::new(None));
        preview.reset();
        let mut fresh = Editor::blank(Playback::with_audio(self.playback.audio.take()), preview, self.fonts.clone());
        fresh.playback.scrub_audio = self.playback.scrub_audio;
        fresh.prefs = self.prefs.clone();
        fresh.render_cache.set_budget_mb(fresh.prefs.render_cache_mb);
        fresh.proxies = std::mem::take(&mut self.proxies);
        fresh.session = self.session.take();
        fresh.recoveries = std::mem::take(&mut self.recoveries);
        fresh.project = project;
        fresh.project_path = path.clone();
        if let Some(s) = &mut fresh.session {
            s.set_project(path);
            s.autosaved_rev = fresh.rev;
        }
        *self = fresh;
    }

    /// File → New.
    pub fn new_project(&mut self) {
        self.replace_project(Project::new(), None);
        self.status = "New project".into();
    }

    /// Mark the project as changed.
    pub fn touch(&mut self) {
        self.rev += 1;
    }

    /// Are there changes since the last save?
    pub fn is_dirty(&self) -> bool {
        self.rev != self.saved_rev
    }

    pub fn seq(&self) -> &Sequence {
        &self.project.sequences[self.active]
    }

    /// Show sequence `i` (the main edit, or a composition) in the timeline and viewer.
    pub fn open_sequence(&mut self, i: usize) {
        if i >= self.project.sequences.len() {
            return;
        }
        if i > 0 {
            self.comp_open = i;
        }
        if i == self.active {
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

    // ---- Undo ----------------------------------------------------------------

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
        self.touch();
        self.validate_selection();
    }

    pub fn redo(&mut self) {
        self.commit_live_edit();
        match self.undo.redo(&mut self.project) {
            Some(n) => self.status = format!("Redo: {n}"),
            None => self.status = "Nothing to redo".into(),
        }
        self.touch();
        self.validate_selection();
    }

    /// Jump through the history to the state after `pos` steps (0 = before the first).
    pub fn history_go_to(&mut self, pos: usize) {
        self.commit_live_edit();
        let n = self.undo.go_to(&mut self.project, pos);
        if n > 0 {
            self.status = format!("History: step {}", self.undo.position());
            self.touch();
            self.validate_selection();
        }
    }

    fn validate_selection(&mut self) {
        // Undoing "Make composition" can remove the sequence being shown.
        if self.active >= self.project.sequences.len() {
            self.active = 0;
            self.select_only(None);
        }
        if self.comp_open >= self.project.sequences.len() {
            self.comp_open = self.project.sequences.len() - 1;
        }
        let ids = self.selected_ids();
        self.selected = ids;
        if self.selection.is_some_and(|id| self.seq().find_clip(id).is_none()) {
            self.selection = self.selected.last().copied();
        }
    }

    // ---- Media ----------------------------------------------------------------

    /// Import media files (each becomes one undo step).
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

    /// How long a still image lasts when placed.
    pub fn still_length() -> Time {
        Time::from_secs_f64(5.0)
    }

    fn clip_duration(media: &MediaAsset) -> Time {
        if media.info.duration > Time::ZERO { media.info.duration } else { Self::still_length() }
    }

    /// Place media on the timeline at `at` (or after existing clips if that overlaps).
    /// Footage with sound becomes a linked pair of clips.
    pub fn add_to_timeline(&mut self, media_id: MediaId, track: Option<usize>, at: Option<Time>) {
        self.commit_live_edit();
        let Some(media) = self.project.media(media_id).cloned() else { return };
        // First clip in an empty sequence: match the sequence to the footage.
        if self.seq().duration() == Time::ZERO {
            if let Some(v) = &media.info.video {
                let s = &mut self.project.sequences[self.active];
                (s.width, s.height, s.frame_rate) = (v.width, v.height, v.frame_rate);
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
            cmds.push(Box::new(AddClip { seq: self.active, track, clip: Clip::new(id, media_id, start, Time::ZERO, duration, link) }));
        }
        self.execute(Box::new(Batch { name: "Add clip".into(), cmds }));
        self.select_only(first);
    }

    /// Decode a peak envelope for drawing, off the calling thread.
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

    /// The file to decode for each media item in the viewer: its proxy when
    /// proxies are on and one is ready, otherwise the original.
    pub fn decode_paths(&self) -> HashMap<MediaId, PathBuf> {
        self.project
            .media
            .iter()
            .map(|m| {
                let p = if self.prefs.use_proxies { self.proxies.ready(&m.path).cloned() } else { None };
                (m.id, p.unwrap_or_else(|| m.path.clone()))
            })
            .collect()
    }

    // ---- Selection ----------------------------------------------------------------

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

    /// Select exactly `ids` (a box selection), the first being the one the Controls show.
    pub fn select_many(&mut self, ids: Vec<ClipId>) {
        self.selection = ids.first().copied();
        self.selected = ids;
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

    /// Selected clips and everything linked to them.
    pub fn selected_with_links(&self) -> Vec<ClipId> {
        reelforge_core::undo::expand_links(&self.project, self.active, &self.selected_ids())
    }

    pub fn select_all(&mut self) {
        let ids: Vec<ClipId> = self.seq().tracks.iter().flat_map(|t| &t.clips).map(|c| c.id).collect();
        self.status = format!("Selected {} clip{}", ids.len(), if ids.len() == 1 { "" } else { "s" });
        self.select_many(ids);
    }

    // ---- Timeline edits ----------------------------------------------------------

    /// Split the selected clip (or the top one) at the playhead.
    pub fn split_at_playhead(&mut self) {
        self.commit_live_edit();
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

    /// Move the selection (led by `lead`, which goes to `track` at `start`).
    pub fn move_clips(&mut self, lead: ClipId, track: usize, start: Time) {
        self.commit_live_edit();
        let mut sel = self.selected_ids();
        if !sel.contains(&lead) {
            sel = vec![lead];
        }
        let start = start.max(Time::ZERO).snap(self.seq().frame_rate);
        match reelforge_core::undo::move_clips(&self.project, self.active, lead, &sel, (track, start)) {
            Ok(cmd) => self.execute(cmd),
            Err(e) => self.status = format!("Can't move there: {e}"),
        }
    }

    /// Delete every selected clip. Returns whether anything was deleted.
    pub fn delete_selection(&mut self) -> bool {
        self.commit_live_edit();
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
                self.status = format!("Ripple delete: closed a {} gap", closed.timecode(rate));
            }
            None => self.status = "Nothing to delete".into(),
        }
    }

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
            self.status = "Nothing to paste: copy some clips first".into();
            return;
        }
        let t = self.playback.playhead.snap(self.seq().frame_rate);
        let cb = self.clipboard.clone();
        match reelforge_core::clipboard::paste_clips(&mut self.project, self.active, &cb, t) {
            Some(p) => {
                let n = p.ids.len();
                self.execute(p.cmd);
                self.select_many(p.ids);
                self.status = format!("Pasted {n} clip{}", if n == 1 { "" } else { "s" });
            }
            None => self.status = "Can't paste here (a locked track, or a composition inside itself)".into(),
        }
    }

    /// Move the selected clips `frames` frames along the timeline (negative = earlier).
    pub fn nudge(&mut self, frames: i64) {
        self.commit_live_edit();
        let ids = self.selected_ids();
        let by = Time(self.seq().frame_rate.frame_duration().0 * frames);
        match reelforge_core::undo::nudge_clips(&self.project, self.active, &ids, by) {
            Ok(cmd) => self.execute(cmd),
            Err(e) => self.status = format!("Can't nudge: {e}"),
        }
    }

    pub fn unlink_selection(&mut self) {
        let Some(id) = self.selection else { return };
        let ids: Vec<ClipId> = self.seq().link_group(id).iter().map(|(_, c)| c.id).collect();
        if ids.len() > 1 {
            self.execute(Box::new(SetLink::new(self.active, ids, None)));
        }
    }

    /// Set the speed / direction of `id` (and its linked clips).
    pub fn set_speed(&mut self, id: ClipId, speed: f32, reverse: bool) {
        self.commit_live_edit();
        match reelforge_core::speed::change_speed(&self.project, self.active, id, speed, reverse) {
            Ok(cmd) => self.execute(cmd),
            Err(e) => self.status = format!("Can't change the speed: {e}"),
        }
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
                self.execute(Box::new(cmd));
                self.validate_selection();
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

    pub fn toggle_mute(&mut self, track: usize) {
        let Some(tr) = self.project.sequences[self.active].tracks.get_mut(track) else { return };
        tr.muted = !tr.muted;
        self.status = format!("{} {}", tr.name, if tr.muted { "muted" } else { "unmuted" });
        self.touch();
    }

    pub fn toggle_lock(&mut self, track: usize) {
        self.commit_live_edit();
        let Some(tr) = self.project.sequences[self.active].tracks.get_mut(track) else { return };
        tr.locked = !tr.locked;
        self.status = format!("{} {}", tr.name, if tr.locked { "locked" } else { "unlocked" });
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

    /// The out point goes after the frame under the playhead, so that frame is included.
    pub fn mark_out(&mut self) {
        let s = &mut self.project.sequences[self.active];
        let rate = s.frame_rate;
        let t = self.playback.playhead.snap(rate) + rate.frame_duration();
        s.out_point = Some(t);
        if s.in_point.is_some_and(|i| i >= t) {
            s.in_point = None;
        }
        self.status = format!("Out point: {}", t.timecode(rate));
        self.touch();
    }

    pub fn clear_marks(&mut self) {
        let s = &mut self.project.sequences[self.active];
        (s.in_point, s.out_point) = (None, None);
        self.status = "In/out points cleared".into();
        self.touch();
    }

    // ---- Titles and compositions ---------------------------------------------------

    /// Installed fonts, once the background scan has finished.
    pub fn fonts(&self) -> Option<Arc<Vec<FontEntry>>> {
        self.fonts.lock().unwrap().clone()
    }

    /// Add a 5 s title at the playhead on the top video track with room, and select it.
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
        let free = (0..s.tracks.len()).rev().find(|&t| s.tracks[t].kind == TrackKind::Video && !s.tracks[t].locked && !s.overlaps(t, start, dur, &[]));
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

    /// A new, empty composition the size of the current sequence, opened for editing.
    pub fn new_composite(&mut self) {
        self.commit_live_edit();
        let s = self.seq();
        let seq = Sequence::new(reelforge_core::undo::next_composite_name(&self.project), s.width, s.height, s.frame_rate);
        let cmd = reelforge_core::undo::AddSequence::new(&self.project, seq);
        let index = cmd.index;
        self.execute(Box::new(cmd));
        self.open_sequence(index);
    }

    /// "New composition from media": a composition shaped like the media, holding it, opened.
    pub fn new_composition_from_media(&mut self, media: MediaId) {
        self.commit_live_edit();
        let fallback = self.project.sequences[0].clone();
        match reelforge_core::undo::composition_from_media(&mut self.project, media, &fallback, Self::still_length()) {
            Ok(cmd) => {
                let index = cmd.index;
                self.execute(Box::new(Batch { name: "New composition from media".into(), cmds: vec![Box::new(cmd)] }));
                self.open_sequence(index);
                let s = self.seq();
                self.status = format!("New composition {} ({}×{}, {:.3} fps)", s.name, s.width, s.height, s.frame_rate.fps());
            }
            Err(e) => self.status = e,
        }
    }

    /// Put composition `index` into the current sequence at the playhead.
    pub fn add_composite_to_timeline(&mut self, index: usize) {
        self.commit_live_edit();
        if self.project.contains_sequence(index, self.active) {
            self.status = "A composition can't contain itself".into();
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
        self.execute(Box::new(Batch { name: "Add composition".into(), cmds }));
        self.select_only(first);
    }

    // ---- Live edits (sliders, dragging) ---------------------------------------------

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
        self.touch();
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
        self.touch();
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
        self.touch();
    }

    pub fn set_track_audio_live(&mut self, track: usize, a: TrackAudio) {
        let Some(before) = self.seq().tracks.get(track).map(|t| t.audio) else { return };
        if before == a {
            return;
        }
        self.begin_live(LiveEdit::TrackAudio(track, before));
        self.project.sequences[self.active].tracks[track].audio = a;
        self.touch();
    }

    /// Timings of every clip in the sequence (a snapshot to restore during a live trim).
    pub fn all_timings(&self) -> Vec<(ClipId, Timing)> {
        self.seq().tracks.iter().flat_map(|t| &t.clips).map(|c| (c.id, (c.timeline_start, c.source_in, c.duration))).collect()
    }

    fn restore_timings(&mut self, start: &[(ClipId, Timing)]) {
        for (cid, (a, b, c)) in start {
            if let Some((ti, clip)) = self.project.sequences[self.active].find_clip_mut(*cid) {
                (clip.timeline_start, clip.source_in, clip.duration) = (*a, *b, *c);
                self.project.sequences[self.active].tracks[ti].sort();
            }
        }
    }

    /// Trim `edge` of `id` live in `mode`, `delta` from where the gesture started.
    /// Every call starts again from the timings before the gesture began, so
    /// the result never depends on the drag's history.
    pub fn trim_live(&mut self, id: ClipId, edge: Edge, mode: TrimMode, delta: Time) -> Option<TrimPlan> {
        let snapshot = match &self.live_edit {
            Some(LiveEdit::Timing(lead, snap, m)) if *lead == id && *m == mode => snap.clone(),
            _ => self.all_timings(),
        };
        if self.seq().is_locked(id) {
            self.status = LOCKED.into();
            return None;
        }
        self.begin_live(LiveEdit::Timing(id, snapshot.clone(), mode));
        self.restore_timings(&snapshot);
        let plan = plan_trim(&self.project, self.active, id, edge, mode, delta);
        if let Some(plan) = &plan {
            plan.apply(&mut self.project, self.active);
        }
        self.touch();
        plan
    }

    /// Turn the pending live edit into one undoable command.
    pub fn commit_live_edit(&mut self) {
        let Some(edit) = self.live_edit.take() else { return };
        let seq = self.active;
        let cmd: Option<Box<dyn Command>> = match edit {
            LiveEdit::Visual(id, old) => self.visual(id).filter(|n| *n != old).map(|new| Box::new(SetClipVisual { seq, id, old, new }) as _),
            LiveEdit::Timing(_, start, mode) => {
                let cmds: Vec<Box<dyn Command>> = start
                    .into_iter()
                    .filter_map(|(cid, old)| {
                        let (_, c) = self.seq().find_clip(cid)?;
                        let new = (c.timeline_start, c.source_in, c.duration);
                        (new != old).then(|| Box::new(SetClipTiming { seq, id: cid, old, new }) as Box<dyn Command>)
                    })
                    .collect();
                (!cmds.is_empty()).then(|| Box::new(Batch { name: mode.undo_name().into(), cmds }) as _)
            }
            LiveEdit::Title(id, old) => {
                let new = self.seq().find_clip(id).and_then(|(_, c)| c.title.clone()).filter(|n| *n != old);
                new.map(|new| Box::new(SetTitle { seq, id, old, new }) as _)
            }
            LiveEdit::ClipAudio(id, old) => self.clip_audio(id).filter(|n| *n != old).map(|new| Box::new(SetClipAudio { seq, id, old, new }) as _),
            LiveEdit::Tracker(id, old) => {
                let new = self.seq().find_clip(id).map(|(_, c)| c.tracker.clone()).filter(|n| *n != old);
                new.map(|new| Box::new(SetTracker { seq, id, old, new, label: "Move tracker" }) as _)
            }
            LiveEdit::TrackAudio(track, old) => {
                let new = self.seq().tracks.get(track).map(|t| t.audio).filter(|n| *n != old);
                new.map(|new| Box::new(SetTrackAudio { seq, track, old, new }) as _)
            }
            LiveEdit::Markers(old) => {
                let new = self.seq().markers.clone();
                (new != old).then(|| Box::new(reelforge_core::undo::SetMarkers { seq, old, new }) as _)
            }
        };
        if let Some(cmd) = cmd {
            self.execute(cmd);
        }
    }

    // ---- Keyframes ------------------------------------------------------------------

    /// The clip's source time at the playhead (held at the clip's ends) and
    /// whether the playhead is on the clip (keyframes can only be set there).
    pub fn key_time(&self, clip: &Clip) -> (Time, bool) {
        let ph = self.playback.playhead;
        (clip.source_time(ph.max(clip.timeline_start).min(clip.timeline_end())), clip.contains(ph))
    }

    /// Change a clip's transform as it's seen at the playhead (a live edit):
    /// keyframed properties get a key there, the others change for the whole clip.
    pub fn set_transform_live(&mut self, id: ClipId, edited: Transform) {
        let Some((_, c)) = self.seq().find_clip(id) else { return };
        let (src, on_clip) = self.key_time(c);
        let Some(mut v) = self.visual(id) else { return };
        let shown = v.anim.eval(&v.transform, src);
        apply_transform_edit(&mut v, &shown, &edited, src, on_clip);
        if v.anim.is_animated() && !on_clip && Prop::ALL.iter().any(|p| shown.prop_differs(*p, &edited) && v.anim.prop_animated(*p)) {
            self.status = "Keyframed: move the playhead onto the clip to change it".into();
        }
        self.set_visual_live(id, v);
    }

    /// The keyframe button of a transform property: removes the key at the
    /// playhead if there is one, otherwise adds one there. One undo step.
    pub fn toggle_transform_key(&mut self, id: ClipId, prop: Prop) {
        self.commit_live_edit();
        let Some((_, c)) = self.seq().find_clip(id) else { return };
        let (src, on_clip) = self.key_time(c);
        if !on_clip {
            self.status = "Move the playhead onto the clip to set a keyframe".into();
            return;
        }
        let Some(mut v) = self.visual(id) else { return };
        let shown = v.anim.eval(&v.transform, src);
        if v.anim.has_key(prop, src) {
            v.anim.remove_key(prop, src);
            if !v.anim.prop_animated(prop) {
                // The last key gone: keep the value it had.
                v.transform.copy_prop(prop, &shown);
            }
        } else {
            v.anim.set_key(prop, src, &shown);
        }
        self.set_visual_live(id, v);
        self.commit_live_edit();
    }

    // ---- Markers ----------------------------------------------------------------------

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
        new.push(Marker { at, name: s.next_marker_name(), color: Default::default() });
        new.sort_by_key(|m| m.at);
        self.execute(Box::new(reelforge_core::undo::SetMarkers { seq: self.active, old, new }));
        self.status = format!("Marker at {} (double-click it on the ruler to name it)", at.timecode(self.seq().frame_rate));
    }

    /// Change the markers as part of a gesture (dragging one, typing its name);
    /// `commit_live_edit` makes it one undo step. Kept in time order.
    pub fn set_markers_live(&mut self, mut markers: Vec<Marker>) {
        markers.sort_by_key(|m| m.at);
        let before = self.seq().markers.clone();
        if before == markers {
            return;
        }
        self.begin_live(LiveEdit::Markers(before));
        self.project.sequences[self.active].markers = markers;
        self.touch();
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

    /// Jump to the previous / next cut, marker or in/out point.
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

    /// Jump to the previous / next marker only.
    pub fn jump_marker(&mut self, forward: bool) {
        let s = self.seq();
        let rate = s.frame_rate;
        let now = self.playback.playhead;
        let half = Time(rate.frame_duration().0 / 2);
        let m = if forward { s.markers.iter().find(|m| m.at > now + half) } else { s.markers.iter().rev().find(|m| m.at + half < now) };
        match m.cloned() {
            Some(m) => {
                self.playback.pause();
                self.playback.seek(m.at);
                self.status = format!("{} · {}", m.at.timecode(rate), m.name);
            }
            None => self.status = if forward { "No more markers after this" } else { "No markers before this" }.into(),
        }
    }

    // ---- Relinking --------------------------------------------------------------------

    /// Point missing media `id` at `new_path`; then look in that folder for any
    /// other missing files with their original names.
    pub fn relink(&mut self, id: MediaId, new_path: PathBuf) {
        let Some(m) = self.project.media(id).cloned() else { return };
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
    pub fn find_missing_in(&mut self, dir: &Path) {
        let missing: Vec<PathBuf> = reelforge_core::undo::missing_media(&self.project).into_iter().filter_map(|o| self.project.media(o).map(|m| m.path.clone())).collect();
        let pairs = crate::safety::find_by_name(dir, &missing);
        if pairs.is_empty() {
            self.status = format!("None of the missing files are in {}", dir.display());
        } else {
            self.apply_relinks(pairs);
        }
    }

    fn apply_relinks(&mut self, pairs: Vec<(PathBuf, PathBuf)>) {
        self.commit_live_edit();
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
            for m in self.project.media.clone() {
                self.compute_waveform(&m);
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

    // ---- Masks --------------------------------------------------------------------

    /// The size a clip's masks and tracker are in (its picture, or the frame for a title).
    pub fn layer_size(&self, clip: &Clip) -> [f32; 2] {
        match self.project.picture_size(clip) {
            Some([w, h]) => [w as f32, h as f32],
            None => [self.seq().width as f32, self.seq().height as f32],
        }
    }

    /// The source time a clip's masks are seen at (the playhead, kept inside the clip).
    pub fn mask_time(&self, clip: &Clip) -> Time {
        clip.source_time(self.playback.playhead.max(clip.timeline_start).min(clip.timeline_end()))
    }

    /// The media time a clip's tracker is seen at.
    pub fn track_time(&self, clip: &Clip) -> Time {
        clip.media_at(self.playback.playhead.max(clip.timeline_start).min(clip.timeline_end()))
    }

    /// Add a mask to a clip as one undo step, named after its shape. Returns its index.
    pub fn add_mask(&mut self, id: ClipId, mut mask: Mask) -> Option<usize> {
        self.commit_live_edit();
        let mut v = self.visual(id)?;
        let n = v.masks.len();
        mask.name = format!("{} {}", mask.shape.label(), n + 1);
        v.masks.push(mask);
        self.set_visual_live(id, v);
        self.commit_live_edit();
        self.status = "Mask added: drag its corners in the Viewer, or inside it to move it".into();
        Some(n)
    }

    /// Replace a clip's masks as part of a gesture (or, followed by
    /// `commit_live_edit`, as one step).
    pub fn set_masks_live(&mut self, id: ClipId, masks: Vec<Mask>) {
        let Some(mut v) = self.visual(id) else { return };
        v.masks = masks;
        self.set_visual_live(id, v);
    }

    /// Move mask `index`'s points to `points` as seen at the playhead (a
    /// keyframe there if the shape is animated).
    pub fn set_mask_points_live(&mut self, id: ClipId, index: usize, points: Vec<[f32; 2]>) {
        let Some((_, c)) = self.seq().find_clip(id) else { return };
        let t = self.mask_time(c);
        let Some(mut v) = self.visual(id) else { return };
        let Some(m) = v.masks.get_mut(index) else { return };
        m.set_points_at(t, points);
        self.set_visual_live(id, v);
    }

    // ---- Motion tracking -----------------------------------------------------------

    /// Change a clip's tracker as part of a gesture (dragging it, a slider).
    pub fn set_tracker_live(&mut self, id: ClipId, t: Option<Tracker>) {
        let Some(before) = self.seq().find_clip(id).map(|(_, c)| c.tracker.clone()) else { return };
        if before == t {
            return;
        }
        self.begin_live(LiveEdit::Tracker(id, before));
        if let Some((_, c)) = self.project.sequences[self.active].find_clip_mut(id) {
            c.tracker = t;
        }
        self.touch();
    }

    /// Change a clip's tracker as one undo step.
    pub fn set_tracker(&mut self, id: ClipId, t: Option<Tracker>, label: &'static str) {
        self.commit_live_edit();
        let Some(old) = self.seq().find_clip(id).map(|(_, c)| c.tracker.clone()) else { return };
        if old != t {
            self.execute(Box::new(SetTracker { seq: self.active, id, old, new: t, label }));
        }
    }

    /// Put a new tracker on a footage clip, centred on its picture.
    pub fn add_tracker(&mut self, id: ClipId, mode: TrackMode) {
        let Some((_, c)) = self.seq().find_clip(id) else { return };
        let Some([w, h]) = self.project.picture_size(c) else { return };
        let layer = [w as f32, h as f32];
        let src = self.track_time(c);
        self.set_tracker(id, Some(Tracker::new(mode, [layer[0] / 2.0, layer[1] / 2.0], layer, src)), "Add tracker");
        self.status = match mode {
            TrackMode::Position => "Drag the tracker box in the Viewer onto a sharp detail, then press Track forward".into(),
            TrackMode::Transform => "Drag boxes 1 and 2 onto two sharp details, well apart on the same object, then press Track forward".into(),
            TrackMode::CornerPin => "Drag boxes 1–4 onto the corners of the surface (top left, top right, bottom right, bottom left), then press Track forward".into(),
        };
    }

    /// Track the clip's points from the playhead to the end of the clip (or
    /// back to its start), in the background.
    pub fn start_tracking(&mut self, id: ClipId, forward: bool) {
        self.commit_live_edit();
        if self.tracking.is_some() {
            return;
        }
        let frame = self.seq().frame_rate.frame_duration();
        let Some((_, c)) = self.seq().find_clip(id) else { return };
        let (Some(track), Some(media)) = (c.tracker.clone(), self.project.media(c.media).cloned()) else { return };
        let t = self.playback.playhead.max(c.timeline_start).min(c.timeline_end() - frame);
        // Tracking works in media time; for a reversed clip "forward" on the timeline is backwards in the media.
        let from = c.media_at(t);
        let until = if forward { c.media_at(c.timeline_end() - frame) } else { c.media_at(c.timeline_start) };
        let (tx, rx) = unbounded();
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop = cancel.clone();
        self.playback.pause();
        std::thread::Builder::new()
            .name("tracker".into())
            .spawn(move || {
                let progress_tx = tx.clone();
                let r = reelforge_media::track_points(&media.path, &media.info, &track, from, until, &stop, |p| {
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
                let n = samples.first().map_or(0, |s| s.len().saturating_sub(1));
                let Some(old) = self.project.sequences.get(job.seq).and_then(|s| s.find_clip(job.clip)).and_then(|(_, c)| c.tracker.clone()) else { return };
                let mut new = old.clone();
                for (p, s) in new.points.iter_mut().zip(samples) {
                    p.merge(s);
                }
                let rate = self.project.sequences[job.seq].frame_rate;
                let many = new.points.len() > 1;
                let status = match end {
                    reelforge_media::TrackEnd::Done => format!("Tracked {n} frames"),
                    reelforge_media::TrackEnd::Cancelled => format!("Tracking stopped after {n} frames"),
                    reelforge_media::TrackEnd::Lost(t, i) => {
                        let what = if many { format!("point {}", i + 1) } else { "the feature".into() };
                        format!("Tracked {n} frames, then lost {what} at {} (source time): go there, drag it back onto its detail and track again", t.timecode(rate))
                    }
                };
                let label = if many { "Track points" } else { "Track point" };
                self.execute(Box::new(SetTracker { seq: job.seq, id: job.clip, old: Some(old), new: Some(new), label }));
                self.status = status;
            }
        }
    }

    /// Make mask `index` of the tracked clip move with its tracked points.
    pub fn apply_track_to_mask(&mut self, tracked: ClipId, index: usize) {
        self.commit_live_edit();
        let Some((_, c)) = self.seq().find_clip(tracked) else { return };
        let (Some(track), Some(mask)) = (&c.tracker, c.masks.get(index)) else { return };
        let Some(m) = reelforge_core::track::follow_mask(c, mask, track) else {
            self.status = "Nothing to apply: track the points first".into();
            return;
        };
        let name = m.name.clone();
        let mut v = c.visual();
        v.masks[index] = m;
        self.set_visual_live(tracked, v);
        self.commit_live_edit();
        self.status = format!("{name} now follows the tracked motion");
    }

    /// Make `target` follow the tracked motion of `tracked` (or, if they're the
    /// same clip, hold it steady), as keyframes. `smoothing` (frames) keeps
    /// slow camera moves when stabilising.
    pub fn apply_track(&mut self, tracked: ClipId, target: ClipId, smoothing: f32) {
        self.commit_live_edit();
        let s = self.seq();
        let (Some((_, a)), Some((_, b))) = (s.find_clip(tracked), s.find_clip(target)) else { return };
        let Some(track) = &a.tracker else { return };
        if self.project.picture_size(a).is_none() {
            return;
        }
        let (layer, target_layer) = (self.layer_size(a), self.layer_size(b));
        let frame = [s.width as f32, s.height as f32];
        let stabilise = tracked == target;
        let opts = ApplyOptions { stabilise, smoothing: if stabilise { smoothing } else { 0.0 } };
        let mode = track.mode;
        let Some(anim) = reelforge_core::track::follow_keys(a, track, layer, frame, b, target_layer, opts) else {
            self.status = if track.frames().is_empty() { "Nothing to apply: track every point first".into() } else { "Nothing to apply: the track doesn't overlap that clip".into() };
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
        self.status = match (stabilise, mode) {
            (true, TrackMode::CornerPin) => "Stabilised: the clip is corner-pinned against the tracked perspective".into(),
            (true, _) => "Stabilised: the clip now moves against the tracked motion (scale it up a little to hide the edges)".into(),
            (false, TrackMode::CornerPin) => format!("{name} is now corner-pinned to the tracked surface"),
            (false, TrackMode::Transform) => format!("{name} now follows the tracked position, rotation and scale"),
            (false, TrackMode::Position) => format!("{name} now follows the tracked point"),
        };
    }

    // ---- Files -------------------------------------------------------------------

    /// Save to `path`. Files inside the project's folder are saved by relative
    /// path, so the folder can be moved and still open.
    pub fn save_to(&mut self, path: &Path) -> anyhow::Result<()> {
        self.commit_live_edit();
        let base = path.parent().map(Path::to_path_buf).unwrap_or_default();
        let base = if base.is_absolute() { base } else { std::env::current_dir().map(|d| d.join(&base)).unwrap_or(base) };
        let json = serde_json::to_string_pretty(&self.project.paths_relative_to(&base))?;
        crate::safety::write_atomic(path, json.as_bytes())?;
        self.status = format!("Saved {}", path.display());
        self.project_path = Some(path.to_owned());
        self.saved_rev = self.rev;
        if let Some(s) = &mut self.session {
            s.set_project(Some(path.to_owned()));
            s.autosaved_rev = self.rev;
        }
        Ok(())
    }

    pub fn open_project_path(&mut self, path: PathBuf) {
        match load_project(&path) {
            Ok(p) => {
                self.replace_project(p, Some(path.clone()));
                self.saved_rev = self.rev;
                self.status = format!("Opened {}", path.display());
                self.after_load();
            }
            Err(e) => self.status = format!("Open failed: {e:#}"),
        }
    }

    /// Load crashed session `i`'s autosave as an unsaved project.
    pub fn recover(&mut self, i: usize) {
        if i >= self.recoveries.len() {
            return;
        }
        let r = self.recoveries.remove(i);
        match r.load() {
            Ok(p) => {
                let path = r.info.project.clone();
                self.replace_project(p, path);
                self.saved_rev = self.rev.wrapping_sub(1); // recovered work hasn't been saved
                self.after_load();
                self.status = "Recovered your work: save it to keep it".into();
                r.dismiss(true);
            }
            Err(e) => self.status = format!("Couldn't read the autosave: {e:#}"),
        }
    }

    /// Forget crashed session `i` (deleting its autosave).
    pub fn dismiss_recovery(&mut self, i: usize) {
        if i < self.recoveries.len() {
            self.recoveries.remove(i).dismiss(false);
        }
    }

    /// After opening or recovering: waveforms, and a warning about missing files.
    fn after_load(&mut self) {
        for m in self.project.media.clone() {
            self.compute_waveform(&m);
        }
        let missing = reelforge_core::undo::missing_media(&self.project);
        if !missing.is_empty() {
            self.status = format!("{} media file{} can't be found", missing.len(), if missing.len() == 1 { "" } else { "s" });
        }
    }

    // ---- Each frame ----------------------------------------------------------------

    /// Housekeeping and the viewer picture, called often (each UI frame):
    /// moves the playhead, pushes edits to the audio engine, collects
    /// waveforms and proxies, autosaves, and renders the picture at the playhead.
    pub fn tick(&mut self) {
        if self.rev != self.proxied_rev {
            self.proxies.refresh(&self.project, self.prefs.auto_proxies);
            self.proxied_rev = self.rev;
        }
        if self.proxies.poll() {
            self.status = "A proxy is ready: the viewer uses it now".into();
        }
        if let Some(s) = &mut self.session {
            if let Some(msg) = s.tick(&self.project, self.rev, self.prefs.autosave_minutes) {
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
        self.poll_tracking();
        let seq = &self.project.sequences[self.active];
        let (rate, end) = (seq.frame_rate, seq.duration());
        self.playback.tick(rate, end);
        self.playback.scrub_if_moved();
        self.meters.poll(self.playback.audio.as_ref());
        let sources = self.decode_paths();
        self.preview.update(&self.project, self.active, self.playback.playhead, self.playback.playing, &sources, &mut self.render_cache);
        if let Some(e) = self.preview.error.take() {
            self.status = e;
        }
    }

    /// Play or pause.
    pub fn toggle_play(&mut self) {
        let end = self.seq().duration();
        self.playback.toggle(end);
    }

    /// Move the playhead to `t` (snapped to a frame).
    pub fn seek(&mut self, t: Time) {
        let rate = self.seq().frame_rate;
        self.playback.seek(t.max(Time::ZERO).snap(rate));
    }
}

/// Read a project file; relative media paths are relative to the file.
pub fn load_project(path: &Path) -> anyhow::Result<Project> {
    let mut p: Project = serde_json::from_str(&std::fs::read_to_string(path)?)?;
    if let Some(dir) = path.parent() {
        // (Not canonicalized: on Windows that gives \\?\ paths, which some tools mishandle.)
        let dir = if dir.is_absolute() { dir.to_path_buf() } else { std::env::current_dir()?.join(dir) };
        p.resolve_paths(&dir);
    }
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn editor() -> Editor {
        Editor::new(None, None)
    }

    #[test]
    fn transform_keys_toggle_and_edits_key_animated_properties() {
        let mut e = editor();
        e.add_title();
        let id = e.selection.unwrap();
        let sec = Time::from_secs_f64(1.0);
        // Not on the clip: no key.
        e.seek(Time(sec.0 * 100));
        e.toggle_transform_key(id, Prop::Position);
        assert!(!e.visual(id).unwrap().anim.is_animated());
        e.seek(Time::ZERO);
        e.toggle_transform_key(id, Prop::Position);
        assert_eq!(e.visual(id).unwrap().anim.key_count(Prop::Position), 1);
        // Editing an animated property sets a key at the playhead; others stay static.
        e.seek(sec);
        let mut t = e.visual(id).unwrap().transform;
        t.position = [100.0, 50.0];
        t.rotation = 30.0;
        e.set_transform_live(id, t);
        e.commit_live_edit();
        let v = e.visual(id).unwrap();
        assert_eq!(v.anim.key_count(Prop::Position), 2);
        assert_eq!(v.transform.position, [0.0, 0.0], "the static value stays");
        assert_eq!(v.transform.rotation, 30.0, "not keyframed: changed for the whole clip");
        let half = v.anim.eval(&v.transform, Time(sec.0 / 2));
        assert_eq!(half.position, [50.0, 25.0]);
        // Toggling the key here removes it; removing the last keeps its value.
        e.toggle_transform_key(id, Prop::Position);
        e.seek(Time::ZERO);
        e.toggle_transform_key(id, Prop::Position);
        let v = e.visual(id).unwrap();
        assert!(!v.anim.prop_animated(Prop::Position));
        e.undo();
        assert_eq!(e.visual(id).unwrap().anim.key_count(Prop::Position), 1, "each toggle is one undo step");
    }

    #[test]
    fn markers_add_move_rename_remove_and_jump() {
        let mut e = editor();
        e.add_title();
        let rate = e.seq().frame_rate;
        let sec = Time::from_secs_f64(1.0).snap(rate);
        e.seek(sec);
        e.add_marker();
        e.add_marker(); // same place: refused
        e.seek(Time(sec.0 * 3));
        e.add_marker();
        assert_eq!(e.seq().markers.len(), 2);
        let steps = e.undo.position();
        // Drag the second one before the first: kept in time order, one undo step.
        let mut list = e.seq().markers.clone();
        list[1].at = Time(sec.0 / 2);
        list[1].name = "Start".into();
        e.set_markers_live(list.clone());
        list[1].at = Time(sec.0 / 4);
        e.set_markers_live(list);
        e.commit_live_edit();
        assert_eq!(e.undo.position(), steps + 1);
        assert_eq!(e.seq().markers[0].name, "Start");
        assert_eq!(e.seq().markers[0].at, Time(sec.0 / 4));
        e.seek(Time::ZERO);
        e.jump_marker(true);
        assert_eq!(e.playback.playhead, Time(sec.0 / 4));
        e.jump_marker(true);
        assert_eq!(e.playback.playhead, sec);
        e.remove_marker(0);
        assert_eq!(e.seq().markers.len(), 1);
        e.undo();
        assert_eq!(e.seq().markers.len(), 2);
    }

    #[test]
    fn titles_and_tracks_undo_and_redo() {
        let mut e = editor();
        let tracks = e.seq().tracks.len();
        e.add_title();
        let id = e.selection.expect("the new title is selected");
        assert!(e.seq().find_clip(id).is_some());
        assert!(e.is_dirty());
        e.add_track(TrackKind::Audio);
        assert_eq!(e.seq().tracks.len(), tracks + 1);
        e.undo();
        e.undo();
        assert_eq!(e.seq().tracks.len(), tracks);
        assert!(e.seq().find_clip(id).is_none());
        assert_eq!(e.selection, None, "an undone clip isn't selected");
        e.redo();
        assert!(e.seq().find_clip(id).is_some());
    }

    #[test]
    fn a_slider_drag_is_one_undo_step() {
        let mut e = editor();
        e.add_title();
        let id = e.selection.unwrap();
        let steps = e.undo.position();
        for x in [10.0, 20.0, 30.0] {
            let mut v = e.visual(id).unwrap();
            v.transform.position[0] = x;
            e.set_visual_live(id, v);
        }
        e.commit_live_edit();
        assert_eq!(e.undo.position(), steps + 1);
        assert_eq!(e.visual(id).unwrap().transform.position[0], 30.0);
        e.undo();
        assert_eq!(e.visual(id).unwrap().transform.position[0], 0.0);
    }

    #[test]
    fn trimming_starts_from_the_gesture_start_every_time() {
        let mut e = editor();
        e.add_title();
        let id = e.selection.unwrap();
        let before = e.seq().find_clip(id).unwrap().1.duration;
        let sec = Time::from_secs_f64(1.0);
        e.trim_live(id, Edge::End, TrimMode::Normal, Time(-sec.0));
        e.trim_live(id, Edge::End, TrimMode::Normal, Time(-2 * sec.0));
        e.commit_live_edit();
        assert_eq!(e.seq().find_clip(id).unwrap().1.duration, Time(before.0 - 2 * sec.0));
        e.undo();
        assert_eq!(e.seq().find_clip(id).unwrap().1.duration, before);
    }

    #[test]
    fn saving_and_opening_round_trips() {
        let dir = std::env::temp_dir().join(format!("rf-engine-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.rfproj");
        let mut e = editor();
        e.add_title();
        e.save_to(&path).unwrap();
        assert!(!e.is_dirty());
        let mut f = editor();
        f.open_project_path(path.clone());
        assert_eq!(f.project_path.as_deref(), Some(path.as_path()));
        assert_eq!(f.seq().tracks.iter().map(|t| t.clips.len()).sum::<usize>(), 1);
        assert!(!f.is_dirty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn masks_and_trackers_are_undoable() {
        let mut e = editor();
        e.add_title();
        let id = e.selection.unwrap();
        let size = { let c = e.seq().find_clip(id).unwrap().1.clone(); e.layer_size(&c) };
        let i = e.add_mask(id, Mask::centred(reelforge_core::mask::MaskShape::Ellipse, size)).unwrap();
        assert_eq!(e.seq().find_clip(id).unwrap().1.masks[i].name, "Ellipse 1");
        let steps = e.undo.position();
        e.set_mask_points_live(id, i, vec![[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0]]);
        e.set_mask_points_live(id, i, vec![[0.0, 0.0], [20.0, 0.0], [20.0, 20.0], [0.0, 20.0]]);
        e.commit_live_edit();
        assert_eq!(e.undo.position(), steps + 1, "a drag is one step");
        assert_eq!(e.seq().find_clip(id).unwrap().1.masks[i].points[2], [20.0, 20.0]);
        e.undo();
        e.undo();
        assert!(e.seq().find_clip(id).unwrap().1.masks.is_empty());

        // Trackers: a title has no picture of its own, so set one directly.
        let t = Tracker::new(TrackMode::Position, [50.0, 50.0], [100.0, 100.0], Time::ZERO);
        e.set_tracker(id, Some(t.clone()), "Add tracker");
        let mut moved = t.clone();
        moved.points[0].move_to(Time::ZERO, [60.0, 40.0]);
        e.set_tracker_live(id, Some(moved.clone()));
        e.commit_live_edit();
        assert_eq!(e.seq().find_clip(id).unwrap().1.tracker.as_ref(), Some(&moved));
        e.undo();
        assert_eq!(e.seq().find_clip(id).unwrap().1.tracker.as_ref(), Some(&t));
    }

    #[test]
    fn compositions_open_and_close_with_undo() {
        let mut e = editor();
        e.new_composite();
        assert_eq!((e.active, e.comp_open), (1, 1));
        e.undo();
        assert_eq!(e.active, 0, "undoing the composition goes back to the edit");
    }
}

/// Apply an edit of the transform as seen at source time `src` (`shown`) to
/// `edited`: keyframed properties get a key at `src` (when `can_key`), the
/// others change the clip's static value.
pub fn apply_transform_edit(v: &mut ClipVisual, shown: &Transform, edited: &Transform, src: Time, can_key: bool) {
    for p in Prop::ALL {
        if !shown.prop_differs(p, edited) {
            continue;
        }
        if v.anim.prop_animated(p) {
            if can_key {
                v.anim.set_key(p, src, edited);
            }
        } else {
            v.transform.copy_prop(p, edited);
        }
    }
}
