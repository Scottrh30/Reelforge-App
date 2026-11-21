//! A look pass on smoke pushed about by something moving through it, on
//! footage like the real thing: a person walking through a column of thick
//! smoke, and a car driving through low mist (each a keyed layer with alpha
//! over a background, the smoke on its own layer flowing round it). Not run
//! with the others: `cargo test -p reelforge-export --test smoke_look --
//! --ignored` writes frames to `target/look/` to look at.

use reelforge_core::effects::fluid_params as fp;
use reelforge_core::{Clip, ClipId, Effect, EffectKind, FrameRate, Layer, MediaAsset, MediaId, MediaInfo, Project, Rational, Sequence, Time, Track, TrackKind, VideoStreamInfo};
use reelforge_export::{FrameProvider, Picture, Scene};
use reelforge_media::VideoFrame;
use reelforge_render::{headless_device, Compositor};
use std::sync::Arc;

const W: u32 = 640;
const H: u32 = 360;
const FPS: u32 = 25;

#[derive(Clone, Copy, PartialEq)]
enum Subject {
    Walker,
    Car,
    /// Two people walking towards the camera out of a backlit cloud of smoke (after a
    /// stock shot of just that, looked at side by side).
    Pair,
}

/// Media 1: the subject (with alpha); 2: clear (the smoke's layer); 3: the
/// background.
struct Footage(Subject);

fn put(px: &mut [u8], x: i64, y: i64, c: [u8; 4]) {
    if (0..W as i64).contains(&x) && (0..H as i64).contains(&y) {
        let i = ((y * W as i64 + x) * 4) as usize;
        px[i..i + 4].copy_from_slice(&c);
    }
}

/// A filled capsule from `a` to `b`, `r` thick.
fn limb(px: &mut [u8], a: [f64; 2], b: [f64; 2], r: f64, c: [u8; 4]) {
    let (x0, x1) = (a[0].min(b[0]) - r, a[0].max(b[0]) + r);
    let (y0, y1) = (a[1].min(b[1]) - r, a[1].max(b[1]) + r);
    let d = [b[0] - a[0], b[1] - a[1]];
    let len2 = (d[0] * d[0] + d[1] * d[1]).max(1e-9);
    for y in y0.floor() as i64..=y1.ceil() as i64 {
        for x in x0.floor() as i64..=x1.ceil() as i64 {
            let p = [x as f64 - a[0], y as f64 - a[1]];
            let t = ((p[0] * d[0] + p[1] * d[1]) / len2).clamp(0.0, 1.0);
            let q = [p[0] - d[0] * t, p[1] - d[1] * t];
            if q[0] * q[0] + q[1] * q[1] <= r * r {
                put(px, x, y, c);
            }
        }
    }
}

fn walker(px: &mut [u8], t: f64) {
    let c = [38, 34, 44, 255];
    let x = -40.0 + t * 130.0;
    let swing = (t * 7.0).sin();
    let foot = 330.0;
    let hip = [x, foot - 82.0];
    let neck = [x + 3.0, foot - 150.0];
    for s in [swing, -swing] {
        let knee = [hip[0] + s * 16.0, hip[1] + 42.0];
        limb(px, hip, knee, 9.0, c);
        limb(px, knee, [knee[0] + s * 10.0 - 2.0, foot], 8.0, c);
        let elbow = [neck[0] - s * 14.0, neck[1] + 34.0];
        limb(px, [neck[0], neck[1] + 6.0], elbow, 6.0, c);
        limb(px, elbow, [elbow[0] - s * 10.0 + 4.0, elbow[1] + 30.0], 5.0, c);
    }
    limb(px, hip, neck, 16.0, c);
    limb(px, [neck[0] + 2.0, neck[1] - 20.0], [neck[0] + 2.0, neck[1] - 18.0], 13.0, c);
}

