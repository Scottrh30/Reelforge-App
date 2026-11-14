//! What the web interface is sent: plain, serialisable pictures of the
//! editor's state. Times are in ticks (`Time::TICKS_PER_SECOND` per second);
//! ids are numbers.

use reelforge_core::mask::Mask;
use reelforge_core::track::TrackMode;
use reelforge_core::transform::LayerMap;
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
    /// Clips of the open sequence with a tracker, for the Track panel (which
    /// can work with one while something else is selected).
    pub trackers: Vec<TrackSource>,
    pub recoveries: Vec<String>,
    pub prefs: reelforge_engine::editor::Prefs,
    /// The Viewer's view: "Active" (through the sequence's camera), "Front", "Top", "Left" or "Custom".
    pub view: reelforge_engine::editor::ViewMode,
}

#[derive(Serialize)]
pub struct SeqInfo {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub duration: i64,
    /// A multicam source.
    pub multicam: bool,
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
    /// HDR footage: "PQ" or "HLG".
    pub hdr: Option<String>,
    pub missing: bool,
    /// "", "queued", "building 40%", "ready" or "failed".
    pub proxy: String,
    /// Why the proxy failed.
    pub proxy_error: Option<String>,
    /// What the ready proxy is made as ("ProRes 422 Proxy"…).
    pub proxy_format: Option<&'static str>,
    /// Footage that can have a proxy.
    pub proxyable: bool,
    pub detail: String,
}

#[derive(Serialize)]
pub struct SeqView {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub rate_num: i64,
    pub rate_den: i64,
    /// Its pixels' width ÷ height (1 square), and their name.
    pub par: f32,
    pub par_name: String,
    pub pixel_aspect: (u32, u32),
    pub frame: i64,
    pub duration: i64,
    pub in_point: Option<i64>,
    pub out_point: Option<i64>,
    pub markers: Vec<crate::ops::MarkerView>,
    /// Transitions and crossfades at cuts.
    pub transitions: Vec<crate::trans::TransitionMark>,
    pub tracks: Vec<TrackView>,
    /// The highlighted track: where Paste and new layers go.
    pub target_track: Option<usize>,
    /// The magnetic timeline is on: the main track closes up its gaps.
    pub magnetic: bool,
    /// The main track (the first video track).
    pub main_track: Option<usize>,
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
    /// Row height in the timeline, and whether it's collapsed to a thin row.
    pub height: u16,
    pub collapsed: bool,
    /// Showing a clip's effects and properties in its header.
    pub expanded: bool,
    /// For an expanded track: the clip shown there (the selected one on the
    /// track, else the one at the playhead), `None` if there's none.
    pub props: Option<TrackProps>,
    /// Linked-sound waveforms under its clips: shown, hidden, or as Settings says (`None`).
    pub waveforms: Option<bool>,
}

/// A clip's effects and transform, for an expanded track's header.
#[derive(Serialize)]
pub struct TrackProps {
    pub clip: u64,
    pub name: String,
    /// The transform as seen at the playhead (keyframes evaluated).
    pub transform: Transform,
    /// Opacity, Position, Scale, Anchor and Rotation, in that order.
    pub props: Vec<TrackPropKeys>,
    /// The playhead is on the clip (keyframed properties can be changed there).
    pub on_clip: bool,
    pub effects: Vec<TrackEffect>,
}

/// A transform property's keyframes, for its row.
#[derive(Serialize)]
pub struct TrackPropKeys {
    pub prop: &'static str,
    pub animated: bool,
    pub key_here: bool,
    /// Its keyframes' timeline times.
    pub keys: Vec<i64>,
}

#[derive(Serialize)]
pub struct TrackEffect {
    pub index: usize,
    pub label: String,
    pub enabled: bool,
    /// Its settings (shown when twirled open).
    pub params: Vec<crate::fx::ParamView>,
}

