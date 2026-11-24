//! HitFilm projects (`.hfp`) and composite shots (`.hfcs`): plain XML. A
//! project is `<BiffProject>` → `<Project>` with an `<AssetList>` (media,
//! images, planes and composite shots, each with an `<ID>`) and the editor's
//! `<EditorSequence>`; an exported composite shot is `<BiffCompositeShot>`
//! with the composition (and what it uses) inside.
//!
//! The editor has `<Video>` and `<Audio>` tracks (bottom track first), each
//! with `<Objects>`: clips (`AssetVisualObject`, `AssetAudioObject`, the
//! older `VisualObject`) and text (`TextVisualObject`). A clip's
//! `ObjectBase` gives its `StartFrame`/`EndFrame` in sequence frames, its
//! `LinkedObjectID` (picture and sound together) and its properties; its
//! `AssetInstanceStart` is where in the media it starts, in milliseconds.
//! A composite shot's `<Layers>` are top layer first, each with a
//! `LayerBase` holding the same.
//!
//! Properties (`PropertyManager`) are `<Prop><Name>…</Name>` or elements
//! named for the property, each with a `<Default>`, a `<Static>` (the value
//! set) and perhaps an `<Animation>` of `<Key Ti="…">` keyframes, their
//! times in milliseconds from the start of the clip or layer. Positions are
//! pixels from the middle of the frame with Y up; anchors pixels from the
//! middle of the layer, also Y up; scale per cent; rotation degrees
//! anticlockwise; opacity per cent; audio level decibels; speed a factor.
//!
//! Transitions (`InTrans`/`OutTrans` → `Transition`) name the clips either
//! side and their length in frames.

// Reads files from anywhere: no position in a list is taken on trust (`get`,
// checked slices), so a damaged or hostile file is an error, never a crash.
#![cfg_attr(not(test), deny(clippy::indexing_slicing))]
// (clippy takes the closures that are called once and then handed on for
// the keyframes, `f(&p.value)` then `curve(.., f, ..)`, for ones called only once)
#![allow(clippy::redundant_closure_call)]

use super::{count, file_name, locate, rate_of, secs, Imported, Notes};
use crate::anim::{Curve, Interp, Key};
use crate::audio::VolumeKey;
use crate::project::*;
use crate::time::{FrameRate, Rational, Time};
use roxmltree::{Document, Node};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The most clips and layers read from one project (a guard against a hostile file).
const MAX_ITEMS: usize = 200_000;
/// Frame numbers beyond this are nonsense (some 40 days at 120 fps).
const MAX_FRAME: i64 = 400_000_000;
/// A nil id: nothing.
const NIL: &str = "00000000-0000-0000-0000-000000000000";

/// Is `head` (a file's first bytes) a HitFilm project or composite shot?
pub fn sniff(head: &[u8]) -> bool {
    let n = head.len().min(4096);
    let head = head.get(..n).unwrap_or(&[]);
    head.windows(5).any(|w| w == b"<Biff")
}

/// Read a HitFilm project or composite shot; `dir` is the folder it's in.
pub fn read(bytes: &[u8], dir: Option<&Path>) -> Result<Imported, String> {
    let text = String::from_utf8_lossy(bytes);
    let text = text.trim_start_matches('\u{feff}');
    let opts = roxmltree::ParsingOptions { allow_dtd: false, nodes_limit: 50_000_000 };
    let doc = Document::parse_with_options(text, opts).map_err(|e| format!("The HitFilm project couldn't be read: {e}"))?;
    let root = doc.root_element();
    let shot = match root.tag_name().name() {
        "BiffProject" => false,
        "BiffCompositeShot" => true,
        _ => return Err("That isn't a HitFilm project".into()),
    };
    Reader::new(root, dir).run(shot)
}

fn kid<'a, 'i>(n: Node<'a, 'i>, tag: &str) -> Option<Node<'a, 'i>> {
    n.children().find(|c| c.is_element() && c.tag_name().name() == tag)
}

fn at<'a, 'i>(n: Node<'a, 'i>, path: &str) -> Option<Node<'a, 'i>> {
    path.split('/').try_fold(n, kid)
}

fn text<'a>(n: Node<'a, '_>, path: &str) -> Option<&'a str> {
    at(n, path).and_then(|e| e.text()).map(str::trim)
}

fn int(n: Node, path: &str) -> Option<i64> {
    text(n, path).and_then(|t| t.parse::<i64>().ok().or_else(|| t.parse::<f64>().ok().filter(|f| f.is_finite()).map(|f| f.round().clamp(-9e15, 9e15) as i64)))
}

fn num(n: Node, path: &str) -> Option<f64> {
    text(n, path).and_then(|t| t.parse::<f64>().ok()).filter(|f| f.is_finite())
}

fn attr(n: Node, name: &str) -> Option<f64> {
    n.attribute(name).and_then(|t| t.trim().parse::<f64>().ok()).filter(|f| f.is_finite())
}

fn elements<'a, 'i>(n: Node<'a, 'i>) -> impl Iterator<Item = Node<'a, 'i>> {
    n.children().filter(|c| c.is_element())
}

/// A property's value, as numbers (one for a number or switch; X, Y and Z
/// for a point or scale; R, G, B and A for a colour).
fn value_of(v: Node) -> Option<Vec<f64>> {
    let e = elements(v).next()?;
    match e.tag_name().name() {
        "p2" | "v2" => Some(vec![attr(e, "X").unwrap_or(0.0), attr(e, "Y").unwrap_or(0.0)]),
        "p3" | "sc" | "v3" => Some(vec![attr(e, "X").unwrap_or(0.0), attr(e, "Y").unwrap_or(0.0), attr(e, "Z").unwrap_or(0.0)]),
        "cl" => Some(vec![attr(e, "R").unwrap_or(0.0), attr(e, "G").unwrap_or(0.0), attr(e, "B").unwrap_or(0.0), attr(e, "A").unwrap_or(1.0)]),
        _ => e.text().and_then(|t| t.trim().parse::<f64>().ok()).filter(|f| f.is_finite()).map(|f| vec![f]),
    }
}

/// A property: its value and keyframes (milliseconds from the start, value, how it moves on).
#[derive(Clone, Debug, Default)]
struct Prop {
    value: Vec<f64>,
    keys: Vec<(f64, Vec<f64>, Interp)>,
}

impl Prop {
    fn first(&self) -> Option<f64> {
        self.value.first().copied()
    }
}

/// HitFilm's key interpolation: 1 linear, 2 constant; the rest (smooth, ease, manual) eased.
fn interp_of(code: Option<&str>) -> Interp {
    match code.map(str::trim) {
        Some("1") => Interp::Linear,
        Some("2") => Interp::Hold,
        _ => Interp::Ease,
    }
}

/// The properties under a node's `PropertyManager`, by name.
fn props(owner: Node) -> HashMap<String, Prop> {
    let mut out = HashMap::new();
    let Some(pm) = kid(owner, "PropertyManager") else { return out };
    for p in elements(pm) {
        let name = if p.tag_name().name() == "Prop" { text(p, "Name").unwrap_or("").to_string() } else { p.tag_name().name().to_string() };
        if name.is_empty() {
            continue;
        }
        let value = kid(p, "Static").and_then(value_of).or_else(|| kid(p, "Default").and_then(value_of)).unwrap_or_default();
        let mut keys: Vec<(f64, Vec<f64>, Interp)> = kid(p, "Animation")
            .map(|a| {
                elements(a)
                    .filter(|k| k.tag_name().name() == "Key")
                    .take(100_000)
                    .filter_map(|k| Some((attr(k, "Ti")?.clamp(-1e12, 1e12), kid(k, "Vl").and_then(value_of)?, interp_of(k.attribute("Tp")))))
                    .collect()
            })
            .unwrap_or_default();
        keys.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        out.insert(name, Prop { value, keys });
    }
    out
}

