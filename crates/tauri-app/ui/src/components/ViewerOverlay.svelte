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

    <!-- The motion tracker: search area, feature box and path for each point. -->
    {#if ins.tracker}
      {@const t = ins.tracker}
      {#if t.shown.length > 1}
        <polyline points={pts(t.shown.length > 2 ? [...t.shown, t.shown[0]] : t.shown)} class="joint" />
      {/if}
      {#each t.shown as c, i}
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
    font-size: 12px;
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
  .draw.ghost {
    opacity: 0.6;
  }
</style>
