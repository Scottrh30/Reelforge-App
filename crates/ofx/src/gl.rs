//! An OpenGL context for plugins that render on the graphics card (the
//! OpenFX OpenGL render suite): one per process, made with EGL on Linux (no
//! window needed; Mesa's software renderer works too) or WGL on Windows (a
//! hidden window), and made current on whichever thread renders, one render
//! at a time.

use std::ffi::{c_char, c_void, CStr, CString};
use std::sync::{Mutex, OnceLock};

pub type GLenum = u32;
pub type GLuint = u32;
pub type GLint = i32;
pub type GLsizei = i32;

pub const TEXTURE_2D: GLenum = 0x0DE1;
pub const RGBA: GLenum = 0x1908;
pub const RGBA32F: GLenum = 0x8814;
pub const RGBA16: GLenum = 0x805B;
pub const RGBA8: GLenum = 0x8058;
pub const FLOAT: GLenum = 0x1406;
pub const TEXTURE_MIN_FILTER: GLenum = 0x2801;
pub const TEXTURE_MAG_FILTER: GLenum = 0x2800;
pub const LINEAR: GLint = 0x2601;
pub const FRAMEBUFFER: GLenum = 0x8D40;
pub const COLOR_ATTACHMENT0: GLenum = 0x8CE0;
pub const FRAMEBUFFER_COMPLETE: GLenum = 0x8CD5;
pub const PROJECTION: GLenum = 0x1701;
pub const MODELVIEW: GLenum = 0x1700;
pub const PACK_ALIGNMENT: GLenum = 0x0D05;
pub const UNPACK_ALIGNMENT: GLenum = 0x0CF5;
pub const COLOR_BUFFER_BIT: u32 = 0x4000;
const RENDERER: GLenum = 0x1F01;
const VERSION: GLenum = 0x1F02;

/// The OpenGL functions the host itself uses.
#[allow(clippy::type_complexity)]
pub struct Fns {
    pub gen_textures: unsafe extern "system" fn(GLsizei, *mut GLuint),
    pub delete_textures: unsafe extern "system" fn(GLsizei, *const GLuint),
    pub bind_texture: unsafe extern "system" fn(GLenum, GLuint),
    pub tex_image_2d: unsafe extern "system" fn(GLenum, GLint, GLint, GLsizei, GLsizei, GLint, GLenum, GLenum, *const c_void),
    pub tex_parameteri: unsafe extern "system" fn(GLenum, GLenum, GLint),
    pub gen_framebuffers: unsafe extern "system" fn(GLsizei, *mut GLuint),
    pub delete_framebuffers: unsafe extern "system" fn(GLsizei, *const GLuint),
    pub bind_framebuffer: unsafe extern "system" fn(GLenum, GLuint),
    pub framebuffer_texture_2d: unsafe extern "system" fn(GLenum, GLenum, GLenum, GLuint, GLint),
    pub check_framebuffer_status: unsafe extern "system" fn(GLenum) -> GLenum,
    pub viewport: unsafe extern "system" fn(GLint, GLint, GLsizei, GLsizei),
    pub matrix_mode: unsafe extern "system" fn(GLenum),
    pub load_identity: unsafe extern "system" fn(),
    pub ortho: unsafe extern "system" fn(f64, f64, f64, f64, f64, f64),
    pub read_pixels: unsafe extern "system" fn(GLint, GLint, GLsizei, GLsizei, GLenum, GLenum, *mut c_void),
    pub finish: unsafe extern "system" fn(),
    pub get_error: unsafe extern "system" fn() -> GLenum,
    pub pixel_storei: unsafe extern "system" fn(GLenum, GLint),
    pub clear_color: unsafe extern "system" fn(f32, f32, f32, f32),
    pub clear: unsafe extern "system" fn(u32),
    pub get_string: unsafe extern "system" fn(GLenum) -> *const u8,
}

