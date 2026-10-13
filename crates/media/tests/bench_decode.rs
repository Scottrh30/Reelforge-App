//! Decode throughput (run with --ignored --nocapture).
use reelforge_core::Time;
use reelforge_media::FrameSource;
use std::time::{Duration, Instant};

#[test]
#[ignore]
fn decode_throughput() {
    let path = std::env::var("RF_BENCH").unwrap();
    let mut src = FrameSource::open(std::path::Path::new(&path), None).unwrap();
    let start = Instant::now();
    let mut bytes = 0usize;
    for i in 0..120 {
        let f = src.frame_at_blocking(Time::from_secs_f64(i as f64 / 30.0), Duration::from_secs(10)).unwrap();
        bytes += f.byte_len();
    }
    let el = start.elapsed();
    println!("BENCH {:.2} ms/frame, {:.1} MB/frame", el.as_secs_f64() * 1000.0 / 120.0, bytes as f64 / 120.0 / 1e6);
}
