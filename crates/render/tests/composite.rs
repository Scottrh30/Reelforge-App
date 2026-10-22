//! Pixel-exact compositor tests on a headless GPU (skipped if no adapter is available).

use reelforge_core::{BlendMode, Transform};
use reelforge_render::{headless_device, Compositor, LayerInput};

fn solid(w: u32, h: u32, rgba: [u8; 4]) -> Vec<u8> {
    rgba.iter().copied().cycle().take((w * h * 4) as usize).collect()
}

fn layer<'a>(key: u64, pixels: &'a [u8], px: [u32; 2], size: [f32; 2], t: Transform, blend: BlendMode) -> LayerInput<'a> {
    LayerInput { key, content_id: 1, pixels: PixelData::Rgba(pixels), pixel_size: px, layer_size: size, transform: t, blend, effects: Vec::new(), mask: None, wipe: None, mesh: None , space: None}
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
        LayerInput { key: 7, content_id: id, pixels: PixelData::Rgba(pixels), pixel_size: [4, 4], layer_size: [4.0, 4.0], transform: Transform::default(), blend: BlendMode::Normal, effects: Vec::new(), mask: None, wipe: None, mesh: None , space: None}
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

use reelforge_render::{DrawItem, PixelData};

fn full(key: u64, pixels: &[u8], frame: [u32; 2]) -> LayerInput<'_> {
    layer(key, pixels, [1, 1], [frame[0] as f32, frame[1] as f32], Transform::default(), BlendMode::Normal)
}

