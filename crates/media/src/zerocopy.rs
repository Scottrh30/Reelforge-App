//! Zero-copy video, the decoder's half (Windows, D3D11VA).
//!
//! A hardware-decoded frame lives in one slice of FFmpeg's decoder texture
//! array, which can't be shared. So each frame is copied, within video memory,
//! into a texture of our own made for sharing (NV12 or P010, with an NT
//! handle), and a shared fence is signalled when the copy is done. The
//! compositor opens both in Direct3D 12 and reads the frame from there (see
//! `reelforge_render::interop`). The textures come from a pool, and a texture
//! goes back to the pool only once every copy of its [`GpuFrame`] is gone
//! (the compositor keeps one until the GPU has read it).
//!
//! Anything unexpected returns `None`, and the frame is copied back to memory
//! the usual way instead.

use ffmpeg_next::ffi::{AVD3D11VADeviceContext, AVFrame, AVHWFramesContext, AVPixelFormat};
use reelforge_core::frame::{zero_copy, GpuFrame, YuvLayout, YuvMatrix, YuvPlanes};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use windows::core::{Interface, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, GENERIC_ALL, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT, DXGI_FORMAT_NV12, DXGI_FORMAT_P010, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIDevice, IDXGIFactory1, IDXGIResource1, DXGI_SHARED_RESOURCE_READ, DXGI_SHARED_RESOURCE_WRITE};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

/// Video memory the shared textures may use in all (beyond that, frames are copied back as usual).
const POOL_BYTES: u64 = 640 << 20;
/// How long to wait for the copy into a shared texture before giving up on it.
const COPY_TIMEOUT_MS: u32 = 250;

/// A LUID as one number (the same packing the compositor uses).
fn luid_u64(l: windows::Win32::Foundation::LUID) -> u64 {
    ((l.HighPart as u32 as u64) << 32) | l.LowPart as u64
}

/// The index of the DXGI adapter with this LUID, to create FFmpeg's D3D11 device
/// on the same graphics card as the compositor.
pub fn adapter_index(luid: u64) -> Option<u32> {
    // SAFETY: plain DXGI enumeration.
    unsafe {
        let f: IDXGIFactory1 = CreateDXGIFactory1().ok()?;
        (0..16).find(|&i| f.EnumAdapters1(i).ok().and_then(|a| a.GetDesc1().ok()).is_some_and(|d| luid_u64(d.AdapterLuid) == luid))
    }
}

/// One shared texture.
struct Slot {
    texture: ID3D11Texture2D,
    handle: HANDLE,
    id: u64,
}
// SAFETY: D3D11 device-child objects are free-threaded; the handle is just a number.
unsafe impl Send for Slot {}
unsafe impl Sync for Slot {}

impl Drop for Slot {
    fn drop(&mut self) {
        // SAFETY: our own handle, closed once.
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

#[derive(Default)]
struct Pool {
    free: Vec<Slot>,
    made: usize,
}

/// FFmpeg's D3D11 device, our fence on it, and the texture pools.
struct Shared {
    device: ID3D11Device,
    context: ID3D11DeviceContext4,
    lock: Option<unsafe extern "C" fn(*mut std::ffi::c_void)>,
    unlock: Option<unsafe extern "C" fn(*mut std::ffi::c_void)>,
    lock_ctx: usize,
    fence: ID3D11Fence,
    /// Kept open for the life of the process, so the compositor can cache what it opened.
    fence_handle: HANDLE,
    value: AtomicU64,
    luid: u64,
    pools: Mutex<HashMap<(i32, u32, u32), Pool>>,
}
// SAFETY: the device and fence are free-threaded; the immediate context is only
// used between FFmpeg's lock and unlock callbacks (its own locking).
unsafe impl Send for Shared {}
unsafe impl Sync for Shared {}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);
static SHARED: OnceLock<Option<Arc<Shared>>> = OnceLock::new();

impl Shared {
    /// Set up from the device FFmpeg decodes on (once per process; FFmpeg's
    /// device is shared by every decoder, see `hw::device`).
    unsafe fn get(dev: *const AVD3D11VADeviceContext) -> Option<Arc<Shared>> {
        SHARED
            .get_or_init(|| {
                // SAFETY: `dev` is FFmpeg's live D3D11VA device context.
                let r = unsafe { Self::make(dev) };
                if let Err(e) = &r {
                    zero_copy::note_decoder(format!("the video decoder couldn't share frames ({e})"));
                    log::warn!("zero-copy video: {e}");
                }
                r.ok().map(Arc::new)
            })
            .clone()
    }

