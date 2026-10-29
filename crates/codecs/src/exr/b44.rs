//! B44 and B44A: half-float channels in 4×4 blocks of 14 bytes (B44A: 3
//! bytes for a block of one value); other channels are stored as they are.
//! Written from the OpenEXR reference implementation (BSD-3-Clause).

use super::{fail, ChannelLayout, PixelType};
use crate::Result;

/// A 14-byte block: a starting value and 6-bit differences, scaled by a shift.
fn unpack14(b: &[u8], s: &mut [u16; 16]) {
    let b: [u32; 14] = std::array::from_fn(|i| b[i] as u32);
    let shift = b[2] >> 2;
    let bias = 0x20u32 << shift;
    let w = |v: u32| v as u16;
    s[0] = w((b[0] << 8) | b[1]);
    let d = |prev: u16, v: u32| w((prev as u32).wrapping_add((v & 0x3f) << shift).wrapping_sub(bias));
    s[4] = d(s[0], (b[2] << 4) | (b[3] >> 4));
    s[8] = d(s[4], (b[3] << 2) | (b[4] >> 6));
    s[12] = d(s[8], b[4]);
    s[1] = d(s[0], b[5] >> 2);
    s[5] = d(s[4], (b[5] << 4) | (b[6] >> 4));
    s[9] = d(s[8], (b[6] << 2) | (b[7] >> 6));
    s[13] = d(s[12], b[7]);
    s[2] = d(s[1], b[8] >> 2);
    s[6] = d(s[5], (b[8] << 4) | (b[9] >> 4));
    s[10] = d(s[9], (b[9] << 2) | (b[10] >> 6));
    s[14] = d(s[13], b[10]);
    s[3] = d(s[2], b[11] >> 2);
    s[7] = d(s[6], (b[11] << 4) | (b[12] >> 4));
    s[11] = d(s[10], (b[12] << 2) | (b[13] >> 6));
    s[15] = d(s[14], b[13]);
    for v in s.iter_mut() {
        *v = if *v & 0x8000 != 0 { *v & 0x7fff } else { !*v };
    }
}

/// A 3-byte block: one value for all 16.
fn unpack3(b: &[u8], s: &mut [u16; 16]) {
    let mut v = ((b[0] as u16) << 8) | b[1] as u16;
    v = if v & 0x8000 != 0 { v & 0x7fff } else { !v };
    *s = [v; 16];
}

pub fn decompress(data: &[u8], layout: &ChannelLayout, out: &mut [u8]) -> Result<()> {
    if layout.channels.iter().any(|c| c.linear && c.ty == PixelType::Half) {
        return Err(crate::Error::Unsupported("B44 with perceptually-linear channels".into()));
    }
    // Each channel planar (as 16-bit units), as the blocks are laid out.
    let mut planes: Vec<Vec<u16>> = Vec::with_capacity(layout.channels.len());
    let mut at = 0usize;
    for c in &layout.channels {
        let (nx, ny) = (c.nx, c.ny);
        if c.ty != PixelType::Half {
            let n = nx * ny * c.units * 2;
            if data.len() < at + n {
                return fail("B44 data cut short");
            }
            planes.push(data[at..at + n].chunks_exact(2).map(|b| u16::from_le_bytes([b[0], b[1]])).collect());
            at += n;
            continue;
        }
        let mut p = vec![0u16; nx * ny];
        let mut s = [0u16; 16];
        for y in (0..ny).step_by(4) {
            for x in (0..nx).step_by(4) {
                if data.len() < at + 3 {
                    return fail("B44 data cut short");
                }
                if data[at + 2] >= 13 << 2 {
                    unpack3(&data[at..], &mut s);
                    at += 3;
                } else {
                    if data.len() < at + 14 {
                        return fail("B44 data cut short");
                    }
                    unpack14(&data[at..], &mut s);
                    at += 14;
                }
                let n = (nx - x).min(4);
                for r in 0..4.min(ny - y) {
                    p[(y + r) * nx + x..(y + r) * nx + x + n].copy_from_slice(&s[r * 4..r * 4 + n]);
                }
            }
        }
        planes.push(p);
    }
    // Back to the interleaved line layout.
    let mut o = 0;
    let mut rows = vec![0usize; layout.channels.len()];
    for y in layout.y0..layout.y0 + layout.lines as i32 {
        for (ci, c) in layout.channels.iter().enumerate() {
            if y.rem_euclid(c.ys) != 0 {
                continue;
            }
            let n = c.nx * c.units;
            for v in &planes[ci][rows[ci] * n..rows[ci] * n + n] {
                out[o..o + 2].copy_from_slice(&v.to_le_bytes());
                o += 2;
            }
            rows[ci] += 1;
        }
    }
    if o != out.len() {
        return fail("B44 block size mismatch");
    }
    Ok(())
}
