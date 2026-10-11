//! File → Export: the settings dialog (with presets), and the export queue
//! that renders jobs one after another on a background thread.
//!
//! Every job takes a snapshot of the project when it's added, so you can keep
//! editing (or queue several versions of the same timeline) while it renders.

use crate::app::EditorState;
use eframe::egui_wgpu::wgpu;
use egui::{Align2, RichText};
use reelforge_core::{Project, Sequence, Time};
use reelforge_export::{export_sequence, frame_range, Cancelled, ExportSettings, Progress, Report};
use reelforge_media::{Fit, Quality, VideoCodec};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use std::time::Duration;

// ---- Presets ---------------------------------------------------------------

/// The output size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Size {
    /// The sequence's size divided by 1, 2 or 4.
    Scaled(u32),
    /// A fixed size (presets, and "Custom").
    Fixed(u32, u32),
}

impl Size {
    pub fn resolve(self, seq_w: u32, seq_h: u32) -> (u32, u32) {
        match self {
            Size::Scaled(d) => (((seq_w / d) & !1).max(2), ((seq_h / d) & !1).max(2)),
            Size::Fixed(w, h) => (w.max(2), h.max(2)),
        }
    }
}

const COMMON_SIZES: [(u32, u32, &str); 6] = [
    (3840, 2160, "4K UHD"),
    (1920, 1080, "1080p"),
    (1280, 720, "720p"),
    (1080, 1920, "Vertical 9:16"),
    (1080, 1350, "Portrait 4:5"),
    (1080, 1080, "Square 1:1"),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Preset {
    YouTube1080,
    YouTube4k,
    Vertical,
    Portrait,
    Square,
    MasterProRes,
    MasterDnxhr,
    Draft,
    PngSequence,
    JpegSequence,
    AudioWav,
    Custom,
}

/// What a preset sets.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spec {
    pub video: Option<VideoCodec>,
    pub size: Size,
    pub quality: Quality,
    pub fit: Fit,
    pub hardware: bool,
}

impl Preset {
    pub const ALL: [Preset; 11] = [
        Preset::YouTube1080,
        Preset::YouTube4k,
        Preset::Vertical,
        Preset::Portrait,
        Preset::Square,
        Preset::MasterProRes,
        Preset::MasterDnxhr,
        Preset::Draft,
        Preset::PngSequence,
        Preset::JpegSequence,
        Preset::AudioWav,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Preset::YouTube1080 => "YouTube 1080p",
            Preset::YouTube4k => "YouTube 4K (H.265)",
            Preset::Vertical => "Shorts / Reels / TikTok (9:16)",
            Preset::Portrait => "Instagram portrait (4:5)",
            Preset::Square => "Square (1:1)",
            Preset::MasterProRes => "Master: ProRes 422 HQ",
            Preset::MasterDnxhr => "Master: DNxHR HQ",
            Preset::Draft => "Quick draft (half size, GPU)",
            Preset::PngSequence => "PNG image sequence",
            Preset::JpegSequence => "JPEG image sequence",
            Preset::AudioWav => "Audio only (WAV)",
            Preset::Custom => "Custom",
        }
    }

    pub fn hint(self) -> &'static str {
        match self {
            Preset::YouTube1080 => "H.264 + AAC at 1920×1080, high quality. Plays everywhere.",
            Preset::YouTube4k => "H.265 + AAC at 3840×2160. Smaller files than H.264 at the same quality.",
            Preset::Vertical => "1080×1920 for phone-first platforms. Wide footage is cropped to fill the frame.",
            Preset::Portrait => "1080×1350 for feeds. Wide footage is cropped to fill the frame.",
            Preset::Square => "1080×1080. Wide footage is cropped to fill the frame.",
            Preset::MasterProRes => "Near-lossless 10-bit master at the sequence size, with uncompressed audio. Big files.",
            Preset::MasterDnxhr => "Avid's master codec at the sequence size, with uncompressed audio. Big files.",
            Preset::Draft => "Fast, small H.264 for checking a cut. Uses the graphics card's encoder if there is one.",
            Preset::PngSequence => "One lossless PNG per frame, for VFX and colour work. No audio.",
            Preset::JpegSequence => "One JPEG per frame, for thumbnails and contact sheets. No audio.",
            Preset::AudioWav => "The mixed soundtrack as 24-bit, 48 kHz stereo WAV.",
            Preset::Custom => "Your own choice of settings.",
        }
    }

    pub fn spec(self) -> Option<Spec> {
        let s = |video, size, quality, fit, hardware| Some(Spec { video, size, quality, fit, hardware });
        use VideoCodec as V;
        match self {
            Preset::YouTube1080 => s(Some(V::H264), Size::Fixed(1920, 1080), Quality::High, Fit::Letterbox, false),
            Preset::YouTube4k => s(Some(V::H265), Size::Fixed(3840, 2160), Quality::High, Fit::Letterbox, false),
            Preset::Vertical => s(Some(V::H264), Size::Fixed(1080, 1920), Quality::High, Fit::Crop, false),
            Preset::Portrait => s(Some(V::H264), Size::Fixed(1080, 1350), Quality::High, Fit::Crop, false),
            Preset::Square => s(Some(V::H264), Size::Fixed(1080, 1080), Quality::High, Fit::Crop, false),
            Preset::MasterProRes => s(Some(V::ProRes422Hq), Size::Scaled(1), Quality::High, Fit::Letterbox, false),
            Preset::MasterDnxhr => s(Some(V::DnxhrHq), Size::Scaled(1), Quality::High, Fit::Letterbox, false),
            Preset::Draft => s(Some(V::H264), Size::Scaled(2), Quality::Low, Fit::Letterbox, true),
            Preset::PngSequence => s(Some(V::PngSequence), Size::Scaled(1), Quality::High, Fit::Letterbox, false),
            Preset::JpegSequence => s(Some(V::JpegSequence), Size::Scaled(1), Quality::High, Fit::Letterbox, false),
            Preset::AudioWav => s(None, Size::Scaled(1), Quality::High, Fit::Letterbox, false),
            Preset::Custom => None,
        }
    }
}

