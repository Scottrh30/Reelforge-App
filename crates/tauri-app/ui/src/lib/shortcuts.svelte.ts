// Keyboard shortcuts: every command that has one, its default keys, and the
// keys you've set instead (Settings → Keyboard shortcuts…). Your keys are kept
// in this computer's local storage. A key is written as its modifiers then the
// key: "Ctrl+Shift+S", "Alt+ArrowLeft", "Space", "Shift+Delete", "J".

export interface Shortcut {
  id: string;
  /// What it does, as the editor lists it.
  label: string;
  /// Its menu, for grouping and search.
  group: string;
  /// The keys it comes with.
  keys: string[];
}

export const SHORTCUTS: Shortcut[] = [
  // File
  { id: "new_project", label: "New project", group: "File", keys: ["Ctrl+N"] },
  { id: "start_screen", label: "Start screen", group: "File", keys: ["Ctrl+Shift+N"] },
  { id: "open", label: "Open…", group: "File", keys: ["Ctrl+O"] },
  { id: "save", label: "Save", group: "File", keys: ["Ctrl+S"] },
  { id: "save_as", label: "Save as…", group: "File", keys: ["Ctrl+Shift+S"] },
  { id: "save_version", label: "Save version…", group: "File", keys: ["Ctrl+Alt+S"] },
  { id: "import", label: "Import media…", group: "File", keys: ["Ctrl+I"] },
  { id: "export", label: "Export…", group: "File", keys: ["Ctrl+M"] },
  { id: "snapshot", label: "Snapshot of the frame", group: "File", keys: ["Ctrl+Shift+E"] },
  // Edit
  { id: "undo", label: "Undo", group: "Edit", keys: ["Ctrl+Z"] },
  { id: "redo", label: "Redo", group: "Edit", keys: ["Ctrl+Y", "Ctrl+Shift+Z"] },
  { id: "copy", label: "Copy", group: "Edit", keys: ["Ctrl+C"] },
  { id: "cut", label: "Cut", group: "Edit", keys: ["Ctrl+X"] },
  { id: "paste", label: "Paste", group: "Edit", keys: ["Ctrl+V"] },
  { id: "paste_attributes", label: "Paste attributes…", group: "Edit", keys: ["Ctrl+Alt+V"] },
  { id: "select_all", label: "Select all", group: "Edit", keys: ["Ctrl+A"] },
  { id: "deselect", label: "Deselect (or stop the render preview)", group: "Edit", keys: ["Escape"] },
  { id: "delete", label: "Delete", group: "Edit", keys: ["Delete", "Backspace"] },
  { id: "ripple_delete", label: "Ripple delete", group: "Edit", keys: ["Shift+Delete"] },
  // Timeline
  { id: "split", label: "Split at the playhead", group: "Timeline", keys: ["S"] },
  { id: "make_composite", label: "Make composition", group: "Timeline", keys: ["Ctrl+G"] },
  { id: "add_title", label: "Add title", group: "Timeline", keys: ["Ctrl+T"] },
  { id: "dissolve", label: "Add a cross dissolve", group: "Timeline", keys: ["Ctrl+D"] },
  { id: "crossfade", label: "Add a crossfade", group: "Timeline", keys: ["Ctrl+Shift+D"] },
  { id: "mark_in", label: "Mark in", group: "Timeline", keys: ["I"] },
  { id: "mark_out", label: "Mark out", group: "Timeline", keys: ["O"] },
  { id: "add_marker", label: "Add marker", group: "Timeline", keys: ["M"] },
  { id: "freeze_frame", label: "Freeze frame", group: "Timeline", keys: ["Shift+F"] },
  { id: "nudge_left", label: "Nudge left a frame", group: "Timeline", keys: [",", "Alt+ArrowLeft"] },
  { id: "nudge_right", label: "Nudge right a frame", group: "Timeline", keys: [".", "Alt+ArrowRight"] },
  { id: "nudge_left_10", label: "Nudge left 10 frames", group: "Timeline", keys: ["Shift+,", "Shift+<"] },
  { id: "nudge_right_10", label: "Nudge right 10 frames", group: "Timeline", keys: ["Shift+.", "Shift+>"] },
  { id: "highlight_up", label: "Highlight the track above", group: "Timeline", keys: ["Alt+ArrowUp"] },
  { id: "highlight_down", label: "Highlight the track below", group: "Timeline", keys: ["Alt+ArrowDown"] },
  { id: "fit_timeline", label: "Fit the timeline", group: "Timeline", keys: ["\\"] },
  // Tools
  { id: "tool_select", label: "Selection tool", group: "Tools", keys: ["V"] },
  { id: "tool_ripple", label: "Ripple edit tool", group: "Tools", keys: ["B"] },
  { id: "tool_roll", label: "Rolling edit tool", group: "Tools", keys: ["N"] },
  { id: "tool_slip", label: "Slip tool", group: "Tools", keys: ["Y"] },
  { id: "tool_slide", label: "Slide tool", group: "Tools", keys: ["U"] },
  { id: "effects", label: "Effects…", group: "Tools", keys: ["E"] },
  // Playback
  { id: "play", label: "Play / pause", group: "Playback", keys: ["Space"] },
  { id: "prev_frame", label: "Previous frame", group: "Playback", keys: ["ArrowLeft"] },
  { id: "next_frame", label: "Next frame", group: "Playback", keys: ["ArrowRight"] },
  { id: "prev_edit", label: "Previous edit", group: "Playback", keys: ["ArrowUp"] },
  { id: "next_edit", label: "Next edit", group: "Playback", keys: ["ArrowDown"] },
  { id: "prev_marker", label: "Previous marker", group: "Playback", keys: ["Shift+ArrowUp"] },
  { id: "next_marker", label: "Next marker", group: "Playback", keys: ["Shift+ArrowDown"] },
  { id: "start", label: "Go to the start", group: "Playback", keys: ["Home"] },
  { id: "end", label: "Go to the end", group: "Playback", keys: ["End"] },
  { id: "shuttle_back", label: "Shuttle back", group: "Playback", keys: ["J"] },
  { id: "shuttle_stop", label: "Shuttle stop", group: "Playback", keys: ["K"] },
  { id: "shuttle_forward", label: "Shuttle forward", group: "Playback", keys: ["L"] },
  { id: "render_preview", label: "Render preview", group: "Playback", keys: ["Enter"] },
  { id: "full_screen", label: "Full-screen preview", group: "Playback", keys: ["`"] },
];

