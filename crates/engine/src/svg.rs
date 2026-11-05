//! SVG logos as shapes for 3D (or flat) titles: every visible filled path,
//! and every stroke turned into the area it covers, with their transforms,
//! curves cut into short straight pieces.

use reelforge_core::{Shape, ShapePath};
use std::path::Path;

/// Pieces each curve is cut into.
const CURVE_STEPS: usize = 16;

/// The shape in an SVG file (named after the file).
pub fn shape_from_svg(path: &Path) -> Result<Shape, String> {
    let data = std::fs::read(path).map_err(|e| format!("couldn't read {}: {e}", path.display()))?;
    let name = path.file_stem().map_or("Logo".into(), |s| s.to_string_lossy().to_string());
    shape_from_svg_data(&data, &name)
}

/// The shape in SVG `data` (plain or gzipped).
pub fn shape_from_svg_data(data: &[u8], name: &str) -> Result<Shape, String> {
    let tree = usvg::Tree::from_data(data, &usvg::Options::default()).map_err(|e| format!("not an SVG Reelforge can read: {e}"))?;
    let mut paths = Vec::new();
    collect(tree.root(), &mut paths);
    Shape::new(name, paths).ok_or_else(|| "the SVG has no filled or stroked shapes (text has to be converted to outlines first)".into())
}

fn collect(g: &usvg::Group, out: &mut Vec<ShapePath>) {
    for n in g.children() {
        match n {
            usvg::Node::Group(g) => collect(g, out),
            usvg::Node::Path(p) if p.is_visible() => {
                let t = p.abs_transform();
                if let Some(f) = p.fill() {
                    out.push(flatten(p.data(), t, f.rule() == usvg::FillRule::EvenOdd));
                }
                if let Some(s) = p.stroke() {
                    if let Some(area) = p.data().stroke(&s.to_tiny_skia(), 1.0) {
                        out.push(flatten(&area, t, false));
                    }
                }
            }
            _ => {}
        }
    }
}

fn flatten(path: &usvg::tiny_skia_path::Path, t: usvg::Transform, even_odd: bool) -> ShapePath {
    use usvg::tiny_skia_path::PathSegment;
    let map = |p: usvg::tiny_skia_path::Point| -> [f32; 2] { [t.sx * p.x + t.kx * p.y + t.tx, t.ky * p.x + t.sy * p.y + t.ty] };
    let mut contours: Vec<Vec<[f32; 2]>> = Vec::new();
    let mut cur: Vec<[f32; 2]> = Vec::new();
    let mut last = usvg::tiny_skia_path::Point::zero();
    let mut flush = |cur: &mut Vec<[f32; 2]>| {
        if cur.len() >= 3 {
            contours.push(std::mem::take(cur));
        } else {
            cur.clear();
        }
    };
    for seg in path.segments() {
        match seg {
            PathSegment::MoveTo(p) => {
                flush(&mut cur);
                cur.push(map(p));
                last = p;
            }
            PathSegment::LineTo(p) => {
                cur.push(map(p));
                last = p;
            }
            PathSegment::QuadTo(c, p) => {
                for k in 1..=CURVE_STEPS {
                    let s = k as f32 / CURVE_STEPS as f32;
                    let u = 1.0 - s;
                    let q = usvg::tiny_skia_path::Point::from_xy(u * u * last.x + 2.0 * u * s * c.x + s * s * p.x, u * u * last.y + 2.0 * u * s * c.y + s * s * p.y);
                    cur.push(map(q));
                }
                last = p;
            }
            PathSegment::CubicTo(c1, c2, p) => {
                for k in 1..=CURVE_STEPS {
                    let s = k as f32 / CURVE_STEPS as f32;
                    let u = 1.0 - s;
                    let (a, b, c, d) = (u * u * u, 3.0 * u * u * s, 3.0 * u * s * s, s * s * s);
                    let q = usvg::tiny_skia_path::Point::from_xy(a * last.x + b * c1.x + c * c2.x + d * p.x, a * last.y + b * c1.y + c * c2.y + d * p.y);
                    cur.push(map(q));
                }
                last = p;
            }
            PathSegment::Close => flush(&mut cur),
        }
    }
    flush(&mut cur);
    ShapePath { contours, even_odd }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_filled_and_stroked_paths_with_their_transforms_and_rules() {
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="200" height="100">
            <g transform="translate(10,0)">
              <path d="M0 0 H80 V100 H0 Z M20 20 H60 V80 H20 Z" fill="red" fill-rule="evenodd"/>
              <circle cx="140" cy="50" r="40" fill="blue"/>
            </g>
            <line x1="0" y1="100" x2="200" y2="100" stroke="black" stroke-width="0"/>
            <rect x="0" y="0" width="5" height="5" fill="none" stroke="black" stroke-width="2"/>
            <text x="0" y="50">ignored</text>
        </svg>"#;
        let s = shape_from_svg_data(svg, "logo").unwrap();
        assert_eq!(s.name, "logo");
        // The square with a hole (even-odd), the circle, and the stroked little square.
        assert_eq!(s.paths.len(), 3);
        assert!(s.paths[0].even_odd && s.paths[0].contours.len() == 2);
        assert!(!s.paths[1].even_odd && s.paths[1].contours[0].len() >= 4 * CURVE_STEPS);
        // Normalised: 1 high, centred; the extent was x −1..190, y −1..100.
        let all: Vec<[f32; 2]> = s.paths.iter().flat_map(|p| p.contours.iter().flatten().copied()).collect();
        let (lo, hi) = all.iter().fold(([f32::MAX; 2], [f32::MIN; 2]), |(l, h), q| ([l[0].min(q[0]), l[1].min(q[1])], [h[0].max(q[0]), h[1].max(q[1])]));
        assert!((hi[1] - lo[1] - 1.0).abs() < 1e-4 && (lo[0] + hi[0]).abs() < 1e-4 && (lo[1] + hi[1]).abs() < 1e-4, "{lo:?} {hi:?}");
        assert!((s.aspect - 191.0 / 101.0).abs() < 0.02, "{}", s.aspect);
        // Nothing drawable: an error saying why.
        assert!(shape_from_svg_data(br#"<svg xmlns="http://www.w3.org/2000/svg"><text>hi</text></svg>"#, "x").unwrap_err().contains("outlines"));
        assert!(shape_from_svg_data(b"not svg", "x").is_err());
    }
}
