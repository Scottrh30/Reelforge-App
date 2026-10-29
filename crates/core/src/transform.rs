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
    /// Corner pin: when set, the layer is stretched (in perspective) so its
    /// four corners land on these frame points, and position, scale, rotation
    /// and anchor are ignored.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pin: Option<Pin>,
    /// A 3D layer: a flat plane in a perspective scene, seen through the
    /// sequence's camera and lit by its lights (see [`crate::space3d`]). Then
    /// `depth` and `orientation` apply, and `rotation` turns it about Z.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub three_d: bool,
    /// Position along Z, in pixels: positive is further from the camera.
    #[serde(skip_serializing_if = "is_zero")]
    pub depth: f32,
    /// Rotation about the X and Y axes, in degrees (3D layers).
    #[serde(skip_serializing_if = "is_zero2")]
    pub orientation: [f32; 2],
    /// A 3D layer is shaded by the scene's lights (when there are any).
    #[serde(skip_serializing_if = "is_true")]
    pub lit: bool,
}

fn is_zero(v: &f32) -> bool {
    *v == 0.0
}
fn is_zero2(v: &[f32; 2]) -> bool {
    *v == [0.0, 0.0]
}
fn is_true(v: &bool) -> bool {
    *v
}

impl Default for Transform {
    fn default() -> Self {
        Self { position: [0.0, 0.0], scale: [1.0, 1.0], rotation: 0.0, opacity: 1.0, anchor: [0.0, 0.0], crop: [0.0; 4], pin: None, three_d: false, depth: 0.0, orientation: [0.0, 0.0], lit: true }
    }
}

/// Where a layer's top-left, top-right, bottom-right and bottom-left corners
/// go, in frame pixels.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Pin(pub [[f32; 2]; 4]);

impl Pin {
    /// Is it a usable shape: a convex quadrilateral that isn't squashed flat?
    /// (A twisted or concave one can't be drawn as a perspective view.)
    pub fn is_valid(&self) -> bool {
        let q = &self.0;
        if q.iter().flatten().any(|v| !v.is_finite()) {
            return false;
        }
        let mut sign = 0.0f32;
        for i in 0..4 {
            let (a, b, c) = (q[i], q[(i + 1) % 4], q[(i + 2) % 4]);
            let cross = (b[0] - a[0]) * (c[1] - b[1]) - (b[1] - a[1]) * (c[0] - b[0]);
            if cross.abs() < 1e-3 || (sign != 0.0 && cross.signum() != sign) {
                return false;
            }
            sign = cross.signum();
        }
        true
    }

    /// Every corner moved by `f`.
    pub fn map(&self, f: impl Fn([f32; 2]) -> [f32; 2]) -> Pin {
        Pin(self.0.map(f))
    }
}

/// A projective (perspective) map of the plane, row-major:
/// `(x, y) -> ((h0 x + h1 y + h2) / w, (h3 x + h4 y + h5) / w)`, `w = h6 x + h7 y + h8`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Homography(pub [f64; 9]);

impl Homography {
    pub const IDENTITY: Homography = Homography([1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]);

