//! An OpenFX host: loads image effect plugins (`.ofx` bundles, the plugin
//! format of Resolve, Nuke, Vegas, HitFilm and others) and renders frames
//! with them on the CPU.
//!
//! What a plugin gets: the Filter context (or General, with only its Source
//! clip connected); one frame at a time, the whole frame, at full scale, in
//! 32-bit float RGBA (or 16-bit, 8-bit or RGB when that's all it takes),
//! premultiplied; parameters set by the host before each render (Reelforge
//! keyframes them); the property, image effect, parameter, memory,
//! multithread, message, progress and timeline suites; other frames of the
//! source (temporal access: the host asks which, with GetFramesNeeded, and
//! has them ready); and OpenGL rendering (the OpenGL render suite, on the
//! host's own context; see [`gl`]), falling back to the processor. Not
//! offered: overlays drawn by the plugin and custom interacts (Reelforge
//! draws handles for a plugin's position parameters instead), CUDA, Metal and
//! OpenCL rendering, parametric parameters and the Generator, Transition and
//! Retimer contexts. A plugin that needs those may refuse to load, or may
//! still work.
//!
//! The headers this follows are the OpenFX standard's (BSD-3-Clause), from
//! the Academy Software Foundation; no OpenFX code is copied in here.

pub mod gl;
mod props;
mod suites;

use props::PropSet;
use std::ffi::{c_char, c_int, c_uint, c_void, CStr, CString};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use suites::{EffectObj, OfxHost, ParamObj};

/// Plugins that can render with OpenGL do (switch off to keep them all on the processor).
pub static GPU: AtomicBool = AtomicBool::new(true);

pub fn set_gpu(on: bool) {
    GPU.store(on, Ordering::Relaxed);
}

/// Whether OpenGL plugins can run on the graphics card here (and on what).
pub fn gpu_status() -> Result<String, String> {
    gl::available()
}

pub use props::Status;

#[repr(C)]
struct OfxPlugin {
    plugin_api: *const c_char,
    api_version: c_int,
    plugin_identifier: *const c_char,
    version_major: c_uint,
    version_minor: c_uint,
    set_host: Option<unsafe extern "C" fn(*mut OfxHost)>,
    main_entry: Option<unsafe extern "C" fn(*const c_char, *const c_void, *mut c_void, *mut c_void) -> c_int>,
}

/// A loaded plugin binary (one `.ofx` file can hold several plugins). Kept
/// loaded for as long as any of its plugins are in use.
pub struct Library {
    pub path: PathBuf,
    _lib: libloading::Library,
}

/// What kind of value a parameter takes.
#[derive(Clone, Debug, PartialEq)]
pub enum ParamKind {
    /// `dims` numbers (1–3). `normalised`: given as a fraction of the frame.
    /// `point`: a position in the picture (x, y; y up, as OpenFX counts),
    /// which a host shows as a handle to drag.
    Double { dims: usize, normalised: bool, point: bool },
    Int { dims: usize },
    Bool,
    Choice(Vec<String>),
    /// A menu whose value is a string: the labels shown, and the value of each.
    StrChoice { labels: Vec<String>, values: Vec<String> },
    /// Colours: 0–1 per channel.
    Rgb,
    Rgba,
    String,
    /// Not values (pages, groups, buttons, custom data…).
    Other(String),
}

/// One of a plugin's parameters.
#[derive(Clone, Debug)]
pub struct ParamInfo {
    pub name: String,
    pub label: String,
    pub hint: String,
    pub kind: ParamKind,
    pub default: Vec<f64>,
    /// Slider range (the display range, or the hard one), per dimension.
    pub min: Vec<f64>,
    pub max: Vec<f64>,
    /// Hidden from users.
    pub secret: bool,
    pub default_string: String,
}

/// A parameter's value, to render with.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Numbers(Vec<f64>),
    Text(String),
}

/// A plugin, described and ready to make instances of.
pub struct Plugin {
    pub library: Arc<Library>,
    /// The plugin's own identifier, e.g. "uk.co.thefoundry.OfxInvertExample".
    pub identifier: String,
    pub version: (u32, u32),
    pub label: String,
    /// Where the plugin files itself, e.g. "OFX Example" (`/` for sub-groups).
    pub group: String,
    pub description: String,
    pub context: &'static str,
    pub params: Vec<ParamInfo>,
    /// It reads other frames of its source (and says which with GetFramesNeeded).
    pub temporal: bool,
    /// It can render with OpenGL (`Some(true)`: only that way).
    pub opengl: Option<bool>,
    /// The bit depth frames are given in.
    depth: &'static str,
    /// The texture depth for OpenGL renders.
    gl_depth: &'static str,
    components: &'static str,
    entry: *const OfxPlugin,
    /// The context descriptor (plugins may keep pointers into it).
    descriptor: Box<EffectObj>,
}

// SAFETY: a plugin is only driven from one thread at a time (each instance
// is owned by one renderer); the raw pointers are to the loaded library's
// static data and to the host-owned descriptor.
unsafe impl Send for Plugin {}
unsafe impl Sync for Plugin {}

fn c(s: &str) -> CString {
    CString::new(s).unwrap()
}

impl Plugin {
    unsafe fn call(&self, action: &str, handle: *const c_void, in_args: Option<&mut PropSet>, out_args: Option<&mut PropSet>) -> c_int {
        call(self.entry, action, handle, in_args, out_args)
    }
}

