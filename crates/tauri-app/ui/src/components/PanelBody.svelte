<script lang="ts">
  // The contents of a dockable panel, by id.
  import type { PanelId } from "../lib/dock.svelte";
  import MediaPanel from "./MediaPanel.svelte";
  import History from "./History.svelte";
  import Controls from "./Controls.svelte";
  import EffectsLibrary from "./EffectsLibrary.svelte";
  import TrackPanel from "./TrackPanel.svelte";

  let { id }: { id: PanelId } = $props();
</script>

{#if id === "media"}<MediaPanel />{:else if id === "effects"}<EffectsLibrary />{:else if id === "history"}<History />{:else if id === "track"}<TrackPanel />{:else}<Controls />{/if}
