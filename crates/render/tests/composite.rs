//! Pixel-exact compositor tests on a headless GPU (skipped if no adapter is available).

use reelforge_core::{BlendMode, Transform};
use reelforge_render::{headless_device, Compositor, LayerInput, PixelData};

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

/// A glow or a drop shadow reaches past the layer's own edges: an opaque
/// layer smaller than the frame (a full-frame clip shrunk, a logo, a
/// picture-in-picture) casts its shadow and glows onto what's around it.
#[test]
fn glow_and_shadow_spill_past_the_layer() {
    let Some(mut c) = setup() else { return };
    let white = solid(16, 16, [255, 255, 255, 255]);
    let red = solid(1, 1, [255, 0, 0, 255]);
    // A 16×16 white layer in the middle of a 64×64 red frame: x and y 24–40.
    let small = || layer(2, &white, [16, 16], [16.0, 16.0], Transform::default(), BlendMode::Normal);
    // A shadow 6 px down-right (angle 135°), hard-edged, fully dark.
    c.render([64, 64], &[full(9, &red, [64, 64]), with_fx(small(), vec![fx(EffectKind::DropShadow, &[(0, 8.5), (1, 135.0), (2, 0.0), (3, 100.0)])])]);
    let out = c.read_output().unwrap();
    assert!(near(px(&out, 64, 32, 32), [255, 255, 255]), "the layer");
    assert!(px(&out, 64, 43, 43)[0] < 40, "its shadow, outside it: {:?}", px(&out, 64, 43, 43));
    assert!(near(px(&out, 64, 20, 20), [255, 0, 0]), "none up-left");
    assert!(near(px(&out, 64, 55, 55), [255, 0, 0]), "nor far away");
    // Cropped on the right: the shadow isn't drawn past the cut, only where the layer's own edge is.
    let crop = Transform { crop: [0.0, 0.0, 0.25, 0.0], ..Default::default() };
    let mut l = small();
    l.transform = crop;
    l.content_id = 2;
    c.render([64, 64], &[full(9, &red, [64, 64]), with_fx(l, vec![fx(EffectKind::DropShadow, &[(0, 8.5), (1, 135.0), (2, 0.0), (3, 100.0)])])]);
    let out = c.read_output().unwrap();
    assert!(px(&out, 64, 30, 43)[0] < 40, "below the layer: {:?}", px(&out, 64, 30, 43));
    assert!(near(px(&out, 64, 39, 33), [255, 0, 0]), "the cut-off right side shows the frame: {:?}", px(&out, 64, 39, 33));
    // A glow lights the frame round the layer.
    let black = solid(1, 1, [0, 0, 0, 255]);
    c.render([64, 64], &[full(10, &black, [64, 64]), with_fx(small(), vec![fx(EffectKind::Glow, &[(0, 50.0), (1, 6.0), (2, 100.0)])])]);
    let out = c.read_output().unwrap();
    assert!(px(&out, 64, 20, 32)[0] > 30, "glow left of the layer: {:?}", px(&out, 64, 20, 32));
    assert!(px(&out, 64, 32, 44)[0] > 30, "and below it: {:?}", px(&out, 64, 32, 44));
    assert!(px(&out, 64, 2, 2)[0] < 10, "not in the far corner: {:?}", px(&out, 64, 2, 2));
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
    let planes = YuvPlanes { layout: YuvLayout::Nv12, width: w, height: h, planes: vec![y, uv], matrix: YuvMatrix::Bt709, full_range: false, transfer: Default::default() };
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
    let spot = Light { kind: 1, color: [1.0, 0.0, 0.0], position: [0.0, 0.0, -200.0], direction: [0.0, 0.0, 1.0], cone: [(8f32).to_radians().cos(), (4f32).to_radians().cos()], falloff: 0, radius: 1000.0, shadow: 0.0, softness: 0.0, env: None };
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
    EffectValues { kind: EffectKind::Plugin, values: v, table: None, plugin: Some(def.clone()), time, prev: None, particles: None, pieces: None, fluid: None, lightning: [0.0; 3] }
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


/// A YUV frame of `layout`, `w`×`h`: gradients in Y, U, V and (with alpha) A.
fn yuv_frame(layout: reelforge_core::frame::YuvLayout, w: u32, h: u32, matrix: reelforge_core::frame::YuvMatrix, full_range: bool) -> reelforge_core::frame::YuvPlanes {
    let (cw, ch) = layout.chroma_size(w, h);
    let k = 1u32 << (layout.bits() - 8);
    let word = |v: u32| -> Vec<u8> {
        let v = (v * k) << layout.shift();
        if layout.sample_bytes() == 1 { vec![v as u8] } else { (v as u16).to_le_bytes().to_vec() }
    };
    let y: Vec<u8> = (0..w * h).flat_map(|i| word(20 + (i % w) * 13 + (i / w) * 3)).collect();
    let u: Vec<u32> = (0..cw * ch).map(|i| 40 + (i % cw) * 180 / cw.max(1)).collect();
    let v: Vec<u32> = (0..cw * ch).map(|i| 220 - (i / cw) * 160 / ch.max(1)).collect();
    let mut planes = if layout.interleaved() {
        vec![y, u.iter().zip(&v).flat_map(|(a, b)| [word(*a), word(*b)].concat()).collect()]
    } else {
        vec![y, u.iter().flat_map(|a| word(*a)).collect(), v.iter().flat_map(|b| word(*b)).collect()]
    };
    if layout.alpha() {
        // Opaque on the left, see-through on the right.
        planes.push((0..w * h).flat_map(|i| word(if i % w < w / 2 { 255 } else { 64 })).collect());
    }
    reelforge_core::frame::YuvPlanes { layout, width: w, height: h, planes, matrix, full_range, transfer: Default::default() }
}

fn yuv_layer<'a>(yuv: &'a reelforge_core::frame::YuvPlanes, content_id: u64) -> LayerInput<'a> {
    LayerInput {
        key: 9,
        content_id,
        pixels: PixelData::Yuv(yuv),
        pixel_size: [yuv.width, yuv.height],
        layer_size: [yuv.width as f32, yuv.height as f32],
        transform: Transform::default(),
        blend: BlendMode::Normal,
        effects: Vec::new(),
        mask: None,
        wipe: None,
        mesh: None,
        space: None,
    }
}

/// YUV frames of every layout (4:2:0, 4:2:2, 4:4:4; 8 to 16 bits; alpha)
/// convert on the GPU as the CPU reference does.
#[test]
fn yuv_layers_convert_like_the_cpu_reference() {
    use reelforge_core::frame::{Chroma, YuvLayout, YuvMatrix};
    let Some(mut c) = setup() else { return };
    let (w, h) = (16u32, 8u32);
    let mut layouts = vec![YuvLayout::Nv12, YuvLayout::P010];
    for bits in [8u8, 10, 12, 16] {
        for chroma in [Chroma::C420, Chroma::C422, Chroma::C444] {
            for alpha in [false, true] {
                layouts.push(YuvLayout::Planar { bits, chroma, alpha });
            }
        }
    }
    let mut id = 0;
    for layout in layouts {
        for (matrix, full_range) in [(YuvMatrix::Bt709, false), (YuvMatrix::Bt601, true), (YuvMatrix::Bt2020, false)] {
            let yuv = yuv_frame(layout, w, h, matrix, full_range);
            id += 1;
            // Over black, so see-through parts show as darker (as the reference, premultiplied).
            c.render([w, h], &[yuv_layer(&yuv, id)]);
            let got = c.read_output().unwrap();
            let want: Vec<u8> = yuv
                .to_rgba()
                .chunks_exact(4)
                .flat_map(|p| {
                    if p[3] == 255 {
                        return [p[0], p[1], p[2], 255];
                    }
                    let a = p[3] as f32 / 255.0;
                    let lin = |v: u8| {
                        let c = v as f32 / 255.0;
                        if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
                    };
                    let enc = |l: f32| ((if l <= 0.003_130_8 { l * 12.92 } else { 1.055 * l.powf(1.0 / 2.4) - 0.055 }) * 255.0).round() as u8;
                    [enc(lin(p[0]) * a), enc(lin(p[1]) * a), enc(lin(p[2]) * a), 255]
                })
                .collect();
            // Deep video keeps values above white, which the 8-bit reference
            // clips: where it clipped and alpha shows them, they differ on purpose.
            let src = yuv.to_rgba();
            let clipped = |i: usize| layout.deep() && src[i] == 255 && src[i / 4 * 4 + 3] < 255;
            let worst = got.iter().zip(&want).enumerate().filter(|(i, _)| i % 4 != 3 && !clipped(*i)).map(|(_, (a, b))| (*a as i32 - *b as i32).abs()).max().unwrap();
            assert!(worst <= 2, "{layout:?} {matrix:?} full={full_range}: off by {worst}");
        }
    }
}

/// 10- and 12-bit video keeps its precision: a gentle ramp makes more
/// distinct levels than 8 bits could hold, and gets no banding steps.
#[test]
fn deep_video_is_not_cut_to_eight_bits() {
    use reelforge_core::frame::{f16_to_f32, Chroma, YuvLayout, YuvMatrix, YuvPlanes};
    let Some(mut c) = setup() else { return };
    let w = 1024u32;
    for (bits, chroma) in [(10u8, Chroma::C420), (10, Chroma::C422), (12, Chroma::C444)] {
        let layout = YuvLayout::Planar { bits, chroma, alpha: false };
        let (cw, ch) = layout.chroma_size(w, 2);
        let mid = 1u16 << (bits - 1);
        // Y climbs one 10-bit code value every two pixels from black: 512
        // levels, where 8 bits would hold 128.
        let lo = 64u16 << (bits - 10);
        let y: Vec<u8> = (0..w * 2).flat_map(|i| (lo + ((i % w) as u16 * (1 << (bits - 10))) / 2).to_le_bytes()).collect();
        let cplane: Vec<u8> = (0..cw * ch).flat_map(|_| mid.to_le_bytes()).collect();
        let yuv = YuvPlanes { layout, width: w, height: 2, planes: vec![y, cplane.clone(), cplane], matrix: YuvMatrix::Bt709, full_range: false, transfer: Default::default() };
        c.render([w, 2], &[yuv_layer(&yuv, bits as u64 * 100 + chroma as u64)]);
        let f = c.read_output_linear().unwrap();
        let row: Vec<f32> = (0..w as usize).map(|x| f16_to_f32(f.rgba[x * 4 + 1])).collect();
        let mut levels: Vec<u16> = f.rgba[..w as usize * 4].chunks_exact(4).map(|p| p[1]).collect();
        levels.dedup();
        assert!(levels.len() > 400, "{bits}-bit {chroma:?}: only {} levels", levels.len());
        // Monotonic, and as smooth as the source (no jump bigger than 2 code values).
        let enc = |l: f32| if l <= 0.003_130_8 { l * 12.92 } else { 1.055 * l.powf(1.0 / 2.4) - 0.055 };
        let step = 1.0 / (219.0 * 4.0);
        for x in 1..w as usize {
            assert!(row[x] >= row[x - 1], "{bits}-bit: falls at {x}");
            assert!(enc(row[x]) - enc(row[x - 1]) < 2.5 * step, "{bits}-bit: a step at {x}");
        }
    }
    // 8-bit video still goes the 8-bit way (and super-whites clip there).
    assert!(!PixelData::Yuv(&yuv_frame(YuvLayout::I420, 4, 4, YuvMatrix::Bt709, false)).is_deep());
    assert!(PixelData::Yuv(&yuv_frame(YuvLayout::P010, 4, 4, YuvMatrix::Bt709, false)).is_deep());
}

