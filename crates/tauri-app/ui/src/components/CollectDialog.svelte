<script lang="ts">
  // File > Collect project…: copy the project and every file it uses (video,
  // sound, pictures, LUTs, title fonts) into one folder, for a backup or to
  // move it to another computer. Runs in the background; closing the window
  // lets it carry on.
  import { app, run, say } from "../lib/session.svelte";
  import { api, type CollectPlanView } from "../lib/api";
  import { checkUnsaved } from "../lib/actions.svelte";

  let dest = $state("");
  let name = $state("");
  let includeUnused = $state(true);
  let includeFonts = $state(true);
  let plan = $state<CollectPlanView | null>(null);

  const job = $derived(app.live?.collect ?? null);

  // Opening: start from the project's name.
  let wasOpen = false;
  $effect(() => {
    const open = app.collectOpen;
    if (open && !wasOpen && !name) name = app.s?.title && app.s.title !== "Untitled" ? app.s.title : "project";
    wasOpen = open;
  });

  // The plan follows the choices (and the project).
  $effect(() => {
    const d = dest;
    const args = [name, includeUnused, includeFonts, app.s?.rev] as const;
    if (!app.collectOpen || !d) {
      plan = null;
      return;
    }
    api.collectPlan(d, args[0], args[1], args[2]).then((p) => (plan = p)).catch((e) => say(String(e)));
  });

  async function choose() {
    const d = await api.collectPickFolder();
    if (d) dest = d;
  }
  async function start() {
    if (!dest) return;
    await run(api.collectStart(dest, name, includeUnused, includeFonts));
  }
  function close() {
    app.collectOpen = false;
    if (job?.done) api.collectDismiss();
  }
  async function openResult(path: string) {
    if (!(await checkUnsaved())) return;
    api.collectDismiss();
    app.collectOpen = false;
    await run(api.openPath(path));
  }

  const size = (b: number) => (b >= 1e9 ? `${(b / 1e9).toFixed(2)} GB` : b >= 1e6 ? `${(b / 1e6).toFixed(1)} MB` : `${Math.max(1, Math.round(b / 1e3))} KB`);
  function key(e: KeyboardEvent) {
    if (app.collectOpen && e.key === "Escape") {
      e.stopPropagation();
      close();
    }
  }
</script>

<svelte:window onkeydown={key} />

