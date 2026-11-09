<script lang="ts">
  // Match colour… (a clip's right-click menu): take the look of a
  // reference — another clip as it's graded, something in the Media panel,
  // or a picture or video file — and give it to the selected clips, as a
  // LUT effect at the end of each one's effects (one undo step).
  import { app, run } from "../lib/session.svelte";
  import { api, type ClipView } from "../lib/api";

  type From = "clip" | "media" | "file";
  // Remembered between openings.
  let from = $state<From>("clip");
  let refClip = $state<number | null>(null);
  let refMedia = $state<number | null>(null);
  let file = $state<string | null>(null);
  let tone = $state(true);
  let color = $state(true);
  // Started from here (closes when it's done).
  let started = $state(false);

  const PICTURE = new Set(["video", "image", "comp"]);
  const clips = $derived.by(() => {
    const out: ClipView[] = [];
    for (const t of app.s?.seq.tracks ?? []) if (t.kind === "video") for (const c of t.clips) if (PICTURE.has(c.kind)) out.push(c);
    return out.sort((a, b) => a.start - b.start);
  });
  const targets = $derived(clips.filter((c) => app.s?.selected.includes(c.id)));
  const others = $derived(clips.filter((c) => !app.s?.selected.includes(c.id)));
  const media = $derived((app.s?.media ?? []).filter((m) => (m.kind === "video" || m.kind === "image") && !m.missing));
  const tps = $derived(app.s?.ticks_per_second ?? 1);
  const at = (t: number) => {
    const s = t / tps;
    return `${Math.floor(s / 60)}:${(s % 60).toFixed(1).padStart(4, "0")}`;
  };

  $effect(() => {
    if (!app.matchColorOpen) return;
    started = false;
    // A sensible reference to start from: the nearest other clip.
    if (refClip == null || !others.some((c) => c.id === refClip)) refClip = others[0]?.id ?? null;
    if (refMedia == null || !media.some((m) => m.id === refMedia)) refMedia = media[0]?.id ?? null;
    if (from === "clip" && !others.length) from = media.length ? "media" : "file";
  });
  // Done (or stopped): close.
  $effect(() => {
    if (started && app.live && app.live.matching == null) {
      started = false;
      app.matchColorOpen = false;
    }
  });

  const ready = $derived(targets.length > 0 && (tone || color) && (from === "clip" ? refClip != null : from === "media" ? refMedia != null : !!file));
  const busy = $derived(app.live?.matching != null);
  async function browse() {
    const p = await api.colorMatchPick();
    if (p) {
      file = p;
      from = "file";
    }
  }
  async function go() {
    if (!ready || busy) return;
    const id = from === "clip" ? refClip! : from === "media" ? refMedia! : 0;
    started = true;
    await run(
      api.colorMatch(
        targets.map((c) => c.id),
        from,
        id,
        from === "file" ? file : null,
        tone,
        color,
      ),
    );
  }
  function close() {
    if (busy) run(api.colorMatchCancel());
    started = false;
    app.matchColorOpen = false;
  }
  function key(e: KeyboardEvent) {
    if (!app.matchColorOpen) return;
    if (e.key === "Escape") {
      e.stopPropagation();
      close();
    } else if (e.key === "Enter" && !(e.target instanceof HTMLSelectElement)) {
      e.stopPropagation();
      go();
    }
  }
  const fileName = (p: string) => p.split(/[\\/]/).pop() ?? p;
</script>

<svelte:window onkeydown={key} />

