<script lang="ts">
  // A 3D model layer's settings: its file (and whether it could be read),
  // size, colours (its own, or one chosen), which way is up, and reading
  // the file again after it changed. One undo step each.
  import { run } from "../lib/session.svelte";
  import { api, type Inspector, type TitleSpec } from "../lib/api";
  import NumberField from "./NumberField.svelte";

  let { ins }: { ins: Inspector } = $props();
  type Rgba = [number, number, number, number];
  type Model = { path: string; z_up?: boolean; own_colours?: boolean; rev?: number; part_colours?: [string, [number, number, number]][] };
  let draft = $state<TitleSpec | null>(null);
  $effect(() => {
    void ins.id;
    draft = null;
  });
  const t = $derived((draft ?? ins.title!) as TitleSpec & Record<string, any>);
  const m = $derived(t.model as Model);
  const info = $derived(ins.model);
  const name = $derived(m.path.split(/[\\/]/).pop() ?? m.path);

  function set(f: (s: TitleSpec & Record<string, any>) => void) {
    const n = structuredClone($state.snapshot(t)) as TitleSpec & Record<string, any>;
    f(n);
    draft = n;
    api.setTitle(ins.id, n);
  }
  async function commit() {
    await run(api.commit());
    draft = null;
  }
  async function once(f: (s: TitleSpec & Record<string, any>) => void) {
    set(f);
    await commit();
  }
  const partColour = (name: string) => (m.part_colours ?? []).find((p) => p[0] === name)?.[1] ?? null;
  function setPart(name: string, c: [number, number, number] | null, live: boolean) {
    const f = (s: TitleSpec & Record<string, any>) => {
      const list: [string, [number, number, number]][] = (s.model.part_colours ?? []).filter((p: [string, unknown]) => p[0] !== name);
      if (c) list.push([name, c]);
      s.model.part_colours = list;
    };
    if (live) set(f);
    else once(f);
  }
  const hex = (c: Rgba) => "#" + c.slice(0, 3).map((v) => v.toString(16).padStart(2, "0")).join("");
  const rgba = (h: string, a: number): Rgba => [parseInt(h.slice(1, 3), 16), parseInt(h.slice(3, 5), 16), parseInt(h.slice(5, 7), 16), a];
</script>

