//! 4:4:4 with separately coded colour planes (separate_colour_plane_flag), checked bit for bit.
//!
//! No encoder at hand makes these (and FFmpeg's decoder refuses them), but each colour plane of
//! such a stream is coded exactly as a monochrome picture is. So three monochrome libx264
//! streams with the same picture structure (one of a source's Y, one of its Cb, one of its Cr)
//! are put together into one: its SPS made 4:4:4 with separate planes, each plane's slices
//! given their colour_plane_id (and a PPS of their own). FFmpeg's decode of each monochrome
//! stream is then what each plane must be.

mod common;

use reelforge_h264::params::{Pps, Sps};
use reelforge_h264::slice::{NalHeader, SliceHeader};
use reelforge_h264::{Decoder, Plane};
use std::path::PathBuf;
use std::process::Command;

struct Bits<'a> {
    d: &'a [u8],
    pos: usize,
}

impl Bits<'_> {
    fn bit(&mut self) -> u32 {
        let b = (self.d[self.pos / 8] >> (7 - self.pos % 8)) & 1;
        self.pos += 1;
        b as u32
    }
    fn bits(&mut self, n: usize) -> u32 {
        (0..n).fold(0, |v, _| (v << 1) | self.bit())
    }
    fn ue(&mut self) -> u32 {
        let mut zeros = 0;
        while self.bit() == 0 {
            zeros += 1;
        }
        ((1u32 << zeros) - 1) + self.bits(zeros)
    }
}

#[derive(Default)]
struct Writer {
    out: Vec<u8>,
    n: usize,
}

impl Writer {
    fn bit(&mut self, b: u32) {
        if self.n.is_multiple_of(8) {
            self.out.push(0);
        }
        if b != 0 {
            let last = self.out.len() - 1;
            self.out[last] |= 0x80 >> (self.n % 8);
        }
        self.n += 1;
    }
    fn bits(&mut self, v: u32, n: usize) {
        for i in (0..n).rev() {
            self.bit((v >> i) & 1);
        }
    }
    fn ue(&mut self, v: u32) {
        let x = v + 1;
        let len = 32 - x.leading_zeros() as usize;
        self.bits(0, len - 1);
        self.bits(x, len);
    }
    /// Bits `from..to` of `d`.
    fn copy(&mut self, d: &[u8], from: usize, to: usize) {
        let mut r = Bits { d, pos: from };
        while r.pos < to {
            let b = r.bit();
            self.bit(b);
        }
    }
    /// rbsp_trailing_bits.
    fn trail(mut self) -> Vec<u8> {
        self.bit(1);
        while !self.n.is_multiple_of(8) {
            self.bit(0);
        }
        self.out
    }
}

/// Position of the rbsp stop bit (the last 1).
fn stop_bit(d: &[u8]) -> usize {
    let Some(i) = d.iter().rposition(|&b| b != 0) else { return 0 };
    i * 8 + 7 - d[i].trailing_zeros() as usize
}

fn unescape(d: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(d.len());
    let mut zeros = 0;
    for &b in d {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue;
        }
        zeros = if b == 0 { zeros + 1 } else { 0 };
        out.push(b);
    }
    out
}

fn escape(d: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(d.len() + 8);
    let mut zeros = 0;
    for &b in d {
        if zeros >= 2 && b <= 3 {
            out.push(3);
            zeros = 0;
        }
        zeros = if b == 0 { zeros + 1 } else { 0 };
        out.push(b);
    }
    out
}

