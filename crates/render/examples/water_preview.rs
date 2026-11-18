//! Renders preview frames of the water effects with the real compositor.
//! cargo run --release -p reelforge-render --example water_preview -- OUT_DIR [WIDTH HEIGHT]
use reelforge_core::effects::{liquid as lq, ocean as oc, water as ws};
use reelforge_core::{BlendMode, Effect, EffectKind, EffectValues, Time, Transform};
use reelforge_render::{headless_device, Compositor, LayerInput, PixelData};
use std::path::PathBuf;

fn fx(kind: EffectKind, set: &[(usize, f32)], t: f64, size: [f32; 2]) -> EffectValues {
    let mut e = Effect::new(kind);
    for &(i, v) in set {
        e.params[i].value = v;
    }
    e.eval_sized(Time::from_secs_f64(t), None, size)
}

/// A pool floor stand-in: tiles with grout lines.
fn tiles(w: u32, h: u32) -> Vec<u8> {
    let mut v = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let t = (w / 16).max(8);
            let grout = x % t < 2 || y % t < 2;
            let shade = (((x / t) * 7 + (y / t) * 3) % 5) as f32 * 6.0;
            let (r, g, b) = if grout { (200.0, 205.0, 210.0) } else { (40.0 + shade, 150.0 + shade, 190.0 + shade) };
            let i = ((y * w + x) * 4) as usize;
            v[i..i + 4].copy_from_slice(&[r as u8, g as u8, b as u8, 255]);
        }
    }
    v
}

/// A green screen with solid things in front of it: a round rock and a slanted block.
fn green_screen(w: u32, h: u32) -> Vec<u8> {
    let mut v = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
            let rock = ((fx - 0.42) * 1.78).powi(2) + (fy - 0.62).powi(2) < 0.018;
            let block = fy > 0.72 && fy < 0.82 && fx > 0.6 && fx < 0.92 && (fy - 0.72) > (fx - 0.6) * 0.2;
            let px = if rock {
                let s = 90.0 + 60.0 * (1.0 - ((fx - 0.42) * 1.78 + (fy - 0.62)).abs() * 3.0).max(0.0);
                [s as u8, (s * 0.85) as u8, (s * 0.7) as u8, 255]
            } else if block {
                [120, 70, 40, 255]
            } else {
                [30, 190, 60, 255]
            };
            let i = ((y * w + x) * 4) as usize;
            v[i..i + 4].copy_from_slice(&px);
        }
    }
    v
}

/// A dim room, for the poured water to show against.
fn room(w: u32, h: u32) -> Vec<u8> {
    let mut v = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
            let wall = if fy < 0.8 { 1.0 } else { 0.55 };
            let light = (1.0 - ((fx - 0.4).powi(2) + (fy - 0.3).powi(2)).sqrt() * 1.2).max(0.2);
            let stripe = if ((x / 40) % 2) == 0 { 1.0 } else { 0.9 };
            let base = 120.0 * light * wall * stripe;
            let i = ((y * w + x) * 4) as usize;
            v[i..i + 4].copy_from_slice(&[(base * 1.1) as u8, base as u8, (base * 0.85) as u8, 255]);
        }
    }
    v
}

