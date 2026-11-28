<script lang="ts">
  // A clip's audio effects (in the Sound section): add EQ, compressor,
  // limiter, reverb or noise reduction; switch each on or off, reorder,
  // remove; set their parameters, keyframed at the playhead like volume.
  import { api, type AudioEffect, type AudioFxInfo, type AudioFxKind, type ClipAudio } from "../lib/api";
  import { app } from "../lib/session.svelte";
  import { openMenu } from "../lib/menu.svelte";
  import NumberField from "./NumberField.svelte";

  let {
    audio,
    src,
    onClip,
    edit,
    once,
    commit,
  }: {
    audio: ClipAudio;
    /// Source time at the playhead (keys go there).
    src: number;
    onClip: boolean;
    edit: (f: (n: ClipAudio) => void) => void;
    once: (f: (n: ClipAudio) => void) => void;
    commit: () => void;
  } = $props();

  let kinds = $state<AudioFxInfo[]>([]);
  api.audioFxKinds().then((k) => (kinds = k));
  const info = (k: AudioFxKind) => kinds.find((x) => x.kind === k);
  const effects = $derived(audio.effects ?? []);

  // Folded effects (by position), so a long chain stays tidy.
  let folded = $state<Record<number, boolean>>({});
  // Not folded or opened by hand: as Settings → expand every section says.
  const shut = (k: number) => folded[k] ?? !(app.s?.prefs.expand_controls ?? true);

  /// Parameter `p` at source time `t`: its keys (straight lines between them), else its value.
  function valueAt(fx: AudioEffect, i: number, t: number): number {
    const p = fx.params[i];
    const spec = info(fx.kind)?.params[i];
    if (!p) return spec?.default ?? 0;
    const k = p.keys ?? [];
    let v = p.value;
    if (k.length) {
      const j = k.findIndex((x) => x.at > t);
      if (j === 0) v = k[0].value;
      else if (j < 0) v = k[k.length - 1].value;
      else {
        const [a, b] = [k[j - 1], k[j]];
        v = b.at === a.at ? b.value : a.value + ((b.value - a.value) * (t - a.at)) / (b.at - a.at);
      }
    }
    return spec ? Math.min(spec.max, Math.max(spec.min, v)) : v;
  }

  function add(e: MouseEvent) {
    openMenu(
      e,
      kinds.map((k) => ({
        label: k.label,
        run: () =>
          once((n) => {
            n.effects = [...(n.effects ?? []), { kind: k.kind, enabled: true, params: k.params.map((p) => ({ value: p.default, keys: [] })) }];
          }),
      })),
    );
  }
  function more(e: MouseEvent, i: number) {
    const fx = effects[i];
    openMenu(e, [
      { heading: true, label: info(fx.kind)?.label ?? fx.kind },
      { label: "Move up", disabled: i === 0, run: () => once((n) => n.effects && ([n.effects[i - 1], n.effects[i]] = [n.effects[i], n.effects[i - 1]])) },
      { label: "Move down", disabled: i === effects.length - 1, run: () => once((n) => n.effects && ([n.effects[i + 1], n.effects[i]] = [n.effects[i], n.effects[i + 1]])) },
      { label: "Reset to defaults", run: () => once((n) => n.effects && (n.effects[i].params = (info(fx.kind)?.params ?? []).map((p) => ({ value: p.default, keys: [] })))) },
      { separator: true },
      { label: "Remove", run: () => once((n) => (n.effects = (n.effects ?? []).filter((_, j) => j !== i))) },
    ]);
  }

  function ensure(n: ClipAudio, i: number, p: number) {
    const fx = n.effects![i];
    while (fx.params.length <= p) {
      const spec = info(fx.kind)?.params[fx.params.length];
      fx.params.push({ value: spec?.default ?? 0, keys: [] });
    }
    fx.params[p].keys ??= [];
    return fx.params[p];
  }
  /// Set a value: at the playhead's key when it's keyframed, else for the whole clip.
  function set(i: number, p: number, v: number) {
    edit((n) => {
      const prm = ensure(n, i, p);
      if (prm.keys.length) {
        if (!onClip) return;
        const k = prm.keys.find((x) => x.at === src);
        if (k) k.value = v;
        else prm.keys = [...prm.keys, { at: src, value: v }].sort((a, b) => a.at - b.at);
      } else prm.value = v;
    });
  }
  function toggleKey(i: number, p: number) {
    once((n) => {
      const prm = ensure(n, i, p);
      const here = prm.keys.findIndex((x) => x.at === src);
      if (here >= 0) {
        // The last key gone: the value stays where it was.
        if (prm.keys.length === 1) prm.value = prm.keys[0].value;
        prm.keys = prm.keys.filter((_, j) => j !== here);
      } else prm.keys = [...prm.keys, { at: src, value: valueAt(n.effects![i], p, src) }].sort((a, b) => a.at - b.at);
    });
  }
