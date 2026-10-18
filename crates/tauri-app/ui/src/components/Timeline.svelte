<script lang="ts">
  import { untrack } from "svelte";
  import { app, run, tps, fps, timecode, shortTime, snapToFrame, say } from "../lib/session.svelte";
  import { MIN_DB, MAX_DB, volKeys, volumeAt, setVolKey, clampedFades, fadeCurve, tidyDb, dbText, dbGain } from "../lib/audio";
  import { api, type ClipAudio, type ClipView, type TrackView, type MarkerColor, type TransitionMark, type Transition, type TransitionKind } from "../lib/api";
  import { livePump } from "../lib/live";
  import { openMenu } from "../lib/menu.svelte";
  import { view, geo, perSequence, TRACK_H, RULER_H, MIN_PPS, MAX_PPS, MIN_HEADER, MAX_HEADER, zoomAround, clipEnd, rulerStep, hitTrack } from "../lib/timeline.svelte";
  import Thumb from "./Thumb.svelte";
  import CurvesPanel from "./CurvesPanel.svelte";

  const SNAP_PX = 8;
  const EDGE_PX = 6;

  const s = $derived(app.s);
  const seq = $derived(s?.seq);
  // Highest video track at the top, then the audio tracks.
  const rows = $derived.by<TrackView[]>(() => {
    const t = seq?.tracks ?? [];
    return [...t.filter((x) => x.kind === "video").reverse(), ...t.filter((x) => x.kind === "audio")];
  });
  const selected = $derived(new Set(s?.selected ?? []));
  const T = $derived(tps());
  const mediaById = $derived(new Map((s?.media ?? []).map((m) => [m.id, m])));

  let lanesEl: HTMLDivElement | undefined = $state();
  let lanesW = $state(800);
  let lanesH = $state(200);
  let measured = $state(false);

  $effect(() => {
    if (!lanesEl) return;
    const ro = new ResizeObserver(() => {
      const r = lanesEl!.getBoundingClientRect();
      lanesW = r.width;
      lanesH = r.height;
      geo.lanes = r;
      measured = r.width > 50;
    });
    ro.observe(lanesEl);
    return () => ro.disconnect();
  });
  $effect(() => {
    geo.rows = rows;
  });

  // Remember zoom/scroll per sequence; fit a sequence seen for the first time.
  let shownSeq = -1;
  $effect(() => {
    const a = s?.active ?? 0;
    untrack(() => {
      if (a === shownSeq) return;
      if (shownSeq >= 0) perSequence.set(shownSeq, { pps: view.pps, scroll: view.scroll, vscroll: view.vscroll });
      shownSeq = a;
      const v = perSequence.get(a);
      if (v) Object.assign(view, v);
      else view.fit = true;
    });
  });
  $effect(() => {
    if (view.fit && seq && measured) {
      untrack(() => {
        const secs = Math.max(1, seq.duration / T);
        view.pps = Math.min(MAX_PPS, Math.max(MIN_PPS, (lanesW - 24) / secs));
        view.scroll = 0;
        view.fit = false;
      });
    }
  });

  const x = (t: number) => (t / T - view.scroll) * view.pps;
  const tAt = (px: number) => Math.max(0, (view.scroll + px / view.pps) * T);
  const maxV = $derived(Math.max(0, rows.length * TRACK_H + 40 - lanesH));
  $effect(() => {
    if (view.vscroll > maxV) view.vscroll = maxV;
  });

  // Keep the playhead in view while playing.
  $effect(() => {
    const live = app.live;
    if (!live?.playing) return;
    const px = (live.playhead / T - view.scroll) * view.pps;
    if (px > lanesW - 30 || px < 0) untrack(() => (view.scroll = Math.max(0, live.playhead / T - 30 / view.pps)));
  });

  // ---- Snapping -------------------------------------------------------------

  function snapTargets(exclude: Set<number>): number[] {
    const out: number[] = [app.live?.playhead ?? 0];
    for (const tr of seq?.tracks ?? []) for (const c of tr.clips) if (!exclude.has(c.id)) out.push(c.start, clipEnd(c));
    if (seq?.in_point != null) out.push(seq.in_point);
    if (seq?.out_point != null) out.push(seq.out_point);
    for (const m of seq?.markers ?? []) out.push(m.at);
    return out;
  }
  /// The nearest target to time `t` within SNAP_PX, or null.
  function snapTo(t: number, targets: number[]): number | null {
    if (!app.snapping) return null;
    let best: number | null = null;
    let bestPx = SNAP_PX;
    for (const g of targets) {
      const d = Math.abs((g - t) / T) * view.pps;
      if (d < bestPx) {
        bestPx = d;
        best = g;
      }
    }
    return best;
  }

  // ---- Gestures -------------------------------------------------------------

  type Gesture =
    | { kind: "move"; lead: ClipView; leadKind: "video" | "audio"; dt: number; drow: number; ids: Set<number> }
    | { kind: "box"; x0: number; y0: number; x1: number; y1: number }
    | { kind: "scrub" }
    | null;
  let gesture = $state<Gesture>(null);
  let snapLine = $state<number | null>(null);

  function local(e: PointerEvent | MouseEvent) {
    const r = lanesEl!.getBoundingClientRect();
    return { px: e.clientX - r.left, py: e.clientY - r.top + view.vscroll };
  }

  function clipDown(e: PointerEvent, c: ClipView, tr: TrackView, rowIndex: number) {
    if (e.button !== 0) return;
    e.stopPropagation();
    const el = lanesEl!;
    el.setPointerCapture(e.pointerId);
    const { px } = local(e);
    const left = x(c.start);
    const right = x(clipEnd(c));
    const ctrl = e.ctrlKey || e.metaKey;
    const tool = app.tool;
    const trimTool = tool === "select" || tool === "ripple" || tool === "roll";
    const edge: "start" | "end" | null = trimTool && !tr.locked ? (px - left < EDGE_PX && right - left > 3 * EDGE_PX ? "start" : right - px < EDGE_PX ? "end" : null) : null;

    if (!selected.has(c.id) || ctrl) run(api.select(c.id, ctrl ? "toggle" : "only"));
    if (ctrl) return;

    const x0 = e.clientX;
    const y0 = e.clientY;
    let started = false;
    let pending = false;
    let latest: (() => Promise<unknown>) | null = null;
    // Send live edits one at a time, always the newest.
    const pump = async () => {
      if (pending) return;
      while (latest) {
        const f = latest;
        latest = null;
        pending = true;
        try {
          await f();
        } finally {
          pending = false;
        }
      }
    };
    const mode = tool === "select" ? "normal" : tool;
    const exclude = new Set([c.id, ...(s?.selected ?? [])]);
    const targets = snapTargets(exclude);
    const edgeAt = edge === "end" ? clipEnd(c) : c.start;

    const move = (ev: PointerEvent) => {
      if (!started && Math.abs(ev.clientX - x0) < 3 && Math.abs(ev.clientY - y0) < 3) return;
      started = true;
      let dt = snapToFrame(((ev.clientX - x0) / view.pps) * T);
      snapLine = null;
      if (edge) {
        if (!ev.altKey) {
          const g = snapTo(edgeAt + dt, targets);
          if (g !== null) {
            dt = g - edgeAt;
            snapLine = g;
          }
        }
        latest = () => api.trim(c.id, edge, mode, dt);
        pump();
      } else if (tool === "slip" || tool === "slide") {
        const d = tool === "slip" ? -dt : dt;
        latest = () => api.trim(c.id, "start", tool, d).then(() => api.seek(c.start));
        pump();
      } else if (!tr.locked) {
        if (!ev.altKey) {
          const a = snapTo(c.start + dt, targets);
          const b = snapTo(clipEnd(c) + dt, targets);
          if (a !== null) {
            dt = a - c.start;
            snapLine = a;
          } else if (b !== null) {
            dt = b - clipEnd(c);
            snapLine = b;
          }
        }
        if (c.start + dt < 0) dt = -c.start;
        const { py } = local(ev);
        const row = Math.max(0, Math.min(rows.length - 1, Math.floor(py / TRACK_H)));
        const drow = rows[row].kind === tr.kind ? row - rowIndex : 0;
        gesture = { kind: "move", lead: c, leadKind: tr.kind, dt, drow, ids: new Set(s?.selected ?? [c.id]) };
      }
    };
    const up = async () => {
      el.removeEventListener("pointermove", move);
      el.removeEventListener("pointerup", up);
      snapLine = null;
      if (edge || tool === "slip" || tool === "slide") {
        while (pending || latest) await new Promise((r) => setTimeout(r, 5));
        if (started) await run(api.commit());
        return;
      }
      const g = gesture;
      gesture = null;
      if (g?.kind === "move" && (g.dt !== 0 || g.drow !== 0)) {
        const target = rows[rowIndex + g.drow];
        await run(api.moveClips(c.id, target.index, c.start + g.dt));
      }
    };
    el.addEventListener("pointermove", move);
    el.addEventListener("pointerup", up);
  }

  function lanesDown(e: PointerEvent) {
    if (e.button !== 0) return;
    const el = lanesEl!;
    el.setPointerCapture(e.pointerId);
    const a = local(e);
    gesture = { kind: "box", x0: a.px, y0: a.py, x1: a.px, y1: a.py };
    const add = e.ctrlKey || e.metaKey;
    const move = (ev: PointerEvent) => {
      const b = local(ev);
      if (gesture?.kind === "box") gesture = { ...gesture, x1: b.px, y1: b.py };
    };
    const up = async () => {
      el.removeEventListener("pointermove", move);
      el.removeEventListener("pointerup", up);
      const g = gesture;
      gesture = null;
      if (g?.kind !== "box") return;
      if (Math.abs(g.x1 - g.x0) < 4 && Math.abs(g.y1 - g.y0) < 4) {
        if (!add) await run(api.select(null, "none"));
        return;
      }
      const [t0, t1] = [tAt(Math.min(g.x0, g.x1)), tAt(Math.max(g.x0, g.x1))];
      const [r0, r1] = [Math.floor(Math.min(g.y0, g.y1) / TRACK_H), Math.floor(Math.max(g.y0, g.y1) / TRACK_H)];
      const ids: number[] = [];
      rows.forEach((tr, i) => {
        if (i < r0 || i > r1) return;
        for (const c of tr.clips) if (c.start < t1 && clipEnd(c) > t0) ids.push(c.id);
      });
      await run(api.selectMany(ids, add));
    };
    el.addEventListener("pointermove", move);
    el.addEventListener("pointerup", up);
  }

  function rulerDown(e: PointerEvent) {
    if (e.button !== 0) return;
    const el = e.currentTarget as HTMLElement;
    el.setPointerCapture(e.pointerId);
    const r = el.getBoundingClientRect();
    const targets = snapTargets(new Set());
    const seekTo = (cx: number, alt: boolean) => {
      let t = snapToFrame(tAt(cx - r.left));
      if (!alt) t = snapTo(t, targets.slice(1)) ?? t;
      api.seek(t);
    };
    gesture = { kind: "scrub" };
    seekTo(e.clientX, e.altKey);
    const move = (ev: PointerEvent) => seekTo(ev.clientX, ev.altKey);
    const up = () => {
      gesture = null;
      el.removeEventListener("pointermove", move);
      el.removeEventListener("pointerup", up);
    };
    el.addEventListener("pointermove", move);
    el.addEventListener("pointerup", up);
  }

  function wheel(e: WheelEvent) {
    e.preventDefault();
    const r = lanesEl!.getBoundingClientRect();
    if (e.ctrlKey || e.metaKey) {
      zoomAround(view.pps * Math.exp(-e.deltaY * 0.0025), Math.max(0, e.clientX - r.left));
    } else if (e.shiftKey || Math.abs(e.deltaX) > Math.abs(e.deltaY) || maxV === 0) {
      const d = Math.abs(e.deltaX) > Math.abs(e.deltaY) ? e.deltaX : e.deltaY;
      view.scroll = Math.max(0, view.scroll + d / view.pps);
    } else {
      view.vscroll = Math.min(maxV, Math.max(0, view.vscroll + e.deltaY));
    }
  }

  // ---- Media dragged in from the Media panel ----
  const dropAt = $derived.by(() => {
    const d = app.dragMedia;
    if (!d) return null;
    const h = hitTrack(d.x, d.y);
    if (!h) return null;
    return { ...h, t: snapToFrame(h.secs * T) };
  });
  function windowUp() {
    const d = app.dragMedia;
    const at = dropAt;
    if (d && at) {
      const tr = seq?.tracks[at.track ?? -1];
      const m = mediaById.get(d.id);
      // A matching track under the pointer is used; otherwise the first free one.
      const ok = tr && m && ((tr.kind === "video" && m.kind !== "audio") || (tr.kind === "audio" && m.kind === "audio"));
      run(api.addToTimeline(d.id, ok ? at.track : null, at.t));
    }
  }

  // ---- Header width ----
  function headerResize(e: PointerEvent) {
    const el = e.currentTarget as HTMLElement;
    el.setPointerCapture(e.pointerId);
    const x0 = e.clientX;
    const w0 = view.headerW;
    const move = (ev: PointerEvent) => (view.headerW = Math.min(MAX_HEADER, Math.max(MIN_HEADER, w0 + ev.clientX - x0)));
    const up = () => {
      el.removeEventListener("pointermove", move);
      el.removeEventListener("pointerup", up);
      try {
        localStorage.setItem("timelineHeader", String(Math.round(view.headerW)));
      } catch {}
    };
    el.addEventListener("pointermove", move);
    el.addEventListener("pointerup", up);
  }

  // ---- Tracks ----
  let renaming = $state<number | null>(null);
  function trackMenu(e: MouseEvent, tr: TrackView) {
    openMenu(e, [
      { heading: true, label: tr.name },
      { label: "Rename track…", run: () => (renaming = tr.index) },
      { label: tr.muted ? (tr.kind === "video" ? "Show track" : "Unmute track") : tr.kind === "video" ? "Hide track" : "Mute track", run: () => run(api.trackOp(tr.index, "mute")) },
      { label: tr.locked ? "Unlock track" : "Lock track", run: () => run(api.trackOp(tr.index, "lock")) },
      { separator: true },
      { label: "Add video track", run: () => run(api.action("add_video_track")) },
      { label: "Add audio track", run: () => run(api.action("add_audio_track")) },
      { separator: true },
      { label: tr.kind === "video" ? "Delete video track" : "Delete audio track", run: () => run(api.trackOp(tr.index, "delete")) },
    ]);
  }
  function clipMenu(e: MouseEvent, c: ClipView) {
    if (!selected.has(c.id)) run(api.select(c.id, "only"));
    openMenu(e, [
      { heading: true, label: c.name },
      { label: "Split at playhead", hint: "S", run: () => run(api.action("split")) },
      { label: "Cut", hint: "Ctrl+X", run: () => run(api.action("cut")) },
      { label: "Copy", hint: "Ctrl+C", run: () => run(api.action("copy")) },
      { label: "Delete", hint: "Del", run: () => run(api.action("delete")) },
      { label: "Ripple delete", hint: "Shift+Del", run: () => run(api.action("ripple_delete")) },
      { separator: true },
      { label: "Unlink audio and video", disabled: c.link == null, run: () => run(api.action("unlink")) },
      { label: "Open composition", disabled: c.kind !== "comp", run: () => openCompFromClip(c) },
      { separator: true },
      ...(touchesBefore(c)
        ? [
            c.kind === "audio"
              ? { label: c.dissolve_in || hasXfade(c) ? "Remove crossfade" : "Crossfade from the clip before", run: () => (hasXfade(c) ? run(api.transitionRemove(c.id)) : setXfadeOn(c.id)) }
              : {
                  label: "Transition from the clip before",
                  submenu: [
                    ...kinds.map((k) => ({ label: k.label, run: () => run(api.transitionApply(k.kind, c.id)) })),
                    ...(hasXfade(c) ? [{ separator: true }, { label: "Remove transition", run: () => run(api.transitionRemove(c.id)) }] : []),
                  ],
                },
          ]
        : []),
      ...(c.kind === "video" || c.kind === "audio" || c.kind === "comp"
        ? [
            {
              label: "Speed",
              submenu: [
                ...[0.25, 0.5, 1, 2, 4].map((sp) => ({ label: `${sp * 100}%`, checked: !c.reverse && Math.abs(c.speed - sp) < 1e-3, run: () => run(api.setSpeed(c.id, sp, false)) })),
                { separator: true },
                { label: "Reverse", checked: c.reverse, run: () => run(api.setSpeed(c.id, c.speed || 1, !c.reverse)) },
                { label: "Freeze frame at playhead", hint: "Shift+F", run: () => run(api.action("freeze_frame")) },
              ],
            },
          ]
        : []),
    ]);
  }
  /// Does another clip end exactly where this one starts on its track?
  function touchesBefore(c: ClipView): boolean {
    const tr = seq?.tracks.find((t) => t.clips.some((x) => x.id === c.id));
    return !!tr?.clips.some((x) => x.id !== c.id && clipEnd(x) === c.start);
  }
  const hasXfade = (c: ClipView) => !!seq?.transitions.some((t) => t.right === c.id);
  async function setXfadeOn(id: number) {
    await api.transitionLength(id, snapToFrame(tps()));
    await run(api.commit());
  }
  function openCompFromClip(c: ClipView) {
    const i = s?.sequences.findIndex((q) => q.name === c.name) ?? -1;
    if (i > 0) {
      app.timelineTab = "comp";
      run(api.openSequence(i));
    }
  }

  // ---- Tabs ----
  async function showTab(tab: "edit" | "comp" | "curves") {
    app.timelineTab = tab;
    if (tab === "curves") return;
    if (tab === "edit") await run(api.openSequence(0));
    else if ((s?.sequences.length ?? 0) > 1) await run(api.openSequence(s!.comp_open || 1));
  }
  // The tab follows the sequence being edited (e.g. opened from the Media panel).
  // (Only when the sequence changes: the Curves tab stays open while editing.)
  let tabFor = -1;
  $effect(() => {
    const a = s?.active ?? 0;
    untrack(() => {
      if (a !== tabFor) {
        if (tabFor !== -1 || app.timelineTab !== "curves") app.timelineTab = a === 0 ? "edit" : "comp";
        tabFor = a;
      }
    });
  });

  // ---- Transitions ----
  // ---- Volume line, keys and fades on audio clips ----
  // The band the line moves in, inside the clip (40 px high): below the name.
  const VOL_TOP = 15,
    VOL_BOTTOM = 36,
    FADE_H = 8;
  const dbY = (db: number) => VOL_BOTTOM - ((Math.max(MIN_DB, Math.min(MAX_DB, db)) - MIN_DB) / (MAX_DB - MIN_DB)) * (VOL_BOTTOM - VOL_TOP);
  const yDb = (y: number) => Math.max(MIN_DB, Math.min(MAX_DB, MIN_DB + ((VOL_BOTTOM - y) / (VOL_BOTTOM - VOL_TOP)) * (MAX_DB - MIN_DB)));
  // x inside the clip of source time `at`.
  const srcX = (c: ClipView, at: number) => ((at - c.source_in) / T) * view.pps;
  function volPoints(c: ClipView, w: number): string {
    const a = c.audio!;
    const pts = [`0,${dbY(volumeAt(a, c.source_in))}`];
    for (const k of volKeys(a)) {
      const px = srcX(c, k.at);
      if (px > 0 && px < w) pts.push(`${px},${dbY(k.db)}`);
    }
    pts.push(`${w},${dbY(volumeAt(a, c.source_in + c.duration))}`);
    return pts.join(" ");
  }
  function fadePath(c: ClipView, out: boolean): string {
    const [fi, fo] = clampedFades(c.audio!, c.duration);
    const len = out ? fo : fi;
    const fw = (len / T) * view.pps;
    if (fw < 1) return "";
    const w = (c.duration / T) * view.pps;
    const steps = Math.max(4, Math.min(48, Math.round(fw / 3)));
    let d = "";
    for (let i = 0; i <= steps; i++) {
      const u = i / steps;
      const px = out ? w - fw + u * fw : u * fw;
      const g = out ? fadeCurve(1 - u) : fadeCurve(u);
      d += `${i ? "L" : "M"}${px.toFixed(1)},${(40 - g * 40).toFixed(1)}`;
    }
    // Close along the top edge: the shaded, faded-out part.
    return d + (out ? `L${w},0L${w - fw},0Z` : `L${fw},0L0,0Z`);
  }
  const volPump = livePump();
  /// A gesture on the volume line, a key, or a fade handle of audio clip `c`.
  function volDown(e: PointerEvent, c: ClipView, tr: TrackView, what: "line" | "key" | "fadein" | "fadeout", index = -1) {
    if (e.button !== 0 || !c.audio || tr.locked) return;
    e.stopPropagation();
    const el = e.currentTarget as Element;
    const clipEl = el.closest(".clip") as HTMLElement;
    const box = clipEl.getBoundingClientRect();
    const orig = $state.snapshot(c.audio) as ClipAudio;
    if (!selected.has(c.id)) run(api.select(c.id, "only"));
    const frameT = T / fps();
    const atX = (cx: number) => snapToFrame(c.start + ((cx - box.left) / view.pps) * T);
    if (what === "key" && (e.altKey || e.detail >= 2)) {
      const n = structuredClone(orig);
      n.volume_keys = volKeys(n).filter((_, i) => i !== index);
      run(api.setClipAudio(c.id, n).then(() => api.commit()));
      say("Removed the volume keyframe");
      return;
    }
    if (what === "line" && (e.ctrlKey || e.metaKey)) {
      const n = structuredClone(orig);
      const src = c.source_in + Math.max(0, Math.min(c.duration, atX(e.clientX) - c.start));
      setVolKey(n, src, volKeys(n).length ? volumeAt(n, src) : n.volume_db);
      run(api.setClipAudio(c.id, n).then(() => api.commit()));
      say("Added a volume keyframe");
      return;
    }
    (el as HTMLElement).setPointerCapture?.(e.pointerId);
    const y0 = e.clientY - box.top;
    let moved = false;
    const move = (ev: PointerEvent) => {
      moved = true;
      const n = structuredClone(orig);
      const y = ev.clientY - box.top;
      if (what === "fadein" || what === "fadeout") {
        const [fi, fo] = clampedFades(orig, c.duration);
        const t = atX(ev.clientX);
        if (what === "fadeout") n.fade_out = Math.max(0, Math.min(c.duration - fi, c.start + c.duration - t));
        else n.fade_in = Math.max(0, Math.min(c.duration - fo, t - c.start));
        say(`Fade ${what === "fadeout" ? "out" : "in"}: ${((what === "fadeout" ? n.fade_out : n.fade_in) / T).toFixed(2)} s`);
      } else if (what === "key") {
        const keys = volKeys(n).map((k) => ({ ...k }));
        const lo = index > 0 ? keys[index - 1].at + frameT : c.source_in;
        const hi = index + 1 < keys.length ? keys[index + 1].at - frameT : c.source_in + c.duration;
        const at = Math.max(lo, Math.min(Math.max(lo, hi), c.source_in + (atX(ev.clientX) - c.start)));
        const db = tidyDb(yDb(y));
        keys[index] = { at: Math.round(at), db };
        n.volume_keys = keys;
        say(`Volume key: ${db > 0 ? "+" : ""}${db.toFixed(1)} dB at ${timecode(c.start + (at - c.source_in))}`);
      } else {
        const delta = yDb(y) - yDb(y0);
        if (volKeys(n).length) {
          n.volume_keys = volKeys(n).map((k) => ({ at: k.at, db: tidyDb(Math.max(MIN_DB, Math.min(MAX_DB, k.db + delta))) }));
          say(`Volume keys ${delta >= 0 ? "+" : ""}${delta.toFixed(1)} dB`);
        } else {
          n.volume_db = tidyDb(Math.max(MIN_DB, Math.min(MAX_DB, orig.volume_db + delta)));
          say(`Clip volume: ${n.volume_db > 0 ? "+" : ""}${n.volume_db.toFixed(1)} dB`);
        }
      }
      volPump.push(() => api.setClipAudio(c.id, n));
    };
    const up = async () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      if (!moved) return;
      await volPump.done();
      await run(api.commit());
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
  }

  const xfadePump = livePump();
  let kinds = $state<TransitionKind[]>([]);
  $effect(() => {
    api.transitionList().then((k) => (kinds = k));
  });

  /// Drag either end of a transition: its length grows or shrinks about the cut.
  function xfadeEdge(e: PointerEvent, tx: TransitionMark) {
    if (e.button !== 0) return;
    e.stopPropagation();
    const el = e.currentTarget as HTMLElement;
    el.setPointerCapture(e.pointerId);
    const r = lanesEl!.getBoundingClientRect();
    const move = (ev: PointerEvent) => {
      const t = tAt(ev.clientX - r.left);
      const len = snapToFrame(2 * Math.abs(t - tx.cut));
      xfadePump.push(() => api.transitionLength(tx.right, len));
    };
    const up = async () => {
      el.removeEventListener("pointermove", move);
      el.removeEventListener("pointerup", up);
      await xfadePump.done();
      await run(api.commit());
    };
    el.addEventListener("pointermove", move);
    el.addEventListener("pointerup", up);
  }

  function xfadeMenu(e: MouseEvent, tx: TransitionMark) {
    e.preventDefault();
    e.stopPropagation();
    const T1 = tps();
    const len = (tx.requested / T1).toFixed(2);
    const groups = [...new Set(kinds.map((k) => k.group))];
    openMenu(e, [
      { heading: true, label: `${tx.label} · ${len} s` },
      ...(tx.audio
        ? []
        : groups.map((g) => ({
            label: g,
            submenu: kinds.filter((k) => k.group === g).map((k) => ({ label: k.label, checked: k.label === tx.label, run: () => run(api.transitionKind(tx.right, k.kind as Transition)) })),
          }))),
      { label: "Half a second", run: () => setXfade(tx, 0.5) },
      { label: "One second", run: () => setXfade(tx, 1) },
      { label: "Two seconds", run: () => setXfade(tx, 2) },
      { separator: true },
      { label: "Select the clip after it", run: () => run(api.select(tx.right, "only")) },
      { label: tx.audio ? "Remove crossfade" : "Remove transition", run: () => run(api.transitionRemove(tx.right)) },
    ]);
  }
  async function setXfade(tx: TransitionMark, secs: number) {
    await api.transitionLength(tx.right, snapToFrame(secs * tps()));
    await run(api.commit());
  }

  // ---- Markers ----
  const markerPump = livePump();
  let colors = $state<[MarkerColor, string][]>([]);
  $effect(() => {
    api.markerColors().then((c) => (colors = c));
  });

  /// Drag a marker along the ruler (snapping like clips); a click without moving goes to it.
  function markerDown(e: PointerEvent, i: number) {
    if (e.button !== 0 || !seq) return;
    e.stopPropagation();
    const el = e.currentTarget as HTMLElement;
    el.setPointerCapture(e.pointerId);
    const mk = seq.markers[i];
    const ruler = el.parentElement!.getBoundingClientRect();
    const targets = snapTargets(new Set()).filter((t) => t !== mk.at);
    const x0 = e.clientX;
    let moved = false;
    const move = (ev: PointerEvent) => {
      if (!moved && Math.abs(ev.clientX - x0) < 3) return;
      moved = true;
      let t = snapToFrame(tAt(ev.clientX - ruler.left));
      if (!ev.altKey) t = snapTo(t, targets) ?? t;
      markerPump.push(() => api.markerSet(i, t, mk.name, mk.color));
    };
    const up = async () => {
      el.removeEventListener("pointermove", move);
      el.removeEventListener("pointerup", up);
      if (moved) {
        await markerPump.done();
        await run(api.commit());
      } else api.seek(mk.at);
    };
    el.addEventListener("pointermove", move);
    el.addEventListener("pointerup", up);
  }

  function markerMenu(e: MouseEvent, i: number) {
    const mk = seq?.markers[i];
    if (!mk) return;
    openMenu(e, [
      { heading: true, label: `${mk.name || "Marker"} · ${timecode(mk.at)}` },
      { label: "Rename…", run: () => (app.markerEdit = i) },
      { label: "Colour", submenu: colors.map(([c]) => ({ label: c, checked: mk.color === c, run: () => setMarker(i, { color: c }, true) })) },
      { label: "Go to marker", run: () => api.seek(mk.at) },
      { separator: true },
      { label: "Delete marker", run: () => run(api.markerRemove(i)) },
    ]);
  }

  async function setMarker(i: number, ch: Partial<{ name: string; color: MarkerColor }>, done: boolean) {
    const mk = seq?.markers[i];
    if (!mk) return;
    await api.markerSet(i, mk.at, ch.name ?? mk.name, ch.color ?? mk.color);
    if (done) await run(api.commit());
  }
  let rulerEl = $state<HTMLDivElement | null>(null);
  // Rendered preview: a green bar along the top of the ruler (frames as runs).
  let rendered = $state<[number, number][]>([]);
  $effect(() => {
    void app.live?.render_gen;
    void s?.rev;
    void s?.active;
    const step = Math.max(1, Math.floor((2 * fps()) / Math.max(1, view.pps)));
    const timer = setTimeout(() => api.renderedRuns(step).then((r) => (rendered = r)).catch(() => {}), 150);
    return () => clearTimeout(timer);
  });
  let markerName = $state("");
  $effect(() => {
    const i = app.markerEdit;
    if (i != null) untrack(() => (markerName = seq?.markers[i]?.name ?? ""));
  });
  async function closeMarkerEdit(keep: boolean) {
    const i = app.markerEdit;
    app.markerEdit = null;
    if (i == null) return;
    if (keep) await setMarker(i, { name: markerName.trim() }, true);
  }

  // ---- Ruler ----
  const ticks = $derived.by(() => {
    if (!seq) return [];
    const step = rulerStep(view.pps, fps());
    const first = Math.floor(view.scroll / step) * step;
    const out: { px: number; label: string }[] = [];
    for (let t = first; (t - view.scroll) * view.pps < lanesW + 100; t += step) {
      if (t < 0) continue;
      out.push({ px: (t - view.scroll) * view.pps, label: timecode(Math.round(t * T)) });
    }
    return out;
  });

  const playX = $derived(x(app.live?.playhead ?? 0));
  const tools: [typeof app.tool, string, string, string][] = [
    ["select", "Select", "V", "Select and trim: dragging an edge changes just that clip"],
    ["ripple", "Ripple", "B", "Ripple: trimming an edge moves every later clip to keep them butted up"],
    ["roll", "Roll", "N", "Roll: drag a cut between two clips; one gets longer, the other shorter"],
    ["slip", "Slip", "Y", "Slip: drag a clip to change which part of its media it shows"],
    ["slide", "Slide", "U", "Slide: drag a clip along; the clips either side adjust to fit"],
  ];

  // ---- Waveforms ----
  // Scaled by the clip's volume (and keys), its fades and the track volume, so
  // it shows what you'll hear; square-rooted so quiet passages stay visible.
  function wave(canvas: HTMLCanvasElement, p: { media: number; from: number; secs: number; w: number; h: number; speed: number; rev: number; audio: ClipAudio | null; srcIn: number; dur: number; offset: number; trackDb: number }) {
    const draw = (q: typeof p) => {
      const data = app.waveforms.get(q.media);
      const dpr = window.devicePixelRatio || 1;
      canvas.width = Math.max(1, Math.round(q.w * dpr));
      canvas.height = Math.max(1, Math.round(q.h * dpr));
      const g = canvas.getContext("2d")!;
      g.clearRect(0, 0, canvas.width, canvas.height);
      if (!data) return;
      g.fillStyle = "rgba(170, 235, 190, 0.55)";
      const rate = app.waveformRate;
      const mid = canvas.height / 2;
      for (let i = 0; i < canvas.width; i++) {
        const s0 = (q.from + (i / canvas.width) * q.secs * q.speed) * rate;
        const s1 = (q.from + ((i + 1) / canvas.width) * q.secs * q.speed) * rate;
        let peak = 0;
        for (let k = Math.floor(s0); k <= Math.max(Math.floor(s0), Math.floor(s1)) && k < data.length; k++) if (k >= 0 && data[k] > peak) peak = data[k];
        // Gain at this column: seconds into the clip -> its source time.
        let gain = dbGain(q.trackDb);
        const a = q.audio;
        if (a) {
          const into = q.offset + (i / canvas.width) * q.secs;
          gain *= dbGain(volumeAt(a, q.srcIn + into * T));
          const [fi, fo] = clampedFades(a, q.dur);
          if (fi > 0) gain *= fadeCurve((into * T) / fi);
          if (fo > 0) gain *= fadeCurve((q.dur - into * T) / fo);
        }
        const h = Math.sqrt(Math.min(1, (peak / 255) * gain)) * mid;
        g.fillRect(i, mid - h, 1, Math.max(1, 2 * h));
      }
    };
    draw(p);
    return { update: draw };
  }

  function zoomStep(f: number) {
    const live = app.live;
    const px = live ? x(live.playhead) : lanesW / 2;
    zoomAround(view.pps * f, px >= 0 && px <= lanesW ? px : lanesW / 2);
  }
