//! PIZ: a lookup table that packs the 16-bit values used into a smaller range,
//! a Haar-like wavelet per channel, and Huffman coding of the result (with a
//! run-length code for repeats). Written from the OpenEXR file format
//! documentation and its reference implementation (BSD-3-Clause).

// Reads files from anywhere: no position in a list is taken on trust (`get`,
// checked slices), so a damaged or hostile file is an error, never a crash.
#![cfg_attr(not(test), deny(clippy::indexing_slicing))]

use super::{fail, ChannelLayout};
use crate::Result;

const USHORT_RANGE: usize = 1 << 16;
const BITMAP_SIZE: usize = USHORT_RANGE >> 3;

const HUF_ENCSIZE: usize = (1 << 16) + 1;
const SHORT_ZEROCODE_RUN: u64 = 59;
const LONG_ZEROCODE_RUN: u64 = 63;
const SHORTEST_LONG_RUN: u64 = 2 + LONG_ZEROCODE_RUN - SHORT_ZEROCODE_RUN;
/// Codes this long or shorter are found with one table lookup.
const DEC_BITS: u32 = 12;

/// Decompress one PIZ block into `out` (the uncompressed block layout:
/// line by line, channel by channel, little-endian).
pub fn decompress(data: &[u8], layout: &ChannelLayout, out: &mut [u8]) -> Result<()> {
    if data.len() < 4 {
        return fail("PIZ block too short");
    }
    let Some(&[a, b, c, d]) = data.get(..4) else { return fail("PIZ block too short") };
    let (min_nz, max_nz) = (u16::from_le_bytes([a, b]) as usize, u16::from_le_bytes([c, d]) as usize);
    if max_nz >= BITMAP_SIZE {
        return fail("PIZ bitmap range");
    }
    let mut bitmap = vec![0u8; BITMAP_SIZE];
    let mut at = 4;
    if min_nz <= max_nz {
        let n = max_nz - min_nz + 1;
        let (Some(dst), Some(src)) = (bitmap.get_mut(min_nz..=max_nz), data.get(at..at + n)) else { return fail("PIZ bitmap cut short") };
        dst.copy_from_slice(src);
        at += n;
    }
    let (lut, max_value) = reverse_lut(&bitmap);
    if data.len() < at + 4 {
        return fail("PIZ length missing");
    }
    let len = i32::from_le_bytes(super::le4(data, at)?);
    at += 4;
    let Some(packed) = usize::try_from(len).ok().and_then(|len| data.get(at..at.checked_add(len)?)) else { return fail("PIZ data cut short") };
    // Every channel's samples, planar, as 16-bit units (a 32-bit sample is two).
    let total: usize = layout.channels.iter().map(|c| c.nx * c.ny * c.units).sum();
    let mut tmp = vec![0u16; total];
    huf_uncompress(packed, &mut tmp)?;

    let mut start = 0;
    let mut starts = Vec::with_capacity(layout.channels.len());
    for c in &layout.channels {
        starts.push(start);
        let n = c.nx * c.ny * c.units;
        let Some(plane) = tmp.get_mut(start..start + n) else { return fail("PIZ channel sizes") };
        for j in 0..c.units {
            wav2_decode(plane, j, c.nx, c.units, c.ny, c.nx * c.units, max_value);
        }
        start += n;
    }
    for v in tmp.iter_mut() {
        *v = lut.get(*v as usize).copied().unwrap_or(0);
    }
    // Back to the interleaved line layout.
    let mut o = 0;
    let mut ends = starts.clone();
    for y in layout.y0..layout.y0 + layout.lines as i32 {
        for (ci, c) in layout.channels.iter().enumerate() {
            if y.rem_euclid(c.ys) != 0 {
                continue;
            }
            let n = c.nx * c.units;
            let Some(end) = ends.get_mut(ci) else { return fail("PIZ channel list") };
            let (Some(src), Some(dst)) = (tmp.get(*end..*end + n), out.get_mut(o..o + n * 2)) else { return fail("PIZ block size mismatch") };
            if cfg!(target_endian = "little") {
                // SAFETY: u16s are plain bytes, already little-endian here.
                dst.copy_from_slice(unsafe { std::slice::from_raw_parts(src.as_ptr() as *const u8, n * 2) });
            } else {
                for (d, v) in dst.chunks_exact_mut(2).zip(src) {
                    d.copy_from_slice(&v.to_le_bytes());
                }
            }
            o += n * 2;
            *end += n;
        }
    }
    if o != out.len() {
        return fail("PIZ block size mismatch");
    }
    Ok(())
}

