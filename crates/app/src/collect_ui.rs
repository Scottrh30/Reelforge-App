//! File → Collect project…: copy the project and every file it uses into one
//! folder (see `reelforge_core::collect`), in the background, with progress.

use crate::app::{EditorState, Pending};
use crossbeam_channel::{unbounded, Receiver};
use egui::{Color32, RichText};
use reelforge_core::collect::{plan_collect, same_file, CollectOptions, CollectPlan};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

/// What a finished collect did.
#[derive(Clone, Debug)]
pub struct Report {
    pub project_file: PathBuf,
    pub copied: usize,
    /// Already in the folder from an earlier collect (same size and date), so not copied again.
    pub reused: usize,
    pub bytes: u64,
    pub missing: Vec<PathBuf>,
    pub seconds: f64,
}

enum Msg {
    Progress { bytes: u64, file: String, index: usize },
    Done(Result<Report, String>),
}

struct Job {
    rx: Receiver<Msg>,
    cancel: Arc<AtomicBool>,
    bytes: u64,
    total: u64,
    file: String,
    index: usize,
    count: usize,
}

#[derive(Default)]
pub struct CollectState {
    pub open: bool,
    dest: Option<PathBuf>,
    name: String,
    opts: Opts,
    /// The plan for the current choices, and what it was made for.
    plan: Option<(PlanKey, CollectPlan)>,
    job: Option<Job>,
    result: Option<Result<Report, String>>,
}

#[derive(Clone, Copy, PartialEq)]
struct Opts(CollectOptions);

impl Default for Opts {
    fn default() -> Self {
        Opts(CollectOptions::default())
    }
}

#[derive(Clone, PartialEq)]
struct PlanKey {
    dest: PathBuf,
    name: String,
    opts: CollectOptions,
    rev: u64,
}

impl CollectState {
    pub fn busy(&self) -> bool {
        self.job.is_some()
    }
}

/// A name that's safe for a file on Windows too.
fn safe_name(s: &str) -> String {
    let t: String = s.chars().map(|c| if "<>:\"/\\|?*".contains(c) || c.is_control() { '_' } else { c }).collect();
    let t = t.trim().trim_end_matches('.').to_string();
    if t.is_empty() { "project".into() } else { t }
}

/// Copy one file, `on_bytes` told after every chunk. Returns false if an
/// identical copy was already there (nothing copied). The copy goes to a
/// temporary name first, so a stopped or failed copy never leaves a
/// half-written file under the real name; it keeps the source's date.
pub fn copy_file(from: &Path, to: &Path, cancel: &AtomicBool, mut on_bytes: impl FnMut(u64)) -> std::io::Result<bool> {
    if to.exists() && same_file(from, to) {
        on_bytes(std::fs::metadata(to)?.len());
        return Ok(false);
    }
    if let Some(dir) = to.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let part = to.with_file_name(format!("{}.part", to.file_name().unwrap_or_default().to_string_lossy()));
    let result = (|| -> std::io::Result<()> {
        let mut src = std::fs::File::open(from)?;
        let mut dst = std::fs::File::create(&part)?;
        let mut buf = vec![0u8; 4 << 20];
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(std::io::Error::new(std::io::ErrorKind::Interrupted, "stopped"));
            }
            let n = src.read(&mut buf)?;
            if n == 0 {
                break;
            }
            dst.write_all(&buf[..n])?;
            on_bytes(n as u64);
        }
        dst.sync_all()?;
        if let Ok(t) = std::fs::metadata(from).and_then(|m| m.modified()) {
            let _ = dst.set_modified(t);
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            if to.exists() {
                std::fs::remove_file(to)?;
            }
            std::fs::rename(&part, to)?;
            Ok(true)
        }
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            Err(e)
        }
    }
}

/// Carry out `plan`: copy every file, then write the project (with paths
/// relative to it). `progress` gets the bytes done so far, the file and its index.
pub fn run_collect(plan: &CollectPlan, cancel: &AtomicBool, mut progress: impl FnMut(u64, &str, usize)) -> Result<Report, String> {
    let started = Instant::now();
    std::fs::create_dir_all(&plan.dest).map_err(|e| format!("can't make the folder {}: {e}", plan.dest.display()))?;
    let (mut done, mut copied, mut reused) = (0u64, 0, 0);
    for (i, item) in plan.items.iter().enumerate() {
        let label = item.to.to_string_lossy().replace('\\', "/");
        progress(done, &label, i);
        let base = done;
        let mut moved = 0u64;
        let to = plan.dest.join(&item.to);
        match copy_file(&item.from, &to, cancel, |n| {
            moved += n;
            progress(base + moved, &label, i);
        }) {
            Ok(true) => copied += 1,
            Ok(false) => reused += 1,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => return Err("stopped: the files copied so far are in the folder, but no project was written".into()),
            Err(e) => return Err(format!("couldn't copy {}: {e}", item.from.display())),
        }
        done = base + item.size;
    }
    let json = serde_json::to_string_pretty(&plan.project.paths_relative_to(&plan.dest)).map_err(|e| e.to_string())?;
    crate::safety::write_atomic(&plan.project_file, json.as_bytes()).map_err(|e| format!("couldn't write {}: {e}", plan.project_file.display()))?;
    Ok(Report { project_file: plan.project_file.clone(), copied, reused, bytes: done, missing: plan.missing.clone(), seconds: started.elapsed().as_secs_f64() })
}

