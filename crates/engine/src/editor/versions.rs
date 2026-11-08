//! Project versions: named snapshots of the whole project kept beside it
//! (in a "<project> versions" folder), to go back to or open as a copy.
//! **Save version…** keeps one with a name and a note; every Save also keeps
//! one by itself (when the project changed since the last), the newest
//! [`AUTO_KEEP`] of those. Restoring is one undo step, and what was there
//! before is kept as a version first.

use super::{load_project, Editor};
use reelforge_core::undo::Command;
use reelforge_core::Project;
use serde::{Deserialize, Serialize};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

/// How many versions saved by themselves (on Save) are kept.
pub const AUTO_KEEP: usize = 30;
const INDEX: &str = "versions.json";

/// One kept version.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Version {
    /// Its file in the versions folder.
    pub file: String,
    pub name: String,
    #[serde(default)]
    pub note: String,
    /// When it was kept (seconds since 1970).
    pub saved: u64,
    /// Kept by itself on Save (not by Save version…).
    #[serde(default)]
    pub auto: bool,
    /// Its first sequence's size, rate and length, its sequences, clips and media.
    #[serde(default)]
    pub summary: String,
    /// Of the saved file, to skip keeping the same project twice in a row.
    #[serde(default)]
    pub hash: u64,
}

#[derive(Default, Serialize, Deserialize)]
struct Index {
    versions: Vec<Version>,
}

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// The folder versions of the project saved at `project` are kept in.
pub fn folder_for(project: &Path) -> PathBuf {
    let stem = project.file_stem().map_or("project".into(), |s| s.to_string_lossy().into_owned());
    project.with_file_name(format!("{stem} versions"))
}

