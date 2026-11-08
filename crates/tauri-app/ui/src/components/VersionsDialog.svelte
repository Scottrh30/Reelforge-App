<script lang="ts">
  // File > Project versions…: keep the project as it is now under a name,
  // and go back to (or open a copy of) any version kept before.
  import { app, run } from "../lib/session.svelte";
  import { api, type VersionsView, type ProjectVersion } from "../lib/api";
  import { question } from "../lib/actions.svelte";

  let list = $state<VersionsView | null>(null);
  let name = $state("");
  let note = $state("");
  let editing = $state<string | null>(null);
  let editName = $state("");
  let editNote = $state("");
  let showAuto = $state(true);

  async function refresh() {
    list = await api.versionsList();
  }
  $effect(() => {
    if (app.versionsOpen) {
      name = "";
      note = "";
      editing = null;
      refresh();
    }
  });
  const shown = $derived((list?.versions ?? []).filter((v) => showAuto || !v.auto));
  const when = (secs: number) => new Date(secs * 1000).toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" });

  async function keep() {
    if (await run(api.saveVersion(name, note))) {
      name = "";
      note = "";
    }
    refresh();
  }
  async function restore(v: ProjectVersion) {
    const a = await question(`Restore "${v.name}"?`, "The project goes back to how it was then. What's there now is kept as a version first, and Undo brings it back too. Nothing is written to the project file until you save.", ["Restore", "Cancel"]);
    if (a !== "Restore") return;
    if (await run(api.restoreVersion(v.file))) app.versionsOpen = false;
    else refresh();
  }
  async function openCopy(v: ProjectVersion) {
    if (app.live?.dirty) {
      const a = await question(`Open "${v.name}" as a new project?`, "The project open now has unsaved changes: keep a version or save it first, or they're lost.", ["Open anyway", "Cancel"]);
      if (a !== "Open anyway") return;
    }
    if (await run(api.openVersionCopy(v.file))) app.versionsOpen = false;
  }
  function startEdit(v: ProjectVersion) {
    editing = v.file;
    editName = v.name;
    editNote = v.note;
  }
  async function saveEdit() {
    if (editing) await run(api.renameVersion(editing, editName, editNote));
    editing = null;
    refresh();
  }
  async function remove(v: ProjectVersion) {
    const a = await question(`Delete "${v.name}"?`, "Its file goes from the versions folder. This can't be undone.", ["Delete", "Cancel"]);
    if (a !== "Delete") return;
    await run(api.deleteVersion(v.file));
    refresh();
  }
  async function toggleOnSave() {
    await run(api.setVersionsOnSave(!list?.on_save));
    refresh();
  }
</script>

<svelte:window onkeydown={(e) => app.versionsOpen && e.key === "Escape" && !editing && (app.versionsOpen = false)} />