</script>

<svelte:window onpointerup={windowUp} />

<div class="timeline" style="--hw:{view.headerW}px">
  <div class="top">
    <div class="tabs" role="tablist">
      <button role="tab" aria-selected={app.timelineTab === "edit"} class:active={app.timelineTab === "edit"} onclick={() => showTab("edit")}>Editor Timeline</button>
      <button role="tab" aria-selected={app.timelineTab === "comp"} class:active={app.timelineTab === "comp"} onclick={() => showTab("comp")}>Composition Timeline</button>
      <button role="tab" aria-selected={app.timelineTab === "curves"} class:active={app.timelineTab === "curves"} onclick={() => showTab("curves")} title="The selected clip's keyframes as graphs">Curves</button>
    </div>
    {#if app.timelineTab === "comp"}
      <div class="comps">
        {#each (s?.sequences ?? []).slice(1) as c, i}
          <button class="chip" class:on={s?.active === i + 1} onclick={() => run(api.openSequence(i + 1))}>{c.name}</button>
        {/each}
        <button class="chip" onclick={() => run(api.newComposite())} title="New empty composition">+</button>
      </div>
    {/if}
    <span class="grow"></span>
    {#if app.timelineTab !== "curves"}
    <div class="tools" role="radiogroup" aria-label="Edit tool">
      {#each tools as [id, label, key, tip]}
        <button role="radio" aria-checked={app.tool === id} class:on={app.tool === id} onclick={() => (app.tool = id)} title="{tip} ({key})">{label}</button>
      {/each}
    </div>
    <button class:on={app.snapping} onclick={() => (app.snapping = !app.snapping)} title="Snap to cuts, the playhead and markers (hold Alt to drag freely)">Snap</button>
    <button onclick={() => run(api.action("split"))} title="Split at the playhead (S)">Split</button>
    {/if}
  </div>

  {#if app.timelineTab === "curves"}
    <CurvesPanel />
  {:else if app.timelineTab === "comp" && (s?.sequences.length ?? 0) < 2}
    <div class="empty">
      <p><strong>No compositions yet.</strong></p>
      <p class="muted">A composition is a shot built from layers (footage, titles, effects) on its own timeline, then used in your edit like one clip.</p>
      <p>Right-click a clip in the Media panel and choose New composition from media, or</p>
      <button class="primary" onclick={() => run(api.newComposite())}>New composition</button>
    </div>
  {:else if seq}
    <div class="grid">
      <div class="corner">
        <span class="muted tc">{timecode(app.live?.playhead ?? 0)}</span>
      </div>
      <div class="ruler" bind:this={rulerEl} role="slider" aria-label="Playhead" aria-valuenow={app.live?.playhead ?? 0} tabindex="-1" onpointerdown={rulerDown} onwheel={wheel}>
        {#if seq.in_point != null || seq.out_point != null}
          {@const a = x(seq.in_point ?? 0)}
          {@const b = x(seq.out_point ?? seq.duration)}
          <div class="marks" style="left:{a}px; width:{Math.max(0, b - a)}px"></div>
        {/if}
        {#each rendered as [a, b]}
          {@const xa = x((a / fps()) * T)}
          <div class="rendered" style="left:{xa}px; width:{Math.max(1, x((b / fps()) * T) - xa)}px" title="Rendered preview"></div>
        {/each}
        {#each ticks as tk}
          <span class="tick" style="left:{tk.px}px"></span>
          <span class="tlabel tc" style="left:{tk.px + 4}px">{tk.label}</span>
        {/each}
        {#each seq.markers as mk, i}
          <span
            class="marker"
            role="button"
            tabindex="-1"
            style="left:{x(mk.at)}px; --mc:{mk.rgb}"
            title="{mk.name || 'Marker'} · {timecode(mk.at)} — drag to move, double-click to rename, right-click for more"
            onpointerdown={(e) => markerDown(e, i)}
            ondblclick={(e) => (e.stopPropagation(), (app.markerEdit = i))}
            oncontextmenu={(e) => (e.preventDefault(), e.stopPropagation(), markerMenu(e, i))}
          ></span>
          {#if mk.name}<span class="mlabel" style="left:{x(mk.at) + 7}px; --mc:{mk.rgb}">{mk.name}</span>{/if}
        {/each}
        {#if app.markerEdit != null && seq.markers[app.markerEdit]}
          {@const mk = seq.markers[app.markerEdit]}
          <div class="medit" style="left:{(rulerEl?.getBoundingClientRect().left ?? 0) + Math.max(4, x(mk.at) - 20)}px; top:{(rulerEl?.getBoundingClientRect().bottom ?? 0) + 4}px" role="dialog" aria-label="Marker" tabindex="-1" onpointerdown={(e) => e.stopPropagation()}>
            <!-- svelte-ignore a11y_autofocus -->
            <input
              type="text"
              bind:value={markerName}
              autofocus
              aria-label="Marker name"
              placeholder="Marker name"
              onkeydown={(e) => {
                if (e.key === "Enter") closeMarkerEdit(true);
                if (e.key === "Escape") closeMarkerEdit(false);
                e.stopPropagation();
              }}
            />
            <div class="swatches">
              {#each colors as [c, rgb]}
                <button class="sw" class:on={mk.color === c} style="background:{rgb}" title={c} aria-label={c} onclick={() => setMarker(app.markerEdit!, { color: c }, true)}></button>
              {/each}
              <span class="grow"></span>
              <button class="small" onclick={() => (run(api.markerRemove(app.markerEdit!)), (app.markerEdit = null))}>Delete</button>
              <button class="small primary" onclick={() => closeMarkerEdit(true)}>Done</button>
            </div>
          </div>
        {/if}
        <span class="head" style="left:{playX}px"></span>
      </div>

      <div class="headers">
        <div class="hstack" style="transform: translateY({-view.vscroll}px)">
          {#each rows as tr (tr.index)}
            {@const lv = app.live?.tracks[tr.index]}
            <div class="th" class:muted-track={tr.muted} oncontextmenu={(e) => trackMenu(e, tr)} role="group" aria-label={tr.name}>
              {#if renaming === tr.index}
                <!-- svelte-ignore a11y_autofocus -->
                <input
                  type="text"
                  value={tr.name}
                  autofocus
                  aria-label="Track name"
                  onblur={(e) => (run(api.trackOp(tr.index, "rename", e.currentTarget.value.trim())), (renaming = null))}
                  onkeydown={(e) => {
                    if (e.key === "Enter") e.currentTarget.blur();
                    if (e.key === "Escape") renaming = null;
                  }}
                />
              {:else}
                <span class="tname" role="presentation" ondblclick={() => (renaming = tr.index)} title="Double-click to rename, right-click for more">{tr.name}</span>
              {/if}
              <div class="tbtns">
                <button class="tb" class:lit={tr.kind === "video" ? !tr.muted : tr.muted} onclick={() => run(api.trackOp(tr.index, "mute"))} title={tr.kind === "video" ? "Show / hide track" : "Mute track"} aria-label={tr.kind === "video" ? "Show track" : "Mute track"} aria-pressed={tr.kind === "video" ? !tr.muted : tr.muted}>
                  {#if tr.kind === "video"}
                    <svg viewBox="0 0 16 16" width="12" height="12"><path d="M1 8s2.6-4.5 7-4.5S15 8 15 8s-2.6 4.5-7 4.5S1 8 1 8z" fill="none" stroke="currentColor" stroke-width="1.4" /><circle cx="8" cy="8" r="2" fill="currentColor" /></svg>
                  {:else}M{/if}
                </button>
                <button class="tb lock" class:lit={tr.locked} onclick={() => run(api.trackOp(tr.index, "lock"))} title={tr.locked ? "Unlock track" : "Lock track"} aria-label="Lock track" aria-pressed={tr.locked}>
                  <svg viewBox="0 0 16 16" width="12" height="12"><rect x="3" y="7" width="10" height="7" rx="1.5" fill="currentColor" /><path d={tr.locked ? "M5 7V5a3 3 0 0 1 6 0v2" : "M5 7V5a3 3 0 0 1 6 0"} fill="none" stroke="currentColor" stroke-width="1.5" /></svg>
                </button>
                {#if tr.kind === "audio"}
                  <div class="tmeter" title="{tr.name} level">
                    {#each [0, 1] as ch}
                      <span style="width:{lv ? Math.max(0, Math.min(100, ((lv[ch] + 60) / 60) * 100)) : 0}%" class:hot={lv && lv[ch] > -6}></span>
                    {/each}
                  </div>
                {/if}
              </div>
            </div>
          {/each}
          <div class="addrow">
            <button onclick={() => run(api.action("add_video_track"))} title="Add a video track">+ Video</button>
            <button onclick={() => run(api.action("add_audio_track"))} title="Add an audio track">+ Audio</button>
          </div>
        </div>
        <span class="hresize" role="separator" aria-orientation="vertical" onpointerdown={headerResize} ondblclick={() => (view.headerW = 140)} title="Drag to resize the track names, double-click to reset"></span>
      </div>

      <div class="lanes" bind:this={lanesEl} onpointerdown={lanesDown} onwheel={wheel} role="presentation">
        <div class="stack" style="transform: translateY({-view.vscroll}px)">
          {#each rows as tr, ri (tr.index)}
            <div class="lane" class:locked={tr.locked} class:dropping={dropAt?.track === tr.index}>
              {#each tr.clips as c (c.id)}
                {@const l = x(c.start)}
                {@const w = (c.duration / T) * view.pps}
                {#if l + w > -50 && l < lanesW + 50}
                  {@const ghost = gesture?.kind === "move" && gesture.ids.has(c.id)}
                  <div
                    class="clip {c.kind}"
                    class:sel={selected.has(c.id)}
                    class:moving={ghost}
                    class:dim={tr.muted}
                    style="left:{l}px; width:{Math.max(2, w)}px"
                    role="button"
                    tabindex="-1"
                    aria-label={c.name}
                    onpointerdown={(e) => clipDown(e, c, tr, ri)}
                    oncontextmenu={(e) => clipMenu(e, c)}
                    ondblclick={() => c.kind === "comp" && openCompFromClip(c)}
                  >
                    {#if (c.kind === "video" || c.kind === "image") && w > 70 && app.s?.prefs.timeline_thumbnails !== false && mediaById.get(c.media) && !mediaById.get(c.media)!.missing}
                      <div class="thumb"><Thumb path={mediaById.get(c.media)!.path} at={c.source_in} /></div>
                    {/if}
                    {#if c.kind === "audio" && w > 4}
                      {@const vis0 = Math.max(0, -l)}
                      {@const vis1 = Math.min(w, lanesW - l)}
                      {#if vis1 > vis0}
                        <canvas
                          class="wave"
                          style="left:{vis0}px; width:{vis1 - vis0}px"
                          use:wave={{ media: c.media, from: c.source_in / T + (vis0 / view.pps) * c.speed, secs: (vis1 - vis0) / view.pps, w: vis1 - vis0, h: TRACK_H - 14, speed: c.speed, rev: app.waveforms.size, audio: c.audio, srcIn: c.source_in, dur: c.duration, offset: vis0 / view.pps, trackDb: tr.audio.volume_db }}
                        ></canvas>
                      {/if}
                    {/if}
                    {#if c.kind === "audio" && c.audio && w > 6}
                      {@const [fi, fo] = clampedFades(c.audio, c.duration)}
                      {@const vw = Math.max(2, w)}
                      <svg class="vol" width={vw} height="40" aria-hidden="true">
                        <path class="fadeshade" d={fadePath(c, false)} />
                        <path class="fadeshade" d={fadePath(c, true)} />
                        <polyline role="presentation" class="volhit" points={volPoints(c, vw)} onpointerdown={(e) => volDown(e, c, tr, "line")}><title>Volume {dbText(volumeAt(c.audio, c.source_in))}: drag up or down; Ctrl+click to add a keyframe</title></polyline>
                        <polyline class="volline" points={volPoints(c, vw)} />
                        {#each volKeys(c.audio) as k, i}
                          {@const kx = srcX(c, k.at)}
                          {#if kx >= -5 && kx <= vw + 5}
                            <path role="presentation" class="volkey" d="M{kx} {dbY(k.db) - 4.5}l4.5 4.5-4.5 4.5-4.5-4.5Z" onpointerdown={(e) => volDown(e, c, tr, "key", i)}><title>{dbText(k.db)} — drag to move, Alt+click or double-click to remove</title></path>
                          {/if}
                        {/each}
                        {#if w > 24}
                          <rect role="presentation" class="fadeh" x={Math.max(0.5, Math.min(vw - FADE_H - 0.5, (fi / T) * view.pps - FADE_H / 2))} y="1" width={FADE_H} height={FADE_H} rx="1.5" onpointerdown={(e) => volDown(e, c, tr, "fadein")}><title>Fade in {(fi / T).toFixed(2)} s — drag</title></rect>
                          <rect role="presentation" class="fadeh" x={Math.max(0.5, Math.min(vw - FADE_H - 0.5, vw - (fo / T) * view.pps - FADE_H / 2))} y="1" width={FADE_H} height={FADE_H} rx="1.5" onpointerdown={(e) => volDown(e, c, tr, "fadeout")}><title>Fade out {(fo / T).toFixed(2)} s — drag</title></rect>
                        {/if}
                      </svg>
                    {/if}
                    <span class="label" style="padding-left:{Math.max(6, -l + 6)}px">
                      {#if c.kind === "comp"}<b>Comp</b>{/if}{c.name}{#if c.speed !== 1 || c.reverse}<i> {c.reverse ? "−" : ""}{Math.round(c.speed * 100)}%</i>{/if}{#if c.effects}<i> fx {c.effects}</i>{/if}
                    </span>
                  </div>
                  {#if ghost && gesture?.kind === "move"}
                    <div
                      class="clip {c.kind} ghost"
                      style="left:{x(c.start + gesture.dt)}px; width:{Math.max(2, w)}px; transform: translateY({tr.kind === gesture.leadKind ? gesture.drow * TRACK_H : 0}px)"
                    >
                      <span class="label">{c.name}</span>
                    </div>
                  {/if}
                {/if}
              {/each}
              {#each seq.transitions.filter((t) => t.track === tr.index) as tx (tx.right)}
                {#if tx.empty}
                  <span class="xcut" style="left:{x(tx.cut)}px" title="{tx.label}: no spare media at this cut, so it plays as a plain cut. Trim the clips to leave unused media either side."></span>
                {:else}
                  {@const xl = x(tx.start)}
                  {@const xw = Math.max(6, x(tx.end) - xl)}
                  <div
                    class="xfade"
                    class:audio={tx.audio}
                    style="left:{xl}px; width:{xw}px"
                    role="button"
                    tabindex="-1"
                    title="{tx.label}, {((tx.end - tx.start) / T).toFixed(2)} s{tx.end - tx.start < tx.requested ? ' (limited by spare media)' : ''} — drag an end to change its length, right-click for more"
                    onpointerdown={(e) => (e.stopPropagation(), e.button === 0 && run(api.select(tx.right, "only")))}
                    oncontextmenu={(e) => xfadeMenu(e, tx)}
                  >
                    <svg viewBox="0 0 10 10" preserveAspectRatio="none" aria-hidden="true"><path d="M0 0 L10 10 M0 10 L10 0" /></svg>
                    {#if xw > 56}<span class="xlabel">{xw > 110 ? `${tx.label} ` : ""}{((tx.end - tx.start) / T).toFixed(1)}s</span>{/if}
                    <span class="xedge l" role="presentation" onpointerdown={(e) => xfadeEdge(e, tx)}></span>
                    <span class="xedge r" role="presentation" onpointerdown={(e) => xfadeEdge(e, tx)}></span>
                  </div>
                {/if}
              {/each}
            </div>
          {/each}
        </div>
        {#if gesture?.kind === "box"}
          <div
            class="box"
            style="left:{Math.min(gesture.x0, gesture.x1)}px; top:{Math.min(gesture.y0, gesture.y1) - view.vscroll}px; width:{Math.abs(gesture.x1 - gesture.x0)}px; height:{Math.abs(gesture.y1 - gesture.y0)}px"
          ></div>
        {/if}
        {#if snapLine !== null}<div class="snapline" style="left:{x(snapLine)}px"></div>{/if}
        {#if dropAt}<div class="dropline" style="left:{x(dropAt.t)}px"><span>{app.dragMedia?.name}</span></div>{/if}
        {#each seq.markers as mk}
          <div class="mline" style="left:{x(mk.at)}px; --mc:{mk.rgb}"></div>
        {/each}
        <div class="playhead" style="left:{playX}px"></div>
        {#if rows.every((r) => r.clips.length === 0)}
          <div class="lanehint">Drag clips here from the Media panel, or double-click one to add it</div>
        {/if}
      </div>
    </div>

    <div class="zoombar">
      <span class="muted tc">{shortTime((lanesW / view.pps) * T)} in view</span>
      <span class="grow"></span>
      <span class="muted">Zoom</span>
      <button class="ghost z" onclick={() => zoomStep(1 / 1.5)} title="Zoom out (Ctrl + wheel)" aria-label="Zoom out">−</button>
      <input
        type="range"
        min={Math.log(MIN_PPS)}
        max={Math.log(MAX_PPS)}
        step="0.01"
        value={Math.log(view.pps)}
        aria-label="Timeline zoom"
        oninput={(e) => {
          const px = x(app.live?.playhead ?? 0);
          zoomAround(Math.exp(+e.currentTarget.value), px >= 0 && px <= lanesW ? px : lanesW / 2);
        }}
      />
      <button class="ghost z" onclick={() => zoomStep(1.5)} title="Zoom in (Ctrl + wheel)" aria-label="Zoom in">+</button>
      <button onclick={() => (view.fit = true)} title="Show the whole sequence">Fit</button>
    </div>
  {/if}
</div>

{#if app.dragMedia}
  <div class="dragghost" style="left:{app.dragMedia.x + 12}px; top:{app.dragMedia.y + 8}px">{app.dragMedia.name}</div>
{/if}

<style>
  .timeline {
    height: 100%;
    display: flex;
    flex-direction: column;
    min-height: 0;
  }
  .top {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 5px 8px;
    flex-wrap: wrap;
  }
  .grow {
    flex: 1;
  }
  .tabs {
    display: flex;
    gap: 2px;
  }
  .tabs button {
    background: none;
    border: 0;
    border-bottom: 2px solid transparent;
    border-radius: 0;
    color: var(--muted);
    padding: 3px 10px;
  }
  .tabs button.active {
    color: var(--text);
    border-bottom-color: var(--accent);
  }
  .comps {
    display: flex;
    gap: 4px;
    margin-left: 8px;
  }
  .chip {
    min-height: 22px;
    padding: 0 8px;
    font-size: calc(12px * var(--text-scale));
    border-radius: 11px;
  }
  .tools {
    display: flex;
  }
  .tools button {
    border-radius: 0;
    margin-left: -1px;
    padding: 3px 9px;
    position: relative;
  }
  /* The buttons overlap by their 1px border; lift the chosen one (and the one
     pointed at) so its whole outline shows, right edge included. */
  .tools button:hover {
    z-index: 1;
  }
  .tools button.on {
    z-index: 2;
  }
  .tools button:first-child {
    border-radius: var(--radius) 0 0 var(--radius);
  }
  .tools button:last-child {
    border-radius: 0 var(--radius) var(--radius) 0;
  }
  .empty {
    padding: 30px;
    text-align: center;
  }
  .empty p {
    margin: 6px auto;
    max-width: 60ch;
  }

  .grid {
    flex: 1;
    min-height: 0;
    display: grid;
    grid-template-columns: var(--hw) 1fr;
    grid-template-rows: 28px 1fr;
    border-top: 1px solid var(--line-soft);
  }
  .corner {
    display: flex;
    align-items: center;
    padding: 0 10px;
    border-right: 1px solid var(--line);
    border-bottom: 1px solid var(--line-soft);
    font-size: calc(12px * var(--text-scale));
  }
  .ruler {
    position: relative;
    overflow: hidden;
    border-bottom: 1px solid var(--line-soft);
    background: var(--panel);
    cursor: text;
  }
  .rendered {
    position: absolute;
    top: 0;
    height: 3px;
    background: rgb(70, 200, 100);
    pointer-events: none;
  }
  .tick {
    position: absolute;
    bottom: 0;
    height: 8px;
    width: 1px;
    background: var(--faint);
  }
  .tlabel {
    position: absolute;
    top: 5px;
    font-size: calc(11px * var(--text-scale));
    color: var(--muted);
    white-space: nowrap;
  }
  .marks {
    position: absolute;
    top: 0;
    bottom: 0;
    background: var(--accent-soft);
    border-left: 1px solid var(--accent);
    border-right: 1px solid var(--accent);
  }
  .marker {
    position: absolute;
    bottom: 1px;
    width: 10px;
    height: 10px;
    margin-left: -5px;
    background: var(--mc);
    transform: rotate(45deg);
    cursor: ew-resize;
    z-index: 2;
    border: 1px solid rgba(0, 0, 0, 0.35);
  }
  .marker:hover {
    filter: brightness(1.2);
  }
  .mlabel {
    position: absolute;
    bottom: 0;
    font-size: calc(10.5px * var(--text-scale));
    color: var(--mc);
    white-space: nowrap;
    pointer-events: none;
    max-width: 140px;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .mline {
    position: absolute;
    top: 0;
    bottom: 0;
    width: 0;
    border-left: 1px dashed color-mix(in srgb, var(--mc) 55%, transparent);
    pointer-events: none;
  }
  .medit {
    position: fixed;
    z-index: 30;
    width: 240px;
    padding: 8px;
    background: var(--raised);
    border: 1px solid var(--line);
    border-radius: 6px;
    box-shadow: 0 10px 30px rgba(0, 0, 0, 0.5);
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .swatches {
    display: flex;
    align-items: center;
    gap: 4px;
  }
  .sw {
    width: 18px;
    height: 18px;
    min-height: 0;
    padding: 0;
    border-radius: 50%;
    border: 2px solid transparent;
  }
  .sw.on {
    border-color: var(--text);
  }
  .medit .small {
    min-height: 22px;
    padding: 0 7px;
    font-size: calc(12px * var(--text-scale));
  }
  .head {
    position: absolute;
    bottom: 0;
    margin-left: -6px;
    width: 0;
    height: 0;
    border: 6px solid transparent;
    border-top: 8px solid var(--red);
    border-bottom: 0;
  }

  .headers {
    position: relative;
    overflow: hidden;
    border-right: 1px solid var(--line);
    background: var(--panel);
  }
  .th {
    height: 52px;
    padding: 5px 10px 0;
    border-bottom: 1px solid var(--line-soft);
    display: flex;
    flex-direction: column;
    gap: 5px;
  }
  .th input {
    height: 20px;
    min-height: 20px;
    padding: 0 4px;
  }
  .tname {
    font-weight: 500;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .muted-track .tname {
    color: var(--faint);
  }
  .tbtns {
    display: flex;
    align-items: center;
    gap: 4px;
  }
  .tb {
    min-height: 18px;
    height: 18px;
    width: 24px;
    padding: 0;
    font-size: calc(11px * var(--text-scale));
    font-weight: 600;
    display: grid;
    place-items: center;
  }
  .tb.lit {
    background: var(--accent);
    border-color: var(--accent);
    color: var(--accent-ink);
  }
  .tb.lock.lit {
    background: var(--red);
    border-color: var(--red);
  }
  .tmeter {
    flex: 1;
    height: 10px;
    display: flex;
    flex-direction: column;
    gap: 2px;
    background: var(--well);
    border-radius: 2px;
    padding: 1px;
    min-width: 20px;
  }
  .tmeter span {
    display: block;
    height: 3px;
    background: var(--green);
    border-radius: 1px;
  }
  .tmeter span.hot {
    background: var(--accent);
  }
  .addrow {
    display: flex;
    gap: 6px;
    padding: 8px 8px;
  }
  .addrow button {
    flex: 1;
    font-size: calc(12px * var(--text-scale));
    padding: 2px 4px;
  }
  .hresize {
    position: absolute;
    top: 0;
    bottom: 0;
    right: 0;
    width: 6px;
    cursor: col-resize;
    z-index: 2;
  }
  .hresize:hover {
    background: linear-gradient(to right, transparent 4px, var(--accent) 4px);
  }

  .lanes {
    position: relative;
    overflow: hidden;
    background: var(--well);
  }
  .lane {
    position: relative;
    height: 52px;
    border-bottom: 1px solid var(--line-soft);
  }
  .xfade {
    position: absolute;
    top: 4px;
    bottom: 4px;
    z-index: 3;
    border-radius: 3px;
    background: rgba(255, 255, 255, 0.16);
    border: 1px solid rgba(255, 255, 255, 0.7);
    cursor: pointer;
    overflow: hidden;
  }
  .xfade:hover {
    background: rgba(255, 255, 255, 0.24);
  }
  .xfade svg {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
  }
  .xfade path {
    stroke: rgba(255, 255, 255, 0.75);
    stroke-width: 1.2;
    vector-effect: non-scaling-stroke;
    fill: none;
  }
  .xlabel {
    position: absolute;
    left: 0;
    right: 0;
    bottom: 1px;
    text-align: center;
    font-size: calc(10px * var(--text-scale));
    color: #fff;
    text-shadow: 0 1px 2px rgba(0, 0, 0, 0.8);
    white-space: nowrap;
    pointer-events: none;
  }
  .xedge {
    position: absolute;
    top: 0;
    bottom: 0;
    width: 7px;
    cursor: ew-resize;
  }
  .xedge.l {
    left: -1px;
  }
  .xedge.r {
    right: -1px;
  }
  .xedge:hover {
    background: rgba(255, 255, 255, 0.35);
  }
  .xcut {
    position: absolute;
    top: 4px;
    bottom: 4px;
    width: 3px;
    margin-left: -1.5px;
    background: #dc3c3c;
    z-index: 3;
  }
  .lane.locked {
    background: repeating-linear-gradient(135deg, transparent 0 8px, rgba(255, 255, 255, 0.025) 8px 16px);
  }
  .lane.dropping {
    background: color-mix(in srgb, var(--accent) 8%, transparent);
  }
  .clip {
    position: absolute;
    top: 6px;
    bottom: 6px;
    border-radius: 4px;
    overflow: hidden;
    cursor: pointer;
    border: 1px solid rgba(255, 255, 255, 0.12);
    display: flex;
    align-items: flex-start;
  }
  .clip.video {
    background: var(--clip-video);
  }
  .clip.image {
    background: var(--clip-image);
  }
  .clip.audio {
    background: var(--clip-audio);
  }
  .clip.title {
    background: var(--clip-title);
  }
  .clip.comp {
    background: var(--clip-comp);
  }
  .clip.sel {
    border-color: var(--accent);
    box-shadow: inset 0 0 0 1px var(--accent);
  }
  .clip.dim {
    filter: saturate(0.3) brightness(0.7);
  }
  .clip.moving {
    opacity: 0.35;
  }
  .clip.ghost {
    pointer-events: none;
    opacity: 0.85;
    border: 1.5px dashed #fff;
    z-index: 2;
  }
  .clip::before,
  .clip::after {
    content: "";
    position: absolute;
    top: 0;
    bottom: 0;
    width: 6px;
    cursor: ew-resize;
    z-index: 1;
  }
  .clip::before {
    left: 0;
  }
  .clip::after {
    right: 0;
  }
  .thumb {
    height: 100%;
    aspect-ratio: 16 / 9;
    flex: none;
    opacity: 0.9;
    border-right: 1px solid rgba(0, 0, 0, 0.3);
  }
  .vol {
    position: absolute;
    left: 0;
    top: 0;
    overflow: visible;
    pointer-events: none;
    z-index: 2;
  }
  .fadeshade {
    fill: rgba(0, 0, 0, 0.42);
    stroke: rgb(255, 230, 150);
    stroke-width: 1.2;
  }
  .volline {
    fill: none;
    stroke: rgb(255, 210, 80);
    stroke-width: 1.5;
  }
  .volhit {
    fill: none;
    stroke: transparent;
    stroke-width: 8;
    pointer-events: stroke;
    cursor: ns-resize;
  }
  .volkey {
    fill: rgb(255, 210, 80);
    stroke: rgba(0, 0, 0, 0.65);
    stroke-width: 1;
    pointer-events: all;
    cursor: grab;
  }
  .volkey:hover,
  .fadeh:hover {
    fill: rgb(255, 196, 64);
  }
  .fadeh {
    fill: rgba(255, 255, 255, 0.8);
    pointer-events: all;
    cursor: ew-resize;
  }
  .label {
    position: relative;
    color: var(--clip-text);
    font-size: calc(12px * var(--text-scale));
    padding: 2px 6px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    text-shadow: 0 1px 2px rgba(0, 0, 0, 0.6);
    pointer-events: none;
  }
  .label b {
    font-weight: 600;
    font-size: calc(10.5px * var(--text-scale));
    background: rgba(0, 0, 0, 0.3);
    border-radius: 3px;
    padding: 0 4px;
    margin-right: 5px;
  }
  .label i {
    font-style: normal;
    opacity: 0.75;
    margin-left: 4px;
  }
  .wave {
    position: absolute;
    top: 1px;
    height: calc(100% - 2px);
    pointer-events: none;
  }
  .box {
    position: absolute;
    border: 1px solid var(--accent);
    background: var(--accent-soft);
    pointer-events: none;
  }
  .playhead {
    position: absolute;
    top: 0;
    bottom: 0;
    width: 1.5px;
    margin-left: -0.75px;
    background: var(--red);
    pointer-events: none;
    z-index: 3;
  }
  .snapline {
    position: absolute;
    top: 0;
    bottom: 0;
    width: 1px;
    background: #fff;
    opacity: 0.7;
    pointer-events: none;
    z-index: 3;
  }
  .dropline {
    position: absolute;
    top: 0;
    bottom: 0;
    width: 2px;
    background: var(--accent);
    pointer-events: none;
    z-index: 3;
  }
  .dropline span {
    position: absolute;
    left: 6px;
    top: 4px;
    background: var(--accent);
    color: var(--accent-ink);
    font-size: calc(11px * var(--text-scale));
    padding: 0 6px;
    border-radius: 3px;
    white-space: nowrap;
  }
  .lanehint {
    position: absolute;
    left: 0;
    right: 0;
    top: 50%;
    text-align: center;
    color: var(--faint);
    pointer-events: none;
  }
  .zoombar {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 4px 10px;
    border-top: 1px solid var(--line-soft);
    background: var(--panel);
    font-size: calc(12px * var(--text-scale));
  }
  .zoombar input {
    width: 180px;
  }
  .z {
    min-height: 20px;
    padding: 0 7px;
    font-size: calc(14px * var(--text-scale));
    line-height: 1;
  }
  .dragghost {
    position: fixed;
    z-index: 60;
    background: var(--raised);
    border: 1px solid var(--accent);
    border-radius: 4px;
    padding: 2px 8px;
    pointer-events: none;
    font-size: calc(12px * var(--text-scale));
  }
</style>
