<script lang="ts">
  // The Motion tracking section of the Controls panel: place a tracker (1, 2
  // or 4 points), track it, and apply the motion to another layer, a mask, or
  // this clip (stabilise).
  import { app, run, timecode } from "../lib/session.svelte";
  import { api, type Inspector, type TrackMode } from "../lib/api";
  import { livePump } from "../lib/live";
  import NumberField from "./NumberField.svelte";

  let { ins }: { ins: Inspector } = $props();

  const modes: [TrackMode, string][] = [
    ["Position", "Position (1 point)"],
    ["Transform", "Position, rotation and scale (2 points)"],
    ["CornerPin", "Corner pin / perspective (4 points)"],
  ];
  const hints: Record<TrackMode, string> = {
    Position: "Drag the box in the Viewer onto a sharp detail (a corner, a mark, a light), then track. The outer box is how far it may move per frame.",
    Transform: "Put the two boxes on sharp details of the same object, as far apart as you can: the line between them gives the rotation and scale.",
    CornerPin: "Put boxes 1 to 4 on the surface's top-left, top-right, bottom-right and bottom-left corners (or on details near them).",
  };

  let newMode = $state<TrackMode>("Position");
  let smoothing = $state(0);
  let target = $state("");
  const t = $derived(ins.tracker);
  const progress = $derived(app.live?.tracking && app.live.tracking[0] === ins.id ? app.live.tracking[1] : null);
  const elsewhere = $derived(!!app.live?.tracking && app.live.tracking[0] !== ins.id);
  const key = (c: { clip: number; mask: number | null }) => `${c.clip}:${c.mask ?? ""}`;

  // Pick a sensible target: another layer or a mask before stabilising this one.
  $effect(() => {
    const list = t?.targets ?? [];
    if (!list.some((c) => key(c) === target)) {
      const first = list.find((c) => c.clip !== ins.id || c.mask !== null) ?? list[0];
      target = first ? key(first) : "";
    }
  });
  const chosen = $derived(t?.targets.find((c) => key(c) === target) ?? null);
  const stabilising = $derived(chosen?.clip === ins.id && chosen?.mask === null);

  const pump = livePump();
  function sizes(size: number, search: number) {
    pump.push(() => api.trackerSizes(ins.id, size, search));
  }
  async function commit() {
    await pump.done();
    await run(api.commit());
  }
</script>

<div class="group">
  <h4>Motion tracking</h4>
  {#if !t}
    <p class="note">Follow details through the clip, then make another layer move with them, pin a picture onto a moving surface, or hold this clip steady.</p>
    <div class="row">
      <select bind:value={newMode} aria-label="Tracker points">
        {#each modes as [m, label]}<option value={m}>{label}</option>{/each}
      </select>
      <button onclick={() => run(api.trackerAdd(ins.id, newMode))}>Add tracker</button>
    </div>
  {:else}
    <p class="note">{hints[t.mode]}</p>
    <div class="grid">
      <span class="lbl">Points</span>
      <select value={t.mode} aria-label="Tracker points" onchange={(e) => run(api.trackerMode(ins.id, e.currentTarget.value as TrackMode))}>
        {#each modes as [m, label]}<option value={m}>{label}</option>{/each}
      </select>
      <span class="lbl">Feature box</span>
      <NumberField label="Feature box size" value={t.size} min={9} max={201} step={0.5} digits={0} suffix=" px" oninput={(v) => sizes(v, t.search)} oncommit={commit} />
      <span class="lbl" title="How far a feature may move between frames. Raise it for fast motion; lower is less likely to jump to a look-alike">Search</span>
      <NumberField label="Search area" value={t.search} min={2} max={300} step={0.5} digits={0} suffix=" px" oninput={(v) => sizes(t.size, v)} oncommit={commit} />
    </div>

    {#if progress !== null}
      <div class="row track">
        <div class="bar"><div style="width:{progress * 100}%"></div></div>
        <span class="tc">{Math.round(progress * 100)}%</span>
        <button onclick={() => run(api.trackCancel())}>Stop</button>
      </div>
    {:else}
      <div class="row track">
        <button disabled={elsewhere} onclick={() => run(api.trackStart(ins.id, false))} title="From the playhead back to the clip's start">◀ Track backward</button>
        <button class="primary" disabled={elsewhere} onclick={() => run(api.trackStart(ins.id, true))} title="From the playhead to the clip's end">Track forward ▶</button>
      </div>
      {#if elsewhere}<p class="note">Another clip is being tracked.</p>{/if}
    {/if}

    {#if t.range}
      <p class="muted">
        {t.frames} {t.shown.length > 1 ? "frames with every point tracked" : "frames tracked"},
        <span class="tc">{timecode(t.range[0])}</span> to <span class="tc">{timecode(t.range[1])}</span>
      </p>
      <div class="grid">
        <span class="lbl">Apply to</span>
        <select bind:value={target} aria-label="Apply the motion to">
          {#each t.targets as c}<option value={key(c)}>{c.label}</option>{/each}
        </select>
        {#if stabilising && t.mode !== "CornerPin"}
          <span class="lbl" title="0 locks the shot completely. Higher keeps slow, deliberate camera moves and only takes out the shake: 10 to 30 frames suits most handheld footage">Smoothing</span>
          <NumberField label="Smoothing" value={smoothing} min={0} max={120} step={0.25} digits={0} suffix=" frames" oninput={(v) => (smoothing = v)} oncommit={() => {}} />
        {/if}
      </div>
      <div class="row">
        <button class="primary" disabled={!chosen} onclick={() => chosen && run(api.trackApply(ins.id, chosen.clip, chosen.mask, smoothing))} title="Writes keyframes (undoable)">Apply</button>
      </div>
      {#if t.mode === "CornerPin" && chosen && chosen.clip !== ins.id}
        <p class="note">A layer that isn't pinned is pinned straight into the four points. To place it differently on the surface, adjust its corner pin at the first tracked frame and apply again.</p>
      {/if}
    {/if}

    <div class="row">
      <button disabled={!t.tracked} onclick={() => run(api.trackerOp(ins.id, "clear"))}>Clear paths</button>
      <button onclick={() => run(api.trackerOp(ins.id, "remove"))}>Remove tracker</button>
    </div>
  {/if}
</div>

<style>
  .group {
    padding: 8px 0 12px;
    border-bottom: 1px solid var(--line-soft);
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  h4 {
    font-size: 12.5px;
    font-weight: 600;
    color: var(--muted);
    margin: 0;
  }
  .note {
    color: var(--faint);
    font-size: 12px;
    margin: 0;
  }
  .muted {
    color: var(--muted);
    font-size: 12px;
    margin: 0;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 6px;
    flex-wrap: wrap;
  }
  .row select {
    flex: 1;
    min-width: 0;
  }
  .grid {
    display: grid;
    grid-template-columns: 84px 1fr;
    gap: 6px;
    align-items: center;
  }
  .grid select {
    min-width: 0;
  }
  .lbl {
    color: var(--muted);
  }
  .bar {
    flex: 1;
    height: 8px;
    background: var(--base);
    border-radius: 4px;
    overflow: hidden;
  }
  .bar div {
    height: 100%;
    background: var(--accent);
  }
</style>
