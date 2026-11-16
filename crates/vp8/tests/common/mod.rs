//! Test fixtures (ffmpeg + libvpx as external encoder and oracle), a tiny IVF reader, and the
//! sample-exact comparison.
#![allow(dead_code)]

use reelforge_vp8::{Decoder, Picture};
use std::path::{Path, PathBuf};
use std::process::Command;

/// One encoder configuration of the fixture matrix.
pub struct Fixture {
    pub name: &'static str,
    /// lavfi source (without size / rate).
    pub source: &'static str,
    pub width: u32,
    pub height: u32,
    pub frames: u32,
    /// Extra video filter appended after the source.
    pub filter: &'static str,
    /// libvpx arguments ("2pass" first for a two-pass encode).
    pub args: &'static [&'static str],
}

const NOISE: &str = "noise=alls=10:allf=t+u";
/// Motion in every direction (zoom and turn), for all the vector modes and splits.
const MOTION: &str = "rotate=a=t*0.7:c=black,zoompan=z='1+0.02*on':d=1:s=352x288,noise=alls=6:allf=t+u";

macro_rules! fx {
    ($name:expr, $src:expr, $w:expr, $h:expr, $n:expr, $filter:expr, [$($a:expr),* $(,)?]) => {
        Fixture { name: $name, source: $src, width: $w, height: $h, frames: $n, filter: $filter, args: &[$($a),*] }
    };
}

pub const FIXTURES: &[Fixture] = &[
    fx!("intra_only", "testsrc2", 352, 288, 4, NOISE, ["-g", "1", "-b:v", "1M"]),
    fx!("default_good", "testsrc2", 352, 288, 20, NOISE, ["-b:v", "800k"]),
    fx!("motion_best", "testsrc2", 352, 288, 16, MOTION, ["-b:v", "600k", "-quality", "best", "-cpu-used", "0"]),
    fx!("motion_good", "testsrc2", 352, 288, 20, MOTION, ["-b:v", "400k", "-cpu-used", "1"]),
    fx!("motion_realtime", "testsrc2", 352, 288, 20, MOTION, ["-b:v", "400k", "-deadline", "realtime", "-cpu-used", "8"]),
    fx!("altref_hidden", "testsrc2", 352, 288, 30, NOISE, ["2pass", "-b:v", "600k", "-auto-alt-ref", "1", "-lag-in-frames", "16", "-arnr-maxframes", "7", "-arnr-strength", "5"]),
    fx!("altref_motion", "mandelbrot", 352, 288, 30, "noise=alls=4:allf=t", ["2pass", "-b:v", "500k", "-auto-alt-ref", "1", "-lag-in-frames", "25"]),
    fx!("partitions_2", "testsrc2", 640, 360, 8, NOISE, ["-b:v", "1M", "-slices", "2"]),
    fx!("partitions_8", "testsrc2", 640, 360, 8, NOISE, ["-b:v", "1M", "-slices", "8"]),
    fx!("profile1", "testsrc2", 352, 288, 12, MOTION, ["-b:v", "500k", "-profile:v", "1"]),
    fx!("profile2", "testsrc2", 352, 288, 12, MOTION, ["-b:v", "500k", "-profile:v", "2"]),
    fx!("profile3", "testsrc2", 352, 288, 12, MOTION, ["-b:v", "500k", "-profile:v", "3"]),
    fx!("odd_size", "testsrc2", 347, 251, 10, MOTION, ["-b:v", "500k"]),
    fx!("tiny_odd", "testsrc2", 33, 17, 8, NOISE, ["-b:v", "100k"]),
    fx!("one_mb", "testsrc2", 16, 16, 6, NOISE, ["-b:v", "50k"]),
    fx!("q_low", "testsrc2", 352, 288, 6, NOISE, ["-qmin", "0", "-qmax", "0", "-b:v", "20M"]),
    fx!("q_high", "testsrc2", 352, 288, 10, MOTION, ["-qmin", "63", "-qmax", "63", "-b:v", "50k"]),
    fx!("sharpness", "testsrc2", 352, 288, 8, MOTION, ["-b:v", "400k", "-sharpness", "5"]),
    fx!("error_resilient", "testsrc2", 352, 288, 16, MOTION, ["-b:v", "500k", "-error-resilient", "1"]),
    fx!("screen_segments", "testsrc2", 352, 288, 16, MOTION, ["-b:v", "300k", "-deadline", "realtime", "-cpu-used", "6", "-screen-content-mode", "1"]),
    fx!("noise_sensitive", "testsrc2", 352, 288, 12, NOISE, ["-b:v", "400k", "-deadline", "realtime", "-noise-sensitivity", "4", "-static-thresh", "200"]),
    fx!(
        "temporal_layers",
        "testsrc2",
        352,
        288,
        24,
        MOTION,
        ["-b:v", "600k", "-deadline", "realtime", "-cpu-used", "8", "-ts-parameters", "ts_number_layers=3:ts_target_bitrate=200,400,600:ts_rate_decimator=4,2,1:ts_periodicity=4:ts_layer_id=0,2,1,2"]
    ),
    fx!("fade_intra", "testsrc2", 352, 288, 20, "fade=in:0:15", ["-b:v", "400k", "-g", "8"]),
    fx!("hd_720p", "testsrc2", 1280, 720, 6, NOISE, ["-b:v", "3M", "-deadline", "realtime", "-cpu-used", "4"]),
    fx!("bench_720p", "testsrc2", 1280, 720, 60, "noise=alls=3:allf=t", ["-b:v", "2M", "-cpu-used", "2"]),
];

