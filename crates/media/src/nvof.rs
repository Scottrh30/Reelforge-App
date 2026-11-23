//! NVIDIA's optical flow engine (NVOFA): the dedicated motion-estimation hardware in
//! RTX graphics cards (Turing and newer), separate from the shader and tensor cores.
//! Optical-flow retiming (slow motion, frame interpolation) can take its motion from it
//! rather than working it out in shaders: faster at large sizes, and better at fast
//! motion, which it searches widely for.
//!
//! The engine is reached through NVIDIA's Optical Flow API (`nvofapi64.dll`, which
//! comes with the graphics driver) on CUDA's driver API (`nvcuda.dll`, also the
//! driver's): nothing extra to install. The structures and calls are those of NVIDIA's
//! published `nvOpticalFlowCommon.h` and `nvOpticalFlowCuda.h` (API 2.0, BSD-licensed),
//! written out here in Rust. Windows only; elsewhere [`HardwareFlow::open`] says no.
//!
//! It gives one vector per block of 1, 2 or 4 pixels (whatever the card offers, finest
//! first) in 1/32 pixel; [`expand`] spreads them to every pixel. Which way round its
//! vectors point is checked against the pictures on the first pair ([`orientation`]),
//! so a driver that reads them the other way can't turn the motion backwards.

use anyhow::Result;

/// Effort: NVIDIA's "slow" (best), "medium" and "fast" levels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effort {
    Fast,
    Medium,
    Slow,
}

impl Effort {
    /// NV_OF_PERF_LEVEL.
    pub fn level(self) -> u32 {
        match self {
            Effort::Fast => 20,
            Effort::Medium => 10,
            Effort::Slow => 5,
        }
    }
}

/// Block vectors (`gw` × `gh`, in 1/32 pixel, one per `grid` pixels) → one per pixel
/// (`w` × `h`, in pixels), bilinearly between block centres.
pub fn expand(v: &[[i16; 2]], gw: u32, gh: u32, grid: u32, w: u32, h: u32) -> Vec<[f32; 2]> {
    let mut out = Vec::with_capacity((w as usize).saturating_mul(h as usize));
    if gw == 0 || gh == 0 || grid == 0 {
        out.resize((w as usize).saturating_mul(h as usize), [0.0; 2]);
        return out;
    }
    let at = |x: i64, y: i64| -> [f32; 2] {
        let x = x.clamp(0, gw as i64 - 1) as usize;
        let y = y.clamp(0, gh as i64 - 1) as usize;
        let p = v.get(y * gw as usize + x).copied().unwrap_or([0, 0]);
        [p[0] as f32 / 32.0, p[1] as f32 / 32.0]
    };
    let g = grid as f32;
    for y in 0..h {
        let fy = (y as f32 + 0.5) / g - 0.5;
        let y0 = fy.floor();
        let ty = fy - y0;
        for x in 0..w {
            let fx = (x as f32 + 0.5) / g - 0.5;
            let x0 = fx.floor();
            let tx = fx - x0;
            let (x0, y0i) = (x0 as i64, y0 as i64);
            let a = at(x0, y0i);
            let b = at(x0 + 1, y0i);
            let c = at(x0, y0i + 1);
            let d = at(x0 + 1, y0i + 1);
            let mix = |k: usize| {
                let top = a[k] + (b[k] - a[k]) * tx;
                let bot = c[k] + (d[k] - c[k]) * tx;
                top + (bot - top) * ty
            };
            out.push([mix(0), mix(1)]);
        }
    }
    out
}

