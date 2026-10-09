//! Audio output engine and the master playback clock.
//!
//! Three parties:
//! - the **UI** sends Play/Stop/Clips commands and reads the clock;
//! - a **mixer thread** renders the timeline into 1024-frame blocks
//!   (decoding is allowed to be slow here) and queues a few blocks ahead;
//! - the **cpal callback** (realtime: no locks, no allocation, no decoding)
//!   copies queued blocks to the device and publishes which timeline sample
//!   is reaching the speakers.
//!
//! Video follows that published position, which is what keeps picture and
//! sound in sync even if the UI hitches.
//!
//! While paused, moving the playhead *scrubs*: the mixer renders a short
//! snippet (~60 ms, faded at both ends so there are no clicks) at the new
//! position. Snippets queue back to back but the mixer only renders the next
//! one when the device has nearly run out, always for the latest position, so
//! fast drags stay responsive instead of building up a backlog.

use anyhow::{anyhow, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, OutputCallbackInfo, SampleFormat, SizedSample};
use crossbeam_channel::{bounded, select, Receiver, Sender};
use reelforge_core::{FrameRate, Time};
use reelforge_media::{MixClip, Mixer};
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering::*};
use std::sync::Arc;
use std::time::Instant;

const BLOCK_FRAMES: usize = 1024;
/// Blocks queued between mixer and device (~128 ms at 48 kHz).
const QUEUE_BLOCKS: usize = 6;
/// Length of one scrub snippet.
const SCRUB_SECS: f64 = 0.06;
/// De-click ramps at each end of a scrub snippet.
const SCRUB_RAMP_SECS: f64 = 0.004;

struct Block {
    generation: u64,
    start: i64,
    data: Vec<f32>, // interleaved stereo; usually BLOCK_FRAMES long, scrub snippets vary
    /// Scrub audio doesn't drive the playback clock.
    scrub: bool,
}

enum Cmd {
    Play { from: i64, generation: u64 },
    Stop,
    Clips(Vec<MixClip>),
    Scrub { at: i64, generation: u64 },
}

/// State the callback publishes for the UI.
struct Shared {
    generation: AtomicU64,
    /// Generation the clock values below belong to.
    clock_generation: AtomicU64,
    /// Timeline sample audible at `clock_nanos`.
    clock_sample: AtomicI64,
    /// When that was, in nanoseconds since `epoch`.
    clock_nanos: AtomicI64,
    /// Last sample handed to the device; the clock never extrapolates past it.
    written_sample: AtomicI64,
    epoch: Instant,
}

pub struct AudioEngine {
    _stream: cpal::Stream,
    shared: Arc<Shared>,
    cmd: Sender<Cmd>,
    generation: u64,
    /// Where the current playback started (timeline samples).
    from: i64,
    pub rate: u32,
    pub device_name: String,
}

impl AudioEngine {
    pub fn new() -> Result<Self> {
        let host = cpal::default_host();
        let device = host.default_output_device().ok_or_else(|| anyhow!("no audio output device"))?;
        let device_name = device.id().map(|id| id.to_string()).unwrap_or_else(|_| "default".into());
        let supported = device.default_output_config()?;
        let format = supported.sample_format();
        let config: cpal::StreamConfig = supported.into();
        let rate = config.sample_rate;

        let shared = Arc::new(Shared {
            generation: AtomicU64::new(0),
            clock_generation: AtomicU64::new(u64::MAX),
            clock_sample: AtomicI64::new(0),
            clock_nanos: AtomicI64::new(0),
            written_sample: AtomicI64::new(0),
            epoch: Instant::now(),
        });
        let (cmd_tx, cmd_rx) = bounded(64);
        let (block_tx, block_rx) = bounded::<Block>(QUEUE_BLOCKS);

        std::thread::Builder::new()
            .name("audio-mixer".into())
            .spawn(move || mixer_thread(Mixer::new(rate), cmd_rx, block_tx))?;

        let stream = match format {
            SampleFormat::F32 => build::<f32>(&device, &config, block_rx, shared.clone())?,
            SampleFormat::I16 => build::<i16>(&device, &config, block_rx, shared.clone())?,
            SampleFormat::U16 => build::<u16>(&device, &config, block_rx, shared.clone())?,
            SampleFormat::I32 => build::<i32>(&device, &config, block_rx, shared.clone())?,
            SampleFormat::F64 => build::<f64>(&device, &config, block_rx, shared.clone())?,
            other => return Err(anyhow!("unsupported device sample format {other}")),
        };
        stream.play()?;
        log::info!("audio output: {device_name} @ {rate} Hz, {} ch, {format}", config.channels);

        Ok(Self { _stream: stream, shared, cmd: cmd_tx, generation: 0, from: 0, rate, device_name })
    }

