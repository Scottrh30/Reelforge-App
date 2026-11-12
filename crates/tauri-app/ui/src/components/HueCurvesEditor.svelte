<script lang="ts">
  // The Hue curves effect's five curves. The middle line is "no change":
  // drag a point up or down to turn that hue (Hue vs hue), or to raise or
  // lower saturation or brightness there. Click the line to add a point (on
  // an empty curve, two neutral neighbours come with it so only the hues
  // nearby change); right-click or double-click a point to remove it. The
  // first three wrap round: hue is a circle.
  import type { HueCurves } from "../lib/api";
  import { evalCurve, type P } from "../lib/curve";

  let { curves, onchange }: { curves: HueCurves; onchange: (c: HueCurves, done: boolean) => void } = $props();

  type Key = keyof HueCurves;
  const TABS: { key: Key; name: string; short: string; hue: boolean; tip: string }[] = [
    { key: "hue_hue", name: "Hue vs hue", short: "Hue → hue", hue: true, tip: "Turn one hue into another: up turns it one way round the wheel, down the other" },
    { key: "hue_sat", name: "Hue vs saturation", short: "Hue → sat", hue: true, tip: "More or less saturation for some hues (down to grey at the bottom)" },
    { key: "hue_lum", name: "Hue vs luminance", short: "Hue → lum", hue: true, tip: "Brighter or darker for some hues (greys are left alone)" },
    { key: "lum_sat", name: "Luminance vs saturation", short: "Lum → sat", hue: false, tip: "More or less saturation by brightness: take the colour out of the shadows or highlights" },
    { key: "sat_sat", name: "Saturation vs saturation", short: "Sat → sat", hue: false, tip: "Boost the muted colours without pushing the strong ones (or the other way)" },
  ];
  let tab = $state(0);
  const cur = $derived(TABS[tab]);
  let draft = $state<HueCurves | null>(null);
  const shown = $derived(draft ?? curves);
  const S = 200;
  const H = 120;

  /// A curve at x (no points: 0.5, no change); the hue ones wrap round.
  function evalAt(pts: P[], x: number, wrap: boolean): number {
    if (!pts.length) return 0.5;
    if (!wrap) return evalCurve(pts, x);
    const p: P[] = [];
    for (const q of pts) {
      const qx = ((q[0] % 1) + 1) % 1;
      p.push([qx - 1, q[1]], [qx, q[1]], [qx + 1, q[1]]);
    }
    return Math.min(1, Math.max(0, evalCurve(p, ((x % 1) + 1) % 1)));
  }
  const path = (pts: P[], wrap: boolean) => {
    let d = "";
    for (let k = 0; k <= 120; k++) {
      const x = k / 120;
      d += `${k ? "L" : "M"}${(x * S).toFixed(1)},${((1 - evalAt(pts, x, wrap)) * H).toFixed(1)}`;
    }
    return d;
  };
  const copy = (): HueCurves => structuredClone($state.snapshot(shown)) as HueCurves;

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
      const d = Math.hypot((p[0] - (e.clientX - r.left) / r.width) * r.width, (1 - p[1] - (e.clientY - r.top) / r.height) * r.height);
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
    const key = cur.key;
    const wrap = cur.hue;
    let pts = c[key];
    let i = near(pts, e);
    if (i < 0) {
      const [x] = at(e);
      if (!pts.length) {
        // Neutral neighbours, so only the hues (or levels) nearby change.
        if (wrap) pts.push([(x + 0.92) % 1, 0.5], [(x + 0.08) % 1, 0.5]);
        else pts.push([0, 0.5], [1, 0.5]);
      }
      pts.push([x, evalAt(pts, x, wrap)]);
      pts.sort((a, b) => a[0] - b[0]);
      i = pts.findIndex((p) => p[0] === x);
    }
    const idx = i;
    draft = c;
    onchange(c, false);
    svg.setPointerCapture(e.pointerId);
    const move = (ev: PointerEvent) => {
      const n = copy();
      pts = n[key];
      let [x, y] = at(ev);
      const lo = idx > 0 ? pts[idx - 1][0] + 0.005 : 0;
      const hi = idx + 1 < pts.length ? pts[idx + 1][0] - 0.005 : 1;
      x = Math.min(Math.max(x, lo), Math.max(lo, hi));
      // Shift: up and down only.
      if (ev.shiftKey) x = pts[idx][0];
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
    const pts = c[cur.key];
    const i = near(pts, e);
    if (i >= 0) {
      pts.splice(i, 1);
      onchange(c, true);
    }
  }
  function reset() {
    const c = copy();
    c[cur.key] = [];
    onchange(c, true);
  }
  const used = (k: Key) => shown[k].some((p) => Math.abs(p[1] - 0.5) > 0.004);
  // The strip under the curve: what's along the x axis.
  const strip = $derived(
    cur.hue
      ? "linear-gradient(to right, #f00, #ff0 16.7%, #0f0 33.3%, #0ff 50%, #00f 66.7%, #f0f 83.3%, #f00)"
      : cur.key === "lum_sat"
        ? "linear-gradient(to right, #000, #fff)"
        : "linear-gradient(to right, #808080, #e03030)",
  );
  const yLabel = $derived(cur.key === "hue_hue" ? ["+180°", "−180°"] : cur.key === "hue_lum" ? ["×2", "×0"] : ["×2", "grey"]);
