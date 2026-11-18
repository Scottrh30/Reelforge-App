//! 3D text: a title's letters extruded into a solid mesh.
//!
//! The glyph outlines (from the font, the same layout as the flat title) are
//! flattened into polygons. The front face is those polygons filled
//! (triangulated with the font's non-zero rule, so holes and overlapping
//! contours come out right), the back face is the same set back by the
//! extrusion depth, and the sides are a wall of quads along every contour.
//! Curves get smooth side normals, corners sharp ones.
//!
//! Coordinates are pixels of the title's frame-sized canvas measured from its
//! centre (X right, Y down), with Z going back from the front face at 0, so
//! the front face lands exactly where the flat title's letters would.

use crate::text::{layout, load_font};
use ab_glyph::{Font, OutlineCurve, ScaleFont};
use lyon_tessellation::geom::point;
use lyon_tessellation::path::Path;
use lyon_tessellation::{BuffersBuilder, FillOptions, FillTessellator, FillVertex, VertexBuffers};
use reelforge_core::{BevelStyle, TitleSpec};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

/// One mesh vertex: position, normal, and which part it's on.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Vertex {
    pub pos: [f32; 3],
    pub normal: [f32; 3],
    /// 0 front, 1 side, 2 back, 3 its own colour (a 3D model's).
    pub part: f32,
    /// Linear colour, for part 3.
    pub color: [f32; 4],
    /// A model's texture coordinates, and where its texture is in the mesh's
    /// atlas (x, y, width, height; width 0: no texture).
    pub uv: [f32; 2],
    pub rect: [f32; 4],
}

pub const PART_FRONT: f32 = 0.0;
pub const PART_SIDE: f32 = 1.0;
pub const PART_BACK: f32 = 2.0;
pub const PART_OWN: f32 = 3.0;

/// Extruded text, ready to draw as a triangle list.
pub struct TextMesh {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
    /// Identifies the mesh (the title's content hash).
    pub id: u64,
    /// The letters' extent across: min X, min Y, max X, max Y (0 if none).
    pub bounds: [f32; 4],
    /// How deep the extrusion goes (Z from 0 to this; a model's from −depth/2 to depth/2).
    pub depth: f32,
    /// The Z halfway through it (where it casts its shadow from, and where glows sit).
    pub mid_z: f32,
    /// A model's textures (see `model3d::Atlas`).
    pub atlas: Option<Arc<crate::model3d::Atlas>>,
}

/// Curves are cut into straight pieces at most this far (canvas pixels) from the curve.
const TOLERANCE: f32 = 0.1;
/// Neighbouring side faces meeting at less than this angle are shaded as one smooth surface.
const SMOOTH_ANGLE_DEG: f32 = 40.0;
/// The deepest bevel, as a fraction of the letters' thinnest stroke (each side
/// is cut back, so more would cross over).
pub const MAX_BEVEL: f32 = 0.4;
/// A corner moved in by a bevel goes at most this many times as far (sharp spikes).
const MITER_LIMIT: f32 = 3.0;

/// The mesh for `spec` on a `frame`-sized canvas (cached).
pub fn text_mesh(spec: &TitleSpec, frame: [u32; 2]) -> Arc<TextMesh> {
    type Cache = Mutex<HashMap<u64, Arc<TextMesh>>>;
    static CACHE: OnceLock<Cache> = OnceLock::new();
    let id = spec.content_hash(frame);
    let cache = CACHE.get_or_init(Default::default);
    if let Some(m) = cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(&id) {
        return m.clone();
    }
    let mesh = Arc::new(build(spec, frame, id));
    let mut c = cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if c.len() >= 32 {
        c.clear();
    }
    c.insert(id, mesh.clone());
    mesh
}

type P2 = [f32; 2];

/// Each glyph's outline (or each path of a shape) as closed polygons in
/// canvas pixels, and whether it's filled by the even-odd rule.
fn glyph_polygons(spec: &TitleSpec, frame: [u32; 2]) -> Vec<(Vec<Vec<P2>>, bool)> {
    glyph_polygons_within(spec, frame, TOLERANCE)
}