    fn sample_rate(&self) -> FrameRate {
        FrameRate::new(self.rate as i64, 1)
    }

    pub fn play(&mut self, from: Time) {
        self.generation += 1;
        self.shared.generation.store(self.generation, Release);
        let from = from.to_frame(self.sample_rate());
        self.from = from;
        let _ = self.cmd.send(Cmd::Play { from, generation: self.generation });
    }

    pub fn stop(&mut self) {
        self.generation += 1;
        self.shared.generation.store(self.generation, Release);
        let _ = self.cmd.send(Cmd::Stop);
    }

    /// Play a short snippet at `at` (call while paused, whenever the playhead moves).
    pub fn scrub(&self, at: Time) {
        let at = at.to_frame(self.sample_rate());
        let _ = self.cmd.try_send(Cmd::Scrub { at, generation: self.generation });
    }

    pub fn set_clips(&self, clips: Vec<MixClip>) {
        let _ = self.cmd.send(Cmd::Clips(clips));
    }

    /// The timeline position currently coming out of the speakers, or `None`
    /// until the first block of the current playback has been played.
    pub fn position(&self) -> Option<Time> {
        let s = &self.shared;
        if s.clock_generation.load(Acquire) != self.generation {
            return None;
        }
        let sample = s.clock_sample.load(Acquire);
        let at = s.clock_nanos.load(Acquire);
        let now = s.epoch.elapsed().as_nanos() as i64;
        let advanced = (now - at).max(0) as i128 * self.rate as i128 / 1_000_000_000;
        // Clamp: output latency can put the audible sample just before the start point.
        let pos = (sample + advanced as i64).min(s.written_sample.load(Acquire)).max(self.from);
        Some(Time::from_frame(pos, self.sample_rate()))
    }
}

fn mixer_thread(mut mixer: Mixer, cmds: Receiver<Cmd>, out: Sender<Block>) {
    let rate = mixer.rate() as f64;
    // (generation, next sample) while playing.
    let mut playing: Option<(u64, i64)> = None;
    // Latest scrub request not yet rendered.
    let mut scrub: Option<(u64, i64)> = None;
    loop {
        let Some((generation, pos)) = playing else {
            if let Some((generation, at)) = scrub {
                // Render only when the device is about to run dry, for the newest position.
                if out.len() <= 1 {
                    let _ = out.send(scrub_snippet(&mut mixer, at, generation, rate));
                    scrub = None;
                    continue;
                }
                match cmds.recv_timeout(std::time::Duration::from_millis(4)) {
                    Ok(cmd) => handle_idle(cmd, &mut mixer, &mut playing, &mut scrub),
                    Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                    Err(_) => return,
                }
                continue;
            }
            match cmds.recv() {
                Ok(cmd) => handle_idle(cmd, &mut mixer, &mut playing, &mut scrub),
                Err(_) => return,
            }
            continue;
        };
        scrub = None;

        let mut data = vec![0.0; BLOCK_FRAMES * 2];
        mixer.render(pos, &mut data);
        let mut block = Some(Block { generation, start: pos, data, scrub: false });
        // Wait for room in the queue, staying responsive to commands.
        while let Some(b) = block.take() {
            select! {
                send(out, b) -> r => {
                    if r.is_err() { return }
                    playing = Some((generation, pos + BLOCK_FRAMES as i64));
                }
                recv(cmds) -> cmd => match cmd {
                    Ok(Cmd::Play { from, generation }) => playing = Some((generation, from)),
                    Ok(Cmd::Stop) => playing = None,
                    Ok(Cmd::Scrub { .. }) => {} // ignored while playing
                    Ok(Cmd::Clips(c)) => {
                        mixer.set_clips(c);
                        // Re-render this block with the new edit and keep going.
                        let mut data = vec![0.0; BLOCK_FRAMES * 2];
                        mixer.render(pos, &mut data);
                        block = Some(Block { generation, start: pos, data, scrub: false });
                    }
                    Err(_) => return,
                },
            }
        }
    }
}

