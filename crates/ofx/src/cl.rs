//! OpenCL for plugins that render with it (OpenFX 1.5's OpenCL render): the
//! system's OpenCL (OpenCL.dll / libOpenCL.so.1, whatever drivers are
//! installed), one context and in-order command queue on the best device (a
//! graphics card if there is one), loaded when first needed.

use std::ffi::{c_char, c_void, CString};
use std::sync::OnceLock;

pub type Mem = *mut c_void;
pub type Queue = *mut c_void;
type Context = *mut c_void;
type Device = *mut c_void;
type Platform = *mut c_void;

const DEVICE_TYPE_GPU: u64 = 1 << 2;
const DEVICE_TYPE_ALL: u64 = 0xFFFF_FFFF;
const DEVICE_NAME: u32 = 0x102B;
const DEVICE_TYPE: u32 = 0x1000;
const PLATFORM_NAME: u32 = 0x0902;
pub const MEM_READ_WRITE: u64 = 1;
const RGBA: u32 = 0x10B5;
const FLOAT: u32 = 0x10DE;
const UNORM_INT8: u32 = 0x10D2;
const UNORM_INT16: u32 = 0x10D3;

#[repr(C)]
struct ImageFormat {
    order: u32,
    kind: u32,
}

#[allow(clippy::type_complexity)]
struct Fns {
    get_platform_ids: unsafe extern "system" fn(u32, *mut Platform, *mut u32) -> i32,
    get_platform_info: unsafe extern "system" fn(Platform, u32, usize, *mut c_void, *mut usize) -> i32,
    get_device_ids: unsafe extern "system" fn(Platform, u64, u32, *mut Device, *mut u32) -> i32,
    get_device_info: unsafe extern "system" fn(Device, u32, usize, *mut c_void, *mut usize) -> i32,
    create_context: unsafe extern "system" fn(*const isize, u32, *const Device, *const c_void, *mut c_void, *mut i32) -> Context,
    create_queue: unsafe extern "system" fn(Context, Device, u64, *mut i32) -> Queue,
    create_buffer: unsafe extern "system" fn(Context, u64, usize, *mut c_void, *mut i32) -> Mem,
    create_image_2d: unsafe extern "system" fn(Context, u64, *const ImageFormat, usize, usize, usize, *mut c_void, *mut i32) -> Mem,
    write_buffer: unsafe extern "system" fn(Queue, Mem, u32, usize, usize, *const c_void, u32, *const c_void, *mut c_void) -> i32,
    read_buffer: unsafe extern "system" fn(Queue, Mem, u32, usize, usize, *mut c_void, u32, *const c_void, *mut c_void) -> i32,
    write_image: unsafe extern "system" fn(Queue, Mem, u32, *const usize, *const usize, usize, usize, *const c_void, u32, *const c_void, *mut c_void) -> i32,
    read_image: unsafe extern "system" fn(Queue, Mem, u32, *const usize, *const usize, usize, usize, *mut c_void, u32, *const c_void, *mut c_void) -> i32,
    finish: unsafe extern "system" fn(Queue) -> i32,
    release_mem: unsafe extern "system" fn(Mem) -> i32,
    create_program: unsafe extern "system" fn(Context, u32, *const *const c_char, *const usize, *mut i32) -> *mut c_void,
    build_program: unsafe extern "system" fn(*mut c_void, u32, *const Device, *const c_char, *const c_void, *mut c_void) -> i32,
}

/// The context everything runs in.
pub struct Cl {
    _lib: libloading::Library,
    fns: Fns,
    context: Context,
    device: Device,
    pub queue: Queue,
    /// e.g. "NVIDIA GeForce RTX 4070 (NVIDIA CUDA)".
    pub name: String,
    /// A graphics card (not OpenCL on the processor).
    pub gpu: bool,
}

// SAFETY: OpenCL objects may be used from any thread (the API is thread-safe
// for everything the host does with them).
unsafe impl Send for Cl {}
unsafe impl Sync for Cl {}

static CL: OnceLock<Result<Cl, String>> = OnceLock::new();

