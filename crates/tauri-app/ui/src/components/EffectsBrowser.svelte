<script lang="ts">
  // The effects browser (E, or Browse… in the Effects section): every
  // built-in effect and installed plugin by category, with a search box.
  // Add puts the effect at the end of the selected clip's stack.
  import { app, run } from "../lib/session.svelte";
  import { api, type CatalogGroup, type CatalogItem, type TransitionKind } from "../lib/api";
  import { ask } from "../lib/actions.svelte";
  import PresetList from "./PresetList.svelte";
  import { favourites, isFav, toggleFav } from "../lib/favourites.svelte";
  import { menu } from "../lib/menu.svelte";

  let groups = $state<CatalogGroup[]>([]);
  let search = $state("");
  let input = $state<HTMLInputElement | null>(null);
  let added = $state("");
  let tab = $state<"effects" | "presets" | "transitions">("effects");
  function savePresetFrom() {
    if (ins) app.savePreset = { clip: ins.id };
  }
  let kinds = $state<TransitionKind[]>([]);
  $effect(() => {
    if (app.effectsOpen && !kinds.length) api.transitionList().then((k) => (kinds = k));
  });
  // A transition goes into the selected clip (from the clip just before it), or
  // onto the video cut nearest the playhead.
  const into = $derived(app.s?.inspector?.transition_in?.kind ? app.s.inspector : null);
  const tShown = $derived.by(() => {
    const groups: { label: string; items: TransitionKind[] }[] = [];
    for (const k of kinds.filter((k) => !q || [k.label, k.description, k.group].some((x) => x.toLowerCase().includes(q)))) {
      let g = groups.find((x) => x.label === k.group);
      if (!g) groups.push((g = { label: k.group, items: [] }));
      g.items.push(k);
    }
    return groups;
  });
  async function applyT(k: TransitionKind) {
    await run(api.transitionApply(k.kind, into ? into.id : null));
    added = k.label;
    setTimeout(() => (added === k.label ? (added = "") : null), 2500);
  }

  // Reload the list when the window opens and whenever the plugins change.
  $effect(() => {
    if (app.effectsOpen) {
      void app.live?.plugins_rev;
      api.effectCatalog().then((g) => (groups = g));
    }
  });
  $effect(() => {
    if (app.effectsOpen && input) input.focus();
  });

  const ins = $derived(app.s?.inspector ?? null);
  const target = $derived(ins?.visual ? ins : null);
  const q = $derived(search.trim().toLowerCase());
  const hit = (g: CatalogGroup, it: CatalogItem) =>
    !q || [it.label, it.description, g.label, it.author, it.plugin ? "plugin" : ""].some((s) => s.toLowerCase().includes(q));
  const shown = $derived.by(() => {
    const list = groups.map((g) => ({ ...g, items: g.items.filter((it) => hit(g, it)) })).filter((g) => g.items.length);
    // Starred ones first (as well as in their own group).
    const favs = favourites(list.flatMap((g) => g.items));
    return favs.length ? [{ label: "Favourites", items: favs }, ...list] : list;
  });
  const plugins = $derived(groups.reduce((n, g) => n + g.items.filter((i) => i.plugin).length, 0));

  async function add(it: CatalogItem) {
    if (!target) return;
    await run(api.effectAdd(target.id, it.kind, it.plugin));
    added = it.label;
    setTimeout(() => (added === it.label ? (added = "") : null), 2500);
  }

  function key(e: KeyboardEvent) {
    if (app.effectsOpen && !ask.open && !app.savePreset && !menu.open && e.key === "Escape") {
      e.stopPropagation();
      app.effectsOpen = false;
    }
  }
</script>

<svelte:window onkeydown={key} />

