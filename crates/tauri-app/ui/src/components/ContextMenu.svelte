<script lang="ts">
  import { menu, closeMenu } from "../lib/menu.svelte";

  let el: HTMLDivElement | undefined = $state();
  let pos = $state({ x: 0, y: 0 });
  /// The open submenu: which item, and where it goes.
  let sub = $state<{ index: number; x: number; y: number } | null>(null);
  let subEl: HTMLDivElement | undefined = $state();

  $effect(() => {
    if (!menu.open) sub = null;
  });

  function openSub(e: Event, index: number, focus = false) {
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    if (sub?.index !== index) sub = { index, x: r.right + 2, y: r.top - 5 };
    if (focus) queueMicrotask(() => subEl?.querySelector<HTMLButtonElement>("button:not(:disabled)")?.focus());
  }

  // Keep the submenu on screen too (to the left of the menu if there's no room on the right).
  $effect(() => {
    if (!sub || !subEl || !el) return;
    const r = subEl.getBoundingClientRect();
    const m = el.getBoundingClientRect();
    const x = sub.x + r.width > window.innerWidth - 4 ? m.left - r.width - 2 : sub.x;
    const y = Math.min(sub.y, window.innerHeight - r.height - 4);
    if (x !== sub.x || y !== sub.y) sub = { ...sub, x, y };
  });

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
    {#each menu.items as it, i}
      {#if it.separator}
        <div class="sep"></div>
      {:else if it.heading}
        <div class="heading">{it.label}</div>
      {:else if it.submenu}
        <button
          role="menuitem"
          aria-haspopup="menu"
          aria-expanded={sub?.index === i}
          class:open={sub?.index === i}
          disabled={it.disabled}
          onpointerenter={(e) => openSub(e, i)}
          onclick={(e) => openSub(e, i, true)}
          onkeydown={(e) => e.key === "ArrowRight" && openSub(e, i, true)}
        >
          <span class="check"></span>
          <span class="label">{it.label}</span>
          <span class="arrow" aria-hidden="true">▸</span>
        </button>
      {:else}
        <button
          role="menuitem"
          disabled={it.disabled}
          onpointerenter={() => (sub = null)}
          onclick={() => {
            closeMenu();
            it.run?.();
          }}
        >
          <span class="check">{it.checked ? "✓" : ""}</span>
          {#if it.swatch}<span class="swatch" style="background:{it.swatch}"></span>{/if}
          <span class="label">{it.label}</span>
          {#if it.hint}<span class="hint">{it.hint}</span>{/if}
        </button>
      {/if}
    {/each}
  </div>
  {#if sub && menu.items[sub.index]?.submenu}
    <div
      class="menu"
      role="menu"
      tabindex="-1"
      bind:this={subEl}
      style="left:{sub.x}px; top:{sub.y}px"
      onkeydown={(e) => e.key === "ArrowLeft" && (sub = null)}
    >
      {#each menu.items[sub.index].submenu ?? [] as it}
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
            {#if it.swatch}<span class="swatch" style="background:{it.swatch}"></span>{/if}
            <span class="label">{it.label}</span>
            {#if it.hint}<span class="hint">{it.hint}</span>{/if}
          </button>
        {/if}
      {/each}
    </div>
  {/if}
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
    color: var(--text);
  }
  button:disabled {
    color: var(--faint);
  }
  button:hover:not(:disabled),
  button.open,
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
  .swatch {
    width: 12px;
    height: 12px;
    border-radius: 3px;
    margin-right: 8px;
    flex: none;
    box-shadow: inset 0 0 0 1px rgba(255, 255, 255, 0.25);
  }
  .hint {
    color: var(--muted);
    font-size: calc(12px * var(--text-scale));
    margin-left: 18px;
  }
  button:hover .hint {
    color: inherit;
    opacity: 0.7;
  }
  .arrow {
    font-size: calc(11px * var(--text-scale));
    opacity: 0.8;
  }
  .sep {
    height: 1px;
    background: var(--line);
    margin: 4px 2px;
  }
  .heading {
    padding: 4px 10px 2px 26px;
    color: var(--muted);
    font-size: calc(12px * var(--text-scale));
    font-weight: 600;
  }
</style>
