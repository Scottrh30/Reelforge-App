<script lang="ts">
  // Edit > Preferences: label (clip) colours, interface colours, button
  // colours and fonts. Changes show at once; Cancel puts back what was there.
  import { untrack } from "svelte";
  import { api } from "../lib/api";
  import { app, run } from "../lib/session.svelte";
  import { look, DEFAULTS, UI_SCHEMES, LABEL_SETS, PLAYHEAD_SWATCHES, prefsDialog, save, snapshot, restore, type Look } from "../lib/prefs.svelte";
  import ColorRow from "./ColorRow.svelte";

  const pages: [typeof prefsDialog.page, string][] = [
    ["labels", "Label colors"],
    ["ui", "Interface colors"],
    ["buttons", "Button colors"],
    ["fonts", "Fonts"],
  ];

  // What was there when the window opened, for Cancel.
  let before: Look | null = null;
  let picsBefore: boolean | null = null;
  $effect(() => {
    if (prefsDialog.open)
      untrack(() => {
        before = snapshot();
        picsBefore = app.s?.prefs.timeline_thumbnails !== false;
      });
  });

  // How clips look on the timeline: a solid colour, or pictures from the
  // clip (a filmstrip or its first frame, as Settings has it). It's a
  // setting kept with the others, so it shows at once.
  const pics = $derived(app.s?.prefs.timeline_thumbnails !== false);
  async function setPics(on: boolean) {
    const p = app.s?.prefs;
    if (!p || (p.timeline_thumbnails !== false) === on) return;
    await run(api.setPrefs({ ...$state.snapshot(p), timeline_thumbnails: on }));
  }

  function close(keep: boolean) {
    if (keep) save();
    else {
      if (before) restore(before);
      if (picsBefore !== null) setPics(picsBefore);
    }
    prefsDialog.open = false;
  }
  function resetPage() {
    const p = prefsDialog.page;
    // (The Label colours page has the playhead's colour too.)
    const extra = p === "labels" ? { timeline: structuredClone(DEFAULTS.timeline) } : {};
    restore({ ...snapshot(), [p]: structuredClone(DEFAULTS[p]), ...extra });
  }
  function resetAll() {
    restore(structuredClone(DEFAULTS));
  }

  // Installed fonts, asked for once.
  let families = $state<string[]>([]);
  let fontFilter = $state("");
  $effect(() => {
    if (prefsDialog.open && prefsDialog.page === "fonts" && families.length === 0) api.fontFamilies().then((f) => (families = f));
  });
  const shown = $derived(families.filter((f) => f.toLowerCase().includes(fontFilter.trim().toLowerCase())));
  const scheme = $derived(UI_SCHEMES.find((s) => JSON.stringify(s.ui) === JSON.stringify(look.ui))?.name ?? "");
  const labelSet = $derived(LABEL_SETS.find((s) => JSON.stringify(s.labels) === JSON.stringify(look.labels))?.name ?? "");

  function key(e: KeyboardEvent) {
    if (prefsDialog.open && e.key === "Escape") {
      e.stopPropagation();
      close(false);
    }
  }
</script>

<svelte:window onkeydown={key} />

