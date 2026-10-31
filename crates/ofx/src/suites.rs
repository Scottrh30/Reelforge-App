//! The host side of OpenFX: the objects plugins get handles to (effects,
//! parameter sets, parameters, clips, images) and the suites they call.

use crate::props::*;
use std::cell::Cell;
use std::ffi::{c_char, c_int, c_uint, c_void, CStr, CString};
use std::sync::{Condvar, Mutex, OnceLock};

// ---- Objects behind the handles -------------------------------------------------

/// What a parameter holds now.
#[derive(Clone, Debug, Default)]
pub struct ParamValue {
    pub d: [f64; 4],
    pub i: [c_int; 4],
    pub s: CString,
}

pub struct ParamObj {
    pub name: String,
    pub kind: String,
    pub props: Box<PropSet>,
    pub value: ParamValue,
}

impl ParamObj {
    /// 1 = doubles, 2 = ints, 3 = a string, 0 = nothing to get or set; and how many.
    pub fn shape(&self) -> (c_int, c_int) {
        match self.kind.as_str() {
            "OfxParamTypeDouble" => (1, 1),
            "OfxParamTypeDouble2D" => (1, 2),
            "OfxParamTypeDouble3D" => (1, 3),
            "OfxParamTypeRGB" => (1, 3),
            "OfxParamTypeRGBA" => (1, 4),
            "OfxParamTypeInteger" | "OfxParamTypeBoolean" | "OfxParamTypeChoice" => (2, 1),
            "OfxParamTypeInteger2D" => (2, 2),
            "OfxParamTypeInteger3D" => (2, 3),
            "OfxParamTypeString" | "OfxParamTypeStrChoice" | "OfxParamTypeCustom" => (3, 1),
            _ => (0, 0),
        }
    }
}

pub struct ParamSetObj {
    pub props: Box<PropSet>,
    pub params: Vec<Box<ParamObj>>,
}

/// A buffer of pixels a clip shows the plugin.
#[derive(Clone, Copy)]
pub struct Pixels {
    pub data: *mut c_void,
    pub width: u32,
    pub height: u32,
    pub row_bytes: i32,
    pub depth: &'static str,
    pub components: &'static str,
    /// `data` is an OpenCL image (not memory or a buffer).
    pub cl_image: bool,
}

pub struct ClipObj {
    pub name: String,
    pub props: Box<PropSet>,
    /// The image at the render's time.
    pub pixels: Option<Pixels>,
    /// That time (frames).
    pub time: f64,
    /// Images at other times (frames), for plugins that read other frames.
    pub others: Vec<(f64, Pixels)>,
    /// While rendering with OpenGL: what textures can be made from.
    pub gl: Option<GlClip>,
}

impl ClipObj {
    pub fn new(name: String, props: Box<PropSet>) -> Self {
        ClipObj { name, props, pixels: None, time: 0.0, others: Vec::new(), gl: None }
    }

    /// The image at `time` (frames; the nearest within half a frame).
    pub fn pixels_at(&self, time: f64) -> Option<Pixels> {
        if (time - self.time).abs() < 0.5 {
            return self.pixels;
        }
        self.others.iter().find(|(t, _)| (t - time).abs() < 0.5).map(|o| o.1)
    }
}

/// A clip's pictures for an OpenGL render: float RGBA, rows bottom to top.
#[derive(Clone, Default)]
pub struct GlClip {
    /// (time in frames, pixels, width, height).
    pub frames: Vec<(f64, *const f32, u32, u32)>,
    /// The output: (texture, width, height).
    pub output: Option<(crate::gl::GLuint, u32, u32)>,
    /// Which texture format the plugin asked for.
    pub depth: &'static str,
}

pub struct EffectObj {
    pub props: Box<PropSet>,
    pub params: Box<ParamSetObj>,
    pub clips: Vec<Box<ClipObj>>,
    /// An instance (rather than a descriptor): clips get images.
    pub instance: bool,
}

impl EffectObj {
    pub fn new(props: PropSet) -> Box<Self> {
        let mut ps = PropSet::new();
        ps.s("OfxPropType", "OfxTypeParameterSet");
        Box::new(EffectObj { props: Box::new(props), params: Box::new(ParamSetObj { props: ps, params: Vec::new() }), clips: Vec::new(), instance: false })
    }
}

unsafe fn cstr<'a>(p: *const c_char) -> Option<&'a str> {
    if p.is_null() {
        None
    } else {
        CStr::from_ptr(p).to_str().ok()
    }
}

// ---- The image effect suite -----------------------------------------------------

unsafe extern "C" fn get_property_set(effect: *mut c_void, out: *mut *mut c_void) -> Status {
    let Some(e) = (effect as *mut EffectObj).as_mut() else { return ERR_BAD_HANDLE };
    *out = &mut *e.props as *mut PropSet as *mut c_void;
    OK
}

unsafe extern "C" fn get_param_set(effect: *mut c_void, out: *mut *mut c_void) -> Status {
    let Some(e) = (effect as *mut EffectObj).as_mut() else { return ERR_BAD_HANDLE };
    *out = &mut *e.params as *mut ParamSetObj as *mut c_void;
    OK
}