/// A path on one line, cut short on the left if it's long (hover for all of it).
fn path_label(ui: &mut egui::Ui, p: &Path) {
    let full = p.display().to_string();
    let max = 52;
    let n = full.chars().count();
    let shown = if n > max { format!("…{}", full.chars().skip(n - max + 1).collect::<String>()) } else { full.clone() };
    ui.monospace(shown).on_hover_text(full);
}

fn size_text(b: u64) -> String {
    let b = b as f64;
    if b >= 1e9 {
        format!("{:.2} GB", b / 1e9)
    } else if b >= 1e6 {
        format!("{:.1} MB", b / 1e6)
    } else {
        format!("{:.0} KB", (b / 1e3).max(1.0))
    }
}

impl EditorState {
    pub fn open_collect(&mut self) {
        self.commit_live_edit();
        let c = &mut self.collect;
        c.open = true;
        if c.job.is_none() {
            c.result = None;
        }
        if c.name.is_empty() {
            c.name = self.project_path.as_ref().and_then(|p| p.file_stem()).map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "project".into());
        }
    }

    fn start_collect(&mut self, plan: CollectPlan) {
        let (tx, rx) = unbounded();
        let cancel = Arc::new(AtomicBool::new(false));
        let stop = cancel.clone();
        let (total, count) = (plan.total_bytes, plan.items.len());
        std::thread::Builder::new()
            .name("collect".into())
            .spawn(move || {
                let mut last = Instant::now();
                let r = run_collect(&plan, &stop, |bytes, file, index| {
                    // At most ~20 updates a second.
                    if last.elapsed().as_millis() >= 50 {
                        last = Instant::now();
                        let _ = tx.send(Msg::Progress { bytes, file: file.to_string(), index });
                    }
                });
                let _ = tx.send(Msg::Done(r));
            })
            .expect("collect thread");
        self.collect.job = Some(Job { rx, cancel, bytes: 0, total, file: String::new(), index: 0, count });
        self.collect.result = None;
        self.status = "Collecting the project…".into();
    }

    /// Pick up progress from a running collect.
    pub fn poll_collect(&mut self) {
        let Some(job) = &mut self.collect.job else { return };
        let mut done = None;
        while let Ok(m) = job.rx.try_recv() {
            match m {
                Msg::Progress { bytes, file, index } => {
                    job.bytes = bytes;
                    job.file = file;
                    job.index = index;
                }
                Msg::Done(r) => done = Some(r),
            }
        }
        if let Some(r) = done {
            self.collect.job = None;
            self.status = match &r {
                Ok(rep) => format!("Collected the project into {}", rep.project_file.parent().unwrap_or(Path::new("")).display()),
                Err(e) => format!("Collect project: {e}"),
            };
            self.collect.result = Some(r);
            self.collect.open = true;
        }
    }
}