/// Ten-bit super-whites (above code 940) survive as values above 1, for
/// grading to bring back; 8-bit sources clip at white as before.
#[test]
fn deep_video_keeps_super_whites() {
    use reelforge_core::frame::{f16_to_f32, Chroma, YuvLayout, YuvMatrix, YuvPlanes};
    let Some(mut c) = setup() else { return };
    let layout = YuvLayout::Planar { bits: 10, chroma: Chroma::C444, alpha: false };
    let plane = |v: u16| -> Vec<u8> { (0..16).flat_map(|_| v.to_le_bytes()).collect() };
    let yuv = YuvPlanes { layout, width: 4, height: 4, planes: vec![plane(1019), plane(512), plane(512)], matrix: YuvMatrix::Bt709, full_range: false, transfer: Default::default() };
    c.render([4, 4], &[yuv_layer(&yuv, 77)]);
    let f = c.read_output_linear().unwrap();
    let g = f16_to_f32(f.rgba[1]);
    // (1019 − 64) / 876 = 1.090 encoded → about 1.22 linear.
    assert!(g > 1.15 && g < 1.3, "{g}");
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

#[test]
fn particles_fly_fall_and_add_light() {
    use reelforge_core::particles as p;
    let Some(mut c) = setup() else { return };
    let black = solid(200, 100, [0, 0, 0, 255]);
    let draw = |c: &mut reelforge_render::Compositor, set: &[(usize, f32)], at: f64| {
        let mut e = Effect::new(EffectKind::Particles);
        for &(i, v) in set {
            e.params[i].value = v;
        }
        let v = e.eval(reelforge_core::Time::from_secs_f64(at));
        c.render([200, 100], &[with_fx(layer(1, &black, [200, 100], [200.0, 100.0], Transform::default(), BlendMode::Normal), vec![v])]);
        c.read_output().unwrap()
    };
    let bright = |img: &[u8], x0: u32, x1: u32, y0: u32, y1: u32| -> u32 { (y0..y1).flat_map(|y| (x0..x1).map(move |x| (x, y))).map(|(x, y)| px(img, 200, x, y)[..3].iter().map(|&v| v as u32).sum::<u32>()).sum() };
    // Thrown straight right from the left middle, no gravity: light right of the emitter, none left of it.
    let right = [(p::EMIT_X, 10.0), (p::EMIT_Y, 50.0), (p::DIRECTION, 0.0), (p::SPREAD, 0.0), (p::SPEED, 100.0), (p::GRAVITY, 0.0), (p::RATE, 200.0), (p::LIFETIME, 1.5), (p::RANDOMNESS, 0.0), (p::SIZE_START, 6.0), (p::SIZE_END, 6.0)];
    let img = draw(&mut c, &right, 2.0);
    assert!(bright(&img, 30, 160, 44, 56) > 30_000, "{}", bright(&img, 30, 160, 44, 56));
    assert_eq!(bright(&img, 0, 15, 0, 100), 0, "nothing behind the emitter");
    assert!(bright(&img, 30, 160, 0, 30) < 500, "nothing far above the line");
    // Particles 1.5 s old at 100 px/s die before x = 20 + 150 = 170.
    assert!(bright(&img, 180, 200, 0, 100) < 200);
    // The colour goes from birth (yellow) to death (orange-red): near the emitter more green.
    let near_g = px(&img, 200, 40, 50);
    let far_g = px(&img, 200, 150, 50);
    assert!(near_g[0] > 100 && near_g[1] > far_g[1], "{near_g:?} {far_g:?}");

    // Gravity bends them down: below the line on the right.
    let mut fall = right.to_vec();
    fall.push((p::GRAVITY, 40.0));
    let img = draw(&mut c, &fall, 2.0);
    assert!(bright(&img, 120, 175, 65, 100) > bright(&img, 120, 175, 40, 60) * 3, "falls");

    // Opacity 0 draws nothing; before anything is born, nothing either.
    let mut none = right.to_vec();
    none.push((p::OPACITY, 0.0));
    assert_eq!(bright(&draw(&mut c, &none, 2.0), 0, 200, 0, 100), 0);
    assert_eq!(bright(&draw(&mut c, &right, 0.0), 0, 200, 0, 100), 0);

    // Every look draws something; sparks streak along the motion (wider than tall).
    for look in 0..p::LOOKS.len() {
        let mut v = right.to_vec();
        v.extend([(p::LOOK, look as f32), (p::RATE, 3.0), (p::SPEED, 400.0)]);
        let img = draw(&mut c, &v, 0.3);
        assert!(bright(&img, 0, 200, 0, 100) > 300, "look {look}");
        if look == p::LOOK_SPARKS as usize {
            let lit: Vec<(u32, u32)> = (0..100).flat_map(|y| (0..200).map(move |x| (x, y))).filter(|&(x, y)| px(&img, 200, x, y)[0] > 40).collect();
            let w = lit.iter().map(|p| p.0).max().unwrap() - lit.iter().map(|p| p.0).min().unwrap();
            let h = lit.iter().map(|p| p.1).max().unwrap() - lit.iter().map(|p| p.1).min().unwrap();
            assert!(w > h * 2, "streaks {w}×{h}");
        }
    }
}

#[test]
fn every_blend_mode_matches_the_reference_formula() {
    let Some(mut c) = setup() else { return };
    let enc = |v: f32| if v <= 0.0031308 { v * 12.92 } else { 1.055 * v.powf(1.0 / 2.4) - 0.055 };
    let dec = |v: f32| if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) };
    let tops: [[u8; 3]; 3] = [[200, 60, 120], [30, 200, 90], [128, 128, 128]];
    let unders: [[u8; 3]; 2] = [[51, 128, 204], [230, 180, 20]];
    for under in unders {
        let bg = solid(1, 1, [under[0], under[1], under[2], 255]);
        let mut back = full(1, &bg, [16, 16]);
        back.content_id = under[0] as u64 + 100;
        for top in tops {
            for (alpha, through_fx) in [(255u8, false), (128, false), (255, true)] {
                let fg = solid(1, 1, [top[0], top[1], top[2], alpha]);
                // (Add, Multiply and Screen use the GPU's blender in linear light, as before.)
                for mode in BlendMode::ALL.into_iter().filter(|m| m.reads_backdrop() || *m == BlendMode::Normal) {
                    let mut l = layer(2, &fg, [1, 1], [16.0, 16.0], Transform::default(), mode);
                    // A new picture each time (the compositor keeps textures by content id).
                    l.content_id = u64::from_le_bytes([top[0], top[1], top[2], alpha, under[0], 7, 7, 7]);
                    if through_fx {
                        // An effect that changes nothing visible, so the layer is drawn premultiplied.
                        l = with_fx(l, vec![fx(EffectKind::ColorCorrect, &[(0, 0.0001)])]);
                    }
                    c.render([16, 16], &[back.clone(), l]);
                    let got = px(&c.read_output().unwrap(), 16, 8, 8);
                    let f = |v: [u8; 3]| v.map(|x| x as f32 / 255.0);
                    let b = mode.blend_rgb(f(under), f(top));
                    let a = alpha as f32 / 255.0;
                    // Opacity mixes in linear light.
                    let want: [u8; 3] = [0, 1, 2].map(|i| (enc(a * dec(b[i].clamp(0.0, 1.0)) + (1.0 - a) * dec(f(under)[i])) * 255.0).round() as u8);
                    // Dodge and burn divide by the top colour, magnifying 8-bit rounding.
                    let tol = if matches!(mode, BlendMode::ColorDodge | BlendMode::ColorBurn) { 6 } else { 3 };
                    assert!((0..3).all(|i| (got[i] as i32 - want[i] as i32).abs() <= tol), "{mode:?} {under:?} under {top:?} α{alpha} fx {through_fx}: got {got:?}, want {want:?}");
                }
            }
        }
    }
}

#[test]
fn float_layers_keep_linear_light_highlights_and_premultiplied_glow() {
    use reelforge_core::frame::{f32_to_f16, FloatPixels};
    let Some(mut c) = setup() else { return };
    let float = |w: u32, h: u32, f: &dyn Fn(u32, u32) -> [f32; 4]| FloatPixels {
        width: w,
        height: h,
        rgba: (0..h).flat_map(|y| (0..w).flat_map(move |x| f(x, y))).map(f32_to_f16).collect(),
    };
    let draw = |c: &mut reelforge_render::Compositor, under: &[u8], f: &FloatPixels, effects: Vec<EffectValues>| {
        let mut back = full(1, under, [16, 16]);
        back.content_id = 1000 + under[0] as u64;
        let l = LayerInput { key: 2, content_id: f.rgba.iter().map(|&v| v as u64).sum::<u64>() + effects.len() as u64, pixels: PixelData::Float(f), pixel_size: [f.width, f.height], layer_size: [16.0, 16.0], transform: Transform::default(), blend: BlendMode::Normal, effects, mask: None, wipe: None, mesh: None, space: None };
        c.render([16, 16], &[back, l]);
        c.read_output().unwrap()
    };
    let black = solid(1, 1, [0, 0, 0, 255]);
    let grey = solid(1, 1, [128, 128, 128, 255]);
    // Linear 0.2 is sRGB 124 (no 8-bit gamma step in between).
    let img = draw(&mut c, &black, &float(16, 16, &|_, _| [0.2, 0.2, 0.2, 1.0]), vec![]);
    assert!(near(px(&img, 16, 8, 8), [124, 124, 124]), "{:?}", px(&img, 16, 8, 8));
    // Premultiplied: half-covered 0.2 (stored as 0.1, alpha 0.5) over black is 0.1.
    let img = draw(&mut c, &black, &float(16, 16, &|_, _| [0.1, 0.1, 0.1, 0.5]), vec![]);
    assert!(near(px(&img, 16, 8, 8), [89, 89, 89]), "{:?}", px(&img, 16, 8, 8));
    // Light with no alpha (a fire element) adds to what's under it.
    let img = draw(&mut c, &grey, &float(16, 16, &|_, _| [0.3, 0.0, 0.0, 0.0]), vec![]);
    let p = px(&img, 16, 8, 8);
    assert!(p[0] > 180 && (p[1] as i32 - 128).abs() <= 2, "{p:?}");
    // A highlight far above 1 keeps its energy through a blur: a single
    // 200.0 pixel spread over a 5×5 area is still brighter than white.
    let spot = float(16, 16, &|x, y| if (x, y) == (8, 8) { [200.0, 200.0, 200.0, 1.0] } else { [0.0, 0.0, 0.0, 1.0] });
    let img = draw(&mut c, &black, &spot, vec![fx(EffectKind::Blur, &[(0, 2.0)])]);
    let ring = px(&img, 16, 10, 8);
    assert!(ring[0] > 240, "the blurred highlight stays bright: {ring:?}");
    // The same spot clipped to 1 first (as an 8-bit picture would be) fades away.
    let clipped = float(16, 16, &|x, y| if (x, y) == (8, 8) { [1.0, 1.0, 1.0, 1.0] } else { [0.0, 0.0, 0.0, 1.0] });
    let img = draw(&mut c, &black, &clipped, vec![fx(EffectKind::Blur, &[(0, 2.0)])]);
    assert!(px(&img, 16, 10, 8)[0] < ring[0] - 60, "{:?}", px(&img, 16, 10, 8));
}

