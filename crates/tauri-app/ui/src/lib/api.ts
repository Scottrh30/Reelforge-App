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
  proxy_error: string | null;
  proxyable: boolean;
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
  audio: ClipAudio | null;
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
  markers: MarkerView[];
  transitions: TransitionMark[];
  tracks: TrackView[];
}

export type Transition =
  | "Dissolve" | "DipToBlack" | "DipToWhite" | "WipeLeft" | "WipeRight" | "WipeUp" | "WipeDown" | "Iris"
  | "SlideLeft" | "SlideRight" | "SlideUp" | "SlideDown" | "PushLeft" | "PushRight" | "PushUp" | "PushDown";

export interface TransitionMark {
  track: number;
  left: number;
  right: number;
  cut: Ticks;
  start: Ticks;
  end: Ticks;
  requested: Ticks;
  label: string;
  empty: boolean;
  audio: boolean;
}

export interface TransitionIn {
  kind: Transition | null;
  length: Ticks;
  actual: Ticks;
  limit: Ticks;
}

export interface TransitionKind {
  kind: Transition;
  label: string;
  description: string;
  group: string;
}

export type MarkerColor = "Yellow" | "Red" | "Green" | "Blue" | "Purple";

export interface MarkerView {
  at: Ticks;
  name: string;
  color: MarkerColor;
  rgb: string;
}

export type Interp = "Linear" | "Ease" | "Hold";

export interface KeyView {
  at: Ticks;
  value: number;
  interp: Interp;
}

export interface ChannelView {
  id: string;
  name: string;
  group: string;
  unit: string;
  range: [number, number] | null;
  value: number;
  keys: KeyView[];
  samples: number[];
}

export interface KeysView {
  on_clip: boolean;
  shown: Transform;
  props: { prop: string; animated: boolean; key_here: boolean; count: number }[];
  channels: ChannelView[];
  span: [Ticks, Ticks];
  prev_key: Ticks | null;
  next_key: Ticks | null;
  interp_here: "Linear" | "Ease" | "Hold" | null;
  pin_twisted: boolean;
}

export interface CollectPlanView {
  files: number;
  bytes: number;
  already: number;
  missing: string[];
  project_file: string;
  replaces: boolean;
  unsaved: boolean;
}

