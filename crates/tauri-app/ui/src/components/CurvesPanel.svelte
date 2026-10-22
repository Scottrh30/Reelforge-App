<script lang="ts">
  // The Curves tab: the selected clip's keyframes as value graphs. Tick
  // channels on the left (transform and effect settings); drag keys in time
  // and value (Shift: value only), double-click a line to add a key,
  // double-click a key to delete it, right-click a key for its easing.
  import { untrack } from "svelte";
  import { app, run, timecode, snapToFrame } from "../lib/session.svelte";
  import { api, type ChannelView, type Interp } from "../lib/api";
  import { livePump } from "../lib/live";
  import { openMenu } from "../lib/menu.svelte";

  const COLORS = ["#f05a5a", "#6ed26e", "#64a0ff", "#ffc846", "#d278ff", "#50dcdc", "#ff963c", "#dcdcdc"];
  const M = { l: 52, r: 14, t: 22, b: 20 };
  const pump = livePump();

  const ins = $derived(app.s?.inspector ?? null);
  const keys = $derived(ins?.keys ?? null);
  const channels = $derived(keys?.channels ?? []);
  const colorOf = (id: string) => COLORS[Math.max(0, channels.findIndex((c) => c.id === id)) % COLORS.length];

  // Which channels are drawn: per clip, starting with the keyframed ones.
  $effect(() => {
    const id = ins?.id;
    const ch = channels;
    if (id == null || !keys) return;
    untrack(() => {
      if (!(id in app.curvesShown)) app.curvesShown[id] = ch.filter((c) => c.keys.length).map((c) => c.id);
    });
  });
  const shownIds = $derived(ins ? (app.curvesShown[ins.id] ?? []) : []);
  const shown = $derived(channels.filter((c) => shownIds.includes(c.id)));
  function toggle(id: string) {
    if (!ins) return;
    const cur = app.curvesShown[ins.id] ?? [];
    app.curvesShown[ins.id] = cur.includes(id) ? cur.filter((x) => x !== id) : [...cur, id];
  }
  const groups = $derived.by(() => {
    const g: { name: string; items: ChannelView[] }[] = [];
    for (const c of channels) {
      let last = g[g.length - 1];
      if (!last || last.name !== c.group) g.push((last = { name: c.group, items: [] }));
      last.items.push(c);
    }
    return g;
  });

  // ---- Geometry ----
  let box = $state<HTMLDivElement | null>(null);
  let W = $state(600);
  let H = $state(200);
  $effect(() => {
    if (!box) return;
    const ro = new ResizeObserver(() => {
      const r = box!.getBoundingClientRect();
      W = r.width;
      H = r.height;
    });
    ro.observe(box);
    return () => ro.disconnect();
  });
  const pw = $derived(Math.max(10, W - M.l - M.r));
  const ph = $derived(Math.max(10, H - M.t - M.b));
  const span = $derived(keys?.span ?? [0, 1]);
  const xOf = (t: number) => M.l + ((t - span[0]) / Math.max(1, span[1] - span[0])) * pw;
  const tOf = (x: number) => span[0] + ((x - M.l) / pw) * (span[1] - span[0]);

  // Value range: everything shown fits, frozen while a key is dragged.
  let frozen = $state<[number, number] | null>(null);
  const fitted = $derived.by((): [number, number] => {
    let lo = Infinity;
    let hi = -Infinity;
    for (const c of shown) {
      for (const v of c.samples.length ? c.samples : [c.value]) (lo = Math.min(lo, v)), (hi = Math.max(hi, v));
      for (const k of c.keys) (lo = Math.min(lo, k.value)), (hi = Math.max(hi, k.value));
    }
    if (!Number.isFinite(lo)) return [0, 100];
    const extra = Math.max((hi - lo) * 0.1, Math.max(1, Math.max(Math.abs(hi), Math.abs(lo)) * 0.05));
    return [lo - extra, hi + extra];
  });
  const range = $derived(frozen ?? fitted);
  const yOf = (v: number) => M.t + ph - ((v - range[0]) / (range[1] - range[0])) * ph;
  const vOf = (y: number) => range[0] + ((M.t + ph - y) / ph) * (range[1] - range[0]);

  function niceStep(spanV: number, target: number) {
    const raw = Math.max(spanV / target, 1e-6);
    const mag = 10 ** Math.floor(Math.log10(raw));
    return [1, 2, 5, 10].map((m) => m * mag).find((s) => s >= raw) ?? 10 * mag;
  }
  const vlines = $derived.by(() => {
    const step = niceStep(range[1] - range[0], Math.max(2, ph / 30));
    const out: { y: number; label: string }[] = [];
    for (let v = Math.ceil(range[0] / step) * step; v <= range[1]; v += step) out.push({ y: yOf(v), label: step < 1 ? v.toFixed(2) : v.toFixed(0) });
    return out;
  });
  const tlines = $derived.by(() => {
    const T = app.s?.ticks_per_second ?? 1;
    const secs = (span[1] - span[0]) / T;
    const step = niceStep(secs, Math.max(2, pw / 120)) * T;
    const out: { x: number; label: string }[] = [];
    for (let t = Math.ceil(span[0] / step) * step; t <= span[1]; t += step) out.push({ x: xOf(t), label: timecode(Math.max(0, t)) });
    return out;
  });
  const path = (c: ChannelView) => {
    if (!c.samples.length) return `M${M.l},${yOf(c.value)}H${M.l + pw}`;
    const n = c.samples.length;
    return c.samples.map((v, i) => `${i ? "L" : "M"}${(M.l + (i / (n - 1)) * pw).toFixed(1)},${yOf(v).toFixed(1)}`).join("");
  };

  // ---- Keys ----
  let selected = $state<{ ch: string; index: number } | null>(null);
  let drag = $state<{ ch: string; index: number; at: number; value: number } | null>(null);
  let svg = $state<SVGSVGElement | null>(null);
  const local = (e: PointerEvent | MouseEvent) => {
    const r = svg!.getBoundingClientRect();
    return { x: e.clientX - r.left, y: e.clientY - r.top };
  };
  const keyPos = (c: ChannelView, i: number) => (drag && drag.ch === c.id && drag.index === i ? { at: drag.at, value: drag.value } : c.keys[i]);

  function keyDown(e: PointerEvent, c: ChannelView, i: number) {
    if (e.button !== 0 || !ins) return;
    e.stopPropagation();
    svg!.setPointerCapture(e.pointerId);
    const id = ins.id;
    selected = { ch: c.id, index: i };
    frozen = range;
    const p0 = local(e);
    const k0 = c.keys[i];
    let moved = false;
    const move = (ev: PointerEvent) => {
      const p = local(ev);
      if (!moved && Math.hypot(p.x - p0.x, p.y - p0.y) < 3) return;
      moved = true;
      const at = ev.shiftKey ? k0.at : snapToFrame(tOf(p.x));
      let value = vOf(p.y);
      if (c.range) value = Math.min(c.range[1], Math.max(c.range[0], value));
      drag = { ch: c.id, index: i, at, value };
      pump.push(async () => {
        const used = await api.keyMove(id, c.id, i, at, value, ev.shiftKey);
        if (used != null && drag && drag.ch === c.id && drag.index === i) drag.at = used;
      });
    };
    const up = async () => {
      svg!.removeEventListener("pointermove", move);
      svg!.removeEventListener("pointerup", up);
      if (moved) {
        await pump.done();
        await run(api.commit());
        if (drag) api.seek(drag.at);
      } else {
        api.seek(k0.at);
      }
      drag = null;
      frozen = null;
    };
    svg!.addEventListener("pointermove", move);
    svg!.addEventListener("pointerup", up);
  }

  function keyMenu(e: MouseEvent, c: ChannelView, i: number) {
    if (!ins) return;
    const id = ins.id;
    selected = { ch: c.id, index: i };
    const cur = c.keys[i]?.interp;
    const set = (m: Interp) => run(api.keyInterp(id, c.id, i, m));
    openMenu(e, [
      { heading: true, label: `${c.name} · ${timecode(c.keys[i].at)}` },
      { label: "Linear", checked: cur === "Linear", run: () => set("Linear") },
      { label: "Ease in and out", checked: cur === "Ease", run: () => set("Ease") },
      { label: "Hold (jump to the next key)", checked: cur === "Hold", run: () => set("Hold") },
      { label: "Bézier (drag its handles)", checked: cur === "Bezier", run: () => set("Bezier") },
      { separator: true },
      { label: "Go to this keyframe", run: () => api.seek(c.keys[i].at) },
      { label: "Delete keyframe", run: () => ((selected = null), run(api.keyDelete(id, c.id, i))) },
    ]);
  }

  // ---- Bézier handles (of the selected key) ----
  // Stored as fractions of the segment they shape: x of its time, y of its
  // change in value, the in-handle measured back from its key.
  type Handle = { out: boolean; x: number; y: number; kx: number; ky: number; flat: boolean };
  let hdrag = $state<{ ch: string; index: number; out: boolean; h: [number, number] } | null>(null);
  const handlesOf = (c: ChannelView, i: number): Handle[] => {
    const out: Handle[] = [];
    const k = keyPos(c, i);
    const fr = (out_: boolean, stored: [number, number]) => (hdrag && hdrag.ch === c.id && hdrag.index === i && hdrag.out === out_ ? hdrag.h : stored);
    if (i > 0 && c.keys[i - 1].interp === "Bezier") {
      const a = keyPos(c, i - 1);
      const h = fr(false, c.keys[i].ease_in);
      const dv = k.value - a.value;
      out.push({ out: false, x: xOf(k.at - h[0] * (k.at - a.at)), y: yOf(k.value - h[1] * dv), kx: xOf(k.at), ky: yOf(k.value), flat: Math.abs(dv) < 1e-6 });
    }
    if (i < c.keys.length - 1 && c.keys[i].interp === "Bezier") {
      const b = keyPos(c, i + 1);
      const h = fr(true, c.keys[i].ease_out);
      const dv = b.value - k.value;
      out.push({ out: true, x: xOf(k.at + h[0] * (b.at - k.at)), y: yOf(k.value + h[1] * dv), kx: xOf(k.at), ky: yOf(k.value), flat: Math.abs(dv) < 1e-6 });
    }
    return out;
  };

  function handleDown(e: PointerEvent, c: ChannelView, i: number, h: Handle) {
    if (e.button !== 0 || !ins) return;
    e.stopPropagation();
    svg!.setPointerCapture(e.pointerId);
    const id = ins.id;
    frozen = range;
    const k = c.keys[i];
    const other = c.keys[h.out ? i + 1 : i - 1];
    const dt = h.out ? other.at - k.at : k.at - other.at;
    const dv = h.out ? other.value - k.value : k.value - other.value;
    const start: [number, number] = h.out ? [...k.ease_out] : [...k.ease_in];
    const move = (ev: PointerEvent) => {
      const p = local(ev);
      const t = tOf(p.x);
      const v = vOf(p.y);
      let x = h.out ? (t - k.at) / Math.max(1, dt) : (k.at - t) / Math.max(1, dt);
      x = Math.min(1, Math.max(0, x));
      // A flat segment has no change in value to scale by: only its timing moves.
      let y = Math.abs(dv) < 1e-6 ? start[1] : h.out ? (v - k.value) / dv : (k.value - v) / dv;
      // Shift: keep the handle level with the key (pure ease, no speed-up).
      if (ev.shiftKey) y = 0;
      y = Math.min(10, Math.max(-10, y));
      hdrag = { ch: c.id, index: i, out: h.out, h: [x, y] };
      pump.push(() => api.keyHandle(id, c.id, i, h.out, x, y));
    };
    const up = async () => {
      svg!.removeEventListener("pointermove", move);
      svg!.removeEventListener("pointerup", up);
      if (hdrag) {
        await pump.done();
        await run(api.commit());
      }
      hdrag = null;
      frozen = null;
    };
    svg!.addEventListener("pointermove", move);
    svg!.addEventListener("pointerup", up);
  }

  function lineDouble(e: MouseEvent, c: ChannelView) {
    if (!ins) return;
    const p = local(e);
    run(api.keyAdd(ins.id, c.id, snapToFrame(tOf(p.x)))).then((k) => {
      if (k != null) selected = { ch: c.id, index: k };
    });
  }

  // Click or drag on empty graph: move the playhead.
  function bgDown(e: PointerEvent) {
    if (e.button !== 0) return;
    selected = null;
    svg!.setPointerCapture(e.pointerId);
    const go = (ev: PointerEvent) => api.seek(Math.max(0, snapToFrame(tOf(local(ev).x))));
    go(e);
    const up = () => {
      svg!.removeEventListener("pointermove", go);
      svg!.removeEventListener("pointerup", up);
    };
    svg!.addEventListener("pointermove", go);
    svg!.addEventListener("pointerup", up);
  }

  const sel = $derived.by(() => {
    if (!selected) return null;
    const c = channels.find((x) => x.id === selected!.ch);
    const k = c?.keys[selected.index];
    return c && k ? { c, k: keyPos(c, selected.index) as { at: number; value: number }, interp: k.interp } : null;
  });
  const fmt = (v: number, unit: string) => `${Math.abs(v) >= 100 ? v.toFixed(1) : v.toFixed(2)}${unit}`;
  const clipX = $derived(ins ? [xOf(ins.start), xOf(ins.start + ins.duration)] : [0, 0]);
  const playX = $derived(xOf(app.live?.playhead ?? 0));
