<script lang="ts">
  // The Effects panel: your presets, and every effect and transition in its group, by name.
  // Double-click one to add it to the selected clip (a transition goes in at
  // the selected clip's start, or the cut nearest the playhead), or drag it
  // onto a clip on the timeline.
  import { app, run } from "../lib/session.svelte";
  import { api, type CatalogGroup, type CatalogItem, type FxPreset, type TransitionKind } from "../lib/api";
  import { favourites, isFav, toggleFav } from "../lib/favourites.svelte";
  import { effectThumb } from "../lib/thumbs";

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
  const isOpen = (label: string) => !!q || (open[label] ?? label === "Favourites");

  type Entry = { label: string; effect?: CatalogItem; transition?: TransitionKind; preset?: FxPreset };
  const sortedPresets = $derived([...presets].sort((a, b) => Number(b.favourite) - Number(a.favourite)));
  const favs = $derived(favourites(groups.flatMap((g) => g.items)));
  const all = $derived<{ label: string; items: Entry[] }[]>([
    ...(favs.length ? [{ label: "Favourites", items: favs.map((it) => ({ label: it.label, effect: it })) }] : []),
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

  // Hovering an effect: a card beside the panel with what it does, and the
  // selected clip's frame at the playhead with it added (as in the browser,
  // and off with the browser's Pictures switch).
  type Peek = { it: CatalogItem; x: number; y: number; left: boolean; url: string | null };
  let peek = $state<Peek | null>(null);
  let peekTimer: ReturnType<typeof setTimeout> | undefined;
  const stamp = $derived(`${app.live?.rev ?? 0}:${app.live?.playhead ?? 0}`);
  const picturesOn = () => {
    try {
      return localStorage.getItem("fxPictures") !== "0";
    } catch {
      return true;
    }
  };
  function enter(ev: PointerEvent, e: Entry) {
    leave();
    const it = e.effect;
    if (!it || app.dragEffect) return;
    const el = ev.currentTarget as HTMLElement;
    peekTimer = setTimeout(() => {
      const r = el.getBoundingClientRect();
      const panel = el.closest(".lib")?.getBoundingClientRect() ?? r;
      // Beside the panel, on whichever side has room for the card.
      const left = panel.right + 236 > window.innerWidth;
      const y = Math.max(8, Math.min(r.top - 8, window.innerHeight - 220));
      const p: Peek = { it, x: left ? panel.left - 8 : panel.right + 8, y, left, url: null };
      peek = p;
      if (target && picturesOn()) {
        effectThumb(target.id, it.kind, it.plugin, stamp).then((u) => {
          if (peek && peek.it === it) peek = { ...peek, url: u };
        });
      }
    }, 350);
  }
  function leave() {
    clearTimeout(peekTimer);
    peek = null;
  }

  // Dragging onto the timeline (the Timeline picks the clip under the pointer).
  function down(ev: PointerEvent, e: Entry) {
    leave();
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
          <div class="item" class:fav={!!e.effect && isFav(e.effect)} role="treeitem" aria-selected="false" tabindex="0" onpointerdown={(ev) => down(ev, e)} onpointerenter={(ev) => enter(ev, e)} onpointerleave={leave} ondblclick={() => apply(e, null)} onkeydown={(ev) => ev.key === "Enter" && apply(e, null)}>
            <span class="label">{e.label}</span>
            {#if e.effect}
              {@const it = e.effect}
              <button class="star" class:on={isFav(it)} onpointerdown={(ev) => ev.stopPropagation()} ondblclick={(ev) => ev.stopPropagation()} onclick={(ev) => { ev.stopPropagation(); toggleFav(it); }} aria-pressed={isFav(it)} aria-label={isFav(it) ? `Remove ${it.label} from favourites` : `Add ${it.label} to favourites`} title={isFav(it) ? "Remove from favourites" : "Add to favourites (listed first)"}>{isFav(it) ? "★" : "☆"}</button>
            {/if}
          </div>
        {/each}
      {/if}
    {:else}
      <p class="none">No effect matches “{search}”</p>
    {/each}
  </div>
</div>

{#if peek}
  <div class="peek" class:left={peek.left} style="{peek.left ? `right:${window.innerWidth - peek.x}px` : `left:${peek.x}px`}; top:{peek.y}px" role="tooltip">
    {#if target && picturesOn()}
      <div class="pic">
        {#if peek.url}<img src={peek.url} alt="" />{:else}<span class="wait">…</span>{/if}
      </div>
    {/if}
    <div class="name">{peek.it.label}</div>
    {#if peek.it.description}<div class="desc">{peek.it.description}</div>{/if}
    {#if !target}<div class="hint">Select a clip to see it on your footage.</div>{/if}
  </div>
{/if}

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
    display: flex;
    align-items: center;
    gap: 4px;
    padding: 3px 4px 3px 28px;
    border-radius: 4px;
    cursor: grab;
    user-select: none;
  }
  .item .label {
    flex: 1;
    min-width: 0;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  /* A star: shown on hover, or always once starred. */
  .star {
    background: none;
    border: 0;
    padding: 0 4px;
    min-height: 0;
    line-height: 1;
    color: var(--faint);
    cursor: pointer;
    visibility: hidden;
  }
  .item:hover .star,
  .item:focus-within .star,
  .star.on {
    visibility: visible;
  }
  .star.on {
    color: var(--accent);
  }
  .star:hover {
    color: var(--text);
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
  .peek {
    position: fixed;
    z-index: 70;
    width: 220px;
    background: var(--raised);
    border: 1px solid var(--line);
    border-radius: 6px;
    padding: 6px;
    box-shadow: 0 6px 20px rgba(0, 0, 0, 0.45);
    pointer-events: none;
  }
  .pic {
    aspect-ratio: 16 / 9;
    background: #000;
    border-radius: 4px;
    overflow: hidden;
    display: grid;
    place-items: center;
    margin-bottom: 6px;
  }
  .pic img {
    width: 100%;
    height: 100%;
    object-fit: contain;
  }
  .wait {
    color: var(--faint);
  }
  .peek .name {
    font-weight: 500;
  }
  .peek .desc,
  .peek .hint {
    color: var(--muted);
    font-size: calc(11px * var(--text-scale));
    margin-top: 2px;
  }
  .none {
    color: var(--faint);
    padding: 8px;
    margin: 0;
  }
</style>
