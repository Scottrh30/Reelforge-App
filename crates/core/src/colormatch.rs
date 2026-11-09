//! Colour match: measure the look of a reference (a picture, or frames of a
//! clip) and of a clip, and make the grade that turns the one into the
//! other, as a 3D LUT.
//!
//! Done in CIE L*a*b* (D65) on display values. Brightness and contrast:
//! the clip's lightness percentiles are mapped onto the reference's (a
//! smooth, rising tone curve, kept within sensible slopes). Colour: the
//! colour (a*, b*) of the shadows, midtones and highlights is each moved
//! to the reference's, and spread as widely (so a teal-and-orange or a
//! faded look comes across, not just a single tint).

use std::fmt::Write;

/// Display R′G′B′ (0–1, sRGB) to L*a*b*.
pub fn to_lab(c: [f32; 3]) -> [f32; 3] {
    let lin = |v: f32| if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) };
    let (r, g, b) = (lin(c[0]), lin(c[1]), lin(c[2]));
    let x = (0.4124 * r + 0.3576 * g + 0.1805 * b) / 0.95047;
    let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
    let z = (0.0193 * r + 0.1192 * g + 0.9505 * b) / 1.08883;
    let f = |t: f32| if t > 0.008856 { t.cbrt() } else { 7.787 * t + 16.0 / 116.0 };
    let (fx, fy, fz) = (f(x), f(y), f(z));
    [116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz)]
}

/// L*a*b* back to display R′G′B′ (not clamped).
pub fn from_lab(l: [f32; 3]) -> [f32; 3] {
    let fy = (l[0] + 16.0) / 116.0;
    let (fx, fz) = (fy + l[1] / 500.0, fy - l[2] / 200.0);
    let inv = |t: f32| if t > 0.206_893 { t * t * t } else { (t - 16.0 / 116.0) / 7.787 };
    let (x, y, z) = (inv(fx) * 0.95047, inv(fy), inv(fz) * 1.08883);
    let r = 3.2406 * x - 1.5372 * y - 0.4986 * z;
    let g = -0.9689 * x + 1.8758 * y + 0.0415 * z;
    let b = 0.0557 * x - 0.2040 * y + 1.0570 * z;
    let enc = |v: f32| if v <= 0.003_130_8 { 12.92 * v } else { 1.055 * v.max(0.0).powf(1.0 / 2.4) - 0.055 };
    [enc(r), enc(g), enc(b)]
}

/// Where the shadows, midtones and highlights are centred (L*).
const BANDS: [f32; 3] = [15.0, 50.0, 85.0];

/// How much each band a lightness belongs to (sums to 1).
fn band_weights(l: f32) -> [f32; 3] {
    if l <= BANDS[0] {
        [1.0, 0.0, 0.0]
    } else if l <= BANDS[1] {
        let u = (l - BANDS[0]) / (BANDS[1] - BANDS[0]);
        [1.0 - u, u, 0.0]
    } else if l <= BANDS[2] {
        let u = (l - BANDS[1]) / (BANDS[2] - BANDS[1]);
        [0.0, 1.0 - u, u]
    } else {
        [0.0, 0.0, 1.0]
    }
}

/// The measured look of a picture or a set of frames.
#[derive(Clone, Debug, Default)]
pub struct ColorStats {
    /// How many samples at each lightness (L* 0–100 in 256 steps).
    hist: Vec<f64>,
    /// Per band: total weight, Σa, Σb, Σa², Σb².
    bands: [[f64; 5]; 3],
    pub samples: usize,
}

/// A band's colour: mean a*, mean b*, and how widely it spreads (the RMS
/// distance from that mean), with how much went into it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BandColor {
    pub a: f32,
    pub b: f32,
    pub spread: f32,
    pub weight: f32,
}

impl ColorStats {
    pub fn new() -> Self {
        ColorStats { hist: vec![0.0; 256], ..Default::default() }
    }

    /// Add a picture (RGBA, straight alpha): about 40 000 samples at most,
    /// leaving out see-through pixels.
    pub fn add_rgba(&mut self, rgba: &[u8], w: usize, h: usize) {
        if w == 0 || h == 0 || rgba.len() < w * h * 4 {
            return;
        }
        let step = ((w * h) as f64 / 40_000.0).sqrt().ceil().max(1.0) as usize;
        for y in (0..h).step_by(step) {
            for x in (0..w).step_by(step) {
                let p = &rgba[(y * w + x) * 4..(y * w + x) * 4 + 4];
                if p[3] < 128 {
                    continue;
                }
                self.add([p[0] as f32 / 255.0, p[1] as f32 / 255.0, p[2] as f32 / 255.0]);
            }
        }
    }