unsafe fn call(entry: *const OfxPlugin, action: &str, handle: *const c_void, in_args: Option<&mut PropSet>, out_args: Option<&mut PropSet>) -> c_int {
    let Some(main) = (*entry).main_entry else { return props::FAILED };
    let a = c(action);
    let i = in_args.map_or(std::ptr::null_mut(), |p| p as *mut PropSet as *mut c_void);
    let o = out_args.map_or(std::ptr::null_mut(), |p| p as *mut PropSet as *mut c_void);
    main(a.as_ptr(), handle, i, o)
}

/// "Not handled" (kOfxStatReplyDefault) counts as fine for optional actions.
fn ok(s: c_int) -> bool {
    s == props::OK || s == props::REPLY_DEFAULT
}

/// The binary inside a bundle (`Name.ofx.bundle/Contents/<arch>/Name.ofx`),
/// or the file itself.
pub fn binary_of(path: &Path) -> Option<PathBuf> {
    if path.is_file() {
        return Some(path.to_path_buf());
    }
    let arch = if cfg!(windows) { "Win64" } else if cfg!(target_os = "macos") { "MacOS" } else { "Linux-x86-64" };
    let stem = path.file_name()?.to_str()?.strip_suffix(".bundle")?;
    let bin = path.join("Contents").join(arch).join(stem);
    bin.is_file().then_some(bin)
}

/// Plugin bundles (and bare `.ofx` files) in `dirs` and the folders below them.
pub fn find_bundles(dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack: Vec<(PathBuf, usize)> = dirs.iter().map(|d| (d.clone(), 0)).collect();
    while let Some((d, depth)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        let mut entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
        entries.sort();
        for p in entries {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("").to_lowercase();
            if name.ends_with(".ofx.bundle") && p.is_dir() {
                out.push(p);
            } else if name.ends_with(".ofx") && p.is_file() {
                out.push(p);
            } else if p.is_dir() && depth < 3 {
                stack.push((p, depth + 1));
            }
        }
    }
    out
}

/// Where OpenFX plugins are installed on this system: the `OFX_PLUGIN_PATH`
/// folders, then the standard place.
pub fn standard_folders() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::env::var_os("OFX_PLUGIN_PATH").map(|v| std::env::split_paths(&v).collect()).unwrap_or_default();
    if cfg!(windows) {
        let common = std::env::var_os("CommonProgramFiles").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Program Files\Common Files"));
        out.push(common.join("OFX").join("Plugins"));
    } else if cfg!(target_os = "macos") {
        out.push(PathBuf::from("/Library/OFX/Plugins"));
    } else {
        out.push(PathBuf::from("/usr/OFX/Plugins"));
    }
    out
}

/// Load the binary of `bundle` and describe every image effect in it that
/// works in the Filter (or General) context. Errors per plugin come back
/// alongside the ones that loaded.
///
/// # Safety
/// This runs the plugin's own code, which can do anything (including crash).
pub unsafe fn load(bundle: &Path) -> Result<(Vec<Plugin>, Vec<String>), String> {
    let bin = binary_of(bundle).ok_or_else(|| format!("{}: no plugin binary for this system in it", bundle.display()))?;
    let lib = libloading::Library::new(&bin).map_err(|e| format!("{}: {e}", bin.display()))?;
    let count: libloading::Symbol<unsafe extern "C" fn() -> c_int> = lib.get(b"OfxGetNumberOfPlugins\0").map_err(|_| format!("{}: not an OpenFX plugin", bin.display()))?;
    let get: libloading::Symbol<unsafe extern "C" fn(c_int) -> *const OfxPlugin> = lib.get(b"OfxGetPlugin\0").map_err(|_| format!("{}: not an OpenFX plugin", bin.display()))?;
    // Newer bundles take the host for the whole binary first.
    if let Ok(set_host) = lib.get::<unsafe extern "C" fn(*const OfxHost) -> c_int>(b"OfxSetHost\0") {
        set_host(suites::host());
    }
    let n = count();
    let entries: Vec<*const OfxPlugin> = (0..n.max(0)).map(|i| get(i)).filter(|p| !p.is_null()).collect();
    let library = Arc::new(Library { path: bin.clone(), _lib: lib });
    let mut plugins = Vec::new();
    let mut errors = Vec::new();
    for e in entries {
        match describe(e, &library) {
            Ok(p) => plugins.push(p),
            Err(err) => errors.push(err),
        }
    }
    Ok((plugins, errors))
}

unsafe fn cstring_of(p: *const c_char) -> String {
    if p.is_null() { String::new() } else { CStr::from_ptr(p).to_string_lossy().into_owned() }
}

