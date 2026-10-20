<script lang="ts">
  import { app, run, viewer, timecode } from "../lib/session.svelte";
  import { api, type Pt } from "../lib/api";
  import { livePump } from "../lib/live";
  import { actions } from "../lib/actions.svelte";
  import ViewerOverlay from "./ViewerOverlay.svelte";
  import ViewerTools from "./ViewerTools.svelte";

  let box: HTMLDivElement | undefined = $state();
  let canvas: HTMLCanvasElement | undefined = $state();
  let boxSize = $state({ w: 640, h: 360 });

  $effect(() => {
    if (!box) return;
    const ro = new ResizeObserver(() => {
      const r = box!.getBoundingClientRect();
      boxSize = { w: r.width, h: r.height };
    });
    ro.observe(box);
    return () => ro.disconnect();
  });

  // Zoom (1 = the whole frame fitted in the Viewer) and pan (screen pixels the
  // frame is moved from the middle). The wheel zooms about the pointer; the
  // middle button, or the Hand tool, drags the picture around.
  let zoom = $state(1);
  let pan = $state({ x: 0, y: 0 });
  let hand = $state(false);
  let panning = $state(false);

  // The picture is fetched at the size it's shown (up to the sequence's own), so a
  // zoomed-in view stays sharp.
  $effect(() => {
    const dpr = window.devicePixelRatio || 1;
    const w = Math.max(64, Math.round(Math.min(fit.w, fit.W * 1.0001) * dpr));
    const h = Math.max(36, Math.round(Math.min(fit.h, fit.H * 1.0001) * dpr));
    if (w !== Math.round(viewer.width) || h !== Math.round(viewer.height)) {
      viewer.width = w;
      viewer.height = h;
      viewer.have = -1;
    }
  });

  $effect(() => {
    viewer.canvas = canvas ?? null;
    viewer.have = -1;
  });

  // Where the sequence frame sits inside the box: fitted and centred, then zoomed and panned.
  const fit = $derived.by(() => {
    const W = app.s?.seq.width ?? 1920;
    const H = app.s?.seq.height ?? 1080;
    const k0 = Math.min(boxSize.w / W, boxSize.h / H);
    const k = k0 * zoom;
    return { k, k0, x: (boxSize.w - W * k) / 2 + pan.x, y: (boxSize.h - H * k) / 2 + pan.y, w: W * k, h: H * k, W, H };
  });

  /// Keep part of the picture in view however far it's dragged.
  function clampPan(p: { x: number; y: number }, k: number) {
    const W = fit.W * k;
    const H = fit.H * k;
    const mx = (boxSize.w + W) / 2 - 60;
    const my = (boxSize.h + H) / 2 - 60;
    return { x: Math.max(-mx, Math.min(mx, p.x)), y: Math.max(-my, Math.min(my, p.y)) };
  }

  /// Zoom to `k` (frame pixels → screen pixels), keeping the frame point under (sx, sy) where it is.
  function zoomTo(k: number, sx = boxSize.w / 2, sy = boxSize.h / 2) {
    const kMin = fit.k0 * 0.1;
    const kMax = 32;
    const nk = Math.max(kMin, Math.min(kMax, k));
    const fx = (sx - fit.x) / fit.k;
    const fy = (sy - fit.y) / fit.k;
    const nx = sx - fx * nk;
    const ny = sy - fy * nk;
    zoom = nk / fit.k0;
    pan = clampPan({ x: nx - (boxSize.w - fit.W * nk) / 2, y: ny - (boxSize.h - fit.H * nk) / 2 }, nk);
  }

  function fitView() {
    zoom = 1;
    pan = { x: 0, y: 0 };
  }

  function wheel(e: WheelEvent) {
    e.preventDefault();
    const r = box!.getBoundingClientRect();
    if (e.shiftKey && !e.ctrlKey) {
      // Shift+wheel: move sideways.
      pan = clampPan({ x: pan.x - (e.deltaY || e.deltaX), y: pan.y }, fit.k);
      return;
    }
    // Lines (a mouse) or pixels (a touchpad): about 12% per notch.
    const d = e.deltaMode === 1 ? e.deltaY * 33 : e.deltaY;
    zoomTo(fit.k * Math.exp(-d * 0.0012), e.clientX - r.left, e.clientY - r.top);
  }

  /// Middle button anywhere, or the left with the Hand tool: drag the picture around.
  function panDown(e: PointerEvent) {
    if (!(e.button === 1 || (e.button === 0 && hand))) return;
    e.preventDefault();
    e.stopPropagation();
    const el = box!;
    el.setPointerCapture(e.pointerId);
    panning = true;
    const start = { ...pan };
    const x0 = e.clientX;
    const y0 = e.clientY;
    const move = (ev: PointerEvent) => (pan = clampPan({ x: start.x + ev.clientX - x0, y: start.y + ev.clientY - y0 }, fit.k));
    const up = () => {
      panning = false;
      el.removeEventListener("pointermove", move);
      el.removeEventListener("pointerup", up);
      el.removeEventListener("pointercancel", up);
    };
    el.addEventListener("pointermove", move);
    el.addEventListener("pointerup", up);
    el.addEventListener("pointercancel", up);
  }

  // Another frame size (a composition opened, the sequence changed): fit again.
  let lastSize = "";
  $effect(() => {
    const key = `${fit.W}x${fit.H}`;
    if (key !== lastSize) {
      lastSize = key;
      fitView();
    }
  });

  const zoomLabel = $derived(zoom === 1 && pan.x === 0 && pan.y === 0 ? `Fit (${Math.round(fit.k * 100)}%)` : `${Math.round(fit.k * 100)}%`);

  // The selected layer's gizmo (from the editor, in frame pixels), unless a
  // mask is being edited or drawn on it.
  const giz = $derived.by(() => {
    const ins = app.s?.inspector;
    const g = ins?.gizmo;
    if (!ins || !g) return null;
    if ((app.activeMask && app.activeMask.clip === ins.id) || (app.drawing && app.drawing.clip === ins.id) || (app.maskTool && ins.masks.length)) return null;
    return g;
  });
  const toBox = (p: Pt): Pt => [fit.x + p[0] * fit.k, fit.y + p[1] * fit.k];
  const ptsAttr = (list: Pt[]) => list.map((p) => toBox(p).join(",")).join(" ");
  const dist = (a: Pt, b: Pt) => Math.hypot(a[0] - b[0], a[1] - b[1]);
  const small = $derived(!!giz && (dist(giz.corners[0], giz.corners[1]) * fit.k < 32 || dist(giz.corners[1], giz.corners[2]) * fit.k < 32));
  const stemTip = $derived.by((): Pt | null => {
    if (!giz) return null;
    const [bx, by] = toBox(giz.stem_base);
    return [bx + giz.stem_dir[0] * 26, by + giz.stem_dir[1] * 26];
  });

  // Ctrl held: edges crop and the anchor can be grabbed.
  let ctrl = $state(false);
  // (Releasing Ctrl itself can still report ctrlKey on some systems.)
  const keys = (e: KeyboardEvent) => (ctrl = e.type === "keyup" && (e.key === "Control" || e.key === "Meta") ? false : e.ctrlKey || e.metaKey);

  /// Client pixels → frame pixels.
  function framePt(e: PointerEvent): Pt {
    const r = box!.getBoundingClientRect();
    return [(e.clientX - r.left - fit.x) / fit.k, (e.clientY - r.top - fit.y) / fit.k];
  }

  // Hovering: the handle under the pointer sets the cursor and tooltip.
  let cursor = $state("default");
  let tip = $state("");
  let hoverBusy = false;
  async function hover(e: PointerEvent) {
    if (e.buttons || hoverBusy || !giz) {
      if (!giz) (cursor = "default"), (tip = "");
      return;
    }
    hoverBusy = true;
    try {
      const h = await api.viewerHover(framePt(e), e.ctrlKey || e.metaKey, fit.k);
      if (!h) {
        cursor = "default";
        tip = "";
      } else {
        tip = h.tip;
        if (h.handle.kind === "rotate") cursor = "grab";
        else if (h.handle.kind === "anchor") cursor = "crosshair";
        else {
          const a = (((h.dir ?? 0) % 180) + 180) % 180;
          cursor = ["ew-resize", "nwse-resize", "ns-resize", "nesw-resize"][Math.floor((a + 22.5) / 45) % 4];
        }
      }
    } finally {
      hoverBusy = false;
    }
  }

  // Dragging: a handle of the selected layer, or the layer itself.
  let readout = $state<{ text: string; x: number; y: number } | null>(null);
  let guides = $state<{ xs: number[]; ys: number[] }>({ xs: [], ys: [] });
  const dragPump = livePump();
  function press(e: PointerEvent) {
    if (e.button !== 0) return;
    if (app.activeMask) {
      app.activeMask = null;
      return;
    }
    const el = e.currentTarget as HTMLElement;
    el.setPointerCapture(e.pointerId);
    const r = box!.getBoundingClientRect();
    // Listen straight away: the button can come up before the editor answers.
    let released = false;
    const pressed = api
      .viewerPress(framePt(e), e.ctrlKey || e.metaKey, fit.k)
      .then((what) => {
        if (what === "move") cursor = "move";
        else if (what === "handle" && cursor === "grab") cursor = "grabbing";
        return what;
      })
      .catch(() => "none" as const)
      .finally(() => run(Promise.resolve()));
    const move = (ev: PointerEvent) => {
      if (released) return;
      const p = framePt(ev);
      const [sx, sy] = [ev.clientX - r.left, ev.clientY - r.top];
      const [shift, alt] = [ev.shiftKey, ev.altKey];
      dragPump.push(async () => {
        if ((await pressed) === "none" || released) return;
        const f = await api.viewerDrag(p, shift, alt, fit.k);
        if (released) return;
        readout = { text: f.readout, x: sx, y: sy };
        guides = { xs: f.guides_x, ys: f.guides_y };
      });
    };
    const up = async () => {
      released = true;
      el.removeEventListener("pointermove", move);
      el.removeEventListener("pointerup", up);
      el.removeEventListener("pointercancel", up);
      await pressed;
      await dragPump.done();
      readout = null;
      guides = { xs: [], ys: [] };
      cursor = "default";
      await run(api.viewerRelease());
    };
    el.addEventListener("pointermove", move);
    el.addEventListener("pointerup", up);
    el.addEventListener("pointercancel", up);
  }

  const playing = $derived(app.live?.playing ?? false);
  const proxies = $derived((app.s?.media ?? []).filter((m) => m.proxy === "ready").length);
