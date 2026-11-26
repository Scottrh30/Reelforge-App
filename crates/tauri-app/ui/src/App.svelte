<script lang="ts">
  import { onMount } from "svelte";
  import { getCurrentWindow } from "@tauri-apps/api/window";
  import { getCurrentWebview } from "@tauri-apps/api/webview";
  import { app, start, run } from "./lib/session.svelte";
  import DiskWarning from "./components/DiskWarning.svelte";
  import { api } from "./lib/api";
  import { onKey, question, checkUnsaved } from "./lib/actions.svelte";
  import { hitTrack } from "./lib/timeline.svelte";
  import MenuBar from "./components/MenuBar.svelte";
  import Viewer from "./components/Viewer.svelte";
  import Dock from "./components/Dock.svelte";
  import FloatPanel from "./components/FloatPanel.svelte";
  import { dock } from "./lib/dock.svelte";
  import { sizes } from "./lib/workspace.svelte";
  import Timeline from "./components/Timeline.svelte";
  import Meters from "./components/Meters.svelte";
  import Splitter from "./components/Splitter.svelte";
  import ContextMenu from "./components/ContextMenu.svelte";
  import ColorPicker from "./components/ColorPicker.svelte";
  import PlaneDialog from "./components/PlaneDialog.svelte";
  import Dialogs from "./components/Dialogs.svelte";
  import DuckDialog from "./components/DuckDialog.svelte";
  import SequenceSettingsDialog from "./components/SequenceSettingsDialog.svelte";
  import MatchColorDialog from "./components/MatchColorDialog.svelte";
  import MulticamDialog from "./components/MulticamDialog.svelte";
  import VersionsDialog from "./components/VersionsDialog.svelte";
  import AboutDialog from "./components/AboutDialog.svelte";
  import ExportDialog from "./components/ExportDialog.svelte";
  import ExportQueue from "./components/ExportQueue.svelte";
  import EffectsBrowser from "./components/EffectsBrowser.svelte";
  import SavePresetDialog from "./components/SavePresetDialog.svelte";
  import PluginsDialog from "./components/PluginsDialog.svelte";
  import StartScreen from "./components/StartScreen.svelte";
  import CollectDialog from "./components/CollectDialog.svelte";
  import PasteAttributes from "./components/PasteAttributes.svelte";
  import Preferences from "./components/Preferences.svelte";
  import { look, apply } from "./lib/prefs.svelte";

  // Colours and fonts from Edit > Preferences, put on the page whenever they change.
  $effect(() => {
    JSON.stringify(look);
    apply(look);
  });

  let fileHover = $state(false);

  // Each dock's panels that are switched on (Tabs menu); a dock with none is hidden.
  const leftShown = $derived(dock.left.filter((p) => app.panels[p]));
  const rightShown = $derived(dock.right.filter((p) => app.panels[p]));
  const floating = $derived(dock.float.filter((p) => app.panels[p]));

  /// Show the start screen once the state is in, unless there's crashed work to recover first.
  let launchStart = $state(false);
  $effect(() => {
    if (launchStart && app.s) {
      launchStart = false;
      if (!app.s.recoveries.length) app.startOpen = true;
    }
  });

  onMount(() => {
    start();
    // Opened without a project to open: the start screen (unless it's switched off).
    api.startAtLaunch().then((show) => (launchStart = show)).catch(() => {});
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
  <DiskWarning />
  <div class="work">
    <div class="upper" style="flex: 1 1 0">
      {#if leftShown.length}
        <section class="pane" style="width:{sizes.leftW}px" aria-label="Left dock">
          <Dock side="left" shown={leftShown} />
        </section>
        <Splitter bind:value={sizes.leftW} key="leftW" min={200} max={700} />
      {/if}
      {#if app.panels.viewer}
        <section class="pane grow" aria-label="Viewer"><Viewer /></section>
      {:else}
        <div class="grow"></div>
      {/if}
      {#if rightShown.length}
        <Splitter bind:value={sizes.rightW} key="rightW" sign={-1} min={220} max={700} />
        <section class="pane" style="width:{sizes.rightW}px" aria-label="Right dock">
          <Dock side="right" shown={rightShown} />
        </section>
      {/if}
    </div>
    {#if app.panels.timeline || app.panels.meters}
      <Splitter bind:value={sizes.bottomH} dir="row" sign={-1} key="bottomH" min={160} max={1200} />
      <div class="lower" style="height:{sizes.bottomH}px">
        {#if app.panels.timeline}
          <section class="pane grow" aria-label="Timeline"><Timeline /></section>
        {:else}
          <div class="grow"></div>
        {/if}
        {#if app.panels.meters}
          <Splitter bind:value={sizes.metersW} key="metersW" sign={-1} min={50} max={200} />
          <section class="pane" style="width:{sizes.metersW}px" aria-label="Meters"><Meters /></section>
        {/if}
      </div>
    {/if}
  </div>
  <footer class="status">
    <span class="msg">{app.note || app.live?.status || ""}</span>
    {#if app.live?.tracking}
      <span class="job" data-automation-id="status-job">
        <span class="jobbar"><span style="width:{Math.round(app.live.tracking[1] * 100)}%"></span></span>
        {Math.round(app.live.tracking[1] * 100)}%
        <button class="ghost link" title="Stop the job running in the background (nothing is changed)" onclick={() => run(api.trackCancel())} data-automation-id="status-job-stop">Stop</button>
      </span>
    {/if}
    {#if app.live?.collect?.running && !app.collectOpen}
      <button class="ghost link" onclick={() => (app.collectOpen = true)}>Collecting {Math.round((app.live.collect.bytes / Math.max(1, app.live.collect.total)) * 100)}%</button>
    {/if}
    {#if app.live?.export.summary}
      <button class="ghost link" onclick={() => (app.queueOpen = true)}>{app.live.export.summary}</button>
    {/if}
    <span class="muted">{app.s ? `${app.s.seq.width}×${app.s.seq.height} @ ${(app.s.seq.rate_num / app.s.seq.rate_den).toFixed(3).replace(".000", "")} fps` : ""}</span>
  </footer>
</div>

{#each floating as id, i (id)}<FloatPanel {id} z={i} />{/each}

{#if fileHover}<div class="filehover">Drop to import</div>{/if}
<ContextMenu />
<PlaneDialog />
<ColorPicker />
<Dialogs />
<ExportDialog />
<DuckDialog />
<SequenceSettingsDialog />
<MatchColorDialog />
<MulticamDialog />
<VersionsDialog />
<AboutDialog />
<ExportQueue />
<EffectsBrowser />
<SavePresetDialog />
<PluginsDialog />
<StartScreen />
<CollectDialog />
<PasteAttributes />
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
  .job {
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }
  .jobbar {
    width: 80px;
    height: 5px;
    border-radius: 3px;
    background: var(--line);
    overflow: hidden;
  }
  .jobbar span {
    display: block;
    height: 100%;
    background: var(--accent);
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
