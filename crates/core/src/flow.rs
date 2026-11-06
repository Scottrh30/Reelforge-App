//! Optical flow for retiming: how the picture moves from one frame to the
//! next, measured at the points of a grid, so the frames either side of a
//! moment between them can be pushed along that motion and cross-faded
//! ("optical flow" frame blending, see `FrameBlend::Flow`).
//!
//! Block matching, coarse to fine: both frames are shrunk to grey pictures at
//! most [`WORK`] pixels across, then for each grid point the patch round it
//! in the first frame is looked for in the second, first in a quarter-size
//! copy (so large motion is found), then refined at half and full size. The
//! grid of motions is then median-filtered to drop odd ones out.

/// The working picture's longer side.
pub const WORK: usize = 320;
/// Half the patch matched (pixels at each level).
const PATCH: i32 = 5;
/// How far each level searches round its starting guess.
const SEARCH: i32 = 4;

struct Grey {
    w: usize,
    h: usize,
    v: Vec<f32>,
}

impl Grey {
    /// RGBA8 (`w`×`h`) shrunk by box-averaging to at most `max` across.
    fn from_rgba(rgba: &[u8], w: usize, h: usize, max: usize) -> Grey {
        let k = (w.max(h) as f32 / max as f32).max(1.0);
        let (ow, oh) = (((w as f32 / k).round() as usize).max(1), ((h as f32 / k).round() as usize).max(1));
        let mut sum = vec![0.0f32; ow * oh];
        let mut n = vec![0.0f32; ow * oh];
        for y in 0..h {
            let oy = (y * oh / h).min(oh - 1);
            for x in 0..w {
                let ox = (x * ow / w).min(ow - 1);
                let i = (y * w + x) * 4;
                let g = 0.299 * rgba[i] as f32 + 0.587 * rgba[i + 1] as f32 + 0.114 * rgba[i + 2] as f32;
                sum[oy * ow + ox] += g;
                n[oy * ow + ox] += 1.0;
            }
        }
        Grey { w: ow, h: oh, v: sum.iter().zip(&n).map(|(s, n)| s / n.max(1.0)).collect() }
    }

    fn half(&self) -> Grey {
        let (w, h) = ((self.w / 2).max(1), (self.h / 2).max(1));
        let at = |x: usize, y: usize| self.v[y.min(self.h - 1) * self.w + x.min(self.w - 1)];
        let v = (0..h).flat_map(|y| (0..w).map(move |x| (x, y))).map(|(x, y)| (at(2 * x, 2 * y) + at(2 * x + 1, 2 * y) + at(2 * x, 2 * y + 1) + at(2 * x + 1, 2 * y + 1)) * 0.25).collect();
        Grey { w, h, v }
    }

    fn at(&self, x: i32, y: i32) -> f32 {
        self.v[(y.clamp(0, self.h as i32 - 1) as usize) * self.w + x.clamp(0, self.w as i32 - 1) as usize]
    }
}

/// How unlike the patch round `p` in `a` is the one round `p + d` in `b`.
fn cost(a: &Grey, b: &Grey, p: (i32, i32), d: (i32, i32)) -> f32 {
    let mut s = 0.0;
    for dy in -PATCH..=PATCH {
        for dx in -PATCH..=PATCH {
            s += (a.at(p.0 + dx, p.1 + dy) - b.at(p.0 + dx + d.0, p.1 + dy + d.1)).abs();
        }
    }
    // A slight preference for small motion, so flat areas stay put.
    s + 0.05 * (d.0.abs() + d.1.abs()) as f32 * (2 * PATCH + 1) as f32
}

