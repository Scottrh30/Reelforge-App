<script lang="ts">
  // The colour pop-up: a saturation/brightness square, a hue strip, the hex code, the
  // colour before and after, an eyedropper where the system has one, and OK / Cancel.
  // Enter is OK, Escape is Cancel; clicking outside cancels.
  import { colorPick } from "../lib/colorpick.svelte";

  let el: HTMLDivElement | undefined = $state();
  let h = $state(0);
  let s = $state(0);
  let v = $state(0.5);
  let hexText = $state("#808080");
  let pos = $state({ x: 0, y: 0 });

  const clamp = (n: number, a: number, b: number) => Math.min(b, Math.max(a, n));

  function toRgb(h: number, s: number, v: number): [number, number, number] {
    const f = (n: number) => {
      const k = (n + h / 60) % 6;
      return v - v * s * Math.max(0, Math.min(k, 4 - k, 1));
    };
    return [f(5), f(3), f(1)].map((c) => Math.round(c * 255)) as [number, number, number];
  }
  function toHex([r, g, b]: [number, number, number]) {
    return "#" + [r, g, b].map((c) => c.toString(16).padStart(2, "0")).join("");
  }
  function fromHex(hex: string) {
    const m = /^#?([0-9a-f]{6})$/i.exec(hex.trim());
    if (!m) return false;
    const n = parseInt(m[1], 16);
    const [r, g, b] = [(n >> 16) & 255, (n >> 8) & 255, n & 255].map((c) => c / 255);
    const max = Math.max(r, g, b);
    const d = max - Math.min(r, g, b);
    let hue = h;
    if (d > 0) {
      if (max === r) hue = 60 * (((g - b) / d + 6) % 6);
      else if (max === g) hue = 60 * ((b - r) / d + 2);
      else hue = 60 * ((r - g) / d + 4);
    }
    h = hue;
    s = max === 0 ? 0 : d / max;
    v = max;
    return true;
  }

  const hex = $derived(toHex(toRgb(h, s, v)));
  const pure = $derived(toHex(toRgb(h, 1, 1)));

  // Opened: start at its colour, and keep it on screen.
  let opened: unknown = null;
  $effect(() => {
    const o = colorPick.open;
    if (!o || o === opened) return;
    opened = o;
    fromHex(o.start);
    hexText = o.start;
    pos = { x: o.x, y: o.y };
    queueMicrotask(() => {
      if (!el) return;
      const r = el.getBoundingClientRect();
      pos = { x: clamp(o.x, 4, window.innerWidth - r.width - 4), y: clamp(o.y, 4, window.innerHeight - r.height - 4) };
      el.querySelector<HTMLButtonElement>("button.primary")?.focus();
    });
  });
  $effect(() => {
    hexText = hex;
  });

  function ok() {
    const o = colorPick.open;
    colorPick.open = null;
    opened = null;
    o?.done(hex);
  }
  function cancel() {
    colorPick.open = null;
    opened = null;
  }

  function drag(e: PointerEvent, set: (x: number, y: number) => void) {
    const t = e.currentTarget as HTMLElement;
    const r = t.getBoundingClientRect();
    const at = (ev: PointerEvent) => set(clamp((ev.clientX - r.left) / r.width, 0, 1), clamp((ev.clientY - r.top) / r.height, 0, 1));
    t.setPointerCapture(e.pointerId);
    at(e);
    const move = (ev: PointerEvent) => at(ev);
    const up = () => (t.removeEventListener("pointermove", move), t.removeEventListener("pointerup", up));
    t.addEventListener("pointermove", move);
    t.addEventListener("pointerup", up);
  }

  function keys(e: KeyboardEvent) {
    if (e.key === "Escape") (e.preventDefault(), e.stopPropagation(), cancel());
    else if (e.key === "Enter" && !(e.target instanceof HTMLInputElement && e.target.type === "text" && !fromHex(hexText))) (e.preventDefault(), e.stopPropagation(), ok());
  }
  function squareKeys(e: KeyboardEvent) {
    const step = e.shiftKey ? 0.1 : 0.01;
    const k: Record<string, () => void> = { ArrowLeft: () => (s = clamp(s - step, 0, 1)), ArrowRight: () => (s = clamp(s + step, 0, 1)), ArrowUp: () => (v = clamp(v + step, 0, 1)), ArrowDown: () => (v = clamp(v - step, 0, 1)) };
    if (k[e.key]) (e.preventDefault(), k[e.key]());
  }
  function hueKeys(e: KeyboardEvent) {
    const step = e.shiftKey ? 10 : 1;
    if (e.key === "ArrowLeft" || e.key === "ArrowDown") (e.preventDefault(), (h = (h - step + 360) % 360));
    if (e.key === "ArrowRight" || e.key === "ArrowUp") (e.preventDefault(), (h = (h + step) % 360));
  }

  const dropper = typeof window !== "undefined" && "EyeDropper" in window;
  async function eyedrop() {
    try {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const r = await new (window as any).EyeDropper().open();
      if (r?.sRGBHex) fromHex(r.sRGBHex);
    } catch {
      /* cancelled */
    }
  }
