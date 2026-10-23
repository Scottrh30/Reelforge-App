//! GoPro CineForm (CFHD): Reelforge's own decoder.
//!
//! Written from GoPro's CineForm SDK (open source since 2017 under MIT or
//! Apache-2.0), which defines the format. A CineForm sample is a list of
//! big-endian 16-bit tag / 16-bit value pairs; a negative tag is an optional
//! one. Between the tags sit the coefficient data of each channel's wavelet
//! bands:
//!
//! - the lowpass band, raw 16-bit numbers after a `0x0f0f` marker;
//! - highpass bands, run-length coded with a Huffman codebook (SDK codesets
//!   9 and 17), each ending in a band-end code and padded to 4 bytes.
//!
//! Intra frames use three levels of a 2/6 wavelet per channel (10 bands).
//! A two-frame group adds a Haar transform across the two frames (17 bands):
//! the first frame of the group comes out of its sample, the second out of
//! the small sample that follows it.
//!
//! Handled: YUV 4:2:2 10-bit, RGB 4:4:4 12-bit and RGBA 4:4:4:4 12-bit, intra
//! and two-frame groups, progressive and interlaced, peak tables, difference
//! coding and the lossless second pass. Bayer (raw) samples are left to FFmpeg.

use std::sync::OnceLock;

use crate::bits::BitReader;
use crate::cineform_tables::{TABLE17, TABLE17_END, TABLE9, TABLE9_END};
use crate::vlc::Vlc;
use crate::{invalid, Decoder, Error, Format, Output, Picture, Plane, Result, StreamInfo};

// Tags (SDK codec.h).
const SAMPLE_TYPE: u16 = 1;
const SAMPLE_INDEX_TABLE: u16 = 2;
const BITSTREAM_MARKER: u16 = 4;
const TRANSFORM_TYPE: u16 = 10;
const CHANNEL_COUNT: u16 = 12;
const SUBBAND_COUNT: u16 = 14;
const IMAGE_WIDTH: u16 = 20;
const IMAGE_HEIGHT: u16 = 21;
const LOWPASS_WIDTH: u16 = 27;
const LOWPASS_HEIGHT: u16 = 28;
const HIGHPASS_WIDTH: u16 = 41;
const HIGHPASS_HEIGHT: u16 = 42;
const BAND_WIDTH: u16 = 49;
const BAND_HEIGHT: u16 = 50;
const SUBBAND_BAND: u16 = 51;
const BAND_ENCODING: u16 = 52;
const QUANTIZATION: u16 = 53;
const BAND_HEADER: u16 = 55;
const CHANNEL_NUMBER: u16 = 62;
const SAMPLE_FLAGS: u16 = 68;
const PRECISION: u16 = 70;
const INPUT_FORMAT: u16 = 71;
const BAND_CODING_FLAGS: u16 = 72;
const PEAK_LEVEL: u16 = 74;
const PEAK_OFFSET_LOW: u16 = 75;
const PEAK_OFFSET_HIGH: u16 = 76;
const BAND_SECOND_PASS: u16 = 82;
const PRESCALE_TABLE: u16 = 83;
const ENCODED_FORMAT: u16 = 84;
const DISPLAY_HEIGHT: u16 = 85;
/// The marker value before a channel's lowpass coefficients.
const COEFFICIENT_SEGMENT: u16 = 0x0f0f;
/// Band encodings: raw 16-bit numbers, and lossless run-lengths.
const ENCODING_16BIT: u16 = 4;
const ENCODING_LOSSLESS: u16 = 5;
/// A chunk of peak values (that peak-offset tags point into).
const PEAK_TABLE_CHUNK: u16 = 0x4001;

// Sample types.
const SAMPLE_SECOND_FRAME: u16 = 1;

/// Bands per channel: intra (3 spatial levels) and two-frame groups.
const BANDS_INTRA: usize = 10;
const BANDS_GROUP: usize = 17;
/// A placeholder band in groups that carries no data.
const NO_BAND: usize = 255;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Yuv422,
    Rgb,
    Rgba,
    Bayer,
}

impl Kind {
    fn channels(self) -> usize {
        match self {
            Kind::Yuv422 | Kind::Rgb => 3,
            Kind::Rgba | Kind::Bayer => 4,
        }
    }
    fn format(self) -> Format {
        match self {
            Kind::Yuv422 => Format::Yuv422P10,
            Kind::Rgb => Format::Gbrp12,
            _ => Format::Gbrap12,
        }
    }
}

/// A codebook: the code table and what each symbol means, (run, signed value);
/// run 0 is the band end.
struct Book {
    vlc: Vlc,
    syms: Vec<(u16, i16)>,
}

