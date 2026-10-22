//! Diagnostics: a log file, and a report to paste into a message when something
//! goes wrong (Help → About & diagnostics).
//!
//! The log goes to the console as before and to `logs/reelforge.log` in the
//! local app data folder (`%LOCALAPPDATA%\Reelforge` on Windows). The previous
//! session's log is kept as `reelforge.previous.log`; a log stops growing at
//! 8 MB. A crash (panic) is written to it too, with where it happened.

use std::collections::VecDeque;
use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

const MAX_BYTES: u64 = 8 << 20;
/// Lines kept in memory for the diagnostics report.
const RECENT: usize = 400;

struct Logger {
    level: log::LevelFilter,
    start: Instant,
    file: Mutex<Option<(File, u64)>>,
    recent: Mutex<VecDeque<String>>,
}

static LOGGER: OnceLock<Logger> = OnceLock::new();

/// Where the logs go.
pub fn log_dir() -> PathBuf {
    reelforge_media::proxy::cache_dir().join("logs")
}

pub fn log_path() -> PathBuf {
    log_dir().join("reelforge.log")
}

/// "2026-10-22 18:05:03 UTC" from seconds since 1970.
fn utc(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let (h, m, s) = (secs / 3600 % 24, secs / 60 % 60, secs % 60);
    // Civil date from days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(mo <= 2);
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{m:02}:{s:02} UTC")
}

fn now_utc() -> String {
    utc(SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()))
}

/// Libraries that talk a lot at "info": only their warnings.
fn quiet(target: &str) -> bool {
    ["wgpu", "naga", "tao", "tauri", "wry", "hyper", "reqwest", "mio", "polling", "async_io", "symphonia"].iter().any(|n| target.starts_with(n))
}

impl log::Log for Logger {
    fn enabled(&self, m: &log::Metadata) -> bool {
        m.level() <= self.level && (m.level() <= log::Level::Warn || !quiet(m.target()))
    }

    fn log(&self, r: &log::Record) {
        if !self.enabled(r.metadata()) {
            return;
        }
        let t = self.start.elapsed().as_secs_f64();
        let line = format!("[{t:9.3}s {:<5} {}] {}", r.level(), r.target(), r.args());
        eprintln!("{line}");
        self.write_line(&line);
    }

    fn flush(&self) {
        if let Ok(mut f) = self.file.lock() {
            if let Some((f, _)) = f.as_mut() {
                let _ = f.flush();
            }
        }
    }
}

impl Logger {
    fn write_line(&self, line: &str) {
        if let Ok(mut f) = self.file.lock() {
            if let Some((file, written)) = f.as_mut() {
                if *written < MAX_BYTES {
                    let _ = writeln!(file, "{line}");
                    *written += line.len() as u64 + 1;
                    if *written >= MAX_BYTES {
                        let _ = writeln!(file, "(the log reached {} MB; nothing more is written this session)", MAX_BYTES >> 20);
                    }
                }
            }
        }
        if let Ok(mut q) = self.recent.lock() {
            if q.len() >= RECENT {
                q.pop_front();
            }
            q.push_back(line.to_string());
        }
    }
}

/// Start logging (instead of `env_logger`). `RUST_LOG=debug` (or `trace`,
/// `warn`…) sets the level; the default is info.
pub fn init_logging() {
    let level = match std::env::var("RUST_LOG").unwrap_or_default().to_ascii_lowercase().as_str() {
        "trace" => log::LevelFilter::Trace,
        "debug" => log::LevelFilter::Debug,
        "warn" => log::LevelFilter::Warn,
        "error" => log::LevelFilter::Error,
        "off" => log::LevelFilter::Off,
        _ => log::LevelFilter::Info,
    };
    let dir = log_dir();
    let file = std::fs::create_dir_all(&dir).ok().and_then(|_| {
        let path = log_path();
        let _ = std::fs::rename(&path, dir.join("reelforge.previous.log"));
        File::create(&path).ok().map(|f| (f, 0u64))
    });
    let logger = LOGGER.get_or_init(|| Logger { level, start: Instant::now(), file: Mutex::new(file), recent: Mutex::new(VecDeque::new()) });
    if log::set_logger(logger).is_ok() {
        log::set_max_level(level);
    }
    logger.write_line(&format!("Reelforge {} started {} ({} {})", env!("CARGO_PKG_VERSION"), now_utc(), std::env::consts::OS, std::env::consts::ARCH));

    // Crashes go to the log too, with where they happened.
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current().name().unwrap_or("unnamed").to_string();
        let msg = info.payload().downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| info.payload().downcast_ref::<String>().cloned()).unwrap_or_default();
        let at = info.location().map(|l| format!("{}:{}", l.file(), l.line())).unwrap_or_default();
        if let Some(l) = LOGGER.get() {
            l.write_line(&format!("PANIC in thread '{thread}' at {at}: {msg}"));
            l.write_line(&format!("{}", std::backtrace::Backtrace::force_capture()));
            log::Log::flush(l);
        }
        previous(info);
    }));
}

