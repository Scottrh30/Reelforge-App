//! Water effects.
//!
//! - **Ocean**: an open sea drawn pixel by pixel on the graphics card (a sum of
//!   Gerstner waves shaped by the wind, a swell and fine ripples, the sky and
//!   sun reflected, foam where the crests fold). Nothing is simulated: every
//!   moment is worked out on its own, so it needs nothing from here but its
//!   settings.
//! - **Water surface**: a height grid over the layer stepped by the wave
//!   equation (drops of rain, drops at a point, waves rolling in from a side,
//!   wind ripples), solid objects reflecting the waves and pushing water away
//!   as they move (a wake).
//! - **Liquid**: water poured from a source and flowing under gravity, seen
//!   from the side: particles carried on a grid (FLIP: the grid makes the
//!   flow incompressible, the particles keep its detail and splashes), filling
//!   up against the frame's floor and walls and solid objects.
//!
//! Like the fluid, both simulations run on the card (render crate,
//! `watergpu`) step by step from the clip's start, with a checkpoint every
//! second for seeking back. This module gives them their grid ([`job`]) and
//! each step's settings ([`surface_step`], [`liquid_step`]), all worked out
//! from the effect's (keyframed) values at the step's time, so the same clip
//! always gives the same water.

use crate::effects::{liquid as lq, water as ws, Effect, EffectKind};
use crate::fluid::FluidJob;
use crate::time::Time;

/// Water surface steps a second.
pub const SURFACE_RATE: f64 = 60.0;
/// Liquid steps a second (fast water needs small steps to stay on the grid).
pub const LIQUID_RATE: f64 = 120.0;
/// Grid columns of the water surface for each Detail setting.
const SURFACE_COLUMNS: [usize; 3] = [256, 384, 512];
/// Grid columns of the liquid for each Detail setting.
const LIQUID_COLUMNS: [usize; 3] = [120, 176, 240];
/// Most drops in one step (more in heavy rain wait for the next).
pub const MAX_DROPS: usize = 8;
/// Particles a cell holds when full.
pub const PER_CELL: usize = 4;

/// Steps a second for a simulated water effect.
pub fn rate(kind: EffectKind) -> f64 {
    if kind == EffectKind::Liquid { LIQUID_RATE } else { SURFACE_RATE }
}

fn detail(e: &Effect, i: usize) -> usize {
    (e.value_at(i, Time::ZERO).round().max(0.0) as usize).min(2)
}

/// The simulation grid for effect `e` on a layer `size` pixels across.
pub fn grid(e: &Effect, size: [f32; 2]) -> (usize, usize) {
    let w = match e.kind {
        EffectKind::Liquid => LIQUID_COLUMNS[detail(e, lq::DETAIL)],
        _ => SURFACE_COLUMNS[detail(e, ws::DETAIL)],
    };
    let h = ((w as f32 * size[1] / size[0].max(1.0)).round() as usize).clamp(8, 1024);
    (w, h)
}

/// Settings that change the simulation (the look is drawn afterwards).
fn sim_params(kind: EffectKind) -> std::ops::Range<usize> {
    match kind {
        EffectKind::Liquid => 0..lq::COLOR,
        _ => 0..ws::COLOR,
    }
}

/// The job for water effect `e` (a water surface or a liquid) on a `size`
/// layer at source time `t`: what the graphics card simulates.
pub fn job(e: &Effect, t: Time, size: [f32; 2]) -> Option<FluidJob> {
    let enough = match e.kind {
        EffectKind::WaterSurface => e.params.len() > ws::OPACITY,
        EffectKind::Liquid => e.params.len() > lq::OPACITY,
        _ => false,
    };
    if !enough || !(size[0] >= 1.0 && size[1] >= 1.0) {
        return None;
    }
    let (w, h) = grid(e, size);
    let solids = match e.kind {
        EffectKind::Liquid => e.value_at(lq::SOLIDS, Time::ZERO),
        _ => e.value_at(ws::SOLIDS, Time::ZERO),
    };
    let key = {
        use std::hash::{Hash, Hasher};
        let mut hs = std::collections::hash_map::DefaultHasher::new();
        (e.kind as u32).hash(&mut hs);
        format!("{:?}", e.params.get(sim_params(e.kind))).hash(&mut hs);
        (w, h).hash(&mut hs);
        size.map(f32::to_bits).hash(&mut hs);
        hs.finish()
    };
    let step = (t.as_secs_f64().max(0.0) * rate(e.kind)).floor().min(1e9) as usize;
    Some(FluidJob { key: key ^ 0x5741_7465, effect: e.clone(), size, w, h, step, obstacles: solids >= 0.5 })
}

