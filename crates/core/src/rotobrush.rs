//! Roto brush: paint over a subject (and, with Alt, over what isn't it) and
//! its outline is found, then followed through the shot.
//!
//! Each frame is cut into subject and background by a graph cut (as
//! GrabCut does): every pixel weighs how well its colour fits the subject's
//! colours against the background's (each a handful of colour clusters,
//! learnt from the strokes and refined from the cut), against keeping
//! neighbours together except across edges in the picture; the cheapest
//! split is a minimum cut, found by max flow ([`segment`]). The cut's outline
//! becomes a mask shape ([`outline`]). In the next frame the outline is
//! carried along by following points on it, and the cut redone in a band
//! round it, so it snaps to the subject's edges again.

use crate::inpaint::{dilate, Image};

/// A brush stroke, in layer pixels.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Stroke {
    /// Over the subject (true) or over what isn't (false).
    pub fg: bool,
    pub radius: f32,
    pub points: Vec<[f32; 2]>,
}

/// Paint `strokes` (scaled by `k` into the picture's pixels) onto a `w`×`h` grid.
pub fn paint(strokes: &[Stroke], k: f32, w: usize, h: usize, fg: bool) -> Vec<bool> {
    let mut out = vec![false; w * h];
    for s in strokes.iter().filter(|s| s.fg == fg) {
        let r = (s.radius * k).max(1.0);
        let pts: Vec<[f32; 2]> = s.points.iter().map(|p| [p[0] * k, p[1] * k]).collect();
        let segs: Vec<([f32; 2], [f32; 2])> = if pts.len() == 1 { vec![(pts[0], pts[0])] } else { pts.windows(2).map(|w| (w[0], w[1])).collect() };
        for (a, b) in segs {
            let (x0, x1) = ((a[0].min(b[0]) - r).floor().max(0.0) as usize, ((a[0].max(b[0]) + r).ceil() as usize).min(w));
            let (y0, y1) = ((a[1].min(b[1]) - r).floor().max(0.0) as usize, ((a[1].max(b[1]) + r).ceil() as usize).min(h));
            for y in y0..y1 {
                for x in x0..x1 {
                    let p = [x as f32 + 0.5, y as f32 + 0.5];
                    let ab = [b[0] - a[0], b[1] - a[1]];
                    let l2 = ab[0] * ab[0] + ab[1] * ab[1];
                    let t = if l2 > 0.0 { (((p[0] - a[0]) * ab[0] + (p[1] - a[1]) * ab[1]) / l2).clamp(0.0, 1.0) } else { 0.0 };
                    let q = [a[0] + ab[0] * t - p[0], a[1] + ab[1] * t - p[1]];
                    if q[0] * q[0] + q[1] * q[1] <= r * r {
                        out[y * w + x] = true;
                    }
                }
            }
        }
    }
    out
}

/// A colour model: a few clusters (k-means), each with its mean, spread and share.
struct Colours {
    clusters: Vec<([f32; 3], [f32; 3], f32)>,
}

impl Colours {
    fn learn(px: &[[f32; 3]], k: usize) -> Option<Colours> {
        if px.is_empty() {
            return None;
        }
        // Spread the samples out (at most 4000 used).
        let step = (px.len() / 4000).max(1);
        let s: Vec<[f32; 3]> = px.iter().step_by(step).copied().collect();
        let k = k.min(s.len());
        let mut means: Vec<[f32; 3]> = (0..k).map(|i| s[i * s.len() / k]).collect();
        let mut assign = vec![0usize; s.len()];
        for _ in 0..8 {
            for (i, p) in s.iter().enumerate() {
                assign[i] = (0..k).min_by(|&a, &b| d2(p, &means[a]).total_cmp(&d2(p, &means[b]))).unwrap_or(0);
            }
            for (c, m) in means.iter_mut().enumerate() {
                let (mut sum, mut n) = ([0.0f32; 3], 0.0f32);
                for (i, p) in s.iter().enumerate().filter(|(i, _)| assign[*i] == c) {
                    let _ = i;
                    for j in 0..3 {
                        sum[j] += p[j];
                    }
                    n += 1.0;
                }
                if n > 0.0 {
                    *m = sum.map(|v| v / n);
                }
            }
        }
        let mut clusters = Vec::new();
        for (c, m) in means.iter().enumerate() {
            let (mut var, mut n) = ([0.0f32; 3], 0.0f32);
            for (i, p) in s.iter().enumerate() {
                if assign[i] == c {
                    for j in 0..3 {
                        var[j] += (p[j] - m[j]).powi(2);
                    }
                    n += 1.0;
                }
            }
            if n > 0.0 {
                clusters.push((*m, var.map(|v| (v / n).max(25.0)), n / s.len() as f32));
            }
        }
        Some(Colours { clusters })
    }

