<script lang="ts">
  // File > Export…: presets (built in and your own), every setting, and
  // Export (runs the queue now) or Add to queue (to set up another first).
  import { app, run, timecode, tps, fps } from "../lib/session.svelte";
  import { api, type ExportOptions, type Spec, type Size, type UserPreset, type VideoCodec } from "../lib/api";

  let opts = $state<ExportOptions | null>(null);
  let spec = $state<Spec | null>(null);
  let path = $state("");
  let includeAudio = $state(true);
  let markedOnly = $state(false);
  let hasAudio = $state(true);
  /// The saved preset these settings came from (while they still match it).
  let userPreset = $state<string | null>(null);
  /// The name typed for "Save as preset" (while that box is open).
  let saveAs = $state<string | null>(null);
  let saveInput = $state<HTMLInputElement | null>(null);

  // The last settings used, to start the next export from.
  let last: { spec: Spec; includeAudio: boolean; userPreset: string | null } | null = null;

  const seq = $derived(app.s?.seq ?? null);
  const hasMarks = $derived(seq?.in_point != null || seq?.out_point != null);
  const saved = $derived<UserPreset[]>(app.s?.prefs.export_presets ?? []);
  const codec = $derived(opts && spec?.video ? opts.codecs.find((c) => c.id === spec!.video) ?? null : null);
  const ext = $derived(codec?.extension ?? "wav");
  /// The file types this format can be written in (the extension picks the container).
  const exts = $derived(codec?.extensions ?? ["wav"]);
  /// The current file's extension.
  const pathExt = $derived.by(() => {
    const v = path.trim();
    const dot = v.lastIndexOf(".");
    const sep = Math.max(v.lastIndexOf("/"), v.lastIndexOf("\\"));
    return dot > sep ? v.slice(dot + 1).toLowerCase() : "";
  });
  /// The codecs under their headings, in order.
  const groups = $derived.by(() => {
    const out: [string, NonNullable<typeof opts>["codecs"]][] = [];
    for (const c of opts?.codecs ?? []) {
      const g = out.find(([n]) => n === c.group);
      if (g) g[1].push(c);
      else out.push([c.group, [c]]);
    }
    return out;
  });
  // DNxHD only comes in a few sizes.
  const DNXHD_SIZES: [number, number][] = [[1920, 1080], [1440, 1080], [1280, 720], [960, 720]];
  const DNXHD10_SIZES: [number, number][] = [[1920, 1080], [1280, 720]];
  const busy = $derived((app.live?.export.jobs ?? []).some((j) => j.state === "waiting" || j.state === "running"));

  const same = (a: unknown, b: unknown) => JSON.stringify(a) === JSON.stringify(b);
  const norm = (s: Spec): Spec => ({ video: s.video, size: s.size, quality: s.quality, fit: s.fit, hardware: s.hardware, color: s.color, bitrate_kbps: s.bitrate_kbps ?? null, alpha: !!s.alpha, frame_rate: s.frame_rate ?? null, between: s.between ?? null });
  // Frame rates to write at; the sequence's own is the first choice.
  const RATES: [string, number, number][] = [["23.976", 24000, 1001], ["24", 24, 1], ["25", 25, 1], ["29.97", 30000, 1001], ["30", 30, 1], ["50", 50, 1], ["59.94", 60000, 1001], ["60", 60, 1], ["120", 120, 1]];
  const outFps = $derived(spec?.frame_rate ? spec.frame_rate.num / spec.frame_rate.den : fps());
  const builtIn = $derived(opts && spec ? opts.presets.find((p) => p.spec && same(norm(p.spec), norm(spec!))) ?? null : null);
  // What the preset box shows.
  const presetValue = $derived(userPreset && saved.some((u) => u.name === userPreset && same(norm(u.spec), norm(spec!))) ? `u:${userPreset}` : builtIn ? `b:${builtIn.index}` : "custom");
  const hint = $derived(presetValue.startsWith("u:") ? "Your saved preset." : builtIn ? builtIn.hint : "Your own settings. Save them as a preset to use them again.");

  // Opening: start from the last settings, or a preset that fits the sequence.
  let wasOpen = false;
  $effect(() => {
    const open = app.exportOpen;
    if (open && !wasOpen) start();
    wasOpen = open;
  });
  async function start() {
    opts ??= await api.exportOptions();
    const sug = await api.exportSuggest("mp4");
    hasAudio = sug.has_audio;
    markedOnly = hasMarks;
    saveAs = null;
    if (last) {
      spec = $state.snapshot(last.spec) as Spec;
      includeAudio = last.includeAudio;
      userPreset = last.userPreset;
    } else {
      spec = $state.snapshot(opts.presets[sug.preset].spec!) as Spec;
      includeAudio = true;
      userPreset = null;
    }
    const s = await api.exportSuggest(ext);
    path = s.path;
  }

  /// Make the file name's extension match the format (any type it can go in), or set it to `want`.
  function fixExtension(want?: string) {
    const v = path.trim();
    if (!v) return;
    const dot = v.lastIndexOf(".");
    const sep = Math.max(v.lastIndexOf("/"), v.lastIndexOf("\\"));
    const base = dot > sep ? v.slice(0, dot) : v;
    const now = dot > sep ? v.slice(dot + 1).toLowerCase() : "";
    const to = want ?? (exts.includes(now) ? now : ext);
    if (now !== to) path = `${base}.${to}`;
  }

  function pickPreset(v: string) {
    if (!opts) return;
    if (v.startsWith("b:")) {
      const p = opts.presets[+v.slice(2)];
      if (p.spec) spec = $state.snapshot(p.spec) as Spec;
      userPreset = null;
    } else if (v.startsWith("u:")) {
      const u = saved.find((x) => x.name === v.slice(2));
      if (u) {
        spec = $state.snapshot(u.spec) as Spec;
        includeAudio = u.include_audio;
        userPreset = u.name;
      }
    }
    fixExtension();
  }

  function setCodec(v: string) {
    if (!spec) return;
    spec.video = v === "wav" ? null : (v as VideoCodec);
    const c = opts?.codecs.find((x) => x.id === spec!.video);
    if (c && !c.bitrate) spec.bitrate_kbps = null;
    if (c && !c.hardware) spec.hardware = false;
    if (c && !c.ten_bit && !c.hdr) spec.color = "Sdr8";
    if (c && !c.alpha) spec.alpha = false;
    fixExtension();
  }

  // ---- Size ----
  const resolve = (s: Size): [number, number] => {
    const sw = seq?.width ?? 1920;
    const sh = seq?.height ?? 1080;
    if ("Scaled" in s) return [Math.max(2, Math.floor(sw / s.Scaled) & ~1), Math.max(2, Math.floor(sh / s.Scaled) & ~1)];
    return [Math.max(2, s.Fixed[0]), Math.max(2, s.Fixed[1])];
  };
  const out = $derived(spec ? resolve(spec.size) : ([0, 0] as [number, number]));
  const isCommon = (w: number, h: number) => !!opts?.sizes.some(([a, b]) => a === w && b === h);
  const sizeValue = $derived(!spec ? "" : "Scaled" in spec.size ? `s:${spec.size.Scaled}` : isCommon(spec.size.Fixed[0], spec.size.Fixed[1]) ? `f:${spec.size.Fixed[0]}x${spec.size.Fixed[1]}` : "custom");
  function setSize(v: string) {
    if (!spec) return;
    if (v.startsWith("s:")) spec.size = { Scaled: +v.slice(2) };
    else if (v.startsWith("f:")) {
      const [w, h] = v.slice(2).split("x").map(Number);
      spec.size = { Fixed: [w, h] };
    } else {
      const [w, h] = out;
      spec.size = { Fixed: [w + 2, h] }; // off the common sizes, so the fields show
    }
  }
  function setCustom(i: 0 | 1, v: number) {
    if (!spec || !("Fixed" in spec.size)) return;
    const f: [number, number] = [...spec.size.Fixed];
    f[i] = Math.min(8192, Math.max(16, (Math.round(v) + 1) & ~1));
    spec.size = { Fixed: f };
  }
  const shapeDiffers = $derived(!!seq && Math.abs(out[0] / out[1] / (seq.width / seq.height) - 1) > 0.005);

  // ---- Rate ----
  function suggestedKbps(w: number, h: number, c: VideoCodec): number {
    const bpp = c === "H265" ? 0.08 : 0.13;
    return Math.max(500, Math.round((w * h * 30 * bpp) / 1000 / 100) * 100);
  }
  const range = $derived.by((): [number, number] => {
    if (!seq) return [0, 0];
    if (markedOnly && hasMarks) return [seq.in_point ?? 0, seq.out_point ?? seq.duration];
    return [0, seq.duration];
  });
  const secs = $derived(Math.max(0, range[1] - range[0]) / tps());
  // The estimated file size and the free space where it goes (asked again,
  // a moment after anything that changes them).
  let estimate = $state<{ need: number; free: number | null; fits: boolean } | null>(null);
  $effect(() => {
    if (!app.exportOpen || !spec || secs === 0) {
      estimate = null;
      return;
    }
    const args = [$state.snapshot(spec) as Spec, path, includeAudio, markedOnly && hasMarks] as const;
    const t = setTimeout(() => {
      api.exportEstimate(...args).then((r) => (estimate = r), () => (estimate = null));
    }, 200);
    return () => clearTimeout(t);
  });
  const gb = (b: number) => (b >= 1e9 ? `${(b / 1e9).toFixed(1)} GB` : `${Math.max(1, Math.round(b / 1e6))} MB`);
  const sizeMb = $derived(spec?.bitrate_kbps ? ((spec.bitrate_kbps * 1000) / 8 * secs + (includeAudio && hasAudio ? 24000 * secs : 0)) / 1e6 : 0);
  const frames = $derived(Math.round(secs * outFps));

  // ---- Presets ----
  async function savePreset() {
    const name = saveAs?.trim();
    if (!name || !spec) return;
    await run(api.exportPresetSave(name, $state.snapshot(spec) as Spec, includeAudio));
    userPreset = name;
    saveAs = null;
  }
  async function deletePreset() {
    if (!userPreset) return;
    await run(api.exportPresetDelete(userPreset));
    userPreset = null;
  }
  $effect(() => {
    if (saveAs !== null && saveInput) saveInput.focus();
  });

  async function browse() {
    const r = await api.exportPathDialog(path, ext);
    if (r) {
      path = r;
      fixExtension();
    }
  }

  async function go(now: boolean) {
    if (!spec) return;
    fixExtension();
    if (!path.trim()) await browse();
    if (!path.trim()) return;
    const label = presetValue.startsWith("u:") ? userPreset : builtIn && builtIn.label !== "Custom" ? builtIn.label : null;
    const ok = await run(api.exportAdd($state.snapshot(spec) as Spec, path, includeAudio, markedOnly && hasMarks, label, now).then(() => true));
    if (!ok) return;
    last = { spec: $state.snapshot(spec) as Spec, includeAudio, userPreset };
    app.exportOpen = false;
    app.queueOpen = true;
  }

  function key(e: KeyboardEvent) {
    if (!app.exportOpen || e.key !== "Escape") return;
    e.stopPropagation();
    if (saveAs !== null) saveAs = null;
    else app.exportOpen = false;
  }
  const canGo = $derived(!!spec && secs > 0 && (spec.video !== null || hasAudio));
