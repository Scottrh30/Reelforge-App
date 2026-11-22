//! Liquify: the picture pushed, twirled, pinched and swelled with a brush in
//! the Viewer, as in Photoshop's Liquify (the Liquify effect).
//!
//! Each stroke is kept (so it can be undone, and the effect's Amount can
//! fade the whole warp in and out); together they make a displacement field
//! over the layer: for each point, how far away to take the picture from.
//! The field is a grid of points a few pixels apart, worked out on the CPU
//! (stroke after stroke, dab after dab) and sent to the GPU as a small table:
//! each texel one grid point, its x then y offset as 16-bit numbers in
//! eighths of a pixel (32 768 is none).

use crate::lut::Table;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, PoisonError};

/// The most grid points across or down.
pub const MAX_GRID: usize = 512;
/// Offsets are kept to this many pixels (either way).
pub const MAX_SHIFT: f32 = 4000.0;
/// Most dabs a stroke is cut into (a longer one is spaced further apart).
const MAX_DABS: usize = 4000;
/// Most grid points a stroke changes (all its dabs together).
const WORK: f32 = 3.0e7;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Tool {
    /// The picture under the brush dragged along with it (Photoshop's
    /// forward warp).
    #[default]
    Push,
    /// Turned round under the brush, clockwise.
    TwirlClockwise,
    /// Turned round, anticlockwise.
    TwirlAnticlockwise,
    /// Drawn in towards the brush's middle (made smaller).
    Pucker,
    /// Pushed out from the middle (made bigger).
    Bloat,
    /// Earlier strokes taken back where it brushes.
    Reconstruct,
}

