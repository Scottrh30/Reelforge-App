//! Text titles: lays out and rasterises a [`TitleSpec`] on the CPU into an
//! RGBA image the size of the frame, which the compositor then treats like any
//! other layer (so transforms, keyframes and effects all apply).
//!
//! Draw order, bottom to top: background box, shadow, outline, fill. The
//! outline is made from a distance transform of the glyph coverage (so it's
//! round and even at any width), and the shadow is the text's silhouette,
//! offset and softened with a three-pass box blur (close to a Gaussian).

use ab_glyph::{point, Font, FontArc, FontVec, PxScale, ScaleFont};
use reelforge_core::{FontChoice, TextAlign, TitleSpec};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

/// A rasterised title.
pub struct RenderedTitle {
    pub width: u32,
    pub height: u32,
    /// Straight-alpha sRGB RGBA8, `width × height`.
    pub rgba: Arc<[u8]>,
    /// The visible text (with outline and box, not the shadow) in canvas
    /// pixels: `[x0, y0, x1, y1]`. `None` for an empty title.
    pub bounds: Option<[f32; 4]>,
    /// The chosen font couldn't be loaded, so the built-in one was used.
    pub missing_font: bool,
}

// (The font's bytes are compiled in: loading them can't go differently at
// run time, and the tests load it.)
#[allow(clippy::expect_used)]
fn builtin_font() -> FontArc {
    static F: OnceLock<FontArc> = OnceLock::new();
    F.get_or_init(|| FontArc::try_from_slice(epaint_default_fonts::UBUNTU_LIGHT).expect("built-in font"))
        .clone()
}

/// Load (and cache) the font for `choice`; falls back to the built-in font.
pub fn load_font(choice: &FontChoice) -> (FontArc, bool) {
    type Cache = Mutex<HashMap<(PathBuf, u32), Option<FontArc>>>;
    static CACHE: OnceLock<Cache> = OnceLock::new();
    let Some(path) = &choice.path else { return (builtin_font(), false) };
    let mut cache = CACHE.get_or_init(Default::default).lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let entry = cache.entry((path.clone(), choice.index)).or_insert_with(|| {
        let data = std::fs::read(path).ok()?;
        FontVec::try_from_vec_and_index(data, choice.index).ok().map(FontArc::new)
    });
    match entry {
        Some(f) => (f.clone(), false),
        None => (builtin_font(), true),
    }
}

/// One installed font face, for the font picker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FontEntry {
    pub family: String,
    pub style: String,
    pub path: PathBuf,
    pub index: u32,
}

impl FontEntry {
    pub fn name(&self) -> String {
        if self.style.is_empty() || self.style == "Regular" { self.family.clone() } else { format!("{} {}", self.family, self.style) }
    }
    pub fn choice(&self) -> FontChoice {
        FontChoice { path: Some(self.path.clone()), index: self.index, name: self.name() }
    }
}

/// Folders where the OS keeps fonts.
fn font_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let env = |k: &str| std::env::var_os(k).map(PathBuf::from);
    if cfg!(windows) {
        dirs.extend(env("WINDIR").map(|w| w.join("Fonts")));
        dirs.extend(env("LOCALAPPDATA").map(|l| l.join("Microsoft").join("Windows").join("Fonts")));
    } else if cfg!(target_os = "macos") {
        dirs.extend(["/System/Library/Fonts", "/Library/Fonts"].map(PathBuf::from));
        dirs.extend(env("HOME").map(|h| h.join("Library/Fonts")));
    } else {
        dirs.extend(["/usr/share/fonts", "/usr/local/share/fonts"].map(PathBuf::from));
        dirs.extend(env("HOME").map(|h| h.join(".local/share/fonts")));
        dirs.extend(env("HOME").map(|h| h.join(".fonts")));
    }
    dirs
}

fn collect_files(dir: &Path, depth: u32, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            if depth < 6 {
                collect_files(&p, depth + 1, out);
            }
        } else if p.extension().and_then(|x| x.to_str()).is_some_and(|x| matches!(x.to_ascii_lowercase().as_str(), "ttf" | "otf" | "ttc" | "otc")) {
            out.push(p);
        }
    }
}

/// Every installed font face, sorted by family and style. Reads every font
/// file, so call it off the UI thread.
pub fn system_fonts() -> Vec<FontEntry> {
    let mut files = Vec::new();
    for d in font_dirs() {
        collect_files(&d, 0, &mut files);
    }
    let mut out = Vec::new();
    for path in files {
        let Ok(data) = std::fs::read(&path) else { continue };
        let n = ttf_parser::fonts_in_collection(&data).unwrap_or(1);
        for index in 0..n {
            let Ok(face) = ttf_parser::Face::parse(&data, index) else { continue };
            let name = |ids: &[u16]| {
                ids.iter().find_map(|id| face.names().into_iter().find(|n| n.name_id == *id && n.is_unicode()).and_then(|n| n.to_string()))
            };
            use ttf_parser::name_id::*;
            let Some(family) = name(&[TYPOGRAPHIC_FAMILY, FAMILY]) else { continue };
            let style = name(&[TYPOGRAPHIC_SUBFAMILY, SUBFAMILY]).unwrap_or_default();
            out.push(FontEntry { family, style, path: path.clone(), index });
        }
    }
    out.sort_by_key(|a| (a.family.to_lowercase(), a.style.to_lowercase()));
    out.dedup_by(|a, b| a.family == b.family && a.style == b.style);
    out
}

/// A good default for new titles: a common bold sans-serif if one is installed.
pub fn default_title_font(fonts: &[FontEntry]) -> FontChoice {
    const PREFERRED: [&str; 7] = ["Arial", "Segoe UI", "Helvetica", "DejaVu Sans", "Liberation Sans", "Noto Sans", "Roboto"];
    PREFERRED
        .iter()
        .find_map(|fam| fonts.iter().find(|f| f.family == *fam && f.style == "Bold"))
        .or_else(|| fonts.iter().find(|f| f.style == "Bold"))
        .map(FontEntry::choice)
        .unwrap_or_else(FontChoice::builtin)
}

// ---- Rasterising --------------------------------------------------------------

/// A float image covering part of the canvas.
struct Plane {
    x0: i32,
    y0: i32,
    w: usize,
    h: usize,
    v: Vec<f32>,
}

impl Plane {
    fn new(x0: i32, y0: i32, w: usize, h: usize) -> Self {
        Self { x0, y0, w, h, v: vec![0.0; w * h] }
    }
    fn get(&self, x: i32, y: i32) -> f32 {
        let (lx, ly) = (x - self.x0, y - self.y0);
        if lx < 0 || ly < 0 || lx as usize >= self.w || ly as usize >= self.h {
            0.0
        } else {
            self.v[ly as usize * self.w + lx as usize]
        }
    }
}

/// 1-D squared Euclidean distance transform (Felzenszwalb & Huttenlocher).
fn edt_1d(f: &[f32], out: &mut [f32], v: &mut [usize], z: &mut [f32]) {
    let n = f.len();
    let mut k = 0;
    v[0] = 0;
    z[0] = f32::NEG_INFINITY;
    z[1] = f32::INFINITY;
    for q in 1..n {
        loop {
            let p = v[k];
            let s = ((f[q] + (q * q) as f32) - (f[p] + (p * p) as f32)) / (2.0 * (q as f32 - p as f32));
            if s <= z[k] && k > 0 {
                k -= 1;
                continue;
            }
            if s <= z[k] {
                // k == 0 and the new parabola dominates everywhere: replace it.
                v[0] = q;
                z[0] = f32::NEG_INFINITY;
                z[1] = f32::INFINITY;
                break;
            }
            k += 1;
            v[k] = q;
            z[k] = s;
            z[k + 1] = f32::INFINITY;
            break;
        }
    }
    k = 0;
    for (q, o) in out.iter_mut().enumerate() {
        while z[k + 1] < q as f32 {
            k += 1;
        }
        let p = v[k];
        *o = (q as f32 - p as f32).powi(2) + f[p];
    }
}

/// Distance (in pixels) from each pixel to the nearest pixel where `mask >= 0.5`.
fn distance_to_shape(mask: &Plane) -> Vec<f32> {
    const BIG: f32 = 1e12;
    let (w, h) = (mask.w, mask.h);
    let mut d: Vec<f32> = mask.v.iter().map(|&c| if c >= 0.5 { 0.0 } else { BIG }).collect();
    let n = w.max(h);
    let (mut f, mut out, mut v, mut z) = (vec![0.0; n], vec![0.0; n], vec![0usize; n], vec![0.0; n + 1]);
    for x in 0..w {
        for y in 0..h {
            f[y] = d[y * w + x];
        }
        edt_1d(&f[..h], &mut out[..h], &mut v, &mut z);
        for y in 0..h {
            d[y * w + x] = out[y];
        }
    }
    for y in 0..h {
        f[..w].copy_from_slice(&d[y * w..(y + 1) * w]);
        edt_1d(&f[..w], &mut out[..w], &mut v, &mut z);
        d[y * w..(y + 1) * w].copy_from_slice(&out[..w]);
    }
    d.iter_mut().for_each(|x| *x = x.sqrt());
    d
}

