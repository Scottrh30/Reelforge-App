<script lang="ts">
  // An expanded video track's header: the clip shown there (the selected one
  // on the track, else the one at the playhead) — its effects, each to switch
  // on or off, and its opacity, position, anchor point and rotation as seen
  // at the playhead (keyframed ones get a key there, as in Controls).
  import { api, type TrackProps, type Transform } from "../lib/api";
  import { app, run } from "../lib/session.svelte";
  import NumberField from "./NumberField.svelte";

  let { props, locked }: { props: TrackProps | null; locked: boolean } = $props();

  let draft = $state<Transform | null>(null);
  let shownId = -1;
  $effect(() => {
    const id = props?.clip ?? -1;
    if (id !== shownId) {
      shownId = id;
      draft = null;
    }
  });
  const t = $derived(draft ?? props?.transform ?? null);

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
  }
  const keyed = (i: number, what: string) => (props?.animated[i] ? `${what} (keyframed: changing it adds a key at the playhead)` : what);
  const select = () => props && run(api.select(props.clip, "only"));
</script>

<!-- (Its own pointer handling: dragging a value mustn't drag the track.) -->
<div class="tp" class:locked role="group" aria-label="Clip properties" onpointerdown={(e) => e.stopPropagation()}>
  {#if !props || !t}
    <p class="none">No clip here at the playhead</p>
  {:else}
    <button class="clipname" onclick={select} title="Select {props.name}">{props.name}</button>
    <div class="row">
      <span class="lbl" class:anim={props.animated[0]} title={keyed(0, "Opacity")}>Opacity</span>
      <NumberField label="Opacity" value={Math.round(t.opacity * 1000) / 10} min={0} max={100} step={0.5} digits={0} suffix="%" oninput={(v) => setT((n) => (n.opacity = v / 100))} oncommit={commit} />
    </div>
    <div class="row two">
      <span class="lbl" class:anim={props.animated[1]} title={keyed(1, "Position (pixels from the middle)")}>Position</span>
      <NumberField label="Position X" value={t.position[0]} digits={0} oninput={(v) => setT((n) => (n.position[0] = v))} oncommit={commit} />
      <NumberField label="Position Y" value={t.position[1]} digits={0} oninput={(v) => setT((n) => (n.position[1] = v))} oncommit={commit} />
    </div>
    <div class="row two">
      <span class="lbl" class:anim={props.animated[2]} title={keyed(2, "Anchor point (what it scales and turns about)")}>Anchor</span>
      <NumberField label="Anchor X" value={t.anchor[0]} digits={0} oninput={(v) => setT((n) => (n.anchor[0] = v))} oncommit={commit} />
      <NumberField label="Anchor Y" value={t.anchor[1]} digits={0} oninput={(v) => setT((n) => (n.anchor[1] = v))} oncommit={commit} />
    </div>
    <div class="row">
      <span class="lbl" class:anim={props.animated[3]} title={keyed(3, "Rotation")}>Rotation</span>
      <NumberField label="Rotation" value={t.rotation} step={0.5} digits={1} suffix="°" oninput={(v) => setT((n) => (n.rotation = v))} oncommit={commit} />
    </div>
    {#each props.effects as fx (fx.index)}
      <label class="fx" class:off={!fx.enabled} title="Turn {fx.label} on or off">
        <input type="checkbox" checked={fx.enabled} disabled={locked} onchange={() => run(api.effectOp(props!.clip, fx.index, "toggle"))} aria-label="{fx.label} on" />
        <span>{fx.label}</span>
      </label>
    {/each}
    {#if !props.effects.length}
      <p class="none">No effects</p>
    {/if}
  {/if}
</div>

<style>
  .tp {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
    padding: 2px 0 4px 18px;
    font-size: calc(11px * var(--text-scale));
  }
  .tp.locked {
    opacity: 0.6;
    pointer-events: none;
  }
  .row {
    display: grid;
    grid-template-columns: 50px 1fr;
    gap: 3px;
    align-items: center;
    height: 18px;
  }
  .row.two {
    grid-template-columns: 50px 1fr 1fr;
  }
  .row :global(.num) {
    height: 18px;
    min-height: 18px;
    font-size: calc(11px * var(--text-scale));
    padding: 0 3px;
  }
  .lbl {
    position: relative;
    overflow: visible;
    color: var(--muted);
    text-overflow: clip;
    white-space: nowrap;
  }
  .lbl.anim {
    color: var(--text);
  }
  .lbl.anim::before {
    content: "◆";
    position: absolute;
    left: -11px;
    color: var(--accent);
    font-size: 8px;
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
  }
  .clipname:hover {
    color: var(--accent);
  }
  .fx {
    display: flex;
    align-items: center;
    gap: 4px;
    height: 18px;
    min-width: 0;
    cursor: pointer;
  }
  .fx span {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .fx.off span {
    color: var(--muted);
    text-decoration: line-through;
  }
  .fx input {
    margin: 0;
    width: 12px;
    height: 12px;
    min-height: 0;
  }
  .none {
    margin: 0;
    color: var(--muted);
    height: 18px;
    line-height: 18px;
  }
</style>
