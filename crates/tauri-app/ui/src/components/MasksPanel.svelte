<script lang="ts">
  // The Masks section of the Controls panel.
  import { app, run, say } from "../lib/session.svelte";
  import { api, type Inspector, type Mask, type MaskMotion } from "../lib/api";
  import { livePump } from "../lib/live";
  import NumberField from "./NumberField.svelte";

  let { ins }: { ins: Inspector } = $props();

  const modes: Mask["mode"][] = ["Add", "Subtract", "Intersect"];
  const active = $derived(app.activeMask?.clip === ins.id ? app.activeMask.index : null);
  const drawing = $derived(app.drawing?.clip === ins.id);
  const pump = livePump();
  const hasKey = $derived(ins.effects.some((e) => ["ChromaKey", "LumaKey", "DifferenceKey"].includes(e.kind)));

  // Rotoscoping: track what's inside a mask and key its shape every frame.
  // Footage follows what's in it; an adjustment layer, solid, title or picture
  // follows the footage below it.
  const trackable = $derived(["video", "adjustment", "plane", "title", "image"].includes(ins.kind));
  const through = $derived(ins.kind !== "video");
  let motion = $state<MaskMotion>("Transform");
  let smoothFrames = $state(2);
  const motions: [MaskMotion, string][] = [
    ["Position", "Moves"],
    ["Transform", "Moves, turns, scales"],
    ["Perspective", "…and tilts (perspective)"],
    ["Bend", "Bends (cloth, skin, hands)"],
  ];
  const progress = $derived(app.live?.tracking && app.live.tracking[0] === ins.id ? app.live.tracking[1] : null);
  const elsewhere = $derived(!!app.live?.tracking && app.live.tracking[0] !== ins.id);

  const masks = (): Mask[] => ins.masks.map((m) => structuredClone($state.snapshot(m.mask)) as Mask);

  /// Change mask `i`; `done` makes it one undo step now, otherwise it's live until `commit`.
  function change(i: number, f: (m: Mask) => void, done: boolean) {
    const all = masks();
    f(all[i]);
    if (done) run(api.setMasks(ins.id, all, true));
    else pump.push(() => api.setMasks(ins.id, all, false));
  }
  async function commit() {
    await pump.done();
    await run(api.commit());
  }

  async function add(shape: "rectangle" | "ellipse") {
    app.drawing = null;
    const i = await run(api.addMask(ins.id, shape));
    if (i != null) app.activeMask = { clip: ins.id, index: i };
  }
  const roto = $derived(app.roto?.clip === ins.id ? app.roto : null);
  function rotoBrush(pick = false) {
    if (roto && !!roto.pick === pick && roto.mask === null) {
      app.roto = null;
      return;
    }
    app.drawing = null;
    app.maskTool = null;
    app.activeMask = null;
    app.roto = { clip: ins.id, mask: null, size: app.roto?.size ?? 14, pick, removal: app.roto?.removal ?? false };
    api.rotoPrepare(ins.id).catch(() => {});
    say(pick ? "Click the object in the Viewer: it's picked out and carried through the clip (Esc stops)" : "Paint over the subject in the Viewer; hold Alt to paint over what isn't it (Esc stops painting)");
  }
  function freehand() {
    if (drawing) {
      app.drawing = null;
      return;
    }
    app.activeMask = null;
    app.maskTool = null;
    app.drawing = { clip: ins.id, points: [] };
    say("Click in the Viewer to place the mask's points; click the first point or press Enter to close it (Esc cancels)");
  }
  /// Give point `pt` of mask `n` its own feather (`v` px), or (`null`) use the mask's again.
  function pointFeather(n: Mask, pt: number, count: number, v: number | null) {
    const f = Array.from({ length: count }, (_, k) => n.feathers?.[k] ?? -1);
    f[pt] = v == null ? -1 : Math.max(0, v);
    n.feathers = f.some((x) => x >= 0) ? f : [];
  }
  async function remove(i: number) {
    const all = masks();
    all.splice(i, 1);
    app.activeMask = null;
    await run(api.setMasks(ins.id, all, true));
  }

  // Replace background: what goes behind once it's cut away.
  let behind = $state("blur");
  let behindColor = $state("#1e7a3c");
  const pictures = $derived((app.s?.media ?? []).filter((m) => (m.kind === "video" || m.kind === "image") && !m.missing));
  function putBehind() {
    const hex = behindColor.replace("#", "");
    const what =
      behind === "blur"
        ? ("Blur" as const)
        : behind === "color"
          ? { Color: [parseInt(hex.slice(0, 2), 16), parseInt(hex.slice(2, 4), 16), parseInt(hex.slice(4, 6), 16)] as [number, number, number] }
          : { Media: Number(behind.slice(1)) };
    run(api.replaceBackground(ins.id, what));
  }
