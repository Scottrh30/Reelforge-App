//! Pixel-exact compositor tests on a headless GPU (skipped if no adapter is available).

use reelforge_core::{BlendMode, Transform};
use reelforge_render::{headless_device, Compositor, LayerInput};

fn solid(w: u32, h: u32, rgba: [u8; 4]) -> Vec<u8> {
    rgba.iter().copied().cycle().take((w * h * 4) as usize).collect()
}

fn layer<'a>(key: u64, pixels: &'a [u8], px: [u32; 2], size: [f32; 2], t: Transform, blend: BlendMode) -> LayerInput<'a> {
    LayerInput { key, content_id: 1, pixels, pixel_size: px, layer_size: size, transform: t, blend, effects: Vec::new(), mask: None }
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
        LayerInput { key: 7, content_id: id, pixels, pixel_size: [4, 4], layer_size: [4.0, 4.0], transform: Transform::default(), blend: BlendMode::Normal, effects: Vec::new(), mask: None }
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

use reelforge_render::DrawItem;

fn full(key: u64, pixels: &[u8], frame: [u32; 2]) -> LayerInput<'_> {
    layer(key, pixels, [1, 1], [frame[0] as f32, frame[1] as f32], Transform::default(), BlendMode::Normal)
}

#[test]
fn full_frame_dissolve_mixes_in_linear_light() {
    let Some(mut c) = setup() else { return };
    let red = solid(1, 1, [255, 0, 0, 255]);
    let blue = solid(1, 1, [0, 0, 255, 255]);
    for (mix, want) in [(0.0, [255, 0, 0]), (0.5, [188, 0, 188]), (1.0, [0, 0, 255])] {
        c.render_items([16, 16], &[DrawItem::Dissolve { from: full(1, &red, [16, 16]), to: full(2, &blue, [16, 16]), mix }]);
        let got = px(&c.read_output().unwrap(), 16, 8, 8);
        assert!(near(got, want), "mix {mix}: got {got:?}, want {want:?}");
    }
}

#[test]
fn dissolve_between_small_layers_has_no_midpoint_dip() {
    let Some(mut c) = setup() else { return };
    let gray = solid(1, 1, [188, 188, 188, 255]); // 0.5 linear
    let white = solid(1, 1, [255, 255, 255, 255]);
    // A centred 8x8; B the same size shifted right by 4, so they half overlap.
    let a = layer(2, &white, [1, 1], [8.0, 8.0], Transform::default(), BlendMode::Normal);
    let b = layer(3, &white, [1, 1], [8.0, 8.0], Transform { position: [4.0, 0.0], ..Default::default() }, BlendMode::Normal);
    c.render_items([32, 32], &[DrawItem::Layer(full(1, &gray, [32, 32])), DrawItem::Dissolve { from: a, to: b, mix: 0.5 }]);
    let img = c.read_output().unwrap();
    // Where both cover: 0.5·white + 0.5·white = white (a naive A-then-B fade would darken this).
    assert!(near(px(&img, 32, 17, 16), [255, 255, 255]), "overlap: {:?}", px(&img, 32, 17, 16));
    // Only A: 0.5·white + 0.5·gray(0.5) = 0.75 linear -> 225.
    assert!(near(px(&img, 32, 13, 16), [225, 225, 225]), "A only: {:?}", px(&img, 32, 13, 16));
    // Only B: same.
    assert!(near(px(&img, 32, 21, 16), [225, 225, 225]), "B only: {:?}", px(&img, 32, 21, 16));
    // Untouched background.
    assert!(near(px(&img, 32, 2, 2), [188, 188, 188]));
}