/// Which way the vectors point: `true` when following them from `a` lands on the same
/// thing in `b` (`a[p] ≈ b[p + v]`), `false` when they point the other way. Sampled on a
/// sparse grid; with no motion to tell by, `true`.
pub fn orientation(a: &[u8], b: &[u8], flow: &[[f32; 2]], w: u32, h: u32) -> bool {
    let at = |img: &[u8], x: f32, y: f32| -> Option<f32> {
        let (x, y) = (x.round(), y.round());
        if x < 0.0 || y < 0.0 || x >= w as f32 || y >= h as f32 {
            return None;
        }
        img.get(y as usize * w as usize + x as usize).map(|&v| v as f32)
    };
    let (mut fwd, mut back, mut n) = (0.0f64, 0.0f64, 0u32);
    let step = (w.max(h) / 64).max(1) as usize;
    for y in (0..h as usize).step_by(step) {
        for x in (0..w as usize).step_by(step) {
            let Some(f) = flow.get(y * w as usize + x) else { continue };
            if f[0].abs() + f[1].abs() < 1.0 {
                continue;
            }
            let Some(p) = at(a, x as f32, y as f32) else { continue };
            if let (Some(q), Some(r)) = (at(b, x as f32 + f[0], y as f32 + f[1]), at(b, x as f32 - f[0], y as f32 - f[1])) {
                fwd += (p - q).abs() as f64;
                back += (p - r).abs() as f64;
                n += 1;
            }
        }
    }
    n == 0 || fwd <= back
}

#[cfg(windows)]
pub use imp::HardwareFlow;

/// No optical flow engine to reach.
#[cfg(not(windows))]
pub struct HardwareFlow;