</script>

<div class="group">
  <div class="head">
    <h4>Masks</h4>
    <div class="adds">
      <button class="small" onclick={() => add("rectangle")} title="A rectangle in the middle of the layer">+ Rectangle</button>
      <button class="small" onclick={() => add("ellipse")} title="An ellipse in the middle of the layer">+ Ellipse</button>
      <button class="small" class:on={drawing} onclick={freehand} title="Click points in the Viewer; click the first point or press Enter to close">+ Freehand</button>
      <button class="small" class:on={!!roto} disabled={!trackable || ins.kind !== "video"} onclick={() => rotoBrush(false)} title={ins.kind === "video" ? "Paint over a subject in the Viewer to cut a mask round it, then carry it through the shot" : "The roto brush works on footage"}>+ Roto brush</button>
      <button class="small" class:on={!!roto?.pick} disabled={!trackable || ins.kind !== "video"} onclick={() => rotoBrush(true)} title={ins.kind === "video" ? "Click an object in the Viewer: it's picked out and carried through the rest of the clip by itself" : "Picking objects works on footage"}>+ Pick object</button>
      <button
        class="small"
        disabled={!trackable || ins.kind !== "video"}
        onclick={() => {
          app.roto = null;
          run(api.removeBackground(ins.id));
        }}
        title={ins.kind === "video" ? "Cut the background away through the whole clip: the main subject is found by itself (at the playhead), cut out with a soft edge, and followed through the clip" : "Background removal works on footage"}>Remove background</button>
    </div>
  </div>
  {#if ins.kind === "video" && ins.masks.some((m) => m.mask.name === "Background removal")}
    <!-- Then what goes behind: on the track below, over the same time. -->
    <div class="behind" role="group" aria-label="Replace the background">
      <span class="lbl">Behind it</span>
      <select bind:value={behind} aria-label="What goes behind it">
        <option value="blur">A blur of the shot</option>
        <option value="color">A colour</option>
        {#if pictures.length}
          <optgroup label="From the project">
            {#each pictures as m}<option value={"m" + m.id}>{m.name}</option>{/each}
          </optgroup>
        {/if}
      </select>
      {#if behind === "color"}
        <input type="color" bind:value={behindColor} aria-label="Background colour" />
      {/if}
      <button class="small" onclick={putBehind} title="Put it on the video track below this clip (a new track if that's taken), over the same stretch of time">Put it behind</button>
    </div>
  {/if}
  {#if drawing}
    <p class="note">Click in the Viewer to place points. Click the first point, double-click or press Enter to close; Esc cancels.</p>
  {/if}
  {#if roto?.pick}
    <p class="note">Click the object in the Viewer: it's picked out and carried through the rest of the clip by itself (from the last frame, back to the start). Then paint over any frame where it slipped to correct it, and carry on from there. Esc stops.</p>
  {:else if roto}
    <p class="note">Paint over the subject in the Viewer: the object model picks out what you paint over. Paint again to add what was missed; hold <b>Alt</b> to paint over what isn't it. Then carry it on through the shot (below). Esc stops painting.</p>
  {/if}
  {#if roto && roto.mask === null}
    <label class="check rm" title="The new mask doesn't cut the layer: it marks what the Object removal effect fills in"><input type="checkbox" checked={!!roto.removal} onchange={(e) => app.roto && (app.roto.removal = e.currentTarget.checked)} /> Make it an Object to remove</label>
  {/if}
  {#if roto && !roto.pick}
    <div class="row">
      <span class="lbl">Brush</span>
      <input type="range" min="3" max="60" value={roto.size} oninput={(e) => app.roto && (app.roto.size = Number(e.currentTarget.value))} aria-label="Brush size" />
      <span class="lbl">{roto.size} px</span>
    </div>
  {/if}
  {#if ins.masks.length === 0}
    <p class="note">Cut the layer down to a shape. Masks are applied before effects and move with the layer.</p>
  {/if}
  {#each ins.masks as mv, i}
    {@const m = mv.mask}
    <div class="card" class:on={i === active}>
      <div class="row">
        <input type="checkbox" checked={m.enabled} aria-label="Mask on" title="On / off" onchange={(e) => change(i, (n) => (n.enabled = e.currentTarget.checked), true)} />
        <button
          class="name"
          class:on={i === active}
          onclick={() => (app.activeMask = i === active ? null : { clip: ins.id, index: i })}
          title="Edit this mask in the Viewer: drag its points, or inside it to move it">{m.name}</button
        >
        <select value={m.mode} aria-label="How this mask combines with the ones above it" onchange={(e) => change(i, (n) => (n.mode = e.currentTarget.value as Mask["mode"]), true)}>
          {#each modes as md}<option value={md}>{md}</option>{/each}
        </select>
        <label class="check"><input type="checkbox" checked={m.inverted} onchange={(e) => change(i, (n) => (n.inverted = e.currentTarget.checked), true)} /> Invert</label>
      </div>
      <div class="row">
        <span class="lbl" title="What the mask does. A garbage matte for keys keeps the layer inside it whatever the chroma, luma or difference key says (a subject near the screen's colour, a dark logo on a luma key); it doesn't cut the layer. To keep something out of the shot, use an ordinary mask (it cuts the layer before the key). Leave out of the 3D camera track marks something moving on its own, so the camera solve ignores it; it doesn't cut the layer either. Object to remove marks what the Object removal effect fills in.">Use</span>
        <select
          value={m.key_keep ? "keep" : m.track_ignore ? "track" : m.fill_area ? "fill" : m.text_path ? "path" : "cut"}
          aria-label="What this mask does"
          onchange={(e) =>
            change(
              i,
              (n) => {
                const v = e.currentTarget.value;
                n.key_keep = v === "keep";
                n.track_ignore = v === "track";
                n.fill_area = v === "fill";
                n.text_path = v === "path";
              },
              true,
            )}
        >
          <option value="cut">Cut the layer (show only inside)</option>
          <option value="keep">Garbage matte: keys keep what's inside</option>
          <option value="track">Leave out of the 3D camera track</option>
          <option value="fill">Object to remove (Object removal fills it)</option>
          <option value="path">A path (for text, or the Neon path effect)</option>
        </select>
      </div>
      {#if m.fill_area}<p class="note">Doesn't change the picture by itself: add the Object removal effect (Repair) to this clip and press Remove object. Animate or track the mask to follow what's moving.</p>{/if}
      {#if m.track_ignore}<p class="note">Doesn't change the picture. Details inside aren't used to solve the camera: draw it over people, cars, water or anything else moving on its own (animate it to follow them), then track the camera again.</p>{/if}
      {#if m.key_keep && !hasKey}<p class="note">This clip has no chroma, luma or difference key yet: add one and it keeps what's inside this mask.</p>{/if}
      <div class="grid">
        <div class="field">
          <span class="lbl">Feather</span>
          <NumberField label="Feather" value={m.feather} min={0} max={1000} step={0.5} digits={0} suffix=" px" oninput={(v) => change(i, (n) => (n.feather = v), false)} oncommit={commit} />
        </div>
        <div class="field">
          <span class="lbl">Expand</span>
          <NumberField label="Expand" value={m.expansion} min={-1000} max={1000} step={0.5} digits={0} suffix=" px" oninput={(v) => change(i, (n) => (n.expansion = v), false)} oncommit={commit} />
        </div>
        <div class="field">
          <span class="lbl">Opacity</span>
          <NumberField label="Mask opacity" value={m.opacity * 100} min={0} max={100} step={0.5} digits={0} suffix="%" oninput={(v) => change(i, (n) => (n.opacity = v / 100), false)} oncommit={commit} />
        </div>
      </div>
      {#if m.text_path}<p class="note">Doesn't cut the layer: a title's text can run along it (Text tab → Path → Along), and the Neon path effect can glow along it (its Mask number is this one's place in the list).</p>{/if}
      {#if !m.track_ignore && !m.fill_area && !m.key_keep && !m.text_path}
        <div class="row track">
          <label class="lbl" title="Fits the edge to the picture: within the radius each side of the mask's edge, every pixel's coverage is worked out from where its colour sits between the colours surely inside and surely outside, so hair, fur and motion blur go soft and background the mask took in goes">
            <input type="checkbox" checked={!!m.refine} onchange={(e) => change(i, (n) => (n.refine = e.currentTarget.checked ? { radius: 8, smooth: 20, shift: 0 } : undefined), true)} />
            Refine edge
          </label>
        </div>
        {#if m.refine}
          {@const r = m.refine}
          <div class="grid">
            <div class="field">
              <span class="lbl" title="How far each side of the mask's edge is worked out (and how far round each pixel is looked at)">Radius</span>
              <NumberField label="Refine radius" value={r.radius} min={1} max={200} step={0.5} digits={0} suffix=" px" oninput={(v) => change(i, (n) => n.refine && (n.refine.radius = v), false)} oncommit={commit} />
            </div>
            <div class="field">
              <span class="lbl" title="0: crisp edges; 100: as soft as the picture is">Softness</span>
              <NumberField label="Refine softness" value={r.smooth} min={0} max={100} step={0.5} digits={0} suffix="%" oninput={(v) => change(i, (n) => n.refine && (n.refine.smooth = v), false)} oncommit={commit} />
            </div>
            <div class="field">
              <span class="lbl" title="Shrinks (−) or grows (+) the refined edge">Shift</span>
              <NumberField label="Refine shift" value={r.shift} min={-100} max={100} step={0.5} digits={0} suffix="%" oninput={(v) => change(i, (n) => n.refine && (n.refine.shift = v), false)} oncommit={commit} />
            </div>
          </div>
        {/if}
      {/if}
      {#if i === active && m.shape !== "Ellipse"}
        {@const pt = app.activeMaskPoint}
        {#if pt != null && pt < mv.handles.length}
          {@const own = m.feathers?.[pt] ?? -1}
          {@const curved = (mv.curves[pt] ?? []).some((v) => v !== 0)}
          <div class="row pointrow">
            <span class="lbl">Point {pt + 1}</span>
            <button class="small" onclick={() => run(api.maskPointCurve(ins.id, i, pt))} title="Double-click the point in the Viewer does the same">{curved ? "Make sharp" : "Make curve"}</button>
            <button class="small" onclick={async () => { const at = await run(api.maskPointAdd(ins.id, i, pt, false)); if (typeof at === "number") app.activeMaskPoint = at; }} title="Add a point halfway to the next one (right-click a point in the Viewer for more)">Add point</button>
            <button class="small" disabled={mv.handles.length <= 3} onclick={async () => { if (await run(api.maskPointDelete(ins.id, i, pt))) app.activeMaskPoint = Math.min(pt, mv.handles.length - 2); }} title={mv.handles.length <= 3 ? "A mask keeps at least three points" : "Delete this point"}>Delete</button>
            <span class="lbl" title="The soft edge at this point; it blends into the next points' along the sides">Feather here</span>
            <NumberField label="Feather at this point" value={own >= 0 ? own : m.feather} min={0} max={1000} step={0.5} digits={0} suffix=" px" oninput={(v) => change(i, (n) => pointFeather(n, pt, mv.handles.length, v), false)} oncommit={commit} />
            {#if own >= 0}
              <button class="small" onclick={() => change(i, (n) => pointFeather(n, pt, mv.handles.length, null), true)} title="Use the mask's feather at this point again">Reset</button>
            {/if}
          </div>
        {:else}
          <p class="note">Click a point in the Viewer to give it its own feather. Double-click a point to make it a curve, or Alt-drag one to pull out its handles. Right-click the mask's line to add a point there, or a point to add points beside it or delete it.</p>
        {/if}
      {/if}
      <div class="row">
        <label class="check" title="Moving the shape's points then sets a keyframe at the playhead">
          <input type="checkbox" checked={m.keys.length > 0} onchange={(e) => run(api.maskOp(ins.id, i, e.currentTarget.checked ? "animate" : "still"))} />
          Animate shape
        </label>
        {#if m.keys.length}
          <span class="muted">{m.keys.length} key{m.keys.length === 1 ? "" : "s"}</span>
          {#if !mv.key_here}
            <button class="small" onclick={() => run(api.maskOp(ins.id, i, "key"))}>Key here</button>
          {:else if m.keys.length > 1}
            <button class="small" onclick={() => run(api.maskOp(ins.id, i, "unkey"))}>Delete key</button>
          {/if}
        {/if}
        <span class="grow"></span>
        <button class="small" onclick={() => run(api.shapeFromMask(ins.id, i))} title="A new 3D shape (like 3D text) in this mask's outline as seen at the playhead, placed where the mask is: extrude it, bevel it, light it">3D shape</button>
        <button class="small" onclick={() => remove(i)}>Remove</button>
      </div>
      {#if m.keys.length >= 3}
        <div class="row track">
          <span class="lbl" title="Takes the jitter out of the shape's keyframes (a tracked mask's): each is eased towards a steady line through its neighbours, so shake goes and the movement stays. Undo puts them back">Smooth keys</span>
          <select bind:value={smoothFrames} aria-label="How much to smooth (frames)">
            {#each [1, 2, 3, 5, 8] as f}<option value={f}>{f === 1 ? "Gently (1 frame)" : `${f} frames`}</option>{/each}
          </select>
          <button class="small" onclick={() => run(api.maskSmooth(ins.id, i, smoothFrames))}>Smooth</button>
        </div>
      {/if}
      {#if m.roto?.length && progress === null}
        <div class="row track">
          <span class="lbl" title="Carries the roto brush's cut on frame by frame from the playhead: the outline moves with what's in it and snaps to the subject's edges again. Where it slips, paint over that frame to correct it and carry on from there">Roto brush</span>
          <button class="small" class:on={roto?.mask === i} onclick={() => { app.roto = { clip: ins.id, mask: i, size: app.roto?.size ?? 14 }; api.rotoPrepare(ins.id).catch(() => {}); }} title="Paint on this mask (correct it at the playhead)">Paint</button>
          <button class="small" disabled={elsewhere} onclick={() => run(api.rotoPropagate(ins.id, i, false))} title="From the playhead back to the clip's start">◀ Carry back</button>
          <button class="small" disabled={elsewhere} onclick={() => run(api.rotoPropagate(ins.id, i, true))} title="From the playhead to the clip's end">Carry on ▶</button>
        </div>
      {/if}
      {#if trackable}
        <div class="row track">
          <span class="lbl" title={through ? "Follows what's inside the mask in the footage below this layer, from the playhead, keying the shape at every frame. Fix a frame where it slips by dragging the points, then track on from there" : "Follows what's inside the mask from the playhead, keying the shape at every frame. Fix a frame where it slips by dragging the points, then track on from there"}>Track shape</span>
          {#if progress !== null}
            <div class="bar"><div style="width:{progress * 100}%"></div></div>
            <span class="tc">{Math.round(progress * 100)}%</span>
            <button class="small" onclick={() => run(api.trackCancel())}>Stop</button>
          {:else}
            <select bind:value={motion} aria-label="How the shape may move" title="How the shape may move as it follows">
              {#each motions as [v, l]}<option value={v}>{l}</option>{/each}
            </select>
            <button class="small" disabled={elsewhere} onclick={() => run(api.maskTrack(ins.id, i, false, motion))} title="From the playhead back to the clip's start">◀</button>
            <button class="small" disabled={elsewhere} onclick={() => run(api.maskTrack(ins.id, i, true, motion))} title="From the playhead to the clip's end">▶</button>
          {/if}
        </div>
      {/if}
    </div>
  {/each}
</div>

<style>
  .track {
    gap: 6px;
  }
  .track .lbl {
    color: var(--muted);
  }
  .track select {
    flex: 1;
    min-width: 0;
  }
  .bar {
    flex: 1;
    height: 8px;
    background: var(--base);
    border-radius: 4px;
    overflow: hidden;
  }
  .bar div {
    height: 100%;
    background: var(--accent);
  }
  .tc {
    font-variant-numeric: tabular-nums;
  }
  .group {
    padding: 8px 0 12px;
    border-bottom: 1px solid var(--line-soft);
  }
  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    flex-wrap: wrap;
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
    flex-wrap: wrap;
  }
  .behind {
    display: flex;
    align-items: center;
    gap: 6px;
    flex-wrap: wrap;
    margin: 4px 0 6px;
  }
  .behind .lbl {
    color: var(--muted);
    font-size: calc(11.5px * var(--text-scale));
  }
  .behind select {
    min-width: 0;
    max-width: 14em;
  }
  .behind input[type="color"] {
    width: 34px;
    height: 22px;
    padding: 0;
  }
  .small {
    min-height: 22px;
    padding: 0 7px;
    font-size: calc(12px * var(--text-scale));
  }
  .pointrow {
    flex-wrap: wrap;
  }
  .note {
    color: var(--faint);
    font-size: calc(12px * var(--text-scale));
    margin: 2px 0 8px;
  }
  .card {
    border: 1px solid var(--line-soft);
    border-left: 3px solid var(--line);
    border-radius: 5px;
    padding: 6px 8px;
    margin-bottom: 6px;
    display: flex;
    flex-direction: column;
    gap: 6px;
    background: var(--base);
  }
  .card.on {
    border-left-color: #ff6ec8;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 6px;
    flex-wrap: wrap;
  }
  .name {
    flex: 1;
    min-width: 60px;
    text-align: left;
    background: none;
    border-color: transparent;
    padding: 2px 4px;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .name.on {
    color: #ff6ec8;
  }
  .rm {
    margin: 2px 0 6px;
  }
  .check {
    display: flex;
    align-items: center;
    gap: 4px;
  }
  .grid {
    display: grid;
    grid-template-columns: repeat(3, minmax(0, 1fr));
    gap: 6px;
  }
  .field {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }
  .lbl {
    color: var(--muted);
    font-size: calc(12px * var(--text-scale));
  }
  .grow {
    flex: 1;
  }
  .muted {
    color: var(--muted);
    font-size: calc(12px * var(--text-scale));
  }
</style>