unsafe fn describe(entry: *const OfxPlugin, library: &Arc<Library>) -> Result<Plugin, String> {
    let p = &*entry;
    let identifier = cstring_of(p.plugin_identifier);
    let api = cstring_of(p.plugin_api);
    if api != "OfxImageEffectPluginAPI" || p.api_version != 1 {
        return Err(format!("{identifier}: not an image effect ({api} v{})", p.api_version));
    }
    let Some(set_host) = p.set_host else { return Err(format!("{identifier}: no setHost")) };
    set_host(suites::host());
    let st = call(entry, "OfxActionLoad", std::ptr::null(), None, None);
    if !ok(st) {
        return Err(format!("{identifier}: failed to load ({st})"));
    }
    // The general description.
    let mut base = PropSet::default();
    base.s("OfxPropType", "OfxTypeImageEffect")
        .s("OfxPropLabel", &identifier)
        .s("OfxImageEffectPluginPropGrouping", "")
        .sn("OfxImageEffectPropSupportedContexts", &[])
        .sn("OfxImageEffectPropSupportedPixelDepths", &[])
        .i("OfxImageEffectPluginPropSingleInstance", 0)
        .s("OfxImageEffectPluginRenderThreadSafety", "OfxImageEffectRenderInstanceSafe")
        .i("OfxImageEffectPluginPropHostFrameThreading", 0)
        .i("OfxImageEffectPropSupportsMultiResolution", 1)
        .i("OfxImageEffectPropSupportsTiles", 1)
        .i("OfxImageEffectPropTemporalClipAccess", 0)
        .i("OfxImageEffectPluginPropFieldRenderTwiceAlways", 1)
        .i("OfxImageEffectPropMultipleClipDepths", 0)
        .s("OfxImageEffectPropOpenGLRenderSupported", "false")
        .s("OfxImageEffectPropCPURenderSupported", "true")
        .s("OfxPropPluginDescription", "");
    let mut general = EffectObj::new(base);
    let st = call(entry, "OfxActionDescribe", &mut *general as *mut EffectObj as *const c_void, None, None);
    if !ok(st) {
        return Err(format!("{identifier}: failed to describe itself ({st})"));
    }
    let contexts = general.props.strs("OfxImageEffectPropSupportedContexts");
    let context = if contexts.iter().any(|c| c == "OfxImageEffectContextFilter") {
        "OfxImageEffectContextFilter"
    } else if contexts.iter().any(|c| c == "OfxImageEffectContextGeneral") {
        "OfxImageEffectContextGeneral"
    } else {
        return Err(format!("{identifier}: only works as {} (not an effect on a clip)", contexts.join(", ").replace("OfxImageEffectContext", "")));
    };
    // In that context: a copy of the description, which the plugin adds its clips and parameters to.
    let mut desc = EffectObj::new((*general.props).clone());
    let mut args = PropSet::default();
    args.s("OfxImageEffectPropContext", context);
    let st = call(entry, "OfxImageEffectActionDescribeInContext", &mut *desc as *mut EffectObj as *const c_void, Some(&mut args), None);
    if !ok(st) {
        return Err(format!("{identifier}: failed to describe itself in {context} ({st})"));
    }
    if !desc.clips.iter().any(|c| c.name == "Source") || !desc.clips.iter().any(|c| c.name == "Output") {
        return Err(format!("{identifier}: no Source and Output clips"));
    }
    if let Some(other) = desc.clips.iter().find(|c| c.name != "Source" && c.name != "Output" && c.props.get_int("OfxImageClipPropOptional", 0) != Some(1)) {
        return Err(format!("{identifier}: needs a second input clip ({})", other.name));
    }
    let depths = desc.props.strs("OfxImageEffectPropSupportedPixelDepths");
    let depth = ["OfxBitDepthFloat", "OfxBitDepthShort", "OfxBitDepthByte"].into_iter().find(|d| depths.is_empty() || depths.iter().any(|x| x == d)).ok_or_else(|| format!("{identifier}: no bit depth Reelforge can give ({})", depths.join(", ")))?;
    let src = desc.clips.iter().find(|c| c.name == "Source").unwrap();
    let comps = src.props.strs("OfxImageEffectPropSupportedComponents");
    let components = if comps.is_empty() || comps.iter().any(|c| c == "OfxImageComponentRGBA") {
        "OfxImageComponentRGBA"
    } else if comps.iter().any(|c| c == "OfxImageComponentRGB") {
        "OfxImageComponentRGB"
    } else {
        return Err(format!("{identifier}: only works on {}", comps.join(", ")));
    };
    let params = desc.params.params.iter().map(|p| param_info(p)).collect();
    let label = desc.props.get_str("OfxPropLabel", 0).filter(|s| !s.is_empty()).unwrap_or_else(|| identifier.clone());
    let temporal = desc.props.get_int("OfxImageEffectPropTemporalClipAccess", 0) == Some(1) || src.props.get_int("OfxImageEffectPropTemporalClipAccess", 0) == Some(1);
    let gl = desc.props.get_str("OfxImageEffectPropOpenGLRenderSupported", 0).unwrap_or_default();
    let cpu = desc.props.get_str("OfxImageEffectPropCPURenderSupported", 0).is_none_or(|s| s != "false");
    let opengl = match gl.as_str() {
        "true" | "needed" => Some(gl == "needed" || !cpu),
        _ => None,
    };
    let gl_depths = desc.props.strs("OfxOpenGLPropPixelDepth");
    let gl_depth = ["OfxBitDepthFloat", "OfxBitDepthShort", "OfxBitDepthByte"].into_iter().find(|d| gl_depths.is_empty() || gl_depths.iter().any(|x| x == d)).unwrap_or("OfxBitDepthFloat");
    Ok(Plugin {
        library: library.clone(),
        version: (p.version_major, p.version_minor),
        group: desc.props.get_str("OfxImageEffectPluginPropGrouping", 0).unwrap_or_default(),
        description: desc.props.get_str("OfxPropPluginDescription", 0).unwrap_or_default(),
        identifier,
        label,
        context,
        params,
        temporal,
        opengl,
        depth,
        gl_depth,
        components,
        entry,
        descriptor: desc,
    })
}

