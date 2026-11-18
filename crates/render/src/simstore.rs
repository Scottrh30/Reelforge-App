//! Simulation checkpoints kept on disk, so reopening a project (or exporting
//! it) doesn't work its water and fluid out again. The simulations keep a
//! checkpoint every second of simulated time on the graphics card; with a
//! store set, each is also read back and handed to it, and a simulation
//! starting afresh (or going back past what's in memory) looks there first.
//!
//! The store itself (where, how much) is the app's: see
//! `reelforge_engine::render_cache::DiskStore`. A checkpoint's key covers
//! everything its contents depend on: the simulation's own key (its settings,
//! grid and layer), its step, its buffers' sizes, and the shader code, so
//! saved results from another version of the simulation are never loaded.

use std::sync::{Arc, PoisonError, RwLock};

/// Where checkpoints are kept.
pub trait SimulationStore: Send + Sync {
    fn has(&self, key: u64) -> bool;
    fn load(&self, key: u64) -> Option<Vec<u8>>;
    /// Keep `bytes` under `key` (in the background is fine).
    fn save(&self, key: u64, bytes: Vec<u8>);
}

static STORE: RwLock<Option<Arc<dyn SimulationStore>>> = RwLock::new(None);

/// Keep simulation checkpoints in `store` (`None`: in memory only), for every
/// compositor from now on.
pub fn set_simulation_store(store: Option<Arc<dyn SimulationStore>>) {
    *STORE.write().unwrap_or_else(PoisonError::into_inner) = store;
}

pub(crate) fn store() -> Option<Arc<dyn SimulationStore>> {
    STORE.read().unwrap_or_else(PoisonError::into_inner).clone()
}

/// A checkpoint's key (see the module's notes). `shader` is the simulation's
/// shader text.
pub(crate) fn checkpoint_key(sim: u64, step: usize, lens: &[u64], shader: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (b"reelforge simulation checkpoint 1", sim, step, lens, shader).hash(&mut h);
    h.finish()
}

/// The buffers' contents as one blob: how many, each one's length, then each.
pub(crate) fn pack(parts: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + parts.len() * 8 + parts.iter().map(Vec::len).sum::<usize>());
    out.extend_from_slice(&(parts.len() as u64).to_le_bytes());
    for p in parts {
        out.extend_from_slice(&(p.len() as u64).to_le_bytes());
    }
    for p in parts {
        out.extend_from_slice(p);
    }
    out
}

/// A blob back as its buffers, if it holds exactly buffers of `lens` bytes.
pub(crate) fn unpack<'a>(bytes: &'a [u8], lens: &[u64]) -> Option<Vec<&'a [u8]>> {
    let word = |i: usize| bytes.get(i * 8..i * 8 + 8).and_then(|b| b.try_into().ok()).map(u64::from_le_bytes);
    if word(0)? != lens.len() as u64 {
        return None;
    }
    let mut at = 8usize.checked_add(lens.len().checked_mul(8)?)?;
    let mut out = Vec::with_capacity(lens.len());
    for (i, &want) in lens.iter().enumerate() {
        let n = word(i + 1)?;
        if n != want {
            return None;
        }
        let n = usize::try_from(n).ok()?;
        out.push(bytes.get(at..at.checked_add(n)?)?);
        at += n;
    }
    (at == bytes.len()).then_some(out)
}

/// Read buffers back from the graphics card (waits for it).
pub(crate) fn read_back(device: &wgpu::Device, queue: &wgpu::Queue, bufs: &[(&wgpu::Buffer, u64)]) -> Option<Vec<Vec<u8>>> {
    let mut enc = device.create_command_encoder(&Default::default());
    let staging: Vec<wgpu::Buffer> = bufs
        .iter()
        .map(|(b, len)| {
            let s = device.create_buffer(&wgpu::BufferDescriptor { label: Some("checkpoint read"), size: (*len).max(4), usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ, mapped_at_creation: false });
            enc.copy_buffer_to_buffer(b, 0, &s, 0, *len);
            s
        })
        .collect();
    queue.submit([enc.finish()]);
    let (tx, rx) = std::sync::mpsc::channel();
    for (i, s) in staging.iter().enumerate() {
        let tx = tx.clone();
        s.slice(..).map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send((i, r.is_ok()));
        });
    }
    drop(tx);
    device.poll(wgpu::PollType::wait_indefinitely()).ok()?;
    if rx.iter().take(staging.len()).filter(|(_, ok)| *ok).count() != staging.len() {
        return None;
    }
    let mut out = Vec::with_capacity(bufs.len());
    for (s, (_, len)) in staging.iter().zip(bufs) {
        let data = s.slice(..).get_mapped_range().ok()?;
        out.push(data.get(..*len as usize)?.to_vec());
        drop(data);
        s.unmap();
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blobs_come_back_only_as_what_they_were() {
        let parts = vec![vec![1u8, 2, 3, 4], vec![], vec![9u8; 8]];
        let b = pack(&parts);
        let back = unpack(&b, &[4, 0, 8]).unwrap();
        assert_eq!(back, vec![&[1u8, 2, 3, 4][..], &[][..], &[9u8; 8][..]]);
        // Other sizes, a cut-off blob, junk: nothing.
        assert!(unpack(&b, &[4, 0, 7]).is_none());
        assert!(unpack(&b, &[4, 0]).is_none());
        assert!(unpack(&b[..b.len() - 1], &[4, 0, 8]).is_none());
        assert!(unpack(&[0xff; 40], &[4]).is_none());
        assert!(unpack(&[], &[]).is_none());
        // The key changes with each thing it covers.
        let k = checkpoint_key(1, 60, &[4, 8], "a");
        assert_ne!(k, checkpoint_key(2, 60, &[4, 8], "a"));
        assert_ne!(k, checkpoint_key(1, 120, &[4, 8], "a"));
        assert_ne!(k, checkpoint_key(1, 60, &[4, 9], "a"));
        assert_ne!(k, checkpoint_key(1, 60, &[4, 8], "b"));
    }
}