#[cfg(not(windows))]
impl HardwareFlow {
    pub fn open() -> Result<HardwareFlow> {
        anyhow::bail!("NVIDIA's optical flow engine is used on Windows only")
    }
    pub fn name(&self) -> String {
        String::new()
    }
    pub fn flow(&mut self, _a: &[u8], _b: &[u8], _w: u32, _h: u32, _effort: Effort) -> Result<(Vec<[f32; 2]>, Vec<[f32; 2]>)> {
        anyhow::bail!("NVIDIA's optical flow engine is used on Windows only")
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use anyhow::{anyhow, bail};
    use std::ffi::{c_char, c_void};
    use std::ptr::{null, null_mut};

    type CuResult = i32;
    type CuContext = *mut c_void;
    type CuDevicePtr = u64;
    type OfStatus = i32;
    type OfHandle = *mut c_void;
    type OfBuffer = *mut c_void;

    // NV_OF_* values (nvOpticalFlowCommon.h).
    const API_VERSION: u32 = 0x20; // 2.0
    const CAPS_OUTPUT_GRID_SIZES: u32 = 0;
    const CAPS_WIDTH_MIN: u32 = 4;
    const CAPS_HEIGHT_MIN: u32 = 5;
    const CAPS_WIDTH_MAX: u32 = 6;
    const CAPS_HEIGHT_MAX: u32 = 7;
    const MODE_OPTICALFLOW: u32 = 1;
    const USAGE_INPUT: u32 = 1;
    const USAGE_OUTPUT: u32 = 2;
    const FORMAT_GRAYSCALE8: u32 = 1;
    const FORMAT_SHORT2: u32 = 5;
    const CUDA_BUFFER_DEVICEPTR: u32 = 2;
    // CUmemorytype.
    const CU_HOST: u32 = 1;
    const CU_DEVICE: u32 = 2;

    #[repr(C)]
    struct InitParams {
        width: u32,
        height: u32,
        out_grid: u32,
        hint_grid: u32,
        mode: u32,
        perf_level: u32,
        external_hints: u32,
        output_cost: u32,
        priv_data: *mut c_void,
        disparity_range: u32,
        roi: u32,
    }

    #[repr(C)]
    struct BufferDesc {
        width: u32,
        height: u32,
        usage: u32,
        format: u32,
    }

    #[repr(C)]
    struct ExecIn {
        input: OfBuffer,
        reference: OfBuffer,
        hints: OfBuffer,
        disable_temporal_hints: u32,
        padding: u32,
        priv_data: *mut c_void,
        padding2: u32,
        num_rois: u32,
        rois: *mut c_void,
    }

    #[repr(C)]
    struct ExecOut {
        output: OfBuffer,
        cost: OfBuffer,
        priv_data: *mut c_void,
    }

    #[repr(C)]
    #[derive(Default)]
    struct Stride {
        x: u32,
        y: u32,
    }

    #[repr(C)]
    #[derive(Default)]
    struct StrideInfo {
        planes: [Stride; 3],
        num_planes: u32,
    }

    #[repr(C)]
    struct FunctionList {
        create: Option<unsafe extern "system" fn(CuContext, *mut OfHandle) -> OfStatus>,
        init: Option<unsafe extern "system" fn(OfHandle, *const InitParams) -> OfStatus>,
        create_buffer: Option<unsafe extern "system" fn(OfHandle, *const BufferDesc, u32, *mut OfBuffer) -> OfStatus>,
        get_cuarray: Option<unsafe extern "system" fn(OfBuffer) -> *mut c_void>,
        get_devptr: Option<unsafe extern "system" fn(OfBuffer) -> CuDevicePtr>,
        get_stride: Option<unsafe extern "system" fn(OfBuffer, *mut StrideInfo) -> OfStatus>,
        set_streams: Option<unsafe extern "system" fn(OfHandle, *mut c_void, *mut c_void) -> OfStatus>,
        execute: Option<unsafe extern "system" fn(OfHandle, *const ExecIn, *mut ExecOut) -> OfStatus>,
        destroy_buffer: Option<unsafe extern "system" fn(OfBuffer) -> OfStatus>,
        destroy: Option<unsafe extern "system" fn(OfHandle) -> OfStatus>,
        last_error: Option<unsafe extern "system" fn(OfHandle, *mut c_char, *mut u32) -> OfStatus>,
        get_caps: Option<unsafe extern "system" fn(OfHandle, u32, *mut u32, *mut u32) -> OfStatus>,
    }

    /// CUDA_MEMCPY2D (cuda.h).
    #[repr(C)]
    struct Copy2D {
        src_x: usize,
        src_y: usize,
        src_type: u32,
        src_host: *const c_void,
        src_device: CuDevicePtr,
        src_array: *mut c_void,
        src_pitch: usize,
        dst_x: usize,
        dst_y: usize,
        dst_type: u32,
        dst_host: *mut c_void,
        dst_device: CuDevicePtr,
        dst_array: *mut c_void,
        dst_pitch: usize,
        width_bytes: usize,
        height: usize,
    }

    struct Cuda {
        _lib: libloading::Library,
        init: unsafe extern "system" fn(u32) -> CuResult,
        device_get: unsafe extern "system" fn(*mut i32, i32) -> CuResult,
        device_name: unsafe extern "system" fn(*mut c_char, i32, i32) -> CuResult,
        ctx_create: unsafe extern "system" fn(*mut CuContext, u32, i32) -> CuResult,
        ctx_destroy: unsafe extern "system" fn(CuContext) -> CuResult,
        ctx_push: unsafe extern "system" fn(CuContext) -> CuResult,
        ctx_pop: unsafe extern "system" fn(*mut CuContext) -> CuResult,
        memcpy2d: unsafe extern "system" fn(*const Copy2D) -> CuResult,
    }

    /// Buffers for one picture size.
    struct Session {
        w: u32,
        h: u32,
        grid: u32,
        gw: u32,
        gh: u32,
        effort: Effort,
        a: OfBuffer,
        b: OfBuffer,
        out: OfBuffer,
    }

    /// NVIDIA's optical flow engine, ready for pairs of grey frames.
    pub struct HardwareFlow {
        cuda: Cuda,
        _of_lib: libloading::Library,
        f: FunctionList,
        ctx: CuContext,
        of: OfHandle,
        name: String,
        grids: Vec<u32>,
        min: (u32, u32),
        max: (u32, u32),
        session: Option<Session>,
        /// Which way the engine's vectors point, once seen (None: not yet).
        forward: Option<bool>,
    }

    // SAFETY: the CUDA context is pushed onto whichever thread uses it for each call
    // and popped after; the handles are only used through `&mut self`.
    unsafe impl Send for HardwareFlow {}

    impl Drop for HardwareFlow {
        fn drop(&mut self) {
            // SAFETY: handles made by this object, destroyed once, with its context current.
            unsafe {
                if (self.cuda.ctx_push)(self.ctx) == 0 {
                    self.free_session();
                    if let Some(d) = self.f.destroy {
                        d(self.of);
                    }
                    let mut old = null_mut();
                    (self.cuda.ctx_pop)(&mut old);
                }
                (self.cuda.ctx_destroy)(self.ctx);
            }
        }
    }

    fn cuda() -> Result<Cuda> {
        const LOAD_LIBRARY_SEARCH_SYSTEM32: u32 = 0x800;
        // SAFETY: the graphics driver's own CUDA library, from System32.
        let lib: libloading::Library = unsafe { libloading::os::windows::Library::load_with_flags("nvcuda.dll", LOAD_LIBRARY_SEARCH_SYSTEM32) }.map_err(|e| anyhow!("no NVIDIA driver with CUDA ({e})"))?.into();
        // SAFETY: the functions have cuda.h's signatures.
        unsafe {
            macro_rules! sym {
                ($n:literal) => {
                    *lib.get(concat!($n, "\0").as_bytes()).map_err(|e| anyhow!("{}: {e}", $n))?
                };
            }
            Ok(Cuda {
                init: sym!("cuInit"),
                device_get: sym!("cuDeviceGet"),
                device_name: sym!("cuDeviceGetName"),
                ctx_create: sym!("cuCtxCreate_v2"),
                ctx_destroy: sym!("cuCtxDestroy_v2"),
                ctx_push: sym!("cuCtxPushCurrent_v2"),
                ctx_pop: sym!("cuCtxPopCurrent_v2"),
                memcpy2d: sym!("cuMemcpy2D_v2"),
                _lib: lib,
            })
        }
    }

    /// The context made current on this thread while it lives.
    struct Current<'a>(&'a Cuda);

