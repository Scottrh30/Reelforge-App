//! Opening Adobe projects: Premiere Pro (`.prproj`, see [`prproj`]) and
//! After Effects (`.aep`, see [`aep`]). Each becomes a Reelforge project:
//! its media (by path, found again beside the project if it moved), its
//! sequences or compositions (composition clips where one is used inside
//! another), their tracks or layers, every clip's timing and speed, its
//! transform and opacity (keyframes too), blend mode, volume, cross
//! dissolves, fades and markers. What has no counterpart here (most
//! effects, graphics, masks, cameras…) is left out and listed in
//! [`Imported::notes`], so nothing goes missing without a word.
//!
//! Everything read from the file is treated as untrusted: sizes, counts and
//! times are checked and kept in range, and a damaged file gives an error,
//! never a crash.

// Reads files from anywhere: no position in a list is taken on trust (`get`,
// checked slices), so a damaged or hostile file is an error, never a crash.
#![cfg_attr(not(test), deny(clippy::indexing_slicing))]

pub mod aep;
pub mod prproj;

use crate::project::Project;
use crate::time::{FrameRate, Time};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// An Adobe project read in.
#[derive(Debug)]
pub struct Imported {
    pub project: Project,
    /// What came across only partly or not at all, one line each, most
    /// common first ("3 clips: effect Lumetri Color left out").
    pub notes: Vec<String>,
    /// One line on what came across ("2 sequences, 14 clips, 6 media files").
    pub summary: String,
}

/// The most a project file may unpack to (a big Premiere project is tens of MB).
pub const MAX_UNPACKED: u64 = 1 << 30;

/// What kind of Adobe project a file is (by its first bytes, else its name).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Premiere,
    AfterEffects,
}

pub fn kind_of(path: &Path, head: &[u8]) -> Option<Kind> {
    if head.starts_with(b"RIFX") {
        return Some(Kind::AfterEffects);
    }
    let ext = path.extension().map(|e| e.to_string_lossy().to_ascii_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "prproj" => Some(Kind::Premiere),
        "aep" => Some(Kind::AfterEffects),
        _ if head.starts_with(&[0x1f, 0x8b]) || head.starts_with(b"<?xml") => Some(Kind::Premiere),
        _ => None,
    }
}

/// Is `path` an Adobe project this can open?
pub fn is_adobe_project(path: &Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case("prproj") || e.eq_ignore_ascii_case("aep"))
}

/// Read the Adobe project at `path`.
pub fn import(path: &Path) -> Result<Imported, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("Couldn't open {}: {e}", path.display()))?;
    if meta.len() > MAX_UNPACKED {
        return Err("The project file is too big to read".into());
    }
    let bytes = std::fs::read(path).map_err(|e| format!("Couldn't read {}: {e}", path.display()))?;
    match kind_of(path, &bytes) {
        Some(Kind::Premiere) => prproj::read(&bytes, path.parent()),
        Some(Kind::AfterEffects) => aep::read(&bytes, path.parent()),
        None => Err("That isn't a Premiere Pro or After Effects project".into()),
    }
}

/// Things left out, counted by kind.
#[derive(Default)]
pub(crate) struct Notes(BTreeMap<String, usize>);

impl Notes {
    pub fn add(&mut self, what: impl Into<String>) {
        *self.0.entry(what.into()).or_default() += 1;
    }
    pub fn lines(self) -> Vec<String> {
        let mut v: Vec<(String, usize)> = self.0.into_iter().collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        v.into_iter().map(|(what, n)| if n == 1 { what } else { format!("{what} (×{n})") }).collect()
    }
}

/// The frame rate nearest `fps` that Reelforge names exactly: NTSC rates
/// (23.976, 29.97, 59.94…) as n×1000/1001, whole rates as they are.
pub fn rate_of(fps: f64) -> FrameRate {
    if !fps.is_finite() || fps <= 0.5 || fps > 1000.0 {
        return FrameRate::new(30, 1);
    }
    for base in [24i64, 30, 48, 60, 120, 240] {
        if (fps - base as f64 * 1000.0 / 1001.0).abs() < 0.006 {
            return FrameRate::new(base * 1000, 1001);
        }
    }
    let r = fps.round();
    if (fps - r).abs() < 0.006 {
        return FrameRate::new(r as i64, 1);
    }
    FrameRate::new((fps * 1000.0).round() as i64, 1000)
}

/// Seconds to a time, kept to ±10 000 hours (and 0 for nonsense).
pub(crate) fn secs(s: f64) -> Time {
    if !s.is_finite() {
        return Time::ZERO;
    }
    Time::from_secs_f64(s.clamp(-36_000_000.0, 36_000_000.0))
}

/// A media file named by the project: where it was saved, else (moved
/// along with the project) the same name beside the project or a
/// relative path from it. The saved path when none exists, so it can be
/// relinked later.
pub(crate) fn locate(saved: &[&str], dir: Option<&Path>) -> Option<PathBuf> {
    let saved: Vec<&str> = saved.iter().map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
    for s in &saved {
        let p = PathBuf::from(s);
        if p.is_absolute() && p.exists() {
            return Some(p);
        }
    }
    if let Some(dir) = dir {
        for s in &saved {
            let p = PathBuf::from(s.replace('\\', "/"));
            if p.is_relative() {
                let q = dir.join(&p);
                if q.exists() {
                    return Some(q);
                }
            }
            // A Windows or Mac path saved elsewhere: its name beside the project.
            if let Some(name) = s.rsplit(['/', '\\']).next().filter(|n| !n.is_empty()) {
                for q in [dir.join(name), dir.join("media").join(name), dir.join("Media").join(name), dir.join("footage").join(name), dir.join("Footage").join(name)] {
                    if q.exists() {
                        return Some(q);
                    }
                }
            }
        }
    }
    let first = saved.iter().find(|s| PathBuf::from(*s).is_absolute()).or(saved.first())?;
    let p = PathBuf::from(first);
    Some(match (p.is_relative(), dir) {
        (true, Some(d)) => d.join(p),
        _ => p,
    })
}

/// A file name for display (the last part of a Windows or Unix path).
pub(crate) fn file_name(p: &str) -> String {
    p.rsplit(['/', '\\']).next().unwrap_or(p).to_string()
}

/// "1 sequence" / "3 sequences".
pub(crate) fn count(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rates_come_out_exact() {
        assert_eq!(rate_of(23.976), FrameRate::new(24000, 1001));
        assert_eq!(rate_of(29.97002997), FrameRate::new(30000, 1001));
        assert_eq!(rate_of(25.0), FrameRate::new(25, 1));
        assert_eq!(rate_of(12.5), FrameRate::new(12500, 1000));
        assert_eq!(rate_of(f64::NAN), FrameRate::new(30, 1));
        assert_eq!(rate_of(-3.0), FrameRate::new(30, 1));
    }

    #[test]
    fn notes_count_and_sort() {
        let mut n = Notes::default();
        n.add("b");
        n.add("a");
        n.add("b");
        assert_eq!(n.lines(), vec!["b (×2)".to_string(), "a".to_string()]);
    }

    #[test]
    fn kinds_by_content_or_name() {
        assert_eq!(kind_of(Path::new("x.bin"), b"RIFX\0\0"), Some(Kind::AfterEffects));
        assert_eq!(kind_of(Path::new("x.prproj"), b"\x1f\x8b"), Some(Kind::Premiere));
        assert_eq!(kind_of(Path::new("x.txt"), b"hello"), None);
    }
}
