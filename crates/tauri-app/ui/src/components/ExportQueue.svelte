<script lang="ts">
  // File > Export queue: jobs render one after another in the background
  // while you keep editing. Not modal: it floats over the bottom right.
  import { app, run } from "../lib/session.svelte";
  import { api, type JobView } from "../lib/api";

  const q = $derived(app.live?.export ?? null);
  const waiting = $derived(q?.jobs.filter((j) => j.state === "waiting").length ?? 0);
  const over = $derived(q?.jobs.filter((j) => j.state === "done" || j.state === "failed" || j.state === "cancelled").length ?? 0);
  let small = $state(false);

  // Times as Settings → Export says: 1:23, 83 s or 1 min 23 s.
  const fmt = (s: number | null) => {
    if (s == null || !isFinite(s)) return "estimating…";
    const t = Math.max(0, Math.round(s));
    const style = app.s?.prefs.export_time_format ?? 0;
    if (style === 1) return `${t} s`;
    const h = Math.floor(t / 3600);
    const m = Math.floor((t % 3600) / 60);
    const sec = t % 60;
    if (style === 2) return [h ? `${h} h` : "", m ? `${m} min` : "", sec || t === 0 ? `${sec} s` : ""].filter(Boolean).join(" ");
    const two = (n: number) => String(n).padStart(2, "0");
    return h ? `${h}:${two(m)}:${two(sec)}` : `${m}:${two(sec)}`;
  };

  // Settings → Export: a chime when the queue has run through.
  let busy = false;
  $effect(() => {
    const now = !!q?.jobs.some((j) => j.state === "running" || (q.run && j.state === "waiting"));
    if (busy && !now && app.s?.prefs.export_sound) chime();
    busy = now;
  });
  function chime() {
    try {
      const ctx = new AudioContext();
      const at = ctx.currentTime;
      // Two soft bell notes, a fifth apart.
      [659.25, 987.77].forEach((f, i) => {
        const o = ctx.createOscillator();
        const g = ctx.createGain();
        o.type = "sine";
        o.frequency.value = f;
        const t0 = at + i * 0.18;
        g.gain.setValueAtTime(0, t0);
        g.gain.linearRampToValueAtTime(0.25, t0 + 0.01);
        g.gain.exponentialRampToValueAtTime(0.001, t0 + 1.2);
        o.connect(g).connect(ctx.destination);
        o.start(t0);
        o.stop(t0 + 1.25);
      });
      setTimeout(() => ctx.close().catch(() => {}), 2000);
    } catch {}
  }
  const pct = (j: JobView) => (j.total ? Math.round((j.done / j.total) * 100) : 0);
  const job = (id: number, op: "cancel" | "remove" | "retry" | "up") => run(api.exportJob(id, op));
</script>

