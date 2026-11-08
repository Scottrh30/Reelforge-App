//! Handing an edit to another editor: the cuts of a sequence written as a
//! CMX 3600 **EDL** (Avid, Premiere, Resolve and most others), **FCPXML**
//! 1.10 (Final Cut Pro, Resolve), **Final Cut Pro 7 XML** (`xmeml`: Premiere,
//! Resolve, Avid via its importer) and **OpenTimelineIO** JSON (Resolve,
//! Nuke Studio, Hiero, RV and pipeline tools).
//!
//! What goes across is the edit itself: which part of which file plays
//! where, on which track, at what speed, with dissolves between clips and
//! the markers. Effects, titles, masks, colour and keyframes don't (each
//! program has its own); titles, solids and compositions go as named
//! placeholders (gaps in an EDL, with a comment).

use crate::project::{Clip, Project, TrackKind};
use crate::time::{FrameRate, Time};
use crate::MediaId;
use std::fmt::Write as _;
use std::path::PathBuf;

/// What a clip on the timeline plays.
#[derive(Clone, Debug, PartialEq)]
pub enum Source {
    /// Part of a media file.
    File { media: MediaId, path: PathBuf, video: bool, audio: bool, duration: i64 },
    /// A title, solid, adjustment layer or other generated layer (named).
    Generated,
    /// A composition (by name).
    Composition,
}

/// One clip, in whole frames of the sequence's rate.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub name: String,
    pub source: Source,
    /// Where it plays on the timeline: first frame and one past the last.
    pub rec: (i64, i64),
    /// The media frame shown at `rec.0` (at the sequence's rate).
    pub src_in: i64,
    /// Media frames per timeline frame (negative: backwards, 0: a freeze).
    pub speed: f64,
    /// A dissolve into it from the clip before: how far it starts before
    /// `rec.0` and how far that clip runs on past it.
    pub dissolve: Option<(i64, i64)>,
    /// Up to 8 characters naming the source, for the EDL's reel column.
    pub reel: String,
}

impl Item {
    /// The media frame one past the last shown (the far end for backwards clips).
    pub fn src_out(&self) -> i64 {
        self.src_in + ((self.rec.1 - self.rec.0) as f64 * self.speed).round() as i64
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Lane {
    pub kind: TrackKind,
    pub name: String,
    pub items: Vec<Item>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Marker {
    pub at: i64,
    pub name: String,
}

/// A sequence as other editors see it.
#[derive(Clone, Debug, PartialEq)]
pub struct Edit {
    pub name: String,
    pub rate: FrameRate,
    pub width: u32,
    pub height: u32,
    pub duration: i64,
    /// Video tracks bottom first, then audio tracks.
    pub lanes: Vec<Lane>,
    pub markers: Vec<Marker>,
}

/// Up to 8 capitals and digits from a file name (an EDL reel name).
fn reel_of(name: &str) -> String {
    let stem = std::path::Path::new(name).file_stem().map_or(name.to_owned(), |s| s.to_string_lossy().into_owned());
    let r: String = stem.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_').map(|c| c.to_ascii_uppercase()).take(8).collect();
    if r.is_empty() { "AX".into() } else { r }
}

/// Sequence `seq` of `p` as an [`Edit`].
pub fn edit_of(p: &Project, seq: usize) -> Edit {
    let s = &p.sequences[seq];
    let rate = s.frame_rate;
    let f = |t: Time| t.to_frame(rate);
    let dissolves = p.transitions(seq);
    let mut lanes = Vec::new();
    let mut order: Vec<usize> = (0..s.tracks.len()).filter(|&i| s.tracks[i].kind == TrackKind::Video).collect();
    order.extend((0..s.tracks.len()).filter(|&i| s.tracks[i].kind == TrackKind::Audio));
    for ti in order {
        let tr = &s.tracks[ti];
        let mut items = Vec::new();
        for c in &tr.clips {
            if c.scene.is_some() {
                continue; // cameras and lights
            }
            let source = match (c.nested, p.media(c.media)) {
                (Some(_), _) => Source::Composition,
                (None, Some(m)) if c.title.is_none() && c.plane.is_none() && !c.adjustment => Source::File {
                    media: m.id,
                    path: m.path.clone(),
                    video: m.info.video.is_some(),
                    audio: m.info.audio.is_some(),
                    duration: f(m.info.duration).max(1),
                },
                _ => Source::Generated,
            };
            let name = clip_name(p, c);
            let reel = match &source {
                Source::File { .. } => reel_of(&p.media(c.media).map_or(name.clone(), |m| m.name.clone())),
                Source::Composition => "AX".into(),
                Source::Generated => "BL".into(),
            };
            let speed = if c.remap.is_some() { c.rate_at(Time::ZERO) } else { c.rate() };
            let dissolve = dissolves.iter().find(|x| x.right == c.id && !x.is_empty()).map(|x| (f(x.cut) - f(x.start()), f(x.end()) - f(x.cut)));
            items.push(Item { name, source, rec: (f(c.timeline_start), f(c.timeline_end())), src_in: f(c.media_at(c.timeline_start)), speed, dissolve, reel });
        }
        lanes.push(Lane { kind: tr.kind, name: tr.name.clone(), items });
    }
    Edit {
        name: s.name.clone(),
        rate,
        width: s.width,
        height: s.height,
        duration: f(s.duration()),
        lanes,
        markers: s.markers.iter().map(|m| Marker { at: f(m.at), name: m.name.clone() }).collect(),
    }
}

fn clip_name(p: &Project, c: &Clip) -> String {
    if let Some(t) = &c.title {
        let first = t.text.lines().next().unwrap_or("").trim();
        return if first.is_empty() { "Title".into() } else { first.to_owned() };
    }
    c.display_name(p).into_owned()
}

// ---- Shared helpers -------------------------------------------------------------------

/// Whole frames per second for timecode (30 for 29.97, 24 for 23.976).
fn tc_fps(rate: FrameRate) -> i64 {
    (rate.0.num as f64 / rate.0.den as f64).round().max(1.0) as i64
}

/// Non-drop-frame timecode of frame `n`.
pub fn timecode(n: i64, rate: FrameRate) -> String {
    let fps = tc_fps(rate);
    let n = n.max(0);
    let (fr, s) = (n % fps, n / fps);
    format!("{:02}:{:02}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60, fr)
}

fn ntsc(rate: FrameRate) -> bool {
    rate.0.den == 1001
}

/// `<`, `>`, `&` and quotes escaped for XML.
fn xml(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&apos;"),
            c if (c as u32) < 0x20 && c != '\n' && c != '\t' => {}
            c => o.push(c),
        }
    }
    o
}

/// A `file://` URL for a path (Windows paths too), with unsafe characters
/// percent-encoded.
pub fn file_url(path: &std::path::Path) -> String {
    let s = path.to_string_lossy().replace('\\', "/");
    let s = if s.starts_with('/') { s } else { format!("/{s}") };
    let mut o = String::from("file://");
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' | b':' => o.push(b as char),
            _ => {
                let _ = write!(o, "%{b:02X}");
            }
        }
    }
    o
}

