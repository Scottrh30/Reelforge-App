//! Neon path: a glowing tube along one of a layer's masks. The mask's
//! outline at the moment, the part of it shown (start, end and offset, so it
//! can be written on or travel round), cut into dashes if wanted and
//! thinner towards its ends with taper, becomes segments in the same table
//! the Lightning effect draws from (see [`crate::lightning::table`]); the
//! flicker is how bright it is at the moment.
//!
//! On a title the path can instead be the letters themselves (Path: "The
//! letters"): every outline of every glyph, written on one after another,
//! with no mask to draw. The outlines come from the renderer, which has the
//! fonts (see [`set_letter_outlines`]).

use crate::effects::{neonpath as p, EffectValues};
use crate::lightning::{Bolts, Segment};
use crate::mask::Mask;
use crate::time::Time;
use crate::title::TitleSpec;
use std::sync::OnceLock;

/// The most segments drawn (the table is three texels a segment).
const MAX_SEGMENTS: usize = 2600;
/// The most letter outlines followed.
const MAX_LINES: usize = 600;

/// A title's letter outlines (closed polygons in the pixels of a `frame`-sized canvas).
pub type LetterOutlines = fn(&TitleSpec, [u32; 2]) -> Vec<Vec<[f32; 2]>>;
static LETTERS: OnceLock<LetterOutlines> = OnceLock::new();

/// Set by the renderer, which lays out the text.
pub fn set_letter_outlines(f: LetterOutlines) {
    let _ = LETTERS.set(f);
}

/// The Path setting is "The letters" (rather than a mask).
pub fn on_letters(v: &[f32]) -> bool {
    v.get(p::MASK).is_some_and(|m| m.round() < 0.5)
}

/// Fill in a Neon path effect's table and brightness from `masks` (or, on a
/// title, its letters) at source time `t`, `since` seconds after the clip's
/// start (for switching on).
pub fn fill(v: &mut EffectValues, masks: &[Mask], title: Option<&TitleSpec>, t: Time, since: f32, size: [f32; 2]) {
    let vals = &v.values;
    let segs = if on_letters(vals) {
        let lines = match (title, LETTERS.get()) {
            (Some(spec), Some(f)) if !spec.is_3d() => f(spec, [size[0].max(1.0) as u32, size[1].max(1.0) as u32]),
            _ => Vec::new(),
        };
        let lines: Vec<(Vec<[f32; 2]>, bool)> = lines.into_iter().filter(|l| l.len() >= 2).take(MAX_LINES).map(|l| (l, true)).collect();
        let on = switch_on(vals, &letters(&lines), since);
        let mut segs = segments_lit(vals, &lines, Some(&on), v.time);
        segs.retain(|s| s.bright > 1e-3);
        segs
    } else {
        let b = switch_on(vals, &[0], since).first().copied().unwrap_or(1.0);
        let mut segs = segments(vals, masks, t, v.time);
        segs.iter_mut().for_each(|s| s.bright *= b);
        segs.retain(|s| s.bright > 1e-3);
        segs
    };
    let flash = brightness(vals, v.time);
    v.table = if segs.is_empty() { None } else { crate::lightning::table(&Bolts { segments: segs.clone(), flash }, size) };
    let n = segs.len();
    // How much the layer itself is dimmed (0: as it is; 1: only the tube).
    let dim = 1.0 - (vals.get(p::SHOW_LAYER).copied().unwrap_or(100.0) / 100.0).clamp(0.0, 1.0);
    v.gpu = [flash, n as f32, n.div_ceil(crate::lightning::CHUNK) as f32, dim];
}