fn book(table: &[(u8, u32, u16, u8)], end: (u8, u32)) -> Book {
    let (mut codes, mut lens, mut syms) = (Vec::new(), Vec::new(), Vec::new());
    for &(len, code, run, mag) in table {
        if mag == 0 {
            codes.push(code);
            lens.push(len);
            syms.push((run, 0));
        } else {
            for (s, v) in [(0, mag as i16), (1, -(mag as i16))] {
                codes.push(code << 1 | s);
                lens.push(len + 1);
                syms.push((run, v));
            }
        }
    }
    codes.push(end.1);
    lens.push(end.0);
    syms.push((0, 0));
    Book { vlc: Vlc::new(&codes, &lens, None), syms }
}

struct Tables {
    /// Codebook 0 (SDK codeset 9) and codebooks 1–2 (codesets 17 and 18 share a table).
    books: [Book; 2],
    /// Magnitude decompanding: codebook 0's, then codebook 1's (cubic).
    lut0: [i32; 64],
    lut1: [i32; 256],
}

fn tables() -> &'static Tables {
    static T: OnceLock<Tables> = OnceLock::new();
    T.get_or_init(|| {
        let mut lut0 = [0i32; 64];
        for (i, v) in lut0.iter_mut().enumerate() {
            // Magnitudes from 40 up stand for steps of 4, and from 54 up for steps of 16.
            let mut m = i as i32;
            if m >= 40 {
                if m >= 54 {
                    m = (m - 54) * 4 + 54;
                }
                m = (m - 40) * 4 + 40;
            }
            *v = m;
        }
        let mut lut1 = [0i32; 256];
        for (i, v) in lut1.iter_mut().enumerate() {
            let i = i as i64;
            *v = (i + 768 * i * i * i / (256 * 256 * 256)) as i32;
        }
        Tables { books: [book(&TABLE9, TABLE9_END), book(&TABLE17, TABLE17_END)], lut0, lut1 }
    })
}

/// One wavelet band: `h` rows of `w` coefficients, `stride` apart.
#[derive(Clone, Default)]
struct Band {
    w: usize,
    h: usize,
    stride: usize,
    data: Vec<i16>,
    ok: bool,
}

/// A grid of coefficients for the inverse transform (a band or a result).
struct View<'a> {
    w: usize,
    h: usize,
    stride: usize,
    d: &'a [i16],
}

impl Band {
    fn view(&self) -> View<'_> {
        View { w: self.w, h: self.h, stride: self.stride, d: &self.data }
    }
}

/// A transform result, `w`×`h`, rows packed.
struct Grid {
    w: usize,
    h: usize,
    d: Vec<i16>,
}

impl Grid {
    fn view(&self) -> View<'_> {
        View { w: self.w, h: self.h, stride: self.w, d: &self.d }
    }
}

/// The peak table: coefficients above `level` are replaced, in order, by
/// 16-bit values stored elsewhere in the sample.
#[derive(Default)]
struct Peak {
    level: i32,
    offset: u32,
    /// Where `offset` counts from, then the next value to take.
    pos: usize,
}

/// Everything a sample's tags set.
struct Header {
    sample_type: u16,
    width: usize,
    height: usize,
    display_height: usize,
    bpc: u32,
    transform: Option<u16>,
    encoded: Option<u16>,
    input: Option<u16>,
    channels: usize,
    progressive: bool,
    prescale: Option<[u32; 8]>,
}

impl Header {
    fn kind(&self) -> Result<Kind> {
        let k = match self.encoded {
            Some(1) => Kind::Yuv422,
            Some(2) => Kind::Bayer,
            Some(3) => Kind::Rgb,
            Some(4) => Kind::Rgba,
            Some(v) => return Err(Error::Unsupported(format!("CineForm encoded format {v}"))),
            None => match self.input {
                Some(100..=105) => Kind::Bayer,
                Some(120..=128) => Kind::Rgb,
                Some(30) => Kind::Rgba,
                _ if self.channels == 4 => Kind::Rgba,
                _ => Kind::Yuv422,
            },
        };
        if k == Kind::Bayer {
            return Err(Error::Unsupported("CineForm RAW (Bayer)".into()));
        }
        Ok(k)
    }
}

pub struct CineformDecoder {
    out: Output,
    /// Bands of each channel, reused between samples.
    planes: Vec<Vec<Band>>,
    /// The second frame of the last group, waiting for its own sample.
    held: Option<Picture>,
    /// Scratch buffers for each channel's inverse transform.
    pools: Vec<Pool>,
}