#[test]
fn full_frame_dissolve_mixes_in_linear_light() {
    let Some(mut c) = setup() else { return };
    let red = solid(1, 1, [255, 0, 0, 255]);
    let blue = solid(1, 1, [0, 0, 255, 255]);
    for (mix, want) in [(0.0, [255, 0, 0]), (0.5, [188, 0, 188]), (1.0, [0, 0, 255])] {
        c.render_items([16, 16], &[DrawItem::Dissolve { dip: None, from: full(1, &red, [16, 16]), to: full(2, &blue, [16, 16]), mix }]);
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
    c.render_items([32, 32], &[DrawItem::Layer(full(1, &gray, [32, 32])), DrawItem::Dissolve { dip: None, from: a, to: b, mix: 0.5 }]);
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
        DrawItem::Dissolve { dip: None, from: full(1, &red, [16, 16]), to: full(2, &blue, [16, 16]), mix: 0.5 },
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
    let t = Transform { position: [8.0, 0.0], scale: [0.5, 0.5], ..Default::default() };
    let l = with_fx(layer(1, &white, [1, 1], [16.0, 16.0], t, BlendMode::Normal), vec![fx(EffectKind::ColorCorrect, &[(0, -1.0)])]);
    c.render([16, 16], &[full(9, &black, [16, 16]), l]);
    let out = c.read_output().unwrap();
    let half = srgb(0.5);
    assert!(near(px(&out, 16, 12, 8), [half, half, half]), "{:?}", px(&out, 16, 12, 8));
    assert!(near(px(&out, 16, 2, 8), [0, 0, 0]));
    // A dissolve where one side has effects still sums to the right mix.
    let a = with_fx(full(1, &white, [16, 16]), vec![fx(EffectKind::ColorCorrect, &[(0, -1.0)])]);
    c.render_items([16, 16], &[DrawItem::Dissolve { dip: None, from: a, to: full(2, &black, [16, 16]), mix: 0.5 }]);
    let q = srgb(0.25);
    assert!(near(px(&c.read_output().unwrap(), 16, 8, 8), [q, q, q]));
}

// ---- New effects ---------------------------------------------------------------

#[test]
fn levels_curves_and_luts_remap_colours() {
    let Some(mut c) = setup() else { return };
    let grey = solid(1, 1, [128, 128, 128, 255]);
    // Levels: input white at 128 makes mid-grey white.
    c.render([4, 4], &[with_fx(full(1, &grey, [4, 4]), vec![fx(EffectKind::Levels, &[(1, 128.0)])])]);
    assert!(near(px(&c.read_output().unwrap(), 4, 2, 2), [255, 255, 255]));
    // Curves: a master curve that halves everything (display values).
    let mut e = Effect::new(EffectKind::Curves);
    e.data = Some(reelforge_core::EffectData::Curves(reelforge_core::lut::Curves { master: vec![[0.0, 0.0], [1.0, 0.5]], ..Default::default() }));
    let white = solid(1, 1, [255, 255, 255, 255]);
    c.render([4, 4], &[with_fx(full(2, &white, [4, 4]), vec![e.eval(reelforge_core::Time::ZERO)])]);
    let v = px(&c.read_output().unwrap(), 4, 2, 2)[0] as i32;
    assert!((v - 128).abs() <= 8, "curves: {v}");
    // A LUT that swaps red and blue.
    let mut text = String::from("LUT_3D_SIZE 2\n");
    for b in 0..2 {
        for g in 0..2 {
            for r in 0..2 {
                text += &format!("{b} {g} {r}\n");
            }
        }
    }
    let path = std::env::temp_dir().join("rf_swap.cube");
    std::fs::write(&path, text).unwrap();
    let mut e = Effect::new(EffectKind::Lut);
    e.data = Some(reelforge_core::EffectData::Lut(path));
    let red = solid(1, 1, [255, 0, 0, 255]);
    c.render([4, 4], &[with_fx(full(3, &red, [4, 4]), vec![e.eval(reelforge_core::Time::ZERO)])]);
    assert!(near(px(&c.read_output().unwrap(), 4, 2, 2), [0, 0, 255]), "{:?}", px(&c.read_output().unwrap(), 4, 2, 2));
}

#[test]
fn sharpen_glow_and_drop_shadow() {
    let Some(mut c) = setup() else { return };
    // Sharpen: a soft step gets an overshoot on its bright side.
    let mut img = Vec::new();
    for _y in 0..8 {
        for x in 0..32 {
            let v = if x < 15 { 60 } else if x > 16 { 190 } else { 125 };
            img.extend([v, v, v, 255]);
        }
    }
    let l = layer(1, &img, [32, 8], [32.0, 8.0], Transform::default(), BlendMode::Normal);
    c.render([32, 8], &[with_fx(l, vec![fx(EffectKind::Sharpen, &[(0, 200.0), (1, 1.5)])])]);
    let out = c.read_output().unwrap();
    assert!(px(&out, 32, 17, 4)[0] > 200 && px(&out, 32, 14, 4)[0] < 50, "{:?} {:?}", px(&out, 32, 17, 4), px(&out, 32, 14, 4));
    assert!((px(&out, 32, 2, 4)[0] as i32 - 60).abs() <= 2, "flat areas stay");

    // Glow: a white square on black spreads light around it.
    let mut img = vec![0u8; 32 * 32 * 4];
    for y in 0..32 {
        for x in 0..32 {
            let v = if (12..20).contains(&x) && (12..20).contains(&y) { 255 } else { 0 };
            img[(y * 32 + x) * 4..][..4].copy_from_slice(&[v, v, v, 255]);
        }
    }
    let l = layer(2, &img, [32, 32], [32.0, 32.0], Transform::default(), BlendMode::Normal);
    c.render([32, 32], &[with_fx(l, vec![fx(EffectKind::Glow, &[(1, 6.0)])])]);
    let out = c.read_output().unwrap();
    assert!(px(&out, 32, 9, 16)[0] > 30, "glow outside the square: {:?}", px(&out, 32, 9, 16));
    assert!(px(&out, 32, 1, 1)[0] < 10, "but not far away");

    // Drop shadow: a white square with nothing around it, over red: the shadow falls down-right.
    let mut img = vec![0u8; 32 * 32 * 4];
    for y in 8..16 {
        for x in 8..16 {
            img[(y * 32 + x) * 4..][..4].copy_from_slice(&[255, 255, 255, 255]);
        }
    }
    let red = solid(1, 1, [255, 0, 0, 255]);
    let l = layer(3, &img, [32, 32], [32.0, 32.0], Transform::default(), BlendMode::Normal);
    c.render([32, 32], &[full(9, &red, [32, 32]), with_fx(l, vec![fx(EffectKind::DropShadow, &[(0, 6.0), (2, 0.0), (3, 100.0)])])]);
    let out = c.read_output().unwrap();
    assert!(near(px(&out, 32, 12, 12), [255, 255, 255]), "the square itself");
    assert!(px(&out, 32, 19, 19)[0] < 40, "shadow down-right: {:?}", px(&out, 32, 19, 19));
    assert!(near(px(&out, 32, 4, 4), [255, 0, 0]), "no shadow up-left");
}

#[test]
fn energy_blade_core_glow_colour_length_and_motion_blur() {
    use reelforge_core::effects::blade as b;
    let Some(mut c) = setup() else { return };
    let black = solid(200, 100, [0, 0, 0, 255]);
    // A 200×100 black layer; a horizontal blade from (20%, 50%) to (80%, 50%): x 40–160 at y 50.
    let base = [(b::HILT_X, 20.0), (b::HILT_Y, 50.0), (b::TIP_X, 80.0), (b::TIP_Y, 50.0), (b::FLICKER, 0.0), (b::WOBBLE, 0.0), (b::TAPER, 0.0), (b::CORE_WIDTH, 8.0), (b::GLOW_SIZE, 20.0)];
    let draw = |c: &mut reelforge_render::Compositor, v: EffectValues| {
        c.render([200, 100], &[with_fx(layer(1, &black, [200, 100], [200.0, 100.0], Transform::default(), BlendMode::Normal), vec![v])]);
        c.read_output().unwrap()
    };
    let out = draw(&mut c, fx(EffectKind::EnergyBlade, &base));
    let core = px(&out, 200, 100, 50);
    assert!(core[0] > 230 && core[1] > 230 && core[2] > 240, "white-hot core: {core:?}");
    let glow = px(&out, 200, 100, 62);
    assert!(glow[2] > 60 && glow[2] > glow[0] + 30, "blue glow beside the blade: {glow:?}");
    let far = px(&out, 200, 100, 98);
    assert!(far[2] < glow[2] / 2, "fades with distance: {far:?}");
    assert!(px(&out, 200, 190, 50)[2] < core[2] / 2, "nothing much past the tip");

    // Red blade.
    let mut red = base.to_vec();
    red.push((b::BLADE_COLOR, 0xff0000 as f32));
    let g = px(&draw(&mut c, fx(EffectKind::EnergyBlade, &red)), 200, 100, 62);
    assert!(g[0] > 60 && g[0] > g[2] + 30, "red glow: {g:?}");

    // Half length: the blade ends at x 100.
    let mut half = base.to_vec();
    half.push((b::LENGTH, 50.0));
    let out = draw(&mut c, fx(EffectKind::EnergyBlade, &half));
    assert!(px(&out, 200, 70, 50)[0] > 230 && px(&out, 200, 140, 50)[0] < 120, "{:?} {:?}", px(&out, 200, 70, 50), px(&out, 200, 140, 50));

    // Motion blur: a frame ago the blade was 30 px higher, so the sweep between glows.
    let mut v = fx(EffectKind::EnergyBlade, &[base.as_slice(), &[(b::MOTION_BLUR, 100.0)]].concat());
    v.prev = Some([20.0, 20.0, 80.0, 20.0]);
    let out = draw(&mut c, v.clone());
    let between = px(&out, 200, 100, 35);
    v.prev = None;
    let sharp = px(&draw(&mut c, v), 200, 100, 35);
    assert!(between[2] > sharp[2] + 30, "smeared over its sweep: {between:?} vs {sharp:?}");
}

#[test]
fn frames_on_the_graphics_card_fall_back_to_the_usual_copy() {
    use reelforge_core::frame::{zero_copy, GpuFrame, Pixels, YuvLayout, YuvMatrix, YuvPlanes};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    let Some(mut c) = setup() else { return };
    // A 16×8 NV12 picture: a luma ramp, reddish chroma.
    let (w, h) = (16u32, 8u32);
    let y: Vec<u8> = (0..w * h).map(|i| 16 + (i % w) as u8 * 13).collect();
    let uv: Vec<u8> = (0..(w / 2) * (h / 2)).flat_map(|_| [100u8, 200]).collect();
    let planes = YuvPlanes { layout: YuvLayout::Nv12, width: w, height: h, planes: vec![y, uv], matrix: YuvMatrix::Bt709, full_range: false };
    let draw = |c: &mut reelforge_render::Compositor, px: &Pixels, id| {
        c.render([w, h], &[LayerInput { content_id: id, pixels: PixelData::from(px), ..layer(1, &[], [w, h], [w as f32, h as f32], Transform::default(), BlendMode::Normal) }]);
        c.read_output().unwrap()
    };
    let usual = draw(&mut c, &Pixels::Yuv(Arc::new(planes.clone())), 1);
    // The same frame "left on the graphics card": with no way to share it here,
    // the compositor copies it back (once) and draws the same picture.
    let fetched = Arc::new(AtomicUsize::new(0));
    let (p2, f2) = (planes.clone(), fetched.clone());
    let g = GpuFrame::new(YuvLayout::Nv12, [w, h], YuvMatrix::Bt709, false, (0, 1, [w, h]), (0, 1), 0, Arc::new(()), Arc::new(move || {
        f2.fetch_add(1, Ordering::Relaxed);
        Some(p2.clone())
    }));
    let gpu = Pixels::Gpu(Arc::new(g));
    assert_eq!(draw(&mut c, &gpu, 2), usual);
    assert_eq!(draw(&mut c, &gpu, 3), usual);
    assert_eq!(fetched.load(Ordering::Relaxed), 1, "copied back once, then reused");
    assert_eq!(gpu.to_rgba().len(), (w * h * 4) as usize, "and readable on the CPU");
    assert!(zero_copy::failure().is_none(), "not being able to share isn't a failure");
}

#[test]
fn layers_in_3d_perspective_and_light() {
    use reelforge_core::space3d::{layer_space, Camera, Light, Scene3d};
    let Some(mut c) = setup() else { return };
    let frame = [200u32, 100u32];
    let f = [200.0, 100.0];
    let black = solid(4, 4, [0, 0, 0, 255]);
    let white = solid(64, 64, [255, 255, 255, 255]);
    let cam = Camera::default_for(f);
    let draw = |c: &mut reelforge_render::Compositor, t: Transform, sp: &reelforge_core::space3d::LayerSpace| {
        c.render(frame, &[layer(1, &black, [4, 4], f, Transform::default(), BlendMode::Normal), LayerInput { space: Some(sp), ..layer(2, &white, [64, 64], [64.0, 64.0], t, BlendMode::Normal) }]);
        c.read_output().unwrap()
    };
    // At Z = 0: the same 64-pixel square as in 2D.
    let t = Transform { three_d: true, ..Default::default() };
    let unlit = Scene3d { camera: cam, lights: Vec::new() };
    let out = draw(&mut c, t, &layer_space(&t, [64.0, 64.0], &unlit));
    assert!(near(px(&out, 200, 100 + 30, 50), [255, 255, 255]) && near(px(&out, 200, 100 + 34, 50), [0, 0, 0]));
    // Twice as far from the camera: half the size.
    let far = Transform { three_d: true, depth: cam.focal(), ..Default::default() };
    let out = draw(&mut c, far, &layer_space(&far, [64.0, 64.0], &unlit));
    assert!(near(px(&out, 200, 100 + 14, 50), [255, 255, 255]) && near(px(&out, 200, 100 + 18, 50), [0, 0, 0]), "{:?}", px(&out, 200, 100 + 18, 50));
    // Behind the camera: nothing drawn (and no garbage).
    let behind = Transform { three_d: true, depth: -cam.focal() * 3.0, ..Default::default() };
    let out = draw(&mut c, behind, &layer_space(&behind, [64.0, 64.0], &unlit));
    assert!(near(px(&out, 200, 100, 50), [0, 0, 0]) && near(px(&out, 200, 10, 10), [0, 0, 0]));

    // A red spot light aimed at the middle: the middle is red, the edge of the square dark.
    let spot = Light { kind: 1, color: [1.0, 0.0, 0.0], position: [0.0, 0.0, -200.0], direction: [0.0, 0.0, 1.0], cone: [(8f32).to_radians().cos(), (4f32).to_radians().cos()], falloff: 0, radius: 1000.0 };
    let lit = Scene3d { camera: cam, lights: vec![spot] };
    let out = draw(&mut c, t, &layer_space(&t, [64.0, 64.0], &lit));
    let mid = px(&out, 200, 100, 50);
    assert!(mid[0] > 240 && mid[1] < 10 && mid[2] < 10, "red in the middle: {mid:?}");
    assert!(px(&out, 200, 100 + 28, 50 + 28)[0] < 20, "dark outside the cone: {:?}", px(&out, 200, 128, 78));
    // Plus a dim blue ambient light: the edge picks that up.
    let ambient = Light { kind: 3, color: [0.0, 0.0, 0.2], ..spot };
    let lit = Scene3d { camera: cam, lights: vec![spot, ambient] };
    let out = draw(&mut c, t, &layer_space(&t, [64.0, 64.0], &lit));
    let edge = px(&out, 200, 128, 78);
    assert!(edge[2] > 100 && edge[0] < 20, "ambient blue: {edge:?}");
    // Unlit layers ignore the lights.
    let off = Transform { lit: false, ..t };
    let out = draw(&mut c, off, &layer_space(&off, [64.0, 64.0], &lit));
    assert!(near(px(&out, 200, 128, 78), [255, 255, 255]));
}

#[test]
fn wipes_dips_and_iris() {
    use reelforge_render::{DrawItem, Wipe};
    let Some(mut c) = setup() else { return };
    let red = solid(1, 1, [255, 0, 0, 255]);
    let blue = solid(1, 1, [0, 0, 255, 255]);
    // Wipe right, half way: the new clip (blue) covers the left half.
    let mut b = full(2, &blue, [32, 16]);
    b.wipe = Some(Wipe::moving([1.0, 0.0], 0.5));
    c.render_items([32, 16], &[DrawItem::Layer(full(1, &red, [32, 16])), DrawItem::Layer(b)]);
    let out = c.read_output().unwrap();
    assert!(near(px(&out, 32, 4, 8), [0, 0, 255]) && near(px(&out, 32, 28, 8), [255, 0, 0]), "{:?} {:?}", px(&out, 32, 4, 8), px(&out, 32, 28, 8));
    // Iris, half way: blue in the middle, red in the corners.
    let mut b = full(2, &blue, [32, 16]);
    b.wipe = Some(Wipe::iris(0.5));
    c.render_items([32, 16], &[DrawItem::Layer(full(1, &red, [32, 16])), DrawItem::Layer(b)]);
    let out = c.read_output().unwrap();
    assert!(near(px(&out, 32, 16, 8), [0, 0, 255]) && near(px(&out, 32, 0, 0), [255, 0, 0]));
    // Dip to white: white half way, and a quarter of the way it's half red, half white.
    let item = |mix| DrawItem::Dissolve { from: full(1, &red, [32, 16]), to: full(2, &blue, [32, 16]), mix, dip: Some([1.0; 3]) };
    c.render_items([32, 16], &[item(0.5)]);
    assert!(near(px(&c.read_output().unwrap(), 32, 16, 8), [255, 255, 255]));
    c.render_items([32, 16], &[item(0.25)]);
    let p = px(&c.read_output().unwrap(), 32, 16, 8);
    let h = srgb(0.5);
    assert!(near(p, [255, h, h]), "{p:?}");
}

#[test]
fn crop_cuts_the_layer_and_anchor_moves_the_pivot() {
    let Some(mut c) = setup() else { return };
    // A 16×16 picture: left half red, right half green.
    let mut img = Vec::new();
    for _y in 0..16 {
        for x in 0..16 {
            img.extend(if x < 8 { [255, 0, 0, 255] } else { [0, 255, 0, 255] });
        }
    }
    // Crop away the left half: only green remains, still in its place.
    let t = Transform { crop: [0.5, 0.0, 0.0, 0.25], ..Default::default() };
    c.render([32, 32], &[layer(1, &img, [16, 16], [16.0, 16.0], t, BlendMode::Normal)]);
    let out = c.read_output().unwrap();
    assert!(near(px(&out, 32, 12, 16), [0, 0, 0]), "the cropped left half is gone");
    assert!(near(px(&out, 32, 20, 10), [0, 255, 0]), "the right half stays where it was");
    assert!(near(px(&out, 32, 20, 22), [0, 0, 0]), "the bottom quarter is cropped");

    // Anchor at the layer's top-left corner: that corner sits on the frame centre.
    let t = Transform { anchor: [-8.0, -8.0], ..Default::default() };
    c.render([32, 32], &[layer(1, &img, [16, 16], [16.0, 16.0], t, BlendMode::Normal)]);
    let out = c.read_output().unwrap();
    assert!(near(px(&out, 32, 18, 18), [255, 0, 0]), "the layer hangs down-right of the centre");
    assert!(near(px(&out, 32, 14, 14), [0, 0, 0]));
    // Rotating 180° about that corner swings the layer up-left.
    let t = Transform { anchor: [-8.0, -8.0], rotation: 180.0, ..Default::default() };
    c.render([32, 32], &[layer(1, &img, [16, 16], [16.0, 16.0], t, BlendMode::Normal)]);
    let out = c.read_output().unwrap();
    assert!(near(px(&out, 32, 13, 13), [255, 0, 0]));
    assert!(near(px(&out, 32, 4, 4), [0, 255, 0]));
    assert!(near(px(&out, 32, 18, 18), [0, 0, 0]));
}

#[test]
fn corner_pin_draws_in_perspective() {
    use reelforge_core::transform::{Homography, Pin};
    let Some(mut c) = setup() else { return };
    // A 64×64 layer: left half red, right half blue.
    let mut img = Vec::with_capacity(64 * 64 * 4);
    for _y in 0..64 {
        for x in 0..64 {
            img.extend(if x < 32 { [255, 0, 0, 255] } else { [0, 0, 255, 255] });
        }
    }
    // Pinned into a quad that recedes strongly on the right.
    let quad = [[20.0, 20.0], [180.0, 60.0], [180.0, 100.0], [20.0, 140.0]];
    let t = Transform { pin: Some(Pin(quad)), ..Default::default() };
    let black = solid(4, 4, [0, 0, 0, 255]);
    c.render([200, 160], &[
        layer(1, &black, [4, 4], [200.0, 160.0], Transform::default(), BlendMode::Normal),
        layer(2, &img, [64, 64], [64.0, 64.0], t, BlendMode::Normal),
    ]);
    let out = c.read_output().unwrap();
    let h = Homography::from_quads(&[[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]], &quad).unwrap();
    // The colour change is where the layer's middle really lands in perspective
    // (well right of the quad's halfway line at x = 100, as the right side recedes).
    let mid = h.apply([0.5, 0.5]);
    assert!(mid[0] > 115.0, "{mid:?}");
    let at = |p: [f32; 2]| px(&out, 200, p[0] as u32, p[1] as u32);
    assert!(near(at([mid[0] - 4.0, mid[1]]), [255, 0, 0]), "just left of the middle: {:?}", at([mid[0] - 4.0, mid[1]]));
    assert!(near(at([mid[0] + 4.0, mid[1]]), [0, 0, 255]), "just right of it: {:?}", at([mid[0] + 4.0, mid[1]]));
    // A plain (affine) split would put the change at x = 100: that's still red here.
    assert!(near(at([104.0, 80.0]), [255, 0, 0]));
    // Inside the corners, and outside the quad.
    assert!(near(at(h.apply([0.05, 0.05])), [255, 0, 0]));
    assert!(near(at(h.apply([0.95, 0.95])), [0, 0, 255]));
    assert!(near(at([190.0, 20.0]), [0, 0, 0]), "outside the pin is the background");
    assert!(near(at([100.0, 30.0]), [0, 0, 0]), "above the receding top edge too");
}

// ---- Plugins ------------------------------------------------------------------

fn plugin(id: &str, manifest_extra: &str, params: &str, source: &str) -> std::sync::Arc<reelforge_core::plugin::PluginDef> {
    let manifest = format!("id = \"{id}\"\nname = \"{id}\"\n{manifest_extra}\n{params}");
    std::sync::Arc::new(reelforge_core::plugin::parse(&manifest, source, std::path::Path::new("/p")).unwrap())
}

fn plugin_fx(def: &std::sync::Arc<reelforge_core::plugin::PluginDef>, values: &[f32], time: f32) -> EffectValues {
    let mut v = [0.0; reelforge_core::effects::MAX_PARAMS];
    v[..values.len()].copy_from_slice(values);
    EffectValues { kind: EffectKind::Plugin, values: v, table: None, plugin: Some(def.clone()), time, prev: None }
}

#[test]
fn plugins_run_with_params_time_and_several_passes() {
    let Some(mut c) = setup() else { return };
    // Left half red, right half blue.
    let mut img = Vec::new();
    for _y in 0..16 {
        for x in 0..32 {
            img.extend(if x < 16 { [255, 0, 0, 255] } else { [0, 0, 255, 255] });
        }
    }
    let lay = |fx: Vec<EffectValues>| with_fx(layer(1, &img, [32, 16], [32.0, 16.0], Transform::default(), BlendMode::Normal), fx);

    // A shift by a number of pixels (param 0), proving param(), pixel() and sampling work.
    let shift = plugin("t.shift", "", "[[param]]\nname = \"Px\"\nmax = 32", "fn effect(uv: vec2<f32>) -> vec4<f32> { return sample(uv - vec2(param(0), 0.0) * pixel()); }");
    c.render([32, 16], &[lay(vec![plugin_fx(&shift, &[8.0], 0.0)])]);
    let out = c.read_output().unwrap();
    assert!(near(px(&out, 32, 20, 8), [255, 0, 0]), "red moved 8 px right: {:?}", px(&out, 32, 20, 8));
    assert!(near(px(&out, 32, 26, 8), [0, 0, 255]));

    // Time: grey whose level is the time (0.5 s -> linear 0.5 -> sRGB 188).
    let t = plugin("t.time", "", "", "fn effect(uv: vec2<f32>) -> vec4<f32> { return vec4(vec3(time()), 1.0); }");
    c.render([32, 16], &[lay(vec![plugin_fx(&t, &[], 0.5)])]);
    assert!(near(px(&c.read_output().unwrap(), 32, 4, 4), [188, 188, 188]));

    // Three passes: blacken, then invert (reads the previous pass), then mix with
    // the original (the red/blue picture). Result = 0.5·white + 0.5·original.
    let multi = plugin(
        "t.multi",
        "passes = [\"black\", \"invert\", \"mix_back\"]",
        "",
        "fn black(uv: vec2<f32>) -> vec4<f32> { return vec4(0.0, 0.0, 0.0, 1.0); }\n\
         fn invert(uv: vec2<f32>) -> vec4<f32> { let c = sample(uv); return vec4(1.0 - c.rgb, c.a); }\n\
         fn mix_back(uv: vec2<f32>) -> vec4<f32> { return mix(sample(uv), original(uv), 0.5); }",
    );
    c.render([32, 16], &[lay(vec![plugin_fx(&multi, &[], 0.0)])]);
    let out = c.read_output().unwrap();
    assert!(near(px(&out, 32, 4, 8), [255, 188, 188]), "{:?}", px(&out, 32, 4, 8));
    assert!(near(px(&out, 32, 28, 8), [188, 188, 255]), "{:?}", px(&out, 32, 28, 8));

    // A plugin after a built-in effect, and alpha: straight-colour output is premultiplied.
    let half = plugin("t.half", "", "", "fn effect(uv: vec2<f32>) -> vec4<f32> { let c = sample(uv); return vec4(c.rgb, 0.5); }");
    let black = solid(4, 4, [0, 0, 0, 255]);
    c.render([32, 16], &[layer(2, &black, [4, 4], [32.0, 16.0], Transform::default(), BlendMode::Normal), lay(vec![fx(EffectKind::ColorCorrect, &[(2, 0.0)]), plugin_fx(&half, &[], 0.0)])]);
    // Red, desaturated to its luma (0.2126 linear), at 50% over black -> 0.1063 -> sRGB 92.
    assert!(near(px(&c.read_output().unwrap(), 32, 4, 8), [92, 92, 92]), "{:?}", px(&c.read_output().unwrap(), 32, 4, 8));

    // A broken plugin is skipped, not a crash; the rest still renders.
    let broken = plugin("t.broken", "", "", "fn effect(uv: vec2<f32>) -> vec4<f32> { return nope; }");
    c.render([32, 16], &[lay(vec![plugin_fx(&broken, &[], 0.0), plugin_fx(&shift, &[8.0], 0.0)])]);
    assert!(near(px(&c.read_output().unwrap(), 32, 20, 8), [255, 0, 0]));
}


/// YUV frames convert on the GPU exactly as the CPU reference does.
#[test]
fn yuv_layers_convert_like_the_cpu_reference() {
    use reelforge_core::frame::{YuvLayout, YuvMatrix, YuvPlanes};
    let Some(mut c) = setup() else { return };
    let (w, h) = (16u32, 8u32);
    let (cw, ch) = (w / 2, h / 2);
    for layout in [YuvLayout::I420, YuvLayout::Nv12, YuvLayout::I420P10, YuvLayout::P010] {
        for (matrix, full_range) in [(YuvMatrix::Bt709, false), (YuvMatrix::Bt601, true), (YuvMatrix::Bt2020, false)] {
            // A gradient in Y and a different one in each chroma channel.
            let bits = layout.bits();
            let k = 1u32 << (bits - 8);
            let word = |v: u32| -> Vec<u8> {
                let v = (v * k) << layout.shift();
                if layout.sample_bytes() == 1 { vec![v as u8] } else { (v as u16).to_le_bytes().to_vec() }
            };
            let y: Vec<u8> = (0..w * h).flat_map(|i| word(20 + (i % w) * 13 + (i / w) * 3)).collect();
            let u: Vec<u32> = (0..cw * ch).map(|i| 40 + (i % cw) * 25).collect();
            let v: Vec<u32> = (0..cw * ch).map(|i| 220 - (i / cw) * 40).collect();
            let planes = if layout.interleaved() {
                vec![y, u.iter().zip(&v).flat_map(|(a, b)| [word(*a), word(*b)].concat()).collect()]
            } else {
                vec![y, u.iter().flat_map(|a| word(*a)).collect(), v.iter().flat_map(|b| word(*b)).collect()]
            };
            let yuv = YuvPlanes { layout, width: w, height: h, planes, matrix, full_range };
            let want = yuv.to_rgba();
            let l = LayerInput {
                key: 9,
                content_id: layout as u64 * 10 + matrix as u64 * 2 + full_range as u64,
                pixels: PixelData::Yuv(&yuv),
                pixel_size: [w, h],
                layer_size: [w as f32, h as f32],
                transform: Transform::default(),
                blend: BlendMode::Normal,
                effects: Vec::new(),
                mask: None,
                wipe: None,
                mesh: None, space: None
            };
            c.render([w, h], &[l]);
            let got = c.read_output().unwrap();
            let worst = got.iter().zip(&want).enumerate().filter(|(i, _)| i % 4 != 3).map(|(_, (a, b))| (*a as i32 - *b as i32).abs()).max().unwrap();
            assert!(worst <= 2, "{layout:?} {matrix:?} full={full_range}: off by {worst}");
        }
    }
}

/// A mesh warp puts each part of the layer where its grid says: a layer whose
/// left half is red and right half blue, over a 2 × 1 grid on the frame's
/// right half, then with the middle column pulled left.
#[test]
fn mesh_warp_places_the_layer_over_its_grid() {
    let Some(mut c) = setup() else { return };
    let (w, h) = (8u32, 8u32);
    let mut img = Vec::new();
    for _y in 0..h {
        for x in 0..w {
            img.extend(if x < w / 2 { [255, 0, 0, 255] } else { [0, 0, 255, 255] });
        }
    }
    let grid = |mid: f32| reelforge_core::meshwarp::MeshShape { cols: 2, rows: 1, points: vec![[32.0, 0.0], [mid, 0.0], [64.0, 0.0], [32.0, 32.0], [mid, 32.0], [64.0, 32.0]] };
    for (mid, red_until) in [(48.0, 48), (40.0, 40)] {
        let mut l = layer(1, &img, [w, h], [8.0, 8.0], Transform::default(), BlendMode::Normal);
        l.mesh = Some(grid(mid));
        c.render([64, 32], &[l]);
        let out = c.read_output().unwrap();
        assert!(near(px(&out, 64, 10, 16), [0, 0, 0]), "left of the grid is empty: {:?}", px(&out, 64, 10, 16));
        assert!(near(px(&out, 64, 34, 16), [255, 0, 0]), "red starts the grid");
        assert!(near(px(&out, 64, red_until - 3, 16), [255, 0, 0]), "red up to the middle column at {mid}");
        assert!(near(px(&out, 64, red_until + 3, 16), [0, 0, 255]), "blue after it at {mid}: {:?}", px(&out, 64, red_until + 3, 16));
        assert!(near(px(&out, 64, 62, 2), [0, 0, 255]), "blue to the right edge");
    }
}

fn adjustment(key: u64, frame: [u32; 2], t: Transform, effects: Vec<EffectValues>) -> LayerInput<'static> {
    LayerInput { key, content_id: 1, pixels: PixelData::Below, pixel_size: frame, layer_size: [frame[0] as f32, frame[1] as f32], transform: t, blend: BlendMode::Normal, effects, mask: None, wipe: None, mesh: None, space: None }
}

