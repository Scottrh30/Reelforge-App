<script lang="ts">
  // The Curves effect's tone curves: drag a point, click on an empty spot to
  // add one there (and drag it), right-click or double-click a point to remove it.
  import type { Curves } from "../lib/api";
  import { evalCurve, type P } from "../lib/curve";

  let { curves, onchange }: { curves: Curves; onchange: (c: Curves, done: boolean) => void } = $props();

  const channels: { key: keyof Curves; name: string; color: string }[] = [
    { key: "master", name: "Master", color: "var(--text)" },
    { key: "red", name: "Red", color: "#eb5050" },
    { key: "green", name: "Green", color: "#50c85a" },
    { key: "blue", name: "Blue", color: "#5a8cff" },
  ];
  let ch = $state(0);
  // While dragging: our own copy, so the line keeps up with the pointer.
  let draft = $state<Curves | null>(null);
  const shown = $derived(draft ?? curves);
  const S = 200;
  const path = (pts: P[]) => {
    let d = "";
    for (let k = 0; k <= 96; k++) {
      const x = k / 96;
      d += `${k ? "L" : "M"}${(x * S).toFixed(1)},${((1 - evalCurve(pts, x)) * S).toFixed(1)}`;
    }
    return d;
  };
  const copy = (): Curves => structuredClone($state.snapshot(shown)) as Curves;

  let svg: SVGSVGElement;
  function at(e: PointerEvent | MouseEvent): P {
    const r = svg.getBoundingClientRect();
    return [Math.min(1, Math.max(0, (e.clientX - r.left) / r.width)), Math.min(1, Math.max(0, 1 - (e.clientY - r.top) / r.height))];
  }
  function near(pts: P[], e: PointerEvent | MouseEvent): number {
    const r = svg.getBoundingClientRect();
    let best = -1;
    let bd = 9;
    pts.forEach((p, i) => {
      const dx = (p[0] - (e.clientX - r.left) / r.width) * r.width;
      const dy = (1 - p[1] - (e.clientY - r.top) / r.height) * r.height;
      const d = Math.hypot(dx, dy);
      if (d < bd) {
        bd = d;
        best = i;
      }
    });
    return best;
  }

  function down(e: PointerEvent) {
    if (e.button !== 0) return;
    const c = copy();
    const key = channels[ch].key;
    let pts = c[key];
    let i = near(pts, e);
    if (i < 0) {
      const [x] = at(e);
      pts.push([x, evalCurve(pts, x)]);
      pts.sort((a, b) => a[0] - b[0]);
      i = pts.findIndex((p) => p[0] === x);
    }
    let idx = i;
    draft = c;
    onchange(c, false);
    svg.setPointerCapture(e.pointerId);
    const move = (ev: PointerEvent) => {
      const n = copy();
      pts = n[key];
      let [x, y] = at(ev);
      // Inner points stay between their neighbours; the ends can move up and down and inwards.
      const lo = idx > 0 ? pts[idx - 1][0] + 0.01 : 0;
      const hi = idx + 1 < pts.length ? pts[idx + 1][0] - 0.01 : 1;
      x = Math.min(Math.max(x, lo), Math.max(lo, hi));
      pts[idx] = [x, y];
      draft = n;
      onchange(n, false);
    };
    const up = () => {
      svg.removeEventListener("pointermove", move);
      svg.removeEventListener("pointerup", up);
      const n = copy();
      draft = null;
      onchange(n, true);
    };
    svg.addEventListener("pointermove", move);
    svg.addEventListener("pointerup", up);
  }

  function remove(e: MouseEvent) {
    e.preventDefault();
    const c = copy();
    const pts = c[channels[ch].key];
    const i = near(pts, e);
    if (i >= 0 && pts.length > 2) {
      pts.splice(i, 1);
      onchange(c, true);
    }
  }

  function reset() {
    const c = copy();
    c[channels[ch].key] = [
      [0, 0],
      [1, 1],
    ];
    onchange(c, true);
  }
</script>

<div class="curves">
  <div class="tabs" role="tablist" aria-label="Channel">
    {#each channels as c, i}
      <button role="tab" aria-selected={ch === i} class:on={ch === i} onclick={() => (ch = i)} style="--c:{c.color}">{c.name}</button>
    {/each}
    <span class="grow"></span>
    <button class="small" onclick={reset} title="Straighten this channel's curve">Reset</button>
  </div>
  <svg
    bind:this={svg}
    viewBox="0 0 {S} {S}"
    role="application"
    aria-label="Tone curve: drag points; click to add one; right-click a point to remove it"
    onpointerdown={down}
    oncontextmenu={remove}
    ondblclick={remove}
  >
    <rect width={S} height={S} class="bg" />
    {#each [1, 2, 3] as k}
      <line x1={(k * S) / 4} y1="0" x2={(k * S) / 4} y2={S} class="grid" />
      <line x1="0" y1={(k * S) / 4} x2={S} y2={(k * S) / 4} class="grid" />
    {/each}
    <line x1="0" y1={S} x2={S} y2="0" class="diag" />
    {#each channels as c, i}
      {#if i !== ch}<path d={path(shown[c.key])} stroke={c.color} class="faint" />{/if}
    {/each}
    <path d={path(shown[channels[ch].key])} stroke={channels[ch].color} class="line" />
    {#each shown[channels[ch].key] as p}
      <circle cx={p[0] * S} cy={(1 - p[1]) * S} r="4.5" stroke={channels[ch].color} class="pt" />
    {/each}
  </svg>
  <p class="hint">Drag a point; click the line to add one; right-click a point to remove it.</p>
</div>

<style>
  .curves {
    display: flex;
    flex-direction: column;
    gap: 6px;
    margin-top: 6px;
  }
  .tabs {
    display: flex;
    gap: 2px;
    align-items: center;
  }
  .tabs button {
    min-height: 22px;
    padding: 0 7px;
    font-size: calc(12px * var(--text-scale));
    background: none;
    border-color: transparent;
    color: var(--muted);
  }
  .tabs button.on {
    color: var(--c);
    border-color: var(--line);
    background: var(--raised);
  }
  .tabs .small {
    color: var(--text);
    border-color: var(--btn-border);
    background: var(--btn);
  }
  .grow {
    flex: 1;
  }
  svg {
    width: 100%;
    max-width: 240px;
    aspect-ratio: 1;
    cursor: crosshair;
    touch-action: none;
    border-radius: 4px;
    display: block;
  }
  .bg {
    fill: var(--well);
  }
  .grid {
    stroke: var(--line-soft);
    stroke-width: 1;
  }
  .diag {
    stroke: var(--line);
    stroke-width: 1;
    stroke-dasharray: 3 3;
  }
  path {
    fill: none;
  }
  .faint {
    stroke-width: 1;
    opacity: 0.35;
  }
  .line {
    stroke-width: 2;
  }
  .pt {
    fill: var(--well);
    stroke-width: 1.6;
  }
  .hint {
    margin: 0;
    color: var(--faint);
    font-size: calc(11.5px * var(--text-scale));
  }
</style>
