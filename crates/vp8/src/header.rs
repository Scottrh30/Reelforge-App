//! The frame tag, key frame start code and first-partition frame header (RFC 6386 sections 9
//! and 19.2).

use crate::Error;
use crate::boolcoder::BoolDecoder;
use crate::tables::{COEF_UPDATE_PROBS, MV_UPDATE_PROBS};

/// The uncompressed part at the start of every frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameTag {
    pub key: bool,
    /// 0: 6-tap filters; 1, 2: bilinear; 3: bilinear with whole-pixel chroma.
    pub version: u8,
    pub show: bool,
    pub first_part_size: usize,
    /// Key frames only.
    pub width: u16,
    pub height: u16,
    /// Bytes before the first partition (3, or 10 on key frames).
    pub header_len: usize,
}

pub fn frame_tag(data: &[u8]) -> Result<FrameTag, Error> {
    let [b0, b1, b2, ..] = *data else { return Err(Error::Invalid("frame shorter than its tag")) };
    let raw = u32::from(b0) | u32::from(b1) << 8 | u32::from(b2) << 16;
    let key = raw & 1 == 0;
    let version = ((raw >> 1) & 7) as u8;
    let show = (raw >> 4) & 1 != 0;
    let first_part_size = (raw >> 5) as usize;
    let (mut width, mut height, mut header_len) = (0, 0, 3);
    if key {
        let Some(h) = data.get(3..10) else { return Err(Error::Invalid("key frame header cut short")) };
        if h[..3] != [0x9d, 0x01, 0x2a] {
            return Err(Error::Invalid("key frame start code missing"));
        }
        // The top two bits of each are an upscaling hint for display; the decode ignores them.
        width = u16::from_le_bytes([h[3], h[4]]) & 0x3fff;
        height = u16::from_le_bytes([h[5], h[6]]) & 0x3fff;
        header_len = 10;
        if width == 0 || height == 0 {
            return Err(Error::Invalid("zero frame size"));
        }
    }
    if version > 3 {
        return Err(Error::Unsupported("VP8 version above 3"));
    }
    Ok(FrameTag { key, version, show, first_part_size, width, height, header_len })
}

/// Entropy coding probabilities that persist from frame to frame.
#[derive(Clone)]
pub struct Probs {
    pub coef: [[[[u8; 11]; 3]; 8]; 4],
    pub ymode: [u8; 4],
    pub uv_mode: [u8; 3],
    pub mv: [[u8; 19]; 2],
}

impl Default for Probs {
    fn default() -> Self {
        Probs { coef: crate::tables::COEF_PROBS, ymode: crate::tables::YMODE_PROB, uv_mode: crate::tables::UV_MODE_PROB, mv: crate::tables::MV_DEFAULT_PROBS }
    }
}

/// Segmentation and loop filter adjustments: persistent until changed.
#[derive(Clone, Default)]
pub struct Persist {
    pub seg_abs: bool,
    pub seg_quant: [i32; 4],
    pub seg_lf: [i32; 4],
    pub ref_lf_delta: [i32; 4],
    pub mode_lf_delta: [i32; 4],
}

/// What the first partition's header says about this frame.
#[derive(Clone, Debug, Default)]
pub struct Header {
    pub segmentation: bool,
    pub update_seg_map: bool,
    pub seg_tree_probs: [u8; 3],
    pub simple_filter: bool,
    pub filter_level: i32,
    pub sharpness: i32,
    pub lf_delta: bool,
    pub partitions: usize,
    pub q_index: i32,
    pub y1_dc: i32,
    pub y2_dc: i32,
    pub y2_ac: i32,
    pub uv_dc: i32,
    pub uv_ac: i32,
    pub refresh_golden: bool,
    pub refresh_alt: bool,
    pub copy_to_golden: u8,
    pub copy_to_alt: u8,
    pub sign_bias_golden: bool,
    pub sign_bias_alt: bool,
    pub refresh_probs: bool,
    pub refresh_last: bool,
    pub skip_enabled: bool,
    pub prob_skip: u8,
    pub prob_intra: u8,
    pub prob_last: u8,
    pub prob_golden: u8,
}

