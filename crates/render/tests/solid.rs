//! 3D text drawn by the compositor on a headless GPU (skipped without an adapter).

use reelforge_core::space3d::{solid_space, Camera, Scene3d};
use reelforge_core::{BlendMode, TitleSpec, Transform};
use reelforge_render::text3d::text_mesh;
use reelforge_render::{headless_device, Compositor, LayerInput, PixelData, SolidInput};

const W: u32 = 480;
const H: u32 = 270;

fn spec(extrude: f32) -> TitleSpec {
    TitleSpec { text: "HI".into(), size: 120.0, color: [230, 40, 30, 255], shadow: false, extrude, side_color: Some([40, 90, 230, 255]), ..TitleSpec::default() }
}

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

fn scene() -> Scene3d {
    Scene3d { camera: Camera::default_for([W as f32, H as f32]), lights: Vec::new() }
}

/// Mostly red (the face), mostly blue (a side), or the black background.
fn classify(p: &[u8]) -> u8 {
    let (r, g, b) = (p[0] as i32, p[1] as i32, p[2] as i32);
    if r + g + b < 30 {
        0
    } else if r > b + 40 {
        1
    } else if b > r + 40 {
        2
    } else {
        3
    }
}

#[test]
fn front_on_it_covers_the_flat_titles_letters() {
    let Some((d, q)) = headless_device() else { return };
    let mut c = Compositor::new(&d, &q);
    let img = render(&mut c, &spec(60.0), Transform { three_d: true, ..Default::default() }, &scene());
    let flat = reelforge_render::text::render_title(&TitleSpec { extrude: 0.0, ..spec(0.0) }, [W, H]);
    // Where the flat title is solid, the 3D text's front face shows in the
    // text colour (a face turned to the camera is fully lit); where the flat
    // title is empty, there's at most a sliver of side (perspective).
    let (mut both, mut flat_only, mut solid_only, mut sides) = (0, 0, 0, 0);
    for (i, (a, b)) in img.chunks_exact(4).zip(flat.rgba.chunks_exact(4)).enumerate() {
        let k = classify(a);
        if k == 2 {
            sides += 1;
        }
        match (b[3] > 250, k != 0) {
            (true, true) => {
                both += 1;
                if k == 1 && b[3] == 255 && i % 7 == 0 {
                    assert!((a[0] as i32 - 230).abs() <= 3 && (a[1] as i32 - 40).abs() <= 3, "face colour {a:?}");
                }
            }
            (true, false) => flat_only += 1,
            (false, true) if b[3] == 0 => solid_only += 1,
            _ => {}
        }
    }
    assert!(both > 1000, "{both}");
    assert!(flat_only < both / 50, "front face missing: {flat_only} of {both}");
    // Seen straight on, a little of the sides shows towards the middle of the frame.
    assert!(sides > 0 && solid_only < both / 2, "{sides} {solid_only} {both}");
}

#[test]
fn turned_it_shows_its_sides_shaded() {
    let Some((d, q)) = headless_device() else { return };
    let mut c = Compositor::new(&d, &q);
    let turned = Transform { three_d: true, orientation: [25.0, 35.0], ..Default::default() };
    let deep = render(&mut c, &spec(80.0), turned, &scene());
    let blue = deep.chunks_exact(4).filter(|p| classify(p) == 2).count();
    let red = deep.chunks_exact(4).filter(|p| classify(p) == 1).count();
    assert!(blue > 1000 && red > 1000, "sides {blue}, faces {red}");
    // The studio light shades the sides (turned away from the camera, they get less than full light).
    let mut shades: Vec<u8> = deep.chunks_exact(4).filter(|p| classify(p) == 2 && p[3] == 255).map(|p| p[2]).collect();
    shades.sort_unstable();
    let median = shades[shades.len() / 2];
    assert!(median < 228 && median > 120, "{median}");
    // Flat text turned the same way has no sides at all.
    let flat = render(&mut c, &TitleSpec { extrude: 0.0, ..spec(0.0) }, turned, &scene());
    assert_eq!(flat.chunks_exact(4).filter(|p| classify(p) == 2).count(), 0);
    // Deeper text from a different angle is a different picture (the layer was redrawn).
    let other = render(&mut c, &spec(80.0), Transform { orientation: [-25.0, 35.0], ..turned }, &scene());
    assert_ne!(other, deep);
}

