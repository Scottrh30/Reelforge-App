//! Engine-agnostic core of Reelforge.
//!
//! Nothing in here knows about FFmpeg, wgpu or egui. Keeping the project model
//! pure makes it easy to test, serialize, and drive from a headless renderer later.

pub mod anim;
pub mod audio;
pub mod channels;
pub mod edit;
pub mod effects;
pub mod project;
pub mod time;
pub mod transform;
pub mod undo;

pub use anim::{Curve, Interp, Key, Prop, TransformAnim};
pub use audio::{ClipAudio, TrackAudio, VolumeKey};
pub use channels::Channel;
pub use edit::{TrimMode, TrimPlan};
pub use effects::{Effect, EffectKind, EffectValues, Param, ParamSpec};
pub use project::*;
pub use time::{FrameRate, Rational, Time};
pub use transform::{Affine2, BlendMode, Transform};
pub use undo::{Command, UndoStack};
