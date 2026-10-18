//! Project data model. Serializable, renderer-agnostic.

use crate::anim::TransformAnim;
use crate::audio::{ClipAudio, TrackAudio};
use crate::effects::{eval_stack, Effect, EffectValues};
use crate::time::{FrameRate, Rational, Time};
use crate::title::TitleSpec;
use crate::transform::{BlendMode, Transform};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct MediaId(pub u64);

impl MediaId {
    /// The "media" of clips whose picture is generated (titles): no file behind it.
    pub const GENERATED: MediaId = MediaId(0);
    /// The "media" of composition clips: another sequence of the project (see `Clip::nested`).
    pub const NESTED: MediaId = MediaId(u64::MAX);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ClipId(pub u64);

/// What we learned about a file when importing it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MediaInfo {
    pub duration: Time,
    pub video: Option<VideoStreamInfo>,
    pub audio: Option<AudioStreamInfo>,
    pub container: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VideoStreamInfo {
    pub width: u32,
    pub height: u32,
    pub frame_rate: FrameRate,
    pub codec: String,
    pub time_base: Rational,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AudioStreamInfo {
    pub sample_rate: u32,
    pub channels: u16,
    pub codec: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MediaAsset {
    pub id: MediaId,
    pub path: PathBuf,
    pub name: String,
    pub info: MediaInfo,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum TrackKind {
    Video,
    Audio,
}

/// A placement of (part of) a media asset on a track.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Clip {
    pub id: ClipId,
    pub media: MediaId,
    /// Where the clip starts on the sequence timeline.
    pub timeline_start: Time,
    /// Where in the source file playback begins.
    pub source_in: Time,
    pub duration: Time,
    /// Clips sharing a link id (e.g. a video clip and its audio) move, split
    /// and delete together.
    #[serde(default)]
    pub link: Option<u64>,
    /// Position/scale/rotation/opacity on the frame (video clips).
    #[serde(default)]
    pub transform: Transform,
    #[serde(default)]
    pub blend: BlendMode,
    /// Volume, pan and fades (audio clips).
    #[serde(default)]
    pub audio: ClipAudio,
    /// Keyframes for transform properties (video clips).
    #[serde(default)]
    pub anim: TransformAnim,
    /// Length of a cross-dissolve from the clip that ends exactly where this one
    /// starts on the same video track (0 = a plain cut).
    #[serde(default)]
    pub dissolve_in: Time,
    /// What kind of transition `dissolve_in` is (cross-dissolve, wipe, slide…).
    #[serde(default, skip_serializing_if = "is_default_transition")]
    pub transition: crate::transition::Transition,
    /// Effects applied to the layer, top of the list first (video clips).
    #[serde(default)]
    pub effects: Vec<Effect>,
    /// For a title clip: the text and how it looks (its media is `MediaId::GENERATED`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<TitleSpec>,
    /// For a composition clip: the index of the sequence it plays (its media
    /// is `MediaId::NESTED`). Its source time is that sequence's time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nested: Option<usize>,
    /// Playback speed: 1 = normal, 0.5 = half speed (slow motion), 2 = double,
    /// 0 = a freeze frame. See [`Clip::media_time`].
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub speed: f32,
    /// Play the media backwards.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub reverse: bool,
    /// With a speed change or reverse, media time = `anchor` + rate × clip time.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub anchor: Time,
    /// Masks cutting the layer down, combined in order (see `mask`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub masks: Vec<crate::mask::Mask>,
    /// A motion tracker on this clip (see `track`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tracker: Option<crate::track::Tracker>,
    /// The layer stretched over a bending grid (from a mesh tracker): while
    /// set, it replaces position, scale, rotation and corner pin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mesh: Option<crate::meshwarp::MeshWarp>,
}

/// Everything about how a video clip looks, edited and undone as one unit.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClipVisual {
    pub transform: Transform,
    pub blend: BlendMode,
    pub anim: TransformAnim,
    pub effects: Vec<Effect>,
    pub masks: Vec<crate::mask::Mask>,
    pub mesh: Option<crate::meshwarp::MeshWarp>,
}

impl Clip {
    /// A clip with default transform and blend mode.
    pub fn new(id: ClipId, media: MediaId, timeline_start: Time, source_in: Time, duration: Time, link: Option<u64>) -> Self {
        Self {
            id,
            media,
            timeline_start,
            source_in,
            duration,
            link,
            transform: Transform::default(),
            blend: BlendMode::Normal,
            audio: ClipAudio::default(),
            anim: TransformAnim::default(),
            dissolve_in: Time::ZERO,
            transition: Default::default(),
            effects: Vec::new(),
            title: None,
            nested: None,
            speed: 1.0,
            reverse: false,
            anchor: Time::ZERO,
            masks: Vec::new(),
            tracker: None,
            mesh: None,
        }
    }

    /// A title clip on a video track.
    pub fn new_title(id: ClipId, timeline_start: Time, duration: Time, spec: TitleSpec) -> Self {
        Self { title: Some(spec), ..Self::new(id, MediaId::GENERATED, timeline_start, Time::ZERO, duration, None) }
    }

    /// A composition clip playing sequence `seq` from its start.
    pub fn new_nested(id: ClipId, seq: usize, timeline_start: Time, duration: Time, link: Option<u64>) -> Self {
        Self { nested: Some(seq), ..Self::new(id, MediaId::NESTED, timeline_start, Time::ZERO, duration, link) }
    }

    /// What to call the clip in the UI: the media name, a title's first line, or a composition's name.
    pub fn display_name<'a>(&'a self, p: &'a Project) -> std::borrow::Cow<'a, str> {
        if let Some(t) = &self.title {
            return std::borrow::Cow::Owned(t.label());
        }
        if let Some(s) = self.nested {
            return std::borrow::Cow::Borrowed(p.sequences.get(s).map(|s| s.name.as_str()).unwrap_or("<missing composition>"));
        }
        std::borrow::Cow::Borrowed(p.media(self.media).map(|m| m.name.as_str()).unwrap_or("<missing media>"))
    }

    pub fn visual(&self) -> ClipVisual {
        ClipVisual { transform: self.transform, blend: self.blend, anim: self.anim.clone(), effects: self.effects.clone(), masks: self.masks.clone(), mesh: self.mesh.clone() }
    }

    pub fn set_visual(&mut self, v: ClipVisual) {
        (self.transform, self.blend, self.anim, self.effects, self.masks, self.mesh) = (v.transform, v.blend, v.anim, v.effects, v.masks, v.mesh);
    }

    /// The mesh warp's grid at sequence time `t`, if the clip has one.
    pub fn mesh_at(&self, t: Time) -> Option<crate::meshwarp::MeshShape> {
        self.mesh.as_ref()?.at(self.source_time(t))
    }

    /// The enabled effects evaluated at *source* time `src`, ready to render.
    pub fn effects_at(&self, src: Time) -> Vec<EffectValues> {
        eval_stack(&self.effects, src)
    }

    /// The (possibly animated) transform at sequence time `t`.
    pub fn transform_at(&self, t: Time) -> Transform {
        self.anim.eval(&self.transform, self.source_time(t))
    }

    pub fn timeline_end(&self) -> Time {
        self.timeline_start + self.duration
    }
    pub fn contains(&self, t: Time) -> bool {
        t >= self.timeline_start && t < self.timeline_end()
    }
    /// The clip's own time at sequence time `t`. Keyframes, effects, masks and
    /// volume automation are anchored to it. At normal speed it's also the
    /// media time; with a speed change, [`Clip::media_at`] gives that.
    pub fn source_time(&self, t: Time) -> Time {
        self.source_in + (t - self.timeline_start)
    }

    /// Media seconds per timeline second: negative when reversed, 0 for a freeze frame.
    pub fn rate(&self) -> f64 {
        let s = self.speed.max(0.0) as f64;
        if self.reverse { -s } else { s }
    }

    /// Does the clip play at anything other than normal speed, forwards?
    pub fn is_retimed(&self) -> bool {
        self.speed != 1.0 || self.reverse
    }

    /// The media time shown at clip time `ct`.
    pub fn media_time(&self, ct: Time) -> Time {
        if !self.is_retimed() {
            return ct;
        }
        self.anchor + scale(ct, self.rate())
    }

    /// The media time shown at sequence time `t`.
    pub fn media_at(&self, t: Time) -> Time {
        self.media_time(self.source_time(t))
    }

    /// The clip time at which media time `m` is shown (none for a freeze frame).
    pub fn clip_time_of_media(&self, m: Time) -> Option<Time> {
        if !self.is_retimed() {
            return Some(m);
        }
        let r = self.rate();
        (r != 0.0).then(|| scale(m - self.anchor, 1.0 / r))
    }

    /// The sequence time at which media time `m` is shown (if it ever is).
    pub fn timeline_of_media(&self, m: Time) -> Option<Time> {
        self.clip_time_of_media(m).map(|ct| self.timeline_start + (ct - self.source_in))
    }

    /// The clip times whose media lies inside `[0, media_len]`: how far the clip
    /// can be trimmed out or slipped. Unbounded for a freeze frame.
    pub fn clip_time_limits(&self, media_len: Time) -> (Time, Time) {
        let big = Time(i64::MAX / 4);
        let r = self.rate();
        if !self.is_retimed() {
            return (Time::ZERO, media_len);
        }
        if r == 0.0 {
            return (-big, big);
        }
        let (a, b) = (scale(Time::ZERO - self.anchor, 1.0 / r), scale(media_len - self.anchor, 1.0 / r));
        (a.min(b), a.max(b))
    }
}

/// `t` × `k`, rounded to the nearest tick.
pub fn scale(t: Time, k: f64) -> Time {
    Time((t.0 as f64 * k).round() as i64)
}

fn is_default_transition(t: &crate::transition::Transition) -> bool {
    *t == crate::transition::Transition::Dissolve
}

fn one() -> f32 {
    1.0
}
fn is_one(v: &f32) -> bool {
    *v == 1.0
}
fn is_zero(t: &Time) -> bool {
    *t == Time::ZERO
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Track {
    pub name: String,
    pub kind: TrackKind,
    /// Kept sorted by `timeline_start`, non-overlapping.
    pub clips: Vec<Clip>,
    pub muted: bool,
    /// Track volume and pan, applied on top of each clip's (audio tracks).
    #[serde(default)]
    pub audio: TrackAudio,
    /// A locked track's clips can't be edited, and ripple edits leave them where they are.
    #[serde(default)]
    pub locked: bool,
}

impl Track {
    pub fn new(name: impl Into<String>, kind: TrackKind) -> Self {
        Self { name: name.into(), kind, clips: Vec::new(), muted: false, audio: TrackAudio::default(), locked: false }
    }
    pub fn clip_at(&self, t: Time) -> Option<&Clip> {
        self.clips.iter().find(|c| c.contains(t))
    }
    pub fn end(&self) -> Time {
        self.clips.iter().map(Clip::timeline_end).max().unwrap_or(Time::ZERO)
    }
    pub fn sort(&mut self) {
        self.clips.sort_by_key(|c| c.timeline_start);
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Sequence {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub frame_rate: FrameRate,
    /// Index 0 is the bottom video track; higher tracks composite on top.
    pub tracks: Vec<Track>,
    /// In and out marks (e.g. the range to export). The out point is exclusive.
    #[serde(default)]
    pub in_point: Option<Time>,
    #[serde(default)]
    pub out_point: Option<Time>,
    /// Markers on the timeline ruler, sorted by time.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub markers: Vec<Marker>,
}

/// A named point on the timeline (M adds one at the playhead).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Marker {
    pub at: Time,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub color: MarkerColor,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MarkerColor {
    #[default]
    Yellow,
    Red,
    Green,
    Blue,
    Purple,
}

impl MarkerColor {
    pub const ALL: [MarkerColor; 5] = [MarkerColor::Yellow, MarkerColor::Red, MarkerColor::Green, MarkerColor::Blue, MarkerColor::Purple];
    pub fn rgb(self) -> [u8; 3] {
        match self {
            MarkerColor::Yellow => [240, 200, 60],
            MarkerColor::Red => [230, 80, 80],
            MarkerColor::Green => [90, 200, 110],
            MarkerColor::Blue => [80, 150, 240],
            MarkerColor::Purple => [180, 110, 230],
        }
    }
}

impl Sequence {
    pub fn new(name: impl Into<String>, width: u32, height: u32, frame_rate: FrameRate) -> Self {
        Self {
            name: name.into(),
            width,
            height,
            frame_rate,
            tracks: vec![
                Track::new("Video 1", TrackKind::Video),
                Track::new("Video 2", TrackKind::Video),
                Track::new("Audio 1", TrackKind::Audio),
            ],
            in_point: None,
            out_point: None,
            markers: Vec::new(),
        }
    }

    /// Places worth jumping to with Up/Down: every clip's start and end, the
    /// markers, the in and out points, and the start. Sorted, no repeats.
    pub fn edit_points(&self) -> Vec<Time> {
        let mut v: Vec<Time> = std::iter::once(Time::ZERO)
            .chain(self.tracks.iter().flat_map(|t| &t.clips).flat_map(|c| [c.timeline_start, c.timeline_end()]))
            .chain(self.markers.iter().map(|m| m.at))
            .chain(self.in_point)
            .chain(self.out_point)
            .collect();
        v.sort();
        v.dedup();
        v
    }

    /// A name for a new marker: "Marker 3", the first number not taken.
    pub fn next_marker_name(&self) -> String {
        (1..).map(|n| format!("Marker {n}")).find(|name| !self.markers.iter().any(|m| &m.name == name)).unwrap()
    }
    pub fn duration(&self) -> Time {
        self.tracks.iter().map(Track::end).max().unwrap_or(Time::ZERO)
    }

    /// The marked range `[in, out)`, falling back to the start and end of the
    /// sequence for a missing mark. `None` if the marks don't make a range.
    pub fn marked_range(&self) -> Option<(Time, Time)> {
        let start = self.in_point.unwrap_or(Time::ZERO);
        let end = self.out_point.unwrap_or_else(|| self.duration());
        (end > start).then_some((start, end))
    }

    /// Is `id`, or any clip linked to it, on a locked track? Such clips can't be edited.
    pub fn is_locked(&self, id: ClipId) -> bool {
        self.link_group(id).iter().any(|(ti, _)| self.tracks[*ti].locked)
    }

    /// Where a new track of `kind` goes: video tracks stack on top of the
    /// other video tracks (before the audio ones), audio tracks go last.
    pub fn new_track_index(&self, kind: TrackKind) -> usize {
        match kind {
            TrackKind::Video => self.tracks.iter().rposition(|t| t.kind == TrackKind::Video).map_or(0, |i| i + 1),
            TrackKind::Audio => self.tracks.len(),
        }
    }

    /// "Video 3", "Audio 2"…: the first name of that pattern not already used.
    pub fn next_track_name(&self, kind: TrackKind) -> String {
        let base = if kind == TrackKind::Video { "Video" } else { "Audio" };
        (1..).map(|n| format!("{base} {n}")).find(|name| !self.tracks.iter().any(|t| &t.name == name)).unwrap()
    }

    /// Topmost visible video clip at `t` — what the viewer should show until
    /// a real compositor blends all layers.
    pub fn top_video_clip_at(&self, t: Time) -> Option<&Clip> {
        self.tracks
            .iter()
            .rev()
            .filter(|tr| tr.kind == TrackKind::Video && !tr.muted)
            .find_map(|tr| tr.clip_at(t))
    }
    pub fn find_clip(&self, id: ClipId) -> Option<(usize, &Clip)> {
        self.tracks.iter().enumerate().find_map(|(i, tr)| tr.clips.iter().find(|c| c.id == id).map(|c| (i, c)))
    }

    /// `id` plus every clip linked to it, as `(track, clip)`.
    pub fn link_group(&self, id: ClipId) -> Vec<(usize, &Clip)> {
        let Some((ti, clip)) = self.find_clip(id) else { return Vec::new() };
        match clip.link {
            None => vec![(ti, clip)],
            Some(l) => self
                .tracks
                .iter()
                .enumerate()
                .flat_map(|(i, tr)| tr.clips.iter().filter(move |c| c.link == Some(l)).map(move |c| (i, c)))
                .collect(),
        }
    }

    /// Would `[start, start+dur)` on `track` overlap any clip not in `ignore`?
    pub fn overlaps(&self, track: usize, start: Time, dur: Time, ignore: &[ClipId]) -> bool {
        self.tracks[track]
            .clips
            .iter()
            .filter(|c| !ignore.contains(&c.id))
            .any(|c| start < c.timeline_end() && c.timeline_start < start + dur)
    }

    pub fn find_clip_mut(&mut self, id: ClipId) -> Option<(usize, &mut Clip)> {
        self.tracks.iter_mut().enumerate().find_map(|(i, tr)| {
            tr.clips.iter_mut().find(|c| c.id == id).map(|c| (i, c))
        })
    }
}

/// One clip as it should be drawn at a moment in time.
#[derive(Clone, Debug)]
pub struct Layer<'a> {
    pub track: usize,
    pub clip: &'a Clip,
    /// Where to read the media (may be outside the clip's range during a dissolve).
    pub source_time: Time,
    /// Transform with keyframes applied.
    pub transform: Transform,
    pub blend: BlendMode,
    /// Effects with keyframes applied (enabled ones that do something).
    pub effects: Vec<EffectValues>,
}

#[derive(Clone, Debug)]
pub enum VideoItem<'a> {
    Single(Layer<'a>),
    /// A transition (the kind is the incoming clip's): `mix` goes 0 → 1 from all-`from` to all-`to`.
    Dissolve { from: Layer<'a>, to: Layer<'a>, mix: f32, kind: crate::transition::Transition },
}

impl<'a> VideoItem<'a> {
    /// Layers involved, bottom first.
    pub fn layers(&self) -> Vec<&Layer<'a>> {
        match self {
            VideoItem::Single(l) => vec![l],
            VideoItem::Dissolve { from, to, .. } => vec![from, to],
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Project {
    pub media: Vec<MediaAsset>,
    pub sequences: Vec<Sequence>,
    next_id: u64,
}

/// A transition between two adjacent clips on one track (an audio crossfade or
/// a video cross-dissolve), resolved against the media actually available
/// beyond each clip's edges.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Crossfade {
    pub track: usize,
    pub kind: TrackKind,
    pub left: ClipId,
    pub right: ClipId,
    /// Where the clips touch.
    pub cut: Time,
    /// How far the outgoing clip plays on past the cut (its spare media).
    pub after: Time,
    /// How far the incoming clip starts before the cut (its spare media).
    pub before: Time,
    /// What was asked for; `after + before` can be less if there's not enough spare media.
    pub requested: Time,
}

impl Crossfade {
    pub fn start(&self) -> Time {
        self.cut - self.before
    }
    pub fn end(&self) -> Time {
        self.cut + self.after
    }
    pub fn len(&self) -> Time {
        self.after + self.before
    }
    pub fn is_empty(&self) -> bool {
        self.len() == Time::ZERO
    }
}

/// Split a requested crossfade `d` across the cut, centred if both sides have
/// enough spare media, otherwise leaning on whichever side has more.
/// `left_spare`: media after the outgoing clip's end. `right_spare`: media before
/// the incoming clip's start. Returns `(after, before)`.
pub fn crossfade_extents(d: Time, left_spare: Time, right_spare: Time) -> (Time, Time) {
    let d = d.max(Time::ZERO);
    let (ls, rs) = (left_spare.max(Time::ZERO), right_spare.max(Time::ZERO));
    let mut before = Time(d.0 / 2).min(rs);
    let after = (d - before).min(ls);
    before = (d - after).min(rs);
    (after, before)
}

impl Project {
    /// Is this a still image (no meaningful source time, unlimited length)?
    pub fn is_still(&self, id: MediaId) -> bool {
        id == MediaId::GENERATED || self.media(id).is_some_and(|m| m.info.video.is_some() && m.info.duration <= Time::ZERO)
    }

    /// How long a clip's source is: the media's duration, or the nested
    /// sequence's. (Stills have no length limit; check `is_still` first.)
    pub fn source_len(&self, c: &Clip) -> Time {
        match c.nested {
            Some(s) => self.sequences.get(s).map_or(Time::ZERO, |s| s.duration()),
            None => self.media(c.media).map(|m| m.info.duration).unwrap_or(Time::ZERO),
        }
    }

    /// The clip times `c` can show without running out of media (see
    /// [`Clip::clip_time_limits`]); unbounded for stills.
    pub fn clip_limits(&self, c: &Clip) -> (Time, Time) {
        if self.is_still(c.media) {
            let big = Time(i64::MAX / 4);
            return (-big, big);
        }
        c.clip_time_limits(self.source_len(c))
    }

    /// A video layer's native size: the media's, or the composition's frame.
    /// (Titles are drawn on a canvas the size of the frame they're in.)
    pub fn picture_size(&self, c: &Clip) -> Option<[u32; 2]> {
        match c.nested {
            Some(s) => self.sequences.get(s).map(|s| [s.width, s.height]),
            None => self.media(c.media)?.info.video.as_ref().map(|v| [v.width, v.height]),
        }
    }

    /// Does sequence `outer` show sequence `inner` (itself, or through nesting at any depth)?
    /// A composition can't be put inside itself.
    pub fn contains_sequence(&self, outer: usize, inner: usize) -> bool {
        fn walk(p: &Project, s: usize, target: usize, depth: usize) -> bool {
            if s == target {
                return true;
            }
            if depth > 32 {
                return true; // a cycle already exists; treat as containing
            }
            p.sequences.get(s).is_some_and(|seq| {
                seq.tracks.iter().flat_map(|t| &t.clips).filter_map(|c| c.nested).any(|n| walk(p, n, target, depth + 1))
            })
        }
        walk(self, outer, inner, 0)
    }

    /// Sequences that appear inside sequence `seq` (directly), in order, without repeats.
    pub fn nested_in(&self, seq: usize) -> Vec<usize> {
        let mut out: Vec<usize> = Vec::new();
        for n in self.sequences[seq].tracks.iter().flat_map(|t| &t.clips).filter_map(|c| c.nested) {
            if !out.contains(&n) {
                out.push(n);
            }
        }
        out
    }

    /// Every transition in sequence `seq`: crossfades on audio tracks and
    /// cross-dissolves on video tracks.
    pub fn transitions(&self, seq: usize) -> Vec<Crossfade> {
        let s = &self.sequences[seq];
        let mut out = Vec::new();
        for (ti, tr) in s.tracks.iter().enumerate() {
            for pair in tr.clips.windows(2) {
                let (l, r) = (&pair[0], &pair[1]);
                let wanted = match tr.kind {
                    TrackKind::Audio => r.audio.crossfade_in,
                    TrackKind::Video => r.dissolve_in,
                };
                if l.timeline_end() != r.timeline_start || wanted <= Time::ZERO {
                    continue;
                }
                // Overlap can't run past the far end of either clip.
                let requested = wanted.min(l.duration).min(r.duration);
                let left_spare = (self.clip_limits(l).1 - (l.source_in + l.duration)).min(r.duration);
                let right_spare = (r.source_in - self.clip_limits(r).0).min(l.duration);
                let (after, before) = crossfade_extents(requested, left_spare, right_spare);
                out.push(Crossfade { track: ti, kind: tr.kind, left: l.id, right: r.id, cut: r.timeline_start, after, before, requested });
            }
        }
        out
    }

    /// Audio crossfades only.
    pub fn crossfades(&self, seq: usize) -> Vec<Crossfade> {
        self.transitions(seq).into_iter().filter(|x| x.kind == TrackKind::Audio).collect()
    }

    /// Video cross-dissolves only.
    pub fn dissolves(&self, seq: usize) -> Vec<Crossfade> {
        self.transitions(seq).into_iter().filter(|x| x.kind == TrackKind::Video).collect()
    }

    /// The transition into clip `right`, if it has one.
    pub fn crossfade_into(&self, seq: usize, right: ClipId) -> Option<Crossfade> {
        self.transitions(seq).into_iter().find(|x| x.right == right)
    }

    /// What's visible at `t`, bottom track first: one layer per video track, or
    /// a dissolve between two clips where a transition is in progress.
    pub fn video_layers_at(&self, seq: usize, t: Time) -> Vec<VideoItem<'_>> {
        let s = &self.sequences[seq];
        let dissolves = self.dissolves(seq);
        fn layer<'a>(p: &Project, track: usize, c: &'a Clip, t: Time) -> Layer<'a> {
            let source_time = if p.is_still(c.media) { Time::ZERO } else { c.media_at(t) };
            // Keys are anchored to source time (also for stills, like the transform's).
            Layer { track, clip: c, source_time, transform: c.transform_at(t), blend: c.blend, effects: c.effects_at(c.source_time(t)) }
        }
        let mut out = Vec::new();
        for (ti, tr) in s.tracks.iter().enumerate() {
            if tr.kind != TrackKind::Video || tr.muted {
                continue;
            }
            if let Some(x) = dissolves.iter().find(|x| x.track == ti && !x.is_empty() && t >= x.start() && t < x.end()) {
                let (Some((_, a)), Some((_, b))) = (s.find_clip(x.left), s.find_clip(x.right)) else { continue };
                let mix = (t - x.start()).0 as f32 / x.len().0 as f32;
                out.push(VideoItem::Dissolve { from: layer(self, ti, a, t), to: layer(self, ti, b, t), mix, kind: b.transition });
            } else if let Some(c) = tr.clip_at(t) {
                let l = layer(self, ti, c, t);
                if l.transform.opacity > 0.0 {
                    out.push(VideoItem::Single(l));
                }
            }
        }
        out
    }

    /// The clip that ends exactly where `id` starts on the same track (a crossfade candidate).
    pub fn clip_before(&self, seq: usize, id: ClipId) -> Option<ClipId> {
        let s = &self.sequences[seq];
        let (ti, c) = s.find_clip(id)?;
        s.tracks[ti].clips.iter().find(|o| o.timeline_end() == c.timeline_start).map(|o| o.id)
    }

    pub fn new() -> Self {
        Self {
            media: Vec::new(),
            sequences: vec![Sequence::new("Sequence 1", 1920, 1080, FrameRate::new(30, 1))],
            next_id: 1,
        }
    }
    pub fn alloc_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }
    pub fn media(&self, id: MediaId) -> Option<&MediaAsset> {
        self.media.iter().find(|m| m.id == id)
    }
}

#[cfg(test)]
mod crossfade_tests {
    use super::*;

