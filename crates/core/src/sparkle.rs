//! Sparkle trail: a magic trail behind a moving point — a glowing streak and
//! sparkles shed along it that drift, fall, twinkle and fade.
//!
//! Where the point has been is worked out here: the head now, then where it
//! was at regular moments before (`POINTS` of them over the trail's length,
//! on a fixed grid of times so each sparkle keeps its own random look as the
//! trail moves on). The head follows the effect's Head X/Y keyframes, or one
//! of the layer's masks (as far along it as Progress says). The GPU gets them
//! as a table: row 0 the points (as Lightning's: two 16-bit fractions of −½
//! to 1½ layers), row 1 whether each moment is inside the clip.

use crate::effects::{sparkle as p, Effect, EffectValues};
use crate::lut::Table;
use crate::mask::Mask;
use crate::Time;
use std::sync::Arc;

/// Moments the trail remembers.
pub const POINTS: usize = 48;

/// The point at a fraction `f` (0–1) of the way along a polyline, by length.
pub fn along(line: &[[f32; 2]], closed: bool, f: f32) -> Option<[f32; 2]> {
    let mut pts: Vec<[f32; 2]> = line.iter().copied().filter(|p| p[0].is_finite() && p[1].is_finite()).collect();
    if closed {
        if let Some(&first) = pts.first() {
            pts.push(first);
        }
    }
    let first = *pts.first()?;
    let lens: Vec<f32> = pts.windows(2).map(|w| ((w[1][0] - w[0][0]).powi(2) + (w[1][1] - w[0][1]).powi(2)).sqrt()).collect();
    let total: f32 = lens.iter().sum();
    if total <= 0.0 || !f.is_finite() {
        return Some(first);
    }
    let mut want = f.clamp(0.0, 1.0) * total;
    for (w, l) in pts.windows(2).zip(&lens) {
        if want <= *l && *l > 0.0 {
            let k = want / l;
            return Some([w[0][0] + (w[1][0] - w[0][0]) * k, w[0][1] + (w[1][1] - w[0][1]) * k]);
        }
        want -= l;
    }
    pts.last().copied()
}

/// Fill in the trail at source time `t` (s, from the clip's start: before 0 it
/// wasn't there yet), the head at a time given by `head` (layer pixels).
pub fn fill_with(v: &mut EffectValues, t: f64, size: [f32; 2], head: &dyn Fn(f64) -> Option<[f32; 2]>) {
    let trail = (v.values[p::TRAIL] as f64).clamp(0.05, 30.0);
    let dt = trail / POINTS as f64;
    if !t.is_finite() || !dt.is_finite() || dt <= 0.0 || t.abs() > 1e9 {
        v.table = None;
        return;
    }
    let base = (t / dt).floor();
    let q = |v: f32, s: f32| -> [u8; 2] {
        let f = ((v / s.max(1.0) + 0.5) / 2.0).clamp(0.0, 1.0);
        ((f * 65535.0).round() as u16).to_be_bytes()
    };
    let width = POINTS + 1;
    let mut rgba = vec![0u8; width * 2 * 4];
    let mut any = false;
    for i in 0..width {
        let when = if i == 0 { t } else { (base - (i - 1) as f64) * dt };
        let Some(pt) = head(when).filter(|p| p[0].is_finite() && p[1].is_finite()) else { continue };
        let (x, y) = (q(pt[0], size[0]), q(pt[1], size[1]));
        let o = i * 4;
        rgba[o..o + 4].copy_from_slice(&[x[0], x[1], y[0], y[1]]);
        if when >= -1e-6 {
            let o = (width + i) * 4;
            rgba[o..o + 4].copy_from_slice(&[255, 0, 0, 255]);
            any = true;
        }
    }
    let mut h = std::collections::hash_map::DefaultHasher::new();
    std::hash::Hash::hash(&rgba, &mut h);
    v.table = any.then(|| Arc::new(Table { id: std::hash::Hasher::finish(&h), width: width as u32, height: 2, rgba, cube_size: 0 }));
    v.gpu = [dt as f32, POINTS as f32, (t - base * dt) as f32, base.rem_euclid(65536.0) as f32];
}