#[test]
fn stylise_effects_do_what_they_say() {
    use reelforge_core::effects::{aberration as ab, flare as fl, glitch as gl, grain as gr, leaks as lk, shake as sh};
    let Some(mut c) = setup() else { return };
    let (w, h) = (160u32, 90u32);
    let black = solid(w, h, [0, 0, 0, 255]);
    let grey = solid(w, h, [128, 128, 128, 255]);
    // A picture with detail everywhere (for the effects that move things).
    let pattern: Vec<u8> = (0..h).flat_map(|y| (0..w).flat_map(move |x| [(x * 255 / w) as u8, (y * 255 / h) as u8, (((x / 8) + (y / 8)) % 2 * 200) as u8, 255])).collect();
    let mut key = 100u64;
    let mut draw = |c: &mut reelforge_render::Compositor, px: &[u8], kind: EffectKind, set: &[(usize, f32)], t: f64| {
        let mut v = fx(kind, set);
        v.time = t as f32;
        key += 1;
        let mut l = layer(1, px, [w, h], [w as f32, h as f32], Transform::default(), BlendMode::Normal);
        l.content_id = px.as_ptr() as u64 ^ px.len() as u64 ^ key;
        c.render([w, h], &[with_fx(l, vec![v])]);
        c.read_output().unwrap()
    };
    let lum = |p: [u8; 4]| p[0] as u32 + p[1] as u32 + p[2] as u32;
    let diff = |a: &[u8], b: &[u8]| a.iter().zip(b).filter(|(x, y)| x.abs_diff(**y) > 6).count();

    // Lens flare: brightest at the light, ghosts across the middle, a long streak when anamorphic.
    let img = draw(&mut c, &black, EffectKind::LensFlare, &[(fl::X, 25.0), (fl::Y, 30.0)], 0.0);
    assert!(lum(px(&img, w, 40, 27)) > 600, "the light: {:?}", px(&img, w, 40, 27));
    assert!(lum(px(&img, w, 40, 27)) > lum(px(&img, w, 120, 63)), "falls off");
    // The biggest ghost: on the far side of the middle, 0.55 of the way back.
    let (gx, gy) = (80.0 + (80.0 - 40.0) * 0.55, 45.0 + (45.0 - 27.0) * 0.55);
    let ghost = lum(px(&img, w, gx as u32, gy as u32));
    let off_axis = lum(px(&img, w, gx as u32, (90.0 - gy) as u32));
    assert!(ghost > off_axis + 10, "ghosts lie on the line through the middle: {ghost} vs {off_axis}");
    let ana = draw(&mut c, &black, EffectKind::LensFlare, &[(fl::X, 25.0), (fl::Y, 30.0), (fl::STYLE, 1.0), (fl::STREAK, 150.0)], 0.0);
    let along = px(&ana, w, 150, 27);
    assert!(along[2] > 20 && along[2] > along[0], "a blue streak along the row: {along:?}");
    // (Below, but between the starburst rays.)
    assert!(lum(px(&ana, w, 60, 85)) < lum(along) / 2, "not up and down: {:?} vs {along:?}", px(&ana, w, 60, 85));

    // Camera shake: moves with time, and differently at different times.
    let a = draw(&mut c, &pattern, EffectKind::CameraShake, &[(sh::AMOUNT, 10.0)], 0.3);
    let b = draw(&mut c, &pattern, EffectKind::CameraShake, &[(sh::AMOUNT, 10.0)], 0.8);
    assert!(diff(&a, &b) > 2000, "shakes over time");
    assert_eq!(a, draw(&mut c, &pattern, EffectKind::CameraShake, &[(sh::AMOUNT, 10.0)], 0.3), "the same moment looks the same");
    // Zoomed out with transparent edges: the corners show nothing (black background).
    let z = draw(&mut c, &pattern, EffectKind::CameraShake, &[(sh::AMOUNT, 0.0), (sh::ROTATION, 0.0), (sh::ZOOM, 100.0), (sh::EDGES, 2.0)], 0.3);
    assert!(diff(&z, &draw(&mut c, &pattern, EffectKind::Blur, &[(0, 0.0)], 0.0)) < 50, "no shake, no zoom: unchanged");
    let big = draw(&mut c, &pattern, EffectKind::CameraShake, &[(sh::AMOUNT, 0.0), (sh::ROTATION, 0.0), (sh::ZOOM, 200.0)], 0.3);
    assert!(lum(px(&big, w, 2, 2)) > 60, "zoomed in, the corner shows the middle: {:?}", px(&big, w, 2, 2));

    // Chromatic aberration: a white square gets colour fringes, its middle stays white.
    let square: Vec<u8> = (0..h).flat_map(|y| (0..w).flat_map(move |x| if (50..110).contains(&x) && (25..65).contains(&y) { [255u8; 4] } else { [0, 0, 0, 255] })).collect();
    let img = draw(&mut c, &square, EffectKind::ChromaticAberration, &[(ab::AMOUNT, 6.0), (ab::FALLOFF, 0.0)], 0.0);
    assert!(lum(px(&img, w, 80, 45)) > 750, "middle: {:?}", px(&img, w, 80, 45));
    let fringe = (40..120).map(|x| px(&img, w, x, 45)).filter(|p| p[0].abs_diff(p[2]) > 100).count();
    assert!(fringe >= 4, "fringes: {fringe}");

    // Film grain: noise on flat grey that changes every frame, not within one.
    let g0 = draw(&mut c, &grey, EffectKind::FilmGrain, &[(gr::AMOUNT, 60.0)], 0.0);
    let spread = g0.chunks_exact(4).map(|p| p[1] as i32).max().unwrap() - g0.chunks_exact(4).map(|p| p[1] as i32).min().unwrap();
    assert!(spread > 15, "grainy: {spread}");
    assert_eq!(g0, draw(&mut c, &grey, EffectKind::FilmGrain, &[(gr::AMOUNT, 60.0)], 0.01), "the same film frame");
    assert!(diff(&g0, &draw(&mut c, &grey, EffectKind::FilmGrain, &[(gr::AMOUNT, 60.0)], 1.0 / 24.0 + 0.001)) > 2000, "the next frame");
    let mono = draw(&mut c, &grey, EffectKind::FilmGrain, &[(gr::AMOUNT, 60.0), (gr::COLOR, 0.0)], 0.0);
    assert!(mono.chunks_exact(4).all(|p| p[0].abs_diff(p[1]) <= 1 && p[1].abs_diff(p[2]) <= 1), "mono grain stays grey");

    // Glitch: changes the picture, the same at the same moment, different a tick later.
    let gl0 = draw(&mut c, &pattern, EffectKind::Glitch, &[(gl::AMOUNT, 100.0)], 0.05);
    let plain = draw(&mut c, &pattern, EffectKind::Blur, &[(0, 0.0)], 0.0);
    assert!(diff(&gl0, &plain) > 1000);
    assert_eq!(gl0, draw(&mut c, &pattern, EffectKind::Glitch, &[(gl::AMOUNT, 100.0)], 0.06));
    assert!(diff(&gl0, &draw(&mut c, &pattern, EffectKind::Glitch, &[(gl::AMOUNT, 100.0)], 0.25)) > 500);

    // Light leaks from the left: warm light on the left, little on the right.
    let lk0 = draw(&mut c, &black, EffectKind::LightLeaks, &[(lk::FROM, 0.0), (lk::SIZE, 60.0)], 0.0);
    let left: u32 = (0..h).map(|y| lum(px(&lk0, w, 4, y))).sum();
    let right: u32 = (0..h).map(|y| lum(px(&lk0, w, w - 5, y))).sum();
    assert!(left > right * 3 && left > 3000, "left {left}, right {right}");
    let warm = (0..h).map(|y| px(&lk0, w, 4, y)).max_by_key(|p| lum(*p)).unwrap();
    assert!(warm[0] > warm[2] + 40, "warm: {warm:?}");
}

/// A 32×32 green screen with a skin-toned 12×12 square in the middle (10–21),
/// keyed over black: the matte view, clipping, choke and edge softness.
#[test]
fn key_matte_view_choke_and_softness() {
    use reelforge_core::effects::key;
    let Some(mut c) = setup() else { return };
    let black = solid(1, 1, [0, 0, 0, 255]);
    let mut img = Vec::new();
    for y in 0..32 {
        for x in 0..32 {
            img.extend(if (10..22).contains(&x) && (10..22).contains(&y) { [205, 150, 125, 255] } else { [50, 180, 80, 255] });
        }
    }
    let mut n = 10;
    let mut shot = |set: &[(usize, f32)]| {
        n += 1;
        let fg = layer(n, &img, [32, 32], [32.0, 32.0], Transform::default(), BlendMode::Normal);
        c.render([32, 32], &[full(1, &black, [32, 32]), with_fx(fg, vec![fx(EffectKind::ChromaKey, set)])]);
        c.read_output().unwrap()
    };
    let row = |img: &[u8], y: u32| -> Vec<u8> { (0..32).map(|x| px(img, 32, x, y)[1]).collect() };
    // The matte: white where kept, black where keyed out (opaque, over the black below).
    let m = shot(&[(key::VIEW, key::VIEW_MATTE)]);
    assert!(near(px(&m, 32, 16, 16), [255, 255, 255]) && near(px(&m, 32, 3, 16), [0, 0, 0]), "{:?}", row(&m, 16));
    // The result: the square, the screen gone.
    let r = shot(&[]);
    assert!(near(px(&r, 32, 16, 16), [205, 150, 125]) && near(px(&r, 32, 3, 16), [0, 0, 0]));
    // Choke 3 px: the square's matte shrinks by 3 px each side; −3 grows it.
    let m = shot(&[(key::VIEW, key::VIEW_MATTE), (key::CHOKE, 3.0)]);
    let r16 = row(&m, 16);
    assert!(r16[12] < 10 && r16[13] > 245 && r16[18] > 245 && r16[19] < 10, "choke: {r16:?}");
    assert!(px(&m, 32, 16, 12)[0] < 10 && px(&m, 32, 16, 13)[0] > 245, "both ways");
    let m = shot(&[(key::VIEW, key::VIEW_MATTE), (key::CHOKE, -3.0)]);
    let r16 = row(&m, 16);
    assert!(r16[6] < 10 && r16[7] > 245 && r16[24] > 245 && r16[25] < 10, "grow: {r16:?}");
    // …and grown, the extra ring takes the square's colour, not the screen's.
    let r = shot(&[(key::CHOKE, -2.0)]);
    assert!(near(px(&r, 32, 8, 16), [205, 150, 125]), "{:?}", px(&r, 32, 8, 16));
    // Edge softness: the edge ramps over several pixels; the middle stays solid.
    let m = shot(&[(key::VIEW, key::VIEW_MATTE), (key::EDGE_SOFTNESS, 4.0)]);
    let r16 = row(&m, 16);
    let between = r16[7..13].iter().filter(|&&v| v > 20 && v < 235).count();
    assert!(between >= 3 && r16[16] > 245 && r16[2] < 10, "soft: {r16:?}");
    // Clip white below how much of the screen is left lifts a half-keyed screen to solid.
    let half = shot(&[(key::VIEW, key::VIEW_MATTE), (1, 0.0), (2, 100.0)]);
    let clipped = shot(&[(key::VIEW, key::VIEW_MATTE), (1, 0.0), (2, 100.0), (key::CLIP_WHITE, 20.0)]);
    let (h, k) = (px(&half, 32, 3, 3)[0], px(&clipped, 32, 3, 3)[0]);
    assert!(h > 5 && h < 250 && k > h, "clip white: {h} → {k}");
    let gone = shot(&[(key::VIEW, key::VIEW_MATTE), (1, 0.0), (2, 100.0), (key::CLIP_BLACK, 98.0)]);
    assert!(px(&gone, 32, 3, 3)[0] < h, "clip black");
}

/// Luma key: a dark background goes, the bright subject stays; "light parts"
/// does the reverse. The shared matte settings (choke) work for it too.
#[test]
fn luma_key_removes_dark_or_light() {
    use reelforge_core::effects::key;
    let Some(mut c) = setup() else { return };
    let red = solid(1, 1, [255, 0, 0, 255]);
    // Near-black around a light grey 12×12 square at 10–21.
    let mut img = Vec::new();
    for y in 0..32 {
        for x in 0..32 {
            img.extend(if (10..22).contains(&x) && (10..22).contains(&y) { [200, 200, 200, 255] } else { [12, 12, 12, 255] });
        }
    }
    let mut n = 40;
    let mut shot = |kind: EffectKind, set: &[(usize, f32)]| {
        n += 1;
        let fg = layer(n, &img, [32, 32], [32.0, 32.0], Transform::default(), BlendMode::Normal);
        c.render([32, 32], &[full(1, &red, [32, 32]), with_fx(fg, vec![fx(kind, set)])]);
        c.read_output().unwrap()
    };
    let r = shot(EffectKind::LumaKey, &[]);
    assert!(near(px(&r, 32, 3, 3), [255, 0, 0]) && near(px(&r, 32, 16, 16), [200, 200, 200]), "dark keyed: {:?} {:?}", px(&r, 32, 3, 3), px(&r, 32, 16, 16));
    let r = shot(EffectKind::LumaKey, &[(key::LUMA_MODE, 1.0), (key::LUMA_THRESHOLD, 30.0)]);
    assert!(near(px(&r, 32, 3, 3), [12, 12, 12]) && near(px(&r, 32, 16, 16), [255, 0, 0]), "light keyed");
    // Choke 3 px through the luma key's own matte settings (from index 3).
    let base = key::matte_base(EffectKind::LumaKey).unwrap();
    let m = shot(EffectKind::LumaKey, &[(base + 4, key::VIEW_MATTE), (base + 2, 3.0)]);
    let row: Vec<u8> = (0..32).map(|x| px(&m, 32, x, 16)[0]).collect();
    assert!(row[12] < 10 && row[13] > 245 && row[18] > 245 && row[19] < 10, "choke: {row:?}");
}

/// Key cleaner: green spill goes from a grey (yellow is left alone), a
/// pinhole in the matte is filled and a lone speck removed, with the
/// subject's edge where it was.
#[test]
fn key_cleaner_removes_spill_holes_and_specks() {
    use reelforge_core::effects::cleaner;
    let Some(mut c) = setup() else { return };
    let red = solid(1, 1, [255, 0, 0, 255]);
    // A 16×16 square at 8–23 (green-tinted grey on the left, yellow on the
    // right) with a pinhole at 16,16, and a speck at 2,2, on nothing.
    let mut img = Vec::new();
    for y in 0..32 {
        for x in 0..32 {
            let square = (8..24).contains(&x) && (8..24).contains(&y) && (x, y) != (16, 16);
            img.extend(if (x, y) == (2, 2) {
                [200, 200, 200, 255]
            } else if square && x < 16 {
                [110, 170, 110, 255]
            } else if square {
                [220, 220, 30, 255]
            } else {
                [0, 0, 0, 0]
            });
        }
    }
    let mut n = 90;
    let mut shot = |set: &[(usize, f32)]| {
        n += 1;
        let fg = layer(n, &img, [32, 32], [32.0, 32.0], Transform::default(), BlendMode::Normal);
        c.render([32, 32], &[full(1, &red, [32, 32]), with_fx(fg, vec![fx(EffectKind::KeyCleaner, set)])]);
        c.read_output().unwrap()
    };
    let r = shot(&[]);
    let grey = px(&r, 32, 11, 12);
    assert!((grey[1] as i32 - grey[0] as i32) < 20 && (grey[1] as i32 - grey[2] as i32) < 20, "spill left: {grey:?}");
    assert!(grey[1] > 110 && grey[1] < 170, "lightness kept: {grey:?}");
    assert!(near(px(&r, 32, 20, 12), [220, 220, 30]), "yellow changed: {:?}", px(&r, 32, 20, 12));
    // Nothing done to the matte by default.
    assert!(near(px(&r, 32, 16, 16), [255, 0, 0]) && near(px(&r, 32, 2, 2), [200, 200, 200]));
    let r = shot(&[(cleaner::HOLES, 2.0), (cleaner::SPECKS, 2.0)]);
    assert!(px(&r, 32, 16, 16)[1] > 150, "hole filled: {:?}", px(&r, 32, 16, 16));
    assert!(near(px(&r, 32, 2, 2), [255, 0, 0]), "speck left: {:?}", px(&r, 32, 2, 2));
    assert!(near(px(&r, 32, 7, 12), [255, 0, 0]) && !near(px(&r, 32, 8, 12), [255, 0, 0]) && near(px(&r, 32, 24, 12), [255, 0, 0]), "edge moved");
    let m = shot(&[(cleaner::HOLES, 2.0), (cleaner::VIEW, 1.0)]);
    assert!(near(px(&m, 32, 16, 16), [255, 255, 255]) && near(px(&m, 32, 4, 4), [0, 0, 0]));
    assert!(fx(EffectKind::KeyCleaner, &[(cleaner::SPILL, 0.0)]).is_identity());
}

