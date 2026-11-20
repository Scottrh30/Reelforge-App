//! OpenEXR: the high-dynamic-range image format of visual effects work
//! (renders, comps, plates), one file per frame, scene-linear floating point.
//!
//! Reelforge's own decoder, written from the OpenEXR file layout
//! documentation and checked against the reference library's output:
//! - single-part and multi-part files, scanline and tiled (the full-size
//!   level of mipmapped and ripmapped files)
//! - compression: none, RLE, ZIPS, ZIP, PIZ, PXR24, B44, B44A, DWAA and DWAB
//!   (HTJ2K and the newer ZSTD are left to FFmpeg's decoder)
//! - half, float and uint channels; R, G, B (and A) or Y (luminance), of the
//!   unnamed layer or else the first layer that has them (`beauty.R`…)
//! - the data window placed within the display window, transparent outside
//!
//! The picture comes out planar (G, B, R, A, as FFmpeg's `gbrap…`), in half
//! floats for a half-float file or 32-bit floats otherwise, scene-linear and
//! premultiplied, like the file.

// Reads files from anywhere: no position in a list is taken on trust (`get`,
// checked slices), so a damaged or hostile file is an error, never a crash.
#![cfg_attr(not(test), deny(clippy::indexing_slicing))]

mod b44;
mod dwa;

pub use dwa::f32_to_half;
mod piz;

use crate::{Decoder, Error, Format, Output, Picture, Plane, Result, StreamInfo};
use rayon::prelude::*;

pub(crate) fn fail<T>(s: &str) -> Result<T> {
    Err(Error::Invalid(format!("EXR: {s}")))
}