fn main() {
    let mut args = std::env::args().skip(1);
    let out = PathBuf::from(args.next().unwrap_or_else(|| "water-preview".into()));
    let w: u32 = args.next().and_then(|a| a.parse().ok()).unwrap_or(960);
    let h: u32 = args.next().and_then(|a| a.parse().ok()).unwrap_or(540);
    std::fs::create_dir_all(&out).unwrap();
    let (device, queue) = headless_device().expect("a GPU (or lavapipe)");
    let mut c = Compositor::new(&device, &queue);
    c.transparent = true;
    let size = [w as f32, h as f32];
    let clear = vec![0u8; (w * h * 4) as usize];
    let floor = tiles(w, h);
    let green = green_screen(w, h);
    let wall = room(w, h);
    let shots: Vec<(&str, &Vec<u8>, EffectValues)> = vec![
        ("01-ocean", &clear, fx(EffectKind::Ocean, &[], 4.0, size)),
        ("02-ocean-storm-low-sun", &clear, fx(EffectKind::Ocean, &[(oc::WIND, 17.0), (oc::CHOP, 110.0), (oc::FOAM, 130.0), (oc::SUN_HEIGHT, 4.0), (oc::HAZE, 55.0), (oc::CAM_HEIGHT, 6.0)], 4.0, size)),
        ("03-ocean-calm-from-above", &clear, fx(EffectKind::Ocean, &[(oc::WIND, 4.0), (oc::SWELL, 90.0), (oc::CAM_HEIGHT, 25.0), (oc::TILT, 25.0), (oc::SUN_HEIGHT, 50.0)], 4.0, size)),
        ("04-ocean-water-only", &clear, fx(EffectKind::Ocean, &[(oc::SHOW, 1.0)], 4.0, size)),
        ("05-surface-rain-over-tiles", &floor, fx(EffectKind::WaterSurface, &[(ws::RAIN, 25.0)], 3.0, size)),
        ("06-surface-drops-round-rock-hidden", &green, fx(EffectKind::WaterSurface, &[(ws::RAIN, 3.0), (ws::DROP_EVERY, 0.8), (ws::DROP_X, 25.0), (ws::DROP_Y, 40.0), (ws::DROP_SIZE, 30.0), (ws::SOLIDS, 2.0), (ws::LAYER, 2.0)], 2.5, size)),
        ("07-surface-waves-against-rock-shown", &green, fx(EffectKind::WaterSurface, &[(ws::RAIN, 0.0), (ws::WAVE_HEIGHT, 120.0), (ws::WAVELENGTH, 70.0), (ws::WAVE_DIR, -20.0), (ws::SOLIDS, 2.0), (ws::LAYER, 1.0)], 4.0, size)),
        ("07b-surface-waves-plain", &floor, fx(EffectKind::WaterSurface, &[(ws::RAIN, 0.0), (ws::WAVE_HEIGHT, 120.0), (ws::WAVELENGTH, 70.0), (ws::WAVE_DIR, -20.0)], 4.0, size)),
        ("08-surface-water-only", &clear, fx(EffectKind::WaterSurface, &[(ws::RAIN, 20.0), (ws::OUTPUT, 1.0)], 3.0, size)),
        ("09-liquid-pour", &wall, fx(EffectKind::Liquid, &[], 3.0, size)),
        ("10-liquid-onto-rock-keyed", &green, fx(EffectKind::Liquid, &[(lq::X, 38.0), (lq::Y, 5.0), (lq::DIRECTION, -80.0), (lq::SPEED, 150.0), (lq::WIDTH, 60.0), (lq::SOLIDS, 2.0), (lq::SHOW, 1.0)], 3.0, size)),
        ("11-liquid-filling-water-only", &clear, fx(EffectKind::Liquid, &[(lq::FILL, 25.0), (lq::FLOW, 200.0)], 3.0, size)),
        ("13-ocean-rocks-keyed", &green, fx(EffectKind::Ocean, &[(oc::SOLIDS, 2.0), (oc::SHOW, 3.0)], 6.0, size)),
        ("14-ocean-rocks-big-swell", &green, fx(EffectKind::Ocean, &[(oc::SOLIDS, 2.0), (oc::SHOW, 3.0), (oc::SWELL, 220.0), (oc::WIND, 14.0), (oc::CAM_HEIGHT, 3.0)], 6.0, size)),
        ("14b-ocean-rocks-big-swell-no-foam", &green, fx(EffectKind::Ocean, &[(oc::SOLIDS, 2.0), (oc::SHOW, 3.0), (oc::SWELL, 220.0), (oc::WIND, 14.0), (oc::CAM_HEIGHT, 3.0), (oc::SURF, 0.0), (oc::WAKE, 0.0)], 6.0, size)),
        ("15-liquid-depth-2d", &wall, fx(EffectKind::Liquid, &[(lq::FILL, 30.0), (lq::FLOW, 200.0), (lq::DEPTH, 500.0), (lq::VIEW, 25.0)], 3.0, size)),
        ("15b-liquid-flat-2d", &wall, fx(EffectKind::Liquid, &[(lq::FILL, 30.0), (lq::FLOW, 200.0)], 3.0, size)),
        ("12-liquid-thick-onto-rock", &green, fx(EffectKind::Liquid, &[(lq::X, 45.0), (lq::Y, 2.0), (lq::DIRECTION, -90.0), (lq::SPEED, 80.0), (lq::THICK, 70.0), (lq::SOLIDS, 2.0), (lq::SHOW, 1.0), (lq::COLOR, 0xd0a020 as f32), (lq::CLEAR, 20.0)], 4.0, size)),
    ];
    let only: Option<String> = std::env::var("ONLY").ok();
    // The liquid on a 3D layer, turned towards the camera: a body of water with depth.
    if only.as_deref().is_none_or(|o| "16-liquid-3d".contains(o)) {
        use reelforge_core::space3d::{layer_space, Camera, Scene3d};
        for (name, orient) in [("16-liquid-3d-turned", [30.0f32, 30.0f32]), ("16b-liquid-3d-from-above", [55.0, 0.0])] {
            let tr = Transform { three_d: true, orientation: orient, ..Transform::default() };
            let scene = Scene3d { camera: Camera::default_for(size), lights: vec![] };
            let ls = layer_space(&tr, size, &scene);
            let e = fx(EffectKind::Liquid, &[(lq::FILL, 30.0), (lq::FLOW, 200.0), (lq::DEPTH, 500.0), (lq::SHOW, 2.0)], 3.0, size);
            let layers = vec![LayerInput { key: 77, content_id: 77, pixels: PixelData::Rgba(&clear), pixel_size: [w, h], layer_size: size, transform: tr, blend: BlendMode::Normal, effects: vec![e], mask: None, wipe: None, mesh: None, space: Some(&ls) }];
            c.render([w, h], &layers);
            image::save_buffer(out.join(format!("{name}.png")), &c.read_output().unwrap(), w, h, image::ColorType::Rgba8).unwrap();
            println!("{name}");
        }
    }
    if let Ok(frames) = std::env::var("ANIM") {
        animate(&mut c, &out, w, h, frames.parse().unwrap_or(90));
        return;
    }
    for (n, (name, px, e)) in shots.into_iter().enumerate() {
        if only.as_ref().is_some_and(|o| !name.contains(o.as_str())) {
            continue;
        }
        let t0 = std::time::Instant::now();
        let layers = vec![LayerInput {
            key: n as u64 + 1,
            content_id: n as u64 + 1,
            pixels: PixelData::Rgba(px),
            pixel_size: [w, h],
            layer_size: size,
            transform: Transform::default(),
            blend: BlendMode::Normal,
            effects: if e.is_identity() { vec![] } else { vec![e] },
            mask: None,
            wipe: None,
            mesh: None,
            space: None,
        }];
        c.render([w, h], &layers);
        let rgba = c.read_output().unwrap();
        image::save_buffer(out.join(format!("{name}.png")), &rgba, w, h, image::ColorType::Rgba8).unwrap();
        println!("{name} ({:.1} s)", t0.elapsed().as_secs_f32());
    }
}

