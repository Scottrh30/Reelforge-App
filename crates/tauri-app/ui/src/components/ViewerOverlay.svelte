<script lang="ts">
  // Masks and the motion tracker of the selected clip, drawn over the Viewer
  // picture: drag a mask's points (or inside the mask being edited to move it
  // all), click out a freehand mask, and drag tracker boxes onto details.
  import { app, run, say } from "../lib/session.svelte";
  import { api, type Pt, type Curve4 } from "../lib/api";
  import { finishDrawing } from "../lib/actions.svelte";
  import { layerToFrame, frameToLayer, inside } from "../lib/geom";
  import { livePump } from "../lib/live";
  import { openMenu } from "../lib/menu.svelte";

  let { fit, box }: { fit: { k: number; kx: number; x: number; y: number }; box: HTMLElement | undefined } = $props();

  const MASK = "#ff6ec8";
  const POINT_COLOURS = ["#50dcff", "#ffd246", "#ff6ec8", "#78f078"];

  const ins = $derived(app.s?.inspector ?? null);
  const map = $derived(ins?.layer_map ?? null);
  const inv = $derived(ins?.layer_inv ?? null);
  const size = $derived(ins?.layer_px ?? ([1, 1] as Pt));
  const active = $derived(app.activeMask && ins && app.activeMask.clip === ins.id ? app.activeMask.index : null);
  const drawing = $derived(app.drawing && ins && app.drawing.clip === ins.id ? app.drawing : null);
  const busyTracking = $derived(!!app.live?.tracking);
  const tool = $derived(ins && ins.masks.length && !app.drawing ? app.maskTool : null);

  // Forget an edit target that no longer exists.
  $effect(() => {
    if (app.activeMask && (!ins || app.activeMask.clip !== ins.id || app.activeMask.index >= ins.masks.length)) app.activeMask = null;
    if (app.drawing && (!ins || app.drawing.clip !== ins.id)) app.drawing = null;
    // Picked mesh points belong to that clip's grid as it is.
    const n = ins?.tracker?.mode === "Mesh" ? ins.tracker.shown.length : 0;
    if (app.meshSel && (!ins || app.meshSel.clip !== ins.id || app.meshSel.points.some((p) => p >= n))) app.meshSel = null;
  });

  /// Layer pixels → box pixels.
  function toBox(p: Pt): Pt {
    const [fx, fy] = layerToFrame(map!, size, p);
    return [fit.x + fx * fit.kx, fit.y + fy * fit.k];
  }
  /// A pointer position (client pixels) → box pixels.
  function toBoxPt(e: PointerEvent | MouseEvent): Pt {
    const r = box!.getBoundingClientRect();
    return [e.clientX - r.left, e.clientY - r.top];
  }
  /// A pointer position (client pixels) → layer pixels.
  function toLayer(e: PointerEvent | MouseEvent): Pt {
    const r = box!.getBoundingClientRect();
    return frameToLayer(inv!, size, [(e.clientX - r.left - fit.x) / fit.kx, (e.clientY - r.top - fit.y) / fit.k]);
  }
  const pts = (list: Pt[]) => list.map((p) => toBox(p).join(",")).join(" ");
  const square = (c: Pt, r: number): Pt[] => [
    [c[0] - r, c[1] - r],
    [c[0] + r, c[1] - r],
    [c[0] + r, c[1] + r],
    [c[0] - r, c[1] + r],
  ];

  /// Drag mask `index`: one point (`point`), all of them (`null`), or resize
  /// it about its centre (`"resize"`; Shift changes only its width or height).
  /// Right-click on a mask point: add points beside it, delete it, curve it.
  function pointMenu(e: MouseEvent, index: number, point: number) {
    if (!ins) return;
    const id = ins.id;
    const m = ins.masks[index];
    app.activeMask = { clip: id, index };
    app.activeMaskPoint = point;
    const ellipse = m.mask.shape === "Ellipse";
    const n = m.handles.length;
    const add = async (before: boolean) => {
      const at = await run(api.maskPointAdd(id, index, point, before));
      if (typeof at === "number") app.activeMaskPoint = at;
    };
    const curved = isCurved(m.curves[point]);
    openMenu(e, [
      { heading: true, label: `${m.mask.name} · point ${point + 1}` },
      { label: ellipse ? "Add a point (not on an ellipse)" : "Add a point after this one", disabled: ellipse, run: () => add(false) },
      { label: "Add a point before this one", disabled: ellipse, run: () => add(true) },
      { label: n <= 3 ? "Delete point (a mask keeps three)" : "Delete point", disabled: ellipse || n <= 3, run: async () => {
        if (await run(api.maskPointDelete(id, index, point))) app.activeMaskPoint = Math.min(point, n - 2);
      } },
      { separator: true },
      { label: curved ? "Make a sharp corner" : "Make a smooth curve", hint: "Double-click", disabled: ellipse, run: () => run(api.maskPointCurve(id, index, point)) },
    ]);
  }

  /// Right-click on a mask's line: add a point there.
  function lineMenu(e: MouseEvent, index: number) {
    if (!ins || !inv) return;
    e.preventDefault();
    e.stopPropagation();
    const id = ins.id;
    const m = ins.masks[index];
    const at = toLayer(e);
    app.activeMask = { clip: id, index };
    openMenu(e, [
      { heading: true, label: m.mask.name },
      {
        label: "Add a point here",
        run: async () => {
          const p = await run(api.maskPointAddAt(id, index, at));
          if (typeof p === "number") app.activeMaskPoint = p;
        },
      },
    ]);
  }

  /// Left-click on a mask's line: drag the mask being edited, or start editing this one.
  function lineDown(e: PointerEvent, index: number) {
    if (e.button !== 0 || !ins) return;
    if (index === active) return dragMask(e, index, null);
    e.stopPropagation();
    app.activeMask = { clip: ins.id, index };
    app.activeMaskPoint = null;
  }

  function dragMask(e: PointerEvent, index: number, point: number | null | "resize") {
    if (e.button !== 0 || !ins || !inv || app.live?.playing) return;
    e.stopPropagation();
    e.preventDefault();
    const id = ins.id;
    if (app.activeMask?.clip !== id || app.activeMask.index !== index) app.activeMaskPoint = null;
    app.activeMask = { clip: id, index };
    if (typeof point === "number") {
      app.activeMaskPoint = point;
      // Alt-drag a point: pull a curve out of it.
      if (e.altKey && ins.masks[index].mask.shape !== "Ellipse") return pullCurve(e, index, point);
    }
    const el = e.currentTarget as SVGElement;
    el.setPointerCapture(e.pointerId);
    const start = toLayer(e);
    const orig = ins.masks[index].handles.map((p) => [...p] as Pt);
    const c: Pt = [orig.reduce((a, p) => a + p[0], 0) / orig.length, orig.reduce((a, p) => a + p[1], 0) / orig.length];
    const pump = livePump();
    const move = (ev: PointerEvent) => {
      const now = toLayer(ev);
      let next: Pt[];
      if (point === "resize") {
        const d0 = Math.hypot(start[0] - c[0], start[1] - c[1]) || 1;
        let [kx, ky] = [Math.hypot(now[0] - c[0], now[1] - c[1]) / d0, 0];
        ky = kx;
        if (ev.shiftKey) {
          // Width or height only, whichever way the drag mostly goes.
          const wide = Math.abs(start[0] - c[0]) >= Math.abs(start[1] - c[1]);
          if (wide) [kx, ky] = [Math.abs(now[0] - c[0]) / (Math.abs(start[0] - c[0]) || 1), 1];
          else [kx, ky] = [1, Math.abs(now[1] - c[1]) / (Math.abs(start[1] - c[1]) || 1)];
        }
        kx = Math.max(0.02, kx);
        ky = Math.max(0.02, ky);
        next = orig.map((p) => [c[0] + (p[0] - c[0]) * kx, c[1] + (p[1] - c[1]) * ky] as Pt);
      } else {
        const [dx, dy] = [now[0] - start[0], now[1] - start[1]];
        next = orig.map((p, j) => (point === null || point === j ? ([p[0] + dx, p[1] + dy] as Pt) : p));
      }
      pump.push(() => api.setMaskPoints(id, index, next));
    };
    const up = async () => {
      el.removeEventListener("pointermove", move);
      el.removeEventListener("pointerup", up);
      await pump.done();
      await run(api.commit());
    };
    el.addEventListener("pointermove", move);
    el.addEventListener("pointerup", up);
  }

  // ---- Curves: Bézier handles on mask points ----
  const isCurved = (c: Curve4 | undefined) => !!c && c.some((v) => v !== 0);

  /// Alt-drag from point `j`: its out-handle follows the pointer, the in-handle mirrors it.
  function pullCurve(e: PointerEvent, index: number, j: number) {
    if (!ins) return;
    const id = ins.id;
    const el = e.currentTarget as SVGElement;
    el.setPointerCapture(e.pointerId);
    const p = ins.masks[index].handles[j];
    const orig = ins.masks[index].curves.map((c) => [...c] as Curve4);
    const pump = livePump();
    say("Drag out the curve's handles (release to finish)");
    const move = (ev: PointerEvent) => {
      const now = toLayer(ev);
      const d: Pt = [now[0] - p[0], now[1] - p[1]];
      const next = orig.map((c, k) => (k === j ? ([-d[0], -d[1], d[0], d[1]] as Curve4) : c));
      pump.push(() => api.setMaskCurves(id, index, next));
    };
    const up = async () => {
      el.removeEventListener("pointermove", move);
      el.removeEventListener("pointerup", up);
      await pump.done();
      await run(api.commit());
    };
    el.addEventListener("pointermove", move);
    el.addEventListener("pointerup", up);
  }

  /// Drag one handle end of point `j` (`out`: the side towards the next point).
  /// The other handle turns to stay opposite, keeping its length, so the curve
  /// stays smooth through the point; Alt moves this one alone (a sharp bend).
  function dragCurveHandle(e: PointerEvent, index: number, j: number, out: boolean) {
    if (e.button !== 0 || !ins || !inv || app.live?.playing) return;
    e.stopPropagation();
    e.preventDefault();
    const id = ins.id;
    app.activeMaskPoint = j;
    const el = e.currentTarget as SVGElement;
    el.setPointerCapture(e.pointerId);
    const p = ins.masks[index].handles[j];
    const orig = ins.masks[index].curves.map((c) => [...c] as Curve4);
    const c0 = orig[j];
    const otherLen = out ? Math.hypot(c0[0], c0[1]) : Math.hypot(c0[2], c0[3]);
    // Was it smooth (the two handles opposite) to begin with?
    const smooth = (() => {
      const [ax, ay, bx, by] = c0;
      const la = Math.hypot(ax, ay), lb = Math.hypot(bx, by);
      if (la < 1e-3 || lb < 1e-3) return true;
      return (ax * bx + ay * by) / (la * lb) < -0.995;
    })();
    const pump = livePump();
    const move = (ev: PointerEvent) => {
      const now = toLayer(ev);
      const d: Pt = [now[0] - p[0], now[1] - p[1]];
      const len = Math.hypot(d[0], d[1]) || 1;
      const opp: Pt = [(-d[0] / len) * otherLen, (-d[1] / len) * otherLen];
      const c: Curve4 = [...c0];
      const mirror = smooth && !ev.altKey;
      if (out) {
        c[2] = d[0];
        c[3] = d[1];
        if (mirror) [c[0], c[1]] = opp;
      } else {
        c[0] = d[0];
        c[1] = d[1];
        if (mirror) [c[2], c[3]] = opp;
      }
      const next = orig.map((x, k) => (k === j ? c : x));
      pump.push(() => api.setMaskCurves(id, index, next));
    };
    const up = async () => {
      el.removeEventListener("pointermove", move);
      el.removeEventListener("pointerup", up);
      await pump.done();
      await run(api.commit());
    };
    el.addEventListener("pointermove", move);
    el.addEventListener("pointerup", up);
  }

  /// With the Move or Resize tool: the mask under the pointer (the one being
  /// edited first), and drag it.
  function toolDown(e: PointerEvent) {
    if (!ins || !inv || e.button !== 0) return;
    e.stopPropagation();
    const p = toLayer(e);
    const order = ins.masks.map((_, i) => i).sort((a, b) => (a === active ? -1 : b === active ? 1 : b - a));
    const hit = order.find((i) => ins.masks[i].mask.enabled && inside(ins.masks[i].outline, p));
    if (hit === undefined) {
      app.activeMask = null;
      return;
    }
    dragMask(e, hit, app.maskTool === "resize" ? "resize" : null);
  }

  /// Drag tracker point `i`'s box onto a detail. On a mesh: Shift+click
  /// picks points (or drops them); dragging a picked point moves all the
  /// picked ones, and with a falloff the points around follow, fading out.
  function dragTracker(e: PointerEvent, i: number) {
    const t = ins?.tracker;
    if (e.button !== 0 || !ins || !t || !inv || app.live?.playing || busyTracking) return;
    e.stopPropagation();
    e.preventDefault();
    const id = ins.id;
    const mesh = t.mode === "Mesh";
    const picked = mesh && app.meshSel?.clip === id ? app.meshSel.points : [];
    if (mesh && e.shiftKey) {
      app.meshSel = { clip: id, points: picked.includes(i) ? picked.filter((p) => p !== i) : [...picked, i] };
      return;
    }
    // Dragging a point that isn't picked picks it alone.
    const group = picked.includes(i) ? picked : [i];
    if (mesh && !picked.includes(i)) app.meshSel = { clip: id, points: [i] };
    const el = e.currentTarget as SVGElement;
    el.setPointerCapture(e.pointerId);
    const start = toLayer(e);
    const from = t.shown.map((p) => [...p] as Pt);
    const weights = mesh ? meshWeights(t.grid, group, app.meshFalloff) : [];
    // One point and no falloff: as before (a corner before tracking carries the grid).
    const many = mesh && weights.filter((w) => w > 0).length > 1;
    const pump = livePump();
    const move = (ev: PointerEvent) => {
      const now = toLayer(ev);
      const [dx, dy] = [now[0] - start[0], now[1] - start[1]];
      if (many) {
        const moves: [number, Pt][] = weights.flatMap((w, k) => (w > 0 ? [[k, [from[k][0] + dx * w, from[k][1] + dy * w] as Pt] as [number, Pt]] : []));
        pump.push(() => api.trackerMoveMany(id, moves, app.trackBlend));
      } else {
        pump.push(() => api.trackerMove(id, i, [from[i][0] + dx, from[i][1] + dy], app.trackBlend));
      }
    };
    const up = async () => {
      el.removeEventListener("pointermove", move);
      el.removeEventListener("pointerup", up);
      await pump.done();
      await run(api.commit());
    };
    el.addEventListener("pointermove", move);
    el.addEventListener("pointerup", up);
  }

  /// How much each point of a `grid` mesh moves with the `group`: 1 for the
  /// group, fading smoothly to 0 at `falloff` cells from the nearest of them.
  function meshWeights(grid: [number, number], group: number[], falloff: number): number[] {
    const [gc, gr] = grid;
    const n = (gc + 1) * (gr + 1);
    const at = (k: number) => [k % (gc + 1), Math.floor(k / (gc + 1))];
    const g = group.map(at);
    return Array.from({ length: n }, (_, k) => {
      if (group.includes(k)) return 1;
      if (falloff <= 0) return 0;
      const [x, y] = at(k);
      const d = Math.min(...g.map(([a, b]) => Math.hypot(x - a, y - b)));
      if (d >= falloff) return 0;
      const s = 1 - d / falloff;
      return s * s * (3 - 2 * s);
    });
  }

  /// Shift+drag over a mesh: pick the points inside the box (added to those picked).
  let meshBox = $state<{ a: Pt; b: Pt } | null>(null);
  function meshBoxDown(e: PointerEvent) {
    const t = ins?.tracker;
    if (!e.shiftKey || e.button !== 0 || !ins || !t || t.mode !== "Mesh" || !inv || app.live?.playing || busyTracking) return;
    e.stopPropagation();
    e.preventDefault();
    const id = ins.id;
    const el = e.currentTarget as SVGElement;
    el.setPointerCapture(e.pointerId);
    const a = toBoxPt(e);
    meshBox = { a, b: a };
    const move = (ev: PointerEvent) => meshBox && (meshBox = { a, b: toBoxPt(ev) });
    const up = () => {
      el.removeEventListener("pointermove", move);
      el.removeEventListener("pointerup", up);
      if (!meshBox) return;
      const [x0, x1] = [Math.min(meshBox.a[0], meshBox.b[0]), Math.max(meshBox.a[0], meshBox.b[0])];
      const [y0, y1] = [Math.min(meshBox.a[1], meshBox.b[1]), Math.max(meshBox.a[1], meshBox.b[1])];
      const inBox = t.shown.flatMap((p, k) => {
        const [x, y] = toBox(p);
        return x >= x0 && x <= x1 && y >= y0 && y <= y1 ? [k] : [];
      });
      const had = app.meshSel?.clip === id ? app.meshSel.points : [];
      app.meshSel = { clip: id, points: [...new Set([...had, ...inBox])] };
      meshBox = null;
    };
    el.addEventListener("pointermove", move);
    el.addEventListener("pointerup", up);
  }

  // ---- 3D camera tracker: the solved points at the playhead (frame pixels) ----
  const cam = $derived(ins?.cam_track && (app.cam3d || !ins.tracker) ? ins.cam_track : null);
  const camPicked = $derived(app.camSel && ins && app.camSel.clip === ins.id ? app.camSel.points : []);
  // Forget picks that are no longer points of this clip's solve.
  $effect(() => {
    if (app.camSel && (!ins || app.camSel.clip !== ins.id || !ins.cam_track || app.camSel.points.some((p) => p >= ins.cam_track!.point_count))) app.camSel = null;
  });
  const fbox = (p: Pt): Pt => [fit.x + p[0] * fit.kx, fit.y + p[1] * fit.k];
  /// Green fits well, yellow fairly, orange poorly.
  const fitColour = (e: number) => (e < 0.6 ? "#5ef08a" : e < 1.5 ? "#ffd246" : "#ff8a3d");
  function camPick(e: PointerEvent, k: number) {
    if (!ins || e.button !== 0) return;
    e.stopPropagation();
    e.preventDefault();
    const id = ins.id;
    if (e.shiftKey) app.camSel = { clip: id, points: camPicked.includes(k) ? camPicked.filter((p) => p !== k) : [...camPicked, k] };
    else app.camSel = { clip: id, points: [k] };
  }
  // Shift held: a layer over the picture takes Shift+drags as a box to pick points in.
  let shiftHeld = $state(false);
  let camBox = $state<{ a: Pt; b: Pt } | null>(null);
  function camBoxDown(e: PointerEvent) {
    if (!e.shiftKey || e.button !== 0 || !ins || !cam) return;
    e.stopPropagation();
    e.preventDefault();
    const id = ins.id;
    const pts = cam.points;
    const el = e.currentTarget as SVGElement;
    el.setPointerCapture(e.pointerId);
    const a = toBoxPt(e);
    camBox = { a, b: a };
    const move = (ev: PointerEvent) => camBox && (camBox = { a, b: toBoxPt(ev) });
    const up = () => {
      el.removeEventListener("pointermove", move);
      el.removeEventListener("pointerup", up);
      if (!camBox) return;
      const [x0, x1] = [Math.min(camBox.a[0], camBox.b[0]), Math.max(camBox.a[0], camBox.b[0])];
      const [y0, y1] = [Math.min(camBox.a[1], camBox.b[1]), Math.max(camBox.a[1], camBox.b[1])];
      const inBox = pts.flatMap(([k, p]) => {
        const [x, y] = fbox(p);
        return x >= x0 && x <= x1 && y >= y0 && y <= y1 ? [k] : [];
      });
      const had = app.camSel?.clip === id ? app.camSel.points : [];
      app.camSel = { clip: id, points: [...new Set([...had, ...inBox])] };
      camBox = null;
    };
    el.addEventListener("pointermove", move);
    el.addEventListener("pointerup", up);
  }

  // Energy blades of the selected clip: hilt and tip handles (% of the layer).
  type Blade = { index: number; hilt: Pt; tip: Pt; colour: string };
  const blades = $derived.by((): Blade[] => {
    if (!ins) return [];
    return ins.effects.flatMap((fx, index) => {
      if (fx.kind !== "EnergyBlade" || !fx.enabled || fx.params.length < 6) return [];
      const v = (k: number) => fx.params[k].value;
      const at = (x: number, y: number): Pt => [(x / 100) * size[0], (y / 100) * size[1]];
      const c = Math.round(v(5)).toString(16).padStart(6, "0");
      return [{ index, hilt: at(v(0), v(1)), tip: at(v(2), v(3)), colour: `#${c}` }];
    });
  });
  /// Drag a blade's hilt, tip, or (`"both"`) the whole blade.
  function dragBlade(e: PointerEvent, b: Blade, part: "hilt" | "tip" | "both") {
    if (e.button !== 0 || !ins || !inv || app.live?.playing || busyTracking) return;
    e.stopPropagation();
    e.preventDefault();
    const id = ins.id;
    const el = e.currentTarget as SVGElement;
    el.setPointerCapture(e.pointerId);
    const start = toLayer(e);
    const pct = (p: Pt): Pt => [(p[0] / size[0]) * 100, (p[1] / size[1]) * 100];
    const pump = livePump();
    const move = (ev: PointerEvent) => {
      const now = toLayer(ev);
      const d: Pt = [now[0] - start[0], now[1] - start[1]];
      const shift = (p: Pt): Pt => pct([p[0] + d[0], p[1] + d[1]]);
      const sets: [number, number][] = [];
      if (part !== "tip") {
        const h = shift(b.hilt);
        sets.push([0, h[0]], [1, h[1]]);
      }
      if (part !== "hilt") {
        let t = shift(b.tip);
        if (part === "tip" && ev.shiftKey) {
          // Shift: keep the blade's length, only turn it.
          const L = Math.hypot(b.tip[0] - b.hilt[0], b.tip[1] - b.hilt[1]);
          const [px, py] = [b.tip[0] + d[0] - b.hilt[0], b.tip[1] + d[1] - b.hilt[1]];
          const k = L / (Math.hypot(px, py) || 1);
          t = pct([b.hilt[0] + px * k, b.hilt[1] + py * k]);
        }
        sets.push([2, t[0]], [3, t[1]]);
      }
      pump.push(() => Promise.all(sets.map(([k, v]) => api.effectParam(id, b.index, k, v))));
    };
    const up = async () => {
      el.removeEventListener("pointermove", move);
      el.removeEventListener("pointerup", up);
      await pump.done();
      await run(api.commit());
    };
    el.addEventListener("pointermove", move);
    el.addEventListener("pointerup", up);
  }

  // Particle emitters of the selected clip: where they are, their shape and
  // which way they throw (in layer units; settings in core particles.rs).
  type Emitter = { index: number; at: Pt; dir: number; shape: number; extent: number };
  const emitters = $derived.by((): Emitter[] => {
    if (!ins) return [];
    return ins.effects.flatMap((fx, index) => {
      if (fx.kind !== "Particles" || !fx.enabled || fx.params.length < 10) return [];
      const v = (k: number) => fx.params[k].value;
      return [{ index, at: [(v(0) / 100) * size[0], (v(1) / 100) * size[1]] as Pt, dir: v(8), shape: Math.round(v(2)), extent: v(3) }];
    });
  });
  // Lens flares of the selected clip: where their light is.
  const flares = $derived.by((): Emitter[] => {
    if (!ins) return [];
    return ins.effects.flatMap((fx, index) => {
      if (fx.kind !== "LensFlare" || !fx.enabled || fx.params.length < 2) return [];
      const v = (k: number) => fx.params[k].value;
      return [{ index, at: [(v(0) / 100) * size[0], (v(1) / 100) * size[1]] as Pt, dir: 0, shape: 0, extent: 0 }];
    });
  });
  // OpenFX plugins' positions: a handle each (x, y in % of the layer, y down).
  type FxPoint = { index: number; param: number; label: string; at: Pt; base: Pt };
  const fxPoints = $derived.by((): FxPoint[] => {
    if (!ins) return [];
    return ins.effects.flatMap((fx, index) =>
      !fx.enabled || !fx.points
        ? []
        : fx.points
            .filter((p) => fx.params.length > p.param + 1 && (p.relative === null || fx.params.length > p.relative + 1))
            .map((p) => {
              // A relative point (a patch's source) is measured from its base point.
              const v = (k: number) => fx.params[k].value;
              const base: Pt = p.relative === null ? [0, 0] : [(v(p.relative) / 100) * size[0], (v(p.relative + 1) / 100) * size[1]];
              const at: Pt = [base[0] + (v(p.param) / 100) * size[0], base[1] + (v(p.param + 1) / 100) * size[1]];
              return { index, param: p.param, label: `${fx.label}: ${p.label}`, at, base };
            }),
    );
  });
  // Wire removals and lightning: a line between their two ends (the ends are fxPoints).
  const wires = $derived.by((): [Pt, Pt][] => {
    if (!ins) return [];
    return ins.effects.flatMap((fx) => {
      if ((fx.kind !== "WireRemoval" && fx.kind !== "Lightning") || !fx.enabled || fx.params.length < 4) return [];
      const v = (k: number) => fx.params[k].value;
      const at = (x: number, y: number): Pt => [(x / 100) * size[0], (y / 100) * size[1]];
      return [[at(v(0), v(1)), at(v(2), v(3))] as [Pt, Pt]];
    });
  });
  /// Drag a plugin's position.
  function dragFxPoint(e: PointerEvent, m: FxPoint) {
    if (e.button !== 0 || !ins || !inv || app.live?.playing || busyTracking) return;
    e.stopPropagation();
    e.preventDefault();
    const id = ins.id;
    const el = e.currentTarget as SVGElement;
    el.setPointerCapture(e.pointerId);
    const start = toLayer(e);
    const pump = livePump();
    const move = (ev: PointerEvent) => {
      const now = toLayer(ev);
      let [dx, dy] = [now[0] - start[0], now[1] - start[1]];
      // Shift: straight across or straight down.
      if (ev.shiftKey) Math.abs(dx) > Math.abs(dy) ? (dy = 0) : (dx = 0);
      const x = ((m.at[0] + dx - m.base[0]) / size[0]) * 100;
      const y = ((m.at[1] + dy - m.base[1]) / size[1]) * 100;
      pump.push(() => Promise.all([api.effectParam(id, m.index, m.param, x), api.effectParam(id, m.index, m.param + 1, y)]));
    };
    const up = async () => {
      el.removeEventListener("pointermove", move);
      el.removeEventListener("pointerup", up);
      await pump.done();
      await run(api.commit());
    };
    el.addEventListener("pointermove", move);
    el.addEventListener("pointerup", up);
  }
  /// Screen pixels the direction arrow is long.
  const ARROW = 46;
  /// Drag an emitter (`"move"`) or the tip of its direction arrow (`"aim"`).
  function dragEmitter(e: PointerEvent, m: Emitter, part: "move" | "aim") {
    if (e.button !== 0 || !ins || !inv || app.live?.playing || busyTracking) return;
    e.stopPropagation();
    e.preventDefault();
    const id = ins.id;
    const el = e.currentTarget as SVGElement;
    el.setPointerCapture(e.pointerId);
    const start = toLayer(e);
    const pump = livePump();
    const move = (ev: PointerEvent) => {
      const now = toLayer(ev);
      if (part === "move") {
        const x = ((m.at[0] + now[0] - start[0]) / size[0]) * 100;
        const y = ((m.at[1] + now[1] - start[1]) / size[1]) * 100;
        pump.push(() => Promise.all([api.effectParam(id, m.index, 0, x), api.effectParam(id, m.index, 1, y)]));
      } else {
        // Anticlockwise from the right, y down the screen; Shift snaps to 15°.
        let deg = (Math.atan2(-(now[1] - m.at[1]), now[0] - m.at[0]) * 180) / Math.PI;
        if (ev.shiftKey) deg = Math.round(deg / 15) * 15;
        pump.push(() => api.effectParam(id, m.index, 8, deg));
      }
    };
    const up = async () => {
      el.removeEventListener("pointermove", move);
      el.removeEventListener("pointerup", up);
      await pump.done();
      await run(api.commit());
    };
    el.addEventListener("pointermove", move);
    el.addEventListener("pointerup", up);
  }

  // A camera's or light's points: drag one across the camera's view.
  function dragScenePoint(e: PointerEvent, which: string) {
    if (e.button !== 0 || !ins || app.live?.playing) return;
    e.stopPropagation();
    e.preventDefault();
    const id = ins.id;
    const el = e.currentTarget as SVGElement;
    el.setPointerCapture(e.pointerId);
    const [sx, sy] = [e.clientX, e.clientY];
    const pump = livePump();
    let first = true;
    const move = (ev: PointerEvent) => {
      const d: Pt = [(ev.clientX - sx) / fit.kx, (ev.clientY - sy) / fit.k];
      const start = first;
      first = false;
      pump.push(() => api.scenePointDrag(id, which, d, start));
    };
    const up = async () => {
      el.removeEventListener("pointermove", move);
      el.removeEventListener("pointerup", up);
      await pump.done();
      await run(api.commit());
    };
    el.addEventListener("pointermove", move);
    el.addEventListener("pointerup", up);
  }

  // The key-colour eyedropper: a click picks round a point, a drag averages the box.
  const keyPick = $derived(app.keyPick && ins && app.keyPick.clip === ins.id ? app.keyPick : null);
  $effect(() => {
    if (app.keyPick && (!ins || app.keyPick.clip !== ins.id)) app.keyPick = null;
  });
  let pickBox = $state<[Pt, Pt] | null>(null);
  function pickDown(e: PointerEvent) {
    if (!keyPick || !inv || e.button !== 0) return;
    e.stopPropagation();
    const el = e.currentTarget as SVGElement;
    el.setPointerCapture(e.pointerId);
    const start = toLayer(e);
    pickBox = [start, start];
    const move = (m: PointerEvent) => {
      if (pickBox) pickBox = [start, toLayer(m)];
    };
    const up = async () => {
      el.removeEventListener("pointermove", move);
      el.removeEventListener("pointerup", up);
      const [a, b] = pickBox ?? [start, start];
      pickBox = null;
      const { clip, index } = keyPick!;
      app.keyPick = null;
      app.note = "";
      await run(api.effectKeyPick(clip, index, a, b));
    };
    el.addEventListener("pointermove", move);
    el.addEventListener("pointerup", up);
  }

  // Roto brush: drag to paint (Alt: over what isn't the subject); each stroke cuts the mask again.
  const roto = $derived(app.roto && ins && app.roto.clip === ins.id ? app.roto : null);
  let stroke = $state<{ fg: boolean; points: Pt[] } | null>(null);
  let brushAt = $state<Pt | null>(null);
  /// Screen pixels per layer pixel.
  function layerScale(): number {
    const [a, b] = [toBox([0, 0]), toBox([100, 0])];
    return Math.hypot(b[0] - a[0], b[1] - a[1]) / 100 || 1;
  }
  function rotoDown(e: PointerEvent) {
    if (!roto || !inv || e.button !== 0 || busyTracking) return;
    e.stopPropagation();
    e.preventDefault();
    const el = e.currentTarget as SVGElement;
    el.setPointerCapture(e.pointerId);
    stroke = { fg: !e.altKey, points: [toLayer(e)] };
    const move = (ev: PointerEvent) => {
      if (!stroke) return;
      const p = toLayer(ev);
      const last = stroke.points[stroke.points.length - 1];
      // A point every few screen pixels.
      if (Math.hypot(p[0] - last[0], p[1] - last[1]) * layerScale() >= 3) stroke = { fg: stroke.fg, points: [...stroke.points, p] };
    };
    const up = async () => {
      el.removeEventListener("pointermove", move);
      el.removeEventListener("pointerup", up);
      const s = stroke;
      if (!s || !app.roto || !ins) return;
      const id = ins.id;
      const radius = app.roto.size / layerScale();
      if (app.roto.pick && s.fg) {
        // One click: pick the object out and carry it through the clip; then paint to correct it.
        say("Picking out the object…");
        const r = await run(api.rotoPick(id, s.points[0], !!app.roto.removal));
        stroke = null;
        if (typeof r === "number" && app.roto && app.roto.clip === id) app.roto = { ...app.roto, mask: r, pick: false };
        return;
      }
      say("Cutting round the subject…");
      const r = await run(api.rotoStroke(id, app.roto.mask, s.fg, radius, s.points, !!app.roto.removal));
      stroke = null;
      if (typeof r === "number" && app.roto && app.roto.clip === id) app.roto = { ...app.roto, mask: r };
    };
    el.addEventListener("pointermove", move);
    el.addEventListener("pointerup", up);
  }

  // Paint: drag to paint a stroke with the Paint effect; Alt-click sets where clones copy from.
  const painting = $derived(app.paint && ins && app.paint.clip === ins.id ? app.paint : null);
  const paintFx = $derived(painting && ins ? ins.effects[painting.index] : null);
  /// The brush's radius on screen.
  const paintRadius = $derived(paintFx ? (paintFx.params[1]?.value ?? 20) / 2 : 10);
  function paintDown(e: PointerEvent) {
    if (!painting || !inv || e.button !== 0) return;
    e.stopPropagation();
    e.preventDefault();
    const at = toLayer(e);
    if (e.altKey) {
      app.paint = { ...painting, source: at };
      say("Clone source set: paint where it should go");
      return;
    }
    const el = e.currentTarget as SVGElement;
    el.setPointerCapture(e.pointerId);
    stroke = { fg: true, points: [at] };
    const move = (ev: PointerEvent) => {
      if (!stroke) return;
      const p = toLayer(ev);
      const last = stroke.points[stroke.points.length - 1];
      if (Math.hypot(p[0] - last[0], p[1] - last[1]) * layerScale() >= 2) stroke = { fg: true, points: [...stroke.points, p] };
    };
    const up = async () => {
      el.removeEventListener("pointermove", move);
      el.removeEventListener("pointerup", up);
      const s = stroke;
      if (!s || !app.paint || !ins) return;
      const source = app.paint.source;
      await run(api.paintStroke(ins.id, app.paint.index, s.points, source));
      stroke = null;
      if (app.paint) app.paint = { ...app.paint, source: null };
    };
    el.addEventListener("pointermove", move);
    el.addEventListener("pointerup", up);
  }

  // Freehand: each click adds a point; clicking the first one (or Enter, or a double-click) closes it.
  let hover = $state<Pt | null>(null);
  function drawClick(e: MouseEvent) {
    if (!drawing || !inv) return;
    e.stopPropagation();
    const p = toLayer(e);
    const list = drawing.points;
    if (list.length >= 3) {
      const [fx, fy] = toBox(list[0]);
      const r = box!.getBoundingClientRect();
      if (Math.hypot(e.clientX - r.left - fx, e.clientY - r.top - fy) <= 9) {
        finishDrawing();
        return;
      }
    }
    app.drawing = { clip: drawing.clip, points: [...list, p] };
    say(`${list.length + 1} points: click the first point or press Enter to close the mask (Esc cancels)`);
  }

