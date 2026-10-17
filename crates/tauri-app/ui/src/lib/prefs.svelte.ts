// Edit > Preferences: the interface's colours and fonts. They're CSS custom
// properties (see app.css), set on the page as soon as they change and kept
// in this computer's local storage.

export interface Look {
  labels: {
    video: string;
    image: string;
    audio: string;
    title: string;
    comp: string;
    /// Clip names on the timeline.
    text: string;
  };
  ui: {
    base: string;
    panel: string;
    raised: string;
    well: string;
    line: string;
    text: string;
    muted: string;
    accent: string;
  };
  buttons: {
    bg: string;
    text: string;
    hover: string;
    border: string;
    primary: string;
    primaryText: string;
    radius: number;
  };
  fonts: {
    /// A family name, or "" for the built-in IBM Plex Sans.
    ui: string;
    /// For timecode and figures; "" = same as the interface.
    numbers: string;
    /// Text size, 1 = normal.
    scale: number;
  };
}

export const DEFAULTS: Look = {
  labels: { video: "#3d6a96", image: "#4a7d8c", audio: "#3e7a55", title: "#6c55a8", comp: "#9a7330", text: "#ffffff" },
  ui: { base: "#1e1f22", panel: "#26282c", raised: "#2f3237", well: "#17181b", line: "#3a3d43", text: "#d9dbdf", muted: "#8b9099", accent: "#f2a93b" },
  buttons: { bg: "#2f3237", text: "#d9dbdf", hover: "#383b41", border: "#3a3d43", primary: "#f2a93b", primaryText: "#1e1f22", radius: 4 },
  fonts: { ui: "", numbers: "", scale: 1 },
};

/// Ready-made interface colour schemes (Interface page).
export const UI_SCHEMES: { name: string; ui: Look["ui"] }[] = [
  { name: "Graphite (default)", ui: DEFAULTS.ui },
  { name: "Midnight", ui: { base: "#161a22", panel: "#1d2330", raised: "#262e3d", well: "#10131a", line: "#323c4f", text: "#d6dceb", muted: "#8792a8", accent: "#5aa9ff" } },
  { name: "Charcoal", ui: { base: "#121212", panel: "#1b1b1b", raised: "#262626", well: "#0b0b0b", line: "#333333", text: "#e2e2e2", muted: "#909090", accent: "#e0e0e0" } },
  { name: "Warm slate", ui: { base: "#211f1d", panel: "#2a2724", raised: "#35312d", well: "#191715", line: "#443f39", text: "#e6dfd6", muted: "#a0968a", accent: "#e8915a" } },
  { name: "High contrast", ui: { base: "#000000", panel: "#0d0d0d", raised: "#1f1f1f", well: "#000000", line: "#8a8a8a", text: "#ffffff", muted: "#d0d0d0", accent: "#ffd400" } },
];

/// Ready-made label colour sets (Label colours page).
export const LABEL_SETS: { name: string; labels: Look["labels"] }[] = [
  { name: "Muted (default)", labels: DEFAULTS.labels },
  { name: "Bright", labels: { video: "#2f7fe0", image: "#1fa8b8", audio: "#2fb35a", title: "#9b5cf0", comp: "#e09a1f", text: "#ffffff" } },
  { name: "Pastel", labels: { video: "#8fb3dc", image: "#94cbd0", audio: "#9ccfa8", title: "#bba9e0", comp: "#e3c58f", text: "#1b1b1b" } },
  { name: "HitFilm-style", labels: { video: "#3c78b4", image: "#5a8c5a", audio: "#3a9a8a", title: "#a050a0", comp: "#c87832", text: "#ffffff" } },
];

const KEY = "look";

function load(): Look {
  const d = structuredClone(DEFAULTS);
  try {
    const s = JSON.parse(localStorage.getItem(KEY) ?? "{}");
    for (const k of Object.keys(d) as (keyof Look)[]) Object.assign(d[k], s[k] ?? {});
  } catch {}
  return d;
}

/// The look in effect (what the dialog edits, live).
export const look = $state<Look>(load());

export function save() {
  try {
    localStorage.setItem(KEY, JSON.stringify(look));
  } catch {}
}

export function snapshot(): Look {
  return structuredClone($state.snapshot(look)) as Look;
}

export function restore(l: Look) {
  for (const k of Object.keys(l) as (keyof Look)[]) Object.assign(look[k], l[k]);
}

const quote = (f: string) => (/^[\w-]+$/.test(f) ? f : `"${f.replace(/"/g, "")}"`);

/// Put the look on the page.
export function apply(l: Look = look) {
  const r = document.documentElement.style;
  const set = (k: string, v: string) => r.setProperty(k, v);
  set("--clip-video", l.labels.video);
  set("--clip-image", l.labels.image);
  set("--clip-audio", l.labels.audio);
  set("--clip-title", l.labels.title);
  set("--clip-comp", l.labels.comp);
  set("--clip-text", l.labels.text);
  set("--base", l.ui.base);
  set("--panel", l.ui.panel);
  set("--raised", l.ui.raised);
  set("--hover", `color-mix(in srgb, ${l.ui.raised} 80%, ${l.ui.text})`);
  set("--well", l.ui.well);
  set("--line", l.ui.line);
  set("--line-soft", `color-mix(in srgb, ${l.ui.line} 55%, ${l.ui.panel})`);
  set("--text", l.ui.text);
  set("--muted", l.ui.muted);
  set("--faint", `color-mix(in srgb, ${l.ui.muted} 60%, ${l.ui.panel})`);
  set("--accent", l.ui.accent);
  set("--btn", l.buttons.bg);
  set("--btn-text", l.buttons.text);
  set("--btn-hover", l.buttons.hover);
  set("--btn-border", l.buttons.border);
  set("--btn-primary", l.buttons.primary);
  set("--btn-primary-text", l.buttons.primaryText);
  set("--radius", `${l.buttons.radius}px`);
  const builtIn = `"IBM Plex Sans", system-ui, sans-serif`;
  set("--font", l.fonts.ui ? `${quote(l.fonts.ui)}, ${builtIn}` : builtIn);
  set("--font-tc", l.fonts.numbers ? `${quote(l.fonts.numbers)}, var(--font)` : "var(--font)");
  set("--text-scale", String(l.fonts.scale));
  // Light backgrounds want light native controls (scrollbars, pickers).
  set("color-scheme", luminance(l.ui.base) > 0.5 ? "light" : "dark");
}

function luminance(hex: string): number {
  const n = parseInt(hex.slice(1), 16);
  return (0.2126 * ((n >> 16) & 255) + 0.7152 * ((n >> 8) & 255) + 0.0722 * (n & 255)) / 255;
}

/// The Preferences window: open, and on which page.
export const prefsDialog = $state({ open: false, page: "labels" as "labels" | "ui" | "buttons" | "fonts" });

export function openPrefs(page: typeof prefsDialog.page) {
  prefsDialog.page = page;
  prefsDialog.open = true;
}
