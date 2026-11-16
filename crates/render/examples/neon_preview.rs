//! Renders preview frames of the Neon FX plugin pack with the real compositor.
//! cargo run -p reelforge-render --example neon_preview -- OUT_DIR
use ab_glyph::{Font, FontArc, PxScale, ScaleFont};
use reelforge_core::plugin::{self, PluginDef};
use reelforge_core::{BlendMode, EffectKind, EffectValues, Transform};
use reelforge_render::{headless_device, Compositor, LayerInput, PixelData};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const W: u32 = 960;
const H: u32 = 540;

fn fx(def: &Arc<PluginDef>, set: &[(usize, f32)], time: f32) -> EffectValues {
    let mut values = [0.0f32; reelforge_core::effects::MAX_PARAMS];
    for (i, p) in def.params.iter().enumerate() {
        values[i] = p.default;
    }
    for &(i, v) in set {
        values[i] = v;
    }
    EffectValues { kind: EffectKind::Plugin, values, table: None, plugin: Some(def.clone()), time, prev: None, particles: None, pieces: None, fluid: None, table2: None, gpu: [0.0; 4] }
}

/// A dark, slightly lit backdrop with some structure (so glows have something to sit on).
fn backdrop() -> Vec<u8> {
    let mut v = vec![0u8; (W * H * 4) as usize];
    for y in 0..H {
        for x in 0..W {
            let (fx, fy) = (x as f32 / W as f32, y as f32 / H as f32);
            let wall = if fy < 0.72 { 1.0 } else { 0.6 };
            let brick = if ((y / 30) % 2 == 0 && (x % 90) < 3) || ((y / 30) % 2 == 1 && ((x + 45) % 90) < 3) || y % 30 < 2 { 0.6 } else { 1.0 };
            let light = (1.0 - ((fx - 0.5).powi(2) + (fy - 0.35).powi(2)).sqrt() * 1.4).max(0.15);
            let base = 40.0 * light * wall * brick;
            let i = ((y * W + x) * 4) as usize;
            v[i] = (base * 1.05) as u8;
            v[i + 1] = (base * 0.95) as u8;
            v[i + 2] = (base * 1.15) as u8;
            v[i + 3] = 255;
        }
    }
    v
}

/// A colourful "photo" stand-in for the mask: diagonal gradients and soft blobs.
fn photo() -> Vec<u8> {
    let mut v = vec![0u8; (W * H * 4) as usize];
    for y in 0..H {
        for x in 0..W {
            let (fx, fy) = (x as f32 / W as f32, y as f32 / H as f32);
            let b1 = (-((fx - 0.35).powi(2) + (fy - 0.4).powi(2)) * 18.0).exp();
            let b2 = (-((fx - 0.7).powi(2) + (fy - 0.65).powi(2)) * 14.0).exp();
            let r = 60.0 + 150.0 * fx + 60.0 * b2;
            let g = 90.0 + 120.0 * b1 + 40.0 * fy;
            let b = 160.0 - 100.0 * fx + 80.0 * b1;
            let i = ((y * W + x) * 4) as usize;
            v[i] = r.min(255.0) as u8;
            v[i + 1] = g.min(255.0) as u8;
            v[i + 2] = b.min(255.0) as u8;
            v[i + 3] = 255;
        }
    }
    v
}

/// White text on transparent, `w` x `h`.
fn text_layer(text: &str, w: u32, h: u32, px: f32) -> Vec<u8> {
    let font = FontArc::try_from_slice(epaint_default_fonts::UBUNTU_LIGHT).unwrap();
    let sf = font.as_scaled(PxScale::from(px));
    let width: f32 = text.chars().map(|c| sf.h_advance(font.glyph_id(c))).sum();
    let mut x = (w as f32 - width) * 0.5;
    let base = (h as f32 + sf.ascent() * 0.7) * 0.5;
    let mut a = vec![0f32; (w * h) as usize];
    for c in text.chars() {
        let g = font.glyph_id(c).with_scale_and_position(PxScale::from(px), ab_glyph::point(x, base));
        x += sf.h_advance(font.glyph_id(c));
        if let Some(o) = font.outline_glyph(g) {
            let bb = o.px_bounds();
            o.draw(|gx, gy, cov| {
                let (px, py) = (bb.min.x as i32 + gx as i32, bb.min.y as i32 + gy as i32);
                if px >= 0 && py >= 0 && (px as u32) < w && (py as u32) < h {
                    let i = (py as u32 * w + px as u32) as usize;
                    a[i] = (a[i] + cov).min(1.0);
                }
            });
        }
    }
    a.iter().flat_map(|&c| [255, 255, 255, (c * 255.0) as u8]).collect()
}

struct Shot<'a> {
    name: &'a str,
    layers: Vec<(&'a [u8], [u32; 2], Vec<EffectValues>)>,
}