</script>

{#if !ins || !keys}
  <div class="empty">
    <p><strong>Curves</strong> show a clip's keyframes as graphs.</p>
    <p class="muted">Select a video, picture or title clip. Add keyframes with the ◆ buttons in the Controls panel (transform and effect settings), then shape them here.</p>
  </div>
{:else}
  <div class="curves">
    <div class="list" aria-label="Channels">
      <div class="listhead">
        <span class="muted">{ins.name}</span>
        <button class="ghost tiny" onclick={() => (app.curvesShown[ins.id] = channels.filter((c) => c.keys.length).map((c) => c.id))} title="Show just the keyframed channels">Keyed</button>
      </div>
      {#each groups as g}
        <div class="group">{g.name}</div>
        {#each g.items as c (c.id)}
          {@const on = shownIds.includes(c.id)}
          <label class="ch" class:on class:keyed={c.keys.length > 0} style="--c:{colorOf(c.id)}">
            <input type="checkbox" checked={on} onchange={() => toggle(c.id)} />
            <span class="dot"></span>
            <span class="cname">{c.name}</span>
            {#if c.keys.length}<span class="count">{c.keys.length}</span>{/if}
          </label>
        {/each}
      {/each}
    </div>
    <div class="graph" bind:this={box}>
      <svg bind:this={svg} width={W} height={H} role="application" aria-label="Keyframe curves" onpointerdown={bgDown} oncontextmenu={(e) => e.preventDefault()}>
        <rect x={clipX[0]} y={M.t} width={Math.max(0, clipX[1] - clipX[0])} height={ph} class="clip" />
        {#each vlines as l}
          <line x1={M.l} x2={M.l + pw} y1={l.y} y2={l.y} class="grid" />
          <text x={M.l - 6} y={l.y + 3.5} class="vlabel">{l.label}</text>
        {/each}
        {#each tlines as l}
          <line x1={l.x} x2={l.x} y1={M.t} y2={M.t + ph} class="grid faint" />
          <text x={l.x + 3} y={H - 5} class="tlabel">{l.label}</text>
        {/each}
        <rect x={M.l} y={M.t} width={pw} height={ph} class="frame" />
        {#each shown as c (c.id)}
          <path d={path(c)} stroke={colorOf(c.id)} class="line" />
          <!-- a wide invisible copy to double-click on -->
          <path d={path(c)} class="hit" role="presentation" ondblclick={(e) => lineDouble(e, c)} onpointerdown={(e) => e.button === 0 && e.detail > 1 && e.stopPropagation()} />
        {/each}
        {#each shown as c (c.id)}
          {#each c.keys as _, i}
            {@const k = keyPos(c, i)}
            {@const cx = xOf(k.at)}
            {@const cy = yOf(k.value)}
            {@const isSel = selected?.ch === c.id && selected.index === i}
            {#if cx >= M.l - 6 && cx <= M.l + pw + 6}
              <g
                class="key"
                class:sel={isSel}
                role="button"
                tabindex="-1"
                aria-label="{c.name} keyframe at {timecode(k.at)}"
                onpointerdown={(e) => keyDown(e, c, i)}
                ondblclick={(e) => (e.stopPropagation(), (selected = null), run(api.keyDelete(ins.id, c.id, i)))}
                oncontextmenu={(e) => (e.preventDefault(), e.stopPropagation(), keyMenu(e, c, i))}
              >
                {#if c.keys[i].interp === "Hold"}
                  <rect x={cx - 5} y={cy - 5} width="10" height="10" stroke={isSel ? "var(--text)" : colorOf(c.id)} fill={colorOf(c.id)} />
                {:else if c.keys[i].interp === "Bezier"}
                  <circle {cx} {cy} r="5.5" stroke={isSel ? "var(--text)" : colorOf(c.id)} fill={colorOf(c.id)} />
                {:else}
                  <path d="M{cx},{cy - 6.5} L{cx + 6.5},{cy} L{cx},{cy + 6.5} L{cx - 6.5},{cy}Z" stroke={isSel ? "var(--text)" : colorOf(c.id)} fill={c.keys[i].interp === "Ease" ? "var(--well)" : colorOf(c.id)} />
                {/if}
              </g>
            {/if}
          {/each}
        {/each}
        {#if selected}
          {@const sc = shown.find((x) => x.id === selected!.ch)}
          {#if sc && sc.keys[selected.index]}
            {#each handlesOf(sc, selected.index) as h (h.out)}
              <line x1={h.kx} y1={h.ky} x2={h.x} y2={h.y} class="arm" stroke={colorOf(sc.id)} />
              <circle
                cx={h.x}
                cy={h.y}
                r="4.5"
                class="handle"
                stroke={colorOf(sc.id)}
                role="button"
                tabindex="-1"
                aria-label="{h.out ? 'Out' : 'In'} handle"
                onpointerdown={(e) => handleDown(e, sc, selected!.index, h)}
              />
            {/each}
          {/if}
        {/if}
        {#if playX >= M.l && playX <= M.l + pw}
          <line x1={playX} x2={playX} y1={4} y2={H - M.b} class="playhead" />
        {/if}
        <text x={M.l + 4} y={14} class="hint">
          {#if sel}
            {#if hdrag}
              Drag the handle: further out holds the speed longer, up or down speeds up or overshoots · Shift: level (pure ease){sel.c.unit === " px" && /Position|Scale|Anchor/.test(sel.c.name) ? " · X and Y share the timing" : ""}
            {:else}
              {sel.c.name}: {fmt(sel.k.value, sel.c.unit)} at {timecode(sel.k.at)} · {sel.interp === "Bezier" ? "Bézier" : sel.interp}  — drag to move (Shift: value only), right-click for easing{sel.interp === "Bezier" || (selected && selected.index > 0 && sel.c.keys[selected.index - 1]?.interp === "Bezier") ? ", drag the round handles to shape it" : ""}
            {/if}
          {:else if shown.length === 0}
            Tick channels on the left to see their curves
          {:else}
            Drag keys to move them · double-click a line to add a key · double-click a key to delete it · right-click a key for easing
          {/if}
        </text>
      </svg>
    </div>
  </div>
{/if}

<style>
  .empty {
    padding: 24px;
    text-align: center;
  }
  .empty p {
    margin: 6px auto;
    max-width: 520px;
  }
  .muted {
    color: var(--muted);
  }
  .curves {
    flex: 1;
    min-height: 0;
    display: flex;
    border-top: 1px solid var(--line-soft);
  }
  .list {
    width: 210px;
    flex: none;
    overflow: auto;
    border-right: 1px solid var(--line-soft);
    padding: 4px 0 8px;
  }
  .listhead {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 2px 8px 4px;
  }
  .listhead .muted {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: calc(12px * var(--text-scale));
  }
  .tiny {
    min-height: 20px;
    padding: 0 6px;
    font-size: calc(11.5px * var(--text-scale));
  }
  .group {
    font-size: calc(11px * var(--text-scale));
    color: var(--muted);
    text-transform: uppercase;
    letter-spacing: 0.04em;
    padding: 8px 8px 2px;
  }
  .ch {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 2px 8px;
    color: var(--faint);
    font-size: calc(12.5px * var(--text-scale));
    cursor: pointer;
  }
  .ch:hover {
    background: var(--raised);
  }
  .ch.keyed {
    color: var(--muted);
  }
  .ch.on {
    color: var(--text);
  }
  .ch input {
    display: none;
  }
  .dot {
    width: 9px;
    height: 9px;
    border-radius: 2px;
    border: 1.5px solid var(--c);
    flex: none;
  }
  .ch.on .dot {
    background: var(--c);
  }
  .cname {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .count {
    font-size: calc(11px * var(--text-scale));
    color: var(--accent);
  }
  .graph {
    flex: 1;
    min-width: 0;
    position: relative;
    background: var(--well);
  }
  svg {
    position: absolute;
    inset: 0;
    touch-action: none;
    user-select: none;
  }
  .clip {
    fill: color-mix(in srgb, var(--text) 4%, transparent);
  }
  .grid {
    stroke: var(--line-soft);
    stroke-width: 1;
  }
  .grid.faint {
    opacity: 0.6;
  }
  .frame {
    fill: none;
    stroke: var(--line);
  }
  .vlabel {
    fill: var(--faint);
    font-size: 10px;
    text-anchor: end;
    font-family: var(--font-tc);
  }
  .tlabel {
    fill: var(--faint);
    font-size: 10px;
    font-family: var(--font-tc);
  }
  .line {
    fill: none;
    stroke-width: 1.8;
    pointer-events: none;
  }
  .hit {
    fill: none;
    stroke: transparent;
    stroke-width: 12;
    cursor: copy;
  }
  .key {
    cursor: grab;
  }
  .key rect,
  .key path {
    stroke-width: 1.6;
  }
  .key.sel rect,
  .key.sel path {
    stroke-width: 2.4;
  }
  .arm {
    stroke-width: 1.2;
    opacity: 0.8;
    pointer-events: none;
  }
  .handle {
    fill: var(--well);
    stroke-width: 1.8;
    cursor: grab;
  }
  .key circle {
    stroke-width: 1.6;
  }
  .key.sel circle {
    stroke-width: 2.4;
  }
  .playhead {
    stroke: var(--red);
    stroke-width: 1.5;
    pointer-events: none;
  }
  .hint {
    fill: var(--muted);
    font-size: 11px;
    pointer-events: none;
  }
</style>
