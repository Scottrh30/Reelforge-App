//! After Effects projects (`.aep`): a big-endian RIFF ("RIFX", form `Egg!`)
//! of chunks. `LIST Fold` holds the project's items, each a `LIST Item`
//! with an `idta` (type: 1 folder, 4 composition, 7 footage; and its id), a
//! `Utf8` name and then, by type:
//!
//! - a folder: `LIST Sfdr` of more items;
//! - footage: `LIST Pin ` with `sspc` (size, duration, frame rate, audio),
//!   `opti` (`Soli` for a solid and its colour) and `LIST Als2` → `alas`
//!   (JSON with the file's `fullpath`);
//! - a composition: `cdta` (size, frame rate, duration) and a `LIST Layr`
//!   per layer, top layer first: `ldta` (id, start, in and out as
//!   dividend/divisor seconds, stretch, switches, source item, blend mode,
//!   track matte, layer type, parent), a `Utf8` name and a `LIST tdgp` of
//!   properties.
//!
//! A property group (`LIST tdgp`) is pairs of a `tdmn` match name and its
//! `LIST`: `tdgp` (a group), `tdbs` (a property: `tdb4` with its number of
//! values, whether it's spatial and its keyframe ticks a second; `cdat`
//! its value as doubles; `LIST list` → `lhd3` (count, record size) and
//! `ldat` (the keyframes)), or `otst` (orientation). Transform properties
//! at their defaults aren't written at all.

use super::{count, file_name, locate, rate_of, secs, Imported, Notes};
use crate::anim::{Curve, Interp, Key};
use crate::project::*;
use crate::time::{FrameRate, Rational, Time};
use crate::transform::BlendMode;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// How deep chunks may nest (a guard against a hostile file).
const MAX_DEPTH: usize = 64;
/// The most layers read from one project.
const MAX_LAYERS: usize = 100_000;

#[derive(Clone, Copy, Debug)]
struct Chunk {
    id: [u8; 4],
    /// A LIST's type.
    list: Option<[u8; 4]>,
    start: usize,
    end: usize,
}

struct Riff<'a> {
    d: &'a [u8],
}

impl<'a> Riff<'a> {
    /// The chunks in `start..end`.
    fn chunks(&self, start: usize, end: usize) -> Vec<Chunk> {
        let mut out = Vec::new();
        let end = end.min(self.d.len());
        let mut p = start;
        while p.saturating_add(8) <= end {
            let Some(id) = self.d.get(p..p + 4).and_then(|s| <[u8; 4]>::try_from(s).ok()) else { break };
            let Some(n) = self.u32(p + 4) else { break };
            let n = n as usize;
            let body = p + 8;
            let Some(stop) = body.checked_add(n).filter(|s| *s <= end) else { break };
            if &id == b"LIST" && n >= 4 {
                let list = self.d.get(body..body + 4).and_then(|s| <[u8; 4]>::try_from(s).ok());
                out.push(Chunk { id, list, start: body + 4, end: stop });
            } else {
                out.push(Chunk { id, list: None, start: body, end: stop });
            }
            p = stop + (n & 1);
        }
        out
    }
    fn kids(&self, c: &Chunk) -> Vec<Chunk> {
        if c.list.is_none() || c.list == Some(*b"btdk") {
            return Vec::new();
        }
        self.chunks(c.start, c.end)
    }
    fn body(&self, c: &Chunk) -> &'a [u8] {
        self.d.get(c.start..c.end).unwrap_or(&[])
    }
    fn u32(&self, at: usize) -> Option<u32> {
        be32(self.d, at)
    }
}

fn be16(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(b.get(at..at + 2)?.try_into().ok()?))
}
fn be32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}
fn bei32(b: &[u8], at: usize) -> Option<i32> {
    Some(i32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}
fn bef64(b: &[u8], at: usize) -> Option<f64> {
    Some(f64::from_be_bytes(b.get(at..at + 8)?.try_into().ok()?)).filter(|v| v.is_finite())
}
fn bef32(b: &[u8], at: usize) -> Option<f32> {
    Some(f32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?)).filter(|v| v.is_finite())
}
/// Seconds as dividend / divisor (0 for a zero divisor).
fn ratio(num: i64, den: u32) -> f64 {
    if den == 0 {
        0.0
    } else {
        num as f64 / den as f64
    }
}

fn find<'c>(cs: &'c [Chunk], id: &[u8; 4]) -> Option<&'c Chunk> {
    cs.iter().find(|c| &c.id == id)
}
fn find_list<'c>(cs: &'c [Chunk], list: &[u8; 4]) -> Option<&'c Chunk> {
    cs.iter().find(|c| c.list.as_ref() == Some(list))
}

/// A NUL-ended (or whole) string.
fn cstr(b: &[u8]) -> String {
    let b = b.split(|c| *c == 0).next().unwrap_or(b);
    String::from_utf8_lossy(b).trim().to_string()
}

/// An item of the project.
#[derive(Clone, Debug)]
enum Item {
    Comp(CompRead),
    Footage(Footage),
}

#[derive(Clone, Debug)]
struct Footage {
    name: String,
    size: [u32; 2],
    duration: f64,
    fps: f64,
    audio: bool,
    kind: FootageKind,
}

#[derive(Clone, Debug)]
enum FootageKind {
    File(PathBuf),
    Solid([u8; 4]),
    /// Missing or a placeholder, with nothing to point at.
    Nothing,
}

#[derive(Clone, Debug)]
struct CompRead {
    name: String,
    size: [u32; 2],
    fps: f64,
    duration: f64,
    /// The item's `LIST Item` chunk (its layers are read later).
    chunk: Chunk,
}

/// A property as read: its value, keys (layer seconds, value, out-going
/// interpolation), and whether its keys were for a split part.
#[derive(Clone, Debug, Default)]
struct Prop {
    value: Vec<f64>,
    keys: Vec<(f64, Vec<f64>, Interp)>,
    /// `tdsb` flags (bit 1: hidden, as Position's split parts are while unsplit).
    flags: u32,
    expression: bool,
}

pub fn read(bytes: &[u8], dir: Option<&Path>) -> Result<Imported, String> {
    if !bytes.starts_with(b"RIFX") || bytes.get(8..12) != Some(b"Egg!") {
        return Err("That isn't an After Effects project".into());
    }
    let r = Riff { d: bytes };
    let top = r.chunks(12, bytes.len());
    let fold = find_list(&top, b"Fold").ok_or("The After Effects project has no items")?;
    let mut items: Vec<(u32, Item)> = Vec::new();
    collect(&r, fold, dir, &mut items, 0);
    let mut reader = Reader { r: &r, items: items.iter().cloned().collect(), project: Project::new(), notes: Notes::default(), comp_index: HashMap::new(), built: HashMap::new(), media: HashMap::new(), layers: 0 };
    reader.project.sequences.clear();
    let comps: Vec<(u32, CompRead)> = items.iter().filter_map(|(id, i)| if let Item::Comp(c) = i { Some((*id, c.clone())) } else { None }).collect();
    let files: Vec<(u32, Footage, PathBuf)> = items
        .iter()
        .filter_map(|(id, i)| match i {
            Item::Footage(f) => match &f.kind {
                FootageKind::File(p) => Some((*id, f.clone(), p.clone())),
                _ => None,
            },
            Item::Comp(_) => None,
        })
        .collect();
    if comps.is_empty() && files.is_empty() {
        return Err("The After Effects project has no compositions or footage to bring across".into());
    }
    // Compositions used inside others go after the ones that aren't.
    let used: std::collections::HashSet<u32> = comps.iter().flat_map(|(_, c)| layer_sources(&r, &c.chunk)).collect();
    let mut order: Vec<&(u32, CompRead)> = comps.iter().filter(|(id, _)| !used.contains(id)).collect();
    order.sort_by(|a, b| b.1.duration.partial_cmp(&a.1.duration).unwrap_or(std::cmp::Ordering::Equal));
    order.extend(comps.iter().filter(|(id, _)| used.contains(id)));
    for (i, (id, c)) in order.iter().enumerate() {
        reader.comp_index.insert(*id, i);
        reader.project.sequences.push(Sequence::new(c.name.clone(), c.size[0], c.size[1], rate_of(c.fps)));
    }
    let mut clips = 0;
    for (i, (_, c)) in order.iter().enumerate() {
        let seq = reader.comp(c);
        clips += seq.tracks.iter().map(|t| t.clips.len()).sum::<usize>();
        if let Some(slot) = reader.project.sequences.get_mut(i) {
            *slot = seq;
        }
    }
    // Footage in the project panel that no composition uses comes across too.
    for (id, f, path) in &files {
        reader.media(*id, f, path);
    }
    if reader.project.sequences.is_empty() {
        reader.project.sequences.push(Sequence::new("Sequence 1", 1920, 1080, FrameRate::new(30, 1)));
    }
    let summary = format!("{}, {}, {}", count(reader.project.sequences.len(), "composition", "compositions"), count(clips, "layer", "layers"), count(reader.project.media.len(), "media file", "media files"));
    Ok(Imported { project: reader.project, notes: reader.notes.lines(), summary })
}

