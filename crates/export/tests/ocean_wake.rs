//! A keyed boat crossing the ocean: the white water at its hull, the spray,
//! and the wake and foam it leaves, played through frame by frame.

use reelforge_core::effects::ocean as oc;
use reelforge_core::{Clip, ClipId, Effect, EffectKind, FrameRate, Layer, MediaAsset, MediaId, MediaInfo, Project, Rational, Sequence, Time, VideoStreamInfo};
use reelforge_export::{FrameProvider, Picture, Scene};
use reelforge_media::VideoFrame;
use reelforge_render::{headless_device, Compositor};
use std::sync::Arc;

const W: u32 = 480;
const H: u32 = 270;
/// Pixels a second the boat moves right.
const SPEED: f64 = 70.0;

/// Footage of a boat (dark hull, white cabin) on a clear background, moving right.
struct Boat;

fn boat_x(t: f64) -> f64 {
    40.0 + t * SPEED
}

impl FrameProvider for Boat {
    fn picture(&mut self, _: &Project, l: &Layer, _: [u32; 2]) -> anyhow::Result<Option<Picture>> {
        let t = l.source_time.as_secs_f64();
        let x0 = boat_x(t);
        let mut px = vec![0u8; (W * H * 4) as usize];
        for y in 150..190i64 {
            for x in 0..W as i64 {
                let fx = x as f64 - x0;
                // A hull 90 px long, its bow pointed (to the right), and a cabin on top.
                let hull = (174..190).contains(&y) && fx >= 0.0 && fx <= 90.0 - (y - 174) as f64 * 0.0 - ((y - 174) as f64).max(0.0) * 0.0 && fx <= 90.0 - (190 - y) as f64 * 0.2;
                let cabin = (155..174).contains(&y) && (20.0..55.0).contains(&fx);
                let i = ((y * W as i64 + x) * 4) as usize;
                if hull {
                    px[i..i + 4].copy_from_slice(&[70, 40, 30, 255]);
                } else if cabin {
                    px[i..i + 4].copy_from_slice(&[235, 235, 230, 255]);
                }
            }
        }
        let frame = VideoFrame::from_rgba(Time::from_secs_f64(t), W, H, Arc::from(px), (t * 1000.0) as u64 + 1);
        Ok(Some(Picture { frame, size: [W as f32, H as f32], media: MediaId(1) }))
    }
}

/// The boat on the ocean, in a sequence of `fps` frames a second (its footage changing each frame).
fn project_at(fps: i64) -> Project {
    let mut p = Project::new();
    let info = MediaInfo {
        duration: Time::from_secs_f64(10.0),
        video: Some(VideoStreamInfo { width: W, height: H, frame_rate: FrameRate::new(fps, 1), codec: "prores".into(), time_base: Rational::new(1, fps), hdr: None, pixel_aspect: (1, 1) }),
        audio: None,
        container: String::new(),
    };
    p.media.push(MediaAsset { id: MediaId(1), path: "/nowhere/boat.mov".into(), name: "boat.mov".into(), info, conform: None });
    let mut seq = Sequence::new("Boat", W, H, FrameRate::new(fps, 1));
    let mut clip = Clip::new(ClipId(1), MediaId(1), Time::ZERO, Time::ZERO, Time::from_secs_f64(10.0), None);
    let mut e = Effect::new(EffectKind::Ocean);
    // The boat in the sea: its shape solid, shown in the water.
    e.params[oc::SHOW].value = 3.0;
    e.params[oc::SOLIDS].value = 1.0;
    // (for looking: RF_SET="29=0,27=50" sets those settings)
    for kv in std::env::var("RF_SET").unwrap_or_default().split(',').filter(|s| !s.is_empty()) {
        if let Some((k, v)) = kv.split_once('=') {
            e.params[k.parse::<usize>().unwrap()].value = v.parse().unwrap();
        }
    }
    clip.effects.push(e);
    seq.tracks[0].clips.push(clip);
    p.sequences = vec![seq];
    p
}

/// The picture at each of `at` (seconds), played frame by frame from the start.
fn played(at: &[f64]) -> Vec<Vec<u8>> {
    played_at(25, at)
}

/// As [`played`], at `fps` frames a second.
fn played_at(fps: i64, at: &[f64]) -> Vec<Vec<u8>> {
    let (device, queue) = headless_device().unwrap();
    let (mut scene, mut c) = (Scene::new(&device, &queue), Compositor::new(&device, &queue));
    let p = project_at(fps);
    let end = at.iter().copied().fold(0.0, f64::max);
    let mut out = Vec::new();
    for k in 0..=(end * fps as f64).round() as usize {
        let t = k as f64 / fps as f64;
        scene.render(&p, 0, Time::from_secs_f64(t), &mut c, &mut Boat, None).unwrap();
        if at.iter().any(|a| (a - t).abs() < 1e-6) {
            out.push(c.read_output().unwrap());
        }
    }
    out
}

#[test]
#[ignore]
fn dump_boat_frames() {
    if headless_device().is_none() {
        return;
    }
    let dir = std::env::var("RF_DUMP").unwrap();
    for (k, img) in played(&[1.0, 2.0, 3.0, 4.0]).iter().enumerate() {
        std::fs::write(format!("{dir}/boat{}.rgba", k + 1), img).unwrap();
    }
}

/// How far apart two pictures are in the water behind the boat at 3 s (its
/// track, from where it started to just behind its stern): mean difference.
fn wake_difference(a: &[u8], b: &[u8]) -> f64 {
    let stern = boat_x(3.0) as usize;
    let (mut sum, mut n) = (0.0, 0usize);
    for y in 160..200usize {
        for x in 30..stern.saturating_sub(6) {
            let i = (y * W as usize + x) * 4;
            for c in 0..3 {
                sum += (a[i + c] as f64 - b[i + c] as f64).abs();
                n += 1;
            }
        }
    }
    sum / n.max(1) as f64
}

/// Footage brings a new picture only every two or three of the water's
/// steps (60 a second against 25 frames); the boat's wake comes out much as
/// it would with a new picture every step (it used to be pushed only on the
/// steps with one, leaving stripes along the track).
#[test]
fn the_wake_doesnt_depend_on_the_footages_frame_rate() {
    if headless_device().is_none() {
        return;
    }
    let at25 = played_at(25, &[3.0]);
    let at60 = played_at(60, &[3.0]);
    let d = wake_difference(&at25[0], &at60[0]);
    if let Ok(dir) = std::env::var("RF_DUMP") {
        std::fs::write(format!("{dir}/at25.rgba"), &at25[0]).unwrap();
        std::fs::write(format!("{dir}/at60.rgba"), &at60[0]).unwrap();
    }
    // (pushed only on the steps with a new picture, it was 3.9; now 1.6, what's
    // left being the boat moving in bigger jumps at 25 frames a second)
    assert!(d < 2.5, "the wake at 25 fps differs from 60 fps by {d:.2}");
}
