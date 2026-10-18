//! Motion JPEG: every packet is a JPEG picture (or, for interlaced video,
//! two: one per field). The JPEG decoding itself is zune-jpeg (pure Rust),
//! which also fills in the standard Huffman tables that AVI-style MJPEG
//! frames leave out.

use crate::{invalid, Decoder, Error, Format, Matrix, Output, Picture, Result, StreamInfo};
use zune_core::bytestream::ZCursor;
use zune_core::colorspace::ColorSpace;
use zune_core::options::DecoderOptions;
use zune_jpeg::JpegDecoder;

pub struct MjpegDecoder {
    /// The stream's picture height (to tell a field from a whole frame).
    height: u32,
    out: Output,
}

impl MjpegDecoder {
    pub fn new(info: &StreamInfo) -> Self {
        MjpegDecoder { height: info.height, out: Output::default() }
    }
}

/// The JPEG images in a packet: from each SOI marker to its EOI.
fn images(data: &[u8]) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 1 < data.len() {
        if data[i] == 0xFF && data[i + 1] == 0xD8 {
            // Find the EOI, stepping over marker segments so a 0xFFD9 inside one isn't taken for it.
            let start = i;
            let mut j = i + 2;
            let mut end = data.len();
            while j + 1 < data.len() {
                if data[j] != 0xFF {
                    j += 1;
                    continue;
                }
                let m = data[j + 1];
                if m == 0xD9 {
                    end = j + 2;
                    break;
                }
                if m == 0x00 || m == 0xFF || (0xD0..=0xD7).contains(&m) || m == 0x01 {
                    j += if m == 0xFF { 1 } else { 2 };
                    continue;
                }
                // A segment with a length.
                if j + 3 >= data.len() {
                    break;
                }
                let len = u16::from_be_bytes([data[j + 2], data[j + 3]]) as usize;
                j += 2 + len;
            }
            out.push(&data[start..end.min(data.len())]);
            i = end;
        } else {
            i += 1;
        }
    }
    out
}

/// One JPEG to planar YUV (or gray) at its own size.
fn decode_one(jpeg: &[u8]) -> Result<Picture> {
    let opts = DecoderOptions::default().set_strict_mode(false).jpeg_set_out_colorspace(ColorSpace::YCbCr);
    let mut d = JpegDecoder::new_with_options(ZCursor::new(jpeg), opts);
    d.decode_headers().map_err(|e| Error::Invalid(format!("JPEG: {e:?}")))?;
    let info = d.info().ok_or_else(|| Error::Invalid("JPEG without a frame header".into()))?;
    let (w, h) = (info.width as u32, info.height as u32);
    let gray = info.components == 1;
    if gray {
        d.set_options(DecoderOptions::default().set_strict_mode(false).jpeg_set_out_colorspace(ColorSpace::Luma));
    } else if info.components != 3 {
        return Err(Error::Unsupported(format!("JPEG with {} components", info.components)));
    }
    let px = d.decode().map_err(|e| Error::Invalid(format!("JPEG: {e:?}")))?;
    if gray {
        let mut p = Picture::new(w, h, Format::Gray8);
        p.planes[0].data.copy_from_slice(&px[..(w * h) as usize]);
        p.full_range = true;
        return Ok(p);
    }
    if px.len() < (w * h * 3) as usize {
        return invalid("JPEG decoded short");
    }
    // Interleaved YCbCr 4:4:4 to planes.
    let mut p = Picture::new(w, h, Format::Yuv444P8);
    let n = (w * h) as usize;
    let (y, rest) = p.planes.split_at_mut(1);
    let (cb, cr) = rest.split_at_mut(1);
    for (i, s) in px.chunks_exact(3).take(n).enumerate() {
        y[0].data[i] = s[0];
        cb[0].data[i] = s[1];
        cr[0].data[i] = s[2];
    }
    p.full_range = true;
    p.matrix = Matrix::Bt601;
    Ok(p)
}

/// Weave two fields (top first) into one frame.
fn weave(top: &Picture, bottom: &Picture) -> Picture {
    let (w, h) = (top.width, top.height + bottom.height);
    let mut p = Picture::new(w, h, top.format);
    for pl in 0..top.format.planes() {
        let (pw, _) = top.format.plane_size(pl, w, h);
        let row = pw as usize * top.format.bytes_per_sample();
        let (_, ph) = top.format.plane_size(pl, w, h);
        for line in 0..ph as usize {
            let (src, sy) = if line % 2 == 0 { (top, line / 2) } else { (bottom, line / 2) };
            let s = &src.planes[pl];
            if sy * s.stride + row > s.data.len() {
                continue;
            }
            let d = p.planes[pl].stride;
            p.planes[pl].data[line * d..line * d + row].copy_from_slice(&s.data[sy * s.stride..sy * s.stride + row]);
        }
    }
    p.full_range = top.full_range;
    p.matrix = top.matrix;
    p.interlaced = true;
    p
}

impl Decoder for MjpegDecoder {
    fn send(&mut self, packet: &[u8], pts: i64) -> Result<()> {
        let imgs = images(packet);
        let mut pics = Vec::new();
        for img in imgs.iter().take(2) {
            pics.push(decode_one(img)?);
        }
        let mut pic = match pics.len() {
            0 => return invalid("no JPEG picture in the packet"),
            2 if pics[0].format == pics[1].format && pics[0].width == pics[1].width && pics[0].height + pics[1].height <= self.height.max(1) + 1 => {
                weave(&pics[0], &pics[1])
            }
            _ => pics.swap_remove(0),
        };
        pic.pts = pts;
        self.out.push(pic);
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
    fn finds_both_fields_in_a_packet() {
        let a = [0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x04, 0xFF, 0xD9, 0x11, 0xFF, 0xD9];
        let mut two = a.to_vec();
        two.extend_from_slice(&[0xFF, 0xD8, 1, 2, 0xFF, 0xD9]);
        let found = images(&two);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0], &a[..], "the 0xFFD9 inside the APP0 segment is not the end");
        assert_eq!(found[1], &[0xFF, 0xD8, 1, 2, 0xFF, 0xD9]);
    }
}
