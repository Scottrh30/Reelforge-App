//! DCT coefficient tokens (RFC 6386 section 13) and dequantisation (section 14.1).

use crate::boolcoder::BoolDecoder;

pub const ZIGZAG: [usize; 16] = [0, 1, 4, 8, 5, 2, 3, 6, 9, 12, 13, 10, 7, 11, 14, 15];
const BANDS: [usize; 17] = [0, 1, 2, 3, 6, 4, 5, 6, 6, 6, 6, 6, 6, 6, 6, 7, 0];

const CAT3: [u8; 3] = [173, 148, 140];
const CAT4: [u8; 4] = [176, 155, 140, 135];
const CAT5: [u8; 5] = [180, 157, 141, 134, 130];
const CAT6: [u8; 11] = [254, 254, 243, 230, 196, 177, 153, 140, 133, 130, 129];

/// Block types (the first index of the coefficient probabilities).
pub const TYPE_Y_AFTER_Y2: usize = 0;
pub const TYPE_Y2: usize = 1;
pub const TYPE_UV: usize = 2;
pub const TYPE_Y_WITH_DC: usize = 3;

fn extra(bd: &mut BoolDecoder, probs: &[u8]) -> i32 {
    let mut v = 0;
    for &p in probs {
        v = (v << 1) | i32::from(bd.read(p));
    }
    v
}

/// Decode one block's tokens into `out` (raster order, dequantised by `dq` = [dc, ac] as 16-bit
/// products, as the reference decoder stores them). `ctx` is the number of the above and left
/// blocks with coefficients. Returns the end position (the index after the last token), which
/// is `first` for an empty block.
pub fn block(bd: &mut BoolDecoder, probs: &[[[u8; 11]; 3]; 8], first: usize, ctx: usize, dq: [i16; 2], out: &mut [i16; 16]) -> usize {
    let mut i = first;
    let mut ctx = ctx.min(2);
    let mut after_zero = false;
    while i < 16 {
        let p = &probs[BANDS[i]][ctx];
        if !after_zero && !bd.read(p[0]) {
            return i; // end of block
        }
        if !bd.read(p[1]) {
            ctx = 0;
            after_zero = true;
            i += 1;
            continue;
        }
        after_zero = false;
        let v: i32 = if !bd.read(p[2]) {
            ctx = 1;
            1
        } else {
            ctx = 2;
            if !bd.read(p[3]) {
                if !bd.read(p[4]) { 2 } else { 3 + i32::from(bd.read(p[5])) }
            } else if !bd.read(p[6]) {
                if !bd.read(p[7]) { 5 + i32::from(bd.read(159)) } else { 7 + extra(bd, &[165, 145]) }
            } else if !bd.read(p[8]) {
                if !bd.read(p[9]) { 11 + extra(bd, &CAT3) } else { 19 + extra(bd, &CAT4) }
            } else if !bd.read(p[10]) {
                35 + extra(bd, &CAT5)
            } else {
                67 + extra(bd, &CAT6)
            }
        };
        let v = if bd.bit() { -v } else { v };
        let q = if i == 0 { dq[0] } else { dq[1] };
        out[ZIGZAG[i]] = (v as i16).wrapping_mul(q);
        i += 1;
    }
    16
}

/// Quantiser index to the six dequantisation factors (RFC 6386 section 14.1).
#[derive(Clone, Copy, Default, Debug)]
pub struct Dequant {
    pub y1: [i16; 2],
    pub y2: [i16; 2],
    pub uv: [i16; 2],
}

impl Dequant {
    pub fn new(q: i32, y1_dc: i32, y2_dc: i32, y2_ac: i32, uv_dc: i32, uv_ac: i32) -> Self {
        let dc = |d: i32| i32::from(crate::tables::DC_Q[(q + d).clamp(0, 127) as usize]);
        let ac = |d: i32| i32::from(crate::tables::AC_Q[(q + d).clamp(0, 127) as usize]);
        let y2ac = ((ac(y2_ac) * 101581) >> 16).max(8);
        Dequant {
            y1: [dc(y1_dc) as i16, ac(0) as i16],
            y2: [(dc(y2_dc) * 2) as i16, y2ac as i16],
            uv: [dc(uv_dc).min(132) as i16, ac(uv_ac) as i16],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dequant_matches_the_standard_at_the_ends() {
        let d = Dequant::new(0, 0, 0, 0, 0, 0);
        assert_eq!((d.y1, d.y2, d.uv), ([4, 4], [8, 8], [4, 4]));
        let d = Dequant::new(127, 0, 0, 0, 0, 0);
        assert_eq!((d.y1, d.y2, d.uv), ([157, 284], [314, 440], [132, 284]));
        // Out-of-range deltas clamp the index.
        let d = Dequant::new(120, 15, -15, 15, 15, -15);
        assert_eq!(d.y1[0], 157);
    }

    #[test]
    fn empty_input_gives_empty_blocks() {
        let probs = crate::tables::COEF_PROBS[0];
        let mut bd = BoolDecoder::new(&[]);
        let mut out = [0i16; 16];
        // All-zero input reads 0 for every bool: end of block straight away.
        assert_eq!(block(&mut bd, &probs, 1, 0, [1, 1], &mut out), 1);
        assert!(out.iter().all(|&c| c == 0));
    }
}
