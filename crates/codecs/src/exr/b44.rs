//! B44 and B44A: half-float channels in 4×4 blocks of 14 bytes (B44A: 3
//! bytes for a block of one value); other channels are stored as they are.
//! Written from the OpenEXR reference implementation (BSD-3-Clause).

// Reads files from anywhere: no position in a list is taken on trust (`get`,
// checked slices), so a damaged or hostile file is an error, never a crash.
#![cfg_attr(not(test), deny(clippy::indexing_slicing))]

use super::{fail, ChannelLayout, PixelType};
use crate::Result;

/// A 14-byte block: a starting value and 6-bit differences, scaled by a shift.
fn unpack14(b: &[u8; 14], s: &mut [u16; 16]) {
    let b: [u32; 14] = b.map(u32::from);
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
fn unpack3(b: &[u8; 3], s: &mut [u16; 16]) {
    let mut v = u16::from_be_bytes([b[0], b[1]]);
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
            let Some(raw) = at.checked_add(n).and_then(|end| data.get(at..end)) else { return fail("B44 data cut short") };
            planes.push(raw.chunks_exact(2).map(|b| u16::from_le_bytes([b.first().copied().unwrap_or(0), b.get(1).copied().unwrap_or(0)])).collect());
            at += n;
            continue;
        }
        let mut p = vec![0u16; nx * ny];
        let mut s = [0u16; 16];
        for y in (0..ny).step_by(4) {
            for x in (0..nx).step_by(4) {
                let block = |len: usize| at.checked_add(len).and_then(|end| data.get(at..end));
                let Some(&[_, _, kind]) = block(3) else { return fail("B44 data cut short") };
                if kind >= 13 << 2 {
                    let Some(Ok(b)) = block(3).map(<&[u8; 3]>::try_from) else { return fail("B44 data cut short") };
                    unpack3(b, &mut s);
                    at += 3;
                } else {
                    let Some(Ok(b)) = block(14).map(<&[u8; 14]>::try_from) else { return fail("B44 data cut short") };
                    unpack14(b, &mut s);
                    at += 14;
                }
                let n = (nx - x).min(4);
                for (r, src) in s.chunks_exact(4).take(ny - y).enumerate() {
                    let row = (y + r) * nx + x;
                    if let (Some(d), Some(v)) = (p.get_mut(row..row + n), src.get(..n)) {
                        d.copy_from_slice(v);
                    }
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
            let (Some(plane), Some(row)) = (planes.get(ci), rows.get_mut(ci)) else { return fail("B44 channel list") };
            let Some(vals) = plane.get(*row * n..*row * n + n) else { return fail("B44 block size mismatch") };
            for v in vals {
                let Some(d) = out.get_mut(o..o + 2) else { return fail("B44 block size mismatch") };
                d.copy_from_slice(&v.to_le_bytes());
                o += 2;
            }
            *row += 1;
        }
    }
    if o != out.len() {
        return fail("B44 block size mismatch");
    }
    Ok(())
}
