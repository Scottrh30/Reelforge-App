// Clip audio maths, matching reelforge_core::audio: volume and pan keyframes
// are anchored to the clip's source time (source_in + time into the clip).
import type { ClipAudio } from "./api";

export const MIN_DB = -60;
export const MAX_DB = 12;

export type VolumeKey = { at: number; db: number };
export type PanKey = { at: number; pan: number };

export const volKeys = (a: ClipAudio) => (a.volume_keys as VolumeKey[] | undefined) ?? [];
export const panKeys = (a: ClipAudio) => (a.pan_keys as PanKey[] | undefined) ?? [];

function interp<K extends { at: number }>(keys: K[], t: number, v: (k: K) => number, flat: number): number {
  if (!keys.length) return flat;
  let i = 0;
  while (i < keys.length && keys[i].at <= t) i++;
  if (i === 0) return v(keys[0]);
  if (i === keys.length) return v(keys[i - 1]);
  const a = keys[i - 1],
    b = keys[i];
  if (b.at === a.at) return v(b);
  return v(a) + (v(b) - v(a)) * ((t - a.at) / (b.at - a.at));
}

export const volumeAt = (a: ClipAudio, src: number) => interp(volKeys(a), src, (k) => k.db, a.volume_db);
export const panAt = (a: ClipAudio, src: number) => interp(panKeys(a), src, (k) => k.pan, a.pan);

export function setVolKey(a: ClipAudio, at: number, db: number) {
  const keys = volKeys(a).slice();
  db = Math.max(MIN_DB, Math.min(MAX_DB, db));
  const k = keys.find((k) => k.at === at);
  if (k) k.db = db;
  else {
    let i = 0;
    while (i < keys.length && keys[i].at < at) i++;
    keys.splice(i, 0, { at, db });
  }
  a.volume_keys = keys;
}

export function setPanKey(a: ClipAudio, at: number, pan: number) {
  const keys = panKeys(a).slice();
  pan = Math.max(-1, Math.min(1, pan));
  const k = keys.find((k) => k.at === at);
  if (k) k.pan = pan;
  else {
    let i = 0;
    while (i < keys.length && keys[i].at < at) i++;
    keys.splice(i, 0, { at, pan });
  }
  a.pan_keys = keys;
}

/// Fades limited so they fit in the clip and can't overlap.
export function clampedFades(a: ClipAudio, duration: number): [number, number] {
  const d = Math.max(0, duration);
  const fin = Math.min(Math.max(0, a.fade_in), d);
  const fout = Math.min(Math.max(0, a.fade_out), d - fin);
  return [fin, fout];
}

/// Decibels to amplitude (MIN_DB and below: silence).
export const dbGain = (db: number) => (db <= MIN_DB ? 0 : Math.pow(10, db / 20));

/// Equal-power fade curve, 0..1.
export const fadeCurve = (x: number) => Math.sin(Math.max(0, Math.min(1, x)) * (Math.PI / 2));

/// Whole tenths of a dB, and 0 dB when close.
export const tidyDb = (db: number) => (Math.abs(db) < 0.6 ? 0 : Math.round(db * 10) / 10);

export const dbText = (db: number) => (db <= MIN_DB ? "−∞ dB" : `${db > 0 ? "+" : ""}${db.toFixed(1)} dB`);
export const panText = (p: number) => (Math.abs(p) < 0.005 ? "C" : p < 0 ? `L${Math.round(-p * 100)}` : `R${Math.round(p * 100)}`);

export const defaultAudio = (): ClipAudio => ({ volume_db: 0, pan: 0, fade_in: 0, fade_out: 0, volume_keys: [], crossfade_in: 0, pan_keys: [] });
