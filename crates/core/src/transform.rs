//! Per-layer 2D transform, opacity and blend mode, plus the geometry the
//! compositor (GPU) and the viewer gizmo (CPU) both use, so they always agree.
//!
//! Coordinates: frame pixels, origin top-left, +y down. A layer at the default
//! transform sits centred in the frame at its native pixel size (HitFilm's
//! behaviour), so 100% scale means one source pixel per frame pixel.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Transform {
    /// Offset of the layer centre from the frame centre, in frame pixels.
    pub position: [f32; 2],
    /// 1.0 = 100%.
    pub scale: [f32; 2],
    /// Degrees, clockwise on screen.
    pub rotation: f32,
    /// 0.0..=1.0
    pub opacity: f32,
    /// The point the layer scales and rotates around (and that `position`
    /// places), in layer pixels from the layer's centre at 100%.
    pub anchor: [f32; 2],
    /// How much of the layer is cut away from the left, top, right and bottom,
    /// as fractions of its width/height (0–1).
    pub crop: [f32; 4],
}

impl Default for Transform {
    fn default() -> Self {
        Self { position: [0.0, 0.0], scale: [1.0, 1.0], rotation: 0.0, opacity: 1.0, anchor: [0.0, 0.0], crop: [0.0; 4] }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BlendMode {
    #[default]
    Normal,
    Add,
    Multiply,
    Screen,
}

impl BlendMode {
    pub const ALL: [BlendMode; 4] = [BlendMode::Normal, BlendMode::Add, BlendMode::Multiply, BlendMode::Screen];
    pub fn label(self) -> &'static str {
        match self {
            BlendMode::Normal => "Normal",
            BlendMode::Add => "Add",
            BlendMode::Multiply => "Multiply",
            BlendMode::Screen => "Screen",
        }
    }
}

/// `p' = [a c; b d] · p + [tx; ty]`
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Affine2 {
    pub a: f32,
    pub b: f32,
    pub c: f32,
    pub d: f32,
    pub tx: f32,
    pub ty: f32,
}

impl Affine2 {
    pub fn apply(&self, [x, y]: [f32; 2]) -> [f32; 2] {
        [self.a * x + self.c * y + self.tx, self.b * x + self.d * y + self.ty]
    }

    pub fn inverse(&self) -> Option<Affine2> {
        let det = self.a * self.d - self.b * self.c;
        if det.abs() < 1e-12 {
            return None;
        }
        let (a, b, c, d) = (self.d / det, -self.b / det, -self.c / det, self.a / det);
        Some(Affine2 { a, b, c, d, tx: -(a * self.tx + c * self.ty), ty: -(b * self.tx + d * self.ty) })
    }

    /// Column-major 4x4 for WGSL, mapping layer pixels to clip space for a frame of `frame` size.
    pub fn to_clip_space(&self, frame: [f32; 2]) -> [[f32; 4]; 4] {
        // clip.x = 2*px/w - 1, clip.y = 1 - 2*py/h
        let (sx, sy) = (2.0 / frame[0], -2.0 / frame[1]);
        [
            [self.a * sx, self.b * sy, 0.0, 0.0],
            [self.c * sx, self.d * sy, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [self.tx * sx - 1.0, self.ty * sy + 1.0, 0.0, 1.0],
        ]
    }
}

impl Transform {
    /// Maps layer-local pixels (origin at the layer centre) to frame pixels:
    /// the anchor point lands at the frame centre plus `position`, and the
    /// layer scales and rotates around it.
    pub fn matrix(&self, frame: [f32; 2]) -> Affine2 {
        let (s, c) = self.rotation.to_radians().sin_cos();
        let [kx, ky] = self.scale;
        let (a, b, cc, d) = (c * kx, s * kx, -s * ky, c * ky);
        let [ax, ay] = self.anchor;
        Affine2 {
            a,
            b,
            c: cc,
            d,
            tx: frame[0] / 2.0 + self.position[0] - (a * ax + cc * ay),
            ty: frame[1] / 2.0 + self.position[1] - (b * ax + d * ay),
        }
    }

    /// Frame-pixel corners of a `layer`-sized layer (after cropping): top-left, top-right, bottom-right, bottom-left.
    pub fn corners(&self, layer: [f32; 2], frame: [f32; 2]) -> [[f32; 2]; 4] {
        self.rect_corners(self.crop_rect(layer), frame)
    }

