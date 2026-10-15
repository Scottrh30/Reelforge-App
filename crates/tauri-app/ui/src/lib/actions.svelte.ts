// Things the menus, buttons and shortcuts do, in one place so they behave the same everywhere.

import { api } from "./api";
import { app, run } from "./session.svelte";

/// A question in a dialog: the answer is the label of the button pressed (or null).
export const ask = $state({ open: false, title: "", text: "", buttons: [] as string[], resolve: (_: string | null) => {} });

export function question(title: string, text: string, buttons: string[]): Promise<string | null> {
  return new Promise((resolve) => {
    Object.assign(ask, {
      open: true,
      title,
      text,
      buttons,
      resolve: (v: string | null) => {
        ask.open = false;
        resolve(v);
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
  save: () => run(api.save(false)),
  saveAs: () => run(api.save(true)),
  importMedia: () => run(api.importDialog()),
  undo: () => run(api.action("undo")),
  redo: () => run(api.action("redo")),
  act: (name: string) => run(api.action(name)),
  addTitle: () => run(api.addTitle()),
  exportOpen() {
    app.exportOpen = true;
  },
};

/// Keyboard shortcuts (not while typing in a field).
export function onKey(e: KeyboardEvent) {
  const t = e.target as HTMLElement;
  if (t && (t.tagName === "INPUT" || t.tagName === "TEXTAREA" || t.tagName === "SELECT" || t.isContentEditable)) return;
  if (ask.open || app.exportOpen || app.settingsOpen || (app.s?.recoveries.length ?? 0) > 0) return;
  const ctrl = e.ctrlKey || e.metaKey;
  const k = e.key.toLowerCase();
  const go = (f: () => unknown) => {
    e.preventDefault();
    f();
  };
  if (ctrl) {
    if (k === "z" && !e.shiftKey) return go(actions.undo);
    if (k === "y" || (k === "z" && e.shiftKey)) return go(actions.redo);
    if (k === "s" && e.shiftKey) return go(actions.saveAs);
    if (k === "s") return go(actions.save);
    if (k === "o") return go(actions.open);
    if (k === "n") return go(actions.newProject);
    if (k === "i") return go(actions.importMedia);
    if (k === "m") return go(actions.exportOpen);
    if (k === "c") return go(() => actions.act("copy"));
    if (k === "x") return go(() => actions.act("cut"));
    if (k === "v") return go(() => actions.act("paste"));
    if (k === "a") return go(() => actions.act("select_all"));
    if (k === "t") return go(actions.addTitle);
    return;
  }
  switch (e.key) {
    case " ":
      return go(() => actions.act("play"));
    case "ArrowLeft":
      return go(() => actions.act(e.altKey ? "nudge_left" : "prev_frame"));
    case "ArrowRight":
      return go(() => actions.act(e.altKey ? "nudge_right" : "next_frame"));
    case "Home":
      return go(() => actions.act("start"));
    case "End":
      return go(() => actions.act("end"));
    case "Delete":
      return go(() => actions.act(e.shiftKey ? "ripple_delete" : "delete"));
    case "Backspace":
      return go(() => actions.act("delete"));
    case "Escape":
      return go(() => actions.act("deselect"));
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
  }
}
