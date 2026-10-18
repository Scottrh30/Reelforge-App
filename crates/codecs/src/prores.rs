//! Apple ProRes (422 Proxy/LT/Standard/HQ, 4444 and 4444 XQ, with alpha),
//! following the bitstream described in SMPTE RDD 36.
//!
//! A frame is one picture (or two fields). A picture is cut into slices — runs
//! of 1, 2, 4 or 8 macroblocks along a macroblock row — that decode on their
//! own, so they're spread over threads. In a slice, each plane's coefficients
//! are coded together: the DC values of all its blocks (differentially, with
//! adaptive Rice/Exp-Golomb codes), then the AC values interleaved across the
//! blocks (run and level codes whose code choice adapts to the previous run
//! and level). Dequantised with the frame's matrices times the slice's scale,
//! then an 8×8 inverse DCT. Output is 10-bit 4:2:2 or 4:4:4 (+ alpha).

use crate::bits::BitReader;
use crate::{invalid, Decoder, Error, Format, Matrix, Output, Picture, Result, StreamInfo};

const PROGRESSIVE_SCAN: [u8; 64] = [
    0, 1, 8, 9, 2, 3, 10, 11, 16, 17, 24, 25, 18, 19, 26, 27, 4, 5, 12, 20, 13, 6, 7, 14, 21, 28, 29, 22, 15, 23, 30, 31, 32, 33, 40, 48, 41, 34, 35, 42, 49, 56,
    57, 50, 43, 36, 37, 44, 51, 58, 59, 52, 45, 38, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

const INTERLACED_SCAN: [u8; 64] = [
    0, 8, 1, 9, 16, 24, 17, 25, 2, 10, 3, 11, 18, 26, 19, 27, 32, 40, 33, 34, 41, 48, 56, 49, 42, 35, 43, 50, 57, 58, 51, 59, 4, 12, 5, 6, 13, 20, 28, 21, 14, 7,
    15, 22, 29, 36, 44, 37, 30, 23, 31, 38, 45, 52, 60, 53, 46, 39, 47, 54, 61, 62, 55, 63,
];

/// Code choice for each DC difference, by the size of the previous one.
const DC_CODEBOOK: [u8; 7] = [0x04, 0x28, 0x28, 0x4D, 0x4D, 0x70, 0x70];
const FIRST_DC_CODEBOOK: u8 = 0xB8;
const RUN_CODEBOOK: [u8; 16] = [0x06, 0x06, 0x05, 0x05, 0x04, 0x29, 0x29, 0x29, 0x29, 0x28, 0x28, 0x28, 0x28, 0x28, 0x28, 0x4C];
const LEVEL_CODEBOOK: [u8; 10] = [0x04, 0x0A, 0x05, 0x06, 0x04, 0x28, 0x28, 0x28, 0x28, 0x4C];

/// One adaptive codeword: Rice for small values, Exp-Golomb above `switch_bits`.
/// `codebook` packs the Rice order (bits 5–7), Exp-Golomb order (2–4) and switch point (0–1).
#[inline]
fn codeword(r: &mut BitReader, codebook: u8) -> Result<u32> {
    let switch_bits = (codebook & 3) as u32;
    let rice_order = (codebook >> 5) as u32;
    let exp_order = ((codebook >> 2) & 7) as u32;
    let q = r.peek32().leading_zeros();
    if q > switch_bits {
        let bits = exp_order as i32 - switch_bits as i32 + (q << 1) as i32;
        if !(1..=32).contains(&bits) {
            return invalid("ProRes: codeword too long");
        }
        let bits = bits as u32;
        let v = r.read(bits);
        Ok(v.wrapping_sub(1 << exp_order).wrapping_add((switch_bits + 1) << rice_order))
    } else if rice_order > 0 {
        r.skip(q as usize + 1);
        Ok((q << rice_order) + r.read(rice_order))
    } else {
        r.skip(q as usize + 1);
        Ok(q)
    }
}

struct FrameHeader {
    width: u32,
    height: u32,
    chroma_444: bool,
    /// 0 progressive, 1 top field first, 2 bottom field first.
    frame_type: u8,
    alpha: u8,
    matrix: Matrix,
    qmat_luma: [u8; 64],
    qmat_chroma: [u8; 64],
}

fn frame_header(buf: &[u8]) -> Result<(FrameHeader, usize)> {
    if buf.len() < 20 {
        return invalid("ProRes: frame header cut short");
    }
    let hdr_size = u16::from_be_bytes([buf[0], buf[1]]) as usize;
    let version = u16::from_be_bytes([buf[2], buf[3]]);
    if version > 1 {
        return Err(Error::Unsupported(format!("ProRes bitstream version {version}")));
    }
    if hdr_size < 20 || hdr_size > buf.len() {
        return invalid("ProRes: bad frame header size");
    }
    let width = u16::from_be_bytes([buf[8], buf[9]]) as u32;
    let height = u16::from_be_bytes([buf[10], buf[11]]) as u32;
    let chroma_444 = (buf[12] >> 6) == 3;
    let frame_type = (buf[12] >> 2) & 3;
    let matrix = match buf[16] {
        1 => Matrix::Bt709,
        5 | 6 => Matrix::Bt601,
        9 => Matrix::Bt2020,
        _ => Matrix::Unknown,
    };
    let alpha = buf[17] & 0xF;
    let flags = buf[19];
    let mut p = 20;
    let mut qmat_luma = [4u8; 64];
    if flags & 2 != 0 {
        if p + 64 > hdr_size {
            return invalid("ProRes: luma matrix cut short");
        }
        qmat_luma.copy_from_slice(&buf[p..p + 64]);
        p += 64;
    }
    let mut qmat_chroma = qmat_luma;
    if flags & 1 != 0 {
        if p + 64 > hdr_size {
            return invalid("ProRes: chroma matrix cut short");
        }
        qmat_chroma.copy_from_slice(&buf[p..p + 64]);
    }
    if width == 0 || height == 0 {
        return invalid("ProRes: empty picture");
    }
    Ok((FrameHeader { width, height, chroma_444, frame_type, alpha, matrix, qmat_luma, qmat_chroma }, hdr_size))
}

/// DC values of `n` blocks (into coefficient 0 of each 64-entry block).
fn decode_dc(r: &mut BitReader, out: &mut [i32], n: usize) -> Result<()> {
    let code = codeword(r, FIRST_DC_CODEBOOK)?;
    let mut prev = ((code >> 1) as i32) ^ -((code & 1) as i32);
    out[0] = prev;
    let mut code = 5u32;
    let mut sign = 0i32;
    for i in 1..n {
        code = codeword(r, DC_CODEBOOK[code.min(6) as usize])?;
        if code != 0 {
            sign ^= -((code & 1) as i32);
        } else {
            sign = 0;
        }
        prev += ((((code + 1) >> 1) as i32) ^ sign) - sign;
        out[i * 64] = prev;
    }
    Ok(())
}

/// AC values of `n` blocks (a power of two), interleaved across them.
fn decode_ac(r: &mut BitReader, out: &mut [i32], n: usize, scan: &[u8; 64], total_bits: usize) -> Result<()> {
    let log2 = n.trailing_zeros();
    let max = 64usize << log2;
    let mask = n - 1;
    let mut run = 4u32;
    let mut level = 2u32;
    let mut pos = mask;
    loop {
        let left = total_bits as isize - r.position() as isize;
        if left <= 0 || (left < 32 && r.peek(left as u32) == 0) {
            break;
        }
        run = codeword(r, RUN_CODEBOOK[run.min(15) as usize])?;
        pos += run as usize + 1;
        if pos >= max {
            return invalid("ProRes: AC coefficients run past the blocks");
        }
        level = codeword(r, LEVEL_CODEBOOK[level.min(9) as usize])? + 1;
        let i = pos >> log2;
        let neg = r.bit();
        let v = if neg { -(level as i32) } else { level as i32 };
        out[((pos & mask) << 6) + scan[i] as usize] = v;
    }
    Ok(())
}

/// Dequantise and inverse-transform one block into 10-bit samples.
/// (Coefficients are at 4× sample scale; samples are centred on mid-grey, 512.)
#[inline]
fn put_block(coeffs: &[i32], q: &[f32; 64], dst: &mut [u16], stride: usize) {
    crate::idct::idct_put_u16(coeffs, q, 512.0, 4.0, 1019.0, dst, stride);
}

/// Where a slice sits: macroblock column and row, and how many macroblocks.
#[derive(Clone, Copy)]
struct SliceAt {
    mb_x: u32,
    mb_y: u32,
    mbs: u32,
}

/// A picture's planes as 16-bit samples, padded to whole macroblocks.
struct Planes {
    w: usize,
    h: usize,
    cw: usize,
    y: Vec<u16>,
    u: Vec<u16>,
    v: Vec<u16>,
    a: Vec<u16>,
}

/// Decoded samples of one slice: its own little planes, copied into place afterwards.
struct SliceOut {
    at: SliceAt,
    y: Vec<u16>,
    u: Vec<u16>,
    v: Vec<u16>,
    a: Vec<u16>,
}

struct Ctx<'a> {
    hdr: &'a FrameHeader,
    scan: &'a [u8; 64],
}

