//! Engine-agnostic core of Reelforge.
//!
//! Nothing in here knows about FFmpeg, wgpu or egui. Keeping the project model
//! pure makes it easy to test, serialize, and drive from a headless renderer later.

pub mod project;
pub mod time;
pub mod undo;

pub use project::*;
pub use time::{FrameRate, Rational, Time};
pub use undo::{Command, UndoStack};
