//! Tracks and masks from Trackwright, the planar tracker.
//!
//! Trackwright exports two JSON files for Reelforge:
//!
//! * `.rftrack.json` (`"format": "trackwright.reelforge.v1"`): a tracked flat
//!   surface, its four corners on every tracked frame. It becomes the clip's
//!   [`Tracker`] (a planar one), so everything a planar track made here can do
//!   works with it: corner-pin another layer onto the surface, stabilise,
//!   move a mask with it.
//! * `.rfmask.json` (`"format": "trackwright.mask.v1"`): a tracked roto shape
//!   on every tracked frame. It becomes an animated [`Mask`], keyed on every
//!   frame, with the shape's feather.
//!
//! Both are in the footage's pixels (top left = 0, 0, y down), frame `frame`
//! of a clip that starts at `source.first_frame`. They're scaled to the
//! clip's own pixels if its size differs (a proxy or a re-render), and frame
//! numbers are turned into source time at the media's frame rate.

// Reads files from anywhere: nothing in them is taken on trust.
#![cfg_attr(not(test), deny(clippy::indexing_slicing))]

use crate::mask::{Mask, MaskShape, ShapeKey};
use crate::time::{FrameRate, Time};
use crate::track::{PointTrack, TrackMode, TrackSample, Tracker};
use serde::Deserialize;

/// What a Trackwright file holds.
#[derive(Clone, Debug, PartialEq)]
pub enum Import {
    /// A planar track: the name of the layer and the tracker made from it.
    Track { name: String, tracker: Tracker, frames: usize, size_note: Option<String> },
    /// An animated mask.
    Mask { mask: Mask, frames: usize, size_note: Option<String> },
}

#[derive(Deserialize)]
struct Source {
    #[serde(default)]
    path: String,
    width: f64,
    height: f64,
    #[serde(default)]
    fps: f64,
    #[serde(default)]
    first_frame: i64,
}

#[derive(Deserialize)]
struct Corners {
    tl: [f64; 2],
    tr: [f64; 2],
    br: [f64; 2],
    bl: [f64; 2],
}

#[derive(Deserialize)]
struct TrackFrame {
    frame: i64,
    #[serde(default)]
    time: Option<f64>,
    corners: Corners,
    #[serde(default)]
    confidence: Option<Confidence>,
}

#[derive(Deserialize)]
struct Confidence {
    #[serde(default)]
    rms_px: f64,
}

#[derive(Deserialize, Default)]
struct LayerInfo {
    #[serde(default)]
    name: String,
}

#[derive(Deserialize)]
struct TrackFile {
    format: String,
    source: Source,
    #[serde(default)]
    layer: LayerInfo,
    frames: Vec<TrackFrame>,
}

#[derive(Deserialize)]
struct MaskInfo {
    #[serde(default)]
    name: String,
    #[serde(default)]
    smooth: bool,
    #[serde(default)]
    feather_px: f64,
}

#[derive(Deserialize)]
struct MaskFrame {
    frame: i64,
    #[serde(default)]
    time: Option<f64>,
    #[serde(default)]
    points: Vec<[f64; 2]>,
    #[serde(default)]
    polygon: Vec<[f64; 2]>,
}

#[derive(Deserialize)]
struct MaskFile {
    format: String,
    source: Source,
    mask: MaskInfo,
    frames: Vec<MaskFrame>,
}

#[derive(Deserialize)]
struct Probe {
    #[serde(default)]
    format: String,
}

/// Whether `text` looks like a Trackwright file (so other JSON can be tried instead).
pub fn is_trackwright(text: &str) -> bool {
    serde_json::from_str::<Probe>(text).is_ok_and(|p| p.format.starts_with("trackwright."))
}

/// How the file's pixels and frames map onto the clip's.
struct Mapping {
    sx: f64,
    sy: f64,
    first: i64,
    rate: Option<FrameRate>,
    fps: f64,
    size_note: Option<String>,
}

