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
}

impl Default for Transform {
    fn default() -> Self {
        Self { position: [0.0, 0.0], scale: [1.0, 1.0], rotation: 0.0, opacity: 1.0 }
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
    /// Maps layer-local pixels (origin at the layer centre) to frame pixels.
    pub fn matrix(&self, frame: [f32; 2]) -> Affine2 {
        let (s, c) = self.rotation.to_radians().sin_cos();
        let [kx, ky] = self.scale;
        Affine2 {
            a: c * kx,
            b: s * kx,
            c: -s * ky,
            d: c * ky,
            tx: frame[0] / 2.0 + self.position[0],
            ty: frame[1] / 2.0 + self.position[1],
        }
    }

    /// Frame-pixel corners of a `layer`-sized layer: top-left, top-right, bottom-right, bottom-left.
    pub fn corners(&self, layer: [f32; 2], frame: [f32; 2]) -> [[f32; 2]; 4] {
        self.rect_corners(Self::full_rect(layer), frame)
    }

    /// Is frame point `p` inside the layer?
    pub fn hit(&self, layer: [f32; 2], frame: [f32; 2], p: [f32; 2]) -> bool {
        self.hit_rect(Self::full_rect(layer), frame, p)
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
        let t = Transform { position: [30.0, -20.0], scale: [2.0, 0.5], rotation: 33.0, opacity: 1.0 };
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
    fn fit_and_fill() {
        assert_eq!(Transform::fit_scale([3840.0, 2160.0], [1920.0, 1080.0], false), 0.5);
        assert_eq!(Transform::fit_scale([1080.0, 1920.0], [1920.0, 1080.0], true), 1920.0 / 1080.0);
    }
}
