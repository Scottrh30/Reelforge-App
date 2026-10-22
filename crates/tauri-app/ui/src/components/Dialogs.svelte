<script lang="ts">
  // The modal windows: Settings, questions, and crash recovery (Export is ExportDialog).
  import { untrack } from "svelte";
  import { app, run } from "../lib/session.svelte";
  import { api, type Prefs, type SettingsInfo } from "../lib/api";
  import { ask } from "../lib/actions.svelte";

  let prefs = $state<Prefs | null>(null);
  let info = $state<SettingsInfo | null>(null);
  $effect(() => {
    if (app.settingsOpen && app.s) {
      prefs = { ...app.s.prefs };
      api.settingsInfo().then((i) => (info = i));
    }
  });

  function key(e: KeyboardEvent) {
    if (e.key !== "Escape") return;
    if (ask.open) ask.resolve(null);
    else if (app.settingsOpen) app.settingsOpen = false;
  }
</script>

<svelte:window onkeydown={key} />

{#if app.settingsOpen && prefs}
  <div class="scrim">
    <div class="dialog" role="dialog" aria-modal="true" aria-labelledby="set-title">
      <h2 id="set-title">Settings</h2>
      <h3>Playback</h3>
      <label class="check" title="Decode on the graphics card. Falls back to software by itself if it doesn't work. Applies to clips opened from now on."><input type="checkbox" bind:checked={prefs.hw_decode} /> Hardware video decoding{info ? ` (${info.hw_api})` : ""}</label>
      {#if info}<p class="info">Status: {info.hw_status}</p>{/if}
      <label class="check" title="Hardware-decoded video stays in the graphics card's memory on its way to the viewer instead of being copied to main memory and back: less work for the processor and the memory bus, most of all with 4K. The first frame is checked against the usual copy, and if anything doesn't match it switches itself off. Windows only, for now. Applies to clips opened from now on."><input type="checkbox" bind:checked={prefs.zero_copy} disabled={!prefs.hw_decode} /> Keep decoded video on the graphics card (zero-copy)</label>
      {#if info}<p class="info">Zero-copy: {info.zero_copy_status}</p>{/if}
      <label class="check" title="Reelforge's own decoders, written in Rust. If one can't read a file, FFmpeg takes over by itself. For H.264, HEVC, VP9 and AV1, hardware decoding (when on and supported by the graphics card) is used first."><input type="checkbox" bind:checked={prefs.native_decoders} /> Use Reelforge's own decoders</label>
      {#if info}<p class="info">{info.native.length ? `For ${info.native.join(", ")}; FFmpeg decodes everything else` : "None yet"}</p>{/if}
      <label class="check"><input type="checkbox" bind:checked={prefs.scrub_audio} /> Audio scrubbing: hear the sound under the playhead while dragging it or stepping frames</label>
      <label class="check"><input type="checkbox" bind:checked={prefs.timeline_thumbnails} /> Picture thumbnails on timeline clips</label>
      <label class="check"><input type="checkbox" bind:checked={prefs.viewer_snapping} /> Snap layers in the Viewer to the frame and each other (hold Alt to place freely)</label>
      <label class="field">
        <span>Frame cache</span>
        <div class="row"><input type="number" min="128" max="16384" step="128" bind:value={prefs.cache_mb} style="width:100px" /> MB</div>
      </label>
      {#if info}<p class="info">Holding {info.cache_frames} frames ({info.cache_mb} MB) · {Math.round(info.cache_hits * 100)}% of requests served from memory</p>{/if}
      <label class="field" title="Memory for Render preview (Enter). A 1080p frame takes about 8 MB: 2 GB holds ~10 s at 25 fps.">
        <span>Render cache</span>
        <div class="row"><input type="number" min="256" max="32768" step="256" bind:value={prefs.render_cache_mb} style="width:100px" /> MB</div>
      </label>
      {#if info}<p class="info">Holding {info.render_frames} rendered frames ({info.render_mb} MB)</p>{/if}

      <h3>Proxies</h3>
      <label class="check" title="Small all-keyframe copies of heavy footage. Export always uses the original files."><input type="checkbox" bind:checked={prefs.use_proxies} /> Play proxies in the viewer when they're ready</label>
      <label class="check" title="Above 1080p, and 720p or more in HEVC, AV1, VP9, ProRes, DNxHD or CineForm. You can make one for any clip from the Media panel."><input type="checkbox" bind:checked={prefs.auto_proxies} /> Make proxies for heavy footage by themselves</label>
      {#if info}<p class="info">{info.proxies} prox{info.proxies === 1 ? "y" : "ies"}, {info.proxy_bytes >= 1e9 ? `${(info.proxy_bytes / 1e9).toFixed(1)} GB` : `${Math.round(info.proxy_bytes / 1e6)} MB`}</p>{/if}
      <div class="row">
        <button onclick={async () => run(api.openFolder(await api.proxyFolder()))}>Open the proxy folder</button>
        <button disabled={info?.proxies === 0} onclick={async () => (await run(api.proxiesDeleteAll()), (info = await api.settingsInfo()))} title="Free the disk space; heavy footage won't get new ones by itself until you ask (Media panel, right-click)">Delete all proxies</button>
      </div>

      <h3>Safety</h3>
      <label class="field">
        <span>Autosave every</span>
        <div class="row"><input type="number" min="0" max="60" bind:value={prefs.autosave_minutes} style="width:80px" /> minutes {prefs.autosave_minutes === 0 ? "(off)" : ""}</div>
      </label>
      {#if info}<p class="info path" title={info.autosave_dir}>Autosaves are kept in {info.autosave_dir}</p>{/if}
      <div class="row">
        <button onclick={() => info && run(api.openFolder(info.autosave_dir))}>Open the autosave folder</button>
      </div>
      <div class="buttons">
        <button onclick={() => (app.settingsOpen = false)}>Cancel</button>
        <button class="primary" onclick={async () => (await run(api.setPrefs(prefs!)), (app.settingsOpen = false))}>Save settings</button>
      </div>
    </div>
  </div>
{/if}

{#if ask.open}
  <div class="scrim">
    <div class="dialog small" role="alertdialog" aria-modal="true" aria-labelledby="ask-title">
      <h2 id="ask-title">{ask.title}</h2>
      <p>{ask.text}</p>
      {#if ask.field !== null}
        <!-- svelte-ignore a11y_autofocus -->
        <input class="field-in" type="text" bind:value={ask.field} autofocus spellcheck="false" aria-label={ask.title} onkeydown={(e) => e.key === "Enter" && ask.field?.trim() && ask.resolve(ask.buttons[0])} />
      {/if}
      <div class="buttons">
        {#each [...ask.buttons].reverse() as b, i}
          <button class:primary={i === ask.buttons.length - 1} disabled={ask.field !== null && i === ask.buttons.length - 1 && !ask.field.trim()} onclick={() => ask.resolve(b)}>{b}</button>
        {/each}
      </div>
    </div>
  </div>
{/if}

{#if (app.s?.recoveries.length ?? 0) > 0}
  <div class="scrim">
    <div class="dialog" role="dialog" aria-modal="true" aria-labelledby="rec-title">
      <h2 id="rec-title">Recover unsaved work?</h2>
      <p>Reelforge didn't close properly last time. Its autosave can be opened:</p>
      {#each app.s?.recoveries ?? [] as r, i}
        <div class="rec">
          <span>{r}</span>
          <button class="primary" onclick={() => run(api.recover(i))}>Recover</button>
          <button onclick={() => run(api.dismissRecovery(i))}>Discard</button>
        </div>
      {/each}
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
    z-index: 40;
  }
  .dialog {
    width: min(560px, calc(100vw - 32px));
    background: var(--panel);
    border: 1px solid var(--line);
    border-radius: 8px;
    padding: 18px 20px;
    box-shadow: 0 20px 60px rgba(0, 0, 0, 0.6);
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  .dialog:has(#set-title) {
    max-height: calc(100vh - 32px);
    overflow: auto;
  }
  h3 {
    font-size: calc(12.5px * var(--text-scale));
    font-weight: 600;
    color: var(--muted);
    text-transform: uppercase;
    letter-spacing: 0.04em;
    margin: 8px 0 0;
  }
  .info {
    color: var(--faint);
    font-size: calc(12px * var(--text-scale));
    margin: -4px 0 0 26px;
  }
  .info.path {
    margin-left: 0;
    word-break: break-all;
  }
  .dialog.small {
    width: min(420px, calc(100vw - 32px));
  }
  h2 {
    font-size: calc(16px * var(--text-scale));
    font-weight: 600;
    margin: 0 0 4px;
  }
  p {
    margin: 0;
  }
  .field {
    display: grid;
    grid-template-columns: 110px 1fr;
    align-items: center;
    gap: 8px;
  }
  .field > span {
    color: var(--muted);
  }
  .row {
    display: flex;
    gap: 6px;
    align-items: center;
  }
  .check {
    display: flex;
    gap: 8px;
    align-items: center;
  }
  .buttons {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 8px;
  }
  .rec {
    display: flex;
    align-items: center;
    gap: 8px;
    background: var(--base);
    border-radius: 5px;
    padding: 6px 8px;
  }
  .rec span {
    flex: 1;
  }
  .field-in {
    width: 100%;
    margin: 4px 0 12px;
  }
</style>