impl Mapping {
    fn new(src: &Source, layer: [f32; 2], rate: Option<FrameRate>) -> Result<Self, String> {
        if !(src.width > 0.0 && src.height > 0.0 && src.width.is_finite() && src.height.is_finite()) {
            return Err("its footage size is missing".into());
        }
        let (lw, lh) = (layer[0] as f64, layer[1] as f64);
        let (sx, sy) = (lw / src.width, lh / src.height);
        let size_note = ((sx - 1.0).abs() > 1e-3 || (sy - 1.0).abs() > 1e-3).then(|| {
            let aspect_off = (src.width / src.height - lw / lh).abs() > 0.01;
            format!(
                " (tracked on {}×{} footage, scaled to this clip's {}×{}{})",
                src.width, src.height, lw, lh,
                if aspect_off { "; the shapes differ, so check it lines up" } else { "" }
            )
        });
        Ok(Mapping { sx, sy, first: src.first_frame, rate, fps: src.fps, size_note })
    }

    fn point(&self, p: [f64; 2]) -> Option<[f32; 2]> {
        let q = [(p[0] * self.sx) as f32, (p[1] * self.sy) as f32];
        (q[0].is_finite() && q[1].is_finite()).then_some(q)
    }

    /// Source time of footage frame `frame`: at the media's frame rate, else
    /// the file's, else its `time` (seconds from the footage's first frame).
    fn time(&self, frame: i64, time: Option<f64>) -> Option<Time> {
        let index = frame.checked_sub(self.first)?;
        if index < 0 {
            return None;
        }
        if let Some(rate) = self.rate.filter(|r| r.fps() > 0.0) {
            return Some(Time::from_frame(index, rate));
        }
        // The file's frame rate is exact; its `time` is rounded.
        let secs = match time {
            _ if self.fps > 0.0 && self.fps.is_finite() => index as f64 / self.fps,
            Some(t) if t.is_finite() && t >= 0.0 => t,
            _ => return None,
        };
        Some(Time::from_secs_f64(secs))
    }
}

/// Read a Trackwright `.rftrack.json` or `.rfmask.json` for a clip whose
/// pictures are `layer` pixels and whose media runs at `rate` (if known).
pub fn read(text: &str, layer: [f32; 2], rate: Option<FrameRate>) -> Result<Import, String> {
    let probe: Probe = serde_json::from_str(text).map_err(|e| format!("it isn't a Trackwright file ({e})"))?;
    match probe.format.as_str() {
        "trackwright.reelforge.v1" => read_track(text, layer, rate),
        "trackwright.mask.v1" => read_mask(text, layer, rate),
        f if f.starts_with("trackwright.") => Err(format!(
            "it's a Trackwright {} export; in Trackwright choose “Reelforge track (.rftrack.json)” or “Reelforge animated mask (.rfmask.json)”",
            f.trim_start_matches("trackwright.").trim_end_matches(".v1")
        )),
        _ => Err("it isn't a Trackwright track or mask".into()),
    }
}

fn read_track(text: &str, layer: [f32; 2], rate: Option<FrameRate>) -> Result<Import, String> {
    let f: TrackFile = serde_json::from_str(text).map_err(|e| format!("the track is damaged ({e})"))?;
    debug_assert_eq!(f.format, "trackwright.reelforge.v1");
    let map = Mapping::new(&f.source, layer, rate)?;
    let mut points: Vec<PointTrack> = (0..4).map(|_| PointTrack::default()).collect();
    let mut frames: Vec<(Time, [[f32; 2]; 4], f32)> = Vec::new();
    for fr in &f.frames {
        let Some(at) = map.time(fr.frame, fr.time) else { continue };
        let c = &fr.corners;
        let (Some(a), Some(b), Some(cc), Some(d)) = (map.point(c.tl), map.point(c.tr), map.point(c.br), map.point(c.bl)) else { continue };
        // Trackwright's fit error (px) as a 0–1 match score: 0 px is a perfect 1.
        let rms = fr.confidence.as_ref().map_or(0.0, |c| c.rms_px.max(0.0)) as f32;
        let score = (1.0 / (1.0 + rms * 0.25)).clamp(crate::track::MIN_SCORE, 1.0);
        frames.push((at, [a, b, cc, d], score));
    }
    frames.sort_by_key(|f| f.0);
    frames.dedup_by_key(|f| f.0);
    let Some(first) = frames.first() else { return Err("it has no tracked frames".into()) };
    let (start, start_corners) = (first.0, first.1);
    for (i, p) in points.iter_mut().enumerate() {
        p.start = start;
        p.point = start_corners.get(i).copied().unwrap_or_default();
        p.path = frames.iter().filter_map(|(at, c, score)| Some(TrackSample { at: *at, pos: *c.get(i)?, score: *score })).collect();
    }
    let n = frames.len();
    // Trackwright's track is already clean: no extra smoothing.
    let tracker = Tracker { mode: TrackMode::Planar, points, grid: [0, 0], smooth: 0.0, matching: Default::default() };
    let name = if f.layer.name.is_empty() { file_stem(&f.source.path) } else { f.layer.name };
    Ok(Import::Track { name, tracker, frames: n, size_note: map.size_note })
}