/// The last lines logged this session.
pub fn recent_lines() -> Vec<String> {
    LOGGER.get().and_then(|l| l.recent.lock().ok().map(|q| q.iter().cloned().collect())).unwrap_or_default()
}

/// What the operating system says it is.
fn os_name() -> String {
    #[cfg(windows)]
    {
        // "Microsoft Windows [Version 10.0.22631.4317]"
        use std::os::windows::process::CommandExt;
        const NO_WINDOW: u32 = 0x0800_0000;
        if let Ok(o) = std::process::Command::new("cmd").args(["/C", "ver"]).creation_flags(NO_WINDOW).output() {
            let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
            if !s.is_empty() {
                return s;
            }
        }
        "Windows".into()
    }
    #[cfg(not(windows))]
    {
        std::fs::read_to_string("/etc/os-release")
            .ok()
            .and_then(|s| s.lines().find_map(|l| l.strip_prefix("PRETTY_NAME=").map(|v| v.trim_matches('"').to_string())))
            .unwrap_or_else(|| std::env::consts::OS.into())
    }
}

/// The processor's name, cores and the vector instructions the decoders use.
fn cpu() -> String {
    let name = std::env::var("PROCESSOR_IDENTIFIER").ok().or_else(|| {
        std::fs::read_to_string("/proc/cpuinfo").ok().and_then(|s| s.lines().find_map(|l| l.strip_prefix("model name").map(|v| v.trim_start_matches([' ', '\t', ':']).to_string())))
    });
    let cores = std::thread::available_parallelism().map_or(0, |n| n.get());
    #[allow(unused_mut)]
    let mut feats: Vec<&str> = Vec::new();
    #[cfg(target_arch = "x86_64")]
    {
        for (f, on) in [
            ("SSE4.1", std::is_x86_feature_detected!("sse4.1")),
            ("AVX", std::is_x86_feature_detected!("avx")),
            ("AVX2", std::is_x86_feature_detected!("avx2")),
            ("FMA", std::is_x86_feature_detected!("fma")),
            ("AVX-512", std::is_x86_feature_detected!("avx512f")),
        ] {
            if on {
                feats.push(f);
            }
        }
    }
    format!("{} · {cores} threads · {}", name.unwrap_or_else(|| "unknown".into()), if feats.is_empty() { "no SSE4.1/AVX".into() } else { feats.join(" ") })
}

/// The report for Help → Copy diagnostics.
pub fn report(e: &reelforge_engine::editor::Editor, extra: &[(&str, String)]) -> String {
    let mut out = String::new();
    let mut line = |k: &str, v: String| out.push_str(&format!("{k}: {v}\n"));
    line("Reelforge", format!("{} ({} {}, built {})", env!("CARGO_PKG_VERSION"), std::env::consts::OS, std::env::consts::ARCH, option_env!("REELFORGE_BUILD").unwrap_or("locally")));
    line("Report made", now_utc());
    line("System", os_name());
    line("Processor", cpu());
    line("Graphics", reelforge_render::adapter_info().unwrap_or("none found: the viewer stays blank").to_string());
    line("Hardware decoding", format!("{} — {}", reelforge_media::hw::api_name(), reelforge_media::hw::status()));
    line("Zero-copy video", reelforge_core::frame::zero_copy::status().to_string());
    line("FFmpeg", reelforge_media::ffmpeg_version());
    line("Decode workers", format!("{} shared threads", reelforge_media::decode_threads()));
    line("AVX2 decoding", if reelforge_media::codecs::idct::avx2_in_use() { "on".into() } else { "off (portable)".into() });
    for (k, v) in extra {
        line(k, v.clone());
    }
    // The project: what's in it, not what it's called.
    let s = e.seq();
    let mut codecs: Vec<String> = e.project.media.iter().filter_map(|m| m.info.video.as_ref().map(|v| format!("{} {}×{}", v.codec, v.width, v.height))).collect();
    codecs.sort();
    codecs.dedup();
    line("Project", format!("{} media files; sequence {}×{} at {:.3} fps, {} tracks", e.project.media.len(), s.width, s.height, s.frame_rate.fps(), s.tracks.len()));
    line("Video in the project", if codecs.is_empty() { "none".into() } else { codecs.join(", ") });
    line("Log file", log_path().display().to_string());
    out.push_str("\nRecent log:\n");
    for l in recent_lines().iter().rev().take(200).collect::<Vec<_>>().into_iter().rev() {
        out.push_str(l);
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn utc_dates() {
        assert_eq!(super::utc(0), "1970-01-01 00:00:00 UTC");
        assert_eq!(super::utc(951_782_400), "2000-02-29 00:00:00 UTC");
        assert_eq!(super::utc(1_792_708_271), "2026-10-22 22:31:11 UTC");
    }
}
