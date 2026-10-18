//! The parts of Reelforge that don't depend on a user interface toolkit:
//! the editor model ([`editor::Editor`]), playback and audio output, the
//! viewer picture, the render cache, proxies, autosave and crash recovery,
//! meters and export presets. The egui app (`reelforge`) and the Tauri app
//! (`reelforge-tauri`) are both built on these.

pub mod audio;
pub mod collect;
pub mod editor;
pub mod meters;
pub mod playback;
pub mod plugins;
pub mod presets;
pub mod preview;
pub mod proxies;
pub mod render_cache;
pub mod safety;
pub mod snap;