/// Where a clip's own details are: its `ObjectBase` (or the object itself, in older files).
fn base_of<'a, 'i>(o: Node<'a, 'i>) -> Option<Node<'a, 'i>> {
    for path in ["VisualObject/ObjectBase", "AudioObject/ObjectBase", "ObjectBase", "LayerBase"] {
        if let Some(b) = at(o, path) {
            return Some(b);
        }
    }
    kid(o, "StartFrame").map(|_| o)
}

/// What an asset is.
#[derive(Clone)]
enum Asset {
    /// A media file (or picture): its path, whether it's a picture, its frame rate and length.
    File { path: PathBuf, still: bool, sequence: bool, rate: FrameRate, length: Time, name: String },
    /// A composite shot: its index among our sequences.
    Comp(usize),
    /// A plane: its colour and size.
    Plane([u8; 4], [u32; 2]),
}

fn rgba(v: &[f64]) -> [u8; 4] {
    let c = |i: usize| (v.get(i).copied().unwrap_or(if i == 3 { 1.0 } else { 0.0 }).clamp(0.0, 1.0) * 255.0).round() as u8;
    [c(0), c(1), c(2), c(3)]
}

fn color_attrs(n: Node) -> [u8; 4] {
    rgba(&[attr(n, "R").unwrap_or(0.0), attr(n, "G").unwrap_or(0.0), attr(n, "B").unwrap_or(0.0), attr(n, "A").unwrap_or(1.0)])
}

fn is_audio_file(p: &Path) -> bool {
    p.extension().is_some_and(|e| ["wav", "mp3", "aac", "m4a", "flac", "ogg", "aif", "aiff", "wma", "opus"].iter().any(|x| e.eq_ignore_ascii_case(x)))
}

/// A frame rate as HitFilm writes it ("30", "29.97", "23.976").
fn rate_at(n: Node, path: &str) -> FrameRate {
    rate_of(num(n, path).unwrap_or(30.0))
}

fn frame(f: i64, rate: FrameRate) -> Time {
    Time::from_frame(f.clamp(-MAX_FRAME, MAX_FRAME), rate)
}

/// A clip's timing on its track (or a layer's in its composition).
struct Timing {
    start: Time,
    duration: Time,
    /// Where in its media it starts.
    source_in: Time,
    speed: f64,
    /// The frame's size (planes are drawn to it).
    frame: [f32; 2],
}

/// A transition, as one side of it names it.
struct Trans {
    from: String,
    to: String,
    frames: i64,
    name: String,
}

struct Reader<'a, 'i> {
    root: Node<'a, 'i>,
    dir: Option<PathBuf>,
    project: Project,
    notes: Notes,
    assets: HashMap<&'a str, Node<'a, 'i>>,
    /// Assets read, by id.
    read: HashMap<&'a str, Asset>,
    /// Media files added, by path.
    media: HashMap<PathBuf, MediaId>,
    comps: Vec<Node<'a, 'i>>,
    comp_index: HashMap<&'a str, usize>,
    items: usize,
}

impl<'a, 'i> Reader<'a, 'i> {
    fn new(root: Node<'a, 'i>, dir: Option<&Path>) -> Self {
        let mut assets = HashMap::new();
        let mut comps = Vec::new();
        for n in root.descendants().filter(|n| n.is_element()).take(5_000_000) {
            let tag = n.tag_name().name();
            if !matches!(tag, "MediaAsset" | "ImageAsset" | "CompositionAsset" | "SolidAsset" | "AudioAsset") {
                continue;
            }
            let Some(id) = text(n, "ID") else { continue };
            if assets.insert(id, n).is_none() && tag == "CompositionAsset" {
                comps.push(n);
            }
        }
        let mut project = Project::new();
        project.sequences.clear();
        Reader { root, dir: dir.map(Path::to_path_buf), project, notes: Notes::default(), assets, read: HashMap::new(), media: HashMap::new(), comps, comp_index: HashMap::new(), items: 0 }
    }

    fn run(mut self, shot: bool) -> Result<Imported, String> {
        let editor = if shot { None } else { at(self.root, "Project/EditorSequence") };
        if editor.is_none() && self.comps.is_empty() {
            return Err("The HitFilm file has no editor timeline or composite shots to bring across".into());
        }
        // The editor first, then each composite shot (placeholders now, so clips can point at any).
        let first = usize::from(editor.is_some());
        for (i, c) in self.comps.iter().enumerate() {
            if let Some(id) = text(*c, "ID") {
                self.comp_index.insert(id, first + i);
            }
        }
        if let Some(e) = editor {
            self.project.sequences.push(Sequence::new(text(e, "Name").filter(|n| !n.is_empty()).unwrap_or("Editor"), 1920, 1080, FrameRate::new(30, 1)));
        }
        for c in &self.comps {
            self.project.sequences.push(Sequence::new(text(*c, "Name").unwrap_or("Composite Shot"), 1920, 1080, FrameRate::new(30, 1)));
        }
        if let Some(e) = editor {
            let s = self.editor(e);
            if let Some(slot) = self.project.sequences.get_mut(0) {
                *slot = s;
            }
        }
        for (i, c) in self.comps.clone().into_iter().enumerate() {
            let s = self.comp(c);
            if let Some(slot) = self.project.sequences.get_mut(first + i) {
                *slot = s;
            }
        }
        // Media in the project that nothing uses comes across too.
        let ids: Vec<&'a str> = self.assets.iter().filter(|(_, n)| matches!(n.tag_name().name(), "MediaAsset" | "ImageAsset" | "AudioAsset")).map(|(id, _)| *id).collect();
        let mut ids = ids;
        ids.sort_unstable();
        for id in ids {
            if let Some(Asset::File { path, still, rate, length, name, .. }) = self.asset(id) {
                self.add_media(&path, still, rate, length, &name);
            }
        }
        let (seqs, comps) = (usize::from(editor.is_some()), self.comps.len());
        let what = match (seqs, comps) {
            (1, 0) => "the editor timeline".to_string(),
            (1, n) => format!("the editor timeline and {}", count(n, "composite shot", "composite shots")),
            (_, n) => count(n, "composite shot", "composite shots"),
        };
        let summary = format!("{what}, {}, {}", count(self.items, "clip", "clips"), count(self.project.media.len(), "media file", "media files"));
        Ok(Imported { project: self.project, notes: self.notes.lines(), summary })
    }

