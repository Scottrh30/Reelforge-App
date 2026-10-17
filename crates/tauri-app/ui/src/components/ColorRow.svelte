<script lang="ts">
  // One colour setting: a swatch to pick from, and its hex code to type.
  let { label, hint = "", value = $bindable() }: { label: string; hint?: string; value: string } = $props();
  let text = $state("");
  $effect(() => {
    text = value;
  });
  function typed() {
    const v = text.trim();
    const m = /^#?([0-9a-f]{3}|[0-9a-f]{6})$/i.exec(v);
    if (!m) {
      text = value;
      return;
    }
    let h = m[1].toLowerCase();
    if (h.length === 3) h = [...h].map((c) => c + c).join("");
    value = `#${h}`;
  }
</script>

<label class="row">
  <span class="name">
    {label}
    {#if hint}<small>{hint}</small>{/if}
  </span>
  <input type="color" bind:value aria-label="{label} colour" />
  <input
    type="text"
    class="hex"
    bind:value={text}
    spellcheck="false"
    aria-label="{label} hex code"
    onblur={typed}
    onkeydown={(e) => e.key === "Enter" && typed()}
  />
</label>

<style>
  .row {
    display: grid;
    grid-template-columns: 1fr 40px 84px;
    align-items: center;
    gap: 8px;
    padding: 4px 0;
  }
  .name {
    display: flex;
    flex-direction: column;
  }
  small {
    color: var(--muted);
    font-size: calc(11.5px * var(--text-scale));
  }
  input[type="color"] {
    width: 40px;
    height: 26px;
    padding: 0;
    border: 1px solid var(--line);
    border-radius: var(--radius);
    background: none;
    cursor: pointer;
  }
  .hex {
    font-family: var(--font-tc);
    text-transform: lowercase;
  }
</style>