fn unsupported<T>(s: impl Into<String>) -> Result<T> {
    Err(Error::Unsupported(format!("EXR: {}", s.into())))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PixelType {
    Uint,
    Half,
    Float,
}

impl PixelType {
    fn bytes(self) -> usize {
        if self == PixelType::Half { 2 } else { 4 }
    }
}

#[derive(Clone, Debug)]
struct Channel {
    name: String,
    ty: PixelType,
    linear: bool,
    xs: i32,
    ys: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Compression {
    None,
    Rle,
    Zips,
    Zip,
    Piz,
    Pxr24,
    B44,
    B44a,
    Dwaa,
    Dwab,
}

impl Compression {
    fn lines(self) -> i32 {
        match self {
            Compression::None | Compression::Rle | Compression::Zips => 1,
            Compression::Zip | Compression::Pxr24 => 16,
            Compression::Piz | Compression::B44 | Compression::B44a | Compression::Dwaa => 32,
            Compression::Dwab => 256,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Box2 {
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
}

impl Box2 {
    fn width(&self) -> i64 {
        self.x1 as i64 - self.x0 as i64 + 1
    }
    fn height(&self) -> i64 {
        self.y1 as i64 - self.y0 as i64 + 1
    }
}

/// One part's header (what's needed to decode it).
#[derive(Clone, Debug)]
struct Header {
    channels: Vec<Channel>,
    compression: Compression,
    data: Box2,
    display: Box2,
    /// Tile size, for a tiled part.
    tiles: Option<(u32, u32)>,
    deep: bool,
    chunks: Option<usize>,
}

/// One channel within one chunk: how its samples are laid out.
#[derive(Clone, Debug)]
pub(crate) struct ChannelInfo {
    pub ty: PixelType,
    pub linear: bool,
    pub ys: i32,
    /// Samples per line, and sampled lines, in this chunk.
    pub nx: usize,
    pub ny: usize,
    /// 16-bit units per sample (2 for float and uint).
    pub units: usize,
}

/// A chunk's lines (`y0` and the `lines` after it) and channels.
#[derive(Clone, Debug)]
pub(crate) struct ChannelLayout {
    pub y0: i32,
    pub lines: usize,
    pub channels: Vec<ChannelInfo>,
}

impl ChannelLayout {
    fn new(h: &Header, x0: i32, x1: i32, y0: i32, y1: i32) -> Self {
        let channels = h
            .channels
            .iter()
            .map(|c| ChannelInfo { ty: c.ty, linear: c.linear, ys: c.ys, nx: num_samples(c.xs, x0, x1), ny: num_samples(c.ys, y0, y1), units: c.ty.bytes() / 2 })
            .collect();
        ChannelLayout { y0, lines: (y1 - y0 + 1) as usize, channels }
    }

    fn bytes(&self) -> usize {
        self.channels.iter().map(|c| c.nx * c.ny * c.units * 2).sum()
    }
}

/// How many multiples of `s` lie in `a..=b`.
fn num_samples(s: i32, a: i32, b: i32) -> usize {
    let (s, a, b) = (s as i64, a as i64, b as i64);
    (b.div_euclid(s) - (a - 1).div_euclid(s)).max(0) as usize
}

// ---- Reading the header -------------------------------------------------------

struct Reader<'a> {
    d: &'a [u8],
    p: usize,
}

impl<'a> Reader<'a> {
    fn bytes(&mut self, n: usize) -> Result<&'a [u8]> {
        // (lengths and offsets come from the file: no wrapping past the end)
        let Some(s) = self.p.checked_add(n).and_then(|end| self.d.get(self.p..end)) else {
            return fail("file cut short");
        };
        self.p += n;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8> {
        let [b] = self.bytes(1)? else { return fail("file cut short") };
        Ok(*b)
    }
    fn i32(&mut self) -> Result<i32> {
        Ok(i32::from_le_bytes(le4(self.bytes(4)?, 0)?))
    }
    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(le8(self.bytes(8)?, 0)?))
    }
    fn cstr(&mut self) -> Result<&'a str> {
        let rest = self.d.get(self.p..).unwrap_or_default();
        let n = rest.iter().position(|&b| b == 0).ok_or(Error::Invalid("EXR: unterminated name".into()))?;
        self.p += n + 1;
        std::str::from_utf8(rest.get(..n).unwrap_or_default()).map_err(|_| Error::Invalid("EXR: name isn't text".into()))
    }
}

/// Four bytes at `at` of `b` (cut short: a damaged file).
pub(crate) fn le4(b: &[u8], at: usize) -> Result<[u8; 4]> {
    b.get(at..at.saturating_add(4)).and_then(|s| s.try_into().ok()).ok_or_else(|| Error::Invalid("EXR: cut short".into()))
}

/// Eight bytes at `at` of `b`.
fn le8(b: &[u8], at: usize) -> Result<[u8; 8]> {
    b.get(at..at.saturating_add(8)).and_then(|s| s.try_into().ok()).ok_or_else(|| Error::Invalid("EXR: cut short".into()))
}

fn read_box(v: &[u8]) -> Result<Box2> {
    if v.len() < 16 {
        return fail("bad box");
    }
    let i = |k: usize| le4(v, k * 4).map(i32::from_le_bytes);
    let b = Box2 { x0: i(0)?, y0: i(1)?, x1: i(2)?, y1: i(3)? };
    if b.x1 < b.x0 || b.y1 < b.y0 || b.width() > 1 << 16 || b.height() > 1 << 16 {
        return fail("bad window");
    }
    Ok(b)
}

/// Read a header's attributes up to its terminating null.
fn read_header(r: &mut Reader, tiled_file: bool, deep_file: bool) -> Result<Header> {
    let (mut channels, mut compression, mut data, mut display, mut tiles, mut chunks) = (None, None, None, None, None, None);
    let mut deep = deep_file;
    let mut tiled = tiled_file;
    loop {
        let name = r.cstr()?;
        if name.is_empty() {
            break;
        }
        let ty = r.cstr()?;
        let size = r.i32()?;
        if size < 0 {
            return fail("bad attribute size");
        }
        let v = r.bytes(size as usize)?;
        match (name, ty) {
            ("channels", "chlist") => {
                let mut cr = Reader { d: v, p: 0 };
                let mut list = Vec::new();
                loop {
                    let n = cr.cstr()?;
                    if n.is_empty() {
                        break;
                    }
                    let t = match cr.i32()? {
                        0 => PixelType::Uint,
                        1 => PixelType::Half,
                        2 => PixelType::Float,
                        _ => return fail("bad channel type"),
                    };
                    let linear = cr.u8()? != 0;
                    cr.bytes(3)?;
                    let (xs, ys) = (cr.i32()?, cr.i32()?);
                    if xs < 1 || ys < 1 {
                        return fail("bad channel sampling");
                    }
                    list.push(Channel { name: n.to_string(), ty: t, linear, xs, ys });
                }
                channels = Some(list);
            }
            ("compression", "compression") => {
                compression = Some(match v.first().copied() {
                    Some(0) => Compression::None,
                    Some(1) => Compression::Rle,
                    Some(2) => Compression::Zips,
                    Some(3) => Compression::Zip,
                    Some(4) => Compression::Piz,
                    Some(5) => Compression::Pxr24,
                    Some(6) => Compression::B44,
                    Some(7) => Compression::B44a,
                    Some(8) => Compression::Dwaa,
                    Some(9) => Compression::Dwab,
                    Some(10 | 11) => return unsupported("HTJ2K compression"),
                    Some(c) => return unsupported(format!("compression {c}")),
                    None => return fail("bad compression"),
                });
            }
            ("dataWindow", "box2i") => data = Some(read_box(v)?),
            ("displayWindow", "box2i") => display = Some(read_box(v)?),
            ("tiles", "tiledesc") if v.len() >= 9 => {
                let (tx, ty) = (u32::from_le_bytes(le4(v, 0)?), u32::from_le_bytes(le4(v, 4)?));
                if tx == 0 || ty == 0 || tx > 1 << 16 || ty > 1 << 16 {
                    return fail("bad tile size");
                }
                tiles = Some((tx, ty));
            }
            ("type", "string") => {
                let t = std::str::from_utf8(v).unwrap_or("");
                deep = t.starts_with("deep");
                tiled = t == "tiledimage";
            }
            ("chunkCount", "int") if v.len() >= 4 => chunks = Some(i32::from_le_bytes(le4(v, 0)?).max(0) as usize),
            _ => {}
        }
    }
    let (Some(channels), Some(compression), Some(data), Some(display)) = (channels, compression, data, display) else {
        return fail("header is missing channels, compression or windows");
    };
    if tiled && tiles.is_none() {
        return fail("tiled without a tile size");
    }
    Ok(Header { channels, compression, data, display, tiles: if tiled { tiles } else { None }, deep, chunks })
}

/// Which channels make the picture: indices of R, G, B and maybe A (or Y).
#[derive(Debug)]
struct Pick {
    rgb: [usize; 3],
    alpha: Option<usize>,
}

fn pick_channels(ch: &[Channel]) -> Option<Pick> {
    let find = |name: &str| ch.iter().position(|c| c.name == name);
    // Layers, in order: the unnamed one, then any `layer.` prefix.
    let mut prefixes = vec![String::new()];
    for c in ch {
        if let Some(i) = c.name.rfind('.') {
            let p = c.name[..=i].to_string();
            if !prefixes.contains(&p) {
                prefixes.push(p);
            }
        }
    }
    for p in &prefixes {
        let get = |n: &str| find(&format!("{p}{n}")).or_else(|| find(&format!("{p}{}", n.to_lowercase())));
        if let (Some(r), Some(g), Some(b)) = (get("R"), get("G"), get("B")) {
            return Some(Pick { rgb: [r, g, b], alpha: get("A") });
        }
        if let Some(y) = get("Y") {
            return Some(Pick { rgb: [y, y, y], alpha: get("A") });
        }
    }
    None
}

#[inline]
pub fn half_to_f32(h: u16) -> f32 {
    let sign = ((h as u32) & 0x8000) << 16;
    let exp = ((h >> 10) & 0x1f) as u32;
    let man = (h & 0x3ff) as u32;
    let bits = match exp {
        0 if man == 0 => sign,
        0 => {
            // Subnormal: normalise.
            let mut e = 113u32;
            let mut m = man;
            while m & 0x400 == 0 {
                m <<= 1;
                e -= 1;
            }
            sign | (e << 23) | ((m & 0x3ff) << 13)
        }
        31 => sign | 0x7f80_0000 | (man << 13),
        _ => sign | ((exp + 112) << 23) | (man << 13),
    };
    f32::from_bits(bits)
}

// ---- Decompression ------------------------------------------------------------

fn inflate(data: &[u8], size: usize) -> Result<Vec<u8>> {
    let mut out = vec![0u8; size];
    // The checksum is skipped: a damaged chunk shows up as the wrong size, and
    // checking every byte again costs a tenth of the decode.
    let n = miniz_oxide::inflate::decompress_slice_iter_to_slice(&mut out, std::iter::once(data), true, true).map_err(|_| Error::Invalid("EXR: zlib data".into()))?;
    if n != size {
        return fail("zlib size");
    }
    Ok(out)
}

/// The predictor and byte interleaving ZIP and RLE share: each byte was
/// stored as its difference from the one before (plus 128), and the two
/// bytes of every 16-bit unit were split into a first and second half.
fn unpredict_deinterleave(t: &mut [u8]) -> Vec<u8> {
    unpredict(t);
    let n = t.len();
    let half = n.div_ceil(2);
    let mut out = vec![0u8; n];
    let (lo, hi) = t.split_at(half);
    let pairs = n / 2;
    let mut done = 0;
    #[cfg(target_arch = "x86_64")]
    {
        use std::arch::x86_64::*;
        // SAFETY: SSE2 is part of every x86-64 CPU; loads and stores stay
        // within `lo`, `hi` (16 bytes from i < pairs - 15) and `out` (32 from 2i).
        unsafe {
            while done + 16 <= pairs {
                let a = _mm_loadu_si128(lo.as_ptr().add(done) as *const __m128i);
                let b = _mm_loadu_si128(hi.as_ptr().add(done) as *const __m128i);
                _mm_storeu_si128(out.as_mut_ptr().add(done * 2) as *mut __m128i, _mm_unpacklo_epi8(a, b));
                _mm_storeu_si128(out.as_mut_ptr().add(done * 2 + 16) as *mut __m128i, _mm_unpackhi_epi8(a, b));
                done += 16;
            }
        }
    }
    // (the rest, and an odd last byte: from lo only)
    for ((o, &a), &b) in out.get_mut(done * 2..).unwrap_or_default().chunks_exact_mut(2).zip(lo.get(done..).unwrap_or_default()).zip(hi.get(done..).unwrap_or_default()) {
        o.copy_from_slice(&[a, b]);
    }
    if n % 2 == 1 {
        if let (Some(o), Some(&a)) = (out.last_mut(), lo.last()) {
            *o = a;
        }
    }
    out
}

/// `t[i] = t[i - 1] + t[i] - 128` along the bytes (wrapping): a running sum.
fn unpredict(t: &mut [u8]) {
    if t.len() < 2 {
        return;
    }
    let mut i = 1;
    #[cfg(target_arch = "x86_64")]
    {
        use std::arch::x86_64::*;
        // 16 bytes at a time: a prefix sum within the vector (shift-and-add in
        // four steps), plus the running total so far in every lane.
        // SAFETY: SSE2 is part of every x86-64 CPU; each load/store is 16
        // bytes at i with i + 16 <= len.
        unsafe {
            let bias = _mm_set1_epi8(-128i8);
            let mut carry = _mm_set1_epi8(t.first().copied().unwrap_or(0) as i8);
            while i + 16 <= t.len() {
                let p = t.as_mut_ptr().add(i) as *mut __m128i;
                let mut v = _mm_sub_epi8(_mm_loadu_si128(p), bias);
                v = _mm_add_epi8(v, _mm_slli_si128::<1>(v));
                v = _mm_add_epi8(v, _mm_slli_si128::<2>(v));
                v = _mm_add_epi8(v, _mm_slli_si128::<4>(v));
                v = _mm_add_epi8(v, _mm_slli_si128::<8>(v));
                v = _mm_add_epi8(v, carry);
                _mm_storeu_si128(p, v);
                let last = (_mm_extract_epi16::<7>(v) >> 8) as i8;
                carry = _mm_set1_epi8(last);
                i += 16;
            }
        }
    }
    let mut prev = t.get(i.wrapping_sub(1)).copied().unwrap_or(0);
    for b in t.get_mut(i..).unwrap_or_default() {
        *b = prev.wrapping_add(*b).wrapping_sub(128);
        prev = *b;
    }
}

fn rle(data: &[u8], size: usize) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(size);
    let mut i = 0;
    while let Some(&c) = data.get(i) {
        let c = c as i8;
        i += 1;
        if c < 0 {
            let n = (-(c as i32)) as usize;
            let Some(lit) = data.get(i..i + n) else { return fail("RLE literal") };
            out.extend_from_slice(lit);
            i += n;
        } else {
            let Some(&b) = data.get(i) else { return fail("RLE run") };
            out.extend(std::iter::repeat_n(b, c as usize + 1));
            i += 1;
        }
        if out.len() > size {
            return fail("RLE too long");
        }
    }
    if out.len() != size {
        return fail("RLE size");
    }
    Ok(out)
}

