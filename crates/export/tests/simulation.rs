//! Water pushed about by a moving object comes out the same however the
//! frame is reached: played up to it, or jumped straight to it (a scrub, a
//! seek, an export starting part way in).

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
struct Crossing {
    frozen: Option<Time>,
}

impl FrameProvider for Crossing {
    fn picture(&mut self, _: &Project, l: &Layer, _: [u32; 2]) -> anyhow::Result<Option<Picture>> {
        let t = self.frozen.unwrap_or(l.source_time).as_secs_f64();
        let x0 = (10.0 + t * 100.0) as i64;
        let mut px = vec![0u8; (W * H * 4) as usize];
        for y in 30..60i64 {
            for x in x0..x0 + 20 {
                if (0..W as i64).contains(&x) {
                    let i = ((y * W as i64 + x) * 4) as usize;
                    px[i..i + 4].copy_from_slice(&[255, 255, 255, 255]);
                }
            }
        }
        let frame = VideoFrame::from_rgba(Time::from_secs_f64(t), W, H, Arc::from(px), (t * 1000.0) as u64 + 1);
        Ok(Some(Picture { frame, size: [W as f32, H as f32], media: MediaId(1) }))
    }
}

fn project() -> Project {
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
    let mut e = Effect::new(EffectKind::WaterSurface);
    // Still water but for the wake: what's solid is what isn't clear.
    for (i, v) in [(ws::RAIN, 0.0), (ws::WAVE_HEIGHT, 0.0), (ws::WIND, 0.0), (ws::SOLIDS, 1.0), (ws::WAKE, 100.0)] {
        e.params[i].value = v;
    }
    clip.effects.push(e);
    seq.tracks[0].clips.push(clip);
    p.sequences = vec![seq];
    p
}

fn picture(scene: &mut Scene, c: &mut Compositor, p: &Project, t: f64, frames: &mut Crossing) -> Vec<u8> {
    scene.render(p, 0, Time::from_secs_f64(t), c, frames, None).unwrap();
    c.read_output().unwrap()
}

fn differ(a: &[u8], b: &[u8]) -> usize {
    a.chunks(4).zip(b.chunks(4)).filter(|(x, y)| x.iter().zip(y.iter()).any(|(p, q)| p.abs_diff(*q) > 3)).count()
}

#[test]
fn water_pushed_by_a_moving_object_is_the_same_played_or_jumped_to() {
    let Some((device, queue)) = headless_device() else { return };
    let p = project();
    let end = 1.2;
    // Played: every frame up to 1.2 s.
    let (mut scene, mut c) = (Scene::new(&device, &queue), Compositor::new(&device, &queue));
    let mut frames = Crossing { frozen: None };
    let mut played = Vec::new();
    for k in 0..=30 {
        played = picture(&mut scene, &mut c, &p, k as f64 / 25.0, &mut frames);
    }
    // Jumped straight to 1.2 s with a fresh simulation.
    let (mut scene, mut c) = (Scene::new(&device, &queue), Compositor::new(&device, &queue));
    let jumped = picture(&mut scene, &mut c, &p, end, &mut frames);
    // What a jump used to give: every step seeing the frame jumped to.
    let (mut scene, mut c) = (Scene::new(&device, &queue), Compositor::new(&device, &queue));
    let stale = picture(&mut scene, &mut c, &p, end, &mut Crossing { frozen: Some(Time::from_secs_f64(end)) });
    let pixels = (W * H) as usize;
    let teleported = differ(&played, &stale);
    assert!(teleported > pixels / 50, "the object's path shows in the water ({teleported} of {pixels} pixels differ when it hasn't moved)");
    let off = differ(&played, &jumped);
    assert!(off <= pixels / 500, "jumped to, the water is as played ({off} of {pixels} pixels differ; {teleported} if it teleports)");
}
