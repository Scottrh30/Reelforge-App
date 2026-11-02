//! 10-, 12- and 16-bit sources keep their precision out of the decoder: deep
//! YUV stays YUV at its own bit depth (4:2:2 and 4:4:4 too, alpha too), deep
//! RGB becomes half floats. Needs the `ffmpeg` CLI (to make the media).

use reelforge_core::frame::{f16_to_f32, Chroma, Pixels, YuvLayout};
use reelforge_core::Time;
use reelforge_media::{FrameSource, VideoFrame};
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

const W: u32 = 1024;

/// A clip whose brightness climbs smoothly from left to right in `pix_fmt`
/// (made with geq, in that format), encoded with `codec`.
fn make(name: &str, pix_fmt: &str, expr: &str, codec: &[&str]) -> Option<PathBuf> {
    let dir = std::env::temp_dir().join(format!("rf-deep-{}", std::process::id()));
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(name);
    let ok = Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i", &format!("color=black:s={W}x64:r=24:d=0.25")])
        .args(["-vf", &format!("format={pix_fmt},geq={expr}")])
        .args(codec)
        .arg(&path)
        .status()
        .is_ok_and(|s| s.success());
    ok.then_some(path)
}

fn frame(path: &PathBuf, max: Option<(u32, u32)>) -> VideoFrame {
    let mut src = FrameSource::open(path, max).unwrap();
    src.frame_at_blocking(Time::ZERO, Duration::from_secs(10)).expect("frame")
}

/// Distinct brightness values along row 20.
fn levels(f: &VideoFrame) -> usize {
    let mut v: Vec<u32> = match &f.pixels {
        Pixels::Yuv(y) => {
            let row = 20 * y.width as usize;
            (0..y.width as usize)
                .map(|x| match y.layout.sample_bytes() {
                    1 => y.planes[0][row + x] as u32,
                    _ => u16::from_le_bytes([y.planes[0][(row + x) * 2], y.planes[0][(row + x) * 2 + 1]]) as u32 >> y.layout.shift(),
                })
                .collect()
        }
        Pixels::Float(p) => (0..p.width as usize).map(|x| p.rgba[(20 * p.width as usize + x) * 4 + 1] as u32).collect(),
        other => panic!("8-bit pixels: {other:?}"),
    };
    v.dedup();
    v.len()
}

#[test]
fn deep_yuv_stays_yuv_at_its_bit_depth() {
    // 10-bit TV-range luma from 64 to 940 across the frame: 877 levels.
    let ramp10 = "lum='64+X*876/1023':cb=512:cr=512";
    let Some(prores) = make("hq.mov", "yuv422p10le", ramp10, &["-c:v", "prores_ks", "-profile:v", "3"]) else { eprintln!("ffmpeg CLI not found; skipping"); return };
    let cases: Vec<(PathBuf, u32, Chroma, bool)> = vec![
        (prores, 10, Chroma::C422, false),
        (make("raw444p12.nut", "yuv444p12le", "lum='256+X*3504/1023':cb=2048:cr=2048", &["-c:v", "rawvideo"]).unwrap(), 12, Chroma::C444, false),
        (make("raw420p10.nut", "yuv420p10le", ramp10, &["-c:v", "rawvideo"]).unwrap(), 10, Chroma::C420, false),
        (make("alpha.nut", "yuva444p10le", &format!("{ramp10}:a='if(lt(X,512),1023,0)'"), &["-c:v", "rawvideo"]).unwrap(), 10, Chroma::C444, true),
    ];
    for (path, bits, chroma, alpha) in cases {
        let f = frame(&path, None);
        let Pixels::Yuv(y) = &f.pixels else { panic!("{}: {:?}", path.display(), f.pixels) };
        // ProRes may come out of Reelforge's own decoder at 12 bits; never fewer than were made.
        assert!(y.layout.bits() >= bits && y.layout.chroma() == chroma && y.layout.alpha() == alpha, "{}: {:?}", path.display(), y.layout);
        assert!(y.layout.deep());
        assert!(levels(&f) > 600, "{}: {} levels", path.display(), levels(&f));
        if alpha {
            let rgba = f.rgba();
            assert_eq!((rgba[(20 * W as usize + 100) * 4 + 3], rgba[(20 * W as usize + 900) * 4 + 3]), (255, 0));
        }
        // Shrunk for the viewer: still deep, same layout.
        let small = frame(&path, Some((512, 512)));
        let Pixels::Yuv(s) = &small.pixels else { panic!("{:?}", small.pixels) };
        assert_eq!((s.width, s.layout.chroma(), s.layout.alpha()), (512, chroma, alpha));
        assert!(s.layout.deep());
    }
    // 8-bit stays 8-bit (and 4:2:2 8-bit is now kept as YUV too).
    let eight = make("raw422p.nut", "yuv422p", "lum='16+X*219/1023':cb=128:cr=128", &["-c:v", "rawvideo"]).unwrap();
    let f = frame(&eight, None);
    let Pixels::Yuv(y) = &f.pixels else { panic!("{:?}", f.pixels) };
    assert_eq!(y.layout, YuvLayout::Planar { bits: 8, chroma: Chroma::C422, alpha: false });
}

