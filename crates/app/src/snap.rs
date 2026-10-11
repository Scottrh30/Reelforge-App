//! Snapping a layer being dragged in the Viewer: its left edge, middle and
//! right edge (and top, middle, bottom) catch on the frame's edges and
//! centre lines and on other layers' edges and middles, with guide lines
//! showing what lined up. Hold Alt while dragging to place it freely.

/// An axis-aligned box in frame pixels: `[x0, y0, x1, y1]`.
pub type Bounds = [f32; 4];

/// The bounding box of some frame points.
pub fn bounds(points: &[[f32; 2]]) -> Bounds {
    let mut b = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
    for p in points {
        b = [b[0].min(p[0]), b[1].min(p[1]), b[2].max(p[0]), b[3].max(p[1])];
    }
    b
}

/// The lines things can snap to: x positions (vertical lines) and y
/// positions (horizontal lines).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Targets {
    pub xs: Vec<f32>,
    pub ys: Vec<f32>,
}

impl Targets {
    /// The frame's edges and centre lines, plus the edges and middles of `others`.
    pub fn new(frame: [f32; 2], others: &[Bounds]) -> Self {
        let mut t = Targets { xs: vec![0.0, frame[0] / 2.0, frame[0]], ys: vec![0.0, frame[1] / 2.0, frame[1]] };
        for b in others {
            t.xs.extend([b[0], (b[0] + b[2]) / 2.0, b[2]]);
            t.ys.extend([b[1], (b[1] + b[3]) / 2.0, b[3]]);
        }
        t
    }
}

/// How far to move a dragged box so it lines up, and the guide lines to draw.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Snapped {
    pub offset: [f32; 2],
    /// Vertical guides (x) and horizontal guides (y) that the box now touches.
    pub guides_x: Vec<f32>,
    pub guides_y: Vec<f32>,
}

/// The closest snap within `tolerance` (frame pixels) on one axis: the box's
/// start, middle and end against every target line.
fn axis(lo: f32, hi: f32, lines: &[f32], tolerance: f32) -> Option<f32> {
    let features = [lo, (lo + hi) / 2.0, hi];
    let mut best: Option<f32> = None;
    for f in features {
        for &l in lines {
            let d = l - f;
            if d.abs() <= tolerance && best.is_none_or(|b| d.abs() < b.abs()) {
                best = Some(d);
            }
        }
    }
    best
}

/// Snap box `b` (where the drag would put it) to `targets`.
pub fn snap(b: Bounds, targets: &Targets, tolerance: f32) -> Snapped {
    let dx = axis(b[0], b[2], &targets.xs, tolerance).unwrap_or(0.0);
    let dy = axis(b[1], b[3], &targets.ys, tolerance).unwrap_or(0.0);
    let moved = [b[0] + dx, b[1] + dy, b[2] + dx, b[3] + dy];
    // Every line the snapped box now sits on (several can line up at once).
    let on = |lo: f32, hi: f32, lines: &[f32]| {
        let mut v: Vec<f32> = lines.iter().copied().filter(|l| [lo, (lo + hi) / 2.0, hi].iter().any(|f| (f - l).abs() < 0.01)).collect();
        v.sort_by(f32::total_cmp);
        v.dedup_by(|a, b| (*a - *b).abs() < 0.01);
        v
    };
    Snapped {
        offset: [dx, dy],
        guides_x: on(moved[0], moved[2], &targets.xs),
        guides_y: on(moved[1], moved[3], &targets.ys),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snaps_to_the_frame_centre_and_edges() {
        let t = Targets::new([1920.0, 1080.0], &[]);
        // A 200×100 box whose middle is 5 px left of the frame's centre line.
        let s = snap([855.0, 300.0, 1055.0, 400.0], &t, 8.0);
        assert_eq!(s.offset, [5.0, 0.0]);
        assert_eq!(s.guides_x, vec![960.0]);
        assert!(s.guides_y.is_empty(), "nothing near vertically");
        // Its left edge near the frame's left edge, its bottom near the bottom.
        let s = snap([-6.0, 975.0, 194.0, 1075.0], &t, 8.0);
        assert_eq!(s.offset, [6.0, 5.0]);
        assert_eq!((s.guides_x.clone(), s.guides_y.clone()), (vec![0.0], vec![1080.0]));
        // Too far: no snap.
        assert_eq!(snap([100.0, 100.0, 300.0, 200.0], &t, 8.0).offset, [0.0, 0.0]);
    }

    #[test]
    fn snaps_to_other_layers_and_picks_the_closest() {
        let other = [1000.0, 200.0, 1400.0, 500.0];
        let t = Targets::new([1920.0, 1080.0], &[other]);
        // Left edge 3 px past the other layer's right edge, top 7 px below its top:
        // both snap, the nearest line on each axis.
        let s = snap([1403.0, 207.0, 1603.0, 307.0], &t, 8.0);
        assert_eq!(s.offset, [-3.0, -7.0]);
        assert_eq!(s.guides_x, vec![1400.0]);
        assert_eq!(s.guides_y, vec![200.0]);
        assert_eq!(bounds(&[[3.0, 9.0], [-1.0, 4.0], [7.0, 2.0]]), [-1.0, 2.0, 7.0, 9.0]);
    }
}