<div class="group">
  <h4>3D model</h4>
  <p class="file" title={m.path}><b>{name}</b></p>
  {#if info?.error}
    <p class="warn">Can't show it: {info.error}</p>
    <div class="row btns"><button class="small" onclick={() => run(api.relinkModel(ins.id))} title="Find the model file where it is now (other missing models and textures in the same folder are found too)">Find file…</button></div>
  {:else if info}
    <p class="note">{info.triangles.toLocaleString()} triangles{info.textures ? `, ${info.textures} texture${info.textures === 1 ? "" : "s"}` : ""}{info.parts.length > 1 ? `, ${info.parts.length} parts` : ""}</p>
    {#if info.missing_textures?.length}
      <p class="warn" title={info.missing_textures.join("\n")}>{info.missing_textures.length} texture{info.missing_textures.length === 1 ? "" : "s"} not found: {info.missing_textures.slice(0, 3).map((t) => t.split(/[\\/]/).pop()).join(", ")}{info.missing_textures.length > 3 ? "…" : ""}</p>
      <div class="row btns"><button class="small" onclick={() => run(api.findModelTextures(ins.id))} title="Choose a folder to look for them in (and its subfolders), by name">Find textures…</button></div>
    {/if}
  {/if}
  <div class="grid">
    <span class="lbl" title="Its largest side, so it fits in a box this big">Size</span>
    <NumberField label="Model size: its largest side" value={t.size} min={4} max={20000} step={1} digits={0} suffix=" px" oninput={(v) => set((s) => (s.size = v))} oncommit={commit} />

    <span class="lbl">Colours</span>
    <div class="row">
      <label class="check" title="The colours and textures in the file (materials, vertex colours, texture maps). Untick to colour the whole model one colour."><input type="checkbox" checked={m.own_colours !== false} onchange={(e) => once((s) => (s.model.own_colours = e.currentTarget.checked))} /> Its own</label>
      <input type="color" value={hex(t.color as Rgba)} disabled={m.own_colours !== false} aria-label="Model colour" title={m.own_colours !== false ? "Untick “Its own” to choose one colour" : "The model's colour"} oninput={(e) => set((s) => (s.color = rgba(e.currentTarget.value, s.color[3])))} onchange={commit} />
    </div>

    <span class="lbl">Up is</span>
    <div class="seg" role="radiogroup" aria-label="Which of the file's axes is up">
      <button role="radio" aria-checked={!m.z_up} class:on={!m.z_up} onclick={() => once((s) => (s.model.z_up = false))} title="Y up: glTF, OBJ, FBX, Maya, most game engines">Y</button>
      <button role="radio" aria-checked={!!m.z_up} class:on={!!m.z_up} onclick={() => once((s) => (s.model.z_up = true))} title="Z up: STL, CAD, 3D printing, Blender's own axes">Z</button>
    </div>
  </div>
  {#if info && !info.error && m.own_colours !== false && info.parts.length}
    <div class="parts">
      <span class="lbl">Parts</span>
      {#each info.parts as p (p)}
        {@const c = partColour(p)}
        <div class="part">
          <span class="pname" title={p}>{p}</span>
          <input type="color" value={c ? hex([...c, 255] as Rgba) : "#999999"} class:own={!c} aria-label="Colour of {p}" title={c ? `${p}: its own colour` : `${p}: as in the file (choose a colour to paint it)`} oninput={(e) => setPart(p, rgba(e.currentTarget.value, 255).slice(0, 3) as [number, number, number], true)} onchange={commit} />
          {#if c}<button class="small ghost" onclick={() => setPart(p, null, false)} title="Back to the file's colour or texture">↺</button>{:else}<span class="as-file">as in file</span>{/if}
        </div>
      {/each}
    </div>
  {/if}
  <div class="row btns">
    <button class="small" onclick={() => once((s) => (s.model.rev = (s.model.rev ?? 0) + 1))} title="Read the file again (after changing it in another program)">Reload</button>
    <button class="small" onclick={() => run(api.openFolder(m.path.replace(/[\\/][^\\/]*$/, "")))} title="Show the file's folder">Show file</button>
  </div>
  <p class="note">Move it, turn it and push it back in Transform (it's a 3D layer); light it with lights (Timeline → Add light), and it casts and takes shadows.</p>
</div>

<style>
  .group {
    padding: 8px 0 12px;
    border-bottom: 1px solid var(--line-soft);
  }
  h4 {
    font-size: calc(12.5px * var(--text-scale));
    font-weight: 600;
    color: var(--muted);
    margin: 0 0 6px;
  }
  .file {
    margin: 0;
    overflow-wrap: anywhere;
  }
  .warn {
    color: var(--red);
    margin: 4px 0 0;
  }
  .note {
    margin: 4px 0 0;
    color: var(--muted);
    font-size: calc(11.5px * var(--text-scale));
    line-height: 1.4;
  }
  .grid {
    display: grid;
    grid-template-columns: 96px minmax(0, 1fr);
    gap: 6px;
    align-items: center;
    margin-top: 8px;
  }
  .lbl {
    color: var(--muted);
  }
  .row {
    display: flex;
    gap: 6px;
    align-items: center;
  }
  .btns {
    margin-top: 8px;
  }
  .parts {
    margin-top: 8px;
    display: grid;
    gap: 4px;
  }
  .part {
    display: grid;
    grid-template-columns: minmax(0, 1fr) 40px 44px;
    gap: 6px;
    align-items: center;
  }
  .pname {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .as-file {
    color: var(--faint);
    font-size: calc(10.5px * var(--text-scale));
  }
  input[type="color"].own {
    opacity: 0.45;
  }
  .check {
    display: flex;
    gap: 5px;
    align-items: center;
    color: var(--muted);
  }
  .seg {
    display: flex;
  }
  .seg button {
    flex: 1;
    min-height: 22px;
  }
  .seg button.on {
    background: var(--accent);
    color: #111;
  }
  input[type="color"] {
    width: 40px;
    height: 24px;
    padding: 0;
    border: 1px solid var(--line);
    background: none;
  }
</style>