unsafe extern "C" fn clip_define(effect: *mut c_void, name: *const c_char, out: *mut *mut c_void) -> Status {
    guard(|| {
        let (Some(e), Some(n)) = ((effect as *mut EffectObj).as_mut(), cstr(name)) else { return ERR_BAD_HANDLE };
        if !e.clips.iter().any(|c| c.name == n) {
            let mut props = PropSet::new();
            props
                .s("OfxPropType", "OfxTypeClip")
                .s("OfxPropName", n)
                .s("OfxPropLabel", n)
                .s("OfxImageEffectPropSupportedComponents", "OfxImageComponentRGBA")
                .i("OfxImageEffectPropTemporalClipAccess", 0)
                .i("OfxImageClipPropOptional", 0)
                .i("OfxImageClipPropIsMask", 0)
                .s("OfxImageClipPropFieldExtraction", "OfxImageFieldDoubled")
                .i("OfxImageEffectPropSupportsTiles", 1);
            e.clips.push(Box::new(ClipObj::new(n.to_string(), props)));
        }
        let c = e.clips.iter_mut().find(|c| c.name == n).unwrap();
        if !out.is_null() {
            *out = &mut *c.props as *mut PropSet as *mut c_void;
        }
        OK
    })
}

unsafe extern "C" fn clip_get_handle(effect: *mut c_void, name: *const c_char, clip: *mut *mut c_void, props: *mut *mut c_void) -> Status {
    let (Some(e), Some(n)) = ((effect as *mut EffectObj).as_mut(), cstr(name)) else { return ERR_BAD_HANDLE };
    let Some(c) = e.clips.iter_mut().find(|c| c.name == n) else { return ERR_UNKNOWN };
    if !clip.is_null() {
        *clip = &mut **c as *mut ClipObj as *mut c_void;
    }
    if !props.is_null() {
        *props = &mut *c.props as *mut PropSet as *mut c_void;
    }
    OK
}

unsafe extern "C" fn clip_get_property_set(clip: *mut c_void, out: *mut *mut c_void) -> Status {
    let Some(c) = (clip as *mut ClipObj).as_mut() else { return ERR_BAD_HANDLE };
    *out = &mut *c.props as *mut PropSet as *mut c_void;
    OK
}

unsafe extern "C" fn clip_get_image(clip: *mut c_void, time: f64, _region: *const c_void, out: *mut *mut c_void) -> Status {
    guard(|| {
        let Some(c) = (clip as *mut ClipObj).as_mut() else { return ERR_BAD_HANDLE };
        // An unconnected clip (an optional mask), or a time not fetched: no image.
        let Some(px) = c.pixels_at(time) else { return FAILED };
        let bounds = [0, 0, px.width as c_int, px.height as c_int];
        let mut img = PropSet::new();
        img.s("OfxPropType", "OfxTypeImage")
            .p("OfxImagePropData", if px.cl_image { std::ptr::null_mut() } else { px.data })
            .p("OfxImageEffectPropOpenCLImage", if px.cl_image { px.data } else { std::ptr::null_mut() })
            .in_("OfxImagePropBounds", &bounds)
            .in_("OfxImagePropRegionOfDefinition", &bounds)
            .i("OfxImagePropRowBytes", px.row_bytes)
            .d("OfxImagePropPixelAspectRatio", 1.0)
            .s("OfxImageEffectPropPixelDepth", px.depth)
            .s("OfxImageEffectPropComponents", px.components)
            .s("OfxImageEffectPropPreMultiplication", "OfxImageAlphaPremultiplied")
            .dn("OfxImageEffectPropRenderScale", &[1.0, 1.0])
            .s("OfxImagePropField", "OfxFieldNone")
            .s("OfxImagePropUniqueIdentifier", &format!("{:p}", px.data));
        *out = Box::into_raw(img) as *mut c_void;
        OK
    })
}

unsafe extern "C" fn clip_release_image(image: *mut c_void) -> Status {
    if image.is_null() {
        return ERR_BAD_HANDLE;
    }
    drop(Box::from_raw(image as *mut PropSet));
    OK
}

#[repr(C)]
pub struct RectD {
    pub x1: f64,
    pub y1: f64,
    pub x2: f64,
    pub y2: f64,
}

unsafe extern "C" fn clip_get_rod(clip: *mut c_void, _time: f64, out: *mut RectD) -> Status {
    let Some(c) = (clip as *mut ClipObj).as_mut() else { return ERR_BAD_HANDLE };
    let (w, h) = match (c.pixels, c.gl.as_ref()) {
        (Some(p), _) => (p.width, p.height),
        (None, Some(g)) => g.output.map(|o| (o.1, o.2)).or(g.frames.first().map(|f| (f.2, f.3))).unwrap_or((0, 0)),
        _ => (0, 0),
    };
    *out = RectD { x1: 0.0, y1: 0.0, x2: w as f64, y2: h as f64 };
    OK
}

unsafe extern "C" fn abort(_effect: *mut c_void) -> c_int {
    0
}

// Memory: a block with its size in front.
const HEADER: usize = 16;

pub unsafe fn alloc(n: usize) -> *mut c_void {
    let Ok(layout) = std::alloc::Layout::from_size_align(n + HEADER, 16) else { return std::ptr::null_mut() };
    let p = std::alloc::alloc(layout);
    if p.is_null() {
        return p as *mut c_void;
    }
    (p as *mut usize).write(n);
    p.add(HEADER) as *mut c_void
}

pub unsafe fn free(p: *mut c_void) {
    if p.is_null() {
        return;
    }
    let base = (p as *mut u8).sub(HEADER);
    let n = (base as *mut usize).read();
    std::alloc::dealloc(base, std::alloc::Layout::from_size_align_unchecked(n + HEADER, 16));
}

unsafe extern "C" fn image_memory_alloc(_effect: *mut c_void, n: usize, out: *mut *mut c_void) -> Status {
    let p = alloc(n);
    *out = p;
    if p.is_null() { ERR_MEMORY } else { OK }
}
unsafe extern "C" fn image_memory_free(h: *mut c_void) -> Status {
    free(h);
    OK
}
unsafe extern "C" fn image_memory_lock(h: *mut c_void, out: *mut *mut c_void) -> Status {
    *out = h;
    OK
}
unsafe extern "C" fn image_memory_unlock(_h: *mut c_void) -> Status {
    OK
}

