//! Runs a motion tracker (`reelforge_core::track`) over a clip's frames.

use crate::source::FrameSource;
use anyhow::{anyhow, Result};
use reelforge_core::meshtrack::MeshTracker;
use reelforge_core::meshwarp::MeshShape;
use reelforge_core::planar::PlanarTracker;
use reelforge_core::track::{local_linear, motion_between, FeatureTracker, Luma, PointTrack, Pyramid, TrackMode, TrackSample, Tracker, MIN_SCORE, REFIND_SCORE};
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
    track_points_live(path, info, tracker, from, until, cancel, |p, _| progress(p))
}

/// [`track_points`], also handing over each frame's samples (one a point) as
/// they're found, so the run can be watched.
#[allow(clippy::too_many_arguments)]
pub fn track_points_live(
    path: &Path,
    info: &MediaInfo,
    tracker: &Tracker,
    from: Time,
    until: Time,
    cancel: &AtomicBool,
    mut report: impl FnMut(f32, &[TrackSample]),
) -> Result<(Vec<Vec<TrackSample>>, TrackEnd)> {
    let mut progress = |p: f32, out: &[Vec<TrackSample>]| {
        let now: Vec<TrackSample> = out.iter().filter_map(|o| o.last().copied()).collect();
        report(p, &now);
    };
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
    if tracker.mode == TrackMode::Planar {
        return track_plane(tracker, pyr, (w, h), (v.width, v.height), step, from, until, cancel, &mut grab, &mut progress);
    }
    if tracker.mode == TrackMode::Mesh {
        return track_mesh(tracker, pyr, (w, h), (v.width, v.height), step, from, until, cancel, &mut grab, &mut progress);
    }
    // The first frame, kept to compare each feature with how it looked then.
    let first = pyr.level(0).data.to_vec();
    let (sx, sy) = (w as f32 / v.width as f32, h as f32 / v.height as f32);
    let mut followers = Vec::new();
    let mut starts = Vec::new();
    let mut out = Vec::new();
    for (i, p) in tracker.points.iter().enumerate() {
        let start = p.shown_at(from);
        let f = FeatureTracker::new_matching(&pyr, [start[0] * sx, start[1] * sy], p.size * sx, p.search * sx, tracker.matching).anchored();
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
    // Frames each point has been carried along without being found (a blink,
    // a hand passing, a blurred frame), and the time it was last found.
    let mut missing = vec![0usize; followers.len()];
    let mut last_found = vec![from; followers.len()];
    loop {
        t = if backwards { t - step } else { t + step };
        if (backwards && t < until) || (!backwards && t > until) || t < Time::ZERO {
            return Ok((out, TrackEnd::Done));
        }
        if cancel.load(Ordering::Relaxed) {
            return Ok((out, TrackEnd::Cancelled));
        }
        let (pyr, _, _) = grab(t)?;
        let before: Vec<[f32; 2]> = followers.iter().map(|f| f.pos).collect();
        let moving: Vec<[f32; 2]> = followers.iter().map(|f| f.velocity()).collect();
        let steps: Vec<([f32; 2], f32)> = followers.iter_mut().map(|f| f.step(&pyr)).collect();
        let mut good: Vec<bool> = steps.iter().zip(&missing).map(|(s, &m)| s.1 >= if m > 0 { REFIND_SCORE } else { MIN_SCORE }).collect();
        // Points on one surface move together from one frame to the next. When
        // one jumped somewhere else than the others did (to a look-alike: the
        // other eye, a brow), the one whose move least fits how it was moving
        // is the one that's wrong: it's carried with the others instead.
        let d = |i: usize| [steps[i].0[0] - before[i][0], steps[i].0[1] - before[i][1]];
        let surprise = |i: usize| {
            let v = moving.get(i).copied().unwrap_or_default();
            (d(i)[0] - v[0]).hypot(d(i)[1] - v[1]) - steps[i].1 * 4.0
        };
        for i in 0..steps.len() {
            for j in i + 1..steps.len() {
                if !good[i] || !good[j] {
                    continue;
                }
                // Allowed: what turning or zooming by a few percent a frame moves it.
                let apart = (before[i][0] - before[j][0]).hypot(before[i][1] - before[j][1]);
                let off = (d(i)[0] - d(j)[0]).hypot(d(i)[1] - d(j)[1]);
                // (A point carried along while hidden may be found again anywhere
                // it was looked for round where it was carried to.)
                // (Never as far as the other point: that's where a look-alike, the
                // other eye, would be.)
                let back = |k: usize| if missing[k] > 0 { followers.get(k).map_or(0.0, |f| f.reach()).min(apart * 0.4) } else { 0.0 };
                if off > (apart * 0.06).max(8.0) + back(i).max(back(j)) {
                    let wrong = if surprise(i) > surprise(j) { i } else { j };
                    good[wrong] = false;
                }
            }
        }
        let moved: Vec<[f32; 2]> = (0..followers.len()).filter(|&i| good[i]).map(|i| [steps[i].0[0] - before[i][0], steps[i].0[1] - before[i][1]]).collect();
        let carried = (!moved.is_empty()).then(|| {
            let k = moved.len() as f32;
            [moved.iter().map(|m| m[0]).sum::<f32>() / k, moved.iter().map(|m| m[1]).sum::<f32>() / k]
        });
        let mut found = Vec::with_capacity(followers.len());
        for (i, f) in followers.iter_mut().enumerate() {
            let (pos, score) = steps[i];
            if good[i] {
                f.found();
                missing[i] = 0;
                last_found[i] = t;
                found.push(TrackSample { at: t, pos: [pos[0] / sx, pos[1] / sy], score });
                continue;
            }
            missing[i] += 1;
            // Carried by points that are still found, it can stay hidden longer.
            if missing[i] > if carried.is_some() { MAX_CARRIED } else { MAX_MISSING } {
                // Gone for good: stop where it was last found (the frames it was
                // carried through are guesses, not tracked).
                let lf = last_found[i];
                for o in &mut out {
                    o.retain(|s| if backwards { s.at >= lf } else { s.at <= lf });
                }
                return Ok((out, TrackEnd::Lost(last_found[i] + if backwards { Time(-step.0) } else { step }, i)));
            }
            // Not found this frame: carried with the points that were, or on
            // with its own last movement, and looked for again next frame.
            // (On its own, slowing down: whatever hid it, it's likelier to
            // have slowed than to have kept going at full speed.)
            let d = carried.unwrap_or_else(|| {
                let was = prev_pos(&out[i], sx, sy, before[i]);
                [(before[i][0] - was[0]) * COAST_SLOWING, (before[i][1] - was[1]) * COAST_SLOWING]
            });
            let p = [before[i][0] + d[0], before[i][1] + d[1]];
            f.coast(p, d);
            found.push(TrackSample { at: t, pos: [p[0] / sx, p[1] / sy], score });
        }
        // With several points the surface's motion is known: check each feature
        // against its original look, turned/scaled/tilted to match (see
        // `FeatureTracker::correct`), which stops errors building up.
        if followers.len() > 1 && good.iter().all(|g| *g) {
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
        progress((n / total).min(1.0), &out);
    }
}

/// The most frames in a row a point may go unfound (a blink, a hand or a
/// blurred moment passing over it, a squint) and be picked up again: a second.
const MAX_MISSING: usize = 25;
/// The same for a point carried along by others that are still found (an eye
/// squinting while the other stays open): five seconds.
const MAX_CARRIED: usize = 125;

/// How much of its speed a point carried along on its own keeps each frame.
const COAST_SLOWING: f32 = 0.8;

/// Where point `path` was the frame before its latest sample (frame pixels of
/// the tracker, `now` if there's none), for carrying it on at the same speed.
fn prev_pos(path: &[TrackSample], sx: f32, sy: f32, now: [f32; 2]) -> [f32; 2] {
    match path.len().checked_sub(2).and_then(|i| path.get(i)) {
        Some(s) => [s.pos[0] * sx, s.pos[1] * sy],
        None => now,
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

/// Planar tracking (see [`reelforge_core::planar`]): the four points are the
/// surface's corners, moved together by the surface's tracked perspective.
#[allow(clippy::too_many_arguments)]
fn track_plane(
    tracker: &Tracker,
    pyr: Pyramid,
    (w, h): (usize, usize),
    (mw, mh): (u32, u32),
    step: Time,
    from: Time,
    until: Time,
    cancel: &AtomicBool,
    grab: &mut impl FnMut(Time) -> Result<(Pyramid, usize, usize)>,
    progress: &mut impl FnMut(f32, &[Vec<TrackSample>]),
) -> Result<(Vec<Vec<TrackSample>>, TrackEnd)> {
    let start: Vec<[f32; 2]> = tracker.points.iter().map(|p| p.shown_at(from)).collect();
    let quad: [[f32; 2]; 4] = std::array::from_fn(|i| start[i]);
    let no_detail = "there's too little detail inside the shape to follow: draw it around a surface with some texture or print on it";
    follow_region(quad, quad.to_vec(), tracker.points[0].search, no_detail, pyr, (w, h), (mw, mh), step, from, until, cancel, grab, progress)
}

/// Follow what's inside `region` (media pixels; `quad` bounds it) as a flat
/// surface: `quad`'s corners as they move. Fails with `no_detail` when
/// there's too little to follow.
#[allow(clippy::too_many_arguments)]
fn follow_region(
    quad: [[f32; 2]; 4],
    region: Vec<[f32; 2]>,
    search: f32,
    no_detail: &str,
    pyr: Pyramid,
    (w, h): (usize, usize),
    (mw, mh): (u32, u32),
    step: Time,
    from: Time,
    until: Time,
    cancel: &AtomicBool,
    grab: &mut impl FnMut(Time) -> Result<(Pyramid, usize, usize)>,
    progress: &mut impl FnMut(f32, &[Vec<TrackSample>]),
) -> Result<(Vec<Vec<TrackSample>>, TrackEnd)> {
    let (sx, sy) = (w as f32 / mw as f32, h as f32 / mh as f32);
    let start = quad;
    let scaled = |p: [f32; 2]| [p[0] * sx, p[1] * sy];
    let mut plane = PlanarTracker::in_region(&pyr, quad.map(scaled), region.into_iter().map(scaled).collect(), search * sx).map_err(|_| anyhow!("{no_detail}"))?;
    let mut out: Vec<Vec<TrackSample>> = start.iter().map(|&p| vec![TrackSample { at: from, pos: p, score: 1.0 }]).collect();
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
        let Ok(corners) = plane.step(&pyr) else {
            return Ok((out, TrackEnd::Lost(t, SURFACE)));
        };
        let score = plane.agreement.clamp(0.0, 1.0);
        for (o, c) in out.iter_mut().zip(corners) {
            o.push(TrackSample { at: t, pos: [c[0] / sx, c[1] / sy], score });
        }
        n += 1.0;
        progress((n / total).min(1.0), &out);
    }
}

/// Rotoscoping: follow what's inside a mask's `outline` (media pixels, as
/// it is at media time `from`; `quad` is its bounds: top left, top right,
/// bottom right, bottom left) towards `until`, as a flat surface; or, with
/// `grid` (a mesh over it, see [`reelforge_core::track::mask_grid`]), as one
/// that bends. Returns `quad`'s four corners (or the grid's points) at every
/// frame, for [`reelforge_core::track::mask_from_region`].
#[allow(clippy::too_many_arguments)]
pub fn track_outline(
    path: &Path,
    info: &MediaInfo,
    quad: [[f32; 2]; 4],
    grid: Option<MeshShape>,
    outline: Vec<[f32; 2]>,
    search: f32,
    from: Time,
    until: Time,
    cancel: &AtomicBool,
    mut progress: impl FnMut(f32),
) -> Result<(Vec<Vec<TrackSample>>, TrackEnd)> {
    track_outline_live(path, info, quad, grid, outline, search, from, until, cancel, |p, _| progress(p))
}

/// [`track_outline`], also handing over each frame's corners (or grid
/// points) as they're found, so the run can be watched.
#[allow(clippy::too_many_arguments)]
pub fn track_outline_live(
    path: &Path,
    info: &MediaInfo,
    quad: [[f32; 2]; 4],
    grid: Option<MeshShape>,
    outline: Vec<[f32; 2]>,
    search: f32,
    from: Time,
    until: Time,
    cancel: &AtomicBool,
    mut report: impl FnMut(f32, &[TrackSample]),
) -> Result<(Vec<Vec<TrackSample>>, TrackEnd)> {
    let mut progress = |p: f32, out: &[Vec<TrackSample>]| {
        let now: Vec<TrackSample> = out.iter().filter_map(|o| o.last().copied()).collect();
        report(p, &now);
    };
    let v = info.video.as_ref().ok_or_else(|| anyhow!("this clip has no video to track"))?;
    let step = v.frame_rate.frame_duration();
    let mut src = FrameSource::open(path, None)?;
    let wait = Duration::from_secs(20);
    let levels = Pyramid::levels_for(search);
    let mut grab = |t: Time| -> Result<(Pyramid, usize, usize)> {
        let f = src.frame_at_blocking(t, wait).ok_or_else(|| anyhow!("no frame at {t}"))?;
        let (w, h) = (f.width as usize, f.height as usize);
        Ok((Pyramid::new(f.luma(), w, h, levels), w, h))
    };
    let (pyr, w, h) = grab(from)?;
    let no_detail = "there's too little detail inside the mask to follow: draw it around something with some texture or print on it";
    match grid {
        Some(g) => follow_grid(g, Some(outline), search, no_detail, pyr, (w, h), (v.width, v.height), step, from, until, cancel, &mut grab, &mut progress),
        None => follow_region(quad, outline, search, no_detail, pyr, (w, h), (v.width, v.height), step, from, until, cancel, &mut grab, &mut progress),
    }
}

/// In [`TrackEnd::Lost`]: the whole surface was lost (planar tracking), not one point.
pub const SURFACE: usize = usize::MAX;

/// Mesh tracking (see [`reelforge_core::meshtrack`]): the points are the grid's,
/// row by row, each following the surface under it.
#[allow(clippy::too_many_arguments)]
fn track_mesh(
    tracker: &Tracker,
    pyr: Pyramid,
    (w, h): (usize, usize),
    (mw, mh): (u32, u32),
    step: Time,
    from: Time,
    until: Time,
    cancel: &AtomicBool,
    grab: &mut impl FnMut(Time) -> Result<(Pyramid, usize, usize)>,
    progress: &mut impl FnMut(f32, &[Vec<TrackSample>]),
) -> Result<(Vec<Vec<TrackSample>>, TrackEnd)> {
    let start: Vec<[f32; 2]> = tracker.points.iter().map(|p| p.shown_at(from)).collect();
    let grid = MeshShape { cols: tracker.grid[0], rows: tracker.grid[1], points: start };
    let no_detail = "there's too little detail inside the grid to follow: put it over a surface with some texture or print on it";
    follow_grid(grid, None, tracker.points[0].search, no_detail, pyr, (w, h), (mw, mh), step, from, until, cancel, grab, progress)
}

/// Follow a bending surface with grid `grid` (media pixels), using only
/// what's inside `region` (media pixels) if given: the grid's points as they
/// move. Fails with `no_detail` when there's too little to follow.
#[allow(clippy::too_many_arguments)]
fn follow_grid(
    grid: MeshShape,
    region: Option<Vec<[f32; 2]>>,
    search: f32,
    no_detail: &str,
    pyr: Pyramid,
    (w, h): (usize, usize),
    (mw, mh): (u32, u32),
    step: Time,
    from: Time,
    until: Time,
    cancel: &AtomicBool,
    grab: &mut impl FnMut(Time) -> Result<(Pyramid, usize, usize)>,
    progress: &mut impl FnMut(f32, &[Vec<TrackSample>]),
) -> Result<(Vec<Vec<TrackSample>>, TrackEnd)> {
    let (sx, sy) = (w as f32 / mw as f32, h as f32 / mh as f32);
    let scaled = |p: &[f32; 2]| [p[0] * sx, p[1] * sy];
    let start = grid.points.clone();
    let grid = MeshShape { points: grid.points.iter().map(scaled).collect(), ..grid };
    if !grid.is_valid() || grid.is_folded() {
        return Err(anyhow!("the grid is folded over: drag its corners back so it isn't twisted"));
    }
    let region = region.map(|r| r.iter().map(scaled).collect());
    let mut mesh = MeshTracker::in_region(&pyr, grid, region, search * sx).map_err(|_| anyhow!("{no_detail}"))?;
    let mut out: Vec<Vec<TrackSample>> = start.iter().map(|&p| vec![TrackSample { at: from, pos: p, score: 1.0 }]).collect();
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
        let Ok(g) = mesh.step(&pyr) else {
            return Ok((out, TrackEnd::Lost(t, SURFACE)));
        };
        let score = mesh.agreement.clamp(0.0, 1.0);
        for (o, p) in out.iter_mut().zip(&g.points) {
            o.push(TrackSample { at: t, pos: [p[0] / sx, p[1] / sy], score });
        }
        n += 1.0;
        progress((n / total).min(1.0), &out);
    }
}

/// Follow features through `path` from media time `from` to `until` (later),
/// for 3D camera solving (see [`reelforge_core::camtrack`]). Returns the
/// tracks (media pixels) and each frame's media time. Large frames are
/// followed at half size (positions are given back at full size).
pub fn track_scene(
    path: &Path,
    info: &MediaInfo,
    from: Time,
    until: Time,
    cancel: &AtomicBool,
    mut progress: impl FnMut(f32),
) -> Result<Option<(Vec<reelforge_core::camtrack::Track2d>, Vec<Time>)>> {
    let v = info.video.as_ref().ok_or_else(|| anyhow!("this clip has no video to track"))?;
    let step = v.frame_rate.frame_duration();
    let mut src = FrameSource::open(path, None)?;
    let wait = Duration::from_secs(20);
    let total = ((until - from).0 / step.0.max(1)).max(1) as f32 + 1.0;
    let mut follower: Option<(reelforge_core::camtrack::SceneFollower, usize, usize, usize)> = None;
    let mut times = Vec::new();
    let mut t = from;
    while t <= until {
        if cancel.load(Ordering::Relaxed) {
            return Ok(None);
        }
        let f = src.frame_at_blocking(t, wait).ok_or_else(|| anyhow!("no frame at {t}"))?;
        let (w, h) = (f.width as usize, f.height as usize);
        let half = if w > 1400 { 2 } else { 1 };
        let luma = f.luma();
        let (fw, fh) = (w / half, h / half);
        let data = if half == 1 {
            luma
        } else {
            let mut d = vec![0.0f32; fw * fh];
            for y in 0..fh {
                for x in 0..fw {
                    let i = 2 * y * w + 2 * x;
                    d[y * fw + x] = (luma[i] + luma[i + 1] + luma[i + w] + luma[i + w + 1]) * 0.25;
                }
            }
            d
        };
        // Working pixels → media pixels.
        let k = half as f32 * v.width as f32 / w as f32;
        let (fol, ..) = follower.get_or_insert_with(|| (reelforge_core::camtrack::SceneFollower::new(fw, fh, k), fw, fh, half));
        let pyr = Pyramid::new(data, fw, fh, fol.levels());
        fol.add_frame(&pyr);
        times.push(t);
        progress((times.len() as f32 / total).min(1.0));
        t = t + step;
    }
    let (fol, ..) = follower.ok_or_else(|| anyhow!("no frames to track"))?;
    Ok(Some((fol.tracks(), times)))
}

/// Each frame's average colour from media time `from` to `until` (later), as
/// linear light 0–1 (red, green, blue), and its media time; for deflickering.
/// Every few pixels are sampled. `None`: stopped.
pub fn frame_means(path: &Path, info: &MediaInfo, from: Time, until: Time, cancel: &AtomicBool, mut progress: impl FnMut(f32)) -> Result<Option<(Vec<[f32; 3]>, Vec<Time>)>> {
    let v = info.video.as_ref().ok_or_else(|| anyhow!("this clip has no video to deflicker"))?;
    let step = v.frame_rate.frame_duration();
    if step.0 <= 0 {
        return Err(anyhow!("this clip's frame rate is unknown"));
    }
    let mut src = FrameSource::open(path, None)?;
    let wait = Duration::from_secs(20);
    let total = ((until - from).0 / step.0).max(1) as f32 + 1.0;
    // sRGB code → linear light.
    let lin: Vec<f32> = (0..256).map(|i| {
        let c = i as f32 / 255.0;
        if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    }).collect();
    let (mut means, mut times) = (Vec::new(), Vec::new());
    let mut t = from;
    while t <= until {
        if cancel.load(Ordering::Relaxed) {
            return Ok(None);
        }
        let f = src.frame_at_blocking(t, wait).ok_or_else(|| anyhow!("no frame at {t}"))?;
        let rgba = f.rgba();
        let px = rgba.len() / 4;
        // About 40 000 samples a frame.
        let every = (px / 40_000).max(1);
        let (mut sum, mut n) = ([0.0f64; 3], 0u64);
        for p in rgba.chunks_exact(4).step_by(every) {
            for (s, &c) in sum.iter_mut().zip(p) {
                *s += lin.get(c as usize).copied().unwrap_or(0.0) as f64;
            }
            n += 1;
        }
        let k = 1.0 / n.max(1) as f64;
        means.push(sum.map(|s| (s * k) as f32));
        times.push(t);
        progress((times.len() as f32 / total).min(1.0));
        t = t + step;
    }
    Ok(Some((means, times)))
}