fn extension(video: Option<VideoCodec>) -> &'static str {
    video.map_or("wav", |v| v.extension())
}

fn format_label(video: Option<VideoCodec>) -> &'static str {
    video.map_or("Audio only (WAV)", |v| v.label())
}

// ---- Dialog ----------------------------------------------------------------

#[derive(Clone)]
struct Dialog {
    path: String,
    preset: Preset,
    spec: Spec,
    include_audio: bool,
    /// Export only between the in and out points.
    marked_only: bool,
}

impl Dialog {
    fn apply(&mut self, preset: Preset) {
        self.preset = preset;
        if let Some(spec) = preset.spec() {
            self.spec = spec;
        }
        self.fix_extension();
    }

    /// Make the file name's extension match the format.
    fn fix_extension(&mut self) {
        let want = extension(self.spec.video);
        let mut p = PathBuf::from(self.path.trim());
        if p.as_os_str().is_empty() || p.extension().is_some_and(|e| e.eq_ignore_ascii_case(want)) {
            return;
        }
        p.set_extension(want);
        self.path = p.to_string_lossy().into_owned();
    }

    /// Settings changed by hand that no longer match the preset make it "Custom".
    fn update_preset(&mut self) {
        if self.preset.spec() != Some(self.spec) {
            self.preset = Preset::ALL.into_iter().find(|p| p.spec() == Some(self.spec)).unwrap_or(Preset::Custom);
        }
    }
}

// ---- Queue -----------------------------------------------------------------

enum Msg {
    Progress(Progress),
    Done(Result<Report, String>),
}

enum JobState {
    Waiting,
    Running { rx: Receiver<Msg>, cancel: Arc<AtomicBool>, progress: Option<Progress> },
    Finished(Report),
    Failed(String),
    Cancelled,
}

impl JobState {
    fn is_over(&self) -> bool {
        matches!(self, JobState::Finished(_) | JobState::Failed(_) | JobState::Cancelled)
    }
}

struct Job {
    id: u64,
    /// What the queue shows: file name and preset.
    title: String,
    project: Arc<Project>,
    settings: ExportSettings,
    state: JobState,
}

#[derive(Default)]
pub struct ExportUi {
    dialog: Option<Dialog>,
    /// The last settings used, to start the next export from.
    last: Option<Dialog>,
    jobs: Vec<Job>,
    /// Start waiting jobs automatically (off after "Pause").
    run: bool,
    queue_open: bool,
    next_id: u64,
}

