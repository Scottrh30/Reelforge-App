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
  /// A 3D layer (placed by the camera, lit by lights). Left out when off.
  three_d?: boolean;
  /// Z position (3D layers), pixels; positive is further away.
  depth?: number;
  /// X and Y rotation (3D layers), degrees.
  orientation?: [number, number];
  /// Shaded by the scene's lights (left out when true).
  lit?: boolean;
}

export type Blend =
  | "Normal"
  | "Add"
  | "Multiply"
  | "Screen"
  | "Overlay"
  | "SoftLight"
  | "HardLight"
  | "ColorDodge"
  | "ColorBurn"
  | "Darken"
  | "Lighten"
  | "Difference"
  | "Exclusion"
  | "Hue"
  | "Saturation"
  | "Color"
  | "Luminosity";

export type ProxyFormat = "ProResProxy" | "DnxhrLb" | "H264";

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
  /** What a ready proxy is made as ("ProRes 422 Proxy"…). */
  proxy_format: string | null;
  proxyable: boolean;
  detail: string;
}

/// How a tracked mask's shape may move.
export type MaskMotion = "Position" | "Transform" | "Perspective" | "Bend";

export type ClipKind = "video" | "audio" | "image" | "title" | "comp" | "camera" | "light" | "point" | "plane" | "adjustment";

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
  /// The point clip it's parented to.
  parent: number | null;
  /// Its own colour on the timeline (#rrggbb), if one was chosen.
  label: string | null;
}