#[repr(C)]
pub struct ImageEffectSuiteV1 {
    get_property_set: unsafe extern "C" fn(*mut c_void, *mut *mut c_void) -> Status,
    get_param_set: unsafe extern "C" fn(*mut c_void, *mut *mut c_void) -> Status,
    clip_define: unsafe extern "C" fn(*mut c_void, *const c_char, *mut *mut c_void) -> Status,
    clip_get_handle: unsafe extern "C" fn(*mut c_void, *const c_char, *mut *mut c_void, *mut *mut c_void) -> Status,
    clip_get_property_set: unsafe extern "C" fn(*mut c_void, *mut *mut c_void) -> Status,
    clip_get_image: unsafe extern "C" fn(*mut c_void, f64, *const c_void, *mut *mut c_void) -> Status,
    clip_release_image: unsafe extern "C" fn(*mut c_void) -> Status,
    clip_get_region_of_definition: unsafe extern "C" fn(*mut c_void, f64, *mut RectD) -> Status,
    abort: unsafe extern "C" fn(*mut c_void) -> c_int,
    image_memory_alloc: unsafe extern "C" fn(*mut c_void, usize, *mut *mut c_void) -> Status,
    image_memory_free: unsafe extern "C" fn(*mut c_void) -> Status,
    image_memory_lock: unsafe extern "C" fn(*mut c_void, *mut *mut c_void) -> Status,
    image_memory_unlock: unsafe extern "C" fn(*mut c_void) -> Status,
}

pub static IMAGE_EFFECT_SUITE: ImageEffectSuiteV1 = ImageEffectSuiteV1 {
    get_property_set,
    get_param_set,
    clip_define,
    clip_get_handle,
    clip_get_property_set,
    clip_get_image,
    clip_release_image,
    clip_get_region_of_definition: clip_get_rod,
    abort,
    image_memory_alloc,
    image_memory_free,
    image_memory_lock,
    image_memory_unlock,
};

// ---- The parameter suite --------------------------------------------------------

unsafe extern "C" fn param_define(set: *mut c_void, kind: *const c_char, name: *const c_char, out: *mut *mut c_void) -> Status {
    guard(|| {
        let (Some(ps), Some(kind), Some(n)) = ((set as *mut ParamSetObj).as_mut(), cstr(kind), cstr(name)) else { return ERR_BAD_HANDLE };
        if ps.params.iter().any(|p| p.name == n) {
            return 6; // kOfxStatErrExists
        }
        let mut props = PropSet::new();
        props
            .s("OfxPropType", "OfxTypeParameter")
            .s("OfxPropName", n)
            .s("OfxPropLabel", n)
            .s("OfxParamPropType", kind)
            .s("OfxParamPropScriptName", n)
            .s("OfxParamPropHint", "")
            .s("OfxParamPropParent", "")
            .i("OfxParamPropSecret", 0)
            .i("OfxParamPropEnabled", 1)
            .i("OfxParamPropAnimates", 1)
            .i("OfxParamPropIsAnimating", 0)
            .i("OfxParamPropEvaluateOnChange", 1)
            .i("OfxParamPropPersistant", 1)
            .i("OfxParamPropCanUndo", 1)
            .s("OfxParamPropCacheInvalidation", "OfxParamInvalidateValueChange")
            .p("OfxParamPropDataPtr", std::ptr::null_mut());
        let mut p = Box::new(ParamObj { name: n.to_string(), kind: kind.to_string(), props, value: ParamValue::default() });
        if !out.is_null() {
            *out = &mut *p.props as *mut PropSet as *mut c_void;
        }
        ps.params.push(p);
        OK
    })
}

unsafe extern "C" fn param_get_handle(set: *mut c_void, name: *const c_char, param: *mut *mut c_void, props: *mut *mut c_void) -> Status {
    let (Some(ps), Some(n)) = ((set as *mut ParamSetObj).as_mut(), cstr(name)) else { return ERR_BAD_HANDLE };
    let Some(p) = ps.params.iter_mut().find(|p| p.name == n) else { return ERR_UNKNOWN };
    if !param.is_null() {
        *param = &mut **p as *mut ParamObj as *mut c_void;
    }
    if !props.is_null() {
        *props = &mut *p.props as *mut PropSet as *mut c_void;
    }
    OK
}

unsafe extern "C" fn param_set_get_property_set(set: *mut c_void, out: *mut *mut c_void) -> Status {
    let Some(ps) = (set as *mut ParamSetObj).as_mut() else { return ERR_BAD_HANDLE };
    *out = &mut *ps.props as *mut PropSet as *mut c_void;
    OK
}

unsafe extern "C" fn param_get_property_set(param: *mut c_void, out: *mut *mut c_void) -> Status {
    let Some(p) = (param as *mut ParamObj).as_mut() else { return ERR_BAD_HANDLE };
    *out = &mut *p.props as *mut PropSet as *mut c_void;
    OK
}

