// Quick actions: buttons beside the menus for one-click access (Settings →
// Interface → Quick actions chooses which).
import { api } from "./api";
import { actions, COMMANDS } from "./actions.svelte";
import { app, run } from "./session.svelte";
import { hintOf } from "./shortcuts.svelte";

export interface QuickAction {
  id: string;
  label: string;
  /// A short symbol for the button.
  icon: string;
  run: () => unknown;
}

export const QUICK_ACTIONS: QuickAction[] = [
  { id: "new_project", label: "New project", icon: "＋", run: () => actions.newProject() },
  { id: "open", label: "Open…", icon: "📂", run: () => actions.open() },
  { id: "save", label: "Save", icon: "💾", run: () => actions.save() },
  { id: "import", label: "Import media…", icon: "⤓", run: () => actions.importMedia() },
  { id: "export", label: "Export…", icon: "⤒", run: () => actions.exportOpen() },
  { id: "undo", label: "Undo", icon: "↶", run: () => actions.undo() },
  { id: "redo", label: "Redo", icon: "↷", run: () => actions.redo() },
  { id: "add_title", label: "Add title", icon: "T", run: () => actions.addTitle() },
  { id: "new_composite", label: "New composition", icon: "▣", run: () => run(api.newComposite()) },
  { id: "snapshot", label: "Snapshot", icon: "📷", run: () => run(api.snapshot(false)) },
  { id: "render_preview", label: "Render preview", icon: "▶︎", run: () => COMMANDS.render_preview() },
  { id: "full_screen", label: "Full-screen preview", icon: "⛶", run: () => COMMANDS.full_screen() },
  { id: "settings", label: "Settings…", icon: "⚙", run: () => (app.settingsOpen = true) },
];

/// The quick actions chosen, in the list's order, with their key as a hint.
export function chosen(): (QuickAction & { hint: string })[] {
  const on = app.s?.prefs.quick_actions ?? [];
  return QUICK_ACTIONS.filter((q) => on.includes(q.id)).map((q) => ({ ...q, hint: hintOf(q.id) }));
}
