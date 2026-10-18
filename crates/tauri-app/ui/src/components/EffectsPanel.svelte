<script lang="ts">
  // The Effects section of the Controls panel: the clip's effect stack, top
  // of the list applied first. Add, reorder, switch off, edit and keyframe.
  import { untrack } from "svelte";
  import { app, run } from "../lib/session.svelte";
  import { api, type Inspector, type CatalogGroup, type Curves, type EffectView, type ParamView } from "../lib/api";
  import { livePump } from "../lib/live";
  import { openMenu, type MenuItem } from "../lib/menu.svelte";
  import NumberField from "./NumberField.svelte";
  import CurvesEditor from "./CurvesEditor.svelte";

  let { ins }: { ins: Inspector } = $props();
  const pump = livePump();

  // Values being dragged, so the numbers keep up with the pointer.
  let drafts = $state<Record<string, number>>({});
  // Effects folded shut (by position), per clip.
  let folded = $state<Record<string, boolean>>({});
  let shownId: number | null = null;
  $effect(() => {
    const id = ins.id;
    untrack(() => {
      if (id !== shownId) {
        shownId = id;
        drafts = {};
      }
    });
  });

  const value = (i: number, p: number, pv: ParamView) => drafts[`${i}:${p}`] ?? pv.value;
  function set(i: number, p: number, v: number) {
    drafts[`${i}:${p}`] = v;
    const id = ins.id;
    pump.push(() => api.effectParam(id, i, p, v));
  }
  async function commit() {
    await pump.done();
    await run(api.commit());
    drafts = {};
  }
  async function setNow(i: number, p: number, v: number) {
    set(i, p, v);
    await commit();
  }
  function curves(i: number, c: Curves, done: boolean) {
    const id = ins.id;
    pump.push(() => api.effectCurves(id, i, c));
    if (done) commit();
  }

  const op = (i: number, o: "up" | "down" | "remove" | "toggle" | "duplicate" | "reset") => run(api.effectOp(ins.id, i, o));

  function more(e: MouseEvent, i: number, fx: EffectView) {
    const items: MenuItem[] = [
      { label: "Duplicate", run: () => op(i, "duplicate") },
      { label: "Reset to defaults", disabled: fx.missing, run: () => op(i, "reset") },
      { separator: true },
      { label: "Move up", disabled: i === 0, run: () => op(i, "up") },
      { label: "Move down", disabled: i === ins.effects.length - 1, run: () => op(i, "down") },
      { separator: true },
      { label: "Remove", run: () => op(i, "remove") },
    ];
    openMenu(e, items);
  }

  // The "Add" menu: every effect by category (plugins included).
  let catalog = $state<CatalogGroup[]>([]);
  async function add(e: MouseEvent) {
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    catalog = await api.effectCatalog();
    const items: MenuItem[] = catalog.map((g) => ({
      label: g.label,
      submenu: g.items.map((it) => ({ label: it.plugin ? `${it.label}  ·  plugin` : it.label, run: () => run(api.effectAdd(ins.id, it.kind, it.plugin)) })),
    }));
    items.push({ separator: true }, { label: "Browse effects…", hint: "E", run: () => (app.effectsOpen = true) }, { label: "Manage plugins…", run: () => (app.pluginsOpen = true) });
    openMenu({ clientX: r.left, clientY: r.bottom + 2, preventDefault() {}, stopPropagation() {} } as MouseEvent, items);
  }

  const keyTip = (p: ParamView) => (p.key_here ? "Remove the keyframe here" : p.animated ? "Add a keyframe at the playhead" : "Animate: add a keyframe at the playhead");
  const fileName = (path: string) => path.split(/[\\/]/).pop() ?? path;
</script>