impl Fns {
    // (each function pointer's type is its field's: spelling the 19 out again adds nothing)
    #[allow(clippy::missing_transmute_annotations)]
    unsafe fn load(get: &dyn Fn(&str) -> *const c_void) -> Result<Self, String> {
        macro_rules! f {
            ($name:literal) => {{
                let p = get($name);
                if p.is_null() {
                    return Err(format!("OpenGL has no {}", $name));
                }
                std::mem::transmute::<*const c_void, _>(p)
            }};
        }
        // Framebuffers: core since 3.0, else the EXT ones.
        let fb = |core: &str, ext: &str| {
            let p = get(core);
            if p.is_null() { get(ext) } else { p }
        };
        let gen_fb = fb("glGenFramebuffers", "glGenFramebuffersEXT");
        let del_fb = fb("glDeleteFramebuffers", "glDeleteFramebuffersEXT");
        let bind_fb = fb("glBindFramebuffer", "glBindFramebufferEXT");
        let tex_fb = fb("glFramebufferTexture2D", "glFramebufferTexture2DEXT");
        let check_fb = fb("glCheckFramebufferStatus", "glCheckFramebufferStatusEXT");
        if [gen_fb, del_fb, bind_fb, tex_fb, check_fb].iter().any(|p| p.is_null()) {
            return Err("OpenGL has no framebuffer objects".into());
        }
        Ok(Fns {
            gen_textures: f!("glGenTextures"),
            delete_textures: f!("glDeleteTextures"),
            bind_texture: f!("glBindTexture"),
            tex_image_2d: f!("glTexImage2D"),
            tex_parameteri: f!("glTexParameteri"),
            gen_framebuffers: std::mem::transmute::<*const c_void, _>(gen_fb),
            delete_framebuffers: std::mem::transmute::<*const c_void, _>(del_fb),
            bind_framebuffer: std::mem::transmute::<*const c_void, _>(bind_fb),
            framebuffer_texture_2d: std::mem::transmute::<*const c_void, _>(tex_fb),
            check_framebuffer_status: std::mem::transmute::<*const c_void, _>(check_fb),
            viewport: f!("glViewport"),
            matrix_mode: f!("glMatrixMode"),
            load_identity: f!("glLoadIdentity"),
            ortho: f!("glOrtho"),
            read_pixels: f!("glReadPixels"),
            finish: f!("glFinish"),
            get_error: f!("glGetError"),
            pixel_storei: f!("glPixelStorei"),
            clear_color: f!("glClearColor"),
            clear: f!("glClear"),
            get_string: f!("glGetString"),
        })
    }
}

/// The context, its functions, and what it runs on.
pub struct Gl {
    platform: platform::Context,
    pub fns: Fns,
    /// e.g. "llvmpipe (LLVM 15.0.7, 256 bits), OpenGL 4.5".
    pub renderer: String,
}

// SAFETY: the context is only made current inside `with`, under a mutex, and
// released again before the lock is given up.
unsafe impl Send for Gl {}

static GL: Mutex<Option<Result<Gl, String>>> = Mutex::new(None);
static FNS: OnceLock<usize> = OnceLock::new();

fn init() -> Result<Gl, String> {
    // SAFETY: creating a context and reading function pointers from the system's OpenGL.
    unsafe {
        let platform = platform::Context::new()?;
        platform.make_current(true)?;
        let fns = Fns::load(&|n| platform.proc_address(n));
        let result = fns.map(|fns| {
            let s = |e| {
                let p = (fns.get_string)(e);
                if p.is_null() { String::new() } else { CStr::from_ptr(p as *const c_char).to_string_lossy().into_owned() }
            };
            let renderer = format!("{}, OpenGL {}", s(RENDERER), s(VERSION));
            Gl { platform, fns, renderer }
        });
        if let Ok(g) = &result {
            let _ = g.platform.make_current(false);
        }
        result
    }
}

/// Run `f` with the context current on this thread (made the first time).
/// `Err` if there's no OpenGL here.
pub fn with<R>(f: impl FnOnce(&Gl) -> R) -> Result<R, String> {
    let mut g = GL.lock().unwrap_or_else(|e| e.into_inner());
    if g.is_none() {
        let made = init();
        if let Err(e) = &made {
            log::info!("OpenFX: no OpenGL for plugins: {e}");
        }
        *g = Some(made);
    }
    let gl = match g.as_ref() {
        Some(Ok(gl)) => gl,
        Some(Err(e)) => return Err(e.clone()),
        None => return Err("OpenGL wasn't set up".into()),
    };
    // SAFETY: our context, made current here and released below.
    unsafe { gl.platform.make_current(true)? };
    FNS.get_or_init(|| &gl.fns as *const Fns as usize);
    let r = f(gl);
    // SAFETY: as above.
    unsafe {
        let _ = gl.platform.make_current(false);
    }
    Ok(r)
}

