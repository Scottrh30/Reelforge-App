<script lang="ts">
  // The Track panel: track the selected clip with the method of your choice
  // (one point, two points, four points, a planar surface or a bending mesh),
  // then put the motion on any layer, a mask, or the clip itself (stabilise),
  // choosing which parts of the motion to use.
  import { app, run, timecode } from "../lib/session.svelte";
  import { api, type CamFormat, type CamMake, type StabZoom, type TrackAxes, type TrackMode, type TrackerView } from "../lib/api";
  import { livePump } from "../lib/live";
  import NumberField from "./NumberField.svelte";

  // The clip being tracked: the selected clip when it can be tracked, else
  // the last one picked here (or any clip with a tracker).
  const sel = $derived(app.s?.inspector?.can_track ? app.s.inspector : null);
  let picked = $state<number | null>(null);
  const selId = $derived(sel?.id ?? null);
  $effect(() => {
    if (selId !== null) picked = selId;
  });
  type Source = { id: number; name: string; track: string; tracker: TrackerView | null };
  const sources = $derived.by((): Source[] => {
    const list: Source[] = (app.s?.trackers ?? []).map((t) => ({ ...t }));
    if (sel && !list.some((t) => t.id === sel.id)) list.unshift({ id: sel.id, name: sel.name, track: sel.track, tracker: sel.tracker });
    return list;
  });
  const ins = $derived(sources.find((x) => x.id === picked) ?? sources[0] ?? null);

  const methods: [TrackMode, string, string][] = [
    ["Position", "Single point", "1"],
    ["Transform", "Two point", "2"],
    ["CornerPin", "Four point", "4"],
    ["Planar", "Planar", "▱"],
    ["Mesh", "Mesh", "▦"],
  ];
  const methodTip: Record<TrackMode, string> = {
    Position: "Position only",
    Transform: "Position, rotation and scale",
    CornerPin: "Corner pin / perspective",
    Planar: "A whole flat surface, in perspective",
    Mesh: "A surface that bends",
  };
  function setBlend(v: number) {
    app.trackBlend = Math.max(0, Math.min(120, Math.round(v)));
    try {
      localStorage.setItem("reelforge.trackBlend", String(app.trackBlend));
    } catch {}
  }
  function setFalloff(v: number) {
    app.meshFalloff = Math.max(0, Math.min(16, Math.round(v * 10) / 10));
    try {
      localStorage.setItem("reelforge.meshFalloff", String(app.meshFalloff));
    } catch {}
  }
  // Choosing Custom… shows boxes for any number of cells across and down.
  let customGrid = $state(false);
  function setGrid(c: number, r: number) {
    if (!ins?.tracker) return;
    const fit = (v: number) => Math.min(32, Math.max(1, Math.round(Number.isFinite(v) ? v : 1)));
    run(api.trackerGrid(ins.id, fit(c), fit(r)));
  }
  const grids: [number, number][] = [[2, 2], [3, 3], [4, 3], [4, 4], [6, 4], [6, 6], [8, 6], [8, 8], [16, 16], [32, 32]];
  const hints: Record<TrackMode, string> = {
    Position: "Drag the box in the Viewer onto a sharp detail (a corner, a mark, a light), then track. The outer box is how far it may move per frame.",
    Transform: "Put the two boxes on sharp details of the same object, as far apart as you can: the line between them gives the rotation and scale.",
    CornerPin: "Put boxes 1 to 4 on the surface's top-left, top-right, bottom-right and bottom-left corners (or on details near them).",
    Planar: "Drag corners 1 to 4 onto a flat surface (a screen, a sign, a wall). Everything with detail inside the shape is tracked, so the corners can sit on blank areas, and the surface can be partly covered.",
    Mesh: "Drag the grid's four big corners over a surface that bends (cloth, skin, a flag). Before tracking you can also drag the inner points to follow a curve.",
  };
  const perspective = (m: TrackMode) => m === "CornerPin" || m === "Planar";
  const areaTracked = (m: TrackMode) => m === "Planar" || m === "Mesh";

  let smoothing = $state(0);
  let stabZoom = $state<StabZoom>("Off");
  let target = $state("");
  let axes = $state<TrackAxes>({ x: true, y: true, rotation: true, scale: true });
  const t = $derived(ins?.tracker ?? null);
  const progress = $derived(ins && app.live?.tracking && app.live.tracking[0] === ins.id ? app.live.tracking[1] : null);
  const elsewhere = $derived(!!ins && !!app.live?.tracking && app.live.tracking[0] !== ins.id);
  const key = (c: { clip: number; mask: number | null; blade: number | null }) => `${c.clip}:${c.mask ?? ""}:${c.blade ?? ""}`;

  // Pick a sensible target: another layer or a mask before stabilising this one.
  $effect(() => {
    const list = t?.targets ?? [];
    if (!list.some((c) => key(c) === target)) {
      const first = list.find((c) => c.clip !== ins?.id || c.mask !== null || c.blade !== null) ?? list[0];
      target = first ? key(first) : "";
    }
  });
  const chosen = $derived(t?.targets.find((c) => key(c) === target) ?? null);
  const stabilising = $derived(!!ins && chosen?.clip === ins.id && chosen?.mask === null && chosen?.blade === null);
  const toBlade = $derived(chosen?.blade != null);
  const toMask = $derived(chosen?.mask != null);
  // A point turns and scales with any track that has more than one point.
  const toPoint = $derived(chosen?.point === true);
  const turns = $derived(t?.mode === "Transform" || (toPoint && t?.mode !== "Position"));
  const anyAxis = $derived(axes.x || axes.y || (turns && (axes.rotation || axes.scale)));

  function pick(m: TrackMode) {
    if (!ins) return;
    app.cam3d = false;
    if (!t) run(api.trackerAdd(ins.id, m));
    else if (t.mode !== m) run(api.trackerMode(ins.id, m));
  }

  const pump = livePump();
  function sizes(size: number, search: number) {
    if (ins) pump.push(() => api.trackerSizes(ins.id, size, search));
  }
  async function commit() {
    await pump.done();
    await run(api.commit());
  }
  // Smoothing being dragged, so the number keeps up with the pointer.
  let smoothDraft = $state<number | null>(null);
  function smoothTo(v: number) {
    if (!ins) return;
    const id = ins.id;
    smoothDraft = v;
    pump.push(() => api.trackerSmooth(id, v));
  }
  async function smoothDone() {
    await commit();
    smoothDraft = null;
  }
  // ---- The 3D camera tracker ----
  // Its data comes with the selected clip (the inspector).
  const cam = $derived(sel && ins && sel.id === ins.id ? sel.cam_track : null);
  const show3d = $derived(app.cam3d || (!t && !!cam));
  const camBusy = $derived(!!sel && sel.id === ins?.id && sel.cam_tracking && progress !== null);
  const camPicked = $derived(app.camSel && ins && app.camSel.clip === ins.id ? app.camSel.points : []);
  let knownLens = $state(false);
  let focalMm = $state(35);
  let tripod = $state(false);
  // 35 mm-equivalent focal length (36 mm across) ↔ vertical field of view, for this frame's shape.
  const aspect = $derived((app.s?.seq.width ?? 16) / (app.s?.seq.height ?? 9));
  const fovOf = (mm: number) => (2 * Math.atan(36 / aspect / 2 / mm) * 180) / Math.PI;
  const mmOf = (fov: number) => 36 / aspect / 2 / Math.tan((fov * Math.PI) / 360);
  function analyse() {
    if (!ins) return;
    app.camSel = null;
    run(api.camtrackStart(ins.id, knownLens ? fovOf(focalMm) : null, tripod));
  }
  function make(what: CamMake) {
    if (ins) run(api.camtrackMake(ins.id, what, camPicked));
  }
  const exports: [CamFormat, string, string][] = [
    ["AfterEffects", "After Effects", "A script (.jsx): File → Scripts → Run Script File… makes a comp with the camera and a 3D null per point"],
    ["Blender", "Blender", "A script (.py): run it in Blender's Scripting workspace for the camera and the points"],
    ["Nuke", "Nuke", "A Nuke script (.nk) with a Camera2, and the points as an .obj beside it"],
    ["Chan", ".chan", "The camera's path as a .chan file, which Nuke, Blender, Maya and tracking apps read"],
    ["Json", "Reelforge", "The whole solve, points and all, to load onto a clip in another project"],
  ];
  const fit = (e: number) => (e < 0.5 ? "very good" : e < 1 ? "good" : e < 2 ? "fair" : "poor");

  function apply() {
    if (!ins || !chosen) return;
    run(api.trackApply(ins.id, chosen.clip, chosen.mask, chosen.blade, smoothing, { ...axes }, stabilising ? stabZoom : "Off"));
  }