    /// The map taking each of the four `from` points to the matching `to` point.
    pub fn from_quads(from: &[[f32; 2]; 4], to: &[[f32; 2]; 4]) -> Option<Homography> {
        // h8 = 1; two equations per point pair, eight unknowns.
        let mut m = [[0.0f64; 9]; 8];
        for i in 0..4 {
            let ([x, y], [u, v]) = (from[i].map(|c| c as f64), to[i].map(|c| c as f64));
            m[2 * i] = [x, y, 1.0, 0.0, 0.0, 0.0, -u * x, -u * y, u];
            m[2 * i + 1] = [0.0, 0.0, 0.0, x, y, 1.0, -v * x, -v * y, v];
        }
        let h = solve8(m)?;
        Some(Homography([h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7], 1.0]))
    }

    pub fn from_affine(a: &Affine2) -> Homography {
        let [a0, b, c, d, tx, ty] = [a.a, a.b, a.c, a.d, a.tx, a.ty].map(|v| v as f64);
        Homography([a0, c, tx, b, d, ty, 0.0, 0.0, 1.0])
    }

    pub fn apply(&self, [x, y]: [f32; 2]) -> [f32; 2] {
        let h = &self.0;
        let (x, y) = (x as f64, y as f64);
        let w = h[6] * x + h[7] * y + h[8];
        let w = if w.abs() < 1e-12 { 1e-12 } else { w };
        [((h[0] * x + h[1] * y + h[2]) / w) as f32, ((h[3] * x + h[4] * y + h[5]) / w) as f32]
    }

    /// `self` after `first`: `p -> self(first(p))`.
    pub fn then_after(&self, first: &Homography) -> Homography {
        let (a, b) = (&self.0, &first.0);
        let mut r = [0.0; 9];
        for i in 0..3 {
            for j in 0..3 {
                r[i * 3 + j] = (0..3).map(|k| a[i * 3 + k] * b[k * 3 + j]).sum();
            }
        }
        Homography(r)
    }

    pub fn inverse(&self) -> Option<Homography> {
        let h = &self.0;
        let c = [
            h[4] * h[8] - h[5] * h[7],
            h[2] * h[7] - h[1] * h[8],
            h[1] * h[5] - h[2] * h[4],
            h[5] * h[6] - h[3] * h[8],
            h[0] * h[8] - h[2] * h[6],
            h[2] * h[3] - h[0] * h[5],
            h[3] * h[7] - h[4] * h[6],
            h[1] * h[6] - h[0] * h[7],
            h[0] * h[4] - h[1] * h[3],
        ];
        let det = h[0] * c[0] + h[1] * c[3] + h[2] * c[6];
        if det.abs() < 1e-15 {
            return None;
        }
        Some(Homography(c.map(|v| v / det)))
    }

    /// Column-major 4x4 for WGSL, mapping layer pixels to clip space for a
    /// frame of `frame` size. The perspective goes into clip-space w, so the
    /// GPU interpolates texture coordinates in perspective.
    pub fn to_clip_space(&self, frame: [f32; 2]) -> [[f32; 4]; 4] {
        // Keep w positive where the layer is (h8 > 0 for a pin that's in front).
        let s = if self.0[8] < 0.0 { -1.0 } else { 1.0 };
        let h = self.0.map(|v| (v * s) as f32);
        let (sx, sy) = (2.0 / frame[0], -2.0 / frame[1]);
        // clip.x = sx·X − W, clip.y = sy·Y + W, clip.w = W, with (X, Y, W) = H·(x, y, 1).
        [
            [h[0] * sx - h[6], h[3] * sy + h[6], 0.0, h[6]],
            [h[1] * sx - h[7], h[4] * sy + h[7], 0.0, h[7]],
            [0.0, 0.0, 1.0, 0.0],
            [h[2] * sx - h[8], h[5] * sy + h[8], 0.0, h[8]],
        ]
    }
}

/// Solve an 8×8 linear system given as an augmented matrix (Gaussian elimination
/// with partial pivoting).
fn solve8(mut m: [[f64; 9]; 8]) -> Option<[f64; 8]> {
    for col in 0..8 {
        let piv = (col..8).max_by(|&a, &b| m[a][col].abs().total_cmp(&m[b][col].abs()))?;
        if m[piv][col].abs() < 1e-12 {
            return None;
        }
        m.swap(col, piv);
        for r in 0..8 {
            if r != col {
                let f = m[r][col] / m[col][col];
                if f != 0.0 {
                    for c in col..9 {
                        m[r][c] -= f * m[col][c];
                    }
                }
            }
        }
    }
    Some(std::array::from_fn(|i| m[i][8] / m[i][i]))
}