/// The clip an expanded track shows: the selected one on it, else the one at the playhead.
fn track_props(e: &Editor, p: &reelforge_core::Project, t: &reelforge_core::project::Track) -> Option<TrackProps> {
    if !t.expanded || t.collapsed || t.kind != TrackKind::Video {
        return None;
    }
    let ph = e.playback.playhead;
    let c = e.selection.and_then(|id| t.clips.iter().find(|c| c.id == id)).or_else(|| t.clips.iter().find(|c| c.contains(ph)))?;
    if c.scene.is_some() {
        return None;
    }
    use reelforge_core::{Channel, Prop};
    let v = c.visual();
    let (src, on_clip) = e.key_time(c);
    let a = &v.anim;
    let channels = v.channels();
    let props = [(Prop::Opacity, "Opacity"), (Prop::Position, "Position"), (Prop::Scale, "Scale"), (Prop::Anchor, "Anchor"), (Prop::Rotation, "Rotation")]
        .into_iter()
        .map(|(prop, name)| {
            let mut keys: Vec<i64> = channels
                .iter()
                .filter(|ch| matches!(ch, Channel::Transform(q, _) if *q == prop))
                .flat_map(|ch| v.channel_keys(*ch))
                .map(|(at, _, _)| (c.timeline_start + (at - c.source_in)).0)
                .collect();
            keys.sort_unstable();
            keys.dedup();
            TrackPropKeys { prop: name, animated: a.prop_animated(prop), key_here: on_clip && a.has_key(prop, src), keys }
        })
        .collect();
    let effects = c
        .effects
        .iter()
        .enumerate()
        .map(|(index, fx)| {
            let mut n = fx.clone();
            n.normalize();
            TrackEffect { index, label: fx.label(), enabled: fx.enabled, params: crate::fx::param_views(&n, c, src, on_clip) }
        })
        .collect();
    Some(TrackProps { clip: c.id.0, name: clip_name(p, c), transform: a.eval(&v.transform, src), props, on_clip, effects })
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
    /// For a retimed clip: the media time its clip time 0 maps to (see `Clip::media_time`).
    pub anchor: i64,
    /// A composition clip's sequence.
    pub nested: Option<usize>,
    pub dissolve_in: i64,
    pub fade_in: i64,
    pub fade_out: i64,
    pub effects: usize,
    /// Audio clips: the levels, keyframes and fades (for the volume line).
    pub audio: Option<ClipAudio>,
    /// The point clip it's parented to.
    pub parent: Option<u64>,
    /// Its own colour on the timeline (#rrggbb), if one was chosen.
    pub label: Option<String>,
}

/// A template control on a composition clip: what to show and its value now.
#[derive(Serialize)]
pub struct ControlView {
    pub label: String,
    pub kind: reelforge_core::template::Kind,
    pub value: Option<reelforge_core::template::Value>,
    /// For a choice: its options.
    pub options: Vec<&'static str>,
}

/// Something of the selected clip that can be made a control.
#[derive(Serialize)]
pub struct ExposeView {
    pub label: String,
    pub what: reelforge_core::template::Target,
    /// Already a control.
    pub exposed: bool,
}

