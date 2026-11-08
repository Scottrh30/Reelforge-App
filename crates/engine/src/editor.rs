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
use reelforge_core::track::{ApplyOptions, Axes, TrackMode, Tracker};
use reelforge_core::undo::{
    split_group, AddClip, AddTrack, Batch, MoveTrack, Edge, ImportMedia, RemoveTrack, RenameTrack, SetClipAudio, SetClipTiming, SetClipVisual, SetLink,
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
pub const MEDIA_EXTENSIONS: [&str; 23] = ["mp4", "mov", "mkv", "avi", "webm", "m4v", "mxf", "mts", "png", "jpg", "jpeg", "gif", "exr", "dpx", "tif", "tiff", "tga", "bmp", "wav", "mp3", "aac", "flac", "ogg"];

/// Choices that change how the editor behaves (kept with the app's settings).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Prefs {
    /// Decode proxies in the viewer when they're ready.
    pub use_proxies: bool,
    /// Make proxies for big footage by themselves.
    pub auto_proxies: bool,
    /// What new proxies are made as.
    pub proxy_format: reelforge_media::proxy::ProxyFormat,
    /// Autosave every this many minutes (0 = never).
    pub autosave_minutes: u32,
    /// Size limit of the render cache.
    pub render_cache_mb: usize,
    /// Rendered frames kept on disk too (in the cache folder), MB; 0: not at all.
    pub render_cache_disk_mb: usize,
    /// 3D shadows: 0 Low, 1 Standard, 2 High (how sharp shadows from big layers can be).
    pub shadow_quality: u8,
    /// Export settings saved under a name.
    pub export_presets: Vec<crate::presets::UserPreset>,
    /// Play the sound under the playhead while scrubbing.
    pub scrub_audio: bool,
    /// Pictures along video clips in the timeline.
    pub timeline_thumbnails: bool,
    /// …one at the clip's start, or a filmstrip all along it.
    pub timeline_filmstrip: bool,
    /// A waveform of a video clip's linked sound along its lower edge.
    pub timeline_video_waveforms: bool,
    /// Playback quality: the Viewer draws at 1/this of full resolution (1, 2
    /// or 4; 0: automatic, lower while playing falls behind).
    pub playback_quality: u32,
    /// …but the paused picture at full resolution.
    pub full_when_paused: bool,
    /// Layers dragged in the viewer snap to the frame and to each other (Alt: free).
    pub viewer_snapping: bool,
    /// Decode video on the graphics card when possible.
    pub hw_decode: bool,
    /// Memory for keeping decoded frames, in MB.
    pub cache_mb: usize,
    /// Decode with Reelforge's own decoders where it has them (else FFmpeg).
    pub native_decoders: bool,
    /// Leave hardware-decoded frames on the graphics card for the viewer (zero-copy).
    pub zero_copy: bool,
    /// Show the start screen (presets, templates, recent projects) when Reelforge opens.
    pub start_screen: bool,
    /// Effects starred as favourites (an effect's kind, or `plugin:` and the
    /// plugin's id), listed first in the Effects panel and browser.
    pub fx_favourites: Vec<String>,
    /// Where proxies and rendered previews go (`None`: the cache folder).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cache_folder: Option<PathBuf>,
    /// Where Viewer snapshots go (`None`: a Snapshots folder beside the
    /// project, or in Pictures before it's saved).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot_folder: Option<PathBuf>,
    /// Add each snapshot to the Media panel as well.
    pub snapshot_import: bool,
    /// Keep a project version (see `versions`) every time the project is saved.
    pub versions_on_save: bool,
    /// Warn when the drive Reelforge saves to has less than this many GB free (0: never).
    pub disk_warn_gb: f32,
    /// Effects added lately, newest first (as `fx_favourites` names them), at most `RECENT_FX`.
    pub fx_recent: Vec<String>,
}

/// How many recently used effects are remembered.
pub const RECENT_FX: usize = 8;

/// RGBA pixels (`w` × `h`) shrunk by whole steps to about `width` wide
/// (averaging blocks), as a JPEG.
fn small_jpeg(rgba: &[u8], w: u32, h: u32, width: u32) -> Option<Vec<u8>> {
    let k = (w / width.max(16)).max(1);
    let (sw, sh) = (w / k, h / k);
    let mut small = vec![0u8; (sw * sh * 4) as usize];
    for y in 0..sh {
        for x in 0..sw {
            let mut acc = [0u32; 4];
            for dy in 0..k {
                for dx in 0..k {
                    let i = (((y * k + dy) * w + x * k + dx) * 4) as usize;
                    for c in 0..4 {
                        acc[c] += rgba[i + c] as u32;
                    }
                }
            }
            let o = ((y * sw + x) * 4) as usize;
            for c in 0..4 {
                small[o + c] = (acc[c] / (k * k)) as u8;
            }
        }
    }
    let mut jpg = Vec::new();
    jpeg_encoder::Encoder::new(&mut jpg, 78).encode(&small, sw as u16, sh as u16, jpeg_encoder::ColorType::Rgba).ok()?;
    Some(jpg)
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            use_proxies: true,
            auto_proxies: true,
            proxy_format: Default::default(),
            autosave_minutes: 2,
            shadow_quality: 1,
            render_cache_mb: 2048,
            render_cache_disk_mb: 8192,
            export_presets: Vec::new(),
            scrub_audio: true,
            timeline_thumbnails: true,
            timeline_filmstrip: true,
            timeline_video_waveforms: false,
            playback_quality: 1,
            full_when_paused: false,
            viewer_snapping: true,
            hw_decode: true,
            cache_mb: reelforge_media::cache::DEFAULT_BUDGET_MB,
            native_decoders: true,
            zero_copy: true,
            start_screen: true,
            fx_favourites: Vec::new(),
            fx_recent: Vec::new(),
            disk_warn_gb: 2.0,
            cache_folder: None,
            snapshot_folder: None,
            snapshot_import: false,
            versions_on_save: true,
        }
    }
}

impl Editor {
    /// Use new preferences: push them into the parts of the engine that use them.
    /// Remember that effect `key` (a kind's name, or `plugin:` and its id)
    /// was just added: first in Recent, once.
    pub fn note_recent_effect(&mut self, key: &str) {
        let r = &mut self.prefs.fx_recent;
        r.retain(|k| k != key);
        r.insert(0, key.to_string());
        r.truncate(RECENT_FX);
    }

    pub fn set_prefs(&mut self, prefs: Prefs) {
        self.prefs = prefs;
        self.apply_prefs();
    }

    pub fn apply_prefs(&mut self) {
        let before = reelforge_media::proxy::media_cache_dir();
        reelforge_media::proxy::set_media_cache_dir(self.prefs.cache_folder.clone());
        if reelforge_media::proxy::media_cache_dir() != before {
            // Proxies are looked for again in the new place (rendered previews reopen below).
            self.proxies.forget_ready();
        }
        self.render_cache.set_budget_mb(self.prefs.render_cache_mb);
        self.apply_render_disk();
        self.playback.scrub_audio = self.prefs.scrub_audio;
        reelforge_render::set_shadow_quality(self.prefs.shadow_quality as usize);
        // (Tests don't depend on the machine's free space; the disk test sets it.)
        if !cfg!(test) {
            self.disk_low_at = (self.prefs.disk_warn_gb.max(0.0) as f64 * (1u64 << 30) as f64) as u64;
            self.disk_checked = None;
        }
        reelforge_media::hw::set_enabled(self.prefs.hw_decode);
        reelforge_media::codecs::set_enabled(self.prefs.native_decoders);
        reelforge_core::frame::zero_copy::set_enabled(self.prefs.zero_copy);
        reelforge_media::FrameCache::global().set_budget_mb(self.prefs.cache_mb.clamp(128, 65536));
        reelforge_media::proxy::set_format(self.prefs.proxy_format);
        self.preview.set_quality(self.prefs.playback_quality);
        self.preview.full_when_paused = self.prefs.full_when_paused;
    }

    /// Open the rendered previews kept on disk again (after files moved in).
    pub fn reopen_render_disk(&mut self) {
        self.render_cache.disk = None;
        self.apply_render_disk();
    }

    /// Keep rendered frames on disk as the settings say (the same store across projects).
    fn apply_render_disk(&mut self) {
        let mb = self.prefs.render_cache_disk_mb as u64;
        if mb == 0 {
            self.render_cache.disk = None;
            return;
        }
        let dir = self.render_disk_dir.clone().unwrap_or_else(|| reelforge_media::proxy::media_cache_dir().join("render"));
        match &self.render_cache.disk {
            Some(d) if d.dir() == dir => d.set_budget(mb << 20),
            _ => self.render_cache.disk = Some(crate::render_cache::DiskStore::open(&dir, mb << 20)),
        }
    }

    /// Show the paused picture at full resolution whatever the playback quality.
    pub fn set_full_when_paused(&mut self, on: bool) {
        self.prefs.full_when_paused = on;
        self.preview.full_when_paused = on;
        self.status = if on { "Full resolution when paused".into() } else { "Paused picture at the playback quality".into() };
    }

    /// Set the Viewer's playback quality (1 full, 2 half, 4 quarter resolution,
    /// 0 automatic).
    pub fn set_playback_quality(&mut self, q: u32) {
        self.prefs.playback_quality = q;
        self.preview.set_quality(q);
        self.prefs.playback_quality = self.preview.quality();
        self.status = format!(
            "Playback quality: {}",
            match self.preview.quality() {
                0 => "automatic (lower while playing falls behind)",
                1 => "full resolution",
                2 => "half resolution",
                _ => "quarter resolution",
            }
        );
    }

    /// Make every proxy again in the current format (those already in it stay).
    /// Returns how many are being remade.
    pub fn remake_proxies(&mut self) -> usize {
        // Let go of the old proxy files first (Windows can't delete open files).
        self.preview.reset();
        let n = self.proxies.remake_all(&self.project);
        self.status = match n {
            0 => format!("Every proxy is already {}", reelforge_media::proxy::format().label()),
            1 => format!("Making 1 proxy again as {}", reelforge_media::proxy::format().label()),
            n => format!("Making {n} proxies again as {}", reelforge_media::proxy::format().label()),
        };
        n
    }
}

/// Messages from a tracking run.
enum TrackMsg {
    Progress(f32),
    Done(Result<(Vec<Vec<reelforge_core::track::TrackSample>>, reelforge_media::TrackEnd), String>),
    /// A 3D camera solve (`None`: stopped).
    CameraDone(Result<Option<reelforge_core::camtrack::CameraTrack>, String>),
    /// Object removal's frames, in this folder (`None`: stopped).
    FillDone(Result<Option<std::path::PathBuf>, String>),
    /// A roto brush run (`None`: stopped).
    RotoDone(Result<Option<roto::RotoRun>, String>),
}

mod camtrack;
mod fill;
pub mod roto;
mod diskspace;
mod viewcam;
mod remap;
mod multicam;
pub mod versions;
mod interchange;
pub mod magnetic;
pub mod simulate;
pub use multicam::MulticamAt;
mod replace;
mod snapshot;
mod stilltrim_tests;
pub use viewcam::ViewMode;
pub use camtrack::CamMake;

/// A motion-tracking run going on in the background.
pub struct TrackJob {
    seq: usize,
    pub clip: ClipId,
    rx: Receiver<TrackMsg>,
    cancel: Arc<std::sync::atomic::AtomicBool>,
    /// 0–1.
    pub progress: f32,
    /// Tracking a mask's shape (rotoscoping): which mask, and how it may move.
    pub mask: Option<(usize, reelforge_core::track::MaskMotion)>,
    /// …through another clip's footage (below an adjustment layer or a solid).
    pub through: Option<ClipId>,
    /// Solving the 3D camera (not 2D tracking).
    pub camera: bool,
    /// Working out object removal effect `.0` (not tracking).
    pub fill: Option<usize>,
    /// Carrying roto brush mask `.0` on through the shot.
    pub roto: Option<usize>,
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
    /// A video clip's transition length (dragging it).
    Dissolve(ClipId, Time),
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
            (LiveEdit::Dissolve(a, _), LiveEdit::Dissolve(b, _)) => a == b,
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
    /// Projects opened or saved lately (the start screen and File > Open recent).
    pub recent: crate::start::Recent,
    /// Decoders + GPU compositor producing the viewer picture.
    pub preview: Preview,
    /// Finished frames from Render preview.
    pub render_cache: RenderCache,
    /// Where rendered frames are kept on disk (`None`: the cache folder's
    /// `render`; tests use their own).
    pub render_disk_dir: Option<PathBuf>,
    live_edit: Option<LiveEdit>,
    pub meters: crate::meters::Meters,
    /// The sequence shown in the timeline and viewer (0 = the main edit).
    pub active: usize,
    /// The highlighted track (its name was clicked): where Paste and new
    /// layers go. Kept as its place and name, so it can be found again after
    /// tracks move about (see [`Editor::target_track`]).
    target: Option<(usize, String)>,
    /// The composition last opened (0 = none yet).
    pub comp_open: usize,
    /// Where the playhead was in each sequence, to come back to.
    playheads: HashMap<usize, Time>,
    /// Bumped on every project change.
    pub rev: u64,
    /// The Viewer looking from somewhere other than the sequence's camera
    /// (see `viewcam`); `None` through the camera.
    pub view: Option<(viewcam::ViewMode, reelforge_core::space3d::Camera)>,
    /// Bumped when the view changes (it isn't a project change).
    pub view_rev: u64,
    /// The Viewer shows multicam clips as a grid of their angles.
    pub multicam_grid: bool,
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
    /// Space running short where the app writes (shown until dismissed), when last looked, and below what it counts as low.
    pub disk_low: Option<crate::disk::DiskLow>,
    pub disk_dismissed: bool,
    disk_checked: Option<std::time::Instant>,
    pub disk_low_at: u64,
    /// Camera solves' fit per frame, by clip (and which solve it was for).
    cam_fit: Mutex<HashMap<ClipId, (u64, Arc<Vec<Option<f32>>>)>>,
    /// This run's autosave/crash-detection session.
    pub session: Option<crate::safety::Session>,
    /// Crashed sessions found at startup, offered for recovery.
    pub recoveries: Vec<crate::safety::Recovery>,
    /// A motion-tracking run in progress.
    pub tracking: Option<TrackJob>,
    /// A preset shown on a clip while it's hovered in the effects browser:
    /// the clip, and its look and title style before (put back by
    /// [`Editor::end_preset_preview`], and before anything else is done).
    preset_preview: Option<(ClipId, ClipVisual, Option<TitleSpec>, bool)>,
    /// Composition clips' filmstrip pictures: (sequence, time, width) → (recipe, JPEG).
    comp_thumbs: HashMap<(usize, i64, u32), (u64, Vec<u8>)>,
    /// Effects-browser pictures: (clip, effect, width) → (frame recipe, JPEG).
    fx_thumbs: HashMap<(ClipId, String, u32), (u64, Vec<u8>)>,
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
            recent: Default::default(),
            preview,
            render_cache: RenderCache::new(prefs.render_cache_mb),
            render_disk_dir: cfg!(test).then(|| std::env::temp_dir().join(format!("rf_render_cache_test_{}", std::process::id()))),
            live_edit: None,
            meters: Default::default(),
            active: 0,
            comp_open: 0,
            playheads: HashMap::new(),
            rev: 1,
            view: None,
            view_rev: 0,
            multicam_grid: false,
            target: None,
            mixed_rev: 0,
            saved_rev: 1,
            waveforms: HashMap::new(),
            waveform_tx,
            waveform_rx,
            fonts,
            prefs,
            proxies: Default::default(),
            proxied_rev: 0,
            disk_low: None,
            disk_dismissed: false,
            disk_checked: None,
            // (Tests don't depend on the machine's free space; the disk test sets it.)
            disk_low_at: if cfg!(test) { 0 } else { crate::disk::LOW },
            cam_fit: Default::default(),
            session: None,
            recoveries: Vec::new(),
            tracking: None,
            preset_preview: None,
            comp_thumbs: HashMap::new(),
            fx_thumbs: HashMap::new(),
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
        // The frames on disk stay (they're for any project).
        fresh.render_cache.disk = self.render_cache.disk.clone();
        fresh.render_disk_dir = self.render_disk_dir.clone();
        fresh.proxies = std::mem::take(&mut self.proxies);
        fresh.session = self.session.take();
        fresh.recoveries = std::mem::take(&mut self.recoveries);
        fresh.recent = std::mem::take(&mut self.recent);
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

    /// A new project whose sequence is `width` × `height` at `rate` (a
    /// preset from the start screen, or sizes typed in).
    pub fn new_project_with(&mut self, width: u32, height: u32, rate: reelforge_core::FrameRate) {
        self.replace_project(crate::start::project_with(width, height, rate), None);
        let s = self.seq();
        self.status = format!("New project: {}", crate::start::summary_of(s));
    }

    /// A new, unsaved project copied from the template file `path` (its
    /// sequences, tracks, titles, effects and media).
    pub fn new_from_template(&mut self, path: &Path) {
        match load_project(path) {
            Ok(p) => {
                self.replace_project(p, None);
                self.saved_rev = self.rev;
                self.after_load();
                let name = path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                if !self.status.contains("can't be found") {
                    self.status = format!("New project from the template \"{name}\"");
                }
            }
            Err(e) => self.status = format!("Couldn't read the template: {e:#}"),
        }
    }

