//! Blackmagic RAW through Blackmagic Design's own SDK, loaded at run time.
//!
//! Nothing of the SDK is built into Reelforge: its runtime library
//! (`BlackmagicRawAPI.dll`, `libBlackmagicRawAPI.so` or `BlackmagicRawAPI.framework`, installed
//! with Blackmagic RAW Player, the Blackmagic RAW SDK or DaVinci Resolve) is opened when a
//! `.braw` file is, and its one exported function hands out the codec's COM-style interfaces,
//! called here through their vtables. The vtable layouts follow Blackmagic's public SDK manual
//! ("Blackmagic RAW SDK — Developer Information", Interface Reference: each interface's
//! methods in declaration order). Frames are decoded with the clip's own settings (as
//! Blackmagic RAW Player shows them) into 16-bit RGBA.
//!
//! Untested here: no Blackmagic RAW runtime or footage was available while writing it.

use anyhow::{anyhow, bail, Result};
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError};

type Hr = i32;
const S_OK: Hr = 0;
const E_NOINTERFACE: Hr = 0x8000_4002_u32 as i32;
const E_POINTER: Hr = 0x8000_4003_u32 as i32;

const fn fourcc(s: &[u8; 4]) -> u32 {
    (s[0] as u32) << 24 | (s[1] as u32) << 16 | (s[2] as u32) << 8 | s[3] as u32
}
const SCALE_FULL: u32 = fourcc(b"full");
const SCALE_HALF: u32 = fourcc(b"half");
const SCALE_QUARTER: u32 = fourcc(b"qrtr");
const SCALE_EIGHTH: u32 = fourcc(b"eith");
const FORMAT_RGBA_U16: u32 = fourcc(b"16al");

// Vtable slots (IUnknown takes 0 QueryInterface, 1 AddRef, 2 Release).
const RELEASE: usize = 2;
const FACTORY_CREATE_CODEC: usize = 3;
const CODEC_OPEN_CLIP: usize = 3;
const CODEC_SET_CALLBACK: usize = 5;
const CODEC_FLUSH_JOBS: usize = 8;
const CLIP_WIDTH: usize = 3;
const CLIP_HEIGHT: usize = 4;
const CLIP_FRAME_RATE: usize = 5;
const CLIP_FRAME_COUNT: usize = 6;
const CLIP_READ_FRAME: usize = 18;
const FRAME_SET_SCALE: usize = 9;
const FRAME_SET_FORMAT: usize = 11;
const FRAME_DECODE_AND_PROCESS: usize = 14;
const IMAGE_WIDTH: usize = 3;
const IMAGE_HEIGHT: usize = 4;
const IMAGE_RESOURCE: usize = 5;
const IMAGE_SIZE: usize = 8;
const JOB_SUBMIT: usize = 3;

/// A reference to one of the SDK's objects; released when dropped.
struct Com(*mut c_void);

// SAFETY: the SDK's objects are reference counted and may be used from any thread (its own
// worker threads call back with them).
unsafe impl Send for Com {}
unsafe impl Sync for Com {}

impl Com {
    fn new(p: *mut c_void) -> Option<Self> {
        // Not `then_some(Com(p))`: that would make (and drop, so Release) a null one.
        if p.is_null() { None } else { Some(Com(p)) }
    }
    /// The function in vtable slot `i`.
    ///
    /// # Safety
    /// `self` points at a live object whose vtable has at least `i + 1` slots.
    unsafe fn slot(&self, i: usize) -> *const c_void {
        // SAFETY: an interface pointer points at its vtable pointer (COM / C++ layout).
        unsafe {
            let vt = *(self.0 as *const *const *const c_void);
            *vt.add(i)
        }
    }
}

impl Drop for Com {
    fn drop(&mut self) {
        // SAFETY: slot 2 of every interface is Release.
        unsafe {
            let f: unsafe extern "system" fn(*mut c_void) -> u32 = std::mem::transmute(self.slot(RELEASE));
            f(self.0);
        }
    }
}

/// Call vtable slot `$slot` of `$com` as `fn(this, args...) -> $ret`.
macro_rules! call {
    ($com:expr, $slot:expr, fn($($t:ty),*) -> $ret:ty, $($a:expr),*) => {{
        let c: &Com = $com;
        let f: unsafe extern "system" fn(*mut c_void $(, $t)*) -> $ret = std::mem::transmute(c.slot($slot));
        f(c.0 $(, $a)*)
    }};
}

/// The SDK runtime, opened once and kept for the life of the program (its worker threads
/// make unloading unsafe).
struct Runtime {
    _lib: Option<libloading::Library>,
    create_factory: unsafe extern "system" fn() -> *mut c_void,
    strings: Strings,
}