/// The selected clip, for the Controls panel.
#[derive(Serialize)]
pub struct Inspector {
    /// A composition clip: its composition's template controls.
    pub controls: Vec<ControlView>,
    /// What of this clip could be a control of the sequence it's in.
    pub exposable: Vec<ExposeView>,
    /// The sequence being edited's controls: (label, the clip it's on, what of it).
    pub seq_controls: Vec<(String, u64, String)>,
    pub id: u64,
    pub name: String,
    pub kind: &'static str,
    pub track: String,
    pub start: i64,
    pub duration: i64,
    pub source_in: i64,
    pub visual: Option<VisualView>,
    pub title: Option<TitleSpec>,
    /// The title's font file couldn't be found (the built-in one is used).
    pub title_missing_font: bool,
    /// A 3D model title: how many triangles, or why it can't be shown.
    pub model: Option<ModelInfo>,
    /// The transform gizmo in the viewer (frame pixels), when the layer shows at the playhead.
    pub gizmo: Option<reelforge_engine::gizmo::Geometry>,
    /// A 3D layer's arrows and rings (frame pixels).
    pub gizmo3d: Option<reelforge_engine::gizmo3d::Gizmo3d>,
    pub audio: Option<ClipAudio>,
    pub speed: f32,
    pub reverse: bool,
    /// Time remapping, if it's on.
    pub remap: Option<RemapView>,
    /// What retimed footage shows between its frames: "Nearest", "Blend" or "Flow".
    pub frame_blend: reelforge_core::FrameBlend,
    pub flow_quality: reelforge_core::FlowQuality,
    /// Footage with fewer frames a second than the sequence (so "Between
    /// frames" applies at normal speed too).
    pub fewer_frames: bool,
    /// A multicam clip: its source's angles and the ones seen and heard.
    pub multicam: Option<AngleView>,
    pub effects: Vec<crate::fx::EffectView>,
    /// Transform properties with keyframes.
    pub animated: Vec<&'static str>,
    /// Keyframes: the transform's buttons and the curve editor's channels (video clips).
    pub keys: Option<crate::keys::KeysView>,
    /// The transition (or crossfade) from the clip just before this one.
    pub transition_in: Option<crate::trans::TransitionIn>,
    /// Footage playing forwards: a speed ramp can be made.
    pub can_ramp: bool,
    /// Moving footage (speed, freeze frames).
    pub footage: bool,
    pub linked: bool,
    /// How the layer's pixels (origin at its centre) land on the frame at the
    /// playhead: a 3×3 matrix, row by row (perspective when corner-pinned), and its inverse.
    pub layer_map: Option<[f64; 9]>,
    pub layer_inv: Option<[f64; 9]>,
    /// The layer's size in its own pixels (what masks and trackers are measured in).
    pub layer_px: [f32; 2],
    pub masks: Vec<MaskView>,
    /// Footage that can be tracked.
    pub can_track: bool,
    pub tracker: Option<TrackerView>,
    /// Keys of the clip's mesh warp, if it has one.
    pub mesh_keys: Option<usize>,
    /// A camera, light or point: its points the camera can see (frame pixels).
    pub scene_points: Vec<crate::scene::ScenePoint>,
    /// A plane's colour, #rrggbb.
    pub plane: Option<String>,
    /// The point clip it's parented to, and the ones it can be: (id, name).
    pub parent: Option<u64>,
    pub parents: Vec<(u64, String)>,
    /// A line in the Viewer from the clip to the point it's parented to (frame
    /// pixels), when both show at the playhead.
    pub parent_link: Option<[[f32; 2]; 2]>,
    /// The footage's solved 3D camera, if it has one.
    pub cam_track: Option<CamTrackView>,
    /// The tracking run on this clip is solving its 3D camera.
    pub cam_tracking: bool,
    /// Progress (0–1) of a tracking run on this clip.
    pub tracking: Option<f32>,
    /// A tracking run is going on for another clip.
    pub tracking_elsewhere: bool,
}

/// A 3D model layer's file, as its settings show it.
#[derive(Serialize)]
pub struct AngleView {
    pub angles: Vec<String>,
    pub video: u8,
    /// The angle heard, if it doesn't follow the one seen.
    pub audio: Option<u8>,
}

#[derive(Serialize)]
pub struct ModelInfo {
    pub path: String,
    pub triangles: usize,
    pub error: Option<String>,
    /// Its parts (materials or objects), by name, and how many textures it has.
    pub parts: Vec<String>,
    pub textures: usize,
}

/// A solved 3D camera, as the Track panel and the Viewer show it.
#[derive(Serialize)]
pub struct CamTrackView {
    /// Lens: vertical field of view, degrees.
    pub fov: f32,
    /// Root mean square error, pixels.
    pub error: f32,
    pub frames: usize,
    /// Frames placed from the features (the rest filled in).
    pub placed: u32,
    pub tripod: bool,
    pub point_count: usize,
    /// The solved points seen at the playhead: index, where (frame pixels), how well each fits (pixels).
    pub points: Vec<(usize, [f32; 2], f32)>,
    /// A camera made from it is still in the sequence.
    pub camera: bool,
    /// Every point's error (pixels), in order.
    pub errors: Vec<f32>,
    /// It keeps the details it was solved from: points can be deleted and it solved again.
    pub can_resolve: bool,
    /// How well it fits each frame (average pixels; `None`: no points then). Empty without its details.
    pub fit: Vec<Option<f32>>,
    /// The frame at the playhead.
    pub frame: usize,
}

