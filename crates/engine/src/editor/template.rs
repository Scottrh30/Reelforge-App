//! Motion graphics templates in the editor (see `reelforge_core::template`).

use super::{Editor, LOCKED};
use reelforge_core::template::{self, Control, SetClip, SetControls, Target, TemplateFile, Value};
use reelforge_core::undo::Batch;
use reelforge_core::*;
use std::path::Path;

impl Editor {
    /// Expose `what` of clip `id` (in the sequence being edited) as a
    /// control of this composition, shown on the clips that play it.
    pub fn expose_control(&mut self, id: ClipId, what: Target, label: Option<String>) {
        self.commit_live_edit();
        let Some((_, c)) = self.seq().find_clip(id) else { return };
        if what.get(c).is_none() {
            self.status = "That isn't on this clip (text and its colour are a title's)".into();
            return;
        }
        let label = label.unwrap_or_else(|| what.describe(c));
        let old = self.seq().controls.clone();
        if old.iter().any(|k| k.clip == id && k.what == what) {
            self.status = format!("{label} is a control already");
            return;
        }
        let mut new = old.clone();
        new.push(Control { label: label.clone(), clip: id, what });
        self.execute(Box::new(SetControls { seq: self.active, old, new }));
        self.status = format!("{label} is a control of {}: it shows on the clips that play this composition (and in templates saved from it)", self.seq().name);
    }

    /// Rename (`Some`) or remove (`None`) control `index` of the sequence being edited.
    pub fn edit_control(&mut self, index: usize, label: Option<String>) {
        self.commit_live_edit();
        let old = self.seq().controls.clone();
        if index >= old.len() {
            return;
        }
        let mut new = old.clone();
        match label {
            Some(l) => new[index].label = l,
            None => {
                new.remove(index);
            }
        }
        self.execute(Box::new(SetControls { seq: self.active, old, new }));
    }

    /// Set control `index` of the composition clip `id` plays (one undo step).
    pub fn set_control(&mut self, id: ClipId, index: usize, value: Value) {
        self.commit_live_edit();
        if self.seq().is_locked(id) {
            self.status = LOCKED.into();
            return;
        }
        let Some(n) = self.seq().find_clip(id).and_then(|(_, c)| c.nested) else { return };
        let Some(k) = self.project.sequences.get(n).and_then(|s| s.controls.get(index)).cloned() else { return };
        let Some(old) = self.project.sequences[n].find_clip(k.clip).map(|(_, c)| c.clone()) else {
            self.status = format!("{}: its clip has gone from the composition", k.label);
            return;
        };
        let Some(new) = k.what.set(&old, &value) else { return };
        if new == old {
            return;
        }
        self.execute(Box::new(SetClip { seq: n, old, new }));
        self.status = format!("{}: set (in {}, which every clip playing it shows)", k.label, self.project.sequences[n].name);
    }

    /// Save the sequence being edited (and the compositions in it) as a template file.
    pub fn save_template(&mut self, path: &Path) {
        self.commit_live_edit();
        let t = template::make(&self.project, self.active);
        let text = match serde_json::to_string_pretty(&t) {
            Ok(t) => t,
            Err(e) => {
                self.status = format!("Couldn't save the template: {e}");
                return;
            }
        };
        match std::fs::write(path, text) {
            Ok(()) => {
                let n = self.seq().controls.len();
                self.status = format!("Saved template {} with {n} control{}", path.display(), if n == 1 { "" } else { "s" });
            }
            Err(e) => self.status = format!("Couldn't save the template: {e}"),
        }
    }

    /// Add the template in `path` to the project (a composition of its own)
    /// and put it in the sequence being edited at the playhead.
    pub fn add_template(&mut self, path: &Path) {
        self.commit_live_edit();
        let t: TemplateFile = match std::fs::read_to_string(path).map_err(|e| e.to_string()).and_then(|s| serde_json::from_str(&s).map_err(|e| e.to_string())) {
            Ok(t) => t,
            Err(e) => {
                self.status = format!("Couldn't read the template: {e}");
                return;
            }
        };
        if t.sequences.is_empty() || t.format > template::FORMAT {
            self.status = "That template is empty, or from a newer Reelforge".into();
            return;
        }
        let added = template::add(&mut self.project, &t);
        let index = added.index;
        self.execute(Box::new(Batch { name: format!("Add template {}", t.name), cmds: added.cmds }));
        self.add_composite_to_timeline(index);
        let n = self.project.sequences[index].controls.len();
        self.status = format!("Added template {} at the playhead: set its {n} control{} in the Controls panel", t.name, if n == 1 { "" } else { "s" });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controls_are_exposed_saved_added_and_set_per_copy() {
        let mut e = Editor::new(None, None);
        // A lower third: a title in a composition, its text a control.
        let comp = e.project.sequences.len();
        let mut s = reelforge_core::Sequence::new("Lower third", 320, 180, e.seq().frame_rate);
        s.tracks.retain(|t| t.kind == TrackKind::Video);
        let id = ClipId(e.project.alloc_id());
        s.tracks[0].clips.push(Clip::new_title(id, Time::ZERO, Time::from_secs_f64(2.0), reelforge_core::title::TitleSpec { text: "Name".into(), ..Default::default() }));
        e.project.sequences.push(s);
        e.open_sequence(comp);
        e.expose_control(id, Target::Text, None);
        e.expose_control(id, Target::Opacity, Some("Fade".into()));
        assert_eq!(e.seq().controls.len(), 2, "{}", e.status);
        let file = std::env::temp_dir().join(format!("rf-template-{}.rftemplate", std::process::id()));
        e.save_template(&file);
        e.open_sequence(0);
        e.add_template(&file);
        e.add_template(&file);
        let copies: Vec<usize> = (0..e.project.sequences.len()).filter(|&i| e.project.sequences[i].name == "Lower third").collect();
        assert_eq!(copies.len(), 3, "the original and two copies");
        let clips: Vec<ClipId> = e.seq().tracks.iter().flat_map(|t| &t.clips).filter(|c| c.nested.is_some()).map(|c| c.id).collect();
        assert_eq!(clips.len(), 2, "{}", e.status);
        e.set_control(clips[0], 0, Value::Text("Grace Hopper".into()));
        let text = |e: &Editor, clip: ClipId| {
            let n = e.seq().find_clip(clip).unwrap().1.nested.unwrap();
            e.project.sequences[n].tracks[0].clips[0].title.as_ref().unwrap().text.clone()
        };
        assert_eq!(text(&e, clips[0]), "Grace Hopper");
        assert_eq!(text(&e, clips[1]), "Name", "each copy has its own");
        e.undo();
        assert_eq!(text(&e, clips[0]), "Name");
        let _ = std::fs::remove_file(&file);
    }
}
