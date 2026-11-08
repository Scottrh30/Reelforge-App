//! Particle emitters: where every particle alive at one moment was born and
//! how it was thrown. The GPU (render crate) does the rest: from these birth
//! states and the current settings it works out each particle's age, path
//! (speed, gravity, wind, turbulence), size, colour and fade, and draws them
//! all in one instanced draw.
//!
//! Nothing is simulated step by step, so any frame can be drawn on its own
//! (seeking, scrubbing, exporting on several threads all agree): births come
//! from integrating the (keyframable) rate from the clip's source time 0, and
//! each particle's randomness from a hash of its number and the seed. The
//! emitter settings (position, shape, rate, lifetime, speed, direction) are
//! read at each particle's birth, so a moving emitter leaves a trail.

use crate::effects::{Effect, EffectKind};
use crate::time::Time;

pub const EMIT_X: usize = 0;
pub const EMIT_Y: usize = 1;
pub const SHAPE: usize = 2;
pub const EMIT_SIZE: usize = 3;
pub const RATE: usize = 4;
pub const LIFETIME: usize = 5;
pub const RANDOMNESS: usize = 6;
pub const SPEED: usize = 7;
pub const DIRECTION: usize = 8;
pub const SPREAD: usize = 9;
pub const GRAVITY: usize = 10;
pub const WIND: usize = 11;
pub const TURBULENCE: usize = 12;
pub const SIZE_START: usize = 13;
pub const SIZE_END: usize = 14;
pub const COLOR_START: usize = 15;
pub const COLOR_END: usize = 16;
pub const OPACITY: usize = 17;
pub const LOOK: usize = 18;
pub const SEED: usize = 19;

pub const SHAPES: [&str; 7] = ["Point", "Line", "Disc", "Ring (outwards)", "Whole layer", "Layer's pixels", "Layer's pixels, layer hidden"];
pub const SHAPE_POINT: u32 = 0;
pub const SHAPE_LINE: u32 = 1;
pub const SHAPE_DISC: u32 = 2;
pub const SHAPE_RING: u32 = 3;
pub const SHAPE_LAYER: u32 = 4;
/// Born anywhere the layer's picture shows, in its colour there ("atomic"
/// particles: a picture made of particles, or breaking up into them).
pub const SHAPE_PIXELS: u32 = 5;
/// The same, with the layer itself not drawn (only its particles).
pub const SHAPE_PIXELS_ONLY: u32 = 6;
/// Glow, sparks and flame add light (drawn with additive blending); the
/// others cover what's under them.
pub const LOOKS: [&str; 8] = ["Glow (add)", "Sparks (add)", "Soft dots", "Solid dots", "Flame (add)", "Smoke", "Rain streaks", "Debris"];
pub const LOOK_SPARKS: u32 = 1;
pub const LOOK_FLAME: u32 = 4;
pub const LOOK_SMOKE: u32 = 5;
pub const LOOK_RAIN: u32 = 6;
pub const LOOK_DEBRIS: u32 = 7;

/// Does look `look` add light (rather than cover)?
pub fn additive(look: f32) -> bool {
    let l = look.round().max(0.0) as u32;
    l <= 1 || l == LOOK_FLAME
}