/// How bright the tube is at `secs` (0–4): its Brightness, dipping as it flickers.
pub fn brightness(v: &[f32], secs: f32) -> f32 {
    let base = v[p::BRIGHTNESS].max(0.0) / 100.0;
    let amount = (v[p::FLICKER] / 100.0).clamp(0.0, 1.0);
    if amount <= 0.0 {
        return base;
    }
    let seed = v[p::SEED].round() as u32;
    let x = secs * v[p::FLICKER_SPEED].max(0.01);
    // Mostly on, with sudden dips (a failing tube), and a faint hum.
    let n = smooth_noise(seed, x);
    let dip = ((n - 0.55) / 0.3).clamp(0.0, 1.0);
    let hum = smooth_noise(seed.wrapping_add(7), x * 5.0) * 0.12;
    base * (1.0 - amount * (dip * 0.9 + hum)).max(0.0)
}

fn hash(seed: u32, i: i64) -> f32 {
    let mut h = (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (seed as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
    h ^= h >> 31;
    h = h.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    h ^= h >> 29;
    (h >> 40) as f32 / (1u64 << 24) as f32
}

fn smooth_noise(seed: u32, x: f32) -> f32 {
    let i = x.floor();
    let f = x - i;
    let u = f * f * (3.0 - 2.0 * f);
    hash(seed, i as i64) * (1.0 - u) + hash(seed, i as i64 + 1) * u
}

/// The tube's segments (layer pixels) at source time `t` (`secs` for dashes on the move).
pub fn segments(v: &[f32], masks: &[Mask], t: Time, secs: f32) -> Vec<Segment> {
    if on_letters(v) {
        return Vec::new();
    }
    let index = (v[p::MASK].round() as usize).max(1) - 1;
    let Some(mask) = masks.get(index) else { return Vec::new() };
    segments_on(v, &[(mask.outline(t), v[p::CLOSED] >= 0.5)], secs)
}

/// Which letter each of a title's outlines belongs to, numbered left to
/// right: an outline inside another's bounds (the hole in an O, the inside
/// of an A) goes with it.
pub fn letters(lines: &[(Vec<[f32; 2]>, bool)]) -> Vec<usize> {
    let bounds: Vec<[f32; 4]> = lines
        .iter()
        .map(|(l, _)| l.iter().fold([f32::MAX, f32::MAX, f32::MIN, f32::MIN], |b, p| [b[0].min(p[0]), b[1].min(p[1]), b[2].max(p[0]), b[3].max(p[1])]))
        .collect();
    let area = |b: &[f32; 4]| (b[2] - b[0]).max(0.0) * (b[3] - b[1]).max(0.0);
    let inside = |a: &[f32; 4], b: &[f32; 4]| a[0] >= b[0] - 0.5 && a[1] >= b[1] - 0.5 && a[2] <= b[2] + 0.5 && a[3] <= b[3] + 0.5;
    // Each outline's owner: the biggest outline it lies inside (itself if none).
    let owner: Vec<usize> = (0..bounds.len())
        .map(|i| (0..bounds.len()).filter(|&j| j != i && area(&bounds[j]) > area(&bounds[i]) && inside(&bounds[i], &bounds[j])).max_by(|&a, &b| area(&bounds[a]).total_cmp(&area(&bounds[b]))).unwrap_or(i))
        .collect();
    let mut tops: Vec<usize> = (0..bounds.len()).filter(|&i| owner[i] == i).collect();
    tops.sort_by(|&a, &b| bounds[a][0].total_cmp(&bounds[b][0]));
    owner.iter().map(|o| tops.iter().position(|t| t == o).unwrap_or(0)).collect()
}

/// How lit each outline is (0–1) `since` seconds after the clip's start,
/// with `letter` giving each one's letter: Switch on spreads the letters'
/// moments over its length (in Order), and each comes on stuttering (a
/// failing starter, by Stutter) or with a quick fade up.
pub fn switch_on(v: &[f32], letter: &[usize], since: f32) -> Vec<f32> {
    let over = v.get(p::SWITCH_ON).copied().unwrap_or(0.0);
    if !(over > 0.0) {
        return vec![1.0; letter.len()];
    }
    let n = letter.iter().copied().max().map_or(1, |m| m + 1);
    let stutter = (v.get(p::STUTTER).copied().unwrap_or(0.0) / 100.0).clamp(0.0, 1.0);
    let order = v.get(p::ORDER).map_or(0, |o| o.round() as u32);
    let seed = v[p::SEED].round() as u32;
    // A random order: the letters' places shuffled by the seed.
    let mut place: Vec<usize> = (0..n).collect();
    if order == 1 {
        place.sort_by(|&a, &b| hash(seed ^ 0x5eed, a as i64).total_cmp(&hash(seed ^ 0x5eed, b as i64)));
    }
    let len = 0.15 + 0.5 * stutter;
    letter
        .iter()
        .map(|&l| {
            let k = if order == 2 { 0 } else { place.iter().position(|&x| x == l).unwrap_or(0) };
            let start = if order == 2 || n <= 1 { (over - len).max(0.0) } else { (over - len).max(0.0) * k as f32 / (n - 1) as f32 };
            let u = (since - start) / len;
            if u <= 0.0 {
                return 0.0;
            }
            if u >= 1.0 {
                return 1.0;
            }
            let fade = u * u * (3.0 - 2.0 * u);
            // Stuttering: flashes on and off, on more and more of the time.
            let tick = (since * 24.0).floor() as i64;
            let flash = if hash(seed.wrapping_add(l as u32 * 31 + 1), tick) < u.powf(0.6) { 1.0 } else { 0.06 };
            fade + (flash - fade) * stutter
        })
        .collect()
}

/// The tube along `lines` (each with whether it's closed), one after
/// another: Start and End are along all of them together (so a sign
/// writes on letter by letter); Offset, dashes and taper go round each.
pub fn segments_on(v: &[f32], lines: &[(Vec<[f32; 2]>, bool)], secs: f32) -> Vec<Segment> {
    segments_lit(v, lines, None, secs)
}

/// As [`segments_on`], each line's segments as bright as `lit` says.
pub fn segments_lit(v: &[f32], lines: &[(Vec<[f32; 2]>, bool)], lit: Option<&[f32]>, secs: f32) -> Vec<Segment> {
    let length = |l: &[[f32; 2]], closed: bool| -> f32 { l.windows(2).map(|w| dist(w[0], w[1])).sum::<f32>() + if closed && l.len() > 2 { l.first().zip(l.last()).map_or(0.0, |(a, b)| dist(*a, *b)) } else { 0.0 } };
    let lens: Vec<f32> = lines.iter().map(|(l, c)| length(l, *c)).collect();
    let total: f32 = lens.iter().sum();
    if !(total > 0.5) {
        return Vec::new();
    }
    let (s, e) = ((v[p::START] / 100.0).clamp(0.0, 1.0), (v[p::END] / 100.0).clamp(0.0, 1.0));
    let (s, e) = (s.min(e) * total, s.max(e) * total);
    let mut out = Vec::new();
    let mut at = 0.0f32;
    for (i, ((line, closed), len)) in lines.iter().zip(&lens).enumerate() {
        let (a, b) = (at, at + len);
        at = b;
        if *len <= 0.0 || e <= a || s >= b {
            continue;
        }
        let (ls, le) = (((s - a) / len).clamp(0.0, 1.0), ((e - a) / len).clamp(0.0, 1.0));
        let from = out.len();
        along(v, line.clone(), *closed, ls, le, secs, &mut out);
        if let Some(b) = lit.and_then(|l| l.get(i)) {
            out[from..].iter_mut().for_each(|s| s.bright *= b);
        }
        if out.len() >= MAX_SEGMENTS {
            out.truncate(MAX_SEGMENTS);
            break;
        }
    }
    out
}

/// The part `s`–`e` (fractions of its length) of one line, into `out`.
fn along(v: &[f32], mut line: Vec<[f32; 2]>, closed: bool, s: f32, e: f32, secs: f32, out: &mut Vec<Segment>) {
    if line.len() < 2 {
        return;
    }
    if closed && line.first() != line.last() {
        line.push(line[0]);
    }
    // Lengths along it.
    let mut along = vec![0.0f32];
    for w in line.windows(2) {
        along.push(along.last().copied().unwrap_or(0.0) + dist(w[0], w[1]));
    }
    let total = along.last().copied().unwrap_or(0.0);
    if total < 0.5 {
        return;
    }
    // The part shown, in lengths (it can run past the end of a closed path and round again).
    if e - s <= 1e-4 {
        return;
    }
    let off = v[p::OFFSET] / 100.0;
    let (from, to) = if closed { ((s + off) * total, (e + off) * total) } else { (((s + off).clamp(0.0, 1.0)) * total, ((e + off).clamp(0.0, 1.0)) * total) };
    if to - from <= 1e-3 {
        return;
    }
    // Dashes: pieces `dash` long every `dash + gap`, sliding along at the dash speed.
    let (dash, gap) = (v[p::DASH].max(0.0), v[p::GAP].max(0.0));
    let mut pieces: Vec<(f32, f32)> = Vec::new();
    if dash > 0.5 {
        let period = dash + gap;
        let shift = (secs * v[p::DASH_SPEED]).rem_euclid(period);
        let mut a = from + shift - period * ((shift / period).ceil() + 1.0);
        while a < to && pieces.len() < MAX_SEGMENTS {
            let (x0, x1) = (a.max(from), (a + dash).min(to));
            if x1 > x0 {
                pieces.push((x0, x1));
            }
            a += period;
        }
    } else {
        pieces.push((from, to));
    }
    let taper = (v[p::TAPER] / 100.0).clamp(0.0, 1.0);
    let span = (to - from).max(1e-3);
    let at = |d: f32| -> [f32; 2] {
        let d = if closed { d.rem_euclid(total) } else { d.clamp(0.0, total) };
        let i = along.partition_point(|&x| x <= d).clamp(1, along.len() - 1);
        let (a, b) = (line[i - 1], line[i]);
        let k = ((d - along[i - 1]) / (along[i] - along[i - 1]).max(1e-6)).clamp(0.0, 1.0);
        [a[0] + (b[0] - a[0]) * k, a[1] + (b[1] - a[1]) * k]
    };
    let width = |d: f32| {
        let u = ((d - from) / span).clamp(0.0, 1.0);
        1.0 - taper * (1.0 - (2.0 * u.min(1.0 - u)).min(1.0))
    };
    for (x0, x1) in pieces {
        // Corners of the outline inside the piece, and its two ends.
        let mut cuts = vec![x0];
        let base = if closed { (x0 / total).floor() * total } else { 0.0 };
        for lap in 0..=(((x1 - base) / total).ceil() as i32).clamp(0, 64) {
            for &a in &along {
                let d = base + lap as f32 * total + a;
                if d > x0 && d < x1 {
                    cuts.push(d);
                }
            }
        }
        cuts.push(x1);
        cuts.sort_by(f32::total_cmp);
        // Long straight runs cut finer when tapering (so the width changes smoothly).
        let step = if taper > 0.0 { (span / 40.0).max(2.0) } else { f32::MAX };
        for w in cuts.windows(2) {
            let n = (((w[1] - w[0]) / step).ceil() as usize).clamp(1, MAX_SEGMENTS);
            for k in 0..n {
                let (d0, d1) = (w[0] + (w[1] - w[0]) * k as f32 / n as f32, w[0] + (w[1] - w[0]) * (k + 1) as f32 / n as f32);
                out.push(Segment { a: at(d0), b: at(d1), width: width((d0 + d1) / 2.0), bright: 1.0 });
                if out.len() >= MAX_SEGMENTS {
                    return;
                }
            }
        }
    }
}

fn dist(a: [f32; 2], b: [f32; 2]) -> f32 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::{Effect, EffectKind};
    use crate::mask::MaskShape;

    fn values(set: &[(usize, f32)]) -> Vec<f32> {
        let mut v: Vec<f32> = EffectKind::NeonPath.params().iter().map(|s| s.default).collect();
        for &(i, x) in set {
            v[i] = x;
        }
        v
    }
    fn square() -> Vec<Mask> {
        vec![Mask { shape: MaskShape::Rectangle, points: vec![[100.0, 100.0], [300.0, 100.0], [300.0, 300.0], [100.0, 300.0]], text_path: true, ..Mask::default() }]
    }
    fn length(s: &[Segment]) -> f32 {
        s.iter().map(|g| dist(g.a, g.b)).sum()
    }

    #[test]
    fn the_tube_follows_the_mask() {
        let m = square();
        let all = segments(&values(&[]), &m, Time::ZERO, 0.0);
        assert!((length(&all) - 800.0).abs() < 0.5, "round the square: {}", length(&all));
        // Open: three sides.
        assert!((length(&segments(&values(&[(p::CLOSED, 0.0)]), &m, Time::ZERO, 0.0)) - 600.0).abs() < 0.5);
        // Written on halfway; then that half moved a quarter on (round the corner).
        let half = segments(&values(&[(p::END, 50.0)]), &m, Time::ZERO, 0.0);
        assert!((length(&half) - 400.0).abs() < 0.5);
        assert_eq!(half[0].a, [100.0, 100.0]);
        let moved = segments(&values(&[(p::END, 50.0), (p::OFFSET, 75.0)]), &m, Time::ZERO, 0.0);
        assert!((length(&moved) - 400.0).abs() < 0.5);
        assert_eq!(moved[0].a, [100.0, 300.0]);
        // Dashes 20 long every 40: about half of it lit.
        let dashed = segments(&values(&[(p::DASH, 20.0), (p::GAP, 20.0)]), &m, Time::ZERO, 0.0);
        assert!((length(&dashed) - 400.0).abs() < 25.0, "{}", length(&dashed));
        // Taper: thin ends, full middle.
        let tapered = segments(&values(&[(p::TAPER, 100.0), (p::CLOSED, 0.0)]), &m, Time::ZERO, 0.0);
        let (first, mid) = (tapered[0].width, tapered[tapered.len() / 2].width);
        assert!(first < 0.1 && mid > 0.9, "{first} {mid}");
        // No such mask, or nothing shown: nothing.
        assert!(segments(&values(&[(p::MASK, 2.0)]), &m, Time::ZERO, 0.0).is_empty());
        assert!(segments(&values(&[(p::END, 0.0)]), &m, Time::ZERO, 0.0).is_empty());
    }

    #[test]
    fn several_lines_write_on_one_after_another() {
        let a = vec![[0.0, 0.0], [100.0, 0.0], [100.0, 100.0], [0.0, 100.0]];
        let b = vec![[200.0, 0.0], [300.0, 0.0], [300.0, 100.0], [200.0, 100.0]];
        let lines = vec![(a, true), (b, true)];
        assert!((length(&segments_on(&values(&[]), &lines, 0.0)) - 800.0).abs() < 0.5);
        // Halfway: the first square only; three quarters: half the second too.
        let half = segments_on(&values(&[(p::END, 50.0)]), &lines, 0.0);
        assert!((length(&half) - 400.0).abs() < 0.5 && half.iter().all(|s| s.a[0] <= 100.0));
        assert!((length(&segments_on(&values(&[(p::END, 75.0)]), &lines, 0.0)) - 600.0).abs() < 0.5);
        // The letters setting without a title (or a renderer): nothing, and no mask is read.
        assert!(on_letters(&values(&[(p::MASK, 0.0)])));
        assert!(segments(&values(&[(p::MASK, 0.0)]), &square(), Time::ZERO, 0.0).is_empty());
    }

    /// Switching on: letters (an outline and the holes in it together) light
    /// one after another, left to right, stuttering as they come on, all lit
    /// by the end; random order is a shuffle; 0 s means always on.
    #[test]
    fn letters_switch_on_one_after_another() {
        let sq = |x: f32, s: f32| vec![[x, 0.0], [x + s, 0.0], [x + s, s], [x, s]];
        // "O" (an outline and its hole), then "I", given right to left.
        let lines = vec![(sq(200.0, 40.0), true), (sq(0.0, 60.0), true), (sq(15.0, 30.0), true)];
        assert_eq!(letters(&lines), vec![1, 0, 0]);
        let v = values(&[(p::SWITCH_ON, 2.0), (p::STUTTER, 0.0)]);
        let l = letters(&lines);
        // Fade up over 0.15 s: the first letter at 0, the second at 1.85 s.
        assert_eq!(switch_on(&v, &l, 0.0), vec![0.0, 0.0, 0.0]);
        let mid = switch_on(&v, &l, 0.5);
        assert_eq!((mid[0], mid[1], mid[2]), (0.0, 1.0, 1.0), "the O lit, hole and all; the I not yet");
        assert_eq!(switch_on(&v, &l, 2.0), vec![1.0, 1.0, 1.0]);
        assert!(switch_on(&values(&[]), &l, 0.0).iter().all(|&b| b == 1.0), "off: always lit");
        // Stuttering: on and off in between, by the end lit.
        let st = values(&[(p::SWITCH_ON, 2.0), (p::STUTTER, 100.0)]);
        let seen: Vec<f32> = (0..30).map(|i| switch_on(&st, &l, 1.35 + i as f32 / 60.0)[0]).collect();
        assert!(seen.iter().any(|&b| b > 0.9) && seen.iter().any(|&b| b < 0.1), "{seen:?}");
        assert_eq!(switch_on(&st, &l, 2.0)[0], 1.0);
        // On a mask path: the whole tube comes on at the end.
        let mut e = Effect::new(EffectKind::NeonPath);
        e.params[p::SWITCH_ON].value = 2.0;
        e.params[p::STUTTER].value = 0.0;
        let mut ev = e.eval_sized(Time::ZERO, None, [640.0, 360.0]);
        fill(&mut ev, &square(), None, Time::ZERO, 0.5, [640.0, 360.0]);
        assert!(ev.table.is_none(), "not on yet");
        fill(&mut ev, &square(), None, Time::ZERO, 2.5, [640.0, 360.0]);
        assert!(ev.table.is_some());
    }

    #[test]
    fn it_flickers_when_asked() {
        let steady: Vec<f32> = (0..50).map(|i| brightness(&values(&[]), i as f32 * 0.05)).collect();
        assert!(steady.iter().all(|&b| (b - 1.0).abs() < 1e-6));
        let flick: Vec<f32> = (0..200).map(|i| brightness(&values(&[(p::FLICKER, 100.0)]), i as f32 * 0.05)).collect();
        let lo = flick.iter().cloned().fold(9.0, f32::min);
        let on = flick.iter().filter(|&&b| b > 0.8).count();
        assert!(lo < 0.4 && on > 80, "dips to {lo}, mostly on ({on} of 200)");
    }

    #[test]
    fn a_clip_evaluates_it_with_its_masks() {
        let mut c = crate::Clip::new(crate::ClipId(1), crate::MediaId::GENERATED, Time::ZERO, Time::ZERO, Time::from_secs_f64(2.0), None);
        c.effects = vec![Effect::new(EffectKind::NeonPath)];
        let f = Time::from_secs_f64(1.0 / 30.0);
        assert!(c.effects_sized(Time::ZERO, f, [640.0, 360.0]).is_empty(), "no mask yet");
        c.masks = square();
        let v = c.effects_sized(Time::ZERO, f, [640.0, 360.0]);
        assert_eq!(v.len(), 1);
        assert!(v[0].table.is_some() && v[0].gpu[1] >= 4.0);
    }
}
