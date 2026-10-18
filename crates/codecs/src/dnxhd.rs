//! Avid DNxHD and DNxHR (SMPTE VC-3): intra-only, 8×8 DCT, 4:2:2 at 8, 10
//! or 12 bits, and 4:4:4 (RGB or YCbCr) at 10 or 12 bits.
//!
//! A frame (or each field of an interlaced one) starts with a header that
//! names its compression ID (which fixes the code tables and weights), its
//! size and bit depth, and a table of where each macroblock row starts, so
//! rows decode independently (spread over threads). Each macroblock has a
//! quantiser scale, then its blocks: a DC difference (JPEG-style size code
//! plus extra bits) and AC coefficients as level/run codes in zigzag order.

use crate::bits::BitReader;
use crate::dnxhd_tables::{Cid, CIDS};
use crate::idct::{fast_qmat, idct_fast};
use crate::vlc::Vlc;
use crate::{invalid, Decoder, Error, Format, Matrix, Output, Picture, Result, StreamInfo};
use std::sync::OnceLock;

const ZIGZAG: [u8; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20, 13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22,
    15, 23, 30, 37, 44, 51, 58, 59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

/// The decoding tables for one CID, built once.
struct Tables {
    dc: Vlc,
    ac: Vlc,
    run: Vlc,
}

fn tables(i: usize) -> &'static Tables {
    static T: OnceLock<Vec<OnceLock<Tables>>> = OnceLock::new();
    let all = T.get_or_init(|| (0..CIDS.len()).map(|_| OnceLock::new()).collect());
    all[i].get_or_init(|| {
        let c = &CIDS[i];
        let wide = |v: &[u16]| v.iter().map(|&x| x as u32).collect::<Vec<u32>>();
        let dc_codes: Vec<u32> = c.dc_codes.iter().map(|&x| x as u32).collect();
        let run_syms: Vec<u16> = c.run.iter().map(|&x| x as u16).collect();
        Tables {
            dc: Vlc::new(&dc_codes, c.dc_bits, None),
            ac: Vlc::new(&wide(c.ac_codes), c.ac_bits, None),
            run: Vlc::new(&wide(c.run_codes), c.run_bits, Some(&run_syms)),
        }
    })
}

/// How coefficient levels are rescaled, by bit depth and chroma format.
#[derive(Clone, Copy)]
struct LevelRule {
    index_bits: u32,
    bias: i32,
    shift: u32,
    dc_shift: u32,
}

struct Header {
    cid: usize,
    width: u32,
    height: u32,
    bit_depth: u32,
    is_444: bool,
    /// 4:4:4 stored as YCbCr (else RGB, as G, B, R).
    act: bool,
    interlaced: bool,
    /// Which field this coding unit holds (0 = top).
    field: u32,
    mbaff: bool,
    mb_width: u32,
    mb_height: u32,
    matrix: Matrix,
    data_offset: usize,
    row_offsets: Vec<u32>,
    rule: LevelRule,
}

