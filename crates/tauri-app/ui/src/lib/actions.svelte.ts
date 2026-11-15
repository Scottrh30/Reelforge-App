// Things the menus, buttons and shortcuts do, in one place so they behave the same everywhere.

import { api } from "./api";
import { app, run, say } from "./session.svelte";
import { prefsDialog } from "./prefs.svelte";
import { view } from "./timeline.svelte";
import { allWorkspaces, switchTo } from "./workspace.svelte";

/// A question in a dialog: the answer is the label of the button pressed (or null).
/// With `field` set, the dialog also has a text box (its value is in `field`).
export const ask = $state({ open: false, title: "", text: "", buttons: [] as string[], field: null as string | null, resolve: (_: string | null) => {} });

export function question(title: string, text: string, buttons: string[]): Promise<string | null> {
  return new Promise((resolve) => {
    Object.assign(ask, {
      open: true,
      title,
      text,
      buttons,
      field: null,
      resolve: (v: string | null) => {
        ask.open = false;
        resolve(v);
      },
    });
  });
}

/// Ask for a line of text (`ok` confirms, Cancel or Esc gives null).
export function askText(title: string, text: string, initial: string, ok: string): Promise<string | null> {
  return new Promise((resolve) => {
    Object.assign(ask, {
      open: true,
      title,
      text,
      buttons: [ok, "Cancel"],
      field: initial,
      resolve: (v: string | null) => {
        ask.open = false;
        resolve(v === ok ? ask.field : null);
      },
    });
  });
}

/// Before throwing the project away: offer to save it. True when it's fine to go on.
export async function checkUnsaved(): Promise<boolean> {
  if (!app.live?.dirty) return true;
  const a = await question("Save changes?", `“${app.s?.title ?? "Untitled"}” has changes that haven't been saved.`, ["Save", "Don't save", "Cancel"]);
  if (a === "Save") return (await run(api.save(false))) === true;
  return a === "Don't save";
}

export const actions = {
  async newProject() {
    if (await checkUnsaved()) await run(api.newProject());
  },
  async open() {
    if (await checkUnsaved()) await run(api.openDialog());
  },
  async openAdobe() {
    if (await checkUnsaved()) await run(api.openAdobeDialog());
  },
  async openAutosave() {
    if (await checkUnsaved()) await run(api.openAutosaveDialog());
  },
  save: () => run(api.save(false)),
  saveAs: () => run(api.save(true)),
  importMedia: () => run(api.importDialog()),
  undo: () => run(api.action("undo")),
  redo: () => run(api.action("redo")),
  act: (name: string) => run(api.action(name)),
  addTitle: () => run(api.addTitle()),
  add3dText: () => run(api.add3dText()),
  add3dLogo: () => run(api.add3dLogo()),
  add3dModel: () => run(api.add3dModel()),
  exportOpen() {
    app.exportOpen = true;
  },
};

/// Close the freehand mask being drawn and add it.
export async function finishDrawing() {
  const d = app.drawing;
  app.drawing = null;
  if (!d) return;
  if (d.points.length < 3) {
    say("A freehand mask needs at least 3 points");
    return;
  }
  app.note = "";
  const i = await run(api.addFreehandMask(d.clip, d.points));
  if (i != null) app.activeMask = { clip: d.clip, index: i };
}

/// Move the track highlight one row up or down the timeline (as shown: video
/// tracks top first, then audio); with none highlighted, start at the top.
function stepHighlight(by: number) {
  const t = app.s?.seq.tracks ?? [];
  const rows = [...t.filter((x) => x.kind === "video").reverse(), ...t.filter((x) => x.kind === "audio")].map((x) => x.index);
  if (!rows.length) return;
  const now = app.s?.seq.target_track;
  const at = now == null ? -1 : rows.indexOf(now);
  const next = at < 0 ? rows[0] : rows[Math.max(0, Math.min(rows.length - 1, at + by))];
  run(api.trackOp(next, "target"));
}

/// Delete: the selected clips; with none selected, the highlighted track (after asking).
async function deleteKey(ripple: boolean) {
  const target = app.s?.seq.target_track;
  if ((app.s?.selected.length ?? 0) > 0 || target == null) return actions.act(ripple ? "ripple_delete" : "delete");
  const tr = app.s?.seq.tracks.find((x) => x.index === target);
  if (!tr) return;
  const n = tr.clips.length;
  const a = await question(`Delete ${tr.name}?`, n ? `The track and its ${n} clip${n === 1 ? "" : "s"} go. Undo brings them back.` : "The track is empty.", ["Delete track", "Cancel"]);
  if (a === "Delete track") run(api.trackOp(target, "delete"));
}

