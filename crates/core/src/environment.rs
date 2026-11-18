//! Image-based lighting: a panorama (an equirectangular picture, usually an
//! HDR from a light probe) that lights a 3D scene from every direction and
//! is reflected in shiny 3D text, shapes and models.
//!
//! The picture is read once (Radiance `.hdr` here; OpenEXR and ordinary
//! pictures through the media crate's loader), shrunk to [`MAX_WIDTH`], and
//! its diffuse light summed into four spherical-harmonic terms, so a surface
//! facing `n` gets `k0 + kx·n.x + ky·n.y + kz·n.z` (what Lambert shading of
//! every direction would give, to first order).
//!
//! Directions: the middle of the picture is +Z (the way the default camera
//! looks), its top is up (−Y), and its left and right edges meet behind.

use crate::space3d::V3;
use std::collections::HashMap;
use std::f32::consts::PI;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

/// Kept at most this wide (and half as high): plenty for reflections and light.
pub const MAX_WIDTH: u32 = 1024;

/// A loaded environment.
#[derive(Debug)]
pub struct Environment {
    /// Changes with the file (path and modification time).
    pub id: u64,
    pub width: u32,
    pub height: u32,
    /// Linear-light RGB, row by row from the top.
    pub rgb: Vec<f32>,
    /// Diffuse light by surface direction: constant, then the X, Y and Z terms.
    pub sh: [V3; 4],
}

/// Reads a picture as linear-light RGB (set by the media crate, which can
/// decode OpenEXR and ordinary pictures).
pub type EnvLoader = fn(&Path) -> Option<(u32, u32, Vec<f32>)>;
static LOADER: OnceLock<EnvLoader> = OnceLock::new();

pub fn set_env_loader(f: EnvLoader) {
    let _ = LOADER.set(f);
}

type Cache = Mutex<HashMap<PathBuf, (std::time::SystemTime, Option<Arc<Environment>>)>>;
fn cache() -> &'static Cache {
    static C: OnceLock<Cache> = OnceLock::new();
    C.get_or_init(Default::default)
}
fn by_id() -> &'static Mutex<HashMap<u64, Arc<Environment>>> {
    static C: OnceLock<Mutex<HashMap<u64, Arc<Environment>>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

/// The environment in `path` (cached until the file changes), `None` if it
/// can't be read.
pub fn load(path: &Path) -> Option<Arc<Environment>> {
    let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok()?;
    if let Some((m, e)) = cache().lock().unwrap().get(path) {
        if *m == modified {
            return e.clone();
        }
    }
    let id = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (path, modified).hash(&mut h);
        h.finish() | 1
    };
    let is_hdr = path.extension().is_some_and(|e| e.eq_ignore_ascii_case("hdr") || e.eq_ignore_ascii_case("pic"));
    let read = if is_hdr { std::fs::read(path).ok().and_then(|b| read_hdr(&b, MAX_WIDTH)) } else { LOADER.get().and_then(|f| f(path)).map(|(w, h, rgb)| shrink(w, h, &rgb, MAX_WIDTH)) };
    let env = read.map(|(w, h, rgb)| Arc::new(Environment::new(id, w, h, rgb)));
    // A picture the loader can't read yet (there's no loader) is tried again later.
    if is_hdr || LOADER.get().is_some() {
        cache().lock().unwrap().insert(path.to_path_buf(), (modified, env.clone()));
    }
    if let Some(e) = &env {
        by_id().lock().unwrap().insert(e.id, e.clone());
    }
    env
}

/// A loaded environment by its id (for the renderer, which uploads its picture).
pub fn get(id: u64) -> Option<Arc<Environment>> {
    by_id().lock().unwrap().get(&id).cloned()
}

/// The direction the picture's point (`u`, `v`, 0–1 from the top left) looks along.
pub fn direction(u: f32, v: f32) -> V3 {
    let phi = (u - 0.5) * 2.0 * PI;
    let theta = v * PI;
    [theta.sin() * phi.sin(), -theta.cos(), theta.sin() * phi.cos()]
}