fn read_index(dir: &Path) -> Index {
    std::fs::read_to_string(dir.join(INDEX)).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

fn write_index(dir: &Path, index: &Index) -> Result<(), String> {
    let json = serde_json::to_string_pretty(index).map_err(|e| e.to_string())?;
    crate::safety::write_atomic(&dir.join(INDEX), json.as_bytes()).map_err(|e| format!("couldn't write the versions list: {e}"))
}

/// A file name from a version's name: letters, digits, spaces and a few marks.
fn file_safe(name: &str) -> String {
    let s: String = name.chars().map(|c| if c.is_alphanumeric() || " -_.()".contains(c) { c } else { '-' }).collect();
    let s = s.trim().trim_matches('.').to_owned();
    if s.is_empty() { "version".into() } else { s.chars().take(60).collect() }
}

fn describe(p: &Project) -> String {
    let clips: usize = p.sequences.iter().flat_map(|s| &s.tracks).map(|t| t.clips.len()).sum();
    let first = p.sequences.first().map(crate::start::summary_of).unwrap_or_default();
    let seqs = p.sequences.len();
    format!("{first} · {seqs} sequence{} · {clips} clip{} · {} media", if seqs == 1 { "" } else { "s" }, if clips == 1 { "" } else { "s" }, p.media.len())
}

/// The whole project swapped for another (restoring a version), undoably.
struct ReplaceAll {
    other: Project,
    name: String,
}

impl Command for ReplaceAll {
    fn name(&self) -> &str {
        &self.name
    }
    fn apply(&mut self, p: &mut Project) {
        std::mem::swap(p, &mut self.other);
    }
    fn undo(&mut self, p: &mut Project) {
        std::mem::swap(p, &mut self.other);
    }
}

impl Editor {
    /// Where this project's versions are kept (`None` until it's saved).
    pub fn versions_folder(&self) -> Option<PathBuf> {
        self.project_path.as_deref().map(folder_for)
    }

    /// The versions kept, newest first (those whose file is still there).
    pub fn versions(&self) -> Vec<Version> {
        let Some(dir) = self.versions_folder() else { return Vec::new() };
        // Newest first; within a second, the one kept later first.
        let mut v: Vec<(usize, Version)> = read_index(&dir).versions.into_iter().enumerate().filter(|(_, v)| dir.join(&v.file).is_file()).collect();
        v.sort_by(|(i, a), (j, b)| b.saved.cmp(&a.saved).then(j.cmp(i)));
        v.into_iter().map(|(_, v)| v).collect()
    }

    /// Keep the project as it is now as a version called `name`.
    pub fn save_version(&mut self, name: &str, note: &str) -> Result<Version, String> {
        self.commit_live_edit();
        if self.project_path.is_none() {
            return Err("Save the project first: its versions are kept beside it".into());
        }
        let name = if name.trim().is_empty() { format!("Version {}", self.versions().iter().filter(|v| !v.auto).count() + 1) } else { name.trim().to_owned() };
        let v = self.write_version(&name, note.trim(), false, true)?.expect("always written");
        self.status = format!("Kept the version \"{}\"", v.name);
        Ok(v)
    }

    /// Write the project into the versions folder (`auto`: on Save, only if
    /// it changed since the newest version, `force` aside). Returns the
    /// version, or `None` when nothing needed keeping.
    fn write_version(&mut self, name: &str, note: &str, auto: bool, force: bool) -> Result<Option<Version>, String> {
        let dir = self.versions_folder().ok_or("The project isn't saved yet")?;
        std::fs::create_dir_all(&dir).map_err(|e| format!("couldn't make {}: {e}", dir.display()))?;
        // Media paths relative to the versions folder, so a moved project folder keeps working.
        let json = serde_json::to_string_pretty(&self.project.paths_relative_to(&dir)).map_err(|e| e.to_string())?;
        let mut h = std::collections::hash_map::DefaultHasher::new();
        // Without the media paths' spelling: the same project kept from another folder is the same.
        serde_json::to_string(&self.project).unwrap_or_default().hash(&mut h);
        let hash = h.finish();
        let mut index = read_index(&dir);
        let newest = index.versions.iter().filter(|v| dir.join(&v.file).is_file()).max_by_key(|v| v.saved);
        if !force && newest.is_some_and(|v| v.hash == hash) {
            return Ok(None);
        }
        let saved = now();
        let stamp = chrono_like(saved);
        let base = format!("{stamp} {}", file_safe(name));
        let file = (1..).map(|n| if n == 1 { format!("{base}.rfproj") } else { format!("{base} {n}.rfproj") }).find(|f| !dir.join(f).exists()).unwrap();
        crate::safety::write_atomic(&dir.join(&file), json.as_bytes()).map_err(|e| format!("couldn't keep the version: {e}"))?;
        let v = Version { file, name: name.to_owned(), note: note.to_owned(), saved, auto, summary: describe(&self.project), hash };
        index.versions.push(v.clone());
        // Only the newest automatic ones are kept.
        let mut autos: Vec<(u64, String)> = index.versions.iter().filter(|v| v.auto).map(|v| (v.saved, v.file.clone())).collect();
        autos.sort();
        if autos.len() > AUTO_KEEP {
            for (_, f) in &autos[..autos.len() - AUTO_KEEP] {
                let _ = std::fs::remove_file(dir.join(f));
                index.versions.retain(|v| &v.file != f);
            }
        }
        write_index(&dir, &index)?;
        Ok(Some(v))
    }

    /// After Save: keep the saved project as a version if it changed.
    pub(crate) fn auto_version(&mut self) {
        if !self.prefs.versions_on_save {
            return;
        }
        if let Err(e) = self.write_version("Saved", "", true, false) {
            self.status = format!("{} (but {e})", self.status);
        }
    }

    /// The project in version `file`, read from its folder.
    fn load_version(&self, file: &str) -> Result<Project, String> {
        let dir = self.versions_folder().ok_or("The project isn't saved yet")?;
        let path = dir.join(file);
        if !path.starts_with(&dir) || !path.is_file() {
            return Err("That version isn't there any more".into());
        }
        load_project(&path).map_err(|e| format!("couldn't read the version: {e:#}"))
    }

    /// Bring version `file` back in place of the project (one undo step).
    /// What was there is kept as a version first. The project file isn't
    /// written until you save.
    pub fn restore_version(&mut self, file: &str) -> Result<(), String> {
        self.commit_live_edit();
        let restored = self.load_version(file)?;
        let name = self.versions().into_iter().find(|v| v.file == file).map_or_else(|| file.to_owned(), |v| v.name);
        self.write_version("Before restoring", &format!("The project just before \"{name}\" was restored"), true, false)?;
        self.select_only(None);
        self.execute(Box::new(ReplaceAll { other: restored, name: format!("Restore version \"{name}\"") }));
        self.validate_selection();
        self.after_load();
        if !self.status.contains("can't be found") {
            self.status = format!("Restored \"{name}\" (undo to go back; save to keep it)");
        }
        Ok(())
    }

    /// Open version `file` as a new, unsaved project (a copy to look at or
    /// take parts from); the project itself stays as it is on disk.
    pub fn open_version_copy(&mut self, file: &str) -> Result<(), String> {
        let p = self.load_version(file)?;
        let name = self.versions().into_iter().find(|v| v.file == file).map_or_else(|| file.to_owned(), |v| v.name);
        self.replace_project(p, None);
        self.saved_rev = self.rev.wrapping_sub(1);
        self.after_load();
        self.status = format!("Opened the version \"{name}\" as a new project: save it to keep it");
        Ok(())
    }

    /// Rename version `file` and change its note.
    pub fn rename_version(&mut self, file: &str, name: &str, note: &str) -> Result<(), String> {
        let dir = self.versions_folder().ok_or("The project isn't saved yet")?;
        let mut index = read_index(&dir);
        let v = index.versions.iter_mut().find(|v| v.file == file).ok_or("That version isn't there any more")?;
        if !name.trim().is_empty() {
            v.name = name.trim().to_owned();
        }
        v.note = note.trim().to_owned();
        // A named version is one you meant to keep.
        v.auto = false;
        write_index(&dir, &index)
    }

    /// Delete version `file`.
    pub fn delete_version(&mut self, file: &str) -> Result<(), String> {
        let dir = self.versions_folder().ok_or("The project isn't saved yet")?;
        let mut index = read_index(&dir);
        let before = index.versions.len();
        index.versions.retain(|v| v.file != file);
        if index.versions.len() == before || file.contains(['/', '\\']) {
            return Err("That version isn't there any more".into());
        }
        let _ = std::fs::remove_file(dir.join(file));
        write_index(&dir, &index)
    }
}

/// "2026-11-07 1830" (UTC) for a file name, sorting by time.
fn chrono_like(secs: u64) -> String {
    let days = (secs / 86400) as i64;
    let (h, m, s) = ((secs % 86400) / 3600, (secs % 3600) / 60, secs % 60);
    // Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(mo <= 2);
    format!("{y:04}-{mo:02}-{d:02} {h:02}{m:02}{s:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_for_file_names() {
        assert_eq!(chrono_like(0), "1970-01-01 000000");
        assert_eq!(chrono_like(1_794_009_600 + 3600 * 18 + 60 * 30 + 5), "2026-11-07 183005");
    }

    /// Versions kept by hand and on Save (not when nothing changed), listed
    /// newest first; restoring is undoable and keeps what was there; a
    /// version opens as an unsaved copy; renaming and deleting.
    #[test]
    fn versions_keep_restore_and_open() {
        let dir = std::env::temp_dir().join(format!("rf-versions-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut e = Editor::new(None, None);
        assert!(e.save_version("x", "").is_err(), "unsaved: it says to save first");
        let path = dir.join("film.rfproj");
        e.save_to(&path).unwrap();
        assert_eq!(e.versions_folder().unwrap(), dir.join("film versions"));
        assert_eq!(e.versions().len(), 1, "Save keeps one");
        e.save_to(&path).unwrap();
        assert_eq!(e.versions().len(), 1, "nothing changed: no new one");

        e.add_title();
        let first = e.save_version("Rough cut", "  for the client ").unwrap();
        assert_eq!((first.name.as_str(), first.note.as_str(), first.auto), ("Rough cut", "for the client", false));
        assert!(first.summary.contains("1 clip"), "{}", first.summary);
        e.add_title();
        e.save_to(&path).unwrap();
        let list = e.versions();
        assert_eq!(list.len(), 3);
        assert!(list[0].auto && list[1].name == "Rough cut");
        let clips = |e: &Editor| e.seq().tracks.iter().map(|t| t.clips.len()).sum::<usize>();
        assert_eq!(clips(&e), 2);

        e.restore_version(&first.file).unwrap();
        assert_eq!(clips(&e), 1, "back to the rough cut");
        assert!(e.is_dirty() && e.project_path.as_deref() == Some(path.as_path()));
        assert_eq!(e.versions().len(), 3, "the project before restoring was the newest version already");
        e.undo();
        assert_eq!(clips(&e), 2, "undo brings the later edit back");

        e.rename_version(&first.file, "Rough cut 1", "sent").unwrap();
        assert!(e.versions().iter().any(|v| v.name == "Rough cut 1" && v.note == "sent"));
        e.open_version_copy(&first.file).unwrap();
        assert!(e.project_path.is_none() && clips(&e) == 1);
        e.project_path = Some(path.clone());
        e.delete_version(&first.file).unwrap();
        assert_eq!(e.versions().len(), 2);
        assert!(e.delete_version("../film.rfproj").is_err() && path.is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