fn param_info(p: &ParamObj) -> ParamInfo {
    let ps = &p.props;
    let label = ps.get_str("OfxPropLabel", 0).filter(|s| !s.is_empty()).unwrap_or_else(|| p.name.clone());
    let dtype = ps.get_str("OfxParamPropDoubleType", 0).unwrap_or_default();
    let normalised = dtype.contains("Normalised") || ps.get_str("OfxParamPropDefaultCoordinateSystem", 0).is_some_and(|t| t == "OfxParamCoordinatesNormalised");
    let kind = match p.kind.as_str() {
        "OfxParamTypeDouble" => ParamKind::Double { dims: 1, normalised, point: false },
        // XY, XYAbsolute, NormalisedXY, NormalisedXYAbsolute: a place in the picture.
        "OfxParamTypeDouble2D" => ParamKind::Double { dims: 2, normalised, point: dtype.contains("XY") },
        "OfxParamTypeDouble3D" => ParamKind::Double { dims: 3, normalised: false, point: false },
        "OfxParamTypeInteger" => ParamKind::Int { dims: 1 },
        "OfxParamTypeInteger2D" => ParamKind::Int { dims: 2 },
        "OfxParamTypeInteger3D" => ParamKind::Int { dims: 3 },
        "OfxParamTypeBoolean" => ParamKind::Bool,
        "OfxParamTypeChoice" => ParamKind::Choice(ps.strs("OfxParamPropChoiceOption")),
        "OfxParamTypeStrChoice" => ParamKind::StrChoice { labels: ps.strs("OfxParamPropChoiceOption"), values: ps.strs("OfxParamPropChoiceEnum") },
        "OfxParamTypeRGB" => ParamKind::Rgb,
        "OfxParamTypeRGBA" => ParamKind::Rgba,
        "OfxParamTypeString" => ParamKind::String,
        other => ParamKind::Other(other.to_string()),
    };
    let n = match &kind {
        ParamKind::Double { dims, .. } | ParamKind::Int { dims } => *dims,
        ParamKind::Rgb => 3,
        ParamKind::Rgba => 4,
        _ => 1,
    };
    let get = |k: &str, fill: f64| -> Vec<f64> {
        let mut v = ps.get_dbls(k);
        v.resize(n, *v.last().unwrap_or(&fill));
        v
    };
    let (dmin, dmax) = (get("OfxParamPropDisplayMin", f64::NAN), get("OfxParamPropDisplayMax", f64::NAN));
    let (hmin, hmax) = (get("OfxParamPropMin", f64::NAN), get("OfxParamPropMax", f64::NAN));
    // The display range when it's sensible, else the hard range, else a guess.
    let pick = |d: f64, h: f64, fallback: f64| if d.is_finite() && d.abs() < 1e9 { d } else if h.is_finite() && h.abs() < 1e9 { h } else { fallback };
    let (min, max): (Vec<f64>, Vec<f64>) = (0..n)
        .map(|i| match &kind {
            ParamKind::Rgb | ParamKind::Rgba | ParamKind::Bool => (0.0, 1.0),
            ParamKind::Choice(o) | ParamKind::StrChoice { labels: o, .. } => (0.0, (o.len().max(1) - 1) as f64),
            _ => (pick(dmin[i], hmin[i], -1000.0), pick(dmax[i], hmax[i], 1000.0)),
        })
        .unzip();
    // A string menu's default is one of its values: its position.
    let default = match &kind {
        ParamKind::StrChoice { values, .. } => {
            let d = ps.get_str("OfxParamPropDefault", 0).unwrap_or_default();
            vec![values.iter().position(|v| *v == d).unwrap_or(0) as f64]
        }
        _ => get("OfxParamPropDefault", 0.0),
    };
    ParamInfo {
        name: p.name.clone(),
        label,
        hint: ps.get_str("OfxParamPropHint", 0).unwrap_or_default(),
        default,
        min,
        max,
        secret: ps.get_int("OfxParamPropSecret", 0) == Some(1),
        default_string: if kind == ParamKind::String { ps.get_str("OfxParamPropDefault", 0).unwrap_or_default() } else { String::new() },
        kind,
    }
}

/// A frame in and out of a plugin: RGBA as 32-bit floats, rows top to
/// bottom, premultiplied.
#[derive(Clone, Debug, PartialEq)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<f32>,
}

/// A working copy of a plugin, holding its settings and any state it keeps.
pub struct Instance {
    plugin: Arc<Plugin>,
    effect: Box<EffectObj>,
    created: bool,
    /// Pixels the clips point at while rendering, in the plugin's depth.
    source: Vec<u8>,
    output: Vec<u8>,
    /// Other frames of the source, likewise.
    others: Vec<(f64, Vec<u8>)>,
    /// Told it's attached to the OpenGL context (OpenGLContextAttached).
    gl_attached: bool,
    /// OpenGL failed for this instance: the processor from now on.
    gl_failed: bool,
}

// SAFETY: see `Plugin`; an instance is only used by its owner.
unsafe impl Send for Instance {}

/// What to render.
#[derive(Clone, Copy)]
pub struct Request<'a> {
    pub input: &'a Image,
    /// Frames, at `fps`.
    pub time: f64,
    pub fps: f64,
    /// Parameter values by position in the plugin's list (`None`: keep).
    pub values: &'a [Option<Value>],
    /// Other frames of the source (time in frames, picture), for plugins
    /// that read them ([`Plugin::temporal`]); see [`Instance::frames_needed`].
    pub others: &'a [(f64, &'a Image)],
    /// The source's first and last frame.
    pub range: (f64, f64),
}

