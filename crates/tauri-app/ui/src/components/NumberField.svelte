<script lang="ts">
  // A number you can type, or drag sideways to change (Shift for fine steps).
  // `oninput` fires while it changes; `oncommit` when the change is done.
  let {
    value,
    step = 1,
    min = -Infinity,
    max = Infinity,
    digits = 1,
    suffix = "",
    label = "",
    oninput,
    oncommit,
  }: {
    value: number;
    step?: number;
    min?: number;
    max?: number;
    digits?: number;
    suffix?: string;
    label?: string;
    oninput: (v: number) => void;
    oncommit: () => void;
  } = $props();

  let editing = $state(false);
  let text = $state("");
  const clamp = (v: number) => Math.min(max, Math.max(min, v));

  function down(e: PointerEvent) {
    if (editing || e.button !== 0) return;
    const el = e.currentTarget as HTMLElement;
    el.setPointerCapture(e.pointerId);
    const x0 = e.clientX;
    const v0 = value;
    let moved = false;
    const move = (ev: PointerEvent) => {
      const dx = ev.clientX - x0;
      if (!moved && Math.abs(dx) < 3) return;
      moved = true;
      oninput(clamp(+(v0 + dx * step * (ev.shiftKey ? 0.1 : 1)).toFixed(4)));
    };
    const up = () => {
      el.removeEventListener("pointermove", move);
      el.removeEventListener("pointerup", up);
      if (moved) oncommit();
      else {
        text = value.toFixed(digits);
        editing = true;
      }
    };
    el.addEventListener("pointermove", move);
    el.addEventListener("pointerup", up);
  }

  function finish() {
    if (!editing) return;
    editing = false;
    const v = parseFloat(text);
    if (Number.isFinite(v)) {
      oninput(clamp(v));
      oncommit();
    }
  }
</script>

{#if editing}
  <!-- svelte-ignore a11y_autofocus -->
  <input
    type="text"
    class="num"
    bind:value={text}
    autofocus
    aria-label={label}
    onblur={finish}
    onkeydown={(e) => {
      if (e.key === "Enter") finish();
      if (e.key === "Escape") editing = false;
    }}
  />
{:else}
  <span class="num tc" role="slider" tabindex="0" aria-label={label} aria-valuenow={value} onpointerdown={down}
    onkeydown={(e) => {
      if (e.key === "ArrowUp" || e.key === "ArrowRight") (oninput(clamp(value + step)), oncommit());
      if (e.key === "ArrowDown" || e.key === "ArrowLeft") (oninput(clamp(value - step)), oncommit());
      if (e.key === "Enter") ((text = value.toFixed(digits)), (editing = true));
    }}
    title="Drag sideways to change, click to type">{value.toFixed(digits)}{suffix}</span>
{/if}

<style>
  .num {
    display: inline-block;
    min-width: 0;
    width: 100%;
    padding: 2px 6px;
    min-height: 24px;
    line-height: 18px;
    border-radius: var(--radius);
    background: var(--base);
    border: 1px solid var(--line);
    color: var(--accent);
    cursor: ew-resize;
    text-align: right;
  }
  span.num:hover {
    border-color: var(--faint);
  }
  input.num {
    color: var(--text);
    cursor: text;
  }
</style>
