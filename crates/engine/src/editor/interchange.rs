//! Export the edit for another editor (EDL, FCPXML, Final Cut Pro 7 XML,
//! OpenTimelineIO): see `reelforge_core::interchange`.

use super::Editor;
use reelforge_core::interchange::{edit_of, Format, Source};
use std::path::Path;

impl Editor {
    /// Write the sequence being edited to `path` as `format`. Returns a
    /// line for the status bar saying what didn't go across.
    pub fn export_edit(&mut self, format: Format, path: &Path) -> Result<String, String> {
        self.commit_live_edit();
        let text = format.write(&self.project, self.active);
        crate::safety::write_atomic(path, text.as_bytes()).map_err(|e| format!("couldn't write {}: {e}", path.display()))?;
        let e = edit_of(&self.project, self.active);
        let items = || e.lanes.iter().flat_map(|l| &l.items);
        let made = items().filter(|i| !matches!(i.source, Source::File { .. })).count();
        let mut notes = Vec::new();
        if made > 0 {
            notes.push(format!("{made} title{}, solid{} or composition{} left as gaps", if made == 1 { "" } else { "s" }, if made == 1 { "" } else { "s" }, if made == 1 { "" } else { "s" }));
        }
        if format == Format::Edl {
            let videos = e.lanes.iter().filter(|l| l.kind == reelforge_core::TrackKind::Video && !l.items.is_empty()).count();
            if videos > 1 {
                notes.push("an EDL holds one video track: Video 1 went".into());
            }
        }
        let name = path.file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned());
        self.status = format!("Exported {} as {name}{}", self.seq().name, if notes.is_empty() { String::new() } else { format!(" ({})", notes.join("; ")) });
        Ok(self.status.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_the_edit_and_says_what_stayed_behind() {
        let dir = std::env::temp_dir().join(format!("rf-interchange-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut e = Editor::new(None, None);
        e.add_title();
        let path = dir.join("cut.otio");
        let msg = e.export_edit(Format::Otio, &path).unwrap();
        assert!(msg.contains("1 title, solid or composition left as gaps"), "{msg}");
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(v["OTIO_SCHEMA"], "Timeline.1");
        assert!(e.export_edit(Format::Edl, &dir.join("no/such/dir/cut.edl")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