/// The functions (only while inside [`with`], on that thread).
pub fn fns() -> Option<&'static Fns> {
    // SAFETY: the context (and so the table) lives as long as the program once made.
    FNS.get().map(|p| unsafe { &*(*p as *const Fns) })
}

/// Can plugins render with OpenGL here? The renderer if so.
pub fn available() -> Result<String, String> {
    with(|g| g.renderer.clone())
}

#[cfg(all(unix, not(target_os = "macos")))]
mod platform {
    use super::*;

    type EGLDisplay = *mut c_void;
    type EGLConfig = *mut c_void;
    type EGLContext = *mut c_void;
    type EGLSurface = *mut c_void;
    const NONE: i32 = 0x3038;
    const OPENGL_API: u32 = 0x30A2;
    const RENDERABLE_TYPE: i32 = 0x3040;
    const OPENGL_BIT: i32 = 0x0008;
    const SURFACE_TYPE: i32 = 0x3033;
    const PBUFFER_BIT: i32 = 0x0001;
    const WIDTH: i32 = 0x3057;
    const HEIGHT: i32 = 0x3056;
    const PLATFORM_SURFACELESS_MESA: u32 = 0x31DD;

    pub struct Context {
        _lib: libloading::Library,
        _gl: Option<libloading::Library>,
        display: EGLDisplay,
        context: EGLContext,
        surface: EGLSurface,
        make_current: unsafe extern "C" fn(EGLDisplay, EGLSurface, EGLSurface, EGLContext) -> u32,
        get_proc: unsafe extern "C" fn(*const c_char) -> *const c_void,
    }

    impl Context {
        pub unsafe fn new() -> Result<Self, String> {
            let lib = libloading::Library::new("libEGL.so.1").map_err(|e| format!("no EGL ({e})"))?;
            macro_rules! sym {
                ($n:literal, $t:ty) => {
                    *lib.get::<$t>($n).map_err(|_| format!("EGL has no {}", String::from_utf8_lossy(&$n[..$n.len() - 1])))?
                };
            }
            let get_proc = sym!(b"eglGetProcAddress\0", unsafe extern "C" fn(*const c_char) -> *const c_void);
            let get_display = sym!(b"eglGetDisplay\0", unsafe extern "C" fn(*mut c_void) -> EGLDisplay);
            let initialize = sym!(b"eglInitialize\0", unsafe extern "C" fn(EGLDisplay, *mut i32, *mut i32) -> u32);
            let bind_api = sym!(b"eglBindAPI\0", unsafe extern "C" fn(u32) -> u32);
            let choose = sym!(b"eglChooseConfig\0", unsafe extern "C" fn(EGLDisplay, *const i32, *mut EGLConfig, i32, *mut i32) -> u32);
            let create = sym!(b"eglCreateContext\0", unsafe extern "C" fn(EGLDisplay, EGLConfig, EGLContext, *const i32) -> EGLContext);
            let pbuffer = sym!(b"eglCreatePbufferSurface\0", unsafe extern "C" fn(EGLDisplay, EGLConfig, *const i32) -> EGLSurface);
            let make_current = sym!(b"eglMakeCurrent\0", unsafe extern "C" fn(EGLDisplay, EGLSurface, EGLSurface, EGLContext) -> u32);
            // No window system needed: Mesa's surfaceless platform, else the default display.
            let mut displays = Vec::new();
            let platform_display = get_proc(c"eglGetPlatformDisplayEXT".as_ptr());
            if !platform_display.is_null() {
                let f: unsafe extern "C" fn(u32, *mut c_void, *const i32) -> EGLDisplay = std::mem::transmute(platform_display);
                displays.push(f(PLATFORM_SURFACELESS_MESA, std::ptr::null_mut(), std::ptr::null()));
            }
            displays.push(get_display(std::ptr::null_mut()));
            let mut last = String::from("no EGL display");
            for display in displays.into_iter().filter(|d| !d.is_null()) {
                let (mut major, mut minor) = (0, 0);
                if initialize(display, &mut major, &mut minor) == 0 {
                    last = "EGL wouldn't start".into();
                    continue;
                }
                if bind_api(OPENGL_API) == 0 {
                    last = "EGL has no desktop OpenGL".into();
                    continue;
                }
                let attribs = [RENDERABLE_TYPE, OPENGL_BIT, SURFACE_TYPE, 0, NONE];
                let mut config = std::ptr::null_mut();
                let mut n = 0;
                if choose(display, attribs.as_ptr(), &mut config, 1, &mut n) == 0 || n < 1 {
                    last = "EGL has no OpenGL configuration".into();
                    continue;
                }
                // A compatibility context (no profile asked for): plugins use the old fixed-function calls too.
                let context = create(display, config, std::ptr::null_mut(), [NONE].as_ptr());
                if context.is_null() {
                    last = "EGL couldn't make an OpenGL context".into();
                    continue;
                }
                // Surfaceless if allowed, else a 1×1 off-screen surface.
                let mut surface = std::ptr::null_mut();
                if make_current(display, surface, surface, context) == 0 {
                    surface = pbuffer(display, config, [WIDTH, 1, HEIGHT, 1, NONE].as_ptr());
                    if make_current(display, surface, surface, context) == 0 {
                        last = "EGL couldn't make the context current".into();
                        continue;
                    }
                }
                make_current(display, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut());
                let _ = PBUFFER_BIT;
                let gl = libloading::Library::new("libGL.so.1").or_else(|_| libloading::Library::new("libOpenGL.so.0")).ok();
                return Ok(Context { _lib: lib, _gl: gl, display, context, surface, make_current, get_proc });
            }
            Err(last)
        }

