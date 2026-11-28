// Things the menus, buttons and shortcuts do, in one place so they behave the same everywhere.

import { importModel } from "./modeldialog.svelte";
import { api } from "./api";
import { app, run, say } from "./session.svelte";
import { prefsDialog } from "./prefs.svelte";
import { view } from "./timeline.svelte";
import { allWorkspaces, snapshot, switchTo, workspaces } from "./workspace.svelte";
import { comboOf, commandFor } from "./shortcuts.svelte";

/// A question in a dialog: the answer is the label of the button pressed (or null).
/// With `field` set, the dialog also has a text box (its value is in `field`).
export const ask = $state({ open: false, title: "", text: "", buttons: [] as string[], field: null as string | null, prompt: null as string | null, never: false, resolve: (_: string | null) => {} });

/// The questions that can be turned off (Settings → Prompts), by key: what
/// each asks. Turned off, the first button is the answer without asking.
export const PROMPTS: [string, string][] = [
  ["delete_track", "Deleting a track with clips on it"],
  ["delete_preset", "Deleting an effect preset"],
  ["delete_template", "Deleting a project template"],
  ["clear_recent", "Clearing the recent projects list"],
  ["restore_version", "Restoring a saved version"],
  ["delete_version", "Deleting a saved version"],
  ["replace_workspace", "Replacing a workspace of the same name"],
  ["delete_workspace", "Deleting a workspace"],
];

/// Whether question `key` is still asked (Settings → Prompts).
export function asks(key: string): boolean {
  return app.s?.prefs.prompts?.[key] !== false;
}

