//! Shatter: a layer breaking into irregular pieces at a moment, thrown out
//! from an impact point. Like particles, nothing is simulated step by step:
//! each piece's path (its throw, gravity and spin) is worked out straight
//! from the time since the break, so any frame can be drawn on its own.
//!
//! The pieces are the cells of a jittered grid's Voronoi diagram. Which
//! piece a point of the picture belongs to is decided on the GPU (render
//! crate, the same hash); here each piece is known by its grid cell, and is
//! thrown from the cell's middle.

use crate::effects::{shatter as p, Effect, EffectKind};
use crate::time::Time;

/// One piece as the GPU draws it (12 floats, like a particle).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[repr(C)]
pub struct Piece {
    /// The cell's middle at rest, layer uv (0–1).
    pub centre: [f32; 2],
    /// How far it has moved, layer pixels.
    pub moved: [f32; 2],
    /// How far it has turned (radians), and how opaque it still is.
    pub angle: f32,
    pub alpha: f32,
    /// Its grid cell, and the grid's columns and rows.
    pub cell: [f32; 2],
    pub grid: [f32; 2],
    pub _pad: [f32; 2],
}

/// The pieces at one moment.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Pieces {
    pub id: u64,
    /// Whether the layer has broken yet (before, it's drawn whole).
    pub broken: bool,
    pub list: Vec<Piece>,
}

impl Pieces {
    pub fn as_floats(&self) -> &[f32] {
        // Safety: `Piece` is `repr(C)` and made only of f32s.
        unsafe { std::slice::from_raw_parts(self.list.as_ptr() as *const f32, self.list.len() * 12) }
    }
}

fn mix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

fn rnd(seed: u64, k: u64, j: u64) -> f32 {
    ((mix(seed ^ mix(k.wrapping_mul(8).wrapping_add(j))) >> 40) as f32) / (1u64 << 24) as f32
}

/// The grid for `across` pieces over a layer `size` (pixels): columns and rows.
pub fn grid(across: f32, size: [f32; 2]) -> (u32, u32) {
    let cols = across.round().clamp(2.0, 60.0) as u32;
    let rows = ((cols as f32 * size[1] / size[0].max(1.0)).round() as u32).clamp(1, 60);
    (cols, rows)
}