/// The values the bitmap marks (0 always), in order: (table, highest index).
fn reverse_lut(bitmap: &[u8]) -> (Vec<u16>, u16) {
    let mut lut = vec![0u16; USHORT_RANGE];
    let mut k = 0usize;
    for i in 0..USHORT_RANGE {
        if i == 0 || bitmap.get(i >> 3).is_some_and(|b| b & (1 << (i & 7)) != 0) {
            if let Some(l) = lut.get_mut(k) {
                *l = i as u16;
            }
            k += 1;
        }
    }
    (lut, k.saturating_sub(1) as u16)
}

// ---- Wavelet ----------------------------------------------------------------

#[inline(always)]
fn wdec14(l: u16, h: u16) -> (u16, u16) {
    let ls = l as i16 as i32;
    let hi = h as i16 as i32;
    let ai = ls + (hi & 1) + (hi >> 1);
    (ai as i16 as u16, (ai - hi) as i16 as u16)
}

#[inline(always)]
fn wdec16(l: u16, h: u16) -> (u16, u16) {
    const A_OFFSET: i32 = 1 << 15;
    const MOD_MASK: i32 = (1 << 16) - 1;
    let (m, d) = (l as i32, h as i32);
    let bb = (m - (d >> 1)) & MOD_MASK;
    let aa = (d + bb - A_OFFSET) & MOD_MASK;
    (aa as u16, bb as u16)
}

/// Undo the 2D wavelet of an `nx`×`ny` array whose elements are `ox` apart
/// (starting at `base`) and rows `oy` apart.
fn wav2_decode(buf: &mut [u16], base: usize, nx: usize, ox: usize, ny: usize, oy: usize, mx: u16) {
    // Two copies of the loop, so the step is inlined in each.
    if (mx as u32) < (1 << 14) {
        wav2_decode_with::<true>(buf, base, nx, ox, ny, oy)
    } else {
        wav2_decode_with::<false>(buf, base, nx, ox, ny, oy)
    }
}