fn main() {
    let out = PathBuf::from(std::env::args().nth(1).unwrap_or_else(|| ".".into()));
    std::fs::create_dir_all(&out).unwrap();
    let pack = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/neon-fx");
    let load = |n: &str| Arc::new(plugin::load_folder(&pack.join(n)).unwrap_or_else(|e| panic!("{n}: {e}")));
    let (saber, glow, outline, mask, laser, beam, twin) = (load("lightsaber"), load("neon-glow"), load("neon-outline"), load("neon-mask"), load("laser-blast"), load("laser-beam"), load("twin-laser"));
    plugin::set_installed(vec![saber.clone(), glow.clone(), outline.clone(), mask.clone(), laser.clone(), beam.clone(), twin.clone()]);

    let (device, queue) = headless_device().expect("a GPU (or lavapipe)");
    let mut c = Compositor::new(&device, &queue);
    let bg = backdrop();
    let ph = photo();
    let text = text_layer("NEON", 620, 240, 210.0);
    let full = [W, H];
    let tsize = [620, 240];

    let rgb = |c: u32| c as f32;
    let shots = vec![
        Shot { name: "01-lightsaber-blue", layers: vec![(&bg, full, vec![fx(&saber, &[], 0.3)])] },
        Shot {
            name: "02-lightsaber-duel-unstable-heat",
            layers: vec![(
                &ph,
                full,
                vec![
                    fx(&saber, &[(11, rgb(0xff1a12)), (7, 1.0), (0, 22.0), (1, 80.0), (2, 58.0), (3, 22.0), (21, 70.0), (22, 70.0), (5, 11.0), (6, 9.0)], 0.5),
                    fx(&saber, &[(11, rgb(0x22e04a)), (7, 2.0), (0, 80.0), (1, 85.0), (2, 40.0), (3, 20.0), (4, 85.0)], 0.5),
                ],
            )],
        },
        Shot { name: "03-lightsaber-halo-halation", layers: vec![(&bg, full, vec![fx(&saber, &[(11, rgb(0x9a3dff)), (5, 20.0), (6, 12.0), (9, 50.0), (10, 3.0), (12, 10.0), (13, 22.0), (14, 120.0), (17, 140.0), (18, 220.0)], 0.3)])] },
        Shot { name: "04-laser-beam-pulses", layers: vec![(&bg, full, vec![fx(&beam, &[(13, 5.0), (14, 120.0)], 0.3)])] },
        Shot { name: "05-laser-beam-wave-twist-particles", layers: vec![(&bg, full, vec![fx(&beam, &[(6, rgb(0x18e0ff)), (5, 7.0), (17, 28.0), (18, 180.0), (20, 85.0), (22, 70.0), (13, 3.0)], 0.3)])] },
        Shot { name: "06-laser-beam-distortion", layers: vec![(&ph, full, vec![fx(&beam, &[(6, rgb(0x22e04a)), (5, 12.0), (23, 90.0), (22, 50.0), (0, 5.0), (1, 30.0), (2, 95.0), (3, 75.0)], 0.3)])] },
        Shot { name: "07-laser-blast-sparks", layers: vec![(&bg, full, vec![fx(&laser, &[(4, 60.0), (21, 80.0)], 0.2)])] },
        Shot { name: "08-laser-blast-burst-impact-distortion", layers: vec![(&ph, full, vec![fx(&laser, &[(9, rgb(0x22e04a)), (16, 3.0), (4, 108.0), (17, 35.0), (1, 70.0), (3, 30.0), (21, 60.0), (22, 90.0), (18, 18.0)], 0.2)])] },
        Shot { name: "09-laser-blast-muzzle", layers: vec![(&bg, full, vec![fx(&laser, &[(4, 12.0)], 0.2)])] },
        Shot { name: "13-twin-laser", layers: vec![(&bg, full, vec![fx(&twin, &[], 0.3)])] },
        Shot { name: "14-twin-laser-pulses-bend-halo", layers: vec![(&bg, full, vec![fx(&twin, &[(5, rgb(0x22e04a)), (6, rgb(0xffc21a)), (14, 4.0), (15, 120.0), (17, -140.0), (13, 10.0), (8, 20.0)], 0.3)])] },
        Shot { name: "15-twin-laser-lightning-particles-wave", layers: vec![(&bg, full, vec![fx(&twin, &[(5, rgb(0x9a3dff)), (6, rgb(0x18e0ff)), (18, 30.0), (19, 300.0), (22, 90.0), (21, 70.0), (8, 22.0)], 0.3)])] },
        Shot { name: "16-twin-laser-distortion", layers: vec![(&ph, full, vec![fx(&twin, &[(5, rgb(0xff1a12)), (6, rgb(0xffffff)), (23, 80.0), (22, 40.0), (0, 8.0), (1, 25.0), (2, 92.0), (3, 80.0)], 0.3)])] },
        Shot { name: "10-neon-glow-text", layers: vec![(&bg, full, vec![]), (&text, tsize, vec![fx(&glow, &[], 0.2)])] },
        Shot { name: "11-neon-outline-draw-on", layers: vec![(&bg, full, vec![]), (&text, tsize, vec![fx(&outline, &[(0, 2.0), (5, 1.0), (9, 0.0), (10, 62.0), (11, -90.0)], 0.2)])] },
        Shot { name: "12-neon-mask-star", layers: vec![(&bg, full, vec![]), (&ph, full, vec![fx(&mask, &[(0, 4.0), (2, 5.0), (7, 10.0), (5, 70.0), (6, 70.0)], 0.2)])] },
    ];

    for (n, s) in shots.iter().enumerate() {
        let layers: Vec<LayerInput> = s
            .layers
            .iter()
            .enumerate()
            .map(|(k, (px, size, effects))| LayerInput {
                key: (n * 10 + k) as u64 + 1,
                content_id: (n * 10 + k) as u64 + 1,
                pixels: PixelData::Rgba(px),
                pixel_size: *size,
                layer_size: [size[0] as f32, size[1] as f32],
                transform: Transform::default(),
                blend: BlendMode::Normal,
                effects: effects.clone(),
                mask: None,
                wipe: None,
                mesh: None,
                space: None,
            })
            .collect();
        c.render(full, &layers);
        let rgba = c.read_output().unwrap();
        image::save_buffer(out.join(format!("{}.png", s.name)), &rgba, W, H, image::ColorType::Rgba8).unwrap();
        println!("{}", s.name);
    }
}
