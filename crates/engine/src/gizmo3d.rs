//! The 3D gizmo for a selected 3D layer, 3D text or model: three arrows along
//! its own X (red), Y (green) and Z (blue) axes to move it along one axis,
//! and three rings to turn it about X, Y or Z, drawn as the camera sees them.
//! Dragging the layer anywhere else still moves it across the view.
//!
//! The rings turn the layer's own three turns (`orientation` X and Y, then
//! `rotation` about Z), each about the axis it turns about (see
//! `space3d::turn_axes`), so a drag changes one number in Controls.

use reelforge_core::space3d::{layer_rotation, turn_axes, Scene3d, V3};
use reelforge_core::Transform;
use serde::Serialize;

/// Arrow length, as a share of the frame's height (the gizmo keeps its size on screen).
const ARROW: f32 = 0.13;
/// Ring radius, as a share of the arrow length.
const RING: f32 = 0.78;
/// How close (frame pixels at 100%, divided by the view scale) counts as on a handle.
const GRAB: f32 = 7.0;
const RING_STEPS: usize = 48;

/// What's drawn, in frame pixels.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Gizmo3d {
    /// Where the layer's position is.
    pub origin: [f32; 2],
    /// The tips of the X, Y and Z arrows (`None`: pointing straight at the camera or behind it).
    pub tips: [Option<[f32; 2]>; 3],
    /// The X, Y and Z rings, as polylines (empty if they don't all show).
    pub rings: [Vec<[f32; 2]>; 3],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Handle3d {
    Axis(u8),
    Ring(u8),
}

fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
fn mul(a: V3, k: f32) -> V3 {
    [a[0] * k, a[1] * k, a[2] * k]
}
fn cross(a: V3, b: V3) -> V3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
fn norm(a: V3) -> V3 {
    let l = (a[0] * a[0] + a[1] * a[1] + a[2] * a[2]).sqrt();
    if l > 1e-9 { mul(a, 1.0 / l) } else { [1.0, 0.0, 0.0] }
}

/// The layer's position in the world.
fn pivot(t: &Transform) -> V3 {
    [t.position[0], t.position[1], t.depth]
}

/// How long the arrows are in the world, so they're `ARROW` of the frame on screen.
fn arrow_len(t: &Transform, scene: &Scene3d) -> Option<f32> {
    let cam = &scene.camera;
    let z = cam.view(pivot(t))[2];
    (z > reelforge_core::space3d::NEAR * 2.0).then(|| ARROW * cam.frame[1] * z / cam.focal())
}

/// The layer's own axes (unit, in the world).
fn own_axes(t: &Transform) -> [V3; 3] {
    [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]].map(|a| norm(layer_rotation(t, a)))
}

/// Two unit vectors at right angles to `a` and to each other.
fn across(a: V3) -> (V3, V3) {
    let helper = if a[0].abs() < 0.9 { [1.0, 0.0, 0.0] } else { [0.0, 1.0, 0.0] };
    let b1 = norm(cross(a, helper));
    (b1, cross(a, b1))
}

/// The gizmo for a 3D layer with transform `t` (as it shows) seen in `scene`.
pub fn gizmo3d(t: &Transform, scene: &Scene3d) -> Option<Gizmo3d> {
    let cam = &scene.camera;
    let p = pivot(t);
    let origin = cam.project(p)?;
    let len = arrow_len(t, scene)?;
    let tips = own_axes(t).map(|a| cam.project(add(p, mul(a, len))));
    let rings = turn_axes(t).map(|a| {
        let (b1, b2) = across(norm(a));
        let pts: Option<Vec<[f32; 2]>> = (0..=RING_STEPS)
            .map(|i| {
                let th = i as f32 / RING_STEPS as f32 * std::f32::consts::TAU;
                cam.project(add(p, add(mul(b1, th.cos() * len * RING), mul(b2, th.sin() * len * RING))))
            })
            .collect();
        pts.unwrap_or_default()
    });
    Some(Gizmo3d { origin, tips, rings })
}