fn read_mask(text: &str, layer: [f32; 2], rate: Option<FrameRate>) -> Result<Import, String> {
    let f: MaskFile = serde_json::from_str(text).map_err(|e| format!("the mask is damaged ({e})"))?;
    debug_assert_eq!(f.format, "trackwright.mask.v1");
    let map = Mapping::new(&f.source, layer, rate)?;
    let mut shapes: Vec<(Time, Vec<[f32; 2]>)> = Vec::new();
    for fr in &f.frames {
        let Some(at) = map.time(fr.frame, fr.time) else { continue };
        // A smooth (spline) shape comes as its outline; a straight-edged one as its corners.
        let src = if f.mask.smooth || fr.points.len() < 3 { &fr.polygon } else { &fr.points };
        let pts: Option<Vec<[f32; 2]>> = src.iter().map(|p| map.point(*p)).collect();
        match pts {
            Some(p) if p.len() >= 3 => shapes.push((at, p)),
            _ => {}
        }
    }
    shapes.sort_by_key(|s| s.0);
    shapes.dedup_by_key(|s| s.0);
    if shapes.is_empty() {
        return Err("it has no tracked frames".into());
    }
    // Keys need the same number of points to blend between; a shape whose
    // points were added or removed part way is cut evenly to the most.
    let counts: Vec<usize> = shapes.iter().map(|s| s.1.len()).collect();
    if counts.windows(2).any(|w| w.first() != w.get(1)) {
        let most = counts.iter().copied().max().unwrap_or(3);
        crate::mask::conform_outlines(&mut shapes, most);
    }
    let n = shapes.len();
    let feather = (f.mask.feather_px * (map.sx + map.sy) / 2.0) as f32;
    let first = shapes.first().map(|s| s.1.clone()).unwrap_or_default();
    let mask = Mask {
        name: if f.mask.name.is_empty() { "Trackwright mask".into() } else { f.mask.name },
        shape: MaskShape::Freehand,
        points: first,
        keys: shapes.into_iter().map(|(at, p)| ShapeKey::new(at, p)).collect(),
        feather: if feather.is_finite() { feather.max(0.0) } else { 0.0 },
        ..Mask::default()
    };
    Ok(Import::Mask { mask, frames: n, size_note: map.size_note })
}

