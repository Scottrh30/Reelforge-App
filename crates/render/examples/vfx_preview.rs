//! Renders preview frames of built-in effects with the real compositor, over a test picture
//! (bricks, a lit wall, a figure) so distortions and light have something to show on.
//! cargo run -p reelforge-render --example vfx_preview -- OUT_DIR [NAME…]
use reelforge_core::effects::*;
use reelforge_core::{BlendMode, Effect, EffectKind, EffectValues, Time, Transform};
use reelforge_render::{headless_device, Compositor, LayerInput, PixelData};
use std::path::PathBuf;

const W: u32 = 640;
const H: u32 = 360;

fn fx(kind: EffectKind, set: &[(usize, f32)], t: f32) -> EffectValues {
    let mut e = Effect::new(kind);
    for &(i, v) in set {
        if let Some(p) = e.params.get_mut(i) {
            p.value = v;
        }
    }
    let mut v = e.eval(Time::from_secs_f64(t as f64));
    v.time = t;
    v
}

/// A wall of bricks lit from the left, a floor, a dark figure.
fn picture() -> Vec<u8> {
    let mut v = vec![0u8; (W * H * 4) as usize];
    for y in 0..H {
        for x in 0..W {
            let (fx, fy) = (x as f32 / W as f32, y as f32 / H as f32);
            let floor = fy > 0.75;
            let row = y / 24;
            let mortar = y % 24 < 2 || ((x + if row % 2 == 0 { 0 } else { 36 }) % 72) < 2;
            let light = (1.1 - ((fx - 0.3).powi(2) + (fy - 0.3).powi(2)).sqrt() * 1.3).clamp(0.2, 1.0);
            let (mut r, mut g, mut b) = if floor { (70.0, 66.0, 62.0) } else if mortar { (120.0, 115.0, 105.0) } else { (150.0, 70.0, 50.0) };
            r *= light;
            g *= light;
            b *= light;
            // A figure standing in the middle.
            let (dx, dy) = (fx - 0.5, fy - 0.5);
            let body = (dx.abs() < 0.04 && (0.32..0.78).contains(&fy)) || (dx * dx * 4.0 + (dy + 0.24).powi(2) * 9.0 < 0.004);
            if body {
                (r, g, b) = (20.0, 22.0, 30.0);
            }
            let i = ((y * W + x) * 4) as usize;
            v[i] = r as u8;
            v[i + 1] = g as u8;
            v[i + 2] = b as u8;
            v[i + 3] = 255;
        }
    }
    v
}

/// The figure alone (and a bar like a title under it), see-through elsewhere.
fn cutout() -> Vec<u8> {
    let mut v = picture();
    for y in 0..H {
        for x in 0..W {
            let (fx, fy) = (x as f32 / W as f32, y as f32 / H as f32);
            let (dx, dy) = (fx - 0.5, fy - 0.5);
            let body = (dx.abs() < 0.04 && (0.32..0.78).contains(&fy)) || (dx * dx * 4.0 + (dy + 0.24).powi(2) * 9.0 < 0.004);
            let bar = dx.abs() < 0.3 && (0.82..0.92).contains(&fy);
            let i = ((y * W + x) * 4) as usize;
            if bar {
                v[i..i + 4].copy_from_slice(&[200, 170, 90, 255]);
            } else if !body {
                v[i + 3] = 0;
            }
        }
    }
    v
}

