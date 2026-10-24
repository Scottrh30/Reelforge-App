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
