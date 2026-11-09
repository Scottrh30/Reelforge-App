//! Video scopes: a waveform (luma or RGB overlaid), an RGB parade, a
//! vectorscope and a histogram of a picture (the Viewer's), each drawn as
//! an RGBA image of the size asked for, with its graticule. Levels are the
//! picture's display values (R′G′B′, 0–100 %); luma and chroma are Rec. 709.

use serde::Deserialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
pub enum Scope {
    /// Brightness (left to right as in the picture) against level.
    Waveform,
    /// A waveform for each of red, green and blue, side by side.
    Parade,
    /// Hue (angle) and saturation (distance from the middle).
    Vectorscope,
    /// How many pixels at each level.
    Histogram,
}

#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
pub struct ScopeOptions {
    pub scope: Scope,
    pub width: u32,
    pub height: u32,
    /// Waveform: red, green and blue overlaid rather than luma. Histogram:
    /// red, green and blue rather than luma.
    #[serde(default)]
    pub rgb: bool,
    /// How bright the trace is (1 normal; 0.25–4).
    #[serde(default = "one")]
    pub brightness: f32,
    /// Vectorscope magnification (1, 2 or 4: low-saturation pictures).
    #[serde(default = "one")]
    pub zoom: f32,
}

fn one() -> f32 {
    1.0
}

/// Rec. 709 luma of display values (0–1).
pub fn luma(r: f32, g: f32, b: f32) -> f32 {
    0.2126 * r + 0.7152 * g + 0.0722 * b
}

/// Rec. 709 (Cb, Cr) of display values (0–1), each -0.5 to 0.5.
pub fn chroma(r: f32, g: f32, b: f32) -> (f32, f32) {
    let y = luma(r, g, b);
    ((b - y) / 1.8556, (r - y) / 1.5748)
}

/// The chroma that reaches the vectorscope's rim at zoom 1.
pub const VECTOR_RIM: f32 = 0.6;

/// Where chroma (cb, cr) lands on a vectorscope of `size` pixels square,
/// as fractions (0–1, from the top left): blue to the right, red up.
pub fn vector_point(cb: f32, cr: f32, zoom: f32) -> (f32, f32) {
    let k = 0.5 * zoom / VECTOR_RIM;
    (0.5 + cb * k, 0.5 - cr * k)
}

/// The colours the vectorscope marks (at 75 % and 100 %): name, R′G′B′.
pub const TARGETS: [(&str, [f32; 3]); 6] =
    [("R", [1.0, 0.0, 0.0]), ("Mg", [1.0, 0.0, 1.0]), ("B", [0.0, 0.0, 1.0]), ("Cy", [0.0, 1.0, 1.0]), ("G", [0.0, 1.0, 0.0]), ("Yl", [1.0, 1.0, 0.0])];

/// The skin tone line's angle (degrees from the +Cb axis, towards +Cr).
pub const SKIN_LINE: f32 = 123.0;

/// Draw scope `o` of the `iw`×`ih` RGBA picture `rgba`.
pub fn render(rgba: &[u8], iw: u32, ih: u32, o: &ScopeOptions) -> Vec<u8> {
    let (w, h) = (o.width.clamp(16, 2048) as usize, o.height.clamp(16, 2048) as usize);
    let mut c = Canvas { w, h, px: vec![0u8; w * h * 4] };
    c.fill([16, 16, 18]);
    if iw == 0 || ih == 0 || rgba.len() < (iw * ih * 4) as usize {
        return c.px;
    }
    let pic = Picture::new(rgba, iw as usize, ih as usize);
    let bright = o.brightness.clamp(0.1, 8.0);
    match o.scope {
        Scope::Waveform => waveform(&mut c, &pic, 0, w, if o.rgb { Trace::Rgb } else { Trace::Luma }, bright),
        Scope::Parade => {
            let third = w / 3;
            for (k, ch) in [Trace::Red, Trace::Green, Trace::Blue].into_iter().enumerate() {
                let x0 = k * third;
                let x1 = if k == 2 { w } else { x0 + third };
                waveform(&mut c, &pic, x0, x1 - x0, ch, bright);
                if k > 0 {
                    c.vline(x0, 0, h, [70, 70, 76]);
                }
            }
        }
        Scope::Vectorscope => vectorscope(&mut c, &pic, bright, o.zoom.clamp(0.5, 8.0)),
        Scope::Histogram => histogram(&mut c, &pic, o.rgb),
    }
    c.px
}

/// A picture sampled sparsely enough to be quick (every `step`-th pixel
/// each way, about a third of a million samples at most).
struct Picture<'a> {
    rgba: &'a [u8],
    w: usize,
    h: usize,
    step: usize,
}