impl Tool {
    /// The tool for the effect's Tool menu entry.
    pub fn from_index(i: i32) -> Tool {
        match i {
            1 => Tool::TwirlClockwise,
            2 => Tool::TwirlAnticlockwise,
            3 => Tool::Pucker,
            4 => Tool::Bloat,
            5 => Tool::Reconstruct,
            _ => Tool::Push,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LiquifyStroke {
    pub tool: Tool,
    /// Where the brush went (layer pixels). A point repeated means the brush
    /// was held there (twirl, pucker and bloat keep working).
    pub points: Vec<[f32; 2]>,
    /// Layer pixels.
    pub radius: f32,
    /// 0–100.
    pub pressure: f32,
}

/// The warp: how far away (layer pixels) to take the picture from, at each
/// grid point `cell` pixels apart, row by row.
#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    pub cols: usize,
    pub rows: usize,
    pub cell: f32,
    pub shift: Vec<[f32; 2]>,
}

impl Field {
    /// No warp over a layer `size` pixels across.
    pub fn new(size: [f32; 2]) -> Field {
        let (w, h) = (size[0].max(1.0), size[1].max(1.0));
        let w = if w.is_finite() { w.min(65_536.0) } else { 1.0 };
        let h = if h.is_finite() { h.min(65_536.0) } else { 1.0 };
        // Points at most MAX_GRID across, at least 2 px apart.
        let cell = (w.max(h) / (MAX_GRID - 1) as f32).max(2.0);
        let cols = ((w / cell).ceil() as usize + 1).clamp(2, MAX_GRID);
        let rows = ((h / cell).ceil() as usize + 1).clamp(2, MAX_GRID);
        Field { cols, rows, cell, shift: vec![[0.0; 2]; cols * rows] }
    }

    /// The offset at layer point `p` (between grid points, blended; past the
    /// edges, the nearest).
    pub fn at(&self, p: [f32; 2]) -> [f32; 2] {
        let gx = (p[0] / self.cell).clamp(0.0, (self.cols - 1) as f32);
        let gy = (p[1] / self.cell).clamp(0.0, (self.rows - 1) as f32);
        if !gx.is_finite() || !gy.is_finite() {
            return [0.0; 2];
        }
        let (x0, y0) = (gx.floor() as usize, gy.floor() as usize);
        let (x1, y1) = ((x0 + 1).min(self.cols - 1), (y0 + 1).min(self.rows - 1));
        let (fx, fy) = (gx - x0 as f32, gy - y0 as f32);
        let g = |x: usize, y: usize| self.shift.get(y * self.cols + x).copied().unwrap_or([0.0; 2]);
        let (a, b, c, d) = (g(x0, y0), g(x1, y0), g(x0, y1), g(x1, y1));
        let mix = |i: usize| {
            let top = a[i] + (b[i] - a[i]) * fx;
            let bottom = c[i] + (d[i] - c[i]) * fx;
            top + (bottom - top) * fy
        };
        [mix(0), mix(1)]
    }

    /// Does it move anything?
    pub fn is_empty(&self) -> bool {
        self.shift.iter().all(|s| s[0].abs() < 0.01 && s[1].abs() < 0.01)
    }

    /// One dab of `tool` at `c` with `radius`, `strength` (0–1) and, for a
    /// push, how far the brush moved since the last dab.
    fn dab(&mut self, tool: Tool, c: [f32; 2], radius: f32, strength: f32, moved: [f32; 2]) {
        let r = radius.max(0.5);
        let (lo_x, hi_x) = (((c[0] - r) / self.cell).floor().max(0.0) as usize, ((c[0] + r) / self.cell).ceil().max(0.0) as usize);
        let (lo_y, hi_y) = (((c[1] - r) / self.cell).floor().max(0.0) as usize, ((c[1] + r) / self.cell).ceil().max(0.0) as usize);
        let (hi_x, hi_y) = (hi_x.min(self.cols - 1), hi_y.min(self.rows - 1));
        if lo_x > hi_x || lo_y > hi_y {
            return;
        }
        // Read from the field as it was before this dab; write afterwards.
        let mut new = Vec::with_capacity((hi_x - lo_x + 1) * (hi_y - lo_y + 1));
        for y in lo_y..=hi_y {
            for x in lo_x..=hi_x {
                let p = [x as f32 * self.cell, y as f32 * self.cell];
                let (dx, dy) = (p[0] - c[0], p[1] - c[1]);
                let d2 = (dx * dx + dy * dy) / (r * r);
                if d2 >= 1.0 {
                    continue;
                }
                let w = (1.0 - d2) * (1.0 - d2) * strength;
                // Where this point now takes the picture from (before the old warp).
                let q = match tool {
                    Tool::Push => [p[0] - moved[0] * w, p[1] - moved[1] * w],
                    Tool::TwirlClockwise | Tool::TwirlAnticlockwise => {
                        // Clockwise on screen (y down): the picture turns towards +angle,
                        // so each point takes from a turn back.
                        let a = 0.3 * w * if tool == Tool::TwirlClockwise { -1.0 } else { 1.0 };
                        let (s, co) = a.sin_cos();
                        [c[0] + dx * co - dy * s, c[1] + dx * s + dy * co]
                    }
                    Tool::Pucker => [c[0] + dx * (1.0 + 0.1 * w), c[1] + dy * (1.0 + 0.1 * w)],
                    Tool::Bloat => [c[0] + dx * (1.0 - 0.1 * w), c[1] + dy * (1.0 - 0.1 * w)],
                    Tool::Reconstruct => {
                        if let Some(s) = self.shift.get(y * self.cols + x) {
                            let k = 1.0 - 0.35 * w;
                            new.push((y * self.cols + x, [s[0] * k, s[1] * k]));
                        }
                        continue;
                    }
                };
                let before = self.at(q);
                let s = [(q[0] + before[0] - p[0]).clamp(-MAX_SHIFT, MAX_SHIFT), (q[1] + before[1] - p[1]).clamp(-MAX_SHIFT, MAX_SHIFT)];
                if s[0].is_finite() && s[1].is_finite() {
                    new.push((y * self.cols + x, s));
                }
            }
        }
        for (i, s) in new {
            if let Some(slot) = self.shift.get_mut(i) {
                *slot = s;
            }
        }
    }

    /// Apply a stroke: its points cut into dabs a little way apart.
    pub fn stroke(&mut self, s: &LiquifyStroke) {
        let r = if s.radius.is_finite() { s.radius.clamp(0.5, 8000.0) } else { return };
        let strength = if s.pressure.is_finite() { (s.pressure / 100.0).clamp(0.0, 1.0) } else { return };
        if strength == 0.0 {
            return;
        }
        let pts: Vec<[f32; 2]> = s.points.iter().copied().filter(|p| p[0].is_finite() && p[1].is_finite()).collect();
        let Some(&first) = pts.first() else { return };
        // How far apart dabs are: closer for a push, which follows the brush.
        let length: f32 = pts.windows(2).map(|w| ((w[1][0] - w[0][0]).powi(2) + (w[1][1] - w[0][1]).powi(2)).sqrt()).sum();
        // At most so much work a stroke: a huge brush makes fewer dabs.
        let per_dab = ((r / self.cell).powi(2) * 4.0).min((self.cols * self.rows) as f32).max(1.0);
        let max_dabs = ((WORK / per_dab) as usize).clamp(1, MAX_DABS);
        let step = (r * 0.15).max(1.0).max(length / max_dabs as f32);
        self.dab(s.tool, first, r, strength, [0.0; 2]);
        let (mut last, mut carry, mut dabs) = (first, 0.0f32, 1usize);
        for w in pts.windows(2) {
            let (a, b) = (w[0], w[1]);
            let seg = ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt();
            if seg < 1e-3 {
                // Held still: keep working there (not for a push or reconstruct, which
                // need the brush to move… reconstruct keeps restoring).
                if s.tool != Tool::Push && dabs < max_dabs {
                    self.dab(s.tool, b, r, strength, [0.0; 2]);
                    dabs += 1;
                }
                continue;
            }
            let mut t = step - carry;
            while t <= seg && dabs < max_dabs {
                let p = [a[0] + (b[0] - a[0]) * t / seg, a[1] + (b[1] - a[1]) * t / seg];
                self.dab(s.tool, p, r, strength, [p[0] - last[0], p[1] - last[1]]);
                last = p;
                dabs += 1;
                t += step;
            }
            carry = seg - (t - step);
        }
    }

    /// As the GPU's table (see the module notes).
    pub fn table(&self, id: u64) -> Table {
        let mut rgba = Vec::with_capacity(self.shift.len() * 4);
        for s in &self.shift {
            for v in s {
                let q = (v * 8.0).round().clamp(-32768.0, 32767.0) as i32 + 32768;
                rgba.push((q >> 8) as u8);
                rgba.push((q & 255) as u8);
            }
        }
        Table { id, width: self.cols as u32, height: self.rows as u32, rgba, cube_size: 0 }
    }
}

/// The warp of `strokes` over a layer `size` pixels across.
pub fn field(strokes: &[LiquifyStroke], size: [f32; 2]) -> Field {
    let mut f = Field::new(size);
    for s in strokes {
        f.stroke(s);
    }
    f
}

/// Recently worked out tables (strokes and size → table), so a frame doesn't
/// redo the strokes each time it's drawn.
static CACHE: Mutex<Option<HashMap<u64, Arc<Table>>>> = Mutex::new(None);

/// The strokes' warp as the GPU's table, and its grid spacing (`None`:
/// nothing moves).
pub fn table(strokes: &[LiquifyStroke], size: [f32; 2]) -> Option<(Arc<Table>, f32)> {
    if strokes.is_empty() {
        return None;
    }
    let mut h = std::collections::hash_map::DefaultHasher::new();
    size[0].to_bits().hash(&mut h);
    size[1].to_bits().hash(&mut h);
    for s in strokes {
        (s.tool as u8, s.radius.to_bits(), s.pressure.to_bits(), s.points.len()).hash(&mut h);
        for p in &s.points {
            (p[0].to_bits(), p[1].to_bits()).hash(&mut h);
        }
    }
    let key = h.finish();
    let cell = Field::new(size).cell;
    if let Some(t) = CACHE.lock().unwrap_or_else(PoisonError::into_inner).as_ref().and_then(|m| m.get(&key)).cloned() {
        return (t.width > 0).then_some((t, cell));
    }
    let f = field(strokes, size);
    let t = Arc::new(if f.is_empty() { Table { id: key, width: 0, height: 0, rgba: Vec::new(), cube_size: 0 } } else { f.table(key) });
    let mut cache = CACHE.lock().unwrap_or_else(PoisonError::into_inner);
    let m = cache.get_or_insert_with(HashMap::new);
    if m.len() >= 16 {
        m.clear();
    }
    m.insert(key, t.clone());
    (t.width > 0).then_some((t, cell))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stroke(tool: Tool, points: Vec<[f32; 2]>) -> LiquifyStroke {
        LiquifyStroke { tool, points, radius: 40.0, pressure: 100.0 }
    }

    #[test]
    fn a_push_drags_the_picture_along() {
        // Pushing right through the middle: the picture there comes from the left.
        let f = field(&[stroke(Tool::Push, vec![[60.0, 100.0], [140.0, 100.0]])], [200.0, 200.0]);
        let s = f.at([140.0, 100.0]);
        assert!(s[0] < -20.0 && s[1].abs() < 2.0, "takes from the left: {s:?}");
        // Far from the stroke: nothing.
        assert_eq!(f.at([100.0, 10.0]), [0.0, 0.0]);
    }

    #[test]
    fn twirl_turns_pucker_pinches_bloat_swells() {
        let held = |tool| field(&[stroke(tool, vec![[100.0, 100.0]; 10])], [200.0, 200.0]);
        // A point right of the middle takes the picture from round the circle: above
        // for clockwise (y down: from the top), below for anticlockwise.
        let cw = held(Tool::TwirlClockwise).at([115.0, 100.0]);
        let ccw = held(Tool::TwirlAnticlockwise).at([115.0, 100.0]);
        assert!(cw[1] < -1.0 && ccw[1] > 1.0, "{cw:?} {ccw:?}");
        // Pucker takes from further out (things shrink in); bloat from nearer in.
        assert!(held(Tool::Pucker).at([115.0, 100.0])[0] > 1.0);
        assert!(held(Tool::Bloat).at([115.0, 100.0])[0] < -1.0);
        // The middle itself stays put.
        let m = held(Tool::Bloat).at([100.0, 100.0]);
        assert!(m[0].abs() < 0.5 && m[1].abs() < 0.5, "{m:?}");
    }

    #[test]
    fn reconstruct_takes_it_back() {
        let push = stroke(Tool::Push, vec![[60.0, 100.0], [140.0, 100.0]]);
        let before = field(std::slice::from_ref(&push), [200.0, 200.0]).at([120.0, 100.0])[0].abs();
        let back = stroke(Tool::Reconstruct, vec![[120.0, 100.0]; 30]);
        let after = field(&[push, back], [200.0, 200.0]).at([120.0, 100.0])[0].abs();
        assert!(after < before * 0.1, "{before} → {after}");
    }

    #[test]
    fn hostile_strokes_do_no_harm() {
        let bad = [
            LiquifyStroke { tool: Tool::Push, points: vec![[f32::NAN, 1.0], [f32::INFINITY, 2.0]], radius: 10.0, pressure: 100.0 },
            LiquifyStroke { tool: Tool::Bloat, points: vec![[1e30, -1e30]], radius: f32::MAX, pressure: 100.0 },
            LiquifyStroke { tool: Tool::Push, points: vec![[0.0, 0.0], [1e9, 1e9]], radius: 1e9, pressure: 1e9 },
            LiquifyStroke { tool: Tool::TwirlClockwise, points: vec![], radius: -5.0, pressure: f32::NAN },
        ];
        for size in [[0.0, 0.0], [f32::NAN, 5.0], [1e9, 1e9], [100.0, 100.0]] {
            let f = field(&bad, size);
            assert!(f.cols <= MAX_GRID && f.rows <= MAX_GRID);
            assert!(f.shift.iter().flatten().all(|v| v.is_finite() && v.abs() <= MAX_SHIFT));
            let _ = table(&bad, size);
        }
    }

    #[test]
    fn the_table_holds_the_offsets() {
        let f = field(&[stroke(Tool::Push, vec![[60.0, 100.0], [140.0, 100.0]])], [200.0, 200.0]);
        let t = f.table(1);
        assert_eq!((t.width as usize, t.height as usize, t.rgba.len()), (f.cols, f.rows, f.cols * f.rows * 4));
        let i = f.cols * 50 + 70; // grid point (70, 50) at 2 px: (140, 100)
        let q = |k: usize| ((t.rgba[i * 4 + k] as i32) << 8 | t.rgba[i * 4 + k + 1] as i32) - 32768;
        let s = f.shift[i];
        assert!((q(0) as f32 / 8.0 - s[0]).abs() <= 0.07 && (q(2) as f32 / 8.0 - s[1]).abs() <= 0.07);
    }
}
