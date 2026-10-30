//! An OpenFX host: loads image effect plugins (`.ofx` bundles, the plugin
//! format of Resolve, Nuke, Vegas, HitFilm and others) and renders frames
//! with them on the CPU.
//!
//! What a plugin gets: the Filter context (or General, with only its Source
//! clip connected); one frame at a time, the whole frame, at full scale, in
//! 32-bit float RGBA (or 16-bit, 8-bit or RGB when that's all it takes),
//! premultiplied; parameters set by the host before each render (Reelforge
//! keyframes them); the property, image effect, parameter, memory,
//! multithread, message, progress and timeline suites. Not offered: overlays
//! and custom interacts, OpenGL/GPU rendering, parametric parameters, other
//! frames (temporal access) and the Generator/Transition/Retimer contexts.
//! A plugin that needs those may refuse to load, or may still work.
//!
//! The headers this follows are the OpenFX standard's (BSD-3-Clause), from
//! the Academy Software Foundation; no OpenFX code is copied in here.

mod props;
mod suites;

use props::PropSet;
use std::ffi::{c_char, c_int, c_uint, c_void, CStr, CString};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use suites::{EffectObj, OfxHost, ParamObj};

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
    Double { dims: usize, normalised: bool },
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
    /// The bit depth frames are given in.
    depth: &'static str,
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
    Ok(Plugin {
        library: library.clone(),
        version: (p.version_major, p.version_minor),
        group: desc.props.get_str("OfxImageEffectPluginPropGrouping", 0).unwrap_or_default(),
        description: desc.props.get_str("OfxPropPluginDescription", 0).unwrap_or_default(),
        identifier,
        label,
        context,
        params,
        depth,
        components,
        entry,
        descriptor: desc,
    })
}

fn param_info(p: &ParamObj) -> ParamInfo {
    let ps = &p.props;
    let label = ps.get_str("OfxPropLabel", 0).filter(|s| !s.is_empty()).unwrap_or_else(|| p.name.clone());
    let normalised = ps.get_str("OfxParamPropDoubleType", 0).is_some_and(|t| t.contains("Normalised")) || ps.get_str("OfxParamPropDefaultCoordinateSystem", 0).is_some_and(|t| t == "OfxParamCoordinatesNormalised");
    let kind = match p.kind.as_str() {
        "OfxParamTypeDouble" => ParamKind::Double { dims: 1, normalised },
        "OfxParamTypeDouble2D" => ParamKind::Double { dims: 2, normalised },
        "OfxParamTypeDouble3D" => ParamKind::Double { dims: 3, normalised: false },
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
}

// SAFETY: see `Plugin`; an instance is only used by its owner.
unsafe impl Send for Instance {}

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
            effect.clips.push(Box::new(suites::ClipObj { name: d.name.clone(), props: Box::new(props), pixels: None }));
        }
        let mut inst = Instance { plugin: self.clone(), effect, created: false, source: Vec::new(), output: Vec::new() };
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
        let (w, h) = (input.width, input.height);
        if w == 0 || h == 0 || input.rgba.len() != w as usize * h as usize * 4 {
            return Err("empty or mismatched picture".into());
        }
        self.set_project(w, h, fps);
        self.set_values(values, (w, h));
        let depth = self.plugin.depth;
        let comps = if self.plugin.components == "OfxImageComponentRGB" { 3 } else { 4 };
        let bpc = bytes_per_channel(depth);
        let row = w as usize * comps * bpc;
        // Rows bottom to top, as OpenFX counts y upwards.
        self.source.clear();
        self.source.resize(row * h as usize, 0);
        for y in 0..h as usize {
            let src = &input.rgba[(h as usize - 1 - y) * w as usize * 4..][..w as usize * 4];
            let dst = &mut self.source[y * row..(y + 1) * row];
            for x in 0..w as usize {
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
        for c in &mut self.effect.clips {
            c.pixels = match c.name.as_str() {
                "Source" => Some(sp),
                "Output" => Some(op),
                _ => None,
            };
            c.props.dn("OfxImagePropRegionOfDefinition", &[0.0, 0.0, w as f64, h as f64]);
        }
        self.effect.props.d("OfxPropTime", time).dn("OfxImageEffectPropFrameRange", &[time, time]);
        let handle = self.handle();
        // SAFETY: the plugin's code, on its own instance, whose clips point at
        // buffers that outlive these calls.
        let st = unsafe {
            let mut seq = PropSet::default();
            seq.dn("OfxImageEffectPropFrameRange", &[time, time])
                .d("OfxImageEffectPropFrameStep", 1.0)
                .i("OfxPropIsInteractive", 0)
                .dn("OfxImageEffectPropRenderScale", &[1.0, 1.0])
                .i("OfxImageEffectPropSequentialRenderStatus", 0)
                .i("OfxImageEffectPropInteractiveRenderStatus", 0);
            let begin = self.plugin.call("OfxImageEffectActionBeginSequenceRender", handle, Some(&mut seq.clone()), None);
            let mut args = PropSet::default();
            args.d("OfxPropTime", time)
                .s("OfxImageEffectPropFieldToRender", "OfxFieldNone")
                .in_("OfxImageEffectPropRenderWindow", &[0, 0, w as c_int, h as c_int])
                .dn("OfxImageEffectPropRenderScale", &[1.0, 1.0])
                .i("OfxImageEffectPropSequentialRenderStatus", 0)
                .i("OfxImageEffectPropInteractiveRenderStatus", 0)
                .i("OfxImageEffectPropRenderQualityDraft", 0);
            let st = self.plugin.call("OfxImageEffectActionRender", handle, Some(&mut args), None);
            if ok(begin) {
                self.plugin.call("OfxImageEffectActionEndSequenceRender", handle, Some(&mut seq), None);
            }
            st
        };
        for c in &mut self.effect.clips {
            c.pixels = None;
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
}

impl Drop for Instance {
    fn drop(&mut self) {
        if self.created {
            let h = self.handle();
            // SAFETY: the plugin's code, letting go of its own instance.
            unsafe { self.plugin.call("OfxActionDestroyInstance", h, None, None) };
        }
    }
}
