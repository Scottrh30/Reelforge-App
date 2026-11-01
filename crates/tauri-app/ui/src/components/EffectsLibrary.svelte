<script lang="ts">
  // The Effects panel: your presets, and every effect and transition in its group, by name.
  // Double-click one to add it to the selected clip (a transition goes in at
  // the selected clip's start, or the cut nearest the playhead), or drag it
  // onto a clip on the timeline.
  import { app, run } from "../lib/session.svelte";
  import { api, type CatalogGroup, type CatalogItem, type FxPreset, type TransitionKind } from "../lib/api";

  let groups = $state<CatalogGroup[]>([]);
  let transitions = $state<TransitionKind[]>([]);
  let search = $state("");

  // Reload whenever the plugins change.
  $effect(() => {
    void app.live?.plugins_rev;
    api.effectCatalog().then((g) => (groups = g));
  });
  $effect(() => {
    api.transitionList().then((k) => (transitions = k));
  });
  // Saved presets (again whenever Save as preset… closes).
  let presets = $state<FxPreset[]>([]);
  $effect(() => {
    if (!app.savePreset) api.fxPresetList().then((p) => (presets = p));
  });

  // Which groups are open (remembered).
  const stored = (): Record<string, boolean> => {
    try {
      return JSON.parse(localStorage.getItem("fxGroups") ?? "{}");
    } catch {
      return {};
    }
  };
  let open = $state<Record<string, boolean>>(stored());
  function toggle(label: string) {
    open[label] = !isOpen(label);
    try {
      localStorage.setItem("fxGroups", JSON.stringify(open));
    } catch {}
  }
  const q = $derived(search.trim().toLowerCase());
  // While searching, every group with a match is open.
  const isOpen = (label: string) => !!q || (open[label] ?? false);

  type Entry = { label: string; effect?: CatalogItem; transition?: TransitionKind; preset?: FxPreset };
  const sortedPresets = $derived([...presets].sort((a, b) => Number(b.favourite) - Number(a.favourite)));
  const all = $derived<{ label: string; items: Entry[] }[]>([
    ...(presets.length ? [{ label: "Presets", items: sortedPresets.map((p) => ({ label: p.favourite ? `★ ${p.name}` : p.name, preset: p })) }] : []),
    ...groups.map((g) => ({ label: g.label, items: g.items.map((it) => ({ label: it.label, effect: it })) })),
    { label: "Transitions", items: transitions.map((k) => ({ label: k.label, transition: k })) },
  ]);
  const shown = $derived(all.map((g) => ({ ...g, items: g.items.filter((e) => !q || e.label.toLowerCase().includes(q)) })).filter((g) => g.items.length));

  const target = $derived(app.s?.inspector?.visual ? app.s.inspector : null);

  async function apply(e: Entry, clip: number | null) {
    if (e.preset) {
      // Any kind of clip (audio presets go on audio); several selected: all of them.
      const id = clip ?? app.s?.inspector?.id;
      if (id == null) {
        app.note = "Select a clip first (or drag the preset onto one)";
        return;
      }
      await run(api.fxPresetApply(id, e.preset.name, false));
    } else if (e.effect) {
      const id = clip ?? target?.id;
      if (id == null) {
        app.note = "Select a clip first (or drag the effect onto one)";
        return;
      }
      await run(api.effectAdd(id, e.effect.kind, e.effect.plugin));
    } else if (e.transition) {
      await run(api.transitionApply(e.transition.kind, clip ?? (target?.id ?? null)));
    }
  }

  // Dragging onto the timeline (the Timeline picks the clip under the pointer).
  function down(ev: PointerEvent, e: Entry) {
    if (ev.button !== 0) return;
    const x0 = ev.clientX;
    const y0 = ev.clientY;
    const move = (m: PointerEvent) => {
      if (!app.dragEffect && Math.hypot(m.clientX - x0, m.clientY - y0) < 5) return;
      app.dragEffect = { name: e.label, kind: e.effect?.kind ?? null, plugin: e.effect?.plugin ?? null, transition: e.transition?.kind ?? null, preset: e.preset?.name, x: m.clientX, y: m.clientY };
    };
    const up = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      // The Timeline handles the drop first (same pointerup), then this clears.
      setTimeout(() => (app.dragEffect = null), 0);
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
  }
</script>

<div class="lib">
  <input class="search" type="search" placeholder="Search effects" bind:value={search} aria-label="Search effects" />
  <div class="list" role="tree" aria-label="Effects">
    {#each shown as g (g.label)}
      <button class="group" role="treeitem" aria-selected="false" aria-expanded={isOpen(g.label)} onclick={() => toggle(g.label)}>
        <span class="arrow" class:down={isOpen(g.label)}>▸</span>
        <span class="gname">{g.label}</span>
        <span class="count">{g.items.length}</span>
      </button>
      {#if isOpen(g.label)}
        {#each g.items as e (e.label + (e.effect?.plugin ?? "") + (e.preset ? "p" : ""))}
          <div class="item" role="treeitem" aria-selected="false" tabindex="0" onpointerdown={(ev) => down(ev, e)} ondblclick={() => apply(e, null)} onkeydown={(ev) => ev.key === "Enter" && apply(e, null)}>{e.label}</div>
        {/each}
      {/if}
    {:else}
      <p class="none">No effect matches “{search}”</p>
    {/each}
  </div>
</div>

{#if app.dragEffect}
  <div class="ghost" style="left:{app.dragEffect.x + 12}px; top:{app.dragEffect.y + 8}px">{app.dragEffect.name}</div>
{/if}

<style>
  .lib {
    height: 100%;
    display: flex;
    flex-direction: column;
    min-height: 0;
  }
  .search {
    margin: 8px 8px 6px;
  }
  .list {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding: 0 4px 8px;
  }
  .group {
    display: flex;
    align-items: center;
    gap: 6px;
    width: 100%;
    background: none;
    border: 0;
    border-radius: 4px;
    padding: 4px 6px;
    min-height: 26px;
    color: var(--text);
    font-weight: 500;
    text-align: left;
  }
  .group:hover {
    background: var(--raised);
  }
  .arrow {
    display: inline-block;
    width: 10px;
    color: var(--muted);
    transition: transform 0.1s;
  }
  .arrow.down {
    transform: rotate(90deg);
  }
  .gname {
    flex: 1;
  }
  .count {
    color: var(--faint);
    font-size: calc(11px * var(--text-scale));
  }
  .item {
    padding: 3px 8px 3px 28px;
    border-radius: 4px;
    cursor: grab;
    user-select: none;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .item:hover,
  .item:focus-visible {
    background: var(--raised);
    outline: none;
  }
  .ghost {
    position: fixed;
    z-index: 80;
    background: var(--raised);
    border: 1px solid var(--accent);
    border-radius: 4px;
    padding: 2px 8px;
    pointer-events: none;
    font-size: calc(12px * var(--text-scale));
  }
  .none {
    color: var(--faint);
    padding: 8px;
    margin: 0;
  }
</style>