fn header(buf: &[u8]) -> Result<Header> {
    if buf.len() < 0x280 {
        return invalid("DNxHD: frame too small");
    }
    let prefix = ((u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as u64) << 16) | (buf[4] as u64) << 8;
    let hr = {
        let off = prefix >> 16;
        (prefix & 0xFFFF_0000_FFFF) == 0x0300 && (0x0280..=0x2170).contains(&off) && off & 3 == 0
    };
    if prefix != 0x0000_0280_0100 && prefix != 0x0000_0280_0200 && !hr {
        return invalid("DNxHD: not a VC-3 frame header");
    }
    let interlaced = buf[5] & 2 != 0;
    let field = if interlaced { (buf[5] & 1) as u32 } else { 0 };
    let mbaff = (buf[6] >> 5) & 1 == 1;
    if buf[7] & 1 != 0 {
        return Err(Error::Unsupported("DNxHD with alpha".into()));
    }
    let mut height = u16::from_be_bytes([buf[0x18], buf[0x19]]) as u32;
    let mut width = u16::from_be_bytes([buf[0x1a], buf[0x1b]]) as u32;
    let bit_depth = match buf[0x21] >> 5 {
        1 => 8,
        2 => 10,
        3 => 12,
        _ => return invalid("DNxHD: unknown bit depth"),
    };
    let cid_num = u32::from_be_bytes([buf[0x28], buf[0x29], buf[0x2a], buf[0x2b]]);
    let cid = CIDS.iter().position(|c| c.cid == cid_num).ok_or_else(|| Error::Unsupported(format!("DNxHD compression ID {cid_num}")))?;
    let c: &Cid = &CIDS[cid];
    if c.bit_depth != 0 && c.bit_depth != bit_depth {
        return invalid("DNxHD: bit depth doesn't match the compression ID");
    }
    let matrix = match (buf[0x2c] >> 1) & 3 {
        0 => Matrix::Bt709,
        1 | 2 => Matrix::Bt2020,
        _ => Matrix::Bt709,
    };
    let act = buf[0x2c] & 1 == 1;
    let is_444 = (buf[0x2c] >> 6) & 1 == 1;
    if is_444 && bit_depth == 8 {
        return Err(Error::Unsupported("DNxHD 4:4:4 at 8 bits".into()));
    }
    let hqx = cid_num == 1271;
    let rule = match (bit_depth, is_444 || hqx) {
        (8, _) => LevelRule { index_bits: 4, bias: 32, shift: 6, dc_shift: 0 },
        (10, false) => LevelRule { index_bits: 6, bias: 8, shift: 4, dc_shift: 0 },
        (10, true) => LevelRule { index_bits: 6, bias: 32, shift: 6, dc_shift: 0 },
        (_, false) => LevelRule { index_bits: 6, bias: 8, shift: 4, dc_shift: 2 },
        (_, true) => LevelRule { index_bits: 6, bias: 32, shift: 4, dc_shift: 2 },
    };
    // DNxHD 100 stores 1920/1280 wide as 1440/960 (anamorphic); show it full width.
    if c.width != 0 && width != c.width {
        width = c.width;
    }
    if buf.len() < 0x170 + 2 {
        return invalid("DNxHD: header cut short");
    }
    let mb_width = width.div_ceil(16);
    let mb_height = u16::from_be_bytes([buf[0x16c], buf[0x16d]]) as u32;
    if interlaced && height.div_ceil(16) == mb_height {
        height <<= 1;
    }
    let data_offset = if mb_height > 68 && hr {
        0x170 + ((mb_height as usize) << 2)
    } else if mb_height > 68 {
        return invalid("DNxHD: too many macroblock rows");
    } else {
        0x280
    };
    if (mb_height << interlaced as u32) > height.div_ceil(16) + 1 || buf.len() < data_offset {
        return invalid("DNxHD: bad picture size");
    }
    let mut row_offsets = Vec::with_capacity(mb_height as usize);
    for i in 0..mb_height as usize {
        let p = 0x170 + i * 4;
        let off = u32::from_be_bytes([buf[p], buf[p + 1], buf[p + 2], buf[p + 3]]);
        if off as usize > buf.len() - data_offset {
            return invalid("DNxHD: row starts past the frame");
        }
        row_offsets.push(off);
    }
    if width == 0 || height == 0 {
        return invalid("DNxHD: empty picture");
    }
    Ok(Header { cid, width, height, bit_depth, is_444, act, interlaced, field, mbaff, mb_width, mb_height, matrix, data_offset, row_offsets, rule })
}

/// One decoded macroblock row: planes of 16-bit samples (`16 × mb_width × 16` luma).
struct Row {
    y: Vec<u16>,
    u: Vec<u16>,
    v: Vec<u16>,
    /// The macroblocks' colour flag (4:4:4: YCbCr when set), from the first one.
    act: Option<bool>,
}

