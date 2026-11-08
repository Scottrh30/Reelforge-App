//! Multicam: make a multicam source from takes, cut between its angles, and
//! the Viewer's angle grid.

use crate::{ed, Res};
use reelforge_core::multicam::MulticamSync;
use reelforge_core::{ClipId, MediaId};
use serde::Serialize;

/// The multicam clip at the playhead, for the Viewer.
#[derive(Serialize)]
pub struct MulticamView {
    pub clip: u64,
    pub angles: Vec<String>,
    /// The angle seen and the angle heard (0-based).
    pub video: u8,
    pub heard: u8,
    /// The sound is on an angle of its own (it doesn't follow the cuts).
    pub audio_fixed: bool,
    /// The Viewer shows the angle grid.
    pub grid: bool,
}

pub fn view(e: &reelforge_engine::editor::Editor) -> Option<MulticamView> {
    let at = e.multicam_at()?;
    Some(MulticamView { clip: at.clip.0, angles: at.angles, video: at.angle.video, heard: at.angle.heard(), audio_fixed: at.angle.audio.is_some(), grid: e.multicam_grid })
}

/// Make a multicam source of `media` (in angle order), lined up by `sync`
/// ("audio" or "start"), and put it at the playhead.
#[tauri::command]
pub fn create_multicam(media: Vec<u64>, sync: String) -> Res<()> {
    ed(|e| {
        let ids: Vec<MediaId> = media.into_iter().map(MediaId).collect();
        e.create_multicam(&ids, if sync == "start" { MulticamSync::Start } else { MulticamSync::Audio });
    })
}

/// Cut to angle `angle` (0-based) at the playhead.
#[tauri::command]
pub fn switch_angle(angle: u8, picture_only: bool) -> Res<()> {
    ed(|e| e.switch_angle(angle, picture_only))
}

/// Cut to the angle whose grid cell is at `x`, `y` (frame pixels).
#[tauri::command]
pub fn switch_angle_at(x: f32, y: f32, picture_only: bool) -> Res<()> {
    ed(|e| {
        if let Some(a) = e.grid_angle_at([x, y]) {
            e.switch_angle(a, picture_only);
        }
    })
}

/// Hear angle `audio` (0-based; `None`: whichever is seen) on clip `clip`.
#[tauri::command]
pub fn set_audio_angle(clip: u64, audio: Option<u8>) -> Res<()> {
    ed(|e| e.set_audio_angle(ClipId(clip), audio))
}

#[tauri::command]
pub fn set_multicam_grid(on: bool) -> Res<()> {
    ed(|e| e.set_multicam_grid(on))
}
