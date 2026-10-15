//! What the web interface is sent: plain, serialisable pictures of the
//! editor's state. Times are in ticks (`Time::TICKS_PER_SECOND` per second);
//! ids are numbers.

use reelforge_core::{BlendMode, Clip, ClipAudio, Project, Time, TitleSpec, TrackAudio, TrackKind, Transform};
use reelforge_engine::editor::Editor;
use reelforge_engine::proxies::ProxyState;
use serde::Serialize;

#[derive(Serialize)]
pub struct State {
    pub rev: u64,
    pub title: String,
    pub path: Option<String>,
    pub dirty: bool,
    pub undo: Option<String>,
    pub redo: Option<String>,
    pub history: Vec<String>,
    pub history_pos: usize,
    pub ticks_per_second: i64,
    pub active: usize,
    pub comp_open: usize,
    pub sequences: Vec<SeqInfo>,
    pub media: Vec<MediaView>,
    pub seq: SeqView,
    pub selection: Option<u64>,
    /// Selected clips and the clips linked to them.
    pub selected: Vec<u64>,
    pub inspector: Option<Inspector>,
    pub recoveries: Vec<String>,
    pub presets: Vec<PresetView>,
    pub prefs: reelforge_engine::editor::Prefs,
}

#[derive(Serialize)]
pub struct SeqInfo {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub duration: i64,
}

#[derive(Serialize)]
pub struct MediaView {
    pub id: u64,
    pub name: String,
    pub path: String,
    /// "video", "audio" or "image".
    pub kind: &'static str,
    pub duration: i64,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub has_audio: bool,
    pub missing: bool,
    /// "", "queued", "building 40%", "ready" or "failed".
    pub proxy: String,
    pub detail: String,
}

#[derive(Serialize)]
pub struct SeqView {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub rate_num: i64,
    pub rate_den: i64,
    pub frame: i64,
    pub duration: i64,
    pub in_point: Option<i64>,
    pub out_point: Option<i64>,
    pub markers: Vec<(i64, String)>,
    pub tracks: Vec<TrackView>,
}

#[derive(Serialize)]
pub struct TrackView {
    pub index: usize,
    pub name: String,
    pub kind: &'static str,
    pub muted: bool,
    pub locked: bool,
    pub audio: TrackAudio,
    pub clips: Vec<ClipView>,
}

#[derive(Serialize)]
pub struct ClipView {
    pub id: u64,
    pub name: String,
    /// "video", "audio", "image", "title" or "comp".
    pub kind: &'static str,
    pub media: u64,
    pub start: i64,
    pub duration: i64,
    pub source_in: i64,
    pub link: Option<u64>,
    pub speed: f32,
    pub reverse: bool,
    pub dissolve_in: i64,
    pub fade_in: i64,
    pub fade_out: i64,
    pub effects: usize,
}

/// The selected clip, for the Controls panel.
#[derive(Serialize)]
pub struct Inspector {
    pub id: u64,
    pub name: String,
    pub kind: &'static str,
    pub track: String,
    pub start: i64,
    pub duration: i64,
    pub source_in: i64,
    pub visual: Option<VisualView>,
    pub title: Option<TitleSpec>,
    pub audio: Option<ClipAudio>,
    pub speed: f32,
    pub reverse: bool,
    pub effects: Vec<String>,
    pub masks: usize,
    /// Properties with keyframes (edited in the classic app for now).
    pub animated: Vec<&'static str>,
    pub linked: bool,
}

#[derive(Serialize)]
pub struct VisualView {
    pub transform: Transform,
    pub blend: BlendMode,
    pub layer_size: [f32; 2],
}

#[derive(Serialize)]
pub struct PresetView {
    pub index: usize,
    pub label: &'static str,
    pub hint: &'static str,
    pub extension: &'static str,
}

fn clip_kind(p: &Project, kind: TrackKind, c: &Clip) -> &'static str {
    if c.title.is_some() {
        "title"
    } else if c.nested.is_some() {
        "comp"
    } else if kind == TrackKind::Audio {
        "audio"
    } else if p.is_still(c.media) {
        "image"
    } else {
        "video"
    }
}

fn clip_name(p: &Project, c: &Clip) -> String {
    if let Some(t) = &c.title {
        let first = t.text.lines().next().unwrap_or("").trim();
        return if first.is_empty() { "Title".into() } else { first.to_string() };
    }
    if let Some(s) = c.nested {
        return p.sequences.get(s).map(|s| s.name.clone()).unwrap_or_else(|| "Composition".into());
    }
    p.media(c.media).map(|m| m.name.clone()).unwrap_or_else(|| "(missing)".into())
}

