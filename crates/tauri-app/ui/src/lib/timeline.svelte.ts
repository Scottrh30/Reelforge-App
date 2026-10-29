// Timeline view state (zoom, scroll, header width) and the geometry other
// panels need, e.g. to drop files from the desktop at the right place.

import type { ClipView, TrackView } from "./api";

/// A track row's default height; rows can be resized (MIN_TRACK_H–MAX_TRACK_H) or collapsed.
export const TRACK_H = 52;
export const MIN_TRACK_H = 32;
export const MAX_TRACK_H = 240;
export const COLLAPSED_H = 22;

/// How tall a track's row is on screen.
export function rowH(tr: { height?: number; collapsed?: boolean }): number {
  return tr.collapsed ? COLLAPSED_H : tr.height || TRACK_H;
}

/// The row at `y` (pixels from the top of the first row), given each row's top and the end.
export function rowAtY(tops: number[], y: number): number {
  if (tops.length < 2) return 0;
  let lo = 0,
    hi = tops.length - 2;
  while (lo < hi) {
    const mid = (lo + hi + 1) >> 1;
    if (tops[mid] <= y) lo = mid;
    else hi = mid - 1;
  }
  return lo;
}
export const RULER_H = 28;
export const MIN_PPS = 2;
export const MAX_PPS = 2000;
export const MIN_HEADER = 110;
export const MAX_HEADER = 420;

function storedHeader(): number {
  try {
    const v = Number(localStorage.getItem("timelineHeader"));
    return v >= MIN_HEADER && v <= MAX_HEADER ? v : 140;
  } catch {
    return 140;
  }
}

export const view = $state({
  /// Pixels per second of the sequence.
  pps: 60,
  /// Seconds scrolled off the left edge.
  scroll: 0,
  /// Pixels the tracks are scrolled down.
  vscroll: 0,
  headerW: storedHeader(),
  /// Fit the sequence to the width next time the timeline is drawn.
  fit: true,
});

/// Zoom and scroll of each sequence, to come back to.
export const perSequence = new Map<number, { pps: number; scroll: number; vscroll: number }>();

/// The lanes' screen rectangle and the track rows, kept up to date by the Timeline.
export const geo = {
  lanes: null as DOMRect | null,
  rows: [] as TrackView[],
  /// Each row's top (pixels from the first row's top), and the bottom of the last.
  tops: [0] as number[],
};

/// Which track (index) and time (seconds) are at screen point `x`, `y`, if over the tracks.
export function hitTrack(x: number, y: number): { track: number | null; secs: number } | null {
  const r = geo.lanes;
  if (!r || x < r.left || x > r.right || y < r.top || y > r.bottom) return null;
  const yy = y - r.top + view.vscroll;
  if (yy >= geo.tops[geo.tops.length - 1]) return { track: null, secs: Math.max(0, view.scroll + (x - r.left) / view.pps) };
  const row = rowAtY(geo.tops, yy);
  const secs = Math.max(0, view.scroll + (x - r.left) / view.pps);
  return { track: geo.rows[row]?.index ?? null, secs };
}

export function zoomAround(pps: number, anchorPx: number) {
  const anchor = view.scroll + anchorPx / view.pps;
  view.pps = Math.min(MAX_PPS, Math.max(MIN_PPS, pps));
  view.scroll = Math.max(0, anchor - anchorPx / view.pps);
}

export function clipEnd(c: ClipView) {
  return c.start + c.duration;
}

/// A tick spacing for the ruler giving labels at least `minPx` apart.
export function rulerStep(pps: number, fps: number, minPx = 90): number {
  const frame = 1 / fps;
  const steps = [frame, 2 * frame, 5 * frame, 10 * frame, 0.5, 1, 2, 5, 10, 15, 30, 60, 120, 300, 600, 1800, 3600];
  return steps.find((s) => s * pps >= minPx) ?? 3600;
}
