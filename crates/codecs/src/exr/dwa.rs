//! DWAA and DWAB: DreamWorks Animation's lossy compression. Colour channels
//! go through an 8×8 DCT (R, G and B of one layer together, as Y'CbCr, after
//! a gamma-like curve); alpha is run-length coded; everything else is zlib.
//! DWAA works on 32 lines at a time, DWAB on 256; the data is the same.
//!
//! Written from the format as OpenEXR's reference library (BSD-3-Clause)
//! reads it, and checked against that library's output.

use super::{fail, half_to_f32, inflate, piz, rle, unpredict_deinterleave, Channel, ChannelLayout, PixelType};
use crate::Result;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scheme {
    Unknown,
    Lossy,
    Rle,
}

/// Which channels a rule picks (by the name after the last `.` and the type)
/// and what it does with them.
#[derive(Clone, Debug)]
struct Rule {
    suffix: String,
    scheme: Scheme,
    ty: PixelType,
    /// 0, 1, 2 for the R, G, B of a colour set; -1 otherwise.
    csc: i8,
    nocase: bool,
}

impl Rule {
    fn matches(&self, suffix: &str, ty: PixelType) -> bool {
        self.ty == ty && if self.nocase { self.suffix.eq_ignore_ascii_case(suffix) } else { self.suffix == suffix }
    }
}

/// The rules of files written before rules were stored in the data.
fn legacy_rules() -> Vec<Rule> {
    let mut v = Vec::new();
    for (names, scheme, csc) in [
        (&["r", "red"][..], Scheme::Lossy, 0),
        (&["g", "grn", "green"][..], Scheme::Lossy, 1),
        (&["b", "blu", "blue"][..], Scheme::Lossy, 2),
        (&["y", "by", "ry"][..], Scheme::Lossy, -1),
        (&["a"][..], Scheme::Rle, -1),
    ] {
        for n in names {
            let types: &[PixelType] = if scheme == Scheme::Rle { &[PixelType::Uint, PixelType::Half, PixelType::Float] } else { &[PixelType::Half, PixelType::Float] };
            for &ty in types {
                v.push(Rule { suffix: n.to_string(), scheme, ty, csc, nocase: true });
            }
        }
    }
    v
}

fn read_rules(b: &[u8]) -> Result<Vec<Rule>> {
    let mut rules = Vec::new();
    let mut p = 0usize;
    while p < b.len() {
        let rest = b.get(p..).unwrap_or_default();
        if rest.len() <= 3 {
            return fail("DWA rule cut short");
        }
        let Some(len) = rest.iter().take(129).position(|&c| c == 0) else { return fail("DWA rule name") };
        let (Some(name), Some(&value), Some(&ty)) = (rest.get(..len), rest.get(len + 1), rest.get(len + 2)) else { return fail("DWA rule cut short") };
        let csc = (value >> 4) as i8 - 1;
        if !(-1..3).contains(&csc) {
            return fail("DWA rule colour index");
        }
        let scheme = match (value >> 2) & 3 {
            0 => Scheme::Unknown,
            1 => Scheme::Lossy,
            2 => Scheme::Rle,
            _ => return fail("DWA rule scheme"),
        };
        let ty = match ty {
            0 => PixelType::Uint,
            1 => PixelType::Half,
            2 => PixelType::Float,
            _ => return fail("DWA rule type"),
        };
        rules.push(Rule { suffix: String::from_utf8_lossy(name).into_owned(), scheme, ty, csc, nocase: value & 1 != 0 });
        p += len + 3;
    }
    Ok(rules)
}

fn le16(b: &[u8]) -> u16 {
    match b {
        [a, b, ..] => u16::from_le_bytes([*a, *b]),
        _ => 0,
    }
}

fn u64_at(b: &[u8], i: usize) -> Result<usize> {
    let Some(v) = b.get(i * 8..i * 8 + 8) else { return fail("DWA header cut short") };
    let mut a = [0u8; 8];
    a.copy_from_slice(v);
    usize::try_from(u64::from_le_bytes(a)).or_else(|_| fail("DWA size"))
}

