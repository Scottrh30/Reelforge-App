<script lang="ts">
  // The effects browser's Presets tab: favourites first, then your folders,
  // then the starter looks, each folding shut. Hovering a preset shows it on
  // the selected clip; Apply puts it there (on every selected clip when
  // several are); ★ marks favourites; ⋯ renames, files, exports or deletes.
  import { onDestroy } from "svelte";
  import { app, run } from "../lib/session.svelte";
  import { api, type FxPreset } from "../lib/api";
  import { askText, question } from "../lib/actions.svelte";
  import { openMenu, type MenuItem } from "../lib/menu.svelte";

  let { q }: { q: string } = $props();

  let presets = $state<FxPreset[]>([]);
  let thumbs = $state<Record<string, string>>({});
  let applied = $state("");
  const reload = async () => {
    presets = await api.fxPresetList();
    for (const p of presets) {
      if (p.thumb && !thumbs[p.name]) {
        api.fxPresetThumb(p.name).then((buf) => {
          if (buf.byteLength) thumbs[p.name] = URL.createObjectURL(new Blob([buf], { type: "image/jpeg" }));
        });
      }
    }
  };
  $effect(() => {
    if (!app.savePreset) reload();
  });
  onDestroy(() => {
    Object.values(thumbs).forEach(URL.revokeObjectURL);
    endPreview();
  });

  // What a preset goes on: the selected clip (any kind: audio presets go on audio).
  const ins = $derived(app.s?.inspector ?? null);
  const many = $derived((app.s?.selected.length ?? 0) > 1 && !!ins && app.s!.selected.includes(ins.id));
  const fits = (p: FxPreset) =>
    !!ins &&
    p.parts.some((k) => (k === "look" ? ins.visual : k === "title" ? ins.kind === "title" : k === "audio" ? ins.kind === "audio" || ins.kind === "video" : ins.kind !== "audio"));

  const hit = (p: FxPreset) => !q || [p.name, p.summary, p.folder].some((x) => x.toLowerCase().includes(q));
  const groups = $derived.by(() => {
    const list = presets.filter(hit);
    const out: { key: string; label: string; items: FxPreset[] }[] = [];
    const favs = list.filter((p) => p.favourite);
    if (favs.length) out.push({ key: "★", label: "★ Favourites", items: favs });
    const folders = [...new Set(list.filter((p) => !p.builtin && p.folder).map((p) => p.folder))].sort((a, b) => a.localeCompare(b));
    for (const f of folders) out.push({ key: "f:" + f, label: f, items: list.filter((p) => !p.builtin && p.folder === f) });
    const loose = list.filter((p) => !p.builtin && !p.folder);
    if (loose.length) out.push({ key: "mine", label: folders.length ? "Not in a folder" : "Your presets", items: loose });
    // The built-in ones in their own folders (Starter looks, Neon signs, Light looks).
    const builtins = list.filter((p) => p.builtin);
    for (const f of [...new Set(builtins.map((p) => p.folder || "Starter looks"))]) {
      out.push({ key: f === "Starter looks" ? "starters" : "b:" + f, label: f, items: builtins.filter((p) => (p.folder || "Starter looks") === f) });
    }
    return out;
  });

  // Folded groups, remembered.
  const FOLD = "reelforge.presets.folded";
  let folded = $state<Record<string, boolean>>(
    (() => {
      try {
        return JSON.parse(localStorage.getItem(FOLD) ?? "{}") ?? {};
      } catch {
        return {};
      }
    })(),
  );
  function toggle(k: string) {
    folded[k] = !folded[k];
    try {
      localStorage.setItem(FOLD, JSON.stringify(folded));
    } catch {}
  }
  const shut = (k: string) => !q && !!folded[k];

  // Hover: show it on the clip after a moment; leaving puts the clip back.
  let previewing: string | null = null;
  let timer: ReturnType<typeof setTimeout> | null = null;
  function enter(p: FxPreset) {
    if (timer) clearTimeout(timer);
    if (!ins || !(p.parts.includes("look") || p.parts.includes("title")) || !fits(p)) return;
    const id = ins.id;
    timer = setTimeout(() => {
      previewing = p.name;
      api.fxPresetPreview(id, p.name).catch(() => {});
    }, 300);
  }
  function endPreview() {
    if (timer) clearTimeout(timer);
    timer = null;
    if (previewing) {
      previewing = null;
      api.fxPresetPreviewEnd().catch(() => {});
    }
  }

  async function apply(p: FxPreset, replace: boolean) {
    if (!ins) return;
    if (timer) clearTimeout(timer);
    previewing = null;
    const n = await run(api.fxPresetApply(ins.id, p.name, replace));
    if (typeof n === "number" && n > 0) {
      applied = p.name;
      setTimeout(() => (applied === p.name ? (applied = "") : null), 2500);
    }
  }
  async function star(p: FxPreset) {
    await run(api.fxPresetFavourite(p.name, !p.favourite));
    reload();
  }
  async function more(e: MouseEvent, p: FxPreset) {
    e.stopPropagation();
    const folders = [...new Set(presets.filter((x) => !x.builtin && x.folder).map((x) => x.folder))];
    const items: MenuItem[] = [
      { heading: true, label: p.name },
      { label: "Rename…", disabled: p.builtin, run: () => rename(p) },
      {
        label: "Move to folder",
        disabled: p.builtin,
        submenu: [
          ...folders.filter((f) => f !== p.folder).map((f) => ({ label: f, run: () => moveTo(p, f) })),
          ...(folders.length ? [{ separator: true } as MenuItem] : []),
          { label: "New folder…", run: () => newFolder(p) },
          { label: "Out of its folder", disabled: !p.folder, run: () => moveTo(p, "") },
        ],
      },
      { label: "Export…", run: () => run(api.fxPresetExport([p.name])) },
      { separator: true },
      { label: p.builtin ? "Delete (a starter look stays)" : "Delete…", disabled: p.builtin, run: () => remove(p) },
    ];
    openMenu(e, items);
  }
  async function rename(p: FxPreset) {
    const to = await askText("Rename preset", "", p.name, "Rename");
    if (to?.trim() && to.trim() !== p.name) await run(api.fxPresetRename(p.name, to.trim()));
    reload();
  }
  async function moveTo(p: FxPreset, folder: string) {
    await run(api.fxPresetFolder(p.name, folder));
    reload();
  }
  async function newFolder(p: FxPreset) {
    const f = await askText("New folder", `File “${p.name}” in a new folder:`, "", "Move");
    if (f?.trim()) moveTo(p, f.trim());
  }
  async function remove(p: FxPreset) {
    if ((await question("Delete preset", `Delete the preset “${p.name}”? Clips it was applied to keep what it gave them.`, ["Delete", "Cancel"], "delete_preset")) !== "Delete") return;
    await run(api.fxPresetDelete(p.name));
    reload();
  }
  async function importP() {
    await run(api.fxPresetImport());
    reload();
  }
  const mine = $derived(presets.filter((p) => !p.builtin).map((p) => p.name));
  const PART: Record<string, string> = { look: "look", title: "title", audio: "audio", transition: "transition" };