    unsafe fn make(dev: *const AVD3D11VADeviceContext) -> Result<Shared, String> {
        // SAFETY: reading FFmpeg's device context; the COM pointers are borrowed and
        // AddRef'd by `cloned()`.
        unsafe {
            let d = &*dev;
            let device = ID3D11Device::from_raw_borrowed(&(d.device as *mut std::ffi::c_void)).ok_or("no D3D11 device")?.clone();
            let context = ID3D11DeviceContext::from_raw_borrowed(&(d.device_context as *mut std::ffi::c_void)).ok_or("no D3D11 context")?.clone();
            let context: ID3D11DeviceContext4 = context.cast().map_err(|_| "Direct3D 11.4 is needed (Windows 10 or later)")?;
            let device5: ID3D11Device5 = device.cast().map_err(|_| "Direct3D 11.4 is needed (Windows 10 or later)")?;
            let mut fence: Option<ID3D11Fence> = None;
            device5.CreateFence(0, D3D11_FENCE_FLAG_SHARED, &mut fence).map_err(|e| format!("no shared fence ({e})"))?;
            let fence = fence.ok_or("no shared fence")?;
            let fence_handle = fence.CreateSharedHandle(None, GENERIC_ALL.0, PCWSTR::null()).map_err(|e| format!("the fence can't be shared ({e})"))?;
            let luid = device.cast::<IDXGIDevice>().and_then(|x| x.GetAdapter()).and_then(|a| a.GetDesc()).map(|desc| luid_u64(desc.AdapterLuid)).map_err(|e| format!("unknown adapter ({e})"))?;
            Ok(Shared {
                device,
                context,
                lock: d.lock,
                unlock: d.unlock,
                lock_ctx: d.lock_ctx as usize,
                fence,
                fence_handle,
                value: AtomicU64::new(0),
                luid,
                pools: Mutex::new(HashMap::new()),
            })
        }
    }

    fn with_context<R>(&self, f: impl FnOnce(&ID3D11DeviceContext4) -> R) -> R {
        // SAFETY: FFmpeg's (recursive) lock protecting the immediate context.
        unsafe {
            if let Some(l) = self.lock {
                l(self.lock_ctx as *mut _);
            }
            let r = f(&self.context);
            if let Some(u) = self.unlock {
                u(self.lock_ctx as *mut _);
            }
            r
        }
    }

