//! 3D layers, the camera and lights.
//!
//! A 3D layer is a flat plane placed in a scene. Coordinates are pixels: X to
//! the right, Y down and Z away from the viewer, with the frame's centre at the
//! origin, so a 3D layer at Z = 0 seen through the default camera looks exactly
//! as it would in 2D. The camera is the topmost camera clip at that moment
//! (else a default one); lights are every light clip there.
//!
//! Each layer gets a 4×4 matrix taking its pixels (origin at its centre) to
//! clip space, so the GPU draws it in true perspective and cuts away what's
//! behind the camera. Lights shade it per pixel (diffuse, both sides), and the
//! camera's depth of field blurs it by its distance from the focus.

use crate::effects::{camera as cam, light as li, unpack_rgb, Effect};
use crate::project::{Project, TrackKind};
use crate::time::Time;
use crate::transform::{Homography, LayerMap, Transform};
use serde::{Deserialize, Serialize};

/// What a scene clip is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SceneObject {
    Camera,
    Light,
    /// An invisible point (a "null") that other layers can be parented to.
    Point,
}

/// Where a point clip puts the layers parented to it: their positions are
/// turned by `rotation` (degrees, clockwise on screen like a layer's),
/// scaled by `scale` and moved by `position`, all about the frame's centre,
/// and their own rotation and scale change to match. A point at 0, 0, 0
/// with no rotation and 100% scale leaves them where they are.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Null {
    pub position: V3,
    pub rotation: f32,
    pub scale: f32,
}

impl Null {
    pub const IDENTITY: Null = Null { position: [0.0; 3], rotation: 0.0, scale: 1.0 };

    /// From a point clip's settings at source time `t`.
    pub fn from_effect(fx: &Effect, t: Time) -> Self {
        use crate::effects::point as pt;
        let v = |i| fx.value_at(i, t);
        Null { position: [v(pt::X), v(pt::Y), v(pt::Z)], rotation: v(pt::ROTATION), scale: (v(pt::SCALE) / 100.0).max(1e-4) }
    }

    /// Turn and scale a vector (no move).
    pub fn apply_vector(&self, p: V3) -> V3 {
        let (s, c) = self.rotation.to_radians().sin_cos();
        let k = self.scale;
        [k * (c * p[0] - s * p[1]), k * (s * p[0] + c * p[1]), k * p[2]]
    }

    /// Where world point `p` (relative to the frame's centre) goes.
    pub fn apply(&self, p: V3) -> V3 {
        add(self.position, self.apply_vector(p))
    }

    /// This one inside `outer` (a point parented to another point).
    pub fn within(&self, outer: &Null) -> Null {
        Null { position: outer.apply(self.position), rotation: self.rotation + outer.rotation, scale: self.scale * outer.scale }
    }

    /// The null that undoes this one.
    pub fn inverse(&self) -> Null {
        let r = Null { position: [0.0; 3], rotation: -self.rotation, scale: 1.0 / self.scale };
        Null { position: mul(r.apply_vector(self.position), -1.0), ..r }
    }

    /// A layer's transform as its parent places it, for a `frame`-sized frame.
    pub fn transform(&self, t: &Transform, frame: [f32; 2]) -> Transform {
        let mut out = *t;
        let p = self.apply([t.position[0], t.position[1], t.depth]);
        out.position = [p[0], p[1]];
        out.depth = p[2];
        out.rotation += self.rotation;
        out.scale = [t.scale[0] * self.scale, t.scale[1] * self.scale];
        if let Some(pin) = &mut out.pin {
            let (hw, hh) = (frame[0] * 0.5, frame[1] * 0.5);
            for q in &mut pin.0 {
                let m = self.apply([q[0] - hw, q[1] - hh, 0.0]);
                *q = [m[0] + hw, m[1] + hh];
            }
        }
        out
    }
}

type V3 = [f32; 3];

fn sub(a: V3, b: V3) -> V3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn add(a: V3, b: V3) -> V3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
fn mul(a: V3, k: f32) -> V3 {
    [a[0] * k, a[1] * k, a[2] * k]
}
fn dot(a: V3, b: V3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: V3, b: V3) -> V3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}
fn norm(a: V3) -> Option<V3> {
    let l = dot(a, a).sqrt();
    (l > 1e-6 && l.is_finite()).then(|| mul(a, 1.0 / l))
}
/// Rotate `v` about unit axis `k` by `angle` radians (Rodrigues).
fn rotate(v: V3, k: V3, angle: f32) -> V3 {
    let (s, c) = angle.sin_cos();
    add(add(mul(v, c), mul(cross(k, v), s)), mul(k, dot(k, v) * (1.0 - c)))
}

