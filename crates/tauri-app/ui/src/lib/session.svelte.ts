import { SvelteMap } from "svelte/reactivity";
// The editor as the interface sees it: the last full state, the live values
// polled every frame, and the loop that keeps both (and the picture) fresh.

import { api, type EffectKind, type Live, type State, type Ticks, type Transition } from "./api";

function loadPanels(): Record<string, boolean> {
  const all = { media: true, effects: true, history: true, viewer: true, controls: true, track: true, timeline: true, meters: true };
  try {
    return { ...all, ...JSON.parse(localStorage.getItem("panels") ?? "{}") };
  } catch {
    return all;
  }
}

export const app = $state({
  s: null as State | null,
  live: null as Live | null,
  /// Shown in the status bar when the UI itself has something to say.
  note: "",
  /// The timeline tab in front: the main edit or the composition timeline.
  timelineTab: "edit" as "edit" | "comp" | "curves",
  /// The curve editor's shown channels, per clip.
  curvesShown: {} as Record<number, string[]>,
  /// The Collect project window.
  collectOpen: false,
  pasteAttrsOpen: false,
  /// Bumped by Tabs > Reset layout.
  layoutReset: 0,
  /// A marker being named / recoloured (its index).
  markerEdit: null as number | null,
  tool: "select" as "select" | "ripple" | "roll" | "slip" | "slide",
  snapping: true,
  exportOpen: false,
  /// The export queue window (not modal: editing goes on while it renders).
  queueOpen: false,
  /// The effects browser and the Plugins window.
  effectsOpen: false,
  pluginsOpen: false,
  settingsOpen: false,
  /// Help → About & diagnostics, on one of its tabs (null: closed).
  aboutTab: null as null | "diagnostics" | "check" | "notices",
  /// Timeline > Duck music under dialogue… is open.
  duckOpen: false,
  /// Media being dragged from the Media panel (pointer position in the window).
  dragMedia: null as null | { id: number; name: string; x: number; y: number },
  /// An effect or transition being dragged from the Effects panel.
  dragEffect: null as null | { name: string; kind: EffectKind | null; plugin: string | null; transition: Transition | null; x: number; y: number },
  // Reactive, so clips redraw their waveform when it arrives.
  waveforms: new SvelteMap<number, Uint8Array>(),
  waveformRate: 100,
  /// Which windows are shown (the Tabs menu).
  panels: loadPanels(),
  /// The mask being edited in the Viewer (of the selected clip), by index.
  activeMask: null as null | { clip: number; index: number },
  /// The point of the edited mask last clicked in the Viewer (for its own feather).
  activeMaskPoint: null as null | number,
  /// Drawing a freehand mask: the clip and the points so far (layer pixels).
  drawing: null as null | { clip: number; points: [number, number][] },
  /// The Viewer's mask tool: dragging a mask moves it, or resizes it about its centre.
  maskTool: null as null | "move" | "resize",
});

export function setPanel(name: string, on: boolean) {
  app.panels[name] = on;
  try {
    localStorage.setItem("panels", JSON.stringify(app.panels));
  } catch {}
}

/// Where the viewer wants the picture drawn (set by the Viewer).
export const viewer = {
  canvas: null as HTMLCanvasElement | null,
  width: 640,
  height: 360,
  have: -1,
  wasPlaying: false,
  size: [0, 0] as [number, number],
};

let busy = false;
let lastRev = -1;
let lastPlugins = -1;
let lastProxies = -1;
let lastPlayhead = -1;
let lastPlayheadFetch = 0;
let waveCount = -1;

/// Run a command, then refresh at once (rather than waiting for the next frame).
export async function run<T>(p: Promise<T>): Promise<T | undefined> {
  try {
    const r = await p;
    await refresh(true);
    return r;
  } catch (e) {
    say(String(e));
    return undefined;
  }
}

let noteTimer: ReturnType<typeof setTimeout> | undefined;
/// Show a message from the interface in the status bar for a few seconds.
export function say(text: string) {
  app.note = text;
  clearTimeout(noteTimer);
  noteTimer = setTimeout(() => (app.note = ""), 6000);
}

/// `force`: fetch the full state even if the project didn't change (a new selection, say).
let lastPicture = -1;
export async function refresh(force = false) {
  const live = await api.tick();
  app.live = live;
  // Keyframed values in the Controls panel follow the playhead (a few times a
  // second while playing).
  const ins = app.s?.inspector;
  const keyed = !!ins && (ins.effects.some((f) => f.animated) || ins.masks.some((m) => m.mask.keys.length > 0) || !!ins.keys);
  const now = performance.now();
  const moved = keyed && live.playhead !== lastPlayhead && (!live.playing || now - lastPlayheadFetch > 250);
  // A title's outline is known once it has been drawn: fetch it then.
  const outline = !!ins?.visual && !ins.gizmo && !live.playing && live.picture_rev !== lastPicture;
  lastPicture = live.picture_rev;
  if (force || moved || outline || live.rev !== lastRev || live.plugins_rev !== lastPlugins || live.proxies !== lastProxies || !app.s) {
    lastProxies = live.proxies;
    lastRev = live.rev;
    lastPlugins = live.plugins_rev;
    lastPlayhead = live.playhead;
    lastPlayheadFetch = now;
    app.s = await api.state();
  }
  if (live.waveforms !== waveCount) {
    waveCount = live.waveforms;
    await loadWaveforms();
  }
}

