<script lang="ts">
  // A title clip's text and styling: font, size and colour, alignment and
  // spacing, outline, shadow and a box behind it, plus placement presets.
  // Typing or dragging a value is one undo step.
  import { app, run } from "../lib/session.svelte";
  import { api, type Inspector, type TitleSpec } from "../lib/api";
  import NumberField from "./NumberField.svelte";

  // `kerningTool`: show the kerning tool (the Text panel does; Controls keeps it short).
  let { ins, kerningTool = true }: { ins: Inspector; kerningTool?: boolean } = $props();

  type Font = { path: string | null; index: number; name: string };
  type Rgba = [number, number, number, number];
  // A local copy while editing so fields don't jump back between updates.
  let draft = $state<TitleSpec | null>(null);
  $effect(() => {
    void ins.id;
    draft = null;
  });
  const t = $derived((draft ?? ins.title!) as TitleSpec & Record<string, any>);

  function set(f: (s: TitleSpec & Record<string, any>) => void) {
    const n = structuredClone($state.snapshot(t)) as TitleSpec & Record<string, any>;
    f(n);
    draft = n;
    api.setTitle(ins.id, n);
  }
  async function commit() {
    await run(api.commit());
    draft = null;
  }
  async function once(f: (s: TitleSpec & Record<string, any>) => void) {
    set(f);
    await commit();
  }

  // Fonts: the built-in one plus every installed face, searchable.
  let fonts = $state<Font[]>([]);
  let fontOpen = $state(false);
  let fontSearch = $state("");
  let searchEl = $state<HTMLInputElement | null>(null);
  async function openFonts() {
    fontOpen = !fontOpen;
    if (fontOpen) {
      fontSearch = "";
      if (!fonts.length) fonts = await api.fontFaces();
      // The scan may still be going: try again shortly.
      if (!fonts.length) setTimeout(async () => (fonts = await api.fontFaces()), 1500);
      queueMicrotask(() => searchEl?.focus());
    }
  }
  const builtin: Font = { path: null, index: 0, name: "Built-in (Ubuntu Light)" };
  const shownFonts = $derived.by(() => {
    const q = fontSearch.trim().toLowerCase();
    const all = [builtin, ...fonts];
    return (q ? all.filter((f) => f.name.toLowerCase().includes(q)) : all).slice(0, 300);
  });
  const fontName = $derived(((t.font as Font | undefined)?.path ? (t.font as Font).name : "") || "Built-in");
  async function pickFont(f: Font) {
    fontOpen = false;
    await once((s) => (s.font = f.path ? { ...f } : { path: null, index: 0, name: "" }));
  }

  const hex = (c: Rgba) => "#" + c.slice(0, 3).map((v) => v.toString(16).padStart(2, "0")).join("");
  const rgba = (h: string, a: number): Rgba => [parseInt(h.slice(1, 3), 16), parseInt(h.slice(3, 5), 16), parseInt(h.slice(5, 7), 16), a];
  // Extruded into solid 3D letters.
  const solid = $derived((t.extrude ?? 0) > 0);
  async function make3d() {
    const tr = ins.visual?.transform;
    if (!tr) return;
    await api.setTransform(ins.id, { ...tr, three_d: true });
    await run(api.commit());
  }

  // Text animation: kinds and the timing each starts with (as in TextAnim::preset).
  type AnimKind = "None" | "Typewriter" | "FadeLetters" | "FadeWords" | "SlideLines" | "Bounce";
  type Anim = { kind: AnimKind; stagger: number; length: number; delay: number; out: boolean };
  const ANIM_LABELS: Record<AnimKind, string> = { None: "None", Typewriter: "Typewriter", FadeLetters: "Fade in letters", FadeWords: "Fade in words", SlideLines: "Slide up lines", Bounce: "Bounce in letters" };
  const ANIM_PRESETS: Record<AnimKind, Anim> = {
    None: { kind: "None", stagger: 0.05, length: 0.4, delay: 0, out: false },
    Typewriter: { kind: "Typewriter", stagger: 0.06, length: 0, delay: 0, out: false },
    FadeLetters: { kind: "FadeLetters", stagger: 0.04, length: 0.4, delay: 0, out: false },
    FadeWords: { kind: "FadeWords", stagger: 0.25, length: 0.5, delay: 0, out: false },
    SlideLines: { kind: "SlideLines", stagger: 0.35, length: 0.6, delay: 0, out: false },
    Bounce: { kind: "Bounce", stagger: 0.06, length: 0.7, delay: 0, out: false },
  };
  const anim = $derived(({ ...ANIM_PRESETS.None, ...((t.anim as Partial<Anim>) ?? {}) }) as Anim);

  // Case: how the letters show, whatever was typed.
  const cases = [
    ["AsTyped", "Aa", "As typed"],
    ["Upper", "AB", "ALL CAPS"],
    ["Start", "Ab", "Start Caps: the first letter of each word"],
    ["Lower", "ab", "lowercase"],
  ];
  // The characters as shown (in the chosen case), each with its place in the text, by line (as TitleSpec::shown_lines).
  type Ch = { at: number; c: string };
  const shownLines = $derived.by(() => {
    const out: Ch[][] = [[]];
    const kase = (t.case as string) ?? "AsTyped";
    let start = true;
    [...(t.text ?? "")].forEach((ch, i) => {
      if (ch === "\n") {
        out.push([]);
        start = true;
        return;
      }
      if (ch === "\r") return;
      const up = kase === "Upper" || (kase === "Start" && start);
      const shown = kase === "AsTyped" ? ch : up ? ch.toUpperCase() : ch.toLowerCase();
      out[out.length - 1].push({ at: i, c: shown });
      start = /\s/.test(ch) || "-/(\"".includes(ch);
    });
    return out;
  });
  const kerning = $derived((t.kerning ?? {}) as Record<string, number>);
  // The kerning tool: the gap picked (the place of the character after it).
  let gap = $state<number | null>(null);
  $effect(() => {
    void ins.id;
    gap = null;
  });
  const gapOk = $derived(gap != null && shownLines.some((l) => l.some((c, i) => i > 0 && c.at === gap)));
  function setKern(at: number, v: number) {
    set((s) => {
      const k = { ...((s.kerning ?? {}) as Record<string, number>) };
      const r = Math.round(Math.max(-1000, Math.min(1000, v)));
      if (r) k[at] = r;
      else delete k[at];
      s.kerning = k;
    });
  }
  async function nudgeKern(d: number) {
    if (gap == null || !gapOk) return;
    setKern(gap, (kerning[gap] ?? 0) + d);
    await commit();
  }
  // Every gap, in order (for moving through them with the arrow keys).
  const gaps = $derived(shownLines.flatMap((l) => l.filter((c, i) => i > 0 && l[i - 1].at !== c.at).map((c) => c.at)));
  function kernKeys(e: KeyboardEvent) {
    if (e.key !== "ArrowLeft" && e.key !== "ArrowRight") return;
    e.preventDefault();
    e.stopPropagation();
    const dir = e.key === "ArrowLeft" ? -1 : 1;
    if (e.altKey) {
      // Alt+arrows: tighter or looser by 10 (100 with Shift), as in After Effects.
      nudgeKern(dir * (e.shiftKey ? 100 : 10));
      return;
    }
    const i = gap == null ? -1 : gaps.indexOf(gap);
    const n = i < 0 ? (dir > 0 ? 0 : gaps.length - 1) : Math.max(0, Math.min(gaps.length - 1, i + dir));
    if (gaps.length) gap = gaps[n];
  }
  /// Typing changes where characters are: kerning stays with the characters it was set between.
  function remapKerning(old: string, now: string, k: Record<string, number>): Record<string, number> {
    const a = [...old];
    const b = [...now];
    let pre = 0;
    while (pre < a.length && pre < b.length && a[pre] === b[pre]) pre++;
    let suf = 0;
    while (suf < a.length - pre && suf < b.length - pre && a[a.length - 1 - suf] === b[b.length - 1 - suf]) suf++;
    const out: Record<string, number> = {};
    for (const [key, v] of Object.entries(k)) {
      const i = Number(key);
      if (i < pre || (i === pre && pre === a.length - suf)) out[i] = v;
      else if (i >= a.length - suf) out[i + b.length - a.length] = v;
    }
    return out;
  }
  function setText(v: string) {
    set((s) => {
      if (s.kerning && Object.keys(s.kerning).length) s.kerning = remapKerning(s.text, v, s.kerning as Record<string, number>);
      s.text = v;
    });
  }

  const aligns = [
    ["Left", "Left", "M2 3h10M2 6h7M2 9h10M2 12h6"],
    ["Center", "Centre", "M2 3h12M4 6h8M2 9h12M5 12h6"],
    ["Right", "Right", "M4 3h10M7 6h7M4 9h10M8 12h6"],
  ];