/// Where in the picture (`u`, `v`) direction `d` (unit) looks.
pub fn uv_of(d: V3) -> [f32; 2] {
    let phi = d[0].atan2(d[2]);
    let theta = (-d[1]).clamp(-1.0, 1.0).acos();
    [phi / (2.0 * PI) + 0.5, theta / PI]
}

impl Environment {
    pub fn new(id: u64, width: u32, height: u32, rgb: Vec<f32>) -> Self {
        let mut l = [[0.0f32; 3]; 4];
        let (w, h) = (width.max(1) as usize, height.max(1) as usize);
        let dw = 2.0 * PI / w as f32 * PI / h as f32;
        for y in 0..h {
            let v = (y as f32 + 0.5) / h as f32;
            let solid = dw * (v * PI).sin();
            for x in 0..w {
                let d = direction((x as f32 + 0.5) / w as f32, v);
                let i = (y * w + x) * 3;
                let ys = [0.282_095, 0.488_603 * d[0], 0.488_603 * d[1], 0.488_603 * d[2]];
                for (k, yk) in ys.iter().enumerate() {
                    for c in 0..3 {
                        l[k][c] += rgb[i + c] * yk * solid;
                    }
                }
            }
        }
        // Irradiance over π (Lambert): A0/π = 1, A1/π = 2/3 (Ramamoorthi & Hanrahan).
        let sh = [l[0].map(|v| v * 0.282_095), l[1].map(|v| v * 0.488_603 * 2.0 / 3.0), l[2].map(|v| v * 0.488_603 * 2.0 / 3.0), l[3].map(|v| v * 0.488_603 * 2.0 / 3.0)];
        Environment { id, width, height, rgb, sh }
    }

    /// The diffuse terms with the picture turned `turn` radians about the up
    /// axis (as the shaders turn their lookups).
    pub fn turned_sh(&self, turn: f32) -> [V3; 4] {
        let (s, c) = turn.sin_cos();
        let [k0, kx, ky, kz] = self.sh;
        let x = [0, 1, 2].map(|i| kx[i] * c + kz[i] * s);
        let z = [0, 1, 2].map(|i| kz[i] * c - kx[i] * s);
        [k0, x, ky, z]
    }

    /// The light a surface facing `n` gets, per channel (the CPU reference).
    pub fn diffuse(sh: &[V3; 4], n: V3) -> V3 {
        [0, 1, 2].map(|c| (sh[0][c] + sh[1][c] * n[0] + sh[2][c] * n[1] + sh[3][c] * n[2]).max(0.0))
    }

    /// The picture's colour along direction `d` (nearest pixel).
    pub fn sample(&self, d: V3) -> V3 {
        let [u, v] = uv_of(d);
        let x = ((u.rem_euclid(1.0) * self.width as f32) as usize).min(self.width as usize - 1);
        let y = ((v * self.height as f32) as usize).min(self.height as usize - 1);
        let i = (y * self.width as usize + x) * 3;
        [self.rgb[i], self.rgb[i + 1], self.rgb[i + 2]]
    }
}

/// Averaged down to at most `max_w` wide (each output pixel the mean of the
/// source pixels it covers), and to twice as wide as high.
pub fn shrink(w: u32, h: u32, rgb: &[f32], max_w: u32) -> (u32, u32, Vec<f32>) {
    let (ow, oh) = target_size(w, h, max_w);
    let mut acc = Accumulator::new(w, h, ow, oh);
    for y in 0..h as usize {
        acc.row(y, |x| {
            let i = (y * w as usize + x) * 3;
            [rgb[i], rgb[i + 1], rgb[i + 2]]
        });
    }
    (ow, oh, acc.finish())
}

fn target_size(w: u32, h: u32, max_w: u32) -> (u32, u32) {
    let ow = w.clamp(1, max_w.max(2));
    (ow, (ow / 2).clamp(1, h.max(1)))
}

