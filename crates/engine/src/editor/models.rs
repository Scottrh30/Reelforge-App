//! Relinking 3D model layers: a model file that moved, and texture files that
//! moved away from their model (found by name; the folder they're in is kept
//! with the layer, so the model reads them from there).

use super::Editor;
use reelforge_core::undo::{Batch, Command, SetTitle};
use reelforge_core::ClipId;
use std::path::{Path, PathBuf};

/// A 3D model layer: which sequence, which clip, and the file it shows.
struct ModelUse {
    seq: usize,
    clip: ClipId,
    title: reelforge_core::TitleSpec,
    path: PathBuf,
}

impl Editor {
    /// Every 3D model layer, in every sequence.
    fn model_uses(&self) -> Vec<ModelUse> {
        let mut v = Vec::new();
        for (seq, s) in self.project.sequences.iter().enumerate() {
            for c in s.tracks.iter().flat_map(|t| &t.clips) {
                if let Some(t) = c.title.as_ref().filter(|t| t.model.is_some()) {
                    let path = t.model.as_ref().map(|m| m.path.clone()).unwrap_or_default();
                    v.push(ModelUse { seq, clip: c.id, title: t.clone(), path });
                }
            }
        }
        v
    }

    /// Model files not where the project says (by path, each once).
    pub fn missing_models(&self) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = self.model_uses().into_iter().map(|u| u.path).filter(|p| !p.exists()).collect();
        v.sort();
        v.dedup();
        v
    }

    /// Point the model layer `id` (and every layer showing the same missing
    /// file) at `new_path`; then look in that folder for any other missing
    /// model files and missing textures by name.
    pub fn relink_model(&mut self, id: ClipId, new_path: PathBuf) {
        let Some(old) = self.model_uses().into_iter().find(|u| u.clip == id).map(|u| u.path) else { return };
        let mut pairs = vec![(old, new_path.clone())];
        if let Some(dir) = new_path.parent() {
            let others: Vec<PathBuf> = self.missing_models().into_iter().filter(|p| *p != pairs[0].0).collect();
            pairs.extend(crate::safety::find_by_name(dir, &others));
        }
        let dir = new_path.parent().map(Path::to_path_buf);
        let (models, textures) = self.relink_models_in(pairs, dir.as_deref());
        self.status = relink_note(models, textures);
    }

    /// Look in `dir` (and its subfolders) for the missing textures of model
    /// layer `id`.
    pub fn find_model_textures(&mut self, id: ClipId, dir: &Path) {
        let (_, textures) = self.relink_textures(dir, Some(id));
        self.status = if textures == 0 { format!("None of its missing textures are in {}", dir.display()) } else { relink_note(0, textures) };
    }

    /// For the Media panel's folder search: missing model files and textures
    /// in `dir`. How many model files and textures were found.
    pub(crate) fn find_missing_models_in(&mut self, dir: &Path) -> (usize, usize) {
        let pairs = crate::safety::find_by_name(dir, &self.missing_models());
        self.relink_models_in(pairs, Some(dir))
    }

    /// Repoint the model layers on the (old, new) file pairs, then look in
    /// `dir` for missing textures. One undo step.
    fn relink_models_in(&mut self, pairs: Vec<(PathBuf, PathBuf)>, dir: Option<&Path>) -> (usize, usize) {
        self.commit_live_edit();
        let mut cmds: Vec<Box<dyn Command>> = Vec::new();
        let mut models = 0;
        for (old, new) in &pairs {
            for u in self.model_uses().into_iter().filter(|u| u.path == *old) {
                let mut t = u.title.clone();
                if let Some(m) = t.model.as_mut() {
                    m.path = new.clone();
                }
                cmds.push(Box::new(SetTitle { seq: u.seq, id: u.clip, old: u.title, new: t }));
            }
            models += 1;
        }
        if !cmds.is_empty() {
            self.execute(Box::new(Batch { name: "Relink 3D model".into(), cmds }));
        }
        let textures = dir.map_or(0, |d| self.relink_textures(d, None).1);
        (models, textures)
    }

    /// Find the missing textures of every model layer (or just `only`) in
    /// `dir` by name, and keep the folders they're in with those layers. One
    /// undo step. (Layers changed, textures found.)
    fn relink_textures(&mut self, dir: &Path, only: Option<ClipId>) -> (usize, usize) {
        self.commit_live_edit();
        let mut cmds: Vec<Box<dyn Command>> = Vec::new();
        let mut found_total = std::collections::HashSet::new();
        for u in self.model_uses().into_iter().filter(|u| only.is_none_or(|o| o == u.clip)) {
            let Some(src) = u.title.model.as_ref() else { continue };
            let Ok(m) = reelforge_render::model3d::cached(src) else { continue };
            if m.missing_textures.is_empty() {
                continue;
            }
            let wanted: Vec<PathBuf> = m.missing_textures.iter().map(|t| PathBuf::from(t.rsplit(['/', '\\']).next().unwrap_or(t))).collect();
            let found = crate::safety::find_by_name(dir, &wanted);
            if found.is_empty() {
                continue;
            }
            let mut t = u.title.clone();
            if let Some(ms) = t.model.as_mut() {
                for (name, at) in &found {
                    found_total.insert(name.clone());
                    if let Some(folder) = at.parent().filter(|f| !ms.texture_dirs.iter().any(|d| d == f)) {
                        ms.texture_dirs.push(folder.to_path_buf());
                    }
                }
            }
            if t != u.title {
                cmds.push(Box::new(SetTitle { seq: u.seq, id: u.clip, old: u.title, new: t }));
            }
        }
        let n = cmds.len();
        if n > 0 {
            self.execute(Box::new(Batch { name: "Find model textures".into(), cmds }));
        }
        (n, found_total.len())
    }
}

fn relink_note(models: usize, textures: usize) -> String {
    let plural = |n: usize, w: &str| format!("{n} {w}{}", if n == 1 { "" } else { "s" });
    match (models, textures) {
        (0, 0) => "Nothing to relink there".into(),
        (m, 0) => format!("Relinked {}", plural(m, "3D model file")),
        (0, t) => format!("Found {}", plural(t, "texture")),
        (m, t) => format!("Relinked {} and found {}", plural(m, "3D model file"), plural(t, "texture")),
    }
}