impl<'a> Picture<'a> {
    fn new(rgba: &'a [u8], w: usize, h: usize) -> Self {
        let step = ((w * h) as f64 / 350_000.0).sqrt().ceil().max(1.0) as usize;
        Picture { rgba, w, h, step }
    }
    /// Every sample: x, y, and its R′G′B′ (0–1). Fully transparent pixels are left out.
    fn each(&self, mut f: impl FnMut(usize, usize, [f32; 3])) {
        for y in (0..self.h).step_by(self.step) {
            for x in (0..self.w).step_by(self.step) {
                let i = (y * self.w + x) * 4;
                let p = &self.rgba[i..i + 4];
                if p[3] == 0 {
                    continue;
                }
                f(x, y, [p[0] as f32 / 255.0, p[1] as f32 / 255.0, p[2] as f32 / 255.0]);
            }
        }
    }
    fn samples(&self) -> usize {
        self.w.div_ceil(self.step) * self.h.div_ceil(self.step)
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Trace {
    Luma,
    Rgb,
    Red,
    Green,
    Blue,
}

/// A waveform in columns `x0..x0 + w` of the canvas.
fn waveform(c: &mut Canvas, pic: &Picture, x0: usize, w: usize, trace: Trace, bright: f32) {
    let h = c.h;
    // Room at the top and bottom so 0 and 100 % lines aren't on the edge.
    let (top, bottom) = (pad(h), h - 1 - pad(h));
    let span = (bottom - top) as f32;
    let row = |v: f32| (bottom as f32 - v.clamp(0.0, 1.0) * span).round() as usize;
    graticule_levels(c, x0, w, top, bottom);
    let chans = if trace == Trace::Rgb { 3 } else { 1 };
    let mut n = vec![0u32; w * h * chans];
    // The columns a sample's column covers (several when the scope is wider
    // than the picture is sampled).
    let cols = |x: usize| {
        let a = (x * w / pic.w).min(w - 1);
        let b = ((x + pic.step) * w / pic.w).clamp(a + 1, w);
        a..b
    };
    pic.each(|x, _, p| {
        for col in cols(x) {
            match trace {
                Trace::Luma => n[row(luma(p[0], p[1], p[2])) * w + col] += 1,
                Trace::Red => n[row(p[0]) * w + col] += 1,
                Trace::Green => n[row(p[1]) * w + col] += 1,
                Trace::Blue => n[row(p[2]) * w + col] += 1,
                Trace::Rgb => {
                    for k in 0..3 {
                        n[(row(p[k]) * w + col) * 3 + k] += 1;
                    }
                }
            }
        }
    });
    // A cell holding a 40th of its column's samples is about two-thirds lit.
    let per_col = pic.samples() as f32 / w.min(pic.w.div_ceil(pic.step)) as f32;
    let k = bright * 40.0 / per_col.max(1.0);
    let lit = |count: u32| 1.0 - (-(count as f32) * k).exp();
    for y in 0..h {
        for x in 0..w {
            let col = match trace {
                Trace::Rgb => {
                    let i = (y * w + x) * 3;
                    [lit(n[i]), lit(n[i + 1]), lit(n[i + 2])]
                }
                t => {
                    let v = lit(n[y * w + x]);
                    match t {
                        Trace::Red => [v, v * 0.25, v * 0.2],
                        Trace::Green => [v * 0.25, v, v * 0.3],
                        Trace::Blue => [v * 0.3, v * 0.45, v],
                        _ => [v * 0.75, v, v * 0.78],
                    }
                }
            };
            if col.iter().any(|&v| v > 0.003) {
                c.add(x0 + x, y, col);
            }
        }
    }
}

fn pad(h: usize) -> usize {
    (h / 24).clamp(2, 12)
}

/// Lines at every 10 % (brighter at 0, 50 and 100 %).
fn graticule_levels(c: &mut Canvas, x0: usize, w: usize, top: usize, bottom: usize) {
    for k in 0..=10 {
        let y = (bottom as f32 - k as f32 / 10.0 * (bottom - top) as f32).round() as usize;
        let shade = if k % 5 == 0 { [62, 62, 68] } else { [34, 34, 38] };
        c.hline(x0, x0 + w, y, shade);
    }
}

/// Where level `v` (0–1) is drawn on a waveform or parade `h` pixels high
/// (rows from the top).
pub fn level_row(v: f32, h: u32) -> f32 {
    let h = h as usize;
    let (top, bottom) = (pad(h), h - 1 - pad(h));
    bottom as f32 - v * (bottom - top) as f32
}

fn vectorscope(c: &mut Canvas, pic: &Picture, bright: f32, zoom: f32) {
    let s = c.w.min(c.h);
    let (ox, oy) = ((c.w - s) / 2, (c.h - s) / 2);
    let at = |cb: f32, cr: f32| {
        let (fx, fy) = vector_point(cb, cr, zoom);
        ((fx * (s - 1) as f32).round() as isize, (fy * (s - 1) as f32).round() as isize)
    };
    // Graticule: the rim, a cross, the skin tone line, and the colour targets.
    let mid = (s as f32 - 1.0) / 2.0;
    c.circle(ox as f32 + mid, oy as f32 + mid, mid - 0.5, [62, 62, 68]);
    c.hline(ox, ox + s, oy + s / 2, [34, 34, 38]);
    c.vline(ox + s / 2, oy, oy + s, [34, 34, 38]);
    let a = SKIN_LINE.to_radians();
    for k in 0..(s / 2) {
        if (k / 3) % 2 == 0 {
            let r = k as f32;
            c.put((ox as f32 + mid + a.cos() * r).round() as usize, (oy as f32 + mid - a.sin() * r).round() as usize, [120, 96, 70]);
        }
    }
    let box_r = (s / 64).max(2) as isize;
    for (_, rgb) in TARGETS {
        for (amount, shade) in [(0.75, [110u8, 110, 118]), (1.0, [70, 70, 78])] {
            let (cb, cr) = chroma(rgb[0] * amount, rgb[1] * amount, rgb[2] * amount);
            let (x, y) = at(cb, cr);
            c.rect(ox as isize + x - box_r, oy as isize + y - box_r, ox as isize + x + box_r, oy as isize + y + box_r, shade);
        }
    }
    // Each cell: how many samples land there and their colours (so the
    // trace shows the hues it stands for).
    let mut n = vec![0u32; s * s];
    let mut sum = vec![[0.0f32; 3]; s * s];
    pic.each(|_, _, p| {
        let (cb, cr) = chroma(p[0], p[1], p[2]);
        let (x, y) = at(cb, cr);
        if x < 0 || y < 0 || x >= s as isize || y >= s as isize {
            return;
        }
        let i = y as usize * s + x as usize;
        n[i] += 1;
        for k in 0..3 {
            sum[i][k] += p[k];
        }
    });
    let k = bright * 2000.0 / pic.samples().max(1) as f32;
    for y in 0..s {
        for x in 0..s {
            let i = y * s + x;
            if n[i] == 0 {
                continue;
            }
            let v = 1.0 - (-(n[i] as f32) * k).exp();
            let avg = sum[i].map(|t| t / n[i] as f32);
            let top = avg.iter().cloned().fold(1e-3, f32::max);
            // Its own hue, at full brightness, half towards white.
            let col = avg.map(|t| (0.5 + 0.5 * t / top) * v);
            c.add(ox + x, oy + y, col);
        }
    }
}

fn histogram(c: &mut Canvas, pic: &Picture, rgb: bool) {
    let (w, h) = (c.w, c.h);
    let mut bins = [[0u32; 256]; 4];
    pic.each(|_, _, p| {
        for k in 0..3 {
            bins[k][(p[k] * 255.0).round() as usize] += 1;
        }
        bins[3][(luma(p[0], p[1], p[2]) * 255.0).round().min(255.0) as usize] += 1;
    });
    for k in 1..4 {
        let x = k * w / 4;
        c.vline(x, 0, h, [34, 34, 38]);
    }
    let shown: &[usize] = if rgb { &[0, 1, 2] } else { &[3] };
    // Scaled to the tallest bin away from the ends (clipped black or white
    // would otherwise flatten the rest); those two may run off the top.
    let tallest = shown.iter().flat_map(|&k| bins[k][1..255].iter().copied()).max().unwrap_or(0).max(1) as f32;
    let top = pad(h) as f32;
    for x in 0..w {
        // The bins this column covers (as a column may span several).
        let (b0, b1) = (x * 256 / w, ((x + 1) * 256 / w).max(x * 256 / w + 1).min(256));
        for &k in shown {
            let count = bins[k][b0..b1].iter().copied().max().unwrap_or(0) as f32;
            let tall = ((count / tallest) * (h as f32 - top)).min(h as f32);
            let col = match k {
                0 => [0.85, 0.2, 0.18],
                1 => [0.2, 0.8, 0.28],
                2 => [0.25, 0.4, 0.95],
                _ => [0.78, 0.78, 0.8],
            };
            let y0 = h as f32 - tall;
            for y in (y0.floor().max(0.0) as usize)..h {
                c.add(x, y, col.map(|v| v * 0.7));
            }
        }
    }
}

struct Canvas {
    w: usize,
    h: usize,
    px: Vec<u8>,
}

impl Canvas {
    fn fill(&mut self, rgb: [u8; 3]) {
        for p in self.px.chunks_exact_mut(4) {
            p.copy_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
        }
    }
    fn put(&mut self, x: usize, y: usize, rgb: [u8; 3]) {
        if x < self.w && y < self.h {
            let i = (y * self.w + x) * 4;
            self.px[i..i + 3].copy_from_slice(&rgb);
        }
    }
    /// Light added on top (0–1 per channel).
    fn add(&mut self, x: usize, y: usize, rgb: [f32; 3]) {
        if x < self.w && y < self.h {
            let i = (y * self.w + x) * 4;
            for k in 0..3 {
                self.px[i + k] = (self.px[i + k] as f32 + rgb[k].clamp(0.0, 1.0) * 255.0).min(255.0) as u8;
            }
        }
    }
    fn hline(&mut self, x0: usize, x1: usize, y: usize, rgb: [u8; 3]) {
        for x in x0..x1.min(self.w) {
            self.put(x, y, rgb);
        }
    }
    fn vline(&mut self, x: usize, y0: usize, y1: usize, rgb: [u8; 3]) {
        for y in y0..y1.min(self.h) {
            self.put(x, y, rgb);
        }
    }
    fn rect(&mut self, x0: isize, y0: isize, x1: isize, y1: isize, rgb: [u8; 3]) {
        for x in x0..=x1 {
            for y in [y0, y1] {
                if x >= 0 && y >= 0 {
                    self.put(x as usize, y as usize, rgb);
                }
            }
        }
        for y in y0..=y1 {
            for x in [x0, x1] {
                if x >= 0 && y >= 0 {
                    self.put(x as usize, y as usize, rgb);
                }
            }
        }
    }
    fn circle(&mut self, cx: f32, cy: f32, r: f32, rgb: [u8; 3]) {
        let n = (r * 7.0).max(16.0) as usize;
        for k in 0..n {
            let a = k as f32 / n as f32 * std::f32::consts::TAU;
            let (x, y) = (cx + a.cos() * r, cy + a.sin() * r);
            if x >= 0.0 && y >= 0.0 {
                self.put(x.round() as usize, y.round() as usize, rgb);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(w: u32, h: u32, f: impl Fn(u32, u32) -> [u8; 3]) -> Vec<u8> {
        let mut v = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let c = f(x, y);
                v.extend_from_slice(&[c[0], c[1], c[2], 255]);
            }
        }
        v
    }
    fn at(img: &[u8], w: usize, x: usize, y: usize) -> [u8; 3] {
        let i = (y * w + x) * 4;
        [img[i], img[i + 1], img[i + 2]]
    }
    fn lum(p: [u8; 3]) -> u32 {
        p.iter().map(|&v| v as u32).sum()
    }
    fn opts(scope: Scope) -> ScopeOptions {
        ScopeOptions { scope, width: 200, height: 120, rgb: false, brightness: 1.0, zoom: 1.0 }
    }

    #[test]
    fn a_waveform_shows_each_columns_levels() {
        // Left half black, right half white.
        let pic = image(64, 36, |x, _| if x < 32 { [0, 0, 0] } else { [255, 255, 255] });
        let o = opts(Scope::Waveform);
        let s = render(&pic, 64, 36, &o);
        let (black, white) = (level_row(0.0, 120).round() as usize, level_row(1.0, 120).round() as usize);
        assert!(lum(at(&s, 200, 40, black)) > 400 && lum(at(&s, 200, 40, white)) < 200, "black on the left at 0 %");
        assert!(lum(at(&s, 200, 160, white)) > 400 && lum(at(&s, 200, 160, black)) < 200, "white on the right at 100 %");
        // Mid-grey (0.5) sits halfway.
        let grey = image(16, 16, |_, _| [128, 128, 128]);
        let s = render(&grey, 16, 16, &o);
        let mid = level_row(128.0 / 255.0, 120).round() as usize;
        assert!(lum(at(&s, 200, 100, mid)) > 400);
        // Only the luma: a pure red picture sits at 21 %.
        let red = image(16, 16, |_, _| [255, 0, 0]);
        let s = render(&red, 16, 16, &o);
        assert!(lum(at(&s, 200, 100, level_row(0.2126, 120).round() as usize)) > 400);
    }

    #[test]
    fn the_parade_splits_red_green_and_blue() {
        // Orange: red high, green middling, blue none.
        let pic = image(32, 32, |_, _| [255, 128, 0]);
        let s = render(&pic, 32, 32, &opts(Scope::Parade));
        let row = |v: f32| level_row(v, 120).round() as usize;
        let red = at(&s, 200, 33, row(1.0));
        let green = at(&s, 200, 100, row(128.0 / 255.0));
        let blue = at(&s, 200, 166, row(0.0));
        assert!(red[0] > 150 && red[0] > red[1] * 2, "{red:?}");
        assert!(green[1] > 150 && green[1] > green[0] * 2, "{green:?}");
        assert!(blue[2] > 150 && blue[2] > blue[0], "{blue:?}");
        assert!(lum(at(&s, 200, 100, row(1.0))) < 200, "no green at 100 %");
    }

    #[test]
    fn the_vectorscope_puts_colours_at_their_targets() {
        let o = ScopeOptions { width: 160, height: 160, ..opts(Scope::Vectorscope) };
        // Grey sits in the middle.
        let s = render(&image(16, 16, |_, _| [128, 128, 128]), 16, 16, &o);
        assert!(lum(at(&s, 160, 80, 80)) > 300 || lum(at(&s, 160, 79, 79)) > 300);
        // 75 % red lands in its box, up and a little left; and is drawn red.
        let s = render(&image(16, 16, |_, _| [191, 0, 0]), 16, 16, &o);
        let (cb, cr) = chroma(0.75, 0.0, 0.0);
        let (fx, fy) = vector_point(cb, cr, 1.0);
        let (x, y) = ((fx * 159.0).round() as usize, (fy * 159.0).round() as usize);
        assert!(x < 80 && y < 50, "{x},{y}");
        let p = at(&s, 160, x, y);
        assert!(p[0] > 200 && p[0] > p[2] + 60, "{p:?}");
        // Zoomed in, the same red runs off the rim; a pale one moves out.
        let pale = image(16, 16, |_, _| [150, 128, 128]);
        let d = |zoom: f32| {
            let (cb, cr) = chroma(150.0 / 255.0, 128.0 / 255.0, 128.0 / 255.0);
            let (fx, fy) = vector_point(cb, cr, zoom);
            ((fx - 0.5).powi(2) + (fy - 0.5).powi(2)).sqrt()
        };
        assert!(d(4.0) > d(1.0) * 3.9);
        let s = render(&pale, 16, 16, &ScopeOptions { zoom: 4.0, ..o });
        let (cb, cr) = chroma(150.0 / 255.0, 128.0 / 255.0, 128.0 / 255.0);
        let (fx, fy) = vector_point(cb, cr, 4.0);
        assert!(lum(at(&s, 160, (fx * 159.0).round() as usize, (fy * 159.0).round() as usize)) > 300);
    }

    #[test]
    fn the_histogram_counts_levels() {
        // A quarter black, three quarters at 192.
        let pic = image(40, 40, |x, _| if x < 10 { [0, 0, 0] } else { [192, 192, 192] });
        let s = render(&pic, 40, 40, &ScopeOptions { width: 256, height: 100, ..opts(Scope::Histogram) });
        assert!(lum(at(&s, 256, 192, 30)) > 300, "the tall bar at 192");
        assert!(lum(at(&s, 256, 128, 95)) < 160, "nothing at 128");
        assert!(lum(at(&s, 256, 0, 95)) > 300, "the blacks");
        // RGB: a red picture fills only the red bar at 255 and the 0 bars of green and blue.
        let s = render(&image(8, 8, |_, _| [255, 0, 0]), 8, 8, &ScopeOptions { width: 256, height: 100, rgb: true, ..opts(Scope::Histogram) });
        let p = at(&s, 256, 255, 90);
        assert!(p[0] > 150 && p[1] < 60, "{p:?}");
        let p = at(&s, 256, 0, 90);
        assert!(p[1] > 150 && p[2] > 150 && p[0] < p[1], "{p:?}");
    }

    #[test]
    fn odd_inputs_are_safe() {
        for scope in [Scope::Waveform, Scope::Parade, Scope::Vectorscope, Scope::Histogram] {
            assert_eq!(render(&[], 0, 0, &opts(scope)).len(), 200 * 120 * 4);
            assert_eq!(render(&[1, 2, 3], 5, 5, &opts(scope)).len(), 200 * 120 * 4);
            let big = image(4000, 30, |x, _| [x as u8, 0, 0]);
            assert_eq!(render(&big, 4000, 30, &ScopeOptions { width: 3, height: 9000, ..opts(scope) }).len(), 16 * 2048 * 4);
        }
    }
}
