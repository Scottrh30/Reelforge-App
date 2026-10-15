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
