//! OpenFX effects end to end: the OpenFX SDK's example plugins, built here
//! with the system C++ compiler, found by a scan, put on a clip and exported.
//! Skipped without a compiler, `ffmpeg`-built media support or a GPU.

use reelforge_core::{Clip, ClipId, Effect, FrameRate, Project, Sequence, Time};
use reelforge_export::{export_sequence, frame_recipe, ofxhost, ExportSettings};
use reelforge_media::{ColorOut, Fit, FrameSource, Quality, VideoCodec};
use reelforge_render::headless_device;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The tests here install plugins (a scan replaces the installed set), so
/// they take turns.
fn serial() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// Build some of the SDK's examples (and Reelforge's test plugin) as bundles
/// in a fresh folder, once.
fn build_plugins() -> Option<PathBuf> {
    static DIR: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        if !cfg!(target_os = "linux") {
            return None;
        }
        let tests = Path::new(env!("CARGO_MANIFEST_DIR")).join("../ofx/tests");
        let sdk = tests.join("sdk");
        let out = std::env::temp_dir().join(format!("rf_ofx_export_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&out);
        for (name, src) in [("Invert", "sdk/examples/invert"), ("Rectangle", "sdk/examples/rectangle"), ("FrameAverage", "plugins/frameavg"), ("OpenGL", "sdk/examples/opengl")] {
            let bin = out.join(format!("Examples/{name}.ofx.bundle/Contents/Linux-x86-64"));
            std::fs::create_dir_all(&bin).ok()?;
            let ok = std::process::Command::new("c++")
                .args(["-O1", "-shared", "-fPIC", "-w", "-I"])
                .arg(sdk.join("include"))
                .arg("-o")
                .arg(bin.join(format!("{name}.ofx")))
                .arg(tests.join(format!("{src}.cpp")))
                .args(if name == "OpenGL" { &["-lGL"][..] } else { &[] })
                .status()
                .is_ok_and(|s| s.success());
            if !ok {
                eprintln!("no C++ compiler: skipping");
                return None;
            }
        }
        Some(out)
    })
    .clone()
}

fn settings(path: PathBuf) -> ExportSettings {
    ExportSettings {
        path,
        width: 160,
        height: 90,
        quality: Quality::High,
        include_audio: false,
        start: Time::ZERO,
        end: Time::from_secs_f64(0.2),
        video: Some(VideoCodec::ProRes422Hq),
        hardware: false,
        fit: Fit::Letterbox,
        sequence: 0,
        color: ColorOut::Sdr8,
        bitrate_kbps: None,
        alpha: false,
    }
}

fn px(rgba: &[u8], w: usize, x: usize, y: usize) -> [u8; 3] {
    let i = (y * w + x) * 4;
    [rgba[i], rgba[i + 1], rgba[i + 2]]
}

fn near(a: [u8; 3], b: [u8; 3]) -> bool {
    a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= 5)
}

