<script lang="ts">
  // The Viewer's multicam view: every angle of the multicam clip at the
  // playhead in a grid (drawn by the renderer), with each cell's name, the
  // angle seen outlined and the one heard marked. Click a cell to cut to
  // it at the playhead (Shift: the picture only).
  import { app, run } from "../lib/session.svelte";
  import { api } from "../lib/api";

  let { fit }: { fit: { x: number; y: number; w: number; h: number; k: number } } = $props();

  const mc = $derived(app.live?.multicam ?? null);
  const n = $derived(Math.min(mc?.angles.length ?? 0, 16));
  // As reelforge_core::multicam::grid_size.
  const cols = $derived(n <= 1 ? 1 : n <= 4 ? 2 : n <= 9 ? 3 : 4);
  const rows = $derived(Math.max(1, Math.min(cols, Math.ceil(n / cols))));

  function pick(e: MouseEvent, i: number) {
    e.stopPropagation();
    run(api.switchAngle(i, e.shiftKey));
  }
</script>

{#if mc && mc.grid && n > 0}
  <div class="grid" style="left:{fit.x}px; top:{fit.y}px; width:{fit.w}px; height:{fit.h}px; grid-template-columns:repeat({cols},1fr); grid-template-rows:repeat({rows},1fr)">
    {#each mc.angles.slice(0, n) as name, i}
      <button
        class="cell"
        class:seen={mc.video === i}
        onpointerdown={(e) => e.stopPropagation()}
        onclick={(e) => pick(e, i)}
        title="Cut to {name} at the playhead ({i < 9 ? `${i + 1}` : 'click'}; Shift: picture only, the sound stays)"
      >
        <span class="name"><b>{i + 1}</b> {name}{#if mc.heard === i}<span class="heard" title="The sound heard">♪</span>{/if}</span>
      </button>
    {/each}
  </div>
{/if}

<style>
  .grid {
    position: absolute;
    display: grid;
    pointer-events: none;
  }
  .cell {
    pointer-events: auto;
    background: transparent;
    border: 1px solid rgba(255, 255, 255, 0.18);
    border-radius: 0;
    padding: 0;
    margin: 0;
    position: relative;
    cursor: pointer;
    min-width: 0;
    min-height: 0;
  }
  .cell:hover {
    border-color: rgba(255, 255, 255, 0.6);
  }
  .cell.seen {
    border: 3px solid var(--accent);
  }
  .name {
    position: absolute;
    left: 6px;
    bottom: 6px;
    max-width: calc(100% - 12px);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    background: rgba(0, 0, 0, 0.6);
    color: #fff;
    font-size: calc(11.5px * var(--text-scale));
    padding: 2px 6px;
    border-radius: 3px;
  }
  .seen .name {
    background: var(--accent);
  }
  .heard {
    margin-left: 6px;
    color: #7ee08f;
  }
  .seen .heard {
    color: #fff;
  }
</style>
