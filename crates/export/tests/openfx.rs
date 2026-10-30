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

/// Build some of the SDK's examples as bundles in a fresh folder.
fn build_plugins() -> Option<PathBuf> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let sdk = Path::new(env!("CARGO_MANIFEST_DIR")).join("../ofx/tests/sdk");
    let out = std::env::temp_dir().join(format!("rf_ofx_export_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&out);
    for (name, src) in [("Invert", "invert"), ("Rectangle", "rectangle")] {
        let bin = out.join(format!("Examples/{name}.ofx.bundle/Contents/Linux-x86-64"));
        std::fs::create_dir_all(&bin).ok()?;
        let ok = std::process::Command::new("c++")
            .args(["-O1", "-shared", "-fPIC", "-w", "-I"])
            .arg(sdk.join("include"))
            .arg("-o")
            .arg(bin.join(format!("{name}.ofx")))
            .arg(sdk.join("examples").join(format!("{src}.cpp")))
            .status()
            .is_ok_and(|s| s.success());
        if !ok {
            eprintln!("no C++ compiler: skipping");
            return None;
        }
    }
    Some(out)
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
    let Some(dir) = build_plugins() else { return };
    let state = dir.join("state");
    std::fs::create_dir_all(&state).unwrap();
    let report = ofxhost::scan(std::slice::from_ref(&dir), Some(&state));
    assert_eq!(report.len(), 2, "{report:?}");
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

    // Then a red rectangle over the left half, bottom half (OpenFX counts y up).
    let mut fx = Effect::new_ofx(&rect);
    let set = |fx: &mut Effect, name: &str, v: f32| {
        let i = rect.params.iter().position(|p| p.name == name).unwrap_or_else(|| panic!("{name}: {names:?}"));
        fx.params[i].value = v;
    };
    let corner = |k: usize| rect.params[rect.slots[k].first].name.trim_end_matches(" X").to_string();
    let (c1, c2) = (corner(0), corner(1));
    set(&mut fx, &format!("{c1} X"), 0.0);
    set(&mut fx, &format!("{c1} Y"), 0.0);
    set(&mut fx, &format!("{c2} X"), 50.0);
    set(&mut fx, &format!("{c2} Y"), 50.0);
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
    let _ = std::fs::remove_dir_all(&dir);
}
