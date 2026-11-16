//! Inputs the fuzzer found that once crashed, hung or ran away with memory
//! (`fuzz/regressions/<target>/*`). Each is fed to its decoder the way `fuzz/src/lib.rs` does;
//! the test fails if any panics or takes more than 20 s (the hangs). Run the fuzzers with
//! `fuzz/run.sh` (see `fuzz/README.md`).

use reelforge_codecs::{Codec, StreamInfo};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Panics anywhere, including on decoder threads that catch them and carry on (the fuzzer
/// counts those as crashes too).
static PANICS: AtomicUsize = AtomicUsize::new(0);

const MAX_PACKETS: usize = 48;

fn records(data: &[u8]) -> Option<(u32, u32, Vec<&[u8]>)> {
    let (w, rest) = data.split_first_chunk::<2>()?;
    let (h, mut rest) = rest.split_first_chunk::<2>()?;
    let mut out = Vec::new();
    while out.len() <= MAX_PACKETS {
        let Some((len, tail)) = rest.split_first_chunk::<4>() else { break };
        let n = (u32::from_le_bytes(*len) as usize).min(tail.len());
        out.push(&tail[..n]);
        rest = &tail[n..];
    }
    Some((u32::from(u16::from_le_bytes(*w)), u32::from(u16::from_le_bytes(*h)), out))
}

fn run(codec: Codec, data: &[u8]) {
    let Some((width, height, recs)) = records(data) else { return };
    let mut it = recs.into_iter();
    let extradata = it.next().unwrap_or(&[]).to_vec();
    let Some(mut dec) = reelforge_codecs::open(codec, &StreamInfo { width, height, extradata }) else { return };
    for (i, p) in it.enumerate() {
        let _ = dec.send(p, i as i64);
        while dec.receive().is_some() {}
        if i % 17 == 16 {
            dec.flush();
        }
    }
    dec.finish();
    while dec.receive().is_some() {}
}

fn codec(target: &str) -> Option<Codec> {
    Some(match target {
        "mjpeg" => Codec::Mjpeg,
        "prores" => Codec::ProRes,
        "dnxhd" => Codec::Dnxhd,
        "mpeg2" => Codec::Mpeg2,
        "h264" => Codec::H264,
        "hevc" => Codec::Hevc,
        "vp8" => Codec::Vp8,
        "vp9" => Codec::Vp9,
        "av1" => Codec::Av1,
        "cineform" => Codec::Cineform,
        "exr" => Codec::Exr,
        _ => return None,
    })
}

#[test]
fn fuzz_finds_no_longer_crash() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fuzz/regressions");
    let Ok(dirs) = std::fs::read_dir(&root) else { return };
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        PANICS.fetch_add(1, Ordering::Relaxed);
        default(info);
    }));
    let mut failed = Vec::new();
    let mut ran = 0;
    for dir in dirs.flatten() {
        let name = dir.file_name().to_string_lossy().into_owned();
        let Some(c) = codec(&name) else { continue };
        if !c.available() {
            continue;
        }
        let Ok(files) = std::fs::read_dir(dir.path()) else { continue };
        for f in files.flatten() {
            let Ok(data) = std::fs::read(f.path()) else { continue };
            ran += 1;
            let t0 = std::time::Instant::now();
            let name = format!("{name}/{}", f.file_name().to_string_lossy());
            let before = PANICS.load(Ordering::Relaxed);
            if std::panic::catch_unwind(|| run(c, &data)).is_err() || PANICS.load(Ordering::Relaxed) != before {
                failed.push(name);
            } else if t0.elapsed().as_secs() > 20 {
                failed.push(format!("{name} (took {:.0?})", t0.elapsed()));
            }
        }
    }
    assert!(failed.is_empty(), "{ran} inputs, these failed: {failed:?}");
}