// ---- CMX 3600 EDL -----------------------------------------------------------------

/// A CMX 3600 EDL of video lane `video` (index among the video tracks) and
/// up to four audio tracks (A, A2, A3, A4), record timecode starting at
/// `start` (frames; 01:00:00:00 is usual).
pub fn edl(e: &Edit, video: usize, start: i64) -> String {
    let rate = e.rate;
    let tc = |n: i64| timecode(n, rate);
    let mut events: Vec<(i64, u8, String)> = Vec::new(); // (record start, order, lines)
    let videos: Vec<&Lane> = e.lanes.iter().filter(|l| l.kind == TrackKind::Video).collect();
    let audios: Vec<&Lane> = e.lanes.iter().filter(|l| l.kind == TrackKind::Audio).take(4).collect();
    let mut chans: Vec<(&Lane, &str)> = Vec::new();
    if let Some(v) = videos.get(video) {
        chans.push((v, "V"));
    }
    for (i, a) in audios.iter().enumerate() {
        chans.push((a, ["A", "A2", "A3", "A4"][i]));
    }
    for (ci, (lane, ch)) in chans.iter().enumerate() {
        for (k, it) in lane.items.iter().enumerate() {
            let mut lines = String::new();
            let (reel, src_in, src_out) = match it.source {
                Source::File { .. } => (it.reel.clone(), it.src_in, it.src_out()),
                // A black/placeholder event for titles, solids and compositions.
                _ => ("BL".to_owned(), 0, it.rec.1 - it.rec.0),
            };
            let (src_in, src_out) = if it.speed < 0.0 { (src_out, src_in) } else { (src_in, src_out) };
            // The outgoing clip stops where a dissolve into the next starts.
            let next_dissolve = lane.items.get(k + 1).filter(|n| n.rec.0 == it.rec.1).and_then(|n| n.dissolve);
            let rec_out = it.rec.1 - next_dissolve.map_or(0, |d| d.0);
            match it.dissolve {
                Some((before, after)) if k > 0 => {
                    let prev = &lane.items[k - 1];
                    let at = it.rec.0 - before;
                    let prev_src = match prev.source {
                        Source::File { .. } => prev.src_in + ((at - prev.rec.0) as f64 * prev.speed).round() as i64,
                        _ => at - prev.rec.0,
                    };
                    let prev_reel = if matches!(prev.source, Source::File { .. }) { prev.reel.as_str() } else { "BL" };
                    let s_in = src_in - (before as f64 * it.speed).round() as i64;
                    let _ = writeln!(lines, "{{N}}  {prev_reel:<8} {ch:<5} C        {} {} {} {}", tc(prev_src), tc(prev_src), tc(start + at), tc(start + at));
                    let _ = writeln!(lines, "{{N}}  {reel:<8} {ch:<5} D    {:03} {} {} {} {}", (before + after).min(999), tc(s_in), tc(src_out), tc(start + at), tc(start + rec_out));
                    let _ = after;
                }
                _ => {
                    let out = src_in + ((rec_out - it.rec.0) as f64 * it.speed.abs()).round() as i64;
                    let out = if matches!(it.source, Source::File { .. }) { out.max(src_in) } else { src_in + rec_out - it.rec.0 };
                    let _ = writeln!(lines, "{{N}}  {reel:<8} {ch:<5} C        {} {} {} {}", tc(src_in), tc(out), tc(start + it.rec.0), tc(start + rec_out));
                }
            }
            if (it.speed - 1.0).abs() > 1e-6 && matches!(it.source, Source::File { .. }) {
                let fps = tc_fps(rate) as f64 * it.speed;
                let _ = writeln!(lines, "M2   {reel:<8}       {fps:05.1}                {}", tc(src_in));
            }
            let _ = writeln!(lines, "* FROM CLIP NAME: {}", it.name);
            if let Source::File { path, .. } = &it.source {
                let _ = writeln!(lines, "* SOURCE FILE: {}", path.display());
            } else {
                let _ = writeln!(lines, "* {} (made in Reelforge: not carried across)", if matches!(it.source, Source::Composition) { "COMPOSITION" } else { "GENERATED LAYER" });
            }
            events.push((it.rec.0 - it.dissolve.map_or(0, |d| d.0), ci as u8, lines));
        }
    }
    events.sort_by_key(|(at, ch, _)| (*at, *ch));
    let title: String = e.name.chars().filter(|c| !c.is_control()).collect();
    let mut out = format!("TITLE: {title}\nFCM: NON-DROP FRAME\n\n");
    let mut n = 0;
    for (_, _, lines) in events {
        for line in lines.lines() {
            if line.starts_with("{N}") {
                // Each pair of dissolve lines shares a number.
                if !line.contains(" D ") {
                    n += 1;
                }
                let _ = writeln!(out, "{n:03}{}", &line[3..]);
            } else {
                let _ = writeln!(out, "{line}");
            }
        }
        out.push('\n');
    }
    for m in &e.markers {
        let _ = writeln!(out, "* MARKER {} {}", tc(start + m.at), m.name);
    }
    out
}