/// A figure seen from the front, its feet at `foot`, `scale` times life size here, walking
/// in place (arms and legs swinging a little).
fn figure(px: &mut [u8], cx: f64, foot: f64, scale: f64, t: f64, c: [u8; 4]) {
    let s = scale;
    let swing = (t * 5.0).sin() * 0.35;
    let hip = [cx, foot - 95.0 * s];
    let neck = [cx, foot - 170.0 * s];
    for side in [-1.0, 1.0] {
        let knee = [hip[0] + side * 9.0 * s, hip[1] + 48.0 * s];
        limb(px, [hip[0] + side * 7.0 * s, hip[1]], knee, 10.0 * s * (1.0 + side * swing * 0.1), c);
        limb(px, knee, [knee[0] + side * 2.0 * s, foot], 8.0 * s, c);
        let elbow = [neck[0] + side * 30.0 * s, neck[1] + 40.0 * s];
        limb(px, [neck[0] + side * 22.0 * s, neck[1] + 6.0 * s], elbow, 7.0 * s, c);
        limb(px, elbow, [elbow[0] + side * 4.0 * s, elbow[1] + 38.0 * s + side * swing * 6.0 * s], 6.0 * s, c);
    }
    limb(px, [cx - 8.0 * s, hip[1]], [cx - 10.0 * s, neck[1] + 10.0 * s], 16.0 * s, c);
    limb(px, [cx + 8.0 * s, hip[1]], [cx + 10.0 * s, neck[1] + 10.0 * s], 16.0 * s, c);
    limb(px, [cx, neck[1] - 24.0 * s], [cx, neck[1] - 20.0 * s], 15.0 * s, c);
}

fn car(px: &mut [u8], t: f64) {
    let x = -200.0 + t * 330.0;
    let (body, glass, wheel) = ([150, 20, 24, 255], [60, 80, 96, 255], [18, 18, 18, 255]);
    for y in 262..302i64 {
        for k in 0..190i64 {
            put(px, x as i64 + k, y, body);
        }
    }
    // The cabin: sloping front and back.
    for y in 226..262i64 {
        let up = (262 - y) as f64;
        for k in (30.0 + up * 0.8) as i64..(150.0 - up * 0.9) as i64 {
            put(px, x as i64 + k, y, if y > 232 && k % 60 > 4 { glass } else { body });
        }
    }
    for cx in [40.0, 150.0] {
        limb(px, [x + cx, 302.0], [x + cx, 302.0], 17.0, wheel);
    }
}

impl FrameProvider for Footage {
    fn picture(&mut self, _: &Project, l: &Layer, _: [u32; 2]) -> anyhow::Result<Option<Picture>> {
        let t = l.source_time.as_secs_f64();
        let mut px = vec![0u8; (W * H * 4) as usize];
        match l.clip.media.0 {
            1 if self.0 == Subject::Walker => walker(&mut px, t),
            1 if self.0 == Subject::Car => car(&mut px, t),
            1 => {
                // They come closer (bigger), she from inside the cloud.
                let c = [6, 8, 18, 255];
                let k = (t / 8.0).min(1.0);
                figure(&mut px, 250.0 + 15.0 * k, 400.0 + 60.0 * k, 0.95 + 0.55 * k, t, c);
                figure(&mut px, 420.0 + 10.0 * k, 410.0 + 60.0 * k, 1.05 + 0.6 * k, t + 1.3, c);
            }
            3 if self.0 == Subject::Pair => {
                // Black, a little blue.
                for p in px.chunks_exact_mut(4) {
                    p.copy_from_slice(&[4, 6, 16, 255]);
                }
            }
            3 => {
                for y in 0..H as usize {
                    // A dim street: sky down to the ground, the road darker.
                    let k = y as f32 / H as f32;
                    let c = if y < 320 { [(70.0 + 60.0 * k) as u8, (80.0 + 50.0 * k) as u8, (100.0 + 30.0 * k) as u8, 255] } else { [52, 50, 54, 255] };
                    for x in 0..W as usize {
                        px[(y * W as usize + x) * 4..][..4].copy_from_slice(&c);
                    }
                }
            }
            _ => {}
        }
        let frame = VideoFrame::from_rgba(Time::from_secs_f64(t), W, H, Arc::from(px), (t * 1000.0) as u64 + 1 + l.clip.media.0 * 1_000_000);
        Ok(Some(Picture { frame, size: [W as f32, H as f32], media: l.clip.media }))
    }
}

