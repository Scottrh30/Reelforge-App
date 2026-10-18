<script lang="ts">
  import { app, run, viewer, timecode } from "../lib/session.svelte";
  import { api, type Transform } from "../lib/api";
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
      const dpr = window.devicePixelRatio || 1;
      viewer.width = Math.max(64, r.width * dpr);
      viewer.height = Math.max(36, r.height * dpr);
      viewer.have = -1;
    });
    ro.observe(box);
    return () => ro.disconnect();
  });

  $effect(() => {
    viewer.canvas = canvas ?? null;
    viewer.have = -1;
  });

  // Where the sequence frame sits inside the box (fitted, centred).
  const fit = $derived.by(() => {
    const W = app.s?.seq.width ?? 1920;
    const H = app.s?.seq.height ?? 1080;
    const k = Math.min(boxSize.w / W, boxSize.h / H);
    return { k, x: (boxSize.w - W * k) / 2, y: (boxSize.h - H * k) / 2, w: W * k, h: H * k, W, H };
  });

  // The selected layer's outline, in box pixels (anchor and corner pin ignored for the outline).
  const sel = $derived.by(() => {
    const ins = app.s?.inspector;
    const v = ins?.visual;
    const shownT = ins?.keys?.shown ?? v?.transform;
    if (!v || !shownT || shownT.pin) return null;
    // Keyframed and the playhead off the clip: nothing to move.
    if (ins!.animated.length && !ins!.keys?.on_clip) return null;
    // Editing a mask or drawing one: the layer stays put.
    if ((app.activeMask && app.activeMask.clip === ins!.id) || (app.drawing && app.drawing.clip === ins!.id) || (app.maskTool && ins!.masks.length)) return null;
    const t = shownT;
    const [lw, lh] = v.layer_size;
    const cx = fit.x + (fit.W / 2 + t.position[0] - t.anchor[0] * t.scale[0]) * fit.k;
    const cy = fit.y + (fit.H / 2 + t.position[1] - t.anchor[1] * t.scale[1]) * fit.k;
    return { id: ins!.id, cx, cy, w: Math.abs(lw * t.scale[0] * fit.k), h: Math.abs(lh * t.scale[1] * fit.k), rot: t.rotation, t };
  });

  function drag(e: PointerEvent, what: "move" | "scale") {
    if (!sel || e.button !== 0) return;
    e.stopPropagation();
    const el = e.currentTarget as HTMLElement;
    el.setPointerCapture(e.pointerId);
    const start = { x: e.clientX, y: e.clientY };
    const t0: Transform = structuredClone($state.snapshot(sel.t)) as Transform;
    const { cx, cy, id } = sel;
    const r0 = Math.hypot(start.x - (box!.getBoundingClientRect().left + cx), start.y - (box!.getBoundingClientRect().top + cy)) || 1;
    const move = (ev: PointerEvent) => {
      const t = structuredClone(t0);
      if (what === "move") {
        let dx = (ev.clientX - start.x) / fit.k;
        let dy = (ev.clientY - start.y) / fit.k;
        if (ev.shiftKey) Math.abs(dx) > Math.abs(dy) ? (dy = 0) : (dx = 0);
        t.position = [Math.round(t0.position[0] + dx), Math.round(t0.position[1] + dy)];
      } else {
        const b = box!.getBoundingClientRect();
        const r = Math.hypot(ev.clientX - (b.left + cx), ev.clientY - (b.top + cy));
        const f = r / r0;
        t.scale = [t0.scale[0] * f, t0.scale[1] * f];
      }
      api.setTransform(id, t);
    };
    const up = () => {
      el.removeEventListener("pointermove", move);
      el.removeEventListener("pointerup", up);
      run(api.commit());
    };
    el.addEventListener("pointermove", move);
    el.addEventListener("pointerup", up);
  }

  const playing = $derived(app.live?.playing ?? false);
  const proxies = $derived((app.s?.media ?? []).filter((m) => m.proxy === "ready").length);
</script>

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
  </div>
  <div
    class="stage"
    bind:this={box}
    role="presentation"
    onpointerdown={() => {
      if (app.activeMask) app.activeMask = null;
      else if (sel) run(api.select(null, "none"));
    }}
  >
    <canvas
      bind:this={canvas}
      style="left:{fit.x}px; top:{fit.y}px; width:{fit.w}px; height:{fit.h}px"
      aria-label="Viewer"
    ></canvas>
    {#if (app.s?.seq.duration ?? 0) === 0}
      <div class="hint">Drag media to the timeline to see it here</div>
    {/if}
    {#if sel}
      <div
        class="gizmo"
        role="presentation"
        style="left:{sel.cx}px; top:{sel.cy}px; width:{sel.w}px; height:{sel.h}px; transform: translate(-50%, -50%) rotate({sel.rot}deg)"
        onpointerdown={(e) => drag(e, "move")}
      >
        {#each ["nw", "ne", "sw", "se"] as c}
          <span class="handle {c}" role="presentation" onpointerdown={(e) => drag(e, "scale")}></span>
        {/each}
      </div>
    {/if}
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
    border: 1.5px solid var(--accent);
    cursor: move;
    box-shadow: 0 0 0 1px rgba(0, 0, 0, 0.35);
  }
  .handle {
    position: absolute;
    width: 10px;
    height: 10px;
    background: #fff;
    border: 1.5px solid var(--accent);
    border-radius: 2px;
  }
  .handle.nw {
    left: -6px;
    top: -6px;
    cursor: nwse-resize;
  }
  .handle.ne {
    right: -6px;
    top: -6px;
    cursor: nesw-resize;
  }
  .handle.sw {
    left: -6px;
    bottom: -6px;
    cursor: nesw-resize;
  }
  .handle.se {
    right: -6px;
    bottom: -6px;
    cursor: nwse-resize;
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