impl CineformDecoder {
    pub fn new(_info: &StreamInfo) -> Self {
        CineformDecoder { out: Output::default(), planes: Vec::new(), held: None, pools: Vec::new() }
    }

    /// Decode one sample: the pictures it completes, in order.
    fn decode(&mut self, pkt: &[u8]) -> Result<Vec<Picture>> {
        let t = tables();
        let mut hd = Header {
            sample_type: 0,
            width: 0,
            height: 0,
            display_height: 0,
            bpc: 10,
            transform: None,
            encoded: None,
            input: None,
            channels: 3,
            progressive: false,
            prescale: None,
        };
        let mut peak = Peak::default();
        let mut started = false;
        let (mut channel, mut actual) = (0usize, 0usize);
        let (mut lw, mut lh, mut bw, mut bh) = (0usize, 0usize, 0usize, 0usize);
        let (mut quant, mut codebook, mut difference, mut encoding) = (1i32, 0u16, false, 0u16);
        let mut nbands = BANDS_INTRA;
        let mut pos = 0usize;
        while pos + 4 <= pkt.len() {
            let tag = i16::from_be_bytes([pkt[pos], pkt[pos + 1]]);
            let val = u16::from_be_bytes([pkt[pos + 2], pkt[pos + 3]]);
            pos += 4;
            let a = tag.unsigned_abs();
            match a {
                // Chunks: skip their contents (a size in 4-byte words).
                0x4000..=0x4fff => {
                    if a == PEAK_TABLE_CHUNK {
                        peak.level = 0;
                    }
                    pos += val as usize * 4;
                    continue;
                }
                0x6000..=0x6fff => {
                    pos += (val as usize + ((a as usize & 0xff) << 16)) * 4;
                    continue;
                }
                // Size markers around normal tags: nothing to do.
                0x2000..=0x3fff => continue,
                _ => {}
            }
            match a {
                SAMPLE_TYPE => hd.sample_type = val,
                SAMPLE_INDEX_TABLE => pos += val as usize * 4,
                TRANSFORM_TYPE => {
                    if val == 1 || val > 2 {
                        return Err(Error::Unsupported(format!("CineForm transform type {val}")));
                    }
                    hd.transform.get_or_insert(val);
                }
                CHANNEL_COUNT => {
                    if !(1..=4).contains(&val) {
                        return invalid(format!("CineForm: {val} channels"));
                    }
                    hd.channels = val as usize;
                }
                SUBBAND_COUNT => {
                    if val as usize != BANDS_INTRA && val as usize != BANDS_GROUP {
                        return Err(Error::Unsupported(format!("CineForm with {val} bands")));
                    }
                }
                IMAGE_WIDTH => hd.width = val as usize,
                IMAGE_HEIGHT => hd.height = val as usize,
                DISPLAY_HEIGHT => hd.display_height = val as usize,
                SAMPLE_FLAGS => hd.progressive = val & 1 != 0,
                PRECISION => {
                    if val != 10 && val != 12 {
                        return invalid(format!("CineForm: {val} bits per sample"));
                    }
                    hd.bpc = val as u32;
                }
                INPUT_FORMAT => hd.input = Some(val),
                ENCODED_FORMAT => hd.encoded = Some(val),
                PRESCALE_TABLE => {
                    let mut p = [0u32; 8];
                    for (i, v) in p.iter_mut().enumerate() {
                        *v = (val as u32 >> (14 - i * 2)) & 3;
                    }
                    hd.prescale = Some(p);
                }
                CHANNEL_NUMBER => channel = val as usize,
                LOWPASS_WIDTH => lw = val as usize,
                LOWPASS_HEIGHT => lh = val as usize,
                HIGHPASS_WIDTH | BAND_WIDTH => bw = val as usize,
                HIGHPASS_HEIGHT | BAND_HEIGHT => bh = val as usize,
                SUBBAND_BAND => actual = val as usize,
                QUANTIZATION => quant = val as i32,
                BAND_ENCODING => encoding = val,
                BAND_CODING_FLAGS => {
                    codebook = val & 0xf;
                    difference = val & 0x10 != 0;
                }
                PEAK_OFFSET_LOW if tag < 0 => {
                    peak.offset = (peak.offset & !0xffff) | val as u32;
                    peak.pos = pos;
                    peak.level = 0;
                }
                PEAK_OFFSET_HIGH if tag < 0 => {
                    peak.offset = (peak.offset & 0xffff) | (val as u32) << 16;
                    peak.pos = pos;
                    peak.level = 0;
                }
                PEAK_LEVEL if tag < 0 && peak.offset != 0 => {
                    peak.level = val as i32;
                    let to = peak.pos as i64 + peak.offset as i64 - 4;
                    if to < 0 || to as usize > pkt.len() {
                        return invalid("CineForm: peak table outside the sample");
                    }
                    peak.pos = to as usize;
                }
                BITSTREAM_MARKER if val == COEFFICIENT_SEGMENT => {
                    // A channel's lowpass band.
                    if !started {
                        let kind = hd.kind()?;
                        nbands = if hd.transform == Some(2) { BANDS_GROUP } else { BANDS_INTRA };
                        self.planes.resize_with(kind.channels(), Vec::new);
                        for p in &mut self.planes {
                            p.resize_with(nbands, Band::default);
                            for b in p.iter_mut() {
                                b.ok = false;
                            }
                        }
                        started = true;
                    }
                    if channel >= self.planes.len() {
                        return invalid(format!("CineForm: channel {channel}"));
                    }
                    if lw < 3 || lh < 3 || pos + lw * lh * 2 > pkt.len() {
                        return invalid("CineForm: bad lowpass band");
                    }
                    let b = &mut self.planes[channel][0];
                    b.data.clear();
                    b.data.extend(pkt[pos..pos + lw * lh * 2].chunks_exact(2).map(|c| i16::from_be_bytes([c[0], c[1]])));
                    (b.w, b.h, b.stride, b.ok) = (lw, lh, lw, true);
                    pos = (pos + lw * lh * 2 + 3) & !3;
                }
                BAND_HEADER | BAND_SECOND_PASS => {
                    if actual == NO_BAND {
                        codebook = 0;
                        continue;
                    }
                    if !started || channel >= self.planes.len() || actual == 0 || actual >= nbands {
                        return invalid(format!("CineForm: band {actual} of channel {channel} out of place"));
                    }
                    if bw < 3 || bh < 3 {
                        return invalid("CineForm: bad band size");
                    }
                    if codebook == 0 && nbands == BANDS_GROUP && actual == 7 {
                        codebook = 1;
                    }
                    let stride = (bw + 7) & !7;
                    let b = &mut self.planes[channel][actual];
                    let second = a == BAND_SECOND_PASS;
                    if second {
                        if !b.ok || b.w != bw || b.h != bh {
                            return invalid("CineForm: second pass without a first");
                        }
                    } else {
                        // Runs fill it from the start; decode_band zeroes the rest.
                        b.data.resize(stride * bh, 0);
                        b.data.truncate(stride * bh);
                        (b.w, b.h, b.stride) = (bw, bh, stride);
                    }
                    let used = if encoding == ENCODING_16BIT && !second {
                        // Raw 16-bit numbers (the temporal difference's lowpass band in groups).
                        let n = bw * bh * 2;
                        let Some(raw) = pkt.get(pos..pos + n) else {
                            return invalid("CineForm: band runs past the sample");
                        };
                        for (row, src) in b.data.chunks_exact_mut(stride).zip(raw.chunks_exact(bw * 2)) {
                            for (d, c) in row.iter_mut().zip(src.chunks_exact(2)) {
                                *d = (i16::from_be_bytes([c[0], c[1]]) as i32).wrapping_mul(quant) as i16;
                            }
                        }
                        (n + 3) & !3
                    } else {
                        decode_band(t, &pkt[pos..], b, codebook, quant, encoding == ENCODING_LOSSLESS, second, &mut peak, pkt)?
                    };
                    if difference {
                        for row in b.data.chunks_exact_mut(stride) {
                            for x in 1..bw {
                                row[x] = row[x].wrapping_add(row[x - 1]);
                            }
                        }
                    }
                    b.ok = true;
                    pos += used;
                    codebook = 0;
                }
                _ => {}
            }
        }

        if hd.sample_type == SAMPLE_SECOND_FRAME {
            return match self.held.take() {
                Some(p) => Ok(vec![p]),
                None if started => invalid("CineForm: a second frame with picture data"),
                None => invalid("CineForm: the second frame of a group without its first"),
            };
        }
        if !started {
            // A sequence header or trailer: no picture.
            return Ok(Vec::new());
        }
        let kind = hd.kind()?;
        let group = nbands == BANDS_GROUP;
        if hd.width == 0 || hd.height == 0 {
            // Older samples leave the size out: it's the lowpass band's times 8.
            let lo = &self.planes[0][0];
            hd.width = lo.w * 8;
            hd.height = lo.h * 8;
        }
        for (c, p) in self.planes.iter().enumerate() {
            for (i, b) in p.iter().enumerate() {
                if !b.ok {
                    return invalid(format!("CineForm: band {i} of channel {c} is missing"));
                }
            }
        }
        // Wavelet prescale shifts, by wavelet: intra 0 (finest)–2, groups 0–5.
        let prescale = hd.prescale.unwrap_or(if group {
            [0, 0, 0, 0, 2, if hd.bpc == 12 { 2 } else { 0 }, 0, 0]
        } else {
            [0, 2, if hd.bpc == 12 { 2 } else { 0 }, 0, 0, 0, 0, 0]
        });
        let fmt = kind.format();
        let (w, h) = (hd.width as u32, hd.height as u32);
        let mut pics = vec![Picture::new(w, h, fmt)];
        if group {
            pics.push(Picture::new(w, h, fmt));
        }
        let bpc = hd.bpc;
        let progressive = hd.progressive;
        // Each channel's planes, filled in parallel (straight into the
        // pictures' planes when they're aligned for 16-bit access).
        let mut planes: Vec<Vec<Plane>> = (0..fmt.planes()).map(|_| Vec::new()).collect();
        for pic in &mut pics {
            for (i, pl) in std::mem::take(&mut pic.planes).into_iter().enumerate() {
                planes[i].push(pl);
            }
        }
        self.pools.resize_with(self.planes.len(), Pool::default);
        let results: Vec<Result<()>> = {
            use rayon::prelude::*;
            let mut jobs: Vec<(usize, &Vec<Band>, &mut Pool, &mut Vec<Plane>)> = Vec::new();
            let mut by_plane: Vec<Option<&mut Vec<Plane>>> = planes.iter_mut().map(Some).collect();
            for (c, (bands, pool)) in self.planes.iter().zip(self.pools.iter_mut()).enumerate() {
                let plane = if c == 1 { 2 } else if c == 2 { 1 } else { c };
                if let Some(pls) = by_plane.get_mut(plane).and_then(|p| p.take()) {
                    jobs.push((plane, bands, pool, pls));
                }
            }
            jobs.into_par_iter()
                .map(|(plane, bands, pool, pls)| {
                    let (pw, ph) = fmt.plane_size(plane, w, h);
                    let (pw, ph) = (pw as usize, ph as usize);
                    let alpha = kind == Kind::Rgba && plane == 3;
                    let mut temps: Vec<Vec<u16>> = Vec::new();
                    let aligned = cfg!(target_endian = "little") && pls.iter_mut().all(|p| unsafe { p.data.align_to_mut::<u16>().0.is_empty() });
                    let r = if aligned {
                        let mut outs: Vec<&mut [u16]> = pls
                            .iter_mut()
                            // SAFETY: u16 has no invalid bit patterns, the prefix is empty
                            // (checked above), and on little-endian machines the bytes of
                            // a u16 are the little-endian samples the planes hold.
                            .map(|p| unsafe { p.data.align_to_mut::<u16>().1 })
                            .collect();
                        reconstruct(pool, bands, &mut outs, &prescale, pw, ph, bpc, progressive, alpha)
                    } else {
                        temps.resize_with(pls.len(), || vec![0u16; pw * ph]);
                        let mut outs: Vec<&mut [u16]> = temps.iter_mut().map(|t| t.as_mut_slice()).collect();
                        reconstruct(pool, bands, &mut outs, &prescale, pw, ph, bpc, progressive, alpha)
                    };
                    for (p, t) in pls.iter_mut().zip(&temps) {
                        p.put_u16_rows(t, pw, pw, ph);
                    }
                    r
                })
                .collect()
        };
        for (k, pic) in pics.iter_mut().enumerate() {
            pic.planes = planes.iter_mut().map(|p| std::mem::take(&mut p[k])).collect();
        }
        results.into_iter().collect::<Result<()>>()?;
        let shown = if hd.display_height > 0 && hd.display_height < hd.height { hd.display_height as u32 } else { h };
        for p in &mut pics {
            p.interlaced = !progressive;
            if shown < h {
                crop_height(p, shown);
            }
        }
        if group {
            self.held = pics.pop();
        }
        Ok(pics)
    }
}