fn smoke(subject: Subject) -> Effect {
    let mut e = Effect::new(EffectKind::Fluid);
    let set: &[(usize, f32)] = match subject {
        // "Thick smoke", rising from the ground in the middle.
        Subject::Walker => &[(fp::X, 50.0), (fp::Y, 98.0), (fp::SIZE, 110.0), (fp::FORCE, 300.0), (fp::AMOUNT, 250.0), (fp::COLOR_A, 0x8a8a8a as f32), (fp::COLOR_B, 0x5c5c5c as f32), (fp::CYCLE, 0.1), (fp::RISE, 90.0), (fp::SWIRL, 120.0), (fp::FADE, 5.0)],
        // Low mist rolling along the road.
        // A big, dense, slowly rising cloud, lit from behind (white, blue at its edges).
        Subject::Pair => &[(fp::X, 30.0), (fp::Y, 95.0), (fp::SIZE, 110.0), (fp::FORCE, 160.0), (fp::AMOUNT, 160.0), (fp::COLOR_A, 0xf2f6ff as f32), (fp::COLOR_B, 0x9fb8ff as f32), (fp::CYCLE, 0.2), (fp::RISE, 60.0), (fp::SWIRL, 150.0), (fp::FADE, 3.0), (fp::BODY, 1.0), (fp::LIGHT, 60.0), (fp::SHADING, 100.0)],
        Subject::Car => &[(fp::X, 30.0), (fp::Y, 88.0), (fp::DIRECTION, 0.0), (fp::SIZE, 160.0), (fp::FORCE, 160.0), (fp::AMOUNT, 140.0), (fp::COLOR_A, 0xdfe6ea as f32), (fp::COLOR_B, 0xf2f4f6 as f32), (fp::CYCLE, 0.0), (fp::RISE, 15.0), (fp::SWIRL, 70.0), (fp::FADE, 4.0)],
    };
    for &(i, v) in set {
        e.params[i].value = v;
    }
    e.params[fp::DETAIL].value = 1.0;
    e.params[fp::OBSTACLES].value = 1.0;
    e.params[fp::OBSTACLE_LAYER].value = 1.0;
    e
}

fn project(subject: Subject, push: bool) -> Project {
    let mut p = Project::new();
    let info = MediaInfo {
        duration: Time::from_secs_f64(10.0),
        video: Some(VideoStreamInfo { width: W, height: H, frame_rate: FrameRate::new(FPS as i64, 1), codec: "prores".into(), time_base: Rational::new(1, FPS as i64), hdr: None, pixel_aspect: (1, 1) }),
        audio: None,
        container: String::new(),
    };
    for (id, name) in [(1, "subject.mov"), (2, "clear.mov"), (3, "street.mov")] {
        p.media.push(MediaAsset { id: MediaId(id), path: format!("/nowhere/{name}").into(), name: name.into(), info: info.clone(), conform: None });
    }
    let mut seq = Sequence::new("Look", W, H, FrameRate::new(FPS as i64, 1));
    seq.tracks.insert(2, Track::new("Video 3", TrackKind::Video));
    let clip = |id: u64, media: u64| Clip::new(ClipId(id), MediaId(media), Time::ZERO, Time::ZERO, Time::from_secs_f64(10.0), None);
    seq.tracks[0].clips.push(clip(3, 3));
    let mut fog = clip(2, 2);
    let mut e = smoke(subject);
    if !push {
        e.params[fp::OBSTACLES].value = 0.0;
    }
    fog.effects.push(e);
    seq.tracks[1].clips.push(fog);
    seq.tracks[2].clips.push(clip(1, 1));
    p.sequences = vec![seq];
    p
}

#[test]
#[ignore]
fn smoke_look_pass() {
    let Some((device, queue)) = headless_device() else { return };
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/look");
    let _ = std::fs::create_dir_all(&dir);
    for (subject, name, shots) in [(Subject::Walker, "walker", [1.6, 2.4, 3.2, 4.4]), (Subject::Car, "car", [0.8, 1.2, 1.6, 2.2]), (Subject::Pair, "pair", [2.0, 3.0, 4.0, 5.0])] {
        for push in [true, false] {
            let p = project(subject, push);
            let (mut scene, mut c) = (Scene::new(&device, &queue), Compositor::new(&device, &queue));
            let last = shots[3];
            for k in 0..=(last * FPS as f64).round() as usize {
                let t = k as f64 / FPS as f64;
                scene.render(&p, 0, Time::from_secs_f64(t), &mut c, &mut Footage(subject), None).unwrap();
                if shots.iter().any(|s| (s - t).abs() < 1e-6) {
                    let px = c.read_output().unwrap();
                    let file = dir.join(format!("{name}-{}-{t:.1}.png", if push { "push" } else { "through" }));
                    image::save_buffer(&file, &px, W, H, image::ExtendedColorType::Rgba8).unwrap();
                }
            }
        }
    }
}