    /// What asset `id` is (read once).
    fn asset(&mut self, id: &str) -> Option<Asset> {
        if let Some(a) = self.read.get(id) {
            return Some(a.clone());
        }
        let (&key, &n) = self.assets.get_key_value(id)?;
        let a = match n.tag_name().name() {
            "CompositionAsset" => {
                let i = *self.comp_index.get(key)?;
                Asset::Comp(i)
            }
            "SolidAsset" => {
                let color = kid(n, "Color").map(color_attrs).unwrap_or([128, 128, 128, 255]);
                Asset::Plane(color, [int(n, "Width").unwrap_or(1920).clamp(1, 16384) as u32, int(n, "Height").unwrap_or(1080).clamp(1, 16384) as u32])
            }
            tag => {
                let saved = text(n, "Filename").unwrap_or("");
                let path = locate(&[saved], self.dir.as_deref())?;
                let rate = rate_at(n, "FrameRate");
                let out = int(n, "OutPoint").unwrap_or(0).clamp(0, MAX_FRAME);
                let still = tag == "ImageAsset";
                let sequence = text(n, "IsImageSequence") == Some("1");
                let name = text(n, "Name").filter(|t| !t.is_empty()).map(str::to_string).unwrap_or_else(|| file_name(saved));
                Asset::File { path, still, sequence, rate, length: if still { Time::ZERO } else { frame(out, rate) }, name }
            }
        };
        self.read.insert(key, a.clone());
        Some(a)
    }

    /// A media file in the project (once per path); looked at again when opened, so its details are right.
    fn add_media(&mut self, path: &Path, still: bool, rate: FrameRate, length: Time, name: &str) -> MediaId {
        if let Some(id) = self.media.get(path) {
            return *id;
        }
        let video = (!is_audio_file(path)).then(|| VideoStreamInfo { width: 1920, height: 1080, frame_rate: rate, codec: String::new(), time_base: Rational::new(rate.0.den, rate.0.num), hdr: None, pixel_aspect: (1, 1) });
        let audio = (!still).then_some(AudioStreamInfo { sample_rate: 48_000, channels: 2, codec: String::new() });
        let info = MediaInfo { duration: length, video, audio, container: String::new() };
        let id = MediaId(self.project.alloc_id());
        let name = if name.is_empty() { file_name(&path.to_string_lossy()) } else { name.to_string() };
        self.project.media.push(MediaAsset { id, path: path.to_path_buf(), name, info, conform: None });
        self.media.insert(path.to_path_buf(), id);
        id
    }

    // ---- The editor ------------------------------------------------------------

    fn editor(&mut self, e: Node<'a, 'i>) -> Sequence {
        let (w, h) = (int(e, "AudioVideoSettings/Width").unwrap_or(1920).clamp(2, 16384) as u32, int(e, "AudioVideoSettings/Height").unwrap_or(1080).clamp(2, 16384) as u32);
        let rate = rate_at(e, "AudioVideoSettings/FrameRate");
        let mut seq = Sequence::new(text(e, "Name").filter(|n| !n.is_empty()).unwrap_or("Editor"), (w + 1) & !1, (h + 1) & !1, rate);
        seq.tracks.clear();
        let fr = [w as f32, h as f32];
        // Transitions, wherever they're written.
        let trans: Vec<Trans> = e.descendants().filter(|n| n.tag_name().name() == "Transition").filter_map(|t| Some(Trans { from: text(t, "FromObjectID")?.to_string(), to: text(t, "ToObjectID")?.to_string(), frames: int(t, "Length")?.clamp(0, MAX_FRAME), name: text(t, "PluginName").unwrap_or("").to_string() })).collect();
        // Linked picture and sound: one link id per pair.
        let mut links: HashMap<String, u64> = HashMap::new();
        let mut ids: HashMap<String, ClipId> = HashMap::new();
        for (group, kind) in [("Video", TrackKind::Video), ("Audio", TrackKind::Audio)] {
            let Some(g) = kid(e, group) else { continue };
            for (n, t) in elements(g).filter(|t| matches!(t.tag_name().name(), "VideoTrack" | "AudioTrack")).enumerate() {
                let label = if kind == TrackKind::Video { "Video" } else { "Audio" };
                let mut track = Track::new(text(t, "Name").filter(|s| !s.is_empty()).map(str::to_string).unwrap_or_else(|| format!("{label} {}", n + 1)), kind);
                track.locked = text(t, "Locked") == Some("1");
                track.muted = if kind == TrackKind::Video { text(t, "Visible") == Some("0") } else { text(t, "Muted") == Some("1") };
                // A track's own level (sound tracks) adds to each clip's.
                let track_db = props(t).get("audioLevel").and_then(Prop::first).unwrap_or(0.0) as f32;
                if props(t).get("audioLevel").is_some_and(|p| !p.keys.is_empty()) {
                    self.notes.add("Keyframed track levels came across at their first value");
                }
                if let Some(objs) = kid(t, "Objects") {
                    for o in elements(objs) {
                        if self.items >= MAX_ITEMS {
                            self.notes.add("Very many clips: the rest were left out");
                            break;
                        }
                        let Some((mut c, own, linked)) = self.object(o, kind, rate, fr) else { continue };
                        self.items += 1;
                        if kind == TrackKind::Audio {
                            c.audio.volume_db = (c.audio.volume_db + track_db).clamp(crate::audio::MIN_DB, crate::audio::MAX_DB);
                        }
                        if !linked.is_empty() && linked != NIL {
                            let key = if own < linked { format!("{own}|{linked}") } else { format!("{linked}|{own}") };
                            let next = links.len() as u64;
                            let id = *links.entry(key).or_insert_with(|| next);
                            c.link = Some(id);
                        }
                        ids.insert(own, c.id);
                        track.clips.push(c);
                    }
                }
                track.sort();
                seq.tracks.push(track);
            }
        }
        // Link groups numbered from the project's own ids.
        let mut fresh: HashMap<u64, u64> = HashMap::new();
        for t in &mut seq.tracks {
            for c in &mut t.clips {
                if let Some(l) = c.link {
                    let id = *fresh.entry(l).or_insert_with(|| self.project.alloc_id());
                    c.link = Some(id);
                }
            }
        }
        self.transitions(&trans, &ids, &mut seq, rate);
        if !seq.tracks.iter().any(|t| t.kind == TrackKind::Video) {
            seq.tracks.insert(0, Track::new("Video 1", TrackKind::Video));
        }
        if !seq.tracks.iter().any(|t| t.kind == TrackKind::Audio) {
            seq.tracks.push(Track::new("Audio 1", TrackKind::Audio));
        }
        seq
    }

    /// An editor clip: the clip, its HitFilm id, and the id of the clip it's linked to.
    fn object(&mut self, o: Node<'a, 'i>, kind: TrackKind, rate: FrameRate, fr: [f32; 2]) -> Option<(Clip, String, String)> {
        let b = base_of(o)?;
        let own = text(b, "ID").unwrap_or("").to_string();
        let linked = text(b, "LinkedObjectID").unwrap_or("").to_string();
        let (s, e) = (int(b, "StartFrame")?, int(b, "EndFrame")?);
        if e <= s {
            return None;
        }
        let p = props(b);
        let timing = Timing { start: frame(s, rate), duration: frame(e, rate) - frame(s, rate), source_in: secs(int(o, "AssetInstanceStart").unwrap_or(0).max(0) as f64 / 1000.0), speed: speed_of(&p), frame: fr };
        let id = ClipId(self.project.alloc_id());
        let mut offset = None;
        let mut c = if o.tag_name().name() == "TextVisualObject" {
            if kind == TrackKind::Audio {
                return None;
            }
            let (spec, at) = self.title(kid(o, "TextBox"));
            offset = Some(at);
            Clip::new_title(id, timing.start, timing.duration, spec)
        } else {
            let asset = text(o, "AssetID").unwrap_or("");
            self.clip_for(asset, id, &timing, kind, text(b, "Name").unwrap_or(""))?
        };
        if kind == TrackKind::Video {
            let blend = int(o, "VisualObject/BlendMode").or_else(|| int(o, "BlendMode")).unwrap_or(0);
            if blend != 0 {
                self.notes.add("Blend modes came across as Normal");
            }
            self.place(&mut c, &p);
            if let Some(at) = offset {
                shift(&mut c, at);
            }
            let masks = at(o, "VisualObject/Masks").or_else(|| kid(o, "Masks"));
            if masks.is_some_and(|m| elements(m).next().is_some()) {
                self.notes.add("Masks were left out");
            }
        } else {
            self.sound(&mut c, &p);
        }
        self.effects(b, kind);
        Some((c, own, linked))
    }