/// Keep the top `h` rows.
fn crop_height(p: &mut Picture, h: u32) {
    p.height = h;
    for (i, pl) in p.planes.iter_mut().enumerate() {
        let (_, ph) = p.format.plane_size(i, p.width, h);
        pl.data.truncate(pl.stride * ph as usize);
    }
}

/// Decode a highpass band's runs into `b` (already sized); the bytes used,
/// padded to 4.
#[allow(clippy::too_many_arguments)]
fn decode_band(t: &Tables, data: &[u8], b: &mut Band, codebook: u16, quant: i32, lossless: bool, second: bool, peak: &mut Peak, pkt: &[u8]) -> Result<usize> {
    let book = &t.books[(codebook != 0) as usize];
    let expected = b.data.len();
    let mut r = BitReader::new(data);
    let mut count = 0usize;
    loop {
        let Some(sym) = book.vlc.read(&mut r) else {
            return invalid("CineForm: bad code in a band");
        };
        let (run, level) = book.syms[sym as usize];
        if run == 0 {
            break;
        }
        let run = run as usize;
        if count + run > expected || r.overrun {
            return invalid("CineForm: band runs past its end");
        }
        let coeff = if lossless {
            level as i32
        } else {
            let m = level.unsigned_abs() as usize;
            let mag = match codebook {
                0 => t.lut0[m.min(63)],
                1 => t.lut1[m.min(255)],
                _ => m as i32,
            };
            mag.wrapping_mul(quant) * if level < 0 { -1 } else { 1 }
        };
        let out = &mut b.data[count..count + run];
        if second {
            for v in out {
                *v = ((*v as i32 | coeff.wrapping_mul(256)).wrapping_mul(quant)) as i16;
            }
        } else {
            out.fill(coeff as i16);
        }
        count += run;
    }
    if !second {
        b.data[count..].fill(0);
    }
    if peak.level > 0 {
        for v in &mut b.data[..count] {
            if (*v as i32).abs() > peak.level {
                let Some(p) = pkt.get(peak.pos..peak.pos + 2) else {
                    return invalid("CineForm: peak table ran out");
                };
                *v = i16::from_le_bytes([p[0], p[1]]);
                peak.pos += 2;
            }
        }
    }
    let bytes = r.position().div_ceil(8);
    let bytes = (bytes + 3) & !3;
    if bytes > data.len() {
        return invalid("CineForm: band runs past the sample");
    }
    Ok(bytes)
}

