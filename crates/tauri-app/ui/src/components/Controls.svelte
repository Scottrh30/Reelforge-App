<script lang="ts">
  import { untrack } from "svelte";
  import { app, run, timecode } from "../lib/session.svelte";
  import { pickColor } from "../lib/colorpick.svelte";
  import { api, type Transform, type Blend, type TrackMatte, type TitleSpec, type ClipAudio, type TrackAudio } from "../lib/api";
  import NumberField from "./NumberField.svelte";
  import MasksPanel from "./MasksPanel.svelte";
  import TemplateControls from "./TemplateControls.svelte";
  import { showPanel } from "../lib/dock.svelte";
  import { setPanel } from "../lib/session.svelte";
  import EffectsPanel from "./EffectsPanel.svelte";
  import AudioPanel from "./AudioPanel.svelte";
  import ModelPanel from "./ModelPanel.svelte";
  import TitlePanel from "./TitlePanel.svelte";
  import { tps } from "../lib/session.svelte";
  import { livePump } from "../lib/live";
  import type { Transition, TransitionKind, Interp } from "../lib/api";

  const ins = $derived(app.s?.inspector ?? null);
  /// Blend modes in menu groups: [heading, [mode, label, tooltip]].
  const blendGroups: [string, [Blend, string, string][]][] = [
    ["", [["Normal", "Normal", "Covers what's under it"]]],
    ["Darken", [
      ["Darken", "Darken", "Keeps the darker of the layer and what's under it, per channel"],
      ["Multiply", "Multiply", "Darkens: white disappears, black stays black"],
      ["ColorBurn", "Colour burn", "Darkens and adds contrast, deepening colours"],
    ]],
    ["Lighten", [
      ["Lighten", "Lighten", "Keeps the lighter of the layer and what's under it, per channel"],
      ["Screen", "Screen", "Lightens: black disappears, white stays white"],
      ["ColorDodge", "Colour dodge", "Brightens strongly, blowing out highlights"],
      ["Add", "Add", "Adds light (fire, flares, glows)"],
    ]],
    ["Contrast", [
      ["Overlay", "Overlay", "Contrast from the layer: darker under dark areas, lighter under light ones; mid grey disappears"],
      ["SoftLight", "Soft light", "A gentle overlay, like a soft spotlight"],
      ["HardLight", "Hard light", "A strong overlay, led by the layer"],
    ]],
    ["Compare", [
      ["Difference", "Difference", "The difference between the two: black disappears, white inverts"],
      ["Exclusion", "Exclusion", "Like difference, with less contrast"],
    ]],
    ["Colour", [
      ["Hue", "Hue", "The layer's hue, with the saturation and brightness of what's under it"],
      ["Saturation", "Saturation", "The layer's saturation only"],
      ["Color", "Colour", "The layer's hue and saturation, with the brightness of what's under it (tinting)"],
      ["Luminosity", "Luminosity", "The layer's brightness, with the colour of what's under it"],
    ]],
  ];
  const blendTip = (b: Blend) => blendGroups.flatMap(([, g]) => g).find(([m]) => m === b)?.[2] ?? "";
  const speeds = [0.25, 0.5, 1, 2, 4];
  let linkScale = $state(true);

  // Speed: a typed or dragged speed applies when it's let go.
  let speedDraft = $state<number | null>(null);
  async function applySpeed() {
    if (!ins || speedDraft == null) return;
    const v = Math.max(1, speedDraft);
    speedDraft = null;
    await run(api.setSpeed(ins.id, v / 100, ins.reverse));
  }
  // Time remap graph: media time (up) against clip time (across); keys drag up and down.
  const RG = { w: 220, h: 90 };
  const remapScale = $derived.by(() => {
    const r = ins?.remap;
    if (!r) return null;
    const top = Math.max(r.media_length, ...r.keys.map((k) => k[1]), 0.04);
    return { x: (s: number) => (s / Math.max(r.length, 1e-3)) * RG.w, y: (m: number) => RG.h - (m / top) * RG.h, top };
  });
  const remapPath = $derived.by(() => {
    const r = ins?.remap;
    const sc = remapScale;
    if (!r || !sc) return "";
    const ease = (u: number) => (r.interp === "Ease" ? u * u * (3 - 2 * u) : u);
    let d = "";
    for (let i = 0; i < r.keys.length; i++) {
      const [x, y] = r.keys[i];
      if (i === 0) {
        d += `M0,${sc.y(y)}H${sc.x(x)}`;
        continue;
      }
      const [px, py] = r.keys[i - 1];
      if (r.interp === "Hold") {
        d += `H${sc.x(x)}V${sc.y(y)}`;
      } else {
        for (let k = 1; k <= 12; k++) {
          const u = k / 12;
          d += `L${sc.x(px + (x - px) * u)},${sc.y(py + (y - py) * ease(u))}`;
        }
      }
    }
    if (r.keys.length) d += `H${RG.w}`;
    return d;
  });
  let remapDrag = $state<{ index: number; secs: number } | null>(null);
  let remapDraft = $state<number | null>(null);
  async function applyRemapValue() {
    if (!ins || remapDraft == null) return;
    const v = remapDraft;
    remapDraft = null;
    await run(api.timeRemapValue(ins.id, v));
  }
  function dragRemapKey(e: PointerEvent, index: number) {
    if (!ins?.remap || !remapScale || e.button !== 0) return;
    e.preventDefault();
    const el = e.currentTarget as SVGElement;
    const svg = el.ownerSVGElement!;
    el.setPointerCapture(e.pointerId);
    const id = ins.id;
    const top = remapScale.top;
    const move = (ev: PointerEvent) => {
      const r = svg.getBoundingClientRect();
      const y = ((ev.clientY - r.top) / r.height) * RG.h;
      remapDrag = { index, secs: Math.max(0, ((RG.h - y) / RG.h) * top) };
    };
    const up = async () => {
      el.removeEventListener("pointermove", move);
      el.removeEventListener("pointerup", up);
      const d = remapDrag;
      remapDrag = null;
      if (d) await run(api.timeRemapMoveKey(id, d.index, d.secs));
    };
    el.addEventListener("pointermove", move);
    el.addEventListener("pointerup", up);
  }
  const blendTips: Record<string, string> = {
    Nearest: "Each frame is held until the next: crisp, but slow motion steps",
    Blend: "The frames either side are cross-faded: smoother, a little ghosted",
    Flow: "Every pixel follows the motion between the frames either side (worked out on the graphics card, aware of things passing in front of others): the smoothest slow motion. Slower to work out; see Flow quality",
  };
  let rampOpen = $state(false);
  let ramp = $state({ from: 100, to: 25, steps: 8, smooth: true });
  const rampPath = $derived.by(() => {
    const n = ramp.steps;
    const hi = Math.max(ramp.from, ramp.to, 1);
    let d = "";
    for (let i = 0; i < n; i++) {
      const u0 = n > 1 ? i / (n - 1) : 0;
      const u = ramp.smooth ? u0 * u0 * (3 - 2 * u0) : u0;
      const sp = ramp.from + (ramp.to - ramp.from) * u;
      const y = 28 - (sp / hi) * 26;
      d += `${i ? "L" : "M"}${(i / n) * 100},${y}H${((i + 1) / n) * 100}`;
    }
    return d;
  });
  async function applyRamp() {
    if (!ins) return;
    await run(api.speedRamp(ins.id, ramp.from / 100, ramp.to / 100, ramp.steps, ramp.smooth));
    rampOpen = false;
  }

  // Transition in.
  let kinds = $state<TransitionKind[]>([]);
  $effect(() => {
    api.transitionList().then((k) => (kinds = k));
  });
  const xfPump = livePump();
  let xfDraft = $state<number | null>(null);
  async function commitXf() {
    await xfPump.done();
    await run(api.commit());
    xfDraft = null;
  }

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

  // Track volume and pan (shown when no clip is selected).
  let trackDraft = $state<Record<number, TrackAudio>>({});
  function setTrack(index: number, cur: TrackAudio, change: Partial<TrackAudio>) {
    const n = { ...$state.snapshot(cur), ...change };
    trackDraft[index] = n;
    api.setTrackAudio(index, n);
  }
  async function commitTrack(index: number) {
    await run(api.commit());
    delete trackDraft[index];
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
    {#if app.s && app.s.seq.tracks.some((t) => t.kind === "audio")}
      <div class="group">
        <h4>Track levels</h4>
        <div class="grid2 tracks">
          {#each app.s.seq.tracks.filter((t) => t.kind === "audio") as tr (tr.index)}
            {@const ta = trackDraft[tr.index] ?? tr.audio}
            <span class="lbl tname" title={tr.name}>{tr.name}</span>
            <div class="row">
              <input type="range" min="-60" max="12" step="0.5" value={ta.volume_db} aria-label="{tr.name} volume" oninput={(e) => setTrack(tr.index, ta, { volume_db: +e.currentTarget.value })} onchange={() => commitTrack(tr.index)} ondblclick={() => (setTrack(tr.index, ta, { volume_db: 0 }), commitTrack(tr.index))} />
              <span class="tc val">{ta.volume_db <= -60 ? "−∞" : `${ta.volume_db > 0 ? "+" : ""}${ta.volume_db.toFixed(1)}`} dB</span>
            </div>
            <span class="lbl sub">Pan</span>
            <div class="row">
              <input type="range" min="-1" max="1" step="0.02" value={ta.pan} aria-label="{tr.name} pan" oninput={(e) => setTrack(tr.index, ta, { pan: +e.currentTarget.value })} onchange={() => commitTrack(tr.index)} ondblclick={() => (setTrack(tr.index, ta, { pan: 0 }), commitTrack(tr.index))} />
              <span class="tc val">{Math.abs(ta.pan) < 0.005 ? "C" : ta.pan < 0 ? `L${Math.round(-ta.pan * 100)}` : `R${Math.round(ta.pan * 100)}`}</span>
            </div>
          {/each}
        </div>
      </div>
    {/if}
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

    {#if ins.kind === "plane" || ins.kind === "adjustment" || ins.parent != null || ins.parents.length}
      <div class="group">
        <h4>Layer</h4>
        <div class="grid2">
          {#if ins.plane}
            <span class="lbl">Colour</span>
            <div class="row">
              <button
                class="swatch"
                style="background:{ins.plane}"
                aria-label="Plane colour"
                title="The plane's colour (click to change it; OK to take the new one)"
                data-automation-id="plane-colour"
                onclick={(e) => {
                  const id = ins.id;
                  pickColor(ins.plane ?? "#808080", (hex) => run(api.setPlaneColor(id, hex)), e.currentTarget, "Plane colour");
                }}
              ></button>
              <span class="tc muted">{ins.plane}</span>
            </div>
          {/if}
          {#if ins.parent != null || ins.parents.length}
            <span class="lbl">Parent</span>
            <select
              value={ins.parent ?? ""}
              aria-label="Parent"
              title="Follow a point layer: this layer moves, turns and scales with it"
              onchange={(e) => {
                const v = e.currentTarget.value;
                run(api.setParent(ins.id, v === "" ? null : +v));
              }}
            >
              <option value="">None</option>
              {#each ins.parents as [id, name]}<option value={id}>{name}</option>{/each}
              {#if ins.parent != null && !ins.parents.some(([id]) => id === ins.parent)}<option value={ins.parent}>(missing point)</option>{/if}
            </select>
          {/if}
        </div>
        {#if ins.kind === "adjustment"}
          <p class="note">Effects added to this layer change every layer below it. Its opacity, crop, masks and blend mode apply; it doesn't move.</p>
        {:else if ins.kind === "point"}
          <p class="note">Parent other layers to this point (in their Layer settings), then move, turn or scale the point to move them together.</p>
        {/if}
      </div>
    {/if}

    {#if ins.kind === "video" || ins.kind === "audio" || ins.kind === "comp"}
      <div class="group">
        <h4>Speed</h4>
        {#if ins.footage && ins.kind !== "audio"}
          <label class="check" title="Keyframe which moment of the footage shows when: slow it, speed it up, hold it or play it backwards anywhere along the clip (the clip's sound is silent while it's on)"><input type="checkbox" checked={!!ins.remap} onchange={(e) => run(api.timeRemap(ins.id, e.currentTarget.checked))} /> Time remapping</label>
        {/if}
        {#if ins.remap}
          {@const r = ins.remap}
          <div class="grid2">
            <span class="lbl">Shows</span>
            <div class="row">
              <NumberField label="Footage time at the playhead" value={remapDraft ?? r.here} min={0} max={Math.max(r.media_length, r.here)} step={0.04} digits={2} suffix=" s" oninput={(v) => (remapDraft = v)} oncommit={applyRemapValue} />
              <button class="small" class:on={r.key_here} onclick={() => run(api.timeRemapKey(ins.id))} title={r.key_here ? "Remove the key at the playhead" : "Add a key at the playhead (keeps what shows there now)"}>◆</button>
            </div>
            <span class="lbl">Speed here</span>
            <span class="muted">{r.speed_here.toFixed(0)}%{Math.abs(r.speed_here) < 0.5 ? " (held)" : r.speed_here < 0 ? " (backwards)" : ""}</span>
            <span class="lbl">Ramps</span>
            <select aria-label="Speed changes between keys" title="How the speed changes between keys: steadily, easing in and out, or holding each key's frame until the next" value={r.interp === "Bezier" ? "Ease" : r.interp} onchange={(e) => run(api.timeRemapInterp(ins.id, e.currentTarget.value as "Linear" | "Ease" | "Hold"))}>
              <option value="Linear">Steady speed</option>
              <option value="Ease">Ease in and out</option>
              <option value="Hold">Hold frames</option>
            </select>
          </div>
          {#if remapScale}
            {@const sc = remapScale}
            <svg class="remapgraph" viewBox="-6 -6 {RG.w + 12} {RG.h + 12}" preserveAspectRatio="none" aria-label="Time remap: footage time (up) along the clip (across); drag a key up or down">
              <path d={remapPath} />
              <line x1={sc.x(r.at)} y1="0" x2={sc.x(r.at)} y2={RG.h} class="now" />
              {#each r.keys as k, i}
                {@const y = remapDrag?.index === i ? remapDrag.secs : k[1]}
                <circle cx={sc.x(k[0])} cy={sc.y(y)} r="4" class="key" role="presentation" onpointerdown={(e) => dragRemapKey(e, i)}><title>{k[0].toFixed(2)} s into the clip shows {y.toFixed(2)} s of the footage: drag up or down</title></circle>
              {/each}
            </svg>
            <p class="note">Up the graph is later in the footage. Flat is a held frame, steeper is faster, falling is backwards.</p>
          {/if}
        {:else}
        <div class="row wrap">
          {#each speeds as sp}
            <button class:on={!ins.reverse && Math.abs(ins.speed - sp) < 1e-3} onclick={() => run(api.setSpeed(ins.id, sp, false))}>{sp * 100}%</button>
          {/each}
          <label class="check"><input type="checkbox" checked={ins.reverse} onchange={(e) => run(api.setSpeed(ins.id, ins.speed || 1, e.currentTarget.checked))} /> Reverse</label>
        </div>
        <div class="grid2">
          <span class="lbl">Exactly</span>
          <div class="row">
            <NumberField label="Speed" value={speedDraft ?? (ins.speed === 0 ? 0 : ins.speed * 100)} min={1} max={2000} step={1} digits={0} suffix=" %" oninput={(v) => (speedDraft = v)} oncommit={applySpeed} />
            {#if ins.speed === 0}<span class="muted">Freeze frame</span><button class="small" onclick={() => run(api.setSpeed(ins.id, 1, false))} title="Turn it back into moving footage at 100%">Play from here</button>{/if}
          </div>
        </div>
        <div class="row wrap tools">
          <button class="small" onclick={() => run(api.action("freeze_frame"))} title="Hold the frame at the playhead for 2 s; the rest of the clip moves later (Shift+F)">Freeze frame at playhead</button>
          <button class="small" class:on={rampOpen} disabled={!ins.can_ramp} onclick={() => (rampOpen = !rampOpen)} title={ins.can_ramp ? "Speed up or slow down gradually across the clip" : "Ramps work on clips playing forwards"}>Speed ramp…</button>
        </div>
        {#if rampOpen && ins.can_ramp}
          <div class="ramp">
            <div class="grid2">
              <span class="lbl">From</span>
              <NumberField label="Ramp from" value={ramp.from} min={1} max={2000} step={1} digits={0} suffix=" %" oninput={(v) => (ramp.from = v)} oncommit={() => {}} />
              <span class="lbl">To</span>
              <NumberField label="Ramp to" value={ramp.to} min={1} max={2000} step={1} digits={0} suffix=" %" oninput={(v) => (ramp.to = v)} oncommit={() => {}} />
              <span class="lbl">Steps</span>
              <select bind:value={ramp.steps} aria-label="Steps">
                {#each [4, 6, 8, 12, 16, 24] as n}<option value={n}>{n} parts</option>{/each}
              </select>
            </div>
            <label class="check"><input type="checkbox" bind:checked={ramp.smooth} /> Ease in and out</label>
            <svg class="rampgraph" viewBox="0 0 100 30" preserveAspectRatio="none" aria-hidden="true"><path d={rampPath} /></svg>
            <p class="note">The clip is cut into {ramp.steps} parts that play at speeds going from {Math.round(ramp.from)}% to {Math.round(ramp.to)}%. Later clips move to make room.</p>
            <div class="row">
              <button class="small primary" onclick={applyRamp}>Make the ramp</button>
              <button class="small" onclick={() => (rampOpen = false)}>Cancel</button>
            </div>
          </div>
        {/if}
        {/if}
        {#if ins.footage && ins.kind === "video" && (ins.remap || ins.speed !== 1 || ins.reverse || ins.fewer_frames)}
          <div class="grid2">
            <span class="lbl" title={ins.fewer_frames && !(ins.remap || ins.speed !== 1 || ins.reverse) ? "This footage has fewer frames a second than the sequence: choose how the frames in between are made" : undefined}>Between frames</span>
            <select aria-label="Between frames" value={ins.frame_blend} title={blendTips[ins.frame_blend]} onchange={(e) => run(api.setFrameBlend(ins.id, e.currentTarget.value as "Nearest" | "Blend" | "Flow"))}>
              <option value="Nearest">Nearest frame</option>
              <option value="Blend">Frame blending</option>
              <option value="Flow">Optical flow</option>
            </select>
            {#if ins.frame_blend === "Flow"}
              <span class="lbl">Flow quality</span>
              <select aria-label="Optical flow quality" value={ins.flow_quality} title="How carefully the motion is worked out (on the graphics card): Fast looks at a quarter-HD copy of the frames, Balanced at half HD, Best at full size with more refining. Better is slower" onchange={(e) => run(api.setFlowQuality(ins.id, e.currentTarget.value as "Fast" | "Balanced" | "Best"))}>
                <option value="Fast">Fast</option>
                <option value="Balanced">Balanced</option>
                <option value="Best">Best</option>
              </select>
            {/if}
          </div>
        {/if}
      </div>
    {/if}

    {#if ins.multicam}
      {@const mc = ins.multicam}
      <div class="group">
        <h4>Multicam</h4>
        <div class="grid2">
          <span class="lbl">Angle</span>
          <span>{mc.angles[mc.video] ?? "?"}</span>
          <span class="lbl">Sound</span>
          <select aria-label="Sound" value={mc.audio == null ? "follow" : String(mc.audio)} onchange={(e) => run(api.setAudioAngle(ins.id, e.currentTarget.value === "follow" ? null : Number(e.currentTarget.value)))}>
            <option value="follow">Follows the picture</option>
            {#each mc.angles as a, i}<option value={String(i)}>{a}</option>{/each}
          </select>
        </div>
        <p class="note">Play and press 1–9 to cut to an angle at the playhead (Shift: the picture only, the sound stays), or click one in the Viewer's angle grid.</p>
      </div>
    {/if}

    {#if ins.transition_in}
      {@const tx = ins.transition_in}
      <div class="group">
        <h4>{tx.kind ? "Transition in" : "Crossfade in"}</h4>
        <div class="grid2">
          {#if tx.kind}
            <span class="lbl">Kind</span>
            <select value={tx.kind} aria-label="Transition" onchange={(e) => run(api.transitionKind(ins.id, e.currentTarget.value as Transition))}>
              {#each kinds as k}<option value={k.kind} title={k.description}>{k.label}</option>{/each}
            </select>
          {/if}
          <span class="lbl">Length</span>
          <div class="row">
            <NumberField label="Transition length" value={xfDraft ?? tx.length / tps()} min={0} max={tx.limit / tps()} step={0.02} digits={2} suffix=" s" oninput={(v) => ((xfDraft = v), xfPump.push(() => api.transitionLength(ins.id, v * tps())))} oncommit={commitXf} />
            {#if tx.length > 0}<button class="small" onclick={() => run(api.transitionRemove(ins.id))}>Remove</button>{/if}
          </div>
        </div>
        {#if tx.length === 0}
          <p class="note">A plain cut. Set a length{tx.kind ? " and a kind" : ""} to {tx.kind ? "blend" : "crossfade"} from the clip before.</p>
        {:else if tx.actual === 0}
          <p class="warn">No spare media at the cut, so it plays as a plain cut. Trim the clips to leave unused media either side.</p>
        {:else if tx.actual < tx.length}
          <p class="warn">Only {(tx.actual / tps()).toFixed(2)} s of spare media at the cut.</p>
        {/if}
      </div>
    {/if}

    {#if ins.title?.model}
      <ModelPanel {ins} />
    {:else if ins.title}
      <TitlePanel {ins} kerningTool={false} />
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
          <span class="lbl"><button class="key" class:here={pk("Crop")?.key_here} class:anim={pk("Crop")?.animated} disabled={!ins.keys?.on_clip} onclick={() => run(api.transformKey(ins.id, "Crop"))} title={keyTip("Crop")} aria-label="Keyframe crop"><svg viewBox="0 0 10 10" width="10" height="10"><path d="M5 1 9 5 5 9 1 5Z" /></svg></button>Crop</span>
          <NumberField label="Crop left" value={t.crop[0] * 100} min={0} max={100} step={0.2} digits={1} suffix="% L" oninput={(v) => setT((n) => (n.crop[0] = v / 100))} oncommit={commit} />
          <NumberField label="Crop top" value={t.crop[1] * 100} min={0} max={100} step={0.2} digits={1} suffix="% T" oninput={(v) => setT((n) => (n.crop[1] = v / 100))} oncommit={commit} />
          <span class="lbl"><button class="small clear" disabled={!t.crop.some((c) => c > 0)} onclick={() => run(api.transformOp(ins.id, "crop_clear"))} title="Show the whole layer">Clear</button></span>
          <NumberField label="Crop right" value={t.crop[2] * 100} min={0} max={100} step={0.2} digits={1} suffix="% R" oninput={(v) => setT((n) => (n.crop[2] = v / 100))} oncommit={commit} />
          <NumberField label="Crop bottom" value={t.crop[3] * 100} min={0} max={100} step={0.2} digits={1} suffix="% B" oninput={(v) => setT((n) => (n.crop[3] = v / 100))} oncommit={commit} />
        </div>
        <div class="grid2">
          <span class="lbl">3D layer</span>
          <div class="row wrap">
            <label class="check" title="Make the layer a flat plane in 3D space: it gets a Z position and X/Y rotation, is seen through the camera (Timeline → Add camera, or a default one) and can be lit (Timeline → Add light). At Z 0 with the default camera it looks the same as in 2D"><input type="checkbox" checked={!!t.three_d} onchange={(e) => { const on = e.currentTarget.checked; setT((n) => (n.three_d = on)); commit(); }} /> {t.three_d ? "On" : "Off"}</label>
            {#if t.three_d && t.pin}<span class="warn">Corner pin is on, so it's drawn in 2D</span>{/if}
          </div>
        </div>
        {#if t.three_d}
          <div class="grid3">
            <span class="lbl"><button class="key" class:here={pk("Depth")?.key_here} class:anim={pk("Depth")?.animated} disabled={!ins.keys?.on_clip} onclick={() => run(api.transformKey(ins.id, "Depth"))} title={keyTip("Depth")} aria-label="Keyframe Z position"><svg viewBox="0 0 10 10" width="10" height="10"><path d="M5 1 9 5 5 9 1 5Z" /></svg></button>Position Z</span>
            <NumberField label="Position Z" value={t.depth ?? 0} digits={0} oninput={(v) => setT((n) => (n.depth = v))} oncommit={commit} />
            <span class="note small-note">+ is further away</span>
            <span class="lbl"><button class="key" class:here={pk("Orientation")?.key_here} class:anim={pk("Orientation")?.animated} disabled={!ins.keys?.on_clip} onclick={() => run(api.transformKey(ins.id, "Orientation"))} title={keyTip("Orientation")} aria-label="Keyframe X and Y rotation"><svg viewBox="0 0 10 10" width="10" height="10"><path d="M5 1 9 5 5 9 1 5Z" /></svg></button>Rotation X / Y</span>
            <NumberField label="Rotation X" value={t.orientation?.[0] ?? 0} step={0.5} digits={1} suffix="°" oninput={(v) => setT((n) => (n.orientation = [v, n.orientation?.[1] ?? 0]))} oncommit={commit} />
            <NumberField label="Rotation Y" value={t.orientation?.[1] ?? 0} step={0.5} digits={1} suffix="°" oninput={(v) => setT((n) => (n.orientation = [n.orientation?.[0] ?? 0, v]))} oncommit={commit} />
          </div>
          <div class="grid2">
            <span class="lbl">Lights</span>
            <label class="check" title="Shaded by the scene's lights (when it has any). Untick for a layer that should glow by itself, like a screen or a sign"><input type="checkbox" checked={t.lit !== false} onchange={(e) => { const on = e.currentTarget.checked; setT((n) => (n.lit = on)); commit(); }} /> Lit by the scene's lights</label>
          </div>
          <p class="note">Rotation above turns it about Z. Drag it in the Viewer to move it across the camera's view.</p>
        {/if}
        <div class="grid2">
          <span class="lbl">{#if t.pin}<button class="key" class:here={pk("Pin")?.key_here} class:anim={pk("Pin")?.animated} disabled={!ins.keys?.on_clip} onclick={() => run(api.transformKey(ins.id, "Pin"))} title={keyTip("Pin")} aria-label="Keyframe corner pin"><svg viewBox="0 0 10 10" width="10" height="10"><path d="M5 1 9 5 5 9 1 5Z" /></svg></button>{/if}Corner pin</span>
          <div class="row wrap">
            <label class="check" title="Stretch the layer in perspective so its four corners land wherever you put them (drag them in the Viewer). Position, scale, rotation and anchor don't apply while it's on"><input type="checkbox" checked={!!t.pin} onchange={(e) => run(api.transformOp(ins.id, e.currentTarget.checked ? "pin_on" : "pin_off"))} /> {t.pin ? "On" : "Off"}</label>
            {#if t.pin}
              <button class="small" onclick={() => run(api.transformOp(ins.id, "pin_reset"))} title="Back to the layer's own rectangle (from its position, scale and rotation)">Reset corners</button>
            {/if}
          </div>
          {#if ins.keys?.pin_twisted}<span></span><span class="warn">Twisted: drag the corners apart</span>{/if}
        </div>
        {#if ins.mesh_keys != null}
          <div class="grid2">
            <span class="lbl">Mesh warp</span>
            <div class="row wrap">
              <span class="note" title="Stretched over a mesh tracker's grid, frame by frame. Position, scale, rotation and corner pin don't apply while it's on">On ({ins.mesh_keys} {ins.mesh_keys === 1 ? "key" : "keys"})</span>
              <button class="small" onclick={() => run(api.meshRemove(ins.id))} title="Take the mesh warp off: the layer goes back to its own transform">Remove</button>
            </div>
          </div>
        {/if}
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
          <select value={ins.visual.blend} aria-label="Blend mode" title={blendTip(ins.visual.blend)} onchange={(e) => run(api.setBlend(ins.id, e.currentTarget.value as Blend))}>
            {#each blendGroups as [heading, modes]}
              {#if heading}
                <optgroup label={heading}>
                  {#each modes as [m, label, tip]}<option value={m} title={tip}>{label}</option>{/each}
                </optgroup>
              {:else}
                {#each modes as [m, label, tip]}<option value={m} title={tip}>{label}</option>{/each}
              {/if}
            {/each}
          </select>
          <span class="lbl">Track matte</span>
          <select
            value={ins.visual.track_matte ?? ""}
            aria-label="Track matte"
            title="Show this layer only through the layer on the video track directly above it (that layer becomes the matte and isn't drawn itself)"
            onchange={(e) => run(api.setTrackMatte(ins.id, (e.currentTarget.value || null) as TrackMatte | null))}
          >
            <option value="">None</option>
            <option value="Alpha">Alpha (where the layer above is)</option>
            <option value="AlphaInverted">Alpha inverted (where it isn't)</option>
            <option value="Luma">Luma (where it's bright)</option>
            <option value="LumaInverted">Luma inverted (where it's dark)</option>
          </select>
        </div>
        {#if ins.keys && (ins.keys.prev_key != null || ins.keys.next_key != null)}
          <div class="grid2">
            <span class="lbl">Keyframes</span>
            <div class="row">
              <button class="small" disabled={ins.keys.prev_key == null} onclick={() => run(api.seek(ins.keys!.prev_key!))} title="Previous transform keyframe" aria-label="Previous keyframe">◀</button>
              <button class="small" disabled={ins.keys.next_key == null} onclick={() => run(api.seek(ins.keys!.next_key!))} title="Next transform keyframe" aria-label="Next keyframe">▶</button>
              {#if ins.keys.interp_here}
                <select value={ins.keys.interp_here} aria-label="Interpolation" title="How the keyframe(s) here move towards the next one" onchange={(e) => run(api.transformInterp(ins.id, e.currentTarget.value as Interp))}>
                  <option value="Linear">Linear</option>
                  <option value="Ease">Ease in and out</option>
                  <option value="Hold">Hold</option>
                  <option value="Bezier">Bézier (shape it in Curves)</option>
                </select>
              {/if}
            </div>
          </div>
        {/if}
        <div class="row tools wrap">
          <button class="small" onclick={() => run(api.transformOp(ins.id, "reset"))} title="Default transform; removes transform keyframes">Reset</button>
          <button class="small" onclick={() => run(api.transformOp(ins.id, "fit"))} title="Scale to fit inside the frame">Fit</button>
          <button class="small" onclick={() => run(api.transformOp(ins.id, "fill"))} title="Scale to cover the whole frame">Fill</button>
          <button class="small" disabled={!t.anchor[0] && !t.anchor[1]} onclick={() => run(api.transformOp(ins.id, "anchor_centre"))} title="Put the anchor back in the middle of the layer, keeping the picture where it is">Centre anchor</button>
        </div>
      </div>
    {/if}

    {#if ins.kind === "camera" || ins.kind === "light" || ins.kind === "point"}
      <EffectsPanel {ins} scene={true} />
    {/if}
    {#if ins.visual}
      <TemplateControls {ins} />
      <EffectsPanel {ins} />
      <MasksPanel {ins} />
    {/if}
    {#if ins.can_track}
      <div class="trackrow">
        <span>Motion tracking{ins.tracker ? `: ${ins.tracker.mode === "Position" ? "single point" : ins.tracker.mode === "Transform" ? "two point" : ins.tracker.mode === "CornerPin" ? "four point" : ins.tracker.mode.toLowerCase()}` : ""}</span>
        <button class="small" onclick={() => { setPanel("track", true); showPanel("track"); }}>Open Track panel</button>
      </div>
    {/if}

    {#if ins.audio}
      <AudioPanel {ins} />
    {/if}

  {/if}
</div>

<style>
  .trackrow {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    padding: 8px 0 12px;
    border-bottom: 1px solid var(--line-soft);
    color: var(--muted);
  }
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
  .tname {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--text);
  }
  .sub {
    padding-left: 10px;
  }
  .tracks {
    margin-top: 0;
  }
  .clear {
    margin-left: 18px;
    min-height: 20px !important;
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
  .small {
    min-height: 22px;
    padding: 0 7px;
    font-size: calc(12px * var(--text-scale));
  }
  .tools {
    margin-top: 8px;
  }
  .ramp {
    margin-top: 8px;
    padding: 8px;
    border: 1px solid var(--line-soft);
    border-radius: 6px;
    background: var(--base);
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .ramp .grid2 {
    margin-top: 0;
  }
  .remapgraph {
    width: 100%;
    height: 90px;
    background: var(--well);
    border-radius: 4px;
    touch-action: none;
  }
  .remapgraph path {
    fill: none;
    stroke: var(--accent);
    stroke-width: 1.5;
    vector-effect: non-scaling-stroke;
  }
  .remapgraph .now {
    stroke: var(--faint);
    stroke-dasharray: 3 3;
    vector-effect: non-scaling-stroke;
  }
  .remapgraph .key {
    fill: var(--accent);
    stroke: var(--base);
    stroke-width: 1;
    cursor: ns-resize;
    vector-effect: non-scaling-stroke;
  }
  .rampgraph {
    width: 100%;
    height: 34px;
    background: var(--well);
    border-radius: 4px;
  }
  .rampgraph path {
    fill: none;
    stroke: var(--accent);
    stroke-width: 1.5;
    vector-effect: non-scaling-stroke;
  }
  .warn {
    color: #f0a050;
    font-size: calc(12px * var(--text-scale));
    margin: 6px 0 0;
  }
  .note {
    color: var(--faint);
    font-size: calc(12px * var(--text-scale));
    margin: 4px 0 8px;
  }
  .muted {
    color: var(--muted);
  }
  .swatch {
    width: 36px;
    height: 20px;
    padding: 0;
    border: 1px solid var(--line);
    border-radius: 3px;
    cursor: pointer;
  }
</style>
