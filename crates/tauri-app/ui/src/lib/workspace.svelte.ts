// Workspaces: named arrangements of the interface (which panels are shown,
// where each tab is docked or floats, and how big the docks, timeline and
// meters are), to switch between in one click. Four come built in (Editing,
// Effects, Tracking, Colour); you can change those, save your own, rename and
// delete them. Kept in this computer's local storage, like the layout itself.

import { app } from "./session.svelte";
import { dock, saveDock, type PanelId, type FloatRect } from "./dock.svelte";
import { view } from "./timeline.svelte";

/// The sizes of the areas around the Viewer, in pixels.
export interface Sizes {
  leftW: number;
  rightW: number;
  bottomH: number;
  metersW: number;
}

export interface Workspace {
  name: string;
  dock: {
    left: PanelId[];
    right: PanelId[];
    float: PanelId[];
    active: { left: PanelId | null; right: PanelId | null };
    rects: Partial<Record<PanelId, FloatRect>>;
  };
  sizes: Sizes;
  /// Panels switched on (Tabs menu), by name.
  panels: Record<string, boolean>;
  /// Width of the timeline's track headers.
  headerW: number;
}

const DEFAULT_SIZES: Sizes = { leftW: 300, rightW: 320, bottomH: 340, metersW: 64 };
const ALL_ON = { media: true, effects: true, history: true, viewer: true, controls: true, text: true, track: true, scopes: true, timeline: true, meters: true };

/// The built-in workspaces as they come.
export const BUILT_IN: Workspace[] = [
  {
    name: "Editing",
    dock: { left: ["media", "effects", "history"], right: ["controls", "text", "track", "scopes"], float: [], active: { left: "media", right: "controls" }, rects: {} },
    sizes: { ...DEFAULT_SIZES },
    panels: { ...ALL_ON },
    headerW: 140,
  },
  {
    // The effects library beside the clip's settings, and a bigger Viewer.
    name: "Effects",
    dock: { left: ["effects", "media", "history"], right: ["controls", "text", "track", "scopes"], float: [], active: { left: "effects", right: "controls" }, rects: {} },
    sizes: { leftW: 280, rightW: 380, bottomH: 260, metersW: 64 },
    panels: { ...ALL_ON },
    headerW: 140,
  },
  {
    // The Track panel in front with Controls beside it, a big Viewer, and a short timeline.
    name: "Tracking",
    dock: { left: ["media", "effects", "history"], right: ["track", "controls", "text", "scopes"], float: [], active: { left: "media", right: "track" }, rects: {} },
    sizes: { leftW: 240, rightW: 360, bottomH: 220, metersW: 64 },
    panels: { ...ALL_ON, history: false, meters: false },
    headerW: 120,
  },
  {
    // The scopes on the left, Controls (colour wheels, curves) wide on the right.
    name: "Colour",
    dock: { left: ["scopes", "media", "effects", "history"], right: ["controls", "text", "track"], float: [], active: { left: "scopes", right: "controls" }, rects: {} },
    sizes: { leftW: 340, rightW: 420, bottomH: 240, metersW: 64 },
    panels: { ...ALL_ON, media: false, effects: false, history: false, meters: false },
    headerW: 120,
  },
];

const clone = <T>(v: T): T => JSON.parse(JSON.stringify(v));

function stored(k: string, d: number) {
  try {
    const v = Number(localStorage.getItem(k));
    return v > 0 ? v : d;
  } catch {
    return d;
  }
}

/// The current sizes (the App binds its splitters to these).
export const sizes = $state<Sizes>({
  leftW: stored("leftW", DEFAULT_SIZES.leftW),
  rightW: stored("rightW", DEFAULT_SIZES.rightW),
  bottomH: stored("bottomH", DEFAULT_SIZES.bottomH),
  metersW: stored("metersW", DEFAULT_SIZES.metersW),
});

function saveSizes() {
  try {
    for (const [k, v] of Object.entries(sizes)) localStorage.setItem(k, String(Math.round(v)));
    localStorage.setItem("timelineHeader", String(Math.round(view.headerW)));
  } catch {}
}

function loadSaved(): Workspace[] {
  try {
    const v = JSON.parse(localStorage.getItem("workspaces") ?? "[]");
    return Array.isArray(v) ? v.filter((w) => w && typeof w.name === "string" && w.dock && w.sizes) : [];
  } catch {
    return [];
  }
}

/// Saved workspaces: your own, and built-in ones you've changed (same name).
/// `current` is the one last switched to or saved.
export const workspaces = $state({
  saved: loadSaved(),
  current: (() => {
    try {
      return localStorage.getItem("workspace") ?? "Editing";
    } catch {
      return "Editing";
    }
  })(),
});

