//! AI upscale (Media panel): a copy of a clip or picture made two or four
//! times the size by Real-ESRGAN (see `reelforge_sam::upscale`), frame by
//! frame in the background, then added to the project or put in the
//! original's place (laid out at its size, so framing, masks and keyframes
//! carry on as they were).
//!
//! The same machinery runs NVIDIA's Video Effects on RTX cards (see
//! `reelforge_media::nvfx`): background removal, background blur, super
//! resolution, upscale, artefact reduction and noise removal.

use super::Editor;
use crossbeam_channel::{unbounded, Receiver};
use reelforge_core::{MediaId, Time};
use reelforge_media::encode::{ColorOut, MovieWriter, Quality, VideoCodec, WriterSettings};
use reelforge_media::nvfx::NvEffect;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

/// The largest picture it makes (wider than this, 4× becomes 2×).
const MAX_WIDTH: u32 = 7680;

pub struct UpscaleJob {
    pub media: MediaId,
    /// What it's doing ("Upscale", "NVIDIA background removed"…), for the status line.
    pub what: String,
    /// 0–1.
    pub progress: f32,
    rx: Receiver<UpMsg>,
    cancel: Arc<AtomicBool>,
    replace: bool,
}

enum UpMsg {
    Progress(f32),
    /// The new file (`None`: stopped).
    Done(Result<Option<PathBuf>, String>),
}

/// Where the copy goes: a folder beside the original.
pub fn upscaled_path(src: &Path, factor: u32, still: bool) -> PathBuf {
    let dir = src.parent().unwrap_or(Path::new(".")).join("Reelforge upscaled");
    let stem = src.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "clip".into());
    dir.join(format!("{stem} {factor}x.{}", if still { "png" } else { "mp4" }))
}

/// Where an NVIDIA effect's copy goes: a folder beside the original (a `.mov` when it
/// keeps transparency).
pub fn nvidia_path(src: &Path, fx: NvEffect, still: bool) -> PathBuf {
    let dir = src.parent().unwrap_or(Path::new(".")).join("Reelforge NVIDIA");
    let stem = src.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "clip".into());
    let ext = if still {
        "png"
    } else if fx.alpha() {
        "mov"
    } else {
        "mp4"
    };
    dir.join(format!("{stem} {}.{ext}", fx.label()))
}

/// Turns a frame (RGBA, the source's size) into the new one (RGBA, `size`), reporting
/// how far into the frame it is.
struct Proc {
    size: (u32, u32),
    /// The frames come out as HDR10 (packed 10-bit PQ, see `reelforge_media::rtxvideo`).
    hdr: bool,
    f: Box<dyn FnMut(&[u8], &mut dyn FnMut(f32)) -> Result<Vec<u8>, String>>,
}

type MakeProc = Box<dyn FnOnce(u32, u32) -> Result<Proc, String> + Send>;

impl Editor {
    /// Make an upscaled copy of media `id` (`factor` 2 or 4) in the
    /// background; when it's done it's added to the project, or put in the
    /// original's place everywhere it's used when `replace`.
    pub fn start_upscale(&mut self, id: MediaId, factor: u32, replace: bool) {
        if self.upscale.is_some() {
            self.status = "An upscale is already running: wait for it (or stop it)".into();
            return;
        }
        let Some(m) = self.project.media(id).cloned() else { return };
        let Some(v) = m.info.video.clone() else {
            self.status = "Only footage and pictures can be upscaled".into();
            return;
        };
        let mut factor = if factor >= 4 { 4 } else { 2 };
        if v.width * factor > MAX_WIDTH {
            factor = 2;
        }
        if v.width * factor > MAX_WIDTH {
            self.status = format!("{} is already {} pixels wide: too big to upscale", m.name, v.width);
            return;
        }
        let still = self.project.is_still(id);
        let dst = upscaled_path(&m.path, factor, still);
        let make: MakeProc = Box::new(move |w, h| {
            Ok(Proc {
                size: (w.saturating_mul(factor), h.saturating_mul(factor)),
                hdr: false,
                f: Box::new(move |rgba: &[u8], p: &mut dyn FnMut(f32)| {
                    let (w, h) = (w as usize, h as usize);
                    let big = reelforge_sam::upscale::upscale(rgba, w, h, &mut *p).ok_or("the upscaling model didn't load")?;
                    Ok(reelforge_sam::upscale::shrink(&big, w * 4, h * 4, (4 / factor) as usize))
                }),
            })
        });
        let how = if reelforge_sam::gpu().is_some() { "on the graphics card" } else { "on the CPU (slow: a few seconds to a minute a frame)" };
        let status = format!("Upscaling {} {factor}× {how}…", m.name);
        self.spawn_job(id, &m, dst, still, false, make, "Upscale", status, replace);
    }