<div class="group">
  <div class="head">
    <h4>Effects</h4>
    <div class="adds">
      <button class="small" onclick={add} title="Add an effect to this clip">+ Add</button>
      <button class="small" onclick={() => (app.effectsOpen = true)} title="Search and browse every effect (E)">Browse…</button>
    </div>
  </div>
  {#if ins.effects.length === 0}
    <p class="note">None. Effects change the clip's picture before its transform; the top one is applied first.</p>
  {/if}
  {#each ins.effects as fx, i (i + ":" + fx.label)}
    {@const key = `${ins.id}:${i}`}
    <div class="card" class:off={!fx.enabled}>
      <div class="row">
        <input type="checkbox" checked={fx.enabled} aria-label="{fx.label} on" title="Turn the effect on or off" onchange={() => op(i, "toggle")} />
        <button class="fold" aria-expanded={!folded[key]} onclick={() => (folded[key] = !folded[key])} title={folded[key] ? "Show the settings" : "Hide the settings"}>
          <span class="chev" class:shut={folded[key]} aria-hidden="true">▾</span>
          <span class="name">{fx.label}</span>
          {#if fx.kind === "Plugin"}<span class="tag">plugin</span>{/if}
          {#if fx.animated}<span class="tag keyed" title="Some settings are keyframed">◆</span>{/if}
        </button>
        <button class="ghost icon" disabled={i === 0} onclick={() => op(i, "up")} title="Move up (applied earlier)" aria-label="Move {fx.label} up">
          <svg viewBox="0 0 12 12" width="12" height="12"><path d="M2.5 7.5 6 4l3.5 3.5" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" /></svg>
        </button>
        <button class="ghost icon" disabled={i === ins.effects.length - 1} onclick={() => op(i, "down")} title="Move down (applied later)" aria-label="Move {fx.label} down">
          <svg viewBox="0 0 12 12" width="12" height="12"><path d="M2.5 4.5 6 8l3.5-3.5" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" /></svg>
        </button>
        <button class="ghost icon" onclick={(e) => more(e, i, fx)} title="More: duplicate, reset, remove" aria-label="More for {fx.label}">⋯</button>
      </div>
      {#if fx.missing}
        <p class="warn">This plugin isn't installed (id “{fx.plugin}”), so the effect is skipped. Its settings are kept: install the plugin and it comes back.</p>
      {/if}
      {#if !folded[key]}
        {#if fx.params.length}
          <div class="params">
            {#each fx.params as p, pi}
              <span class="lbl" title={p.name}>{p.name}</span>
              <button class="key" class:here={p.key_here} class:anim={p.animated} onclick={() => run(api.effectKey(ins.id, i, pi))} title={keyTip(p)} aria-label="{keyTip(p)}: {p.name}">
                <svg viewBox="0 0 10 10" width="10" height="10"><path d="M5 1 9 5 5 9 1 5Z" /></svg>
              </button>
              <div class="ctl">
                {#if p.ui === "toggle"}
                  <input type="checkbox" checked={value(i, pi, p) >= 0.5} aria-label={p.name} onchange={(e) => setNow(i, pi, e.currentTarget.checked ? 1 : 0)} />
                {:else if p.ui === "choice"}
                  <select value={Math.round(value(i, pi, p))} aria-label={p.name} onchange={(e) => setNow(i, pi, +e.currentTarget.value)}>
                    {#each p.options as o, oi}<option value={oi}>{o}</option>{/each}
                  </select>
                {:else}
                  <NumberField label={p.name} value={value(i, pi, p)} min={p.min} max={p.max} step={p.step} digits={p.decimals} suffix={p.suffix} oninput={(v) => set(i, pi, v)} oncommit={commit} />
                  {#if fx.kind === "ChromaKey" && pi === 0}
                    <span class="swatch" style="background:hsl({value(i, pi, p)} 100% 50%)" title="The colour being keyed out"></span>
                  {/if}
                {/if}
                {#if p.ui === "number" && value(i, pi, p) !== p.default}
                  <button class="ghost tiny" onclick={() => setNow(i, pi, p.default)} title="Back to {p.default}{p.suffix}" aria-label="Reset {p.name}">↺</button>
                {/if}
              </div>
            {/each}
          </div>
        {/if}
        {#if fx.kind === "Curves" && fx.curves}
          <CurvesEditor curves={fx.curves} onchange={(c, done) => curves(i, c, done)} />
        {/if}
        {#if fx.kind === "Lut"}
          <div class="row lut">
            <span class="file" title={fx.lut ?? ""}>{fx.lut ? fileName(fx.lut) : "No LUT chosen"}</span>
            <button class="small" onclick={() => run(api.effectLut(ins.id, i))}>Choose .cube…</button>
          </div>
          {#if fx.lut_error}<p class="warn">{fx.lut_error}</p>{/if}
        {/if}
      {/if}
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
  }
  .small {
    min-height: 22px;
    padding: 0 7px;
    font-size: calc(12px * var(--text-scale));
  }
  .note {
    color: var(--faint);
    font-size: calc(12px * var(--text-scale));
    margin: 2px 0 8px;
  }
  .warn {
    color: #f0a050;
    font-size: calc(12px * var(--text-scale));
    margin: 0;
  }
  .card {
    border: 1px solid var(--line-soft);
    border-radius: 5px;
    padding: 5px 8px 8px;
    margin-bottom: 6px;
    display: flex;
    flex-direction: column;
    gap: 6px;
    background: var(--base);
  }
  .card.off .params,
  .card.off .name {
    opacity: 0.5;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 4px;
  }
  .fold {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: center;
    gap: 5px;
    background: none;
    border-color: transparent;
    padding: 2px 4px;
    text-align: left;
  }
  .chev {
    color: var(--muted);
    transition: transform 0.12s;
    font-size: calc(11px * var(--text-scale));
  }
  .chev.shut {
    transform: rotate(-90deg);
  }
  .name {
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .tag {
    font-size: calc(10.5px * var(--text-scale));
    color: var(--muted);
    border: 1px solid var(--line);
    border-radius: 3px;
    padding: 0 4px;
  }
  .tag.keyed {
    color: var(--accent);
    border-color: transparent;
  }
  .icon {
    min-height: 22px;
    min-width: 22px;
    padding: 0 4px;
    display: inline-grid;
    place-items: center;
  }
  .params {
    display: grid;
    grid-template-columns: minmax(60px, 0.9fr) 18px minmax(0, 1.4fr);
    gap: 5px 6px;
    align-items: center;
  }
  .lbl {
    color: var(--muted);
    font-size: calc(12px * var(--text-scale));
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .ctl {
    display: flex;
    align-items: center;
    gap: 4px;
    min-width: 0;
  }
  .ctl select {
    flex: 1;
    min-width: 0;
  }
  .key {
    width: 18px;
    height: 18px;
    min-height: 0;
    padding: 0;
    display: grid;
    place-items: center;
    background: none;
    border: 0;
  }
  .key path {
    fill: none;
    stroke: var(--faint);
    stroke-width: 1.2;
  }
  .key:hover path {
    stroke: var(--text);
  }
  .key.anim path {
    stroke: var(--accent);
  }
  .key.here path {
    fill: var(--accent);
    stroke: var(--accent);
  }
  .swatch {
    width: 14px;
    height: 14px;
    border-radius: 3px;
    flex: none;
  }
  .tiny {
    min-height: 20px;
    padding: 0 4px;
    color: var(--muted);
    flex: none;
  }
  .lut .file {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
</style>