/// Spare coefficient buffers, reused from frame to frame.
#[derive(Default)]
struct Pool(Vec<Vec<i16>>);

impl Pool {
    /// A buffer of `n` values (contents left over: every user writes all of it).
    fn get(&mut self, n: usize) -> Vec<i16> {
        let mut v = self.0.pop().unwrap_or_default();
        if v.len() < n {
            v.resize(n, 0);
        } else {
            v.truncate(n);
        }
        v
    }
    fn put(&mut self, g: Grid) {
        self.0.push(g.d);
    }
}

#[inline(always)]
fn tmp(v: i32) -> i32 {
    (v >> 3) as i16 as i32
}

/// The inverse 2/6 wavelet along one line: `n` lowpass and highpass values
/// in, `2n` values out. The arithmetic (16-bit intermediates) follows the
/// SDK's decoder exactly.
#[inline(always)]
fn filter_row(l: &[i16], h: &[i16], out: &mut [i16]) {
    let n = l.len();
    let (h, out) = (&h[..n], &mut out[..2 * n]);
    let (a, b, c, s) = (l[0] as i32, l[1] as i32, l[2] as i32, h[0] as i32);
    out[0] = ((tmp(11 * a - 4 * b + c + 4) + s) >> 1) as i16;
    out[1] = ((tmp(5 * a + 4 * b - c + 4) - s) >> 1) as i16;
    for ((o, w), s) in out[2..2 * n - 2].chunks_exact_mut(2).zip(l.windows(3)).zip(&h[1..n - 1]) {
        let (a, b, c, s) = (w[0] as i32, w[1] as i32, w[2] as i32, *s as i32);
        o[0] = ((tmp(a - c + 4) + b + s) >> 1) as i16;
        o[1] = ((tmp(c - a + 4) + b - s) >> 1) as i16;
    }
    let (a, b, c, s) = (l[n - 3] as i32, l[n - 2] as i32, l[n - 1] as i32, h[n - 1] as i32);
    out[2 * n - 2] = ((tmp(5 * c + 4 * b - a + 4) + s) >> 1) as i16;
    out[2 * n - 1] = ((tmp(11 * c - 4 * b + a + 4) - s) >> 1) as i16;
}

