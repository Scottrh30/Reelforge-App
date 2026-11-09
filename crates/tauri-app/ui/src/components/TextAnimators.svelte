<script lang="ts">
  // A title's per-character animators (as After Effects' text animators:
  // position, scale, rotation, opacity, colour and tracking, applied through
  // a range selector whose start, end and offset can be keyframed), and the
  // mask its text runs along.
  import { app, run, tps } from "../lib/session.svelte";
  import { api, type Inspector, type Mask, type TitleSpec } from "../lib/api";
  import NumberField from "./NumberField.svelte";

  type Spec = TitleSpec & Record<string, any>;
  let {
    ins,
    t,
    set,
    once,
    commit,
  }: { ins: Inspector; t: Spec; set: (f: (s: Spec) => void) => void; once: (f: (s: Spec) => void) => Promise<void>; commit: () => Promise<void> } = $props();

  type Key = { at: number; value: number; interp?: string };
  type Selector = {
    start: number;
    end: number;
    offset: number;
    start_keys?: Key[];
    end_keys?: Key[];
    offset_keys?: Key[];
    units: string;
    shape: string;
    amount: number;
    randomize: boolean;
    seed: number;
  };
  type Animator = {
    name: string;
    enabled: boolean;
    position: [number, number];
    scale: [number, number];
    rotation: number;
    opacity: number;
    fill?: [number, number, number, number] | null;
    tracking: number;
    selector: Selector;
  };
  type Rgba = [number, number, number, number];

  const animators = $derived(((t.animators ?? []) as Animator[]).map(withDefaults));
  function withDefaults(a: Partial<Animator>): Animator {
    return {
      name: "Animator",
      enabled: true,
      position: [0, 0],
      scale: [100, 100],
      rotation: 0,
      opacity: 100,
      tracking: 0,
      ...a,
      selector: { start: 0, end: 100, offset: 0, units: "Characters", shape: "Square", amount: 100, randomize: false, seed: 0, ...(a.selector ?? {}) },
    } as Animator;
  }
  function edit(i: number, f: (a: Animator) => void, done: boolean) {
    const go = (s: Spec) => {
      const list = ((s.animators ?? []) as Animator[]).map(withDefaults);
      f(list[i]);
      s.animators = list;
    };
    if (done) once(go);
    else set(go);
  }

  // ---- Time: keys are at times into the clip (ticks) ----
  const here = $derived(Math.max(0, Math.min(ins.duration, Math.round(((app.live?.playhead ?? ins.start) - ins.start) * (ins.speed || 1)))));
  const sec = (s: number) => Math.round(s * tps());
  function valueAt(keys: Key[] | undefined, base: number, at: number): number {
    if (!keys?.length) return base;
    const i = keys.findIndex((k) => k.at > at);
    if (i === 0) return keys[0].value;
    if (i < 0) return keys[keys.length - 1].value;
    const [a, b] = [keys[i - 1], keys[i]];
    const u = (at - a.at) / Math.max(1, b.at - a.at);
    const p = a.interp === "Hold" ? 0 : a.interp === "Ease" || a.interp === "Bezier" ? u * u * (3 - 2 * u) : u;
    return a.value + (b.value - a.value) * p;
  }
  type Which = "start" | "end" | "offset";
  const keysOf = (a: Animator, w: Which) => (a.selector[`${w}_keys`] ?? []) as Key[];
  const valueOf = (a: Animator, w: Which) => valueAt(keysOf(a, w), a.selector[w], here);
  const keyHere = (a: Animator, w: Which) => keysOf(a, w).some((k) => k.at === here);
  /// Typing a value: with keys, it's a key at the playhead (made if needed).
  function setRange(i: number, w: Which, v: number, done: boolean) {
    edit(
      i,
      (a) => {
        const keys = [...keysOf(a, w)];
        if (!keys.length) {
          a.selector[w] = v;
          return;
        }
        const k = keys.findIndex((k) => k.at === here);
        if (k >= 0) keys[k] = { ...keys[k], value: v };
        else {
          keys.push({ at: here, value: v, interp: "Ease" });
          keys.sort((x, y) => x.at - y.at);
        }
        a.selector[`${w}_keys`] = keys;
      },
      done,
    );
  }
  /// The stopwatch: a key at the playhead, or take away the one there.
  function toggleKey(i: number, w: Which) {
    edit(
      i,
      (a) => {
        const keys = [...keysOf(a, w)];
        const k = keys.findIndex((k) => k.at === here);
        if (k >= 0) {
          keys.splice(k, 1);
          // The last key gone: the value stays where it was.
          if (!keys.length) a.selector[w] = valueAt([...keysOf(a, w)], a.selector[w], here);
        } else {
          keys.push({ at: here, value: valueOf(a, w), interp: "Ease" });
          keys.sort((x, y) => x.at - y.at);
        }
        a.selector[`${w}_keys`] = keys;
      },
      true,
    );
  }

  // ---- Presets ----
  // Each brings the text in over its first second: the range's start runs
  // from 0 to 100 %, so letters leave the selection (and settle) one by one.
  const PRESETS: [string, string, Partial<Animator>][] = [
    ["custom", "Empty animator", {}],
    ["fade", "Fade in letters", { opacity: 0 }],
    ["rise", "Rise and fade in", { position: [0, 60], opacity: 0 }],
    ["drop", "Drop in letters", { position: [0, -120], opacity: 0, rotation: -20 }],
    ["scale", "Scale up letters", { scale: [0, 0], opacity: 0 }],
    ["spin", "Spin in letters", { rotation: 360, scale: [0, 0], opacity: 0 }],
    ["track", "Tracking in", { tracking: 40, opacity: 0 }],
    ["blur", "Stretch in", { scale: [400, 30], opacity: 0 }],
    ["colour", "Colour sweep", { fill: [255, 170, 40, 255] }],
    ["words", "Words pop in", { scale: [160, 160], opacity: 0 }],
    ["random", "Letters in random order", { opacity: 0 }],
  ];
  function add(kind: string) {
    const p = PRESETS.find((x) => x[0] === kind);
    if (!p) return;
    const [, label, values] = p;
    const a = withDefaults({ ...values, name: label });
    if (kind === "custom") {
      a.selector.end = 100;
    } else if (kind === "colour") {
      // A band of colour sweeping across and off again.
      a.selector = { ...a.selector, shape: "Smooth", start: 0, end: 30, offset_keys: [{ at: 0, value: -30, interp: "Linear" }, { at: sec(2), value: 100, interp: "Linear" }] };
    } else {
      a.selector = {
        ...a.selector,
        shape: kind === "random" || kind === "words" ? "Square" : "RampUp",
        units: kind === "words" ? "Words" : "Characters",
        randomize: kind === "random",
        seed: kind === "random" ? Math.floor(Math.random() * 1e6) : 0,
        start_keys: [{ at: 0, value: 0, interp: "Ease" }, { at: sec(1), value: 100, interp: "Ease" }],
      };
      // A ramp needs a range wider than the text to let every letter finish.
      if (a.selector.shape === "RampUp") a.selector = { ...a.selector, end: 100, start_keys: [{ at: 0, value: -60, interp: "Ease" }, { at: sec(1.2), value: 100, interp: "Ease" }] };
    }
    once((s) => (s.animators = [...((s.animators ?? []) as Animator[]), a]));
  }
  function remove(i: number) {
    once((s) => (s.animators = ((s.animators ?? []) as Animator[]).filter((_, k) => k !== i)));
  }

  const hex = (c: Rgba) => "#" + c.slice(0, 3).map((v) => v.toString(16).padStart(2, "0")).join("");
  const rgba = (h: string, a: number): Rgba => [parseInt(h.slice(1, 3), 16), parseInt(h.slice(3, 5), 16), parseInt(h.slice(5, 7), 16), a];
  let open = $state<Record<number, boolean>>({});

  // ---- Path ----
  type Path = { mask: number; reverse: boolean; perpendicular: boolean; open: boolean; margin: number };
  const path = $derived((t.path ?? null) as Path | null);
  const masks = $derived(ins.masks.map((m) => m.mask));
  /// Use mask `i` (or none) as the path; it then doesn't cut the layer.
  async function usePath(i: number | null) {
    const all = masks.map((m, k) => ({ ...m, text_path: k === i ? true : path && k === path.mask ? false : m.text_path }));
    if (i != null || path) await run(api.setMasks(ins.id, all as Mask[], true));
    await once((s) => (s.path = i == null ? null : { mask: i, reverse: false, perpendicular: true, open: masks[i]?.shape === "Freehand", margin: 0 }));
  }
  async function newPath(shape: "ellipse" | "rectangle") {
    const i = await run(api.addMask(ins.id, shape));
    if (typeof i !== "number") return;
    // The state's back with the new mask: flag it as a path, then use it.
    const ms = (app.s?.inspector?.masks ?? []).map((m) => m.mask);
    if (ms[i]) await run(api.setMasks(ins.id, ms.map((m, k) => ({ ...m, text_path: k === i ? true : m.text_path })) as Mask[], true));
    await once((s) => (s.path = { mask: i, reverse: false, perpendicular: true, open: false, margin: 0 }));
  }
  const UNITS = [
    ["Characters", "Characters"],
    ["CharactersNoSpaces", "Characters (no spaces)"],
    ["Words", "Words"],
    ["Lines", "Lines"],
  ];
  const SHAPES = [
    ["Square", "Square"],
    ["RampUp", "Ramp up"],
    ["RampDown", "Ramp down"],
    ["Triangle", "Triangle"],
    ["Round", "Round"],
    ["Smooth", "Smooth"],
  ];