/// Decompress one DWAA/DWAB chunk into `out` (the uncompressed layout: line
/// by line, channel by channel, little-endian).
pub fn decompress(data: &[u8], layout: &ChannelLayout, names: &[Channel], out: &mut [u8]) -> Result<()> {
    const HEADER: usize = 11 * 8;
    if data.len() < HEADER {
        return fail("DWA header cut short");
    }
    let [version, unknown_raw, unknown_size, ac_size, dc_size, rle_size, rle_packed, rle_raw, ac_count, dc_count, ac_compression] = {
        let mut c = [0usize; 11];
        for (i, v) in c.iter_mut().enumerate() {
            *v = u64_at(data, i)?;
        }
        c
    };
    if version > 2 {
        return fail("DWA version");
    }
    let mut p = HEADER;
    let rules = if version < 2 {
        legacy_rules()
    } else {
        let Some(&[a, b]) = data.get(p..p + 2) else { return fail("DWA rules cut short") };
        let size = u16::from_le_bytes([a, b]) as usize;
        let Some(rb) = data.get(p + 2..p + size.max(2)).filter(|_| size >= 2) else { return fail("DWA rules cut short") };
        p += size;
        read_rules(rb)?
    };
    if layout.channels.len() != names.len() {
        return fail("DWA channel list");
    }

    // Each channel's scheme (the last rule that matches wins), and the colour
    // sets: R, G, B channels of one layer, sampled alike.
    let mut schemes = vec![Scheme::Unknown; names.len()];
    let mut layers: Vec<(&str, [Option<usize>; 3])> = Vec::new();
    for (c, ch) in names.iter().enumerate() {
        let (prefix, suffix) = match ch.name.rfind('.') {
            Some(i) => (ch.name.get(..=i).unwrap_or_default(), ch.name.get(i + 1..).unwrap_or_default()),
            None => ("", ch.name.as_str()),
        };
        let li = match layers.iter().position(|(n, _)| *n == prefix) {
            Some(i) => i,
            None => {
                layers.push((prefix, [None; 3]));
                layers.len() - 1
            }
        };
        for r in &rules {
            if r.matches(suffix, ch.ty) {
                if let Some(s) = schemes.get_mut(c) {
                    *s = r.scheme;
                }
                if let (Ok(k), Some(l)) = (usize::try_from(r.csc), layers.get_mut(li)) {
                    if let Some(slot) = l.1.get_mut(k) {
                        *slot = Some(c);
                    }
                }
            }
        }
    }
    let sets: Vec<[usize; 3]> = layers
        .iter()
        .filter_map(|(_, s)| match *s {
            [Some(r), Some(g), Some(b)] => Some([r, g, b]),
            _ => None,
        })
        .filter(|s| {
            let n: Vec<&Channel> = s.iter().filter_map(|&i| names.get(i)).collect();
            n.len() == 3 && n.windows(2).all(|w| matches!(w, [a, b] if a.xs == b.xs && a.ys == b.ys))
        })
        .collect();

    let size_of = |c: &super::ChannelInfo| c.nx * c.ny * c.units * 2;
    let need = |s: Scheme| -> usize { layout.channels.iter().zip(&schemes).filter(|(_, &k)| k == s).map(|(c, _)| size_of(c)).sum() };
    let (need_unknown, need_rle) = (need(Scheme::Unknown), need(Scheme::Rle));
    for (needed, raw, packed, extra) in [(need_unknown, unknown_raw, unknown_size, 0), (need_rle, rle_raw, rle_size, rle_packed)] {
        if needed > 0 {
            if raw < needed || packed == 0 {
                return fail("DWA section sizes");
            }
        } else if raw > 0 || packed > 0 || extra > 0 {
            return fail("DWA section sizes");
        }
    }
    if unknown_raw > out.len() || rle_raw > out.len() {
        return fail("DWA section sizes");
    }
    if layout.channels.iter().zip(&schemes).any(|(c, &s)| s == Scheme::Lossy && c.ty == PixelType::Uint) {
        return fail("DWA lossy uint channel");
    }

    // The four sections, one after another.
    let mut take = |n: usize| -> Result<&[u8]> {
        let Some(s) = p.checked_add(n).and_then(|end| data.get(p..end)) else { return fail("DWA data cut short") };
        p += n;
        Ok(s)
    };
    let (unknown_data, ac_data, dc_data, rle_data) = (take(unknown_size)?, take(ac_size)?, take(dc_size)?, take(rle_size)?);

    let unknown = if unknown_size > 0 { inflate(unknown_data, unknown_raw)? } else { Vec::new() };

    // Room for every block's coefficients, at most.
    let blocks: usize = layout.channels.iter().zip(&schemes).filter(|(_, &s)| s == Scheme::Lossy).map(|(c, _)| c.nx.div_ceil(8) * c.ny.div_ceil(8)).sum();
    if ac_count > blocks * 63 || dc_count > blocks {
        return fail("DWA coefficient count");
    }
    let mut ac = vec![0u16; ac_count];
    if ac_size > 0 {
        match ac_compression {
            0 => piz::huf_uncompress(ac_data, &mut ac)?,
            1 => {
                for (v, b) in ac.iter_mut().zip(inflate(ac_data, ac_count * 2)?.chunks_exact(2)) {
                    *v = le16(b);
                }
            }
            _ => return fail("DWA AC compression"),
        }
    }
    let dc: Vec<u16> = if dc_size > 0 {
        unpredict_deinterleave(&mut inflate(dc_data, dc_count * 2)?).chunks_exact(2).map(le16).collect()
    } else if dc_count != 0 {
        return fail("DWA DC count");
    } else {
        Vec::new()
    };
    let rle_bytes = if rle_raw > 0 {
        if rle_packed > rle_raw.saturating_mul(2).saturating_add(1024) {
            return fail("DWA RLE size");
        }
        rle(&inflate(rle_data, rle_packed)?, rle_raw)?
    } else {
        Vec::new()
    };

    // Where each channel's lines start in `out`.
    let mut rows: Vec<Vec<usize>> = vec![Vec::new(); layout.channels.len()];
    let mut o = 0usize;
    for y in layout.y0..layout.y0 + layout.lines as i32 {
        for (ci, c) in layout.channels.iter().enumerate() {
            if y.rem_euclid(c.ys) != 0 {
                continue;
            }
            if let Some(r) = rows.get_mut(ci) {
                r.push(o);
            }
            o += c.nx * c.units * 2;
        }
    }
    if o != out.len() || rows.iter().zip(&layout.channels).any(|(r, c)| r.len() != c.ny) {
        return fail("DWA block size");
    }

    let tables = Tables::get();
    let mut done = vec![false; layout.channels.len()];
    let mut cur = Cursor { ac: &ac, dc: &dc };
    for set in &sets {
        if set.iter().any(|&c| schemes.get(c) != Some(&Scheme::Lossy)) {
            return fail("DWA colour set");
        }
        let chans: Vec<Plane> = set.iter().map(|&c| plane(layout, &rows, c)).collect::<Result<_>>()?;
        lossy(&mut cur, &chans, Some(&*tables.to_linear), out)?;
        for &c in set {
            if let Some(d) = done.get_mut(c) {
                *d = true;
            }
        }
    }
    let (mut unknown_at, mut rle_at) = (0usize, 0usize);
    for (c, info) in layout.channels.iter().enumerate() {
        if done.get(c) == Some(&true) || info.nx == 0 || info.ny == 0 {
            continue;
        }
        let row_bytes = info.nx * info.units * 2;
        let starts = rows.get(c).map(|r| r.as_slice()).unwrap_or_default();
        match schemes.get(c) {
            Some(Scheme::Lossy) => {
                let lin = if info.linear { None } else { Some(&*tables.to_linear) };
                lossy(&mut cur, &[plane(layout, &rows, c)?], lin, out)?;
            }
            Some(Scheme::Rle) => {
                // Byte planes: every first byte of the channel, then every second…
                let n = info.nx * info.ny;
                let bytes = info.units * 2;
                let Some(src) = rle_at.checked_add(n * bytes).and_then(|e| rle_bytes.get(rle_at..e)) else { return fail("DWA RLE data") };
                for (j, &s) in starts.iter().enumerate() {
                    let Some(dst) = out.get_mut(s..s + row_bytes) else { return fail("DWA block size") };
                    for (x, d) in dst.chunks_exact_mut(bytes).enumerate() {
                        for (k, b) in d.iter_mut().enumerate() {
                            *b = src.get(k * n + j * info.nx + x).copied().unwrap_or(0);
                        }
                    }
                }
                rle_at += n * bytes;
            }
            _ => {
                for &s in starts {
                    let (Some(src), Some(dst)) = (unknown.get(unknown_at..unknown_at + row_bytes), out.get_mut(s..s + row_bytes)) else { return fail("DWA unknown data") };
                    dst.copy_from_slice(src);
                    unknown_at += row_bytes;
                }
            }
        }
    }
    Ok(())
}