    /// A clip of asset `id`.
    fn clip_for(&mut self, asset: &str, id: ClipId, t: &Timing, kind: TrackKind, name: &str) -> Option<Clip> {
        let Some(a) = self.asset(asset) else {
            self.notes.add(if kind == TrackKind::Audio { "Sound clips whose file isn't named in the project were left out" } else { "Clips whose file isn't named in the project were left out" });
            return None;
        };
        let mut c = match a {
            Asset::File { path, still, sequence, rate, length, name: n } => {
                if kind == TrackKind::Video && is_audio_file(&path) {
                    return None;
                }
                if sequence {
                    self.notes.add("Image sequences came across as their first picture (import the sequence to play it)");
                }
                let m = self.add_media(&path, still, rate, length, if n.is_empty() { name } else { &n });
                Clip::new(id, m, t.start, if still { Time::ZERO } else { t.source_in }, t.duration, None)
            }
            Asset::Comp(i) => {
                let mut c = Clip::new_nested(id, i, t.start, t.duration, None);
                c.source_in = t.source_in;
                c
            }
            Asset::Plane(color, size) => {
                if kind == TrackKind::Audio {
                    return None;
                }
                let mut c = Clip::new_plane(id, t.start, t.duration, color);
                // Planes are drawn frame-sized: their own size as a scale.
                c.transform.scale = [size[0] as f32 / t.frame[0].max(1.0), size[1] as f32 / t.frame[1].max(1.0)];
                c
            }
        };
        if (t.speed - 1.0).abs() > 1e-4 && c.media != MediaId::GENERATED {
            c.speed = t.speed as f32;
            let m = c.source_in;
            // Media time = anchor + speed × clip time, the clip time starting at the in point.
            c.anchor = m - secs(m.as_secs_f64() * t.speed);
        }
        Some(c)
    }

    /// Position, anchor, scale, rotation and opacity (and their keyframes).
    fn place(&mut self, c: &mut Clip, p: &HashMap<String, Prop>) {
        let si = c.source_in;
        let to_ct = move |ms: f64| si + secs(ms.max(0.0) / 1000.0);
        let flip = |v: &[f64]| [v.first().copied().unwrap_or(0.0) as f32, -(v.get(1).copied().unwrap_or(0.0) as f32)];
        // (Planes keep the size scale they were given: HitFilm's scale multiplies it.)
        let base_scale = c.transform.scale;
        if let Some(pos) = p.get("position") {
            c.transform.position = flip(&pos.value);
            c.anim.position = curve(&pos.keys, flip, &to_ct);
            if pos.value.get(2).is_some_and(|z| z.abs() > 1e-3) || pos.keys.iter().any(|k| k.1.get(2).is_some_and(|z| z.abs() > 1e-3)) {
                self.notes.add("Depth (Z) positions were left out");
            }
        }
        if let Some(a) = p.get("anchorPoint") {
            c.transform.anchor = flip(&a.value);
            c.anim.anchor = curve(&a.keys, flip, &to_ct);
        }
        if let Some(s) = p.get("scale") {
            let pct = |v: &[f64]| [base_scale[0] * v.first().copied().unwrap_or(100.0) as f32 / 100.0, base_scale[1] * v.get(1).copied().unwrap_or(100.0) as f32 / 100.0];
            c.transform.scale = pct(&s.value);
            c.anim.scale = curve(&s.keys, pct, &to_ct);
        }
        if let Some(r) = p.get("rotationZ") {
            let deg = |v: &[f64]| -(v.first().copied().unwrap_or(0.0) as f32);
            c.transform.rotation = deg(&r.value);
            c.anim.rotation = curve(&r.keys, deg, &to_ct);
        }
        if ["rotationX", "rotationY"].iter().any(|n| p.get(*n).is_some_and(|r| r.first().is_some_and(|v| v.abs() > 1e-3) || !r.keys.is_empty())) {
            self.notes.add("Rotation about X and Y (3D) was left out");
        }
        if let Some(o) = p.get("opacity") {
            let f = |v: &[f64]| (v.first().copied().unwrap_or(100.0) as f32 / 100.0).clamp(0.0, 1.0);
            c.transform.opacity = f(&o.value);
            c.anim.opacity = curve(&o.keys, f, &to_ct);
        }
    }

    /// A sound clip's level (decibels) and its keyframes.
    fn sound(&mut self, c: &mut Clip, p: &HashMap<String, Prop>) {
        let db = |v: f64| (v as f32).clamp(crate::audio::MIN_DB, crate::audio::MAX_DB);
        if let Some(l) = p.get("audioLevel") {
            c.audio.volume_db = db(l.first().unwrap_or(0.0));
            let start = c.source_in;
            c.audio.volume_keys = l.keys.iter().filter_map(|(ms, v, _)| Some(VolumeKey { at: start + secs(ms.max(0.0) / 1000.0), db: db(*v.first()?) })).collect();
            c.audio.volume_keys.dedup_by_key(|k| k.at);
        }
        if p.get("stereoBalance").and_then(Prop::first).is_some_and(|b| b.abs() > 1e-3) {
            c.audio.pan = (p.get("stereoBalance").and_then(Prop::first).unwrap_or(0.0) as f32 / 100.0).clamp(-1.0, 1.0);
        }
    }

    /// Effects on a clip or layer, named in the notes.
    fn effects(&mut self, b: Node, kind: TrackKind) {
        let lists = [kid(b, "Effects"), b.parent_element().and_then(|p| kid(p, "Effects"))];
        for list in lists.into_iter().flatten() {
            for e in elements(list) {
                if text(e, "Enabled") == Some("0") {
                    continue;
                }
                let name = text(e, "PluginName").filter(|n| !n.is_empty()).unwrap_or("an effect");
                self.notes.add(if kind == TrackKind::Audio { format!("Audio effect left out: {name}") } else { format!("Effect left out: {name}") });
            }
        }
    }