/// Every outline of a title's letters (or its shape's paths), in canvas
/// pixels, curves cut to within half a pixel: what Neon path follows when
/// its Path is the letters. Cached.
pub fn letter_outlines(spec: &TitleSpec, frame: [u32; 2]) -> Vec<Vec<[f32; 2]>> {
    type Cache = Mutex<HashMap<u64, Arc<Vec<Vec<P2>>>>>;
    static CACHE: OnceLock<Cache> = OnceLock::new();
    let id = spec.content_hash(frame);
    let cache = CACHE.get_or_init(Default::default);
    if let Some(l) = cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(&id) {
        return l.as_ref().clone();
    }
    let lines: Arc<Vec<Vec<P2>>> = Arc::new(glyph_polygons_within(spec, frame, 0.5).into_iter().flat_map(|(polys, _)| polys).collect());
    let mut c = cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if c.len() >= 32 {
        c.clear();
    }
    c.insert(id, lines.clone());
    lines.as_ref().clone()
}

fn glyph_polygons_within(spec: &TitleSpec, frame: [u32; 2], tolerance: f32) -> Vec<(Vec<Vec<P2>>, bool)> {
    if let Some(shape) = &spec.shape {
        let c = [frame[0].max(1) as f32 / 2.0, frame[1].max(1) as f32 / 2.0];
        return shape.placed(c, spec.size.max(1.0)).into_iter().map(|p| (p.contours.into_iter().map(clean).filter(|c| c.len() >= 3).collect::<Vec<_>>(), p.even_odd)).filter(|p| !p.0.is_empty()).collect();
    }
    let (font, _) = load_font(&spec.font);
    let lay = layout(spec, &font, frame);
    let mut out = Vec::new();
    for g in &lay.glyphs {
        let Some(outline) = font.outline(g.id) else { continue };
        let sf = font.as_scaled(g.scale);
        let (hs, vs) = (sf.h_scale_factor(), sf.v_scale_factor());
        let (gx, gy) = (g.position.x, g.position.y);
        // Font units (Y up) → canvas pixels (Y down).
        let px = |p: ab_glyph::Point| -> P2 { [gx + p.x * hs, gy - p.y * vs] };
        let mut contours: Vec<Vec<P2>> = Vec::new();
        let mut last: Option<P2> = None;
        for c in &outline.curves {
            let (start, pts): (P2, Vec<P2>) = match *c {
                OutlineCurve::Line(a, b) => (px(a), vec![px(b)]),
                OutlineCurve::Quad(a, b, c) => {
                    let (a, b, c) = (px(a), px(b), px(c));
                    let dd = len([a[0] - 2.0 * b[0] + c[0], a[1] - 2.0 * b[1] + c[1]]);
                    let n = ((dd / (8.0 * tolerance)).sqrt().ceil() as usize).clamp(1, 64);
                    (a, (1..=n).map(|i| quad(a, b, c, i as f32 / n as f32)).collect())
                }
                OutlineCurve::Cubic(a, b, c, d) => {
                    let (a, b, c, d) = (px(a), px(b), px(c), px(d));
                    let d1 = len([a[0] - 2.0 * b[0] + c[0], a[1] - 2.0 * b[1] + c[1]]);
                    let d2 = len([b[0] - 2.0 * c[0] + d[0], b[1] - 2.0 * c[1] + d[1]]);
                    let n = ((0.75 * d1.max(d2) / tolerance).sqrt().ceil() as usize).clamp(1, 64);
                    (a, (1..=n).map(|i| cubic(a, b, c, d, i as f32 / n as f32)).collect())
                }
            };
            let joined = last.is_some_and(|l| (l[0] - start[0]).abs() < 1e-3 && (l[1] - start[1]).abs() < 1e-3);
            if !joined {
                contours.push(vec![start]);
            }
            let Some(cur) = contours.last_mut() else { continue };
            cur.extend(pts);
            last = cur.last().copied();
        }
        let polys: Vec<Vec<P2>> = contours.into_iter().map(clean).filter(|c| c.len() >= 3).collect();
        if !polys.is_empty() {
            out.push((polys, false));
        }
    }
    out
}

