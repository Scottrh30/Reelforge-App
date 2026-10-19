<script lang="ts">
  // A panel floating in its own window over the editor: drag its title bar to
  // move it (drop it on a dock's tabs to dock it again), the corner to resize.
  import { setPanel } from "../lib/session.svelte";
  import { dock, tabDrag, dockPanel, raise, saveDock, clampRect, dropTarget, PANEL_LABEL, type PanelId } from "../lib/dock.svelte";
  import PanelBody from "./PanelBody.svelte";

  let { id, z }: { id: PanelId; z: number } = $props();
  const r = $derived(dock.rects[id] ?? { x: 120, y: 90, w: 320, h: 460 });

  function moveDown(e: PointerEvent) {
    if (e.button !== 0 || (e.target as HTMLElement).closest("button")) return;
    raise(id);
    const start = { ...r };
    const x0 = e.clientX;
    const y0 = e.clientY;
    const move = (ev: PointerEvent) => {
      dock.rects[id] = clampRect({ ...start, x: start.x + ev.clientX - x0, y: start.y + ev.clientY - y0 });
      // Over a dock's tabs: it would dock there.
      const t = dropTarget(ev.clientX, ev.clientY);
      tabDrag.over = t && (document.elementFromPoint(ev.clientX, ev.clientY)?.closest("[role=tablist]") ? t : null);
    };
    const up = (ev: PointerEvent) => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      const t = tabDrag.over;
      tabDrag.over = null;
      if (t) dockPanel(id, t.side, t.index);
      else saveDock();
      void ev;
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
  }

  function resizeDown(e: PointerEvent) {
    e.stopPropagation();
    raise(id);
    const start = { ...r };
    const x0 = e.clientX;
    const y0 = e.clientY;
    const move = (ev: PointerEvent) => (dock.rects[id] = clampRect({ ...start, w: start.w + ev.clientX - x0, h: start.h + ev.clientY - y0 }));
    const up = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      saveDock();
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
  }
</script>

<section class="float" style="left:{r.x}px; top:{r.y}px; width:{r.w}px; height:{r.h}px; z-index:{40 + z}" aria-label={PANEL_LABEL[id]} onpointerdown={() => raise(id)}>
  <header role="toolbar" aria-label="{PANEL_LABEL[id]} window" tabindex="-1" onpointerdown={moveDown} title="Drag to move; drop on a dock's tabs to dock it">
    <span class="title">{PANEL_LABEL[id]}</span>
    <button class="ghostbtn" onclick={() => dockPanel(id, "left")} title="Dock on the left" aria-label="Dock on the left">⇤</button>
    <button class="ghostbtn" onclick={() => dockPanel(id, "right")} title="Dock on the right" aria-label="Dock on the right">⇥</button>
    <button class="ghostbtn" onclick={() => setPanel(id, false)} title="Close" aria-label="Close">✕</button>
  </header>
  <div class="body"><PanelBody {id} /></div>
  <span class="grip" role="separator" aria-label="Resize" onpointerdown={resizeDown}></span>
</section>

<style>
  .float {
    position: fixed;
    background: var(--panel);
    border: 1px solid var(--line);
    border-radius: 8px;
    box-shadow: 0 10px 30px rgba(0, 0, 0, 0.45);
    display: flex;
    flex-direction: column;
    overflow: hidden;
    min-width: 0;
  }
  header {
    display: flex;
    align-items: center;
    gap: 4px;
    height: 30px;
    padding: 0 6px 0 10px;
    border-bottom: 1px solid var(--line-soft);
    cursor: move;
    user-select: none;
  }
  .title {
    flex: 1;
    font-weight: 500;
  }
  .ghostbtn {
    background: none;
    border: 0;
    min-height: 22px;
    padding: 0 6px;
    color: var(--muted);
  }
  .ghostbtn:hover {
    color: var(--text);
  }
  .body {
    flex: 1;
    min-height: 0;
  }
  .grip {
    position: absolute;
    right: 0;
    bottom: 0;
    width: 14px;
    height: 14px;
    cursor: nwse-resize;
    background: linear-gradient(135deg, transparent 50%, var(--line) 50%, var(--line) 60%, transparent 60%, transparent 70%, var(--line) 70%, var(--line) 80%, transparent 80%);
  }
</style>
