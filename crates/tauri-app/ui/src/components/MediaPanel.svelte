<script lang="ts">
  import { app, run, shortTime } from "../lib/session.svelte";
  import { api, type MediaView } from "../lib/api";
  import { openMenu } from "../lib/menu.svelte";
  import Thumb from "./Thumb.svelte";

  let filter = $state("");
  const media = $derived((app.s?.media ?? []).filter((m) => m.name.toLowerCase().includes(filter.trim().toLowerCase())));
  const comps = $derived((app.s?.sequences ?? []).map((s, i) => ({ ...s, index: i })).slice(1));

  function mediaMenu(e: MouseEvent, m: MediaView) {
    const seq = app.s?.seq.name ?? "the edit";
    openMenu(e, [
      { heading: true, label: m.name },
      { label: "New composition from media", run: () => run(api.newCompositionFromMedia(m.id)) },
      { label: `Add to ${seq} at playhead`, run: () => run(api.addToTimeline(m.id, null, app.live?.playhead ?? 0)) },
      { label: `Append to end of ${seq}`, run: () => run(api.addToTimeline(m.id, null, null)) },
      { separator: true },
      { label: "Show in folder", run: () => run(api.showInFolder(m.path)) },
    ]);
  }

  function compMenu(e: MouseEvent, index: number, name: string) {
    openMenu(e, [
      { heading: true, label: name },
      { label: "Open in Composition Timeline", run: () => openComp(index) },
      { label: "Add to the edit at playhead", disabled: app.s?.active === index, run: () => run(api.addComposite(index)) },
    ]);
  }

  function openComp(index: number) {
    app.timelineTab = "comp";
    run(api.openSequence(index));
  }

  // Dragging a media item out to the timeline (pointer-based, so it works
  // alongside files dropped from the desktop).
  function down(e: PointerEvent, m: MediaView) {
    if (e.button !== 0) return;
    const x0 = e.clientX;
    const y0 = e.clientY;
    const move = (ev: PointerEvent) => {
      if (!app.dragMedia && Math.hypot(ev.clientX - x0, ev.clientY - y0) < 5) return;
      app.dragMedia = { id: m.id, name: m.name, x: ev.clientX, y: ev.clientY };
    };
    const up = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      // The timeline handles the drop on pointerup, then clears this.
      setTimeout(() => (app.dragMedia = null), 0);
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
  }
</script>

