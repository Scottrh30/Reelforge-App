//! Decode speed on a 720p clip (run with `--ignored --nocapture`).

mod common;

#[test]
#[ignore]
fn decode_speed_720p() {
    let Some((ivf, _)) = common::ensure(common::fixture("bench_720p")) else { return };
    let data = std::fs::read(ivf).unwrap();
    let frames = common::ivf_frames(&data);
    for _ in 0..3 {
        let t = std::time::Instant::now();
        let mut dec = reelforge_vp8::Decoder::new();
        for (i, f) in frames.iter().enumerate() {
            dec.decode(f, i as i64).unwrap();
        }
        let s = t.elapsed().as_secs_f64();
        eprintln!("{} frames in {:.3} s: {:.1} fps", frames.len(), s, frames.len() as f64 / s);
    }
}
