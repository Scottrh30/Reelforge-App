<script lang="ts">
  // A divider between two panels: drag it to resize the one beside it.
  // `value` is that panel's size in pixels; `sign` is -1 when the panel is
  // after the divider (dragging toward it makes it bigger).
  let {
    value = $bindable(),
    dir = "col",
    sign = 1,
    min = 120,
    max = 2000,
    key = "",
  }: { value: number; dir?: "col" | "row"; sign?: 1 | -1; min?: number; max?: number; key?: string } = $props();

  let dragging = $state(false);

  function down(e: PointerEvent) {
    const el = e.currentTarget as HTMLElement;
    el.setPointerCapture(e.pointerId);
    dragging = true;
    const start = dir === "col" ? e.clientX : e.clientY;
    const from = value;
    const move = (ev: PointerEvent) => {
      const now = dir === "col" ? ev.clientX : ev.clientY;
      value = Math.max(min, Math.min(max, from + sign * (now - start)));
    };
    const up = () => {
      dragging = false;
      el.removeEventListener("pointermove", move);
      el.removeEventListener("pointerup", up);
      if (key) {
        try {
          localStorage.setItem(key, String(Math.round(value)));
        } catch {}
      }
    };
    el.addEventListener("pointermove", move);
    el.addEventListener("pointerup", up);
  }
</script>

<div
  class="split {dir}"
  class:dragging
  role="separator"
  aria-orientation={dir === "col" ? "vertical" : "horizontal"}
  onpointerdown={down}
  title="Drag to resize"
></div>

<style>
  .split {
    flex: none;
    position: relative;
    z-index: 3;
  }
  .split.col {
    width: 5px;
    cursor: col-resize;
  }
  .split.row {
    height: 5px;
    cursor: row-resize;
  }
  .split::after {
    content: "";
    position: absolute;
    border-radius: 2px;
    background: var(--line);
    opacity: 0;
    transition: opacity 0.12s;
  }
  .split.col::after {
    left: 2px;
    width: 1px;
    top: 30%;
    bottom: 30%;
  }
  .split.row::after {
    top: 2px;
    height: 1px;
    left: 30%;
    right: 30%;
  }
  .split:hover::after,
  .split.dragging::after {
    opacity: 1;
    background: var(--accent);
  }
</style>