#[test]
fn openfx_plugins_are_found_and_render_in_export() {
    let _turn = serial();
    let Some(dir) = build_plugins() else { return };
    let state = dir.join("state");
    std::fs::create_dir_all(&state).unwrap();
    let report = ofxhost::scan(std::slice::from_ref(&dir), Some(&state));
    assert_eq!(report.len(), 4, "{report:?}");
    assert!(report.iter().all(|r| r.errors.is_empty() && !r.blocked), "{report:?}");
    let installed = reelforge_core::ofx::installed();
    let invert = installed.iter().find(|d| d.name == "OFX Invert Example").expect("invert").clone();
    let rect = installed.iter().find(|d| d.name.contains("Rectangle")).expect("rectangle").clone();
    assert_eq!(invert.group, "OFX Example");
    // The rectangle's corners show as % of the frame, its colour as a colour and an opacity.
    let names: Vec<&str> = rect.params.iter().map(|p| p.name).collect();
    assert!(names.iter().any(|n| n.ends_with(" X")) && names.iter().any(|n| n.ends_with(" opacity")), "{names:?}");

    let Some((device, queue)) = headless_device() else { return };
    let s = Time::from_secs_f64;
    let mut p = Project::new();
    let mut sq = Sequence::new("O", 160, 90, FrameRate::new(24, 1));
    let mut plane = Clip::new_plane(ClipId(2), s(0.0), s(1.0), [200, 100, 50, 255]);
    plane.effects.push(Effect::new_ofx(&invert));
    sq.tracks[0].clips = vec![plane];
    p.sequences = vec![sq];
    // Settings and plugins are part of what decides the picture.
    let r0 = frame_recipe(&p, 0, s(0.0), &HashMap::new());
    let out = std::env::temp_dir().join(format!("rf_ofx_{}.mov", std::process::id()));
    export_sequence(&p, &settings(out.clone()), &device, &queue, |_| true).unwrap();
    let mut src = FrameSource::open(&out, None).unwrap();
    let f = src.frame_at_blocking(s(0.0), Duration::from_secs(5)).unwrap().rgba().to_vec();
    assert!(near(px(&f, 160, 80, 45), [55, 155, 205]), "inverted: {:?}", px(&f, 160, 80, 45));

    // Then a red rectangle over the left half, bottom half (positions are % of
    // the frame, y down, like Reelforge's own; the plugin gets them y up).
    let mut fx = Effect::new_ofx(&rect);
    let set = |fx: &mut Effect, name: &str, v: f32| {
        let i = rect.params.iter().position(|p| p.name == name).unwrap_or_else(|| panic!("{name}: {names:?}"));
        fx.params[i].value = v;
    };
    let corner = |k: usize| rect.params[rect.slots[k].first].name.trim_end_matches(" X").to_string();
    let (c1, c2) = (corner(0), corner(1));
    set(&mut fx, &format!("{c1} X"), 0.0);
    set(&mut fx, &format!("{c1} Y"), 50.0);
    set(&mut fx, &format!("{c2} X"), 50.0);
    set(&mut fx, &format!("{c2} Y"), 100.0);
    assert_eq!(rect.points().len(), 2, "both corners get handles in the Viewer");
    let colour = rect.params.iter().position(|p| p.ui == reelforge_core::effects::ParamUi::Color).unwrap();
    fx.params[colour].value = 0xff0000 as f32;
    fx.params[colour + 1].value = 100.0;
    p.sequences[0].tracks[0].clips[0].effects.push(fx);
    assert_ne!(frame_recipe(&p, 0, s(0.0), &HashMap::new()), r0);
    export_sequence(&p, &settings(out.clone()), &device, &queue, |_| true).unwrap();
    let mut src = FrameSource::open(&out, None).unwrap();
    let f = src.frame_at_blocking(s(0.0), Duration::from_secs(5)).unwrap().rgba().to_vec();
    assert!(near(px(&f, 160, 30, 70), [255, 0, 0]), "rectangle: {:?}", px(&f, 160, 30, 70));
    assert!(near(px(&f, 160, 120, 20), [55, 155, 205]), "outside it, still inverted: {:?}", px(&f, 160, 120, 20));
    assert!(near(px(&f, 160, 30, 20), [55, 155, 205]), "above it: {:?}", px(&f, 160, 30, 20));

    // Uninstalled: the project still exports, without the effects.
    ofxhost::scan(&[], None);
    export_sequence(&p, &settings(out.clone()), &device, &queue, |_| true).unwrap();
    let mut src = FrameSource::open(&out, None).unwrap();
    let f = src.frame_at_blocking(s(0.0), Duration::from_secs(5)).unwrap().rgba().to_vec();
    assert!(near(px(&f, 160, 30, 70), [200, 100, 50]), "{:?}", px(&f, 160, 30, 70));
    std::fs::remove_file(&out).ok();
}

/// A clip whose frames alternate dark and light grey (lossless).
fn flicker() -> Option<PathBuf> {
    let path = std::env::temp_dir().join(format!("rf_ofx_flicker_{}.mkv", std::process::id()));
    let ok = std::process::Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i", "color=black:s=160x90:r=24:d=1"])
        .args(["-vf", "format=gbrp,geq=r='if(mod(N,2),200,40)':g='if(mod(N,2),200,40)':b='if(mod(N,2),200,40)'"])
        .args(["-c:v", "ffv1"])
        .arg(&path)
        .status()
        .is_ok_and(|s| s.success());
    ok.then_some(path)
}