{#if app.versionsOpen}
  <div class="scrim">
    <div class="dialog" role="dialog" aria-modal="true" aria-labelledby="ver-title">
      <h2 id="ver-title">Project versions</h2>
      {#if !list}
        <p class="muted">Reading…</p>
      {:else if !list.folder}
        <p class="warn">Save the project first: its versions are kept in a folder beside it.</p>
      {:else}
        <p class="muted">Kept in <span class="path" title={list.folder}>{list.folder}</span></p>
        <div class="keep">
          <input placeholder="Name (e.g. Rough cut for the client)" bind:value={name} aria-label="Version name" onkeydown={(e) => e.key === "Enter" && keep()} />
          <input placeholder="Note (optional)" bind:value={note} aria-label="Version note" onkeydown={(e) => e.key === "Enter" && keep()} />
          <button class="primary" onclick={keep} title="Keep the project as it is now (Ctrl+Alt+S)">Keep this version</button>
        </div>
        <div class="opts">
          <label class="check"><input type="checkbox" checked={list.on_save} onchange={toggleOnSave} /> Keep a version every time I save (the newest 30)</label>
          <label class="check"><input type="checkbox" bind:checked={showAuto} /> Show those</label>
        </div>
        <div class="list" role="list">
          {#each shown as v (v.file)}
            <div class="ver" role="listitem" class:auto={v.auto}>
              {#if editing === v.file}
                <div class="edit">
                  <input bind:value={editName} aria-label="Name" onkeydown={(e) => e.key === "Enter" && saveEdit()} />
                  <input bind:value={editNote} placeholder="Note" aria-label="Note" onkeydown={(e) => (e.key === "Enter" ? saveEdit() : e.key === "Escape" && (editing = null))} />
                  <button class="small primary" onclick={saveEdit}>OK</button>
                  <button class="small" onclick={() => (editing = null)}>Cancel</button>
                </div>
              {:else}
                <div class="head">
                  <span class="nm">{v.name}</span>
                  {#if v.auto}<span class="badge" title="Kept by itself when the project was saved">on save</span>{/if}
                  <span class="grow"></span>
                  <span class="muted when">{when(v.saved)}</span>
                </div>
                {#if v.note}<div class="note">{v.note}</div>{/if}
                <div class="sum muted">{v.summary}</div>
                <div class="btns">
                  <button class="small" onclick={() => restore(v)} title="Put the project back to this version (undoable)">Restore</button>
                  <button class="small" onclick={() => openCopy(v)} title="Open it as a separate, unsaved project">Open as copy</button>
                  <button class="small" onclick={() => startEdit(v)} title={v.auto ? "Name it to keep it (named versions aren't cleared out)" : "Rename, or change the note"}>{v.auto ? "Name it" : "Rename"}</button>
                  <button class="small ghost" onclick={() => remove(v)}>Delete</button>
                </div>
              {/if}
            </div>
          {:else}
            <p class="muted">No versions yet. Keep one above, or save the project.</p>
          {/each}
        </div>
      {/if}
      <div class="buttons">
        <span class="grow"></span>
        <button onclick={() => (app.versionsOpen = false)}>Close</button>
      </div>
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
    width: min(640px, calc(100vw - 32px));
    max-height: calc(100vh - 32px);
    overflow: hidden;
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
    font-size: calc(12.5px * var(--text-scale));
  }
  .muted {
    color: var(--muted);
  }
  .warn {
    color: var(--accent);
  }
  .path {
    font-family: var(--font-tc);
    word-break: break-all;
  }
  .keep {
    display: grid;
    grid-template-columns: 1fr 1fr auto;
    gap: 6px;
  }
  @media (max-width: 520px) {
    .keep {
      grid-template-columns: 1fr;
    }
  }
  .keep input,
  .edit input {
    min-width: 0;
  }
  .opts {
    display: flex;
    gap: 16px;
    flex-wrap: wrap;
  }
  .check {
    display: flex;
    gap: 6px;
    align-items: center;
    font-size: calc(12.5px * var(--text-scale));
  }
  .list {
    overflow: auto;
    min-height: 80px;
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding-right: 2px;
  }
  .ver {
    border: 1px solid var(--line-soft);
    border-radius: 6px;
    padding: 8px 10px;
    display: flex;
    flex-direction: column;
    gap: 3px;
    font-size: calc(12.5px * var(--text-scale));
  }
  .ver.auto {
    border-style: dashed;
  }
  .head {
    display: flex;
    align-items: baseline;
    gap: 8px;
    min-width: 0;
  }
  .nm {
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .badge {
    font-size: calc(10.5px * var(--text-scale));
    color: var(--muted);
    border: 1px solid var(--line);
    border-radius: 3px;
    padding: 0 4px;
  }
  .when {
    white-space: nowrap;
  }
  .sum {
    font-size: calc(11.5px * var(--text-scale));
  }
  .btns,
  .edit {
    display: flex;
    gap: 6px;
    flex-wrap: wrap;
    margin-top: 3px;
  }
  .edit input {
    flex: 1;
  }
  .buttons {
    display: flex;
    gap: 8px;
    align-items: center;
  }
  .grow {
    flex: 1;
  }
</style>
