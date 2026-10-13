//! Runs a motion tracker (`reelforge_core::track`) over a clip's frames.

use crate::source::FrameSource;
use anyhow::{anyhow, Result};
use reelforge_core::track::{local_linear, motion_between, FeatureTracker, Luma, PointTrack, Pyramid, TrackSample, Tracker, MIN_SCORE};
use reelforge_core::{MediaInfo, Time};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// How a tracking run ended.
#[derive(Clone, Debug, PartialEq)]
pub enum TrackEnd {
    /// Reached the end of the range.
    Done,
    /// Point `.1` (0-based) couldn't be found any more at this time (blocked,
    /// out of frame, blurred). Every point stops there.
    Lost(Time, usize),
    Cancelled,
}

/// Track every point of `tracker` through `path` from source time `from`
/// towards `until` (backwards if `until` is earlier), one frame at a time,
/// decoding each frame once. Each point starts where the tracker has it at
/// `from`. Calls `progress` with 0–1. Returns each point's samples (in point
/// order, each including the one at `from`); all have the same frames.
pub fn track_points(
    path: &Path,
    info: &MediaInfo,
    tracker: &Tracker,
    from: Time,
    until: Time,
    cancel: &AtomicBool,
    mut progress: impl FnMut(f32),
) -> Result<(Vec<Vec<TrackSample>>, TrackEnd)> {
    let v = info.video.as_ref().ok_or_else(|| anyhow!("this clip has no video to track"))?;
    let step = v.frame_rate.frame_duration();
    let mut src = FrameSource::open(path, None)?;
    let wait = Duration::from_secs(20);
    let levels = tracker.points.iter().map(|p| Pyramid::levels_for(p.search)).max().unwrap_or(0);
    let mut grab = |t: Time| -> Result<(Pyramid, usize, usize)> {
        let f = src.frame_at_blocking(t, wait).ok_or_else(|| anyhow!("no frame at {t}"))?;
        let (w, h) = (f.width as usize, f.height as usize);
        Ok((Pyramid::new(f.luma(), w, h, levels), w, h))
    };

    // Points are in media pixels; frames may decode at that size or not (they
    // do at full size here), so scale between the two.
    let (pyr, w, h) = grab(from)?;
    // The first frame, kept to compare each feature with how it looked then.
    let first = pyr.level(0).data.to_vec();
    let (sx, sy) = (w as f32 / v.width as f32, h as f32 / v.height as f32);
    let mut followers = Vec::new();
    let mut starts = Vec::new();
    let mut out = Vec::new();
    for (i, p) in tracker.points.iter().enumerate() {
        let start = p.shown_at(from);
        let f = FeatureTracker::new(&pyr, [start[0] * sx, start[1] * sy], p.size * sx, p.search * sx);
        if f.is_flat() {
            let which = if tracker.points.len() > 1 { format!("point {} has", i + 1) } else { "there's".into() };
            return Err(anyhow!("{which} no detail under it to follow: put it on a corner, a mark or an edge"));
        }
        starts.push(f.pos);
        followers.push(f);
        out.push(vec![TrackSample { at: from, pos: start, score: 1.0 }]);
    }

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
        let (pyr, _, _) = grab(t)?;
        let mut found = Vec::with_capacity(followers.len());
        for (i, f) in followers.iter_mut().enumerate() {
            let (pos, score) = f.step(&pyr);
            if score < MIN_SCORE {
                return Ok((out, TrackEnd::Lost(t, i)));
            }
            found.push(TrackSample { at: t, pos: [pos[0] / sx, pos[1] / sy], score });
        }
        // With several points the surface's motion is known: check each feature
        // against its original look, turned/scaled/tilted to match (see
        // `FeatureTracker::correct`), which stops errors building up.
        if followers.len() > 1 {
            let now: Vec<[f32; 2]> = followers.iter().map(|f| f.pos).collect();
            if let Some(hm) = motion_between(tracker.mode, &starts, &now) {
                let start = Luma { data: &first, width: w, height: h };
                for (i, f) in followers.iter_mut().enumerate() {
                    if let Some(score) = f.correct(&start, starts[i], local_linear(&hm, starts[i]), &pyr) {
                        found[i].pos = [f.pos[0] / sx, f.pos[1] / sy];
                        found[i].score = found[i].score.max(score);
                    }
                }
            }
        }
        for (o, s) in out.iter_mut().zip(found) {
            o.push(s);
        }
        n += 1.0;
        progress((n / total).min(1.0));
    }
}

/// Track a single point (see [`track_points`]).
pub fn track_point(
    path: &Path,
    info: &MediaInfo,
    track: &PointTrack,
    from: Time,
    until: Time,
    cancel: &AtomicBool,
    progress: impl FnMut(f32),
) -> Result<(Vec<TrackSample>, TrackEnd)> {
    let (mut v, end) = track_points(path, info, &Tracker::from(track.clone()), from, until, cancel, progress)?;
    Ok((v.pop().unwrap_or_default(), end))
}
