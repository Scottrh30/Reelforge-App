<script lang="ts">
  // Save effects as preset…: name it, and choose what goes in: which of the
  // clip's effects, its masks, its movement and opacity, its blend mode, and
  // where the keyframes land on the clips it's applied to.
  import { app, run } from "../lib/session.svelte";
  import { api, type PresetTiming } from "../lib/api";

  const ins = $derived(app.savePreset && app.s?.inspector?.id === app.savePreset.clip ? app.s.inspector : null);
  // A camera's, light's or point's settings belong to that clip.
  const savable = $derived((ins?.effects ?? []).map((e, i) => ({ e, i })).filter(({ e }) => !["Camera", "Light", "Point"].includes(e.kind)));
  const moving = $derived((ins?.animated ?? []).length > 0);
  const blendName = $derived(ins?.visual?.blend ?? "Normal");

  let name = $state("");
  let chosen = $state<number[]>([]);
  let masks = $state(false);
  let motion = $state(false);
  let blend = $state(false);
  let timing = $state<PresetTiming>("FromStart");
  let folder = $state("");
  let folders = $state<string[]>([]);
  let extras = $state<{ title: boolean; audio: boolean; transition: string | null }>({ title: false, audio: false, transition: null });
  let title = $state(false);
  let audio = $state(false);
  let transition = $state(false);
  let input = $state<HTMLInputElement | null>(null);

  // Fresh choices each time it opens: every effect, the blend mode if it isn't Normal.
  let openedFor: number | null = null;
  $effect(() => {
    const id = app.savePreset?.clip ?? null;
    if (id === openedFor) return;
    openedFor = id;
    if (id === null || !ins) return;
    chosen = savable.map(({ i }) => i);
    name = savable.length === 1 ? savable[0].e.label : "My look";
    masks = false;
    motion = false;
    blend = blendName !== "Normal";
    timing = "FromStart";
    (title = false), (audio = false), (transition = false);
    extras = { title: false, audio: false, transition: null };
    api.fxPresetFolders().then((f) => (folders = f));
    api.fxPresetClipExtras(id).then((x) => {
      extras = x;
      // A title's style is what a title preset is usually for.
      title = x.title;
      if (x.title && !savable.length) name = "My title style";
      // An audio clip: its levels are what there is to save.
      if (x.audio && ins?.kind === "audio") {
        audio = true;
        name = "My audio levels";
      }
    });
    queueMicrotask(() => input?.select());
  });
  $effect(() => {
    if (app.savePreset && !ins) app.savePreset = null;
  });

  const anything = $derived(chosen.length > 0 || (masks && (ins?.masks.length ?? 0) > 0) || motion || blend || (title && extras.title) || (audio && extras.audio) || (transition && !!extras.transition));
  const toggle = (i: number) => (chosen = chosen.includes(i) ? chosen.filter((x) => x !== i) : [...chosen, i].sort((a, b) => a - b));

  async function save() {
    if (!ins || !name.trim() || !anything) return;
    const id = ins.id;
    app.savePreset = null;
    await run(api.fxPresetSave(id, name.trim(), { effects: chosen, masks, motion, blend, timing, title: title && extras.title, audio: audio && extras.audio, transition: transition && !!extras.transition, folder: folder.trim() }));
  }
  function key(e: KeyboardEvent) {
    if (!app.savePreset) return;
    if (e.key === "Escape") {
      e.stopImmediatePropagation();
      app.savePreset = null;
    }
  }
</script>

<svelte:window onkeydown={key} />