/// Matte choker: shrinks and grows a cut-out square's edge, softens it, and
/// matte contrast turns a half see-through area solid (or gone).
#[test]
fn matte_choker_moves_softens_and_firms_the_edge() {
    use reelforge_core::effects::choker;
    let Some(mut c) = setup() else { return };
    let black = solid(1, 1, [0, 0, 0, 255]);
    // A white 12×12 square at 10–21 on nothing, and a 60% strip at x 26–29.
    let mut img = Vec::new();
    for y in 0..32 {
        for x in 0..32 {
            img.extend(if (10..22).contains(&x) && (10..22).contains(&y) {
                [255, 255, 255, 255]
            } else if (26..30).contains(&x) {
                [255, 255, 255, 153]
            } else {
                [0, 0, 0, 0]
            });
        }
    }
    let mut n = 110;
    let mut shot = |set: &[(usize, f32)]| {
        n += 1;
        let fg = layer(n, &img, [32, 32], [32.0, 32.0], Transform::default(), BlendMode::Normal);
        c.render([32, 32], &[full(1, &black, [32, 32]), with_fx(fg, vec![fx(EffectKind::MatteChoker, set)])]);
        let r = c.read_output().unwrap();
        (0..32).map(|x| px(&r, 32, x, 16)[0]).collect::<Vec<u8>>()
    };
    let row = shot(&[(choker::CHOKE, 2.0), (choker::SOFTNESS, 0.0)]);
    assert!(row[11] < 10 && row[12] > 245 && row[19] > 245 && row[20] < 10, "choke: {row:?}");
    let row = shot(&[(choker::CHOKE, -2.0), (choker::SOFTNESS, 0.0)]);
    // (The strip grows too, from 24, but stays 60%.)
    assert!(row[7] < 10 && row[8] > 245 && row[23] > 245 && row[24] < 230, "grow: {row:?}");
    let row = shot(&[(choker::CHOKE, 0.0), (choker::SOFTNESS, 4.0)]);
    assert!(row[10] > 30 && row[10] < 225 && row[16] > 245 && row[6] < 40, "soft: {row:?}");
    // The 60% strip: as it is, then solid with full contrast and a 50% midpoint,
    // then gone with the midpoint above it.
    let strip = |row: &[u8]| row[27];
    let as_is = strip(&shot(&[(choker::CHOKE, 0.0), (choker::SOFTNESS, 0.0)]));
    assert!(as_is > 150 && as_is < 230, "strip: {as_is}");
    assert!(strip(&shot(&[(choker::CHOKE, 0.0), (choker::SOFTNESS, 0.0), (choker::CONTRAST, 100.0)])) > 250);
    assert!(strip(&shot(&[(choker::CHOKE, 0.0), (choker::SOFTNESS, 0.0), (choker::CONTRAST, 100.0), (choker::MIDPOINT, 70.0)])) < 5);
    assert!(fx(EffectKind::MatteChoker, &[(choker::CHOKE, 0.0), (choker::SOFTNESS, 0.0)]).is_identity());
}

/// Refine matte: a half see-through edge tinted by the screen takes its
/// colour from the solid inside; smoothing wipes out a lone speck and keeps
/// the body; the edge shifts and feathers.
#[test]
fn refine_matte_decontaminates_smooths_and_shifts() {
    use reelforge_core::effects::refine;
    let Some(mut c) = setup() else { return };
    let black = solid(1, 1, [0, 0, 0, 255]);
    // A skin-coloured square at 8–23, a 50% green ring 2 px wide round it, and
    // a speck at 2,2.
    let mut img = Vec::new();
    for y in 0..32i32 {
        for x in 0..32i32 {
            let inside = |lo: i32, hi: i32| (lo..hi).contains(&x) && (lo..hi).contains(&y);
            img.extend(if (x, y) == (2, 2) {
                [255, 255, 255, 255]
            } else if inside(8, 24) {
                [200, 120, 80, 255]
            } else if inside(6, 26) {
                [60, 200, 60, 128]
            } else {
                [0, 0, 0, 0]
            });
        }
    }
    let mut n = 130;
    let mut shot = |set: &[(usize, f32)]| {
        n += 1;
        let fg = layer(n, &img, [32, 32], [32.0, 32.0], Transform::default(), BlendMode::Normal);
        c.render([32, 32], &[full(1, &black, [32, 32]), with_fx(fg, vec![fx(EffectKind::RefineMatte, set)])]);
        c.read_output().unwrap()
    };
    let plain = [(refine::SMOOTH, 0.0), (refine::DECONTAMINATE, 0.0)];
    let r = shot(&plain);
    let edge = px(&r, 32, 7, 16);
    assert!(edge[1] > edge[0], "green edge to start with: {edge:?}");
    let r = shot(&[(refine::SMOOTH, 0.0), (refine::DECONTAMINATE, 100.0)]);
    let edge = px(&r, 32, 7, 16);
    assert!(edge[0] > edge[1] && edge[0] > 60, "edge takes the skin's colour: {edge:?}");
    assert!(near(px(&r, 32, 16, 16), [200, 120, 80]), "inside unchanged");
    let mut matte = |set: &[(usize, f32)]| {
        let mut all = set.to_vec();
        all.push((refine::VIEW, 1.0));
        shot(&all)
    };
    let m = matte(&[(refine::SMOOTH, 2.0)]);
    assert!(px(&m, 32, 2, 2)[0] < 20, "speck left: {:?}", px(&m, 32, 2, 2));
    assert!(px(&m, 32, 12, 16)[0] > 245 && px(&m, 32, 2, 16)[0] < 10);
    let m = matte(&[(refine::SMOOTH, 0.0), (refine::SHIFT, 3.0)]);
    let v = px(&m, 32, 10, 16)[0];
    // (The matte shows in sRGB: 50% is 188.)
    assert!(v > 175 && v < 200, "shifted in to the ring: {v}");
    let m = matte(&[(refine::SMOOTH, 0.0), (refine::FEATHER, 6.0)]);
    let v = px(&m, 32, 5, 16)[0];
    assert!(v > 30 && v < 188, "feathered: {v}");
    assert!(fx(EffectKind::RefineMatte, &plain).is_identity());
}

/// Wire removal: a black vertical wire across a left-to-right ramp is filled
/// from either side, close to what the ramp would have been; the rest is untouched.
#[test]
fn wire_removal_fills_the_wire_from_either_side() {
    use reelforge_core::effects::wire;
    let Some(mut c) = setup() else { return };
    let mut img = Vec::new();
    for _y in 0..32 {
        for x in 0..32u8 {
            img.extend(if x == 15 || x == 16 { [0, 0, 0, 255] } else { [x * 8, 100, 50, 255] });
        }
    }
    let mut n = 150;
    let mut shot = |set: &[(usize, f32)]| {
        n += 1;
        let l = layer(n, &img, [32, 32], [32.0, 32.0], Transform::default(), BlendMode::Normal);
        c.render([32, 32], &[with_fx(l, vec![fx(EffectKind::WireRemoval, set)])]);
        c.read_output().unwrap()
    };
    let on = [(wire::START_X, 50.0), (wire::START_Y, -10.0), (wire::END_X, 50.0), (wire::END_Y, 110.0), (wire::WIDTH, 2.0)];
    let r = shot(&on);
    for (x, want) in [(15u32, 120i32), (16, 128)] {
        let p = px(&r, 32, x, 16);
        assert!((p[0] as i32 - want).abs() < 25 && (p[1] as i32 - 100).abs() < 10, "wire at {x}: {p:?}");
    }
    assert!(near(px(&r, 32, 25, 16), [200, 100, 50]) && near(px(&r, 32, 5, 3), [40, 100, 50]), "outside changed");
    // Away from the wire (moved to the right edge), it stays black.
    let r = shot(&[(wire::START_X, 90.0), (wire::START_Y, -10.0), (wire::END_X, 90.0), (wire::END_Y, 110.0), (wire::WIDTH, 2.0)]);
    assert!(near(px(&r, 32, 15, 16), [0, 0, 0]));
    let mut show = on.to_vec();
    show.push((wire::SHOW, 1.0));
    let r = shot(&show);
    let p = px(&r, 32, 16, 16);
    assert!(p[0] > p[1] + 60, "area shown red: {p:?}");
}

/// Difference key: whatever matches the clean plate goes; what differs stays.
#[test]
fn difference_key_removes_what_matches_the_plate() {
    use reelforge_core::effects::key;
    use std::sync::Arc;
    let Some(mut c) = setup() else { return };
    let red = solid(1, 1, [255, 0, 0, 255]);
    // A busy background (a gradient with noise), and the same with a blue square on it.
    let bg = |x: u32, y: u32| -> [u8; 4] { [(x * 7 + y * 3) as u8 % 200 + 20, (y * 6) as u8 % 180 + 40, ((x ^ y) * 9) as u8 % 160 + 50, 255] };
    let mut plate = Vec::new();
    let mut shot = Vec::new();
    for y in 0..32u32 {
        for x in 0..32u32 {
            plate.extend(bg(x, y));
            shot.extend(if (10..22).contains(&x) && (10..22).contains(&y) { [30, 60, 230, 255] } else { bg(x, y) });
        }
    }
    let table = Arc::new(reelforge_core::lut::Table { id: 77, width: 32, height: 32, rgba: plate, cube_size: 0 });
    let mut e = fx(EffectKind::DifferenceKey, &[]);
    e.table = Some(table.clone());
    let fg = layer(60, &shot, [32, 32], [32.0, 32.0], Transform::default(), BlendMode::Normal);
    c.render([32, 32], &[full(1, &red, [32, 32]), with_fx(fg, vec![e])]);
    let r = c.read_output().unwrap();
    for (x, y) in [(2, 2), (5, 28), (28, 9), (30, 30)] {
        assert!(near(px(&r, 32, x, y), [255, 0, 0]), "background at {x},{y} keyed: {:?}", px(&r, 32, x, y));
    }
    assert!(near(px(&r, 32, 16, 16), [30, 60, 230]), "the square stays: {:?}", px(&r, 32, 16, 16));
    // The matte view, with the matte settings from index 2.
    let base = key::matte_base(EffectKind::DifferenceKey).unwrap();
    let mut e = fx(EffectKind::DifferenceKey, &[(base + 4, key::VIEW_MATTE)]);
    e.table = Some(table);
    let fg = layer(61, &shot, [32, 32], [32.0, 32.0], Transform::default(), BlendMode::Normal);
    c.render([32, 32], &[full(1, &red, [32, 32]), with_fx(fg, vec![e])]);
    let m = c.read_output().unwrap();
    assert!(near(px(&m, 32, 2, 2), [0, 0, 0]) && near(px(&m, 32, 16, 16), [255, 255, 255]));
    // Without its plate it changes nothing.
    assert!(fx(EffectKind::DifferenceKey, &[]).is_identity());
}

