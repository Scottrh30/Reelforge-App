//! Starting a project: built-in presets (sizes and frame rates for social,
//! web, broadcast and cinema), templates you save yourself (a whole project
//! to start from: its sequences, tracks, titles and media), and the list of
//! recent projects.

use reelforge_core::{FrameRate, Project, Sequence};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// A preset for a new project's sequence.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Preset {
    pub id: &'static str,
    /// "Social", "Web & TV" or "Cinema".
    pub group: &'static str,
    pub name: &'static str,
    /// Where it's for, in a few words.
    pub detail: &'static str,
    pub width: u32,
    pub height: u32,
    /// Frames per second, as a fraction.
    pub rate: (u32, u32),
}

impl Preset {
    pub fn frame_rate(&self) -> FrameRate {
        FrameRate::new(self.rate.0 as i64, self.rate.1 as i64)
    }

    /// e.g. "1080 × 1920 · 30 fps".
    pub fn summary(&self) -> String {
        format!("{} × {} · {} fps", self.width, self.height, rate_label(self.frame_rate()))
    }
}

/// "30", "29.97", "23.976"…
pub fn rate_label(r: FrameRate) -> String {
    let fps = r.fps();
    if (fps - fps.round()).abs() < 1e-6 {
        format!("{}", fps.round() as i64)
    } else {
        let s = format!("{fps:.3}");
        s.trim_end_matches('0').to_string()
    }
}

const fn p(id: &'static str, group: &'static str, name: &'static str, detail: &'static str, width: u32, height: u32, rate: (u32, u32)) -> Preset {
    Preset { id, group, name, detail, width, height, rate }
}

/// The built-in presets, in the order the start screen shows them.
pub const PRESETS: &[Preset] = &[
    p("vertical-30", "Social", "Vertical 9:16", "Reels, TikTok, Shorts, Stories", 1080, 1920, (30, 1)),
    p("vertical-60", "Social", "Vertical 9:16, 60 fps", "Smooth motion, gameplay", 1080, 1920, (60, 1)),
    p("portrait-30", "Social", "Portrait 4:5", "Instagram and Facebook feed", 1080, 1350, (30, 1)),
    p("square-30", "Social", "Square 1:1", "Feeds, ads, carousels", 1080, 1080, (30, 1)),
    p("hd-30", "Web & TV", "HD 1080p", "YouTube, the web", 1920, 1080, (30, 1)),
    p("hd-60", "Web & TV", "HD 1080p, 60 fps", "Sport, gameplay, screen recordings", 1920, 1080, (60, 1)),
    p("hd-25", "Web & TV", "HD 1080p, 25 fps", "PAL broadcast (Europe, Australia…)", 1920, 1080, (25, 1)),
    p("hd-2997", "Web & TV", "HD 1080p, 29.97 fps", "NTSC broadcast (Americas, Japan)", 1920, 1080, (30000, 1001)),
    p("uhd-30", "Web & TV", "4K UHD", "YouTube 4K, TVs", 3840, 2160, (30, 1)),
    p("uhd-60", "Web & TV", "4K UHD, 60 fps", "High-frame-rate 4K", 3840, 2160, (60, 1)),
    p("uhd-24", "Web & TV", "4K UHD, 24 fps", "Film look at 4K", 3840, 2160, (24, 1)),
    p("hd-720", "Web & TV", "HD 720p", "Light and quick", 1280, 720, (30, 1)),
    p("film-hd", "Cinema", "Film look 1080p", "24 fps, HD", 1920, 1080, (24, 1)),
    p("film-2398", "Cinema", "1080p 23.976", "Most cinema cameras' \"24p\"", 1920, 1080, (24000, 1001)),
    p("scope-uhd", "Cinema", "Scope 2.39:1, 4K", "Widescreen, UHD width", 3840, 1608, (24, 1)),
    p("flat-uhd", "Cinema", "Flat 1.85:1, 4K", "Theatrical flat, DCI height", 3996, 2160, (24, 1)),
    p("dci-2k", "Cinema", "DCI 2K", "Digital cinema, full container", 2048, 1080, (24, 1)),
    p("dci-4k", "Cinema", "DCI 4K", "Digital cinema, full container", 4096, 2160, (24, 1)),
    p("scope-2k", "Cinema", "DCI 2K Scope", "Digital cinema 2.39:1", 2048, 858, (24, 1)),
];

pub fn preset(id: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|p| p.id == id)
}

/// A new project with one sequence of this size and rate. Sizes are kept
/// even (video encoders want that) and between 16 and 16384.
pub fn project_with(width: u32, height: u32, rate: FrameRate) -> Project {
    let even = |v: u32| (v.clamp(16, 16384) + 1) & !1;
    let mut p = Project::new();
    p.sequences = vec![Sequence::new("Sequence 1", even(width), even(height), rate)];
    p
}

// ---- Recent projects -----------------------------------------------------------

/// Most projects remembered.
pub const RECENT_MAX: usize = 15;

