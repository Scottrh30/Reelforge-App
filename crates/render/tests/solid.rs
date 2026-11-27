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
        let light = Light { kind: 0, color: [1.0; 3], position: [0.0, 0.0, -600.0], direction: [0.0, 0.0, 1.0], cone: [0.5, 0.6], falloff: 0, radius: 2000.0, shadow, softness: 0.0, env: None };
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

/// 3D text takes shadows: a card between a light and the letters darkens the
/// part of the face behind it (the rest stays lit), and with the light's
/// shadow off the face is evenly lit.
#[test]
fn it_takes_shadows_from_3d_layers() {
    use reelforge_core::space3d::{layer_space, Light};
    let Some((d, q)) = headless_device() else { return };
    let mut c = Compositor::new(&d, &q);
    let white: Vec<u8> = [255u8; 4].iter().copied().cycle().take(16 * 4).collect();
    let s = spec(30.0);
    let mesh = text_mesh(&s, [W, H]);
    let text = Transform { three_d: true, ..Default::default() };
    // A card halfway between the light (off to the right, in front) and the text.
    let card = Transform { three_d: true, position: [200.0, 0.0], depth: -200.0, ..Default::default() };
    let draw = |c: &mut Compositor, shadow: f32| {
        let light = Light { kind: 0, color: [1.0; 3], position: [400.0, 0.0, -400.0], direction: [0.0, 0.0, 1.0], cone: [0.5, 0.6], falloff: 0, radius: 2000.0, shadow, softness: 0.0, env: None };
        let sc = Scene3d { camera: Camera::default_for([W as f32, H as f32]), lights: vec![light] };
        let input = SolidInput { mesh: mesh.clone(), space: solid_space(&text, &sc), face: s.color, side: s.side_color.unwrap() };
        let cs = layer_space(&card, [24.0, 24.0], &sc);
        let card_layer = LayerInput { key: 2, content_id: 1, pixels: PixelData::Rgba(&white), pixel_size: [4, 4], layer_size: [24.0, 24.0], transform: card, blend: BlendMode::Normal, effects: Vec::new(), mask: None, wipe: None, mesh: None, space: Some(&cs) };
        let text_layer = LayerInput { key: 1, content_id: input.content_id(), pixels: PixelData::Solid(&input), pixel_size: [W, H], layer_size: [W as f32, H as f32], transform: Transform::default(), blend: BlendMode::Normal, effects: Vec::new(), mask: None, wipe: None, mesh: None, space: None };
        c.render([W, H], &[text_layer, card_layer]);
        c.read_output().unwrap()
    };
    let lit = draw(&mut c, 0.0);
    let shadowed = draw(&mut c, 1.0);
    let (mut face, mut dark) = (0, 0);
    for i in (0..(W * H) as usize).map(|i| i * 4) {
        if classify(&lit[i..]) == 1 {
            face += 1;
            if (shadowed[i] as i32) < lit[i] as i32 - 40 {
                dark += 1;
            }
        }
    }
    assert!(face > 1000, "the face shows: {face}");
    assert!(dark > 150 && dark < face * 3 / 4, "part of the face in shadow: {dark} of {face}");
    // Lit again: no shadow left over from the frame before.
    let again = draw(&mut c, 0.0);
    assert_eq!(again, lit);
}