/// Nearest distance in front of the camera that's drawn.
pub const NEAR: f32 = 1.0;
const FAR: f32 = 1.0e6;

/// The camera: where it is, what it looks at, and its lens.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    pub position: V3,
    pub target: V3,
    /// Degrees about the viewing direction: the camera turns clockwise (so
    /// the picture turns anticlockwise).
    pub roll: f32,
    /// Vertical field of view, degrees.
    pub fov: f32,
    /// Depth of field: (focus distance, blur in pixels for something far out of focus).
    pub dof: Option<(f32, f32)>,
    /// Frame size the camera films.
    pub frame: [f32; 2],
}

impl Camera {
    /// The camera used when there's no camera clip: 2D layers at Z = 0 look unchanged.
    pub fn default_for(frame: [f32; 2]) -> Self {
        let mut c = Camera { position: [0.0, 0.0, 0.0], target: [0.0, 0.0, 0.0], roll: 0.0, fov: 39.6, dof: None, frame };
        c.position[2] = -c.focal();
        c
    }

    /// From a camera clip's settings at source time `t`.
    pub fn from_effect(fx: &Effect, t: Time, frame: [f32; 2]) -> Self {
        let v = |i| fx.value_at(i, t);
        Camera {
            position: [v(cam::X), v(cam::Y), v(cam::Z)],
            target: [v(cam::TARGET_X), v(cam::TARGET_Y), v(cam::TARGET_Z)],
            roll: v(cam::ROLL),
            fov: v(cam::FOV).clamp(1.0, 170.0),
            dof: (v(cam::DOF) >= 0.5).then(|| (v(cam::FOCUS).max(1.0), v(cam::APERTURE).max(0.0))),
            frame,
        }
    }

    /// The distance (pixels) at which one pixel of a layer is one pixel of the frame.
    pub fn focal(&self) -> f32 {
        self.frame[1] * 0.5 / (self.fov.to_radians() * 0.5).tan()
    }

    /// How far the camera is from its point of interest.
    pub fn distance(&self) -> f32 {
        dot(sub(self.target, self.position), sub(self.target, self.position)).sqrt()
    }

    /// Unit right, down and forward vectors.
    pub fn basis(&self) -> (V3, V3, V3) {
        let f = norm(sub(self.target, self.position)).unwrap_or([0.0, 0.0, 1.0]);
        // Right from "world down × forward"; looking straight up or down, from Z instead.
        let r = norm(cross([0.0, 1.0, 0.0], f)).or_else(|| norm(cross(f, [0.0, 0.0, 1.0]))).unwrap_or([1.0, 0.0, 0.0]);
        let d = cross(f, r);
        let roll = self.roll.to_radians();
        (rotate(r, f, roll), rotate(d, f, roll), f)
    }

    /// Camera-space coordinates (right, down, forward) of world point `p`.
    pub fn view(&self, p: V3) -> V3 {
        let (r, d, f) = self.basis();
        let q = sub(p, self.position);
        [dot(q, r), dot(q, d), dot(q, f)]
    }

    /// Where world point `p` lands on the frame (pixels), if it's in front of the camera.
    pub fn project(&self, p: V3) -> Option<[f32; 2]> {
        let [x, y, z] = self.view(p);
        (z > NEAR).then(|| {
            let k = self.focal() / z;
            [self.frame[0] * 0.5 + x * k, self.frame[1] * 0.5 + y * k]
        })
    }
}

/// A light, in a form ready to shade with.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Light {
    /// 0 point, 1 spot, 2 directional, 3 ambient.
    pub kind: u32,
    /// Linear-light colour, times intensity.
    pub color: V3,
    pub position: V3,
    /// Unit direction it points (spot, directional).
    pub direction: V3,
    /// Cosines of the cone's outer and inner half-angles (spot).
    pub cone: [f32; 2],
    /// 0 none, 1 smooth, 2 inverse square; and the distance it works over.
    pub falloff: u32,
    pub radius: f32,
    /// How dark the shadows 3D layers cast in this light are, 0–1 (0: none).
    pub shadow: f32,
}

fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