fn pxr24(data: &[u8], layout: &ChannelLayout, size: usize) -> Result<Vec<u8>> {
    // Floats keep 24 bits: the stored data is smaller than the block.
    let stored: usize = layout.channels.iter().map(|c| c.nx * c.ny * if c.ty == PixelType::Float { 3 } else { c.ty.bytes() }).sum();
    let t = inflate(data, stored)?;
    let mut out = Vec::with_capacity(size);
    let mut p = 0usize;
    for y in layout.y0..layout.y0 + layout.lines as i32 {
        for c in &layout.channels {
            if y.rem_euclid(c.ys) != 0 {
                continue;
            }
            let n = c.nx;
            let mut pixel = 0u32;
            // The row's bytes, one plane per byte of each value (most significant first).
            let planes = match c.ty {
                PixelType::Uint => 4,
                PixelType::Half => 2,
                PixelType::Float => 3,
            };
            let Some(row) = p.checked_add(planes * n).and_then(|end| t.get(p..end)) else { return fail("PXR24 size") };
            p += planes * n;
            let plane = |k: usize| row.get(k * n..(k + 1) * n).unwrap_or_default();
            match c.ty {
                PixelType::Uint => {
                    for (((a, b), c2), d) in plane(0).iter().zip(plane(1)).zip(plane(2)).zip(plane(3)) {
                        pixel = pixel.wrapping_add(u32::from_be_bytes([*a, *b, *c2, *d]));
                        out.extend_from_slice(&pixel.to_le_bytes());
                    }
                }
                PixelType::Half => {
                    for (a, b) in plane(0).iter().zip(plane(1)) {
                        pixel = pixel.wrapping_add(u32::from(u16::from_be_bytes([*a, *b])));
                        out.extend_from_slice(&(pixel as u16).to_le_bytes());
                    }
                }
                PixelType::Float => {
                    for ((a, b), c2) in plane(0).iter().zip(plane(1)).zip(plane(2)) {
                        pixel = pixel.wrapping_add(u32::from_be_bytes([*a, *b, *c2, 0]));
                        out.extend_from_slice(&pixel.to_le_bytes());
                    }
                }
            }
        }
    }
    if out.len() != size {
        return fail("PXR24 size");
    }
    Ok(out)
}