fn len(v: P2) -> f32 {
    (v[0] * v[0] + v[1] * v[1]).sqrt()
}
fn quad(a: P2, b: P2, c: P2, t: f32) -> P2 {
    let u = 1.0 - t;
    [u * u * a[0] + 2.0 * u * t * b[0] + t * t * c[0], u * u * a[1] + 2.0 * u * t * b[1] + t * t * c[1]]
}
fn cubic(a: P2, b: P2, c: P2, d: P2, t: f32) -> P2 {
    let u = 1.0 - t;
    let (k0, k1, k2, k3) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
    [k0 * a[0] + k1 * b[0] + k2 * c[0] + k3 * d[0], k0 * a[1] + k1 * b[1] + k2 * c[1] + k3 * d[1]]
}

/// Drop repeated points (and the closing repeat of the first).
fn clean(c: Vec<P2>) -> Vec<P2> {
    let mut out: Vec<P2> = Vec::with_capacity(c.len());
    for p in c {
        if out.last().is_none_or(|q| len([p[0] - q[0], p[1] - q[1]]) > 1e-4) {
            out.push(p);
        }
    }
    while out.len() > 1 && len([out[0][0] - out[out.len() - 1][0], out[0][1] - out[out.len() - 1][1]]) <= 1e-4 {
        out.pop();
    }
    out
}

/// Whether `p` is inside `polys` by the fill rule.
fn inside(polys: &[Vec<P2>], p: P2, even_odd: bool) -> bool {
    let w = winding(polys, p);
    if even_odd { w % 2 != 0 } else { w != 0 }
}

/// Non-zero winding number of `p` against closed polygons `polys`.
fn winding(polys: &[Vec<P2>], p: P2) -> i32 {
    let mut w = 0;
    for c in polys {
        for i in 0..c.len() {
            let (a, b) = (c[i], c[(i + 1) % c.len()]);
            let cross = (b[0] - a[0]) * (p[1] - a[1]) - (p[0] - a[0]) * (b[1] - a[1]);
            if a[1] <= p[1] {
                if b[1] > p[1] && cross > 0.0 {
                    w += 1;
                }
            } else if b[1] <= p[1] && cross < 0.0 {
                w -= 1;
            }
        }
    }
    w
}

/// The bevel's profile from the front face to the side: (how far in from the
/// outline, how far back, and the normal's (outward, along Z) parts), for a
/// bevel `b` deep.
fn bevel_profile(style: BevelStyle, b: f32) -> Vec<(f32, f32, [f32; 2])> {
    const STEPS: usize = 5;
    let quarter = |f: &dyn Fn(f32) -> (f32, f32, [f32; 2])| (0..=STEPS).map(|k| f(std::f32::consts::FRAC_PI_2 * k as f32 / STEPS as f32)).collect();
    match style {
        // A straight cut at 45°: one band, its normal halfway.
        BevelStyle::Flat => {
            let n = [std::f32::consts::FRAC_1_SQRT_2, -std::f32::consts::FRAC_1_SQRT_2];
            vec![(b, 0.0, n), (0.0, b, n)]
        }
        // Bulging out: from facing front to facing out.
        BevelStyle::Round => quarter(&|t| (b * (1.0 - t.sin()), b * (1.0 - t.cos()), [t.sin(), -t.cos()])),
        // Hollowed in: a quarter circle round the old corner.
        BevelStyle::Cove => quarter(&|t| (b * t.cos(), b * t.sin(), [t.cos(), -t.sin()])),
    }
}

/// A glyph contour with its outward edge normals and how its corners move
/// when the outline is moved in.
struct Contour {
    pts: Vec<P2>,
    normals: Vec<P2>,
    /// Per corner: the shared normal if its edges meet smoothly.
    shared: Vec<Option<P2>>,
    /// Per corner: how a point moves for each pixel the outline moves outwards.
    miter: Vec<P2>,
}