/// How a layer's own pixels (origin at its centre) map onto the frame: the
/// usual affine transform, or a perspective one when it's corner-pinned.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LayerMap {
    Affine(Affine2),
    Projective(Homography),
}

impl LayerMap {
    pub fn apply(&self, p: [f32; 2]) -> [f32; 2] {
        match self {
            LayerMap::Affine(a) => a.apply(p),
            LayerMap::Projective(h) => h.apply(p),
        }
    }
    pub fn inverse(&self) -> Option<LayerMap> {
        match self {
            LayerMap::Affine(a) => a.inverse().map(LayerMap::Affine),
            LayerMap::Projective(h) => h.inverse().map(LayerMap::Projective),
        }
    }
    pub fn to_clip_space(&self, frame: [f32; 2]) -> [[f32; 4]; 4] {
        match self {
            LayerMap::Affine(a) => a.to_clip_space(frame),
            LayerMap::Projective(h) => h.to_clip_space(frame),
        }
    }
    pub fn homography(&self) -> Homography {
        match self {
            LayerMap::Affine(a) => Homography::from_affine(a),
            LayerMap::Projective(h) => *h,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BlendMode {
    #[default]
    Normal,
    Add,
    Multiply,
    Screen,
    Overlay,
    SoftLight,
    HardLight,
    ColorDodge,
    ColorBurn,
    Darken,
    Lighten,
    Difference,
    Exclusion,
    Hue,
    Saturation,
    Color,
    Luminosity,
}

impl BlendMode {
    pub const ALL: [BlendMode; 17] = [
        BlendMode::Normal,
        BlendMode::Add,
        BlendMode::Multiply,
        BlendMode::Screen,
        BlendMode::Overlay,
        BlendMode::SoftLight,
        BlendMode::HardLight,
        BlendMode::ColorDodge,
        BlendMode::ColorBurn,
        BlendMode::Darken,
        BlendMode::Lighten,
        BlendMode::Difference,
        BlendMode::Exclusion,
        BlendMode::Hue,
        BlendMode::Saturation,
        BlendMode::Color,
        BlendMode::Luminosity,
    ];

    pub fn label(self) -> &'static str {
        match self {
            BlendMode::Normal => "Normal",
            BlendMode::Add => "Add",
            BlendMode::Multiply => "Multiply",
            BlendMode::Screen => "Screen",
            BlendMode::Overlay => "Overlay",
            BlendMode::SoftLight => "Soft light",
            BlendMode::HardLight => "Hard light",
            BlendMode::ColorDodge => "Colour dodge",
            BlendMode::ColorBurn => "Colour burn",
            BlendMode::Darken => "Darken",
            BlendMode::Lighten => "Lighten",
            BlendMode::Difference => "Difference",
            BlendMode::Exclusion => "Exclusion",
            BlendMode::Hue => "Hue",
            BlendMode::Saturation => "Saturation",
            BlendMode::Color => "Colour",
            BlendMode::Luminosity => "Luminosity",
        }
    }

    /// True for the modes that need the pixels under the layer (everything
    /// but Normal, Add, Multiply and Screen, which the GPU's blender does).
    /// These blend sRGB-encoded values, as Photoshop and HitFilm do, so they
    /// look the way you'd expect from those.
    pub fn reads_backdrop(self) -> bool {
        !matches!(self, BlendMode::Normal | BlendMode::Add | BlendMode::Multiply | BlendMode::Screen)
    }

    /// The colour of fully opaque `top` blended onto `under`, both sRGB-encoded
    /// 0–1: a reference for the GPU shader. (Opacity then mixes the result
    /// with `under` in linear light, as for Normal.)
    pub fn blend_rgb(self, under: [f32; 3], top: [f32; 3]) -> [f32; 3] {
        blend_fn(self, under, top)
    }
}

/// The W3C compositing spec's blend functions (`cb` under, `cs` on top).
fn blend_fn(mode: BlendMode, cb: [f32; 3], cs: [f32; 3]) -> [f32; 3] {
    let sep = |f: fn(f32, f32) -> f32| [f(cb[0], cs[0]), f(cb[1], cs[1]), f(cb[2], cs[2])];
    fn hard(b: f32, s: f32) -> f32 {
        if s <= 0.5 { b * 2.0 * s } else { let s2 = 2.0 * s - 1.0; b + s2 - b * s2 }
    }
    match mode {
        BlendMode::Normal => cs,
        BlendMode::Add => sep(|b, s| (b + s).min(1.0)),
        BlendMode::Multiply => sep(|b, s| b * s),
        BlendMode::Screen => sep(|b, s| b + s - b * s),
        BlendMode::Overlay => sep(|b, s| hard(s, b)),
        BlendMode::HardLight => sep(hard),
        BlendMode::SoftLight => sep(|b, s| {
            if s <= 0.5 {
                b - (1.0 - 2.0 * s) * b * (1.0 - b)
            } else {
                let d = if b <= 0.25 { ((16.0 * b - 12.0) * b + 4.0) * b } else { b.sqrt() };
                b + (2.0 * s - 1.0) * (d - b)
            }
        }),
        BlendMode::ColorDodge => sep(|b, s| if b <= 0.0 { 0.0 } else if s >= 1.0 { 1.0 } else { (b / (1.0 - s)).min(1.0) }),
        BlendMode::ColorBurn => sep(|b, s| if b >= 1.0 { 1.0 } else if s <= 0.0 { 0.0 } else { 1.0 - ((1.0 - b) / s).min(1.0) }),
        BlendMode::Darken => sep(f32::min),
        BlendMode::Lighten => sep(f32::max),
        BlendMode::Difference => sep(|b, s| (b - s).abs()),
        BlendMode::Exclusion => sep(|b, s| b + s - 2.0 * b * s),
        BlendMode::Hue => set_lum(set_sat(cs, sat(cb)), lum(cb)),
        BlendMode::Saturation => set_lum(set_sat(cb, sat(cs)), lum(cb)),
        BlendMode::Color => set_lum(cs, lum(cb)),
        BlendMode::Luminosity => set_lum(cb, lum(cs)),
    }
}

fn lum(c: [f32; 3]) -> f32 {
    0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2]
}

fn set_lum(c: [f32; 3], l: f32) -> [f32; 3] {
    let d = l - lum(c);
    let c = c.map(|v| v + d);
    let l = lum(c);
    let (n, x) = (c[0].min(c[1]).min(c[2]), c[0].max(c[1]).max(c[2]));
    c.map(|v| {
        let mut v = v;
        if n < 0.0 {
            v = l + (v - l) * l / (l - n);
        }
        if x > 1.0 {
            v = l + (v - l) * (1.0 - l) / (x - l);
        }
        v
    })
}

fn sat(c: [f32; 3]) -> f32 {
    c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2])
}

