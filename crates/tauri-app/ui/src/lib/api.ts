// The Rust side's commands and the shapes they send (see ../../../src/views.rs).

import { invoke } from "@tauri-apps/api/core";

export type Ticks = number;

export interface Transform {
  position: [number, number];
  scale: [number, number];
  rotation: number;
  opacity: number;
  anchor: [number, number];
  crop: [number, number, number, number];
  pin?: unknown;
}

export type Blend = "Normal" | "Add" | "Multiply" | "Screen";

export interface TitleSpec {
  text: string;
  size: number;
  color: [number, number, number, number];
  [key: string]: unknown;
}

export interface ClipAudio {
  volume_db: number;
  pan: number;
  fade_in: Ticks;
  fade_out: Ticks;
  [key: string]: unknown;
}

export interface TrackAudio {
  volume_db: number;
  pan: number;
}

export interface MediaView {
  id: number;
  name: string;
  path: string;
  kind: "video" | "audio" | "image";
  duration: Ticks;
  width: number;
  height: number;
  fps: number;
  has_audio: boolean;
  missing: boolean;
  proxy: string;
  detail: string;
}

export type ClipKind = "video" | "audio" | "image" | "title" | "comp";

export interface ClipView {
  id: number;
  name: string;
  kind: ClipKind;
  media: number;
  start: Ticks;
  duration: Ticks;
  source_in: Ticks;
  link: number | null;
  speed: number;
  reverse: boolean;
  dissolve_in: Ticks;
  fade_in: Ticks;
  fade_out: Ticks;
  effects: number;
}

export interface TrackView {
  index: number;
  name: string;
  kind: "video" | "audio";
  muted: boolean;
  locked: boolean;
  audio: TrackAudio;
  clips: ClipView[];
}

export interface SeqView {
  name: string;
  width: number;
  height: number;
  rate_num: number;
  rate_den: number;
  frame: Ticks;
  duration: Ticks;
  in_point: Ticks | null;
  out_point: Ticks | null;
  markers: [Ticks, string][];
  tracks: TrackView[];
}

export interface SeqInfo {
  name: string;
  width: number;
  height: number;
  fps: number;
  duration: Ticks;
}

export interface Inspector {
  id: number;
  name: string;
  kind: ClipKind;
  track: string;
  start: Ticks;
  duration: Ticks;
  source_in: Ticks;
  visual: { transform: Transform; blend: Blend; layer_size: [number, number] } | null;
  title: TitleSpec | null;
  audio: ClipAudio | null;
  speed: number;
  reverse: boolean;
  effects: string[];
  animated: string[];
  linked: boolean;
  layer_map: number[] | null;
  layer_inv: number[] | null;
  layer_px: Pt;
  masks: MaskView[];
  can_track: boolean;
  tracker: TrackerView | null;
  tracking: number | null;
  tracking_elsewhere: boolean;
}

export type Pt = [number, number];

export interface Mask {
  name: string;
  shape: "Rectangle" | "Ellipse" | "Freehand";
  points: Pt[];
  keys: { at: Ticks; points: Pt[] }[];
  mode: "Add" | "Subtract" | "Intersect";
  inverted: boolean;
  feather: number;
  expansion: number;
  opacity: number;
  enabled: boolean;
}

export interface MaskView {
  mask: Mask;
  outline: Pt[];
  handles: Pt[];
  key_here: boolean;
}

export type TrackMode = "Position" | "Transform" | "CornerPin";

export interface TrackerView {
  mode: TrackMode;
  size: number;
  search: number;
  shown: Pt[];
  paths: Pt[][];
  frames: number;
  range: [Ticks, Ticks] | null;
  tracked: boolean;
  targets: { clip: number; mask: number | null; label: string }[];
}

export interface PresetView {
  index: number;
  label: string;
  hint: string;
  extension: string;
}

export interface Prefs {
  use_proxies: boolean;
  auto_proxies: boolean;
  autosave_minutes: number;
  render_cache_mb: number;
}

export interface State {
  rev: number;
  title: string;
  path: string | null;
  dirty: boolean;
  undo: string | null;
  redo: string | null;
  history: string[];
  history_pos: number;
  ticks_per_second: number;
  active: number;
  comp_open: number;
  sequences: SeqInfo[];
  media: MediaView[];
  seq: SeqView;
  selection: number | null;
  selected: number[];
  inspector: Inspector | null;
  recoveries: string[];
  presets: PresetView[];
  prefs: Prefs;
}

export interface ExportStatus {
  running: boolean;
  done: number;
  total: number;
  remaining: number | null;
  path: string;
  message: string | null;
  failed: boolean;
}

export interface Live {
  rev: number;
  picture_rev: number;
  playhead: Ticks;
  playing: boolean;
  status: string;
  dirty: boolean;
  clock: string;
  exact: boolean;
  from_cache: boolean;
  master: { peak: [number, number]; rms: [number, number]; hold: [number, number]; clipped: [boolean, boolean] };
  tracks: [number, number][];
  export: ExportStatus | null;
  waveforms: number;
  tracking: [number, number] | null;
}