    /// Is frame point `p` inside the (cropped) layer?
    pub fn hit(&self, layer: [f32; 2], frame: [f32; 2], p: [f32; 2]) -> bool {
        self.hit_rect(self.crop_rect(layer), frame, p)
    }

    /// The crop amounts, clamped so the sides never pass each other.
    pub fn crop_clamped(&self) -> [f32; 4] {
        let [l, t, r, b] = self.crop.map(|v| if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 });
        [l, t, r.min(1.0 - l), b.min(1.0 - t)]
    }

    /// Does the crop cut anything?
    pub fn is_cropped(&self) -> bool {
        self.crop_clamped().iter().any(|v| *v > 0.0)
    }

    /// The part of the layer's texture that's shown, as UVs `[u0, v0, u1, v1]`.
    pub fn crop_uv(&self) -> [f32; 4] {
        let [l, t, r, b] = self.crop_clamped();
        [l, t, 1.0 - r, 1.0 - b]
    }

    /// What's left of a `layer`-sized layer after cropping, layer-local `[x0, y0, x1, y1]`.
    pub fn crop_rect(&self, layer: [f32; 2]) -> [f32; 4] {
        let [u0, v0, u1, v1] = self.crop_uv();
        let [w, h] = layer;
        [(u0 - 0.5) * w, (v0 - 0.5) * h, (u1 - 0.5) * w, (v1 - 0.5) * h]
    }

    /// Rectangle `r` (layer-local) limited to the cropped area of a `layer`-sized layer.
    pub fn clip_to_crop(&self, r: [f32; 4], layer: [f32; 2]) -> [f32; 4] {
        let c = self.crop_rect(layer);
        let x0 = r[0].max(c[0]);
        let y0 = r[1].max(c[1]);
        [x0, y0, r[2].min(c[2]).max(x0), r[3].min(c[3]).max(y0)]
    }

    /// Change the anchor without moving the picture: `position` shifts to make up for it.
    pub fn with_anchor_kept_in_place(&self, anchor: [f32; 2], frame: [f32; 2]) -> Transform {
        let before = self.matrix(frame).apply(anchor);
        let mut t = *self;
        t.anchor = anchor;
        let after = t.matrix(frame).apply(anchor);
        t.position[0] += before[0] - after[0];
        t.position[1] += before[1] - after[1];
        t
    }

    /// A whole `layer`-sized layer as a layer-local rectangle `[x0, y0, x1, y1]`.
    pub fn full_rect(layer: [f32; 2]) -> [f32; 4] {
        [-layer[0] / 2.0, -layer[1] / 2.0, layer[0] / 2.0, layer[1] / 2.0]
    }

    /// Frame-pixel corners of part of a layer, given in layer-local pixels
    /// (origin at the layer centre), e.g. just the text of a title.
    pub fn rect_corners(&self, r: [f32; 4], frame: [f32; 2]) -> [[f32; 2]; 4] {
        let m = self.matrix(frame);
        [[r[0], r[1]], [r[2], r[1]], [r[2], r[3]], [r[0], r[3]]].map(|p| m.apply(p))
    }

    /// Is frame point `p` inside that part of the layer?
    pub fn hit_rect(&self, r: [f32; 4], frame: [f32; 2], p: [f32; 2]) -> bool {
        let Some(inv) = self.matrix(frame).inverse() else { return false };
        let [x, y] = inv.apply(p);
        x >= r[0] && x <= r[2] && y >= r[1] && y <= r[3]
    }