    /// −log of how likely colour `p` is.
    fn cost(&self, p: &[f32; 3]) -> f32 {
        let mut best = 0.0f32;
        for (m, v, w) in &self.clusters {
            let e: f32 = (0..3).map(|j| (p[j] - m[j]).powi(2) / v[j]).sum::<f32>();
            let norm = (v[0] * v[1] * v[2]).sqrt();
            best += w * (-0.5 * e).exp() / norm;
        }
        -(best.max(1e-30)).ln()
    }
}

fn d2(a: &[f32; 3], b: &[f32; 3]) -> f32 {
    (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)
}

/// Max flow on a graph (Dinic's algorithm); after it, `cut_source_side`
/// says which nodes are on the source's side of a minimum cut.
pub struct Flow {
    head: Vec<u32>,
    to: Vec<u32>,
    next: Vec<u32>,
    cap: Vec<f32>,
}

const NONE: u32 = u32::MAX;

impl Flow {
    pub fn new(nodes: usize) -> Self {
        Flow { head: vec![NONE; nodes], to: Vec::new(), next: Vec::new(), cap: Vec::new() }
    }
    /// An edge `a → b` with capacity `ab`, and its reverse with `ba`.
    pub fn edge(&mut self, a: usize, b: usize, ab: f32, ba: f32) {
        for (from, to, c) in [(a, b, ab), (b, a, ba)] {
            self.to.push(to as u32);
            self.cap.push(c);
            self.next.push(self.head[from]);
            self.head[from] = (self.to.len() - 1) as u32;
        }
    }

    pub fn run(&mut self, s: usize, t: usize) -> f32 {
        let n = self.head.len();
        let mut total = 0.0;
        let mut level = vec![-1i32; n];
        let mut it = vec![NONE; n];
        let mut queue = Vec::with_capacity(n);
        loop {
            level.iter_mut().for_each(|l| *l = -1);
            level[s] = 0;
            queue.clear();
            queue.push(s as u32);
            let mut qi = 0;
            while qi < queue.len() {
                let u = queue[qi] as usize;
                qi += 1;
                let mut e = self.head[u];
                while e != NONE {
                    let v = self.to[e as usize] as usize;
                    if self.cap[e as usize] > 1e-6 && level[v] < 0 {
                        level[v] = level[u] + 1;
                        queue.push(v as u32);
                    }
                    e = self.next[e as usize];
                }
            }
            if level[t] < 0 {
                return total;
            }
            it.copy_from_slice(&self.head);
            // Blocking flow by depth-first search (kept as an explicit stack).
            loop {
                let mut path: Vec<u32> = Vec::new();
                let mut u = s;
                let pushed = loop {
                    if u == t {
                        let f = path.iter().map(|&e| self.cap[e as usize]).fold(f32::MAX, f32::min);
                        break Some(f);
                    }
                    let mut advanced = false;
                    while it[u] != NONE {
                        let e = it[u] as usize;
                        let v = self.to[e] as usize;
                        if self.cap[e] > 1e-6 && level[v] == level[u] + 1 {
                            path.push(e as u32);
                            u = v;
                            advanced = true;
                            break;
                        }
                        it[u] = self.next[e];
                    }
                    if !advanced {
                        // Dead end: back up and don't come here again.
                        level[u] = -1;
                        match path.pop() {
                            Some(e) => {
                                u = self.to[(e ^ 1) as usize] as usize;
                                it[u] = self.next[it[u] as usize];
                            }
                            None => break None,
                        }
                    }
                };
                let Some(f) = pushed else { break };
                for &e in &path {
                    self.cap[e as usize] -= f;
                    self.cap[(e ^ 1) as usize] += f;
                }
                total += f;
            }
        }
    }

