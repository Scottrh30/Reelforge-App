<script lang="ts">
  import { untrack } from "svelte";
  import { app, run, timecode } from "../lib/session.svelte";
  import { api, type Transform, type Blend, type TitleSpec, type ClipAudio } from "../lib/api";
  import NumberField from "./NumberField.svelte";
  import MasksPanel from "./MasksPanel.svelte";
  import TrackingPanel from "./TrackingPanel.svelte";
  import EffectsPanel from "./EffectsPanel.svelte";

  const ins = $derived(app.s?.inspector ?? null);
  const blends: Blend[] = ["Normal", "Add", "Multiply", "Screen"];
  const speeds = [0.25, 0.5, 1, 2, 4];
  let linkScale = $state(true);

  // Edits made here go to the editor at once; the Controls show the editor's
  // values, so while dragging we keep a local copy to avoid flicker.
  let draft = $state<Transform | null>(null);
  // What's shown is the transform at the playhead (keyframes applied).
  const t = $derived(draft ?? ins?.keys?.shown ?? ins?.visual?.transform ?? null);
  const pk = (prop: string) => ins?.keys?.props.find((p) => p.prop === prop);
  const keyTip = (prop: string) => {
    const k = pk(prop);
    if (!ins?.keys?.on_clip) return "Move the playhead onto the clip to set keyframes";
    return k?.key_here ? "Remove the keyframe here" : k?.animated ? "Add a keyframe at the playhead" : "Animate: add a keyframe at the playhead";
  };
  // A different clip selected: drop the local copy.
  let shownId: number | null = null;
  $effect(() => {
    const id = ins?.id ?? null;
    untrack(() => {
      if (id !== shownId) {
        shownId = id;
        draft = null;
      }
    });
  });

  function setT(f: (t: Transform) => void) {
    if (!ins || !t) return;
    const n = structuredClone($state.snapshot(t)) as Transform;
    f(n);
    draft = n;
    api.setTransform(ins.id, n);
  }
  async function commit() {
    await run(api.commit());
    draft = null;
  }

  function setTitle(f: (s: TitleSpec) => void, done = false) {
    if (!ins?.title) return;
    const n = structuredClone($state.snapshot(ins.title)) as TitleSpec;
    f(n);
    api.setTitle(ins.id, n).then(() => (done ? commit() : run(Promise.resolve())));
  }

  function setAudio(f: (a: ClipAudio) => void) {
    if (!ins?.audio) return;
    const n = structuredClone($state.snapshot(ins.audio)) as ClipAudio;
    f(n);
    api.setClipAudio(ins.id, n);
  }

  const hex = (c: number[]) => "#" + c.slice(0, 3).map((v) => v.toString(16).padStart(2, "0")).join("");
  const fromHex = (h: string, a: number): [number, number, number, number] => [parseInt(h.slice(1, 3), 16), parseInt(h.slice(3, 5), 16), parseInt(h.slice(5, 7), 16), a];
</script>

