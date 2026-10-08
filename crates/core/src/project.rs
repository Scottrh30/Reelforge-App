//! Project data model. Serializable, renderer-agnostic.

use crate::time::{FrameRate, Rational, Time};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct MediaId(pub u64);

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
}

impl Clip {
    pub fn timeline_end(&self) -> Time {
        self.timeline_start + self.duration
    }
    pub fn contains(&self, t: Time) -> bool {
        t >= self.timeline_start && t < self.timeline_end()
    }
    /// Map a sequence time to a time inside the source media.
    pub fn source_time(&self, t: Time) -> Time {
        self.source_in + (t - self.timeline_start)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Track {
    pub name: String,
    pub kind: TrackKind,
    /// Kept sorted by `timeline_start`, non-overlapping.
    pub clips: Vec<Clip>,
    pub muted: bool,
}

impl Track {
    pub fn new(name: impl Into<String>, kind: TrackKind) -> Self {
        Self { name: name.into(), kind, clips: Vec::new(), muted: false }
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
        }
    }
    pub fn duration(&self) -> Time {
        self.tracks.iter().map(Track::end).max().unwrap_or(Time::ZERO)
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
    pub fn find_clip_mut(&mut self, id: ClipId) -> Option<(usize, &mut Clip)> {
        self.tracks.iter_mut().enumerate().find_map(|(i, tr)| {
            tr.clips.iter_mut().find(|c| c.id == id).map(|c| (i, c))
        })
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Project {
    pub media: Vec<MediaAsset>,
    pub sequences: Vec<Sequence>,
    next_id: u64,
}

impl Project {
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