    pub fn cut_source_side(&self, s: usize) -> Vec<bool> {
        let mut seen = vec![false; self.head.len()];
        let mut stack = vec![s];
        seen[s] = true;
        while let Some(u) = stack.pop() {
            let mut e = self.head[u];
            while e != NONE {
                let v = self.to[e as usize] as usize;
                if self.cap[e as usize] > 1e-6 && !seen[v] {
                    seen[v] = true;
                    stack.push(v);
                }
                e = self.next[e as usize];
            }
        }
        seen
    }
}

/// What to cut a frame by.
pub struct Hints<'a> {
    /// Surely the subject / surely not (painted, or from the last frame's outline).
    pub fg: &'a [bool],
    pub bg: &'a [bool],
    /// Where the subject was expected (0–1), if known: leans the cut that way.
    pub prior: Option<&'a [f32]>,
}

/// Cut `img` into the subject (true) and the rest.
pub fn segment(img: &Image, hints: &Hints) -> Vec<bool> {
    let (w, h) = (img.w, img.h);
    let n = w * h;
    // Contrast: how sharply neighbours differ on average sets how much an edge counts.
    let mut sum = 0.0f64;
    let mut cnt = 0.0f64;
    for y in 0..h {
        for x in 0..w {
            let p = &img.px[y * w + x];
            if x + 1 < w {
                sum += d2(p, &img.px[y * w + x + 1]) as f64;
                cnt += 1.0;
            }
            if y + 1 < h {
                sum += d2(p, &img.px[(y + 1) * w + x]) as f64;
                cnt += 1.0;
            }
        }
    }
    let beta = (1.0 / (2.0 * (sum / cnt.max(1.0)).max(1.0))) as f32;
    let lambda = 50.0f32;
    let mut label: Vec<bool> = (0..n).map(|i| hints.fg[i] || hints.prior.is_some_and(|p| p[i] >= 0.5)).collect();
    for round in 0..3 {
        let fgc: Vec<[f32; 3]> = (0..n).filter(|&i| label[i] && !hints.bg[i]).map(|i| img.px[i]).collect();
        // At first only what's surely background teaches its colours (the
        // rest of the subject would otherwise count as background too).
        let sure = round == 0 && hints.prior.is_none() && hints.bg.iter().any(|&b| b);
        let bgc: Vec<[f32; 3]> = (0..n).filter(|&i| if sure { hints.bg[i] } else { !label[i] && !hints.fg[i] }).map(|i| img.px[i]).collect();
        let (Some(mf), Some(mb)) = (Colours::learn(&fgc, 5), Colours::learn(&bgc, 5)) else { return label };
        let (s, t) = (n, n + 1);
        let mut g = Flow::new(n + 2);
        let big = 1e9f32;
        for i in 0..n {
            let (mut to_fg, mut to_bg) = if hints.fg[i] {
                (big, 0.0)
            } else if hints.bg[i] {
                (0.0, big)
            } else {
                // Source side = subject: s→i is paid when i ends up background.
                (mb.cost(&img.px[i]), mf.cost(&img.px[i]))
            };
            if let Some(p) = hints.prior {
                if !hints.fg[i] && !hints.bg[i] {
                    let q = p[i].clamp(0.02, 0.98);
                    to_fg += -(1.0 - q).ln() * 2.0;
                    to_bg += -q.ln() * 2.0;
                }
            }
            // Only the difference matters.
            let m = to_fg.min(to_bg);
            g.edge(s, i, to_fg - m, 0.0);
            g.edge(i, t, to_bg - m, 0.0);
        }
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                if x + 1 < w {
                    let c = lambda * (-beta * d2(&img.px[i], &img.px[i + 1])).exp();
                    g.edge(i, i + 1, c, c);
                }
                if y + 1 < h {
                    let c = lambda * (-beta * d2(&img.px[i], &img.px[i + w])).exp();
                    g.edge(i, i + w, c, c);
                }
            }
        }
        g.run(s, t);
        let side = g.cut_source_side(s);
        let next: Vec<bool> = (0..n).map(|i| side[i]).collect();
        let same = next == label;
        label = next;
        if same && round > 0 {
            break;
        }
    }
    label
}

