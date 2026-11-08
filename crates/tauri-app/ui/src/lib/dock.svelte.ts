// Docking: the tabbed panels (Media, Effects, History, Controls, Text, Track) live in the
// left or right dock, in any order, or float as windows of their own. Tabs
// are dragged between docks, or out into a floating window, and back. The
// arrangement is remembered between sessions.

export type PanelId = "media" | "effects" | "history" | "controls" | "text" | "track";
export type DockSide = "left" | "right";

export const PANEL_LABEL: Record<PanelId, string> = {
  media: "Media",
  effects: "Effects",
  history: "History",
  controls: "Controls",
  text: "Text",
  track: "Track",
};

export interface FloatRect {
  x: number;
  y: number;
  w: number;
  h: number;
}

interface Layout {
  left: PanelId[];
  right: PanelId[];
  float: PanelId[];
  active: Record<DockSide, PanelId | null>;
  rects: Partial<Record<PanelId, FloatRect>>;
}

const ALL: PanelId[] = ["media", "effects", "history", "controls", "text", "track"];

function initial(): Layout {
  return { left: ["media", "effects", "history"], right: ["controls", "text", "track"], float: [], active: { left: "media", right: "controls" }, rects: {} };
}

function load(): Layout {
  try {
    const v = JSON.parse(localStorage.getItem("dock") ?? "null") as Layout | null;
    if (!v || !Array.isArray(v.left) || !Array.isArray(v.right) || !Array.isArray(v.float)) return initial();
    // Every panel exactly once (panels added in a newer version go to the left dock).
    const seen = new Set<PanelId>();
    const keep = (l: PanelId[]) => l.filter((p) => ALL.includes(p) && !seen.has(p) && (seen.add(p), true));
    const out: Layout = { left: keep(v.left), right: keep(v.right), float: keep(v.float), active: v.active ?? { left: null, right: null }, rects: v.rects ?? {} };
    for (const p of ALL) {
      if (seen.has(p)) continue;
      if (p === "track" || p === "text") out.right.push(p);
      else out.left.splice(p === "effects" ? Math.min(1, out.left.length) : out.left.length, 0, p);
    }
    return out;
  } catch {
    return initial();
  }
}

export const dock = $state(load());

/// A tab being dragged (pointer position in the window), for drop indicators.
export const tabDrag = $state({ id: null as PanelId | null, x: 0, y: 0, over: null as null | { side: DockSide; index: number } });

export function saveDock() {
  try {
    localStorage.setItem("dock", JSON.stringify(dock));
  } catch {}
}

export function resetDock() {
  Object.assign(dock, initial());
  saveDock();
}

function remove(id: PanelId) {
  for (const side of ["left", "right"] as DockSide[]) {
    const i = dock[side].indexOf(id);
    if (i >= 0) {
      dock[side].splice(i, 1);
      if (dock.active[side] === id) dock.active[side] = dock[side][Math.max(0, i - 1)] ?? null;
    }
  }
  const f = dock.float.indexOf(id);
  if (f >= 0) dock.float.splice(f, 1);
}

/// Put a panel into a dock (at `index`, else at the end) and bring it to the front there.
export function dockPanel(id: PanelId, side: DockSide, index?: number) {
  const from = dock[side].indexOf(id);
  remove(id);
  const list = dock[side];
  // Moving within the same dock: the gap it left shifts later places by one.
  let at = index ?? list.length;
  if (from >= 0 && index != null && index > from) at -= 1;
  list.splice(Math.max(0, Math.min(list.length, at)), 0, id);
  dock.active[side] = id;
  saveDock();
}

/// Take a panel out into its own floating window, placed around `at` (window pixels).
export function floatPanel(id: PanelId, at?: { x: number; y: number }) {
  remove(id);
  const old = dock.rects[id];
  const w = old?.w ?? 320;
  const h = old?.h ?? 460;
  const x = at ? at.x - 60 : (old?.x ?? 120);
  const y = at ? at.y - 14 : (old?.y ?? 90);
  dock.rects[id] = clampRect({ x, y, w, h });
  dock.float.push(id);
  saveDock();
}

/// Bring a floating panel to the front.
export function raise(id: PanelId) {
  const i = dock.float.indexOf(id);
  if (i >= 0 && i !== dock.float.length - 1) {
    dock.float.splice(i, 1);
    dock.float.push(id);
  }
}

/// Keep a floating window reachable: at least its title bar inside the app window.
export function clampRect(r: FloatRect): FloatRect {
  const W = typeof window !== "undefined" ? window.innerWidth : 1600;
  const H = typeof window !== "undefined" ? window.innerHeight : 1000;
  const w = Math.max(220, Math.min(r.w, W - 20));
  const h = Math.max(160, Math.min(r.h, H - 40));
  return { x: Math.max(-w + 120, Math.min(r.x, W - 120)), y: Math.max(28, Math.min(r.y, H - 40)), w, h };
}

/// Where a panel is.
export function placeOf(id: PanelId): DockSide | "float" {
  if (dock.left.includes(id)) return "left";
  if (dock.right.includes(id)) return "right";
  return "float";
}

/// Show a panel (e.g. from a menu): in front in its dock, or raised if floating.
export function showPanel(id: PanelId) {
  const p = placeOf(id);
  if (p === "float") raise(id);
  else dock.active[p] = id;
}

/// The drop target under the pointer, if any: a dock and the tab position there.
export function dropTarget(x: number, y: number): { side: DockSide; index: number } | null {
  const el = document.elementFromPoint(x, y)?.closest("[data-dock]") as HTMLElement | null;
  if (!el) return null;
  const side = el.dataset.dock as DockSide;
  const tabs = Array.from(el.querySelectorAll<HTMLElement>("[data-tab]"));
  let index = tabs.length;
  for (let i = 0; i < tabs.length; i++) {
    const r = tabs[i].getBoundingClientRect();
    if (x < r.left + r.width / 2) {
      index = i;
      break;
    }
  }
  return { side, index };
}