/// 3D text casts shadows: extruded letters between a point light and a lit
/// wall darken the wall in their shape (not their box), and with the light's
/// shadow off the wall stays fully lit.
#[test]
fn it_casts_shadows_on_3d_layers() {
    use reelforge_core::space3d::{layer_space, Light};
    let Some((d, q)) = headless_device() else { return };
    let mut c = Compositor::new(&d, &q);
    let white: Vec<u8> = [255u8; 4].iter().copied().cycle().take(16 * 4).collect();
    let wall = Transform { three_d: true, ..Default::default() };
    let text = Transform { three_d: true, depth: -300.0, ..Default::default() };
    let s = spec(30.0);
    let mesh = text_mesh(&s, [W, H]);
    let draw = |c: &mut Compositor, shadow: f32, with_wall: bool| {
        let light = Light { kind: 0, color: [1.0; 3], position: [0.0, 0.0, -600.0], direction: [0.0, 0.0, 1.0], cone: [0.5, 0.6], falloff: 0, radius: 2000.0, shadow, softness: 0.0 };
        let sc = Scene3d { camera: Camera::default_for([W as f32, H as f32]), lights: vec![light] };
        let input = SolidInput { mesh: mesh.clone(), space: solid_space(&text, &sc), face: s.color, side: s.side_color.unwrap() };
        let ws = layer_space(&wall, [W as f32, H as f32], &sc);
        let wall_layer = LayerInput { key: 2, content_id: 1, pixels: PixelData::Rgba(&white), pixel_size: [4, 4], layer_size: [W as f32, H as f32], transform: wall, blend: BlendMode::Normal, effects: Vec::new(), mask: None, wipe: None, mesh: None, space: Some(&ws) };
        let text_layer = LayerInput { key: 1, content_id: input.content_id(), pixels: PixelData::Solid(&input), pixel_size: [W, H], layer_size: [W as f32, H as f32], transform: Transform::default(), blend: BlendMode::Normal, effects: Vec::new(), mask: None, wipe: None, mesh: None, space: None };
        if with_wall {
            c.render([W, H], &[wall_layer, text_layer]);
        } else {
            c.render([W, H], &[text_layer]);
        }
        c.read_output().unwrap()
    };
    let alone = draw(&mut c, 0.8, false);
    let lit = draw(&mut c, 0.0, true);
    let shadowed = draw(&mut c, 0.8, true);
    // Away from the letters (and a little margin round them), how the wall changed.
    let covered = |x: u32, y: u32| {
        (x.saturating_sub(3)..(x + 4).min(W)).any(|x| (y.saturating_sub(3)..(y + 4).min(H)).any(|y| { let i = ((y * W + x) * 4) as usize; alone[i] as u32 + alone[i + 1] as u32 + alone[i + 2] as u32 > 6 }))
    };
    let (mut dark, mut wall_px) = (0, 0);
    let (mut x0, mut x1, mut y0, mut y1) = (W, 0, H, 0);
    for y in 0..H {
        for x in 0..W {
            if covered(x, y) {
                continue;
            }
            let i = ((y * W + x) * 4) as usize;
            assert!(lit[i] > 200, "no shadow: lit at {x},{y}: {}", lit[i]);
            wall_px += 1;
            if (shadowed[i] as i32) < lit[i] as i32 - 60 {
                dark += 1;
                (x0, x1, y0, y1) = (x0.min(x), x1.max(x), y0.min(y), y1.max(y));
            }
        }
    }
    assert!(dark > 400, "the letters' shadow shows round them: {dark} of {wall_px}");
    // Letter-shaped: plenty of the shadow's box (between the letters) stays lit.
    let mut lit_inside = 0;
    for y in y0..=y1 {
        for x in x0..=x1 {
            let i = ((y * W + x) * 4) as usize;
            if !covered(x, y) && (shadowed[i] as i32) >= lit[i] as i32 - 20 {
                lit_inside += 1;
            }
        }
    }
    assert!(lit_inside > 200, "not a box: {lit_inside} lit inside {x0}..{x1} × {y0}..{y1}");
}

/// 3D text and 3D layers cut through each other: text turned 45° through a
/// white wall facing the camera (drawn after it) shows on the side where it's
/// in front of the wall and is hidden on the other; and a layer simply behind
/// the text doesn't cover it.
#[test]
fn it_cuts_through_3d_layers() {
    use reelforge_core::space3d::layer_space;
    let Some((d, q)) = headless_device() else { return };
    let mut c = Compositor::new(&d, &q);
    let white: Vec<u8> = [255u8; 4].iter().copied().cycle().take(16 * 4).collect();
    let s = spec(30.0);
    let sc = scene();
    let draw = |c: &mut Compositor, text: Transform, wall: Transform| {
        let input = SolidInput { mesh: text_mesh(&s, [W, H]), space: solid_space(&text, &sc), face: s.color, side: s.side_color.unwrap() };
        let ws = layer_space(&wall, [W as f32, H as f32], &sc);
        let wall_layer = LayerInput { key: 2, content_id: 1, pixels: PixelData::Rgba(&white), pixel_size: [4, 4], layer_size: [W as f32, H as f32], transform: wall, blend: BlendMode::Normal, effects: Vec::new(), mask: None, wipe: None, mesh: None, space: Some(&ws) };
        let text_layer = LayerInput { key: 1, content_id: input.content_id(), pixels: PixelData::Solid(&input), pixel_size: [W, H], layer_size: [W as f32, H as f32], transform: Transform::default(), blend: BlendMode::Normal, effects: Vec::new(), mask: None, wipe: None, mesh: None, space: None };
        c.render([W, H], &[text_layer, wall_layer]);
        c.read_output().unwrap()
    };
    // Coloured (the text's red or blue) pixels in the left and right halves.
    let count = |img: &[u8]| {
        let mut n = [0u32; 2];
        for y in 0..H {
            for x in 0..W {
                let k = classify(&img[((y * W + x) * 4) as usize..]);
                if k == 1 || k == 2 {
                    n[(x >= W / 2) as usize] += 1;
                }
            }
        }
        n
    };
    let wall = Transform { three_d: true, ..Default::default() };
    let mut turned = Transform { three_d: true, ..Default::default() };
    turned.orientation = [0.0, 45.0];
    let [l, r] = count(&draw(&mut c, turned, wall));
    assert!((l > 1500 && r < 50) || (r > 1500 && l < 50), "one side in front: left {l}, right {r}");
    // Text in front of a wall that's further away: all of it shows, though the wall is drawn later.
    let far = Transform { three_d: true, depth: 200.0, ..Default::default() };
    let front = count(&draw(&mut c, Transform { three_d: true, ..Default::default() }, far));
    assert!(front[0] > 800 && front[1] > 800, "in front: {front:?}");
    // And behind a nearer wall: none of it.
    let near = Transform { three_d: true, depth: -100.0, ..Default::default() };
    let behind = count(&draw(&mut c, Transform { three_d: true, ..Default::default() }, near));
    assert!(behind[0] + behind[1] < 20, "behind: {behind:?}");
}
