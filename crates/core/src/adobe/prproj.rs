//! Premiere Pro projects (`.prproj`): a gzipped XML object graph. Each
//! top-level element is an object with an `ObjectID` (referenced by
//! `ObjectRef`) or an `ObjectUID` (referenced by `ObjectURef`). Times are
//! ticks, 254 016 000 000 a second.
//!
//! Sequence → track groups → tracks → track items. A track item has its
//! timeline `Start`/`End`, a `SubClip` whose clip holds the source
//! `InPoint`/`OutPoint`, `PlaybackSpeed` and a `Source` (a media file, or
//! another sequence: a nest), and a component chain: Motion (position as a
//! fraction of the frame, scale %, rotation °, anchor as a fraction of the
//! clip, crop %), Opacity (% and blend mode), the clip's Volume (a gain
//! where 1.0 is +15 dB) and any other effects. Keyframes are written
//! `time,value,interpolation,…;` in the clip's source time; interpolation
//! 0 is linear, 4 hold and 5 Bézier.

// Reads files from anywhere: no position in a list is taken on trust (`get`,
// checked slices), so a damaged or hostile file is an error, never a crash.
#![cfg_attr(not(test), deny(clippy::indexing_slicing))]

// (clippy takes the closures that are called once and then handed on for
// the keyframes, `f(&p.value)` then `curve(.., f, ..)`, for ones called only once)
#![allow(clippy::redundant_closure_call)]

use super::{count, file_name, locate, rate_of, Imported, Notes, MAX_UNPACKED};
use crate::anim::{Curve, Interp, Key};
use crate::audio::VolumeKey;
use crate::project::*;
use crate::time::{FrameRate, Rational, Time};
use crate::transform::BlendMode;
use roxmltree::{Document, Node};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

/// Premiere's ticks a second.
pub const TICKS: i64 = 254_016_000_000;
/// The time of a value that isn't keyframed ("the beginning of time").
const STATIC_T: i64 = -91_445_760_000_000_000;
/// The most clips read from one project (a guard against a hostile file).
const MAX_ITEMS: usize = 200_000;

pub fn ticks(t: i64) -> Time {
    // 705 600 000 of ours a second: a tick is 1/360 of one.
    Time(((t as i128 * Time::TICKS_PER_SECOND as i128) / TICKS as i128).clamp(-(1i128 << 60), 1i128 << 60) as i64)
}

/// Read a Premiere project (gzipped or plain XML); `dir` is the folder it's
/// in (to find media saved by a relative path, or moved with it).
pub fn read(bytes: &[u8], dir: Option<&Path>) -> Result<Imported, String> {
    let xml = unpack(bytes)?;
    let text = String::from_utf8_lossy(&xml);
    let opts = roxmltree::ParsingOptions { allow_dtd: false, nodes_limit: 50_000_000 };
    let doc = Document::parse_with_options(&text, opts).map_err(|e| format!("The Premiere project couldn't be read: {e}"))?;
    let root = doc.root_element();
    if root.tag_name().name() != "PremiereData" {
        return Err("That isn't a Premiere Pro project".into());
    }
    Reader::new(root, dir).run()
}

fn unpack(bytes: &[u8]) -> Result<Vec<u8>, String> {
    if !bytes.starts_with(&[0x1f, 0x8b]) {
        return Ok(bytes.to_vec());
    }
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(bytes).take(MAX_UNPACKED).read_to_end(&mut out).map_err(|e| format!("The Premiere project couldn't be unpacked: {e}"))?;
    Ok(out)
}

/// A child element by tag.
fn kid<'a, 'i>(n: Node<'a, 'i>, tag: &str) -> Option<Node<'a, 'i>> {
    n.children().find(|c| c.is_element() && c.tag_name().name() == tag)
}

/// A descendant along a path of tags ("ClipTrackItem/TrackItem/End").
fn at<'a, 'i>(n: Node<'a, 'i>, path: &str) -> Option<Node<'a, 'i>> {
    path.split('/').try_fold(n, |n, tag| kid(n, tag))
}

fn text<'a>(n: Node<'a, '_>, path: &str) -> Option<&'a str> {
    at(n, path).and_then(|n| n.text()).map(str::trim)
}

fn int(n: Node, path: &str) -> Option<i64> {
    text(n, path)?.parse().ok()
}

fn num(n: Node, path: &str) -> Option<f64> {
    text(n, path)?.parse::<f64>().ok().filter(|v| v.is_finite())
}

/// Children with an `Index`, in that order.
fn indexed<'a, 'i>(n: Node<'a, 'i>, tag: &str) -> Vec<Node<'a, 'i>> {
    let mut v: Vec<(i64, Node)> = n.children().filter(|c| c.is_element() && c.tag_name().name() == tag).enumerate().map(|(i, c)| (c.attribute("Index").and_then(|x| x.parse().ok()).unwrap_or(i as i64), c)).collect();
    v.sort_by_key(|p| p.0);
    v.into_iter().map(|p| p.1).collect()
}

/// A parsed parameter: its value when not keyframed and its keys (source
/// ticks, value, interpolation code).
#[derive(Clone, Debug, Default)]
struct Param {
    value: Vec<f64>,
    keys: Vec<(i64, Vec<f64>, u8)>,
}

/// "x:y" or "v" (or "true"/"false") to numbers.
fn values(s: &str) -> Vec<f64> {
    match s.trim() {
        "true" => vec![1.0],
        "false" => vec![0.0],
        s => s.split(':').filter_map(|v| v.trim().parse::<f64>().ok().filter(|v| v.is_finite())).collect(),
    }
}

fn parse_param(n: Node) -> Param {
    let mut p = Param::default();
    if let Some(s) = text(n, "StartKeyframe") {
        let mut f = s.split(',');
        if let (Some(_), Some(v)) = (f.next(), f.next()) {
            p.value = values(v);
        }
    }
    if let Some(k) = text(n, "Keyframes") {
        for key in k.split(';').take(100_000) {
            let f: Vec<&str> = key.split(',').collect();
            let (Some(t), Some(v)) = (f.first().and_then(|t| t.trim().parse::<i64>().ok()), f.get(1)) else { continue };
            if t == STATIC_T {
                continue;
            }
            let interp = f.get(2).and_then(|i| i.trim().parse::<u8>().ok()).unwrap_or(0);
            let v = values(v);
            if !v.is_empty() {
                p.keys.push((t, v, interp));
            }
        }
        p.keys.sort_by_key(|k| k.0);
    }
    p
}

fn interp_of(code: u8) -> Interp {
    match code {
        4 => Interp::Hold,
        5 => Interp::Ease,
        _ => Interp::Linear,
    }
}

/// Premiere's blend mode codes (the Opacity effect's Blend Mode).
fn blend_of(code: i64) -> Option<BlendMode> {
    Some(match code {
        18 => BlendMode::Normal,
        3 => BlendMode::Darken,
        17 => BlendMode::Multiply,
        1 => BlendMode::ColorBurn,
        11 => BlendMode::Lighten,
        22 => BlendMode::Screen,
        2 => BlendMode::ColorDodge,
        14 => BlendMode::Add,
        19 => BlendMode::Overlay,
        23 => BlendMode::SoftLight,
        8 => BlendMode::HardLight,
        5 => BlendMode::Difference,
        7 => BlendMode::Exclusion,
        10 => BlendMode::Hue,
        21 => BlendMode::Saturation,
        0 => BlendMode::Color,
        16 => BlendMode::Luminosity,
        _ => return None,
    })
}

/// A media file in the project, as read.
struct MediaRead {
    id: MediaId,
    /// Its picture size (none for sound only).
    size: Option<[u32; 2]>,
}

/// What a clip plays.
enum Src<'a, 'i> {
    Media(MediaId, Option<[u32; 2]>),
    Sequence(usize),
    /// Made in Premiere (a graphic, colour matte, adjustment layer…): its title.
    Synthetic(String, Node<'a, 'i>),
    Nothing,
}

