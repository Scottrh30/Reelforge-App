<script lang="ts">
  import { app, run } from "../lib/session.svelte";
  import { api } from "../lib/api";
</script>

<div class="history">
  <p class="muted">Click a step to go back to it. Later steps stay until you make a new change.</p>
  <ol>
    <li>
      <button class:current={app.s?.history_pos === 0} onclick={() => run(api.historyGoTo(0))}>Start</button>
    </li>
    {#each app.s?.history ?? [] as h, i}
      <li>
        <button class:current={app.s?.history_pos === i + 1} class:future={(app.s?.history_pos ?? 0) < i + 1} onclick={() => run(api.historyGoTo(i + 1))}>{h}</button>
      </li>
    {/each}
  </ol>
</div>

<style>
  .history {
    height: 100%;
    overflow: auto;
    padding: 8px;
  }
  p {
    font-size: 12px;
    margin: 0 2px 8px;
  }
  ol {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  button {
    width: 100%;
    text-align: left;
    background: none;
    border: 0;
    border-left: 2px solid transparent;
    border-radius: 0;
    padding: 4px 8px;
  }
  button:hover {
    background: var(--raised);
  }
  .current {
    border-left-color: var(--accent);
    background: var(--accent-soft);
  }
  .future {
    color: var(--faint);
  }
</style>
