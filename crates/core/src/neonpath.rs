//! Neon path: a glowing tube along one of a layer's masks. The mask's
//! outline at the moment, the part of it shown (start, end and offset, so it
//! can be written on or travel round), cut into dashes if wanted and
//! thinner towards its ends with taper, becomes segments in the same table
//! the Lightning effect draws from (see [`crate::lightning::table`]); the
//! flicker is how bright it is at the moment.

use crate::effects::{neonpath as p, EffectValues};
use crate::lightning::{Bolts, Segment};
use crate::mask::Mask;
use crate::time::Time;

/// The most segments drawn (the table is three texels a segment).
const MAX_SEGMENTS: usize = 2600;

/// Fill in a Neon path effect's table and brightness from `masks` at source time `t`.
pub fn fill(v: &mut EffectValues, masks: &[Mask], t: Time, size: [f32; 2]) {
    let vals = &v.values;
    let segs = segments(vals, masks, t, v.time);
    let flash = brightness(vals, v.time);
    v.table = if segs.is_empty() { None } else { crate::lightning::table(&Bolts { segments: segs.clone(), flash }, size) };
    let n = segs.len();
    v.gpu = [flash, n as f32, n.div_ceil(crate::lightning::CHUNK) as f32, 0.0];
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
    let index = (v[p::MASK].round() as usize).max(1) - 1;
    let Some(mask) = masks.get(index) else { return Vec::new() };
    let mut line = mask.outline(t);
    let closed = v[p::CLOSED] >= 0.5;
    if line.len() < 2 {
        return Vec::new();
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
        return Vec::new();
    }
    // The part shown, in lengths (it can run past the end of a closed path and round again).
    let (s, e) = ((v[p::START] / 100.0).clamp(0.0, 1.0), (v[p::END] / 100.0).clamp(0.0, 1.0));
    let (s, e) = (s.min(e), s.max(e));
    if e - s <= 1e-4 {
        return Vec::new();
    }
    let off = v[p::OFFSET] / 100.0;
    let (from, to) = if closed { ((s + off) * total, (e + off) * total) } else { (((s + off).clamp(0.0, 1.0)) * total, ((e + off).clamp(0.0, 1.0)) * total) };
    if to - from <= 1e-3 {
        return Vec::new();
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
    let mut out = Vec::new();
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
                    return out;
                }
            }
        }
    }
    out
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
