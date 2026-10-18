<script lang="ts">
  // The modal windows: Settings, questions, and crash recovery (Export is ExportDialog).
  import { untrack } from "svelte";
  import { app, run } from "../lib/session.svelte";
  import { api, type Prefs } from "../lib/api";
  import { ask } from "../lib/actions.svelte";

  let prefs = $state<Prefs | null>(null);
  $effect(() => {
    if (app.settingsOpen && app.s) prefs = { ...app.s.prefs };
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
</style>
