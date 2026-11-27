<script lang="ts">
  // 3D Model Properties: comes up when a 3D model is added (and from a model layer
  // later). Its name and file; a preview to turn round (drag), move (the move tool,
  // or Shift-drag) and zoom (the wheel); and three tabs: Groups (show or hide each
  // part), Materials (each part's picture, colour, highlights, own light and
  // see-through) and Advanced (which way is up, its own colours or one, where its
  // textures are). Nothing changes until OK, and then it's one undo step.
  import { onDestroy } from "svelte";
  import { api } from "../lib/api";
  import { run, say } from "../lib/session.svelte";
  import { modelDialog, type ModelSource, type PartLook } from "../lib/modeldialog.svelte";
  import { pickColor } from "../lib/colorpick.svelte";
  import { ModelView, unpackPreview, type PreviewPart } from "../lib/modelview";

  type Rgb = [number, number, number];
  const DEFAULT_LOOK = { specular: [0, 0, 0] as Rgb, shininess: 30, emissive: [0, 0, 0] as Rgb, opacity: 100, diffuse_map: null as string | null };

  let source = $state<ModelSource>({ path: "" });
  let name = $state("");
  let tab = $state<"groups" | "materials" | "advanced">("materials");
  let search = $state("");
  let parts = $state<PreviewPart[]>([]);
  let triangles = $state(0);
  let missing = $state<string[]>([]);
  let loading = $state(false);
  let error = $state<string | null>(null);
  let open = $state<Record<string, boolean>>({});
  let tool = $state<"orbit" | "move">("orbit");
  let canvas: HTMLCanvasElement | undefined = $state();
  let view: ModelView | null = null;
  let opened: unknown = null;

  // Opened: start from what it was given.
  $effect(() => {
    const o = modelDialog.open;
    if (!o) {
      opened = null;
      return;
    }
    if (o === opened) return;
    opened = o;
    source = structuredClone($state.snapshot(o.source)) as ModelSource;
    name = o.name;
    tab = "materials";
    search = "";
    parts = [];
    open = {};
    error = null;
    firstLoad = o.mode === "add";
  });

  let firstLoad = false;
  // The preview: read again when the file, its texture folders or the parts' pictures change.
  const fetchKey = $derived(JSON.stringify([source.path, source.rev ?? 0, source.texture_dirs ?? [], (source.materials ?? []).map((m) => [m.part, m.diffuse_map ?? null])]));
  let timer: ReturnType<typeof setTimeout> | undefined;
  $effect(() => {
    void fetchKey;
    if (!modelDialog.open || !canvas) return;
    clearTimeout(timer);
    timer = setTimeout(load, 60);
  });
  async function load() {
    if (!canvas) return;
    loading = true;
    try {
      const buf = await api.modelPreview(cleaned());
      const p = unpackPreview(buf);
      if (!view) view = new ModelView(canvas);
      if (firstLoad) {
        source.z_up = p.head.z_up;
        firstLoad = false;
      }
      parts = p.head.parts;
      triangles = p.head.triangles;
      missing = p.head.missing_textures;
      error = view.ok ? null : "This system can't show a 3D preview (no WebGL 2), but the settings still work.";
      view.zUp = !!source.z_up;
      await view.setData(p);
      shade();
    } catch (e) {
      error = `Can't show it: ${e}`;
      parts = [];
    } finally {
      loading = false;
    }
  }

  // The parts' looks to the preview, as they change.
  const lin = (v: number) => {
    const c = v / 255;
    return c <= 0.04045 ? c / 12.92 : Math.pow((c + 0.055) / 1.055, 2.4);
  };
  function shade() {
    if (!view) return;
    view.zUp = !!source.z_up;
    view.own = source.own_colours !== false;
    view.fit();
    view.setParts(
      parts.map((p) => {
        const l = look(p.name);
        const c = ownColour(p.name);
        return {
          visible: !(source.hidden ?? []).includes(p.name),
          color: c ? (c.map(lin) as Rgb) : null,
          specular: l.specular.map(lin) as Rgb,
          shininess: l.shininess,
          emissive: l.emissive.map(lin) as Rgb,
          opacity: l.opacity / 100,
          map: !!l.diffuse_map,
        };
      }),
    );
  }
  $effect(() => {
    void JSON.stringify([source.materials ?? [], source.part_colours ?? [], source.hidden ?? [], source.z_up, source.own_colours]);
    shade();
  });

  const look = (part: string): PartLook => (source.materials ?? []).find((m) => m.part === part) ?? { part, ...DEFAULT_LOOK };
  function setLook(part: string, f: (l: PartLook) => void) {
    const list = (source.materials ?? []).filter((m) => m.part !== part);
    const l = structuredClone($state.snapshot(look(part))) as PartLook;
    f(l);
    list.push(l);
    source.materials = list;
  }
  const ownColour = (part: string): Rgb | null => (source.part_colours ?? []).find((p) => p[0] === part)?.[1] ?? null;
  function setColour(part: string, c: Rgb | null) {
    const list = (source.part_colours ?? []).filter((p) => p[0] !== part);
    if (c) list.push([part, c]);
    source.part_colours = list;
  }
  function toggleHidden(part: string, show: boolean) {
    const list = (source.hidden ?? []).filter((h) => h !== part);
    if (!show) list.push(part);
    source.hidden = list;
  }

  // What's sent: only the parts changed from the file.
  function cleaned(): ModelSource {
    const s = structuredClone($state.snapshot(source)) as ModelSource;
    s.materials = (s.materials ?? []).filter((m) => m.diffuse_map || m.specular.some((v) => v > 0) || m.emissive.some((v) => v > 0) || m.opacity < 100);
    return s;
  }

  function close() {
    modelDialog.open = null;
    view?.dispose();
    view = null;
  }
  async function ok() {
    const o = modelDialog.open;
    if (!o) return;
    const s = cleaned();
    const n = name.trim() || (s.path.split(/[\\/]/).pop() ?? "3D model");
    close();
    if (o.mode === "add") {
      await run(api.addModelLayer(s, n));
    } else {
      const spec = { ...o.spec, text: n, model: s };
      await run(api.setTitle(o.id, spec).then(() => api.commit()));
    }
  }
  onDestroy(() => view?.dispose());

  async function importMaps() {
    const dir = await api.pickTextureFolder();
    if (!dir) return;
    source.texture_dirs = [...(source.texture_dirs ?? []).filter((d) => d !== dir), dir];
    say("Looking for its textures in " + dir);
  }
  async function chooseMap(part: string) {
    const f = await api.pickMapFile();
    if (f) setLook(part, (l) => (l.diffuse_map = f));
  }

  // ---- The preview's mouse ----
  function drag(e: PointerEvent) {
    if (!view || e.button !== 0) return;
    const el = e.currentTarget as HTMLElement;
    el.setPointerCapture(e.pointerId);
    let [x, y] = [e.clientX, e.clientY];
    const moving = tool === "move" || e.shiftKey;
    const move = (ev: PointerEvent) => {
      if (!view) return;
      const [dx, dy] = [ev.clientX - x, ev.clientY - y];
      [x, y] = [ev.clientX, ev.clientY];
      if (moving) {
        const k = view.distance / Math.max(1, el.clientHeight) * 0.9;
        view.pan = [view.pan[0] + dx * k, view.pan[1] - dy * k];
      } else {
        view.yaw += dx * 0.5;
        view.pitch = Math.max(-89, Math.min(89, view.pitch + dy * 0.5));
      }
      view.draw();
    };
    const up = () => (el.removeEventListener("pointermove", move), el.removeEventListener("pointerup", up));
    el.addEventListener("pointermove", move);
    el.addEventListener("pointerup", up);
  }
  function wheel(e: WheelEvent) {
    if (!view) return;
    e.preventDefault();
    view.distance = Math.max(0.3, Math.min(20, view.distance * Math.exp(e.deltaY * 0.001)));
    view.draw();
  }

  function keys(e: KeyboardEvent) {
    if (e.key === "Escape") (e.preventDefault(), e.stopPropagation(), close());
    else if (e.key === "Enter" && !(e.target instanceof HTMLInputElement && e.target.type === "text" && e.target !== nameInput)) (e.preventDefault(), e.stopPropagation(), ok());
  }
  let nameInput: HTMLInputElement | undefined = $state();

  const shown = $derived(parts.filter((p) => p.name.toLowerCase().includes(search.trim().toLowerCase())));
  const hex = (c: Rgb) => "#" + c.map((v) => Math.max(0, Math.min(255, v | 0)).toString(16).padStart(2, "0")).join("");
  const fromHex = (h: string): Rgb => [parseInt(h.slice(1, 3), 16), parseInt(h.slice(3, 5), 16), parseInt(h.slice(5, 7), 16)];
  const byte = (v: string) => Math.max(0, Math.min(255, Math.round(+v || 0)));
  const fileName = (p: string) => p.split(/[\\/]/).pop() ?? p;