/// Ask `title`; with a `prompt` key, the question can be turned off ("Don't
/// ask again" here, or Settings → Prompts), and then its first button is the answer.
export function question(title: string, text: string, buttons: string[], prompt: string | null = null): Promise<string | null> {
  if (prompt && !asks(prompt)) return Promise.resolve(buttons[0] ?? null);
  return new Promise((resolve) => {
    Object.assign(ask, {
      open: true,
      title,
      text,
      buttons,
      field: null,
      prompt,
      never: false,
      resolve: (v: string | null) => {
        ask.open = false;
        // "Don't ask again" counts once an answer is given (not on Cancel).
        if (prompt && ask.never && v === buttons[0] && app.s) run(api.setPrefs({ ...app.s.prefs, prompts: { ...app.s.prefs.prompts, [prompt]: false } }));
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
      prompt: null,
      never: false,
      resolve: (v: string | null) => {
        ask.open = false;
        resolve(v === ok ? ask.field : null);
      },
    });
  });
}

/// Save (or Save as): with Settings → save the screen layout with the
/// project, the layout goes in first.
export async function saveProject(saveAs: boolean): Promise<boolean | undefined> {
  if (app.s?.prefs.layout_in_project) await run(api.setProjectLayout(snapshot(workspaces.current)));
  return run(api.save(saveAs));
}

/// Before throwing the project away: offer to save it. True when it's fine to go on.
export async function checkUnsaved(): Promise<boolean> {
  if (!app.live?.dirty) return true;
  const a = await question("Save changes?", `“${app.s?.title ?? "Untitled"}” has changes that haven't been saved.`, ["Save", "Don't save", "Cancel"]);
  if (a === "Save") return (await saveProject(false)) === true;
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
  save: () => saveProject(false),
  saveAs: () => saveProject(true),
  importMedia: () => run(api.importDialog()),
  undo: () => run(api.action("undo")),
  redo: () => run(api.action("redo")),
  act: (name: string) => run(api.action(name)),
  addTitle: () => run(api.addTitle()),
  add3dText: () => run(api.add3dText()),
  add3dLogo: () => run(api.add3dLogo()),
  add3dModel: () => importModel(),
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
  const a = await question(`Delete ${tr.name}?`, n ? `The track and its ${n} clip${n === 1 ? "" : "s"} go. Undo brings them back.` : "The track is empty.", ["Delete track", "Cancel"], "delete_track");
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
  const go = (f: () => unknown) => {
    e.preventDefault();
    f();
  };
  // Alt+Shift+1…9: switch workspace.
  if (e.altKey && e.shiftKey && /^Digit[1-9]$/.test(e.code)) {
    const w = allWorkspaces()[Number(e.code.slice(5)) - 1];
    if (w) return go(() => (switchTo(w.name), say(`${w.name} workspace`)));
    return;
  }
  // 1…9 (Shift: picture only): cut the multicam clip at the playhead to that angle.
  if (!e.altKey && !e.ctrlKey && !e.metaKey && /^Digit[1-9]$/.test(e.code) && app.live?.multicam) {
    const angle = Number(e.code.slice(5)) - 1;
    return go(() => run(api.switchAngle(angle, e.shiftKey)));
  }
  // Everything else by the keys set for it (Settings → Keyboard shortcuts…).
  const id = commandFor(comboOf(e));
  const f = id ? COMMANDS[id] : undefined;
  if (f) go(f);
}

/// What each shortcut's command does.
export const COMMANDS: Record<string, () => unknown> = {
  new_project: () => actions.newProject(),
  start_screen: () => (app.startOpen = true),
  open: () => actions.open(),
  save: () => actions.save(),
  save_as: () => actions.saveAs(),
  save_version: () => (app.versionsOpen = true),
  import: () => actions.importMedia(),
  export: () => actions.exportOpen(),
  snapshot: () => run(api.snapshot(false)),
  undo: () => actions.undo(),
  redo: () => actions.redo(),
  copy: () => actions.act("copy"),
  cut: () => actions.act("cut"),
  paste: () => actions.act("paste"),
  paste_attributes: () => (app.pasteAttrsOpen = true),
  select_all: () => actions.act("select_all"),
  deselect: () => {
    if (app.live?.render == null && app.s?.seq.target_track != null) run(api.trackOp(0, "untarget"));
    return actions.act(app.live?.render != null ? "stop_render_preview" : "deselect");
  },
  delete: () => deleteKey(false),
  ripple_delete: () => deleteKey(true),
  split: () => actions.act("split"),
  make_composite: () => actions.act("make_composite"),
  add_title: () => actions.addTitle(),
  dissolve: () => actions.act("dissolve"),
  crossfade: () => actions.act("crossfade"),
  mark_in: () => actions.act("mark_in"),
  mark_out: () => actions.act("mark_out"),
  add_marker: () => actions.act("add_marker"),
  freeze_frame: () => actions.act("freeze_frame"),
  nudge_left: () => actions.act("nudge_left"),
  nudge_right: () => actions.act("nudge_right"),
  nudge_left_10: () => actions.act("nudge_left_10"),
  nudge_right_10: () => actions.act("nudge_right_10"),
  highlight_up: () => stepHighlight(-1),
  highlight_down: () => stepHighlight(1),
  fit_timeline: () => (view.fit = true),
  tool_select: () => (app.tool = "select"),
  tool_ripple: () => (app.tool = "ripple"),
  tool_roll: () => (app.tool = "roll"),
  tool_slip: () => (app.tool = "slip"),
  tool_slide: () => (app.tool = "slide"),
  effects: () => (app.effectsOpen = true),
  play: () => actions.act("play"),
  prev_frame: () => actions.act("prev_frame"),
  next_frame: () => actions.act("next_frame"),
  prev_edit: () => actions.act("prev_edit"),
  next_edit: () => actions.act("next_edit"),
  prev_marker: () => actions.act("prev_marker"),
  next_marker: () => actions.act("next_marker"),
  start: () => actions.act("start"),
  end: () => actions.act("end"),
  shuttle_back: () => actions.act("shuttle_back"),
  shuttle_stop: () => actions.act("shuttle_stop"),
  shuttle_forward: () => actions.act("shuttle_forward"),
  render_preview: () => app.live?.render == null && actions.act("render_preview"),
  full_screen: () => toggleFullScreen(),
};

/// The Viewer full screen (the whole display), or back.
export function toggleFullScreen() {
  if (document.fullscreenElement) {
    document.exitFullscreen().catch(() => {});
    return;
  }
  const stage = document.querySelector(".viewer .stage") as HTMLElement | null;
  stage?.requestFullscreen().catch(() => say("Full screen isn't available here"));
}