    /// Add one display colour.
    pub fn add(&mut self, c: [f32; 3]) {
        if self.hist.len() != 256 {
            self.hist = vec![0.0; 256];
        }
        let lab = to_lab(c);
        self.hist[((lab[0] / 100.0).clamp(0.0, 1.0) * 255.0).round() as usize] += 1.0;
        for (k, w) in band_weights(lab[0]).into_iter().enumerate() {
            if w > 0.0 {
                let (a, b, w) = (lab[1] as f64, lab[2] as f64, w as f64);
                let s = &mut self.bands[k];
                s[0] += w;
                s[1] += w * a;
                s[2] += w * b;
                s[3] += w * a * a;
                s[4] += w * b * b;
            }
        }
        self.samples += 1;
    }

    /// Lightness at percentile `p` (0–100).
    pub fn percentile(&self, p: f32) -> f32 {
        let total: f64 = self.hist.iter().sum();
        if total <= 0.0 {
            return p;
        }
        let want = total * (p as f64 / 100.0).clamp(0.0, 1.0);
        let mut run = 0.0;
        for (i, &n) in self.hist.iter().enumerate() {
            if run + n >= want && n > 0.0 {
                // Within the bin, in proportion.
                let u = ((want - run) / n).clamp(0.0, 1.0) as f32;
                return ((i as f32 - 0.5 + u) / 255.0 * 100.0).clamp(0.0, 100.0);
            }
            run += n;
        }
        100.0
    }

    /// The colour of each band (shadows, midtones, highlights). A band with
    /// little in it leans on the whole picture's.
    pub fn band_colors(&self) -> [BandColor; 3] {
        let all = self.bands.iter().fold([0.0f64; 5], |acc, b| std::array::from_fn(|i| acc[i] + b[i]));
        let color = |s: &[f64; 5]| {
            let n = s[0].max(1e-9);
            let (ma, mb) = (s[1] / n, s[2] / n);
            let var = ((s[3] / n - ma * ma) + (s[4] / n - mb * mb)).max(0.0) / 2.0;
            BandColor { a: ma as f32, b: mb as f32, spread: var.sqrt() as f32, weight: s[0] as f32 }
        };
        let whole = color(&all);
        let total = all[0].max(1e-9) as f32;
        std::array::from_fn(|k| {
            let c = color(&self.bands[k]);
            // Trusted fully from 5 % of the samples.
            let t = (c.weight / total / 0.05).clamp(0.0, 1.0);
            BandColor { a: whole.a + (c.a - whole.a) * t, b: whole.b + (c.b - whole.b) * t, spread: whole.spread + (c.spread - whole.spread) * t, weight: c.weight }
        })
    }
}

/// What to match.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MatchWhat {
    /// Brightness and contrast.
    pub tone: bool,
    /// Colour (tint, saturation, split toning).
    pub color: bool,
}

/// The grade that turns one look into another.
#[derive(Clone, Debug)]
pub struct Transfer {
    /// New L* for L* 0–100 (in 256 steps).
    tone: Vec<f32>,
    /// Per band: (from mean a, b; to mean a, b; spread scale).
    color: [([f32; 2], [f32; 2], f32); 3],
    what: MatchWhat,
}

/// The percentiles the tone curve passes through.
const PCTS: [f32; 9] = [0.5, 3.0, 10.0, 25.0, 50.0, 75.0, 90.0, 97.0, 99.5];