/// The eyedropper's settings (`key::pick`) take out the screen it was picked
/// from, even a dull, dark one the default settings leave behind, and keep a
/// skin tone.
#[test]
fn a_picked_key_colour_takes_the_screen_out() {
    use reelforge_core::effects::key;
    let Some(mut c) = setup() else { return };
    let red = solid(1, 1, [255, 0, 0, 255]);
    let mut n = 80;
    for screen in [[46u8, 66, 48], [40, 70, 150], [60, 175, 95]] {
        let mut img = Vec::new();
        for y in 0..8 {
            for _ in 0..8 {
                img.extend(if y < 4 { [screen[0], screen[1], screen[2], 255] } else { [205, 150, 125, 255] });
            }
        }
        let rgb = screen.map(|v| v as f32 / 255.0);
        let (hue, lower) = key::pick(rgb, 12.0, 10.0).unwrap();
        let mut set = vec![(0, hue)];
        if let Some((t, s)) = lower {
            set.extend([(1, t), (2, s)]);
        }
        n += 1;
        let fg = layer(n, &img, [8, 8], [8.0, 8.0], Transform::default(), BlendMode::Normal);
        c.render([8, 8], &[full(1, &red, [8, 8]), with_fx(fg, vec![fx(EffectKind::ChromaKey, &set)])]);
        let out = c.read_output().unwrap();
        assert!(near(px(&out, 8, 4, 1), [255, 0, 0]), "{screen:?} keyed (hue {hue}, threshold {lower:?}): {:?}", px(&out, 8, 4, 1));
        assert!(near(px(&out, 8, 4, 6), [205, 150, 125]), "{screen:?}: skin stays: {:?}", px(&out, 8, 4, 6));
    }
    // The dull screen does need the lower threshold: with the default it stays.
    n += 1;
    let img: Vec<u8> = (0..64).flat_map(|_| [46u8, 66, 48, 255]).collect();
    let (hue, _) = key::pick([46.0 / 255.0, 66.0 / 255.0, 48.0 / 255.0], 12.0, 10.0).unwrap();
    let fg = layer(n, &img, [8, 8], [8.0, 8.0], Transform::default(), BlendMode::Normal);
    c.render([8, 8], &[full(1, &red, [8, 8]), with_fx(fg, vec![fx(EffectKind::ChromaKey, &[(0, hue)])])]);
    assert!(!near(px(&c.read_output().unwrap(), 8, 4, 4), [255, 0, 0]));
}

/// A garbage matte for keys: inside it the layer is kept, however much it
/// looks like the screen; outside, the key works as usual. With a layer
/// mask too, what the mask cuts away stays cut.
#[test]
fn keys_keep_the_layer_inside_a_garbage_matte() {
    use reelforge_core::effects::key;
    use reelforge_render::MaskInput;
    let Some(mut c) = setup() else { return };
    let red = solid(1, 1, [255, 0, 0, 255]);
    // All green screen: 16 × 16.
    let img: Vec<u8> = (0..16 * 16).flat_map(|_| [50u8, 180, 80, 255]).collect();
    // The keep mask: the left half. The layer mask: all but the bottom row.
    let keep: Vec<u8> = (0..16 * 16).map(|i| if i % 16 < 8 { 255 } else { 0 }).collect();
    let all: Vec<u8> = (0..16 * 16).map(|i| if i / 16 < 15 { 255 } else { 0 }).collect();
    let mut n = 90;
    for choke in [0.0, 2.0] {
        n += 1;
        let mut fg = layer(n, &img, [16, 16], [16.0, 16.0], Transform::default(), BlendMode::Normal);
        fg.mask = Some(MaskInput { content_id: 5, pixels: &all, size: [16, 16], keep: &keep });
        fg = with_fx(fg, vec![fx(EffectKind::ChromaKey, &[(key::CHOKE, choke)])]);
        c.render([16, 16], &[full(1, &red, [16, 16]), fg]);
        let out = c.read_output().unwrap();
        assert!(near(px(&out, 16, 3, 8), [50, 180, 80]), "choke {choke}: kept inside the matte: {:?}", px(&out, 16, 3, 8));
        assert!(near(px(&out, 16, 12, 8), [255, 0, 0]), "choke {choke}: keyed outside it: {:?}", px(&out, 16, 12, 8));
        assert!(near(px(&out, 16, 3, 15), [255, 0, 0]), "choke {choke}: the layer mask still cuts: {:?}", px(&out, 16, 3, 15));
    }
    // Only garbage mattes (no layer mask): the layer isn't cut anywhere.
    n += 1;
    let full_mask = vec![255u8; 16 * 16];
    let mut fg = layer(n, &img, [16, 16], [16.0, 16.0], Transform::default(), BlendMode::Normal);
    fg.mask = Some(MaskInput { content_id: 6, pixels: &full_mask, size: [16, 16], keep: &keep });
    c.render([16, 16], &[full(1, &red, [16, 16]), with_fx(fg, vec![fx(EffectKind::ChromaKey, &[])])]);
    let out = c.read_output().unwrap();
    assert!(near(px(&out, 16, 3, 15), [50, 180, 80]) && near(px(&out, 16, 12, 15), [255, 0, 0]));
}

#[test]
fn a_lower_output_scale_draws_the_same_picture_smaller() {
    let Some(mut c) = setup() else { return };
    let red = solid(64, 32, [255, 0, 0, 255]);
    let blue = solid(2, 2, [0, 0, 255, 255]);
    let top = Transform { position: [10.0, 0.0], opacity: 0.5, ..Default::default() };
    let layers = |red: &'static [u8], blue: &'static [u8]| [layer(1, red, [64, 32], [64.0, 32.0], Transform::default(), BlendMode::Normal), layer(2, blue, [2, 2], [16.0, 16.0], top, BlendMode::Normal)];
    let (red, blue): (&'static [u8], &'static [u8]) = (Box::leak(red.into_boxed_slice()), Box::leak(blue.into_boxed_slice()));
    // Half resolution: a 32 × 16 picture, the blue square (x 34–50, y 8–24 in
    // the frame) at x 17–25, y 4–12.
    c.output_scale = 0.5;
    c.render([64, 32], &layers(red, blue));
    assert_eq!(c.output_size(), Some([32, 16]));
    let img = c.read_output().unwrap();
    assert!(near(px(&img, 32, 21, 8), [188, 0, 188]), "{:?}", px(&img, 32, 21, 8));
    assert!(near(px(&img, 32, 14, 8), [255, 0, 0]), "left of it stays red");
    assert!(near(px(&img, 32, 21, 2), [255, 0, 0]), "above it stays red");
    // Quarter, then back to full.
    c.output_scale = 0.25;
    c.render([64, 32], &layers(red, blue));
    assert_eq!(c.output_size(), Some([16, 8]));
    c.output_scale = 1.0;
    c.render([64, 32], &layers(red, blue));
    assert_eq!(c.output_size(), Some([64, 32]));
    assert!(near(px(&c.read_output().unwrap(), 64, 42, 16), [188, 0, 188]));
}

/// A 32 × 32 mesh warp (the finest grid): drawn where its points put it.
#[test]
fn a_fine_mesh_warp_draws_where_its_points_are() {
    let Some(mut c) = setup() else { return };
    let green = solid(4, 4, [0, 255, 0, 255]);
    // The layer squeezed into the left half of a 64 × 32 frame, with the
    // grid's middle column (x 16) pulled right by 8 px: a thin spike to x 24
    // (only a fine grid's points can be this close together).
    let mut m = reelforge_core::meshwarp::MeshShape::filling([[0.0, 0.0], [32.0, 0.0], [32.0, 32.0], [0.0, 32.0]], 32, 32);
    for j in 0..=32 {
        m.points[j * 33 + 16][0] += 8.0;
    }
    let mut l = layer(1, &green, [4, 4], [64.0, 32.0], Transform::default(), BlendMode::Normal);
    l.mesh = Some(m);
    c.render([64, 32], &[l]);
    let img = c.read_output().unwrap();
    assert!(near(px(&img, 64, 4, 16), [0, 255, 0]), "inside the grid");
    assert!(near(px(&img, 64, 22, 16), [0, 255, 0]), "in the spike (x 17 → 24)");
    assert!(near(px(&img, 64, 40, 16), [0, 0, 0]), "past the grid: nothing");
}

/// HDR footage (10-bit PQ and HLG, BT.2020) comes in as HDR: SDR white at 1,
/// highlights above it, and written out again with the same curve it's the
/// same signal (a grey and a 1000-nit highlight, through PQ and back).
#[test]
fn hdr_footage_stays_hdr_through_to_an_hdr_export() {
    use reelforge_core::frame::{f16_to_f32, Chroma, Transfer, YuvLayout, YuvMatrix, YuvPlanes};
    use reelforge_render::DeepCurve;
    let Some(mut c) = setup() else { return };
    let layout = YuvLayout::Planar { bits: 10, chroma: Chroma::C444, alpha: false };
    // Full-range 10-bit neutral greys: Y code = signal × 1023, chroma at 512.
    let frame = |signal: f32, transfer: Transfer| {
        let y = (signal * 1023.0).round() as u16;
        let plane = |v: u16| -> Vec<u8> { (0..16).flat_map(|_| v.to_le_bytes()).collect() };
        YuvPlanes { layout, width: 4, height: 4, planes: vec![plane(y), plane(512), plane(512)], matrix: YuvMatrix::Bt2020, full_range: true, transfer }
    };
    for (i, (transfer, curve, signal, want)) in [
        // PQ: 203 nits (SDR white) is signal 0.5806; 1000 nits is 0.7518 (4.93 × white).
        (Transfer::Pq, DeepCurve::Pq, 0.5806, 1.0),
        (Transfer::Pq, DeepCurve::Pq, 0.7518, 1000.0 / 203.0),
        // HLG: 75% signal is SDR white; 90% is well above it.
        (Transfer::Hlg, DeepCurve::Hlg, 0.75, 1.0),
        (Transfer::Hlg, DeepCurve::Hlg, 0.9, Transfer::Hlg.linear(0.9)),
    ]
    .into_iter()
    .enumerate()
    {
        let yuv = frame(signal, transfer);
        c.render([4, 4], &[yuv_layer(&yuv, 91 + i as u64)]);
        let f = c.read_output_linear().unwrap();
        let g = f16_to_f32(f.rgba[1]);
        assert!((g / want - 1.0).abs() < 0.02, "{transfer:?} {signal}: {g} (want {want})");
        // The CPU's conversion agrees.
        let cpu = f16_to_f32(yuv.to_float().rgba[1]);
        assert!((cpu / g - 1.0).abs() < 0.01, "{transfer:?} CPU {cpu} vs GPU {g}");
        // Out through the same curve: the same signal.
        let out = c.read_output_deep(curve).unwrap();
        let back = out[1] as f32 / 65535.0;
        let code = (signal * 1023.0).round() / 1023.0;
        assert!((back - code).abs() < 0.004, "{transfer:?} {signal} came back {back}");
    }
    // An SDR picture of HDR footage (thumbnails, 8-bit): white at white, the highlight cut.
    let hi = frame(0.7518, Transfer::Pq).to_rgba();
    assert_eq!(hi[1], 255);
    let white = frame(0.5806, Transfer::Pq).to_rgba();
    assert!(white[1] >= 253, "{}", white[1]);
}

/// Shadows: a small square between a light and a big wall casts a shadow on
/// the wall, as dark as the light says; its picture's shape counts (a
/// transparent hole lets light through), and a light without shadows casts none.
#[test]
fn three_d_layers_cast_shadows_on_each_other() {
    use reelforge_core::space3d::{layer_space, Camera, Light, Scene3d};
    let Some(mut c) = setup() else { return };
    let (frame, f) = ([400u32, 200u32], [400.0f32, 200.0f32]);
    let cam = Camera::default_for(f);
    let white = solid(4, 4, [255, 255, 255, 255]);
    // A blocker with a transparent left half.
    let mut half = Vec::new();
    for _y in 0..8 {
        for x in 0..8 {
            half.extend(if x < 4 { [0, 0, 0, 0] } else { [255, 255, 255, 255] });
        }
    }
    let wall = Transform { three_d: true, ..Default::default() };
    let blocker = Transform { three_d: true, depth: -300.0, ..Default::default() };
    // A point light straight in front, so the shadow lands right behind the blocker, a little bigger.
    let light = |shadow: f32| Light { kind: 0, color: [1.0; 3], position: [0.0, 0.0, -600.0], direction: [0.0, 0.0, 1.0], cone: [0.5, 0.6], falloff: 0, radius: 1000.0, shadow, softness: 0.0, env: None };
    let render = |c: &mut reelforge_render::Compositor, shadow: f32, blocker_pixels: &[u8], blocker_px: [u32; 2]| {
        let scene = Scene3d { camera: cam, lights: vec![light(shadow)] };
        let (ws, bs) = (layer_space(&wall, [360.0, 180.0], &scene), layer_space(&blocker, [40.0, 40.0], &scene));
        let b = LayerInput { space: Some(&bs), ..layer(3, blocker_pixels, blocker_px, [40.0, 40.0], blocker, BlendMode::Normal) };
        c.render(frame, &[LayerInput { space: Some(&ws), ..layer(2, &white, [4, 4], [360.0, 180.0], wall, BlendMode::Normal) }, b]);
        c.read_output().unwrap()
    };
    // The blocker sits at depth −300, so it shows bigger on screen; its shadow on
    // the wall (seen past it) peeks out around it: look just outside the
    // blocker's picture but inside where the shadow falls.
    // On screen the blocker spans ±40·f/(f−300)/2; its shadow on the wall
    // (light at −600, blocker at −300: twice the size) spans ±40 at depth 0.
    let k = cam.focal() / (cam.focal() - 300.0);
    let blocker_half = 20.0 * k;
    assert!(blocker_half < 38.0, "the shadow must peek out: {blocker_half}");
    let probe = (200 + 36) as u32; // right of the blocker, inside the shadow
    let off = render(&mut c, 0.0, &white, [4, 4]);
    assert!(near(px(&off, 400, probe, 100), [255, 255, 255]), "no shadows: {:?}", px(&off, 400, probe, 100));
    let on = render(&mut c, 0.8, &white, [4, 4]);
    let g = px(&on, 400, probe, 100);
    // 80% darker in linear light: 0.2 → about 124 in sRGB.
    assert!((110..140).contains(&g[0]), "shadowed: {g:?}");
    // Far from the blocker: lit as before.
    assert!(near(px(&on, 400, 200 + 120, 100), [255, 255, 255]));
    // The blocker's own picture counts: its transparent left half lets light through.
    let holed = render(&mut c, 0.8, &half, [8, 8]);
    assert!(near(px(&holed, 400, 200 - 36, 100), [255, 255, 255]), "through the hole: {:?}", px(&holed, 400, 200 - 36, 100));
    assert!((110..140).contains(&px(&holed, 400, probe, 100)[0]), "behind the solid half: {:?}", px(&holed, 400, probe, 100));
}