pub fn fixture(name: &str) -> &'static Fixture {
    FIXTURES.iter().find(|f| f.name == name).unwrap_or_else(|| panic!("unknown fixture {name}"))
}

pub fn ffmpeg() -> Option<PathBuf> {
    reelforge_testkit::oracle::ffmpeg()
}

pub fn fixtures_dir() -> PathBuf {
    let d = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/fixtures/vp8");
    std::fs::create_dir_all(&d).expect("create fixtures dir");
    d
}

/// Unique suffix for temporary files (tests run in parallel threads of one process).
pub fn unique() -> String {
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("{}-{n}", std::process::id())
}

pub fn run(cmd: &mut Command) -> bool {
    match cmd.output() {
        Ok(o) if o.status.success() => true,
        Ok(o) => {
            eprintln!("command failed: {:?}\n{}", cmd, String::from_utf8_lossy(&o.stderr));
            false
        }
        Err(e) => {
            eprintln!("failed to run {:?}: {e}", cmd);
            false
        }
    }
}

/// Reference decode of `ivf` with ffmpeg's `decoder` (its own "vp8", or "libvpx").
pub fn reference_decode(ivf: &Path, yuv: &Path, decoder: &str) -> bool {
    let Some(ff) = ffmpeg() else { return false };
    let tmp = yuv.with_extension(format!("tmp.{}.yuv", unique()));
    let mut c = Command::new(&ff);
    c.args(["-hide_banner", "-loglevel", "error", "-y", "-c:v", decoder, "-i"]).arg(ivf);
    c.args(["-fps_mode", "passthrough", "-noautoscale", "-f", "rawvideo", "-pix_fmt", "yuv420p"]).arg(&tmp);
    if !run(&mut c) {
        return false;
    }
    std::fs::rename(&tmp, yuv).is_ok() || yuv.exists()
}

/// Generate (if needed) the fixture stream and its ffmpeg reference decode.
pub fn ensure(f: &Fixture) -> Option<(PathBuf, PathBuf)> {
    let Some(ff) = ffmpeg() else {
        eprintln!("SKIP {}: ffmpeg not found", f.name);
        return None;
    };
    let dir = fixtures_dir();
    let ivf = dir.join(format!("{}.ivf", f.name));
    let yuv = dir.join(format!("{}.yuv", f.name));
    if !ivf.exists() {
        let tmp = dir.join(format!("{}.tmp.{}.ivf", f.name, unique()));
        let mut vf = format!("{}=size={}x{}:rate=25,format=yuv420p", f.source, f.width, f.height);
        if !f.filter.is_empty() {
            let filter = f.filter.replace("s=352x288", &format!("s={}x{}", f.width, f.height));
            vf.push(',');
            vf.push_str(&filter);
        }
        let two_pass = f.args.first() == Some(&"2pass");
        let args: Vec<&str> = f.args.iter().copied().filter(|a| *a != "2pass").collect();
        let log = dir.join(format!("{}.{}.passlog", f.name, unique()));
        let enc = |pass: Option<u32>, out: &Path| {
            let mut c = Command::new(&ff);
            c.args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", &vf]);
            c.args(["-frames:v", &f.frames.to_string(), "-c:v", "libvpx", "-pix_fmt", "yuv420p"]);
            c.args(&args);
            if let Some(p) = pass {
                c.args(["-pass", &p.to_string(), "-passlogfile"]).arg(&log);
            }
            if pass == Some(1) {
                c.args(["-f", "null", "-"]);
            } else {
                c.args(["-f", "ivf"]).arg(out);
            }
            c
        };
        if two_pass && !run(&mut enc(Some(1), &tmp)) {
            eprintln!("SKIP {}: first pass failed", f.name);
            return None;
        }
        if !run(&mut enc(if two_pass { Some(2) } else { None }, &tmp)) {
            let _ = std::fs::remove_file(&tmp);
            eprintln!("SKIP {}: libvpx encode failed", f.name);
            return None;
        }
        let _ = std::fs::remove_file(format!("{}-0.log", log.display()));
        if std::fs::rename(&tmp, &ivf).is_err() && !ivf.exists() {
            panic!("could not store fixture {}", ivf.display());
        }
    }
    if !yuv.exists() {
        assert!(reference_decode(&ivf, &yuv, "vp8"), "reference decode failed for {}", f.name);
    }
    Some((ivf, yuv))
}

