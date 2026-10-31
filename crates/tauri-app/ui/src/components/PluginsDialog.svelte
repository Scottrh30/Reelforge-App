<script lang="ts">
  // Edit > Plugins…: effect plugins (GPU shaders anyone can write). Install
  // from a folder or a .zip, make a new one from the template, remove, reload.
  import { app, run, say } from "../lib/session.svelte";
  import { api, type OfxView, type PluginsView } from "../lib/api";

  let view = $state<PluginsView | null>(null);
  let ofx = $state<OfxView | null>(null);
  let tab = $state<"shaders" | "ofx">("shaders");
  let confirm = $state<string | null>(null);

  async function load() {
    [view, ofx] = await Promise.all([api.pluginsList(), api.ofxList()]);
    // A scan running in the background: look again shortly.
    if (ofx.scanning) setTimeout(() => app.pluginsOpen && load(), 700);
  }
  $effect(() => {
    if (app.pluginsOpen) {
      void app.live?.plugins_rev;
      load();
    } else confirm = null;
  });

  async function act(p: Promise<string | null>) {
    const msg = await run(p);
    if (msg) say(msg);
    await load();
  }

  function key(e: KeyboardEvent) {
    if (app.pluginsOpen && e.key === "Escape") {
      e.stopPropagation();
      app.pluginsOpen = false;
    }
  }
  const ofxEffects = $derived(ofx?.bundles.reduce((n, b) => n + b.effects.length, 0) ?? 0);
  const ok = $derived(view?.entries.filter((e) => e.ok).length ?? 0);
  const bad = $derived((view?.entries.length ?? 0) - ok);
</script>

<svelte:window onkeydown={key} />

