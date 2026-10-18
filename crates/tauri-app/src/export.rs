//! Export for the web interface: the settings (built-in presets, the user's
//! saved presets, every option), and the export queue that renders jobs one
//! after another in the background.
//!
//! Each job takes a snapshot of the project when it's added, so editing can
//! go on (or several versions of the same timeline can be queued) while it renders.

use crate::{ed, save_settings, Res};
use reelforge_core::{Project, Time};
use reelforge_engine::editor::Editor;
use reelforge_engine::presets::{extension, Preset, Spec, UserPreset, COMMON_SIZES};
use reelforge_export::{ExportSettings, Report};
use reelforge_media::{ColorOut, Fit, Quality, VideoCodec};
use serde::Serialize;
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

// ---- The options ---------------------------------------------------------------

#[derive(Serialize)]
pub struct Options {
    pub codecs: Vec<CodecView>,
    pub qualities: Vec<(Quality, &'static str)>,
    pub fits: Vec<(Fit, &'static str)>,
    pub colors: Vec<ColorView>,
    pub sizes: Vec<(u32, u32, &'static str)>,
    pub presets: Vec<PresetView>,
}

#[derive(Serialize)]
pub struct CodecView {
    pub id: VideoCodec,
    pub label: &'static str,
    pub extension: &'static str,
    /// An image sequence (no audio).
    pub sequence: bool,
    pub quality: bool,
    pub bitrate: bool,
    pub ten_bit: bool,
    pub hdr: bool,
    pub hardware: bool,
}

#[derive(Serialize)]
pub struct ColorView {
    pub id: ColorOut,
    pub label: &'static str,
    pub hint: &'static str,
}

#[derive(Serialize)]
pub struct PresetView {
    pub index: usize,
    pub label: &'static str,
    pub hint: &'static str,
    /// What it sets (`None` for "Custom").
    pub spec: Option<Spec>,
}

/// Everything the export window offers.
#[tauri::command]
pub fn export_options() -> Options {
    Options {
        codecs: VideoCodec::ALL
            .into_iter()
            .map(|v| CodecView {
                id: v,
                label: v.label(),
                extension: v.extension(),
                sequence: v.is_sequence(),
                quality: v.uses_quality(),
                bitrate: v.uses_bitrate(),
                ten_bit: v.supports_10bit(),
                hdr: v.supports_hdr(),
                hardware: v.has_hardware_encoders(),
            })
            .collect(),
        qualities: Quality::ALL.into_iter().map(|q| (q, q.label())).collect(),
        fits: Fit::ALL.into_iter().map(|f| (f, f.label())).collect(),
        colors: ColorOut::ALL.into_iter().map(|c| ColorView { id: c, label: c.label(), hint: c.hint() }).collect(),
        sizes: COMMON_SIZES.to_vec(),
        presets: Preset::ALL.into_iter().enumerate().map(|(index, p)| PresetView { index, label: p.label(), hint: p.hint(), spec: p.spec() }).collect(),
    }
}

#[derive(Serialize)]
pub struct Suggestion {
    /// A file name that doesn't clash with a file or a queued job.
    pub path: String,
    /// The built-in preset that fits the sequence's shape.
    pub preset: usize,
    /// The timeline has sound to export.
    pub has_audio: bool,
}

/// Where to export to (`<folder>/<sequence name>.<ext>`, next to the project
/// file or in the Videos folder) and a preset to start from.
#[tauri::command]
pub fn export_suggest(ext: String) -> Res<Suggestion> {
    ed(|e| {
        let s = e.seq();
        let preset = if s.height > s.width { Preset::Vertical } else if s.width >= 3200 { Preset::YouTube4k } else { Preset::YouTube1080 };
        Suggestion {
            path: default_path(e, &ext).display().to_string(),
            preset: Preset::ALL.iter().position(|p| *p == preset).unwrap_or(0),
            has_audio: !reelforge_export::mix_clips(&e.project, e.active, 48_000).is_empty(),
        }
    })
}

fn default_path(e: &Editor, ext: &str) -> PathBuf {
    let dir = e
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
    let name: String = e.seq().name.chars().map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' || c == '_' { c } else { '_' }).collect();
    let name = if name.trim().is_empty() { "export".to_string() } else { name.trim().to_string() };
    let queued: Vec<PathBuf> = queue(|q| q.jobs.iter().filter(|j| !j.state.is_over()).map(|j| j.settings.path.clone()).collect());
    let taken = |p: &Path| p.exists() || queued.iter().any(|q| q == p);
    let mut path = dir.join(format!("{name}.{ext}"));
    let mut n = 2;
    while taken(&path) {
        path = dir.join(format!("{name} ({n}).{ext}"));
        n += 1;
    }
    path
}

/// Ask where to export to; returns the chosen path.
#[tauri::command]
pub fn export_path_dialog(current: String, extension: String) -> Res<Option<String>> {
    let cur = PathBuf::from(current.trim());
    let mut dlg = rfd::FileDialog::new().add_filter(extension.to_uppercase(), &[extension.as_str()]);
    if let Some(dir) = cur.parent().filter(|p| p.is_dir()) {
        dlg = dlg.set_directory(dir);
    }
    if let Some(name) = cur.file_name() {
        dlg = dlg.set_file_name(name.to_string_lossy());
    }
    Ok(dlg.save_file().map(|p| p.display().to_string()))
}

// ---- Saved presets ----------------------------------------------------------------

/// Save these settings as a preset called `name` (replacing one of that name).
#[tauri::command]
pub fn export_preset_save(name: String, spec: Spec, include_audio: bool) -> Res<()> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err("Give the preset a name".into());
    }
    ed(|e| {
        let u = UserPreset { name: name.clone(), spec, include_audio };
        let list = &mut e.prefs.export_presets;
        match list.iter_mut().find(|x| x.name == name) {
            Some(x) => *x = u,
            None => list.push(u),
        }
        list.sort_by_key(|x| x.name.to_lowercase());
        save_settings(e);
        e.status = format!("Saved the export preset \"{name}\"");
    })
}

#[tauri::command]
pub fn export_preset_delete(name: String) -> Res<()> {
    ed(|e| {
        e.prefs.export_presets.retain(|x| x.name != name);
        save_settings(e);
        e.status = format!("Deleted the export preset \"{name}\"");
    })
}

// ---- The queue ----------------------------------------------------------------------

/// What a running job's thread reports.
#[derive(Default)]
struct Run {
    done: u64,
    total: u64,
    elapsed: f64,
    remaining: Option<f64>,
    result: Option<Result<Report, String>>,
}

enum JobState {
    Waiting,
    Running { cancel: Arc<AtomicBool>, run: Arc<Mutex<Run>> },
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
    title: String,
    /// Format, size and so on, in a few words.
    detail: String,
    project: Arc<Project>,
    settings: ExportSettings,
    state: JobState,
}

#[derive(Default)]
struct Queue {
    jobs: Vec<Job>,
    /// Start waiting jobs (off after "Pause").
    run: bool,
    next_id: u64,
}

thread_local! {
    static QUEUE: RefCell<Queue> = RefCell::new(Queue::default());
}

fn queue<R>(f: impl FnOnce(&mut Queue) -> R) -> R {
    QUEUE.with(|q| f(&mut q.borrow_mut()))
}

/// Add an export of the sequence shown to the queue. `start`: run the queue
/// now (Export) rather than just adding to it (Add to queue).
#[tauri::command]
pub fn export_add(spec: Spec, path: String, include_audio: bool, marked_only: bool, preset: Option<String>, start: bool) -> Res<()> {
    let mut path = PathBuf::from(path.trim());
    if path.as_os_str().is_empty() {
        return Err("Choose where to save the export".into());
    }
    if path.extension().is_none() {
        path.set_extension(extension(spec.video));
    }
    let (project, settings) = ed(|e| -> Res<(Project, ExportSettings)> {
        e.commit_live_edit();
        let missing: Vec<String> = reelforge_core::undo::missing_media(&e.project).iter().filter_map(|id| e.project.media(*id).map(|m| m.name.clone())).collect();
        if !missing.is_empty() {
            return Err(format!("Can't export: missing media ({}). Relink it first.", missing.join(", ")));
        }
        let s = e.seq();
        let (w, h) = spec.size.resolve(s.width, s.height);
        let (start, end) = if marked_only { s.marked_range().unwrap_or((Time::ZERO, s.duration())) } else { (Time::ZERO, s.duration()) };
        if end <= start {
            return Err("There's nothing to export: the timeline is empty".into());
        }
        let has_audio = !reelforge_export::mix_clips(&e.project, e.active, 48_000).is_empty();
        if spec.video.is_none() && !has_audio {
            return Err("Nothing to export: the timeline has no sound".into());
        }
        let settings = ExportSettings {
            path: path.clone(),
            width: w,
            height: h,
            quality: spec.quality,
            include_audio: (include_audio || spec.video.is_none()) && has_audio && !spec.video.is_some_and(|v| v.is_sequence()),
            start,
            end,
            video: spec.video,
            hardware: spec.hardware && spec.video.is_some_and(|v| v.has_hardware_encoders()),
            fit: spec.fit,
            sequence: e.active,
            color: if spec.video.is_some_and(|v| v.supports_10bit() || v.supports_hdr()) { spec.color } else { ColorOut::Sdr8 },
            bitrate_kbps: spec.bitrate_kbps.filter(|_| spec.video.is_some_and(|v| v.uses_bitrate())),
        };
        Ok((e.project.clone(), settings))
    })??;
    let seq_name = project.sequences.get(settings.sequence).map(|s| s.name.clone()).unwrap_or_default();
    queue(|q| {
        let out = settings.output_path();
        if q.jobs.iter().any(|j| !j.state.is_over() && j.settings.output_path() == out) {
            return Err(format!("{} is already in the export queue; pick another file name", settings.path.display()));
        }
        let name = settings.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let mut title = match &preset {
            Some(p) if !p.is_empty() => format!("{name} ({p})"),
            _ => name,
        };
        if settings.sequence != 0 {
            title = format!("{title} - {seq_name}");
        }
        q.next_id += 1;
        let detail = detail(&settings);
        q.jobs.push(Job { id: q.next_id, title, detail, project: Arc::new(project), settings, state: JobState::Waiting });
        if start {
            q.run = true;
        }
        Ok(())
    })?;
    if !start {
        let _ = ed(|e| {
            let waiting = queue(|q| q.jobs.iter().filter(|j| matches!(j.state, JobState::Waiting)).count());
            e.status = format!("Added to the export queue ({waiting} waiting)");
        });
    }
    Ok(())
}

fn detail(s: &ExportSettings) -> String {
    match s.video {
        Some(v) => {
            let mut w = format!("{} · {}×{}", v.label(), s.width, s.height);
            if s.color != ColorOut::Sdr8 {
                w += &format!(" · {}", s.color.label());
            }
            if let Some(k) = s.bitrate_kbps {
                w += &format!(" · {:.1} Mbit/s", k as f64 / 1000.0);
            } else if v.uses_quality() {
                w += &format!(" · {} quality", s.quality.label());
            }
            if s.hardware {
                w += " · GPU encoder";
            }
            w
        }
        None => "WAV audio".into(),
    }
}

/// A job: "cancel" (running), "remove", "retry" (failed or cancelled) or "up".
#[tauri::command]
pub fn export_job(id: u64, op: String) {
    queue(|q| {
        let Some(i) = q.jobs.iter().position(|j| j.id == id) else { return };
        match op.as_str() {
            "cancel" => {
                if let JobState::Running { cancel, .. } = &q.jobs[i].state {
                    cancel.store(true, Ordering::Relaxed);
                }
            }
            "remove" if !matches!(q.jobs[i].state, JobState::Running { .. }) => {
                q.jobs.remove(i);
            }
            "retry" if matches!(q.jobs[i].state, JobState::Failed(_) | JobState::Cancelled) => {
                q.jobs[i].state = JobState::Waiting;
                q.run = true;
            }
            "up" if i > 0 => q.jobs.swap(i, i - 1),
            _ => {}
        }
    })
}

/// Start (or, `false`, pause after the current job) the queue.
#[tauri::command]
pub fn export_run(on: bool) {
    queue(|q| q.run = on);
}

/// Forget finished, failed and cancelled jobs.
#[tauri::command]
pub fn export_clear() {
    queue(|q| q.jobs.retain(|j| !j.state.is_over()));
}

/// Called every tick: collect what running jobs report, and start the next
/// waiting job when nothing is running.
pub fn pump(e: &mut Editor) {
    queue(|q| {
        for job in &mut q.jobs {
            let JobState::Running { run, .. } = &job.state else { continue };
            let Some(result) = run.lock().unwrap().result.take() else { continue };
            job.state = match result {
                Ok(r) => {
                    e.status = format!("Exported {} ({} frames, {}, {:.1} s)", r.path.display(), r.frames, if r.hardware { format!("{} on the GPU", r.video_codec) } else { r.video_codec.clone() }, r.elapsed.as_secs_f64());
                    JobState::Finished(r)
                }
                Err(m) if m == "cancelled" => {
                    e.status = format!("Export of {} cancelled", job.title);
                    JobState::Cancelled
                }
                Err(m) => {
                    e.status = format!("Export failed: {m}");
                    JobState::Failed(m)
                }
            };
        }
        if !q.run || q.jobs.iter().any(|j| matches!(j.state, JobState::Running { .. })) {
            return;
        }
        let Some(job) = q.jobs.iter_mut().find(|j| matches!(j.state, JobState::Waiting)) else {
            q.run = false;
            return;
        };
        let Some((device, gpu_queue)) = e.preview.gpu() else {
            job.state = JobState::Failed("Export needs a graphics device, and there isn't one".into());
            return;
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let run = Arc::new(Mutex::new(Run::default()));
        let (project, settings) = (job.project.clone(), job.settings.clone());
        let (c, r) = (cancel.clone(), run.clone());
        std::thread::spawn(move || {
            let result = reelforge_export::export_sequence(&project, &settings, &device, &gpu_queue, |p| {
                let mut s = r.lock().unwrap();
                (s.done, s.total, s.elapsed, s.remaining) = (p.done, p.total, p.elapsed.as_secs_f64(), p.remaining().map(|d| d.as_secs_f64()));
                !c.load(Ordering::Relaxed)
            });
            r.lock().unwrap().result = Some(result.map_err(|e| if e.downcast_ref::<reelforge_export::Cancelled>().is_some() { "cancelled".to_string() } else { format!("{e:#}") }));
        });
        e.status = format!("Exporting {}…", job.title);
        job.state = JobState::Running { cancel, run };
    })
}

// ---- What the interface sees ----------------------------------------------------------

#[derive(Serialize, Default)]
pub struct QueueView {
    pub jobs: Vec<JobView>,
    pub run: bool,
    /// For the status bar: "Exporting 40% · 2 more queued".
    pub summary: Option<String>,
}

#[derive(Serialize)]
pub struct JobView {
    pub id: u64,
    pub title: String,
    pub detail: String,
    /// "waiting", "running", "done", "failed" or "cancelled".
    pub state: &'static str,
    pub done: u64,
    pub total: u64,
    pub fps: Option<f64>,
    pub remaining: Option<f64>,
    pub cancelling: bool,
    /// What happened (done or failed).
    pub message: Option<String>,
    /// The file written (the first image of a sequence), for "Show in folder".
    pub file: String,
}

pub fn view() -> QueueView {
    queue(|q| {
        let jobs: Vec<JobView> = q
            .jobs
            .iter()
            .map(|j| {
                let mut v = JobView {
                    id: j.id,
                    title: j.title.clone(),
                    detail: j.detail.clone(),
                    state: "waiting",
                    done: 0,
                    total: 0,
                    fps: None,
                    remaining: None,
                    cancelling: false,
                    message: None,
                    file: j.settings.path.display().to_string(),
                };
                match &j.state {
                    JobState::Waiting => {}
                    JobState::Running { cancel, run } => {
                        let r = run.lock().unwrap();
                        v.state = "running";
                        (v.done, v.total, v.remaining) = (r.done, r.total, r.remaining);
                        v.fps = (r.elapsed > 0.2 && r.done > 0).then(|| r.done as f64 / r.elapsed);
                        v.cancelling = cancel.load(Ordering::Relaxed);
                    }
                    JobState::Finished(r) => {
                        v.state = "done";
                        (v.done, v.total) = (r.frames, r.frames);
                        v.message = Some(format!("{} frames · {} · {:.1} s", r.frames, if r.hardware { format!("{} on the GPU", r.video_codec) } else { r.video_codec.clone() }, r.elapsed.as_secs_f64()));
                        v.file = match j.settings.video {
                            Some(c) if c.is_sequence() => reelforge_media::sequence_file(&r.path, 1),
                            _ => r.path.clone(),
                        }
                        .display()
                        .to_string();
                    }
                    JobState::Failed(m) => {
                        v.state = "failed";
                        v.message = Some(m.clone());
                    }
                    JobState::Cancelled => v.state = "cancelled",
                }
                v
            })
            .collect();
        let active = jobs.iter().filter(|j| matches!(j.state, "waiting" | "running")).count();
        let running = jobs.iter().find(|j| j.state == "running").map(|j| if j.total > 0 { j.done as f64 / j.total as f64 } else { 0.0 });
        let summary = match running {
            Some(f) if active > 1 => Some(format!("Exporting {:.0}% · {} more queued", f * 100.0, active - 1)),
            Some(f) => Some(format!("Exporting {:.0}%", f * 100.0)),
            None if active > 0 => Some(format!("{active} export{} waiting", if active == 1 { "" } else { "s" })),
            None => None,
        };
        QueueView { jobs, run: q.run, summary }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn details_name_the_format() {
        let s = ExportSettings {
            path: "a.mp4".into(),
            width: 1920,
            height: 1080,
            quality: Quality::High,
            include_audio: true,
            start: Time::ZERO,
            end: Time::from_secs_f64(1.0),
            video: Some(VideoCodec::H264),
            hardware: false,
            fit: Fit::Letterbox,
            sequence: 0,
            color: ColorOut::Sdr8,
            bitrate_kbps: Some(8000),
        };
        assert_eq!(detail(&s), "H.264 (MP4) · 1920×1080 · 8.0 Mbit/s");
        assert_eq!(detail(&ExportSettings { video: None, ..s }), "WAV audio");
    }

    #[test]
    fn saved_presets_round_trip_through_json() {
        let u = UserPreset { name: "Review".into(), spec: Preset::Draft.spec().unwrap(), include_audio: false };
        let s = serde_json::to_string(&u).unwrap();
        assert_eq!(serde_json::from_str::<UserPreset>(&s).unwrap(), u);
    }
}