#[test]
fn layers_after_a_dissolve_still_composite_on_top() {
    let Some(mut c) = setup() else { return };
    let red = solid(1, 1, [255, 0, 0, 255]);
    let blue = solid(1, 1, [0, 0, 255, 255]);
    let green = solid(1, 1, [0, 255, 0, 255]);
    let top = layer(9, &green, [1, 1], [4.0, 4.0], Transform::default(), BlendMode::Normal);
    c.render_items([16, 16], &[
        DrawItem::Dissolve { from: full(1, &red, [16, 16]), to: full(2, &blue, [16, 16]), mix: 0.5 },
        DrawItem::Layer(top),
    ]);
    let img = c.read_output().unwrap();
    assert!(near(px(&img, 16, 8, 8), [0, 255, 0]), "green on top");
    assert!(near(px(&img, 16, 1, 1), [188, 0, 188]), "dissolve underneath");
}

// ---- Effects ----------------------------------------------------------------

use reelforge_core::{Effect, EffectKind, EffectValues};

fn fx(kind: EffectKind, set: &[(usize, f32)]) -> EffectValues {
    let mut e = Effect::new(kind);
    for &(i, v) in set {
        e.params[i].value = v;
    }
    e.eval(reelforge_core::Time::ZERO)
}

fn with_fx<'a>(mut l: LayerInput<'a>, effects: Vec<EffectValues>) -> LayerInput<'a> {
    l.effects = effects;
    l
}