fn cam_track_view(e: &Editor, c: &Clip) -> Option<CamTrackView> {
    let ct = c.camera_track.as_ref()?;
    let i = e.camera_track_frame(c).unwrap_or(0);
    let [w, h] = ct.frame;
    let points = ct
        .points
        .iter()
        .enumerate()
        .filter(|(_, p)| (p.first as usize) <= i && i <= p.last as usize)
        .filter_map(|(k, p)| {
            let q = ct.project(i, p.pos)?;
            (q[0] >= 0.0 && q[1] >= 0.0 && q[0] <= w && q[1] <= h).then_some((k, q, p.error))
        })
        .collect();
    Some(CamTrackView {
        fov: ct.fov,
        error: ct.error,
        frames: ct.cameras.len(),
        placed: ct.placed,
        tripod: ct.tripod,
        point_count: ct.points.len(),
        points,
        camera: ct.camera.is_some_and(|id| e.seq().find_clip(id).is_some()),
        errors: ct.points.iter().map(|p| p.error).collect(),
        can_resolve: ct.source.is_some(),
        fit: e.camera_fit(c.id).map(|v| v.to_vec()).unwrap_or_default(),
        frame: i,
    })
}

#[derive(Serialize)]
pub struct MaskView {
    /// The whole mask (sent back, changed, by the Masks panel).
    pub mask: Mask,
    /// Its outline and its points as seen at the playhead, in layer pixels.
    pub outline: Vec<[f32; 2]>,
    pub handles: Vec<[f32; 2]>,
    /// Each point's Bézier handles at the playhead: [in x, in y, out x, out y]
    /// offsets in layer pixels (zeros for a sharp corner).
    pub curves: Vec<[f32; 4]>,
    /// The shape is animated and has a key at the playhead.
    pub key_here: bool,
}

#[derive(Serialize)]
pub struct TrackerView {
    pub mode: TrackMode,
    pub size: f32,
    pub search: f32,
    /// Each point where it's shown at the playhead, and its tracked path (layer pixels).
    pub shown: Vec<[f32; 2]>,
    pub paths: Vec<Vec<[f32; 2]>>,
    /// Frames where every point is tracked, and their first and last timeline times.
    pub frames: usize,
    pub range: Option<(i64, i64)>,
    pub tracked: bool,
    /// What the motion can be applied to.
    pub targets: Vec<TargetView>,
    /// A mesh tracker's cells across and down.
    pub grid: [u8; 2],
    /// Smoothing, in frames, and (when there is some) each point's smoothed path.
    pub smooth: f32,
    pub smoothed: Vec<Vec<[f32; 2]>>,
}

#[derive(Serialize)]
pub struct TrackSource {
    pub id: u64,
    pub name: String,
    pub track: String,
    pub tracker: TrackerView,
}

#[derive(Serialize)]
pub struct TargetView {
    pub clip: u64,
    /// A mask of the tracked clip, instead of a clip.
    pub mask: Option<usize>,
    /// An energy blade effect of `clip` (its index in the stack), instead of the clip.
    pub blade: Option<usize>,
    /// A point (null) clip: it moves, turns and scales, and its children follow.
    pub point: bool,
    pub label: String,
}

fn matrix(m: &LayerMap) -> [f64; 9] {
    match m {
        LayerMap::Affine(a) => [a.a as f64, a.c as f64, a.tx as f64, a.b as f64, a.d as f64, a.ty as f64, 0.0, 0.0, 1.0],
        LayerMap::Projective(h) => h.0,
    }
}

