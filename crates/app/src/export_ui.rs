//! File → Export: settings dialog, background render job, progress and result windows.

use crate::app::EditorState;
use eframe::egui_wgpu::wgpu;
use egui::{Align2, RichText};
use reelforge_core::{Project, Time};
use reelforge_export::{export_sequence, frame_range, Cancelled, ExportSettings, Progress, Report};
use reelforge_media::Quality;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Scale {
    Full,
    Half,
    Quarter,
}

impl Scale {
    fn divisor(self) -> u32 {
        match self {
            Scale::Full => 1,
            Scale::Half => 2,
            Scale::Quarter => 4,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Scale::Full => "Full",
            Scale::Half => "Half",
            Scale::Quarter => "Quarter",
        }
    }
}

struct Dialog {
    path: String,
    scale: Scale,
    quality: Quality,
    include_audio: bool,
    /// Export only between the in and out points.
    marked_only: bool,
}

enum Msg {
    Progress(Progress),
    Done(Result<Report, String>),
}

struct Job {
    rx: Receiver<Msg>,
    cancel: Arc<AtomicBool>,
    progress: Option<Progress>,
    path: PathBuf,
}

enum Outcome {
    Finished(Report),
    Failed(String),
}

#[derive(Default)]
pub struct ExportUi {
    dialog: Option<Dialog>,
    job: Option<Job>,
    outcome: Option<Outcome>,
}

impl ExportUi {
    pub fn is_running(&self) -> bool {
        self.job.is_some()
    }

    /// Open the export dialog (File → Export…, Ctrl+M).
    pub fn open(&mut self, st: &EditorState) {
        if self.job.is_some() {
            return;
        }
        let path = self
            .dialog
            .as_ref()
            .map(|d| d.path.clone())
            .unwrap_or_else(|| default_path(st).to_string_lossy().into_owned());
        let marked_only = st.seq().in_point.is_some() || st.seq().out_point.is_some();
        self.dialog = Some(Dialog { path, scale: Scale::Full, quality: Quality::Medium, include_audio: true, marked_only });
        self.outcome = None;
    }

    pub fn show(&mut self, ctx: &egui::Context, st: &mut EditorState) {
        self.poll(st);
        self.dialog_window(ctx, st);
        self.progress_window(ctx);
        self.outcome_window(ctx);
    }

    fn poll(&mut self, st: &mut EditorState) {
        let Some(job) = &mut self.job else { return };
        while let Ok(msg) = job.rx.try_recv() {
            match msg {
                Msg::Progress(p) => job.progress = Some(p),
                Msg::Done(result) => {
                    match result {
                        Ok(r) => {
                            st.status = format!("Exported {} ({} frames, {:.1} s)", r.path.display(), r.frames, r.elapsed.as_secs_f64());
                            self.outcome = Some(Outcome::Finished(r));
                        }
                        Err(e) if e == "cancelled" => st.status = "Export cancelled".into(),
                        Err(e) => {
                            st.status = format!("Export failed: {e}");
                            self.outcome = Some(Outcome::Failed(e));
                        }
                    }
                    self.job = None;
                    return;
                }
            }
        }
    }

