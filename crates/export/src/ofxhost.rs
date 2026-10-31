//! OpenFX plugins in the render path: finding and loading them (into
//! [`reelforge_core::ofx`]'s registry), and running a clip's OpenFX effects
//! on its picture before the GPU effects.
//!
//! Plugins see the picture as RGBA floats, premultiplied, in the sRGB-encoded
//! display space they expect from an editing host (values above 1 are kept
//! for HDR and linear sources). What comes back becomes a half-float layer, so
//! nothing is lost on the way to the compositor.

use reelforge_core::frame::{f16_to_f32, f32_to_f16, FloatPixels, Pixels};
use reelforge_core::ofx::{self as core_ofx, OfxDef, OfxStep, ParamDesc};
use reelforge_core::ClipId;
use reelforge_media::VideoFrame;
use reelforge_ofx::{Image, Instance, ParamKind, Plugin, Value};
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

static PLUGINS: RwLock<Vec<Arc<Plugin>>> = RwLock::new(Vec::new());
static REVISION: AtomicU64 = AtomicU64::new(1);

/// The loaded plugin behind an installed [`OfxDef`].
pub fn plugin(id: &str) -> Option<Arc<Plugin>> {
    PLUGINS.read().unwrap().iter().find(|p| p.identifier == id).cloned()
}

/// How loading one bundle went.
#[derive(Clone, Debug, PartialEq)]
pub struct BundleReport {
    pub path: PathBuf,
    /// The effects it added (their names).
    pub plugins: Vec<String>,
    /// Why it (or some plugins in it) didn't load.
    pub errors: Vec<String>,
    /// Not loaded: it crashed Reelforge last time.
    pub blocked: bool,
}

/// The marker files that make scanning crash-safe live in `state`: while a
/// bundle loads its path is in `ofx-loading.txt`; if Reelforge dies there,
/// the next start finds it and lists the bundle in `ofx-blocked.txt` instead
/// of loading it again.
const LOADING: &str = "ofx-loading.txt";
const BLOCKED: &str = "ofx-blocked.txt";

fn read_list(p: &Path) -> Vec<PathBuf> {
    std::fs::read_to_string(p).map(|s| s.lines().filter(|l| !l.trim().is_empty()).map(PathBuf::from).collect()).unwrap_or_default()
}

fn write_list(p: &Path, list: &[PathBuf]) {
    let text: String = list.iter().map(|p| format!("{}\n", p.display())).collect();
    let _ = std::fs::write(p, text);
}

/// Bundles skipped because they crashed Reelforge while loading.
pub fn blocked(state: &Path) -> Vec<PathBuf> {
    read_list(&state.join(BLOCKED))
}

/// Let a blocked bundle be tried again at the next scan.
pub fn unblock(state: &Path, bundle: &Path) {
    let mut list = blocked(state);
    list.retain(|p| p != bundle);
    write_list(&state.join(BLOCKED), &list);
}

/// Find and load the plugins in `folders` (and below), replacing the
/// installed set. `state` holds the crash markers (no crash protection without it).
pub fn scan(folders: &[PathBuf], state: Option<&Path>) -> Vec<BundleReport> {
    let mut blocked_list = state.map(blocked).unwrap_or_default();
    if let Some(st) = state {
        // A load that never finished: Reelforge crashed in it.
        for p in read_list(&st.join(LOADING)) {
            if !blocked_list.contains(&p) {
                log::warn!("OpenFX: {} crashed Reelforge while loading; skipping it from now on", p.display());
                blocked_list.push(p);
            }
        }
        write_list(&st.join(BLOCKED), &blocked_list);
        let _ = std::fs::remove_file(st.join(LOADING));
    }
    let mut reports = Vec::new();
    let mut plugins: Vec<Arc<Plugin>> = Vec::new();
    let mut defs = Vec::new();
    let mut seen = HashSet::new();
    for bundle in reelforge_ofx::find_bundles(folders) {
        let mut r = BundleReport { path: bundle.clone(), plugins: Vec::new(), errors: Vec::new(), blocked: false };
        if blocked_list.contains(&bundle) {
            r.blocked = true;
            reports.push(r);
            continue;
        }
        if let Some(st) = state {
            let _ = std::fs::write(st.join(LOADING), format!("{}\n", bundle.display()));
        }
        // SAFETY: plugins are native code the user installed; a crash in one
        // is caught next time by the marker above.
        match unsafe { reelforge_ofx::load(&bundle) } {
            Ok((list, errors)) => {
                r.errors = errors;
                for p in list {
                    // The first of two plugins with one identifier wins.
                    if !seen.insert(p.identifier.clone()) {
                        r.errors.push(format!("{}: another plugin has the identifier {}", p.label, p.identifier));
                        continue;
                    }
                    let p = Arc::new(p);
                    r.plugins.push(p.label.clone());
                    defs.push(Arc::new(describe(&p, &bundle)));
                    plugins.push(p);
                }
            }
            Err(e) => r.errors.push(e),
        }
        if let Some(st) = state {
            let _ = std::fs::remove_file(st.join(LOADING));
        }
        reports.push(r);
    }
    *PLUGINS.write().unwrap() = plugins;
    core_ofx::set_installed(defs);
    reports
}