fn tracker_view(e: &Editor, c: &Clip) -> Option<TrackerView> {
    let trk = c.tracker.as_ref()?;
    let s = e.seq();
    let src = e.track_time(c);
    let frames = trk.frames();
    let to_seq = |m: Time| c.timeline_of_media(m).unwrap_or(c.timeline_start);
    let range = match (frames.first(), frames.last()) {
        (Some(a), Some(b)) => {
            let (x, y) = (to_seq(a.0), to_seq(b.0));
            Some((x.min(y).0, x.max(y).0))
        }
        _ => None,
    };
    let mut targets = Vec::new();
    let stabilise = match trk.mode {
        TrackMode::CornerPin | TrackMode::Planar => Some("This clip: stabilise (perspective)"),
        TrackMode::Transform => Some("This clip: stabilise (with rotation and scale)"),
        TrackMode::Position => Some("This clip: stabilise"),
        // A bending grid can't be undone by moving the clip.
        TrackMode::Mesh => None,
    };
    if let Some(label) = stabilise {
        targets.push(TargetView { clip: c.id.0, mask: None, blade: None, point: false, label: label.into() });
    }
    for (i, m) in c.masks.iter().enumerate() {
        targets.push(TargetView { clip: c.id.0, mask: Some(i), blade: None, point: false, label: format!("Mask: {}", m.name) });
    }
    // Energy blades: on this clip, or (below) on layers over it.
    // (And patch replacers, whose target follows the same way.)
    let blades = |o: &Clip| -> Vec<usize> { o.effects.iter().enumerate().filter(|(_, f)| matches!(f.kind, reelforge_core::EffectKind::EnergyBlade | reelforge_core::EffectKind::PatchReplacer)).map(|(i, _)| i).collect() };
    let blade_label = |o: &Clip, list: &[usize], i: usize| {
        let patch = o.effects[i].kind == reelforge_core::EffectKind::PatchReplacer;
        let same: Vec<usize> = list.iter().copied().filter(|&j| (o.effects[j].kind == reelforge_core::EffectKind::PatchReplacer) == patch).collect();
        let name = if patch { "Patch replacer" } else { "Energy blade" };
        let what = if patch { "target" } else { "hilt and tip" };
        if same.len() > 1 { format!("{name} {}: {what}", same.iter().position(|&j| j == i).unwrap_or(0) + 1) } else { format!("{name}: {what}") }
    };
    if trk.mode != TrackMode::Mesh {
        let list = blades(c);
        for &i in &list {
            targets.push(TargetView { clip: c.id.0, mask: None, blade: Some(i), point: false, label: format!("{} (this clip)", blade_label(c, &list, i)) });
        }
    }
    if let Some((ta, tb)) = range {
        for tr in s.tracks.iter().filter(|t| t.kind == TrackKind::Video) {
            for o in tr.clips.iter().filter(|o| o.id != c.id && o.timeline_start.0 < tb && o.timeline_end().0 > ta) {
                // Cameras and lights follow by being parented to a tracked point.
                match o.scene {
                    Some(reelforge_core::space3d::SceneObject::Point) => {
                        let kids = s.tracks.iter().flat_map(|t| &t.clips).filter(|k| k.parent == Some(o.id)).count();
                        let with = match kids {
                            0 => String::new(),
                            1 => " and its 1 child".into(),
                            n => format!(" and its {n} children"),
                        };
                        targets.push(TargetView { clip: o.id.0, mask: None, blade: None, point: true, label: format!("Point{with} ({})", tr.name) });
                        continue;
                    }
                    Some(_) => continue,
                    None => {}
                }
                let how = match trk.mode {
                    TrackMode::Mesh => "Warp over the grid: ",
                    m if m.is_perspective() => "Pin into the surface: ",
                    _ => "",
                };
                targets.push(TargetView { clip: o.id.0, mask: None, blade: None, point: false, label: format!("{how}{} ({})", o.display_name(&e.project), tr.name) });
                if trk.mode != TrackMode::Mesh {
                    let list = blades(o);
                    for &i in &list {
                        targets.push(TargetView { clip: o.id.0, mask: None, blade: Some(i), point: false, label: format!("{} ({}, {})", blade_label(o, &list, i), o.display_name(&e.project), tr.name) });
                    }
                }
            }
        }
    }
    Some(TrackerView {
        mode: trk.mode,
        size: trk.points[0].size,
        search: trk.points[0].search,
        shown: trk.shown_at(src),
        paths: trk.points.iter().map(|p| p.path.iter().map(|s| s.pos).collect()).collect(),
        frames: frames.len(),
        range,
        tracked: trk.is_tracked(),
        targets,
        grid: trk.grid,
        smooth: trk.smooth,
        smoothed: if trk.smooth > 0.0 { (0..trk.points.len()).map(|k| frames.iter().map(|f| f.1[k]).collect()).collect() } else { Vec::new() },
    })
}