// ---- FCPXML 1.10 -------------------------------------------------------------------

/// Rational seconds for `frames` at `rate`, as FCPXML writes them ("1001/30000s").
fn fcp_time(frames: i64, rate: FrameRate) -> String {
    if frames == 0 {
        return "0s".into();
    }
    let (num, den) = (frames * rate.0.den, rate.0.num);
    let g = gcd(num.abs(), den);
    let (num, den) = (num / g, den / g);
    if den == 1 { format!("{num}s") } else { format!("{num}/{den}s") }
}

fn gcd(a: i64, b: i64) -> i64 {
    if b == 0 { a.max(1) } else { gcd(b, a % b) }
}

/// FCPXML 1.10: a library with one event and one project. Video track 1
/// is the primary storyline (with gaps where it's empty, and named gaps for
/// titles and the like); the other tracks' footage are connected clips on
/// lanes above (video) and below (audio).
pub fn fcpxml(e: &Edit) -> String {
    let rate = e.rate;
    let t = |n: i64| fcp_time(n, rate);
    let mut o = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE fcpxml>\n<fcpxml version=\"1.10\">\n  <resources>\n");
    let _ = writeln!(o, "    <format id=\"r1\" frameDuration=\"{}\" width=\"{}\" height=\"{}\" name=\"FFVideoFormat{}p{}\"/>", t(1), e.width, e.height, e.height, tc_fps(rate));
    // One asset per file.
    let mut assets: Vec<(MediaId, String)> = Vec::new();
    for it in e.lanes.iter().flat_map(|l| &l.items) {
        if let Source::File { media, path, video, audio, duration } = &it.source {
            if assets.iter().any(|(m, _)| m == media) {
                continue;
            }
            let id = format!("r{}", assets.len() + 2);
            let name = path.file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned());
            let _ = writeln!(
                o,
                "    <asset id=\"{id}\" name=\"{}\" start=\"0s\" duration=\"{}\" hasVideo=\"{}\" hasAudio=\"{}\"{} audioSources=\"1\" audioChannels=\"2\">\n      <media-rep kind=\"original-media\" src=\"{}\"/>\n    </asset>",
                xml(&name),
                t(*duration),
                u8::from(*video),
                u8::from(*audio),
                if *video { " format=\"r1\"" } else { "" },
                xml(&file_url(path))
            );
            assets.push((*media, id));
        }
    }
    o.push_str("  </resources>\n  <library>\n    <event name=\"Reelforge\">\n");
    let _ = writeln!(o, "      <project name=\"{}\">", xml(&e.name));
    let _ = writeln!(o, "        <sequence format=\"r1\" duration=\"{}\" tcStart=\"0s\" tcFormat=\"NDF\" audioLayout=\"stereo\" audioRate=\"48k\">", t(e.duration.max(1)));
    o.push_str("          <spine>\n");
    let asset_of = |it: &Item| match &it.source {
        Source::File { media, .. } => assets.iter().find(|(m, _)| m == media).map(|(_, id)| id.clone()),
        _ => None,
    };
    // The element for one clip (connected ones carry a lane and an offset in their parent's time).
    let clip_xml = |it: &Item, lane: Option<i32>, offset: i64, audio_only: bool, video_only: bool, indent: &str, inner: &str| -> String {
        let dur = it.rec.1 - it.rec.0;
        let lane_attr = lane.map_or(String::new(), |l| format!(" lane=\"{l}\""));
        let mut s = String::new();
        match asset_of(it) {
            Some(id) => {
                let enable = if audio_only { " srcEnable=\"audio\"" } else if video_only { " srcEnable=\"video\"" } else { "" };
                let (start, retime) = if (it.speed - 1.0).abs() > 1e-6 { (it.src_in.min(it.src_out()), true) } else { (it.src_in, false) };
                let _ = write!(s, "{indent}<asset-clip ref=\"{id}\"{lane_attr} offset=\"{}\" name=\"{}\" start=\"{}\" duration=\"{}\"{enable}", t(offset), xml(&it.name), t(start), t(dur));
                if retime || !inner.is_empty() {
                    s.push_str(">\n");
                    if retime {
                        // Clip time → media time, relative to the clip's start.
                        let (a, b) = if it.speed >= 0.0 { (0, it.src_out() - it.src_in) } else { (it.src_in - it.src_out(), 0) };
                        let _ = writeln!(s, "{indent}  <timeMap>\n{indent}    <timept time=\"0s\" value=\"{}\" interp=\"linear\"/>\n{indent}    <timept time=\"{}\" value=\"{}\" interp=\"linear\"/>\n{indent}  </timeMap>", t(a), t(dur), t(b));
                    }
                    s.push_str(inner);
                    let _ = writeln!(s, "{indent}</asset-clip>");
                } else {
                    s.push_str("/>\n");
                }
            }
            None => {
                // Titles, solids and compositions: a named gap-like placeholder.
                let _ = write!(s, "{indent}<gap{lane_attr} name=\"{}\" offset=\"{}\" duration=\"{}\"", xml(&format!("{} (Reelforge)", it.name)), t(offset), t(dur));
                if inner.is_empty() {
                    s.push_str("/>\n");
                } else {
                    let _ = write!(s, ">\n{inner}{indent}</gap>\n");
                }
            }
        }
        s
    };
    let videos: Vec<&Lane> = e.lanes.iter().filter(|l| l.kind == TrackKind::Video).collect();
    let audios: Vec<&Lane> = e.lanes.iter().filter(|l| l.kind == TrackKind::Audio).collect();
    // The spine: track 1's clips, gaps between, out to the end.
    let mut spine: Vec<(i64, i64, Option<&Item>)> = Vec::new();
    let mut at = 0;
    if let Some(v1) = videos.first() {
        for it in &v1.items {
            if it.rec.0 > at {
                spine.push((at, it.rec.0, None));
            }
            if it.rec.0 >= at {
                spine.push((it.rec.0, it.rec.1, Some(it)));
                at = it.rec.1;
            }
        }
    }
    if at < e.duration {
        spine.push((at, e.duration, None));
    }
    // Everything else attaches to the spine element it starts in.
    let mut connected: Vec<Vec<String>> = vec![Vec::new(); spine.len()];
    let attach = |rec0: i64| spine.iter().position(|(a, b, _)| rec0 >= *a && rec0 < *b).unwrap_or(spine.len().saturating_sub(1));
    for (li, lane) in videos.iter().enumerate().skip(1) {
        // (Connected gaps aren't allowed: titles, solids and compositions above the storyline stay behind.)
        for it in lane.items.iter().filter(|it| matches!(it.source, Source::File { .. })) {
            let k = attach(it.rec.0);
            if let Some((a, _, parent)) = spine.get(k) {
                // An offset is in the parent's own time: its start plus how far in.
                let base = parent.map_or(*a, |p| p.src_in.min(p.src_out()));
                connected[k].push(clip_xml(it, Some(li as i32), base + it.rec.0 - a, false, true, "              ", ""));
            }
        }
    }
    for (li, lane) in audios.iter().enumerate() {
        for it in lane.items.iter().filter(|it| matches!(it.source, Source::File { .. })) {
            let k = attach(it.rec.0);
            if let Some((a, _, parent)) = spine.get(k) {
                let base = parent.map_or(*a, |p| p.src_in.min(p.src_out()));
                connected[k].push(clip_xml(it, Some(-(li as i32) - 1), base + it.rec.0 - a, true, false, "              ", ""));
            }
        }
    }
    for (k, (a, b, it)) in spine.iter().enumerate() {
        let mut inner: String = connected[k].concat();
        // Markers inside this element, in its own time.
        let base = it.map_or(*a, |p| p.src_in.min(p.src_out()));
        for m in e.markers.iter().filter(|m| m.at >= *a && m.at < *b) {
            let _ = writeln!(inner, "              <marker start=\"{}\" duration=\"{}\" value=\"{}\"/>", t(base + m.at - a), t(1), xml(&m.name));
        }
        match it {
            Some(it) => {
                // A dissolve into this clip: a transition element just before it.
                if let Some((before, after)) = it.dissolve.filter(|_| k > 0 && spine[k - 1].2.is_some()) {
                    let _ = writeln!(o, "            <transition name=\"Cross Dissolve\" offset=\"{}\" duration=\"{}\"/>", t(a - before), t(before + after));
                }
                o.push_str(&clip_xml(it, None, *a, false, true, "            ", &inner));
            }
            None => {
                let _ = write!(o, "            <gap name=\"Gap\" offset=\"{}\" start=\"{}\" duration=\"{}\"", t(*a), t(*a), t(b - a));
                if inner.is_empty() {
                    o.push_str("/>\n");
                } else {
                    let _ = write!(o, ">\n{inner}            </gap>\n");
                }
            }
        }
    }
    o.push_str("          </spine>\n        </sequence>\n      </project>\n    </event>\n  </library>\n</fcpxml>\n");
    o
}