    /// Transitions between clips (dissolves and the like), and at either end of one (fades).
    fn transitions(&mut self, trans: &[Trans], ids: &HashMap<String, ClipId>, seq: &mut Sequence, rate: FrameRate) {
        let mut done = std::collections::HashSet::new();
        for t in trans {
            if !done.insert((t.from.clone(), t.to.clone())) || t.frames <= 0 {
                continue;
            }
            let len = frame(t.frames, rate);
            let (from, to) = (ids.get(&t.from).copied(), ids.get(&t.to).copied());
            let lower = t.name.to_ascii_lowercase();
            for track in &mut seq.tracks {
                let audio = track.kind == TrackKind::Audio;
                match (from, to) {
                    (Some(_), Some(b)) => {
                        let Some(c) = track.clips.iter_mut().find(|c| c.id == b) else { continue };
                        if audio {
                            c.audio.crossfade_in = len.min(c.duration);
                        } else {
                            c.dissolve_in = len.min(c.duration);
                            c.transition = transition_of(&lower);
                            if !(lower.contains("dissolve") || lower.contains("fade") || lower.contains("dip") || lower.is_empty()) {
                                self.notes.add(format!("Transitions came across as the nearest of Reelforge's (their direction may differ): {}", t.name));
                            }
                        }
                    }
                    // Into the first clip, or out of the last: a fade.
                    (None, Some(b)) | (Some(b), None) => {
                        let into = to == Some(b) && from.is_none();
                        let Some(c) = track.clips.iter_mut().find(|c| c.id == b) else { continue };
                        let len = len.min(c.duration);
                        if audio {
                            if into {
                                c.audio.fade_in = len;
                            } else {
                                c.audio.fade_out = len;
                            }
                        } else {
                            fade(c, len, into);
                            if !(lower.contains("fade") || lower.contains("dissolve") || lower.contains("dip")) {
                                self.notes.add(format!("Transitions at the start or end of a clip came across as fades: {}", t.name));
                            }
                        }
                    }
                    (None, None) => {}
                }
            }
        }
    }

    /// A text clip's title: its words, size, colour and alignment, and where it sits.
    fn title(&mut self, tb: Option<Node>) -> (crate::title::TitleSpec, [f32; 2]) {
        let mut spec = crate::title::TitleSpec::default();
        let Some(tb) = tb else {
            spec.text = "Text".into();
            return (spec, [0.0, 0.0]);
        };
        let mut words = String::new();
        let mut align = None;
        let mut format = None;
        if let Some(tokens) = kid(tb, "Tokens") {
            for (i, k) in elements(tokens).take(100_000).enumerate() {
                match k.attribute("Tp").map(str::trim) {
                    // A paragraph: a new line (after the first), and how it's aligned.
                    Some("1") => {
                        if i > 0 {
                            words.push('\n');
                        }
                        if align.is_none() {
                            align = k.attribute("Jf").map(str::to_string);
                        }
                    }
                    _ => {
                        if let Some(ch) = k.attribute("Ch").and_then(|c| c.trim().parse::<u32>().ok()).and_then(char::from_u32) {
                            words.push(if ch == '\r' { '\n' } else { ch });
                        }
                        if format.is_none() {
                            format = k.attribute("Ft").map(str::to_string);
                        }
                    }
                }
            }
        }
        spec.text = if words.trim().is_empty() { "Text".into() } else { words };
        spec.align = match align.as_deref() {
            Some("1") => crate::title::TextAlign::Center,
            Some("2") => crate::title::TextAlign::Right,
            _ => crate::title::TextAlign::Left,
        };
        let f = kid(tb, "Formats").and_then(|fs| elements(fs).find(|f| format.as_deref().is_none_or(|id| f.attribute("ID") == Some(id))));
        if let Some(f) = f {
            if let Some(size) = attr(f, "Size").filter(|s| *s > 0.0) {
                spec.size = size.clamp(1.0, 2000.0) as f32;
            }
            if let Some(c) = kid(f, "FillColor") {
                spec.color = color_attrs(c);
            }
            // Its font by name (path-less: the opening finds it among the installed fonts).
            if let Some(family) = text(f, "Family").filter(|n| !n.is_empty()) {
                spec.font = crate::title::FontChoice { path: None, index: 0, name: family.to_string() };
            }
        }
        if text(tb, "BackgroundEnable") == Some("1") {
            spec.background = true;
            if let Some(c) = kid(tb, "BackgroundColor") {
                spec.background_color = color_attrs(c);
            }
        }
        // The middle of the text's box, from the clip's own origin (Y up).
        let mid = |a: &str, b: &str| ((num(tb, a).unwrap_or(0.0) + num(tb, b).unwrap_or(0.0)) / 2.0) as f32;
        (spec, [mid("MinX", "MaxX"), -mid("MinY", "MaxY")])
    }

    // ---- Composite shots --------------------------------------------------------

    fn comp(&mut self, c: Node<'a, 'i>) -> Sequence {
        let (w, h) = (int(c, "AudioVideoSettings/Width").unwrap_or(1920).clamp(2, 16384) as u32, int(c, "AudioVideoSettings/Height").unwrap_or(1080).clamp(2, 16384) as u32);
        let rate = rate_at(c, "AudioVideoSettings/FrameRate");
        let mut seq = Sequence::new(text(c, "Name").unwrap_or("Composite Shot"), (w + 1) & !1, (h + 1) & !1, rate);
        seq.tracks.clear();
        let length = int(c, "AudioVideoSettings/FrameCount").map(|n| frame(n.max(0), rate));
        let mut video = Vec::new();
        let mut audio = Vec::new();
        let layers: Vec<Node> = kid(c, "Layers").map(|l| elements(l).collect()).unwrap_or_default();
        // Top layer first; tracks bottom first.
        for l in layers.iter().rev() {
            if self.items >= MAX_ITEMS {
                self.notes.add("Very many layers: the rest were left out");
                break;
            }
            self.layer(*l, rate, length, [w as f32, h as f32], &mut video, &mut audio);
        }
        if video.is_empty() {
            video.push(Track::new("Video 1", TrackKind::Video));
        }
        if audio.is_empty() {
            audio.push(Track::new("Audio 1", TrackKind::Audio));
        }
        seq.tracks = video;
        seq.tracks.extend(audio);
        seq
    }

