<script lang="ts">
  // Masks and the motion tracker of the selected clip, drawn over the Viewer
  // picture: drag a mask's points (or inside the mask being edited to move it
  // all), click out a freehand mask, and drag tracker boxes onto details.
  import { app, run, say } from "../lib/session.svelte";
  import { api, type Pt } from "../lib/api";
  import { finishDrawing } from "../lib/actions.svelte";
  import { layerToFrame, frameToLayer, inside } from "../lib/geom";
  import { livePump } from "../lib/live";

  let { fit, box }: { fit: { k: number; x: number; y: number }; box: HTMLElement | undefined } = $props();

  const MASK = "#ff6ec8";
  const POINT_COLOURS = ["#50dcff", "#ffd246", "#ff6ec8", "#78f078"];

  const ins = $derived(app.s?.inspector ?? null);
  const map = $derived(ins?.layer_map ?? null);
  const inv = $derived(ins?.layer_inv ?? null);
  const size = $derived(ins?.layer_px ?? ([1, 1] as Pt));
  const active = $derived(app.activeMask && ins && app.activeMask.clip === ins.id ? app.activeMask.index : null);
  const drawing = $derived(app.drawing && ins && app.drawing.clip === ins.id ? app.drawing : null);
  const busyTracking = $derived(!!app.live?.tracking);
  const tool = $derived(ins && ins.masks.length && !app.drawing ? app.maskTool : null);

  // Forget an edit target that no longer exists.
  $effect(() => {
    if (app.activeMask && (!ins || app.activeMask.clip !== ins.id || app.activeMask.index >= ins.masks.length)) app.activeMask = null;
    if (app.drawing && (!ins || app.drawing.clip !== ins.id)) app.drawing = null;
  });

  /// Layer pixels → box pixels.
  function toBox(p: Pt): Pt {
    const [fx, fy] = layerToFrame(map!, size, p);
    return [fit.x + fx * fit.k, fit.y + fy * fit.k];
  }
  /// A pointer position (client pixels) → layer pixels.
  function toLayer(e: PointerEvent | MouseEvent): Pt {
    const r = box!.getBoundingClientRect();
    return frameToLayer(inv!, size, [(e.clientX - r.left - fit.x) / fit.k, (e.clientY - r.top - fit.y) / fit.k]);
  }
  const pts = (list: Pt[]) => list.map((p) => toBox(p).join(",")).join(" ");
  const square = (c: Pt, r: number): Pt[] => [
    [c[0] - r, c[1] - r],
    [c[0] + r, c[1] - r],
    [c[0] + r, c[1] + r],
    [c[0] - r, c[1] + r],
  ];

  /// Drag mask `index`: one point (`point`), all of them (`null`), or resize
  /// it about its centre (`"resize"`; Shift changes only its width or height).
  function dragMask(e: PointerEvent, index: number, point: number | null | "resize") {
    if (e.button !== 0 || !ins || !inv || app.live?.playing) return;
    e.stopPropagation();
    e.preventDefault();
    const id = ins.id;
    app.activeMask = { clip: id, index };
    const el = e.currentTarget as SVGElement;
    el.setPointerCapture(e.pointerId);
    const start = toLayer(e);
    const orig = ins.masks[index].handles.map((p) => [...p] as Pt);
    const c: Pt = [orig.reduce((a, p) => a + p[0], 0) / orig.length, orig.reduce((a, p) => a + p[1], 0) / orig.length];
    const pump = livePump();
    const move = (ev: PointerEvent) => {
      const now = toLayer(ev);
      let next: Pt[];
      if (point === "resize") {
        const d0 = Math.hypot(start[0] - c[0], start[1] - c[1]) || 1;
        let [kx, ky] = [Math.hypot(now[0] - c[0], now[1] - c[1]) / d0, 0];
        ky = kx;
        if (ev.shiftKey) {
          // Width or height only, whichever way the drag mostly goes.
          const wide = Math.abs(start[0] - c[0]) >= Math.abs(start[1] - c[1]);
          if (wide) [kx, ky] = [Math.abs(now[0] - c[0]) / (Math.abs(start[0] - c[0]) || 1), 1];
          else [kx, ky] = [1, Math.abs(now[1] - c[1]) / (Math.abs(start[1] - c[1]) || 1)];
        }
        kx = Math.max(0.02, kx);
        ky = Math.max(0.02, ky);
        next = orig.map((p) => [c[0] + (p[0] - c[0]) * kx, c[1] + (p[1] - c[1]) * ky] as Pt);
      } else {
        const [dx, dy] = [now[0] - start[0], now[1] - start[1]];
        next = orig.map((p, j) => (point === null || point === j ? ([p[0] + dx, p[1] + dy] as Pt) : p));
      }
      pump.push(() => api.setMaskPoints(id, index, next));
    };
    const up = async () => {
      el.removeEventListener("pointermove", move);
      el.removeEventListener("pointerup", up);
      await pump.done();
      await run(api.commit());
    };
    el.addEventListener("pointermove", move);
    el.addEventListener("pointerup", up);
  }

  /// With the Move or Resize tool: the mask under the pointer (the one being
  /// edited first), and drag it.
  function toolDown(e: PointerEvent) {
    if (!ins || !inv || e.button !== 0) return;
    e.stopPropagation();
    const p = toLayer(e);
    const order = ins.masks.map((_, i) => i).sort((a, b) => (a === active ? -1 : b === active ? 1 : b - a));
    const hit = order.find((i) => ins.masks[i].mask.enabled && inside(ins.masks[i].outline, p));
    if (hit === undefined) {
      app.activeMask = null;
      return;
    }
    dragMask(e, hit, app.maskTool === "resize" ? "resize" : null);
  }

  /// Drag tracker point `i`'s box onto a detail.
  function dragTracker(e: PointerEvent, i: number) {
    const t = ins?.tracker;
    if (e.button !== 0 || !ins || !t || !inv || app.live?.playing || busyTracking) return;
    e.stopPropagation();
    e.preventDefault();
    const id = ins.id;
    const el = e.currentTarget as SVGElement;
    el.setPointerCapture(e.pointerId);
    const start = toLayer(e);
    const from = [...t.shown[i]] as Pt;
    const pump = livePump();
    const move = (ev: PointerEvent) => {
      const now = toLayer(ev);
      pump.push(() => api.trackerMove(id, i, [from[0] + now[0] - start[0], from[1] + now[1] - start[1]]));
    };
    const up = async () => {
      el.removeEventListener("pointermove", move);
      el.removeEventListener("pointerup", up);
      await pump.done();
      await run(api.commit());
    };
    el.addEventListener("pointermove", move);
    el.addEventListener("pointerup", up);
  }

  // Energy blades of the selected clip: hilt and tip handles (% of the layer).
  type Blade = { index: number; hilt: Pt; tip: Pt; colour: string };
  const blades = $derived.by((): Blade[] => {
    if (!ins) return [];
    return ins.effects.flatMap((fx, index) => {
      if (fx.kind !== "EnergyBlade" || !fx.enabled || fx.params.length < 6) return [];
      const v = (k: number) => fx.params[k].value;
      const at = (x: number, y: number): Pt => [(x / 100) * size[0], (y / 100) * size[1]];
      const c = Math.round(v(5)).toString(16).padStart(6, "0");
      return [{ index, hilt: at(v(0), v(1)), tip: at(v(2), v(3)), colour: `#${c}` }];
    });
  });
  /// Drag a blade's hilt, tip, or (`"both"`) the whole blade.
  function dragBlade(e: PointerEvent, b: Blade, part: "hilt" | "tip" | "both") {
    if (e.button !== 0 || !ins || !inv || app.live?.playing || busyTracking) return;
    e.stopPropagation();
    e.preventDefault();
    const id = ins.id;
    const el = e.currentTarget as SVGElement;
    el.setPointerCapture(e.pointerId);
    const start = toLayer(e);
    const pct = (p: Pt): Pt => [(p[0] / size[0]) * 100, (p[1] / size[1]) * 100];
    const pump = livePump();
    const move = (ev: PointerEvent) => {
      const now = toLayer(ev);
      const d: Pt = [now[0] - start[0], now[1] - start[1]];
      const shift = (p: Pt): Pt => pct([p[0] + d[0], p[1] + d[1]]);
      const sets: [number, number][] = [];
      if (part !== "tip") {
        const h = shift(b.hilt);
        sets.push([0, h[0]], [1, h[1]]);
      }
      if (part !== "hilt") {
        let t = shift(b.tip);
        if (part === "tip" && ev.shiftKey) {
          // Shift: keep the blade's length, only turn it.
          const L = Math.hypot(b.tip[0] - b.hilt[0], b.tip[1] - b.hilt[1]);
          const [px, py] = [b.tip[0] + d[0] - b.hilt[0], b.tip[1] + d[1] - b.hilt[1]];
          const k = L / (Math.hypot(px, py) || 1);
          t = pct([b.hilt[0] + px * k, b.hilt[1] + py * k]);
        }
        sets.push([2, t[0]], [3, t[1]]);
      }
      pump.push(() => Promise.all(sets.map(([k, v]) => api.effectParam(id, b.index, k, v))));
    };
    const up = async () => {
      el.removeEventListener("pointermove", move);
      el.removeEventListener("pointerup", up);
      await pump.done();
      await run(api.commit());
    };
    el.addEventListener("pointermove", move);
    el.addEventListener("pointerup", up);
  }

  // Freehand: each click adds a point; clicking the first one (or Enter, or a double-click) closes it.
  let hover = $state<Pt | null>(null);
  function drawClick(e: MouseEvent) {
    if (!drawing || !inv) return;
    e.stopPropagation();
    const p = toLayer(e);
    const list = drawing.points;
    if (list.length >= 3) {
      const [fx, fy] = toBox(list[0]);
      const r = box!.getBoundingClientRect();
      if (Math.hypot(e.clientX - r.left - fx, e.clientY - r.top - fy) <= 9) {
        finishDrawing();
        return;
      }
    }
    app.drawing = { clip: drawing.clip, points: [...list, p] };
    say(`${list.length + 1} points: click the first point or press Enter to close the mask (Esc cancels)`);
  }

