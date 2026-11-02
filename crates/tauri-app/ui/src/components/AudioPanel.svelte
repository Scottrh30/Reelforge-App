<script lang="ts">
  // The selected audio clip's sound: volume and pan (with keyframes at the
  // playhead), fades, and Reset. With keyframes, the sliders show and set the
  // value at the playhead, keying it there.
  import { app, run, timecode } from "../lib/session.svelte";
  import { api, type ClipAudio, type Inspector } from "../lib/api";
  import NumberField from "./NumberField.svelte";
  import AudioEffects from "./AudioEffects.svelte";
  import { MIN_DB, MAX_DB, volKeys, panKeys, volumeAt, panAt, setVolKey, setPanKey, dbText, panText, defaultAudio } from "../lib/audio";
  import { tps } from "../lib/session.svelte";

  let { ins }: { ins: Inspector } = $props();
  const a = $derived(ins.audio!);
  const ph = $derived(app.live?.playhead ?? 0);
  const onClip = $derived(ph >= ins.start && ph < ins.start + ins.duration);
  // Source time at the playhead (keys are anchored to it).
  const src = $derived(ins.source_in + Math.max(0, Math.min(ins.duration, ph - ins.start)));
  const toTimeline = (at: number) => ins.start + (at - ins.source_in);

  // A local copy while dragging, so the slider doesn't jump back between updates.
  let draft = $state<ClipAudio | null>(null);
  $effect(() => {
    void ins.id;
    draft = null;
  });
  const shown = $derived(draft ?? a);

  function edit(f: (n: ClipAudio) => void) {
    const n = structuredClone($state.snapshot(shown)) as ClipAudio;
    f(n);
    draft = n;
    api.setClipAudio(ins.id, n);
  }
  async function commit() {
    await run(api.commit());
    draft = null;
  }
  async function once(f: (n: ClipAudio) => void) {
    edit(f);
    await commit();
  }

  const vAnim = $derived(volKeys(shown).length > 0);
  const pAnim = $derived(panKeys(shown).length > 0);
  const vol = $derived(volumeAt(shown, src));
  const pan = $derived(panAt(shown, src));
  const vHere = $derived(onClip && volKeys(shown).some((k) => k.at === src));
  const pHere = $derived(onClip && panKeys(shown).some((k) => k.at === src));
  const nav = (ats: number[]) => {
    const ts = ats.map(toTimeline);
    return { prev: [...ts].reverse().find((t) => t < ph) ?? null, next: ts.find((t) => t > ph) ?? null };
  };
  const vNav = $derived(nav(volKeys(shown).map((k) => k.at)));
  const pNav = $derived(nav(panKeys(shown).map((k) => k.at)));

  function setVol(db: number) {
    edit((n) => {
      if (volKeys(n).length) {
        if (onClip) setVolKey(n, src, db);
      } else n.volume_db = db;
    });
  }
  function setPan(p: number) {
    edit((n) => {
      if (panKeys(n).length) {
        if (onClip) setPanKey(n, src, p);
      } else n.pan = p;
    });
  }
  const toggleVolKey = () =>
    once((n) => {
      const keys = volKeys(n);
      const i = keys.findIndex((k) => k.at === src);
      if (i >= 0) n.volume_keys = keys.filter((_, j) => j !== i);
      else setVolKey(n, src, volumeAt(n, src));
    });
  const togglePanKey = () =>
    once((n) => {
      const keys = panKeys(n);
      const i = keys.findIndex((k) => k.at === src);
      if (i >= 0) n.pan_keys = keys.filter((_, j) => j !== i);
      else setPanKey(n, src, panAt(n, src));
    });
  const T = $derived(tps());
  const fadeMax = (other: number) => Math.max(0, (ins.duration - other) / T);
  const snapT = (secs: number) => {
    const q = app.s?.seq;
    const f = q ? q.rate_den / q.rate_num : 1 / 25;
    return Math.round(Math.round(secs / f) * f * T);
  };
  const keyTip = (here: boolean, what: string) => (!onClip ? "Move the playhead onto the clip to set keyframes" : here ? `Remove the ${what} keyframe at the playhead` : `Add a ${what} keyframe at the playhead`);
</script>

