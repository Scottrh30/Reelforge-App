<script lang="ts">
  // The Masks section of the Controls panel.
  import { app, run, say } from "../lib/session.svelte";
  import { api, type Inspector, type Mask } from "../lib/api";
  import { livePump } from "../lib/live";
  import NumberField from "./NumberField.svelte";

  let { ins }: { ins: Inspector } = $props();

  const modes: Mask["mode"][] = ["Add", "Subtract", "Intersect"];
  const active = $derived(app.activeMask?.clip === ins.id ? app.activeMask.index : null);
  const drawing = $derived(app.drawing?.clip === ins.id);
  const pump = livePump();

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
    app.drawing = { clip: ins.id, points: [] };
    say("Click in the Viewer to place the mask's points; click the first point or press Enter to close it (Esc cancels)");
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
    </div>
  {/each}
</div>

<style>
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
    font-size: 12.5px;
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
    font-size: 12px;
  }
  .note {
    color: var(--faint);
    font-size: 12px;
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
    font-size: 12px;
  }
  .grow {
    flex: 1;
  }
  .muted {
    color: var(--muted);
    font-size: 12px;
  }
</style>