    fn layer(&mut self, l: Node<'a, 'i>, rate: FrameRate, length: Option<Time>, fr: [f32; 2], video: &mut Vec<Track>, audio: &mut Vec<Track>) {
        let kind = l.tag_name().name();
        match kind {
            "CameraLayer" => return self.notes.add("Cameras were left out"),
            "LightLayer" => return self.notes.add("Lights were left out"),
            "PointLayer" => return self.notes.add("Points were left out"),
            "ModelLayer" => return self.notes.add("3D models were left out"),
            "ParticleSimulatorLayer" | "ParticleLayer" => return self.notes.add("Particle simulators were left out"),
            _ => {}
        }
        let Some(b) = kid(l, "LayerBase") else { return };
        if text(b, "Visible") == Some("0") {
            return self.notes.add("Hidden layers were left out");
        }
        let (Some(s), Some(e)) = (int(b, "StartFrame"), int(b, "EndFrame")) else { return };
        let (mut a, mut z) = (frame(s.max(0), rate), frame(e, rate));
        if let Some(len) = length.filter(|l| *l > Time::ZERO) {
            z = z.min(len);
        }
        if z <= a {
            return;
        }
        a = a.max(Time::ZERO);
        let p = props(b);
        let timing = Timing { start: a, duration: z - a, source_in: secs(int(l, "AssetInstanceStart").unwrap_or(0).max(0) as f64 / 1000.0), speed: speed_of(&p), frame: fr };
        let name = text(b, "Name").unwrap_or("").to_string();
        let id = ClipId(self.project.alloc_id());
        let mut offset = None;
        // A layer of only sound (a music file) has no picture.
        let mut picture = text(l, "AssetHasVideo") != Some("0");
        let mut clip = match kind {
            "TextLayer" => {
                let (spec, at) = self.title(kid(l, "TextBox"));
                offset = Some(at);
                Clip::new_title(id, timing.start, timing.duration, spec)
            }
            "GradeLayer" | "AdjustmentLayer" => Clip::new_adjustment(id, timing.start, timing.duration),
            "PlaneLayer" | "SolidLayer" => {
                let color = p.get("color").map(|c| rgba(&c.value)).unwrap_or([128, 128, 128, 255]);
                Clip::new_plane(id, timing.start, timing.duration, color)
            }
            "AssetLayer" => {
                let asset = text(l, "AssetID").unwrap_or("");
                if matches!(self.asset(asset), Some(Asset::File { ref path, .. }) if is_audio_file(path)) {
                    picture = false;
                }
                let Some(c) = self.clip_for(asset, id, &timing, if picture { TrackKind::Video } else { TrackKind::Audio }, &name) else { return };
                c
            }
            other => {
                self.notes.add(format!("Layers of a kind Reelforge doesn't have were left out: {}", other.trim_end_matches("Layer")));
                return;
            }
        };
        self.items += 1;
        if text(l, "Dimensions") == Some("1") {
            self.notes.add("3D layers came across flat");
        }
        if text(b, "ParentLayerID").is_some_and(|p| p != NIL && !p.is_empty()) {
            self.notes.add("Parenting was left out (layers keep their own movement)");
        }
        if int(b, "BlendMode").unwrap_or(0) != 0 {
            self.notes.add("Blend modes came across as Normal");
        }
        if kid(l, "Masks").is_some_and(|m| elements(m).next().is_some()) {
            self.notes.add("Masks were left out");
        }
        if let Some(fx) = kid(l, "Effects") {
            for e in elements(fx).filter(|e| text(*e, "Enabled") != Some("0")) {
                self.notes.add(format!("Effect left out: {}", text(e, "PluginName").filter(|n| !n.is_empty()).unwrap_or("an effect")));
            }
        }
        self.place(&mut clip, &p);
        if let Some(at) = offset {
            shift(&mut clip, at);
        }
        let label = if name.is_empty() { format!("Layer {}", video.len() + 1) } else { name };
        // Its sound, linked, on an audio track of its own.
        let sound = kind == "AssetLayer" && (text(l, "AssetHasAudio") == Some("1") || !picture) && clip.media != MediaId::GENERATED && clip.nested.is_none();
        if sound {
            let link = self.project.alloc_id();
            let mut s = clip.clone();
            s.id = ClipId(self.project.alloc_id());
            s.link = Some(link);
            s.anim = Default::default();
            s.transform = Default::default();
            self.sound(&mut s, &p);
            if picture {
                clip.link = Some(link);
            }
            let mut t = Track::new(format!("{label} (sound)"), TrackKind::Audio);
            t.clips.push(s);
            audio.push(t);
        }
        if picture {
            let mut t = Track::new(label, TrackKind::Video);
            t.clips.push(clip);
            video.push(t);
        }
    }
}

/// A clip's speed (1 when it isn't set, or is nonsense).
fn speed_of(p: &HashMap<String, Prop>) -> f64 {
    p.get("speed").and_then(Prop::first).map(f64::abs).filter(|s| *s > 1e-4 && *s < 1e4).unwrap_or(1.0)
}

/// Which of Reelforge's transitions is nearest HitFilm's (by its name, in lower case).
fn transition_of(name: &str) -> crate::transition::Transition {
    use crate::transition::Transition as T;
    match name {
        n if n.contains("white") => T::DipToWhite,
        n if n.contains("color") || n.contains("colour") || n.contains("black") || n.contains("dip") => T::DipToBlack,
        n if n.contains("push") => T::PushLeft,
        n if n.contains("slide") => T::SlideLeft,
        n if n.contains("iris") => T::Iris,
        n if n.contains("wipe") => T::WipeLeft,
        _ => T::Dissolve,
    }
}

/// Move a clip (and its movement) by `by` frame pixels.
fn shift(c: &mut Clip, by: [f32; 2]) {
    c.transform.position = [c.transform.position[0] + by[0], c.transform.position[1] + by[1]];
    for k in &mut c.anim.position.0 {
        k.value = [k.value[0] + by[0], k.value[1] + by[1]];
    }
}

/// A fade in (from nothing) at a clip's start, or out at its end, as opacity keyframes.
fn fade(c: &mut Clip, len: Time, into: bool) {
    if len <= Time::ZERO || !c.anim.opacity.0.is_empty() {
        return;
    }
    let full = c.transform.opacity;
    let (a, b) = (c.source_in, c.source_in + c.duration);
    let keys = if into { vec![Key::new(a, 0.0, Interp::Linear), Key::new(a + len, full, Interp::Linear)] } else { vec![Key::new(b - len, full, Interp::Linear), Key::new(b, 0.0, Interp::Linear)] };
    c.anim.opacity = Curve(keys);
}