fn init() -> Result<Cl, String> {
    // SAFETY: loading the system's OpenCL and calling it as its API says.
    unsafe {
        let names: &[&str] = if cfg!(windows) { &["OpenCL.dll"] } else { &["libOpenCL.so.1", "libOpenCL.so"] };
        let lib = names.iter().find_map(|n| libloading::Library::new(n).ok()).ok_or("no OpenCL (no OpenCL driver installed)")?;
        macro_rules! f {
            ($n:literal) => {
                *lib.get($n).map_err(|_| format!("OpenCL has no {}", String::from_utf8_lossy(&$n[..$n.len() - 1])))?
            };
        }
        let fns = Fns {
            get_platform_ids: f!(b"clGetPlatformIDs\0"),
            get_platform_info: f!(b"clGetPlatformInfo\0"),
            get_device_ids: f!(b"clGetDeviceIDs\0"),
            get_device_info: f!(b"clGetDeviceInfo\0"),
            create_context: f!(b"clCreateContext\0"),
            create_queue: f!(b"clCreateCommandQueue\0"),
            create_buffer: f!(b"clCreateBuffer\0"),
            create_image_2d: f!(b"clCreateImage2D\0"),
            write_buffer: f!(b"clEnqueueWriteBuffer\0"),
            read_buffer: f!(b"clEnqueueReadBuffer\0"),
            write_image: f!(b"clEnqueueWriteImage\0"),
            read_image: f!(b"clEnqueueReadImage\0"),
            finish: f!(b"clFinish\0"),
            release_mem: f!(b"clReleaseMemObject\0"),
            create_program: f!(b"clCreateProgramWithSource\0"),
            build_program: f!(b"clBuildProgram\0"),
        };
        let mut n = 0;
        if (fns.get_platform_ids)(0, std::ptr::null_mut(), &mut n) != 0 || n == 0 {
            return Err("no OpenCL platforms (no OpenCL driver installed)".into());
        }
        let mut platforms = vec![std::ptr::null_mut(); n as usize];
        (fns.get_platform_ids)(n, platforms.as_mut_ptr(), std::ptr::null_mut());
        let text = |f: &dyn Fn(usize, *mut c_void, *mut usize) -> i32| -> String {
            let mut len = 0;
            f(0, std::ptr::null_mut(), &mut len);
            let mut buf = vec![0u8; len.max(1)];
            f(len, buf.as_mut_ptr() as *mut c_void, std::ptr::null_mut());
            String::from_utf8_lossy(&buf).trim_end_matches('\0').trim().to_string()
        };
        // A graphics card on any platform first; else whatever there is.
        for want in [DEVICE_TYPE_GPU, DEVICE_TYPE_ALL] {
            for &p in &platforms {
                let mut dev = std::ptr::null_mut();
                let mut count = 0;
                if (fns.get_device_ids)(p, want, 1, &mut dev, &mut count) != 0 || count == 0 {
                    continue;
                }
                let mut err = 0;
                let context = (fns.create_context)(std::ptr::null(), 1, &dev, std::ptr::null(), std::ptr::null_mut(), &mut err);
                if context.is_null() || err != 0 {
                    continue;
                }
                let queue = (fns.create_queue)(context, dev, 0, &mut err);
                if queue.is_null() || err != 0 {
                    continue;
                }
                let mut kind: u64 = 0;
                (fns.get_device_info)(dev, DEVICE_TYPE, 8, &mut kind as *mut u64 as *mut c_void, std::ptr::null_mut());
                let device_name = text(&|n, b, l| (fns.get_device_info)(dev, DEVICE_NAME, n, b, l));
                let platform_name = text(&|n, b, l| (fns.get_platform_info)(p, PLATFORM_NAME, n, b, l));
                let name = format!("{device_name} ({platform_name})");
                return Ok(Cl { _lib: lib, fns, context, device: dev, queue, name, gpu: kind & DEVICE_TYPE_GPU != 0 });
            }
        }
        Err("OpenCL found no device to use".into())
    }
}

/// The OpenCL context (made the first time; kept for the life of the
/// program). `Err` if there's no OpenCL here.
pub fn get() -> Result<&'static Cl, String> {
    CL.get_or_init(|| {
        let made = init();
        match &made {
            Ok(c) => log::info!("OpenFX: plugins can render with OpenCL on {}", c.name),
            Err(e) => log::info!("OpenFX: no OpenCL for plugins: {e}"),
        }
        made
    })
    .as_ref()
    .map_err(|e| e.clone())
}

/// What OpenCL plugins would run on, and whether it's a graphics card.
pub fn available() -> Result<(String, bool), String> {
    get().map(|c| (c.name.clone(), c.gpu))
}

/// The OpenCL image format for a plugin's pixel depth (RGBA).
fn format(depth: &str) -> ImageFormat {
    let kind = match depth {
        "OfxBitDepthByte" => UNORM_INT8,
        "OfxBitDepthShort" => UNORM_INT16,
        _ => FLOAT,
    };
    ImageFormat { order: RGBA, kind }
}

