<script lang="ts">
  // Edit > Paste attributes… (Ctrl+Alt+V): pick which parts of the copied
  // clip go onto the selected clips, as one undo step.
  import { app, run } from "../lib/session.svelte";
  import { api, type AttrOffers, type AttrSet } from "../lib/api";

  let offers = $state<AttrOffers | null>(null);
  // Remembered between openings.
  let set = $state<AttrSet>({ transform: false, crop: false, corner_pin: false, blend: false, effects: true, replace_effects: true, masks: false, audio: false, text_style: false });

  $effect(() => {
    if (!app.pasteAttrsOpen) return;
    void app.s?.rev;
    void app.s?.selected.length;
    api.attrOffers().then((o) => (offers = o));
  });

  type Row = { key: keyof AttrSet; label: string; tip: string; ok: (o: AttrOffers) => boolean };
  const rows: Row[] = [
    { key: "transform", label: "Position, scale, rotation, anchor and opacity", tip: "With their keyframes, at the same distance from the clip's start", ok: (o) => o.visual },
    { key: "crop", label: "Crop", tip: "The crop amounts (and keyframes)", ok: (o) => o.visual },
    { key: "corner_pin", label: "Corner pin", tip: "Where the four corners go (and keyframes)", ok: (o) => o.corner_pin },
    { key: "blend", label: "Blend mode", tip: "Normal, Add, Multiply or Screen", ok: (o) => o.visual },
    { key: "effects", label: "Effects", tip: "The whole effect stack, with keyframes: the same colour grade on every shot, say", ok: (o) => o.effects > 0 },
    { key: "masks", label: "Masks", tip: "Replaces their masks; shapes are scaled to each clip's picture size", ok: (o) => o.masks > 0 },
    { key: "audio", label: "Audio levels: volume, pan and fades", tip: "Onto audio clips (including those linked to the selected video clips), with volume keyframes", ok: (o) => o.audio },
    { key: "text_style", label: "Text style", tip: "Onto titles: font, size, colour, outline, shadow and box; each keeps its own text", ok: (o) => o.text_style },
  ];
  const label = (r: Row, o: AttrOffers) => (r.key === "effects" ? `Effects (${o.effects})` : r.key === "masks" ? `Masks (${o.masks})` : r.label);
  // What would actually be pasted (rows the copied clip can't give are off).
  const effective = $derived.by(() => {
    const o = offers;
    const v = { ...set };
    if (o) for (const r of rows) if (!r.ok(o)) (v[r.key] as boolean) = false;
    return v;
  });
  const any = $derived(rows.some((r) => effective[r.key]));

  function all(on: boolean) {
    for (const r of rows) (set[r.key] as boolean) = on && !!offers && r.ok(offers);
  }
  async function paste() {
    await run(api.pasteAttributes($state.snapshot(effective)));
    app.pasteAttrsOpen = false;
  }
  function key(e: KeyboardEvent) {
    if (!app.pasteAttrsOpen) return;
    if (e.key === "Escape") {
      e.stopPropagation();
      app.pasteAttrsOpen = false;
    } else if (e.key === "Enter" && offers && offers.targets && any) {
      e.stopPropagation();
      paste();
    }
  }
</script>

<svelte:window onkeydown={key} />

{#if app.pasteAttrsOpen}
  <div class="scrim" role="presentation" onpointerdown={(e) => e.target === e.currentTarget && (app.pasteAttrsOpen = false)}>
    <div class="dialog" role="dialog" aria-modal="true" aria-labelledby="pa-title">
      <h2 id="pa-title">Paste attributes</h2>
      {#if !offers}
        <p class="warn">Copy a clip first (Ctrl+C), then select the clips to paste onto.</p>
        <div class="buttons"><span class="grow"></span><button onclick={() => (app.pasteAttrsOpen = false)}>Close</button></div>
      {:else}
        <p>From: <b>{offers.from}</b></p>
        {#if offers.targets === 0}
          <p class="warn">Select the clips to paste onto (in the timeline).</p>
        {:else}
          <p class="muted">To: {offers.targets} selected clip{offers.targets === 1 ? "" : "s"}</p>
        {/if}
        <div class="rows">
          {#each rows as r}
            {@const ok = r.ok(offers)}
            <label class="check" class:off={!ok} title={ok ? r.tip : "The copied clip doesn't have this"}>
              <input type="checkbox" disabled={!ok} checked={effective[r.key] as boolean} onchange={(e) => ((set[r.key] as boolean) = e.currentTarget.checked)} />
              {label(r, offers)}
            </label>
            {#if r.key === "effects" && effective.effects}
              <div class="sub">
                <label class="check"><input type="radio" name="fxmode" checked={set.replace_effects} onchange={() => (set.replace_effects = true)} /> Replace their effects</label>
                <label class="check"><input type="radio" name="fxmode" checked={!set.replace_effects} onchange={() => (set.replace_effects = false)} /> Add after their effects</label>
              </div>
            {/if}
          {/each}
        </div>
        <div class="buttons">
          <button onclick={() => all(true)}>All</button>
          <button onclick={() => all(false)}>None</button>
          <span class="grow"></span>
          <button onclick={() => (app.pasteAttrsOpen = false)}>Cancel</button>
          <button class="primary" disabled={!offers.targets || !any} onclick={paste}>Paste</button>
        </div>
      {/if}
    </div>
  </div>
{/if}

<style>
  .scrim {
    position: fixed;
    inset: 0;
    background: rgba(10, 10, 12, 0.45);
    display: grid;
    place-items: center;
    z-index: 40;
  }
  .dialog {
    width: min(460px, calc(100vw - 32px));
    background: var(--panel);
    border: 1px solid var(--line);
    border-radius: 8px;
    padding: 16px 18px;
    box-shadow: 0 20px 60px rgba(0, 0, 0, 0.6);
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  h2 {
    font-size: calc(16px * var(--text-scale));
    font-weight: 600;
    margin: 0;
  }
  p {
    margin: 0;
  }
  b {
    font-weight: 600;
  }
  .rows {
    display: flex;
    flex-direction: column;
    gap: 5px;
    margin: 4px 0;
  }
  .check {
    display: flex;
    gap: 8px;
    align-items: center;
  }
  .check.off {
    color: var(--muted);
    opacity: 0.6;
  }
  .sub {
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding-left: 24px;
  }
  .warn {
    color: #f0a050;
  }
  .muted {
    color: var(--muted);
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
