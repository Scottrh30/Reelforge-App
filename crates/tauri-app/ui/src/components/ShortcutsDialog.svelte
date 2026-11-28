<script lang="ts">
  // Settings → Keyboard shortcuts…: every command with a shortcut, its keys,
  // searchable; click + to press new keys for one (a key another command
  // uses is pointed out, and can be taken from it), × to remove one.
  import { SHORTCUTS, custom, keysOf, setKeys, resetAll, comboOf, clashes, pretty, type Shortcut } from "../lib/shortcuts.svelte";
  import { shortcutsDialog } from "../lib/shortcuts.svelte";

  let search = $state("");
  // The command waiting for a key to be pressed, and a key pressed that another uses.
  let recording = $state<string | null>(null);
  let pending = $state<{ id: string; combo: string; others: Shortcut[] } | null>(null);

  const shown = $derived.by(() => {
    void custom.keys;
    const q = search.trim().toLowerCase();
    return SHORTCUTS.filter((s) => !q || s.label.toLowerCase().includes(q) || s.group.toLowerCase().includes(q) || keysOf(s.id).some((k) => k.toLowerCase().includes(q)));
  });
  const groups = $derived([...new Set(shown.map((s) => s.group))]);
  const changed = (s: Shortcut) => custom.keys[s.id] !== undefined;

  function key(e: KeyboardEvent) {
    if (!shortcutsDialog.open) return;
    if (recording) {
      e.preventDefault();
      e.stopImmediatePropagation();
      if (e.key === "Escape" && !e.ctrlKey && !e.altKey && !e.shiftKey) {
        recording = null;
        return;
      }
      const combo = comboOf(e);
      if (!combo) return;
      const id = recording;
      recording = null;
      const others = clashes(combo, id);
      if (others.length) pending = { id, combo, others };
      else add(id, combo);
      return;
    }
    if (e.key === "Escape") {
      e.stopImmediatePropagation();
      if (pending) pending = null;
      else shortcutsDialog.open = false;
    }
  }
  function add(id: string, combo: string) {
    if (!keysOf(id).includes(combo)) setKeys(id, [...keysOf(id), combo]);
  }
  function remove(id: string, combo: string) {
    setKeys(id, keysOf(id).filter((k) => k !== combo));
  }
  /// Give the key to this command, taking it from the others.
  function takeOver() {
    if (!pending) return;
    for (const o of pending.others) remove(o.id, pending.combo);
    add(pending.id, pending.combo);
    pending = null;
  }
  const label = (combo: string) => pretty(combo).replace("Ctrl+", navigator.platform.startsWith("Mac") ? "⌘" : "Ctrl+");
</script>

<svelte:window onkeydowncapture={key} />

{#if shortcutsDialog.open}
  <div class="scrim">
    <div class="dialog" role="dialog" aria-modal="true" aria-labelledby="keys-title">
      <h2 id="keys-title">Keyboard shortcuts</h2>
      <div class="top">
        <input type="search" placeholder="Search commands or keys" bind:value={search} aria-label="Search shortcuts" />
        <button class="ghost" onclick={() => resetAll()} title="Every command back to the keys it came with">Reset all</button>
      </div>
      {#if pending}
        <div class="clash" role="alert">
          <span><b>{label(pending.combo)}</b> is used by {pending.others.map((o) => o.label).join(", ")}.</span>
          <button class="primary" onclick={takeOver}>Use it for {SHORTCUTS.find((s) => s.id === pending?.id)?.label}</button>
          <button class="ghost" onclick={() => (pending = null)}>Cancel</button>
        </div>
      {/if}
      <div class="list">
        {#each groups as g (g)}
          <h3>{g}</h3>
          {#each shown.filter((s) => s.group === g) as s (s.id)}
            <div class="row" class:changed={changed(s)}>
              <span class="name">{s.label}</span>
              <span class="keys">
                {#each keysOf(s.id) as k (k)}
                  <span class="key">{label(k)}<button class="x" aria-label="Remove {k} from {s.label}" title="Remove this key" onclick={() => remove(s.id, k)}>×</button></span>
                {/each}
                {#if recording === s.id}
                  <span class="key wait">Press a key… (Esc: cancel)</span>
                {:else}
                  <button class="tiny" aria-label="Add a key for {s.label}" title="Press the keys to use for this" onclick={() => (recording = s.id)}>+</button>
                {/if}
                {#if changed(s)}<button class="tiny ghost" title="Back to {s.keys.join(', ') || 'no key'}" onclick={() => setKeys(s.id, s.keys)}>Reset</button>{/if}
              </span>
            </div>
          {/each}
        {/each}
        {#if !shown.length}<p class="muted">Nothing matches “{search}”.</p>{/if}
      </div>
      <div class="buttons"><button class="primary" onclick={() => (shortcutsDialog.open = false)}>Done</button></div>
    </div>
  </div>
{/if}

<style>
  .scrim {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.5);
    display: grid;
    place-items: center;
    z-index: 60;
  }
  .dialog {
    background: var(--panel);
    border: 1px solid var(--line);
    border-radius: 8px;
    width: min(640px, 94vw);
    max-height: 86vh;
    display: flex;
    flex-direction: column;
    padding: 16px;
    gap: 10px;
  }
  h2 {
    margin: 0;
    font-size: 15px;
  }
  h3 {
    margin: 10px 0 4px;
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--muted);
  }
  .top {
    display: flex;
    gap: 8px;
  }
  .top input {
    flex: 1;
  }
  .list {
    overflow: auto;
    flex: 1;
    min-height: 200px;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 3px 4px;
    border-radius: 4px;
  }
  .row:hover {
    background: var(--raised);
  }
  .row.changed .name {
    color: var(--accent);
  }
  .name {
    flex: 1;
  }
  .keys {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
    justify-content: flex-end;
  }
  .key {
    display: inline-flex;
    align-items: center;
    gap: 2px;
    background: var(--well);
    border: 1px solid var(--line);
    border-radius: 4px;
    padding: 1px 2px 1px 6px;
    font-family: var(--font-numbers, inherit);
    font-size: 12px;
  }
  .key.wait {
    padding: 1px 6px;
    border-color: var(--accent);
    color: var(--accent);
  }
  .x {
    background: none;
    border: none;
    color: var(--muted);
    padding: 0 3px;
    cursor: pointer;
  }
  .x:hover {
    color: var(--text);
  }
  .clash {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
    background: color-mix(in srgb, var(--accent) 14%, var(--well));
    border: 1px solid var(--accent);
    border-radius: 6px;
    padding: 8px;
  }
  .clash span {
    flex: 1;
  }
  .buttons {
    display: flex;
    justify-content: flex-end;
  }
  .muted {
    color: var(--muted);
  }
</style>