fn contours_of(polys: &[Vec<P2>], size: f32, even_odd: bool) -> Vec<Contour> {
    let cos_smooth = SMOOTH_ANGLE_DEG.to_radians().cos();
    polys
            .iter()
            .map(|c| {
                let n = c.len();
                // Which way is out: test a point just to the left of the longest edge.
                let (i, _) = (0..n).map(|i| (i, len([c[(i + 1) % n][0] - c[i][0], c[(i + 1) % n][1] - c[i][1]]))).fold((0, -1.0), |m, e| if e.1 > m.1 { e } else { m });
                let (a, bb) = (c[i], c[(i + 1) % n]);
                let e = [bb[0] - a[0], bb[1] - a[1]];
                let l = len(e).max(1e-6);
                let left = [e[1] / l, -e[0] / l];
                let eps = size * 1e-3;
                let probe = [(a[0] + bb[0]) * 0.5 + left[0] * eps, (a[1] + bb[1]) * 0.5 + left[1] * eps];
                // Inside to the left: out is to the right.
                let sign = if inside(polys, probe, even_odd) { -1.0 } else { 1.0 };
                let normals: Vec<P2> = (0..n)
                    .map(|i| {
                        let e = [c[(i + 1) % n][0] - c[i][0], c[(i + 1) % n][1] - c[i][1]];
                        let l = len(e).max(1e-9);
                        [sign * e[1] / l, -sign * e[0] / l]
                    })
                    .collect();
                let shared = (0..n)
                    .map(|i| {
                        let (p, q) = (normals[(i + n - 1) % n], normals[i]);
                        (p[0] * q[0] + p[1] * q[1] > cos_smooth).then(|| {
                            let s = [p[0] + q[0], p[1] + q[1]];
                            let l = len(s).max(1e-9);
                            [s[0] / l, s[1] / l]
                        })
                    })
                    .collect();
                let miter = (0..n)
                    .map(|i| {
                        let (p, q) = (normals[(i + n - 1) % n], normals[i]);
                        let d = 1.0 + p[0] * q[0] + p[1] * q[1];
                        let m = [(p[0] + q[0]) / d.max(1e-3), (p[1] + q[1]) / d.max(1e-3)];
                        // Very sharp corners: no further than this many times the distance.
                        let l = len(m);
                        if l > MITER_LIMIT { [m[0] / l * MITER_LIMIT, m[1] / l * MITER_LIMIT] } else { m }
                    })
                    .collect();
                Contour { pts: c.clone(), normals, shared, miter }
            })
            .collect()
}

/// The thinnest part of the letters: the shortest way across, from each
/// corner straight inwards to the far side (within its glyph).
fn thinnest(glyphs: &[(bool, Vec<Contour>)]) -> f32 {
    let mut best = f32::MAX;
    for (_, cs) in glyphs {
        for c in cs {
            let n = c.pts.len();
            for i in 0..n {
                let m = c.shared[i].unwrap_or(c.normals[i]);
                let (o, d) = (c.pts[i], [-m[0], -m[1]]);
                for e in cs {
                    let k = e.pts.len();
                    for a in 0..k {
                        let (p, q) = (e.pts[a], e.pts[(a + 1) % k]);
                        // o + d t = p + (q − p) u
                        let r = [q[0] - p[0], q[1] - p[1]];
                        let den = d[0] * r[1] - d[1] * r[0];
                        if den.abs() < 1e-9 {
                            continue;
                        }
                        let w = [p[0] - o[0], p[1] - o[1]];
                        let t = (w[0] * r[1] - w[1] * r[0]) / den;
                        let u = (w[0] * d[1] - w[1] * d[0]) / den;
                        if t > 1e-2 && (0.0..=1.0).contains(&u) {
                            best = best.min(t);
                        }
                    }
                }
            }
        }
    }
    best
}