/// Frames of an IVF file (32-byte file header, then 12-byte frame headers).
pub fn ivf_frames(data: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    if data.len() < 32 || &data[..4] != b"DKIF" {
        return out;
    }
    let hl = u16::from_le_bytes([data[6], data[7]]) as usize;
    let mut p = hl.max(32);
    while p + 12 <= data.len() {
        let sz = u32::from_le_bytes(data[p..p + 4].try_into().unwrap()) as usize;
        p += 12;
        if p + sz > data.len() {
            break;
        }
        out.push(&data[p..p + sz]);
        p += sz;
    }
    out
}

/// Decode an IVF file, pts = frame index.
pub fn decode_file(path: &Path) -> Result<Vec<Picture>, String> {
    let data = std::fs::read(path).map_err(|e| e.to_string())?;
    let mut dec = Decoder::new();
    let mut out = Vec::new();
    for (i, f) in ivf_frames(&data).into_iter().enumerate() {
        match dec.decode(f, i as i64) {
            Ok(p) => out.extend(p),
            Err(e) => return Err(format!("frame {i}: {e}")),
        }
    }
    Ok(out)
}

/// Compare decoded pictures with a raw 4:2:0 reference; reports the first mismatch.
pub fn compare(pics: &[Picture], reference: &[u8]) -> Result<(), String> {
    let mut off = 0usize;
    for (i, p) in pics.iter().enumerate() {
        let (w, h) = (p.width as usize, p.height as usize);
        let (cw, ch) = (p.chroma_width as usize, p.chroma_height as usize);
        let fsize = w * h + 2 * cw * ch;
        if off + fsize > reference.len() {
            return Err(format!("decoder produced {} frames, reference has fewer (at frame {i})", pics.len()));
        }
        let f = &reference[off..off + fsize];
        off += fsize;
        let planes = [("Y", &p.y, &f[..w * h], w, h), ("U", &p.u, &f[w * h..w * h + cw * ch], cw, ch), ("V", &p.v, &f[w * h + cw * ch..], cw, ch)];
        for (name, got, exp, pw, ph) in planes {
            let mut first = None;
            let mut count = 0usize;
            for y in 0..ph {
                for x in 0..pw {
                    let (a, b) = (got[y * pw + x], exp[y * pw + x]);
                    if a != b {
                        count += 1;
                        if first.is_none() {
                            first = Some((x, y, a, b));
                        }
                    }
                }
            }
            if let Some((x, y, a, b)) = first {
                let mbs = if name == "Y" { 16 } else { 8 };
                return Err(format!(
                    "frame {i} ({w}x{h}) plane {name}: first mismatch at ({x},{y}) [MB ({},{})]: got {a} want {b}; {count} samples differ",
                    x / mbs,
                    y / mbs
                ));
            }
        }
    }
    if off != reference.len() {
        return Err(format!("decoder produced {} frames, reference has more data", pics.len()));
    }
    Ok(())
}

/// Generate, decode and compare one fixture. Ok(false) when skipped.
pub fn check_fixture(name: &str) -> Result<bool, String> {
    let f = fixture(name);
    let Some((ivf, yuv)) = ensure(f) else { return Ok(false) };
    let reference = std::fs::read(&yuv).map_err(|e| e.to_string())?;
    let pics = decode_file(&ivf)?;
    compare(&pics, &reference).map(|_| true)
}
