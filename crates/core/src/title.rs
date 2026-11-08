//! Text titles.
//!
//! A title is a clip on a video track that has no media file: its picture is
//! generated from a [`TitleSpec`]. It behaves like a still image (any length,
//! nothing to slip) and goes through the same compositor as footage, so the
//! transform, keyframes, effects, dissolves and export all apply to it.
//!
//! The text is laid out on a canvas the size of the sequence frame, centred,
//! so at the default transform it sits in the middle of the picture.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Horizontal alignment of the lines within the text block.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TextAlign {
    Left,
    #[default]
    Center,
    Right,
}

impl TextAlign {
    pub const ALL: [TextAlign; 3] = [TextAlign::Left, TextAlign::Center, TextAlign::Right];
    pub fn label(self) -> &'static str {
        match self {
            TextAlign::Left => "Left",
            TextAlign::Center => "Centre",
            TextAlign::Right => "Right",
        }
    }
}

/// Which font file to use. `None` path = the built-in font.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FontChoice {
    pub path: Option<PathBuf>,
    /// Face index inside a font collection (.ttc).
    #[serde(default)]
    pub index: u32,
    /// What to show in the UI, e.g. "Arial Bold".
    #[serde(default)]
    pub name: String,
}

impl FontChoice {
    pub fn builtin() -> Self {
        Self { path: None, index: 0, name: "Built-in (Ubuntu Light)".into() }
    }
}

fn white() -> [u8; 4] {
    [255, 255, 255, 255]
}
fn black() -> [u8; 4] {
    [0, 0, 0, 255]
}
fn half_black() -> [u8; 4] {
    [0, 0, 0, 160]
}
fn one() -> f32 {
    1.0
}

/// Everything about how a title looks. Colours are 8-bit sRGB with straight alpha.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TitleSpec {
    pub text: String,
    #[serde(default)]
    pub font: FontChoice,
    /// Font size in pixels of the sequence frame (the em size).
    pub size: f32,
    #[serde(default = "white")]
    pub color: [u8; 4],
    #[serde(default)]
    pub align: TextAlign,
    /// Multiplier on the font's natural line height.
    #[serde(default = "one")]
    pub line_spacing: f32,
    /// Extra space between letters, in pixels.
    #[serde(default)]
    pub letter_spacing: f32,
    /// Outline width in pixels (0 = none).
    #[serde(default)]
    pub outline: f32,
    #[serde(default = "black")]
    pub outline_color: [u8; 4],
    #[serde(default)]
    pub shadow: bool,
    /// Shadow offset distance in pixels and direction in degrees (0 = right, 90 = down).
    #[serde(default)]
    pub shadow_distance: f32,
    #[serde(default)]
    pub shadow_angle: f32,
    /// Shadow blur radius in pixels.
    #[serde(default)]
    pub shadow_softness: f32,
    #[serde(default = "half_black")]
    pub shadow_color: [u8; 4],
    /// A filled rectangle behind the text (e.g. for lower thirds).
    #[serde(default)]
    pub background: bool,
    #[serde(default = "half_black")]
    pub background_color: [u8; 4],
    /// Space between the text and the edge of the box, in pixels.
    #[serde(default)]
    pub background_padding: f32,
    /// 3D text: how far the letters are extruded back, in pixels (0 = flat
    /// text). Extruded text is a solid object seen through the sequence's
    /// camera and lit by its lights (outline, shadow and box are left out).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub extrude: f32,
    /// The colour of 3D text's sides (`None`: the text colour).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub side_color: Option<[u8; 4]>,
    /// 3D text's edges, front and back, cut back by this much (pixels; 0 =
    /// sharp edges), so they catch the light. At most half the depth.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub bevel: f32,
    /// The shape of the bevel.
    #[serde(default, skip_serializing_if = "BevelStyle::is_default")]
    pub bevel_style: BevelStyle,
    /// A filled shape (an SVG logo, or a mask's outline) instead of the text:
    /// centred, `size` pixels high, in the text colour; extruded like 3D text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shape: Option<std::sync::Arc<Shape>>,
    /// A 3D model (OBJ, glTF, FBX, USD, STL, PLY…) instead of the text:
    /// always 3D, `size` pixels high, lit and shadowed like 3D text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ModelSource>,
    /// Letters, words or lines coming in (and going out) one after another.
    #[serde(default, skip_serializing_if = "TextAnim::is_none")]
    pub anim: TextAnim,
}