/// The string type the SDK takes for file names: UTF-8 (Linux), BSTR (Windows), CFString (macOS).
enum Strings {
    #[allow(dead_code)]
    Utf8,
    #[allow(dead_code)]
    Bstr { _lib: libloading::Library, alloc: unsafe extern "system" fn(*const u16) -> *mut u16, free: unsafe extern "system" fn(*mut u16) },
    #[allow(dead_code)]
    CfString { _lib: libloading::Library, create: unsafe extern "C" fn(*const c_void, *const std::ffi::c_char, u32) -> *const c_void, release: unsafe extern "C" fn(*const c_void) },
}

impl Strings {
    fn load() -> Result<Self> {
        #[cfg(windows)]
        {
            // SAFETY: loading a system library and two of its documented functions.
            unsafe {
                let lib = libloading::Library::new("oleaut32.dll")?;
                let alloc = *lib.get::<unsafe extern "system" fn(*const u16) -> *mut u16>(b"SysAllocString\0")?;
                let free = *lib.get::<unsafe extern "system" fn(*mut u16)>(b"SysFreeString\0")?;
                Ok(Strings::Bstr { _lib: lib, alloc, free })
            }
        }
        #[cfg(target_os = "macos")]
        {
            // SAFETY: loading CoreFoundation and two of its documented functions.
            unsafe {
                let lib = libloading::Library::new("/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation")?;
                let create = *lib.get::<unsafe extern "C" fn(*const c_void, *const std::ffi::c_char, u32) -> *const c_void>(b"CFStringCreateWithCString\0")?;
                let release = *lib.get::<unsafe extern "C" fn(*const c_void)>(b"CFRelease\0")?;
                Ok(Strings::CfString { _lib: lib, create, release })
            }
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        Ok(Strings::Utf8)
    }

    /// Call `f` with `path` as the SDK's string type.
    fn with<R>(&self, path: &Path, f: impl FnOnce(*const c_void) -> R) -> Result<R> {
        let s = path.to_string_lossy();
        match self {
            Strings::Utf8 => {
                let c = std::ffi::CString::new(s.as_bytes()).map_err(|_| anyhow!("file name has a NUL"))?;
                Ok(f(c.as_ptr() as *const c_void))
            }
            Strings::Bstr { alloc, free, .. } => {
                let w: Vec<u16> = s.encode_utf16().chain(std::iter::once(0)).collect();
                // SAFETY: a NUL-terminated wide string; the BSTR is freed after the call.
                let b = unsafe { alloc(w.as_ptr()) };
                if b.is_null() {
                    bail!("out of memory");
                }
                let r = f(b as *const c_void);
                // SAFETY: allocated by SysAllocString above.
                unsafe { free(b) };
                Ok(r)
            }
            Strings::CfString { create, release, .. } => {
                let c = std::ffi::CString::new(s.as_bytes()).map_err(|_| anyhow!("file name has a NUL"))?;
                // SAFETY: kCFStringEncodingUTF8; released after the call.
                let cf = unsafe { create(std::ptr::null(), c.as_ptr(), 0x0800_0100) };
                if cf.is_null() {
                    bail!("couldn't make a CFString");
                }
                let r = f(cf);
                // SAFETY: created above.
                unsafe { release(cf) };
                Ok(r)
            }
        }
    }
}

/// Where the runtime library may be: `REELFORGE_BRAW_DIR` (or `BRAW_RUNTIME_DIR`), then the
/// places Blackmagic's installers put it.
fn candidates() -> Vec<PathBuf> {
    #[cfg(windows)]
    const NAME: &str = "BlackmagicRawAPI.dll";
    #[cfg(target_os = "macos")]
    const NAME: &str = "BlackmagicRawAPI.framework/BlackmagicRawAPI";
    #[cfg(not(any(windows, target_os = "macos")))]
    const NAME: &str = "libBlackmagicRawAPI.so";
    let mut dirs: Vec<PathBuf> = ["REELFORGE_BRAW_DIR", "BRAW_RUNTIME_DIR"].iter().filter_map(std::env::var_os).map(PathBuf::from).collect();
    #[cfg(windows)]
    {
        let pf: Vec<PathBuf> = ["ProgramFiles", "ProgramW6432"].iter().filter_map(std::env::var_os).map(PathBuf::from).chain([PathBuf::from(r"C:\Program Files")]).collect();
        for p in pf {
            let bm = p.join("Blackmagic Design");
            dirs.push(bm.join(r"Blackmagic RAW\Blackmagic RAW Player"));
            dirs.push(bm.join(r"Blackmagic RAW\Blackmagic RAW SDK\Win\Libraries"));
            dirs.push(bm.join(r"Blackmagic RAW\BlackmagicRAWSDK\Win\Libraries"));
            dirs.push(bm.join("DaVinci Resolve"));
        }
    }
    #[cfg(target_os = "macos")]
    {
        dirs.push("/Applications/Blackmagic RAW/Blackmagic RAW Player.app/Contents/Frameworks".into());
        dirs.push("/Applications/Blackmagic RAW/Blackmagic RAW SDK/Mac/Libraries".into());
        dirs.push("/Applications/DaVinci Resolve/DaVinci Resolve.app/Contents/Libraries".into());
        dirs.push("/Library/Frameworks".into());
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        dirs.push("/opt/resolve/libs".into());
        dirs.push("/usr/lib64/blackmagic/BlackmagicRAWSDK/Linux/Libraries".into());
        dirs.push("/usr/lib/blackmagic/BlackmagicRAWPlayer/BlackmagicRawAPI".into());
        dirs.push("/opt/blackmagic-raw/BlackmagicRawAPI".into());
        dirs.push("/usr/local/lib".into());
    }
    dirs.into_iter().map(|d| d.join(NAME)).collect()
}

fn open_library(path: &Path) -> Result<libloading::Library> {
    // SAFETY: loading Blackmagic's runtime; its initialisers are its own.
    unsafe {
        #[cfg(windows)]
        {
            // Its companion DLLs (GPU decoders, instruction set dispatch) sit beside it.
            const LOAD_WITH_ALTERED_SEARCH_PATH: u32 = 0x8;
            Ok(libloading::os::windows::Library::load_with_flags(path, LOAD_WITH_ALTERED_SEARCH_PATH)?.into())
        }
        #[cfg(not(windows))]
        {
            Ok(libloading::Library::new(path)?)
        }
    }
}

fn runtime() -> Result<&'static Runtime> {
    #[cfg(test)]
    if let Some(create) = TEST_FACTORY.get() {
        static FAKE: OnceLock<Runtime> = OnceLock::new();
        return Ok(FAKE.get_or_init(|| Runtime { _lib: None, create_factory: *create, strings: Strings::Utf8 }));
    }
    static RT: OnceLock<std::result::Result<Runtime, String>> = OnceLock::new();
    RT.get_or_init(|| {
        let mut tried = Vec::new();
        for p in candidates() {
            if !p.exists() {
                continue;
            }
            match open_library(&p) {
                Ok(lib) => {
                    // SAFETY: the runtime's one exported entry point, taking no arguments.
                    let create = match unsafe { lib.get::<unsafe extern "system" fn() -> *mut c_void>(b"CreateBlackmagicRawFactoryInstance\0") } {
                        Ok(f) => *f,
                        Err(e) => {
                            tried.push(format!("{}: {e}", p.display()));
                            continue;
                        }
                    };
                    let strings = Strings::load().map_err(|e| format!("{e:#}"))?;
                    log::info!("Blackmagic RAW runtime: {}", p.display());
                    return Ok(Runtime { _lib: Some(lib), create_factory: create, strings });
                }
                Err(e) => tried.push(format!("{}: {e}", p.display())),
            }
        }
        Err(if tried.is_empty() {
            "Blackmagic RAW isn't installed: install Blackmagic RAW Player or DaVinci Resolve (free from Blackmagic Design) to open .braw files".into()
        } else {
            format!("Blackmagic RAW's runtime didn't load ({})", tried.join("; "))
        })
    })
    .as_ref()
    .map_err(|e| anyhow!("{e}"))
}