fn file_stem(path: &str) -> String {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let stem = name.split('.').next().unwrap_or(name);
    if stem.is_empty() {
        "Trackwright track".into()
    } else {
        stem.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRACK: &str = r#"{
      "format": "trackwright.reelforge.v1",
      "generator": "Trackwright 0.1.0",
      "source": { "path": "C:/shots/plate.mp4", "width": 1920, "height": 1080, "fps": 24.0, "first_frame": 1001, "frame_count": 3 },
      "layer": { "name": "Screen", "reference_frame": 1001, "reference_corners": { "tl": [100,100], "tr": [500,100], "br": [500,400], "bl": [100,400] } },
      "coordinate_space": "pixels, origin top-left, y down",
      "frames": [
        { "frame": 1001, "time": 0.0, "corners": { "tl": [100,100], "tr": [500,100], "br": [500,400], "bl": [100,400] }, "confidence": { "inliers": 80, "rms_px": 0.1 } },
        { "frame": 1002, "time": 0.0417, "corners": { "tl": [110,102], "tr": [510,102], "br": [510,402], "bl": [110,402] }, "confidence": { "inliers": 80, "rms_px": 0.2 } },
        { "frame": 1003, "time": 0.0833, "corners": { "tl": [120,104], "tr": [520,104], "br": [520,404], "bl": [120,404] }, "confidence": { "inliers": 80, "rms_px": 0.2 } }
      ]
    }"#;

    const MASK: &str = r#"{
      "format": "trackwright.mask.v1",
      "source": { "path": "plate.mp4", "width": 1920, "height": 1080, "fps": 25.0, "first_frame": 0, "frame_count": 2 },
      "mask": { "name": "Actor", "smooth": false, "feather_px": 4.0 },
      "frames": [
        { "frame": 0, "time": 0.0, "points": [[10,10],[90,10],[90,90],[10,90]], "polygon": [[10,10],[90,10],[90,90],[10,90]] },
        { "frame": 1, "time": 0.04, "points": [[20,10],[100,10],[100,90],[20,90]], "polygon": [[20,10],[100,10],[100,90],[20,90]] }
      ]
    }"#;

    #[test]
    fn track_becomes_a_planar_tracker() {
        let rate = FrameRate::new(24, 1);
        let Import::Track { name, tracker, frames, size_note } = read(TRACK, [1920.0, 1080.0], Some(rate)).unwrap() else { panic!() };
        assert_eq!((name.as_str(), frames, size_note), ("Screen", 3, None));
        assert_eq!(tracker.mode, TrackMode::Planar);
        let f = tracker.raw_frames();
        assert_eq!(f.len(), 3);
        assert_eq!(f[0].0, Time::ZERO);
        assert_eq!(f[2].0, Time::from_frame(2, rate));
        assert_eq!(f[2].1, vec![[120.0, 104.0], [520.0, 104.0], [520.0, 404.0], [120.0, 404.0]]);
    }

    #[test]
    fn track_is_scaled_to_a_smaller_clip() {
        let Import::Track { tracker, size_note, .. } = read(TRACK, [960.0, 540.0], None).unwrap() else { panic!() };
        assert!(size_note.is_some());
        let f = tracker.raw_frames();
        assert_eq!(f[1].1[1], [255.0, 51.0]);
        // Without a media rate, the file's frame rate gives the time.
        assert_eq!(f[1].0, Time::from_secs_f64(1.0 / 24.0));
    }

    #[test]
    fn mask_becomes_keyed_freehand_mask() {
        let Import::Mask { mask, frames, .. } = read(MASK, [1920.0, 1080.0], Some(FrameRate::new(25, 1))).unwrap() else { panic!() };
        assert_eq!(frames, 2);
        assert_eq!(mask.name, "Actor");
        assert_eq!(mask.shape, MaskShape::Freehand);
        assert_eq!(mask.feather, 4.0);
        assert_eq!(mask.keys.len(), 2);
        assert_eq!(mask.keys[1].points[0], [20.0, 10.0]);
    }

    #[test]
    fn other_files_are_refused() {
        assert!(!is_trackwright("{\"cameras\": []}"));
        assert!(is_trackwright(TRACK));
        assert!(read("{\"format\": \"trackwright.planar.v1\"}", [10.0, 10.0], None).unwrap_err().contains("planar"));
        assert!(read("not json", [10.0, 10.0], None).is_err());
        let empty = r#"{"format": "trackwright.reelforge.v1", "source": {"width": 10, "height": 10, "fps": 24}, "frames": []}"#;
        assert!(read(empty, [10.0, 10.0], None).unwrap_err().contains("no tracked frames"));
    }
}