impl Cl {
    /// A buffer holding `data` (or `len` bytes, uninitialised, if `data` is `None`).
    pub fn buffer(&self, len: usize, data: Option<&[u8]>) -> Result<Mem, String> {
        // SAFETY: OpenCL calls on our context and queue; `data` outlives the blocking write.
        unsafe {
            let mut err = 0;
            let m = (self.fns.create_buffer)(self.context, MEM_READ_WRITE, len.max(1), std::ptr::null_mut(), &mut err);
            if m.is_null() || err != 0 {
                return Err(format!("OpenCL couldn't make a {len}-byte buffer ({err})"));
            }
            if let Some(d) = data {
                let st = (self.fns.write_buffer)(self.queue, m, 1, 0, d.len(), d.as_ptr() as *const c_void, 0, std::ptr::null(), std::ptr::null_mut());
                if st != 0 {
                    (self.fns.release_mem)(m);
                    return Err(format!("OpenCL couldn't fill a buffer ({st})"));
                }
            }
            Ok(m)
        }
    }

    /// A `w`×`h` RGBA image of the plugin's depth, holding `data` (rows of `row` bytes) if given.
    pub fn image(&self, w: u32, h: u32, depth: &str, row: usize, data: Option<&[u8]>) -> Result<Mem, String> {
        // SAFETY: as above.
        unsafe {
            let mut err = 0;
            let f = format(depth);
            let m = (self.fns.create_image_2d)(self.context, MEM_READ_WRITE, &f, w as usize, h as usize, 0, std::ptr::null_mut(), &mut err);
            if m.is_null() || err != 0 {
                return Err(format!("OpenCL couldn't make a {w}×{h} image ({err})"));
            }
            if let Some(d) = data {
                let (origin, region) = ([0usize; 3], [w as usize, h as usize, 1]);
                let st = (self.fns.write_image)(self.queue, m, 1, origin.as_ptr(), region.as_ptr(), row, 0, d.as_ptr() as *const c_void, 0, std::ptr::null(), std::ptr::null_mut());
                if st != 0 {
                    (self.fns.release_mem)(m);
                    return Err(format!("OpenCL couldn't fill an image ({st})"));
                }
            }
            Ok(m)
        }
    }

    /// Wait for the queue, then copy buffer `m` (or image, `w`×`h` with rows of `row` bytes) into `out`.
    // (`m` is OpenCL's own handle, made by this module: only the driver looks behind it)
    #[allow(clippy::not_unsafe_ptr_arg_deref)]
    pub fn read(&self, m: Mem, image: Option<(u32, u32, usize)>, out: &mut [u8]) -> Result<(), String> {
        // SAFETY: as above; `out` is big enough (checked by the caller's sizes).
        unsafe {
            let st = (self.fns.finish)(self.queue);
            if st != 0 {
                return Err(format!("OpenCL failed while rendering ({st})"));
            }
            let st = match image {
                None => (self.fns.read_buffer)(self.queue, m, 1, 0, out.len(), out.as_mut_ptr() as *mut c_void, 0, std::ptr::null(), std::ptr::null_mut()),
                Some((w, h, row)) => {
                    let (origin, region) = ([0usize; 3], [w as usize, h as usize, 1]);
                    (self.fns.read_image)(self.queue, m, 1, origin.as_ptr(), region.as_ptr(), row, 0, out.as_mut_ptr() as *mut c_void, 0, std::ptr::null(), std::ptr::null_mut())
                }
            };
            if st != 0 { Err(format!("OpenCL couldn't read the result back ({st})")) } else { Ok(()) }
        }
    }

    // (as `read`)
    #[allow(clippy::not_unsafe_ptr_arg_deref)]
    pub fn release(&self, m: Mem) {
        if !m.is_null() {
            // SAFETY: a memory object we made.
            unsafe { (self.fns.release_mem)(m) };
        }
    }

    /// Build `source` into a program on this context (the OpenCL program suite).
    pub fn compile(&self, source: &str) -> Result<*mut c_void, String> {
        let src = CString::new(source).map_err(|e| e.to_string())?;
        // SAFETY: as above.
        unsafe {
            let mut err = 0;
            let p = src.as_ptr();
            let prog = (self.fns.create_program)(self.context, 1, &p, std::ptr::null(), &mut err);
            if prog.is_null() || err != 0 {
                return Err(format!("OpenCL couldn't take the program ({err})"));
            }
            let st = (self.fns.build_program)(prog, 1, &self.device, std::ptr::null(), std::ptr::null(), std::ptr::null_mut());
            if st != 0 {
                return Err(format!("OpenCL couldn't build the program ({st})"));
            }
            Ok(prog)
        }
    }
}