/// A 3D model as a mesh like 3D text's: its largest side `size` pixels,
/// centred every way (so it turns about its middle) (the file's +Y up, or +Z, and
/// facing the viewer along its +Z, or −Y), in its own colours or the face colour.
fn model_mesh(spec: &TitleSpec, src: &reelforge_core::ModelSource, id: u64) -> TextMesh {
    let empty = TextMesh { vertices: Vec::new(), indices: Vec::new(), id, bounds: [0.0; 4], depth: 0.0, mid_z: 0.0, atlas: None };
    let Ok(m) = crate::model3d::cached(src) else { return empty };
    // File axes (right-handed, Y up, facing +Z; or Z up, facing −Y) → ours
    // (X right, Y down, Z away): turns, never mirrors.
    let axes = |p: [f32; 3]| -> [f32; 3] { if src.z_up { [p[0], -p[2], p[1]] } else { [p[0], -p[1], -p[2]] } };
    let pts: Vec<[f32; 3]> = m.positions.iter().map(|&p| axes(p)).collect();
    let (lo, hi) = pts.iter().fold(([f32::MAX; 3], [f32::MIN; 3]), |(l, h), p| ([l[0].min(p[0]), l[1].min(p[1]), l[2].min(p[2])], [h[0].max(p[0]), h[1].max(p[1]), h[2].max(p[2])]));
    let ext = [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]];
    // `size` is its largest side (so it fits in a box that big).
    let tall = ext[0].max(ext[1]).max(ext[2]);
    if !(tall > 0.0 && tall.is_finite()) {
        return empty;
    }
    let k = spec.size.max(1.0) / tall;
    // Centred every way, so it turns about its middle.
    let c = [(lo[0] + hi[0]) * 0.5, (lo[1] + hi[1]) * 0.5, (lo[2] + hi[2]) * 0.5];
    let own = src.own_colours;
    // Parts given a colour of their own (linear), by part number.
    let lin = |v: u8| {
        let c = v as f32 / 255.0;
        if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    };
    let overrides: Vec<Option<[f32; 4]>> = m.parts.iter().map(|name| src.part_colours.iter().find(|(p, _)| p == name).map(|(_, c)| [lin(c[0]), lin(c[1]), lin(c[2]), 1.0])).collect();
    let rects = m.atlas.as_ref().map(|a| a.rects.as_slice()).unwrap_or(&[]);
    let vertices: Vec<Vertex> = pts
        .iter()
        .enumerate()
        .zip(&m.normals)
        .zip(&m.colors)
        .map(|(((i, p), n), col)| {
            let x = m.extra.get(i).copied().unwrap_or_default();
            let over = overrides.get(x.part as usize).copied().flatten();
            let rect = if over.is_none() && x.tex >= 0 { rects.get(x.tex as usize).copied().unwrap_or([0.0; 4]) } else { [0.0; 4] };
            Vertex { pos: [(p[0] - c[0]) * k, (p[1] - c[1]) * k, (p[2] - c[2]) * k], normal: axes(*n), part: if own { PART_OWN } else { PART_FRONT }, color: over.unwrap_or(*col), uv: x.uv, rect }
        })
        .collect();
    let bounds = [(lo[0] - c[0]) * k, (lo[1] - c[1]) * k, (hi[0] - c[0]) * k, (hi[1] - c[1]) * k];
    let atlas = if own { m.atlas.clone() } else { None };
    TextMesh { vertices, indices: m.indices.clone(), id, bounds, depth: ext[2] * k, mid_z: 0.0, atlas }
}

