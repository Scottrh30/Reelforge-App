//! Hosting the OpenFX SDK's own example plugins (tests/sdk), built here with
//! the system C++ compiler. Skipped where there's no compiler.

use reelforge_ofx::{find_bundles, load, Image, ParamKind, Plugin, Value};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

/// The plugins built for these tests: (bundle name, source).
const PLUGINS: [(&str, &str); 6] = [("Invert", "invert"), ("Gain", "basic"), ("Rectangle", "rectangle"), ("ChoiceParams", "choiceparams"), ("OpenGL", "opengl"), ("FrameAverage", "plugins/frameavg")];

/// Build the examples once, as plugin bundles in a temporary folder.
fn plugins_dir() -> Option<&'static Path> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| {
        if !cfg!(target_os = "linux") {
            return None;
        }
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
        let sdk = dir.join("sdk");
        let out = std::env::temp_dir().join(format!("rf_ofx_sdk_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&out);
        for (name, src) in PLUGINS {
            let bin = out.join(format!("{name}.ofx.bundle/Contents/Linux-x86-64"));
            std::fs::create_dir_all(&bin).ok()?;
            let file = if src.starts_with("plugins/") { dir.join(format!("{src}.cpp")) } else { sdk.join("examples").join(format!("{src}.cpp")) };
            let ok = std::process::Command::new("c++")
                .args(["-O1", "-shared", "-fPIC", "-w"])
                .arg("-I")
                .arg(sdk.join("include"))
                .arg("-o")
                .arg(bin.join(format!("{name}.ofx")))
                .arg(file)
                // The OpenGL example calls OpenGL itself.
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
    .as_deref()
}

fn plugin(name: &str) -> Option<Arc<Plugin>> {
    let dir = plugins_dir()?;
    // SAFETY: the SDK's example plugins, built just now.
    let (mut plugins, errors) = unsafe { load(&dir.join(format!("{name}.ofx.bundle"))) }.unwrap();
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(plugins.len(), 1);
    Some(Arc::new(plugins.remove(0)))
}

fn flat(w: u32, h: u32, px: [f32; 4]) -> Image {
    Image { width: w, height: h, rgba: px.iter().copied().cycle().take((w * h * 4) as usize).collect() }
}

fn at(img: &Image, x: u32, y: u32) -> [f32; 4] {
    let i = ((y * img.width + x) * 4) as usize;
    img.rgba[i..i + 4].try_into().unwrap()
}

fn near(a: [f32; 4], b: [f32; 4], tol: f32) -> bool {
    a.iter().zip(b).all(|(x, y)| (x - y).abs() <= tol)
}

#[test]
fn finds_bundles_in_folders() {
    let Some(dir) = plugins_dir() else { return };
    let found = find_bundles(&[dir.to_path_buf()]);
    let names: Vec<String> = found.iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
    assert_eq!(names.len(), PLUGINS.len(), "{names:?}");
    assert!(names.iter().all(|n| n.ends_with(".ofx.bundle")));
}

#[test]
fn invert_an_8_bit_plugin() {
    let Some(p) = plugin("Invert") else { return };
    assert_eq!(p.label, "OFX Invert Example");
    assert_eq!(p.group, "OFX Example");
    assert!(p.params.is_empty());
    let mut inst = p.instance().unwrap();
    // A gradient, so a mix-up of rows or columns would show.
    let mut img = flat(7, 5, [0.0; 4]);
    for y in 0..5 {
        for x in 0..7 {
            let i = ((y * 7 + x) * 4) as usize;
            img.rgba[i..i + 4].copy_from_slice(&[x as f32 / 6.0, y as f32 / 4.0, 0.25, 1.0]);
        }
    }
    let out = inst.render(&img, 0.0, 25.0, &[]).unwrap();
    for y in 0..5 {
        for x in 0..7 {
            let want = [1.0 - x as f32 / 6.0, 1.0 - y as f32 / 4.0, 0.75, 0.0];
            assert!(near(at(&out, x, y), want, 1.0 / 255.0 + 1e-6), "({x}, {y}): {:?} vs {want:?}", at(&out, x, y));
        }
    }
    // Again (the instance is reused frame after frame).
    assert_eq!(inst.render(&img, 1.0, 25.0, &[]).unwrap(), out);
}

#[test]
fn gain_with_double_and_boolean_parameters_in_float() {
    let Some(p) = plugin("Gain") else { return };
    assert_eq!(p.context, "OfxImageEffectContextFilter");
    let names: Vec<&str> = p.params.iter().map(|q| q.name.as_str()).collect();
    for n in ["scale", "scaleComponents", "scaleR", "scaleG", "scaleB", "scaleA"] {
        assert!(names.contains(&n), "{names:?}");
    }
    let scale = p.params.iter().position(|q| q.name == "scale").unwrap();
    let info = &p.params[scale];
    assert!(matches!(info.kind, ParamKind::Double { dims: 1, normalised: false, .. }));
    assert_eq!((info.default[0], info.min[0], info.max[0]), (1.0, 0.0, 100.0));
    assert!(matches!(p.params[names.iter().position(|n| *n == "scaleComponents").unwrap()].kind, ParamKind::Bool));
    let mut inst = p.instance().unwrap();
    let img = flat(16, 9, [0.25, 0.1, 0.05, 1.0]);
    // Defaults: unchanged.
    let out = inst.render(&img, 0.0, 25.0, &[]).unwrap();
    assert!(near(at(&out, 3, 3), [0.25, 0.1, 0.05, 1.0], 1e-6), "{:?}", at(&out, 3, 3));
    // Scale 2: doubled (in float, above 1 too).
    let mut v: Vec<Option<Value>> = vec![None; p.params.len()];
    v[scale] = Some(Value::Numbers(vec![2.0]));
    let out = inst.render(&flat(16, 9, [0.75, 0.1, 0.05, 1.0]), 0.0, 25.0, &v).unwrap();
    assert!(near(at(&out, 8, 4), [1.5, 0.2, 0.1, 2.0], 1e-5), "{:?}", at(&out, 8, 4));
    // Per channel: red ×3 on top of the overall ×2.
    v[names.iter().position(|n| *n == "scaleComponents").unwrap()] = Some(Value::Numbers(vec![1.0]));
    v[names.iter().position(|n| *n == "scaleR").unwrap()] = Some(Value::Numbers(vec![3.0]));
    let out = inst.render(&img, 0.0, 25.0, &v).unwrap();
    assert!(near(at(&out, 0, 0), [1.5, 0.2, 0.1, 2.0], 1e-5), "{:?}", at(&out, 0, 0));
}

#[test]
fn rectangle_with_normalised_points_and_a_colour() {
    let Some(p) = plugin("Rectangle") else { return };
    let corner1 = p.params.iter().position(|q| q.name == "corner1").unwrap();
    let corner2 = p.params.iter().position(|q| q.name == "corner2").unwrap();
    let colour = p.params.iter().position(|q| q.name == "colour").unwrap();
    assert!(matches!(p.params[corner1].kind, ParamKind::Double { dims: 2, normalised: true, point: true }));
    assert_eq!(p.params[corner1].default, vec![0.4, 0.4]);
    assert_eq!(p.params[colour].kind, ParamKind::Rgba);
    let mut inst = p.instance().unwrap();
    let white = flat(100, 50, [1.0; 4]);
    let mut v: Vec<Option<Value>> = vec![None; p.params.len()];
    // Corners as fractions of the frame, y up (OpenFX's way): x 10–30 %, y 10–50 % from the bottom.
    v[corner1] = Some(Value::Numbers(vec![0.1, 0.1]));
    v[corner2] = Some(Value::Numbers(vec![0.3, 0.5]));
    v[colour] = Some(Value::Numbers(vec![1.0, 0.0, 0.0, 1.0]));
    let out = inst.render(&white, 0.0, 25.0, &v).unwrap();
    // Inside: red. Rows count down from the top: y 10–50 % up is rows 25–45.
    assert!(near(at(&out, 20, 35), [1.0, 0.0, 0.0, 1.0], 1e-5), "{:?}", at(&out, 20, 35));
    // Outside: the picture.
    for (x, y) in [(5, 35), (40, 35), (20, 10), (20, 48), (90, 2)] {
        assert!(near(at(&out, x, y), [1.0; 4], 1e-5), "({x}, {y}): {:?}", at(&out, x, y));
    }
}

#[test]
fn choices_and_string_choices() {
    let Some(p) = plugin("ChoiceParams") else { return };
    let red = p.params.iter().position(|q| q.name == "red_choice").unwrap();
    let blue = p.params.iter().position(|q| q.name == "blue_choice").unwrap();
    assert_eq!(p.params[red].kind, ParamKind::Choice(vec!["Red: none".into(), "Red: some".into(), "Red: lots".into()]));
    let ParamKind::StrChoice { labels, values } = &p.params[blue].kind else { panic!("{:?}", p.params[blue].kind) };
    assert_eq!((labels.len(), values[2].as_str()), (3, "blue_1.0"));
    assert_eq!(p.params[blue].default, vec![2.0], "the default value's position");
    let mut inst = p.instance().unwrap();
    let white = flat(8, 8, [1.0; 4]);
    // Defaults: red none, green none, blue "lots".
    let out = inst.render(&white, 0.0, 25.0, &[]).unwrap();
    assert!(near(at(&out, 4, 4), [0.0, 0.0, 1.0, 1.0], 1e-5), "{:?}", at(&out, 4, 4));
    let mut v: Vec<Option<Value>> = vec![None; p.params.len()];
    v[red] = Some(Value::Numbers(vec![1.0]));
    v[blue] = Some(Value::Numbers(vec![0.0]));
    let out = inst.render(&white, 0.0, 25.0, &v).unwrap();
    assert!(near(at(&out, 4, 4), [0.5, 0.0, 0.0, 1.0], 1e-5), "{:?}", at(&out, 4, 4));
}

#[test]
fn several_instances_keep_their_own_settings() {
    let Some(p) = plugin("Gain") else { return };
    let scale = p.params.iter().position(|q| q.name == "scale").unwrap();
    let (mut a, mut b) = (p.instance().unwrap(), p.instance().unwrap());
    let img = flat(4, 4, [0.1, 0.1, 0.1, 1.0]);
    let mut v: Vec<Option<Value>> = vec![None; p.params.len()];
    v[scale] = Some(Value::Numbers(vec![3.0]));
    a.render(&img, 0.0, 25.0, &v).unwrap();
    // `b` was never told: still the default.
    assert!(near(at(&b.render(&img, 0.0, 25.0, &[]).unwrap(), 0, 0), [0.1, 0.1, 0.1, 1.0], 1e-6));
    // `a` keeps its 3 without being told again.
    assert!(near(at(&a.render(&img, 0.0, 25.0, &[]).unwrap(), 0, 0), [0.3, 0.3, 0.3, 3.0], 1e-6));
}

#[test]
fn plugins_that_read_other_frames_get_them() {
    let Some(p) = plugin("FrameAverage") else { return };
    assert!(p.temporal && p.gpu.is_empty() && p.cpu);
    let radius = p.params.iter().position(|q| q.name == "radius").unwrap();
    let mut inst = p.instance().unwrap();
    // It says which frames it wants, by its own settings and within the source.
    assert_eq!(inst.frames_needed(5.0, 25.0, (4, 4), &[], (0.0, 100.0)), vec![4.0, 6.0]);
    let mut v: Vec<Option<Value>> = vec![None; p.params.len()];
    v[radius] = Some(Value::Numbers(vec![2.0]));
    assert_eq!(inst.frames_needed(5.0, 25.0, (4, 4), &v, (0.0, 100.0)), vec![3.0, 4.0, 6.0, 7.0]);
    assert_eq!(inst.frames_needed(0.0, 25.0, (4, 4), &v, (0.0, 100.0)), vec![1.0, 2.0], "nothing before the first frame");
    // Rendered with those frames: the average of all five.
    let frames: Vec<Image> = (0..5).map(|k| flat(4, 4, [0.1 * k as f32, 0.0, 0.0, 1.0])).collect();
    let others: Vec<(f64, &Image)> = [(3.0, 0), (4.0, 1), (6.0, 3), (7.0, 4)].iter().map(|&(t, i)| (t, &frames[i])).collect();
    let req = reelforge_ofx::Request { input: &frames[2], time: 5.0, fps: 25.0, values: &v, others: &others, range: (0.0, 100.0) };
    let out = inst.render_with(&req).unwrap();
    assert!(near(at(&out, 1, 1), [0.2, 0.0, 0.0, 1.0], 1e-6), "{:?}", at(&out, 1, 1));
    // A frame the host didn't have: averaged over the rest (here, three).
    let req = reelforge_ofx::Request { others: &others[1..3], ..req };
    let out = inst.render_with(&req).unwrap();
    assert!(near(at(&out, 1, 1), [0.2, 0.0, 0.0, 1.0], 1e-6));
    let req = reelforge_ofx::Request { others: &others[2..], ..req };
    let out = inst.render_with(&req).unwrap();
    assert!(near(at(&out, 1, 1), [0.3, 0.0, 0.0, 1.0], 1e-6), "{:?}", at(&out, 1, 1));
}

#[test]
fn an_opengl_plugin_renders_on_the_graphics_card() {
    let Some(p) = plugin("OpenGL") else { return };
    assert_eq!((p.gpu.clone(), p.cpu), (vec![reelforge_ofx::Gpu::OpenGl], false), "OpenGL only");
    let mut inst = p.instance().unwrap();
    let (w, h) = (320u32, 160u32);
    let mut img = flat(w, h, [0.0; 4]);
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            img.rgba[i..i + 4].copy_from_slice(&[x as f32 / w as f32, y as f32 / h as f32, 0.5, 1.0]);
        }
    }
    match reelforge_ofx::gpu_status() {
        Ok(renderer) => {
            eprintln!("OpenGL: {renderer}");
            let out = inst.render(&img, 0.0, 25.0, &[]).unwrap();
            // It copies the source (a textured quad) and draws a red box from
            // (10, 10) to (200, 100), y up: rows 60–149 from the top.
            assert!(near(at(&out, 100, 100), [1.0, 0.0, 0.0, 1.0], 1e-3), "red box: {:?}", at(&out, 100, 100));
            for (x, y) in [(250, 100), (100, 20), (5, 155)] {
                let want = [x as f32 / w as f32, y as f32 / h as f32, 0.5, 1.0];
                assert!(near(at(&out, x, y), want, 0.01), "({x}, {y}): {:?} vs {want:?}", at(&out, x, y));
            }
            // Again, with the same instance (attached once).
            assert_eq!(inst.render(&img, 1.0, 25.0, &[]).unwrap(), out);
        }
        Err(e) => {
            eprintln!("no OpenGL here ({e}): the plugin must say why it can't render");
            let err = inst.render(&img, 0.0, 25.0, &[]).unwrap_err();
            assert!(err.contains("graphics card"), "{err}");
        }
    }
    // Switched off: it can't render on the processor, and says so.
    reelforge_ofx::set_gpu(false);
    let err = p.instance().unwrap().render(&img, 0.0, 25.0, &[]).unwrap_err();
    reelforge_ofx::set_gpu(true);
    assert!(err.contains("switched off") || err.contains("graphics card"), "{err}");
}