/// In-place box blur of radius `r` along rows (`horizontal`) or columns.
fn box_blur(p: &mut Plane, r: usize, horizontal: bool) {
    if r == 0 {
        return;
    }
    let (w, h) = (p.w, p.h);
    let (len, lines) = if horizontal { (w, h) } else { (h, w) };
    let idx = |line: usize, i: usize| if horizontal { line * w + i } else { i * w + line };
    let mut buf = vec![0.0f32; len];
    let norm = 1.0 / (2 * r + 1) as f32;
    for line in 0..lines {
        let mut sum = 0.0;
        // Window [i - r, i + r], zero outside.
        for i in 0..=r.min(len.saturating_sub(1)) {
            sum += p.v[idx(line, i)];
        }
        for (i, b) in buf.iter_mut().enumerate() {
            *b = sum * norm;
            let add = i + r + 1;
            if add < len {
                sum += p.v[idx(line, add)];
            }
            if i >= r {
                sum -= p.v[idx(line, i - r)];
            }
        }
        for (i, b) in buf.iter().enumerate() {
            p.v[idx(line, i)] = *b;
        }
    }
}

/// Lay out and rasterise `spec` on a transparent `frame`-sized canvas.
/// Where a title's glyphs go on its frame-sized canvas.
pub(crate) struct Layout {
    /// Every glyph, scaled and placed (position = its origin on the baseline).
    pub glyphs: Vec<ab_glyph::Glyph>,
    /// The text block: `[x0, y0, x1, y1]` (ascent of the first line to descent of the last).
    pub text_box: [f32; 4],
    /// Each glyph's line, where it starts along it and how far it advances.
    pub info: Vec<GlyphInfo>,
    /// Each line's width.
    pub widths: Vec<f32>,
    pub line_h: f32,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct GlyphInfo {
    pub line: usize,
    pub x: f32,
    pub advance: f32,
}

/// Lay out `spec`'s text with `font`: lines centred as a block in the frame,
/// each aligned within it, with kerning, letter and line spacing.
pub(crate) fn layout(spec: &TitleSpec, font: &FontArc, frame: [u32; 2]) -> Layout {
    let size = spec.size.max(1.0);
    let scale = PxScale { x: size * spec.scale_x.clamp(0.05, 20.0), y: size * spec.scale_y.clamp(0.05, 20.0) };
    let sf = font.as_scaled(scale);
    let (ascent, descent) = (sf.ascent(), sf.descent());
    let line_h = (ascent - descent + sf.line_gap()) * spec.line_spacing.max(0.1);
    // Kerning by hand is in thousandths of the (stretched) width of the font size.
    let kern_unit = scale.x / 1000.0;
    // Glyphs per line at baseline 0, and each line's width.
    let lines: Vec<(Vec<(ab_glyph::Glyph, f32)>, f32)> = spec
        .shown_lines()
        .into_iter()
        .map(|line| {
            let mut caret = 0.0f32;
            let mut prev: Option<(ab_glyph::GlyphId, u32)> = None;
            let mut glyphs = Vec::new();
            for (at, ch) in line {
                let id = font.glyph_id(ch);
                if let Some((p, pat)) = prev {
                    caret += sf.kern(p, id);
                    if pat != at {
                        caret += spec.kerning.get(&at).copied().unwrap_or(0.0) * kern_unit;
                    }
                }
                glyphs.push((id.with_scale_and_position(scale, point(caret, 0.0)), sf.h_advance(id)));
                caret += sf.h_advance(id) + spec.letter_spacing;
                prev = Some((id, at));
            }
            let width = if glyphs.is_empty() { 0.0 } else { caret - spec.letter_spacing };
            (glyphs, width)
        })
        .collect();
    let (w, h) = (frame[0].max(1) as f32, frame[1].max(1) as f32);
    let block_w = lines.iter().map(|(_, lw)| *lw).fold(0.0, f32::max);
    let block_h = (lines.len().saturating_sub(1)) as f32 * line_h + (ascent - descent);
    let (cx, cy) = (w / 2.0, h / 2.0);
    // A crawl moves the whole block through the frame.
    let [dx, dy] = crawl_shift(spec, block_w, block_h);
    let (cx, cy) = (cx + dx, cy + dy);
    let top = cy - block_h / 2.0;
    let text_box = [cx - block_w / 2.0, top, cx + block_w / 2.0, top + block_h];
    let mut glyphs = Vec::new();
    let mut info = Vec::new();
    let widths: Vec<f32> = lines.iter().map(|(_, w)| *w).collect();
    for (i, (line, lw)) in lines.into_iter().enumerate() {
        let baseline = top + ascent + i as f32 * line_h;
        let x0 = match spec.align {
            TextAlign::Left => text_box[0],
            TextAlign::Center => cx - lw / 2.0,
            TextAlign::Right => text_box[2] - lw,
        };
        for (mut g, advance) in line {
            info.push(GlyphInfo { line: i, x: g.position.x, advance });
            g.position = point(g.position.x + x0, baseline);
            glyphs.push(g);
        }
    }
    Layout { glyphs, text_box, info, widths, line_h }
}

/// How far a crawling title's text block (`bw` × `bh`) is moved from the
/// middle of the canvas, as far along as its `crawl_at` says.
fn crawl_shift(spec: &TitleSpec, bw: f32, bh: f32) -> [f32; 2] {
    use reelforge_core::CrawlKind as K;
    let Some(at) = spec.crawl_at else { return [0.0, 0.0] };
    let c = &spec.crawl;
    let (fw, fh) = (at.frame[0], at.frame[1]);
    let p = if at.progress.is_finite() { at.progress.clamp(0.0, 1.0) } else { 0.0 };
    let lerp = |a: f32, b: f32| a + (b - a) * p;
    // The block's leading edge from the middle of the frame, from start to end.
    let shift = match c.kind {
        K::None => [0.0, 0.0],
        // Its top: just below the frame (or at its top), to just above it (or its last line at the bottom).
        K::RollUp | K::SciFi => [0.0, lerp(if c.start_off { fh / 2.0 } else { -fh / 2.0 }, if c.end_off { -fh / 2.0 - bh } else { fh / 2.0 - bh }) + bh / 2.0],
        K::RollDown => [0.0, lerp(if c.start_off { -fh / 2.0 - bh } else { fh / 2.0 - bh }, if c.end_off { fh / 2.0 } else { -fh / 2.0 }) + bh / 2.0],
        // Its left edge.
        K::CrawlLeft => [lerp(if c.start_off { fw / 2.0 } else { -fw / 2.0 }, if c.end_off { -fw / 2.0 - bw } else { fw / 2.0 - bw }) + bw / 2.0, 0.0],
        K::CrawlRight => [lerp(if c.start_off { -fw / 2.0 - bw } else { fw / 2.0 - bw }, if c.end_off { fw / 2.0 } else { -fw / 2.0 }) + bw / 2.0, 0.0],
    };
    if shift.iter().all(|v| v.is_finite()) { shift } else { [0.0, 0.0] }
}

/// A sci-fi crawl fades out towards the top of the frame (where, tipped
/// back, it's farthest away): `title`'s alpha faded over the top `fade` % of
/// the frame (centred on the canvas, its size in canvas pixels).
fn fade_far(spec: &TitleSpec, title: RenderedTitle) -> RenderedTitle {
    let Some(at) = spec.crawl_at.filter(|_| spec.crawl.kind == reelforge_core::CrawlKind::SciFi) else { return title };
    let (w, h) = (title.width as usize, title.height as usize);
    let top = h as f32 / 2.0 - at.frame[1] / 2.0;
    let len = (spec.crawl.fade.clamp(0.0, 100.0) / 100.0 * at.frame[1]).max(1e-3);
    let mut rgba = title.rgba.to_vec();
    for (y, row) in rgba.chunks_exact_mut(w.max(1) * 4).enumerate().take(h) {
        let k = ((y as f32 + 0.5 - top) / len).clamp(0.0, 1.0);
        let k = k * k * (3.0 - 2.0 * k);
        if k >= 1.0 {
            continue;
        }
        for px in row.chunks_exact_mut(4) {
            px[3] = (px[3] as f32 * k).round() as u8;
        }
    }
    RenderedTitle { rgba: rgba.into(), ..title }
}

pub fn render_title(spec: &TitleSpec, frame: [u32; 2]) -> RenderedTitle {
    let (w, h) = (frame[0].max(1), frame[1].max(1));
    let (font, missing_font) = load_font(&spec.font);
    if spec.places_glyphs() {
        let lay = layout(spec, &font, [w, h]);
        return render_placed(spec, &font, missing_font, &lay, spec.glyph_mods(0.0).as_deref(), None, [w, h]);
    }
    if let Some(shape) = &spec.shape {
        // A shape instead of text: centred, `size` high.
        let placed = shape.placed([w as f32 / 2.0, h as f32 / 2.0], spec.size.max(1.0));
        let (hw, hh) = (spec.size.max(1.0) * shape.aspect * 0.5, spec.size.max(1.0) * 0.5);
        let b = [w as f32 / 2.0 - hw, h as f32 / 2.0 - hh, w as f32 / 2.0 + hw, h as f32 / 2.0 + hh];
        return draw_with(spec, &font, &[], &placed, b, spec.background, [w, h], false);
    }
    let lay = layout(spec, &font, [w, h]);
    draw(spec, &font, &lay.glyphs, lay.text_box, spec.background, [w, h], missing_font)
}

/// `spec` at `t` seconds into a clip `dur` long: with a text animation, its
/// letters, words or lines each where and as visible as the animation has
/// them then (the box behind stays put). Without one, [`render_title`].
pub fn render_title_at(spec: &TitleSpec, frame: [u32; 2], t: f32, dur: f32) -> RenderedTitle {
    // A crawl: where it is at `t` (unless the caller placed it already).
    if let Some(crawled) = spec.crawled(t, dur, frame) {
        return fade_far(&crawled, placed_title_at(&crawled, frame, t, dur));
    }
    fade_far(spec, placed_title_at(spec, frame, t, dur))
}

fn placed_title_at(spec: &TitleSpec, frame: [u32; 2], t: f32, dur: f32) -> RenderedTitle {
    if spec.places_glyphs() {
        let (w, h) = (frame[0].max(1), frame[1].max(1));
        let (font, missing_font) = load_font(&spec.font);
        let lay = layout(spec, &font, [w, h]);
        return render_placed(spec, &font, missing_font, &lay, spec.glyph_mods(t).as_deref(), spec.glyph_states(t, dur).as_deref(), [w, h]);
    }
    let Some(states) = spec.glyph_states(t, dur).filter(|_| spec.shape.is_none()) else { return render_title(spec, frame) };
    let (w, h) = (frame[0].max(1), frame[1].max(1));
    let (font, missing_font) = load_font(&spec.font);
    let lay = layout(spec, &font, [w, h]);
    // Where the whole title is (for selecting it in the Viewer), as if settled.
    let full = draw_bounds(spec, &font, &lay);
    let mut out = if spec.background { draw(spec, &font, &[], lay.text_box, true, [w, h], missing_font).rgba.to_vec() } else { vec![0u8; (w * h * 4) as usize] };
    // Glyphs in the same state are drawn together (outline, shadow and all),
    // then laid over the canvas as one, so fading letters don't lose their
    // edges to their neighbours'.
    let q = |g: &reelforge_core::GlyphState| ((g.alpha * 255.0).round() as i32, (g.dy * spec.size * 8.0).round() as i32);
    let mut groups: Vec<((i32, i32), Vec<usize>)> = Vec::new();
    for (i, g) in states.iter().enumerate().take(lay.glyphs.len()) {
        let k = q(g);
        if k.0 <= 0 {
            continue;
        }
        match groups.iter_mut().find(|(key, _)| *key == k) {
            Some((_, v)) => v.push(i),
            None => groups.push((k, vec![i])),
        }
    }
    for ((alpha, dy8), idx) in groups {
        let dy = dy8 as f32 / 8.0;
        let glyphs: Vec<ab_glyph::Glyph> = idx
            .iter()
            .map(|&i| {
                let mut g = lay.glyphs[i].clone();
                g.position.y += dy;
                g
            })
            .collect();
        let b = lay.text_box;
        let part = draw(spec, &font, &glyphs, [b[0], b[1] + dy, b[2], b[3] + dy], false, [w, h], missing_font);
        let a = alpha as f32 / 255.0;
        // "Over", straight alpha in sRGB (as the title is drawn).
        for (o, p) in out.chunks_exact_mut(4).zip(part.rgba.chunks_exact(4)) {
            let pa = p[3] as f32 / 255.0 * a;
            if pa <= 0.0 {
                continue;
            }
            let oa = o[3] as f32 / 255.0;
            let ra = pa + oa * (1.0 - pa);
            for k in 0..3 {
                o[k] = ((p[k] as f32 * pa + o[k] as f32 * oa * (1.0 - pa)) / ra).round().clamp(0.0, 255.0) as u8;
            }
            o[3] = (ra * 255.0).round() as u8;
        }
    }
    RenderedTitle { width: w, height: h, rgba: out.into(), bounds: full, missing_font }
}

/// A title drawn sharp for the size it's shown at (see [`render_title_sharp`]).
pub struct SharpTitle {
    /// The picture (`k` pixels per frame pixel); its bounds are in frame pixels.
    pub title: RenderedTitle,
    /// How much of the frame it covers (frame pixels), centred on the frame's middle.
    pub size: [f32; 2],
    /// Pixels of the picture per frame pixel.
    pub k: f32,
}

/// The most pixels a sharp title's picture has (about 64 MB).
pub const SHARP_MAX_PIXELS: f32 = 16_777_216.0;

/// `spec`, laid out on a `frame`-sized canvas as [`render_title_at`] lays it
/// out, drawn `k` times as sharp (so a layer scaled up `k` times stays
/// crisp) on only the part of the canvas round its middle that the title
/// reaches (all of it when `whole`: masks or effects that need the full
/// canvas). The picture is kept under [`SHARP_MAX_PIXELS`] (and 8192 a
/// side), so a very big `k` on a big canvas gives a little less.
pub fn render_title_sharp(spec: &TitleSpec, frame: [u32; 2], t: f32, dur: f32, k: f32, whole: bool) -> SharpTitle {
    // A crawl is placed for the whole frame first, so it's where it should
    // be on the smaller canvas round the middle too.
    let crawled = spec.crawled(t, dur, frame);
    let spec = crawled.as_ref().unwrap_or(spec);
    let (w, h) = (frame[0].max(1) as f32, frame[1].max(1) as f32);
    let (cx, cy) = (w / 2.0, h / 2.0);
    let (hw, hh) = if whole {
        (cx, cy)
    } else {
        match reach(spec, [frame[0].max(1), frame[1].max(1)], t, dur) {
            // Out to the farther side either way, so the middle stays the middle;
            // no further than the canvas (the title is cut off there).
            Some(r) => (((cx - r[0]).max(r[2] - cx)).clamp(2.0, cx), ((cy - r[1]).max(r[3] - cy)).clamp(2.0, cy)),
            None => (2.0_f32.min(cx), 2.0_f32.min(cy)),
        }
    };
    let k = k.clamp(1.0, 8.0).min((SHARP_MAX_PIXELS / (4.0 * hw * hh)).sqrt().max(1.0)).min(4096.0 / hw).min(4096.0 / hh);
    let (cw, ch) = (2 * (hw * k).ceil() as u32, 2 * (hh * k).ceil() as u32);
    let size = [cw as f32 / k, ch as f32 / k];
    let origin = [cx - size[0] / 2.0, cy - size[1] / 2.0];
    let mut title = if k == 1.0 && whole && [cw, ch] == [frame[0].max(1), frame[1].max(1)] {
        render_title_at(spec, frame, t, dur)
    } else {
        render_title_at(&spec.magnified(k, origin), [cw, ch], t, dur)
    };
    title.bounds = title.bounds.map(|b| [b[0] / k + origin[0], b[1] / k + origin[1], b[2] / k + origin[0], b[3] / k + origin[1]]);
    SharpTitle { title, size, k }
}

/// How far a title can draw on a `frame`-sized canvas (outline, shadow, box
/// and moving letters included), without drawing it.
fn reach(spec: &TitleSpec, frame: [u32; 2], t: f32, dur: f32) -> Option<[f32; 4]> {
    let (w, h) = (frame[0] as f32, frame[1] as f32);
    let (sdx, sdy) = if spec.shadow {
        let a = spec.shadow_angle.to_radians();
        (spec.shadow_distance * a.cos(), spec.shadow_distance * a.sin())
    } else {
        (0.0, 0.0)
    };
    let soft = if spec.shadow { spec.shadow_softness.max(0.0) * 1.5 } else { 0.0 };
    let pad = if spec.background { spec.background_padding.max(0.0) } else { 0.0 };
    let mut margin = spec.outline.max(0.0) + pad + sdx.abs().max(sdy.abs()) + soft + 4.0 + spec.size.abs() * 0.25;
    let b = if let Some(shape) = &spec.shape {
        let (hw, hh) = (spec.size.max(1.0) * shape.aspect * 0.5, spec.size.max(1.0) * 0.5);
        [w / 2.0 - hw, h / 2.0 - hh, w / 2.0 + hw, h / 2.0 + hh]
    } else {
        let (font, _) = load_font(&spec.font);
        let lay = layout(spec, &font, frame);
        if spec.places_glyphs() {
            let mods = spec.glyph_mods(t);
            let states = spec.glyph_states(t, dur);
            let mut b: Option<[f32; 4]> = (spec.background && spec.path.is_none()).then_some(lay.text_box);
            for p in place(spec, &lay, mods.as_deref(), states.as_deref(), frame) {
                for q in glyph_contours(&font, &lay.glyphs[p.index], lay.info[p.index].advance, &p).iter().flatten() {
                    b = Some(b.map_or([q[0], q[1], q[0], q[1]], |k| [k[0].min(q[0]), k[1].min(q[1]), k[2].max(q[0]), k[3].max(q[1])]));
                }
            }
            b?
        } else {
            if spec.glyph_states(t, dur).is_some() {
                // Letters moving in or out go up to about a line away.
                margin += spec.size.abs() * 1.5;
            }
            let ink = draw_bounds(spec, &font, &lay);
            if ink.is_none() && !spec.background {
                return None;
            }
            let t = lay.text_box;
            ink.map_or(t, |i| [t[0].min(i[0]), t[1].min(i[1]), t[2].max(i[2]), t[3].max(i[3])])
        }
    };
    Some([b[0] - margin, b[1] - margin, b[2] + margin, b[3] + margin])
}

/// One glyph placed on its own: the middle of its baseline at `at`, turned
/// by `angle` (radians, clockwise), scaled, with its opacity and colour.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Placed {
    pub index: usize,
    pub at: [f32; 2],
    pub angle: f32,
    pub scale: [f32; 2],
    pub alpha: f32,
    pub color: [u8; 4],
}

