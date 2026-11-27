//! Bringing in tracks and masks made in Trackwright (see
//! `reelforge_core::trackwright`): a planar track becomes the clip's tracker,
//! a roto shape becomes an animated mask.

use super::{Editor, LOCKED};
use reelforge_core::trackwright::{self as tw, Import};
use reelforge_core::undo::{SetClipVisual, SetTracker};
use reelforge_core::*;
use std::path::Path;

impl Editor {
    /// Read a Trackwright `.rftrack.json` or `.rfmask.json` from `path` onto
    /// footage clip `id`. One undo step.
    pub fn import_trackwright_file(&mut self, id: ClipId, path: &Path) {
        self.commit_live_edit();
        let file = path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
        if self.tracking.is_some() {
            self.status = "Wait for the job running to finish (or stop it) first".into();
            return;
        }
        let Some(c) = self.seq().find_clip(id).map(|(_, c)| c.clone()) else {
            self.status = "Select the footage clip the track was made on, then import it".into();
            return;
        };
        if self.seq().is_locked(id) {
            self.status = LOCKED.into();
            return;
        }
        let media = self.project.media(c.media).filter(|_| c.title.is_none() && c.nested.is_none() && c.scene.is_none());
        let Some(rate) = media.and_then(|m| m.info.video.as_ref()).map(|v| v.frame_rate) else {
            self.status = "Select the footage clip the track was made on (a video or image sequence), then import it".into();
            return;
        };
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) => {
                self.status = format!("Couldn't read {}: {e}", path.display());
                return;
            }
        };
        let layer = self.layer_size(&c);
        let clip_name = c.display_name(&self.project).into_owned();
        match tw::read(&text, layer, Some(rate)) {
            Err(e) => self.status = format!("Couldn't import {file}: {e}"),
            Ok(Import::Track { name, tracker, frames, size_note }) => {
                let replaced = c.tracker.as_ref().is_some_and(|t| t.is_tracked());
                self.execute(Box::new(SetTracker { seq: self.active, id, old: c.tracker.clone(), new: Some(tracker), label: "Import Trackwright track" }));
                let note = size_note.unwrap_or_default();
                let was = if replaced { " (replacing its track)" } else { "" };
                self.status = format!("Imported {name} from Trackwright onto {clip_name}{was}: {frames} frames{note}. Under Apply tracking data, pick a layer to corner-pin to the surface, or stabilise");
            }
            Ok(Import::Mask { mask, frames, size_note }) => {
                let old = c.visual();
                let mut new = old.clone();
                let name = mask.name.clone();
                new.masks.push(mask);
                self.execute(Box::new(SetClipVisual { seq: self.active, id, old, new }));
                let note = size_note.unwrap_or_default();
                self.status = format!("Imported the {name} mask from Trackwright onto {clip_name}: keyed on {frames} frames{note}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::editor::Editor;
    use reelforge_core::track::TrackMode;
    use reelforge_core::*;

    fn footage(e: &mut Editor) -> ClipId {
        e.project.media = vec![MediaAsset {
            id: MediaId(1),
            path: "/x/plate.mp4".into(),
            name: String::new(),
            info: MediaInfo {
                duration: Time::from_secs_f64(4.0),
                video: Some(VideoStreamInfo { width: 960, height: 540, frame_rate: FrameRate::new(24, 1), codec: "h264".into(), time_base: Rational::new(1, 24), hdr: None, pixel_aspect: (1, 1) }),
                audio: None,
                container: String::new(),
            },
            conform: None,
        }];
        let s = Time::from_secs_f64;
        e.project.sequences[e.active].tracks[0].clips = vec![Clip::new(ClipId(10), MediaId(1), s(0.0), s(0.0), s(4.0), None)];
        ClipId(10)
    }

    /// A Trackwright track becomes the clip's planar tracker, scaled to the
    /// clip's size; a mask becomes a keyed mask; both undo; other files are refused.
    #[test]
    fn trackwright_files_come_in() {
        let mut e = Editor::new(None, None);
        let id = footage(&mut e);
        let dir = std::env::temp_dir().join(format!("rf_tw_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let frames: Vec<String> = (0..10)
            .map(|k| {
                let x = 100.0 + 4.0 * k as f64;
                format!(r#"{{"frame": {}, "time": {}, "corners": {{"tl": [{x},100], "tr": [{},100], "br": [{},500], "bl": [{x},500]}}, "confidence": {{"inliers": 50, "rms_px": 0.1}}}}"#, 1001 + k, k as f64 / 24.0, x + 800.0, x + 800.0)
            })
            .collect();
        let track = format!(r#"{{"format": "trackwright.reelforge.v1", "source": {{"path": "plate.mp4", "width": 1920, "height": 1080, "fps": 24, "first_frame": 1001}}, "layer": {{"name": "Sign"}}, "frames": [{}]}}"#, frames.join(","));
        std::fs::write(dir.join("sign.rftrack.json"), &track).unwrap();
        e.import_trackwright_file(id, &dir.join("sign.rftrack.json"));
        assert!(e.status.starts_with("Imported Sign from Trackwright"), "{}", e.status);
        let t = e.seq().find_clip(id).unwrap().1.tracker.clone().unwrap();
        assert_eq!(t.mode, TrackMode::Planar);
        let f = t.raw_frames();
        assert_eq!(f.len(), 10);
        assert_eq!(f[9].0, Time::from_frame(9, FrameRate::new(24, 1)));
        assert_eq!(f[9].1[0], [68.0, 50.0], "halved to the 960×540 clip");
        e.undo();
        assert!(e.seq().find_clip(id).unwrap().1.tracker.is_none());

        let mask = r#"{"format": "trackwright.mask.v1", "source": {"path": "plate.mp4", "width": 960, "height": 540, "fps": 24, "first_frame": 0},
          "mask": {"name": "Actor", "smooth": false, "feather_px": 3},
          "frames": [{"frame": 0, "points": [[1,1],[50,1],[50,50]], "polygon": []}, {"frame": 5, "points": [[11,1],[60,1],[60,50]], "polygon": []}]}"#;
        std::fs::write(dir.join("actor.rfmask.json"), mask).unwrap();
        e.import_trackwright_file(id, &dir.join("actor.rfmask.json"));
        assert!(e.status.contains("Actor mask"), "{}", e.status);
        let m = e.seq().find_clip(id).unwrap().1.masks.clone();
        assert_eq!(m.len(), 1);
        assert_eq!((m[0].keys.len(), m[0].feather), (2, 3.0));
        assert_eq!(m[0].keys[1].at, Time::from_frame(5, FrameRate::new(24, 1)));
        e.undo();
        assert!(e.seq().find_clip(id).unwrap().1.masks.is_empty());

        std::fs::write(dir.join("cam.json"), r#"{"format": "trackwright.forma3d.camera.v1"}"#).unwrap();
        e.import_trackwright_file(id, &dir.join("cam.json"));
        assert!(e.status.starts_with("Couldn't import cam.json"), "{}", e.status);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