/// Ready-made emitters: a name and the settings from Shape to Look (the
/// emitter's position and seed are kept). Sizes and speeds suit a 1080p frame.
#[rustfmt::skip]
pub const PRESETS: [(&str, [f32; 17]); 14] = [
    //            shape size    rate   life rand speed  dir  spread grav    wind turb birth death colour at birth/death    opacity look
    ("Sparks",   [0.0, 10.0,   300.0, 1.2, 50.0, 700.0, 70.0, 50.0, 900.0, 0.0, 0.0, 6.0, 1.5, 0xfff2c0 as f32, 0xff7a1a as f32, 100.0, 1.0]),
    ("Embers",   [1.0, 900.0,  40.0,  4.0, 60.0, 120.0, 90.0, 40.0, -40.0, 30.0, 60.0, 8.0, 2.0, 0xffb347 as f32, 0xff3000 as f32, 100.0, 0.0]),
    ("Fire",     [1.0, 160.0,  220.0, 1.0, 45.0, 160.0, 90.0, 18.0, -220.0, 0.0, 22.0, 70.0, 22.0, 0xff8a28 as f32, 0x901000 as f32, 55.0, 4.0]),
    ("Smoke",    [2.0, 80.0,   30.0,  5.0, 40.0, 90.0, 90.0, 25.0, -30.0, 15.0, 40.0, 60.0, 260.0, 0x8c8c8c as f32, 0x4a4a4a as f32, 45.0, 5.0]),
    ("Magic",    [2.0, 60.0,   200.0, 1.5, 60.0, 80.0, 90.0, 360.0, -60.0, 0.0, 50.0, 10.0, 0.0, 0xc8a0ff as f32, 0x40c0ff as f32, 100.0, 0.0]),
    ("Snow",     [1.0, 2600.0, 90.0,  7.0, 50.0, 70.0, -95.0, 25.0, 15.0, 20.0, 40.0, 8.0, 6.0, 0xffffff as f32, 0xe8f0ff as f32, 90.0, 2.0]),
    ("Dust",     [4.0, 0.0,    20.0,  6.0, 60.0, 10.0, 90.0, 360.0, 0.0, 0.0, 30.0, 4.0, 4.0, 0xfff0d8 as f32, 0xffe0b0 as f32, 60.0, 0.0]),
    ("Confetti", [1.0, 1600.0, 120.0, 5.0, 40.0, 120.0, -90.0, 30.0, 120.0, 0.0, 60.0, 12.0, 12.0, 0xff3d7f as f32, 0x3dc8ff as f32, 100.0, 3.0]),
    ("Rain",     [1.0, 2800.0, 900.0, 1.1, 30.0, 1500.0, -100.0, 4.0, 900.0, 0.0, 0.0, 2.5, 2.5, 0xdbe6f0 as f32, 0xc8d4e0 as f32, 45.0, 6.0]),
    ("Fog",      [4.0, 0.0,    3.0,   14.0, 50.0, 25.0, 0.0, 360.0, 0.0, 6.0, 30.0, 700.0, 1000.0, 0xd8dde2 as f32, 0xc8ced4 as f32, 16.0, 5.0]),
    ("Debris",   [2.0, 60.0,   60.0,  2.5, 60.0, 650.0, 90.0, 120.0, 1400.0, 0.0, 0.0, 14.0, 10.0, 0x3a3530 as f32, 0x2a2622 as f32, 100.0, 7.0]),
    ("Atomic",   [5.0, 0.0,    3000.0, 2.0, 60.0, 60.0, 90.0, 360.0, -40.0, 0.0, 40.0, 4.0, 1.0, 0xffffff as f32, 0x40c0ff as f32, 100.0, 0.0]),
    ("Disintegrate", [6.0, 0.0, 5000.0, 3.0, 50.0, 60.0, 30.0, 60.0, -60.0, 120.0, 50.0, 6.0, 1.0, 0xffffff as f32, 0xffa040 as f32, 100.0, 3.0]),
    ("Water spray", [0.0, 12.0, 500.0, 1.0, 40.0, 900.0, 75.0, 14.0, 1500.0, 0.0, 6.0, 5.0, 2.0, 0xe6f2ff as f32, 0xbcd8ff as f32, 70.0, 6.0]),
];

/// At most this many particles are drawn (the newest are kept).
pub const MAX_ALIVE: usize = 60_000;
/// The step a keyframed rate is integrated with (seconds).
const STEP: f64 = 1.0 / 240.0;

/// One particle's birth state and age, as the GPU reads it (12 floats).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[repr(C)]
pub struct Particle {
    /// The emitter's position at birth, % of the layer.
    pub emit: [f32; 2],
    /// Where on the emitter's shape it was born, frame pixels from `emit`.
    pub offset: [f32; 2],
    /// Its starting velocity, frame pixels per second (y down).
    pub velocity: [f32; 2],
    /// Seconds since birth, and how long it lives.
    pub age: f32,
    pub life: f32,
    /// Size multiplier (0.5–1, with randomness).
    pub size: f32,
    /// A random number 0–1 of its own (turbulence phase, flicker).
    pub seed: f32,
    pub _pad: [f32; 2],
}

/// The particles alive at one moment.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Particles {
    /// Changes whenever the list could (the settings, their keyframes or the time).
    pub id: u64,
    /// Oldest first (so the newest are drawn on top).
    pub list: Vec<Particle>,
}

