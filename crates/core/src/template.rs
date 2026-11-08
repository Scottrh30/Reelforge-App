//! Motion graphics templates: a composition saved to a file with some of
//! its settings exposed as controls (a title's text, a colour, an effect's
//! setting, opacity, position), added to any project as a clip of its own
//! copy, whose controls are then set from that clip.

use crate::effects::{EffectData, ParamUi};
use crate::project::{Clip, ClipId, MediaAsset, MediaId, Project, Sequence};
use crate::undo::{AddSequence, Command, ImportMedia};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// A setting inside a composition shown on the clips that play it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Control {
    pub label: String,
    /// The clip inside the composition it sets.
    pub clip: ClipId,
    pub what: Target,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Target {
    /// A title's text.
    Text,
    /// A title's fill colour.
    TextColour,
    /// Setting `param` of effect `effect`.
    Param { effect: usize, param: usize },
    Opacity,
    Position,
}

/// A control's value.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Value {
    Text(String),
    Number(f32),
    Pair([f32; 2]),
}

/// What kind of editor a control wants.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Kind {
    Text,
    Colour,
    Number,
    Choice,
    Toggle,
    Percent,
    Point,
}

impl Target {
    pub fn kind(self, c: &Clip) -> Kind {
        match self {
            Target::Text => Kind::Text,
            Target::TextColour => Kind::Colour,
            Target::Opacity => Kind::Percent,
            Target::Position => Kind::Point,
            Target::Param { effect, param } => match c.effects.get(effect).and_then(|e| e.kind.params().get(param)).map(|p| p.ui) {
                Some(ParamUi::Color) => Kind::Colour,
                Some(ParamUi::Choice(_)) => Kind::Choice,
                Some(ParamUi::Toggle) => Kind::Toggle,
                _ => Kind::Number,
            },
        }
    }

    /// A name for it, from what it sets.
    pub fn describe(self, c: &Clip) -> String {
        match self {
            Target::Text => "Text".into(),
            Target::TextColour => "Text colour".into(),
            Target::Opacity => "Opacity".into(),
            Target::Position => "Position".into(),
            Target::Param { effect, param } => c.effects.get(effect).map_or("Setting".into(), |e| format!("{}: {}", e.kind.label(), e.kind.params().get(param).map_or("?", |p| p.name))),
        }
    }

    /// Its value on `c` now.
    pub fn get(self, c: &Clip) -> Option<Value> {
        Some(match self {
            Target::Text => Value::Text(c.title.as_ref()?.text.clone()),
            Target::TextColour => {
                let k = c.title.as_ref()?.color;
                Value::Number(((k[0] as u32) << 16 | (k[1] as u32) << 8 | k[2] as u32) as f32)
            }
            Target::Opacity => Value::Number(c.transform.opacity * 100.0),
            Target::Position => Value::Pair(c.transform.position),
            Target::Param { effect, param } => Value::Number(c.effects.get(effect)?.params.get(param)?.value),
        })
    }

    /// `c` with it set to `v` (`None` if it doesn't apply).
    pub fn set(self, c: &Clip, v: &Value) -> Option<Clip> {
        let mut c = c.clone();
        match (self, v) {
            (Target::Text, Value::Text(t)) => c.title.as_mut()?.text = t.clone(),
            (Target::TextColour, Value::Number(n)) => {
                let n = n.round().clamp(0.0, 16777215.0) as u32;
                let t = c.title.as_mut()?;
                t.color = [(n >> 16) as u8, (n >> 8) as u8, n as u8, t.color[3]];
            }
            (Target::Opacity, Value::Number(n)) => c.transform.opacity = (n / 100.0).clamp(0.0, 1.0),
            (Target::Position, Value::Pair(p)) => c.transform.position = *p,
            (Target::Param { effect, param }, Value::Number(n)) => {
                let p = c.effects.get_mut(effect)?.params.get_mut(param)?;
                p.value = *n;
            }
            _ => return None,
        }
        Some(c)
    }
}

/// A template file.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TemplateFile {
    pub format: u32,
    pub name: String,
    /// The template's composition first, then any compositions inside it.
    pub sequences: Vec<Sequence>,
    /// The footage and pictures its clips use.
    pub media: Vec<MediaAsset>,
}

pub const FORMAT: u32 = 1;
pub const EXTENSION: &str = "rftemplate";

/// Composition `seq` of `p` (and those inside it) as a template.
pub fn make(p: &Project, seq: usize) -> TemplateFile {
    // The compositions reachable from it, root first.
    let mut order = vec![seq];
    let mut i = 0;
    while i < order.len() {
        for c in p.sequences[order[i]].tracks.iter().flat_map(|t| &t.clips) {
            if let Some(n) = c.nested {
                if n < p.sequences.len() && !order.contains(&n) {
                    order.push(n);
                }
            }
        }
        i += 1;
    }
    let index: HashMap<usize, usize> = order.iter().enumerate().map(|(i, &s)| (s, i)).collect();
    let mut media = Vec::new();
    let sequences = order
        .iter()
        .map(|&s| {
            let mut sq = p.sequences[s].clone();
            for c in sq.tracks.iter_mut().flat_map(|t| t.clips.iter_mut()) {
                c.nested = c.nested.and_then(|n| index.get(&n).copied());
                if let Some(m) = p.media(c.media) {
                    if !media.iter().any(|x: &MediaAsset| x.id == m.id) {
                        media.push(m.clone());
                    }
                }
            }
            sq
        })
        .collect();
    TemplateFile { format: FORMAT, name: p.sequences[seq].name.clone(), sequences, media }
}

/// What adding a template does: the commands, and where its composition lands.
pub struct Added {
    pub cmds: Vec<Box<dyn Command>>,
    pub index: usize,
}