fn handle_idle(cmd: Cmd, mixer: &mut Mixer, playing: &mut Option<(u64, i64)>, scrub: &mut Option<(u64, i64)>) {
    match cmd {
        Cmd::Play { from, generation } => *playing = Some((generation, from)),
        Cmd::Stop => *scrub = None,
        Cmd::Clips(c) => mixer.set_clips(c),
        Cmd::Scrub { at, generation } => *scrub = Some((generation, at)), // newest wins
    }
}

/// A short slice of the mix at `at`, ramped in and out so back-to-back snippets don't click.
fn scrub_snippet(mixer: &mut Mixer, at: i64, generation: u64, rate: f64) -> Block {
    let n = (SCRUB_SECS * rate) as usize;
    let ramp = ((SCRUB_RAMP_SECS * rate) as usize).max(1);
    let mut data = vec![0.0; n * 2];
    mixer.render(at, &mut data);
    for i in 0..ramp.min(n / 2) {
        let g = 0.5 - 0.5 * (std::f32::consts::PI * i as f32 / ramp as f32).cos();
        for k in [i, n - 1 - i] {
            data[k * 2] *= g;
            data[k * 2 + 1] *= g;
        }
    }
    Block { generation, start: at, data, scrub: true }
}

fn build<T>(device: &cpal::Device, config: &cpal::StreamConfig, blocks: Receiver<Block>, shared: Arc<Shared>) -> Result<cpal::Stream>
where
    T: SizedSample + FromSample<f32>,
{
    let channels = config.channels as usize;
    let rate = config.sample_rate as i64;
    let mut current: Option<Block> = None;
    let mut offset = 0usize;

    let callback = move |data: &mut [T], info: &OutputCallbackInfo| {
        let generation = shared.generation.load(Acquire);
        let mut first: Option<i64> = None;
        let mut last: Option<i64> = None;

        for frame in data.chunks_mut(channels) {
            // Make sure we have a current, non-stale, non-exhausted block.
            loop {
                match &current {
                    Some(b) if b.generation == generation && offset * 2 < b.data.len() => break,
                    _ => {}
                }
                current = None;
                offset = 0;
                match blocks.try_recv() {
                    Ok(b) if b.generation == generation => current = Some(b),
                    Ok(_stale) => continue, // dropped here; freeing a Vec is acceptable at this size
                    Err(_) => break,        // underrun or stopped: play silence
                }
            }

            let (l, r) = match &current {
                Some(b) => {
                    if !b.scrub {
                        let pos = b.start + offset as i64;
                        first.get_or_insert(pos);
                        last = Some(pos);
                    }
                    let lr = (b.data[offset * 2], b.data[offset * 2 + 1]);
                    offset += 1;
                    lr
                }
                None => (0.0, 0.0),
            };
            match channels {
                1 => frame[0] = T::from_sample(0.5 * (l + r)),
                _ => {
                    frame[0] = T::from_sample(l);
                    frame[1] = T::from_sample(r);
                    for c in &mut frame[2..] {
                        *c = T::from_sample(0.0f32);
                    }
                }
            }
        }

        if let (Some(first), Some(last)) = (first, last) {
            // `first` reaches the speakers at `playback`; work out what's audible right now.
            let ts = info.timestamp();
            let latency = ts.playback.checked_duration_since(ts.callback).unwrap_or_default();
            let latency_samples = (latency.as_nanos() as i64 * rate) / 1_000_000_000;
            shared.clock_sample.store(first - latency_samples, Release);
            shared.clock_nanos.store(shared.epoch.elapsed().as_nanos() as i64, Release);
            shared.written_sample.store(last, Release);
            shared.clock_generation.store(generation, Release);
        }
    };

    // Some drivers report the same recoverable error on every callback; don't flood the log.
    let mut errors = 0u64;
    let err = move |e: cpal::Error| {
        errors += 1;
        if errors <= 3 || errors.is_power_of_two() {
            log::warn!("audio stream ({errors} errors so far): {e}");
        }
    };
    Ok(device.build_output_stream(*config, callback, err, None)?)
}