/// A polyline measured along its length.
struct Track {
    pts: Vec<[f32; 2]>,
    /// Distance along it to each point.
    run: Vec<f32>,
    closed: bool,
}

impl Track {
    fn new(pts: &[[f32; 2]]) -> Option<Track> {
        let mut p: Vec<[f32; 2]> = Vec::with_capacity(pts.len());
        for &q in pts {
            if p.last().is_none_or(|l: &[f32; 2]| (l[0] - q[0]).abs() + (l[1] - q[1]).abs() > 1e-3) {
                p.push(q);
            }
        }
        if p.len() < 2 {
            return None;
        }
        let closed = pts.len() > 2 && pts.first() == pts.last();
        let mut run = vec![0.0f32];
        for w in p.windows(2) {
            run.push(run.last().copied().unwrap_or(0.0) + ((w[1][0] - w[0][0]).powi(2) + (w[1][1] - w[0][1]).powi(2)).sqrt());
        }
        Some(Track { pts: p, run, closed })
    }
    fn len(&self) -> f32 {
        self.run.last().copied().unwrap_or(0.0)
    }
    /// The point `d` along it (wrapping round a closed one; carried on
    /// straight past the ends of an open one).
    fn at(&self, d: f32) -> [f32; 2] {
        let len = self.len().max(1e-6);
        let d = if self.closed { d.rem_euclid(len) } else { d };
        let n = self.pts.len();
        let i = if d <= 0.0 { 0 } else if d >= len { n - 2 } else { (self.run.partition_point(|&r| r <= d)).clamp(1, n - 1) - 1 };
        let (a, b) = (self.pts[i], self.pts[i + 1]);
        let seg = (self.run[i + 1] - self.run[i]).max(1e-6);
        let u = (d - self.run[i]) / seg;
        [a[0] + (b[0] - a[0]) * u, a[1] + (b[1] - a[1]) * u]
    }
}