fn check(v: &View, w: usize, h: usize) -> Result<()> {
    if v.w < w || v.h < h || w < 3 || h < 3 || (h - 1) * v.stride + w > v.d.len() {
        return invalid("CineForm: band sizes don't fit together");
    }
    Ok(())
}

/// Inverse along columns: `w`×`h` lowpass and highpass in, `w`×`2h` out.
fn vertical(pool: &mut Pool, lo: &View, hi: &View, w: usize, h: usize) -> Result<Grid> {
    check(lo, w, h)?;
    check(hi, w, h)?;
    let mut d = pool.get(w * h * 2);
    let l = |r: usize| &lo.d[r * lo.stride..r * lo.stride + w];
    // Row by row (the same sums for every column), so it vectorises.
    for (i, pair) in d.chunks_exact_mut(2 * w).enumerate() {
        let (even, odd) = pair.split_at_mut(w);
        let hh = &hi.d[i * hi.stride..i * hi.stride + w];
        if i == 0 {
            let (l0, l1, l2) = (l(0), l(1), l(2));
            for x in 0..w {
                let (p, q, r, s) = (l0[x] as i32, l1[x] as i32, l2[x] as i32, hh[x] as i32);
                even[x] = ((tmp(11 * p - 4 * q + r + 4) + s) >> 1) as i16;
                odd[x] = ((tmp(5 * p + 4 * q - r + 4) - s) >> 1) as i16;
            }
        } else if i == h - 1 {
            let (l0, l1, l2) = (l(i - 2), l(i - 1), l(i));
            for x in 0..w {
                let (p, q, r, s) = (l0[x] as i32, l1[x] as i32, l2[x] as i32, hh[x] as i32);
                even[x] = ((tmp(5 * r + 4 * q - p + 4) + s) >> 1) as i16;
                odd[x] = ((tmp(11 * r - 4 * q + p + 4) - s) >> 1) as i16;
            }
        } else {
            let (l0, l1, l2) = (l(i - 1), l(i), l(i + 1));
            for x in 0..w {
                let (p, q, r, s) = (l0[x] as i32, l1[x] as i32, l2[x] as i32, hh[x] as i32);
                even[x] = ((tmp(p - r + 4) + q + s) >> 1) as i16;
                odd[x] = ((tmp(r - p + 4) + q - s) >> 1) as i16;
            }
        }
    }
    Ok(Grid { w, h: h * 2, d })
}

