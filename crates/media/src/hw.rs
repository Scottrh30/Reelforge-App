//! Hardware video decoding through FFmpeg's hwaccel API.
//!
//! The decoder runs on the GPU's video engine (D3D11VA on Windows, VAAPI on
//! Linux, VideoToolbox on macOS); each frame is then copied back to memory
//! (`av_hwframe_transfer_data`) and converted to RGBA as before. That takes
//! most of the work of decoding H.264/HEVC/VP9/AV1 off the CPU.
//!
//! Everything is best-effort: if there's no device, the codec has no hardware
//! path, or anything fails later, the decoder carries on in software. One
//! device is created per process and shared by every decoder.

use ffmpeg_next::ffi::*;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

static ENABLED: AtomicBool = AtomicBool::new(true);

/// Turn hardware decoding on or off for decoders opened from now on.
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// The hardware API this platform uses.
fn device_type() -> Option<AVHWDeviceType> {
    if cfg!(windows) {
        Some(AVHWDeviceType::AV_HWDEVICE_TYPE_D3D11VA)
    } else if cfg!(target_os = "macos") {
        Some(AVHWDeviceType::AV_HWDEVICE_TYPE_VIDEOTOOLBOX)
    } else if cfg!(target_os = "linux") {
        Some(AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI)
    } else {
        None
    }
}

pub fn api_name() -> &'static str {
    if cfg!(windows) {
        "D3D11VA"
    } else if cfg!(target_os = "macos") {
        "VideoToolbox"
    } else {
        "VAAPI"
    }
}

/// The shared device (or why there isn't one).
struct Device(*mut AVBufferRef);
// SAFETY: hardware device contexts are internally synchronised and only ever
// referenced (av_buffer_ref) from here, under the mutex.
unsafe impl Send for Device {}

static DEVICE: Mutex<Option<Result<Device, String>>> = Mutex::new(None);

/// A new reference to the shared device, creating it on first use.
fn device() -> Option<*mut AVBufferRef> {
    let mut slot = DEVICE.lock().unwrap();
    let dev = slot.get_or_insert_with(|| {
        let Some(ty) = device_type() else { return Err("not supported on this platform".into()) };
        let mut ctx: *mut AVBufferRef = std::ptr::null_mut();
        // SAFETY: valid out-pointer; null device name/options mean "the default adapter".
        let r = unsafe { av_hwdevice_ctx_create(&mut ctx, ty, std::ptr::null(), std::ptr::null_mut(), 0) };
        if r < 0 || ctx.is_null() {
            log::info!("hardware decoding unavailable ({}: error {r}); using software", api_name());
            Err(format!("no {} device", api_name()))
        } else {
            log::info!("hardware decoding: {}", api_name());
            Ok(Device(ctx))
        }
    });
    match dev {
        // SAFETY: the device buffer lives for the whole process.
        Ok(d) => Some(unsafe { av_buffer_ref(d.0) }).filter(|p| !p.is_null()),
        Err(_) => None,
    }
}

/// What the decoders are doing, for the settings window.
pub fn status() -> String {
    if !enabled() {
        return "off (software decoding)".into();
    }
    match &*DEVICE.lock().unwrap() {
        None => format!("{} (not used yet)", api_name()),
        Some(Ok(_)) => format!("{} available", api_name()),
        Some(Err(e)) => format!("unavailable: {e}; using software"),
    }
}

/// Pick the hardware format stashed in `opaque` if the decoder offers it,
/// otherwise let FFmpeg choose its software format.
unsafe extern "C" fn get_format(ctx: *mut AVCodecContext, fmts: *const AVPixelFormat) -> AVPixelFormat {
    // SAFETY: FFmpeg passes a valid context and a list terminated by AV_PIX_FMT_NONE.
    unsafe {
        let want = (*ctx).opaque as isize as i32;
        let mut p = fmts;
        while *p != AVPixelFormat::AV_PIX_FMT_NONE {
            if *p as i32 == want {
                return *p;
            }
            p = p.add(1);
        }
        avcodec_default_get_format(ctx, fmts)
    }
}

/// Set up hardware decoding on a not-yet-opened decoder context. Returns the
/// hardware pixel format its frames will have, or `None` to decode in software.
///
/// # Safety
/// `ctx` must be a valid, unopened decoder context.
pub unsafe fn attach(ctx: *mut AVCodecContext) -> Option<i32> {
    if !enabled() {
        return None;
    }
    let ty = device_type()?;
    // SAFETY: the caller guarantees `ctx` is valid; the codec and its hardware
    // configs are static FFmpeg data.
    unsafe {
        let codec = avcodec_find_decoder((*ctx).codec_id);
        if codec.is_null() {
            return None;
        }
        let mut i = 0;
        let pix_fmt = loop {
            let cfg = avcodec_get_hw_config(codec, i);
            if cfg.is_null() {
                return None; // this codec has no hardware path here
            }
            let by_device = (*cfg).methods & AV_CODEC_HW_CONFIG_METHOD_HW_DEVICE_CTX as i32 != 0;
            if by_device && (*cfg).device_type == ty {
                break (*cfg).pix_fmt;
            }
            i += 1;
        };
        let dev = device()?;
        (*ctx).hw_device_ctx = dev;
        (*ctx).opaque = pix_fmt as i32 as isize as *mut std::ffi::c_void;
        (*ctx).get_format = Some(get_format);
        Some(pix_fmt as i32)
    }
}

/// Copy a hardware frame back to memory (keeping its timestamps and colour info).
///
/// # Safety
/// `src` must be a decoded hardware frame; `dst` a valid (empty) frame.
pub unsafe fn download(src: *const AVFrame, dst: *mut AVFrame) -> bool {
    // SAFETY: as documented above.
    unsafe { av_hwframe_transfer_data(dst, src, 0) >= 0 && av_frame_copy_props(dst, src) >= 0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_reads_cleanly_and_can_be_switched_off() {
        crate::init();
        assert!(!status().is_empty());
        set_enabled(false);
        assert!(status().starts_with("off"));
        set_enabled(true);
    }
}
