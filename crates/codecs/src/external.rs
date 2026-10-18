//! H.264, HEVC, VP9 and AV1: pure-Rust decoder libraries behind Reelforge's
//! [`Decoder`] interface. Pictures they can't express in a Reelforge
//! [`Format`] (RGB VP9, for instance) are reported as unsupported, which makes
//! the media crate switch that file to FFmpeg's decoder.

use crate::{Decoder, Error, Format, Matrix, Output, Picture, Result, StreamInfo};

/// Samples of one decoded plane, 8-bit or wider.
enum Samples<'a> {
    U8(&'a [u8]),
    U16(&'a [u16]),
}

/// One plane as a library gives it: samples, row stride in samples.
struct Src<'a> {
    samples: Samples<'a>,
    stride: usize,
}

/// Colour matrix from an ITU-T H.273 `matrix_coefficients` code point.
fn h273_matrix(code: u8) -> Matrix {
    match code {
        1 => Matrix::Bt709,
        5 | 6 => Matrix::Bt601,
        9 | 10 => Matrix::Bt2020,
        _ => Matrix::Unknown,
    }
}

/// Build a Reelforge picture from library planes. `chroma` is (x shift, y shift),
/// or None for monochrome (neutral chroma is filled in, as 4:2:0).
#[allow(clippy::too_many_arguments)]
fn picture(width: u32, height: u32, depth: u32, chroma: Option<(u32, u32)>, planes: [Option<Src>; 3], matrix: Matrix, full_range: bool, pts: i64) -> Result<Picture> {
    let (sx, sy) = chroma.unwrap_or((1, 1));
    let format = match (depth, sx, sy) {
        (8, 1, 1) => Format::Yuv420P8,
        (8, 1, 0) => Format::Yuv422P8,
        (8, 0, 0) => Format::Yuv444P8,
        (10, 1, 1) => Format::Yuv420P10,
        (10, 1, 0) => Format::Yuv422P10,
        (10, 0, 0) => Format::Yuv444P10,
        (12, 1, 1) => Format::Yuv420P12,
        (12, 1, 0) => Format::Yuv422P12,
        (12, 0, 0) => Format::Yuv444P12,
        _ => return Err(Error::Unsupported(format!("{depth}-bit pictures with chroma subsampling {sx},{sy}"))),
    };
    let mut out = Picture::new(width, height, format);
    out.matrix = matrix;
    out.full_range = full_range;
    out.pts = pts;
    let wide = format.bytes_per_sample() == 2;
    for (i, src) in planes.into_iter().enumerate() {
        let (pw, ph) = format.plane_size(i, width, height);
        let (pw, ph) = (pw as usize, ph as usize);
        let dst = &mut out.planes[i];
        let Some(src) = src else {
            // Monochrome: mid-grey chroma.
            let mid = 1u16 << (depth - 1);
            if wide {
                for c in dst.data.chunks_exact_mut(2) {
                    c.copy_from_slice(&mid.to_le_bytes());
                }
            } else {
                dst.data.fill(mid as u8);
            }
            continue;
        };
        let need = |len: usize| -> Result<()> {
            if ph > 0 && (src.stride < pw || len < (ph - 1) * src.stride + pw) {
                return Err(Error::Invalid(format!("plane {i} smaller than {pw}×{ph}")));
            }
            Ok(())
        };
        match (&src.samples, wide) {
            (Samples::U8(s), false) => {
                need(s.len())?;
                for y in 0..ph {
                    dst.data[y * dst.stride..y * dst.stride + pw].copy_from_slice(&s[y * src.stride..y * src.stride + pw]);
                }
            }
            (Samples::U16(s), true) => {
                need(s.len())?;
                for y in 0..ph {
                    let row = &mut dst.data[y * dst.stride..y * dst.stride + pw * 2];
                    for (d, v) in row.chunks_exact_mut(2).zip(&s[y * src.stride..y * src.stride + pw]) {
                        d.copy_from_slice(&v.to_le_bytes());
                    }
                }
            }
            _ => return Err(Error::Invalid(format!("{depth}-bit picture with the wrong sample size"))),
        }
    }
    Ok(out)
}

