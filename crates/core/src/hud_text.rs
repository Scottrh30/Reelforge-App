//! A HUD element's readout: the user's own text beside it, with live values
//! filled in, drawn on the CPU (in Hack, a monospaced font fit for readouts)
//! into a table the GPU places by the element.
//!
//! Live values, written in braces:
//!
//! | | |
//! |---|---|
//! | `{x}` `{y}` | where the element's centre is, in pixels of the layer |
//! | `{size}` | its size, in pixels |
//! | `{time}` | the clip's time, in seconds |
//! | `{rand}` | a four-digit number that keeps changing |
//! | `{lock}` | SEARCHING, LOCKING or LOCKED, as the lock-on goes (TRACKING without one) |
//! | `{lock%}` | how far the lock-on has got |

use crate::effects::{hud, EffectValues};
use crate::lut::Table;
use ab_glyph::{point, Font, FontRef, PxScale, ScaleFont};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

/// The longest readout drawn (characters), and the biggest picture of it.
const MAX_CHARS: usize = 2000;
const MAX_SIDE: u32 = 4096;

/// How far the lock-on has got (0–1) at clip time `t`, and whether there is one.
pub fn lock_progress(v: &[f32], t: f32) -> (bool, f32) {
    let on = v.get(hud::LOCK_ON).copied().unwrap_or(0.0) >= 0.5;
    let at = v.get(hud::LOCK_AT).copied().unwrap_or(0.0);
    let dur = v.get(hud::LOCK_TIME).copied().unwrap_or(1.0).max(0.01);
    (on, ((t - at) / dur).clamp(0.0, 1.0))
}

/// `template` with its live values filled in, for an element with values `v` on a layer
/// `size` pixels big, at clip time `t` (seconds).
pub fn expand(template: &str, v: &[f32], t: f32, size: [f32; 2]) -> String {
    let get = |i: usize| v.get(i).copied().unwrap_or(0.0);
    let px = |f: f32| if f.is_finite() { format!("{:04}", f.round().clamp(-99_999.0, 999_999.0) as i64) } else { "----".into() };
    let (lock_on, k) = lock_progress(v, t);
    let lock_at = get(hud::LOCK_AT);
    let lock = if !lock_on {
        "TRACKING"
    } else if t < lock_at {
        "SEARCHING"
    } else if k < 1.0 {
        "LOCKING"
    } else {
        "LOCKED"
    };
    // A new number eight times a second, its own for each seed.
    let tick = if t.is_finite() { (t * 8.0).floor().clamp(-1e9, 1e9) as i64 } else { 0 };
    let seed = get(hud::SEED).clamp(0.0, 1e6) as u64;
    let rand = {
        let mut h = (tick as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ seed.wrapping_mul(0xBF58_476D_1CE4_E5B9);
        h ^= h >> 31;
        h = h.wrapping_mul(0x94D0_49BB_1331_11EB);
        h ^= h >> 29;
        format!("{:04}", h % 10_000)
    };
    let mut out = String::new();
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(rest.get(..open).unwrap_or(""));
        let after = rest.get(open..).unwrap_or("");
        let Some(close) = after.find('}') else {
            out.push_str(after);
            rest = "";
            break;
        };
        let name = after.get(1..close).unwrap_or("");
        let value = match name.trim().to_ascii_lowercase().as_str() {
            "x" => Some(px(get(hud::X) / 100.0 * size[0])),
            "y" => Some(px(get(hud::Y) / 100.0 * size[1])),
            "size" => Some(px(get(hud::SIZE) / 100.0 * size[1])),
            "time" => Some(if t.is_finite() { format!("{:.2}", t.clamp(-1e7, 1e7)) } else { "--".into() }),
            "rand" => Some(rand.clone()),
            "lock" => Some(lock.into()),
            "lock%" => Some(format!("{:3.0}%", if lock_on { k * 100.0 } else { 100.0 })),
            _ => None,
        };
        match value {
            Some(s) => out.push_str(&s),
            // Not a value: left as written.
            None => out.push_str(after.get(..=close).unwrap_or("")),
        }
        rest = after.get(close + 1..).unwrap_or("");
        if out.len() > MAX_CHARS * 4 {
            break;
        }
    }
    out.push_str(rest);
    out.chars().take(MAX_CHARS).collect()
}

