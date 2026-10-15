<script lang="ts">
  import { thumb } from "../lib/thumbs";

  let { path, at = 0, alt = "" }: { path: string; at?: number; alt?: string } = $props();
  let src = $state<string | null>(null);

  $effect(() => {
    let live = true;
    src = null;
    thumb(path, at).then((u) => live && (src = u));
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