fn decode_slice(ctx: &Ctx, data: &[u8], at: SliceAt) -> Result<SliceOut> {
    if data.len() < 6 {
        return invalid("ProRes: slice cut short");
    }
    let hdr_size = (data[0] >> 3) as usize;
    let mut qscale = data[1].clamp(1, 224) as i32;
    if qscale > 128 {
        qscale = (qscale - 96) << 2;
    }
    let y_size = u16::from_be_bytes([data[2], data[3]]) as usize;
    let u_size = u16::from_be_bytes([data[4], data[5]]) as usize;
    let v_size = if hdr_size > 7 { u16::from_be_bytes([data[6], data[7]]) as usize } else { data.len().saturating_sub(y_size + u_size + hdr_size) };
    if hdr_size + y_size + u_size + v_size > data.len() {
        return invalid("ProRes: slice sizes don't add up");
    }
    let a_size = data.len() - hdr_size - y_size - u_size - v_size;
    let mbs = at.mbs as usize;
    let qy = crate::idct::fast_qmat(&std::array::from_fn(|i| ctx.hdr.qmat_luma[i] as i32 * qscale), 0.25);
    let qc = crate::idct::fast_qmat(&std::array::from_fn(|i| ctx.hdr.qmat_chroma[i] as i32 * qscale), 0.25);

    // Luma: four blocks per macroblock (top-left, top-right, bottom-left, bottom-right).
    let lw = mbs * 16;
    let mut y = vec![0u16; lw * 16];
    {
        let d = &data[hdr_size..hdr_size + y_size];
        let n = mbs * 4;
        let mut c = vec![0i32; n * 64];
        let mut r = BitReader::new(d);
        decode_dc(&mut r, &mut c, n)?;
        decode_ac(&mut r, &mut c, n, ctx.scan, d.len() * 8)?;
        for m in 0..mbs {
            for b in 0..4 {
                let (bx, by) = (m * 16 + (b & 1) * 8, (b >> 1) * 8);
                put_block(&c[(m * 4 + b) * 64..], &qy, &mut y[by * lw + bx..], lw);
            }
        }
    }

    // Chroma: a column of two blocks per 8 pixels across (one column for 4:2:2, two for 4:4:4).
    let cols = if ctx.hdr.chroma_444 { 2 } else { 1 };
    let cw = mbs * 8 * cols;
    let mut planes = [Vec::new(), Vec::new()];
    for (k, (off, size)) in [(hdr_size + y_size, u_size), (hdr_size + y_size + u_size, v_size)].into_iter().enumerate() {
        let mut out = vec![0u16; cw * 16];
        let d = &data[off..off + size];
        let n = mbs * 2 * cols;
        let mut c = vec![0i32; n * 64];
        let mut r = BitReader::new(d);
        decode_dc(&mut r, &mut c, n)?;
        decode_ac(&mut r, &mut c, n, ctx.scan, d.len() * 8)?;
        let mut bi = 0;
        for m in 0..mbs {
            for col in 0..cols {
                let x = (m * cols + col) * 8;
                put_block(&c[bi * 64..], &qc, &mut out[x..], cw);
                put_block(&c[(bi + 1) * 64..], &qc, &mut out[8 * cw + x..], cw);
                bi += 2;
            }
        }
        planes[k] = out;
    }
    let [u, v] = planes;

    // Alpha: run-length coded differences, raster order over the slice.
    let mut a = Vec::new();
    if ctx.hdr.alpha != 0 && a_size > 0 {
        let d = &data[data.len() - a_size..];
        a = unpack_alpha(d, lw * 16, if ctx.hdr.alpha == 2 { 16 } else { 8 });
    }
    Ok(SliceOut { at, y, u, v, a })
}