    fn dialog_window(&mut self, ctx: &egui::Context, st: &mut EditorState) {
        let Some(d) = &mut self.dialog else { return };
        let seq = st.seq();
        let (w, h) = (seq.width, seq.height);
        let duration = seq.duration();
        let marks = seq.in_point.is_some() || seq.out_point.is_some();
        let marked = seq.marked_range();
        if !marks {
            d.marked_only = false;
        }
        let (range_start, range_end) = if d.marked_only { marked.unwrap_or((Time::ZERO, Time::ZERO)) } else { (Time::ZERO, duration) };
        let (first, last) = frame_range(seq.frame_rate, range_start, range_end);
        let has_audio = !reelforge_export::mix_clips(&st.project, 48_000).is_empty();
        let mut open = true;
        let mut start = false;
        let mut close = false;

        egui::Window::new("Export")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                egui::Grid::new("export_grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                    ui.label("File");
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut d.path).desired_width(320.0));
                        if ui.button("Browse…").clicked() {
                            let current = PathBuf::from(&d.path);
                            let mut dlg = rfd::FileDialog::new()
                                .add_filter("MPEG-4 video", &["mp4"])
                                .add_filter("QuickTime", &["mov"])
                                .add_filter("Matroska", &["mkv"]);
                            if let Some(dir) = current.parent().filter(|p| p.is_dir()) {
                                dlg = dlg.set_directory(dir);
                            }
                            if let Some(name) = current.file_name() {
                                dlg = dlg.set_file_name(name.to_string_lossy());
                            }
                            if let Some(p) = dlg.save_file() {
                                d.path = p.to_string_lossy().into_owned();
                            }
                        }
                    });
                    ui.end_row();

                    ui.label("Format");
                    ui.weak("H.264 video + AAC audio");
                    ui.end_row();

                    ui.label("Resolution");
                    egui::ComboBox::from_id_salt("export_scale")
                        .selected_text(format!("{} ({}×{})", d.scale.label(), (w / d.scale.divisor()) & !1, (h / d.scale.divisor()) & !1))
                        .show_ui(ui, |ui| {
                            for s in [Scale::Full, Scale::Half, Scale::Quarter] {
                                let text = format!("{} ({}×{})", s.label(), (w / s.divisor()) & !1, (h / s.divisor()) & !1);
                                ui.selectable_value(&mut d.scale, s, text);
                            }
                        });
                    ui.end_row();

                    ui.label("Quality");
                    egui::ComboBox::from_id_salt("export_quality").selected_text(d.quality.label()).show_ui(ui, |ui| {
                        for q in Quality::ALL {
                            ui.selectable_value(&mut d.quality, q, q.label());
                        }
                    });
                    ui.end_row();

                    ui.label("Audio");
                    ui.add_enabled_ui(has_audio, |ui| ui.checkbox(&mut d.include_audio, "Include audio (48 kHz stereo)"))
                        .response
                        .on_disabled_hover_text("The timeline has no unmuted audio clips");
                    ui.end_row();

                    ui.label("Range");
                    ui.vertical(|ui| {
                        ui.radio_value(&mut d.marked_only, false, format!("Whole sequence ({})", duration.timecode(seq.frame_rate)));
                        let label = match marked {
                            Some((a, b)) => format!("In to out: {} – {}", a.timecode(seq.frame_rate), b.timecode(seq.frame_rate)),
                            None if marks => "In to out (the marks don't make a range)".into(),
                            None => "In to out".into(),
                        };
                        ui.add_enabled_ui(marks, |ui| ui.radio_value(&mut d.marked_only, true, label))
                            .response
                            .on_disabled_hover_text("Set an in point (I) and/or an out point (O) on the timeline first");
                        ui.weak(format!(
                            "{} · {} frames @ {:.3} fps",
                            (range_end - range_start).max(Time::ZERO).timecode(seq.frame_rate),
                            last - first,
                            seq.frame_rate.fps()
                        ));
                    });
                    ui.end_row();
                });
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let can = range_end > range_start && !d.path.trim().is_empty();
                    if ui.add_enabled(can, egui::Button::new(RichText::new("Export").strong())).clicked() {
                        start = true;
                    }
                    if ui.button("Cancel").clicked() {
                        close = true;
                    }
                    if duration == Time::ZERO {
                        ui.weak("The timeline is empty.");
                    }
                });
            });

        let missing: Vec<String> = reelforge_core::undo::missing_media(&st.project).iter().filter_map(|id| st.project.media(*id).map(|m| m.name.clone())).collect();
        if start && !missing.is_empty() {
            st.status = format!("Can't export: missing media ({}). Relink it in the Media panel first.", missing.join(", "));
            start = false;
        }
        if start {
            let mut path = PathBuf::from(d.path.trim());
            if path.extension().is_none() {
                path.set_extension("mp4");
            }
            let settings = ExportSettings {
                path: path.clone(),
                width: (w / d.scale.divisor()).max(2),
                height: (h / d.scale.divisor()).max(2),
                quality: d.quality,
                include_audio: d.include_audio && has_audio,
                start: range_start,
                end: range_end,
            };
            match st.preview.gpu() {
                Some((device, queue)) => {
                    self.job = Some(spawn(ctx.clone(), st.project.clone(), settings, device, queue));
                    st.status = format!("Exporting to {}…", path.display());
                    self.dialog = None;
                }
                None => st.status = "Export needs the GPU renderer, which isn't available".into(),
            }
        } else if close || !open {
            self.dialog = None;
        }
    }

    fn progress_window(&mut self, ctx: &egui::Context) {
        let Some(job) = &self.job else { return };
        egui::Window::new("Exporting…")
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.set_min_width(380.0);
                ui.label(job.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
                let p = job.progress;
                let frac = p.map_or(0.0, |p| p.fraction());
                ui.add(egui::ProgressBar::new(frac).show_percentage().animate(true));
                match p {
                    Some(p) => {
                        let fps = p.done as f64 / p.elapsed.as_secs_f64().max(1e-3);
                        let eta = p.remaining().map(fmt_duration).unwrap_or_else(|| "estimating…".into());
                        ui.weak(format!("Frame {} of {} · {:.1} fps · {} left", p.done, p.total, fps, eta));
                    }
                    None => {
                        ui.weak("Starting…");
                    }
                }
                ui.add_space(6.0);
                if job.cancel.load(Ordering::Relaxed) {
                    ui.weak("Cancelling…");
                } else if ui.button("Cancel").clicked() {
                    job.cancel.store(true, Ordering::Relaxed);
                }
            });
    }

    fn outcome_window(&mut self, ctx: &egui::Context) {
        let Some(outcome) = &self.outcome else { return };
        let mut open = true;
        let mut close = false;
        egui::Window::new("Export")
            .id(egui::Id::new("export_outcome"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                match outcome {
                    Outcome::Finished(r) => {
                        ui.label(RichText::new("Export complete").strong());
                        ui.label(r.path.display().to_string());
                        ui.weak(format!(
                            "{}×{} · {} frames · {}{} · took {}",
                            r.width,
                            r.height,
                            r.frames,
                            r.video_codec,
                            if r.has_audio { " + AAC" } else { "" },
                            fmt_duration(r.elapsed)
                        ));
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            if ui.button("Show in folder").clicked() {
                                show_in_folder(&r.path);
                            }
                            if ui.button("Close").clicked() {
                                close = true;
                            }
                        });
                    }
                    Outcome::Failed(e) => {
                        ui.label(RichText::new("Export failed").strong().color(ui.visuals().error_fg_color));
                        ui.label(e);
                        if ui.button("Close").clicked() {
                            close = true;
                        }
                    }
                }
            });
        if close || !open {
            self.outcome = None;
        }
    }
}

fn spawn(ctx: egui::Context, project: Project, settings: ExportSettings, device: wgpu::Device, queue: wgpu::Queue) -> Job {
    let (tx, rx) = channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let path = settings.path.clone();
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
    Job { rx, cancel, progress: None, path }
}

/// `<folder>/<sequence name>.mp4`, next to the project file or in the user's Videos folder.
fn default_path(st: &EditorState) -> PathBuf {
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
    let mut path = dir.join(format!("{name}.mp4"));
    let mut n = 2;
    while path.exists() {
        path = dir.join(format!("{name} ({n}).mp4"));
        n += 1;
    }
    path
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