    impl Drop for Current<'_> {
        fn drop(&mut self) {
            let mut old = null_mut();
            // SAFETY: pops what `current` pushed.
            unsafe { (self.0.ctx_pop)(&mut old) };
        }
    }

    impl HardwareFlow {
        /// Open the engine on the first NVIDIA card.
        pub fn open() -> Result<HardwareFlow> {
            let cuda = cuda()?;
            // SAFETY (this block): CUDA's driver API as documented, with out-pointers to locals.
            let (ctx, name) = unsafe {
                if (cuda.init)(0) != 0 {
                    bail!("CUDA didn't start");
                }
                let mut dev = 0i32;
                if (cuda.device_get)(&mut dev, 0) != 0 {
                    bail!("no CUDA device");
                }
                let mut name = [0u8; 128];
                (cuda.device_name)(name.as_mut_ptr() as *mut c_char, name.len() as i32, dev);
                let end = name.iter().position(|&b| b == 0).unwrap_or(name.len());
                let name = String::from_utf8_lossy(name.get(..end).unwrap_or(&[])).into_owned();
                let mut ctx: CuContext = null_mut();
                if (cuda.ctx_create)(&mut ctx, 0, dev) != 0 || ctx.is_null() {
                    bail!("couldn't make a CUDA context");
                }
                let mut old = null_mut();
                (cuda.ctx_pop)(&mut old);
                (ctx, name)
            };
            let fail = |cuda: &Cuda, e: anyhow::Error| -> anyhow::Error {
                // SAFETY: the context made above, not used again.
                unsafe { (cuda.ctx_destroy)(ctx) };
                e
            };
            const LOAD_LIBRARY_SEARCH_SYSTEM32: u32 = 0x800;
            // SAFETY: the driver's optical flow library, from System32.
            let of_lib: libloading::Library = match unsafe { libloading::os::windows::Library::load_with_flags("nvofapi64.dll", LOAD_LIBRARY_SEARCH_SYSTEM32) } {
                Ok(l) => l.into(),
                Err(e) => return Err(fail(&cuda, anyhow!("this driver has no optical flow library ({e})"))),
            };
            let mut f = FunctionList { create: None, init: None, create_buffer: None, get_cuarray: None, get_devptr: None, get_stride: None, set_streams: None, execute: None, destroy_buffer: None, destroy: None, last_error: None, get_caps: None };
            // SAFETY: NVIDIA's entry point fills in the function list for this API version.
            let made = unsafe {
                match of_lib.get::<unsafe extern "system" fn(u32, *mut FunctionList) -> OfStatus>(b"NvOFAPICreateInstanceCuda\0") {
                    Ok(create) => create(API_VERSION, &mut f),
                    Err(_) => -1,
                }
            };
            let (Some(create), 0) = (f.create, made) else {
                return Err(fail(&cuda, anyhow!("the optical flow engine isn't available on this card or driver")));
            };
            let mut of: OfHandle = null_mut();
            let st = {
                let _c = current(&cuda, ctx)?;
                // SAFETY: a CUDA context and somewhere to put the handle.
                unsafe { create(ctx, &mut of) }
            };
            if st != 0 || of.is_null() {
                return Err(fail(&cuda, anyhow!("the optical flow engine didn't open (status {st})")));
            }
            let mut me = HardwareFlow { cuda, _of_lib: of_lib, f, ctx, of, name, grids: Vec::new(), min: (32, 32), max: (4096, 4096), session: None, forward: None };
            {
                let _c = current(&me.cuda, me.ctx)?;
                me.grids = me.caps(CAPS_OUTPUT_GRID_SIZES);
                me.grids.retain(|g| matches!(g, 1 | 2 | 4));
                me.grids.sort_unstable();
                let one = |v: Vec<u32>, d: u32| v.first().copied().unwrap_or(d);
                me.min = (one(me.caps(CAPS_WIDTH_MIN), 32), one(me.caps(CAPS_HEIGHT_MIN), 32));
                me.max = (one(me.caps(CAPS_WIDTH_MAX), 4096), one(me.caps(CAPS_HEIGHT_MAX), 4096));
            }
            if me.grids.is_empty() {
                me.grids.push(4);
            }
            Ok(me)
        }

        /// The card's name.
        pub fn name(&self) -> String {
            self.name.clone()
        }

        fn caps(&self, which: u32) -> Vec<u32> {
            let Some(get) = self.f.get_caps else { return Vec::new() };
            let mut n = 0u32;
            // SAFETY: first asks how many values, then reads that many.
            unsafe {
                if get(self.of, which, null_mut(), &mut n) != 0 || n == 0 || n > 16 {
                    return Vec::new();
                }
                let mut v = vec![0u32; n as usize];
                if get(self.of, which, v.as_mut_ptr(), &mut n) != 0 {
                    return Vec::new();
                }
                v.truncate(n as usize);
                v
            }
        }

        fn free_session(&mut self) {
            if let (Some(s), Some(d)) = (self.session.take(), self.f.destroy_buffer) {
                // SAFETY: buffers made by create_buffer, freed once.
                unsafe {
                    d(s.a);
                    d(s.b);
                    d(s.out);
                }
            }
        }

        fn status(&self, st: OfStatus, what: &str) -> Result<()> {
            if st == 0 {
                return Ok(());
            }
            let mut msg = [0u8; 256];
            let mut n = msg.len() as u32;
            if let Some(le) = self.f.last_error {
                // SAFETY: a buffer of the size given.
                unsafe { le(self.of, msg.as_mut_ptr() as *mut c_char, &mut n) };
            }
            let end = msg.iter().position(|&b| b == 0).unwrap_or(0);
            Err(anyhow!("optical flow engine: {what} (status {st}) {}", String::from_utf8_lossy(msg.get(..end).unwrap_or(&[]))))
        }

        fn session(&mut self, w: u32, h: u32, effort: Effort) -> Result<()> {
            if matches!(&self.session, Some(s) if s.w == w && s.h == h && s.effort == effort) {
                return Ok(());
            }
            self.free_session();
            let (Some(init), Some(create_buffer)) = (self.f.init, self.f.create_buffer) else { bail!("incomplete optical flow API") };
            let grid = self.grids.first().copied().unwrap_or(4);
            let p = InitParams { width: w, height: h, out_grid: grid, hint_grid: 0, mode: MODE_OPTICALFLOW, perf_level: effort.level(), external_hints: 0, output_cost: 0, priv_data: null_mut(), disparity_range: 0, roi: 0 };
            // SAFETY: init parameters as the header lays them out.
            self.status(unsafe { init(self.of, &p) }, "setting up")?;
            let (gw, gh) = (w.div_ceil(grid), h.div_ceil(grid));
            let make = |d: BufferDesc| -> Result<OfBuffer> {
                let mut b: OfBuffer = null_mut();
                // SAFETY: a buffer description and somewhere to put the handle.
                let st = unsafe { create_buffer(self.of, &d, CUDA_BUFFER_DEVICEPTR, &mut b) };
                self.status(st, "making room")?;
                Ok(b)
            };
            let a = make(BufferDesc { width: w, height: h, usage: USAGE_INPUT, format: FORMAT_GRAYSCALE8 })?;
            let b = make(BufferDesc { width: w, height: h, usage: USAGE_INPUT, format: FORMAT_GRAYSCALE8 })?;
            let out = make(BufferDesc { width: gw, height: gh, usage: USAGE_OUTPUT, format: FORMAT_SHORT2 })?;
            self.session = Some(Session { w, h, grid, gw, gh, effort, a, b, out });
            Ok(())
        }

        fn stride(&self, buf: OfBuffer) -> Result<(CuDevicePtr, usize)> {
            let (Some(dp), Some(gs)) = (self.f.get_devptr, self.f.get_stride) else { bail!("incomplete optical flow API") };
            let mut s = StrideInfo::default();
            // SAFETY: a buffer made by this engine.
            let st = unsafe { gs(buf, &mut s) };
            self.status(st, "reading the buffer layout")?;
            // SAFETY: as above.
            Ok((unsafe { dp(buf) }, s.planes[0].x as usize))
        }

        fn copy(&self, c: &Copy2D) -> Result<()> {
            // SAFETY: the copy's pointers and sizes are checked by the callers.
            if unsafe { (self.cuda.memcpy2d)(c) } != 0 {
                bail!("copying to or from the graphics card failed");
            }
            Ok(())
        }

        /// The motion between grey frames `a` and `b` (`w` × `h`, a byte a pixel): A to B
        /// and B to A, a vector per pixel in pixels (`a[p] ≈ b[p + ab[p]]`).
        pub fn flow(&mut self, a: &[u8], b: &[u8], w: u32, h: u32, effort: Effort) -> Result<(Vec<[f32; 2]>, Vec<[f32; 2]>)> {
            let n = (w as usize).saturating_mul(h as usize);
            if a.len() < n || b.len() < n {
                bail!("the frames are the wrong size");
            }
            if w < self.min.0 || h < self.min.1 || w > self.max.0 || h > self.max.1 {
                bail!("the optical flow engine takes {}×{} to {}×{}, not {w}×{h}", self.min.0, self.min.1, self.max.0, self.max.1);
            }
            let cuda: *const Cuda = &self.cuda;
            // SAFETY: `cuda` lives as long as self; the guard only pops the context.
            let _c = current(unsafe { &*cuda }, self.ctx)?;
            self.session(w, h, effort)?;
            let Some(s) = self.session.as_ref() else { bail!("no session") };
            let (sa, sb, so, grid, gw, gh) = (s.a, s.b, s.out, s.grid, s.gw, s.gh);
            let up = |buf: OfBuffer, img: &[u8]| -> Result<()> {
                let (dev, pitch) = self.stride(buf)?;
                self.copy(&Copy2D { src_x: 0, src_y: 0, src_type: CU_HOST, src_host: img.as_ptr() as *const c_void, src_device: 0, src_array: null_mut(), src_pitch: w as usize, dst_x: 0, dst_y: 0, dst_type: CU_DEVICE, dst_host: null_mut(), dst_device: dev, dst_array: null_mut(), dst_pitch: pitch, width_bytes: w as usize, height: h as usize })
            };
            up(sa, a)?;
            up(sb, b)?;
            let Some(execute) = self.f.execute else { bail!("incomplete optical flow API") };
            let run = |input: OfBuffer, reference: OfBuffer| -> Result<Vec<[i16; 2]>> {
                let ein = ExecIn { input, reference, hints: null_mut(), disable_temporal_hints: 1, padding: 0, priv_data: null_mut(), padding2: 0, num_rois: 0, rois: null_mut() };
                let mut eout = ExecOut { output: so, cost: null_mut(), priv_data: null_mut() };
                // SAFETY: buffers made for this session.
                let st = unsafe { execute(self.of, &ein, &mut eout) };
                self.status(st, "working out the motion")?;
                let (dev, pitch) = self.stride(so)?;
                let mut v = vec![[0i16; 2]; (gw as usize).saturating_mul(gh as usize)];
                self.copy(&Copy2D { src_x: 0, src_y: 0, src_type: CU_DEVICE, src_host: null(), src_device: dev, src_array: null_mut(), src_pitch: pitch, dst_x: 0, dst_y: 0, dst_type: CU_HOST, dst_host: v.as_mut_ptr() as *mut c_void, dst_device: 0, dst_array: null_mut(), dst_pitch: gw as usize * 4, width_bytes: gw as usize * 4, height: gh as usize })?;
                Ok(v)
            };
            let ab = expand(&run(sa, sb)?, gw, gh, grid, w, h);
            let ba = expand(&run(sb, sa)?, gw, gh, grid, w, h);
            // Which way round the engine's vectors point, judged once on real motion.
            let forward = match self.forward {
                Some(f) => f,
                None => {
                    let f = orientation(a, b, &ab, w, h);
                    if ab.iter().any(|v| v[0].abs() + v[1].abs() >= 1.0) {
                        self.forward = Some(f);
                    }
                    f
                }
            };
            if forward {
                Ok((ab, ba))
            } else {
                let neg = |v: Vec<[f32; 2]>| v.into_iter().map(|[x, y]| [-x, -y]).collect();
                Ok((neg(ab), neg(ba)))
            }
        }
    }

    fn current(cuda: &Cuda, ctx: CuContext) -> Result<Current<'_>> {
        // SAFETY: a context made by this module.
        if unsafe { (cuda.ctx_push)(ctx) } != 0 {
            bail!("couldn't use the CUDA context");
        }
        Ok(Current(cuda))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_vectors_spread_to_every_pixel() {
        // 2×1 blocks of 4 pixels: (1, 0) px then (3, -1) px (in 1/32).
        let v = [[32, 0], [96, -32]];
        let f = expand(&v, 2, 1, 4, 8, 4);
        assert_eq!(f.len(), 32);
        assert_eq!(f[0], [1.0, 0.0], "the first block's own value at its edge");
        assert_eq!(f[7], [3.0, -1.0]);
        // Between block centres (x 1.5 and 5.5), halfway at x 3.5.
        assert!((f[3][0] - 1.75).abs() < 1e-5 && (f[4][0] - 2.25).abs() < 1e-5, "{:?}", &f[..8]);
        // Nothing to spread: zeros, not a crash.
        assert!(expand(&[], 0, 0, 4, 3, 3).iter().all(|p| *p == [0.0, 0.0]));
    }

    #[test]
    fn tells_which_way_the_vectors_point() {
        // A bright square moves 6 pixels right from A to B.
        let (w, h) = (64u32, 64u32);
        let square = |dx: i32| -> Vec<u8> { (0..h as i32).flat_map(|y| (0..w as i32).map(move |x| if (20 + dx..36 + dx).contains(&x) && (20..36).contains(&y) { 230 } else { (x * 2 + y) as u8 })).collect() };
        let (a, b) = (square(0), square(6));
        let right: Vec<[f32; 2]> = (0..w * h).map(|_| [6.0, 0.0]).collect();
        let left: Vec<[f32; 2]> = (0..w * h).map(|_| [-6.0, 0.0]).collect();
        assert!(orientation(&a, &b, &right, w, h));
        assert!(!orientation(&a, &b, &left, w, h));
        // Still pictures can't tell: taken as they come.
        assert!(orientation(&a, &a, &vec![[0.0, 0.0]; (w * h) as usize], w, h));
        if !cfg!(windows) {
            assert!(HardwareFlow::open().is_err());
        }
    }
}
