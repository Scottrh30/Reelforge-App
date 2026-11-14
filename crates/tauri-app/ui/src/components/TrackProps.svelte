<script lang="ts">
  // An expanded video track's header: the clip shown there (the selected one
  // on the track, else the one at the playhead), one row each — as in After
  // Effects' layer properties, with the keyframes in the track's lane on the
  // same rows: its opacity, position, scale, anchor point and rotation, then
  // its effects (on/off; twirl one open for its settings). Each property has
  // a stopwatch (start or stop keyframing) and, once keyframed, a diamond to
  // add or remove the key at the playhead. Values are as seen at the
  // playhead; changing a keyframed one sets a key there.
  import { api, type ParamView, type TrackProps, type Transform } from "../lib/api";
  import { run } from "../lib/session.svelte";
  import { trackRows, twirlKey, twirls } from "../lib/trackrows.svelte";
  import NumberField from "./NumberField.svelte";

  let { props, locked }: { props: TrackProps | null; locked: boolean } = $props();

  let draft = $state<Transform | null>(null);
  // Effect settings being dragged ("index:param" → value), until the next state.
  let fxDraft = $state<Record<string, number>>({});
  let shownId = -1;
  $effect(() => {
    const id = props?.clip ?? -1;
    if (id !== shownId) {
      shownId = id;
      draft = null;
      fxDraft = {};
    }
  });
  const t = $derived(draft ?? props?.transform ?? null);
  const rows = $derived(trackRows(props));

  function setT(f: (t: Transform) => void) {
    if (!props || !t) return;
    const n = structuredClone($state.snapshot(t)) as Transform;
    f(n);
    draft = n;
    api.setTransform(props.clip, n);
  }
  async function commit() {
    await run(api.commit());
    draft = null;
    fxDraft = {};
  }
  function setP(index: number, pi: number, v: number) {
    if (!props) return;
    fxDraft[`${index}:${pi}`] = v;
    api.effectParam(props.clip, index, pi, v);
  }
  // A click-and-done setting (a toggle, a menu, a colour): set and commit.
  async function setNow(index: number, pi: number, v: number) {
    if (!props) return;
    await api.effectParam(props.clip, index, pi, v);
    await commit();
  }
  const pval = (index: number, pi: number, p: ParamView) => fxDraft[`${index}:${pi}`] ?? p.value;
  const hex = (v: number) => "#" + (Math.round(v) & 0xffffff).toString(16).padStart(6, "0");
  const select = () => props && run(api.select(props.clip, "only"));
  function twirl(index: number) {
    if (!props) return;
    const k = twirlKey(props.clip, index);
    twirls[k] = !twirls[k];
  }
  const watchTip = (name: string, animated: boolean) => (animated ? `Stop keyframing ${name} (removes its keyframes, keeping the value here)` : `Keyframe ${name}: a keyframe at the playhead`);
  const keyTip = (here: boolean) => (here ? "Remove the keyframe here" : "Add a keyframe at the playhead");
</script>

