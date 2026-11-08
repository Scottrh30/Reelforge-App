//! Lightning: jagged, branching bolts between two points (or all round
//! one), struck afresh several times a second and flickering.
//!
//! The bolts are worked out here (midpoint displacement, with branches
//! forking off and forking again) as a list of line segments; the GPU then
//! draws each pixel's distance to them as a hot core and a glow. The list
//! goes to the GPU as a small table: row 0 holds the segments (three texels
//! each: start, end, width and brightness, positions as 16-bit fractions of
//! twice the layer, so bolts can run a layer's width past its edges), row 1
//! a box round each run of [`CHUNK`] segments, so a pixel far from a run
//! skips it.

use crate::effects::{lightning as p, Effect, EffectKind};
use crate::lut::Table;
use crate::time::Time;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

/// Segments in a run (one box).
pub const CHUNK: usize = 8;
/// At most this many segments are drawn.
pub const MAX_SEGMENTS: usize = 2400;

/// One straight piece of a bolt, in layer pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Segment {
    pub a: [f32; 2],
    pub b: [f32; 2],
    /// Width, relative to the core width (thinner along branches and towards the tip).
    pub width: f32,
    /// 0–1.
    pub bright: f32,
}

/// The bolts at one moment, and how bright the strike is (0–1).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Bolts {
    pub segments: Vec<Segment>,
    pub flash: f32,
}

/// splitmix64.
fn mix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> f32 {
        self.0 = mix(self.0);
        (self.0 >> 40) as f32 / (1u64 << 24) as f32
    }
    /// -1..1.
    fn signed(&mut self) -> f32 {
        self.next() * 2.0 - 1.0
    }
}

/// A jagged path from `a` to `b`: `levels` rounds of moving each midpoint
/// sideways by up to `jag` of the piece's length.
fn jagged(r: &mut Rng, a: [f32; 2], b: [f32; 2], levels: u32, jag: f32) -> Vec<[f32; 2]> {
    let mut pts = vec![a, b];
    let mut amount = jag;
    for _ in 0..levels {
        let mut next = Vec::with_capacity(pts.len() * 2);
        for w in pts.windows(2) {
            let (p, q) = (w[0], w[1]);
            let d = [q[0] - p[0], q[1] - p[1]];
            let len = (d[0] * d[0] + d[1] * d[1]).sqrt();
            let n = if len > 0.0 { [-d[1] / len, d[0] / len] } else { [0.0, 0.0] };
            let off = r.signed() * amount * len * 0.5;
            let along = 0.5 + r.signed() * 0.12;
            next.push(p);
            next.push([p[0] + d[0] * along + n[0] * off, p[1] + d[1] * along + n[1] * off]);
        }
        next.push(*pts.last().unwrap());
        pts = next;
        // Finer kinks are smaller, as in real lightning.
        amount *= 0.92;
    }
    pts
}

struct Shape {
    levels: u32,
    jag: f32,
    branches: f32,
    branch_len: f32,
    grow: f32,
}

/// A bolt from `a` to `b` (its branches too), as much of it as has grown.
#[allow(clippy::too_many_arguments)]
fn bolt(out: &mut Vec<Segment>, r: &mut Rng, s: &Shape, a: [f32; 2], b: [f32; 2], width: f32, bright: f32, depth: u32, grow: f32) {
    let levels = s.levels.saturating_sub(depth * 2).max(2);
    let pts = jagged(r, a, b, levels, s.jag);
    let n = pts.len() - 1;
    let shown = ((n as f32) * grow.clamp(0.0, 1.0)).ceil() as usize;
    // Branches: about `branches` × 6 along a main bolt, fewer on branches.
    let chance = s.branches * 6.0 / n as f32 / (1 + depth * 2) as f32;
    let main_len = ((b[0] - a[0]).powi(2) + (b[1] - a[1]).powi(2)).sqrt();
    for i in 0..shown.min(n) {
        if out.len() >= MAX_SEGMENTS {
            return;
        }
        let along = i as f32 / n as f32;
        // Thinner towards the tip.
        let w = width * (1.0 - 0.45 * along);
        out.push(Segment { a: pts[i], b: pts[i + 1], width: w, bright });
        if depth < 3 && i > 0 && r.next() < chance {
            let d = [pts[i + 1][0] - pts[i][0], pts[i + 1][1] - pts[i][1]];
            let main_dir = [b[0] - a[0], b[1] - a[1]];
            // Mostly onwards, turned off to one side.
            let base = (main_dir[1]).atan2(main_dir[0]) * 0.7 + d[1].atan2(d[0]) * 0.3;
            let turn = (0.35 + 0.5 * r.next()) * if r.next() < 0.5 { -1.0 } else { 1.0 };
            let len = main_len * (1.0 - along) * s.branch_len * (0.35 + 0.65 * r.next());
            if len < 4.0 {
                continue;
            }
            let ang = base + turn;
            let end = [pts[i][0] + ang.cos() * len, pts[i][1] + ang.sin() * len];
            // A branch grows as the bolt does, from where it forks.
            let left = if grow >= 1.0 { 1.0 } else { ((grow - along) / (1.0 - along).max(1e-3)).clamp(0.0, 1.0) };
            bolt(out, r, s, pts[i], end, w * 0.6, bright * 0.7, depth + 1, left);
        }
    }
}