struct Reader<'a, 'i> {
    by_id: HashMap<&'a str, Node<'a, 'i>>,
    by_uid: HashMap<&'a str, Node<'a, 'i>>,
    root: Node<'a, 'i>,
    dir: Option<PathBuf>,
    project: Project,
    media: HashMap<&'a str, MediaRead>,
    /// Sequence ObjectUID → its index in the project.
    seq_index: HashMap<&'a str, usize>,
    notes: Notes,
    clips: usize,
    links: HashMap<&'a str, u64>,
}

impl<'a, 'i> Reader<'a, 'i> {
    fn new(root: Node<'a, 'i>, dir: Option<&Path>) -> Self {
        let mut by_id = HashMap::new();
        let mut by_uid = HashMap::new();
        // Only the direct children are objects (Project holds a local scope of its own).
        for o in root.children().filter(|c| c.is_element()) {
            if let Some(id) = o.attribute("ObjectID") {
                by_id.entry(id).or_insert(o);
            }
            if let Some(id) = o.attribute("ObjectUID") {
                by_uid.entry(id).or_insert(o);
            }
        }
        let mut project = Project::new();
        project.sequences.clear();
        Self { by_id, by_uid, root, dir: dir.map(Path::to_path_buf), project, media: HashMap::new(), seq_index: HashMap::new(), notes: Notes::default(), clips: 0, links: HashMap::new() }
    }

    /// The object a reference element points at.
    fn deref(&self, n: Option<Node<'a, 'i>>) -> Option<Node<'a, 'i>> {
        let n = n?;
        if let Some(r) = n.attribute("ObjectRef") {
            return self.by_id.get(r).copied();
        }
        n.attribute("ObjectURef").and_then(|r| self.by_uid.get(r).copied())
    }

    fn follow(&self, n: Node<'a, 'i>, path: &str) -> Option<Node<'a, 'i>> {
        self.deref(at(n, path))
    }

    fn run(mut self) -> Result<Imported, String> {
        let seqs: Vec<Node> = self.root.children().filter(|c| c.is_element() && c.tag_name().name() == "Sequence").collect();
        if seqs.is_empty() && !self.root.children().any(|c| c.tag_name().name() == "Media") {
            return Err("The Premiere project has no sequences or media to bring across".into());
        }
        // Sequences used inside others go after the ones that aren't (the main edit first).
        // (Every sequence has sources of its own, for its project item: only
        // those that clips on a timeline play count.)
        let nested: std::collections::HashSet<&str> = self
            .root
            .children()
            .filter(|c| matches!(c.tag_name().name(), "VideoClipTrackItem" | "AudioClipTrackItem"))
            .filter_map(|item| {
                let clip = self.follow(self.follow(item, "ClipTrackItem/SubClip")?, "Clip")?;
                let src = self.follow(clip, "Clip/Source")?;
                at(src, "SequenceSource/Sequence")?.attribute("ObjectURef")
            })
            .collect();
        let mut order: Vec<Node> = seqs.iter().copied().filter(|s| !s.attribute("ObjectUID").is_some_and(|u| nested.contains(u))).collect();
        // The longest edit first (the main one, not an empty sequence).
        let lens: HashMap<&str, i64> = seqs.iter().filter_map(|s| Some((s.attribute("ObjectUID")?, self.length(*s)))).collect();
        order.sort_by_key(|s| std::cmp::Reverse(s.attribute("ObjectUID").and_then(|u| lens.get(u)).copied().unwrap_or(0)));
        order.extend(seqs.iter().copied().filter(|s| s.attribute("ObjectUID").is_some_and(|u| nested.contains(u))));
        for (i, s) in order.iter().enumerate() {
            if let Some(u) = s.attribute("ObjectUID") {
                self.seq_index.insert(u, i);
            }
            // A placeholder now, so nests can point at it whatever the order.
            self.project.sequences.push(Sequence::new(text(*s, "Name").unwrap_or("Sequence"), 1920, 1080, FrameRate::new(30, 1)));
        }
        for (i, s) in order.iter().enumerate() {
            let built = self.sequence(*s);
            if let Some(slot) = self.project.sequences.get_mut(i) {
                *slot = built;
            }
        }
        if self.project.sequences.is_empty() {
            self.project.sequences.push(Sequence::new("Sequence 1", 1920, 1080, FrameRate::new(30, 1)));
        }
        // Media in the project's bins that no sequence uses come across too.
        let all: Vec<Node> = self.root.children().filter(|c| c.is_element() && c.tag_name().name() == "Media").collect();
        for m in all {
            let path = text(m, "ActualMediaFilePath").or(text(m, "FilePath")).unwrap_or("");
            let real = text(m, "Infinite") != Some("true") && (path.contains(['/', '\\']) || text(m, "RelativePath").is_some_and(|r| !r.is_empty()));
            if real && (self.follow(m, "VideoStream").is_some() || self.follow(m, "AudioStream").is_some()) {
                self.media(m);
            }
        }
        let media = self.project.media.len();
        let summary = format!("{}, {}, {}", count(self.project.sequences.len(), "sequence", "sequences"), count(self.clips, "clip", "clips"), count(media, "media file", "media files"));
        Ok(Imported { project: self.project, notes: self.notes.lines(), summary })
    }

    /// How long a sequence is: its latest clip end (ticks).
    fn length(&self, s: Node<'a, 'i>) -> i64 {
        let mut end = 0;
        if let Some(tg) = kid(s, "TrackGroups") {
            for g in indexed(tg, "TrackGroup").into_iter().filter_map(|g| self.follow(g, "Second")) {
                let Some(tracks) = at(g, "TrackGroup/Tracks") else { continue };
                for t in indexed(tracks, "Track").into_iter().filter_map(|t| self.deref(Some(t))) {
                    let Some(items) = at(t, "ClipTrack/ClipItems/TrackItems") else { continue };
                    for i in indexed(items, "TrackItem").into_iter().filter_map(|i| self.deref(Some(i))) {
                        end = end.max(int(i, "ClipTrackItem/TrackItem/End").unwrap_or(0));
                    }
                }
            }
        }
        end
    }

    fn sequence(&mut self, s: Node<'a, 'i>) -> Sequence {
        let name = text(s, "Name").unwrap_or("Sequence").to_string();
        let mut groups: Vec<Node> = Vec::new();
        if let Some(tg) = kid(s, "TrackGroups") {
            for g in indexed(tg, "TrackGroup") {
                if let Some(o) = self.follow(g, "Second") {
                    groups.push(o);
                }
            }
        }
        let video = groups.iter().copied().find(|g| g.tag_name().name() == "VideoTrackGroup");
        let audio = groups.iter().copied().find(|g| g.tag_name().name() == "AudioTrackGroup");
        let (mut w, mut h) = (1920u32, 1080u32);
        let mut rate = FrameRate::new(30, 1);
        if let Some(v) = video {
            if let Some(r) = text(v, "FrameRect").and_then(rect) {
                (w, h) = (r[0].clamp(2, 16384), r[1].clamp(2, 16384));
            }
            if let Some(t) = int(v, "TrackGroup/FrameRate").filter(|t| *t > 0) {
                rate = rate_of(TICKS as f64 / t as f64);
            }
        }
        let mut seq = Sequence::new(name, w, h, rate);
        seq.tracks.clear();
        // Link groups: each Link's track items share a link id.
        if let Some(links) = at(s, "PersistentGroupContainer/LinkContainer/Links") {
            for l in indexed(links, "Link") {
                let Some(link) = self.deref(Some(l)) else { continue };
                let id = self.project.alloc_id();
                if let Some(items) = at(link, "TrackItemGroup/TrackItems") {
                    for it in indexed(items, "TrackItem") {
                        if let Some(r) = it.attribute("ObjectRef") {
                            self.links.insert(r, id);
                        }
                    }
                }
            }
        }
        let frame = [w as f32, h as f32];
        for (g, kind) in [(video, TrackKind::Video), (audio, TrackKind::Audio)] {
            let Some(g) = g else { continue };
            let Some(tracks) = at(g, "TrackGroup/Tracks") else { continue };
            for (n, t) in indexed(tracks, "Track").into_iter().enumerate() {
                let Some(t) = self.deref(Some(t)) else { continue };
                let label = if kind == TrackKind::Video { "Video" } else { "Audio" };
                let mut track = Track::new(text(t, "ClipTrack/Track/Node/Properties/MZ.TrackName").filter(|s| !s.is_empty()).map(str::to_string).unwrap_or_else(|| format!("{label} {}", n + 1)), kind);
                track.locked = text(t, "ClipTrack/Track/IsLocked") == Some("true");
                track.muted = kind == TrackKind::Audio && self.track_muted(t);
                self.track_items(t, kind, frame, rate, &mut track);
                seq.tracks.push(track);
            }
        }
        if !seq.tracks.iter().any(|t| t.kind == TrackKind::Video) {
            seq.tracks.insert(0, Track::new("Video 1", TrackKind::Video));
        }
        if !seq.tracks.iter().any(|t| t.kind == TrackKind::Audio) {
            seq.tracks.push(Track::new("Audio 1", TrackKind::Audio));
        }
        self.markers(s, &mut seq);
        seq
    }