pub fn state(e: &Editor) -> State {
    let p = &e.project;
    let s = e.seq();
    let rate = s.frame_rate;
    let media = p
        .media
        .iter()
        .map(|m| {
            let v = m.info.video.as_ref();
            let kind = match (v, m.info.duration > Time::ZERO) {
                (Some(_), true) => "video",
                (Some(_), false) => "image",
                (None, _) => "audio",
            };
            let proxy = match e.proxies.state(&m.path) {
                None => String::new(),
                Some(ProxyState::Queued) => "queued".into(),
                Some(ProxyState::Building(f)) => format!("building {:.0}%", f * 100.0),
                Some(ProxyState::Ready(_)) => "ready".into(),
                Some(ProxyState::Failed(_)) => "failed".into(),
            };
            let mut detail = Vec::new();
            if m.info.duration > Time::ZERO {
                detail.push(format_duration(m.info.duration));
            }
            if let Some(v) = v {
                detail.push(format!("{}×{}", v.width, v.height));
                if kind == "video" {
                    detail.push(format!("{:.3} fps", v.frame_rate.fps()).replace(".000", ""));
                }
            }
            if let Some(a) = &m.info.audio {
                detail.push(format!("{} Hz {}ch", a.sample_rate, a.channels));
            }
            MediaView {
                id: m.id.0,
                name: m.name.clone(),
                path: m.path.display().to_string(),
                kind,
                duration: m.info.duration.0,
                width: v.map_or(0, |v| v.width),
                height: v.map_or(0, |v| v.height),
                fps: v.map_or(0.0, |v| v.frame_rate.fps()),
                has_audio: m.info.audio.is_some(),
                missing: !m.path.exists(),
                proxy,
                detail: detail.join(" · "),
            }
        })
        .collect();
    let tracks = s
        .tracks
        .iter()
        .enumerate()
        .map(|(i, t)| TrackView {
            index: i,
            name: t.name.clone(),
            kind: if t.kind == TrackKind::Video { "video" } else { "audio" },
            muted: t.muted,
            locked: t.locked,
            audio: t.audio,
            clips: t
                .clips
                .iter()
                .map(|c| ClipView {
                    id: c.id.0,
                    name: clip_name(p, c),
                    kind: clip_kind(p, t.kind, c),
                    media: c.media.0,
                    start: c.timeline_start.0,
                    duration: c.duration.0,
                    source_in: c.source_in.0,
                    link: c.link,
                    speed: c.speed,
                    reverse: c.reverse,
                    dissolve_in: c.dissolve_in.0,
                    fade_in: c.audio.fade_in.0,
                    fade_out: c.audio.fade_out.0,
                    effects: c.effects.len(),
                })
                .collect(),
        })
        .collect();
    let seq = SeqView {
        name: s.name.clone(),
        width: s.width,
        height: s.height,
        rate_num: rate.0.num,
        rate_den: rate.0.den,
        frame: rate.frame_duration().0,
        duration: s.duration().0,
        in_point: s.in_point.map(|t| t.0),
        out_point: s.out_point.map(|t| t.0),
        markers: s.markers.iter().map(|m| (m.at.0, m.name.clone())).collect(),
        tracks,
    };
    let inspector = e.selection.and_then(|id| s.find_clip(id)).map(|(ti, c)| {
        let tr = &s.tracks[ti];
        let kind = clip_kind(p, tr.kind, c);
        let visual = (tr.kind == TrackKind::Video).then(|| {
            let size = match p.picture_size(c) {
                Some([w, h]) => [w as f32, h as f32],
                None => [s.width as f32, s.height as f32],
            };
            VisualView { transform: c.transform, blend: c.blend, layer_size: size }
        });
        let a = &c.anim;
        let mut animated = Vec::new();
        for (name, keyed) in [
            ("position", !a.position.is_empty()),
            ("scale", !a.scale.is_empty()),
            ("rotation", !a.rotation.is_empty()),
            ("opacity", !a.opacity.is_empty()),
        ] {
            if keyed {
                animated.push(name);
            }
        }
        Inspector {
            id: c.id.0,
            name: clip_name(p, c),
            kind,
            track: tr.name.clone(),
            start: c.timeline_start.0,
            duration: c.duration.0,
            source_in: c.source_in.0,
            visual,
            title: c.title.clone(),
            audio: (tr.kind == TrackKind::Audio).then(|| c.audio.clone()),
            speed: c.speed,
            reverse: c.reverse,
            effects: c.effects.iter().map(|fx| fx.plugin.as_ref().map(|p| p.name.clone()).unwrap_or_else(|| fx.kind.label().to_string())).collect(),
            masks: c.masks.len(),
            animated,
            linked: s.link_group(c.id).len() > 1,
        }
    });
    let entries = e.undo.entries();
    State {
        rev: e.rev,
        title: match &e.project_path {
            Some(path) => path.file_stem().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            None => "Untitled".into(),
        },
        path: e.project_path.as_ref().map(|p| p.display().to_string()),
        dirty: e.is_dirty(),
        undo: e.undo.undo_name().map(str::to_owned),
        redo: e.undo.redo_name().map(str::to_owned),
        history: entries.iter().map(|h| h.name.clone()).collect(),
        history_pos: e.undo.position(),
        ticks_per_second: Time::TICKS_PER_SECOND,
        active: e.active,
        comp_open: e.comp_open,
        sequences: p
            .sequences
            .iter()
            .map(|s| SeqInfo { name: s.name.clone(), width: s.width, height: s.height, fps: s.frame_rate.fps(), duration: s.duration().0 })
            .collect(),
        media,
        seq,
        selection: e.selection.map(|c| c.0),
        selected: e.selected_with_links().iter().map(|c| c.0).collect(),
        inspector,
        recoveries: e.recoveries.iter().map(|r| r.describe()).collect(),
        presets: reelforge_engine::presets::Preset::ALL
            .iter()
            .enumerate()
            .map(|(index, p)| PresetView { index, label: p.label(), hint: p.hint(), extension: reelforge_engine::presets::extension(p.spec().and_then(|s| s.video)) })
            .collect(),
        prefs: e.prefs.clone(),
    }
}

/// "1:23.4" or "0:04.0".
pub fn format_duration(t: Time) -> String {
    let s = t.as_secs_f64();
    format!("{}:{:04.1}", (s / 60.0).floor() as i64, s % 60.0)
}
