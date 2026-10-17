// Live edits while dragging: send them one at a time, always the newest, so a
// fast drag never queues up stale calls. `done()` waits for the last one.

export function livePump() {
  let latest: (() => Promise<unknown>) | null = null;
  let running: Promise<void> | null = null;
  const drain = async () => {
    while (latest) {
      const f = latest;
      latest = null;
      try {
        await f();
      } catch {
        // The next state refresh shows what happened.
      }
    }
    running = null;
  };
  return {
    push(f: () => Promise<unknown>) {
      latest = f;
      if (!running) running = drain();
    },
    async done() {
      while (running) await running;
    },
  };
}
