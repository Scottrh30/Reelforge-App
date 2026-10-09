//! YUV <-> RGB matrix and range handling for swscale.
//!
//! swscale defaults to BT.601 limited range in both directions. Real footage is
//! mostly BT.709 (HD) and sometimes full range, so without this HD colours are
//! slightly off (greens/reds shift) and full-range sources look washed out.

use ffmpeg_next as ff;
use ff::util::color::{Range, Space};
use ff::software::scaling::context::Context as Scaler;

/// The YUV matrix to assume for a stream: its tag if present, else by size
/// (the same heuristic players use: HD and up is BT.709).
pub fn effective_space(tagged: Space, height: u32) -> Space {
    match tagged {
        Space::BT709 | Space::BT470BG | Space::SMPTE170M | Space::SMPTE240M | Space::FCC => tagged,
        Space::BT2020NCL | Space::BT2020CL => tagged,
        _ if height >= 720 => Space::BT709,
        _ => Space::SMPTE170M,
    }
}

// The constants' integer type differs between bindgen versions/platforms, hence the casts.
#[allow(clippy::unnecessary_cast)]
fn sws_cs(space: Space) -> i32 {
    match space {
        Space::BT709 => ff::ffi::SWS_CS_ITU709 as i32,
        Space::FCC => ff::ffi::SWS_CS_FCC as i32,
        Space::SMPTE240M => ff::ffi::SWS_CS_SMPTE240M as i32,
        Space::BT2020NCL | Space::BT2020CL => ff::ffi::SWS_CS_BT2020 as i32,
        _ => ff::ffi::SWS_CS_ITU601 as i32,
    }
}

/// Configure `scaler` for YUV `space`/`range` on its YUV side; the RGB side is full range.
/// `yuv_is_source`: true when decoding (YUV -> RGB), false when encoding (RGB -> YUV).
pub fn configure(scaler: &mut Scaler, space: Space, range: Range, yuv_is_source: bool) {
    let yuv_full = matches!(range, Range::JPEG) as i32;
    unsafe {
        let table = ff::ffi::sws_getCoefficients(sws_cs(space));
        let (src_range, dst_range) = if yuv_is_source { (yuv_full, 1) } else { (1, yuv_full) };
        // brightness 0, contrast 1.0, saturation 1.0 (16.16 fixed point)
        ff::ffi::sws_setColorspaceDetails(scaler.as_mut_ptr(), table, src_range, table, dst_range, 0, 1 << 16, 1 << 16);
    }
}
