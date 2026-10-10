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

    /// First line of the text, for labels.
    pub fn label(&self) -> String {
        let first = self.text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
        if first.is_empty() { "(empty title)".into() } else { first.to_string() }
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
        ] {
            f.to_bits().hash(&mut h);
        }
        (self.color, self.outline_color, self.shadow_color, self.background_color).hash(&mut h);
        (self.align, self.shadow, self.background).hash(&mut h);
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
        assert_eq!(TitleSpec { text: "\n  Hello world \nmore".into(), ..a }.label(), "Hello world");
    }
}
