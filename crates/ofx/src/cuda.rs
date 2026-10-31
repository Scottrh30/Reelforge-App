//! CUDA for plugins that render with it (OpenFX 1.5's CUDA render), through
//! NVIDIA's driver (nvcuda.dll / libcuda.so.1, installed with the graphics
//! driver: nothing to ship). Images go to the plugin as device memory in the
//! device's primary context, which is the one the CUDA runtime that plugins
//! are built with uses, with a stream when the plugin takes one.
//!
//! `REELFORGE_CUDA_DRIVER` names another driver library to load instead (the
//! tests use a stand-in, as the machines they run on have no NVIDIA card).

use std::ffi::{c_char, c_int, c_uint, c_void, CStr};
use std::sync::OnceLock;

pub type DevicePtr = u64;
pub type Stream = *mut c_void;
type Context = *mut c_void;

#[allow(clippy::type_complexity)]
struct Fns {
    init: unsafe extern "system" fn(c_uint) -> c_int,
    device_get_count: unsafe extern "system" fn(*mut c_int) -> c_int,
    device_get: unsafe extern "system" fn(*mut c_int, c_int) -> c_int,
    device_get_name: unsafe extern "system" fn(*mut c_char, c_int, c_int) -> c_int,
    primary_retain: unsafe extern "system" fn(*mut Context, c_int) -> c_int,
    ctx_push: unsafe extern "system" fn(Context) -> c_int,
    ctx_pop: unsafe extern "system" fn(*mut Context) -> c_int,
    ctx_synchronize: unsafe extern "system" fn() -> c_int,
    mem_alloc: unsafe extern "system" fn(*mut DevicePtr, usize) -> c_int,
    mem_free: unsafe extern "system" fn(DevicePtr) -> c_int,
    htod: unsafe extern "system" fn(DevicePtr, *const c_void, usize) -> c_int,
    dtoh: unsafe extern "system" fn(*mut c_void, DevicePtr, usize) -> c_int,
    stream_create: unsafe extern "system" fn(*mut Stream, c_uint) -> c_int,
    stream_synchronize: unsafe extern "system" fn(Stream) -> c_int,
}

pub struct Cuda {
    _lib: libloading::Library,
    fns: Fns,
    context: Context,
    /// For plugins that take a stream.
    pub stream: Stream,
    /// e.g. "NVIDIA GeForce RTX 4070".
    pub name: String,
}

// SAFETY: the driver API is thread-safe; the context is pushed on whichever
// thread renders, for the length of the render.
unsafe impl Send for Cuda {}
unsafe impl Sync for Cuda {}

static CUDA: OnceLock<Result<Cuda, String>> = OnceLock::new();

fn init() -> Result<Cuda, String> {
    // SAFETY: loading NVIDIA's driver and calling it as its API says.
    unsafe {
        let custom = std::env::var_os("REELFORGE_CUDA_DRIVER");
        let lib = match &custom {
            Some(p) => libloading::Library::new(p).map_err(|e| format!("{}: {e}", p.to_string_lossy()))?,
            None => {
                let names: &[&str] = if cfg!(windows) { &["nvcuda.dll"] } else { &["libcuda.so.1", "libcuda.so"] };
                names.iter().find_map(|n| libloading::Library::new(n).ok()).ok_or("no CUDA (no NVIDIA graphics driver)")?
            }
        };
        macro_rules! f {
            ($n:literal) => {
                *lib.get($n).map_err(|_| format!("the CUDA driver has no {}", String::from_utf8_lossy(&$n[..$n.len() - 1])))?
            };
        }
        let fns = Fns {
            init: f!(b"cuInit\0"),
            device_get_count: f!(b"cuDeviceGetCount\0"),
            device_get: f!(b"cuDeviceGet\0"),
            device_get_name: f!(b"cuDeviceGetName\0"),
            primary_retain: f!(b"cuDevicePrimaryCtxRetain\0"),
            ctx_push: f!(b"cuCtxPushCurrent_v2\0"),
            ctx_pop: f!(b"cuCtxPopCurrent_v2\0"),
            ctx_synchronize: f!(b"cuCtxSynchronize\0"),
            mem_alloc: f!(b"cuMemAlloc_v2\0"),
            mem_free: f!(b"cuMemFree_v2\0"),
            htod: f!(b"cuMemcpyHtoD_v2\0"),
            dtoh: f!(b"cuMemcpyDtoH_v2\0"),
            stream_create: f!(b"cuStreamCreate\0"),
            stream_synchronize: f!(b"cuStreamSynchronize\0"),
        };
        let r = (fns.init)(0);
        if r != 0 {
            return Err(format!("CUDA wouldn't start ({r})"));
        }
        let mut n = 0;
        if (fns.device_get_count)(&mut n) != 0 || n < 1 {
            return Err("CUDA found no graphics card".into());
        }
        let mut dev = 0;
        (fns.device_get)(&mut dev, 0);
        let mut name = [0 as c_char; 256];
        (fns.device_get_name)(name.as_mut_ptr(), 255, dev);
        let name = CStr::from_ptr(name.as_ptr()).to_string_lossy().into_owned();
        let mut context = std::ptr::null_mut();
        let r = (fns.primary_retain)(&mut context, dev);
        if r != 0 || context.is_null() {
            return Err(format!("CUDA couldn't use {name} ({r})"));
        }
        (fns.ctx_push)(context);
        let mut stream = std::ptr::null_mut();
        let r = (fns.stream_create)(&mut stream, 1); // CU_STREAM_NON_BLOCKING
        let mut popped = std::ptr::null_mut();
        (fns.ctx_pop)(&mut popped);
        if r != 0 {
            return Err(format!("CUDA couldn't make a stream ({r})"));
        }
        Ok(Cuda { _lib: lib, fns, context, stream, name })
    }
}