/// A filled outline used in place of a title's text: one or more paths, each
/// closed contours filled by its own rule. Kept centred on 0 and 1 high (Y
/// down), so the title's size scales it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Shape {
    /// What it's called (the SVG's file name, or the mask's).
    pub name: String,
    pub paths: Vec<ShapePath>,
    /// Width over height.
    pub aspect: f32,
    /// Identifies the outline (a hash of its points).
    pub id: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ShapePath {
    pub contours: Vec<Vec<[f32; 2]>>,
    /// Filled by the even-odd rule (otherwise non-zero).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub even_odd: bool,
}

impl Shape {
    /// From paths in any units (Y down): centred and scaled to 1 high.
    /// `None` without at least one contour of three points with some area.
    pub fn new(name: &str, paths: Vec<ShapePath>) -> Option<Shape> {
        let paths: Vec<ShapePath> = paths
            .into_iter()
            .map(|p| ShapePath { contours: p.contours.into_iter().filter(|c| c.len() >= 3).collect(), even_odd: p.even_odd })
            .filter(|p| !p.contours.is_empty())
            .collect();
        let pts = paths.iter().flat_map(|p| p.contours.iter().flatten());
        let (lo, hi) = pts.fold(([f32::MAX; 2], [f32::MIN; 2]), |(lo, hi), q| ([lo[0].min(q[0]), lo[1].min(q[1])], [hi[0].max(q[0]), hi[1].max(q[1])]));
        let (w, h) = (hi[0] - lo[0], hi[1] - lo[1]);
        if !(w > 1e-6 && h > 1e-6 && w.is_finite() && h.is_finite()) {
            return None;
        }
        let c = [(lo[0] + hi[0]) * 0.5, (lo[1] + hi[1]) * 0.5];
        let paths: Vec<ShapePath> = paths.into_iter().map(|p| ShapePath { contours: p.contours.into_iter().map(|k| k.into_iter().map(|q| [(q[0] - c[0]) / h, (q[1] - c[1]) / h]).collect()).collect(), even_odd: p.even_odd }).collect();
        use std::hash::{Hash, Hasher};
        let mut hs = std::collections::hash_map::DefaultHasher::new();
        for p in &paths {
            p.even_odd.hash(&mut hs);
            for k in &p.contours {
                k.len().hash(&mut hs);
                for q in k {
                    (q[0].to_bits(), q[1].to_bits()).hash(&mut hs);
                }
            }
        }
        Some(Shape { name: name.into(), paths, aspect: w / h, id: hs.finish() })
    }

    /// Its paths placed `height` pixels high, centred on `centre`.
    pub fn placed(&self, centre: [f32; 2], height: f32) -> Vec<ShapePath> {
        self.paths.iter().map(|p| ShapePath { contours: p.contours.iter().map(|k| k.iter().map(|q| [centre[0] + q[0] * height, centre[1] + q[1] * height]).collect()).collect(), even_odd: p.even_odd }).collect()
    }
}

/// A 3D model file a title shows.
#[derive(Clone, Debug, PartialEq, Hash, Serialize, Deserialize)]
pub struct ModelSource {
    pub path: std::path::PathBuf,
    /// Up is the file's Z axis (CAD, 3D printing, Blender's own axes), not Y.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub z_up: bool,
    /// Use the model's own colours (materials, vertex colours); otherwise the title colour.
    #[serde(default = "yes")]
    pub own_colours: bool,
    /// Bumped to read the file again (after it changed on disk).
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub rev: u32,
    /// Parts (by name) shown in a colour of their own instead of the file's
    /// (and without their texture).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub part_colours: Vec<(String, [u8; 3])>,
}

fn yes() -> bool {
    true
}

fn is_zero_u32(v: &u32) -> bool {
    *v == 0
}

/// File types read as 3D models.
pub const MODEL_EXTENSIONS: [&str; 10] = ["obj", "gltf", "glb", "fbx", "usd", "usda", "usdc", "usdz", "stl", "ply"];

