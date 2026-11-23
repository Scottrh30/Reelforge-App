//! NVIDIA RTX Video (RTX graphics cards, Windows): **Video Super Resolution** (AI
//! upscaling that also cleans up compression) and **RTX Video HDR** (SDR video turned
//! into HDR10), the effects NVIDIA's driver applies to video in browsers, here applied
//! to footage frame by frame.
//!
//! They come from NVIDIA's RTX Video SDK, which isn't open source and so isn't part of
//! this code: the Windows download carries a small library built against it
//! (`reelforge_rtxvideo.dll`, beside `reelforge.exe`, with NVIDIA's own
//! `nvngx_vsr.dll` and `nvngx_truehdr.dll`), and Reelforge loads it at run time when
//! it's there. Without it (a build from source, or another system) [`available`] says
//! no and nothing else changes.
//!
//! The library's interface (all C, our own):
//!
//! ```c
//! uint32_t rfrtx_version(void);                      // 1
//! void* rfrtx_open(uint32_t vsr, uint32_t hdr, const wchar_t* data_dir, char* err, uint32_t errlen);
//! int rfrtx_run(void* h, const uint8_t* rgba, uint32_t w, uint32_t h, uint8_t* out, uint32_t ow,
//!               uint32_t oh, uint32_t quality, const uint32_t* hdr4, char* err, uint32_t errlen);
//! void rfrtx_close(void* h);
//! ```
//!
//! `rfrtx_run` returns RGBA8 (super resolution alone) or, with HDR, 32-bit pixels of
//! 10-bit R, G, B (low bits first) and 2-bit alpha in BT.2020 with the PQ curve.

use anyhow::{bail, Result};
use std::path::PathBuf;

/// The library's name.
pub const LIBRARY: &str = "reelforge_rtxvideo.dll";

/// RTX Video HDR's look (NVIDIA's ranges and defaults).
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct HdrLook {
    /// 0–200 (100): the difference between lights and darks.
    pub contrast: u32,
    /// 0–200 (100): colour intensity.
    pub saturation: u32,
    /// 10–100 (50): average brightness.
    pub middle_gray: u32,
    /// 400–2000 nits (1000): the brightest highlights, for the screen it's meant for.
    pub peak: u32,
}

impl Default for HdrLook {
    fn default() -> Self {
        HdrLook { contrast: 100, saturation: 100, middle_gray: 50, peak: 1000 }
    }
}

impl HdrLook {
    /// Within NVIDIA's ranges.
    pub fn clamped(self) -> [u32; 4] {
        [self.contrast.min(200), self.saturation.min(200), self.middle_gray.clamp(10, 100), self.peak.clamp(400, 2000)]
    }
}

/// Where the library is: `REELFORGE_RTX_VIDEO` (a folder), else beside the program.
pub fn library_dir() -> Option<PathBuf> {
    if !cfg!(windows) {
        return None;
    }
    let dir = match std::env::var_os("REELFORGE_RTX_VIDEO") {
        Some(d) => PathBuf::from(d),
        None => std::env::current_exe().ok()?.parent()?.to_path_buf(),
    };
    dir.join(LIBRARY).is_file().then_some(dir)
}

/// The library (and NVIDIA's feature files beside it) is there: the menu offers RTX Video.
pub fn available() -> bool {
    library_dir().is_some_and(|d| d.join("nvngx_vsr.dll").is_file() || d.join("nvngx_truehdr.dll").is_file())
}

/// HDR10 pixels as `rfrtx_run` gives them (10-bit R, G, B, 2-bit alpha in 32 bits) →
/// 16-bit RGBA (opaque), for the writer.
pub fn unpack_10bit(px: &[u8]) -> Vec<u16> {
    let wide = |v: u32| ((v << 6) | (v >> 4)) as u16;
    px.chunks_exact(4)
        .flat_map(|p| {
            let v = u32::from_le_bytes([p[0], p[1], p[2], p[3]]);
            [wide(v & 0x3ff), wide((v >> 10) & 0x3ff), wide((v >> 20) & 0x3ff), u16::MAX]
        })
        .collect()
}

#[cfg(windows)]
pub use imp::Runner;

/// Without the library there's nothing to run.
#[cfg(not(windows))]
pub struct Runner;

#[cfg(not(windows))]
impl Runner {
    pub fn new(_scale: Option<(u32, u32, u32)>, _hdr: Option<HdrLook>, _w: u32, _h: u32) -> Result<Runner> {
        bail!("NVIDIA RTX Video runs on Windows only")
    }
    pub fn process(&mut self, _rgba: &[u8]) -> Result<Vec<u8>> {
        bail!("NVIDIA RTX Video runs on Windows only")
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use anyhow::anyhow;
    use std::ffi::{c_char, c_void};

    type Open = unsafe extern "C" fn(u32, u32, *const u16, *mut c_char, u32) -> *mut c_void;
    type Run = unsafe extern "C" fn(*mut c_void, *const u8, u32, u32, *mut u8, u32, u32, u32, *const u32, *mut c_char, u32) -> i32;
    type Close = unsafe extern "C" fn(*mut c_void);

    struct Lib {
        _lib: libloading::Library,
        open: Open,
        run: Run,
        close: Close,
    }

    fn lib() -> Result<&'static Lib> {
        static LIB: std::sync::OnceLock<std::result::Result<Lib, String>> = std::sync::OnceLock::new();
        LIB.get_or_init(|| load().map_err(|e| format!("{e:#}"))).as_ref().map_err(|e| anyhow!("{e}"))
    }