/// Is `data` an MP4-style configuration record (avcC / hvcC) rather than Annex-B parameter sets?
fn is_config_record(data: &[u8]) -> bool {
    data.first() == Some(&1)
}

// ---- H.264 --------------------------------------------------------------------------

pub struct H264Decoder {
    dec: filmcraft_h264::Decoder,
    extradata: Vec<u8>,
    out: Output,
}

impl H264Decoder {
    pub fn new(info: &StreamInfo) -> Self {
        let mut d = H264Decoder { dec: filmcraft_h264::Decoder::new(), extradata: info.extradata.clone(), out: Output::default() };
        d.configure();
        d
    }

    fn configure(&mut self) {
        self.dec = filmcraft_h264::Decoder::new();
        if self.extradata.is_empty() {
            return;
        }
        // An avcC record (MP4/MOV/MKV) or Annex-B parameter sets (some MKV and raw streams).
        let r = if is_config_record(&self.extradata) { self.dec.configure_avcc(&self.extradata) } else { self.dec.decode(&self.extradata, 0).map(drop) };
        if let Err(e) = r {
            log::warn!("H.264 setup data rejected: {e}");
        }
    }

    fn take(&mut self, pics: Vec<filmcraft_h264::Picture>) -> Result<()> {
        for p in pics {
            let c = p.color;
            let planes = [
                Some(Src { samples: Samples::U8(&p.y), stride: p.y_stride }),
                Some(Src { samples: Samples::U8(&p.u), stride: p.uv_stride }),
                Some(Src { samples: Samples::U8(&p.v), stride: p.uv_stride }),
            ];
            self.out.push(picture(p.width, p.height, 8, Some((1, 1)), planes, h273_matrix(c.matrix), c.full_range, p.pts)?);
        }
        Ok(())
    }
}

impl Decoder for H264Decoder {
    fn send(&mut self, packet: &[u8], pts: i64) -> Result<()> {
        let pics = self.dec.decode(packet, pts).map_err(|e| Error::Unsupported(format!("H.264: {e}")))?;
        self.take(pics)
    }
    fn receive(&mut self) -> Option<Picture> {
        self.out.pop()
    }
    fn flush(&mut self) {
        // Drops everything in flight; parameter sets and the thread pool stay.
        let _ = self.dec.flush();
        self.out.clear();
    }
    fn finish(&mut self) {
        let pics = self.dec.flush();
        if let Err(e) = self.take(pics) {
            log::warn!("H.264: {e}");
        }
    }
}

// ---- HEVC ---------------------------------------------------------------------------

fn hevc_samples(p: &filmcraft_hevc::Plane) -> Samples<'_> {
    match p {
        filmcraft_hevc::Plane::U8(v) => Samples::U8(v),
        filmcraft_hevc::Plane::U16(v) => Samples::U16(v),
    }
}

pub struct HevcDecoder {
    dec: filmcraft_hevc::Decoder,
    extradata: Vec<u8>,
    out: Output,
}

impl HevcDecoder {
    pub fn new(info: &StreamInfo) -> Self {
        let mut d = HevcDecoder { dec: filmcraft_hevc::Decoder::new(), extradata: info.extradata.clone(), out: Output::default() };
        d.configure();
        d
    }

    fn configure(&mut self) {
        self.dec = filmcraft_hevc::Decoder::new();
        if self.extradata.is_empty() {
            return;
        }
        let r = if is_config_record(&self.extradata) { self.dec.configure_hvcc(&self.extradata) } else { self.dec.decode(&self.extradata, 0).map(drop) };
        if let Err(e) = r {
            log::warn!("HEVC setup data rejected: {e}");
        }
    }