fn rot(v: [f32; 2], a: f32) -> [f32; 2] {
    let (s, c) = a.sin_cos();
    [v[0] * c - v[1] * s, v[0] * s + v[1] * c]
}

/// Where each glyph goes: along its line (or the path), moved, turned,
/// scaled and faded by the animators and the text animation.
pub(crate) fn place(spec: &TitleSpec, lay: &Layout, mods: Option<&[reelforge_core::GlyphMod]>, states: Option<&[reelforge_core::GlyphState]>, frame: [u32; 2]) -> Vec<Placed> {
    let n = lay.glyphs.len();
    // Tracking pushes the rest of the line along (and the line is aligned again).
    let mut before = vec![0.0f32; n];
    let mut extra = vec![0.0f32; lay.widths.len()];
    for i in 0..n {
        let line = lay.info[i].line;
        before[i] = extra[line];
        extra[line] += mods.and_then(|m| m.get(i)).map_or(0.0, |m| m.tracking);
    }
    let track = spec.path.as_ref().and_then(|p| p.line.as_deref()).and_then(|l| Track::new(l));
    let margin = spec.path.as_ref().map_or(0.0, |p| p.margin);
    let perpendicular = spec.path.as_ref().is_none_or(|p| p.perpendicular);
    let (cx, b) = (frame[0] as f32 / 2.0, lay.text_box);
    (0..n)
        .map(|i| {
            let g = &lay.glyphs[i];
            let inf = lay.info[i];
            let lw = lay.widths[inf.line] + extra[inf.line];
            let x = inf.x + before[i] + inf.advance / 2.0;
            let (mut at, mut angle) = match &track {
                None => {
                    let x0 = match spec.align {
                        TextAlign::Left => b[0],
                        TextAlign::Center => cx - lw / 2.0,
                        TextAlign::Right => b[2] - lw,
                    };
                    ([x0 + x, g.position.y], 0.0)
                }
                Some(t) => {
                    let start = match (spec.align, t.closed) {
                        (TextAlign::Left, _) => 0.0,
                        (TextAlign::Center, true) => -lw / 2.0,
                        (TextAlign::Right, true) => -lw,
                        (TextAlign::Center, false) => (t.len() - lw) / 2.0,
                        (TextAlign::Right, false) => t.len() - lw,
                    } + margin;
                    let d = start + x;
                    let p = t.at(d);
                    let h = (inf.advance / 2.0).max(1.0);
                    let (p0, p1) = (t.at(d - h), t.at(d + h));
                    let angle = if perpendicular { (p1[1] - p0[1]).atan2(p1[0] - p0[0]) } else { 0.0 };
                    // Later lines below the first, across the path.
                    let down = rot([0.0, inf.line as f32 * lay.line_h], angle);
                    ([p[0] + down[0], p[1] + down[1]], angle)
                }
            };
            let mut scale = [1.0, 1.0];
            let mut alpha = 1.0;
            let mut color = spec.color;
            if let Some(st) = states.and_then(|s| s.get(i)) {
                alpha *= st.alpha;
                let d = rot([0.0, st.dy * spec.size], angle);
                at = [at[0] + d[0], at[1] + d[1]];
            }
            if let Some(m) = mods.and_then(|m| m.get(i)) {
                let d = if track.is_some() { rot(m.offset, angle) } else { m.offset };
                at = [at[0] + d[0], at[1] + d[1]];
                angle += m.rotation.to_radians();
                scale = m.scale;
                alpha *= m.opacity;
                if let Some(c) = m.color {
                    color = c;
                }
            }
            Placed { index: i, at, angle, scale, alpha, color }
        })
        .collect()
}

/// A placed glyph's outline as polygons on the canvas.
fn glyph_contours(font: &FontArc, g: &ab_glyph::Glyph, advance: f32, p: &Placed) -> Vec<Vec<[f32; 2]>> {
    let Some(outline) = font.outline(g.id) else { return Vec::new() };
    let sf = font.as_scaled(g.scale);
    let (hs, vs) = (sf.h_scale_factor(), sf.v_scale_factor());
    let map = |q: ab_glyph::Point| -> [f32; 2] {
        let local = [(q.x * hs - advance / 2.0) * p.scale[0], -q.y * vs * p.scale[1]];
        let r = rot(local, p.angle);
        [p.at[0] + r[0], p.at[1] + r[1]]
    };
    let mut out: Vec<Vec<[f32; 2]>> = Vec::new();
    let mut last: Option<ab_glyph::Point> = None;
    for c in &outline.curves {
        use ab_glyph::OutlineCurve::*;
        let (p0, end) = match c {
            Line(a, b) => (*a, *b),
            Quad(a, _, b) => (*a, *b),
            Cubic(a, _, _, b) => (*a, *b),
        };
        if last.is_none_or(|l| (l.x - p0.x).abs() + (l.y - p0.y).abs() > 1e-3) {
            out.push(vec![map(p0)]);
        }
        let Some(contour) = out.last_mut() else { continue };
        let px = |a: ab_glyph::Point, b: ab_glyph::Point| ((a.x - b.x) * hs).hypot((a.y - b.y) * vs) * p.scale[0].abs().max(p.scale[1].abs());
        match c {
            Line(_, b) => contour.push(map(*b)),
            Quad(a, c1, b) => {
                let steps = ((px(*a, *c1) + px(*c1, *b)) / 3.0).ceil().clamp(2.0, 16.0) as usize;
                for k in 1..=steps {
                    let t = k as f32 / steps as f32;
                    let u = 1.0 - t;
                    contour.push(map(ab_glyph::point(u * u * a.x + 2.0 * u * t * c1.x + t * t * b.x, u * u * a.y + 2.0 * u * t * c1.y + t * t * b.y)));
                }
            }
            Cubic(a, c1, c2, b) => {
                let steps = ((px(*a, *c1) + px(*c1, *c2) + px(*c2, *b)) / 3.0).ceil().clamp(2.0, 20.0) as usize;
                for k in 1..=steps {
                    let t = k as f32 / steps as f32;
                    let u = 1.0 - t;
                    let (w0, w1, w2, w3) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
                    contour.push(map(ab_glyph::point(w0 * a.x + w1 * c1.x + w2 * c2.x + w3 * b.x, w0 * a.y + w1 * c1.y + w2 * c2.y + w3 * b.y)));
                }
            }
        }
        last = Some(end);
    }
    out.retain(|c| c.len() >= 3);
    out
}

