//! The 3D text and model preferences (anti-aliasing, reflection map size),
//! which are app-wide: a test binary of their own, so changing them can't
//! disturb other tests (skipped without a GPU adapter).

use reelforge_core::environment::load;
use reelforge_core::space3d::{solid_space, Camera, EnvLight, Light, Scene3d};
use reelforge_core::{BlendMode, TitleSpec, Transform};
use reelforge_render::text3d::text_mesh;
use reelforge_render::{headless_device, Compositor, LayerInput, PixelData, SolidInput};

const W: u32 = 480;
const H: u32 = 270;

fn render(c: &mut Compositor, spec: &TitleSpec, t: Transform, scene: &Scene3d) -> Vec<u8> {
    let input = SolidInput { mesh: text_mesh(spec, [W, H]), space: solid_space(&t, scene), face: spec.color, side: spec.side_color.unwrap_or(spec.color) };
    let layer = LayerInput {
        key: 1,
        content_id: input.content_id(),
        pixels: PixelData::Solid(&input),
        pixel_size: [W, H],
        layer_size: [W as f32, H as f32],
        transform: Transform::default(),
        blend: BlendMode::Normal,
        effects: Vec::new(),
        mask: None,
        wipe: None,
        mesh: None,
        space: None,
    };
    c.render([W, H], &[layer]);
    c.read_output().unwrap()
}

#[test]
fn anti_aliasing_and_reflection_map_size() {
    let Some((d, q)) = headless_device() else { return };
    let mut c = Compositor::new(&d, &q);
    let spec = TitleSpec { text: "WAVE".into(), size: 110.0, color: [230, 40, 30, 255], shadow: false, extrude: 30.0, side_color: Some([40, 90, 230, 255]), ..TitleSpec::default() };
    let turned = Transform { three_d: true, orientation: [12.0, 25.0], rotation: 8.0, ..Default::default() };
    let sc = Scene3d { camera: Camera::default_for([W as f32, H as f32]), lights: Vec::new() };
    // Where a face meets a side, pixels part one and part the other are
    // mixed (purple between the red faces and blue sides) with 4 samples a
    // pixel; with one, each pixel is one or the other.
    let soft = |img: &[u8]| img.chunks_exact(4).filter(|p| p[0] > 70 && p[2] > 70 && p[0].abs_diff(p[2]) < 120).count();
    reelforge_render::set_solid_samples(4);
    let first = render(&mut c, &spec, turned, &sc);
    let smooth = soft(&first);
    reelforge_render::set_solid_samples(1);
    let stepped = render(&mut c, &spec, turned, &sc);
    reelforge_render::set_solid_samples(4);
    let lit = stepped.chunks_exact(4).filter(|p| p[0].max(p[1]).max(p[2]) > 100).count();
    assert!(lit > 5000, "drawn without anti-aliasing too: {lit}");
    assert!(smooth > 400 && soft(&stepped) * 10 < smooth, "smooth {smooth}, stepped {}", soft(&stepped));

    // An environment half red, half green: at full size a mirror shows one
    // or the other; shrunk to one pixel, the two mixed everywhere.
    let (w, h) = (64usize, 32usize);
    let mut file = format!("#?RADIANCE\nFORMAT=32-bit_rle_rgbe\n\n-Y {h} +X {w}\n").into_bytes();
    for _y in 0..h {
        for x in 0..w {
            file.extend(if x < w / 2 { [128, 0, 0, 129] } else { [0, 128, 0, 129] });
        }
    }
    let dir = std::env::temp_dir().join(format!("rf_reflsize_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("halves.hdr");
    std::fs::write(&path, &file).unwrap();
    let env = load(&path).unwrap();
    let light = Light { kind: 4, color: [1.0; 3], position: [0.0; 3], direction: [0.0, 0.0, 1.0], cone: [0.5, 0.6], falloff: 0, radius: 1000.0, shadow: 0.0, softness: 0.0, env: Some(EnvLight { id: env.id, sh: env.turned_sh(0.0), turn: 0.0, reflect: 1.0, blur: 0.0 }) };
    let lit_sc = Scene3d { camera: sc.camera, lights: vec![light] };
    // How far apart red and green are, at most, over the solid pixels.
    let spread = |img: &[u8]| img.chunks_exact(4).filter(|p| p[0].max(p[1]) > 100).map(|p| (p[0] as i32 - p[1] as i32).abs()).max().unwrap_or(0);
    reelforge_render::set_reflection_map_size(0);
    let sharp = spread(&render(&mut c, &spec, turned, &lit_sc));
    reelforge_render::set_reflection_map_size(1);
    let mixed = spread(&render(&mut c, &spec, turned, &lit_sc));
    reelforge_render::set_reflection_map_size(2048);
    assert!(sharp > 150 && mixed + 100 < sharp, "full size {sharp}, one pixel {mixed}");
    let _ = std::fs::remove_dir_all(&dir);
}
