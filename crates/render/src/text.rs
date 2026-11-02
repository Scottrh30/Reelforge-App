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
    let mut cache = CACHE.get_or_init(Default::default).lock().unwrap();
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
}

/// Lay out `spec`'s text with `font`: lines centred as a block in the frame,
/// each aligned within it, with kerning, letter and line spacing.
pub(crate) fn layout(spec: &TitleSpec, font: &FontArc, frame: [u32; 2]) -> Layout {
    let scale = PxScale::from(spec.size.max(1.0));
    let sf = font.as_scaled(scale);
    let (ascent, descent) = (sf.ascent(), sf.descent());
    let line_h = (ascent - descent + sf.line_gap()) * spec.line_spacing.max(0.1);
    // Glyphs per line at baseline 0, and each line's width.
    let lines: Vec<(Vec<ab_glyph::Glyph>, f32)> = spec
        .text
        .split('\n')
        .map(|line| {
            let mut caret = 0.0f32;
            let mut prev = None;
            let mut glyphs = Vec::new();
            for ch in line.trim_end_matches('\r').chars() {
                let id = font.glyph_id(ch);
                if let Some(p) = prev {
                    caret += sf.kern(p, id);
                }
                glyphs.push(id.with_scale_and_position(scale, point(caret, 0.0)));
                caret += sf.h_advance(id) + spec.letter_spacing;
                prev = Some(id);
            }
            let width = if glyphs.is_empty() { 0.0 } else { caret - spec.letter_spacing };
            (glyphs, width)
        })
        .collect();
    let (w, h) = (frame[0].max(1) as f32, frame[1].max(1) as f32);
    let block_w = lines.iter().map(|(_, lw)| *lw).fold(0.0, f32::max);
    let block_h = (lines.len().saturating_sub(1)) as f32 * line_h + (ascent - descent);
    let (cx, cy) = (w / 2.0, h / 2.0);
    let top = cy - block_h / 2.0;
    let text_box = [cx - block_w / 2.0, top, cx + block_w / 2.0, top + block_h];
    let mut glyphs = Vec::new();
    for (i, (line, lw)) in lines.into_iter().enumerate() {
        let baseline = top + ascent + i as f32 * line_h;
        let x0 = match spec.align {
            TextAlign::Left => text_box[0],
            TextAlign::Center => cx - lw / 2.0,
            TextAlign::Right => text_box[2] - lw,
        };
        for mut g in line {
            g.position = point(g.position.x + x0, baseline);
            glyphs.push(g);
        }
    }
    Layout { glyphs, text_box }
}

pub fn render_title(spec: &TitleSpec, frame: [u32; 2]) -> RenderedTitle {
    let (w, h) = (frame[0].max(1), frame[1].max(1));
    let (font, missing_font) = load_font(&spec.font);
    let lay = layout(spec, &font, [w, h]);
    draw(spec, &font, &lay.glyphs, lay.text_box, spec.background, [w, h], missing_font)
}

/// `spec` at `t` seconds into a clip `dur` long: with a text animation, its
/// letters, words or lines each where and as visible as the animation has
/// them then (the box behind stays put). Without one, [`render_title`].
pub fn render_title_at(spec: &TitleSpec, frame: [u32; 2], t: f32, dur: f32) -> RenderedTitle {
    let Some(states) = spec.glyph_states(t, dur) else { return render_title(spec, frame) };
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
    let [w, h] = frame;
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    let font = font.clone();
    let has_text = glyphs.iter().any(|g| font.outline_glyph(g.clone()).is_some());
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

    fn px(r: &RenderedTitle, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * r.width + x) * 4) as usize;
        [r.rgba[i], r.rgba[i + 1], r.rgba[i + 2], r.rgba[i + 3]]
    }

    fn plain(text: &str) -> TitleSpec {
        TitleSpec { text: text.into(), size: 60.0, shadow: false, ..TitleSpec::default() }
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
