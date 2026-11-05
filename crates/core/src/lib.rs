//! Engine-agnostic core of Reelforge.
//!
//! Nothing in here knows about FFmpeg, wgpu or the interface. Keeping the project model
//! pure makes it easy to test, serialize, and drive from a headless renderer later.

pub mod anim;
pub mod attrs;
pub mod audio;
pub mod audiofx;
pub mod camtrack;
pub mod camtrack_io;
pub mod duck;
pub mod channels;
pub mod clipboard;
pub mod collect;
pub mod edit;
pub mod effects;
pub mod frame;
pub mod imageseq;
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
pub mod planar;
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
pub use title::{BevelStyle, Shape, ShapePath, FontChoice, GlyphState, TextAlign, TextAnim, TextAnimKind, TitleSpec};
pub use transform::{Affine2, BlendMode, Transform};
pub use transition::Transition;
pub use undo::{Command, UndoStack};
