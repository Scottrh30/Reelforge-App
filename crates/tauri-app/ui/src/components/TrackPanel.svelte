<script lang="ts">
  // The Track panel: track the selected clip with the method of your choice
  // (one point, two points, four points, a planar surface or a bending mesh),
  // then put the motion on any layer, a mask, or the clip itself (stabilise),
  // choosing which parts of the motion to use.
  import { app, run, timecode } from "../lib/session.svelte";
  import { api, type TrackAxes, type TrackMode, type TrackerView } from "../lib/api";
  import { livePump } from "../lib/live";
  import NumberField from "./NumberField.svelte";

  // The clip being tracked: the selected clip when it can be tracked, else
  // the last one picked here (or any clip with a tracker).
  const sel = $derived(app.s?.inspector?.can_track ? app.s.inspector : null);
  let picked = $state<number | null>(null);
  const selId = $derived(sel?.id ?? null);
  $effect(() => {
    if (selId !== null) picked = selId;
  });
  type Source = { id: number; name: string; track: string; tracker: TrackerView | null };
  const sources = $derived.by((): Source[] => {
    const list: Source[] = (app.s?.trackers ?? []).map((t) => ({ ...t }));
    if (sel && !list.some((t) => t.id === sel.id)) list.unshift({ id: sel.id, name: sel.name, track: sel.track, tracker: sel.tracker });
    return list;
  });
  const ins = $derived(sources.find((x) => x.id === picked) ?? sources[0] ?? null);

  const methods: [TrackMode, string, string][] = [
    ["Position", "Single point", "1"],
    ["Transform", "Two point", "2"],
    ["CornerPin", "Four point", "4"],
    ["Planar", "Planar", "▱"],
    ["Mesh", "Mesh", "▦"],
  ];
  const methodTip: Record<TrackMode, string> = {
    Position: "Position only",
    Transform: "Position, rotation and scale",
    CornerPin: "Corner pin / perspective",
    Planar: "A whole flat surface, in perspective",
    Mesh: "A surface that bends",
  };
  const grids: [number, number][] = [[2, 2], [3, 3], [4, 3], [4, 4], [6, 4], [6, 6], [8, 6], [8, 8], [16, 16], [32, 32]];
  const hints: Record<TrackMode, string> = {
    Position: "Drag the box in the Viewer onto a sharp detail (a corner, a mark, a light), then track. The outer box is how far it may move per frame.",
    Transform: "Put the two boxes on sharp details of the same object, as far apart as you can: the line between them gives the rotation and scale.",
    CornerPin: "Put boxes 1 to 4 on the surface's top-left, top-right, bottom-right and bottom-left corners (or on details near them).",
    Planar: "Drag corners 1 to 4 onto a flat surface (a screen, a sign, a wall). Everything with detail inside the shape is tracked, so the corners can sit on blank areas, and the surface can be partly covered.",
    Mesh: "Drag the grid's four big corners over a surface that bends (cloth, skin, a flag). Before tracking you can also drag the inner points to follow a curve.",
  };
  const perspective = (m: TrackMode) => m === "CornerPin" || m === "Planar";
  const areaTracked = (m: TrackMode) => m === "Planar" || m === "Mesh";

  let smoothing = $state(0);
  let target = $state("");
  let axes = $state<TrackAxes>({ x: true, y: true, rotation: true, scale: true });
  const t = $derived(ins?.tracker ?? null);
  const progress = $derived(ins && app.live?.tracking && app.live.tracking[0] === ins.id ? app.live.tracking[1] : null);
  const elsewhere = $derived(!!ins && !!app.live?.tracking && app.live.tracking[0] !== ins.id);
  const key = (c: { clip: number; mask: number | null; blade: number | null }) => `${c.clip}:${c.mask ?? ""}:${c.blade ?? ""}`;

  // Pick a sensible target: another layer or a mask before stabilising this one.
  $effect(() => {
    const list = t?.targets ?? [];
    if (!list.some((c) => key(c) === target)) {
      const first = list.find((c) => c.clip !== ins?.id || c.mask !== null || c.blade !== null) ?? list[0];
      target = first ? key(first) : "";
    }
  });
  const chosen = $derived(t?.targets.find((c) => key(c) === target) ?? null);
  const stabilising = $derived(!!ins && chosen?.clip === ins.id && chosen?.mask === null && chosen?.blade === null);
  const toBlade = $derived(chosen?.blade != null);
  const toMask = $derived(chosen?.mask != null);
  // A point turns and scales with any track that has more than one point.
  const toPoint = $derived(chosen?.point === true);
  const turns = $derived(t?.mode === "Transform" || (toPoint && t?.mode !== "Position"));
  const anyAxis = $derived(axes.x || axes.y || (turns && (axes.rotation || axes.scale)));

  function pick(m: TrackMode) {
    if (!ins) return;
    if (!t) run(api.trackerAdd(ins.id, m));
    else if (t.mode !== m) run(api.trackerMode(ins.id, m));
  }

  const pump = livePump();
  function sizes(size: number, search: number) {
    if (ins) pump.push(() => api.trackerSizes(ins.id, size, search));
  }
  async function commit() {
    await pump.done();
    await run(api.commit());
  }
  // Smoothing being dragged, so the number keeps up with the pointer.
  let smoothDraft = $state<number | null>(null);
  function smoothTo(v: number) {
    if (!ins) return;
    const id = ins.id;
    smoothDraft = v;
    pump.push(() => api.trackerSmooth(id, v));
  }
  async function smoothDone() {
    await commit();
    smoothDraft = null;
  }
  function apply() {
    if (!ins || !chosen) return;
    run(api.trackApply(ins.id, chosen.clip, chosen.mask, chosen.blade, smoothing, { ...axes }));
  }
