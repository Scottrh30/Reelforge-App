//! The loop filter (RFC 6386 section 15): normal (macroblock and subblock edges, luma and
//! chroma) and simple (luma only).

use crate::recon::Plane;

/// Filter strengths of one macroblock.
#[derive(Clone, Copy, Debug, Default)]
pub struct Strength {
    /// Macroblock edge limit, subblock edge limit, interior limit, high edge variance threshold.
    pub mb_lim: i32,
    pub sub_lim: i32,
    pub interior: i32,
    pub hev: i32,
}

impl Strength {
    pub fn new(level: i32, sharpness: i32, key: bool) -> Self {
        let mut interior = level >> (i32::from(sharpness > 0) + i32::from(sharpness > 4));
        if sharpness > 0 {
            interior = interior.min(9 - sharpness);
        }
        let interior = interior.max(1);
        let hev = if key {
            if level >= 40 {
                2
            } else if level >= 15 {
                1
            } else {
                0
            }
        } else if level >= 40 {
            3
        } else if level >= 20 {
            2
        } else if level >= 15 {
            1
        } else {
            0
        };
        Strength { mb_lim: (level + 2) * 2 + interior, sub_lim: level * 2 + interior, interior, hev }
    }
}

#[inline]
fn c8(v: i32) -> i32 {
    v.clamp(-128, 127)
}
#[inline]
fn s(v: u8) -> i32 {
    i32::from(v) - 128
}
#[inline]
fn u(v: i32) -> u8 {
    (c8(v) + 128) as u8
}

// One position across an edge: p3 p2 p1 p0 | q0 q1 q2 q3 (indices 0..8).
const P2: usize = 1;
const P1: usize = 2;
const P0: usize = 3;
const Q0: usize = 4;
const Q1: usize = 5;
const Q2: usize = 6;

#[inline(always)]
fn simple_ok(l: &[u8; 8], lim: i32) -> bool {
    let d = |a: usize, b: usize| (i32::from(l[a]) - i32::from(l[b])).abs();
    d(P0, Q0) * 2 + d(P1, Q1) / 2 <= lim
}

#[inline(always)]
fn normal_ok(l: &[u8; 8], edge: i32, interior: i32) -> bool {
    let d = |a: usize, b: usize| (i32::from(l[a]) - i32::from(l[b])).abs();
    simple_ok(l, edge) && d(0, 1) <= interior && d(1, 2) <= interior && d(2, 3) <= interior && d(5, 4) <= interior && d(6, 5) <= interior && d(7, 6) <= interior
}

#[inline(always)]
fn hev(l: &[u8; 8], t: i32) -> bool {
    (i32::from(l[P1]) - i32::from(l[P0])).abs() > t || (i32::from(l[Q1]) - i32::from(l[Q0])).abs() > t
}

/// The common adjustment: p0 and q0 move toward each other. With `outer` the p1 - q1 term is
/// included. Returns filter1 (the value q0 moved by).
#[inline(always)]
fn common(l: &mut [u8; 8], outer: bool) -> i32 {
    let (p1, p0, q0, q1) = (s(l[P1]), s(l[P0]), s(l[Q0]), s(l[Q1]));
    let a = c8(if outer { c8(p1 - q1) } else { 0 } + 3 * (q0 - p0));
    let f1 = c8(a + 4) >> 3;
    let f2 = c8(a + 3) >> 3;
    l[Q0] = u(q0 - f1);
    l[P0] = u(p0 + f2);
    f1
}

#[inline(always)]
fn simple_edge(l: &mut [u8; 8], lim: i32) -> bool {
    if !simple_ok(l, lim) {
        return false;
    }
    common(l, true);
    true
}

#[inline(always)]
fn subblock_edge(l: &mut [u8; 8], st: &Strength) -> bool {
    if !normal_ok(l, st.sub_lim, st.interior) {
        return false;
    }
    let h = hev(l, st.hev);
    let f1 = common(l, h);
    if !h {
        let a = (f1 + 1) >> 1;
        let (p1, q1) = (s(l[P1]), s(l[Q1]));
        l[Q1] = u(q1 - a);
        l[P1] = u(p1 + a);
    }
    true
}

#[inline(always)]
fn mb_edge(l: &mut [u8; 8], st: &Strength) -> bool {
    if !normal_ok(l, st.mb_lim, st.interior) {
        return false;
    }
    if hev(l, st.hev) {
        common(l, true);
        return true;
    }
    let (p2, p1, p0, q0, q1, q2) = (s(l[P2]), s(l[P1]), s(l[P0]), s(l[Q0]), s(l[Q1]), s(l[Q2]));
    let w = c8(c8(p1 - q1) + 3 * (q0 - p0));
    let a = c8((27 * w + 63) >> 7);
    l[Q0] = u(q0 - a);
    l[P0] = u(p0 + a);
    let a = c8((18 * w + 63) >> 7);
    l[Q1] = u(q1 - a);
    l[P1] = u(p1 + a);
    let a = c8((9 * w + 63) >> 7);
    l[Q2] = u(q2 - a);
    l[P2] = u(p2 + a);
    true
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Mb,
    Sub,
    Simple(i32),
}