/// Whether `path` is a 3D model file (by its extension).
pub fn is_model_file(path: &std::path::Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| MODEL_EXTENSIONS.contains(&e.to_lowercase().as_str()))
}

/// The shape of 3D text's bevelled edges.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BevelStyle {
    /// A quarter circle: smooth, rounded edges.
    #[default]
    Round,
    /// A straight 45° cut.
    Flat,
    /// A concave quarter circle, like a cove moulding.
    Cove,
}

impl BevelStyle {
    pub fn is_default(&self) -> bool {
        *self == Self::Round
    }
}

/// How a title's text comes in, piece by piece.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TextAnimKind {
    /// All at once (no animation).
    #[default]
    None,
    /// Letter by letter, each appearing at once, like typing.
    Typewriter,
    /// Letter by letter, each fading in.
    FadeLetters,
    /// Word by word, each fading in.
    FadeWords,
    /// Line by line, each sliding up into place as it fades in.
    SlideLines,
    /// Letter by letter, each dropping in from above and bouncing to rest.
    Bounce,
}

impl TextAnimKind {
    pub const ALL: [TextAnimKind; 6] = [TextAnimKind::None, TextAnimKind::Typewriter, TextAnimKind::FadeLetters, TextAnimKind::FadeWords, TextAnimKind::SlideLines, TextAnimKind::Bounce];

    pub fn label(self) -> &'static str {
        match self {
            TextAnimKind::None => "None",
            TextAnimKind::Typewriter => "Typewriter",
            TextAnimKind::FadeLetters => "Fade in letters",
            TextAnimKind::FadeWords => "Fade in words",
            TextAnimKind::SlideLines => "Slide up lines",
            TextAnimKind::Bounce => "Bounce in letters",
        }
    }
}

/// A title's text animation.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TextAnim {
    pub kind: TextAnimKind,
    /// Seconds between one piece starting and the next.
    pub stagger: f32,
    /// Seconds each piece takes to come in.
    pub length: f32,
    /// Seconds into the clip before the first piece starts.
    pub delay: f32,
    /// Also go out again at the end of the clip, in the same order.
    pub out: bool,
}

impl Default for TextAnim {
    fn default() -> Self {
        TextAnim { kind: TextAnimKind::None, stagger: 0.05, length: 0.4, delay: 0.0, out: false }
    }
}

impl TextAnim {
    pub fn is_none(&self) -> bool {
        self.kind == TextAnimKind::None
    }

    /// Sensible timing for a kind (picked in the Title panel).
    pub fn preset(kind: TextAnimKind) -> Self {
        let (stagger, length) = match kind {
            TextAnimKind::None => (0.05, 0.4),
            TextAnimKind::Typewriter => (0.06, 0.0),
            TextAnimKind::FadeLetters => (0.04, 0.4),
            TextAnimKind::FadeWords => (0.25, 0.5),
            TextAnimKind::SlideLines => (0.35, 0.6),
            TextAnimKind::Bounce => (0.06, 0.7),
        };
        TextAnim { kind, stagger, length, delay: 0.0, out: false }
    }
}

/// One glyph's state at a moment of a text animation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GlyphState {
    /// Opacity, 0–1.
    pub alpha: f32,
    /// Moved down by this much, in ems (negative: up).
    pub dy: f32,
}

fn smooth(p: f32) -> f32 {
    let p = p.clamp(0.0, 1.0);
    p * p * (3.0 - 2.0 * p)
}

/// The usual "ease out bounce": falls to 1, bouncing a little less each time.
fn bounce(p: f32) -> f32 {
    let (n, d) = (7.5625, 2.75);
    let p = p.clamp(0.0, 1.0);
    if p < 1.0 / d {
        n * p * p
    } else if p < 2.0 / d {
        let q = p - 1.5 / d;
        n * q * q + 0.75
    } else if p < 2.5 / d {
        let q = p - 2.25 / d;
        n * q * q + 0.9375
    } else {
        let q = p - 2.625 / d;
        n * q * q + 0.984375
    }
}

fn is_zero(v: &f32) -> bool {
    *v == 0.0
}