/// Only the largest connected piece (4-connected), with holes in it filled.
pub fn largest(mask: &[bool], w: usize, h: usize) -> Vec<bool> {
    let mut comp = vec![u32::MAX; w * h];
    let mut best = (0usize, u32::MAX);
    let mut id = 0u32;
    for start in 0..w * h {
        if !mask[start] || comp[start] != u32::MAX {
            continue;
        }
        let mut stack = vec![start];
        comp[start] = id;
        let mut size = 0;
        while let Some(i) = stack.pop() {
            size += 1;
            let (x, y) = (i % w, i / w);
            for (nx, ny) in [(x.wrapping_sub(1), y), (x + 1, y), (x, y.wrapping_sub(1)), (x, y + 1)] {
                if nx < w && ny < h {
                    let j = ny * w + nx;
                    if mask[j] && comp[j] == u32::MAX {
                        comp[j] = id;
                        stack.push(j);
                    }
                }
            }
        }
        if size > best.0 {
            best = (size, id);
        }
        id += 1;
    }
    // Holes: what the outside can't reach.
    let keep: Vec<bool> = comp.iter().map(|&c| c == best.1).collect();
    let mut outside = vec![false; w * h];
    let mut stack: Vec<usize> = (0..w * h).filter(|&i| (i % w == 0 || i % w == w - 1 || i / w == 0 || i / w == h - 1) && !keep[i]).collect();
    for &i in &stack {
        outside[i] = true;
    }
    while let Some(i) = stack.pop() {
        let (x, y) = (i % w, i / w);
        for (nx, ny) in [(x.wrapping_sub(1), y), (x + 1, y), (x, y.wrapping_sub(1)), (x, y + 1)] {
            if nx < w && ny < h {
                let j = ny * w + nx;
                if !keep[j] && !outside[j] {
                    outside[j] = true;
                    stack.push(j);
                }
            }
        }
    }
    outside.iter().map(|&o| !o).collect()
}

/// The outline of the largest piece of `mask` (pixel corners, clockwise),
/// simplified to within `tol` pixels.
pub fn outline(mask: &[bool], w: usize, h: usize, tol: f32) -> Vec<[f32; 2]> {
    let m = largest(mask, w, h);
    let at = |x: isize, y: isize| x >= 0 && y >= 0 && (x as usize) < w && (y as usize) < h && m[y as usize * w + x as usize];
    // Start at the top-left pixel of the piece; walk its boundary (square tracing on corners).
    let Some(start) = (0..w * h).find(|&i| m[i]) else { return Vec::new() };
    let (sx, sy) = ((start % w) as isize, (start / w) as isize);
    // Corner-walk: the piece is on the right of the direction travelled.
    let mut pts = Vec::new();
    let (mut x, mut y) = (sx, sy);
    let mut dir = 0usize; // 0 right, 1 down, 2 left, 3 up
    let dirs = [(1isize, 0isize), (0, 1), (-1, 0), (0, -1)];
    let limit = 4 * (w + h) * 4 + w * h;
    for _ in 0..limit {
        pts.push([x as f32, y as f32]);
        // The two pixels ahead of corner (x, y) going `dir`: left-ahead and right-ahead.
        let (l, r) = match dir {
            0 => (at(x, y - 1), at(x, y)),
            1 => (at(x, y), at(x - 1, y)),
            2 => (at(x - 1, y), at(x - 1, y - 1)),
            _ => (at(x - 1, y - 1), at(x, y - 1)),
        };
        dir = if l {
            (dir + 3) % 4
        } else if r {
            dir
        } else {
            (dir + 1) % 4
        };
        x += dirs[dir].0;
        y += dirs[dir].1;
        if x == sx && y == sy {
            break;
        }
    }
    simplify(&pts, tol)
}

