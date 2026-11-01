// Small pictures of media, decoded on the Rust side in the background and kept here.

import { api, type Ticks } from "./api";

const cache = new Map<string, Promise<string | null>>();
let queue: Promise<unknown> = Promise.resolve();

/// An object URL for a JPEG of `path` at `at`, or null if it has no picture there.
/// Requests run one after another so a long timeline doesn't flood the decoder.
export function thumb(path: string, at: Ticks): Promise<string | null> {
  const key = `${path}@${at}`;
  let p = cache.get(key);
  if (!p) {
    p = queue.then(async () => {
      try {
        const buf = await api.thumbnail(path, Math.round(at));
        return URL.createObjectURL(new Blob([buf], { type: "image/jpeg" }));
      } catch {
        return null;
      }
    });
    queue = p;
    cache.set(key, p);
  }
  return p;
}

// Compositions' pictures: rendered on the Rust side (which keeps them until
// what's in them changes); here, the latest asked for at each time.
const comps = new Map<string, { rev: number; p: Promise<string | null> }>();

/// An object URL for a JPEG of composition `seq` at its time `at`, as of
/// project revision `rev` (a newer `rev` asks again; an unchanged picture
/// comes straight back from the cache there).
export function compThumb(seq: number, at: Ticks, rev: number): Promise<string | null> {
  const key = `${seq}@${at}`;
  const had = comps.get(key);
  if (had && had.rev === rev) return had.p;
  const p = queue.then(async () => {
    try {
      const buf = await api.compThumbnail(seq, Math.round(at));
      return URL.createObjectURL(new Blob([buf], { type: "image/jpeg" }));
    } catch {
      return null;
    }
  });
  queue = p;
  comps.set(key, { rev, p });
  // The older picture's URL is let go once the new one is in.
  if (had) p.then(() => had.p.then((u) => u && setTimeout(() => URL.revokeObjectURL(u), 2000)));
  return p;
}