</script>

<div class="panel">
  {#if !ins}
    <p class="empty">Select a video clip in the timeline to track it.</p>
  {:else}
    <div class="source">
      <span class="lbl">Tracking</span>
      {#if sources.length > 1}
        <select value={ins.id} aria-label="Clip being tracked" onchange={(e) => (picked = Number(e.currentTarget.value))}>
          {#each sources as x}<option value={x.id}>{x.name} ({x.track})</option>{/each}
        </select>
      {:else}
        <span class="name">{ins.name}</span>
        <span class="trk">{ins.track}</span>
      {/if}
    </div>
    {#if app.s?.selection !== ins.id}
      <div class="row away">
        <span class="note">Its tracker shows in the Viewer while the clip is selected.</span>
        <button class="small" onclick={() => run(api.select(ins.id, "only"))}>Select</button>
      </div>
    {/if}

    <div class="sect">
      <h4>Method</h4>
      <div class="methods" role="radiogroup" aria-label="Tracking method">
        {#each methods as [m, label, icon]}
          <button
            role="radio"
            aria-checked={!show3d && t?.mode === m}
            class:on={!show3d && t?.mode === m}
            title={methodTip[m]}
            disabled={progress !== null}
            onclick={() => pick(m)}><span class="ico">{icon}</span>{label}</button
          >
        {/each}
        <button role="radio" aria-checked={show3d} class:on={show3d} title="Solve the camera that filmed the shot, to put 3D layers, text and points into it" disabled={progress !== null && !camBusy} onclick={() => (app.cam3d = true)}><span class="ico">3D</span>3D camera</button>
      </div>
      {#if show3d}
        <p class="note">Works out how the camera moved through the whole clip, its lens, and where the details in the shot are in 3D. Then add a camera that moves the same way, and layers that stay put in the scene.</p>
      {:else if !t}
        <p class="note">Pick a method to place a tracker on this clip.</p>
      {:else}
        <p class="note">{hints[t.mode]}</p>
      {/if}
    </div>

    {#if show3d}
      {#if !sel || sel.id !== ins.id}
        <p class="note">Select the clip in the timeline to solve its camera.</p>
      {:else}
        <div class="sect">
          <h4>Analyse</h4>
          <div class="checks">
            <label title="If you know the lens, give it: a short shot can't always pin it down by itself"><input type="checkbox" bind:checked={knownLens} disabled={camBusy} /> I know the lens</label>
            <label title="The camera turned in place without moving (on a tripod): solve it as turning only"><input type="checkbox" bind:checked={tripod} disabled={camBusy} /> Camera only turns (tripod)</label>
          </div>
          {#if knownLens}
            <div class="grid">
              <span class="lbl" title="Focal length as on a full-frame (35 mm) camera, 36 mm across">Focal length</span>
              <div class="ctl">
                <NumberField label="Focal length" value={focalMm} min={6} max={600} step={0.1} digits={1} suffix=" mm" oninput={(v) => (focalMm = v)} oncommit={() => {}} />
                <span class="muted nowrap">{fovOf(focalMm).toFixed(1)}° high</span>
              </div>
            </div>
          {/if}
          {#if camBusy}
            <div class="row">
              <div class="bar"><div style="width:{(progress ?? 0) * 100}%"></div></div>
              <span class="tc">{Math.round((progress ?? 0) * 100)}%</span>
              <button onclick={() => run(api.trackCancel())}>Stop</button>
            </div>
            <p class="note">{(progress ?? 0) < 0.6 ? "Following details through the shot…" : "Solving the camera…"}</p>
          {:else}
            <div class="row">
              <button class="primary wide" disabled={elsewhere} onclick={analyse} title="Analyses the whole clip">{cam ? "Analyse again" : "Analyse"}</button>
            </div>
            <div class="row">
              <button class="wide" onclick={() => run(api.camtrackImport())} title="A camera path (.chan) from Nuke, Blender, SynthEyes, 3DEqualizer or PFTrack becomes a camera clip from this clip's start; a Reelforge solve (.json) goes onto this clip">Import a camera…</button>
            </div>
            {#if elsewhere}<p class="note">Another clip is being tracked.</p>{/if}
          {/if}
        </div>

        {#if cam}
          <div class="sect">
            <h4>Solved</h4>
            <p class="muted">
              {cam.frames} frames, {cam.point_count} points · error {cam.error.toFixed(2)} px ({fit(cam.error)}) · lens {cam.fov.toFixed(1)}° high (about {mmOf(cam.fov).toFixed(0)} mm){cam.tripod ? " · turning in place" : ""}
            </p>
            {#if cam.placed < cam.frames}<p class="note">{cam.frames - cam.placed} frames had too little to go on and were filled in from their neighbours.</p>{/if}
            <p class="note">The points in the Viewer are details found in 3D. Click one to pick it; Shift+click or Shift+drag a box to pick more. Pick points on the surface where something should go.</p>
            <div class="row away">
              <span class="muted">{camPicked.length ? `${camPicked.length} picked` : "None picked"}</span>
              {#if camPicked.length}<button class="ghost small" onclick={() => (app.camSel = null)} title="Drop the picked points (Escape)">Clear</button>{/if}
            </div>
          </div>

          <div class="sect">
            <h4>Create</h4>
            <div class="makes">
              <button onclick={() => make("Camera")} title="A camera clip keyed to move like the real one (3D layers then stay put in the shot)">{cam.camera ? "Camera again" : "Camera"}</button>
              <button disabled={!camPicked.length} onclick={() => make("Solid")} title="A solid lying on the picked points' surface">Solid</button>
              <button disabled={!camPicked.length} onclick={() => make("Title")} title="A title on the picked points' surface">Title</button>
              <button disabled={!camPicked.length} onclick={() => make("Text3d")} title="3D text on the picked points' surface">3D text</button>
              <button disabled={!camPicked.length} onclick={() => make("Point")} title="An invisible point at the picked points: parent anything to it">Point</button>
            </div>
            <p class="note">{cam.camera ? "Layers made here use the camera already made." : "The first thing made also makes the camera."} Three or more points set the surface's tilt; with fewer, it faces the camera.</p>
            <div class="row">
              <button class="wide" disabled={camPicked.length < 3} onclick={() => ins && run(api.camtrackGround(ins.id, camPicked))} title="Turn the scene so the picked points' surface is the floor, its middle the origin (for lights and 3D text that stand on it)">Set ground plane</button>
            </div>
          </div>

          <div class="sect">
            <h4>Export</h4>
            <div class="makes">
              {#each exports as [f, label, tip]}
                <button onclick={() => ins && run(api.camtrackExport(ins.id, f, camPicked))} title={tip}>{label}</button>
              {/each}
            </div>
            <p class="note">The camera, and {camPicked.length ? `the ${camPicked.length} picked points` : "all the points"}. For Blender, Nuke and .chan, 100 pixels become 1 unit (a metre in Blender).</p>
          </div>

          <div class="row">
            <button class="wide" onclick={() => ins && run(api.camtrackRemove(ins.id))}>Remove camera track</button>
          </div>
        {/if}
      {/if}
    {:else if t}
      <div class="sect">
        <h4>Track</h4>
        <div class="grid">
          {#if t.mode === "Mesh"}
            <span class="lbl" title="Cells across × down. Changing it starts the grid again from its four corners">Grid</span>
            {@const preset = grids.some(([c, r]) => c === t.grid[0] && r === t.grid[1])}
            <span class="gridpick">
              <select value={customGrid || !preset ? "custom" : `${t.grid[0]}x${t.grid[1]}`} aria-label="Mesh grid" onchange={(e) => {
                const v = e.currentTarget.value;
                if (v === "custom") {
                  customGrid = true;
                  return;
                }
                customGrid = false;
                const [c, r] = v.split("x").map(Number);
                run(api.trackerGrid(ins.id, c, r));
              }}>
                {#each grids as [c, r]}<option value={`${c}x${r}`}>{c} × {r} cells</option>{/each}
                <option value="custom">Custom…</option>
              </select>
              {#if customGrid || !preset}
                <!-- Any size: cells across × down, 1 to 32 each. -->
                <input class="cells" type="number" min="1" max="32" step="1" value={t.grid[0]} aria-label="Cells across" title="Cells across (1–32)" onchange={(e) => setGrid(Number(e.currentTarget.value), t.grid[1])} />
                <span class="times" aria-hidden="true">×</span>
                <input class="cells" type="number" min="1" max="32" step="1" value={t.grid[1]} aria-label="Cells down" title="Cells down (1–32)" onchange={(e) => setGrid(t.grid[0], Number(e.currentTarget.value))} />
              {/if}
            </span>
          {/if}
          {#if t.mode === "Mesh"}
            <span class="lbl" title="Dragging a mesh point, the points around it follow, less and less, out to this many cells (0: only the point). Shift+click points, or Shift+drag a box over the grid, to pick several and move them together">Falloff</span>
            <span class="gridpick">
              <NumberField label="Falloff" value={app.meshFalloff} min={0} max={16} step={0.05} digits={1} suffix=" cells" oninput={setFalloff} oncommit={() => {}} />
              {#if app.meshSel?.clip === ins.id && app.meshSel.points.length}
                <span class="muted nowrap">{app.meshSel.points.length} picked</span>
                <button class="ghost small" onclick={() => (app.meshSel = null)} title="Drop the picked points (Escape)">Clear</button>
              {/if}
            </span>
          {/if}
          {#if !areaTracked(t.mode)}
            <span class="lbl">Feature box</span>
            <NumberField label="Feature box size" value={t.size} min={9} max={201} step={0.5} digits={0} suffix=" px" oninput={(v) => sizes(v, t.search)} oncommit={commit} />
          {/if}
          <span class="lbl" title="How far it may move between frames. Raise it for fast motion">Search</span>
          <NumberField label="Search area" value={t.search} min={2} max={300} step={0.5} digits={0} suffix=" px" oninput={(v) => sizes(t.size, v)} oncommit={commit} />
        </div>

        {#if progress !== null}
          <div class="row">
            <div class="bar"><div style="width:{progress * 100}%"></div></div>
            <span class="tc">{Math.round(progress * 100)}%</span>
            <button onclick={() => run(api.trackCancel())}>Stop</button>
          </div>
        {:else}
          <div class="row two">
            <button disabled={elsewhere} onclick={() => run(api.trackStart(ins.id, false))} title="From the playhead back to the clip's start">◀ Backward</button>
            <button class="primary" disabled={elsewhere} onclick={() => run(api.trackStart(ins.id, true))} title="From the playhead to the clip's end">Forward ▶</button>
          </div>
          {#if elsewhere}<p class="note">Another clip is being tracked.</p>{/if}
        {/if}
        {#if t.range}
          <p class="muted">
            {t.frames} frames tracked, <span class="tc">{timecode(t.range[0])}</span> to <span class="tc">{timecode(t.range[1])}</span>
          </p>
          <div class="grid">
            <span class="lbl" title="Takes the jitter out of the tracked paths without tracking again. The tracked data is kept: set it back to 0 any time. Everything you apply the track to uses the smoothed paths">Smooth track</span>
            <div class="ctl">
              <NumberField label="Smooth track" value={smoothDraft ?? t.smooth} min={0} max={30} step={0.05} digits={1} suffix=" frames" oninput={smoothTo} oncommit={smoothDone} />
              {#if t.smooth > 0}<button class="ghost tiny" onclick={() => { smoothTo(0); smoothDone(); }} title="No smoothing" aria-label="No smoothing">↺</button>{/if}
            </div>
            <span class="lbl" title="Dragging a point on a tracked frame corrects it there; this eases the correction into this many tracked frames on each side (fading out), so the path doesn't jump at that frame. 0: that frame only">Blend fixes</span>
            <div class="ctl">
              <NumberField label="Blend fixes" value={app.trackBlend} min={0} max={120} step={0.1} digits={0} suffix=" frames" oninput={setBlend} oncommit={() => {}} />
            </div>
          </div>
          {#if t.smooth > 0}<p class="note">The solid lines in the Viewer are the smoothed paths; the faint dotted ones are as tracked. Apply the track again to use the new smoothing.</p>{/if}
        {/if}
      </div>

      <div class="sect">
        <h4>Apply tracking data</h4>
        {#if !t.range}
          <p class="note">Track the clip first; then choose a layer here to move with it.</p>
        {:else}
          <div class="grid">
            <span class="lbl">Apply to</span>
            <select bind:value={target} aria-label="Apply the tracking data to">
              {#each t.targets as c}<option value={key(c)}>{c.label}</option>{/each}
            </select>
          </div>
          <div class="checks" class:dim={toMask}>
            <label title="Movement across"><input type="checkbox" bind:checked={axes.x} disabled={toMask} /> X position</label>
            <label title="Movement up and down"><input type="checkbox" bind:checked={axes.y} disabled={toMask} /> Y position</label>
            {#if turns}
              <label><input type="checkbox" bind:checked={axes.rotation} disabled={toMask} /> Rotation</label>
              <label><input type="checkbox" bind:checked={axes.scale} disabled={toMask} /> Scale</label>
            {/if}
          </div>
          {#if toMask}<p class="note">A mask follows the whole motion.</p>{/if}
          {#if stabilising && !perspective(t.mode)}
            <div class="grid">
              <span class="lbl" title="Smooth camera: 0 locks the shot completely. Higher keeps slow, deliberate camera moves and only takes out the shake: 10 to 30 frames suits most handheld footage">Keep camera moves</span>
              <NumberField label="Smoothing" value={smoothing} min={0} max={120} step={0.25} digits={0} suffix=" frames" oninput={(v) => (smoothing = v)} oncommit={() => {}} />
            </div>
          {/if}
          {#if stabilising && t.mode !== "Mesh"}
            <div class="grid">
              <span class="lbl" title="Moving the clip against the shake uncovers its edges. Zoom in just enough that they never show: one zoom for the whole clip, or a zoom that eases with the motion (less where the shake is small)">Hide edges</span>
              <select bind:value={stabZoom} aria-label="Hide edges">
                <option value="Off">Off (edges show)</option>
                <option value="Fixed">Zoom in once</option>
                <option value="Follow">Zoom with the motion</option>
              </select>
            </div>
          {/if}
          <div class="row">
            <button class="primary wide" disabled={!chosen || (!toMask && !anyAxis)} onclick={apply} title="Writes keyframes (undoable)">Apply</button>
          </div>
          {#if toPoint}
            <p class="note">The point keeps where it is at the start of the track and moves with it from there. Every layer, camera and light parented to the point follows too (Controls → Layer → Parent).{#if t.mode !== "Position" && t.mode !== "Transform"} A point can't take perspective: it gets the surface's move, turn and scale.{/if}</p>
          {:else if toBlade}
            <p class="note">The blade keeps where its hilt and tip are at the playhead and moves with the tracked points from there. For a prop with two markers, a two-point track also turns and stretches it.</p>
          {:else if t.mode === "Mesh" && chosen && chosen.clip !== ins.id}
            <p class="note">The layer is stretched over the grid and bends with it (Controls → Transform: remove the mesh warp to go back).</p>
          {:else if perspective(t.mode) && chosen && chosen.clip !== ins.id}
            <p class="note">An unpinned layer is pinned straight into the four points. To place it differently, adjust its corner pin at the first tracked frame and apply again.</p>
          {/if}
        {/if}
      </div>

      <div class="row two">
        <button disabled={!t.tracked} onclick={() => run(api.trackerOp(ins.id, "clear"))}>Clear paths</button>
        <button onclick={() => run(api.trackerOp(ins.id, "remove"))}>Remove tracker</button>
      </div>
    {/if}
  {/if}
</div>

<style>
  .panel {
    height: 100%;
    overflow: auto;
    padding: 8px 10px 12px;
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .empty {
    color: var(--faint);
    margin: 18px 4px;
    text-align: center;
  }
  .source {
    display: flex;
    align-items: baseline;
    gap: 6px;
    padding: 6px 8px;
    background: var(--base);
    border-radius: 5px;
    min-width: 0;
  }
  .source select {
    flex: 1;
    min-width: 0;
  }
  .away {
    justify-content: space-between;
  }
  .source .name {
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    flex: 1;
    min-width: 0;
  }
  .source .trk {
    color: var(--faint);
    font-size: calc(11.5px * var(--text-scale));
  }
  .sect {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding-bottom: 10px;
    border-bottom: 1px solid var(--line-soft);
  }
  .ctl {
    display: flex;
    align-items: center;
    gap: 4px;
    min-width: 0;
  }
  .ctl :global(.num) {
    flex: 1;
    min-width: 0;
  }
  .tiny {
    min-height: 20px;
    padding: 0 4px;
    color: var(--muted);
    flex: none;
  }
  h4 {
    font-size: calc(12.5px * var(--text-scale));
    font-weight: 600;
    color: var(--muted);
    margin: 0;
  }
  .methods {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(92px, 1fr));
    gap: 4px;
  }
  .methods button {
    display: flex;
    align-items: center;
    gap: 6px;
    justify-content: flex-start;
    padding: 5px 7px;
    white-space: nowrap;
  }
  .methods button.on {
    background: var(--accent);
    border-color: var(--accent);
    color: #fff;
  }
  .ico {
    display: inline-grid;
    place-items: center;
    width: 16px;
    height: 16px;
    border-radius: 3px;
    font-size: 11px;
    font-weight: 700;
    background: rgba(127, 127, 127, 0.25);
  }
  .note {
    color: var(--faint);
    font-size: calc(12px * var(--text-scale));
    margin: 0;
  }
  .muted {
    color: var(--muted);
    font-size: calc(12px * var(--text-scale));
    margin: 0;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .row.two > button {
    flex: 1;
  }
  .wide {
    flex: 1;
  }
  .grid {
    display: grid;
    grid-template-columns: 84px 1fr;
    gap: 6px;
    align-items: center;
  }
  .grid select {
    min-width: 0;
  }
  .lbl {
    color: var(--muted);
  }
  .checks {
    display: flex;
    flex-wrap: wrap;
    gap: 6px 14px;
  }
  .checks label {
    display: flex;
    align-items: center;
    gap: 5px;
    cursor: pointer;
  }
  .checks.dim {
    opacity: 0.5;
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
  .gridpick {
    display: flex;
    align-items: center;
    gap: 4px;
    min-width: 0;
  }
  .gridpick select {
    min-width: 0;
  }
  .cells {
    width: 46px;
  }
  .times {
    color: var(--muted);
  }
  .small {
    min-height: 22px;
    padding: 0 8px;
  }
  .makes {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(84px, 1fr));
    gap: 4px;
  }
  .nowrap {
    white-space: nowrap;
  }
</style>
