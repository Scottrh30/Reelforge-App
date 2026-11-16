//! Field pictures (PAFF), picture-adaptive frame/field coding and MBAFF with temporal direct:
//! JM-encoded streams (`tests/data/jm`) must decode exactly as FFmpeg decodes them.

use reelforge_h264::{Decoder, Picture};
use std::path::Path;

fn fnv(data: &[u8], mut h: u64) -> u64 {
    for &b in data {
        h = (h ^ b as u64).wrapping_mul(0x100000001b3);
    }
    h
}

fn hash(pics: &[Picture]) -> u64 {
    pics.iter().fold(0xcbf29ce484222325, |h, p| {
        let h = fnv(&p.y.to_le_bytes(), h);
        let h = fnv(&p.u.to_le_bytes(), h);
        fnv(&p.v.to_le_bytes(), h)
    })
}

/// The NAL units of an Annex-B stream, start codes included.
fn nals(d: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= d.len() {
        if d[i..i + 3] == [0, 0, 1] {
            starts.push(if i > 0 && d[i - 1] == 0 { i - 1 } else { i });
            i += 3;
        } else {
            i += 1;
        }
    }
    starts.iter().enumerate().map(|(k, &s)| &d[s..starts.get(k + 1).copied().unwrap_or(d.len())]).collect()
}

/// The stream cut into access units (each field picture its own): a new one at every slice
/// with first_mb_in_slice 0 (its first bit is 1), parameter sets going with the next one.
fn access_units(d: &[u8]) -> Vec<Vec<u8>> {
    let mut out: Vec<Vec<u8>> = Vec::new();
    let mut cur = Vec::new();
    let mut has_slice = false;
    for n in nals(d) {
        let body = &n[n.iter().position(|&b| b == 1).unwrap() + 1..];
        let t = body[0] & 0x1f;
        let first = matches!(t, 1 | 5) && body.get(1).is_some_and(|b| b & 0x80 != 0);
        if (first || t == 7) && has_slice {
            out.push(std::mem::take(&mut cur));
            has_slice = false;
        }
        has_slice |= matches!(t, 1 | 5);
        cur.extend_from_slice(n);
    }
    out.push(cur);
    out
}

#[test]
fn jm_interlaced_streams_match_ffmpeg() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/jm");
    let expected = std::fs::read_to_string(dir.join("expected.txt")).unwrap();
    let mut checked = 0;
    for line in expected.lines().filter(|l| !l.starts_with('#') && !l.trim().is_empty()) {
        let f: Vec<&str> = line.split_whitespace().collect();
        let (name, frames, want) = (f[0], f[1].parse::<usize>().unwrap(), u64::from_str_radix(f[2], 16).unwrap());
        let data = std::fs::read(dir.join(format!("{name}.264"))).unwrap();
        for threads in [1, 4] {
            // the whole stream at once, and one access unit per call (fields arrive apart)
            let mut dec = Decoder::with_threads(threads);
            let mut pics = dec.decode(&data, 0).unwrap();
            pics.extend(dec.flush());
            assert_eq!((pics.len(), hash(&pics)), (frames, want), "{name}, {threads} threads");
            let mut dec = Decoder::with_threads(threads);
            let mut pics = Vec::new();
            for (i, au) in access_units(&data).iter().enumerate() {
                pics.extend(dec.decode(au, i as i64).unwrap());
            }
            pics.extend(dec.flush());
            assert_eq!((pics.len(), hash(&pics)), (frames, want), "{name}, {threads} threads, by access unit");
            assert!(dec.take_error().is_none(), "{name}");
        }
        checked += 1;
    }
    assert_eq!(checked, 8);
}

/// A first field whose second field never comes (the stream cut after it) still comes out,
/// as a frame of its own lines, and nothing waits on the missing field.
#[test]
fn unpaired_field_is_output() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/jm");
    let data = std::fs::read(dir.join("fields_temporal_cabac.264")).unwrap();
    let units = nals(&data);
    // SPS, PPS, then the IDR's top field only
    let mut dec = Decoder::with_threads(4);
    let mut pics = Vec::new();
    for n in &units[..3] {
        pics.extend(dec.decode(n, 0).unwrap());
    }
    pics.extend(dec.flush());
    assert_eq!(pics.len(), 1);
    let p = &pics[0];
    assert_eq!((p.width, p.height), (176, 144));
    // both lines of each pair are the field's line
    let y = p.y.to_le_bytes();
    assert_eq!(&y[..176], &y[176..352]);
}