unsafe extern "C" fn param_get_num_keys(_p: *mut c_void, n: *mut c_uint) -> Status {
    *n = 0;
    OK
}
unsafe extern "C" fn param_get_key_time(_p: *mut c_void, _nth: c_uint, _t: *mut f64) -> Status {
    ERR_BAD_INDEX
}
unsafe extern "C" fn param_get_key_index(_p: *mut c_void, _t: f64, _dir: c_int, _i: *mut c_int) -> Status {
    FAILED
}
unsafe extern "C" fn param_delete_key(_p: *mut c_void, _t: f64) -> Status {
    OK
}
unsafe extern "C" fn param_delete_all_keys(_p: *mut c_void) -> Status {
    OK
}
unsafe extern "C" fn param_copy(to: *mut c_void, from: *mut c_void, _offset: f64, _range: *const c_void) -> Status {
    let (Some(t), Some(f)) = ((to as *mut ParamObj).as_mut(), (from as *mut ParamObj).as_ref()) else { return ERR_BAD_HANDLE };
    t.value = f.value.clone();
    OK
}
unsafe extern "C" fn param_edit_begin(_set: *mut c_void, _name: *const c_char) -> Status {
    OK
}
unsafe extern "C" fn param_edit_end(_set: *mut c_void) -> Status {
    OK
}

// The variadic ones live in shim.c, which calls these back.
unsafe extern "C" fn cb_kind(param: *mut c_void, dims: *mut c_int) -> c_int {
    let Some(p) = (param as *mut ParamObj).as_ref() else { return 0 };
    let (k, n) = p.shape();
    if !dims.is_null() {
        *dims = n;
    }
    k
}
unsafe extern "C" fn cb_get(param: *mut c_void, _time: f64, d: *mut f64, i: *mut c_int, s: *mut *const c_char) -> Status {
    let Some(p) = (param as *mut ParamObj).as_ref() else { return ERR_BAD_HANDLE };
    for k in 0..4 {
        *d.add(k) = p.value.d[k];
        *i.add(k) = p.value.i[k];
    }
    *s = p.value.s.as_ptr();
    OK
}
unsafe extern "C" fn cb_set(param: *mut c_void, d: *const f64, i: *const c_int, s: *const c_char) -> Status {
    guard(|| {
        let Some(p) = (param as *mut ParamObj).as_mut() else { return ERR_BAD_HANDLE };
        for k in 0..4 {
            p.value.d[k] = *d.add(k);
            p.value.i[k] = *i.add(k);
        }
        if !s.is_null() {
            p.value.s = CStr::from_ptr(s).to_owned();
        }
        OK
    })
}
unsafe extern "C" fn cb_message(kind: *const c_char, text: *const c_char, persistent: c_int) {
    let (k, t) = (cstr(kind).unwrap_or(""), cstr(text).unwrap_or(""));
    let k = k.trim_start_matches("OfxMessage");
    if k == "Error" || k == "Fatal" {
        log::warn!("OpenFX plugin {}: {t}", if persistent != 0 { "(persistent) error" } else { "error" });
    } else {
        log::info!("OpenFX plugin {k}: {t}");
    }
    *LAST_MESSAGE.lock().unwrap() = Some(format!("{k}: {t}"));
}

/// The last message a plugin showed (for error reports).
pub static LAST_MESSAGE: Mutex<Option<String>> = Mutex::new(None);

type Variadic0 = unsafe extern "C" fn(*mut c_void, ...) -> Status;
type Variadic1 = unsafe extern "C" fn(*mut c_void, f64, ...) -> Status;
type Variadic2 = unsafe extern "C" fn(*mut c_void, f64, f64, ...) -> Status;

#[repr(C)]
struct Callbacks {
    kind: unsafe extern "C" fn(*mut c_void, *mut c_int) -> c_int,
    get: unsafe extern "C" fn(*mut c_void, f64, *mut f64, *mut c_int, *mut *const c_char) -> Status,
    set: unsafe extern "C" fn(*mut c_void, *const f64, *const c_int, *const c_char) -> Status,
    message: unsafe extern "C" fn(*const c_char, *const c_char, c_int),
}

extern "C" {
    fn rf_ofx_shim_init(c: *const Callbacks);
    fn rf_ofx_param_get_value(h: *mut c_void, ...) -> Status;
    fn rf_ofx_param_get_value_at_time(h: *mut c_void, t: f64, ...) -> Status;
    fn rf_ofx_param_get_derivative(h: *mut c_void, t: f64, ...) -> Status;
    fn rf_ofx_param_get_integral(h: *mut c_void, t1: f64, t2: f64, ...) -> Status;
    fn rf_ofx_param_set_value(h: *mut c_void, ...) -> Status;
    fn rf_ofx_param_set_value_at_time(h: *mut c_void, t: f64, ...) -> Status;
    fn rf_ofx_message_suite(version: c_int) -> *const c_void;
}

#[repr(C)]
pub struct ParameterSuiteV1 {
    define: unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char, *mut *mut c_void) -> Status,
    get_handle: unsafe extern "C" fn(*mut c_void, *const c_char, *mut *mut c_void, *mut *mut c_void) -> Status,
    set_get_property_set: unsafe extern "C" fn(*mut c_void, *mut *mut c_void) -> Status,
    get_property_set: unsafe extern "C" fn(*mut c_void, *mut *mut c_void) -> Status,
    get_value: Variadic0,
    get_value_at_time: Variadic1,
    get_derivative: Variadic1,
    get_integral: Variadic2,
    set_value: Variadic0,
    set_value_at_time: Variadic1,
    get_num_keys: unsafe extern "C" fn(*mut c_void, *mut c_uint) -> Status,
    get_key_time: unsafe extern "C" fn(*mut c_void, c_uint, *mut f64) -> Status,
    get_key_index: unsafe extern "C" fn(*mut c_void, f64, c_int, *mut c_int) -> Status,
    delete_key: unsafe extern "C" fn(*mut c_void, f64) -> Status,
    delete_all_keys: unsafe extern "C" fn(*mut c_void) -> Status,
    copy: unsafe extern "C" fn(*mut c_void, *mut c_void, f64, *const c_void) -> Status,
    edit_begin: unsafe extern "C" fn(*mut c_void, *const c_char) -> Status,
    edit_end: unsafe extern "C" fn(*mut c_void) -> Status,
}