{#snippet watch(animated: boolean, name: string, onclick: () => void)}
  <button class="watch" class:on={animated} disabled={locked} {onclick} title={watchTip(name, animated)} aria-label={watchTip(name, animated)} aria-pressed={animated}>
    <svg viewBox="0 0 12 12" width="11" height="11" aria-hidden="true"><circle cx="6" cy="6.8" r="4.3" /><path d="M6 6.8V4.4M4.6 1.2h2.8" /></svg>
  </button>
{/snippet}
{#snippet keyBtn(show: boolean, here: boolean, onclick: () => void)}
  {#if show}
    <button class="key" class:here disabled={locked || !props?.on_clip} {onclick} title={keyTip(here)} aria-label={keyTip(here)}>
      <svg viewBox="0 0 10 10" width="9" height="9" aria-hidden="true"><path d="M5 1 9 5 5 9 1 5Z" /></svg>
    </button>
  {:else}
    <span class="key"></span>
  {/if}
{/snippet}

<!-- (Its own pointer handling: dragging a value mustn't drag the track.) -->
<div class="tp" class:locked role="group" aria-label="Clip properties" onpointerdown={(e) => e.stopPropagation()}>
  {#if !props || !t}
    <p class="none">No clip here at the playhead</p>
  {:else}
    {#each rows as r}
      {#if r.kind === "name"}
        <button class="clipname" onclick={select} title="Select {props.name}">{props.name}</button>
      {:else if r.kind === "prop"}
        {@const pr = r.prop}
        <div class="row" class:two={pr.prop !== "Opacity" && pr.prop !== "Rotation"}>
          {@render watch(pr.animated, pr.prop.toLowerCase(), () => run(api.transformStopwatch(props!.clip, pr.prop)))}
          <span class="lbl" class:anim={pr.animated}>{pr.prop}</span>
          {#if pr.prop === "Opacity"}
            <NumberField label="Opacity" value={Math.round(t.opacity * 1000) / 10} min={0} max={100} step={0.5} digits={0} suffix="%" oninput={(v) => setT((n) => (n.opacity = v / 100))} oncommit={commit} />
          {:else if pr.prop === "Rotation"}
            <NumberField label="Rotation" value={t.rotation} step={0.5} digits={1} suffix="°" oninput={(v) => setT((n) => (n.rotation = v))} oncommit={commit} />
          {:else if pr.prop === "Scale"}
            <NumberField label="Scale X" value={Math.round(t.scale[0] * 1000) / 10} step={0.5} digits={0} suffix="%" oninput={(v) => setT((n) => (n.scale[0] = v / 100))} oncommit={commit} />
            <NumberField label="Scale Y" value={Math.round(t.scale[1] * 1000) / 10} step={0.5} digits={0} suffix="%" oninput={(v) => setT((n) => (n.scale[1] = v / 100))} oncommit={commit} />
          {:else}
            {@const k = pr.prop === "Position" ? "position" : "anchor"}
            <NumberField label="{pr.prop} X" value={t[k][0]} digits={0} oninput={(v) => setT((n) => (n[k][0] = v))} oncommit={commit} />
            <NumberField label="{pr.prop} Y" value={t[k][1]} digits={0} oninput={(v) => setT((n) => (n[k][1] = v))} oncommit={commit} />
          {/if}
          {@render keyBtn(pr.animated, pr.key_here, () => run(api.transformKey(props!.clip, pr.prop)))}
        </div>
      {:else if r.kind === "fx"}
        {@const fx = r.fx}
        <div class="fxrow" class:off={!fx.enabled}>
          <button class="twirl" class:open={r.open} onclick={() => twirl(fx.index)} title={r.open ? "Hide its settings" : "Show its settings"} aria-label="{fx.label} settings" aria-expanded={r.open} disabled={!fx.params.length}>
            <svg viewBox="0 0 10 10" width="8" height="8" aria-hidden="true"><path d="M3 1.5 7 5 3 8.5" /></svg>
          </button>
          <input type="checkbox" checked={fx.enabled} disabled={locked} onchange={() => run(api.effectOp(props!.clip, fx.index, "toggle"))} aria-label="{fx.label} on" title="Turn {fx.label} on or off" />
          <span class="fxname" title={fx.label}>{fx.label}{#if fx.params.some((p) => p.animated)}<span class="dot" title="Keyframed"></span>{/if}</span>
        </div>
      {:else}
        {@const p = r.param}
        {@const ix = r.fx.index}
        {@const v = pval(ix, r.pi, p)}
        <div class="row param">
          {@render watch(p.animated, p.name, () => run(api.effectStopwatch(props!.clip, ix, r.pi)))}
          <span class="lbl" class:anim={p.animated} title={p.name}>{p.name}</span>
          {#if p.ui === "toggle"}
            <input type="checkbox" class="ptoggle" checked={v >= 0.5} disabled={locked} onchange={(e) => setNow(ix, r.pi, e.currentTarget.checked ? 1 : 0)} aria-label={p.name} />
          {:else if p.ui === "choice"}
            <select class="pchoice" value={Math.round(v)} disabled={locked} onchange={(e) => setNow(ix, r.pi, Number(e.currentTarget.value))} aria-label={p.name}>
              {#each p.options as o, oi}<option value={oi}>{o}</option>{/each}
            </select>
          {:else if p.ui === "color"}
            <input type="color" class="pcolor" value={hex(v)} disabled={locked} onchange={(e) => setNow(ix, r.pi, parseInt(e.currentTarget.value.slice(1), 16))} aria-label={p.name} />
          {:else}
            <NumberField label={p.name} value={v} min={p.min} max={p.max} step={p.step} digits={p.decimals} suffix={p.suffix} oninput={(x) => setP(ix, r.pi, x)} oncommit={commit} />
          {/if}
          {@render keyBtn(p.animated, p.key_here, () => run(api.effectKey(props!.clip, ix, r.pi)))}
        </div>
      {/if}
    {/each}
  {/if}
</div>

<style>
  .tp {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
    padding: 0 0 4px 0;
    font-size: calc(11px * var(--text-scale));
  }
  .tp.locked {
    opacity: 0.6;
  }
  .tp.locked :global(.num) {
    pointer-events: none;
  }
  .row {
    display: grid;
    grid-template-columns: 14px 46px minmax(0, 1fr) 12px;
    gap: 3px;
    align-items: center;
    height: 18px;
  }
  .row.two {
    grid-template-columns: 14px 46px minmax(0, 1fr) minmax(0, 1fr) 12px;
  }
  .row.param {
    padding-left: 10px;
  }
  .row :global(.num) {
    overflow: hidden;
    height: 18px;
    min-height: 18px;
    min-width: 0;
    font-size: calc(11px * var(--text-scale));
    padding: 0 3px;
  }
  .lbl {
    color: var(--muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .lbl.anim {
    color: var(--text);
  }
  button.watch,
  button.key,
  button.twirl {
    all: unset;
    display: grid;
    place-items: center;
    width: 14px;
    height: 16px;
    cursor: pointer;
    color: var(--muted);
  }
  button.key {
    width: 12px;
  }
  span.key {
    width: 12px;
  }
  button:disabled {
    cursor: default;
    opacity: 0.5;
  }
  .watch svg {
    fill: none;
    stroke: currentColor;
    stroke-width: 1.3;
    stroke-linecap: round;
  }
  .watch.on {
    color: var(--accent);
  }
  .key svg {
    fill: none;
    stroke: var(--accent);
    stroke-width: 1.3;
  }
  .key.here svg {
    fill: var(--accent);
  }
  .twirl svg {
    fill: none;
    stroke: currentColor;
    stroke-width: 1.6;
    stroke-linecap: round;
    stroke-linejoin: round;
    transition: transform 0.12s;
  }
  .twirl.open svg {
    transform: rotate(90deg);
  }
  .clipname {
    all: unset;
    cursor: pointer;
    color: var(--text);
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    height: 18px;
    line-height: 18px;
    padding-left: 2px;
  }
  .clipname:hover {
    color: var(--accent);
  }
  .fxrow {
    display: flex;
    align-items: center;
    gap: 3px;
    height: 18px;
    min-width: 0;
  }
  .fxrow input {
    margin: 0;
    width: 12px;
    height: 12px;
    min-height: 0;
    flex: none;
  }
  .fxname {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .fxrow.off .fxname {
    color: var(--muted);
    text-decoration: line-through;
  }
  .dot {
    display: inline-block;
    width: 5px;
    height: 5px;
    margin-left: 4px;
    border-radius: 50%;
    background: var(--accent);
    vertical-align: middle;
  }
  .ptoggle {
    margin: 0;
    width: 12px;
    height: 12px;
    min-height: 0;
    justify-self: start;
  }
  .pchoice {
    height: 18px;
    min-height: 18px;
    min-width: 0;
    padding: 0 2px;
    font-size: calc(11px * var(--text-scale));
  }
  .pcolor {
    width: 100%;
    height: 16px;
    min-height: 0;
    padding: 0;
    border: 1px solid var(--line);
    background: none;
  }
  .none {
    margin: 0;
    color: var(--muted);
    height: 18px;
    line-height: 18px;
    padding-left: 18px;
  }
</style>