fn splitmix(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

fn hash(a: u64, b: u64) -> u64 {
    splitmix(a ^ splitmix(b))
}

/// A random number in [0, 1) for (`seed`, `step`, `k`).
fn rand(seed: u64, step: usize, k: u64) -> f32 {
    (hash(hash(seed, step as u64), k) >> 40) as f32 / (1u64 << 24) as f32
}

/// How a layer becomes solid for the water (matching [`ws::SOLID_KINDS`]),
/// and the key's tolerance (0–1).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Solids {
    pub kind: u32,
    pub tolerance: f32,
}

fn solids(e: &Effect, kind_at: usize, tol_at: usize, at: Time) -> Solids {
    Solids { kind: e.value_at(kind_at, at).round().clamp(0.0, 5.0) as u32, tolerance: (e.value_at(tol_at, at) / 100.0).clamp(0.0, 1.0) }
}

/// One step of a water surface (grid cells, steps).
#[derive(Clone, Debug, PartialEq)]
pub struct SurfaceStep {
    /// Drops this step: (x, y, radius, strength), heights in cells.
    pub drops: Vec<[f32; 4]>,
    /// Waves rolling in: height (cells), wave number (radians a cell),
    /// travel direction (unit), phase now (radians).
    pub wave: [f32; 4],
    pub wave_dir: [f32; 2],
    /// Wind ripples: noise added a step.
    pub wind: f32,
    /// Wave speed squared (cells² a step², at most 0.45 for stability).
    pub c2: f32,
    /// Fraction of the motion kept a step.
    pub keep: f32,
    pub solids: Solids,
    /// How much moving objects push the water.
    pub wake: f32,
    pub step: u32,
}

/// The settings of step `step` of water surface `e` on a `w`×`h` grid over a `size` layer.
pub fn surface_step(e: &Effect, step: usize, w: usize, h: usize, size: [f32; 2]) -> SurfaceStep {
    let t = step as f64 / SURFACE_RATE;
    let at = Time::from_secs_f64(t);
    let v = |i: usize| e.value_at(i, at);
    let cell = size[0] / w.max(1) as f32;
    let seed = e.value_at(ws::SEED, Time::ZERO).max(0.0) as u64;
    let mut drops = Vec::new();
    let radius = (v(ws::DROP_SIZE) * 0.5 / cell).max(0.7);
    // A drop's dent: deeper for bigger drops, but never more than a few cells.
    let strength = -(v(ws::DROP_STRENGTH) / 100.0) * radius.min(6.0) * 0.8;
    // Rain: on average Rain drops a second, each step's few at random places.
    let rain = (v(ws::RAIN).max(0.0) as f64 / SURFACE_RATE) as f32;
    let mut n = rain.floor() as usize + usize::from(rand(seed, step, 0) < rain.fract());
    n = n.min(MAX_DROPS - 1);
    for k in 0..n as u64 {
        let size_jitter = 0.6 + 0.8 * rand(seed, step, 10 + k * 3);
        drops.push([rand(seed, step, 11 + k * 3) * w as f32, rand(seed, step, 12 + k * 3) * h as f32, radius * size_jitter, strength * size_jitter]);
    }
    // A drop at the point, every so often (the first at the start).
    let every = v(ws::DROP_EVERY);
    if every > 0.0 {
        let period = (every as f64 * SURFACE_RATE).round().max(1.0) as usize;
        if step % period == 0 {
            drops.push([v(ws::DROP_X) / 100.0 * w as f32, v(ws::DROP_Y) / 100.0 * h as f32, radius, strength * 1.5]);
        }
    }
    let c = (v(ws::SPEED) / 100.0).clamp(0.1, 1.0) * 0.67;
    let c2 = c * c;
    // Waves rolling in from one side: c cells a step, so ω = k·c.
    let wavelength = (v(ws::WAVELENGTH) / cell).max(4.0);
    let k = std::f32::consts::TAU / wavelength;
    let dir = v(ws::WAVE_DIR).to_radians();
    let amp = v(ws::WAVE_HEIGHT) / 100.0 * 0.6;
    let phase = (k * c * step as f32) % std::f32::consts::TAU;
    let calm = (v(ws::CALM) / 100.0).clamp(0.0, 1.0);
    SurfaceStep {
        drops,
        wave: [amp, k, 0.0, phase],
        wave_dir: [dir.cos(), -dir.sin()],
        wind: v(ws::WIND) / 100.0 * 0.004,
        c2,
        // Calming 0: waves run on; 100: they're gone in a fraction of a second.
        keep: 1.0 - (0.0005 + calm * calm * 0.06),
        solids: solids(e, ws::SOLIDS, ws::TOLERANCE, at),
        wake: v(ws::WAKE) / 100.0,
        step: step as u32,
    }
}