export interface CollectView {
  running: boolean;
  bytes: number;
  total: number;
  file: string;
  index: number;
  count: number;
  stopping: boolean;
  done: null | { ok: boolean; error: string; project_file: string; folder: string; copied: number; reused: number; bytes: number; missing: string[]; seconds: number };
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
  title_missing_font: boolean;
  gizmo: Gizmo | null;
  track: string;
  start: Ticks;
  duration: Ticks;
  source_in: Ticks;
  visual: { transform: Transform; blend: Blend; layer_size: [number, number] } | null;
  title: TitleSpec | null;
  audio: ClipAudio | null;
  speed: number;
  reverse: boolean;
  effects: EffectView[];
  animated: string[];
  keys: KeysView | null;
  transition_in: TransitionIn | null;
  can_ramp: boolean;
  footage: boolean;
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

export interface Prefs {
  use_proxies: boolean;
  auto_proxies: boolean;
  autosave_minutes: number;
  render_cache_mb: number;
  export_presets: UserPreset[];
  scrub_audio: boolean;
  timeline_thumbnails: boolean;
  viewer_snapping: boolean;
  hw_decode: boolean;
  cache_mb: number;
}

export interface SettingsInfo {
  hw_api: string;
  hw_status: string;
  cache_frames: number;
  cache_mb: number;
  cache_hits: number;
  render_frames: number;
  render_mb: number;
  proxies: number;
  proxy_bytes: number;
  proxy_dir: string;
  autosave_dir: string;
}

export type GizmoHandle = { kind: "corner" | "edge"; index: number } | { kind: "rotate" | "anchor" };

export interface Gizmo {
  corners: Pt[];
  edges: Pt[];
  stem_base: Pt;
  stem_dir: Pt;
  anchor: Pt;
  full: Pt[];
  pinned: boolean;
  cropped: boolean;
}

export interface ViewerHover {
  handle: GizmoHandle;
  dir: number | null;
  tip: string;
}

export interface ViewerFeedback {
  readout: string;
  guides_x: number[];
  guides_y: number[];
}

// ---- Effects ----

export type EffectKind = "ColorCorrect" | "Blur" | "ChromaKey" | "Vignette" | "Levels" | "Curves" | "Sharpen" | "Glow" | "DropShadow" | "Lut" | "Plugin";

export interface Curves {
  master: Pt[];
  red: Pt[];
  green: Pt[];
  blue: Pt[];
}

export interface ParamView {
  name: string;
  value: number;
  default: number;
  min: number;
  max: number;
  suffix: string;
  step: number;
  decimals: number;
  ui: "number" | "toggle" | "choice";
  options: string[];
  animated: boolean;
  key_here: boolean;
}

export interface EffectView {
  label: string;
  kind: EffectKind;
  enabled: boolean;
  plugin: string | null;
  missing: boolean;
  params: ParamView[];
  curves: Curves | null;
  lut: string | null;
  lut_error: string | null;
  animated: boolean;
}

export interface CatalogItem {
  kind: EffectKind;
  plugin: string | null;
  label: string;
  description: string;
  author: string;
  version: string;
}

export interface CatalogGroup {
  label: string;
  items: CatalogItem[];
}

export interface PluginView {
  folder: string;
  place: string;
  ok: boolean;
  name: string;
  id: string;
  version: string;
  author: string;
  category: string;
  description: string;
  params: number;
  passes: number;
  error: string | null;
  removable: boolean;
}

export interface PluginsView {
  entries: PluginView[];
  live_reload: boolean;
  folder: string;
}

// ---- Export ----

export type VideoCodec = "H264" | "H265" | "ProRes422" | "ProRes422Hq" | "DnxhrSq" | "DnxhrHq" | "PngSequence" | "JpegSequence" | "TiffSequence";
export type Quality = "High" | "Medium" | "Low";
export type Fit = "Letterbox" | "Crop" | "Stretch";
export type ColorOut = "Sdr8" | "Sdr10" | "HdrPq" | "HdrHlg";
export type Size = { Scaled: number } | { Fixed: [number, number] };

export interface Spec {
  video: VideoCodec | null;
  size: Size;
  quality: Quality;
  fit: Fit;
  hardware: boolean;
  color: ColorOut;
  bitrate_kbps: number | null;
}

export interface UserPreset {
  name: string;
  spec: Spec;
  include_audio: boolean;
}

export interface ExportOptions {
  codecs: { id: VideoCodec; label: string; extension: string; sequence: boolean; quality: boolean; bitrate: boolean; ten_bit: boolean; hdr: boolean; hardware: boolean }[];
  qualities: [Quality, string][];
  fits: [Fit, string][];
  colors: { id: ColorOut; label: string; hint: string }[];
  sizes: [number, number, string][];
  presets: { index: number; label: string; hint: string; spec: Spec | null }[];
}

export interface JobView {
  id: number;
  title: string;
  detail: string;
  state: "waiting" | "running" | "done" | "failed" | "cancelled";
  done: number;
  total: number;
  fps: number | null;
  remaining: number | null;
  cancelling: boolean;
  message: string | null;
  file: string;
}

export interface QueueView {
  jobs: JobView[];
  run: boolean;
  summary: string | null;
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
  prefs: Prefs;
}

export interface Live {
  rev: number;
  picture_rev: number;
  playhead: Ticks;
  playing: boolean;
  shuttle: number;
  status: string;
  dirty: boolean;
  clock: string;
  exact: boolean;
  from_cache: boolean;
  master: { peak: [number, number]; rms: [number, number]; hold: [number, number]; clipped: [boolean, boolean] };
  tracks: [number, number][];
  export: QueueView;
  waveforms: number;
  plugins_rev: number;
  proxies: number;
  collect: CollectView | null;
  tracking: [number, number] | null;
  render: number | null;
  render_gen: number;
}

export interface AttrOffers {
  from: string;
  targets: number;
  visual: boolean;
  corner_pin: boolean;
  effects: number;
  masks: number;
  audio: boolean;
  text_style: boolean;
}

export interface AttrSet {
  transform: boolean;
  crop: boolean;
  corner_pin: boolean;
  blend: boolean;
  effects: boolean;
  replace_effects: boolean;
  masks: boolean;
  audio: boolean;
  text_style: boolean;
}

export const api = {
  tick: () => invoke<Live>("tick"),
  state: () => invoke<State>("get_state"),
  frame: (max_w: number, max_h: number, have: number, jpeg: boolean) => invoke<ArrayBuffer>("frame", { maxW: max_w, maxH: max_h, have, jpeg }),
  waveform: (media: number) => invoke<ArrayBuffer>("waveform", { media }),
  waveformRate: () => invoke<number>("waveform_rate"),
  fontFamilies: () => invoke<string[]>("font_families"),
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
  exportOptions: () => invoke<ExportOptions>("export_options"),
  exportSuggest: (ext: string) => invoke<{ path: string; preset: number; has_audio: boolean }>("export_suggest", { ext }),
  exportPathDialog: (current: string, extension: string) => invoke<string | null>("export_path_dialog", { current, extension }),
  exportPresetSave: (name: string, spec: Spec, includeAudio: boolean) => invoke("export_preset_save", { name, spec, includeAudio }),
  exportPresetDelete: (name: string) => invoke("export_preset_delete", { name }),
  exportAdd: (spec: Spec, path: string, includeAudio: boolean, markedOnly: boolean, preset: string | null, start: boolean) =>
    invoke("export_add", { spec, path, includeAudio, markedOnly, preset, start }),
  exportJob: (id: number, op: "cancel" | "remove" | "retry" | "up") => invoke("export_job", { id, op }),
  exportRun: (on: boolean) => invoke("export_run", { on }),
  exportClear: () => invoke("export_clear"),
  effectAdd: (id: number, kind: EffectKind, plugin: string | null) => invoke("effect_add", { id, kind, plugin }),
  effectOp: (id: number, index: number, op: "up" | "down" | "remove" | "toggle" | "duplicate" | "reset") => invoke("effect_op", { id, index, op }),
  effectParam: (id: number, index: number, param: number, value: number) => invoke("effect_param", { id, index, param, value }),
  effectKey: (id: number, index: number, param: number) => invoke("effect_key", { id, index, param }),
  effectCurves: (id: number, index: number, curves: Curves) => invoke("effect_curves", { id, index, curves }),
  effectLut: (id: number, index: number) => invoke("effect_lut", { id, index }),
  effectCatalog: () => invoke<CatalogGroup[]>("effect_catalog"),
  pluginsList: () => invoke<PluginsView>("plugins_list"),
  pluginsInstall: (zip: boolean) => invoke<string | null>("plugins_install", { zip }),
  pluginsNew: () => invoke<string>("plugins_new"),
  pluginsRemove: (folder: string) => invoke<string>("plugins_remove", { folder }),
  pluginsReload: () => invoke<string>("plugins_reload"),
  pluginsLive: (on: boolean) => invoke("plugins_live", { on }),
  openFolder: (path: string) => invoke("open_folder", { path }),
  transformKey: (id: number, prop: string) => invoke("transform_key", { id, prop }),
  keyMove: (id: number, channel: string, index: number, at: Ticks, value: number, keepTime: boolean) => invoke<number | null>("key_move", { id, channel, index, at: Math.round(at), value, keepTime }),
  keyInterp: (id: number, channel: string, index: number, interp: Interp) => invoke("key_interp", { id, channel, index, interp }),
  keyDelete: (id: number, channel: string, index: number) => invoke("key_delete", { id, channel, index }),
  keyAdd: (id: number, channel: string, at: Ticks) => invoke<number | null>("key_add", { id, channel, at: Math.round(at) }),
  markerColors: () => invoke<[MarkerColor, string][]>("marker_colors"),
  markerAdd: () => invoke("marker_add"),
  markerSet: (index: number, at: Ticks, name: string, color: MarkerColor) => invoke("marker_set", { index, at: Math.round(at), name, color }),
  markerRemove: (index: number) => invoke("marker_remove", { index }),
  proxyMake: (media: number) => invoke("proxy_make", { media }),
  proxyDelete: (media: number) => invoke("proxy_delete", { media }),
  proxiesDeleteAll: () => invoke("proxies_delete_all"),
  proxiesUse: (on: boolean) => invoke("proxies_use", { on }),
  proxyFolder: () => invoke<string>("proxy_folder"),
  relinkMedia: (media: number) => invoke("relink_media", { media }),
  relinkSearch: () => invoke("relink_search"),
  collectPlan: (dest: string, name: string, includeUnused: boolean, includeFonts: boolean) => invoke<CollectPlanView>("collect_plan", { dest, name, includeUnused, includeFonts }),
  collectPickFolder: () => invoke<string | null>("collect_pick_folder"),
  collectStart: (dest: string, name: string, includeUnused: boolean, includeFonts: boolean) => invoke("collect_start", { dest, name, includeUnused, includeFonts }),
  collectStop: () => invoke("collect_stop"),
  collectDismiss: () => invoke("collect_dismiss"),
  transitionList: () => invoke<TransitionKind[]>("transition_list"),
  transitionApply: (kind: Transition, id: number | null) => invoke("transition_apply", { kind, id }),
  transitionKind: (id: number, kind: Transition) => invoke("transition_kind", { id, kind }),
  transitionLength: (id: number, length: Ticks) => invoke("transition_length", { id, length: Math.round(length) }),
  transitionRemove: (id: number) => invoke("transition_remove", { id }),
  speedRamp: (id: number, from: number, to: number, steps: number, smooth: boolean) => invoke("speed_ramp", { id, from, to, steps, smooth }),
  showInFolder: (path: string) => invoke("show_in_folder", { path }),
  openAutosaveDialog: () => invoke("open_autosave_dialog"),
  renderedRuns: (step: number) => invoke<[number, number][]>("rendered_runs", { step }),
  fontFaces: () => invoke<{ path: string | null; index: number; name: string }[]>("font_faces"),
  placeTitle: (id: number, place: "centre" | "lower_third" | "top") => invoke("place_title", { id, place }),
  viewerHover: (at: Pt, ctrl: boolean, scale: number) => invoke<ViewerHover | null>("viewer_hover", { at, ctrl, scale }),
  viewerPress: (at: Pt, ctrl: boolean, scale: number) => invoke<"handle" | "move" | "none">("viewer_press", { at, ctrl, scale }),
  viewerDrag: (at: Pt, shift: boolean, alt: boolean, scale: number) => invoke<ViewerFeedback>("viewer_drag", { at, shift, alt, scale }),
  viewerRelease: () => invoke("viewer_release"),
  settingsInfo: () => invoke<SettingsInfo>("settings_info"),
  transformOp: (id: number, op: "anchor_centre" | "crop_clear" | "pin_on" | "pin_off" | "pin_reset" | "fit" | "fill" | "reset") => invoke("transform_op", { id, op }),
  transformInterp: (id: number, interp: "Linear" | "Ease" | "Hold") => invoke("transform_interp", { id, interp }),
  attrOffers: () => invoke<AttrOffers | null>("attr_offers"),
  pasteAttributes: (set: AttrSet) => invoke("paste_attributes", { set }),
};
