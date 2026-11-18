//! Replace footage: swap a media file for another and keep every edit,
//! effect, mask, keyframe and track that uses it. Everywhere the media is
//! used (the Media panel), or just one clip and the clips linked to it (the
//! timeline). Optionally the new footage is laid out at the original's size,
//! so a 4K replacement for an HD shot doesn't come out twice as big.

use super::Editor;
use reelforge_core::undo::{Batch, Command, ImportMedia, RelinkMedia, SetClipMedia};
use reelforge_core::{ClipId, MediaAsset, MediaId, Time};
use std::path::{Path, PathBuf};

impl Editor {
    /// Swap media `id`'s file for `path` wherever it's used.
    pub fn replace_footage(&mut self, id: MediaId, path: &Path, keep_size: bool) {
        self.commit_live_edit();
        let Some(old) = self.project.media(id).cloned() else { return };
        let new = match replacement(&old, id, path, keep_size, self.seq().frame_rate.0) {
            Ok(n) => n,
            Err(e) => {
                self.status = e;
                return;
            }
        };
        let notes = self.replace_notes(&[id], &new);
        let name = new.name.clone();
        self.execute(Box::new(Batch { name: "Replace footage".into(), cmds: vec![Box::new(RelinkMedia { id, old: old.clone(), new: new.clone() }) as Box<dyn Command>] }));
        self.after_replace(&new);
        self.status = format!("Replaced {} with {name} everywhere it's used{notes}", old.name);
    }

    /// Swap the footage of clip `clip` (and the clips linked to it) for
    /// `path`, leaving other clips of the same media as they are.
    pub fn replace_clip_footage(&mut self, clip: ClipId, path: &Path, keep_size: bool) {
        self.commit_live_edit();
        let group: Vec<(ClipId, MediaId)> = self.seq().link_group(clip).iter().map(|(_, c)| (c.id, c.media)).collect();
        let Some(&(_, media)) = group.iter().find(|(id, _)| *id == clip) else { return };
        if self.seq().is_locked(clip) {
            self.status = "The clip is on a locked track".into();
            return;
        }
        let Some(old) = self.project.media(media).cloned() else {
            self.status = "Only footage clips can have their footage replaced".into();
            return;
        };
        let new_id = MediaId(self.project.alloc_id());
        let new = match replacement(&old, new_id, path, keep_size, self.seq().frame_rate.0) {
            Ok(n) => n,
            Err(e) => {
                self.status = e;
                return;
            }
        };
        let notes = self.replace_notes(&group.iter().map(|g| g.1).collect::<Vec<_>>(), &new);
        // The same file laid out the same way already in the project: use that.
        let existing = self.project.media.iter().find(|m| m.path == new.path && m.conform == new.conform).map(|m| m.id);
        let to = existing.unwrap_or(new_id);
        let mut cmds: Vec<Box<dyn Command>> = Vec::new();
        if existing.is_none() {
            cmds.push(Box::new(ImportMedia(new.clone())));
        }
        cmds.push(Box::new(SetClipMedia { seq: self.active, clips: group.iter().filter(|(_, m)| *m == media).map(|(id, m)| (*id, *m, to)).collect() }));
        self.execute(Box::new(Batch { name: "Replace footage".into(), cmds }));
        if existing.is_none() {
            self.after_replace(&new);
        }
        self.status = format!("This clip now shows {} instead of {}{notes}", new.name, old.name);
    }

    /// What the swap might change, for the status line: clips running past
    /// the new file's end, sound or picture it hasn't got.
    fn replace_notes(&self, media: &[MediaId], new: &MediaAsset) -> String {
        let mut past = 0;
        let (mut wants_sound, mut wants_picture) = (false, false);
        for s in &self.project.sequences {
            for t in &s.tracks {
                for c in t.clips.iter().filter(|c| media.contains(&c.media)) {
                    match t.kind {
                        reelforge_core::TrackKind::Audio => wants_sound = true,
                        reelforge_core::TrackKind::Video => wants_picture = true,
                    }
                    if new.info.duration > Time::ZERO && reelforge_core::speed::media_span(c).1 > new.info.duration + Time::from_secs_f64(0.02) {
                        past += 1;
                    }
                }
            }
        }
        let mut notes = Vec::new();
        if past > 0 {
            notes.push(format!("{past} clip{} run{} past the end of the new file (they hold its last frame)", if past == 1 { "" } else { "s" }, if past == 1 { "s" } else { "" }));
        }
        if wants_sound && new.info.audio.is_none() {
            notes.push("it has no sound, so its sound clips are silent".into());
        }
        if wants_picture && new.info.video.is_none() {
            notes.push("it has no picture".into());
        }
        if notes.is_empty() { String::new() } else { format!(": {}", notes.join("; ")) }
    }