/// Add template `t` to `p` as compositions of its own (new ids; footage it
/// shares with the project by path isn't imported twice). Allocates ids.
pub fn add(p: &mut Project, t: &TemplateFile) -> Added {
    let mut cmds: Vec<Box<dyn Command>> = Vec::new();
    let mut media_map: HashMap<MediaId, MediaId> = HashMap::new();
    for m in &t.media {
        if let Some(have) = p.media.iter().find(|x| x.path == m.path) {
            media_map.insert(m.id, have.id);
        } else {
            let id = MediaId(p.alloc_id());
            media_map.insert(m.id, id);
            cmds.push(Box::new(ImportMedia(MediaAsset { id, ..m.clone() })));
        }
    }
    let base = p.sequences.len();
    let mut clip_map: HashMap<ClipId, ClipId> = HashMap::new();
    let mut link_map: HashMap<u64, u64> = HashMap::new();
    for c in t.sequences.iter().flat_map(|s| s.tracks.iter().flat_map(|t| &t.clips)) {
        clip_map.insert(c.id, ClipId(p.alloc_id()));
        if let Some(l) = c.link {
            if !link_map.contains_key(&l) {
                let n = p.alloc_id();
                link_map.insert(l, n);
            }
        }
    }
    for (i, s) in t.sequences.iter().enumerate() {
        let mut sq = s.clone();
        for c in sq.tracks.iter_mut().flat_map(|t| t.clips.iter_mut()) {
            c.id = clip_map[&c.id];
            c.link = c.link.map(|l| link_map[&l]);
            c.parent = c.parent.and_then(|q| clip_map.get(&q).copied());
            c.nested = c.nested.map(|n| base + n);
            if let Some(m) = media_map.get(&c.media) {
                c.media = *m;
            }
            // Fills and paint don't travel (they're made for the original footage).
            for e in &mut c.effects {
                if matches!(e.data, Some(EffectData::Fill(_))) {
                    e.data = None;
                }
            }
        }
        for k in &mut sq.controls {
            k.clip = clip_map.get(&k.clip).copied().unwrap_or(k.clip);
        }
        cmds.push(Box::new(AddSequence { seq: sq, index: base + i }));
    }
    Added { cmds, index: base }
}

/// Set a composition's controls (undoable).
pub struct SetControls {
    pub seq: usize,
    pub old: Vec<Control>,
    pub new: Vec<Control>,
}

impl Command for SetControls {
    fn name(&self) -> &str {
        "Template controls"
    }
    fn apply(&mut self, p: &mut Project) {
        if let Some(s) = p.sequences.get_mut(self.seq) {
            s.controls = self.new.clone();
        }
    }
    fn undo(&mut self, p: &mut Project) {
        if let Some(s) = p.sequences.get_mut(self.seq) {
            s.controls = self.old.clone();
        }
    }
}

/// Replace a clip in sequence `seq` (undoable).
pub struct SetClip {
    pub seq: usize,
    pub old: Clip,
    pub new: Clip,
}

impl Command for SetClip {
    fn name(&self) -> &str {
        "Template control"
    }
    fn apply(&mut self, p: &mut Project) {
        if let Some(c) = p.sequences.get_mut(self.seq).and_then(|s| s.tracks.iter_mut().flat_map(|t| t.clips.iter_mut()).find(|c| c.id == self.new.id)) {
            *c = self.new.clone();
        }
    }
    fn undo(&mut self, p: &mut Project) {
        if let Some(c) = p.sequences.get_mut(self.seq).and_then(|s| s.tracks.iter_mut().flat_map(|t| t.clips.iter_mut()).find(|c| c.id == self.old.id)) {
            *c = self.old.clone();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::{Track, TrackKind};
    use crate::time::{FrameRate, Time};
    use crate::undo::Batch;

    #[test]
    fn a_template_is_added_as_its_own_copy_with_its_controls() {
        let mut p = Project::new();
        p.sequences = vec![Sequence::new("Main", 64, 36, FrameRate::new(25, 1))];
        let mut comp = Sequence { tracks: vec![Track::new("V1", TrackKind::Video)], ..Sequence::new("Lower third", 64, 36, FrameRate::new(25, 1)) };
        let spec = crate::title::TitleSpec { text: "Name".into(), ..Default::default() };
        let id = ClipId(p.alloc_id());
        comp.tracks[0].clips.push(Clip::new_title(id, Time::ZERO, Time::from_secs_f64(3.0), spec));
        comp.controls = vec![Control { label: "Name".into(), clip: id, what: Target::Text }];
        p.sequences.push(comp);
        let t = make(&p, 1);
        assert_eq!((t.sequences.len(), t.media.len()), (1, 0));
        // Through a file and back.
        let t: TemplateFile = serde_json::from_str(&serde_json::to_string(&t).unwrap()).unwrap();
        let added = add(&mut p, &t);
        let index = added.index;
        let mut b = Batch { name: "Add template".into(), cmds: added.cmds };
        b.apply(&mut p);
        assert_eq!((index, p.sequences.len()), (2, 3));
        let copy = &p.sequences[2];
        let c = &copy.tracks[0].clips[0];
        assert_ne!(c.id, id, "new ids");
        assert_eq!(copy.controls[0].clip, c.id, "the control follows");
        assert_eq!(copy.controls[0].what.get(c), Some(Value::Text("Name".into())));
        let set = copy.controls[0].what.set(c, &Value::Text("Ada Lovelace".into())).unwrap();
        assert_eq!(set.title.unwrap().text, "Ada Lovelace");
        assert_eq!(p.sequences[1].tracks[0].clips[0].title.as_ref().unwrap().text, "Name", "the original's untouched");
        b.undo(&mut p);
        assert_eq!(p.sequences.len(), 2);
    }
}
