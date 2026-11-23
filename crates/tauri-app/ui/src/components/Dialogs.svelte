<script lang="ts">
  // The modal windows: Settings, questions, and crash recovery (Export is ExportDialog).
  import { untrack } from "svelte";
  import { app, run } from "../lib/session.svelte";
  import { api, type Prefs, type SettingsInfo } from "../lib/api";
  import { ask } from "../lib/actions.svelte";

  let prefs = $state<Prefs | null>(null);
  let info = $state<SettingsInfo | null>(null);
  let rend = $state<Awaited<ReturnType<typeof api.renderers>> | null>(null);
  $effect(() => {
    if (app.settingsOpen && app.s) {
      prefs = { ...app.s.prefs };
      api.settingsInfo().then((i) => (info = i));
      api.renderers().then((r) => (rend = r)).catch(() => (rend = null));
    }
  });
  const rname = (k: "gpu" | "cpu") => rend?.available.find((r) => r.kind === k)?.name;

  // Proxy formats, as the Media panel names them.
  const formatLabels: Record<string, string> = { ProResProxy: "ProRes 422 Proxy", DnxhrLb: "DNxHR LB", H264: "H.264 (all keyframes)" };
  const formatLabel = (f: string) => formatLabels[f] ?? f;
  // Proxy sizes, as the Media panel names them.
  const sizeLabels: Record<string, string> = { Small: "360p", Standard: "540p", Large: "720p", FullHd: "1080p" };
  const sizeLabel = (s: string | undefined) => sizeLabels[s ?? "Standard"] ?? "540p";
  // Ready proxies in another format or size than the ones chosen.
  const remakeFrom = $derived(
    prefs
      ? (app.s?.media ?? [])
          .filter((m) => m.proxy === "ready" && m.proxy_format && (m.proxy_format !== formatLabel(prefs!.proxy_format) || (m.proxy_size ?? "540p") !== sizeLabel(prefs!.proxy_size)))
          .map((m) => `${m.proxy_format}, ${m.proxy_size ?? "540p"}`)
      : [],
  );
  async function remake() {
    if (!prefs) return;
    await run(api.proxiesRemake(prefs.proxy_format, prefs.proxy_size ?? "Standard"));
    info = await api.settingsInfo();
  }

  function key(e: KeyboardEvent) {
    if (e.key !== "Escape") return;
    if (ask.open) {
      // Only the question closes (not a dialog behind it).
      e.stopImmediatePropagation();
      ask.resolve(null);
    } else if (app.settingsOpen) app.settingsOpen = false;
  }
  const size = (b: number) => (b >= 1 << 30 ? `${(b / (1 << 30)).toFixed(1)} GB` : `${Math.round(b / (1 << 20))} MB`);
  let moveFiles = $state(true);
  async function moveCache(choose: boolean) {
    const r = await run(api.cacheFolderSet(choose, moveFiles));
    if (prefs && r !== undefined) prefs.cache_folder = r;
    info = await api.settingsInfo();
  }
  async function freeDisk(what: "render" | "proxies" | "autosaves" | "fills") {
    await run(api.diskFree(what));
    info = await api.settingsInfo();
  }
</script>

<svelte:window onkeydown={key} />

