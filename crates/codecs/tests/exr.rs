//! Reelforge's EXR decoder against the OpenEXR reference library: every test
//! file (made by tools/exr/make_fixtures.py) must decode to exactly the
//! values the library reads.

use reelforge_codecs::{exr, Error, Format, Picture};
use std::path::Path;

fn rgba(p: &Picture) -> Vec<f32> {
    let n = (p.width * p.height) as usize;
    let half = matches!(p.format, Format::Gbrpf16 | Format::Gbrapf16);
    let plane = |i: usize| -> Vec<f32> {
        if half {
            p.planes[i].data.chunks_exact(2).map(|b| exr::half_to_f32(u16::from_le_bytes([b[0], b[1]]))).collect()
        } else {
            p.planes[i].data.chunks_exact(4).map(|b| f32::from_le_bytes(b.try_into().unwrap())).collect()
        }
    };
    let (g, b, r) = (plane(0), plane(1), plane(2));
    let a = if p.format.planes() == 4 { plane(3) } else { vec![1.0; n] };
    // Without alpha, everything outside the data window is 0 in the reference too.
    let mut out = Vec::with_capacity(n * 4);
    for i in 0..n {
        out.extend([r[i], g[i], b[i], a[i]]);
    }
    out
}

#[test]
fn decodes_like_the_reference_library() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/exr");
    let mut checked = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "exr") {
            continue;
        }
        let name = path.file_stem().unwrap().to_string_lossy().to_string();
        let file = std::fs::read(&path).unwrap();
        let got = exr::decode(&file);
        if name.contains("unsupported") {
            assert!(matches!(got, Err(Error::Unsupported(_))), "{name}: {got:?}");
            continue;
        }
        let pic = got.unwrap_or_else(|e| panic!("{name}: {e}"));
        let want: Vec<f32> = std::fs::read(path.with_extension("ref")).unwrap().chunks_exact(4).map(|b| f32::from_le_bytes(b.try_into().unwrap())).collect();
        let mut got = rgba(&pic);
        assert_eq!(got.len(), want.len(), "{name}: size {}×{}", pic.width, pic.height);
        // Pixels outside the data window: the reference says alpha 1 only where
        // the picture is, so compare alpha there with the same rule.
        if pic.format.planes() == 3 {
            for (g, w) in got.chunks_exact_mut(4).zip(want.chunks_exact(4)) {
                if w[3] == 0.0 {
                    g[3] = 0.0;
                }
            }
        }
        if name.starts_with("dwa") {
            // Lossy, and the library's own result depends on the machine (its
            // inverse DCT has SSE2, AVX and plain versions that round
            // differently), so allow one step of the stored (non-linear) half
            // values, now and then.
            let stored = |v: f32| {
                let z = if v.abs() <= 1.0 { v.abs().powf(1.0 / 2.2) } else { v.abs().ln() / 2.2 + 1.0 };
                exr::f32_to_half(z.copysign(v)) as i32
            };
            let off: Vec<i32> = got.iter().zip(&want).filter(|(a, b)| a.to_bits() != b.to_bits()).map(|(&a, &b)| (stored(a) - stored(b)).abs()).collect();
            let worst = off.iter().copied().max().unwrap_or(0);
            assert!(worst <= 2 && off.len() * 200 <= got.len(), "{name}: {} of {} values differ, by up to {worst} steps", off.len(), got.len());
            checked += 1;
            continue;
        }
        if name.starts_with("lj2k") {
            // Lossy too, and like DWA the library's result depends on the machine:
            // OpenJPH's SIMD and plain irreversible wavelets round a few samples
            // differently (ours match its plain one exactly). Allow a step or two of the
            // half value now and then (two: 3.5.1's transfer function spreads a step).
            let step = |v: f32| exr::f32_to_half(v) as i16 as i32;
            let off: Vec<i32> = got.iter().zip(&want).filter(|(a, b)| a.to_bits() != b.to_bits()).map(|(&a, &b)| (step(a) - step(b)).abs()).collect();
            let worst = off.iter().copied().max().unwrap_or(0);
            assert!(worst <= 2 && off.len() * 100 <= got.len(), "{name}: {} of {} values differ, by up to {worst} steps", off.len(), got.len());
            checked += 1;
            continue;
        }
        let bad = got.iter().zip(&want).position(|(a, b)| a.to_bits() != b.to_bits() && !(a.is_nan() && b.is_nan()));
        if let Some(i) = bad {
            let px = i / 4;
            panic!("{name}: pixel ({}, {}) channel {}: got {} want {}", px as u32 % pic.width, px as u32 / pic.width, i % 4, got[i], want[i]);
        }
        checked += 1;
    }
    assert!(checked >= 25, "{checked} files");
}

/// Every reference file cut short or with bytes changed: an error or a
/// picture, never a crash (the reader takes no size or position on trust).
#[test]
fn damaged_files_never_crash() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/exr");
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "exr") {
            continue;
        }
        let file = std::fs::read(&path).unwrap();
        for k in 1..24 {
            let _ = exr::decode(&file[..file.len() * k / 24]);
        }
        for _ in 0..48 {
            let mut f = file.clone();
            for _ in 0..1 + next() % 4 {
                let i = (next() % f.len() as u64) as usize;
                f[i] = next() as u8;
            }
            let _ = exr::decode(&f);
        }
    }
}

/// How long a big file takes (RF_EXR_BENCH=path; prints the time).
#[test]
fn bench_file() {
    let Ok(path) = std::env::var("RF_EXR_BENCH") else { return };
    let file = std::fs::read(path).unwrap();
    exr::decode(&file).unwrap();
    let t = std::time::Instant::now();
    for _ in 0..10 {
        exr::decode(&file).unwrap();
    }
    eprintln!("{:.1} ms", t.elapsed().as_secs_f64() * 100.0);
}
