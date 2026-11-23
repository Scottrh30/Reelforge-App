//! NVIDIA's optical flow engine for optical-flow retiming: opened in the background at
//! start-up (CUDA takes a moment to wake) and handed to the renderer, which then takes
//! the motion from it instead of working it out in shaders (Settings → Renders).

use reelforge_core::FlowQuality;
use reelforge_media::nvof::{Effort, HardwareFlow};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

struct Nvof {
    engine: HardwareFlow,
    /// Said once in the log when it can't (each pair then falls back to the shaders).
    warned: AtomicBool,
}

impl reelforge_render::flowgpu::HardwareFlow for Nvof {
    fn flow(&mut self, a: &[u8], b: &[u8], w: u32, h: u32, quality: FlowQuality) -> Option<(Vec<[f32; 2]>, Vec<[f32; 2]>)> {
        let effort = match quality {
            FlowQuality::Fast => Effort::Fast,
            FlowQuality::Balanced => Effort::Medium,
            FlowQuality::Best => Effort::Slow,
        };
        // (A fault in the driver is the shaders' turn, not a crash.)
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.engine.flow(a, b, w, h, effort)));
        match r {
            Ok(Ok(v)) => Some(v),
            Ok(Err(e)) => {
                if !self.warned.swap(true, Ordering::Relaxed) {
                    log::warn!("NVIDIA optical flow engine: {e:#} (the shaders work out the motion instead)");
                }
                None
            }
            Err(_) => None,
        }
    }
}

static STATUS: OnceLock<String> = OnceLock::new();

/// Try to open the engine, in the background (once).
pub fn start() {
    if !cfg!(windows) {
        let _ = STATUS.set("Not on this system (Windows with an NVIDIA RTX card)".into());
        return;
    }
    let _ = std::thread::Builder::new().name("optical flow engine".into()).spawn(|| {
        let opened = std::panic::catch_unwind(HardwareFlow::open).unwrap_or_else(|_| Err(anyhow::anyhow!("it failed to start")));
        match opened {
            Ok(engine) => {
                let name = engine.name();
                log::info!("NVIDIA optical flow engine ready on {name}");
                reelforge_render::flowgpu::set_hardware_flow(Box::new(Nvof { engine, warned: AtomicBool::new(false) }));
                let _ = STATUS.set(format!("Ready on {name}"));
            }
            Err(e) => {
                log::info!("no NVIDIA optical flow engine: {e:#}");
                let _ = STATUS.set(format!("Not available: {e:#}"));
            }
        }
    });
}

/// What became of it (for Settings and the diagnostics).
pub fn status() -> String {
    STATUS.get().cloned().unwrap_or_else(|| "Starting…".into())
}

/// It opened (whether or not the setting uses it).
pub fn ready() -> bool {
    STATUS.get().is_some_and(|s| s.starts_with("Ready"))
}
