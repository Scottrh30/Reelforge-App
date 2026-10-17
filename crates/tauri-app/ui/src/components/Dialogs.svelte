<script lang="ts">
  // The modal windows: Export, Settings, questions, and crash recovery.
  import { untrack } from "svelte";
  import { app, run, timecode } from "../lib/session.svelte";
  import { api, type Prefs } from "../lib/api";
  import { ask } from "../lib/actions.svelte";

  let preset = $state(0);
  let path = $state("");
  let includeAudio = $state(true);
  let markedOnly = $state(false);
  const p = $derived(app.s?.presets[preset]);
  const hasMarks = $derived(app.s?.seq.in_point != null || app.s?.seq.out_point != null);
  const exp = $derived(app.live?.export ?? null);

  // Keep the file's extension right for the format (when the preset changes,
  // or the name has been typed).
  function fixExtension() {
    const ext = p?.extension;
    const v = path.trim();
    if (!ext || !v) return;
    const dot = v.lastIndexOf(".");
    const sep = Math.max(v.lastIndexOf("/"), v.lastIndexOf("\\"));
    const base = dot > sep ? v.slice(0, dot) : v;
    if (!v.toLowerCase().endsWith("." + ext)) path = `${base}.${ext}`;
  }
  $effect(() => {
    void p?.extension;
    untrack(fixExtension);
  });

  async function choose() {
    const r = await api.exportPathDialog(p?.extension ?? "mp4");
    if (r) path = r;
  }
  async function start() {
    fixExtension();
    if (!path) await choose();
    if (!path) return;
    await run(api.exportStart(preset, path, includeAudio, markedOnly && hasMarks));
  }

  let prefs = $state<Prefs | null>(null);
  $effect(() => {
    if (app.settingsOpen && app.s) prefs = { ...app.s.prefs };
  });

  function key(e: KeyboardEvent) {
    if (e.key !== "Escape") return;
    if (ask.open) ask.resolve(null);
    else if (app.settingsOpen) app.settingsOpen = false;
    else if (app.exportOpen && !exp?.running) app.exportOpen = false;
  }
  const fmtLeft = (s: number | null) => (s == null ? "" : s > 90 ? `about ${Math.round(s / 60)} min left` : `${Math.round(s)} s left`);
</script>

<svelte:window onkeydown={key} />

{#if app.exportOpen}
  <div class="scrim">
    <div class="dialog" role="dialog" aria-modal="true" aria-labelledby="exp-title">
      <h2 id="exp-title">Export {app.s?.seq.name ?? ""}</h2>
      {#if exp && (exp.running || exp.message)}
        <div class="progress">
          {#if exp.running}
            <div class="track"><div class="done" style="width:{exp.total ? (exp.done / exp.total) * 100 : 0}%"></div></div>
            <p class="tc">{exp.done} of {exp.total} frames · {fmtLeft(exp.remaining)}</p>
            <p class="muted path">{exp.path}</p>
            <div class="buttons"><button onclick={() => api.exportCancel()}>Cancel export</button></div>
          {:else}
            <p class:bad={exp.failed}>{exp.message}</p>
            <div class="buttons">
              {#if !exp.failed}<button onclick={() => api.showInFolder(exp.path)}>Show in folder</button>{/if}
              <button class="primary" onclick={() => (api.exportDismiss(), (app.exportOpen = false))}>Done</button>
            </div>
          {/if}
        </div>
      {:else}
        <label class="field">
          <span>Preset</span>
          <select bind:value={preset}>
            {#each app.s?.presets ?? [] as pr}<option value={pr.index}>{pr.label}</option>{/each}
          </select>
        </label>
        <p class="muted hint">{p?.hint}</p>
        <label class="field">
          <span>Save as</span>
          <div class="row">
            <input type="text" bind:value={path} placeholder="Choose where to save" onblur={fixExtension} />
            <button onclick={choose}>Browse…</button>
          </div>
        </label>
        <label class="check"><input type="checkbox" bind:checked={includeAudio} /> Include sound</label>
        <label class="check" class:off={!hasMarks}>
          <input type="checkbox" bind:checked={markedOnly} disabled={!hasMarks} />
          Only between the in and out marks
          {#if hasMarks}<span class="muted tc">({timecode(app.s?.seq.in_point ?? 0)} to {timecode(app.s?.seq.out_point ?? app.s?.seq.duration ?? 0)})</span>{:else}<span class="muted">(set them with I and O)</span>{/if}
        </label>
        <div class="buttons">
          <button onclick={() => (app.exportOpen = false)}>Cancel</button>
          <button class="primary" onclick={start} disabled={(app.s?.seq.duration ?? 0) === 0}>Export</button>
        </div>
      {/if}
    </div>
  </div>
{/if}

{#if app.settingsOpen && prefs}
  <div class="scrim">
    <div class="dialog" role="dialog" aria-modal="true" aria-labelledby="set-title">
      <h2 id="set-title">Settings</h2>
      <label class="check"><input type="checkbox" bind:checked={prefs.use_proxies} /> Play proxies in the viewer when they're ready</label>
      <label class="check"><input type="checkbox" bind:checked={prefs.auto_proxies} /> Make proxies for large footage by themselves</label>
      <label class="field">
        <span>Autosave every</span>
        <div class="row"><input type="number" min="0" max="60" bind:value={prefs.autosave_minutes} style="width:80px" /> minutes (0 turns it off)</div>
      </label>
      <label class="field">
        <span>Render cache</span>
        <div class="row"><input type="number" min="256" max="32768" step="256" bind:value={prefs.render_cache_mb} style="width:100px" /> MB</div>
      </label>
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
      <div class="buttons">
        {#each [...ask.buttons].reverse() as b, i}
          <button class:primary={i === ask.buttons.length - 1} onclick={() => ask.resolve(b)}>{b}</button>
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
  .row input[type="text"] {
    flex: 1;
  }
  .hint {
    margin-left: 118px;
    font-size: calc(12px * var(--text-scale));
    min-height: 2.8em;
  }
  .check {
    display: flex;
    gap: 8px;
    align-items: center;
  }
  .check.off {
    color: var(--faint);
  }
  .buttons {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 8px;
  }
  .progress .track {
    height: 8px;
    background: var(--base);
    border-radius: 4px;
    overflow: hidden;
    margin: 6px 0 8px;
  }
  .done {
    height: 100%;
    background: var(--accent);
    transition: width 0.2s;
  }
  .path {
    font-size: calc(12px * var(--text-scale));
    word-break: break-all;
    margin-top: 4px;
  }
  .bad {
    color: var(--red);
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
</style>