/// How Reelforge shows a loaded plugin.
pub fn describe(p: &Plugin, bundle: &Path) -> OfxDef {
    let descs: Vec<ParamDesc> = p
        .params
        .iter()
        .map(|q| {
            let label = if q.label.is_empty() { q.name.clone() } else { q.label.clone() };
            let fill = |v: &[f64], n: usize, d: f64| (0..n).map(|k| v.get(k).copied().unwrap_or(d)).collect::<Vec<_>>();
            if q.secret {
                return ParamDesc::Hidden;
            }
            match &q.kind {
                ParamKind::Double { dims, normalised, point } => ParamDesc::Double { label, dims: *dims, normalised: *normalised, point: *point, default: fill(&q.default, *dims, 0.0), min: fill(&q.min, *dims, 0.0), max: fill(&q.max, *dims, 1.0) },
                ParamKind::Int { dims } => ParamDesc::Int { label, dims: *dims, default: fill(&q.default, *dims, 0.0), min: fill(&q.min, *dims, 0.0), max: fill(&q.max, *dims, 100.0) },
                ParamKind::Bool => ParamDesc::Bool { label, default: q.default.first().is_some_and(|&v| v != 0.0) },
                ParamKind::Choice(options) | ParamKind::StrChoice { labels: options, .. } => ParamDesc::Choice { label, options: options.clone(), default: q.default.first().map_or(0, |&v| v.max(0.0) as usize) },
                ParamKind::Rgb => ParamDesc::Color { label, alpha: false, default: q.default.clone() },
                ParamKind::Rgba => ParamDesc::Color { label, alpha: true, default: q.default.clone() },
                ParamKind::String | ParamKind::Other(_) => ParamDesc::Hidden,
            }
        })
        .collect();
    let (params, slots) = core_ofx::map_params(&descs);
    OfxDef {
        id: p.identifier.clone(),
        name: p.label.clone(),
        group: p.group.clone(),
        description: p.description.clone(),
        version: format!("{}.{}", p.version.0, p.version.1),
        bundle: bundle.to_path_buf(),
        params,
        slots,
        plugin_params: p.params.len(),
        revision: REVISION.fetch_add(1, Ordering::Relaxed),
        temporal: p.temporal,
        opengl: p.opengl,
    }
}

fn srgb_encode(v: f32) -> f32 {
    // Extended: the curve is mirrored below 0 and carries on above 1.
    let a = v.abs();
    let e = if a <= 0.003_130_8 { a * 12.92 } else { 1.055 * a.powf(1.0 / 2.4) - 0.055 };
    e.copysign(v)
}

fn srgb_decode(v: f32) -> f32 {
    let a = v.abs();
    let d = if a <= 0.040_45 { a / 12.92 } else { ((a + 0.055) / 1.055).powf(2.4) };
    d.copysign(v)
}

fn finite(v: f32) -> f32 {
    if v.is_finite() { v } else { 0.0 }
}

