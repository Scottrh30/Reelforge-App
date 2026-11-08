<script lang="ts">
  // The Text tab: add 2D text, and style the text clip picked on the
  // timeline (font, colour, size, scale, spacing, case, alignment, and
  // kerning pair by pair).
  import { app } from "../lib/session.svelte";
  import { actions } from "../lib/actions.svelte";
  import TitlePanel from "./TitlePanel.svelte";

  const ins = $derived(app.s?.inspector ?? null);
  const text = $derived(ins?.title && !ins.title.model && !ins.title.shape ? ins : null);
</script>

<div class="text-panel">
  <div class="top">
    <button class="primary" onclick={actions.addTitle} title="Add 2D text at the playhead (Ctrl+T)">+ Add text</button>
    <span class="muted">{text ? (ins?.name ?? "") : ""}</span>
  </div>
  {#if text}
    {#key text.id}<TitlePanel ins={text} />{/key}
  {:else}
    <p class="empty">
      {#if ins?.title}
        This clip is a 3D logo or model: its settings are in Controls.
      {:else}
        Add text, or pick a text clip on the timeline to change its font, colour, size, scale, spacing, case, alignment and kerning.
      {/if}
    </p>
  {/if}
</div>

<style>
  .text-panel {
    padding: 8px 10px;
    overflow: auto;
    height: 100%;
    box-sizing: border-box;
  }
  .top {
    display: flex;
    gap: 8px;
    align-items: center;
    padding-bottom: 4px;
  }
  .muted {
    color: var(--muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .empty {
    color: var(--muted);
    line-height: 1.45;
  }
</style>