/// NAL units (without start codes) of an Annex-B stream.
fn nals(d: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= d.len() {
        if d[i] == 0 && d[i + 1] == 0 && d[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut out = Vec::new();
    for (k, &s) in starts.iter().enumerate() {
        let mut e = starts.get(k + 1).map_or(d.len(), |&n| n - 3);
        while e > s && d[e - 1] == 0 {
            e -= 1;
        }
        out.push(&d[s..e]);
    }
    out
}

/// The monochrome SPS made 4:4:4 with separately coded planes (High 4:4:4 Predictive).
fn separate_sps(rbsp: &[u8]) -> Vec<u8> {
    let mut r = Bits { d: rbsp, pos: 0 };
    let profile = r.bits(8);
    assert!(matches!(profile, 100 | 110), "a High or High 10 SPS");
    let constraints = r.bits(8);
    let level = r.bits(8);
    let id = r.ue();
    assert_eq!(r.ue(), 0, "monochrome");
    let depth = r.ue();
    let _ = r.ue();
    let bypass = r.bit();
    assert_eq!(r.bit(), 0, "no scaling matrices in the SPS");
    let mut w = Writer::default();
    w.bits(244, 8);
    w.bits(constraints, 8);
    w.bits(level, 8);
    w.ue(id);
    w.ue(3);
    w.bit(1);
    w.ue(depth);
    w.ue(depth);
    w.bit(bypass);
    w.bit(0);
    w.copy(rbsp, r.pos, stop_bit(rbsp));
    w.trail()
}

/// The PPS with id `id`.
fn renumber_pps(rbsp: &[u8], id: u32) -> Vec<u8> {
    let mut r = Bits { d: rbsp, pos: 0 };
    let _ = r.ue();
    let mut w = Writer::default();
    w.ue(id);
    w.copy(rbsp, r.pos, stop_bit(rbsp));
    w.trail()
}

/// The slice for colour plane `plane` (using PPS `plane`).
fn plane_slice(rbsp: &[u8], hdr: NalHeader, sps: &Sps, pps: &Pps, plane: u32) -> Vec<u8> {
    let (sh, _, _) = SliceHeader::parse(rbsp, hdr, |_| Ok((pps, sps))).expect("slice header");
    let end = sh.header_bits;
    let mut r = Bits { d: rbsp, pos: 0 };
    let (first, kind) = (r.ue(), r.ue());
    let _ = r.ue();
    let mut w = Writer::default();
    w.ue(first);
    w.ue(kind);
    w.ue(plane);
    w.bits(plane, 2);
    if pps.entropy_coding_mode {
        // the header, then cabac_alignment_one_bits and the byte-aligned slice data
        w.copy(rbsp, r.pos, end);
        while !w.n.is_multiple_of(8) {
            w.bit(1);
        }
        w.out.extend_from_slice(&rbsp[end.div_ceil(8)..]);
        w.out
    } else {
        w.copy(rbsp, r.pos, stop_bit(rbsp));
        w.trail()
    }
}

struct Case {
    name: &'static str,
    width: u32,
    height: u32,
    frames: u32,
    ten_bit: bool,
    x264: &'static str,
}

const CASES: &[Case] = &[
    Case { name: "sep_intra", width: 352, height: 288, frames: 3, ten_bit: false, x264: "keyint=1" },
    Case { name: "sep_cabac_b", width: 352, height: 288, frames: 14, ten_bit: false, x264: "keyint=30:scenecut=0:b-adapt=0:bframes=2:b-pyramid=none:weightp=0" },
    Case { name: "sep_cavlc", width: 320, height: 240, frames: 12, ten_bit: false, x264: "cabac=0:keyint=30:scenecut=0:b-adapt=0:bframes=2:b-pyramid=none:weightp=0" },
    Case { name: "sep_slices", width: 352, height: 288, frames: 8, ten_bit: false, x264: "slices=3:keyint=30:scenecut=0:b-adapt=0:bframes=1:b-pyramid=none:weightp=0" },
    Case { name: "sep_10bit_crop", width: 318, height: 238, frames: 8, ten_bit: true, x264: "keyint=30:scenecut=0:b-adapt=0:bframes=2:b-pyramid=none:weightp=0" },
];

/// The three monochrome streams (Y, Cb, Cr of the source) and FFmpeg's decode of each.
fn monochrome_streams(c: &Case) -> Option<Vec<(PathBuf, PathBuf)>> {
    let ff = reelforge_testkit::ffmpeg_or_skip(c.name)?;
    let dir = common::fixtures_dir();
    let (src, gray, reference) = if c.ten_bit { ("yuv444p10le", "gray10le", "yuv420p10le") } else { ("yuv444p", "gray", "yuvj420p") };
    let mut out = Vec::new();
    for p in ["y", "u", "v"] {
        let (h264, yuv) = (dir.join(format!("{}_{p}.h264", c.name)), dir.join(format!("{}_{p}.yuv", c.name)));
        if !h264.exists() || !yuv.exists() {
            let vf = format!("testsrc2=size={}x{}:rate=25,format=yuv444p,noise=alls=12:allf=t+u,format={src},extractplanes={p},format={gray}", c.width, c.height);
            let tmp = reelforge_testkit::temp_path(&h264);
            let ok = Command::new(&ff)
                .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", &vf, "-frames:v", &c.frames.to_string(), "-c:v", "libx264", "-pix_fmt", gray, "-qp", "24", "-x264-params", c.x264, "-f", "h264"])
                .arg(&tmp)
                .status()
                .is_ok_and(|s| s.success());
            assert!(ok, "making {}", h264.display());
            std::fs::rename(&tmp, &h264).unwrap();
            let tmp = reelforge_testkit::temp_path(&yuv);
            let ok = Command::new(&ff).args(["-hide_banner", "-loglevel", "error", "-y", "-i"]).arg(&h264).args(["-fps_mode", "passthrough", "-f", "rawvideo", "-pix_fmt", reference]).arg(&tmp).status().is_ok_and(|s| s.success());
            assert!(ok, "decoding {}", h264.display());
            std::fs::rename(&tmp, &yuv).unwrap();
        }
        out.push((h264, yuv));
    }
    Some(out)
}

/// The access units of the stream with separate planes.
fn combine(streams: &[Vec<u8>], keep: [bool; 3]) -> Vec<Vec<u8>> {
    let units: Vec<Vec<&[u8]>> = streams.iter().map(|s| common::split_access_units(s)).collect();
    assert!(units.iter().all(|u| u.len() == units[0].len()), "the planes' streams have the same pictures");
    let mut sps: Option<Sps> = None;
    let mut ppss: Vec<Option<Pps>> = vec![None, None, None];
    let mut aus = Vec::new();
    for i in 0..units[0].len() {
        let mut au = Vec::new();
        let mut put = |rbsp_with_header: Vec<u8>| {
            au.extend_from_slice(&[0, 0, 0, 1]);
            au.extend_from_slice(&escape(&rbsp_with_header));
        };
        // Parameter sets first (the SPS from the Y stream; each plane's PPS as its own id).
        for (k, u) in units.iter().enumerate() {
            for nal in nals(u[i]) {
                let hdr = NalHeader::parse(nal[0]).unwrap();
                let rbsp = unescape(&nal[1..]);
                match hdr.nal_unit_type {
                    7 if k == 0 => {
                        sps = Some(Sps::parse(&rbsp).unwrap());
                        put([vec![nal[0]], separate_sps(&rbsp)].concat());
                    }
                    8 => {
                        let spss: Vec<Option<Sps>> = (0..32).map(|_| sps.clone()).collect();
                        ppss[k] = Some(Pps::parse(&rbsp, &spss).unwrap());
                        put([vec![nal[0]], renumber_pps(&rbsp, k as u32)].concat());
                    }
                    _ => {}
                }
            }
        }
        // Then the slices, plane by plane.
        for (k, u) in units.iter().enumerate() {
            for nal in nals(u[i]) {
                let hdr = NalHeader::parse(nal[0]).unwrap();
                if matches!(hdr.nal_unit_type, 1 | 5) && keep[k] {
                    let (s, p) = (sps.as_ref().unwrap(), ppss[k].as_ref().unwrap());
                    let rbsp = unescape(&nal[1..]);
                    put([vec![nal[0]], plane_slice(&rbsp, hdr, s, p, k as u32)].concat());
                }
            }
        }
        aus.push(au);
    }
    aus
}

fn samples(p: &Plane) -> Vec<u16> {
    match p {
        Plane::U8(v) => v.iter().map(|&x| x as u16).collect(),
        Plane::U16(v) => v.clone(),
    }
}

#[test]
fn separate_colour_planes_decode_exactly() {
    let mut ran = 0;
    for c in CASES {
        let Some(files) = monochrome_streams(c) else { return };
        let streams: Vec<Vec<u8>> = files.iter().map(|(h, _)| std::fs::read(h).unwrap()).collect();
        let aus = combine(&streams, [true; 3]);
        let (w, h) = (c.width as usize, c.height as usize);
        let bps = if c.ten_bit { 2 } else { 1 };
        let fsize = (w * h + 2 * w.div_ceil(2) * h.div_ceil(2)) * bps;
        let refs: Vec<Vec<u8>> = files.iter().map(|(_, y)| std::fs::read(y).unwrap()).collect();
        for threads in [1, 3, 0] {
            let mut dec = if threads == 0 { Decoder::new() } else { Decoder::with_threads(threads) };
            let mut pics = Vec::new();
            for (i, au) in aus.iter().enumerate() {
                pics.extend(dec.decode(au, i as i64).unwrap_or_else(|e| panic!("{}: access unit {i}: {e}", c.name)));
            }
            pics.extend(dec.flush());
            assert_eq!(pics.len(), c.frames as usize, "{}: pictures", c.name);
            for (i, p) in pics.iter().enumerate() {
                assert_eq!((p.width as usize, p.height as usize, p.chroma_width as usize, p.chroma_height as usize), (w, h, w, h), "{}: 4:4:4 at the size", c.name);
                for (k, (plane, r)) in [&p.y, &p.u, &p.v].into_iter().zip(&refs).enumerate() {
                    let got = samples(plane);
                    let f = &r[i * fsize..][..w * h * bps];
                    let want: Vec<u16> = if bps == 2 { f.chunks(2).map(|b| u16::from_le_bytes([b[0], b[1]])).collect() } else { f.iter().map(|&x| x as u16).collect() };
                    let diff = got.iter().zip(&want).filter(|(a, b)| a != b).count();
                    assert!(got.len() == want.len() && diff == 0, "{} (threads {threads}): picture {i} plane {k}: {diff} samples differ", c.name);
                }
            }
        }
        ran += 1;
    }
    assert_eq!(ran, CASES.len());
}

/// A stream missing one plane's slices (broken) still gives every picture (the planes there
/// are, the rest grey), and nothing piles up.
#[test]
fn a_missing_colour_plane_is_survived() {
    let c = &CASES[1];
    let Some(files) = monochrome_streams(c) else { return };
    let streams: Vec<Vec<u8>> = files.iter().map(|(h, _)| std::fs::read(h).unwrap()).collect();
    for keep in [[true, true, false], [true, false, false], [false, true, true]] {
        let aus = combine(&streams, keep);
        let mut dec = Decoder::with_threads(1);
        let mut pics = Vec::new();
        for (i, au) in aus.iter().enumerate() {
            if let Ok(p) = dec.decode(au, i as i64) {
                pics.extend(p);
            }
        }
        pics.extend(dec.flush());
        if keep[0] {
            assert_eq!(pics.len(), c.frames as usize, "{keep:?}: every picture");
        }
    }
}