const STORE = "reelforge.shortcuts";

function load(): Record<string, string[]> {
  try {
    const v = JSON.parse(localStorage.getItem(STORE) ?? "{}");
    if (v && typeof v === "object") {
      const out: Record<string, string[]> = {};
      for (const [k, keys] of Object.entries(v)) if (Array.isArray(keys)) out[k] = keys.filter((x) => typeof x === "string");
      return out;
    }
  } catch {}
  return {};
}

/// Your keys, by command (commands left out have their defaults).
export const custom = $state<{ keys: Record<string, string[]> }>({ keys: load() });

function save() {
  try {
    localStorage.setItem(STORE, JSON.stringify(custom.keys));
  } catch {}
}

/// A command's keys now.
export function keysOf(id: string): string[] {
  return custom.keys[id] ?? SHORTCUTS.find((s) => s.id === id)?.keys ?? [];
}

/// The first of a command's keys, for menus ("" if none).
export function hintOf(id: string): string {
  return pretty(keysOf(id)[0] ?? "");
}

/// A key as menus show it ("Shift+↑", "Del").
export function pretty(combo: string): string {
  return combo.replace("ArrowUp", "↑").replace("ArrowDown", "↓").replace("ArrowLeft", "←").replace("ArrowRight", "→").replace(/Delete$/, "Del");
}

export function setKeys(id: string, keys: string[]) {
  const def = SHORTCUTS.find((s) => s.id === id)?.keys ?? [];
  const uniq = [...new Set(keys)];
  if (uniq.length === def.length && uniq.every((k, i) => k === def[i])) delete custom.keys[id];
  else custom.keys[id] = uniq;
  save();
}

export function resetAll() {
  custom.keys = {};
  save();
}

/// The key a keyboard event is, written as a shortcut ("" for a modifier on its own).
export function comboOf(e: KeyboardEvent): string {
  if (["Control", "Shift", "Alt", "Meta", "AltGraph", "CapsLock", "Dead", "Unidentified"].includes(e.key)) return "";
  let key = e.key === " " ? "Space" : e.key;
  if (key.length === 1) key = key.toUpperCase();
  const mods = [e.ctrlKey || e.metaKey ? "Ctrl" : "", e.altKey ? "Alt" : "", e.shiftKey ? "Shift" : ""].filter(Boolean);
  return [...mods, key].join("+");
}

/// The command a key runs now (the first that has it).
export function commandFor(combo: string): string | null {
  for (const s of SHORTCUTS) if (keysOf(s.id).includes(combo)) return s.id;
  return null;
}

/// Other commands that also use `combo` (a conflict).
export function clashes(combo: string, except: string): Shortcut[] {
  return SHORTCUTS.filter((s) => s.id !== except && keysOf(s.id).includes(combo));
}

/// The Keyboard shortcuts dialog (opened from Settings).
export const shortcutsDialog = $state({ open: false });