impl Plugin {
    /// Make an instance (the plugin's CreateInstance).
    pub fn instance(self: &Arc<Self>) -> Result<Instance, String> {
        let mut props = (*self.descriptor.props).clone();
        props.s("OfxPropType", "OfxTypeImageEffectInstance").s("OfxImageEffectPropContext", self.context).i("OfxPropIsInteractive", 0).p("OfxPropInstanceData", std::ptr::null_mut());
        let mut effect = EffectObj::new(props);
        effect.instance = true;
        // Parameters: copies of the descriptions, at their defaults.
        for d in &self.descriptor.params.params {
            let mut p = ParamObj { name: d.name.clone(), kind: d.kind.clone(), props: Box::new((*d.props).clone()), value: Default::default() };
            p.props.s("OfxPropType", "OfxTypeParameterInstance");
            let defaults = p.props.get_dbls("OfxParamPropDefault");
            for (k, v) in defaults.iter().take(4).enumerate() {
                p.value.d[k] = *v;
                p.value.i[k] = v.round() as c_int;
            }
            if let Some(s) = p.props.get_str("OfxParamPropDefault", 0) {
                p.value.s = CString::new(s).unwrap_or_default();
            }
            effect.params.params.push(Box::new(p));
        }
        for d in &self.descriptor.clips {
            let mut props = (*d.props).clone();
            let connected = d.name == "Source" || d.name == "Output";
            props
                .i("OfxImageClipPropConnected", connected as c_int)
                .s("OfxImageEffectPropPixelDepth", self.depth)
                .s("OfxImageEffectPropComponents", self.components)
                .s("OfxImageClipPropUnmappedPixelDepth", self.depth)
                .s("OfxImageClipPropUnmappedComponents", self.components)
                .s("OfxImageEffectPropPreMultiplication", "OfxImageAlphaPremultiplied")
                .d("OfxImagePropPixelAspectRatio", 1.0)
                .s("OfxImageClipPropFieldOrder", "OfxFieldNone")
                .i("OfxImageClipPropContinuousSamples", 0)
                .d("OfxImageEffectPropFrameRate", 25.0)
                .dn("OfxImageEffectPropFrameRange", &[0.0, 1e6])
                .d("OfxImageEffectPropUnmappedFrameRate", 25.0)
                .dn("OfxImageEffectPropUnmappedFrameRange", &[0.0, 1e6]);
            effect.clips.push(Box::new(suites::ClipObj::new(d.name.clone(), Box::new(props))));
        }
        let mut inst = Instance { plugin: self.clone(), effect, created: false, source: Vec::new(), output: Vec::new(), others: Vec::new(), gl_attached: false, gl_failed: false };
        inst.set_project(1920, 1080, 25.0);
        // SAFETY: the plugin's code, with its own instance handle.
        let st = unsafe { self.call("OfxActionCreateInstance", inst.handle(), None, None) };
        if !ok(st) {
            return Err(format!("{}: couldn't make an instance ({st}){}", self.label, last_message()));
        }
        inst.created = true;
        Ok(inst)
    }
}

fn last_message() -> String {
    suites::LAST_MESSAGE.lock().unwrap().take().map(|m| format!(": {m}")).unwrap_or_default()
}

/// How a plugin's pixel depth stores a 0–1 value.
fn bytes_per_channel(depth: &str) -> usize {
    match depth {
        "OfxBitDepthFloat" => 4,
        "OfxBitDepthShort" => 2,
        _ => 1,
    }
}

impl Instance {
    fn handle(&mut self) -> *const c_void {
        &mut *self.effect as *mut EffectObj as *const c_void
    }

    pub fn plugin(&self) -> &Arc<Plugin> {
        &self.plugin
    }

    fn set_project(&mut self, w: u32, h: u32, fps: f64) {
        self.effect
            .props
            .dn("OfxImageEffectPropProjectSize", &[w as f64, h as f64])
            .dn("OfxImageEffectPropProjectExtent", &[w as f64, h as f64])
            .dn("OfxImageEffectPropProjectOffset", &[0.0, 0.0])
            .d("OfxImageEffectPropPixelAspectRatio", 1.0)
            .d("OfxImageEffectPropFrameRate", fps)
            .d("OfxImageEffectInstancePropEffectDuration", 1e6)
            .i("OfxImageEffectInstancePropSequentialRender", 0)
            .i("OfxImageEffectPropSupportsTiles", 0);
        for c in &mut self.effect.clips {
            c.props.d("OfxImageEffectPropFrameRate", fps).d("OfxImageEffectPropUnmappedFrameRate", fps);
        }
    }

    /// Set the parameters (by position in the plugin's list; normalised
    /// values are fractions of the frame, given to the plugin in pixels).
    fn set_values(&mut self, values: &[Option<Value>], size: (u32, u32)) {
        for (k, p) in self.effect.params.params.iter_mut().enumerate() {
            let Some(Some(v)) = values.get(k) else { continue };
            let info = &self.plugin.params[k];
            match v {
                Value::Numbers(n) if matches!(info.kind, ParamKind::StrChoice { .. }) => {
                    let ParamKind::StrChoice { values, .. } = &info.kind else { unreachable!() };
                    if let Some(v) = n.first().and_then(|i| values.get(i.round().max(0.0) as usize)) {
                        p.value.s = CString::new(v.replace('\0', "")).unwrap_or_default();
                    }
                }
                Value::Numbers(n) => {
                    for (j, x) in n.iter().take(4).enumerate() {
                        let x = match info.kind {
                            ParamKind::Double { normalised: true, .. } => x * if j == 0 { size.0 as f64 } else { size.1 as f64 },
                            _ => *x,
                        };
                        p.value.d[j] = x;
                        p.value.i[j] = x.round() as c_int;
                    }
                }
                Value::Text(s) => p.value.s = CString::new(s.replace('\0', "")).unwrap_or_default(),
            }
        }
    }