export const api = {
  tick: () => invoke<Live>("tick"),
  state: () => invoke<State>("get_state"),
  frame: (max_w: number, max_h: number, have: number, jpeg: boolean) => invoke<ArrayBuffer>("frame", { maxW: max_w, maxH: max_h, have, jpeg }),
  waveform: (media: number) => invoke<ArrayBuffer>("waveform", { media }),
  waveformRate: () => invoke<number>("waveform_rate"),
  thumbnail: (path: string, at: Ticks) => invoke<ArrayBuffer>("thumbnail", { path, at }),
  newProject: () => invoke("new_project"),
  openDialog: () => invoke("open_dialog"),
  openPath: (path: string) => invoke("open_path", { path }),
  save: (saveAs: boolean) => invoke<boolean>("save", { saveAs }),
  importDialog: () => invoke("import_dialog"),
  importPaths: (paths: string[]) => invoke<number[]>("import_paths", { paths }),
  recover: (index: number) => invoke("recover", { index }),
  dismissRecovery: (index: number) => invoke("dismiss_recovery", { index }),
  setPrefs: (prefs: Prefs) => invoke("set_prefs", { prefs }),
  addToTimeline: (media: number, track: number | null, at: Ticks | null) => invoke("add_to_timeline", { media, track, at }),
  newCompositionFromMedia: (media: number) => invoke("new_composition_from_media", { media }),
  newComposite: () => invoke("new_composite"),
  addComposite: (index: number) => invoke("add_composite", { index }),
  openSequence: (index: number) => invoke("open_sequence", { index }),
  addTitle: () => invoke("add_title"),
  select: (id: number | null, mode: "only" | "toggle" | "none") => invoke("select", { id, mode }),
  selectMany: (ids: number[], add: boolean) => invoke("select_many", { ids, add }),
  moveClips: (lead: number, track: number, start: Ticks) => invoke("move_clips", { lead, track, start }),
  trim: (id: number, edge: "start" | "end", mode: string, delta: Ticks) => invoke<number | null>("trim", { id, edge, mode, delta }),
  commit: () => invoke("commit"),
  action: (name: string) => invoke("action", { name }),
  historyGoTo: (pos: number) => invoke("history_go_to", { pos }),
  seek: (t: Ticks) => invoke("seek", { t: Math.max(0, Math.round(t)) }),
  trackOp: (index: number, op: "mute" | "lock" | "delete" | "rename", name?: string) => invoke("track_op", { index, op, name }),
  setTrackAudio: (index: number, audio: TrackAudio) => invoke("set_track_audio", { index, audio }),
  setTransform: (id: number, transform: Transform) => invoke("set_transform", { id, transform }),
  setBlend: (id: number, blend: Blend) => invoke("set_blend", { id, blend }),
  setTitle: (id: number, spec: TitleSpec) => invoke("set_title", { id, spec }),
  setClipAudio: (id: number, audio: ClipAudio) => invoke("set_clip_audio", { id, audio }),
  setSpeed: (id: number, speed: number, reverse: boolean) => invoke("set_speed", { id, speed, reverse }),
  addMask: (id: number, shape: "rectangle" | "ellipse") => invoke<number | null>("add_mask", { id, shape }),
  addFreehandMask: (id: number, points: Pt[]) => invoke<number | null>("add_freehand_mask", { id, points }),
  setMasks: (id: number, masks: Mask[], done: boolean) => invoke("set_masks", { id, masks, done }),
  setMaskPoints: (id: number, index: number, points: Pt[]) => invoke("set_mask_points", { id, index, points }),
  maskOp: (id: number, index: number, op: "animate" | "still" | "key" | "unkey") => invoke("mask_op", { id, index, op }),
  trackerAdd: (id: number, mode: TrackMode) => invoke("tracker_add", { id, mode }),
  trackerMode: (id: number, mode: TrackMode) => invoke("tracker_mode", { id, mode }),
  trackerSizes: (id: number, size: number, search: number) => invoke("tracker_sizes", { id, size, search }),
  trackerMove: (id: number, point: number, pos: Pt) => invoke("tracker_move", { id, point, pos }),
  trackerOp: (id: number, op: "clear" | "remove") => invoke("tracker_op", { id, op }),
  trackStart: (id: number, forward: boolean) => invoke("track_start", { id, forward }),
  trackCancel: () => invoke("track_cancel"),
  trackApply: (id: number, target: number, mask: number | null, smoothing: number) => invoke("track_apply", { id, target, mask, smoothing }),
  exportPathDialog: (extension: string) => invoke<string | null>("export_path_dialog", { extension }),
  exportStart: (preset: number, path: string, includeAudio: boolean, markedOnly: boolean) =>
    invoke("export_start", { preset, path, includeAudio, markedOnly }),
  exportCancel: () => invoke("export_cancel"),
  exportDismiss: () => invoke("export_dismiss"),
  showInFolder: (path: string) => invoke("show_in_folder", { path }),
};