/// Alpha values (as 10-bit) from their run-length coding.
fn unpack_alpha(data: &[u8], count: usize, bits: u32) -> Vec<u16> {
    let mut r = BitReader::new(data);
    let total = data.len() as isize * 8;
    let mask = (1i32 << bits) - 1;
    let to10 = |v: i32| -> u16 { if bits == 16 { (v >> 6) as u16 } else { ((v << 2) | (v >> 6)) as u16 } };
    let mut out = Vec::with_capacity(count);
    let mut val = mask;
    while out.len() < count {
        loop {
            let d = if r.bit() {
                r.read(bits) as i32
            } else {
                let v = r.read(if bits == 16 { 7 } else { 4 }) as i32;
                let s = v & 1;
                let m = (v + 2) >> 1;
                if s != 0 { -m } else { m }
            };
            val = (val + d) & mask;
            out.push(to10(val));
            if out.len() >= count || r.bits_left() <= 0 || !r.bit() {
                break;
            }
        }
        if out.len() >= count || r.position() as isize >= total {
            break;
        }
        let mut n = r.read(4) as usize;
        if n == 0 {
            n = r.read(11) as usize;
        }
        let n = n.min(count - out.len());
        out.extend(std::iter::repeat_n(to10(val), n));
    }
    out.resize(count, to10(val));
    out
}

