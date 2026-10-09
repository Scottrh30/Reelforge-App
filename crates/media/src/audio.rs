//! Random-access audio reading: decode with FFmpeg, resample to interleaved
//! stereo `f32` at a fixed output rate, and serve any sample range on request.
//!
//! Synchronous by design — it runs on the mixer thread, never the realtime
//! audio callback.

use crate::{init, to_rational, AV_TIME_BASE};
use anyhow::{anyhow, Context as _, Result};
use ffmpeg_next as ff;
use ff::software::resampling::context::Context as Resampler;
use ff::ChannelLayout;
use reelforge_core::Rational;
use std::collections::VecDeque;
use std::path::Path;

/// Output channel count. Everything is mixed in stereo.
pub const CHANNELS: usize = 2;

pub struct AudioReader {
    ictx: ff::format::context::Input,
    decoder: ff::decoder::Audio,
    stream_index: usize,
    time_base: Rational,
    start_pts: i64,
    out_rate: u32,
    /// Resampler and the input it was made for; output channels are 1 for mono sources.
    resampler: Option<(Resampler, (ff::format::Sample, u32, u16))>,
    /// Channels the resampler outputs: mono is kept mono and duplicated in `push`,
    /// so it plays at full level on both sides (FFmpeg's upmix would drop it 3 dB).
    resampled_channels: usize,
    /// Decoded, resampled interleaved stereo samples.
    buf: VecDeque<f32>,
    /// Output-rate sample index of `buf[0]`; `None` right after a seek until a frame lands.
    buf_start: Option<i64>,
    eof: bool,
}

impl AudioReader {
    pub fn open(path: &Path, out_rate: u32) -> Result<Self> {
        init();
        let ictx = ff::format::input(path).with_context(|| format!("cannot open {}", path.display()))?;
        let st = ictx.streams().best(ff::media::Type::Audio).ok_or_else(|| anyhow!("no audio stream"))?;
        let stream_index = st.index();
        let time_base = to_rational(st.time_base());
        let start_pts = if st.start_time() == ff::ffi::AV_NOPTS_VALUE { 0 } else { st.start_time() };
        let decoder = ff::codec::context::Context::from_parameters(st.parameters())?.decoder().audio()?;
        Ok(Self {
            ictx,
            decoder,
            stream_index,
            time_base,
            start_pts,
            out_rate,
            resampler: None,
            resampled_channels: CHANNELS,
            buf: VecDeque::new(),
            buf_start: Some(0),
            eof: false,
        })
    }

    pub fn out_rate(&self) -> u32 {
        self.out_rate
    }

    fn buf_frames(&self) -> i64 {
        (self.buf.len() / CHANNELS) as i64
    }

    fn seek(&mut self, at: i64) {
        let at_us = at as i128 * AV_TIME_BASE as i128 / self.out_rate as i128;
        let start_us = self.start_pts as i128 * self.time_base.num as i128 * AV_TIME_BASE as i128 / self.time_base.den as i128;
        // Land ~100 ms early: codecs like AAC need the previous packet to decode
        // the first one cleanly (MDCT overlap), so we pre-roll and discard.
        let us = (at_us + start_us) as i64 - AV_TIME_BASE / 10;
        if let Err(e) = self.ictx.seek(us, ..us) {
            log::warn!("audio seek failed: {e}");
        }
        self.decoder.flush();
        self.resampler = None;
        self.buf.clear();
        self.buf_start = None;
        self.eof = false;
    }

    /// Fill `out` (interleaved stereo) with samples starting at output-rate sample `at`.
    /// Positions outside the media are silence.
    pub fn read(&mut self, at: i64, out: &mut [f32]) {
        let frames = (out.len() / CHANNELS) as i64;
        out.fill(0.0);
        if at < 0 {
            // Partly before the media: read the part that isn't.
            let skip = (-at).min(frames);
            if skip < frames {
                self.read(0, &mut out[skip as usize * CHANNELS..]);
            }
            return;
        }

        // Seek if the request isn't contiguous with what's buffered.
        let jump = self.out_rate as i64 / 4;
        match self.buf_start {
            Some(s) if at >= s && at <= s + self.buf_frames() + jump => {}
            _ => self.seek(at),
        }

        // Decode until we cover [at, at + frames) or run out.
        loop {
            if let Some(s) = self.buf_start {
                // Drop what's before `at` so the buffer doesn't grow forever.
                let drop = (at - s).clamp(0, self.buf_frames());
                self.buf.drain(..drop as usize * CHANNELS);
                self.buf_start = Some(s + drop);
                if s + drop + self.buf_frames() >= at + frames {
                    break;
                }
            }
            if self.eof || !self.decode_more() {
                break;
            }
        }

        let Some(s) = self.buf_start else { return };
        for i in 0..frames {
            let idx = at + i - s;
            if idx >= 0 && idx < self.buf_frames() {
                let (o, b) = (i as usize * CHANNELS, idx as usize * CHANNELS);
                out[o] = self.buf[b];
                out[o + 1] = self.buf[b + 1];
            }
        }
    }