</script>

<div class="panel">
  {#if !ins}
    <p class="empty">Select a video clip in the timeline to track it.</p>
  {:else}
    <div class="source">
      <span class="lbl">Tracking</span>
      {#if sources.length > 1}
        <select value={ins.id} aria-label="Clip being tracked" onchange={(e) => (picked = Number(e.currentTarget.value))}>
          {#each sources as x}<option value={x.id}>{x.name} ({x.track})</option>{/each}
        </select>
      {:else}
        <span class="name">{ins.name}</span>
        <span class="trk">{ins.track}</span>
      {/if}
    </div>
    {#if app.s?.selection !== ins.id}
      <div class="row away">
        <span class="note">Its tracker shows in the Viewer while the clip is selected.</span>
        <button class="small" onclick={() => run(api.select(ins.id, "only"))}>Select</button>
      </div>
    {/if}

    <div class="sect">
      <h4>Method</h4>
      <div class="methods" role="radiogroup" aria-label="Tracking method">
        {#each methods as [m, label, icon]}
          <button
            role="radio"
            aria-checked={t?.mode === m}
            class:on={t?.mode === m}
            title={methodTip[m]}
            disabled={progress !== null}
            onclick={() => pick(m)}><span class="ico">{icon}</span>{label}</button
          >
        {/each}
      </div>
      {#if !t}
        <p class="note">Pick a method to place a tracker on this clip.</p>
      {:else}
        <p class="note">{hints[t.mode]}</p>
      {/if}
    </div>

    {#if t}
      <div class="sect">
        <h4>Track</h4>
        <div class="grid">
          {#if t.mode === "Mesh"}
            <span class="lbl" title="Cells across × down. Changing it starts the grid again from its four corners">Grid</span>
            <select value={`${t.grid[0]}x${t.grid[1]}`} aria-label="Mesh grid" onchange={(e) => {
              const [c, r] = e.currentTarget.value.split("x").map(Number);
              run(api.trackerGrid(ins.id, c, r));
            }}>
              {#each grids as [c, r]}<option value={`${c}x${r}`}>{c} × {r} cells</option>{/each}
            </select>
          {/if}
          {#if !areaTracked(t.mode)}
            <span class="lbl">Feature box</span>
            <NumberField label="Feature box size" value={t.size} min={9} max={201} step={0.5} digits={0} suffix=" px" oninput={(v) => sizes(v, t.search)} oncommit={commit} />
          {/if}
          <span class="lbl" title="How far it may move between frames. Raise it for fast motion">Search</span>
          <NumberField label="Search area" value={t.search} min={2} max={300} step={0.5} digits={0} suffix=" px" oninput={(v) => sizes(t.size, v)} oncommit={commit} />
        </div>

        {#if progress !== null}
          <div class="row">
            <div class="bar"><div style="width:{progress * 100}%"></div></div>
            <span class="tc">{Math.round(progress * 100)}%</span>
            <button onclick={() => run(api.trackCancel())}>Stop</button>
          </div>
        {:else}
          <div class="row two">
            <button disabled={elsewhere} onclick={() => run(api.trackStart(ins.id, false))} title="From the playhead back to the clip's start">◀ Backward</button>
            <button class="primary" disabled={elsewhere} onclick={() => run(api.trackStart(ins.id, true))} title="From the playhead to the clip's end">Forward ▶</button>
          </div>
          {#if elsewhere}<p class="note">Another clip is being tracked.</p>{/if}
        {/if}
        {#if t.range}
          <p class="muted">
            {t.frames} frames tracked, <span class="tc">{timecode(t.range[0])}</span> to <span class="tc">{timecode(t.range[1])}</span>
          </p>
          <div class="grid">
            <span class="lbl" title="Takes the jitter out of the tracked paths without tracking again. The tracked data is kept: set it back to 0 any time. Everything you apply the track to uses the smoothed paths">Smooth track</span>
            <div class="ctl">
              <NumberField label="Smooth track" value={smoothDraft ?? t.smooth} min={0} max={30} step={0.05} digits={1} suffix=" frames" oninput={smoothTo} oncommit={smoothDone} />
              {#if t.smooth > 0}<button class="ghost tiny" onclick={() => { smoothTo(0); smoothDone(); }} title="No smoothing" aria-label="No smoothing">↺</button>{/if}
            </div>
          </div>
          {#if t.smooth > 0}<p class="note">The solid lines in the Viewer are the smoothed paths; the faint dotted ones are as tracked. Apply the track again to use the new smoothing.</p>{/if}
        {/if}
      </div>

      <div class="sect">
        <h4>Apply tracking data</h4>
        {#if !t.range}
          <p class="note">Track the clip first; then choose a layer here to move with it.</p>
        {:else}
          <div class="grid">
            <span class="lbl">Apply to</span>
            <select bind:value={target} aria-label="Apply the tracking data to">
              {#each t.targets as c}<option value={key(c)}>{c.label}</option>{/each}
            </select>
          </div>
          <div class="checks" class:dim={toMask}>
            <label title="Movement across"><input type="checkbox" bind:checked={axes.x} disabled={toMask} /> X position</label>
            <label title="Movement up and down"><input type="checkbox" bind:checked={axes.y} disabled={toMask} /> Y position</label>
            {#if turns}
              <label><input type="checkbox" bind:checked={axes.rotation} disabled={toMask} /> Rotation</label>
              <label><input type="checkbox" bind:checked={axes.scale} disabled={toMask} /> Scale</label>
            {/if}
          </div>
          {#if toMask}<p class="note">A mask follows the whole motion.</p>{/if}
          {#if stabilising && !perspective(t.mode)}
            <div class="grid">
              <span class="lbl" title="Smooth camera: 0 locks the shot completely. Higher keeps slow, deliberate camera moves and only takes out the shake: 10 to 30 frames suits most handheld footage">Keep camera moves</span>
              <NumberField label="Smoothing" value={smoothing} min={0} max={120} step={0.25} digits={0} suffix=" frames" oninput={(v) => (smoothing = v)} oncommit={() => {}} />
            </div>
          {/if}
          <div class="row">
            <button class="primary wide" disabled={!chosen || (!toMask && !anyAxis)} onclick={apply} title="Writes keyframes (undoable)">Apply</button>
          </div>
          {#if toPoint}
            <p class="note">The point keeps where it is at the start of the track and moves with it from there. Every layer, camera and light parented to the point follows too (Controls → Layer → Parent).{#if t.mode !== "Position" && t.mode !== "Transform"} A point can't take perspective: it gets the surface's move, turn and scale.{/if}</p>
          {:else if toBlade}
            <p class="note">The blade keeps where its hilt and tip are at the playhead and moves with the tracked points from there. For a prop with two markers, a two-point track also turns and stretches it.</p>
          {:else if t.mode === "Mesh" && chosen && chosen.clip !== ins.id}
            <p class="note">The layer is stretched over the grid and bends with it (Controls → Transform: remove the mesh warp to go back).</p>
          {:else if perspective(t.mode) && chosen && chosen.clip !== ins.id}
            <p class="note">An unpinned layer is pinned straight into the four points. To place it differently, adjust its corner pin at the first tracked frame and apply again.</p>
          {/if}
        {/if}
      </div>

      <div class="row two">
        <button disabled={!t.tracked} onclick={() => run(api.trackerOp(ins.id, "clear"))}>Clear paths</button>
        <button onclick={() => run(api.trackerOp(ins.id, "remove"))}>Remove tracker</button>
      </div>
    {/if}
  {/if}
</div>

<style>
  .panel {
    height: 100%;
    overflow: auto;
    padding: 8px 10px 12px;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .empty {
    color: var(--faint);
    margin: 18px 4px;
    text-align: center;
  }
  .source {
    display: flex;
    align-items: baseline;
    gap: 6px;
    padding: 6px 8px;
    background: var(--base);
    border-radius: 5px;
    min-width: 0;
  }
  .source select {
    flex: 1;
    min-width: 0;
  }
  .away {
    justify-content: space-between;
  }
  .source .name {
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    flex: 1;
    min-width: 0;
  }
  .source .trk {
    color: var(--faint);
    font-size: calc(11.5px * var(--text-scale));
  }
  .sect {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding-bottom: 10px;
    border-bottom: 1px solid var(--line-soft);
  }
  .ctl {
    display: flex;
    align-items: center;
    gap: 4px;
    min-width: 0;
  }
  .ctl :global(.num) {
    flex: 1;
    min-width: 0;
  }
  .tiny {
    min-height: 20px;
    padding: 0 4px;
    color: var(--muted);
    flex: none;
  }
  h4 {
    font-size: calc(12.5px * var(--text-scale));
    font-weight: 600;
    color: var(--muted);
    margin: 0;
  }
  .methods {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(92px, 1fr));
    gap: 4px;
  }
  .methods button {
    display: flex;
    align-items: center;
    gap: 6px;
    justify-content: flex-start;
    padding: 5px 7px;
    white-space: nowrap;
  }
  .methods button.on {
    background: var(--accent);
    border-color: var(--accent);
    color: #fff;
  }
  .ico {
    display: inline-grid;
    place-items: center;
    width: 16px;
    height: 16px;
    border-radius: 3px;
    font-size: 11px;
    font-weight: 700;
    background: rgba(127, 127, 127, 0.25);
  }
  .note {
    color: var(--faint);
    font-size: calc(12px * var(--text-scale));
    margin: 0;
  }
  .muted {
    color: var(--muted);
    font-size: calc(12px * var(--text-scale));
    margin: 0;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .row.two > button {
    flex: 1;
  }
  .wide {
    flex: 1;
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
  .checks {
    display: flex;
    flex-wrap: wrap;
    gap: 6px 14px;
  }
  .checks label {
    display: flex;
    align-items: center;
    gap: 5px;
    cursor: pointer;
  }
  .checks.dim {
    opacity: 0.5;
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
