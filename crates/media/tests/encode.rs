//! MovieWriter round trip: encode known frames + a tone, then probe and decode them back.

use reelforge_core::{FrameRate, Time};
use reelforge_media::{probe, AudioReader, FrameSource, MovieWriter, Quality, WriterSettings};
use std::time::Duration;

fn writer(path: &std::path::Path, w: u32, h: u32, audio: bool) -> MovieWriter {
    MovieWriter::create(path, &WriterSettings {
        width: w,
        height: h,
        input_size: None,
        frame_rate: FrameRate::new(30000, 1001),
        quality: Quality::High,
        audio_rate: audio.then_some(48_000),
        proxy: false,
        ..Default::default()
    })
    .unwrap()
}

#[test]
fn writes_playable_h264_aac_mp4() {
    let path = std::env::temp_dir().join("reelforge_writer_test.mp4");
    let (w, h) = (320u32, 240u32);
    let mut mw = writer(&path, w, h, true);
    // 60 frames: first half orange, second half teal. 2 s of 440 Hz.
    for i in 0..60 {
        let c: [u8; 4] = if i < 30 { [230, 120, 30, 255] } else { [20, 160, 170, 255] };
        let frame: Vec<u8> = c.iter().copied().cycle().take((w * h * 4) as usize).collect();
        mw.write_video(&frame).unwrap();
    }
    let n = 2 * 48_000;
    let tone: Vec<f32> = (0..n).flat_map(|i| {
        let v = 0.5 * (i as f32 * 440.0 * std::f32::consts::TAU / 48_000.0).sin();
        [v, v]
    }).collect();
    for chunk in tone.chunks(1000) {
        mw.write_audio(chunk).unwrap();
    }
    let codec = mw.video_codec.clone();
    mw.finish().unwrap();

    let info = probe(&path).unwrap();
    let v = info.video.unwrap();
    assert_eq!((v.width, v.height), (w, h));
    assert_eq!(v.frame_rate, FrameRate::new(30000, 1001));
    eprintln!("encoded with {codec} -> {}", v.codec);
    let a = info.audio.unwrap();
    assert_eq!((a.codec.as_str(), a.sample_rate, a.channels), ("aac", 48_000, 2));
    assert!((info.duration.as_secs_f64() - 2.0).abs() < 0.1, "duration {}", info.duration);

    // Colours survive RGB -> YUV (BT.709) -> RGB within compression error.
    let rate = FrameRate::new(30000, 1001);
    let mut src = FrameSource::open(&path, None).unwrap();
    for (frame_no, want) in [(10, [230, 120, 30]), (45, [20, 160, 170])] {
        let f = src.frame_at_blocking(Time::from_frame(frame_no, rate), Duration::from_secs(5)).unwrap();
        let i = ((h / 2 * w + w / 2) * 4) as usize;
        let got = [f.rgba[i], f.rgba[i + 1], f.rgba[i + 2]];
        for (g, w) in got.iter().zip(want) {
            assert!((*g as i32 - w).abs() <= 4, "frame {frame_no}: got {got:?}, want {want:?}");
        }
    }

    // Audio comes back as the same tone, in sync with the start.
    let mut r = AudioReader::open(&path, 48_000).unwrap();
    let mut out = vec![0.0f32; 48_000 * 2];
    r.read(24_000, &mut out);
    let rms = (out.iter().map(|v| v * v).sum::<f32>() / out.len() as f32).sqrt();
    assert!((rms - 0.5 / 2f32.sqrt()).abs() < 0.05, "rms {rms}");
}

#[test]
fn odd_sizes_and_video_only() {
    let path = std::env::temp_dir().join("reelforge_writer_odd.mp4");
    let mut mw = writer(&path, 101, 57, false);
    let frame = vec![128u8; 101 * 57 * 4];
    for _ in 0..5 {
        mw.write_video(&frame).unwrap();
    }
    assert!(mw.write_video(&frame[..10]).is_err(), "wrong-size frame is rejected");
    mw.finish().unwrap();
    let info = probe(&path).unwrap();
    let v = info.video.unwrap();
    assert_eq!((v.width, v.height), (100, 56), "rounded down to even");
    assert!(info.audio.is_none());
}

#[test]
fn dropping_unfinished_writer_removes_the_file() {
    let path = std::env::temp_dir().join("reelforge_writer_cancel.mp4");
    let mut mw = writer(&path, 64, 64, true);
    mw.write_video(&vec![0u8; 64 * 64 * 4]).unwrap();
    assert!(path.exists());
    drop(mw);
    assert!(!path.exists());
}