impl ExportUi {
    pub fn is_running(&self) -> bool {
        self.jobs.iter().any(|j| matches!(j.state, JobState::Running { .. }))
    }

    /// A short note for the status bar while the queue is busy.
    pub fn summary(&self) -> Option<String> {
        let active = self.jobs.iter().filter(|j| !j.state.is_over()).count();
        let running = self.jobs.iter().find_map(|j| match &j.state {
            JobState::Running { progress, .. } => Some(progress.map_or(0.0, |p| p.fraction())),
            _ => None,
        });
        match running {
            Some(f) if active > 1 => Some(format!("Exporting {:.0}% · {} more queued", f * 100.0, active - 1)),
            Some(f) => Some(format!("Exporting {:.0}%", f * 100.0)),
            None if active > 0 => Some(format!("{active} export{} waiting", if active == 1 { "" } else { "s" })),
            None => None,
        }
    }

    pub fn open_queue(&mut self) {
        self.queue_open = true;
    }

    /// Open the export dialog (File → Export…, Ctrl+M).
    pub fn open(&mut self, st: &EditorState) {
        let marked_only = st.seq().in_point.is_some() || st.seq().out_point.is_some();
        let mut d = match &self.last {
            Some(d) => Dialog { marked_only, ..d.clone() },
            None => Dialog {
                path: String::new(),
                preset: Preset::Custom,
                spec: Preset::YouTube1080.spec().unwrap(),
                include_audio: true,
                marked_only,
            },
        };
        if self.last.is_none() {
            // First time: match the sequence (a vertical sequence gets the vertical preset).
            let seq = st.seq();
            let preset = if seq.height > seq.width { Preset::Vertical } else if seq.width >= 3200 { Preset::YouTube4k } else { Preset::YouTube1080 };
            d.apply(preset);
        }
        d.path = self.default_path(st, extension(d.spec.video)).to_string_lossy().into_owned();
        self.dialog = Some(d);
    }