fn build(spec: &TitleSpec, frame: [u32; 2], id: u64) -> TextMesh {
    if let Some(src) = &spec.model {
        return model_mesh(spec, src, id);
    }
    let (cx, cy) = (frame[0].max(1) as f32 / 2.0, frame[1].max(1) as f32 / 2.0);
    let depth = spec.extrude.max(0.0);
    let size = spec.size.max(1.0);
    let glyphs: Vec<(bool, Vec<Contour>)> = glyph_polygons(spec, frame).into_iter().map(|(p, eo)| (eo, contours_of(&p, size, eo))).collect();
    // Within half the depth, and not crossing over in the thinnest strokes.
    let bevel = if depth > 0.0 && spec.bevel > 0.0 {
        let thin = thinnest(&glyphs);
        spec.bevel.clamp(0.0, (depth * 0.5).min(thin * MAX_BEVEL))
    } else {
        0.0
    };
    let profile = if bevel > 0.0 { bevel_profile(spec.bevel_style, bevel) } else { Vec::new() };
    let mut vertices = Vec::new();
    let mut indices: Vec<u32> = Vec::new();
    let mut tess = FillTessellator::new();
    for (even_odd, contours) in &glyphs {
        // The outline moved in by `d`.
        let inset = |c: &Contour, i: usize, d: f32| -> P2 { [c.pts[i][0] - c.miter[i][0] * d, c.pts[i][1] - c.miter[i][1] * d] };

        // ---- Front and back faces (inside the bevel) ----
        let mut b = Path::builder();
        for c in contours {
            b.begin(point(inset(c, 0, bevel)[0], inset(c, 0, bevel)[1]));
            for i in 1..c.pts.len() {
                let p = inset(c, i, bevel);
                b.line_to(point(p[0], p[1]));
            }
            b.end(true);
        }
        let path = b.build();
        let mut buf: VertexBuffers<P2, u32> = VertexBuffers::new();
        let rule = if *even_odd { FillOptions::even_odd() } else { FillOptions::non_zero() };
        let ok = tess.tessellate_path(&path, &rule.with_tolerance(TOLERANCE), &mut BuffersBuilder::new(&mut buf, |v: FillVertex| v.position().to_array()));
        if ok.is_ok() {
            for (z, normal, part) in [(0.0, [0.0, 0.0, -1.0], PART_FRONT), (depth, [0.0, 0.0, 1.0], PART_BACK)] {
                let base = vertices.len() as u32;
                vertices.extend(buf.vertices.iter().map(|p| Vertex { pos: [p[0] - cx, p[1] - cy, z], normal, part, color: [0.0; 4], uv: [0.0; 2], rect: [0.0; 4] }));
                indices.extend(buf.indices.iter().map(|i| base + i));
            }
        }
        if depth <= 0.0 {
            continue;
        }
        // ---- Sides, and the bevels front and back ----
        for c in contours {
            let n = c.pts.len();
            for i in 0..n {
                let j = (i + 1) % n;
                let ne = c.normals[i];
                let na = c.shared[i].unwrap_or(ne);
                let nb = c.shared[j].unwrap_or(ne);
                // A strip of quads along this edge through `rings`: (inset, z, normal (out, along Z)).
                let mut strip = |rings: &[(f32, f32, [f32; 2])], part: f32| {
                    for w in rings.windows(2) {
                        let base = vertices.len() as u32;
                        let v = |k: usize, r: &(f32, f32, [f32; 2]), nn: P2| {
                            let p = inset(c, k, r.0);
                            Vertex { pos: [p[0] - cx, p[1] - cy, r.1], normal: [nn[0] * r.2[0], nn[1] * r.2[0], r.2[1]], part, color: [0.0; 4], uv: [0.0; 2], rect: [0.0; 4] }
                        };
                        vertices.extend([v(i, &w[0], na), v(j, &w[0], nb), v(j, &w[1], nb), v(i, &w[1], na)]);
                        indices.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
                    }
                };
                strip(&[(0.0, bevel, [1.0, 0.0]), (0.0, depth - bevel, [1.0, 0.0])], PART_SIDE);
                if bevel > 0.0 {
                    // In front (facing forward at the face), and its mirror at the back.
                    strip(&profile, PART_FRONT);
                    let back: Vec<(f32, f32, [f32; 2])> = profile.iter().rev().map(|&(d, z, nz)| (d, depth - z, [nz[0], -nz[1]])).collect();
                    strip(&back, PART_BACK);
                }
            }
        }
    }
    let bounds = if vertices.is_empty() {
        [0.0; 4]
    } else {
        vertices.iter().fold([f32::MAX, f32::MAX, f32::MIN, f32::MIN], |b, v| [b[0].min(v.pos[0]), b[1].min(v.pos[1]), b[2].max(v.pos[0]), b[3].max(v.pos[1])])
    };
    TextMesh { vertices, indices, id, bounds, depth, mid_z: depth * 0.5, atlas: None }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(text: &str, extrude: f32) -> TitleSpec {
        TitleSpec { text: text.into(), size: 100.0, shadow: false, extrude, ..TitleSpec::default() }
    }

    #[test]
    fn letters_become_a_closed_solid() {
        let m = text_mesh(&spec("OH", 40.0), [640, 360]);
        assert!(!m.indices.is_empty() && m.indices.len().is_multiple_of(3));
        let zs: Vec<f32> = m.vertices.iter().map(|v| v.pos[2]).collect();
        assert!(zs.iter().all(|z| *z == 0.0 || *z == 40.0));
        // Front faces face the viewer (−Z), back faces away, sides sideways.
        for v in &m.vertices {
            match v.part {
                PART_FRONT => assert_eq!(v.normal, [0.0, 0.0, -1.0]),
                PART_BACK => assert_eq!(v.normal, [0.0, 0.0, 1.0]),
                _ => assert!(v.normal[2] == 0.0 && (len([v.normal[0], v.normal[1]]) - 1.0).abs() < 1e-3),
            }
        }
        // Centred on the canvas, roughly the size of the flat text.
        let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
        for v in &m.vertices {
            for k in 0..2 {
                lo[k] = lo[k].min(v.pos[k]);
                hi[k] = hi[k].max(v.pos[k]);
            }
        }
        let flat = crate::text::render_title(&spec("OH", 0.0), [640, 360]).bounds.unwrap();
        assert!((lo[0] + 320.0 - flat[0]).abs() < 2.0 && (hi[0] + 320.0 - flat[2]).abs() < 2.0, "{lo:?} {hi:?} {flat:?}");
        assert!((lo[1] + 180.0 - flat[1]).abs() < 2.0 && (hi[1] + 180.0 - flat[3]).abs() < 2.0, "{lo:?} {hi:?} {flat:?}");
    }

/// Bevels: the faces shrink in by the bevel, the band between turns from
    /// facing front to facing out (back likewise), the letters' extent stays
    /// the same, and a bevel too deep for the strokes is held back.
    #[test]
    fn bevels_cut_back_the_edges() {
        let plain = text_mesh(&spec("OH", 40.0), [640, 360]);
        let x_extent = |m: &TextMesh, part: f32| {
            m.vertices.iter().filter(|v| v.part == part && v.pos[2] == 0.0 && v.normal == [0.0, 0.0, -1.0]).fold((f32::MAX, f32::MIN), |(lo, hi), v| (lo.min(v.pos[0]), hi.max(v.pos[0])))
        };
        let (plo, phi) = x_extent(&plain, PART_FRONT);
        for style in [BevelStyle::Round, BevelStyle::Flat, BevelStyle::Cove] {
            let m = text_mesh(&TitleSpec { bevel: 2.0, bevel_style: style, ..spec("OH", 40.0) }, [640, 360]);
            for v in &m.vertices {
                assert!((len([v.normal[0], v.normal[1]]).hypot(v.normal[2]) - 1.0).abs() < 1e-3, "{style:?}: unit normals");
                assert!((0.0..=40.0).contains(&v.pos[2]));
            }
            // The face is 2 px in from the outline each side.
            let (lo, hi) = x_extent(&m, PART_FRONT);
            assert!((lo - plo - 2.0).abs() < 0.3 && (phi - hi - 2.0).abs() < 0.3, "{style:?}: face {lo}..{hi} vs {plo}..{phi}");
            // The bevel: front-coloured faces between z 0 and 2 turning outwards, and their mirror at the back.
            let front: Vec<&Vertex> = m.vertices.iter().filter(|v| v.part == PART_FRONT && v.normal[2] > -0.999).collect();
            let back: Vec<&Vertex> = m.vertices.iter().filter(|v| v.part == PART_BACK && v.normal[2] < 0.999).collect();
            assert!(!front.is_empty() && front.len() == back.len(), "{style:?}");
            assert!(front.iter().all(|v| v.pos[2] <= 2.0 + 1e-4 && v.normal[2] < 1e-4), "{style:?}: front bevel faces forward-ish");
            assert!(back.iter().all(|v| v.pos[2] >= 38.0 - 1e-4 && v.normal[2] > -1e-4), "{style:?}: back bevel faces back-ish");
            // Sides only between the bevels.
            assert!(m.vertices.iter().filter(|v| v.part == PART_SIDE).all(|v| (2.0 - 1e-4..=38.0 + 1e-4).contains(&v.pos[2])));
            // Same extent overall.
            assert_eq!(m.bounds.map(|b| b.round()), plain.bounds.map(|b| b.round()), "{style:?}");
        }
        // Round: from facing front to facing out; a 45° band when flat.
        let round = text_mesh(&TitleSpec { bevel: 2.0, ..spec("OH", 40.0) }, [640, 360]);
        assert!(round.vertices.iter().any(|v| v.part == PART_FRONT && v.pos[2] > 0.1 && v.pos[2] < 1.9 && v.normal[2] < -0.1 && v.normal[2] > -0.99), "curving");
        let flat = text_mesh(&TitleSpec { bevel: 2.0, bevel_style: BevelStyle::Flat, ..spec("OH", 40.0) }, [640, 360]);
        assert!(flat.vertices.iter().filter(|v| v.part == PART_FRONT && v.normal[2] > -0.999).all(|v| (v.normal[2] + std::f32::consts::FRAC_1_SQRT_2).abs() < 1e-4));
        // Too deep for these strokes (about 6.4 px across): held to 0.4 of them.
        let deep = text_mesh(&TitleSpec { bevel: 30.0, ..spec("OH", 40.0) }, [640, 360]);
        let zmax = deep.vertices.iter().filter(|v| v.part == PART_FRONT).map(|v| v.pos[2]).fold(0.0, f32::max);
        assert!(zmax > 2.0 && zmax < 2.7, "{zmax}");
        // Flat text has no bevel.
        assert_eq!(text_mesh(&TitleSpec { bevel: 2.0, ..spec("OH", 0.0) }, [640, 360]).vertices.len(), text_mesh(&spec("OH", 0.0), [640, 360]).vertices.len());
    }

    #[test]
    fn side_normals_point_out_of_the_letters_and_holes() {
        // An "O": the outer wall faces away from the middle, the hole's wall towards it.
        let m = text_mesh(&spec("O", 30.0), [400, 400]);
        let sides: Vec<&Vertex> = m.vertices.iter().filter(|v| v.part == PART_SIDE).collect();
        let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
        for v in &sides {
            for k in 0..2 {
                lo[k] = lo[k].min(v.pos[k]);
                hi[k] = hi[k].max(v.pos[k]);
            }
        }
        let mid = [(lo[0] + hi[0]) / 2.0, (lo[1] + hi[1]) / 2.0];
        let r = |v: &Vertex| len([(v.pos[0] - mid[0]) / (hi[0] - lo[0]), (v.pos[1] - mid[1]) / (hi[1] - lo[1])]);
        let rmax = sides.iter().map(|v| r(v)).fold(0.0, f32::max);
        let rmin = sides.iter().map(|v| r(v)).fold(f32::MAX, f32::min);
        let split = (rmin + rmax) / 2.0;
        let (mut outer, mut inner) = (0, 0);
        for v in &sides {
            let radial = [v.pos[0] - mid[0], v.pos[1] - mid[1]];
            let d = v.normal[0] * radial[0] + v.normal[1] * radial[1];
            if r(v) > split {
                assert!(d > 0.0, "outer wall faces out: {v:?}");
                outer += 1;
            } else {
                assert!(d < 0.0, "hole wall faces in: {v:?}");
                inner += 1;
            }
        }
        assert!(outer > 20 && inner > 20, "{outer} {inner}");
    }

    #[test]
    fn flat_text_has_only_faces_and_empty_text_nothing() {
        let m = text_mesh(&spec("A", 0.0), [200, 200]);
        assert!(m.vertices.iter().all(|v| v.part != PART_SIDE));
        assert!(text_mesh(&spec("  ", 20.0), [200, 200]).indices.is_empty());
    }
}
