//! Colour bit depth (Settings → Renders): how precisely frames are composited, and
//! whether layers blend in linear light or as display values.

use reelforge_core::frame::{f16_to_f32, f32_to_f16, FloatPixels};
use reelforge_core::{BlendMode, Effect, EffectKind, Transform};
use reelforge_render::{headless_device, ColorDepth, Compositor, DrawItem, LayerInput, PixelData};

fn solid(rgba: [u8; 4]) -> Vec<u8> {
    rgba.to_vec()
}

fn layer(key: u64, pixels: &[u8], t: Transform) -> LayerInput<'_> {
    LayerInput { key, content_id: key, pixels: PixelData::Rgba(pixels), pixel_size: [1, 1], layer_size: [16.0, 16.0], transform: t, blend: BlendMode::Normal, effects: Vec::new(), mask: None, wipe: None, mesh: None, space: None }
}

fn at(img: &[u8], x: u32, y: u32) -> [u8; 3] {
    let i = ((y * 16 + x) * 4) as usize;
    [img[i], img[i + 1], img[i + 2]]
}

fn near(v: u8, want: u8, tol: u8) -> bool {
    v.abs_diff(want) <= tol
}

#[test]
fn colour_bit_depths_blend_as_they_say() {
    let Some((d, q)) = headless_device() else { return };
    let black = solid([0, 0, 0, 255]);
    let white = solid([255, 255, 255, 255]);
    let red = solid([255, 0, 0, 255]);
    let blue = solid([0, 0, 255, 255]);
    for depth in ColorDepth::ALL {
        let mut c = Compositor::with_depth(&d, &q, depth);
        let got = c.depth();
        // 32-bit float only where the graphics card can; else the nearest 16-bit.
        assert!(got == depth || got == depth.at_most_half(), "{depth:?} worked at {got:?}");
        eprintln!("{depth:?} works at {got:?}");
        let linear = got.linear();
        // Mid-grey in display values (128), or as half the light (188).
        let half = if linear { 188 } else { 128 };

        // White at half opacity over black.
        c.render([16, 16], &[layer(1, &black, Transform::default()), layer(2, &white, Transform { opacity: 0.5, ..Default::default() })]);
        let p = at(&c.read_output().unwrap(), 8, 8);
        assert!(p.iter().all(|&v| near(v, half, 3)), "{depth:?}: half white over black is {p:?}, wanted {half}");

        // A dissolve halfway from red to blue.
        c.render_items([16, 16], &[DrawItem::Dissolve { dip: None, from: layer(3, &red, Transform::default()), to: layer(4, &blue, Transform::default()), mix: 0.5 }]);
        let p = at(&c.read_output().unwrap(), 8, 8);
        assert!(near(p[0], half, 3) && near(p[2], half, 3) && p[1] < 4, "{depth:?}: the dissolve's middle is {p:?}");

        // An adjustment layer a stop down over white: half the light whichever way the
        // frame is held (decoded for the effect, encoded back).
        let mut adj = LayerInput { key: 5, content_id: 5, pixels: PixelData::Below, pixel_size: [16, 16], layer_size: [16.0, 16.0], transform: Transform::default(), blend: BlendMode::Normal, effects: Vec::new(), mask: None, wipe: None, mesh: None, space: None };
        let mut e = Effect::new(EffectKind::ColorCorrect);
        e.params[0].value = -1.0;
        adj.effects = vec![e.eval(reelforge_core::Time::ZERO)];
        c.render([16, 16], &[layer(6, &white, Transform::default()), adj]);
        let p = at(&c.read_output().unwrap(), 8, 8);
        assert!(p.iter().all(|&v| near(v, 188, 4)), "{depth:?}: a stop down from white is {p:?}");

        // Highlights: a float layer at twice white reads back as it was, but 8-bit clips.
        let hot = FloatPixels { width: 1, height: 1, rgba: [2.0, 2.0, 2.0, 1.0].map(f32_to_f16).to_vec() };
        let l = LayerInput { key: 7, content_id: 7, pixels: PixelData::Float(&hot), pixel_size: [1, 1], layer_size: [16.0, 16.0], transform: Transform::default(), blend: BlendMode::Normal, effects: Vec::new(), mask: None, wipe: None, mesh: None, space: None };
        c.render([16, 16], &[l]);
        let f = c.read_output_linear().unwrap();
        let v = f16_to_f32(f.rgba[(8 * 16 + 8) * 4]);
        let want = if got.bits() == 8 { 1.0 } else { 2.0 };
        assert!((v - want).abs() < 0.05, "{depth:?}: a highlight at 2.0 reads back {v}");
    }
}

#[test]
fn the_default_depth_is_what_new_compositors_use() {
    let Some((d, q)) = headless_device() else { return };
    assert_eq!(reelforge_render::default_depth(), ColorDepth::HalfLinear);
    reelforge_render::set_default_depth(ColorDepth::Int8);
    assert_eq!(Compositor::new(&d, &q).depth(), ColorDepth::Int8);
    reelforge_render::set_default_depth(ColorDepth::HalfLinear);
    assert_eq!(Compositor::new(&d, &q).depth(), ColorDepth::HalfLinear);
}