#[derive(Serialize)]
pub struct VisualView {
    pub transform: Transform,
    pub blend: BlendMode,
    pub track_matte: Option<reelforge_core::TrackMatte>,
    pub layer_size: [f32; 2],
}

fn clip_kind(p: &Project, kind: TrackKind, c: &Clip) -> &'static str {
    if let Some(s) = c.scene {
        match s {
            reelforge_core::space3d::SceneObject::Camera => "camera",
            reelforge_core::space3d::SceneObject::Light => "light",
            reelforge_core::space3d::SceneObject::Point => "point",
        }
    } else if c.plane.is_some() {
        "plane"
    } else if c.adjustment {
        "adjustment"
    } else if c.title.is_some() {
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
    if c.scene.is_some() || c.plane.is_some() || c.adjustment {
        return c.display_name(p).into_owned();
    }
    if let Some(t) = &c.title {
        let first = t.text.lines().next().unwrap_or("").trim();
        if let Some(name) = c.effect_layer_name().filter(|_| first.is_empty()) {
            return name.into();
        }
        return if first.is_empty() { "Title".into() } else { first.to_string() };
    }
    if let (Some(a), Some(s)) = (c.angle, c.nested.and_then(|s| p.sequences.get(s)).filter(|s| s.multicam)) {
        // A multicam clip: the angle it's on.
        let names = reelforge_core::multicam::angle_names(s);
        return names.get(a.video as usize).cloned().unwrap_or_else(|| format!("Angle {}", a.video + 1));
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
            let proxy_error = match e.proxies.state(&m.path) {
                Some(ProxyState::Failed(err)) => Some(err.clone()),
                _ => None,
            };
            let proxy_format = match e.proxies.state(&m.path) {
                Some(ProxyState::Ready(p)) => reelforge_media::proxy::format_of(p).map(|f| f.label()),
                _ => None,
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
                hdr: m.info.video.as_ref().and_then(|v| v.hdr.clone()),
                missing: !reelforge_core::imageseq::media_exists(&m.path),
                proxy,
                proxy_error,
                proxy_format,
                proxyable: kind == "video",
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
            height: t.height,
            collapsed: t.collapsed,
            expanded: t.expanded && !t.collapsed && t.kind == TrackKind::Video,
            props: track_props(e, p, t),
            waveforms: t.waveforms,
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
                    anchor: c.anchor.0,
                    nested: c.nested,
                    dissolve_in: c.dissolve_in.0,
                    fade_in: c.audio.fade_in.0,
                    fade_out: c.audio.fade_out.0,
                    effects: c.effects.len(),
                    audio: (t.kind == TrackKind::Audio).then(|| c.audio.clone()),
                    parent: c.parent.map(|p| p.0),
                    label: c.label.map(|[r, g, b]| format!("#{r:02x}{g:02x}{b:02x}")),
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
        par: s.par(),
        par_name: reelforge_core::project::pixel_aspect_name(s.pixel_aspect),
        pixel_aspect: s.pixel_aspect,
        frame: rate.frame_duration().0,
        duration: s.duration().0,
        in_point: s.in_point.map(|t| t.0),
        out_point: s.out_point.map(|t| t.0),
        markers: crate::ops::markers(s),
        transitions: crate::trans::marks(p, e.active),
        tracks,
        target_track: e.target_track(),
        magnetic: s.magnetic,
        main_track: reelforge_engine::editor::magnetic::main_track(s),
    };
    let inspector = e.selection.and_then(|id| s.find_clip(id)).map(|(ti, c)| {
        let tr = &s.tracks[ti];
        let kind = clip_kind(p, tr.kind, c);
        let visual = (tr.kind == TrackKind::Video && c.scene.is_none()).then(|| {
            let size = p.layer_dims(c, s).unwrap_or([s.width as f32, s.height as f32]);
            VisualView { transform: c.transform, blend: c.blend, track_matte: c.track_matte, layer_size: size }
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
        let layer_px = e.layer_size(c);
        let video = tr.kind == TrackKind::Video;
        let map = if video && c.scene.is_none() { e.layer_map_of(c) } else { None };
        let mt = e.mask_time(c);
        let masks = c
            .masks
            .iter()
            .map(|m| MaskView { mask: m.clone(), outline: m.outline(mt), handles: m.points_at(mt), curves: m.curves_at(mt), key_here: m.keys.iter().any(|k| k.at == mt) })
            .collect();
        let footage = c.title.is_none() && c.nested.is_none() && p.media(c.media).is_some_and(|m| m.info.video.is_some() && m.info.duration > Time::ZERO);
        let job = e.tracking.as_ref().map(|j| (j.clip, j.progress));
        let cam_job = e.tracking.as_ref().is_some_and(|j| j.camera && j.clip == c.id);
        use reelforge_core::template::Target;
        let controls = c
            .nested
            .and_then(|n| p.sequences.get(n))
            .map(|s| {
                s.controls
                    .iter()
                    .map(|k| {
                        let target = s.find_clip(k.clip).map(|(_, x)| x);
                        let options = match k.what {
                            Target::Param { effect, param } => target.and_then(|x| x.effects.get(effect)).and_then(|f| f.kind.params().get(param)).and_then(|pp| match pp.ui {
                                reelforge_core::effects::ParamUi::Choice(o) => Some(o.to_vec()),
                                _ => None,
                            }),
                            _ => None,
                        };
                        ControlView { label: k.label.clone(), kind: target.map_or(reelforge_core::template::Kind::Number, |x| k.what.kind(x)), value: target.and_then(|x| k.what.get(x)), options: options.unwrap_or_default() }
                    })
                    .collect()
            })
            .unwrap_or_default();
        let here = &e.seq().controls;
        let mut can: Vec<Target> = Vec::new();
        if c.title.is_some() {
            can.extend([Target::Text, Target::TextColour]);
        }
        if video {
            can.extend([Target::Opacity, Target::Position]);
            for (ei, f) in c.effects.iter().enumerate() {
                for pi in 0..f.params.len() {
                    can.push(Target::Param { effect: ei, param: pi });
                }
            }
        }
        let exposable = can.into_iter().map(|w| ExposeView { label: w.describe(c), what: w, exposed: here.iter().any(|k| k.clip == c.id && k.what == w) }).collect();
        let seq_controls = here.iter().map(|k| (k.label.clone(), k.clip.0, e.seq().find_clip(k.clip).map_or("(gone)".into(), |(_, x)| format!("{}: {}", x.display_name(p), k.what.describe(x))))).collect();
        Inspector {
            controls,
            exposable,
            seq_controls,
            layer_map: map.as_ref().map(matrix),
            layer_inv: map.and_then(|m| m.inverse()).as_ref().map(matrix),
            layer_px,
            masks,
            can_track: video && footage,
            tracker: if video { tracker_view(e, c) } else { None },
            mesh_keys: c.mesh.as_ref().map(|m| m.keys.len()),
            scene_points: crate::scene::points(e),
            plane: c.plane.map(|[r, g, b, _]| format!("#{r:02x}{g:02x}{b:02x}")),
            parent: c.parent.map(|p| p.0),
            parent_link: crate::scene::parent_link(e, c),
            parents: if tr.kind == TrackKind::Video { e.parent_choices(c.id).into_iter().map(|(id, n)| (id.0, n)).collect() } else { Vec::new() },
            cam_track: if video && footage { cam_track_view(e, c) } else { None },
            cam_tracking: cam_job,
            tracking: job.filter(|(id, _)| *id == c.id).map(|(_, p)| p),
            tracking_elsewhere: job.is_some_and(|(id, _)| id != c.id),
            id: c.id.0,
            name: clip_name(p, c),
            kind,
            track: tr.name.clone(),
            start: c.timeline_start.0,
            duration: c.duration.0,
            source_in: c.source_in.0,
            visual,
            title: c.title.clone(),
            title_missing_font: c.title.is_some() && e.preview.title_missing_font(c.id),
            model: c.title.as_ref().and_then(|t| t.model.as_ref()).map(|m| match reelforge_render::model3d::cached(m) {
                Ok(md) => ModelInfo { triangles: md.indices.len() / 3, error: None, path: m.path.display().to_string(), parts: md.parts.clone(), textures: md.atlas.as_ref().map_or(0, |a| a.rects.len()) },
                Err(err) => ModelInfo { triangles: 0, error: Some(err), path: m.path.display().to_string(), parts: Vec::new(), textures: 0 },
            }),
            gizmo: if video { crate::viewer::geometry(e) } else { None },
            gizmo3d: if video { crate::viewer::gizmo_3d(e) } else { None },
            audio: (tr.kind == TrackKind::Audio).then(|| c.audio.clone()),
            speed: c.speed,
            reverse: c.reverse,
            remap: remap_view(e, c),
            frame_blend: c.frame_blend,
            flow_quality: c.flow_quality,
            fewer_frames: !p.is_still(c.media)
                && p.media(c.media).and_then(|m| m.info.video.as_ref()).map(|v| v.frame_rate.fps()).is_some_and(|r| r.is_finite() && r > 0.0 && r < e.seq().frame_rate.fps() * 0.999),
            multicam: c.angle.zip(c.nested.and_then(|i| p.sequences.get(i)).filter(|s| s.multicam)).map(|(a, s)| AngleView { angles: reelforge_core::multicam::angle_names(s), video: a.video, audio: a.audio }),
            effects: crate::fx::views(e, c),
            animated,
            keys: (video && c.scene.is_none()).then(|| crate::keys::view(e, c)),
            transition_in: crate::trans::transition_in(e, c, tr.kind),
            can_ramp: !p.is_still(c.media) && c.title.is_none() && !c.reverse && c.speed > 0.0,
            footage: !p.is_still(c.media) && c.title.is_none(),
            linked: s.link_group(c.id).len() > 1,
        }
    });
    let trackers = s
        .tracks
        .iter()
        .filter(|t| t.kind == TrackKind::Video)
        .flat_map(|t| t.clips.iter().filter_map(move |c| Some(TrackSource { id: c.id.0, name: clip_name(p, c), track: t.name.clone(), tracker: tracker_view(e, c)? })))
        .collect();
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
            .map(|s| SeqInfo { name: s.name.clone(), width: s.width, height: s.height, fps: s.frame_rate.fps(), duration: s.duration().0, multicam: s.multicam })
            .collect(),
        media,
        seq,
        selection: e.selection.map(|c| c.0),
        selected: e.selected_with_links().iter().map(|c| c.0).collect(),
        inspector,
        trackers,
        recoveries: e.recoveries.iter().map(|r| r.describe()).collect(),
        prefs: e.prefs.clone(),
        view: e.view_mode(),
    }
}

/// "1:23.4" or "0:04.0".
pub fn format_duration(t: Time) -> String {
    let s = t.as_secs_f64();
    format!("{}:{:04.1}", (s / 60.0).floor() as i64, s % 60.0)
}

/// A clip's time remap for the Speed section.
#[derive(Serialize)]
pub struct RemapView {
    /// Keys: (seconds into the clip, media seconds shown).
    pub keys: Vec<[f64; 2]>,
    /// "Linear", "Ease" or "Hold" (the first key's).
    pub interp: reelforge_core::anim::Interp,
    /// The clip's length (s), and the media time and speed (%) at the playhead.
    pub length: f64,
    pub here: f64,
    pub speed_here: f64,
    /// The playhead's place in the clip (s), and whether there's a key there.
    pub at: f64,
    pub key_here: bool,
    /// How long the media is (s), for the graph.
    pub media_length: f64,
}

fn remap_view(e: &Editor, c: &reelforge_core::Clip) -> Option<RemapView> {
    let r = c.remap.as_ref()?;
    let t = e.playback.playhead.max(c.timeline_start).min(c.timeline_end() - reelforge_core::Time(1));
    let ct = c.source_time(t);
    Some(RemapView {
        keys: r.0.iter().map(|k| [(k.at - c.source_in).as_secs_f64(), k.value as f64]).collect(),
        interp: r.0.first().map_or(reelforge_core::anim::Interp::Linear, |k| k.interp),
        length: c.duration.as_secs_f64(),
        here: c.media_time(ct).as_secs_f64(),
        speed_here: c.rate_at(ct) * 100.0,
        at: (ct - c.source_in).as_secs_f64(),
        key_here: r.index_at(ct).is_some(),
        media_length: e.project.media(c.media).map_or(0.0, |m| m.info.duration.as_secs_f64()),
    })
}