/// Soft shadows: with a softness, a shadow's edge fades over a width that
/// grows with how far the shadow falls from what casts it; its middle stays dark.
#[test]
fn soft_shadows_fade_at_the_edges() {
    use reelforge_core::space3d::{layer_space, shadow_softness, Camera, Light, Scene3d};
    let Some(mut c) = setup() else { return };
    let (frame, f) = ([400u32, 200u32], [400.0f32, 200.0f32]);
    let cam = Camera::default_for(f);
    let white = solid(4, 4, [255, 255, 255, 255]);
    let wall = Transform { three_d: true, ..Default::default() };
    let blocker = Transform { three_d: true, depth: -300.0, ..Default::default() };
    let render = |c: &mut reelforge_render::Compositor, softness: f32| {
        let light = Light { kind: 0, color: [1.0; 3], position: [0.0, 0.0, -600.0], direction: [0.0, 0.0, 1.0], cone: [0.5, 0.6], falloff: 0, radius: 1000.0, shadow: 0.8, softness, env: None };
        let scene = Scene3d { camera: cam, lights: vec![light] };
        let (ws, bs) = (layer_space(&wall, [360.0, 180.0], &scene), layer_space(&blocker, [40.0, 40.0], &scene));
        let b = LayerInput { space: Some(&bs), ..layer(3, &white, [4, 4], [40.0, 40.0], blocker, BlendMode::Normal) };
        c.render(frame, &[LayerInput { space: Some(&ws), ..layer(2, &white, [4, 4], [360.0, 180.0], wall, BlendMode::Normal) }, b]);
        c.read_output().unwrap()
    };
    // Along the row through the middle, from just right of the blocker outwards.
    let k = cam.focal() / (cam.focal() - 300.0);
    let from = 200 + (20.0 * k).ceil() as u32 + 1;
    let row = |img: &[u8]| (from..300).map(|x| px(img, 400, x, 100)[0]).collect::<Vec<u8>>();
    let between = |r: &[u8]| r.iter().filter(|v| (140..235).contains(*v)).count();
    let hard = row(&render(&mut c, 0.0));
    let soft_img = render(&mut c, shadow_softness(20.0));
    let soft = row(&soft_img);
    assert!(between(&hard) <= 3, "a hard edge: {hard:?}");
    assert!(between(&soft) >= 15, "a soft edge: {soft:?}");
    // Brighter steadily outwards (a little grain allowed), and lit far out.
    assert!(soft.windows(4).all(|w| w[3] as i32 >= w[0] as i32 - 6), "{soft:?}");
    assert!(*soft.last().unwrap() > 245);
    // Straight behind the blocker it's still fully in shadow: look from above,
    // past the blocker's top edge, at the wall just inside the shadow.
    let top = 100 - (20.0 * k).ceil() as u32 - 2;
    let (h, s2) = (px(&render(&mut c, 0.0), 400, 200, top)[0], px(&soft_img, 400, 200, top)[0]);
    assert!((110..140).contains(&h) && s2 < 160, "in the shadow: hard {h}, soft {s2}");
}

/// 3D layers that cut through each other do: a red one facing the camera and
/// a blue one turned 45° through the same middle, drawn after it. Each shows
/// on the side where it's nearer (not the later one all over), and a 2D
/// layer after them still covers both.
#[test]
fn three_d_layers_cut_through_each_other() {
    use reelforge_core::space3d::{layer_space, Camera, Scene3d};
    let Some(mut c) = setup() else { return };
    let (frame, f) = ([400u32, 200u32], [400.0f32, 200.0f32]);
    let scene = Scene3d { camera: Camera::default_for(f), lights: Vec::new() };
    let red = solid(4, 4, [255, 0, 0, 255]);
    let blue = solid(4, 4, [0, 0, 255, 255]);
    let flat = Transform { three_d: true, ..Default::default() };
    let mut turned = flat;
    turned.orientation = [0.0, 45.0];
    let (rs, bs) = (layer_space(&flat, [160.0, 120.0], &scene), layer_space(&turned, [160.0, 120.0], &scene));
    let layers = [LayerInput { space: Some(&rs), ..layer(1, &red, [4, 4], [160.0, 120.0], flat, BlendMode::Normal) }, LayerInput { space: Some(&bs), ..layer(2, &blue, [4, 4], [160.0, 120.0], turned, BlendMode::Normal) }];
    c.render(frame, &layers);
    let out = c.read_output().unwrap();
    let (l, r) = (px(&out, 400, 200 - 30, 100), px(&out, 400, 200 + 30, 100));
    let is = |p: [u8; 4], want: [u8; 3]| near(p, want);
    assert!(
        (is(l, [255, 0, 0]) && is(r, [0, 0, 255])) || (is(l, [0, 0, 255]) && is(r, [255, 0, 0])),
        "one side each: left {l:?}, right {r:?}"
    );
    // A 2D layer after the 3D run is drawn over it as usual.
    let green = solid(4, 4, [0, 255, 0, 255]);
    let mut all = layers.to_vec();
    all.push(layer(3, &green, [4, 4], [40.0, 40.0], Transform::default(), BlendMode::Normal));
    c.render(frame, &all);
    let out = c.read_output().unwrap();
    assert!(near(px(&out, 400, 200, 100), [0, 255, 0]));
    assert!(near(px(&out, 400, 200 - 30, 100), [l[0], l[1], l[2]]));
}

/// Neon edges: a white square on a transparent layer gets a cyan tube along
/// its edge with a glow outside it, keeps its middle (or not, at Original 0),
/// finds the edges in a picture's brightness, and flickers when asked.
#[test]
fn neon_edges_trace_the_shape() {
    use reelforge_core::effects::neon;
    let Some(mut c) = setup() else { return };
    let (w, h) = (160u32, 90u32);
    let square: Vec<u8> = (0..h).flat_map(|y| (0..w).flat_map(move |x| if (50..110).contains(&x) && (25..65).contains(&y) { [255u8; 4] } else { [0, 0, 0, 0] })).collect();
    let mut key = 900u64;
    let mut draw = |c: &mut reelforge_render::Compositor, px: &[u8], set: &[(usize, f32)], t: f32| {
        let mut v = fx(EffectKind::NeonEdges, set);
        v.time = t;
        key += 1;
        let mut l = layer(1, px, [w, h], [w as f32, h as f32], Transform::default(), BlendMode::Normal);
        l.content_id = key;
        c.render([w, h], &[with_fx(l, vec![v])]);
        c.read_output().unwrap()
    };
    let img = draw(&mut c, &square, &[(neon::CORE, 0.0)], 0.0);
    // On the edge (just outside the square, where the tube is over nothing): cyan, bright.
    let edge = px(&img, w, 48, 45);
    assert!(edge[2] > 200 && edge[1] > 200 && edge[0] < 120, "tube: {edge:?}");
    // Just outside: the glow, cyan, dimmer.
    let glow = px(&img, w, 40, 45);
    assert!(glow[2] > 30 && glow[2] < edge[2] && glow[0] < glow[2], "glow: {glow:?}");
    // Far away: nothing; the middle: the white square.
    assert!(px(&img, w, 2, 2)[2] < 10, "{:?}", px(&img, w, 2, 2));
    assert!(near(px(&img, w, 80, 45), [255, 255, 255]));
    // Original 0: the middle is dark, the tube still there (a neon sign).
    let sign = draw(&mut c, &square, &[(neon::ORIGINAL, 0.0), (neon::GLOW_INTENSITY, 0.0)], 0.0);
    assert!(px(&sign, w, 80, 45)[2] < 10 && px(&sign, w, 48, 45)[2] > 200);
    // Hot core: the tube's middle goes white.
    let hot = draw(&mut c, &square, &[(neon::CORE, 100.0), (neon::GLOW_INTENSITY, 0.0)], 0.0);
    assert!(px(&hot, w, 48, 45)[0] > edge[0] + 60, "{:?}", px(&hot, w, 48, 45));
    // From the picture: an opaque black-to-white step gets a tube at the step.
    let step: Vec<u8> = (0..h).flat_map(|_| (0..w).flat_map(|x| if x < 80 { [0u8, 0, 0, 255] } else { [255u8; 4] })).collect();
    let pic = draw(&mut c, &step, &[(neon::SOURCE, 1.0), (neon::ORIGINAL, 0.0), (neon::GLOW_INTENSITY, 0.0)], 0.0);
    assert!(px(&pic, w, 80, 45)[2] > 150 && px(&pic, w, 30, 45)[2] < 10 && px(&pic, w, 130, 45)[2] < 10);
    // Flicker: some moments dimmer than others.
    let mut lit = |t: f32| {
        let i = draw(&mut c, &square, &[(neon::FLICKER, 100.0), (neon::FLICKER_SPEED, 10.0), (neon::GLOW_INTENSITY, 0.0)], t);
        px(&i, w, 48, 45)[2]
    };
    let levels: Vec<u8> = (0..30).map(|k| lit(k as f32 * 0.1)).collect();
    assert!(levels.iter().max().unwrap() - levels.iter().min().unwrap() > 40, "{levels:?}");
}

#[test]
fn halftone_prints_dots() {
    use reelforge_core::effects::halftone;
    let Some(mut c) = setup() else { return };
    let (w, h) = (160u32, 90u32);
    let flat = |rgb: [u8; 3]| -> Vec<u8> { (0..w * h).flat_map(|_| [rgb[0], rgb[1], rgb[2], 255]).collect() };
    let mut key = 1900u64;
    let mut draw = |c: &mut reelforge_render::Compositor, px: &[u8], set: &[(usize, f32)]| {
        key += 1;
        let mut l = layer(1, px, [w, h], [w as f32, h as f32], Transform::default(), BlendMode::Normal);
        l.content_id = key;
        c.render([w, h], &[with_fx(l, vec![fx(EffectKind::Halftone, set)])]);
        c.read_output().unwrap()
    };
    // Black ink (0x161616) and paper (0xf6f1e6): count the inked pixels.
    let inked = |img: &[u8]| img.chunks(4).filter(|p| p[0] < 128).count() as f32 / (w * h) as f32;
    let grey = draw(&mut c, &flat([128, 128, 128]), &[]);
    let f = inked(&grey);
    assert!((0.35..0.65).contains(&f), "mid grey half inked: {f}");
    // Only ink and paper (the dots' soft edges aside).
    let pure = grey.chunks(4).filter(|p| p[0] < 40 || p[0] > 230).count() as f32 / (w * h) as f32;
    assert!(pure > 0.7, "{pure}");
    assert!(inked(&draw(&mut c, &flat([250, 250, 250]), &[])) < 0.02);
    assert!(inked(&draw(&mut c, &flat([5, 5, 5]), &[])) > 0.95);
    // Lighter grey, less ink; square dots and lines work too.
    let light = inked(&draw(&mut c, &flat([200, 200, 200]), &[]));
    assert!(light < f - 0.15, "{light} vs {f}");
    for shape in [1.0, 2.0] {
        let s = inked(&draw(&mut c, &flat([128, 128, 128]), &[(halftone::SHAPE, shape)]));
        assert!((0.35..0.65).contains(&s), "shape {shape}: {s}");
    }
    // CMYK print of red: magenta and yellow dots, so red on average.
    let avg = |img: &[u8], ch: usize| img.chunks(4).map(|p| p[ch] as f32).sum::<f32>() / (w * h) as f32;
    let red = draw(&mut c, &flat([230, 20, 20]), &[(halftone::STYLE, 1.0)]);
    assert!(avg(&red, 0) > avg(&red, 1) + 60.0 && avg(&red, 0) > avg(&red, 2) + 40.0, "{} {} {}", avg(&red, 0), avg(&red, 1), avg(&red, 2));
    // Colour dots of blue: blue dots on paper.
    let blue = draw(&mut c, &flat([30, 40, 200]), &[(halftone::STYLE, 2.0)]);
    assert!(blue.chunks(4).any(|p| p[2] as i32 > p[0] as i32 + 100), "blue dots");
    assert!(blue.chunks(4).any(|p| p[0] > 230 && p[1] > 230), "paper between");
    // Mix 0: the picture as it was.
    let none = draw(&mut c, &flat([128, 128, 128]), &[(halftone::MIX, 0.0)]);
    assert!(near(px(&none, w, 80, 45), [128, 128, 128]));
}