/// Draw a title whose glyphs are placed one by one ([`place`]): glyphs
/// alike in opacity and colour are drawn together (outline and shadow
/// included) and laid over the canvas.
fn render_placed(spec: &TitleSpec, font: &FontArc, missing_font: bool, lay: &Layout, mods: Option<&[reelforge_core::GlyphMod]>, states: Option<&[reelforge_core::GlyphState]>, frame: [u32; 2]) -> RenderedTitle {
    let [w, h] = frame;
    let placed = place(spec, lay, mods, states, frame);
    let boxed = spec.background && spec.path.is_none();
    let mut out = if boxed { draw(spec, font, &[], lay.text_box, true, frame, missing_font).rgba.to_vec() } else { vec![0u8; (w * h * 4) as usize] };
    let mut bounds: Option<[f32; 4]> = None;
    let mut grow = |b: [f32; 4]| bounds = Some(bounds.map_or(b, |k| [k[0].min(b[0]), k[1].min(b[1]), k[2].max(b[2]), k[3].max(b[3])]));
    if boxed {
        if let Some(b) = draw_bounds(spec, font, lay) {
            grow(b);
        }
    }
    let mut groups: Vec<((u8, [u8; 4]), Vec<Vec<[f32; 2]>>)> = Vec::new();
    for p in &placed {
        let a = (p.alpha.clamp(0.0, 1.0) * 255.0).round() as u8;
        if a == 0 {
            continue;
        }
        let contours = glyph_contours(font, &lay.glyphs[p.index], lay.info[p.index].advance, p);
        if contours.is_empty() {
            continue;
        }
        let key = (a, p.color);
        match groups.iter_mut().find(|(k, _)| *k == key) {
            Some((_, c)) => c.extend(contours),
            None => groups.push((key, contours)),
        }
    }
    let o = spec.outline.max(0.0);
    for ((alpha, color), contours) in groups {
        let mut gb = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
        for q in contours.iter().flatten() {
            gb = [gb[0].min(q[0]), gb[1].min(q[1]), gb[2].max(q[0]), gb[3].max(q[1])];
        }
        let styled = TitleSpec { color, ..spec.clone() };
        let part = draw_with(&styled, font, &[], &[reelforge_core::ShapePath { contours, even_odd: false }], gb, false, frame, missing_font);
        grow([gb[0] - o, gb[1] - o, gb[2] + o, gb[3] + o]);
        // Lay it over the canvas, only where it can have drawn.
        let (sdx, sdy) = if spec.shadow { let a = spec.shadow_angle.to_radians(); (spec.shadow_distance * a.cos(), spec.shadow_distance * a.sin()) } else { (0.0, 0.0) };
        let m = o + sdx.abs().max(sdy.abs()) + if spec.shadow { spec.shadow_softness.max(0.0) * 1.5 } else { 0.0 } + 4.0 + spec.size * 0.25;
        let (x0, y0) = (((gb[0] - m).floor().max(0.0)) as u32, ((gb[1] - m).floor().max(0.0)) as u32);
        let (x1, y1) = (((gb[2] + m).ceil().max(0.0) as u32).min(w), ((gb[3] + m).ceil().max(0.0) as u32).min(h));
        let a = alpha as f32 / 255.0;
        for y in y0..y1 {
            for x in x0..x1 {
                let i = ((y * w + x) * 4) as usize;
                let pa = part.rgba[i + 3] as f32 / 255.0 * a;
                if pa <= 0.0 {
                    continue;
                }
                let oa = out[i + 3] as f32 / 255.0;
                let ra = pa + oa * (1.0 - pa);
                for k in 0..3 {
                    out[i + k] = ((part.rgba[i + k] as f32 * pa + out[i + k] as f32 * oa * (1.0 - pa)) / ra).round().clamp(0.0, 255.0) as u8;
                }
                out[i + 3] = (ra * 255.0).round() as u8;
            }
        }
    }
    RenderedTitle { width: w, height: h, rgba: out.into(), bounds, missing_font }
}

/// Where a title's text (and box) is, settled, without drawing it.
fn draw_bounds(spec: &TitleSpec, font: &FontArc, lay: &Layout) -> Option<[f32; 4]> {
    let mut ink: Option<[f32; 4]> = None;
    for g in &lay.glyphs {
        if let Some(og) = font.outline_glyph(g.clone()) {
            let b = og.px_bounds();
            ink = Some(match ink {
                None => [b.min.x, b.min.y, b.max.x, b.max.y],
                Some(k) => [k[0].min(b.min.x), k[1].min(b.min.y), k[2].max(b.max.x), k[3].max(b.max.y)],
            });
        }
    }
    let o = spec.outline.max(0.0);
    if spec.background {
        let (t, i, p) = (lay.text_box, ink.unwrap_or(lay.text_box), spec.background_padding.max(0.0));
        return Some([t[0].min(i[0]) - p - o, t[1] - p - o, t[2].max(i[2]) + p + o, t[3] + p + o]);
    }
    ink.map(|k| [k[0] - o, k[1] - o, k[2] + o, k[3] + o])
}

/// Draw `glyphs` (laid out in a block `text_box`) as `spec` styles them,
/// with its box behind them when `background`, on a transparent canvas.
fn draw(spec: &TitleSpec, font: &FontArc, glyphs: &[ab_glyph::Glyph], text_box: [f32; 4], background: bool, frame: [u32; 2], missing_font: bool) -> RenderedTitle {
    draw_with(spec, font, glyphs, &[], text_box, background, frame, missing_font)
}

