//! Multicam: several cameras' takes of the same event, lined up in one
//! composition (a *multicam source*, [`crate::Sequence::multicam`]) with one
//! video track per camera angle (and one audio track per angle, in the same
//! order). A clip playing it (a *multicam clip*) shows one angle at a time,
//! [`Angle`]; cutting it at the playhead and choosing another angle for the
//! rest is how the edit is made (number keys, live while playing).
//!
//! The angles are lined up by their sound ([`sync_offsets`]: the loudness
//! envelopes are cross-correlated) or by where each starts.

use serde::{Deserialize, Serialize};

use crate::{Sequence, TrackKind, Transform};

/// Which angle of a multicam source a clip shows and plays.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Angle {
    /// The angle seen: its index among the source's video tracks.
    pub video: u8,
    /// The angle heard (among its audio tracks); `None`: the one seen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<u8>,
}

impl Angle {
    pub fn heard(&self) -> u8 {
        self.audio.unwrap_or(self.video)
    }
}

/// How a multicam source's angles are lined up when it's made.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MulticamSync {
    /// By their sound: the same claps and words heard in each.
    Audio,
    /// All starting together.
    Start,
}

/// The video track indices of sequence `s`'s angles, in angle order.
pub fn video_angles(s: &Sequence) -> Vec<usize> {
    s.tracks.iter().enumerate().filter(|(_, t)| t.kind == TrackKind::Video).map(|(i, _)| i).collect()
}

/// The audio track indices of sequence `s`'s angles, in angle order.
pub fn audio_angles(s: &Sequence) -> Vec<usize> {
    s.tracks.iter().enumerate().filter(|(_, t)| t.kind == TrackKind::Audio).map(|(i, _)| i).collect()
}

/// The angles' names (their video tracks').
pub fn angle_names(s: &Sequence) -> Vec<String> {
    video_angles(s).into_iter().map(|i| s.tracks[i].name.clone()).collect()
}

/// The layers of sequence `seq` at `t` as a clip showing `angle` sees them:
/// only that angle's track, or (in the grid view) every angle shrunk into
/// its cell. Without an angle, or for a sequence that isn't a multicam
/// source, all of them as usual.
pub fn layers_at(p: &crate::Project, seq: usize, t: crate::Time, angle: Option<Angle>) -> Vec<crate::project::VideoItem<'_>> {
    use crate::project::VideoItem;
    let mut items = p.video_layers_at(seq, t);
    let (Some(angle), Some(s)) = (angle, p.sequences.get(seq)) else { return items };
    if !s.multicam {
        return items;
    }
    let tracks = video_angles(s);
    let track_of = |item: &VideoItem| match item {
        VideoItem::Single(l) => l.track,
        VideoItem::Dissolve { from, .. } => from.track,
    };
    if grid() {
        let n = tracks.len().min(GRID_MAX);
        let frame = [s.width as f32, s.height as f32];
        items.retain(|i| tracks.iter().take(n).any(|t| *t == track_of(i)));
        for item in &mut items {
            let Some(cell) = tracks.iter().position(|t| *t == track_of(item)) else { continue };
            match item {
                VideoItem::Single(l) => l.transform = into_cell(&l.transform, cell, n, frame),
                VideoItem::Dissolve { from, to, .. } => {
                    from.transform = into_cell(&from.transform, cell, n, frame);
                    to.transform = into_cell(&to.transform, cell, n, frame);
                }
            }
        }
    } else {
        let seen = tracks.get(angle.video as usize).copied();
        items.retain(|i| Some(track_of(i)) == seen);
    }
    items
}

// ---- Lining up by sound -------------------------------------------------------------

/// How much louder each moment is than the one before (only rises): claps,
/// consonants and drum hits stand out, steady level doesn't.
fn onsets(env: &[f32]) -> Vec<f32> {
    let mut out = Vec::with_capacity(env.len());
    let mut prev = 0.0f32;
    for &v in env {
        out.push((v - prev).max(0.0));
        prev = v;
    }
    out
}

fn decimate(v: &[f32], k: usize) -> Vec<f32> {
    v.chunks(k).map(|c| c.iter().sum::<f32>() / c.len() as f32).collect()
}