/// A shot: its name, the effect, the settings changed, and the time (s).
type Shot = (&'static str, EffectKind, Vec<(usize, f32)>, f32);

/// A landscape: blue sky, green trees and grass, a path, a person in a red top.
fn landscape() -> Vec<u8> {
    let mut v = vec![0u8; (W * H * 4) as usize];
    for y in 0..H {
        for x in 0..W {
            let (fx, fy) = (x as f32 / W as f32, y as f32 / H as f32);
            let wobble = ((fx * 23.0).sin() * 0.03 + (fx * 61.0).sin() * 0.015).abs();
            let tree_line = 0.42 - wobble - if (0.6..0.8).contains(&fx) { 0.12 * (1.0 - ((fx - 0.7) / 0.1).powi(2)).max(0.0) } else { 0.0 };
            let mut c = if fy < tree_line {
                [(90.0 + 80.0 * fy) as u8, (150.0 + 60.0 * fy) as u8, 235]
            } else if fy < 0.6 {
                let n = ((fx * 90.0).sin() * (fy * 70.0).cos() * 0.5 + 0.5) * 40.0;
                [(30.0 + n * 0.3) as u8, (95.0 + n) as u8, (35.0 + n * 0.2) as u8]
            } else {
                let path = (fx - 0.5).abs() < 0.05 + (fy - 0.6) * 0.4;
                if path { [170, 150, 120] } else { [(70.0 + 30.0 * fy) as u8, (140.0 + 40.0 * fy) as u8, 60] }
            };
            // A person: skin face, red top, dark legs.
            let (dx, dy) = (fx - 0.42, fy - 0.5);
            if dx * dx * 6.0 + (dy + 0.12).powi(2) * 4.0 < 0.0016 {
                c = [225, 175, 140];
            } else if dx.abs() < 0.025 && (-0.08..0.06).contains(&dy) {
                c = [200, 40, 40];
            } else if dx.abs() < 0.02 && (0.06..0.2).contains(&dy) {
                c = [40, 45, 70];
            }
            let i = ((y * W + x) * 4) as usize;
            v[i..i + 4].copy_from_slice(&[c[0], c[1], c[2], 255]);
        }
    }
    v
}

fn shots() -> Vec<Shot> {
    vec![
        ("turbulent", EffectKind::TurbulentDisplace, vec![(turbulent::AMOUNT, 18.0), (turbulent::SIZE, 90.0)], 0.0),
        ("turbulent-fine", EffectKind::TurbulentDisplace, vec![(turbulent::AMOUNT, 8.0), (turbulent::SIZE, 25.0), (turbulent::COMPLEXITY, 5.0)], 0.0),
        ("heat-haze", EffectKind::HeatHaze, vec![(haze::STRENGTH, 5.0), (haze::AREA, 1.0), (haze::HEIGHT, 60.0)], 0.4),
        ("shockwave", EffectKind::Shockwave, vec![(shockwave::SPEED, 600.0), (shockwave::WIDTH, 50.0), (shockwave::STRENGTH, 22.0), (shockwave::BRIGHTNESS, 60.0)], 0.2),
        ("shockwave-later", EffectKind::Shockwave, vec![(shockwave::SPEED, 600.0), (shockwave::WIDTH, 50.0), (shockwave::STRENGTH, 22.0), (shockwave::BRIGHTNESS, 60.0)], 0.4),
        ("stars-sky", EffectKind::Starfield, vec![(stars::DRAW, 1.0)], 0.5),
        ("stars-drift", EffectKind::Starfield, vec![(stars::MODE, 1.0), (stars::DRAW, 1.0), (stars::AMOUNT, 70.0)], 2.0),
        ("stars-warp", EffectKind::Starfield, vec![(stars::MODE, 2.0), (stars::DRAW, 1.0), (stars::SPEED, 300.0), (stars::AMOUNT, 70.0)], 1.0),
        ("hologram", EffectKind::Hologram, vec![], 0.3),
        ("portal", EffectKind::Portal, vec![], 0.6),
        ("portal-dark", EffectKind::Portal, vec![(portal::INSIDE, 1.0), (portal::STRETCH, 160.0)], 1.1),
        ("hud-reticle", EffectKind::Hud, vec![], 0.5),
        ("hud-radar", EffectKind::Hud, vec![(hud::ELEMENT, 1.0)], 0.5),
        ("hud-rings", EffectKind::Hud, vec![(hud::ELEMENT, 2.0)], 0.5),
        ("hud-bars", EffectKind::Hud, vec![(hud::ELEMENT, 3.0)], 0.5),
        ("hud-brackets", EffectKind::Hud, vec![(hud::ELEMENT, 4.0)], 0.5),
        ("cut-sweep", EffectKind::LightSweep, vec![(sweep::BRIGHTNESS, 250.0), (sweep::SECOND, 50.0)], 0.45),
        ("cut-arcs", EffectKind::ElectricArcs, vec![], 0.3),
        ("cut-arcs-more", EffectKind::ElectricArcs, vec![(arcs::AMOUNT, 90.0), (arcs::REACH, 50.0)], 0.7),
        ("cut-hologram", EffectKind::Hologram, vec![], 0.3),
        ("vis-land-night", EffectKind::NightVision, vec![(night_vision::GOGGLES, 2.0)], 0.3),
        ("vis-land-thermal", EffectKind::ThermalCamera, vec![(thermal::SCALE, 1.0)], 0.3),
        ("vis-land-infrared", EffectKind::Infrared, vec![], 0.3),
        ("vis-land-cir", EffectKind::Infrared, vec![(infrared::MODE, 1.0)], 0.3),
        ("vis-land-golden", EffectKind::Infrared, vec![(infrared::MODE, 2.0)], 0.3),
        ("vis-land-xray", EffectKind::XRay, vec![], 0.3),
        ("vis-land", EffectKind::Infrared, vec![(infrared::MIX, 0.0)], 0.3),
        ("vis-night", EffectKind::NightVision, vec![(night_vision::GOGGLES, 2.0)], 0.3),
        ("vis-thermal", EffectKind::ThermalCamera, vec![(thermal::SCALE, 1.0)], 0.3),
        ("vis-infrared", EffectKind::Infrared, vec![(infrared::MODE, 1.0)], 0.3),
        ("vis-xray", EffectKind::XRay, vec![], 0.3),
        ("blur-dir", EffectKind::DirectionalBlur, vec![(dir_blur::LENGTH, 60.0), (dir_blur::ANGLE, 20.0)], 0.0),
        ("blur-spin", EffectKind::RadialBlur, vec![(radial_blur::AMOUNT, 25.0)], 0.0),
        ("blur-zoom", EffectKind::RadialBlur, vec![(radial_blur::TYPE, 1.0), (radial_blur::AMOUNT, 40.0), (radial_blur::X, 30.0), (radial_blur::Y, 30.0)], 0.0),
    ]
}

fn main() {
    let mut args = std::env::args().skip(1);
    let out = PathBuf::from(args.next().unwrap_or_else(|| "target/vfx".into()));
    let only: Vec<String> = args.collect();
    let _ = std::fs::create_dir_all(&out);
    let Some((device, queue)) = headless_device() else {
        eprintln!("no graphics device");
        return;
    };
    let mut c = Compositor::new(&device, &queue);
    let px = picture();
    let cut = cutout();
    let land = landscape();
    for (n, (name, kind, set, t)) in shots().into_iter().enumerate() {
        if !only.is_empty() && !only.iter().any(|o| name.starts_with(o.as_str())) {
            continue;
        }
        let layer = LayerInput {
            key: n as u64 + 1,
            content_id: n as u64 + 1,
            pixels: PixelData::Rgba(if name.starts_with("cut") { &cut } else if name.contains("land") { &land } else { &px }),
            pixel_size: [W, H],
            layer_size: [W as f32, H as f32],
            transform: Transform::default(),
            blend: BlendMode::Normal,
            effects: vec![fx(kind, &set, t)],
            mask: None,
            wipe: None,
            mesh: None,
            space: None,
        };
        c.render([W, H], &[layer]);
        let Some(rgba) = c.read_output() else { continue };
        if let Err(e) = image::save_buffer(out.join(format!("{name}.png")), &rgba, W, H, image::ColorType::Rgba8) {
            eprintln!("{name}: {e}");
        }
        println!("{name}");
    }
}
