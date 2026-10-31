//! Plugins that render with OpenCL and CUDA.
//!
//! OpenCL runs for real, on whatever OpenCL the machine has (PoCL, OpenCL on
//! the processor, does): the test plugin builds and runs its own kernels on
//! the host's queue, with buffers and with images.
//!
//! CUDA needs an NVIDIA card, which test machines don't have, so the host
//! loads a stand-in driver (`plugins/fakecuda.c`, through
//! `REELFORGE_CUDA_DRIVER`) whose "device memory" is ordinary memory, and the
//! test plugin checks it's given device memory, a current context and the
//! host's stream. That covers the host's side; real kernels on a real card
//! haven't been run.

use reelforge_ofx::{load, Gpu, Image, Plugin, Value};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

/// Build the plugins and the stand-in driver once, and point the host at it
/// before anything looks for CUDA.
fn setup() -> Option<&'static Path> {
    static DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
    DIR.get_or_init(|| {
        if !cfg!(target_os = "linux") {
            return None;
        }
        let tests = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
        let out = std::env::temp_dir().join(format!("rf_ofx_gpu_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&out);
        std::fs::create_dir_all(&out).ok()?;
        let driver = out.join("libfakecuda.so");
        let ok = std::process::Command::new("cc").args(["-O1", "-shared", "-fPIC", "-o"]).arg(&driver).arg(tests.join("plugins/fakecuda.c")).status().is_ok_and(|s| s.success());
        if !ok {
            eprintln!("no C compiler: skipping");
            return None;
        }
        // SAFETY: set before any thread of this test binary reads it (the
        // tests all come through here first, and the host reads it once).
        unsafe { std::env::set_var("REELFORGE_CUDA_DRIVER", &driver) };
        let builds: [(&str, &str, &[&str]); 4] = [
            ("CLGain", "clgain", &["-DRF_IMAGES=0", "-DRF_CPU=1", "-lOpenCL"]),
            ("CLGainOnly", "clgain", &["-DRF_IMAGES=0", "-DRF_CPU=0", "-lOpenCL"]),
            ("CLGainImages", "clgain", &["-DRF_IMAGES=1", "-DRF_CPU=0", "-lOpenCL"]),
            ("CudaGain", "cudagain", &["-ldl"]),
        ];
        for (name, src, flags) in builds {
            let bin = out.join(format!("{name}.ofx.bundle/Contents/Linux-x86-64"));
            std::fs::create_dir_all(&bin).ok()?;
            let ok = std::process::Command::new("c++")
                .args(["-O1", "-shared", "-fPIC", "-w", "-I"])
                .arg(tests.join("sdk/include"))
                .arg("-o")
                .arg(bin.join(format!("{name}.ofx")))
                .arg(tests.join(format!("plugins/{src}.cpp")))
                .args(flags)
                .status()
                .is_ok_and(|s| s.success());
            if !ok {
                eprintln!("couldn't build {name} (no C++ compiler or OpenCL headers): skipping");
                return None;
            }
        }
        Some(out)
    })
    .as_deref()
}

/// The tests switch the graphics card off and on: one at a time.
fn turn() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

fn plugin(name: &str) -> Option<Arc<Plugin>> {
    let dir = setup()?;
    // SAFETY: test plugins built just now.
    let (mut plugins, errors) = unsafe { load(&dir.join(format!("{name}.ofx.bundle"))) }.unwrap();
    assert!(errors.is_empty(), "{errors:?}");
    Some(Arc::new(plugins.remove(0)))
}

/// A gradient, so a mix-up of rows or channels would show.
fn picture(w: u32, h: u32) -> Image {
    let mut rgba = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            rgba.extend([x as f32 / w as f32, y as f32 / h as f32, 0.25, 0.5]);
        }
    }
    Image { width: w, height: h, rgba }
}

fn times(img: &Image, k: f32) -> Vec<f32> {
    img.rgba.chunks_exact(4).flat_map(|p| [p[0] * k, p[1] * k, p[2] * k, p[3]]).collect()
}

fn close(a: &[f32], b: &[f32]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-5)
}

