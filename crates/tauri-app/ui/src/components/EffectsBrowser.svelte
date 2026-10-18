<script lang="ts">
  // The effects browser (E, or Browse… in the Effects section): every
  // built-in effect and installed plugin by category, with a search box.
  // Add puts the effect at the end of the selected clip's stack.
  import { app, run } from "../lib/session.svelte";
  import { api, type CatalogGroup, type CatalogItem } from "../lib/api";

  let groups = $state<CatalogGroup[]>([]);
  let search = $state("");
  let input = $state<HTMLInputElement | null>(null);
  let added = $state("");

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
  const shown = $derived(groups.map((g) => ({ ...g, items: g.items.filter((it) => hit(g, it)) })).filter((g) => g.items.length));
  const plugins = $derived(groups.reduce((n, g) => n + g.items.filter((i) => i.plugin).length, 0));

  async function add(it: CatalogItem) {
    if (!target) return;
    await run(api.effectAdd(target.id, it.kind, it.plugin));
    added = it.label;
    setTimeout(() => (added === it.label ? (added = "") : null), 2500);
  }

  function key(e: KeyboardEvent) {
    if (app.effectsOpen && e.key === "Escape") {
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
        <h2 id="fx-title">Effects</h2>
        <span class="for">
          {#if target}Adding to <b>{target.name}</b>{:else}<span class="warn">Select a video, picture or title clip to add effects to it</span>{/if}
        </span>
        <button class="ghost close" onclick={() => (app.effectsOpen = false)} aria-label="Close">✕</button>
      </header>
      <input bind:this={input} type="search" bind:value={search} placeholder="Search: blur, key, colour…" aria-label="Search effects" />
      <div class="list">
        {#each shown as g}
          <h3>{g.label}</h3>
          {#each g.items as it}
            <div class="item">
              <div class="text">
                <span class="name">{it.label}{#if it.plugin}<span class="tag">plugin</span>{/if}</span>
                <span class="desc">{[it.description, it.author && `by ${it.author}`, it.version].filter(Boolean).join(" · ")}</span>
              </div>
              <button disabled={!target} onclick={() => add(it)}>{added === it.label ? "Added ✓" : "Add"}</button>
            </div>
          {/each}
        {:else}
          <p class="muted">Nothing matches “{search}”.</p>
        {/each}
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
