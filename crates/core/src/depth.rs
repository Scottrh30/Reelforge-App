//! The colour bit depth renders work at (Settings → Renders → Default colour bit
//! depth): how precisely the frame is composited, and whether layers blend in linear
//! light or in display (gamma-encoded) values.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ColorDepth {
    /// 8 bits per channel, blending gamma-encoded values: what older editors do. Fastest
    /// and smallest; nothing above white survives, and smooth gradients can band.
    Int8,
    /// 16-bit float, blending gamma-encoded values (as 8-bit looks, with more precision
    /// and highlights above white kept).
    Half,
    /// 16-bit float in linear light: blends, dissolves and glows behave as light does.
    /// The standard.
    #[default]
    HalfLinear,
    /// 32-bit float, blending gamma-encoded values.
    Float,
    /// 32-bit float in linear light: the most precise, for VFX and grading.
    FloatLinear,
}

impl ColorDepth {
    pub const ALL: [ColorDepth; 5] = [ColorDepth::Int8, ColorDepth::Half, ColorDepth::HalfLinear, ColorDepth::Float, ColorDepth::FloatLinear];

    pub fn label(self) -> &'static str {
        match self {
            ColorDepth::Int8 => "8-bit Integer",
            ColorDepth::Half => "16-bit Float",
            ColorDepth::HalfLinear => "16-bit Float (Linear Color)",
            ColorDepth::Float => "32-bit Float",
            ColorDepth::FloatLinear => "32-bit Float (Linear Color)",
        }
    }

    /// Layers blend in linear light (else in gamma-encoded values).
    pub fn linear(self) -> bool {
        matches!(self, ColorDepth::HalfLinear | ColorDepth::FloatLinear)
    }

    /// Bits per channel.
    pub fn bits(self) -> u32 {
        match self {
            ColorDepth::Int8 => 8,
            ColorDepth::Half | ColorDepth::HalfLinear => 16,
            ColorDepth::Float | ColorDepth::FloatLinear => 32,
        }
    }

    /// The nearest at 16 bits (where a graphics card can't do 32-bit float).
    pub fn at_most_half(self) -> ColorDepth {
        match self {
            ColorDepth::Float => ColorDepth::Half,
            ColorDepth::FloatLinear => ColorDepth::HalfLinear,
            d => d,
        }
    }
}