fn decode_row(h: &Header, t: &Tables, data: &[u8]) -> Result<Row> {
    let c = &CIDS[h.cid];
    let mut r = BitReader::new(data);
    let w = h.mb_width as usize * 16;
    let cw = if h.is_444 { w } else { w / 2 };
    let mut row = Row { y: vec![0; w * 16], u: vec![0; cw * 16], v: vec![0; cw * 16], act: None };
    let mut last_dc = [1i32 << (h.bit_depth + 2); 3];
    let rule = h.rule;
    let max = ((1u32 << h.bit_depth) - 1) as f32;
    let q = fast_qmat(&[1; 64], 1.0);
    let nblocks = if h.is_444 { 12 } else { 8 };
    let mut blocks = vec![[0i32; 64]; nblocks];
    let mut luma_scale = [0i32; 64];
    let mut chroma_scale = [0i32; 64];
    let mut last_q = -1i32;
    for mb in 0..h.mb_width as usize {
        let interlaced_mb = if h.mbaff { r.bit() } else { false };
        let qscale = if h.mbaff { r.read(10) } else { r.read(11) } as i32;
        let act = r.bit();
        row.act.get_or_insert(act);
        if qscale != last_q {
            for i in 0..64 {
                luma_scale[i] = qscale * c.luma_weight[i] as i32;
                chroma_scale[i] = qscale * c.chroma_weight[i] as i32;
            }
            last_q = qscale;
        }
        for (n, block) in blocks.iter_mut().enumerate() {
            *block = [0; 64];
            let component = if h.is_444 { (n >> 1) % 3 } else if n & 2 != 0 { 1 + (n & 1) } else { 0 };
            let (scale, weight) = if component == 0 { (&luma_scale, c.luma_weight) } else { (&chroma_scale, c.chroma_weight) };
            // DC: a size code, then that many bits (JPEG-style sign).
            let len = t.dc.read(&mut r).ok_or_else(|| Error::Invalid("DNxHD: bad DC code".into()))? as u32;
            if len > 0 {
                let bits = r.read(len) as i32;
                let level = if bits >> (len - 1) != 0 { bits } else { bits - ((1 << len) - 1) };
                last_dc[component] += level << rule.dc_shift;
            }
            block[0] = last_dc[component];
            // AC: level/run codes until the end-of-block code.
            let mut i = 0usize;
            loop {
                let idx = t.ac.read(&mut r).ok_or_else(|| Error::Invalid("DNxHD: bad AC code".into()))?;
                if idx == c.eob_index {
                    break;
                }
                let mut level = c.ac_info[2 * idx as usize] as i32;
                let flags = c.ac_info[2 * idx as usize + 1];
                let neg = r.bit();
                if flags & 1 != 0 {
                    level += (r.read(rule.index_bits) as i32) << 7;
                }
                if flags & 2 != 0 {
                    i += t.run.read(&mut r).ok_or_else(|| Error::Invalid("DNxHD: bad run code".into()))? as usize;
                }
                i += 1;
                if i > 63 {
                    return invalid("DNxHD: coefficients run past the block");
                }
                level *= scale[i];
                level += scale[i] >> 1;
                if rule.bias < 32 || weight[i] as i32 != rule.bias {
                    level += rule.bias;
                }
                level >>= rule.shift;
                block[ZIGZAG[i] as usize] = if neg { -level } else { level };
            }
            if r.overrun {
                return invalid("DNxHD: row data cut short");
            }
        }
        // Place the blocks. Luma 16×16 from four blocks; an interlaced
        // macroblock (MBAFF) takes its two field halves on alternate lines.
        let put = |dst: &mut [u16], stride: usize, x: usize, top: usize, b: &[i32; 64]| {
            let mut o = [[0f32; 8]; 8];
            idct_fast(b, &q, &mut o);
            for (yy, src) in o.iter().enumerate() {
                let line = if interlaced_mb { top + yy * 2 } else { top + yy };
                let d = &mut dst[line * stride + x..line * stride + x + 8];
                for xx in 0..8 {
                    // SAFETY: clamped to a finite range inside u16 just before.
                    d[xx] = unsafe { (src[xx] + 0.5).clamp(0.5, max + 0.5).to_int_unchecked::<i32>() } as u16;
                }
            }
        };
        let x = mb * 16;
        let (t0, t1) = if interlaced_mb { (0, 1) } else { (0, 8) };
        if !h.is_444 {
            put(&mut row.y, w, x, t0, &blocks[0]);
            put(&mut row.y, w, x + 8, t0, &blocks[1]);
            put(&mut row.y, w, x, t1, &blocks[4]);
            put(&mut row.y, w, x + 8, t1, &blocks[5]);
            let cx = mb * 8;
            put(&mut row.u, cw, cx, t0, &blocks[2]);
            put(&mut row.v, cw, cx, t0, &blocks[3]);
            put(&mut row.u, cw, cx, t1, &blocks[6]);
            put(&mut row.v, cw, cx, t1, &blocks[7]);
        } else {
            put(&mut row.y, w, x, t0, &blocks[0]);
            put(&mut row.y, w, x + 8, t0, &blocks[1]);
            put(&mut row.y, w, x, t1, &blocks[6]);
            put(&mut row.y, w, x + 8, t1, &blocks[7]);
            put(&mut row.u, cw, x, t0, &blocks[2]);
            put(&mut row.u, cw, x + 8, t0, &blocks[3]);
            put(&mut row.u, cw, x, t1, &blocks[8]);
            put(&mut row.u, cw, x + 8, t1, &blocks[9]);
            put(&mut row.v, cw, x, t0, &blocks[4]);
            put(&mut row.v, cw, x + 8, t0, &blocks[5]);
            put(&mut row.v, cw, x, t1, &blocks[10]);
            put(&mut row.v, cw, x + 8, t1, &blocks[11]);
        }
    }
    Ok(row)
}