/// Every item in a folder, and in the folders in it.
fn collect(r: &Riff, folder: &Chunk, dir: Option<&Path>, out: &mut Vec<(u32, Item)>, depth: usize) {
    if depth > MAX_DEPTH {
        return;
    }
    for c in r.kids(folder) {
        match c.list.as_ref() {
            Some(b"Item") => {
                let k = r.kids(&c);
                let Some(idta) = find(&k, b"idta").map(|c| r.body(c)) else { continue };
                let (Some(kind), Some(id)) = (be16(idta, 0), be32(idta, 16)) else { continue };
                let name = find(&k, b"Utf8").map(|c| cstr(r.body(c))).unwrap_or_default();
                match kind {
                    1 => {
                        if let Some(sub) = find_list(&k, b"Sfdr") {
                            collect(r, sub, dir, out, depth + 1);
                        }
                    }
                    4 => {
                        if let Some(cd) = find(&k, b"cdta").map(|c| r.body(c)) {
                            let size = [be16(cd, 140).unwrap_or(1920) as u32, be16(cd, 142).unwrap_or(1080) as u32];
                            let fps = be16(cd, 156).unwrap_or(30) as f64 + be16(cd, 158).unwrap_or(0) as f64 / 65536.0;
                            let duration = ratio(be32(cd, 44).unwrap_or(0) as i64, be32(cd, 48).unwrap_or(1));
                            out.push((id, Item::Comp(CompRead { name: if name.is_empty() { "Composition".into() } else { name }, size: [size[0].clamp(2, 16384), size[1].clamp(2, 16384)], fps, duration, chunk: c })));
                        }
                    }
                    7 => {
                        if let Some(f) = footage(r, &k, name, dir) {
                            out.push((id, Item::Footage(f)));
                        }
                    }
                    _ => {}
                }
            }
            Some(b"Sfdr") | Some(b"Fold") => collect(r, &c, dir, out, depth + 1),
            _ => {}
        }
    }
}

fn footage(r: &Riff, k: &[Chunk], name: String, dir: Option<&Path>) -> Option<Footage> {
    let pin = find_list(k, b"Pin ")?;
    let pk = r.kids(pin);
    let ss = find(&pk, b"sspc").map(|c| r.body(c)).unwrap_or(&[]);
    let size = [be16(ss, 32).unwrap_or(0) as u32, be16(ss, 36).unwrap_or(0) as u32];
    let duration = ratio(be32(ss, 38).unwrap_or(0) as i64, be32(ss, 42).unwrap_or(1));
    let fps = be32(ss, 56).unwrap_or(0) as f64 + be16(ss, 60).unwrap_or(0) as f64 / 65536.0;
    let audio = be16(ss, 100).unwrap_or(0) > 0 || bef64(ss, 160).is_some_and(|r| r > 0.0);
    let opti = find(&pk, b"opti").map(|c| r.body(c)).unwrap_or(&[]);
    let kind = if opti.starts_with(b"Soli") {
        let ch = |at| (bef32(opti, at).unwrap_or(0.0).clamp(0.0, 1.0) * 255.0).round() as u8;
        FootageKind::Solid([ch(14), ch(18), ch(22), 255])
    } else {
        let path = find_list(&pk, b"Als2").and_then(|a| find(&r.kids(a), b"alas").map(|c| r.body(c))).and_then(|b| serde_json::from_str::<serde_json::Value>(&cstr(b)).ok()).and_then(|v| v["fullpath"].as_str().map(str::to_string));
        match path.and_then(|p| locate(&[p.as_str()], dir)) {
            Some(p) => FootageKind::File(p),
            None => FootageKind::Nothing,
        }
    };
    let name = if !name.is_empty() {
        name
    } else {
        match &kind {
            FootageKind::File(p) => file_name(&p.to_string_lossy()),
            FootageKind::Solid(_) => cstr(opti.get(26..).unwrap_or(&[])),
            FootageKind::Nothing => String::new(),
        }
    };
    Some(Footage { name, size, duration: duration.max(0.0), fps, audio, kind })
}

/// The items a composition's layers show.
fn layer_sources(r: &Riff, comp: &Chunk) -> Vec<u32> {
    r.kids(comp).iter().filter(|c| c.list.as_ref() == Some(b"Layr")).filter_map(|l| find(&r.kids(l), b"ldta").and_then(|c| be32(r.body(c), 40))).filter(|id| *id != 0).collect()
}