impl Light {
    /// From a light clip's settings at source time `t`.
    pub fn from_effect(fx: &Effect, t: Time) -> Self {
        Self::from_effect_within(fx, t, None)
    }

    /// The same, moved along with the point it's parented to (if any).
    pub fn from_effect_within(fx: &Effect, t: Time, parent: Option<&Null>) -> Self {
        let v = |i| fx.value_at(i, t);
        let rgb = unpack_rgb(v(li::COLOR)).map(srgb_to_linear);
        let k = v(li::INTENSITY) / 100.0;
        let place = |p: V3| parent.map_or(p, |n| n.apply(p));
        let position = place([v(li::X), v(li::Y), v(li::Z)]);
        let target = place([v(li::TARGET_X), v(li::TARGET_Y), v(li::TARGET_Z)]);
        let half = (v(li::CONE).clamp(1.0, 179.0) * 0.5).to_radians();
        let feather = (v(li::FEATHER) / 100.0).clamp(0.0, 1.0);
        Light {
            kind: (v(li::KIND) as u32).min(3),
            color: mul(rgb, k),
            position,
            direction: norm(sub(target, position)).unwrap_or([0.0, 0.0, 1.0]),
            cone: [half.cos(), (half * (1.0 - feather)).cos()],
            falloff: (v(li::FALLOFF) as u32).min(2),
            radius: v(li::RADIUS).max(1.0),
            shadow: if v(li::SHADOWS) >= 0.5 { (v(li::SHADOW_DARKNESS) / 100.0).clamp(0.0, 1.0) } else { 0.0 },
        }
    }

    /// How much of the light reaches world point `p` facing `normal` (the CPU
    /// reference for the shader in `composite.wgsl`), per channel.
    pub fn reach(&self, p: V3, normal: V3) -> V3 {
        if self.kind == 3 {
            return self.color;
        }
        let (l, dist) = if self.kind == 2 {
            (mul(self.direction, -1.0), 0.0)
        } else {
            let d = sub(self.position, p);
            let len = dot(d, d).sqrt().max(1e-3);
            (mul(d, 1.0 / len), len)
        };
        let lambert = dot(normal, l).max(0.0);
        let atten = match (self.kind, self.falloff) {
            (2, _) | (_, 0) => 1.0,
            (_, 1) => (1.0 - dist / self.radius).clamp(0.0, 1.0).powi(2),
            _ => (self.radius * self.radius) / (self.radius * self.radius + dist * dist) * 2.0,
        };
        let spot = if self.kind == 1 {
            let c = dot(mul(l, -1.0), self.direction);
            smoothstep(self.cone[0], self.cone[1].max(self.cone[0] + 1e-4), c)
        } else {
            1.0
        };
        mul(self.color, lambert * atten * spot)
    }
}