/// The bolts of lightning effect `e` on a `size` layer at time `t`.
pub fn bolts(e: &Effect, t: Time, size: [f32; 2]) -> Bolts {
    if e.kind != EffectKind::Lightning || e.params.len() <= p::SEED {
        return Bolts::default();
    }
    let v = |i| e.value_at(i, t);
    let secs = t.as_secs_f64();
    let rate = v(p::RATE).max(0.0) as f64;
    // Which strike it is, and how far through it.
    let (strike, phase) = if rate > 0.0 { ((secs * rate).floor() as i64, (secs * rate).fract() as f32) } else { (0, 0.0) };
    let seed = mix(v(p::SEED) as u64 ^ 0x5eed_1197) ^ mix(strike as u64);
    let mut r = Rng(seed);
    let flicker = (v(p::FLICKER) / 100.0).clamp(0.0, 1.0);
    // Each strike flashes at its own brightness and dies away towards the next.
    let flash = (1.0 - flicker * (0.55 * r.next() + 0.45 * phase)).clamp(0.0, 1.0);
    let at = |x: usize, y: usize| [v(x) / 100.0 * size[0], v(y) / 100.0 * size[1]];
    let (start, end) = (at(p::START_X, p::START_Y), at(p::END_X, p::END_Y));
    let shape = Shape {
        levels: v(p::DETAIL).round().clamp(2.0, 10.0) as u32,
        jag: (v(p::JAGGED) / 100.0).clamp(0.0, 3.0),
        branches: (v(p::BRANCHES) / 100.0).clamp(0.0, 3.0),
        branch_len: (v(p::BRANCH_LENGTH) / 100.0).clamp(0.0, 2.0),
        grow: (v(p::GROW) / 100.0).clamp(0.0, 1.0),
    };
    let count = v(p::COUNT).round().clamp(1.0, 24.0) as usize;
    let reach = ((end[0] - start[0]).powi(2) + (end[1] - start[1]).powi(2)).sqrt();
    let mut segments = Vec::new();
    for k in 0..count {
        let b = if v(p::TYPE).round() as i32 == 1 {
            // A ball: bolts all round the start, out to about the end's distance.
            let ang = (k as f32 + r.next() * 0.8) / count as f32 * std::f32::consts::TAU;
            let len = reach * (0.55 + 0.45 * r.next());
            [start[0] + ang.cos() * len, start[1] + ang.sin() * len]
        } else {
            end
        };
        let bright = if k == 0 { 1.0 } else { 0.55 + 0.45 * r.next() };
        bolt(&mut segments, &mut r, &shape, start, b, 1.0, bright, 0, shape.grow);
    }
    Bolts { segments, flash }
}