{#if app.pluginsOpen && view}
  <div class="scrim" role="presentation" onpointerdown={(e) => e.target === e.currentTarget && (app.pluginsOpen = false)}>
    <div class="dialog" role="dialog" aria-modal="true" aria-labelledby="pl-title">
      <header>
        <h2 id="pl-title">Plugins</h2>
        <div class="tabs" role="tablist">
          <button role="tab" class:on={tab === "shaders"} aria-selected={tab === "shaders"} onclick={() => (tab = "shaders")}>Shader plugins <span class="muted">{ok}</span>{#if bad}<span class="bad"> ⚠</span>{/if}</button>
          <button role="tab" class:on={tab === "ofx"} aria-selected={tab === "ofx"} onclick={() => (tab = "ofx")}>OpenFX <span class="muted">{ofxEffects}</span></button>
        </div>
        <span class="grow"></span>
        <button class="ghost close" onclick={() => (app.pluginsOpen = false)} aria-label="Close">✕</button>
      </header>
      {#if tab === "shaders"}
      <p class="lead">Effect plugins are GPU shaders you can write yourself. They show up in the effects browser with the built-in effects. See PLUGINS.md next to Reelforge for how to write one.</p>
      <div class="tools">
        <button onclick={() => act(api.pluginsInstall(false))} title="A folder with a plugin.toml in it">Install from folder…</button>
        <button onclick={() => act(api.pluginsInstall(true))}>Install from .zip…</button>
        <button onclick={() => act(api.pluginsNew())} title="Make a starter plugin in your plugin folder and open it, to edit">New plugin…</button>
        <button onclick={() => run(api.openFolder(view!.folder))} title={view.folder}>Open plugin folder</button>
        <button onclick={() => act(api.pluginsReload())} title="Look at every plugin folder again">Reload</button>
      </div>
      <label class="check">
        <input type="checkbox" checked={view.live_reload} onchange={async (e) => (await api.pluginsLive(e.currentTarget.checked), load())} />
        Reload automatically when a plugin's files change <span class="muted">(edit a shader, save, and the Viewer updates)</span>
      </label>
      <div class="list">
        {#each view.entries as p (p.folder)}
          <div class="item" class:broken={!p.ok}>
            <div class="top">
              <span class="name">{p.name}</span>
              {#if p.ok}
                <span class="muted">{p.version || "–"} · {p.category}</span>
                <span class="state good">✓ loaded</span>
              {:else}
                <span class="state bad">⚠ not loaded</span>
              {/if}
              <span class="grow"></span>
              <button class="small" onclick={() => run(api.openFolder(p.folder))}>Show folder</button>
              {#if p.removable}
                {#if confirm === p.folder}
                  <button class="small danger" onclick={() => ((confirm = null), act(api.pluginsRemove(p.folder)))}>Really remove</button>
                  <button class="small" onclick={() => (confirm = null)}>Keep</button>
                {:else}
                  <button class="small" onclick={() => (confirm = p.folder)} title="Delete this plugin's folder">Remove</button>
                {/if}
              {/if}
            </div>
            {#if p.ok}
              {#if p.description}<p>{p.description}</p>{/if}
              <p class="muted small-text">{[p.author && `by ${p.author}`, `id ${p.id}`, `${p.params} setting${p.params === 1 ? "" : "s"}, ${p.passes} pass${p.passes === 1 ? "" : "es"}`].filter(Boolean).join(" · ")}</p>
            {:else}
              <pre>{p.error}</pre>
            {/if}
            <p class="muted small-text path">{p.folder} ({p.place})</p>
          </div>
        {:else}
          <p class="muted">No plugins yet. Put plugin folders in {view.folder}, or use Install or New plugin above.</p>
        {/each}
      </div>
      {:else if ofx}
      <p class="lead">OpenFX plugins (.ofx bundles, as used by Resolve, Nuke and Vegas) run on the picture before the other effects: on the graphics card for plugins that render with OpenGL, else on the processor. Their settings keyframe like any other, and their positions can be dragged in the Viewer.</p>
      <div class="tools">
        <button onclick={() => act(api.ofxAddFolder())} title="Search another folder for .ofx.bundle plugins">Add folder…</button>
        <button onclick={() => run(api.ofxOpenFolder())} title={ofx.own_folder}>Open OpenFX folder</button>
        <button onclick={() => act(api.ofxRescan())} disabled={ofx.scanning}>{ofx.scanning ? "Scanning…" : "Scan again"}</button>
      </div>
      <label class="check">
        <input type="checkbox" checked={ofx.enabled} onchange={(e) => act(api.ofxEnable(e.currentTarget.checked))} />
        Load OpenFX plugins <span class="muted">(they're programs: only install ones you trust)</span>
      </label>
      <label class="check">
        <input type="checkbox" checked={ofx.gpu} disabled={!ofx.enabled} onchange={(e) => act(api.ofxGpu(e.currentTarget.checked))} />
        Let plugins render on the graphics card (OpenGL) when they can
        <span class="muted">{ofx.gpu_status ? (ofx.gpu_ok ? `· ${ofx.gpu_status}` : `· not available: ${ofx.gpu_status}`) : ""}</span>
      </label>
      <div class="list">
        <p class="muted small-text">Searched:</p>
        {#each ofx.folders as f (f.path)}
          <div class="top folder">
            <span class="path" class:gone={!f.exists}>{f.path}</span>
            <span class="muted small-text">{f.place}{f.exists ? "" : " · not there"}</span>
            <span class="grow"></span>
            {#if f.removable}<button class="small" onclick={() => act(api.ofxRemoveFolder(f.path))}>Remove</button>{/if}
          </div>
        {/each}
        {#each ofx.bundles as b (b.path)}
          <div class="item" class:broken={b.blocked || (b.effects.length === 0 && b.errors.length > 0)}>
            <div class="top">
              <span class="name">{b.name}</span>
              {#if b.blocked}
                <span class="state bad">⚠ skipped: it crashed Reelforge last time</span>
                <span class="grow"></span>
                <button class="small" onclick={() => act(api.ofxUnblock(b.path))} title="Load it again (if it crashes again, it's skipped again)">Try again</button>
              {:else if b.effects.length}
                <span class="state good">✓ {b.effects.length} effect{b.effects.length === 1 ? "" : "s"}</span>
              {:else}
                <span class="state bad">⚠ not loaded</span>
              {/if}
            </div>
            {#if b.effects.length}
              <p class="small-text">
                {#each b.effects as e}<span class="fx">{e.name}{#if e.gpu}<span class="tag" title={e.gpu === "only" ? "Renders only on the graphics card (OpenGL)" : "Renders on the graphics card (OpenGL) when it can"}>GPU{e.gpu === "only" ? " only" : ""}</span>{/if}{#if e.temporal}<span class="tag" title="Reads the clip's neighbouring frames">frames</span>{/if}</span>{/each}
              </p>
            {/if}
            {#each b.errors as err}<pre>{err}</pre>{/each}
            <p class="muted small-text path">{b.path}</p>
          </div>
        {:else}
          <p class="muted">{ofx.scanning ? "Looking for plugins…" : ofx.enabled ? `No OpenFX plugins found. Install some, or put .ofx.bundle folders in ${ofx.own_folder}.` : "OpenFX plugins are switched off."}</p>
        {/each}
      </div>
      {/if}
      <footer>
        <span class="grow"></span>
        <button class="primary" onclick={() => (app.pluginsOpen = false)}>Done</button>
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
    z-index: 42;
  }
  .dialog {
    width: min(640px, calc(100vw - 32px));
    height: min(640px, calc(100vh - 40px));
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
  .close {
    align-self: center;
  }
  .tabs {
    display: flex;
    gap: 4px;
    align-self: center;
  }
  .tabs button {
    min-height: 24px;
    padding: 0 10px;
    border-radius: 12px;
    background: transparent;
  }
  .tabs button.on {
    background: var(--well);
    border-color: var(--accent, var(--line));
  }
  .folder {
    padding: 0 2px;
  }
  .gone {
    text-decoration: line-through;
  }
  .fx {
    display: inline-block;
    margin-right: 12px;
  }
  .tag {
    margin-left: 4px;
    padding: 0 5px;
    border-radius: 8px;
    font-size: calc(10.5px * var(--text-scale));
    background: var(--well);
    color: var(--muted);
  }
  .lead {
    margin: 0;
    color: var(--muted);
  }
  .tools {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .check {
    display: flex;
    gap: 6px;
    align-items: center;
  }
  .list {
    flex: 1;
    min-height: 0;
    overflow: auto;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .item {
    border: 1px solid var(--line-soft);
    border-radius: 6px;
    background: var(--base);
    padding: 8px 10px;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .item.broken {
    border-color: color-mix(in srgb, #f0a050 50%, var(--line-soft));
  }
  .top {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
  }
  .name {
    font-weight: 600;
  }
  .state {
    font-size: calc(12px * var(--text-scale));
  }
  .good {
    color: #6ec878;
  }
  .bad {
    color: #f0a050;
  }
  p {
    margin: 0;
  }
  .small-text {
    font-size: calc(12px * var(--text-scale));
  }
  .path {
    word-break: break-all;
    color: var(--faint);
  }
  pre {
    margin: 0;
    white-space: pre-wrap;
    font-size: calc(12px * var(--text-scale));
    color: #f5be8c;
    background: var(--well);
    padding: 6px 8px;
    border-radius: 4px;
  }
  .small {
    min-height: 22px;
    padding: 0 7px;
    font-size: calc(12px * var(--text-scale));
  }
  .danger {
    color: #ff7864;
    border-color: #ff7864;
  }
  footer {
    display: flex;
  }
  .grow {
    flex: 1;
  }
  .muted {
    color: var(--muted);
  }
</style>