</script>

<div class="block">
  <div class="head">
    <h4>Animators</h4>
    <select
      class="add"
      aria-label="Add an animator"
      value=""
      onchange={(e) => {
        add(e.currentTarget.value);
        e.currentTarget.value = "";
      }}
    >
      <option value="" disabled>+ Add animator…</option>
      {#each PRESETS as [k, label]}<option value={k}>{label}</option>{/each}
    </select>
  </div>
  {#if !animators.length}
    <p class="note">Animate letters, words or lines one after another: an animator moves, scales, turns, fades or colours the characters its range picks, and keyframing the range (start, end or offset) sweeps it across the text. The presets bring the text in over its first second.</p>
  {/if}
  {#each animators as a, i}
    <div class="anim" class:off={!a.enabled}>
      <div class="arow">
        <input type="checkbox" checked={a.enabled} aria-label="On" title="On" onchange={(e) => edit(i, (n) => (n.enabled = e.currentTarget.checked), true)} />
        <button class="twisty" onclick={() => (open[i] = !(open[i] ?? true))} aria-expanded={open[i] ?? true} aria-label="Show or hide its settings">{(open[i] ?? true) ? "▾" : "▸"}</button>
        <input class="name" value={a.name} aria-label="Animator name" onchange={(e) => edit(i, (n) => (n.name = e.currentTarget.value), true)} />
        <button class="ghost small" onclick={() => remove(i)} title="Delete this animator" aria-label="Delete">✕</button>
      </div>
      {#if open[i] ?? true}
        <div class="grid">
          <span class="lbl">Position</span>
          <div class="pair">
            <NumberField label="Position X" value={a.position[0]} step={1} digits={0} suffix=" px" oninput={(v) => edit(i, (n) => (n.position = [v, n.position[1]]), false)} oncommit={commit} />
            <NumberField label="Position Y" value={a.position[1]} step={1} digits={0} suffix=" px" oninput={(v) => edit(i, (n) => (n.position = [n.position[0], v]), false)} oncommit={commit} />
          </div>
          <span class="lbl">Scale</span>
          <div class="pair">
            <NumberField label="Scale X" value={a.scale[0]} min={-1000} max={2000} step={1} digits={0} suffix="%" oninput={(v) => edit(i, (n) => (n.scale = [v, n.scale[1]]), false)} oncommit={commit} />
            <NumberField label="Scale Y" value={a.scale[1]} min={-1000} max={2000} step={1} digits={0} suffix="%" oninput={(v) => edit(i, (n) => (n.scale = [n.scale[0], v]), false)} oncommit={commit} />
          </div>
          <span class="lbl">Rotation</span>
          <NumberField label="Rotation" value={a.rotation} min={-3600} max={3600} step={1} digits={0} suffix="°" oninput={(v) => edit(i, (n) => (n.rotation = v), false)} oncommit={commit} />
          <span class="lbl">Opacity</span>
          <NumberField label="Opacity" value={a.opacity} min={0} max={100} step={1} digits={0} suffix="%" oninput={(v) => edit(i, (n) => (n.opacity = v), false)} oncommit={commit} />
          <span class="lbl"><label class="check"><input type="checkbox" checked={!!a.fill} onchange={(e) => { const on = e.currentTarget.checked; edit(i, (n) => (n.fill = on ? [255, 170, 40, 255] : null), true); }} /> Colour</label></span>
          <div class="pair" class:dim={!a.fill}>
            <input type="color" value={hex((a.fill ?? [255, 170, 40, 255]) as Rgba)} disabled={!a.fill} aria-label="Fill colour" oninput={(e) => edit(i, (n) => (n.fill = rgba(e.currentTarget.value, (n.fill ?? [0, 0, 0, 255])[3])), false)} onchange={commit} />
            <NumberField label="How far towards the colour" value={Math.round(((a.fill ?? [0, 0, 0, 255])[3] / 255) * 100)} min={0} max={100} step={1} digits={0} suffix="%" oninput={(v) => edit(i, (n) => n.fill && (n.fill = [n.fill[0], n.fill[1], n.fill[2], Math.round((v / 100) * 255)]), false)} oncommit={commit} />
          </div>
          <span class="lbl" title="Extra space after each selected character">Tracking</span>
          <NumberField label="Tracking" value={a.tracking} min={-500} max={2000} step={0.5} digits={1} suffix=" px" oninput={(v) => edit(i, (n) => (n.tracking = v), false)} oncommit={commit} />

          <span class="lbl sect">Range</span>
          <span class="hint">◆ adds or takes away a key at the playhead</span>
          {#each [["start", "Start"], ["end", "End"], ["offset", "Offset"]] as [w, label]}
            <span class="lbl sub">{label}</span>
            <div class="pair key">
              <NumberField label={`Range ${label.toLowerCase()}`} value={Math.round(valueOf(a, w as Which) * 10) / 10} min={w === "offset" ? -200 : -100} max={200} step={0.5} digits={1} suffix="%" oninput={(v) => setRange(i, w as Which, v, false)} oncommit={commit} />
              <button class="stopwatch" class:on={keyHere(a, w as Which)} class:keyed={keysOf(a, w as Which).length > 0} onclick={() => toggleKey(i, w as Which)} title={keyHere(a, w as Which) ? "Take away the key at the playhead" : "Add a key at the playhead"} aria-label={`Key ${label}`}>◆</button>
            </div>
          {/each}
          <span class="lbl sub">Based on</span>
          <select value={a.selector.units} aria-label="Based on" onchange={(e) => edit(i, (n) => (n.selector.units = e.currentTarget.value), true)}>
            {#each UNITS as [v, l]}<option value={v}>{l}</option>{/each}
          </select>
          <span class="lbl sub">Shape</span>
          <select value={a.selector.shape} aria-label="Shape" onchange={(e) => edit(i, (n) => (n.selector.shape = e.currentTarget.value), true)}>
            {#each SHAPES as [v, l]}<option value={v}>{l}</option>{/each}
          </select>
          <span class="lbl sub">Amount</span>
          <NumberField label="Amount" value={a.selector.amount} min={-100} max={100} step={1} digits={0} suffix="%" oninput={(v) => edit(i, (n) => (n.selector.amount = v), false)} oncommit={commit} />
          <span></span>
          <label class="check"><input type="checkbox" checked={a.selector.randomize} onchange={(e) => { const on = e.currentTarget.checked; edit(i, (n) => { n.selector.randomize = on; if (on) n.selector.seed = Math.floor(Math.random() * 1e6); }, true); }} /> Random order</label>
        </div>
      {/if}
    </div>
  {/each}
</div>

<div class="block">
  <div class="head"><h4>Path</h4></div>
  <div class="grid">
    <span class="lbl" title="Set the text along the outline of one of this clip's masks (it then doesn't cut the layer)">Along</span>
    <select value={path ? String(path.mask) : ""} aria-label="Text path" onchange={(e) => usePath(e.currentTarget.value === "" ? null : Number(e.currentTarget.value))}>
      <option value="">None (straight lines)</option>
      {#each masks as m, k}<option value={String(k)}>{m.name || `Mask ${k + 1}`}</option>{/each}
    </select>
    {#if !path}
      <span></span>
      <div class="row wrap">
        <button class="small" onclick={() => newPath("ellipse")} title="Add a circle mask and set the text round it">Round a circle</button>
        <span class="hint">or draw a freehand mask (Masks, in Controls) and pick it here</span>
      </div>
    {:else}
      <span class="lbl sub">Margin</span>
      <NumberField label="Margin: how far along the path the text starts" value={path.margin} step={1} digits={0} suffix=" px" oninput={(v) => set((s) => (s.path = { ...s.path, margin: v }))} oncommit={commit} />
      <span></span>
      <div class="checks">
        <label class="check" title="Letters turn to follow the path (otherwise they stay upright)"><input type="checkbox" checked={path.perpendicular} onchange={(e) => once((s) => (s.path = { ...s.path, perpendicular: e.currentTarget.checked }))} /> Letters follow it</label>
        <label class="check" title="Run the other way round it (round a circle: inside it)"><input type="checkbox" checked={path.reverse} onchange={(e) => once((s) => (s.path = { ...s.path, reverse: e.currentTarget.checked }))} /> Reverse</label>
        {#if masks[path.mask]?.shape !== "Ellipse"}
          <label class="check" title="Don't join the last point back to the first"><input type="checkbox" checked={path.open} onchange={(e) => once((s) => (s.path = { ...s.path, open: e.currentTarget.checked }))} /> Open path</label>
        {/if}
      </div>
      <span></span>
      <p class="note">Alignment sets where it sits: Left starts at the path's first point, Centre centres on it (round a closed path) or in the middle of an open one, Right ends there. Edit, animate or track the mask to move the path.</p>
    {/if}
  </div>
</div>

<style>
  .block {
    padding: 8px 0 12px;
    border-bottom: 1px solid var(--line-soft);
  }
  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 6px;
  }
  h4 {
    font-size: calc(12.5px * var(--text-scale));
    font-weight: 600;
    color: var(--muted);
    margin: 0;
  }
  .add {
    max-width: 60%;
  }
  .anim {
    margin-top: 8px;
    border: 1px solid var(--line);
    border-radius: 6px;
    padding: 6px 8px 8px;
    background: color-mix(in srgb, var(--well) 40%, transparent);
  }
  .anim.off .grid {
    opacity: 0.55;
  }
  .arow {
    display: flex;
    gap: 6px;
    align-items: center;
  }
  .name {
    flex: 1;
    min-width: 0;
  }
  .twisty {
    background: none;
    border: 0;
    padding: 0 2px;
    min-height: 0;
    color: var(--muted);
  }
  .grid {
    display: grid;
    grid-template-columns: 86px minmax(0, 1fr);
    gap: 6px;
    align-items: center;
    margin-top: 8px;
  }
  .lbl {
    color: var(--muted);
  }
  .sub {
    padding-left: 14px;
  }
  .sect {
    color: var(--text);
    font-weight: 600;
    margin-top: 4px;
  }
  .pair {
    display: flex;
    gap: 6px;
    min-width: 0;
    align-items: center;
  }
  .pair > :global(*:not(input[type="color"]):not(button)) {
    flex: 1;
    min-width: 0;
  }
  .dim {
    opacity: 0.5;
  }
  .stopwatch {
    min-height: 22px;
    padding: 0 6px;
    color: var(--muted);
    flex: none;
  }
  .stopwatch.keyed {
    color: color-mix(in srgb, var(--accent) 60%, var(--muted));
  }
  .stopwatch.on {
    color: var(--accent);
    border-color: var(--accent);
  }
  .row {
    display: flex;
    gap: 6px;
    align-items: center;
  }
  .wrap {
    flex-wrap: wrap;
  }
  .checks {
    display: flex;
    flex-wrap: wrap;
    gap: 4px 12px;
  }
  .check {
    display: flex;
    gap: 5px;
    align-items: center;
    color: var(--muted);
  }
  .note,
  .hint {
    margin: 0;
    color: var(--muted);
    font-size: calc(11.5px * var(--text-scale));
    line-height: 1.4;
  }
  .note {
    margin-top: 6px;
  }
  input[type="color"] {
    width: 40px;
    height: 24px;
    padding: 0;
    border: 1px solid var(--line);
    background: none;
    border-radius: var(--radius);
    flex: none;
  }
  .small {
    min-height: 22px;
    padding: 0 8px;
    font-size: calc(12px * var(--text-scale));
  }
</style>
