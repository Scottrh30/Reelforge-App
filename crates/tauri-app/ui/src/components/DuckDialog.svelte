<script lang="ts">
  // Timeline > Duck music under dialogue…: pick the dialogue and music audio
  // tracks and how much and how fast; Reelforge listens to the dialogue and
  // writes volume keyframes on the music clips.
  import { app, run } from "../lib/session.svelte";
  import { api, type TrackView } from "../lib/api";

  const STORE = "reelforge.duck";
  type Opts = { amount: number; threshold: number; down: number; up: number; hold: number };
  const DEFAULTS: Opts = { amount: -12, threshold: -32, down: 0.25, up: 0.6, hold: 0.8 };
  function load(): Opts {
    try {
      const v = JSON.parse(localStorage.getItem(STORE) ?? "null");
      if (v && typeof v === "object") return { ...DEFAULTS, ...v };
    } catch {}
    return { ...DEFAULTS };
  }
  let o = $state<Opts>(load());

  const tracks = $derived((app.s?.seq.tracks ?? []).filter((t) => t.kind === "audio"));
  let dialogue = $state<number[]>([]);
  let music = $state<number[]>([]);
  // Starting guess: tracks named like speech are dialogue, the first audio track otherwise; the rest are music.
  $effect(() => {
    if (!app.duckOpen) return;
    const list = tracks;
    const talky = (t: TrackView) => /dia|voice|vo\b|speech|narr|interview|talk/i.test(t.name);
    const d = list.filter(talky).map((t) => t.index);
    dialogue = d.length ? d : list.slice(0, 1).map((t) => t.index);
    music = list.filter((t) => !dialogue.includes(t.index)).map((t) => t.index);
  });
  const toggle = (list: number[], i: number) => (list.includes(i) ? list.filter((x) => x !== i) : [...list, i]);
  const ready = $derived(dialogue.length > 0 && music.length > 0 && !dialogue.some((i) => music.includes(i)));

  function save() {
    try {
      localStorage.setItem(STORE, JSON.stringify(o));
    } catch {}
  }
  async function duck() {
    save();
    await run(api.autoDuck(dialogue, music, o.amount, o.threshold, o.down, o.up, o.hold));
    app.duckOpen = false;
  }
  async function undoDuck() {
    await run(api.removeDucking(music));
    app.duckOpen = false;
  }
  const num = (e: Event) => Number((e.currentTarget as HTMLInputElement).value);
</script>

<svelte:window onkeydown={(e) => app.duckOpen && e.key === "Escape" && (app.duckOpen = false)} />

{#if app.duckOpen}
  <div class="scrim">
    <div class="dialog" role="dialog" aria-modal="true" aria-labelledby="duck-title">
      <h2 id="duck-title">Duck music under dialogue</h2>
      <p class="muted">Reelforge listens to the dialogue and lowers the music wherever someone speaks, as volume keyframes on the music clips. You can fine-tune them afterwards, or duck again to replace them.</p>
      {#if tracks.length < 2}
        <p class="warn">This needs at least two audio tracks: one with the dialogue and one with the music.</p>
      {:else}
        <div class="cols">
          <fieldset>
            <legend>Dialogue (listen to)</legend>
            {#each tracks as t}
              <label class="check"><input type="checkbox" checked={dialogue.includes(t.index)} onchange={() => ((dialogue = toggle(dialogue, t.index)), (music = music.filter((x) => x !== t.index)))} /> {t.name}{t.muted ? " (muted)" : ""}</label>
            {/each}
          </fieldset>
          <fieldset>
            <legend>Music (lower)</legend>
            {#each tracks as t}
              <label class="check"><input type="checkbox" checked={music.includes(t.index)} onchange={() => ((music = toggle(music, t.index)), (dialogue = dialogue.filter((x) => x !== t.index)))} /> {t.name}</label>
            {/each}
          </fieldset>
        </div>
        <div class="grid">
          <label for="duck-amount" title="How far the music goes down while someone speaks">Lower music by</label>
          <div class="row"><input id="duck-amount" type="range" min="-30" max="-3" step="1" value={o.amount} oninput={(e) => (o.amount = num(e))} /><span class="val">{-o.amount} dB</span></div>
          <label for="duck-threshold" title="Dialogue louder than this counts as speech. Lower it if quiet speakers are missed; raise it if background noise ducks the music">Speech louder than</label>
          <div class="row"><input id="duck-threshold" type="range" min="-60" max="-6" step="1" value={o.threshold} oninput={(e) => (o.threshold = num(e))} /><span class="val">{o.threshold} dB</span></div>
          <label for="duck-down" title="The music starts going down this long before the speech">Fade down</label>
          <div class="row"><input id="duck-down" type="range" min="0" max="2" step="0.05" value={o.down} oninput={(e) => (o.down = num(e))} /><span class="val">{o.down.toFixed(2)} s</span></div>
          <label for="duck-up" title="How long the music takes to come back up after the speech">Fade up</label>
          <div class="row"><input id="duck-up" type="range" min="0" max="3" step="0.05" value={o.up} oninput={(e) => (o.up = num(e))} /><span class="val">{o.up.toFixed(2)} s</span></div>
          <label for="duck-hold" title="Pauses in the speech shorter than this keep the music down, so it doesn't bob up between sentences">Stay down through pauses up to</label>
          <div class="row"><input id="duck-hold" type="range" min="0" max="4" step="0.1" value={o.hold} oninput={(e) => (o.hold = num(e))} /><span class="val">{o.hold.toFixed(1)} s</span></div>
        </div>
      {/if}
      <div class="buttons">
        <button class="ghost" onclick={() => (o = { ...DEFAULTS })}>Defaults</button>
        <span class="grow"></span>
        <button disabled={music.length === 0} onclick={undoDuck} title="Take the ducking off the music tracks ticked above (keyframes you added yourself stay)">Remove ducking</button>
        <button onclick={() => (app.duckOpen = false)}>Cancel</button>
        <button class="primary" disabled={!ready} onclick={duck}>Duck music</button>
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
    width: min(560px, calc(100vw - 32px));
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
  .cols {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 10px;
  }
  @media (max-width: 480px) {
    .cols {
      grid-template-columns: 1fr;
    }
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
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .grid {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 6px 12px;
    align-items: center;
    font-size: calc(12.5px * var(--text-scale));
  }
  .row {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
  }
  .row input {
    flex: 1;
    min-width: 0;
  }
  .val {
    width: 52px;
    text-align: right;
    font-family: var(--font-tc);
    color: var(--muted);
  }
  .buttons {
    display: flex;
    gap: 8px;
    align-items: center;
    flex-wrap: wrap;
    margin-top: 6px;
  }
  .grow {
    flex: 1;
  }
</style>