#[test]
fn adjustment_layers_change_everything_below_and_nothing_above() {
    let Some(mut c) = setup() else { return };
    let white = solid(1, 1, [255, 255, 255, 255]);
    let blue = solid(1, 1, [0, 0, 255, 255]);
    let down = || vec![fx(EffectKind::ColorCorrect, &[(0, -1.0)])];
    // White below at −1 stop; a blue square above is left alone.
    let top = layer(3, &blue, [1, 1], [4.0, 4.0], Transform { position: [4.0, 0.0], ..Default::default() }, BlendMode::Normal);
    c.render([16, 16], &[full(1, &white, [16, 16]), adjustment(2, [16, 16], Transform::default(), down()), top]);
    let out = c.read_output().unwrap();
    let half = srgb(0.5);
    assert!(near(px(&out, 16, 2, 2), [half, half, half]), "{:?}", px(&out, 16, 2, 2));
    assert!(near(px(&out, 16, 12, 8), [0, 0, 255]), "{:?}", px(&out, 16, 12, 8));
    // Half opacity mixes the adjusted frame with the original.
    let t = Transform { opacity: 0.5, ..Default::default() };
    c.render([16, 16], &[full(1, &white, [16, 16]), adjustment(2, [16, 16], t, down())]);
    let q = srgb(0.75);
    assert!(near(px(&c.read_output().unwrap(), 16, 8, 8), [q, q, q]));
    // Cropped: only the uncropped part is adjusted.
    let t = Transform { crop: [0.0, 0.0, 0.5, 0.0], ..Default::default() };
    c.render([16, 16], &[full(1, &white, [16, 16]), adjustment(2, [16, 16], t, down())]);
    let out = c.read_output().unwrap();
    assert!(near(px(&out, 16, 3, 8), [half, half, half]), "{:?}", px(&out, 16, 3, 8));
    assert!(near(px(&out, 16, 12, 8), [255, 255, 255]), "{:?}", px(&out, 16, 12, 8));
    // Two in a row stack; with no effects one does nothing.
    c.render([16, 16], &[full(1, &white, [16, 16]), adjustment(2, [16, 16], Transform::default(), down()), adjustment(4, [16, 16], Transform::default(), down()), adjustment(5, [16, 16], Transform::default(), vec![])]);
    let q = srgb(0.25);
    assert!(near(px(&c.read_output().unwrap(), 16, 8, 8), [q, q, q]));
}
