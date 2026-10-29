//! Image sequences: numbered still frames (`shot.1001.exr`, `shot.1002.exr`…)
//! used as one clip, as VFX renders and comps are handed over.
//!
//! A sequence is kept as a media path holding the frame-number pattern, the
//! frame rate it plays at and its first frame, e.g. `/shots/comp.%04d.exr#24@1001`: FFmpeg's
//! image-sequence reader takes the pattern, the media crate passes it the
//! first frame number and the rate. Everything else treats it as a path.

use crate::time::Rational;
use std::path::{Path, PathBuf};

/// Still-image types that make sequences.
pub const EXTENSIONS: [&str; 10] = ["exr", "dpx", "png", "tif", "tiff", "jpg", "jpeg", "tga", "bmp", "webp"];

/// A sequence: `dir/prefix` + the frame number (padded to `digits`, 0 = not
/// padded) + `suffix`, frames `first..=last`, played at `rate`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageSequence {
    pub dir: PathBuf,
    pub prefix: String,
    pub digits: usize,
    pub suffix: String,
    pub first: u64,
    pub last: u64,
    pub rate: Rational,
}

impl ImageSequence {
    /// The frame-number pattern FFmpeg reads (`comp.%04d.exr`).
    pub fn pattern(&self) -> PathBuf {
        let num = if self.digits > 1 { format!("%0{}d", self.digits) } else { "%d".to_string() };
        self.dir.join(format!("{}{}{}", self.prefix, num, self.suffix))
    }

    /// The path a project keeps: the pattern, `#` and the rate, `@` and the first frame.
    pub fn media_path(&self) -> PathBuf {
        let mut s = self.pattern().into_os_string();
        s.push(format!("#{}@{}", rate_text(self.rate), self.first));
        PathBuf::from(s)
    }

    /// Frame `n`'s file.
    pub fn frame(&self, n: u64) -> PathBuf {
        self.dir.join(format!("{}{:0width$}{}", self.prefix, n, self.suffix, width = self.digits))
    }

    pub fn len(&self) -> u64 {
        self.last - self.first + 1
    }

    pub fn is_empty(&self) -> bool {
        false
    }

    /// What to call it: `comp.[1001-1100].exr`.
    pub fn name(&self) -> String {
        format!("{}[{}-{}]{}", self.prefix, self.first, self.last, self.suffix)
    }

    /// The same frames at another rate.
    pub fn with_rate(mut self, rate: Rational) -> Self {
        self.rate = rate;
        self
    }
}

fn rate_text(r: Rational) -> String {
    if r.den == 1 { r.num.to_string() } else { format!("{}/{}", r.num, r.den) }
}

/// A sequence path (`…%04d.exr#24@1001`) taken apart; the frames are looked up on disk.
pub fn parse(path: &Path) -> Option<ImageSequence> {
    let s = path.to_str()?;
    let (pat, tail) = s.rsplit_once('#')?;
    let (rate, start) = match tail.split_once('@') {
        Some((r, f)) => (r, Some(f.parse::<u64>().ok()?)),
        None => (tail, None),
    };
    let rate = match rate.split_once('/') {
        Some((n, d)) => Rational::new(n.parse().ok()?, d.parse().ok()?),
        None => Rational::new(rate.parse().ok()?, 1),
    };
    if rate.num <= 0 || rate.den <= 0 {
        return None;
    }
    let pat = Path::new(pat);
    let name = pat.file_name()?.to_str()?;
    let at = name.find('%')?;
    let rest = &name[at + 1..];
    let d_at = rest.find('d')?;
    let width = &rest[..d_at];
    let digits = if width.is_empty() { 0 } else { width.trim_start_matches('0').parse().ok()? };
    let (prefix, suffix) = (name[..at].to_string(), rest[d_at + 1..].to_string());
    let dir = pat.parent().map(Path::to_path_buf).unwrap_or_default();
    let numbers = numbers_in(&dir, &prefix, &suffix, digits);
    let (first, last) = contiguous(&numbers, start.unwrap_or(*numbers.first()?))?;
    Some(ImageSequence { dir, prefix, digits, suffix, first, last, rate })
}

