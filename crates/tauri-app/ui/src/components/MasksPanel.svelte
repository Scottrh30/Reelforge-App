<script lang="ts">
  // The Masks section of the Controls panel.
  import { app, run, say } from "../lib/session.svelte";
  import { api, type Inspector, type Mask, type MaskMotion } from "../lib/api";
  import { livePump } from "../lib/live";
  import NumberField from "./NumberField.svelte";

  let { ins }: { ins: Inspector } = $props();

  const modes: Mask["mode"][] = ["Add", "Subtract", "Intersect"];
  const active = $derived(app.activeMask?.clip === ins.id ? app.activeMask.index : null);
  const drawing = $derived(app.drawing?.clip === ins.id);
  const pump = livePump();
  const hasKey = $derived(ins.effects.some((e) => ["ChromaKey", "LumaKey", "DifferenceKey"].includes(e.kind)));

  // Rotoscoping: track what's inside a mask and key its shape every frame.
  // Footage follows what's in it; an adjustment layer, solid, title or picture
  // follows the footage below it.
  const trackable = $derived(["video", "adjustment", "plane", "title", "image"].includes(ins.kind));
  const through = $derived(ins.kind !== "video");
  let motion = $state<MaskMotion>("Transform");
  const motions: [MaskMotion, string][] = [
    ["Position", "Moves"],
    ["Transform", "Moves, turns, scales"],
    ["Perspective", "…and tilts (perspective)"],
    ["Bend", "Bends (cloth, skin, hands)"],
  ];
  const progress = $derived(app.live?.tracking && app.live.tracking[0] === ins.id ? app.live.tracking[1] : null);
  const elsewhere = $derived(!!app.live?.tracking && app.live.tracking[0] !== ins.id);

  const masks = (): Mask[] => ins.masks.map((m) => structuredClone($state.snapshot(m.mask)) as Mask);

  /// Change mask `i`; `done` makes it one undo step now, otherwise it's live until `commit`.
  function change(i: number, f: (m: Mask) => void, done: boolean) {
    const all = masks();
    f(all[i]);
    if (done) run(api.setMasks(ins.id, all, true));
    else pump.push(() => api.setMasks(ins.id, all, false));
  }
  async function commit() {
    await pump.done();
    await run(api.commit());
  }

  async function add(shape: "rectangle" | "ellipse") {
    app.drawing = null;
    const i = await run(api.addMask(ins.id, shape));
    if (i != null) app.activeMask = { clip: ins.id, index: i };
  }
  function freehand() {
    if (drawing) {
      app.drawing = null;
      return;
    }
    app.activeMask = null;
    app.maskTool = null;
    app.drawing = { clip: ins.id, points: [] };
    say("Click in the Viewer to place the mask's points; click the first point or press Enter to close it (Esc cancels)");
  }
  /// Give point `pt` of mask `n` its own feather (`v` px), or (`null`) use the mask's again.
  function pointFeather(n: Mask, pt: number, count: number, v: number | null) {
    const f = Array.from({ length: count }, (_, k) => n.feathers?.[k] ?? -1);
    f[pt] = v == null ? -1 : Math.max(0, v);
    n.feathers = f.some((x) => x >= 0) ? f : [];
  }
  async function remove(i: number) {
    const all = masks();
    all.splice(i, 1);
    app.activeMask = null;
    await run(api.setMasks(ins.id, all, true));
  }
</script>

