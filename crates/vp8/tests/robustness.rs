//! Damaged streams give errors or wrong pictures, never a crash or a hang.

mod common;

/// A small deterministic generator, so failures reproduce.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

#[test]
fn damaged_frames_never_crash() {
    let mut streams = Vec::new();
    for name in ["motion_best", "partitions_8", "screen_segments", "altref_hidden", "tiny_odd"] {
        let Some((ivf, _)) = common::ensure(common::fixture(name)) else { return };
        streams.push(std::fs::read(ivf).unwrap());
    }
    let mut rng = Rng(0x9e3779b97f4a7c15);
    for data in &streams {
        let frames: Vec<Vec<u8>> = common::ivf_frames(data).into_iter().map(|f| f.to_vec()).collect();
        for round in 0..40 {
            let mut dec = reelforge_vp8::Decoder::new();
            for (i, f) in frames.iter().enumerate() {
                let mut f = f.clone();
                match (round + i) % 5 {
                    0 => {
                        // Flip a few bits anywhere (headers included).
                        for _ in 0..1 + rng.next() % 8 {
                            let k = (rng.next() as usize) % f.len().max(1);
                            if let Some(b) = f.get_mut(k) {
                                *b ^= 1 << (rng.next() % 8);
                            }
                        }
                    }
                    1 => f.truncate((rng.next() as usize) % (f.len() + 1)),
                    2 => {
                        // Random bytes after the frame tag.
                        for b in f.iter_mut().skip(3) {
                            if rng.next().is_multiple_of(16) {
                                *b = rng.next() as u8;
                            }
                        }
                    }
                    3
                        // Huge or zero sizes in a key frame header.
                        if f.len() > 10 && f[0] & 1 == 0 => {
                            f[6] = rng.next() as u8;
                            f[7] = rng.next() as u8;
                            f[8] = rng.next() as u8;
                            f[9] = rng.next() as u8;
                        }
                    _ => {}
                }
                let _ = dec.decode(&f, i as i64);
            }
        }
    }
    // Garbage and empty input.
    let mut dec = reelforge_vp8::Decoder::new();
    assert!(dec.decode(&[], 0).is_err());
    assert_eq!(dec.decode(&[1, 0, 0, 0], 0).unwrap_err(), reelforge_vp8::Error::NoKeyframe);
    for _ in 0..2000 {
        let n = (rng.next() % 64) as usize;
        let mut f: Vec<u8> = (0..n).map(|_| rng.next() as u8).collect();
        if f.len() >= 10 {
            f[0] &= !1;
            f[3..6].copy_from_slice(&[0x9d, 0x01, 0x2a]);
            f[7] &= 0x01;
            f[9] &= 0x01;
        }
        let _ = dec.decode(&f, 0);
    }
}

/// An inter frame after a damaged key frame still decodes against whatever was there.
#[test]
fn recovers_at_the_next_key_frame() {
    let Some((ivf, yuv)) = common::ensure(common::fixture("intra_only")) else { return };
    let data = std::fs::read(ivf).unwrap();
    let frames = common::ivf_frames(&data);
    let mut dec = reelforge_vp8::Decoder::new();
    let mut bad = frames[0].to_vec();
    bad.truncate(bad.len() / 3);
    let _ = dec.decode(&bad, 0);
    // Every frame is a key frame: the rest come out exactly right.
    let reference = std::fs::read(yuv).unwrap();
    let per = 352 * 288 * 3 / 2;
    for (i, f) in frames.iter().enumerate().skip(1) {
        let p = dec.decode(f, i as i64).unwrap().unwrap();
        let mut got = p.y.clone();
        got.extend(&p.u);
        got.extend(&p.v);
        assert!(got == reference[i * per..(i + 1) * per], "frame {i}");
    }
}
