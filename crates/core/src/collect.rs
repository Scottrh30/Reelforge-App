//! Collect project: gather a project and every file it uses (media, LUTs,
//! title fonts) into one folder, so it can be backed up or moved to another
//! computer.
//!
//! The collected project refers to its files by paths relative to the project
//! file (`Media/clip.mp4`), so the folder can live anywhere. Projects are
//! always held in memory with absolute paths: [`Project::paths_relative_to`]
//! makes them relative for saving, and [`Project::resolve_paths`] turns them
//! back when opening. Relative paths are written with `/`, which Windows
//! accepts too, so a project collected on one system opens on another.

use crate::effects::EffectData;
use crate::project::{MediaId, Project};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// What a file is used for (and so which subfolder it's collected into).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FileKind {
    Media,
    Lut,
    Font,
}

impl FileKind {
    pub fn folder(self) -> &'static str {
        match self {
            FileKind::Media => "Media",
            FileKind::Lut => "LUTs",
            FileKind::Font => "Fonts",
        }
    }
}

impl Project {
    /// Visit every file path the project refers to: media, LUT files and title fonts.
    pub fn for_each_path_mut(&mut self, mut f: impl FnMut(FileKind, &mut PathBuf)) {
        for m in &mut self.media {
            f(FileKind::Media, &mut m.path);
        }
        for s in &mut self.sequences {
            for t in &mut s.tracks {
                for c in &mut t.clips {
                    for e in &mut c.effects {
                        if let Some(EffectData::Lut(p)) = &mut e.data {
                            f(FileKind::Lut, p);
                        }
                    }
                    if let Some(p) = c.title.as_mut().and_then(|t| t.font.path.as_mut()) {
                        f(FileKind::Font, p);
                    }
                }
            }
        }
    }

    /// Every file path the project refers to, with what it's for.
    pub fn file_paths(&self) -> Vec<(FileKind, PathBuf)> {
        let mut v = Vec::new();
        self.clone().for_each_path_mut(|k, p| v.push((k, p.clone())));
        v
    }

    /// A copy for saving in folder `base`: paths to files inside `base` (or its
    /// subfolders) become relative to it, written with `/`; others stay as they are.
    pub fn paths_relative_to(&self, base: &Path) -> Project {
        let mut p = self.clone();
        // Only against a real folder: an empty or relative base would match nothing sensibly.
        if base.as_os_str().is_empty() || !base.is_absolute() {
            return p;
        }
        p.for_each_path_mut(|_, path| {
            if let Ok(rel) = path.strip_prefix(base) {
                let parts: Vec<String> = rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
                if !parts.is_empty() {
                    *path = PathBuf::from(parts.join("/"));
                }
            }
        });
        p
    }

    /// After loading from folder `base`: relative paths (either separator) become absolute.
    pub fn resolve_paths(&mut self, base: &Path) {
        self.for_each_path_mut(|_, path| {
            if path.as_os_str().is_empty() || is_absolute_anywhere(path) {
                return;
            }
            let s = path.to_string_lossy().into_owned();
            let mut full = base.to_path_buf();
            for part in s.split(['/', '\\']).filter(|x| !x.is_empty() && *x != ".") {
                full.push(part);
            }
            *path = full;
        });
    }

    /// Media that no clip in any sequence (or composite shot) uses.
    pub fn unused_media(&self) -> Vec<MediaId> {
        let used: HashSet<MediaId> = self.sequences.iter().flat_map(|s| s.tracks.iter()).flat_map(|t| t.clips.iter()).map(|c| c.media).collect();
        self.media.iter().filter(|m| !used.contains(&m.id)).map(|m| m.id).collect()
    }
}

/// Absolute on this system, or a Windows drive/UNC path seen on another system.
fn is_absolute_anywhere(p: &Path) -> bool {
    if p.is_absolute() {
        return true;
    }
    let s = p.to_string_lossy();
    let b = s.as_bytes();
    (b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic()) || s.starts_with("\\\\") || s.starts_with('/')
}

/// What to gather.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CollectOptions {
    /// Also media that's imported but not used in any sequence.
    pub include_unused: bool,
    /// Copy the font files titles use (otherwise a title falls back to the
    /// built-in font on a computer without them).
    pub include_fonts: bool,
}

impl Default for CollectOptions {
    fn default() -> Self {
        Self { include_unused: true, include_fonts: true }
    }
}

/// One file to copy.
#[derive(Clone, Debug, PartialEq)]
pub struct CollectItem {
    pub kind: FileKind,
    pub from: PathBuf,
    /// Where it goes, relative to the collect folder (e.g. `Media/clip.mp4`).
    pub to: PathBuf,
    pub size: u64,
}

/// Everything a collect will do.
#[derive(Clone, Debug)]
pub struct CollectPlan {
    pub dest: PathBuf,
    /// The project file to write.
    pub project_file: PathBuf,
    pub items: Vec<CollectItem>,
    pub total_bytes: u64,
    /// Files the project uses that can't be found (left pointing where they were).
    pub missing: Vec<PathBuf>,
    /// The project as it will be written: absolute paths into `dest` (made
    /// relative when saved with [`Project::paths_relative_to`]).
    pub project: Project,
}

