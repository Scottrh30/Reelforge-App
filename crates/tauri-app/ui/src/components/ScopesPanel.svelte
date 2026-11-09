<script lang="ts">
  // The Scopes tab: the Viewer's picture measured — a waveform (luma, or
  // red, green and blue), an RGB parade, a vectorscope and a histogram, any
  // of them at once. Choices are remembered.
  import ScopeView from "./ScopeView.svelte";
  import type { ScopeKind } from "../lib/api";

  const KINDS: { kind: ScopeKind; label: string; tip: string }[] = [
    { kind: "Waveform", label: "Waveform", tip: "Brightness across the picture (left to right as in the Viewer) against level: 0 % is black, 100 % white" },
    { kind: "Parade", label: "Parade", tip: "A waveform each for red, green and blue: matched heights in something white or grey mean it's neutral" },
    { kind: "Vectorscope", label: "Vectorscope", tip: "Hue round the circle and saturation out from the middle; the boxes are 75 % colour bars, the dashed line is skin tone" },
    { kind: "Histogram", label: "Histogram", tip: "How many pixels there are at each level, from black (left) to white (right)" },
  ];

  const KEY = "reelforge.scopes";
  interface Prefs {
    shown: ScopeKind[];
    rgb: boolean;
    brightness: number;
    zoom: number;
  }
  const prefs = $state<Prefs>(
    (() => {
      const d: Prefs = { shown: ["Waveform", "Vectorscope"], rgb: false, brightness: 1, zoom: 1 };
      try {
        const v = JSON.parse(localStorage.getItem(KEY) ?? "null");
        return v && Array.isArray(v.shown) ? { ...d, ...v } : d;
      } catch {
        return d;
      }
    })(),
  );
  $effect(() => {
    const snap = JSON.stringify(prefs);
    try {
      localStorage.setItem(KEY, snap);
    } catch {
      // Not remembered, then.
    }
  });
  function toggle(k: ScopeKind) {
    prefs.shown = prefs.shown.includes(k) ? prefs.shown.filter((x) => x !== k) : KINDS.map((x) => x.kind).filter((x) => x === k || prefs.shown.includes(x));
  }
</script>

<div class="scopes">
  <div class="bar" role="toolbar" aria-label="Scopes">
    <div class="kinds">
      {#each KINDS as k}
        <button class="chip" class:on={prefs.shown.includes(k.kind)} aria-pressed={prefs.shown.includes(k.kind)} onclick={() => toggle(k.kind)} title={k.tip}>{k.label}</button>
      {/each}
    </div>
    <div class="opts">
      {#if prefs.shown.includes("Waveform") || prefs.shown.includes("Histogram")}
        <label class="check" title="Waveform and histogram: red, green and blue rather than luma"><input type="checkbox" bind:checked={prefs.rgb} /> RGB</label>
      {/if}
      {#if prefs.shown.includes("Vectorscope")}
        <label title="Vectorscope magnification, for pictures with little colour">
          Zoom
          <select bind:value={prefs.zoom}>
            <option value={1}>1×</option>
            <option value={2}>2×</option>
            <option value={4}>4×</option>
          </select>
        </label>
      {/if}
      <label title="How bright the traces are">
        Brightness
        <input type="range" min="0.25" max="4" step="0.05" bind:value={prefs.brightness} ondblclick={() => (prefs.brightness = 1)} />
      </label>
    </div>
  </div>
  {#if prefs.shown.length}
    <div class="list" style="--n:{prefs.shown.length}">
      {#each prefs.shown as k (k)}
        <div class="cell">
          <ScopeView kind={k} rgb={prefs.rgb} brightness={prefs.brightness} zoom={prefs.zoom} />
        </div>
      {/each}
    </div>
  {:else}
    <p class="empty">Pick a scope above to measure the Viewer's picture.</p>
  {/if}
</div>

<style>
  .scopes {
    height: 100%;
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 6px;
    min-height: 0;
  }
  .bar {
    display: flex;
    flex-wrap: wrap;
    gap: 6px 10px;
    align-items: center;
  }
  .kinds {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
  }
  .chip {
    border-radius: 12px;
    min-height: 24px;
    padding: 0 10px;
    background: transparent;
  }
  .chip.on {
    background: var(--accent-soft);
    border-color: var(--accent);
    color: var(--text);
  }
  .opts {
    display: flex;
    flex-wrap: wrap;
    gap: 6px 10px;
    align-items: center;
    font-size: calc(12px * var(--text-scale));
    color: var(--muted);
  }
  .opts label {
    display: flex;
    align-items: center;
    gap: 5px;
  }
  .opts input[type="range"] {
    width: 80px;
  }
  .list {
    flex: 1;
    min-height: 0;
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(240px, 1fr));
    grid-auto-rows: minmax(120px, 1fr);
    gap: 6px;
    overflow: auto;
  }
  .cell {
    min-height: 0;
    min-width: 0;
  }
  .empty {
    color: var(--muted);
    padding: 8px 2px;
  }
</style>
