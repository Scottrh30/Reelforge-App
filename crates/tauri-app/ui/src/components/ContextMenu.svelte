<script lang="ts">
  import { menu, closeMenu } from "../lib/menu.svelte";

  let el: HTMLDivElement | undefined = $state();
  let pos = $state({ x: 0, y: 0 });

  // Keep the menu on screen.
  $effect(() => {
    if (!menu.open || !el) return;
    const r = el.getBoundingClientRect();
    pos = {
      x: Math.min(menu.x, window.innerWidth - r.width - 4),
      y: Math.min(menu.y, window.innerHeight - r.height - 4),
    };
    el.querySelector<HTMLButtonElement>("button:not(:disabled)")?.focus();
  });
</script>

<svelte:window
  onkeydown={(e) => menu.open && e.key === "Escape" && closeMenu()}
  onresize={closeMenu}
/>

{#if menu.open}
  <div class="scrim" role="presentation" onpointerdown={closeMenu} oncontextmenu={(e) => (e.preventDefault(), closeMenu())}></div>
  <div class="menu" role="menu" bind:this={el} style="left:{pos.x || menu.x}px; top:{pos.y || menu.y}px">
    {#each menu.items as it}
      {#if it.separator}
        <div class="sep"></div>
      {:else if it.heading}
        <div class="heading">{it.label}</div>
      {:else}
        <button
          role="menuitem"
          disabled={it.disabled}
          onclick={() => {
            closeMenu();
            it.run?.();
          }}
        >
          <span class="check">{it.checked ? "✓" : ""}</span>
          <span class="label">{it.label}</span>
          {#if it.hint}<span class="hint">{it.hint}</span>{/if}
        </button>
      {/if}
    {/each}
  </div>
{/if}

<style>
  .scrim {
    position: fixed;
    inset: 0;
    z-index: 50;
  }
  .menu {
    position: fixed;
    z-index: 51;
    min-width: 220px;
    background: var(--raised);
    border: 1px solid var(--line);
    border-radius: 6px;
    padding: 4px;
    box-shadow: 0 12px 32px rgba(0, 0, 0, 0.5);
    display: flex;
    flex-direction: column;
  }
  button {
    display: flex;
    align-items: center;
    gap: 6px;
    background: none;
    border: 0;
    border-radius: 3px;
    padding: 4px 10px 4px 4px;
    text-align: left;
    min-height: 26px;
  }
  button:hover:not(:disabled),
  button:focus-visible {
    background: var(--accent);
    color: var(--accent-ink);
    outline: none;
  }
  .check {
    width: 16px;
    text-align: center;
  }
  .label {
    flex: 1;
  }
  .hint {
    color: var(--muted);
    font-size: 12px;
    margin-left: 18px;
  }
  button:hover .hint {
    color: inherit;
    opacity: 0.7;
  }
  .sep {
    height: 1px;
    background: var(--line);
    margin: 4px 2px;
  }
  .heading {
    padding: 4px 10px 2px 26px;
    color: var(--muted);
    font-size: 12px;
    font-weight: 600;
  }
</style>
