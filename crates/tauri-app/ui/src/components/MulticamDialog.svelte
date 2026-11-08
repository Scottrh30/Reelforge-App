<script lang="ts">
  // Timeline > New multicam source… (or a media item's menu): pick the
  // takes, in angle order, and how to line them up.
  import { app, run } from "../lib/session.svelte";
  import { api } from "../lib/api";

  const takes = $derived((app.s?.media ?? []).filter((m) => !m.missing && m.duration > 0 && (m.kind === "video" || m.has_audio)));
  let chosen = $state<number[]>([]);
  let sync = $state<"audio" | "start">("audio");
  $effect(() => {
    if (app.multicamOpen) chosen = app.multicamOpen.filter((id) => takes.some((m) => m.id === id));
  });
  const toggle = (id: number) => (chosen = chosen.includes(id) ? chosen.filter((x) => x !== id) : [...chosen, id]);
  const noSound = $derived(chosen.filter((id) => !takes.find((m) => m.id === id)?.has_audio).length);

  async function make() {
    const ids = chosen;
    app.multicamOpen = null;
    await run(api.createMulticam(ids, sync));
  }
</script>

<svelte:window onkeydown={(e) => app.multicamOpen && e.key === "Escape" && (app.multicamOpen = null)} />

{#if app.multicamOpen}
  <div class="scrim">
    <div class="dialog" role="dialog" aria-modal="true" aria-labelledby="mc-title">
      <h2 id="mc-title">New multicam source</h2>
      <p class="muted">Each take becomes a camera angle, in the order you tick them. The source goes on the timeline at the playhead; play it and press 1–9 (or click the Viewer's grid) to cut between angles.</p>
      <fieldset class="list">
        <legend>Takes ({chosen.length} chosen)</legend>
        {#each takes as m}
          {@const at = chosen.indexOf(m.id)}
          <label class="check">
            <input type="checkbox" checked={at >= 0} onchange={() => toggle(m.id)} />
            <span class="num">{at >= 0 ? at + 1 : ""}</span>
            <span class="nm">{m.name}</span>
            <span class="muted">{m.kind === "video" ? "" : "sound only"}{m.has_audio ? "" : "no sound"}</span>
          </label>
        {:else}
          <p class="muted">Import the takes first.</p>
        {/each}
      </fieldset>
      <fieldset>
        <legend>Line them up by</legend>
        <label class="check"><input type="radio" name="mc-sync" checked={sync === "audio"} onchange={() => (sync = "audio")} /> Sound (the same claps and words heard in each)</label>
        <label class="check"><input type="radio" name="mc-sync" checked={sync === "start"} onchange={() => (sync = "start")} /> Their starts (all begin together)</label>
      </fieldset>
      {#if sync === "audio" && noSound > 0}<p class="warn">{noSound === 1 ? "One take has" : `${noSound} takes have`} no sound and will start with the first.</p>{/if}
      <div class="buttons">
        <span class="grow"></span>
        <button onclick={() => (app.multicamOpen = null)}>Cancel</button>
        <button class="primary" disabled={chosen.length < 2} onclick={make}>Make multicam</button>
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
    width: min(520px, calc(100vw - 32px));
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
    font-size: calc(12.5px * var(--text-scale));
  }
  .muted {
    color: var(--muted);
  }
  .warn {
    color: var(--accent);
  }
  fieldset {
    border: 1px solid var(--line-soft);
    border-radius: 6px;
    padding: 6px 10px 8px;
    margin: 0;
    display: flex;
    flex-direction: column;
    gap: 4px;
    min-width: 0;
  }
  .list {
    max-height: 260px;
    overflow: auto;
  }
  legend {
    font-size: calc(12px * var(--text-scale));
    color: var(--muted);
    padding: 0 4px;
  }
  .check {
    display: flex;
    gap: 6px;
    align-items: center;
    font-size: calc(12.5px * var(--text-scale));
    min-width: 0;
  }
  .num {
    width: 14px;
    text-align: right;
    color: var(--accent);
    font-weight: 600;
  }
  .nm {
    flex: 1;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .buttons {
    display: flex;
    gap: 8px;
    align-items: center;
    margin-top: 6px;
  }
  .grow {
    flex: 1;
  }
</style>