    fn take(&mut self, pics: Vec<filmcraft_hevc::Picture>) -> Result<()> {
        for p in pics {
            let c = p.color;
            // The library outputs 4:2:0; chroma plane size tells us otherwise if that changes.
            let sx = u32::from(p.chroma_width < p.width);
            let sy = u32::from(p.chroma_height < p.height);
            let planes = [
                Some(Src { samples: hevc_samples(&p.y), stride: p.y_stride }),
                Some(Src { samples: hevc_samples(&p.u), stride: p.uv_stride }),
                Some(Src { samples: hevc_samples(&p.v), stride: p.uv_stride }),
            ];
            self.out.push(picture(p.width, p.height, p.bit_depth, Some((sx, sy)), planes, h273_matrix(c.matrix), c.full_range, p.pts)?);
        }
        Ok(())
    }
}

impl Decoder for HevcDecoder {
    fn send(&mut self, packet: &[u8], pts: i64) -> Result<()> {
        let pics = self.dec.decode(packet, pts).map_err(|e| Error::Unsupported(format!("HEVC: {e}")))?;
        self.take(pics)
    }
    fn receive(&mut self) -> Option<Picture> {
        self.out.pop()
    }
    fn flush(&mut self) {
        // Drops everything in flight; parameter sets and the thread pool stay.
        let _ = self.dec.flush();
        self.out.clear();
    }
    fn finish(&mut self) {
        let pics = self.dec.flush();
        if let Err(e) = self.take(pics) {
            log::warn!("HEVC: {e}");
        }
    }
}

// ---- VP9 ----------------------------------------------------------------------------

fn vp9_samples(p: &filmcraft_vp9::Plane) -> Samples<'_> {
    match p {
        filmcraft_vp9::Plane::U8(v) => Samples::U8(v),
        filmcraft_vp9::Plane::U16(v) => Samples::U16(v),
    }
}

pub struct Vp9Decoder {
    dec: filmcraft_vp9::Decoder,
    out: Output,
}

impl Vp9Decoder {
    pub fn new(_info: &StreamInfo) -> Self {
        Vp9Decoder { dec: filmcraft_vp9::Decoder::new(), out: Output::default() }
    }

    fn take(&mut self, pics: Vec<filmcraft_vp9::Picture>) -> Result<()> {
        for p in pics {
            let matrix = match p.color.color_space {
                1 | 3 => Matrix::Bt601,
                2 => Matrix::Bt709,
                5 => Matrix::Bt2020,
                7 => return Err(Error::Unsupported("VP9 in RGB".into())),
                _ => Matrix::Unknown,
            };
            let planes = [
                Some(Src { samples: vp9_samples(&p.y), stride: p.y_stride }),
                Some(Src { samples: vp9_samples(&p.u), stride: p.uv_stride }),
                Some(Src { samples: vp9_samples(&p.v), stride: p.uv_stride }),
            ];
            let chroma = (u32::from(p.subsampling_x), u32::from(p.subsampling_y));
            self.out.push(picture(p.width, p.height, p.bit_depth, Some(chroma), planes, matrix, p.color.full_range, p.pts)?);
        }
        Ok(())
    }
}

impl Decoder for Vp9Decoder {
    fn send(&mut self, packet: &[u8], pts: i64) -> Result<()> {
        let pics = self.dec.decode(packet, pts).map_err(|e| Error::Unsupported(format!("VP9: {e}")))?;
        self.take(pics)
    }
    fn receive(&mut self) -> Option<Picture> {
        self.out.pop()
    }
    fn flush(&mut self) {
        let _ = self.dec.flush();
        self.dec.reset();
        self.out.clear();
    }
    fn finish(&mut self) {
        let pics = self.dec.flush();
        if let Err(e) = self.take(pics) {
            log::warn!("VP9: {e}");
        }
    }
}

// ---- AV1 ----------------------------------------------------------------------------