    fn secs(s: f64) -> Time {
        Time::from_secs_f64(s)
    }

    #[test]
    fn extents_centre_when_possible_and_lean_otherwise() {
        assert_eq!(crossfade_extents(secs(1.0), secs(5.0), secs(5.0)), (secs(0.5), secs(0.5)));
        // Outgoing clip has only 0.2 s of spare media: the incoming side covers the rest.
        assert_eq!(crossfade_extents(secs(1.0), secs(0.2), secs(5.0)), (secs(0.2), secs(0.8)));
        assert_eq!(crossfade_extents(secs(1.0), secs(5.0), secs(0.1)), (secs(0.9), secs(0.1)));
        assert_eq!(crossfade_extents(secs(1.0), Time::ZERO, Time::ZERO), (Time::ZERO, Time::ZERO));
    }

    fn media(id: u64, dur: f64) -> MediaAsset {
        MediaAsset {
            id: MediaId(id),
            path: format!("{id}.wav").into(),
            name: String::new(),
            info: MediaInfo { duration: secs(dur), video: None, audio: None, container: String::new() },
        }
    }

    #[test]
    fn project_resolves_crossfades_against_available_media() {
        let mut p = Project::new();
        p.media.push(media(1, 10.0));
        p.media.push(media(2, 10.0));
        let a1 = 2; // audio track index in a new sequence
        // A: source 0..4 (6 s spare after). B: source 0.3..5.3 (0.3 s spare before).
        let a = Clip::new(ClipId(10), MediaId(1), secs(0.0), secs(0.0), secs(4.0), None);
        let mut b = Clip::new(ClipId(11), MediaId(2), secs(4.0), secs(0.3), secs(5.0), None);
        b.audio.crossfade_in = secs(1.0);
        p.sequences[0].tracks[a1].clips = vec![a, b];
        let x = p.crossfades(0);
        assert_eq!(x.len(), 1);
        let x = x[0];
        assert_eq!((x.left, x.right, x.cut), (ClipId(10), ClipId(11), secs(4.0)));
        assert_eq!((x.before, x.after), (secs(0.3), secs(0.7)));
        assert_eq!((x.start(), x.end()), (secs(3.7), secs(4.7)));
        assert_eq!(p.clip_before(0, ClipId(11)), Some(ClipId(10)));

        // Moving B away from A turns it back into a plain clip.
        p.sequences[0].tracks[a1].clips[1].timeline_start = secs(4.5);
        assert!(p.crossfades(0).is_empty());
        assert_eq!(p.clip_before(0, ClipId(11)), None);
    }