    pub fn show(&mut self, ctx: &egui::Context, st: &mut EditorState) {
        self.poll(st);
        self.start_next(ctx, st);
        self.dialog_window(ctx, st);
        self.queue_window(ctx);
        if self.jobs.iter().any(|j| !j.state.is_over()) {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
    }

    fn poll(&mut self, st: &mut EditorState) {
        for job in &mut self.jobs {
            let JobState::Running { rx, progress, .. } = &mut job.state else { continue };
            let mut done = None;
            while let Ok(msg) = rx.try_recv() {
                match msg {
                    Msg::Progress(p) => *progress = Some(p),
                    Msg::Done(r) => done = Some(r),
                }
            }
            match done {
                Some(Ok(r)) => {
                    st.status = format!(
                        "Exported {} ({} frames, {}, {})",
                        r.path.display(),
                        r.frames,
                        encoder_note(&r),
                        fmt_duration(r.elapsed)
                    );
                    job.state = JobState::Finished(r);
                }
                Some(Err(e)) if e == "cancelled" => {
                    st.status = format!("Export of {} cancelled", job.title);
                    job.state = JobState::Cancelled;
                }
                Some(Err(e)) => {
                    st.status = format!("Export failed: {e}");
                    job.state = JobState::Failed(e);
                    self.queue_open = true;
                }
                None => {}
            }
        }
    }

    /// Run the next waiting job when nothing is running.
    fn start_next(&mut self, ctx: &egui::Context, st: &mut EditorState) {
        if !self.run || self.is_running() {
            return;
        }
        let Some(job) = self.jobs.iter_mut().find(|j| matches!(j.state, JobState::Waiting)) else {
            self.run = false;
            return;
        };
        match st.preview.gpu() {
            Some((device, queue)) => {
                let (rx, cancel) = spawn(ctx.clone(), job.project.clone(), job.settings.clone(), device, queue);
                job.state = JobState::Running { rx, cancel, progress: None };
                st.status = format!("Exporting {}…", job.title);
            }
            None => {
                job.state = JobState::Failed("Export needs the GPU renderer, which isn't available".into());
            }
        }
    }

    /// Add a job; returns false (with a status message) if it can't be.
    fn enqueue(&mut self, st: &mut EditorState, settings: ExportSettings, preset: Preset) -> bool {
        let missing: Vec<String> = reelforge_core::undo::missing_media(&st.project).iter().filter_map(|id| st.project.media(*id).map(|m| m.name.clone())).collect();
        if !missing.is_empty() {
            st.status = format!("Can't export: missing media ({}). Relink it in the Media panel first.", missing.join(", "));
            return false;
        }
        let out = settings.output_path();
        if self.jobs.iter().any(|j| !j.state.is_over() && j.settings.output_path() == out) {
            st.status = format!("{} is already in the export queue; pick another file name", settings.path.display());
            return false;
        }
        let name = settings.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let title = if preset == Preset::Custom { name } else { format!("{name} ({})", preset.label()) };
        self.next_id += 1;
        self.jobs.push(Job { id: self.next_id, title, project: Arc::new(st.project.clone()), settings, state: JobState::Waiting });
        true
    }

    fn dialog_window(&mut self, ctx: &egui::Context, st: &mut EditorState) {
        let Some(d) = &mut self.dialog else { return };
        let seq = st.seq();
        let (sw, sh) = (seq.width, seq.height);
        let duration = seq.duration();
        let marks = seq.in_point.is_some() || seq.out_point.is_some();
        let marked = seq.marked_range();
        if !marks {
            d.marked_only = false;
        }
        let (range_start, range_end) = if d.marked_only { marked.unwrap_or((Time::ZERO, Time::ZERO)) } else { (Time::ZERO, duration) };
        let has_audio = !reelforge_export::mix_clips(&st.project, 48_000).is_empty();
        let mut open = true;
        let mut action = None::<bool>; // Some(true) = export now, Some(false) = add to queue
        let mut close = false;
        let busy = self.jobs.iter().any(|j| !j.state.is_over());

        egui::Window::new("Export")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                let format_before = d.spec.video;
                egui::Grid::new("export_grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                    ui.label("Preset");
                    ui.vertical(|ui| {
                        let mut preset = d.preset;
                        egui::ComboBox::from_id_salt("export_preset").width(300.0).selected_text(preset.label()).show_ui(ui, |ui| {
                            for p in Preset::ALL {
                                ui.selectable_value(&mut preset, p, p.label()).on_hover_text(p.hint());
                            }
                        });
                        if preset != d.preset {
                            d.apply(preset);
                        }
                        ui.weak(d.preset.hint());
                    });
                    ui.end_row();

                    ui.label("File");
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut d.path).desired_width(320.0));
                        if ui.button("Browse…").clicked() {
                            browse(d);
                        }
                    });
                    ui.end_row();

                    ui.label("Format");
                    ui.vertical(|ui| {
                        egui::ComboBox::from_id_salt("export_format").width(300.0).selected_text(format_label(d.spec.video)).show_ui(ui, |ui| {
                            for v in VideoCodec::ALL {
                                ui.selectable_value(&mut d.spec.video, Some(v), v.label());
                            }
                            ui.selectable_value(&mut d.spec.video, None, format_label(None));
                        });
                        if let Some(v) = d.spec.video.filter(|v| v.is_sequence()) {
                            let p = PathBuf::from(d.path.trim());
                            let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "frame".into());
                            ui.weak(format!("Writes {stem}_00001.{ext}, {stem}_00002.{ext}, … (no audio)", ext = v.extension()));
                        }
                    });
                    ui.end_row();

                    if let Some(v) = d.spec.video {
                        ui.label("Size");
                        ui.horizontal(|ui| size_picker(ui, &mut d.spec.size, sw, sh));
                        ui.end_row();

                        let (w, h) = d.spec.size.resolve(sw, sh);
                        let differs = (w as f64 / h as f64) / (sw as f64 / sh as f64) - 1.0;
                        if differs.abs() > 0.005 {
                            ui.label("Shape");
                            ui.vertical(|ui| {
                                ui.horizontal(|ui| {
                                    for f in Fit::ALL {
                                        ui.radio_value(&mut d.spec.fit, f, f.label());
                                    }
                                });
                                ui.weak(format!("The sequence is {sw}×{sh}; this frame is a different shape."));
                            });
                            ui.end_row();
                        }

                        if v.uses_quality() {
                            ui.label("Quality");
                            egui::ComboBox::from_id_salt("export_quality").selected_text(d.spec.quality.label()).show_ui(ui, |ui| {
                                for q in Quality::ALL {
                                    ui.selectable_value(&mut d.spec.quality, q, q.label());
                                }
                            });
                            ui.end_row();
                        }

                        ui.label("Encoder");
                        ui.add_enabled_ui(v.has_hardware_encoders(), |ui| {
                            ui.checkbox(&mut d.spec.hardware, "Use the graphics card (NVENC / Quick Sync / AMF)")
                                .on_hover_text("Much faster, slightly bigger files at the same quality. If no GPU encoder works, the software encoder is used instead.")
                        })
                        .response
                        .on_disabled_hover_text("Only H.264 and H.265 have GPU encoders");
                        ui.end_row();
                    }

                    let sequence = d.spec.video.is_some_and(|v| v.is_sequence());
                    ui.label("Audio");
                    if d.spec.video.is_none() {
                        ui.weak(if has_audio { "48 kHz stereo, 24-bit" } else { "The timeline has no unmuted audio clips" });
                    } else {
                        ui.add_enabled_ui(has_audio && !sequence, |ui| ui.checkbox(&mut d.include_audio, "Include audio (48 kHz stereo)"))
                            .response
                            .on_disabled_hover_text(if sequence { "Image sequences have no audio" } else { "The timeline has no unmuted audio clips" });
                    }
                    ui.end_row();

                    ui.label("Range");
                    range_picker(ui, d, seq, marks, marked, duration, (range_start, range_end));
                    ui.end_row();
                });
                if d.spec.video != format_before {
                    d.fix_extension();
                }
                d.update_preset();

                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let audio_ok = d.spec.video.is_some() || has_audio;
                    let can = range_end > range_start && !d.path.trim().is_empty() && audio_ok;
                    let now = if busy { "Export after the queue" } else { "Export" };
                    if ui.add_enabled(can, egui::Button::new(RichText::new(now).strong())).clicked() {
                        action = Some(true);
                    }
                    if ui.add_enabled(can, egui::Button::new("Add to queue")).on_hover_text("Queue it and set up another; start the queue from its window").clicked() {
                        action = Some(false);
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                    if duration == Time::ZERO {
                        ui.weak("The timeline is empty.");
                    } else if !audio_ok {
                        ui.weak("Nothing to export: no audio.");
                    }
                });
            });

        if let Some(now) = action {
            let d = d.clone();
            let mut path = PathBuf::from(d.path.trim());
            if path.extension().is_none() {
                path.set_extension(extension(d.spec.video));
            }
            let (width, height) = d.spec.size.resolve(sw, sh);
            let settings = ExportSettings {
                path,
                width,
                height,
                quality: d.spec.quality,
                include_audio: (d.include_audio || d.spec.video.is_none()) && has_audio,
                start: range_start,
                end: range_end,
                video: d.spec.video,
                hardware: d.spec.hardware,
                fit: d.spec.fit,
            };
            if self.enqueue(st, settings, d.preset) {
                self.last = Some(d);
                self.dialog = None;
                if now {
                    self.run = true;
                    self.queue_open = true;
                } else {
                    let waiting = self.jobs.iter().filter(|j| matches!(j.state, JobState::Waiting)).count();
                    st.status = format!("Added to the export queue ({waiting} waiting). File > Export queue to start it.");
                    self.queue_open = true;
                }
            }
        } else if close || !open {
            self.dialog = None;
        }
    }

    fn queue_window(&mut self, ctx: &egui::Context) {
        if !self.queue_open {
            return;
        }
        let mut open = true;
        let mut remove = None;
        let mut retry = None;
        let mut move_up = None;
        egui::Window::new("Export queue")
            .open(&mut open)
            .collapsible(true)
            .resizable(true)
            .default_width(520.0)
            .pivot(Align2::CENTER_CENTER)
            .default_pos(ctx.content_rect().center())
            .show(ctx, |ui| {
                if self.jobs.is_empty() {
                    ui.weak("Nothing queued. Use File > Export… and \"Add to queue\".");
                }
                egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                    for (i, job) in self.jobs.iter().enumerate() {
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                ui.set_width(ui.available_width() - 150.0);
                                ui.label(RichText::new(&job.title).strong()).on_hover_text(job.settings.output_path().display().to_string());
                                let s = &job.settings;
                                let what = match s.video {
                                    Some(v) => format!("{} · {}×{}", v.label(), s.width, s.height),
                                    None => "WAV audio".into(),
                                };
                                match &job.state {
                                    JobState::Waiting => {
                                        ui.weak(format!("{what} · waiting"));
                                    }
                                    JobState::Running { progress, cancel, .. } => {
                                        let frac = progress.map_or(0.0, |p| p.fraction());
                                        ui.add(egui::ProgressBar::new(frac).show_percentage().animate(true));
                                        let line = match progress {
                                            _ if cancel.load(Ordering::Relaxed) => "Cancelling…".to_string(),
                                            Some(p) => {
                                                let fps = p.done as f64 / p.elapsed.as_secs_f64().max(1e-3);
                                                let eta = p.remaining().map(fmt_duration).unwrap_or_else(|| "estimating…".into());
                                                format!("Frame {} of {} · {:.1} fps · {} left", p.done, p.total, fps, eta)
                                            }
                                            None => "Starting…".into(),
                                        };
                                        ui.weak(line);
                                    }
                                    JobState::Finished(r) => {
                                        ui.colored_label(egui::Color32::from_rgb(110, 200, 120), format!("Done · {} frames · {} · {}", r.frames, encoder_note(r), fmt_duration(r.elapsed)));
                                    }
                                    JobState::Failed(e) => {
                                        ui.colored_label(ui.visuals().error_fg_color, format!("Failed: {e}"));
                                    }
                                    JobState::Cancelled => {
                                        ui.weak(format!("{what} · cancelled"));
                                    }
                                }
                            });
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| match &job.state {
                                JobState::Running { cancel, .. } => {
                                    if ui.button("Cancel").clicked() {
                                        cancel.store(true, Ordering::Relaxed);
                                    }
                                }
                                JobState::Waiting => {
                                    if ui.button("Remove").clicked() {
                                        remove = Some(job.id);
                                    }
                                    if i > 0 && ui.small_button("^").on_hover_text("Move up").clicked() {
                                        move_up = Some(i);
                                    }
                                }
                                JobState::Finished(r) => {
                                    if ui.button("Remove").clicked() {
                                        remove = Some(job.id);
                                    }
                                    if ui.button("Show in folder").clicked() {
                                        show_in_folder(&first_file(r, &job.settings));
                                    }
                                }
                                JobState::Failed(_) | JobState::Cancelled => {
                                    if ui.button("Remove").clicked() {
                                        remove = Some(job.id);
                                    }
                                    if ui.button("Retry").clicked() {
                                        retry = Some(job.id);
                                    }
                                }
                            });
                        });
                        ui.separator();
                    }
                });
                ui.horizontal(|ui| {
                    let waiting = self.jobs.iter().filter(|j| matches!(j.state, JobState::Waiting)).count();
                    if self.run {
                        if ui.button("Pause after this one").on_hover_text("Finish the current export, then wait").clicked() {
                            self.run = false;
                        }
                    } else if ui.add_enabled(waiting > 0, egui::Button::new(RichText::new(format!("Start queue ({waiting})")).strong())).clicked() {
                        self.run = true;
                    }
                    let finished = self.jobs.iter().filter(|j| j.state.is_over()).count();
                    if ui.add_enabled(finished > 0, egui::Button::new("Clear finished")).clicked() {
                        self.jobs.retain(|j| !j.state.is_over());
                    }
                });
            });
        if let Some(id) = remove {
            self.jobs.retain(|j| j.id != id);
        }
        if let Some(i) = move_up {
            self.jobs.swap(i, i - 1);
        }
        if let Some(job) = retry.and_then(|id| self.jobs.iter_mut().find(|j| j.id == id)) {
            job.state = JobState::Waiting;
            self.run = true;
        }
        if !open {
            self.queue_open = false;
        }
    }

    /// `<folder>/<sequence name>.<ext>`, next to the project file or in the
    /// user's Videos folder, not clashing with a file or a queued job.
    fn default_path(&self, st: &EditorState, ext: &str) -> PathBuf {
        let dir = st
            .project_path
            .as_ref()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .or_else(|| {
                let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
                let videos = PathBuf::from(home).join("Videos");
                videos.is_dir().then_some(videos)
            })
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_default();
        let name: String = st.seq().name.chars().map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' || c == '_' { c } else { '_' }).collect();
        let name = if name.trim().is_empty() { "export".to_string() } else { name.trim().to_string() };
        let taken = |p: &Path| p.exists() || self.jobs.iter().any(|j| !j.state.is_over() && j.settings.path == p);
        let mut path = dir.join(format!("{name}.{ext}"));
        let mut n = 2;
        while taken(&path) {
            path = dir.join(format!("{name} ({n}).{ext}"));
            n += 1;
        }
        path
    }
}