// (Positions follow from nx, ny and the spacing alone, all checked against
// the buffer up front; plain indexing keeps the loop fast.)
#[allow(clippy::indexing_slicing)]
fn wav2_decode_with<const W14: bool>(buf: &mut [u16], base: usize, nx: usize, ox: usize, ny: usize, oy: usize) {
    // The furthest element is base + oy·(ny − 1) + ox·(nx − 1): in the buffer,
    // or nothing is done (a damaged file's sizes; the size check after says so).
    let furthest = (|| base.checked_add(oy.checked_mul(ny.checked_sub(1)?)?)?.checked_add(ox.checked_mul(nx.checked_sub(1)?)?))();
    if ox == 0 || oy == 0 || furthest.is_none_or(|f| f >= buf.len()) {
        return;
    }
    #[inline(always)]
    fn dec<const W14: bool>(l: u16, h: u16) -> (u16, u16) {
        if W14 { wdec14(l, h) } else { wdec16(l, h) }
    }
    let dec = dec::<W14>;
    let n = nx.min(ny);
    let mut p = 1usize;
    while p <= n {
        p <<= 1;
    }
    p >>= 1;
    let mut p2 = p;
    p >>= 1;
    while p >= 1 {
        let (oy1, oy2, ox1, ox2) = (oy * p, oy * p2, ox * p, ox * p2);
        let ey = base + oy * (ny - p2);
        let mut py = base;
        while py <= ey {
            let ex = py + ox * (nx - p2);
            let mut px = py;
            while px <= ex {
                let (p01, p10) = (px + ox1, px + oy1);
                let p11 = p10 + ox1;
                let (i00, i10) = dec(buf[px], buf[p10]);
                let (i01, i11) = dec(buf[p01], buf[p11]);
                (buf[px], buf[p01]) = dec(i00, i01);
                (buf[p10], buf[p11]) = dec(i10, i11);
                px += ox2;
            }
            // An odd column.
            if nx & p != 0 {
                let p10 = px + oy1;
                (buf[px], buf[p10]) = dec(buf[px], buf[p10]);
            }
            py += oy2;
        }
        // An odd row.
        if ny & p != 0 {
            let ex = py + ox * (nx - p2);
            let mut px = py;
            while px <= ex {
                let p01 = px + ox1;
                (buf[px], buf[p01]) = dec(buf[px], buf[p01]);
                px += ox2;
            }
        }
        p2 = p;
        p >>= 1;
    }
}

// ---- Huffman ----------------------------------------------------------------

/// MSB-first bits for reading the code-length table.
struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
}

impl Bits<'_> {
    fn read(&mut self, n: u32) -> u64 {
        let mut v = 0u64;
        for _ in 0..n {
            let byte = self.data.get(self.pos >> 3).copied().unwrap_or(0);
            v = (v << 1) | ((byte >> (7 - (self.pos & 7))) & 1) as u64;
            self.pos += 1;
        }
        v
    }
}