/// A frame as a plugin wants it: top-down RGBA floats, sRGB-encoded, premultiplied.
pub fn to_image(frame: &VideoFrame) -> Image {
    let (w, h) = (frame.width, frame.height);
    let mut rgba = Vec::with_capacity(w as usize * h as usize * 4);
    // Deep video goes over in floating point too, not through 8 bits.
    let deep = match &frame.pixels {
        Pixels::Yuv(y) if y.layout.deep() => Some(Arc::new(y.to_float())),
        Pixels::Gpu(g) if g.layout.deep() => g.cpu().map(|y| Arc::new(y.to_float())),
        _ => None,
    };
    let pixels = deep.map(Pixels::Float);
    match pixels.as_ref().unwrap_or(&frame.pixels) {
        // Linear, premultiplied: encode the colour, not the alpha.
        Pixels::Float(f) => {
            for p in f.rgba.chunks_exact(4) {
                let a = f16_to_f32(p[3]);
                for &c in &p[..3] {
                    let c = f16_to_f32(c);
                    rgba.push(if a > 1e-6 { srgb_encode(c / a) * a } else { srgb_encode(c) });
                }
                rgba.push(a);
            }
        }
        // Everything else as 8-bit sRGB, straight alpha.
        px => {
            for p in px.to_rgba().chunks_exact(4) {
                let a = p[3] as f32 / 255.0;
                for &c in &p[..3] {
                    rgba.push(c as f32 / 255.0 * a);
                }
                rgba.push(a);
            }
        }
    }
    Image { width: w, height: h, rgba }
}

/// What a plugin returned, as a half-float frame (linear, premultiplied).
pub fn from_image(img: &Image, pts: reelforge_core::Time, generation: u64) -> VideoFrame {
    let mut out = Vec::with_capacity(img.rgba.len());
    for p in img.rgba.chunks_exact(4) {
        let a = finite(p[3]);
        for &c in &p[..3] {
            let c = finite(c);
            let lin = if a.abs() > 1e-6 { srgb_decode(c / a) * a } else { srgb_decode(c) };
            out.push(f32_to_f16(lin));
        }
        out.push(f32_to_f16(a));
    }
    let pixels = Pixels::Float(Arc::new(FloatPixels { width: img.width, height: img.height, rgba: out }));
    VideoFrame { pts, width: img.width, height: img.height, pixels, generation }
}

/// Why OpenFX effects didn't render, per (clip, position in its effect
/// stack), for the effect panel to show. Cleared when they render again.
static ERRORS: std::sync::Mutex<Vec<((ClipId, usize), String)>> = std::sync::Mutex::new(Vec::new());

/// Why effect `index` of `clip` (an OpenFX one) isn't rendering, if it isn't.
pub fn error(clip: ClipId, index: usize) -> Option<String> {
    ERRORS.lock().unwrap().iter().find(|e| e.0 == (clip, index)).map(|e| e.1.clone())
}

fn set_error(clip: ClipId, index: usize, e: Option<String>) {
    let mut v = ERRORS.lock().unwrap();
    v.retain(|x| x.0 != (clip, index));
    if let Some(e) = e {
        v.push(((clip, index), e));
    }
}

/// Runs clips' OpenFX effects, keeping a plugin instance per effect (plugins
/// may keep state between frames) and each clip's last result.
#[derive(Default)]
pub struct OfxRunner {
    instances: HashMap<(ClipId, usize), (String, u64, Instance, u64)>,
    last: HashMap<ClipId, (u64, VideoFrame, u64)>,
    /// Effects that failed, so the error is logged once.
    failed: HashSet<(ClipId, usize, u64)>,
    /// Each clip's recent source frames (frame number, what it shows,
    /// picture): rendering frame after frame, the one before is already
    /// here, so plugins that read it don't make the decoder seek back.
    recent: HashMap<ClipId, VecDeque<(i64, u64, VideoFrame)>>,
    tick: u64,
}

/// Source frames kept per clip for plugins that read other frames.
const RECENT: usize = 40;

