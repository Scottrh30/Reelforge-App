//! Faces followed through a clip (found by the Face beauty effect's Find
//! faces), and the table the GPU reads them from.

use crate::lut::Table;
use crate::time::Time;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Where a face is at a moment: its middle and size, as fractions of the layer.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct FaceKey {
    /// Source time.
    pub at: Time,
    /// Middle x, y; width, height.
    pub rect: [f32; 4],
}

/// One face, followed from where it's first seen to where it's last.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FaceTrack {
    /// Unticked faces are left as they are.
    #[serde(default = "yes")]
    pub on: bool,
    pub keys: Vec<FaceKey>,
}

fn yes() -> bool {
    true
}

/// At most this many faces are drawn at once.
pub const MAX: usize = 32;

impl FaceTrack {
    /// Where it is at `t` (between its keys; for half a key's spacing past its ends), if it's there.
    pub fn at(&self, t: Time) -> Option<[f32; 4]> {
        let k = &self.keys;
        let (first, last) = (k.first()?, k.last()?);
        let gap = if k.len() > 1 { (last.at.0 - first.at.0) / (k.len() as i64 - 1) } else { Time::from_secs_f64(0.1).0 };
        if t.0 < first.at.0 - gap / 2 || t.0 > last.at.0 + gap / 2 {
            return None;
        }
        let i = k.partition_point(|x| x.at <= t);
        if i == 0 {
            return Some(first.rect);
        }
        if i == k.len() {
            return Some(last.rect);
        }
        let (a, b) = (&k[i - 1], &k[i]);
        let u = (t.0 - a.at.0) as f32 / (b.at.0 - a.at.0).max(1) as f32;
        Some([0, 1, 2, 3].map(|j| a.rect[j] + (b.rect[j] - a.rect[j]) * u))
    }
}

/// The faces showing at `t` as a table for the GPU: two texels a face
/// (middle x, y; half width, half height, as 16-bit fractions of the layer),
/// and how many.
pub fn table(tracks: &[FaceTrack], t: Time) -> (Arc<Table>, usize) {
    let faces: Vec<[f32; 4]> = tracks.iter().filter(|f| f.on).filter_map(|f| f.at(t)).take(MAX).collect();
    let width = (faces.len() * 2).max(1);
    let mut rgba = vec![0u8; width * 4];
    let q = |v: f32| ((v.clamp(-0.5, 1.5) + 0.5) / 2.0 * 65535.0).round() as u16;
    for (i, f) in faces.iter().enumerate() {
        let vals = [q(f[0]), q(f[1]), q(f[2] / 2.0), q(f[3] / 2.0)];
        for (k, v) in vals.iter().enumerate() {
            let o = i * 8 + k * 2;
            rgba[o..o + 2].copy_from_slice(&v.to_be_bytes());
        }
    }
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    rgba.hash(&mut h);
    (Arc::new(Table { id: h.finish() | 1 << 60, width: width as u32, height: 1, rgba, cube_size: 0 }), faces.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn faces_are_where_their_keys_put_them() {
        let s = Time::from_secs_f64;
        let f = FaceTrack { on: true, keys: vec![FaceKey { at: s(1.0), rect: [0.2, 0.3, 0.1, 0.2] }, FaceKey { at: s(1.1), rect: [0.4, 0.3, 0.1, 0.2] }] };
        assert_eq!(f.at(s(0.5)), None);
        assert!((f.at(s(1.05)).unwrap()[0] - 0.3).abs() < 1e-4);
        assert_eq!(f.at(s(1.12)).unwrap()[0], 0.4);
        assert_eq!(f.at(s(1.3)), None);
        let (t, n) = table(&[f.clone(), FaceTrack { on: false, ..f }], s(1.0));
        assert_eq!((n, t.width), (1, 2));
        let (_, n) = table(&[], s(1.0));
        assert_eq!(n, 0);
    }
}
