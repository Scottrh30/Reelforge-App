//! Motion blur from a layer's own movement: a moving box smears along its
//! path by as much as the shutter is open; a still one doesn't.

use reelforge_core::effects::motion_blur as mb;
use reelforge_core::{Clip, ClipId, Effect, EffectKind, FrameRate, Layer, Project, Sequence, Time};
use reelforge_export::{FrameProvider, Picture, Scene};
use reelforge_render::{headless_device, Compositor};

const W: u32 = 320;
const H: u32 = 180;

struct Nothing;

impl FrameProvider for Nothing {
    fn picture(&mut self, _: &Project, _: &Layer, _: [u32; 2]) -> anyhow::Result<Option<Picture>> {
        Ok(None)
    }
}

/// A white box 32 × 18 px moving right at 1000 px a second (40 px a frame at
/// 25 a second), with Motion blur at `shutter` degrees; `moving: false`, still.
fn project(shutter: f32, moving: bool) -> Project {
    let mut p = Project::new();
    let mut seq = Sequence::new("Blur", W, H, FrameRate::new(25, 1));
    let mut c = Clip::new_plane(ClipId(1), Time::ZERO, Time::from_secs_f64(2.0), [255, 255, 255, 255]);
    c.transform.scale = [0.1, 0.1];
    if moving {
        c.anim.position.set(Time::ZERO, [-100.0, 0.0]);
        c.anim.position.set(Time::from_secs_f64(1.0), [900.0, 0.0]);
    }
    let mut e = Effect::new(EffectKind::MotionBlur);
    e.params[mb::SHUTTER].value = shutter;
    e.params[mb::SAMPLES].value = 32.0;
    c.effects.push(e);
    seq.tracks[0].clips.push(c);
    p.sequences = vec![seq];
    p
}

fn frame(p: &Project, t: f64) -> Option<Vec<u8>> {
    let (device, queue) = headless_device()?;
    let mut c = Compositor::new(&device, &queue);
    let mut scene = Scene::new(&device, &queue);
    scene.render(p, 0, Time::from_secs_f64(t), &mut c, &mut Nothing, None).unwrap();
    c.read_output()
}

/// The box's lit columns along its middle row: (first, last, how many fully lit).
fn span(img: &[u8]) -> (u32, u32, usize) {
    let y = H / 2;
    let v: Vec<u8> = (0..W).map(|x| img[((y * W + x) * 4) as usize]).collect();
    let lit: Vec<u32> = (0..W).filter(|&x| v[x as usize] > 8).collect();
    (lit.first().copied().unwrap_or(0), lit.last().copied().unwrap_or(0), v.iter().filter(|&&p| p > 245).count())
}

#[test]
fn a_moving_layer_smears_along_its_path() {
    // At 0.2 s the box's centre is at 100 px right of the middle: x 244–276.
    let Some(sharp) = frame(&project(0.0, true), 0.2) else { return };
    let (a, b, full) = span(&sharp);
    assert!((242..=246).contains(&a) && (273..=277).contains(&b) && full >= 28, "sharp: {a}–{b}, {full} full");
    // 180°: open half a frame (20 px), centred on now: 10 px longer each way.
    let Some(blurred) = frame(&project(180.0, true), 0.2) else { return };
    let (a2, b2, full2) = span(&blurred);
    assert!((a as i64 - 10 - a2 as i64).abs() <= 3 && (b2 as i64 - b as i64 - 10).abs() <= 3, "smeared: {a2}–{b2}");
    assert!(full2 < full, "the ends fade: {full2} full");
    // A still box: no different with the effect on.
    let Some(still) = frame(&project(180.0, false), 0.2) else { return };
    let Some(still_sharp) = frame(&project(0.0, false), 0.2) else { return };
    assert_eq!(span(&still), span(&still_sharp));
}