// (The font's bytes are compiled in: it can't fail at run time, and the tests load it.)
fn font() -> Option<&'static FontRef<'static>> {
    static F: OnceLock<Option<FontRef<'static>>> = OnceLock::new();
    F.get_or_init(|| FontRef::try_from_slice(epaint_default_fonts::HACK_REGULAR).ok()).as_ref()
}

/// `text` drawn white on clear, its lines `px` pixels apart: the table and its size.
/// Kept for reuse (the same text at the same size isn't drawn twice).
pub fn table(text: &str, px: f32) -> Option<Arc<Table>> {
    if text.trim().is_empty() || !px.is_finite() {
        return None;
    }
    let px = px.clamp(4.0, 400.0);
    type Cache = Mutex<HashMap<(String, u32), Arc<Table>>>;
    static CACHE: OnceLock<Cache> = OnceLock::new();
    let key = (text.to_string(), (px * 4.0).round() as u32);
    let mut cache = CACHE.get_or_init(Default::default).lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(t) = cache.get(&key) {
        return Some(t.clone());
    }
    let t = Arc::new(draw(text, px)?);
    if cache.len() >= 64 {
        cache.clear();
    }
    cache.insert(key, t.clone());
    Some(t)
}

fn draw(text: &str, px: f32) -> Option<Table> {
    let font = font()?;
    let scaled = font.as_scaled(PxScale::from(px));
    let line_h = (scaled.ascent() - scaled.descent() + scaled.line_gap()).max(1.0);
    let lines: Vec<&str> = text.lines().collect();
    let widths: Vec<f32> = lines.iter().map(|l| l.chars().map(|c| scaled.h_advance(font.glyph_id(c))).sum::<f32>()).collect();
    let pad = (px * 0.15).ceil();
    let w = (widths.iter().copied().fold(0.0f32, f32::max) + 2.0 * pad).ceil().clamp(1.0, MAX_SIDE as f32) as u32;
    let h = (lines.len().max(1) as f32 * line_h + 2.0 * pad).ceil().clamp(1.0, MAX_SIDE as f32) as u32;
    let mut rgba = vec![0u8; (w as usize) * (h as usize) * 4];
    for (row, line) in lines.iter().enumerate() {
        let mut x = pad;
        let base = pad + scaled.ascent() + row as f32 * line_h;
        for c in line.chars() {
            let id = font.glyph_id(c);
            let g = id.with_scale_and_position(PxScale::from(px), point(x, base));
            x += scaled.h_advance(id);
            let Some(o) = font.outline_glyph(g) else { continue };
            let b = o.px_bounds();
            o.draw(|gx, gy, cov| {
                let (px_x, px_y) = (b.min.x as i64 + gx as i64, b.min.y as i64 + gy as i64);
                if px_x < 0 || px_y < 0 || px_x >= w as i64 || px_y >= h as i64 {
                    return;
                }
                let i = ((px_y as usize) * (w as usize) + px_x as usize) * 4;
                if let Some(p) = rgba.get_mut(i..i + 4) {
                    let a = (cov.clamp(0.0, 1.0) * 255.0).round() as u8;
                    let a = a.max(p.get(3).copied().unwrap_or(0));
                    p.copy_from_slice(&[255, 255, 255, a]);
                }
            });
        }
    }
    let mut hsh = std::collections::hash_map::DefaultHasher::new();
    std::hash::Hash::hash(&(text, (px * 4.0).round() as u32, 0x0048_5544_u32), &mut hsh);
    Some(Table { id: std::hash::Hasher::finish(&hsh), width: w, height: h, rgba, cube_size: 0 })
}