/// A chunk's data in the uncompressed layout.
fn decompress(h: &Header, layout: &ChannelLayout, data: &[u8]) -> Result<Vec<u8>> {
    let size = layout.bytes();
    // Data that compression didn't shrink is stored as it is.
    if data.len() == size || h.compression == Compression::None {
        if data.len() != size {
            return fail("uncompressed chunk size");
        }
        return Ok(data.to_vec());
    }
    match h.compression {
        // (returned above)
        Compression::None => Ok(data.to_vec()),
        Compression::Rle => Ok(unpredict_deinterleave(&mut rle(data, size)?)),
        Compression::Zips | Compression::Zip => Ok(unpredict_deinterleave(&mut inflate(data, size)?)),
        Compression::Pxr24 => pxr24(data, layout, size),
        Compression::Piz => {
            let mut out = vec![0u8; size];
            piz::decompress(data, layout, &mut out)?;
            Ok(out)
        }
        Compression::B44 | Compression::B44a => {
            let mut out = vec![0u8; size];
            b44::decompress(data, layout, &mut out)?;
            Ok(out)
        }
        Compression::Dwaa | Compression::Dwab => {
            let mut out = vec![0u8; size];
            dwa::decompress(data, layout, &h.channels, &mut out)?;
            Ok(out)
        }
    }
}