/// A green screen with a rock moving across it at time `t`.
fn moving_rock(w: u32, h: u32, t: f32) -> Vec<u8> {
    let mut v = vec![0u8; (w * h * 4) as usize];
    let cx = 0.15 + 0.25 * t;
    for y in 0..h {
        for x in 0..w {
            let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
            let d = ((fx - cx) * 1.78).powi(2) + (fy - 0.55).powi(2);
            let px = if d < 0.012 { [130, 110, 90, 255] } else { [30, 190, 60, 255] };
            let i = ((y * w + x) * 4) as usize;
            v[i..i + 4].copy_from_slice(&px);
        }
    }
    v
}

/// Clips of each effect, frame by frame (as playback steps them), to OUT/anim/<name>_NNN.png.
fn animate(c: &mut Compositor, out: &std::path::Path, w: u32, h: u32, frames: usize) {
    let size = [w as f32, h as f32];
    let dir = out.join("anim");
    std::fs::create_dir_all(&dir).unwrap();
    let clear = vec![0u8; (w * h * 4) as usize];
    let floor = tiles(w, h);
    type Make = Box<dyn Fn(f64) -> (Vec<u8>, EffectValues)>;
    let shots: Vec<(&str, Make)> = vec![
        ("ocean", Box::new(move |t| (clear.clone(), fx(EffectKind::Ocean, &[(oc::CAM_SPEED, 3.0)], t, size)))),
        ("storm", Box::new(move |t| (vec![0u8; (w * h * 4) as usize], fx(EffectKind::Ocean, &[(oc::WIND, 17.0), (oc::CHOP, 110.0), (oc::FOAM, 130.0), (oc::SUN_HEIGHT, 4.0), (oc::HAZE, 55.0), (oc::CAM_HEIGHT, 6.0)], t, size)))),
        ("rain", Box::new(move |t| (floor.clone(), fx(EffectKind::WaterSurface, &[(ws::RAIN, 18.0)], t, size)))),
        ("wake", Box::new(move |t| (moving_rock(w, h, t as f32), fx(EffectKind::WaterSurface, &[(ws::RAIN, 2.0), (ws::SOLIDS, 2.0), (ws::LAYER, 1.0), (ws::WAKE, 200.0)], t, size)))),
        ("boat", Box::new(move |t| (moving_rock(w, h, t as f32), fx(EffectKind::Ocean, &[(oc::SOLIDS, 2.0), (oc::SHOW, 3.0), (oc::WAKE, 200.0), (oc::SWELL, 120.0)], t, size)))),
        ("pour", Box::new(move |t| (moving_rock(w, h, t as f32), fx(EffectKind::Liquid, &[(lq::X, 40.0), (lq::Y, 2.0), (lq::DIRECTION, -90.0), (lq::SPEED, 120.0), (lq::WIDTH, 50.0), (lq::FLOW, 220.0), (lq::SOLIDS, 2.0), (lq::SHOW, 1.0)], t, size)))),
    ];
    let only: Option<String> = std::env::var("ONLY").ok();
    for (n, (name, make)) in shots.iter().enumerate() {
        if only.as_ref().is_some_and(|o| !name.contains(o.as_str())) {
            continue;
        }
        for f in 0..frames {
            let t = f as f64 / 30.0;
            let (px, e) = make(t);
            let layers = vec![LayerInput {
                key: 100 + n as u64,
                content_id: (100 + n * 1000 + f) as u64,
                pixels: PixelData::Rgba(&px),
                pixel_size: [w, h],
                layer_size: size,
                transform: Transform::default(),
                blend: BlendMode::Normal,
                effects: if e.is_identity() { vec![] } else { vec![e] },
                mask: None,
                wipe: None,
                mesh: None,
                space: None,
            }];
            c.render([w, h], &layers);
            let rgba = c.read_output().unwrap();
            image::save_buffer(dir.join(format!("{name}_{f:03}.png")), &rgba, w, h, image::ColorType::Rgba8).unwrap();
        }
        println!("{name}");
    }
}