/// Coverage of filled paths (canvas pixels) added to `plane`: each row
/// sampled at 4 heights, with exact coverage across.
fn fill_paths(plane: &mut Plane, paths: &[reelforge_core::ShapePath]) {
    const SUB: usize = 4;
    let (w, h) = (plane.w, plane.h);
    let mut acc = vec![0.0f32; w * h];
    for p in paths {
        let edges: Vec<([f32; 2], [f32; 2])> = p.contours.iter().flat_map(|c| (0..c.len()).map(move |i| (c[i], c[(i + 1) % c.len()]))).collect();
        let mut xs: Vec<(f32, i32)> = Vec::new();
        for row in 0..h {
            for k in 0..SUB {
                let y = (plane.y0 + row as i32) as f32 + (k as f32 + 0.5) / SUB as f32;
                xs.clear();
                for (a, b) in &edges {
                    if (a[1] <= y) != (b[1] <= y) {
                        let x = a[0] + (y - a[1]) / (b[1] - a[1]) * (b[0] - a[0]);
                        xs.push((x - plane.x0 as f32, if b[1] > a[1] { 1 } else { -1 }));
                    }
                }
                xs.sort_by(|a, b| a.0.total_cmp(&b.0));
                let mut wind = 0;
                for i in 0..xs.len() {
                    wind += xs[i].1;
                    let filled = if p.even_odd { wind % 2 != 0 } else { wind != 0 };
                    if filled && i + 1 < xs.len() {
                        // Span from xs[i] to xs[i + 1]: exact coverage across the pixels it touches.
                        let (x0, x1) = (xs[i].0.max(0.0), xs[i + 1].0.min(w as f32));
                        if x1 <= x0 {
                            continue;
                        }
                        let line = &mut acc[row * w..(row + 1) * w];
                        let (a, b) = (x0.floor() as usize, (x1.ceil() as usize).min(w));
                        for (px, v) in line.iter_mut().enumerate().take(b).skip(a) {
                            let cov = (x1.min(px as f32 + 1.0) - x0.max(px as f32)).max(0.0);
                            *v += cov / SUB as f32;
                        }
                    }
                }
            }
        }
    }
    for (v, a) in plane.v.iter_mut().zip(acc) {
        *v = v.max(a.min(1.0));
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_with(spec: &TitleSpec, font: &FontArc, glyphs: &[ab_glyph::Glyph], shapes: &[reelforge_core::ShapePath], text_box: [f32; 4], background: bool, frame: [u32; 2], missing_font: bool) -> RenderedTitle {
    let [w, h] = frame;
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    let font = font.clone();
    let has_text = !shapes.is_empty() || glyphs.iter().any(|g| font.outline_glyph(g.clone()).is_some());
    if !has_text && !background {
        return RenderedTitle { width: w, height: h, rgba: rgba.into(), bounds: None, missing_font };
    }

    // Work only in the region the title can touch.
    let outline = spec.outline.max(0.0);
    let pad = if background { spec.background_padding.max(0.0) } else { 0.0 };
    let (sdx, sdy) = if spec.shadow {
        let a = spec.shadow_angle.to_radians();
        (spec.shadow_distance * a.cos(), spec.shadow_distance * a.sin())
    } else {
        (0.0, 0.0)
    };
    let soft = if spec.shadow { spec.shadow_softness.max(0.0) } else { 0.0 };
    let margin = outline + pad + sdx.abs().max(sdy.abs()) + soft * 1.5 + 4.0 + spec.size * 0.25;
    let rx0 = ((text_box[0] - margin).floor() as i32).max(0);
    let ry0 = ((text_box[1] - margin).floor() as i32).max(0);
    let rx1 = ((text_box[2] + margin).ceil() as i32).min(w as i32);
    let ry1 = ((text_box[3] + margin).ceil() as i32).min(h as i32);
    if rx1 <= rx0 || ry1 <= ry0 {
        return RenderedTitle { width: w, height: h, rgba: rgba.into(), bounds: None, missing_font };
    }
    let (rw, rh) = ((rx1 - rx0) as usize, (ry1 - ry0) as usize);

    // ---- Fill coverage ----
    let mut fill = Plane::new(rx0, ry0, rw, rh);
    let mut ink: Option<[f32; 4]> = None; // tight bounds of the glyph pixels
    for g in glyphs {
        {
            let Some(og) = font.outline_glyph(g.clone()) else { continue };
            let b = og.px_bounds();
            ink = Some(match ink {
                None => [b.min.x, b.min.y, b.max.x, b.max.y],
                Some(k) => [k[0].min(b.min.x), k[1].min(b.min.y), k[2].max(b.max.x), k[3].max(b.max.y)],
            });
            og.draw(|x, y, c| {
                let (px, py) = (b.min.x as i32 + x as i32 - rx0, b.min.y as i32 + y as i32 - ry0);
                if px >= 0 && py >= 0 && (px as usize) < rw && (py as usize) < rh {
                    let v = &mut fill.v[py as usize * rw + px as usize];
                    *v = v.max(c.min(1.0));
                }
            });
        }
    }

    if !shapes.is_empty() {
        fill_paths(&mut fill, shapes);
        ink = Some(text_box);
    }

    // ---- Outline: everything within `outline` px of the glyphs ----
    let shape = if outline > 0.0 {
        let dist = distance_to_shape(&fill);
        let mut o = Plane::new(rx0, ry0, rw, rh);
        for (i, v) in o.v.iter_mut().enumerate() {
            *v = (outline + 0.5 - dist[i]).clamp(0.0, 1.0).max(fill.v[i]);
        }
        Some(o)
    } else {
        None
    };
    let silhouette = shape.as_ref().unwrap_or(&fill);

    // ---- Shadow: the silhouette, offset and blurred ----
    let shadow = spec.shadow.then(|| {
        let mut s = Plane::new(rx0, ry0, rw, rh);
        for y in 0..rh {
            for x in 0..rw {
                // Bilinear sample at (x - dx, y - dy) so sub-pixel offsets stay smooth.
                let (fx, fy) = (x as f32 + rx0 as f32 - sdx, y as f32 + ry0 as f32 - sdy);
                let (ix, iy) = (fx.floor() as i32, fy.floor() as i32);
                let (tx, ty) = (fx - ix as f32, fy - iy as f32);
                let a = silhouette.get(ix, iy) * (1.0 - tx) + silhouette.get(ix + 1, iy) * tx;
                let b = silhouette.get(ix, iy + 1) * (1.0 - tx) + silhouette.get(ix + 1, iy + 1) * tx;
                s.v[y * rw + x] = a * (1.0 - ty) + b * ty;
            }
        }
        // Three box passes ≈ a Gaussian with sigma ≈ softness / 2.
        let r = (soft / 2.0).round() as usize;
        for _ in 0..3 {
            box_blur(&mut s, r, true);
            box_blur(&mut s, r, false);
        }
        s
    });

    // ---- Background box around the text ----
    let ink = ink.unwrap_or(text_box);
    // The box spans the layout block horizontally and the font's full line height vertically,
    // so it doesn't jump around as letters with descenders come and go.
    let box_rect = [text_box[0].min(ink[0]) - pad - outline, text_box[1] - pad - outline, text_box[2].max(ink[2]) + pad + outline, text_box[3] + pad + outline];
    let in_box = |x: f32, y: f32| -> f32 {
        // Anti-aliased edges: coverage of the pixel square by the rectangle.
        let cov = |lo: f32, hi: f32, p: f32| ((p + 0.5).min(hi) - (p - 0.5).max(lo)).clamp(0.0, 1.0);
        cov(box_rect[0], box_rect[2], x) * cov(box_rect[1], box_rect[3], y)
    };

    // ---- Composite (premultiplied, in sRGB space like a paint program) ----
    let layer = |acc: &mut [f32; 4], col: [u8; 4], alpha: f32| {
        let a = alpha * col[3] as f32 / 255.0;
        if a <= 0.0 {
            return;
        }
        for k in 0..3 {
            acc[k] = col[k] as f32 / 255.0 * a + acc[k] * (1.0 - a);
        }
        acc[3] = a + acc[3] * (1.0 - a);
    };
    for y in 0..rh {
        for x in 0..rw {
            let i = y * rw + x;
            let (px, py) = (x as i32 + rx0, y as i32 + ry0);
            let mut acc = [0.0f32; 4];
            if background {
                layer(&mut acc, spec.background_color, in_box(px as f32 + 0.5, py as f32 + 0.5));
            }
            if let Some(s) = &shadow {
                layer(&mut acc, spec.shadow_color, s.v[i]);
            }
            if let Some(o) = &shape {
                layer(&mut acc, spec.outline_color, o.v[i]);
            }
            layer(&mut acc, spec.color, fill.v[i]);
            if acc[3] > 0.0 {
                let o = ((py as u32 * w + px as u32) * 4) as usize;
                for k in 0..3 {
                    rgba[o + k] = ((acc[k] / acc[3]).clamp(0.0, 1.0) * 255.0).round() as u8;
                }
                rgba[o + 3] = (acc[3].clamp(0.0, 1.0) * 255.0).round() as u8;
            }
        }
    }

    let bounds = if background {
        Some(box_rect)
    } else if has_text {
        Some([ink[0] - outline, ink[1] - outline, ink[2] + outline, ink[3] + outline])
    } else {
        None
    };
    RenderedTitle { width: w, height: h, rgba: rgba.into(), bounds, missing_font }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reelforge_core::TextCase;

    fn px(r: &RenderedTitle, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * r.width + x) * 4) as usize;
        [r.rgba[i], r.rgba[i + 1], r.rgba[i + 2], r.rgba[i + 3]]
    }

    fn plain(text: &str) -> TitleSpec {
        TitleSpec { text: text.into(), size: 60.0, shadow: false, ..TitleSpec::default() }
    }

    /// Alpha at frame point (x, y) of a sharp title (nearest picture pixel).
    fn alpha_at(r: &SharpTitle, frame: [u32; 2], x: f32, y: f32) -> u8 {
        let ox = frame[0] as f32 / 2.0 - r.size[0] / 2.0;
        let oy = frame[1] as f32 / 2.0 - r.size[1] / 2.0;
        let (px_, py_) = (((x - ox) * r.k) as i64, ((y - oy) * r.k) as i64);
        if px_ < 0 || py_ < 0 || px_ >= r.title.width as i64 || py_ >= r.title.height as i64 {
            return 0;
        }
        px(&r.title, px_ as u32, py_ as u32)[3]
    }

    /// Titles drawn for their size: at 1× exactly the middle of the usual
    /// picture, only as big as the text needs; magnified, the same shapes in
    /// the same places with edges a fraction as soft; text on a path too.
    #[test]
    fn titles_stay_sharp_when_magnified() {
        let frame = [640u32, 360];
        let spec = TitleSpec { text: "Crisp".into(), size: 48.0, outline: 2.0, outline_color: [0, 0, 0, 255], shadow: true, shadow_distance: 4.0, shadow_softness: 2.0, ..TitleSpec::default() };
        let full = render_title_at(&spec, frame, 0.0, 5.0);
        let one = render_title_sharp(&spec, frame, 0.0, 5.0, 1.0, false);
        assert_eq!(one.k, 1.0);
        assert!(one.title.width * one.title.height * 3 < frame[0] * frame[1], "only round the text: {}×{}", one.title.width, one.title.height);
        let (ox, oy) = ((frame[0] - one.title.width) / 2, (frame[1] - one.title.height) / 2);
        for y in 0..one.title.height {
            for x in 0..one.title.width {
                let (a, b) = (px(&one.title, x, y), px(&full, x + ox, y + oy));
                // (The colour of fully transparent pixels doesn't show.)
                assert!(a == b || (a[3] == 0 && b[3] == 0), "at {x},{y}: {a:?} vs {b:?}");
            }
        }
        // Nothing of the full picture is left outside.
        let inside = |x: u32, y: u32| x >= ox && y >= oy && x < ox + one.title.width && y < oy + one.title.height;
        assert!((0..frame[1]).all(|y| (0..frame[0]).all(|x| inside(x, y) || px(&full, x, y)[3] == 0)));
        assert_eq!(one.title.bounds, full.bounds);
        // 4× as fine: same bounds, same coverage (sampled), sharper edges.
        let four = render_title_sharp(&spec, frame, 0.0, 5.0, 4.0, false);
        assert_eq!(four.k, 4.0);
        let (b1, b4) = (full.bounds.unwrap(), four.title.bounds.unwrap());
        for i in 0..4 {
            assert!((b1[i] - b4[i]).abs() < 1.5, "{b1:?} vs {b4:?}");
        }
        let mut diff = 0.0;
        let mut n = 0.0;
        for y in (b1[1] as u32)..(b1[3] as u32) {
            for x in (b1[0] as u32)..(b1[2] as u32) {
                // The 4× picture averaged over this frame pixel.
                let mut sum = 0.0;
                for j in 0..4 {
                    for i in 0..4 {
                        sum += alpha_at(&four, frame, x as f32 + (i as f32 + 0.5) / 4.0, y as f32 + (j as f32 + 0.5) / 4.0) as f32;
                    }
                }
                diff += (sum / 16.0 - px(&full, x, y)[3] as f32).abs();
                n += 1.0;
            }
        }
        // (The outline is a little finer drawn finely, so not exactly the same.)
        assert!(diff / n < 16.0, "the same shapes: mean alpha difference {}", diff / n);
        let fill = plain("Crisp");
        let (f1, f4) = (render_title_at(&fill, frame, 0.0, 5.0), render_title_sharp(&fill, frame, 0.0, 5.0, 4.0, false));
        let b = f1.bounds.unwrap();
        let (mut diff, mut n) = (0.0, 0.0);
        for y in (b[1] as u32)..(b[3] as u32) {
            for x in (b[0] as u32)..(b[2] as u32) {
                let mut sum = 0.0;
                for j in 0..4 {
                    for i in 0..4 {
                        sum += alpha_at(&f4, frame, x as f32 + (i as f32 + 0.5) / 4.0, y as f32 + (j as f32 + 0.5) / 4.0) as f32;
                    }
                }
                diff += (sum / 16.0 - px(&f1, x, y)[3] as f32).abs();
                n += 1.0;
            }
        }
        assert!(diff / n < 2.0, "the same letters: mean alpha difference {}", diff / n);
        // Edge softness: partly covered samples along the middle row, per frame pixel.
        let row = (b1[1] + b1[3]) / 2.0;
        let soft = |r: &SharpTitle, step: f32| {
            let mut c = 0;
            let mut x = b1[0];
            while x < b1[2] {
                let a = alpha_at(r, frame, x, row);
                if a > 20 && a < 235 {
                    c += 1;
                }
                x += step;
            }
            c as f32 * step
        };
        // (Letters only: a soft shadow is soft at any size.)
        let fill1 = render_title_sharp(&fill, frame, 0.0, 5.0, 1.0, false);
        let (s1, s4) = (soft(&fill1, 1.0), soft(&f4, 0.25));
        assert!(s4 < s1 * 0.5, "edges {s4} frame pixels soft at 4×, {s1} at 1×");
        // Masks or effects: the whole canvas, still magnified (within the pixel budget).
        let whole = render_title_sharp(&spec, frame, 0.0, 5.0, 2.0, true);
        assert_eq!((whole.size, whole.title.width, whole.title.height), ([640.0, 360.0], 1280, 720));
        let big = render_title_sharp(&spec, [3840, 2160], 0.0, 5.0, 8.0, true);
        assert!(big.title.width as f32 * big.title.height as f32 <= SHARP_MAX_PIXELS * 1.01 && big.k > 1.0 && big.k < 2.0, "{}", big.k);
        // Text on a path lands where it did.
        let line: Vec<[f32; 2]> = (0..=32).map(|i| { let a = i as f32 / 32.0 * std::f32::consts::PI; [320.0 - 150.0 * a.cos(), 220.0 - 120.0 * a.sin()] }).collect();
        let on_path = TitleSpec { path: Some(reelforge_core::TextPath { mask: 0, reverse: false, perpendicular: true, open: true, margin: 0.0, line: Some(std::sync::Arc::new(line)) }), ..plain("Round the arc") };
        let p1 = render_title_at(&on_path, frame, 0.0, 5.0).bounds.unwrap();
        let p3 = render_title_sharp(&on_path, frame, 0.0, 5.0, 3.0, false);
        let b = p3.title.bounds.unwrap();
        for i in 0..4 {
            assert!((p1[i] - b[i]).abs() < 1.5, "{p1:?} vs {b:?}");
        }
        assert!(p3.size[0] < 640.0 * 0.9 && p3.size[1] < 360.0 * 0.95, "{:?}", p3.size);
    }

    /// Animated titles: letters show up in turn, fades are partial, sliding
    /// lines start low, and once settled it's the same picture as without.
    #[test]
    fn animated_titles_draw_each_piece_in_its_state() {
        use reelforge_core::{TextAnim, TextAnimKind};
        let alpha_in = |r: &RenderedTitle, x0: u32, x1: u32| (0..r.height).flat_map(|y| (x0..x1).map(move |x| (x, y))).map(|(x, y)| px(r, x, y)[3] as u32).max().unwrap_or(0);
        let mut s = plain("HHHH");
        s.anim = TextAnim::preset(TextAnimKind::Typewriter);
        let settled = render_title(&s, [400, 200]);
        let b = settled.bounds.unwrap();
        let (l, r) = (b[0] as u32, b[2] as u32);
        let quarter = (r - l) / 4;
        // Typing: at 0.07 s two letters are there, the last two not yet.
        let early = render_title_at(&s, [400, 200], 0.07, 5.0);
        assert!(alpha_in(&early, l, l + quarter) > 200);
        assert_eq!(alpha_in(&early, r - quarter + 2, r), 0);
        assert_eq!(early.bounds, settled.bounds, "selected by where the whole title will be");
        // Settled: the same as a still title (box and shadow too).
        let mut full = s.clone();
        (full.background, full.shadow) = (true, true);
        let a = render_title(&full, [400, 200]);
        let z = render_title_at(&full, [400, 200], 4.0, 5.0);
        let diff = a.rgba.iter().zip(z.rgba.iter()).map(|(p, q)| (*p as i32 - *q as i32).abs()).max().unwrap();
        assert!(diff <= 2, "{diff}");
        // Fading words: one word halfway in is about half as opaque.
        let mut f = plain("HH HH");
        f.anim = TextAnim { kind: TextAnimKind::FadeWords, stagger: 1.0, length: 1.0, delay: 0.0, out: false };
        let half = render_title_at(&f, [400, 200], 0.5, 5.0);
        let b = render_title(&f, [400, 200]).bounds.unwrap();
        let a0 = alpha_in(&half, b[0] as u32, b[0] as u32 + 20);
        assert!((100..160).contains(&a0), "{a0}");
        assert_eq!(alpha_in(&half, b[2] as u32 - 20, b[2] as u32), 0);
        // Sliding lines: lower than where they settle.
        let mut sl = plain("H");
        sl.anim = TextAnim { kind: TextAnimKind::SlideLines, stagger: 0.0, length: 1.0, delay: 0.0, out: false };
        let mid = render_title_at(&sl, [200, 200], 0.3, 5.0);
        let top_row = |r: &RenderedTitle| (0..r.height).find(|&y| (0..r.width).any(|x| px(r, x, y)[3] > 20)).unwrap();
        assert!(top_row(&mid) > top_row(&render_title(&sl, [200, 200])) + 5);
    }

    #[test]
    fn text_is_centred_and_opaque_where_the_glyphs_are() {
        let r = render_title(&plain("HH"), [400, 200]);
        let b = r.bounds.unwrap();
        let mid = ((b[0] + b[2]) / 2.0, (b[1] + b[3]) / 2.0);
        assert!((mid.0 - 200.0).abs() < 3.0 && (mid.1 - 100.0).abs() < 12.0, "centred: {b:?}");
        // Somewhere in the middle of the left stem of the first H is solid white.
        let solid = (b[0] as u32..b[2] as u32).any(|x| px(&r, x, 100) == [255, 255, 255, 255]);
        assert!(solid);
        assert_eq!(px(&r, 5, 5)[3], 0, "transparent elsewhere");
    }

    #[test]
    fn scale_case_and_kerning_change_the_layout() {
        let font = builtin_font();
        let wide = |spec: &TitleSpec| {
            let l = layout(spec, &font, [800, 300]);
            (l.text_box[2] - l.text_box[0], l.text_box[3] - l.text_box[1])
        };
        let base = plain("Type");
        let (w0, h0) = wide(&base);
        let (w, h) = wide(&TitleSpec { scale_x: 1.5, ..base.clone() });
        assert!((w / w0 - 1.5).abs() < 0.02 && (h - h0).abs() < 0.01, "{w} {w0} {h} {h0}");
        let (w, h) = wide(&TitleSpec { scale_y: 2.0, ..base.clone() });
        assert!((w - w0).abs() < 0.01 && (h / h0 - 2.0).abs() < 0.02);
        assert!(wide(&TitleSpec { case: TextCase::Upper, ..base.clone() }).0 > w0);
        // Kerning before "y" (position 1): −100 thousandths of 60 px pulls "ype" 6 px left.
        let kerned = TitleSpec { kerning: [(1, -100.0)].into(), ..base.clone() };
        assert!((wide(&kerned).0 - (w0 - 6.0)).abs() < 0.01);
        let (a, b) = (layout(&base, &font, [800, 300]), layout(&kerned, &font, [800, 300]));
        let gap = |l: &Layout| l.glyphs[1].position.x - l.glyphs[0].position.x;
        assert!((gap(&a) - gap(&b) - 6.0).abs() < 0.01);
        assert!((a.glyphs[3].position.x - a.glyphs[2].position.x - (b.glyphs[3].position.x - b.glyphs[2].position.x)).abs() < 0.01);
    }

    #[test]
    fn text_follows_a_path_and_animators_move_letters() {
        use reelforge_core::{RangeSelector, TextAnimator, TextPath};
        // Along a straight path down the middle of the frame, upright text turns to run down it.
        let line = std::sync::Arc::new(vec![[200.0, 20.0], [200.0, 380.0]]);
        let spec = TitleSpec { text: "ABCDEFGH".into(), size: 40.0, shadow: false, align: TextAlign::Left, path: Some(TextPath { mask: 0, reverse: false, perpendicular: true, open: true, margin: 10.0, line: Some(line) }), ..TitleSpec::default() };
        let r = render_title(&spec, [400, 400]);
        let b = r.bounds.unwrap();
        // Turned a quarter: tall and narrow, standing on x = 200 (their tops to its right), from 10 px in.
        assert!(b[3] - b[1] > 4.0 * (b[2] - b[0]) && b[2] - b[0] < 40.0, "{b:?}");
        assert!((b[0] - 200.0).abs() < 2.0 && b[1] > 28.0 && b[1] < 36.0, "{b:?}");
        // An animator fading out the second half: only the first half drawn.
        let a = TextAnimator { opacity: 0.0, selector: RangeSelector { start: 50.0, end: 100.0, ..Default::default() }, ..Default::default() };
        let plain_spec = plain("IIII");
        let full = render_title(&plain_spec, [400, 200]).bounds.unwrap();
        let half = render_title(&TitleSpec { animators: vec![a.clone()], ..plain_spec.clone() }, [400, 200]).bounds.unwrap();
        assert!((half[0] - full[0]).abs() < 1.0 && half[2] < (full[0] + full[2]) / 2.0 + 2.0, "{half:?} {full:?}");
        // The second half moved down 30 px instead.
        let moved = TextAnimator { position: [0.0, 30.0], opacity: 100.0, ..a };
        let m = render_title(&TitleSpec { animators: vec![moved], ..plain_spec }, [400, 200]).bounds.unwrap();
        assert!((m[3] - full[3] - 30.0).abs() < 1.0 && (m[1] - full[1]).abs() < 1.0, "{m:?} {full:?}");
    }

    #[test]
    fn empty_titles_are_blank() {
        let r = render_title(&plain("  \n "), [64, 64]);
        assert!(r.bounds.is_none());
        assert!(r.rgba.iter().all(|&v| v == 0));
    }

    #[test]
    fn more_lines_are_taller_and_alignment_moves_lines() {
        let one = render_title(&plain("Wide line"), [600, 400]).bounds.unwrap();
        let two = render_title(&plain("Wide line\nx"), [600, 400]).bounds.unwrap();
        assert!(two[3] - two[1] > (one[3] - one[1]) * 1.5);
        // With left alignment the short second line starts at the block's left edge.
        let left = TitleSpec { align: TextAlign::Left, ..plain("Wide line\nx") };
        let right = TitleSpec { align: TextAlign::Right, ..plain("Wide line\nx") };
        let ink_x = |r: &RenderedTitle, y0: u32, y1: u32| {
            (0..r.width).find(|&x| (y0..y1).any(|y| px(r, x, y)[3] > 128)).unwrap()
        };
        let (rl, rr) = (render_title(&left, [600, 400]), render_title(&right, [600, 400]));
        let low = (two[3] - 25.0) as u32..two[3] as u32;
        assert!(ink_x(&rl, low.start, low.end) + 100 < ink_x(&rr, low.start, low.end));
    }

    #[test]
    fn outline_shadow_and_box() {
        let base = plain("I");
        let r0 = render_title(&base, [300, 200]);
        let b0 = r0.bounds.unwrap();
        // A red 6 px outline grows the bounds by 6 and paints red just outside the stem.
        let o = TitleSpec { outline: 6.0, outline_color: [255, 0, 0, 255], ..base.clone() };
        let r1 = render_title(&o, [300, 200]);
        let b1 = r1.bounds.unwrap();
        assert!((b0[0] - b1[0] - 6.0).abs() < 0.01);
        let edge = (0..300).find(|&x| px(&r1, x, 100)[3] == 255).unwrap();
        assert_eq!(px(&r1, edge + 1, 100), [255, 0, 0, 255], "outline colour at the outer edge");
        // A hard shadow to the right: black to the right of the glyph, nothing to the left.
        let s = TitleSpec { shadow: true, shadow_distance: 10.0, shadow_angle: 0.0, shadow_softness: 0.0, shadow_color: [0, 0, 0, 255], ..base.clone() };
        let r2 = render_title(&s, [300, 200]);
        // The shadow is the glyph moved 10 px right: sample the middle of that copy.
        let right_of_stem = ((b0[0] + b0[2]) / 2.0 + 10.0) as u32;
        assert_eq!(px(&r2, right_of_stem, 100), [0, 0, 0, 255]);
        assert_eq!(px(&r2, (b0[0] - 5.0) as u32, 100)[3], 0);
        // A box: opaque blue well outside the glyph but inside the padding.
        let bx = TitleSpec { background: true, background_color: [0, 0, 255, 255], background_padding: 20.0, ..base };
        let r3 = render_title(&bx, [300, 200]);
        assert_eq!(px(&r3, (b0[0] - 10.0) as u32, 100), [0, 0, 255, 255]);
        let b3 = r3.bounds.unwrap();
        assert!(b3[0] < b0[0] - 15.0);
    }

    #[test]
    fn distance_transform_is_exact_for_a_point() {
        let mut p = Plane::new(0, 0, 9, 7);
        p.v[3 * 9 + 4] = 1.0;
        let d = distance_to_shape(&p);
        assert_eq!(d[3 * 9 + 4], 0.0);
        assert!((d[0] - (16.0f32 + 9.0).sqrt()).abs() < 1e-4);
        assert!((d[3 * 9 + 8] - 4.0).abs() < 1e-4);
    }

    #[test]
    fn missing_fonts_fall_back() {
        let spec = TitleSpec { font: FontChoice { path: Some("/nope/none.ttf".into()), index: 0, name: "Gone".into() }, ..plain("Hi") };
        let r = render_title(&spec, [200, 100]);
        assert!(r.missing_font && r.bounds.is_some());
    }

    #[test]
    fn finds_installed_fonts() {
        let fonts = system_fonts();
        eprintln!("{} fonts; default {:?}", fonts.len(), default_title_font(&fonts).name);
        if let Some(f) = fonts.first() {
            let r = render_title(&TitleSpec { font: f.choice(), ..plain("Hi") }, [200, 100]);
            assert!(!r.missing_font);
        }
    }
}