<div class="panel">
  {#if !ins}
    <div class="group">
      <h3>{app.s?.seq.name ?? "Sequence"}</h3>
      {#if app.s}
        <dl>
          <dt>Size</dt><dd>{app.s.seq.width}×{app.s.seq.height}</dd>
          <dt>Frame rate</dt><dd>{(app.s.seq.rate_num / app.s.seq.rate_den).toFixed(3).replace(".000", "")} fps</dd>
          <dt>Length</dt><dd class="tc">{timecode(app.s.seq.duration)}</dd>
          <dt>Tracks</dt><dd>{app.s.seq.tracks.length}</dd>
        </dl>
      {/if}
      <p class="muted">Select a clip on the timeline to change how it looks and sounds.</p>
    </div>
  {:else}
    <div class="group">
      <h3 title={ins.name}>{ins.name}</h3>
      <dl>
        <dt>Track</dt><dd>{ins.track}</dd>
        <dt>Starts at</dt><dd class="tc">{timecode(ins.start)}</dd>
        <dt>Length</dt><dd class="tc">{timecode(ins.duration)}</dd>
        {#if ins.kind === "video" || ins.kind === "audio"}<dt>Media from</dt><dd class="tc">{timecode(ins.source_in)}</dd>{/if}
      </dl>
    </div>

    {#if ins.kind === "video" || ins.kind === "audio" || ins.kind === "comp"}
      <div class="group">
        <h4>Speed</h4>
        <div class="row wrap">
          {#each speeds as s}
            <button class:on={!ins.reverse && Math.abs(ins.speed - s) < 1e-3} onclick={() => run(api.setSpeed(ins.id, s, false))}>{s * 100}%</button>
          {/each}
          <label class="check"><input type="checkbox" checked={ins.reverse} onchange={(e) => run(api.setSpeed(ins.id, ins.speed || 1, e.currentTarget.checked))} /> Reverse</label>
        </div>
      </div>
    {/if}

    {#if ins.title}
      <div class="group">
        <h4>Title</h4>
        <textarea
          rows="3"
          value={ins.title.text}
          aria-label="Title text"
          oninput={(e) => setTitle((s) => (s.text = e.currentTarget.value))}
          onblur={commit}
        ></textarea>
        <div class="grid2">
          <span class="lbl">Size</span>
          <NumberField label="Text size" value={ins.title.size} min={4} max={2000} step={1} digits={0} suffix=" px" oninput={(v) => setTitle((s) => (s.size = v))} oncommit={commit} />
          <span class="lbl">Colour</span>
          <input type="color" value={hex(ins.title.color)} aria-label="Text colour" onchange={(e) => setTitle((s) => (s.color = fromHex(e.currentTarget.value, s.color[3])), true)} />
        </div>
      </div>
    {/if}

    {#if t && ins.visual}
      <div class="group">
        <h4>Transform</h4>
        {#if ins.keys && !ins.keys.on_clip && ins.animated.length}
          <p class="note">The playhead is off this clip: keyframed values show the nearest key. Move onto the clip to change them.</p>
        {/if}
        <div class="grid3">
          <span class="lbl"><button class="key" class:here={pk("Position")?.key_here} class:anim={pk("Position")?.animated} disabled={!ins.keys?.on_clip} onclick={() => run(api.transformKey(ins.id, "Position"))} title={keyTip("Position")} aria-label="Keyframe position"><svg viewBox="0 0 10 10" width="10" height="10"><path d="M5 1 9 5 5 9 1 5Z" /></svg></button>Position</span>
          <NumberField label="Position X" value={t.position[0]} digits={0} oninput={(v) => setT((n) => (n.position[0] = v))} oncommit={commit} />
          <NumberField label="Position Y" value={t.position[1]} digits={0} oninput={(v) => setT((n) => (n.position[1] = v))} oncommit={commit} />
          <span class="lbl">
            <button class="key" class:here={pk("Scale")?.key_here} class:anim={pk("Scale")?.animated} disabled={!ins.keys?.on_clip} onclick={() => run(api.transformKey(ins.id, "Scale"))} title={keyTip("Scale")} aria-label="Keyframe scale"><svg viewBox="0 0 10 10" width="10" height="10"><path d="M5 1 9 5 5 9 1 5Z" /></svg></button>Scale
            <button class="ghost link" class:on={linkScale} onclick={() => (linkScale = !linkScale)} title="Keep width and height together" aria-label="Link scale">⛓</button>
          </span>
          <NumberField label="Scale X" value={t.scale[0] * 100} step={0.5} digits={1} suffix="%" oninput={(v) => setT((n) => { const k = v / 100; if (linkScale && n.scale[0] !== 0) n.scale[1] *= k / n.scale[0]; n.scale[0] = k; })} oncommit={commit} />
          <NumberField label="Scale Y" value={t.scale[1] * 100} step={0.5} digits={1} suffix="%" oninput={(v) => setT((n) => { const k = v / 100; if (linkScale && n.scale[1] !== 0) n.scale[0] *= k / n.scale[1]; n.scale[1] = k; })} oncommit={commit} />
          <span class="lbl"><button class="key" class:here={pk("Rotation")?.key_here} class:anim={pk("Rotation")?.animated} disabled={!ins.keys?.on_clip} onclick={() => run(api.transformKey(ins.id, "Rotation"))} title={keyTip("Rotation")} aria-label="Keyframe rotation"><svg viewBox="0 0 10 10" width="10" height="10"><path d="M5 1 9 5 5 9 1 5Z" /></svg></button>Rotation</span>
          <NumberField label="Rotation" value={t.rotation} step={0.5} digits={1} suffix="°" oninput={(v) => setT((n) => (n.rotation = v))} oncommit={commit} />
          <span></span>
          <span class="lbl"><button class="key" class:here={pk("Anchor")?.key_here} class:anim={pk("Anchor")?.animated} disabled={!ins.keys?.on_clip} onclick={() => run(api.transformKey(ins.id, "Anchor"))} title={keyTip("Anchor")} aria-label="Keyframe anchor"><svg viewBox="0 0 10 10" width="10" height="10"><path d="M5 1 9 5 5 9 1 5Z" /></svg></button>Anchor</span>
          <NumberField label="Anchor X" value={t.anchor[0]} digits={0} oninput={(v) => setT((n) => (n.anchor[0] = v))} oncommit={commit} />
          <NumberField label="Anchor Y" value={t.anchor[1]} digits={0} oninput={(v) => setT((n) => (n.anchor[1] = v))} oncommit={commit} />
        </div>
        <div class="grid2">
          <span class="lbl"><button class="key" class:here={pk("Opacity")?.key_here} class:anim={pk("Opacity")?.animated} disabled={!ins.keys?.on_clip} onclick={() => run(api.transformKey(ins.id, "Opacity"))} title={keyTip("Opacity")} aria-label="Keyframe opacity"><svg viewBox="0 0 10 10" width="10" height="10"><path d="M5 1 9 5 5 9 1 5Z" /></svg></button>Opacity</span>
          <div class="row">
            <input
              type="range"
              min="0"
              max="100"
              step="1"
              value={Math.round(t.opacity * 100)}
              aria-label="Opacity"
              oninput={(e) => setT((n) => (n.opacity = +e.currentTarget.value / 100))}
              onchange={commit}
            />
            <span class="tc val">{Math.round(t.opacity * 100)}%</span>
          </div>
          <span class="lbl">Blend</span>
          <select value={ins.visual.blend} aria-label="Blend mode" onchange={(e) => run(api.setBlend(ins.id, e.currentTarget.value as Blend))}>
            {#each blends as b}<option value={b}>{b}</option>{/each}
          </select>
        </div>
        <button
          class="reset"
          onclick={async () => {
            setT((n) => {
              n.position = [0, 0];
              n.scale = [1, 1];
              n.rotation = 0;
              n.opacity = 1;
              n.anchor = [0, 0];
            });
            await commit();
          }}>Reset transform</button
        >
      </div>
    {/if}

    {#if ins.visual}
      <EffectsPanel {ins} />
      <MasksPanel {ins} />
    {/if}
    {#if ins.can_track}
      <TrackingPanel {ins} />
    {/if}

    {#if ins.audio}
      <div class="group">
        <h4>Sound</h4>
        <div class="grid2">
          <span class="lbl">Volume</span>
          <div class="row">
            <input type="range" min="-60" max="12" step="0.5" value={ins.audio.volume_db} aria-label="Volume" oninput={(e) => setAudio((a) => (a.volume_db = +e.currentTarget.value))} onchange={commit} />
            <span class="tc val">{ins.audio.volume_db > 0 ? "+" : ""}{ins.audio.volume_db.toFixed(1)} dB</span>
          </div>
          <span class="lbl">Pan</span>
          <div class="row">
            <input type="range" min="-1" max="1" step="0.02" value={ins.audio.pan} aria-label="Pan" oninput={(e) => setAudio((a) => (a.pan = +e.currentTarget.value))} onchange={commit} ondblclick={() => (setAudio((a) => (a.pan = 0)), commit())} />
            <span class="tc val">{ins.audio.pan === 0 ? "C" : ins.audio.pan < 0 ? `L${Math.round(-ins.audio.pan * 100)}` : `R${Math.round(ins.audio.pan * 100)}`}</span>
          </div>
        </div>
      </div>
    {/if}

  {/if}
</div>

<style>
  .panel {
    height: 100%;
    overflow: auto;
    padding: 4px 10px 12px;
  }
  .group {
    padding: 8px 0 12px;
    border-bottom: 1px solid var(--line-soft);
  }
  h3 {
    font-size: calc(15px * var(--text-scale));
    font-weight: 600;
    margin: 4px 0 8px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  h4 {
    font-size: calc(12.5px * var(--text-scale));
    font-weight: 600;
    color: var(--muted);
    margin: 0 0 8px;
  }
  dl {
    display: grid;
    grid-template-columns: 90px 1fr;
    gap: 3px 8px;
    margin: 0;
  }
  dt {
    color: var(--muted);
  }
  dd {
    margin: 0;
  }
  .grid3 {
    display: grid;
    grid-template-columns: 78px 1fr 1fr;
    gap: 5px 6px;
    align-items: center;
  }
  .grid2 {
    display: grid;
    grid-template-columns: 78px 1fr;
    gap: 6px;
    align-items: center;
    margin-top: 8px;
  }
  .lbl {
    color: var(--muted);
    display: flex;
    align-items: center;
    gap: 4px;
  }
  .row {
    display: flex;
    gap: 6px;
    align-items: center;
  }
  .row input[type="range"] {
    flex: 1;
    min-width: 0;
  }
  .wrap {
    flex-wrap: wrap;
  }
  .val {
    width: 64px;
    text-align: right;
    color: var(--text);
  }
  .link {
    padding: 0 3px;
    min-height: 18px;
    font-size: calc(11px * var(--text-scale));
  }
  .check {
    display: flex;
    gap: 5px;
    align-items: center;
    margin-left: 6px;
  }
  textarea {
    width: 100%;
  }
  input[type="color"] {
    width: 48px;
    height: 24px;
    padding: 0;
    border: 1px solid var(--line);
    background: none;
    border-radius: var(--radius);
  }
  .reset {
    margin-top: 10px;
  }
  .key {
    width: 16px;
    height: 18px;
    min-height: 0;
    padding: 0;
    margin-left: -2px;
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
  .note {
    color: var(--faint);
    font-size: calc(12px * var(--text-scale));
    margin: 4px 0 8px;
  }
  .muted {
    color: var(--muted);
  }
</style>
