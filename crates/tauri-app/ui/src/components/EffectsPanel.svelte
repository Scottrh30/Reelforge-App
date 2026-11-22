<script lang="ts">
  // The Effects section of the Controls panel: the clip's effect stack, top
  // of the list applied first. Add, reorder, switch off, edit and keyframe.
  import { untrack } from "svelte";
  import { app, run, say } from "../lib/session.svelte";
  import { api, type FxPreset, type Inspector, type CatalogGroup, type Curves, type HueCurves, type EffectView, type ParamView } from "../lib/api";
  import { livePump } from "../lib/live";
  import { openMenu, type MenuItem } from "../lib/menu.svelte";
  import { actions } from "../lib/actions.svelte";
  import NumberField from "./NumberField.svelte";
  import CurvesEditor from "./CurvesEditor.svelte";
  import HueCurvesEditor from "./HueCurvesEditor.svelte";
  import ColorWheels from "./ColorWheels.svelte";

  // `scene`: the settings of a camera or light clip (one fixed card, no adding).
  let { ins, scene = false }: { ins: Inspector; scene?: boolean } = $props();
  const pump = livePump();

  // Values being dragged, so the numbers keep up with the pointer.
  let drafts = $state<Record<string, number>>({});
  // Effects folded shut (by position), per clip.
  let folded = $state<Record<string, boolean>>({});
  // Colour wheels whose numbers (for typing and keyframing) are shown.
  let numbers = $state<Record<string, boolean>>({});
  let shownId: number | null = null;
  $effect(() => {
    const id = ins.id;
    untrack(() => {
      if (id !== shownId) {
        shownId = id;
        drafts = {};
      }
    });
  });

  const value = (i: number, p: number, pv: ParamView) => drafts[`${i}:${p}`] ?? pv.value;
  function set(i: number, p: number, v: number) {
    drafts[`${i}:${p}`] = v;
    const id = ins.id;
    pump.push(() => api.effectParam(id, i, p, v));
  }
  /// Several at once (one live edit, so none is dropped by the pump).
  function setMany(i: number, vs: [number, number][]) {
    for (const [p, v] of vs) drafts[`${i}:${p}`] = v;
    const id = ins.id;
    pump.push(() => api.effectParams(id, i, vs));
  }
  async function commit() {
    await pump.done();
    await run(api.commit());
    drafts = {};
  }
  async function setNow(i: number, p: number, v: number) {
    set(i, p, v);
    await commit();
  }
  function curves(i: number, c: Curves, done: boolean) {
    const id = ins.id;
    pump.push(() => api.effectCurves(id, i, c));
    if (done) commit();
  }
  function hueCurves(i: number, c: HueCurves, done: boolean) {
    const id = ins.id;
    pump.push(() => api.effectHueCurves(id, i, c));
    if (done) commit();
  }

  const op = (i: number, o: "up" | "down" | "remove" | "toggle" | "duplicate" | "reset") => run(api.effectOp(ins.id, i, o));

  function more(e: MouseEvent, i: number, fx: EffectView) {
    const items: MenuItem[] = [
      { label: "Duplicate", run: () => op(i, "duplicate") },
      { label: "Reset to defaults", disabled: fx.missing, run: () => op(i, "reset") },
      { separator: true },
      { label: "Move up", disabled: i === 0, run: () => op(i, "up") },
      { label: "Move down", disabled: i === ins.effects.length - 1, run: () => op(i, "down") },
      { separator: true },
      { label: "Remove", run: () => op(i, "remove") },
    ];
    openMenu(e, items);
  }

  // The "Add" menu: every effect by category (plugins included).
  let catalog = $state<CatalogGroup[]>([]);
  async function add(e: MouseEvent) {
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    catalog = await api.effectCatalog();
    const items: MenuItem[] = catalog.map((g) => ({
      label: g.label,
      submenu: g.items.map((it) => ({ label: it.plugin ? `${it.label}  ·  plugin` : it.label, run: () => run(api.effectAdd(ins.id, it.kind, it.plugin)) })),
    }));
    const presets = await api.fxPresetList();
    items.push(
      { separator: true },
      {
        label: "Presets",
        submenu: presetMenu(presets.filter((p) => p.parts.includes("look"))),
      },
      { label: "Save as preset…", run: savePreset },
    );
    items.push({ separator: true }, { label: "Browse effects…", hint: "E", run: () => (app.effectsOpen = true) }, { label: "Manage plugins…", run: () => (app.pluginsOpen = true) });
    openMenu({ clientX: r.left, clientY: r.bottom + 2, preventDefault() {}, stopPropagation() {} } as MouseEvent, items);
  }

  // Save the whole stack (settings and keyframes) under a name, for the
  // effects browser's Presets tab and the Add menu's Presets.
  // Presets for the Add menu: favourites first, then a submenu per folder.
  function presetMenu(list: FxPreset[]): MenuItem[] {
    const one = (p: FxPreset): MenuItem => ({ label: p.favourite ? `★ ${p.name}` : p.name, run: () => run(api.fxPresetApply(ins.id, p.name, false)) });
    const favs = list.filter((p) => p.favourite).map(one);
    const folders = [...new Set(list.map((p) => (p.builtin ? p.folder || "Starter looks" : p.folder)).filter(Boolean))];
    const loose = list.filter((p) => !p.builtin && !p.folder).map(one);
    const out: MenuItem[] = [...favs];
    if (favs.length && (loose.length || folders.length)) out.push({ separator: true });
    out.push(...loose);
    for (const f of folders) out.push({ label: f, submenu: list.filter((p) => (p.builtin ? p.folder || "Starter looks" : p.folder) === f).map(one) });
    return out.length ? out : [{ label: "None saved yet", disabled: true, run: () => {} }];
  }
  function savePreset() {
    app.savePreset = { clip: ins.id };
  }

  // Colours: 0xRRGGBB numbers ↔ "#rrggbb".
  const hex = (v: number) => "#" + Math.round(v).toString(16).padStart(6, "0").slice(-6);
  const unhex = (h: string) => parseInt(h.replace("#", ""), 16) || 0;
  function typedHex(i: number, pi: number, el: HTMLInputElement, pv: ParamView) {
    const m = /^#?([0-9a-f]{3}|[0-9a-f]{6})$/i.exec(el.value.trim());
    if (!m) {
      el.value = hex(value(i, pi, pv));
      return;
    }
    const h = m[1].length === 3 ? [...m[1]].map((c) => c + c).join("") : m[1];
    setNow(i, pi, unhex(h));
  }

  // Energy blade tools.
  /// The same order as core particles::PRESETS.
  const PARTICLE_PRESETS = ["Sparks", "Embers", "Fire", "Smoke", "Magic", "Snow", "Dust", "Confetti", "Rain", "Fog", "Debris", "Atomic", "Disintegrate", "Water spray"];
  const BLADE_COLOURS: [string, string][] = [
    ["Blue", "#1f6bff"],
    ["Green", "#22e04a"],
    ["Red", "#ff1a12"],
    ["Purple", "#9a3dff"],
    ["Yellow", "#ffc21a"],
    ["Orange", "#ff6a10"],
    ["Cyan", "#18e0ff"],
    ["White", "#d8e4ff"],
  ];
  let igniteSecs = $state(0.3);

  const keyTip = (p: ParamView) => (p.key_here ? "Remove the keyframe here" : p.animated ? "Add a keyframe at the playhead" : "Animate: add a keyframe at the playhead");
  const fileName = (path: string) => path.split(/[\\/]/).pop() ?? path;
  /// (Face beauty's list of faces.)
  /// Object removal: the roto brush in one-click mode, making an Object to remove mask.
  function pickToRemove() {
    const id = app.s?.inspector?.id;
    if (id == null) return;
    app.drawing = null;
    app.paint = null;
    app.roto = { clip: id, mask: null, size: app.roto?.size ?? 14, pick: true, removal: true };
    api.rotoPrepare(id).catch(() => {});
    say("Click what to remove in the Viewer: it's picked out and followed through the clip (Esc stops)");
  }