/// A plugin that reads other frames gets the clip's neighbouring frames, in
/// export: averaging a flickering clip over three frames evens it out.
#[test]
fn a_plugin_that_reads_other_frames_gets_its_neighbours() {
    let _turn = serial();
    let Some(dir) = build_plugins() else { return };
    let Some(src) = flicker() else { eprintln!("no ffmpeg: skipping"); return };
    let Some((device, queue)) = headless_device() else { return };
    ofxhost::scan(std::slice::from_ref(&dir), None);
    let avg = reelforge_core::ofx::installed().into_iter().find(|d| d.name == "Test Frame Average").expect("installed");
    assert!(avg.temporal);
    let s = Time::from_secs_f64;
    let rate = FrameRate::new(24, 1);
    let mut p = Project::new();
    p.media.push(reelforge_core::MediaAsset { id: reelforge_core::MediaId(1), path: src.clone(), name: "flicker".into(), info: reelforge_media::probe(&src).unwrap() });
    let mut sq = Sequence::new("T", 160, 90, rate);
    let mut clip = Clip::new(ClipId(1), reelforge_core::MediaId(1), s(0.0), s(0.0), s(1.0), None);
    clip.effects.push(Effect::new_ofx(&avg));
    sq.tracks[0].clips = vec![clip];
    p.sequences = vec![sq];
    let out = std::env::temp_dir().join(format!("rf_ofx_avg_{}.mov", std::process::id()));
    let mut st = settings(out.clone());
    (st.start, st.end) = (Time::from_frame(4, rate), Time::from_frame(7, rate));
    export_sequence(&p, &st, &device, &queue, |_| true).unwrap();
    let mut dec = FrameSource::open(&out, None).unwrap();
    // Light frame 5 between dark 4 and 6: (40 + 200 + 40) / 3 (plugins work on
    // the sRGB-encoded values, as editing hosts give them). Dark frame 6
    // between light 5 and 7: (200 + 40 + 200) / 3. Unaveraged: 200 and 40.
    for (k, want) in [(1, 280.0 / 3.0), (2, 440.0 / 3.0)] {
        let f = dec.frame_at_blocking(Time::from_frame(k, rate), Duration::from_secs(5)).unwrap().rgba().to_vec();
        let got = px(&f, 160, 80, 45)[1] as f64;
        assert!((got - want).abs() < 4.0, "frame {}: {got} vs {want:.0}", k + 4);
    }
    std::fs::remove_file(&out).ok();
    std::fs::remove_file(&src).ok();
}

/// An OpenGL plugin renders in export (on Mesa's software OpenGL here).
#[test]
fn an_opengl_plugin_renders_in_export() {
    let _turn = serial();
    let Some(dir) = build_plugins() else { return };
    if let Err(e) = reelforge_ofx::gpu_status() {
        eprintln!("no OpenGL ({e}): skipping");
        return;
    }
    let Some((device, queue)) = headless_device() else { return };
    ofxhost::scan(std::slice::from_ref(&dir), None);
    let gl = reelforge_core::ofx::installed().into_iter().find(|d| d.name == "OFX OpenGL Example").expect("installed");
    assert_eq!((gl.gpu.clone(), gl.cpu), (vec!["OpenGL".to_string()], false));
    let s = Time::from_secs_f64;
    let mut p = Project::new();
    let mut sq = Sequence::new("G", 320, 180, FrameRate::new(24, 1));
    let mut plane = Clip::new_plane(ClipId(2), s(0.0), s(1.0), [40, 200, 40, 255]);
    plane.effects.push(Effect::new_ofx(&gl));
    sq.tracks[0].clips = vec![plane];
    p.sequences = vec![sq];
    let out = std::env::temp_dir().join(format!("rf_ofx_gl_{}.mov", std::process::id()));
    let mut st = settings(out.clone());
    (st.width, st.height) = (320, 180);
    export_sequence(&p, &st, &device, &queue, |_| true).unwrap();
    let mut dec = FrameSource::open(&out, None).unwrap();
    let f = dec.frame_at_blocking(s(0.0), Duration::from_secs(5)).unwrap().rgba().to_vec();
    // Its red box, from (10, 10) to (200, 100) up from the bottom; the green plane elsewhere.
    assert!(near(px(&f, 320, 100, 130), [255, 0, 0]), "box: {:?}", px(&f, 320, 100, 130));
    assert!(near(px(&f, 320, 260, 40), [40, 200, 40]), "plane: {:?}", px(&f, 320, 260, 40));
    assert!(ofxhost::error(ClipId(2), 0).is_none());
    assert!(ofxhost::rendered_with(ClipId(2), 0).is_some_and(|w| w.starts_with("OpenGL on ")), "{:?}", ofxhost::rendered_with(ClipId(2), 0));
    std::fs::remove_file(&out).ok();
}
