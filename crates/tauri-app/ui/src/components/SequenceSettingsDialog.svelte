<script lang="ts">
  // Timeline > Sequence settings…: the open sequence's (or composition's)
  // name, frame size, frame rate and pixel aspect, changed as one undo step.
  import { app, run } from "../lib/session.svelte";
  import { api } from "../lib/api";

  const RATES: [string, string][] = [
    ["23.976", "24000/1001"],
    ["24", "24/1"],
    ["25", "25/1"],
    ["29.97", "30000/1001"],
    ["30", "30/1"],
    ["48", "48/1"],
    ["50", "50/1"],
    ["59.94", "60000/1001"],
    ["60", "60/1"],
    ["100", "100/1"],
    ["120", "120/1"],
  ];

  // Common formats (frame size and rate together): picking one fills in both; any other
  // size or rate is Custom.
  const r = (fps: string) => RATES.find(([l]) => l === fps)?.[1] ?? "30/1";
  const fmt = (name: string, w: number, h: number, rates: string[]) => rates.map((fps) => ({ label: `${name} @ ${fps} fps`, w, h, rate: r(fps) }));
  const FORMATS = [
    ...fmt("720p HD", 1280, 720, ["23.976", "25", "29.97", "30"]),
    ...fmt("Instagram Square", 1080, 1080, ["30"]),
    ...fmt("1080p Full HD", 1920, 1080, ["23.976", "24", "25", "29.97", "30", "50", "59.94", "60"]),
    ...fmt("Vertical 1080p", 1080, 1920, ["25", "30"]),
    ...fmt("2K DCI", 2048, 1080, ["24", "25", "29.97", "30"]),
    ...fmt("2K 16:9 QWXGA", 2048, 1152, ["24", "25", "29.97", "30"]),
    ...fmt("1440p GoPro", 1920, 1440, ["24", "25", "30", "48", "60"]),
    ...fmt("2.5K BMCC", 2432, 1366, ["23.976", "24", "25", "29.97", "30"]),
    ...fmt("1440p QHD", 2560, 1440, ["24", "25", "30", "48", "60"]),
    ...fmt("2.7K GoPro", 2704, 1520, ["24", "25", "30", "48", "60"]),
    ...fmt("4K UHD", 3840, 2160, ["23.976", "24", "25", "29.97", "30", "50", "59.94", "60", "100"]),
    ...fmt("4K DCI 2160p", 4096, 2160, ["24", "25", "29.97", "30"]),
    ...fmt("5K RED EPIC", 5120, 2700, ["24", "25", "30", "48", "60", "100"]),
    ...fmt("6K RED DRAGON", 6144, 3160, ["24", "25", "30", "48", "60", "100"]),
    ...fmt("8K UHD", 7680, 4320, ["23.976", "24", "25", "29.97", "30", "50", "59.94", "60", "100"]),
  ];

  let aspects = $state<[string, number, number][]>([]);
  let f = $state({ name: "", w: 1920, h: 1080, rate: "30/1", aspect: "1/1" });
  // The frame size's shape kept while typing one side.
  let linked = $state(false);
  // What happens to the layers when the frame changes size (remembered).
  let fit = $state<"Keep" | "Fit" | "Fill">(
    (() => {
      try {
        const v = localStorage.getItem("reelforge.seqFit");
        return v === "Keep" || v === "Fill" ? v : "Fit";
      } catch {
        return "Fit";
      }
    })(),
  );
  $effect(() => {
    const v = fit;
    try {
      localStorage.setItem("reelforge.seqFit", v);
    } catch {
      // Not remembered, then.
    }
  });
  // The rate the sequence has, when it isn't in the list.
  let ownRate = $state<[string, string] | null>(null);

  const comp = $derived((app.s?.active ?? 0) > 0);

  $effect(() => {
    if (!app.seqSettingsOpen) return;
    const s = app.s?.seq;
    if (!s) return;
    const rate = `${s.rate_num}/${s.rate_den}`;
    ownRate = RATES.some(([, v]) => v === rate) ? null : [(s.rate_num / s.rate_den).toFixed(3).replace(/\.?0+$/, ""), rate];
    f = { name: s.name, w: s.width, h: s.height, rate, aspect: `${s.pixel_aspect[0]}/${s.pixel_aspect[1]}` };
    linked = false;
    if (!aspects.length) api.pixelAspects().then((a) => (aspects = a));
  });

  const pair = (s: string) => s.split("/").map(Number) as [number, number];
  const even = (v: number) => (Math.min(16384, Math.max(16, Math.round(v || 0))) + 1) & ~1;
  // The format the settings match (square pixels), else "-1" (Custom).
  const format = $derived(f.aspect === "1/1" ? String(FORMATS.findIndex((x) => x.w === even(f.w) && x.h === even(f.h) && x.rate === f.rate)) : "-1");
  function pickFormat(v: string) {
    const x = FORMATS[+v];
    if (!x) return;
    f.w = x.w;
    f.h = x.h;
    f.rate = x.rate;
    f.aspect = "1/1";
  }
  const par = $derived.by(() => {
    const [a, b] = pair(f.aspect);
    return a / b;
  });
  // What the picture looks like on screen.
  const shown = $derived({ w: Math.round(even(f.w) * par), h: even(f.h) });
  function ratio(w: number, h: number): string {
    const r = w / h;
    for (const [v, s] of [
      [16 / 9, "16:9"],
      [4 / 3, "4:3"],
      [9 / 16, "9:16"],
      [4 / 5, "4:5"],
      [1, "1:1"],
      [2.39, "2.39:1"],
      [1.85, "1.85:1"],
      [256 / 135, "1.90:1"],
    ] as [number, string][])
      if (Math.abs(r - v) < 0.012) return s;
    return r >= 1 ? `${r.toFixed(2)}:1` : `1:${(1 / r).toFixed(2)}`;
  }
  const box = $derived.by(() => {
    const k = 64 / Math.max(shown.w, shown.h);
    return { w: Math.max(6, shown.w * k), h: Math.max(6, shown.h * k) };
  });

  function setW(v: number) {
    if (linked && f.w > 0) f.h = Math.round((v * f.h) / f.w);
    f.w = v;
  }
  function setH(v: number) {
    if (linked && f.h > 0) f.w = Math.round((v * f.w) / f.h);
    f.h = v;
  }
  const s = $derived(app.s?.seq);
  const changed = $derived(
    !!s && (f.name.trim() !== s.name || even(f.w) !== s.width || even(f.h) !== s.height || f.rate !== `${s.rate_num}/${s.rate_den}` || f.aspect !== `${s.pixel_aspect[0]}/${s.pixel_aspect[1]}`),
  );
  const sizeChanged = $derived(!!s && (even(f.w) !== s.width || even(f.h) !== s.height));
  // The frame's shape on screen changes (size or pixel shape): the layers can follow.
  const resized = $derived(sizeChanged || (!!s && f.aspect !== `${s.pixel_aspect[0]}/${s.pixel_aspect[1]}`));
  const close = () => (app.seqSettingsOpen = false);
  async function ok() {
    if (changed) {
      const [n, d] = pair(f.rate);
      await run(api.setSequenceSettings(f.name, even(f.w), even(f.h), n, d, pair(f.aspect), resized ? fit : "Keep"));
    }
    close();
  }
  const num = (e: Event) => Number((e.currentTarget as HTMLInputElement).value);