/// One step of a liquid (grid cells and steps; y down).
#[derive(Clone, Debug, PartialEq)]
pub struct LiquidStep {
    /// The source: middle of its mouth, direction (unit), speed (cells a step),
    /// width (cells).
    pub src: [f32; 2],
    pub dir: [f32; 2],
    pub speed: f32,
    pub width: f32,
    /// Particles poured in this step.
    pub emit: u32,
    /// Gravity (cells a step², y down).
    pub gravity: f32,
    /// FLIP fraction (the rest PIC: smoother, thicker water).
    pub flip: f32,
    /// Velocity smoothing a step (thick liquids).
    pub viscosity: f32,
    /// Bounce off solids (0–1).
    pub bounce: f32,
    /// Floor, left and right walls (true: solid).
    pub floor: bool,
    pub walls: bool,
    pub solids: Solids,
    /// The starting pool: its surface, in cells from the top (h: none).
    pub fill_to: f32,
    pub step: u32,
    pub seed: u32,
}

/// The settings of step `step` of liquid `e` on a `w`×`h` grid over a `size` layer.
pub fn liquid_step(e: &Effect, step: usize, w: usize, h: usize, size: [f32; 2]) -> LiquidStep {
    let t = step as f64 / LIQUID_RATE;
    let at = Time::from_secs_f64(t);
    let v = |i: usize| e.value_at(i, at);
    let cell = size[0] / w.max(1) as f32;
    let seed = e.value_at(lq::SEED, Time::ZERO).max(0.0) as u64;
    let rate = LIQUID_RATE as f32;
    let speed = (v(lq::SPEED).max(0.0) / cell / rate).min(4.0);
    let dir = v(lq::DIRECTION).to_radians();
    let width = (v(lq::WIDTH) / cell).max(0.5);
    // Pouring between Pours from and (if set) Pours from + Pours for.
    let from = e.value_at(lq::FROM, Time::ZERO) as f64;
    let dur = e.value_at(lq::FOR, Time::ZERO) as f64;
    let pouring = t >= from && (dur <= 0.0 || t < from + dur);
    // Particles in: a full stream (PER_CELL a cell) through the mouth at its speed, at least a trickle.
    let flow = (v(lq::FLOW) / 100.0).max(0.0);
    let per_step = if pouring { PER_CELL as f32 * width * speed.max(0.15) * flow } else { 0.0 };
    let emit = per_step.floor() as u32 + u32::from(rand(seed, step, 1) < per_step.fract());
    let edges = e.value_at(lq::EDGES, Time::ZERO).round() as u32;
    let fill = e.value_at(lq::FILL, Time::ZERO).clamp(0.0, 95.0) / 100.0;
    let thick = (v(lq::THICK) / 100.0).clamp(0.0, 1.0);
    LiquidStep {
        src: [v(lq::X) / 100.0 * w as f32, v(lq::Y) / 100.0 * h as f32],
        dir: [dir.cos(), -dir.sin()],
        speed,
        width,
        emit: emit.min(4096),
        gravity: (v(lq::GRAVITY) / cell / (rate * rate)).clamp(-0.5, 0.5),
        flip: ((v(lq::SPLASH) / 100.0).clamp(0.0, 1.0) * 0.98) * (1.0 - thick * 0.7),
        viscosity: thick * thick * 0.5,
        bounce: (v(lq::BOUNCE) / 100.0).clamp(0.0, 1.0),
        floor: matches!(edges, 0 | 1),
        walls: matches!(edges, 0 | 2),
        solids: solids(e, lq::SOLIDS, lq::TOLERANCE, at),
        fill_to: h as f32 * (1.0 - fill),
        step: step as u32,
        seed: seed as u32,
    }
}

