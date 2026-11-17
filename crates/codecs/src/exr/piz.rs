//! PIZ: a lookup table that packs the 16-bit values used into a smaller range,
//! a Haar-like wavelet per channel, and Huffman coding of the result (with a
//! run-length code for repeats). Written from the OpenEXR file format
//! documentation and its reference implementation (BSD-3-Clause).

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
    let rd16 = |i: usize| u16::from_le_bytes([data[i], data[i + 1]]) as usize;
    let (min_nz, max_nz) = (rd16(0), rd16(2));
    if max_nz >= BITMAP_SIZE {
        return fail("PIZ bitmap range");
    }
    let mut bitmap = vec![0u8; BITMAP_SIZE];
    let mut at = 4;
    if min_nz <= max_nz {
        let n = max_nz - min_nz + 1;
        if data.len() < at + n {
            return fail("PIZ bitmap cut short");
        }
        bitmap[min_nz..=max_nz].copy_from_slice(&data[at..at + n]);
        at += n;
    }
    let (lut, max_value) = reverse_lut(&bitmap);
    if data.len() < at + 4 {
        return fail("PIZ length missing");
    }
    let len = i32::from_le_bytes(data[at..at + 4].try_into().unwrap());
    at += 4;
    if len < 0 || data.len() < at + len as usize {
        return fail("PIZ data cut short");
    }
    // Every channel's samples, planar, as 16-bit units (a 32-bit sample is two).
    let total: usize = layout.channels.iter().map(|c| c.nx * c.ny * c.units).sum();
    let mut tmp = vec![0u16; total];
    huf_uncompress(&data[at..at + len as usize], &mut tmp)?;

    let mut start = 0;
    let mut starts = Vec::with_capacity(layout.channels.len());
    for c in &layout.channels {
        starts.push(start);
        let n = c.nx * c.ny * c.units;
        for j in 0..c.units {
            wav2_decode(&mut tmp[start..start + n], j, c.nx, c.units, c.ny, c.nx * c.units, max_value);
        }
        start += n;
    }
    for v in tmp.iter_mut() {
        *v = lut[*v as usize];
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
            let src = &tmp[ends[ci]..ends[ci] + n];
            if cfg!(target_endian = "little") {
                // SAFETY: u16s are plain bytes, already little-endian here.
                out[o..o + n * 2].copy_from_slice(unsafe { std::slice::from_raw_parts(src.as_ptr() as *const u8, n * 2) });
            } else {
                for (d, v) in out[o..o + n * 2].chunks_exact_mut(2).zip(src) {
                    d.copy_from_slice(&v.to_le_bytes());
                }
            }
            o += n * 2;
            ends[ci] += n;
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
        if i == 0 || bitmap[i >> 3] & (1 << (i & 7)) != 0 {
            lut[k] = i as u16;
            k += 1;
        }
    }
    (lut, (k - 1) as u16)
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

fn wav2_decode_with<const W14: bool>(buf: &mut [u16], base: usize, nx: usize, ox: usize, ny: usize, oy: usize) {
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

fn huf_uncompress(data: &[u8], out: &mut [u16]) -> Result<()> {
    if out.is_empty() {
        return Ok(());
    }
    if data.len() < 20 {
        return fail("PIZ Huffman header");
    }
    let rd = |i: usize| u32::from_le_bytes(data[i..i + 4].try_into().unwrap()) as usize;
    let (im, i_max, n_bits) = (rd(0), rd(4), rd(12));
    if im >= HUF_ENCSIZE || i_max >= HUF_ENCSIZE || im > i_max {
        return fail("PIZ Huffman table range");
    }
    // Code lengths of symbols im..=i_max, packed 6 bits each, with runs of zeros.
    let mut lens = vec![0u8; i_max - im + 1];
    let mut b = Bits { data: &data[20..], pos: 0 };
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
            lens[i] = l as u8;
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
    let mut count = [0u64; 59];
    for &l in &lens {
        count[l as usize] += 1;
    }
    count[0] = 0;
    let mut first = [0u64; 59];
    let mut c = 0u64;
    for l in (1..=58).rev() {
        let nc = (c + count[l]) >> 1;
        first[l] = c;
        c = nc;
    }
    // Per length: the symbols in code order.
    let mut by_len: Vec<Vec<u32>> = vec![Vec::new(); 59];
    for (k, &l) in lens.iter().enumerate() {
        if l > 0 {
            by_len[l as usize].push((im + k) as u32);
        }
    }
    // Short codes: one lookup of DEC_BITS bits gives (symbol, length).
    let mut fast = vec![(0u32, 0u8); 1 << DEC_BITS];
    for l in 1..=DEC_BITS as usize {
        for (k, &s) in by_len[l].iter().enumerate() {
            let code = first[l] + k as u64;
            let shift = DEC_BITS as usize - l;
            let base = (code << shift) as usize;
            if base + (1 << shift) > fast.len() {
                return fail("PIZ Huffman code overflow");
            }
            fast[base..base + (1 << shift)].fill((s, l as u8));
        }
    }
    let long_lens: Vec<usize> = (DEC_BITS as usize + 1..=58).filter(|&l| !by_len[l].is_empty()).collect();

    let mut bits = crate::bits::BitReader::new(&stream[..n_bits.div_ceil(8)]);
    let mut o = 0usize;
    let rlc = i_max as u32;
    // Bits used so far (kept here: cheaper than asking the reader each time).
    let mut used = 0usize;
    while used < n_bits {
        let e = fast[bits.peek(DEC_BITS) as usize];
        let (sym, len) = if e.1 > 0 {
            e
        } else {
            let mut found = None;
            for &l in &long_lens {
                // Codes up to 32 bits come from one look; longer ones (rare) from two.
                let code = if l <= 32 { bits.peek(l as u32) as u64 } else { ((bits.peek(32) as u64) << (l - 32)) | peek_after(&bits, 32, l as u32 - 32) };
                if code >= first[l] && code - first[l] < by_len[l].len() as u64 {
                    found = Some((by_len[l][(code - first[l]) as usize], l as u8));
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
            if used > n_bits || o == 0 || o + n > out.len() {
                return fail("PIZ Huffman run");
            }
            let v = out[o - 1];
            out[o..o + n].fill(v);
            o += n;
        } else {
            if o >= out.len() {
                return fail("PIZ Huffman: too much data");
            }
            out[o] = sym as u16;
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