{#if app.savePreset && ins}
  <div class="scrim" role="presentation" onpointerdown={(e) => e.target === e.currentTarget && (app.savePreset = null)}>
    <div class="dialog" role="dialog" aria-modal="true" aria-labelledby="sp-title">
      <h2 id="sp-title">Save as preset</h2>
      <p class="muted">From <b>{ins.name}</b>. Apply it to other clips from + Add → Presets or the effects browser (E). A preset of the same name is replaced.</p>
      <label class="name">Name <input bind:this={input} bind:value={name} onkeydown={(e) => e.key === "Enter" && save()} /></label>
      <label class="name">Folder <input bind:value={folder} list="sp-folders" placeholder="None (type a name to make one)" /></label>
      <datalist id="sp-folders">{#each folders as f}<option value={f}></option>{/each}</datalist>

      {#if ins.kind !== "audio"}
      <fieldset>
        <legend>Effects</legend>
        {#each savable as { e, i } (i)}
          <label class="check"><input type="checkbox" checked={chosen.includes(i)} onchange={() => toggle(i)} /> {e.label}{#if !e.enabled}<span class="muted"> (switched off)</span>{/if}</label>
        {:else}
          <p class="muted">This clip has no effects.</p>
        {/each}
      </fieldset>

      {/if}
      <fieldset>
        <legend>{ins.kind === "audio" ? "Save" : "Also"}</legend>
        {#if ins.kind !== "audio"}
        <label class="check" class:dim={!ins.masks.length}><input type="checkbox" bind:checked={masks} disabled={!ins.masks.length} /> Masks{ins.masks.length ? ` (${ins.masks.length})` : " (none)"}<span class="muted"> · scaled to each clip's size</span></label>
        <label class="check"><input type="checkbox" bind:checked={motion} /> Position, scale, rotation and opacity{#if moving}<span class="muted"> · with their keyframes</span>{/if}</label>
        <label class="check"><input type="checkbox" bind:checked={blend} /> Blend mode<span class="muted"> · {blendName}</span></label>
        {/if}
        {#if extras.title}<label class="check"><input type="checkbox" bind:checked={title} /> Title style<span class="muted"> · font, size, colours, outline, shadow, background (not the words)</span></label>{/if}
        {#if extras.audio}<label class="check"><input type="checkbox" bind:checked={audio} /> Audio levels<span class="muted"> · volume, pan and fades, with their keyframes</span></label>{/if}
        {#if extras.transition}<label class="check"><input type="checkbox" bind:checked={transition} /> Transition into the clip<span class="muted"> · {extras.transition}</span></label>{/if}
      </fieldset>

      <fieldset>
        <legend>Keyframes land</legend>
        <label class="check"><input type="radio" name="sp-timing" value="FromStart" bind:group={timing} /> From the clip's start<span class="muted"> · a fade-in, an entrance</span></label>
        <label class="check"><input type="radio" name="sp-timing" value="FromEnd" bind:group={timing} /> Before the clip's end<span class="muted"> · a fade-out, an exit</span></label>
        <label class="check"><input type="radio" name="sp-timing" value="Stretch" bind:group={timing} /> Stretched to the clip's length<span class="muted"> · a move across the whole clip</span></label>
      </fieldset>

      <div class="buttons">
        <button onclick={() => (app.savePreset = null)}>Cancel</button>
        <button class="primary" disabled={!name.trim() || !anything} onclick={save} title={anything ? "" : "Choose at least one thing to save"}>Save preset</button>
      </div>
    </div>
  </div>
{/if}

<style>
  .scrim {
    position: fixed;
    inset: 0;
    background: rgba(10, 10, 12, 0.55);
    display: grid;
    place-items: center;
    z-index: 55;
  }
  .dialog {
    width: min(520px, calc(100vw - 32px));
    max-height: calc(100vh - 32px);
    overflow: auto;
    background: var(--panel);
    border: 1px solid var(--line);
    border-radius: 8px;
    padding: 18px 20px;
    box-shadow: 0 20px 60px rgba(0, 0, 0, 0.6);
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  h2 {
    font-size: calc(16px * var(--text-scale));
    font-weight: 600;
    margin: 0;
  }
  p {
    margin: 0;
    font-size: calc(12.5px * var(--text-scale));
  }
  .muted {
    color: var(--muted);
  }
  .name {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .name input {
    flex: 1;
  }
  .name {
    min-width: 0;
  }
  fieldset {
    border: 1px solid var(--line-soft);
    border-radius: 6px;
    padding: 6px 10px 8px;
    margin: 0;
    display: flex;
    flex-direction: column;
    gap: 4px;
    min-width: 0;
  }
  legend {
    font-size: calc(12px * var(--text-scale));
    color: var(--muted);
    padding: 0 4px;
  }
  .check {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .dim {
    opacity: 0.55;
  }
  .buttons {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 4px;
  }
</style>