{#if app.effectsOpen}
  <div class="scrim" role="presentation" onpointerdown={(e) => e.target === e.currentTarget && (app.effectsOpen = false)}>
    <div class="dialog" role="dialog" aria-modal="true" aria-labelledby="fx-title">
      <header>
        <h2 id="fx-title" class="sr">Effects and transitions</h2>
        <div class="tabs" role="tablist">
          <button role="tab" aria-selected={tab === "effects"} class:active={tab === "effects"} onclick={() => (tab = "effects")}>Effects</button>
          <button role="tab" aria-selected={tab === "presets"} class:active={tab === "presets"} onclick={() => (tab = "presets")}>Presets</button>
          <button role="tab" aria-selected={tab === "transitions"} class:active={tab === "transitions"} onclick={() => (tab = "transitions")}>Transitions</button>
        </div>
        <span class="for">
          {#if tab === "presets"}
            Saved looks, title styles, audio levels and transitions
          {:else if tab === "transitions"}
            {#if into}Into <b>{into.name}</b> from the clip before it{:else}On the video cut nearest the playhead{/if}
          {:else if target}Adding to <b>{target.name}</b>{:else}<span class="warn">Select a video, picture or title clip to add effects to it</span>{/if}
        </span>
        <button class="ghost close" onclick={() => (app.effectsOpen = false)} aria-label="Close">✕</button>
      </header>
      <input bind:this={input} type="search" bind:value={search} placeholder={tab === "effects" ? "Search: blur, key, colour…" : tab === "presets" ? "Search presets and folders…" : "Search: dissolve, wipe, push…"} aria-label="Search effects" />
      <div class="list">
        {#if tab === "presets"}
          <PresetList {q} />
          <p class="muted foot">A preset is a clip's look saved to reuse: its effects, and if chosen its masks, a move and opacity, its blend mode, a title style, audio levels or the transition into it. Its keyframes land from the clip's start, before its end, or stretched to its length. Hover a preset to see it on the clip.
            {#if ins}<button class="link" onclick={savePresetFrom}>Save {ins.name}'s look as a preset…</button>{/if}</p>
        {:else if tab === "transitions"}
          {#each tShown as g}
            <h3>{g.label}</h3>
            {#each g.items as k}
              <div class="item">
                <div class="text">
                  <span class="name">{k.label}</span>
                  <span class="desc">{k.description}</span>
                </div>
                <button onclick={() => applyT(k)}>{added === k.label ? "Applied ✓" : "Apply"}</button>
              </div>
            {/each}
          {:else}
            <p class="muted">Nothing matches “{search}”.</p>
          {/each}
          <p class="muted foot">A transition plays over the cut between two clips that touch on a track (1 s to start with; drag its ends on the timeline to change it). Ctrl+D toggles a cross-dissolve at the playhead, Ctrl+Shift+D an audio crossfade.</p>
        {:else}
        {#each shown as g}
          <h3>{g.label}</h3>
          {#each g.items as it}
            <div class="item">
              <div class="text">
                <span class="name">{it.label}{#if it.plugin}<span class="tag">plugin</span>{/if}</span>
                <span class="desc">{[it.description, it.author && `by ${it.author}`, it.version].filter(Boolean).join(" · ")}</span>
              </div>
              <button class="star" class:on={isFav(it)} onclick={() => toggleFav(it)} aria-pressed={isFav(it)} aria-label={isFav(it) ? `Remove ${it.label} from favourites` : `Add ${it.label} to favourites`} title={isFav(it) ? "Remove from favourites" : "Add to favourites (listed first here and in the Effects panel)"}>{isFav(it) ? "★" : "☆"}</button>
              <button disabled={!target} onclick={() => add(it)}>{added === it.label ? "Added ✓" : "Add"}</button>
            </div>
          {/each}
        {:else}
          <p class="muted">Nothing matches “{search}”.</p>
        {/each}
        {/if}
      </div>
      <footer>
        <span class="muted">{plugins} plugin{plugins === 1 ? "" : "s"} installed</span>
        <button onclick={() => (app.pluginsOpen = true)}>Manage plugins…</button>
        <span class="grow"></span>
        <button class="primary" onclick={() => (app.effectsOpen = false)}>Done</button>
      </footer>
    </div>
  </div>
{/if}

<style>
  .sr {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip: rect(0 0 0 0);
  }
  .tabs {
    display: flex;
    gap: 2px;
  }
  .star {
    background: none;
    border: 0;
    padding: 0 6px;
    font-size: calc(15px * var(--text-scale));
    color: var(--faint);
  }
  .star.on {
    color: var(--accent);
  }
  .star:hover {
    color: var(--text);
  }
  .tabs button {
    background: none;
    border: 0;
    border-bottom: 2px solid transparent;
    border-radius: 0;
    color: var(--muted);
    padding: 2px 8px;
    font-weight: 600;
  }
  .tabs button.active {
    color: var(--text);
    border-bottom-color: var(--accent);
  }
  button.link {
    background: none;
    border: none;
    padding: 0;
    color: var(--accent);
    text-decoration: underline;
    cursor: pointer;
    font: inherit;
  }
  .foot {
    font-size: calc(12px * var(--text-scale));
    margin: 10px 2px 4px;
  }
  .scrim {
    position: fixed;
    inset: 0;
    background: rgba(10, 10, 12, 0.45);
    display: grid;
    place-items: center;
    z-index: 40;
  }
  .dialog {
    width: min(560px, calc(100vw - 32px));
    height: min(620px, calc(100vh - 40px));
    background: var(--panel);
    border: 1px solid var(--line);
    border-radius: 8px;
    box-shadow: 0 20px 60px rgba(0, 0, 0, 0.6);
    display: flex;
    flex-direction: column;
    gap: 10px;
    padding: 14px 16px;
  }
  header {
    display: flex;
    align-items: baseline;
    gap: 12px;
  }
  h2 {
    font-size: calc(16px * var(--text-scale));
    font-weight: 600;
    margin: 0;
  }
  .for {
    flex: 1;
    color: var(--muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .for b {
    color: var(--text);
    font-weight: 600;
  }
  .warn {
    color: #f0a050;
  }
  .close {
    align-self: center;
  }
  .list {
    flex: 1;
    overflow: auto;
    min-height: 0;
    margin: 0 -4px;
    padding: 0 4px;
  }
  h3 {
    font-size: calc(12px * var(--text-scale));
    font-weight: 600;
    color: var(--muted);
    text-transform: uppercase;
    letter-spacing: 0.04em;
    margin: 12px 0 6px;
  }
  h3:first-child {
    margin-top: 2px;
  }
  .item {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 7px 10px;
    border: 1px solid var(--line-soft);
    border-radius: 6px;
    background: var(--base);
    margin-bottom: 5px;
  }
  .item:hover {
    border-color: var(--line);
  }
  .text {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 1px;
  }
  .name {
    font-weight: 600;
  }
  .desc {
    color: var(--muted);
    font-size: calc(12px * var(--text-scale));
  }
  .tag {
    font-weight: 400;
    font-size: calc(10.5px * var(--text-scale));
    color: var(--muted);
    border: 1px solid var(--line);
    border-radius: 3px;
    padding: 0 4px;
    margin-left: 6px;
  }
  .item button {
    min-width: 72px;
  }
  footer {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .grow {
    flex: 1;
  }
  .muted {
    color: var(--muted);
  }
</style>
