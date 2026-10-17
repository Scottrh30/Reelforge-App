<script lang="ts">
  import { app } from "../lib/session.svelte";

  const FLOOR = -60;
  const pct = (db: number) => Math.max(0, Math.min(100, ((db - FLOOR) / -FLOOR) * 100));
  const m = $derived(app.live?.master);
  const scale = [0, -6, -12, -18, -24, -36, -48];
</script>

<div class="meters" aria-label="Output level">
  <div class="bars">
    {#each [0, 1] as ch}
      <div class="bar" title="{ch ? 'Right' : 'Left'}: {m ? m.hold[ch].toFixed(1) : '-∞'} dB peak">
        <span class="clip" class:on={m?.clipped[ch]}></span>
        <div class="well">
          <div class="fill" style="height:{m ? pct(m.peak[ch]) : 0}%"></div>
          <div class="rms" style="height:{m ? pct(m.rms[ch]) : 0}%"></div>
          {#if m && m.hold[ch] > FLOOR}<div class="hold" style="bottom:{pct(m.hold[ch])}%"></div>{/if}
        </div>
      </div>
    {/each}
    <div class="scale">
      {#each scale as db}<span style="bottom:{pct(db)}%">{db}</span>{/each}
    </div>
  </div>
  <div class="read tc">
    {#each [0, 1] as ch}<span>{m && m.hold[ch] > FLOOR ? m.hold[ch].toFixed(0) : "-∞"}</span>{/each}
  </div>
</div>

<style>
  .meters {
    height: 100%;
    display: flex;
    flex-direction: column;
    padding: 10px 8px 8px;
    gap: 6px;
  }
  .bars {
    flex: 1;
    display: flex;
    gap: 4px;
    min-height: 0;
  }
  .bar {
    width: 14px;
    display: flex;
    flex-direction: column;
    gap: 3px;
  }
  .clip {
    height: 5px;
    border-radius: 1px;
    background: #3a2422;
  }
  .clip.on {
    background: var(--red);
  }
  .well {
    flex: 1;
    position: relative;
    background: var(--well);
    border-radius: 2px;
    overflow: hidden;
  }
  .fill,
  .rms {
    position: absolute;
    left: 0;
    right: 0;
    bottom: 0;
  }
  .fill {
    background: linear-gradient(to top, #3fa564 0%, #3fa564 70%, var(--accent) 85%, var(--red) 100%);
    background-size: 100% var(--h, 100%);
    opacity: 0.55;
  }
  .rms {
    background: #3fa564;
  }
  .hold {
    position: absolute;
    left: 0;
    right: 0;
    height: 2px;
    background: #e8eaed;
  }
  .scale {
    position: relative;
    width: 22px;
    font-size: calc(10px * var(--text-scale));
    color: var(--faint);
    margin-top: 8px;
  }
  .scale span {
    position: absolute;
    right: 0;
    transform: translateY(50%);
  }
  .read {
    display: flex;
    gap: 4px;
    font-size: calc(11px * var(--text-scale));
    color: var(--muted);
  }
  .read span {
    width: 14px;
    text-align: center;
  }
</style>