#[test]
fn crt_curves_lines_and_snow() {
    use reelforge_core::effects::crt;
    let Some(mut c) = setup() else { return };
    let (w, h) = (160u32, 90u32);
    let grey: Vec<u8> = (0..w * h).flat_map(|_| [150u8, 150, 150, 255]).collect();
    let mut key = 2900u64;
    let mut draw = |c: &mut reelforge_render::Compositor, px: &[u8], set: &[(usize, f32)], t: f32| {
        key += 1;
        let mut v = fx(EffectKind::Crt, set);
        v.time = t;
        let mut l = layer(1, px, [w, h], [w as f32, h as f32], Transform::default(), BlendMode::Normal);
        l.content_id = key;
        c.render([w, h], &[with_fx(l, vec![v])]);
        c.read_output().unwrap()
    };
    // Curved glass: the corners are black, the middle is the picture.
    let curved = draw(&mut c, &grey, &[(crt::SCANLINES, 0.0), (crt::MASK, 0.0), (crt::NOISE, 0.0), (crt::VIGNETTE, 0.0), (crt::BRIGHTNESS, 100.0), (crt::CURVATURE, 100.0)], 1.0);
    assert!(px(&curved, w, 1, 1)[0] < 10, "{:?}", px(&curved, w, 1, 1));
    assert!(near(px(&curved, w, 80, 45), [150, 150, 150]), "{:?}", px(&curved, w, 80, 45));
    // Scan lines: down a column, light lines and dark gaps every 4 px.
    let lines = draw(&mut c, &grey, &[(crt::CURVATURE, 0.0), (crt::SCANLINES, 100.0), (crt::MASK, 0.0), (crt::NOISE, 0.0), (crt::WOBBLE, 0.0), (crt::VIGNETTE, 0.0), (crt::BRIGHTNESS, 100.0)], 1.0);
    let col: Vec<u8> = (30..46).map(|y| px(&lines, w, 80, y)[0]).collect();
    let (lo, hi) = (*col.iter().min().unwrap(), *col.iter().max().unwrap());
    assert!(hi > 130 && lo < 90, "{col:?}");
    // Phosphor mask: neighbouring columns lean red, green, blue.
    let mask = draw(&mut c, &grey, &[(crt::CURVATURE, 0.0), (crt::SCANLINES, 0.0), (crt::MASK, 100.0), (crt::NOISE, 0.0), (crt::WOBBLE, 0.0), (crt::BLEED, 0.0), (crt::VIGNETTE, 0.0), (crt::BRIGHTNESS, 100.0)], 1.0);
    let lean: Vec<usize> = (78..81).map(|x| { let p = px(&mask, w, x, 45); (0..3).max_by_key(|&i| p[i]).unwrap() }).collect();
    let mut sorted = lean.clone();
    sorted.sort();
    assert_eq!(sorted, vec![0, 1, 2], "{lean:?}");
    // Colour bleed: a red bar's colour smears to the right of it.
    let bar: Vec<u8> = (0..h).flat_map(|_| (0..w).flat_map(|x| if (60..70).contains(&x) { [220u8, 20, 20, 255] } else { [20, 20, 20, 255] })).collect();
    let flat = [(crt::CURVATURE, 0.0), (crt::SCANLINES, 0.0), (crt::MASK, 0.0), (crt::NOISE, 0.0), (crt::WOBBLE, 0.0), (crt::VIGNETTE, 0.0), (crt::BRIGHTNESS, 100.0)];
    let sharp = draw(&mut c, &bar, &[&flat[..], &[(crt::BLEED, 0.0)]].concat(), 1.0);
    let smear = draw(&mut c, &bar, &[&flat[..], &[(crt::BLEED, 12.0)]].concat(), 1.0);
    let redness = |p: [u8; 4]| p[0] as i32 - p[1] as i32;
    assert!(redness(px(&smear, w, 75, 45)) > redness(px(&sharp, w, 75, 45)) + 30, "{:?} {:?}", px(&smear, w, 75, 45), px(&sharp, w, 75, 45));
    // Snow and wobble change from frame to frame.
    let a = draw(&mut c, &grey, &[(crt::NOISE, 60.0)], 1.0);
    let b = draw(&mut c, &grey, &[(crt::NOISE, 60.0)], 1.04);
    let differ = a.chunks(4).zip(b.chunks(4)).filter(|(x, y)| (x[0] as i32 - y[0] as i32).abs() > 10).count();
    assert!(differ > (w * h / 10) as usize, "{differ}");
}

/// More than 8 shadow casters: 24 small cards in front of a wall, under a
/// slanting sun, each leave their own shadow (beside them, where nothing
/// covers the wall); at High shadow quality too.
#[test]
fn two_dozen_layers_cast_shadows() {
    use reelforge_core::space3d::{layer_space, Camera, Light, Scene3d};
    let Some(mut c) = setup() else { return };
    let (frame, f) = ([400u32, 200u32], [400.0f32, 200.0f32]);
    let cam = Camera::default_for(f);
    let white = solid(4, 4, [255, 255, 255, 255]);
    let red = solid(4, 4, [255, 0, 0, 255]);
    let wall = Transform { three_d: true, ..Default::default() };
    let cards: Vec<Transform> = (0..24).map(|n| Transform { three_d: true, position: [-150.0 + 60.0 * (n % 6) as f32, -60.0 + 40.0 * (n / 6) as f32], depth: -20.0, ..Default::default() }).collect();
    let render = |c: &mut reelforge_render::Compositor, shadow: f32| {
        let sun = Light { kind: 2, color: [1.0; 3], position: [0.0; 3], direction: [0.0, 0.8, 1.0], cone: [0.5, 0.6], falloff: 0, radius: 1000.0, shadow, softness: 0.0, env: None };
        let scene = Scene3d { camera: cam, lights: vec![sun] };
        let ws = layer_space(&wall, [400.0, 200.0], &scene);
        let cs: Vec<_> = cards.iter().map(|t| layer_space(t, [6.0, 6.0], &scene)).collect();
        let mut layers = vec![LayerInput { space: Some(&ws), ..layer(1, &white, [4, 4], [400.0, 200.0], wall, BlendMode::Normal) }];
        for (n, (t, s)) in cards.iter().zip(&cs).enumerate() {
            layers.push(LayerInput { space: Some(s), ..layer(10 + n as u64, &red, [4, 4], [6.0, 6.0], *t, BlendMode::Normal) });
        }
        c.render(frame, &layers);
        c.read_output().unwrap()
    };
    let lit = render(&mut c, 0.0);
    let check = |img: &[u8]| {
        let mut shaded = 0;
        for n in 0..24 {
            // Where the sun puts its shadow: 0.8 × 20 below it, on the wall.
            let (x, y) = ((200.0 - 150.0 + 60.0 * (n % 6) as f32) as u32, (100.0 - 60.0 + 40.0 * (n / 6) as f32 + 16.0) as u32);
            let (a, b) = (px(&lit, 400, x, y), px(img, 400, x, y));
            assert!(a[1] > 150, "the wall is uncovered there: {a:?} at {x},{y}");
            if (b[1] as i32) < a[1] as i32 - 60 {
                shaded += 1;
            }
        }
        shaded
    };
    assert_eq!(check(&render(&mut c, 1.0)), 24, "every card's shadow");
    reelforge_render::set_shadow_quality(2);
    let high = check(&render(&mut c, 1.0));
    reelforge_render::set_shadow_quality(1);
    assert_eq!(high, 24, "at High quality too");
}

