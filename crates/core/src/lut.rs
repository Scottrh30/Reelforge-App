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

/// The Hue curves effect's five curves, each a list of points (x, y) in 0–1,
/// where y = 0.5 leaves things as they are. The x of the first three is hue
/// (0 red, ⅙ yellow, ⅓ green, ½ cyan, ⅔ blue, ⅚ magenta, wrapping round);
/// of the fourth luminance, of the fifth saturation.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct HueCurves {
    /// Hue vs hue: y − 0.5 turns the hue (±180° at the edges).
    #[serde(default)]
    pub hue_hue: Vec<[f32; 2]>,
    /// Hue vs saturation: saturation × 2y.
    #[serde(default)]
    pub hue_sat: Vec<[f32; 2]>,
    /// Hue vs luminance: brightness × 2y (of coloured pixels).
    #[serde(default)]
    pub hue_lum: Vec<[f32; 2]>,
    /// Luminance vs saturation: saturation × 2y.
    #[serde(default)]
    pub lum_sat: Vec<[f32; 2]>,
    /// Saturation vs saturation: saturation × 2y.
    #[serde(default)]
    pub sat_sat: Vec<[f32; 2]>,
}

/// The curves' names, in order (as `HueCurves::curve` numbers them).
pub const HUE_CURVE_NAMES: [&str; 5] = ["Hue vs hue", "Hue vs saturation", "Hue vs luminance", "Luminance vs saturation", "Saturation vs saturation"];

impl HueCurves {
    pub fn curve(&self, i: usize) -> &Vec<[f32; 2]> {
        match i {
            0 => &self.hue_hue,
            1 => &self.hue_sat,
            2 => &self.hue_lum,
            3 => &self.lum_sat,
            _ => &self.sat_sat,
        }
    }

    /// Curve `i` at `x`: the first three wrap round (hue is a circle), the
    /// others are flat past their end points. No points: 0.5 (no change).
    pub fn eval(&self, i: usize, x: f32) -> f32 {
        let pts = self.curve(i);
        if pts.is_empty() {
            return 0.5;
        }
        if i < 3 {
            // The points either side, a turn away, so the curve joins up.
            let mut p: Vec<[f32; 2]> = Vec::with_capacity(pts.len() * 3);
            for &q in pts {
                let q = [q[0].rem_euclid(1.0), q[1]];
                p.extend([[q[0] - 1.0, q[1]], q, [q[0] + 1.0, q[1]]]);
            }
            eval_curve(&p, x.rem_euclid(1.0)).clamp(0.0, 1.0)
        } else {
            eval_curve(pts, x.clamp(0.0, 1.0)).clamp(0.0, 1.0)
        }
    }

    pub fn is_identity(&self) -> bool {
        (0..5).all(|i| (0..=32).all(|k| (self.eval(i, k as f32 / 32.0) - 0.5).abs() < 1e-3))
    }

    /// The 256×5 table for the GPU: a row a curve, 16 bits a value (red the
    /// high byte, green the low).
    pub fn table(&self) -> Table {
        let mut rgba = Vec::with_capacity(256 * 5 * 4);
        for i in 0..5 {
            for k in 0..256 {
                let v = (self.eval(i, k as f32 / 255.0) * 65535.0).round() as u16;
                rgba.extend_from_slice(&[(v >> 8) as u8, (v & 255) as u8, 0, 255]);
            }
        }
        Table { id: hash_of(&rgba) ^ 0x4875_6543, width: 256, height: 5, rgba, cube_size: 0 }
    }