export interface TrackView {
  index: number;
  name: string;
  kind: "video" | "audio";
  muted: boolean;
  locked: boolean;
  audio: TrackAudio;
  clips: ClipView[];
  /// Row height in the timeline, and whether it's collapsed to a thin row.
  height: number;
  collapsed: boolean;
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
  /// The highlighted track: where Paste and new layers go.
  target_track: number | null;
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

export type Interp = "Linear" | "Ease" | "Hold" | "Bezier";

export interface KeyView {
  at: Ticks;
  value: number;
  interp: Interp;
  ease_in: [number, number];
  ease_out: [number, number];
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
  interp_here: Interp | null;
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

/** Which parts of a tracked motion are applied. */
export interface TrackAxes {
  x: boolean;
  y: boolean;
  rotation: boolean;
  scale: boolean;
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
  mesh_keys: number | null;
  /// A camera, light or point: its position / point of interest, where the camera sees them.
  scene_points: { which: "position" | "target"; at: Pt }[];
  /// A plane's colour (#rrggbb).
  plane: string | null;
  /// The point clip it's parented to, and the ones it can be: [id, name].
  parent: number | null;
  parents: [number, string][];
  /// Viewer line from the clip to its parent point (frame pixels), when both show.
  parent_link: [[number, number], [number, number]] | null;
  tracking: number | null;
  tracking_elsewhere: boolean;
}

export type Pt = [number, number];

export interface Mask {
  name: string;
  shape: "Rectangle" | "Ellipse" | "Freehand";
  points: Pt[];
  keys: { at: Ticks; points: Pt[]; curves?: [number, number, number, number][] }[];
  mode: "Add" | "Subtract" | "Intersect";
  inverted: boolean;
  feather: number;
  expansion: number;
  opacity: number;
  enabled: boolean;
  /** Bézier handles per point: [in x, in y, out x, out y] offsets (layer px). */
  curves?: [number, number, number, number][];
  /** Feather per point (layer px); negative or missing = the mask's feather. */
  feathers?: number[];
}

export type Curve4 = [number, number, number, number];

export interface MaskView {
  mask: Mask;
  outline: Pt[];
  handles: Pt[];
  curves: Curve4[];
  key_here: boolean;
}

export type TrackMode = "Position" | "Transform" | "CornerPin" | "Planar" | "Mesh";

export interface TrackSource {
  id: number;
  name: string;
  track: string;
  tracker: TrackerView;
}

export interface TrackerView {
  mode: TrackMode;
  size: number;
  search: number;
  shown: Pt[];
  paths: Pt[][];
  frames: number;
  range: [Ticks, Ticks] | null;
  tracked: boolean;
  targets: { clip: number; mask: number | null; blade: number | null; point: boolean; label: string }[];
  grid: [number, number];
  /// Smoothing in frames, and each point's smoothed path (empty when 0).
  smooth: number;
  smoothed: Pt[][];
}

export interface Prefs {
  use_proxies: boolean;
  auto_proxies: boolean;
  proxy_format: ProxyFormat;
  autosave_minutes: number;
  render_cache_mb: number;
  export_presets: UserPreset[];
  scrub_audio: boolean;
  timeline_thumbnails: boolean;
  viewer_snapping: boolean;
  hw_decode: boolean;
  cache_mb: number;
  native_decoders: boolean;
  zero_copy: boolean;
  start_screen: boolean;
}

export interface SettingsInfo {
  hw_api: string;
  hw_status: string;
  zero_copy_status: string;
  cache_frames: number;
  cache_mb: number;
  cache_hits: number;
  render_frames: number;
  render_mb: number;
  proxies: number;
  proxy_bytes: number;
  proxy_dir: string;
  autosave_dir: string;
  native: string[];
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
  three_d: boolean;
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

export type EffectKind = "ColorCorrect" | "Blur" | "ChromaKey" | "LumaKey" | "DifferenceKey" | "Vignette" | "Levels" | "Curves" | "Sharpen" | "Glow" | "DropShadow" | "Lut" | "Plugin" | "OpenFx" | "EnergyBlade" | "Particles" | "LensFlare" | "CameraShake" | "ChromaticAberration" | "FilmGrain" | "Glitch" | "LightLeaks";

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
  ui: "number" | "toggle" | "choice" | "color";
  /// A heading to show above this parameter.
  section: string | null;
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
  /// A difference key's clean plate: [file, timecode in it].
  plate: [string, string] | null;
  plate_error: string | null;
  animated: boolean;
  /// Positions to drag in the Viewer (an OpenFX plugin's): x at `param`, y at `param + 1`.
  points: { param: number; label: string }[];
  /// Why it isn't rendering (an OpenFX plugin's error).
  error: string | null;
  temporal: boolean;
  gpu: string[];
  /// What it rendered with last ("OpenCL on …", "the processor").
  rendered_with: string | null;
}

export interface CatalogItem {
  kind: EffectKind;
  plugin: string | null;
  label: string;
  description: string;
  author: string;
  version: string;
}

/// A saved effect preset (a stack of effects with their settings and keyframes).
export interface FxPreset {
  name: string;
  /// "Glow, Blur · 2 keyframes".
  summary: string;
  effects: number;
  /// Plugins it uses that aren't installed.
  missing: string[];
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

export interface PresetView {
  id: string;
  group: string;
  name: string;
  detail: string;
  width: number;
  height: number;
  summary: string;
}

export interface RecentView {
  path: string;
  name: string;
  folder: string;
  summary: string;
  at: number;
  exists: boolean;
  thumb: boolean;
  open: boolean;
}

export interface UserTemplate {
  name: string;
  path: string;
  summary: string;
  sequences: number;
  media: number;
}

export interface StartView {
  presets: PresetView[];
  recent: RecentView[];
  templates: UserTemplate[];
  templates_dir: string;
  at_startup: boolean;
  dirty: boolean;
  has_project: boolean;
}

export interface OfxView {
  enabled: boolean;
  gpu: boolean;
  gpu_status: string;
  gpu_ok: boolean;
  /// Each way of rendering on the graphics card: name, what on (or why not), works.
  gpus: [string, string, boolean][];
  scanning: boolean;
  folders: { path: string; place: string; exists: boolean; removable: boolean }[];
  bundles: { path: string; name: string; effects: { name: string; group: string; version: string; params: number; temporal: boolean; gpu: string[]; gpu_only: boolean }[]; errors: string[]; blocked: boolean }[];
  own_folder: string;
}

// ---- Export ----

export type VideoCodec =
  | "H264" | "H265"
  | "ProRes422Proxy" | "ProRes422Lt" | "ProRes422" | "ProRes422Hq" | "ProRes4444" | "ProRes4444Xq"
  | "DnxhrLb" | "DnxhrSq" | "DnxhrHq" | "DnxhrHqx" | "Dnxhr444" | "Dnxhd" | "Dnxhd10"
  | "Cineform" | "Cineform4444"
  | "PngSequence" | "JpegSequence" | "TiffSequence";
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
  /// A transparent background (codecs with alpha only).
  alpha?: boolean;
}

export interface UserPreset {
  name: string;
  spec: Spec;
  include_audio: boolean;
}

export interface ExportOptions {
  codecs: {
    id: VideoCodec;
    label: string;
    /// The heading it's listed under ("Apple ProRes", …).
    group: string;
    hint: string;
    extension: string;
    /// File types it can be written in, the usual one first (the extension picks the container).
    extensions: string[];
    sequence: boolean;
    quality: boolean;
    bitrate: boolean;
    ten_bit: boolean;
    hdr: boolean;
    hardware: boolean;
    /// Can keep a transparent background.
    alpha: boolean;
    /// Always more than 8 bits per channel.
    deep: boolean;
  }[];
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
  trackers: TrackSource[];
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
  add3dText: () => invoke("add_3d_text"),
  addBladeLayer: () => invoke("add_blade_layer"),
  addParticlesLayer: () => invoke("add_particles_layer"),
  /// A camera, or a light (0 point, 1 spot, 2 directional, 3 ambient).
  addScene: (camera: boolean, light: number) => invoke("add_scene", { camera, light }),
  /// The New menu: a plane ("layer", of `color`), a light, a point, a camera or an adjustment layer.
  addNew: (what: "layer" | "light" | "point" | "camera" | "adjustment", light = 0, color: string | null = null) => invoke("add_new", { what, light, color }),
  setPlaneColor: (id: number, color: string) => invoke("set_plane_color", { id, color }),
  setClipLabel: (color: string | null) => invoke("set_clip_label", { color }),
  setParent: (id: number, parent: number | null) => invoke("set_parent", { id, parent }),
  /// Drag a camera's or light's point by `d` frame pixels (live; `start` on the first call).
  scenePointDrag: (id: number, which: string, d: Pt, start: boolean) => invoke("scene_point_drag", { id, which, d, start }),
  select: (id: number | null, mode: "only" | "toggle" | "none") => invoke("select", { id, mode }),
  selectMany: (ids: number[], add: boolean) => invoke("select_many", { ids, add }),
  moveClips: (lead: number, track: number, start: Ticks) => invoke("move_clips", { lead, track, start }),
  trim: (id: number, edge: "start" | "end", mode: string, delta: Ticks) => invoke<number | null>("trim", { id, edge, mode, delta }),
  commit: () => invoke("commit"),
  action: (name: string) => invoke("action", { name }),
  historyGoTo: (pos: number) => invoke("history_go_to", { pos }),
  seek: (t: Ticks) => invoke("seek", { t: Math.max(0, Math.round(t)) }),
  trackOp: (
    index: number,
    op: "mute" | "lock" | "delete" | "rename" | "duplicate" | "target" | "untarget" | "collapse" | "height" | "collapse_all" | "expand_all" | "reset_heights" | "collapse_every" | "expand_every",
    name?: string,
    to?: number,
  ) => invoke("track_op", { index, op, name, to }),
  moveTrack: (index: number, to: number) => invoke("track_op", { index, op: "move", to }),
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
  autoDuck: (dialogue: number[], music: number[], amountDb: number, thresholdDb: number, fadeDown: number, fadeUp: number, hold: number) =>
    invoke("auto_duck", { dialogue, music, amountDb, thresholdDb, fadeDown, fadeUp, hold }),
  removeDucking: (music: number[]) => invoke("remove_ducking", { music }),
  setMaskCurves: (id: number, index: number, curves: Curve4[]) => invoke("set_mask_curves", { id, index, curves }),
  maskPointCurve: (id: number, index: number, point: number) => invoke("mask_point_curve", { id, index, point }),
  maskPointAdd: (id: number, index: number, point: number, before: boolean) => invoke<number | null>("mask_point_add", { id, index, point, before }),
  maskPointDelete: (id: number, index: number, point: number) => invoke<boolean>("mask_point_delete", { id, index, point }),
  maskOp: (id: number, index: number, op: "animate" | "still" | "key" | "unkey") => invoke("mask_op", { id, index, op }),
  trackerAdd: (id: number, mode: TrackMode) => invoke("tracker_add", { id, mode }),
  trackerMode: (id: number, mode: TrackMode) => invoke("tracker_mode", { id, mode }),
  trackerSizes: (id: number, size: number, search: number) => invoke("tracker_sizes", { id, size, search }),
  trackerSmooth: (id: number, frames: number) => invoke("tracker_smooth", { id, frames }),
  trackerGrid: (id: number, cols: number, rows: number) => invoke("tracker_grid", { id, cols, rows }),
  meshRemove: (id: number) => invoke("mesh_remove", { id }),
  trackerMove: (id: number, point: number, pos: Pt) => invoke("tracker_move", { id, point, pos }),
  trackerOp: (id: number, op: "clear" | "remove") => invoke("tracker_op", { id, op }),
  trackStart: (id: number, forward: boolean) => invoke("track_start", { id, forward }),
  trackCancel: () => invoke("track_cancel"),
  maskTrack: (id: number, index: number, forward: boolean, motion: MaskMotion) => invoke("mask_track", { id, index, forward, motion }),
  trackApply: (id: number, target: number, mask: number | null, blade: number | null, smoothing: number, axes?: TrackAxes) => invoke("track_apply", { id, target, mask, blade, smoothing, axes }),
  exportOptions: () => invoke<ExportOptions>("export_options"),
  diagnostics: () => invoke<string>("diagnostics"),
  notices: () => invoke<string>("notices"),
  openLogFolder: () => invoke("open_log_folder"),
  checkDecoders: () => invoke<DecoderCheck[]>("check_decoders"),
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
  bladePreset: (id: number, index: number, preset: number) => invoke("blade_preset", { id, index, preset }),
  particlesPreset: (id: number, index: number, preset: number) => invoke("particles_preset", { id, index, preset }),
  bladeIgnite: (id: number, index: number, retract: boolean, seconds: number) => invoke("blade_ignite", { id, index, retract, seconds }),
  effectCurves: (id: number, index: number, curves: Curves) => invoke("effect_curves", { id, index, curves }),
  effectLut: (id: number, index: number) => invoke("effect_lut", { id, index }),
  effectKeyPick: (id: number, index: number, a: Pt, b: Pt) => invoke<string>("effect_key_pick", { id, index, a, b }),
  effectPlate: (id: number, index: number, file: boolean) => invoke("effect_plate", { id, index, file }),
  effectCatalog: () => invoke<CatalogGroup[]>("effect_catalog"),
  fxPresetList: () => invoke<FxPreset[]>("fx_preset_list"),
  fxPresetSave: (id: number, name: string) => invoke<string>("fx_preset_save", { id, name }),
  fxPresetApply: (id: number, name: string, replace: boolean) => invoke("fx_preset_apply", { id, name, replace }),
  fxPresetRename: (from: string, to: string) => invoke("fx_preset_rename", { from, to }),
  fxPresetDelete: (name: string) => invoke("fx_preset_delete", { name }),
  pluginsList: () => invoke<PluginsView>("plugins_list"),
  pluginsInstall: (zip: boolean) => invoke<string | null>("plugins_install", { zip }),
  pluginsNew: () => invoke<string>("plugins_new"),
  pluginsRemove: (folder: string) => invoke<string>("plugins_remove", { folder }),
  pluginsReload: () => invoke<string>("plugins_reload"),
  pluginsLive: (on: boolean) => invoke("plugins_live", { on }),
  openFolder: (path: string) => invoke("open_folder", { path }),
  ofxList: () => invoke<OfxView>("ofx_list"),
  ofxRescan: () => invoke<string>("ofx_rescan"),
  ofxEnable: (on: boolean) => invoke<string>("ofx_enable", { on }),
  ofxAddFolder: () => invoke<string | null>("ofx_add_folder"),
  ofxRemoveFolder: (path: string) => invoke<string>("ofx_remove_folder", { path }),
  ofxUnblock: (path: string) => invoke<string>("ofx_unblock", { path }),
  ofxOpenFolder: () => invoke("ofx_open_folder"),
  ofxGpu: (on: boolean) => invoke<string>("ofx_gpu", { on }),
  startView: () => invoke<StartView>("start_view"),
  startAtLaunch: () => invoke<boolean>("start_at_launch"),
  startNewPreset: (id: string) => invoke("start_new_preset", { id }),
  startNewCustom: (width: number, height: number, num: number, den: number) => invoke("start_new_custom", { width, height, num, den }),
  startNewTemplate: (path: string) => invoke("start_new_template", { path }),
  startOpenRecent: (path: string) => invoke<boolean>("start_open_recent", { path }),
  startForget: (path: string | null) => invoke("start_forget", { path }),
  startThumb: (path: string) => invoke<ArrayBuffer>("start_thumb", { path }),
  saveAsTemplate: (name: string) => invoke<string>("save_as_template", { name }),
  templateRemove: (path: string) => invoke("template_remove", { path }),
  startScreenSetting: (on: boolean) => invoke("start_screen_setting", { on }),
  transformKey: (id: number, prop: string) => invoke("transform_key", { id, prop }),
  keyMove: (id: number, channel: string, index: number, at: Ticks, value: number, keepTime: boolean) => invoke<number | null>("key_move", { id, channel, index, at: Math.round(at), value, keepTime }),
  keyInterp: (id: number, channel: string, index: number, interp: Interp) => invoke("key_interp", { id, channel, index, interp }),
  keyHandle: (id: number, channel: string, index: number, out: boolean, x: number, y: number) => invoke("key_handle", { id, channel, index, out, x, y }),
  keyDelete: (id: number, channel: string, index: number) => invoke("key_delete", { id, channel, index }),
  keyAdd: (id: number, channel: string, at: Ticks) => invoke<number | null>("key_add", { id, channel, at: Math.round(at) }),
  markerColors: () => invoke<[MarkerColor, string][]>("marker_colors"),
  markerAdd: () => invoke("marker_add"),
  markerSet: (index: number, at: Ticks, name: string, color: MarkerColor) => invoke("marker_set", { index, at: Math.round(at), name, color }),
  markerRemove: (index: number) => invoke("marker_remove", { index }),
  proxyMake: (media: number) => invoke("proxy_make", { media }),
  proxyDelete: (media: number) => invoke("proxy_delete", { media }),
  proxiesDeleteAll: () => invoke("proxies_delete_all"),
  proxiesRemake: (format: ProxyFormat) => invoke<number>("proxies_remake", { format }),
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
  transformInterp: (id: number, interp: Interp) => invoke("transform_interp", { id, interp }),
  attrOffers: () => invoke<AttrOffers | null>("attr_offers"),
  pasteAttributes: (set: AttrSet) => invoke("paste_attributes", { set }),
};

/// One format's result from Help → Check decoders.
export interface DecoderCheck {
  format: string;
  status: "ok" | "mismatch" | "error" | "skipped";
  psnr: number | null;
  psnr_portable: number | null;
  frames: number;
  detail: string;
}
