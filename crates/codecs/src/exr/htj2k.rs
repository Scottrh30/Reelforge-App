//! OpenEXR's HTJ2K compressions (OpenEXR 3.4/3.5): HTJ2K256 and HTJ2K32 (lossless, chunks of
//! 256 or 32 lines) and LJ2K (lossy, 256 lines). Each chunk is a small header and a
//! High-Throughput JPEG 2000 codestream (ITU-T T.814), decoded by the `htj2k` crate (a safe
//! port of OpenJPH, the coder OpenEXR uses).
//!
//! The header (big-endian): the magic "HT" (or "HL" for LJ2K with its transfer function in
//! the codestream), the length of the rest, the number of channels and, for each codestream
//! component, the file channel it is. Components are 16- or 32-bit samples whose bits are
//! the half, float or uint value. A lossy chunk's codestream may be padded a few rows and
//! columns beyond the chunk (to suit the wavelet); those are dropped.

use super::{fail, ChannelLayout, Result};

const MAGIC_HT: u16 = u16::from_be_bytes(*b"HT");
const MAGIC_HL: u16 = u16::from_be_bytes(*b"HL");
/// Most a chunk's codestream may make the decoder allocate.
const MEMORY: u64 = 1 << 30;

/// One chunk (`size` bytes uncompressed, in `layout`); `lossy`: LJ2K.
pub(super) fn decompress(data: &[u8], layout: &ChannelLayout, lossy: bool, size: usize) -> Result<Vec<u8>> {
    let be16 = |at: usize| data.get(at..at + 2).and_then(|b| b.try_into().ok()).map(u16::from_be_bytes);
    let (Some(magic), Some(len)) = (be16(0), data.get(2..6).and_then(|b| b.try_into().ok()).map(|b| u32::from_be_bytes(b) as usize)) else { return fail("HTJ2K chunk header") };
    match magic {
        MAGIC_HT | MAGIC_HL if lossy => {}
        MAGIC_HT => {}
        _ => return fail("HTJ2K chunk magic"),
    }
    let Some(start) = len.checked_add(6).filter(|&s| s <= data.len()) else { return fail("HTJ2K chunk header length") };
    let nch = be16(6).unwrap_or(0) as usize;
    if nch != layout.channels.len() || 8 + nch * 2 > start {
        return fail("HTJ2K channel map");
    }
    // Codestream component -> file channel (each file channel once).
    let mut to_file = Vec::with_capacity(nch);
    let mut seen = vec![false; nch];
    for i in 0..nch {
        let f = be16(8 + i * 2).unwrap_or(u16::MAX) as usize;
        match seen.get_mut(f) {
            Some(s) if !*s => *s = true,
            _ => return fail("HTJ2K channel map"),
        }
        to_file.push(f);
    }
    let Some(codestream) = data.get(start..).filter(|c| !c.is_empty()) else { return fail("HTJ2K codestream") };

    let mut cs = htj2k::codestream::Codestream::new();
    cs.set_memory_limit(MEMORY);
    let err = |e: htj2k::Error| super::Error::Invalid(format!("EXR: HTJ2K: {e}"));
    cs.read_headers(codestream).map_err(err)?;
    let siz = cs.access_siz();
    if siz.get_num_components() as usize != nch {
        return fail("HTJ2K component count");
    }
    let extent = siz.get_image_extent();
    let offset = siz.get_image_offset();
    let (iw, ih) = (extent.x.saturating_sub(offset.x) as usize, extent.y.saturating_sub(offset.y) as usize);
    // Each component's size, and the file channel's.
    let mut dims = Vec::with_capacity(nch);
    for (c, &f) in to_file.iter().enumerate() {
        let Some(ch) = layout.channels.get(f) else { return fail("HTJ2K channel map") };
        let (w, h) = (siz.get_recon_width(c as u32) as usize, siz.get_recon_height(c as u32) as usize);
        let fits = (w == ch.nx && h == ch.ny) || (lossy && w >= ch.nx && h >= ch.ny && w <= ch.nx + 32 && h <= ch.ny + 32);
        if !fits || iw < ch.nx || ih < ch.ny {
            return fail("HTJ2K component size");
        }
        dims.push((w, h));
    }
    // All the samples, each line to its component (whichever order the codestream gives
    // them in).
    // LJ2K as OpenEXR 3.5.0 and 3.5.1 wrote it ("HT"): the transfer function was applied
    // to the lossy components before coding, so it's undone here (later files carry it
    // in the codestream, where the decoder undoes it).
    let legacy: Vec<bool> = (0..nch).map(|c| lossy && magic == MAGIC_HT && !cs.access_cod().is_reversible_comp(c as u32)).collect();
    cs.create().map_err(err)?;
    let mut planes: Vec<Vec<i32>> = dims.iter().map(|&(w, h)| Vec::with_capacity(w.saturating_mul(h).min(1 << 26))).collect();
    while let Some((comp, line)) = cs.pull().map_err(err)? {
        let Some(&(w, h)) = dims.get(comp as usize) else { return fail("HTJ2K component") };
        let Some(p) = planes.get_mut(comp as usize) else { return fail("HTJ2K component") };
        if p.len() >= w * h {
            return fail("HTJ2K too many lines");
        }
        let Some(samples) = line.i32().get(..w) else { return fail("HTJ2K line") };
        p.extend_from_slice(samples);
    }

    // Each file channel's samples, cropped to the chunk.
    let mut by_file: Vec<Option<(&[i32], usize, bool)>> = vec![None; nch];
    for (((&f, &(w, h)), p), &old) in to_file.iter().zip(&dims).zip(&planes).zip(&legacy) {
        if p.len() != w * h {
            return fail("HTJ2K component cut short");
        }
        if let Some(slot) = by_file.get_mut(f) {
            *slot = Some((p, w, old));
        }
    }
    let mut out = Vec::with_capacity(size);
    // Rows of each file channel used so far.
    let mut rows = vec![0usize; nch];
    for line in 0..layout.lines {
        let y = layout.y0 as i64 + line as i64;
        for (f, ch) in layout.channels.iter().enumerate() {
            if !(ch.ys <= 1 || y % ch.ys as i64 == 0) || ch.ny == 0 {
                continue;
            }
            let (Some(Some((samples, w, old))), Some(row)) = (by_file.get(f).copied(), rows.get_mut(f)) else { return fail("HTJ2K channel") };
            let Some(src) = samples.get(*row * w..*row * w + ch.nx) else { return fail("HTJ2K channel rows") };
            *row += 1;
            if old && ch.units == 1 {
                for &s in src {
                    out.extend_from_slice(&legacy_half(s).to_le_bytes());
                }
            } else if old {
                for &s in src {
                    out.extend_from_slice(&legacy_float(s).to_bits().to_le_bytes());
                }
            } else if ch.units == 1 {
                for &s in src {
                    out.extend_from_slice(&(s as u16).to_le_bytes());
                }
            } else {
                for &s in src {
                    out.extend_from_slice(&(s as u32).to_le_bytes());
                }
            }
        }
    }
    if out.len() != size {
        return fail("HTJ2K chunk size");
    }
    Ok(out)
}

/// OpenEXR's transfer function, back to linear: a 2.2 power up to 1, then exponential
/// (its constants are single-precision literals worked in double, as there).
fn tf_to_linear(x: f64) -> f64 {
    let k = 2.2f32 as f64;
    let v = x.abs();
    let lin = if v <= 1.0 { v.powf(k) } else { (k * (v - 1.0)).exp() };
    lin.copysign(x)
}

/// A legacy LJ2K 16-bit sample as a half (32 767 is the largest half after the transfer
/// function).
fn legacy_half(s: i32) -> u16 {
    const FACTOR: f64 = 5424.23808866629;
    super::f32_to_half(tf_to_linear(s.clamp(-32767, 32767) as f64 / FACTOR) as f32)
}

/// A legacy LJ2K 32-bit sample as a float.
fn legacy_float(s: i32) -> f32 {
    const FACTOR: f64 = 51961246.180338;
    tf_to_linear(s as f64 / FACTOR).clamp(-(f32::MAX as f64), f32::MAX as f64) as f32
}