fn set_sat(c: [f32; 3], s: f32) -> [f32; 3] {
    let (n, x) = (c[0].min(c[1]).min(c[2]), c[0].max(c[1]).max(c[2]));
    if x <= n {
        return [0.0; 3];
    }
    c.map(|v| (v - n) * s / (x - n))
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

    /// The pin, if there is a usable one.
    pub fn valid_pin(&self) -> Option<Pin> {
        self.pin.filter(Pin::is_valid)
    }

    /// How a `layer`-sized layer's pixels (origin at its centre) map onto the
    /// frame, corner pin included. Prefer this to [`Transform::matrix`]
    /// anywhere a pinned layer may turn up.
    pub fn layer_map(&self, layer: [f32; 2], frame: [f32; 2]) -> LayerMap {
        if let Some(pin) = self.valid_pin() {
            let r = Self::full_rect(layer);
            let from = [[r[0], r[1]], [r[2], r[1]], [r[2], r[3]], [r[0], r[3]]];
            if let Some(h) = Homography::from_quads(&from, &pin.0) {
                return LayerMap::Projective(h);
            }
        }
        LayerMap::Affine(self.matrix(frame))
    }

    /// The pin that shows the layer exactly where its transform puts it now
    /// (the starting point when corner-pinning a layer).
    pub fn pin_of_transform(&self, layer: [f32; 2], frame: [f32; 2]) -> Pin {
        let r = Self::full_rect(layer);
        let m = self.matrix(frame);
        Pin([[r[0], r[1]], [r[2], r[1]], [r[2], r[3]], [r[0], r[3]]].map(|p| m.apply(p)))
    }

    /// Frame-pixel corners of a `layer`-sized layer (after cropping): top-left, top-right, bottom-right, bottom-left.
    pub fn corners(&self, layer: [f32; 2], frame: [f32; 2]) -> [[f32; 2]; 4] {
        let r = self.crop_rect(layer);
        let m = self.layer_map(layer, frame);
        [[r[0], r[1]], [r[2], r[1]], [r[2], r[3]], [r[0], r[3]]].map(|p| m.apply(p))
    }

    /// Is frame point `p` inside the (cropped) layer?
    pub fn hit(&self, layer: [f32; 2], frame: [f32; 2], p: [f32; 2]) -> bool {
        let r = self.crop_rect(layer);
        let Some(inv) = self.layer_map(layer, frame).inverse() else { return false };
        let [x, y] = inv.apply(p);
        x >= r[0] && x <= r[2] && y >= r[1] && y <= r[3]
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

    #[test]
    fn blend_modes_follow_the_usual_formulas() {
        use BlendMode as M;
        let near = |a: [f32; 3], b: [f32; 3]| a.iter().zip(&b).all(|(x, y)| (x - y).abs() < 1e-4);
        let under = [0.2, 0.5, 0.8];
        let grey = [0.5; 3];
        // Mid grey on top changes nothing with overlay and soft light; under hard light it is the backdrop that is neutral.
        for m in [M::Overlay, M::SoftLight] {
            assert!(near(m.blend_rgb(under, grey), under), "{m:?}");
        }
        assert!(near(M::HardLight.blend_rgb(grey, under), under));
        // Overlay: darks darker, lights lighter (a contrast boost from the layer).
        let o = M::Overlay.blend_rgb([0.25, 0.75, 0.5], [0.75; 3]);
        assert!(near(o, [0.375, 0.875, 0.75]), "{o:?}");
        assert!(near(M::Difference.blend_rgb(under, under), [0.0; 3]));
        assert!(near(M::Exclusion.blend_rgb(under, [1.0; 3]), [0.8, 0.5, 0.2]));
        assert!(near(M::Darken.blend_rgb(under, grey), [0.2, 0.5, 0.5]));
        assert!(near(M::Lighten.blend_rgb(under, grey), [0.5, 0.5, 0.8]));
        assert!(near(M::ColorDodge.blend_rgb([0.25; 3], [0.5; 3]), [0.5; 3]));
        assert!(near(M::ColorBurn.blend_rgb([0.75; 3], [0.5; 3]), [0.5; 3]));
        // Luminosity of a grey keeps the backdrop's hue but takes the grey's brightness.
        let red = [0.8, 0.2, 0.2];
        let l = M::Luminosity.blend_rgb(red, [0.3; 3]);
        assert!((lum(l) - 0.3).abs() < 1e-4 && l[0] > l[1] && (l[1] - l[2]).abs() < 1e-4, "{l:?}");
        // Colour: the layer's hue and saturation at the backdrop's brightness.
        let c = M::Color.blend_rgb([0.4; 3], red);
        assert!((lum(c) - 0.4).abs() < 1e-4 && c[0] > c[1], "{c:?}");
        // Saturation from a grey layer: the backdrop goes grey.
        let g = M::Saturation.blend_rgb(red, grey);
        assert!((g[0] - g[1]).abs() < 1e-4 && (g[1] - g[2]).abs() < 1e-4);
        // Hue from a blue layer onto red keeps the red's saturation and brightness.
        let h = M::Hue.blend_rgb(red, [0.1, 0.1, 0.9]);
        assert!(h[2] > h[0] && (lum(h) - lum(red)).abs() < 1e-4 && (sat(h) - sat(red)).abs() < 1e-3, "{h:?}");
        assert!(M::ALL.iter().filter(|m| m.reads_backdrop()).count() == 13);
    }

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
    fn homography_maps_the_corners_and_inverts() {
        let from = [[0.0, 0.0], [100.0, 0.0], [100.0, 50.0], [0.0, 50.0]];
        let to = [[10.0, 20.0], [130.0, 5.0], [120.0, 90.0], [15.0, 70.0]];
        let h = Homography::from_quads(&from, &to).unwrap();
        for i in 0..4 {
            assert!(close(h.apply(from[i]), to[i]), "{:?}", h.apply(from[i]));
        }
        let inv = h.inverse().unwrap();
        assert!(close(inv.apply(h.apply([33.0, 17.0])), [33.0, 17.0]));
        // Composition: (inv after h) is the identity.
        assert!(close(inv.then_after(&h).apply([70.0, 40.0]), [70.0, 40.0]));
        // An affine map turns into the same homography.
        let t = Transform { position: [3.0, 4.0], rotation: 20.0, scale: [1.5, 0.5], ..Default::default() };
        let m = t.matrix([200.0, 100.0]);
        assert!(close(Homography::from_affine(&m).apply([7.0, -2.0]), m.apply([7.0, -2.0])));
    }

    #[test]
    fn pinned_layers_land_on_their_pin() {
        let (layer, frame) = ([40.0, 20.0], [200.0, 100.0]);
        let pin = Pin([[10.0, 10.0], [90.0, 20.0], [80.0, 90.0], [20.0, 70.0]]);
        let t = Transform { pin: Some(pin), position: [500.0, 0.0], ..Default::default() };
        let c = t.corners(layer, frame);
        for i in 0..4 {
            assert!(close(c[i], pin.0[i]), "{:?}", c);
        }
        assert!(t.hit(layer, frame, [50.0, 50.0]));
        assert!(!t.hit(layer, frame, [5.0, 95.0]));
        // Clip space: the top-left corner of the layer goes to the pin's top-left.
        let m = t.layer_map(layer, frame).to_clip_space(frame);
        let (x, y) = (-20.0, -10.0);
        let w = m[0][3] * x + m[1][3] * y + m[3][3];
        let cx = (m[0][0] * x + m[1][0] * y + m[3][0]) / w;
        let cy = (m[0][1] * x + m[1][1] * y + m[3][1]) / w;
        assert!(close([(cx + 1.0) * 100.0, (1.0 - cy) * 50.0], [10.0, 10.0]), "{cx},{cy}");
        // Twisted or flat pins are ignored (the transform is used instead).
        let twisted = Transform { pin: Some(Pin([[10.0, 10.0], [90.0, 90.0], [90.0, 10.0], [10.0, 90.0]])), ..Default::default() };
        assert!(twisted.valid_pin().is_none());
        assert!(matches!(twisted.layer_map(layer, frame), LayerMap::Affine(_)));
        // A pin made from the transform reproduces it.
        let r = Transform { position: [12.0, -6.0], rotation: 25.0, scale: [1.2, 0.8], ..Default::default() };
        let p = Transform { pin: Some(r.pin_of_transform(layer, frame)), ..r };
        assert!(close(p.layer_map(layer, frame).apply([7.0, 3.0]), r.matrix(frame).apply([7.0, 3.0])));
    }

    #[test]
    fn fit_and_fill() {
        assert_eq!(Transform::fit_scale([3840.0, 2160.0], [1920.0, 1080.0], false), 0.5);
        assert_eq!(Transform::fit_scale([1080.0, 1920.0], [1920.0, 1080.0], true), 1920.0 / 1080.0);
    }
}
