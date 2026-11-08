//! A 2D fluid: coloured smoke or ink poured from a source into still air,
//! swirling and rising. Jos Stam's "stable fluids" on a grid over the layer
//! (velocity pushed by the source and by buoyancy, swirls kept alive by
//! vorticity confinement, made incompressible by a pressure solve, colour
//! carried along by the flow and slowly fading).
//!
//! Unlike particles, a fluid has to be simulated step by step from the
//! clip's start, 30 steps a second. The states are cached (one every second
//! as a checkpoint, and the latest), so playing forwards takes a step a
//! frame and seeking goes back to the nearest checkpoint. The picture goes
//! to the GPU as a small texture (a "table") the effect stretches over the layer.

use crate::effects::{fluid_params as p, Effect, EffectKind};
use crate::lut::Table;
use crate::time::Time;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Simulation steps a second.
pub const RATE: f64 = 30.0;
/// A checkpoint every this many steps.
const EVERY: usize = 30;
/// Simulations kept (different effects or settings).
const KEEP: usize = 6;
/// Pressure-solve iterations a step.
const ITERS: usize = 24;

/// Grid columns for each Detail setting.
const COLUMNS: [usize; 3] = [96, 160, 240];

#[derive(Clone)]
struct State {
    u: Vec<f32>,
    v: Vec<f32>,
    /// Premultiplied colour (linear) and density.
    dye: [Vec<f32>; 4],
}

struct Sim {
    w: usize,
    h: usize,
    checkpoints: Vec<(usize, State)>,
    last: (usize, State),
    used: u64,
}

static CACHE: Mutex<Option<HashMap<u64, Sim>>> = Mutex::new(None);
static TICK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}
fn linear_to_srgb(c: f32) -> f32 {
    let c = c.clamp(0.0, 1.0);
    if c <= 0.003_130_8 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 }
}

/// Grid size for an effect on a layer of `size` pixels.
pub fn grid(e: &Effect, size: [f32; 2]) -> (usize, usize) {
    let detail = (e.value_at(p::DETAIL, Time::ZERO).round().max(0.0) as usize).min(COLUMNS.len() - 1);
    let w = COLUMNS[detail];
    let h = ((w as f32 * size[1] / size[0].max(1.0)).round() as usize).clamp(8, 400);
    (w, h)
}

impl State {
    fn new(n: usize) -> State {
        State { u: vec![0.0; n], v: vec![0.0; n], dye: [vec![0.0; n], vec![0.0; n], vec![0.0; n], vec![0.0; n]] }
    }
}

struct Grid {
    w: usize,
    h: usize,
}

impl Grid {
    fn ix(&self, x: usize, y: usize) -> usize {
        y * self.w + x
    }
    /// Bilinear sample of `f` at (x, y) in cells (clamped to the grid).
    fn sample(&self, f: &[f32], x: f32, y: f32) -> f32 {
        let x = x.clamp(0.0, (self.w - 1) as f32);
        let y = y.clamp(0.0, (self.h - 1) as f32);
        let (x0, y0) = (x.floor() as usize, y.floor() as usize);
        let (x1, y1) = ((x0 + 1).min(self.w - 1), (y0 + 1).min(self.h - 1));
        let (fx, fy) = (x - x0 as f32, y - y0 as f32);
        let a = f[self.ix(x0, y0)] * (1.0 - fx) + f[self.ix(x1, y0)] * fx;
        let b = f[self.ix(x0, y1)] * (1.0 - fx) + f[self.ix(x1, y1)] * fx;
        a * (1.0 - fy) + b * fy
    }
    /// `f` carried along the velocity (u, v) for one step (semi-Lagrangian).
    fn advect(&self, f: &[f32], u: &[f32], v: &[f32]) -> Vec<f32> {
        let mut out = vec![0.0; f.len()];
        for y in 0..self.h {
            for x in 0..self.w {
                let i = self.ix(x, y);
                out[i] = self.sample(f, x as f32 - u[i], y as f32 - v[i]);
            }
        }
        out
    }
}

