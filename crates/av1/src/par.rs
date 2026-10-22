//! Optional data parallelism (rayon behind the `threads` feature; sequential otherwise and on
//! wasm32).

/// Whether to spread work over the process-wide worker pool (rayon's global
/// one, shared by every open decoder) or run it on the calling thread. The work
/// here is fork-join with no waiting inside, so sharing is safe.
#[derive(Clone, Default)]
pub(crate) struct Pool {
    #[cfg(feature = "threads")]
    parallel: bool,
}

impl Pool {
    /// A pool of `threads` workers (sequential when `threads` <= 1, without the `threads`
    /// feature, or on wasm32).
    pub fn new(threads: usize) -> Pool {
        #[cfg(feature = "threads")]
        {
            Pool { parallel: threads > 1 && cfg!(not(target_arch = "wasm32")) }
        }
        #[cfg(not(feature = "threads"))]
        {
            let _ = threads;
            Pool {}
        }
    }

    pub fn is_parallel(&self) -> bool {
        #[cfg(feature = "threads")]
        {
            self.parallel
        }
        #[cfg(not(feature = "threads"))]
        {
            false
        }
    }

    /// `f(i, &mut items[i])` for every item, in parallel when possible; results in order.
    pub fn map_mut<T: Send, R: Send>(&self, items: &mut [T], f: impl Fn(usize, &mut T) -> R + Sync + Send) -> Vec<R> {
        #[cfg(feature = "threads")]
        if self.parallel && items.len() > 1 {
            use rayon::prelude::*;
            return items.par_iter_mut().enumerate().map(|(i, t)| f(i, t)).collect();
        }
        items.iter_mut().enumerate().map(|(i, t)| f(i, t)).collect()
    }

    /// `f(i, chunk)` for the consecutive `chunk_len`-element chunks of `data`.
    pub fn chunks<T: Send>(&self, data: &mut [T], chunk_len: usize, f: impl Fn(usize, &mut [T]) + Sync + Send) {
        let chunk_len = chunk_len.max(1);
        #[cfg(feature = "threads")]
        if self.parallel && data.len() > chunk_len {
            use rayon::prelude::*;
            data.par_chunks_mut(chunk_len).enumerate().for_each(|(i, c)| f(i, c));
            return;
        }
        for (i, c) in data.chunks_mut(chunk_len).enumerate() {
            f(i, c);
        }
    }
}