pub static PARAMETER_SUITE: ParameterSuiteV1 = ParameterSuiteV1 {
    define: param_define,
    get_handle: param_get_handle,
    set_get_property_set: param_set_get_property_set,
    get_property_set: param_get_property_set,
    get_value: rf_ofx_param_get_value,
    get_value_at_time: rf_ofx_param_get_value_at_time,
    get_derivative: rf_ofx_param_get_derivative,
    get_integral: rf_ofx_param_get_integral,
    set_value: rf_ofx_param_set_value,
    set_value_at_time: rf_ofx_param_set_value_at_time,
    get_num_keys: param_get_num_keys,
    get_key_time: param_get_key_time,
    get_key_index: param_get_key_index,
    delete_key: param_delete_key,
    delete_all_keys: param_delete_all_keys,
    copy: param_copy,
    edit_begin: param_edit_begin,
    edit_end: param_edit_end,
};

// ---- Memory ---------------------------------------------------------------------

unsafe extern "C" fn memory_alloc(_h: *mut c_void, n: usize, out: *mut *mut c_void) -> Status {
    let p = alloc(n);
    *out = p;
    if p.is_null() { ERR_MEMORY } else { OK }
}
unsafe extern "C" fn memory_free(p: *mut c_void) -> Status {
    free(p);
    OK
}

#[repr(C)]
pub struct MemorySuiteV1 {
    alloc: unsafe extern "C" fn(*mut c_void, usize, *mut *mut c_void) -> Status,
    free: unsafe extern "C" fn(*mut c_void) -> Status,
}
pub static MEMORY_SUITE: MemorySuiteV1 = MemorySuiteV1 { alloc: memory_alloc, free: memory_free };

// ---- Threads --------------------------------------------------------------------

thread_local! {
    static THREAD_INDEX: Cell<(u32, bool)> = const { Cell::new((0, false)) };
}

type ThreadFn = unsafe extern "C" fn(c_uint, c_uint, *mut c_void);

unsafe extern "C" fn multi_thread(func: Option<ThreadFn>, n: c_uint, arg: *mut c_void) -> Status {
    let Some(func) = func else { return ERR_VALUE };
    let n = n.max(1);
    if n == 1 {
        func(0, 1, arg);
        return OK;
    }
    let arg = arg as usize;
    let ok = std::panic::catch_unwind(|| {
        std::thread::scope(|s| {
            for i in 1..n {
                s.spawn(move || {
                    THREAD_INDEX.with(|c| c.set((i, true)));
                    func(i, n, arg as *mut c_void);
                });
            }
            let before = THREAD_INDEX.with(|c| c.replace((0, true)));
            func(0, n, arg as *mut c_void);
            THREAD_INDEX.with(|c| c.set(before));
        })
    });
    if ok.is_ok() { OK } else { FAILED }
}

unsafe extern "C" fn num_cpus(out: *mut c_uint) -> Status {
    *out = std::thread::available_parallelism().map_or(1, |n| n.get()) as c_uint;
    OK
}
unsafe extern "C" fn thread_index(out: *mut c_uint) -> Status {
    *out = THREAD_INDEX.with(|c| c.get().0);
    OK
}
unsafe extern "C" fn is_spawned() -> c_int {
    THREAD_INDEX.with(|c| c.get().1) as c_int
}

/// A mutex the same thread may lock several times.
struct RecursiveMutex {
    state: Mutex<(Option<std::thread::ThreadId>, u32)>,
    free: Condvar,
}

unsafe extern "C" fn mutex_create(out: *mut *mut c_void, count: c_int) -> Status {
    let owner = if count > 0 { Some(std::thread::current().id()) } else { None };
    let m = Box::new(RecursiveMutex { state: Mutex::new((owner, count.max(0) as u32)), free: Condvar::new() });
    *out = Box::into_raw(m) as *mut c_void;
    OK
}
unsafe extern "C" fn mutex_destroy(m: *mut c_void) -> Status {
    if m.is_null() {
        return ERR_BAD_HANDLE;
    }
    drop(Box::from_raw(m as *mut RecursiveMutex));
    OK
}
unsafe extern "C" fn mutex_lock(m: *mut c_void) -> Status {
    let Some(m) = (m as *mut RecursiveMutex).as_ref() else { return ERR_BAD_HANDLE };
    let me = std::thread::current().id();
    let mut st = m.state.lock().unwrap();
    while st.0.is_some_and(|o| o != me) {
        st = m.free.wait(st).unwrap();
    }
    st.0 = Some(me);
    st.1 += 1;
    OK
}
unsafe extern "C" fn mutex_unlock(m: *mut c_void) -> Status {
    let Some(m) = (m as *mut RecursiveMutex).as_ref() else { return ERR_BAD_HANDLE };
    let mut st = m.state.lock().unwrap();
    if st.0 != Some(std::thread::current().id()) || st.1 == 0 {
        return FAILED;
    }
    st.1 -= 1;
    if st.1 == 0 {
        st.0 = None;
        m.free.notify_one();
    }
    OK
}
unsafe extern "C" fn mutex_try_lock(m: *mut c_void) -> Status {
    let Some(mm) = (m as *mut RecursiveMutex).as_ref() else { return ERR_BAD_HANDLE };
    let me = std::thread::current().id();
    let mut st = mm.state.lock().unwrap();
    if st.0.is_some_and(|o| o != me) {
        return FAILED;
    }
    st.0 = Some(me);
    st.1 += 1;
    OK
}

