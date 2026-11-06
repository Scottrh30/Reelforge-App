//! Check zero-copy video on this PC (Help → About & diagnostics): decode a
//! short sample of 8-bit H.264 and 10-bit HEVC with the hardware decoder,
//! keep the frames on the graphics card, and draw each one both ways: read
//! straight from the shared texture, and copied back to memory as usual.
//! The two pictures should be the same; the report says how alike they were,
//! how long each way took, and if the frames never reached the graphics card
//! (no hardware decoder, zero-copy switched off, another platform), why.

use reelforge_core::frame::{zero_copy, Pixels};
use reelforge_core::{BlendMode, Time, Transform};
use reelforge_media::decoder::DecoderHandle;
use reelforge_render::{wgpu, Compositor, LayerInput, PixelData};
use serde::Serialize;
use std::path::Path;
use std::time::{Duration, Instant};

/// One format's result.
#[derive(Clone, Debug, Serialize)]
pub struct ZeroCopyFormat {
    pub format: &'static str,
    /// "ok", "mismatch", "not used", "skipped" or "error".
    pub status: &'static str,
    /// Frames decoded, and how many of them stayed on the graphics card.
    pub frames: usize,
    pub on_gpu: usize,
    /// The largest and the average difference between the two ways (0–255 levels).
    pub worst: u8,
    pub mean: f64,
    /// Average time to draw a frame (and read it back): straight from the
    /// graphics card, and copied back first.
    pub ms_shared: Option<f64>,
    pub ms_copied: Option<f64>,
    pub detail: String,
}

/// The whole check.
#[derive(Clone, Debug, Serialize)]
pub struct ZeroCopyReport {
    /// "works", "mismatch", "unavailable" or "off".
    pub verdict: &'static str,
    pub summary: String,
    pub formats: Vec<ZeroCopyFormat>,
}

/// More than this (levels of 255) between the two ways counts as different.
const TOLERANCE: u8 = 3;
const FRAMES: usize = 24;