// ---- The image ----------------------------------------------------------------

/// Decode one EXR file to a float picture.
pub fn decode(file: &[u8]) -> Result<Picture> {
    let mut r = Reader { d: file, p: 0 };
    if r.bytes(4)? != [0x76, 0x2f, 0x31, 0x01] {
        return fail("not an EXR file");
    }
    let version = r.i32()?;
    if version & 0xff != 2 {
        return unsupported(format!("version {}", version & 0xff));
    }
    let (tiled, deep, multi) = (version & 0x200 != 0, version & 0x800 != 0, version & 0x1000 != 0);
    let mut headers = Vec::new();
    if multi {
        loop {
            if r.d.get(r.p) == Some(&0) {
                r.p += 1;
                break;
            }
            headers.push(read_header(&mut r, false, false)?);
        }
    } else {
        headers.push(read_header(&mut r, tiled, deep)?);
    }
    // Offset tables, one per part.
    let mut tables = Vec::with_capacity(headers.len());
    for h in &headers {
        let n = match (h.chunks, h.tiles) {
            (Some(n), _) => n,
            (None, Some((tx, ty))) => tile_count(h, tx, ty),
            (None, None) => (h.data.height() as usize).div_ceil(h.compression.lines() as usize),
        };
        let mut t = Vec::with_capacity(n.min(1 << 20));
        for _ in 0..n {
            t.push(r.u64()?);
        }
        tables.push(t);
    }
    // The first part that's a flat image with colour channels.
    let Some((part, h, pick)) = headers.iter().enumerate().find_map(|(i, h)| if h.deep { None } else { pick_channels(&h.channels).map(|p| (i, h, p)) }) else {
        return unsupported("no R, G, B or Y channels");
    };
    for &c in pick.rgb.iter().chain(&pick.alpha) {
        let Some(ch) = h.channels.get(c) else { return fail("channel list") };
        if ch.xs != 1 || ch.ys != 1 {
            return unsupported("subsampled colour channels");
        }
    }
    let (w, ht) = (h.display.width() as u32, h.display.height() as u32);
    // Half-float files stay half floats (no conversion either way); anything
    // with a float or uint colour channel comes out as 32-bit float.
    let half = pick.rgb.iter().chain(&pick.alpha).all(|&c| h.channels.get(c).is_some_and(|ch| ch.ty == PixelType::Half));
    let format = match (pick.alpha.is_some(), half) {
        (true, true) => Format::Gbrapf16,
        (false, true) => Format::Gbrpf16,
        (true, false) => Format::Gbrapf32,
        (false, false) => Format::Gbrpf32,
    };

    // Each chunk's place and data.
    let mut jobs: Vec<(ChannelLayout, i32, &[u8])> = Vec::new();
    let table = tables.get(part).map(Vec::as_slice).unwrap_or_default();
    let level0 = match h.tiles {
        Some((tx, ty)) => tiles_across(h.data.width(), tx) * tiles_across(h.data.height(), ty),
        None => table.len(),
    };
    for &off in table.iter().take(level0) {
        let mut cr = Reader { d: file, p: off as usize };
        if multi && cr.i32()? as usize != part {
            return fail("chunk of another part");
        }
        let (layout, x0) = match h.tiles {
            Some((tx, ty)) => {
                let (dx, dy, lx, ly) = (cr.i32()?, cr.i32()?, cr.i32()?, cr.i32()?);
                if lx != 0 || ly != 0 {
                    continue;
                }
                let x0 = h.data.x0 + dx * tx as i32;
                let y0 = h.data.y0 + dy * ty as i32;
                if dx < 0 || dy < 0 || x0 > h.data.x1 || y0 > h.data.y1 {
                    return fail("tile outside the image");
                }
                let x1 = (x0 + tx as i32 - 1).min(h.data.x1);
                let y1 = (y0 + ty as i32 - 1).min(h.data.y1);
                (ChannelLayout::new(h, x0, x1, y0, y1), x0)
            }
            None => {
                let y0 = cr.i32()?;
                if y0 < h.data.y0 || y0 > h.data.y1 {
                    return fail("chunk outside the image");
                }
                let y1 = (y0 + h.compression.lines() - 1).min(h.data.y1);
                (ChannelLayout::new(h, h.data.x0, h.data.x1, y0, y1), h.data.x0)
            }
        };
        let size = cr.i32()?;
        if size < 0 {
            return fail("bad chunk size");
        }
        jobs.push((layout, x0, cr.bytes(size as usize)?));
    }

    // Each chunk is decompressed and its picked channels' rows put in place on
    // a worker thread: chunks cover separate pixels, so they never write the same bytes.
    let bps = format.bytes_per_sample();
    let stride = w as usize * bps;
    let mut planes: Vec<Vec<u8>> = (0..format.planes()).map(|_| vec![0u8; stride * ht as usize]).collect();
    let slot = |c: usize| -> Option<usize> {
        if c == pick.rgb[1] && pick.rgb[0] != pick.rgb[1] {
            Some(0)
        } else if c == pick.rgb[2] && pick.rgb[0] != pick.rgb[2] {
            Some(1)
        } else if c == pick.rgb[0] {
            Some(2)
        } else if Some(c) == pick.alpha {
            Some(3)
        } else {
            None
        }
    };
    let gray = pick.rgb[0] == pick.rgb[1];
    struct Out(Vec<*mut u8>);
    // SAFETY (Send/Sync): the planes outlive the parallel loop, and every
    // chunk writes only the pixels it covers (see above).
    unsafe impl Send for Out {}
    unsafe impl Sync for Out {}
    impl Out {
        // (A method, so closures capture the whole `Out`, not its field.)
        fn plane(&self, t: usize) -> Option<*mut u8> {
            self.0.get(t).copied()
        }
    }
    let out = Out(planes.iter_mut().map(|p| p.as_mut_ptr()).collect());
    let results: Vec<Result<()>> = jobs
        .par_iter()
        .map(|(layout, x0, data)| {
            let block = decompress(h, layout, data)?;
            // The part of each row inside the display window.
            let first = (h.display.x0 as i64 - *x0 as i64).max(0) as usize;
            let mut p = 0usize;
            for y in layout.y0..layout.y0 + layout.lines as i32 {
                let oy = y as i64 - h.display.y0 as i64;
                for (ci, c) in layout.channels.iter().enumerate() {
                    if y.rem_euclid(c.ys) != 0 {
                        continue;
                    }
                    let bytes = c.nx * c.units * 2;
                    let Some(row) = p.checked_add(bytes).and_then(|end| block.get(p..end)) else { return fail("chunk size mismatch") };
                    p += bytes;
                    let Some(s) = slot(ci) else { continue };
                    if oy < 0 || oy >= ht as i64 {
                        continue;
                    }
                    let ox0 = (*x0 as i64 + first as i64 - h.display.x0 as i64) as usize;
                    let n = c.nx.saturating_sub(first).min((w as usize).saturating_sub(ox0));
                    if n == 0 {
                        continue;
                    }
                    let targets: &[usize] = if gray && s == 2 { &[0, 1, 2] } else { std::slice::from_ref(&s) };
                    // (the row's samples from `first` on, as stored)
                    let from = |size: usize| row.get(first * size..).unwrap_or_default();
                    for &t in targets {
                        let Some(base) = out.plane(t) else { continue };
                        // SAFETY: inside plane `t` (row `oy`, columns ox0..ox0+n), which only this chunk writes.
                        let dst = unsafe { std::slice::from_raw_parts_mut(base.add(oy as usize * stride + ox0 * bps), n * bps) };
                        match (c.ty, half) {
                            (PixelType::Half, true) | (PixelType::Float, false) => {
                                let Some(src) = from(c.ty.bytes()).get(..dst.len()) else { return fail("chunk size mismatch") };
                                dst.copy_from_slice(src);
                            }
                            (PixelType::Half, false) => {
                                for (d, s) in dst.chunks_exact_mut(4).zip(from(2).chunks_exact(2)) {
                                    d.copy_from_slice(&half_to_f32(u16::from_le_bytes([s.first().copied().unwrap_or(0), s.get(1).copied().unwrap_or(0)])).to_le_bytes());
                                }
                            }
                            (PixelType::Uint, false) => {
                                for (d, s) in dst.chunks_exact_mut(4).zip(from(4).chunks_exact(4)) {
                                    d.copy_from_slice(&(u32::from_le_bytes(le4(s, 0)?) as f32).to_le_bytes());
                                }
                            }
                            // `half` means every picked channel is half (done above).
                            (_, true) => {}
                        }
                    }
                }
            }
            if p != block.len() {
                return fail("chunk size mismatch");
            }
            Ok(())
        })
        .collect();
    drop(out);
    results.into_iter().collect::<Result<()>>()?;
    let mut pic = Picture::new(0, 0, format);
    pic.width = w;
    pic.height = ht;
    pic.full_range = true;
    pic.planes = planes.into_iter().map(|data| Plane { stride, data }).collect();
    Ok(pic)
}

