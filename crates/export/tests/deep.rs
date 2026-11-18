//! 10-bit footage through to a 10-bit export keeps its precision: a smooth
//! ramp in a ProRes 422 HQ clip comes out of a ProRes 422 HQ export with as
//! many levels as went in, used directly and inside a composition, with a
//! colour correction on top. Needs the `ffmpeg` CLI and a GPU; skips otherwise.

use reelforge_core::{Clip, ClipId, Effect, EffectKind, FrameRate, MediaAsset, MediaId, Project, Sequence, Time};
use reelforge_export::{export_sequence, ExportSettings};
use reelforge_media::{probe, ColorOut, Fit, FrameSource, Quality, VideoCodec};
use reelforge_render::headless_device;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

const W: u32 = 1024;
const H: u32 = 64;

fn ramp() -> Option<PathBuf> {
    let path = std::env::temp_dir().join(format!("rf-deep-ramp-{}.mov", std::process::id()));
    let ok = Command::new("ffmpeg")
        .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i", &format!("color=black:s={W}x{H}:r=24:d=0.5")])
        // 10-bit TV-range luma 64 → 940 across the frame.
        .args(["-vf", "format=yuv422p10le,geq=lum='64+X*876/1023':cb=512:cr=512"])
        .args(["-c:v", "prores_ks", "-profile:v", "3"])
        .arg(&path)
        .status()
        .is_ok_and(|s| s.success());
    ok.then_some(path)
}

fn settings(path: PathBuf) -> ExportSettings {
    ExportSettings {
        path,
        width: W,
        height: H,
        quality: Quality::High,
        include_audio: false,
        start: Time::ZERO,
        end: Time::from_secs_f64(0.125),
        video: Some(VideoCodec::ProRes422Hq),
        hardware: false,
        fit: Fit::Letterbox,
        sequence: 0,
        color: ColorOut::Sdr10,
        bitrate_kbps: None,
        alpha: false,
        frame_rate: None,
        between_frames: None,
    }
}

/// Distinct luma code values along row 30 of the exported file's first frame.
fn levels_out(path: &std::path::Path) -> usize {
    let mut src = FrameSource::open(path, None).unwrap();
    let f = src.frame_at_blocking(Time::ZERO, Duration::from_secs(10)).unwrap();
    let reelforge_core::frame::Pixels::Yuv(y) = &f.pixels else { panic!("not deep YUV: {:?}", f.pixels) };
    assert!(y.layout.deep(), "{:?}", y.layout);
    let row = 30 * y.width as usize;
    let mut v: Vec<u16> = (0..y.width as usize).map(|x| u16::from_le_bytes([y.planes[0][(row + x) * 2], y.planes[0][(row + x) * 2 + 1]]) >> y.layout.shift()).collect();
    v.dedup();
    v.len()
}

#[test]
fn ten_bit_footage_exports_with_ten_bit_precision() {
    let Some(src) = ramp() else { eprintln!("ffmpeg CLI not found; skipping"); return };
    let Some((device, queue)) = headless_device() else { return };
    let s = Time::from_secs_f64;
    let asset = MediaAsset { id: MediaId(1), path: src.clone(), name: "ramp".into(), info: probe(&src).unwrap(), conform: None };
    let out = std::env::temp_dir().join(format!("rf-deep-out-{}.mov", std::process::id()));
    let rate = FrameRate::new(24, 1);

    // Straight through, then with a gentle contrast change (effects run in float too).
    for contrast in [None, Some(110.0)] {
        let mut p = Project::new();
        p.media.push(asset.clone());
        let mut sq = Sequence::new("D", W, H, rate);
        let mut clip = Clip::new(ClipId(1), MediaId(1), s(0.0), s(0.0), s(0.5), None);
        if let Some(c) = contrast {
            let mut fx = Effect::new(EffectKind::ColorCorrect);
            fx.params[1].value = c;
            clip.effects.push(fx);
        }
        sq.tracks[0].clips = vec![clip];
        p.sequences = vec![sq];
        export_sequence(&p, &settings(out.clone()), &device, &queue, |_| true).unwrap();
        let n = levels_out(&out);
        // 877 levels went in; 8 bits anywhere on the way would leave about 256.
        assert!(n > 600, "contrast {contrast:?}: {n} levels");
    }

    // Inside a composition.
    let mut p = Project::new();
    p.media.push(asset);
    let mut inner = Sequence::new("Inner", W, H, rate);
    inner.tracks[0].clips = vec![Clip::new(ClipId(1), MediaId(1), s(0.0), s(0.0), s(0.5), None)];
    let mut outer = Sequence::new("Outer", W, H, rate);
    outer.tracks[0].clips = vec![Clip::new_nested(ClipId(2), 1, s(0.0), s(0.5), None)];
    p.sequences = vec![outer, inner];
    export_sequence(&p, &settings(out.clone()), &device, &queue, |_| true).unwrap();
    let n = levels_out(&out);
    assert!(n > 600, "nested: {n} levels");
    std::fs::remove_file(&out).ok();
    std::fs::remove_file(&src).ok();
}