        pub unsafe fn make_current(&self, on: bool) -> Result<(), String> {
            let ok = if on {
                (self.make_current)(self.display, self.surface, self.surface, self.context)
            } else {
                (self.make_current)(self.display, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut())
            };
            if ok == 0 { Err("couldn't make the OpenGL context current".into()) } else { Ok(()) }
        }

        pub unsafe fn proc_address(&self, name: &str) -> *const c_void {
            let Ok(n) = CString::new(name) else { return std::ptr::null() };
            let p = (self.get_proc)(n.as_ptr());
            if !p.is_null() {
                return p;
            }
            self._gl.as_ref().and_then(|l| l.get::<*const c_void>(n.as_bytes_with_nul()).ok().map(|s| *s)).unwrap_or(std::ptr::null())
        }
    }
}

#[cfg(windows)]
mod platform {
    use super::*;

    type HWND = *mut c_void;
    type HDC = *mut c_void;
    type HGLRC = *mut c_void;

    #[repr(C)]
    struct WndClassW {
        style: u32,
        wnd_proc: unsafe extern "system" fn(HWND, u32, usize, isize) -> isize,
        cls_extra: i32,
        wnd_extra: i32,
        instance: *mut c_void,
        icon: *mut c_void,
        cursor: *mut c_void,
        background: *mut c_void,
        menu_name: *const u16,
        class_name: *const u16,
    }

    #[repr(C)]
    #[derive(Default)]
    struct PixelFormatDescriptor {
        size: u16,
        version: u16,
        flags: u32,
        pixel_type: u8,
        color_bits: u8,
        red_bits: u8,
        red_shift: u8,
        green_bits: u8,
        green_shift: u8,
        blue_bits: u8,
        blue_shift: u8,
        alpha_bits: u8,
        alpha_shift: u8,
        accum_bits: u8,
        accum_red: u8,
        accum_green: u8,
        accum_blue: u8,
        accum_alpha: u8,
        depth_bits: u8,
        stencil_bits: u8,
        aux_buffers: u8,
        layer_type: u8,
        reserved: u8,
        layer_mask: u32,
        visible_mask: u32,
        damage_mask: u32,
    }

    #[link(name = "user32")]
    extern "system" {
        fn RegisterClassW(c: *const WndClassW) -> u16;
        fn CreateWindowExW(ex: u32, class: *const u16, name: *const u16, style: u32, x: i32, y: i32, w: i32, h: i32, parent: HWND, menu: *mut c_void, inst: *mut c_void, param: *mut c_void) -> HWND;
        fn DefWindowProcW(h: HWND, m: u32, w: usize, l: isize) -> isize;
        fn GetDC(h: HWND) -> HDC;
    }
    #[link(name = "gdi32")]
    extern "system" {
        fn ChoosePixelFormat(dc: HDC, pfd: *const PixelFormatDescriptor) -> i32;
        fn SetPixelFormat(dc: HDC, format: i32, pfd: *const PixelFormatDescriptor) -> i32;
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetModuleHandleW(name: *const u16) -> *mut c_void;
    }

