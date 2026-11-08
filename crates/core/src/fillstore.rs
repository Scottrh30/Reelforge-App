//! The frames an Object removal effect made (see `crate::inpaint`), kept on
//! disk: a folder with `index.json` (each frame's media time and where its
//! patch sits) and one raw file per frame: the patch's RGBA (sRGB colour, and
//! alpha for how much of it covers the picture). Frames are read back as
//! they're played and the last few dozen kept in memory.

use crate::lut::Table;
use crate::time::Time;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

/// One filled frame.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FillFrame {
    /// Media time (ticks).
    pub at: i64,
    pub file: String,
    /// The patch: left, top, width, height in pixels of the media frame.
    pub rect: [u32; 4],
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FillIndex {
    /// The media frame's size the patches were made at.
    pub size: [u32; 2],
    /// In time order.
    pub frames: Vec<FillFrame>,
    /// A preview of one frame (a frame's length, in ticks): it shows only
    /// at that frame, not stretched over the rest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<i64>,
}

/// Write one frame's patch (`rgba`, `rect[2]` × `rect[3]`) into `dir`.
pub fn write_frame(dir: &Path, n: usize, rect: [u32; 4], rgba: &[u8]) -> std::io::Result<String> {
    let file = format!("{n:06}.rff");
    let mut data = Vec::with_capacity(rgba.len() + 20);
    data.extend_from_slice(b"RFF1");
    for v in rect {
        data.extend_from_slice(&v.to_le_bytes());
    }
    data.extend_from_slice(rgba);
    std::fs::write(dir.join(&file), data)?;
    Ok(file)
}

pub fn write_index(dir: &Path, index: &FillIndex) -> std::io::Result<()> {
    std::fs::write(dir.join("index.json"), serde_json::to_vec(index).map_err(std::io::Error::other)?)
}

fn read_index(dir: &Path) -> Option<Arc<FillIndex>> {
    type Cache = Mutex<HashMap<PathBuf, (std::time::SystemTime, Arc<FillIndex>)>>;
    static CACHE: OnceLock<Cache> = OnceLock::new();
    let path = dir.join("index.json");
    let modified = std::fs::metadata(&path).and_then(|m| m.modified()).ok()?;
    let cache = CACHE.get_or_init(Default::default);
    if let Some((m, i)) = cache.lock().unwrap().get(dir) {
        if *m == modified {
            return Some(i.clone());
        }
    }
    let idx: FillIndex = serde_json::from_slice(&std::fs::read(&path).ok()?).ok()?;
    let idx = Arc::new(idx);
    cache.lock().unwrap().insert(dir.to_path_buf(), (modified, idx.clone()));
    Some(idx)
}

/// Is there a finished fill in `dir`?
pub fn exists(dir: &Path) -> bool {
    read_index(dir).is_some_and(|i| !i.frames.is_empty())
}

/// Is the fill in `dir` a preview of one frame?
pub fn is_preview(dir: &Path) -> bool {
    read_index(dir).is_some_and(|i| i.preview.is_some())
}

/// How many frames the fill in `dir` has.
pub fn frame_count(dir: &Path) -> usize {
    read_index(dir).map_or(0, |i| i.frames.len())
}

/// The patch for media time `t` (the nearest frame made), as a table for the
/// GPU, and where it sits: left, top, width, height as fractions of the frame.
pub fn load(dir: &Path, t: Time) -> Option<(Arc<Table>, [f32; 4])> {
    let idx = read_index(dir)?;
    let i = idx.frames.partition_point(|f| f.at < t.0);
    let f = [i.checked_sub(1), Some(i)].into_iter().flatten().filter_map(|k| idx.frames.get(k)).min_by_key(|f| (f.at - t.0).abs())?;
    if idx.preview.is_some_and(|len| (f.at - t.0).abs() * 2 > len) {
        return None;
    }
    let size = [idx.size[0].max(1) as f32, idx.size[1].max(1) as f32];
    let place = [f.rect[0] as f32 / size[0], f.rect[1] as f32 / size[1], f.rect[2] as f32 / size[0], f.rect[3] as f32 / size[1]];
    type Lru = Mutex<VecDeque<(PathBuf, Arc<Table>)>>;
    static FRAMES: OnceLock<Lru> = OnceLock::new();
    let path = dir.join(&f.file);
    let lru = FRAMES.get_or_init(Default::default);
    if let Some(t) = lru.lock().unwrap().iter().find(|(p, _)| *p == path).map(|(_, t)| t.clone()) {
        return Some((t, place));
    }
    let data = std::fs::read(&path).ok()?;
    if data.len() < 20 || &data[..4] != b"RFF1" {
        return None;
    }
    let word = |k: usize| u32::from_le_bytes(data[4 + 4 * k..8 + 4 * k].try_into().unwrap());
    let (w, h) = (word(2), word(3));
    let rgba = data[20..].to_vec();
    if w == 0 || h == 0 || rgba.len() != (w * h * 4) as usize {
        return None;
    }
    let id = {
        use std::hash::{Hash, Hasher};
        let mut hs = std::collections::hash_map::DefaultHasher::new();
        (&path, std::fs::metadata(&path).and_then(|m| m.modified()).ok()).hash(&mut hs);
        hs.finish() | 1 << 62
    };
    let table = Arc::new(Table { id, width: w, height: h, rgba, cube_size: 0 });
    let mut l = lru.lock().unwrap();
    l.push_back((path, table.clone()));
    while l.len() > 48 {
        l.pop_front();
    }
    Some((table, place))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_are_written_and_found_by_time() {
        let dir = std::env::temp_dir().join(format!("rf-fill-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut idx = FillIndex { size: [100, 50], frames: Vec::new(), preview: None };
        for n in 0..3 {
            let rgba = vec![n as u8 * 100; 2 * 2 * 4];
            let file = write_frame(&dir, n, [10, 20, 2, 2], &rgba).unwrap();
            idx.frames.push(FillFrame { at: Time::from_secs_f64(n as f64 / 10.0).0, file, rect: [10, 20, 2, 2] });
        }
        assert!(!exists(&dir));
        write_index(&dir, &idx).unwrap();
        assert!(exists(&dir));
        let (t, place) = load(&dir, Time::from_secs_f64(0.11)).unwrap();
        assert_eq!(t.rgba[0], 100);
        assert_eq!(place, [0.1, 0.4, 0.02, 0.04]);
        assert_eq!(load(&dir, Time::from_secs_f64(5.0)).unwrap().0.rgba[0], 200);
        // A preview shows only at its own frame.
        idx.frames.truncate(1);
        idx.preview = Some(Time::from_secs_f64(0.04).0);
        write_index(&dir, &idx).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        let _ = std::fs::File::options().append(true).open(dir.join("index.json")).and_then(|f| f.set_modified(std::time::SystemTime::now()));
        assert!(is_preview(&dir));
        assert!(load(&dir, Time::from_secs_f64(0.01)).is_some());
        assert!(load(&dir, Time::from_secs_f64(0.5)).is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