/// Can `existing` (already in the folder) stand in for `source`? Same size and
/// modification time: a previous collect of the same file.
pub fn same_file(source: &Path, existing: &Path) -> bool {
    match (std::fs::metadata(source), std::fs::metadata(existing)) {
        (Ok(a), Ok(b)) => a.len() == b.len() && a.modified().ok().zip(b.modified().ok()).is_some_and(|(x, y)| x.duration_since(y).unwrap_or_else(|e| e.duration()).as_secs() < 2),
        _ => false,
    }
}

/// Work out what collecting `p` into folder `dest` (as `name`.rfproj) takes:
/// which files go where (no two files sharing a name, and nothing already in
/// the folder overwritten unless it's the same file), and the rewritten project.
pub fn plan_collect(p: &Project, dest: &Path, name: &str, opts: CollectOptions) -> CollectPlan {
    let mut project = p.clone();
    if !opts.include_unused {
        let unused = p.unused_media();
        project.media.retain(|m| !unused.contains(&m.id));
    }
    let mut items: Vec<CollectItem> = Vec::new();
    let mut missing: Vec<PathBuf> = Vec::new();
    // Source file -> where it goes (the same file used twice is copied once).
    let mut placed: HashMap<(FileKind, PathBuf), PathBuf> = HashMap::new();
    // Names taken in each folder (lower case: Windows doesn't tell case apart).
    let mut taken: HashSet<String> = HashSet::new();
    project.for_each_path_mut(|kind, path| {
        if kind == FileKind::Font && !opts.include_fonts {
            return;
        }
        let key = (kind, path.clone());
        if let Some(to) = placed.get(&key) {
            *path = dest.join(to);
            return;
        }
        let Some(meta) = std::fs::metadata(&*path).ok().filter(|m| m.is_file()) else {
            if !missing.contains(path) {
                missing.push(path.clone());
            }
            return;
        };
        let file = path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
        let (stem, ext) = match file.rsplit_once('.') {
            Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
            _ => (file.clone(), String::new()),
        };
        // The first free name: "clip.mp4", "clip (2).mp4", …
        let mut n = 1;
        let to = loop {
            let candidate = if n == 1 { file.clone() } else { format!("{stem} ({n}){ext}") };
            let rel = PathBuf::from(kind.folder()).join(&candidate);
            let lower = rel.to_string_lossy().to_lowercase();
            let on_disk = dest.join(&rel);
            if !taken.contains(&lower) && (!on_disk.exists() || same_file(path, &on_disk)) {
                taken.insert(lower);
                break rel;
            }
            n += 1;
        };
        items.push(CollectItem { kind, from: path.clone(), to: to.clone(), size: meta.len() });
        placed.insert(key, to.clone());
        *path = dest.join(&to);
    });
    let total_bytes = items.iter().map(|i| i.size).sum();
    let file = format!("{}.rfproj", if name.trim().is_empty() { "project" } else { name.trim() });
    CollectPlan { dest: dest.to_path_buf(), project_file: dest.join(file), items, total_bytes, missing, project }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::{Effect, EffectKind};
    use crate::project::{Clip, ClipId, MediaAsset, MediaInfo, Sequence};
    use crate::time::{FrameRate, Time};

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("rf_collect_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn write(p: &Path, bytes: usize) {
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, vec![7u8; bytes]).unwrap();
    }

    fn asset(id: u64, path: &Path) -> MediaAsset {
        MediaAsset { id: MediaId(id), path: path.to_owned(), name: path.file_name().unwrap().to_string_lossy().into(), info: MediaInfo { duration: Time::ZERO, video: None, audio: None, container: String::new() } }
    }

    /// Two files both called clip.mp4 in different folders, an unused one, a LUT,
    /// a font, and a missing file.
    fn project(src: &Path) -> Project {
        write(&src.join("a/clip.mp4"), 1000);
        write(&src.join("b/clip.mp4"), 2000);
        write(&src.join("unused.mov"), 300);
        write(&src.join("look.cube"), 50);
        write(&src.join("fonts/Bold.ttf"), 70);
        let mut p = Project::new();
        p.media = vec![asset(1, &src.join("a/clip.mp4")), asset(2, &src.join("b/clip.mp4")), asset(3, &src.join("unused.mov")), asset(4, &src.join("gone.wav"))];
        let s = Time::from_secs_f64;
        let mut seq = Sequence::new("S", 640, 360, FrameRate::new(25, 1));
        let mut c1 = Clip::new(ClipId(10), MediaId(1), s(0.0), s(0.0), s(1.0), None);
        let mut lut = Effect::new(EffectKind::Lut);
        lut.data = Some(EffectData::Lut(src.join("look.cube")));
        c1.effects.push(lut);
        let c2 = Clip::new(ClipId(11), MediaId(2), s(1.0), s(0.0), s(1.0), None);
        let mut t = Clip::new(ClipId(12), MediaId::GENERATED, s(0.0), s(0.0), s(1.0), None);
        t.title = Some(crate::title::TitleSpec { font: crate::title::FontChoice { path: Some(src.join("fonts/Bold.ttf")), index: 0, name: "Bold".into() }, ..Default::default() });
        let a = Clip::new(ClipId(13), MediaId(4), s(0.0), s(0.0), s(1.0), None);
        seq.tracks[0].clips = vec![c1, c2];
        seq.tracks[1].clips = vec![t];
        seq.tracks[2].clips = vec![a];
        p.sequences = vec![seq];
        p
    }

    #[test]
    fn plans_every_file_without_name_clashes() {
        let src = tmp("src");
        let dest = tmp("dest");
        let p = project(&src);
        let plan = plan_collect(&p, &dest, "My film", CollectOptions::default());
        let tos: Vec<String> = plan.items.iter().map(|i| i.to.to_string_lossy().replace('\\', "/")).collect();
        assert_eq!(tos, ["Media/clip.mp4", "Media/clip (2).mp4", "Media/unused.mov", "LUTs/look.cube", "Fonts/Bold.ttf"]);
        assert_eq!(plan.total_bytes, 1000 + 2000 + 300 + 50 + 70);
        assert_eq!(plan.missing, vec![src.join("gone.wav")]);
        assert_eq!(plan.project_file, dest.join("My film.rfproj"));
        // The project now points into the folder (and at the missing file where it was).
        let paths: Vec<PathBuf> = plan.project.file_paths().into_iter().map(|(_, p)| p).collect();
        assert!(paths.contains(&dest.join("Media").join("clip (2).mp4")));
        assert!(paths.contains(&src.join("gone.wav")));
        // Leaving out unused media and fonts.
        let lean = plan_collect(&p, &dest, "x", CollectOptions { include_unused: false, include_fonts: false });
        assert_eq!(lean.items.len(), 3);
        assert!(lean.project.media.iter().all(|m| m.id != MediaId(3)));
        assert_eq!(lean.project.file_paths().iter().find(|(k, _)| *k == FileKind::Font).unwrap().1, src.join("fonts/Bold.ttf"));
    }

    #[test]
    fn existing_files_are_reused_only_when_they_are_the_same() {
        let src = tmp("src2");
        let dest = tmp("dest2");
        let p = project(&src);
        // A different file already called clip.mp4 in the folder: not overwritten.
        write(&dest.join("Media/clip.mp4"), 5);
        let plan = plan_collect(&p, &dest, "p", CollectOptions::default());
        assert_eq!(plan.items[0].to, Path::new("Media").join("clip (2).mp4"));
        // The same file from an earlier collect: reused under its name.
        std::fs::copy(src.join("a/clip.mp4"), dest.join("Media/clip.mp4")).unwrap();
        let f = std::fs::File::options().write(true).open(dest.join("Media/clip.mp4")).unwrap();
        f.set_modified(std::fs::metadata(src.join("a/clip.mp4")).unwrap().modified().unwrap()).unwrap();
        let plan = plan_collect(&p, &dest, "p", CollectOptions::default());
        assert_eq!(plan.items[0].to, Path::new("Media").join("clip.mp4"));
    }

    #[test]
    fn relative_paths_round_trip_and_survive_a_move() {
        let src = tmp("src3");
        let dest = tmp("dest3");
        let p = project(&src);
        let plan = plan_collect(&p, &dest, "p", CollectOptions::default());
        let saved = plan.project.paths_relative_to(&dest);
        let json = serde_json::to_string(&saved).unwrap();
        assert!(json.contains("\"Media/clip (2).mp4\"") && json.contains("\"LUTs/look.cube\"") && json.contains("\"Fonts/Bold.ttf\""), "{json}");
        // Opened from somewhere else entirely (the folder was moved).
        let moved = Path::new("/somewhere/else");
        let mut back: Project = serde_json::from_str(&json).unwrap();
        back.resolve_paths(moved);
        let paths: Vec<PathBuf> = back.file_paths().into_iter().map(|(_, p)| p).collect();
        assert!(paths.contains(&moved.join("Media").join("clip (2).mp4")));
        assert!(paths.contains(&moved.join("LUTs").join("look.cube")));
        // The missing file kept its absolute path.
        assert!(paths.contains(&src.join("gone.wav")));
        // Windows-style relative and absolute paths written elsewhere.
        let mut q = Project::new();
        q.media = vec![asset(1, Path::new("Media\\x.mp4")), asset(2, Path::new("D:\\Footage\\y.mp4"))];
        q.resolve_paths(moved);
        assert_eq!(q.media[0].path, moved.join("Media").join("x.mp4"));
        assert_eq!(q.media[1].path, PathBuf::from("D:\\Footage\\y.mp4"));
        assert_eq!(p.unused_media(), vec![MediaId(3)]);
        // Without a proper folder nothing is changed.
        assert_eq!(p.paths_relative_to(Path::new("")).file_paths(), p.file_paths());
    }
}