fn size_picker(ui: &mut egui::Ui, size: &mut Size, sw: u32, sh: u32) {
    let label = |s: Size| -> String {
        let (w, h) = s.resolve(sw, sh);
        match s {
            Size::Scaled(1) => format!("Sequence size ({w}×{h})"),
            Size::Scaled(2) => format!("Half ({w}×{h})"),
            Size::Scaled(_) => format!("Quarter ({w}×{h})"),
            Size::Fixed(..) => match COMMON_SIZES.iter().find(|c| (c.0, c.1) == (w, h)) {
                Some(c) => format!("{} ({w}×{h})", c.2),
                None => format!("Custom ({w}×{h})"),
            },
        }
    };
    let custom = matches!(*size, Size::Fixed(w, h) if !COMMON_SIZES.iter().any(|c| (c.0, c.1) == (w, h)));
    egui::ComboBox::from_id_salt("export_size").width(220.0).selected_text(label(*size)).show_ui(ui, |ui| {
        for d in [1, 2, 4] {
            ui.selectable_value(size, Size::Scaled(d), label(Size::Scaled(d)));
        }
        ui.separator();
        for (w, h, _) in COMMON_SIZES {
            ui.selectable_value(size, Size::Fixed(w, h), label(Size::Fixed(w, h)));
        }
        ui.separator();
        if ui.selectable_label(custom, "Custom…").clicked() && !custom {
            let (w, h) = size.resolve(sw, sh);
            // Nudge off the common sizes so the custom fields show.
            *size = Size::Fixed(w + 2, h);
        }
    });
    if custom {
        if let Size::Fixed(w, h) = size {
            ui.add(egui::DragValue::new(w).range(16..=8192).speed(2));
            ui.label("×");
            ui.add(egui::DragValue::new(h).range(16..=8192).speed(2));
            // Most video codecs need even sizes.
            *w = (*w + 1) & !1;
            *h = (*h + 1) & !1;
        }
    }
}

