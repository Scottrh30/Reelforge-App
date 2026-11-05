//! The parts of Reelforge that don't depend on a user interface toolkit:
//! the editor model ([`editor::Editor`]), playback and audio output, the
//! viewer picture, the render cache, proxies, autosave and crash recovery,
//! meters, export presets, plugins, Collect project and the viewer gizmo.
//! The app (`crates/tauri-app`, the `reelforge` program) is built on these.

pub mod audio;
pub mod collect;
pub mod disk;
pub mod editor;
pub mod fx_presets;
pub mod gizmo;
pub mod meters;
pub mod playback;
pub mod plugins;
pub mod presets;
pub mod preview;
pub mod proxies;
pub mod render_cache;
pub mod safety;
pub mod snap;
pub mod start;