/// Box-filters rows of a `w`×`h` picture down to `ow`×`oh` as they come.
struct Accumulator {
    w: usize,
    h: usize,
    ow: usize,
    oh: usize,
    sum: Vec<f32>,
    count: Vec<f32>,
}

impl Accumulator {
    fn new(w: u32, h: u32, ow: u32, oh: u32) -> Self {
        let (ow, oh) = (ow as usize, oh as usize);
        Accumulator { w: w as usize, h: h as usize, ow, oh, sum: vec![0.0; ow * oh * 3], count: vec![0.0; ow * oh] }
    }
    fn row(&mut self, y: usize, px: impl Fn(usize) -> [f32; 3]) {
        let oy = (y * self.oh / self.h.max(1)).min(self.oh - 1);
        for x in 0..self.w {
            let ox = (x * self.ow / self.w.max(1)).min(self.ow - 1);
            let p = px(x);
            let o = oy * self.ow + ox;
            for c in 0..3 {
                // Inf and NaN (a broken file) count as black.
                self.sum[o * 3 + c] += if p[c].is_finite() { p[c].max(0.0) } else { 0.0 };
            }
            self.count[o] += 1.0;
        }
    }
    fn finish(mut self) -> Vec<f32> {
        for (o, n) in self.count.iter().enumerate() {
            for c in 0..3 {
                self.sum[o * 3 + c] /= n.max(1.0);
            }
        }
        self.sum
    }
}

/// A Radiance RGBE picture (`.hdr`), shrunk to at most `max_w` wide as it's read.
pub fn read_hdr(b: &[u8], max_w: u32) -> Option<(u32, u32, Vec<f32>)> {
    // The header: lines up to an empty one, then the size ("-Y h +X w").
    let mut pos = 0;
    let mut line = || -> Option<&[u8]> {
        let start = pos;
        let end = start + b.get(start..)?.iter().position(|&c| c == b'\n')?;
        pos = end + 1;
        Some(&b[start..end])
    };
    let first = line()?;
    if !first.starts_with(b"#?") {
        return None;
    }
    loop {
        let l = line()?;
        if l.is_empty() {
            break;
        }
        if l.starts_with(b"FORMAT=") && !l.ends_with(b"32-bit_rle_rgbe") {
            return None;
        }
    }
    let size = std::str::from_utf8(line()?).ok()?.to_owned();
    let parts: Vec<&str> = size.split_whitespace().collect();
    if parts.len() != 4 || parts[0] != "-Y" || parts[2] != "+X" {
        return None;
    }
    let (h, w): (u32, u32) = (parts[1].parse().ok()?, parts[3].parse().ok()?);
    if w == 0 || h == 0 || w > 65_536 || h > 65_536 {
        return None;
    }
    let (ow, oh) = target_size(w, h, max_w);
    let mut acc = Accumulator::new(w, h, ow, oh);
    let wu = w as usize;
    let mut scan = vec![0u8; wu * 4];
    for y in 0..h as usize {
        let rest = b.get(pos..)?;
        if (8..32_768).contains(&wu) && rest.len() >= 4 && rest[0] == 2 && rest[1] == 2 && rest[2] & 0x80 == 0 {
            // New run-length encoding: each of R, G, B, E in turn.
            if ((rest[2] as usize) << 8 | rest[3] as usize) != wu {
                return None;
            }
            pos += 4;
            for c in 0..4 {
                let mut x = 0;
                while x < wu {
                    let n = *b.get(pos)? as usize;
                    pos += 1;
                    if n > 128 {
                        let run = n - 128;
                        let v = *b.get(pos)?;
                        pos += 1;
                        if x + run > wu {
                            return None;
                        }
                        for i in 0..run {
                            scan[(x + i) * 4 + c] = v;
                        }
                        x += run;
                    } else {
                        if n == 0 || x + n > wu {
                            return None;
                        }
                        let src = b.get(pos..pos + n)?;
                        for (i, v) in src.iter().enumerate() {
                            scan[(x + i) * 4 + c] = *v;
                        }
                        pos += n;
                        x += n;
                    }
                }
            }
        } else {
            // Flat (or old-style runs, read as flat).
            scan.copy_from_slice(b.get(pos..pos + wu * 4)?);
            pos += wu * 4;
        }
        acc.row(y, |x| rgbe(&scan[x * 4..x * 4 + 4]));
    }
    Some((ow, oh, acc.finish()))
}