    /// Is an audio track muted (its fader's Mute on)?
    fn track_muted(&self, t: Node<'a, 'i>) -> bool {
        let Some(chain) = t.descendants().find(|d| d.tag_name().name() == "Components" && d.attribute("ObjectRef").is_some()).and_then(|c| self.deref(Some(c))) else { return false };
        self.components(chain).into_iter().any(|c| self.params(c).iter().any(|(name, _, p)| name == "Mute" && p.value.first().is_some_and(|v| *v >= 0.5)))
    }

    /// A chain's components, in order.
    fn components(&self, chain: Node<'a, 'i>) -> Vec<Node<'a, 'i>> {
        let Some(list) = at(chain, "ComponentChain/Components") else { return Vec::new() };
        indexed(list, "Component").into_iter().filter_map(|c| self.deref(Some(c))).collect()
    }

    /// A component's params: (name, ParameterID, value).
    fn params(&self, c: Node<'a, 'i>) -> Vec<(String, i64, Param)> {
        let Some(list) = c.descendants().find(|d| d.tag_name().name() == "Params") else { return Vec::new() };
        indexed(list, "Param")
            .into_iter()
            .enumerate()
            .filter_map(|(i, p)| {
                let p = self.deref(Some(p))?;
                Some((text(p, "Name").unwrap_or("").to_string(), int(p, "ParameterID").unwrap_or(i as i64 + 1), parse_param(p)))
            })
            .collect()
    }

