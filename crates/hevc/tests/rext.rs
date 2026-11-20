//! The range-extension tools (cross-component prediction, implicit / explicit residual DPCM,
//! transform skip rotation, context and larger sizes, persistent Rice adaptation, extended
//! precision, CABAC bypass alignment, chroma QP offset lists, SAO offset scaling,
//! high-precision weights, no intra smoothing) against the HEVC reference software: streams
//! made by HM (tools/hevc_rext/make_fixtures.sh) and HM's own decoding of them, as a hash per
//! frame.

mod common;

use reelforge_bitstream::{annexb_nals, unescape_rbsp};
use reelforge_hevc::params::{Pps, PpsRext, Sps, SpsRext};
use std::path::Path;

fn fnv(bytes: impl Iterator<Item = u8>) -> u64 {
    bytes.fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01b3))
}

/// A plane's samples without stride padding, 16-bit LE above 8 bits.
fn plane_bytes(p: &reelforge_hevc::Plane, w: usize, h: usize, stride: usize, wide: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(w * h * 2);
    for y in 0..h {
        for x in 0..w {
            let v = p.get(y * stride + x);
            if wide {
                out.extend(v.to_le_bytes());
            } else {
                out.push(v as u8);
            }
        }
    }
    out
}

#[test]
fn range_extension_tools_match_the_reference_decoder() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/rext");
    let mut names: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().path()).filter(|p| p.extension().is_some_and(|e| e == "bit")).collect();
    names.sort();
    let mut bad = Vec::new();
    // Which tools the streams turn on, together.
    let (mut sps_tools, mut pps_tools) = (SpsRext::default(), PpsRext::default());
    for path in &names {
        let name = path.file_stem().unwrap().to_string_lossy().to_string();
        for nal in annexb_nals(&std::fs::read(path).unwrap()) {
            let rbsp = unescape_rbsp(&nal[2..]);
            match (nal[0] >> 1) & 63 {
                33 => {
                    let r = Sps::parse(&rbsp).unwrap().rext;
                    let t = &mut sps_tools;
                    t.transform_skip_rotation |= r.transform_skip_rotation;
                    t.transform_skip_context |= r.transform_skip_context;
                    t.implicit_rdpcm |= r.implicit_rdpcm;
                    t.explicit_rdpcm |= r.explicit_rdpcm;
                    t.extended_precision |= r.extended_precision;
                    t.intra_smoothing_disabled |= r.intra_smoothing_disabled;
                    t.high_precision_offsets |= r.high_precision_offsets;
                    t.persistent_rice_adaptation |= r.persistent_rice_adaptation;
                    t.cabac_bypass_alignment |= r.cabac_bypass_alignment;
                }
                34 => {
                    let r = Pps::parse(&rbsp).unwrap().rext;
                    let t = &mut pps_tools;
                    t.log2_max_ts = t.log2_max_ts.max(r.log2_max_ts);
                    t.cross_component_prediction |= r.cross_component_prediction;
                    if r.chroma_qp_offset_list.is_some() {
                        t.chroma_qp_offset_list = r.chroma_qp_offset_list;
                    }
                    t.log2_sao_offset_scale = [t.log2_sao_offset_scale[0].max(r.log2_sao_offset_scale[0]), t.log2_sao_offset_scale[1].max(r.log2_sao_offset_scale[1])];
                }
                _ => {}
            }
        }
        let want: Vec<String> = std::fs::read_to_string(path.with_extension("hash")).unwrap().lines().map(str::to_string).collect();
        let pics = match common::decode_file_threads(path, 1) {
            Ok(p) => p,
            Err((i, e, _)) => {
                bad.push(format!("{name}: access unit {i}: {e}"));
                continue;
            }
        };
        if pics.len() != want.len() {
            bad.push(format!("{name}: {} pictures, want {}", pics.len(), want.len()));
            continue;
        }
        for (i, (p, w)) in pics.iter().zip(&want).enumerate() {
            let wide = p.bit_depth > 8;
            let (pw, ph, cw, ch) = (p.width as usize, p.height as usize, p.chroma_width as usize, p.chroma_height as usize);
            let mut bytes = plane_bytes(&p.y, pw, ph, p.y_stride, wide);
            bytes.extend(plane_bytes(&p.u, cw, ch, p.uv_stride, wide));
            bytes.extend(plane_bytes(&p.v, cw, ch, p.uv_stride, wide));
            let got = format!("{:016x}", fnv(bytes.into_iter()));
            if &got != w {
                bad.push(format!("{name}: frame {i} differs"));
                break;
            }
        }
        eprintln!("{name}: checked {} frames", pics.len());
    }
    assert!(names.len() >= 9, "{} streams", names.len());
    let all = SpsRext {
        transform_skip_rotation: true,
        transform_skip_context: true,
        implicit_rdpcm: true,
        explicit_rdpcm: true,
        extended_precision: true,
        intra_smoothing_disabled: true,
        high_precision_offsets: true,
        persistent_rice_adaptation: true,
        cabac_bypass_alignment: true,
    };
    assert_eq!(sps_tools, all, "not every SPS tool is in the test streams");
    assert!(pps_tools.log2_max_ts == 5 && pps_tools.cross_component_prediction && pps_tools.chroma_qp_offset_list.is_some(), "{pps_tools:?}");
    assert!(pps_tools.log2_sao_offset_scale[0] > 0 && pps_tools.log2_sao_offset_scale[1] > 0, "{pps_tools:?}");
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// The range-extension streams damaged: errors or pictures, never a crash.
#[test]
fn damaged_range_extension_streams_never_crash() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/rext");
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "bit") {
            continue;
        }
        let data = std::fs::read(&path).unwrap();
        for _ in 0..24 {
            let mut d = data.clone();
            for _ in 0..1 + next() % 6 {
                let i = (next() % d.len() as u64) as usize;
                d[i] = next() as u8;
            }
            let mut dec = reelforge_hevc::Decoder::with_threads(1);
            for au in common::split_access_units(&d) {
                let _ = dec.decode(au, 0);
            }
            let _ = dec.flush();
        }
    }
}
