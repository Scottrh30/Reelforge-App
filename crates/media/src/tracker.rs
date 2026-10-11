//! Runs a point tracker (`reelforge_core::track`) over a clip's frames.

use crate::source::FrameSource;
use anyhow::{anyhow, Result};
use reelforge_core::track::{luma_of_rgba, Luma, PointTrack, Template, TrackSample, MIN_SCORE};
use reelforge_core::{MediaInfo, Time};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// How a tracking run ended.
#[derive(Clone, Debug, PartialEq)]
pub enum TrackEnd {
    /// Reached the end of the range.
    Done,
    /// The feature couldn't be found any more at this time (blocked, out of frame, blurred).
    Lost(Time),
    Cancelled,
}

/// Track `track`'s feature through `path` from source time `from` towards
/// `until` (backwards if `until` is earlier), one frame at a time. The feature
/// starts where the track has it at `from` (or its start point). Calls
/// `progress` with 0–1. Returns the samples, including the one at `from`.
pub fn track_point(
    path: &Path,
    info: &MediaInfo,
    track: &PointTrack,
    from: Time,
    until: Time,
    cancel: &AtomicBool,
    mut progress: impl FnMut(f32),
) -> Result<(Vec<TrackSample>, TrackEnd)> {
    let v = info.video.as_ref().ok_or_else(|| anyhow!("this clip has no video to track"))?;
    let step = v.frame_rate.frame_duration();
    let mut src = FrameSource::open(path, None)?;
    let wait = Duration::from_secs(20);
    let mut grab = |t: Time| -> Result<(Vec<f32>, usize, usize)> {
        let f = src.frame_at_blocking(t, wait).ok_or_else(|| anyhow!("no frame at {t}"))?;
        Ok((luma_of_rgba(&f.rgba), f.width as usize, f.height as usize))
    };

    // The feature's position is in media pixels; frames may decode at that size or
    // not (they do at full size here), so scale between the two.
    let (img, w, h) = grab(from)?;
    let (sx, sy) = (w as f32 / v.width as f32, h as f32 / v.height as f32);
    let start = track.shown_at(from);
    let mut pos = [start[0] * sx, start[1] * sy];
    let mut template = Template::new(&Luma { data: &img, width: w, height: h }, pos, track.size * sx);
    if template.is_flat() {
        return Err(anyhow!("there's no detail under the tracker to follow: put it on a corner, a mark or an edge"));
    }
    let mut out = vec![TrackSample { at: from, pos: start, score: 1.0 }];

    let backwards = until < from;
    let total = ((until - from).0.abs() / step.0.max(1)).max(1) as f32;
    let mut t = from;
    let mut n = 0.0;
    loop {
        t = if backwards { t - step } else { t + step };
        if (backwards && t < until) || (!backwards && t > until) || t < Time::ZERO {
            return Ok((out, TrackEnd::Done));
        }
        if cancel.load(Ordering::Relaxed) {
            return Ok((out, TrackEnd::Cancelled));
        }
        let (img, w, h) = grab(t)?;
        let luma = Luma { data: &img, width: w, height: h };
        let (found, score) = template.find(&luma, pos, track.search * sx);
        if score < MIN_SCORE {
            return Ok((out, TrackEnd::Lost(t)));
        }
        pos = found;
        // Follow gradual changes (turning, light) by refreshing the patch, but only
        // from good matches, so one poor frame doesn't make it wander off.
        if score > 0.9 {
            template = Template::new(&luma, pos, track.size * sx);
        }
        out.push(TrackSample { at: t, pos: [pos[0] / sx, pos[1] / sy], score });
        n += 1.0;
        progress((n / total).min(1.0));
    }
}