/// Decode one picture (the whole frame, or one field) into `pl`, starting at
/// row `first_line` and stepping `line_step` rows.
fn decode_picture(hdr: &FrameHeader, buf: &[u8], pl: &mut Planes, field: Option<u32>) -> Result<usize> {
    if buf.len() < 8 {
        return invalid("ProRes: picture header cut short");
    }
    let hdr_size = (buf[0] >> 3) as usize;
    let pic_size = u32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]) as usize;
    let slice_count = u16::from_be_bytes([buf[5], buf[6]]) as usize;
    let log2_slice_w = (buf[7] >> 4) as u32;
    if buf[7] & 0xF != 0 || log2_slice_w > 3 {
        return invalid("ProRes: unexpected slice size");
    }
    if pic_size > buf.len() || hdr_size < 8 || hdr_size + slice_count * 2 > pic_size {
        return invalid("ProRes: bad picture size");
    }
    let mb_w = hdr.width.div_ceil(16);
    let mb_h = if field.is_some() { hdr.height.div_ceil(32) } else { hdr.height.div_ceil(16) };

    // Where each slice goes.
    let mut at = Vec::with_capacity(slice_count);
    let (mut x, mut y) = (0u32, 0u32);
    let full = 1u32 << log2_slice_w;
    let mut count = full;
    for _ in 0..slice_count {
        while mb_w - x < count {
            count >>= 1;
        }
        at.push(SliceAt { mb_x: x, mb_y: y, mbs: count });
        x += count;
        if x == mb_w {
            count = full;
            x = 0;
            y += 1;
        }
    }
    if y < mb_h && x == 0 && slice_count > 0 && at.last().is_some_and(|s| s.mb_y + 1 < mb_h) {
        return invalid("ProRes: not enough slices for the picture");
    }

    // Slice data, after the table of slice sizes.
    let table = &buf[hdr_size..hdr_size + slice_count * 2];
    let mut slices = Vec::with_capacity(slice_count);
    let mut off = hdr_size + slice_count * 2;
    for i in 0..slice_count {
        let n = u16::from_be_bytes([table[i * 2], table[i * 2 + 1]]) as usize;
        if off + n > pic_size {
            return invalid("ProRes: slice runs past the picture");
        }
        slices.push((&buf[off..off + n], at[i]));
        off += n;
    }

    // Decode the slices on several threads.
    let ctx = Ctx { hdr, scan: if hdr.frame_type == 0 { &PROGRESSIVE_SCAN } else { &INTERLACED_SCAN } };
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(1, 16);
    let chunk = slices.len().div_ceil(threads).max(1);
    let results: Vec<Result<Vec<SliceOut>>> = std::thread::scope(|s| {
        let handles: Vec<_> = slices
            .chunks(chunk)
            .map(|part| {
                let ctx = &ctx;
                s.spawn(move || part.iter().map(|(d, a)| decode_slice(ctx, d, *a)).collect::<Result<Vec<_>>>())
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap_or_else(|_| invalid("ProRes: slice decoder panicked"))).collect()
    });

    // Copy each slice into place (every other row, for a field).
    let (start, step) = match field {
        Some(f) => (f as usize, 2usize),
        None => (0, 1),
    };
    let chroma_scale = if hdr.chroma_444 { 1 } else { 2 };
    for part in results {
        for so in part? {
            let lw = so.at.mbs as usize * 16;
            let x0 = so.at.mb_x as usize * 16;
            for r in 0..16 {
                let row = start + (so.at.mb_y as usize * 16 + r) * step;
                if row >= pl.h {
                    break;
                }
                pl.y[row * pl.w + x0..row * pl.w + x0 + lw].copy_from_slice(&so.y[r * lw..(r + 1) * lw]);
                if !so.a.is_empty() {
                    pl.a[row * pl.w + x0..row * pl.w + x0 + lw].copy_from_slice(&so.a[r * lw..(r + 1) * lw]);
                }
                let cw = lw / chroma_scale;
                let cx = x0 / chroma_scale;
                pl.u[row * pl.cw + cx..row * pl.cw + cx + cw].copy_from_slice(&so.u[r * cw..(r + 1) * cw]);
                pl.v[row * pl.cw + cx..row * pl.cw + cx + cw].copy_from_slice(&so.v[r * cw..(r + 1) * cw]);
            }
        }
    }
    Ok(pic_size)
}