/// CUDA (made the first time; kept for the life of the program). `Err` if there's none here.
pub fn get() -> Result<&'static Cuda, String> {
    CUDA.get_or_init(|| {
        let made = init();
        match &made {
            Ok(c) => log::info!("OpenFX: plugins can render with CUDA on {}", c.name),
            Err(e) => log::info!("OpenFX: no CUDA for plugins: {e}"),
        }
        made
    })
    .as_ref()
    .map_err(|e| e.clone())
}

pub fn available() -> Result<String, String> {
    get().map(|c| c.name.clone())
}

impl Cuda {
    /// Make the context current on this thread for `f` (and let it go after).
    pub fn with<R>(&self, f: impl FnOnce() -> R) -> R {
        // SAFETY: our primary context, pushed and popped around `f`.
        unsafe {
            (self.fns.ctx_push)(self.context);
            let r = f();
            let mut popped = std::ptr::null_mut();
            (self.fns.ctx_pop)(&mut popped);
            r
        }
    }

    /// Device memory holding `data` (or `len` bytes if `data` is `None`). Inside [`Self::with`].
    pub fn alloc(&self, len: usize, data: Option<&[u8]>) -> Result<DevicePtr, String> {
        // SAFETY: driver calls with the context current; `data` is `len` bytes when given.
        unsafe {
            let mut p: DevicePtr = 0;
            let r = (self.fns.mem_alloc)(&mut p, len.max(1));
            if r != 0 || p == 0 {
                return Err(format!("CUDA couldn't allocate {len} bytes ({r})"));
            }
            if let Some(d) = data {
                let r = (self.fns.htod)(p, d.as_ptr() as *const c_void, d.len());
                if r != 0 {
                    (self.fns.mem_free)(p);
                    return Err(format!("CUDA couldn't copy to the graphics card ({r})"));
                }
            }
            Ok(p)
        }
    }

    /// Wait for the plugin's work (on `stream`, or everything), then copy `p` into `out`.
    pub fn read(&self, p: DevicePtr, stream: Option<Stream>, out: &mut [u8]) -> Result<(), String> {
        // SAFETY: as above; `out` is at most the allocation's size (the caller's).
        unsafe {
            let r = match stream {
                Some(s) => (self.fns.stream_synchronize)(s),
                None => (self.fns.ctx_synchronize)(),
            };
            if r != 0 {
                return Err(format!("CUDA failed while rendering ({r})"));
            }
            let r = (self.fns.dtoh)(out.as_mut_ptr() as *mut c_void, p, out.len());
            if r != 0 { Err(format!("CUDA couldn't copy the result back ({r})")) } else { Ok(()) }
        }
    }

    pub fn free(&self, p: DevicePtr) {
        if p != 0 {
            // SAFETY: memory we allocated, with the context current.
            unsafe { (self.fns.mem_free)(p) };
        }
    }
}
