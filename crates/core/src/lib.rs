//! Engine-agnostic core of Reelforge.
//!
//! Nothing in here knows about FFmpeg, wgpu or the interface. Keeping the project model
//! pure makes it easy to test, serialize, and drive from a headless renderer later.

// Never crash: no unwrap, expect, panic, unreachable, todo or unimplemented
// outside tests (a damaged file or an odd setting is an error, not a crash).
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

// (numeric code indexes several arrays in step; GPU-free maths helpers take
// their inputs plainly; a few nested return types read better spelled out)
#![allow(clippy::needless_range_loop, clippy::too_many_arguments, clippy::type_complexity)]

pub mod adobe;
pub mod colormatch;
pub mod neonpath;
pub mod audiovis;
pub mod anim;
pub mod attrs;
pub mod audio;
pub mod audiofx;
pub mod audiokeys;
pub mod autostab;
pub mod camtrack;
pub mod deflicker;
pub mod camtrack_io;
pub mod duck;
pub mod channels;
pub mod clipboard;
pub mod collect;
pub mod edit;
pub mod disintegrate;
pub mod lightning;
pub mod liquify;
pub mod sparkle;
pub mod inpaint;
pub mod fillstore;
pub mod faces;
pub mod rotobrush;
pub mod paint;
pub mod template;
pub mod effects;
pub mod environment;
pub mod fluid;
pub mod water;
pub mod frame;
pub mod imageseq;
pub mod interchange;
pub mod depth;
pub mod hud_text;
pub mod lut;
pub mod mask;
pub mod matte;
pub mod ofx;
pub mod particles;
pub mod plugin;
pub mod project;
pub mod speed;
pub mod time;
pub mod title;
pub mod meshtrack;
pub mod meshwarp;
pub mod multicam;
pub mod planar;
pub mod shatter;
pub mod space3d;
pub mod track;
pub mod transform;
pub mod transition;
pub mod undo;

pub use anim::{Curve, Interp, Key, Prop, TransformAnim};
pub use audio::{ClipAudio, PanKey, TrackAudio, VolumeKey};
pub use channels::Channel;
pub use edit::{TrimMode, TrimPlan};
pub use effects::{Effect, EffectCategory, EffectData, EffectKind, EffectValues, Param, ParamSpec, ParamUi};
pub use project::*;
pub use time::{FrameRate, Rational, Time};
pub use title::{is_model_file, BevelStyle, ModelSource, Shape, ShapePath, MODEL_EXTENSIONS, FontChoice, GlyphState, TextAlign, TextAnim, TextAnimKind, TextAnimator, TextCase, TextPath, GlyphMod, RangeSelector, SelectorShape, SelectorUnits, TitleSpec};
pub use transform::{Affine2, BlendMode, Transform};
pub use transition::Transition;
pub use undo::{Command, UndoStack};