</script>

<svelte:window onkeydown={keys} onkeyup={keys} onblur={() => (ctrl = false)} />

<div class="viewer">
  <div class="head">
    <span class="name">{app.s?.seq.name ?? ""}</span>
    <span class="muted">{app.s ? `${app.s.seq.width}×${app.s.seq.height} · ${(app.s.seq.rate_num / app.s.seq.rate_den).toFixed(3).replace(".000", "")} fps` : ""}</span>
    <span class="grow"></span>
    {#if app.live?.from_cache}<span class="rendered" title="Shown from Render preview">Rendered</span>{/if}
    {#if proxies > 0}
      <button
        class="chip"
        class:on={app.s?.prefs.use_proxies}
        onclick={() => run(api.proxiesUse(!app.s?.prefs.use_proxies))}
        title="Play the small proxy copies (smooth) or the original files (full quality). Export always uses the originals.">{app.s?.prefs.use_proxies ? "Proxies on" : "Proxies off"}</button
      >
    {/if}
    {#if app.live && !app.live.exact && !playing}<span class="muted" title="Some layers are still decoding">Catching up…</span>{/if}
    <div class="zoom" role="group" aria-label="Viewer zoom">
      <button class="chip" class:on={hand} onclick={() => (hand = !hand)} title="Hand: drag the picture around (or drag with the middle mouse button anywhere)" aria-pressed={hand}>
        <svg viewBox="0 0 16 16" width="13" height="13" aria-hidden="true"><path d="M5.5 8V3.2a1 1 0 0 1 2 0V7.5M7.5 7V2.3a1 1 0 0 1 2 0V7M9.5 7V3a1 1 0 0 1 2 0v5.2M11.5 6.5a1 1 0 0 1 2 0V10c0 2.8-2 4.5-4.5 4.5S5 13.5 3.6 11.4L2.4 9.6a1 1 0 0 1 1.6-1.2L5.5 10" fill="none" stroke="currentColor" stroke-width="1.2" stroke-linecap="round" stroke-linejoin="round" /></svg>
      </button>
      <button class="chip" onclick={() => zoomTo(fit.k / 1.25)} title="Zoom out (or the mouse wheel over the picture)" aria-label="Zoom out">−</button>
      <button class="chip zlevel" onclick={fitView} title="Fit the whole frame in the Viewer">{zoomLabel}</button>
      <button class="chip" onclick={() => zoomTo(fit.k * 1.25)} title="Zoom in (or the mouse wheel over the picture)" aria-label="Zoom in">+</button>
      <button class="chip" onclick={() => zoomTo(1)} title="Actual pixels: one frame pixel to one screen pixel">1:1</button>
    </div>
  </div>
  <div
    class="stage"
    bind:this={box}
    role="presentation"
    style="cursor:{panning ? 'grabbing' : hand ? 'grab' : cursor}"
    title={tip}
    onpointerdowncapture={panDown}
    onpointerdown={press}
    onpointermove={hover}
    onwheel={wheel}
    onauxclick={(e) => e.button === 1 && e.preventDefault()}
  >
    <canvas
      bind:this={canvas}
      style="left:{fit.x}px; top:{fit.y}px; width:{fit.w}px; height:{fit.h}px"
      aria-label="Viewer"
    ></canvas>
    {#if (app.s?.seq.duration ?? 0) === 0}
      <div class="hint">Drag media to the timeline to see it here</div>
    {/if}
    {#if giz}
      <svg class="gizmo" aria-hidden="true">
        {#if giz.three_d}
          <!-- A 3D layer: its outline as the camera sees it (drag to move; turn it in Controls). -->
          <polygon points={ptsAttr(giz.corners)} class="box three" />
        {:else if giz.pinned}
          <polygon points={ptsAttr(giz.corners)} class="pin" />
          <line class="diag" x1={toBox(giz.corners[0])[0]} y1={toBox(giz.corners[0])[1]} x2={toBox(giz.corners[2])[0]} y2={toBox(giz.corners[2])[1]} />
          <line class="diag" x1={toBox(giz.corners[1])[0]} y1={toBox(giz.corners[1])[1]} x2={toBox(giz.corners[3])[0]} y2={toBox(giz.corners[3])[1]} />
          {#each giz.corners as c}
            <circle class="pinc" cx={toBox(c)[0]} cy={toBox(c)[1]} r="5" />
          {/each}
        {:else}
          {#if giz.cropped}<polygon points={ptsAttr(giz.full)} class="full" />{/if}
          <polygon points={ptsAttr(giz.corners)} class="box" />
          {#if stemTip}
            <line class="stem" x1={toBox(giz.stem_base)[0]} y1={toBox(giz.stem_base)[1]} x2={stemTip[0]} y2={stemTip[1]} />
            <circle class="rot" cx={stemTip[0]} cy={stemTip[1]} r="4.5" />
          {/if}
          {#each giz.corners as c}
            <rect class="h fill" x={toBox(c)[0] - 3.5} y={toBox(c)[1] - 3.5} width="7" height="7" rx="1" />
          {/each}
          {#if !small}
            {#each giz.edges as c}
              {#if ctrl}
                <circle class="crop" cx={toBox(c)[0]} cy={toBox(c)[1]} r="3.5" />
              {:else}
                <rect class="h" x={toBox(c)[0] - 3.5} y={toBox(c)[1] - 3.5} width="7" height="7" rx="1" />
              {/if}
            {/each}
          {/if}
          {@const [ax, ay] = toBox(giz.anchor)}
          {#if ctrl}<circle class="anchorhot" cx={ax} cy={ay} r="7" />{/if}
          <circle class="anchor" cx={ax} cy={ay} r="5" />
          <line class="anchor" x1={ax - 9} y1={ay} x2={ax + 9} y2={ay} />
          <line class="anchor" x1={ax} y1={ay - 9} x2={ax} y2={ay + 9} />
        {/if}
      </svg>
    {/if}
    {#each guides.xs as gx}<div class="guide v" style="left:{fit.x + gx * fit.k}px; top:{fit.y}px; height:{fit.h}px"></div>{/each}
    {#each guides.ys as gy}<div class="guide h" style="top:{fit.y + gy * fit.k}px; left:{fit.x}px; width:{fit.w}px"></div>{/each}
    {#if readout?.text}<div class="readout" style="left:{readout.x + 16}px; top:{readout.y + 16}px">{readout.text}</div>{/if}
    <ViewerOverlay {fit} {box} />
    <ViewerTools />
  </div>
  <div class="transport">
    <button class="ghost" onclick={() => actions.act("start")} aria-label="Go to start" title="Go to start (Home)">
      <svg viewBox="0 0 16 16" width="14" height="14"><path d="M3 3h2v10H3zM13 3v10L6 8z" fill="currentColor" /></svg>
    </button>
    <button class="ghost" onclick={() => actions.act("prev_frame")} aria-label="Previous frame" title="Previous frame (←)">
      <svg viewBox="0 0 16 16" width="14" height="14"><path d="M11 3v10L4 8z" fill="currentColor" /></svg>
    </button>
    <button class="play" class:playing onclick={() => actions.act("play")} aria-label={playing ? "Pause" : "Play"} title="Play / pause (Space)">
      {#if playing}
        <svg viewBox="0 0 16 16" width="16" height="16"><path d="M4 3h3v10H4zM9 3h3v10H9z" fill="currentColor" /></svg>
      {:else}
        <svg viewBox="0 0 16 16" width="16" height="16"><path d="M4 2.5v11l9.5-5.5z" fill="currentColor" /></svg>
      {/if}
    </button>
    <button class="ghost" onclick={() => actions.act("next_frame")} aria-label="Next frame" title="Next frame (→)">
      <svg viewBox="0 0 16 16" width="14" height="14"><path d="M5 3v10l7-5z" fill="currentColor" /></svg>
    </button>
    <button class="ghost" onclick={() => actions.act("end")} aria-label="Go to end" title="Go to end (End)">
      <svg viewBox="0 0 16 16" width="14" height="14"><path d="M11 3h2v10h-2zM3 3v10l7-5z" fill="currentColor" /></svg>
    </button>
    <div class="deck" title="Playhead / sequence length">
      <span class="lamp" class:lit={playing} aria-hidden="true"></span>
      <span class="tc now">{timecode(app.live?.playhead ?? 0)}</span>
      <span class="tc total">/ {timecode(app.s?.seq.duration ?? 0)}</span>
    </div>
    {#if app.live && app.live.playing && app.live.shuttle !== 1}
      <span class="shuttle" title="J / L change the speed, K stops (no sound while shuttling)">{app.live.shuttle < 0 ? `◀◀ ${Math.abs(app.live.shuttle)}×` : `▶▶ ${app.live.shuttle}×`}</span>
    {/if}
    <span class="grow"></span>
    <span class="muted clock" title="What drives playback">{app.live?.clock ?? ""}</span>
  </div>
</div>

<style>
  .shuttle {
    margin-left: 10px;
    color: var(--accent);
    font-weight: 600;
    font-family: var(--font-tc);
  }
  .zoom {
    display: flex;
    gap: 3px;
    align-items: center;
  }
  .zoom .chip {
    min-width: 24px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
  }
  .zoom .zlevel {
    min-width: 76px;
    font-family: var(--font-tc);
  }
  .chip.on {
    border-color: var(--accent);
    color: var(--accent);
  }
  .chip {
    min-height: 20px;
    padding: 0 8px;
    font-size: calc(11.5px * var(--text-scale));
    border-radius: 10px;
  }
  .viewer {
    height: 100%;
    display: flex;
    flex-direction: column;
    min-height: 0;
  }
  .head {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 6px 10px;
    min-height: 32px;
  }
  .name {
    font-weight: 600;
  }
  .grow {
    flex: 1;
  }
  .rendered {
    color: var(--green);
    font-size: calc(12px * var(--text-scale));
    font-weight: 600;
  }
  .stage {
    flex: 1;
    position: relative;
    background: var(--well);
    overflow: hidden;
    min-height: 120px;
  }
  canvas {
    position: absolute;
    background: #000;
    image-rendering: auto;
  }
  .hint {
    position: absolute;
    inset: 0;
    display: grid;
    place-items: center;
    color: var(--faint);
    pointer-events: none;
  }
  .gizmo {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    pointer-events: none;
    overflow: visible;
  }
  .gizmo .box {
    fill: none;
    stroke: rgb(255, 196, 64);
    stroke-width: 1.5;
  }
  .gizmo .full {
    fill: none;
    stroke: rgba(255, 196, 64, 0.45);
    stroke-dasharray: 4 4;
  }
  .gizmo .h {
    fill: rgb(20, 20, 20);
    stroke: rgb(255, 196, 64);
    stroke-width: 1.5;
  }
  .gizmo .h.fill {
    fill: rgb(255, 196, 64);
  }
  .gizmo .crop {
    fill: rgb(120, 200, 255);
  }
  .gizmo .stem,
  .gizmo line.anchor {
    stroke: rgb(255, 196, 64);
    stroke-width: 1.2;
  }
  .gizmo .rot,
  .gizmo circle.anchor {
    fill: rgba(20, 20, 20, 0.6);
    stroke: rgb(255, 196, 64);
    stroke-width: 1.5;
  }
  .gizmo circle.anchor {
    fill: none;
  }
  .gizmo .anchorhot {
    fill: rgba(120, 200, 255, 0.5);
  }
  .gizmo .box.three {
    stroke-dasharray: 6 4;
  }
  .gizmo .pin {
    fill: none;
    stroke: rgb(255, 120, 200);
    stroke-width: 1.5;
  }
  .gizmo .diag {
    stroke: rgba(255, 120, 200, 0.35);
  }
  .gizmo .pinc {
    fill: rgb(20, 20, 20);
    stroke: rgb(255, 120, 200);
    stroke-width: 2;
  }
  .guide {
    position: absolute;
    background: rgb(255, 70, 70);
    pointer-events: none;
  }
  .guide.v {
    width: 1.5px;
  }
  .guide.h {
    height: 1.5px;
  }
  .readout {
    position: absolute;
    pointer-events: none;
    background: rgba(0, 0, 0, 0.8);
    color: #fff;
    padding: 3px 6px;
    border-radius: 3px;
    font-size: calc(12px * var(--text-scale));
    white-space: nowrap;
    z-index: 5;
  }
  .transport {
    display: flex;
    align-items: center;
    gap: 2px;
    padding: 6px 10px;
    border-top: 1px solid var(--line-soft);
  }
  .transport button {
    display: grid;
    place-items: center;
    padding: 4px 8px;
  }
  .play {
    width: 40px;
    height: 30px;
    margin: 0 4px;
    border-radius: 15px;
  }
  .play.playing {
    background: var(--accent);
    border-color: var(--accent);
    color: var(--accent-ink);
  }
  .deck {
    display: flex;
    align-items: baseline;
    gap: 8px;
    margin-left: 14px;
    padding: 2px 12px 2px 10px;
    background: var(--well);
    border: 1px solid var(--line-soft);
    border-radius: 6px;
  }
  .lamp {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: color-mix(in srgb, var(--accent) 18%, var(--well));
    align-self: center;
    transition: background 0.15s, box-shadow 0.15s;
  }
  .lamp.lit {
    background: var(--accent);
    box-shadow: 0 0 8px var(--accent);
  }
  .now {
    font-size: calc(21px * var(--text-scale));
    font-weight: 500;
    letter-spacing: 0.02em;
    color: var(--text);
  }
  .total {
    color: var(--muted);
    font-size: calc(12px * var(--text-scale));
  }
  .clock {
    font-size: calc(12px * var(--text-scale));
  }
</style>
