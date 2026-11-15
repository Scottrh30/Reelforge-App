<script lang="ts">
  // The start screen (at launch, and File > Start screen…): a new project
  // from a preset, one of your templates or any size; or a recent project.
  import { onDestroy } from "svelte";
  import { app, run, say } from "../lib/session.svelte";
  import { checkUnsaved, question } from "../lib/actions.svelte";
  import { api, type StartView, type PresetView } from "../lib/api";

  let view = $state<StartView | null>(null);
  let group = $state("presets");
  let thumbs = $state<Record<string, string>>({});
  let custom = $state({ w: 1920, h: 1080, rate: "30/1", aspect: "1/1" });
  /// The pixel shape presets start with: "own" (each preset's), or "w/h".
  let aspect = $state("own");
  const pair = (s: string): [number, number] => s.split("/").map(Number) as [number, number];
  /// A preset's pixel shape, as the picker has it.
  const presetAspect = (p: PresetView): [number, number] => (aspect === "own" ? p.aspect : pair(aspect));
  const aspectName = (a: [number, number]) => view?.aspects.find(([, w, h]) => w === a[0] && h === a[1])?.[0] ?? `${a[0]}:${a[1]}`;
  const RATES: [string, string][] = [
    ["23.976", "24000/1001"],
    ["24", "24/1"],
    ["25", "25/1"],
    ["29.97", "30000/1001"],
    ["30", "30/1"],
    ["50", "50/1"],
    ["59.94", "60000/1001"],
    ["60", "60/1"],
    ["120", "120/1"],
  ];

  async function load() {
    view = await api.startView();
    for (const r of view.recent) {
      if (r.thumb && !thumbs[r.path]) {
        api.startThumb(r.path).then((buf) => {
          if (buf.byteLength) thumbs[r.path] = URL.createObjectURL(new Blob([buf], { type: "image/jpeg" }));
        });
      }
    }
  }
  $effect(() => {
    if (app.startOpen) load();
  });
  onDestroy(() => Object.values(thumbs).forEach(URL.revokeObjectURL));

  const groups = $derived(view ? [...new Set(view.presets.map((p) => p.group))] : []);
  /// A long folder: its end, which is the telling part.
  const tail = (f: string) => (f.length > 44 ? "…" + f.slice(-43) : f);

  /// Start something new, asking about unsaved changes first.
  async function begin(make: () => Promise<unknown>) {
    if (!(await checkUnsaved())) return;
    await run(make());
    app.startOpen = false;
  }
  const fromPreset = (p: PresetView, rate: [number, number] | null = null) => begin(() => api.startNewPreset(p.id, rate, aspect === "own" ? null : pair(aspect)));

  // Preset groups folded shut, remembered between sessions (all open to start with).
  const FOLD_KEY = "reelforge.start.folded";
  let folded = $state<Record<string, boolean>>(
    (() => {
      try {
        return JSON.parse(localStorage.getItem(FOLD_KEY) ?? "{}") ?? {};
      } catch {
        return {};
      }
    })(),
  );
  function toggle(g: string) {
    folded[g] = !folded[g];
    try {
      localStorage.setItem(FOLD_KEY, JSON.stringify(folded));
    } catch {
      // Not remembered, then.
    }
  }
  const fromTemplate = (path: string) => begin(() => api.startNewTemplate(path));
  function fromCustom() {
    const [num, den] = custom.rate.split("/").map(Number);
    begin(() => api.startNewCustom(Math.round(custom.w), Math.round(custom.h), num, den, pair(custom.aspect)));
  }
  async function openRecent(path: string, exists: boolean) {
    if (!exists) return;
    if (!(await checkUnsaved())) return;
    if (await run(api.startOpenRecent(path))) app.startOpen = false;
    else load();
  }
  async function openOther() {
    if (!(await checkUnsaved())) return;
    await run(api.openDialog());
    if ((await api.startView()).recent.some((r) => r.open)) app.startOpen = false;
  }
  async function openAdobe() {
    if (!(await checkUnsaved())) return;
    await run(api.openAdobeDialog());
    if ((await api.state()).import_report) app.startOpen = false;
  }
  async function forget(e: Event, path: string | null) {
    e.stopPropagation();
    if (path === null && (await question("Clear the recent list?", "Your projects stay where they are; only the list is cleared.", ["Clear", "Cancel"])) !== "Clear") return;
    await run(api.startForget(path));
    load();
  }
  async function removeTemplate(e: Event, path: string, name: string) {
    e.stopPropagation();
    if ((await question(`Delete the template “${name}”?`, "Projects already started from it aren't changed.", ["Delete", "Cancel"])) !== "Delete") return;
    await run(api.templateRemove(path));
    load();
  }

  /// A preset's shape, scaled into a 56 × 56 box.
  function shape(w: number, h: number): [number, number] {
    const k = 52 / Math.max(w, h);
    return [Math.max(6, w * k), Math.max(6, h * k)];
  }
  function ratio(w: number, h: number): string {
    const r = w / h;
    for (const [v, s] of [
      [16 / 9, "16:9"],
      [9 / 16, "9:16"],
      [4 / 5, "4:5"],
      [1, "1:1"],
      [2.39, "2.39:1"],
      [1.85, "1.85:1"],
      [256 / 135, "1.90:1"],
    ] as [number, string][])
      if (Math.abs(r - v) < 0.01) return s;
    return r >= 1 ? `${r.toFixed(2)}:1` : `1:${(1 / r).toFixed(2)}`;
  }
  function ago(at: number): string {
    const s = Date.now() / 1000 - at;
    if (s < 90) return "just now";
    if (s < 3600) return `${Math.round(s / 60)} minutes ago`;
    if (s < 86400 * 1.5) return `${Math.round(s / 3600)} hour${Math.round(s / 3600) === 1 ? "" : "s"} ago`;
    if (s < 86400 * 30) return `${Math.round(s / 86400)} days ago`;
    return new Date(at * 1000).toLocaleDateString();
  }

  function key(e: KeyboardEvent) {
    if (app.startOpen && e.key === "Escape") {
      e.stopPropagation();
      app.startOpen = false;
    }
  }