/// The file name of a sequence path's first frame (`comp.1001.exr`),
/// worked out from the path alone (the frames may be missing).
pub fn first_frame_name(path: &Path) -> Option<String> {
    let s = path.to_str()?;
    let (pat, tail) = s.rsplit_once('#')?;
    let first: u64 = tail.split_once('@')?.1.parse().ok()?;
    let name = Path::new(pat).file_name()?.to_str()?;
    let at = name.find('%')?;
    let rest = &name[at + 1..];
    let d_at = rest.find('d')?;
    let width = &rest[..d_at];
    let digits: usize = if width.is_empty() { 0 } else { width.trim_start_matches('0').parse().ok()? };
    Some(format!("{}{:0digits$}{}", &name[..at], first, &rest[d_at + 1..]))
}

/// The rate a sequence path plays at.
pub fn rate_of(path: &Path) -> Option<Rational> {
    let tail = path.to_str()?.rsplit_once('#')?.1;
    let rate = tail.split_once('@').map_or(tail, |(r, _)| r);
    Some(match rate.split_once('/') {
        Some((n, d)) => Rational::new(n.parse().ok()?, d.parse().ok()?),
        None => Rational::new(rate.parse().ok()?, 1),
    })
}

/// Is `path` a sequence path?
pub fn is_sequence(path: &Path) -> bool {
    path.to_str().is_some_and(|s| s.contains('%') && s.contains('#'))
}

/// Does the media exist (for a sequence: at least its first frame)?
pub fn media_exists(path: &Path) -> bool {
    if is_sequence(path) {
        parse(path).is_some()
    } else {
        path.exists()
    }
}

/// The frames of `file`'s sequence if it's a numbered still with numbered
/// neighbours (`shot.1001.exr` next to `shot.1002.exr`), at `rate`. The
/// sequence runs from the first frame without a gap up to `file`, and on
/// until the next missing frame.
pub fn detect(file: &Path, rate: Rational) -> Option<ImageSequence> {
    let ext = file.extension()?.to_str()?.to_ascii_lowercase();
    if !EXTENSIONS.contains(&ext.as_str()) {
        return None;
    }
    let name = file.file_name()?.to_str()?;
    let stem_len = name.len() - ext.len() - 1;
    // The last run of digits before the extension.
    let stem = &name[..stem_len];
    let end = stem.len() - stem.bytes().rev().take_while(|b| !b.is_ascii_digit()).count();
    let start = stem[..end].bytes().rposition(|b| !b.is_ascii_digit()).map_or(0, |i| i + 1);
    if start == end {
        return None;
    }
    let num_text = &stem[start..end];
    let n: u64 = num_text.parse().ok()?;
    let (prefix, suffix) = (name[..start].to_string(), name[end..].to_string());
    let dir = file.parent().map(Path::to_path_buf).unwrap_or_default();
    // Padded (`0042`) numbers keep their width; others (`42`, `100`) don't.
    let padded = num_text.len() > 1 && num_text.starts_with('0');
    let digits = if padded { num_text.len() } else { 0 };
    let mut numbers = numbers_in(&dir, &prefix, &suffix, digits);
    if !padded {
        // Unpadded, unless all the neighbours are the same width (then padding
        // just never showed: 1001, 1002…).
        let same = numbers_in(&dir, &prefix, &suffix, num_text.len());
        if same.len() == numbers.len() {
            numbers = same;
            return finish(dir, prefix, num_text.len(), suffix, &numbers, n, rate);
        }
    }
    finish(dir, prefix, digits, suffix, &numbers, n, rate)
}

fn finish(dir: PathBuf, prefix: String, digits: usize, suffix: String, numbers: &[u64], n: u64, rate: Rational) -> Option<ImageSequence> {
    let (first, last) = contiguous(numbers, n)?;
    if first == last {
        return None;
    }
    Some(ImageSequence { dir, prefix, digits, suffix, first, last, rate })
}

/// The gap-free run of `numbers` (sorted) around `n`.
fn contiguous(numbers: &[u64], n: u64) -> Option<(u64, u64)> {
    let i = numbers.binary_search(&n).ok()?;
    let (mut a, mut b) = (i, i);
    while a > 0 && numbers[a - 1] + 1 == numbers[a] {
        a -= 1;
    }
    while b + 1 < numbers.len() && numbers[b] + 1 == numbers[b + 1] {
        b += 1;
    }
    Some((numbers[a], numbers[b]))
}

