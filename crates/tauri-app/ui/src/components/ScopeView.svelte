<script lang="ts">
  // One scope of the Viewer's picture, drawn by the editor at this box's
  // size and fetched again whenever the picture changes (a few times a
  // second while playing). Waveforms show their levels and the level under
  // the pointer; the vectorscope names its colour targets.
  import { onDestroy } from "svelte";
  import { app } from "../lib/session.svelte";
  import { api, type ScopeKind } from "../lib/api";

  let { kind, rgb = false, brightness = 1, zoom = 1 }: { kind: ScopeKind; rgb?: boolean; brightness?: number; zoom?: number } = $props();

  let box = $state<HTMLDivElement>();
  let canvas = $state<HTMLCanvasElement>();
  let css = $state({ w: 0, h: 0 });
  const dpr = () => Math.min(2, window.devicePixelRatio || 1);
  // The size asked for (device pixels).
  const px = $derived({ w: Math.max(16, Math.min(1600, Math.round(css.w * dpr()))), h: Math.max(16, Math.min(1600, Math.round(css.h * dpr()))) });

  $effect(() => {
    if (!box) return;
    const ro = new ResizeObserver(([e]) => (css = { w: e.contentRect.width, h: e.contentRect.height }));
    ro.observe(box);
    return () => ro.disconnect();
  });

  // Fetching: one request at a time; a change while one is out asks again after.
  let have = -1;
  let lastKey = "";
  let busy = false;
  let again = false;
  let lastAt = 0;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let gone = false;
  onDestroy(() => {
    gone = true;
    clearTimeout(timer);
  });

  $effect(() => {
    // Again when the picture, the size or the settings change.
    void app.live?.picture_rev;
    const key = `${kind}|${rgb}|${brightness}|${zoom}|${px.w}x${px.h}`;
    if (key !== lastKey) {
      lastKey = key;
      have = -1;
    }
    if (css.w > 8 && css.h > 8) pull();
  });

  async function pull() {
    if (busy) {
      again = true;
      return;
    }
    // While playing, a few times a second is plenty (and keeps playback smooth).
    const wait = app.live?.playing ? 90 - (performance.now() - lastAt) : 0;
    if (wait > 0) {
      clearTimeout(timer);
      timer = setTimeout(pull, wait);
      return;
    }
    busy = true;
    again = false;
    lastAt = performance.now();
    try {
      const { w, h } = px;
      const buf = await api.scope({ scope: kind, width: w, height: h, rgb, brightness, zoom }, have);
      const b = new Uint8Array(buf);
      if (!gone && b.length > 17 && b[0] === 1 && canvas) {
        const dv = new DataView(buf);
        const [iw, ih] = [dv.getUint32(1, true), dv.getUint32(5, true)];
        have = Number(dv.getBigUint64(9, true));
        if (canvas.width !== iw || canvas.height !== ih) [canvas.width, canvas.height] = [iw, ih];
        canvas.getContext("2d")?.putImageData(new ImageData(new Uint8ClampedArray(buf, 17, iw * ih * 4), iw, ih), 0, 0);
      }
    } catch {
      // The next change tries again.
    } finally {
      busy = false;
    }
    if (again && !gone) pull();
  }

  // Where a level (0–1) sits on a waveform, in CSS pixels from the top (as engine scopes::level_row).
  function levelY(v: number): number {
    const H = px.h;
    const pad = Math.min(12, Math.max(2, Math.floor(H / 24)));
    const bottom = H - 1 - pad;
    return (bottom - v * (bottom - pad)) * (css.h / H);
  }
  const wave = $derived(kind === "Waveform" || kind === "Parade");

  // The vectorscope's colour targets (75 %), as engine scopes::vector_point.
  const TARGETS: [string, number[]][] = [
    ["R", [1, 0, 0]],
    ["Mg", [1, 0, 1]],
    ["B", [0, 0, 1]],
    ["Cy", [0, 1, 1]],
    ["G", [0, 1, 0]],
    ["Yl", [1, 1, 0]],
  ];
  const targets = $derived.by(() => {
    const s = Math.min(css.w, css.h);
    const [ox, oy] = [(css.w - s) / 2, (css.h - s) / 2];
    const k = (0.5 * zoom) / 0.6;
    return TARGETS.map(([name, c]) => {
      const [r, g, b] = c.map((v) => v * 0.75);
      const y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
      const [cb, cr] = [(b - y) / 1.8556, (r - y) / 1.5748];
      // Labels just outside their boxes.
      const len = Math.hypot(cb, cr) || 1;
      const [fx, fy] = [0.5 + (cb + (cb / len) * 0.06) * k, 0.5 - (cr + (cr / len) * 0.06) * k];
      return { name, x: ox + fx * s, y: oy + fy * s, out: fx < 0 || fx > 1 || fy < 0 || fy > 1 };
    });
  });

  // The level under the pointer (waveforms).
  let hover = $state<number | null>(null);
  function move(e: PointerEvent) {
    if (!wave || !box) return;
    const r = box.getBoundingClientRect();
    const y = e.clientY - r.top;
    const v = (levelY(0) - y) / (levelY(0) - levelY(1));
    hover = v >= -0.02 && v <= 1.02 ? Math.min(1, Math.max(0, v)) : null;
  }