    fn load() -> Result<Lib> {
        let dir = library_dir().ok_or_else(|| anyhow!("NVIDIA RTX Video isn't in this copy of Reelforge"))?;
        const LOAD_WITH_ALTERED_SEARCH_PATH: u32 = 0x8;
        // SAFETY: the library that ships beside Reelforge, with NVIDIA's files next to it.
        let lib: libloading::Library = unsafe { libloading::os::windows::Library::load_with_flags(dir.join(LIBRARY), LOAD_WITH_ALTERED_SEARCH_PATH) }.map_err(|e| anyhow!("loading {LIBRARY}: {e}"))?.into();
        // SAFETY: the functions have the signatures in this module's documentation.
        unsafe {
            let version: unsafe extern "C" fn() -> u32 = *lib.get(b"rfrtx_version\0").map_err(|e| anyhow!("{e}"))?;
            if version() != 1 {
                bail!("{LIBRARY} is a different version");
            }
            let open: Open = *lib.get(b"rfrtx_open\0").map_err(|e| anyhow!("{e}"))?;
            let run: Run = *lib.get(b"rfrtx_run\0").map_err(|e| anyhow!("{e}"))?;
            let close: Close = *lib.get(b"rfrtx_close\0").map_err(|e| anyhow!("{e}"))?;
            Ok(Lib { _lib: lib, open, run, close })
        }
    }

    fn message(err: &[u8]) -> String {
        let end = err.iter().position(|&b| b == 0).unwrap_or(err.len());
        String::from_utf8_lossy(err.get(..end).unwrap_or(&[])).into_owned()
    }

    /// RTX Video at work on frames of one size.
    pub struct Runner {
        lib: &'static Lib,
        h: *mut c_void,
        w: u32,
        hgt: u32,
        out: (u32, u32),
        quality: u32,
        hdr: Option<[u32; 4]>,
    }

    impl Drop for Runner {
        fn drop(&mut self) {
            // SAFETY: made by rfrtx_open, closed once.
            unsafe { (self.lib.close)(self.h) }
        }
    }

    impl Runner {
        /// `scale`: super resolution to `(width, height)` at `quality` 1–4; `hdr`: RTX Video
        /// HDR with this look. One or both.
        pub fn new(scale: Option<(u32, u32, u32)>, hdr: Option<HdrLook>, w: u32, h: u32) -> Result<Runner> {
            if scale.is_none() && hdr.is_none() {
                bail!("nothing to do");
            }
            let lib = lib()?;
            let logs: Vec<u16> = std::env::temp_dir().join("Reelforge RTX Video").to_string_lossy().encode_utf16().chain([0]).collect();
            let _ = std::fs::create_dir_all(std::env::temp_dir().join("Reelforge RTX Video"));
            let mut err = [0u8; 512];
            // SAFETY: a nul-terminated wide path and an error buffer of the size given.
            let handle = unsafe { (lib.open)(scale.is_some() as u32, hdr.is_some() as u32, logs.as_ptr(), err.as_mut_ptr() as *mut c_char, err.len() as u32) };
            if handle.is_null() {
                bail!("NVIDIA RTX Video: {}", message(&err));
            }
            let (out, quality) = match scale {
                Some((ow, oh, q)) => ((ow, oh), q.clamp(1, 4)),
                None => ((w, h), 4),
            };
            Ok(Runner { lib, h: handle, w, hgt: h, out, quality, hdr: hdr.map(HdrLook::clamped) })
        }

        /// One frame (RGBA8, the size given) → RGBA8 at the new size, or (with HDR) packed
        /// 10-bit PQ pixels ([`unpack_10bit`]).
        pub fn process(&mut self, rgba: &[u8]) -> Result<Vec<u8>> {
            let n = (self.w as usize).saturating_mul(self.hgt as usize).saturating_mul(4);
            if rgba.len() < n {
                bail!("the frame is the wrong size");
            }
            let mut out = vec![0u8; (self.out.0 as usize).saturating_mul(self.out.1 as usize).saturating_mul(4)];
            let mut err = [0u8; 512];
            let hdr = self.hdr.unwrap_or([0; 4]);
            // SAFETY: the buffers are as big as the sizes passed say.
            let ok = unsafe { (self.lib.run)(self.h, rgba.as_ptr(), self.w, self.hgt, out.as_mut_ptr(), self.out.0, self.out.1, self.quality, hdr.as_ptr(), err.as_mut_ptr() as *mut c_char, err.len() as u32) };
            if ok == 0 {
                bail!("NVIDIA RTX Video: {}", message(&err));
            }
            Ok(out)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hdr10_pixels_unpack_and_looks_stay_in_range() {
        // R = 1023, G = 512, B = 0, alpha 3.
        let v: u32 = 1023 | (512 << 10) | (3 << 30);
        let px = unpack_10bit(&v.to_le_bytes());
        assert_eq!(px, vec![65535, 32800, 0, 65535]);
        assert_eq!(HdrLook { contrast: 900, saturation: 5, middle_gray: 1, peak: 99_999 }.clamped(), [200, 5, 10, 2000]);
        assert_eq!(HdrLook::default().clamped(), [100, 100, 50, 1000]);
        if !available() {
            assert!(Runner::new(Some((128, 72, 4)), None, 64, 36).is_err());
        }
    }
}