    /// Uniform scale that fits (`fill = false`) or covers (`fill = true`) the frame.
    pub fn fit_scale(layer: [f32; 2], frame: [f32; 2], fill: bool) -> f32 {
        let (sx, sy) = (frame[0] / layer[0].max(1.0), frame[1] / layer[1].max(1.0));
        if fill { sx.max(sy) } else { sx.min(sy) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: [f32; 2], b: [f32; 2]) -> bool {
        (a[0] - b[0]).abs() < 1e-3 && (a[1] - b[1]).abs() < 1e-3
    }

    #[test]
    fn default_is_centred_at_native_size() {
        let c = Transform::default().corners([640.0, 360.0], [1920.0, 1080.0]);
        assert!(close(c[0], [640.0, 360.0]));
        assert!(close(c[2], [1280.0, 720.0]));
    }

    #[test]
    fn rotation_is_clockwise_on_screen() {
        let t = Transform { rotation: 90.0, ..Default::default() };
        // The layer's right edge midpoint should end up below the centre.
        let p = t.matrix([100.0, 100.0]).apply([10.0, 0.0]);
        assert!(close(p, [50.0, 60.0]), "{p:?}");
    }

    #[test]
    fn inverse_and_hit() {
        let t = Transform { position: [30.0, -20.0], scale: [2.0, 0.5], rotation: 33.0, ..Default::default() };
        let m = t.matrix([800.0, 600.0]);
        let q = m.inverse().unwrap().apply(m.apply([12.0, -7.0]));
        assert!(close(q, [12.0, -7.0]));
        assert!(t.hit([100.0, 100.0], [800.0, 600.0], [430.0, 280.0]));
        assert!(!t.hit([100.0, 100.0], [800.0, 600.0], [10.0, 10.0]));
    }

    #[test]
    fn clip_space_corners() {
        let t = Transform::default();
        let frame = [200.0, 100.0];
        let m = t.matrix(frame).to_clip_space(frame);
        // Layer the size of the frame: its top-left corner maps to clip (-1, 1).
        let (x, y) = (-100.0, -50.0);
        let cx = m[0][0] * x + m[1][0] * y + m[3][0];
        let cy = m[0][1] * x + m[1][1] * y + m[3][1];
        assert!(close([cx, cy], [-1.0, 1.0]), "{cx},{cy}");
    }

    #[test]
    fn anchor_is_the_pivot_and_position_places_it() {
        let frame = [200.0, 100.0];
        // Anchor at the layer's top-left corner of a 40×20 layer.
        let t = Transform { anchor: [-20.0, -10.0], ..Default::default() };
        // The anchor lands on the frame centre...
        assert!(close(t.matrix(frame).apply([-20.0, -10.0]), [100.0, 50.0]));
        // ...and stays there when scaled or rotated.
        let t2 = Transform { scale: [3.0, 3.0], rotation: 45.0, ..t };
        assert!(close(t2.matrix(frame).apply([-20.0, -10.0]), [100.0, 50.0]));
        // Moving the anchor "in place" keeps the picture still.
        let r = Transform { rotation: 30.0, scale: [2.0, 1.5], position: [5.0, 7.0], ..Default::default() };
        let moved = r.with_anchor_kept_in_place([12.0, -4.0], frame);
        for p in [[0.0, 0.0], [10.0, 3.0], [-7.0, 9.0]] {
            assert!(close(r.matrix(frame).apply(p), moved.matrix(frame).apply(p)));
        }
        let at = [100.0 + moved.position[0], 50.0 + moved.position[1]];
        assert!(close(moved.matrix(frame).apply([12.0, -4.0]), at), "position now places the new anchor");
        let spun = Transform { rotation: 75.0, ..moved };
        assert!(close(spun.matrix(frame).apply([12.0, -4.0]), at), "and it's the pivot");
    }

    #[test]
    fn crop_trims_the_shown_area() {
        let t = Transform { crop: [0.25, 0.0, 0.25, 0.5], ..Default::default() };
        assert!(t.is_cropped());
        assert_eq!(t.crop_uv(), [0.25, 0.0, 0.75, 0.5]);
        assert_eq!(t.crop_rect([100.0, 40.0]), [-25.0, -20.0, 25.0, 0.0]);
        let frame = [100.0, 40.0];
        assert!(t.hit([100.0, 40.0], frame, [50.0, 10.0]));
        assert!(!t.hit([100.0, 40.0], frame, [10.0, 10.0]), "cropped away on the left");
        assert!(!t.hit([100.0, 40.0], frame, [50.0, 30.0]), "cropped away at the bottom");
        // Opposite sides can't cross.
        let over = Transform { crop: [0.8, 0.0, 0.8, 0.0], ..Default::default() };
        let uv = over.crop_uv();
        assert!(uv[2] >= uv[0]);
        assert_eq!(t.clip_to_crop([-50.0, -20.0, -30.0, 20.0], [100.0, 40.0]), [-25.0, -20.0, -25.0, 0.0]);
    }

    #[test]
    fn fit_and_fill() {
        assert_eq!(Transform::fit_scale([3840.0, 2160.0], [1920.0, 1080.0], false), 0.5);
        assert_eq!(Transform::fit_scale([1080.0, 1920.0], [1920.0, 1080.0], true), 1920.0 / 1080.0);
    }
}