#[cfg(test)]
static TEST_FACTORY: OnceLock<unsafe extern "system" fn() -> *mut c_void> = OnceLock::new();

/// Is the Blackmagic RAW runtime installed (and loadable)?
pub fn available() -> bool {
    runtime().is_ok()
}

// ---- The callback object the SDK reports to ------------------------------------------------

/// What the last decode produced.
#[derive(Default)]
struct Slot {
    scale: u32,
    image: Option<std::result::Result<(u32, u32, Vec<u8>), String>>,
}

#[repr(C)]
struct Callback {
    vtbl: *const CallbackVtbl,
    slot: Mutex<Slot>,
    done: Condvar,
}

#[repr(C)]
struct CallbackVtbl {
    query_interface: unsafe extern "system" fn(*mut Callback, *const c_void, *mut *mut c_void) -> Hr,
    add_ref: unsafe extern "system" fn(*mut Callback) -> u32,
    release: unsafe extern "system" fn(*mut Callback) -> u32,
    read_complete: unsafe extern "system" fn(*mut Callback, *mut c_void, Hr, *mut c_void),
    decode_complete: unsafe extern "system" fn(*mut Callback, *mut c_void, Hr),
    process_complete: unsafe extern "system" fn(*mut Callback, *mut c_void, Hr, *mut c_void),
    trim_progress: unsafe extern "system" fn(*mut Callback, *mut c_void, f32),
    trim_complete: unsafe extern "system" fn(*mut Callback, *mut c_void, Hr),
    sidecar_warning: unsafe extern "system" fn(*mut Callback, *mut c_void, *const c_void, u32, *const c_void),
    sidecar_error: unsafe extern "system" fn(*mut Callback, *mut c_void, *const c_void, u32, *const c_void),
    prepare_pipeline_complete: unsafe extern "system" fn(*mut Callback, *mut c_void, Hr),
}