/// Fill in a HUD element's readout: its table, and for the GPU (spare slots) the
/// picture's size in layer pixels.
pub fn fill(v: &mut EffectValues, template: &str, size: [f32; 2]) {
    let px = v.values.get(hud::TEXT_SIZE).copied().unwrap_or(4.0) / 100.0 * size[1].max(1.0);
    let text = expand(template, &v.values, v.time, size);
    match table(&text, px) {
        Some(tb) => {
            // Drawn at the layer's own pixels (clamped): its size there.
            let k = px / px.clamp(4.0, 400.0);
            v.gpu = [tb.width as f32 * k, tb.height as f32 * k, 0.0, 0.0];
            v.table = Some(tb);
        }
        None => v.table = None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(set: &[(usize, f32)]) -> Vec<f32> {
        let e = crate::effects::Effect::new(crate::effects::EffectKind::Hud);
        let mut v: Vec<f32> = e.params.iter().map(|p| p.value).collect();
        for &(i, x) in set {
            v[i] = x;
        }
        v
    }

    #[test]
    fn fills_in_live_values() {
        let v = values(&[(hud::X, 25.0), (hud::Y, 50.0)]);
        assert_eq!(expand("TGT {x},{y}", &v, 1.0, [1920.0, 1080.0]), "TGT 0480,0540");
        assert_eq!(expand("T+{time}s", &v, 2.5, [100.0, 100.0]), "T+2.50s");
        // Unknown or unclosed braces are left alone.
        assert_eq!(expand("{nope} {x", &v, 0.0, [100.0, 100.0]), "{nope} {x");
        // The random number changes, and is four digits.
        let a = expand("{rand}", &v, 0.0, [1.0, 1.0]);
        let b = expand("{rand}", &v, 0.5, [1.0, 1.0]);
        assert_eq!(a.len(), 4);
        assert_ne!(a, b);
        assert_eq!(a, expand("{rand}", &v, 0.0, [1.0, 1.0]), "the same at the same moment");
    }

    #[test]
    fn says_how_the_lock_on_goes() {
        assert_eq!(expand("{lock}", &values(&[]), 1.0, [1.0, 1.0]), "TRACKING");
        let v = values(&[(hud::LOCK_ON, 1.0), (hud::LOCK_AT, 1.0), (hud::LOCK_TIME, 2.0)]);
        assert_eq!(expand("{lock}", &v, 0.5, [1.0, 1.0]), "SEARCHING");
        assert_eq!(expand("{lock} {lock%}", &v, 2.0, [1.0, 1.0]), "LOCKING  50%");
        assert_eq!(expand("{lock}", &v, 3.5, [1.0, 1.0]), "LOCKED");
    }

    #[test]
    fn draws_the_text() {
        let t = table("LOCK 42", 20.0).expect("drawn");
        assert!(t.width > 60 && t.height >= 20, "{}×{}", t.width, t.height);
        assert!(t.rgba.chunks_exact(4).any(|p| p[3] > 200), "ink");
        assert!(t.rgba.chunks_exact(4).any(|p| p[3] == 0), "and clear between");
        // Two lines are taller; the same text comes from the cache.
        assert!(table("A\nB", 20.0).unwrap().height > table("A", 20.0).unwrap().height);
        assert!(Arc::ptr_eq(&t, &table("LOCK 42", 20.0).unwrap()));
        assert!(table("   ", 20.0).is_none());
        // Huge, tiny or odd sizes and long text do no harm.
        for px in [f32::NAN, 0.0, -5.0, 1e9] {
            let _ = table("X", px);
        }
        let long = "W".repeat(5000);
        let t = table(&expand(&long, &values(&[]), 0.0, [1.0, 1.0]), 400.0).unwrap();
        assert!(t.width <= MAX_SIDE && t.height <= MAX_SIDE);
    }
}
