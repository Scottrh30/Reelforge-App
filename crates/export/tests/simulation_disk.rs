//! Simulations worked out once come back from their saved checkpoints (see
//! `reelforge_render::simstore`) rather than being worked out again: the same
//! water, in a fresh compositor (a project opened again, an export).

use reelforge_core::effects::liquid as lq;
use reelforge_core::{Clip, ClipId, Effect, EffectKind, FrameRate, Layer, Project, Sequence, Time, TitleSpec};
use reelforge_export::{FrameProvider, Picture, Scene};
use reelforge_render::{headless_device, set_simulation_store, Compositor, SimulationStore};
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Memory {
    kept: Mutex<HashMap<u64, Vec<u8>>>,
    loads: AtomicUsize,
}

impl SimulationStore for Memory {
    fn has(&self, key: u64) -> bool {
        self.kept.lock().unwrap().contains_key(&key)
    }
    fn load(&self, key: u64) -> Option<Vec<u8>> {
        self.loads.fetch_add(1, Ordering::SeqCst);
        self.kept.lock().unwrap().get(&key).cloned()
    }
    fn save(&self, key: u64, bytes: Vec<u8>) {
        self.kept.lock().unwrap().insert(key, bytes);
    }
}

/// A clear picture for the (empty) title the water is on.
struct Clear;
impl FrameProvider for Clear {
    fn picture(&mut self, _: &Project, _: &Layer, f: [u32; 2]) -> anyhow::Result<Option<Picture>> {
        let px: Arc<[u8]> = vec![0u8; (f[0] * f[1] * 4) as usize].into();
        Ok(Some(Picture { frame: reelforge_media::VideoFrame::from_rgba(Time::ZERO, f[0], f[1], px, 7), size: [f[0] as f32, f[1] as f32], media: reelforge_core::MediaId::GENERATED }))
    }
}

#[test]
fn a_simulation_comes_back_from_its_saved_checkpoints() {
    let Some((device, queue)) = headless_device() else { return };
    let store = Arc::new(Memory::default());
    set_simulation_store(Some(store.clone()));
    // Water poured over an empty title.
    let mut p = Project::new();
    let mut seq = Sequence::new("Pour", 320, 180, FrameRate::new(25, 1));
    let mut spec = TitleSpec::for_frame(180);
    spec.text = String::new();
    let mut clip = Clip::new_title(ClipId(1), Time::ZERO, Time::from_secs_f64(5.0), spec);
    let mut e = Effect::new(EffectKind::Liquid);
    for (i, v) in [(lq::X, 30.0), (lq::Y, 5.0), (lq::DIRECTION, -70.0), (lq::FLOW, 250.0), (lq::SHOW, 2.0)] {
        e.params[i].value = v;
    }
    clip.effects.push(e);
    seq.tracks[0].clips.push(clip);
    p.sequences = vec![seq];
    let at = Time::from_secs_f64(2.5);
    let draw = || {
        let (mut scene, mut c) = (Scene::new(&device, &queue), Compositor::new(&device, &queue));
        let start = std::time::Instant::now();
        scene.render(&p, 0, at, &mut c, &mut Clear, None).unwrap();
        (c.read_output().unwrap(), start.elapsed())
    };
    let (first, worked_out) = draw();
    let saved = store.kept.lock().unwrap().len();
    assert_eq!(saved, 2, "a checkpoint for each whole second simulated");
    assert_eq!(store.loads.load(Ordering::SeqCst), 0);
    // A fresh compositor finds the checkpoint at 2 s and goes on from there.
    let (again, from_disk) = draw();
    assert!(store.loads.load(Ordering::SeqCst) >= 1, "loaded from the store");
    assert_eq!(first, again, "exactly the same water");
    eprintln!("worked out in {worked_out:?}, from the saved checkpoint in {from_disk:?}");
    // A saved checkpoint that doesn't fit (a damaged file) is passed over.
    for v in store.kept.lock().unwrap().values_mut() {
        v.truncate(v.len() / 2);
    }
    let (third, _) = draw();
    assert_eq!(first, third, "worked out again instead");
    set_simulation_store(None);
}
