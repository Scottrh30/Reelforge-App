<script lang="ts">
  import { compThumb, thumb } from "../lib/thumbs";

  // A media file's picture (`path`), or a composition's (`comp`, its
  // sequence, as of project revision `rev`).
  let { path = "", at = 0, alt = "", comp = null, rev = 0 }: { path?: string; at?: number; alt?: string; comp?: number | null; rev?: number } = $props();
  let src = $state<string | null>(null);

  let shown = "";
  $effect(() => {
    let live = true;
    const key = comp === null ? `${path}@${at}` : `#${comp}@${at}`;
    // A composition's newer picture replaces the old one when it's ready (no blink).
    if (key !== shown) src = null;
    shown = key;
    (comp === null ? thumb(path, at) : compThumb(comp, at, rev)).then((u) => live && (src = u));
    return () => (live = false);
  });
</script>

{#if src}<img {src} {alt} draggable="false" />{/if}

<style>
  img {
    width: 100%;
    height: 100%;
    object-fit: cover;
    display: block;
    pointer-events: none;
  }
</style>
