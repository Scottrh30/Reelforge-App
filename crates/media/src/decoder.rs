//! A video decoder running on its own thread.
//!
//! The thread decodes forward from the current position and pushes RGBA frames
//! into a small bounded channel, so it naturally stays a few frames ahead of
//! playback and blocks when the consumer stops reading. Seeks carry a
//! `generation` number; frames from before the latest seek are tagged with the
//! old generation so the consumer can drop them.

use crate::{init, to_rational, AV_TIME_BASE};
use anyhow::{anyhow, Context as _, Result};
use crossbeam_channel::{bounded, select, Receiver, Sender};
use ffmpeg_next as ff;
use ff::software::scaling::{context::Context as Scaler, flag::Flags};
use reelforge_core::{Rational, Time};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

/// A decoded frame, converted to tightly packed 8-bit RGBA.
#[derive(Clone)]
pub struct VideoFrame {
    /// Presentation time relative to the start of the stream.
    pub pts: Time,
    pub width: u32,
    pub height: u32,
    pub rgba: Arc<[u8]>,
    pub generation: u64,
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

struct DecodeState {
    ictx: ff::format::context::Input,
    decoder: ff::decoder::Video,
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

        let mut ctx = ff::codec::context::Context::from_parameters(st.parameters())?;
        // `..Default::default()` covers fields that only exist on older FFmpeg versions.
        #[allow(clippy::needless_update)]
        ctx.set_threading(ff::codec::threading::Config {
            kind: ff::codec::threading::Type::Frame,
            count: 0, // let FFmpeg pick based on core count
            ..Default::default()
        });
        let decoder = ctx.decoder().video()?;

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
        })
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
                if let Err(e) = self.decoder.send_packet(&packet) {
                    log::warn!("send_packet: {e}");
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
                let _ = self.decoder.send_eof();
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
        while self.decoder.receive_frame(&mut decoded).is_ok() {
            let ts = decoded.timestamp().or(decoded.pts()).unwrap_or(self.start_pts);
            let pts = Time::from_pts(ts - self.start_pts, self.time_base);

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

            let frame = self.convert(&decoded, pts)?;
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
        let (sw, sh, fmt) = (src.width(), src.height(), src.format());
        let (dw, dh) = fit(sw, sh, self.max_size);
        let space = crate::color::effective_space(src.color_space(), sh);
        let range = src.color_range();
        let key = (fmt, sw, sh, space, range);
        if self.scaler.as_ref().map(|(_, k)| *k != key).unwrap_or(true) {
            let mut s = Scaler::get(fmt, sw, sh, ff::format::Pixel::RGBA, dw, dh, Flags::BILINEAR)?;
            crate::color::configure(&mut s, space, range, true);
            self.scaler = Some((s, key));
        }
        let mut rgba = ff::frame::Video::empty();
        self.scaler.as_mut().unwrap().0.run(src, &mut rgba)?;

        // Repack: FFmpeg rows are padded to `stride`, egui/wgpu want tight rows.
        let row = dw as usize * 4;
        let stride = rgba.stride(0);
        let data = rgba.data(0);
        let mut packed = Vec::with_capacity(row * dh as usize);
        for y in 0..dh as usize {
            packed.extend_from_slice(&data[y * stride..y * stride + row]);
        }
        Ok(VideoFrame { pts, width: dw, height: dh, rgba: packed.into(), generation: self.generation })
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