fn curve<T, F: Fn(&[f64]) -> T>(keys: &[(f64, Vec<f64>, Interp)], f: F, to_ct: &dyn Fn(f64) -> Time) -> Curve<T> {
    let mut out: Vec<Key<T>> = keys.iter().map(|(t, v, i)| Key::new(to_ct(*t), f(v), *i)).collect();
    out.sort_by_key(|k| k.at);
    out.dedup_by_key(|k| k.at);
    Curve(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small project in HitFilm's form: a picture and a clip of a video on Video 1 (the
    /// video moved, scaled, faded in and sped up, linked to its sound), a text on Video 2,
    /// a cross dissolve between the two clips, and a composite shot used as a clip.
    fn project() -> String {
        r#"<?xml version="1.0" encoding="UTF-8"?>
<BiffProject Version="0" AppVersion="18.0" AppEdition="1000"><Project Version="7"><Name>t.hfp</Name>
<AssetList Version="7"><Assets>
 <MediaAsset Version="10"><ID>m1</ID><Name>clip.mp4</Name><Filename>C:\Users\x\Videos\clip.mp4</Filename><IsImageSequence>0</IsImageSequence><FrameRate>29.97</FrameRate><InPoint>0</InPoint><OutPoint>900</OutPoint></MediaAsset>
 <ImageAsset Version="3"><ID>i1</ID><Name>card.png</Name><Filename>C:\Users\x\card.png</Filename></ImageAsset>
 <SolidAsset Version="1"><ID>s1</ID><Name>Plane</Name><Color R="1" G="0" B="0" A="1"/><Width>960</Width><Height>1080</Height></SolidAsset>
 <CompositionAsset Version="11"><ID>c1</ID><Name>Shot</Name><AudioVideoSettings Version="1"><FrameCount>60</FrameCount><Width>1280</Width><Height>720</Height><FrameRate>30</FrameRate></AudioVideoSettings>
  <Layers>
   <TextLayer Version="1"><Dimensions>0</Dimensions><TextBox Version="1"><ID>tb</ID><MinX>-100</MinX><MaxX>100</MaxX><MinY>0</MinY><MaxY>50</MaxY><Tokens><Tk Tp="1" Jf="1" V="2"/><Tk Tp="0" Ch="72" Ft="f" V="2"/><Tk Tp="0" Ch="105" Ft="f" V="2"/></Tokens><Formats><Format ID="f" Size="80"><Family>Arial</Family><FillColor R="0" G="1" B="0" A="1"/></Format></Formats></TextBox><Effects/><Masks/><LayerBase Version="0"><ID>L1</ID><Name>Title</Name><StartFrame>0</StartFrame><EndFrame>60</EndFrame><Visible>1</Visible><BlendMode>0</BlendMode><ParentLayerID>00000000-0000-0000-0000-000000000000</ParentLayerID><PropertyManager Version="5"><position><Default V="4"><p3 X="0" Y="0" Z="0"/></Default></position></PropertyManager></LayerBase></TextLayer>
   <CameraLayer Version="4"><LayerBase Version="2"><ID>L2</ID><Name>Cam</Name><StartFrame>0</StartFrame><EndFrame>60</EndFrame></LayerBase></CameraLayer>
   <AssetLayer Version="15"><AssetHasAudio>0</AssetHasAudio><AssetHasVideo>1</AssetHasVideo><AssetID>s1</AssetID><AssetInstanceStart>0</AssetInstanceStart><Dimensions>0</Dimensions><Effects/><Masks/><LayerBase Version="0"><ID>L3</ID><Name>Back</Name><StartFrame>0</StartFrame><EndFrame>900</EndFrame><Visible>1</Visible><PropertyManager Version="5"><position><Default V="4"><p3 X="10" Y="20" Z="0"/></Default></position></PropertyManager></LayerBase></AssetLayer>
  </Layers></CompositionAsset>
</Assets></AssetList>
<EditorSequence Version="5"><ID>seq</ID><Name>Editor</Name><AudioVideoSettings Version="1"><FrameCount>900</FrameCount><Width>1920</Width><Height>1080</Height><FrameRate>30</FrameRate></AudioVideoSettings>
<Video>
 <VideoTrack Version="3"><ID>v1</ID><Name>Video 1</Name><Visible>1</Visible><Locked>0</Locked><Objects>
  <AssetVisualObject Version="1"><AssetID>i1</AssetID><AssetInstanceStart>0</AssetInstanceStart><VisualObject Version="9"><BlendMode>0</BlendMode><Masks/><ObjectBase Version="2"><ID>A</ID><LinkedObjectID>00000000-0000-0000-0000-000000000000</LinkedObjectID><Name>card.png</Name><StartFrame>0</StartFrame><EndFrame>60</EndFrame><Effects/>
   <OutTrans><Transition Version="3"><ID>t1</ID><PluginName>Cross Dissolve</PluginName><FromObjectID>A</FromObjectID><ToObjectID>B</ToObjectID><Length>15</Length></Transition></OutTrans>
   <PropertyManager Version="7"/></ObjectBase></VisualObject></AssetVisualObject>
  <AssetVisualObject Version="1"><AssetID>m1</AssetID><AssetInstanceStart>2000</AssetInstanceStart><VisualObject Version="9"><BlendMode>0</BlendMode><Masks/><ObjectBase Version="2"><ID>B</ID><LinkedObjectID>BA</LinkedObjectID><Name>clip.mp4</Name><StartFrame>60</StartFrame><EndFrame>150</EndFrame><Effects><Instance Version="8"><Enabled>1</Enabled><PluginName>Chroma Key</PluginName></Instance></Effects>
   <InTrans fromObjectID="A" ref="1" transID="t1"/>
   <PropertyManager Version="7">
    <Prop CanInterpT="1" Spatial="1" Type="0"><Name>position</Name><Default V="4"><p2 X="0" Y="0"/></Default><Static V="4"><p2 X="100" Y="50"/></Static></Prop>
    <Prop CanInterpT="1" Spatial="0" Type="0"><Name>scale</Name><Default V="4"><sc X="100" Y="100" Z="100"/></Default><Static V="4"><sc X="50" Y="50" Z="100"/></Static></Prop>
    <Prop CanInterpT="1" Spatial="0" Type="0"><Name>rotationZ</Name><Default V="4"><fl>0</fl></Default><Static V="4"><fl>30</fl></Static></Prop>
    <Prop CanInterpT="1" Spatial="0" Type="0"><Name>opacity</Name><Default V="4"><fl>100</fl></Default><Animation><Key Tp="1" STp="2" V="5" Ti="0"><Vl V="4"><fl>0</fl></Vl></Key><Key Tp="2" STp="2" V="5" Ti="500"><Vl V="4"><fl>100</fl></Vl></Key></Animation></Prop>
    <Prop CanInterpT="1" Spatial="0" Type="1"><Name>speed</Name><Default V="4"><db>1</db></Default><Static V="4"><db>2</db></Static></Prop>
   </PropertyManager></ObjectBase></VisualObject></AssetVisualObject>
  <VisualObject Version="3"><AssetID>c1</AssetID><AssetInstanceStart>0</AssetInstanceStart><AssetType>3</AssetType><BlendMode>0</BlendMode><Effects/><EndFrame>210</EndFrame><ID>C</ID><LinkedObjectID>00000000-0000-0000-0000-000000000000</LinkedObjectID><Name>Shot</Name><PropertyManager Version="5"/><StartFrame>150</StartFrame></VisualObject>
 </Objects></VideoTrack>
 <VideoTrack Version="3"><ID>v2</ID><Name>Video 2</Name><Visible>1</Visible><Locked>1</Locked><Objects>
  <TextVisualObject Version="0"><TextBox Version="0"><ID>x</ID><MinX>0</MinX><MaxX>400</MaxX><MinY>0</MinY><MaxY>100</MaxY><Tokens><Tk Tp="1" Jf="0" V="2"/><Tk Tp="0" Ch="79" Ft="g" V="2"/><Tk Tp="0" Ch="75" Ft="g" V="2"/><Tk Tp="1" Jf="0" V="2"/><Tk Tp="0" Ch="33" Ft="g" V="2"/></Tokens><Formats><Format ID="g" Size="120"><Family>Arial</Family><FillColor R="1" G="1" B="1" A="1"/></Format></Formats></TextBox>
   <VisualObject Version="9"><BlendMode>0</BlendMode><Masks/><ObjectBase Version="2"><ID>T</ID><Name>Text</Name><StartFrame>30</StartFrame><EndFrame>90</EndFrame><Effects/><PropertyManager Version="7"><Prop><Name>position</Name><Default V="4"><p2 X="-500" Y="300"/></Default></Prop></PropertyManager></ObjectBase></VisualObject></TextVisualObject>
 </Objects></VideoTrack>
</Video>
<Audio>
 <AudioTrack Version="3"><ID>a1</ID><Name>Audio 1</Name><Muted>0</Muted><Locked>0</Locked><Objects>
  <AssetAudioObject Version="0"><AssetID>m1</AssetID><AssetInstanceStart>2000</AssetInstanceStart><AudioObject Version="2"><Muted>0</Muted><ObjectBase Version="2"><ID>BA</ID><LinkedObjectID>B</LinkedObjectID><Name>clip.mp4</Name><StartFrame>60</StartFrame><EndFrame>150</EndFrame><Effects/>
   <PropertyManager Version="7"><Prop><Name>audioLevel</Name><Default V="4"><fl>0</fl></Default><Static V="4"><fl>-6</fl></Static></Prop><Prop><Name>speed</Name><Default V="4"><db>1</db></Default><Static V="4"><db>2</db></Static></Prop></PropertyManager></ObjectBase></AudioObject></AssetAudioObject>
 </Objects><PropertyManager Version="7"><Prop><Name>audioLevel</Name><Default V="4"><fl>0</fl></Default><Static V="4"><fl>-3</fl></Static></Prop></PropertyManager></AudioTrack>
</Audio>
</EditorSequence></Project></BiffProject>"#
            .to_string()
    }

    fn secs_of(t: Time) -> f64 {
        t.as_secs_f64()
    }

    #[test]
    fn reads_the_editor_timeline() {
        let im = read(project().as_bytes(), None).expect("read");
        let p = &im.project;
        assert_eq!(p.sequences.len(), 2, "the editor and the composite shot");
        let s = &p.sequences[0];
        assert_eq!((s.name.as_str(), s.width, s.height, s.frame_rate), ("Editor", 1920, 1080, FrameRate::new(30, 1)));
        let v: Vec<&Track> = s.tracks.iter().filter(|t| t.kind == TrackKind::Video).collect();
        assert_eq!(v.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), ["Video 1", "Video 2"], "bottom first");
        assert!(v[1].locked);
        // The picture, the video and the composite shot on Video 1.
        let c = &v[0].clips;
        assert_eq!(c.len(), 3);
        assert!((secs_of(c[1].timeline_start) - 2.0).abs() < 1e-6 && (secs_of(c[1].duration) - 3.0).abs() < 1e-6);
        assert!((secs_of(c[1].source_in) - 2.0).abs() < 1e-6, "AssetInstanceStart is milliseconds");
        assert_eq!(c[1].speed, 2.0);
        // Position Y up → ours Y down; anticlockwise → clockwise.
        assert_eq!(c[1].transform.position, [100.0, -50.0]);
        assert_eq!(c[1].transform.scale, [0.5, 0.5]);
        assert_eq!(c[1].transform.rotation, -30.0);
        let o = &c[1].anim.opacity.0;
        assert_eq!(o.len(), 2);
        assert!((secs_of(o[1].at - c[1].source_in) - 0.5).abs() < 1e-6, "keys in milliseconds from the clip's start");
        assert_eq!((o[0].value, o[0].interp, o[1].interp), (0.0, Interp::Linear, Interp::Hold));
        // The dissolve into the video.
        assert!((secs_of(c[1].dissolve_in) - 0.5).abs() < 1e-6);
        assert_eq!(c[1].transition, crate::transition::Transition::Dissolve);
        // The composite shot, nested.
        assert_eq!(c[2].nested, Some(1));
        // The text: two lines, left-aligned, its size and colour, placed by its box.
        let t = v[1].clips[0].title.as_ref().expect("a title");
        assert_eq!(t.text, "OK\n!");
        assert_eq!((t.size, t.color, t.align), (120.0, [255, 255, 255, 255], crate::title::TextAlign::Left));
        assert_eq!((t.font.path.as_deref(), t.font.name.as_str()), (None, "Arial"), "named, to be found when opened");
        // Its position, moved to the middle of its box (200 right, 50 up of the origin).
        assert_eq!(v[1].clips[0].transform.position, [-300.0, -350.0]);
        // The sound: linked to its picture, its level plus the track's.
        let a = s.tracks.iter().find(|t| t.kind == TrackKind::Audio).expect("audio");
        let snd = &a.clips[0];
        assert_eq!(snd.audio.volume_db, -9.0);
        assert!(snd.link.is_some() && snd.link == c[1].link);
        // One media file for the video (shared by picture and sound) and the card.
        assert_eq!(p.media.len(), 2);
        assert!(im.notes.iter().any(|n| n.contains("Chroma Key")), "{:?}", im.notes);
        assert!(im.summary.contains("composite shot"), "{}", im.summary);
    }

    #[test]
    fn reads_composite_shots() {
        let im = read(project().as_bytes(), None).expect("read");
        let s = &im.project.sequences[1];
        assert_eq!((s.name.as_str(), s.width, s.height), ("Shot", 1280, 720));
        let v: Vec<&Track> = s.tracks.iter().filter(|t| t.kind == TrackKind::Video).collect();
        // Bottom first: the plane, then the title (the camera left out).
        assert_eq!(v.len(), 2);
        let plane = &v[0].clips[0];
        assert_eq!(plane.plane, Some([255, 0, 0, 255]));
        assert_eq!(plane.transform.position, [10.0, -20.0]);
        assert!((plane.transform.scale[0] - 0.75).abs() < 1e-6 && (plane.transform.scale[1] - 1.5).abs() < 1e-6, "a 960 × 1080 plane in a 1280 × 720 shot");
        assert!((secs_of(plane.duration) - 2.0).abs() < 1e-6, "cut to the shot's length");
        let title = v[1].clips[0].title.as_ref().expect("title");
        assert_eq!((title.text.as_str(), title.align, title.color), ("Hi", crate::title::TextAlign::Center, [0, 255, 0, 255]));
        assert!(im.notes.iter().any(|n| n.contains("Cameras")));
        // A composite shot file on its own.
        let shot = format!("<BiffCompositeShot>{}</BiffCompositeShot>", &project()[project().find("<CompositionAsset").unwrap()..project().find("</CompositionAsset>").unwrap() + 19]);
        let im = read(shot.as_bytes(), None).expect("shot");
        assert_eq!(im.project.sequences.len(), 1);
        assert_eq!(im.project.sequences[0].name, "Shot");
    }

    #[test]
    fn damaged_or_odd_files_are_errors_not_crashes() {
        assert!(read(b"<html/>", None).is_err());
        assert!(read(b"<BiffProject><Project", None).is_err());
        assert!(read(b"<BiffProject><Project/></BiffProject>", None).is_err(), "nothing to bring across");
        // Nonsense numbers are kept in range.
        let odd = project().replace("<EndFrame>150</EndFrame>", "<EndFrame>99999999999999999999</EndFrame>").replace("<db>2</db>", "<db>NaN</db>").replace("Ti=\"500\"", "Ti=\"-1e300\"").replace("<FrameRate>30</FrameRate>", "<FrameRate>0</FrameRate>");
        let _ = read(odd.as_bytes(), None);
        assert!(sniff(b"<?xml version=\"1.0\"?>\n<BiffProject Version=\"0\">"));
        assert!(!sniff(b"<?xml version=\"1.0\"?><PremiereData>"));
    }

    /// Real projects, when a folder of them is named (`REELFORGE_HFP_DIR`): each reads,
    /// with clips on its timeline.
    #[test]
    fn real_projects_read() {
        let Some(dir) = std::env::var_os("REELFORGE_HFP_DIR") else { return };
        for e in std::fs::read_dir(dir).expect("folder").flatten() {
            let p = e.path();
            if !p.extension().is_some_and(|x| x == "hfp" || x == "hfcs") {
                continue;
            }
            let im = read(&std::fs::read(&p).expect("file"), p.parent()).unwrap_or_else(|e| panic!("{}: {e}", p.display()));
            let clips: usize = im.project.sequences.iter().flat_map(|s| &s.tracks).map(|t| t.clips.len()).sum();
            eprintln!("{}: {} | {clips} clips | {:?}", p.file_name().unwrap().to_string_lossy(), im.summary, im.notes);
        }
    }
}
