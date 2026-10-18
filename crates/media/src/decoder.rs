//! A video decoder running on its own thread.
//!
//! The thread decodes forward from the current position and pushes frames
//! (YUV planes as decoded, converted to RGB later on the GPU; RGBA for the
//! occasional format that isn't 4:2:0) into a small bounded channel, so it naturally stays a few frames ahead of
//! playback and blocks when the consumer stops reading. Seeks carry a
//! `generation` number; frames from before the latest seek are tagged with the
//! old generation so the consumer can drop them.

use crate::{init, to_rational, AV_TIME_BASE};
use anyhow::{anyhow, Context as _, Result};
use crossbeam_channel::{bounded, select, Receiver, Sender};
use ffmpeg_next as ff;
use ff::software::scaling::{context::Context as Scaler, flag::Flags};
use reelforge_core::frame::{Pixels, YuvLayout, YuvMatrix, YuvPlanes};
use reelforge_core::{Rational, Time};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

/// A decoded frame: YUV planes as the decoder made them, or RGBA.
#[derive(Clone)]
pub struct VideoFrame {
    /// Presentation time relative to the start of the stream.
    pub pts: Time,
    pub width: u32,
    pub height: u32,
    pub pixels: Pixels,
    pub generation: u64,
}

impl VideoFrame {
    /// A frame from tightly packed RGBA8.
    pub fn from_rgba(pts: Time, width: u32, height: u32, rgba: Arc<[u8]>, generation: u64) -> Self {
        Self { pts, width, height, pixels: Pixels::Rgba(rgba), generation }
    }

    /// The pixels as RGBA8 (converted on the CPU if they're YUV).
    pub fn rgba(&self) -> Arc<[u8]> {
        self.pixels.to_rgba()
    }

    /// Memory used by the pixels.
    pub fn byte_len(&self) -> usize {
        self.pixels.byte_len()
    }

    /// Brightness 0–255 per pixel (for the motion tracker).
    pub fn luma(&self) -> Vec<f32> {
        match &self.pixels {
            Pixels::Yuv(y) => y.luma(),
            Pixels::Rgba(b) => reelforge_core::track::luma_of_rgba(b),
        }
    }
}

impl std::fmt::Debug for VideoFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "VideoFrame({}x{} @ {} gen {})", self.width, self.height, self.pts, self.generation)
    }
}

enum Cmd {
    Seek { to: Time, generation: u64 },
    Stop,
}

/// Owner side of a decoder thread. Dropping it stops the thread.
pub struct DecoderHandle {
    cmd: Sender<Cmd>,
    pub frames: Receiver<VideoFrame>,
    thread: Option<JoinHandle<()>>,
    pub path: PathBuf,
    /// Nominal duration of one frame (from the stream's average frame rate).
    pub frame_duration: Time,
    /// Generation in which the decoder ran out of frames (`u64::MAX`: not yet).
    eof: Arc<AtomicU64>,
}

impl DecoderHandle {
    /// Spawn a decoder for the best video stream of `path`.
    ///
    /// `max_size` caps the output resolution (keeping aspect) so the preview
    /// doesn't pay for full 4K conversions. Use `None` for full resolution.
    pub fn spawn(path: &Path, max_size: Option<(u32, u32)>, queue: usize) -> Result<Self> {
        init();
        let (cmd_tx, cmd_rx) = bounded(16);
        let (frame_tx, frame_rx) = bounded(queue.max(1));
        let (ready_tx, ready_rx) = bounded::<Result<Time>>(1);
        let owned = path.to_owned();
        let eof = Arc::new(AtomicU64::new(u64::MAX));
        let eof_thread = eof.clone();
        // FFmpeg contexts aren't `Send`, so the decoder is opened on its own thread.
        let thread = std::thread::Builder::new()
            .name(format!("decode:{}", path.file_name().unwrap_or_default().to_string_lossy()))
            .spawn(move || {
                let state = match DecodeState::open(&owned, max_size, eof_thread) {
                    Ok(s) => {
                        let _ = ready_tx.send(Ok(s.frame_dur));
                        s
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                        return;
                    }
                };
                if let Err(e) = state.run(cmd_rx, frame_tx) {
                    log::error!("decoder thread failed: {e:#}");
                }
            })?;
        let frame_duration = ready_rx.recv().map_err(|_| anyhow!("decoder thread died"))??;
        Ok(Self { cmd: cmd_tx, frames: frame_rx, thread: Some(thread), path: path.to_owned(), frame_duration, eof })
    }