    /// Render `input` at `time` (frames, at `fps`) with these parameter
    /// values (`None` keeps the current value), and return the result.
    pub fn render(&mut self, input: &Image, time: f64, fps: f64, values: &[Option<Value>]) -> Result<Image, String> {
        self.render_with(&Request { input, time, fps, values, others: &[], range: (0.0, 1e6) })
    }

    fn set_range(&mut self, range: (f64, f64)) {
        let r = [range.0, range.1.max(range.0)];
        self.effect.props.dn("OfxImageEffectPropFrameRange", &r).d("OfxImageEffectInstancePropEffectDuration", r[1] - r[0] + 1.0);
        for c in &mut self.effect.clips {
            c.props.dn("OfxImageEffectPropFrameRange", &r).dn("OfxImageEffectPropUnmappedFrameRange", &r);
        }
    }

    /// Which other frames of the source the plugin will want to render at
    /// `time` with these settings (empty for plugins that don't read other
    /// frames). Fetch them and pass them in [`Request::others`].
    pub fn frames_needed(&mut self, time: f64, fps: f64, size: (u32, u32), values: &[Option<Value>], range: (f64, f64)) -> Vec<f64> {
        if !self.plugin.temporal {
            return Vec::new();
        }
        self.set_project(size.0, size.1, fps);
        self.set_values(values, size);
        self.set_range(range);
        let mut args = PropSet::default();
        args.d("OfxPropTime", time);
        let mut out = PropSet::default();
        out.dn("OfxImageClipPropFrameRange_Source", &[time, time]);
        let handle = self.handle();
        // SAFETY: the plugin's code, on its own instance.
        let st = unsafe { self.plugin.call("OfxImageEffectActionGetFramesNeeded", handle, Some(&mut args), Some(&mut out)) };
        // Not answered: the frames either side (what most such plugins read).
        let pairs = if st == props::OK { out.get_dbls("OfxImageClipPropFrameRange_Source") } else { vec![time - 1.0, time + 1.0] };
        let mut frames: Vec<f64> = Vec::new();
        for r in pairs.chunks_exact(2) {
            let (a, b) = (r[0].min(r[1]).max(range.0), r[0].max(r[1]).min(range.1));
            // At most 16 frames either side.
            let (a, b) = (a.max(time - 16.0).round(), b.min(time + 16.0).round());
            let mut t = a;
            while t <= b {
                if (t - time).abs() >= 0.5 && !frames.iter().any(|f| (f - t).abs() < 0.5) {
                    frames.push(t);
                }
                t += 1.0;
            }
        }
        frames
    }

    /// A picture in the plugin's depth and components, rows bottom to top.
    fn to_buffer(&self, img: &Image, buf: &mut Vec<u8>) {
        let (w, h) = (img.width as usize, img.height as usize);
        let comps = if self.plugin.components == "OfxImageComponentRGB" { 3 } else { 4 };
        let bpc = bytes_per_channel(self.plugin.depth);
        let row = w * comps * bpc;
        buf.clear();
        buf.resize(row * h, 0);
        for y in 0..h {
            let src = &img.rgba[(h - 1 - y) * w * 4..][..w * 4];
            let dst = &mut buf[y * row..(y + 1) * row];
            for x in 0..w {
                for ch in 0..comps {
                    let v = src[x * 4 + ch];
                    let at = (x * comps + ch) * bpc;
                    match bpc {
                        4 => dst[at..at + 4].copy_from_slice(&v.to_ne_bytes()),
                        2 => dst[at..at + 2].copy_from_slice(&((v.clamp(0.0, 1.0) * 65535.0).round() as u16).to_ne_bytes()),
                        _ => dst[at] = (v.clamp(0.0, 1.0) * 255.0).round() as u8,
                    }
                }
            }
        }
    }

    /// Render one frame (with other frames of the source, for plugins that
    /// read them). Plugins that can render with OpenGL do, when it's there;
    /// if that fails, they render on the processor if they can.
    pub fn render_with(&mut self, req: &Request) -> Result<Image, String> {
        let input = req.input;
        let (w, h) = (input.width, input.height);
        if w == 0 || h == 0 || input.rgba.len() != w as usize * h as usize * 4 {
            return Err("empty or mismatched picture".into());
        }
        if req.others.iter().any(|(_, o)| o.width != w || o.height != h || o.rgba.len() != input.rgba.len()) {
            return Err("other frames of a different size".into());
        }
        self.set_project(w, h, req.fps);
        self.set_values(req.values, (w, h));
        self.set_range(req.range);
        if let Some(only_gl) = self.plugin.opengl {
            let gl = !self.gl_failed && suites::gl_offered();
            if gl {
                match self.render_gl(req) {
                    Ok(img) => return Ok(img),
                    Err(e) if !only_gl => {
                        log::warn!("OpenFX {}: OpenGL render failed ({e}); using the processor", self.plugin.label);
                        self.gl_failed = true;
                    }
                    Err(e) => return Err(e),
                }
            } else if only_gl {
                let why = if !GPU.load(Ordering::Relaxed) { "switched off in Edit > Plugins > OpenFX".to_string() } else { gl::available().err().unwrap_or_else(|| "it failed for this plugin".into()) };
                return Err(format!("{} only renders on the graphics card (OpenGL), which isn't available: {why}", self.plugin.label));
            }
        }
        self.render_cpu(req)
    }