    /// A free shared texture like `desc` (same format and size), made if there's room.
    fn take(&self, format: DXGI_FORMAT, w: u32, h: u32) -> Option<Slot> {
        let mut pools = self.pools.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let bytes = |w: u32, h: u32| (w as u64 * h as u64 * 3 / 2) * if format == DXGI_FORMAT_P010 { 2 } else { 1 };
        let in_use: u64 = pools.iter().map(|((f, pw, ph), p)| p.made as u64 * if *f == DXGI_FORMAT_P010.0 { 2 } else { 1 } * (*pw as u64 * *ph as u64 * 3 / 2)).sum();
        let pool = pools.entry((format.0, w, h)).or_default();
        if let Some(s) = pool.free.pop() {
            return Some(s);
        }
        if in_use + bytes(w, h) > POOL_BYTES && pool.made >= 4 {
            return None; // full: this frame is copied back as usual
        }
        let desc = D3D11_TEXTURE2D_DESC {
            Width: w,
            Height: h,
            MipLevels: 1,
            ArraySize: 1,
            Format: format,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: (D3D11_RESOURCE_MISC_SHARED.0 | D3D11_RESOURCE_MISC_SHARED_NTHANDLE.0) as u32,
        };
        // SAFETY: a valid description; the texture and handle are ours.
        let made = unsafe {
            let mut t: Option<ID3D11Texture2D> = None;
            self.device.CreateTexture2D(&desc, None, Some(&mut t)).ok().and(t).and_then(|t| {
                let h = t.cast::<IDXGIResource1>().ok()?.CreateSharedHandle(None, (DXGI_SHARED_RESOURCE_READ | DXGI_SHARED_RESOURCE_WRITE).0, PCWSTR::null()).ok()?;
                Some(Slot { texture: t, handle: h, id: NEXT_ID.fetch_add(1, Ordering::Relaxed) })
            })
        };
        match made {
            Some(s) => {
                pool.made += 1;
                Some(s)
            }
            None => {
                zero_copy::note_decoder("the graphics driver wouldn't make a shareable video texture");
                None
            }
        }
    }

    fn give_back(&self, format: DXGI_FORMAT, w: u32, h: u32, slot: Slot) {
        self.pools.lock().unwrap_or_else(std::sync::PoisonError::into_inner).entry((format.0, w, h)).or_default().free.push(slot);
    }