/// Keyboard shortcuts (not while typing in a field).
export function onKey(e: KeyboardEvent) {
  const t = e.target as HTMLElement;
  if (t && (t.tagName === "INPUT" || t.tagName === "TEXTAREA" || t.tagName === "SELECT" || t.isContentEditable)) return;
  if (app.keyPick && e.key === "Escape") {
    e.preventDefault();
    app.keyPick = null;
    app.note = "";
    return;
  }
  if (app.camTool && e.key === "Escape") {
    e.preventDefault();
    app.camTool = null;
    return;
  }
  if (app.maskTool && e.key === "Escape" && !app.drawing) {
    e.preventDefault();
    app.maskTool = null;
    return;
  }
  if (app.drawing && (e.key === "Enter" || e.key === "Escape")) {
    e.preventDefault();
    if (e.key === "Enter") finishDrawing();
    else {
      app.drawing = null;
      app.note = "";
    }
    return;
  }
  if (ask.open || app.startOpen || app.exportOpen || app.effectsOpen || app.savePreset || app.pluginsOpen || app.collectOpen || app.pasteAttrsOpen || app.versionsOpen || app.multicamOpen || app.markerEdit != null || app.settingsOpen || prefsDialog.open || (app.s?.recoveries.length ?? 0) > 0) return;
  const ctrl = e.ctrlKey || e.metaKey;
  const k = e.key.toLowerCase();
  const go = (f: () => unknown) => {
    e.preventDefault();
    f();
  };
  if (ctrl) {
    if (k === "z" && !e.shiftKey) return go(actions.undo);
    if (k === "y" || (k === "z" && e.shiftKey)) return go(actions.redo);
    if (k === "s" && e.altKey) return go(() => (app.versionsOpen = true));
    if (k === "s" && e.shiftKey) return go(actions.saveAs);
    if (k === "s") return go(actions.save);
    if (k === "o") return go(actions.open);
    if (k === "n" && e.shiftKey) return go(() => (app.startOpen = true));
    if (k === "n") return go(actions.newProject);
    if (k === "i") return go(actions.importMedia);
    if (k === "m") return go(actions.exportOpen);
    if (k === "c") return go(() => actions.act("copy"));
    if (k === "x") return go(() => actions.act("cut"));
    if (k === "v" && e.altKey) return go(() => (app.pasteAttrsOpen = true));
    if (k === "v") return go(() => actions.act("paste"));
    if (k === "g") return go(() => actions.act("make_composite"));
    if (k === "a") return go(() => actions.act("select_all"));
    if (k === "t") return go(actions.addTitle);
    if (k === "d") return go(() => actions.act(e.shiftKey ? "crossfade" : "dissolve"));
    if (k === "e" && e.shiftKey) return go(() => run(api.snapshot(false)));
    return;
  }
  // Alt+Shift+1…9: switch workspace.
  if (e.altKey && e.shiftKey && /^Digit[1-9]$/.test(e.code)) {
    const w = allWorkspaces()[Number(e.code.slice(5)) - 1];
    if (w) return go(() => (switchTo(w.name), say(`${w.name} workspace`)));
    return;
  }
  // 1…9 (Shift: picture only): cut the multicam clip at the playhead to that angle.
  if (!e.altKey && /^Digit[1-9]$/.test(e.code) && app.live?.multicam) {
    const angle = Number(e.code.slice(5)) - 1;
    return go(() => run(api.switchAngle(angle, e.shiftKey)));
  }
  switch (e.key) {
    case " ":
      return go(() => actions.act("play"));
    case "ArrowLeft":
      return go(() => actions.act(e.altKey ? "nudge_left" : "prev_frame"));
    case "ArrowRight":
      return go(() => actions.act(e.altKey ? "nudge_right" : "next_frame"));
    case "ArrowUp":
      if (e.altKey) return go(() => stepHighlight(-1));
      return go(() => actions.act(e.shiftKey ? "prev_marker" : "prev_edit"));
    case "ArrowDown":
      if (e.altKey) return go(() => stepHighlight(1));
      return go(() => actions.act(e.shiftKey ? "next_marker" : "next_edit"));
    case "Home":
      return go(() => actions.act("start"));
    case "End":
      return go(() => actions.act("end"));
    case "Delete":
      return go(() => deleteKey(e.shiftKey));
    case "Backspace":
      return go(() => deleteKey(false));
    case "Escape":
      if (app.live?.render == null && app.s?.seq.target_track != null) run(api.trackOp(0, "untarget"));
      return go(() => actions.act(app.live?.render != null ? "stop_render_preview" : "deselect"));
    case "Enter":
      if (app.live?.render == null) return go(() => actions.act("render_preview"));
      return;
    case ",":
    case "<":
      return go(() => actions.act(e.shiftKey ? "nudge_left_10" : "nudge_left"));
    case ".":
    case ">":
      return go(() => actions.act(e.shiftKey ? "nudge_right_10" : "nudge_right"));
    case "\\":
      return go(() => (view.fit = true));
  }
  switch (k) {
    case "s":
      return go(() => actions.act("split"));
    case "i":
      return go(() => actions.act("mark_in"));
    case "o":
      return go(() => actions.act("mark_out"));
    case "v":
      return go(() => (app.tool = "select"));
    case "b":
      return go(() => (app.tool = "ripple"));
    case "n":
      return go(() => (app.tool = "roll"));
    case "y":
      return go(() => (app.tool = "slip"));
    case "u":
      return go(() => (app.tool = "slide"));
    case "e":
      return go(() => (app.effectsOpen = true));
    case "m":
      return go(() => actions.act("add_marker"));
    case "j":
      return go(() => actions.act("shuttle_back"));
    case "k":
      return go(() => actions.act("shuttle_stop"));
    case "l":
      return go(() => actions.act("shuttle_forward"));
    case "f":
      if (e.shiftKey) return go(() => actions.act("freeze_frame"));
      return;
  }
}