// ---- Final Cut Pro 7 XML (xmeml) -----------------------------------------------------

fn rate_xml(rate: FrameRate, indent: &str) -> String {
    format!("{indent}<rate>\n{indent}  <timebase>{}</timebase>\n{indent}  <ntsc>{}</ntsc>\n{indent}</rate>\n", tc_fps(rate), if ntsc(rate) { "TRUE" } else { "FALSE" })
}

/// Final Cut Pro 7 XML (xmeml version 5), as Premiere Pro and Resolve read it.
pub fn xmeml(e: &Edit) -> String {
    let rate = e.rate;
    let mut o = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE xmeml>\n<xmeml version=\"5\">\n");
    let _ = writeln!(o, "  <sequence id=\"sequence-1\">\n    <name>{}</name>\n    <duration>{}</duration>", xml(&e.name), e.duration);
    o.push_str(&rate_xml(rate, "    "));
    o.push_str("    <timecode>\n");
    o.push_str(&rate_xml(rate, "      "));
    o.push_str("      <string>00:00:00:00</string>\n      <frame>0</frame>\n      <displayformat>NDF</displayformat>\n    </timecode>\n");
    o.push_str("    <media>\n");
    let mut files: Vec<MediaId> = Vec::new();
    let mut n = 0;
    for kind in [TrackKind::Video, TrackKind::Audio] {
        let tag = if kind == TrackKind::Video { "video" } else { "audio" };
        let _ = writeln!(o, "      <{tag}>");
        if kind == TrackKind::Video {
            let _ = writeln!(o, "        <format>\n          <samplecharacteristics>\n{}            <width>{}</width>\n            <height>{}</height>\n            <pixelaspectratio>square</pixelaspectratio>\n          </samplecharacteristics>\n        </format>", rate_xml(rate, "            "), e.width, e.height);
        }
        for lane in e.lanes.iter().filter(|l| l.kind == kind) {
            o.push_str("        <track>\n");
            for (k, it) in lane.items.iter().enumerate() {
                let Source::File { media, path, video, audio, duration } = &it.source else {
                    // Titles, solids and compositions aren't carried across: leave the gap.
                    continue;
                };
                if let Some((before, after)) = it.dissolve.filter(|_| k > 0 && kind == TrackKind::Video) {
                    let _ = writeln!(
                        o,
                        "          <transitionitem>\n{}            <start>{}</start>\n            <end>{}</end>\n            <alignment>center</alignment>\n            <effect>\n              <name>Cross Dissolve</name>\n              <effectid>Cross Dissolve</effectid>\n              <effectcategory>Dissolve</effectcategory>\n              <effecttype>transition</effecttype>\n              <mediatype>video</mediatype>\n            </effect>\n          </transitionitem>",
                        rate_xml(rate, "            "),
                        it.rec.0 - before,
                        it.rec.0 + after
                    );
                }
                n += 1;
                let (lo, hi) = (it.src_in.min(it.src_out()), it.src_in.max(it.src_out()));
                let _ = writeln!(o, "          <clipitem id=\"clipitem-{n}\">\n            <name>{}</name>\n            <enabled>TRUE</enabled>\n            <duration>{duration}</duration>", xml(&it.name));
                o.push_str(&rate_xml(rate, "            "));
                let _ = writeln!(o, "            <start>{}</start>\n            <end>{}</end>\n            <in>{lo}</in>\n            <out>{}</out>", it.rec.0, it.rec.1, hi.max(lo + 1));
                let fid = files.iter().position(|m| m == media);
                match fid {
                    Some(i) => {
                        let _ = writeln!(o, "            <file id=\"file-{}\"/>", i + 1);
                    }
                    None => {
                        files.push(*media);
                        let name = path.file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned());
                        let _ = writeln!(o, "            <file id=\"file-{}\">\n              <name>{}</name>\n              <pathurl>{}</pathurl>", files.len(), xml(&name), xml(&file_url(path)));
                        o.push_str(&rate_xml(rate, "              "));
                        let _ = writeln!(o, "              <duration>{duration}</duration>\n              <media>");
                        if *video {
                            let _ = writeln!(o, "                <video>\n                  <samplecharacteristics>\n                    <width>{}</width>\n                    <height>{}</height>\n                  </samplecharacteristics>\n                </video>", e.width, e.height);
                        }
                        if *audio {
                            o.push_str("                <audio>\n                  <channelcount>2</channelcount>\n                </audio>\n");
                        }
                        o.push_str("              </media>\n            </file>\n");
                    }
                }
                if (it.speed - 1.0).abs() > 1e-6 {
                    let _ = writeln!(
                        o,
                        "            <filter>\n              <effect>\n                <name>Time Remap</name>\n                <effectid>timeremap</effectid>\n                <effectcategory>motion</effectcategory>\n                <effecttype>motion</effecttype>\n                <mediatype>video</mediatype>\n                <parameter>\n                  <parameterid>speed</parameterid>\n                  <name>speed</name>\n                  <valuemin>-100000</valuemin>\n                  <valuemax>100000</valuemax>\n                  <value>{}</value>\n                </parameter>\n                <parameter>\n                  <parameterid>reverse</parameterid>\n                  <name>reverse</name>\n                  <value>{}</value>\n                </parameter>\n              </effect>\n            </filter>",
                        (it.speed.abs() * 100.0 * 1000.0).round() / 1000.0,
                        if it.speed < 0.0 { "TRUE" } else { "FALSE" }
                    );
                }
                if kind == TrackKind::Audio {
                    o.push_str("            <sourcetrack>\n              <mediatype>audio</mediatype>\n              <trackindex>1</trackindex>\n            </sourcetrack>\n");
                }
                o.push_str("          </clipitem>\n");
            }
            o.push_str("        </track>\n");
        }
        let _ = writeln!(o, "      </{tag}>");
    }
    o.push_str("    </media>\n");
    for m in &e.markers {
        let _ = writeln!(o, "    <marker>\n      <name>{}</name>\n      <in>{}</in>\n      <out>-1</out>\n    </marker>", xml(&m.name), m.at);
    }
    o.push_str("  </sequence>\n</xmeml>\n");
    o
}