#[test]
fn opencl_plugins_run_their_kernels_on_the_hosts_queue() {
    let _t = turn();
    let Some(only) = plugin("CLGainOnly") else { return };
    let images = plugin("CLGainImages").unwrap();
    let either = plugin("CLGain").unwrap();
    assert_eq!((only.gpu.clone(), only.cpu), (vec![Gpu::OpenCl], false));
    assert_eq!((either.gpu.clone(), either.cpu), (vec![Gpu::OpenCl], true));
    let img = picture(37, 21);
    let gain = only.params.iter().position(|q| q.name == "gain").unwrap();
    let mut v: Vec<Option<Value>> = vec![None; only.params.len()];
    v[gain] = Some(Value::Numbers(vec![3.0]));
    match reelforge_ofx::cl::available() {
        Ok((device, on_gpu)) => {
            eprintln!("OpenCL: {device}");
            // Buffers, then images, each with its kernel on the host's queue.
            for p in [&only, &images] {
                let mut inst = p.instance().unwrap();
                let out = inst.render(&img, 0.0, 25.0, &v).unwrap();
                assert!(close(&out.rgba, &times(&img, 3.0)), "{}", p.label);
                assert!(inst.rendered_with().unwrap().starts_with("OpenCL on "), "{:?}", inst.rendered_with());
                // Again (the kernel kept per context).
                assert!(close(&inst.render(&img, 1.0, 25.0, &v).unwrap().rgba, &times(&img, 3.0)));
            }
            // One that can use the processor too: OpenCL only on a graphics card.
            let mut inst = either.instance().unwrap();
            let out = inst.render(&img, 0.0, 25.0, &v).unwrap();
            assert!(close(&out.rgba, &times(&img, 3.0)));
            let with = inst.rendered_with().unwrap();
            assert_eq!(with.starts_with("OpenCL"), on_gpu, "{with}");
        }
        Err(e) => {
            eprintln!("no OpenCL here ({e})");
            let err = only.instance().unwrap().render(&img, 0.0, 25.0, &v).unwrap_err();
            assert!(err.contains("OpenCL"), "{err}");
        }
    }
    // The graphics card switched off: the OpenCL-only one says why; the other uses the processor.
    reelforge_ofx::set_gpu(false);
    let err = only.instance().unwrap().render(&img, 0.0, 25.0, &v).unwrap_err();
    let mut inst = either.instance().unwrap();
    let out = inst.render(&img, 0.0, 25.0, &v);
    reelforge_ofx::set_gpu(true);
    assert!(err.contains("switched off"), "{err}");
    assert!(close(&out.unwrap().rgba, &times(&img, 3.0)));
    assert_eq!(inst.rendered_with().as_deref(), Some("the processor"));
}

#[test]
fn cuda_plugins_get_device_memory_a_context_and_the_hosts_stream() {
    let _t = turn();
    let Some(p) = plugin("CudaGain") else { return };
    assert_eq!(p.gpu, vec![Gpu::Cuda]);
    assert_eq!(reelforge_ofx::cuda::available().as_deref(), Ok("Reelforge stand-in CUDA device"));
    // SAFETY: the stand-in driver the host loaded (the same library, so the same counts).
    let driver = unsafe { libloading::Library::new(std::env::var_os("REELFORGE_CUDA_DRIVER").unwrap()) }.unwrap();
    let stats = || {
        let (mut a, mut f, mut s) = (0, 0, 0);
        // SAFETY: a function of the stand-in driver, writing three ints.
        unsafe { driver.get::<unsafe extern "C" fn(*mut i32, *mut i32, *mut i32)>(b"rf_fake_cuda_stats\0").unwrap()(&mut a, &mut f, &mut s) };
        (a, f, s)
    };
    let before = stats();
    let img = picture(16, 9);
    let mut inst = p.instance().unwrap();
    // With other frames too: they go to the device as well.
    let other = picture(16, 9);
    let req = reelforge_ofx::Request { input: &img, time: 5.0, fps: 25.0, values: &[], others: &[(4.0, &other)], range: (0.0, 10.0) };
    let out = inst.render_with(&req).unwrap();
    assert!(close(&out.rgba, &times(&img, 2.0)));
    assert_eq!(inst.rendered_with().as_deref(), Some("CUDA on Reelforge stand-in CUDA device"));
    let after = stats();
    assert_eq!(after.0 - before.0, 3, "source, the other frame and the output");
    assert_eq!(after.1 - before.1, 3, "all freed");
    assert!(after.2 > before.2, "waited for the stream");
    // Switched off: it renders on the processor (it can).
    reelforge_ofx::set_gpu(false);
    let out = inst.render(&img, 1.0, 25.0, &[]);
    reelforge_ofx::set_gpu(true);
    assert!(close(&out.unwrap().rgba, &times(&img, 2.0)));
    assert_eq!(inst.rendered_with().as_deref(), Some("the processor"));
    assert_eq!(stats().0, after.0, "nothing on the device");
}

#[test]
fn the_host_says_what_it_offers() {
    let _t = turn();
    if setup().is_none() {
        return;
    }
    let report = reelforge_ofx::gpu_report();
    let get = |g: Gpu| report.iter().find(|r| r.0 == g).unwrap().1.clone();
    assert!(get(Gpu::Cuda).is_ok());
    assert_eq!(get(Gpu::Metal), Err("only on a Mac".into()));
}