    /// Wait (on this thread) until the fence reaches `value`.
    fn wait(&self, value: u64) -> bool {
        thread_local! {
            static EVENT: Option<HANDLE> = unsafe { CreateEventW(None, false, false, PCWSTR::null()).ok() };
        }
        // SAFETY: a live fence and our own event.
        unsafe {
            if self.fence.GetCompletedValue() >= value {
                return true;
            }
            EVENT.with(|e| match e {
                Some(e) => self.fence.SetEventOnCompletion(value, *e).is_ok() && WaitForSingleObject(*e, COPY_TIMEOUT_MS) == WAIT_OBJECT_0,
                None => false,
            })
        }
    }
}

/// Keeps a shared texture out of the pool while a frame in it is in use.
struct Hold {
    shared: Arc<Shared>,
    key: (DXGI_FORMAT, u32, u32),
    slot: Option<Slot>,
}

impl Drop for Hold {
    fn drop(&mut self) {
        if let Some(s) = self.slot.take() {
            self.shared.give_back(self.key.0, self.key.1, self.key.2, s);
        }
    }
}

/// Copy the shared texture back to memory (for anything that wants the pixels there).
fn download(hold: &Hold, layout: YuvLayout, size: [u32; 2], matrix: YuvMatrix, full_range: bool) -> Option<YuvPlanes> {
    let slot = hold.slot.as_ref()?;
    let sh = &hold.shared;
    let (format, aw, ah) = hold.key;
    let desc = D3D11_TEXTURE2D_DESC {
        Width: aw,
        Height: ah,
        MipLevels: 1,
        ArraySize: 1,
        Format: format,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_STAGING,
        BindFlags: 0,
        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
        MiscFlags: 0,
    };
    // SAFETY: a staging copy of our own texture, mapped and unmapped under FFmpeg's lock.
    unsafe {
        let mut st: Option<ID3D11Texture2D> = None;
        sh.device.CreateTexture2D(&desc, None, Some(&mut st)).ok()?;
        let st = st?;
        sh.with_context(|ctx| {
            ctx.CopyResource(&st, &slot.texture);
            let mut m = D3D11_MAPPED_SUBRESOURCE::default();
            ctx.Map(&st, 0, D3D11_MAP_READ, 0, Some(&mut m)).ok()?;
            let b = layout.sample_bytes();
            let (w, h) = (size[0] as usize, size[1] as usize);
            let pitch = m.RowPitch as usize;
            let base = m.pData as *const u8;
            let mut y = Vec::with_capacity(w * h * b);
            for r in 0..h {
                y.extend_from_slice(std::slice::from_raw_parts(base.add(r * pitch), w * b));
            }
            // The chroma plane follows the luma plane's allocated rows.
            let cbase = base.add(pitch * ah as usize);
            let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
            let mut uv = Vec::with_capacity(cw * ch * 2 * b);
            for r in 0..ch {
                uv.extend_from_slice(std::slice::from_raw_parts(cbase.add(r * pitch), cw * 2 * b));
            }
            ctx.Unmap(&st, 0);
            Some(YuvPlanes { layout, width: size[0], height: size[1], planes: vec![y, uv], matrix, full_range, transfer: Default::default() })
        })
    }
}

/// Hand over a decoded D3D11VA frame without copying it to memory: `None` if
/// that can't be done for it (the caller then downloads it as usual).
///
/// # Safety
/// `frame` must be a decoded D3D11VA hardware frame.
pub unsafe fn export(frame: *const AVFrame, size: [u32; 2], matrix: YuvMatrix, full_range: bool) -> Option<GpuFrame> {
    let renderer = zero_copy::renderer()?;
    // SAFETY: as documented; the frames and device contexts are FFmpeg's, alive with the frame.
    unsafe {
        let fr = &*frame;
        if fr.hw_frames_ctx.is_null() {
            return None;
        }
        let frames = &*((*fr.hw_frames_ctx).data as *const AVHWFramesContext);
        let layout = match frames.sw_format {
            AVPixelFormat::AV_PIX_FMT_NV12 => YuvLayout::Nv12,
            AVPixelFormat::AV_PIX_FMT_P010LE => YuvLayout::P010,
            _ => return None, // 4:2:2 / 4:4:4 hardware formats: the usual way
        };
        let dev = (*frames.device_ctx).hwctx as *const AVD3D11VADeviceContext;
        let shared = Shared::get(dev)?;
        if shared.luid != renderer {
            zero_copy::note_decoder("the video decoder is on a different graphics adapter from the compositor");
            return None;
        }
        let src_ptr = fr.data[0] as *mut std::ffi::c_void;
        let index = fr.data[1] as usize as u32;
        let src = ID3D11Texture2D::from_raw_borrowed(&src_ptr)?;
        let mut sd = D3D11_TEXTURE2D_DESC::default();
        src.GetDesc(&mut sd);
        let format = sd.Format;
        if format != DXGI_FORMAT_NV12 && format != DXGI_FORMAT_P010 {
            return None;
        }
        let slot = shared.take(format, sd.Width, sd.Height)?;
        let value = shared.with_context(|ctx| {
            ctx.CopySubresourceRegion(&slot.texture, 0, 0, 0, 0, src, index, None);
            let v = shared.value.fetch_add(1, Ordering::Relaxed) + 1;
            let ok = ctx.Signal(&shared.fence, v).is_ok();
            ctx.Flush();
            ok.then_some(v)
        });
        // The copy has to have happened before anyone reads the texture: wait for
        // it here, so the compositor's own wait can never hold it up.
        let Some(value) = value.filter(|v| shared.wait(*v)) else {
            shared.give_back(format, sd.Width, sd.Height, slot);
            zero_copy::note_decoder("copying a frame into a shared texture didn't finish in time");
            return None;
        };
        let (texture, id) = (slot.handle.0 as usize as u64, slot.id);
        let hold = Arc::new(Hold { shared: shared.clone(), key: (format, sd.Width, sd.Height), slot: Some(slot) });
        let h2 = hold.clone();
        let fetch = Arc::new(move || download(&h2, layout, size, matrix, full_range));
        Some(GpuFrame::new(
            layout,
            size,
            matrix,
            full_range,
            (texture, id, [sd.Width, sd.Height]),
            (shared.fence_handle.0 as usize as u64, value),
            shared.luid,
            hold,
            fetch,
        ))
    }
}