/// Inverse along rows: `w`×`h` lowpass and highpass in, `2w`×`h` out.
fn horizontal(pool: &mut Pool, lo: &View, hi: &View, w: usize, h: usize) -> Result<Grid> {
    check(lo, w, h)?;
    check(hi, w, h)?;
    let mut d = pool.get(w * h * 2);
    for (r, out) in d.chunks_exact_mut(w * 2).enumerate() {
        filter_row(&lo.d[r * lo.stride..r * lo.stride + w], &hi.d[r * hi.stride..r * hi.stride + w], out);
    }
    Ok(Grid { w: w * 2, h, d })
}

/// One spatial level: the lowpass and three highpass bands (in the order
/// they're numbered) of a `w`×`h` level in, `2w`×`2h` out, scaled up by
/// `shift` bits.
fn spatial(pool: &mut Pool, lo: &View, b1: &View, b2: &View, b3: &View, shift: u32) -> Result<Grid> {
    let (w, h) = (b1.w, b1.h);
    let a = vertical(pool, lo, b2, w, h)?;
    let b = vertical(pool, b1, b3, w, h)?;
    let mut g = horizontal(pool, &a.view(), &b.view(), w, h * 2)?;
    pool.put(a);
    pool.put(b);
    if shift > 0 {
        for v in &mut g.d {
            *v = v.wrapping_shl(shift);
        }
    }
    Ok(g)
}

/// The last spatial level, straight to output samples: a `pw`×`ph` plane
/// (`out`), clipped to `bpc` bits.
#[allow(clippy::too_many_arguments)]
fn last_level(pool: &mut Pool, lo: &View, b1: &View, b2: &View, b3: &View, out: &mut [u16], pw: usize, ph: usize, bpc: u32, progressive: bool, alpha: bool) -> Result<()> {
    let (w, h) = (b1.w, b1.h);
    if 2 * w < pw || 2 * h < ph || out.len() < pw * ph {
        return invalid("CineForm: the bands are smaller than the picture");
    }
    let max = (1i32 << bpc) - 1;
    if progressive {
        let a = vertical(pool, lo, b2, w, h)?;
        let b = vertical(pool, b1, b3, w, h)?;
        let mut line = pool.get(2 * w);
        for (r, dst) in out.chunks_exact_mut(pw).take(ph).enumerate() {
            filter_row(&a.d[r * w..r * w + w], &b.d[r * w..r * w + w], &mut line);
            if alpha {
                for (d, &v) in dst.iter_mut().zip(&line) {
                    // Undo the alpha channel's companding.
                    let v = (v as i32).clamp(0, max);
                    *d = ((((v - 256) << 3) * 9400) >> 16).clamp(0, 4095) as u16;
                }
            } else {
                for (d, &v) in dst.iter_mut().zip(&line) {
                    *d = (v as i32).clamp(0, max) as u16;
                }
            }
        }
        pool.0.push(line);
        pool.put(a);
        pool.put(b);
    } else {
        // Interlaced: the last vertical step is a Haar pair per field line.
        let a = horizontal(pool, lo, b1, w, h)?;
        let b = horizontal(pool, b2, b3, w, h)?;
        for (r, rows) in out.chunks_exact_mut(2 * pw).take(ph / 2).enumerate() {
            let (top, bottom) = rows.split_at_mut(pw);
            let l = &a.d[r * 2 * w..r * 2 * w + pw];
            let hh = &b.d[r * 2 * w..r * 2 * w + pw];
            for x in 0..pw {
                let (lv, hv) = (l[x] as i32, hh[x] as i32);
                top[x] = (((lv - hv) / 2) as i16 as i32).clamp(0, max) as u16;
                bottom[x] = (((lv + hv) / 2) as i16 as i32).clamp(0, max) as u16;
            }
        }
        pool.put(a);
        pool.put(b);
    }
    Ok(())
}