    fn markers(&self, s: Node<'a, 'i>, seq: &mut Sequence) {
        let Some(m) = self.follow(s, "MarkerOwner/Markers") else { return };
        let Some(list) = kid(m, "Markers") else { return };
        for e in indexed(list, "Marker") {
            let Some(mk) = self.follow(e, "Second") else { continue };
            let Some(json) = text(mk, "DVAMarker") else { continue };
            let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else { continue };
            let d = |path: &str| v.pointer(&format!("/DVAMarker{path}"));
            let ticks_at = d("/mStartTime/ticks");
            let Some(t) = ticks_at.and_then(|t| t.as_i64().or_else(|| t.as_str().and_then(|s| s.parse().ok()))) else { continue };
            let mut name = d("/mName").and_then(|n| n.as_str()).unwrap_or("").to_string();
            if let Some(c) = d("/mComment").and_then(|c| c.as_str()).filter(|c| !c.is_empty()) {
                name = if name.is_empty() { c.to_string() } else { format!("{name}: {c}") };
            }
            let color = d("/mCuePointList")
                .and_then(|l| l.as_array())
                .and_then(|l| l.iter().find_map(|c| c.get("mValue").and_then(|m| m.as_str()).and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok()).and_then(|v| v.get("color").and_then(|c| c.as_u64()))))
                .map(marker_color)
                .unwrap_or_default();
            seq.markers.push(Marker { at: ticks(t).max(Time::ZERO), name, color });
        }
        seq.markers.sort_by_key(|m| m.at);
    }

    /// A media file, read once.
    fn media(&mut self, m: Node<'a, 'i>) -> Option<(MediaId, Option<[u32; 2]>)> {
        let uid = m.attribute("ObjectUID")?;
        if let Some(r) = self.media.get(uid) {
            return Some((r.id, r.size));
        }
        let saved: Vec<&str> = ["ActualMediaFilePath", "FilePath", "RelativePath"].iter().filter_map(|t| text(m, t)).collect();
        let path = locate(&saved, self.dir.as_deref())?;
        let video = self.follow(m, "VideoStream");
        let audio = self.follow(m, "AudioStream");
        let size = video.and_then(|v| text(v, "FrameRect")).and_then(rect);
        let vrate = video.and_then(|v| int(v, "FrameRate")).filter(|t| *t > 0).map(|t| rate_of(TICKS as f64 / t as f64)).unwrap_or(FrameRate::new(30, 1));
        let duration = [video, audio].into_iter().flatten().filter_map(|s| int(s, "Duration")).filter(|d| *d > 0 && *d < TICKS * 360_000).max().map(ticks).unwrap_or(Time::ZERO);
        let still = video.is_some_and(|v| text(v, "IsStill") == Some("true")) || (video.is_some() && audio.is_none() && is_image(&path));
        let info = MediaInfo {
            duration: if still { Time::ZERO } else { duration },
            video: size.map(|s| VideoStreamInfo { width: s[0], height: s[1], frame_rate: vrate, codec: String::new(), time_base: Rational::new(vrate.0.den, vrate.0.num), hdr: None, pixel_aspect: (1, 1) }),
            audio: audio.map(|_| AudioStreamInfo { sample_rate: 48_000, channels: 2, codec: String::new() }),
            container: String::new(),
        };
        let name = text(m, "Title").filter(|t| !t.is_empty()).map(str::to_string).unwrap_or_else(|| file_name(&path.to_string_lossy()));
        if let Some(existing) = self.project.media.iter().find(|a| a.path == path) {
            let id = existing.id;
            self.media.insert(uid, MediaRead { id, size });
            return Some((id, size));
        }
        let id = MediaId(self.project.alloc_id());
        self.project.media.push(MediaAsset { id, path, name, info, conform: None });
        self.media.insert(uid, MediaRead { id, size });
        Some((id, size))
    }

    /// What a clip's source is.
    fn source(&mut self, clip: Node<'a, 'i>) -> Src<'a, 'i> {
        let Some(src) = self.follow(clip, "Clip/Source") else { return Src::Nothing };
        if let Some(seq) = at(src, "SequenceSource/Sequence").and_then(|r| r.attribute("ObjectURef")) {
            return self.seq_index.get(seq).map_or(Src::Nothing, |i| Src::Sequence(*i));
        }
        let Some(m) = self.follow(src, "MediaSource/Media") else { return Src::Nothing };
        // Made in Premiere: no file, or a "file" that's only a number.
        let path = text(m, "ActualMediaFilePath").or(text(m, "FilePath")).unwrap_or("");
        let synthetic = text(m, "Infinite") == Some("true") || (path.is_empty() && text(m, "RelativePath").unwrap_or("").is_empty()) || (!path.is_empty() && !path.contains(['/', '\\', '.']));
        if synthetic {
            return Src::Synthetic(text(m, "Title").unwrap_or("").to_string(), m);
        }
        match self.media(m) {
            Some((id, size)) => Src::Media(id, size),
            None => Src::Nothing,
        }
    }

    fn track_items(&mut self, t: Node<'a, 'i>, kind: TrackKind, frame: [f32; 2], rate: FrameRate, track: &mut Track) {
        let Some(items) = at(t, "ClipTrack/ClipItems/TrackItems") else { return };
        for it in indexed(items, "TrackItem") {
            if self.clips >= MAX_ITEMS {
                self.notes.add("Very many clips: the rest were left out");
                return;
            }
            let Some(item) = self.deref(Some(it)) else { continue };
            let item_id = it.attribute("ObjectRef").unwrap_or("");
            if let Some(c) = self.clip(item, item_id, kind, frame, rate) {
                self.clips += 1;
                track.clips.push(c);
            }
        }
        track.sort();
        // Transitions: video cross dissolves and dips, audio fades and crossfades.
        if let Some(trans) = at(t, "ClipTrack/TransitionItems/TrackItems") {
            for it in indexed(trans, "TrackItem") {
                let Some(tr) = self.deref(Some(it)) else { continue };
                self.transition(tr, kind, track);
            }
        }
    }

    fn clip(&mut self, item: Node<'a, 'i>, item_id: &'a str, kind: TrackKind, frame: [f32; 2], rate: FrameRate) -> Option<Clip> {
        let start = ticks(int(item, "ClipTrackItem/TrackItem/Start").unwrap_or(0));
        let end = ticks(int(item, "ClipTrackItem/TrackItem/End")?);
        if end <= start {
            return None;
        }
        let duration = end - start;
        let sub = self.follow(item, "ClipTrackItem/SubClip")?;
        let clip = self.follow(sub, "Clip")?;
        let in_ticks = int(clip, "Clip/InPoint").unwrap_or(0);
        let out_ticks = int(clip, "Clip/OutPoint");
        let speed = num(clip, "Clip/PlaybackSpeed").map(f64::abs).filter(|s| *s > 1e-4 && *s < 1e4).or_else(|| out_ticks.map(|o| ticks(o - in_ticks).as_secs_f64() / duration.as_secs_f64().max(1e-9)).filter(|s| (*s - 1.0).abs() > 1e-3 && *s > 1e-4 && *s < 1e4)).unwrap_or(1.0);
        let reverse = num(clip, "Clip/PlaybackSpeed").is_some_and(|s| s < 0.0);
        let media_in = ticks(in_ticks);
        let link = self.links.get(item_id).copied();
        let id = ClipId(self.project.alloc_id());
        let (mut c, layer) = match self.source(clip) {
            Src::Media(m, size) => (Clip::new(id, m, start, media_in, duration, link), size.map(|s| [s[0] as f32, s[1] as f32])),
            Src::Sequence(i) => {
                let size = self.project.sequences.get(i).map(|s| [s.width as f32, s.height as f32]);
                (Clip { source_in: media_in, ..Clip::new_nested(id, i, start, duration, link) }, size)
            }
            Src::Synthetic(title, _media) => {
                if kind == TrackKind::Audio {
                    return None;
                }
                let c = self.synthetic(id, &title, item, start, duration);
                return Some(c);
            }
            Src::Nothing => {
                self.notes.add(if kind == TrackKind::Audio { "Sound clips whose file isn't named in the project were left out" } else { "Clips whose file isn't named in the project were left out" });
                return None;
            }
        };
        if (speed - 1.0).abs() > 1e-4 || reverse {
            c.speed = speed as f32;
            c.reverse = reverse;
            // Media time = anchor + speed × clip time, with clip time starting at the in point.
            c.anchor = if reverse { media_in + super::secs(media_in.as_secs_f64() * speed) } else { media_in - super::secs(media_in.as_secs_f64() * speed) };
        }
        if self.follow(clip, "Clip/TimeRemapping").is_some_and(|r| r.descendants().any(|d| d.tag_name().name() == "Keyframes")) {
            self.notes.add("Time remapping (speed ramps) came across as a constant speed");
        }
        let chain = self.follow(item, "ClipTrackItem/ComponentOwner/Components");
        // Source ticks → our clip time (keys are in the clip's source time).
        let to_ct = |t: i64| -> Time {
            let m = ticks(t);
            if (speed - 1.0).abs() > 1e-4 {
                media_in + super::secs((m - media_in).as_secs_f64() / speed)
            } else {
                m
            }
        };
        if kind == TrackKind::Video {
            let layer = layer.unwrap_or(frame);
            if let Some(chain) = chain {
                self.video_chain(chain, &mut c, frame, layer, &to_ct);
            }
        } else if let Some(chain) = chain {
            self.audio_chain(chain, &mut c, &to_ct);
        }
        let _ = rate;
        Some(c)
    }

    /// A clip made in Premiere: a text graphic becomes a title, an
    /// adjustment layer one of ours, a colour matte or black video a plane.
    fn synthetic(&mut self, id: ClipId, title: &str, item: Node<'a, 'i>, start: Time, duration: Time) -> Clip {
        let lower = title.to_ascii_lowercase();
        if lower.contains("adjustment") {
            return Clip::new_adjustment(id, start, duration);
        }
        if lower.contains("black video") {
            return Clip::new_plane(id, start, duration, [0, 0, 0, 255]);
        }
        if lower.contains("matte") {
            self.notes.add("Colour mattes came across grey (pick their colour again)");
            return Clip::new_plane(id, start, duration, [128, 128, 128, 255]);
        }
        if lower.contains("bars") {
            self.notes.add("Bars and tone came across as a grey plane");
            return Clip::new_plane(id, start, duration, [128, 128, 128, 255]);
        }
        let text = self.follow(item, "ClipTrackItem/ComponentOwner/Components").and_then(|chain| self.components(chain).into_iter().find_map(|c| self.graphic_text(c)));
        self.notes.add("Graphics came across as titles with their text: their font, size, style and position are Reelforge's defaults");
        let spec = crate::title::TitleSpec { text: text.filter(|t| !t.trim().is_empty()).unwrap_or_else(|| if title.is_empty() { "Title".into() } else { title.to_string() }), ..Default::default() };
        Clip::new_title(id, start, duration, spec)
    }

    /// The words of a text graphic (its Source Text: a binary record whose
    /// last length-prefixed string is the text, after the font's name).
    fn graphic_text(&self, c: Node<'a, 'i>) -> Option<String> {
        let list = c.descendants().find(|d| d.tag_name().name() == "Params")?;
        for p in indexed(list, "Param") {
            let p = self.deref(Some(p))?;
            if text(p, "Name") != Some("Source Text") {
                continue;
            }
            let b64 = text(p, "StartKeyframeValue").or_else(|| text(p, "Keyframes").and_then(|k| k.split(';').next()).and_then(|k| k.rsplit(',').next()))?;
            let bytes = base64_decode(b64)?;
            return strings_in(&bytes).pop();
        }
        None
    }

    fn video_chain(&mut self, chain: Node<'a, 'i>, c: &mut Clip, frame: [f32; 2], layer: [f32; 2], to_ct: &dyn Fn(i64) -> Time) {
        for comp in self.components(chain) {
            let mn = text(comp, "MatchName").unwrap_or("");
            let shown = text(comp, "Component/DisplayName").unwrap_or(mn).to_string();
            if text(comp, "Component/Bypass") == Some("true") {
                continue;
            }
            let params = self.params(comp);
            let get = |pid: i64| params.iter().find(|p| p.1 == pid).map(|p| &p.2);
            match mn {
                "AE.ADBE Motion" => {
                    place(c, &get, Ids { position: 1, anchor: 6, scale: 2, width: 3, uniform: 4, rotation: 5 }, frame, layer, to_ct);
                    let crop = |pid| get(pid).and_then(|p| p.value.first()).map_or(0.0, |v| (*v as f32 / 100.0).clamp(0.0, 1.0));
                    c.transform.crop = [crop(8), crop(9), crop(10), crop(11)];
                    if [8, 9, 10, 11].iter().any(|pid| get(*pid).is_some_and(|p| !p.keys.is_empty())) {
                        self.notes.add("Keyframed crops came across at their first value");
                    }
                }
                "AE.ADBE Opacity" => {
                    if let Some(p) = get(1) {
                        let o = |v: &[f64]| v.first().map_or(1.0, |x| (*x as f32 / 100.0).clamp(0.0, 1.0));
                        c.transform.opacity = o(&p.value);
                        c.anim.opacity = curve(&p.keys, o, to_ct);
                    }
                    if let Some(code) = get(2).and_then(|p| p.value.first()) {
                        match blend_of(code.round() as i64) {
                            Some(b) => c.blend = b,
                            None => self.notes.add("A blend mode Reelforge doesn't have came across as Normal"),
                        }
                    }
                    if params.iter().any(|p| p.0.contains("Mask")) || comp.descendants().any(|d| d.tag_name().name() == "Masks" && d.has_children()) {
                        self.notes.add("Opacity masks were left out");
                    }
                }
                "AE.ADBE Text" => {}
                // The Transform effect: the clip's own transform, if its Motion is left as it is.
                "AE.ADBE Geometry2" => {
                    let plain = c.transform.position == [0.0, 0.0] && c.transform.scale == [1.0, 1.0] && c.transform.rotation == 0.0 && c.transform.anchor == [0.0, 0.0] && c.anim.position.is_empty() && c.anim.scale.is_empty() && c.anim.rotation.is_empty() && c.anim.anchor.is_empty();
                    if plain {
                        place(c, &get, Ids { position: 2, anchor: 1, scale: 3, width: 4, uniform: 11, rotation: 7 }, frame, layer, to_ct);
                        if let Some(p) = get(8) {
                            let o = |v: &[f64]| v.first().map_or(1.0, |x| (*x as f32 / 100.0).clamp(0.0, 1.0));
                            if c.anim.opacity.is_empty() {
                                c.transform.opacity *= o(&p.value);
                                c.anim.opacity = curve(&p.keys, o, to_ct);
                            }
                        }
                        if get(5).and_then(|p| p.value.first()).is_some_and(|v| v.abs() > 1e-6) {
                            self.notes.add("The Transform effect's skew was left out");
                        }
                    } else {
                        self.notes.add("A Transform effect on top of a changed Motion was left out");
                    }
                }
                _ => {
                    if let Some(e) = effect_of(mn, &params) {
                        c.effects.push(e);
                    } else {
                        self.notes.add(format!("Effect left out: {}", if shown.is_empty() { mn.to_string() } else { shown }));
                    }
                }
            }
        }
    }

    fn audio_chain(&mut self, chain: Node<'a, 'i>, c: &mut Clip, to_ct: &dyn Fn(i64) -> Time) {
        for comp in self.components(chain) {
            let mn = text(comp, "FilterMatchName").or(text(comp, "MatchName")).unwrap_or("");
            if mn.starts_with("Internal Volume") {
                let params = self.params(comp);
                if let Some((_, _, p)) = params.iter().find(|p| p.0 == "Level") {
                    // A gain where 1.0 is +15 dB.
                    let db = |g: f64| if g <= 1e-6 { crate::audio::MIN_DB } else { ((20.0 * g.log10()) as f32 + 15.0).clamp(crate::audio::MIN_DB, crate::audio::MAX_DB) };
                    if let Some(g) = p.value.first() {
                        c.audio.volume_db = db(*g);
                    }
                    c.audio.volume_keys = p.keys.iter().filter_map(|(t, v, _)| Some(VolumeKey { at: to_ct(*t), db: db(*v.first()?) })).collect();
                    c.audio.volume_keys.sort_by_key(|k| k.at);
                    c.audio.volume_keys.dedup_by_key(|k| k.at);
                }
            } else if mn.starts_with("Internal Channel Volume") || mn.is_empty() {
            } else {
                let shown = text(comp, "Component/DisplayName").or(text(comp, "AudioComponent/Component/DisplayName")).unwrap_or(mn);
                self.notes.add(format!("Audio effect left out: {shown}"));
            }
        }
    }

    fn transition(&mut self, tr: Node<'a, 'i>, kind: TrackKind, track: &mut Track) {
        let s = ticks(tr.descendants().find(|d| d.tag_name().name() == "Start").and_then(|n| n.text()).and_then(|t| t.trim().parse().ok()).unwrap_or(0));
        let Some(e) = tr.descendants().find(|d| d.tag_name().name() == "End").and_then(|n| n.text()).and_then(|t| t.trim().parse::<i64>().ok()).map(ticks) else { return };
        if e <= s {
            return;
        }
        let d = e - s;
        let align = tr.descendants().find(|d| d.tag_name().name() == "Alignment").and_then(|n| n.text()).and_then(|t| t.trim().parse::<i64>().ok()).map(ticks);
        let outgoing = text(tr, "TransitionTrackItem/HasOutgoingClip").is_none_or(|v| v == "true");
        let incoming = text(tr, "TransitionTrackItem/HasIncomingClip").is_none_or(|v| v == "true");
        let mn = tr.descendants().find(|d| d.tag_name().name() == "MatchName").and_then(|n| n.text()).unwrap_or("").to_string();
        let cut = align.map_or(s + Time(d.0 / 2), |a| s + a);
        let near = |a: Time, b: Time| (a - b).0.abs() <= Time::TICKS_PER_SECOND / 200;
        match kind {
            TrackKind::Video => {
                let Some(next) = track.clips.iter_mut().find(|c| near(c.timeline_start, cut)) else {
                    self.notes.add("Transitions at the start or end of a clip were left out");
                    return;
                };
                next.dissolve_in = d;
                next.transition = match mn.as_str() {
                    m if m.contains("Dip to Black") || m.contains("DipToBlack") => crate::transition::Transition::DipToBlack,
                    m if m.contains("Dip to White") || m.contains("DipToWhite") => crate::transition::Transition::DipToWhite,
                    m if m.contains("Dissolve") || m.is_empty() => crate::transition::Transition::Dissolve,
                    m => {
                        self.notes.add(format!("Transition came across as a cross dissolve: {}", m.trim_start_matches("AE.ADBE ").trim_start_matches("ADBE ")));
                        crate::transition::Transition::Dissolve
                    }
                };
            }
            TrackKind::Audio => match (outgoing, incoming) {
                (true, true) => {
                    if let Some(next) = track.clips.iter_mut().find(|c| near(c.timeline_start, cut)) {
                        next.audio.crossfade_in = d;
                    }
                }
                (false, true) => {
                    if let Some(c) = track.clips.iter_mut().find(|c| near(c.timeline_start, s)) {
                        c.audio.fade_in = d;
                    }
                }
                (true, false) => {
                    if let Some(c) = track.clips.iter_mut().find(|c| near(c.timeline_end(), e)) {
                        c.audio.fade_out = d;
                    }
                }
                _ => {}
            },
        }
    }
}