/// Douglas–Peucker on a closed outline.
pub fn simplify(pts: &[[f32; 2]], tol: f32) -> Vec<[f32; 2]> {
    if pts.len() < 4 {
        return pts.to_vec();
    }
    fn dp(p: &[[f32; 2]], tol: f32, out: &mut Vec<[f32; 2]>) {
        let (a, b) = (p[0], p[p.len() - 1]);
        let ab = [b[0] - a[0], b[1] - a[1]];
        let l = (ab[0] * ab[0] + ab[1] * ab[1]).sqrt().max(1e-6);
        let (mut far, mut fi) = (0.0, 0);
        for (i, q) in p.iter().enumerate().take(p.len() - 1).skip(1) {
            let d = ((q[0] - a[0]) * ab[1] - (q[1] - a[1]) * ab[0]).abs() / l;
            if d > far {
                (far, fi) = (d, i);
            }
        }
        if far > tol {
            dp(&p[..=fi], tol, out);
            dp(&p[fi..], tol, out);
        } else {
            out.push(a);
        }
    }
    // Split at the point furthest from the first.
    let Some(far) = (1..pts.len()).max_by(|&i, &j| d2f(pts[i], pts[0]).total_cmp(&d2f(pts[j], pts[0]))) else { return pts.to_vec() };
    let mut out = Vec::new();
    dp(&pts[..=far], tol, &mut out);
    let mut back: Vec<[f32; 2]> = pts[far..].to_vec();
    back.push(pts[0]);
    dp(&back, tol, &mut out);
    out
}

fn d2f(a: [f32; 2], b: [f32; 2]) -> f32 {
    (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)
}

/// Hints for a frame from where the subject was expected to be (`prior`,
/// true inside): sure inside and outside, past a band `band` pixels wide
/// either side of its edge, the cut free in the band. Strokes painted on
/// this frame (`fg`, `bg`) count too.
pub fn band_hints(prior: &[bool], w: usize, h: usize, band: usize, fg: &[bool], bg: &[bool]) -> (Vec<bool>, Vec<bool>, Vec<f32>) {
    let outside: Vec<bool> = prior.iter().map(|&p| !p).collect();
    let grown = dilate(prior, w, h, band);
    let shrunk_out = dilate(&outside, w, h, band);
    let sure_fg: Vec<bool> = (0..w * h).map(|i| (!shrunk_out[i] || fg[i]) && !bg[i]).collect();
    let sure_bg: Vec<bool> = (0..w * h).map(|i| (!grown[i] || bg[i]) && !fg[i]).collect();
    let p: Vec<f32> = prior.iter().map(|&b| if b { 0.75 } else { 0.25 }).collect();
    (sure_fg, sure_bg, p)
}