fn range_picker(ui: &mut egui::Ui, d: &mut Dialog, seq: &Sequence, marks: bool, marked: Option<(Time, Time)>, duration: Time, (start, end): (Time, Time)) {
    let (first, last) = frame_range(seq.frame_rate, start, end);
    ui.vertical(|ui| {
        ui.radio_value(&mut d.marked_only, false, format!("Whole sequence ({})", duration.timecode(seq.frame_rate)));
        let label = match marked {
            Some((a, b)) => format!("In to out: {} - {}", a.timecode(seq.frame_rate), b.timecode(seq.frame_rate)),
            None if marks => "In to out (the marks don't make a range)".into(),
            None => "In to out".into(),
        };
        ui.add_enabled_ui(marks, |ui| ui.radio_value(&mut d.marked_only, true, label))
            .response
            .on_disabled_hover_text("Set an in point (I) and/or an out point (O) on the timeline first");
        ui.weak(format!("{} · {} frames @ {:.3} fps", (end - start).max(Time::ZERO).timecode(seq.frame_rate), last - first, seq.frame_rate.fps()));
    });
}

fn browse(d: &mut Dialog) {
    let current = PathBuf::from(d.path.trim());
    let ext = extension(d.spec.video);
    let mut dlg = rfd::FileDialog::new().add_filter(format_label(d.spec.video), &[ext]);
    if let Some(dir) = current.parent().filter(|p| p.is_dir()) {
        dlg = dlg.set_directory(dir);
    }
    if let Some(name) = current.file_name() {
        dlg = dlg.set_file_name(name.to_string_lossy());
    }
    if let Some(p) = dlg.save_file() {
        d.path = p.to_string_lossy().into_owned();
        d.fix_extension();
    }
}