fn seg_dist(p: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let l2 = dx * dx + dy * dy;
    let u = if l2 > 0.0 { (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / l2).clamp(0.0, 1.0) } else { 0.0 };
    ((p[0] - a[0] - u * dx).powi(2) + (p[1] - a[1] - u * dy).powi(2)).sqrt()
}

/// The handle at frame point `at` (`scale`: screen pixels per frame pixel).
/// Arrows win over rings; the nearest of each kind.
pub fn hit3d(g: &Gizmo3d, at: [f32; 2], scale: f32) -> Option<Handle3d> {
    let grab = GRAB / scale.max(1e-3);
    let best = |it: &mut dyn Iterator<Item = (u8, f32)>| it.filter(|(_, d)| *d < grab).min_by(|a, b| a.1.total_cmp(&b.1)).map(|(i, _)| i);
    // Arrows: from a little way out of the middle (so the body can still be grabbed there) to the tip.
    let arrows = (0..3u8).filter_map(|i| {
        let tip = g.tips[i as usize]?;
        let from = [g.origin[0] + (tip[0] - g.origin[0]) * 0.25, g.origin[1] + (tip[1] - g.origin[1]) * 0.25];
        let screen_len = ((tip[0] - g.origin[0]).powi(2) + (tip[1] - g.origin[1]).powi(2)).sqrt();
        (screen_len * scale > 12.0).then(|| (i, seg_dist(at, from, tip)))
    });
    if let Some(i) = best(&mut arrows.collect::<Vec<_>>().into_iter()) {
        return Some(Handle3d::Axis(i));
    }
    let rings = (0..3u8).filter_map(|i| {
        let r = &g.rings[i as usize];
        (r.len() > 1).then(|| (i, r.windows(2).map(|w| seg_dist(at, w[0], w[1])).fold(f32::MAX, f32::min)))
    });
    best(&mut rings.collect::<Vec<_>>().into_iter()).map(Handle3d::Ring)
}

/// A drag of a 3D handle under way.
#[derive(Clone, Debug)]
pub struct Drag3d {
    pub handle: Handle3d,
    pub start: Transform,
    pub from: [f32; 2],
    /// Axis: the world direction, its length in the world, and its arrow on screen.
    axis: (V3, f32, [f32; 2]),
    /// Ring: the centre on screen, and which way a turn goes on screen (±1).
    centre: [f32; 2],
    sign: f32,
}

/// Start dragging `handle` of the layer with transform `t` from frame point `from`.
pub fn start_drag(handle: Handle3d, t: &Transform, scene: &Scene3d, from: [f32; 2]) -> Option<Drag3d> {
    let cam = &scene.camera;
    let p = pivot(t);
    let origin = cam.project(p)?;
    let len = arrow_len(t, scene)?;
    let mut d = Drag3d { handle, start: *t, from, axis: ([0.0; 3], len, [0.0; 2]), centre: origin, sign: 1.0 };
    match handle {
        Handle3d::Axis(i) => {
            let a = own_axes(t)[i as usize];
            let tip = cam.project(add(p, mul(a, len)))?;
            d.axis = (a, len, [tip[0] - origin[0], tip[1] - origin[1]]);
        }
        Handle3d::Ring(i) => {
            // Which way the picture turns on screen when this turn goes up a degree.
            let probe = {
                let (b1, _) = across(norm(turn_axes(t)[i as usize]));
                add(p, mul(b1, len))
            };
            let local = inverse_rotation(t, add(probe, mul(p, -1.0)));
            let turned = with_turn(t, i, 1.0);
            let moved = add(p, layer_rotation(&turned, local));
            let (a0, a1) = (cam.project(probe)?, cam.project(moved)?);
            let ang = |q: [f32; 2]| (q[1] - origin[1]).atan2(q[0] - origin[0]);
            let mut da = ang(a1) - ang(a0);
            if da > std::f32::consts::PI {
                da -= std::f32::consts::TAU;
            } else if da < -std::f32::consts::PI {
                da += std::f32::consts::TAU;
            }
            d.sign = if da >= 0.0 { 1.0 } else { -1.0 };
        }
    }
    Some(d)
}