// The object lives as long as its clip (it's kept alive by `Clip`); reference counting is
// left to Rust.
unsafe extern "system" fn cb_query_interface(this: *mut Callback, _iid: *const c_void, out: *mut *mut c_void) -> Hr {
    if out.is_null() {
        return E_POINTER;
    }
    if this.is_null() {
        // SAFETY: checked non-null.
        unsafe { *out = std::ptr::null_mut() };
        return E_NOINTERFACE;
    }
    // SAFETY: checked non-null.
    unsafe { *out = this as *mut c_void };
    S_OK
}
unsafe extern "system" fn cb_add_ref(_this: *mut Callback) -> u32 {
    1
}
unsafe extern "system" fn cb_release(_this: *mut Callback) -> u32 {
    1
}

fn finish(this: *mut Callback, r: std::result::Result<(u32, u32, Vec<u8>), String>) {
    if this.is_null() {
        return;
    }
    // SAFETY: `this` is the live callback object handed to SetCallback.
    let cb = unsafe { &*this };
    let mut s = cb.slot.lock().unwrap_or_else(PoisonError::into_inner);
    s.image = Some(r);
    cb.done.notify_all();
}

unsafe extern "system" fn cb_read_complete(this: *mut Callback, job: *mut c_void, result: Hr, frame: *mut c_void) {
    // Never unwind into the SDK.
    let _ = std::panic::catch_unwind(|| {
        let job = Com::new(job); // released when done with
        let scale = if this.is_null() {
            SCALE_FULL
        } else {
            // SAFETY: `this` is the live callback object.
            unsafe { &*this }.slot.lock().unwrap_or_else(PoisonError::into_inner).scale
        };
        if result != S_OK || frame.is_null() {
            finish(this, Err(format!("reading the frame failed (0x{result:08x})")));
            return;
        }
        // The frame belongs to the SDK for the length of this call: not released here.
        let frame = Com(frame);
        // SAFETY: calls on a live IBlackmagicRawFrame through its documented slots.
        let r = unsafe {
            let _ = call!(&frame, FRAME_SET_SCALE, fn(u32) -> Hr, scale);
            let _ = call!(&frame, FRAME_SET_FORMAT, fn(u32) -> Hr, FORMAT_RGBA_U16);
            let mut process: *mut c_void = std::ptr::null_mut();
            let hr = call!(&frame, FRAME_DECODE_AND_PROCESS, fn(*mut c_void, *mut c_void, *mut *mut c_void) -> Hr, std::ptr::null_mut(), std::ptr::null_mut(), &mut process);
            match Com::new(process) {
                Some(p) if hr == S_OK => {
                    let hr = call!(&p, JOB_SUBMIT, fn() -> Hr,);
                    if hr == S_OK {
                        // The job is released in ProcessComplete.
                        std::mem::forget(p);
                        Ok(())
                    } else {
                        Err(hr)
                    }
                }
                _ => Err(hr),
            }
        };
        std::mem::forget(frame);
        if let Err(hr) = r {
            finish(this, Err(format!("decoding the frame failed (0x{hr:08x})")));
        }
        drop(job);
    });
}

unsafe extern "system" fn cb_decode_complete(_this: *mut Callback, _job: *mut c_void, _result: Hr) {}

unsafe extern "system" fn cb_process_complete(this: *mut Callback, job: *mut c_void, result: Hr, image: *mut c_void) {
    let _ = std::panic::catch_unwind(|| {
        let job = Com::new(job);
        if result != S_OK || image.is_null() {
            finish(this, Err(format!("processing the frame failed (0x{result:08x})")));
            return;
        }
        let image = Com(image);
        // SAFETY: calls on a live IBlackmagicRawProcessedImage through its documented slots.
        let r = unsafe {
            let (mut w, mut h, mut size) = (0u32, 0u32, 0u32);
            let mut data: *mut c_void = std::ptr::null_mut();
            let ok = call!(&image, IMAGE_WIDTH, fn(*mut u32) -> Hr, &mut w) == S_OK
                && call!(&image, IMAGE_HEIGHT, fn(*mut u32) -> Hr, &mut h) == S_OK
                && call!(&image, IMAGE_SIZE, fn(*mut u32) -> Hr, &mut size) == S_OK
                && call!(&image, IMAGE_RESOURCE, fn(*mut *mut c_void) -> Hr, &mut data) == S_OK;
            let need = (w as usize).saturating_mul(h as usize).saturating_mul(8);
            if !ok || data.is_null() || w == 0 || h == 0 || (size as usize) < need || w > 16384 || h > 16384 {
                Err(format!("the processed frame wasn't usable ({w} x {h}, {size} bytes)"))
            } else {
                Ok((w, h, std::slice::from_raw_parts(data as *const u8, need).to_vec()))
            }
        };
        // The image is the SDK's for the length of this call.
        std::mem::forget(image);
        finish(this, r);
        drop(job);
    });
}