function persist() {
  try {
    localStorage.setItem("workspaces", JSON.stringify(workspaces.saved));
    localStorage.setItem("workspace", workspaces.current);
  } catch {}
}

export const isBuiltIn = (name: string) => BUILT_IN.some((b) => b.name === name);
/// A built-in workspace that has been changed and saved.
export const isChanged = (name: string) => isBuiltIn(name) && workspaces.saved.some((w) => w.name === name);

/// Every workspace in menu order: the built-in ones (as changed), then yours.
export function allWorkspaces(): Workspace[] {
  const mine = (n: string) => workspaces.saved.find((w) => w.name === n);
  return [...BUILT_IN.map((b) => mine(b.name) ?? b), ...workspaces.saved.filter((w) => !isBuiltIn(w.name))];
}

export function find(name: string): Workspace | undefined {
  return allWorkspaces().find((w) => w.name === name);
}

/// The interface as it is now.
export function snapshot(name: string): Workspace {
  return {
    name,
    dock: clone({ left: dock.left, right: dock.right, float: dock.float, active: dock.active, rects: dock.rects }),
    sizes: { ...sizes },
    panels: { ...app.panels },
    headerW: view.headerW,
  };
}

/// Has the interface been changed since the current workspace was chosen or saved?
export function modified(): boolean {
  const w = find(workspaces.current);
  if (!w) return false;
  const a = snapshot(w.name);
  const strip = (x: Workspace) => JSON.stringify({ ...x, dock: { ...x.dock, rects: {} }, sizes: Object.fromEntries(Object.entries(x.sizes).map(([k, v]) => [k, Math.round(v)])), headerW: Math.round(x.headerW) });
  return strip(a) !== strip({ ...w, panels: { ...ALL_ON, ...w.panels } });
}

/// Arrange the interface as workspace `w`.
export function applyWorkspace(w: Workspace) {
  const d = clone(w.dock);
  // Every panel exactly once, even from a workspace saved before a panel existed.
  const all: PanelId[] = ["media", "effects", "history", "controls", "text", "track", "scopes"];
  const seen = new Set<PanelId>();
  const keep = (l: PanelId[]) => (l ?? []).filter((p) => all.includes(p) && !seen.has(p) && (seen.add(p), true));
  dock.left = keep(d.left);
  dock.right = keep(d.right);
  dock.float = keep(d.float);
  for (const p of all) if (!seen.has(p)) dock.right.push(p);
  dock.active = { left: d.active?.left ?? dock.left[0] ?? null, right: d.active?.right ?? dock.right[0] ?? null };
  dock.rects = d.rects ?? {};
  saveDock();
  Object.assign(sizes, { ...DEFAULT_SIZES, ...w.sizes });
  view.headerW = w.headerW || 140;
  saveSizes();
  const panels = { ...ALL_ON, ...w.panels };
  for (const [k, v] of Object.entries(panels)) app.panels[k] = v;
  try {
    localStorage.setItem("panels", JSON.stringify(app.panels));
  } catch {}
  workspaces.current = w.name;
  persist();
}

export function switchTo(name: string) {
  const w = find(name);
  if (w) applyWorkspace(w);
}

/// Save the interface as it is now under `name` (replacing a workspace of that name).
export function saveAs(name: string) {
  const w = snapshot(name);
  const i = workspaces.saved.findIndex((x) => x.name === name);
  if (i >= 0) workspaces.saved[i] = w;
  else workspaces.saved.push(w);
  workspaces.current = name;
  persist();
}

/// Delete one of your workspaces, or put a changed built-in one back as it came.
export function remove(name: string) {
  workspaces.saved = workspaces.saved.filter((w) => w.name !== name);
  if (isBuiltIn(name)) {
    if (workspaces.current === name) switchTo(name);
  } else if (workspaces.current === name) {
    workspaces.current = "Editing";
  }
  persist();
}

export function rename(from: string, to: string) {
  const w = workspaces.saved.find((x) => x.name === from);
  if (!w || isBuiltIn(from)) return;
  workspaces.saved = workspaces.saved.filter((x) => x.name !== to);
  w.name = to;
  if (workspaces.current === from) workspaces.current = to;
  persist();
}

/// Tabs > Reset layout: the Editing workspace as it comes.
export function resetLayout() {
  applyWorkspace(BUILT_IN[0]);
}

/// Remember the sizes after a splitter drag.
export const rememberSizes = saveSizes;