</script>

<div class="fx">
  <div class="head">
    <span class="lbl">Effects</span>
    <button class="ghost small" onclick={add} disabled={!kinds.length} title="Add an effect to this clip's sound (it works before the volume, pan and fades)">+ Add</button>
  </div>
  {#if !effects.length}
    <p class="note">EQ, compressor, limiter, reverb, noise reduction. They change the clip's own sound, before its volume and fades; playback and export sound the same.</p>
  {/if}
  {#each effects as fx, i (i)}
    {@const meta = info(fx.kind)}
    <div class="effect" class:off={!fx.enabled}>
      <div class="title">
        <button class="ghost fold" onclick={() => (folded[i] = !shut(i))} aria-expanded={!shut(i)} aria-label="Show settings">{shut(i) ? "▸" : "▾"}</button>
        <label class="check"><input type="checkbox" checked={fx.enabled} onchange={() => once((n) => n.effects && (n.effects[i].enabled = !n.effects[i].enabled))} title="Switch this effect on or off" /> {meta?.label ?? fx.kind}</label>
        <span class="grow"></span>
        <button class="ghost small" onclick={(e) => more(e, i)} aria-label="More for {meta?.label}" title="Move, reset, remove">⋯</button>
      </div>
      {#if !shut(i) && meta}
        <div class="params">
          {#each meta.params as spec, p}
            {@const keys = fx.params[p]?.keys ?? []}
            {@const here = onClip && keys.some((k) => k.at === src)}
            <span class="pname" title={spec.name}>{spec.name}</span>
            <NumberField label={spec.name} value={valueAt(fx, p, src)} min={spec.min} max={spec.max} step={spec.speed} digits={spec.max - spec.min > 200 ? 0 : 1} suffix={spec.suffix} oninput={(v) => set(i, p, v)} oncommit={commit} />
            <button class="key" class:anim={keys.length > 0} class:here disabled={!onClip} onclick={() => toggleKey(i, p)} title={!onClip ? "Move the playhead onto the clip to set keyframes" : here ? `Remove the ${spec.name} keyframe at the playhead` : `Add a ${spec.name} keyframe at the playhead`} aria-label="{spec.name} keyframe"><svg viewBox="0 0 10 10" width="10" height="10"><path d="M5 1 9 5 5 9 1 5Z" /></svg></button>
          {/each}
        </div>
      {/if}
    </div>
  {/each}
</div>

<style>
  .fx {
    margin-top: 10px;
  }
  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }
  .lbl {
    color: var(--muted);
  }
  .small {
    min-height: 20px;
    padding: 0 6px;
    font-size: calc(11.5px * var(--text-scale));
  }
  .note {
    color: var(--faint);
    font-size: calc(12px * var(--text-scale));
    margin: 4px 0;
  }
  .effect {
    margin-top: 6px;
    border: 1px solid var(--line-soft);
    border-radius: 6px;
    padding: 4px 6px 6px;
  }
  .effect.off .params,
  .effect.off .check {
    opacity: 0.55;
  }
  .title {
    display: flex;
    align-items: center;
    gap: 4px;
  }
  .fold {
    min-height: 20px;
    padding: 0 4px;
  }
  .check {
    display: flex;
    align-items: center;
    gap: 6px;
    font-weight: 600;
  }
  .grow {
    flex: 1;
  }
  .params {
    display: grid;
    grid-template-columns: 96px 1fr 18px;
    gap: 4px 6px;
    align-items: center;
    margin-top: 4px;
  }
  .pname {
    color: var(--muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
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
</style>
