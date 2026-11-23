//! The parts of Reelforge that don't depend on a user interface toolkit:
//! the editor model ([`editor::Editor`]), playback and audio output, the
//! viewer picture, the render cache, proxies, autosave and crash recovery,
//! meters, export presets, plugins, Collect project and the viewer gizmo.
//! The app (`crates/tauri-app`, the `reelforge` program) is built on these.

// Never crash: no unwrap, expect, panic, unreachable, todo or unimplemented
// outside tests (a damaged file or an odd setting is an error, not a crash).
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

// (as reelforge-core: numeric code indexing several arrays in step, plain
// argument lists, nested types spelled out)
#![allow(clippy::needless_range_loop, clippy::too_many_arguments, clippy::type_complexity)]

pub mod audio;
pub mod background;
pub mod camraw;
pub mod collect;
pub mod disk;
pub mod svg;
pub mod editor;
pub mod fx_presets;
pub mod gizmo;
pub mod gizmo3d;
pub mod hwflow;
pub mod meters;
pub mod playback;
pub mod plugins;
pub mod presets;
pub mod preview;
pub mod proxies;
pub mod render_cache;
pub mod safety;
pub mod scopes;
pub mod snap;
pub mod start;