impl Particles {
    pub fn as_floats(&self) -> &[f32] {
        // Safety: `Particle` is `repr(C)` and made only of f32s.
        unsafe { std::slice::from_raw_parts(self.list.as_ptr() as *const f32, self.list.len() * 12) }
    }
}

/// splitmix64: a well-mixed 64-bit number from any input.
fn mix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Random number `j` (0–1) of particle `k`.
fn rnd(seed: u64, k: u64, j: u64) -> f64 {
    (mix(seed ^ mix(k.wrapping_mul(16).wrapping_add(j))) >> 11) as f64 / (1u64 << 53) as f64
}

/// When each particle alive at `t` (seconds) was born: (particle number,
/// birth time), oldest first, born no earlier than `t - window`.
fn births(e: &Effect, seed: u64, t: f64, window: f64) -> Vec<(u64, f64)> {
    let from = (t - window).max(0.0);
    let mut out = Vec::new();
    let rate_param = &e.params[RATE];
    // Particle k is born when the count of births so far passes k + jitter.
    if !rate_param.keys.is_animated() {
        let r = e.value_at(RATE, Time::ZERO) as f64;
        if r <= 0.0 {
            return out;
        }
        let first = (from * r).floor().max(0.0) as u64;
        let mut k = first;
        loop {
            let b = (k as f64 + rnd(seed, k, 0)) / r;
            if b > t {
                break;
            }
            if b >= from {
                out.push((k, b));
            }
            k += 1;
        }
        return out;
    }
    let rate = |s: f64| e.value_at(RATE, Time::from_secs_f64(s)).max(0.0) as f64;
    let (mut acc, mut k) = (0.0f64, 0u64);
    let mut next = rnd(seed, 0, 0);
    let mut s = 0.0;
    let mut r0 = rate(0.0);
    while s < t {
        let s1 = (s + STEP).min(t);
        let r1 = rate(s1);
        let area = (r0 + r1) * 0.5 * (s1 - s);
        while acc + area >= next && area > 0.0 {
            let b = s + (next - acc) / area * (s1 - s);
            if b >= from {
                out.push((k, b));
            }
            k += 1;
            next = k as f64 + rnd(seed, k, 0);
        }
        acc += area;
        (s, r0) = (s1, r1);
    }
    out
}

/// The longest any particle can live: the lifetime setting's highest value.
fn max_life(e: &Effect) -> f64 {
    let p = &e.params[LIFETIME];
    let s = &EffectKind::Particles.params()[LIFETIME];
    p.keys.0.iter().map(|k| k.value).fold(p.value, f32::max).clamp(s.min, s.max) as f64
}