<div class="group">
  <div class="head">
    <h4>Masks</h4>
    <div class="adds">
      <button class="small" onclick={() => add("rectangle")} title="A rectangle in the middle of the layer">+ Rectangle</button>
      <button class="small" onclick={() => add("ellipse")} title="An ellipse in the middle of the layer">+ Ellipse</button>
      <button class="small" class:on={drawing} onclick={freehand} title="Click points in the Viewer; click the first point or press Enter to close">+ Freehand</button>
    </div>
  </div>
  {#if drawing}
    <p class="note">Click in the Viewer to place points. Click the first point, double-click or press Enter to close; Esc cancels.</p>
  {/if}
  {#if ins.masks.length === 0}
    <p class="note">Cut the layer down to a shape. Masks are applied before effects and move with the layer.</p>
  {/if}
  {#each ins.masks as mv, i}
    {@const m = mv.mask}
    <div class="card" class:on={i === active}>
      <div class="row">
        <input type="checkbox" checked={m.enabled} aria-label="Mask on" title="On / off" onchange={(e) => change(i, (n) => (n.enabled = e.currentTarget.checked), true)} />
        <button
          class="name"
          class:on={i === active}
          onclick={() => (app.activeMask = i === active ? null : { clip: ins.id, index: i })}
          title="Edit this mask in the Viewer: drag its points, or inside it to move it">{m.name}</button
        >
        <select value={m.mode} aria-label="How this mask combines with the ones above it" onchange={(e) => change(i, (n) => (n.mode = e.currentTarget.value as Mask["mode"]), true)}>
          {#each modes as md}<option value={md}>{md}</option>{/each}
        </select>
        <label class="check"><input type="checkbox" checked={m.inverted} onchange={(e) => change(i, (n) => (n.inverted = e.currentTarget.checked), true)} /> Invert</label>
      </div>
      <div class="row">
        <span class="lbl" title="What the mask does. A garbage matte for keys keeps the layer inside it whatever the chroma, luma or difference key says (a subject near the screen's colour, a dark logo on a luma key); it doesn't cut the layer. To keep something out of the shot, use an ordinary mask (it cuts the layer before the key).">Use</span>
        <select value={m.key_keep ? "keep" : "cut"} aria-label="What this mask does" onchange={(e) => change(i, (n) => (n.key_keep = e.currentTarget.value === "keep"), true)}>
          <option value="cut">Cut the layer (show only inside)</option>
          <option value="keep">Garbage matte: keys keep what's inside</option>
        </select>
      </div>
      {#if m.key_keep && !hasKey}<p class="note">This clip has no chroma, luma or difference key yet: add one and it keeps what's inside this mask.</p>{/if}
      <div class="grid">
        <div class="field">
          <span class="lbl">Feather</span>
          <NumberField label="Feather" value={m.feather} min={0} max={1000} step={0.5} digits={0} suffix=" px" oninput={(v) => change(i, (n) => (n.feather = v), false)} oncommit={commit} />
        </div>
        <div class="field">
          <span class="lbl">Expand</span>
          <NumberField label="Expand" value={m.expansion} min={-1000} max={1000} step={0.5} digits={0} suffix=" px" oninput={(v) => change(i, (n) => (n.expansion = v), false)} oncommit={commit} />
        </div>
        <div class="field">
          <span class="lbl">Opacity</span>
          <NumberField label="Mask opacity" value={m.opacity * 100} min={0} max={100} step={0.5} digits={0} suffix="%" oninput={(v) => change(i, (n) => (n.opacity = v / 100), false)} oncommit={commit} />
        </div>
      </div>
      {#if i === active && m.shape !== "Ellipse"}
        {@const pt = app.activeMaskPoint}
        {#if pt != null && pt < mv.handles.length}
          {@const own = m.feathers?.[pt] ?? -1}
          {@const curved = (mv.curves[pt] ?? []).some((v) => v !== 0)}
          <div class="row pointrow">
            <span class="lbl">Point {pt + 1}</span>
            <button class="small" onclick={() => run(api.maskPointCurve(ins.id, i, pt))} title="Double-click the point in the Viewer does the same">{curved ? "Make sharp" : "Make curve"}</button>
            <button class="small" onclick={async () => { const at = await run(api.maskPointAdd(ins.id, i, pt, false)); if (typeof at === "number") app.activeMaskPoint = at; }} title="Add a point halfway to the next one (right-click a point in the Viewer for more)">Add point</button>
            <button class="small" disabled={mv.handles.length <= 3} onclick={async () => { if (await run(api.maskPointDelete(ins.id, i, pt))) app.activeMaskPoint = Math.min(pt, mv.handles.length - 2); }} title={mv.handles.length <= 3 ? "A mask keeps at least three points" : "Delete this point"}>Delete</button>
            <span class="lbl" title="The soft edge at this point; it blends into the next points' along the sides">Feather here</span>
            <NumberField label="Feather at this point" value={own >= 0 ? own : m.feather} min={0} max={1000} step={0.5} digits={0} suffix=" px" oninput={(v) => change(i, (n) => pointFeather(n, pt, mv.handles.length, v), false)} oncommit={commit} />
            {#if own >= 0}
              <button class="small" onclick={() => change(i, (n) => pointFeather(n, pt, mv.handles.length, null), true)} title="Use the mask's feather at this point again">Reset</button>
            {/if}
          </div>
        {:else}
          <p class="note">Click a point in the Viewer to give it its own feather. Double-click a point to make it a curve, or Alt-drag one to pull out its handles. Right-click the mask's line to add a point there, or a point to add points beside it or delete it.</p>
        {/if}
      {/if}
      <div class="row">
        <label class="check" title="Moving the shape's points then sets a keyframe at the playhead">
          <input type="checkbox" checked={m.keys.length > 0} onchange={(e) => run(api.maskOp(ins.id, i, e.currentTarget.checked ? "animate" : "still"))} />
          Animate shape
        </label>
        {#if m.keys.length}
          <span class="muted">{m.keys.length} key{m.keys.length === 1 ? "" : "s"}</span>
          {#if !mv.key_here}
            <button class="small" onclick={() => run(api.maskOp(ins.id, i, "key"))}>Key here</button>
          {:else if m.keys.length > 1}
            <button class="small" onclick={() => run(api.maskOp(ins.id, i, "unkey"))}>Delete key</button>
          {/if}
        {/if}
        <span class="grow"></span>
        <button class="small" onclick={() => remove(i)}>Remove</button>
      </div>
      {#if trackable}
        <div class="row track">
          <span class="lbl" title={through ? "Follows what's inside the mask in the footage below this layer, from the playhead, keying the shape at every frame. Fix a frame where it slips by dragging the points, then track on from there" : "Follows what's inside the mask from the playhead, keying the shape at every frame. Fix a frame where it slips by dragging the points, then track on from there"}>Track shape</span>
          {#if progress !== null}
            <div class="bar"><div style="width:{progress * 100}%"></div></div>
            <span class="tc">{Math.round(progress * 100)}%</span>
            <button class="small" onclick={() => run(api.trackCancel())}>Stop</button>
          {:else}
            <select bind:value={motion} aria-label="How the shape may move" title="How the shape may move as it follows">
              {#each motions as [v, l]}<option value={v}>{l}</option>{/each}
            </select>
            <button class="small" disabled={elsewhere} onclick={() => run(api.maskTrack(ins.id, i, false, motion))} title="From the playhead back to the clip's start">◀</button>
            <button class="small" disabled={elsewhere} onclick={() => run(api.maskTrack(ins.id, i, true, motion))} title="From the playhead to the clip's end">▶</button>
          {/if}
        </div>
      {/if}
    </div>
  {/each}
</div>

<style>
  .track {
    gap: 6px;
  }
  .track .lbl {
    color: var(--muted);
  }
  .track select {
    flex: 1;
    min-width: 0;
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
  .tc {
    font-variant-numeric: tabular-nums;
  }
  .group {
    padding: 8px 0 12px;
    border-bottom: 1px solid var(--line-soft);
  }
  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    flex-wrap: wrap;
    gap: 6px;
    margin-bottom: 8px;
  }
  h4 {
    font-size: calc(12.5px * var(--text-scale));
    font-weight: 600;
    color: var(--muted);
    margin: 0;
  }
  .adds {
    display: flex;
    gap: 4px;
    flex-wrap: wrap;
  }
  .small {
    min-height: 22px;
    padding: 0 7px;
    font-size: calc(12px * var(--text-scale));
  }
  .pointrow {
    flex-wrap: wrap;
  }
  .note {
    color: var(--faint);
    font-size: calc(12px * var(--text-scale));
    margin: 2px 0 8px;
  }
  .card {
    border: 1px solid var(--line-soft);
    border-left: 3px solid var(--line);
    border-radius: 5px;
    padding: 6px 8px;
    margin-bottom: 6px;
    display: flex;
    flex-direction: column;
    gap: 6px;
    background: var(--base);
  }
  .card.on {
    border-left-color: #ff6ec8;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 6px;
    flex-wrap: wrap;
  }
  .name {
    flex: 1;
    min-width: 60px;
    text-align: left;
    background: none;
    border-color: transparent;
    padding: 2px 4px;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .name.on {
    color: #ff6ec8;
  }
  .check {
    display: flex;
    align-items: center;
    gap: 4px;
  }
  .grid {
    display: grid;
    grid-template-columns: repeat(3, minmax(0, 1fr));
    gap: 6px;
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }
  .lbl {
    color: var(--muted);
    font-size: calc(12px * var(--text-scale));
  }
  .grow {
    flex: 1;
  }
  .muted {
    color: var(--muted);
    font-size: calc(12px * var(--text-scale));
  }
</style>
