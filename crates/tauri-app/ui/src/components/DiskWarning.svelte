<script lang="ts">
  // A strip under the menu bar when the drive Reelforge saves to is running
  // short: how much is free, and buttons to make room (rendered previews,
  // proxies of media not in this project). Hidden with ✕ until space runs
  // short again.
  import { app, run } from "../lib/session.svelte";
  import { api } from "../lib/api";

  const d = $derived(app.live?.disk ?? null);
  const size = (b: number) => (b >= 1 << 30 ? `${(b / (1 << 30)).toFixed(1)} GB` : `${Math.max(1, Math.round(b / (1 << 20)))} MB`);
</script>

{#if d}
  <div class="disk" role="alert">
    <span class="ico" aria-hidden="true">⚠</span>
    <span class="msg">
      <b>Running out of disk space:</b> {size(d.free)} free on the drive with <span class="place" title={d.place}>{d.place}</span>.
      Renders and exports may not fit. Rendered previews aren't kept on disk for now, and proxies aren't made by themselves.
    </span>
    {#if d.render_bytes > 0}
      <button onclick={() => run(api.diskFree("render"))} title="Delete the rendered previews kept on disk (they're made again when needed)">Clear rendered previews ({size(d.render_bytes)})</button>
    {/if}
    {#if d.old_proxies > 0}
      <button onclick={() => run(api.diskFree("proxies"))} title="Delete proxies of footage that isn't in this project (other projects' proxies are made again if those are opened)">Delete {d.old_proxies} unused prox{d.old_proxies === 1 ? "y" : "ies"} ({size(d.old_proxy_bytes)})</button>
    {/if}
    <button class="x" onclick={() => run(api.diskDismiss())} aria-label="Hide this warning" title="Hide this warning">✕</button>
  </div>
{/if}

<style>
  .disk {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 5px 10px;
    background: color-mix(in srgb, var(--red) 18%, var(--panel));
    border-bottom: 1px solid color-mix(in srgb, var(--red) 45%, var(--line));
    font-size: calc(12px * var(--text-scale));
  }
  .ico {
    color: var(--red);
  }
  .msg {
    flex: 1;
    min-width: 0;
  }
  .place {
    font-family: ui-monospace, monospace;
    overflow-wrap: anywhere;
  }
  button {
    white-space: nowrap;
  }
  .x {
    background: none;
    border: 0;
    color: var(--muted);
    padding: 2px 6px;
  }
  .x:hover {
    color: var(--text);
  }
</style>