    fn render_cpu(&mut self, req: &Request) -> Result<Image, String> {
        let (w, h) = (req.input.width, req.input.height);
        let time = req.time;
        let depth = self.plugin.depth;
        let comps = if self.plugin.components == "OfxImageComponentRGB" { 3 } else { 4 };
        let bpc = bytes_per_channel(depth);
        let row = w as usize * comps * bpc;
        let mut source = std::mem::take(&mut self.source);
        self.to_buffer(req.input, &mut source);
        self.source = source;
        let mut others = std::mem::take(&mut self.others);
        others.resize_with(req.others.len(), || (0.0, Vec::new()));
        for ((t, buf), (ot, img)) in others.iter_mut().zip(req.others) {
            *t = *ot;
            self.to_buffer(img, buf);
        }
        self.others = others;
        self.output.clear();
        self.output.resize(row * h as usize, 0);
        let px = |buf: &mut Vec<u8>| suites::Pixels {
            data: buf.as_mut_ptr() as *mut c_void,
            width: w,
            height: h,
            row_bytes: row as i32,
            depth,
            components: if comps == 3 { "OfxImageComponentRGB" } else { "OfxImageComponentRGBA" },
        };
        let (sp, op) = (px(&mut self.source), px(&mut self.output));
        let other_px: Vec<(f64, suites::Pixels)> = self.others.iter_mut().map(|(t, b)| (*t, px(b))).collect();
        for c in &mut self.effect.clips {
            c.time = time;
            c.others.clear();
            c.pixels = match c.name.as_str() {
                "Source" => {
                    c.others = other_px.clone();
                    Some(sp)
                }
                "Output" => Some(op),
                _ => None,
            };
            c.props.dn("OfxImagePropRegionOfDefinition", &[0.0, 0.0, w as f64, h as f64]);
        }
        self.effect.props.d("OfxPropTime", time);
        let st = self.run_actions(time, w, h, |_| {});
        for c in &mut self.effect.clips {
            c.pixels = None;
            c.others.clear();
        }
        if st != props::OK {
            return Err(format!("{} failed to render ({st}){}", self.plugin.label, last_message()));
        }
        let mut out = Image { width: w, height: h, rgba: vec![0.0; w as usize * h as usize * 4] };
        for y in 0..h as usize {
            let src = &self.output[y * row..(y + 1) * row];
            let dst = &mut out.rgba[(h as usize - 1 - y) * w as usize * 4..][..w as usize * 4];
            for x in 0..w as usize {
                for ch in 0..4 {
                    let v = if ch >= comps {
                        1.0
                    } else {
                        let at = (x * comps + ch) * bpc;
                        match bpc {
                            4 => f32::from_ne_bytes(src[at..at + 4].try_into().unwrap()),
                            2 => u16::from_ne_bytes([src[at], src[at + 1]]) as f32 / 65535.0,
                            _ => src[at] as f32 / 255.0,
                        }
                    };
                    dst[x * 4 + ch] = v;
                }
            }
        }
        Ok(out)
    }

    /// BeginSequenceRender, Render, EndSequenceRender; `extra` adds to the
    /// arguments of each (OpenGL's). The Render action's status.
    fn run_actions(&mut self, time: f64, w: u32, h: u32, extra: impl Fn(&mut PropSet)) -> c_int {
        let handle = self.handle();
        // SAFETY: the plugin's code, on its own instance, whose clips point at
        // buffers that outlive these calls.
        unsafe {
            let mut seq = PropSet::default();
            seq.dn("OfxImageEffectPropFrameRange", &[time, time])
                .d("OfxImageEffectPropFrameStep", 1.0)
                .i("OfxPropIsInteractive", 0)
                .dn("OfxImageEffectPropRenderScale", &[1.0, 1.0])
                .i("OfxImageEffectPropSequentialRenderStatus", 0)
                .i("OfxImageEffectPropInteractiveRenderStatus", 0);
            extra(&mut seq);
            let begin = self.plugin.call("OfxImageEffectActionBeginSequenceRender", handle, Some(&mut seq.clone()), None);
            let mut args = PropSet::default();
            args.d("OfxPropTime", time)
                .s("OfxImageEffectPropFieldToRender", "OfxFieldNone")
                .in_("OfxImageEffectPropRenderWindow", &[0, 0, w as c_int, h as c_int])
                .dn("OfxImageEffectPropRenderScale", &[1.0, 1.0])
                .i("OfxImageEffectPropSequentialRenderStatus", 0)
                .i("OfxImageEffectPropInteractiveRenderStatus", 0)
                .i("OfxImageEffectPropRenderQualityDraft", 0);
            extra(&mut args);
            let st = self.plugin.call("OfxImageEffectActionRender", handle, Some(&mut args), None);
            if ok(begin) {
                self.plugin.call("OfxImageEffectActionEndSequenceRender", handle, Some(&mut seq), None);
            }
            st
        }
    }