impl OfxRunner {
    /// Clip `clip`'s OpenFX `steps` applied to `frame`, at `fps`. The same
    /// frame back when there's nothing to run (or every step failed).
    /// `range`: the source's first and last frame; `fetch` gets the source
    /// at another frame (for plugins that read other frames).
    #[allow(clippy::too_many_arguments)]
    pub fn apply(&mut self, clip: ClipId, steps: &[OfxStep], frame: &VideoFrame, media_key: u64, fps: f64, range: (f64, f64), fetch: &mut dyn FnMut(f64) -> Option<VideoFrame>) -> VideoFrame {
        self.tick += 1;
        if steps.is_empty() {
            return frame.clone();
        }
        let size = [frame.width as f32, frame.height as f32];
        let fps = if fps > 0.0 { fps } else { 25.0 };
        // An instance per step, and its values for this picture.
        let mut plan: Vec<(usize, Vec<Option<Value>>, f64)> = Vec::new();
        for (k, s) in steps.iter().enumerate() {
            let Some(p) = plugin(&s.def.id) else { continue };
            let slot = (clip, s.index);
            let fresh = !matches!(self.instances.get(&slot), Some((id, rev, ..)) if *id == s.def.id && *rev == s.def.revision);
            if fresh {
                match p.instance() {
                    Ok(i) => {
                        self.instances.insert(slot, (s.def.id.clone(), s.def.revision, i, self.tick));
                    }
                    Err(e) => {
                        if self.failed.insert((clip, s.index, s.def.revision)) {
                            log::warn!("OpenFX {}: {e}", s.def.name);
                        }
                        set_error(clip, s.index, Some(e));
                        continue;
                    }
                }
            }
            let values: Vec<Option<Value>> = s.def.plugin_values(&s.values, size).into_iter().map(|v| v.map(Value::Numbers)).collect();
            // OpenFX counts time in frames.
            plan.push((k, values, s.time * fps));
        }
        // Other frames the plugins want, fetched once for all of them.
        let mut wanted: Vec<f64> = Vec::new();
        for (k, values, t) in &plan {
            let s = &steps[*k];
            if !s.def.temporal {
                continue;
            }
            let inst = &mut self.instances.get_mut(&(clip, s.index)).unwrap().2;
            for f in inst.frames_needed(*t, fps, (frame.width, frame.height), values, range) {
                if !wanted.iter().any(|w| (w - f).abs() < 0.5) {
                    wanted.push(f);
                }
            }
        }
        let mut others: Vec<(f64, VideoFrame)> = Vec::new();
        if !wanted.is_empty() {
            let recent = self.recent.entry(clip).or_default();
            let now = plan.first().map_or(0, |p| p.2.round() as i64);
            let keep = |n: i64, f: &VideoFrame, recent: &mut VecDeque<(i64, u64, VideoFrame)>| {
                recent.retain(|r| r.0 != n);
                recent.push_back((n, media_key, f.clone()));
                while recent.len() > RECENT {
                    recent.pop_front();
                }
            };
            keep(now, frame, recent);
            for &t in &wanted {
                let n = t.round() as i64;
                let got = match recent.iter().find(|r| r.0 == n && r.1 == media_key) {
                    Some(r) => Some(r.2.clone()),
                    None => fetch(t).inspect(|f| keep(n, f, recent)),
                };
                if let Some(f) = got.filter(|f| f.width == frame.width && f.height == frame.height) {
                    others.push((t, f));
                }
            }
        }
        let mut h = DefaultHasher::new();
        (media_key, frame.pts, frame.generation, frame.width, frame.height).hash(&mut h);
        // Switching the graphics card on or off can change what OpenGL-only plugins give.
        reelforge_ofx::GPU.load(std::sync::atomic::Ordering::Relaxed).hash(&mut h);
        for s in steps {
            s.hash_into(&mut h);
        }
        for (t, f) in &others {
            (t.to_bits(), f.pts, f.generation).hash(&mut h);
        }
        let key = h.finish();
        if let Some(e) = self.last.get_mut(&clip).filter(|e| e.0 == key) {
            e.2 = self.tick;
            return e.1.clone();
        }
        let other_images: Vec<(f64, Image)> = others.iter().map(|(t, f)| (*t, to_image(f))).collect();
        let mut img = to_image(frame);
        let mut ran = false;
        for (k, values, t) in &plan {
            let s = &steps[*k];
            let entry = self.instances.get_mut(&(clip, s.index)).unwrap();
            entry.3 = self.tick;
            // Other frames are the clip's own (before effects above this one).
            let refs: Vec<(f64, &Image)> = if s.def.temporal { other_images.iter().map(|(t, i)| (*t, i)).collect() } else { Vec::new() };
            let req = reelforge_ofx::Request { input: &img, time: *t, fps, values, others: &refs, range };
            match entry.2.render_with(&req) {
                Ok(out) if out.width == img.width && out.height == img.height => {
                    img = out;
                    ran = true;
                    set_error(clip, s.index, None);
                }
                Ok(_) => {
                    if self.failed.insert((clip, s.index, s.def.revision)) {
                        log::warn!("OpenFX {}: returned a picture of the wrong size", s.def.name);
                    }
                    set_error(clip, s.index, Some("It returned a picture of the wrong size".into()));
                }
                Err(e) => {
                    if self.failed.insert((clip, s.index, s.def.revision)) {
                        log::warn!("OpenFX {}: {e}", s.def.name);
                    }
                    set_error(clip, s.index, Some(e));
                }
            }
        }
        if !ran {
            return frame.clone();
        }
        let out = from_image(&img, frame.pts, key);
        self.last.insert(clip, (key, out.clone(), self.tick));
        out
    }