    /// Save the project as a template called `name` in `dir` (media by
    /// full path, so it works from anywhere). Replaces one of that name.
    pub fn save_as_template(&mut self, dir: &Path, name: &str) -> anyhow::Result<PathBuf> {
        self.commit_live_edit();
        let path = crate::start::template_file(dir, name).ok_or_else(|| anyhow::anyhow!("give the template a name"))?;
        std::fs::create_dir_all(dir)?;
        let json = serde_json::to_string_pretty(&self.project)?;
        crate::safety::write_atomic(&path, json.as_bytes())?;
        self.status = format!("Saved the template \"{}\"", path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
        Ok(path)
    }

    /// The main sequence's size and rate, for the recent list.
    fn summary(&self) -> String {
        self.project.sequences.first().map(crate::start::summary_of).unwrap_or_default()
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
        self.target = None;
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
        // 3D models become 3D model layers at the playhead.
        let (models, paths): (Vec<PathBuf>, Vec<PathBuf>) = paths.iter().cloned().partition(|p| reelforge_core::is_model_file(p));
        for m in &models {
            self.add_3d_model(m);
        }
        if paths.is_empty() {
            return;
        }
        let mut ok = 0;
        // Store full paths, so the project works wherever it's opened from.
        let paths: Vec<PathBuf> = paths.iter().map(|p| std::path::absolute(p).unwrap_or_else(|_| p.clone())).collect();
        // A numbered still with numbered neighbours (`shot.1001.exr`…) brings in
        // the whole image sequence, once however many of its frames were picked,
        // at the sequence's frame rate.
        let rate = self.seq().frame_rate.0;
        let mut items: Vec<(PathBuf, String)> = Vec::new();
        let mut sequences = 0;
        for path in &paths {
            let item = match reelforge_core::imageseq::detect(path, rate) {
                Some(seq) => {
                    sequences += 1;
                    (seq.media_path(), seq.name())
                }
                None => (path.clone(), path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()),
            };
            if !items.iter().any(|(p, _)| *p == item.0) {
                items.push(item);
            }
        }
        for (path, name) in &items {
            let (path, name) = (path, name.clone());
            if self.project.media.iter().any(|m| m.path == *path) {
                continue;
            }
            match reelforge_media::probe(path) {
                Ok(info) => {
                    let id = MediaId(self.project.alloc_id());
                    let asset = MediaAsset { id, path: path.clone(), name, info, conform: None };
                    self.compute_waveform(&asset);
                    self.execute(Box::new(ImportMedia(asset)));
                    ok += 1;
                }
                Err(e) => self.status = format!("Could not import {}: {e:#}", path.display()),
            }
        }
        if ok > 0 {
            self.status = if sequences > 0 { format!("Imported {ok} item(s), including image sequences at {:.3} fps", self.seq().frame_rate.fps()).replace(".000 fps", " fps") } else { format!("Imported {ok} file(s)") };
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
        match self.magnetic_move(lead, &sel, track, start) {
            Some(Ok((cmd, what))) => {
                self.execute(cmd);
                self.status = what.into();
                return;
            }
            Some(Err(e)) => {
                self.status = format!("Can't move there: {e}");
                return;
            }
            None => {}
        }
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
        if let Some((cmd, closed)) = self.magnetic_delete(&ids) {
            self.execute(cmd);
            let rate = self.seq().frame_rate;
            self.status = format!("Deleted, and closed up {} on the main track", closed.timecode(rate));
            return true;
        }
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
        let target = self.target_track();
        match reelforge_core::clipboard::paste_clips_onto(&mut self.project, self.active, &cb, t, target) {
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
            Ok(cmd) => {
                self.execute(cmd);
                let n = frames.unsigned_abs();
                self.status = format!("Nudged {} frame{} {}", n, if n == 1 { "" } else { "s" }, if frames < 0 { "earlier" } else { "later" });
            }
            Err(e) => self.status = format!("Can't nudge: {e}"),
        }
    }

    /// Paste the chosen attributes of the copied clip onto the selected clips, as one undo step.
    pub fn paste_attributes(&mut self, set: reelforge_core::attrs::AttrSet) {
        self.commit_live_edit();
        let ids = self.selected_ids();
        if self.clipboard.is_empty() {
            self.status = "Paste attributes: copy a clip first (Ctrl+C), then select the clips to paste onto".into();
            return;
        }
        match reelforge_core::attrs::paste_attributes(&self.project, self.active, &self.clipboard, &ids, set) {
            Ok((cmd, n)) => {
                self.execute(cmd);
                self.status = format!("Pasted attributes onto {n} clip{}", if n == 1 { "" } else { "s" });
            }
            Err(e) => self.status = format!("Paste attributes: {e}"),
        }
    }

    /// The clips "Make composition" would take: the selected clips if there are
    /// several, else everything entirely between the in and out marks (on
    /// unlocked tracks) if they're set, else the selected clip.
    pub fn composite_candidates(&self) -> Vec<ClipId> {
        let s = self.seq();
        let selected = self.selected_ids();
        if selected.len() > 1 {
            return selected;
        }
        if s.in_point.is_some() || s.out_point.is_some() {
            if let Some((a, b)) = s.marked_range() {
                return s.tracks.iter().filter(|t| !t.locked).flat_map(|t| &t.clips).filter(|c| c.timeline_start >= a && c.timeline_end() <= b).map(|c| c.id).collect();
            }
        }
        selected
    }

    /// Turn the candidate clips into a composition clip (Ctrl+G).
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

    // ---- Render preview --------------------------------------------------------

    /// Start rendering every frame between the marks (or of the whole sequence)
    /// into the render cache, a little each tick.
    pub fn render_preview(&mut self) {
        self.commit_live_edit();
        let s = self.seq();
        let (start, end) = s.marked_range().filter(|_| s.in_point.is_some() || s.out_point.is_some()).unwrap_or((Time::ZERO, s.duration()));
        if end <= start {
            self.status = "Nothing to render: the sequence is empty".into();
            return;
        }
        let (first, last) = reelforge_export::frame_range(s.frame_rate, start, end);
        self.playback.pause();
        self.render_cache.job = Some(crate::render_cache::RenderJob { seq: self.active, next: first, end: last, first, rendered: 0, started: std::time::Instant::now() });
        self.status = format!("Rendering preview: {} frames (Esc to stop)", last - first);
    }

    pub fn stop_render_preview(&mut self) {
        if let Some(j) = self.render_cache.job.take() {
            self.status = format!("Stopped rendering the preview ({} of {} frames done)", j.next - j.first, j.end - j.first);
        }
    }

    /// How far a render-preview run has got (0–1), while one runs.
    pub fn render_progress(&self) -> Option<f32> {
        self.render_cache.job.as_ref().map(|j| j.fraction())
    }

    /// A small JPEG of composition `seq` at its time `t` (for composition
    /// clips' filmstrips), at most `width` pixels wide. Kept until what
    /// makes that picture changes.
    pub fn comp_thumbnail(&mut self, seq: usize, t: Time, width: u32) -> Option<Vec<u8>> {
        if seq >= self.project.sequences.len() {
            return None;
        }
        let sources = self.decode_paths();
        let recipe = reelforge_export::frame_recipe(&self.project, seq, t, &sources);
        let key = (seq, t.0, width);
        if let Some((r, jpg)) = self.comp_thumbs.get(&key) {
            if *r == recipe {
                return Some(jpg.clone());
            }
        }
        let (rgba, [w, h]) = self.preview.render_for_cache(&self.project, seq, t, &sources)?;
        let jpg = small_jpeg(&rgba, w, h, width)?;
        if self.comp_thumbs.len() > 400 {
            self.comp_thumbs.clear();
        }
        self.comp_thumbs.insert(key, (recipe, jpg.clone()));
        Some(jpg)
    }

    /// A small JPEG (about `width` pixels wide) of the frame at the playhead
    /// (or the middle of clip `id`, if the playhead is off it) with effect
    /// `fx` added to the end of the clip's stack, at its defaults: how the
    /// effect would look, for the effects browser. Nothing changes in the
    /// project. Kept until what makes that picture changes.
    pub fn effect_thumbnail(&mut self, id: ClipId, fx: reelforge_core::effects::Effect, width: u32) -> Option<Vec<u8>> {
        self.effect_thumbnail_at(id, fx, width, 0)
    }

    /// The same, `step` frames after the playhead (wrapping round to the
    /// clip's start past its end): the frames of a moving preview, so effects
    /// that change over time (Glitch, flicker, noise) show what they do.
    pub fn effect_thumbnail_at(&mut self, id: ClipId, fx: reelforge_core::effects::Effect, width: u32, step: u32) -> Option<Vec<u8>> {
        let (_, c) = self.seq().find_clip(id)?;
        if c.scene.is_some() {
            return None;
        }
        let ph = self.playback.playhead;
        let t0 = if ph >= c.timeline_start && ph < c.timeline_end() { ph } else { c.timeline_start + Time(c.duration.0 / 2) };
        let frame = self.seq().frame_rate.frame_duration();
        let t = if step == 0 || c.duration.0 <= 0 {
            t0
        } else {
            let along = (t0 - c.timeline_start).0 + frame.0 * step as i64;
            c.timeline_start + Time(along.rem_euclid(c.duration.0.max(1)))
        };
        let mut project = self.project.clone();
        project.sequences[self.active].find_clip_mut(id)?.1.effects.push(fx.clone());
        let sources = self.decode_paths();
        let recipe = reelforge_export::frame_recipe(&project, self.active, t, &sources);
        let key = (id, format!("{:?}{}@{step}", fx.kind, fx.plugin.as_ref().map_or("", |p| p.id.as_str())), width);
        if let Some((r, jpg)) = self.fx_thumbs.get(&key) {
            if *r == recipe {
                return Some(jpg.clone());
            }
        }
        // Drawn at about twice the picture's size (then averaged down): much
        // quicker than full size for big frames, and still sharp.
        let scale = (2.0 * width as f32 / self.seq().width.max(1) as f32).min(1.0);
        let (rgba, [w, h]) = self.preview.render_scaled(&project, self.active, t, &sources, scale)?;
        let jpg = small_jpeg(&rgba, w, h, width)?;
        if self.fx_thumbs.len() > 600 {
            self.fx_thumbs.clear();
        }
        self.fx_thumbs.insert(key, (recipe, jpg.clone()));
        Some(jpg)
    }

    /// Render the next frames of a render-preview run, for up to ~40 ms.
    fn step_render_job(&mut self, sources: &HashMap<MediaId, PathBuf>) {
        let Some(mut job) = self.render_cache.job.take() else { return };
        if self.playback.playing {
            self.status = format!("Stopped rendering the preview to play ({} of {} frames done)", job.next - job.first, job.end - job.first);
            return;
        }
        if job.seq != self.active || job.seq >= self.project.sequences.len() {
            self.status = "Stopped rendering the preview (another sequence was opened)".into();
            return;
        }
        let rate = self.project.sequences[job.seq].frame_rate;
        let budget = std::time::Instant::now();
        while job.next < job.end && budget.elapsed() < std::time::Duration::from_millis(40) {
            let t = Time::from_frame(job.next, rate);
            let recipe = reelforge_export::frame_recipe(&self.project, job.seq, t, sources);
            if !self.render_cache.has(job.seq, job.next, recipe) {
                match self.preview.render_for_cache(&self.project, job.seq, t, sources) {
                    Some((rgba, size)) => {
                        let (rgba, size) = crate::render_cache::fit_for_cache(rgba, size);
                        self.render_cache.insert(job.seq, job.next, recipe, size, rgba.into());
                        job.rendered += 1;
                    }
                    None => {
                        self.status = format!("Couldn't render the preview at {t}: {}", self.preview.error.take().unwrap_or_else(|| "no picture".into()));
                        return;
                    }
                }
            }
            job.next += 1;
        }
        if job.next >= job.end {
            let total = job.end - job.first;
            let (bytes, kept) = self.render_cache.stats();
            self.status = if job.rendered == 0 {
                format!("Preview already rendered ({total} frames)")
            } else {
                format!("Rendered the preview: {} of {total} frames in {:.1} s ({kept} kept, {} MB)", job.rendered, job.started.elapsed().as_secs_f64(), bytes >> 20)
            };
            if kept < total as usize {
                self.status += ". Not all fit: raise the render cache size in Settings";
            }
        } else {
            self.status = format!("Rendering preview… {:.0}% (Esc to stop)", job.fraction() * 100.0);
            self.render_cache.job = Some(job);
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
            Ok(cmd) => {
                self.execute(cmd);
                self.status = match (speed, reverse) {
                    (s, _) if s == 0.0 => "Freeze frame: holds the clip's first frame".into(),
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
        let top = || s.tracks.iter().rev().filter(|tr| tr.kind == TrackKind::Video && !tr.muted).find_map(|tr| tr.clips.iter().find(|c| footage(c))).map(|c| c.id);
        let Some(id) = self.selection.filter(|id| is_video(*id)).or_else(top) else {
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

    /// Ramp clip `id`'s speed from `from` to `to` over `steps` parts (see
    /// `reelforge_core::speed::speed_ramp`).
    pub fn speed_ramp(&mut self, id: ClipId, from: f32, to: f32, steps: usize, smooth: bool) {
        self.commit_live_edit();
        match reelforge_core::speed::speed_ramp(&mut self.project, self.active, id, from, to, steps, smooth) {
            Ok(cmd) => {
                self.execute(cmd);
                self.select_only(Some(id));
                self.status = format!("Speed ramp from {:.0}% to {:.0}% in {steps} steps", from * 100.0, to * 100.0);
            }
            Err(e) => self.status = format!("Can't ramp the speed: {e}"),
        }
    }

    // ---- Transitions ----------------------------------------------------------------

    /// The incoming clip of the cut nearest the playhead on a `kind` track
    /// (preferring the selected clips), within a second unless selected.
    fn cut_near_playhead(&self, kind: TrackKind) -> Option<ClipId> {
        let t = self.playback.playhead;
        let seq = self.seq();
        let mut cuts: Vec<(Time, ClipId)> = Vec::new();
        for tr in seq.tracks.iter().filter(|t| t.kind == kind) {
            for pair in tr.clips.windows(2) {
                if pair[0].timeline_end() == pair[1].timeline_start {
                    cuts.push((Time((pair[1].timeline_start.0 - t.0).abs()), pair[1].id));
                }
            }
        }
        let selected: Vec<ClipId> = reelforge_core::undo::expand_links(&self.project, self.active, &self.selected_ids());
        let preferred = cuts.iter().filter(|(_, id)| selected.contains(id) || self.project.clip_before(self.active, *id).is_some_and(|l| selected.contains(&l)));
        preferred
            .min_by_key(|(d, _)| *d)
            .or_else(|| cuts.iter().filter(|(d, _)| *d <= Time(Time::TICKS_PER_SECOND)).min_by_key(|(d, _)| *d))
            .map(|(_, id)| *id)
    }

    fn transition_status(&self, id: ClipId, what: &str) -> String {
        match self.project.crossfade_into(self.active, id) {
            Some(x) if x.is_empty() => format!("{what} added, but there's no spare media at the cut, so it plays as a plain cut"),
            Some(x) if x.len() < x.requested => format!("{what}: {:.2} s (limited by spare media)", x.len().as_secs_f64()),
            Some(x) => format!("{what}: {:.2} s", x.len().as_secs_f64()),
            None => format!("{what} removed"),
        }
    }

    /// Put a transition of `kind` on the video cut nearest the playhead (1 s if it had none).
    pub fn apply_transition(&mut self, kind: reelforge_core::Transition) {
        self.commit_live_edit();
        let Some(id) = self.cut_near_playhead(TrackKind::Video) else {
            self.status = "No cut between two video clips near the playhead: move the playhead to a cut first".into();
            return;
        };
        self.apply_transition_to(id, kind);
    }

    /// Put a transition of `kind` into clip `id` (from the clip before it).
    pub fn apply_transition_to(&mut self, id: ClipId, kind: reelforge_core::Transition) {
        self.commit_live_edit();
        let Some((_, c)) = self.seq().find_clip(id) else { return };
        if self.project.clip_before(self.active, id).is_none() {
            self.status = "A transition needs a clip just before this one on its track".into();
            return;
        }
        let mut cmds: Vec<Box<dyn Command>> = vec![Box::new(reelforge_core::transition::SetTransition { seq: self.active, id, old: c.transition, new: kind })];
        if c.dissolve_in == Time::ZERO {
            let one_sec = Time::from_secs_f64(1.0).snap(self.seq().frame_rate);
            cmds.push(Box::new(reelforge_core::undo::SetDissolve { seq: self.active, id, old: Time::ZERO, new: one_sec }));
        }
        self.execute(Box::new(Batch { name: format!("Add {}", kind.label().to_lowercase()), cmds }));
        self.select_only(Some(id));
        self.status = self.transition_status(id, kind.label());
    }

    /// Change the kind of the transition into clip `id`.
    pub fn set_transition_kind(&mut self, id: ClipId, kind: reelforge_core::Transition) {
        self.commit_live_edit();
        let Some((_, c)) = self.seq().find_clip(id) else { return };
        if c.transition != kind {
            self.execute(Box::new(reelforge_core::transition::SetTransition { seq: self.active, id, old: c.transition, new: kind }));
        }
    }

    /// A video clip's transition length (live: dragging; `commit_live_edit` ends it).
    pub fn set_dissolve_live(&mut self, id: ClipId, d: Time) {
        let Some(before) = self.seq().find_clip(id).map(|(_, c)| c.dissolve_in) else { return };
        let d = d.max(Time::ZERO);
        if before == d {
            return;
        }
        self.begin_live(LiveEdit::Dissolve(id, before));
        if let Some((_, c)) = self.project.sequences[self.active].find_clip_mut(id) {
            c.dissolve_in = d;
        }
        self.touch();
    }

    /// Ctrl+D (video) / Ctrl+Shift+D (audio): add a 1 s dissolve or crossfade
    /// at the cut nearest the playhead, or take it away if there is one.
    pub fn toggle_transition_at_playhead(&mut self, kind: TrackKind) {
        self.commit_live_edit();
        let what = if kind == TrackKind::Audio { "Crossfade" } else { "Cross-dissolve" };
        let Some(id) = self.cut_near_playhead(kind) else {
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
                self.execute(Box::new(reelforge_core::undo::SetDissolve { seq: self.active, id, old, new }));
            }
        }
        self.select_only(Some(id));
        self.status = self.transition_status(id, what);
    }

    /// Take away the transition (or audio crossfade) into clip `id`.
    pub fn remove_transition(&mut self, id: ClipId) {
        self.commit_live_edit();
        let Some((ti, c)) = self.seq().find_clip(id) else { return };
        if self.seq().tracks[ti].kind == TrackKind::Audio {
            let old = c.audio.clone();
            if old.crossfade_in > Time::ZERO {
                let mut new = old.clone();
                new.crossfade_in = Time::ZERO;
                self.execute(Box::new(SetClipAudio { seq: self.active, id, old, new }));
            }
        } else if c.dissolve_in > Time::ZERO {
            let old = c.dissolve_in;
            self.execute(Box::new(reelforge_core::undo::SetDissolve { seq: self.active, id, old, new: Time::ZERO }));
        }
        self.status = "Transition removed".into();
    }

    // ---- Tracks ---------------------------------------------------------------

    /// The highlighted track, if it still exists: where it was, or else the
    /// track with its name (it moved), or none (it went).
    pub fn target_track(&self) -> Option<usize> {
        let (i, name) = self.target.as_ref()?;
        let tracks = &self.seq().tracks;
        if tracks.get(*i).is_some_and(|t| &t.name == name) {
            return Some(*i);
        }
        tracks.iter().position(|t| &t.name == name)
    }

    /// Highlight track `index` (or none).
    pub fn set_target_track(&mut self, index: Option<usize>) {
        self.target = index.and_then(|i| self.seq().tracks.get(i).map(|t| (i, t.name.clone())));
        self.touch();
    }

    /// Where a new layer from `start` for `dur` goes: on the highlighted video
    /// track if it has room there, else on a new track just above it; with no
    /// video track highlighted, on a new track on top. Gives the command that
    /// adds that track, if one's needed, and the track's index.
    fn layer_track(&self, start: Time, dur: Time) -> (Option<AddTrack>, usize) {
        let s = self.seq();
        match self.target_track().filter(|&t| s.tracks[t].kind == TrackKind::Video) {
            Some(t) if !s.tracks[t].locked && !s.overlaps(t, start, dur, &[]) => (None, t),
            Some(t) => {
                let add = AddTrack { seq: self.active, index: t + 1, track: reelforge_core::project::Track::new(s.next_track_name(TrackKind::Video), TrackKind::Video) };
                (Some(add), t + 1)
            }
            None => {
                let add = AddTrack::new(&self.project, self.active, TrackKind::Video);
                let t = add.index;
                (Some(add), t)
            }
        }
    }

    /// After adding a layer: the highlight moves to its track, so the next one stacks on it.
    fn follow_new_layer(&mut self, track: usize) {
        if self.target.is_some() {
            self.set_target_track(Some(track));
        }
    }

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

    /// Copy track `index`, clips and all, to just above it.
    pub fn duplicate_track(&mut self, index: usize) {
        self.commit_live_edit();
        match reelforge_core::undo::DuplicateTrack::new(&mut self.project, self.active, index) {
            Ok(cmd) => {
                let (name, n, at) = (cmd.track.name.clone(), cmd.track.clips.len(), cmd.index);
                self.execute(Box::new(cmd));
                self.set_target_track(Some(at));
                self.status = format!("Duplicated the track as {name} ({n} clip{})", if n == 1 { "" } else { "s" });
            }
            Err(why) => self.status = format!("Can't duplicate that track: {why}"),
        }
    }

    /// Move a track to `to` in the stack (among tracks of its own kind).
    pub fn move_track(&mut self, from: usize, to: usize) {
        self.commit_live_edit();
        match MoveTrack::new(&self.project, self.active, from, to) {
            Ok(cmd) => self.execute(Box::new(cmd)),
            Err(why) => self.status = format!("Can't move that track: {why}"),
        }
    }

    pub fn rename_track(&mut self, index: usize, name: String) {
        let Some(old) = self.seq().tracks.get(index).map(|t| t.name.clone()) else { return };
        if name.is_empty() || name == old {
            return;
        }
        let was_target = self.target_track() == Some(index);
        self.execute(Box::new(RenameTrack { seq: self.active, index, old, new: name }));
        if was_target {
            self.set_target_track(Some(index));
        }
    }

    /// Set a track's row height in the timeline. Like hiding a track, it's
    /// saved with the project but isn't an edit to undo.
    pub fn set_track_height(&mut self, track: usize, height: u16) {
        let Some(tr) = self.project.sequences[self.active].tracks.get_mut(track) else { return };
        let h = height.clamp(Track::MIN_HEIGHT, Track::MAX_HEIGHT);
        if tr.height != h || tr.collapsed {
            (tr.height, tr.collapsed) = (h, false);
            self.touch();
        }
    }

    /// Show a video track's linked-sound waveforms under its clips (`Some`), or
    /// hide them, or follow Settings (`None`).
    pub fn set_track_waveforms(&mut self, track: usize, on: Option<bool>) {
        let Some(tr) = self.project.sequences[self.active].tracks.get_mut(track) else { return };
        tr.waveforms = on;
        self.status = match on {
            Some(true) => format!("{}: sound waveforms shown under its clips", tr.name),
            Some(false) => format!("{}: sound waveforms hidden", tr.name),
            None => format!("{}: sound waveforms as in Settings", tr.name),
        };
        self.touch();
    }

    /// Collapse a track to a thin row, or expand it again.
    pub fn toggle_collapse(&mut self, track: usize) {
        let Some(tr) = self.project.sequences[self.active].tracks.get_mut(track) else { return };
        tr.collapsed = !tr.collapsed;
        self.status = format!("{} {}", tr.name, if tr.collapsed { "collapsed" } else { "expanded" });
        self.touch();
    }

    /// Collapse or expand every track of `kind` (all tracks if `None`); `reset`
    /// also puts their heights back to the default.
    pub fn set_all_tracks(&mut self, kind: Option<TrackKind>, collapsed: bool, reset: bool) {
        for tr in self.project.sequences[self.active].tracks.iter_mut().filter(|t| kind.is_none_or(|k| t.kind == k)) {
            tr.collapsed = collapsed;
            if reset {
                tr.height = Track::DEFAULT_HEIGHT;
            }
        }
        self.status = match (collapsed, reset) {
            (true, _) => "Tracks collapsed".into(),
            (false, true) => "Track heights reset".into(),
            (false, false) => "Tracks expanded".into(),
        };
        self.touch();
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
        self.add_title_kind(false);
    }

    /// 3D text: a title extruded into solid letters, a 3D layer turned a
    /// little so the depth shows.
    pub fn add_3d_text(&mut self) {
        self.add_title_kind(true);
    }

    fn add_title_kind(&mut self, solid: bool) {
        self.add_title_full(solid, None, None);
    }

    /// A 3D shape: `shape` extruded like 3D text, `height` pixels high
    /// (default: 40% of the frame) at `position` (from the frame's centre).
    pub fn add_3d_shape(&mut self, shape: reelforge_core::Shape, place: Option<([f32; 2], f32)>) {
        self.add_title_full(true, Some((std::sync::Arc::new(shape), place)), None);
    }

    /// A 3D model file (OBJ, glTF, FBX, USD, STL, PLY…) as a 3D layer at
    /// the playhead, half the frame high, turned a little so its depth shows.
    pub fn add_3d_model(&mut self, path: &Path) {
        let path = std::path::absolute(path).unwrap_or_else(|_| path.to_owned());
        let mut src = reelforge_core::ModelSource { path: path.clone(), z_up: false, own_colours: true, rev: 0, part_colours: Vec::new() };
        let name = path.file_name().map_or("model".into(), |n| n.to_string_lossy().into_owned());
        match reelforge_render::model3d::cached(&src) {
            Ok(m) => {
                src.z_up = m.z_up;
                let tris = m.indices.len() / 3;
                self.add_title_full(true, None, Some(src));
                self.status = format!("{name}: a 3D model ({tris} triangles). Move and turn it in Transform; light it with lights; size and colours in its settings");
            }
            Err(e) => self.status = format!("Couldn't use {name}: {e}"),
        }
    }

    /// An SVG logo as a 3D shape.
    pub fn import_svg_shape(&mut self, path: &Path) {
        match crate::svg::shape_from_svg(path) {
            Ok(shape) => {
                let name = shape.name.clone();
                self.add_3d_shape(shape, None);
                self.status = format!("{name}: a 3D shape (set its depth, bevel and colours in Title; 0 depth makes it flat)");
            }
            Err(e) => self.status = format!("Couldn't use {}: {e}", path.display()),
        }
    }

    /// Mask `index` of clip `id`, as seen at the playhead, as a new 3D shape
    /// where the mask is (on an unturned layer), the same size.
    pub fn shape_from_mask(&mut self, id: ClipId, index: usize) {
        let Some((_, c)) = self.seq().find_clip(id) else { return };
        let Some(m) = c.masks.get(index) else { return };
        let t = self.mask_time(c);
        let outline = m.outline(t);
        let name = format!("{} shape", if m.name.is_empty() { format!("Mask {}", index + 1) } else { m.name.clone() });
        let Some(shape) = reelforge_core::Shape::new(&name, vec![reelforge_core::ShapePath { contours: vec![outline.clone()], even_odd: false }]) else {
            self.status = "That mask has no area to make a shape from".into();
            return;
        };
        let layer = self.layer_size(c);
        let tr = c.transform;
        let (lo, hi) = outline.iter().fold(([f32::MAX; 2], [f32::MIN; 2]), |(l, h), q| ([l[0].min(q[0]), l[1].min(q[1])], [h[0].max(q[0]), h[1].max(q[1])]));
        let centre = [(lo[0] + hi[0]) * 0.5 - layer[0] * 0.5, (lo[1] + hi[1]) * 0.5 - layer[1] * 0.5];
        let at = [tr.position[0] + centre[0] * tr.scale[0], tr.position[1] + centre[1] * tr.scale[1]];
        let height = ((hi[1] - lo[1]) * tr.scale[1].abs()).max(1.0);
        self.add_3d_shape(shape, Some((at, height)));
        self.status = format!("{name}: a 3D shape from the mask (set its depth, bevel and colours in Title)");
    }

    fn add_title_full(&mut self, solid: bool, shape: Option<(std::sync::Arc<reelforge_core::Shape>, Option<([f32; 2], f32)>)>, model: Option<reelforge_core::ModelSource>) {
        self.commit_live_edit();
        let s = self.seq();
        let rate = s.frame_rate;
        let start = self.playback.playhead.snap(rate);
        let dur = Time::from_secs_f64(5.0).snap(rate);
        let mut spec = TitleSpec::for_frame(s.height);
        let frame_h = s.height as f32;
        if let Some(fonts) = self.fonts() {
            spec.font = reelforge_render::text::default_title_font(&fonts);
        }
        let free = (0..s.tracks.len()).rev().find(|&t| s.tracks[t].kind == TrackKind::Video && !s.tracks[t].locked && !s.overlaps(t, start, dur, &[]));
        let mut cmds: Vec<Box<dyn Command>> = Vec::new();
        let track = match free {
            // A highlighted track decides; otherwise the top track with room.
            _ if self.target_track().is_some_and(|t| s.tracks[t].kind == TrackKind::Video) => {
                let (add, t) = self.layer_track(start, dur);
                cmds.extend(add.map(|a| Box::new(a) as Box<dyn Command>));
                t
            }
            Some(t) => t,
            None => {
                let add = AddTrack::new(&self.project, self.active, TrackKind::Video);
                let t = add.index;
                cmds.push(Box::new(add));
                t
            }
        };
        let id = ClipId(self.project.alloc_id());
        let mut place = None;
        let is_model = model.is_some();
        if let Some(m) = model {
            spec.text = m.path.file_stem().map_or("3D model".into(), |n| n.to_string_lossy().into_owned());
            spec.model = Some(m);
            spec.size = (frame_h * 0.5).round();
            spec.shadow = false;
            // (Used when its own colours are off.)
            spec.color = [200, 200, 200, 255];
        } else if let Some((sh, at)) = &shape {
            spec.text = sh.name.clone();
            spec.shape = Some(sh.clone());
            spec.size = at.map_or((frame_h * 0.4).round(), |a| a.1.round());
            spec.extrude = (spec.size * 0.25).round().max(1.0);
            spec.bevel = (spec.size * 0.02).round();
            spec.shadow = false;
            spec.side_color = Some([spec.color[0] / 2, spec.color[1] / 2, spec.color[2] / 2, spec.color[3]]);
            place = at.map(|a| a.0);
        } else if solid {
            spec.text = "3D Text".into();
            spec.size = (spec.size * 2.0).round();
            spec.extrude = (spec.size * 0.4).round();
            spec.shadow = false;
            // Sides a shade darker than the letters, so the faces read clearly.
            spec.side_color = Some([spec.color[0] / 2, spec.color[1] / 2, spec.color[2] / 2, spec.color[3]]);
        }
        let mut clip = Clip::new_title(id, start, dur, spec);
        if solid {
            clip.transform.three_d = true;
            clip.transform.orientation = if is_model { [-10.0, 30.0] } else { [-15.0, 25.0] };
        }
        if let Some(p) = place {
            // Where the mask was, facing the camera like it.
            clip.transform.position = p;
            clip.transform.orientation = [0.0, 0.0];
        }
        cmds.push(Box::new(AddClip { seq: self.active, track, clip }));
        self.execute(Box::new(Batch { name: if is_model { "Add 3D model" } else if shape.is_some() { "Add 3D shape" } else if solid { "Add 3D text" } else { "Add title" }.into(), cmds }));
        self.follow_new_layer(track);
        self.select_only(Some(id));
        self.status = if solid { "Added 3D text: type its text and set its depth in the Controls panel" } else { "Added a title: type its text in the Controls panel" }.into();
    }

    /// An energy blade on its own transparent layer above everything: as long
    /// as the selected video clip (or 5 seconds from the playhead).
    pub fn add_blade_layer(&mut self) {
        self.add_effect_layer(EffectKind::EnergyBlade, "Add energy blade layer", "Added an energy blade layer: drag its hilt and tip in the Viewer");
    }

    /// A particle emitter on its own transparent layer above everything (as
    /// long as the selected video clip, or 5 seconds from the playhead).
    pub fn add_particles_layer(&mut self) {
        self.add_effect_layer(EffectKind::Particles, "Add particles layer", "Added a particles layer: drag the emitter in the Viewer, and shape it in the Controls panel");
    }

    /// An empty frame-sized layer drawn by effect `kind` (a blade or particles).
    fn add_effect_layer(&mut self, kind: EffectKind, name: &str, status: &str) {
        self.commit_live_edit();
        let s = self.seq();
        let rate = s.frame_rate;
        let (start, dur) = match self.selection.and_then(|id| s.find_clip(id)).filter(|(_, c)| c.title.is_none()) {
            Some((_, c)) if c.media != MediaId::GENERATED || c.nested.is_some() => (c.timeline_start, c.duration),
            _ => (self.playback.playhead.snap(rate), Time::from_secs_f64(5.0).snap(rate)),
        };
        let mut spec = TitleSpec::for_frame(s.height);
        spec.text = String::new();
        // A new track on top (or above the highlighted one), so it's above the footage it's drawn over.
        let (add, track) = self.layer_track(start, dur);
        let id = ClipId(self.project.alloc_id());
        let mut clip = Clip::new_title(id, start, dur, spec);
        clip.effects.push(Effect::new(kind));
        let mut cmds: Vec<Box<dyn Command>> = add.into_iter().map(|a| Box::new(a) as Box<dyn Command>).collect();
        cmds.push(Box::new(AddClip { seq: self.active, track, clip }));
        self.execute(Box::new(Batch { name: name.into(), cmds }));
        self.follow_new_layer(track);
        self.select_only(Some(id));
        self.status = status.into();
    }

    /// A camera, or a light of type `light` (an index into
    /// `effects::light::KINDS`), on a new top track from the playhead to the
    /// end of the sequence (at least 5 seconds).
    pub fn add_scene_object(&mut self, what: reelforge_core::space3d::SceneObject, light: usize) {
        use reelforge_core::space3d::SceneObject;
        self.commit_live_edit();
        let s = self.seq();
        let rate = s.frame_rate;
        let start = self.playback.playhead.snap(rate);
        let dur = (s.duration() - start).max(Time::from_secs_f64(5.0)).snap(rate);
        let frame = [s.width as f32, s.height as f32];
        let any_3d = s.tracks.iter().flat_map(|t| &t.clips).any(|c| c.transform.three_d);
        let (add, track) = self.layer_track(start, dur);
        let id = ClipId(self.project.alloc_id());
        let mut clip = Clip::new_scene(id, start, dur, what, frame);
        if what == SceneObject::Light {
            clip.effects[0].params[reelforge_core::effects::light::KIND].value = light.min(reelforge_core::effects::light::ENVIRONMENT as usize) as f32;
        }
        let name = clip.display_name(&self.project).into_owned();
        let mut cmds: Vec<Box<dyn Command>> = add.into_iter().map(|a| Box::new(a) as Box<dyn Command>).collect();
        cmds.push(Box::new(AddClip { seq: self.active, track, clip }));
        self.execute(Box::new(Batch { name: format!("Add {}", name.to_lowercase()), cmds }));
        self.follow_new_layer(track);
        self.select_only(Some(id));
        self.status = if what == SceneObject::Point {
            "Added a point: parent layers to it in their Transform settings, then move, turn or scale it to move them together".into()
        } else if any_3d {
            format!("Added a {}: its settings are in the Controls panel", name.to_lowercase())
        } else {
            format!("Added a {}. It affects 3D layers: tick 3D layer in a clip's Transform settings", name.to_lowercase())
        };
    }

    /// Where a new generated layer goes: from the playhead to the end of the
    /// sequence (at least 5 seconds), on a new track on top.
    fn new_top_layer(&mut self, clip: impl FnOnce(ClipId, Time, Time) -> Clip, label: &str) -> ClipId {
        self.commit_live_edit();
        let s = self.seq();
        let rate = s.frame_rate;
        let start = self.playback.playhead.snap(rate);
        let dur = (s.duration() - start).max(Time::from_secs_f64(5.0)).snap(rate);
        let (add, track) = self.layer_track(start, dur);
        let id = ClipId(self.project.alloc_id());
        let mut cmds: Vec<Box<dyn Command>> = add.into_iter().map(|a| Box::new(a) as Box<dyn Command>).collect();
        cmds.push(Box::new(AddClip { seq: self.active, track, clip: clip(id, start, dur) }));
        self.execute(Box::new(Batch { name: label.into(), cmds }));
        self.follow_new_layer(track);
        self.select_only(Some(id));
        id
    }

    /// "New layer": a plain plane of colour the size of the frame.
    pub fn add_plane(&mut self, color: [u8; 4]) {
        self.new_top_layer(|id, start, dur| Clip::new_plane(id, start, dur, color), "Add plane");
        self.status = "Added a plane: set its colour in the Controls panel, and scale, mask or add effects to it".into();
    }

    /// "New adjustment layer": its effects change everything below it.
    pub fn add_adjustment_layer(&mut self) {
        self.new_top_layer(Clip::new_adjustment, "Add adjustment layer");
        self.status = "Added an adjustment layer: drag effects onto it to change everything below".into();
    }

    /// Give the selected clips (and what's linked to them) a colour on the
    /// timeline, or their kind's colour again with `None`.
    pub fn set_clip_label(&mut self, color: Option<[u8; 3]>) {
        self.commit_live_edit();
        let s = self.seq();
        let changes: Vec<_> = self
            .selected_with_links()
            .into_iter()
            .filter_map(|id| s.find_clip(id).map(|(_, c)| (id, c.label, color)))
            .filter(|(_, old, new)| old != new)
            .collect();
        if changes.is_empty() {
            return;
        }
        let n = changes.len();
        self.execute(Box::new(reelforge_core::undo::SetLabel { seq: self.active, changes }));
        self.status = match (color, n) {
            (None, 1) => "The clip has its usual colour again".into(),
            (None, n) => format!("{n} clips have their usual colour again"),
            (Some(_), 1) => "Coloured the clip".into(),
            (Some(_), n) => format!("Coloured {n} clips"),
        };
    }

    /// Change a plane's colour.
    pub fn set_plane_color(&mut self, id: ClipId, color: [u8; 4]) {
        self.commit_live_edit();
        let Some(old) = self.seq().find_clip(id).and_then(|(_, c)| c.plane) else { return };
        if old != color {
            self.execute(Box::new(reelforge_core::undo::SetPlane { seq: self.active, id, old, new: color }));
        }
    }

    /// The point clips a clip can be parented to (not itself, nor a point
    /// that already follows it): (id, name).
    pub fn parent_choices(&self, id: ClipId) -> Vec<(ClipId, String)> {
        use reelforge_core::space3d::SceneObject;
        let s = self.seq();
        let follows = |mut p: Option<ClipId>| {
            for _ in 0..8 {
                match p {
                    Some(x) if x == id => return true,
                    Some(x) => p = s.find_clip(x).and_then(|(_, c)| c.parent),
                    None => return false,
                }
            }
            false
        };
        s.tracks
            .iter()
            .rev()
            .flat_map(|t| t.clips.iter().map(move |c| (t, c)))
            .filter(|(_, c)| c.scene == Some(SceneObject::Point) && c.id != id && !follows(c.parent))
            .map(|(t, c)| (c.id, format!("Point on {}", t.name)))
            .collect()
    }

    /// Parent clip `id` to point clip `parent` (or remove its parent). It
    /// stays where it is on screen at the playhead, unless its place is
    /// animated (then its keys are kept as they are, relative to the new parent).
    pub fn set_parent(&mut self, id: ClipId, parent: Option<ClipId>) {
        self.commit_live_edit();
        if parent.is_some_and(|p| !self.parent_choices(id).iter().any(|(c, _)| *c == p)) {
            self.status = "Can't follow that: only points can be parents, and not one that already follows this".into();
            return;
        }
        let Some((_, c)) = self.seq().find_clip(id) else { return };
        if c.parent == parent {
            return;
        }
        // Keep it where it is at the playhead: undo the old parent's place there
        // and apply the new one's inverse, to its transform and every keyframe
        // (its own animation stays; from now on it moves with the new parent).
        use reelforge_core::space3d::Null;
        let t = self.playback.playhead;
        let frame = self.frame_px();
        let mut moved = c.clone();
        moved.parent = parent;
        let old_parent = self.project.parent_of(self.active, c, t).unwrap_or(Null::IDENTITY);
        let new_parent = self.project.parent_of(self.active, &moved, t).unwrap_or(Null::IDENTITY);
        let fix = old_parent.within(&new_parent.inverse());
        reelforge_core::space3d::reparent(&mut moved, &fix, frame);
        let cmd = reelforge_core::undo::SetParent { seq: self.active, id, old: Box::new(c.clone()), new: Box::new(moved) };
        self.execute(Box::new(cmd));
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
            let mut clip = Clip::new_nested(id, index, start, dur, link);
            // A multicam source's clip starts on its first angle.
            clip.angle = self.project.sequences[index].multicam.then(reelforge_core::multicam::Angle::default);
            cmds.push(Box::new(AddClip { seq: self.active, track, clip }));
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
        let mode = self.magnetic_mode(id, mode);
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
        // Lengthening a still picture whose end touches the next clip: push
        // the clips after it along (a ripple) instead of stopping dead.
        let pushes = mode == TrimMode::Normal && edge == Edge::End && delta > Time::ZERO && self.still_touches_next(id);
        let plan = plan_trim(&self.project, self.active, id, edge, if pushes { TrimMode::Ripple } else { mode }, delta);
        if let Some(p) = &plan {
            if p.limited && mode == TrimMode::Normal && !pushes && self.blocked_by_neighbour(id, edge) {
                self.status = "Stopped by the next clip: use the Ripple tool (B) to push the clips after it along".into();
            }
        }
        if let Some(plan) = &plan {
            plan.apply(&mut self.project, self.active);
        }
        self.touch();
        plan
    }

    /// Is clip `id` a still picture (or a title, or a solid) with another clip
    /// starting right where it ends?
    fn still_touches_next(&self, id: ClipId) -> bool {
        let s = self.seq();
        let Some((ti, c)) = s.find_clip(id) else { return false };
        (self.project.is_still(c.media) || c.title.is_some() || c.plane.is_some()) && s.tracks[ti].clips.iter().any(|o| o.id != id && o.timeline_start == c.timeline_end())
    }

    /// Does a neighbouring clip touch clip `id`'s `edge`?
    fn blocked_by_neighbour(&self, id: ClipId, edge: Edge) -> bool {
        let s = self.seq();
        let Some((ti, c)) = s.find_clip(id) else { return false };
        s.tracks[ti].clips.iter().any(|o| o.id != id && if edge == Edge::End { o.timeline_start == c.timeline_end() } else { o.timeline_end() == c.timeline_start })
    }

    /// Turn the pending live edit into one undoable command.
    pub fn commit_live_edit(&mut self) {
        self.end_preset_preview();
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
                // Name the step after what changed.
                let label = match (&old, &new) {
                    (Some(o), Some(Some(n))) if o.smooth != n.smooth && o.points.iter().zip(&n.points).all(|(a, b)| a.point == b.point && a.path == b.path) => "Smooth track",
                    (Some(o), Some(Some(n))) if o.points.iter().zip(&n.points).all(|(a, b)| a.point == b.point && a.path == b.path) => "Tracker sizes",
                    _ => "Move tracker",
                };
                new.map(|new| Box::new(SetTracker { seq, id, old, new, label }) as _)
            }
            LiveEdit::TrackAudio(track, old) => {
                let new = self.seq().tracks.get(track).map(|t| t.audio).filter(|n| *n != old);
                new.map(|new| Box::new(SetTrackAudio { seq, track, old, new }) as _)
            }
            LiveEdit::Dissolve(id, old) => {
                let new = self.seq().find_clip(id).map(|(_, c)| c.dissolve_in).filter(|n| *n != old);
                new.map(|new| Box::new(reelforge_core::undo::SetDissolve { seq, id, old, new }) as _)
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

    /// Move a title to a placement preset: "centre", "lower_third" (bottom left,
    /// inside the title-safe area) or "top". One undo step.
    pub fn place_title(&mut self, id: ClipId, place: &str) {
        self.commit_live_edit();
        let Some((_, c)) = self.seq().find_clip(id) else { return };
        let frame = [self.seq().width as f32, self.seq().height as f32];
        let bounds = self.preview.title_bounds(id);
        let pos = match (place, bounds) {
            ("centre", _) => [0.0, 0.0],
            ("lower_third", Some(b)) => [frame[0] * 0.10 - b[0], frame[1] * 0.85 - b[3]],
            ("top", Some(b)) => [0.0, frame[1] * 0.10 - b[1]],
            _ => {
                self.status = "Show the title in the viewer first, so its size is known".into();
                return;
            }
        };
        let (src, on_clip) = self.key_time(c);
        let Some(v) = self.visual(id) else { return };
        let mut moved = v.anim.eval(&v.transform, src);
        moved.position = pos;
        self.set_transform_live(id, moved);
        let _ = on_clip;
        self.commit_live_edit();
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
            // An image sequence is relinked by any of its frames, at the rate it had.
            let mut seq_name = None;
            let new_path = match reelforge_core::imageseq::rate_of(&old_path).filter(|_| reelforge_core::imageseq::is_sequence(&old_path)) {
                Some(rate) => match reelforge_core::imageseq::detect(&new_path, rate) {
                    Some(seq) => {
                        seq_name = Some(seq.name());
                        seq.media_path()
                    }
                    None => new_path,
                },
                None => new_path,
            };
            match reelforge_media::probe(&new_path) {
                Ok(info) => {
                    if old.info.video.is_some() != info.video.is_some() || old.info.audio.is_some() != info.audio.is_some() {
                        problems.push(format!("{} has different streams from the original", new_path.display()));
                    }
                    let name = seq_name.unwrap_or_else(|| new_path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
                    let new = MediaAsset { id: old.id, path: new_path, name, info, conform: None };
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

    // ---- Presets --------------------------------------------------------------------

    /// Put preset `p` on clips `ids` (adding its effects and masks after
    /// theirs, or in place of them with `replace`): its look on video clips,
    /// its title style on titles, its audio levels on audio clips (and the
    /// audio linked to a video clip), its transition into clips that have a
    /// clip before them. One undo step. Returns how many clips changed.
    pub fn apply_preset(&mut self, ids: &[ClipId], p: &crate::fx_presets::EffectPreset, replace: bool) -> usize {
        self.commit_live_edit();
        let seq = self.active;
        let s = &self.project.sequences[seq];
        let mut cmds: Vec<Box<dyn Command>> = Vec::new();
        let mut changed: Vec<ClipId> = Vec::new();
        let mut done_audio: Vec<ClipId> = Vec::new();
        let mut locked = 0;
        for &id in ids {
            let Some((ti, c)) = s.find_clip(id) else { continue };
            if c.scene.is_some() {
                continue;
            }
            if s.tracks[ti].locked {
                locked += 1;
                continue;
            }
            let video = s.tracks[ti].kind == TrackKind::Video;
            let before = cmds.len();
            if video && p.has_visual() {
                let old = c.visual();
                let mut new = old.clone();
                p.apply(&mut new, c.source_in, c.duration, self.layer_size(c), replace);
                if new != old {
                    cmds.push(Box::new(SetClipVisual { seq, id, old, new }));
                }
            }
            if let (Some(style), Some(old)) = (&p.title, &c.title) {
                let new = TitleSpec { text: old.text.clone(), ..style.clone() };
                if new != *old {
                    cmds.push(Box::new(SetTitle { seq, id, old: old.clone(), new }));
                }
            }
            if p.audio.is_some() {
                // The clip itself if it's audio, else the audio linked to it.
                let targets: Vec<ClipId> = if video { reelforge_core::undo::expand_links(&self.project, seq, &[id]) } else { vec![id] };
                for a in targets {
                    let Some((ati, ac)) = s.find_clip(a) else { continue };
                    if s.tracks[ati].kind != TrackKind::Audio || s.tracks[ati].locked || done_audio.contains(&a) {
                        continue;
                    }
                    done_audio.push(a);
                    let Some(mut new) = p.audio_on(ac.source_in, ac.duration) else { continue };
                    (new.crossfade_in, new.ducking) = (ac.audio.crossfade_in, ac.audio.ducking.clone());
                    if new != ac.audio {
                        cmds.push(Box::new(SetClipAudio { seq, id: a, old: ac.audio.clone(), new }));
                    }
                }
            }
            if let (Some(t), true) = (p.transition, video) {
                if let Some(left) = self.project.clip_before(seq, id).and_then(|l| s.find_clip(l)).map(|(_, l)| l.duration) {
                    let len = t.length.snap(s.frame_rate).min(left.min(c.duration));
                    if c.transition != t.kind {
                        cmds.push(Box::new(reelforge_core::transition::SetTransition { seq, id, old: c.transition, new: t.kind }));
                    }
                    if c.dissolve_in != len {
                        cmds.push(Box::new(reelforge_core::undo::SetDissolve { seq, id, old: c.dissolve_in, new: len }));
                    }
                }
            }
            if cmds.len() > before {
                changed.push(id);
            }
        }
        let n = changed.len();
        if cmds.is_empty() {
            self.status = if locked > 0 {
                "Those clips are on locked tracks".into()
            } else {
                format!("Nothing for \"{}\" to change on {}", p.name, if ids.len() == 1 { "that clip" } else { "those clips" })
            };
            return 0;
        }
        let name = format!("Apply preset {}", p.name);
        self.execute(Box::new(Batch { name, cmds }));
        self.status = match n {
            1 => format!("Applied the preset \"{}\"", p.name),
            n => format!("Applied the preset \"{}\" to {n} clips", p.name),
        };
        n
    }

    /// Show preset `p` on clip `id` without changing the project for good
    /// (hovering it in the effects browser); see [`Self::end_preset_preview`].
    pub fn preview_preset(&mut self, id: ClipId, p: &crate::fx_presets::EffectPreset) {
        self.commit_live_edit();
        let seq = self.active;
        let layer = match self.seq().find_clip(id) {
            Some((_, c)) if c.scene.is_none() => self.layer_size(c),
            _ => return,
        };
        let Some((_, c)) = self.project.sequences[seq].find_clip_mut(id) else { return };
        let (old_v, old_t) = (c.visual(), c.title.clone());
        let mut v = old_v.clone();
        p.apply(&mut v, c.source_in, c.duration, layer, false);
        c.set_visual(v);
        if let (Some(style), Some(t)) = (&p.title, c.title.as_mut()) {
            *t = TitleSpec { text: t.text.clone(), ..style.clone() };
        }
        let clean = !self.is_dirty();
        self.preset_preview = Some((id, old_v, old_t, clean));
        self.touch();
    }

    /// Put back what a preset preview changed.
    pub fn end_preset_preview(&mut self) {
        let Some((id, v, t, clean)) = self.preset_preview.take() else { return };
        let seq = self.active;
        if let Some((_, c)) = self.project.sequences.get_mut(seq).and_then(|s| s.find_clip_mut(id)) {
            c.set_visual(v);
            c.title = t;
        }
        self.touch();
        // Nothing has really changed.
        if clean {
            self.saved_rev = self.rev;
        }
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

    /// Mask from key: trace what key effect `index` of clip `id` keeps at the
    /// playhead (its biggest kept area) into a new freehand mask, to edit,
    /// track, or use as a garbage matte. Returns the new mask's index.
    pub fn mask_from_key(&mut self, id: ClipId, index: usize) -> Option<usize> {
        self.commit_live_edit();
        let (_, c) = self.seq().find_clip(id)?;
        let Some(fx) = c.effects.get(index).filter(|e| reelforge_core::effects::key::matte_base(e.kind).is_some()) else {
            self.status = "Choose a chroma, luma or difference key".into();
            return None;
        };
        let Some(media) = self.project.media(c.media).filter(|m| m.info.video.is_some()).cloned() else {
            self.status = "Mask from key works on a clip with a picture of its own".into();
            return None;
        };
        let layer = self.layer_size(c);
        let src = self.mask_time(c);
        let values = fx.eval(src);
        let at = self.track_time(c);
        // A small picture is plenty for an outline.
        let frame = reelforge_media::FrameSource::open(&media.path, Some((480, 480))).ok().and_then(|mut s| s.frame_at_blocking(at, std::time::Duration::from_secs(10)));
        let Some(f) = frame else {
            self.status = "Couldn't read the frame at the playhead".into();
            return None;
        };
        let (w, h) = (f.width as usize, f.height as usize);
        let rgba = f.pixels.to_rgba();
        let plate = values.table.as_ref().map(|t| (t.rgba.as_slice(), t.width as usize, t.height as usize));
        let alpha = reelforge_core::matte::key_alpha(&values, &rgba, w, h, plate);
        let Some(outline) = reelforge_core::matte::largest_outline(&alpha, w, h, 48) else {
            self.status = "The key keeps nothing here to outline: move the playhead to where the subject is".into();
            return None;
        };
        let (kx, ky) = (layer[0] / w as f32, layer[1] / h as f32);
        let mask = Mask { shape: reelforge_core::mask::MaskShape::Freehand, points: outline.iter().map(|p| [p[0] * kx, p[1] * ky]).collect(), ..Mask::default() };
        let n = mask.points.len();
        let i = self.add_mask(id, mask)?;
        self.status = format!("Mask from key: {n} points round what the key keeps. Edit or track it, or set its Use to a garbage matte");
        Some(i)
    }

    /// Smooth mask `index`'s shape keys (see `Mask::smooth_keys`), `frames`
    /// frames wide. One undo step.
    pub fn smooth_mask(&mut self, id: ClipId, index: usize, frames: f32) {
        self.commit_live_edit();
        if self.seq().is_locked(id) {
            self.status = LOCKED.into();
            return;
        }
        let sigma = frames.max(0.0) as f64 * self.seq().frame_rate.frame_duration().as_secs_f64();
        let Some(mut v) = self.visual(id) else { return };
        let Some(m) = v.masks.get_mut(index) else { return };
        if m.keys.len() < 3 {
            self.status = "Smoothing needs a mask with keyframes: track it (Track shape) or key it first".into();
            return;
        }
        let name = m.name.clone();
        let n = m.smooth_keys(sigma);
        self.set_visual_live(id, v);
        self.commit_live_edit();
        self.status = format!("Smoothed {name}: {n} keyframe{} eased over about {frames:.0} frames (Undo puts them back)", if n == 1 { "" } else { "s" });
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

    /// Set mask `index`'s Bézier handles as seen at the playhead (a keyframe
    /// there if the shape is animated).
    pub fn set_mask_curves_live(&mut self, id: ClipId, index: usize, curves: Vec<[f32; 4]>) {
        let Some((_, c)) = self.seq().find_clip(id) else { return };
        let t = self.mask_time(c);
        let Some(mut v) = self.visual(id) else { return };
        let Some(m) = v.masks.get_mut(index) else { return };
        m.set_curves_at(t, curves);
        self.set_visual_live(id, v);
    }

    /// Turn point `point` of mask `index` into a smooth curve, or (if it's
    /// curved already) back into a sharp corner. One undo step.
    pub fn toggle_mask_point_curve(&mut self, id: ClipId, index: usize, point: usize) {
        self.commit_live_edit();
        let Some((_, c)) = self.seq().find_clip(id) else { return };
        let t = self.mask_time(c);
        let Some(m) = c.masks.get(index) else { return };
        if m.shape == reelforge_core::mask::MaskShape::Ellipse {
            self.status = "An ellipse is already round: use a rectangle or freehand mask for curves".into();
            return;
        }
        let pts = m.points_at(t);
        let mut curves = m.curves_at(t);
        let Some(cur) = curves.get_mut(point) else { return };
        let curved = cur.iter().any(|v| *v != 0.0);
        *cur = if curved { [0.0; 4] } else { reelforge_core::mask::smooth_handles(&pts, point) };
        self.set_mask_curves_live(id, index, curves);
        self.commit_live_edit();
        self.status = if curved { format!("Point {} is a sharp corner", point + 1) } else { format!("Point {} is a smooth curve: drag its handles to shape it", point + 1) };
    }

    /// Add a point to mask `index` halfway along the side after point `point`
    /// (or before it), keeping the shape. One undo step. Returns the new
    /// point's index.
    pub fn add_mask_point(&mut self, id: ClipId, index: usize, point: usize, before: bool) -> Option<usize> {
        self.commit_live_edit();
        let mut v = self.visual(id)?;
        let m = v.masks.get_mut(index)?;
        let n = m.points.len().max(m.keys.first().map_or(0, |k| k.points.len()));
        if n == 0 {
            return None;
        }
        let side = if before { (point + n - 1) % n } else { point };
        let was_rect = m.shape == reelforge_core::mask::MaskShape::Rectangle;
        let Some(new) = m.insert_point_after(side) else {
            self.status = "An ellipse has no points to add: use a freehand mask for that".into();
            return None;
        };
        if was_rect && m.name.starts_with("Rectangle") {
            m.name = m.name.replacen("Rectangle", "Freehand", 1);
        }
        self.set_visual_live(id, v);
        self.commit_live_edit();
        self.status = format!("Added point {}: drag it to shape the mask{}", new + 1, if was_rect { " (it's a freehand mask now)" } else { "" });
        Some(new)
    }

    /// Add a point to mask `index` where its outline (as it is at the
    /// playhead) passes nearest to `at` (layer pixels): right-clicking the
    /// line. One undo step. Returns the new point's index.
    pub fn add_mask_point_near(&mut self, id: ClipId, index: usize, at: [f32; 2]) -> Option<usize> {
        self.commit_live_edit();
        let t = self.seq().find_clip(id).map(|(_, c)| self.mask_time(c))?;
        let mut v = self.visual(id)?;
        let m = v.masks.get_mut(index)?;
        let Some((side, s, _)) = m.nearest_side(t, at) else {
            self.status = "An ellipse has no points to add: use a freehand mask for that".into();
            return None;
        };
        let was_rect = m.shape == reelforge_core::mask::MaskShape::Rectangle;
        let new = m.insert_point_on(side, s)?;
        if was_rect && m.name.starts_with("Rectangle") {
            m.name = m.name.replacen("Rectangle", "Freehand", 1);
        }
        self.set_visual_live(id, v);
        self.commit_live_edit();
        self.status = format!("Added point {}: drag it to shape the mask{}", new + 1, if was_rect { " (it's a freehand mask now)" } else { "" });
        Some(new)
    }

    /// Take point `point` out of mask `index` (a mask keeps at least three). One undo step.
    pub fn delete_mask_point(&mut self, id: ClipId, index: usize, point: usize) -> bool {
        self.commit_live_edit();
        let Some(mut v) = self.visual(id) else { return false };
        let Some(m) = v.masks.get_mut(index) else { return false };
        if !m.remove_point(point) {
            self.status = "A mask needs at least three points".into();
            return false;
        }
        self.set_visual_live(id, v);
        self.commit_live_edit();
        self.status = format!("Deleted point {}", point + 1);
        true
    }

    // ---- Automatic ducking ----------------------------------------------------------

    /// Peaks of a media item's sound (`WAVEFORM_RATE` per second), worked out
    /// now if the background job hasn't got to it yet.
    fn peaks_now(&mut self, id: MediaId) -> Option<Arc<Vec<f32>>> {
        if let Some(p) = self.waveforms.get(&id) {
            return Some(p.clone());
        }
        let m = self.project.media(id)?;
        m.info.audio.as_ref()?;
        let p = Arc::new(reelforge_media::waveform_peaks(&m.path, WAVEFORM_RATE).ok()?);
        self.waveforms.insert(id, p.clone());
        Some(p)
    }

    /// Lower the clips on the `music` audio tracks wherever someone speaks on
    /// the `dialogue` tracks, as volume keyframes (one undo step). Ducking
    /// them again replaces the earlier ducking.
    pub fn auto_duck(&mut self, dialogue: &[usize], music: &[usize], settings: reelforge_core::duck::DuckSettings) {
        use reelforge_core::duck::{envelope, speech_spans};
        self.commit_live_edit();
        let audio = |s: &Sequence, i: usize| s.tracks.get(i).is_some_and(|t| t.kind == TrackKind::Audio);
        let dialogue: Vec<usize> = dialogue.iter().copied().filter(|&i| audio(self.seq(), i) && !music.contains(&i)).collect();
        let music: Vec<usize> = music.iter().copied().filter(|&i| audio(self.seq(), i)).collect();
        if dialogue.is_empty() || music.is_empty() {
            self.status = "Choose at least one dialogue track and one music track (different ones)".into();
            return;
        }
        // The dialogue's level over the whole sequence, from its waveforms and clip volumes.
        let mut sources: Vec<(Clip, Arc<Vec<f32>>)> = Vec::new();
        let clips: Vec<Clip> = dialogue.iter().filter(|&&i| !self.seq().tracks[i].muted).flat_map(|&i| self.seq().tracks[i].clips.clone()).collect();
        for c in clips {
            if let Some(p) = self.peaks_now(c.media) {
                sources.push((c, p));
            }
        }
        if sources.is_empty() {
            self.status = "The dialogue tracks have no sound to listen to".into();
            return;
        }
        let rate = WAVEFORM_RATE as f64;
        let level = |t: Time| -> f32 {
            let mut best = f32::NEG_INFINITY;
            for (c, p) in &sources {
                if !c.contains(t) {
                    continue;
                }
                let m = c.media_at(t).as_secs_f64();
                let i = (m * rate).floor();
                if i < 0.0 {
                    continue;
                }
                let Some(&peak) = p.get(i as usize) else { continue };
                if peak <= 0.0 {
                    continue;
                }
                let db = 20.0 * peak.log10() + c.audio.volume_db_at(c.source_time(t));
                best = best.max(db);
            }
            best
        };
        let end = self.seq().duration();
        let spans = speech_spans(level, Time::ZERO, end, Time::from_secs_f64(1.0 / rate), &settings);
        let env = envelope(&spans, &settings);
        let (mut cmds, mut ducked, mut locked): (Vec<Box<dyn Command>>, usize, usize) = (Vec::new(), 0, 0);
        for &ti in &music {
            for c in &self.seq().tracks[ti].clips {
                if self.seq().is_locked(c.id) {
                    locked += 1;
                    continue;
                }
                let to_src = |t: Time| c.source_time(t);
                let local: Vec<(Time, f32)> = env.iter().map(|&(t, d)| (to_src(t), d)).collect();
                let mut a = c.audio.clone();
                a.set_ducking(local, (c.source_in, c.source_in + c.duration));
                if a != c.audio {
                    if a.is_ducked() {
                        ducked += 1;
                    }
                    cmds.push(Box::new(SetClipAudio { seq: self.active, id: c.id, old: c.audio.clone(), new: a }));
                }
            }
        }
        let talk: f64 = spans.iter().map(|(a, b)| (*b - *a).as_secs_f64()).sum();
        if !cmds.is_empty() {
            self.execute(Box::new(Batch { name: "Duck music under dialogue".into(), cmds }));
        }
        let lockednote = if locked > 0 { format!(" ({locked} locked clip{} left alone)", if locked == 1 { "" } else { "s" }) } else { String::new() };
        self.status = if spans.is_empty() {
            format!("No speech found louder than {:.0} dB: lower the threshold to duck under quieter dialogue{lockednote}", settings.threshold_db)
        } else {
            format!(
                "Ducked {ducked} music clip{} by {:.0} dB under {} stretch{} of speech ({:.1} s){lockednote}. The ducking is ordinary volume keyframes: fine-tune them, or duck again to replace it",
                if ducked == 1 { "" } else { "s" },
                -settings.amount_db.min(0.0),
                spans.len(),
                if spans.len() == 1 { "" } else { "es" },
                talk
            )
        };
    }

    /// Take automatic ducking off the clips on the `music` tracks (one undo
    /// step); volume keyframes added by hand stay.
    pub fn remove_ducking(&mut self, music: &[usize]) {
        self.commit_live_edit();
        let mut cmds: Vec<Box<dyn Command>> = Vec::new();
        for &ti in music {
            let Some(t) = self.seq().tracks.get(ti) else { continue };
            for c in t.clips.iter().filter(|c| c.audio.is_ducked() && !self.seq().is_locked(c.id)) {
                let mut a = c.audio.clone();
                a.set_ducking(Vec::new(), (c.source_in, c.source_in + c.duration));
                cmds.push(Box::new(SetClipAudio { seq: self.active, id: c.id, old: c.audio.clone(), new: a }));
            }
        }
        let n = cmds.len();
        if n > 0 {
            self.execute(Box::new(Batch { name: "Remove ducking".into(), cmds }));
        }
        self.status = match n {
            0 => "Nothing to take off: those tracks aren't ducked".into(),
            1 => "Took the ducking off 1 clip".into(),
            n => format!("Took the ducking off {n} clips"),
        };
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
            TrackMode::Planar => "Drag corners 1–4 onto the corners of a flat surface (top left, top right, bottom right, bottom left), then press Track forward".into(),
            TrackMode::Mesh => "Drag the grid's four corners onto the surface (the grid fills them), then press Track forward".into(),
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
        self.tracking = Some(TrackJob { seq: self.active, clip: id, rx, cancel, progress: 0.0, mask: None, through: None, camera: false, fill: None, roto: None });
        self.status = "Tracking…".into();
    }

    /// Rotoscoping: track what's inside mask `index` of clip `id` from the
    /// playhead to the end of the clip (or back to its start), in the
    /// background, then key the mask's shape at every frame, moving as
    /// `motion` allows.
    pub fn start_mask_tracking(&mut self, id: ClipId, index: usize, forward: bool, motion: reelforge_core::track::MaskMotion) {
        self.commit_live_edit();
        if self.tracking.is_some() {
            return;
        }
        let frame = self.seq().frame_rate.frame_duration();
        let Some((_, c)) = self.seq().find_clip(id) else { return };
        if self.seq().is_locked(id) {
            self.status = LOCKED.into();
            return;
        }
        let Some(mask) = c.masks.get(index) else { return };
        // The footage followed: the clip's own, or (an adjustment layer, a
        // solid, a title) the topmost footage below it at the playhead.
        let own = self.project.media(c.media).is_some_and(|m| m.info.video.is_some()) && c.title.is_none() && c.nested.is_none() && !self.project.is_still(c.media);
        let src_id = if own { id } else { match self.footage_below(id) { Some(b) => b, None => {
            self.status = "Mask tracking follows moving footage: there's none in this clip, or below it at the playhead".into();
            return;
        } } };
        let Some((_, src)) = self.seq().find_clip(src_id) else { return };
        let Some(media) = self.project.media(src.media).cloned() else { return };
        // The stretch both clips cover.
        let (start, end) = (c.timeline_start.max(src.timeline_start), c.timeline_end().min(src.timeline_end()));
        let t = self.playback.playhead.max(start).min(end - frame);
        let from = src.media_at(t);
        let until = if forward { src.media_at(end - frame) } else { src.media_at(start) };
        if from == until {
            self.status = format!("Nothing to track: the playhead is at the clip's {}", if forward { "end" } else { "start" });
            return;
        }
        // The shape where tracking starts (mask keys are in clip time), in the
        // footage's pixels.
        let at = if own { c.clip_time_of_media(from).unwrap_or_else(|| c.source_time(t)) } else { c.source_time(t) };
        let outline = mask.outline(at);
        let outline = if own {
            outline
        } else {
            let Some(to_src) = self.layer_to_layer(c, src, t) else { return };
            outline.into_iter().map(to_src).collect()
        };
        let probe = reelforge_core::mask::Mask { points: outline.clone(), shape: reelforge_core::mask::MaskShape::Freehand, ..Default::default() };
        let Some(quad) = reelforge_core::track::mask_bounds(&probe, Time::ZERO) else {
            self.status = "The mask is too small to track: draw it around what it should follow".into();
            return;
        };
        let name = mask.name.clone();
        let src_name = media.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        // How far it may move in a frame: a sixteenth of the picture's width.
        let search = (media.info.video.as_ref().map_or(1920, |v| v.width) as f32 / 16.0).max(24.0);
        let (tx, rx) = unbounded();
        let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop = cancel.clone();
        self.playback.pause();
        std::thread::Builder::new()
            .name("mask tracker".into())
            .spawn(move || {
                let progress_tx = tx.clone();
                let grid = (motion == reelforge_core::track::MaskMotion::Bend).then(|| reelforge_core::track::mask_grid(quad));
                let r = reelforge_media::track_outline(&media.path, &media.info, quad, grid, outline, search, from, until, &stop, |p| {
                    let _ = progress_tx.send(TrackMsg::Progress(p));
                });
                let _ = tx.send(TrackMsg::Done(r.map_err(|e| format!("{e:#}"))));
            })
            .expect("mask tracker thread");
        self.tracking = Some(TrackJob { seq: self.active, clip: id, rx, cancel, progress: 0.0, mask: Some((index, motion)), through: (!own).then_some(src_id), camera: false, fill: None, roto: None });
        self.status = if own { format!("Tracking {name}…") } else { format!("Tracking {name} through the footage below ({src_name})…") };
    }

    /// The topmost clip of moving footage below clip `id` at the playhead
    /// (what an adjustment layer's or a solid's mask is tracked through).
    pub fn footage_below(&self, id: ClipId) -> Option<ClipId> {
        let s = self.seq();
        let (ti, c) = s.find_clip(id)?;
        let t = self.playback.playhead.max(c.timeline_start).min(c.timeline_end());
        s.tracks[..ti]
            .iter()
            .rev()
            .filter(|tr| tr.kind == TrackKind::Video && !tr.muted)
            .flat_map(|tr| tr.clips.iter())
            .find(|b| {
                b.timeline_start <= t
                    && t < b.timeline_end()
                    && b.title.is_none()
                    && b.nested.is_none()
                    && self.project.media(b.media).is_some_and(|m| m.info.video.is_some())
                    && !self.project.is_still(b.media)
            })
            .map(|b| b.id)
    }

    /// Clip `c`'s layer pixels → frame pixels at timeline time `t` (an
    /// adjustment layer covers the frame and doesn't move).
    fn layer_frame_map(&self, c: &Clip, t: Time) -> reelforge_core::transform::LayerMap {
        let tr = if c.adjustment { Transform::default() } else { self.project.world_transform(self.active, c, t) };
        tr.layer_map(self.layer_size(c), self.frame_px())
    }

    /// Clip `a`'s layer pixels → clip `b`'s, through the frame, at timeline time `t`.
    fn layer_to_layer(&self, a: &Clip, b: &Clip, t: Time) -> Option<impl Fn([f32; 2]) -> [f32; 2]> {
        let (la, lb) = (self.layer_size(a), self.layer_size(b));
        let ma = self.layer_frame_map(a, t);
        let mb = self.layer_frame_map(b, t).inverse()?;
        Some(move |p: [f32; 2]| {
            let q = ma.apply([p[0] - la[0] / 2.0, p[1] - la[1] / 2.0]);
            let r = mb.apply(q);
            [r[0] + lb[0] / 2.0, r[1] + lb[1] / 2.0]
        })
    }

    pub fn cancel_tracking(&mut self) {
        if let Some(j) = &self.tracking {
            j.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    fn poll_tracking(&mut self) {
        let Some(job) = &mut self.tracking else { return };
        let mut done = None;
        let mut camera = None;
        let mut fill = None;
        let mut roto = None;
        while let Ok(m) = job.rx.try_recv() {
            match m {
                TrackMsg::Progress(p) => job.progress = p,
                TrackMsg::Done(r) => done = Some(r),
                TrackMsg::CameraDone(r) => camera = Some(r),
                TrackMsg::FillDone(r) => fill = Some(r),
                TrackMsg::RotoDone(r) => roto = Some(r),
            }
        }
        if let Some(r) = roto {
            let job = self.tracking.take().unwrap();
            self.finish_roto(&job, r);
            return;
        }
        if let Some(r) = fill {
            let job = self.tracking.take().unwrap();
            self.finish_object_removal(&job, r);
            return;
        }
        if let Some(r) = camera {
            let job = self.tracking.take().unwrap();
            self.finish_camera_tracking(&job, r);
            return;
        }
        let Some(result) = done else { return };
        let job = self.tracking.take().unwrap();
        match result {
            Err(e) => self.status = format!("Tracking failed: {e}"),
            Ok((samples, end)) if job.mask.is_some() => self.finish_mask_tracking(&job, samples, end),
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
                    reelforge_media::TrackEnd::Lost(t, reelforge_media::tracker::SURFACE) => format!(
                        "Tracked {n} frames, then lost the surface at {} (source time): too much of it was covered, blurred or out of frame. Go there, drag the corners back onto it and track again",
                        t.timecode(rate)
                    ),
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

    /// A mask tracking run is done: key the mask's shape (one undo step).
    fn finish_mask_tracking(&mut self, job: &TrackJob, corners: Vec<Vec<reelforge_core::track::TrackSample>>, end: reelforge_media::TrackEnd) {
        let Some((index, motion)) = job.mask else { return };
        let n = corners.first().map_or(0, |s| s.len().saturating_sub(1));
        let Some((_, c)) = self.project.sequences.get(job.seq).and_then(|s| s.find_clip(job.clip)) else { return };
        let Some(mask) = c.masks.get(index) else { return };
        let rate = self.project.sequences[job.seq].frame_rate;
        if n == 0 {
            self.status = match end {
                reelforge_media::TrackEnd::Lost(t, _) => format!("Lost what's inside {} straight away, at {} (source time): draw it around something with more detail", mask.name, t.timecode(rate)),
                _ => "Nothing tracked".into(),
            };
            return;
        }
        // Tracked through the footage below: each sample back into this
        // clip's pixels and time, through the frame as both are at that moment.
        let corners = match job.through {
            None => corners,
            Some(b) => {
                let Some((_, src)) = self.project.sequences.get(job.seq).and_then(|s| s.find_clip(b)) else { return };
                let mut out = corners.clone();
                for (k, list) in corners.iter().enumerate() {
                    for (i, smp) in list.iter().enumerate() {
                        let Some(tl) = src.timeline_of_media(smp.at) else { continue };
                        let Some(back) = self.layer_to_layer(src, c, tl) else { continue };
                        out[k][i] = reelforge_core::track::TrackSample { at: c.source_time(tl), pos: back(smp.pos), score: smp.score };
                    }
                }
                out
            }
        };
        let Some(m) = reelforge_core::track::mask_from_region(c, mask, &corners, motion) else {
            self.status = "Couldn't follow the mask: what's inside it turned too far".into();
            return;
        };
        let name = m.name.clone();
        let old = c.visual();
        let mut new = old.clone();
        new.masks[index] = m;
        self.execute(Box::new(SetClipVisual { seq: job.seq, id: job.clip, old, new }));
        self.status = match end {
            reelforge_media::TrackEnd::Done => format!("{name} tracked over {n} frames: check it, fix any frame by dragging its points, and track on from there"),
            reelforge_media::TrackEnd::Cancelled => format!("Stopped: {name} tracked over {n} frames"),
            reelforge_media::TrackEnd::Lost(t, _) => format!(
                "{name} tracked over {n} frames, then lost what's inside it at {} (source time): go there, fix the shape and track on",
                t.timecode(rate)
            ),
        };
    }

    /// Take a clip's mesh warp off (it goes back to its transform).
    pub fn remove_mesh_warp(&mut self, id: ClipId) {
        self.commit_live_edit();
        let Some((_, c)) = self.seq().find_clip(id) else { return };
        if c.mesh.is_none() {
            return;
        }
        if self.seq().is_locked(id) {
            self.status = LOCKED.into();
            return;
        }
        let old = c.visual();
        let new = ClipVisual { mesh: None, ..old.clone() };
        self.execute(Box::new(SetClipVisual { seq: self.active, id, old, new }));
        self.status = "Mesh warp removed".into();
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

    /// Make energy blade `index` (an effect of `target`, which may be the
    /// tracked clip itself or a layer over it) follow the tracked motion: its
    /// hilt and tip keep where they are at the playhead and move, turn and
    /// stretch with the tracked points, as keyframes over the tracked span.
    pub fn apply_track_to_blade(&mut self, tracked: ClipId, target: ClipId, index: usize, axes: Axes) {
        use reelforge_core::effects::blade;
        if !axes.x && !axes.y && !axes.rotation && !axes.scale {
            self.status = "Nothing to apply: tick at least one of X, Y, rotation or scale".into();
            return;
        }
        self.commit_live_edit();
        let s = self.seq();
        let (Some((_, a)), Some((_, b))) = (s.find_clip(tracked), s.find_clip(target)) else { return };
        let Some(track) = &a.tracker else { return };
        let Some(fx) = b.effects.get(index).filter(|f| matches!(f.kind, EffectKind::EnergyBlade | EffectKind::PatchReplacer)) else { return };
        // A patch replacer's target moves (its source, measured from the target, comes with it).
        let patch = fx.kind == EffectKind::PatchReplacer;
        let params = if patch {
            use reelforge_core::effects::patch as pr;
            [pr::X, pr::Y, pr::X, pr::Y]
        } else {
            [blade::HILT_X, blade::HILT_Y, blade::TIP_X, blade::TIP_Y]
        };
        if s.is_locked(target) {
            self.status = LOCKED.into();
            return;
        }
        let (la, lb) = (self.layer_size(a), self.layer_size(b));
        let frame = [s.width as f32, s.height as f32];
        let same = tracked == target;
        // Timeline time ↔ the two clips' layer pixels (via the frame, for another layer).
        let to_seq = |m: Time| a.timeline_of_media(m).unwrap_or(a.timeline_start);
        let clamp_b = |t: Time| t.max(b.timeline_start).min(b.timeline_end());
        let into_a = |t: Time, p: [f32; 2]| -> Option<[f32; 2]> {
            if same {
                return Some(p);
            }
            let q = b.transform_at(clamp_b(t)).layer_map(lb, frame).apply([p[0] - lb[0] / 2.0, p[1] - lb[1] / 2.0]);
            let r = a.transform_at(t).layer_map(la, frame).inverse()?.apply(q);
            Some([r[0] + la[0] / 2.0, r[1] + la[1] / 2.0])
        };
        let into_b = |t: Time, p: [f32; 2]| -> Option<[f32; 2]> {
            if same {
                return Some(p);
            }
            let q = a.transform_at(t).layer_map(la, frame).apply([p[0] - la[0] / 2.0, p[1] - la[1] / 2.0]);
            let r = b.transform_at(clamp_b(t)).layer_map(lb, frame).inverse()?.apply(q);
            Some([r[0] + lb[0] / 2.0, r[1] + lb[1] / 2.0])
        };
        // The blade where it is at the playhead, in the tracked clip's pixels.
        let ref_media = self.track_time(a);
        let ref_seq = to_seq(ref_media);
        let src = b.source_time(clamp_b(ref_seq));
        let pct = |i: usize| fx.value_at(i, src) / 100.0;
        let ends_b = [[pct(params[0]) * lb[0], pct(params[1]) * lb[1]], [pct(params[2]) * lb[0], pct(params[3]) * lb[1]]];
        let (Some(h), Some(t)) = (into_a(ref_seq, ends_b[0]), into_a(ref_seq, ends_b[1])) else { return };
        let Some(path) = reelforge_core::track::follow_blade(track, [h, t], ref_media, axes) else {
            self.status = if track.mode == TrackMode::Mesh {
                "A mesh track can't carry a blade: use a single, two-point, four-point or planar track".into()
            } else {
                "Nothing to apply: track the points first".into()
            };
            return;
        };
        let mut keys: Vec<(Time, [f32; 4])> = Vec::new();
        for (m, [h, t]) in path {
            let at = to_seq(m);
            if at < b.timeline_start || at > b.timeline_end() {
                continue;
            }
            let (Some(h), Some(t)) = (into_b(at, h), into_b(at, t)) else { continue };
            keys.push((b.source_time(at), [h[0] / lb[0] * 100.0, h[1] / lb[1] * 100.0, t[0] / lb[0] * 100.0, t[1] / lb[1] * 100.0]));
        }
        let (Some(first), Some(last)) = (keys.first().map(|k| k.0), keys.last().map(|k| k.0)) else {
            self.status = "Nothing to apply: the track doesn't overlap that layer".into();
            return;
        };
        let (lo, hi) = (first.min(last), first.max(last));
        let mut v = b.visual();
        let e = &mut v.effects[index];
        e.normalize();
        for (j, i) in params.into_iter().enumerate().take(if patch { 2 } else { 4 }) {
            let p = &mut e.params[i];
            // A part left out keeps the blade's own value (and keys).
            let moves = if j % 2 == 0 { axes.x || axes.rotation || axes.scale } else { axes.y || axes.rotation || axes.scale };
            if !moves {
                continue;
            }
            p.keys.0.retain(|k| k.at < lo || k.at > hi);
            for (at, val) in &keys {
                p.keys.set(*at, val[j]);
            }
        }
        let what = if patch { "The patch" } else { "The blade" };
        let name = if same { what.to_string() } else { format!("{what} on {}", b.display_name(&self.project)) };
        self.set_visual_live(target, v);
        self.commit_live_edit();
        self.status = format!("{name} now follows the tracked motion ({} keyframes)", keys.len());
    }

    /// Make `target` follow the tracked motion of `tracked` (or, if they're the
    /// same clip, hold it steady), as keyframes. `smoothing` (frames) keeps
    /// slow camera moves when stabilising.
    pub fn apply_track(&mut self, tracked: ClipId, target: ClipId, smoothing: f32) {
        self.apply_track_axes(tracked, target, smoothing, Axes::ALL);
    }

    /// [`Self::apply_track`] with only the chosen parts of the motion
    /// (X and Y position; rotation and scale for two-point tracks).
    pub fn apply_track_axes(&mut self, tracked: ClipId, target: ClipId, smoothing: f32, axes: Axes) {
        self.apply_track_with(tracked, target, smoothing, axes, reelforge_core::track::StabZoom::Off);
    }

    /// [`Self::apply_track_axes`], and when stabilising, zooming in (`zoom`)
    /// to hide the edges the counter-motion uncovers.
    pub fn apply_track_with(&mut self, tracked: ClipId, target: ClipId, smoothing: f32, axes: Axes, zoom: reelforge_core::track::StabZoom) {
        if !axes.x && !axes.y && !axes.rotation && !axes.scale {
            self.status = "Nothing to apply: tick at least one of X, Y, rotation or scale".into();
            return;
        }
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
        let opts = ApplyOptions { stabilise, smoothing: if stabilise { smoothing } else { 0.0 }, axes, zoom: if stabilise { zoom } else { Default::default() } };
        let mode = track.mode;
        if b.scene == Some(reelforge_core::space3d::SceneObject::Point) && !stabilise {
            if s.is_locked(target) {
                self.status = LOCKED.into();
                return;
            }
            let parent_at = |t: Time| self.project.parent_of(self.active, b, t);
            let Some(fx) = reelforge_core::track::follow_point(a, track, layer, frame, b, parent_at, axes) else {
                self.status = if track.frames().is_empty() {
                    "Nothing to apply: track the points first".into()
                } else if !axes.x && !axes.y && (mode == TrackMode::Position || (!axes.rotation && !axes.scale)) {
                    "Nothing to apply: tick X or Y position".into()
                } else {
                    "Nothing to apply: the track doesn't overlap that point".into()
                };
                return;
            };
            let old = b.visual();
            let mut new = old.clone();
            new.effects[0] = fx;
            let kids = s.tracks.iter().flat_map(|t| &t.clips).filter(|c| c.parent == Some(target)).count();
            self.execute(Box::new(SetClipVisual { seq: self.active, id: target, old, new }));
            let what = if mode == TrackMode::Position { "point" } else { "position, rotation and scale" };
            self.status = match kids {
                0 => format!("The point now follows the tracked {what}: parent layers to it (Controls → Layer → Parent) to make them follow"),
                1 => format!("The point now follows the tracked {what}, and so does the layer parented to it"),
                n => format!("The point now follows the tracked {what}, and so do the {n} layers parented to it"),
            };
            return;
        }
        if mode == TrackMode::Mesh {
            if stabilise {
                self.status = "A mesh track can't stabilise its own clip: apply it to another layer or a mask".into();
                return;
            }
            let Some(mesh) = reelforge_core::track::follow_mesh(a, track, layer, frame, b, axes) else {
                self.status = if track.frames().is_empty() { "Nothing to apply: track the grid first".into() } else { "Nothing to apply: the track doesn't overlap that clip".into() };
                return;
            };
            if s.is_locked(target) {
                self.status = LOCKED.into();
                return;
            }
            let old = b.visual();
            let new = ClipVisual { mesh: Some(mesh), ..old.clone() };
            let name = b.display_name(&self.project).into_owned();
            self.execute(Box::new(SetClipVisual { seq: self.active, id: target, old, new }));
            self.status = format!("{name} is now warped over the tracked grid");
            return;
        }
        let Some(anim) = reelforge_core::track::follow_keys(a, track, layer, frame, b, target_layer, opts) else {
            self.status = if track.frames().is_empty() {
                "Nothing to apply: track every point first".into()
            } else if !axes.x && !axes.y && !(mode == TrackMode::Transform && (axes.rotation || axes.scale)) {
                "Nothing to apply: tick X or Y position".into()
            } else {
                "Nothing to apply: the track doesn't overlap that clip".into()
            };
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
            (true, TrackMode::CornerPin | TrackMode::Planar) => "Stabilised: the clip is corner-pinned against the tracked perspective".into(),
            (true, _) => "Stabilised: the clip now moves against the tracked motion (scale it up a little to hide the edges)".into(),
            (false, TrackMode::CornerPin | TrackMode::Planar) => format!("{name} is now corner-pinned to the tracked surface"),
            (false, TrackMode::Transform) => format!("{name} now follows the tracked position, rotation and scale"),
            (false, TrackMode::Position) => format!("{name} now follows the tracked point"),
            (false, TrackMode::Mesh) => String::new(), // handled above
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
        let summary = self.summary();
        self.recent.add(path, summary);
        self.saved_rev = self.rev;
        if let Some(s) = &mut self.session {
            s.set_project(Some(path.to_owned()));
            s.autosaved_rev = self.rev;
        }
        self.auto_version();
        Ok(())
    }

    /// Open an autosave file as a new, unsaved project (so Save asks where,
    /// rather than writing over the autosave).
    pub fn open_autosave(&mut self, path: PathBuf) {
        self.open_project_path(path.clone());
        if self.project_path.as_deref() == Some(path.as_path()) {
            // An autosave isn't a project of yours to come back to.
            self.recent.remove(&path);
            self.project_path = None;
            self.saved_rev = self.rev.wrapping_sub(1);
            if let Some(s) = &mut self.session {
                s.set_project(None);
            }
            self.status = format!("Opened the autosave {}: save it to keep it", path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
        }
    }

    /// The runs of frames (first, last + 1) of the active sequence that Render
    /// preview has finished, looking at every `step`th frame.
    pub fn rendered_runs(&self, step: i64) -> Vec<(i64, i64)> {
        let mut runs: Vec<(i64, i64)> = Vec::new();
        if self.render_cache.stats().1 == 0 {
            return runs;
        }
        let step = step.max(1);
        let sources = self.decode_paths();
        let s = self.seq();
        let rate = s.frame_rate;
        let last = reelforge_export::frame_range(rate, Time::ZERO, s.duration()).1;
        let mut f = 0;
        while f < last {
            let t = Time::from_frame(f, rate);
            if self.render_cache.has(self.active, f, reelforge_export::frame_recipe(&self.project, self.active, t, &sources)) {
                match runs.last_mut() {
                    Some(r) if r.1 >= f => r.1 = (f + step).min(last),
                    _ => runs.push((f, (f + step).min(last))),
                }
            }
            f += step;
        }
        runs
    }

    pub fn open_project_path(&mut self, path: PathBuf) {
        match load_project(&path) {
            Ok(p) => {
                self.replace_project(p, Some(path.clone()));
                self.saved_rev = self.rev;
                self.status = format!("Opened {}", path.display());
                let summary = self.summary();
                self.recent.add(&path, summary);
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
        if self.disk_checked.is_none_or(|t| t.elapsed() > std::time::Duration::from_secs(15)) {
            self.check_disk();
        }
        if self.rev != self.proxied_rev {
            // (Not started by themselves while the drive is nearly full.)
            self.proxies.refresh(&self.project, self.prefs.auto_proxies && self.disk_low.is_none());
            self.proxied_rev = self.rev;
        }
        if self.proxies.poll() {
            self.status = "A proxy is ready: the viewer uses it now".into();
        }
        // (Not while a preset is only being previewed.)
        if let Some(s) = self.session.as_mut().filter(|_| self.preset_preview.is_none()) {
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
        // Rendered previews are of the edit, never the angle grid.
        reelforge_core::multicam::set_grid(false);
        self.step_render_job(&sources);
        self.apply_view();
        reelforge_core::multicam::set_grid(self.multicam_grid);
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

/// Delete bad points and re-solve: points fitting worse than a limit go,
    /// the camera is solved again from the rest of the followed details (in
    /// the background), keeping the camera clip it made; undo brings the old
    /// solve back. A solve without its details says to analyse again.
    #[test]
    fn deleting_bad_points_solves_the_camera_again() {
        use reelforge_core::camtrack::{CamPose, CameraTrack, ScenePoint, SolveSource, Track2d};
        let mut e = editor();
        e.new_project_with(1280, 720, reelforge_core::FrameRate::new(25, 1));
        e.add_plane([40, 40, 40, 255]);
        let id = e.selection.unwrap();
        let n = 40;
        let times: Vec<Time> = (0..n).map(|k| Time::from_secs_f64(k as f64 / 25.0)).collect();
        let cameras: Vec<CamPose> = (0..n).map(|k| CamPose { position: [-300.0 + 15.0 * k as f32, -80.0, -1200.0 + 4.0 * k as f32], target: [0.0, 0.0, 300.0], roll: 0.0 }).collect();
        let mut ct = CameraTrack { times, cameras, fov: 45.0, frame: [1280.0, 720.0], points: Vec::new(), error: 0.9, tripod: false, placed: n as u32, camera: None, source: None };
        // A floor and a back wall of details; the last 10 drift off their spot (on something moving).
        let mut seed = 99u64;
        let mut uni = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed >> 11) as f32 / (1u64 << 53) as f32
        };
        let (mut tracks, mut ids, mut floor) = (Vec::new(), Vec::new(), Vec::new());
        for k in 0..160 {
            let p = if k % 2 == 0 { [uni() * 1600.0 - 800.0, 250.0, uni() * 900.0] } else { [uni() * 1600.0 - 800.0, 250.0 - uni() * 700.0, 900.0] };
            let bad = k >= 150;
            let pos: Vec<[f32; 2]> = (0..n).map_while(|i| ct.project(i, p).map(|q| if bad { [q[0] + 1.5 * i as f32, q[1] - 0.8 * i as f32] } else { q })).collect();
            if pos.len() < n {
                continue;
            }
            ids.push(tracks.len() as u32);
            tracks.push(Track2d { start: 0, pos });
            if k % 2 == 0 && !bad {
                floor.push(ct.points.len());
            }
            ct.points.push(ScenePoint { pos: p, error: if bad { 6.0 } else { 0.2 }, first: 0, last: (n - 1) as u32 });
        }
        let total = ct.points.len();
        assert!(total > 100, "{total}");
        let without = ct.clone();
        ct.source = Some(Box::new(SolveSource { tracks, ids, known_lens: true, ground: None }));
        ct.camera = Some(ClipId(424242));
        e.project.sequences[0].find_clip_mut(id).unwrap().1.camera_track = Some(Box::new(ct.clone()));
        // A ground plane on the floor's points.
        e.set_camera_ground(id, &floor[..12]);
        let ct = *e.seq().find_clip(id).unwrap().1.camera_track.clone().unwrap();
        assert!(ct.source.as_ref().unwrap().ground.as_ref().is_some_and(|g| g.0.len() == 12));
        assert!(floor[..12].iter().all(|&k| ct.points[k].pos[1].abs() < 1.0), "the floor is the ground");
        // Nothing worse than 10 px: nothing to do.
        e.resolve_camera(id, &[], Some(10.0));
        assert!(e.tracking.is_none() && e.status.contains("No points"), "{}", e.status);
        e.resolve_camera(id, &[], Some(1.0));
        assert!(e.status.contains("Deleted 10 points"), "{}", e.status);
        let t0 = std::time::Instant::now();
        while e.tracking.is_some() && t0.elapsed().as_secs() < 60 {
            e.poll_tracking();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let new = e.seq().find_clip(id).unwrap().1.camera_track.clone().unwrap();
        assert!(new.points.len() <= total - 10, "{} points", new.points.len());
        assert!(new.error < 0.2, "error {}: {}", new.error, e.status);
        assert!((new.fov - 45.0).abs() < 0.01, "kept the known lens: {}", new.fov);
        assert_eq!(new.camera, Some(ClipId(424242)), "kept the camera it made");
        let src = new.source.as_ref().unwrap();
        assert_eq!(src.tracks.len(), total - 10);
        assert!(src.known_lens);
        // The ground set again, through the same details: the floor's points are on it.
        let g = src.ground.clone().expect("ground kept");
        assert_eq!(g.0.len(), 12);
        let on: Vec<f32> = new.points.iter().zip(&src.ids).filter(|(_, i)| g.0.contains(i)).map(|(p, _)| p.pos[1]).collect();
        assert_eq!(on.len(), 12);
        assert!(on.iter().all(|y| y.abs() < 2.0), "{on:?}");
        // So are the rest of the floor's (the camera's height kept, give or take).
        assert!(new.points.iter().filter(|p| p.pos[1].abs() < 3.0).count() > 50);
        // The fit per frame (worked out once), and going to a frame.
        let fit = e.camera_fit(id).unwrap();
        assert_eq!(fit.len(), 40);
        assert!(fit.iter().all(|f| f.is_some_and(|f| f < 0.5)), "{fit:?}");
        assert!(Arc::ptr_eq(&fit, &e.camera_fit(id).unwrap()), "cached");
        e.seek_camera_frame(id, 10);
        assert_eq!(e.playback.playhead, Time::from_secs_f64(10.0 / 25.0));
        // Undo: the old solve.
        e.undo();
        assert_eq!(e.seq().find_clip(id).unwrap().1.camera_track.as_deref(), Some(&ct));
        // Without the details it came from.
        e.project.sequences[0].find_clip_mut(id).unwrap().1.camera_track = Some(Box::new(without));
        e.resolve_camera(id, &[0], None);
        assert!(e.tracking.is_none() && e.status.contains("Analyse again"), "{}", e.status);
    }

    #[test]
    fn a_solved_camera_makes_a_camera_and_layers_that_sit_in_the_shot() {
        use reelforge_core::camtrack::{CamPose, CameraTrack, ScenePoint};
        let mut e = editor();
        e.new_project_with(1920, 1080, reelforge_core::FrameRate::new(25, 1));
        e.add_plane([40, 40, 40, 255]);
        let id = e.selection.unwrap();
        // A camera sliding right and down a little, looking at the origin; a
        // floor (y = 300) and a few points off it.
        let n = 50;
        let times: Vec<Time> = (0..n).map(|k| Time::from_secs_f64(k as f64 / 25.0)).collect();
        let cameras: Vec<CamPose> = (0..n).map(|k| CamPose { position: [-200.0 + 8.0 * k as f32, -100.0 - 2.0 * k as f32, -1400.0], target: [0.0, 0.0, 0.0], roll: 0.5 * k as f32 }).collect();
        let mut points: Vec<ScenePoint> = [[-200.0, 300.0, 0.0], [200.0, 300.0, 0.0], [-200.0, 300.0, 400.0], [200.0, 300.0, 400.0], [0.0, 300.0, 200.0]]
            .iter()
            .map(|&p| ScenePoint { pos: p, error: 0.2, first: 0, last: (n - 1) as u32 })
            .collect();
        points.push(ScenePoint { pos: [50.0, -200.0, 600.0], error: 0.3, first: 10, last: 40 });
        let ct = CameraTrack { times: times.clone(), cameras: cameras.clone(), fov: 40.0, frame: [1920.0, 1080.0], points, error: 0.25, tripod: false, placed: n as u32, camera: None, source: None };
        e.project.sequences[0].find_clip_mut(id).unwrap().1.camera_track = Some(Box::new(ct.clone()));
        let floor = [0usize, 1, 2, 3, 4];
        // A solid on the floor, and with it the camera.
        e.create_from_camera_track(id, CamMake::Solid, &floor);
        let solid = e.selection.unwrap();
        let cams = |e: &Editor| e.seq().tracks.iter().flat_map(|t| &t.clips).filter(|c| c.scene == Some(reelforge_core::space3d::SceneObject::Camera)).count();
        assert_eq!(cams(&e), 1);
        let cam_id = e.seq().find_clip(id).unwrap().1.camera_track.as_ref().unwrap().camera.unwrap();
        assert!(e.seq().find_clip(cam_id).is_some());
        // The sequence's camera is the solved one, frame by frame.
        for k in [0usize, 13, 37, 49] {
            let t = times[k];
            let sc = e.project.scene3d_at(0, t);
            let want = ct.camera(k);
            for j in 0..3 {
                assert!((sc.camera.position[j] - want.position[j]).abs() < 1e-2 && (sc.camera.target[j] - want.target[j]).abs() < 1e-2, "{k}: {:?} {:?}", sc.camera, want);
            }
            assert!((sc.camera.roll - want.roll).abs() < 1e-3 && (sc.camera.fov - 40.0).abs() < 1e-4);
            // The solid's middle stays on the floor points' middle, seen from that camera.
            let c = e.seq().find_clip(solid).unwrap().1;
            let tr = e.project.world_transform(0, c, t);
            assert!(tr.three_d);
            let ls = reelforge_core::space3d::layer_space(&tr, e.layer_size(c), &sc);
            let a = sc.camera.project(ls.plane[0]).unwrap();
            let b = ct.project(k, [0.0, 300.0, 200.0]).unwrap();
            assert!((a[0] - b[0]).abs() < 0.05 && (a[1] - b[1]).abs() < 0.05, "{a:?} {b:?}");
            // Lying flat: its face points up.
            let [_, u, v] = ls.plane;
            let nrm = [v[1] * u[2] - v[2] * u[1], v[2] * u[0] - v[0] * u[2], v[0] * u[1] - v[1] * u[0]];
            let l = (nrm[0] * nrm[0] + nrm[1] * nrm[1] + nrm[2] * nrm[2]).sqrt();
            assert!(nrm[1] / l < -0.999, "{nrm:?}");
        }
        // More layers use the same camera.
        e.create_from_camera_track(id, CamMake::Text3d, &[5]);
        e.create_from_camera_track(id, CamMake::Point, &[5]);
        assert_eq!(cams(&e), 1);
        let p = e.selection.unwrap();
        let pc = e.seq().find_clip(p).unwrap().1;
        assert_eq!(pc.scene, Some(reelforge_core::space3d::SceneObject::Point));
        assert_eq!(pc.effects[0].params[reelforge_core::effects::point::Z].value, 600.0);
        // Undo takes the point away; undoing the first create removes the camera and forgets it.
        e.undo();
        e.undo();
        e.undo();
        assert_eq!(cams(&e), 0);
        assert!(e.seq().find_clip(id).unwrap().1.camera_track.as_ref().unwrap().camera.is_none());
        // Ground plane from the floor: the floor is level at the origin.
        e.set_camera_ground(id, &floor);
        let g = e.seq().find_clip(id).unwrap().1.camera_track.clone().unwrap();
        let (c, nrm) = g.plane(&floor, 0).unwrap();
        assert!(nrm[1] < -0.999 && c.iter().all(|v| v.abs() < 1e-2), "{c:?} {nrm:?}");
        // Too few points: nothing happens.
        e.set_camera_ground(id, &[0, 1]);
        assert_eq!(e.seq().find_clip(id).unwrap().1.camera_track.as_deref(), Some(&*g));
        e.remove_camera_track(id);
        assert!(e.seq().find_clip(id).unwrap().1.camera_track.is_none());
    }

    #[test]
    fn camera_solves_export_and_come_back() {
        use reelforge_core::camtrack::{CamPose, CameraTrack, ScenePoint};
        use reelforge_core::camtrack_io::CamFormat;
        let mut e = editor();
        e.new_project_with(1920, 1080, reelforge_core::FrameRate::new(25, 1));
        e.add_plane([40, 40, 40, 255]);
        let id = e.selection.unwrap();
        let n = 40;
        let times: Vec<Time> = (0..n).map(|k| Time::from_secs_f64(k as f64 / 25.0)).collect();
        let cameras: Vec<CamPose> = (0..n).map(|k| CamPose { position: [-200.0 + 9.0 * k as f32, -120.0, -1300.0 + 5.0 * k as f32], target: [30.0, 20.0, 400.0], roll: 3.0 - 0.2 * k as f32 }).collect();
        let points = vec![ScenePoint { pos: [-150.0, 250.0, 300.0], error: 0.2, first: 0, last: 39 }, ScenePoint { pos: [200.0, -80.0, 700.0], error: 0.2, first: 0, last: 39 }];
        let ct = CameraTrack { times: times.clone(), cameras, fov: 38.0, frame: [1920.0, 1080.0], points, error: 0.2, tripod: false, placed: n as u32, camera: None, source: None };
        // Nothing to export yet.
        let dir = std::env::temp_dir().join(format!("rf_camio_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        e.export_camera_track(id, CamFormat::Chan, &dir.join("none.chan"), &[]);
        assert!(!dir.join("none.chan").exists() && e.status.contains("no solved camera"));
        e.project.sequences[0].find_clip_mut(id).unwrap().1.camera_track = Some(Box::new(ct.clone()));
        for (fmt, file) in [(CamFormat::AfterEffects, "shot.jsx"), (CamFormat::Blender, "shot.py"), (CamFormat::Nuke, "shot.nk"), (CamFormat::Chan, "shot.chan"), (CamFormat::Json, "shot.json")] {
            e.export_camera_track(id, fmt, &dir.join(file), &[]);
            assert!(dir.join(file).exists(), "{file}: {}", e.status);
            assert!(e.status.starts_with("Exported"), "{}", e.status);
        }
        assert!(std::fs::read_to_string(dir.join("shot.obj")).unwrap().lines().filter(|l| l.starts_with("v ")).count() == 2, "Nuke's points beside it");
        // A .chan comes back as a camera clip from the selected clip's start, matching frame by frame.
        e.import_camera_file(&dir.join("shot.chan"));
        let cam = e.selection.unwrap();
        let cc = e.seq().find_clip(cam).unwrap().1.clone();
        assert_eq!(cc.scene, Some(reelforge_core::space3d::SceneObject::Camera));
        assert_eq!(cc.timeline_start, Time::ZERO);
        assert_eq!(cc.duration, Time::from_secs_f64(40.0 / 25.0).snap(reelforge_core::FrameRate::new(25, 1)));
        for k in [0usize, 11, 39] {
            let sc = e.project.scene3d_at(0, times[k]);
            let want = ct.camera(k);
            assert!((sc.camera.fov - 38.0).abs() < 1e-3);
            for p in &ct.points {
                let (a, b) = (sc.camera.project(p.pos).unwrap(), want.project(p.pos).unwrap());
                assert!((a[0] - b[0]).abs() < 0.05 && (a[1] - b[1]).abs() < 0.05, "{k}: {a:?} {b:?}");
            }
        }
        // The whole solve goes onto the selected footage clip (not onto a camera).
        e.project.sequences[0].find_clip_mut(id).unwrap().1.camera_track = None;
        e.import_camera_file(&dir.join("shot.json"));
        assert!(e.status.contains("Select the footage clip"), "{}", e.status);
        // A broken file is refused with a reason.
        std::fs::write(dir.join("bad.chan"), "1 2 3\n").unwrap();
        e.import_camera_file(&dir.join("bad.chan"));
        assert!(e.status.starts_with("Couldn't import bad.chan"), "{}", e.status);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stabilising_with_the_solved_camera() {
        use reelforge_core::camtrack::{CamPose, CameraTrack, ScenePoint, StabPath};
        use reelforge_core::track::StabZoom;
        let mut e = editor();
        e.new_project_with(1920, 1080, reelforge_core::FrameRate::new(25, 1));
        e.add_plane([40, 40, 40, 255]);
        let id = e.selection.unwrap();
        let n = 50;
        // A slow pan with shake in the aim and roll.
        let wob = |k: usize, a: f32| (k as f32 * 2.3 + a).sin() * 0.5 + (k as f32 * 5.1 + a).cos() * 0.5;
        let times: Vec<Time> = (0..n).map(|k| Time::from_secs_f64(k as f64 / 25.0)).collect();
        let cameras: Vec<CamPose> = (0..n).map(|k| CamPose { position: [3.0 * k as f32, -60.0, -1300.0], target: [4.0 * k as f32 + 12.0 * wob(k, 0.0), 10.0 * wob(k, 1.0), 400.0], roll: 0.8 * wob(k, 2.0) }).collect();
        let points: Vec<ScenePoint> = (0..20).map(|k| ScenePoint { pos: [-600.0 + 60.0 * k as f32, -200.0 + 20.0 * (k % 7) as f32, 400.0], error: 0.2, first: 0, last: (n - 1) as u32 }).collect();
        let ct = CameraTrack { times: times.clone(), cameras, fov: 40.0, frame: [1920.0, 1080.0], points, error: 0.2, tripod: false, placed: n as u32, camera: None, source: None };
        e.project.sequences[0].find_clip_mut(id).unwrap().1.camera_track = Some(Box::new(ct.clone()));
        let before = e.seq().find_clip(id).unwrap().1.anim.clone();
        e.stabilise_with_camera(id, StabPath::Smooth(10.0), StabZoom::Fixed);
        let c = e.seq().find_clip(id).unwrap().1.clone();
        assert_eq!(c.anim.pin.0.len(), n, "{}", e.status);
        let layer = e.layer_size(&c);
        // Fixed zoom: the picture covers the frame every frame.
        for &t in &times {
            let q = c.transform_at(t).corners(layer, [1920.0, 1080.0]);
            assert!(reelforge_core::track::cover_zoom(&q, [1920.0, 1080.0]) <= 1.001, "at {t:?}");
        }
        // A point seen through the stabilised picture moves smoothly.
        let filmed = |k: usize| ct.project(k, ct.points[5].pos).unwrap();
        let shown = |c: &Clip, k: usize| {
            let tr = c.transform_at(times[k]);
            tr.layer_map(layer, [1920.0, 1080.0]).apply([filmed(k)[0] - 960.0, filmed(k)[1] - 540.0])
        };
        let jerk = |f: &dyn Fn(usize) -> [f32; 2]| (1..n - 1).map(|k| { let (a, b, c) = (f(k - 1), f(k), f(k + 1)); ((a[0] - 2.0 * b[0] + c[0]).powi(2) + (a[1] - 2.0 * b[1] + c[1]).powi(2)).sqrt() }).sum::<f32>() / (n - 2) as f32;
        let (raw, steady) = (jerk(&filmed), jerk(&|k| shown(&c, k)));
        assert!(raw > 3.0 && steady < raw / 8.0, "shake {raw} → {steady}");
        // Undo puts it back; Lock holds the point still.
        e.undo();
        assert_eq!(e.seq().find_clip(id).unwrap().1.anim, before);
        e.stabilise_with_camera(id, StabPath::Lock(0), StabZoom::Off);
        let c = e.seq().find_clip(id).unwrap().1.clone();
        let p0 = shown(&c, 0);
        let wander = (0..n).map(|k| { let p = shown(&c, k); ((p[0] - p0[0]).powi(2) + (p[1] - p0[1]).powi(2)).sqrt() }).fold(0.0f32, f32::max);
        assert!(wander < 1.0, "locked point wanders {wander} px");
        // A clip without a solve.
        e.project.sequences[0].find_clip_mut(id).unwrap().1.camera_track = None;
        e.stabilise_with_camera(id, StabPath::Lock(0), StabZoom::Off);
        assert!(e.status.contains("Analyse"));
    }

    #[test]
    fn effect_favourites_are_kept_with_the_settings() {
        // Settings saved before favourites existed still load, with none.
        let old: Prefs = serde_json::from_str(r#"{"use_proxies": false, "start_screen": true}"#).unwrap();
        assert!(old.fx_favourites.is_empty() && !old.use_proxies);
        let mut e = editor();
        let mut p = e.prefs.clone();
        p.fx_favourites = vec!["Halftone".into(), "plugin:com.example.swirl".into()];
        e.set_prefs(p.clone());
        assert_eq!(e.prefs.fx_favourites, p.fx_favourites);
        let back: Prefs = serde_json::from_str(&serde_json::to_string(&e.prefs).unwrap()).unwrap();
        assert_eq!(back.fx_favourites, p.fx_favourites);
    }

    #[test]
    fn new_from_a_preset_recent_list_and_templates() {
        let dir = std::env::temp_dir().join(format!("rf_start_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut e = editor();
        e.recent = crate::start::Recent::load(dir.join("recent.json"));
        // A vertical project from its preset.
        let p = crate::start::preset("vertical-30").unwrap();
        e.new_project_with(p.width, p.height, p.frame_rate());
        assert_eq!((e.seq().width, e.seq().height, e.seq().frame_rate.fps()), (1080, 1920, 30.0));
        assert!(e.project_path.is_none() && !e.is_dirty());
        // Saving puts it on the recent list (newest first), with its size.
        e.add_title();
        let a = dir.join("a.rfproj");
        e.save_to(&a).unwrap();
        assert_eq!(e.recent.list.len(), 1);
        assert_eq!(e.recent.list[0].summary, "1080 × 1920 · 30 fps");
        // And as a template; a project started from it is the same, but new and unsaved-to.
        let tpl = e.save_as_template(&dir.join("templates"), "Vertical: my look").unwrap();
        assert_eq!(tpl.file_name().unwrap(), "Vertical my look.rfproj");
        let templates = crate::start::user_templates(&dir.join("templates"));
        assert_eq!((templates.len(), templates[0].name.as_str(), templates[0].summary.as_str()), (1, "Vertical my look", "1080 × 1920 · 30 fps"));
        e.new_project_with(3840, 2160, reelforge_core::FrameRate::new(24, 1));
        assert_eq!(e.seq().width, 3840);
        e.new_from_template(&tpl);
        assert_eq!((e.seq().width, e.seq().height), (1080, 1920));
        let clips: usize = e.seq().tracks.iter().map(|t| t.clips.len()).sum();
        assert_eq!(clips, 1, "the title came along");
        assert!(e.project_path.is_none(), "Save asks where: the template isn't written over");
        assert!(e.recent.list.iter().all(|r| !r.path.starts_with(dir.join("templates"))), "templates aren't recent projects");
        // Opening puts it back on top.
        let b = dir.join("b.rfproj");
        e.save_to(&b).unwrap();
        e.open_project_path(a.clone());
        assert_eq!(e.recent.list[0].path, std::fs::canonicalize(&a).unwrap());
        assert_eq!(e.recent.list.len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A small real video file made with the ffmpeg CLI (tests skip without it).
    fn clip_file(dir: &Path, name: &str) -> Option<PathBuf> {
        let p = dir.join(name);
        let ok = std::process::Command::new("ffmpeg")
            .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i", "testsrc2=size=160x90:rate=25:duration=1", "-pix_fmt", "yuv420p"])
            .arg(&p)
            .status()
            .is_ok_and(|s| s.success());
        ok.then_some(p)
    }

    #[test]
    fn track_heights_and_collapsing_are_kept_with_the_project() {
        let mut e = editor();
        e.set_track_height(0, 120);
        e.set_track_height(1, 5);
        e.toggle_collapse(2);
        let t = &e.seq().tracks;
        assert_eq!((t[0].height, t[1].height, t[2].collapsed), (120, Track::MIN_HEIGHT, true));
        // Saved and opened again.
        let json = serde_json::to_string(&e.project).unwrap();
        let back: Project = serde_json::from_str(&json).unwrap();
        assert_eq!((back.sequences[0].tracks[0].height, back.sequences[0].tracks[2].collapsed), (120, true));
        // A project from before tracks had heights: the default.
        let old = json.replace("\"height\":120,", "").replace(",\"collapsed\":true", "").replace(",\"collapsed\":false", "");
        assert!(!old.contains("collapsed"));
        let back: Project = serde_json::from_str(&old).unwrap();
        assert_eq!((back.sequences[0].tracks[0].height, back.sequences[0].tracks[2].collapsed), (Track::DEFAULT_HEIGHT, false));
        // Setting a height expands a collapsed track; all of a kind at once.
        e.set_track_height(2, 80);
        assert!(!e.seq().tracks[2].collapsed);
        let video = e.seq().tracks[0].kind;
        e.set_all_tracks(Some(video), true, false);
        assert!(e.seq().tracks.iter().all(|t| t.collapsed == (t.kind == video)));
        e.set_all_tracks(None, false, true);
        assert!(e.seq().tracks.iter().all(|t| !t.collapsed && t.height == Track::DEFAULT_HEIGHT));
        // Not edits: nothing to undo.
        e.undo();
        assert_eq!(e.status, "Nothing to undo");
    }

    #[test]
    fn importing_a_frame_of_an_exr_sequence_brings_in_the_whole_sequence() {
        let dir = std::env::temp_dir().join(format!("rf_import_seq_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let ok = std::process::Command::new("ffmpeg")
            .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i", "testsrc2=size=160x90:rate=24:duration=0.5", "-vf", "format=gbrapf32le", "-c:v", "exr", "-compression", "zip16", "-start_number", "1001"])
            .arg(dir.join("plate_v3.%04d.exr"))
            .status()
            .is_ok_and(|s| s.success());
        if !ok {
            return;
        }
        let mut e = editor();
        e.project.sequences[0].frame_rate = reelforge_core::FrameRate::new(24, 1);
        // Two frames of the same sequence picked: one item.
        e.import(&[dir.join("plate_v3.1004.exr"), dir.join("plate_v3.1001.exr")]);
        assert_eq!(e.project.media.len(), 1, "{}", e.status);
        let m = e.project.media[0].clone();
        assert_eq!(m.name, "plate_v3.[1001-1012].exr");
        assert!(reelforge_core::imageseq::is_sequence(&m.path));
        assert!((m.info.duration.as_secs_f64() - 0.5).abs() < 0.01, "{:?}", m.info.duration);
        assert!(reelforge_core::undo::missing_media(&e.project).is_empty());
        e.add_to_timeline(m.id, None, None);
        let clip = &e.seq().tracks[0].clips[0];
        assert_eq!(clip.duration.to_frame(reelforge_core::FrameRate::new(24, 1)), 12);
        // Moved away: missing, then found again by name in the new folder.
        let moved = dir.with_file_name(format!("rf_import_seq_moved_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&moved);
        std::fs::rename(&dir, &moved).unwrap();
        assert_eq!(reelforge_core::undo::missing_media(&e.project), vec![m.id]);
        e.find_missing_in(&moved);
        assert!(reelforge_core::undo::missing_media(&e.project).is_empty(), "{}", e.status);
        assert!(e.project.media[0].path.starts_with(&moved));
        std::fs::remove_dir_all(&moved).ok();
    }

    #[test]
    fn the_new_menu_adds_planes_points_cameras_lights_and_adjustment_layers() {
        use reelforge_core::effects::point as pt;
        use reelforge_core::space3d::SceneObject;
        let mut e = editor();
        let tracks = e.seq().tracks.len();
        e.add_plane([10, 20, 30, 255]);
        let plane = e.selection.unwrap();
        e.add_adjustment_layer();
        let adj = e.selection.unwrap();
        e.add_scene_object(SceneObject::Point, 0);
        let point = e.selection.unwrap();
        e.add_scene_object(SceneObject::Camera, 0);
        e.add_scene_object(SceneObject::Light, 1);
        assert_eq!(e.seq().tracks.len(), tracks + 5, "each on its own new track");
        assert!(e.seq().find_clip(adj).unwrap().1.adjustment);
        // Parenting keeps the plane where it shows, then it follows the point.
        e.set_transform_live(plane, Transform { position: [100.0, 0.0], ..Default::default() });
        e.commit_live_edit();
        let mut moved = e.seq().find_clip(point).unwrap().1.effects[0].clone();
        moved.params[pt::X].value = 50.0;
        moved.params[pt::ROTATION].value = 90.0;
        let pi = e.seq().find_clip(point).unwrap().0;
        e.project.sequences[e.active].tracks[pi].clips[0].effects[0] = moved;
        assert_eq!(e.parent_choices(plane).len(), 1);
        e.set_parent(plane, Some(point));
        let c = e.seq().find_clip(plane).unwrap().1.clone();
        let world = e.project.world_transform(e.active, &c, e.playback.playhead);
        assert!((world.position[0] - 100.0).abs() < 1e-3 && world.position[1].abs() < 1e-3 && world.rotation.abs() < 1e-3, "{world:?}");
        // A point can't follow a layer that follows it.
        e.set_parent(point, Some(point));
        assert_eq!(e.seq().find_clip(point).unwrap().1.parent, None);
        e.undo();
        assert_eq!(e.seq().find_clip(plane).unwrap().1.parent, None);
        e.set_plane_color(plane, [1, 2, 3, 255]);
        assert_eq!(e.seq().find_clip(plane).unwrap().1.plane, Some([1, 2, 3, 255]));
    }

    #[test]
    fn relinking_points_media_at_the_moved_files_and_undoes() {
        let dir = std::env::temp_dir().join(format!("rf-engine-relink-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("new/sub")).unwrap();
        let (Some(a), Some(b)) = (clip_file(&dir.join("new"), "a.mp4"), clip_file(&dir.join("new/sub"), "b.mp4")) else { return };
        let mut e = editor();
        let info = reelforge_media::probe(&a).unwrap();
        for (id, name) in [(1, "a.mp4"), (2, "b.mp4")] {
            e.project.media.push(MediaAsset { id: MediaId(id), path: dir.join("old").join(name), name: name.into(), info: info.clone(), conform: None });
        }
        assert_eq!(reelforge_core::undo::missing_media(&e.project).len(), 2);
        // Relinking one file finds the other in the same folder tree by name.
        let mut pairs = vec![(dir.join("old/a.mp4"), a.clone())];
        pairs.extend(crate::safety::find_by_name(&dir.join("new"), &[dir.join("old/b.mp4")]));
        e.apply_relinks(pairs);
        assert!(reelforge_core::undo::missing_media(&e.project).is_empty(), "{}", e.status);
        assert_eq!(e.project.media(MediaId(2)).unwrap().path, b);
        assert!(e.status.starts_with("Relinked 2 files"), "{}", e.status);
        e.undo();
        assert_eq!(reelforge_core::undo::missing_media(&e.project).len(), 2, "one undo step");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn energy_blades_follow_a_track_on_their_own_layer_or_the_clip() {
        use reelforge_core::effects::blade;
        use reelforge_core::track::TrackSample;
        let dir = std::env::temp_dir().join(format!("rf-engine-blade-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let Some(f) = clip_file(&dir, "a.mp4") else { return };
        let mut e = editor();
        let info = reelforge_media::probe(&f).unwrap();
        e.project.media.push(MediaAsset { id: MediaId(1), path: f, name: "a.mp4".into(), info, conform: None });
        (e.project.sequences[0].width, e.project.sequences[0].height) = (160, 90);
        let id = ClipId(e.project.alloc_id());
        let mut c = Clip::new(id, MediaId(1), Time::ZERO, Time::ZERO, Time::from_secs_f64(1.0), None);
        // A prop moving 16 px (10% of the width) to the right over half a second.
        let mut t = Tracker::new(TrackMode::Position, [80.0, 45.0], [160.0, 90.0], Time::ZERO);
        t.points[0].path = vec![
            TrackSample { at: Time::ZERO, pos: [80.0, 45.0], score: 1.0 },
            TrackSample { at: Time::from_secs_f64(0.5), pos: [96.0, 45.0], score: 1.0 },
        ];
        c.tracker = Some(t);
        c.effects.push(Effect::new(EffectKind::EnergyBlade));
        let v = e.seq().tracks.iter().position(|tr| tr.kind == TrackKind::Video).unwrap();
        e.project.sequences[0].tracks[v].clips.push(c);
        e.select_only(Some(id));

        // On its own layer above, as long as the clip.
        e.add_blade_layer();
        let layer = e.selection.unwrap();
        let (_, b) = e.seq().find_clip(layer).unwrap();
        assert_eq!((b.timeline_start, b.duration), (Time::ZERO, Time::from_secs_f64(1.0)));
        assert_eq!(b.display_name(&e.project), "Energy blade");
        e.apply_track_to_blade(id, layer, 0, Axes::ALL);
        let (_, b) = e.seq().find_clip(layer).unwrap();
        let x = |c: &Clip, at: f64| c.effects[0].value_at(blade::HILT_X, Time::from_secs_f64(at));
        assert!((x(b, 0.0) - 40.0).abs() < 0.01 && (x(b, 0.5) - 50.0).abs() < 0.01, "{} {} ({})", x(b, 0.0), x(b, 0.5), e.status);
        assert!((b.effects[0].value_at(blade::TIP_X, Time::from_secs_f64(0.5)) - 72.0).abs() < 0.01);
        assert!(b.effects[0].params[blade::HILT_Y].keys.is_animated(), "Y keyed too");

        // On the tracked clip itself, X only.
        e.apply_track_to_blade(id, id, 0, Axes { x: true, y: false, rotation: false, scale: false });
        let (_, c) = e.seq().find_clip(id).unwrap();
        assert!((x(c, 0.5) - 50.0).abs() < 0.01, "{}", e.status);
        assert!(!c.effects[0].params[blade::HILT_Y].keys.is_animated(), "Y left alone");
        e.undo();
        assert!(!e.seq().find_clip(id).unwrap().1.effects[0].params[blade::HILT_X].keys.is_animated(), "one undo step");

        // A patch replacer's target follows too; its source offset is left alone.
        {
            use reelforge_core::effects::patch as pr;
            let mut v = e.visual(id).unwrap();
            v.effects.push(Effect::new(EffectKind::PatchReplacer));
            e.set_visual_live(id, v);
            e.commit_live_edit();
            e.apply_track_to_blade(id, id, 1, Axes::ALL);
            let (_, c) = e.seq().find_clip(id).unwrap();
            let fx = &c.effects[1];
            let at = |i, s: f64| fx.value_at(i, Time::from_secs_f64(s));
            assert!((at(pr::X, 0.0) - 50.0).abs() < 0.01 && (at(pr::X, 0.5) - 60.0).abs() < 0.01, "{} {} ({})", at(pr::X, 0.0), at(pr::X, 0.5), e.status);
            assert!(!fx.params[pr::SOURCE_X].keys.is_animated());
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn clip_colours_apply_to_the_selection_and_undo() {
        let mut e = editor();
        e.add_plane([255, 0, 0, 255]);
        let a = e.selection.unwrap();
        e.add_title();
        let b = e.selection.unwrap();
        e.select_many(vec![a, b]);
        e.set_clip_label(Some([40, 200, 90]));
        let label = |e: &Editor, id| e.seq().find_clip(id).unwrap().1.label;
        assert_eq!((label(&e, a), label(&e, b)), (Some([40, 200, 90]), Some([40, 200, 90])));
        // Saved with the project.
        let json = serde_json::to_string(&e.project).unwrap();
        assert!(json.contains("\"label\":[40,200,90]"));
        e.undo();
        assert_eq!((label(&e, a), label(&e, b)), (None, None));
        e.redo();
        e.select_only(Some(b));
        e.set_clip_label(None);
        assert_eq!((label(&e, a), label(&e, b)), (Some([40, 200, 90]), None));
    }

    #[test]
    fn adding_3d_text_makes_a_turned_extruded_3d_title() {
        let mut e = editor();
        e.add_3d_text();
        let id = e.selection.unwrap();
        let (_, c) = e.seq().find_clip(id).unwrap();
        let spec = c.title.as_ref().unwrap();
        assert!(spec.is_3d() && spec.extrude > 10.0 && !spec.shadow);
        assert!(c.transform.three_d && c.transform.orientation != [0.0, 0.0]);
        // One undo step takes it away again.
        e.undo();
        assert!(e.seq().find_clip(id).is_none());
    }

/// 3D shapes: an SVG logo becomes an extruded, bevelled shape title (one
    /// undo step), a bad file says why, and a mask's outline becomes a shape
    /// the mask's height, where the mask is, facing the camera.
    #[test]
    fn logos_and_masks_become_3d_shapes() {
        let mut e = editor();
        let dir = std::env::temp_dir().join(format!("rf_logo_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let svg = dir.join("Acme.svg");
        std::fs::write(&svg, r#"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="50"><path d="M0 0 H100 V50 H0 Z"/></svg>"#).unwrap();
        e.import_svg_shape(&svg);
        let id = e.selection.unwrap();
        let (_, c) = e.seq().find_clip(id).unwrap();
        let spec = c.title.as_ref().unwrap();
        let shape = spec.shape.as_ref().unwrap();
        assert_eq!((shape.name.as_str(), spec.label().as_str()), ("Acme", "Acme"));
        assert!((shape.aspect - 2.0).abs() < 1e-4);
        assert!(spec.is_3d() && spec.bevel > 0.0 && c.transform.three_d);
        assert!(e.status.contains("3D shape"), "{}", e.status);
        e.undo();
        assert!(e.seq().find_clip(id).is_none());
        std::fs::write(&svg, "not an svg").unwrap();
        e.import_svg_shape(&svg);
        assert!(e.status.starts_with("Couldn't use"), "{}", e.status);
        let _ = std::fs::remove_dir_all(&dir);

        // From a mask: a rectangle 40 px high, right of the middle of a full-frame plane.
        e.add_plane([20, 20, 20, 255]);
        let plane = e.selection.unwrap();
        let layer = e.layer_size(e.seq().find_clip(plane).unwrap().1);
        let (cx, cy) = (layer[0] / 2.0, layer[1] / 2.0);
        let rect = Mask::freehand(vec![[cx + 100.0, cy - 20.0], [cx + 180.0, cy - 20.0], [cx + 180.0, cy + 20.0], [cx + 100.0, cy + 20.0]]);
        let i = e.add_mask(plane, rect).unwrap();
        e.shape_from_mask(plane, i);
        let id = e.selection.unwrap();
        assert_ne!(id, plane);
        let (_, c) = e.seq().find_clip(id).unwrap();
        let spec = c.title.as_ref().unwrap();
        assert_eq!(spec.size, 40.0);
        assert!((spec.shape.as_ref().unwrap().aspect - 2.0).abs() < 1e-4);
        assert_eq!(c.transform.position, [140.0, 0.0]);
        assert_eq!(c.transform.orientation, [0.0, 0.0]);
        assert!(c.transform.three_d);
    }

/// Recently used effects: newest first, each once, at most eight; kept
    /// with the settings.
    #[test]
    fn recent_effects_are_remembered() {
        let mut e = editor();
        for k in ["Glow", "Blur", "plugin:com.example.swirl", "Glow"] {
            e.note_recent_effect(k);
        }
        assert_eq!(e.prefs.fx_recent, ["Glow", "plugin:com.example.swirl", "Blur"]);
        for k in 0..10 {
            e.note_recent_effect(&format!("Fx{k}"));
        }
        assert_eq!(e.prefs.fx_recent.len(), RECENT_FX);
        assert_eq!(e.prefs.fx_recent[0], "Fx9");
        let json = serde_json::to_string(&e.prefs).unwrap();
        let back: Prefs = serde_json::from_str(&json).unwrap();
        assert_eq!(back.fx_recent, e.prefs.fx_recent);
        // Older settings without it read fine.
        let old: Prefs = serde_json::from_str("{}").unwrap();
        assert!(old.fx_recent.is_empty());
    }

/// 3D models: importing a model file makes a turned 3D model layer (one
    /// undo step) with the file's up axis, alongside ordinary media; a bad
    /// file says why and adds nothing.
    #[test]
    fn model_files_become_3d_model_layers() {
        let mut e = editor();
        let dir = std::env::temp_dir().join(format!("rf_model_import_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let stl = dir.join("Part.stl");
        std::fs::write(&stl, "solid p\nfacet normal 0 0 1\nouter loop\nvertex 0 0 0\nvertex 1 0 0\nvertex 0 1 0\nendloop\nendfacet\nendsolid p\n").unwrap();
        let clips = |e: &Editor| e.seq().tracks.iter().map(|t| t.clips.len()).sum::<usize>();
        let before = clips(&e);
        e.import(&[stl.clone()]);
        let id = e.selection.unwrap();
        let (_, c) = e.seq().find_clip(id).unwrap();
        let spec = c.title.as_ref().unwrap();
        let m = spec.model.as_ref().unwrap();
        assert!(m.z_up && m.own_colours && m.path.is_absolute(), "{m:?}");
        assert_eq!(spec.label(), "Part.stl");
        assert!(spec.is_3d() && c.transform.three_d && c.transform.orientation != [0.0, 0.0]);
        assert!(e.status.contains("1 triangles") || e.status.contains("triangles"), "{}", e.status);
        assert_eq!(clips(&e), before + 1);
        assert!(e.project.media.is_empty(), "not put in the media bin");
        e.undo();
        assert_eq!(clips(&e), before);
        std::fs::write(&stl, "nonsense").unwrap();
        e.add_3d_model(&stl);
        assert!(e.status.starts_with("Couldn't use Part.stl"), "{}", e.status);
        assert_eq!(clips(&e), before);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_highlighted_track_takes_new_layers_and_pastes() {
        let mut e = editor(); // V1 V2 A1
        let names = |e: &Editor| e.seq().tracks.iter().map(|t| t.name.clone()).collect::<Vec<_>>();
        // Nothing highlighted: a new plane goes on a new top track.
        e.add_plane([255, 0, 0, 255]);
        assert_eq!(names(&e), ["Video 1", "Video 2", "Video 3", "Audio 1"]);
        let first = e.selection.unwrap();
        // Highlight V1 (empty): the next plane goes on V1 itself, and V1 stays highlighted.
        e.set_target_track(Some(0));
        e.add_plane([0, 255, 0, 255]);
        assert_eq!(e.seq().find_clip(e.selection.unwrap()).unwrap().0, 0);
        assert_eq!(e.target_track(), Some(0));
        // V1 is now taken there: the next one goes on a new track just above it, which becomes the highlight.
        e.add_adjustment_layer();
        assert_eq!(names(&e), ["Video 1", "Video 4", "Video 2", "Video 3", "Audio 1"]);
        assert_eq!(e.seq().find_clip(e.selection.unwrap()).unwrap().0, 1);
        assert_eq!(e.target_track(), Some(1));
        // A title with V4 highlighted (taken): above it again.
        e.add_title();
        assert_eq!(e.seq().find_clip(e.selection.unwrap()).unwrap().0, 2);
        assert_eq!(e.seq().tracks[2].kind, TrackKind::Video);

        // Paste: the copied plane (on "Video 3", the top) lands on the highlighted track.
        e.select_only(Some(first));
        assert_eq!(e.copy_selection(), 1);
        e.set_target_track(Some(0));
        let at = e.playback.playhead;
        e.playback.seek(at + Time::from_secs_f64(60.0));
        e.paste();
        assert_eq!(e.seq().find_clip(e.selection.unwrap()).unwrap().0, 0, "pasted onto the highlighted track");

        // The highlight follows its track by name when tracks move, and goes when it's deleted.
        let v1 = e.seq().tracks.iter().position(|t| t.name == "Video 1").unwrap();
        e.set_target_track(Some(v1));
        e.move_track(v1, 3);
        assert_eq!(e.seq().tracks[e.target_track().unwrap()].name, "Video 1");
        e.undo();
        assert_eq!(e.seq().tracks[e.target_track().unwrap()].name, "Video 1");
        let a1 = e.seq().tracks.iter().position(|t| t.name == "Audio 1").unwrap();
        e.add_track(TrackKind::Audio);
        e.set_target_track(Some(a1));
        e.delete_track(a1);
        assert_eq!(e.target_track(), None);
        // Duplicating highlights the copy.
        e.duplicate_track(0);
        assert_eq!(e.seq().tracks[e.target_track().unwrap()].name, format!("{} copy", e.seq().tracks[0].name));
    }

    #[test]
    fn parenting_a_keyframed_layer_keeps_it_in_place() {
        use reelforge_core::anim::Key;
        use reelforge_core::effects::point as pt;
        use reelforge_core::space3d::SceneObject;
        let s = Time::from_secs_f64;
        let mut e = editor();
        e.add_plane([200, 0, 0, 255]);
        let plane = e.selection.unwrap();
        // The plane moves (keys at 0 s and 4 s), turns and grows.
        {
            let c = e.project.sequences[0].find_clip_mut(plane).unwrap().1;
            c.anim.position.0 = vec![Key::new(s(0.0), [-200.0, 50.0], Default::default()), Key::new(s(4.0), [300.0, -80.0], Default::default())];
            c.anim.rotation.0 = vec![Key::new(s(1.0), 10.0, Default::default()), Key::new(s(3.0), 60.0, Default::default())];
            c.anim.scale.0 = vec![Key::new(s(0.0), [1.0, 1.0], Default::default()), Key::new(s(4.0), [1.5, 0.5], Default::default())];
        }
        // A point, moved, turned 90° and doubled.
        e.add_scene_object(SceneObject::Point, 0);
        let point = e.selection.unwrap();
        {
            let fx = &mut e.project.sequences[0].find_clip_mut(point).unwrap().1.effects[0];
            fx.normalize();
            (fx.params[pt::X].value, fx.params[pt::Y].value, fx.params[pt::ROTATION].value, fx.params[pt::SCALE].value) = (120.0, -40.0, 90.0, 200.0);
        }
        let world = |e: &Editor, at: f64| {
            let c = e.seq().find_clip(plane).unwrap().1.clone();
            e.project.world_transform(e.active, &c, s(at))
        };
        let close = |a: Transform, b: Transform| {
            (a.position[0] - b.position[0]).abs() < 1e-2 && (a.position[1] - b.position[1]).abs() < 1e-2 && (a.rotation - b.rotation).abs() < 1e-3 && (a.scale[0] - b.scale[0]).abs() < 1e-4 && (a.scale[1] - b.scale[1]).abs() < 1e-4
        };
        let before: Vec<Transform> = [0.0, 1.0, 2.5, 4.0].iter().map(|&t| world(&e, t)).collect();
        e.playback.seek(s(2.0));
        e.set_parent(plane, Some(point));
        // The point doesn't move: the plane looks the same at every moment, animation and all.
        for (t, b) in [0.0, 1.0, 2.5, 4.0].iter().zip(&before) {
            assert!(close(world(&e, *t), *b), "at {t} s: {:?} vs {:?}", world(&e, *t), b);
        }
        assert_eq!(e.seq().find_clip(plane).unwrap().1.anim.position.0.len(), 2, "the same keys, converted");
        // Now the point moves: the plane goes with it.
        {
            let fx = &mut e.project.sequences[0].find_clip_mut(point).unwrap().1.effects[0];
            fx.params[pt::X].value += 50.0;
        }
        assert!((world(&e, 2.5).position[0] - before[2].position[0] - 50.0).abs() < 1e-2);
        {
            let fx = &mut e.project.sequences[0].find_clip_mut(point).unwrap().1.effects[0];
            fx.params[pt::X].value -= 50.0;
        }
        // Unparenting keeps it in place too; undo goes back step by step.
        e.set_parent(plane, None);
        for (t, b) in [0.0, 1.0, 2.5, 4.0].iter().zip(&before) {
            assert!(close(world(&e, *t), *b), "unparented, at {t} s");
        }
        e.undo();
        e.undo();
        let c = e.seq().find_clip(plane).unwrap().1;
        assert_eq!((c.parent, c.anim.position.0[1].value), (None, [300.0, -80.0]), "undo restores the keys");

        // A camera parented to the point stays put too, its place and aim.
        e.add_scene_object(SceneObject::Camera, 0);
        let cam = e.selection.unwrap();
        let place = |e: &Editor| e.project.scene3d_at(e.active, s(2.0)).camera;
        let c0 = place(&e);
        e.set_parent(cam, Some(point));
        let c1 = place(&e);
        for k in 0..3 {
            assert!((c0.position[k] - c1.position[k]).abs() < 1e-2 && (c0.target[k] - c1.target[k]).abs() < 1e-2, "{c0:?} vs {c1:?}");
        }
    }

    #[test]
    fn a_tracked_point_carries_the_layers_parented_to_it() {
        use reelforge_core::space3d::SceneObject;
        use reelforge_core::track::TrackSample;
        let dir = std::env::temp_dir().join(format!("rf-engine-trackpoint-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let Some(f) = clip_file(&dir, "a.mp4") else { return };
        let mut e = editor();
        let info = reelforge_media::probe(&f).unwrap();
        e.project.media.push(MediaAsset { id: MediaId(1), path: f, name: "a.mp4".into(), info, conform: None });
        (e.project.sequences[0].width, e.project.sequences[0].height) = (160, 90);
        let id = ClipId(e.project.alloc_id());
        let mut c = Clip::new(id, MediaId(1), Time::ZERO, Time::ZERO, Time::from_secs_f64(1.0), None);
        let mut t = Tracker::new(TrackMode::Position, [80.0, 45.0], [160.0, 90.0], Time::ZERO);
        t.points[0].path = vec![
            TrackSample { at: Time::ZERO, pos: [80.0, 45.0], score: 1.0 },
            TrackSample { at: Time::from_secs_f64(0.5), pos: [96.0, 45.0], score: 1.0 },
        ];
        c.tracker = Some(t);
        let v = e.seq().tracks.iter().position(|tr| tr.kind == TrackKind::Video).unwrap();
        e.project.sequences[0].tracks[v].clips.push(c);
        e.add_scene_object(SceneObject::Point, 0);
        let point = e.selection.unwrap();
        // Two planes, both following the point.
        let mut planes = Vec::new();
        for x in [-30.0, 30.0] {
            e.add_plane([200, 0, 0, 255]);
            let p = e.selection.unwrap();
            e.set_transform_live(p, Transform { position: [x, 5.0], ..Default::default() });
            e.commit_live_edit();
            e.set_parent(p, Some(point));
            planes.push(p);
        }
        e.apply_track_axes(id, point, 0.0, Axes::ALL);
        assert!(e.status.contains("2 layers parented"), "{}", e.status);
        let at = |e: &Editor, p: ClipId, s: f64| {
            let c = e.seq().find_clip(p).unwrap().1.clone();
            e.project.world_transform(e.active, &c, Time::from_secs_f64(s)).position
        };
        for (p, x) in planes.iter().zip([-30.0f32, 30.0]) {
            assert!((at(&e, *p, 0.0)[0] - x).abs() < 1e-3, "stays put at the start");
            let end = at(&e, *p, 0.5);
            assert!((end[0] - (x + 16.0)).abs() < 1e-3 && (end[1] - 5.0).abs() < 1e-3, "{end:?}");
        }
        e.undo();
        assert!((at(&e, planes[0], 0.5)[0] + 30.0).abs() < 1e-3, "one undo step");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn music_ducks_under_speech_and_comes_back_up() {
        let dir = std::env::temp_dir().join(format!("rf-engine-duck-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // "Speech": a tone for 2 s, 2 s of silence, then 2 s more.
        let speech = dir.join("speech.wav");
        let ok = std::process::Command::new("ffmpeg")
            .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i", "sine=frequency=300:duration=6", "-af", "volume=enable='between(t,2,4)':volume=0"])
            .arg(&speech)
            .status()
            .is_ok_and(|s| s.success());
        if !ok {
            return;
        }
        let mut e = editor();
        let info = reelforge_media::probe(&speech).unwrap();
        e.project.media.push(MediaAsset { id: MediaId(1), path: speech, name: "speech.wav".into(), info: info.clone(), conform: None });
        e.project.media.push(MediaAsset { id: MediaId(2), path: dir.join("music.wav"), name: "music.wav".into(), info, conform: None });
        let audio: Vec<usize> = e.seq().tracks.iter().enumerate().filter(|(_, t)| t.kind == TrackKind::Audio).map(|(i, _)| i).collect();
        let (dlg, mus) = if audio.len() >= 2 {
            (audio[0], audio[1])
        } else {
            e.add_track(TrackKind::Audio);
            let a: Vec<usize> = e.seq().tracks.iter().enumerate().filter(|(_, t)| t.kind == TrackKind::Audio).map(|(i, _)| i).collect();
            (a[0], a[1])
        };
        let secs = Time::from_secs_f64;
        let (d, m) = (ClipId(e.project.alloc_id()), ClipId(e.project.alloc_id()));
        e.project.sequences[0].tracks[dlg].clips.push(Clip::new(d, MediaId(1), Time::ZERO, Time::ZERO, secs(6.0), None));
        let mut music = Clip::new(m, MediaId(2), Time::ZERO, Time::ZERO, secs(8.0), None);
        music.audio.volume_db = -4.0;
        e.project.sequences[0].tracks[mus].clips.push(music);
        let st = reelforge_core::duck::DuckSettings::default();
        e.auto_duck(&[dlg], &[mus], st);
        assert!(e.status.starts_with("Ducked 1 music clip by 12 dB under 2 stretches"), "{}", e.status);
        let vol = |e: &Editor, t: f64| e.seq().find_clip(m).unwrap().1.audio.volume_db_at(secs(t));
        assert!((vol(&e, 1.0) + 16.0).abs() < 0.01, "{}", vol(&e, 1.0));
        assert!((vol(&e, 3.0) + 4.0).abs() < 0.01, "back up in the pause: {}", vol(&e, 3.0));
        assert!((vol(&e, 5.0) + 16.0).abs() < 0.01);
        assert!((vol(&e, 7.5) + 4.0).abs() < 0.01, "back up after the speech");
        // A deeper duck replaces the first one.
        e.auto_duck(&[dlg], &[mus], reelforge_core::duck::DuckSettings { amount_db: -20.0, ..st });
        assert!((vol(&e, 1.0) + 24.0).abs() < 0.01);
        e.remove_ducking(&[mus]);
        let c = e.seq().find_clip(m).unwrap().1.clone();
        assert!(c.audio.volume_keys.is_empty() && c.audio.volume_db == -4.0, "{:?}", c.audio);
        e.undo();
        assert!((vol(&e, 1.0) + 24.0).abs() < 0.01, "removing is one undo step");
        // The same track as both: refused.
        e.auto_duck(&[mus], &[mus], st);
        assert!(e.status.starts_with("Choose"), "{}", e.status);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn history_jumps_back_and_forward() {
        let mut e = editor();
        e.add_track(TrackKind::Video);
        e.add_title();
        e.add_marker();
        assert_eq!(e.undo.entries().len(), 3);
        let titled = e.selection.expect("the new title is selected");
        let rev = e.rev;
        e.history_go_to(1); // just after adding the track
        assert!(e.rev > rev, "jumping counts as a change");
        assert!(e.seq().markers.is_empty());
        assert!(e.seq().find_clip(titled).is_none());
        e.history_go_to(3);
        assert_eq!(e.seq().markers.len(), 1);
        assert!(e.seq().find_clip(titled).is_some());
        e.history_go_to(0);
        assert_eq!(e.seq().tracks.len(), 3);
        assert_eq!(e.undo.entries().len(), 3, "undone steps stay listed");
        e.redo();
        assert_eq!(e.seq().tracks.len(), 4);
    }

    #[test]
    fn edits_are_unsaved_until_saved() {
        let dir = std::env::temp_dir().join(format!("rf-engine-dirty-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut e = editor();
        assert!(!e.is_dirty(), "a new project has nothing to save");
        e.add_track(TrackKind::Video);
        assert!(e.is_dirty());
        e.save_to(&dir.join("p.rfproj")).unwrap();
        assert!(!e.is_dirty());
        let saved: Project = serde_json::from_str(&std::fs::read_to_string(dir.join("p.rfproj")).unwrap()).unwrap();
        assert_eq!(saved.sequences[0].tracks.len(), 4);
        // An autosave opened is a new, unsaved project (Save asks where).
        e.open_autosave(dir.join("p.rfproj"));
        assert_eq!(e.project_path, None);
        assert!(e.is_dirty());
        assert_eq!(e.seq().tracks.len(), 4);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn composites_paste_attributes_and_nudges() {
        let mut e = editor();
        e.add_title();
        let a = e.selection.unwrap();
        // Nudge: two frames later, one undo step.
        let start = e.seq().find_clip(a).unwrap().1.timeline_start;
        e.nudge(2);
        let frame = e.seq().frame_rate.frame_duration();
        assert_eq!(e.seq().find_clip(a).unwrap().1.timeline_start, start + Time(frame.0 * 2));
        assert!(e.status.starts_with("Nudged 2 frames later"), "{}", e.status);
        // Paste attributes: nothing copied yet.
        e.paste_attributes(reelforge_core::attrs::AttrSet::default());
        assert!(e.status.starts_with("Paste attributes: copy a clip first"));
        // Make a composition from the selected title.
        let comps = e.project.sequences.len();
        e.make_composite();
        assert_eq!(e.project.sequences.len(), comps + 1, "{}", e.status);
        let c = e.selection.unwrap();
        assert!(e.seq().find_clip(c).unwrap().1.nested.is_some());
        e.undo();
        assert!(e.seq().find_clip(a).is_some());
    }

    #[test]
    fn a_tracks_waveform_switch_is_saved_with_it() {
        let mut e = editor();
        let v = e.seq().tracks.iter().position(|t| t.kind == TrackKind::Video).unwrap();
        assert_eq!(e.seq().tracks[v].waveforms, None);
        e.set_track_waveforms(v, Some(true));
        assert_eq!(e.seq().tracks[v].waveforms, Some(true));
        let json = serde_json::to_string(&e.project).unwrap();
        let back: Project = serde_json::from_str(&json).unwrap();
        assert_eq!(back.sequences[0].tracks[v].waveforms, Some(true));
        e.set_track_waveforms(v, None);
        // Not written when it follows Settings (older projects read the same).
        assert!(!serde_json::to_string(&e.seq().tracks[v]).unwrap().contains("waveforms"));
    }

    #[test]
    fn playback_quality_is_full_half_or_quarter() {
        let mut e = editor();
        assert_eq!(e.prefs.playback_quality, 1);
        e.set_playback_quality(2);
        assert_eq!((e.prefs.playback_quality, e.preview.quality()), (2, 2));
        assert!(e.status.contains("half"));
        // Anything else rounds to the nearest offered.
        e.set_playback_quality(3);
        assert_eq!(e.prefs.playback_quality, 2);
        e.set_playback_quality(8);
        assert_eq!(e.prefs.playback_quality, 4);
        e.set_playback_quality(0);
        assert_eq!(e.prefs.playback_quality, 0, "automatic");
        assert!(e.status.contains("automatic"));
        // Kept with the settings.
        let mut p = e.prefs.clone();
        p.playback_quality = 4;
        e.set_prefs(p);
        assert_eq!(e.preview.quality(), 4);

        // The Viewer's picture at half size, and full again.
        let Some(gpu) = reelforge_render::headless_device() else { return };
        let mut e = Editor::new(Some(gpu), None);
        e.add_title();
        let [w, h] = [e.seq().width, e.seq().height];
        e.tick();
        assert_eq!(e.preview.read_output().unwrap().1, [w, h]);
        e.set_playback_quality(2);
        e.tick();
        assert_eq!(e.preview.read_output().unwrap().1, [w / 2, h / 2]);
        e.set_playback_quality(1);
        e.tick();
        assert_eq!(e.preview.read_output().unwrap().1, [w, h]);

        // Full when paused: quarter while playing, full again on pausing.
        e.set_playback_quality(4);
        e.set_full_when_paused(true);
        e.tick();
        assert_eq!(e.preview.read_output().unwrap().1, [w, h], "paused: full");
        e.playback.play();
        e.tick();
        assert_eq!(e.preview.read_output().unwrap().1, [w / 4, h / 4], "playing: quarter");
        e.playback.pause();
        e.tick();
        assert_eq!(e.preview.read_output().unwrap().1, [w, h], "paused again: full");
    }

    #[test]
    fn composition_thumbnails_render_and_follow_edits() {
        let mut e = editor();
        assert!(e.comp_thumbnail(9, Time::ZERO, 200).is_none());
        let Some(gpu) = reelforge_render::headless_device() else { return };
        let mut e = Editor::new(Some(gpu), None);
        e.new_composite();
        let comp = e.active;
        e.add_title();
        let first = e.comp_thumbnail(comp, Time::ZERO, 200).expect("a picture");
        assert_eq!(&first[..2], &[0xff, 0xd8], "a JPEG");
        // Asked again unchanged: the same picture, from the cache.
        assert_eq!(e.comp_thumbnail(comp, Time::ZERO, 200).unwrap(), first);
        // An edit inside the composition: a new picture.
        let id = e.seq().tracks.iter().flat_map(|t| &t.clips).next().unwrap().id;
        e.select_only(Some(id));
        e.delete_selection();
        assert_ne!(e.comp_thumbnail(comp, Time::ZERO, 200).unwrap(), first);
    }

/// Running short of disk space: a warning with what can be cleared, no
    /// more rendered frames kept on disk and no proxies started by themselves;
    /// back to normal when there's room again.
    #[test]
    fn low_disk_space_is_warned_about_and_stops_filling_the_drive() {
        let mut e = editor();
        e.new_project_with(320, 180, reelforge_core::FrameRate::new(25, 1));
        // Never short.
        e.disk_low_at = 0;
        e.check_disk();
        assert!(e.disk_low.is_none());
        // Pretend every drive is short.
        e.disk_low_at = u64::MAX;
        e.check_disk();
        let d = e.disk_low.clone().expect("low");
        assert!(d.free > 0 && !d.place.is_empty());
        assert!(e.status.contains("Running out of disk space"), "{}", e.status);
        if let Some(store) = &e.render_cache.disk {
            assert!(store.paused(), "no more frames kept on disk");
        }
        // Said once, not every check.
        e.status.clear();
        e.check_disk();
        assert!(e.status.is_empty());
        // Clearing previews looks again (still short here).
        e.free_disk_space("render");
        assert!(e.status.contains("Rendered previews cleared") && e.status.contains("Still short"), "{}", e.status);
        // Dismissed: stays hidden while short, comes back after recovering and running short again.
        e.disk_dismissed = true;
        e.check_disk();
        assert!(e.disk_dismissed);
        e.disk_low_at = 0;
        e.check_disk();
        assert!(e.disk_low.is_none() && !e.disk_dismissed);
        if let Some(store) = &e.render_cache.disk {
            assert!(!store.paused(), "keeping frames again");
        }
    }

    #[test]
    fn effect_pictures_for_the_browser() {
        use reelforge_core::effects::{Effect, EffectKind};
        let Some(gpu) = reelforge_render::headless_device() else { return };
        let mut e = Editor::new(Some(gpu), None);
        e.new_project_with(640, 360, reelforge_core::FrameRate::new(25, 1));
        e.add_plane([200, 60, 40, 255]);
        let id = e.selection.unwrap();
        let before = e.seq().find_clip(id).unwrap().1.effects.len();
        // A do-nothing effect shows the clip as it is; Halftone changes it.
        let plain = e.effect_thumbnail(id, Effect::new(EffectKind::ColorCorrect), 160).expect("a picture");
        assert_eq!(&plain[..2], &[0xff, 0xd8], "a JPEG");
        // Its size, from the JPEG's frame header (drawn at half size, then halved again).
        let sof = plain.windows(2).position(|w| w == [0xff, 0xc0]).expect("a frame header");
        let (h, w) = (u16::from_be_bytes([plain[sof + 5], plain[sof + 6]]), u16::from_be_bytes([plain[sof + 7], plain[sof + 8]]));
        assert_eq!((w, h), (160, 90));
        let dots = e.effect_thumbnail(id, Effect::new(EffectKind::Halftone), 160).unwrap();
        assert_ne!(plain, dots);
        // From the cache when nothing changed; the project itself is untouched.
        assert_eq!(e.effect_thumbnail(id, Effect::new(EffectKind::Halftone), 160).unwrap(), dots);
        assert_eq!(e.seq().find_clip(id).unwrap().1.effects.len(), before);
        // The clip changes: a new picture.
        e.project.sequences[e.active].find_clip_mut(id).unwrap().1.effects.push(Effect::new(EffectKind::Vignette));
        assert_ne!(e.effect_thumbnail(id, Effect::new(EffectKind::Halftone), 160).unwrap(), dots);
        // Frames of a moving preview: later frames of an effect that changes over time differ.
        let g0 = e.effect_thumbnail_at(id, Effect::new(EffectKind::Glitch), 160, 0).unwrap();
        let later: Vec<Vec<u8>> = (1..6).map(|k| e.effect_thumbnail_at(id, Effect::new(EffectKind::Glitch), 160, k).unwrap()).collect();
        assert!(later.iter().any(|f| *f != g0), "Glitch moves");
        assert_eq!(e.effect_thumbnail_at(id, Effect::new(EffectKind::Glitch), 160, 3).unwrap(), later[2], "cached per frame");
        // Past the clip's end it wraps round (no failure).
        assert!(e.effect_thumbnail_at(id, Effect::new(EffectKind::Glitch), 160, 100_000).is_some());
        // Cameras have no picture to show it on.
        e.add_scene_object(reelforge_core::space3d::SceneObject::Camera, 0);
        let cam = e.selection.unwrap();
        assert!(e.effect_thumbnail(cam, Effect::new(EffectKind::Halftone), 160).is_none());
    }

    #[test]
    fn rendered_previews_are_found_on_disk_by_a_new_session() {
        let dir = std::env::temp_dir().join(format!("rf_render_disk_editor_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let Some(gpu) = reelforge_render::headless_device() else { return };
        let mut e = Editor::new(Some(gpu), None);
        e.render_disk_dir = Some(dir.clone());
        e.prefs.render_cache_disk_mb = 256;
        e.apply_prefs();
        e.add_title();
        let id = e.seq().tracks.iter().flat_map(|t| &t.clips).next().unwrap().id;
        // Half a second of it.
        let a = e.active;
        {
            // Letters fading in: every frame differs (a still title's frames are all
            // one picture, kept once).
            let c = e.project.sequences[a].find_clip_mut(id).unwrap().1;
            c.duration = Time::from_secs_f64(0.5);
            c.title.as_mut().unwrap().anim = reelforge_core::TextAnim { kind: reelforge_core::TextAnimKind::FadeLetters, stagger: 0.04, length: 0.4, delay: 0.0, out: false };
        }
        e.render_preview();
        for _ in 0..200 {
            if e.render_cache.job.is_none() {
                break;
            }
            e.tick();
        }
        assert!(e.status.starts_with("Rendered the preview"), "{}", e.status);
        e.render_cache.disk.as_ref().unwrap().flush();
        let on_disk = e.render_cache.disk.as_ref().unwrap().stats().1;
        assert!(on_disk >= 10, "{on_disk}: {}", e.status);
        // Another session (the app reopened): the same edit finds its frames.
        let Some(gpu) = reelforge_render::headless_device() else { return };
        let mut f = Editor::new(Some(gpu), None);
        f.render_disk_dir = Some(dir.clone());
        f.prefs.render_cache_disk_mb = 256;
        f.apply_prefs();
        f.project = e.project.clone();
        f.render_preview();
        for _ in 0..200 {
            if f.render_cache.job.is_none() {
                break;
            }
            f.tick();
        }
        assert!(f.status.starts_with("Preview already rendered"), "{}", f.status);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn render_preview_needs_something_to_render_and_stops() {
        let mut e = editor();
        e.render_preview();
        assert!(e.status.starts_with("Nothing to render"), "{}", e.status);
        e.add_title();
        e.render_preview();
        assert!(e.render_progress().is_some());
        e.stop_render_preview();
        assert!(e.render_progress().is_none());
        assert!(e.status.starts_with("Stopped rendering"));
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
    fn mask_points_can_be_added_and_deleted_one_step_each() {
        let mut e = editor();
        e.add_title();
        let id = e.selection.unwrap();
        let size = { let c = e.seq().find_clip(id).unwrap().1.clone(); e.layer_size(&c) };
        let i = e.add_mask(id, Mask::centred(reelforge_core::mask::MaskShape::Rectangle, size)).unwrap();
        let pts = |e: &Editor| e.seq().find_clip(id).unwrap().1.masks[i].points.clone();
        let orig = pts(&e);
        let steps = e.undo.position();
        // After point 0: between points 0 and 1; before point 0: between the last and the first.
        assert_eq!(e.add_mask_point(id, i, 0, false), Some(1));
        let p = pts(&e);
        assert_eq!(p.len(), 5);
        assert_eq!(p[1], [(orig[0][0] + orig[1][0]) / 2.0, (orig[0][1] + orig[1][1]) / 2.0]);
        assert_eq!(e.seq().find_clip(id).unwrap().1.masks[i].shape, reelforge_core::mask::MaskShape::Freehand);
        assert!(e.seq().find_clip(id).unwrap().1.masks[i].name.starts_with("Freehand"));
        assert_eq!(e.add_mask_point(id, i, 0, true), Some(5));
        assert_eq!(pts(&e)[5], [(orig[3][0] + orig[0][0]) / 2.0, (orig[3][1] + orig[0][1]) / 2.0]);
        assert_eq!(e.undo.position(), steps + 2);
        assert!(e.delete_mask_point(id, i, 5));
        assert_eq!(pts(&e).len(), 5);
        e.undo();
        e.undo();
        e.undo();
        assert_eq!(pts(&e), orig);
    }

    /// A mask on an adjustment layer (no footage of its own) is tracked
    /// through the footage below it, which is shown at twice its size: the
    /// keys land in the adjustment layer's own (frame) pixels and time.
    #[test]
    fn an_adjustment_layers_mask_tracks_through_the_footage_below() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("rf-engine-adjtrack-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("block.mp4");
        // 320 × 180: a textured block 60 px wide moving 4 px right a frame over flat grey.
        let child = std::process::Command::new("ffmpeg")
            .args(["-y", "-loglevel", "error", "-f", "rawvideo", "-pix_fmt", "gray", "-s", "320x180", "-r", "25", "-i", "-", "-c:v", "libx264", "-crf", "12", "-pix_fmt", "yuv420p"])
            .arg(&path)
            .stdin(std::process::Stdio::piped())
            .spawn();
        let Ok(mut child) = child else { return };
        let mut stdin = child.stdin.take().unwrap();
        let hash = |x: i32, y: i32| (((x * 73856093) ^ (y * 19349663)) as u32 % 160) as u8 + 50;
        for k in 0..20 {
            let x0 = 60 + 4 * k;
            let mut img = vec![120u8; 320 * 180];
            for y in 60..120 {
                for x in x0..x0 + 60 {
                    img[y as usize * 320 + x as usize] = hash((x - x0) / 3, y / 3);
                }
            }
            stdin.write_all(&img).unwrap();
        }
        drop(stdin);
        if !child.wait().is_ok_and(|s| s.success()) {
            return;
        }
        let mut e = editor();
        e.new_project_with(640, 360, FrameRate::new(25, 1));
        let info = reelforge_media::probe(&path).unwrap();
        e.project.media.push(MediaAsset { id: MediaId(1), path: path.clone(), name: "block.mp4".into(), info, conform: None });
        let secs = Time::from_secs_f64;
        let (f, a) = (ClipId(e.project.alloc_id()), ClipId(e.project.alloc_id()));
        let mut footage = Clip::new(f, MediaId(1), Time::ZERO, Time::ZERO, secs(0.8), None);
        footage.transform.scale = [2.0, 2.0];
        e.project.sequences[0].tracks[0].clips.push(footage);
        let mut adj = Clip::new_adjustment(a, Time::ZERO, secs(0.8));
        // Round the block as the frame shows it (media 60–120 → frame 120–240 across, 120–240 down).
        adj.masks.push(Mask { points: vec![[125.0, 125.0], [235.0, 125.0], [235.0, 235.0], [125.0, 235.0]], ..Mask::default() });
        e.project.sequences[0].tracks[1].clips.push(adj);
        assert_eq!(e.footage_below(a), Some(f));
        e.playback.playhead = Time::ZERO;
        e.start_mask_tracking(a, 0, true, reelforge_core::track::MaskMotion::Position);
        assert!(e.status.contains("through the footage below"), "{}", e.status);
        let t0 = std::time::Instant::now();
        while e.tracking.is_some() && t0.elapsed().as_secs() < 30 {
            e.poll_tracking();
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let m = e.seq().find_clip(a).unwrap().1.masks[0].clone();
        eprintln!("adjustment mask track: {} keys; {}", m.keys.len(), e.status);
        assert!(m.keys.len() >= 18, "{} keys: {}", m.keys.len(), e.status);
        // Moving 4 media px a frame is 8 frame px a frame at 200 %.
        for key in &m.keys {
            let k = (key.at.as_secs_f64() * 25.0).round() as f32;
            let dx = key.points[0][0] - 125.0;
            assert!((dx - 8.0 * k).abs() < 1.5, "frame {k}: moved {dx}, not {}", 8.0 * k);
            assert!((key.points[0][1] - 125.0).abs() < 1.5);
        }
        // A clip with no footage at all below it can't be tracked.
        e.project.sequences[0].tracks[0].clips.clear();
        e.start_mask_tracking(a, 0, true, reelforge_core::track::MaskMotion::Position);
        assert!(e.tracking.is_none() && e.status.contains("no"), "{}", e.status);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A preset goes on several clips in one undo step: the look on video
    /// clips, the title style on titles (keeping their words), the audio on
    /// the audio linked to a video clip, the transition where there's a clip
    /// before. Previewing changes nothing for good.
    #[test]
    fn presets_go_on_many_clips_at_once_and_preview_leaves_no_trace() {
        use crate::fx_presets::{ClipParts, EffectPreset, SaveOptions, TransitionPreset};
        let mut e = editor();
        let secs = Time::from_secs_f64;
        let (a, b, t, au) = (ClipId(e.project.alloc_id()), ClipId(e.project.alloc_id()), ClipId(e.project.alloc_id()), ClipId(e.project.alloc_id()));
        let link = Some(77);
        let s = &mut e.project.sequences[0];
        let v1 = s.tracks.iter().position(|t| t.kind == TrackKind::Video).unwrap();
        let a1 = s.tracks.iter().position(|t| t.kind == TrackKind::Audio).unwrap();
        s.tracks[v1].clips.push(Clip::new(a, MediaId(1), Time::ZERO, Time::ZERO, secs(2.0), link));
        s.tracks[v1].clips.push(Clip::new(b, MediaId(1), secs(2.0), secs(2.0), secs(2.0), None));
        s.tracks[a1].clips.push(Clip::new(au, MediaId(1), Time::ZERO, Time::ZERO, secs(2.0), link));
        let v2 = s.tracks.iter().enumerate().filter(|(_, t)| t.kind == TrackKind::Video).nth(1).unwrap().0;
        s.tracks[v2].clips.push(Clip::new_title(t, Time::ZERO, secs(2.0), TitleSpec { text: "Keep me".into(), ..TitleSpec::default() }));
        let blur = EffectPreset::from_clip("Blurry", &[reelforge_core::Effect::new(EffectKind::Blur)], Time::ZERO).unwrap();
        let mut p = blur.clone();
        p.title = Some(TitleSpec { text: String::new(), size: 120.0, ..TitleSpec::default() });
        p.audio = Some(ClipAudio { volume_db: -9.0, ..ClipAudio::default() });
        p.transition = Some(TransitionPreset { kind: reelforge_core::Transition::default(), length: secs(0.5) });
        let undo_before = e.undo.position();
        assert_eq!(e.apply_preset(&[a, b, t], &p, false), 3);
        assert_eq!(e.undo.position(), undo_before + 1, "one undo step");
        let clip = |e: &Editor, id| e.seq().find_clip(id).unwrap().1.clone();
        assert_eq!(clip(&e, a).effects.len(), 1);
        assert_eq!(clip(&e, b).dissolve_in, secs(0.5), "a transition where there's a clip before");
        assert_eq!(clip(&e, a).dissolve_in, Time::ZERO, "none at the start");
        assert_eq!(clip(&e, au).audio.volume_db, -9.0, "the linked audio");
        let tt = clip(&e, t).title.unwrap();
        assert_eq!((tt.text.as_str(), tt.size), ("Keep me", 120.0));
        e.undo();
        assert!(clip(&e, a).effects.is_empty() && clip(&e, au).audio.volume_db == 0.0 && clip(&e, t).title.unwrap().size != 120.0);
        // Preview, then put back: nothing changed, nothing to save, no undo step.
        e.saved_rev = e.rev;
        let undo_before = e.undo.position();
        e.preview_preset(a, &blur);
        assert_eq!(clip(&e, a).effects.len(), 1);
        e.end_preset_preview();
        assert!(clip(&e, a).effects.is_empty() && !e.is_dirty());
        assert_eq!(e.undo.position(), undo_before);
        // Anything else done ends the preview first.
        e.preview_preset(a, &blur);
        e.commit_live_edit();
        assert!(clip(&e, a).effects.is_empty());
        // Built from a clip's parts.
        let parts = ClipParts { audio: Some(clip(&e, au).audio), ..ClipParts::default() };
        let opts = SaveOptions { effects: Some(vec![]), audio: true, ..SaveOptions::default() };
        assert!(EffectPreset::from_clip_parts("x", &clip(&e, a).visual(), &parts, Time::ZERO, secs(2.0), [1.0, 1.0], &opts).is_some());
    }

    /// Mask from key: a green screen with a skin-coloured box: the new mask
    /// goes round the box, in the layer's pixels, as one undo step.
    #[test]
    fn a_mask_traced_from_a_key() {
        let dir = std::env::temp_dir().join(format!("rf-engine-maskkey-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("screen.mp4");
        let ok = std::process::Command::new("ffmpeg")
            .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i", "color=c=0x32b450:s=640x360:r=25:d=1,drawbox=x=200:y=100:w=240:h=160:color=0xcd967d:t=fill", "-pix_fmt", "yuv420p"])
            .arg(&path)
            .status()
            .is_ok_and(|s| s.success());
        if !ok {
            return;
        }
        let mut e = editor();
        let info = reelforge_media::probe(&path).unwrap();
        e.project.media.push(MediaAsset { id: MediaId(1), path: path.clone(), name: "screen.mp4".into(), info, conform: None });
        let id = ClipId(e.project.alloc_id());
        let mut c = Clip::new(id, MediaId(1), Time::ZERO, Time::ZERO, Time::from_secs_f64(1.0), None);
        c.effects.push(reelforge_core::Effect::new(EffectKind::ColorCorrect));
        c.effects.push(reelforge_core::Effect::new(EffectKind::ChromaKey));
        e.project.sequences[0].tracks[0].clips.push(c);
        assert!(e.mask_from_key(id, 0).is_none(), "not a key");
        let undo = e.undo.position();
        let i = e.mask_from_key(id, 1).expect(&e.status);
        assert_eq!(e.undo.position(), undo + 1);
        let m = &e.seq().find_clip(id).unwrap().1.masks[i];
        let xs: Vec<f32> = m.points.iter().map(|p| p[0]).collect();
        let ys: Vec<f32> = m.points.iter().map(|p| p[1]).collect();
        let (x0, x1) = (xs.iter().cloned().fold(f32::MAX, f32::min), xs.iter().cloned().fold(f32::MIN, f32::max));
        let (y0, y1) = (ys.iter().cloned().fold(f32::MAX, f32::min), ys.iter().cloned().fold(f32::MIN, f32::max));
        assert!((x0 - 200.0).abs() < 6.0 && (x1 - 440.0).abs() < 6.0 && (y0 - 100.0).abs() < 6.0 && (y1 - 260.0).abs() < 6.0, "{x0}–{x1} × {y0}–{y1}");
        assert!(m.points.len() <= 12, "a box needs few points: {}", m.points.len());
        let _ = std::fs::remove_dir_all(&dir);
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
