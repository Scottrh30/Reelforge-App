//! OpenEXR's ZSTD compression (OpenEXR 3.5): one scan line (or one tile) a chunk, lossless.
//!
//! A chunk is a 24-byte prefix (all little-endian: the magic "zstd-exr" as a u64, the wire
//! format, its flags, the length of what follows) and one Zstandard frame. The frame holds
//! one or two segments, each a u64 length and that many bytes: every 16-bit channel row of
//! the chunk (line by line, channel by channel), then every 32-bit row, each segment with
//! its bytes split into planes (all first bytes, then all second bytes…). Wire format 2 with
//! flag 1 also differences each channel row's samples (as 16- or 32-bit integers). A chunk
//! that wouldn't shrink is stored as it is, with no prefix.

use super::{fail, ChannelLayout, Result};
use std::io::Read;

/// "zstd-exr" (as OpenEXR writes it).
const MAGIC: u64 = 8248453963162350458;
const HEADER: usize = 24;
/// Wire format 2's flag: rows differenced after sorting.
const DELTA: u32 = 1;

fn u32_at(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(at..at + 4)?.try_into().ok()?))
}

fn u64_at(d: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(d.get(at..at + 8)?.try_into().ok()?))
}

/// One chunk (`size` bytes uncompressed, in `layout`).
pub(super) fn decompress(data: &[u8], layout: &ChannelLayout, size: usize) -> Result<Vec<u8>> {
    if data.len() < HEADER || u64_at(data, 0) != Some(MAGIC) {
        // stored as it is
        if data.len() != size {
            return fail("ZSTD chunk size");
        }
        return Ok(data.to_vec());
    }
    let (Some(format), Some(flags), Some(len)) = (u32_at(data, 8), u32_at(data, 12), u64_at(data, 16)) else { return fail("ZSTD chunk header") };
    let delta = match (format, flags) {
        (1, 0) => false,
        (2, f) if f & !DELTA == 0 => f & DELTA != 0,
        _ => return fail("ZSTD chunk format"),
    };
    if usize::try_from(len).ok() != Some(data.len() - HEADER) {
        return fail("ZSTD chunk length");
    }
    // The inner stream is the chunk and at most two 8-byte lengths: never let the frame
    // say it's bigger.
    let cap = size.checked_add(16).ok_or(super::Error::Invalid("ZSTD chunk size".into()))?;
    let mut inner = Vec::with_capacity(cap.min(1 << 28));
    let frame = data.get(HEADER..).unwrap_or_default();
    let mut dec = match ruzstd::decoding::StreamingDecoder::new(frame) {
        Ok(d) => d,
        Err(e) => return fail(&format!("ZSTD frame: {e}")),
    };
    if let Err(e) = (&mut dec).take(cap as u64 + 1).read_to_end(&mut inner) {
        return fail(&format!("ZSTD frame: {e}"));
    }
    if inner.len() > cap {
        return fail("ZSTD frame too long");
    }

    // Every channel row of the chunk: (line, channel) -> samples, bytes per sample.
    let mut rows = Vec::new();
    for line in 0..layout.lines {
        let y = layout.y0 as i64 + line as i64;
        for c in &layout.channels {
            let n = if c.ys <= 1 || y % c.ys as i64 == 0 { c.nx } else { 0 };
            rows.push((n, c.units * 2));
        }
    }
    let total: usize = rows.iter().map(|&(n, b)| n * b).sum();
    if total != size {
        return fail("ZSTD chunk layout");
    }
    let split: usize = rows.iter().filter(|r| r.1 == 2).map(|&(n, b)| n * b).sum();
    let segments: &[(usize, usize)] = if split > 0 && split < size { &[(split, 2), (size - split, 4)] } else if split == 0 { &[(size, 4)] } else { &[(size, 2)] };

    // Unshuffle each segment into the sorted buffer.
    let mut sorted = vec![0u8; size];
    let (mut q, mut at) = (0usize, 0usize);
    for &(len, el) in segments {
        if u64_at(&inner, q) != Some(len as u64) {
            return fail("ZSTD segment length");
        }
        q += 8;
        let (Some(src), Some(dst)) = (inner.get(q..q + len), sorted.get_mut(at..at + len)) else { return fail("ZSTD segment") };
        let n = len / el;
        for (i, out) in dst.chunks_exact_mut(el).enumerate() {
            for (b, o) in out.iter_mut().enumerate() {
                *o = src.get(b * n + i).copied().unwrap_or(0);
            }
        }
        q += len;
        at += len;
    }
    if q != inner.len() {
        return fail("ZSTD chunk has extra bytes");
    }

    // Undo the differences, row by row (16-bit rows first, then 32-bit, as sorted).
    if delta {
        let mut off = 0usize;
        for bytes in [2usize, 4] {
            for &(n, b) in rows.iter().filter(|r| r.1 == bytes) {
                let Some(row) = sorted.get_mut(off..off + n * b) else { return fail("ZSTD rows") };
                if b == 2 {
                    let mut prev = 0u16;
                    for s in row.chunks_exact_mut(2) {
                        prev = prev.wrapping_add(u16::from_le_bytes(s.try_into().unwrap_or_default()));
                        s.copy_from_slice(&prev.to_le_bytes());
                    }
                } else {
                    let mut prev = 0u32;
                    for s in row.chunks_exact_mut(4) {
                        prev = prev.wrapping_add(u32::from_le_bytes(s.try_into().unwrap_or_default()));
                        s.copy_from_slice(&prev.to_le_bytes());
                    }
                }
                off += n * b;
            }
        }
    }

    // Back to the usual layout: each line's channels in turn.
    let mut out = Vec::with_capacity(size);
    let (mut half_at, mut full_at) = (0usize, split);
    for &(n, b) in &rows {
        let at = if b == 2 { &mut half_at } else { &mut full_at };
        let Some(row) = sorted.get(*at..*at + n * b) else { return fail("ZSTD rows") };
        out.extend_from_slice(row);
        *at += n * b;
    }
    Ok(out)
}