{#if app.matchColorOpen}
  <div class="scrim">
    <div class="dialog" role="dialog" aria-modal="true" aria-labelledby="mc-title">
      <h2 id="mc-title">Match colour</h2>
      <p class="muted">
        {#if targets.length}
          Give {targets.length === 1 ? `“${targets[0].name}”` : `the ${targets.length} selected clips`} the look of:
        {:else}
          Select the video or picture clips to change first, then choose what to match them to.
        {/if}
      </p>
      <div class="choices" role="radiogroup" aria-label="Match to">
        <label class="choice" class:off={!others.length}>
          <input type="radio" name="mc-from" value="clip" bind:group={from} disabled={!others.length} />
          <span>Another clip</span>
          <select bind:value={refClip} disabled={!others.length} onfocus={() => (from = "clip")} aria-label="Reference clip">
            {#each others as c (c.id)}<option value={c.id}>{c.name} — at {at(c.start)}</option>{/each}
            {#if !others.length}<option value={null}>No other clips</option>{/if}
          </select>
        </label>
        <label class="choice" class:off={!media.length}>
          <input type="radio" name="mc-from" value="media" bind:group={from} disabled={!media.length} />
          <span>From the Media panel</span>
          <select bind:value={refMedia} disabled={!media.length} onfocus={() => (from = "media")} aria-label="Reference media">
            {#each media as m (m.id)}<option value={m.id}>{m.name}</option>{/each}
            {#if !media.length}<option value={null}>Nothing imported</option>{/if}
          </select>
        </label>
        <div class="choice">
          <input type="radio" name="mc-from" value="file" bind:group={from} id="mc-file" disabled={!file} />
          <label for="mc-file">A picture or video file</label>
          <span class="file" title={file ?? ""}>{file ? fileName(file) : ""}</span>
          <button type="button" onclick={browse}>Browse…</button>
        </div>
      </div>
      <div class="what">
        <span class="muted">Match</span>
        <label class="check"><input type="checkbox" bind:checked={color} /> Colour</label>
        <label class="check"><input type="checkbox" bind:checked={tone} /> Brightness and contrast</label>
      </div>
      <p class="muted small">
        {from === "clip" ? "The clip is measured as it's graded, effects and all. " : ""}Each clip gets a LUT effect at the end of its effects: its
        <b>Strength</b> sets how far to go, and matching again replaces it.
      </p>
      {#if busy}
        <div class="progress" role="progressbar" aria-valuemin={0} aria-valuemax={100} aria-valuenow={Math.round((app.live?.matching ?? 0) * 100)}>
          <div style="width:{Math.round((app.live?.matching ?? 0) * 100)}%"></div>
        </div>
      {/if}
      <div class="buttons">
        <span class="grow"></span>
        <button type="button" onclick={close}>{busy ? "Stop" : "Cancel"}</button>
        <button type="button" class="primary" disabled={!ready || busy} onclick={go}>{busy ? "Matching…" : "Match"}</button>
      </div>
    </div>
  </div>
{/if}

<style>
  .scrim {
    position: fixed;
    inset: 0;
    background: rgba(10, 10, 12, 0.45);
    display: grid;
    place-items: center;
    z-index: 40;
  }
  .dialog {
    width: min(500px, calc(100vw - 32px));
    max-height: calc(100vh - 32px);
    overflow: auto;
    background: var(--panel);
    border: 1px solid var(--line);
    border-radius: 8px;
    padding: 16px 18px;
    box-shadow: 0 20px 60px rgba(0, 0, 0, 0.6);
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  h2 {
    font-size: calc(16px * var(--text-scale));
    font-weight: 600;
    margin: 0;
  }
  p {
    margin: 0;
  }
  b {
    font-weight: 600;
  }
  .muted {
    color: var(--muted);
  }
  .small {
    font-size: calc(12px * var(--text-scale));
  }
  .choices {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .choice {
    display: grid;
    grid-template-columns: auto 150px minmax(0, 1fr) auto;
    gap: 8px;
    align-items: center;
  }
  .choice select {
    grid-column: 3 / 5;
    min-width: 0;
  }
  .choice.off {
    opacity: 0.55;
  }
  .file {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--muted);
  }
  .what {
    display: flex;
    flex-wrap: wrap;
    gap: 6px 14px;
    align-items: center;
  }
  .check {
    display: flex;
    gap: 6px;
    align-items: center;
  }
  .progress {
    height: 6px;
    border-radius: 3px;
    background: var(--line);
    overflow: hidden;
  }
  .progress div {
    height: 100%;
    background: var(--accent);
    transition: width 0.2s;
  }
  .buttons {
    display: flex;
    gap: 8px;
    align-items: center;
  }
  .grow {
    flex: 1;
  }
  @media (max-width: 480px) {
    .choice {
      grid-template-columns: auto 1fr;
    }
    .choice select {
      grid-column: 1 / 3;
    }
  }
</style>
