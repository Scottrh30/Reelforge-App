<script lang="ts">
  // The Colour wheels effect's four wheels (Lift, Gamma, Gain, Offset): drag
  // anywhere on a wheel to push its puck towards a colour (Shift for fine
  // moves, double-click to centre it), and the number under it to brighten
  // or darken. The wheels are laid out like a vectorscope (up is red, right
  // is blue), so the trace moves the way the puck does.
  import NumberField from "./NumberField.svelte";

  let {
    values,
    onset,
    oncommit,
  }: {
    /// All the effect's values, in the core's order (see core effects::wheels).
    values: number[];
    /// [param, value] pairs, changed together.
    onset: (values: [number, number][]) => void;
    oncommit: () => void;
  } = $props();

  const WHEELS = [
    { name: "Lift", at: 0, tip: "The shadows: black moves most, white not at all" },
    { name: "Gamma", at: 3, tip: "The midtones: black and white stay put" },
    { name: "Gain", at: 6, tip: "The highlights: white moves most, black not at all" },
    { name: "Offset", at: 9, tip: "Everything, by the same amount" },
  ];
  const R = 46;

  /// The colour a puck at (x, y) (-1 to 1) pushes towards (as core wheels::tint, on mid-grey).
  function tint(x: number, y: number, k = 1): [number, number, number] {
    const cb = x * 0.35 * k;
    const cr = y * 0.35 * k;
    return [0.5 + 1.5748 * cr, 0.5 - 0.1873 * cb - 0.4681 * cr, 0.5 + 1.8556 * cb];
  }
  const css = (c: number[]) => `rgb(${c.map((v) => Math.round(Math.min(1, Math.max(0, v)) * 255)).join(",")})`;
  // The ring: CSS conic angles start at the top and go clockwise.
  const ring = (() => {
    const stops: [number, string][] = [];
    for (let k = 0; k < 24; k++) {
      const th = (k / 24) * 2 * Math.PI;
      const deg = (((90 - (k / 24) * 360) % 360) + 360) % 360;
      stops.push([deg, css(tint(Math.cos(th), Math.sin(th), 1.25))]);
    }
    stops.sort((a, b) => a[0] - b[0]);
    // (One stop falls at 0°; it closes the loop at 360°.)
    return `conic-gradient(${[...stops.map(([d, c]) => `${c} ${d.toFixed(1)}deg`), `${stops[0][1]} 360deg`].join(", ")})`;
  })();

  function drag(e: PointerEvent, at: number) {
    if (e.button !== 0) return;
    const el = e.currentTarget as HTMLElement;
    el.setPointerCapture(e.pointerId);
    let [x, y] = [values[at], values[at + 1]];
    let [px, py] = [e.clientX, e.clientY];
    let moved = false;
    const move = (ev: PointerEvent) => {
      const k = (ev.shiftKey ? 0.2 : 1) * (100 / R);
      x += (ev.clientX - px) * k;
      y -= (ev.clientY - py) * k;
      [px, py] = [ev.clientX, ev.clientY];
      // Kept inside the rim.
      const r = Math.hypot(x, y);
      const [cx, cy] = r > 100 ? [(x / r) * 100, (y / r) * 100] : [x, y];
      moved = true;
      onset([
        [at, Math.round(cx * 10) / 10],
        [at + 1, Math.round(cy * 10) / 10],
      ]);
    };
    const up = () => {
      el.removeEventListener("pointermove", move);
      el.removeEventListener("pointerup", up);
      el.removeEventListener("pointercancel", up);
      if (moved) oncommit();
    };
    el.addEventListener("pointermove", move);
    el.addEventListener("pointerup", up);
    el.addEventListener("pointercancel", up);
  }
  function centre(at: number) {
    onset([
      [at, 0],
      [at + 1, 0],
    ]);
    oncommit();
  }
  function reset(at: number) {
    onset([
      [at, 0],
      [at + 1, 0],
      [at + 2, 0],
    ]);
    oncommit();
  }
  function key(e: KeyboardEvent, at: number) {
    const d = e.shiftKey ? 0.5 : 2;
    const step: Record<string, [number, number]> = { ArrowLeft: [-d, 0], ArrowRight: [d, 0], ArrowUp: [0, d], ArrowDown: [0, -d] };
    const s = step[e.key];
    if (!s) return;
    e.preventDefault();
    onset([
      [at, Math.max(-100, Math.min(100, values[at] + s[0]))],
      [at + 1, Math.max(-100, Math.min(100, values[at + 1] + s[1]))],
    ]);
    oncommit();
  }
  /// What a wheel's push looks like on mid-grey, for its swatch.
  const swatch = (at: number) => css(tint(values[at] / 100, values[at + 1] / 100, 1.6));
  const hueName = (x: number, y: number) => {
    if (Math.hypot(x, y) < 0.5) return "neutral";
    const a = ((Math.atan2(y, x) * 180) / Math.PI + 360) % 360;
    const names: [number, string][] = [[-5, "blue"], [50, "magenta"], [103, "red"], [175, "yellow"], [230, "green"], [283, "cyan"], [355, "blue"]];
    let best = names[0];
    for (const n of names) if (Math.abs(n[0] - a) < Math.abs(best[0] - a)) best = n;
    return `${best[1]} ${Math.round(Math.hypot(x, y))}%`;
  };