/// The projects opened or saved lately, newest first (kept in a file).
#[derive(Debug, Default)]
pub struct Recent {
    file: Option<PathBuf>,
    pub list: Vec<RecentEntry>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RecentEntry {
    pub path: PathBuf,
    /// When it was last opened or saved (seconds since 1970).
    pub at: u64,
    /// Its main sequence when it was last saved: "1920 × 1080 · 30 fps".
    #[serde(default)]
    pub summary: String,
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

impl Recent {
    /// The list kept in `file` (empty if there's none yet).
    pub fn load(file: PathBuf) -> Self {
        let list = std::fs::read(&file).ok().and_then(|b| serde_json::from_slice::<Vec<RecentEntry>>(&b).ok()).unwrap_or_default();
        Recent { file: Some(file), list }
    }

    fn save(&self) {
        if let Some(f) = &self.file {
            if let Some(dir) = f.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            if let Ok(json) = serde_json::to_vec_pretty(&self.list) {
                let _ = crate::safety::write_atomic(f, &json);
            }
        }
    }

    /// `path` was opened or saved: to the top.
    pub fn add(&mut self, path: &Path, summary: String) {
        let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        self.list.retain(|e| e.path != path);
        self.list.insert(0, RecentEntry { path, at: now(), summary });
        self.list.truncate(RECENT_MAX);
        self.save();
    }

    pub fn remove(&mut self, path: &Path) {
        let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        self.list.retain(|e| e.path != path && e.path != canonical);
        self.save();
    }

    pub fn clear(&mut self) {
        self.list.clear();
        self.save();
    }

    /// Where a project's thumbnail is kept (next to the list).
    pub fn thumbnail(&self, project: &Path) -> Option<PathBuf> {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        project.hash(&mut h);
        Some(self.file.as_ref()?.parent()?.join("thumbnails").join(format!("{:016x}.jpg", h.finish())))
    }
}

// ---- Your templates ----------------------------------------------------------------

/// A template you saved: a project file in the templates folder.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct UserTemplate {
    pub name: String,
    pub path: PathBuf,
    /// Its main sequence: "1080 × 1920 · 30 fps".
    pub summary: String,
    /// How many sequences, tracks and media files it holds.
    pub sequences: usize,
    pub media: usize,
}

/// A sequence's size and rate, for lists.
pub fn summary_of(s: &Sequence) -> String {
    format!("{} × {} · {} fps", s.width, s.height, rate_label(s.frame_rate))
}

/// The templates in `dir`, by name.
pub fn user_templates(dir: &Path) -> Vec<UserTemplate> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<UserTemplate> = rd
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("rfproj")))
        .filter_map(|path| {
            let project = crate::editor::load_project(&path).ok()?;
            let main = project.sequences.first()?;
            Some(UserTemplate {
                name: path.file_stem()?.to_string_lossy().into_owned(),
                summary: summary_of(main),
                sequences: project.sequences.len(),
                media: project.media.len(),
                path,
            })
        })
        .collect();
    out.sort_by_key(|t| t.name.to_lowercase());
    out
}

/// A file name for a template called `name` (characters files can't have are dropped).
pub fn template_file(dir: &Path, name: &str) -> Option<PathBuf> {
    let clean: String = name.chars().filter(|c| !matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') && !c.is_control()).collect();
    let clean = clean.trim().trim_matches('.').to_string();
    (!clean.is_empty()).then(|| dir.join(format!("{clean}.rfproj")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presets_are_sound() {
        let mut ids = std::collections::HashSet::new();
        for p in PRESETS {
            assert!(ids.insert(p.id), "{} twice", p.id);
            assert!(p.width % 2 == 0 && p.height % 2 == 0, "{}: even sizes", p.id);
            assert!(["Social", "Web & TV", "Cinema"].contains(&p.group));
        }
        let v = preset("vertical-30").unwrap();
        assert_eq!((v.width, v.height, v.summary()), (1080, 1920, "1080 × 1920 · 30 fps".to_string()));
        assert_eq!(preset("hd-2997").unwrap().summary(), "1920 × 1080 · 29.97 fps");
        assert_eq!(preset("film-2398").unwrap().summary(), "1920 × 1080 · 23.976 fps");
        let scope = preset("scope-uhd").unwrap();
        assert!((scope.width as f64 / scope.height as f64 - 2.39).abs() < 0.01);
    }

    #[test]
    fn a_project_from_a_preset() {
        let p = project_with(3840, 2160, FrameRate::new(24, 1));
        assert_eq!(p.sequences.len(), 1);
        let s = &p.sequences[0];
        assert_eq!((s.width, s.height, s.frame_rate), (3840, 2160, FrameRate::new(24, 1)));
        assert!(!s.tracks.is_empty(), "the usual tracks");
        // Odd or silly sizes are made usable.
        let p = project_with(1081, 3, FrameRate::new(25, 1));
        assert_eq!((p.sequences[0].width, p.sequences[0].height), (1082, 16));
    }

    #[test]
    fn recent_projects_newest_first_without_repeats_and_kept() {
        let dir = std::env::temp_dir().join(format!("rf_recent_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("recent.json");
        let mut r = Recent::load(file.clone());
        assert!(r.list.is_empty());
        let a = dir.join("a.rfproj");
        let b = dir.join("b.rfproj");
        r.add(&a, "A".into());
        r.add(&b, "B".into());
        r.add(&a, "A2".into());
        assert_eq!(r.list.iter().map(|e| e.path.clone()).collect::<Vec<_>>(), vec![a.clone(), b.clone()]);
        assert_eq!(r.list[0].summary, "A2");
        for k in 0..30 {
            r.add(&dir.join(format!("{k}.rfproj")), String::new());
        }
        assert_eq!(r.list.len(), RECENT_MAX);
        // Kept between runs.
        let again = Recent::load(file.clone());
        assert_eq!(again.list, r.list);
        r.remove(&dir.join("29.rfproj"));
        assert_eq!(Recent::load(file.clone()).list.len(), RECENT_MAX - 1);
        assert!(r.thumbnail(&a).unwrap().starts_with(dir.join("thumbnails")));
        assert_ne!(r.thumbnail(&a), r.thumbnail(&b));
        r.clear();
        assert!(Recent::load(file).list.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn template_names_make_safe_file_names() {
        let d = Path::new("/t");
        assert_eq!(template_file(d, "My Vlog: 4K / 60"), Some(d.join("My Vlog 4K  60.rfproj")));
        assert_eq!(template_file(d, "  ..  "), None);
    }
}