    #[test]
    fn video_layers_dissolve_between_adjacent_clips() {
        let mut p = Project::new();
        let mut m = media(1, 10.0);
        m.info.video = Some(VideoStreamInfo { width: 64, height: 64, frame_rate: FrameRate::new(30, 1), codec: String::new(), time_base: Rational::new(1, 30) });
        p.media.push(m);
        // A: source 0–4 at 0–4. B: source 5–9 at 4–8, 1 s dissolve (both have spare media).
        let mut a = Clip::new(ClipId(10), MediaId(1), secs(0.0), secs(0.0), secs(4.0), None);
        a.anim.rotation.set(secs(0.0), 0.0);
        a.anim.rotation.set(secs(4.0), 40.0);
        let mut b = Clip::new(ClipId(11), MediaId(1), secs(4.0), secs(5.0), secs(4.0), None);
        b.dissolve_in = secs(1.0);
        p.sequences[0].tracks[0].clips = vec![a, b];

        assert_eq!(p.dissolves(0).len(), 1);
        assert!(p.crossfades(0).is_empty(), "video transitions aren't audio crossfades");

        let at = |s: f64| p.video_layers_at(0, secs(s));
        match at(2.0).as_slice() {
            [VideoItem::Single(l)] => {
                assert_eq!(l.clip.id, ClipId(10));
                assert_eq!(l.transform.rotation, 20.0, "keyframes applied");
            }
            other => panic!("{other:?}"),
        }
        match at(3.75).as_slice() {
            [VideoItem::Dissolve { from, to, mix, .. }] => {
                assert_eq!((from.clip.id, to.clip.id), (ClipId(10), ClipId(11)));
                assert!((mix - 0.25).abs() < 1e-4);
                assert_eq!(to.source_time, secs(4.75), "incoming clip reads its spare media before its in-point");
            }
            other => panic!("{other:?}"),
        }
        match at(4.25).as_slice() {
            [VideoItem::Dissolve { from, mix, .. }] => {
                assert!((mix - 0.75).abs() < 1e-4);
                assert_eq!(from.source_time, secs(4.25), "outgoing clip plays on past its out-point");
                assert_eq!(from.transform.rotation, 40.0, "holds its last keyframe");
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(at(4.6).as_slice(), [VideoItem::Single(l)] if l.clip.id == ClipId(11)));
    }
}