/// Hints for the first frame from strokes alone: the subject's strokes, and
/// as background the strokes so marked, the picture's edges and what's far
/// outside the strokes (well beyond their extent).
pub fn stroke_hints(fg: &[bool], bg: &[bool], w: usize, h: usize) -> (Vec<bool>, Vec<bool>) {
    let mut b = [usize::MAX, usize::MAX, 0, 0];
    for y in 0..h {
        for x in 0..w {
            if fg[y * w + x] {
                b = [b[0].min(x), b[1].min(y), b[2].max(x + 1), b[3].max(y + 1)];
            }
        }
    }
    if b[0] == usize::MAX {
        return (fg.to_vec(), bg.to_vec());
    }
    let (bw, bh) = (b[2] - b[0], b[3] - b[1]);
    // As far again as the strokes reach, the same each way (a stroke along
    // a subject says little about how wide it is), and a little more.
    let mx = bw.max(bh) + w.max(h) / 8;
    let my = mx;
    let (x0, y0, x1, y1) = (b[0].saturating_sub(mx), b[1].saturating_sub(my), (b[2] + mx).min(w), (b[3] + my).min(h));
    let sure_bg: Vec<bool> = (0..w * h)
        .map(|i| {
            let (x, y) = (i % w, i / w);
            !fg[i] && (bg[i] || x < x0 || y < y0 || x >= x1 || y >= y1 || ((x == 0 || y == 0 || x == w - 1 || y == h - 1) && !(x0 == 0 && x1 == w && y0 == 0 && y1 == h)))
        })
        .collect();
    (fg.to_vec(), sure_bg)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A noisy orange disc on a noisy blue-grey background.
    fn picture(w: usize, h: usize, c: [f32; 2], r: f32) -> Image {
        let mut s = 12345u64;
        let mut rnd = || {
            s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((s >> 33) as f32 / (1u64 << 31) as f32) * 30.0 - 15.0
        };
        let px = (0..w * h)
            .map(|i| {
                let (x, y) = ((i % w) as f32 + 0.5, (i / w) as f32 + 0.5);
                let inside = (x - c[0]).powi(2) + (y - c[1]).powi(2) < r * r;
                let n = rnd();
                if inside {
                    [220.0 + n, 130.0 + n, 40.0 + n]
                } else {
                    [70.0 + n, 90.0 + n, 120.0 + n]
                }
            })
            .collect();
        Image { w, h, px }
    }

    #[test]
    fn a_stroke_finds_the_whole_subject() {
        let (w, h) = (120, 90);
        let img = picture(w, h, [60.0, 45.0], 25.0);
        let strokes = [Stroke { fg: true, radius: 3.0, points: vec![[50.0, 45.0], [70.0, 45.0]] }];
        let fg = paint(&strokes, 1.0, w, h, true);
        let bg = paint(&strokes, 1.0, w, h, false);
        let (f, b) = stroke_hints(&fg, &bg, w, h);
        let cut = segment(&img, &Hints { fg: &f, bg: &b, prior: None });
        let wrong = (0..w * h)
            .filter(|&i| {
                let (x, y) = ((i % w) as f32 + 0.5, (i / w) as f32 + 0.5);
                let inside = (x - 60.0).powi(2) + (y - 45.0).powi(2) < 625.0;
                cut[i] != inside
            })
            .count();
        assert!(wrong < 60, "{wrong} pixels wrong");
        let line = outline(&cut, w, h, 0.8);
        assert!(line.len() >= 8 && line.len() < 120, "{} points", line.len());
        let xs = line.iter().map(|p| p[0]);
        let (lo, hi) = (xs.clone().fold(f32::MAX, f32::min), xs.fold(f32::MIN, f32::max));
        assert!((lo - 35.0).abs() < 2.5 && (hi - 85.0).abs() < 2.5, "{lo}..{hi}");
    }

    #[test]
    fn the_band_follows_a_moved_subject() {
        let (w, h) = (120, 90);
        // Last frame's outline, and the subject now 6 px to the right.
        let prior: Vec<bool> = (0..w * h).map(|i| ((i % w) as f32 + 0.5 - 60.0).powi(2) + ((i / w) as f32 + 0.5 - 45.0).powi(2) < 625.0).collect();
        let img = picture(w, h, [66.0, 45.0], 25.0);
        let none = vec![false; w * h];
        let (f, b, p) = band_hints(&prior, w, h, 10, &none, &none);
        let cut = segment(&img, &Hints { fg: &f, bg: &b, prior: Some(&p) });
        let line = outline(&cut, w, h, 0.8);
        let xs = line.iter().map(|q| q[0]);
        let (lo, hi) = (xs.clone().fold(f32::MAX, f32::min), xs.fold(f32::MIN, f32::max));
        assert!((lo - 41.0).abs() < 2.5 && (hi - 91.0).abs() < 2.5, "{lo}..{hi}");
    }

    #[test]
    fn max_flow_cuts_the_cheapest_edge() {
        // s → a (3), a → b (1), b → t (5): the flow is 1 and a stays with s.
        let mut g = Flow::new(4);
        g.edge(0, 1, 3.0, 0.0);
        g.edge(1, 2, 1.0, 0.0);
        g.edge(2, 3, 5.0, 0.0);
        assert!((g.run(0, 3) - 1.0).abs() < 1e-5);
        let side = g.cut_source_side(0);
        assert_eq!(side, vec![true, true, false, false]);
    }
}