    fn after_replace(&mut self, new: &MediaAsset) {
        self.preview.reset(); // reopen decoders on the new file
        self.compute_waveform(new);
    }
}

/// The new media for `old`'s place: probed, named after the file, and laid out
/// at `old`'s size (fitted to the new shape) if `keep_size`.
fn replacement(old: &MediaAsset, id: MediaId, path: &Path, keep_size: bool, rate: reelforge_core::time::Rational) -> Result<MediaAsset, String> {
    let path: PathBuf = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    // A frame of a numbered image sequence brings in the whole sequence.
    let (path, name) = match reelforge_core::imageseq::detect(&path, rate) {
        Some(seq) => (seq.media_path(), seq.name()),
        None => (path.clone(), path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()),
    };
    let info = reelforge_media::probe(&path).map_err(|e| format!("Couldn't use {name}: {e:#}"))?;
    let conform = match (keep_size, old.layer_size(), info.video.as_ref()) {
        (true, Some([ow, oh]), Some(v)) if [v.width, v.height] != [ow, oh] && v.width > 0 && v.height > 0 => {
            let k = (ow as f64 / v.width as f64).min(oh as f64 / v.height as f64);
            Some([((v.width as f64 * k).round() as u32).max(1), ((v.height as f64 * k).round() as u32).max(1)])
        }
        _ => None,
    };
    Ok(MediaAsset { id, path, name, info, conform })
}

#[cfg(test)]
mod tests {
    use super::*;
    use reelforge_core::effects::{Effect, EffectKind};

    fn video(dir: &Path, name: &str, size: &str, secs: u32) -> Option<PathBuf> {
        let p = dir.join(name);
        let ok = std::process::Command::new("ffmpeg")
            .args(["-y", "-loglevel", "error", "-f", "lavfi", "-i", &format!("testsrc2=size={size}:rate=25:duration={secs}"), "-pix_fmt", "yuv420p"])
            .arg(&p)
            .status()
            .is_ok_and(|s| s.success());
        ok.then_some(p)
    }

    /// Everywhere: the clips keep their place, effects and look, the new file
    /// is laid out at the old size, and it's one undo step. One clip: only it
    /// (and its linked clips) change.
    #[test]
    fn replace_footage_keeps_the_edit() {
        let dir = std::env::temp_dir().join(format!("rf-engine-replace-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (Some(a), Some(b)) = (video(&dir, "a.mp4", "160x90", 3), video(&dir, "b.mp4", "320x180", 1)) else { return };
        let mut e = Editor::new(None, None);
        e.import(std::slice::from_ref(&a));
        let m = e.project.media[0].id;
        e.add_to_timeline(m, Some(0), Some(Time::ZERO));
        e.add_to_timeline(m, Some(0), Some(Time::from_secs_f64(5.0)));
        let ids: Vec<ClipId> = e.seq().tracks[0].clips.iter().map(|c| c.id).collect();
        {
            let c = e.project.sequences[e.active].find_clip_mut(ids[0]).unwrap().1;
            c.effects.push(Effect::new(EffectKind::Blur));
            c.transform.scale = [0.5, 0.5];
        }
        let before = e.seq().clone();
        e.replace_footage(m, &b, true);
        let after = e.project.media(m).unwrap();
        assert_eq!(after.path, std::path::absolute(&b).unwrap());
        assert_eq!(after.name, "b.mp4");
        assert_eq!(after.layer_size(), Some([160, 90]), "laid out at the old size");
        let clips = |s: &reelforge_core::Sequence| s.tracks.iter().map(|t| t.clips.clone()).collect::<Vec<_>>();
        assert_eq!(clips(e.seq()), clips(&before), "every clip, effect and setting as it was");
        assert!(e.status.contains("past the end"), "the 3 s clips run past the 1 s file: {}", e.status);
        e.undo();
        assert_eq!(e.project.media(m).unwrap().path, std::path::absolute(&a).unwrap());
        e.replace_footage(m, &b, false);
        assert_eq!(e.project.media(m).unwrap().layer_size(), Some([320, 180]));
        e.undo();

        // Just the second clip.
        e.replace_clip_footage(ids[1], &b, true);
        let s = e.seq();
        let (c0, c1) = (s.find_clip(ids[0]).unwrap().1, s.find_clip(ids[1]).unwrap().1);
        assert_eq!(c0.media, m);
        assert_ne!(c1.media, m);
        assert_eq!(e.project.media(c1.media).unwrap().layer_size(), Some([160, 90]));
        // Its linked sound (if the file had any) goes along; there's none here.
        let n = e.project.media.len();
        e.undo();
        assert_eq!(e.seq().find_clip(ids[1]).unwrap().1.media, m);
        assert_eq!(e.project.media.len(), n - 1, "the new media goes too");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