</script>

<div class="scope" bind:this={box} onpointermove={move} onpointerleave={() => (hover = null)} role="img" aria-label="{kind} of the Viewer's picture">
  <canvas bind:this={canvas}></canvas>
  {#if wave}
    {#each [0, 25, 50, 75, 100] as l}
      <span class="lvl" class:below={l === 100} style="top:{levelY(l / 100)}px">{l}</span>
    {/each}
    {#if hover !== null}
      <div class="hline" style="top:{levelY(hover)}px"></div>
      <span class="read" style="top:{levelY(hover)}px">{(hover * 100).toFixed(1)} % · {Math.round(hover * 255)}</span>
    {/if}
    {#if kind === "Parade"}
      <span class="ch" style="left:2%;color:#eb6a5a">R</span><span class="ch" style="left:35.3%;color:#5ad06a">G</span><span class="ch" style="left:68.6%;color:#6a90ff">B</span>
    {/if}
  {:else if kind === "Vectorscope"}
    {#each targets as t}
      {#if !t.out}<span class="tgt" style="left:{t.x}px;top:{t.y}px">{t.name}</span>{/if}
    {/each}
    {#if zoom !== 1}<span class="zoom">{zoom}×</span>{/if}
  {:else}
    <span class="axis" style="left:4px">0</span><span class="axis" style="left:50%;transform:translateX(-50%)">50</span><span class="axis" style="right:4px">100</span>
  {/if}
</div>

<style>
  .scope {
    position: relative;
    width: 100%;
    height: 100%;
    min-height: 90px;
    overflow: hidden;
    background: #101012;
    border-radius: 4px;
    cursor: crosshair;
  }
  canvas {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    image-rendering: auto;
  }
  span {
    position: absolute;
    font-size: calc(10px * var(--text-scale));
    color: rgba(220, 220, 228, 0.55);
    pointer-events: none;
    font-variant-numeric: tabular-nums;
    line-height: 1;
  }
  .lvl {
    left: 3px;
    transform: translateY(-110%);
  }
  .lvl.below {
    transform: translateY(20%);
  }
  .hline {
    position: absolute;
    left: 0;
    right: 0;
    height: 1px;
    background: rgba(255, 210, 90, 0.75);
    pointer-events: none;
  }
  .read {
    right: 4px;
    transform: translateY(-120%);
    color: #ffd25a;
    background: rgba(0, 0, 0, 0.55);
    padding: 1px 3px;
    border-radius: 2px;
  }
  .ch {
    top: 4px;
    font-weight: 600;
  }
  .tgt {
    transform: translate(-50%, -50%);
    font-weight: 600;
  }
  .zoom {
    right: 4px;
    top: 4px;
  }
  .axis {
    bottom: 3px;
  }
</style>
