<script lang="ts">
  // Motion graphics template controls: set them on a composition clip, or
  // make the selected clip's settings controls of the sequence it's in.
  import { run } from "../lib/session.svelte";
  import { api, type Inspector, type ControlTarget, type ControlValue } from "../lib/api";
  import NumberField from "./NumberField.svelte";

  let { ins }: { ins: Inspector } = $props();
  let pick = $state(0);
  const free = $derived(ins.exposable.filter((x) => !x.exposed));
  const hex = (n: number) => "#" + Math.round(n).toString(16).padStart(6, "0");
  const num = (v: ControlValue | null) => (typeof v === "number" ? v : 0);
  const set = (i: number, v: ControlValue) => run(api.setControl(ins.id, i, v));
  // A number being dragged or typed: sent when it's done.
  let pending: ControlValue | null = null;
  const hold = (v: ControlValue) => (pending = v);
  const send = (i: number) => {
    if (pending !== null) set(i, pending);
    pending = null;
  };
</script>

{#if ins.controls.length}
  <div class="group">
    <h4 title="Settings of this composition (a motion graphics template) shown here. Each copy of a template is its own composition, so these set this one only">Template controls</h4>
    <div class="grid2">
      {#each ins.controls as k, i}
        <span class="lbl">{k.label}</span>
        {#if k.kind === "Text"}
          <textarea rows="2" value={typeof k.value === "string" ? k.value : ""} aria-label={k.label} onchange={(e) => set(i, e.currentTarget.value)}></textarea>
        {:else if k.kind === "Colour"}
          <input type="color" value={hex(num(k.value))} aria-label={k.label} onchange={(e) => set(i, parseInt(e.currentTarget.value.slice(1), 16))} />
        {:else if k.kind === "Choice"}
          <select value={num(k.value)} aria-label={k.label} onchange={(e) => set(i, Number(e.currentTarget.value))}>
            {#each k.options as o, j}<option value={j}>{o}</option>{/each}
          </select>
        {:else if k.kind === "Toggle"}
          <input type="checkbox" checked={num(k.value) > 0.5} aria-label={k.label} onchange={(e) => set(i, e.currentTarget.checked ? 1 : 0)} />
        {:else if k.kind === "Point"}
          {@const p = Array.isArray(k.value) ? k.value : [0, 0]}
          <div class="row">
            <NumberField label={`${k.label} X`} value={p[0]} step={1} digits={0} suffix=" px" oninput={(v) => hold([v, p[1]])} oncommit={() => send(i)} />
            <NumberField label={`${k.label} Y`} value={p[1]} step={1} digits={0} suffix=" px" oninput={(v) => hold([p[0], v])} oncommit={() => send(i)} />
          </div>
        {:else}
          <NumberField label={k.label} value={num(k.value)} min={k.kind === "Percent" ? 0 : undefined} max={k.kind === "Percent" ? 100 : undefined} step={0.5} digits={k.kind === "Percent" ? 0 : 2} suffix={k.kind === "Percent" ? "%" : ""} oninput={(v) => hold(v)} oncommit={() => send(i)} />
        {/if}
      {/each}
    </div>
  </div>
{/if}

{#if ins.exposable.length || ins.seq_controls.length}
  <details class="group expose">
    <summary title="Make settings of clips in this sequence its controls: they then show on every clip that plays this sequence as a composition, and in a motion graphics template saved from it (File menu)">Template controls of this sequence ({ins.seq_controls.length})</summary>
    {#if free.length}
      <div class="row">
        <select bind:value={pick} aria-label="Setting to make a control">
          {#each free as x, j}<option value={j}>{x.label}</option>{/each}
        </select>
        <button class="small" onclick={() => free[pick] && run(api.exposeControl(ins.id, free[pick].what as ControlTarget))}>Make control</button>
      </div>
    {/if}
    {#each ins.seq_controls as [label, , what], i}
      <div class="row">
        <input value={label} aria-label="Control name" title={what} onchange={(e) => run(api.editControl(i, e.currentTarget.value || label))} />
        <button class="small" onclick={() => run(api.editControl(i, null))} title="Stop showing it as a control" aria-label="Remove control">✕</button>
      </div>
    {/each}
  </details>
{/if}

<style>
  textarea {
    width: 100%;
    resize: vertical;
    font: inherit;
  }
  .expose .row {
    display: flex;
    gap: 6px;
    margin-top: 4px;
  }
  .expose select,
  .expose input {
    flex: 1;
    min-width: 0;
  }
  summary {
    cursor: pointer;
    color: var(--muted);
  }
</style>