/// The frame numbers present: files `prefix` + number + `suffix` (the number
/// exactly `digits` wide if `digits` > 0, else without leading zeros).
fn numbers_in(dir: &Path, prefix: &str, suffix: &str, digits: usize) -> Vec<u64> {
    let read = if dir.as_os_str().is_empty() { std::fs::read_dir(".") } else { std::fs::read_dir(dir) };
    let Ok(rd) = read else { return Vec::new() };
    let mut out: Vec<u64> = rd
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let mid = name.strip_prefix(prefix)?.strip_suffix(suffix)?;
            if mid.is_empty() || !mid.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            let ok = if digits > 0 { mid.len() == digits } else { mid.len() == 1 || !mid.starts_with('0') };
            ok.then(|| mid.parse().ok()).flatten()
        })
        .collect();
    out.sort_unstable();
    out.dedup();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir_with(files: &[&str]) -> PathBuf {
        let d = std::env::temp_dir().join(format!("rf_seq_{}_{}", std::process::id(), files.len() * 7 + files.first().map_or(0, |f| f.len())));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        for f in files {
            std::fs::write(d.join(f), b"x").unwrap();
        }
        d
    }

    #[test]
    fn finds_padded_vfx_sequences_and_round_trips_their_path() {
        let d = dir_with(&["comp_v2.1001.exr", "comp_v2.1002.exr", "comp_v2.1003.exr", "comp_v2.1005.exr", "other.1001.exr", "comp_v2.1002.exr.bak"]);
        let s = detect(&d.join("comp_v2.1002.exr"), Rational::new(24, 1)).unwrap();
        assert_eq!((s.prefix.as_str(), s.digits, s.suffix.as_str(), s.first, s.last), ("comp_v2.", 4, ".exr", 1001, 1003), "stops at the gap");
        assert_eq!(s.name(), "comp_v2.[1001-1003].exr");
        assert_eq!(s.frame(1002), d.join("comp_v2.1002.exr"));
        let p = s.media_path();
        assert!(p.to_str().unwrap().ends_with("comp_v2.%04d.exr#24@1001"), "{p:?}");
        assert!(is_sequence(&p) && media_exists(&p));
        assert_eq!(parse(&p).unwrap(), s);
        assert_eq!(first_frame_name(&p).as_deref(), Some("comp_v2.1001.exr"));
        assert_eq!(rate_of(&p), Some(Rational::new(24, 1)));
        // After the gap: its own run (one frame isn't a sequence).
        assert_eq!(detect(&d.join("comp_v2.1005.exr"), Rational::new(24, 1)), None);
        std::fs::write(d.join("comp_v2.1006.exr"), b"x").unwrap();
        let later = detect(&d.join("comp_v2.1005.exr"), Rational::new(24, 1)).unwrap();
        assert_eq!((later.first, later.last), (1005, 1006));
        assert_eq!(parse(&later.media_path()).unwrap(), later, "reopens as the same run");
        let ntsc = s.clone().with_rate(Rational::new(24000, 1001));
        assert_eq!(parse(&ntsc.media_path()).unwrap().rate, Rational::new(24000, 1001));
        std::fs::remove_dir_all(&d).ok();
        assert!(!media_exists(&p));
    }

    #[test]
    fn zero_padded_and_unpadded_numbers() {
        let d = dir_with(&["shot_0000.png", "shot_0001.png", "shot_0002.png", "frame1.tif", "frame2.tif", "frame10.tif", "frame3.tif", "logo.png", "take2.png"]);
        let s = detect(&d.join("shot_0001.png"), Rational::new(30, 1)).unwrap();
        assert_eq!((s.first, s.last, s.digits), (0, 2, 4));
        assert!(s.pattern().to_str().unwrap().ends_with("shot_%04d.png"));
        let f = detect(&d.join("frame2.tif"), Rational::new(30, 1)).unwrap();
        assert_eq!((f.first, f.last, f.digits), (1, 3, 0), "frame10 is after a gap");
        assert!(f.pattern().to_str().unwrap().ends_with("frame%d.tif"));
        assert_eq!(detect(&d.join("logo.png"), Rational::new(30, 1)), None);
        assert_eq!(detect(&d.join("take2.png"), Rational::new(30, 1)), None, "a still with a number but no neighbours");
        std::fs::remove_dir_all(&d).ok();
    }
}