fn rgbe(p: &[u8]) -> [f32; 3] {
    if p[3] == 0 {
        return [0.0; 3];
    }
    let f = 2f32.powi(p[3] as i32 - 136);
    [(p[0] as f32 + 0.5) * f, (p[1] as f32 + 0.5) * f, (p[2] as f32 + 0.5) * f]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Radiance file of a `w`×`h` picture, `px(x, y)` per pixel, run-length encoded.
    pub fn hdr_file(w: usize, h: usize, px: impl Fn(usize, usize) -> [f32; 3]) -> Vec<u8> {
        let mut b = format!("#?RADIANCE\nFORMAT=32-bit_rle_rgbe\n\n-Y {h} +X {w}\n").into_bytes();
        for y in 0..h {
            let row: Vec<[u8; 4]> = (0..w)
                .map(|x| {
                    let c = px(x, y);
                    let m = c[0].max(c[1]).max(c[2]);
                    if m < 1e-32 {
                        return [0; 4];
                    }
                    let e = m.log2().floor() as i32 + 1;
                    let k = 256.0 / 2f32.powi(e);
                    [(c[0] * k) as u8, (c[1] * k) as u8, (c[2] * k) as u8, (e + 128) as u8]
                })
                .collect();
            b.extend([2, 2, (w >> 8) as u8, w as u8]);
            for c in 0..4 {
                // Plain runs of up to 128.
                for chunk in row.chunks(128) {
                    b.push(chunk.len() as u8);
                    b.extend(chunk.iter().map(|p| p[c]));
                }
            }
        }
        b
    }

    #[test]
    fn directions_and_uv_round_trip() {
        let mid = direction(0.5, 0.5);
        assert!((mid[2] - 1.0).abs() < 1e-5, "the middle looks along +Z: {mid:?}");
        assert!((direction(0.3, 0.0)[1] + 1.0).abs() < 1e-5, "the top looks up (−Y)");
        for (u, v) in [(0.1, 0.2), (0.7, 0.6), (0.5, 0.9)] {
            let [a, b] = uv_of(direction(u, v));
            assert!((a - u).abs() < 1e-4 && (b - v).abs() < 1e-4);
        }
    }

    /// A uniform sky of brightness 1 lights every surface by 1 (as an
    /// ambient light of 1 would); a bright top half lights an upward face
    /// more than a downward one.
    #[test]
    fn diffuse_light_from_the_picture() {
        let (w, h) = (64u32, 32u32);
        let even = Environment::new(1, w, h, vec![1.0; (w * h * 3) as usize]);
        for n in [[0.0, -1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, -1.0]] {
            let l = Environment::diffuse(&even.sh, n);
            assert!((l[0] - 1.0).abs() < 0.02, "{n:?}: {l:?}");
        }
        let sky: Vec<f32> = (0..h).flat_map(|y| [if y < h / 2 { 2.0 } else { 0.0 }; 3].repeat(w as usize)).collect();
        let e = Environment::new(2, w, h, sky);
        let up = Environment::diffuse(&e.sh, [0.0, -1.0, 0.0])[0];
        let down = Environment::diffuse(&e.sh, [0.0, 1.0, 0.0])[0];
        assert!(up > 1.6 && down < 0.4 && (Environment::diffuse(&e.sh, [1.0, 0.0, 0.0])[0] - 1.0).abs() < 0.05, "{up} {down}");
        // A bright patch in front (+Z) lights a face turned to it; turned half
        // way round, the patch is behind.
        let patch: Vec<f32> = (0..h).flat_map(|y| (0..w).flat_map(move |x| [if (x as i32 - 32).abs() < 6 && (y as i32 - 16).abs() < 6 { 10.0 } else { 0.0 }; 3])).collect();
        let p = Environment::new(3, w, h, patch);
        let front = Environment::diffuse(&p.turned_sh(0.0), [0.0, 0.0, 1.0])[0];
        let turned = Environment::diffuse(&p.turned_sh(PI), [0.0, 0.0, 1.0])[0];
        assert!(front > turned * 3.0, "{front} {turned}");
        assert!((p.sample([0.0, 0.0, 1.0])[0] - 10.0).abs() < 1e-4);
    }

    #[test]
    fn reads_radiance_files_and_shrinks_them() {
        let f = hdr_file(40, 20, |x, y| if x < 20 { [4.0, 2.0, 1.0] } else { [0.25 * (y as f32 + 1.0) / 20.0, 0.0, 0.0] });
        let (w, h, rgb) = read_hdr(&f, 1024).unwrap();
        assert_eq!((w, h), (40, 20));
        assert!((rgb[0] - 4.0).abs() < 0.05 && (rgb[1] - 2.0).abs() < 0.03 && (rgb[2] - 1.0).abs() < 0.02, "{:?}", &rgb[..3]);
        let (w, h, small) = read_hdr(&f, 10).unwrap();
        assert_eq!((w, h), (10, 5));
        assert!((small[0] - 4.0).abs() < 0.05, "left half averaged: {}", small[0]);
        assert!(read_hdr(b"not a picture", 64).is_none());
        let (sw, sh, s) = shrink(4, 2, &[1.0, 1.0, 1.0, 3.0, 3.0, 3.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 5.0, 5.0, 5.0, 5.0, 5.0, 5.0, f32::NAN, 0.0, 0.0, 0.0, 0.0, 0.0], 2);
        assert_eq!((sw, sh), (2, 1));
        assert_eq!(&s[..3], &[3.5; 3]);
        assert_eq!(s[3], 0.0, "NaN counts as black");
    }

    /// A light clip of the environment type with a panorama lights with it:
    /// turned, tinted by its colour, no shadows; without a picture it adds nothing.
    #[test]
    fn environment_light_clips() {
        use crate::effects::{light as li, Effect, EffectData, EffectKind};
        use crate::space3d::Light;
        let dir = std::env::temp_dir().join(format!("rf_envclip_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("sky.hdr");
        std::fs::write(&p, hdr_file(32, 16, |_, y| if y < 8 { [2.0, 2.0, 2.0] } else { [0.0; 3] })).unwrap();
        let mut fx = Effect::new(EffectKind::Light);
        fx.params[li::KIND].value = li::ENVIRONMENT as f32;
        let bare = Light::from_effect(&fx, crate::Time::ZERO);
        assert!(bare.env.is_none() && bare.reach([0.0; 3], [0.0, -1.0, 0.0]) == [0.0; 3]);
        fx.data = Some(EffectData::Environment(p.clone()));
        fx.params[li::INTENSITY].value = 50.0;
        let l = Light::from_effect(&fx, crate::Time::ZERO);
        let e = l.env.unwrap();
        assert_eq!((l.kind, l.shadow), (li::ENVIRONMENT, 0.0));
        let up = l.reach([0.0; 3], [0.0, -1.0, 0.0])[0];
        assert!((up - 0.5 * Environment::diffuse(&load(&p).unwrap().sh, [0.0, -1.0, 0.0])[0]).abs() < 1e-4, "half intensity: {up}");
        assert_eq!(l.gpu()[0][3], 4.0);
        assert_eq!(l.gpu()[2][3], e.reflect);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_caches_by_file() {
        let dir = std::env::temp_dir().join(format!("rf_env_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("sky.hdr");
        std::fs::write(&p, hdr_file(16, 8, |_, _| [1.0, 1.0, 1.0])).unwrap();
        let a = load(&p).unwrap();
        assert!(Arc::ptr_eq(&a, &load(&p).unwrap()));
        assert!(get(a.id).is_some_and(|g| Arc::ptr_eq(&g, &a)));
        assert!(load(&dir.join("missing.hdr")).is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