unsafe extern "system" fn cb_trim_progress(_this: *mut Callback, _job: *mut c_void, _p: f32) {}
unsafe extern "system" fn cb_trim_complete(_this: *mut Callback, _job: *mut c_void, _r: Hr) {}
unsafe extern "system" fn cb_sidecar(_this: *mut Callback, _clip: *mut c_void, _file: *const c_void, _line: u32, _info: *const c_void) {}
unsafe extern "system" fn cb_prepare(_this: *mut Callback, _user: *mut c_void, _r: Hr) {}

static VTBL: CallbackVtbl = CallbackVtbl {
    query_interface: cb_query_interface,
    add_ref: cb_add_ref,
    release: cb_release,
    read_complete: cb_read_complete,
    decode_complete: cb_decode_complete,
    process_complete: cb_process_complete,
    trim_progress: cb_trim_progress,
    trim_complete: cb_trim_complete,
    sidecar_warning: cb_sidecar,
    sidecar_error: cb_sidecar,
    prepare_pipeline_complete: cb_prepare,
};

// ---- Clips ---------------------------------------------------------------------------------

/// An open Blackmagic RAW clip.
pub struct Clip {
    // Dropped in this order: the clip, then the codec (which stops calling back), then the
    // callback object.
    clip: Com,
    codec: Com,
    _factory: Com,
    callback: Box<Callback>,
    pub width: u32,
    pub height: u32,
    pub fps: f32,
    pub frames: u64,
    lock: Mutex<()>,
}

// SAFETY: decoding is serialised by `lock`; the SDK's objects are thread-safe.
unsafe impl Send for Clip {}
unsafe impl Sync for Clip {}

impl Clip {
    pub fn open(path: &Path) -> Result<Arc<Clip>> {
        let rt = runtime()?;
        // SAFETY: the SDK's documented calls, on objects it handed out, each checked.
        unsafe {
            let factory = Com::new((rt.create_factory)()).ok_or_else(|| anyhow!("Blackmagic RAW's factory didn't start"))?;
            let mut codec: *mut c_void = std::ptr::null_mut();
            let hr = call!(&factory, FACTORY_CREATE_CODEC, fn(*mut *mut c_void) -> Hr, &mut codec);
            let codec = Com::new(codec).filter(|_| hr == S_OK).ok_or_else(|| anyhow!("Blackmagic RAW's codec didn't start (0x{hr:08x})"))?;
            let mut clip: *mut c_void = std::ptr::null_mut();
            let hr = rt.strings.with(path, |name| call!(&codec, CODEC_OPEN_CLIP, fn(*const c_void, *mut *mut c_void) -> Hr, name, &mut clip))?;
            let clip = Com::new(clip).filter(|_| hr == S_OK).ok_or_else(|| anyhow!("Blackmagic RAW couldn't open {} (0x{hr:08x})", path.display()))?;
            let callback = Box::new(Callback { vtbl: &VTBL, slot: Mutex::new(Slot::default()), done: Condvar::new() });
            let cb_ptr = &*callback as *const Callback as *mut c_void;
            let hr = call!(&codec, CODEC_SET_CALLBACK, fn(*mut c_void) -> Hr, cb_ptr);
            if hr != S_OK {
                bail!("Blackmagic RAW didn't take the callback (0x{hr:08x})");
            }
            let (mut w, mut h, mut fps, mut n) = (0u32, 0u32, 0f32, 0u64);
            let ok = call!(&clip, CLIP_WIDTH, fn(*mut u32) -> Hr, &mut w) == S_OK
                && call!(&clip, CLIP_HEIGHT, fn(*mut u32) -> Hr, &mut h) == S_OK
                && call!(&clip, CLIP_FRAME_RATE, fn(*mut f32) -> Hr, &mut fps) == S_OK
                && call!(&clip, CLIP_FRAME_COUNT, fn(*mut u64) -> Hr, &mut n) == S_OK;
            if !ok || w == 0 || h == 0 || w > 16384 || h > 16384 || !fps.is_finite() || fps <= 0.0 {
                bail!("Blackmagic RAW gave no usable picture size or frame rate for {}", path.display());
            }
            Ok(Arc::new(Clip { clip, codec, _factory: factory, callback, width: w, height: h, fps, frames: n, lock: Mutex::new(()) }))
        }
    }

    /// The scale the SDK decodes at to give at least `max` (the full size if `None`).
    fn scale_for(&self, max: Option<(u32, u32)>) -> u32 {
        let Some((mw, mh)) = max else { return SCALE_FULL };
        for (div, s) in [(8, SCALE_EIGHTH), (4, SCALE_QUARTER), (2, SCALE_HALF)] {
            if self.width / div >= mw.min(self.width) && self.height / div >= mh.min(self.height) {
                return s;
            }
        }
        SCALE_FULL
    }