</script>

<svelte:window onkeydown={(e) => app.seqSettingsOpen && e.key === "Escape" && close()} />

{#if app.seqSettingsOpen && s}
  <div class="scrim">
    <div class="dialog" role="dialog" aria-modal="true" aria-labelledby="seqset-title">
    <form
      class="inner"
      onsubmit={(e) => {
        e.preventDefault();
        ok();
      }}
    >
      <h2 id="seqset-title">{comp ? "Composition" : "Sequence"} settings</h2>
      <div class="body">
        <div class="grid">
          <label for="seqset-name">Name</label>
          <input id="seqset-name" type="text" bind:value={f.name} />

          <label for="seqset-format">Format</label>
          <select id="seqset-format" value={format} onchange={(e) => pickFormat(e.currentTarget.value)} title="A common frame size and rate together; change either below for anything else">
            {#each FORMATS as x, i (x.label)}<option value={String(i)}>{x.label}</option>{/each}
            <option value="-1">Custom</option>
          </select>

          <label for="seqset-w">Frame size</label>
          <div class="row">
            <input id="seqset-w" type="number" min="16" max="16384" step="2" value={f.w} oninput={(e) => setW(num(e))} aria-label="Width" />
            <button type="button" class="ghost link" class:on={linked} aria-pressed={linked} onclick={() => (linked = !linked)} title={linked ? "Width and height change together (click to unlink)" : "Keep the shape while typing one side"}>{linked ? "🔗" : "×"}</button>
            <input type="number" min="16" max="16384" step="2" value={f.h} oninput={(e) => setH(num(e))} aria-label="Height" />
            <span class="unit">pixels</span>
          </div>

          <label for="seqset-rate">Frame rate</label>
          <select id="seqset-rate" bind:value={f.rate}>
            {#if ownRate}<option value={ownRate[1]}>{ownRate[0]} fps</option>{/if}
            {#each RATES as [l, v]}<option value={v}>{l} fps</option>{/each}
          </select>

          <label for="seqset-par">Pixel aspect</label>
          <select id="seqset-par" bind:value={f.aspect} title="The shape of each pixel: square for nearly everything made today; DV, HDV, DVCPro HD and anamorphic footage have wider or narrower pixels">
            {#each aspects as [name, w, h]}<option value="{w}/{h}">{name}</option>{/each}
            {#if !aspects.some(([, w, h]) => `${w}/${h}` === f.aspect)}<option value={f.aspect}>{f.aspect.replace("/", ":")}</option>{/if}
          </select>
        </div>
        <div class="preview" title="The picture's shape on screen">
          <div class="frame" style="width:{box.w}px;height:{box.h}px"></div>
          <span>{ratio(shown.w, shown.h)}</span>
          {#if par !== 1}<span class="muted">shows as {shown.w} × {shown.h}</span>{/if}
        </div>
      </div>
      {#if resized}
        <div class="fit" role="radiogroup" aria-label="Layers">
          <span>Layers</span>
          <label title="Every layer scaled with the frame (and its distance from the middle), so all of the old frame still shows; effect sizes in pixels (blur, glow, outlines) keep their proportions"><input type="radio" name="seqset-fit" value="Fit" bind:group={fit} /> Scale to fit</label>
          <label title="Scaled so the new frame is filled; edges may go off it"><input type="radio" name="seqset-fit" value="Fill" bind:group={fit} /> Scale to fill</label>
          <label title="Layers keep their size, centred: a bigger frame shows more around them"><input type="radio" name="seqset-fit" value="Keep" bind:group={fit} /> Keep their size</label>
        </div>
      {/if}
      <p class="muted">
        {#if resized && fit !== "Keep"}Each layer's size and place (and their keyframes) change with the frame; solids and adjustment layers keep filling it; effects keep their settings in pixels.{:else if sizeChanged}Layers keep their size and stay where they are from the middle of the frame, so a bigger frame shows more around them.{/if}
        Edits keep their times; a new frame rate changes where frames fall, not when things happen. Undo puts it all back.
      </p>
      <div class="buttons">
        <span class="grow"></span>
        <button type="button" onclick={close}>Cancel</button>
        <button type="submit" class="primary" disabled={!f.name.trim()}>OK</button>
      </div>
    </form>
    </div>
  </div>
{/if}

<style>
  .scrim {
    position: fixed;
    inset: 0;
    background: rgba(10, 10, 12, 0.55);
    display: grid;
    place-items: center;
    z-index: 40;
  }
  .dialog {
    width: min(520px, calc(100vw - 32px));
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
    margin: 0;
  }
  .inner {
    display: flex;
    flex-direction: column;
    gap: 12px;
    margin: 0;
  }
  h2 {
    font-size: calc(16px * var(--text-scale));
    font-weight: 600;
    margin: 0;
  }
  p {
    margin: 0;
    font-size: calc(12px * var(--text-scale));
  }
  .muted {
    color: var(--muted);
  }
  .body {
    display: flex;
    gap: 16px;
    align-items: flex-start;
    flex-wrap: wrap;
  }
  .grid {
    flex: 1;
    min-width: 260px;
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 8px 12px;
    align-items: center;
    font-size: calc(12.5px * var(--text-scale));
  }
  .row {
    display: flex;
    align-items: center;
    gap: 4px;
    min-width: 0;
  }
  .row input {
    width: 76px;
    min-width: 0;
  }
  .link {
    width: 26px;
    padding: 0;
    color: var(--muted);
  }
  .link.on {
    color: var(--accent);
  }
  .unit {
    color: var(--muted);
    margin-left: 4px;
  }
  .preview {
    width: 110px;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 4px;
    font-size: calc(12px * var(--text-scale));
  }
  .frame {
    border: 1.5px solid var(--accent);
    border-radius: 2px;
    background: var(--accent-soft);
    margin: 4px 0;
  }
  .fit {
    display: flex;
    flex-wrap: wrap;
    gap: 6px 14px;
    align-items: center;
    font-size: calc(12.5px * var(--text-scale));
  }
  .fit label {
    display: flex;
    gap: 5px;
    align-items: center;
  }
  .grow {
    flex: 1;
  }
  .preview span {
    text-align: center;
  }
  .buttons {
    display: flex;
    gap: 8px;
    align-items: center;
  }
</style>
