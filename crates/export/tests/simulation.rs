//! Water and smoke pushed about by a moving object come out the same however
//! the frame is reached: played up to it, or jumped straight to it (a scrub, a
//! seek, an export starting part way in).

use reelforge_core::effects::fluid_params as fp;
use reelforge_core::effects::water as ws;
use reelforge_core::{Clip, ClipId, Effect, EffectKind, FrameRate, Layer, MediaAsset, MediaId, MediaInfo, Project, Rational, Sequence, Time, VideoStreamInfo};
use reelforge_export::{FrameProvider, Picture, Scene};
use reelforge_media::VideoFrame;
use reelforge_render::{headless_device, Compositor};
use std::sync::Arc;

const W: u32 = 160;
const H: u32 = 90;

/// Footage of a solid square crossing a clear frame (100 px a second). With
/// `frozen`, every frame is the one at that time instead: the object as if
/// it had been there all along.
/// (Media 2 is clear: a layer for smoke on its own. `shift` moves the
/// square in the picture.)
#[derive(Default)]
struct Crossing {
    frozen: Option<Time>,
    shift: [i64; 2],
    /// A second object as big crossing the top the other way (the two
    /// together don't move on the whole).
    decoy: bool,
}

impl FrameProvider for Crossing {
    fn picture(&mut self, _: &Project, l: &Layer, _: [u32; 2]) -> anyhow::Result<Option<Picture>> {
        let t = self.frozen.unwrap_or(l.source_time).as_secs_f64();
        let x0 = (10.0 + t * 100.0) as i64 + self.shift[0];
        let mut px = vec![0u8; (W * H * 4) as usize];
        let rows = if l.clip.media == MediaId(2) { 0..0 } else { 30 + self.shift[1]..60 + self.shift[1] };
        for y in rows {
            for x in x0..x0 + 20 {
                if (0..W as i64).contains(&x) && (0..H as i64).contains(&y) {
                    let i = ((y * W as i64 + x) * 4) as usize;
                    px[i..i + 4].copy_from_slice(&[255, 255, 255, 255]);
                }
            }
        }
        if self.decoy && l.clip.media != MediaId(2) {
            let x1 = (120.0 - t * 100.0) as i64;
            for y in 0..20i64 {
                for x in x1.max(0)..(x1 + 30).min(W as i64) {
                    let i = ((y * W as i64 + x) * 4) as usize;
                    px[i..i + 4].copy_from_slice(&[255, 255, 255, 255]);
                }
            }
        }
        let frame = VideoFrame::from_rgba(Time::from_secs_f64(t), W, H, Arc::from(px), (t * 1000.0) as u64 + 1);
        Ok(Some(Picture { frame, size: [W as f32, H as f32], media: MediaId(1) }))
    }
}

/// The wake: still water but for what the object stirs up (what's solid is
/// what isn't clear).
fn wake() -> Effect {
    let mut e = Effect::new(EffectKind::WaterSurface);
    for (i, v) in [(ws::RAIN, 0.0), (ws::WAVE_HEIGHT, 0.0), (ws::WIND, 0.0), (ws::SOLIDS, 1.0), (ws::WAKE, 100.0)] {
        e.params[i].value = v;
    }
    e
}

/// Smoke rising from the bottom middle into the object's path, flowing round
/// it (`obstacles`) or not; no swirl or colour change, so only the object
/// stirs it.
fn smoke(obstacles: bool) -> Effect {
    let mut e = Effect::new(EffectKind::Fluid);
    for (i, v) in [(fp::SIZE, 14.0), (fp::FORCE, 120.0), (fp::AMOUNT, 200.0), (fp::SWIRL, 0.0), (fp::FADE, 0.0), (fp::CYCLE, 0.0), (fp::OBSTACLES, if obstacles { 1.0 } else { 0.0 })] {
        e.params[i].value = v;
    }
    e
}

