<script lang="ts">
  // How well a camera solve fits each frame: a bar per frame (average pixels
  // between the solved points and the followed details), orange (as in the
  // Viewer) over the "fitting worse than" limit, the playhead's frame marked. Click or drag
  // to go to a frame.
  let { fit, frame, limit, onseek }: { fit: (number | null)[]; frame: number; limit: number; onseek: (frame: number) => void } = $props();

  const W = 240;
  const H = 46;
  const top = $derived(Math.max(limit * 1.5, 1, ...fit.map((v) => v ?? 0)));
  const y = (v: number) => H - Math.min(v / top, 1) * (H - 2);
  const bw = $derived(W / Math.max(fit.length, 1));
  let hover = $state<number | null>(null);
  const at = (ev: PointerEvent) => {
    const r = (ev.currentTarget as SVGElement).getBoundingClientRect();
    return Math.max(0, Math.min(fit.length - 1, Math.floor(((ev.clientX - r.left) / r.width) * fit.length)));
  };
  const worst = $derived(fit.reduce<number | null>((w, v, i) => (v !== null && (w === null || v > (fit[w] ?? 0)) ? i : w), null));
</script>

<div class="fit">
  <svg
    viewBox="0 0 {W} {H}"
    preserveAspectRatio="none"
    role="slider"
    tabindex="0"
    aria-label="How well the solve fits each frame: click to go there"
    aria-valuemin={1}
    aria-valuemax={fit.length}
    aria-valuenow={frame + 1}
    onpointerdown={(e) => {
      (e.currentTarget as SVGElement).setPointerCapture(e.pointerId);
      onseek(at(e));
    }}
    onpointermove={(e) => {
      hover = at(e);
      if (e.buttons & 1) onseek(hover);
    }}
    onpointerleave={() => (hover = null)}
    onkeydown={(e) => {
      if (e.key === "ArrowLeft") onseek(Math.max(0, frame - 1));
      else if (e.key === "ArrowRight") onseek(Math.min(fit.length - 1, frame + 1));
    }}
  >
    {#each fit as v, i}
      {#if v !== null}
        <rect x={i * bw} y={y(v)} width={Math.max(bw - (bw > 3 ? 0.5 : 0), 0.6)} height={H - y(v)} class:bad={v > limit} />
      {:else}
        <rect x={i * bw} y={H - 2} width={Math.max(bw, 0.6)} height={2} class="none" />
      {/if}
    {/each}
    <line x1="0" x2={W} y1={y(limit)} y2={y(limit)} class="limit" />
    <line x1={(frame + 0.5) * bw} x2={(frame + 0.5) * bw} y1="0" y2={H} class="here" />
  </svg>
  <div class="cap">
    {#if hover !== null}
      Frame {hover + 1}: {fit[hover] === null ? "no points" : `${fit[hover]!.toFixed(2)} px`}
    {:else if worst !== null}
      Worst: frame {worst + 1}, {fit[worst]!.toFixed(2)} px · dashed: {limit.toFixed(1)} px
    {/if}
  </div>
</div>

<style>
  .fit svg {
    width: 100%;
    height: 46px;
    display: block;
    background: var(--base);
    border: 1px solid var(--line);
    border-radius: 4px;
    cursor: pointer;
  }
  rect {
    fill: var(--muted);
    opacity: 0.85;
  }
  rect.bad {
    fill: #f0902c;
    opacity: 1;
  }
  rect.none {
    fill: var(--faint);
  }
  .limit {
    stroke: var(--muted);
    stroke-dasharray: 3 3;
    stroke-width: 1;
    vector-effect: non-scaling-stroke;
  }
  .here {
    stroke: var(--playhead, #e33);
    stroke-width: 1.5;
    vector-effect: non-scaling-stroke;
  }
  .cap {
    color: var(--muted);
    font-size: calc(11px * var(--text-scale));
    margin-top: 2px;
    min-height: 1.3em;
  }
</style>
