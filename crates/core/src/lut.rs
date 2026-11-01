//! Lookup tables for colour effects: tone curves (Curves) and 3D colour LUTs
//! (.cube files, as made by DaVinci Resolve, Photoshop and most LUT packs).
//!
//! Both end up as a small 8-bit RGBA image the GPU samples:
//! - a curve is 256×1: each texel holds the output for that input level,
//!   per channel;
//! - a 3D LUT of size N is laid out as N slices of N×N side by side
//!   (width N·N, height N): texel (r + b·N, g) holds the colour for input (r, g, b).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

/// A table the GPU looks colours up in.
#[derive(Debug)]
pub struct Table {
    /// Changes whenever the contents do (for the GPU's texture cache).
    pub id: u64,
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    /// 0 for a 1D curve table, N for an N³ LUT.
    pub cube_size: u32,
}

impl PartialEq for Table {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

fn hash_of(data: &impl std::hash::Hash) -> u64 {
    use std::hash::Hasher;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    data.hash(&mut h);
    h.finish()
}

// ---- Curves ------------------------------------------------------------------------

/// Tone curves: a master curve applied to all channels, then one per channel.
/// Points are (input, output) in 0–1, in display (gamma-encoded) values.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Curves {
    pub master: Vec<[f32; 2]>,
    pub red: Vec<[f32; 2]>,
    pub green: Vec<[f32; 2]>,
    pub blue: Vec<[f32; 2]>,
}

impl Default for Curves {
    fn default() -> Self {
        let line = vec![[0.0, 0.0], [1.0, 1.0]];
        Self { master: line.clone(), red: line.clone(), green: line.clone(), blue: line }
    }
}

impl Curves {
    pub fn channels(&self) -> [&Vec<[f32; 2]>; 4] {
        [&self.master, &self.red, &self.green, &self.blue]
    }
    pub fn channel_mut(&mut self, i: usize) -> &mut Vec<[f32; 2]> {
        match i {
            0 => &mut self.master,
            1 => &mut self.red,
            2 => &mut self.green,
            _ => &mut self.blue,
        }
    }

    /// Does it leave every colour as it is?
    pub fn is_identity(&self) -> bool {
        self.channels().iter().all(|c| (0..=16).all(|i| {
            let x = i as f32 / 16.0;
            (eval_curve(c, x) - x).abs() < 1e-3
        }))
    }

    /// The 256×1 table for the GPU.
    pub fn table(&self) -> Table {
        let mut rgba = Vec::with_capacity(256 * 4);
        for i in 0..256 {
            let x = eval_curve(&self.master, i as f32 / 255.0);
            for ch in [&self.red, &self.green, &self.blue] {
                rgba.push((eval_curve(ch, x).clamp(0.0, 1.0) * 255.0).round() as u8);
            }
            rgba.push(255);
        }
        Table { id: hash_of(&rgba), width: 256, height: 1, rgba, cube_size: 0 }
    }
}

/// A smooth curve through `points` (sorted by input), without overshoot
/// (monotone cubic interpolation). Flat beyond the first and last points.
pub fn eval_curve(points: &[[f32; 2]], x: f32) -> f32 {
    let mut p: Vec<[f32; 2]> = points.to_vec();
    p.sort_by(|a, b| a[0].total_cmp(&b[0]));
    p.dedup_by(|a, b| (a[0] - b[0]).abs() < 1e-6);
    match p.len() {
        0 => return x,
        1 => return p[0][1],
        _ => {}
    }
    if x <= p[0][0] {
        return p[0][1];
    }
    if x >= p[p.len() - 1][0] {
        return p[p.len() - 1][1];
    }
    let n = p.len();
    let d: Vec<f32> = (0..n - 1).map(|i| (p[i + 1][1] - p[i][1]) / (p[i + 1][0] - p[i][0])).collect();
    // Fritsch–Carlson tangents.
    let mut m = vec![0.0f32; n];
    m[0] = d[0];
    m[n - 1] = d[n - 2];
    for i in 1..n - 1 {
        m[i] = if d[i - 1] * d[i] <= 0.0 { 0.0 } else { (d[i - 1] + d[i]) / 2.0 };
    }
    for i in 0..n - 1 {
        if d[i] == 0.0 {
            m[i] = 0.0;
            m[i + 1] = 0.0;
        } else {
            let (a, b) = (m[i] / d[i], m[i + 1] / d[i]);
            let s = a * a + b * b;
            if s > 9.0 {
                let t = 3.0 / s.sqrt();
                m[i] = t * a * d[i];
                m[i + 1] = t * b * d[i];
            }
        }
    }
    let i = p.windows(2).position(|w| x >= w[0][0] && x <= w[1][0]).unwrap_or(n - 2);
    let h = p[i + 1][0] - p[i][0];
    let t = (x - p[i][0]) / h;
    let (t2, t3) = (t * t, t * t * t);
    (2.0 * t3 - 3.0 * t2 + 1.0) * p[i][1] + (t3 - 2.0 * t2 + t) * h * m[i] + (-2.0 * t3 + 3.0 * t2) * p[i + 1][1] + (t3 - t2) * h * m[i + 1]
}