pub struct Av1Decoder {
    dec: filmcraft_av1::Decoder,
    /// The sequence header OBUs from the av1C record (MP4/MKV), fed after every reset.
    config_obus: Vec<u8>,
    out: Output,
}

impl Av1Decoder {
    pub fn new(info: &StreamInfo) -> Self {
        // av1C: 4 bytes of fields, then configOBUs.
        let config_obus = if info.extradata.len() > 4 && info.extradata[0] & 0x80 != 0 { info.extradata[4..].to_vec() } else { Vec::new() };
        let mut d = Av1Decoder { dec: filmcraft_av1::Decoder::new(), config_obus, out: Output::default() };
        d.configure();
        d
    }

    fn configure(&mut self) {
        self.dec = filmcraft_av1::Decoder::new();
        if !self.config_obus.is_empty() {
            if let Err(e) = self.dec.decode(&self.config_obus) {
                log::warn!("AV1 setup data rejected: {e}");
            }
        }
    }

    fn take(&mut self, pics: Vec<filmcraft_av1::Picture>) -> Result<()> {
        for p in pics {
            if p.matrix_coefficients == 0 {
                return Err(Error::Unsupported("AV1 in RGB".into()));
            }
            let chroma = (!p.mono_chrome).then_some((u32::from(p.subsampling_x), u32::from(p.subsampling_y)));
            let depth = u32::from(p.bit_depth);
            // 8-bit pictures arrive as u16 samples too: narrow them.
            let narrow: Vec<Vec<u8>>;
            let planes: [Option<Src>; 3] = if depth == 8 {
                narrow = p.planes.iter().map(|pl| pl.iter().map(|&v| v as u8).collect()).collect();
                std::array::from_fn(|i| (i == 0 || !p.mono_chrome).then(|| Src { samples: Samples::U8(&narrow[i]), stride: p.plane_width(i) as usize }))
            } else {
                std::array::from_fn(|i| (i == 0 || !p.mono_chrome).then(|| Src { samples: Samples::U16(&p.planes[i]), stride: p.plane_width(i) as usize }))
            };
            self.out.push(picture(p.width, p.height, depth, chroma, planes, h273_matrix(p.matrix_coefficients), p.full_range, p.pts)?);
        }
        Ok(())
    }
}

impl Decoder for Av1Decoder {
    fn send(&mut self, packet: &[u8], pts: i64) -> Result<()> {
        let pics = self.dec.decode_pts(packet, pts).map_err(|e| Error::Unsupported(format!("AV1: {e}")))?;
        self.take(pics)
    }
    fn receive(&mut self) -> Option<Picture> {
        self.out.pop()
    }
    fn flush(&mut self) {
        // Drops everything in flight; parameter sets and the thread pool stay.
        let _ = self.dec.flush();
        self.out.clear();
    }
    fn finish(&mut self) {
        match self.dec.flush_result() {
            Ok(pics) => {
                if let Err(e) = self.take(pics) {
                    log::warn!("AV1: {e}");
                }
            }
            Err(e) => log::warn!("AV1: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monochrome_gets_neutral_chroma() {
        let y = vec![50u16; 4 * 2];
        let p = picture(4, 2, 10, None, [Some(Src { samples: Samples::U16(&y), stride: 4 }), None, None], Matrix::Bt709, false, 7).unwrap();
        assert_eq!(p.format, Format::Yuv420P10);
        assert_eq!(p.sample(0, 3, 1), 50);
        assert_eq!(p.sample(1, 1, 0), 512);
        assert_eq!(p.pts, 7);
    }

    #[test]
    fn short_planes_are_rejected() {
        let y = vec![0u8; 5];
        let r = picture(4, 2, 8, Some((1, 1)), [Some(Src { samples: Samples::U8(&y), stride: 4 }), None, None], Matrix::Unknown, false, 0);
        assert!(r.is_err());
    }
}
