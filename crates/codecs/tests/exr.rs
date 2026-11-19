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
        let bad = got.iter().zip(&want).position(|(a, b)| a.to_bits() != b.to_bits() && !(a.is_nan() && b.is_nan()));
        if let Some(i) = bad {
            let px = i / 4;
            panic!("{name}: pixel ({}, {}) channel {}: got {} want {}", px as u32 % pic.width, px as u32 / pic.width, i % 4, got[i], want[i]);
        }
        checked += 1;
    }
    assert!(checked >= 18, "{checked} files");
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
