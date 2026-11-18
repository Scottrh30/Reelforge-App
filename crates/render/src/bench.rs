//! Help → About → Check → Performance: times the heavy GPU work on this
//! computer's graphics card at 1080p (optical flow at each quality, the
//! fluid simulation at each detail, 60,000 particles, a layered frame with
//! effects), so it's known what plays in real time here.

use crate::{Compositor, LayerInput, PixelData};
use reelforge_core::{BlendMode, Effect, EffectKind, FlowQuality, Time, Transform};
use serde::Serialize;
use std::time::Instant;

/// One measurement.
#[derive(Clone, Debug, Serialize)]
pub struct BenchLine {
    pub what: String,
    /// Milliseconds a frame (or a step).
    pub ms: f64,
    /// "realtime" (fits in a 30 fps frame), "slow" (plays, but slower than
    /// real time), or "heavy" (over a quarter second).
    pub verdict: &'static str,
    pub note: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct BenchReport {
    pub adapter: String,
    pub lines: Vec<BenchLine>,
}

fn line(what: impl Into<String>, ms: f64, note: impl Into<String>) -> BenchLine {
    let verdict = if ms <= 33.3 {
        "realtime"
    } else if ms <= 250.0 {
        "slow"
    } else {
        "heavy"
    };
    BenchLine { what: what.into(), ms: (ms * 10.0).round() / 10.0, verdict, note: note.into() }
}

/// A 1080p picture with detail everywhere and a block at `x`.
fn frame(w: u32, h: u32, x: u32) -> Vec<u8> {
    let mut v = Vec::with_capacity((w * h * 4) as usize);
    for j in 0..h {
        for i in 0..w {
            let mut s = (i / 4).wrapping_mul(0x9E37_79B1) ^ (j / 4).wrapping_mul(0x85EB_CA77);
            s ^= s >> 15;
            s = s.wrapping_mul(0x2C1B_3C6D);
            let g = 40 + (s >> 26) as u8;
            if i >= x && i < x + 300 && (390..690).contains(&j) {
                v.extend([200, 120 + (s >> 27) as u8, 60, 255]);
            } else {
                v.extend([g, g, g + 8, 255]);
            }
        }
    }
    v
}

/// The median of `n` timed runs of `f` (milliseconds).
fn time(n: usize, mut f: impl FnMut()) -> f64 {
    let mut t: Vec<f64> = (0..n)
        .map(|_| {
            let s = Instant::now();
            f();
            s.elapsed().as_secs_f64() * 1000.0
        })
        .collect();
    t.sort_by(f64::total_cmp);
    t[t.len() / 2]
}

/// Run every measurement on `device` (a few seconds to half a minute).
pub fn run(device: &wgpu::Device, queue: &wgpu::Queue) -> BenchReport {
    let adapter = crate::adapter_info().unwrap_or_else(|| "(unknown)".to_string());
    let (w, h) = (1920u32, 1080u32);
    let mut lines = Vec::new();
    let a = frame(w, h, 700);
    let b = frame(w, h, 740);
    let wait = || {
        let _ = device.poll(wgpu::PollType::wait_indefinitely());
    };

    // Optical flow: working out the motion (once per pair of frames), then each in-between frame.
    let mut flow = crate::flowgpu::FlowInterp::new(device, queue);
    let (pa, pb) = (reelforge_core::frame::Pixels::Rgba(a.clone().into()), reelforge_core::frame::Pixels::Rgba(b.clone().into()));
    for (k, q) in [FlowQuality::Fast, FlowQuality::Balanced, FlowQuality::Best].into_iter().enumerate() {
        let mut key = 1000 * k as u64;
        let pair = time(2, || {
            key += 1;
            flow.interpolate(1, key, &pa, &pb, [w, h], 0.5, q);
        });
        let tween = time(5, || {
            flow.interpolate(1, key, &pa, &pb, [w, h], 0.3, q);
        });
        lines.push(line(format!("Optical flow, {q:?}: each new pair of frames"), pair, "once per source frame (slow motion reuses it)"));
        lines.push(line(format!("Optical flow, {q:?}: each frame in between"), tween, "every frame drawn"));
    }

    // Fluid: one step (a frame of playback) at each detail.
    let mut fluid = crate::fluidgpu::FluidGpu::new(device, queue);
    for detail in 0..3 {
        let mut e = Effect::new(EffectKind::Fluid);
        e.params[reelforge_core::effects::fluid_params::DETAIL].value = detail as f32;
        let Some(base) = reelforge_core::fluid::job(&e, Time::ZERO, [w as f32, h as f32]) else { continue };
        let job = |s: usize| {
            let mut j = base.clone();
            j.step = s;
            j
        };
        fluid.step_to(&job(30), None);
        wait();
        let mut step = 30;
        let ms = time(5, || {
            step += 1;
            fluid.step_to(&job(step), None);
            wait();
        });
        let cols = job(0).w;
        lines.push(line(format!("Fluid, {} ({cols} cells across): each frame", reelforge_core::effects::fluid_params::DETAILS[detail]), ms, "one simulation step"));
        let seek = time(1, || {
            fluid.step_to(&job(step + 300), None);
            wait();
        });
        lines.push(line(format!("Fluid, {}: seeking 10 seconds ahead", reelforge_core::effects::fluid_params::DETAILS[detail]), seek, "300 steps at once"));
    }

    // Particles and a layered frame, through the compositor.
    let mut c = Compositor::new(device, queue);
    let mut draw = |effects: Vec<reelforge_core::EffectValues>, layers: usize, id: &mut u64| {
        let input = |key: u64, fx: Vec<reelforge_core::EffectValues>, id: u64| LayerInput { key, content_id: id, pixels: PixelData::Rgba(&a), pixel_size: [w, h], layer_size: [w as f32, h as f32], transform: Transform::default(), blend: BlendMode::Normal, effects: fx, mask: None, wipe: None, mesh: None, space: None };
        *id += 1;
        let mut ls: Vec<LayerInput> = (0..layers.saturating_sub(1)).map(|k| input(k as u64 + 10, Vec::new(), 1)).collect();
        ls.push(input(99, effects.clone(), *id));
        c.render([w, h], &ls);
        let _ = c.read_output();
    };
    let mut id = 0;
    let mut p = Effect::new(EffectKind::Particles);
    for (i, v) in [(reelforge_core::particles::RATE, 5000.0), (reelforge_core::particles::LIFETIME, 12.0), (reelforge_core::particles::SHAPE, 4.0)] {
        p.params[i].value = v;
    }
    let pv = p.eval_sized(Time::from_secs_f64(15.0), None, [w as f32, h as f32]);
    let n = pv.particles.as_ref().map_or(0, |l| l.list.len());
    let ms = time(5, || draw(vec![pv.clone()], 1, &mut id));
    lines.push(line(format!("{n} particles over a 1080p layer"), ms, "drawing, and reading the frame back"));
    let mut fx = Vec::new();
    for k in [EffectKind::Blur, EffectKind::Glow, EffectKind::ColorCorrect] {
        let mut e = Effect::new(k);
        if k == EffectKind::Blur {
            e.params[0].value = 12.0;
        }
        fx.push(e.eval(Time::ZERO));
    }
    let ms = time(5, || draw(fx.clone(), 4, &mut id));
    lines.push(line("Four 1080p layers, blur, glow and colour on the top one", ms, "drawing, and reading the frame back"));
    let r = BenchReport { adapter, lines };
    *LAST.lock().unwrap_or_else(|e| e.into_inner()) = Some(r.clone());
    r
}

static LAST: std::sync::Mutex<Option<BenchReport>> = std::sync::Mutex::new(None);

/// The last check this session, for the diagnostics report.
pub fn last_summary() -> Option<String> {
    let r = LAST.lock().ok()?.clone()?;
    Some(std::iter::once(r.adapter).chain(r.lines.iter().map(|l| format!("{}: {} ms", l.what, l.ms))).collect::<Vec<_>>().join("; "))
}

#[cfg(test)]
mod tests {
    /// Minutes on a software renderer: run by hand (`--ignored --nocapture`).
    #[test]
    #[ignore]
    fn measures_everything() {
        let Some((d, q)) = crate::headless_device() else { return };
        let r = super::run(&d, &q);
        println!("{}", r.adapter);
        for l in &r.lines {
            println!("{:>9.1} ms  {:9} {}", l.ms, l.verdict, l.what);
        }
        assert_eq!(r.lines.len(), 14);
        assert!(super::last_summary().is_some());
    }
}