/// `t` with its X, Y or Z turn (`i`) up by `deg`.
fn with_turn(t: &Transform, i: u8, deg: f32) -> Transform {
    let mut n = *t;
    match i {
        0 => n.orientation[0] += deg,
        1 => n.orientation[1] += deg,
        _ => n.rotation += deg,
    }
    n
}

/// `layer_rotation` undone.
fn inverse_rotation(t: &Transform, v: V3) -> V3 {
    // The rotation is orthonormal: its inverse is its transpose.
    let [x, y, z] = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]].map(|a| layer_rotation(t, a));
    let dot = |a: V3, b: V3| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
    [dot(x, v), dot(y, v), dot(z, v)]
}

/// The transform for the pointer at `at` (`fine`: Shift, finer steps for a turn —
/// 15° steps; for a move, whole pixels).
pub fn dragged3d(d: &Drag3d, at: [f32; 2], shift: bool) -> Transform {
    let mut t = d.start;
    match d.handle {
        Handle3d::Axis(_) => {
            let (a, len, s) = d.axis;
            let l2 = s[0] * s[0] + s[1] * s[1];
            if l2 < 4.0 {
                return t;
            }
            let along = ((at[0] - d.from[0]) * s[0] + (at[1] - d.from[1]) * s[1]) / l2 * len;
            let along = if shift { along.round() } else { along };
            t.position[0] += a[0] * along;
            t.position[1] += a[1] * along;
            t.depth += a[2] * along;
        }
        Handle3d::Ring(i) => {
            let ang = |q: [f32; 2]| (q[1] - d.centre[1]).atan2(q[0] - d.centre[0]);
            let mut da = (ang(at) - ang(d.from)).to_degrees();
            if da > 180.0 {
                da -= 360.0;
            } else if da < -180.0 {
                da += 360.0;
            }
            let mut deg = da * d.sign;
            if shift {
                deg = (deg / 15.0).round() * 15.0;
            }
            t = with_turn(&d.start, i, deg);
        }
    }
    t
}

/// What a 3D drag reads, for the Viewer's readout.
pub fn readout3d(d: &Drag3d, t: &Transform) -> String {
    let axis = ["X", "Y", "Z"];
    match d.handle {
        Handle3d::Axis(i) => format!("Along its {}: position {:.0}, {:.0}, {:.0}", axis[i as usize], t.position[0], t.position[1], t.depth),
        Handle3d::Ring(i) => {
            let v = match i {
                0 => t.orientation[0],
                1 => t.orientation[1],
                _ => t.rotation,
            };
            format!("Turn {} {:.1}°", axis[i as usize], v)
        }
    }
}