impl Default for TitleSpec {
    fn default() -> Self {
        Self {
            text: "Title".into(),
            font: FontChoice::builtin(),
            size: 96.0,
            color: white(),
            align: TextAlign::Center,
            line_spacing: 1.0,
            letter_spacing: 0.0,
            outline: 0.0,
            outline_color: black(),
            shadow: true,
            shadow_distance: 4.0,
            shadow_angle: 45.0,
            shadow_softness: 6.0,
            shadow_color: half_black(),
            background: false,
            background_color: half_black(),
            background_padding: 24.0,
            extrude: 0.0,
            side_color: None,
            bevel: 0.0,
            bevel_style: BevelStyle::Round,
            shape: None,
            model: None,
            anim: TextAnim::default(),
        }
    }
}

impl TitleSpec {
    /// A default title sized for a frame of `height` pixels.
    pub fn for_frame(height: u32) -> Self {
        let k = height.max(1) as f32 / 1080.0;
        Self {
            size: (96.0 * k).round().max(8.0),
            shadow_distance: (4.0 * k).max(1.0),
            shadow_softness: (6.0 * k).max(1.0),
            background_padding: (24.0 * k).round(),
            ..Self::default()
        }
    }

    /// Extruded into solid 3D letters.
    pub fn is_3d(&self) -> bool {
        self.extrude > 0.0 || self.model.is_some()
    }

    /// First line of the text, for labels.
    pub fn label(&self) -> String {
        let first = self.text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
        if let Some(s) = &self.shape {
            return s.name.clone();
        }
        if let Some(m) = &self.model {
            return m.path.file_name().map_or("3D model".into(), |n| n.to_string_lossy().into_owned());
        }
        if first.is_empty() { "(empty title)".into() } else { first.to_string() }
    }

    /// Which piece of the animation each character of the text (newlines
    /// left out, in order, as laid out) belongs to, and how many pieces there
    /// are. Spaces go with the piece after them.
    pub fn anim_pieces(&self) -> (Vec<usize>, usize) {
        let lines: Vec<&str> = self.text.split('\n').map(|l| l.trim_end_matches('\r')).collect();
        let mut out = Vec::new();
        let mut n = 0usize;
        match self.anim.kind {
            TextAnimKind::SlideLines => {
                for l in &lines {
                    let chars = l.chars().count();
                    out.extend(std::iter::repeat_n(n, chars));
                    if chars > 0 {
                        n += 1;
                    }
                }
            }
            TextAnimKind::FadeWords => {
                // Each word its own piece; spaces go with the word after them.
                let mut words: Vec<Option<usize>> = Vec::new();
                for l in &lines {
                    let mut in_word = false;
                    for ch in l.chars() {
                        if ch.is_whitespace() {
                            in_word = false;
                            words.push(None);
                        } else {
                            if !in_word {
                                n += 1;
                                in_word = true;
                            }
                            words.push(Some(n - 1));
                        }
                    }
                }
                let mut next = n.saturating_sub(1);
                for w in words.iter().rev() {
                    if let Some(i) = w {
                        next = *i;
                    }
                    out.push(w.unwrap_or(next));
                }
                out.reverse();
            }
            _ => {
                let mut k = 0usize;
                for l in &lines {
                    for ch in l.chars() {
                        out.push(k);
                        if !ch.is_whitespace() {
                            k += 1;
                        }
                    }
                }
                n = k;
            }
        }
        (out, n)
    }