{#if prefsDialog.open}
  <div class="scrim">
    <div class="dialog" role="dialog" aria-modal="true" aria-labelledby="prefs-title">
      <h2 id="prefs-title">Preferences</h2>
      <div class="body">
        <nav class="pages" aria-label="Preference pages">
          {#each pages as [id, name]}
            <button class:active={prefsDialog.page === id} aria-current={prefsDialog.page === id ? "page" : undefined} onclick={() => (prefsDialog.page = id)}>{name}</button>
          {/each}
        </nav>

        <section class="page">
          {#if prefsDialog.page === "labels"}
            <p class="lead">The colours of clips on the timeline, by kind, and of their names.</p>
            <div class="pick">
              <span>Clips show</span>
              <div class="seg" role="radiogroup" aria-label="How clips look on the timeline">
                <button role="radio" aria-checked={!pics} class:on={!pics} onclick={() => setPics(false)} title="Each clip filled with its label colour, no pictures">Solid color</button>
                <button role="radio" aria-checked={pics} class:on={pics} onclick={() => setPics(true)} title="Pictures from the clip behind its name: a filmstrip along it, or just its first frame (Settings, Clip pictures, chooses which)">Pictures from the clip</button>
              </div>
            </div>
            <label class="pick">
              <span>Color set</span>
              <select
                value={labelSet}
                onchange={(e) => {
                  const s = LABEL_SETS.find((x) => x.name === e.currentTarget.value);
                  if (s) Object.assign(look.labels, structuredClone(s.labels));
                }}
              >
                {#if !labelSet}<option value="">Custom</option>{/if}
                {#each LABEL_SETS as s}<option value={s.name}>{s.name}</option>{/each}
              </select>
            </label>
            <ColorRow label="Video" bind:value={look.labels.video} />
            <ColorRow label="Pictures" hint="Still images" bind:value={look.labels.image} />
            <ColorRow label="Audio" bind:value={look.labels.audio} />
            <ColorRow label="Titles" bind:value={look.labels.title} />
            <ColorRow label="Compositions" bind:value={look.labels.comp} />
            <ColorRow label="Clip names" hint="Text on the clips" bind:value={look.labels.text} />
            <div class="sample clips" aria-hidden="true">
              <span style="background:var(--clip-video)">footage.mp4</span>
              <span style="background:var(--clip-image)">logo.png</span>
              <span style="background:var(--clip-title)">Opening title</span>
              <span style="background:var(--clip-comp)">Comp · sign</span>
              <span style="background:var(--clip-audio)">music.wav</span>
            </div>
            <ColorRow label="Playhead" hint="Its head and line on the timeline" bind:value={look.timeline.playhead} />
            <div class="swatches" role="group" aria-label="Playhead colours">
              {#each PLAYHEAD_SWATCHES as [name, c]}
                <button class="swatch" class:on={look.timeline.playhead.toLowerCase() === c} style="background:{c}" title={name} aria-label="Playhead {name.toLowerCase()}" onclick={() => (look.timeline.playhead = c)}></button>
              {/each}
            </div>
          {:else if prefsDialog.page === "ui"}
            <p class="lead">The colours of the panels, text and highlights.</p>
            <label class="pick">
              <span>Scheme</span>
              <select
                value={scheme}
                onchange={(e) => {
                  const s = UI_SCHEMES.find((x) => x.name === e.currentTarget.value);
                  if (s) Object.assign(look.ui, structuredClone(s.ui));
                }}
              >
                {#if !scheme}<option value="">Custom</option>{/if}
                {#each UI_SCHEMES as s}<option value={s.name}>{s.name}</option>{/each}
              </select>
            </label>
            <ColorRow label="Background" hint="Behind the panels" bind:value={look.ui.base} />
            <ColorRow label="Panels" bind:value={look.ui.panel} />
            <ColorRow label="Raised" hint="Menus, cards, controls" bind:value={look.ui.raised} />
            <ColorRow label="Wells" hint="Around the picture, under the tracks" bind:value={look.ui.well} />
            <ColorRow label="Lines" hint="Borders and dividers" bind:value={look.ui.line} />
            <ColorRow label="Text" bind:value={look.ui.text} />
            <ColorRow label="Quiet text" hint="Labels and hints" bind:value={look.ui.muted} />
            <ColorRow label="Highlight" hint="Selection, the active tool, values" bind:value={look.ui.accent} />
          {:else if prefsDialog.page === "buttons"}
            <p class="lead">Ordinary buttons, and the main button of a window (like Export).</p>
            <ColorRow label="Button" bind:value={look.buttons.bg} />
            <ColorRow label="Button text" bind:value={look.buttons.text} />
            <ColorRow label="Button, pointed at" bind:value={look.buttons.hover} />
            <ColorRow label="Button border" bind:value={look.buttons.border} />
            <ColorRow label="Main button" bind:value={look.buttons.primary} />
            <ColorRow label="Main button text" bind:value={look.buttons.primaryText} />
            <label class="pick">
              <span>Corners</span>
              <div class="slider">
                <input type="range" min="0" max="12" step="1" bind:value={look.buttons.radius} aria-label="Corner roundness" />
                <span class="tc">{look.buttons.radius} px</span>
              </div>
            </label>
            <div class="sample buttons">
              <button>Import…</button>
              <button class="on">Snap</button>
              <button disabled>Redo</button>
              <button class="primary">Export</button>
            </div>
          {:else}
            <p class="lead">The typeface and size of the interface's text.</p>
            <label class="pick">
              <span>Interface font</span>
              <select bind:value={look.fonts.ui}>
                <option value="">IBM Plex Sans (built in)</option>
                {#if look.fonts.ui && !families.includes(look.fonts.ui)}<option value={look.fonts.ui}>{look.fonts.ui}</option>{/if}
                {#each shown as f}<option value={f}>{f}</option>{/each}
              </select>
            </label>
            <label class="pick">
              <span>Numbers font</span>
              <select bind:value={look.fonts.numbers}>
                <option value="">Same as the interface</option>
                {#if look.fonts.numbers && !families.includes(look.fonts.numbers)}<option value={look.fonts.numbers}>{look.fonts.numbers}</option>{/if}
                {#each shown as f}<option value={f}>{f}</option>{/each}
              </select>
            </label>
            <label class="pick">
              <span>Find a font</span>
              <input type="search" bind:value={fontFilter} placeholder={families.length ? `${families.length} installed` : "Looking for fonts…"} />
            </label>
            <label class="pick">
              <span>Text size</span>
              <div class="slider">
                <input type="range" min="0.8" max="1.4" step="0.05" bind:value={look.fonts.scale} aria-label="Text size" />
                <span class="tc">{Math.round(look.fonts.scale * 100)}%</span>
              </div>
            </label>
            <div class="sample text">
              <p>The quick brown fox jumps over the lazy dog.</p>
              <p class="muted">Drag clips from the Media panel to the timeline.</p>
              <p class="tc big">00:01:23:12</p>
            </div>
          {/if}
        </section>
      </div>
      <div class="foot">
        <button onclick={resetPage}>Reset this page</button>
        <button onclick={resetAll}>Reset all</button>
        <span class="grow"></span>
        <button onclick={() => close(false)}>Cancel</button>
        <button class="primary" onclick={() => close(true)}>Save</button>
      </div>
    </div>
  </div>
{/if}

<style>
  .scrim {
    position: fixed;
    inset: 0;
    background: rgba(10, 10, 12, 0.45);
    display: grid;
    place-items: center;
    z-index: 45;
  }
  .dialog {
    width: min(660px, calc(100vw - 32px));
    max-height: calc(100vh - 40px);
    background: var(--panel);
    border: 1px solid var(--line);
    border-radius: 8px;
    box-shadow: 0 20px 60px rgba(0, 0, 0, 0.6);
    display: flex;
    flex-direction: column;
  }
  h2 {
    font-size: calc(16px * var(--text-scale));
    font-weight: 600;
    margin: 0;
    padding: 16px 20px 10px;
  }
  .body {
    display: flex;
    min-height: 0;
    flex: 1;
    border-top: 1px solid var(--line-soft);
    border-bottom: 1px solid var(--line-soft);
  }
  .pages {
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: 10px 8px;
    min-width: 150px;
    border-right: 1px solid var(--line-soft);
  }
  .pages button {
    text-align: left;
    background: none;
    border: 0;
    border-left: 2px solid transparent;
    border-radius: 0;
    color: var(--muted);
    padding: 6px 10px;
  }
  .pages button:hover {
    color: var(--text);
    background: var(--raised);
  }
  .pages button.active {
    color: var(--text);
    border-left-color: var(--accent);
    background: var(--raised);
  }
  .page {
    flex: 1;
    overflow: auto;
    padding: 12px 18px 16px;
    /* The same height on every page, so the window doesn't jump. */
    height: min(calc(400px * var(--text-scale)), calc(100vh - 160px));
  }
  .lead {
    color: var(--muted);
    margin: 0 0 10px;
  }
  .pick {
    display: grid;
    grid-template-columns: 120px 1fr;
    align-items: center;
    gap: 8px;
    padding: 4px 0 8px;
  }
  .seg {
    display: inline-flex;
    justify-self: start;
    border: 1px solid var(--btn-border);
    border-radius: var(--btn-radius, 4px);
    overflow: hidden;
  }
  .seg button {
    border: none;
    border-radius: 0;
    padding: 4px 12px;
    background: var(--btn);
    color: var(--muted);
  }
  .seg button + button {
    border-left: 1px solid var(--btn-border);
  }
  .seg button.on {
    background: var(--accent);
    color: var(--base, #111);
  }
  .slider {
    display: flex;
    align-items: center;
    gap: 10px;
  }
  .slider input {
    flex: 1;
  }
  .swatches {
    display: flex;
    gap: 6px;
    margin: 2px 0 10px;
  }
  .swatch {
    width: 22px;
    height: 22px;
    min-height: 0;
    padding: 0;
    border-radius: 50%;
    border: 2px solid var(--line);
  }
  .swatch.on {
    border-color: var(--text);
    box-shadow: 0 0 0 2px var(--panel), 0 0 0 3px var(--text);
  }
  .sample {
    margin-top: 14px;
    padding: 10px;
    border-radius: 6px;
    background: var(--well);
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .clips span {
    color: var(--clip-text);
    border-radius: 4px;
    padding: 6px 10px;
    font-size: calc(12px * var(--text-scale));
    border: 1px solid rgba(255, 255, 255, 0.12);
    text-shadow: 0 1px 2px rgba(0, 0, 0, 0.4);
  }
  .text {
    flex-direction: column;
  }
  .text p {
    margin: 0;
  }
  .big {
    font-size: calc(21px * var(--text-scale));
    color: var(--text);
  }
  .foot {
    display: flex;
    gap: 8px;
    padding: 12px 20px 16px;
  }
  .grow {
    flex: 1;
  }
</style>