// ---- .cube LUTs ---------------------------------------------------------------------

/// Read a .cube 3D LUT.
pub fn parse_cube(text: &str) -> Result<Table, String> {
    let mut size = 0usize;
    let (mut lo, mut hi) = ([0.0f32; 3], [1.0f32; 3]);
    let mut data: Vec<[f32; 3]> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut words = line.split_whitespace();
        let first = words.next().unwrap_or("");
        let nums = |w: std::str::SplitWhitespace| -> Result<Vec<f32>, String> { w.map(|s| s.parse::<f32>().map_err(|_| format!("bad number '{s}'"))).collect() };
        match first {
            "LUT_3D_SIZE" => size = words.next().and_then(|s| s.parse().ok()).ok_or("bad LUT_3D_SIZE")?,
            "LUT_1D_SIZE" => return Err("1D LUTs aren't supported; use a 3D .cube LUT".into()),
            "DOMAIN_MIN" => lo.copy_from_slice(nums(words)?.get(..3).ok_or("bad DOMAIN_MIN")?),
            "DOMAIN_MAX" => hi.copy_from_slice(nums(words)?.get(..3).ok_or("bad DOMAIN_MAX")?),
            "TITLE" | "LUT_3D_INPUT_RANGE" => {}
            s if s.parse::<f32>().is_ok() => {
                let v = nums(line.split_whitespace())?;
                if v.len() < 3 {
                    return Err("a data line needs three numbers".into());
                }
                data.push([v[0], v[1], v[2]]);
            }
            _ => {} // other keywords are harmless
        }
    }
    if !(2..=128).contains(&size) {
        return Err("missing or unusual LUT_3D_SIZE".into());
    }
    if data.len() != size * size * size {
        return Err(format!("expected {} entries, found {}", size * size * size, data.len()));
    }
    // Entries are listed red fastest, then green, then blue. Lay out slices of
    // constant blue side by side: texel (r + b·N, g).
    let n = size;
    let mut rgba = vec![0u8; n * n * n * 4];
    for b in 0..n {
        for g in 0..n {
            for r in 0..n {
                let v = data[r + g * n + b * n * n];
                let o = ((g * n * n) + b * n + r) * 4;
                for c in 0..3 {
                    let x = (v[c] - lo[c]) / (hi[c] - lo[c]).max(1e-6);
                    rgba[o + c] = (x.clamp(0.0, 1.0) * 255.0).round() as u8;
                }
                rgba[o + 3] = 255;
            }
        }
    }
    Ok(Table { id: hash_of(&rgba), width: (n * n) as u32, height: n as u32, rgba, cube_size: n as u32 })
}