</script>

<div class="hc">
  <div class="tabs" role="tablist" aria-label="Curve">
    {#each TABS as t, i}
      <button role="tab" aria-selected={tab === i} class:on={tab === i} class:used={used(t.key)} onclick={() => (tab = i)} title={t.tip}>{t.short}</button>
    {/each}
    <span class="grow"></span>
    <button class="small" onclick={reset} disabled={!shown[cur.key].length} title="Flatten this curve (no change)">Reset</button>
  </div>
  <div class="plot">
    <span class="yl top">{yLabel[0]}</span>
    <span class="yl bottom">{yLabel[1]}</span>
    <svg
      bind:this={svg}
      viewBox="0 0 {S} {H}"
      preserveAspectRatio="none"
      role="application"
      aria-label="{cur.name}: drag points; click the line to add one; right-click a point to remove it"
      onpointerdown={down}
      oncontextmenu={remove}
      ondblclick={remove}
    >
      <rect width={S} height={H} class="bg" />
      {#each [1, 2, 3, 4, 5] as k}
        <line x1={(k * S) / 6} y1="0" x2={(k * S) / 6} y2={H} class="grid" />
      {/each}
      <line x1="0" y1={H / 2} x2={S} y2={H / 2} class="mid" />
      <path d={path(shown[cur.key], cur.hue)} class="line" vector-effect="non-scaling-stroke" />
      {#each shown[cur.key] as p}
        <ellipse cx={p[0] * S} cy={(1 - p[1]) * H} rx="3.6" ry={3.6 * (H / S) * 1.6} class="pt" vector-effect="non-scaling-stroke" />
      {/each}
    </svg>
    <div class="strip" style="background:{strip}" aria-hidden="true"></div>
  </div>
  <p class="hint">{cur.tip}. Click the line to add a point, drag it (Shift: up and down only), right-click to remove it.</p>
</div>

<style>
  .hc {
    display: flex;
    flex-direction: column;
    gap: 6px;
    margin-top: 6px;
  }
  .tabs {
    display: flex;
    flex-wrap: wrap;
    gap: 2px;
    align-items: center;
  }
  .tabs button {
    min-height: 22px;
    padding: 0 6px;
    font-size: calc(11.5px * var(--text-scale));
    background: none;
    border-color: transparent;
    color: var(--muted);
  }
  .tabs button.used {
    color: var(--text);
  }
  .tabs button.used::after {
    content: " •";
    color: var(--accent);
  }
  .tabs button.on {
    border-color: var(--line);
    background: var(--raised);
    color: var(--text);
  }
  .tabs .small {
    color: var(--text);
    border-color: var(--btn-border);
    background: var(--btn);
  }
  .grow {
    flex: 1;
  }
  .plot {
    position: relative;
    max-width: 300px;
  }
  svg {
    width: 100%;
    height: 132px;
    cursor: crosshair;
    touch-action: none;
    display: block;
    border-radius: 4px 4px 0 0;
  }
  .strip {
    height: 8px;
    border-radius: 0 0 4px 4px;
  }
  .yl {
    position: absolute;
    right: 4px;
    font-size: calc(10px * var(--text-scale));
    color: var(--muted);
    pointer-events: none;
  }
  .yl.top {
    top: 2px;
  }
  .yl.bottom {
    top: 116px;
  }
  .bg {
    fill: var(--well);
  }
  .grid {
    stroke: var(--line-soft);
    stroke-width: 0.6;
  }
  .mid {
    stroke: var(--line);
    stroke-width: 0.8;
    stroke-dasharray: 3 3;
  }
  .line {
    fill: none;
    stroke: var(--accent);
    stroke-width: 1.8;
  }
  .pt {
    fill: var(--well);
    stroke: var(--accent);
    stroke-width: 1.6;
  }
  .hint {
    margin: 0;
    color: var(--muted);
    font-size: calc(11.5px * var(--text-scale));
  }
</style>