</script>

<div class="bar">
  <span class="muted">
    {#if !ins}Select a clip to apply a preset to it
    {:else if many}Applying to the <b>{app.s?.selected.length}</b> selected clips
    {:else}Applying to <b>{ins.name}</b>{/if}
  </span>
  <span class="grow"></span>
  <button class="ghost small" onclick={importP} title="Add presets from a .rfpreset file">Import…</button>
  <button class="ghost small" disabled={!mine.length} onclick={() => run(api.fxPresetExport(mine))} title="Save all your presets to one .rfpreset file (to back up or share)">Export yours…</button>
</div>

{#each groups as g (g.key)}
  <button class="group" aria-expanded={!shut(g.key)} onclick={() => toggle(g.key)}>
    <span class="chev" class:shut={shut(g.key)} aria-hidden="true">▾</span>
    <h3>{g.label}</h3>
    <span class="count">{g.items.length}</span>
  </button>
  {#if !shut(g.key)}
    {#each g.items as p (g.key + p.name)}
      <div class="item" class:dim={!!ins && !fits(p)} role="listitem" onpointerenter={() => enter(p)} onpointerleave={endPreview}>
        <div class="pic">
          {#if thumbs[p.name]}<img src={thumbs[p.name]} alt="" />{:else}<span aria-hidden="true">{p.parts.includes("look") ? "✦" : p.parts.includes("title") ? "T" : p.parts.includes("audio") ? "♪" : "⇄"}</span>{/if}
        </div>
        <div class="text">
          <span class="name">{p.name}</span>
          <span class="desc">{p.summary || "—"}{#if p.missing.length}<span class="warn">&nbsp;· not installed: {p.missing.join(", ")}</span>{/if}</span>
          <span class="parts">{#each p.parts as k}<span class="tag">{PART[k]}</span>{/each}</span>
        </div>
        <button class="ghost icon starb" class:on={p.favourite} onclick={() => star(p)} title={p.favourite ? "Take it out of the favourites" : "Add to the favourites (top of the list, and first in + Add → Presets)"} aria-label="Favourite" aria-pressed={p.favourite}>{p.favourite ? "★" : "☆"}</button>
        <button class="ghost icon" onclick={(e) => more(e, p)} title="Rename, move to a folder, export, delete" aria-label="More for {p.name}">⋯</button>
        <button disabled={!ins || !fits(p)} onclick={(e) => apply(p, e.shiftKey)} title={many ? "Apply to every selected clip (Shift+click: replace their effects and masks)" : "Apply: effects and masks go after the clip's own (Shift+click: instead of them)"}>{applied === p.name ? "Applied ✓" : "Apply"}</button>
      </div>
    {/each}
  {/if}
{:else}
  <p class="muted">{presets.length ? `Nothing matches “${q}”.` : "No presets yet."}</p>
{/each}

<style>
  .bar {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 0 2px 6px;
  }
  .grow {
    flex: 1;
  }
  .muted {
    color: var(--muted);
  }
  .warn {
    color: var(--accent);
  }
  .small {
    min-height: 22px;
    padding: 0 8px;
    font-size: calc(12px * var(--text-scale));
  }
  .group {
    display: flex;
    align-items: center;
    gap: 6px;
    width: 100%;
    margin: 8px 0 2px;
    padding: 2px 4px;
    background: none;
    border: 0;
    border-radius: 5px;
    color: var(--muted);
    text-align: left;
  }
  .group:hover {
    background: var(--hover);
    color: var(--text);
  }
  .group h3 {
    margin: 0;
    font-size: calc(12px * var(--text-scale));
    font-weight: 600;
    color: inherit;
  }
  .chev {
    display: inline-block;
    width: 12px;
    transition: transform 0.12s;
  }
  .chev.shut {
    transform: rotate(-90deg);
  }
  .count {
    color: var(--faint);
    font-size: calc(11px * var(--text-scale));
  }
  .item {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px;
    border-radius: 6px;
  }
  .item:hover {
    background: var(--hover);
  }
  .item.dim .text {
    opacity: 0.55;
  }
  .pic {
    flex: none;
    width: 64px;
    height: 36px;
    border-radius: 4px;
    background: var(--well);
    display: grid;
    place-items: center;
    overflow: hidden;
    color: var(--accent);
  }
  .pic img {
    width: 100%;
    height: 100%;
    object-fit: cover;
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
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .parts {
    display: flex;
    gap: 3px;
  }
  .tag {
    font-size: calc(10px * var(--text-scale));
    padding: 0 5px;
    border-radius: 4px;
    border: 1px solid var(--line-soft);
    color: var(--muted);
  }
  .icon {
    min-width: 26px;
    padding: 0 4px;
  }
  .starb.on {
    color: var(--accent);
  }
</style>