/// The particles of emitter `e` (a Particles effect) alive at source time `t`.
pub fn simulate(e: &Effect, t: Time) -> Particles {
    use std::hash::{Hash, Hasher};
    let ts = t.as_secs_f64();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    format!("{:?}", e.params).hash(&mut h);
    ts.to_bits().hash(&mut h);
    let id = h.finish();
    if e.kind != EffectKind::Particles || e.params.len() <= SEED || ts <= 0.0 {
        return Particles { id, list: Vec::new() };
    }
    let seed = mix(e.value_at(SEED, Time::ZERO) as u64 ^ 0x5EED);
    let mut born = births(e, seed, ts, max_life(e));
    if born.len() > MAX_ALIVE * 2 {
        born.drain(..born.len() - MAX_ALIVE * 2);
    }
    // A setting read at a particle's birth: its keyframed value then, or simply its value.
    let animated: Vec<bool> = e.params.iter().map(|p| p.keys.is_animated()).collect();
    let fixed: Vec<f32> = (0..e.params.len()).map(|i| e.value_at(i, t)).collect();
    let at = |i: usize, b: f64| if animated[i] { e.value_at(i, Time::from_secs_f64(b)) } else { fixed[i] };
    let mut list = Vec::with_capacity(born.len().min(MAX_ALIVE));
    for (k, b) in born {
        let r = |j: u64| rnd(seed, k, j) as f32;
        let vary = at(RANDOMNESS, b) / 100.0;
        let life = at(LIFETIME, b) * (1.0 - vary * r(1));
        let age = (ts - b) as f32;
        if age >= life || life <= 0.0 {
            continue;
        }
        let speed = at(SPEED, b) * (1.0 - vary * r(2));
        let dir = at(DIRECTION, b).to_radians();
        let spread = at(SPREAD, b).to_radians();
        let mut angle = dir + spread * (r(3) - 0.5);
        let size = at(EMIT_SIZE, b);
        let mut emit = [at(EMIT_X, b), at(EMIT_Y, b)];
        let mut offset = [0.0f32; 2];
        match at(SHAPE, b) as u32 {
            SHAPE_LINE => {
                // Across the direction of travel (a line of rain falls down from it).
                let along = (r(4) - 0.5) * size;
                offset = [-dir.sin() * along, -dir.cos() * along];
            }
            SHAPE_DISC => {
                let (a, d) = (r(4) * std::f32::consts::TAU, r(5).sqrt() * size * 0.5);
                offset = [a.cos() * d, a.sin() * d];
            }
            SHAPE_RING => {
                let a = r(4) * std::f32::consts::TAU;
                offset = [a.cos() * size * 0.5, a.sin() * size * 0.5];
                // Outwards from the middle (the spread still scatters it).
                angle = -a + spread * (r(3) - 0.5);
            }
            SHAPE_LAYER | SHAPE_PIXELS | SHAPE_PIXELS_ONLY => emit = [r(4) * 100.0, r(5) * 100.0],
            _ => {}
        }
        list.push(Particle {
            emit,
            offset,
            // Angles count anticlockwise from the right, with y down the screen.
            velocity: [angle.cos() * speed, -angle.sin() * speed],
            age,
            life,
            size: 1.0 - vary * 0.5 * r(6),
            seed: r(7),
            _pad: [0.0; 2],
        });
    }
    if list.len() > MAX_ALIVE {
        list.drain(..list.len() - MAX_ALIVE);
    }
    Particles { id, list }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::Effect;

    fn emitter(set: &[(usize, f32)]) -> Effect {
        let mut e = Effect::new(EffectKind::Particles);
        for &(i, v) in set {
            e.params[i].value = v;
        }
        e
    }

    fn secs(s: f64) -> Time {
        Time::from_secs_f64(s)
    }

    #[test]
    fn a_steady_rate_fills_up_to_rate_times_lifetime() {
        let e = emitter(&[(RATE, 100.0), (LIFETIME, 2.0), (RANDOMNESS, 0.0)]);
        assert!(simulate(&e, Time::ZERO).list.is_empty());
        let n = |s| simulate(&e, secs(s)).list.len() as i64;
        assert!((n(1.0) - 100).abs() <= 1, "{}", n(1.0));
        // Once the first have died, as many are born as die.
        for s in [3.0, 7.5, 60.0] {
            assert!((n(s) - 200).abs() <= 1, "{s}: {}", n(s));
        }
        let p = simulate(&e, secs(3.0));
        assert!(p.list.iter().all(|p| p.age >= 0.0 && p.age < p.life && p.life == 2.0));
        // Oldest first.
        assert!(p.list.windows(2).all(|w| w[0].age >= w[1].age));
    }

    #[test]
    fn the_same_moment_gives_the_same_particles_and_a_seed_changes_them() {
        let e = emitter(&[(RATE, 50.0)]);
        let a = simulate(&e, secs(4.2));
        assert_eq!(a, simulate(&e, secs(4.2)));
        assert!(!a.list.is_empty());
        // A particle keeps its birth state from frame to frame: only its age moves on.
        let b = simulate(&e, secs(4.2 + 1.0 / 30.0));
        let same = a.list.iter().filter(|p| b.list.iter().any(|q| q.velocity == p.velocity && q.offset == p.offset && (q.age - p.age - 1.0 / 30.0).abs() < 1e-4)).count();
        assert!(same > a.list.len() * 9 / 10, "{same} of {}", a.list.len());
        let other = simulate(&emitter(&[(RATE, 50.0), (SEED, 7.0)]), secs(4.2));
        assert_ne!(other.list, a.list);
        assert_ne!(other.id, a.id);
    }

    #[test]
    fn a_keyframed_rate_bursts_and_the_emitter_leaves_a_trail() {
        let mut e = emitter(&[(LIFETIME, 5.0), (RANDOMNESS, 0.0), (RATE, 0.0)]);
        // A burst: 0 → 1000/s → 0 over 0.2 s (about 100 particles).
        let k = &mut e.params[RATE].keys;
        k.set(secs(1.0), 0.0);
        k.set(secs(1.1), 1000.0);
        k.set(secs(1.2), 0.0);
        let p = simulate(&e, secs(2.0));
        assert!((p.list.len() as i64 - 100).abs() <= 3, "{}", p.list.len());
        assert!(p.list.iter().all(|p| p.age > 0.8 - 1e-3 && p.age < 1.0 + 1e-3));
        // Nothing more is born after the burst.
        assert_eq!(simulate(&e, secs(4.0)).list.len(), p.list.len());

        // The emitter moves left to right: earlier particles were born further left.
        let mut e = emitter(&[(RATE, 20.0), (LIFETIME, 10.0)]);
        let k = &mut e.params[EMIT_X].keys;
        k.set(secs(0.0), 10.0);
        k.set(secs(2.0), 90.0);
        let p = simulate(&e, secs(2.0));
        assert!(p.list.windows(2).all(|w| w[0].emit[0] <= w[1].emit[0] + 1e-3));
        assert!(p.list[0].emit[0] < 15.0 && p.list.last().unwrap().emit[0] > 85.0);
    }

    #[test]
    fn direction_spread_and_shapes() {
        // Straight up, no spread: velocity (0, -speed).
        let e = emitter(&[(DIRECTION, 90.0), (SPREAD, 0.0), (SPEED, 300.0), (RANDOMNESS, 0.0)]);
        for p in simulate(&e, secs(1.0)).list {
            assert!(p.velocity[0].abs() < 1e-3 && (p.velocity[1] + 300.0).abs() < 1e-3);
            assert_eq!(p.offset, [0.0, 0.0]);
        }
        // A line across the direction of travel (down): offsets along x, within its length.
        let e = emitter(&[(DIRECTION, -90.0), (SPREAD, 0.0), (SHAPE, SHAPE_LINE as f32), (EMIT_SIZE, 400.0)]);
        let l = simulate(&e, secs(1.0)).list;
        assert!(l.iter().all(|p| p.offset[1].abs() < 1e-3 && p.offset[0].abs() <= 200.0 && p.velocity[1] > 0.0));
        assert!(l.iter().any(|p| p.offset[0] < -100.0) && l.iter().any(|p| p.offset[0] > 100.0));
        // A ring throws outwards from the middle.
        let e = emitter(&[(SPREAD, 0.0), (SHAPE, SHAPE_RING as f32), (EMIT_SIZE, 200.0)]);
        for p in simulate(&e, secs(1.0)).list {
            let r = (p.offset[0].powi(2) + p.offset[1].powi(2)).sqrt();
            assert!((r - 100.0).abs() < 0.01);
            let dot = (p.offset[0] * p.velocity[0] + p.offset[1] * p.velocity[1]) / (r * (p.velocity[0].hypot(p.velocity[1])));
            assert!(dot > 0.999, "{dot}");
        }
        // The whole layer: anywhere.
        let e = emitter(&[(SHAPE, SHAPE_LAYER as f32), (RATE, 400.0)]);
        let l = simulate(&e, secs(1.0)).list;
        assert!(l.iter().all(|p| (0.0..=100.0).contains(&p.emit[0]) && (0.0..=100.0).contains(&p.emit[1])));
        assert!(l.iter().any(|p| p.emit[0] < 10.0) && l.iter().any(|p| p.emit[1] > 90.0));
    }

    #[test]
    fn presets_are_within_range() {
        let specs = EffectKind::Particles.params();
        for (name, v) in PRESETS {
            for (k, x) in v.iter().enumerate() {
                let s = &specs[SHAPE + k];
                assert!(*x >= s.min && *x <= s.max, "{name}: {} = {x}", s.name);
            }
            assert_eq!(specs[SHAPE + v.len() - 1].name, "Look");
        }
    }

    #[test]
    fn randomness_varies_life_and_the_count_is_capped() {
        let e = emitter(&[(RATE, 200.0), (LIFETIME, 2.0), (RANDOMNESS, 50.0)]);
        let l = simulate(&e, secs(5.0)).list;
        assert!(l.iter().all(|p| p.life <= 2.0 && p.life >= 1.0));
        assert!(l.iter().any(|p| p.life < 1.2) && l.iter().any(|p| p.life > 1.8));
        let e = emitter(&[(RATE, 5000.0), (LIFETIME, 20.0), (RANDOMNESS, 0.0)]);
        assert_eq!(simulate(&e, secs(30.0)).list.len(), MAX_ALIVE);
    }
}
