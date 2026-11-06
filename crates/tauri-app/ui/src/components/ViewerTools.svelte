<script lang="ts">
  // The mask tools down the left side of the Viewer. Rectangle, Ellipse and
  // Freehand make a mask on the selected clip; Move and Resize are modes in
  // which dragging any of its masks in the Viewer moves it or resizes it.
  // Hovering a tool shows its name.
  import { app, run, say } from "../lib/session.svelte";
  import { api } from "../lib/api";

  type Tool = "rectangle" | "ellipse" | "freehand" | "move" | "resize";
  const tools: { id: Tool; name: string; hint: string }[] = [
    { id: "rectangle", name: "Rectangle mask", hint: "Adds a rectangle mask to the selected clip" },
    { id: "ellipse", name: "Ellipse mask", hint: "Adds an ellipse mask to the selected clip" },
    { id: "freehand", name: "Freehand mask", hint: "Click points in the Viewer; click the first point or press Enter to close" },
    { id: "move", name: "Move mask", hint: "Drag a mask in the Viewer to move it" },
    { id: "resize", name: "Resize mask", hint: "Drag a mask in the Viewer to make it bigger or smaller (Shift: width or height only)" },
  ];

  type CamTool = "orbit" | "pan" | "dolly";
  const camTools: { id: CamTool; name: string; hint: string }[] = [
    { id: "orbit", name: "Orbit camera", hint: "Drag in the Viewer to circle the camera round its point of interest (from Front, Top or Left: the view)" },
    { id: "pan", name: "Pan camera", hint: "Drag in the Viewer to slide the camera and its point of interest across the view" },
    { id: "dolly", name: "Dolly camera", hint: "Drag up or down in the Viewer (or turn the wheel) to move the camera closer or further" },
  ];
  function useCam(t: CamTool) {
    app.maskTool = null;
    app.drawing = null;
    app.camTool = app.camTool === t ? null : t;
    if (app.camTool) say(`${camTools.find((c) => c.id === t)!.name}: drag in the Viewer (Esc leaves the tool)`);
  }

  const ins = $derived(app.s?.inspector ?? null);
  /// Masks go on video layers.
  const canMask = $derived(!!ins?.visual && !!ins.layer_map);
  const hasMasks = $derived((ins?.masks.length ?? 0) > 0);
  const drawing = $derived(!!app.drawing && app.drawing.clip === ins?.id);

  // Leave Move/Resize when there's nothing for them to work on.
  $effect(() => {
    if (app.maskTool && !hasMasks) app.maskTool = null;
  });

  function enabled(t: Tool) {
    if (!canMask) return false;
    return t === "move" || t === "resize" ? hasMasks : true;
  }
  function on(t: Tool) {
    return (t === "freehand" && drawing) || app.maskTool === t;
  }
  function why(t: Tool) {
    if (!ins) return "Select a clip on the timeline first";
    if (!canMask) return "Masks go on video clips";
    if ((t === "move" || t === "resize") && !hasMasks) return "Add a mask first";
    return "";
  }

  async function use(t: Tool) {
    if (!ins || !enabled(t)) return;
    if (t === "rectangle" || t === "ellipse") {
      app.drawing = null;
      const i = await run(api.addMask(ins.id, t));
      if (i != null) app.activeMask = { clip: ins.id, index: i };
    } else if (t === "freehand") {
      app.maskTool = null;
      if (drawing) {
        app.drawing = null;
        app.note = "";
      } else {
        app.activeMask = null;
        app.drawing = { clip: ins.id, points: [] };
        say("Click in the Viewer to place the mask's points; click the first point or press Enter to close it (Esc cancels)");
      }
    } else {
      app.drawing = null;
      app.maskTool = app.maskTool === t ? null : t;
      if (app.maskTool) say(t === "move" ? "Move mask: drag a mask in the Viewer (Esc leaves the tool)" : "Resize mask: drag a mask in the Viewer (Esc leaves the tool)");
    }
  }

  // The name popup: shown after a short pause, next to the tool.
  let tip = $state<{ name: string; hint: string; y: number } | null>(null);
  let timer: ReturnType<typeof setTimeout> | undefined;
  function enter(e: PointerEvent | FocusEvent, t: { id: string; name: string; hint: string }) {
    const el = e.currentTarget as HTMLElement;
    clearTimeout(timer);
    const show = () => {
      const r = el.getBoundingClientRect();
      const parent = el.closest(".tools")!.getBoundingClientRect();
      const extra = t.id === "orbit" || t.id === "pan" || t.id === "dolly" ? "" : why(t.id as Tool);
      tip = { name: t.name, hint: extra || t.hint, y: r.top - parent.top + r.height / 2 };
    };
    if (tip) show();
    else timer = setTimeout(show, e.type === "focus" ? 0 : 350);
  }
  function leave() {
    clearTimeout(timer);
    tip = null;
  }
</script>

