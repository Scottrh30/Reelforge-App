//! Echo: copies of a layer from moments before, each fainter, drawn with it.

use reelforge_core::effects::echo;
use reelforge_core::{Clip, ClipId, Effect, EffectKind, FrameRate, Layer, MediaAsset, MediaId, MediaInfo, Project, Rational, Sequence, Time, VideoStreamInfo};
use reelforge_export::{FrameProvider, Picture, Scene};
use reelforge_media::VideoFrame;
use reelforge_render::{headless_device, Compositor};
use std::sync::Arc;

const W: u32 = 160;
const H: u32 = 90;

/// Footage of a white bar 8 px wide crossing a clear frame at 100 px a second
/// (its left edge at 10 px at the start).
struct Crossing;

impl FrameProvider for Crossing {
    fn picture(&mut self, _: &Project, l: &Layer, _: [u32; 2]) -> anyhow::Result<Option<Picture>> {
        let t = l.source_time.as_secs_f64();
        let x0 = (10.0 + t * 100.0).round() as i64;
        let mut px = vec![0u8; (W * H * 4) as usize];
        for y in 30..60i64 {
            for x in x0..x0 + 8 {
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

fn project(set: &[(usize, f32)]) -> Project {
    let mut p = Project::new();
    let info = MediaInfo {
        duration: Time::from_secs_f64(10.0),
        video: Some(VideoStreamInfo { width: W, height: H, frame_rate: FrameRate::new(25, 1), codec: "prores".into(), time_base: Rational::new(1, 25), hdr: None, pixel_aspect: (1, 1) }),
        audio: None,
        container: String::new(),
    };
    p.media.push(MediaAsset { id: MediaId(1), path: "/nowhere/bar.mov".into(), name: "bar.mov".into(), info, conform: None });
    let mut seq = Sequence::new("Echo", W, H, FrameRate::new(25, 1));
    let mut clip = Clip::new(ClipId(1), MediaId(1), Time::ZERO, Time::ZERO, Time::from_secs_f64(10.0), None);
    let mut e = Effect::new(EffectKind::Echo);
    for &(i, v) in [(echo::DELAY, 0.2), (echo::COUNT, 3.0), (echo::FIRST, 60.0), (echo::DECAY, 30.0)].iter().chain(set) {
        e.params[i].value = v;
    }
    clip.effects.push(e);
    seq.tracks[0].clips.push(clip);
    p.sequences = vec![seq];
    p
}

fn frame(p: &Project, t: f64) -> Option<Vec<u8>> {
    let (device, queue) = headless_device()?;
    let mut c = Compositor::new(&device, &queue);
    let mut scene = Scene::new(&device, &queue);
    scene.render(p, 0, Time::from_secs_f64(t), &mut c, &mut Crossing, None).unwrap();
    c.read_output()
}

/// A share of white, blended in linear light, as the output's (sRGB) level.
fn level(share: f32) -> i32 {
    let s = if share <= 0.003_130_8 { share * 12.92 } else { 1.055 * share.powf(1.0 / 2.4) - 0.055 };
    (s * 255.0).round() as i32
}

/// Within a few levels of `share` of white.
fn about(v: u8, share: f32) -> bool {
    (v as i32 - level(share)).abs() <= 8
}

fn grey(img: &[u8], x: u32) -> u8 {
    let i = ((45 * W + x) * 4) as usize;
    img[i]
}

#[test]
fn echoes_trail_behind_fading() {
    // At 0.7 s the bar is at 80 px; 0.2 s before at 60, then 40, then 20.
    let Some(img) = frame(&project(&[]), 0.7) else { return };
    let (main, e1, e2, e3, gap) = (grey(&img, 83), grey(&img, 63), grey(&img, 43), grey(&img, 23), grey(&img, 72));
    assert!(main > 245, "the bar itself: {main}");
    // 60 %, then 70 % of that, then 70 % again.
    assert!(about(e1, 0.6), "first echo: {e1}");
    assert!(about(e2, 0.42), "second echo: {e2}");
    assert!(about(e3, 0.294), "third echo: {e3}");
    assert!(gap < 5, "nothing between them: {gap}");

    // Early on there's nothing before the clip to echo.
    let Some(img) = frame(&project(&[]), 0.1) else { return };
    assert!(grey(&img, 23) > 245, "the bar");
    assert!(grey(&img, 3) < 5, "no echo from before the clip starts");

    // Without the original, the echoes alone.
    let Some(img) = frame(&project(&[(echo::ORIGINAL, 0.0)]), 0.5) else { return };
    assert!(grey(&img, 63) < 5 && grey(&img, 43) > 180, "echoes only");

    // A negative time echoes what's to come: ahead of the bar.
    let Some(img) = frame(&project(&[(echo::DELAY, -0.2)]), 0.5) else { return };
    assert!(grey(&img, 83) > 180 && grey(&img, 43) < 5, "ahead, not behind");

    // No echoes: only the bar.
    let Some(img) = frame(&project(&[(echo::COUNT, 0.0)]), 0.5) else { return };
    assert!(grey(&img, 63) > 245 && grey(&img, 43) < 5, "no echoes");
}

#[test]
fn added_echoes_brighten_where_they_overlap() {
    // Echoes 0.04 s apart overlap the bar (4 px on): added, they brighten what's behind
    // them past what one copy would give.
    let Some(img) = frame(&project(&[(echo::DELAY, 0.04), (echo::COUNT, 2.0), (echo::FIRST, 50.0), (echo::DECAY, 0.0), (echo::DRAW, 2.0)]), 0.5) else { return };
    // The bar from 60 to 67; echoes from 56 and from 52. 52–55: one echo, 56–59: two.
    let (one, two) = (grey(&img, 53), grey(&img, 57));
    assert!(about(one, 0.5), "one echo: {one}");
    assert!(two > 240, "two added: {two}");
}