    pub struct Context {
        lib: libloading::Library,
        dc: HDC,
        rc: HGLRC,
        make_current: unsafe extern "system" fn(HDC, HGLRC) -> i32,
        get_proc: unsafe extern "system" fn(*const c_char) -> *const c_void,
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    impl Context {
        pub unsafe fn new() -> Result<Self, String> {
            let lib = libloading::Library::new("opengl32.dll").map_err(|e| format!("no OpenGL ({e})"))?;
            let create: unsafe extern "system" fn(HDC) -> HGLRC = *lib.get(b"wglCreateContext\0").map_err(|e| e.to_string())?;
            let make_current: unsafe extern "system" fn(HDC, HGLRC) -> i32 = *lib.get(b"wglMakeCurrent\0").map_err(|e| e.to_string())?;
            let get_proc: unsafe extern "system" fn(*const c_char) -> *const c_void = *lib.get(b"wglGetProcAddress\0").map_err(|e| e.to_string())?;
            let class = wide("ReelforgeOfxGl");
            let inst = GetModuleHandleW(std::ptr::null());
            let wc = WndClassW {
                style: 0x0020, // CS_OWNDC
                wnd_proc: DefWindowProcW,
                cls_extra: 0,
                wnd_extra: 0,
                instance: inst,
                icon: std::ptr::null_mut(),
                cursor: std::ptr::null_mut(),
                background: std::ptr::null_mut(),
                menu_name: std::ptr::null(),
                class_name: class.as_ptr(),
            };
            RegisterClassW(&wc);
            // A window never shown: WGL needs one for its device context.
            let hwnd = CreateWindowExW(0, class.as_ptr(), class.as_ptr(), 0, 0, 0, 16, 16, std::ptr::null_mut(), std::ptr::null_mut(), inst, std::ptr::null_mut());
            if hwnd.is_null() {
                return Err("couldn't make a window for OpenGL".into());
            }
            let dc = GetDC(hwnd);
            let pfd = PixelFormatDescriptor {
                size: std::mem::size_of::<PixelFormatDescriptor>() as u16,
                version: 1,
                flags: 0x04 | 0x20, // PFD_DRAW_TO_WINDOW | PFD_SUPPORT_OPENGL
                color_bits: 32,
                alpha_bits: 8,
                depth_bits: 24,
                ..Default::default()
            };
            let format = ChoosePixelFormat(dc, &pfd);
            if format == 0 || SetPixelFormat(dc, format, &pfd) == 0 {
                return Err("no OpenGL pixel format".into());
            }
            let rc = create(dc);
            if rc.is_null() {
                return Err("couldn't make an OpenGL context".into());
            }
            Ok(Context { lib, dc, rc, make_current, get_proc })
        }

        pub unsafe fn make_current(&self, on: bool) -> Result<(), String> {
            let ok = if on { (self.make_current)(self.dc, self.rc) } else { (self.make_current)(std::ptr::null_mut(), std::ptr::null_mut()) };
            if ok == 0 { Err("couldn't make the OpenGL context current".into()) } else { Ok(()) }
        }

        pub unsafe fn proc_address(&self, name: &str) -> *const c_void {
            let n = CString::new(name).unwrap();
            let p = (self.get_proc)(n.as_ptr());
            // wglGetProcAddress gives 1, 2, 3 or -1 for "no" on some drivers.
            if !p.is_null() && !matches!(p as isize, 1 | 2 | 3 | -1) {
                return p;
            }
            self.lib.get::<*const c_void>(n.as_bytes_with_nul()).map(|s| *s).unwrap_or(std::ptr::null())
        }
    }
}

#[cfg(not(any(windows, all(unix, not(target_os = "macos")))))]
mod platform {
    use super::*;
    pub struct Context;
    impl Context {
        pub unsafe fn new() -> Result<Self, String> {
            Err("OpenGL plugins aren't supported on this system".into())
        }
        pub unsafe fn make_current(&self, _on: bool) -> Result<(), String> {
            Ok(())
        }
        pub unsafe fn proc_address(&self, _name: &str) -> *const c_void {
            std::ptr::null()
        }
    }
}
