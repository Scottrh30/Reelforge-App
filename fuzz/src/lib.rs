//! The fuzz input format and the harness every target shares.
//!
//! An input is the stream's size (two little-endian u16), then records of a little-endian u32
//! length and that many bytes (shorter at the end of the input): the first record is the
//! container's setup data (avcC, hvcC, sequence headers…), each next one a packet. It goes
//! through `reelforge_codecs::open`, exactly as the app feeds a decoder: send each packet, take
//! every picture, then finish. Anything but a panic, hang or runaway allocation is fine.

use reelforge_codecs::{Codec, StreamInfo};

/// At most this many packets per input (keeps each run quick).
pub const MAX_PACKETS: usize = 48;

pub fn records(data: &[u8]) -> Option<(u32, u32, Vec<&[u8]>)> {
    let (w, rest) = data.split_first_chunk::<2>()?;
    let (h, mut rest) = rest.split_first_chunk::<2>()?;
    let mut out = Vec::new();
    while out.len() <= MAX_PACKETS {
        let Some((len, tail)) = rest.split_first_chunk::<4>() else { break };
        let n = (u32::from_le_bytes(*len) as usize).min(tail.len());
        out.push(&tail[..n]);
        rest = &tail[n..];
    }
    Some((u32::from(u16::from_le_bytes(*w)), u32::from(u16::from_le_bytes(*h)), out))
}

/// Decode `data` (the format above) with `codec`'s decoder.
pub fn run(codec: Codec, data: &[u8]) {
    let Some((width, height, recs)) = records(data) else { return };
    let mut it = recs.into_iter();
    let extradata = it.next().unwrap_or(&[]).to_vec();
    let Some(mut dec) = reelforge_codecs::open(codec, &StreamInfo { width, height, extradata }) else { return };
    for (i, p) in it.enumerate() {
        let _ = dec.send(p, i as i64);
        while let Some(pic) = dec.receive() {
            std::hint::black_box(pic);
        }
        // A flush now and then, as seeking does.
        if i % 17 == 16 {
            dec.flush();
        }
    }
    dec.finish();
    while let Some(pic) = dec.receive() {
        std::hint::black_box(pic);
    }
}

/// Encode a stream in the input format (for seeds and regression files).
pub fn encode(width: u16, height: u16, extradata: &[u8], packets: &[&[u8]]) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend(width.to_le_bytes());
    v.extend(height.to_le_bytes());
    for r in std::iter::once(extradata).chain(packets.iter().copied()) {
        v.extend((r.len() as u32).to_le_bytes());
        v.extend(r);
    }
    v
}