</script>

<div class="group">
  <div class="phead"><h4>Title</h4><button class="ghost small" onclick={() => (app.savePreset = { clip: ins.id })} title="Save this title's style (font, size, colours, outline, shadow, background) as a preset for other titles">Save as preset…</button></div>
  {#if t.shape}
    <p class="note">A shape instead of text: <b>{(t.shape as { name: string }).name}</b> (from an SVG logo or a mask). Size is its height; font, alignment and spacing don't apply. 3D depth 0 makes it flat.</p>
  {:else}
    <textarea rows="3" value={t.text} aria-label="Title text" placeholder="Type the title…" oninput={(e) => setText(e.currentTarget.value)} onblur={commit}></textarea>
  {/if}
  <div class="grid">
    <span class="lbl">Font</span>
    <div class="fontpick">
      <button class="fontbtn" onclick={openFonts} aria-expanded={fontOpen} title="Choose a font">{fontName}<span class="caret">▾</span></button>
      {#if fontOpen}
        <div class="fontlist" role="listbox" aria-label="Fonts">
          <input bind:this={searchEl} type="search" bind:value={fontSearch} placeholder="Search fonts…" aria-label="Search fonts" onkeydown={(e) => e.key === "Escape" && (e.stopPropagation(), (fontOpen = false))} />
          <div class="items">
            {#each shownFonts as f}
              <button role="option" aria-selected={f.name === fontName} class:on={(f.path ?? null) === ((t.font as Font | undefined)?.path ?? null) && f.index === ((t.font as Font | undefined)?.index ?? 0)} onclick={() => pickFont(f)}>{f.name}</button>
            {:else}
              <p class="muted">{fonts.length ? "No font matches" : "Looking for installed fonts…"}</p>
            {/each}
          </div>
        </div>
      {/if}
    </div>
    {#if ins.title_missing_font}
      <span></span><span class="warn">Font file not found: using the built-in font</span>
    {/if}

    <span class="lbl">Size</span>
    <div class="row">
      <NumberField label={t.shape ? "Shape height" : "Text size"} value={t.size} min={4} max={2000} step={1} digits={0} suffix=" px" oninput={(v) => set((s) => (s.size = v))} oncommit={commit} />
      <input type="color" value={hex(t.color)} aria-label="Text colour" title="Text colour" oninput={(e) => set((s) => (s.color = rgba(e.currentTarget.value, s.color[3])))} onchange={commit} />
    </div>

    <span class="lbl">Align</span>
    <div class="seg" role="radiogroup" aria-label="Alignment">
      {#each aligns as [v, label, d]}
        <button role="radio" aria-checked={t.align === v} class:on={(t.align ?? "Center") === v} onclick={() => once((s) => (s.align = v))} title={label} aria-label={label}><svg viewBox="0 0 16 15" width="15" height="14"><path {d} /></svg></button>
      {/each}
    </div>

    {#if !t.shape}
      <span class="lbl">Case</span>
      <div class="seg" role="radiogroup" aria-label="Case">
        {#each cases as [v, glyph, label]}
          <button role="radio" aria-checked={(t.case ?? "AsTyped") === v} class:on={(t.case ?? "AsTyped") === v} onclick={() => once((s) => (s.case = v))} title={label} aria-label={label}><span class="glyph">{glyph}</span></button>
        {/each}
      </div>
    {/if}

    <span class="lbl" title="Stretch the letters across (100% = as the font draws them)">Horizontal scale</span>
    <NumberField label="Horizontal scale" value={Math.round((t.scale_x ?? 1) * 1000) / 10} min={5} max={1000} step={0.5} digits={1} suffix="%" oninput={(v) => set((s) => (s.scale_x = v / 100))} oncommit={commit} />
    <span class="lbl" title="Stretch the letters up (100% = as the font draws them)">Vertical scale</span>
    <NumberField label="Vertical scale" value={Math.round((t.scale_y ?? 1) * 1000) / 10} min={5} max={1000} step={0.5} digits={1} suffix="%" oninput={(v) => set((s) => (s.scale_y = v / 100))} oncommit={commit} />

    <span class="lbl">Line spacing</span>
    <NumberField label="Line spacing" value={t.line_spacing ?? 1} min={0.5} max={4} step={0.01} digits={2} suffix="×" oninput={(v) => set((s) => (s.line_spacing = v))} oncommit={commit} />
    <span class="lbl" title="Extra space between every pair of characters (tracking)">Character spacing</span>
    <NumberField label="Character spacing" value={t.letter_spacing ?? 0} min={-50} max={200} step={0.1} digits={1} suffix=" px" oninput={(v) => set((s) => (s.letter_spacing = v))} oncommit={commit} />

    {#if !t.shape && kerningTool}
      <span class="lbl top" title="The space between one pair of characters: pick the gap, then tighten or loosen it">Kerning</span>
      <div class="kern">
        <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
        <div class="strip" role="listbox" tabindex="0" aria-label="Gaps between characters: arrow keys move, Alt+arrows tighten or loosen" onkeydown={kernKeys}>
          {#each shownLines as line}
            <div class="kline">
              {#each line as c, i}
                {#if i > 0 && line[i - 1].at !== c.at}
                  <button
                    class="gap"
                    class:on={gap === c.at}
                    class:set={!!kerning[c.at]}
                    role="option"
                    aria-selected={gap === c.at}
                    tabindex="-1"
                    title={`Between “${line[i - 1].c}” and “${c.c}”${kerning[c.at] ? `: ${kerning[c.at] > 0 ? "+" : ""}${kerning[c.at]}` : ""}`}
                    onclick={() => (gap = c.at)}
                    style:margin-left={`${Math.max(-6, Math.min(12, (kerning[c.at] ?? 0) / 25))}px`}><span></span></button
                  >
                {/if}
                <span class="ch">{c.c === " " ? "\u00a0" : c.c}</span>
              {/each}
              {#if !line.length}<span class="ch empty">&nbsp;</span>{/if}
            </div>
          {/each}
        </div>
        <div class="row">
          <button class="small" disabled={!gapOk} onclick={(e) => nudgeKern(e.shiftKey ? -100 : -10)} title="Tighter (Alt+←; Shift for 100)" aria-label="Tighter">−</button>
          <div class="kval" class:off={!gapOk} inert={!gapOk}><NumberField label="Kerning of the gap picked, in thousandths of the font size" value={gap != null && gapOk ? (kerning[gap] ?? 0) : 0} min={-1000} max={1000} step={1} digits={0} oninput={(v) => gap != null && gapOk && setKern(gap, v)} oncommit={commit} /></div>
          <button class="small" disabled={!gapOk} onclick={(e) => nudgeKern(e.shiftKey ? 100 : 10)} title="Looser (Alt+→; Shift for 100)" aria-label="Looser">+</button>
        </div>
        <div class="row wrap">
          <button class="small" disabled={!gapOk || !kerning[gap ?? -1]} onclick={() => gap != null && once((s) => { const k = { ...((s.kerning ?? {}) as Record<string, number>) }; delete k[gap!]; s.kerning = k; })}>Reset pair</button>
          <button class="small" disabled={!Object.keys(kerning).length} onclick={() => once((s) => (s.kerning = {}))} title="Back to the font's own kerning everywhere">Reset all</button>
          <span class="hint">{gapOk ? "" : "Click a gap between two letters"}</span>
        </div>
      </div>
    {/if}

    <span class="lbl" title="Extrude the letters into solid 3D text, seen through the camera and lit by the lights">3D depth</span>
    <NumberField label="3D depth: how far the letters are extruded back (0 = flat text)" value={t.extrude ?? 0} min={0} max={5000} step={1} digits={0} suffix=" px" oninput={(v) => set((s) => (s.extrude = v))} oncommit={commit} />
    {#if solid}
      <span class="lbl sub"><label class="check" title="Give the sides their own colour (otherwise the text colour)"><input type="checkbox" checked={!!t.side_color} onchange={(e) => { const on = e.currentTarget.checked; once((s) => (s.side_color = on ? [...s.color] : null)); }} /> Sides</label></span>
      <div class="row" class:off={!t.side_color}>
        <input type="color" value={hex((t.side_color ?? t.color) as Rgba)} disabled={!t.side_color} aria-label="Side colour" title={t.side_color ? "Side colour" : "The sides are the text colour: tick Sides to choose another"} oninput={(e) => set((s) => (s.side_color = rgba(e.currentTarget.value, (s.side_color ?? s.color)[3])))} onchange={commit} />
      </div>
      <span class="lbl sub" title="Cut back the letters' edges, front and back, so they catch the light">Bevel</span>
      <div class="row">
        <NumberField label="Bevel: how far the letters' edges are cut back (0 = sharp edges; held to half the depth, and to 0.4 of the thinnest stroke)" value={t.bevel ?? 0} min={0} max={500} step={0.1} digits={1} suffix=" px" oninput={(v) => set((s) => (s.bevel = v))} oncommit={commit} />
        <select value={t.bevel_style ?? "Round"} disabled={!t.bevel} aria-label="Bevel shape" title="Round: smooth bulging edges. Flat: a straight 45° cut. Cove: hollowed in, like a moulding." onchange={(e) => once((s) => (s.bevel_style = e.currentTarget.value))}>
          <option value="Round">Round</option>
          <option value="Flat">Flat</option>
          <option value="Cove">Cove</option>
        </select>
      </div>
      <span></span>
      <p class="note">
        Lit by the lights in the sequence (Timeline → Add light), or a soft light from above left when there are none. Outline, shadow and box aren't drawn on 3D text.
        {#if !ins.visual?.transform.three_d}
          <button class="small" onclick={make3d} title="Show the 3D layer settings (Z position, X and Y rotation) in the Transform section">Turn it in 3D…</button>
        {/if}
      </p>
    {/if}

    <span class="lbl" class:off={solid}>Outline</span>
    <div class="row" class:off={solid}>
      <NumberField label="Outline width" value={t.outline ?? 0} min={0} max={100} step={0.1} digits={1} suffix=" px" oninput={(v) => set((s) => (s.outline = v))} oncommit={commit} />
      <input type="color" value={hex(t.outline_color ?? [0, 0, 0, 255])} aria-label="Outline colour" title="Outline colour" oninput={(e) => set((s) => (s.outline_color = rgba(e.currentTarget.value, (s.outline_color ?? [0, 0, 0, 255])[3])))} onchange={commit} />
    </div>

    <span class="lbl" class:off={solid}><label class="check"><input type="checkbox" checked={!!t.shadow} onchange={(e) => once((s) => (s.shadow = e.currentTarget.checked))} /> Shadow</label></span>
    <div class="row" class:off={!t.shadow || solid}>
      <input type="color" value={hex(t.shadow_color ?? [0, 0, 0, 128])} disabled={!t.shadow} aria-label="Shadow colour" title="Shadow colour" oninput={(e) => set((s) => (s.shadow_color = rgba(e.currentTarget.value, (s.shadow_color ?? [0, 0, 0, 128])[3])))} onchange={commit} />
      <NumberField label="Shadow opacity" value={Math.round(((t.shadow_color ?? [0, 0, 0, 128])[3] / 255) * 100)} min={0} max={100} step={1} digits={0} suffix="%" oninput={(v) => set((s) => (s.shadow_color = [...(s.shadow_color ?? [0, 0, 0, 128]).slice(0, 3), Math.round((v / 100) * 255)] as Rgba))} oncommit={commit} />
    </div>
    {#if t.shadow}
      <span class="lbl sub">Distance</span>
      <NumberField label="Shadow distance" value={t.shadow_distance ?? 0} min={0} max={500} step={0.2} digits={0} suffix=" px" oninput={(v) => set((s) => (s.shadow_distance = v))} oncommit={commit} />
      <span class="lbl sub">Angle</span>
      <NumberField label="Shadow angle (0° = right, 90° = down)" value={t.shadow_angle ?? 0} min={-360} max={360} step={1} digits={0} suffix="°" oninput={(v) => set((s) => (s.shadow_angle = v))} oncommit={commit} />
      <span class="lbl sub">Softness</span>
      <NumberField label="Shadow softness" value={t.shadow_softness ?? 0} min={0} max={200} step={0.2} digits={0} suffix=" px" oninput={(v) => set((s) => (s.shadow_softness = v))} oncommit={commit} />
    {/if}

    <span class="lbl" class:off={solid}><label class="check" title="A filled rectangle behind the text"><input type="checkbox" checked={!!t.background} onchange={(e) => once((s) => (s.background = e.currentTarget.checked))} /> Box</label></span>
    <div class="row" class:off={!t.background || solid}>
      <input type="color" value={hex(t.background_color ?? [0, 0, 0, 128])} disabled={!t.background} aria-label="Box colour" title="Box colour" oninput={(e) => set((s) => (s.background_color = rgba(e.currentTarget.value, (s.background_color ?? [0, 0, 0, 128])[3])))} onchange={commit} />
      <NumberField label="Box opacity" value={Math.round(((t.background_color ?? [0, 0, 0, 128])[3] / 255) * 100)} min={0} max={100} step={1} digits={0} suffix="%" oninput={(v) => set((s) => (s.background_color = [...(s.background_color ?? [0, 0, 0, 128]).slice(0, 3), Math.round((v / 100) * 255)] as Rgba))} oncommit={commit} />
    </div>
    {#if t.background}
      <span class="lbl sub">Padding</span>
      <NumberField label="Box padding" value={t.background_padding ?? 0} min={0} max={500} step={0.2} digits={0} suffix=" px" oninput={(v) => set((s) => (s.background_padding = v))} oncommit={commit} />
    {/if}

    <span class="lbl" class:off={solid} title="Bring the text in piece by piece: letters, words or lines one after another (not for 3D text)">Animate</span>
    <select value={anim.kind} disabled={solid} aria-label="Text animation" onchange={(e) => { const k = e.currentTarget.value as AnimKind; once((s) => (s.anim = { ...ANIM_PRESETS[k], out: !!(s.anim as any)?.out })); }}>
      {#each Object.entries(ANIM_LABELS) as [k, label]}<option value={k}>{label}</option>{/each}
    </select>
    {#if anim.kind !== "None" && !solid}
      <span class="lbl sub" title="Time between one piece starting and the next">Stagger</span>
      <NumberField label="Stagger" value={anim.stagger} min={0} max={5} step={0.002} digits={2} suffix=" s" oninput={(v) => set((s) => (s.anim = { ...anim, stagger: v }))} oncommit={commit} />
      {#if anim.kind !== "Typewriter"}
        <span class="lbl sub" title="How long each piece takes to come in">Each takes</span>
        <NumberField label="Each piece takes" value={anim.length} min={0.01} max={10} step={0.005} digits={2} suffix=" s" oninput={(v) => set((s) => (s.anim = { ...anim, length: v }))} oncommit={commit} />
      {/if}
      <span class="lbl sub" title="Seconds into the clip before the first piece starts">Delay</span>
      <NumberField label="Delay" value={anim.delay} min={0} max={60} step={0.005} digits={2} suffix=" s" oninput={(v) => set((s) => (s.anim = { ...anim, delay: v }))} oncommit={commit} />
      <span></span>
      <label class="check" title="At the end of the clip, the pieces go again in the same order, finishing as it ends"><input type="checkbox" checked={anim.out} onchange={(e) => once((s) => (s.anim = { ...anim, out: e.currentTarget.checked }))} /> Out at the end too</label>
    {/if}

    <span class="lbl">Place</span>
    <div class="row wrap">
      <button class="small" onclick={() => run(api.placeTitle(ins.id, "centre"))} title="In the middle of the frame">Centre</button>
      <button class="small" onclick={() => run(api.placeTitle(ins.id, "lower_third"))} title="Bottom left, inside the title-safe area">Lower third</button>
      <button class="small" onclick={() => run(api.placeTitle(ins.id, "top"))} title="Near the top, inside the title-safe area">Top</button>
    </div>
  </div>
</div>

<style>
  .group {
    padding: 8px 0 12px;
    border-bottom: 1px solid var(--line-soft);
  }
  .phead {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 6px;
  }
  .phead .small {
    min-height: 20px;
    padding: 0 6px;
    font-size: calc(11.5px * var(--text-scale));
  }
  h4 {
    font-size: calc(12.5px * var(--text-scale));
    font-weight: 600;
    color: var(--muted);
    margin: 0 0 8px;
  }
  textarea {
    width: 100%;
  }
  .grid {
    display: grid;
    grid-template-columns: 96px minmax(0, 1fr);
    gap: 6px;
    align-items: center;
    margin-top: 8px;
  }
  .lbl {
    color: var(--muted);
  }
  .sub {
    padding-left: 22px;
  }
  .row {
    display: flex;
    gap: 6px;
    align-items: center;
    min-width: 0;
  }
  .row > :global(*:not(input[type="color"])) {
    flex: 1;
    min-width: 0;
  }
  .wrap {
    flex-wrap: wrap;
  }
  .wrap > button {
    flex: none !important;
  }
  .off {
    opacity: 0.5;
  }
  .note {
    margin: 0;
    color: var(--muted);
    font-size: calc(11.5px * var(--text-scale));
    line-height: 1.4;
  }
  .note button {
    margin-top: 4px;
    display: block;
  }

  .check {
    display: flex;
    gap: 5px;
    align-items: center;
    color: var(--muted);
  }
  input[type="color"] {
    width: 40px;
    height: 24px;
    padding: 0;
    border: 1px solid var(--line);
    background: none;
    border-radius: var(--radius);
    flex: none;
  }
  .seg {
    display: flex;
    gap: 0;
  }
  .seg button {
    min-height: 24px;
    padding: 0 9px;
    border-radius: 0;
    margin-left: -1px;
  }
  .seg button:first-child {
    border-radius: var(--radius) 0 0 var(--radius);
    margin-left: 0;
  }
  .seg button:last-child {
    border-radius: 0 var(--radius) var(--radius) 0;
  }
  .seg button.on {
    background: var(--accent-soft);
    border-color: var(--accent);
    position: relative;
  }
  .seg .glyph {
    font-size: calc(12px * var(--text-scale));
    font-weight: 600;
    min-width: 16px;
    display: inline-block;
  }
  .top {
    align-self: start;
    padding-top: 4px;
  }
  .kern {
    display: flex;
    flex-direction: column;
    gap: 6px;
    min-width: 0;
  }
  .strip {
    background: var(--well);
    border: 1px solid var(--line);
    border-radius: var(--radius);
    padding: 6px 8px;
    max-height: 140px;
    overflow: auto;
    font-size: calc(17px * var(--text-scale));
    line-height: 1.5;
  }
  .strip:focus-visible {
    outline: 1px solid var(--accent);
  }
  .kline {
    display: flex;
    align-items: stretch;
    width: max-content;
  }
  .ch {
    white-space: pre;
  }
  .gap {
    width: 6px;
    min-height: 0;
    padding: 0;
    margin: 0 -3px;
    border: 0;
    background: none;
    display: flex;
    justify-content: center;
    cursor: col-resize;
    position: relative;
    z-index: 1;
  }
  .gap span {
    width: 1px;
    height: 100%;
    background: transparent;
  }
  .gap:hover span {
    background: var(--muted);
  }
  .gap.set span {
    background: color-mix(in srgb, var(--accent) 55%, transparent);
  }
  .gap.on span {
    width: 2px;
    background: var(--accent);
  }
  .hint {
    color: var(--muted);
    font-size: calc(11.5px * var(--text-scale));
    align-self: center;
  }
  .seg path {
    stroke: currentColor;
    stroke-width: 1.4;
    fill: none;
  }
  .fontpick {
    position: relative;
    min-width: 0;
  }
  .fontbtn {
    width: 100%;
    display: flex;
    justify-content: space-between;
    gap: 6px;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
    text-align: left;
  }
  .caret {
    color: var(--muted);
  }
  .fontlist {
    position: absolute;
    z-index: 20;
    top: calc(100% + 2px);
    right: 0;
    width: max(100%, 240px);
    background: var(--panel);
    border: 1px solid var(--line);
    border-radius: 6px;
    box-shadow: 0 10px 30px rgba(0, 0, 0, 0.5);
    padding: 6px;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .items {
    max-height: 280px;
    overflow: auto;
    display: flex;
    flex-direction: column;
  }
  .items button {
    background: none;
    border: 0;
    text-align: left;
    padding: 3px 6px;
    min-height: 0;
    border-radius: 4px;
  }
  .items button:hover {
    background: var(--well);
  }
  .items button.on {
    color: var(--accent);
  }
  .small {
    min-height: 22px;
    padding: 0 8px;
    font-size: calc(12px * var(--text-scale));
  }
  .warn {
    color: #f0a050;
    font-size: calc(12px * var(--text-scale));
  }
  .muted {
    color: var(--muted);
    margin: 4px 6px;
  }
</style>
