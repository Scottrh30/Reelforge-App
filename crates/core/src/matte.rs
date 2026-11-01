//! A key's matte worked out on the processor (the same sums as the GPU
//! keyers in `render/src/effects.wgsl` and `mask.wgsl`), and its outline
//! traced into a mask shape: **Mask from key** turns what a key keeps into
//! an editable freehand mask.

use crate::effects::{key, EffectKind, EffectValues};

fn smoothstep(a: f32, b: f32, x: f32) -> f32 {
    let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

const LUMA: [f32; 3] = [0.2126, 0.7152, 0.0722];

/// How much of each pixel the key `e` keeps (0–1), for an 8-bit sRGB RGBA
/// picture `w` × `h`. A difference key compares with `plate` (8-bit RGBA,
/// any size, stretched over the picture); without one it keeps everything.
pub fn key_alpha(e: &EffectValues, rgba: &[u8], w: usize, h: usize, plate: Option<(&[u8], usize, usize)>) -> Vec<f32> {
    let Some(base) = key::matte_base(e.kind) else { return vec![1.0; w * h] };
    let v = &e.values;
    let (cb, cw) = (v[base] / 100.0, (v[base + 1] / 100.0).max(v[base] / 100.0 + 0.01));
    let clip = |keep: f32| ((keep - cb) / (cw - cb)).clamp(0.0, 1.0);
    let dir = if e.kind == EffectKind::ChromaKey {
        let k = key::chroma(key::hue_rgb(v[0]));
        let n = (k[0] * k[0] + k[1] * k[1]).sqrt().max(1e-6);
        [k[0] / n, k[1] / n]
    } else {
        [0.0, 0.0]
    };
    let mut out = Vec::with_capacity(w * h);
    for y in 0..h {
        for x in 0..w {
            let p = &rgba[(y * w + x) * 4..][..4];
            let g = [p[0] as f32 / 255.0, p[1] as f32 / 255.0, p[2] as f32 / 255.0];
            let keep = match e.kind {
                EffectKind::ChromaKey => {
                    let cc = key::chroma(g);
                    let along = cc[0] * dir[0] + cc[1] * dir[1];
                    let across = (cc[0] * dir[1] - cc[1] * dir[0]).abs();
                    let lo = v[1] / 100.0 * 0.5;
                    let hi = lo + v[2] / 100.0 * 0.5 + 1e-4;
                    1.0 - smoothstep(lo, hi, along - across)
                }
                EffectKind::LumaKey => {
                    let yv = g[0] * LUMA[0] + g[1] * LUMA[1] + g[2] * LUMA[2];
                    let (t, s) = (v[key::LUMA_THRESHOLD] / 100.0, v[key::LUMA_SOFTNESS] / 100.0 + 1e-4);
                    if v[key::LUMA_MODE] > 0.5 {
                        1.0 - smoothstep(1.0 - t - s, 1.0 - t, yv)
                    } else {
                        smoothstep(t, t + s, yv)
                    }
                }
                _ => match plate {
                    Some((pl, pw, ph)) => {
                        let (px, py) = (((x as f32 + 0.5) / w as f32 * pw as f32) as usize, ((y as f32 + 0.5) / h as f32 * ph as f32) as usize);
                        let q = &pl[(py.min(ph - 1) * pw + px.min(pw - 1)) * 4..][..3];
                        let d = (0..3).map(|i| (g[i] - q[i] as f32 / 255.0).abs()).fold(0.0, f32::max);
                        let t = v[key::DIFF_TOLERANCE] / 100.0;
                        smoothstep(t, t + v[key::DIFF_SOFTNESS] / 100.0 + 1e-4, d)
                    }
                    None => 1.0,
                },
            };
            out.push(clip(keep) * p[3] as f32 / 255.0);
        }
    }
    out
}

/// The outline of the biggest area where `alpha` is over one half, traced
/// between pixel centres (marching squares) and simplified to at most
/// `max_points` points. In the grid's pixels (0 to `w`, 0 to `h`).
pub fn largest_outline(alpha: &[f32], w: usize, h: usize, max_points: usize) -> Option<Vec<[f32; 2]>> {
    if w < 2 || h < 2 {
        return None;
    }
    // Padded with a ring of "outside" so every region's outline closes.
    let at = |x: isize, y: isize| -> f32 {
        if x < 0 || y < 0 || x >= w as isize || y >= h as isize {
            0.0
        } else {
            alpha[y as usize * w + x as usize]
        }
    };
    let inside = |x: isize, y: isize| at(x, y) > 0.5;
    // Where the edge crosses between two pixel centres (by the values).
    let cross = |a: (isize, isize), b: (isize, isize)| -> (i64, [f32; 2]) {
        let (va, vb) = (at(a.0, a.1), at(b.0, b.1));
        let t = if (vb - va).abs() > 1e-6 { ((0.5 - va) / (vb - va)).clamp(0.0, 1.0) } else { 0.5 };
        // A key for the cell edge (so segments can be joined): its two ends,
        // in either order.
        let id = |p: (isize, isize)| ((p.1 + 1) * (w as isize + 2) + (p.0 + 1)) as i64;
        let (i, j) = (id(a).min(id(b)), id(a).max(id(b)));
        let key = i * (((w + 2) * (h + 2)) as i64) + j;
        (key, [a.0 as f32 + 0.5 + (b.0 - a.0) as f32 * t, a.1 as f32 + 0.5 + (b.1 - a.1) as f32 * t])
    };
    // Segments per cell, from the four corners (x, y), (x+1, y), (x+1, y+1), (x, y+1).
    let mut next: std::collections::HashMap<i64, (i64, [f32; 2])> = std::collections::HashMap::new();
    for y in -1..h as isize {
        for x in -1..w as isize {
            let c = [(x, y), (x + 1, y), (x + 1, y + 1), (x, y + 1)];
            let bits = c.iter().enumerate().fold(0u8, |m, (i, p)| m | ((inside(p.0, p.1) as u8) << i));
            if bits == 0 || bits == 15 {
                continue;
            }
            let e = |i: usize| cross(c[i], c[(i + 1) % 4]);
            // Edges 0: top, 1: right, 2: bottom, 3: left. Segments go with the inside on the left.
            let segs: &[(usize, usize)] = match bits {
                1 => &[(3, 0)],
                2 => &[(0, 1)],
                3 => &[(3, 1)],
                4 => &[(1, 2)],
                5 => &[(3, 2), (1, 0)],
                6 => &[(0, 2)],
                7 => &[(3, 2)],
                8 => &[(2, 3)],
                9 => &[(2, 0)],
                10 => &[(0, 3), (2, 1)],
                11 => &[(2, 1)],
                12 => &[(1, 3)],
                13 => &[(1, 0)],
                14 => &[(0, 3)],
                _ => &[],
            };
            for &(a, b) in segs {
                let (ka, _) = e(a);
                let (kb, pb) = e(b);
                next.insert(ka, (kb, pb));
            }
        }
    }
    // Follow the segments round into loops; keep the biggest.
    let mut best: Option<(f32, Vec<[f32; 2]>)> = None;
    while let Some(&start) = next.keys().next() {
        let mut lp = Vec::new();
        let mut k = start;
        while let Some((nk, p)) = next.remove(&k) {
            lp.push(p);
            k = nk;
            if k == start {
                break;
            }
        }
        if lp.len() < 3 {
            continue;
        }
        let area = lp.iter().zip(lp.iter().cycle().skip(1)).map(|(a, b)| a[0] * b[1] - b[0] * a[1]).sum::<f32>() / 2.0;
        if best.as_ref().is_none_or(|b| area.abs() > b.0) {
            best = Some((area.abs(), lp));
        }
    }
    let (area, lp) = best?;
    if area < 4.0 {
        return None;
    }
    // Fewer points: drop those within a growing distance of the line through their neighbours.
    let mut tol = 0.6;
    let mut pts = simplify(&lp, tol);
    while pts.len() > max_points.max(3) {
        tol *= 1.5;
        pts = simplify(&lp, tol);
    }
    (pts.len() >= 3).then_some(pts)
}

/// Ramer–Douglas–Peucker on a closed loop (split at its two furthest-apart points).
fn simplify(lp: &[[f32; 2]], tol: f32) -> Vec<[f32; 2]> {
    fn rdp(p: &[[f32; 2]], tol: f32, out: &mut Vec<[f32; 2]>) {
        let (a, b) = (p[0], p[p.len() - 1]);
        let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
        let len = (dx * dx + dy * dy).sqrt().max(1e-6);
        let (mut far, mut at) = (0.0, 0);
        for (i, q) in p.iter().enumerate().take(p.len() - 1).skip(1) {
            let d = ((q[0] - a[0]) * dy - (q[1] - a[1]) * dx).abs() / len;
            if d > far {
                (far, at) = (d, i);
            }
        }
        if far > tol {
            rdp(&p[..=at], tol, out);
            out.pop();
            rdp(&p[at..], tol, out);
        } else {
            out.push(a);
            out.push(b);
        }
    }
    let n = lp.len();
    let i0 = 0;
    let i1 = (0..n).max_by(|&a, &b| {
        let d = |i: usize| (lp[i][0] - lp[i0][0]).powi(2) + (lp[i][1] - lp[i0][1]).powi(2);
        d(a).total_cmp(&d(b))
    }).unwrap_or(n / 2);
    let mut first = Vec::new();
    rdp(&lp[..=i1], tol, &mut first);
    let mut rest: Vec<[f32; 2]> = lp[i1..].to_vec();
    rest.push(lp[0]);
    let mut second = Vec::new();
    rdp(&rest, tol, &mut second);
    first.pop();
    second.pop();
    first.extend(second);
    first
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::Effect;

    #[test]
    fn a_green_screens_matte_traces_to_the_subjects_outline() {
        // 120 × 80 green screen with a skin-toned disc (radius 25 at 60, 40).
        let (w, h) = (120usize, 80usize);
        let mut img = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let d = ((x as f32 + 0.5 - 60.0).powi(2) + (y as f32 + 0.5 - 40.0).powi(2)).sqrt();
                img.extend(if d < 25.0 { [205, 150, 125, 255] } else { [50, 180, 80, 255] });
            }
        }
        let e = Effect::new(EffectKind::ChromaKey).eval(crate::Time::ZERO);
        let a = key_alpha(&e, &img, w, h, None);
        assert!(a[40 * w + 60] > 0.99 && a[5 * w + 5] < 0.01);
        let o = largest_outline(&a, w, h, 32).unwrap();
        assert!(o.len() >= 8 && o.len() <= 32, "{} points", o.len());
        for p in &o {
            let d = ((p[0] - 60.0).powi(2) + (p[1] - 40.0).powi(2)).sqrt();
            assert!((d - 25.0).abs() < 1.5, "{p:?} is {d} from the middle");
        }
        // A luma key, dark parts out: a bright square on black.
        let mut img = vec![0u8; w * h * 4];
        for y in 20..50 {
            for x in 30..90 {
                img[(y * w + x) * 4..][..4].copy_from_slice(&[230, 230, 230, 255]);
            }
        }
        let e = Effect::new(EffectKind::LumaKey).eval(crate::Time::ZERO);
        let o = largest_outline(&key_alpha(&e, &img, w, h, None), w, h, 16).unwrap();
        let (x0, x1) = (o.iter().map(|p| p[0]).fold(f32::MAX, f32::min), o.iter().map(|p| p[0]).fold(f32::MIN, f32::max));
        assert!((x0 - 30.0).abs() < 1.0 && (x1 - 90.0).abs() < 1.0, "{x0}–{x1}");
        assert!(o.len() <= 8, "a rectangle needs few points: {}", o.len());
        // Nothing kept: no outline.
        assert!(largest_outline(&vec![0.0; w * h], w, h, 16).is_none());
    }
}