/// One channel of a chunk, for the DCT decoder.
struct Plane<'a> {
    rows: &'a [usize],
    w: usize,
    h: usize,
    float: bool,
}

fn plane<'a>(layout: &ChannelLayout, rows: &'a [Vec<usize>], c: usize) -> Result<Plane<'a>> {
    let (Some(info), Some(r)) = (layout.channels.get(c), rows.get(c)) else { return fail("DWA channel list") };
    Ok(Plane { rows: r, w: info.nx, h: info.ny, float: info.ty == PixelType::Float })
}

/// How far into the AC and DC coefficients the channels so far have read.
struct Cursor<'a> {
    ac: &'a [u16],
    dc: &'a [u16],
}

/// Zig-zag position of each coefficient, in rows.
const ZIGZAG: [usize; 64] = [
    0, 1, 5, 6, 14, 15, 27, 28, 2, 4, 7, 13, 16, 26, 29, 42, 3, 8, 12, 17, 25, 30, 41, 43, 9, 11, 18, 24, 31, 40, 44, 53, 10, 19, 23, 32, 39, 45, 52, 54, 20, 22, 33, 38, 46, 51, 55, 60, 21, 34, 37, 47, 50, 56, 59,
    61, 35, 36, 48, 49, 57, 58, 62, 63,
];