    /// Decode frame `n` (16-bit RGBA, straight alpha): width, height, samples.
    pub fn decode(&self, n: u64, max: Option<(u32, u32)>) -> Result<(u32, u32, Vec<u8>)> {
        let _one_at_a_time = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
        {
            let mut s = self.callback.slot.lock().unwrap_or_else(PoisonError::into_inner);
            s.scale = self.scale_for(max);
            s.image = None;
        }
        // SAFETY: the SDK's documented calls on live objects.
        unsafe {
            let mut job: *mut c_void = std::ptr::null_mut();
            let hr = call!(&self.clip, CLIP_READ_FRAME, fn(u64, *mut *mut c_void) -> Hr, n.min(self.frames.saturating_sub(1)), &mut job);
            let job = Com::new(job).filter(|_| hr == S_OK).ok_or_else(|| anyhow!("Blackmagic RAW couldn't read frame {n} (0x{hr:08x})"))?;
            let hr = call!(&job, JOB_SUBMIT, fn() -> Hr,);
            if hr != S_OK {
                bail!("Blackmagic RAW refused frame {n} (0x{hr:08x})");
            }
            // Released by ReadComplete.
            std::mem::forget(job);
            // Waits for every submitted job, callbacks included.
            let _ = call!(&self.codec, CODEC_FLUSH_JOBS, fn() -> Hr,);
        }
        let mut s = self.callback.slot.lock().unwrap_or_else(PoisonError::into_inner);
        // FlushJobs returns once the callbacks ran; wait a little in case one is late.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while s.image.is_none() {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                bail!("Blackmagic RAW didn't deliver frame {n}");
            }
            s = self.callback.done.wait_timeout(s, left).unwrap_or_else(PoisonError::into_inner).0;
        }
        s.image.take().unwrap_or_else(|| Err("no frame".into())).map_err(|e| anyhow!("Blackmagic RAW: {e}"))
    }
}