/// Normalised correlation of `a` with `b` moved `lag` samples later
/// (`b[i]` lines up with `a[i + lag]`), over where they overlap.
fn correlation(a: &[f32], b: &[f32], lag: i64) -> f32 {
    let (a0, b0) = if lag >= 0 { (lag as usize, 0) } else { (0, (-lag) as usize) };
    if a0 >= a.len() || b0 >= b.len() {
        return 0.0;
    }
    let n = (a.len() - a0).min(b.len() - b0);
    if n < 8 {
        return 0.0;
    }
    let (mut ab, mut aa, mut bb) = (0.0f64, 0.0f64, 0.0f64);
    for i in 0..n {
        let (x, y) = (a[a0 + i] as f64, b[b0 + i] as f64);
        ab += x * y;
        aa += x * x;
        bb += y * y;
    }
    if aa <= 0.0 || bb <= 0.0 {
        return 0.0;
    }
    // Short overlaps are trusted a little less.
    let trust = (n as f64 / a.len().min(b.len()) as f64).sqrt().min(1.0);
    (ab / (aa * bb).sqrt() * (0.5 + 0.5 * trust)) as f32
}

/// The best lag of `b` against `a` within `range`, stepping by 1.
fn best_lag(a: &[f32], b: &[f32], range: std::ops::RangeInclusive<i64>) -> (i64, f32) {
    let mut best = (0, f32::MIN);
    for lag in range {
        let c = correlation(a, b, lag);
        if c > best.1 {
            best = (lag, c);
        }
    }
    best
}

/// Where each take starts relative to the first (seconds; positive: later),
/// from their loudness envelopes (`rate` values a second, e.g. a waveform's
/// peaks). A take without sound (an empty envelope) gets 0.
///
/// Coarse to fine: tenth-of-a-second onsets are compared over every lag
/// where at least a few seconds overlap, then the best is refined at the
/// envelope's own rate.
pub fn sync_offsets(envelopes: &[&[f32]], rate: u32) -> Vec<f64> {
    let rate = rate.max(1) as usize;
    let k = (rate / 10).max(1);
    let Some(reference) = envelopes.first() else { return Vec::new() };
    let fine_a = onsets(reference);
    let coarse_a = decimate(&fine_a, k);
    let mut out = vec![0.0];
    for env in &envelopes[1..] {
        if env.is_empty() || reference.is_empty() {
            out.push(0.0);
            continue;
        }
        let fine_b = onsets(env);
        let coarse_b = decimate(&fine_b, k);
        let (na, nb) = (coarse_a.len() as i64, coarse_b.len() as i64);
        let (lag, _) = best_lag(&coarse_a, &coarse_b, -(nb - 8).max(0)..=(na - 8).max(0));
        let centre = lag * k as i64;
        let (lag, _) = best_lag(&fine_a, &fine_b, centre - 2 * k as i64..=centre + 2 * k as i64);
        // b[i] lines up with a[i + lag]: b starts `lag` samples after a.
        out.push(lag as f64 / rate as f64);
    }
    out
}

// ---- The angle grid -----------------------------------------------------------------