</script>

{#if colorPick.open}
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div class="shade" onpointerdown={cancel}></div>
  <div class="picker" bind:this={el} style="left:{pos.x}px;top:{pos.y}px" role="dialog" aria-label={colorPick.open.title} tabindex="-1" onkeydown={keys} data-automation-id="color-picker">
    <div class="title">{colorPick.open.title}</div>
    <div
      class="square"
      style="background-color:{pure}"
      role="slider"
      tabindex="0"
      aria-label="Saturation and brightness"
      aria-valuenow={Math.round(v * 100)}
      onpointerdown={(e) => drag(e, (x, y) => ((s = x), (v = 1 - y)))}
      onkeydown={squareKeys}
      data-automation-id="color-picker-square"
    >
      <div class="dot" style="left:{s * 100}%;top:{(1 - v) * 100}%;background:{hex}"></div>
    </div>
    <div class="hue" role="slider" tabindex="0" aria-label="Hue" aria-valuenow={Math.round(h)} aria-valuemin={0} aria-valuemax={360} onpointerdown={(e) => drag(e, (x) => (h = Math.min(x * 360, 359.9)))} onkeydown={hueKeys} data-automation-id="color-picker-hue">
      <div class="knob" style="left:{(h / 360) * 100}%"></div>
    </div>
    <div class="row">
      <div class="swatches" title="Before and after (click before to go back to it)">
        <button class="was" style="background:{colorPick.open.start}" aria-label="The colour before" onclick={() => colorPick.open && fromHex(colorPick.open.start)}></button>
        <span class="now" style="background:{hex}"></span>
      </div>
      <input
        class="hex"
        type="text"
        spellcheck="false"
        maxlength="7"
        aria-label="Hex colour"
        value={hexText}
        oninput={(e) => {
          hexText = e.currentTarget.value;
          fromHex(hexText.startsWith("#") ? hexText : "#" + hexText);
        }}
        data-automation-id="color-picker-hex"
      />
      {#if dropper}<button class="ghost" title="Pick a colour from anywhere on the screen" aria-label="Eyedropper" onclick={eyedrop}>💧</button>{/if}
    </div>
    <div class="buttons">
      <button onclick={cancel} data-automation-id="color-picker-cancel">Cancel</button>
      <button class="primary" onclick={ok} data-automation-id="color-picker-ok">OK</button>
    </div>
  </div>
{/if}

<style>
  .shade {
    position: fixed;
    inset: 0;
    z-index: 990;
  }
  .picker {
    position: fixed;
    z-index: 991;
    width: 240px;
    padding: 10px;
    background: var(--panel);
    border: 1px solid var(--line);
    border-radius: var(--radius);
    box-shadow: 0 8px 28px rgb(0 0 0 / 45%);
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .title {
    color: var(--muted);
    font-size: 11px;
  }
  .square {
    position: relative;
    height: 150px;
    border-radius: 3px;
    background-image: linear-gradient(to top, #000, transparent), linear-gradient(to right, #fff, transparent);
    cursor: crosshair;
    touch-action: none;
  }
  .dot {
    position: absolute;
    width: 12px;
    height: 12px;
    margin: -6px 0 0 -6px;
    border: 2px solid #fff;
    border-radius: 50%;
    box-shadow: 0 0 0 1px rgb(0 0 0 / 60%);
    pointer-events: none;
  }
  .hue {
    position: relative;
    height: 12px;
    border-radius: 6px;
    background: linear-gradient(to right, #f00, #ff0, #0f0, #0ff, #00f, #f0f, #f00);
    cursor: ew-resize;
    touch-action: none;
  }
  .knob {
    position: absolute;
    top: -2px;
    width: 8px;
    height: 16px;
    margin-left: -4px;
    border: 2px solid #fff;
    border-radius: 3px;
    box-shadow: 0 0 0 1px rgb(0 0 0 / 60%);
    pointer-events: none;
  }
  .square:focus-visible,
  .hue:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .swatches {
    display: flex;
    border: 1px solid var(--line);
    border-radius: 3px;
    overflow: hidden;
  }
  .swatches .was,
  .swatches .now {
    width: 28px;
    height: 24px;
    padding: 0;
    border: 0;
    border-radius: 0;
    display: block;
  }
  .hex {
    flex: 1;
    min-width: 0;
    font-family: var(--mono, monospace);
  }
  .buttons {
    display: flex;
    justify-content: flex-end;
    gap: 6px;
  }
  .buttons button {
    min-width: 64px;
  }
</style>