    /// Run NVIDIA's Video Effect `fx` over media `id` in the background, as an upscale
    /// does (the copy added to the project, or put in the original's place when `replace`).
    pub fn start_nvidia(&mut self, id: MediaId, fx: NvEffect, replace: bool) {
        if self.upscale.is_some() {
            self.status = "An AI job is already running: wait for it (or stop it)".into();
            return;
        }
        if !fx.installed() {
            self.status = if fx.rtx() {
                "NVIDIA RTX Video isn't in this copy of Reelforge (it comes with the Windows download)".into()
            } else {
                format!("NVIDIA's Video Effects aren't installed: get the Video Effects SDK redistributable for your RTX card from {}", reelforge_media::nvfx::DOWNLOAD)
            };
            return;
        }
        let Some(m) = self.project.media(id).cloned() else { return };
        let Some(v) = m.info.video.clone() else {
            self.status = "Only footage and pictures can go through NVIDIA's effects".into();
            return;
        };
        if let Some(why) = fx.refuses(v.width, v.height) {
            self.status = format!("NVIDIA's effect can't take {}: {why}", m.name);
            return;
        }
        if fx.out_size(v.width, v.height).0 > MAX_WIDTH {
            self.status = format!("{} would be too big ({} pixels wide)", m.name, fx.out_size(v.width, v.height).0);
            return;
        }
        let still = self.project.is_still(id);
        let dst = nvidia_path(&m.path, fx, still);
        if fx.hdr() && still {
            self.status = "RTX Video HDR is for footage: pictures stay SDR".into();
            return;
        }
        let make: MakeProc = Box::new(move |w, h| {
            let size = fx.out_size(w, h);
            let mut run: Box<dyn FnMut(&[u8]) -> anyhow::Result<Vec<u8>>> = match fx {
                NvEffect::RtxSuperResolution { quality, .. } => {
                    let mut r = reelforge_media::rtxvideo::Runner::new(Some((size.0, size.1, quality)), None, w, h).map_err(|e| format!("{e:#}"))?;
                    Box::new(move |rgba| r.process(rgba))
                }
                NvEffect::RtxHdr { look } => {
                    let mut r = reelforge_media::rtxvideo::Runner::new(None, Some(look), w, h).map_err(|e| format!("{e:#}"))?;
                    Box::new(move |rgba| r.process(rgba))
                }
                _ => {
                    let mut r = reelforge_media::nvfx::Runner::new(fx, w, h).map_err(|e| format!("{e:#}"))?;
                    Box::new(move |rgba| r.process(rgba))
                }
            };
            Ok(Proc { size, hdr: fx.hdr(), f: Box::new(move |rgba: &[u8], _p: &mut dyn FnMut(f32)| run(rgba).map_err(|e| format!("{e:#}"))) })
        });
        let status = format!("NVIDIA: {} — {}…", m.name, fx.label());
        self.spawn_job(id, &m, dst, still, fx.alpha(), make, "NVIDIA effect", status, replace);
    }

