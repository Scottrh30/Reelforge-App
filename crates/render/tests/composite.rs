//! Pixel-exact compositor tests on a headless GPU (skipped if no adapter is available).

use reelforge_core::{BlendMode, Transform};
use reelforge_render::{headless_device, Compositor, LayerInput};

fn solid(w: u32, h: u32, rgba: [u8; 4]) -> Vec<u8> {
    rgba.iter().copied().cycle().take((w * h * 4) as usize).collect()
}

fn layer<'a>(key: u64, pixels: &'a [u8], px: [u32; 2], size: [f32; 2], t: Transform, blend: BlendMode) -> LayerInput<'a> {
    LayerInput { key, content_id: 1, pixels, pixel_size: px, layer_size: size, transform: t, blend }
}

fn px(img: &[u8], w: u32, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * w + x) * 4) as usize;
    [img[i], img[i + 1], img[i + 2], img[i + 3]]
}

fn near(a: [u8; 4], b: [u8; 3]) -> bool {
    (0..3).all(|i| (a[i] as i32 - b[i] as i32).abs() <= 2)
}

fn setup() -> Option<Compositor> {
    let Some((d, q)) = headless_device() else {
        eprintln!("no GPU adapter; skipping");
        return None;
    };
    Some(Compositor::new(&d, &q))
}

#[test]
fn opacity_mixes_in_linear_light_and_position_offsets() {
    let Some(mut c) = setup() else { return };
    let red = solid(64, 32, [255, 0, 0, 255]);
    let blue = solid(2, 2, [0, 0, 255, 255]);
    let top = Transform { position: [10.0, 0.0], opacity: 0.5, ..Default::default() };
    c.render([64, 32], &[
        layer(1, &red, [64, 32], [64.0, 32.0], Transform::default(), BlendMode::Normal),
        layer(2, &blue, [2, 2], [16.0, 16.0], top, BlendMode::Normal),
    ]);
    let img = c.read_output().unwrap();
    // 50/50 in linear light is 0.5 -> sRGB 188 (a gamma-space mix would give 128).
    assert!(near(px(&img, 64, 42, 16), [188, 0, 188]), "{:?}", px(&img, 64, 42, 16));
    assert!(near(px(&img, 64, 30, 16), [255, 0, 0]), "left of the blue square stays red");
    assert!(near(px(&img, 64, 42, 4), [255, 0, 0]), "above it stays red");
}

#[test]
fn blend_modes() {
    let Some(mut c) = setup() else { return };
    let gray = solid(8, 8, [188, 188, 188, 255]); // linear 0.5
    let cases = [
        (BlendMode::Normal, [188, 188, 188]),
        (BlendMode::Add, [255, 255, 255]),      // 0.5 + 0.5
        (BlendMode::Multiply, [137, 137, 137]), // 0.25
        (BlendMode::Screen, [225, 225, 225]),   // 1 - 0.5*0.5 = 0.75
    ];
    for (mode, want) in cases {
        c.render([8, 8], &[
            layer(1, &gray, [8, 8], [8.0, 8.0], Transform::default(), BlendMode::Normal),
            layer(2, &gray, [8, 8], [8.0, 8.0], Transform::default(), mode),
        ]);
        let got = px(&c.read_output().unwrap(), 8, 4, 4);
        assert!(near(got, want), "{mode:?}: got {got:?}, want {want:?}");
    }
}

#[test]
fn multiply_respects_opacity() {
    let Some(mut c) = setup() else { return };
    let white = solid(4, 4, [255, 255, 255, 255]);
    let black = solid(4, 4, [0, 0, 0, 255]);
    let half = Transform { opacity: 0.5, ..Default::default() };
    c.render([4, 4], &[
        layer(1, &white, [4, 4], [4.0, 4.0], Transform::default(), BlendMode::Normal),
        layer(2, &black, [4, 4], [4.0, 4.0], half, BlendMode::Multiply),
    ]);
    // white * lerp(1, 0, 0.5) = 0.5 linear -> 188
    assert!(near(px(&c.read_output().unwrap(), 4, 2, 2), [188, 188, 188]));
}

#[test]
fn rotation_and_scale() {
    let Some(mut c) = setup() else { return };
    let white = solid(1, 1, [255, 255, 255, 255]);
    // A 20x4 bar rotated 90° becomes vertical.
    let t = Transform { rotation: 90.0, ..Default::default() };
    c.render([32, 32], &[layer(1, &white, [1, 1], [20.0, 4.0], t, BlendMode::Normal)]);
    let img = c.read_output().unwrap();
    assert!(near(px(&img, 32, 16, 24), [255, 255, 255]), "below centre is covered");
    assert!(near(px(&img, 32, 24, 16), [0, 0, 0]), "right of centre is not");

    // Scale 2x: a 4x4 layer covers 8x8.
    let t = Transform { scale: [2.0, 2.0], ..Default::default() };
    c.render([32, 32], &[layer(1, &white, [1, 1], [4.0, 4.0], t, BlendMode::Normal)]);
    let img = c.read_output().unwrap();
    assert!(near(px(&img, 32, 19, 19), [255, 255, 255]));
    assert!(near(px(&img, 32, 21, 21), [0, 0, 0]));
}

#[test]
fn textures_are_cached_by_content_id() {
    let Some(mut c) = setup() else { return };
    let red = solid(4, 4, [255, 0, 0, 255]);
    let green = solid(4, 4, [0, 255, 0, 255]);
    fn mk(pixels: &[u8], id: u64) -> LayerInput<'_> {
        LayerInput { key: 7, content_id: id, pixels, pixel_size: [4, 4], layer_size: [4.0, 4.0], transform: Transform::default(), blend: BlendMode::Normal }
    }
    c.render([4, 4], &[mk(&red, 1)]);
    c.render([4, 4], &[mk(&green, 1)]); // same content id: not re-uploaded
    assert!(near(px(&c.read_output().unwrap(), 4, 1, 1), [255, 0, 0]));
    c.render([4, 4], &[mk(&green, 2)]);
    assert!(near(px(&c.read_output().unwrap(), 4, 1, 1), [0, 255, 0]));
}

#[test]
fn resizing_the_frame_bumps_output_generation() {
    let Some(mut c) = setup() else { return };
    c.render([16, 16], &[]);
    let g = c.output_generation;
    c.render([16, 16], &[]);
    assert_eq!(c.output_generation, g);
    c.render([32, 16], &[]);
    assert_eq!(c.output_generation, g + 1);
    assert_eq!(c.read_output().unwrap().len(), 32 * 16 * 4);
}