    /// Every glyph's state at `t` seconds into a clip `dur` seconds long
    /// (`None` without an animation).
    pub fn glyph_states(&self, t: f32, dur: f32) -> Option<Vec<GlyphState>> {
        if self.anim.is_none() || self.is_3d() {
            return None;
        }
        let a = self.anim;
        let (pieces, n) = self.anim_pieces();
        let progress = |i: usize| -> f32 {
            let start = a.delay + i as f32 * a.stagger.max(0.0);
            let p_in = if a.length <= 0.0 { if t >= start { 1.0 } else { 0.0 } } else { ((t - start) / a.length).clamp(0.0, 1.0) };
            if !a.out {
                return p_in;
            }
            // Going out: the first piece first, the last one gone as the clip ends.
            let leave = dur - a.length.max(0.0) - (n.saturating_sub(1) - i.min(n.saturating_sub(1))) as f32 * a.stagger.max(0.0);
            let p_out = if a.length <= 0.0 { if t < dur - (n.saturating_sub(1) - i.min(n.saturating_sub(1))) as f32 * a.stagger.max(0.0) { 1.0 } else { 0.0 } } else { (1.0 - (t - leave) / a.length).clamp(0.0, 1.0) };
            p_in.min(p_out)
        };
        Some(
            pieces
                .iter()
                .map(|&i| {
                    let p = progress(i);
                    match a.kind {
                        TextAnimKind::None => GlyphState { alpha: 1.0, dy: 0.0 },
                        TextAnimKind::Typewriter => GlyphState { alpha: if p > 0.0 { 1.0 } else { 0.0 }, dy: 0.0 },
                        TextAnimKind::FadeLetters | TextAnimKind::FadeWords => GlyphState { alpha: smooth(p), dy: 0.0 },
                        TextAnimKind::SlideLines => GlyphState { alpha: smooth(p), dy: (1.0 - smooth(p)) * 0.5 },
                        TextAnimKind::Bounce => GlyphState { alpha: (p * 5.0).min(1.0), dy: -(1.0 - bounce(p)) * 0.8 },
                    }
                })
                .collect(),
        )
    }

    /// [`Self::content_hash`] including the animation's state at `t` seconds
    /// into a clip `dur` long (quantised: frames that look the same share it).
    pub fn content_hash_at(&self, frame: [u32; 2], t: f32, dur: f32) -> u64 {
        use std::hash::{Hash, Hasher};
        let base = self.content_hash(frame);
        let Some(states) = self.glyph_states(t, dur) else { return base };
        let mut h = std::collections::hash_map::DefaultHasher::new();
        base.hash(&mut h);
        for g in states {
            ((g.alpha * 255.0).round() as i32, (g.dy * self.size * 8.0).round() as i32).hash(&mut h);
        }
        h.finish()
    }

    /// A stable hash of everything that affects the picture (plus the frame size).
    pub fn content_hash(&self, frame: [u32; 2]) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        frame.hash(&mut h);
        self.text.hash(&mut h);
        self.font.hash(&mut h);
        for f in [
            self.size,
            self.line_spacing,
            self.letter_spacing,
            self.outline,
            self.shadow_distance,
            self.shadow_angle,
            self.shadow_softness,
            self.background_padding,
            self.extrude,
            self.bevel,
        ] {
            f.to_bits().hash(&mut h);
        }
        (self.color, self.outline_color, self.shadow_color, self.background_color).hash(&mut h);
        (self.align, self.shadow, self.background, self.side_color, self.bevel_style).hash(&mut h);
        self.shape.as_ref().map(|s| s.id).hash(&mut h);
        self.model.hash(&mut h);
        if !self.anim.is_none() {
            // (Its state at a moment is in `content_hash_at`.)
            (self.anim.kind, self.anim.stagger.to_bits(), self.anim.length.to_bits(), self.anim.delay.to_bits(), self.anim.out).hash(&mut h);
        }
        h.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_scale_with_the_frame_and_round_trip() {
        let t = TitleSpec::for_frame(2160);
        assert_eq!(t.size, 192.0);
        let json = serde_json::to_string(&t).unwrap();
        let back: TitleSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(t, back);
        // Old/minimal files fill in the rest.
        let min: TitleSpec = serde_json::from_str(r#"{"text":"Hi","size":40}"#).unwrap();
        assert_eq!((min.color, min.align, min.line_spacing), ([255; 4], TextAlign::Center, 1.0));
    }

    #[test]
    fn hash_changes_with_anything_visible() {
        let a = TitleSpec::default();
        let mut b = a.clone();
        assert_eq!(a.content_hash([1920, 1080]), b.content_hash([1920, 1080]));
        b.outline = 2.0;
        assert_ne!(a.content_hash([1920, 1080]), b.content_hash([1920, 1080]));
        assert_ne!(a.content_hash([1920, 1080]), a.content_hash([1280, 720]));
        let deep = TitleSpec { extrude: 40.0, ..a.clone() };
        assert!(deep.is_3d() && !a.is_3d());
        assert_ne!(a.content_hash([1920, 1080]), deep.content_hash([1920, 1080]));
        let sides = TitleSpec { side_color: Some([255, 0, 0, 255]), ..deep.clone() };
        assert_ne!(sides.content_hash([1920, 1080]), deep.content_hash([1920, 1080]));
        assert_eq!(TitleSpec { text: "\n  Hello world \nmore".into(), ..a }.label(), "Hello world");
    }
}

#[cfg(test)]
mod anim_tests {
    use super::*;