</script>

<svelte:window
  onkeydown={(e) => {
    shiftHeld = e.shiftKey;
    if (e.key === "Escape" && !e.defaultPrevented) {
      if (app.roto) app.roto = null;
      else if (app.paint) app.paint = null;
      else if (app.meshSel) app.meshSel = null;
      else if (app.camSel) app.camSel = null;
    }
  }}
  onkeyup={(e) => (shiftHeld = e.shiftKey)}
  onblur={() => (shiftHeld = false)}
/>

{#if cam && ins}
  <!-- The 3D camera tracker's solved points: click to pick; Shift+click or Shift+drag for more. -->
  <svg class="overlay" aria-hidden="true">
    {#if shiftHeld || camBox}
      {@const [x0, y0] = fbox([0, 0])}
      {@const [x1, y1] = fbox([app.s?.seq.width ?? 1920, app.s?.seq.height ?? 1080])}
      <rect x={x0} y={y0} width={x1 - x0} height={y1 - y0} class="cambox-catch" onpointerdown={camBoxDown} role="presentation" />
    {/if}
    {#each cam.points as [k, p, err] (k)}
      {@const [x, y] = fbox(p)}
      {@const on = camPicked.includes(k)}
      <g class="campt" class:on>
        <line x1={x - (on ? 6 : 4)} y1={y - (on ? 6 : 4)} x2={x + (on ? 6 : 4)} y2={y + (on ? 6 : 4)} stroke={on ? "#fff" : fitColour(err)} />
        <line x1={x - (on ? 6 : 4)} y1={y + (on ? 6 : 4)} x2={x + (on ? 6 : 4)} y2={y - (on ? 6 : 4)} stroke={on ? "#fff" : fitColour(err)} />
        {#if on}<circle cx={x} cy={y} r="8" class="campick" />{/if}
        <circle cx={x} cy={y} r="7" class="camhit" onpointerdown={(e) => camPick(e, k)} role="presentation"><title>A tracked point ({err.toFixed(2)} px): click to pick it, Shift+click to add it</title></circle>
      </g>
    {/each}
    {#if camBox}<rect x={Math.min(camBox.a[0], camBox.b[0])} y={Math.min(camBox.a[1], camBox.b[1])} width={Math.abs(camBox.b[0] - camBox.a[0])} height={Math.abs(camBox.b[1] - camBox.a[1])} class="meshbox" />{/if}
  </svg>
{/if}

{#if ins?.parent_link}
  <!-- The selected layer's link to the point it follows. -->
  {@const [a, b] = ins.parent_link.map((p) => [fit.x + p[0] * fit.kx, fit.y + p[1] * fit.k])}
  <svg class="overlay" aria-hidden="true">
    <line x1={a[0]} y1={a[1]} x2={b[0]} y2={b[1]} class="plink" />
    <circle cx={a[0]} cy={a[1]} r="3" class="plinkend" />
    <circle cx={b[0]} cy={b[1]} r="5" class="plinkend parent" />
  </svg>
{/if}

{#if ins && ins.scene_points.length}
  <svg class="overlay" aria-hidden="true">
    {#if ins.scene_points.length === 2}
      {@const [a, b] = ins.scene_points.map((p) => [fit.x + p.at[0] * fit.kx, fit.y + p.at[1] * fit.k])}
      <line x1={a[0]} y1={a[1]} x2={b[0]} y2={b[1]} class="bladeaxis" />
    {/if}
    {#each ins.scene_points as p}
      {@const x = fit.x + p.at[0] * fit.kx}
      {@const y = fit.y + p.at[1] * fit.k}
      {#if p.which === "position" && ins.kind === "point"}
        <!-- A point: crosshairs in a ring. -->
        <circle cx={x} cy={y} r="8" class="scenept target" onpointerdown={(e) => dragScenePoint(e, p.which)} role="presentation"><title>The point: drag to move it (and the layers parented to it)</title></circle>
        <line x1={x - 14} y1={y} x2={x + 14} y2={y} class="ray" />
        <line x1={x} y1={y - 14} x2={x} y2={y + 14} class="ray" />
      {:else if p.which === "position"}
        <circle cx={x} cy={y} r="9" class="scenept light" onpointerdown={(e) => dragScenePoint(e, p.which)} role="presentation"><title>{ins.kind === "light" ? "The light" : "The camera"}: drag to move it across the view</title></circle>
        {#each [0, 45, 90, 135, 180, 225, 270, 315] as a}
          <line x1={x + Math.cos((a * Math.PI) / 180) * 12} y1={y + Math.sin((a * Math.PI) / 180) * 12} x2={x + Math.cos((a * Math.PI) / 180) * 16} y2={y + Math.sin((a * Math.PI) / 180) * 16} class="ray" />
        {/each}
      {:else}
        <circle cx={x} cy={y} r="7" class="scenept target" onpointerdown={(e) => dragScenePoint(e, p.which)} role="presentation"><title>Point of interest: drag to aim</title></circle>
        <line x1={x - 11} y1={y} x2={x + 11} y2={y} class="ray" />
        <line x1={x} y1={y - 11} x2={x} y2={y + 11} class="ray" />
      {/if}
    {/each}
  </svg>
{/if}

{#if ins && map && inv}
  <svg class="overlay" class:drawing={!!drawing} aria-hidden="true">
    <!-- Masks: outlines, with handles on their points. -->
    {#each ins.masks as m, i}
      {#if m.outline.length > 1}
        <polygon
          points={pts(m.outline)}
          class="mask"
          class:keep={!!m.mask.key_keep}
          class:ignore={!!m.mask.track_ignore || !!m.mask.fill_area}
          class:on={i === active}
          class:off={!m.mask.enabled}
          stroke={MASK}
          onpointerdown={(e) => (i === active ? dragMask(e, i, null) : null)}
          role="presentation"
        />
        {#if m.mask.shape !== "Ellipse" && m.mask.enabled && !drawing}
          <!-- The line itself (a wide invisible stroke): right-click adds a point there. -->
          <polygon points={pts(m.outline)} class="maskline" oncontextmenu={(e) => lineMenu(e, i)} onpointerdown={(e) => lineDown(e, i)} role="presentation"><title>Click to edit this mask; right-click to add a point here</title></polygon>
        {/if}
      {/if}
    {/each}
    {#if active !== null && ins.masks[active] && ins.masks[active].mask.shape !== "Ellipse" && !drawing}
      {@const am = ins.masks[active]}
      {#each am.handles as h, j}
        {@const c = am.curves[j]}
        {#if isCurved(c)}
          {@const [px, py] = toBox(h)}
          {@const [ix, iy] = toBox([h[0] + c[0], h[1] + c[1]])}
          {@const [ox, oy] = toBox([h[0] + c[2], h[1] + c[3]])}
          <line x1={px} y1={py} x2={ix} y2={iy} class="curvearm" stroke={MASK} />
          <line x1={px} y1={py} x2={ox} y2={oy} class="curvearm" stroke={MASK} />
          <circle cx={ix} cy={iy} r="4" class="curvehandle" stroke={MASK} onpointerdown={(e) => dragCurveHandle(e, active!, j, false)} role="presentation"><title>Curve handle: drag to shape the curve (Alt: bend this side only)</title></circle>
          <circle cx={ox} cy={oy} r="4" class="curvehandle" stroke={MASK} onpointerdown={(e) => dragCurveHandle(e, active!, j, true)} role="presentation"><title>Curve handle: drag to shape the curve (Alt: bend this side only)</title></circle>
        {/if}
      {/each}
    {/if}
    {#each ins.masks as m, i}
      {#if m.mask.enabled}
        {#each m.handles as h, j}
          {@const [x, y] = toBox(h)}
          {#if i === active && app.activeMaskPoint === j}
            <circle cx={x} cy={y} r="8" class="pointring" stroke={MASK} />
          {/if}
          <rect
            x={x - (i === active ? 4.5 : 3.5)}
            y={y - (i === active ? 4.5 : 3.5)}
            width={i === active ? 9 : 7}
            height={i === active ? 9 : 7}
            class="handle"
            fill={i === active ? MASK : "rgba(0,0,0,0.65)"}
            stroke={MASK}
            onpointerdown={(e) => dragMask(e, i, j)}
            ondblclick={(e) => (e.stopPropagation(), m.mask.shape !== "Ellipse" && run(api.maskPointCurve(ins.id, i, j)))}
            oncontextmenu={(e) => pointMenu(e, i, j)}
            role="presentation"
          ><title>Drag to move this point · Alt-drag to pull out a curve · double-click: curve / sharp corner · right-click: add or delete points</title></rect>
        {/each}
      {/if}
    {/each}

    <!-- Energy blades: a line from hilt to tip, with handles at both ends. -->
    {#if !drawing}
      {#each blades as b}
        {@const [hx, hy] = toBox(b.hilt)}
        {@const [tx, ty] = toBox(b.tip)}
        <line x1={hx} y1={hy} x2={tx} y2={ty} class="bladeline" onpointerdown={(e) => dragBlade(e, b, "both")} role="presentation" />
        <line x1={hx} y1={hy} x2={tx} y2={ty} class="bladeaxis" />
        <circle cx={tx} cy={ty} r="6" class="bladeend" stroke={b.colour} onpointerdown={(e) => dragBlade(e, b, "tip")} role="presentation"><title>Blade tip: drag to aim (Shift keeps the length)</title></circle>
        <circle cx={hx} cy={hy} r="8" class="bladeend hilt" stroke={b.colour} onpointerdown={(e) => dragBlade(e, b, "hilt")} role="presentation"><title>Blade hilt: drag to move this end</title></circle>
        <text x={hx} y={hy + 4} text-anchor="middle" class="bladetag">H</text>
      {/each}
    {/if}

    <!-- Particle emitters: the emitter's shape, a handle to move it and an arrow to aim it. -->
    {#if !drawing}
      {#each emitters as m}
        {@const [cx, cy] = toBox(m.at)}
        {@const rad = (m.dir * Math.PI) / 180}
        {@const [ax, ay] = [cx + Math.cos(rad) * ARROW, cy - Math.sin(rad) * ARROW]}
        {@const edge = toBox([m.at[0] + m.extent / 2, m.at[1]])[0] - cx}
        {#if m.shape === 1}
          <line x1={cx - Math.sin(rad) * edge} y1={cy - Math.cos(rad) * edge} x2={cx + Math.sin(rad) * edge} y2={cy + Math.cos(rad) * edge} class="bladeaxis" />
        {:else if m.shape === 2 || m.shape === 3}
          <circle {cx} {cy} r={Math.abs(edge)} class="bladeaxis emitshape" />
        {/if}
        {#if m.shape !== 3 && m.shape !== 4}
          <line x1={cx} y1={cy} x2={ax} y2={ay} class="bladeaxis" />
          <circle cx={ax} cy={ay} r="5" class="bladeend" stroke="var(--accent)" onpointerdown={(e) => dragEmitter(e, m, "aim")} role="presentation"><title>Direction: drag to aim the particles (Shift snaps to 15°)</title></circle>
        {/if}
        {#if m.shape !== 4}
          <circle {cx} {cy} r="8" class="bladeend" stroke="var(--accent)" onpointerdown={(e) => dragEmitter(e, m, "move")} role="presentation"><title>Particle emitter: drag to move it</title></circle>
          <text x={cx} y={cy + 4} text-anchor="middle" class="bladetag">✦</text>
        {/if}
      {/each}
    {/if}

    <!-- Lens flares: drag the light. -->
    {#if !drawing}
      {#each flares as m}
        {@const [cx, cy] = toBox(m.at)}
        <circle {cx} {cy} r="9" class="bladeend" stroke="#ffd27a" onpointerdown={(e) => dragEmitter(e, m, "move")} role="presentation"><title>Lens flare: drag the light</title></circle>
        <text x={cx} y={cy + 4} text-anchor="middle" class="bladetag">☀</text>
      {/each}
    {/if}

    <!-- OpenFX plugins' positions: drag them. -->
    {#if !drawing}
      {#each wires as [w0, w1]}
        {@const [ax, ay] = toBox(w0)}
        {@const [bx, by] = toBox(w1)}
        <line x1={ax} y1={ay} x2={bx} y2={by} class="bladeaxis" />
      {/each}
      {#each fxPoints as m (m.index + ":" + m.param)}
        {@const [cx, cy] = toBox(m.at)}
        <g class="fxpoint">
          <circle {cx} {cy} r="7" class="bladeend" stroke="#7ad7ff" onpointerdown={(e) => dragFxPoint(e, m)} role="presentation"><title>{m.label}: drag to move (Shift: straight)</title></circle>
          <path d="M{cx - 11} {cy}H{cx - 4}M{cx + 4} {cy}H{cx + 11}M{cx} {cy - 11}V{cy - 4}M{cx} {cy + 4}V{cy + 11}" class="fxcross" />
          <text x={cx + 12} y={cy - 9} class="fxlabel">{m.label}</text>
        </g>
      {/each}
    {/if}

    <!-- The motion tracker: search area, feature box and path for each point. -->
    {#if ins.tracker}
      {@const t = ins.tracker}
      {#if t.mode === "Mesh"}
        <!-- A mesh tracker: the grid, big handles at its corners, small ones inside. -->
        {@const [gc, gr] = t.grid}
        {@const row = (j: number) => t.shown.slice(j * (gc + 1), (j + 1) * (gc + 1))}
        {@const col = (i: number) => Array.from({ length: gr + 1 }, (_, j) => t.shown[j * (gc + 1) + i])}
        <polygon points={pts([...row(0), ...col(gc).slice(1), ...row(gr).reverse().slice(1), ...col(0).reverse().slice(1, -1)])} class="plane meshplane" onpointerdown={meshBoxDown} role="presentation"><title>Shift+drag: pick the mesh points in a box (Shift+click a point to pick it or drop it); dragging a picked point moves them all</title></polygon>
        {#each Array.from({ length: gr + 1 }, (_, j) => j) as j}<polyline points={pts(row(j))} class="meshline" />{/each}
        {#each Array.from({ length: gc + 1 }, (_, i) => i) as i}<polyline points={pts(col(i))} class="meshline" />{/each}
        {#each t.shown as c, i}
          {@const corner = i === 0 || i === gc || i === (gr + 1) * (gc + 1) - 1 || i === gr * (gc + 1)}
          {@const [cx, cy] = toBox(c)}
          <circle cx={cx} cy={cy} r={corner ? 6 : gc >= 32 || gr >= 32 ? 2 : gc >= 16 || gr >= 16 ? 2.5 : 3.5} class="corner" class:inner={!corner} class:picked={app.meshSel?.clip === ins.id && app.meshSel.points.includes(i)} stroke={corner ? POINT_COLOURS[[0, gc, (gr + 1) * (gc + 1) - 1, gr * (gc + 1)].indexOf(i)] : "white"} onpointerdown={(e) => dragTracker(e, i)} role="presentation" />
        {/each}
        {#if meshBox}<rect x={Math.min(meshBox.a[0], meshBox.b[0])} y={Math.min(meshBox.a[1], meshBox.b[1])} width={Math.abs(meshBox.b[0] - meshBox.a[0])} height={Math.abs(meshBox.b[1] - meshBox.a[1])} class="meshbox" />{/if}
      {:else if t.mode === "Planar"}
        <!-- A planar tracker: the surface's outline, with a handle at each corner. -->
        <polygon points={pts(t.shown)} class="plane" />
      {:else if t.shown.length > 1}
        <polyline points={pts(t.shown.length > 2 ? [...t.shown, t.shown[0]] : t.shown)} class="joint" />
      {/if}
      {#each t.mode === "Mesh" ? [] : t.shown as c, i}
        {#if t.mode === "Planar"}
          {@const col = POINT_COLOURS[i % 4]}
          {#if t.paths[i]?.length > 1}
            <polyline points={pts(t.paths[i])} class="path" class:raw={t.smoothed.length > 0} stroke={col} />
          {/if}
          {#if t.smoothed[i]?.length > 1}
            <polyline points={pts(t.smoothed[i])} class="path smooth" stroke={col} />
          {/if}
          {@const [cx, cy] = toBox(c)}
          <circle cx={cx} cy={cy} r="6" class="corner" stroke={col} onpointerdown={(e) => dragTracker(e, i)} role="presentation" />
          <text x={cx + 8} y={cy - 7} fill={col}>{i + 1}</text>
        {:else}
          {@const col = POINT_COLOURS[i % 4]}
          {#if t.paths[i]?.length > 1}
            <polyline points={pts(t.paths[i])} class="path" class:raw={t.smoothed.length > 0} stroke={col} />
          {/if}
          {#if t.smoothed[i]?.length > 1}
            <polyline points={pts(t.smoothed[i])} class="path smooth" stroke={col} />
          {/if}
          <polygon points={pts(square(c, t.size / 2 + t.search))} class="search" stroke={col} />
          <polygon
            points={pts(square(c, t.size / 2))}
            class="feature"
            stroke={col}
            onpointerdown={(e) => dragTracker(e, i)}
            role="presentation"
          />
          {@const [cx, cy] = toBox(c)}
          <circle cx={cx} cy={cy} r="2.5" fill={col} />
          {#if t.shown.length > 1}
            {@const [lx, ly] = toBox(square(c, t.size / 2)[0])}
            <text x={lx + 2} y={ly - 3} fill={col}>{i + 1}</text>
          {/if}
        {/if}
      {/each}
    {/if}

    <!-- Move / Resize mask tools: dragging anywhere picks the mask under the pointer. -->
    {#if tool}
      <rect x="0" y="0" width="100%" height="100%" class="catch tool-{tool}" onpointerdown={toolDown} role="presentation" />
    {/if}

    <!-- The key-colour eyedropper. -->
    {#if keyPick}
      <rect x="0" y="0" width="100%" height="100%" class="catch picker" onpointerdown={pickDown} role="presentation" />
      {#if pickBox}
        {@const [a, b] = pickBox}
        <polygon points={pts([a, [b[0], a[1]], b, [a[0], b[1]]])} class="pickbox" />
      {/if}
    {/if}

    <!-- Paint effect strokes. -->
    {#if painting && !roto && !drawing}
      <rect x="0" y="0" width="100%" height="100%" class="catch roto" onpointerdown={paintDown} onpointermove={(e) => (brushAt = toBoxPt(e))} onpointerleave={() => (brushAt = null)} role="presentation" />
      {#if painting.source}
        {@const sp = toBox(painting.source)}
        <circle cx={sp[0]} cy={sp[1]} r={6} class="brush" />
        <line x1={sp[0] - 9} y1={sp[1]} x2={sp[0] + 9} y2={sp[1]} class="brush" />
        <line x1={sp[0]} y1={sp[1] - 9} x2={sp[0]} y2={sp[1] + 9} class="brush" />
      {/if}
      {#if stroke && stroke.points.length}
        <polyline points={pts(stroke.points)} class="rotostroke paint" stroke-width={paintRadius * 2 * layerScale()} />
      {/if}
      {#if brushAt}
        <circle cx={brushAt[0]} cy={brushAt[1]} r={paintRadius * layerScale()} class="brush" />
      {/if}
    {/if}

    <!-- Roto brush painting. -->
    {#if roto && !drawing}
      <rect
        x="0"
        y="0"
        width="100%"
        height="100%"
        class="catch roto"
        onpointerdown={rotoDown}
        onpointermove={(e) => (brushAt = toBoxPt(e))}
        onpointerleave={() => (brushAt = null)}
        role="presentation"
      />
      {#if stroke && stroke.points.length}
        <polyline points={pts(stroke.points)} class="rotostroke" class:bg={!stroke.fg} stroke-width={roto.size * 2} />
      {/if}
      {#if brushAt}
        <circle cx={brushAt[0]} cy={brushAt[1]} r={roto.size} class="brush" />
      {/if}
    {/if}

    <!-- Freehand drawing. -->
    {#if drawing}
      <rect
        x="0"
        y="0"
        width="100%"
        height="100%"
        class="catch"
        onclick={drawClick}
        ondblclick={(e) => (e.stopPropagation(), finishDrawing())}
        onpointermove={(e) => (hover = toLayer(e))}
        onpointerdown={(e) => e.stopPropagation()}
        role="presentation"
      />
      {#if drawing.points.length > 1}
        <polyline points={pts(drawing.points)} class="draw" stroke={MASK} />
      {/if}
      {#if drawing.points.length && hover}
        <polyline points={pts([drawing.points[drawing.points.length - 1], hover])} class="draw ghost" stroke={MASK} />
      {/if}
      {#each drawing.points as p, k}
        {@const [x, y] = toBox(p)}
        <circle cx={x} cy={y} r={k === 0 && drawing.points.length >= 3 ? 5.5 : 3} fill={MASK} />
      {/each}
    {/if}
  </svg>
{/if}

<style>
  .overlay {
    position: absolute;
    inset: 0;
    width: 100%;
    height: 100%;
    pointer-events: none;
    overflow: visible;
    z-index: 2;
  }
  .campt line {
    stroke-width: 1.6;
    pointer-events: none;
    filter: drop-shadow(0 0 1px rgba(0, 0, 0, 0.9));
  }
  .campt.on line {
    stroke-width: 2;
  }
  .campick {
    fill: none;
    stroke: var(--accent);
    stroke-width: 2;
    pointer-events: none;
  }
  .camhit {
    fill: transparent;
    stroke: none;
    pointer-events: all;
    cursor: pointer;
  }
  .cambox-catch {
    fill: transparent;
    pointer-events: all;
    cursor: crosshair;
  }
  .overlay.drawing {
    cursor: crosshair;
  }
  .mask {
    fill: transparent;
    stroke-width: 1;
    pointer-events: none;
  }
  .mask.on {
    stroke-width: 1.8;
    fill: rgba(255, 110, 200, 0.06);
    pointer-events: fill;
    cursor: move;
  }
  /* A key's garbage matte: dashed, so it's told apart from masks that cut. */
  .mask.keep {
    stroke-dasharray: 6 4;
  }
  /* Left out of the camera track: dotted. */
  .mask.ignore {
    stroke-dasharray: 2 4;
  }
  .mask.off {
    opacity: 0.35;
  }
  .handle {
    stroke-width: 1;
    pointer-events: all;
    cursor: crosshair;
  }
  .curvearm {
    stroke-width: 1;
    opacity: 0.85;
    pointer-events: none;
  }
  .curvehandle {
    fill: rgba(0, 0, 0, 0.7);
    stroke-width: 1.5;
    pointer-events: all;
    cursor: pointer;
  }
  .pointring {
    fill: none;
    stroke-width: 1.2;
    stroke-dasharray: 3 2;
    pointer-events: none;
  }
  .plane {
    fill: rgba(242, 169, 59, 0.08);
    stroke: rgba(255, 255, 255, 0.8);
    stroke-width: 1.4;
    stroke-dasharray: 6 4;
    pointer-events: none;
  }
  /* A mesh's area takes Shift+drag (a box to pick points); other presses pass on. */
  .meshplane {
    pointer-events: visiblePainted;
  }
  .corner.picked,
  .corner.inner.picked {
    fill: var(--accent);
    stroke: white;
  }
  .meshbox {
    fill: rgba(242, 169, 59, 0.12);
    stroke: var(--accent);
    stroke-dasharray: 4 3;
    pointer-events: none;
  }
  .meshline {
    fill: none;
    stroke: rgba(255, 255, 255, 0.75);
    stroke-width: 1;
    pointer-events: none;
  }
  .corner.inner {
    stroke-width: 1.5;
    fill: rgba(0, 0, 0, 0.3);
  }
  .corner {
    fill: rgba(0, 0, 0, 0.35);
    stroke-width: 2;
    cursor: move;
    pointer-events: all;
  }
  .joint {
    fill: none;
    stroke: rgba(255, 255, 255, 0.6);
    stroke-width: 1.2;
    stroke-dasharray: 6 4;
  }
  .path {
    fill: none;
    stroke-width: 1;
    opacity: 0.6;
  }
  .path.raw {
    opacity: 0.25;
    stroke-dasharray: 2 3;
  }
  .path.smooth {
    opacity: 0.9;
    stroke-width: 1.5;
  }
  .search {
    fill: none;
    stroke-width: 1;
    opacity: 0.4;
  }
  .feature {
    fill: rgba(0, 0, 0, 0.01);
    stroke-width: 1.5;
    pointer-events: all;
    cursor: crosshair;
  }
  text {
    font-size: calc(12px * var(--text-scale));
    font-weight: 600;
  }
  .catch {
    fill: transparent;
    pointer-events: all;
  }
  .tool-move {
    cursor: move;
  }
  .picker {
    cursor: crosshair;
  }
  .maskline {
    fill: none;
    stroke: transparent;
    stroke-width: 12;
    pointer-events: stroke;
  }
  .pickbox {
    fill: rgba(255, 255, 255, 0.12);
    stroke: #fff;
    stroke-width: 1.5;
    stroke-dasharray: 4 3;
    pointer-events: none;
  }
  .tool-resize {
    cursor: nwse-resize;
  }
  .draw {
    fill: none;
    stroke-width: 1.5;
  }
  .scenept {
    stroke: #ffd246;
    stroke-width: 2;
    fill: rgba(0, 0, 0, 0.45);
    pointer-events: all;
    cursor: move;
  }
  .scenept.light {
    fill: rgba(255, 210, 70, 0.35);
  }
  .ray {
    stroke: #ffd246;
    stroke-width: 1.5;
    pointer-events: none;
  }
  .bladeline {
    stroke: transparent;
    stroke-width: 14;
    pointer-events: stroke;
    cursor: move;
  }
  .plink {
    stroke: var(--accent);
    stroke-width: 1.25;
    stroke-dasharray: 4 4;
    opacity: 0.85;
  }
  .plinkend {
    fill: var(--accent);
    stroke: rgba(0, 0, 0, 0.6);
    stroke-width: 1;
  }
  .plinkend.parent {
    fill: none;
    stroke: var(--accent);
    stroke-width: 1.5;
  }
  .bladeaxis {
    stroke: rgba(255, 255, 255, 0.75);
    stroke-width: 1;
    stroke-dasharray: 5 4;
    pointer-events: none;
  }
  .bladeend {
    fill: rgba(0, 0, 0, 0.45);
    stroke-width: 2;
    pointer-events: all;
    cursor: crosshair;
  }
  .emitshape {
    fill: none;
  }
  .bladetag {
    fill: white;
    font-size: calc(10px * var(--text-scale));
    pointer-events: none;
  }
  .fxcross {
    stroke: #7ad7ff;
    stroke-width: 1.5;
    pointer-events: none;
  }
  .fxlabel {
    fill: #cfefff;
    font-size: calc(10.5px * var(--text-scale));
    paint-order: stroke;
    stroke: rgba(0, 0, 0, 0.7);
    stroke-width: 3px;
    pointer-events: none;
  }
  .draw.ghost {
    opacity: 0.6;
  }
  .catch.roto {
    cursor: none;
  }
  .rotostroke {
    fill: none;
    stroke: rgba(80, 220, 120, 0.45);
    stroke-linecap: round;
    stroke-linejoin: round;
    pointer-events: none;
  }
  .rotostroke.paint {
    stroke: rgba(255, 255, 255, 0.35);
  }
  .rotostroke.bg {
    stroke: rgba(240, 80, 80, 0.45);
  }
  .brush {
    fill: none;
    stroke: white;
    stroke-width: 1.2;
    stroke-dasharray: 3 2;
    pointer-events: none;
  }
</style>