/// Decode one coding unit (a frame or a field) into `planes`.
fn decode_unit(h: &Header, buf: &[u8], planes: &mut [Vec<u16>; 3], strides: [usize; 3], rows_out: usize, act: &mut Option<bool>) -> Result<()> {
    let t = tables(h.cid);
    let data = &buf[h.data_offset..];
    let n = h.mb_height as usize;
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(1, 16);
    let chunk = n.div_ceil(threads).max(1);
    let idx: Vec<usize> = (0..n).collect();
    let results: Vec<Result<Vec<(usize, Row)>>> = std::thread::scope(|s| {
        let handles: Vec<_> = idx
            .chunks(chunk)
            .map(|part| s.spawn(move || part.iter().map(|&i| decode_row(h, t, &data[h.row_offsets[i] as usize..]).map(|r| (i, r))).collect::<Result<Vec<_>>>()))
            .collect();
        handles.into_iter().map(|j| j.join().unwrap_or_else(|_| invalid("DNxHD: row decoder panicked"))).collect()
    });
    let (start, step) = if h.interlaced { (h.field as usize, 2) } else { (0, 1) };
    for part in results {
        for (i, row) in part? {
            if act.is_none() {
                *act = row.act;
            }
            for (p, src) in [&row.y, &row.u, &row.v].into_iter().enumerate() {
                let sw = src.len() / 16;
                for l in 0..16 {
                    let line = start + (i * 16 + l) * step;
                    if line >= rows_out {
                        break;
                    }
                    let n = sw.min(strides[p]);
                    planes[p][line * strides[p]..line * strides[p] + n].copy_from_slice(&src[l * sw..l * sw + n]);
                }
            }
        }
    }
    Ok(())
}

pub fn decode_frame(data: &[u8]) -> Result<Picture> {
    let first = header(data)?;
    let (w, ht) = (first.width, first.height);
    let mbw = first.mb_width as usize * 16;
    let cw = if first.is_444 { mbw } else { mbw / 2 };
    let rows = (ht as usize).div_ceil(16) * 16 + 16;
    let strides = [mbw, cw, cw];
    let mut planes = [vec![0u16; mbw * rows], vec![0u16; cw * rows], vec![0u16; cw * rows]];
    let mut act = None;
    decode_unit(&first, data, &mut planes, strides, rows, &mut act)?;
    if first.interlaced {
        // The second field follows the first coding unit.
        let cu = CIDS[first.cid].coding_unit_size as usize;
        let cu = if cu == 0 { data.len() / 2 } else { cu };
        if data.len() > cu {
            let mut second = header(&data[cu..])?;
            if second.cid == first.cid {
                second.field = 1 - first.field;
                second.interlaced = true;
                decode_unit(&second, &data[cu..], &mut planes, strides, rows, &mut act)?;
            }
        }
    }
    let format = match (first.is_444, first.bit_depth) {
        (false, 8) => Format::Yuv422P8,
        (false, 10) => Format::Yuv422P10,
        (false, _) => Format::Yuv422P12,
        (true, 10) if first.act && act == Some(true) => Format::Yuv444P10,
        (true, _) if first.act && act == Some(true) => Format::Yuv444P12,
        (true, 10) => Format::Gbrp10,
        (true, _) => Format::Gbrp12,
    };
    let mut pic = Picture::new(w, ht, format);
    for (p, src) in planes.iter().enumerate() {
        let (pw, ph) = format.plane_size(p, w, ht);
        let dst = &mut pic.planes[p];
        for y in 0..ph as usize {
            let s = &src[y * strides[p]..y * strides[p] + pw as usize];
            if format.bytes_per_sample() == 1 {
                for (x, v) in s.iter().enumerate() {
                    dst.data[y * dst.stride + x] = *v as u8;
                }
            } else {
                for (x, v) in s.iter().enumerate() {
                    dst.data[y * dst.stride + x * 2..y * dst.stride + x * 2 + 2].copy_from_slice(&v.to_le_bytes());
                }
            }
        }
    }
    pic.matrix = first.matrix;
    pic.interlaced = first.interlaced;
    Ok(pic)
}

pub struct DnxhdDecoder {
    out: Output,
}

impl DnxhdDecoder {
    pub fn new(_info: &StreamInfo) -> Self {
        DnxhdDecoder { out: Output::default() }
    }
}

impl Decoder for DnxhdDecoder {
    fn send(&mut self, packet: &[u8], pts: i64) -> Result<()> {
        let mut p = decode_frame(packet)?;
        p.pts = pts;
        self.out.push(p);
        Ok(())
    }
    fn receive(&mut self) -> Option<Picture> {
        self.out.pop()
    }
    fn flush(&mut self) {
        self.out.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_cid_builds_its_tables() {
        for i in 0..CIDS.len() {
            let _ = tables(i);
        }
    }

    #[test]
    fn rejects_other_data() {
        assert!(decode_frame(&[0u8; 0x300]).is_err());
    }
}