</script>

<div class="wheels">
  {#each WHEELS as w}
    {@const x = values[w.at]}
    {@const y = values[w.at + 1]}
    <div class="wheel" title={w.tip}>
      <div class="top">
        <span class="name">{w.name}</span>
        <span class="sw" style="background:{swatch(w.at)}" title="Its push, on mid-grey"></span>
        {#if x || y || values[w.at + 2]}<button class="ghost tiny" onclick={() => reset(w.at)} title="Reset {w.name}" aria-label="Reset {w.name}">↺</button>{/if}
      </div>
      <div
        class="disc"
        style="width:{R * 2}px;height:{R * 2}px;--ring:{ring}"
        role="slider"
        tabindex="0"
        aria-label="{w.name} colour"
        aria-valuetext={hueName(x, y)}
        aria-valuenow={Math.round(Math.hypot(x, y))}
        onpointerdown={(e) => drag(e, w.at)}
        ondblclick={() => centre(w.at)}
        onkeydown={(e) => key(e, w.at)}
      >
        <div class="cross"></div>
        <div class="puck" style="left:{R + (x / 100) * (R - 6)}px;top:{R - (y / 100) * (R - 6)}px"></div>
      </div>
      <span class="hue">{hueName(x, y)}</span>
      <NumberField label="{w.name} brightness" value={values[w.at + 2]} min={-100} max={100} step={0.2} digits={1} suffix=" %" oninput={(v) => onset([[w.at + 2, v]])} {oncommit} />
    </div>
  {/each}
</div>
<div class="overall">
  {#each [["Saturation", 12, 0, 200, 0.5, " %"], ["Contrast", 13, 0, 200, 0.5, " %"], ["Pivot", 14, 0, 100, 0.2, " %"], ["Hue", 15, -180, 180, 0.5, "°"]] as [label, p, lo, hi, st, suf]}
    <label>
      <span>{label}</span>
      <NumberField label={label as string} value={values[p as number]} min={lo as number} max={hi as number} step={st as number} digits={1} suffix={suf as string} oninput={(v) => onset([[p as number, v]])} {oncommit} />
    </label>
  {/each}
</div>

<style>
  .wheels {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(112px, 1fr));
    gap: 10px 6px;
    margin: 6px 0;
  }
  .wheel {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 4px;
  }
  .top {
    display: flex;
    align-items: center;
    gap: 5px;
    min-height: 18px;
  }
  .name {
    font-weight: 600;
  }
  .sw {
    width: 10px;
    height: 10px;
    border-radius: 50%;
    border: 1px solid var(--line);
  }
  .disc {
    position: relative;
    border-radius: 50%;
    background: radial-gradient(circle, #232326 0 80%, transparent 82%), var(--ring);
    box-shadow: inset 0 0 0 1px rgba(0, 0, 0, 0.5);
    cursor: grab;
    touch-action: none;
  }
  .disc:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  .disc::after {
    /* The colour field: faint hues towards the rim. */
    content: "";
    position: absolute;
    inset: 8%;
    border-radius: 50%;
    background: var(--ring);
    opacity: 0.22;
    mask: radial-gradient(circle, transparent 0, #000 100%);
    pointer-events: none;
  }
  .cross {
    position: absolute;
    inset: 0;
    background:
      linear-gradient(rgba(255, 255, 255, 0.18), rgba(255, 255, 255, 0.18)) center / 1px 60% no-repeat,
      linear-gradient(rgba(255, 255, 255, 0.18), rgba(255, 255, 255, 0.18)) center / 60% 1px no-repeat;
    pointer-events: none;
  }
  .puck {
    position: absolute;
    width: 10px;
    height: 10px;
    margin: -6px 0 0 -6px;
    border-radius: 50%;
    border: 2px solid #fff;
    box-shadow: 0 0 0 1px rgba(0, 0, 0, 0.7);
    pointer-events: none;
    z-index: 1;
  }
  .hue {
    font-size: calc(11px * var(--text-scale));
    color: var(--muted);
  }
  .overall {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(140px, 1fr));
    gap: 4px 10px;
    margin-bottom: 6px;
  }
  .overall label {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 6px;
  }
  .overall span {
    color: var(--muted);
  }
</style>