{#if app.settingsOpen && prefs}
  <div class="scrim">
    <div class="dialog" role="dialog" aria-modal="true" aria-labelledby="set-title">
      <h2 id="set-title">Settings</h2>
      <label class="check" title="New projects from presets and your templates, and your recent projects, when Reelforge opens. File > Start screen shows it any time."><input type="checkbox" bind:checked={prefs.start_screen} /> Show the start screen when Reelforge opens</label>
      <label class="field" title="What draws the pictures, in the Viewer and in exports. The GPU (the graphics card) is fastest. The CPU runs the very same effects in software (WARP on Windows, lavapipe or llvmpipe on Linux): much slower, but the same picture, for a computer whose graphics card is missing, too old, out of memory or giving wrong results. The Viewer switches at once; exports started from now on use it.">
        <span>Render with</span>
        <select bind:value={prefs.renderer}>
          <option value="gpu" disabled={!!rend && !rname("gpu")}>GPU{rname("gpu") ? ` · ${rname("gpu")}` : rend ? " · none found" : ""}</option>
          <option value="cpu" disabled={!!rend && !rname("cpu")}>CPU (slower){rname("cpu") ? ` · ${rname("cpu")}` : rend ? " · none on this computer" : ""}</option>
        </select>
      </label>
      {#if rend?.device}<p class="info">Drawing on: {rend.device}</p>{/if}

      <h3>Playback</h3>
      <label class="check" title="Decode on the graphics card. Falls back to software by itself if it doesn't work. Applies to clips opened from now on."><input type="checkbox" bind:checked={prefs.hw_decode} /> Hardware video decoding{info ? ` (${info.hw_api})` : ""}</label>
      {#if info}<p class="info">Status: {info.hw_status}</p>{/if}
      <label class="check" title="Hardware-decoded video stays in the graphics card's memory on its way to the viewer instead of being copied to main memory and back: less work for the processor and the memory bus, most of all with 4K. The first frame is checked against the usual copy, and if anything doesn't match it switches itself off. Windows only, for now. Applies to clips opened from now on."><input type="checkbox" bind:checked={prefs.zero_copy} disabled={!prefs.hw_decode} /> Keep decoded video on the graphics card (zero-copy)</label>
      {#if info}<p class="info">Zero-copy: {info.zero_copy_status}</p>{/if}
      <label class="check" title="Reelforge's own decoders, written in Rust. If one can't read a file, FFmpeg takes over by itself. For H.264, HEVC, VP9 and AV1, hardware decoding (when on and supported by the graphics card) is used first."><input type="checkbox" bind:checked={prefs.native_decoders} /> Use Reelforge's own decoders</label>
      {#if info}<p class="info">{info.native.length ? `For ${info.native.join(", ")}; FFmpeg decodes everything else` : "None yet"}</p>{/if}
      {#if info}<p class="info" title="Camera RAW opens through the makers' own software: Blackmagic RAW through Blackmagic's SDK; R3D and ARRIRAW are converted once, in the background, to ProRes 4444 XQ in Rec.709 (kept with the proxies), which is what's edited and exported.">Camera RAW: {#each info.camera_raw as line, i}{#if i > 0}<br />{/if}{line}{/each}</p>{/if}
      <label class="check"><input type="checkbox" bind:checked={prefs.scrub_audio} /> Audio scrubbing: hear the sound under the playhead while dragging it or stepping frames</label>
      <label class="field" title="Pictures on video clips in the timeline. A filmstrip shows frames all along each clip (taller video tracks give bigger pictures: drag a track's lower edge)">
        <span>Clip pictures</span>
        <select value={!prefs.timeline_thumbnails ? "off" : prefs.timeline_filmstrip ? "strip" : "first"} onchange={(e) => {
          const v = e.currentTarget.value;
          if (!prefs) return;
          prefs.timeline_thumbnails = v !== "off";
          prefs.timeline_filmstrip = v === "strip";
        }}>
          <option value="strip">Filmstrip along each clip</option>
          <option value="first">First frame only</option>
          <option value="off">None</option>
        </select>
      </label>
      <label class="check" title="A band along the bottom of each video clip that has linked audio, showing that sound (at its volume and fades), so you can cut picture to sound without looking down at the audio tracks. Right-click a video track's header to show or hide them on that track alone"><input type="checkbox" bind:checked={prefs.timeline_video_waveforms} /> Show the linked sound's waveform under video clips</label>
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
      <label class="check" title="When you haven't edited, played or moved the playhead for a few seconds, the heavy parts of the sequence (effects, transitions, compositions, stacked layers) are rendered into the render cache by themselves, so they play smoothly. It stops the moment you do anything, and waits while exporting, tracking or making proxies."><input type="checkbox" bind:checked={prefs.background_render} /> Render in the background when idle</label>
      <label class="field" title="How long nothing has to happen before background rendering starts.">
        <span>…after</span>
        <div class="row"><input type="number" min="1" max="600" step="1" disabled={!prefs.background_render} bind:value={prefs.background_render_after_s} style="width:70px" /> seconds idle</div>
      </label>
      <label class="field" title="Rendered previews are also kept on disk, in the cache folder, so reopening a project (or another using the same clips and effects) finds them again. The least recently used go first when it's full. 0 keeps them in memory only.">
        <span>…and on disk</span>
        <div class="row"><input type="number" min="0" max="1048576" step="1024" bind:value={prefs.render_cache_disk_mb} style="width:100px" /> MB</div>
      </label>
      {#if info}
        <p class="info">
          {info.render_disk_frames} frames on disk ({info.render_disk_mb} MB)
          <button class="tiny" disabled={!info.render_frames && !info.render_disk_frames} onclick={async () => { await run(api.clearRenderCache()); info = await api.settingsInfo(); }} title="Forget every rendered preview, in memory and on disk">Clear</button>
        </p>
      {/if}

      <label class="field" title="How sharp shadows from 3D layers and 3D text can be: each caster's outline is drawn into a tile of 256 (Low), 512 (Standard) or 1024 (High) pixels. Higher is crisper for big layers and long shadows, and takes more graphics memory (1.6, 6.5 or 26 MB). Up to 25 layers cast shadows in a frame. Exports use it too.">
        <span>3D shadow quality</span>
        <select bind:value={prefs.shadow_quality}>
          <option value={0}>Low</option>
          <option value={1}>Standard</option>
          <option value={2}>High</option>
        </select>
      </label>

      <h3>Proxies</h3>
      <label class="check" title="Small all-keyframe copies of heavy footage. Export always uses the original files."><input type="checkbox" bind:checked={prefs.use_proxies} /> Play proxies in the viewer when they're ready</label>
      <label class="check" title="Above 1080p, and 720p or more in HEVC, AV1, VP9, ProRes, DNxHD or CineForm. You can make one for any clip from the Media panel."><input type="checkbox" bind:checked={prefs.auto_proxies} /> Make proxies for heavy footage by themselves</label>
      <label class="field" title="All three have every frame a keyframe, for instant jumps. H.264 is the smallest and decodes on the graphics card. ProRes 422 Proxy (10-bit) and DNxHR LB are 4:2:2 colour, about three times the size, lighter for the processor, and open in other editing programs as standard offline media.">
        <span>Make proxies as</span>
        <select bind:value={prefs.proxy_format}>
          <option value="H264">H.264 (smallest files)</option>
          <option value="ProResProxy">ProRes 422 Proxy (10-bit 4:2:2)</option>
          <option value="DnxhrLb">DNxHR LB (4:2:2, lightest to decode)</option>
        </select>
      </label>
      <label class="field" title="How big proxies are made. Smaller ones play more easily on slow computers and with very heavy 4K/8K projects; bigger ones are sharper, for judging focus, and take more disk space and decoding.">
        <span>Proxy size</span>
        <select bind:value={prefs.proxy_size}>
          <option value="Small">360p (640×360, lightest)</option>
          <option value="Standard">540p (960×540, the standard)</option>
          <option value="Large">720p (1280×720, sharper)</option>
          <option value="FullHd">1080p (1920×1080, sharpest)</option>
        </select>
      </label>
      {#if remakeFrom.length}
        <p class="info">
          {remakeFrom.length} {remakeFrom.length === 1 ? "proxy is" : "proxies are"} in another format or size ({[...new Set(remakeFrom)].join("; ")}). They're still used.
          <button class="link" onclick={remake}>Make {remakeFrom.length === 1 ? "it" : "them"} again as {formatLabel(prefs.proxy_format)}, {sizeLabel(prefs.proxy_size)}</button>
        </p>
      {/if}
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
      <h3>Disk</h3>
      {#if info}
        <p class="info">{size(info.free_bytes)} free on the drive where Reelforge keeps its files</p>
        <div class="disk">
          <span>Rendered previews</span><span class="n">{info.render_disk_mb >= 1024 ? `${(info.render_disk_mb / 1024).toFixed(1)} GB` : `${info.render_disk_mb} MB`}</span>
          <button class="tiny" disabled={!info.render_disk_frames && !info.render_frames} onclick={() => freeDisk("render")} title="Forget every rendered preview, in memory and on disk (made again when needed)">Clear</button>
          <span>Proxies</span><span class="n">{size(info.proxy_bytes)}</span>
          <button class="tiny" disabled={!info.unused_proxies} onclick={() => freeDisk("proxies")} title="Delete the proxies of footage that isn't in this project ({info.unused_proxies}, {size(info.unused_proxy_bytes)}); others' are made again if those projects are opened">Delete {info.unused_proxies} unused</button>
          <span>Object removal fills</span><span class="n">{size(info.fill_bytes)}</span>
          <button class="tiny" disabled={!info.unused_fills} onclick={() => freeDisk("fills")} title="Delete the fills nothing uses any more ({info.unused_fills}, {size(info.unused_fill_bytes)}): earlier fills replaced by Fill again, and ones whose project is gone or no longer has them. Other saved projects' fills are kept">Delete {info.unused_fills} unused</button>
          <span>Autosaves</span><span class="n">{info.autosaves} · {size(info.autosave_bytes)}</span>
          <button class="tiny" disabled={info.autosaves <= 1} onclick={() => freeDisk("autosaves")} title="Delete every autosave but this session's (crash recovery for earlier sessions goes with them)">Delete old</button>
        </div>
      {/if}
      <div class="field" title="Proxies and rendered previews can go on another drive: a fast SSD for smooth playback, or a bigger disk. Autosaves stay in the app's own folder.">
        <span>Cache folder</span>
        <div class="cachedir">
          <span class="path" title={info?.media_cache_dir}>{info?.media_cache_dir ?? "…"}</span>
          <div class="row">
            <button class="tiny" onclick={() => moveCache(true)}>Choose…</button>
            <button class="tiny" disabled={!prefs.cache_folder} onclick={() => moveCache(false)} title="Back to the app's own cache folder">Default</button>
            <label class="check" title="Move the proxies and rendered previews there now to the new folder (in the background); otherwise they stay, and new ones are made in the new place"><input type="checkbox" bind:checked={moveFiles} /> Move what's there</label>
          </div>
        </div>
      </div>
      <label class="field" title="When the drive Reelforge saves to (rendered previews, proxies, autosaves, the project) has less free than this, a warning offers to clear space, and previews and proxies stop filling it. 0 never warns.">
        <span>Warn below</span>
        <div class="row"><input type="number" min="0" max="1000" step="0.5" bind:value={prefs.disk_warn_gb} style="width:80px" /> GB free {prefs.disk_warn_gb === 0 ? "(never)" : ""}</div>
      </label>

      <div class="buttons">
        <button onclick={() => (app.settingsOpen = false)}>Cancel</button>
        <button class="primary" onclick={async () => (await run(api.setPrefs(prefs!)), (app.settingsOpen = false))}>Save settings</button>
      </div>
    </div>
  </div>
{/if}

{#if ask.open}
  <div class="scrim top">
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

{#if app.s?.import_report}
  {@const r = app.s.import_report}
  <div class="scrim">
    <div class="dialog" role="dialog" aria-modal="true" aria-labelledby="imp-title">
      <h2 id="imp-title">Opened from {r.from}</h2>
      <p><b>{r.name}</b>: {r.summary}.{#if r.missing} {r.missing} media file{r.missing === 1 ? "" : "s"} aren't where the project says: relink {r.missing === 1 ? "it" : "them"} in the Media panel.{/if}</p>
      {#if r.notes.length}
        <p class="muted">Not everything has a match in Reelforge. Left out or changed:</p>
        <ul class="notes">
          {#each r.notes as n}<li>{n}</li>{/each}
        </ul>
      {:else}
        <p class="muted">Everything came across.</p>
      {/if}
      <p class="muted">It's a new project: save it to keep it. The original is untouched.</p>
      <div class="buttons"><button class="primary" onclick={() => run(api.dismissImportReport())}>OK</button></div>
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
  .notes {
    margin: 0;
    padding-left: 18px;
    max-height: 40vh;
    overflow: auto;
    font-size: calc(12px * var(--text-scale));
  }
  .muted {
    color: var(--muted);
    margin: 0;
  }
  .scrim {
    position: fixed;
    inset: 0;
    background: rgba(10, 10, 12, 0.55);
    display: grid;
    place-items: center;
    z-index: 40;
  }
  /* Questions can come from other dialogs (the effects browser), so above them. */
  .scrim.top {
    z-index: 60;
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
  button.link {
    background: none;
    border: none;
    padding: 0;
    color: var(--accent);
    text-decoration: underline;
    cursor: pointer;
    font: inherit;
  }
  .info .tiny {
    min-height: 18px;
    padding: 0 6px;
    margin-left: 6px;
    font-size: calc(11px * var(--text-scale));
  }
  .cachedir {
    display: flex;
    flex-direction: column;
    gap: 4px;
    min-width: 0;
  }
  .cachedir .path {
    font-family: ui-monospace, monospace;
    font-size: calc(11px * var(--text-scale));
    color: var(--muted);
    overflow-wrap: anywhere;
  }
  .disk {
    display: grid;
    grid-template-columns: auto auto auto;
    justify-content: start;
    align-items: center;
    gap: 4px 14px;
    margin: 4px 0 8px;
  }
  .disk .n {
    color: var(--muted);
    font-variant-numeric: tabular-nums;
    text-align: right;
  }
</style>
