<script lang="ts">
  import { onMount } from "svelte";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { getCurrentWebview } from "@tauri-apps/api/webview";
  import { app, start, run } from "./lib/session.svelte";
  import { api } from "./lib/api";
  import { onKey, question, checkUnsaved } from "./lib/actions.svelte";
  import { hitTrack } from "./lib/timeline.svelte";
  import MenuBar from "./components/MenuBar.svelte";
  import MediaPanel from "./components/MediaPanel.svelte";
  import History from "./components/History.svelte";
  import Viewer from "./components/Viewer.svelte";
  import Controls from "./components/Controls.svelte";
  import Timeline from "./components/Timeline.svelte";
  import Meters from "./components/Meters.svelte";
  import Splitter from "./components/Splitter.svelte";
  import ContextMenu from "./components/ContextMenu.svelte";
  import Dialogs from "./components/Dialogs.svelte";
  import ExportDialog from "./components/ExportDialog.svelte";
  import ExportQueue from "./components/ExportQueue.svelte";
  import EffectsBrowser from "./components/EffectsBrowser.svelte";
  import PluginsDialog from "./components/PluginsDialog.svelte";
  import CollectDialog from "./components/CollectDialog.svelte";
  import Preferences from "./components/Preferences.svelte";
  import { look, apply } from "./lib/prefs.svelte";

  // Colours and fonts from Edit > Preferences, put on the page whenever they change.
  $effect(() => {
    JSON.stringify(look);
    apply(look);
  });

  const stored = (k: string, d: number) => {
    try {
      const v = Number(localStorage.getItem(k));
      return v > 0 ? v : d;
    } catch {
      return d;
    }
  };
  let leftW = $state(stored("leftW", 300));
  let rightW = $state(stored("rightW", 320));
  let bottomH = $state(stored("bottomH", 340));
  let metersW = $state(stored("metersW", 64));
  let fileHover = $state(false);

  const left = $derived(app.panels.media || app.panels.history);
  $effect(() => {
    if (!app.panels[app.leftTab]) app.leftTab = app.panels.media ? "media" : "history";
  });

  onMount(() => {
    start();
    const un: (() => void)[] = [];
    const scale = () => window.devicePixelRatio || 1;
    // Files dropped from the desktop: imported; over the timeline, placed where they land.
    getCurrentWebview()
      .onDragDropEvent(async (ev) => {
        const p = ev.payload;
        if (p.type === "enter" || p.type === "over") fileHover = true;
        else if (p.type === "leave") fileHover = false;
        else if (p.type === "drop") {
          fileHover = false;
          const at = hitTrack(p.position.x / scale(), p.position.y / scale());
          const ids = await run(api.importPaths(p.paths));
          if (at && ids) {
            let t = Math.round(at.secs * (app.s?.ticks_per_second ?? 705600000));
            for (const id of ids) {
              await run(api.addToTimeline(id, null, t));
              t = app.s?.seq.duration ?? t;
            }
          }
        }
      })
      .then((u) => un.push(u));
    // Closing with unsaved changes asks first.
    const win = getCurrentWindow();
    win
      .onCloseRequested(async (ev) => {
        if (!app.live?.dirty) return;
        ev.preventDefault();
        if (await checkUnsaved()) await win.destroy();
      })
      .then((u) => un.push(u));
    return () => un.forEach((f) => f());
  });

  void question;

  // The window title: the project's name, marked when there are unsaved changes.
  $effect(() => {
    const t = `${app.live?.dirty ? "• " : ""}${app.s?.title ?? "Untitled"} — Reelforge`;
    getCurrentWindow().setTitle(t).catch(() => {});
  });
</script>

<svelte:window onkeydown={onKey} oncontextmenu={(e) => e.preventDefault()} />