    /// Has the decoder delivered everything it will for seek `generation`?
    /// (All of its frames may still be sitting in the channel.)
    pub fn reached_eof(&self, generation: u64) -> bool {
        self.eof.load(Ordering::Acquire) == generation
    }

    pub fn seek(&self, to: Time, generation: u64) {
        let _ = self.cmd.send(Cmd::Seek { to, generation });
    }
}

impl Drop for DecoderHandle {
    fn drop(&mut self) {
        let _ = self.cmd.send(Cmd::Stop);
        // Unblock a thread waiting to push a frame.
        while self.frames.try_recv().is_ok() {}
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

type ScalerKey = (ff::format::Pixel, u32, u32, ff::util::color::Space, ff::util::color::Range);

/// The 4:2:0 formats kept as YUV, and the layout they become.
fn yuv_layout(fmt: ff::format::Pixel) -> Option<YuvLayout> {
    use ff::format::Pixel as P;
    match fmt {
        P::YUV420P | P::YUVJ420P => Some(YuvLayout::I420),
        P::NV12 => Some(YuvLayout::Nv12),
        P::YUV420P10LE => Some(YuvLayout::I420P10),
        P::P010LE => Some(YuvLayout::P010),
        _ => None,
    }
}

/// Copy a frame's planes into tight rows (FFmpeg pads each row to `stride`).
fn pack_planes(f: &ff::frame::Video, layout: YuvLayout) -> Vec<Vec<u8>> {
    let (w, h) = (f.width() as usize, f.height() as usize);
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let b = layout.sample_bytes();
    let shapes: Vec<(usize, usize)> = if layout.interleaved() { vec![(w * b, h), (cw * 2 * b, ch)] } else { vec![(w * b, h), (cw * b, ch), (cw * b, ch)] };
    shapes
        .into_iter()
        .enumerate()
        .map(|(i, (row, rows))| {
            let (data, stride) = (f.data(i), f.stride(i));
            let mut out = Vec::with_capacity(row * rows);
            for y in 0..rows {
                out.extend_from_slice(&data[y * stride..y * stride + row]);
            }
            out
        })
        .collect()
}

/// FFmpeg's decoder, or Reelforge's own (see `native.rs`).
enum Video {
    Ff(ff::decoder::Video),
    Native(crate::native::NativeVideo),
}

impl Video {
    /// Decode a packet. The error is FFmpeg's (`Ok(Err)`) or the native decoder's (`Err`).
    fn send_packet(&mut self, p: &ff::Packet) -> std::result::Result<std::result::Result<(), ff::Error>, reelforge_codecs::Error> {
        match self {
            Video::Ff(d) => Ok(d.send_packet(p)),
            Video::Native(d) => d.send_packet(p).map(Ok),
        }
    }
    fn receive_frame(&mut self, f: &mut ff::frame::Video) -> bool {
        match self {
            Video::Ff(d) => d.receive_frame(f).is_ok(),
            Video::Native(d) => d.receive_frame(f),
        }
    }
    fn send_eof(&mut self) {
        match self {
            Video::Ff(d) => {
                let _ = d.send_eof();
            }
            Video::Native(d) => d.send_eof(),
        }
    }
    fn flush(&mut self) {
        match self {
            Video::Ff(d) => d.flush(),
            Video::Native(d) => d.flush(),
        }
    }
    fn is_native(&self) -> bool {
        matches!(self, Video::Native(_))
    }
}

struct DecodeState {
    ictx: ff::format::context::Input,
    decoder: Video,
    stream_index: usize,
    time_base: Rational,
    start_pts: i64,
    frame_dur: Time,
    max_size: Option<(u32, u32)>,
    scaler: Option<(Scaler, ScalerKey)>,
    generation: u64,
    /// After a seek, drop frames that end before this time (decoding from the keyframe).
    discard_before: Option<Time>,
    /// Last frame skipped while pre-rolling, so a seek past the end still shows the final frame.
    held: Option<(ff::frame::Video, Time)>,
    eof: Arc<AtomicU64>,
    /// Pixel format of hardware frames, when the decoder runs on the GPU.
    hw_format: Option<i32>,
    /// Where decoding has got to (pts of the last frame seen), for resuming after a fallback.
    position: Time,
}

/// Open a decoder for `params`: on the GPU if `hw`, else in software (multi-threaded).
fn open_decoder(params: ff::codec::Parameters, hw: bool) -> Result<(ff::decoder::Video, Option<i32>)> {
    let mut ctx = ff::codec::context::Context::from_parameters(params.clone())?;
    // SAFETY: a fresh, unopened context.
    let hw_format = if hw { unsafe { crate::hw::attach(ctx.as_mut_ptr()) } } else { None };
    if hw_format.is_none() {
        // `..Default::default()` covers fields that only exist on older FFmpeg versions.
        #[allow(clippy::needless_update)]
        ctx.set_threading(ff::codec::threading::Config {
            kind: ff::codec::threading::Type::Frame,
            count: 0, // let FFmpeg pick based on core count
            ..Default::default()
        });
    }
    match ctx.decoder().video() {
        Ok(d) => Ok((d, hw_format)),
        Err(e) if hw_format.is_some() => {
            log::info!("hardware decoder didn't open ({e}); using software");
            open_decoder(params, false)
        }
        Err(e) => Err(e.into()),
    }
}

enum Flow {
    Continue,
    Restart,
    Stop,
}

impl DecodeState {
    fn open(path: &Path, max_size: Option<(u32, u32)>, eof: Arc<AtomicU64>) -> Result<Self> {
        let ictx = ff::format::input(path).with_context(|| format!("cannot open {}", path.display()))?;
        let st = ictx.streams().best(ff::media::Type::Video).ok_or_else(|| anyhow!("no video stream"))?;
        let stream_index = st.index();
        let time_base = to_rational(st.time_base());
        let start_pts = if st.start_time() == ff::ffi::AV_NOPTS_VALUE { 0 } else { st.start_time() };
        let mut rate = to_rational(st.avg_frame_rate());
        if rate.num <= 0 {
            rate = Rational::new(30, 1);
        }
        let frame_dur = Time::from_rational(Rational::new(rate.den, rate.num));

        // Reelforge's own decoder when it has one for this codec; else FFmpeg's.
        let (decoder, hw_format) = match crate::native::open(&st.parameters()) {
            Some(n) => (Video::Native(n), None),
            None => {
                // Stills and tiny files gain nothing from the GPU.
                let small = { let p = st.parameters(); unsafe { (*p.as_ptr()).width * (*p.as_ptr()).height < 640 * 360 } };
                let (d, hw) = open_decoder(st.parameters(), crate::hw::enabled() && !small)?;
                (Video::Ff(d), hw)
            }
        };

        Ok(Self {
            ictx,
            decoder,
            stream_index,
            time_base,
            start_pts,
            frame_dur,
            max_size,
            scaler: None,
            generation: 0,
            discard_before: None,
            held: None,
            eof,
            hw_format,
            position: Time::ZERO,
        })
    }

    /// The GPU decoder (or Reelforge's own) failed: carry on with FFmpeg's
    /// software decoder from where we were.
    fn fall_back_to_software(&mut self) -> Result<()> {
        log::warn!("{} decoding failed; switching this file to FFmpeg's software decoder", if self.decoder.is_native() { "Reelforge's own" } else { "hardware" });
        let params = self.ictx.stream(self.stream_index).ok_or_else(|| anyhow!("stream vanished"))?.parameters();
        let (decoder, _) = open_decoder(params, false)?;
        self.decoder = Video::Ff(decoder);
        self.hw_format = None;
        self.scaler = None;
        // Re-decode from the keyframe before where we were, skipping what was already delivered.
        let resume = self.discard_before.unwrap_or(self.position + self.frame_dur);
        let us = resume.to_pts(Rational::new(1, AV_TIME_BASE))
            + Time::from_pts(self.start_pts, self.time_base).to_pts(Rational::new(1, AV_TIME_BASE));
        let _ = self.ictx.seek(us, ..us);
        self.discard_before = Some(resume);
        self.held = None;
        Ok(())
    }

    fn run(mut self, cmds: Receiver<Cmd>, out: Sender<VideoFrame>) -> Result<()> {
        'outer: loop {
            // Process any pending commands before reading more data.
            while let Ok(cmd) = cmds.try_recv() {
                if let Flow::Stop = self.handle(cmd) {
                    return Ok(());
                }
            }

            let mut eof = true;
            let mut packet = ff::Packet::empty();
            while packet.read(&mut self.ictx).is_ok() {
                if packet.stream() != self.stream_index {
                    continue;
                }
                eof = false;
                match self.decoder.send_packet(&packet) {
                    Err(e) => {
                        log::warn!("Reelforge's decoder: {e}");
                        self.fall_back_to_software()?;
                        continue 'outer;
                    }
                    Ok(Err(e)) => {
                        if self.hw_format.is_some() && !matches!(e, ff::Error::Other { errno: ff::util::error::EAGAIN }) {
                            self.fall_back_to_software()?;
                            continue 'outer;
                        }
                        log::warn!("send_packet: {e}");
                    }
                    Ok(Ok(())) => {}
                }
                match self.drain(&cmds, &out)? {
                    Flow::Continue => {}
                    Flow::Restart => continue 'outer,
                    Flow::Stop => return Ok(()),
                }
                break; // one packet per iteration so commands stay responsive
            }

            if eof {
                // Flush the decoder's delayed frames, then park until told to seek or stop.
                self.decoder.send_eof();
                match self.drain(&cmds, &out)? {
                    Flow::Stop => return Ok(()),
                    Flow::Restart => continue 'outer,
                    Flow::Continue => {}
                }
                // Seeked past the last frame: deliver the final frame instead of nothing.
                if let (Some(_), Some((frame, pts))) = (self.discard_before.take(), self.held.take()) {
                    let f = self.convert(&frame, pts)?;
                    if out.send(f).is_err() {
                        return Ok(());
                    }
                }
                self.eof.store(self.generation, Ordering::Release);
                match cmds.recv() {
                    Ok(cmd) => {
                        if let Flow::Stop = self.handle(cmd) {
                            return Ok(());
                        }
                    }
                    Err(_) => return Ok(()),
                }
            }
        }
    }

    fn handle(&mut self, cmd: Cmd) -> Flow {
        match cmd {
            Cmd::Stop => Flow::Stop,
            Cmd::Seek { to, generation } => {
                self.generation = generation;
                let us = to.to_pts(Rational::new(1, AV_TIME_BASE))
                    + Time::from_pts(self.start_pts, self.time_base).to_pts(Rational::new(1, AV_TIME_BASE));
                // Land on the keyframe at or before the target; we decode forward from there.
                if let Err(e) = self.ictx.seek(us, ..us) {
                    log::warn!("seek to {to} failed: {e}");
                }
                self.decoder.flush();
                self.discard_before = Some(to);
                self.held = None;
                Flow::Restart
            }
        }
    }

    /// Pull every frame the decoder has ready and hand it to the consumer.
    fn drain(&mut self, cmds: &Receiver<Cmd>, out: &Sender<VideoFrame>) -> Result<Flow> {
        let mut decoded = ff::frame::Video::empty();
        while self.decoder.receive_frame(&mut decoded) {
            let ts = decoded.timestamp().or(decoded.pts()).unwrap_or(self.start_pts);
            let pts = Time::from_pts(ts - self.start_pts, self.time_base);
            self.position = pts;

            if let Some(target) = self.discard_before {
                if pts + self.frame_dur <= target {
                    // Pre-roll from the keyframe: skip the expensive conversion, but keep the
                    // frame (by swapping buffers, no copy) in case the stream ends here.
                    let held = self.held.take().map(|(f, _)| f).unwrap_or_else(ff::frame::Video::empty);
                    self.held = Some((std::mem::replace(&mut decoded, held), pts));
                    continue;
                }
                self.discard_before = None;
                self.held = None;
            }

            let frame = match self.convert(&decoded, pts) {
                Ok(f) => f,
                Err(e) if self.hw_format.is_some() => {
                    log::warn!("couldn't read back a hardware frame: {e:#}");
                    self.discard_before = Some(pts);
                    self.position = pts - self.frame_dur;
                    self.fall_back_to_software()?;
                    return Ok(Flow::Restart);
                }
                Err(e) => return Err(e),
            };
            // Block until the consumer takes the frame, but stay responsive to commands.
            select! {
                send(out, frame) -> r => if r.is_err() { return Ok(Flow::Stop) },
                recv(cmds) -> cmd => {
                    return Ok(match cmd { Ok(c) => self.handle(c), Err(_) => Flow::Stop });
                }
            }
        }
        Ok(Flow::Continue)
    }

    fn convert(&mut self, src: &ff::frame::Video, pts: Time) -> Result<VideoFrame> {
        // GPU frames are copied back to memory first (as NV12 or similar).
        let downloaded;
        // SAFETY: reading the format field of a valid frame.
        let src = if self.hw_format.is_some_and(|f| unsafe { (*src.as_ptr()).format } == f) {
            let mut sw = ff::frame::Video::empty();
            // SAFETY: `src` is a decoded hardware frame and `sw` an empty frame.
            if !unsafe { crate::hw::download(src.as_ptr(), sw.as_mut_ptr()) } {
                return Err(anyhow!("hardware frame transfer failed"));
            }
            downloaded = sw;
            &downloaded
        } else {
            src
        };
        let (sw, sh, fmt) = (src.width(), src.height(), src.format());
        let (dw, dh) = fit(sw, sh, self.max_size);
        let space = crate::color::effective_space(src.color_space(), sh);
        let range = src.color_range();

        // 4:2:0 video stays YUV: the GPU converts it as it draws. Shrinking (for the
        // viewer) is a cheap YUV-to-YUV scale; no colour conversion happens here.
        if let Some(layout) = yuv_layout(fmt) {
            let matrix = match space {
                ff::util::color::Space::BT709 => YuvMatrix::Bt709,
                ff::util::color::Space::BT2020NCL | ff::util::color::Space::BT2020CL => YuvMatrix::Bt2020,
                _ => YuvMatrix::Bt601,
            };
            let full_range = range == ff::util::color::Range::JPEG || fmt == ff::format::Pixel::YUVJ420P;
            let (planes, layout, w, h) = if (dw, dh) == (sw, sh) {
                (pack_planes(src, layout), layout, sw, sh)
            } else {
                let (out_fmt, out_layout) = if layout.bits() > 8 { (ff::format::Pixel::YUV420P10LE, YuvLayout::I420P10) } else { (ff::format::Pixel::YUV420P, YuvLayout::I420) };
                let key = (fmt, sw, sh, space, range);
                if self.scaler.as_ref().map(|(_, k)| *k != key).unwrap_or(true) {
                    self.scaler = Some((Scaler::get(fmt, sw, sh, out_fmt, dw, dh, Flags::BILINEAR)?, key));
                }
                let mut small = ff::frame::Video::empty();
                self.scaler.as_mut().unwrap().0.run(src, &mut small)?;
                (pack_planes(&small, out_layout), out_layout, dw, dh)
            };
            let yuv = YuvPlanes { layout, width: w, height: h, planes, matrix, full_range };
            return Ok(VideoFrame { pts, width: w, height: h, pixels: Pixels::Yuv(Arc::new(yuv)), generation: self.generation });
        }

        let key = (fmt, sw, sh, space, range);
        if self.scaler.as_ref().map(|(_, k)| *k != key).unwrap_or(true) {
            let mut s = Scaler::get(fmt, sw, sh, ff::format::Pixel::RGBA, dw, dh, Flags::BILINEAR)?;
            crate::color::configure(&mut s, space, range, true);
            self.scaler = Some((s, key));
        }
        let mut rgba = ff::frame::Video::empty();
        self.scaler.as_mut().unwrap().0.run(src, &mut rgba)?;

        // Repack: FFmpeg rows are padded to `stride`, wgpu wants tight rows.
        let row = dw as usize * 4;
        let stride = rgba.stride(0);
        let data = rgba.data(0);
        let mut packed = Vec::with_capacity(row * dh as usize);
        for y in 0..dh as usize {
            packed.extend_from_slice(&data[y * stride..y * stride + row]);
        }
        Ok(VideoFrame::from_rgba(pts, dw, dh, packed.into(), self.generation))
    }
}

/// Fit `w x h` inside `max`, keeping aspect and even dimensions.
fn fit(w: u32, h: u32, max: Option<(u32, u32)>) -> (u32, u32) {
    let Some((mw, mh)) = max else { return (w, h) };
    if w <= mw && h <= mh {
        return (w, h);
    }
    let s = (mw as f64 / w as f64).min(mh as f64 / h as f64);
    let even = |v: f64| ((v.round() as u32) & !1).max(2);
    (even(w as f64 * s), even(h as f64 * s))
}

#[cfg(test)]
mod tests {
    use super::fit;

    #[test]
    fn fit_keeps_aspect() {
        assert_eq!(fit(3840, 2160, Some((1920, 1080))), (1920, 1080));
        assert_eq!(fit(1280, 720, Some((1920, 1080))), (1280, 720));
        assert_eq!(fit(1080, 1920, Some((1920, 1080))), (608, 1080));
    }
}