    /// Decode at least one more frame. Returns false at end of stream.
    fn decode_more(&mut self) -> bool {
        let mut packet = ff::Packet::empty();
        loop {
            if self.receive_frames() {
                return true;
            }
            match packet.read(&mut self.ictx) {
                Ok(()) => {
                    if packet.stream() == self.stream_index {
                        let _ = self.decoder.send_packet(&packet);
                    }
                }
                Err(_) => {
                    let _ = self.decoder.send_eof();
                    self.receive_frames();
                    self.flush_resampler();
                    self.eof = true;
                    return false;
                }
            }
        }
    }

    /// Drain decoded frames into the buffer. Returns true if any arrived.
    fn receive_frames(&mut self) -> bool {
        let mut got = false;
        let mut frame = ff::frame::Audio::empty();
        while self.decoder.receive_frame(&mut frame).is_ok() {
            got = true;
            if self.buf_start.is_none() {
                let ts = frame.timestamp().or(frame.pts()).unwrap_or(self.start_pts) - self.start_pts;
                let at = ts as i128 * self.time_base.num as i128 * self.out_rate as i128 / self.time_base.den as i128;
                self.buf_start = Some(at as i64);
            }
            // Many files (e.g. WAVs without a channel mask) report an unknown layout.
            // swresample rejects frames whose layout doesn't match its setup, so
            // give mono/stereo (and anything inconsistent) the standard layout.
            let ch = frame.channels();
            if ch <= 2 || frame.channel_layout().channels() != ch as i32 {
                frame.set_channel_layout(ChannelLayout::default(ch as i32));
            }
            if let Err(e) = self.resample(&frame) {
                log::warn!("resample failed: {e}");
            }
        }
        got
    }

    fn resample(&mut self, frame: &ff::frame::Audio) -> Result<()> {
        let key = (frame.format(), frame.rate(), frame.channels());
        if self.resampler.as_ref().map(|(_, k)| *k != key).unwrap_or(true) {
            let mut layout = frame.channel_layout();
            if layout.channels() != frame.channels() as i32 {
                layout = ChannelLayout::default(frame.channels() as i32);
            }
            let out_channels = if frame.channels() == 1 { 1 } else { CHANNELS };
            let r = Resampler::get(
                frame.format(),
                layout,
                frame.rate(),
                ff::format::Sample::F32(ff::format::sample::Type::Packed),
                ChannelLayout::default(out_channels as i32),
                self.out_rate,
            )?;
            self.resampler = Some((r, key));
            self.resampled_channels = out_channels;
        }
        let capacity = frame.samples() * self.out_rate as usize / frame.rate().max(1) as usize + 256;
        let mut out = ff::frame::Audio::new(
            ff::format::Sample::F32(ff::format::sample::Type::Packed),
            capacity,
            ChannelLayout::default(self.resampled_channels as i32),
        );
        self.resampler.as_mut().unwrap().0.run(frame, &mut out)?;
        self.push(&out);
        Ok(())
    }

    fn flush_resampler(&mut self) {
        let Some((r, _)) = self.resampler.as_mut() else { return };
        let mut out = ff::frame::Audio::new(
            ff::format::Sample::F32(ff::format::sample::Type::Packed),
            4096,
            ChannelLayout::default(self.resampled_channels as i32),
        );
        if r.flush(&mut out).is_ok() {
            self.push(&out);
        }
    }

    fn push(&mut self, out: &ff::frame::Audio) {
        let ch = self.resampled_channels;
        let bytes = &out.data(0)[..out.samples() * ch * 4];
        let samples = bytes.chunks_exact(4).map(|b| f32::from_ne_bytes([b[0], b[1], b[2], b[3]]));
        if ch == 1 {
            self.buf.extend(samples.flat_map(|v| [v, v]));
        } else {
            self.buf.extend(samples);
        }
    }
}

/// Peak envelope for drawing waveforms: one value in `0..=1` per `1/buckets_per_sec` seconds.
pub fn waveform_peaks(path: &Path, buckets_per_sec: u32) -> Result<Vec<f32>> {
    // Decode at a low rate; peaks don't need full bandwidth.
    let rate = 8000;
    let mut r = AudioReader::open(path, rate)?;
    let per = (rate / buckets_per_sec).max(1) as usize;
    let mut chunk = vec![0.0f32; per * CHANNELS];
    let mut peaks = Vec::new();
    let mut at = 0i64;
    loop {
        r.read(at, &mut chunk);
        let available = r.buf_start.map_or(0, |s| s + r.buf_frames()) - at;
        peaks.push(chunk.iter().fold(0.0f32, |m, v| m.max(v.abs())).min(1.0));
        at += per as i64;
        if r.eof && available <= per as i64 {
            break;
        }
    }
    Ok(peaks)
}