pub(super) fn huf_uncompress(data: &[u8], out: &mut [u16]) -> Result<()> {
    if out.is_empty() {
        return Ok(());
    }
    if data.len() < 20 {
        return fail("PIZ Huffman header");
    }
    let rd = |i: usize| super::le4(data, i).map(|b| u32::from_le_bytes(b) as usize);
    let (im, i_max, n_bits) = (rd(0)?, rd(4)?, rd(12)?);
    if im >= HUF_ENCSIZE || i_max >= HUF_ENCSIZE || im > i_max {
        return fail("PIZ Huffman table range");
    }
    // Code lengths of symbols im..=i_max, packed 6 bits each, with runs of zeros.
    let mut lens = vec![0u8; i_max - im + 1];
    let mut b = Bits { data: data.get(20..).unwrap_or_default(), pos: 0 };
    let mut i = 0usize;
    while i < lens.len() {
        if b.pos / 8 >= b.data.len() {
            return fail("PIZ Huffman table cut short");
        }
        let l = b.read(6);
        if l == LONG_ZEROCODE_RUN {
            let run = (b.read(8) + SHORTEST_LONG_RUN) as usize;
            if i + run > lens.len() {
                return fail("PIZ Huffman zero run");
            }
            i += run;
        } else if l >= SHORT_ZEROCODE_RUN {
            let run = (l - SHORT_ZEROCODE_RUN + 2) as usize;
            if i + run > lens.len() {
                return fail("PIZ Huffman zero run");
            }
            i += run;
        } else {
            if let Some(x) = lens.get_mut(i) {
                *x = l as u8;
            }
            i += 1;
        }
    }
    let table_bytes = b.pos.div_ceil(8);
    // (the table's last code can end past the data)
    let Some(stream) = data.get(20 + table_bytes..) else {
        return fail("PIZ Huffman table cut short");
    };
    if n_bits > stream.len() * 8 {
        return fail("PIZ Huffman bit count");
    }

    // Canonical codes, as OpenEXR assigns them: per length, longest codes
    // first numerically, symbols in order within a length.
    // (lengths are under 59: longer six-bit values are the zero runs above)
    let mut count = [0u64; 59];
    for &l in &lens {
        if let Some(n) = count.get_mut(l as usize) {
            *n += 1;
        }
    }
    count[0] = 0;
    let mut first = [0u64; 59];
    let mut c = 0u64;
    for (f, &n) in first.iter_mut().zip(&count).skip(1).rev() {
        let nc = (c + n) >> 1;
        *f = c;
        c = nc;
    }
    // Per length: the symbols in code order.
    let mut by_len: Vec<Vec<u32>> = vec![Vec::new(); 59];
    for (k, &l) in lens.iter().enumerate() {
        if let (true, Some(v)) = (l > 0, by_len.get_mut(l as usize)) {
            v.push((im + k) as u32);
        }
    }
    let first_of = |l: usize| first.get(l).copied().unwrap_or(0);
    let syms = |l: usize| by_len.get(l).map(Vec::as_slice).unwrap_or_default();
    // Short codes: one lookup of DEC_BITS bits gives (symbol, length).
    let mut fast = vec![(0u32, 0u8); 1 << DEC_BITS];
    for l in 1..=DEC_BITS as usize {
        for (k, &s) in syms(l).iter().enumerate() {
            let code = first_of(l) + k as u64;
            let shift = DEC_BITS as usize - l;
            let base = (code << shift) as usize;
            let Some(span) = fast.get_mut(base..base + (1 << shift)) else { return fail("PIZ Huffman code overflow") };
            span.fill((s, l as u8));
        }
    }
    let long_lens: Vec<usize> = (DEC_BITS as usize + 1..=58).filter(|&l| !syms(l).is_empty()).collect();

    let mut bits = crate::bits::BitReader::new(stream.get(..n_bits.div_ceil(8)).unwrap_or_default());
    let mut o = 0usize;
    let rlc = i_max as u32;
    // Bits used so far (kept here: cheaper than asking the reader each time).
    let mut used = 0usize;
    while used < n_bits {
        let e = fast.get(bits.peek(DEC_BITS) as usize).copied().unwrap_or((0, 0));
        let (sym, len) = if e.1 > 0 {
            e
        } else {
            let mut found = None;
            for &l in &long_lens {
                // Codes up to 32 bits come from one look; longer ones (rare) from two.
                let code = if l <= 32 { bits.peek(l as u32) as u64 } else { ((bits.peek(32) as u64) << (l - 32)) | peek_after(&bits, 32, l as u32 - 32) };
                if let Some(&s) = code.checked_sub(first_of(l)).and_then(|k| syms(l).get(usize::try_from(k).ok()?)) {
                    found = Some((s, l as u8));
                    break;
                }
            }
            match found {
                Some(f) => f,
                None => return fail("PIZ Huffman: no such code"),
            }
        };
        bits.skip(len as usize);
        used += len as usize;
        if used > n_bits {
            return fail("PIZ Huffman: past the end");
        }
        if sym == rlc {
            // A run: repeat the last value.
            let n = bits.read(8) as usize;
            used += 8;
            let v = o.checked_sub(1).and_then(|p| out.get(p)).copied();
            let (true, Some(v), Some(span)) = (used <= n_bits, v, out.get_mut(o..o + n)) else { return fail("PIZ Huffman run") };
            span.fill(v);
            o += n;
        } else {
            let Some(slot) = out.get_mut(o) else { return fail("PIZ Huffman: too much data") };
            *slot = sym as u16;
            o += 1;
        }
    }
    if o != out.len() {
        return fail("PIZ Huffman: too little data");
    }
    Ok(())
}

/// `n` bits starting `skip` bits ahead, without moving.
fn peek_after(bits: &crate::bits::BitReader, skip: u32, n: u32) -> u64 {
    // A copy is cheap, and this only runs for codes longer than 32 bits.
    let mut b = bits.clone();
    b.skip(skip as usize);
    b.peek(n) as u64
}