{#if app.queueOpen && q}
  <section class="queue" class:folded={small} aria-label="Export queue">
    <header>
      <button class="ghost fold" onclick={() => (small = !small)} aria-expanded={!small} title={small ? "Show the jobs" : "Fold it down"}>
        <span class="chev" class:shut={small} aria-hidden="true">▾</span>
        <h2>Export queue</h2>
      </button>
      <span class="muted sum">{q.summary ?? (q.jobs.length ? "All done" : "")}</span>
      <button class="ghost close" onclick={() => (app.queueOpen = false)} aria-label="Close the export queue">✕</button>
    </header>
    {#if !small}
      <div class="jobs">
        {#each q.jobs as j, i (j.id)}
          <div class="job {j.state}">
            <div class="line">
              <span class="title" title={j.file}>{j.title}</span>
              <span class="grow"></span>
              {#if j.state === "running"}
                <button class="small" disabled={j.cancelling} onclick={() => job(j.id, "cancel")}>Cancel</button>
              {:else if j.state === "waiting"}
                {#if i > 0}<button class="small" onclick={() => job(j.id, "up")} title="Export this one sooner" aria-label="Move up">↑</button>{/if}
                <button class="small" onclick={() => job(j.id, "remove")}>Remove</button>
              {:else if j.state === "done"}
                <button class="small" onclick={() => run(api.showInFolder(j.file))}>Show in folder</button>
                <button class="small" onclick={() => job(j.id, "remove")} aria-label="Remove from the list">✕</button>
              {:else}
                <button class="small" onclick={() => job(j.id, "retry")}>Retry</button>
                <button class="small" onclick={() => job(j.id, "remove")} aria-label="Remove from the list">✕</button>
              {/if}
            </div>
            {#if j.state === "running"}
              <div class="bar"><div class="fill" style="width:{pct(j)}%"></div></div>
              <span class="detail tc">
                {j.cancelling ? "Cancelling…" : [`${pct(j)}%`, `frame ${j.done} of ${j.total}`, j.fps ? `${j.fps.toFixed(1)} fps` : "", j.elapsed != null ? `${fmt(j.elapsed)} so far` : "", `${fmt(j.remaining)} left`].filter(Boolean).join(" · ")}
              </span>
            {:else if j.state === "done"}
              <span class="detail good">Done · {j.message}{j.elapsed != null ? ` · took ${fmt(j.elapsed)}` : ""}</span>
            {:else if j.state === "failed"}
              <span class="detail bad">Failed: {j.message}</span>
            {:else}
              <span class="detail">{j.detail} · {j.state === "waiting" ? (q.run ? "next up" : "waiting") : "cancelled"}</span>
            {/if}
          </div>
        {:else}
          <p class="muted empty">Nothing queued. Use File › Export… and “Add to queue”.</p>
        {/each}
      </div>
      <footer>
        {#if q.run && q.jobs.some((j) => j.state === "running")}
          <button onclick={() => run(api.exportRun(false))} title="Finish the current export, then wait">Pause after this one</button>
        {:else}
          <button class="primary" disabled={waiting === 0} onclick={() => run(api.exportRun(true))}>Start queue{waiting ? ` (${waiting})` : ""}</button>
        {/if}
        <button disabled={over === 0} onclick={() => run(api.exportClear())}>Clear finished</button>
        <span class="grow"></span>
        <button onclick={() => (app.exportOpen = true)}>Export…</button>
      </footer>
    {/if}
  </section>
{/if}

<style>
  .queue {
    position: fixed;
    right: 14px;
    bottom: 34px;
    width: min(460px, calc(100vw - 28px));
    max-height: min(480px, calc(100vh - 90px));
    background: var(--panel);
    border: 1px solid var(--line);
    border-radius: 8px;
    box-shadow: 0 14px 40px rgba(0, 0, 0, 0.55);
    display: flex;
    flex-direction: column;
    z-index: 30;
  }
  header {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 8px 6px 6px;
    border-bottom: 1px solid var(--line-soft);
  }
  .queue.folded header {
    border-bottom: 0;
  }
  .fold {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 2px 6px;
  }
  .chev {
    color: var(--muted);
    transition: transform 0.12s;
  }
  .chev.shut {
    transform: rotate(-90deg);
  }
  h2 {
    font-size: calc(13.5px * var(--text-scale));
    font-weight: 600;
    margin: 0;
  }
  .sum {
    flex: 1;
    font-size: calc(12px * var(--text-scale));
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .jobs {
    overflow: auto;
    padding: 8px;
    display: flex;
    flex-direction: column;
    gap: 6px;
    min-height: 0;
  }
  .job {
    border: 1px solid var(--line-soft);
    border-radius: 6px;
    background: var(--base);
    padding: 7px 9px;
    display: flex;
    flex-direction: column;
    gap: 5px;
  }
  .job.running {
    border-color: color-mix(in srgb, var(--accent) 45%, var(--line-soft));
  }
  .line {
    display: flex;
    align-items: center;
    gap: 5px;
  }
  .title {
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    min-width: 0;
  }
  .grow {
    flex: 1;
  }
  .detail {
    font-size: calc(12px * var(--text-scale));
    color: var(--muted);
  }
  .good {
    color: #6ec878;
  }
  .bad {
    color: var(--red);
  }
  .bar {
    height: 6px;
    background: var(--well);
    border-radius: 3px;
    overflow: hidden;
  }
  .fill {
    height: 100%;
    background: var(--accent);
    transition: width 0.25s;
  }
  .small {
    min-height: 22px;
    padding: 0 7px;
    font-size: calc(12px * var(--text-scale));
  }
  footer {
    display: flex;
    gap: 6px;
    padding: 8px;
    border-top: 1px solid var(--line-soft);
  }
  .muted {
    color: var(--muted);
  }
  .empty {
    margin: 6px 2px;
  }
</style>