/// Filter `n` positions of the edge starting at (x, y): vertical edges (`vertical`) run down
/// from (x, y) with q0 at x; horizontal ones run right with q0 at y.
fn edge(p: &mut Plane, x: usize, y: usize, n: usize, vertical: bool, kind: Kind, st: &Strength) {
    let w = p.width;
    let step = if vertical { 1 } else { w };
    for i in 0..n {
        let at = if vertical { (y + i) * w + x } else { y * w + x + i };
        // Edges are never on the plane's border, so all eight samples exist; skip if not.
        let Some(lo) = at.checked_sub(4 * step) else { continue };
        if lo + 7 * step >= p.data.len() {
            continue;
        }
        let mut l = [0u8; 8];
        for (k, v) in l.iter_mut().enumerate() {
            *v = p.data[lo + k * step];
        }
        let changed = match kind {
            Kind::Mb => mb_edge(&mut l, st),
            Kind::Sub => subblock_edge(&mut l, st),
            Kind::Simple(lim) => simple_edge(&mut l, lim),
        };
        if changed {
            for k in 1..7 {
                p.data[lo + k * step] = l[k];
            }
        }
    }
}

/// Filter one macroblock's edges in the standard's order: left edge, inner vertical edges, top
/// edge, inner horizontal edges. `inner` is false for macroblocks without coefficients that
/// aren't B_PRED or SPLITMV.
pub fn filter_mb(planes: &mut [Plane; 3], mbx: usize, mby: usize, st: &Strength, simple: bool, inner: bool) {
    let (x, y) = (mbx * 16, mby * 16);
    if simple {
        let y_plane = &mut planes[0];
        if mbx > 0 {
            edge(y_plane, x, y, 16, true, Kind::Simple(st.mb_lim), st);
        }
        if inner {
            for k in [4, 8, 12] {
                edge(y_plane, x + k, y, 16, true, Kind::Simple(st.sub_lim), st);
            }
        }
        if mby > 0 {
            edge(y_plane, x, y, 16, false, Kind::Simple(st.mb_lim), st);
        }
        if inner {
            for k in [4, 8, 12] {
                edge(y_plane, x, y + k, 16, false, Kind::Simple(st.sub_lim), st);
            }
        }
        return;
    }
    let (cx, cy) = (mbx * 8, mby * 8);
    if mbx > 0 {
        edge(&mut planes[0], x, y, 16, true, Kind::Mb, st);
        edge(&mut planes[1], cx, cy, 8, true, Kind::Mb, st);
        edge(&mut planes[2], cx, cy, 8, true, Kind::Mb, st);
    }
    if inner {
        for k in [4, 8, 12] {
            edge(&mut planes[0], x + k, y, 16, true, Kind::Sub, st);
        }
        edge(&mut planes[1], cx + 4, cy, 8, true, Kind::Sub, st);
        edge(&mut planes[2], cx + 4, cy, 8, true, Kind::Sub, st);
    }
    if mby > 0 {
        edge(&mut planes[0], x, y, 16, false, Kind::Mb, st);
        edge(&mut planes[1], cx, cy, 8, false, Kind::Mb, st);
        edge(&mut planes[2], cx, cy, 8, false, Kind::Mb, st);
    }
    if inner {
        for k in [4, 8, 12] {
            edge(&mut planes[0], x, y + k, 16, false, Kind::Sub, st);
        }
        edge(&mut planes[1], cx, cy + 4, 8, false, Kind::Sub, st);
        edge(&mut planes[2], cx, cy + 4, 8, false, Kind::Sub, st);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strengths_follow_the_standard() {
        let s = Strength::new(32, 0, true);
        assert_eq!((s.mb_lim, s.sub_lim, s.interior, s.hev), (100, 96, 32, 1));
        let s = Strength::new(63, 7, false);
        assert_eq!((s.interior, s.hev), (2, 3));
        let s = Strength::new(0, 3, false);
        assert_eq!(s.interior, 1);
    }

    #[test]
    fn a_step_is_smoothed_and_flat_areas_are_left_alone() {
        let mut planes = [Plane { data: vec![100; 32 * 16], width: 32, height: 16 }, Plane::new(16, 8), Plane::new(16, 8)];
        for y in 0..16 {
            for x in 16..32 {
                planes[0].data[y * 32 + x] = 110;
            }
        }
        let st = Strength::new(40, 0, true);
        filter_mb(&mut planes, 1, 0, &st, false, true);
        let row: Vec<u8> = planes[0].data[..32].to_vec();
        assert!(row[15] > 100 && row[16] < 110, "{row:?}");
        assert!(row[..12].iter().all(|&v| v == 100));
    }
}