/// Decode the DCT blocks of one channel or of a colour set (3, as Y'CbCr),
/// writing half or float samples to `out`.
fn lossy(cur: &mut Cursor, chans: &[Plane], to_linear: Option<&[u16; 65536]>, out: &mut [u8]) -> Result<()> {
    let Some(first) = chans.first() else { return Ok(()) };
    let (w, h) = (first.w, first.h);
    if w == 0 || h == 0 {
        return Ok(());
    }
    let (bx, by) = (w.div_ceil(8), h.div_ceil(8));
    let n = chans.len();
    let Some(dc_all) = cur.dc.get(..n * bx * by) else { return fail("DWA DC data cut short") };
    cur.dc = cur.dc.get(n * bx * by..).unwrap_or_default();
    let mut ac = cur.ac;
    // A row of blocks, per channel: 8 lines of the blocks' half values.
    let mut row = vec![0u16; n * bx * 64];
    let mut dct = vec![[0f32; 64]; n];
    for yb in 0..by {
        for xb in 0..bx {
            let mut constant = true;
            for (k, d) in dct.iter_mut().enumerate() {
                let mut zig = [0u16; 64];
                zig[0] = dc_all.get(k * bx * by + yb * bx + xb).copied().unwrap_or(0);
                // The AC coefficients: values, runs of zeros (0xffNN), end of block (0xff00).
                let mut i = 1;
                let mut last = 0;
                while i < 64 {
                    let Some((&v, rest)) = ac.split_first() else { return fail("DWA AC data cut short") };
                    ac = rest;
                    if v & 0xff00 == 0xff00 {
                        let run = (v & 0xff) as usize;
                        i += if run == 0 { 64 } else { run };
                    } else {
                        if let Some(z) = zig.get_mut(i) {
                            *z = v;
                        }
                        last = i;
                        i += 1;
                    }
                }
                if last == 0 {
                    let v = half_to_f32(zig[0]) * 3.535536e-01f32 * 3.535536e-01f32;
                    d.fill(v);
                } else {
                    constant = false;
                    for (o, &z) in d.iter_mut().zip(ZIGZAG.iter()) {
                        *o = half_to_f32(zig.get(z).copied().unwrap_or(0));
                    }
                    idct8x8(d, zeroed_rows(last));
                }
            }
            if n == 3 {
                let [r, g, b] = dct.as_mut_slice() else { return fail("DWA colour set") };
                let m = if constant { 1 } else { 64 };
                for ((r, g), b) in r.iter_mut().zip(g.iter_mut()).zip(b.iter_mut()).take(m) {
                    let (y, cb, cr) = (*r, *g, *b);
                    *r = y + 1.5747 * cr;
                    *g = y - 0.1873 * cb - 0.4682 * cr;
                    *b = y + 1.8556 * cb;
                }
            }
            for (k, d) in dct.iter().enumerate() {
                let Some(dst) = row.get_mut((k * bx + xb) * 64..(k * bx + xb + 1) * 64) else { return fail("DWA block") };
                if constant {
                    dst.fill(f32_to_half(d[0]));
                } else {
                    for (o, &v) in dst.iter_mut().zip(d.iter()) {
                        *o = f32_to_half(v);
                    }
                }
            }
        }
        // Unblock into the lines, back to linear light.
        for (k, ch) in chans.iter().enumerate() {
            for ly in 0..8.min(h - yb * 8) {
                let Some(&start) = ch.rows.get(yb * 8 + ly) else { return fail("DWA lines") };
                let unit = if ch.float { 4 } else { 2 };
                let Some(dst) = out.get_mut(start..start + w * unit) else { return fail("DWA block size") };
                for (x, o) in dst.chunks_exact_mut(unit).enumerate() {
                    let v = row.get((k * bx + x / 8) * 64 + ly * 8 + x % 8).copied().unwrap_or(0);
                    let v = match to_linear {
                        Some(t) => t.get(v as usize).copied().unwrap_or(0),
                        None => v,
                    };
                    if ch.float {
                        o.copy_from_slice(&half_to_f32(v).to_le_bytes());
                    } else {
                        o.copy_from_slice(&v.to_le_bytes());
                    }
                }
            }
        }
    }
    cur.ac = ac;
    Ok(())
}