fn encoder_note(r: &Report) -> String {
    if r.hardware { format!("{} on the GPU", r.video_codec) } else { r.video_codec.clone() }
}

/// For an image sequence, the first image (the report holds the pattern).
fn first_file(r: &Report, s: &ExportSettings) -> PathBuf {
    match s.video {
        Some(v) if v.is_sequence() => reelforge_media::sequence_file(&r.path, 1),
        _ => r.path.clone(),
    }
}

fn spawn(ctx: egui::Context, project: Arc<Project>, settings: ExportSettings, device: wgpu::Device, queue: wgpu::Queue) -> (Receiver<Msg>, Arc<AtomicBool>) {
    let (tx, rx) = channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let stop = cancel.clone();
    std::thread::Builder::new()
        .name("export".into())
        .spawn(move || {
            let mut last_sent = std::time::Instant::now() - Duration::from_secs(1);
            let result = export_sequence(&project, &settings, &device, &queue, |p| {
                // Throttle UI updates to ~20/s.
                if last_sent.elapsed() > Duration::from_millis(50) || p.done == p.total {
                    let _ = tx.send(Msg::Progress(p));
                    ctx.request_repaint();
                    last_sent = std::time::Instant::now();
                }
                !stop.load(Ordering::Relaxed)
            });
            let msg = match result {
                Ok(r) => Ok(r),
                Err(e) if e.is::<Cancelled>() => Err("cancelled".to_string()),
                Err(e) => Err(format!("{e:#}")),
            };
            let _ = tx.send(Msg::Done(msg));
            ctx.request_repaint();
        })
        .expect("spawn export thread");
    (rx, cancel)
}