impl Drop for Clip {
    fn drop(&mut self) {
        // Let anything still running finish before the callback object goes.
        // SAFETY: FlushJobs on the live codec.
        unsafe {
            let _ = call!(&self.codec, CODEC_FLUSH_JOBS, fn() -> Hr,);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fourccs_match_the_sdk() {
        assert_eq!(SCALE_FULL, 0x6675_6c6c);
        assert_eq!(FORMAT_RGBA_U16, 0x3136_616c);
    }

    /// A fake SDK, in process, laid out as the manual describes: factory, codec, clip, frames,
    /// jobs and processed images, calling back from another thread, counting references.
    mod fake {
        use super::super::*;
        use std::sync::atomic::{AtomicI64, Ordering};

        pub static LIVE: AtomicI64 = AtomicI64::new(0);

        #[repr(C)]
        struct Obj {
            vtbl: *const [*const c_void; 19],
            refs: AtomicI64,
            kind: u8,
            /// Frame index (read jobs, frames, process jobs); scale and format (frames).
            n: u64,
            scale: std::sync::Mutex<u32>,
            format: std::sync::Mutex<u32>,
            data: Vec<u8>,
            w: u32,
            h: u32,
        }
        const FACTORY: u8 = 0;
        const CODEC: u8 = 1;
        const CLIP: u8 = 2;
        const READ_JOB: u8 = 3;
        const FRAME: u8 = 4;
        const PROCESS_JOB: u8 = 5;
        const IMAGE: u8 = 6;

        static CALLBACK: std::sync::Mutex<usize> = std::sync::Mutex::new(0);
        static QUEUE: std::sync::Mutex<Vec<usize>> = std::sync::Mutex::new(Vec::new());

        fn new(kind: u8, n: u64) -> *mut c_void {
            LIVE.fetch_add(1, Ordering::SeqCst);
            let o = Box::new(Obj { vtbl: &VT.0, refs: AtomicI64::new(1), kind, n, scale: Mutex::new(SCALE_FULL), format: Mutex::new(0), data: Vec::new(), w: 0, h: 0 });
            Box::into_raw(o) as *mut c_void
        }
        unsafe fn obj<'a>(p: *mut c_void) -> &'a Obj {
            unsafe { &*(p as *const Obj) }
        }
        unsafe extern "system" fn qi(_t: *mut c_void, _i: *const c_void, o: *mut *mut c_void) -> Hr {
            unsafe { *o = std::ptr::null_mut() };
            E_NOINTERFACE
        }
        unsafe extern "system" fn add_ref(t: *mut c_void) -> u32 {
            unsafe { obj(t) }.refs.fetch_add(1, Ordering::SeqCst) as u32 + 1
        }
        unsafe extern "system" fn release(t: *mut c_void) -> u32 {
            let left = unsafe { obj(t) }.refs.fetch_sub(1, Ordering::SeqCst) - 1;
            assert!(left >= 0, "released too often");
            if left == 0 {
                LIVE.fetch_sub(1, Ordering::SeqCst);
                drop(unsafe { Box::from_raw(t as *mut Obj) });
            }
            left as u32
        }
        // Slot 3 means a different method on each kind of object.
        unsafe extern "system" fn slot3(t: *mut c_void, a: *mut c_void, b: *mut *mut c_void) -> Hr {
            let o = unsafe { obj(t) };
            match o.kind {
                FACTORY => unsafe { *(a as *mut *mut c_void) = new(CODEC, 0) },
                CODEC => {
                    let name = unsafe { std::ffi::CStr::from_ptr(a as *const std::ffi::c_char) };
                    if !name.to_string_lossy().ends_with("good.braw") {
                        return 0x8000_4005_u32 as i32;
                    }
                    unsafe { *b = new(CLIP, 0) };
                }
                CLIP => unsafe { *(a as *mut u32) = 64 },
                READ_JOB | PROCESS_JOB => QUEUE.lock().unwrap().push(t as usize), // Submit
                IMAGE => unsafe { *(a as *mut u32) = o.w },
                _ => return 0x8000_4001_u32 as i32,
            }
            S_OK
        }
        unsafe extern "system" fn slot4(t: *mut c_void, a: *mut c_void) -> Hr {
            match unsafe { obj(t) }.kind {
                CLIP => unsafe { *(a as *mut u32) = 32 },
                IMAGE => unsafe { *(a as *mut u32) = obj(t).h },
                _ => return 0x8000_4001_u32 as i32,
            }
            S_OK
        }
        unsafe extern "system" fn slot5(t: *mut c_void, a: *mut c_void) -> Hr {
            let o = unsafe { obj(t) };
            match o.kind {
                CODEC => *CALLBACK.lock().unwrap() = a as usize, // SetCallback
                CLIP => unsafe { *(a as *mut f32) = 23.976 },
                IMAGE => unsafe { *(a as *mut *const u8) = o.data.as_ptr() },
                _ => return 0x8000_4001_u32 as i32,
            }
            S_OK
        }
        unsafe extern "system" fn slot6(t: *mut c_void, a: *mut c_void) -> Hr {
            match unsafe { obj(t) }.kind {
                CLIP => unsafe { *(a as *mut u64) = 5 },
                _ => return 0x8000_4001_u32 as i32,
            }
            S_OK
        }
        unsafe extern "system" fn slot8(t: *mut c_void, a: *mut c_void) -> Hr {
            let o = unsafe { obj(t) };
            match o.kind {
                CODEC => flush(), // FlushJobs
                IMAGE => unsafe { *(a as *mut u32) = o.data.len() as u32 },
                _ => return 0x8000_4001_u32 as i32,
            }
            S_OK
        }
        unsafe extern "system" fn slot9(t: *mut c_void, v: u32) -> Hr {
            *unsafe { obj(t) }.scale.lock().unwrap() = v; // Frame::SetResolutionScale
            S_OK
        }
        unsafe extern "system" fn slot11(t: *mut c_void, v: u32) -> Hr {
            *unsafe { obj(t) }.format.lock().unwrap() = v; // Frame::SetResourceFormat
            S_OK
        }
        unsafe extern "system" fn slot14(t: *mut c_void, _a: *mut c_void, _b: *mut c_void, job: *mut *mut c_void) -> Hr {
            let o = unsafe { obj(t) };
            // The process job carries the frame's settings.
            let j = new(PROCESS_JOB, o.n);
            let jo = unsafe { &*(j as *const Obj) };
            *jo.scale.lock().unwrap() = *o.scale.lock().unwrap();
            *jo.format.lock().unwrap() = *o.format.lock().unwrap();
            unsafe { *job = j };
            S_OK
        }
        unsafe extern "system" fn slot18(t: *mut c_void, n: u64, job: *mut *mut c_void) -> Hr {
            assert_eq!(unsafe { obj(t) }.kind, CLIP);
            unsafe { *job = new(READ_JOB, n) };
            S_OK
        }
        unsafe extern "system" fn none() -> Hr {
            0x8000_4001_u32 as i32
        }

        /// Run every queued job on another thread (as the SDK's workers do), then return.
        fn flush() {
            std::thread::spawn(|| loop {
                let Some(j) = QUEUE.lock().unwrap().pop() else { break };
                let cb = *CALLBACK.lock().unwrap() as *mut Callback;
                let j = j as *mut c_void;
                let jo = unsafe { obj(j) };
                let vt = unsafe { &*(*cb).vtbl };
                if jo.kind == READ_JOB {
                    let f = new(FRAME, jo.n);
                    unsafe { (vt.read_complete)(cb, j, S_OK, f) };
                    unsafe { release(f) };
                } else {
                    let scale = *jo.scale.lock().unwrap();
                    assert_eq!(*jo.format.lock().unwrap(), FORMAT_RGBA_U16);
                    let div = match scale {
                        SCALE_HALF => 2,
                        SCALE_QUARTER => 4,
                        SCALE_EIGHTH => 8,
                        _ => 1,
                    };
                    let (w, h) = (64 / div, 32 / div);
                    let img = new(IMAGE, jo.n);
                    let io = unsafe { &mut *(img as *mut Obj) };
                    io.w = w;
                    io.h = h;
                    // Every sample is the frame number (times 1000), alpha opaque.
                    io.data = (0..w * h).flat_map(|_| [jo.n as u16 * 1000, 0, 0, 65535]).flat_map(u16::to_le_bytes).collect();
                    unsafe { (vt.process_complete)(cb, j, S_OK, img) };
                    unsafe { release(img) };
                }
            })
            .join()
            .unwrap();
        }

        struct Table([*const c_void; 19]);
        // SAFETY: a table of function addresses that never changes.
        unsafe impl std::marker::Sync for Table {}
        static VT: Table = Table([
            qi as *const c_void,
            add_ref as *const c_void,
            release as *const c_void,
            slot3 as *const c_void,
            slot4 as *const c_void,
            slot5 as *const c_void,
            slot6 as *const c_void,
            none as *const c_void,
            slot8 as *const c_void,
            slot9 as *const c_void,
            none as *const c_void,
            slot11 as *const c_void,
            none as *const c_void,
            none as *const c_void,
            slot14 as *const c_void,
            none as *const c_void,
            none as *const c_void,
            none as *const c_void,
            slot18 as *const c_void,
        ]);

        pub unsafe extern "system" fn create_factory() -> *mut c_void {
            new(FACTORY, 0)
        }
    }

    #[test]
    fn decodes_through_a_fake_sdk_and_releases_everything() {
        let _ = TEST_FACTORY.set(fake::create_factory);
        assert!(available());
        assert!(Clip::open(Path::new("/clips/bad.braw")).is_err());
        let clip = Clip::open(Path::new("/clips/good.braw")).unwrap();
        assert_eq!((clip.width, clip.height, clip.frames), (64, 32, 5));
        assert!((clip.fps - 23.976).abs() < 1e-3);
        let (w, h, data) = clip.decode(2, None).unwrap();
        assert_eq!((w, h, data.len()), (64, 32, 64 * 32 * 8));
        assert_eq!(u16::from_le_bytes([data[0], data[1]]), 2000);
        // A small preview is decoded at a reduced scale (at least the size asked for).
        let (w, h, data) = clip.decode(4, Some((16, 8))).unwrap();
        assert_eq!((w, h), (16, 8));
        assert_eq!(u16::from_le_bytes([data[0], data[1]]), 4000);
        let (w, _, _) = clip.decode(1, Some((20, 10))).unwrap();
        assert_eq!(w, 32);
        // Past the end: the last frame.
        let (_, _, data) = clip.decode(99, None).unwrap();
        assert_eq!(u16::from_le_bytes([data[0], data[1]]), 4000);
        let info = super::super::braw_info(Path::new("/clips/good.braw")).unwrap();
        assert_eq!(info.video.as_ref().map(|v| v.frame_rate), Some(reelforge_core::FrameRate::new(24000, 1001)));
        assert_eq!(info.duration, reelforge_core::Time::from_frame(5, reelforge_core::FrameRate::new(24000, 1001)));
        drop(clip);
        // Every job, frame, image, clip, codec and factory was released exactly once.
        assert_eq!(fake::LIVE.load(std::sync::atomic::Ordering::SeqCst), 0);

        // Through the app's decode thread: probing, seeking and frames as float pictures.
        let path = Path::new("/clips/good.braw");
        let info = crate::probe(path).unwrap();
        assert_eq!(info.video.as_ref().map(|v| (v.width, v.height)), Some((64, 32)));
        let rate = reelforge_core::FrameRate::new(24000, 1001);
        let mut src = crate::FrameSource::open(path, None).unwrap();
        for n in [3, 0, 4] {
            let f = src.frame_at_blocking(reelforge_core::Time::from_frame(n, rate), std::time::Duration::from_secs(5)).unwrap();
            assert_eq!((f.pts.to_frame(rate), f.width, f.height), (n, 64, 32));
            let reelforge_core::frame::Pixels::Float(px) = &f.pixels else { panic!("not float") };
            // Red is frame × 1000 (sRGB-coded), made linear.
            let red = reelforge_core::frame::f16_to_f32(px.rgba[0]);
            let want = { let c = n as f64 * 1000.0 / 65535.0; if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) } };
            assert!((red as f64 - want).abs() < 1e-3, "frame {n}: {red} vs {want}");
        }
        drop(src);
        assert_eq!(fake::LIVE.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[test]
    fn missing_runtime_is_an_error_not_a_crash() {
        if available() {
            return;
        }
        let e = Clip::open(Path::new("/nonexistent.braw")).err().map(|e| e.to_string()).unwrap_or_default();
        assert!(e.contains("Blackmagic RAW"), "{e}");
    }
}