<div class="tools" role="toolbar" tabindex="-1" aria-label="Mask tools" aria-orientation="vertical" onpointerdown={(e) => e.stopPropagation()}>
  {#each tools as t, i (t.id)}
    {#if i === 3}<span class="sep" aria-hidden="true"></span>{/if}
    <button
      class="tool"
      class:on={on(t.id)}
      aria-label={t.name}
      aria-pressed={t.id === "move" || t.id === "resize" || t.id === "freehand" ? on(t.id) : undefined}
      aria-disabled={!enabled(t.id)}
      onclick={() => use(t.id)}
      onpointerenter={(e) => enter(e, t)}
      onpointerleave={leave}
      onfocus={(e) => enter(e, t)}
      onblur={leave}
    >
      <svg viewBox="0 0 20 20" width="18" height="18" aria-hidden="true">
        {#if t.id === "rectangle"}
          <rect x="3.5" y="5" width="13" height="10" rx="0.5" />
          <rect x="2" y="3.5" width="3" height="3" class="dot" />
          <rect x="15" y="13.5" width="3" height="3" class="dot" />
        {:else if t.id === "ellipse"}
          <ellipse cx="10" cy="10" rx="7" ry="5" />
        {:else if t.id === "freehand"}
          <path d="M4 14.5 6.5 4.5l5 3 4.5-2-1.5 9.5-5 1z" stroke-linejoin="round" />
          <circle cx="4" cy="14.5" r="1.6" class="dot" />
        {:else if t.id === "move"}
          <path d="M10 2.5v15M2.5 10h15" />
          <path d="m10 2.5-2.2 2.4M10 2.5l2.2 2.4M10 17.5l-2.2-2.4M10 17.5l2.2-2.4M2.5 10l2.4-2.2M2.5 10l2.4 2.2M17.5 10l-2.4-2.2M17.5 10l-2.4 2.2" stroke-linecap="round" />
        {:else}
          <rect x="3" y="7" width="10" height="10" stroke-dasharray="2 1.6" />
          <path d="M9.5 10.5 17 3M12.5 3H17v4.5" stroke-linecap="round" stroke-linejoin="round" />
        {/if}
      </svg>
    </button>
  {/each}
  <span class="sep" aria-hidden="true"></span>
  {#each camTools as t (t.id)}
    <button
      class="tool cam"
      class:on={app.camTool === t.id}
      aria-label={t.name}
      aria-pressed={app.camTool === t.id}
      onclick={() => useCam(t.id)}
      onpointerenter={(e) => enter(e, t)}
      onpointerleave={leave}
      onfocus={(e) => enter(e, t)}
      onblur={leave}
    >
      <svg viewBox="0 0 20 20" width="18" height="18" aria-hidden="true">
        {#if t.id === "orbit"}
          <ellipse cx="10" cy="10" rx="7.5" ry="3.2" />
          <circle cx="10" cy="10" r="1.6" class="dot" />
          <path d="M15.2 4.2 17.5 6.7l-3.2.6" stroke-linecap="round" stroke-linejoin="round" />
        {:else if t.id === "pan"}
          <rect x="4" y="6" width="9" height="8" rx="1" />
          <path d="M13 8.5 17 6.5v7l-4-2" stroke-linejoin="round" />
          <path d="M2 17.5h16M15.5 15.5l2.5 2-2.5 2" stroke-linecap="round" stroke-linejoin="round" />
        {:else}
          <rect x="4" y="6" width="9" height="8" rx="1" />
          <path d="M13 8.5 17 6.5v7l-4-2" stroke-linejoin="round" />
          <path d="M8.5 1.5v3M7 3l1.5-1.5L10 3M8.5 18.5v-3M7 17l1.5 1.5L10 17" stroke-linecap="round" stroke-linejoin="round" />
        {/if}
      </svg>
    </button>
  {/each}
  {#if tip}
    <div class="tip" role="tooltip" style="top:{tip.y}px">
      <strong>{tip.name}</strong>
      <span>{tip.hint}</span>
    </div>
  {/if}
</div>

<style>
  .tools {
    position: absolute;
    left: 8px;
    top: 8px;
    z-index: 4;
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: 4px;
    background: color-mix(in srgb, var(--base) 88%, transparent);
    border: 1px solid var(--line);
    border-radius: 7px;
    box-shadow: 0 4px 14px rgba(0, 0, 0, 0.35);
  }
  .tool {
    width: 32px;
    height: 32px;
    min-height: 32px;
    padding: 0;
    display: grid;
    place-items: center;
    background: none;
    border-color: transparent;
    color: var(--text);
  }
  .tool:hover {
    background: var(--hover);
  }
  .tool[aria-disabled="true"] {
    color: var(--faint);
    cursor: default;
  }
  .tool[aria-disabled="true"]:hover {
    background: none;
  }
  .tool.on {
    background: rgba(255, 110, 200, 0.18);
    border-color: #ff6ec8;
    color: #ff6ec8;
  }
  svg {
    fill: none;
    stroke: currentColor;
    stroke-width: 1.5;
  }
  svg :global(.dot) {
    fill: currentColor;
    stroke: none;
  }
  .sep {
    height: 1px;
    margin: 3px 2px;
    background: var(--line);
  }
  .tip {
    position: absolute;
    left: calc(100% + 8px);
    transform: translateY(-50%);
    width: max-content;
    max-width: 240px;
    padding: 5px 9px 6px;
    background: var(--raised);
    border: 1px solid var(--line);
    border-radius: 5px;
    box-shadow: 0 6px 18px rgba(0, 0, 0, 0.45);
    pointer-events: none;
    display: flex;
    flex-direction: column;
    gap: 1px;
  }
  .tip strong {
    font-weight: 600;
    font-size: calc(12.5px * var(--text-scale));
  }
  .tip span {
    color: var(--muted);
    font-size: calc(11.5px * var(--text-scale));
    line-height: 1.35;
  }
  .tip::before {
    content: "";
    position: absolute;
    left: -5px;
    top: 50%;
    width: 8px;
    height: 8px;
    background: var(--raised);
    border-left: 1px solid var(--line);
    border-bottom: 1px solid var(--line);
    transform: translateY(-50%) rotate(45deg);
  }
</style>