#[repr(C)]
pub struct MultiThreadSuiteV1 {
    multi_thread: unsafe extern "C" fn(Option<ThreadFn>, c_uint, *mut c_void) -> Status,
    num_cpus: unsafe extern "C" fn(*mut c_uint) -> Status,
    index: unsafe extern "C" fn(*mut c_uint) -> Status,
    is_spawned: unsafe extern "C" fn() -> c_int,
    mutex_create: unsafe extern "C" fn(*mut *mut c_void, c_int) -> Status,
    mutex_destroy: unsafe extern "C" fn(*mut c_void) -> Status,
    mutex_lock: unsafe extern "C" fn(*mut c_void) -> Status,
    mutex_unlock: unsafe extern "C" fn(*mut c_void) -> Status,
    mutex_try_lock: unsafe extern "C" fn(*mut c_void) -> Status,
}
pub static MULTI_THREAD_SUITE: MultiThreadSuiteV1 = MultiThreadSuiteV1 {
    multi_thread,
    num_cpus,
    index: thread_index,
    is_spawned,
    mutex_create,
    mutex_destroy,
    mutex_lock,
    mutex_unlock,
    mutex_try_lock,
};

// ---- OpenGL rendering ----------------------------------------------------------

/// A texture handle's properties.
fn texture_props(index: crate::gl::GLuint, w: u32, h: u32, depth: &str, owned: bool) -> Box<PropSet> {
    let bounds = [0, 0, w as c_int, h as c_int];
    let mut p = PropSet::new();
    p.s("OfxPropType", "OfxTypeImage")
        .i("OfxImageEffectPropOpenGLTextureIndex", index as c_int)
        .i("OfxImageEffectPropOpenGLTextureTarget", crate::gl::TEXTURE_2D as c_int)
        .s("OfxImageEffectPropPixelDepth", depth)
        .s("OfxImageEffectPropComponents", "OfxImageComponentRGBA")
        .s("OfxImageEffectPropPreMultiplication", "OfxImageAlphaPremultiplied")
        .dn("OfxImageEffectPropRenderScale", &[1.0, 1.0])
        .d("OfxImagePropPixelAspectRatio", 1.0)
        .in_("OfxImagePropBounds", &bounds)
        .in_("OfxImagePropRegionOfDefinition", &bounds)
        .i("OfxImagePropRowBytes", (w * 16) as c_int)
        .s("OfxImagePropField", "OfxFieldNone")
        .s("OfxImagePropUniqueIdentifier", &format!("gl{index}"))
        .i("ReelforgeOwnedTexture", owned as c_int);
    p
}

/// The internal texture format for a pixel depth.
pub fn gl_format(depth: &str) -> crate::gl::GLenum {
    match depth {
        "OfxBitDepthByte" => crate::gl::RGBA8,
        "OfxBitDepthShort" => crate::gl::RGBA16,
        _ => crate::gl::RGBA32F,
    }
}

unsafe extern "C" fn clip_load_texture(clip: *mut c_void, time: f64, _format: *const c_char, _region: *const RectD, out: *mut *mut c_void) -> Status {
    guard(|| {
        let Some(c) = (clip as *mut ClipObj).as_mut() else { return ERR_BAD_HANDLE };
        let (Some(g), Some(f)) = (c.gl.as_ref(), crate::gl::fns()) else { return FAILED };
        if c.name == "Output" {
            let Some((tex, w, h)) = g.output else { return FAILED };
            *out = Box::into_raw(texture_props(tex, w, h, g.depth, false)) as *mut c_void;
            return OK;
        }
        let Some(&(_, data, w, h)) = g.frames.iter().find(|fr| (fr.0 - time).abs() < 0.5) else { return FAILED };
        let mut tex = 0;
        (f.gen_textures)(1, &mut tex);
        (f.bind_texture)(crate::gl::TEXTURE_2D, tex);
        (f.pixel_storei)(crate::gl::UNPACK_ALIGNMENT, 4);
        (f.tex_image_2d)(crate::gl::TEXTURE_2D, 0, gl_format(g.depth) as c_int, w as c_int, h as c_int, 0, crate::gl::RGBA, crate::gl::FLOAT, data as *const c_void);
        (f.tex_parameteri)(crate::gl::TEXTURE_2D, crate::gl::TEXTURE_MIN_FILTER, crate::gl::LINEAR);
        (f.tex_parameteri)(crate::gl::TEXTURE_2D, crate::gl::TEXTURE_MAG_FILTER, crate::gl::LINEAR);
        (f.bind_texture)(crate::gl::TEXTURE_2D, 0);
        if (f.get_error)() == 0x0505 {
            (f.delete_textures)(1, &tex);
            return ERR_MEMORY;
        }
        *out = Box::into_raw(texture_props(tex, w, h, g.depth, true)) as *mut c_void;
        OK
    })
}

unsafe extern "C" fn clip_free_texture(handle: *mut c_void) -> Status {
    if handle.is_null() {
        return ERR_BAD_HANDLE;
    }
    let p = Box::from_raw(handle as *mut PropSet);
    if p.get_int("ReelforgeOwnedTexture", 0) == Some(1) {
        if let (Some(f), Some(t)) = (crate::gl::fns(), p.get_int("OfxImageEffectPropOpenGLTextureIndex", 0)) {
            let t = t as crate::gl::GLuint;
            (f.delete_textures)(1, &t);
        }
    }
    OK
}

unsafe extern "C" fn flush_resources() -> Status {
    REPLY_DEFAULT
}

