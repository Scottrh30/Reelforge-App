//! Disintegrate: a layer breaking up into particles over time, a ragged
//! front sweeping across it. Each point of the picture has its moment
//! ([`moment`]): when the front reaches it, it's gone from the picture
//! and a particle of its colour drifts away from there. The same moment is
//! worked out on the GPU (render crate, `fs_disintegrate`, the same noise)
//! to erase the picture, so the two meet exactly.
//!
//! Like the particle emitters, nothing is simulated step by step: a fixed
//! number of particles is scattered over the layer once (from the seed),
//! each born at its point's moment.

use crate::effects::{disintegrate as p, Effect, EffectKind};
use crate::particles::{Particle, Particles};
use crate::time::Time;

/// Integer hash (PCG), the same as the shaders'.
pub fn pcg(v: u32) -> u32 {
    let s = v.wrapping_mul(747796405).wrapping_add(2891336453);
    let w = ((s >> ((s >> 28) + 4)) ^ s).wrapping_mul(277803737);
    (w >> 22) ^ w
}

fn rnd3(a: u32, b: u32, c: u32) -> f32 {
    (pcg(a ^ pcg(b ^ pcg(c))) >> 8) as f32 / 16_777_216.0
}

/// Smooth value noise, 0..1 (as `vnoise` in the shaders).
pub fn vnoise(seed: u32, x: f32, y: f32) -> f32 {
    let (ix, iy) = (x.floor(), y.floor());
    let (fx, fy) = (x - ix, y - iy);
    let (ux, uy) = (fx * fx * (3.0 - 2.0 * fx), fy * fy * (3.0 - 2.0 * fy));
    let b = |v: f32| v as i32 as u32;
    let a = rnd3(seed, b(ix), b(iy));
    let bb = rnd3(seed, b(ix + 1.0), b(iy));
    let c = rnd3(seed, b(ix), b(iy + 1.0));
    let d = rnd3(seed, b(ix + 1.0), b(iy + 1.0));
    let top = a + (bb - a) * ux;
    let bottom = c + (d - c) * ux;
    top + (bottom - top) * uy
}

/// How far through the sweep (0–1) point `uv` of a `size` layer goes:
/// along `angle` (degrees, 0 = left to right, 90 = bottom to top), made
/// ragged by `rough` (0–1).
pub fn moment(uv: [f32; 2], size: [f32; 2], angle: f32, rough: f32, seed: u32) -> f32 {
    let a = angle.to_radians();
    let d = [a.cos(), -a.sin()];
    let proj = |x: f32, y: f32| x * d[0] + y * d[1];
    let corners = [proj(0.0, 0.0), proj(size[0], 0.0), proj(0.0, size[1]), proj(size[0], size[1])];
    let lo = corners.iter().copied().fold(f32::INFINITY, f32::min);
    let hi = corners.iter().copied().fold(f32::NEG_INFINITY, f32::max);
    let px = [uv[0] * size[0], uv[1] * size[1]];
    let along = (proj(px[0], px[1]) - lo) / (hi - lo).max(1e-3);
    let cell = size[1].max(1.0) / 6.0;
    let n = (vnoise(seed, px[0] / cell, px[1] / cell) * 2.0 + vnoise(seed + 1, px[0] / cell * 3.0, px[1] / cell * 3.0)) / 3.0;
    along * (1.0 - rough) + n * rough
}

/// The particles of disintegrate effect `e` on a `size` layer at source time `t`.
pub fn simulate(e: &Effect, t: Time, size: [f32; 2]) -> Particles {
    use std::hash::{Hash, Hasher};
    let ts = t.as_secs_f64() as f32;
    let v = |i: usize| e.value_at(i, t);
    let mut h = std::collections::hash_map::DefaultHasher::new();
    format!("{:?}", e.params).hash(&mut h);
    ts.to_bits().hash(&mut h);
    size.map(f32::to_bits).hash(&mut h);
    let id = h.finish();
    if e.kind != EffectKind::Disintegrate || e.params.len() <= p::SEED {
        return Particles { id, list: Vec::new() };
    }
    let (start, dur, rough) = (v(p::START), v(p::DURATION).max(0.0), v(p::ROUGH) / 100.0);
    let seed = v(p::SEED).max(0.0) as u32;
    let n = v(p::COUNT).round().clamp(0.0, crate::particles::MAX_ALIVE as f32) as u32;
    let (speed, dir, spread, life) = (v(p::SPEED), v(p::DRIFT).to_radians(), v(p::SPREAD).to_radians(), v(p::LIFETIME).max(0.01));
    let mut list = Vec::new();
    for k in 0..n {
        let r = |j: u32| rnd3(seed.wrapping_add(0x5EED), k, j);
        let uv = [r(0), r(1)];
        let born = start + dur * moment(uv, size, v(p::DIRECTION), rough, seed);
        let age = ts - born;
        let life_k = life * (0.6 + 0.4 * r(2));
        if age < 0.0 || age >= life_k {
            continue;
        }
        let a = dir + spread * (r(3) - 0.5);
        let s = speed * (0.4 + 0.6 * r(4));
        list.push(Particle {
            emit: [uv[0] * 100.0, uv[1] * 100.0],
            offset: [0.0; 2],
            velocity: [a.cos() * s, -a.sin() * s],
            age,
            life: life_k,
            size: 0.6 + 0.4 * r(5),
            seed: r(6),
            trail: [0.0; 2],
        });
    }
    Particles { id, list }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_front_sweeps_across_and_particles_follow_it() {
        let size = [400.0, 200.0];
        // Left to right, smooth: the left edge goes first, the right last.
        assert!(moment([0.0, 0.5], size, 0.0, 0.0, 1) < 0.01);
        assert!(moment([1.0, 0.5], size, 0.0, 0.0, 1) > 0.99);
        // Bottom to top: the bottom first.
        assert!(moment([0.5, 1.0], size, 90.0, 0.0, 1) < moment([0.5, 0.0], size, 90.0, 0.0, 1));
        // Roughness moves points about, within the sweep.
        let m = moment([0.5, 0.5], size, 0.0, 0.5, 3);
        assert!((0.0..=1.0).contains(&m) && (m - 0.5).abs() > 1e-4);

        let mut e = Effect::new(EffectKind::Disintegrate);
        for (i, x) in [(p::START, 1.0), (p::DURATION, 2.0), (p::ROUGH, 0.0), (p::COUNT, 4000.0), (p::LIFETIME, 1.0), (p::SPEED, 50.0)] {
            e.params[i].value = x;
        }
        assert!(simulate(&e, Time::from_secs_f64(0.9), size).list.is_empty(), "not started");
        // Halfway: particles only from the left half, the newest near the middle.
        let l = simulate(&e, Time::from_secs_f64(2.0), size).list;
        assert!(!l.is_empty());
        assert!(l.iter().all(|q| q.emit[0] <= 50.5 && q.emit[0] >= 0.0), "{:?}", l.iter().map(|q| q.emit[0]).fold(0.0, f32::max));
        assert!(l.iter().any(|q| q.emit[0] > 45.0));
        // Long after, all gone.
        assert!(simulate(&e, Time::from_secs_f64(5.0), size).list.is_empty());
    }
}