/// The bolts as the GPU's table (see the module notes), or `None` if there are none.
pub fn table(b: &Bolts, size: [f32; 2]) -> Option<Arc<Table>> {
    let n = b.segments.len();
    if n == 0 {
        return None;
    }
    let chunks = n.div_ceil(CHUNK);
    let width = (3 * n).max(2 * chunks);
    let mut rgba = vec![0u8; width * 2 * 4];
    let q = |v: f32, s: f32| -> [u8; 2] {
        // Layer pixels → a 16-bit fraction of −½ to 1½ layers.
        let f = ((v / s.max(1.0) + 0.5) / 2.0).clamp(0.0, 1.0);
        let u = (f * 65535.0).round() as u16;
        u.to_be_bytes()
    };
    let pt = |p: [f32; 2]| -> [u8; 4] {
        let (x, y) = (q(p[0], size[0]), q(p[1], size[1]));
        [x[0], x[1], y[0], y[1]]
    };
    for (i, s) in b.segments.iter().enumerate() {
        let o = i * 12;
        rgba[o..o + 4].copy_from_slice(&pt(s.a));
        rgba[o + 4..o + 8].copy_from_slice(&pt(s.b));
        rgba[o + 8] = (s.width.clamp(0.0, 1.0) * 255.0).round() as u8;
        rgba[o + 9] = (s.bright.clamp(0.0, 1.0) * 255.0).round() as u8;
    }
    let row = width * 4;
    for c in 0..chunks {
        let run = &b.segments[c * CHUNK..((c + 1) * CHUNK).min(n)];
        let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
        for s in run {
            for p in [s.a, s.b] {
                lo = [lo[0].min(p[0]), lo[1].min(p[1])];
                hi = [hi[0].max(p[0]), hi[1].max(p[1])];
            }
        }
        let o = row + c * 8;
        rgba[o..o + 4].copy_from_slice(&pt(lo));
        rgba[o + 4..o + 8].copy_from_slice(&pt(hi));
    }
    let mut h = std::collections::hash_map::DefaultHasher::new();
    rgba.hash(&mut h);
    Some(Arc::new(Table { id: h.finish(), width: width as u32, height: 2, rgba, cube_size: 0 }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with(set: &[(usize, f32)]) -> Effect {
        let mut e = Effect::new(EffectKind::Lightning);
        for &(i, v) in set {
            e.params[i].value = v;
        }
        e
    }

    #[test]
    fn a_bolt_runs_from_start_to_end_and_branches() {
        let size = [640.0, 360.0];
        let b = bolts(&with(&[(p::BRANCHES, 0.0)]), Time::ZERO, size);
        let first = b.segments.first().unwrap().a;
        let last = b.segments.last().unwrap().b;
        assert!((first[0] - 320.0).abs() < 0.01 && (first[1] - 18.0).abs() < 0.01);
        assert!((last[0] - 320.0).abs() < 0.01 && (last[1] - 342.0).abs() < 0.01);
        // Joined up, and jagged: it strays sideways.
        assert!(b.segments.windows(2).all(|w| w[0].b == w[1].a));
        assert!(b.segments.iter().any(|s| (s.a[0] - 320.0).abs() > 8.0));
        let branched = bolts(&with(&[(p::BRANCHES, 100.0)]), Time::ZERO, size);
        assert!(branched.segments.len() > b.segments.len());
        // The same moment, the same bolt; the next strike, another.
        assert_eq!(bolts(&with(&[]), Time::ZERO, size), bolts(&with(&[]), Time::ZERO, size));
        assert_ne!(bolts(&with(&[]), Time::ZERO, size).segments, bolts(&with(&[]), Time::from_secs_f64(0.5), size).segments);
        // With no strikes a second it holds still.
        let still = |s: f64| bolts(&with(&[(p::RATE, 0.0), (p::FLICKER, 0.0)]), Time::from_secs_f64(s), size);
        assert_eq!(still(0.0), still(3.0));
    }

    #[test]
    fn growing_shows_part_of_the_bolt_and_a_ball_goes_all_round() {
        let size = [640.0, 360.0];
        let full = bolts(&with(&[(p::BRANCHES, 0.0)]), Time::ZERO, size).segments.len();
        let half = bolts(&with(&[(p::BRANCHES, 0.0), (p::GROW, 50.0)]), Time::ZERO, size).segments.len();
        assert!(half * 2 >= full - 1 && half * 2 <= full + 1, "{half} of {full}");
        let ball = bolts(&with(&[(p::TYPE, 1.0), (p::COUNT, 8.0), (p::BRANCHES, 0.0), (p::START_Y, 50.0), (p::END_Y, 90.0)]), Time::ZERO, size);
        let ends: Vec<[f32; 2]> = ball.segments.iter().map(|s| s.b).collect();
        assert!(ends.iter().any(|p| p[1] < 120.0) && ends.iter().any(|p| p[1] > 240.0));
        assert!(ends.iter().any(|p| p[0] < 200.0) && ends.iter().any(|p| p[0] > 440.0));
        let t = table(&ball, size).unwrap();
        assert_eq!(t.height, 2);
        assert_eq!(t.width as usize, 3 * ball.segments.len());
    }
}