// ---- OpenTimelineIO --------------------------------------------------------------------

fn json_str(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

/// OpenTimelineIO JSON (`.otio`).
pub fn otio(e: &Edit) -> String {
    use serde_json::{json, Value};
    let fps = e.rate.0.num as f64 / e.rate.0.den as f64;
    let rt = |v: i64| json!({ "OTIO_SCHEMA": "RationalTime.1", "rate": fps, "value": v as f64 });
    let range = |start: i64, dur: i64| json!({ "OTIO_SCHEMA": "TimeRange.1", "start_time": rt(start), "duration": rt(dur) });
    let mut tracks = Vec::new();
    for lane in &e.lanes {
        let mut children: Vec<Value> = Vec::new();
        let mut at = 0;
        for (k, it) in lane.items.iter().enumerate() {
            if it.rec.0 < at {
                continue;
            }
            if it.rec.0 > at {
                children.push(json!({ "OTIO_SCHEMA": "Gap.1", "name": "", "metadata": {}, "source_range": range(0, it.rec.0 - at), "effects": [], "markers": [], "enabled": true }));
            }
            if let Some((before, after)) = it.dissolve.filter(|_| k > 0 && lane.items[k - 1].rec.1 == it.rec.0) {
                children.push(json!({ "OTIO_SCHEMA": "Transition.1", "name": "", "metadata": {}, "transition_type": "SMPTE_Dissolve", "in_offset": rt(before), "out_offset": rt(after) }));
            }
            let dur = it.rec.1 - it.rec.0;
            let mut effects = Vec::new();
            if (it.speed - 1.0).abs() > 1e-6 {
                effects.push(if it.speed == 0.0 {
                    json!({ "OTIO_SCHEMA": "FreezeFrame.1", "name": "", "metadata": {}, "effect_name": "FreezeFrame", "time_scalar": 0.0 })
                } else {
                    json!({ "OTIO_SCHEMA": "LinearTimeWarp.1", "name": "", "metadata": {}, "effect_name": "LinearTimeWarp", "time_scalar": it.speed })
                });
            }
            let media_reference = match &it.source {
                Source::File { path, duration, .. } => json!({ "OTIO_SCHEMA": "ExternalReference.1", "name": path.file_name().map_or(String::new(), |n| n.to_string_lossy().into_owned()), "metadata": {}, "target_url": file_url(path), "available_range": range(0, *duration) }),
                _ => json!({ "OTIO_SCHEMA": "MissingReference.1", "name": "", "metadata": { "reelforge": if matches!(it.source, Source::Composition) { "composition" } else { "generated layer" } }, "available_range": null }),
            };
            // A clip's source range is in media frames before its time warp.
            let src = if it.speed < 0.0 { it.src_out() } else { it.src_in };
            children.push(json!({
                "OTIO_SCHEMA": "Clip.1",
                "name": it.name,
                "metadata": { "reelforge": { "reel": it.reel } },
                "source_range": range(src, dur),
                "media_reference": media_reference,
                "effects": effects,
                "markers": [],
                "enabled": true
            }));
            at = it.rec.1;
        }
        tracks.push(json!({ "OTIO_SCHEMA": "Track.1", "name": lane.name, "metadata": {}, "kind": if lane.kind == TrackKind::Video { "Video" } else { "Audio" }, "children": children, "source_range": null, "effects": [], "markers": [], "enabled": true }));
    }
    let markers: Vec<Value> = e.markers.iter().map(|m| json!({ "OTIO_SCHEMA": "Marker.2", "name": m.name, "metadata": {}, "color": "RED", "marked_range": range(m.at, 0), "comment": "" })).collect();
    let tl = json!({
        "OTIO_SCHEMA": "Timeline.1",
        "name": e.name,
        "metadata": {},
        "global_start_time": rt(0),
        "tracks": { "OTIO_SCHEMA": "Stack.1", "name": "tracks", "metadata": {}, "children": tracks, "source_range": null, "effects": [], "markers": markers, "enabled": true }
    });
    let _ = json_str;
    serde_json::to_string_pretty(&tl).unwrap_or_default()
}

/// The interchange formats, for menus and file dialogs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Edl,
    Fcpxml,
    Xmeml,
    Otio,
}