fn show_in_folder(path: &Path) {
    #[cfg(target_os = "windows")]
    let r = std::process::Command::new("explorer").arg(format!("/select,{}", path.display())).spawn();
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("open").arg("-R").arg(path).spawn();
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    let r = std::process::Command::new("xdg-open").arg(path.parent().unwrap_or(path)).spawn();
    if let Err(e) = r {
        log::warn!("couldn't open file browser: {e}");
    }
}

fn fmt_duration(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 {
        format!("{}h {:02}m", s / 3600, (s / 60) % 60)
    } else if s >= 60 {
        format!("{}m {:02}s", s / 60, s % 60)
    } else {
        format!("{:.1}s", d.as_secs_f64())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dialog(path: &str) -> Dialog {
        Dialog { path: path.into(), preset: Preset::Custom, spec: Preset::YouTube1080.spec().unwrap(), include_audio: true, marked_only: false }
    }

    #[test]
    fn presets_set_format_size_and_extension() {
        let mut d = dialog("/v/My film.mp4");
        d.apply(Preset::MasterProRes);
        assert_eq!(d.path, "/v/My film.mov");
        assert_eq!(d.spec.size.resolve(2560, 1440), (2560, 1440), "masters keep the sequence size");
        d.apply(Preset::Vertical);
        assert_eq!((d.path.as_str(), d.spec.size.resolve(1920, 1080), d.spec.fit), ("/v/My film.mp4", (1080, 1920), Fit::Crop));
        d.apply(Preset::AudioWav);
        assert!(d.path.ends_with(".wav") && d.spec.video.is_none());
        d.apply(Preset::Draft);
        assert_eq!(d.spec.size.resolve(1920, 1080), (960, 540));
        // Every preset (except Custom) is recognised as itself.
        for p in Preset::ALL {
            let mut d = dialog("x.mp4");
            d.apply(p);
            d.preset = Preset::Custom;
            d.update_preset();
            assert_eq!(d.preset, p);
        }
    }

    #[test]
    fn changing_a_setting_makes_it_custom() {
        let mut d = dialog("a.mp4");
        d.apply(Preset::YouTube1080);
        d.spec.quality = Quality::Low;
        d.update_preset();
        assert_eq!(d.preset, Preset::Custom);
        d.spec.quality = Quality::High;
        d.update_preset();
        assert_eq!(d.preset, Preset::YouTube1080, "back to matching a preset");
    }

    #[test]
    fn odd_sequence_sizes_scale_to_even_ones() {
        assert_eq!(Size::Scaled(2).resolve(1918, 1078), (958, 538));
        assert_eq!(Size::Scaled(4).resolve(6, 6), (2, 2));
    }
}