/// A property group's (match name, LIST) pairs.
fn group(r: &Riff, g: &Chunk) -> Vec<(String, Chunk)> {
    let cs = r.kids(g);
    let mut out = Vec::new();
    let mut i = 0;
    while i < cs.len() {
        if &cs[i].id == b"tdmn" {
            let name = cstr(r.body(&cs[i]));
            if let Some(next) = cs.get(i + 1).filter(|c| c.list.is_some()) {
                out.push((name, *next));
                i += 2;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// A property's value and keys.
fn prop(r: &Riff, c: &Chunk) -> Option<Prop> {
    let c = if c.list.as_ref() == Some(b"otst") { *find_list(&r.kids(c), b"tdbs")? } else { *c };
    if c.list.as_ref() != Some(b"tdbs") {
        return None;
    }
    let cs = r.kids(&c);
    let t4 = find(&cs, b"tdb4").map(|c| r.body(c)).unwrap_or(&[]);
    let dims = (be16(t4, 2).unwrap_or(1) as usize).clamp(1, 4);
    let spatial = t4.get(5).is_some_and(|f| f & 8 != 0);
    let per_sec = be32(t4, 12).unwrap_or(0) as f64;
    let flags = find(&cs, b"tdsb").and_then(|c| be32(r.body(c), 0)).unwrap_or(0);
    let mut p = Prop { flags, ..Default::default() };
    if let Some(cd) = find(&cs, b"cdat").map(|c| r.body(c)) {
        p.value = (0..dims).filter_map(|i| bef64(cd, i * 8)).collect();
    }
    p.expression = cs.iter().filter(|c| &c.id == b"Utf8").any(|c| !cstr(r.body(c)).is_empty());
    if let Some(list) = find_list(&cs, b"list") {
        let lk = r.kids(list);
        let h = find(&lk, b"lhd3").map(|c| r.body(c)).unwrap_or(&[]);
        let n = be32(h, 8).unwrap_or(0) as usize;
        let size = be32(h, 16).unwrap_or(0) as usize;
        let data = find(&lk, b"ldat").map(|c| r.body(c)).unwrap_or(&[]);
        if n > 0 && size >= 8 && per_sec > 0.0 && n.saturating_mul(size) <= data.len() {
            for i in 0..n.min(100_000) {
                let Some(k) = data.get(i * size..(i + 1) * size) else { break };
                let t = bei32(k, 0).unwrap_or(0) as f64 / per_sec;
                let interp = match k.get(5) {
                    Some(3) => Interp::Hold,
                    Some(2) => Interp::Ease,
                    _ => Interp::Linear,
                };
                // Spatial: 8 bytes, five doubles, then the value; else the value first.
                let at = if spatial { 8 + 8 + 40 } else { 8 };
                let v: Vec<f64> = (0..dims).filter_map(|j| bef64(k, at + j * 8)).collect();
                if v.len() == dims {
                    p.keys.push((t, v, interp));
                }
            }
            p.keys.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        }
    }
    Some(p)
}

struct Reader<'r, 'a> {
    r: &'r Riff<'a>,
    items: HashMap<u32, Item>,
    project: Project,
    notes: Notes,
    comp_index: HashMap<u32, usize>,
    /// Layer id → the clips made for it, in the composition being read.
    built: HashMap<u32, Vec<ClipId>>,
    /// Footage item → our media.
    media: HashMap<u32, MediaId>,
    layers: usize,
}

/// A layer as its children see it.
struct LayerInfo {
    name: String,
    parent: u32,
    start: f64,
    stretch: f64,
    /// Its own picture's size (its anchor point is in these pixels).
    own: [f32; 2],
    props: HashMap<String, Prop>,
}

/// A property's value at layer time `t`: its keys, straight between them
/// (held on a hold key), or its one value.
fn value_at(p: &Prop, t: f64) -> Vec<f64> {
    if p.keys.is_empty() {
        return p.value.clone();
    }
    let i = p.keys.partition_point(|k| k.0 <= t + 1e-9);
    match (i.checked_sub(1).and_then(|j| p.keys.get(j)), p.keys.get(i)) {
        (Some(k), Some(n)) if k.2 != Interp::Hold && n.0 > k.0 => {
            let u = ((t - k.0) / (n.0 - k.0)).clamp(0.0, 1.0);
            k.1.iter().zip(&n.1).map(|(a, b)| a + (b - a) * u).collect()
        }
        (Some(k), _) => k.1.clone(),
        (None, Some(n)) => n.1.clone(),
        (None, None) => p.value.clone(),
    }
}

/// How a layer sits: timing in the composition, as one of our clips.
struct Timing {
    start: Time,
    duration: Time,
    /// Layer time at the clip's start (seconds).
    layer_in: f64,
    /// Layer seconds a composition second (the stretch's inverse).
    speed: f64,
    reverse: bool,
}

impl<'r, 'a> Reader<'r, 'a> {
    fn media(&mut self, id: u32, f: &Footage, path: &Path) -> MediaId {
        if let Some(m) = self.media.get(&id) {
            return *m;
        }
        let rate = rate_of(if f.fps > 0.0 { f.fps } else { 30.0 });
        let still = f.duration <= 0.0;
        let info = MediaInfo {
            duration: if still { Time::ZERO } else { secs(f.duration) },
            video: (f.size[0] > 0 && f.size[1] > 0).then(|| VideoStreamInfo { width: f.size[0], height: f.size[1], frame_rate: rate, codec: String::new(), time_base: Rational::new(rate.0.den, rate.0.num), hdr: None, pixel_aspect: (1, 1) }),
            audio: f.audio.then(|| AudioStreamInfo { sample_rate: 48_000, channels: 2, codec: String::new() }),
            container: String::new(),
        };
        let mid = match self.project.media.iter().find(|m| m.path == path) {
            Some(m) => m.id,
            None => {
                let mid = MediaId(self.project.alloc_id());
                let name = if f.name.is_empty() { file_name(&path.to_string_lossy()) } else { f.name.clone() };
                self.project.media.push(MediaAsset { id: mid, path: path.to_path_buf(), name, info, conform: None });
                mid
            }
        };
        self.media.insert(id, mid);
        mid
    }

    /// Does a composition have any sound in it (its footage's, or its compositions')?
    /// (Each composition is looked in once: one used many times, or itself,
    /// would otherwise be searched again and again.)
    fn comp_has_sound(&self, comp: &Chunk) -> bool {
        let mut seen = std::collections::HashSet::new();
        let mut todo = vec![*comp];
        while let Some(c) = todo.pop() {
            for id in layer_sources(self.r, &c) {
                match self.items.get(&id) {
                    Some(Item::Footage(f)) if f.audio => return true,
                    Some(Item::Comp(inner)) if seen.insert(id) && seen.len() <= MAX_LAYERS => todo.push(inner.chunk),
                    _ => {}
                }
            }
        }
        false
    }

    fn comp(&mut self, c: &CompRead) -> Sequence {
        self.built.clear();
        let mut seq = Sequence::new(c.name.clone(), c.size[0], c.size[1], rate_of(c.fps));
        seq.tracks.clear();
        let frame = [c.size[0] as f32, c.size[1] as f32];
        let comp_len = c.duration.max(0.0);
        let mut video: Vec<Track> = Vec::new();
        let mut audio: Vec<Track> = Vec::new();
        // Layers top first; tracks bottom first.
        let layers: Vec<Chunk> = self.r.kids(&c.chunk).into_iter().filter(|l| l.list.as_ref() == Some(b"Layr")).collect();
        for l in layers.iter().rev() {
            if self.layers >= MAX_LAYERS {
                self.notes.add("Very many layers: the rest were left out");
                break;
            }
            self.layers += 1;
            self.layer(l, frame, comp_len, &mut video, &mut audio);
        }
        let info: HashMap<u32, LayerInfo> = layers.iter().filter_map(|l| self.layer_info(l, frame)).collect();
        self.parents(&info, frame, comp_len, c.fps, &mut video);
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

    fn layer(&mut self, l: &Chunk, frame: [f32; 2], comp_len: f64, video: &mut Vec<Track>, audio: &mut Vec<Track>) {
        let r = self.r;
        let k = r.kids(l);
        let ld = find(&k, b"ldta").map(|c| r.body(c)).unwrap_or(&[]);
        if ld.len() < 112 {
            return;
        }
        let name = find(&k, b"Utf8").map(|c| cstr(r.body(c))).filter(|n| !n.is_empty()).unwrap_or_else(|| cstr(ld.get(64..96).unwrap_or(&[])));
        let (f0, f1, f2) = (ld[37], ld[38], ld[39]);
        let enabled = f2 & 1 != 0;
        let audio_on = f2 & 2 != 0;
        let guide = f0 & 2 != 0;
        let null = f1 & 0x80 != 0;
        let adjustment = f1 & 2 != 0;
        let three_d = f1 & 4 != 0;
        let source = be32(ld, 40).unwrap_or(0);
        let blend_code = ld[99];
        let matte = ld[107];
        let kind = ld.get(131).copied().unwrap_or(0);
        let parent = be32(ld, 132).unwrap_or(0);
        let stretch = {
            let (n, d) = (bei32(ld, 8).unwrap_or(1), be32(ld, 108).unwrap_or(1));
            let s = ratio(n as i64, d);
            if s == 0.0 || !s.is_finite() || s.abs() > 1e4 {
                1.0
            } else {
                s
            }
        };
        let start = ratio(bei32(ld, 12).unwrap_or(0) as i64, be32(ld, 16).unwrap_or(1));
        let lin = ratio(bei32(ld, 20).unwrap_or(0) as i64, be32(ld, 24).unwrap_or(1));
        let lout = ratio(bei32(ld, 28).unwrap_or(0) as i64, be32(ld, 32).unwrap_or(1));
        let item = self.items.get(&source).cloned();
        // An unnamed layer shows its source's name.
        let name = if name.is_empty() {
            match &item {
                Some(Item::Footage(f)) => f.name.clone(),
                Some(Item::Comp(c)) => c.name.clone(),
                None => name,
            }
        } else {
            name
        };
        if guide {
            return;
        }
        if !enabled && !matches!(kind, 1 | 2) {
            self.notes.add("Hidden layers (eye off) were left out");
            return;
        }
        match kind {
            1 => return self.notes.add("Lights were left out"),
            2 => return self.notes.add("Cameras were left out"),
            4 => return self.notes.add("Shape layers were left out"),
            _ => {}
        }
        // A null draws nothing: it only moves its children (see `parents`).
        if null && kind == 0 {
            return;
        }
        let _ = parent;
        // Composition times of the layer's ends.
        let (mut a, mut b) = (start + lin * stretch, start + lout * stretch);
        if a > b {
            std::mem::swap(&mut a, &mut b);
        }
        let footage = match &item {
            Some(Item::Footage(f)) => Some(f.clone()),
            _ => None,
        };
        // Footage plays only as long as it lasts.
        if let Some(f) = footage.as_ref().filter(|f| f.duration > 0.0 && matches!(f.kind, FootageKind::File(_))) {
            let (s, e) = if stretch >= 0.0 { (start, start + f.duration * stretch) } else { (start + f.duration * stretch, start) };
            a = a.max(s);
            b = b.min(e);
        }
        let (a, b) = (a.max(0.0), if comp_len > 0.0 { b.min(comp_len) } else { b });
        if b - a < 1e-6 {
            return;
        }
        let speed = 1.0 / stretch.abs();
        let layer_time = |t: f64| (t - start) / stretch;
        let timing = Timing { start: secs(a), duration: secs(b - a), layer_in: layer_time(a), speed, reverse: stretch < 0.0 };
        let id = ClipId(self.project.alloc_id());
        // The layer's own picture size (for its anchor point).
        let (mut clip, layer_px, own) = match (kind, &item) {
            (3, _) => {
                self.notes.add("Text layers came across as titles of the layer's name: their font, size and animators are Reelforge's defaults");
                let spec = crate::title::TitleSpec { text: if name.is_empty() { "Text".into() } else { name.clone() }, ..Default::default() };
                (Clip::new_title(id, timing.start, timing.duration, spec), frame, [0.0, 0.0])
            }
            (_, Some(Item::Comp(inner))) => {
                let Some(i) = self.comp_index.get(&source).copied() else { return };
                let s = [inner.size[0] as f32, inner.size[1] as f32];
                let mut c = Clip::new_nested(id, i, timing.start, timing.duration, None);
                c.source_in = secs(timing.layer_in);
                (c, s, s)
            }
            (_, Some(Item::Footage(f))) => match &f.kind {
                _ if adjustment => (Clip::new_adjustment(id, timing.start, timing.duration), frame, frame),
                FootageKind::Solid(rgba) => {
                    let s = [f.size[0].max(1) as f32, f.size[1].max(1) as f32];
                    (Clip::new_plane(id, timing.start, timing.duration, *rgba), frame, s)
                }
                FootageKind::File(path) => {
                    let m = self.media(source, f, path);
                    let s = [f.size[0].max(1) as f32, f.size[1].max(1) as f32];
                    (Clip::new(id, m, timing.start, secs(timing.layer_in), timing.duration, None), s, s)
                }
                FootageKind::Nothing => {
                    self.notes.add("Layers whose footage isn't named in the project were left out");
                    return;
                }
            },
            _ => {
                self.notes.add("Layers with no source were left out");
                return;
            }
        };
        if (timing.speed - 1.0).abs() > 1e-6 || timing.reverse {
            clip.speed = timing.speed as f32;
            clip.reverse = timing.reverse;
            let m = clip.source_in;
            // Media time = anchor + rate × clip time, the layer time at the start.
            clip.anchor = if timing.reverse { m + secs(m.as_secs_f64() * timing.speed) } else { m - secs(m.as_secs_f64() * timing.speed) };
        }
        clip.blend = blend_of(blend_code).unwrap_or_else(|| {
            self.notes.add("A blend mode Reelforge doesn't have came across as Normal");
            BlendMode::Normal
        });
        clip.transform.three_d = three_d;
        // Layer seconds → our clip time.
        let layer_in = timing.layer_in;
        let clip_in = clip.source_in;
        let to_ct = move |lt: f64| clip_in + secs((lt - layer_in) / speed.max(1e-9) * if timing.reverse { -1.0 } else { 1.0 });
        // The layer's own pixels → ours (a solid is drawn frame-sized and scaled).
        let k = if own[0] > 0.0 && layer_px != own { [layer_px[0] / own[0], layer_px[1] / own[1]] } else { [1.0, 1.0] };
        let own_size = if own[0] > 0.0 { own } else { [0.0, 0.0] };
        clip.transform.scale = [1.0 / k[0], 1.0 / k[1]];
        if let Some(root) = find_list(&r.kids(l), b"tdgp") {
            self.properties(root, &mut clip, frame, own_size, k, &to_ct);
        }
        // Text titles are frame-sized: their own size is the frame's.
        if kind == 3 {
            clip.transform.anchor = [0.0, 0.0];
            clip.anim.anchor = Curve(Vec::new());
        }
        let label = if name.is_empty() { format!("Layer {}", video.len() + 1) } else { name.clone() };
        let matte_of = |m: u8| match m {
            2 => Some(TrackMatte::Alpha),
            3 => Some(TrackMatte::AlphaInverted),
            4 => Some(TrackMatte::Luma),
            5 => Some(TrackMatte::LumaInverted),
            _ => None,
        };
        clip.track_matte = matte_of(matte);
        // Its sound, linked, on an audio track of its own.
        let has_sound = match &item {
            Some(Item::Footage(f)) => f.audio && matches!(f.kind, FootageKind::File(_)),
            Some(Item::Comp(inner)) => self.comp_has_sound(&inner.chunk),
            _ => false,
        } && audio_on;
        let picture = match &item {
            Some(Item::Footage(f)) => f.size[0] > 0,
            _ => true,
        };
        if has_sound {
            let link = self.project.alloc_id();
            let mut s = clip.clone();
            s.id = ClipId(self.project.alloc_id());
            s.link = Some(link);
            s.effects.clear();
            s.anim = Default::default();
            s.transform = Default::default();
            if picture {
                clip.link = Some(link);
            }
            let mut t = Track::new(format!("{label} (sound)"), TrackKind::Audio);
            t.clips.push(s);
            audio.push(t);
        }
        if picture {
            let layer_id = be32(ld, 0).unwrap_or(0);
            self.built.entry(layer_id).or_default().push(clip.id);
            let mut t = Track::new(label, TrackKind::Video);
            t.clips.push(clip);
            video.push(t);
        }
    }

    /// A layer's id, parent, timing, own size and transform, for parenting.
    fn layer_info(&self, l: &Chunk, frame: [f32; 2]) -> Option<(u32, LayerInfo)> {
        let r = self.r;
        let k = r.kids(l);
        let ld = find(&k, b"ldta").map(|c| r.body(c))?;
        if ld.len() < 136 {
            return None;
        }
        let id = be32(ld, 0)?;
        let stretch = {
            let s = ratio(bei32(ld, 8).unwrap_or(1) as i64, be32(ld, 108).unwrap_or(1));
            if s == 0.0 || !s.is_finite() || s.abs() > 1e4 {
                1.0
            } else {
                s
            }
        };
        let start = ratio(bei32(ld, 12).unwrap_or(0) as i64, be32(ld, 16).unwrap_or(1));
        let source = be32(ld, 40).unwrap_or(0);
        let own = match (ld.get(131), self.items.get(&source)) {
            (Some(3), _) => [0.0, 0.0],
            (_, Some(Item::Footage(f))) => [f.size[0] as f32, f.size[1] as f32],
            (_, Some(Item::Comp(c))) => [c.size[0] as f32, c.size[1] as f32],
            _ => frame,
        };
        let name = find(&k, b"Utf8").map(|c| cstr(r.body(c))).filter(|n| !n.is_empty()).unwrap_or_else(|| cstr(ld.get(64..96).unwrap_or(&[])));
        let props = find_list(&k, b"tdgp").and_then(|root| group(r, root).into_iter().find(|(n, _)| n == "ADBE Transform Group")).map(|(_, g)| group(r, &g).into_iter().filter_map(|(n, c)| Some((n, prop(r, &c)?))).collect()).unwrap_or_default();
        Some((id, LayerInfo { name, parent: be32(ld, 132).unwrap_or(0), start, stretch, own, props }))
    }

    /// Parented layers follow a point (a null) of ours made for each parent
    /// layer: where its anchor point is, turned and scaled as it is, so
    /// a child's position (in its parent's pixels) lands where After
    /// Effects puts it. Parents of parents nest the same way.
    fn parents(&mut self, info: &HashMap<u32, LayerInfo>, frame: [f32; 2], comp_len: f64, fps: f64, video: &mut Vec<Track>) {
        let mut wanted: Vec<u32> = Vec::new();
        for (id, l) in info {
            if l.parent != 0 && self.built.contains_key(id) {
                let mut p = l.parent;
                let mut depth = 0;
                while p != 0 && depth < MAX_DEPTH && info.contains_key(&p) {
                    if !wanted.contains(&p) {
                        wanted.push(p);
                    }
                    p = info.get(&p).map_or(0, |l| l.parent);
                    depth += 1;
                }
            }
        }
        if wanted.is_empty() {
            return;
        }
        let len = secs(comp_len.max(1.0 / fps.max(1.0)));
        let mut points: HashMap<u32, ClipId> = HashMap::new();
        let mut tracks = Vec::new();
        for p in &wanted {
            let Some(l) = info.get(p) else { continue };
            let id = ClipId(self.project.alloc_id());
            let mut c = Clip::new_scene(id, Time::ZERO, len, crate::space3d::SceneObject::Point, frame);
            self.fill_point(&mut c, l, frame, comp_len, fps);
            points.insert(*p, id);
            let mut t = Track::new(format!("Parent: {}", if l.name.is_empty() { "layer" } else { &l.name }), TrackKind::Video);
            t.clips.push(c);
            tracks.push((*p, t));
        }
        // Points inside their own parents' points.
        for (p, t) in &mut tracks {
            let pp = info.get(p).map_or(0, |l| l.parent);
            if let (Some(c), Some(outer)) = (t.clips.first_mut(), points.get(&pp)) {
                c.parent = Some(*outer);
            }
        }
        for (id, l) in info {
            let (Some(point), Some(clips)) = (points.get(&l.parent), self.built.get(id)) else { continue };
            for t in video.iter_mut() {
                for c in t.clips.iter_mut().filter(|c| clips.contains(&c.id)) {
                    c.parent = Some(*point);
                }
            }
        }
        // Under everything (they draw nothing).
        for (_, t) in tracks.into_iter().rev() {
            video.insert(0, t);
        }
    }

    /// A parent's motion as a point's settings: position, rotation, scale.
    fn fill_point(&mut self, c: &mut Clip, l: &LayerInfo, frame: [f32; 2], comp_len: f64, fps: f64) {
        use crate::effects::point as pt;
        let get = |n: &str| l.props.get(n);
        let split = get("ADBE Position_0").filter(|p| p.flags & 2 == 0).zip(get("ADBE Position_1").filter(|p| p.flags & 2 == 0));
        let turns = ["ADBE Rotate Z", "ADBE Scale"].iter().any(|n| get(n).is_some_and(|p| !p.keys.is_empty()));
        // Key times (layer seconds) of everything that moves it.
        let mut times: Vec<f64> = ["ADBE Anchor Point", "ADBE Position", "ADBE Position_0", "ADBE Position_1", "ADBE Scale", "ADBE Rotate Z"].iter().filter_map(|n| get(n)).flat_map(|p| p.keys.iter().map(|k| k.0)).collect();
        times.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        times.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
        // Turning or scaling between keys moves the point on a curve: every frame then.
        if turns && times.len() >= 2 {
            let (a, b) = (times[0], times[times.len() - 1]);
            let step = l.stretch.abs() / fps.max(1.0);
            let n = (((b - a) / step).ceil() as usize).min(20_000);
            times = (0..=n).map(|i| a + (b - a) * i as f64 / n.max(1) as f64).collect();
        }
        let cx = [frame[0] as f64 / 2.0, frame[1] as f64 / 2.0];
        let at = |lt: f64| -> [f64; 4] {
            let pos = match split {
                Some((x, y)) => [value_at(x, lt).first().copied().unwrap_or(cx[0]), value_at(y, lt).first().copied().unwrap_or(cx[1])],
                None => get("ADBE Position").map(|p| value_at(p, lt)).map_or(cx, |v| [v.first().copied().unwrap_or(cx[0]), v.get(1).copied().unwrap_or(cx[1])]),
            };
            let anchor = get("ADBE Anchor Point").map(|p| value_at(p, lt)).map_or([l.own[0] as f64 / 2.0, l.own[1] as f64 / 2.0], |v| [v.first().copied().unwrap_or(0.0), v.get(1).copied().unwrap_or(0.0)]);
            let scale = get("ADBE Scale").map(|p| value_at(p, lt)).and_then(|v| v.first().copied()).unwrap_or(1.0);
            let rot = get("ADBE Rotate Z").map(|p| value_at(p, lt)).and_then(|v| v.first().copied()).unwrap_or(0.0);
            // Where the child's frame-centre point goes: P − c + R·S·(c − A).
            let (s, co) = rot.to_radians().sin_cos();
            let d = [(cx[0] - anchor[0]) * scale, (cx[1] - anchor[1]) * scale];
            [pos[0] - cx[0] + co * d[0] - s * d[1], pos[1] - cx[1] + s * d[0] + co * d[1], rot, scale * 100.0]
        };
        if get("ADBE Scale").is_some_and(|p| p.value.len() >= 2 && (p.value[0] - p.value[1]).abs() > 1e-6) {
            self.notes.add("A parent scaled differently across and down moves its children with its width's scale");
        }
        let still = at(times.first().copied().unwrap_or(0.0));
        let Some(fx) = c.effects.get_mut(0) else { return };
        fx.normalize();
        for (i, p) in [pt::X, pt::Y, pt::ROTATION, pt::SCALE].into_iter().enumerate() {
            if let Some(param) = fx.params.get_mut(p) {
                param.value = still[i] as f32;
            }
        }
        if times.len() >= 2 {
            // (Up to a key a frame: gathered, then put in order once, rather than
            // each one slotted in among the rest.)
            let mut keys: [Vec<(Time, f32)>; 4] = Default::default();
            for lt in times {
                let t = l.start + lt * l.stretch;
                if !(0.0..=comp_len.max(0.0) + 1.0).contains(&t) {
                    continue;
                }
                let v = at(lt);
                for (k, v) in keys.iter_mut().zip(v) {
                    k.push((secs(t), v as f32));
                }
            }
            for (k, p) in keys.into_iter().zip([pt::X, pt::Y, pt::ROTATION, pt::SCALE]) {
                if let Some(param) = fx.params.get_mut(p) {
                    let mut k = k;
                    // (a stretch below zero runs them backwards; the last of a time wins)
                    k.sort_by_key(|(t, _)| *t);
                    k.reverse();
                    k.dedup_by_key(|(t, _)| *t);
                    k.reverse();
                    param.keys = crate::anim::Curve(k.into_iter().map(|(t, v)| crate::anim::Key::new(t, v, crate::anim::Interp::Linear)).collect());
                }
            }
        }
    }

    fn properties(&mut self, root: &Chunk, c: &mut Clip, frame: [f32; 2], own: [f32; 2], k: [f32; 2], to_ct: &dyn Fn(f64) -> Time) {
        let r = self.r;
        for (name, g) in group(r, root) {
            match name.as_str() {
                "ADBE Transform Group" => self.transform(&g, c, frame, own, k, to_ct),
                "ADBE Effect Parade" => {
                    for (fx, _) in group(r, &g) {
                        self.notes.add(format!("Effect left out: {}", effect_name(&fx)));
                    }
                }
                "ADBE Mask Parade" => {
                    if !group(r, &g).is_empty() {
                        self.notes.add("Masks were left out");
                    }
                }
                "ADBE Time Remapping" => {
                    if prop(r, &g).is_some_and(|p| !p.keys.is_empty()) {
                        self.notes.add("Time remapping came across as the layer's plain timing");
                    }
                }
                _ => {}
            }
        }
    }

    fn transform(&mut self, g: &Chunk, c: &mut Clip, frame: [f32; 2], own: [f32; 2], k: [f32; 2], to_ct: &dyn Fn(f64) -> Time) {
        let r = self.r;
        let props: HashMap<String, Prop> = group(r, g).into_iter().filter_map(|(n, c)| Some((n, prop(r, &c)?))).collect();
        if props.values().any(|p| p.expression) {
            self.notes.add("Expressions were left out (their properties keep their values)");
        }
        let v = |p: &[f64], i: usize| p.get(i).copied().unwrap_or(0.0) as f32;
        // Anchor: layer pixels from the top left; ours, from the centre (in our layer's pixels).
        if let Some(p) = props.get("ADBE Anchor Point") {
            let f = |a: &[f64]| [(v(a, 0) - own[0] / 2.0) * k[0], (v(a, 1) - own[1] / 2.0) * k[1]];
            c.transform.anchor = f(&p.value);
            c.anim.anchor = curve(&p.keys, f, to_ct);
        }
        // Position: composition pixels from the top left; ours, from the centre.
        let pos = |a: &[f64]| [v(a, 0) - frame[0] / 2.0, v(a, 1) - frame[1] / 2.0];
        let split: Vec<Option<&Prop>> = ["ADBE Position_0", "ADBE Position_1", "ADBE Position_2"].iter().map(|n| props.get(*n).filter(|p| p.flags & 2 == 0)).collect();
        if let (Some(x), Some(y)) = (split[0], split[1]) {
            c.transform.position = pos(&[x.value.first().copied().unwrap_or(0.0), y.value.first().copied().unwrap_or(0.0)]);
            c.anim.position = merge(x, y, |a, b| pos(&[a, b]), to_ct);
            if let Some(z) = split[2] {
                c.transform.depth = v(&z.value, 0);
                c.anim.depth = curve(&z.keys, |a| v(a, 0), to_ct);
            }
        } else if let Some(p) = props.get("ADBE Position") {
            c.transform.position = pos(&p.value);
            c.anim.position = curve(&p.keys, pos, to_ct);
            if c.transform.three_d {
                c.transform.depth = v(&p.value, 2);
                c.anim.depth = curve(&p.keys, |a| v(a, 2), to_ct);
            }
        }
        // Scale: factors (1 = 100 %); a solid drawn frame-sized is scaled down to its own size.
        if let Some(p) = props.get("ADBE Scale") {
            let f = |a: &[f64]| [a.first().copied().unwrap_or(1.0) as f32 / k[0], a.get(1).copied().unwrap_or(1.0) as f32 / k[1]];
            c.transform.scale = f(&p.value);
            c.anim.scale = curve(&p.keys, f, to_ct);
        }
        if let Some(p) = props.get("ADBE Rotate Z") {
            c.transform.rotation = v(&p.value, 0);
            c.anim.rotation = curve(&p.keys, |a| v(a, 0), to_ct);
        }
        if let Some(p) = props.get("ADBE Opacity") {
            let f = |a: &[f64]| v(a, 0).clamp(0.0, 1.0);
            c.transform.opacity = f(&p.value);
            c.anim.opacity = curve(&p.keys, f, to_ct);
        }
        if c.transform.three_d {
            let (x, y) = (props.get("ADBE Rotate X"), props.get("ADBE Rotate Y"));
            let o = props.get("ADBE Orientation");
            let base = |p: Option<&Prop>, i: usize| p.map_or(0.0, |p| v(&p.value, i));
            c.transform.orientation = [base(x, 0) + base(o, 0), base(y, 0) + base(o, 1)];
            if let (Some(x), Some(y)) = (x, y) {
                c.anim.orientation = merge(x, y, |a, b| [a as f32, b as f32], to_ct);
            }
        }
    }
}

/// An effect's match name as people know it ("ADBE Glo2" → "Glow").
fn effect_name(m: &str) -> String {
    let known = [("ADBE Glo2", "Glow"), ("ADBE Geometry2", "Transform"), ("ADBE Easy Levels2", "Levels"), ("ADBE Pro Levels2", "Levels (Individual Controls)"), ("ADBE HUE SATURATION", "Hue/Saturation"), ("ADBE Ramp", "Gradient Ramp"), ("ADBE Fill", "Fill"), ("ADBE Drop Shadow", "Drop Shadow"), ("ADBE Lumetri", "Lumetri Color")];
    if let Some((_, n)) = known.iter().find(|(k, _)| *k == m) {
        return n.to_string();
    }
    let s = m.trim_start_matches("ADBE ");
    // "Gaussian Blur 2": the version number off.
    match s.rsplit_once(' ') {
        Some((a, b)) if !a.is_empty() && b.chars().all(|c| c.is_ascii_digit()) => a.to_string(),
        _ => s.to_string(),
    }
}

/// After Effects' blend mode codes (the layer's `ldta`).
fn blend_of(code: u8) -> Option<BlendMode> {
    Some(match code {
        0 | 2 => BlendMode::Normal,
        4 | 29 => BlendMode::Add,
        5 => BlendMode::Multiply,
        6 => BlendMode::Screen,
        7 => BlendMode::Overlay,
        8 => BlendMode::SoftLight,
        9 => BlendMode::HardLight,
        10 => BlendMode::Darken,
        11 => BlendMode::Lighten,
        12 | 26 => BlendMode::Difference,
        13 => BlendMode::Hue,
        14 => BlendMode::Saturation,
        15 => BlendMode::Color,
        16 => BlendMode::Luminosity,
        23 | 27 => BlendMode::ColorDodge,
        24 | 28 => BlendMode::ColorBurn,
        25 => BlendMode::Exclusion,
        _ => return None,
    })
}

fn curve<T, F: Fn(&[f64]) -> T>(keys: &[(f64, Vec<f64>, Interp)], f: F, to_ct: &dyn Fn(f64) -> Time) -> Curve<T> {
    let mut out: Vec<Key<T>> = keys.iter().map(|(t, v, i)| Key::new(to_ct(*t), f(v), *i)).collect();
    out.sort_by_key(|k| k.at);
    out.dedup_by_key(|k| k.at);
    Curve(out)
}

/// Two one-value properties (X and Y) as one curve, keyed wherever either is.
fn merge<T, F: Fn(f64, f64) -> T>(a: &Prop, b: &Prop, f: F, to_ct: &dyn Fn(f64) -> Time) -> Curve<T> {
    if a.keys.is_empty() && b.keys.is_empty() {
        return Curve(Vec::new());
    }
    let mut times: Vec<f64> = a.keys.iter().chain(&b.keys).map(|k| k.0).collect();
    times.sort_by(|x, y| x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal));
    times.dedup_by(|x, y| (*x - *y).abs() < 1e-9);
    let value_at = |p: &Prop, t: f64| -> (f64, Interp) {
        let first = |v: &Vec<f64>| v.first().copied().unwrap_or(0.0);
        if p.keys.is_empty() {
            return (first(&p.value), Interp::Linear);
        }
        let i = p.keys.partition_point(|k| k.0 <= t + 1e-9);
        match (i.checked_sub(1).and_then(|j| p.keys.get(j)), p.keys.get(i)) {
            (Some(k), Some(n)) if k.2 != Interp::Hold && n.0 > k.0 && (t - k.0).abs() > 1e-9 => {
                let u = (t - k.0) / (n.0 - k.0);
                (first(&k.1) + (first(&n.1) - first(&k.1)) * u, k.2)
            }
            (Some(k), _) => (first(&k.1), k.2),
            (None, Some(n)) => (first(&n.1), n.2),
            (None, None) => (first(&p.value), Interp::Linear),
        }
    };
    Curve(
        times
            .into_iter()
            .map(|t| {
                let ((x, i), (y, _)) = (value_at(a, t), value_at(b, t));
                Key::new(to_ct(t), f(x, y), i)
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build chunks by hand.
    fn chunk(id: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut v = id.to_vec();
        v.extend((body.len() as u32).to_be_bytes());
        v.extend(body);
        if body.len() % 2 == 1 {
            v.push(0);
        }
        v
    }
    fn list(kind: &[u8; 4], kids: &[Vec<u8>]) -> Vec<u8> {
        let mut b = kind.to_vec();
        for k in kids {
            b.extend(k);
        }
        chunk(b"LIST", &b)
    }
    fn idta(kind: u16, id: u32) -> Vec<u8> {
        let mut b = vec![0u8; 84];
        b[0..2].copy_from_slice(&kind.to_be_bytes());
        b[16..20].copy_from_slice(&id.to_be_bytes());
        chunk(b"idta", &b)
    }
    fn tdbs(values: &[f64], keys: &[(i32, &[f64])], spatial: bool) -> Vec<u8> {
        let mut t4 = vec![0u8; 124];
        t4[2..4].copy_from_slice(&(values.len() as u16).to_be_bytes());
        t4[5] = if spatial { 8 } else { 1 };
        t4[12..16].copy_from_slice(&24576u32.to_be_bytes());
        let cdat: Vec<u8> = values.iter().flat_map(|v| v.to_be_bytes()).collect();
        let mut kids = vec![chunk(b"tdsb", &1u32.to_be_bytes()), chunk(b"tdb4", &t4), chunk(b"cdat", &cdat)];
        if !keys.is_empty() {
            let n = values.len();
            let size = if spatial { 8 + 8 + 40 + 24 * n } else { 8 + 40 * n };
            let mut h = vec![0u8; 52];
            h[8..12].copy_from_slice(&(keys.len() as u32).to_be_bytes());
            h[16..20].copy_from_slice(&(size as u32).to_be_bytes());
            let mut data = Vec::new();
            for (t, v) in keys {
                let mut k = vec![0u8; size];
                k[0..4].copy_from_slice(&(t * 24576).to_be_bytes());
                k[4] = 1;
                k[5] = 1;
                let at = if spatial { 56 } else { 8 };
                for (j, x) in v.iter().enumerate() {
                    k[at + j * 8..at + j * 8 + 8].copy_from_slice(&x.to_be_bytes());
                }
                data.extend(k);
            }
            kids.push(list(b"list", &[chunk(b"lhd3", &h), chunk(b"ldat", &data)]));
        }
        list(b"tdbs", &kids)
    }
    fn tdmn(name: &str) -> Vec<u8> {
        let mut b = name.as_bytes().to_vec();
        b.resize(40, 0);
        chunk(b"tdmn", &b)
    }
    fn ldta(id: u32, source: u32, start: i32, inp: i32, out: i32, flags: [u8; 3], blend: u8, kind: u8) -> Vec<u8> {
        let mut b = vec![0u8; 160];
        b[0..4].copy_from_slice(&id.to_be_bytes());
        b[8..12].copy_from_slice(&1i32.to_be_bytes());
        for (at, v) in [(12, start), (20, inp), (28, out)] {
            b[at..at + 4].copy_from_slice(&v.to_be_bytes());
            b[at + 4..at + 8].copy_from_slice(&1u32.to_be_bytes());
        }
        b[37..40].copy_from_slice(&flags);
        b[40..44].copy_from_slice(&source.to_be_bytes());
        b[99] = blend;
        b[108..112].copy_from_slice(&1u32.to_be_bytes());
        b[131] = kind;
        chunk(b"ldta", &b)
    }

    /// A 1280×720 25 fps project: a comp with a moving, fading footage
    /// layer (Multiply), a red solid, a text layer and a precomp of a
    /// second comp; and the footage, solid and second comp items.
    fn sample() -> Vec<u8> {
        let mut cd = vec![0u8; 204];
        cd[8..12].copy_from_slice(&25600u32.to_be_bytes());
        cd[44..48].copy_from_slice(&10u32.to_be_bytes());
        cd[48..52].copy_from_slice(&1u32.to_be_bytes());
        cd[140..142].copy_from_slice(&1280u16.to_be_bytes());
        cd[142..144].copy_from_slice(&720u16.to_be_bytes());
        cd[156..158].copy_from_slice(&25u16.to_be_bytes());
        let mut inner_cd = cd.clone();
        inner_cd[140..142].copy_from_slice(&640u16.to_be_bytes());
        inner_cd[142..144].copy_from_slice(&360u16.to_be_bytes());
        let transform = list(
            b"tdgp",
            &[
                tdmn("ADBE Transform Group"),
                list(b"tdgp", &[tdmn("ADBE Position"), tdbs(&[640.0, 360.0, 0.0], &[(0, &[0.0, 360.0, 0.0]), (2, &[1280.0, 360.0, 0.0])], true), tdmn("ADBE Scale"), tdbs(&[0.5, 0.5, 1.0], &[], false), tdmn("ADBE Opacity"), tdbs(&[1.0], &[(1, &[1.0]), (3, &[0.0])], false), tdmn("ADBE Group End")]),
                tdmn("ADBE Effect Parade"),
                list(b"tdgp", &[tdmn("ADBE Gaussian Blur 2"), list(b"tdgp", &[])]),
            ],
        );
        let footage_layer = list(b"Layr", &[ldta(1, 10, 1, 0, 4, [0, 0, 7], 5, 0), chunk(b"Utf8", b"Shot"), transform]);
        let solid_layer = list(b"Layr", &[ldta(2, 11, 0, 0, 10, [0, 0, 7], 2, 0), chunk(b"Utf8", b"Red")]);
        let text_layer = list(b"Layr", &[ldta(3, 0, 0, 2, 5, [0, 0, 7], 2, 3), chunk(b"Utf8", b"Hello")]);
        let pre_layer = list(b"Layr", &[ldta(4, 12, 6, 0, 3, [0, 0, 7], 2, 0), chunk(b"Utf8", b"Inner")]);
        let comp = list(b"Item", &[idta(4, 1), chunk(b"Utf8", b"Main"), chunk(b"cdta", &cd), text_layer, footage_layer, solid_layer, pre_layer]);
        let inner = list(b"Item", &[idta(4, 12), chunk(b"Utf8", b"Inner"), chunk(b"cdta", &inner_cd)]);
        let mut ss = vec![0u8; 222];
        ss[32..34].copy_from_slice(&1920u16.to_be_bytes());
        ss[36..38].copy_from_slice(&1080u16.to_be_bytes());
        ss[38..42].copy_from_slice(&20u32.to_be_bytes());
        ss[42..46].copy_from_slice(&1u32.to_be_bytes());
        ss[56..60].copy_from_slice(&25u32.to_be_bytes());
        ss[100..102].copy_from_slice(&2u16.to_be_bytes());
        let alas = br#"{"fullpath":"/footage/shot.mov","platform":2}"#;
        let file = list(b"Item", &[idta(7, 10), chunk(b"Utf8", b""), list(b"Pin ", &[chunk(b"sspc", &ss), chunk(b"opti", b"\0\0\0\0\0\0"), list(b"Als2", &[chunk(b"alas", alas)])])]);
        let mut solid_ss = vec![0u8; 222];
        solid_ss[32..34].copy_from_slice(&640u16.to_be_bytes());
        solid_ss[36..38].copy_from_slice(&360u16.to_be_bytes());
        let mut opti = b"Soli".to_vec();
        opti.extend([0, 9, 0, 0, 1, 0x1a, 0x3f, 0x80, 0, 0]);
        for c in [1.0f32, 0.0, 0.0] {
            opti.extend(c.to_be_bytes());
        }
        opti.extend(b"Red Solid\0");
        let solid = list(b"Item", &[idta(7, 11), chunk(b"Utf8", b""), list(b"Pin ", &[chunk(b"sspc", &solid_ss), chunk(b"opti", &opti)])]);
        let folder = list(b"Item", &[idta(1, 20), chunk(b"Utf8", b"Solids"), list(b"Sfdr", &[solid])]);
        let fold = list(b"Fold", &[comp, inner, file, folder]);
        let mut body = b"Egg!".to_vec();
        body.extend(fold);
        let mut out = b"RIFX".to_vec();
        out.extend((body.len() as u32).to_be_bytes());
        out.extend(body);
        out
    }

    fn sec(t: Time) -> f64 {
        (t.as_secs_f64() * 1000.0).round() / 1000.0
    }

    #[test]
    fn an_after_effects_comp_comes_across() {
        let im = read(&sample(), None).unwrap();
        // (`REELFORGE_FUZZ_SEEDS=dir`: fuzzing seeds, see fuzz/seeds.py)
        if let Ok(dir) = std::env::var("REELFORGE_FUZZ_SEEDS") {
            let dir = std::path::Path::new(&dir);
            let _ = std::fs::write(dir.join("sample.aep"), sample());
            let _ = std::fs::write(dir.join("from-after-effects.rfproj"), serde_json::to_vec_pretty(&im.project).unwrap_or_default());
        }
        let p = &im.project;
        assert_eq!(p.sequences.len(), 2, "{:?}", im.notes);
        let s = &p.sequences[0];
        assert_eq!((s.name.as_str(), s.width, s.height, s.frame_rate), ("Main", 1280, 720, FrameRate::new(25, 1)));
        assert_eq!(p.sequences[1].name, "Inner");
        let video: Vec<&Track> = s.tracks.iter().filter(|t| t.kind == TrackKind::Video).collect();
        // Bottom first: the precomp, the solid, the footage, the text.
        let names: Vec<&str> = video.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["Inner", "Red", "Shot", "Hello"]);
        let pre = &video[0].clips[0];
        assert_eq!((pre.nested, sec(pre.timeline_start), sec(pre.duration)), (Some(1), 6.0, 3.0));
        let solid = &video[1].clips[0];
        assert_eq!(solid.plane, Some([255, 0, 0, 255]));
        assert_eq!(solid.transform.scale, [0.5, 0.5], "a 640×360 solid in a 1280×720 comp");
        let shot = &video[2].clips[0];
        assert_eq!((sec(shot.timeline_start), sec(shot.duration), sec(shot.source_in)), (1.0, 4.0, 0.0));
        assert_eq!((shot.blend, shot.transform.scale), (BlendMode::Multiply, [0.5, 0.5]));
        let k = &shot.anim.position.0;
        assert_eq!(k.len(), 2);
        assert_eq!((sec(k[0].at), k[0].value, sec(k[1].at), k[1].value), (0.0, [-640.0, 0.0], 2.0, [640.0, 0.0]));
        assert_eq!(shot.anim.opacity.0.iter().map(|k| (sec(k.at), k.value)).collect::<Vec<_>>(), [(1.0, 1.0), (3.0, 0.0)]);
        let text = &video[3].clips[0];
        assert_eq!(text.title.as_ref().map(|t| t.text.as_str()), Some("Hello"));
        assert_eq!((sec(text.timeline_start), sec(text.duration)), (2.0, 3.0));
        // The footage's sound, linked; the media by path.
        let sound = s.tracks.iter().filter(|t| t.kind == TrackKind::Audio).flat_map(|t| &t.clips).find(|c| c.media == shot.media).unwrap();
        assert!(sound.link.is_some() && sound.link == shot.link);
        assert_eq!(p.media.len(), 1);
        assert_eq!(p.media[0].path, PathBuf::from("/footage/shot.mov"));
        assert!(im.notes.iter().any(|n| n.contains("Gaussian Blur")), "{:?}", im.notes);
        assert!(im.summary.starts_with("2 compositions"), "{}", im.summary);
    }

    /// A layer parented to a null turned 90°: it lands where After Effects
    /// puts it, by way of a point of ours that the child follows.
    #[test]
    fn parented_layers_follow_their_parent() {
        let mut cd = vec![0u8; 204];
        cd[44..48].copy_from_slice(&10u32.to_be_bytes());
        cd[48..52].copy_from_slice(&1u32.to_be_bytes());
        cd[140..142].copy_from_slice(&1280u16.to_be_bytes());
        cd[142..144].copy_from_slice(&720u16.to_be_bytes());
        cd[156..158].copy_from_slice(&25u16.to_be_bytes());
        let with_parent = |mut c: Vec<u8>, parent: u32| {
            // ldta's body starts after the 8-byte chunk header.
            c[8 + 132..8 + 136].copy_from_slice(&parent.to_be_bytes());
            c
        };
        let tg = |pos: [f64; 3], rot: Option<f64>| {
            let mut kids = vec![tdmn("ADBE Position"), tdbs(&pos, &[], true)];
            if let Some(r) = rot {
                kids.extend([tdmn("ADBE Rotate Z"), tdbs(&[r], &[], false)]);
            }
            list(b"tdgp", &[tdmn("ADBE Transform Group"), list(b"tdgp", &kids)])
        };
        // The null: a 100×100 solid's layer, null switch on, at (840, 360), turned 90°.
        let null = list(b"Layr", &[ldta(5, 11, 0, 0, 10, [0, 0x80, 7], 2, 0), chunk(b"Utf8", b"Ctrl"), tg([840.0, 360.0, 0.0], Some(90.0))]);
        // The child: 100 px right of the null's anchor, in the null's pixels.
        let child = list(b"Layr", &[with_parent(ldta(6, 11, 0, 0, 10, [0, 0, 7], 2, 0), 5), chunk(b"Utf8", b"Kid"), tg([150.0, 50.0, 0.0], None)]);
        let comp = list(b"Item", &[idta(4, 1), chunk(b"Utf8", b"Main"), chunk(b"cdta", &cd), child, null]);
        let mut ss = vec![0u8; 222];
        ss[32..34].copy_from_slice(&100u16.to_be_bytes());
        ss[36..38].copy_from_slice(&100u16.to_be_bytes());
        let mut opti = b"Soli".to_vec();
        opti.extend([0u8; 8]);
        opti.extend([0u8; 12]);
        let solid = list(b"Item", &[idta(7, 11), chunk(b"Utf8", b"Null 1"), list(b"Pin ", &[chunk(b"sspc", &ss), chunk(b"opti", &opti)])]);
        let mut body = b"Egg!".to_vec();
        body.extend(list(b"Fold", &[comp, solid]));
        let mut file = b"RIFX".to_vec();
        file.extend((body.len() as u32).to_be_bytes());
        file.extend(body);

        let im = read(&file, None).unwrap();
        let s = &im.project.sequences[0];
        let clips: Vec<&Clip> = s.tracks.iter().flat_map(|t| &t.clips).collect();
        let point = clips.iter().find(|c| c.scene.is_some()).expect("a point for the parent");
        let kid = clips.iter().find(|c| c.scene.is_none()).expect("the child");
        assert_eq!(kid.parent, Some(point.id));
        assert!(clips.len() == 2, "the null itself draws nothing");
        // Where the child's anchor (its centre) ends up, from the frame's centre.
        let n = crate::space3d::Null::from_effect(&point.effects[0], Time::ZERO);
        let w = n.apply([kid.transform.position[0], kid.transform.position[1], 0.0]);
        // After Effects: (840, 360) + 90° of (100, 0) = (840, 460); from the centre (200, 100).
        assert!((w[0] - 200.0).abs() < 0.01 && (w[1] - 100.0).abs() < 0.01, "{w:?}");
        assert!(!im.notes.iter().any(|n| n.contains("arent")), "{:?}", im.notes);
    }

    #[test]
    fn damaged_projects_are_errors_not_crashes() {
        assert!(read(b"", None).is_err());
        assert!(read(b"RIFX\0\0\0\x04Egg!", None).is_err());
        let good = sample();
        // Cut short anywhere, or with sizes scrambled: an error or less, never a panic.
        for n in (12..good.len()).step_by(37) {
            let _ = read(&good[..n], None);
        }
        let mut bad = good.clone();
        for i in (20..bad.len()).step_by(53) {
            bad[i] = 0xff;
        }
        let _ = read(&bad, None);
    }
}