    /// Let go of instances and results not used in the last `age` calls.
    pub fn trim(&mut self, age: u64) {
        let tick = self.tick;
        self.instances.retain(|_, e| tick - e.3 < age);
        self.last.retain(|_, e| tick - e.2 < age);
        let last = &self.last;
        self.recent.retain(|c, _| last.contains_key(c));
    }

    pub fn clear(&mut self) {
        self.instances.clear();
        self.last.clear();
        self.recent.clear();
        self.failed.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colours_survive_the_trip_through_a_plugin_unchanged() {
        // Linear half floats, with a highlight above 1 and see-through pixels.
        let src = [[0.18f32, 0.5, 2.5, 1.0], [0.1, 0.05, 0.2, 0.5], [0.0, 0.0, 0.0, 0.0], [0.002, 0.7, 0.9, 1.0]];
        let rgba: Vec<u16> = src.iter().flatten().map(|&v| f32_to_f16(v)).collect();
        let f = VideoFrame { pts: reelforge_core::Time::ZERO, width: 2, height: 2, pixels: Pixels::Float(Arc::new(FloatPixels { width: 2, height: 2, rgba })), generation: 1 };
        let img = to_image(&f);
        // Encoded for the plugin: mid grey 0.18 linear is about 0.46.
        assert!((img.rgba[0] - 0.461).abs() < 0.002, "{}", img.rgba[0]);
        assert!(img.rgba[2] > 1.0, "highlights kept");
        let back = from_image(&img, f.pts, 2);
        let Pixels::Float(b) = &back.pixels else { panic!() };
        for (i, want) in src.iter().flatten().enumerate() {
            let got = f16_to_f32(b.rgba[i]);
            assert!((got - want).abs() <= want.abs() * 0.004 + 1e-4, "{i}: {got} vs {want}");
        }
        // 8-bit sources: straight alpha in, premultiplied out.
        let f = VideoFrame::from_rgba(reelforge_core::Time::ZERO, 1, 1, Arc::from(vec![255u8, 128, 0, 128]), 1);
        let img = to_image(&f);
        let a = 128.0 / 255.0;
        assert!((img.rgba[0] - a).abs() < 1e-6 && (img.rgba[1] - 128.0 / 255.0 * a).abs() < 1e-6 && (img.rgba[3] - a).abs() < 1e-6);
    }

    #[test]
    fn a_bundle_that_crashed_is_skipped_next_time() {
        let dir = std::env::temp_dir().join(format!("rf_ofx_state_{}", std::process::id()));
        let plugins = dir.join("plugins");
        let bundle = plugins.join("Broken.ofx.bundle");
        std::fs::create_dir_all(bundle.join("Contents")).unwrap();
        // The last start died loading it.
        std::fs::write(dir.join(LOADING), format!("{}\n", bundle.display())).unwrap();
        let r = scan(std::slice::from_ref(&plugins), Some(&dir));
        assert_eq!(r.len(), 1);
        assert!(r[0].blocked, "{r:?}");
        assert_eq!(blocked(&dir), vec![bundle.clone()]);
        assert!(!dir.join(LOADING).exists());
        // Tried again once unblocked (and here, fails politely: no binary).
        unblock(&dir, &bundle);
        let r = scan(&[plugins], Some(&dir));
        assert!(!r[0].blocked && !r[0].errors.is_empty(), "{r:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
