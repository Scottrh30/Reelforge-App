//! Project versions: keep, list, restore, open, rename and delete them.

use crate::{ed, Res};
use reelforge_engine::editor::versions::Version;
use serde::Serialize;

#[derive(Serialize)]
pub struct VersionsView {
    /// Where they're kept (`None`: the project isn't saved yet).
    pub folder: Option<String>,
    pub versions: Vec<Version>,
    /// Keep one on every save.
    pub on_save: bool,
}

#[tauri::command]
pub fn versions_list() -> Res<VersionsView> {
    ed(|e| VersionsView { folder: e.versions_folder().map(|p| p.display().to_string()), versions: e.versions(), on_save: e.prefs.versions_on_save })
}

/// Do `f`, putting an error in the status line too.
fn report(e: &mut reelforge_engine::editor::Editor, r: Result<(), String>) -> bool {
    match r {
        Ok(()) => true,
        Err(err) => {
            e.status = err;
            false
        }
    }
}

#[tauri::command]
pub fn save_version(name: String, note: String) -> Res<bool> {
    ed(|e| {
        let r = e.save_version(&name, &note).map(|_| ());
        report(e, r)
    })
}

#[tauri::command]
pub fn restore_version(file: String) -> Res<bool> {
    ed(|e| {
        let r = e.restore_version(&file);
        report(e, r)
    })
}

#[tauri::command]
pub fn open_version_copy(file: String) -> Res<bool> {
    ed(|e| {
        let r = e.open_version_copy(&file);
        report(e, r)
    })
}

#[tauri::command]
pub fn rename_version(file: String, name: String, note: String) -> Res<bool> {
    ed(|e| {
        let r = e.rename_version(&file, &name, &note);
        report(e, r)
    })
}

#[tauri::command]
pub fn delete_version(file: String) -> Res<bool> {
    ed(|e| {
        let r = e.delete_version(&file);
        report(e, r)
    })
}

#[tauri::command]
pub fn set_versions_on_save(on: bool) -> Res<()> {
    ed(|e| {
        e.prefs.versions_on_save = on;
        e.touch();
        crate::save_settings(e);
    })
}
