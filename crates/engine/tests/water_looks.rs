//! The built-in water and fluid looks, each drawn on a plain layer a little
//! way in: every one draws a picture, and no two look alike. (With
//! `RF_LOOKS_DUMP` set to a folder, each picture is written there as RGBA.)

use reelforge_core::{Clip, ClipId, FrameRate, Layer, Project, Sequence, Time};
use reelforge_engine::fx_presets::{starters, WATER_LOOKS};
use reelforge_export::{FrameProvider, Picture, Scene};
use reelforge_render::{headless_device, Compositor};

const W: u32 = 192;
const H: u32 = 108;

/// Nothing to decode: the layer is a plain one.
struct NoMedia;

impl FrameProvider for NoMedia {
    fn picture(&mut self, _: &Project, _: &Layer, _: [u32; 2]) -> anyhow::Result<Option<Picture>> {
        Ok(None)
    }
}

#[test]
fn every_water_look_draws_its_own_picture() {
    let Some((device, queue)) = headless_device() else { return };
    let looks: Vec<_> = starters().into_iter().filter(|p| p.folder == WATER_LOOKS).collect();
    let mut pictures: Vec<(String, Vec<u8>)> = Vec::new();
    for look in &looks {
        let mut p = Project::new();
        let mut seq = Sequence::new("Look", W, H, FrameRate::new(25, 1));
        // (a clear layer, as the Simulations menu adds them: the water shows its own floor)
        let mut clip = Clip::new_plane(ClipId(1), Time::ZERO, Time::from_secs_f64(10.0), [0, 0, 0, 0]);
        let mut v = clip.visual();
        look.apply(&mut v, Time::ZERO, Time::from_secs_f64(10.0), [W as f32, H as f32], false);
        clip.set_visual(v);
        seq.tracks[0].clips.push(clip);
        p.sequences = vec![seq];
        let (mut scene, mut c) = (Scene::new(&device, &queue), Compositor::new(&device, &queue));
        // (a liquid has to have poured, smoke to have risen)
        scene.render(&p, 0, Time::from_secs_f64(2.5), &mut c, &mut NoMedia, None).unwrap();
        let px = c.read_output().unwrap();
        assert_eq!(px.len(), (W * H * 4) as usize, "{}", look.name);
        if let Ok(dir) = std::env::var("RF_LOOKS_DUMP") {
            std::fs::write(format!("{dir}/{}.rgba", look.name), &px).unwrap();
        }
        pictures.push((look.name.clone(), px));
    }
    let plain = pictures.iter().map(|(_, p)| p).next().map(|p| p.len()).unwrap_or(0);
    assert!(plain > 0);
    // Each differs from every other in a good part of the picture.
    for (i, (a, pa)) in pictures.iter().enumerate() {
        for (b, pb) in pictures.iter().skip(i + 1) {
            let differ = pa.chunks(4).zip(pb.chunks(4)).filter(|(x, y)| x.iter().zip(y.iter()).any(|(p, q)| p.abs_diff(*q) > 8)).count();
            assert!(differ > (W * H / 50) as usize, "{a} and {b} look alike ({differ} pixels differ)");
        }
    }
}