/// How many of the block's last rows are all zero, from the last non-zero
/// coefficient's zig-zag index.
fn zeroed_rows(last: usize) -> usize {
    match last {
        0..2 => 7,
        2 => 6,
        3..9 => 5,
        9 => 4,
        10..20 => 3,
        20 => 2,
        21..35 => 1,
        _ => 0,
    }
}

/// The inverse 8×8 DCT the format uses (rows, then columns), in place.
// (The format's own constants: its π is 3.14159.)
#[allow(clippy::approx_constant)]
fn idct8x8(data: &mut [f32; 64], zeroed_rows: usize) {
    let a = 0.5 * (3.14159f32 / 4.0).cos();
    let b = 0.5 * (3.14159f32 / 16.0).cos();
    let c = 0.5 * (3.14159f32 / 8.0).cos();
    let d = 0.5 * (3.0 * 3.14159f32 / 16.0).cos();
    let e = 0.5 * (5.0 * 3.14159f32 / 16.0).cos();
    let f = 0.5 * (3.0 * 3.14159f32 / 8.0).cos();
    let g = 0.5 * (7.0 * 3.14159f32 / 16.0).cos();
    let pass = |x: [f32; 8]| -> [f32; 8] {
        let alpha = [c * x[2], f * x[2], c * x[6], f * x[6]];
        let beta = [
            b * x[1] + d * x[3] + e * x[5] + g * x[7],
            d * x[1] - g * x[3] - b * x[5] - e * x[7],
            e * x[1] - b * x[3] + g * x[5] + d * x[7],
            g * x[1] - e * x[3] + d * x[5] - b * x[7],
        ];
        let theta = [a * (x[0] + x[4]), alpha[0] + alpha[3], alpha[1] - alpha[2], a * (x[0] - x[4])];
        let gamma = [theta[0] + theta[1], theta[3] + theta[2], theta[3] - theta[2], theta[0] - theta[1]];
        [
            gamma[0] + beta[0],
            gamma[1] + beta[1],
            gamma[2] + beta[2],
            gamma[3] + beta[3],
            gamma[3] - beta[3],
            gamma[2] - beta[2],
            gamma[1] - beta[1],
            gamma[0] - beta[0],
        ]
    };
    for r in data.chunks_exact_mut(8).take(8 - zeroed_rows.min(8)) {
        let mut x = [0f32; 8];
        x.copy_from_slice(r);
        r.copy_from_slice(&pass(x));
    }
    for col in 0..8 {
        let x: [f32; 8] = std::array::from_fn(|i| data.get(i * 8 + col).copied().unwrap_or(0.0));
        for (o, v) in data.iter_mut().skip(col).step_by(8).zip(pass(x)) {
            *o = v;
        }
    }
}