<div class="app">
  <MenuBar />
  <div class="work">
    <div class="upper" style="flex: 1 1 0">
      {#if left}
        <section class="pane" style="width:{leftW}px" aria-label="Media">
          <div class="tabs" role="tablist">
            {#if app.panels.media}<button role="tab" aria-selected={app.leftTab === "media"} class:active={app.leftTab === "media"} onclick={() => (app.leftTab = "media")}>Media</button>{/if}
            {#if app.panels.history}<button role="tab" aria-selected={app.leftTab === "history"} class:active={app.leftTab === "history"} onclick={() => (app.leftTab = "history")}>History</button>{/if}
          </div>
          <div class="body">
            {#if app.leftTab === "media"}<MediaPanel />{:else}<History />{/if}
          </div>
        </section>
        <Splitter bind:value={leftW} key="leftW" min={200} max={700} />
      {/if}
      {#if app.panels.viewer}
        <section class="pane grow" aria-label="Viewer"><Viewer /></section>
      {:else}
        <div class="grow"></div>
      {/if}
      {#if app.panels.controls}
        <Splitter bind:value={rightW} key="rightW" sign={-1} min={220} max={700} />
        <section class="pane" style="width:{rightW}px" aria-label="Controls">
          <div class="tabs"><span class="tabtitle">Controls</span></div>
          <div class="body"><Controls /></div>
        </section>
      {/if}
    </div>
    {#if app.panels.timeline || app.panels.meters}
      <Splitter bind:value={bottomH} dir="row" sign={-1} key="bottomH" min={160} max={1200} />
      <div class="lower" style="height:{bottomH}px">
        {#if app.panels.timeline}
          <section class="pane grow" aria-label="Timeline"><Timeline /></section>
        {:else}
          <div class="grow"></div>
        {/if}
        {#if app.panels.meters}
          <Splitter bind:value={metersW} key="metersW" sign={-1} min={50} max={200} />
          <section class="pane" style="width:{metersW}px" aria-label="Meters"><Meters /></section>
        {/if}
      </div>
    {/if}
  </div>
  <footer class="status">
    <span class="msg">{app.note || app.live?.status || ""}</span>
    {#if app.live?.collect?.running && !app.collectOpen}
      <button class="ghost link" onclick={() => (app.collectOpen = true)}>Collecting {Math.round((app.live.collect.bytes / Math.max(1, app.live.collect.total)) * 100)}%</button>
    {/if}
    {#if app.live?.export.summary}
      <button class="ghost link" onclick={() => (app.queueOpen = true)}>{app.live.export.summary}</button>
    {/if}
    <span class="muted">{app.s ? `${app.s.seq.width}×${app.s.seq.height} @ ${(app.s.seq.rate_num / app.s.seq.rate_den).toFixed(3).replace(".000", "")} fps` : ""}</span>
  </footer>
</div>

{#if fileHover}<div class="filehover">Drop to import</div>{/if}
<ContextMenu />
<Dialogs />
<ExportDialog />
<ExportQueue />
<EffectsBrowser />
<PluginsDialog />
<CollectDialog />
<Preferences />

<style>
  .app {
    height: 100%;
    display: flex;
    flex-direction: column;
  }
  .work {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    padding: 6px 6px 0;
  }
  .upper,
  .lower {
    display: flex;
    min-height: 0;
  }
  .upper {
    min-height: 140px;
  }
  .pane {
    background: var(--panel);
    border-radius: 6px;
    min-width: 0;
    min-height: 0;
    display: flex;
    flex-direction: column;
    overflow: hidden;
    flex: none;
  }
  .grow {
    flex: 1 1 auto;
  }
  .tabs {
    display: flex;
    gap: 2px;
    padding: 4px 8px 0;
    border-bottom: 1px solid var(--line-soft);
    min-height: 31px;
    align-items: flex-end;
  }
  .tabs button {
    background: none;
    border: 0;
    border-bottom: 2px solid transparent;
    border-radius: 0;
    color: var(--muted);
    padding: 3px 10px;
  }
  .tabs button.active {
    color: var(--text);
    border-bottom-color: var(--accent);
  }
  .tabtitle {
    padding: 3px 4px 6px;
    font-weight: 500;
  }
  .body {
    flex: 1;
    min-height: 0;
  }
  .status {
    display: flex;
    gap: 14px;
    align-items: center;
    height: 26px;
    padding: 0 12px;
    font-size: calc(12px * var(--text-scale));
    color: var(--muted);
  }
  .msg {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--text);
  }
  .link {
    color: var(--accent);
    min-height: 20px;
    padding: 0 6px;
  }
  .filehover {
    position: fixed;
    inset: 8px;
    border: 2px dashed var(--accent);
    border-radius: 10px;
    background: color-mix(in srgb, var(--accent) 6%, transparent);
    display: grid;
    place-items: center;
    font-size: calc(18px * var(--text-scale));
    color: var(--accent);
    pointer-events: none;
    z-index: 70;
  }
</style>