thread_local! {
    static GRID: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Show multicam clips as a grid of all their angles (the Viewer's multicam
/// view), for rendering on this thread. Exports never set it.
pub fn set_grid(on: bool) {
    GRID.with(|g| g.set(on));
}

pub fn grid() -> bool {
    GRID.with(|g| g.get())
}

/// Columns and rows for `n` angles: 2×2 up to four, 3×3 up to nine, then 4×4.
pub fn grid_size(n: usize) -> (usize, usize) {
    let cols = match n {
        0..=1 => 1,
        2..=4 => 2,
        5..=9 => 3,
        _ => 4,
    };
    (cols, n.div_ceil(cols).max(1).min(cols))
}

/// The most angles a grid shows.
pub const GRID_MAX: usize = 16;

/// Angle `i`'s cell of the grid, in a `frame`-sized picture: its top-left
/// corner and size (pixels).
pub fn grid_cell(i: usize, n: usize, frame: [f32; 2]) -> ([f32; 2], [f32; 2]) {
    let (cols, rows) = grid_size(n);
    let size = [frame[0] / cols as f32, frame[1] / rows as f32];
    ([(i % cols) as f32 * size[0], (i / cols) as f32 * size[1]], size)
}

/// Which angle's cell `p` (frame pixels from the top left) is in.
pub fn angle_at(p: [f32; 2], n: usize, frame: [f32; 2]) -> Option<usize> {
    let (cols, rows) = grid_size(n);
    if p[0] < 0.0 || p[1] < 0.0 || p[0] >= frame[0] || p[1] >= frame[1] {
        return None;
    }
    let (c, r) = ((p[0] / frame[0] * cols as f32) as usize, (p[1] / frame[1] * rows as f32) as usize);
    let i = r * cols + c;
    (i < n.min(GRID_MAX)).then_some(i)
}

/// A layer's transform shrunk into angle `i`'s cell.
pub fn into_cell(t: &Transform, i: usize, n: usize, frame: [f32; 2]) -> Transform {
    let (cols, rows) = grid_size(n);
    let (corner, size) = grid_cell(i, n, frame);
    // Equal scale both ways keeps the picture's shape (cells are the frame's shape when cols = rows).
    let k = (1.0 / cols as f32).min(1.0 / rows as f32);
    let centre = [corner[0] + size[0] * 0.5 - frame[0] * 0.5, corner[1] + size[1] * 0.5 - frame[1] * 0.5];
    let mut out = *t;
    out.position = [t.position[0] * k + centre[0], t.position[1] * k + centre[1]];
    out.scale = [t.scale[0] * k, t.scale[1] * k];
    if let Some(pin) = out.pin.as_mut() {
        for c in pin.0.iter_mut() {
            *c = [(c[0] - frame[0] * 0.5) * k + centre[0] + frame[0] * 0.5, (c[1] - frame[1] * 0.5) * k + centre[1] + frame[1] * 0.5];
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A take of a "performance": claps at the given seconds, over a little hiss.
    fn take(len: f64, claps: &[f64], rate: u32, seed: u32) -> Vec<f32> {
        let n = (len * rate as f64) as usize;
        let mut s = seed;
        let mut v: Vec<f32> = (0..n)
            .map(|_| {
                s = s.wrapping_mul(1664525).wrapping_add(1013904223);
                0.05 + (s >> 24) as f32 / 255.0 * 0.05
            })
            .collect();
        for &c in claps {
            let i = (c * rate as f64).round() as usize;
            for (j, x) in v.iter_mut().enumerate().skip(i).take(rate as usize / 5) {
                *x = x.max(0.9 - (j - i) as f32 * 0.05);
            }
        }
        v
    }

    #[test]
    fn takes_line_up_by_their_sound() {
        let rate = 100;
        // The event: claps at 3, 5.5, 9, 14, 20, 21 s (event time).
        let event = [3.0, 5.5, 9.0, 14.0, 20.0, 21.0];
        // Camera A starts recording at event time 0, B at 2.37 s, C at 0.8 s.
        let at = |start: f64| event.iter().map(|c| c - start).filter(|c| *c >= 0.0).collect::<Vec<_>>();
        let a = take(25.0, &at(0.0), rate, 1);
        let b = take(20.0, &at(2.37), rate, 2);
        let c = take(24.0, &at(0.8), rate, 3);
        let off = sync_offsets(&[&a, &b, &c, &[]], rate);
        assert_eq!(off.len(), 4);
        assert_eq!(off[0], 0.0);
        assert!((off[1] - 2.37).abs() < 0.021, "{off:?}");
        assert!((off[2] - 0.8).abs() < 0.021, "{off:?}");
        assert_eq!(off[3], 0.0);
    }

    #[test]
    fn the_grid_fits_every_angle() {
        assert_eq!(grid_size(1), (1, 1));
        assert_eq!(grid_size(2), (2, 1));
        assert_eq!(grid_size(4), (2, 2));
        assert_eq!(grid_size(5), (3, 2));
        assert_eq!(grid_size(9), (3, 3));
        let f = [1920.0, 1080.0];
        assert_eq!(grid_cell(3, 4, f), ([960.0, 540.0], [960.0, 540.0]));
        assert_eq!(angle_at([1000.0, 100.0], 4, f), Some(1));
        assert_eq!(angle_at([100.0, 1000.0], 3, f), Some(2));
        assert_eq!(angle_at([1000.0, 1000.0], 3, f), None, "no fourth angle");
        // A full-frame layer lands centred in its cell at half size.
        let t = into_cell(&Transform::default(), 1, 4, f);
        assert_eq!(t.position, [480.0, -270.0]);
        assert_eq!(t.scale, [0.5, 0.5]);
    }
}