fn project(e: Effect) -> Project {
    let mut p = Project::new();
    let info = MediaInfo {
        duration: Time::from_secs_f64(10.0),
        video: Some(VideoStreamInfo { width: W, height: H, frame_rate: FrameRate::new(25, 1), codec: "prores".into(), time_base: Rational::new(1, 25), hdr: None, pixel_aspect: (1, 1) }),
        audio: None,
        container: String::new(),
    };
    p.media.push(MediaAsset { id: MediaId(1), path: "/nowhere/boat.mov".into(), name: "boat.mov".into(), info, conform: None });
    let mut seq = Sequence::new("Wake", W, H, FrameRate::new(25, 1));
    let mut clip = Clip::new(ClipId(1), MediaId(1), Time::ZERO, Time::ZERO, Time::from_secs_f64(10.0), None);
    clip.effects.push(e);
    seq.tracks[0].clips.push(clip);
    p.sequences = vec![seq];
    p
}

/// The smoke on its own clear layer (track 1), flowing round the square on
/// the track above (or, with `from` 0, its own clear layer: nothing), the
/// square's layer moved by `moved` px.
fn two_layers(from: f32, moved: [f32; 2]) -> Project {
    let mut e = smoke(true);
    e.params[fp::OBSTACLE_LAYER].value = from;
    let mut p = project(e);
    let mut info = p.media[0].clone();
    info.id = MediaId(2);
    p.media.push(info);
    let seq = &mut p.sequences[0];
    let mut subject = seq.tracks[0].clips[0].clone();
    subject.effects.clear();
    subject.transform.position = moved;
    seq.tracks[0].clips[0].media = MediaId(2);
    seq.tracks[1].clips.push(Clip { id: ClipId(2), ..subject });
    p
}

fn picture(scene: &mut Scene, c: &mut Compositor, p: &Project, t: f64, frames: &mut Crossing) -> Vec<u8> {
    scene.render(p, 0, Time::from_secs_f64(t), c, frames, None).unwrap();
    c.read_output().unwrap()
}

fn differ(a: &[u8], b: &[u8]) -> usize {
    a.chunks(4).zip(b.chunks(4)).filter(|(x, y)| x.iter().zip(y.iter()).any(|(p, q)| p.abs_diff(*q) > 3)).count()
}

/// The pictures `p` gives at `end` s: played (every frame up to it), jumped
/// to (a fresh simulation), and what a jump used to give (every step seeing
/// the frame jumped to, as if the object had been there all along).
fn played_jumped_stale(device: &reelforge_render::wgpu::Device, queue: &reelforge_render::wgpu::Queue, p: &Project, end: f64) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let (mut scene, mut c) = (Scene::new(device, queue), Compositor::new(device, queue));
    let mut frames = Crossing::default();
    let mut played = Vec::new();
    for k in 0..=(end * 25.0).round() as usize {
        played = picture(&mut scene, &mut c, p, k as f64 / 25.0, &mut frames);
    }
    let (mut scene, mut c) = (Scene::new(device, queue), Compositor::new(device, queue));
    let jumped = picture(&mut scene, &mut c, p, end, &mut frames);
    let (mut scene, mut c) = (Scene::new(device, queue), Compositor::new(device, queue));
    let stale = picture(&mut scene, &mut c, p, end, &mut Crossing { frozen: Some(Time::from_secs_f64(end)), ..Crossing::default() });
    (played, jumped, stale)
}

#[test]
fn water_pushed_by_a_moving_object_is_the_same_played_or_jumped_to() {
    let Some((device, queue)) = headless_device() else { return };
    let p = project(wake());
    let (played, jumped, stale) = played_jumped_stale(&device, &queue, &p, 1.2);
    let pixels = (W * H) as usize;
    let teleported = differ(&played, &stale);
    assert!(teleported > pixels / 50, "the object's path shows in the water ({teleported} of {pixels} pixels differ when it hasn't moved)");
    let off = differ(&played, &jumped);
    assert!(off <= pixels / 500, "jumped to, the water is as played ({off} of {pixels} pixels differ; {teleported} if it teleports)");
}

