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
//! coding and the lossless second pass, and CineForm RAW (Bayer) as a 16-bit
//! sensor mosaic.

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
            Kind::Rgba => Format::Gbrap12,
            Kind::Bayer => Format::BayerRggb16,
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
                    // (a band is at most half the picture: 8K pictures have 4096-wide bands)
                    if bw < 3 || bh < 3 || bw > 8192 || bh > 8192 {
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
        if kind == Kind::Bayer {
            if group {
                return Err(Error::Unsupported("CineForm RAW in two-frame groups".into()));
            }
            return Ok(vec![self.bayer(&hd, &prescale)?]);
        }
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

impl CineformDecoder {
    /// CineForm RAW: four channels at half size, each one sample of every
    /// 2×2 block of the sensor's red-green / green-blue mosaic: a green
    /// average, red and blue as differences from it, and the difference
    /// between the two greens. They're decoded, put back together into the
    /// mosaic's red, green, green and blue, and scaled to 16 bits.
    fn bayer(&mut self, hd: &Header, prescale: &[u32; 8]) -> Result<Picture> {
        let (qw, qh) = (hd.width, hd.height);
        if self.planes.len() != 4 || qw == 0 || qh == 0 {
            return invalid("CineForm RAW: four channels needed");
        }
        let bpc = hd.bpc;
        self.pools.resize_with(4, Pool::default);
        let channels: Vec<Result<Vec<u16>>> = {
            use rayon::prelude::*;
            self.planes
                .par_iter()
                .zip(self.pools.par_iter_mut())
                .map(|(bands, pool)| {
                    let mut out = vec![0u16; qw * qh];
                    // Raw footage is always progressive.
                    reconstruct(pool, bands, &mut [out.as_mut_slice()], prescale, qw, qh, bpc, true, false)?;
                    Ok(out)
                })
                .collect()
        };
        let ch: Vec<Vec<u16>> = channels.into_iter().collect::<Result<_>>()?;
        let shown = if hd.display_height > 0 && hd.display_height < qh { hd.display_height } else { qh };
        let (w, h) = (qw * 2, shown * 2);
        let mut mosaic = vec![0u16; w * h];
        let mid = 1i32 << (bpc - 1);
        let up = 16 - bpc;
        for y in 0..shown {
            let (top, bottom) = mosaic[2 * y * w..(2 * y + 2) * w].split_at_mut(w);
            for x in 0..qw {
                let i = y * qw + x;
                let (g, rg, bg, gd) = (ch[0][i] as i32, ch[1][i] as i32, ch[2][i] as i32, ch[3][i] as i32 - mid);
                let out = |v: i32| (v << up).clamp(0, 65535) as u16;
                top[2 * x] = out((rg - mid) * 2 + g);
                top[2 * x + 1] = out(g + gd);
                bottom[2 * x] = out(g - gd);
                bottom[2 * x + 1] = out((bg - mid) * 2 + g);
            }
        }
        let mut pic = Picture::new(w as u32, h as u32, Format::BayerRggb16);
        pic.planes[0].put_u16_rows(&mosaic, w, w, h);
        Ok(pic)
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
#[inline(never)]
fn decode_band(t: &Tables, data: &[u8], b: &mut Band, codebook: u16, quant: i32, lossless: bool, second: bool, peak: &mut Peak, pkt: &[u8]) -> Result<usize> {
    let book = &t.books[(codebook != 0) as usize];
    let expected = b.data.len();
    // Every symbol's coefficient, dequantised once for the band (the codebook
    // and quantiser don't change inside it): (run, value as stored).
    let dequant = |level: i16| -> i32 {
        if lossless {
            return level as i32;
        }
        let m = level.unsigned_abs() as usize;
        let mag = match codebook {
            0 => t.lut0[m.min(63)],
            1 => t.lut1[m.min(255)],
            _ => m as i32,
        };
        mag.wrapping_mul(quant) * if level < 0 { -1 } else { 1 }
    };
    let mut vals = [(0u16, 0i32); 600];
    let syms = &book.syms[..book.syms.len().min(vals.len())];
    for (v, &(run, level)) in vals.iter_mut().zip(syms) {
        *v = (run, dequant(level));
    }
    let mut r = BitReader::new(data);
    let mut count = 0usize;
    let out = &mut b.data[..];
    loop {
        let Some(sym) = book.vlc.read(&mut r) else {
            return invalid("CineForm: bad code in a band");
        };
        let Some(&(run, coeff)) = vals.get(sym as usize) else {
            return invalid("CineForm: bad code in a band");
        };
        if run == 1 && !second {
            // The usual case: one coefficient (reading past the data ends the band: every
            // code takes a bit, so the work stays in step with the sample's size).
            if r.overrun {
                return invalid("CineForm: band runs past the sample");
            }
            let Some(v) = out.get_mut(count) else {
                return invalid("CineForm: band runs past its end");
            };
            *v = coeff as i16;
            count += 1;
            continue;
        }
        if run == 0 {
            break;
        }
        let run = run as usize;
        if count + run > expected || r.overrun {
            return invalid("CineForm: band runs past its end");
        }
        let o = &mut out[count..count + run];
        if second {
            for v in o {
                *v = ((*v as i32 | coeff.wrapping_mul(256)).wrapping_mul(quant)) as i16;
            }
        } else {
            o.fill(coeff as i16);
        }
        count += run;
    }
    if r.overrun {
        return invalid("CineForm: band runs past the sample");
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

/// The wavelet filters' middle parts with AVX2, 16 values at a time, in
/// 32-bit lanes so every sum is exactly the scalar code's (results wrap to
/// 16 bits the same way). Edges and leftovers stay with the scalar code.
#[cfg(target_arch = "x86_64")]
mod simd {
    use std::arch::x86_64::*;

    /// 16 values of `p − r` style middle-of-the-filter maths: from four rows of
    /// 16 (`a` before, `b` here, `c` after, `s` the highpass) to the even and
    /// odd outputs, as 16-bit lanes.
    #[inline]
    #[target_feature(enable = "avx2")]
    fn middle16(a: __m256i, b: __m256i, c: __m256i, s: __m256i) -> (__m256i, __m256i) {
        let four = _mm256_set1_epi32(4);
        // 16 × i16 → two halves of 8 × i32.
        let wide = |v: __m256i| (_mm256_cvtepi16_epi32(_mm256_castsi256_si128(v)), _mm256_cvtepi16_epi32(_mm256_extracti128_si256::<1>(v)));
        let half = |a: __m256i, b: __m256i, c: __m256i, s: __m256i| {
            // tmp = (a − c + 4) >> 3 fits in 16 bits, so it needs no wrapping.
            let t_even = _mm256_srai_epi32::<3>(_mm256_add_epi32(_mm256_sub_epi32(a, c), four));
            let t_odd = _mm256_srai_epi32::<3>(_mm256_add_epi32(_mm256_sub_epi32(c, a), four));
            let even = _mm256_srai_epi32::<1>(_mm256_add_epi32(_mm256_add_epi32(t_even, b), s));
            let odd = _mm256_srai_epi32::<1>(_mm256_sub_epi32(_mm256_add_epi32(t_odd, b), s));
            (even, odd)
        };
        let ((a0, a1), (b0, b1), (c0, c1), (s0, s1)) = (wide(a), wide(b), wide(c), wide(s));
        let (e0, o0) = half(a0, b0, c0, s0);
        let (e1, o1) = half(a1, b1, c1, s1);
        // Back to 16 bits, wrapping like `as i16`: sign-extend the low 16 bits,
        // then pack (no saturation needed) and undo the pack's lane order.
        let wrap = |v: __m256i| _mm256_srai_epi32::<16>(_mm256_slli_epi32::<16>(v));
        let pack = |lo: __m256i, hi: __m256i| _mm256_permute4x64_epi64::<0b11_01_10_00>(_mm256_packs_epi32(wrap(lo), wrap(hi)));
        (pack(e0, e1), pack(o0, o1))
    }

    /// The middle rows of the column filter: `w` columns, from rows `l0`
    /// (above), `l1`, `l2` (below) and highpass `h`. Returns how many
    /// columns it did (a multiple of 16).
    #[target_feature(enable = "avx2")]
    pub unsafe fn column_middle(l0: &[i16], l1: &[i16], l2: &[i16], h: &[i16], even: &mut [i16], odd: &mut [i16]) -> usize {
        let w = l0.len().min(l1.len()).min(l2.len()).min(h.len()).min(even.len()).min(odd.len());
        let mut x = 0;
        while x + 16 <= w {
            // SAFETY: x + 16 <= every slice's length.
            unsafe {
                let ld = |s: &[i16]| _mm256_loadu_si256(s.as_ptr().add(x) as *const __m256i);
                let (e, o) = middle16(ld(l0), ld(l1), ld(l2), ld(h));
                _mm256_storeu_si256(even.as_mut_ptr().add(x) as *mut __m256i, e);
                _mm256_storeu_si256(odd.as_mut_ptr().add(x) as *mut __m256i, o);
            }
            x += 16;
        }
        x
    }

    /// The middle of the row filter: outputs `2i`, `2i + 1` for `i` from 1,
    /// interleaved into `out`. Returns the first `i` it didn't do.
    #[target_feature(enable = "avx2")]
    pub unsafe fn row_middle(l: &[i16], h: &[i16], out: &mut [i16]) -> usize {
        let n = l.len();
        let mut i = 1;
        // Needs l[i + 16] (the value after the block), so stop 16 short of n − 1.
        while i + 17 < n && h.len() >= i + 16 && out.len() >= 2 * i + 32 {
            // SAFETY: i − 1 + 18 <= n, and the checks above.
            unsafe {
                let ld = |s: &[i16], at: usize| _mm256_loadu_si256(s.as_ptr().add(at) as *const __m256i);
                let (e, o) = middle16(ld(l, i - 1), ld(l, i), ld(l, i + 1), ld(h, i));
                // Interleave even and odd: e0 o0 e1 o1 …
                let lo = _mm256_unpacklo_epi16(e, o);
                let hi = _mm256_unpackhi_epi16(e, o);
                let p = out.as_mut_ptr().add(2 * i);
                _mm256_storeu_si256(p as *mut __m256i, _mm256_permute2x128_si256::<0x20>(lo, hi));
                _mm256_storeu_si256(p.add(16) as *mut __m256i, _mm256_permute2x128_si256::<0x31>(lo, hi));
            }
            i += 16;
        }
        i
    }
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
    // The middle: AVX2 for 16 values at a time where the CPU has it.
    let mut i = 1;
    #[cfg(target_arch = "x86_64")]
    if crate::idct::avx2_in_use() {
        // SAFETY: the CPU has AVX2 (checked), and the function keeps within
        // the slices it's given.
        i = unsafe { simd::row_middle(l, h, out) };
    }
    for ((o, w), s) in out[2 * i..2 * n - 2].chunks_exact_mut(2).zip(l[i - 1..].windows(3)).zip(&h[i..n - 1]) {
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
            let mut start = 0;
            #[cfg(target_arch = "x86_64")]
            if crate::idct::avx2_in_use() {
                // SAFETY: the CPU has AVX2 (checked); all slices hold `w` values.
                start = unsafe { simd::column_middle(l0, l1, l2, hh, even, odd) };
            }
            for x in start..w {
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

    /// The scalar filters, for checking the AVX2 ones against.
    fn filter_row_scalar(l: &[i16], h: &[i16]) -> Vec<i16> {
        let n = l.len();
        let mut out = vec![0i16; 2 * n];
        let (a, b, c, s) = (l[0] as i32, l[1] as i32, l[2] as i32, h[0] as i32);
        out[0] = ((tmp(11 * a - 4 * b + c + 4) + s) >> 1) as i16;
        out[1] = ((tmp(5 * a + 4 * b - c + 4) - s) >> 1) as i16;
        for i in 1..n - 1 {
            let (a, b, c, s) = (l[i - 1] as i32, l[i] as i32, l[i + 1] as i32, h[i] as i32);
            out[2 * i] = ((tmp(a - c + 4) + b + s) >> 1) as i16;
            out[2 * i + 1] = ((tmp(c - a + 4) + b - s) >> 1) as i16;
        }
        let (a, b, c, s) = (l[n - 3] as i32, l[n - 2] as i32, l[n - 1] as i32, h[n - 1] as i32);
        out[2 * n - 2] = ((tmp(5 * c + 4 * b - a + 4) + s) >> 1) as i16;
        out[2 * n - 1] = ((tmp(11 * c - 4 * b + a + 4) - s) >> 1) as i16;
        out
    }

    #[test]
    fn avx2_filters_match_the_scalar_ones_exactly() {
        // Random values over the whole 16-bit range (so the sums wrap), at
        // lengths around the 16-wide blocks.
        let mut seed = 0x1234_5678u32;
        let mut rnd = || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed as i16
        };
        for n in [3usize, 4, 16, 17, 18, 19, 33, 40, 100, 257] {
            let l: Vec<i16> = (0..n).map(|_| rnd()).collect();
            let h: Vec<i16> = (0..n).map(|_| rnd()).collect();
            let mut out = vec![0i16; 2 * n];
            filter_row(&l, &h, &mut out);
            assert_eq!(out, filter_row_scalar(&l, &h), "row filter, n = {n}");
            // Columns: three rows and a highpass row, `n` wide.
            let rows: Vec<Vec<i16>> = (0..4).map(|_| (0..n).map(|_| rnd()).collect()).collect();
            let lo: Vec<i16> = rows[..3].concat();
            let lo = View { w: n, h: 3, stride: n, d: &lo };
            let hi_rows: Vec<i16> = [rows[3].clone(), rows[3].clone(), rows[3].clone()].concat();
            let hi = View { w: n, h: 3, stride: n, d: &hi_rows };
            if n >= 3 {
                let g = vertical(&mut Pool::default(), &lo, &hi, n, 3).unwrap();
                // Row pair 1 is the middle case.
                for x in 0..n {
                    let (p, q, r, s) = (rows[0][x] as i32, rows[1][x] as i32, rows[2][x] as i32, rows[3][x] as i32);
                    assert_eq!(g.d[2 * n + x], ((tmp(p - r + 4) + q + s) >> 1) as i16, "column {x}, n = {n}");
                    assert_eq!(g.d[3 * n + x], ((tmp(r - p + 4) + q - s) >> 1) as i16, "column {x}, n = {n}");
                }
            }
        }
        eprintln!("AVX2 in use: {}", crate::idct::avx2_in_use());
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