/// Load a .cube file, cached by path and modification time (several clips can share one).
pub fn load_cube(path: &Path) -> Result<Arc<Table>, String> {
    type Cache = Mutex<HashMap<PathBuf, (std::time::SystemTime, Arc<Table>)>>;
    static CACHE: OnceLock<Cache> = OnceLock::new();
    let modified = std::fs::metadata(path).and_then(|m| m.modified()).map_err(|e| format!("can't read {}: {e}", path.display()))?;
    let cache = CACHE.get_or_init(Default::default);
    if let Some((t, table)) = cache.lock().unwrap().get(path) {
        if *t == modified {
            return Ok(table.clone());
        }
    }
    let text = std::fs::read_to_string(path).map_err(|e| format!("can't read {}: {e}", path.display()))?;
    let table = Arc::new(parse_cube(&text)?);
    cache.lock().unwrap().insert(path.to_owned(), (modified, table.clone()));
    Ok(table)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curves_are_smooth_and_pass_through_their_points() {
        let pts = [[0.0, 0.0], [0.25, 0.15], [0.75, 0.85], [1.0, 1.0]]; // an S-curve
        assert!((eval_curve(&pts, 0.25) - 0.15).abs() < 1e-5);
        assert!((eval_curve(&pts, 0.5) - 0.5).abs() < 1e-3, "symmetric");
        let mut last = -1.0;
        for i in 0..=100 {
            let y = eval_curve(&pts, i as f32 / 100.0);
            assert!(y >= last, "monotone");
            last = y;
        }
        assert!(Curves::default().is_identity());
        let c = Curves { master: pts.to_vec(), ..Default::default() };
        assert!(!c.is_identity());
        let t = c.table();
        assert_eq!((t.width, t.height, t.rgba.len()), (256, 1, 1024));
        assert_eq!(t.rgba[0], 0);
        assert!(t.rgba[64 * 4] < 64, "darker shadows: {}", t.rgba[64 * 4]);
    }

    #[test]
    fn parses_cube_luts() {
        // A 2×2×2 LUT that swaps red and blue.
        let mut text = String::from("TITLE \"swap\"\n# comment\nLUT_3D_SIZE 2\n");
        for b in 0..2 {
            for g in 0..2 {
                for r in 0..2 {
                    text += &format!("{} {} {}\n", b, g, r);
                }
            }
        }
        let t = parse_cube(&text).unwrap();
        assert_eq!((t.width, t.height, t.cube_size), (4, 2, 2));
        // Input (r=1, g=0, b=0) is texel (1 + 0, 0) and should come out blue.
        assert_eq!(&t.rgba[4..8], &[0, 0, 255, 255]);
        // Input (r=0, g=0, b=1) is texel (0 + 2, 0) and should come out red.
        assert_eq!(&t.rgba[8..12], &[255, 0, 0, 255]);
        assert!(parse_cube("LUT_3D_SIZE 2\n0 0 0\n").is_err());
        assert!(parse_cube("LUT_1D_SIZE 16\n").is_err());
    }
}

/// Reads a clean plate (the frame of a picture or video file at a media
/// time) as RGBA8; set by the media crate, which can decode files.
pub type PlateLoader = fn(&Path, crate::Time) -> Option<(u32, u32, Vec<u8>)>;
static PLATE_LOADER: OnceLock<PlateLoader> = OnceLock::new();

pub fn set_plate_loader(f: PlateLoader) {
    let _ = PLATE_LOADER.set(f);
}

/// A difference key's clean plate as a table the GPU samples (by layer uv),
/// cached by file, time and modification time.
pub fn load_plate(path: &Path, at: crate::Time) -> Option<Arc<Table>> {
    type Cache = Mutex<HashMap<(PathBuf, i64), (std::time::SystemTime, Option<Arc<Table>>)>>;
    static CACHE: OnceLock<Cache> = OnceLock::new();
    let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok()?;
    let cache = CACHE.get_or_init(Default::default);
    let key = (path.to_path_buf(), at.0);
    if let Some((m, t)) = cache.lock().unwrap().get(&key) {
        if *m == modified {
            return t.clone();
        }
    }
    let loaded = PLATE_LOADER.get().and_then(|f| f(path, at)).map(|(width, height, rgba)| {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (&key, modified).hash(&mut h);
        Arc::new(Table { id: h.finish() | 1 << 63, width, height, rgba, cube_size: 0 })
    });
    // Not cached when there's no loader yet (it may be set later).
    if PLATE_LOADER.get().is_some() {
        cache.lock().unwrap().insert(key, (modified, loaded.clone()));
    }
    loaded
}