    /// What the curves do to display colour `c` (the GPU shader
    /// `fs_hue_curves` does the same), before Strength.
    pub fn apply(&self, c: [f32; 3]) -> [f32; 3] {
        let (h, s, _) = crate::effects::qualify::hsl(c);
        let h = h / 360.0;
        let y = 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
        let (cb, cr) = ((c[2] - y) / 1.8556, (c[0] - y) / 1.5748);
        let turn = (self.eval(0, h) - 0.5) * std::f32::consts::TAU;
        let (sn, cs) = turn.sin_cos();
        let gain = self.eval(1, h) * 2.0 * self.eval(3, y) * 2.0 * self.eval(4, s) * 2.0;
        let (cb, cr) = ((cb * cs - cr * sn) * gain, (cb * sn + cr * cs) * gain);
        let lg = self.eval(2, h) * 2.0;
        let y = y * (1.0 + (lg - 1.0) * (s * 1.5).clamp(0.0, 1.0));
        let r = y + 1.5748 * cr;
        let b = y + 1.8556 * cb;
        let g = (y - 0.2126 * r - 0.0722 * b) / 0.7152;
        [r.max(0.0), g.max(0.0), b.max(0.0)]
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

/// Reads a clean plate (the frame of a picture or video file at a media
/// time) as RGBA8; set by the media crate, which can decode files.
pub type PlateLoader = fn(&Path, crate::Time) -> Option<(u32, u32, Vec<u8>)>;
static PLATE_LOADER: OnceLock<PlateLoader> = OnceLock::new();

pub fn set_plate_loader(f: PlateLoader) {
    let _ = PLATE_LOADER.set(f);
}

/// A difference key's clean plate as a table the GPU samples (by layer uv),
/// cached by file, time and modification time.
/// A picture table's average colour, as 0xRRGGBB (sRGB; remembered by table).
pub fn average_rgb(t: &Table) -> u32 {
    static SEEN: OnceLock<Mutex<HashMap<u64, u32>>> = OnceLock::new();
    let seen = SEEN.get_or_init(Default::default);
    if let Some(v) = seen.lock().unwrap().get(&t.id) {
        return *v;
    }
    let n = (t.rgba.len() / 4).max(1) as u64;
    let mut s = [0u64; 3];
    for p in t.rgba.chunks_exact(4).step_by(7) {
        for k in 0..3 {
            s[k] += p[k] as u64;
        }
    }
    let m = n.div_ceil(7).max(1);
    let v = ((s[0] / m) << 16 | (s[1] / m) << 8 | (s[2] / m)) as u32;
    seen.lock().unwrap().insert(t.id, v);
    v
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hue_curves_change_only_what_they_are_drawn_for() {
        let flat = HueCurves::default();
        assert!(flat.is_identity());
        let c = [0.7, 0.3, 0.2];
        let same = flat.apply(c);
        assert!((0..3).all(|k| (same[k] - c[k]).abs() < 1e-4), "{same:?}");
        // Hue vs saturation: blues (⅔) desaturated, reds left.
        let blues_grey = HueCurves { hue_sat: vec![[0.5, 0.5], [0.667, 0.0], [0.83, 0.5]], ..Default::default() };
        let b = blues_grey.apply([0.15, 0.25, 0.85]);
        assert!((b[0] - b[2]).abs() < 0.06, "nearly grey: {b:?}");
        let r = blues_grey.apply([0.85, 0.2, 0.15]);
        assert!((r[0] - 0.85).abs() < 1e-3, "{r:?}");
        // It wraps: a point at 0.95 affects hue 0.02 too (reds near the seam).
        let wrap = HueCurves { hue_sat: vec![[0.3, 0.5], [0.95, 0.0], [0.6, 0.5]], ..Default::default() };
        assert!(wrap.eval(1, 0.02) < 0.3 && wrap.eval(1, 0.98) < 0.3);
        // Hue vs hue: greens turned (towards blue or yellow), with the same luminance.
        let turn = HueCurves { hue_hue: vec![[0.33, 0.6]], ..Default::default() };
        let g = [0.2, 0.7, 0.2];
        let t = turn.apply(g);
        let y = |c: [f32; 3]| 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
        assert!((y(t) - y(g)).abs() < 1e-3 && (t[2] - g[2]).abs() > 0.05, "{t:?}");
        // Luminance vs saturation: no colour in the shadows.
        let dark = HueCurves { lum_sat: vec![[0.0, 0.0], [0.3, 0.5], [1.0, 0.5]], ..Default::default() };
        let d = dark.apply([0.1, 0.03, 0.02]);
        assert!((d[0] - d[1]).abs() < 0.07 * 0.25, "most of the colour gone: {d:?}");
        // Hue vs luminance: oranges brighter, greys untouched.
        let lift = HueCurves { hue_lum: vec![[0.08, 0.75]], ..Default::default() };
        assert!(y(lift.apply([0.8, 0.45, 0.1])) > y([0.8, 0.45, 0.1]) * 1.2);
        assert_eq!(lift.apply([0.5, 0.5, 0.5]), [0.5, 0.5, 0.5]);
        // The table: 256 × 5, 16 bits, a flat curve at the middle.
        let tbl = blues_grey.table();
        assert_eq!((tbl.width, tbl.height), (256, 5));
        let mid = u16::from_be_bytes([tbl.rgba[0], tbl.rgba[1]]);
        assert!((mid as i32 - 32768).abs() < 2);
    }

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