/// One channel's planes (`outs`: one for an intra sample, two for a group).
#[allow(clippy::too_many_arguments)]
fn reconstruct(pool: &mut Pool, b: &[Band], outs: &mut [&mut [u16]], prescale: &[u32; 8], pw: usize, ph: usize, bpc: u32, progressive: bool, alpha: bool) -> Result<()> {
    let v = |i: usize| b[i].view();
    if outs.len() == 1 {
        let l1 = spatial(pool, &v(0), &v(1), &v(2), &v(3), prescale[2])?;
        let l2 = spatial(pool, &l1.view(), &v(4), &v(5), &v(6), prescale[1])?;
        pool.put(l1);
        last_level(pool, &l2.view(), &v(7), &v(8), &v(9), outs[0], pw, ph, bpc, progressive, alpha)?;
        pool.put(l2);
        return Ok(());
    }
    // The two frames' sum and difference, at quarter size...
    let l1 = spatial(pool, &v(0), &v(1), &v(2), &v(3), prescale[5])?;
    let mut sum = spatial(pool, &l1.view(), &v(4), &v(5), &v(6), prescale[4])?;
    pool.put(l1);
    let mut diff = spatial(pool, &v(7), &v(8), &v(9), &v(10), prescale[3])?;
    // ...turned back into the two frames (the inverse Haar transform)...
    let (w, h) = (b[14].w, b[14].h);
    if sum.w < w || sum.h < h || diff.w < w || diff.h < h {
        return invalid("CineForm: group band sizes don't fit together");
    }
    for r in 0..h {
        let s = &mut sum.d[r * sum.w..r * sum.w + w];
        let d = &mut diff.d[r * diff.w..r * diff.w + w];
        for (a, c) in s.iter_mut().zip(d.iter_mut()) {
            let (lo, hi) = (*a as i32, *c as i32);
            *a = ((lo - hi) / 2) as i16;
            *c = ((lo + hi) / 2) as i16;
        }
    }
    // ...then each frame's last spatial level.
    let (first, second) = outs.split_at_mut(1);
    last_level(pool, &sum.view(), &v(14), &v(15), &v(16), first[0], pw, ph, bpc, progressive, alpha)?;
    last_level(pool, &diff.view(), &v(11), &v(12), &v(13), second[0], pw, ph, bpc, progressive, alpha)?;
    pool.put(sum);
    pool.put(diff);
    Ok(())
}

impl Decoder for CineformDecoder {
    fn send(&mut self, packet: &[u8], pts: i64) -> Result<()> {
        for mut p in self.decode(packet)? {
            p.pts = pts;
            self.out.push(p);
        }
        Ok(())
    }
    fn receive(&mut self) -> Option<Picture> {
        self.out.pop()
    }
    fn flush(&mut self) {
        self.out.clear();
        self.held = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codebooks_build() {
        let t = tables();
        assert_eq!(t.books[0].syms.len(), 1 + 63 * 2 + 7 + 1);
        assert_eq!(t.lut0[63], 240);
        assert_eq!(t.lut1[255] as i64, 255 + 768 * 255i64 * 255 * 255 / (1 << 24));
    }

    #[test]
    fn rejects_other_data() {
        let mut d = CineformDecoder::new(&StreamInfo::default());
        // Tags only, no picture data: nothing comes out.
        assert!(d.decode(&[0, 1, 0, 9, 0, 20, 2, 128]).unwrap().is_empty());
        // A second frame with no group before it.
        assert!(d.decode(&[0, 1, 0, 1]).is_err());
    }
}