</script>

<div class="group">
  {#if scene}
    <div class="head">
      <h4>{ins.kind === "camera" ? "Camera" : ins.kind === "point" ? "Point" : "Light"}</h4>
      <button class="small" onclick={() => op(0, "reset")} title="Back to the starting settings (removes their keyframes)">Reset</button>
    </div>
    {#if ins.kind === "camera"}
      <p class="note">The view of every 3D layer (tick 3D layer in a clip's Transform). The topmost camera on the timeline is used. Drag its point of interest in the Viewer.</p>
    {:else}
      <p class="note">Shades 3D layers that are lit by lights. With no lights, 3D layers show as they are. Drag its position or point of interest in the Viewer.</p>
    {/if}
  {:else}
  <div class="head">
    <h4>Effects</h4>
    <div class="adds">
      <button class="small" onclick={add} title="Add an effect to this clip">+ Add</button>
      <button class="small" onclick={() => (app.effectsOpen = true)} title="Search and browse every effect (E)">Browse…</button>
    </div>
  </div>
  {#if ins.effects.length === 0}
    <p class="note">None. Effects change the clip's picture before its transform; the top one is applied first.</p>
  {/if}
  {/if}
  {#each ins.effects as fx, i (i + ":" + fx.label)}
    {@const key = `${ins.id}:${i}`}
    <div class="card" class:off={!fx.enabled}>
      <div class="row">
        <input type="checkbox" checked={fx.enabled} aria-label="{fx.label} on" title="Turn the effect on or off" onchange={() => op(i, "toggle")} />
        <button class="fold" aria-expanded={!folded[key]} onclick={() => (folded[key] = !folded[key])} title={folded[key] ? "Show the settings" : "Hide the settings"}>
          <span class="chev" class:shut={folded[key]} aria-hidden="true">▾</span>
          <span class="name">{fx.label}</span>
          {#if fx.kind === "Plugin"}<span class="tag">plugin</span>{/if}
          {#if fx.kind === "OpenFx"}<span class="tag" title="An OpenFX plugin: runs on the picture before the other effects">OpenFX</span>{/if}
          {#if fx.animated}<span class="tag keyed" title="Some settings are keyframed">◆</span>{/if}
        </button>
        {#if !scene}
        <button class="ghost icon" disabled={i === 0} onclick={() => op(i, "up")} title="Move up (applied earlier)" aria-label="Move {fx.label} up">
          <svg viewBox="0 0 12 12" width="12" height="12"><path d="M2.5 7.5 6 4l3.5 3.5" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" /></svg>
        </button>
        <button class="ghost icon" disabled={i === ins.effects.length - 1} onclick={() => op(i, "down")} title="Move down (applied later)" aria-label="Move {fx.label} down">
          <svg viewBox="0 0 12 12" width="12" height="12"><path d="M2.5 4.5 6 8l3.5-3.5" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" /></svg>
        </button>
        <button class="ghost icon" onclick={(e) => more(e, i, fx)} title="More: duplicate, reset, remove" aria-label="More for {fx.label}">⋯</button>
        {/if}
      </div>
      {#if fx.error}
        <p class="warn" title={fx.error}>Not rendering: {fx.error}</p>
      {/if}
      {#if fx.kind === "OpenFx" && (fx.temporal || fx.gpu.length || fx.points.length || fx.rendered_with) && !folded[key]}
        <p class="note">{[fx.rendered_with ? `Rendering with ${fx.rendered_with}` : fx.gpu.length ? `Can use the graphics card (${fx.gpu.join(", ")})` : "", fx.temporal && "reads the clip's neighbouring frames", fx.points.length && `drag ${fx.points.length === 1 ? "its point" : "its points"} in the Viewer`].filter(Boolean).join(" · ")}</p>
      {/if}
      {#if fx.missing}
        <p class="warn">This plugin isn't installed (id “{fx.plugin}”), so the effect is skipped. Its settings are kept: install the plugin and it comes back.</p>
      {/if}
      {#if !folded[key]}
        {#if fx.kind === "ColorWheels"}
          <ColorWheels values={fx.params.map((p, pi) => value(i, pi, p))} onset={(vs) => setMany(i, vs)} oncommit={commit} />
          <button class="ghost small numbers" aria-expanded={!!numbers[key]} onclick={() => (numbers[key] = !numbers[key])} title="Every setting as a number, to type or keyframe">
            <span class="chev" class:shut={!numbers[key]} aria-hidden="true">▾</span> Numbers and keyframes
          </button>
        {/if}
        {#if fx.params.length && (fx.kind !== "ColorWheels" || numbers[key])}
          <div class="params">
            {#each fx.params as p, pi}
              {#if p.section}<span class="sec">{p.section}</span>{/if}
              <span class="lbl" title={p.name}>{p.name}</span>
              <button class="key" class:here={p.key_here} class:anim={p.animated} onclick={() => run(api.effectKey(ins.id, i, pi))} title={keyTip(p)} aria-label="{keyTip(p)}: {p.name}">
                <svg viewBox="0 0 10 10" width="10" height="10"><path d="M5 1 9 5 5 9 1 5Z" /></svg>
              </button>
              <div class="ctl">
                {#if p.ui === "toggle"}
                  <input type="checkbox" checked={value(i, pi, p) >= 0.5} aria-label={p.name} onchange={(e) => setNow(i, pi, e.currentTarget.checked ? 1 : 0)} />
                {:else if p.ui === "color"}
                  <input type="color" class="pick" value={hex(value(i, pi, p))} aria-label="{p.name}" oninput={(e) => set(i, pi, unhex(e.currentTarget.value))} onchange={commit} />
                  <input type="text" class="hex" value={hex(value(i, pi, p))} spellcheck="false" aria-label="{p.name} hex code" onblur={(e) => typedHex(i, pi, e.currentTarget, p)} onkeydown={(e) => e.key === "Enter" && typedHex(i, pi, e.currentTarget, p)} />
                {:else if p.ui === "choice"}
                  <select value={Math.round(value(i, pi, p))} aria-label={p.name} onchange={(e) => setNow(i, pi, +e.currentTarget.value)}>
                    {#each p.options as o, oi}<option value={oi}>{o}</option>{/each}
                  </select>
                {:else}
                  <NumberField label={p.name} value={value(i, pi, p)} min={p.min} max={p.max} step={p.step} digits={p.decimals} suffix={p.suffix} oninput={(v) => set(i, pi, v)} oncommit={commit} />
                  {#if (fx.kind === "ChromaKey" || fx.kind === "KeyCleaner") && pi === 0}
                    <span class="swatch" style="background:hsl({value(i, pi, p)} 100% 50%)" title="The colour being keyed out"></span>
                  {/if}
                {/if}
                {#if p.ui === "number" && value(i, pi, p) !== p.default}
                  <button class="ghost tiny" onclick={() => setNow(i, pi, p.default)} title="Back to {p.default}{p.suffix}" aria-label="Reset {p.name}">↺</button>
                {/if}
              </div>
            {/each}
          </div>
        {/if}
        {#if fx.kind === "EnergyBlade"}
          <div class="blade">
            <div class="presets" role="group" aria-label="Blade colour presets">
              {#each BLADE_COLOURS as [name, c], k}
                <button class="dot" style="--c:{c}" title="{name} blade" aria-label="{name} blade" onclick={() => run(api.bladePreset(ins.id, i, k))}></button>
              {/each}
            </div>
            <div class="row ignite">
              <button class="small" onclick={() => run(api.bladeIgnite(ins.id, i, false, igniteSecs))} title="Keyframe the length from 0 to 100% starting at the playhead">Ignite here</button>
              <button class="small" onclick={() => run(api.bladeIgnite(ins.id, i, true, igniteSecs))} title="Keyframe the length from 100 to 0% starting at the playhead">Retract here</button>
              <NumberField label="Ignition time" value={igniteSecs} min={0.02} max={10} step={0.01} digits={2} suffix=" s" oninput={(v) => (igniteSecs = v)} oncommit={() => {}} />
            </div>
            <p class="note">Drag the hilt (H) and tip in the Viewer, or drag the blade to move it. To make it follow a prop, track the prop in the Track panel and apply the track to this blade.</p>
          </div>
        {/if}
        {#if fx.kind === "Particles"}
          <div class="blade">
            <div class="row presetrow" role="group" aria-label="Particle presets">
              {#each PARTICLE_PRESETS as name, k}
                <button class="small" title="Make these {name.toLowerCase()} (keeps the emitter's position)" onclick={() => run(api.particlesPreset(ins.id, i, k))}>{name}</button>
              {/each}
            </div>
            <p class="note">Drag the emitter (✦) in the Viewer, and its arrow to aim it. Every setting can be keyframed: keyframe Birth rate up and down for a burst, or move the emitter to leave a trail. Particles start at the clip's start, so trim its start to begin with them already in the air.</p>
          </div>
        {/if}
        {#if fx.kind === "Curves" && fx.curves}
          <CurvesEditor curves={fx.curves} onchange={(c, done) => curves(i, c, done)} />
        {/if}
        {#if fx.kind === "HueCurves" && fx.hue_curves}
          <HueCurvesEditor curves={fx.hue_curves} onchange={(c, done) => hueCurves(i, c, done)} />
        {/if}
        {#if fx.kind === "Lut"}
          <div class="row lut">
            <span class="file" title={fx.lut ?? ""}>{fx.lut ? fileName(fx.lut) : "No LUT chosen"}</span>
            <button class="small" onclick={() => run(api.effectLut(ins.id, i))}>Choose .cube…</button>
          </div>
          {#if fx.lut_error}<p class="warn">{fx.lut_error}</p>{/if}
        {/if}
        {#if fx.kind === "HslQualifier"}
          {@const picking = app.keyPick?.clip === ins.id && app.keyPick.index === i}
          <div class="row">
            <button class="small" class:active={picking} aria-pressed={picking} onclick={() => {
              if (picking) {
                app.keyPick = null;
                app.note = "";
              } else {
                app.keyPick = { clip: ins.id, index: i };
                app.drawing = null;
                app.maskTool = null;
                app.note = "Click the colour to grade in the Viewer, or drag a box over it to average a patch (Esc cancels)";
              }
            }} title="Choose the colour range from the picture in the Viewer: click, or drag a box to average a patch">{picking ? "Picking… (Esc)" : "Pick colour"}</button>
          </div>
        {/if}
        {#if fx.kind === "ChromaKey"}
          {@const picking = app.keyPick?.clip === ins.id && app.keyPick.index === i}
          <div class="row">
            <button class="small" class:active={picking} aria-pressed={picking} onclick={() => {
              if (picking) {
                app.keyPick = null;
                app.note = "";
              } else {
                app.keyPick = { clip: ins.id, index: i };
                app.drawing = null;
                app.maskTool = null;
                app.note = "Click the screen in the Viewer to key its colour, or drag a box over it to average a patch (Esc cancels)";
              }
            }} title="Pick the key colour from the picture in the Viewer: click, or drag a box to average a patch">{picking ? "Picking… (Esc)" : "Pick key colour"}</button>
          </div>
        {/if}
        {#if fx.kind === "ChromaKey" || fx.kind === "LumaKey" || fx.kind === "DifferenceKey"}
          <div class="row">
            <button class="small" onclick={async () => {
              const m = await run(api.maskFromKey(ins.id, i));
              if (typeof m === "number") app.activeMask = { clip: ins.id, index: m };
            }} title="Trace what this key keeps at the playhead (its biggest area) into a new mask to edit, track, or use as a garbage matte">Mask from this key</button>
          </div>
        {/if}
        {#if ins.kind === "light" && i === 0 && Math.round(fx.params[0]?.value ?? 0) === 4}
          <div class="row lut">
            <span class="file" title={fx.environment ?? ""}>{fx.environment ? `Panorama: ${fileName(fx.environment)}` : "No panorama yet"}</span>
            <button class="small" onclick={() => run(api.lightEnvironment(ins.id, i))} title="An equirectangular (2:1) panorama, best an HDR (.hdr or .exr) from a light probe or an HDRI site">Choose…</button>
          </div>
          {#if fx.environment_error}<p class="warn">{fx.environment_error}</p>{:else if !fx.environment}<p class="note">An environment light lights 3D layers from a panorama all round the scene, and shiny 3D text, shapes and models reflect it (Reflections, under Environment). Choose an .hdr or .exr panorama.</p>{/if}
        {/if}
        {#if fx.kind === "Paint"}
          {@const on = app.paint?.clip === ins.id && app.paint.index === i}
          <div class="row lut">
            <span class="file">{fx.strokes} stroke{fx.strokes === 1 ? "" : "s"}</span>
            <button class="small" class:on onclick={() => (app.paint = on ? null : { clip: ins.id, index: i, source: null })} title="Paint in the Viewer with this effect's tool (Esc stops)">{on ? "Stop painting" : "Paint in Viewer"}</button>
            <button class="small" disabled={!fx.strokes} onclick={() => run(api.paintClear(ins.id, i))} title="Take every stroke off (undo brings them back)">Clear</button>
          </div>
          {#if on}
            <p class="note">Drag in the Viewer to paint. For <b>Clone</b>, Alt-click where to copy from first; the next stroke copies from there, keeping the same distance for later strokes. Undo takes the last stroke away.</p>
          {/if}
        {/if}
        {#if fx.kind === "DepthMap" || fx.kind === "Relight"}
          {@const busy = app.live?.tracking && app.live.tracking[0] === ins.id ? app.live.tracking[1] : null}
          <div class="row lut">
            <span class="file">{busy !== null ? `Working out depth… ${Math.round(busy * 100)}%` : fx.depth_frames !== null ? `Depth worked out for ${fx.depth_frames} frames` : "Depth not worked out yet"}</span>
            {#if busy !== null}
              <button class="small" onclick={() => run(api.trackCancel())}>Stop</button>
            {:else}
              <button class="small" disabled={!!app.live?.tracking || ins.kind !== "video"} onclick={() => run(api.findDepth(ins.id, i))} title="Work out how far away everything is in every frame, with the depth model (in the background)">{fx.depth_frames === null ? "Work out depth" : "Work out again"}</button>
            {/if}
          </div>
          {#if fx.depth_frames === null && busy === null}
            <p class="note">Press <b>Work out depth</b>: the depth model looks at every frame (a second or two each, in the background) and works out how near each part of it is.{fx.kind === "Relight" ? " Then drag the light in the Viewer." : ""}</p>
          {/if}
        {/if}
        {#if fx.kind === "Liquify"}
          {@const on = app.paint?.clip === ins.id && app.paint.index === i}
          <div class="row lut">
            <span class="file">{fx.strokes} stroke{fx.strokes === 1 ? "" : "s"}</span>
            <button class="small" class:on onclick={() => (app.paint = on ? null : { clip: ins.id, index: i, source: null })} title="Brush the picture in the Viewer with this effect's tool (Esc stops)">{on ? "Stop" : "Liquify in Viewer"}</button>
            <button class="small" disabled={!fx.strokes} onclick={() => run(api.paintClear(ins.id, i))} title="Take every stroke off (undo brings them back)">Reset all</button>
          </div>
          {#if on}
            <p class="note">Drag in the Viewer to <b>Push</b> the picture along; hold the brush still to keep twirling, pinching or swelling. <b>Reconstruct</b> brushes earlier strokes away. Undo takes the last stroke away; keyframe <b>Amount</b> to bring the warp in or out.</p>
          {/if}
        {/if}
        {#if fx.kind === "FaceBeauty"}
          {@const busy = app.live?.tracking && app.live.tracking[0] === ins.id ? app.live.tracking[1] : null}
          <div class="row lut">
            <span class="file">{busy !== null ? `Finding faces… ${Math.round(busy * 100)}%` : fx.faces === null ? "Faces not looked for yet" : fx.faces.length === 0 ? "No faces found" : `${fx.faces.length} face${fx.faces.length === 1 ? "" : "s"} found`}</span>
            {#if busy !== null}
              <button class="small" onclick={() => run(api.trackCancel())}>Stop</button>
            {:else}
              <button class="small" disabled={!!app.live?.tracking || ins.kind !== "video"} onclick={() => run(api.findFaces(ins.id, i))} title="Find the faces in every frame and follow them through the clip (in the background)">{fx.faces === null ? "Find faces" : "Find again"}</button>
            {/if}
          </div>
          {#if fx.faces?.length}
            <div class="faces">
              {#each fx.faces as f, k}
                <label class="check" title="Seen in {f.frames} frames"><input type="checkbox" checked={f.on} onchange={(e) => run(api.faceOn(ins.id, i, k, e.currentTarget.checked))} /> Face {k + 1} <span class="muted">({f.frames} frames, at {Math.round(f.rect[0] * 100)}%, {Math.round(f.rect[1] * 100)}%)</span></label>
              {/each}
            </div>
            <p class="note">Tick <b>Show faces</b> to see which is which in the Viewer.</p>
          {:else if fx.faces === null && busy === null}
            <p class="note">Press <b>Find faces</b>: faces are found in every frame and followed through the clip. Or set <b>Where</b> to <b>All skin in the picture</b> to smooth skin without looking for faces.</p>
          {/if}
        {/if}
        {#if fx.kind === "SkyReplace"}
          {@const busy = app.live?.tracking && app.live.tracking[0] === ins.id ? app.live.tracking[1] : null}
          <div class="row lut">
            <span class="file" title={fx.sky_image ?? ""}>{fx.sky_image ? fx.sky_image.split(/[\\/]/).pop() : "No picture: a gradient sky"}</span>
            <button class="small" onclick={() => run(api.skyImage(ins.id, i))} title="Choose a picture of a sky">Choose sky…</button>
          </div>
          <div class="row lut">
            <span class="file">{busy !== null ? `Finding the sky… ${Math.round(busy * 100)}%` : fx.sky_frames !== null ? `Sky found in ${fx.sky_frames} frames` : "The sky not found yet"}</span>
            {#if busy !== null}
              <button class="small" onclick={() => run(api.trackCancel())}>Stop</button>
            {:else}
              <button class="small" disabled={!!app.live?.tracking || ins.kind !== "video"} onclick={() => run(api.findSky(ins.id, i))} title="Find the sky in every frame, and how the camera moves (in the background)">{fx.sky_frames === null ? "Find the sky" : "Find again"}</button>
            {/if}
          </div>
          {#if fx.sky_frames === null && busy === null}
            <p class="note">Press <b>Find the sky</b>: the object model picks out the sky in every other frame (it looks for sky along the top of the first frame), its edge is fitted to the trees and roofs, and the camera's movement is worked out so the new sky moves with it. Then choose a sky picture, or use the gradient.</p>
          {/if}
        {/if}
        {#if fx.kind === "Fluid" || fx.kind === "WaterSurface" || fx.kind === "Liquid" || fx.kind === "Liquid3D"}
          {@const busy = app.live?.simulating ? app.live.render : null}
          <div class="row lut">
            <span class="file">{busy !== null ? `Simulating… ${Math.round(busy * 100)}%` : "Worked out as it plays, or all at once"}</span>
            {#if busy !== null}
              <button class="small" onclick={() => actions.act("stop_render_preview")} title="Stop (what's done so far is kept)">Stop</button>
            {:else}
              <button class="small" disabled={app.live?.render != null} onclick={() => actions.act("simulate_now")} title="Work the simulation out now, start to end, and keep it (on disk too, with the rendered previews), so playing, scrubbing and exporting are quick, also when the project is opened again">Simulate now</button>
            {/if}
          </div>
        {/if}
        {#if fx.kind === "ObjectRemoval"}
          {@const areas = ins.masks.filter((m) => m.mask.enabled && m.mask.fill_area).length}
          {@const busy = app.live?.tracking && app.live.tracking[0] === ins.id ? app.live.tracking[1] : null}
          <div class="row lut">
            <span class="file">{busy !== null ? `Filling… ${Math.round(busy * 100)}%` : fx.fill_preview ? "Preview: this frame only" : fx.filled !== null ? `${fx.filled} frames filled` : "Not worked out yet"}</span>
            {#if busy !== null}
              <button class="small" onclick={() => run(api.trackCancel())} title="Stop (what's done so far is dropped)">Stop</button>
            {:else}
              <button class="small" disabled={!areas || !!app.live?.tracking} onclick={() => run(api.objectRemoval(ins.id, i, true))} title="Fill just the frame at the playhead (from the frames round it), to try the method and settings quickly">Preview frame</button>
              <button class="small" disabled={!areas || !!app.live?.tracking} onclick={() => run(api.objectRemoval(ins.id, i))} title={areas ? "Fill every frame of the clip where the Object to remove masks are (in the background)" : "Draw a mask round what to remove first, and set its Use to Object to remove"}>{fx.filled !== null && !fx.fill_preview ? "Fill again" : "Remove object"}</button>
            {/if}
          </div>
          {#if busy === null && ins.kind === "video"}
            <div class="row lut">
              <button class="small" class:on={app.roto?.clip === ins.id && !!app.roto?.pick && !!app.roto?.removal} disabled={!!app.live?.tracking} onclick={pickToRemove} title="Click what to remove in the Viewer: the roto brush picks it out as an Object to remove mask and carries it through the clip">Pick what to remove…</button>
            </div>
          {/if}
          {#if !areas}
            <p class="note">Press <b>Pick what to remove</b> and click it in the Viewer (the roto brush follows it through the clip), or draw a mask round it (Masks panel) and set its <b>Use</b> to <b>Object to remove</b>. Then press Remove object; raise Grow if its edges show.</p>
          {:else if fx.filled === null && busy === null}
            <p class="note">Press Remove object to fill {areas === 1 ? "the masked area" : "the masked areas"} in every frame. Object fills from the other frames where what's behind shows (best when the camera or the object moves), making up the rest; Surface makes it up once and carries it with the camera; Edge blend fills smoothly from the edges.</p>
          {:else if fx.filled !== null && busy === null}
            <p class="note">After changing the masks, the method or Grow and Feather, press Fill again.</p>
          {/if}
        {/if}
        {#if fx.kind === "DifferenceKey"}
          <div class="row lut">
            <span class="file" title={fx.plate ? `${fx.plate[0]} at ${fx.plate[1]}` : ""}>{fx.plate ? `Plate: ${fileName(fx.plate[0])} · ${fx.plate[1]}` : "No clean plate yet"}</span>
          </div>
          <div class="row">
            <button class="small" onclick={() => run(api.effectPlate(ins.id, i, false))} title="Go to a frame where only the background is in shot (before the subject walks in), then press this">Use frame at playhead</button>
            <button class="small" onclick={() => run(api.effectPlate(ins.id, i, true))} title="A picture or video of the same shot with nothing in front">Choose file…</button>
          </div>
          {#if fx.plate_error}<p class="warn">{fx.plate_error}</p>{:else if !fx.plate}<p class="note">The clean plate is the shot with only the background in it. Go to such a frame (before the subject comes in) and press Use frame at playhead; the camera mustn't move. Then raise Tolerance until the background is gone (View → Matte shows it).</p>{/if}
        {/if}
      {/if}
    </div>
  {/each}
</div>

<style>
  .group {
    padding: 8px 0 12px;
    border-bottom: 1px solid var(--line-soft);
  }
  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 6px;
    margin-bottom: 8px;
  }
  h4 {
    font-size: calc(12.5px * var(--text-scale));
    font-weight: 600;
    color: var(--muted);
    margin: 0;
  }
  .adds {
    display: flex;
    gap: 4px;
  }
  .small {
    min-height: 22px;
    padding: 0 7px;
    font-size: calc(12px * var(--text-scale));
  }
  .note {
    color: var(--faint);
    font-size: calc(12px * var(--text-scale));
    margin: 2px 0 8px;
  }
  .warn {
    color: #f0a050;
    font-size: calc(12px * var(--text-scale));
    margin: 0;
  }
  .note {
    color: var(--muted);
    font-size: calc(11.5px * var(--text-scale));
    margin: 0;
  }
  .card {
    border: 1px solid var(--line-soft);
    border-radius: 5px;
    padding: 5px 8px 8px;
    margin-bottom: 6px;
    display: flex;
    flex-direction: column;
    gap: 6px;
    background: var(--base);
  }
  .card.off .params,
  .card.off .name {
    opacity: 0.5;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 4px;
  }
  .fold {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: center;
    gap: 5px;
    background: none;
    border-color: transparent;
    padding: 2px 4px;
    text-align: left;
  }
  .chev {
    color: var(--muted);
    transition: transform 0.12s;
    font-size: calc(11px * var(--text-scale));
  }
  .chev.shut {
    transform: rotate(-90deg);
  }
  .numbers {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    margin-bottom: 4px;
  }
  .name {
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .tag {
    font-size: calc(10.5px * var(--text-scale));
    color: var(--muted);
    border: 1px solid var(--line);
    border-radius: 3px;
    padding: 0 4px;
  }
  .tag.keyed {
    color: var(--accent);
    border-color: transparent;
  }
  .icon {
    min-height: 22px;
    min-width: 22px;
    padding: 0 4px;
    display: inline-grid;
    place-items: center;
  }
  .params {
    display: grid;
    grid-template-columns: minmax(60px, 0.9fr) 18px minmax(0, 1.4fr);
    gap: 5px 6px;
    align-items: center;
  }
  .lbl {
    color: var(--muted);
    font-size: calc(12px * var(--text-scale));
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .ctl {
    display: flex;
    align-items: center;
    gap: 4px;
    min-width: 0;
  }
  .ctl select {
    flex: 1;
    min-width: 0;
  }
  .key {
    width: 18px;
    height: 18px;
    min-height: 0;
    padding: 0;
    display: grid;
    place-items: center;
    background: none;
    border: 0;
  }
  .key path {
    fill: none;
    stroke: var(--faint);
    stroke-width: 1.2;
  }
  .key:hover path {
    stroke: var(--text);
  }
  .key.anim path {
    stroke: var(--accent);
  }
  .key.here path {
    fill: var(--accent);
    stroke: var(--accent);
  }
  .swatch {
    width: 14px;
    height: 14px;
    border-radius: 3px;
    flex: none;
  }
  .tiny {
    min-height: 20px;
    padding: 0 4px;
    color: var(--muted);
    flex: none;
  }
  .sec {
    grid-column: 1 / -1;
    font-size: calc(11px * var(--text-scale));
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--faint);
    padding-top: 6px;
    border-top: 1px solid var(--line-soft);
  }
  .sec:first-child {
    padding-top: 0;
    border-top: 0;
  }
  .pick {
    width: 34px;
    height: 22px;
    padding: 0;
    border: 1px solid var(--line);
    border-radius: var(--radius);
    background: none;
    flex: none;
  }
  .hex {
    flex: 1;
    min-width: 0;
    font-family: var(--mono, ui-monospace, monospace);
  }
  .blade {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding-top: 6px;
    border-top: 1px solid var(--line-soft);
  }
  .presetrow {
    flex-wrap: wrap;
    gap: 4px;
  }
  .presets {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .dot {
    width: 20px;
    height: 20px;
    min-height: 0;
    padding: 0;
    border-radius: 50%;
    border: 2px solid color-mix(in srgb, var(--c) 40%, white);
    background: radial-gradient(circle, white 0 22%, var(--c) 60%);
    box-shadow: 0 0 6px var(--c);
  }
  .dot:hover {
    transform: scale(1.12);
  }
  .ignite {
    flex-wrap: wrap;
  }
  .ignite :global(.num) {
    width: 76px;
  }
  .lut .file {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .faces {
    display: flex;
    flex-direction: column;
    gap: 2px;
    margin: 4px 0;
  }
</style>
