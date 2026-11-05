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
use reelforge_core::TitleSpec;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

/// One mesh vertex: position, normal, and which part it's on.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Vertex {
    pub pos: [f32; 3],
    pub normal: [f32; 3],
    /// 0 front, 1 side, 2 back.
    pub part: f32,
}

pub const PART_FRONT: f32 = 0.0;
pub const PART_SIDE: f32 = 1.0;
pub const PART_BACK: f32 = 2.0;

/// Extruded text, ready to draw as a triangle list.
pub struct TextMesh {
    pub vertices: Vec<Vertex>,
    pub indices: Vec<u32>,
    /// Identifies the mesh (the title's content hash).
    pub id: u64,
    /// The letters' extent across: min X, min Y, max X, max Y (0 if none).
    pub bounds: [f32; 4],
    /// How deep the extrusion goes (Z from 0 to this).
    pub depth: f32,
}

/// Curves are cut into straight pieces at most this far (canvas pixels) from the curve.
const TOLERANCE: f32 = 0.1;
/// Neighbouring side faces meeting at less than this angle are shaded as one smooth surface.
const SMOOTH_ANGLE_DEG: f32 = 40.0;

/// The mesh for `spec` on a `frame`-sized canvas (cached).
pub fn text_mesh(spec: &TitleSpec, frame: [u32; 2]) -> Arc<TextMesh> {
    type Cache = Mutex<HashMap<u64, Arc<TextMesh>>>;
    static CACHE: OnceLock<Cache> = OnceLock::new();
    let id = spec.content_hash(frame);
    let cache = CACHE.get_or_init(Default::default);
    if let Some(m) = cache.lock().unwrap().get(&id) {
        return m.clone();
    }
    let mesh = Arc::new(build(spec, frame, id));
    let mut c = cache.lock().unwrap();
    if c.len() >= 32 {
        c.clear();
    }
    c.insert(id, mesh.clone());
    mesh
}

type P2 = [f32; 2];

/// Each glyph's outline as closed polygons in canvas pixels.
fn glyph_polygons(spec: &TitleSpec, frame: [u32; 2]) -> Vec<Vec<Vec<P2>>> {
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
                    let n = ((dd / (8.0 * TOLERANCE)).sqrt().ceil() as usize).clamp(1, 64);
                    (a, (1..=n).map(|i| quad(a, b, c, i as f32 / n as f32)).collect())
                }
                OutlineCurve::Cubic(a, b, c, d) => {
                    let (a, b, c, d) = (px(a), px(b), px(c), px(d));
                    let d1 = len([a[0] - 2.0 * b[0] + c[0], a[1] - 2.0 * b[1] + c[1]]);
                    let d2 = len([b[0] - 2.0 * c[0] + d[0], b[1] - 2.0 * c[1] + d[1]]);
                    let n = ((0.75 * d1.max(d2) / TOLERANCE).sqrt().ceil() as usize).clamp(1, 64);
                    (a, (1..=n).map(|i| cubic(a, b, c, d, i as f32 / n as f32)).collect())
                }
            };
            let joined = last.is_some_and(|l| (l[0] - start[0]).abs() < 1e-3 && (l[1] - start[1]).abs() < 1e-3);
            if !joined {
                contours.push(vec![start]);
            }
            let cur = contours.last_mut().unwrap();
            cur.extend(pts);
            last = cur.last().copied();
        }
        let polys: Vec<Vec<P2>> = contours.into_iter().map(clean).filter(|c| c.len() >= 3).collect();
        if !polys.is_empty() {
            out.push(polys);
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

fn build(spec: &TitleSpec, frame: [u32; 2], id: u64) -> TextMesh {
    let (cx, cy) = (frame[0].max(1) as f32 / 2.0, frame[1].max(1) as f32 / 2.0);
    let depth = spec.extrude.max(0.0);
    let mut vertices = Vec::new();
    let mut indices: Vec<u32> = Vec::new();
    let mut tess = FillTessellator::new();
    for polys in glyph_polygons(spec, frame) {
        // ---- Front and back faces ----
        let mut b = Path::builder();
        for c in &polys {
            b.begin(point(c[0][0], c[0][1]));
            for p in &c[1..] {
                b.line_to(point(p[0], p[1]));
            }
            b.end(true);
        }
        let path = b.build();
        let mut buf: VertexBuffers<P2, u32> = VertexBuffers::new();
        let ok = tess.tessellate_path(&path, &FillOptions::non_zero().with_tolerance(TOLERANCE), &mut BuffersBuilder::new(&mut buf, |v: FillVertex| v.position().to_array()));
        if ok.is_ok() {
            for (z, normal, part) in [(0.0, [0.0, 0.0, -1.0], PART_FRONT), (depth, [0.0, 0.0, 1.0], PART_BACK)] {
                let base = vertices.len() as u32;
                vertices.extend(buf.vertices.iter().map(|p| Vertex { pos: [p[0] - cx, p[1] - cy, z], normal, part }));
                indices.extend(buf.indices.iter().map(|i| base + i));
            }
        }
        if depth <= 0.0 {
            continue;
        }
        // ---- Sides ----
        let size = spec.size.max(1.0);
        let cos_smooth = SMOOTH_ANGLE_DEG.to_radians().cos();
        for c in &polys {
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
            let sign = if winding(&polys, probe) != 0 { -1.0 } else { 1.0 };
            let normals: Vec<P2> = (0..n)
                .map(|i| {
                    let e = [c[(i + 1) % n][0] - c[i][0], c[(i + 1) % n][1] - c[i][1]];
                    let l = len(e).max(1e-9);
                    [sign * e[1] / l, -sign * e[0] / l]
                })
                .collect();
            // A vertex's shared normal, if the edges either side meet smoothly.
            let smooth = |i: usize| -> Option<P2> {
                let (p, q) = (normals[(i + n - 1) % n], normals[i]);
                (p[0] * q[0] + p[1] * q[1] > cos_smooth).then(|| {
                    let s = [p[0] + q[0], p[1] + q[1]];
                    let l = len(s).max(1e-9);
                    [s[0] / l, s[1] / l]
                })
            };
            let shared: Vec<Option<P2>> = (0..n).map(smooth).collect();
            for i in 0..n {
                let j = (i + 1) % n;
                let ne = normals[i];
                let na = shared[i].unwrap_or(ne);
                let nb = shared[j].unwrap_or(ne);
                let base = vertices.len() as u32;
                let v = |p: P2, z: f32, nn: P2| Vertex { pos: [p[0] - cx, p[1] - cy, z], normal: [nn[0], nn[1], 0.0], part: PART_SIDE };
                vertices.extend([v(c[i], 0.0, na), v(c[j], 0.0, nb), v(c[j], depth, nb), v(c[i], depth, na)]);
                indices.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
            }
        }
    }
    let bounds = if vertices.is_empty() {
        [0.0; 4]
    } else {
        vertices.iter().fold([f32::MAX, f32::MAX, f32::MIN, f32::MIN], |b, v| [b[0].min(v.pos[0]), b[1].min(v.pos[1]), b[2].max(v.pos[0]), b[3].max(v.pos[1])])
    };
    TextMesh { vertices, indices, id, bounds, depth }
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
        assert!(!m.indices.is_empty() && m.indices.len() % 3 == 0);
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