impl Format {
    pub fn parse(s: &str) -> Option<Format> {
        Some(match s {
            "edl" => Format::Edl,
            "fcpxml" => Format::Fcpxml,
            "xml" | "xmeml" => Format::Xmeml,
            "otio" => Format::Otio,
            _ => return None,
        })
    }
    pub fn extension(self) -> &'static str {
        match self {
            Format::Edl => "edl",
            Format::Fcpxml => "fcpxml",
            Format::Xmeml => "xml",
            Format::Otio => "otio",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Format::Edl => "CMX 3600 EDL",
            Format::Fcpxml => "Final Cut Pro X (FCPXML 1.10)",
            Format::Xmeml => "Final Cut Pro 7 XML",
            Format::Otio => "OpenTimelineIO",
        }
    }
    /// The file for sequence `seq` of `p` (an EDL starts at 01:00:00:00, on video track 1).
    pub fn write(self, p: &Project, seq: usize) -> String {
        let e = edit_of(p, seq);
        match self {
            Format::Edl => edl(&e, 0, tc_fps(e.rate) * 3600),
            Format::Fcpxml => fcpxml(&e),
            Format::Xmeml => xmeml(&e),
            Format::Otio => otio(&e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::*;
    use crate::time::Rational;

    fn s(x: f64) -> Time {
        Time::from_secs_f64(x)
    }

    /// An edit at 25 fps: on V1 "a" 0–4 s (from 10 s in) dissolving (1 s)
    /// into "b" 4–8 s; "b" again at half speed 9–11 s; a title on V2; the
    /// sound of "a" on A1; a marker at 2 s.
    pub(crate) fn sample() -> Project {
        let mut p = Project::new();
        let rate = FrameRate::new(25, 1);
        for (id, name) in [(1, "Interview A.mov"), (2, "b-roll.mp4")] {
            p.media.push(MediaAsset {
                id: MediaId(id),
                path: format!("/footage/{name}").into(),
                name: name.into(),
                info: MediaInfo {
                    duration: s(60.0),
                    video: Some(VideoStreamInfo { width: 1920, height: 1080, frame_rate: rate, codec: String::new(), time_base: Rational::new(1, 25), hdr: None }),
                    audio: Some(AudioStreamInfo { sample_rate: 48000, channels: 2, codec: String::new() }),
                    container: String::new(),
                },
                conform: None,
            });
        }
        let seq = &mut p.sequences[0];
        (seq.width, seq.height, seq.frame_rate) = (1920, 1080, rate);
        seq.name = "Cut 1".into();
        let a = Clip::new(ClipId(10), MediaId(1), s(0.0), s(10.0), s(4.0), Some(1));
        let mut b = Clip::new(ClipId(11), MediaId(2), s(4.0), s(20.0), s(4.0), None);
        b.dissolve_in = s(1.0);
        let mut slow = Clip::new(ClipId(12), MediaId(2), s(9.0), s(30.0), s(2.0), None);
        slow.speed = 0.5;
        slow.anchor = s(15.0); // media = anchor + speed × clip time: 30 s at its start
        seq.tracks[0].clips = vec![a, b, slow];
        let mut title = Clip::new(ClipId(13), MediaId::GENERATED, s(1.0), Time::ZERO, s(2.0), None);
        title.title = Some(crate::title::TitleSpec { text: "Hello & <welcome>".into(), ..Default::default() });
        seq.tracks[1].clips = vec![title];
        seq.tracks[2].clips = vec![Clip::new(ClipId(14), MediaId(1), s(0.0), s(10.0), s(4.0), Some(1))];
        seq.markers.push(crate::project::Marker { at: s(2.0), name: "Check this".into(), color: Default::default() });
        p
    }

    #[test]
    fn the_edit_in_frames() {
        let p = sample();
        let e = edit_of(&p, 0);
        assert_eq!((e.duration, e.lanes.len()), (275, 3));
        let v1 = &e.lanes[0].items;
        assert_eq!((v1[0].rec, v1[0].src_in, v1[0].reel.as_str()), ((0, 100), 250, "INTERVIE"));
        assert_eq!((v1[1].rec, v1[1].src_in), ((100, 200), 500));
        assert_eq!(v1[1].dissolve, Some((13, 12)), "the 1 s dissolve, split round the cut");
        assert_eq!((v1[2].speed, v1[2].src_in, v1[2].src_out()), (0.5, 750, 775));
        assert_eq!(e.lanes[1].items[0].source, Source::Generated);
        assert_eq!(e.markers, [super::Marker { at: 50, name: "Check this".into() }]);
        assert_eq!(timecode(90_000 + 37, FrameRate::new(25, 1)), "01:00:01:12");
        assert_eq!(file_url(std::path::Path::new("C:\\Clips\\a b#1.mov")), "file:///C:/Clips/a%20b%231.mov");
    }

    #[test]
    fn edl_events() {
        let text = Format::Edl.write(&sample(), 0);
        assert!(text.starts_with("TITLE: Cut 1\nFCM: NON-DROP FRAME\n"), "{text}");
        let has = |line: &str| text.lines().any(|l| l.len() > 3 && l[..3].chars().all(|c| c.is_ascii_digit()) && &l[3..] == line);
        // a: plain cut, up to where the dissolve starts.
        assert!(has("  INTERVIE V     C        00:00:10:00 00:00:13:12 01:00:00:00 01:00:03:12"), "{text}");
        // The dissolve: a zero-length cut of a, then b dissolving in over 25 frames (same event number).
        assert!(text.contains("  INTERVIE V     C        00:00:13:12 00:00:13:12 01:00:03:12 01:00:03:12\n"), "{text}");
        let d = text.lines().position(|l| l.ends_with("BROLL    V     D    025 00:00:19:12 00:00:24:00 01:00:03:12 01:00:08:00")).expect("dissolve");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[d - 1][..3], lines[d][..3], "one event number for both lines");
        assert!(text.contains("M2   BROLL          012.5                00:00:30:00"), "{text}");
        assert!(text.contains("* FROM CLIP NAME: Interview A.mov") && text.contains("* SOURCE FILE: /footage/b-roll.mp4"));
        assert!(has("  INTERVIE A     C        00:00:10:00 00:00:14:00 01:00:00:00 01:00:04:00"), "{text}");
        assert!(text.contains("* MARKER 01:00:02:00 Check this"));
    }

    #[test]
    fn xml_formats_name_files_and_escape_text() {
        let p = sample();
        let fcpx = Format::Fcpxml.write(&p, 0);
        assert!(fcpx.contains("<fcpxml version=\"1.10\">") && fcpx.contains("frameDuration=\"1/25s\""));
        assert!(fcpx.contains("src=\"file:///footage/Interview%20A.mov\""), "{fcpx}");
        assert!(fcpx.contains("<transition name=\"Cross Dissolve\" offset=\"87/25s\" duration=\"1s\"/>"), "{fcpx}");
        assert!(!fcpx.contains("welcome"), "a title above the storyline stays behind: {fcpx}");
        assert!(fcpx.contains("<timeMap>"), "the slow clip");
        let x7 = Format::Xmeml.write(&p, 0);
        assert!(x7.contains("<pathurl>file:///footage/b-roll.mp4</pathurl>") && x7.contains("<file id=\"file-2\"/>"), "{x7}");
        assert!(x7.contains("<value>50</value>"), "half speed");
        assert!(x7.contains("<start>87</start>\n            <end>112</end>"), "{x7}");
        let otio = Format::Otio.write(&p, 0);
        let v: serde_json::Value = serde_json::from_str(&otio).unwrap();
        let v1 = &v["tracks"]["children"][0]["children"];
        assert_eq!(v1[0]["OTIO_SCHEMA"], "Clip.1");
        assert_eq!(v1[1]["OTIO_SCHEMA"], "Transition.1");
        assert_eq!(v1[3]["OTIO_SCHEMA"], "Gap.1");
        assert_eq!(v1[4]["effects"][0]["time_scalar"], 0.5);
    }
}
