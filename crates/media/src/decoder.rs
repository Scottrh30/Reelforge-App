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
use reelforge_core::frame::{Chroma, FloatPixels, Pixels, Transfer, YuvLayout, YuvMatrix, YuvPlanes};
use reelforge_core::{Rational, Time};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

/// Preferences → Limit the Viewer's video to 8 bits: 10-bit and deeper
/// (SDR) video for the Viewer is brought down to 8 bits as it's decoded,
/// halving the memory and upload each frame takes. Exports always get the
/// full depth, and HDR keeps it (8-bit PQ or HLG would band).
static VIEWER_8BIT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_viewer_8bit(on: bool) {
    VIEWER_8BIT.store(on, Ordering::Relaxed);
}

pub fn viewer_8bit() -> bool {
    VIEWER_8BIT.load(Ordering::Relaxed)
}

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
            Pixels::Float(f) => reelforge_core::track::luma_of_rgba(&f.to_rgba8()),
            Pixels::Gpu(g) => g.cpu().map(|y| y.luma()).unwrap_or_else(|| vec![0.0; g.width as usize * g.height as usize]),
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
        Self::spawn_with(path, max_size, queue, false)
    }

    /// [`Self::spawn`]; with `gpu_frames`, hardware-decoded frames may stay on the
    /// graphics card ([`Pixels::Gpu`], zero-copy) when the compositor can read
    /// them there. Only for frames that go straight to the viewer.
    pub fn spawn_with(path: &Path, max_size: Option<(u32, u32)>, queue: usize, gpu_frames: bool) -> Result<Self> {
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
                // Blackmagic RAW: frames come from Blackmagic's SDK, not FFmpeg.
                if crate::camraw::kind(&owned) == Some(crate::camraw::RawKind::Braw) {
                    run_braw(&owned, max_size, cmd_rx, frame_tx, eof_thread, ready_tx);
                    return;
                }
                let state = match DecodeState::open(&owned, max_size, eof_thread) {
                    Ok(mut s) => {
                        s.gpu_frames = gpu_frames;
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

/// The decode loop for a Blackmagic RAW clip: frame by frame from the SDK, answering seeks and
/// stopping like the FFmpeg loop.
fn run_braw(path: &Path, max_size: Option<(u32, u32)>, cmds: Receiver<Cmd>, out: Sender<VideoFrame>, eof: Arc<AtomicU64>, ready: Sender<Result<Time>>) {
    let clip = match crate::camraw::braw::Clip::open(path) {
        Ok(c) => c,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    let rate = reelforge_core::adobe::rate_of(f64::from(clip.fps));
    let _ = ready.send(Ok(Time::from_frame(1, rate)));
    let mut generation = 0u64;
    let mut next: u64 = 0;
    let mut failed = false;
    let handle = |cmd: Cmd, generation: &mut u64, next: &mut u64| -> bool {
        match cmd {
            Cmd::Stop => false,
            Cmd::Seek { to, generation: g } => {
                *generation = g;
                *next = to.to_frame(rate).max(0) as u64;
                true
            }
        }
    };
    loop {
        while let Ok(cmd) = cmds.try_recv() {
            if !handle(cmd, &mut generation, &mut next) {
                return;
            }
        }
        if next >= clip.frames {
            // Seeked past the end: the last frame, then wait.
            if clip.frames > 0 && next > clip.frames {
                next = clip.frames - 1;
                continue;
            }
            eof.store(generation, Ordering::Release);
            match cmds.recv() {
                Ok(cmd) => {
                    if !handle(cmd, &mut generation, &mut next) {
                        return;
                    }
                    continue;
                }
                Err(_) => return,
            }
        }
        let frame = match clip.decode(next, max_size) {
            Ok((w, h, data)) => {
                failed = false;
                let f = rgba64_to_float(&data, w as usize * 8, w, h);
                VideoFrame { pts: Time::from_frame(next as i64, rate), width: w, height: h, pixels: Pixels::Float(Arc::new(f)), generation }
            }
            Err(e) => {
                if !failed {
                    log::warn!("{}: {e:#}", path.display());
                }
                failed = true;
                next += 1;
                continue;
            }
        };
        // Block until the frame is taken, but answer commands meanwhile (a seek drops it).
        let taken: Option<Result<Cmd, ()>> = select! {
            send(out, frame) -> r => if r.is_err() { Some(Err(())) } else { None },
            recv(cmds) -> cmd => Some(cmd.map_err(drop)),
        };
        match taken {
            None => next += 1,
            Some(Ok(c)) => {
                if !handle(c, &mut generation, &mut next) {
                    return;
                }
            }
            Some(Err(())) => return,
        }
    }
}

type ScalerKey = (ff::format::Pixel, u32, u32, ff::util::color::Space, ff::util::color::Range);

/// The formats kept as YUV (converted on the GPU as they're drawn), and the
/// layout they become: 4:2:0, 4:2:2 and 4:4:4, 8 to 16 bits, with or without alpha.
fn yuv_layout(fmt: ff::format::Pixel) -> Option<YuvLayout> {
    use ff::format::Pixel as P;
    use Chroma::*;
    let planar = |bits: u8, chroma, alpha| Some(YuvLayout::Planar { bits, chroma, alpha });
    match fmt {
        P::NV12 => Some(YuvLayout::Nv12),
        P::P010LE => Some(YuvLayout::P010),
        P::YUV420P | P::YUVJ420P => planar(8, C420, false),
        P::YUV422P | P::YUVJ422P => planar(8, C422, false),
        P::YUV444P | P::YUVJ444P => planar(8, C444, false),
        P::YUVA420P => planar(8, C420, true),
        P::YUVA422P => planar(8, C422, true),
        P::YUVA444P => planar(8, C444, true),
        P::YUV420P10LE => planar(10, C420, false),
        P::YUV422P10LE => planar(10, C422, false),
        P::YUV444P10LE => planar(10, C444, false),
        P::YUV420P12LE => planar(12, C420, false),
        P::YUV422P12LE => planar(12, C422, false),
        P::YUV444P12LE => planar(12, C444, false),
        P::YUV420P16LE => planar(16, C420, false),
        P::YUV422P16LE => planar(16, C422, false),
        P::YUV444P16LE => planar(16, C444, false),
        P::YUVA420P10LE => planar(10, C420, true),
        P::YUVA422P10LE => planar(10, C422, true),
        P::YUVA444P10LE => planar(10, C444, true),
        P::YUVA422P12LE => planar(12, C422, true),
        P::YUVA444P12LE => planar(12, C444, true),
        P::YUVA444P16LE => planar(16, C444, true),
        _ => None,
    }
}

/// The planar FFmpeg format of a layout (what a shrunken copy is made in).
fn planar_format(layout: YuvLayout) -> Option<ff::format::Pixel> {
    use ff::format::Pixel as P;
    use Chroma::*;
    let (bits, chroma, alpha) = match layout {
        YuvLayout::Nv12 => (8, C420, false),
        YuvLayout::P010 => (10, C420, false),
        YuvLayout::Planar { bits, chroma, alpha } => (bits, chroma, alpha),
    };
    Some(match (bits, chroma, alpha) {
        (8, C420, false) => P::YUV420P,
        (8, C422, false) => P::YUV422P,
        (8, C444, false) => P::YUV444P,
        (8, C420, true) => P::YUVA420P,
        (8, C422, true) => P::YUVA422P,
        (8, C444, true) => P::YUVA444P,
        (10, C420, false) => P::YUV420P10LE,
        (10, C422, false) => P::YUV422P10LE,
        (10, C444, false) => P::YUV444P10LE,
        (12, C420, false) => P::YUV420P12LE,
        (12, C422, false) => P::YUV422P12LE,
        (12, C444, false) => P::YUV444P12LE,
        (16, C420, false) => P::YUV420P16LE,
        (16, C422, false) => P::YUV422P16LE,
        (16, C444, false) => P::YUV444P16LE,
        (10, C420, true) => P::YUVA420P10LE,
        (10, C422, true) => P::YUVA422P10LE,
        (10, C444, true) => P::YUVA444P10LE,
        (12, C422, true) => P::YUVA422P12LE,
        (12, C444, true) => P::YUVA444P12LE,
        (16, C444, true) => P::YUVA444P16LE,
        _ => return None,
    })
}

/// Bits per component of a pixel format (the deepest component).
fn format_depth(fmt: ff::format::Pixel) -> u32 {
    // SAFETY: av_pix_fmt_desc_get returns a pointer to static data, or null.
    unsafe {
        let d = ff::ffi::av_pix_fmt_desc_get(fmt.into());
        if d.is_null() {
            return 8;
        }
        let n = ((*d).nb_components as usize).min(4);
        (&(*d).comp)[..n].iter().map(|c| c.depth as u32).max().unwrap_or(8)
    }
}

/// Copy a frame's planes into tight rows (FFmpeg pads each row to `stride`).
fn pack_planes(f: &ff::frame::Video, layout: YuvLayout) -> Vec<Vec<u8>> {
    let (w, h) = (f.width() as usize, f.height() as usize);
    let (cw, ch) = layout.chroma_size(w as u32, h as u32);
    let (cw, ch) = (cw as usize, ch as usize);
    let b = layout.sample_bytes();
    let mut shapes: Vec<(usize, usize)> = if layout.interleaved() { vec![(w * b, h), (cw * 2 * b, ch)] } else { vec![(w * b, h), (cw * b, ch), (cw * b, ch)] };
    if layout.alpha() {
        shapes.push((w * b, h));
    }
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
    /// The scaler makes 16-bit RGBA (for deep RGB), not 8-bit.
    scaler_deep: bool,
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
    /// Packets read ahead while finding a keyframe after a seek, to decode first.
    pending: std::collections::VecDeque<ff::Packet>,
    /// Hand hardware frames over on the graphics card when possible (zero-copy).
    gpu_frames: bool,
    /// The first such frame also carries the usual copy, for the compositor to check.
    #[cfg_attr(not(windows), allow(dead_code))]
    gpu_checked: bool,
    /// ProRes RAW: the camera's settings from the latest frames' headers, by pts, for
    /// developing their mosaics.
    raw: std::collections::VecDeque<(Option<i64>, crate::prores_raw::RawParams)>,
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
        let ictx = crate::open_input(path).with_context(|| format!("cannot open {}", path.display()))?;
        let st = ictx.streams().best(ff::media::Type::Video).ok_or_else(|| anyhow!("no video stream"))?;
        let stream_index = st.index();
        let time_base = to_rational(st.time_base());
        let start_pts = if st.start_time() == ff::ffi::AV_NOPTS_VALUE { 0 } else { st.start_time() };
        let mut rate = to_rational(st.avg_frame_rate());
        if rate.num <= 0 {
            rate = Rational::new(30, 1);
        }
        let frame_dur = Time::from_rational(Rational::new(rate.den, rate.num));

        // Stills and tiny files gain nothing from the GPU.
        let small = { let p = st.parameters(); unsafe { (*p.as_ptr()).width * (*p.as_ptr()).height < 640 * 360 } };
        let want_hw = crate::hw::enabled() && !small;
        // The delivery codecs go to the graphics card first when it can take them
        // (faster and lighter than any CPU decoder); then Reelforge's own decoder;
        // then FFmpeg's.
        let gpu_first = want_hw && crate::native::codec_of(st.parameters().id()).is_some_and(|c| c.gpu_friendly());
        let gpu = if gpu_first { open_decoder(st.parameters(), true).ok().filter(|(_, hw)| hw.is_some()) } else { None };
        let (decoder, hw_format) = match gpu {
            Some((d, hw)) => (Video::Ff(d), hw),
            None => match crate::native::open(&st.parameters()) {
                Some(n) => (Video::Native(n), None),
                None => {
                    let (d, hw) = open_decoder(st.parameters(), want_hw && !gpu_first)?;
                    (Video::Ff(d), hw)
                }
            },
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
            scaler_deep: false,
            generation: 0,
            discard_before: None,
            held: None,
            eof,
            hw_format,
            position: Time::ZERO,
            pending: Default::default(),
            gpu_frames: false,
            gpu_checked: false,
            raw: Default::default(),
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
        self.seek_to_keyframe(resume);
        self.discard_before = Some(resume);
        self.held = None;
        Ok(())
    }

    /// The next packet: read-ahead from a seek first, then the file.
    fn next_packet(&mut self, packet: &mut ff::Packet) -> bool {
        match self.pending.pop_front() {
            Some(p) => {
                *packet = p;
                true
            }
            None => crate::read_packet(packet, &mut self.ictx).is_ok(),
        }
    }

    /// Back to the very start of the file. Timestamp seeks to the start of a
    /// transport stream can land just past its first keyframe; a byte seek can't.
    /// False for containers that don't seek by byte (MP4…), whose index gets it right.
    fn rewind(&mut self) -> bool {
        // SAFETY: a valid, open format context.
        unsafe {
            let ctx = self.ictx.as_mut_ptr();
            let fmt = (*ctx).iformat;
            if fmt.is_null() || (*fmt).flags & ff::ffi::AVFMT_NO_BYTE_SEEK != 0 {
                return false;
            }
            ff::ffi::av_seek_frame(ctx, -1, 0, ff::ffi::AVSEEK_FLAG_BYTE) >= 0
        }
    }

    /// Position the input so that decoding starts at a keyframe at or before `to`.
    /// Containers with an index (MP4, MKV…) land there directly; transport and
    /// program streams (TS, MPG, VOB) are searched by timestamp and can land
    /// mid-GOP, or on a keyframe after the target: then this steps back further.
    /// The packets read while checking are kept in `pending`, from the keyframe on.
    fn seek_to_keyframe(&mut self, to: Time) {
        let base = Time::from_pts(self.start_pts, self.time_base);
        let us = |t: Time| t.to_pts(Rational::new(1, AV_TIME_BASE)) + base.to_pts(Rational::new(1, AV_TIME_BASE));
        let mut margin = Time::ZERO;
        loop {
            self.pending.clear();
            let at = if to > margin { to - margin } else { Time::ZERO };
            let rewound = at == Time::ZERO && margin > Time::ZERO && self.rewind();
            if !rewound {
                if let Err(e) = self.ictx.seek(us(at), ..us(at)) {
                    log::warn!("seek to {to} failed: {e}");
                }
            }
            // Read up to the first keyframe of our stream (within reason).
            let mut key_pts = None;
            let mut skipped = Vec::new();
            let mut hit_end = false;
            for _ in 0..4000 {
                let mut p = ff::Packet::empty();
                if p.read(&mut self.ictx).is_err() {
                    hit_end = true;
                    break;
                }
                if p.stream() != self.stream_index {
                    continue;
                }
                if p.is_key() {
                    key_pts = Some(p.pts().or(p.dts()).unwrap_or(self.start_pts));
                    self.pending.push_back(p);
                    break;
                }
                skipped.push(p);
            }
            let Some(kp) = key_pts else {
                if hit_end && at > Time::ZERO && margin < Time::from_secs_f64(60.0) {
                    // Landed after the last keyframe: step back.
                    margin = if margin == Time::ZERO { Time::from_secs_f64(1.0) } else { margin + margin };
                    continue;
                }
                // No keyframe flags to go by: decode from where the seek landed.
                self.pending.extend(skipped);
                return;
            };
            if skipped.is_empty() && margin == Time::ZERO && Time::from_pts(kp - self.start_pts, self.time_base) <= to {
                return; // the common case: straight onto a keyframe
            }
            let key_at = Time::from_pts(kp - self.start_pts, self.time_base);
            if key_at <= to || margin >= Time::from_secs_f64(60.0) {
                // Packets after the keyframe come straight from the file.
                return;
            }
            if at == Time::ZERO && (rewound || margin > Time::ZERO) {
                // Already at the very start (or the file can't be rewound by byte).
                return;
            }
            // Landed after the keyframe we need; near the start of a program or
            // transport stream that means just past the first one: go back
            // further, which from the start means rewinding to its first byte.
            margin = if margin == Time::ZERO { Time::from_secs_f64(1.0) } else { margin + margin };
        }
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
            while self.next_packet(&mut packet) {
                if packet.stream() != self.stream_index {
                    continue;
                }
                eof = false;
                if let Some(p) = packet.data().and_then(crate::prores_raw::parse) {
                    if self.raw.len() >= 64 {
                        self.raw.pop_front();
                    }
                    self.raw.push_back((packet.pts(), p));
                }
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
                // Land on the keyframe at or before the target; we decode forward from there.
                self.seek_to_keyframe(to);
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
        // SAFETY: reading the format field of a valid frame.
        let on_gpu = self.hw_format.is_some_and(|f| unsafe { (*src.as_ptr()).format } == f);
        // Zero-copy: leave the frame on the graphics card for the compositor.
        #[cfg(windows)]
        if on_gpu && self.gpu_frames && reelforge_core::frame::zero_copy::usable() {
            let (w, h) = (src.width(), src.height());
            let matrix = yuv_matrix(crate::color::effective_space(src.color_space(), h));
            let full_range = src.color_range() == ff::util::color::Range::JPEG;
            // SAFETY: `src` is a decoded D3D11VA frame.
            if let Some(mut g) = unsafe { crate::zerocopy::export(src.as_ptr(), [w, h], matrix, full_range) } {
                g.transfer = self.transfer_of(src);
                if !self.gpu_checked {
                    // The first one also comes the usual way, to be checked against.
                    let mut sw = ff::frame::Video::empty();
                    // SAFETY: as above, into an empty frame.
                    if unsafe { crate::hw::download(src.as_ptr(), sw.as_mut_ptr()) } {
                        if let Some(layout) = yuv_layout(sw.format()) {
                            g.check = Some(Arc::new(YuvPlanes { layout, width: w, height: h, planes: pack_planes(&sw, layout), matrix, full_range, transfer: g.transfer }));
                        }
                    }
                    self.gpu_checked = true;
                }
                return Ok(VideoFrame { pts, width: w, height: h, pixels: Pixels::Gpu(Arc::new(g)), generation: self.generation });
            }
        }
        // GPU frames are copied back to memory first (as NV12 or similar).
        let downloaded;
        let src = if on_gpu {
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

        // ProRes RAW's sensor mosaic, developed with the camera's settings.
        if let Some(f) = self.develop_raw(src, dw, dh) {
            return Ok(VideoFrame { pts, width: f.width, height: f.height, pixels: Pixels::Float(Arc::new(f)), generation: self.generation });
        }

        // Floating point (OpenEXR) stays floating point, for the compositor's linear float pipeline.
        if let Some(f) = float_frame(src, dw, dh) {
            return Ok(VideoFrame { pts, width: f.width, height: f.height, pixels: Pixels::Float(Arc::new(f)), generation: self.generation });
        }

        // YUV video stays YUV: the GPU converts it as it draws (at its own
        // precision: 10-bit and up into half floats). Shrinking (for the
        // viewer) is a cheap YUV-to-YUV scale; no colour conversion happens here.
        if let (Some(layout), Some(out_fmt)) = (yuv_layout(fmt), yuv_layout(fmt).and_then(planar_format)) {
            use ff::format::Pixel as P;
            let matrix = yuv_matrix(space);
            let full_range = range == ff::util::color::Range::JPEG || matches!(fmt, P::YUVJ420P | P::YUVJ422P | P::YUVJ444P);
            let (planes, layout, w, h) = if (dw, dh) == (sw, sh) {
                (pack_planes(src, layout), layout, sw, sh)
            } else {
                let out_layout = yuv_layout(out_fmt).unwrap_or(layout);
                let key = (fmt, sw, sh, space, range);
                if self.scaler.as_ref().map(|(_, k)| *k != key).unwrap_or(true) {
                    self.scaler = Some((Scaler::get(fmt, sw, sh, out_fmt, dw, dh, Flags::BILINEAR)?, key));
                }
                let mut small = ff::frame::Video::empty();
                self.scaler.as_mut().ok_or_else(|| anyhow!("no scaler"))?.0.run(src, &mut small)?;
                (pack_planes(&small, out_layout), out_layout, dw, dh)
            };
            let mut yuv = YuvPlanes { layout, width: w, height: h, planes, matrix, full_range, transfer: self.transfer_of(src) };
            // (`gpu_frames`: this decoder feeds the Viewer.)
            if self.gpu_frames && viewer_8bit() && !yuv.transfer.is_hdr() {
                yuv = yuv.to_8bit();
            }
            return Ok(VideoFrame { pts, width: w, height: h, pixels: Pixels::Yuv(Arc::new(yuv)), generation: self.generation });
        }

        // Anything else deeper than 8 bits (RGB 10/12/16-bit: DPX, 16-bit PNG and
        // TIFF, CineForm RGB, DNxHR 444 RGB): through 16-bit RGBA into half floats.
        if format_depth(fmt) > 8 {
            if let Some(f) = self.deep_rgb(src, dw, dh, space, range) {
                return Ok(VideoFrame { pts, width: f.width, height: f.height, pixels: Pixels::Float(Arc::new(f)), generation: self.generation });
            }
        }

        let key = (fmt, sw, sh, space, range);
        if self.scaler.as_ref().map(|(_, k)| *k != key).unwrap_or(true) || self.scaler_deep {
            self.scaler_deep = false;
            let mut s = Scaler::get(fmt, sw, sh, ff::format::Pixel::RGBA, dw, dh, Flags::BILINEAR)?;
            crate::color::configure(&mut s, space, range, true);
            self.scaler = Some((s, key));
        }
        let mut rgba = ff::frame::Video::empty();
        self.scaler.as_mut().ok_or_else(|| anyhow!("no scaler"))?.0.run(src, &mut rgba)?;

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

impl DecodeState {
    /// How `src`'s signal maps to light: its own tag, else the stream's
    /// (HDR10 is PQ, HLG broadcasts are HLG; anything else counts as SDR).
    fn transfer_of(&self, src: &ff::frame::Video) -> Transfer {
        use ff::ffi::AVColorTransferCharacteristic as T;
        let of = |t: T| match t {
            T::AVCOL_TRC_SMPTE2084 => Some(Transfer::Pq),
            T::AVCOL_TRC_ARIB_STD_B67 => Some(Transfer::Hlg),
            T::AVCOL_TRC_UNSPECIFIED | T::AVCOL_TRC_RESERVED | T::AVCOL_TRC_RESERVED0 => None,
            _ => Some(Transfer::Sdr),
        };
        // SAFETY: reading fields of a valid frame and of the stream's parameters.
        let frame = of(unsafe { (*src.as_ptr()).color_trc });
        frame
            .or_else(|| self.ictx.stream(self.stream_index).and_then(|st| of(unsafe { (*st.parameters().as_ptr()).color_trc })))
            .unwrap_or(Transfer::Sdr)
    }

    /// A ProRes RAW frame (a 16-bit RGGB mosaic from FFmpeg, its header read as it was sent)
    /// developed into linear half floats at `dw` × `dh`. `None` for anything else.
    fn develop_raw(&self, src: &ff::frame::Video, dw: u32, dh: u32) -> Option<FloatPixels> {
        use ff::format::Pixel as P;
        let big = match src.format() {
            P::BAYER_RGGB16LE => false,
            P::BAYER_RGGB16BE => true,
            _ => return None,
        };
        // This frame's settings (by its pts), else the latest.
        let pts = src.pts();
        let params = self.raw.iter().rev().find(|(t, _)| *t == pts && pts.is_some()).or_else(|| self.raw.back())?.1;
        let (w, h) = (src.width() as usize, src.height() as usize);
        let (stride, data) = (src.stride(0), src.data(0));
        let mut samples = vec![0u16; w * h];
        for (y, row) in samples.chunks_exact_mut(w.max(1)).enumerate() {
            let Some(line) = data.get(y * stride..y * stride + w * 2) else { break };
            for (o, b) in row.iter_mut().zip(line.chunks_exact(2)) {
                let b = [b.first().copied().unwrap_or(0), b.get(1).copied().unwrap_or(0)];
                *o = if big { u16::from_be_bytes(b) } else { u16::from_le_bytes(b) };
            }
        }
        let mosaic = crate::prores_raw::Mosaic { data: &samples, stride: w, w, h };
        Some(crate::prores_raw::develop(&mosaic, &params, dw, dh))
    }

    /// `src` (more than 8 bits, not YUV kept as YUV) as half floats, linear
    /// and premultiplied, via FFmpeg's 16-bit RGBA. `None` if FFmpeg can't
    /// make 16-bit RGBA of it (the 8-bit route is taken instead).
    fn deep_rgb(&mut self, src: &ff::frame::Video, dw: u32, dh: u32, space: ff::util::color::Space, range: ff::util::color::Range) -> Option<FloatPixels> {
        let (fmt, sw, sh) = (src.format(), src.width(), src.height());
        let key = (fmt, sw, sh, space, range);
        if self.scaler.as_ref().is_none_or(|(_, k)| *k != key) || !self.scaler_deep {
            let mut s = Scaler::get(fmt, sw, sh, ff::format::Pixel::RGBA64LE, dw, dh, Flags::BILINEAR).ok()?;
            crate::color::configure(&mut s, space, range, true);
            self.scaler = Some((s, key));
            self.scaler_deep = true;
        }
        let mut rgba = ff::frame::Video::empty();
        self.scaler.as_mut()?.0.run(src, &mut rgba).ok()?;
        Some(rgba64_to_float(rgba.data(0), rgba.stride(0), dw, dh))
    }
}

/// sRGB-encoded 16-bit code value → linear half float, for every value.
fn srgb16_to_linear_half() -> &'static [u16] {
    static LUT: std::sync::OnceLock<Vec<u16>> = std::sync::OnceLock::new();
    LUT.get_or_init(|| {
        (0..=65535u32)
            .map(|v| {
                let c = v as f64 / 65535.0;
                let l = if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) };
                reelforge_core::frame::f32_to_f16(l as f32)
            })
            .collect()
    })
}

/// 16-bit straight RGBA rows (sRGB-encoded) → half-float linear premultiplied.
pub(crate) fn rgba64_to_float(data: &[u8], stride: usize, w: u32, h: u32) -> FloatPixels {
    use reelforge_core::frame::{f16_to_f32, f32_to_f16};
    let lut = srgb16_to_linear_half();
    let (w, h) = (w as usize, h as usize);
    let mut out = vec![0u16; w * h * 4];
    let one = f32_to_f16(1.0);
    for (y, dst) in out.chunks_exact_mut(w * 4).enumerate() {
        let row = &data[y * stride..y * stride + w * 8];
        for (px, o) in row.chunks_exact(8).zip(dst.chunks_exact_mut(4)) {
            let c = |i: usize| u16::from_le_bytes([px[i * 2], px[i * 2 + 1]]);
            let a = c(3);
            if a == 65535 {
                o.copy_from_slice(&[lut[c(0) as usize], lut[c(1) as usize], lut[c(2) as usize], one]);
            } else {
                let af = a as f32 / 65535.0;
                for k in 0..3 {
                    o[k] = f32_to_f16(f16_to_f32(lut[c(k) as usize]) * af);
                }
                o[3] = f32_to_f16(af);
            }
        }
    }
    FloatPixels { width: w as u32, height: h as u32, rgba: out }
}

/// A planar float frame (FFmpeg's `gbrp`/`gbrap`/`gray` in 32- or 16-bit
/// float, as EXR decodes to) as half-float RGBA, resized to `dw`×`dh` if need
/// be (bilinear). `None` for any other format.
pub(crate) fn float_frame(src: &ff::frame::Video, dw: u32, dh: u32) -> Option<FloatPixels> {
    use ff::format::Pixel as P;
    use reelforge_core::frame::f32_to_f16;
    // Planes holding R, G, B, A (FFmpeg's order is G, B, R, A).
    let (rgb, alpha, half): ([usize; 3], Option<usize>, bool) = match src.format() {
        P::GBRPF32LE => ([2, 0, 1], None, false),
        P::GBRAPF32LE => ([2, 0, 1], Some(3), false),
        P::GRAYF32LE => ([0, 0, 0], None, false),
        P::GBRPF16LE => ([2, 0, 1], None, true),
        P::GBRAPF16LE => ([2, 0, 1], Some(3), true),
        P::GRAYF16LE => ([0, 0, 0], None, true),
        _ => return None,
    };
    let (sw, sh) = (src.width() as usize, src.height() as usize);
    let plane = |i: usize| -> (&[u8], usize) { (src.data(i), src.stride(i)) };
    let at = |(d, stride): (&[u8], usize), x: usize, y: usize| -> f32 {
        if half {
            let i = y * stride + x * 2;
            reelforge_core::frame::f16_to_f32(u16::from_le_bytes([d[i], d[i + 1]]))
        } else {
            let i = y * stride + x * 4;
            f32::from_le_bytes([d[i], d[i + 1], d[i + 2], d[i + 3]])
        }
    };
    let planes: Vec<(&[u8], usize)> = rgb.iter().chain(&alpha).map(|&i| plane(i)).collect();
    let (dw, dh) = (dw as usize, dh as usize);
    let mut out = Vec::with_capacity(dw * dh * 4);
    if (dw, dh) == (sw, sh) && half {
        // Half floats already: just interleave the planes (the bits are kept).
        let one = f32_to_f16(1.0);
        fn row(p: (&[u8], usize), y: usize, w: usize) -> &[u8] {
            &p.0[y * p.1..y * p.1 + w * 2]
        }
        for y in 0..sh {
            let (r, g, b) = (row(planes[0], y, sw), row(planes[1], y, sw), row(planes[2], y, sw));
            let a = alpha.map(|_| row(planes[3], y, sw));
            for x in 0..sw {
                let px = |c: &[u8]| u16::from_le_bytes([c[x * 2], c[x * 2 + 1]]);
                out.extend([px(r), px(g), px(b), a.map_or(one, px)]);
            }
        }
    } else if (dw, dh) == (sw, sh) {
        for y in 0..sh {
            for x in 0..sw {
                for p in &planes[..3] {
                    out.push(f32_to_f16(at(*p, x, y)));
                }
                out.push(f32_to_f16(alpha.map_or(1.0, |_| at(planes[3], x, y))));
            }
        }
    } else {
        // Smaller for the viewer: bilinear, which is fine for premultiplied values.
        let (kx, ky) = (sw as f32 / dw as f32, sh as f32 / dh as f32);
        for y in 0..dh {
            let fy = ((y as f32 + 0.5) * ky - 0.5).clamp(0.0, (sh - 1) as f32);
            let (y0, ty) = (fy as usize, fy.fract());
            let y1 = (y0 + 1).min(sh - 1);
            for x in 0..dw {
                let fx = ((x as f32 + 0.5) * kx - 0.5).clamp(0.0, (sw - 1) as f32);
                let (x0, tx) = (fx as usize, fx.fract());
                let x1 = (x0 + 1).min(sw - 1);
                let sample = |p| {
                    let top = at(p, x0, y0) * (1.0 - tx) + at(p, x1, y0) * tx;
                    let bottom = at(p, x0, y1) * (1.0 - tx) + at(p, x1, y1) * tx;
                    top * (1.0 - ty) + bottom * ty
                };
                for p in &planes[..3] {
                    out.push(f32_to_f16(sample(*p)));
                }
                out.push(f32_to_f16(alpha.map_or(1.0, |_| sample(planes[3]))));
            }
        }
    }
    Some(FloatPixels { width: dw as u32, height: dh as u32, rgba: out })
}

/// The YUV-to-RGB matrix for a colour space.
fn yuv_matrix(space: ff::util::color::Space) -> YuvMatrix {
    match space {
        ff::util::color::Space::BT709 => YuvMatrix::Bt709,
        ff::util::color::Space::BT2020NCL | ff::util::color::Space::BT2020CL => YuvMatrix::Bt2020,
        _ => YuvMatrix::Bt601,
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