/// Settings read at a step's time.
struct Inputs {
    src: [f32; 2],
    radius: f32,
    push: [f32; 2],
    amount: f32,
    color: [f32; 3],
    rise: f32,
    swirl: f32,
    fade: f32,
}

fn inputs(e: &Effect, t: f64, g: &Grid, size: [f32; 2], seed: u64) -> Inputs {
    let at = Time::from_secs_f64(t);
    let v = |i: usize| e.value_at(i, at);
    let cell = size[0] / g.w as f32;
    // The direction wanders a little, so the plume isn't a ruler-straight jet.
    let wobble = ((t as f32 * 2.3 + seed as f32 * 0.37).sin() * 0.6 + (t as f32 * 5.1 + seed as f32 * 1.7).sin() * 0.4) * 0.18;
    let dir = v(p::DIRECTION).to_radians() + wobble;
    let speed = v(p::FORCE) / cell / RATE as f32;
    let k = 0.5 - 0.5 * (std::f32::consts::TAU * v(p::CYCLE) * t as f32).cos();
    let (a, b) = (crate::effects::unpack_rgb(v(p::COLOR_A)), crate::effects::unpack_rgb(v(p::COLOR_B)));
    let color = [0, 1, 2].map(|c| srgb_to_linear(a[c] * (1.0 - k) + b[c] * k));
    Inputs {
        src: [v(p::X) / 100.0 * g.w as f32, v(p::Y) / 100.0 * g.h as f32],
        radius: (v(p::SIZE) * 0.5 / cell).max(0.75),
        push: [dir.cos() * speed, -dir.sin() * speed],
        amount: v(p::AMOUNT) / 100.0,
        color,
        rise: v(p::RISE) / 100.0,
        swirl: v(p::SWIRL) / 100.0,
        fade: v(p::FADE) / 100.0,
    }
}

/// One step of the simulation.
fn step(s: &mut State, g: &Grid, inp: &Inputs) {
    let dt = 1.0 / RATE as f32;
    // The source: velocity and colour poured in.
    let r = inp.radius;
    let (x0, x1) = (((inp.src[0] - r * 2.0).floor().max(0.0)) as usize, ((inp.src[0] + r * 2.0).ceil().max(0.0) as usize).min(g.w - 1));
    let (y0, y1) = (((inp.src[1] - r * 2.0).floor().max(0.0)) as usize, ((inp.src[1] + r * 2.0).ceil().max(0.0) as usize).min(g.h - 1));
    if inp.src[0] > -r * 2.0 && inp.src[1] > -r * 2.0 {
        for y in y0..=y1 {
            for x in x0..=x1 {
                let d2 = (x as f32 - inp.src[0]).powi(2) + (y as f32 - inp.src[1]).powi(2);
                let w = (-d2 / (r * r)).exp();
                if w < 0.01 {
                    continue;
                }
                let i = g.ix(x, y);
                s.u[i] += (inp.push[0] - s.u[i]) * w * 0.6;
                s.v[i] += (inp.push[1] - s.v[i]) * w * 0.6;
                let add = (inp.amount * w * dt * 6.0).min(1.0);
                for c in 0..3 {
                    s.dye[c][i] += (inp.color[c] - s.dye[c][i]) * add;
                }
                s.dye[3][i] += (1.0 - s.dye[3][i]) * add;
            }
        }
    }
    // Buoyancy: thick colour rises (y is down).
    if inp.rise != 0.0 {
        for i in 0..s.v.len() {
            s.v[i] -= inp.rise * s.dye[3][i] * 0.06;
        }
    }
    // Vorticity confinement: swirls kept from dying out.
    if inp.swirl > 0.0 {
        let (w, h) = (g.w, g.h);
        let mut curl = vec![0.0f32; w * h];
        for y in 1..h - 1 {
            for x in 1..w - 1 {
                curl[g.ix(x, y)] = (s.v[g.ix(x + 1, y)] - s.v[g.ix(x - 1, y)] - s.u[g.ix(x, y + 1)] + s.u[g.ix(x, y - 1)]) * 0.5;
            }
        }
        for y in 2..h - 2 {
            for x in 2..w - 2 {
                let nx = (curl[g.ix(x + 1, y)].abs() - curl[g.ix(x - 1, y)].abs()) * 0.5;
                let ny = (curl[g.ix(x, y + 1)].abs() - curl[g.ix(x, y - 1)].abs()) * 0.5;
                let len = nx.hypot(ny) + 1e-5;
                let c = curl[g.ix(x, y)];
                let i = g.ix(x, y);
                s.u[i] += inp.swirl * 0.35 * (ny / len) * c;
                s.v[i] -= inp.swirl * 0.35 * (nx / len) * c;
            }
        }
    }
    // Carry the velocity along itself, then make it incompressible.
    let (u, v) = (g.advect(&s.u, &s.u, &s.v), g.advect(&s.v, &s.u, &s.v));
    s.u = u;
    s.v = v;
    project(s, g);
    // The colour goes with the flow, fading.
    let keep = (1.0 - inp.fade * dt).clamp(0.0, 1.0);
    for c in 0..4 {
        let mut d = g.advect(&s.dye[c], &s.u, &s.v);
        for x in &mut d {
            *x *= keep;
        }
        s.dye[c] = d;
    }
}