    /// Render with OpenGL: the source frames as textures, the output a
    /// texture bound as the framebuffer (pixel coordinates, y up), read back.
    fn render_gl(&mut self, req: &Request) -> Result<Image, String> {
        let (w, h) = (req.input.width, req.input.height);
        let time = req.time;
        // Float RGBA, rows bottom to top, as textures want them.
        let flip = |img: &Image| -> Vec<f32> {
            let row = img.width as usize * 4;
            let mut v = Vec::with_capacity(img.rgba.len());
            for y in (0..img.height as usize).rev() {
                v.extend_from_slice(&img.rgba[y * row..(y + 1) * row]);
            }
            v
        };
        let frames: Vec<(f64, Vec<f32>)> = std::iter::once((time, flip(req.input))).chain(req.others.iter().map(|(t, i)| (*t, flip(i)))).collect();
        let depth = self.plugin.gl_depth;
        let r = gl::with(|g| {
            let f = &g.fns;
            // SAFETY: OpenGL calls on our current context; the plugin's code on its own instance.
            unsafe {
                if !self.gl_attached {
                    let handle = self.handle();
                    let st = self.plugin.call("OfxActionOpenGLContextAttached", handle, None, None);
                    if !ok(st) {
                        return Err(format!("it wouldn't attach to the OpenGL context ({st})"));
                    }
                    self.gl_attached = true;
                }
                while (f.get_error)() != 0 {}
                let mut tex = 0;
                (f.gen_textures)(1, &mut tex);
                (f.bind_texture)(gl::TEXTURE_2D, tex);
                (f.tex_image_2d)(gl::TEXTURE_2D, 0, suites::gl_format(depth) as c_int, w as c_int, h as c_int, 0, gl::RGBA, gl::FLOAT, std::ptr::null());
                (f.tex_parameteri)(gl::TEXTURE_2D, gl::TEXTURE_MIN_FILTER, gl::LINEAR);
                (f.tex_parameteri)(gl::TEXTURE_2D, gl::TEXTURE_MAG_FILTER, gl::LINEAR);
                (f.bind_texture)(gl::TEXTURE_2D, 0);
                let mut fbo = 0;
                (f.gen_framebuffers)(1, &mut fbo);
                (f.bind_framebuffer)(gl::FRAMEBUFFER, fbo);
                (f.framebuffer_texture_2d)(gl::FRAMEBUFFER, gl::COLOR_ATTACHMENT0, gl::TEXTURE_2D, tex, 0);
                let cleanup = |f: &gl::Fns| {
                    (f.bind_framebuffer)(gl::FRAMEBUFFER, 0);
                    (f.delete_framebuffers)(1, &fbo);
                    (f.delete_textures)(1, &tex);
                };
                if (f.check_framebuffer_status)(gl::FRAMEBUFFER) != gl::FRAMEBUFFER_COMPLETE {
                    cleanup(f);
                    return Err(format!("couldn't draw into a {w}×{h} {depth} texture"));
                }
                (f.viewport)(0, 0, w as c_int, h as c_int);
                (f.matrix_mode)(gl::PROJECTION);
                (f.load_identity)();
                (f.ortho)(0.0, w as f64, 0.0, h as f64, -1.0, 1.0);
                (f.matrix_mode)(gl::MODELVIEW);
                (f.load_identity)();
                (f.clear_color)(0.0, 0.0, 0.0, 0.0);
                (f.clear)(gl::COLOR_BUFFER_BIT);
                let src = suites::GlClip { frames: frames.iter().map(|(t, v)| (*t, v.as_ptr(), w, h)).collect(), output: None, depth };
                for c in &mut self.effect.clips {
                    c.time = time;
                    c.pixels = None;
                    c.others.clear();
                    c.gl = match c.name.as_str() {
                        "Source" => Some(src.clone()),
                        "Output" => Some(suites::GlClip { frames: Vec::new(), output: Some((tex, w, h)), depth }),
                        _ => None,
                    };
                    c.props.dn("OfxImagePropRegionOfDefinition", &[0.0, 0.0, w as f64, h as f64]);
                }
                self.effect.props.d("OfxPropTime", time);
                let st = self.run_actions(time, w, h, |a| {
                    a.i("OfxImageEffectPropOpenGLEnabled", 1).i("OfxImageEffectPropOpenGLTextureIndex", tex as c_int).i("OfxImageEffectPropOpenGLTextureTarget", gl::TEXTURE_2D as c_int);
                });
                for c in &mut self.effect.clips {
                    c.gl = None;
                }
                // Errors from the plugin's own drawing are its business (and
                // harmless ones are common); only the readback's count.
                let mut n = 0;
                while (f.get_error)() != 0 && n < 64 {
                    n += 1;
                }
                // The plugin may have bound something else.
                (f.bind_framebuffer)(gl::FRAMEBUFFER, fbo);
                (f.finish)();
                let mut px = vec![0f32; w as usize * h as usize * 4];
                (f.pixel_storei)(gl::PACK_ALIGNMENT, 4);
                (f.read_pixels)(0, 0, w as c_int, h as c_int, gl::RGBA, gl::FLOAT, px.as_mut_ptr() as *mut c_void);
                let err = (f.get_error)();
                cleanup(f);
                if st != props::OK {
                    return Err(format!("{} failed to render with OpenGL ({st}){}", self.plugin.label, last_message()));
                }
                if err != 0 {
                    return Err(format!("OpenGL error {err:#x}"));
                }
                let row = w as usize * 4;
                let mut out = Image { width: w, height: h, rgba: Vec::with_capacity(px.len()) };
                for y in (0..h as usize).rev() {
                    out.rgba.extend_from_slice(&px[y * row..(y + 1) * row]);
                }
                Ok(out)
            }
        });
        match r {
            Ok(r) => r,
            Err(e) => {
                self.gl_failed = true;
                Err(e)
            }
        }
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        if self.gl_attached {
            let h = self.handle();
            let plugin = self.plugin.clone();
            // SAFETY: the plugin's code, with the context it was attached to current.
            let _ = gl::with(|_| unsafe { plugin.call("OfxActionOpenGLContextDetached", h, None, None) });
        }
        if self.created {
            let h = self.handle();
            // SAFETY: the plugin's code, letting go of its own instance.
            unsafe { self.plugin.call("OfxActionDestroyInstance", h, None, None) };
        }
    }
}