#[repr(C)]
pub struct OpenGLRenderSuiteV1 {
    clip_load_texture: unsafe extern "C" fn(*mut c_void, f64, *const c_char, *const RectD, *mut *mut c_void) -> Status,
    clip_free_texture: unsafe extern "C" fn(*mut c_void) -> Status,
    flush_resources: unsafe extern "C" fn() -> Status,
}
pub static OPENGL_SUITE: OpenGLRenderSuiteV1 = OpenGLRenderSuiteV1 { clip_load_texture, clip_free_texture, flush_resources };

// ---- Progress and the timeline (renders here aren't interactive) -------------------

unsafe extern "C" fn progress_start_v1(_e: *mut c_void, _label: *const c_char) -> Status {
    OK
}
unsafe extern "C" fn progress_start_v2(_e: *mut c_void, _label: *const c_char, _id: *const c_char) -> Status {
    OK
}
unsafe extern "C" fn progress_update(_e: *mut c_void, _p: f64) -> Status {
    OK
}
unsafe extern "C" fn progress_end(_e: *mut c_void) -> Status {
    OK
}
#[repr(C)]
pub struct ProgressSuiteV1 {
    start: unsafe extern "C" fn(*mut c_void, *const c_char) -> Status,
    update: unsafe extern "C" fn(*mut c_void, f64) -> Status,
    end: unsafe extern "C" fn(*mut c_void) -> Status,
}
#[repr(C)]
pub struct ProgressSuiteV2 {
    start: unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char) -> Status,
    update: unsafe extern "C" fn(*mut c_void, f64) -> Status,
    end: unsafe extern "C" fn(*mut c_void) -> Status,
}
pub static PROGRESS_V1: ProgressSuiteV1 = ProgressSuiteV1 { start: progress_start_v1, update: progress_update, end: progress_end };
pub static PROGRESS_V2: ProgressSuiteV2 = ProgressSuiteV2 { start: progress_start_v2, update: progress_update, end: progress_end };

unsafe extern "C" fn timeline_get_time(e: *mut c_void, t: *mut f64) -> Status {
    let Some(e) = (e as *mut EffectObj).as_ref() else { return ERR_BAD_HANDLE };
    *t = e.props.get_dbls("OfxPropTime").first().copied().unwrap_or(0.0);
    OK
}
unsafe extern "C" fn timeline_goto(_e: *mut c_void, _t: f64) -> Status {
    OK
}
unsafe extern "C" fn timeline_bounds(e: *mut c_void, a: *mut f64, b: *mut f64) -> Status {
    let Some(e) = (e as *mut EffectObj).as_ref() else { return ERR_BAD_HANDLE };
    let r = e.props.get_dbls("OfxImageEffectPropFrameRange");
    *a = r.first().copied().unwrap_or(0.0);
    *b = r.get(1).copied().unwrap_or(0.0);
    OK
}
#[repr(C)]
pub struct TimeLineSuiteV1 {
    get_time: unsafe extern "C" fn(*mut c_void, *mut f64) -> Status,
    goto_time: unsafe extern "C" fn(*mut c_void, f64) -> Status,
    get_time_bounds: unsafe extern "C" fn(*mut c_void, *mut f64, *mut f64) -> Status,
}
pub static TIMELINE_SUITE: TimeLineSuiteV1 = TimeLineSuiteV1 { get_time: timeline_get_time, goto_time: timeline_goto, get_time_bounds: timeline_bounds };

// ---- The host ---------------------------------------------------------------------

#[repr(C)]
pub struct OfxHost {
    pub host: *mut c_void,
    pub fetch_suite: unsafe extern "C" fn(*mut c_void, *const c_char, c_int) -> *const c_void,
}

struct HostPtr(*mut OfxHost);
// SAFETY: the host struct and its property set are made once and never change.
unsafe impl Send for HostPtr {}
unsafe impl Sync for HostPtr {}

unsafe extern "C" fn fetch_suite(_host: *mut c_void, name: *const c_char, version: c_int) -> *const c_void {
    let Some(n) = cstr(name) else { return std::ptr::null() };
    match (n, version) {
        ("OfxPropertySuite", 1) => &PROPERTY_SUITE as *const _ as *const c_void,
        ("OfxImageEffectSuite", 1) => &IMAGE_EFFECT_SUITE as *const _ as *const c_void,
        ("OfxParameterSuite", 1) => &PARAMETER_SUITE as *const _ as *const c_void,
        ("OfxMemorySuite", 1) => &MEMORY_SUITE as *const _ as *const c_void,
        ("OfxMultiThreadSuite", 1) => &MULTI_THREAD_SUITE as *const _ as *const c_void,
        ("OfxMessageSuite", 1 | 2) => rf_ofx_message_suite(version),
        ("OfxProgressSuite", 1) => &PROGRESS_V1 as *const _ as *const c_void,
        ("OfxProgressSuite", 2) => &PROGRESS_V2 as *const _ as *const c_void,
        ("OfxTimeLineSuite", 1) => &TIMELINE_SUITE as *const _ as *const c_void,
        ("OfxImageEffectOpenGLRenderSuite", 1) if gl_offered() => &OPENGL_SUITE as *const _ as *const c_void,
        ("OfxOpenCLProgramSuite", 1) if cl_works() => &OPENCL_PROGRAM_SUITE as *const _ as *const c_void,
        // Interact, parametric parameters, drawing…: not offered.
        _ => std::ptr::null(),
    }
}

/// OpenGL works here (found out once).
pub fn gl_works() -> bool {
    static WORKS: OnceLock<bool> = OnceLock::new();
    *WORKS.get_or_init(|| match crate::gl::available() {
        Ok(r) => {
            log::info!("OpenFX: plugins can render with OpenGL on {r}");
            true
        }
        Err(_) => false,
    })
}