impl Transfer {
    /// The grade turning `from` (the clip) into `to` (the reference).
    pub fn new(from: &ColorStats, to: &ColorStats, what: MatchWhat) -> Transfer {
        // Tone: the clip's percentiles onto the reference's, rising, with
        // slopes kept between ¼ and 4, carried on straight to 0 and 100.
        let mut pts: Vec<[f32; 2]> = PCTS.iter().map(|&p| [from.percentile(p), to.percentile(p)]).collect();
        pts.dedup_by(|b, a| (b[0] - a[0]).abs() < 0.5);
        let mut tone = vec![0.0f32; 256];
        if pts.len() >= 2 {
            for i in 1..pts.len() {
                let dx = (pts[i][0] - pts[i - 1][0]).max(0.5);
                let dy = (pts[i][1] - pts[i - 1][1]).clamp(dx / 4.0, dx * 4.0);
                pts[i][1] = pts[i - 1][1] + dy;
            }
            let slope = |a: [f32; 2], b: [f32; 2]| ((b[1] - a[1]) / (b[0] - a[0]).max(0.5)).clamp(0.25, 4.0);
            let (s0, s1) = (slope(pts[0], pts[1]), slope(pts[pts.len() - 2], pts[pts.len() - 1]));
            let (first, last) = (pts[0], pts[pts.len() - 1]);
            for (i, t) in tone.iter_mut().enumerate() {
                let x = i as f32 / 255.0 * 100.0;
                *t = if x <= first[0] {
                    first[1] - (first[0] - x) * s0
                } else if x >= last[0] {
                    last[1] + (x - last[0]) * s1
                } else {
                    let j = pts.partition_point(|p| p[0] <= x).clamp(1, pts.len() - 1);
                    let (a, b) = (pts[j - 1], pts[j]);
                    a[1] + (b[1] - a[1]) * (x - a[0]) / (b[0] - a[0]).max(1e-3)
                };
            }
            // Smoothed a little (the corners at the percentiles), kept rising.
            let raw = tone.clone();
            for i in 0..256usize {
                let (lo, hi) = (i.saturating_sub(6), (i + 6).min(255));
                tone[i] = raw[lo..=hi].iter().sum::<f32>() / (hi - lo + 1) as f32;
            }
            for i in 1..256 {
                tone[i] = tone[i].max(tone[i - 1]);
            }
            for t in &mut tone {
                *t = t.clamp(0.0, 100.0);
            }
        } else {
            for (i, t) in tone.iter_mut().enumerate() {
                *t = i as f32 / 255.0 * 100.0;
            }
        }
        let (fc, tc) = (from.band_colors(), to.band_colors());
        let color = std::array::from_fn(|k| {
            let scale = if fc[k].spread > 0.5 { (tc[k].spread / fc[k].spread).clamp(0.25, 3.0) } else { 1.0 };
            ([fc[k].a, fc[k].b], [tc[k].a, tc[k].b], scale)
        });
        Transfer { tone, color, what }
    }

    /// The grade of display colour `c` (0–1).
    pub fn apply(&self, c: [f32; 3]) -> [f32; 3] {
        let lab = to_lab(c);
        let l = if self.what.tone {
            let x = (lab[0] / 100.0).clamp(0.0, 1.0) * 255.0;
            let i = (x.floor() as usize).min(254);
            let u = x - i as f32;
            self.tone[i] + (self.tone[i + 1] - self.tone[i]) * u
        } else {
            lab[0]
        };
        let (a, b) = if self.what.color {
            let w = band_weights(lab[0]);
            let (mut a, mut b) = (0.0, 0.0);
            for k in 0..3 {
                let (f, t, s) = self.color[k];
                a += w[k] * ((lab[1] - f[0]) * s + t[0]);
                b += w[k] * ((lab[2] - f[1]) * s + t[1]);
            }
            (a, b)
        } else {
            (lab[1], lab[2])
        };
        let out = from_lab([l, a, b]);
        out.map(|v| v.clamp(0.0, 1.0))
    }