fn smoothstep(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// The camera and lights of a sequence at one moment.
#[derive(Clone, Debug, PartialEq)]
pub struct Scene3d {
    pub camera: Camera,
    pub lights: Vec<Light>,
}

/// At most this many lights shade a layer (the rest are left out).
pub const MAX_LIGHTS: usize = 8;

impl Project {
    /// The camera (the topmost camera clip, else the default) and lights at sequence time `t`.
    pub fn scene3d_at(&self, seq: usize, t: Time) -> Scene3d {
        let Some(s) = self.sequences.get(seq) else { return Scene3d { camera: Camera::default_for([1920.0, 1080.0]), lights: Vec::new() } };
        let frame = [s.width.max(2) as f32, s.height.max(2) as f32];
        let mut camera = None;
        let mut lights = Vec::new();
        // Bottom track first, so the topmost camera wins.
        for tr in s.tracks.iter().filter(|tr| tr.kind == TrackKind::Video && !tr.muted) {
            let Some(c) = tr.clip_at(t) else { continue };
            let (Some(what), Some(fx)) = (c.scene, c.effects.first()) else { continue };
            if !fx.enabled {
                continue;
            }
            let src = c.source_time(t);
            let parent = self.parent_of(seq, c, t);
            match what {
                SceneObject::Camera => {
                    let mut cam = Camera::from_effect(fx, src, frame);
                    if let Some(n) = parent {
                        cam.position = n.apply(cam.position);
                        cam.target = n.apply(cam.target);
                    }
                    camera = Some(cam);
                }
                SceneObject::Light if lights.len() < MAX_LIGHTS => lights.push(Light::from_effect_within(fx, src, parent.as_ref())),
                SceneObject::Light | SceneObject::Point => {}
            }
        }
        Scene3d { camera: camera.unwrap_or_else(|| Camera::default_for(frame)), lights }
    }
}

/// How the shader lights one layer: the plane in the world (its pixel (x, y)
/// is at `origin + u·x + v·y`), the camera's position and the lights.
#[derive(Clone, Debug, PartialEq)]
pub struct Lighting {
    pub origin: V3,
    pub u: V3,
    pub v: V3,
    pub camera: V3,
    pub lights: Vec<Light>,
}

/// A 3D layer as seen by the camera.
#[derive(Clone, Debug, PartialEq)]
pub struct LayerSpace {
    /// Layer pixels (origin at its centre) → clip space, columns first, as the compositor takes it.
    pub mvp: [[f32; 4]; 4],
    /// Layer pixels → frame pixels, when the whole layer is in front of the camera.
    pub map: Option<Homography>,
    /// Distance in front of the camera of the layer's anchor (for drawing order).
    pub depth: f32,
    /// Frame pixels per layer pixel at the anchor (for depth-of-field blur).
    pub scale: f32,
    pub lighting: Option<Lighting>,
    /// The layer's plane in the world: where its centre is, and its X and Y
    /// axes per layer pixel (for the shadows it casts).
    pub plane: [V3; 3],
}

impl LayerSpace {
    /// The layer's pixels → frame pixels, for the viewer (hit tests, outline).
    pub fn layer_map(&self) -> Option<LayerMap> {
        self.map.map(LayerMap::Projective)
    }

    /// How much depth of field blurs this layer, in its own pixels (0 when in focus).
    pub fn blur(&self, camera: &Camera) -> f32 {
        let Some((focus, amount)) = camera.dof else { return 0.0 };
        let coc = amount * (self.depth - focus).abs() / self.depth.max(NEAR);
        (coc / self.scale.max(1e-3)).clamp(0.0, 250.0)
    }
}

/// Where the 3D layer with transform `t` (a picture `layer` pixels in size,
/// at 100%) sits, as seen by `scene`'s camera, lit by its lights if `t.lit`.
pub fn layer_space(t: &Transform, layer: [f32; 2], scene: &Scene3d) -> LayerSpace {
    let cam = &scene.camera;
    // The plane's axes in the world: X then Y rotation, then Z (the 2D rotation).
    let rot = |v: V3| {
        let v = rotate(v, [1.0, 0.0, 0.0], t.orientation[0].to_radians());
        let v = rotate(v, [0.0, 1.0, 0.0], -t.orientation[1].to_radians());
        rotate(v, [0.0, 0.0, 1.0], t.rotation.to_radians())
    };
    let u = rot([t.scale[0], 0.0, 0.0]);
    let v = rot([0.0, t.scale[1], 0.0]);
    let pos = [t.position[0], t.position[1], t.depth];
    // The anchor (layer pixels from the centre) lands on the position.
    let origin = sub(sub(pos, mul(u, t.anchor[0])), mul(v, t.anchor[1]));

    let (r, d, f) = cam.basis();
    let o = sub(origin, cam.position);
    let (ur, ud, uf) = (dot(u, r), dot(u, d), dot(u, f));
    let (vr, vd, vf) = (dot(v, r), dot(v, d), dot(v, f));
    let (or, od, of) = (dot(o, r), dot(o, d), dot(o, f));
    let focal = cam.focal();
    let [w, h] = cam.frame;
    let (kx, ky) = (2.0 * focal / w, 2.0 * focal / h);
    let a = FAR / (FAR - NEAR);
    let mvp = [
        [kx * ur, -ky * ud, a * uf, uf],
        [kx * vr, -ky * vd, a * vf, vf],
        [0.0, 0.0, 0.0, 0.0],
        [kx * or, -ky * od, a * of - a * NEAR, of],
    ];
    // Screen = (W/2·z + f·x, H/2·z + f·y) / z, all three affine in the layer's pixels.
    let corners = [[-0.5, -0.5], [0.5, -0.5], [0.5, 0.5], [-0.5, 0.5]].map(|c| [c[0] * layer[0], c[1] * layer[1]]);
    let in_front = corners.iter().all(|c| of + uf * c[0] + vf * c[1] > NEAR);
    let map = in_front.then(|| {
        let row = |a: f32, b: f32, c: f32, za: f32, zb: f32, zc: f32, half: f32| [(focal * a + half * za) as f64, (focal * b + half * zb) as f64, (focal * c + half * zc) as f64];
        let x = row(ur, vr, or, uf, vf, of, w * 0.5);
        let y = row(ud, vd, od, uf, vf, of, h * 0.5);
        Homography([x[0], x[1], x[2], y[0], y[1], y[2], uf as f64, vf as f64, of as f64])
    });
    let depth = dot(sub(pos, cam.position), f);
    let scale = focal / depth.max(NEAR) * ((t.scale[0].abs() + t.scale[1].abs()) * 0.5);
    let lighting = (t.lit && !scene.lights.is_empty()).then(|| Lighting { origin, u: mul(u, 1.0), v: mul(v, 1.0), camera: cam.position, lights: scene.lights.clone() });
    LayerSpace { mvp, map, depth, scale, lighting, plane: [origin, u, v] }
}

/// A solid object (3D text) as the camera sees it. Its own coordinates are
/// pixels from its centre, X right, Y down and Z back (away from the
/// viewer), placed in the world by a layer transform: the same position,
/// anchor, rotations and scale as a 3D layer, with Z scaled by the average
/// of the X and Y scale.
#[derive(Clone, Debug, PartialEq)]
pub struct SolidSpace {
    /// Object → clip space, columns first.
    pub mvp: [[f32; 4]; 4],
    /// Object → world: the origin and the three axes.
    pub origin: V3,
    pub axes: [V3; 3],
    /// Object normals → world normals (normalise after): the inverse
    /// transpose of `axes`, up to a positive factor.
    pub normals: [V3; 3],
    pub camera: V3,
    /// For a scene without lights: the direction towards a light above and
    /// to the left of the camera, scaled so a face turned to the camera gets
    /// exactly 1 (see [`SolidSpace::studio_shade`]).
    pub studio: V3,
    /// The scene's lights, if the layer is lit and there are any.
    pub lights: Vec<Light>,
}

/// How much of the studio light a face gets when its colour is unlit.
pub const STUDIO_AMBIENT: f32 = 0.35;

impl SolidSpace {
    /// The studio light's brightness on a face with unit world normal `n`
    /// (the CPU reference for the shader in `solid.wgsl`).
    pub fn studio_shade(&self, n: V3) -> f32 {
        (STUDIO_AMBIENT + (1.0 - STUDIO_AMBIENT) * dot(n, self.studio).max(0.0)).min(1.0)
    }

    /// Object point → world.
    pub fn world(&self, p: V3) -> V3 {
        add(add(add(self.origin, mul(self.axes[0], p[0])), mul(self.axes[1], p[1])), mul(self.axes[2], p[2]))
    }
}

/// Where a solid placed by transform `t` sits for `scene`'s camera, lit by
/// its lights if `t.lit` (else by the studio light).
pub fn solid_space(t: &Transform, scene: &Scene3d) -> SolidSpace {
    let cam = &scene.camera;
    let rot = |v: V3| {
        let v = rotate(v, [1.0, 0.0, 0.0], t.orientation[0].to_radians());
        let v = rotate(v, [0.0, 1.0, 0.0], -t.orientation[1].to_radians());
        rotate(v, [0.0, 0.0, 1.0], t.rotation.to_radians())
    };
    let sz = (t.scale[0].abs() + t.scale[1].abs()) * 0.5;
    let axes = [rot([t.scale[0], 0.0, 0.0]), rot([0.0, t.scale[1], 0.0]), rot([0.0, 0.0, sz])];
    let pos = [t.position[0], t.position[1], t.depth];
    let origin = sub(sub(pos, mul(axes[0], t.anchor[0])), mul(axes[1], t.anchor[1]));
    let (r, d, f) = cam.basis();
    let focal = cam.focal();
    let [w, h] = cam.frame;
    let (kx, ky) = (2.0 * focal / w, 2.0 * focal / h);
    let a = FAR / (FAR - NEAR);
    let col = |v: V3, point: bool| {
        let (vr, vd, vf) = (dot(v, r), dot(v, d), dot(v, f));
        let shift = if point { a * NEAR } else { 0.0 };
        [kx * vr, -ky * vd, a * vf - shift, vf]
    };
    let mvp = [col(axes[0], false), col(axes[1], false), col(axes[2], false), col(sub(origin, cam.position), true)];
    let det = dot(axes[0], cross(axes[1], axes[2]));
    let k = if det < 0.0 { -1.0 } else { 1.0 };
    let normals = [mul(cross(axes[1], axes[2]), k), mul(cross(axes[2], axes[0]), k), mul(cross(axes[0], axes[1]), k)];
    // Up (−down), left (−right) and towards the camera (−forward).
    let towards = norm(add(add(mul(r, -0.45), mul(d, -0.6)), mul(f, -1.0))).unwrap_or([0.0, 0.0, -1.0]);
    let studio = mul(towards, 1.0 / dot(towards, mul(f, -1.0)).max(0.1));
    let lights = if t.lit { scene.lights.clone() } else { Vec::new() };
    SolidSpace { mvp, origin, axes, normals, camera: cam.position, studio, lights }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: [f32; 2] = [1920.0, 1080.0];

    fn scene() -> Scene3d {
        Scene3d { camera: Camera::default_for(FRAME), lights: Vec::new() }
    }

    /// Clip space → frame pixels.
    fn screen(m: &[[f32; 4]; 4], p: [f32; 2]) -> [f32; 2] {
        let c = |i: usize| m[0][i] * p[0] + m[1][i] * p[1] + m[3][i];
        let (x, y, w) = (c(0), c(1), c(3));
        [(x / w + 1.0) * FRAME[0] / 2.0, (1.0 - y / w) * FRAME[1] / 2.0]
    }

    #[test]
    fn a_3d_layer_at_z0_looks_like_its_2d_self() {
        let t = Transform { position: [100.0, -50.0], scale: [0.5, 0.5], rotation: 30.0, anchor: [20.0, 10.0], three_d: true, ..Default::default() };
        let s = layer_space(&t, [640.0, 360.0], &scene());
        let two_d = t.matrix(FRAME);
        let map = s.map.unwrap();
        for p in [[0.0, 0.0], [300.0, -170.0], [-320.0, 180.0]] {
            let a = two_d.apply(p);
            let b = map.apply(p);
            let c = screen(&s.mvp, p);
            assert!((a[0] - b[0]).abs() < 0.05 && (a[1] - b[1]).abs() < 0.05, "{a:?} {b:?}");
            assert!((a[0] - c[0]).abs() < 0.05 && (a[1] - c[1]).abs() < 0.05, "{a:?} {c:?}");
        }
        assert!((s.depth - Camera::default_for(FRAME).focal()).abs() < 0.01);
    }

    #[test]
    fn further_away_is_smaller_and_turning_gives_perspective() {
        let far = Transform { three_d: true, depth: Camera::default_for(FRAME).focal(), ..Default::default() };
        let s = layer_space(&far, [200.0, 200.0], &scene());
        let m = s.map.unwrap();
        let w = m.apply([100.0, 0.0])[0] - m.apply([-100.0, 0.0])[0];
        assert!((w - 100.0).abs() < 0.05, "twice as far, half the size: {w}");
        // Turned about Y: the near edge is taller than the far one.
        let turned = Transform { three_d: true, orientation: [0.0, 50.0], ..Default::default() };
        let m = layer_space(&turned, [400.0, 400.0], &scene()).map.unwrap();
        let tall = |x: f32| m.apply([x, 200.0])[1] - m.apply([x, -200.0])[1];
        assert!((tall(-200.0) - tall(200.0)).abs() > 20.0, "{} vs {}", tall(-200.0), tall(200.0));
        // Behind the camera: drawn by the GPU (clipped), but no frame map.
        let behind = Transform { three_d: true, depth: -5000.0, ..Default::default() };
        assert!(layer_space(&behind, [100.0, 100.0], &scene()).map.is_none());
    }

    #[test]
    fn camera_moves_and_aims() {
        let mut sc = scene();
        // Camera moved right: the layer appears further left.
        sc.camera.position[0] = 200.0;
        sc.camera.target[0] = 200.0;
        let m = layer_space(&Transform { three_d: true, ..Default::default() }, [100.0, 100.0], &sc).map.unwrap();
        assert!((m.apply([0.0, 0.0])[0] - (960.0 - 200.0)).abs() < 0.05);
        // Aimed at a point: that point is in the middle of the frame.
        sc.camera = Camera { position: [300.0, -200.0, -900.0], target: [-150.0, 80.0, 400.0], ..Camera::default_for(FRAME) };
        let p = sc.camera.project([-150.0, 80.0, 400.0]).unwrap();
        assert!((p[0] - 960.0).abs() < 0.01 && (p[1] - 540.0).abs() < 0.01, "{p:?}");
        // Rolled 90° (the camera turns clockwise, so the picture turns the other
        // way): what was to the right is now above the middle.
        sc.camera = Camera { roll: 90.0, ..Camera::default_for(FRAME) };
        let p = sc.camera.project([100.0, 0.0, 0.0]).unwrap();
        assert!((p[0] - 960.0).abs() < 0.01 && p[1] < 540.0 - 50.0, "{p:?}");
    }

    #[test]
    fn lights_point_spot_directional_ambient() {
        let up: V3 = [0.0, 0.0, -1.0]; // a layer facing the default camera
        let mk = |kind, falloff| Light { kind, color: [1.0, 1.0, 1.0], position: [0.0, 0.0, -500.0], direction: [0.0, 0.0, 1.0], cone: [(30f32).to_radians().cos(), (15f32).to_radians().cos()], falloff, radius: 1000.0, shadow: 0.0 };
        // Point light straight in front: full; off to the side: less.
        let p = mk(0, 0);
        assert!((p.reach([0.0, 0.0, 0.0], up)[0] - 1.0).abs() < 1e-4);
        assert!(p.reach([800.0, 0.0, 0.0], up)[0] < 0.6);
        // Behind the plane's face: no light.
        assert_eq!(p.reach([0.0, 0.0, 0.0], [0.0, 0.0, 1.0])[0], 0.0);
        // Spot: inside the cone lit, outside dark.
        let s = mk(1, 0);
        assert!(s.reach([0.0, 0.0, 0.0], up)[0] > 0.99);
        assert_eq!(s.reach([600.0, 0.0, 0.0], up)[0], 0.0);
        // Smooth falloff fades to nothing at its distance.
        assert_eq!(mk(0, 1).reach([0.0, 0.0, 600.0], up)[0], 0.0);
        // Directional and ambient don't depend on where.
        let d = mk(2, 2);
        assert!((d.reach([5000.0, 0.0, 0.0], up)[0] - 1.0).abs() < 1e-4);
        assert_eq!(mk(3, 2).reach([1.0, 2.0, 3.0], [1.0, 0.0, 0.0]), [1.0; 3]);
    }

    #[test]
    fn a_solid_front_face_sits_where_the_flat_layer_would() {
        let t = Transform { position: [100.0, -50.0], scale: [0.5, 0.5], rotation: 30.0, anchor: [20.0, 10.0], three_d: true, orientation: [10.0, -25.0], depth: 300.0, ..Default::default() };
        let flat = layer_space(&t, [640.0, 360.0], &scene());
        let solid = solid_space(&t, &scene());
        for p in [[0.0, 0.0], [300.0, -170.0], [-320.0, 180.0]] {
            let a = screen(&flat.mvp, p);
            let b = screen(&solid.mvp, p);
            assert!((a[0] - b[0]).abs() < 0.01 && (a[1] - b[1]).abs() < 0.01, "{a:?} {b:?}");
        }
        // Z goes back, away from the camera, scaled with the layer.
        let z = |p: V3| scene().camera.view(solid.world(p))[2];
        assert!((z([0.0, 0.0, 100.0]) - z([0.0, 0.0, 0.0]) - 50.0).abs() < 25.0 && z([0.0, 0.0, 100.0]) > z([0.0, 0.0, 0.0]));
        // Normals: the front face (−Z) turns with the layer and faces the camera.
        let n = solid.normals;
        let front = [-n[2][0], -n[2][1], -n[2][2]];
        let to_cam = sub(scene().camera.position, solid.world([0.0; 3]));
        assert!(dot(front, to_cam) > 0.0);
        // The studio light: full on a face turned to the camera, darker below.
        assert!((solid.studio_shade([0.0, 0.0, -1.0]) - 1.0).abs() < 1e-4);
        assert!(solid.studio_shade([0.0, 1.0, 0.0]) < solid.studio_shade([0.0, -1.0, 0.0]));
        assert!((solid.studio_shade([0.0, 0.0, 1.0]) - STUDIO_AMBIENT).abs() < 1e-4);
    }

    #[test]
    fn depth_of_field_blurs_out_of_focus_layers() {
        let mut sc = scene();
        let focal = sc.camera.focal();
        sc.camera.dof = Some((focal, 40.0));
        let near = layer_space(&Transform { three_d: true, ..Default::default() }, [100.0, 100.0], &sc);
        assert!(near.blur(&sc.camera) < 1e-3, "in focus at the focus distance");
        let far = layer_space(&Transform { three_d: true, depth: 3.0 * focal, ..Default::default() }, [100.0, 100.0], &sc);
        assert!(far.blur(&sc.camera) > 20.0, "{}", far.blur(&sc.camera));
    }
}

/// Move `clip` by `fix` (another null applied on top of where it is): its
/// transform and every keyframe of its position, rotation, scale, Z and corner
/// pin, and for points, cameras and lights their place settings. Used when a
/// clip's parent changes: with `fix` = (new parent)⁻¹ ∘ (old parent) at the
/// playhead, the clip stays exactly where it is on screen there, keeps its own
/// animation, and from then on moves with the new parent.
///
/// Each channel converts on its own (positions move, turn and scale; rotation
/// gains a constant; scale multiplies), so keyframe times, easing and Bézier
/// handles stay valid. A scene object's X/Y/Z settings are converted together
/// at every time any of them has a key (keys are added where only some had one).
pub fn reparent(clip: &mut crate::project::Clip, fix: &Null, frame: [f32; 2]) {
    use crate::effects::point;
    if *fix == Null::IDENTITY {
        return;
    }
    let (hw, hh) = (frame[0] * 0.5, frame[1] * 0.5);
    // The transform and its keyframes.
    clip.transform = fix.transform(&clip.transform, frame);
    let a = &mut clip.anim;
    for k in &mut a.position.0 {
        // Depth is converted on its own (it's scaled and offset only).
        let p = fix.apply([k.value[0], k.value[1], 0.0]);
        k.value = [p[0], p[1]];
    }
    for k in &mut a.rotation.0 {
        k.value += fix.rotation;
    }
    for k in &mut a.scale.0 {
        k.value = [k.value[0] * fix.scale, k.value[1] * fix.scale];
    }
    for k in &mut a.depth.0 {
        k.value = fix.position[2] + fix.scale * k.value;
    }
    for k in &mut a.pin.0 {
        for q in &mut k.value.0 {
            let m = fix.apply([q[0] - hw, q[1] - hh, 0.0]);
            *q = [m[0] + hw, m[1] + hh];
        }
    }
    // A point, camera or light keeps its place in its settings.
    let (Some(what), Some(fx)) = (clip.scene, clip.effects.first_mut()) else { return };
    fx.normalize();
    let triples: &[[usize; 3]] = match what {
        SceneObject::Point => &[[point::X, point::Y, point::Z]],
        SceneObject::Camera => &[[cam::X, cam::Y, cam::Z], [cam::TARGET_X, cam::TARGET_Y, cam::TARGET_Z]],
        SceneObject::Light => &[[li::X, li::Y, li::Z], [li::TARGET_X, li::TARGET_Y, li::TARGET_Z]],
    };
    for idx in triples {
        if idx.iter().any(|&i| i >= fx.params.len()) {
            continue;
        }
        let raw = |fx: &Effect, i: usize, t: Time| fx.params[i].keys.value_at(t, fx.params[i].value);
        let mut times: Vec<Time> = idx.iter().flat_map(|&i| fx.params[i].keys.0.iter().map(|k| k.at)).collect();
        times.sort();
        times.dedup();
        // Work out every new value first, then write them.
        let new_keys: Vec<(Time, V3)> = times.iter().map(|&t| (t, fix.apply([raw(fx, idx[0], t), raw(fx, idx[1], t), raw(fx, idx[2], t)]))).collect();
        let still = fix.apply([fx.params[idx[0]].value, fx.params[idx[1]].value, fx.params[idx[2]].value]);
        for (axis, &i) in idx.iter().enumerate() {
            fx.params[i].value = still[axis];
            for (t, v) in &new_keys {
                fx.params[i].keys.set(*t, v[axis]);
            }
        }
    }
    if what == SceneObject::Point {
        let p = &mut fx.params;
        if point::ROTATION < p.len() {
            p[point::ROTATION].value += fix.rotation;
            for k in &mut p[point::ROTATION].keys.0 {
                k.value += fix.rotation;
            }
        }
        if point::SCALE < p.len() {
            p[point::SCALE].value *= fix.scale;
            for k in &mut p[point::SCALE].keys.0 {
                k.value *= fix.scale;
            }
        }
    }
}