/// How many particles a liquid on a `w`×`h` grid can hold.
pub fn capacity(w: usize, h: usize) -> usize {
    (w * h * PER_CELL).clamp(1024, 1 << 20)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn effect(kind: EffectKind, set: &[(usize, f32)]) -> Effect {
        let mut e = Effect::new(kind);
        for &(i, v) in set {
            e.params[i].value = v;
        }
        e
    }

    #[test]
    fn jobs_track_time_and_ignore_the_look() {
        let e = effect(EffectKind::Liquid, &[]);
        let a = job(&e, Time::from_secs_f64(1.0), [1920.0, 1080.0]).unwrap();
        assert_eq!(a.step, 120);
        assert_eq!((a.w, a.h), (120, 68), "Low detail by default");
        let b = job(&effect(EffectKind::Liquid, &[(lq::COLOR, 0xff0000 as f32), (lq::OPACITY, 50.0)]), Time::from_secs_f64(1.0), [1920.0, 1080.0]).unwrap();
        assert_eq!(a.key, b.key, "colour and opacity don't re-run the simulation");
        let c = job(&effect(EffectKind::Liquid, &[(lq::GRAVITY, 500.0)]), Time::from_secs_f64(1.0), [1920.0, 1080.0]).unwrap();
        assert_ne!(a.key, c.key);
        assert!(job(&effect(EffectKind::Ocean, &[]), Time::ZERO, [100.0, 100.0]).is_none());
        assert!(job(&e, Time::ZERO, [0.0, f32::NAN]).is_none());
        let s = job(&effect(EffectKind::WaterSurface, &[]), Time::from_secs_f64(0.5), [1280.0, 720.0]).unwrap();
        assert_eq!((s.step, s.w, s.h), (30, 256, 144));
    }

    #[test]
    fn rain_falls_at_its_rate_and_the_same_every_time() {
        let e = effect(EffectKind::WaterSurface, &[(ws::RAIN, 30.0)]);
        let total: usize = (0..600).map(|s| surface_step(&e, s, 384, 216, [1280.0, 720.0]).drops.len()).sum();
        assert!((250..350).contains(&total), "{total} in 10 s at 30 a second");
        assert_eq!(surface_step(&e, 77, 384, 216, [1280.0, 720.0]), surface_step(&e, 77, 384, 216, [1280.0, 720.0]));
        let dry = effect(EffectKind::WaterSurface, &[(ws::RAIN, 0.0), (ws::DROP_EVERY, 1.0)]);
        let drops: Vec<usize> = (0..181).filter(|&s| !surface_step(&dry, s, 384, 216, [1280.0, 720.0]).drops.is_empty()).collect();
        assert_eq!(drops, vec![0, 60, 120, 180], "a drop at the point every second");
        for s in 0..200 {
            let st = surface_step(&e, s, 384, 216, [1280.0, 720.0]);
            assert!(st.c2 <= 0.45 && st.keep > 0.9 && st.keep < 1.0);
            assert!(st.drops.iter().all(|d| d[0] >= 0.0 && d[0] <= 384.0 && d[1] >= 0.0 && d[1] <= 216.0));
        }
    }

    #[test]
    fn liquid_pours_only_while_it_should() {
        let e = effect(EffectKind::Liquid, &[(lq::FROM, 1.0), (lq::FOR, 2.0)]);
        let emitted = |a: usize, b: usize| (a..b).map(|s| liquid_step(&e, s, 176, 99, [1920.0, 1080.0]).emit as usize).sum::<usize>();
        assert_eq!(emitted(0, 120), 0, "not before 1 s");
        // a 36 px stream at 250 px/s: about 300 particles a second
        assert!((450..800).contains(&emitted(120, 360)), "{}", emitted(120, 360));
        assert_eq!(emitted(361, 600), 0, "stops after 2 s of pouring");
        let st = liquid_step(&e, 200, 176, 99, [1920.0, 1080.0]);
        assert!(st.gravity > 0.0 && st.floor && st.walls);
        assert!((st.src[0] - 35.2).abs() < 0.01 && (st.src[1] - 11.88).abs() < 0.01);
        // Pouring down to the right (direction -15°): y grows downwards.
        assert!(st.dir[0] > 0.9 && st.dir[1] > 0.2);
        // Hostile values stay finite and bounded.
        let wild = effect(EffectKind::Liquid, &[(lq::SPEED, 1e9), (lq::WIDTH, 1e9), (lq::FLOW, 1e9), (lq::GRAVITY, -1e9)]);
        let st = liquid_step(&wild, 5, 176, 99, [1920.0, 1080.0]);
        assert!(st.emit <= 4096 && st.speed <= 4.0 && st.gravity >= -0.5);
    }
}