/// How much smoke (blue, not the white object) there is, and how far across it
/// is on average, in the band the object crosses.
fn smoke_in_path(img: &[u8]) -> (usize, f64) {
    let (mut n, mut sx) = (0usize, 0.0);
    for y in 20..70 {
        for x in 0..W as usize {
            let q = &img[(y * W as usize + x) * 4..][..4];
            if q[2] > 60 && q[0] < 200 {
                n += 1;
                sx += x as f64;
            }
        }
    }
    (n, if n == 0 { 0.0 } else { sx / n as f64 })
}

#[test]
fn smoke_flows_round_a_moving_object_and_is_pushed_by_it() {
    let Some((device, queue)) = headless_device() else { return };
    let end = 1.0;
    let (played, jumped, stale) = played_jumped_stale(&device, &queue, &project(smoke(true)), end);
    let pixels = (W * H) as usize;
    // (The smoke used to be worked out afresh round each frame's shape: this.)
    let teleported = differ(&played, &stale);
    assert!(teleported > pixels / 50, "the object's path shows in the smoke ({teleported} of {pixels} pixels differ when it hasn't moved)");
    let off = differ(&played, &jumped);
    assert!(off <= pixels / 200, "jumped to, the smoke is as played ({off} of {pixels} pixels differ; {teleported} if it teleports)");
    // Nothing inside the object where it is now.
    let x0 = (10.0 + end * 100.0) as usize;
    let inside = (34..56).flat_map(|y| (x0 + 3..x0 + 17).map(move |x| (x, y))).filter(|&(x, y)| { let q = &played[(y * W as usize + x) * 4..][..4]; q[2] > 60 && q[0] < 200 }).count();
    assert_eq!(inside, 0, "no smoke inside the object");
    // Pushed: the object sweeping right carries smoke with it, further right
    // than the same smoke without obstacles.
    let (still, _, _) = played_jumped_stale(&device, &queue, &project(smoke(false)), end);
    let (n_moved, x_moved) = smoke_in_path(&played);
    let (n_still, x_still) = smoke_in_path(&still);
    assert!(n_moved > 100 && n_still > 100, "smoke in the object's path: {n_moved}, {n_still}");
    assert!(x_moved > x_still + 3.0, "pushed right: on average {x_moved:.1} px across, {x_still:.1} without the object");
}

#[test]
fn smoke_on_its_own_layer_flows_round_a_subject_on_the_layer_above() {
    let Some((device, queue)) = headless_device() else { return };
    let end: f64 = 1.0;
    let pixels = (W * H) as usize;
    let render = |p: &Project, frames: &mut Crossing| {
        let (mut scene, mut c) = (Scene::new(&device, &queue), Compositor::new(&device, &queue));
        let mut out = Vec::new();
        for k in 0..=(end * 25.0).round() as usize {
            out = picture(&mut scene, &mut c, p, k as f64 / 25.0, frames);
        }
        out
    };
    // The square moved on its own layer: as the square drawn there in one
    // layer with the smoke flowing round its own shape.
    let moved = [20.0, 10.0];
    let two = render(&two_layers(1.0, moved), &mut Crossing::default());
    let one = render(&project(smoke(true)), &mut Crossing { shift: [20, 10], ..Crossing::default() });
    let off = differ(&two, &one);
    assert!(off <= pixels / 200, "flowing round the layer above is as flowing round its own shape ({off} of {pixels} pixels differ)");
    // Its own clear shape (nothing to flow round) is another thing.
    let through = render(&two_layers(0.0, moved), &mut Crossing::default());
    let apart = differ(&two, &through);
    assert!(apart > pixels / 50, "the subject on the other layer pushes the smoke ({apart} of {pixels} pixels differ from flowing through)");
    // Nothing below: as nothing to flow round.
    let below = render(&two_layers(2.0, moved), &mut Crossing::default());
    assert!(differ(&below, &through) <= pixels / 500, "no layer below: nothing to flow round");
    // Jumped to, the in-between frames step it on with the other layer too.
    let (mut scene, mut c) = (Scene::new(&device, &queue), Compositor::new(&device, &queue));
    let jumped = picture(&mut scene, &mut c, &two_layers(1.0, moved), end, &mut Crossing::default());
    let off = differ(&two, &jumped);
    assert!(off <= pixels / 200, "jumped to, as played ({off} of {pixels} pixels differ; {apart} if it flows through)");
}
#[test]
fn two_objects_moving_opposite_ways_each_push_the_smoke() {
    let Some((device, queue)) = headless_device() else { return };
    let end: f64 = 1.0;
    let render = |p: &Project, frames: &mut Crossing| {
        let (mut scene, mut c) = (Scene::new(&device, &queue), Compositor::new(&device, &queue));
        let mut out = Vec::new();
        for k in 0..=(end * 25.0).round() as usize {
            out = picture(&mut scene, &mut c, p, k as f64 / 25.0, frames);
        }
        out
    };
    // The object sweeping right still carries smoke right, though another
    // as big goes left across the top (the shape as a whole doesn't move).
    let both = render(&project(smoke(true)), &mut Crossing { decoy: true, ..Crossing::default() });
    let still = render(&project(smoke(false)), &mut Crossing { decoy: true, ..Crossing::default() });
    let (n_moved, x_moved) = smoke_in_path(&both);
    let (n_still, x_still) = smoke_in_path(&still);
    assert!(n_moved > 100 && n_still > 100, "smoke in the object's path: {n_moved}, {n_still}");
    assert!(x_moved > x_still + 3.0, "pushed right: on average {x_moved:.1} px across, {x_still:.1} without the objects");
    // Nothing inside the object where it is now.
    let x0 = (10.0 + end * 100.0) as usize;
    let inside = (34..56).flat_map(|y| (x0 + 3..x0 + 17).map(move |x| (x, y))).filter(|&(x, y)| { let q = &both[(y * W as usize + x) * 4..][..4]; q[2] > 60 && q[0] < 200 }).count();
    assert_eq!(inside, 0, "no smoke inside the object");
}