/// Decode a whole ProRes frame.
pub fn decode_frame(data: &[u8]) -> Result<Picture> {
    if data.len() < 8 || &data[4..8] != b"icpf" {
        return invalid("ProRes: no 'icpf' frame marker");
    }
    let frame_size = u32::from_be_bytes([data[0], data[1], data[2], data[3]]) as usize;
    let data = &data[8..frame_size.clamp(8, data.len())];
    let (hdr, hsize) = frame_header(data)?;
    let mb_w = hdr.width.div_ceil(16) as usize;
    let rows = if hdr.frame_type == 0 { hdr.height.div_ceil(16) * 16 } else { hdr.height.div_ceil(32) * 32 } as usize;
    let w = mb_w * 16;
    let cw = if hdr.chroma_444 { w } else { w / 2 };
    let alpha = hdr.alpha != 0;
    let mut pl = Planes { w, h: rows, cw, y: vec![0; w * rows], u: vec![512; cw * rows], v: vec![512; cw * rows], a: if alpha { vec![1023; w * rows] } else { Vec::new() } };

    let mut rest = &data[hsize..];
    if hdr.frame_type == 0 {
        decode_picture(&hdr, rest, &mut pl, None)?;
    } else {
        // Two fields; the first is the top one for "top field first".
        let first = if hdr.frame_type == 1 { 0 } else { 1 };
        let n = decode_picture(&hdr, rest, &mut pl, Some(first))?;
        rest = &rest[n..];
        decode_picture(&hdr, rest, &mut pl, Some(1 - first))?;
    }

    // Crop to the picture size.
    let format = match (hdr.chroma_444, alpha) {
        (false, _) => Format::Yuv422P10,
        (true, false) => Format::Yuv444P10,
        (true, true) => Format::Yuva444P10,
    };
    let (pw, ph) = (hdr.width, hdr.height);
    let mut pic = Picture::new(pw, ph, format);
    let copy = |src: &[u16], sw: usize, dst: &mut crate::Plane, cols: usize, rows: usize| {
        for r in 0..rows {
            let d = &mut dst.data[r * dst.stride..r * dst.stride + cols * 2];
            for (c, px) in src[r * sw..r * sw + cols].iter().enumerate() {
                d[c * 2..c * 2 + 2].copy_from_slice(&px.to_le_bytes());
            }
        }
    };
    let (cols_c, _) = format.plane_size(1, pw, ph);
    copy(&pl.y, pl.w, &mut pic.planes[0], pw as usize, ph as usize);
    copy(&pl.u, pl.cw, &mut pic.planes[1], cols_c as usize, ph as usize);
    copy(&pl.v, pl.cw, &mut pic.planes[2], cols_c as usize, ph as usize);
    if alpha {
        copy(&pl.a, pl.w, &mut pic.planes[3], pw as usize, ph as usize);
    }
    pic.matrix = hdr.matrix;
    pic.interlaced = hdr.frame_type != 0;
    Ok(pic)
}

pub struct ProResDecoder {
    out: Output,
}

impl ProResDecoder {
    pub fn new(_info: &StreamInfo) -> Self {
        ProResDecoder { out: Output::default() }
    }
}

impl Decoder for ProResDecoder {
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
    fn scans_are_permutations() {
        for s in [PROGRESSIVE_SCAN, INTERLACED_SCAN] {
            let mut seen = [false; 64];
            for v in s {
                assert!(!seen[v as usize]);
                seen[v as usize] = true;
            }
        }
    }

    #[test]
    fn rice_and_golomb_codewords() {
        // Codebook 0x04: exp order 1, no rice, switch 0. "1" → 0; "01x" → 1 + x.
        let d = [0b1000_0000];
        assert_eq!(codeword(&mut BitReader::new(&d), 0x04).unwrap(), 0);
        let d = [0b0110_0000];
        assert_eq!(codeword(&mut BitReader::new(&d), 0x04).unwrap(), 2);
        // Rice order 2, switch 1 (0x45): "01" then 2 bits: q=1 → 4 + v.
        let d = [0b0111_0000];
        assert_eq!(codeword(&mut BitReader::new(&d), 0x45).unwrap(), 4 + 3);
    }
}