    fn spec(text: &str, kind: TextAnimKind) -> TitleSpec {
        TitleSpec { text: text.into(), anim: TextAnim::preset(kind), ..TitleSpec::default() }
    }

    #[test]
    fn pieces_are_letters_words_or_lines() {
        // Letters: spaces take the next letter's turn.
        assert_eq!(spec("ab c", TextAnimKind::Typewriter).anim_pieces(), (vec![0, 1, 2, 2], 3));
        assert_eq!(spec("hi you\nall", TextAnimKind::FadeWords).anim_pieces(), (vec![0, 0, 1, 1, 1, 1, 2, 2, 2], 3));
        assert_eq!(spec("one\n\ntwo", TextAnimKind::SlideLines).anim_pieces(), (vec![0, 0, 0, 1, 1, 1], 2));
    }

    #[test]
    fn states_follow_the_clock() {
        let s = spec("abc", TextAnimKind::Typewriter);
        let shown = |t: f32| s.glyph_states(t, 5.0).unwrap().iter().filter(|g| g.alpha > 0.0).count();
        assert_eq!((shown(0.0), shown(0.07), shown(0.13), shown(3.0)), (1, 2, 3, 3));
        // Fading words: halfway through the first word's fade, it's half there.
        let mut w = spec("one two", TextAnimKind::FadeWords);
        w.anim.length = 1.0;
        w.anim.stagger = 1.0;
        let g = w.glyph_states(0.5, 10.0).unwrap();
        assert!((g[0].alpha - 0.5).abs() < 1e-5 && g[4].alpha == 0.0);
        // Sliding lines start half an em low and arrive.
        let l = spec("a\nb", TextAnimKind::SlideLines);
        let g = l.glyph_states(0.0, 5.0).unwrap();
        assert_eq!((g[0].dy, g[0].alpha), (0.5, 0.0));
        let g = l.glyph_states(2.0, 5.0).unwrap();
        assert!(g.iter().all(|g| g.dy == 0.0 && g.alpha == 1.0));
        // Bouncing letters drop from above and settle.
        let b = spec("ab", TextAnimKind::Bounce);
        let g = b.glyph_states(0.0, 5.0).unwrap();
        assert!(g[0].dy < -0.7);
        assert_eq!(b.glyph_states(2.0, 5.0).unwrap()[1].dy, 0.0);
        // Going out too: gone at the end, first piece first.
        let mut o = spec("ab", TextAnimKind::FadeLetters);
        o.anim.out = true;
        let g = o.glyph_states(5.0, 5.0).unwrap();
        assert!(g.iter().all(|g| g.alpha == 0.0));
        let g = o.glyph_states(4.6, 5.0).unwrap();
        assert!(g[0].alpha < g[1].alpha, "{g:?}");
        // No animation: no states, and the picture's hash doesn't move with time.
        let none = TitleSpec::default();
        assert!(none.glyph_states(1.0, 5.0).is_none());
        assert_eq!(none.content_hash_at([100, 100], 0.0, 5.0), none.content_hash_at([100, 100], 3.0, 5.0));
        assert_ne!(s.content_hash_at([100, 100], 0.0, 5.0), s.content_hash_at([100, 100], 3.0, 5.0));
        assert_eq!(s.content_hash_at([100, 100], 3.0, 5.0), s.content_hash_at([100, 100], 4.0, 5.0), "settled: the same picture");
        // Round trip, and older files without it.
        let j = serde_json::to_string(&b).unwrap();
        assert_eq!(serde_json::from_str::<TitleSpec>(&j).unwrap(), b);
        assert!(!serde_json::to_string(&none).unwrap().contains("anim"));
    }
}
