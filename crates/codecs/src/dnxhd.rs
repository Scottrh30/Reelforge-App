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
use crate::idct::{fast_qmat, idct_put_u16};
use crate::vlc::Vlc;
use crate::{invalid, Decoder, Error, Format, Matrix, Output, Picture, Result, StreamInfo};
use std::sync::OnceLock;

const ZIGZAG: [u8; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20, 13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22,
    15, 23, 30, 37, 44, 51, 58, 59, 52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

/// The AC symbol for "end of block"; the others are `level << 2 | flags`
/// (flag 1: more level bits follow; flag 2: a run follows).
const AC_EOB: u16 = 0xFFFF;

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
        // AC symbols carry their level and flags directly (no second lookup).
        let ac_syms: Vec<u16> = (0..c.ac_codes.len())
            .map(|i| if i == c.eob_index as usize { AC_EOB } else { (c.ac_info[2 * i] as u16) << 2 | (c.ac_info[2 * i + 1] & 3) as u16 })
            .collect();
        Tables {
            dc: Vlc::new(&dc_codes, c.dc_bits, None),
            ac: Vlc::new(&wide(c.ac_codes), c.ac_bits, Some(&ac_syms)),
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

/// Where one macroblock row's samples go: its lines of each plane, `stride`
/// samples apart (every other picture line for a field).
struct RowOut<'p> {
    y: &'p mut [u16],
    u: &'p mut [u16],
    v: &'p mut [u16],
    stride: usize,
    cstride: usize,
}

/// Decode one macroblock row into `row`; gives the macroblocks' colour flag
/// (4:4:4: YCbCr when set), from the first one.
fn decode_row(h: &Header, t: &Tables, data: &[u8], row: &mut RowOut) -> Result<Option<bool>> {
    let c = &CIDS[h.cid];
    let mut r = BitReader::new(data);
    let (w, cw) = (row.stride, row.cstride);
    let mut act_flag = None;
    let mut last_dc = [1i32 << (h.bit_depth + 2); 3];
    let rule = h.rule;
    let max = ((1u32 << h.bit_depth) - 1) as f32;
    let q = fast_qmat(&[1; 64], 1.0);
    let nblocks = if h.is_444 { 12 } else { 8 };
    let mut blocks = vec![[0i32; 64]; nblocks];
    let mut luma_scale = [0i32; 64];
    let mut chroma_scale = [0i32; 64];
    // What gets added before the shift: half a step, plus the bias where it applies.
    let mut luma_add = [0i32; 64];
    let mut chroma_add = [0i32; 64];
    let bias_at = |w: u8| if rule.bias < 32 || w as i32 != rule.bias { rule.bias } else { 0 };
    let mut last_q = -1i32;
    for mb in 0..h.mb_width as usize {
        let interlaced_mb = if h.mbaff { r.bit() } else { false };
        let qscale = if h.mbaff { r.read(10) } else { r.read(11) } as i32;
        let act = r.bit();
        act_flag.get_or_insert(act);
        if qscale != last_q {
            for i in 0..64 {
                luma_scale[i] = qscale * c.luma_weight[i] as i32;
                chroma_scale[i] = qscale * c.chroma_weight[i] as i32;
                luma_add[i] = (luma_scale[i] >> 1) + bias_at(c.luma_weight[i]);
                chroma_add[i] = (chroma_scale[i] >> 1) + bias_at(c.chroma_weight[i]);
            }
            last_q = qscale;
        }
        for (n, block) in blocks.iter_mut().enumerate() {
            *block = [0; 64];
            let component = if h.is_444 { (n >> 1) % 3 } else if n & 2 != 0 { 1 + (n & 1) } else { 0 };
            let (scale, add) = if component == 0 { (&luma_scale, &luma_add) } else { (&chroma_scale, &chroma_add) };
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
                let Some(sym) = t.ac.read(&mut r) else { return invalid("DNxHD: bad AC code") };
                if sym == AC_EOB {
                    break;
                }
                let mut level = (sym >> 2) as i32;
                let neg = r.bit();
                if sym & 1 != 0 {
                    level += (r.read(rule.index_bits) as i32) << 7;
                }
                if sym & 2 != 0 {
                    let Some(run) = t.run.read(&mut r) else { return invalid("DNxHD: bad run code") };
                    i += run as usize;
                }
                i += 1;
                if i > 63 {
                    return invalid("DNxHD: coefficients run past the block");
                }
                let v = (level * scale[i] + add[i]) >> rule.shift;
                block[ZIGZAG[i] as usize] = if neg { -v } else { v };
            }
            if r.overrun {
                return invalid("DNxHD: row data cut short");
            }
        }
        // Place the blocks. Luma 16×16 from four blocks; an interlaced
        // macroblock (MBAFF) takes its two field halves on alternate lines.
        let put = |dst: &mut [u16], stride: usize, x: usize, top: usize, b: &[i32; 64]| {
            // An interlaced macroblock's blocks go on every other line.
            let step = if interlaced_mb { stride * 2 } else { stride };
            idct_put_u16(b, &q, 0.0, 0.0, max, &mut dst[top * stride + x..], step);
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
    Ok(act_flag)
}

/// Decode one coding unit (a frame or a field) into `planes`.
fn decode_unit(h: &Header, buf: &[u8], planes: &mut [Vec<u16>; 3], strides: [usize; 3], rows_out: usize, act: &mut Option<bool>) -> Result<()> {
    let t = tables(h.cid);
    let data = &buf[h.data_offset..];
    let n = h.mb_height as usize;
    // Each macroblock row's lines: 16, or for a field 16 of every other line.
    let (start, step) = if h.interlaced { (h.field as usize, 2) } else { (0, 1) };
    if (n * 16 * step) > rows_out {
        return invalid("DNxHD: more macroblock rows than the picture has");
    }
    let [yp, up, vp] = planes;
    let (ys, us, vs) = (yp.chunks_mut(16 * step * strides[0]), up.chunks_mut(16 * step * strides[1]), vp.chunks_mut(16 * step * strides[2]));
    let mut jobs: Vec<(usize, RowOut)> = ys
        .zip(us)
        .zip(vs)
        .take(n)
        .enumerate()
        .map(|(i, ((y, u), v))| {
            let out = RowOut { y: &mut y[start * strides[0]..], u: &mut u[start * strides[1]..], v: &mut v[start * strides[2]..], stride: strides[0] * step, cstride: strides[1] * step };
            (i, out)
        })
        .collect();
    // Rows on the shared worker pool; each row's colour flag, in order.
    use rayon::prelude::*;
    let results: Vec<Result<Option<bool>>> = jobs.par_iter_mut().map(|(i, out)| decode_row(h, t, &data[h.row_offsets[*i] as usize..], out)).collect();
    for r in results {
        let a = r?;
        if act.is_none() {
            *act = a;
        }
    }
    Ok(())
}

pub fn decode_frame(data: &[u8]) -> Result<Picture> {
    let first = header(data)?;
    let (w, ht) = (first.width, first.height);
    let mbw = first.mb_width as usize * 16;
    let cw = if first.is_444 { mbw } else { mbw / 2 };
    // Room for every macroblock row (a field's rows take every other line).
    let rows = ((ht as usize).div_ceil(16) * 16).max(first.mb_height as usize * if first.interlaced { 32 } else { 16 }) + 16;
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
        if format.bytes_per_sample() == 1 {
            dst.put_u16_rows_as_u8(src, strides[p], pw as usize, ph as usize);
        } else {
            dst.put_u16_rows(src, strides[p], pw as usize, ph as usize);
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