/// 8-bit sRGB code for a linear value.
fn srgb(lin: f32) -> u8 {
    let v = if lin <= 0.003_130_8 { lin * 12.92 } else { 1.055 * lin.powf(1.0 / 2.4) - 0.055 };
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

#[test]
fn colour_correction_works_in_linear_light() {
    let Some(mut c) = setup() else { return };
    let grey = solid(1, 1, [srgb(0.18), srgb(0.18), srgb(0.18), 255]);
    // +1 stop doubles the light.
    c.render([8, 8], &[with_fx(full(1, &grey, [8, 8]), vec![fx(EffectKind::ColorCorrect, &[(0, 1.0)])])]);
    let v = srgb(0.36);
    assert!(near(px(&c.read_output().unwrap(), 8, 4, 4), [v, v, v]), "{:?}", px(&c.read_output().unwrap(), 8, 4, 4));
    // No saturation: pure red becomes grey of the same luminance.
    let red = solid(1, 1, [255, 0, 0, 255]);
    c.render([8, 8], &[with_fx(full(2, &red, [8, 8]), vec![fx(EffectKind::ColorCorrect, &[(2, 0.0)])])]);
    let g = srgb(0.2126);
    assert!(near(px(&c.read_output().unwrap(), 8, 4, 4), [g, g, g]));
    // Contrast pivots on 18% grey, which stays put.
    c.render([8, 8], &[with_fx(full(3, &grey, [8, 8]), vec![fx(EffectKind::ColorCorrect, &[(1, 180.0)])])]);
    let v = srgb(0.18);
    assert!(near(px(&c.read_output().unwrap(), 8, 4, 4), [v, v, v]));
}

#[test]
fn blur_softens_edges_but_keeps_flat_areas() {
    let Some(mut c) = setup() else { return };
    // 64×8: left half black, right half white.
    let mut img = Vec::new();
    for _y in 0..8 {
        for x in 0..64 {
            let v = if x < 32 { 0 } else { 255 };
            img.extend([v, v, v, 255]);
        }
    }
    let l = layer(1, &img, [64, 8], [64.0, 8.0], Transform::default(), BlendMode::Normal);
    c.render([64, 8], &[with_fx(l, vec![fx(EffectKind::Blur, &[(0, 6.0)])])]);
    let out = c.read_output().unwrap();
    let at = |x| px(&out, 64, x, 4)[0];
    assert!(at(2) <= 1 && at(61) >= 254, "far from the edge nothing changes: {} {}", at(2), at(61));
    // Either side of the edge meet near 50% linear light.
    let mid = (at(31) as i32 + at(32) as i32) / 2;
    assert!((mid - srgb(0.5) as i32).abs() <= 12, "edge {mid}");
    assert!(at(29) > 0 && at(34) < 255, "the edge is spread out");
    // Monotonic across the edge.
    assert!((26..38).all(|x| at(x) <= at(x + 1)));
}

#[test]
fn chroma_key_removes_green_and_keeps_other_colours() {
    let Some(mut c) = setup() else { return };
    let red = solid(1, 1, [255, 0, 0, 255]);
    // Top half: a typical green screen; bottom half: a skin tone.
    let mut img = Vec::new();
    for y in 0..8 {
        for _x in 0..8 {
            img.extend(if y < 4 { [50, 180, 80, 255] } else { [205, 150, 125, 255] });
        }
    }
    let fg = layer(2, &img, [8, 8], [8.0, 8.0], Transform::default(), BlendMode::Normal);
    c.render([8, 8], &[full(1, &red, [8, 8]), with_fx(fg, vec![fx(EffectKind::ChromaKey, &[])])]);
    let out = c.read_output().unwrap();
    assert!(near(px(&out, 8, 4, 1), [255, 0, 0]), "green is keyed out to the red below: {:?}", px(&out, 8, 4, 1));
    assert!(near(px(&out, 8, 4, 6), [205, 150, 125]), "skin stays: {:?}", px(&out, 8, 4, 6));
    // A blue key leaves the green screen alone.
    let fg = layer(3, &img, [8, 8], [8.0, 8.0], Transform::default(), BlendMode::Normal);
    c.render([8, 8], &[full(1, &red, [8, 8]), with_fx(fg, vec![fx(EffectKind::ChromaKey, &[(0, 230.0), (3, 0.0)])])]);
    assert!(near(px(&c.read_output().unwrap(), 8, 4, 1), [50, 180, 80]));
}

#[test]
fn vignette_darkens_corners_only() {
    let Some(mut c) = setup() else { return };
    // Effects run at the layer's own pixel resolution, so give it real pixels.
    let white = solid(32, 32, [255, 255, 255, 255]);
    let l = layer(1, &white, [32, 32], [32.0, 32.0], Transform::default(), BlendMode::Normal);
    c.render([32, 32], &[with_fx(l, vec![fx(EffectKind::Vignette, &[(0, 100.0)])])]);
    let out = c.read_output().unwrap();
    assert!(near(px(&out, 32, 16, 16), [255, 255, 255]));
    assert!(px(&out, 32, 0, 0)[0] < 60, "corner {:?}", px(&out, 32, 0, 0));
}

#[test]
fn effects_follow_the_layer_and_work_in_dissolves() {
    let Some(mut c) = setup() else { return };
    let white = solid(1, 1, [255, 255, 255, 255]);
    let black = solid(1, 1, [0, 0, 0, 255]);
    // A white layer at −1 stop, half-size and moved: the effect is applied inside its footprint only.
    let t = Transform { position: [8.0, 0.0], scale: [0.5, 0.5], rotation: 0.0, opacity: 1.0 };
    let l = with_fx(layer(1, &white, [1, 1], [16.0, 16.0], t, BlendMode::Normal), vec![fx(EffectKind::ColorCorrect, &[(0, -1.0)])]);
    c.render([16, 16], &[full(9, &black, [16, 16]), l]);
    let out = c.read_output().unwrap();
    let half = srgb(0.5);
    assert!(near(px(&out, 16, 12, 8), [half, half, half]), "{:?}", px(&out, 16, 12, 8));
    assert!(near(px(&out, 16, 2, 8), [0, 0, 0]));
    // A dissolve where one side has effects still sums to the right mix.
    let a = with_fx(full(1, &white, [16, 16]), vec![fx(EffectKind::ColorCorrect, &[(0, -1.0)])]);
    c.render_items([16, 16], &[DrawItem::Dissolve { from: a, to: full(2, &black, [16, 16]), mix: 0.5 }]);
    let q = srgb(0.25);
    assert!(near(px(&c.read_output().unwrap(), 16, 8, 8), [q, q, q]));
}