/// Which parameters of an effect place the clip (Motion, Transform).
struct Ids {
    position: i64,
    anchor: i64,
    /// Scale, or Scale Height when not uniform.
    scale: i64,
    width: i64,
    uniform: i64,
    rotation: i64,
}

/// Position (a fraction of the frame), anchor (a fraction of the clip's
/// own picture), scale (%) and rotation (°) to the clip's transform.
fn place<'p>(c: &mut Clip, get: &dyn Fn(i64) -> Option<&'p Param>, ids: Ids, frame: [f32; 2], layer: [f32; 2], to_ct: &dyn Fn(i64) -> Time) {
    let half = |v: &[f64], i: usize, size: f32| v.get(i).map_or(0.0, |x| (*x as f32 - 0.5) * size);
    // Ours: from the frame's centre, and the clip's, in pixels.
    if let Some(p) = get(ids.position) {
        let px = |v: &[f64]| [half(v, 0, frame[0]), half(v, 1, frame[1])];
        c.transform.position = px(&p.value);
        c.anim.position = curve(&p.keys, px, to_ct);
    }
    if let Some(p) = get(ids.anchor) {
        let px = |v: &[f64]| [half(v, 0, layer[0]), half(v, 1, layer[1])];
        c.transform.anchor = px(&p.value);
        c.anim.anchor = curve(&p.keys, px, to_ct);
    }
    let uniform = get(ids.uniform).and_then(|p| p.value.first()).is_none_or(|v| *v >= 0.5);
    let pct = |v: &[f64]| v.first().map_or(1.0, |x| *x as f32 / 100.0);
    match (get(ids.scale), get(ids.width).filter(|_| !uniform)) {
        (Some(h), Some(w)) => {
            c.transform.scale = [pct(&w.value), pct(&h.value)];
            c.anim.scale = merge(w, h, |a, b| [pct(&a), pct(&b)], to_ct);
        }
        (Some(s), None) => {
            c.transform.scale = [pct(&s.value); 2];
            c.anim.scale = curve(&s.keys, |v| [pct(v); 2], to_ct);
        }
        _ => {}
    }
    if let Some(p) = get(ids.rotation) {
        let deg = |v: &[f64]| v.first().map_or(0.0, |x| *x as f32);
        c.transform.rotation = deg(&p.value);
        c.anim.rotation = curve(&p.keys, deg, to_ct);
    }
}