</script>

{#if modelDialog.open}
  <div class="backdrop" role="presentation">
    <div class="dialog" role="dialog" aria-label="3D Model Properties" tabindex="-1" onkeydown={keys} data-automation-id="model-properties">
      <header>
        <span>3D Model Properties</span>
        <button class="x" aria-label="Close" title="Close (nothing changes)" onclick={close}>✕</button>
      </header>
      <div class="body">
        <div class="left">
          <div class="field">
            <label for="model-name">Name:</label>
            <input id="model-name" type="text" bind:value={name} bind:this={nameInput} data-automation-id="model-name" />
          </div>
          <div class="field">
            <span class="lbl">Path:</span>
            <input type="text" value={source.path} readonly title={source.path} aria-label="The model's file" />
          </div>
          <div class="viewport">
            <canvas bind:this={canvas} onpointerdown={drag} onwheel={wheel} class:moving={tool === "move"} aria-label="Preview: drag to turn it, Shift-drag or the move tool to move it, the wheel to zoom" data-automation-id="model-preview"></canvas>
            <button class="reset" onclick={() => view?.reset()} title="Back to the first view" data-automation-id="model-preview-reset">Reset</button>
            <div class="tools">
              <button class:on={tool === "orbit"} aria-pressed={tool === "orbit"} title="Turn: drag to turn the model round" aria-label="Turn" onclick={() => (tool = "orbit")}>
                <svg viewBox="0 0 24 24" width="22" height="22" aria-hidden="true"><path d="M12 3v12M8 7l4-4 4 4" fill="none" stroke="currentColor" stroke-width="1.8" /><ellipse cx="12" cy="17" rx="8" ry="3" fill="none" stroke="currentColor" stroke-width="1.6" /></svg>
              </button>
              <button class:on={tool === "move"} aria-pressed={tool === "move"} title="Move: drag to move the model in the preview" aria-label="Move" onclick={() => (tool = "move")}>
                <svg viewBox="0 0 24 24" width="22" height="22" aria-hidden="true"><path d="M12 2v20M2 12h20M12 2l-3 3M12 2l3 3M12 22l-3-3M12 22l3-3M2 12l3-3M2 12l3 3M22 12l-3-3M22 12l-3 3" fill="none" stroke="currentColor" stroke-width="1.8" /></svg>
              </button>
            </div>
            {#if loading}<div class="note over">Reading the model…</div>{/if}
            {#if error}<div class="note over warn">{error}</div>{/if}
            {#if triangles}<div class="count">{triangles.toLocaleString()} triangles · {parts.length} part{parts.length === 1 ? "" : "s"}</div>{/if}
          </div>
        </div>
        <div class="right">
          <div class="tabs" role="tablist">
            {#each [["groups", "Groups"], ["materials", "Materials"], ["advanced", "Advanced"]] as [k, label] (k)}
              <button role="tab" aria-selected={tab === k} class:on={tab === k} onclick={() => (tab = k as typeof tab)} data-automation-id="model-tab-{k}">{label}</button>
            {/each}
          </div>
          {#if tab !== "advanced"}
            <input class="search" type="text" placeholder={tab === "groups" ? "Search in Groups" : "Search in Materials"} bind:value={search} aria-label="Search parts" />
          {/if}
          <div class="pane">
            {#if tab === "groups"}
              <div class="row-btns">
                <button class="small" onclick={() => (source.hidden = [])}>Show all</button>
                <button class="small" onclick={() => (source.hidden = parts.map((p) => p.name))}>Hide all</button>
              </div>
              {#each shown as p (p.name)}
                <label class="group">
                  <input type="checkbox" checked={!(source.hidden ?? []).includes(p.name)} onchange={(e) => toggleHidden(p.name, e.currentTarget.checked)} />
                  <span class="chip" style="background:{hex(ownColour(p.name) ?? p.color)}"></span>
                  <span class="pname" title={p.name}>{p.name}</span>
                </label>
              {/each}
              {#if !shown.length}<p class="note">{parts.length ? "No parts match." : "No parts yet."}</p>{/if}
            {:else if tab === "materials"}
              {#each shown as p, i (p.name)}
                {@const l = look(p.name)}
                {@const own = ownColour(p.name)}
                {@const isOpen = open[p.name] ?? (i === 0 || shown.length <= 2)}
                <div class="mat">
                  <button class="mat-head" aria-expanded={isOpen} onclick={() => (open[p.name] = !isOpen)}>
                    <span class="tri" class:down={isOpen}>▸</span>
                    <span class="chip" style="background:{hex(own ?? p.color)}"></span>
                    <span class="pname" title={p.name}>{p.name}</span>
                  </button>
                  {#if isOpen}
                    <div class="props">
                      <span class="lbl" title="A picture wrapped on it by the model's texture coordinates, instead of its colour or the file's texture">Diffuse Map</span>
                      <div class="pathrow">
                        <span class="path" class:none={!l.diffuse_map} title={l.diffuse_map ?? ""}>{l.diffuse_map ? fileName(l.diffuse_map) : p.textured ? "As in file" : "No Path"}</span>
                        <button class="icon" title="Choose a picture" aria-label="Choose a picture for {p.name}" onclick={() => chooseMap(p.name)}>📂</button>
                        <button class="icon" title="No picture of its own" aria-label="Clear the picture" disabled={!l.diffuse_map} onclick={() => setLook(p.name, (x) => (x.diffuse_map = null))}>✕</button>
                      </div>

                      <span class="lbl" title="Its colour (the file's until changed)">Diffuse Color</span>
                      {@render rgb(own ?? p.color, (c) => setColour(p.name, c), `${p.name}'s colour`, own ? () => setColour(p.name, null) : null)}

                      <span class="lbl" title="The highlights' colour (black: none)">Specular Color</span>
                      {@render rgb(l.specular, (c) => setLook(p.name, (x) => (x.specular = c)), `${p.name}'s highlights`, null)}

                      <span class="lbl" title="Light of its own, whatever lights it">Emissive Color</span>
                      {@render rgb(l.emissive, (c) => setLook(p.name, (x) => (x.emissive = c)), `${p.name}'s own light`, null)}

                      <span class="lbl" title="How tight the highlights are">Shininess</span>
                      <div class="slider">
                        <input type="range" min="1" max="200" step="0.1" value={l.shininess} oninput={(e) => setLook(p.name, (x) => (x.shininess = +e.currentTarget.value))} aria-label="Shininess of {p.name}" />
                        <span class="val">{l.shininess.toFixed(1)}</span>
                      </div>

                      <span class="lbl">Opacity</span>
                      <div class="slider">
                        <input type="range" min="0" max="100" step="0.1" value={l.opacity} oninput={(e) => setLook(p.name, (x) => (x.opacity = +e.currentTarget.value))} aria-label="Opacity of {p.name}" />
                        <span class="val">{l.opacity.toFixed(1)}%</span>
                      </div>
                    </div>
                  {/if}
                </div>
              {/each}
              {#if !shown.length}<p class="note">{parts.length ? "No materials match." : "No materials yet."}</p>{/if}
            {:else}
              <div class="adv">
                <span class="lbl">Up is</span>
                <div class="seg" role="radiogroup" aria-label="Which of the file's axes is up">
                  <button role="radio" aria-checked={!source.z_up} class:on={!source.z_up} onclick={() => (source.z_up = false)} title="Y up: glTF, OBJ, FBX, Maya, most game engines">Y</button>
                  <button role="radio" aria-checked={!!source.z_up} class:on={!!source.z_up} onclick={() => (source.z_up = true)} title="Z up: STL, 3MF, CAD, 3D printing, Blender's own axes">Z</button>
                </div>
                <span class="lbl">Colours</span>
                <label class="check" title="The file's colours and textures, with the Materials tab's changes. Untick for one colour (set in Controls)."><input type="checkbox" checked={source.own_colours !== false} onchange={(e) => (source.own_colours = e.currentTarget.checked)} /> Its own (materials and textures)</label>
                <span class="lbl">Texture folders</span>
                <div class="dirs">
                  {#each source.texture_dirs ?? [] as d (d)}
                    <div class="dir"><span title={d}>{d}</span><button class="icon" aria-label="Stop looking in {d}" onclick={() => (source.texture_dirs = (source.texture_dirs ?? []).filter((x) => x !== d))}>✕</button></div>
                  {/each}
                  <button class="small" onclick={importMaps}>Add folder…</button>
                </div>
              </div>
              {#if missing.length}
                <p class="warn" title={missing.join("\n")}>{missing.length} texture{missing.length === 1 ? "" : "s"} not found: {missing.slice(0, 3).map(fileName).join(", ")}{missing.length > 3 ? "…" : ""}. Import Maps to say where they are.</p>
              {/if}
              <p class="note">Move it, turn it and size it on the timeline afterwards (it's a 3D layer); light it with lights, and it casts and takes shadows.</p>
            {/if}
          </div>
        </div>
      </div>
      <footer>
        <button onclick={close} data-automation-id="model-cancel">Cancel</button>
        <span class="grow"></span>
        <button onclick={importMaps} title="Choose a folder to find its texture files in (by name, subfolders too)" data-automation-id="model-import-maps">Import Maps</button>
        <button class="primary" onclick={ok} data-automation-id="model-ok">OK</button>
      </footer>
    </div>
  </div>
{/if}

{#snippet rgb(c: Rgb, set: (c: Rgb) => void, title: string, reset: (() => void) | null)}
  <div class="rgb">
    {#each [0, 1, 2] as k (k)}
      <input type="number" min="0" max="255" value={c[k]} aria-label="{title}: {['red', 'green', 'blue'][k]}" onchange={(e) => set(c.map((v, i) => (i === k ? byte(e.currentTarget.value) : v)) as Rgb)} />
    {/each}
    <button class="swatch" style="background:{hex(c)}" title="Pick a colour" aria-label="Pick {title}" onclick={(e) => pickColor(hex(c), (h) => set(fromHex(h)), e.currentTarget, title)}></button>
    {#if reset}<button class="icon" title="Back to the file's colour" aria-label="Back to the file's colour" onclick={reset}>↺</button>{:else}<span class="icon-gap"></span>{/if}
  </div>
{/snippet}

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    z-index: 975;
    display: grid;
    place-items: center;
    background: rgb(0 0 0 / 40%);
  }
  .dialog {
    width: min(1060px, 96vw);
    height: min(700px, 94vh);
    display: flex;
    flex-direction: column;
    background: var(--panel);
    border: 1px solid var(--line);
    border-radius: var(--radius);
    box-shadow: 0 14px 44px rgb(0 0 0 / 55%);
  }
  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 10px 12px 10px 16px;
    font-size: 14px;
  }
  .x {
    background: none;
    border: 0;
    font-size: 15px;
    padding: 2px 6px;
  }
  .body {
    flex: 1;
    min-height: 0;
    display: grid;
    grid-template-columns: minmax(0, 1fr) minmax(0, 1fr);
    gap: 10px;
    padding: 4px 12px 0;
  }
  .left {
    display: flex;
    flex-direction: column;
    gap: 6px;
    min-height: 0;
  }
  .field {
    display: grid;
    grid-template-columns: 56px 1fr;
    align-items: center;
    gap: 8px;
  }
  .field label,
  .field .lbl {
    color: var(--muted);
    text-align: right;
  }
  .field input[readonly] {
    color: var(--muted);
  }
  .viewport {
    position: relative;
    flex: 1;
    min-height: 200px;
    background: #000;
    border-radius: 3px;
    overflow: hidden;
  }
  canvas {
    width: 100%;
    height: 100%;
    display: block;
    cursor: grab;
    touch-action: none;
  }
  canvas.moving {
    cursor: move;
  }
  .reset {
    position: absolute;
    top: 0;
    left: 0;
    border-radius: 0 0 3px 0;
  }
  .tools {
    position: absolute;
    top: 0;
    right: 0;
    display: flex;
    flex-direction: column;
    background: rgb(40 40 40 / 85%);
    border-radius: 0 0 0 3px;
  }
  .tools button {
    background: none;
    border: 0;
    padding: 6px;
    color: var(--muted);
  }
  .tools button.on {
    color: var(--accent);
  }
  .note {
    color: var(--muted);
    margin: 6px 0;
  }
  .over {
    position: absolute;
    left: 10px;
    bottom: 8px;
    margin: 0;
  }
  .count {
    position: absolute;
    right: 10px;
    bottom: 8px;
    color: #888;
    font-size: 11px;
  }
  .warn {
    color: var(--red);
  }
  .right {
    display: flex;
    flex-direction: column;
    min-height: 0;
    background: var(--base);
    border: 1px solid var(--line);
    border-radius: 3px;
  }
  .tabs {
    display: flex;
    border-bottom: 1px solid var(--line);
  }
  .tabs button {
    background: none;
    border: 0;
    border-radius: 0;
    padding: 9px 16px;
    color: var(--muted);
  }
  .tabs button.on {
    color: var(--text);
    background: var(--panel);
  }
  .search {
    margin: 8px 10px 4px;
  }
  .pane {
    flex: 1;
    min-height: 0;
    overflow: auto;
    padding: 4px 10px 10px;
  }
  .row-btns {
    display: flex;
    gap: 6px;
    margin: 4px 0 6px;
  }
  .group {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 4px 2px;
  }
  .chip {
    width: 14px;
    height: 14px;
    flex: none;
    border: 1px solid rgb(0 0 0 / 50%);
    border-radius: 2px;
  }
  .pname {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .mat-head {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    background: none;
    border: 0;
    padding: 6px 2px;
    text-align: left;
  }
  .tri {
    display: inline-block;
    color: var(--muted);
    transition: transform 0.1s;
  }
  .tri.down {
    transform: rotate(90deg);
  }
  .props {
    display: grid;
    grid-template-columns: 120px minmax(0, 1fr);
    gap: 6px 10px;
    align-items: center;
    padding: 2px 0 10px 22px;
  }
  .props .lbl,
  .adv .lbl {
    color: var(--text);
  }
  .pathrow,
  .rgb,
  .slider {
    display: flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
  }
  .path {
    flex: 1;
    min-width: 0;
    padding: 3px 6px;
    background: var(--panel);
    border-radius: 2px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .path.none {
    color: var(--muted);
  }
  .icon {
    background: none;
    border: 0;
    padding: 2px 5px;
  }
  .icon-gap {
    width: 24px;
  }
  .rgb input[type="number"] {
    width: 52px;
    text-align: right;
  }
  .swatch {
    width: 26px;
    height: 22px;
    padding: 0;
    border: 1px solid var(--line);
    border-radius: 2px;
  }
  .slider input {
    flex: 1;
    min-width: 0;
  }
  .val {
    width: 52px;
    text-align: right;
    font-variant-numeric: tabular-nums;
  }
  .adv {
    display: grid;
    grid-template-columns: 120px 1fr;
    gap: 10px;
    align-items: center;
    padding: 10px 0;
  }
  .seg {
    display: flex;
  }
  .seg button {
    min-width: 36px;
  }
  .seg button.on {
    background: var(--accent);
    color: #fff;
  }
  .dirs {
    display: flex;
    flex-direction: column;
    gap: 4px;
    align-items: flex-start;
    min-width: 0;
  }
  .dir {
    display: flex;
    align-items: center;
    gap: 4px;
    max-width: 100%;
  }
  .dir span {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  footer {
    display: flex;
    gap: 8px;
    padding: 12px;
  }
  .grow {
    flex: 1;
  }
  footer button {
    min-width: 76px;
  }
</style>