    /// As a .cube 3D LUT `size` points a side (red fastest), with a title.
    pub fn cube(&self, size: usize, title: &str) -> String {
        let n = size.clamp(2, 65);
        let mut s = String::with_capacity(n * n * n * 28 + 200);
        let _ = writeln!(s, "TITLE \"{}\"", title.replace('"', "'"));
        let _ = writeln!(s, "# Made by Reelforge's Match colour");
        let _ = writeln!(s, "LUT_3D_SIZE {n}");
        let _ = writeln!(s, "DOMAIN_MIN 0.0 0.0 0.0\nDOMAIN_MAX 1.0 1.0 1.0");
        let k = 1.0 / (n - 1) as f32;
        for bi in 0..n {
            for gi in 0..n {
                for ri in 0..n {
                    let o = self.apply([ri as f32 * k, gi as f32 * k, bi as f32 * k]);
                    let _ = writeln!(s, "{:.6} {:.6} {:.6}", o[0], o[1], o[2]);
                }
            }
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stats(f: impl Fn(f32, f32) -> [f32; 3]) -> ColorStats {
        let mut s = ColorStats::new();
        for y in 0..60 {
            for x in 0..80 {
                s.add(f(x as f32 / 79.0, y as f32 / 59.0));
            }
        }
        s
    }
    const BOTH: MatchWhat = MatchWhat { tone: true, color: true };

    #[test]
    fn lab_round_trips() {
        for c in [[0.0, 0.0, 0.0], [1.0, 1.0, 1.0], [0.8, 0.3, 0.1], [0.1, 0.5, 0.9], [0.5, 0.5, 0.5]] {
            let back = from_lab(to_lab(c));
            for k in 0..3 {
                assert!((back[k] - c[k]).abs() < 2e-3, "{c:?} → {back:?}");
            }
        }
        let grey = to_lab([0.5, 0.5, 0.5]);
        assert!(grey[1].abs() < 0.2 && grey[2].abs() < 0.2 && (grey[0] - 53.4).abs() < 0.5, "{grey:?}");
    }

    #[test]
    fn a_picture_matched_to_itself_is_unchanged() {
        let s = stats(|x, y| [x, y * 0.8 + 0.1, 1.0 - x * 0.7]);
        let t = Transfer::new(&s, &s, BOTH);
        for c in [[0.2, 0.4, 0.6], [0.5, 0.5, 0.5], [0.9, 0.7, 0.2], [0.05, 0.05, 0.08]] {
            let o = t.apply(c);
            for k in 0..3 {
                assert!((o[k] - c[k]).abs() < 0.03, "{c:?} → {o:?}");
            }
        }
    }

    #[test]
    fn a_dark_flat_clip_takes_a_bright_contrasty_references_tone() {
        // The clip: greys 0.3–0.5. The reference: 0.05–0.95.
        let clip = stats(|x, _| [0.3 + x * 0.2; 3]);
        let reference = stats(|x, _| [0.05 + x * 0.9; 3]);
        let t = Transfer::new(&clip, &reference, BOTH);
        let lo = t.apply([0.3; 3])[0];
        let hi = t.apply([0.5; 3])[0];
        assert!(lo < 0.15 && hi > 0.75, "{lo} {hi}");
        // Grey stays grey.
        let m = t.apply([0.4; 3]);
        assert!((m[0] - m[1]).abs() < 0.01 && (m[1] - m[2]).abs() < 0.01, "{m:?}");
        // Tone only: no colour comes in; colour only: brightness stays.
        let warm = stats(|x, _| [0.3 + x * 0.6, 0.25 + x * 0.5, 0.1 + x * 0.3]);
        let only_tone = Transfer::new(&clip, &warm, MatchWhat { tone: true, color: false }).apply([0.4; 3]);
        assert!((only_tone[0] - only_tone[2]).abs() < 0.01, "{only_tone:?}");
        let only_color = Transfer::new(&clip, &warm, MatchWhat { tone: false, color: true }).apply([0.4; 3]);
        assert!(only_color[0] > only_color[2] + 0.05, "warmer: {only_color:?}");
        assert!((to_lab(only_color)[0] - to_lab([0.4; 3])[0]).abs() < 1.0, "as bright");
    }

    #[test]
    fn split_toning_comes_across() {
        // The reference: teal shadows, orange highlights. The clip: neutral greys.
        let reference = stats(|x, _| {
            let v = x;
            if v < 0.5 { [v * 0.6, v * 0.95 + 0.03, v + 0.06] } else { [v + 0.05, v * 0.85, v * 0.6] }
        });
        let clip = stats(|x, _| [x; 3]);
        let t = Transfer::new(&clip, &reference, BOTH);
        let dark = t.apply([0.15; 3]);
        let bright = t.apply([0.85; 3]);
        assert!(dark[2] > dark[0] + 0.03, "teal shadows: {dark:?}");
        assert!(bright[0] > bright[2] + 0.1, "orange highlights: {bright:?}");
    }

    #[test]
    fn saturation_follows_the_reference() {
        // The clip: muted colours; the reference: the same hues, vivid.
        let hues = |sat: f32| move |x: f32, y: f32| {
            let h = x * 6.0;
            let c = [((h - 0.0).abs() - 1.0).clamp(0.0, 1.0), (2.0 - (h - 2.0).abs()).clamp(0.0, 1.0), (2.0 - (h - 4.0).abs()).clamp(0.0, 1.0)];
            let v = 0.35 + y * 0.3;
            c.map(|k| v + (k - 0.5) * sat)
        };
        let t = Transfer::new(&stats(hues(0.15)), &stats(hues(0.6)), BOTH);
        let c = [0.55, 0.45, 0.45];
        let o = t.apply(c);
        let chroma = |c: [f32; 3]| {
            let l = to_lab(c);
            (l[1] * l[1] + l[2] * l[2]).sqrt()
        };
        assert!(chroma(o) > chroma(c) * 1.8, "{} → {}", chroma(c), chroma(o));
    }

    #[test]
    fn writes_a_cube_that_reads_back() {
        let s = stats(|x, y| [x, y, 0.5]);
        let r = stats(|x, y| [x * 0.9 + 0.05, y * 0.8, 0.6]);
        let t = Transfer::new(&s, &r, BOTH);
        let text = t.cube(17, "Match \"test\"");
        assert!(text.starts_with("TITLE \"Match 'test'\""));
        let table = crate::lut::parse_cube(&text).unwrap();
        assert_eq!(table.cube_size, 17);
        // Empty stats leave things be.
        let none = Transfer::new(&ColorStats::new(), &ColorStats::new(), BOTH);
        let o = none.apply([0.3, 0.6, 0.2]);
        assert!((o[0] - 0.3).abs() < 0.02 && (o[1] - 0.6).abs() < 0.02, "{o:?}");
    }
}