</script>

<svelte:window onkeydown={key} />

{#if app.exportOpen && opts && spec}
  <div class="scrim">
    <div class="dialog" role="dialog" aria-modal="true" aria-labelledby="exp-title">
      <h2 id="exp-title">Export {seq?.name ?? ""}</h2>
      <div class="form">
        <span class="lbl">Preset</span>
        <div class="col">
          <div class="row">
            <select value={presetValue} aria-label="Preset" onchange={(e) => pickPreset(e.currentTarget.value)}>
              {#if presetValue === "custom"}<option value="custom">Custom</option>{/if}
              {#each opts.presets.filter((p) => p.spec) as p}<option value="b:{p.index}">{p.label}</option>{/each}
              {#if saved.length}
                <optgroup label="Your presets">
                  {#each saved as u}<option value="u:{u.name}">★ {u.name}</option>{/each}
                </optgroup>
              {/if}
            </select>
            <button onclick={() => (saveAs = presetValue.startsWith("u:") ? userPreset ?? "" : "")} title="Keep these settings under a name, for next time">Save as preset…</button>
            {#if presetValue.startsWith("u:")}<button onclick={deletePreset} title="Forget the saved preset “{userPreset}”">Delete</button>{/if}
          </div>
          {#if saveAs !== null}
            <div class="row save">
              <input
                bind:this={saveInput}
                type="text"
                bind:value={saveAs}
                placeholder="e.g. Client review 720p"
                aria-label="Preset name"
                onkeydown={(e) => {
                  if (e.key === "Enter") savePreset();
                }}
              />
              <button class="primary" disabled={!saveAs.trim()} onclick={savePreset}>{saved.some((u) => u.name === saveAs?.trim()) ? "Replace" : "Save"}</button>
              <button onclick={() => (saveAs = null)}>Cancel</button>
            </div>
          {/if}
          <p class="hint">{hint}</p>
        </div>

        <span class="lbl">File</span>
        <div class="row">
          <input type="text" bind:value={path} aria-label="Export file" onblur={() => fixExtension()} />
          <button onclick={browse}>Browse…</button>
        </div>

        <span class="lbl">Format</span>
        <div class="col">
          <select value={spec.video ?? "wav"} aria-label="Format" onchange={(e) => setCodec(e.currentTarget.value)}>
            {#each groups as [name, list]}
              <optgroup label={name}>
                {#each list as c}<option value={c.id}>{c.label}</option>{/each}
              </optgroup>
            {/each}
            <optgroup label="Audio">
              <option value="wav">Audio only (WAV)</option>
            </optgroup>
          </select>
          {#if codec}<p class="hint">{codec.hint}</p>{/if}
          {#if codec?.sequence}
            {@const stem = (path.split(/[\\/]/).pop() ?? "frame").replace(/\.[^.]*$/, "")}
            <p class="hint">Writes {stem}_00001.{ext}, {stem}_00002.{ext}, … (no sound)</p>
          {/if}
        </div>

        {#if codec && exts.length > 1}
          <span class="lbl">File type</span>
          <div class="col">
            <div class="row">
              {#each exts as x}
                <label class="radio"><input type="radio" name="container" checked={pathExt === x} onchange={() => fixExtension(x)} /> {x.toUpperCase()}</label>
              {/each}
            </div>
            {#if pathExt === "mxf"}<p class="hint">MXF (OP1a) is what Avid Media Composer links to and imports.</p>{/if}
          </div>
        {/if}

        {#if codec}
          <span class="lbl">Size</span>
          <div class="row">
            <select value={sizeValue} aria-label="Size" onchange={(e) => setSize(e.currentTarget.value)}>
              <option value="s:1">Sequence size ({resolve({ Scaled: 1 }).join("×")})</option>
              <option value="s:2">Half ({resolve({ Scaled: 2 }).join("×")})</option>
              <option value="s:4">Quarter ({resolve({ Scaled: 4 }).join("×")})</option>
              <option disabled>──────────</option>
              {#each opts.sizes as [w, h, name]}<option value="f:{w}x{h}">{name} ({w}×{h})</option>{/each}
              <option disabled>──────────</option>
              <option value="custom">{sizeValue === "custom" ? `Custom (${out[0]}×${out[1]})` : "Custom…"}</option>
            </select>
            {#if sizeValue === "custom"}
              <input class="dim" type="number" min="16" max="8192" step="2" value={out[0]} aria-label="Width" onchange={(e) => setCustom(0, +e.currentTarget.value)} />
              <span>×</span>
              <input class="dim" type="number" min="16" max="8192" step="2" value={out[1]} aria-label="Height" onchange={(e) => setCustom(1, +e.currentTarget.value)} />
            {/if}
          </div>

          {#if spec.video === "Dnxhd" || spec.video === "Dnxhd10"}
            {@const sizes = spec.video === "Dnxhd" ? DNXHD_SIZES : DNXHD10_SIZES}
            {#if !sizes.some(([w, h]) => w === out[0] && h === out[1])}
              <span></span>
              <div class="col">
                <p class="hint warn">
                  {spec.video === "Dnxhd" ? "DNxHD" : "DNxHD 10-bit"} only comes in {sizes.map(([w, h]) => `${w}×${h}`).join(", ")}. Pick one of those, or use DNxHR, which takes any size.
                </p>
                <div class="row">
                  {#each sizes as [w, h]}<button onclick={() => (spec!.size = { Fixed: [w, h] })}>{w}×{h}</button>{/each}
                </div>
              </div>
            {/if}
          {/if}

          {#if shapeDiffers}
            <span class="lbl">Shape</span>
            <div class="col">
              <div class="row">
                {#each opts.fits as [f, label]}
                  <label class="radio"><input type="radio" name="fit" checked={spec.fit === f} onchange={() => (spec!.fit = f)} /> {label}</label>
                {/each}
              </div>
              <p class="hint">The sequence is {seq?.width}×{seq?.height}; this frame is a different shape.</p>
            </div>
          {/if}

          {#if codec.bitrate}
            <span class="lbl">Rate</span>
            <div class="row wrap">
              <label class="radio" title="Constant quality: the file is as big as the picture needs">
                <input type="radio" name="rate" checked={spec.bitrate_kbps == null} onchange={() => (spec!.bitrate_kbps = null)} /> Quality
              </label>
              <select value={spec.quality} disabled={spec.bitrate_kbps != null} aria-label="Quality" onchange={(e) => (spec!.quality = e.currentTarget.value as Spec["quality"])}>
                {#each opts.qualities as [q, label]}<option value={q}>{label}</option>{/each}
              </select>
              <label class="radio" title="A target average bitrate: a predictable file size (peaks go up to 1.5×)">
                <input type="radio" name="rate" checked={spec.bitrate_kbps != null} onchange={() => (spec!.bitrate_kbps = suggestedKbps(out[0], out[1], spec!.video!))} /> Bitrate
              </label>
              {#if spec.bitrate_kbps != null}
                <input class="dim" type="number" min="0.2" max="400" step="0.5" value={(spec.bitrate_kbps / 1000).toFixed(1)} aria-label="Bitrate in Mbit/s" onchange={(e) => (spec!.bitrate_kbps = Math.round(Math.min(400, Math.max(0.2, +e.currentTarget.value)) * 1000))} />
                <span class="muted">Mbit/s · ≈ {sizeMb.toFixed(0)} MB</span>
              {/if}
            </div>
          {:else if codec.quality}
            <span class="lbl">Quality</span>
            <select value={spec.quality} aria-label="Quality" onchange={(e) => (spec!.quality = e.currentTarget.value as Spec["quality"])}>
              {#each opts.qualities as [q, label]}<option value={q}>{label}</option>{/each}
            </select>
          {/if}

          <span class="lbl">Colour</span>
          {#if codec.ten_bit || codec.hdr}
            <div class="col">
              <select value={spec.color} aria-label="Colour" onchange={(e) => (spec!.color = e.currentTarget.value as Spec["color"])}>
                {#each opts.colors as c}<option value={c.id}>{c.label}</option>{/each}
              </select>
              <p class="hint">{opts.colors.find((c) => c.id === spec!.color)?.hint}</p>
            </div>
          {:else}
            <span class="muted" title="HDR needs H.265">{spec.video === "Cineform4444" ? "SDR, 12-bit RGB" : codec.deep ? "SDR, 10-bit" : "SDR, 8-bit"}</span>
          {/if}

          {#if codec.alpha}
            <span class="lbl">Background</span>
            <div class="col">
              <label class="check">
                <input type="checkbox" checked={!!spec.alpha} onchange={(e) => (spec!.alpha = e.currentTarget.checked)} />
                Transparent (keep the alpha channel)
              </label>
              <p class="hint">{spec.alpha ? "Where no layer covers the frame it's see-through, for putting over other footage." : "Where no layer covers the frame it's black."}</p>
            </div>
          {/if}

          <span class="lbl">Frame rate</span>
          <div class="col">
            <select
              aria-label="Frame rate"
              value={spec.frame_rate ? `${spec.frame_rate.num}/${spec.frame_rate.den}` : ""}
              onchange={(e) => {
                const v = e.currentTarget.value;
                if (!v) spec!.frame_rate = null;
                else {
                  const [num, den] = v.split("/").map(Number);
                  spec!.frame_rate = { num, den };
                }
              }}
            >
              <option value="">As the sequence ({Math.round(fps() * 1000) / 1000} fps)</option>
              {#each RATES as [name, num, den]}<option value="{num}/{den}">{name} fps</option>{/each}
            </select>
            <p class="hint">{spec.frame_rate ? "Every frame is drawn at its own moment, so titles, keyframes and effects move smoothly at this rate; footage fills in as Between frames says." : "Written at the sequence's frame rate."}</p>
          </div>
          <span class="lbl">Between frames</span>
          <div class="col">
            <select aria-label="Between frames" value={spec.between ?? ""} onchange={(e) => (spec!.between = (e.currentTarget.value || null) as Spec["between"])}>
              <option value="">As each clip says</option>
              <option value="Blend">Blend, for all footage</option>
              <option value="Flow">Optical flow, for all footage</option>
            </select>
            <p class="hint">{spec.between === "Flow" ? "Footage with fewer frames a second than the export (24p delivered at 60p) gets the frames in between made from the motion: smoothest, slowest to export." : spec.between === "Blend" ? "Footage with fewer frames than the export gets cross-faded frames in between." : "Footage with fewer frames than the export holds each frame, unless its own Between frames (in Controls) says otherwise."}</p>
          </div>

          <span class="lbl">Encoder</span>
          <label class="check" class:off={!codec.hardware} title={codec.hardware ? "Much faster, slightly bigger files at the same quality. 10-bit and HDR work too on H.265 (Main 10, with HDR10's metadata), where the card can. If no GPU encoder works, the software one is used." : "Only H.264 and H.265 have GPU encoders"}>
            <input type="checkbox" checked={spec.hardware && codec.hardware} disabled={!codec.hardware} onchange={(e) => (spec!.hardware = e.currentTarget.checked)} />
            Use the graphics card (NVENC / Quick Sync / AMF)
          </label>
        {/if}

        <span class="lbl">Sound</span>
        {#if !codec}
          <span class="muted">{hasAudio ? "48 kHz stereo, 24-bit" : "The timeline has no sound"}</span>
        {:else}
          <label class="check" class:off={!hasAudio || codec.sequence}>
            <input type="checkbox" checked={includeAudio && hasAudio && !codec.sequence} disabled={!hasAudio || codec.sequence} onchange={(e) => (includeAudio = e.currentTarget.checked)} />
            Include sound (48 kHz stereo)
            {#if codec.sequence}<span class="muted">· image sequences have none</span>{:else if !hasAudio}<span class="muted">· the timeline has no sound</span>{/if}
          </label>
        {/if}

        <span class="lbl">Range</span>
        <div class="col">
          <label class="radio"><input type="radio" name="range" checked={!markedOnly || !hasMarks} onchange={() => (markedOnly = false)} /> Whole sequence <span class="muted tc">({timecode(seq?.duration ?? 0)})</span></label>
          <label class="radio" class:off={!hasMarks}>
            <input type="radio" name="range" checked={markedOnly && hasMarks} disabled={!hasMarks} onchange={() => (markedOnly = true)} />
            In to out {#if hasMarks}<span class="muted tc">({timecode(seq?.in_point ?? 0)} – {timecode(seq?.out_point ?? seq?.duration ?? 0)})</span>{:else}<span class="muted">(set them with I and O)</span>{/if}
          </label>
          <p class="hint tc">{timecode(range[1] - range[0])} · {frames} frames</p>
        </div>
      </div>
      <div class="buttons">
        {#if secs === 0}<span class="muted">The timeline is empty.</span>{/if}
        {#if estimate}
          <span class="est" class:short={!estimate.fits} title="An estimate from the format's usual data rate (or the bitrate set), on the big side, and the free space on the drive it's going to">
            About {gb(estimate.need)}{#if estimate.free !== null} · {gb(estimate.free)} free{/if}{#if !estimate.fits} · not enough room: free some space, or export somewhere else{/if}
          </span>
        {/if}
        <span class="grow"></span>
        <button onclick={() => (app.exportOpen = false)}>Cancel</button>
        <button disabled={!canGo} onclick={() => go(false)} title="Queue it and set up another; start the queue from its window">Add to queue</button>
        <button class="primary" disabled={!canGo} onclick={() => go(true)}>{busy ? "Export after the queue" : "Export"}</button>
      </div>
    </div>
  </div>
{/if}

<style>
  .est {
    color: var(--muted);
    font-size: calc(12px * var(--text-scale));
  }
  .est.short {
    color: var(--red);
  }
  .scrim {
    position: fixed;
    inset: 0;
    background: rgba(10, 10, 12, 0.55);
    display: grid;
    place-items: center;
    z-index: 40;
  }
  .dialog {
    width: min(620px, calc(100vw - 32px));
    max-height: calc(100vh - 32px);
    overflow: auto;
    background: var(--panel);
    border: 1px solid var(--line);
    border-radius: 8px;
    padding: 18px 20px;
    box-shadow: 0 20px 60px rgba(0, 0, 0, 0.6);
    display: flex;
    flex-direction: column;
    gap: 12px;
  }
  h2 {
    font-size: calc(16px * var(--text-scale));
    font-weight: 600;
    margin: 0;
  }
  .form {
    display: grid;
    grid-template-columns: 76px minmax(0, 1fr);
    gap: 10px 12px;
    align-items: center;
  }
  .lbl {
    color: var(--muted);
    align-self: start;
    padding-top: 4px;
  }
  .col {
    display: flex;
    flex-direction: column;
    gap: 5px;
    min-width: 0;
  }
  .row {
    display: flex;
    gap: 6px;
    align-items: center;
    min-width: 0;
  }
  .row.wrap {
    flex-wrap: wrap;
  }
  .row input[type="text"] {
    flex: 1;
    min-width: 0;
  }
  .row select {
    min-width: 0;
  }
  .save input {
    flex: 1;
  }
  .dim {
    width: 78px;
  }
  .hint {
    margin: 0;
    color: var(--muted);
    font-size: calc(12px * var(--text-scale));
  }
  .hint.warn {
    color: var(--accent);
  }
  .radio,
  .check {
    display: flex;
    gap: 6px;
    align-items: center;
  }
  .off {
    color: var(--faint);
  }
  .muted {
    color: var(--muted);
  }
  .buttons {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-top: 4px;
  }
  .grow {
    flex: 1;
  }
</style>