/// A few of Premiere's effects that have a match here.
fn effect_of(match_name: &str, params: &[(String, i64, Param)]) -> Option<crate::effects::Effect> {
    use crate::effects::{Effect, EffectKind};
    let first = |name: &str| params.iter().find(|p| p.0 == name).and_then(|p| p.2.value.first().copied());
    match match_name {
        "AE.ADBE Gaussian Blur 2" | "AE.ADBE Gaussian Blur" => {
            let mut e = Effect::new(EffectKind::Blur);
            if let (Some(v), Some(p)) = (first("Blurriness"), e.params.get_mut(0)) {
                p.value = v as f32;
            }
            Some(e)
        }
        _ => None,
    }
}

/// A Premiere marker colour (0xAABBGGRR) to the nearest of ours.
fn marker_color(n: u64) -> MarkerColor {
    let rgb = [(n & 0xff) as i32, ((n >> 8) & 0xff) as i32, ((n >> 16) & 0xff) as i32];
    MarkerColor::ALL
        .into_iter()
        .min_by_key(|c| {
            let q = c.rgb();
            rgb.iter().zip(q).map(|(a, b)| (a - b as i32).pow(2)).sum::<i32>()
        })
        .unwrap_or_default()
}

/// "0,0,1920,1080" → [1920, 1080].
fn rect(s: &str) -> Option<[u32; 2]> {
    let v: Vec<i64> = s.split(',').filter_map(|x| x.trim().parse().ok()).collect();
    let (l, t, r, b) = (*v.first()?, *v.get(1)?, *v.get(2)?, *v.get(3)?);
    let (w, h) = (r - l, b - t);
    (w > 0 && h > 0 && w < 100_000 && h < 100_000).then_some([w as u32, h as u32])
}

fn is_image(p: &Path) -> bool {
    p.extension().is_some_and(|e| matches!(e.to_string_lossy().to_ascii_lowercase().as_str(), "png" | "jpg" | "jpeg" | "tif" | "tiff" | "psd" | "bmp" | "gif" | "exr" | "dpx" | "tga" | "webp" | "ai" | "heic"))
}

/// Keys to one of our curves, each value converted by `f`.
fn curve<T, F: Fn(&[f64]) -> T>(keys: &[(i64, Vec<f64>, u8)], f: F, to_ct: &dyn Fn(i64) -> Time) -> Curve<T> {
    let mut out: Vec<Key<T>> = keys.iter().map(|(t, v, i)| Key::new(to_ct(*t), f(v), interp_of(*i))).collect();
    out.sort_by_key(|k| k.at);
    out.dedup_by_key(|k| k.at);
    Curve(out)
}

/// Two parameters keyed apart (scale width and height) as one curve: a key
/// wherever either has one, each part as its own keys give it there.
fn merge<T, F: Fn(Vec<f64>, Vec<f64>) -> T>(a: &Param, b: &Param, f: F, to_ct: &dyn Fn(i64) -> Time) -> Curve<T> {
    if a.keys.is_empty() && b.keys.is_empty() {
        return Curve(Vec::new());
    }
    let mut times: Vec<i64> = a.keys.iter().chain(&b.keys).map(|k| k.0).collect();
    times.sort_unstable();
    times.dedup();
    let at = |p: &Param, t: i64| -> (Vec<f64>, u8) {
        if p.keys.is_empty() {
            return (p.value.clone(), 0);
        }
        let i = p.keys.partition_point(|k| k.0 <= t);
        match (i.checked_sub(1).and_then(|j| p.keys.get(j)), p.keys.get(i)) {
            (Some(k), _) if k.0 == t => (k.1.clone(), k.2),
            (Some(k), Some(n)) if k.2 != 4 && n.0 > k.0 => {
                let u = (t - k.0) as f64 / (n.0 - k.0) as f64;
                (k.1.iter().zip(&n.1).map(|(x, y)| x + (y - x) * u).collect(), k.2)
            }
            (Some(k), _) => (k.1.clone(), k.2),
            (None, Some(n)) => (n.1.clone(), n.2),
            (None, None) => (p.value.clone(), 0),
        }
    };
    let keys = times
        .into_iter()
        .map(|t| {
            let ((va, ia), (vb, _)) = (at(a, t), at(b, t));
            Key::new(to_ct(t), f(va, vb), interp_of(ia))
        })
        .collect();
    Curve(keys)
}

/// Standard base64 (as Premiere writes binary values).
fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let val = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => (c - b'A') as u32,
            b'a'..=b'z' => (c - b'a' + 26) as u32,
            b'0'..=b'9' => (c - b'0' + 52) as u32,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        })
    };
    let clean: Vec<u8> = s.bytes().filter(|c| !c.is_ascii_whitespace() && *c != b'=').collect();
    if clean.len() > 16 << 20 {
        return None;
    }
    let mut out = Vec::with_capacity(clean.len() * 3 / 4);
    for chunk in clean.chunks(4) {
        let mut acc = 0u32;
        for (i, c) in chunk.iter().enumerate() {
            acc |= val(*c)? << (18 - 6 * i);
        }
        let n = chunk.len();
        out.push((acc >> 16) as u8);
        if n > 2 {
            out.push((acc >> 8) as u8);
        }
        if n > 3 {
            out.push(acc as u8);
        }
    }
    Some(out)
}