/// Remove the divergence (a Jacobi pressure solve), with walls at the edges.
fn project(s: &mut State, g: &Grid) {
    let (w, h) = (g.w, g.h);
    let mut div = vec![0.0f32; w * h];
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            div[g.ix(x, y)] = -0.5 * (s.u[g.ix(x + 1, y)] - s.u[g.ix(x - 1, y)] + s.v[g.ix(x, y + 1)] - s.v[g.ix(x, y - 1)]);
        }
    }
    let mut pr = vec![0.0f32; w * h];
    let mut next = vec![0.0f32; w * h];
    for _ in 0..ITERS {
        for y in 1..h - 1 {
            for x in 1..w - 1 {
                next[g.ix(x, y)] = (div[g.ix(x, y)] + pr[g.ix(x - 1, y)] + pr[g.ix(x + 1, y)] + pr[g.ix(x, y - 1)] + pr[g.ix(x, y + 1)]) * 0.25;
            }
        }
        std::mem::swap(&mut pr, &mut next);
    }
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let i = g.ix(x, y);
            s.u[i] -= 0.5 * (pr[g.ix(x + 1, y)] - pr[g.ix(x - 1, y)]);
            s.v[i] -= 0.5 * (pr[g.ix(x, y + 1)] - pr[g.ix(x, y - 1)]);
        }
    }
    for x in 0..w {
        for y in [0, h - 1] {
            let i = g.ix(x, y);
            s.u[i] = 0.0;
            s.v[i] = 0.0;
        }
    }
    for y in 0..h {
        for x in [0, w - 1] {
            let i = g.ix(x, y);
            s.u[i] = 0.0;
            s.v[i] = 0.0;
        }
    }
}

/// What the simulation depends on (not the time).
fn key(e: &Effect, w: usize, h: usize, size: [f32; 2]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hs = std::collections::hash_map::DefaultHasher::new();
    format!("{:?}", e.params).hash(&mut hs);
    (w, h).hash(&mut hs);
    size.map(f32::to_bits).hash(&mut hs);
    hs.finish()
}