fn tiles_across(n: i64, t: u32) -> usize {
    (n as usize).div_ceil(t as usize)
}

/// Tiles in every level of a tiled part (for a file without a chunk count).
fn tile_count(h: &Header, tx: u32, ty: u32) -> usize {
    // Only the full-size level is read; its tiles come first in the table.
    // Without the level mode at hand, the table is read up to level 0's size.
    tiles_across(h.data.width(), tx) * tiles_across(h.data.height(), ty)
}

/// The decoder: each packet is one EXR file.
pub struct ExrDecoder {
    out: Output,
}

impl ExrDecoder {
    pub fn new(_info: &StreamInfo) -> Self {
        ExrDecoder { out: Output::default() }
    }
}

impl Decoder for ExrDecoder {
    fn send(&mut self, packet: &[u8], pts: i64) -> Result<()> {
        let mut p = decode(packet)?;
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
    fn half_floats_convert_exactly() {
        assert_eq!(half_to_f32(0x3c00), 1.0);
        assert_eq!(half_to_f32(0xc000), -2.0);
        assert_eq!(half_to_f32(0x7bff), 65504.0);
        assert_eq!(half_to_f32(0x0001), 5.960_464_5e-8);
        assert_eq!(half_to_f32(0x03ff), 6.097_555e-5);
        assert_eq!(half_to_f32(0x7c00), f32::INFINITY);
        assert!(half_to_f32(0x7e00).is_nan());
        assert_eq!(half_to_f32(0x8000).to_bits(), 0x8000_0000);
        assert_eq!(half_to_f32(0x3555), 0.333_251_95);
    }

    #[test]
    fn the_fast_predictor_and_interleave_match_the_plain_ones() {
        for n in [0usize, 1, 2, 3, 15, 16, 17, 31, 32, 33, 100, 1001, 4096] {
            let src: Vec<u8> = (0..n).map(|i| (i as u32).wrapping_mul(2_654_435_761).rotate_left(7) as u8).collect();
            let mut plain = src.clone();
            for i in 1..n {
                plain[i] = plain[i - 1].wrapping_add(plain[i]).wrapping_sub(128);
            }
            let half = n.div_ceil(2);
            let want: Vec<u8> = (0..n).map(|i| if i % 2 == 0 { plain[i / 2] } else { plain[half + i / 2] }).collect();
            assert_eq!(unpredict_deinterleave(&mut src.clone()), want, "{n} bytes");
        }
    }

    #[test]
    fn sample_counts() {
        assert_eq!(num_samples(1, 0, 9), 10);
        assert_eq!(num_samples(2, 0, 9), 5);
        assert_eq!(num_samples(2, 1, 9), 4);
        assert_eq!(num_samples(2, -3, 3), 3);
        assert_eq!(num_samples(3, 4, 5), 0);
    }
}
