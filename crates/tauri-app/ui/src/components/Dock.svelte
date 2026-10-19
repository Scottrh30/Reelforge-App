<script lang="ts">
  // A dock: the tabbed panels on one side of the window. Drag a tab to change
  // the order, onto the other dock to move it there, or anywhere else to float
  // it in its own window. Right-click a tab for the same, and to close it.
  import { setPanel } from "../lib/session.svelte";
  import { openMenu } from "../lib/menu.svelte";
  import { dock, tabDrag, dockPanel, floatPanel, saveDock, dropTarget, PANEL_LABEL, type DockSide, type PanelId } from "../lib/dock.svelte";
  import PanelBody from "./PanelBody.svelte";

  let { side, shown }: { side: DockSide; shown: PanelId[] } = $props();

  const active = $derived(shown.includes(dock.active[side] as PanelId) ? (dock.active[side] as PanelId) : (shown[0] ?? null));

  function select(id: PanelId) {
    dock.active[side] = id;
    saveDock();
  }

  function down(e: PointerEvent, id: PanelId) {
    if (e.button !== 0) return;
    const x0 = e.clientX;
    const y0 = e.clientY;
    const move = (ev: PointerEvent) => {
      if (!tabDrag.id && Math.hypot(ev.clientX - x0, ev.clientY - y0) < 6) return;
      tabDrag.id = id;
      tabDrag.x = ev.clientX;
      tabDrag.y = ev.clientY;
      tabDrag.over = dropTarget(ev.clientX, ev.clientY);
    };
    const up = (ev: PointerEvent) => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      if (!tabDrag.id) {
        select(id);
        return;
      }
      const t = dropTarget(ev.clientX, ev.clientY);
      tabDrag.id = null;
      tabDrag.over = null;
      if (t) dockPanel(id, t.side, t.index);
      else floatPanel(id, { x: ev.clientX, y: ev.clientY });
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
  }

  function menu(e: MouseEvent, id: PanelId) {
    const other: DockSide = side === "left" ? "right" : "left";
    openMenu(e, [
      { heading: true, label: PANEL_LABEL[id] },
      { label: other === "left" ? "Move to the left dock" : "Move to the right dock", run: () => dockPanel(id, other) },
      { label: "Float in its own window", run: () => floatPanel(id, { x: e.clientX, y: e.clientY }) },
      { separator: true },
      { label: "Close", run: () => setPanel(id, false) },
    ]);
  }
</script>

<div class="dock" data-dock={side}>
  <div class="tabs" role="tablist" class:target={tabDrag.over?.side === side}>
    {#each shown as id, i (id)}
      {#if tabDrag.over?.side === side && tabDrag.over.index === i}<span class="caret"></span>{/if}
      <button
        role="tab"
        data-tab={id}
        aria-selected={active === id}
        class:active={active === id}
        class:moving={tabDrag.id === id}
        onpointerdown={(e) => down(e, id)}
        oncontextmenu={(e) => menu(e, id)}
        title="Drag to move or float this panel; right-click for more">{PANEL_LABEL[id]}</button
      >
    {/each}
    {#if tabDrag.over?.side === side && tabDrag.over.index >= shown.length}<span class="caret"></span>{/if}
    <span class="fill"></span>
  </div>
  <div class="body">
    {#if active}<PanelBody id={active} />{/if}
  </div>
</div>

{#if tabDrag.id && side === "left"}
  <div class="ghost" style="left:{tabDrag.x + 12}px; top:{tabDrag.y + 8}px">{PANEL_LABEL[tabDrag.id]}{tabDrag.over ? "" : " — release to float"}</div>
{/if}

<style>
  .dock {
    height: 100%;
    display: flex;
    flex-direction: column;
    min-height: 0;
  }
  .tabs {
    display: flex;
    gap: 2px;
    padding: 4px 8px 0;
    border-bottom: 1px solid var(--line-soft);
    min-height: 31px;
    align-items: flex-end;
  }
  .tabs.target {
    background: color-mix(in srgb, var(--accent) 8%, transparent);
  }
  .tabs button {
    background: none;
    border: 0;
    border-bottom: 2px solid transparent;
    border-radius: 0;
    color: var(--muted);
    padding: 3px 10px;
    cursor: grab;
  }
  .tabs button.active {
    color: var(--text);
    border-bottom-color: var(--accent);
  }
  .tabs button.moving {
    opacity: 0.4;
  }
  .caret {
    width: 3px;
    align-self: stretch;
    margin: 3px 0 4px;
    background: var(--accent);
    border-radius: 2px;
  }
  .fill {
    flex: 1;
    align-self: stretch;
  }
  .body {
    flex: 1;
    min-height: 0;
  }
  .ghost {
    position: fixed;
    z-index: 80;
    background: var(--raised);
    border: 1px solid var(--accent);
    border-radius: 4px;
    padding: 2px 8px;
    pointer-events: none;
    font-size: calc(12px * var(--text-scale));
  }
</style>