<div class="group">
  <div class="phead"><h4>Sound</h4><button class="ghost small" onclick={() => (app.savePreset = { clip: ins.id })} title="Save the volume, pan and fades (with their keyframes) as a preset for other clips">Save as preset…</button></div>
  {#if Array.isArray(shown.ducking) && shown.ducking.length}
    <p class="note">Ducked under dialogue: the dip in the volume keyframes was added by Timeline › Duck music under dialogue… Duck again to replace it, or remove it there (your own keyframes stay).</p>
  {/if}
  {#if (vAnim || pAnim) && !onClip}
    <p class="note">The playhead is off this clip: keyframed values show the nearest key. Move onto the clip to change them.</p>
  {/if}
  <div class="grid">
    <span class="lbl">Volume</span>
    <div class="row">
      <input type="range" min={MIN_DB} max={MAX_DB} step="0.5" value={vol} disabled={vAnim && !onClip} aria-label="Volume" oninput={(e) => setVol(+e.currentTarget.value)} onchange={commit} ondblclick={() => (setVol(0), commit())} />
      <span class="tc val">{dbText(vol)}</span>
    </div>
    <span class="lbl">Keyframes</span>
    <div class="row keys">
      <button class="ghost nav" disabled={vNav.prev == null} onclick={() => run(api.seek(vNav.prev!))} title="Previous volume keyframe" aria-label="Previous volume keyframe">◀</button>
      <button class="key" class:here={vHere} class:anim={vAnim} disabled={!onClip} onclick={toggleVolKey} title={keyTip(vHere, "volume")} aria-label="Volume keyframe"><svg viewBox="0 0 10 10" width="10" height="10"><path d="M5 1 9 5 5 9 1 5Z" /></svg></button>
      <button class="ghost nav" disabled={vNav.next == null} onclick={() => run(api.seek(vNav.next!))} title="Next volume keyframe" aria-label="Next volume keyframe">▶</button>
      {#if vAnim}
        <span class="muted count">{volKeys(shown).length}</span>
        <button class="small" onclick={() => once((n) => ((n.volume_db = volumeAt(n, src)), (n.volume_keys = [])))} title="Remove all volume keyframes, keeping the level at the playhead">Clear</button>
      {/if}
    </div>
    <span class="lbl">Pan</span>
    <div class="row">
      <input type="range" min="-1" max="1" step="0.02" value={pan} disabled={pAnim && !onClip} aria-label="Pan" oninput={(e) => setPan(+e.currentTarget.value)} onchange={commit} ondblclick={() => (setPan(0), commit())} />
      <span class="tc val">{panText(pan)}</span>
    </div>
    <span class="lbl">Pan keys</span>
    <div class="row keys">
      <button class="ghost nav" disabled={pNav.prev == null} onclick={() => run(api.seek(pNav.prev!))} title="Previous pan keyframe" aria-label="Previous pan keyframe">◀</button>
      <button class="key" class:here={pHere} class:anim={pAnim} disabled={!onClip} onclick={togglePanKey} title={keyTip(pHere, "pan")} aria-label="Pan keyframe"><svg viewBox="0 0 10 10" width="10" height="10"><path d="M5 1 9 5 5 9 1 5Z" /></svg></button>
      <button class="ghost nav" disabled={pNav.next == null} onclick={() => run(api.seek(pNav.next!))} title="Next pan keyframe" aria-label="Next pan keyframe">▶</button>
      {#if pAnim}
        <span class="muted count">{panKeys(shown).length}</span>
        <button class="small" onclick={() => once((n) => ((n.pan = panAt(n, src)), (n.pan_keys = [])))} title="Remove all pan keyframes, keeping the pan at the playhead">Clear</button>
      {/if}
    </div>
    <span class="lbl">Fade in</span>
    <NumberField label="Fade in" value={shown.fade_in / T} min={0} max={fadeMax(shown.fade_out)} step={0.01} digits={2} suffix=" s" oninput={(v) => edit((n) => (n.fade_in = snapT(v)))} oncommit={commit} />
    <span class="lbl">Fade out</span>
    <NumberField label="Fade out" value={shown.fade_out / T} min={0} max={fadeMax(shown.fade_in)} step={0.01} digits={2} suffix=" s" oninput={(v) => edit((n) => (n.fade_out = snapT(v)))} oncommit={commit} />
  </div>
  {#if vAnim && onClip}
    <p class="note">At {timecode(ph)}. Ctrl+click the volume line on the timeline to add a key; Alt+click a key to remove it.</p>
  {/if}
  {#if ins.kind !== "comp"}
    <AudioEffects audio={shown} {src} {onClip} {edit} {once} {commit} />
  {/if}
  <button class="reset" onclick={() => once((n) => Object.assign(n, defaultAudio(), { crossfade_in: n.crossfade_in }))}>Reset audio</button>
</div>

<style>
  .group {
    padding: 8px 0 12px;
    border-bottom: 1px solid var(--line-soft);
  }
  .phead {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 6px;
  }
  .phead .small {
    min-height: 20px;
    padding: 0 6px;
    font-size: calc(11.5px * var(--text-scale));
  }
  h4 {
    font-size: calc(12.5px * var(--text-scale));
    font-weight: 600;
    color: var(--muted);
    margin: 0 0 8px;
  }
  .grid {
    display: grid;
    grid-template-columns: 78px 1fr;
    gap: 6px;
    align-items: center;
  }
  .lbl {
    color: var(--muted);
  }
  .row {
    display: flex;
    gap: 6px;
    align-items: center;
    min-width: 0;
  }
  .row input[type="range"] {
    flex: 1;
    min-width: 0;
  }
  .keys {
    gap: 2px;
  }
  .nav {
    min-height: 20px;
    padding: 0 5px;
    font-size: calc(10px * var(--text-scale));
  }
  .count {
    margin: 0 6px 0 4px;
  }
  .val {
    width: 64px;
    text-align: right;
    color: var(--text);
  }
  .key {
    width: 18px;
    height: 20px;
    min-height: 0;
    padding: 0;
    display: grid;
    place-items: center;
    background: none;
    border: 0;
    flex: none;
  }
  .key path {
    fill: none;
    stroke: var(--faint);
    stroke-width: 1.2;
  }
  .key:hover:not(:disabled) path {
    stroke: var(--text);
  }
  .key.anim path {
    stroke: var(--accent);
  }
  .key.here path {
    fill: var(--accent);
    stroke: var(--accent);
  }
  .key:disabled {
    opacity: 0.5;
  }
  .small {
    min-height: 22px;
    padding: 0 7px;
    font-size: calc(12px * var(--text-scale));
  }
  .note {
    color: var(--faint);
    font-size: calc(12px * var(--text-scale));
    margin: 6px 0 4px;
  }
  .muted {
    color: var(--muted);
  }
  .reset {
    margin-top: 10px;
  }
</style>