<div class="panel">
  <div class="tools">
    <button onclick={() => run(api.importDialog())}>Import…</button>
    <button onclick={() => run(api.addTitle())}>New title</button>
    <input type="search" placeholder="Find media" bind:value={filter} aria-label="Find media" />
  </div>

  <div class="scroll">
    <div class="section">
      <span>Compositions</span>
      <button class="ghost small" onclick={() => run(api.newComposite())} title="New empty composition">+ New</button>
    </div>
    {#if comps.length === 0}
      <p class="empty">None yet. Right-click a clip below and choose New composition from media.</p>
    {/if}
    {#each comps as c (c.index)}
      <button
        class="comp"
        class:open={app.s?.active === c.index}
        ondblclick={() => openComp(c.index)}
        onclick={() => openComp(c.index)}
        oncontextmenu={(e) => compMenu(e, c.index, c.name)}
        title="{c.width}×{c.height}, {c.fps.toFixed(3).replace('.000', '')} fps"
      >
        <span class="badge">Comp</span>
        <span class="name">{c.name}</span>
        <span class="muted tc">{shortTime(c.duration)}</span>
      </button>
    {/each}

    <div class="section"><span>Media</span><span class="muted">{app.s?.media.length ?? 0}</span></div>
    {#if (app.s?.media.length ?? 0) === 0}
      <p class="empty">Import footage, pictures and sound, or drop files anywhere on the window.</p>
    {/if}
    <div class="grid">
      {#each media as m (m.id)}
        <div
          class="card"
          role="button"
          tabindex="0"
          onpointerdown={(e) => down(e, m)}
          ondblclick={() => run(api.addToTimeline(m.id, null, null))}
          oncontextmenu={(e) => mediaMenu(e, m)}
          onkeydown={(e) => e.key === "Enter" && run(api.addToTimeline(m.id, null, null))}
        >
          <div class="pic {m.kind}">
            {#if m.kind !== "audio" && !m.missing}
              <Thumb path={m.path} at={Math.round(Math.min(m.duration / 3, 1_000_000_000))} alt="" />
            {:else if m.kind === "audio"}
              <svg viewBox="0 0 60 20" preserveAspectRatio="none" aria-hidden="true">
                {#each Array(30) as _, i}
                  <rect x={i * 2} y={10 - (3 + ((i * 37) % 8))} width="1.2" height={2 * (3 + ((i * 37) % 8))} />
                {/each}
              </svg>
            {/if}
            {#if m.missing}<span class="flag bad">Missing</span>{:else if m.proxy}<span class="flag">Proxy {m.proxy}</span>{/if}
          </div>
          <div class="name" title={m.path}>{m.name}</div>
          <div class="muted detail">{m.detail}</div>
        </div>
      {/each}
    </div>
  </div>
</div>

<style>
  .panel {
    height: 100%;
    display: flex;
    flex-direction: column;
    min-height: 0;
  }
  .tools {
    display: flex;
    gap: 6px;
    padding: 8px;
    flex-wrap: wrap;
  }
  .tools input {
    flex: 1 1 100px;
  }
  .scroll {
    overflow: auto;
    flex: 1;
    padding: 0 8px 10px;
  }
  .section {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin: 10px 2px 6px;
    font-weight: 600;
    font-size: calc(12px * var(--text-scale));
    color: var(--muted);
  }
  .small {
    min-height: 20px;
    padding: 0 6px;
    font-size: calc(12px * var(--text-scale));
  }
  .empty {
    color: var(--faint);
    margin: 2px 2px 8px;
    font-size: calc(12px * var(--text-scale));
  }
  .comp {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    text-align: left;
    margin-bottom: 4px;
    background: var(--base);
  }
  .comp.open {
    border-color: var(--clip-comp);
  }
  .comp .name {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .badge {
    font-size: calc(11px * var(--text-scale));
    font-weight: 600;
    color: color-mix(in srgb, var(--clip-comp) 45%, white);
    background: color-mix(in srgb, var(--clip-comp) 40%, transparent);
    border-radius: 3px;
    padding: 0 5px;
  }
  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(118px, 1fr));
    gap: 8px;
  }
  .card {
    border-radius: 5px;
    background: var(--base);
    border: 1px solid var(--line-soft);
    overflow: hidden;
    cursor: grab;
  }
  .card:hover {
    border-color: var(--line);
  }
  .card:focus-visible {
    outline: 2px solid var(--accent);
  }
  .pic {
    aspect-ratio: 16 / 9;
    background: var(--well);
    position: relative;
  }
  .pic.audio {
    background: color-mix(in srgb, var(--clip-audio) 30%, var(--well));
    display: grid;
    place-items: center;
  }
  .pic.audio svg {
    width: 70%;
    height: 50%;
    fill: color-mix(in srgb, var(--clip-audio) 60%, white);
    opacity: 0.8;
  }
  .flag {
    position: absolute;
    left: 4px;
    bottom: 4px;
    font-size: calc(10.5px * var(--text-scale));
    background: rgba(0, 0, 0, 0.7);
    border-radius: 3px;
    padding: 0 4px;
  }
  .flag.bad {
    background: #8a1f1a;
  }
  .card .name {
    padding: 4px 7px 0;
    font-weight: 500;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .detail {
    padding: 0 7px 6px;
    font-size: calc(11.5px * var(--text-scale));
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
</style>