{#if app.collectOpen}
  <div class="scrim">
    <div class="dialog" role="dialog" aria-modal="true" aria-labelledby="col-title">
      <h2 id="col-title">Collect project</h2>
      {#if job && job.running}
        <p>Copying <span class="tc">{job.file || "…"}</span> ({Math.min(job.index + 1, job.count)} of {job.count})</p>
        <div class="bar"><div class="fill" style="width:{job.total ? (job.bytes / job.total) * 100 : 100}%"></div></div>
        <p class="muted">{size(job.bytes)} of {size(job.total)}</p>
        <div class="buttons">
          <span class="muted grow">Closing this window lets it carry on.</span>
          <button onclick={() => api.collectStop()} disabled={job.stopping}>{job.stopping ? "Stopping…" : "Stop"}</button>
          <button onclick={close}>Close</button>
        </div>
      {:else if job?.done}
        {@const d = job.done}
        {#if d.ok}
          <p><strong>Done.</strong> {d.copied + d.reused} file{d.copied + d.reused === 1 ? "" : "s"} ({size(d.bytes)}) collected in {d.seconds.toFixed(1)} s into:</p>
          <p class="path">{d.folder}</p>
          {#if d.reused}<p class="muted">{d.reused} of them were already there from an earlier collect, so they weren't copied again.</p>{/if}
          {#if d.missing.length}
            <p class="warn">{d.missing.length} file{d.missing.length === 1 ? "" : "s"} couldn't be found and weren't collected:</p>
            <ul>{#each d.missing.slice(0, 6) as m}<li class="path">{m}</li>{/each}</ul>
          {/if}
          <p class="muted">Copy or move the whole folder; open the .rfproj inside it on any computer and everything is found.</p>
          <div class="buttons">
            <button onclick={() => run(api.openFolder(d.folder))}>Show folder</button>
            <span class="grow"></span>
            <button onclick={close}>Close</button>
            <button class="primary" onclick={() => openResult(d.project_file)}>Open the collected project</button>
          </div>
        {:else}
          <p class="warn">Collect didn't finish: {d.error}</p>
          <div class="buttons">
            <button onclick={() => api.collectDismiss()}>Back</button>
            <button onclick={close}>Close</button>
          </div>
        {/if}
      {:else}
        <p class="muted">Copies this project and every file it uses (video, sound, pictures, LUTs and title fonts) into one folder, for a backup or to move it to another computer.</p>
        <div class="form">
          <span class="lbl">Folder</span>
          <div class="row">
            <span class="path grow" title={dest}>{dest ? (dest.length > 46 ? "…" + dest.slice(-45) : dest) : "Not chosen yet"}</span>
            <button onclick={choose}>Choose…</button>
          </div>
          <span class="lbl">Project name</span>
          <input type="text" bind:value={name} aria-label="Project name" />
        </div>
        <label class="check"><input type="checkbox" bind:checked={includeUnused} /> Include media that isn't used in any sequence</label>
        <label class="check"><input type="checkbox" bind:checked={includeFonts} /> Include the fonts titles use <span class="muted">(so they look the same on another computer)</span></label>
        {#if !dest}
          <p class="muted">Choose a folder. An empty one is best: the files go into Media, LUTs and Fonts folders next to the project file.</p>
        {:else if plan}
          <div class="summary">
            <p><strong>{plan.files} file{plan.files === 1 ? "" : "s"}, {size(plan.bytes)}</strong> in all</p>
            {#if plan.already}<p class="muted">{plan.already} already in the folder from an earlier collect: they won't be copied again.</p>{/if}
            {#if plan.missing.length}
              <p class="warn">{plan.missing.length} file{plan.missing.length === 1 ? "" : "s"} can't be found and will be left out:</p>
              <ul>{#each plan.missing.slice(0, 6) as m}<li class="path">{m}</li>{/each}</ul>
            {/if}
            {#if plan.replaces}<p class="warn">{plan.project_file.split(/[\\/]/).pop()} is already there and will be replaced.</p>{/if}
            {#if plan.unsaved}<p class="muted">Unsaved changes are included.</p>{/if}
          </div>
        {/if}
        <div class="buttons">
          <span class="grow"></span>
          <button onclick={close}>Cancel</button>
          <button class="primary" disabled={!dest || !plan || !name.trim()} onclick={start}>Collect</button>
        </div>
      {/if}
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
    width: min(540px, calc(100vw - 32px));
    max-height: calc(100vh - 32px);
    overflow: auto;
    background: var(--panel);
    border: 1px solid var(--line);
    border-radius: 8px;
    padding: 18px 20px;
    box-shadow: 0 20px 60px rgba(0, 0, 0, 0.6);
    display: flex;
    flex-direction: column;
    gap: 10px;
  }
  h2 {
    font-size: calc(16px * var(--text-scale));
    font-weight: 600;
    margin: 0;
  }
  p {
    margin: 0;
  }
  .form {
    display: grid;
    grid-template-columns: 100px minmax(0, 1fr);
    gap: 8px 10px;
    align-items: center;
  }
  .lbl {
    color: var(--muted);
  }
  .row {
    display: flex;
    gap: 8px;
    align-items: center;
    min-width: 0;
  }
  .grow {
    flex: 1;
    min-width: 0;
  }
  .path {
    font-family: var(--font-tc);
    font-size: calc(12px * var(--text-scale));
    word-break: break-all;
  }
  .row .path {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    word-break: normal;
  }
  .check {
    display: flex;
    gap: 8px;
    align-items: center;
  }
  .summary {
    background: var(--base);
    border: 1px solid var(--line-soft);
    border-radius: 6px;
    padding: 8px 10px;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  ul {
    margin: 0;
    padding-left: 18px;
  }
  .warn {
    color: #f0a050;
  }
  .muted {
    color: var(--muted);
  }
  .bar {
    height: 8px;
    background: var(--well);
    border-radius: 4px;
    overflow: hidden;
  }
  .fill {
    height: 100%;
    background: var(--accent);
    transition: width 0.2s;
  }
  .buttons {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-top: 6px;
  }
</style>