</script>

{#if ins && map && inv}
  <svg class="overlay" class:drawing={!!drawing} aria-hidden="true">
    <!-- Masks: outlines, with handles on their points. -->
    {#each ins.masks as m, i}
      {#if m.outline.length > 1}
        <polygon
          points={pts(m.outline)}
          class="mask"
          class:on={i === active}
          class:off={!m.mask.enabled}
          stroke={MASK}
          onpointerdown={(e) => (i === active ? dragMask(e, i, null) : null)}
          role="presentation"
        />
      {/if}
    {/each}
    {#each ins.masks as m, i}
      {#if m.mask.enabled}
        {#each m.handles as h, j}
          {@const [x, y] = toBox(h)}
          <rect
            x={x - (i === active ? 4.5 : 3.5)}
            y={y - (i === active ? 4.5 : 3.5)}
            width={i === active ? 9 : 7}
            height={i === active ? 9 : 7}
            class="handle"
            fill={i === active ? MASK : "rgba(0,0,0,0.65)"}
            stroke={MASK}
            onpointerdown={(e) => dragMask(e, i, j)}
            role="presentation"
          />
        {/each}
      {/if}
    {/each}

    <!-- Energy blades: a line from hilt to tip, with handles at both ends. -->
    {#if !drawing}
      {#each blades as b}
        {@const [hx, hy] = toBox(b.hilt)}
        {@const [tx, ty] = toBox(b.tip)}
        <line x1={hx} y1={hy} x2={tx} y2={ty} class="bladeline" onpointerdown={(e) => dragBlade(e, b, "both")} role="presentation" />
        <line x1={hx} y1={hy} x2={tx} y2={ty} class="bladeaxis" />
        <circle cx={tx} cy={ty} r="6" class="bladeend" stroke={b.colour} onpointerdown={(e) => dragBlade(e, b, "tip")} role="presentation"><title>Blade tip: drag to aim (Shift keeps the length)</title></circle>
        <circle cx={hx} cy={hy} r="8" class="bladeend hilt" stroke={b.colour} onpointerdown={(e) => dragBlade(e, b, "hilt")} role="presentation"><title>Blade hilt: drag to move this end</title></circle>
        <text x={hx} y={hy + 4} text-anchor="middle" class="bladetag">H</text>
      {/each}
    {/if}

    <!-- The motion tracker: search area, feature box and path for each point. -->
    {#if ins.tracker}
      {@const t = ins.tracker}
      {#if t.mode === "Mesh"}
        <!-- A mesh tracker: the grid, big handles at its corners, small ones inside. -->
        {@const [gc, gr] = t.grid}
        {@const row = (j: number) => t.shown.slice(j * (gc + 1), (j + 1) * (gc + 1))}
        {@const col = (i: number) => Array.from({ length: gr + 1 }, (_, j) => t.shown[j * (gc + 1) + i])}
        <polygon points={pts([...row(0), ...col(gc).slice(1), ...row(gr).reverse().slice(1), ...col(0).reverse().slice(1, -1)])} class="plane" />
        {#each Array.from({ length: gr + 1 }, (_, j) => j) as j}<polyline points={pts(row(j))} class="meshline" />{/each}
        {#each Array.from({ length: gc + 1 }, (_, i) => i) as i}<polyline points={pts(col(i))} class="meshline" />{/each}
        {#each t.shown as c, i}
          {@const corner = i === 0 || i === gc || i === (gr + 1) * (gc + 1) - 1 || i === gr * (gc + 1)}
          {@const [cx, cy] = toBox(c)}
          <circle cx={cx} cy={cy} r={corner ? 6 : 3.5} class="corner" class:inner={!corner} stroke={corner ? POINT_COLOURS[[0, gc, (gr + 1) * (gc + 1) - 1, gr * (gc + 1)].indexOf(i)] : "white"} onpointerdown={(e) => dragTracker(e, i)} role="presentation" />
        {/each}
      {:else if t.mode === "Planar"}
        <!-- A planar tracker: the surface's outline, with a handle at each corner. -->
        <polygon points={pts(t.shown)} class="plane" />
      {:else if t.shown.length > 1}
        <polyline points={pts(t.shown.length > 2 ? [...t.shown, t.shown[0]] : t.shown)} class="joint" />
      {/if}
      {#each t.mode === "Mesh" ? [] : t.shown as c, i}
        {#if t.mode === "Planar"}
          {@const col = POINT_COLOURS[i % 4]}
          {#if t.paths[i]?.length > 1}
            <polyline points={pts(t.paths[i])} class="path" stroke={col} />
          {/if}
          {@const [cx, cy] = toBox(c)}
          <circle cx={cx} cy={cy} r="6" class="corner" stroke={col} onpointerdown={(e) => dragTracker(e, i)} role="presentation" />
          <text x={cx + 8} y={cy - 7} fill={col}>{i + 1}</text>
        {:else}
          {@const col = POINT_COLOURS[i % 4]}
          {#if t.paths[i]?.length > 1}
            <polyline points={pts(t.paths[i])} class="path" stroke={col} />
          {/if}
          <polygon points={pts(square(c, t.size / 2 + t.search))} class="search" stroke={col} />
          <polygon
            points={pts(square(c, t.size / 2))}
            class="feature"
            stroke={col}
            onpointerdown={(e) => dragTracker(e, i)}
            role="presentation"
          />
          {@const [cx, cy] = toBox(c)}
          <circle cx={cx} cy={cy} r="2.5" fill={col} />
          {#if t.shown.length > 1}
            {@const [lx, ly] = toBox(square(c, t.size / 2)[0])}
            <text x={lx + 2} y={ly - 3} fill={col}>{i + 1}</text>
          {/if}
        {/if}
      {/each}
    {/if}

    <!-- Move / Resize mask tools: dragging anywhere picks the mask under the pointer. -->
    {#if tool}
      <rect x="0" y="0" width="100%" height="100%" class="catch tool-{tool}" onpointerdown={toolDown} role="presentation" />
    {/if}

    <!-- Freehand drawing. -->
    {#if drawing}
      <rect
        x="0"
        y="0"
        width="100%"
        height="100%"
        class="catch"
        onclick={drawClick}
        ondblclick={(e) => (e.stopPropagation(), finishDrawing())}
        onpointermove={(e) => (hover = toLayer(e))}
        onpointerdown={(e) => e.stopPropagation()}
        role="presentation"
      />
      {#if drawing.points.length > 1}
        <polyline points={pts(drawing.points)} class="draw" stroke={MASK} />
      {/if}
      {#if drawing.points.length && hover}
        <polyline points={pts([drawing.points[drawing.points.length - 1], hover])} class="draw ghost" stroke={MASK} />
      {/if}
      {#each drawing.points as p, k}
        {@const [x, y] = toBox(p)}
        <circle cx={x} cy={y} r={k === 0 && drawing.points.length >= 3 ? 5.5 : 3} fill={MASK} />
      {/each}
    {/if}
  </svg>
{/if}

<style>
  .overlay {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    pointer-events: none;
    overflow: visible;
    z-index: 2;
  }
  .overlay.drawing {
    cursor: crosshair;
  }
  .mask {
    fill: transparent;
    stroke-width: 1;
    pointer-events: none;
  }
  .mask.on {
    stroke-width: 1.8;
    fill: rgba(255, 110, 200, 0.06);
    pointer-events: fill;
    cursor: move;
  }
  .mask.off {
    opacity: 0.35;
  }
  .handle {
    stroke-width: 1;
    pointer-events: all;
    cursor: crosshair;
  }
  .plane {
    fill: rgba(242, 169, 59, 0.08);
    stroke: rgba(255, 255, 255, 0.8);
    stroke-width: 1.4;
    stroke-dasharray: 6 4;
    pointer-events: none;
  }
  .meshline {
    fill: none;
    stroke: rgba(255, 255, 255, 0.75);
    stroke-width: 1;
    pointer-events: none;
  }
  .corner.inner {
    stroke-width: 1.5;
    fill: rgba(0, 0, 0, 0.3);
  }
  .corner {
    fill: rgba(0, 0, 0, 0.35);
    stroke-width: 2;
    cursor: move;
    pointer-events: all;
  }
  .joint {
    fill: none;
    stroke: rgba(255, 255, 255, 0.6);
    stroke-width: 1.2;
    stroke-dasharray: 6 4;
  }
  .path {
    fill: none;
    stroke-width: 1;
    opacity: 0.6;
  }
  .search {
    fill: none;
    stroke-width: 1;
    opacity: 0.4;
  }
  .feature {
    fill: rgba(0, 0, 0, 0.01);
    stroke-width: 1.5;
    pointer-events: all;
    cursor: crosshair;
  }
  text {
    font-size: calc(12px * var(--text-scale));
    font-weight: 600;
  }
  .catch {
    fill: transparent;
    pointer-events: all;
  }
  .tool-move {
    cursor: move;
  }
  .tool-resize {
    cursor: nwse-resize;
  }
  .draw {
    fill: none;
    stroke-width: 1.5;
  }
  .bladeline {
    stroke: transparent;
    stroke-width: 14;
    pointer-events: stroke;
    cursor: move;
  }
  .bladeaxis {
    stroke: rgba(255, 255, 255, 0.75);
    stroke-width: 1;
    stroke-dasharray: 5 4;
    pointer-events: none;
  }
  .bladeend {
    fill: rgba(0, 0, 0, 0.45);
    stroke-width: 2;
    pointer-events: all;
    cursor: crosshair;
  }
  .bladetag {
    fill: white;
    font-size: calc(10px * var(--text-scale));
    pointer-events: none;
  }
  .draw.ghost {
    opacity: 0.6;
  }
</style>