/// The motion from frame `a` to frame `b` (both RGBA8, `w`×`h`) at the
/// `(cols + 1) × (rows + 1)` points of a grid over the picture, row by row
/// from the top left, in the pictures' pixels: what's at grid point `p` in
/// `a` is at `p + motion` in `b`.
pub fn grid_flow(a: &[u8], b: &[u8], w: u32, h: u32, cols: u8, rows: u8) -> Vec<[f32; 2]> {
    let (w, h) = (w as usize, h as usize);
    let (cols, rows) = (cols.max(1) as usize, rows.max(1) as usize);
    let n = (cols + 1) * (rows + 1);
    if w < 4 || h < 4 || a.len() < w * h * 4 || b.len() < w * h * 4 {
        return vec![[0.0; 2]; n];
    }
    let ga = Grey::from_rgba(a, w, h, WORK);
    let gb = Grey::from_rgba(b, w, h, WORK);
    let pyr_a = [ga.half().half(), ga.half(), ga];
    let pyr_b = [gb.half().half(), gb.half(), gb];
    let mut d: Vec<(i32, i32)> = vec![(0, 0); n];
    for (level, (la, lb)) in pyr_a.iter().zip(&pyr_b).enumerate() {
        for j in 0..=rows {
            for i in 0..=cols {
                let k = j * (cols + 1) + i;
                let p = ((i * (la.w - 1) / cols) as i32, (j * (la.h - 1) / rows) as i32);
                let start = if level == 0 { (0, 0) } else { (d[k].0 * 2, d[k].1 * 2) };
                let mut best = (start, cost(la, lb, p, start));
                for sy in -SEARCH..=SEARCH {
                    for sx in -SEARCH..=SEARCH {
                        let c = (start.0 + sx, start.1 + sy);
                        let v = cost(la, lb, p, c);
                        if v < best.1 {
                            best = (c, v);
                        }
                    }
                }
                d[k] = best.0;
            }
        }
    }
    // Median of each point's 3 × 3 neighbourhood, per axis.
    let get = |i: i32, j: i32| d[(j.clamp(0, rows as i32) as usize) * (cols + 1) + i.clamp(0, cols as i32) as usize];
    let scale = [w as f32 / pyr_a[2].w as f32, h as f32 / pyr_a[2].h as f32];
    let mut out = Vec::with_capacity(n);
    for j in 0..=rows as i32 {
        for i in 0..=cols as i32 {
            let mut xs = Vec::with_capacity(9);
            let mut ys = Vec::with_capacity(9);
            for dj in -1..=1 {
                for di in -1..=1 {
                    let v = get(i + di, j + dj);
                    xs.push(v.0);
                    ys.push(v.1);
                }
            }
            xs.sort_unstable();
            ys.sort_unstable();
            out.push([xs[4] as f32 * scale[0], ys[4] as f32 * scale[1]]);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A picture with a bright textured square at `at` on a darker textured background.
    fn picture(w: usize, h: usize, at: (usize, usize)) -> Vec<u8> {
        let mut v = Vec::with_capacity(w * h * 4);
        for y in 0..h {
            for x in 0..w {
                let inside = x >= at.0 && x < at.0 + 120 && y >= at.1 && y < at.1 + 120;
                let tex = (((x - if inside { at.0 } else { 0 }) / 7 + (y - if inside { at.1 } else { 0 }) / 5) % 3) as u8 * 30;
                let g = if inside { 150 + tex } else { 20 + tex / 3 };
                v.extend([g, g, g, 255]);
            }
        }
        v
    }

    /// A square moved 24 px right and 12 down is found to have; the
    /// background (not moved) stays still.
    #[test]
    fn finds_how_far_things_moved() {
        let (w, h) = (640usize, 360usize);
        let a = picture(w, h, (200, 100));
        let b = picture(w, h, (224, 112));
        let flow = grid_flow(&a, &b, w as u32, h as u32, 16, 9);
        assert_eq!(flow.len(), 17 * 10);
        // Grid point (6, 4) is at (240, 160): inside the square.
        let m = flow[4 * 17 + 6];
        assert!((m[0] - 24.0).abs() < 4.5 && (m[1] - 12.0).abs() < 4.5, "inside: {m:?}");
        // (1, 1) is at (40, 40): background.
        let s = flow[17 + 1];
        assert!(s[0].abs() < 2.5 && s[1].abs() < 2.5, "background: {s:?}");
        assert!(grid_flow(&a, &b, 2, 2, 4, 4).iter().all(|m| *m == [0.0; 2]));
    }
}