/// Run the check, drawing with a compositor on `device` (the Viewer's, so the
/// decoder's frames are shared with the right graphics card).
pub fn check_zero_copy(device: &wgpu::Device, queue: &wgpu::Queue) -> ZeroCopyReport {
    reelforge_media::init();
    if !zero_copy::enabled() {
        return ZeroCopyReport { verdict: "off", summary: "Zero-copy is switched off (Settings → Keep decoded video on the graphics card)".into(), formats: Vec::new() };
    }
    let mut c = Compositor::new(device, queue);
    if c.zero_copy_adapter().is_none() {
        let why = if cfg!(windows) { "the graphics card isn't driven through Direct3D 12 here, which zero-copy needs" } else { "zero-copy is only built for Windows (Direct3D 11 video decoding into Direct3D 12)" };
        return ZeroCopyReport { verdict: "unavailable", summary: format!("Not available: {why}"), formats: Vec::new() };
    }
    let dir = std::env::temp_dir().join(format!("reelforge-zerocopy-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let formats: Vec<ZeroCopyFormat> = [("H.264", "8-bit (NV12)"), ("HEVC 10-bit", "10-bit (P010)")].iter().enumerate().map(|(i, (f, kind))| check_format(&mut c, f, kind, &dir.join(format!("zc{i}.mp4")))).collect();
    let _ = std::fs::remove_dir_all(&dir);
    let used: Vec<&ZeroCopyFormat> = formats.iter().filter(|f| f.on_gpu > 0).collect();
    let (verdict, summary) = if formats.iter().any(|f| f.status == "mismatch") {
        ("mismatch", format!("Frames read straight from the graphics card differ from the usual copy; zero-copy has switched itself off ({})", zero_copy::status()))
    } else if used.is_empty() {
        ("unavailable", format!("Frames never stayed on the graphics card: {}", zero_copy::status()))
    } else {
        ("works", format!("Works: {} frames read straight from the graphics card matched the usual copy", used.iter().map(|f| f.on_gpu).sum::<usize>()))
    };
    let r = ZeroCopyReport { verdict, summary, formats };
    log::info!("zero-copy check: {} ({})", r.verdict, r.summary);
    *LAST.lock().unwrap_or_else(|e| e.into_inner()) = Some(r.clone());
    r
}

fn check_format(c: &mut Compositor, format: &'static str, kind: &str, path: &Path) -> ZeroCopyFormat {
    let mut r = ZeroCopyFormat { format, status: "skipped", frames: 0, on_gpu: 0, worst: 0, mean: 0.0, ms_shared: None, ms_copied: None, detail: String::new() };
    if let Err(e) = reelforge_media::selfcheck::make_sample(format, path) {
        r.detail = e;
        return r;
    }
    let dec = match DecoderHandle::spawn_with(path, None, 4, true) {
        Ok(d) => d,
        Err(e) => {
            (r.status, r.detail) = ("error", format!("couldn't open the sample: {e:#}"));
            return r;
        }
    };
    dec.seek(Time::ZERO, 1);
    let (mut sum, mut n, mut t_shared, mut t_copied) = (0.0f64, 0usize, Duration::ZERO, Duration::ZERO);
    let started = Instant::now();
    while r.frames < FRAMES && started.elapsed() < Duration::from_secs(20) {
        let Ok(f) = dec.frames.recv_timeout(Duration::from_millis(500)) else {
            if dec.reached_eof(1) {
                break;
            }
            continue;
        };
        r.frames += 1;
        let Pixels::Gpu(g) = &f.pixels else { continue };
        r.on_gpu += 1;
        let id = r.frames as u64 * 2 + if format == "H.264" { 0 } else { 1000 };
        let draw = |c: &mut Compositor, pixels: PixelData, content: u64| {
            let layer = LayerInput { key: 1, content_id: content, pixels, pixel_size: [f.width, f.height], layer_size: [f.width as f32, f.height as f32], transform: Transform::default(), blend: BlendMode::Normal, effects: Vec::new(), mask: None, wipe: None, mesh: None, space: None };
            c.render([f.width, f.height], &[layer]);
            c.read_output()
        };
        let t0 = Instant::now();
        let shared = draw(c, (&f.pixels).into(), id);
        t_shared += t0.elapsed();
        let t1 = Instant::now();
        let Some(planes) = g.check.clone().or_else(|| g.cpu()) else {
            (r.status, r.detail) = ("error", "couldn't copy a frame back from the graphics card".into());
            return r;
        };
        let copied_pixels = Pixels::Yuv(planes);
        let copied = draw(c, (&copied_pixels).into(), id + 1);
        t_copied += t1.elapsed();
        let (Some(a), Some(b)) = (shared, copied) else {
            (r.status, r.detail) = ("error", "couldn't read the picture back".into());
            return r;
        };
        for (x, y) in a.chunks_exact(4).zip(b.chunks_exact(4)) {
            for k in 0..3 {
                let d = x[k].abs_diff(y[k]);
                r.worst = r.worst.max(d);
                sum += d as f64;
            }
        }
        n += a.len() / 4 * 3;
    }
    if r.on_gpu == 0 {
        r.status = "not used";
        r.detail = format!("{} {kind} frames decoded, none kept on the graphics card: {}", r.frames, zero_copy::status());
        return r;
    }
    r.mean = sum / n.max(1) as f64;
    r.ms_shared = Some(t_shared.as_secs_f64() * 1000.0 / r.on_gpu as f64);
    r.ms_copied = Some(t_copied.as_secs_f64() * 1000.0 / r.on_gpu as f64);
    let failed = zero_copy::failure();
    r.status = if r.worst > TOLERANCE || failed.is_some() { "mismatch" } else { "ok" };
    r.detail = format!(
        "{kind}: {} of {} frames on the graphics card; largest difference {} levels (average {:.3}); {:.1} ms a frame straight from the card, {:.1} ms copied back{}",
        r.on_gpu,
        r.frames,
        r.worst,
        r.mean,
        r.ms_shared.unwrap_or(0.0),
        r.ms_copied.unwrap_or(0.0),
        failed.map(|f| format!("; {f}")).unwrap_or_default()
    );
    r
}

static LAST: std::sync::Mutex<Option<ZeroCopyReport>> = std::sync::Mutex::new(None);

/// The last check this session, for the diagnostics report.
pub fn last_summary() -> Option<String> {
    let r = LAST.lock().ok()?.clone()?;
    Some(std::iter::once(r.summary).chain(r.formats.iter().map(|f| format!("{} {}: {}", f.format, f.status, f.detail))).collect::<Vec<_>>().join("; "))
}

#[cfg(test)]
mod tests {
    /// Here (no hardware decoder, not Windows) the check says why it can't
    /// run rather than failing; on a PC with zero-copy it must find the two
    /// ways alike.
    #[test]
    fn reports_why_or_that_it_works() {
        let Some((d, q)) = reelforge_render::headless_device() else { return };
        let r = super::check_zero_copy(&d, &q);
        eprintln!("{} — {}", r.verdict, r.summary);
        assert!(["works", "unavailable", "off"].contains(&r.verdict), "{r:#?}");
        assert!(!r.summary.is_empty());
        if r.verdict == "works" {
            assert!(r.formats.iter().all(|f| f.status != "mismatch"));
        }
    }
}