pub fn window(ctx: &egui::Context, st: &mut EditorState) {
    if !st.collect.open {
        return;
    }
    let mut open = true;
    let mut start = None;
    let mut open_result = None;
    let mut close = false;
    egui::Window::new("Collect project").open(&mut open).collapsible(false).resizable(false).default_width(460.0).max_width(520.0).show(ctx, |ui| {
        ui.set_max_width(460.0);
        // ---- Running ----
        if let Some(job) = &st.collect.job {
            let frac = if job.total > 0 { job.bytes as f32 / job.total as f32 } else { 1.0 };
            ui.label(format!("Copying {} ({} of {})", job.file, (job.index + 1).min(job.count), job.count));
            ui.add(egui::ProgressBar::new(frac.clamp(0.0, 1.0)).show_percentage());
            ui.weak(format!("{} of {}", size_text(job.bytes), size_text(job.total)));
            if ui.button("Stop").clicked() {
                job.cancel.store(true, Ordering::Relaxed);
            }
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
            return;
        }
        // ---- Finished ----
        if let Some(r) = &st.collect.result {
            match r {
                Ok(rep) => {
                    let folder = rep.project_file.parent().unwrap_or(Path::new("")).to_path_buf();
                    ui.label(RichText::new("Done").strong());
                    let n = rep.copied + rep.reused;
                    ui.label(format!("{n} file{} ({}) collected in {:.1} s, into:", if n == 1 { "" } else { "s" }, size_text(rep.bytes), rep.seconds));
                    ui.horizontal(|ui| {
                        if ui.small_button("Copy path").clicked() {
                            ui.ctx().copy_text(folder.display().to_string());
                        }
                        path_label(ui, &folder);
                    });
                    if rep.reused > 0 {
                        ui.weak(format!("{} of them were already there from an earlier collect, so they weren't copied again.", rep.reused));
                    }
                    ui.weak(format!("Project: {}", rep.project_file.file_name().unwrap_or_default().to_string_lossy()));
                    if !rep.missing.is_empty() {
                        ui.add_space(4.0);
                        ui.colored_label(Color32::from_rgb(255, 170, 80), format!("{} file{} couldn't be found and weren't collected:", rep.missing.len(), if rep.missing.len() == 1 { "" } else { "s" }));
                        for m in rep.missing.iter().take(6) {
                            ui.weak(m.display().to_string());
                        }
                    }
                    ui.add_space(4.0);
                    ui.weak("Copy or move the whole folder; open the .rfproj inside it on any computer and everything is found.");
                    ui.separator();
                    ui.horizontal(|ui| {
                        if ui.button("Open the collected project").clicked() {
                            open_result = Some(rep.project_file.clone());
                        }
                        if ui.button("Close").clicked() {
                            close = true;
                        }
                    });
                }
                Err(e) => {
                    ui.colored_label(Color32::from_rgb(255, 120, 100), format!("Collect didn't finish: {e}"));
                    ui.horizontal(|ui| {
                        if ui.button("Back").clicked() {
                            st.collect.result = None;
                        }
                        if ui.button("Close").clicked() {
                            close = true;
                        }
                    });
                }
            }
            return;
        }
        // ---- Choosing ----
        ui.label("Copies this project and every file it uses (video, audio, images, LUTs and title fonts) into one folder, for a backup or to move it to another computer.");
        ui.add_space(6.0);
        let c = &mut st.collect;
        egui::Grid::new("collect_grid").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
            ui.label("Folder");
            ui.horizontal(|ui| {
                if ui.button("Choose…").clicked() {
                    if let Some(d) = rfd::FileDialog::new().set_title("Folder to collect the project into").pick_folder() {
                        c.dest = Some(d);
                    }
                }
                match &c.dest {
                    Some(d) => path_label(ui, d),
                    None => {
                        ui.weak("not chosen");
                    }
                };
            });
            ui.end_row();
            ui.label("Project name");
            ui.text_edit_singleline(&mut c.name);
            ui.end_row();
        });
        ui.checkbox(&mut c.opts.0.include_unused, "Include media that isn't used in any sequence").on_hover_text("Everything in the Media panel, used or not");
        ui.checkbox(&mut c.opts.0.include_fonts, "Include the fonts titles use").on_hover_text("So titles look the same on a computer that doesn't have those fonts installed");
        ui.add_space(6.0);
        let Some(dest) = c.dest.clone() else {
            ui.weak("Choose a folder. An empty one is best: the files go into Media, LUTs and Fonts subfolders next to the project file.");
            ui.separator();
            if ui.button("Cancel").clicked() {
                close = true;
            }
            return;
        };
        let name = safe_name(&c.name);
        let key = PlanKey { dest: dest.clone(), name: name.clone(), opts: c.opts.0, rev: st.rev };
        if c.plan.as_ref().is_none_or(|(k, _)| *k != key) {
            let plan = plan_collect(&st.project, &dest, &name, c.opts.0);
            c.plan = Some((key, plan));
        }
        let plan = &c.plan.as_ref().unwrap().1;
        ui.label(format!("{} file{}, {} in all", plan.items.len(), if plan.items.len() == 1 { "" } else { "s" }, size_text(plan.total_bytes)));
        let already = plan.items.iter().filter(|i| dest.join(&i.to).exists()).count();
        if already > 0 {
            ui.weak(format!("{already} already in the folder from an earlier collect: they won't be copied again."));
        }
        if !plan.missing.is_empty() {
            ui.colored_label(Color32::from_rgb(255, 170, 80), format!("{} file{} can't be found and will be left out:", plan.missing.len(), if plan.missing.len() == 1 { "" } else { "s" }));
            for m in plan.missing.iter().take(6) {
                ui.weak(m.display().to_string());
            }
            if plan.missing.len() > 6 {
                ui.weak(format!("…and {} more", plan.missing.len() - 6));
            }
        }
        if plan.project_file.exists() {
            ui.colored_label(Color32::from_rgb(255, 170, 80), format!("{} is already there and will be replaced.", plan.project_file.file_name().unwrap_or_default().to_string_lossy()));
        }
        if st.project_path.is_none() || st.rev != st.saved_rev {
            ui.weak("Unsaved changes are included.");
        }
        ui.separator();
        ui.horizontal(|ui| {
            if ui.button(RichText::new("Collect").strong()).clicked() {
                start = Some(plan.clone());
            }
            if ui.button("Cancel").clicked() {
                close = true;
            }
        });
    });
    if let Some(plan) = start {
        st.start_collect(plan);
    }
    if let Some(p) = open_result {
        st.collect.open = false;
        st.request(Pending::OpenPath(p));
        return;
    }
    if close || !open {
        // Closing while it runs keeps it running; the status bar says when it's done.
        st.collect.open = false;
        if st.collect.job.is_none() {
            st.collect.result = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The name field takes typed text (driven through egui directly, with
    /// focus requested and Text events, as a keyboard would send them).
    #[test]
    fn the_project_name_field_takes_typing() {
        let ctx = egui::Context::default();
        let mut name = String::from("ui");
        let id = egui::Id::new("name_field");
        let frame = |ctx: &egui::Context, name: &mut String, events: Vec<egui::Event>| {
            let input = egui::RawInput { events, ..Default::default() };
            let mut out = ctx.run_ui(input, |ui| {
                let r = ui.add(egui::TextEdit::singleline(name).id(id));
                if !r.has_focus() {
                    r.request_focus();
                }
            });
            out.textures_delta.clear();
        };
        frame(&ctx, &mut name, vec![]);
        frame(&ctx, &mut name, vec![]);
        // Select all, then type over it.
        let key = |k| egui::Event::Key { key: k, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::COMMAND };
        frame(&ctx, &mut name, vec![key(egui::Key::A), egui::Event::Text("My Film".into())]);
        assert_eq!(name, "My Film");
        assert_eq!(safe_name(&name), "My Film");
    }
    use reelforge_core::{Clip, ClipId, MediaAsset, MediaId, MediaInfo, Project, Sequence, Time};

    #[test]
    fn collects_copies_and_the_moved_folder_opens() {
        let root = std::env::temp_dir().join(format!("rf_collect_app_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (src, dest, moved) = (root.join("src"), root.join("out"), root.join("moved"));
        std::fs::create_dir_all(&src).unwrap();
        let data: Vec<u8> = (0..9_000_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(src.join("big.mov"), &data).unwrap();
        let mut p = Project::new();
        let info = MediaInfo { duration: Time::ZERO, video: None, audio: None, container: String::new() };
        p.media = vec![MediaAsset { id: MediaId(1), path: src.join("big.mov"), name: "big.mov".into(), info }];
        let mut seq = Sequence::new("S", 640, 360, reelforge_core::FrameRate::new(25, 1));
        seq.tracks[0].clips.push(Clip::new(ClipId(5), MediaId(1), Time::ZERO, Time::ZERO, Time::from_secs_f64(1.0), None));
        p.sequences = vec![seq];

        let plan = plan_collect(&p, &dest, &safe_name("My: film?"), CollectOptions::default());
        assert_eq!(plan.project_file, dest.join("My_ film_.rfproj"));
        let mut calls = 0;
        let rep = run_collect(&plan, &AtomicBool::new(false), |_, _, _| calls += 1).unwrap();
        assert_eq!((rep.copied, rep.reused, rep.bytes), (1, 0, 9_000_000));
        assert!(calls >= 3, "progress during the copy");
        assert_eq!(std::fs::read(dest.join("Media/big.mov")).unwrap(), data);
        assert!(!dest.join("Media/big.mov.part").exists());
        // Collecting again reuses the copy.
        let rep = run_collect(&plan_collect(&p, &dest, "My_ film_", CollectOptions::default()), &AtomicBool::new(false), |_, _, _| {}).unwrap();
        assert_eq!((rep.copied, rep.reused), (0, 1));
        // Move the whole folder, open the project: the media is found there.
        std::fs::rename(&dest, &moved).unwrap();
        let q = crate::app::load_project(&moved.join("My_ film_.rfproj")).unwrap();
        assert_eq!(q.media[0].path, moved.join("Media").join("big.mov"));
        assert!(q.media[0].path.exists());
        // A stopped copy leaves no half-written file.
        let stop = AtomicBool::new(true);
        let err = copy_file(&src.join("big.mov"), &root.join("x/big.mov"), &stop, |_| {}).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::Interrupted);
        assert!(!root.join("x/big.mov").exists() && !root.join("x/big.mov.part").exists());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