#[test]
fn deep_rgb_becomes_half_float() {
    // 16-bit PNG and 10-bit DPX, a grey ramp in each.
    let Some(png) = make("ramp16.png", "gbrp16le", "r='X*64':g='X*64':b='X*64'", &["-frames:v", "1", "-pix_fmt", "rgb48be"]) else { eprintln!("ffmpeg CLI not found; skipping"); return };
    let dpx = make("ramp10.dpx", "gbrp10le", "r='X':g='X':b='X'", &["-frames:v", "1", "-pix_fmt", "gbrp10le"]).unwrap();
    for path in [png, dpx] {
        let f = frame(&path, None);
        let Pixels::Float(p) = &f.pixels else { panic!("{}: {:?}", path.display(), f.pixels) };
        assert!(levels(&f) > 600, "{}: {} levels", path.display(), levels(&f));
        // Linear light: mid-grey code (about half way) is about 0.21.
        let mid = f16_to_f32(p.rgba[(20 * p.width as usize + 512) * 4 + 1]);
        assert!((mid - 0.214).abs() < 0.01, "{}: {mid}", path.display());
        assert_eq!(f16_to_f32(p.rgba[3]), 1.0);
        // Rising, never falling.
        let row: Vec<f32> = (0..p.width as usize).map(|x| f16_to_f32(p.rgba[(20 * p.width as usize + x) * 4])).collect();
        assert!(row.windows(2).all(|w| w[1] >= w[0]), "{}", path.display());
    }
}

/// HDR footage is recognised: HEVC tagged PQ (HDR10) and HLG comes out as
/// YUV marked with its transfer (and the native decoder's frames too, by the
/// stream's tag); untagged footage is SDR.
#[test]
fn hdr_footage_is_tagged_with_its_transfer() {
    use reelforge_core::frame::Transfer;
    let flat = "lum=512:cb=512:cr=512";
    // x265 writes the tags into the stream itself (its VUI), as cameras and HDR masters do.
    let tagged = |params: &'static str| -> Vec<&'static str> { vec!["-c:v", "libx265", "-x265-params", params] };
    let Some(pq) = make("pq.mp4", "yuv420p10le", flat, &tagged("log-level=error:colorprim=bt2020:transfer=smpte2084:colormatrix=bt2020nc")) else { eprintln!("ffmpeg CLI not found; skipping"); return };
    let Some(hlg) = make("hlg.mp4", "yuv420p10le", flat, &tagged("log-level=error:colorprim=bt2020:transfer=arib-std-b67:colormatrix=bt2020nc")) else { return };
    let Some(sdr) = make("sdr10.mp4", "yuv420p10le", flat, &["-c:v", "libx265", "-x265-params", "log-level=error"]) else { return };
    for native in [false, true] {
        reelforge_media::codecs::set_enabled(native);
        for (path, want) in [(&pq, Transfer::Pq), (&hlg, Transfer::Hlg), (&sdr, Transfer::Sdr)] {
            let f = frame(path, None);
            let Pixels::Yuv(y) = &f.pixels else { panic!("{path:?}: not YUV") };
            assert_eq!(y.transfer, want, "{path:?} (native decoders {native})");
            assert_eq!(y.layout.bits(), 10);
        }
    }
    reelforge_media::codecs::set_enabled(true);
    // The Media panel's HDR badge comes from the probe.
    let hdr = |p: &PathBuf| reelforge_media::probe(p).unwrap().video.unwrap().hdr;
    assert_eq!((hdr(&pq).as_deref(), hdr(&hlg).as_deref(), hdr(&sdr)), (Some("PQ"), Some("HLG"), None));
}