</script>

<svelte:window onkeydown={key} />

{#if app.startOpen && view}
  <div class="start" role="dialog" aria-modal="true" aria-labelledby="start-title">
    <div class="frame">
      <header>
        <div class="brand">
          <h1 id="start-title">Reelforge</h1>
          <span class="muted">Start something new, or pick up where you left off.</span>
        </div>
        <span class="grow"></span>
        <button onclick={openOther}>Open a project…</button>
        <button onclick={openAdobe} title="A Premiere Pro (.prproj) or After Effects (.aep) project, opened as a new Reelforge project">From Premiere or After Effects…</button>
        <button class="ghost close" onclick={() => (app.startOpen = false)} title={view.has_project ? "Back to the project that's open" : "Start with an empty 1080p project"} aria-label="Close the start screen">✕</button>
      </header>

      <div class="cols">
        <section class="new" aria-label="New project">
          <h2>New project</h2>
          <div class="chips" role="tablist">
            <button role="tab" class:on={group === "presets"} aria-selected={group === "presets"} onclick={() => (group = "presets")}>Presets</button>
            <button role="tab" class:on={group === "templates"} aria-selected={group === "templates"} onclick={() => (group = "templates")}>Your templates{#if view.templates.length}<span class="count">{view.templates.length}</span>{/if}</button>
            <button role="tab" class:on={group === "custom"} aria-selected={group === "custom"} onclick={() => (group = "custom")}>Any size</button>
            {#if group === "presets"}
              <span class="grow"></span>
              <label class="par" title="The shape of each pixel. Square for nearly everything made today; DV, HDV, DVCPro HD and anamorphic footage have wider or narrower pixels, shown stretched to their true shape.">
                Pixel aspect
                <select bind:value={aspect}>
                  <option value="own">Each preset's own</option>
                  {#each view.aspects as [name, w, h]}<option value="{w}/{h}">{name}</option>{/each}
                </select>
              </label>
            {/if}
          </div>

          {#if group === "templates"}
            {#if view.templates.length}
              <div class="grid">
                {#each view.templates as t (t.path)}
                  <div class="card tpl" role="button" tabindex="0" onclick={() => fromTemplate(t.path)} onkeydown={(e) => e.key === "Enter" && fromTemplate(t.path)} title="Start a new project from “{t.name}”">
                    <div class="shape"><span class="icon" aria-hidden="true">▤</span></div>
                    <div class="text">
                      <span class="name">{t.name}</span>
                      <span class="sum">{t.summary}</span>
                      <span class="detail">{t.sequences} sequence{t.sequences === 1 ? "" : "s"}{t.media ? ` · ${t.media} media file${t.media === 1 ? "" : "s"}` : ""}</span>
                    </div>
                    <button class="ghost x" onclick={(e) => removeTemplate(e, t.path, t.name)} title="Delete this template" aria-label="Delete the template {t.name}">✕</button>
                  </div>
                {/each}
              </div>
            {:else}
              <p class="empty">No templates yet. Set a project up the way you like to start (sequence size, tracks, titles, effects, music), then <b>File → Save as template…</b>, and it shows up here.</p>
            {/if}
          {:else if group === "custom"}
            <div class="custom">
              <label>Width <input type="number" min="16" max="16384" step="2" bind:value={custom.w} /></label>
              <button class="ghost swap" onclick={() => ([custom.w, custom.h] = [custom.h, custom.w])} title="Swap width and height" aria-label="Swap width and height">⇄</button>
              <label>Height <input type="number" min="16" max="16384" step="2" bind:value={custom.h} /></label>
              <label
                >Frame rate
                <select bind:value={custom.rate}>
                  {#each RATES as [l, v]}<option value={v}>{l} fps</option>{/each}
                </select>
              </label>
              <label
                >Pixel aspect
                <select bind:value={custom.aspect}>
                  {#each view.aspects as [name, w, h]}<option value="{w}/{h}">{name}</option>{/each}
                </select>
              </label>
              {#if custom.w >= 16 && custom.h >= 16}
                {@const [pw, ph] = pair(custom.aspect)}
                {@const [sw, sh] = shape((custom.w * pw) / ph, custom.h)}
                <div class="shape big" title="The picture's shape on screen"><div class="box" style="width:{sw}px;height:{sh}px"></div><span>{ratio((custom.w * pw) / ph, custom.h)}</span></div>
              {/if}
              <button class="primary" onclick={fromCustom}>Create</button>
            </div>
          {:else}
            <div class="scroll">
              {#each groups as g}
                {@const list = view.presets.filter((p) => p.group === g)}
                <button class="group" aria-expanded={!folded[g]} aria-controls="presets-{g}" onclick={() => toggle(g)} title={folded[g] ? `Show the ${g} presets` : `Hide the ${g} presets`}>
                  <span class="chev" class:shut={folded[g]} aria-hidden="true">▾</span>
                  <h3>{g}</h3>
                  <span class="count">{list.length}</span>
                </button>
                {#if !folded[g]}
                <div class="grid" id="presets-{g}">
                  {#each list as p (p.id)}
                    {@const pa = presetAspect(p)}
                    {@const dw = (p.width * pa[0]) / pa[1]}
                    {@const [sw, sh] = shape(dw, p.height)}
                    {@const changed = pa[0] !== p.aspect[0] || pa[1] !== p.aspect[1]}
                    <div class="card" role="button" tabindex="0" onclick={() => fromPreset(p)} onkeydown={(e) => e.target === e.currentTarget && e.key === "Enter" && fromPreset(p)} title="New {p.name} project ({p.summary}{changed ? `, with ${aspectName(pa)} pixels` : ""})">
                      <div class="shape"><div class="box" style="width:{sw}px;height:{sh}px"></div><span>{ratio(dw, p.height)}</span></div>
                      <div class="text">
                        <span class="name">{p.name}</span>
                        <span class="sum">{p.summary}{#if changed}<span class="parnote"> · {aspectName(pa)} pixels</span>{/if}</span>
                        <span class="detail">{p.detail}</span>
                        <span class="rates" role="group" aria-label="Frame rate (fps)" title="Frame rate: click one to start at it">
                          {#each p.rates as [n, d, label]}
                            {@const own = n === p.rate[0] && d === p.rate[1]}
                            <button class="rate" class:own onclick={(e) => (e.stopPropagation(), fromPreset(p, [n, d]))} title="New {p.name} project at {label} fps{own ? ' (its usual rate)' : ''}">{label}</button>
                          {/each}
                        </span>
                      </div>
                    </div>
                  {/each}
                </div>
                {/if}
              {/each}
            </div>
          {/if}
        </section>

        <aside class="recent" aria-label="Recent projects">
          <div class="rhead">
            <h2>Recent</h2>
            <span class="grow"></span>
            {#if view.recent.length}<button class="ghost small" onclick={(e) => forget(e, null)}>Clear list</button>{/if}
          </div>
          {#if view.recent.length}
            <ul>
              {#each view.recent as r (r.path)}
                <li class:missing={!r.exists} class:open={r.open}>
                  <div class="row" role="button" tabindex={r.exists ? 0 : -1} onclick={() => openRecent(r.path, r.exists)} onkeydown={(e) => e.key === "Enter" && openRecent(r.path, r.exists)} title={r.exists ? r.path : `${r.path} isn't there any more`}>
                    <div class="thumb">{#if thumbs[r.path]}<img src={thumbs[r.path]} alt="" />{:else}<span aria-hidden="true">▶</span>{/if}</div>
                    <div class="text">
                      <span class="name">{r.name}{#if r.open}<span class="badge">open</span>{/if}</span>
                      <span class="sum">{r.exists ? [r.summary, ago(r.at)].filter(Boolean).join(" · ") : "Not found: moved or deleted"}</span>
                      <span class="detail path">{tail(r.folder)}</span>
                    </div>
                    <button class="ghost x" onclick={(e) => forget(e, r.path)} title="Take it off the list (the file stays)" aria-label="Take {r.name} off the list">✕</button>
                  </div>
                </li>
              {/each}
            </ul>
          {:else}
            <p class="empty">Projects you open or save show up here.</p>
          {/if}
        </aside>
      </div>

      <footer>
        <label class="check"><input type="checkbox" checked={view.at_startup} onchange={(e) => run(api.startScreenSetting(e.currentTarget.checked))} /> Show this when Reelforge opens</label>
        <span class="grow"></span>
        <span class="muted small-text">File → Start screen brings it back · Esc closes</span>
      </footer>
    </div>
  </div>
{/if}

<style>
  .start {
    position: fixed;
    inset: 0;
    z-index: 60;
    background: radial-gradient(1200px 600px at 15% -10%, color-mix(in srgb, var(--accent) 9%, transparent), transparent 60%), var(--base);
    display: grid;
    place-items: center;
    padding: 20px;
    overflow: auto;
  }
  .frame {
    width: min(1180px, 100%);
    height: min(760px, 100%);
    display: flex;
    flex-direction: column;
    gap: 16px;
  }
  header {
    display: flex;
    align-items: center;
    gap: 10px;
  }
  .brand {
    display: flex;
    align-items: baseline;
    gap: 14px;
    flex-wrap: wrap;
  }
  h1 {
    margin: 0;
    font-size: calc(26px * var(--text-scale));
    font-weight: 600;
    letter-spacing: -0.01em;
  }
  h2 {
    margin: 0;
    font-size: calc(13px * var(--text-scale));
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--muted);
  }
  .cols {
    flex: 1;
    min-height: 0;
    display: grid;
    grid-template-columns: minmax(0, 1fr) minmax(300px, 380px);
    gap: 18px;
  }
  @media (max-width: 860px) {
    .cols {
      grid-template-columns: 1fr;
    }
  }
  section,
  aside {
    background: var(--panel);
    border: 1px solid var(--line-soft);
    border-radius: 10px;
    padding: 16px;
    display: flex;
    flex-direction: column;
    gap: 12px;
    min-height: 0;
  }
  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }
  .chips button {
    border-radius: 14px;
    min-height: 26px;
    padding: 0 12px;
    background: transparent;
  }
  .chips button.on {
    background: var(--accent-soft);
    border-color: var(--accent);
    color: var(--text);
  }
  .count {
    margin-left: 6px;
    color: var(--muted);
  }
  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(230px, 1fr));
    gap: 8px;
    align-content: start;
    padding: 2px;
  }
  .card {
    display: flex;
    gap: 12px;
    align-items: center;
    text-align: left;
    padding: 10px 12px;
    min-height: 80px;
    background: var(--raised);
    border: 1px solid var(--line-soft);
    border-radius: 8px;
    cursor: pointer;
    position: relative;
    color: var(--text);
  }
  .card:hover,
  .card:focus-visible {
    border-color: var(--accent);
    background: var(--hover);
  }
  .shape {
    flex: none;
    width: 60px;
    height: 60px;
    display: grid;
    place-items: center;
    position: relative;
  }
  .shape .box {
    border: 1.5px solid var(--accent);
    border-radius: 3px;
    background: color-mix(in srgb, var(--accent) 10%, transparent);
  }
  .shape span {
    position: absolute;
    font-size: calc(10px * var(--text-scale));
    color: var(--muted);
    font-variant-numeric: tabular-nums;
  }
  .shape .icon {
    position: static;
    font-size: calc(26px * var(--text-scale));
    color: var(--accent);
  }
  .shape.big {
    width: 72px;
    height: 72px;
  }
  .text {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }
  .name {
    font-weight: 600;
  }
  .sum {
    font-variant-numeric: tabular-nums;
    color: var(--text);
    opacity: 0.85;
    font-size: calc(12.5px * var(--text-scale));
  }
  .detail {
    color: var(--muted);
    font-size: calc(12px * var(--text-scale));
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .x {
    position: absolute;
    top: 4px;
    right: 4px;
    min-height: 20px;
    width: 20px;
    padding: 0;
    opacity: 0;
  }
  .card:hover .x,
  .row:hover .x,
  .x:focus-visible {
    opacity: 1;
  }
  .custom {
    display: flex;
    flex-wrap: wrap;
    align-items: end;
    gap: 12px;
  }
  .par {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 12px;
    color: var(--muted);
  }
  .parnote {
    color: var(--accent);
  }
  .custom label {
    display: flex;
    flex-direction: column;
    gap: 4px;
    color: var(--muted);
    font-size: calc(12px * var(--text-scale));
  }
  .custom input {
    width: 96px;
  }
  .swap {
    align-self: end;
  }
  .rhead {
    display: flex;
    align-items: center;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
    overflow: auto;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .row {
    display: flex;
    gap: 10px;
    align-items: center;
    padding: 6px;
    border-radius: 7px;
    cursor: pointer;
    position: relative;
  }
  .row:hover,
  .row:focus-visible {
    background: var(--hover);
  }
  .thumb {
    flex: none;
    width: 96px;
    height: 54px;
    border-radius: 5px;
    background: var(--well);
    display: grid;
    place-items: center;
    overflow: hidden;
    color: var(--faint);
  }
  .thumb img {
    width: 100%;
    height: 100%;
    object-fit: contain;
  }
  .missing .row {
    cursor: default;
  }
  .missing .thumb,
  .missing .name {
    opacity: 0.5;
  }
  .missing .sum {
    color: #f0a050;
  }
  .badge {
    margin-left: 6px;
    padding: 0 6px;
    border-radius: 8px;
    background: var(--accent-soft);
    color: var(--accent);
    font-size: calc(10.5px * var(--text-scale));
    font-weight: 500;
  }
  .path {
    color: var(--faint);
  }
  .scroll {
    overflow: auto;
    display: flex;
    flex-direction: column;
    gap: 8px;
    min-height: 0;
    padding-right: 4px;
  }
  .group {
    display: flex;
    align-items: center;
    gap: 6px;
    margin: 6px 0 0;
    padding: 2px 4px;
    min-height: 24px;
    background: none;
    border: 0;
    border-radius: 5px;
    color: var(--muted);
    cursor: pointer;
    text-align: left;
  }
  .group:hover,
  .group:focus-visible {
    background: var(--hover);
    color: var(--text);
  }
  .scroll .group:first-child {
    margin-top: 0;
  }
  .chev {
    display: inline-block;
    width: 12px;
    transition: transform 0.12s;
  }
  .chev.shut {
    transform: rotate(-90deg);
  }
  .group h3 {
    color: inherit;
  }
  .rates {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 3px;
    margin-top: 3px;
  }
  .rate {
    min-height: 18px;
    padding: 0 4px;
    border-radius: 4px;
    font-size: calc(10.5px * var(--text-scale));
    font-variant-numeric: tabular-nums;
    background: var(--panel);
    border: 1px solid var(--line-soft);
    color: var(--muted);
  }
  .rate:hover,
  .rate:focus-visible {
    border-color: var(--accent);
    color: var(--text);
  }
  .rate.own {
    color: var(--text);
    border-color: color-mix(in srgb, var(--accent) 55%, var(--line-soft));
  }
  h3 {
    margin: 6px 0 0;
    font-size: calc(12px * var(--text-scale));
    font-weight: 600;
    color: var(--muted);
  }
  .group h3 {
    margin: 0;
  }
  .empty {
    color: var(--muted);
    margin: 0;
    max-width: 60ch;
  }
  footer {
    display: flex;
    align-items: center;
    gap: 10px;
  }
  .check {
    display: flex;
    gap: 6px;
    align-items: center;
  }
  .grow {
    flex: 1;
  }
  .muted {
    color: var(--muted);
  }
  .small-text {
    font-size: calc(12px * var(--text-scale));
  }
  .small {
    min-height: 22px;
    padding: 0 8px;
    font-size: calc(12px * var(--text-scale));
  }
  .close {
    font-size: calc(16px * var(--text-scale));
  }
</style>
