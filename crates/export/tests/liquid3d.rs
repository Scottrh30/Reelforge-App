//! The 3D liquid poured over a picture: water appears where it's poured,
//! fills the box from its floor, and a pour pointed at the camera reaches
//! further across the picture as it comes.

use reelforge_core::effects::liquid3d as l3;
use reelforge_core::{Clip, ClipId, Effect, EffectKind, FrameRate, Layer, MediaAsset, MediaId, MediaInfo, Project, Rational, Sequence, Time, VideoStreamInfo};
use reelforge_export::{FrameProvider, Picture, Scene};
use reelforge_media::VideoFrame;
use reelforge_render::{headless_device, Compositor};
use std::sync::Arc;

const W: u32 = 320;
const H: u32 = 180;

/// A grey checkerboard (so the water's bending shows).
struct Checks;

impl FrameProvider for Checks {
    fn picture(&mut self, _: &Project, _: &Layer, _: [u32; 2]) -> anyhow::Result<Option<Picture>> {
        let mut px = vec![0u8; (W * H * 4) as usize];
        for y in 0..H {
            for x in 0..W {
                let v = if ((x / 20) + (y / 20)) % 2 == 0 { 200 } else { 120 };
                let i = ((y * W + x) * 4) as usize;
                px[i..i + 4].copy_from_slice(&[v, v, v, 255]);
            }
        }
        let frame = VideoFrame::from_rgba(Time::ZERO, W, H, Arc::from(px), 1);
        Ok(Some(Picture { frame, size: [W as f32, H as f32], media: MediaId(1) }))
    }
}

fn project(set: &[(usize, f32)]) -> Project {
    let mut p = Project::new();
    let info = MediaInfo {
        duration: Time::from_secs_f64(10.0),
        video: Some(VideoStreamInfo { width: W, height: H, frame_rate: FrameRate::new(25, 1), codec: "png".into(), time_base: Rational::new(1, 25), hdr: None, pixel_aspect: (1, 1) }),
        audio: None,
        container: String::new(),
    };
    p.media.push(MediaAsset { id: MediaId(1), path: "/nowhere/checks.png".into(), name: "checks.png".into(), info, conform: None });
    let mut seq = Sequence::new("Pour", W, H, FrameRate::new(25, 1));
    let mut clip = Clip::new(ClipId(1), MediaId(1), Time::ZERO, Time::ZERO, Time::from_secs_f64(10.0), None);
    let mut e = Effect::new(EffectKind::Liquid3D);
    for &(i, v) in set {
        e.params[i].value = v;
    }
    clip.effects.push(e);
    seq.tracks[0].clips.push(clip);
    p.sequences = vec![seq];
    p
}

/// The picture at `t`, played up to it frame by frame.
fn played(p: &Project, t: f64) -> Vec<u8> {
    let (device, queue) = headless_device().unwrap();
    let (mut scene, mut c) = (Scene::new(&device, &queue), Compositor::new(&device, &queue));
    for k in 0..=(t * 25.0).round() as usize {
        scene.render(p, 0, Time::from_secs_f64(k as f64 / 25.0), &mut c, &mut Checks, None).unwrap();
    }
    c.read_output().unwrap()
}

/// How many pixels aren't the checkerboard any more (the water's).
fn wet(img: &[u8]) -> usize {
    img.chunks(4)
        .enumerate()
        .filter(|(k, q)| {
            let (x, y) = ((*k as u32) % W, (*k as u32) / W);
            let v = if ((x / 20) + (y / 20)) % 2 == 0 { 200 } else { 120 };
            q[..3].iter().any(|c| c.abs_diff(v) > 12)
        })
        .count()
}

#[test]
#[ignore]
fn dump_pours() {
    if headless_device().is_none() {
        return;
    }
    let dir = std::env::var("RF_DUMP").unwrap();
    // (the splash as the Simulations menu sets it up, scaled to this little picture)
    let k = H as f32 / 1080.0;
    let splash = vec![(l3::X, 50.0), (l3::Y, 75.0), (l3::Z, 80.0), (l3::DIRECTION, 90.0), (l3::TOWARD, 62.0), (l3::SPEED, 2600.0 * k), (l3::WIDTH, 220.0 * k), (l3::FLOW, 260.0), (l3::FOR, 0.3), (l3::GRAVITY, 980.0 * k), (l3::SPLASH, 95.0), (l3::DROPLETS, 250.0), (l3::CONTAINER, 1.0)];
    for (name, set) in [("pour", vec![]), ("towards", vec![(l3::TOWARD, 60.0), (l3::SPEED, 700.0)]), ("pool", vec![(l3::FILL, 30.0), (l3::FLOW, 0.0)]), ("splash", splash)] {
        if std::env::var("RF_ONLY").is_ok_and(|o| o != name) {
            continue;
        }
        for t in [0.3, 0.6, 1.0] {
            std::fs::write(format!("{dir}/{name}-{t}.rgba"), played(&project(&set), t)).unwrap();
        }
    }
}

/// The fraction of pixels in rows `y0..y1` the water covers.
fn wet_rows(img: &[u8], y0: u32, y1: u32) -> f64 {
    let mut n = 0usize;
    for y in y0..y1 {
        for x in 0..W {
            let i = ((y * W + x) * 4) as usize;
            let v = if ((x / 20) + (y / 20)) % 2 == 0 { 200 } else { 120 };
            if img[i..i + 3].iter().any(|c| c.abs_diff(v) > 12) {
                n += 1;
            }
        }
    }
    n as f64 / ((y1 - y0) * W) as f64
}

#[test]
fn a_3d_pour_shows_where_its_poured_and_comes_towards_the_camera() {
    if headless_device().is_none() {
        return;
    }
    // (a narrow stream: the defaults are for a 1080-line picture, not this little one)
    let narrow = |toward: f32| project(&[(l3::WIDTH, 12.0), (l3::TOWARD, toward)]);
    // Water near the source (30 % across, 20 % down) as it starts.
    let early = played(&narrow(35.0), 0.3);
    let near_source = wet_rows(&early, 20, 60);
    assert!(near_source > 0.05, "water by the source: {near_source:.3}");
    // From the middle of the picture, pointed at the camera it covers more of
    // it (nearer, so bigger) than pointed away. (No gravity: straight on.)
    let middle = |toward: f32| project(&[(l3::WIDTH, 12.0), (l3::TOWARD, toward), (l3::X, 50.0), (l3::Y, 50.0), (l3::DIRECTION, 0.0), (l3::GRAVITY, 0.0), (l3::SPEED, 250.0)]);
    let towards = wet(&played(&middle(70.0), 0.6));
    let away = wet(&played(&middle(-70.0), 0.6));
    assert!(towards * 10 > away * 13, "towards the camera {towards} pixels, away {away}");
}

/// A box filled to 30 %: water across the bottom of the picture, none at the top.
#[test]
fn a_filled_box_is_water_at_the_bottom() {
    if headless_device().is_none() {
        return;
    }
    let img = played(&project(&[(l3::FILL, 30.0), (l3::FLOW, 0.0)]), 0.4);
    let (bottom, top) = (wet_rows(&img, H * 4 / 5, H), wet_rows(&img, 0, H / 4));
    assert!(bottom > 0.8 && top < 0.02, "bottom {bottom:.2}, top {top:.2}");
}