    #[allow(clippy::too_many_arguments)]
    fn spawn_job(&mut self, id: MediaId, m: &reelforge_core::project::MediaAsset, dst: PathBuf, still: bool, alpha: bool, make: MakeProc, what: &str, status: String, replace: bool) {
        let Some(v) = m.info.video.clone() else { return };
        let (tx, rx) = unbounded();
        let cancel = Arc::new(AtomicBool::new(false));
        let stop = cancel.clone();
        let info = m.info.clone();
        let src = m.path.clone();
        let rate = v.frame_rate;
        let spawned = std::thread::Builder::new().name("ai media job".into()).spawn(move || {
            // (A fault in a model or driver is reported, not a crash.)
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                run(&src, &dst, &info, rate, still, alpha, make, &stop, &mut |p| {
                    let _ = tx.send(UpMsg::Progress(p));
                })
            }))
            .unwrap_or_else(|_| Err("it went wrong part way".into()));
            let _ = tx.send(UpMsg::Done(r));
        });
        if spawned.is_err() {
            self.status = format!("Couldn't start the {}", what.to_lowercase());
            return;
        }
        self.status = status;
        self.upscale = Some(UpscaleJob { media: id, what: what.into(), progress: 0.0, rx, cancel, replace });
    }

    pub fn cancel_upscale(&mut self) {
        if let Some(j) = &self.upscale {
            j.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Take in the running upscale's news; at the end, bring the new file in.
    pub(crate) fn poll_upscale(&mut self) {
        let Some(job) = self.upscale.as_mut() else { return };
        let mut done = None;
        loop {
            match job.rx.try_recv() {
                Ok(UpMsg::Progress(p)) => job.progress = p,
                Ok(UpMsg::Done(r)) => done = Some(r),
                Err(crossbeam_channel::TryRecvError::Empty) => break,
                // (its thread gone without finishing: it couldn't start, or failed)
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    if done.is_none() {
                        done = Some(Err("it stopped unexpectedly".into()));
                    }
                    break;
                }
            }
        }
        let Some(result) = done else { return };
        let Some(job) = self.upscale.take() else { return };
        match result {
            Ok(Some(path)) => {
                let name = self.project.media(job.media).map(|m| m.name.clone()).unwrap_or_default();
                if job.replace {
                    self.replace_footage(job.media, &path, true);
                    self.status = format!("{} done ({name}): the clips now use {}", job.what, path.display());
                } else {
                    self.import(std::slice::from_ref(&path));
                    self.status = format!("{} done ({name}): added to the Media panel ({})", job.what, path.display());
                }
            }
            Ok(None) => self.status = format!("{} stopped", job.what),
            Err(e) => self.status = format!("{} failed: {e}", job.what),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run(src: &Path, dst: &Path, info: &reelforge_core::MediaInfo, rate: reelforge_core::FrameRate, still: bool, alpha: bool, make: MakeProc, stop: &AtomicBool, progress: &mut dyn FnMut(f32)) -> Result<Option<PathBuf>, String> {
    if let Some(dir) = dst.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("making {}: {e}", dir.display()))?;
    }
    let mut source = reelforge_media::FrameSource::open(src, None).map_err(|e| format!("{e:#}"))?;
    if still {
        let f = source.frame_at_blocking(Time::ZERO, Duration::from_secs(30)).ok_or("couldn't read the picture")?;
        let mut p = make(f.width, f.height)?;
        let out = (p.f)(&f.rgba(), progress)?;
        image::save_buffer(dst, &out, p.size.0, p.size.1, image::ColorType::Rgba8).map_err(|e| e.to_string())?;
        return Ok(Some(dst.to_path_buf()));
    }
    let frames = info.duration.to_frame(rate).max(1);
    let first = source.frame_at_blocking(Time::ZERO, Duration::from_secs(30)).ok_or("no frames")?;
    let (w, h) = (first.width as usize, first.height as usize);
    let mut p = make(first.width, first.height)?;
    let ext = dst.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_else(|| "mp4".into());
    let tmp = dst.with_extension(format!("partial.{ext}"));
    let audio_rate = info.audio.as_ref().map(|_| 48_000u32);
    let video = if alpha {
        VideoCodec::ProRes4444
    } else if p.hdr {
        VideoCodec::H265
    } else {
        VideoCodec::H264
    };
    let color = if p.hdr { ColorOut::HdrPq } else { ColorOut::Sdr8 };
    let mut writer = MovieWriter::create(&tmp, &WriterSettings { width: p.size.0, height: p.size.1, frame_rate: rate, quality: Quality::High, audio_rate, video: Some(video), hardware: !alpha, alpha, color, ..Default::default() })
    .map_err(|e| format!("{e:#}"))?;
    let mut audio = audio_rate.and_then(|r| reelforge_media::audio::AudioReader::open(src, r).ok());
    let fps = rate.0.num as f64 / rate.0.den.max(1) as f64;
    let blank = vec![0u8; w * h * 4];
    for i in 0..frames {
        if stop.load(Ordering::Relaxed) {
            drop(writer);
            let _ = std::fs::remove_file(&tmp);
            return Ok(None);
        }
        let t = Time::from_frame(i, rate);
        let f = source.frame_at_blocking(t, Duration::from_secs(30));
        let rgba = match &f {
            Some(f) if f.width as usize == w && f.height as usize == h => f.rgba().to_vec(),
            _ => blank.clone(),
        };
        let out = (p.f)(&rgba, &mut |k| progress((i as f32 + k) / frames as f32))?;
        progress((i + 1) as f32 / frames as f32);
        if p.hdr {
            writer.write_video_deep(&reelforge_media::rtxvideo::unpack_10bit(&out)).map_err(|e| format!("{e:#}"))?;
        } else {
            writer.write_video(&out).map_err(|e| format!("{e:#}"))?;
        }
        if let (Some(a), Some(r)) = (audio.as_mut(), audio_rate) {
            let s0 = (i as f64 * r as f64 / fps).round() as i64;
            let s1 = ((i + 1) as f64 * r as f64 / fps).round() as i64;
            let mut buf = vec![0.0f32; (s1 - s0).max(0) as usize * 2];
            a.read(s0, &mut buf);
            writer.write_audio(&buf).map_err(|e| format!("{e:#}"))?;
        }
    }
    writer.finish().map_err(|e| format!("{e:#}"))?;
    std::fs::rename(&tmp, dst).map_err(|e| e.to_string())?;
    Ok(Some(dst.to_path_buf()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use reelforge_core::project::MediaAsset;

    /// A short clip upscaled 2×: a file twice the size, with sound, added to
    /// the project; with `replace`, the clips use it at the old size.
    #[test]
    fn a_clip_is_upscaled_and_brought_in() {
        let dir = std::env::temp_dir().join(format!("rf-upscale-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("small.mp4");
        let ok = std::process::Command::new("ffmpeg")
            .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i", "testsrc2=s=64x36:r=10:d=0.3", "-f", "lavfi", "-i", "sine=f=440:d=0.3", "-pix_fmt", "yuv420p", "-shortest"])
            .arg(&f)
            .status()
            .is_ok_and(|s| s.success());
        if !ok {
            return;
        }
        let mut e = Editor::new(None, None);
        let info = reelforge_media::probe(&f).unwrap();
        e.project.media.push(MediaAsset { id: MediaId(1), path: f.clone(), name: "small.mp4".into(), info, conform: None });
        e.start_upscale(MediaId(1), 2, false);
        let t0 = std::time::Instant::now();
        while e.upscale.is_some() && t0.elapsed().as_secs() < 300 {
            e.poll_upscale();
            std::thread::sleep(Duration::from_millis(20));
        }
        let out = upscaled_path(&f, 2, false);
        assert!(out.is_file(), "{}", e.status);
        let m = e.project.media.iter().find(|m| m.path == out).expect("added").clone();
        let v = m.info.video.unwrap();
        assert_eq!((v.width, v.height), (128, 72));
        assert!(m.info.audio.is_some());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// NVIDIA's effects, where they aren't installed, say how to get them and start nothing;
    /// their copies are named for the effect (a `.mov` when transparent).
    #[test]
    fn nvidia_effects_say_what_they_need() {
        let fx = NvEffect::RemoveBackground { quality: true };
        assert_eq!(nvidia_path(Path::new("/a/b/talk.mp4"), fx, false), Path::new("/a/b/Reelforge NVIDIA/talk background removed.mov"));
        assert_eq!(nvidia_path(Path::new("/a/b/talk.mp4"), NvEffect::SuperResolution { scale: 2.0, clean: true }, false), Path::new("/a/b/Reelforge NVIDIA/talk super resolution 2x.mp4"));
        if reelforge_media::nvfx::available() {
            return;
        }
        let mut e = Editor::new(None, None);
        e.start_nvidia(MediaId(1), fx, false);
        assert!(e.upscale.is_none());
        assert!(e.status.contains("aren't installed"), "{}", e.status);
        if !reelforge_media::rtxvideo::available() {
            e.start_nvidia(MediaId(1), NvEffect::RtxHdr { look: Default::default() }, false);
            assert!(e.upscale.is_none());
            assert!(e.status.contains("RTX Video isn't in this copy"), "{}", e.status);
        }
    }
}