/// Float to half, rounding to nearest even (as the reference library does).
pub fn f32_to_half(f: f32) -> u16 {
    let bits = f.to_bits();
    let ui = bits & 0x7fff_ffff;
    let sign = ((bits >> 16) & 0x8000) as u16;
    if ui >= 0x3880_0000 {
        if ui >= 0x7f80_0000 {
            if ui == 0x7f80_0000 {
                return sign | 0x7c00;
            }
            let m = ((ui & 0x7f_ffff) >> 13) as u16;
            return sign | 0x7c00 | m | (m == 0) as u16;
        }
        if ui > 0x477f_efff {
            return sign | 0x7c00;
        }
        let u = ui - 0x3800_0000;
        return sign | ((u + 0xfff + ((u >> 13) & 1)) >> 13) as u16;
    }
    if ui < 0x3300_0001 {
        return sign;
    }
    let e = ui >> 23;
    let shift = 0x7e - e;
    let m = 0x80_0000 | (ui & 0x7f_ffff);
    let r = m << (32 - shift);
    let mut ret = sign | (m >> shift) as u16;
    if r > 0x8000_0000 || (r == 0x8000_0000 && ret & 1 != 0) {
        ret += 1;
    }
    ret
}

/// The curve DWA stores colour through (gamma 2.2 below 1, logarithmic
/// above), as a table from stored half to linear half.
struct Tables {
    to_linear: Box<[u16; 65536]>,
}

impl Tables {
    fn get() -> &'static Tables {
        static T: std::sync::OnceLock<Tables> = std::sync::OnceLock::new();
        T.get_or_init(|| {
            let mut t = Box::new([0u16; 65536]);
            for (i, o) in t.iter_mut().enumerate() {
                let x = i as u16;
                if x == 0 || x & 0x7c00 == 0x7c00 {
                    continue;
                }
                let f = half_to_f32(x);
                let sign = if f < 0.0 { -1.0f32 } else { 1.0 };
                let f = f.abs();
                let z = if f <= 1.0 { f.powf(2.2) } else { 9.025_013_f32.powf(f - 1.0) };
                *o = f32_to_half(sign * z);
            }
            Tables { to_linear: t }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn float_to_half_rounds_to_nearest_even() {
        for h in [0u16, 1, 0x3c00, 0x7bff, 0x8001, 0x0400, 0x03ff, 0xc000] {
            assert_eq!(f32_to_half(half_to_f32(h)), h);
        }
        assert_eq!(f32_to_half(1e6), 0x7c00);
        assert_eq!(f32_to_half(1.0 + 1.0 / 2048.0), 0x3c00); // a tie: to even
        assert_eq!(f32_to_half(1.0 + 3.0 / 2048.0), 0x3c02);
    }

    #[test]
    fn a_dc_only_block_is_flat() {
        let mut d = [0f32; 64];
        d[0] = 8.0;
        idct8x8(&mut d, 7);
        assert!(d.iter().all(|v| (v - 1.0).abs() < 1e-3), "{d:?}");
    }
}