/// The UTF-8 strings in a binary record stored with a 32-bit length in
/// front and a NUL after, in order.
fn strings_in(b: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 4 < b.len() {
        let Some(Ok(len)) = b.get(i..i + 4).map(<[u8; 4]>::try_from) else { break };
        let n = u32::from_le_bytes(len) as usize;
        if n > 0 && n < 100_000 {
            if let (Some(s), Some(0)) = (b.get(i + 4..i + 4 + n), b.get(i + 4 + n)) {
                if let Ok(t) = std::str::from_utf8(s) {
                    if t.chars().all(|c| !c.is_control() || matches!(c, '\n' | '\r' | '\t')) {
                        out.push(t.replace('\r', "\n"));
                        i += 4 + n;
                        continue;
                    }
                }
            }
        }
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small project as Premiere writes it: a 23.976 sequence with a
    /// linked clip moved and scaled, its volume keyed, a cross dissolve
    /// into a second clip, a marker, and a nest of a second sequence.
    pub(crate) const SAMPLE: &str = r#"<?xml version="1.0" encoding="UTF-8" ?>
<PremiereData Version="3">
	<Project ObjectRef="1"/>
	<Project ObjectID="1"><Node/></Project>
	<Sequence ObjectUID="seq-main"><Name>Main edit</Name>
		<MarkerOwner Version="1"><Markers ObjectRef="90"/></MarkerOwner>
		<PersistentGroupContainer><LinkContainer><Links><Link Index="0" ObjectRef="80"/></Links></LinkContainer></PersistentGroupContainer>
		<TrackGroups><TrackGroup Index="0"><First>228cda18</First><Second ObjectRef="10"/></TrackGroup><TrackGroup Index="1"><First>80b8e3d5</First><Second ObjectRef="11"/></TrackGroup></TrackGroups>
	</Sequence>
	<Sequence ObjectUID="seq-inner"><Name>Inner</Name>
		<TrackGroups><TrackGroup Index="0"><Second ObjectRef="12"/></TrackGroup></TrackGroups>
	</Sequence>
	<VideoTrackGroup ObjectID="10"><TrackGroup><Tracks><Track Index="0" ObjectURef="vt1"/><Track Index="1" ObjectURef="vt2"/></Tracks><FrameRate>10594584000</FrameRate></TrackGroup><FrameRect>0,0,1920,1080</FrameRect></VideoTrackGroup>
	<AudioTrackGroup ObjectID="11"><TrackGroup><Tracks><Track Index="0" ObjectURef="at1"/></Tracks><FrameRate>5292000</FrameRate></TrackGroup></AudioTrackGroup>
	<VideoTrackGroup ObjectID="12"><TrackGroup><Tracks><Track Index="0" ObjectURef="vt3"/></Tracks><FrameRate>10160640000</FrameRate></TrackGroup><FrameRect>0,0,1280,720</FrameRect></VideoTrackGroup>
	<VideoClipTrack ObjectUID="vt1"><ClipTrack><Track><Node><Properties><MZ.TrackName>Pictures</MZ.TrackName></Properties></Node><Index>0</Index><IsLocked>true</IsLocked></Track>
		<ClipItems><TrackItems><TrackItem Index="0" ObjectRef="20"/><TrackItem Index="1" ObjectRef="21"/></TrackItems></ClipItems>
		<TransitionItems><TrackItems><TrackItem Index="0" ObjectRef="70"/></TrackItems></TransitionItems></ClipTrack></VideoClipTrack>
	<VideoClipTrack ObjectUID="vt2"><ClipTrack><Track><Index>1</Index></Track><ClipItems><TrackItems><TrackItem Index="0" ObjectRef="22"/></TrackItems></ClipItems></ClipTrack></VideoClipTrack>
	<VideoClipTrack ObjectUID="vt3"><ClipTrack><Track><Index>0</Index></Track><ClipItems><TrackItems><TrackItem Index="0" ObjectRef="23"/></TrackItems></ClipItems></ClipTrack></VideoClipTrack>
	<AudioClipTrack ObjectUID="at1"><ClipTrack><Track><Index>0</Index></Track><ClipItems><TrackItems><TrackItem Index="0" ObjectRef="24"/></TrackItems></ClipItems></ClipTrack></AudioClipTrack>
	<VideoClipTrackItem ObjectID="20"><ClipTrackItem><ComponentOwner><Components ObjectRef="40"/></ComponentOwner><TrackItem><End>1016064000000</End></TrackItem><SubClip ObjectRef="30"/></ClipTrackItem></VideoClipTrackItem>
	<VideoClipTrackItem ObjectID="21"><ClipTrackItem><TrackItem><Start>1016064000000</Start><End>2032128000000</End></TrackItem><SubClip ObjectRef="31"/></ClipTrackItem></VideoClipTrackItem>
	<VideoClipTrackItem ObjectID="22"><ClipTrackItem><TrackItem><Start>254016000000</Start><End>762048000000</End></TrackItem><SubClip ObjectRef="32"/></ClipTrackItem></VideoClipTrackItem>
	<VideoClipTrackItem ObjectID="23"><ClipTrackItem><TrackItem><End>508032000000</End></TrackItem><SubClip ObjectRef="33"/></ClipTrackItem></VideoClipTrackItem>
	<AudioClipTrackItem ObjectID="24"><ClipTrackItem><ComponentOwner><Components ObjectRef="41"/></ComponentOwner><TrackItem><End>1016064000000</End></TrackItem><SubClip ObjectRef="34"/></ClipTrackItem></AudioClipTrackItem>
	<VideoTransitionTrackItem ObjectID="70"><TransitionTrackItem><TrackItem><Start>889056000000</Start><End>1143072000000</End></TrackItem><HasOutgoingClip>true</HasOutgoingClip><HasIncomingClip>true</HasIncomingClip><Alignment>127008000000</Alignment><MatchName>AE.ADBE Cross Dissolve New</MatchName></TransitionTrackItem></VideoTransitionTrackItem>
	<SubClip ObjectID="30"><Clip ObjectRef="50"/><Name>a.mp4</Name></SubClip>
	<SubClip ObjectID="31"><Clip ObjectRef="51"/><Name>b.mp4</Name></SubClip>
	<SubClip ObjectID="32"><Clip ObjectRef="52"/><Name>Inner</Name></SubClip>
	<SubClip ObjectID="33"><Clip ObjectRef="53"/><Name>a.mp4</Name></SubClip>
	<SubClip ObjectID="34"><Clip ObjectRef="54"/><Name>a.mp4</Name></SubClip>
	<VideoClip ObjectID="50"><Clip><Source ObjectRef="60"/><InPoint>254016000000</InPoint><OutPoint>1270080000000</OutPoint></Clip></VideoClip>
	<VideoClip ObjectID="51"><Clip><Source ObjectRef="61"/><PlaybackSpeed>2</PlaybackSpeed><InPoint>0</InPoint><OutPoint>2032128000000</OutPoint></Clip></VideoClip>
	<VideoClip ObjectID="52"><Clip><Source ObjectRef="62"/><InPoint>0</InPoint><OutPoint>508032000000</OutPoint></Clip></VideoClip>
	<VideoClip ObjectID="53"><Clip><Source ObjectRef="60"/><InPoint>0</InPoint><OutPoint>508032000000</OutPoint></Clip></VideoClip>
	<AudioClip ObjectID="54"><Clip><Source ObjectRef="63"/><InPoint>254016000000</InPoint><OutPoint>1270080000000</OutPoint></Clip></AudioClip>
	<VideoMediaSource ObjectID="60"><MediaSource><Media ObjectURef="media-a"/></MediaSource></VideoMediaSource>
	<VideoMediaSource ObjectID="61"><MediaSource><Media ObjectURef="media-b"/></MediaSource></VideoMediaSource>
	<VideoSequenceSource ObjectID="62"><SequenceSource><Sequence ObjectURef="seq-inner"/></SequenceSource></VideoSequenceSource>
	<AudioMediaSource ObjectID="63"><MediaSource><Media ObjectURef="media-a"/></MediaSource></AudioMediaSource>
	<Media ObjectUID="media-a"><VideoStream ObjectRef="65"/><AudioStream ObjectRef="66"/><FilePath>C:\Footage\a.mp4</FilePath><Title>a.mp4</Title></Media>
	<Media ObjectUID="media-b"><VideoStream ObjectRef="67"/><RelativePath>media/b.mp4</RelativePath><Title>b.mp4</Title></Media>
	<VideoStream ObjectID="65"><FrameRate>10594584000</FrameRate><Duration>2540160000000</Duration><FrameRect>0,0,1280,720</FrameRect></VideoStream>
	<AudioStream ObjectID="66"><Duration>2540160000000</Duration></AudioStream>
	<VideoStream ObjectID="67"><FrameRate>10594584000</FrameRate><Duration>5080320000000</Duration><FrameRect>0,0,1920,1080</FrameRect></VideoStream>
	<VideoComponentChain ObjectID="40"><ComponentChain><Components><Component Index="0" ObjectRef="42"/><Component Index="1" ObjectRef="43"/><Component Index="2" ObjectRef="44"/></Components></ComponentChain></VideoComponentChain>
	<VideoFilterComponent ObjectID="42"><Component><Params><Param Index="0" ObjectRef="100"/><Param Index="1" ObjectRef="101"/><Param Index="2" ObjectRef="102"/><Param Index="3" ObjectRef="103"/><Param Index="4" ObjectRef="104"/><Param Index="5" ObjectRef="105"/></Params><DisplayName>Motion</DisplayName></Component><MatchName>AE.ADBE Motion</MatchName></VideoFilterComponent>
	<VideoFilterComponent ObjectID="43"><Component><Params><Param Index="0" ObjectRef="106"/><Param Index="1" ObjectRef="107"/></Params><DisplayName>Opacity</DisplayName></Component><MatchName>AE.ADBE Opacity</MatchName></VideoFilterComponent>
	<VideoFilterComponent ObjectID="44"><Component><Params/><DisplayName>Lumetri Color</DisplayName></Component><MatchName>AE.ADBE Lumetri</MatchName></VideoFilterComponent>
	<PointComponentParam ObjectID="100"><Name>Position</Name><StartKeyframe>-91445760000000000,0.75:0.5,0,0,0,0,0,0,5,4,0,0,0,0</StartKeyframe><Keyframes>254016000000,0.25:0.5,0,0,0,0,0,0,5,4,0,0,0,0;762048000000,0.75:0.5,4,0,0,0,0,0,5,4,0,0,0,0;</Keyframes><ParameterID>1</ParameterID></PointComponentParam>
	<VideoComponentParam ObjectID="101"><Name>Scale</Name><StartKeyframe>-91445760000000000,50.,0,0,0,0,0,0</StartKeyframe><ParameterID>2</ParameterID></VideoComponentParam>
	<VideoComponentParam ObjectID="102"><Name>Scale Width</Name><StartKeyframe>-91445760000000000,100.,0,0,0,0,0,0</StartKeyframe><ParameterID>3</ParameterID></VideoComponentParam>
	<VideoComponentParam ObjectID="103"><StartKeyframe>-91445760000000000,true,0,0,0,0,0,0</StartKeyframe><ParameterID>4</ParameterID></VideoComponentParam>
	<VideoComponentParam ObjectID="104"><Name>Rotation</Name><StartKeyframe>-91445760000000000,15.,0,0,0,0,0,0</StartKeyframe><ParameterID>5</ParameterID></VideoComponentParam>
	<PointComponentParam ObjectID="105"><Name>Anchor Point</Name><StartKeyframe>-91445760000000000,0.5:0.5,0,0,0,0,0,0,5,4,0,0,0,0</StartKeyframe><ParameterID>6</ParameterID></PointComponentParam>
	<VideoComponentParam ObjectID="106"><Name>Opacity</Name><StartKeyframe>-91445760000000000,80.,0,0,0,0,0,0</StartKeyframe><ParameterID>1</ParameterID></VideoComponentParam>
	<VideoComponentParam ObjectID="107"><Name>Blend Mode</Name><StartKeyframe>-91445760000000000,22,0,0,0,0,0,0</StartKeyframe><ParameterID>2</ParameterID></VideoComponentParam>
	<AudioComponentChain ObjectID="41"><ComponentChain><Components><Component Index="0" ObjectRef="45"/></Components></ComponentChain></AudioComponentChain>
	<AudioFilterComponent ObjectID="45"><AudioComponent><Component><Params><Param Index="0" ObjectRef="108"/></Params></Component></AudioComponent><FilterMatchName>Internal Volume Stereo</FilterMatchName></AudioFilterComponent>
	<AudioComponentParam ObjectID="108"><Name>Level</Name><StartKeyframe>-91445760000000000,0.177827939391,0,0,0,0,0,0</StartKeyframe><Keyframes>254016000000,0.177827939391,0,0,0,0,0,0;508032000000,0.044668357819,0,0,0,0,0,0;</Keyframes></AudioComponentParam>
	<Link ObjectID="80"><TrackItemGroup><TrackItems><TrackItem Index="0" ObjectRef="20"/><TrackItem Index="1" ObjectRef="24"/></TrackItems></TrackItemGroup></Link>
	<Markers ObjectID="90"><Markers><Marker Index="0"><First>m1</First><Second ObjectRef="91"/></Marker></Markers></Markers>
	<Marker ObjectID="91"><DVAMarker>{"DVAMarker":{"mComment":"check","mName":"Fix","mStartTime":{"ticks":508540032000},"mType":"Comment","mCuePointList":[{"mKey":"k","mValue":"{\"color\":4280578025}"}]}}</DVAMarker></Marker>
</PremiereData>
"#;

    fn sec(t: Time) -> f64 {
        (t.as_secs_f64() * 1000.0).round() / 1000.0
    }

    #[test]
    fn a_premiere_edit_comes_across() {
        let im = read(SAMPLE.as_bytes(), None).unwrap();
        // (`REELFORGE_FUZZ_SEEDS=dir`: the sample, gzipped as Premiere saves it, and as a
        // Reelforge project, to start the fuzzers from: see fuzz/seeds.py)
        if let Ok(dir) = std::env::var("REELFORGE_FUZZ_SEEDS") {
            use std::io::Write;
            let dir = std::path::Path::new(&dir);
            let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            let _ = gz.write_all(SAMPLE.as_bytes());
            let _ = std::fs::write(dir.join("sample.prproj.xml"), SAMPLE);
            let _ = std::fs::write(dir.join("sample.prproj"), gz.finish().unwrap_or_default());
            let _ = std::fs::write(dir.join("from-premiere.rfproj"), serde_json::to_vec_pretty(&im.project).unwrap_or_default());
        }
        let p = &im.project;
        assert_eq!(p.sequences.len(), 2);
        let s = &p.sequences[0];
        assert_eq!((s.name.as_str(), s.width, s.height, s.frame_rate), ("Main edit", 1920, 1080, FrameRate::new(24000, 1001)));
        assert_eq!(p.sequences[1].name, "Inner");
        assert_eq!((p.sequences[1].width, p.sequences[1].frame_rate), (1280, FrameRate::new(25, 1)));
        let v1 = &s.tracks[0];
        assert_eq!((v1.name.as_str(), v1.locked, v1.kind), ("Pictures", true, TrackKind::Video));
        let a = &v1.clips[0];
        assert_eq!((sec(a.timeline_start), sec(a.duration), sec(a.source_in)), (0.0, 4.0, 1.0));
        // Moved right a quarter of the frame, half size, turned, 80 % and Screen.
        assert_eq!(a.transform.position, [480.0, 0.0]);
        assert_eq!((a.transform.scale, a.transform.rotation, a.transform.opacity, a.blend), ([0.5, 0.5], 15.0, 0.8, BlendMode::Screen));
        let k = &a.anim.position.0;
        assert_eq!(k.len(), 2);
        assert_eq!((sec(k[0].at), k[0].value, k[1].interp), (1.0, [-480.0, 0.0], Interp::Hold));
        assert!(im.notes.iter().any(|n| n.contains("Lumetri Color")), "{:?}", im.notes);
        // At double speed; the cross dissolve into it, centred on the cut.
        let b = &v1.clips[1];
        assert_eq!((sec(b.timeline_start), b.speed), (4.0, 2.0));
        assert_eq!(b.media_at(b.timeline_start + Time::from_secs_f64(1.0)), Time::from_secs_f64(2.0));
        assert_eq!(sec(b.dissolve_in), 1.0);
        // The nest on V2.
        let n = &s.tracks[1].clips[0];
        assert_eq!((n.nested, sec(n.timeline_start)), (Some(1), 1.0));
        // Sound: linked to the picture, its volume keyed (0 then −12 dB).
        let au = s.tracks.iter().find(|t| t.kind == TrackKind::Audio).unwrap();
        let sound = &au.clips[0];
        assert!(sound.link.is_some() && sound.link == a.link);
        assert!((sound.audio.volume_db).abs() < 0.01);
        assert_eq!(sound.audio.volume_keys.len(), 2);
        assert!((sound.audio.volume_keys[1].db + 12.0).abs() < 0.01);
        // Media by path; the marker, orange → red.
        assert_eq!(p.media.len(), 2);
        assert!(p.media.iter().any(|m| m.path == *"C:\\Footage\\a.mp4" && m.info.video.as_ref().is_some_and(|v| v.width == 1280)));
        assert_eq!(s.markers.len(), 1);
        assert_eq!((s.markers[0].name.as_str(), sec(s.markers[0].at), s.markers[0].color), ("Fix: check", 2.002, MarkerColor::Red));
        assert!(im.summary.contains("2 sequences"), "{}", im.summary);
    }

    #[test]
    fn damaged_projects_are_errors_not_crashes() {
        assert!(read(b"", None).is_err());
        assert!(read(b"<PremiereData>", None).is_err());
        assert!(read(b"<Other/>", None).is_err());
        assert!(read(&[0x1f, 0x8b, 0, 0, 1, 2], None).is_err());
        // References to nothing, and nonsense numbers, are skipped.
        let bad = SAMPLE.replace("ObjectRef=\"50\"", "ObjectRef=\"999\"").replace("<End>2032128000000</End>", "<End>-5</End>").replace("10594584000</FrameRate></TrackGroup><FrameRect>", "0</FrameRate></TrackGroup><FrameRect>");
        let im = read(bad.as_bytes(), None).unwrap();
        assert!(im.project.sequences[0].tracks[0].clips.is_empty());
    }

    #[test]
    fn text_in_a_graphic_record() {
        let mut b = vec![0u8; 8];
        for s in ["Arial-BoldMT", "Hello there"] {
            b.extend((s.len() as u32).to_le_bytes());
            b.extend(s.as_bytes());
            b.push(0);
        }
        assert_eq!(strings_in(&b), vec!["Arial-BoldMT".to_string(), "Hello there".to_string()]);
        assert_eq!(base64_decode("SGVsbG8=").unwrap(), b"Hello");
        assert!(base64_decode("@@").is_none());
    }
}