/// Jumped to `end` with a catch-up of at most `cap` frames (approximate, as
/// the viewer does past 250),
/// then left idle until filled in: (the picture first shown, the picture once
/// filled in, ticks it took).
fn jumped_then_filled_in(p: &Project, end: f64, cap: usize) -> (Vec<u8>, Vec<u8>, usize) {
    let (device, queue) = headless_device().unwrap();
    let (mut scene, mut c) = (Scene::new(&device, &queue), Compositor::new(&device, &queue));
    scene.max_catch_up = Some(cap);
    let mut frames = Crossing::default();
    let t = Time::from_secs_f64(end);
    let mut sig = scene.render(p, 0, t, &mut c, &mut frames, None).unwrap();
    let first = c.read_output().unwrap();
    scene.idle = true;
    for tick in 1..200 {
        let now = scene.render(p, 0, t, &mut c, &mut frames, Some(sig)).unwrap();
        if now != sig {
            return (first, c.read_output().unwrap(), tick);
        }
        sig = now;
    }
    panic!("never filled in");
}

#[test]
fn a_long_jump_is_filled_in_while_idle() {
    let Some((device, queue)) = headless_device() else { return };
    let pixels = (W * H) as usize;
    let mut round_above = smoke(true);
    round_above.params[fp::OBSTACLE_LAYER].value = 1.0;
    for (name, e, end, two) in [("water", wake(), 1.2, false), ("smoke", smoke(true), 1.0, false), ("smoke round the layer above", round_above, 1.0, true)] {
        let p = if two { two_layers(1.0, [20.0, 10.0]) } else { project(e) };
        let (played, _, _) = played_jumped_stale(&device, &queue, &p, end);
        let (first, filled, ticks) = jumped_then_filled_in(&p, end, 3);
        let rough = differ(&played, &first);
        let off = differ(&played, &filled);
        assert!(rough > pixels / 100, "{name}: three frames for a {end} s jump is rough ({rough} pixels off)");
        assert!(off <= pixels / 200, "{name}: filled in, it's as played ({off} pixels off; {rough} before)");
        // (a few frames a tick: ~25 frames in about 4 ticks, then drawn)
        assert!(ticks <= 10, "{name}: took {ticks} ticks");
    }
}