/// The pieces of shatter effect `e` on a layer `size` (pixels) at source time `t`.
pub fn simulate(e: &Effect, t: Time, size: [f32; 2]) -> Pieces {
    use std::hash::{Hash, Hasher};
    let ts = t.as_secs_f64() as f32;
    let v = |i: usize| e.value_at(i, t);
    let mut h = std::collections::hash_map::DefaultHasher::new();
    format!("{:?}", e.params).hash(&mut h);
    ts.to_bits().hash(&mut h);
    size.map(f32::to_bits).hash(&mut h);
    let id = h.finish();
    if e.kind != EffectKind::Shatter || e.params.len() <= p::SEED {
        return Pieces { id, broken: false, list: Vec::new() };
    }
    let tau = ts - v(p::AT);
    let (cols, rows) = grid(v(p::PIECES), size);
    let seed = mix(v(p::SEED) as u64 ^ 0x5A77);
    let impact = [v(p::X) / 100.0 * size[0], v(p::Y) / 100.0 * size[1]];
    let reach = (v(p::REACH) / 100.0 * size[0]).max(1.0);
    let (force, gravity, spin, vary, fade) = (v(p::FORCE), v(p::GRAVITY), v(p::SPIN).to_radians(), v(p::RANDOMNESS) / 100.0, v(p::FADE));
    let mut list = Vec::with_capacity((cols * rows) as usize);
    for j in 0..rows {
        for i in 0..cols {
            let k = (j * cols + i) as u64;
            let r = |n: u64| rnd(seed, k, n);
            let centre = [(i as f32 + 0.5) / cols as f32, (j as f32 + 0.5) / rows as f32];
            let c = [centre[0] * size[0], centre[1] * size[1]];
            let d = [c[0] - impact[0], c[1] - impact[1]];
            let dist = d[0].hypot(d[1]);
            // How hard it's hit: full at the impact, gone by the edge of its reach.
            let strength = (1.0 - (dist / reach).powf(1.5)).max(0.0) * (1.0 - vary * 0.6 * r(0));
            let mut piece = Piece { centre, moved: [0.0; 2], angle: 0.0, alpha: 1.0, cell: [i as f32, j as f32], grid: [cols as f32, rows as f32], _pad: [0.0; 2] };
            if tau > 0.0 && strength > 0.02 {
                let dir = if dist > 1e-3 { [d[0] / dist, d[1] / dist] } else { [0.0, -1.0] };
                // Thrown outwards, scattered a little either way.
                let a = (r(1) - 0.5) * vary * 1.4;
                let (ca, sa) = (a.cos(), a.sin());
                let throw = force * strength * (0.6 + 0.4 * r(2));
                let vel = [(dir[0] * ca - dir[1] * sa) * throw, (dir[0] * sa + dir[1] * ca) * throw];
                piece.moved = [vel[0] * tau, vel[1] * tau + 0.5 * gravity * tau * tau];
                piece.angle = spin * strength * (r(3) * 2.0 - 1.0) * tau;
                if fade > 0.0 {
                    piece.alpha = (1.0 - tau / fade).clamp(0.0, 1.0);
                }
            }
            if piece.alpha > 0.0 {
                list.push(piece);
            }
        }
    }
    Pieces { id, broken: tau > 0.0, list }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shatter(set: &[(usize, f32)]) -> Effect {
        let mut e = Effect::new(EffectKind::Shatter);
        for &(i, v) in set {
            e.params[i].value = v;
        }
        e
    }

    #[test]
    fn whole_until_it_breaks_then_thrown_from_the_impact() {
        let size = [1920.0, 1080.0];
        let e = shatter(&[(p::AT, 1.0), (p::PIECES, 10.0), (p::GRAVITY, 0.0), (p::REACH, 400.0), (p::FADE, 0.0)]);
        let before = simulate(&e, Time::from_secs_f64(0.5), size);
        assert!(!before.broken);
        assert_eq!(before.list.len(), 10 * 6);
        assert!(before.list.iter().all(|p| p.moved == [0.0; 2] && p.angle == 0.0));
        let after = simulate(&e, Time::from_secs_f64(1.5), size);
        assert!(after.broken);
        // Each piece moved away from the impact (the middle).
        for p in &after.list {
            let c = [p.centre[0] * size[0] - 960.0, p.centre[1] * size[1] - 540.0];
            assert!(c[0] * p.moved[0] + c[1] * p.moved[1] > 0.0, "{p:?}");
        }
        // Nearer the impact, thrown further.
        let far = |p: &Piece| (p.centre[0] - 0.5).hypot(p.centre[1] - 0.5);
        let dist = |p: &Piece| p.moved[0].hypot(p.moved[1]);
        let near = after.list.iter().min_by(|a, b| far(a).total_cmp(&far(b))).unwrap();
        let edge = after.list.iter().max_by(|a, b| far(a).total_cmp(&far(b))).unwrap();
        assert!(dist(near) > dist(edge));
        assert_eq!(after, simulate(&e, Time::from_secs_f64(1.5), size), "the same moment, the same pieces");
    }

    #[test]
    fn out_of_reach_stays_put_and_thrown_pieces_fade() {
        let size = [1000.0, 1000.0];
        let e = shatter(&[(p::AT, 0.0), (p::X, 0.0), (p::Y, 0.0), (p::REACH, 30.0), (p::FADE, 1.0), (p::PIECES, 10.0)]);
        let l = simulate(&e, Time::from_secs_f64(0.5), size).list;
        let corner = l.iter().find(|p| p.cell == [9.0, 9.0]).unwrap();
        assert_eq!((corner.moved, corner.alpha), ([0.0; 2], 1.0));
        let thrown = l.iter().find(|p| p.cell == [0.0, 0.0]).unwrap();
        assert!((thrown.alpha - 0.5).abs() < 1e-4);
        // Once faded they're gone; the rest stay.
        let later = simulate(&e, Time::from_secs_f64(2.0), size).list;
        assert!(later.len() < l.len() && later.iter().all(|p| p.moved == [0.0; 2]));
    }
}