/// The trail following the effect's Head X/Y keyframes (`t`: source time).
pub fn fill_head(e: &Effect, v: &mut EffectValues, t: Time, start: Time, size: [f32; 2]) {
    let s0 = start.as_secs_f64();
    let head = |when: f64| -> Option<[f32; 2]> {
        let at = Time::from_secs_f64(when + s0);
        Some([e.value_at(p::HEAD_X, at) / 100.0 * size[0], e.value_at(p::HEAD_Y, at) / 100.0 * size[1]])
    };
    fill_with(v, t.as_secs_f64() - s0, size, &head);
}

/// The trail along mask `PATH − 1`, as far along as Progress says at each moment.
pub fn fill_mask(e: &Effect, v: &mut EffectValues, masks: &[Mask], t: Time, start: Time, size: [f32; 2]) {
    let index = (v.values[p::PATH].round() as usize).saturating_sub(1);
    let Some(mask) = masks.get(index) else {
        v.table = None;
        return;
    };
    let s0 = start.as_secs_f64();
    let head = |when: f64| -> Option<[f32; 2]> {
        let at = Time::from_secs_f64(when + s0);
        along(&mask.outline(at), true, e.value_at(p::PROGRESS, at) / 100.0)
    };
    fill_with(v, t.as_secs_f64() - s0, size, &head);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::EffectKind;

    #[test]
    fn along_a_line_by_length() {
        let l = [[0.0, 0.0], [10.0, 0.0], [10.0, 30.0]];
        assert_eq!(along(&l, false, 0.0), Some([0.0, 0.0]));
        assert_eq!(along(&l, false, 0.25), Some([10.0, 0.0]));
        assert_eq!(along(&l, false, 1.0), Some([10.0, 30.0]));
        assert_eq!(along(&[], false, 0.5), None);
        assert_eq!(along(&[[1.0, 1.0]], false, 0.5), Some([1.0, 1.0]));
    }

    #[test]
    fn the_trail_remembers_where_the_head_was() {
        let mut e = Effect::new(EffectKind::SparkleTrail);
        e.params[p::HEAD_X].keys.set(Time::ZERO, 0.0);
        e.params[p::HEAD_X].keys.set(Time::from_secs_f64(2.0), 100.0);
        let size = [200.0, 100.0];
        let mut v = e.eval_sized(Time::from_secs_f64(1.0), None, size);
        fill_head(&e, &mut v, Time::from_secs_f64(1.0), Time::ZERO, size);
        let tb = v.table.clone().expect("a trail");
        assert_eq!((tb.width as usize, tb.height), (POINTS + 1, 2));
        let x = |i: usize| {
            let b = &tb.rgba[i * 4..i * 4 + 2];
            ((u16::from_be_bytes([b[0], b[1]]) as f32 / 65535.0) * 2.0 - 0.5) * size[0]
        };
        // The head at 1 s is halfway (100 px); a moment earlier, further left.
        assert!((x(0) - 100.0).abs() < 0.5, "{}", x(0));
        assert!(x(10) < x(0) - 10.0);
        // At 0.2 s, moments before the start aren't there.
        let mut v = e.eval_sized(Time::from_secs_f64(0.2), None, size);
        fill_head(&e, &mut v, Time::from_secs_f64(0.2), Time::ZERO, size);
        let tb = v.table.expect("a trail");
        let alive = |i: usize| tb.rgba[((POINTS + 1) + i) * 4] > 0;
        assert!(alive(0) && alive(5) && !alive(POINTS));
    }

    #[test]
    fn odd_numbers_do_no_harm() {
        let e = Effect::new(EffectKind::SparkleTrail);
        let mut v = e.eval_sized(Time::ZERO, None, [100.0, 100.0]);
        for t in [f64::NAN, f64::INFINITY, -1e12, 1e12] {
            fill_with(&mut v, t, [100.0, 100.0], &|_| Some([f32::NAN, 1.0]));
            fill_with(&mut v, t, [0.0, f32::NAN], &|w| Some([w as f32, 1e30]));
        }
        v.values[p::TRAIL] = f32::NAN;
        fill_with(&mut v, 1.0, [100.0, 100.0], &|_| Some([1.0, 1.0]));
    }
}