/// A shape instead of text: a square frame (an outer square with a square
/// hole, even-odd) shows its frame and not its hole, flat and extruded, and
/// with non-zero filling and the hole wound the same way, no hole.
#[test]
fn shapes_fill_by_their_rule_flat_and_extruded() {
    use reelforge_core::{Shape, ShapePath};
    use std::sync::Arc;
    let sq = |a: f32, b: f32| vec![[a, a], [b, a], [b, b], [a, b]];
    let frame = |even_odd| Arc::new(Shape::new("frame", vec![ShapePath { contours: vec![sq(0.0, 100.0), sq(30.0, 70.0)], even_odd }]).unwrap());
    let spec = |shape, extrude| TitleSpec { size: 120.0, color: [230, 40, 30, 255], shadow: false, extrude, side_color: Some([40, 90, 230, 255]), shape: Some(shape), ..TitleSpec::default() };
    let at = |img: &[u8], x: u32, y: u32| classify(&img[((y * W + x) * 4) as usize..]);
    let (cx, cy) = (W / 2, H / 2);
    // Flat: 120 px high, centred. The hole is 36 px across, the frame 42 px wide each side.
    let flat = reelforge_render::text::render_title(&spec(frame(true), 0.0), [W, H]);
    let px = |x: u32, y: u32| flat.rgba[((y * W + x) * 4 + 3) as usize];
    assert_eq!(px(cx, cy), 0, "the hole");
    assert_eq!(px(cx - 45, cy), 255, "the frame");
    assert_eq!(px(cx - 65, cy), 0, "outside");
    let b = flat.bounds.unwrap();
    assert!((b[2] - b[0] - 120.0).abs() < 1.5 && (b[3] - b[1] - 120.0).abs() < 1.5, "{b:?}");
    let solid_flat = reelforge_render::text::render_title(&spec(frame(false), 0.0), [W, H]);
    assert_eq!(solid_flat.rgba[((cy * W + cx) * 4 + 3) as usize], 255, "non-zero, same winding: filled");
    // Extruded, front on: the face colour round a hole.
    let Some((d, q)) = headless_device() else { return };
    let mut c = Compositor::new(&d, &q);
    let sc = scene();
    let t = Transform { three_d: true, ..Default::default() };
    let img = render(&mut c, &spec(frame(true), 30.0), t, &sc);
    assert_eq!(at(&img, cx, cy), 0, "the hole");
    assert_eq!(at(&img, cx - 45, cy), 1, "the frame's face");
    // Turned: its sides show, inside the hole too.
    let mut turned = t;
    turned.orientation = [0.0, 40.0];
    let img = render(&mut c, &spec(frame(true), 30.0), turned, &sc);
    assert!((0..W).any(|x| at(&img, x, cy) == 2), "sides");
}