/// The fluid of effect `e` on a layer of `size` pixels at source time `t`,
/// as a texture: straight sRGB colour and coverage.
pub fn simulate(e: &Effect, t: Time, size: [f32; 2]) -> Option<Arc<Table>> {
    if e.kind != EffectKind::Fluid || e.params.len() <= p::SEED {
        return None;
    }
    let (w, h) = grid(e, size);
    let g = Grid { w, h };
    let key = key(e, w, h, size);
    let target = (t.as_secs_f64() * RATE).floor().max(0.0) as usize;
    let seed = e.value_at(p::SEED, Time::ZERO) as u64;
    let state = {
        let mut cache = CACHE.lock().unwrap_or_else(|p| p.into_inner());
        let cache = cache.get_or_insert_with(HashMap::new);
        if !cache.contains_key(&key) && cache.len() >= KEEP {
            if let Some(old) = cache.iter().min_by_key(|(_, s)| s.used).map(|(k, _)| *k) {
                cache.remove(&old);
            }
        }
        let sim = cache.entry(key).or_insert_with(|| Sim { w, h, checkpoints: vec![(0, State::new(w * h))], last: (0, State::new(w * h)), used: 0 });
        sim.used = TICK.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // The nearest state at or before the target: the latest, or a checkpoint.
        let (mut at, mut s) = std::iter::once(&sim.last).chain(&sim.checkpoints).filter(|(n, _)| *n <= target).max_by_key(|(n, _)| *n).cloned().unwrap_or((0, State::new(w * h)));
        while at < target {
            let inp = inputs(e, at as f64 / RATE, &g, size, seed);
            step(&mut s, &g, &inp);
            at += 1;
            if at % EVERY == 0 && !sim.checkpoints.iter().any(|(n, _)| *n == at) {
                sim.checkpoints.push((at, s.clone()));
            }
        }
        sim.last = (at, s.clone());
        let _ = (sim.w, sim.h);
        s
    };
    let mut rgba = Vec::with_capacity(w * h * 4);
    for i in 0..w * h {
        let a = state.dye[3][i].clamp(0.0, 1.0);
        let c = |k: usize| (linear_to_srgb(if a > 1e-4 { state.dye[k][i] / a } else { 0.0 }) * 255.0).round() as u8;
        rgba.extend_from_slice(&[c(0), c(1), c(2), (a * 255.0).round() as u8]);
    }
    use std::hash::{Hash, Hasher};
    let mut hs = std::collections::hash_map::DefaultHasher::new();
    (key, target).hash(&mut hs);
    Some(Arc::new(Table { id: hs.finish(), width: w as u32, height: h as u32, rgba, cube_size: 0 }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fluid() -> Effect {
        let mut e = Effect::new(EffectKind::Fluid);
        e.params[p::DETAIL].value = 0.0;
        e
    }

    fn mass(t: &Table) -> f32 {
        t.rgba.chunks_exact(4).map(|c| c[3] as f32 / 255.0).sum()
    }

    /// Where the colour is: its middle (in cells).
    fn centre(t: &Table) -> [f32; 2] {
        let (mut sx, mut sy, mut n) = (0.0, 0.0, 0.0);
        for (i, c) in t.rgba.chunks_exact(4).enumerate() {
            let a = c[3] as f32;
            sx += (i % t.width as usize) as f32 * a;
            sy += (i / t.width as usize) as f32 * a;
            n += a;
        }
        [sx / n, sy / n]
    }

    #[test]
    fn colour_pours_in_rises_and_is_the_same_however_you_get_there() {
        let e = fluid();
        let size = [1920.0, 1080.0];
        let s = Time::from_secs_f64;
        let a = simulate(&e, s(0.5), size).unwrap();
        assert_eq!((a.width, a.height), (96, 54));
        let b = simulate(&e, s(2.0), size).unwrap();
        assert!(mass(&b) > mass(&a) && mass(&a) > 0.5, "{} {}", mass(&a), mass(&b));
        // The source is low down, pouring up: the colour's middle rises.
        assert!(centre(&b)[1] < centre(&a)[1] && centre(&a)[1] < 54.0 * 0.85 + 1.0, "{:?} {:?}", centre(&a), centre(&b));
        // Seeking back and forth gives the same picture as playing through.
        let again = simulate(&e, s(0.5), size).unwrap();
        assert_eq!(again.rgba, a.rgba);
        assert_eq!(simulate(&e, s(2.0), size).unwrap().rgba, b.rgba);
        // Mostly the first colour (blue) early on.
        let c = b.rgba.chunks_exact(4).max_by_key(|c| c[3]).unwrap();
        assert!(c[2] > c[0], "{c:?}");
        // A different setting is a different simulation.
        let mut other = fluid();
        other.params[p::DIRECTION].value = 0.0;
        assert_ne!(simulate(&other, s(2.0), size).unwrap().rgba, b.rgba);
    }
}