/// Read the frame header from the first partition. Updates `probs` and `persist`; when the
/// frame keeps its probability changes to itself (`refresh_probs` clear), `saved` receives the
/// probabilities to restore after it.
pub fn read(bd: &mut BoolDecoder, key: bool, probs: &mut Probs, persist: &mut Persist, saved: &mut Option<Probs>) -> Header {
    let mut h = Header::default();
    if key {
        // Colour space (0: YCbCr BT.601) and clamping type: the decode is the same for both.
        bd.bit();
        bd.bit();
    }
    h.segmentation = bd.bit();
    if h.segmentation {
        h.update_seg_map = bd.bit();
        let update_data = bd.bit();
        if update_data {
            persist.seg_abs = bd.bit();
            for q in &mut persist.seg_quant {
                *q = bd.opt_signed(7);
            }
            for l in &mut persist.seg_lf {
                *l = bd.opt_signed(6);
            }
        }
        if h.update_seg_map {
            for p in &mut h.seg_tree_probs {
                *p = if bd.bit() { bd.literal(8) as u8 } else { 255 };
            }
        }
    }
    h.simple_filter = bd.bit();
    h.filter_level = bd.literal(6) as i32;
    h.sharpness = bd.literal(3) as i32;
    h.lf_delta = bd.bit();
    if h.lf_delta && bd.bit() {
        for d in persist.ref_lf_delta.iter_mut().chain(persist.mode_lf_delta.iter_mut()) {
            if bd.bit() {
                *d = bd.signed(6);
            }
        }
    }
    h.partitions = 1 << bd.literal(2);
    h.q_index = bd.literal(7) as i32;
    h.y1_dc = bd.opt_signed(4);
    h.y2_dc = bd.opt_signed(4);
    h.y2_ac = bd.opt_signed(4);
    h.uv_dc = bd.opt_signed(4);
    h.uv_ac = bd.opt_signed(4);
    if key {
        h.refresh_golden = true;
        h.refresh_alt = true;
    } else {
        h.refresh_golden = bd.bit();
        h.refresh_alt = bd.bit();
        if !h.refresh_golden {
            h.copy_to_golden = bd.literal(2) as u8;
        }
        if !h.refresh_alt {
            h.copy_to_alt = bd.literal(2) as u8;
        }
        h.sign_bias_golden = bd.bit();
        h.sign_bias_alt = bd.bit();
    }
    h.refresh_probs = bd.bit();
    if !h.refresh_probs {
        *saved = Some(probs.clone());
    }
    h.refresh_last = key || bd.bit();
    for (i, plane) in probs.coef.iter_mut().enumerate() {
        for (j, band) in plane.iter_mut().enumerate() {
            for (k, ctx) in band.iter_mut().enumerate() {
                for (l, p) in ctx.iter_mut().enumerate() {
                    if bd.read(COEF_UPDATE_PROBS[i][j][k][l]) {
                        *p = bd.literal(8) as u8;
                    }
                }
            }
        }
    }
    h.skip_enabled = bd.bit();
    if h.skip_enabled {
        h.prob_skip = bd.literal(8) as u8;
    }
    if !key {
        h.prob_intra = bd.literal(8) as u8;
        h.prob_last = bd.literal(8) as u8;
        h.prob_golden = bd.literal(8) as u8;
        if bd.bit() {
            for p in &mut probs.ymode {
                *p = bd.literal(8) as u8;
            }
        }
        if bd.bit() {
            for p in &mut probs.uv_mode {
                *p = bd.literal(8) as u8;
            }
        }
        for (comp, up) in probs.mv.iter_mut().zip(MV_UPDATE_PROBS.iter()) {
            for (p, &u) in comp.iter_mut().zip(up.iter()) {
                if bd.read(u) {
                    let x = bd.literal(7) as u8;
                    *p = if x != 0 { x << 1 } else { 1 };
                }
            }
        }
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_parse_and_damage_is_an_error() {
        // Key frame, version 0, shown, partition size 0x123; 64x48.
        let raw: u32 = (0x123 << 5) | (1 << 4);
        let mut d = vec![raw as u8, (raw >> 8) as u8, (raw >> 16) as u8, 0x9d, 0x01, 0x2a, 64, 0, 48, 0];
        let t = frame_tag(&d).unwrap();
        assert!(t.key && t.show && t.version == 0);
        assert_eq!((t.width, t.height, t.first_part_size, t.header_len), (64, 48, 0x123, 10));
        d[4] = 0;
        assert!(frame_tag(&d).is_err());
        assert!(frame_tag(&d[..2]).is_err());
        assert!(frame_tag(&d[..7]).is_err());
        // Inter frame.
        let t = frame_tag(&[1 | (5 << 5), 0, 0]).unwrap();
        assert!(!t.key && t.first_part_size == 5 && t.header_len == 3);
    }
}