/// Particles & simulation: a shatter's pieces fly from the impact and leave
/// holes; a muzzle flash shows only for its moment and lights the scene; a
/// bullet hit leaves its hole; a fluid pours in; atomic particles take the
/// layer's colours.
#[test]
fn simulations_draw_what_they_say() {
    use reelforge_core::effects::{bullet as bh, muzzle as mz, shatter as sh};
    use reelforge_core::particles as p;
    use reelforge_core::Time;
    let Some(mut c) = setup() else { return };
    let (w, h) = (160u32, 90u32);
    let grey = solid(w, h, [128, 128, 128, 255]);
    let red = solid(w, h, [220, 20, 20, 255]);
    let clear = solid(w, h, [0, 0, 0, 0]);
    let mut key = 500u64;
    let mut draw = |c: &mut reelforge_render::Compositor, pixels: &[u8], kind: EffectKind, set: &[(usize, f32)], t: f64| {
        let mut e = Effect::new(kind);
        for &(i, v) in set {
            e.params[i].value = v;
        }
        let v = e.eval_sized(Time::from_secs_f64(t), None, [w as f32, h as f32]);
        key += 1;
        let mut l = layer(1, pixels, [w, h], [w as f32, h as f32], Transform::default(), BlendMode::Normal);
        l.content_id = key;
        let effects = if v.is_identity() { Vec::new() } else { vec![v] };
        c.render([w, h], &[with_fx(l, effects)]);
        c.read_output().unwrap()
    };
    let lum = |p: [u8; 4]| p[0] as u32 + p[1] as u32 + p[2] as u32;
    let dark = |img: &[u8]| (0..h).flat_map(|y| (0..w).map(move |x| (x, y))).filter(|&(x, y)| lum(px(img, w, x, y)) < 30).count();

    // Shatter: whole before, holes (black background showing) after, more near the impact.
    let set = [(sh::AT, 0.5), (sh::X, 50.0), (sh::Y, 50.0), (sh::FORCE, 300.0), (sh::REACH, 60.0), (sh::GRAVITY, 0.0), (sh::FADE, 0.0)];
    let before = draw(&mut c, &grey, EffectKind::Shatter, &set, 0.2);
    assert_eq!(dark(&before), 0, "whole");
    let at_rest = draw(&mut c, &grey, EffectKind::Shatter, &set, 0.5001);
    assert!(dark(&at_rest) < 20, "the pieces fit together: {}", dark(&at_rest));
    let after = draw(&mut c, &grey, EffectKind::Shatter, &set, 0.8);
    assert!(dark(&after) > 200, "gaps open: {}", dark(&after));
    // The far corner (out of reach) is still in place.
    assert!(lum(px(&after, w, 2, 2)) > 300);

    // Muzzle flash: nothing before the shot, a bright flash at the barrel during it, gone after.
    let set = [(mz::X, 30.0), (mz::Y, 50.0), (mz::FIRST, 0.5)];
    let off = draw(&mut c, &grey, EffectKind::MuzzleFlash, &set, 0.3);
    assert!(lum(px(&off, w, 52, 45)) < 400, "no flash yet");
    let on = draw(&mut c, &grey, EffectKind::MuzzleFlash, &set, 0.51);
    let barrel = px(&on, w, 52, 45);
    assert!(lum(barrel) > 600, "{barrel:?}");
    // Lit round it, more near than far.
    assert!(lum(px(&on, w, 48, 70)) > lum(px(&off, w, 48, 70)) + 15);
    assert!(lum(px(&draw(&mut c, &grey, EffectKind::MuzzleFlash, &set, 0.7), w, 52, 45)) < 400, "over");
    // A burst: a flash for each shot.
    let burst = [(mz::X, 30.0), (mz::Y, 50.0), (mz::FIRST, 0.5), (mz::RATE, 10.0), (mz::SHOTS, 3.0)];
    assert!(lum(px(&draw(&mut c, &grey, EffectKind::MuzzleFlash, &burst, 0.71), w, 52, 45)) > 600, "third shot");
    assert!(lum(px(&draw(&mut c, &grey, EffectKind::MuzzleFlash, &burst, 0.91), w, 52, 45)) < 400, "only three");

    // Bullet hit: nothing before, a dark hole after (on every surface), long after too.
    let at = |s: f32| [(bh::X, 50.0), (bh::Y, 50.0), (bh::AT, 0.5), (bh::SURFACE, s), (bh::SIZE, 300.0), (bh::DUST, 0.0)];
    assert_eq!(dark(&draw(&mut c, &grey, EffectKind::BulletHit, &at(0.0), 0.2)), 0);
    for s in [0.0, 1.0, 2.0, 4.0] {
        let img = draw(&mut c, &grey, EffectKind::BulletHit, &at(s), 3.0);
        assert!(lum(px(&img, w, 80, 45)) < 150, "surface {s}: {:?}", px(&img, w, 80, 45));
    }
    // Glass: cracks catch the light (brighter lines round the hole).
    let glass = draw(&mut c, &grey, EffectKind::BulletHit, &at(3.0), 3.0);
    assert!((0..h).flat_map(|y| (0..w).map(move |x| (x, y))).any(|(x, y)| lum(px(&glass, w, x, y)) > 500));

    // Fluid (on the graphics card): on a clear layer, colour appears above
    // the source and rises; seeking back gives the same picture as before;
    // with obstacles on, a block in the way stays clear.
    {
        use reelforge_core::effects::fluid_params as fp;
        let blue = |img: &[u8], y0: u32, y1: u32| (y0..y1).flat_map(|y| (0..w).map(move |x| (x, y))).filter(|&(x, y)| px(img, w, x, y)[2] > 60).count();
        // (A source the size of a 1080p one, for this little layer.)
        let small = [(fp::SIZE, 8.0), (fp::FORCE, 40.0)];
        let early = draw(&mut c, &clear, EffectKind::Fluid, &small, 0.7);
        let later = draw(&mut c, &clear, EffectKind::Fluid, &small, 2.0);
        assert!(blue(&later, 0, h) > blue(&early, 0, h) && blue(&early, 0, h) > 20, "{} {}", blue(&early, 0, h), blue(&later, 0, h));
        assert!(blue(&later, 0, 45) > blue(&early, 0, 45), "rises");
        let again = draw(&mut c, &clear, EffectKind::Fluid, &small, 0.7);
        let diff = early.iter().zip(&again).filter(|(a, b)| a.abs_diff(**b) > 2).count();
        assert!(diff < 20, "seeking back: {diff}");
        // A block above the source, the fluid flowing round it.
        let block: Vec<u8> = (0..h).flat_map(|y| (0..w).flat_map(move |x| if (60..100).contains(&x) && (35..55).contains(&y) { [255u8, 255, 255, 255] } else { [0, 0, 0, 0] })).collect();
        let img = draw(&mut c, &block, EffectKind::Fluid, &[(fp::SIZE, 8.0), (fp::FORCE, 40.0), (fp::OBSTACLES, 1.0)], 2.5);
        let inside = (40..50).flat_map(|y| (65..95).map(move |x| (x, y))).filter(|&(x, y)| { let q = px(&img, w, x, y); q[2] > 60 && q[0] < 200 }).count();
        assert_eq!(inside, 0, "nothing inside the block");
        assert!(blue(&img, 0, h) > 40);
    }

    // Disintegrate, left to right: halfway, the left half gone (bar a few
    // particles), the right half still there, and glowing particles at the front.
    {
        use reelforge_core::effects::disintegrate as d;
        let set = [(d::START, 0.0), (d::DURATION, 2.0), (d::ROUGH, 0.0), (d::SPEED, 0.0), (d::GRAVITY, 0.0), (d::TURBULENCE, 0.0), (d::COUNT, 3000.0), (d::LOOK, 3.0), (d::LIFETIME, 0.3)];
        let img = draw(&mut c, &grey, EffectKind::Disintegrate, &set, 1.0);
        let lit = |x0: u32, x1: u32| (0..h).flat_map(|y| (x0..x1).map(move |x| (x, y))).filter(|&(x, y)| lum(px(&img, w, x, y)) > 150).count();
        assert!(lit(0, 50) < 50 * 90 / 4, "left mostly gone: {}", lit(0, 50));
        assert!(lit(100, 160) == 60 * 90, "right still there: {}", lit(100, 160));
        assert!(lit(60, 80) > 20, "particles at the front");
    }

    // Collisions: thrown down onto a floor at 60%, nothing gets below it.
    {
        let clear = solid(w, h, [0, 0, 0, 0]);
        let fall = [(p::EMIT_X, 50.0), (p::EMIT_Y, 10.0), (p::SHAPE, p::SHAPE_LINE as f32), (p::EMIT_SIZE, 120.0), (p::DIRECTION, -90.0), (p::SPREAD, 10.0), (p::SPEED, 60.0), (p::GRAVITY, 300.0), (p::RATE, 300.0), (p::LIFETIME, 3.0), (p::SIZE_START, 2.0), (p::SIZE_END, 2.0), (p::LOOK, 3.0), (p::COLOR_START, 0xffffff as f32), (p::COLOR_END, 0xffffff as f32)];
        let bright_rows = |img: &[u8], y0: u32, y1: u32| (y0..y1).flat_map(|y| (0..w).map(move |x| (x, y))).filter(|&(x, y)| lum(px(img, w, x, y)) > 200).count();
        let free = draw(&mut c, &clear, EffectKind::Particles, &fall, 2.0);
        assert!(bright_rows(&free, 60, 90) > 20, "without a floor they fall through");
        let mut floor = fall.to_vec();
        floor.extend([(p::COLLIDE, p::COLLIDE_FLOOR as f32), (p::FLOOR, 60.0), (p::BOUNCE, 20.0)]);
        let img = draw(&mut c, &clear, EffectKind::Particles, &floor, 2.0);
        assert_eq!(bright_rows(&img, 58, 90), 0, "none below the floor");
        assert!(bright_rows(&img, 50, 54) > 10, "piled on it: {}", bright_rows(&img, 50, 54));

        // The layer's shape: a block in the lower middle catches them on its top edge.
        let block: Vec<u8> = (0..h).flat_map(|y| (0..w).flat_map(move |x| if (50..110).contains(&x) && y >= 50 { [60u8, 60, 60, 255] } else { [0, 0, 0, 0] })).collect();
        let mut on_shape = fall.to_vec();
        on_shape.extend([(p::COLLIDE, p::COLLIDE_SHAPE as f32), (p::BOUNCE, 0.0)]);
        let img = draw(&mut c, &block, EffectKind::Particles, &on_shape, 2.0);
        let inside = (55..90).flat_map(|y| (55..105).map(move |x| (x, y))).filter(|&(x, y)| lum(px(&img, w, x, y)) > 200).count();
        assert!(inside < 5, "kept out of the block: {inside}");
        let on_top = (40..50).flat_map(|y| (55..105).map(move |x| (x, y))).filter(|&(x, y)| lum(px(&img, w, x, y)) > 200).count();
        assert!(on_top > 10, "resting on it: {on_top}");

        // Born inside the shape only, in the emitter's colour (white).
        let inside_only = [(p::SHAPE, p::SHAPE_INSIDE as f32), (p::RATE, 3000.0), (p::SPEED, 0.0), (p::GRAVITY, 0.0), (p::LIFETIME, 1.0), (p::SIZE_START, 2.0), (p::SIZE_END, 2.0), (p::LOOK, 3.0), (p::COLOR_START, 0xffffff as f32), (p::COLOR_END, 0xffffff as f32)];
        let img = draw(&mut c, &block, EffectKind::Particles, &inside_only, 0.5);
        let out_of = (0..h).flat_map(|y| (0..w).map(move |x| (x, y))).filter(|&(x, y)| !((48..112).contains(&x) && y >= 48) && lum(px(&img, w, x, y)) > 200).count();
        assert_eq!(out_of, 0);
        assert!((50..90).flat_map(|y| (50..110).map(move |x| (x, y))).filter(|&(x, y)| lum(px(&img, w, x, y)) > 200).count() > 100);
    }

    // Atomic particles: born from a red layer's pixels, they're red; from a clear one, none.
    let atomic = [(p::SHAPE, p::SHAPE_PIXELS_ONLY as f32), (p::RATE, 2000.0), (p::LIFETIME, 1.0), (p::SPEED, 0.0), (p::GRAVITY, 0.0), (p::SIZE_START, 4.0), (p::SIZE_END, 4.0), (p::LOOK, 3.0), (p::COLOR_END, 0xff0000 as f32)];
    let img = draw(&mut c, &red, EffectKind::Particles, &atomic, 0.5);
    let reds = (0..h).flat_map(|y| (0..w).map(move |x| (x, y))).filter(|&(x, y)| { let q = px(&img, w, x, y); q[0] > 120 && q[1] < 60 }).count();
    assert!(reds > 300, "{reds}");
    let none = draw(&mut c, &clear, EffectKind::Particles, &atomic, 0.5);
    assert!(none.chunks_exact(4).all(|q| q[0] < 8 && q[1] < 8 && q[2] < 8), "nothing from a clear layer");
}

/// Fractal noise: a pattern across the layer that's the same at the same
/// moment, evolves over time, and can cut the layer out.
#[test]
fn fractal_noise_patterns_evolve_and_cut_out() {
    use reelforge_core::effects::fractal as f;
    use reelforge_core::Time;
    let Some(mut c) = setup() else { return };
    let (w, h) = (160u32, 90u32);
    let grey = solid(w, h, [128, 128, 128, 255]);
    let mut key = 7000u64;
    let mut draw = |c: &mut reelforge_render::Compositor, set: &[(usize, f32)], t: f64| {
        let mut e = Effect::new(EffectKind::FractalNoise);
        e.params[f::SCALE].value = 40.0;
        for &(i, v) in set {
            e.params[i].value = v;
        }
        key += 1;
        let mut l = layer(1, &grey, [w, h], [w as f32, h as f32], Transform::default(), BlendMode::Normal);
        l.content_id = key;
        c.render([w, h], &[with_fx(l, vec![e.eval(Time::from_secs_f64(t))])]);
        c.read_output().unwrap()
    };
    let spread = |img: &[u8]| {
        let v: Vec<u8> = img.chunks(4).map(|p| p[0]).collect();
        (*v.iter().min().unwrap(), *v.iter().max().unwrap())
    };
    for ty in 0..6 {
        let img = draw(&mut c, &[(f::TYPE, ty as f32)], 0.0);
        let (lo, hi) = spread(&img);
        assert!(hi - lo > 80, "type {ty}: {lo}..{hi}");
        assert_eq!(img, draw(&mut c, &[(f::TYPE, ty as f32)], 0.0), "type {ty} is the same each time");
    }
    let a = draw(&mut c, &[], 0.0);
    let b = draw(&mut c, &[], 2.0);
    assert!(a.iter().zip(&b).filter(|(x, y)| x.abs_diff(**y) > 10).count() > 2000, "evolves");
    let still = draw(&mut c, &[(f::SPEED, 0.0)], 2.0);
    assert_eq!(a, still, "no speed, no change");
    // Drawn over black, "as transparency" leaves the grey only where the noise is light.
    let cut = draw(&mut c, &[(f::BLEND, 5.0), (f::CONTRAST, 400.0)], 0.0);
    let dark = cut.chunks(4).filter(|p| p[0] < 10).count();
    let kept = cut.chunks(4).filter(|p| (120..136).contains(&p[0])).count();
    assert!(dark > 500 && kept > 500, "{dark} {kept}");
}

/// Lightning: a bright bolt from the top to the bottom, dark far from it,
/// a new bolt with each strike; nothing with no opacity.
#[test]
fn lightning_strikes_between_its_points() {
    use reelforge_core::effects::lightning as lt;
    use reelforge_core::Time;
    let Some(mut c) = setup() else { return };
    let (w, h) = (160u32, 90u32);
    let dark = solid(w, h, [10, 10, 20, 255]);
    let mut key = 7100u64;
    let mut draw = |c: &mut reelforge_render::Compositor, set: &[(usize, f32)], t: f64| {
        let mut e = Effect::new(EffectKind::Lightning);
        e.params[lt::GLOW].value = 3.0;
        e.params[lt::FLICKER].value = 0.0;
        for &(i, v) in set {
            e.params[i].value = v;
        }
        let v = e.eval_sized(Time::from_secs_f64(t), None, [w as f32, h as f32]);
        key += 1;
        let mut l = layer(1, &dark, [w, h], [w as f32, h as f32], Transform::default(), BlendMode::Normal);
        l.content_id = key;
        let fx = if v.is_identity() { Vec::new() } else { vec![v] };
        c.render([w, h], &[with_fx(l, fx)]);
        c.read_output().unwrap()
    };
    let img = draw(&mut c, &[(lt::BRANCHES, 0.0)], 0.0);
    // Every row in the middle has a white-hot pixel near the centre line.
    for y in 10..80 {
        assert!((50..110).any(|x| px(&img, w, x, y)[0] > 200), "row {y}");
    }
    // The sides stay dark.
    assert!(px(&img, w, 2, 45)[0] < 40 && px(&img, w, 157, 45)[0] < 40, "{:?}", px(&img, w, 2, 45));
    assert_eq!(img, draw(&mut c, &[(lt::BRANCHES, 0.0)], 0.0), "the same moment, the same bolt");
    assert_ne!(img, draw(&mut c, &[(lt::BRANCHES, 0.0)], 0.55), "a new strike");
    assert_eq!(draw(&mut c, &[(lt::OPACITY, 0.0)], 0.0), draw(&mut c, &[(lt::OPACITY, 0.0)], 0.3));
}