/// A 3D model (an OBJ cube with a green material) drawn like 3D text: in its
/// own colour, as high as `size`, turned to show three lit faces of different
/// brightness; in the title colour with its own colours off; nothing for a
/// file that isn't there.
#[test]
fn models_draw_in_their_own_colours() {
    use reelforge_core::ModelSource;
    let d = std::env::temp_dir().join(format!("rf_model_render_{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(
        d.join("cube.obj"),
        "mtllib cube.mtl\nusemtl g\nv -1 -1 -1\nv 1 -1 -1\nv 1 1 -1\nv -1 1 -1\nv -1 -1 1\nv 1 -1 1\nv 1 1 1\nv -1 1 1\nf 1 4 3 2\nf 5 6 7 8\nf 1 2 6 5\nf 2 3 7 6\nf 3 4 8 7\nf 4 1 5 8\n",
    )
    .unwrap();
    std::fs::write(d.join("cube.mtl"), "newmtl g\nKd 0.1 0.8 0.1\n").unwrap();
    let src = ModelSource::new(d.join("cube.obj"));
    let spec = |own: bool| TitleSpec { size: 100.0, color: [230, 40, 30, 255], shadow: false, model: Some(ModelSource { own_colours: own, ..src.clone() }), ..TitleSpec::default() };
    // The mesh: its largest side 100 px, centred every way.
    let m = text_mesh(&spec(true), [W, H]);
    assert_eq!(m.indices.len(), 36);
    assert!((m.bounds[3] - m.bounds[1] - 100.0).abs() < 1e-3 && (m.bounds[0] + m.bounds[2]).abs() < 1e-3, "{:?}", m.bounds);
    assert!((m.depth - 100.0).abs() < 1e-3 && m.mid_z == 0.0);
    assert!(m.vertices.iter().all(|v| v.pos[2].abs() <= 50.0 + 1e-3));
    let Some((dev, q)) = headless_device() else { return };
    let mut c = Compositor::new(&dev, &q);
    let sc = scene();
    let mut t = Transform { three_d: true, ..Default::default() };
    t.orientation = [-20.0, 30.0];
    let img = render(&mut c, &spec(true), t, &sc);
    let greens: Vec<u8> = (0..(W * H) as usize).map(|i| &img[i * 4..i * 4 + 4]).filter(|p| p[1] as i32 > p[0] as i32 + 30 && p[1] as i32 > p[2] as i32 + 30).map(|p| p[1]).collect();
    assert!(greens.len() > 3000, "green cube: {}", greens.len());
    // Three faces lit differently.
    let mut levels: Vec<u8> = greens.iter().map(|g| g / 16).collect();
    levels.sort();
    levels.dedup();
    assert!(levels.len() >= 3, "{levels:?}");
    // One colour: the title's red.
    let img = render(&mut c, &spec(false), t, &sc);
    let reds = (0..(W * H) as usize).filter(|i| classify(&img[i * 4..]) == 1).count();
    assert!(reds > 3000, "{reds}");
    // Not there: nothing drawn.
    let gone = TitleSpec { model: Some(ModelSource { path: d.join("nope.obj"), ..src.clone() }), ..spec(true) };
    assert!(text_mesh(&gone, [W, H]).indices.is_empty());
    let _ = std::fs::remove_dir_all(&d);
}

/// A textured model: an OBJ cube whose material has a texture half red,
/// half blue (map_Kd) shows both; giving its part a colour of its own shows
/// that instead; the parts are listed by material name.
#[test]
fn models_show_their_textures_and_parts_can_be_recoloured() {
    use reelforge_core::ModelSource;
    let d = std::env::temp_dir().join(format!("rf_model_tex_{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    // 8 × 8: left half red, right half blue.
    let px: Vec<u8> = (0..64).flat_map(|i| if i % 8 < 4 { [255, 0, 0, 255] } else { [0, 0, 255, 255] }).collect();
    image::save_buffer(d.join("tex.png"), &px, 8, 8, image::ColorType::Rgba8).unwrap();
    // Every face maps the whole texture.
    std::fs::write(
        d.join("cube.obj"),
        "mtllib cube.mtl\nusemtl painted\nv -1 -1 -1\nv 1 -1 -1\nv 1 1 -1\nv -1 1 -1\nv -1 -1 1\nv 1 -1 1\nv 1 1 1\nv -1 1 1\nvt 0 0\nvt 1 0\nvt 1 1\nvt 0 1\nf 1/1 4/4 3/3 2/2\nf 5/1 6/2 7/3 8/4\nf 1/1 2/2 6/3 5/4\nf 2/1 3/2 7/3 6/4\nf 3/1 4/2 8/3 7/4\nf 4/1 1/2 5/3 8/4\n",
    )
    .unwrap();
    std::fs::write(d.join("cube.mtl"), "newmtl painted\nKd 1 1 1\nmap_Kd tex.png\n").unwrap();
    let src = ModelSource::new(d.join("cube.obj"));
    let model = reelforge_render::model3d::cached(&src).unwrap();
    assert_eq!(model.parts, vec!["painted".to_string()]);
    assert!(model.atlas.is_some());
    let spec = |s: &ModelSource| TitleSpec { size: 100.0, color: [230, 230, 230, 255], shadow: false, model: Some(s.clone()), ..TitleSpec::default() };
    let Some((dev, q)) = headless_device() else { return };
    let mut c = Compositor::new(&dev, &q);
    let sc = scene();
    let mut t = Transform { three_d: true, ..Default::default() };
    t.orientation = [-20.0, 30.0];
    let count = |img: &[u8], f: &dyn Fn(&[u8]) -> bool| (0..(W * H) as usize).filter(|i| f(&img[i * 4..i * 4 + 4])).count();
    let img = render(&mut c, &spec(&src), t, &sc);
    let red = count(&img, &|p| p[0] > 60 && p[0] as i32 > p[2] as i32 + 40 && p[1] < 40);
    let blue = count(&img, &|p| p[2] > 60 && p[2] as i32 > p[0] as i32 + 40 && p[1] < 40);
    assert!(red > 800 && blue > 800, "both halves of the texture: red {red}, blue {blue}");
    let yellow = ModelSource { part_colours: vec![("painted".into(), [255, 220, 0])], ..src.clone() };
    let img = render(&mut c, &spec(&yellow), t, &sc);
    let y = count(&img, &|p| p[0] > 60 && p[1] > 50 && p[2] < 40);
    let blue = count(&img, &|p| p[2] > 60 && p[2] as i32 > p[0] as i32 + 40);
    assert!(y > 2000 && blue == 0, "recoloured: yellow {y}, blue {blue}");
    let _ = std::fs::remove_dir_all(&d);
}

/// An environment light: a panorama green behind the camera and red in
/// front. A white 3D layer facing the camera takes its green light; 3D text
/// in red, matte, is dark in it (red lit by green), and with Reflections up
/// it mirrors the green behind the camera.
#[test]
fn environment_light_lights_layers_and_is_reflected() {
    use reelforge_core::environment::{load, uv_of};
    use reelforge_core::space3d::{layer_space, EnvLight, Light};
    let Some((d, q)) = headless_device() else { return };
    let mut c = Compositor::new(&d, &q);
    // A flat (not run-length) Radiance file, 32 × 16.
    let (w, h) = (32usize, 16usize);
    let mut file = format!("#?RADIANCE\nFORMAT=32-bit_rle_rgbe\n\n-Y {h} +X {w}\n").into_bytes();
    for _y in 0..h {
        for x in 0..w {
            let u = (x as f32 + 0.5) / w as f32;
            file.extend(if (u - 0.5).abs() < 0.25 { [128, 0, 0, 129] } else { [0, 128, 0, 129] });
        }
    }
    let dir = std::env::temp_dir().join(format!("rf_envlight_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("probe.hdr");
    std::fs::write(&path, &file).unwrap();
    let env = load(&path).unwrap();
    assert!((uv_of([0.0, 0.0, -1.0])[0] - 0.5).abs() > 0.49, "behind the camera is the picture's edge");
    let light = |reflect: f32| Light { kind: 4, color: [1.0; 3], position: [0.0; 3], direction: [0.0, 0.0, 1.0], cone: [0.5, 0.6], falloff: 0, radius: 1000.0, shadow: 0.0, softness: 0.0, env: Some(EnvLight { id: env.id, sh: env.turned_sh(0.0), turn: 0.0, reflect, blur: 0.0 }) };
    let cam = Camera::default_for([W as f32, H as f32]);

    let white: Vec<u8> = [255u8; 4].iter().copied().cycle().take(16 * 4).collect();
    let card = Transform { three_d: true, ..Default::default() };
    let sc = Scene3d { camera: cam, lights: vec![light(0.0)] };
    let ws = layer_space(&card, [200.0, 100.0], &sc);
    let layer = LayerInput { key: 2, content_id: 1, pixels: PixelData::Rgba(&white), pixel_size: [4, 4], layer_size: [200.0, 100.0], transform: card, blend: BlendMode::Normal, effects: Vec::new(), mask: None, wipe: None, mesh: None, space: Some(&ws) };
    c.render([W, H], &[layer]);
    let out = c.read_output().unwrap();
    let mid = &out[((H / 2 * W + W / 2) * 4) as usize..][..3];
    assert!(mid[1] > 230 && mid[0] < 40, "the card is lit green: {mid:?}");

    let s = spec(30.0);
    let shot = |c: &mut Compositor, reflect: f32| {
        let sc = Scene3d { camera: cam, lights: vec![light(reflect)] };
        render(c, &s, Transform { three_d: true, ..Default::default() }, &sc)
    };
    let face = |img: &[u8]| {
        // The average of the text's pixels.
        let lit: Vec<&[u8]> = img.chunks_exact(4).filter(|p| p[3] > 250 && (p[0] as u32 + p[1] as u32 + p[2] as u32) > 0).collect();
        let n = lit.len().max(1) as u32;
        [0, 1, 2].map(|k| lit.iter().map(|p| p[k] as u32).sum::<u32>() / n)
    };
    let matte = face(&shot(&mut c, 0.0));
    let mirror = face(&shot(&mut c, 1.0));
    assert!(mirror[1] > 180 && mirror[1] > matte[1] + 80 && mirror[0] < 100, "matte {matte:?}, mirror {mirror:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A Blender file (saved by Blender 5.3) draws in its materials' colours: the red cube,
/// the blue cylinder and its green caps.
#[test]
fn blender_files_draw_in_their_colours() {
    use reelforge_core::ModelSource;
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/scene-5.3.blend");
    let src = ModelSource { z_up: true, ..ModelSource::new(path) };
    let spec = TitleSpec { size: 160.0, color: [230, 230, 230, 255], shadow: false, model: Some(src), ..TitleSpec::default() };
    assert_eq!(text_mesh(&spec, [W, H]).indices.len(), 42 * 3);
    let Some((dev, q)) = headless_device() else { return };
    let mut c = Compositor::new(&dev, &q);
    let mut t = Transform { three_d: true, ..Default::default() };
    // (Tipped, so a cap shows.)
    t.orientation = [-50.0, 20.0];
    let img = render(&mut c, &spec, t, &scene());
    if let Ok(dir) = std::env::var("REELFORGE_LOOK") {
        let _ = image::save_buffer(format!("{dir}/blend.png"), &img, W, H, image::ColorType::Rgba8);
    }
    let count = |f: &dyn Fn(i32, i32, i32) -> bool| (0..(W * H) as usize).filter(|i| { let p = &img[i * 4..i * 4 + 4]; f(p[0] as i32, p[1] as i32, p[2] as i32) }).count();
    let (r, g, b) = (count(&|r, g, b| r > g + 40 && r > b + 40), count(&|r, g, b| g > r + 40 && g > b + 40), count(&|r, g, b| b > r + 40 && b > g + 40));
    assert!(r > 300 && g > 50 && b > 300, "red {r}, green {g}, blue {b}");
}

/// 3D Model Properties' surfaces: highlights brighten, its own light shows unlit,
/// see-through fades it, a hidden part goes, and a picture of its own wraps it.
#[test]
fn model_parts_take_their_surfaces() {
    use reelforge_core::{ModelSource, PartLook};
    let d = std::env::temp_dir().join(format!("rf_model_looks_{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(
        d.join("cube.obj"),
        "mtllib cube.mtl\nusemtl g\nv -1 -1 -1\nv 1 -1 -1\nv 1 1 -1\nv -1 1 -1\nv -1 -1 1\nv 1 -1 1\nv 1 1 1\nv -1 1 1\nf 1 4 3 2\nf 5 6 7 8\nf 1 2 6 5\nf 2 3 7 6\nf 3 4 8 7\nf 4 1 5 8\n",
    )
    .unwrap();
    std::fs::write(d.join("cube.mtl"), "newmtl g\nKd 0.3 0.3 0.3\n").unwrap();
    image::save_buffer(d.join("blue.png"), &[20u8, 60, 230, 255].repeat(16), 4, 4, image::ColorType::Rgba8).unwrap();
    let src = ModelSource::new(d.join("cube.obj"));
    let with = |look: PartLook, hidden: bool| {
        let model = ModelSource { materials: vec![PartLook { part: "g".into(), ..look }], hidden: if hidden { vec!["g".into()] } else { Vec::new() }, ..src.clone() };
        TitleSpec { size: 100.0, color: [230, 40, 30, 255], shadow: false, model: Some(model), ..TitleSpec::default() }
    };
    let Some((dev, q)) = headless_device() else { return };
    let mut c = Compositor::new(&dev, &q);
    let sc = scene();
    let mut t = Transform { three_d: true, ..Default::default() };
    t.orientation = [-20.0, 30.0];
    let mut shot = |s: &TitleSpec| render(&mut c, s, t, &sc);
    let sum = |img: &[u8], ch: usize| img.chunks(4).map(|p| p[ch] as u64).sum::<u64>();
    let brightest = |img: &[u8]| img.chunks(4).map(|p| p[0] as u32 + p[1] as u32 + p[2] as u32).max().unwrap_or(0);
    let plain = shot(&with(PartLook::default(), false));
    let lit = sum(&plain, 1);
    assert!(lit > 0);
    let shiny = shot(&with(PartLook { specular: [255; 3], shininess: 4.0, ..PartLook::default() }, false));
    assert!(brightest(&shiny) > brightest(&plain) + 30, "highlights: {} vs {}", brightest(&shiny), brightest(&plain));
    let glowing = shot(&with(PartLook { emissive: [200, 0, 0], ..PartLook::default() }, false));
    let (r0, r1, g1) = (sum(&plain, 0), sum(&glowing, 0), sum(&glowing, 1));
    assert!(r1 > r0 * 3 / 2 && g1.abs_diff(lit) * 50 < lit, "its own red light: red {r0} → {r1}, green {lit} → {g1}");
    let faint = shot(&with(PartLook { opacity: 30.0, ..PartLook::default() }, false));
    assert!(sum(&faint, 1) < lit * 2 / 3, "see-through: {} vs {lit}", sum(&faint, 1));
    let gone = shot(&with(PartLook::default(), true));
    assert!(sum(&gone, 1) < lit / 50, "hidden");
    let blue = shot(&with(PartLook { diffuse_map: Some(d.join("blue.png")), ..PartLook::default() }, false));
    assert!(sum(&blue, 2) > sum(&blue, 0) * 3, "its own picture");
    let _ = std::fs::remove_dir_all(&d);
}