/// Plugins may render with OpenGL (it works, and it isn't switched off).
pub fn gl_offered() -> bool {
    crate::GPU.load(std::sync::atomic::Ordering::Relaxed) && gl_works()
}

pub fn cuda_works() -> bool {
    crate::cuda::available().is_ok()
}

pub fn cl_works() -> bool {
    crate::cl::available().is_ok()
}

pub fn cuda_offered() -> bool {
    crate::GPU.load(std::sync::atomic::Ordering::Relaxed) && cuda_works()
}

pub fn cl_offered() -> bool {
    crate::GPU.load(std::sync::atomic::Ordering::Relaxed) && cl_works()
}

// The OpenCL program suite: builds a plugin's kernels on the host's context.
unsafe extern "C" fn cl_compile_program(source: *const c_char, _optional: c_int, result: *mut c_void) -> Status {
    guard(|| {
        let (Some(src), false) = (cstr(source), result.is_null()) else { return ERR_BAD_HANDLE };
        match crate::cl::get().and_then(|c| c.compile(src)) {
            Ok(prog) => {
                *(result as *mut *mut c_void) = prog;
                OK
            }
            Err(e) => {
                log::warn!("OpenFX: {e}");
                FAILED
            }
        }
    })
}

#[repr(C)]
pub struct OpenCLProgramSuiteV1 {
    compile_program: unsafe extern "C" fn(*const c_char, c_int, *mut c_void) -> Status,
}
pub static OPENCL_PROGRAM_SUITE: OpenCLProgramSuiteV1 = OpenCLProgramSuiteV1 { compile_program: cl_compile_program };

/// The one host every plugin is given.
pub fn host() -> *mut OfxHost {
    static HOST: OnceLock<HostPtr> = OnceLock::new();
    HOST.get_or_init(|| {
        static CALLBACKS: Callbacks = Callbacks { kind: cb_kind, get: cb_get, set: cb_set, message: cb_message };
        // SAFETY: hands the shim a static table of functions.
        unsafe { rf_ofx_shim_init(&CALLBACKS) };
        let mut p = PropSet::new();
        p.s("OfxPropType", "OfxTypeImageEffectHost")
            .s("OfxPropName", "com.reelforge.Reelforge")
            .s("OfxPropLabel", "Reelforge")
            .in_("OfxPropAPIVersion", &[1, 4])
            .in_("OfxPropVersion", &[0, 1, 0])
            .s("OfxPropVersionLabel", env!("CARGO_PKG_VERSION"))
            .i("OfxImageEffectHostPropIsBackground", 0)
            .i("OfxImageEffectPropSupportsOverlays", 0)
            .i("OfxImageEffectPropSupportsMultiResolution", 0)
            .i("OfxImageEffectPropSupportsTiles", 0)
            .i("OfxImageEffectPropTemporalClipAccess", 1)
            .sn("OfxImageEffectPropSupportedComponents", &["OfxImageComponentRGBA", "OfxImageComponentRGB"])
            .sn("OfxImageEffectPropSupportedContexts", &["OfxImageEffectContextFilter", "OfxImageEffectContextGeneral"])
            .sn("OfxImageEffectPropSupportedPixelDepths", &["OfxBitDepthFloat", "OfxBitDepthShort", "OfxBitDepthByte"])
            .i("OfxImageEffectPropMultipleClipDepths", 0)
            .i("OfxImageEffectPropSupportsMultipleClipPARs", 0)
            .i("OfxImageEffectPropSetableFrameRate", 0)
            .i("OfxImageEffectPropSetableFielding", 0)
            .i("OfxImageEffectInstancePropSequentialRender", 0)
            .s("OfxImageEffectPropOpenGLRenderSupported", if gl_works() { "true" } else { "false" })
            .s("OfxImageEffectPropCPURenderSupported", "true")
            .s("OfxImageEffectPropCudaRenderSupported", if cuda_works() { "true" } else { "false" })
            .s("OfxImageEffectPropCudaStreamSupported", if cuda_works() { "true" } else { "false" })
            // OpenCL buffers ("RenderSupported") and images ("Supported").
            .s("OfxImageEffectPropOpenCLRenderSupported", if cl_works() { "true" } else { "false" })
            .s("OfxImageEffectPropOpenCLSupported", if cl_works() { "true" } else { "false" })
            .s("OfxImageEffectPropMetalRenderSupported", "false")
            .sn("OfxOpenGLPropPixelDepth", &["OfxBitDepthFloat", "OfxBitDepthShort", "OfxBitDepthByte"])
            .i("OfxParamHostPropSupportsCustomInteract", 0)
            .i("OfxParamHostPropSupportsCustomAnimation", 0)
            .i("OfxParamHostPropSupportsStringAnimation", 0)
            .i("OfxParamHostPropSupportsChoiceAnimation", 0)
            .i("OfxParamHostPropSupportsBooleanAnimation", 0)
            .i("OfxParamHostPropSupportsParametricAnimation", 0)
            .i("OfxParamHostPropSupportsStrChoice", 1)
            .i("OfxParamHostPropSupportsStrChoiceAnimation", 0)
            .i("OfxParamHostPropMaxParameters", -1)
            .i("OfxParamHostPropMaxPages", 0)
            .in_("OfxParamHostPropPageRowColumnCount", &[0, 0])
            .p("OfxPropHostOSHandle", std::ptr::null_mut());
        let props = Box::into_raw(p) as *mut c_void;
        HostPtr(Box::into_raw(Box::new(OfxHost { host: props, fetch_suite })))
    })
    .0
}