/// What a handle does, for its tooltip.
pub fn hint3d(h: Handle3d) -> &'static str {
    match h {
        Handle3d::Axis(0) => "Drag to move along its own X (red) axis (Shift: whole pixels)",
        Handle3d::Axis(1) => "Drag to move along its own Y (green) axis (Shift: whole pixels)",
        Handle3d::Axis(_) => "Drag to move along its own Z (blue) axis: nearer or further (Shift: whole pixels)",
        Handle3d::Ring(0) => "Drag round to turn about X (tilt it forwards or back; Shift: 15° steps)",
        Handle3d::Ring(1) => "Drag round to turn about Y (swing it left or right; Shift: 15° steps)",
        Handle3d::Ring(_) => "Drag round to turn about Z (spin it in its plane; Shift: 15° steps)",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reelforge_core::space3d::Camera;

    fn scene() -> Scene3d {
        Scene3d { camera: Camera::default_for([1920.0, 1080.0]), lights: Vec::new() }
    }

    /// Facing the camera: X points right, Y down, Z away (a short arrow
    /// straight in); dragging the X arrow moves it only along X, by what
    /// the pointer moved along the arrow.
    #[test]
    fn arrows_move_along_the_layers_own_axes() {
        let s = scene();
        let t = Transform { three_d: true, ..Default::default() };
        let g = gizmo3d(&t, &s).unwrap();
        assert!((g.origin[0] - 960.0).abs() < 0.5 && (g.origin[1] - 540.0).abs() < 0.5);
        let x = g.tips[0].unwrap();
        assert!(x[0] > g.origin[0] + 100.0 && (x[1] - g.origin[1]).abs() < 0.5, "{x:?}");
        let y = g.tips[1].unwrap();
        assert!(y[1] > g.origin[1] + 100.0, "{y:?}");
        let z = g.tips[2].unwrap();
        assert!(((z[0] - g.origin[0]).powi(2) + (z[1] - g.origin[1]).powi(2)).sqrt() < 1.0, "the Z arrow points into the screen");
        assert_eq!(hit3d(&g, [x[0] - 3.0, x[1] + 2.0], 1.0), Some(Handle3d::Axis(0)));
        let d = start_drag(Handle3d::Axis(0), &t, &s, x).unwrap();
        let m = dragged3d(&d, [x[0] + 50.0, x[1] + 30.0], false);
        assert!((m.position[0] - 50.0).abs() < 0.5 && m.position[1].abs() < 1e-3 && m.depth.abs() < 1e-3, "{m:?}");
        // Turned 90° about Y, its own X goes into the screen: dragging its Z
        // arrow (now pointing right) moves it across.
        let turned = Transform { orientation: [0.0, 90.0], ..t };
        let gz = gizmo3d(&turned, &s).unwrap();
        let tip = gz.tips[2].unwrap();
        assert!((tip[0] - gz.origin[0]).abs() > 100.0, "Z now across the screen: {tip:?}");
        let d = start_drag(Handle3d::Axis(2), &turned, &s, tip).unwrap();
        let sign = (tip[0] - gz.origin[0]).signum();
        let m = dragged3d(&d, [tip[0] + 40.0 * sign, tip[1]], false);
        assert!((m.position[0].abs() - 40.0).abs() < 1.0 && m.depth.abs() < 0.5, "{m:?}");
    }

    /// Dragging round a ring turns that one turn, the way the pointer goes;
    /// Shift snaps it to 15°.
    #[test]
    fn rings_turn_one_axis() {
        let s = scene();
        let t = Transform { three_d: true, ..Default::default() };
        let g = gizmo3d(&t, &s).unwrap();
        // The Z ring faces the camera: a circle round the middle.
        let r = &g.rings[2];
        assert_eq!(r.len(), RING_STEPS + 1);
        let at = r[RING_STEPS / 8]; // 45°, clear of the arrows
        assert_eq!(hit3d(&g, at, 1.0), Some(Handle3d::Ring(2)));
        let d = start_drag(Handle3d::Ring(2), &t, &s, at).unwrap();
        // A quarter turn clockwise on screen (y is down).
        let rad = ((at[0] - g.origin[0]).powi(2) + (at[1] - g.origin[1]).powi(2)).sqrt();
        let a0 = (at[1] - g.origin[1]).atan2(at[0] - g.origin[0]);
        let to = [g.origin[0] + rad * (a0 + 0.5).cos(), g.origin[1] + rad * (a0 + 0.5).sin()];
        let m = dragged3d(&d, to, false);
        assert!((m.rotation.abs() - 0.5f32.to_degrees()).abs() < 0.5 && m.orientation == [0.0, 0.0], "{m:?}");
        // Clockwise on screen is a positive rotation (clockwise, as in 2D).
        assert!(m.rotation > 0.0);
        let snapped = dragged3d(&d, to, true);
        assert_eq!(snapped.rotation, 30.0);
        // The X ring is edge-on: a line through the middle, up and down.
        let rx = &g.rings[0];
        let span = rx.iter().map(|p| p[1]).fold(f32::MIN, f32::max) - rx.iter().map(|p| p[1]).fold(f32::MAX, f32::min);
        assert!(span > 100.0);
        assert!(gizmo3d(&Transform { three_d: true, depth: -5000.0, ..Default::default() }, &s).is_none(), "behind the camera: no gizmo");
    }
}