async function loadWaveforms() {
  if (!app.s) return;
  for (const m of app.s.media) {
    if (m.has_audio && !app.waveforms.has(m.id)) {
      const buf = new Uint8Array(await api.waveform(m.id));
      if (buf.length) app.waveforms.set(m.id, buf);
    }
  }
}

async function drawFrame(playing: boolean) {
  const c = viewer.canvas;
  if (!c || !app.live) return;
  // After playback stops, fetch the crisp (uncompressed) picture again.
  if (viewer.wasPlaying && !playing) viewer.have = -1;
  viewer.wasPlaying = playing;
  if (app.live.picture_rev === viewer.have) return;
  const want = app.live.picture_rev;
  const buf = await api.frame(Math.round(viewer.width), Math.round(viewer.height), viewer.have, playing);
  const bytes = new Uint8Array(buf);
  if (bytes.length < 9 || bytes[0] === 0) {
    if (bytes[0] === 0 && bytes.length === 1 && viewer.have === -1) viewer.have = want;
    return;
  }
  const dv = new DataView(buf);
  const w = dv.getUint32(1, true);
  const h = dv.getUint32(5, true);
  if (c.width !== w || c.height !== h) {
    c.width = w;
    c.height = h;
  }
  viewer.size = [w, h];
  const ctx = c.getContext("2d");
  if (!ctx) return;
  if (bytes[0] === 1) {
    ctx.putImageData(new ImageData(new Uint8ClampedArray(buf, 9, w * h * 4), w, h), 0, 0);
  } else {
    const bmp = await createImageBitmap(new Blob([bytes.subarray(9)], { type: "image/jpeg" }));
    ctx.drawImage(bmp, 0, 0);
    bmp.close();
  }
  viewer.have = want;
}

/// The frame loop: playback, state and picture, once per display refresh while
/// anything is happening, and ten times a second when the editor is idle.
let idleTimer: ReturnType<typeof setTimeout> | undefined;
let scheduled = false;
let prev = { rev: -1, pic: -1 };

function schedule(loop: () => void, idle: boolean) {
  scheduled = true;
  if (idle) idleTimer = setTimeout(() => requestAnimationFrame(loop), 100);
  else requestAnimationFrame(loop);
}

export function start() {
  api.waveformRate().then((r) => (app.waveformRate = r));
  const loop = async () => {
    scheduled = false;
    idleTimer = undefined;
    if (!busy) {
      busy = true;
      try {
        await refresh();
        await drawFrame(app.live?.playing ?? false);
      } catch (e) {
        say(String(e));
      } finally {
        busy = false;
      }
    }
    const l = app.live;
    const quiet =
      !!l &&
      !l.playing &&
      !l.export.jobs.some((j) => j.state === "running") &&
      !l.tracking &&
      l.rev === prev.rev &&
      l.picture_rev === prev.pic &&
      l.picture_rev === viewer.have &&
      l.master.peak[0] <= -60 &&
      l.master.peak[1] <= -60 &&
      l.exact;
    if (l) prev = { rev: l.rev, pic: l.picture_rev };
    schedule(loop, quiet);
  };
  // Any input brings the loop back to full speed at once.
  const wake = () => {
    if (idleTimer !== undefined) {
      clearTimeout(idleTimer);
      idleTimer = undefined;
      requestAnimationFrame(loop);
    }
  };
  for (const ev of ["pointerdown", "pointermove", "keydown", "wheel"]) window.addEventListener(ev, wake, { passive: true, capture: true });
  requestAnimationFrame(loop);
  void scheduled;
}

// ---- Time ----

export function tps(): number {
  return app.s?.ticks_per_second ?? 705_600_000;
}

export function fps(): number {
  const q = app.s?.seq;
  return q ? q.rate_num / q.rate_den : 25;
}

/// HH:MM:SS:FF at the sequence's frame rate.
export function timecode(t: Ticks): string {
  const q = app.s?.seq;
  if (!q) return "00:00:00:00";
  const frame = Math.floor(t / q.frame + 1e-6);
  const base = Math.round(q.rate_num / q.rate_den);
  const ff = frame % base;
  const total = Math.floor(frame / base);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${p(Math.floor(total / 3600))}:${p(Math.floor(total / 60) % 60)}:${p(total % 60)}:${p(ff)}`;
}

/// "1:23.4"
export function shortTime(t: Ticks): string {
  const s = t / tps();
  return `${Math.floor(s / 60)}:${(s % 60).toFixed(1).padStart(4, "0")}`;
}

export function snapToFrame(t: Ticks): Ticks {
  const f = app.s?.seq.frame ?? 1;
  return Math.round(t / f) * f;
}
