<script lang="ts">
  // Plane Properties: a plane's size (width and height in pixels, or the timeline's with
  // Match Timeline), its pixels' shape, and its colour (a swatch that opens the colour
  // picker, an eyedropper, and transparent, white, grey and black at a click). Nothing
  // changes until OK, and then it's one undo step.
  import { run } from "../lib/session.svelte";
  import { api } from "../lib/api";
  import { planeDialog } from "../lib/planedialog.svelte";
  import { pickColor } from "../lib/colorpick.svelte";

  const ASPECTS: [string, number, number][] = [
    ["Square Pixels (1.0)", 1, 1],
    ["D1/DV NTSC (0.9091)", 10, 11],
    ["D1/DV NTSC Widescreen 16:9 (1.2121)", 40, 33],
    ["D1/DV PAL (1.0940)", 59, 54],
    ["D1/DV PAL Widescreen 16:9 (1.4587)", 118, 81],
    ["HDV 1080 / DVCPRO HD 720 (1.3333)", 4, 3],
    ["DVCPRO HD 1080 (1.5)", 3, 2],
    ["Anamorphic 2:1 (2.0)", 2, 1],
  ];

  let width = $state(1920);
  let height = $state(1080);
  let seq = $state<[number, number]>([1920, 1080]);
  let aspect = $state(0);
  let hex = $state("#000000");
  let alpha = $state(255);
  let loaded = $state(false);
  let el: HTMLDivElement | undefined = $state();

  const toHex = (c: number[]) => "#" + c.slice(0, 3).map((v) => Math.max(0, Math.min(255, v | 0)).toString(16).padStart(2, "0")).join("");

  $effect(() => {
    const id = planeDialog.id;
    if (id == null) {
      loaded = false;
      return;
    }
    api
      .planeProps(id)
      .then((p) => {
        if (!p) return close();
        width = p.width;
        height = p.height;
        seq = p.seq;
        aspect = 0;
        hex = toHex(p.color);
        alpha = p.color[3];
        loaded = true;
        queueMicrotask(() => el?.querySelector<HTMLInputElement>("input")?.select());
      })
      .catch(close);
  });

  function close() {
    planeDialog.id = null;
  }
  const size = (v: number) => Math.max(1, Math.min(32768, Math.round(+v || 0)));
  async function ok() {
    const id = planeDialog.id;
    if (id == null) return;
    const [, num, den] = ASPECTS[aspect] ?? ASPECTS[0];
    close();
    await run(api.setPlaneProps(id, hex, alpha, size(width), size(height), num, den));
  }
  function preset(c: string, a = 255) {
    hex = c;
    alpha = a;
  }
  const dropper = typeof window !== "undefined" && "EyeDropper" in window;
  async function eyedrop() {
    try {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const r = await new (window as any).EyeDropper().open();
      if (r?.sRGBHex) preset(String(r.sRGBHex).toLowerCase());
    } catch {
      /* cancelled */
    }
  }
  function keys(e: KeyboardEvent) {
    if (e.key === "Escape") (e.preventDefault(), e.stopPropagation(), close());
    else if (e.key === "Enter") (e.preventDefault(), e.stopPropagation(), ok());
  }
</script>

{#if planeDialog.id != null && loaded}
  <div class="backdrop" role="presentation" onpointerdown={(e) => e.target === e.currentTarget && close()}>
    <div class="dialog" bind:this={el} role="dialog" aria-label="Plane Properties" tabindex="-1" onkeydown={keys} data-automation-id="plane-properties">
      <header>
        <span>Plane Properties</span>
        <button class="x" aria-label="Close" title="Close" onclick={close}>✕</button>
      </header>
      <section>
        <div class="sub">Plane</div>
        <div class="grid">
          <label for="plane-w">Width:</label>
          <div class="pair">
            <input id="plane-w" type="number" min="1" max="32768" bind:value={width} data-automation-id="plane-width" />
            <button class="match" title="The timeline's own width and height" onclick={() => ((width = seq[0]), (height = seq[1]), (aspect = 0))} data-automation-id="plane-match">
              <svg viewBox="0 0 16 16" width="16" height="16" aria-hidden="true"><rect x="1.5" y="1.5" width="13" height="13" fill="none" stroke="currentColor" stroke-dasharray="2 2" /><rect x="6" y="6" width="8.5" height="8.5" fill="var(--playhead)" /></svg>
              Match Timeline
            </button>
          </div>
          <label for="plane-h">Height:</label>
          <div class="pair"><input id="plane-h" type="number" min="1" max="32768" bind:value={height} data-automation-id="plane-height" /></div>
          <label for="plane-par">Aspect Ratio:</label>
          <select id="plane-par" bind:value={aspect} data-automation-id="plane-aspect">
            {#each ASPECTS as [name], i (name)}<option value={i}>{name}</option>{/each}
          </select>
          <span class="lbl">Color:</span>
          <div class="colors">
            <button
              class="swatch"
              class:clear={alpha === 0}
              style="--c:{hex}"
              title="The plane's colour: click to pick another"
              aria-label="Plane colour"
              data-automation-id="plane-colour-swatch"
              onclick={(e) => pickColor(hex, (h) => preset(h), e.currentTarget, "Plane colour")}
            ></button>
            {#if dropper}
              <button class="tool" title="Eyedropper: pick a colour from anywhere on the screen" aria-label="Eyedropper" onclick={eyedrop}>
                <svg viewBox="0 0 16 16" width="16" height="16" aria-hidden="true"><path d="M11.5 1.5l3 3-2 2-.8-.8-6.2 6.2H3.5v-2l6.2-6.2-.8-.8z" fill="#e8657a" /><path d="M3.5 12.5l-2 2" stroke="#8c7cf0" stroke-width="2" stroke-linecap="round" /></svg>
              </button>
            {/if}
            <button class="tool" title="Transparent" aria-label="Transparent" onclick={() => preset(hex, 0)}><span class="chip checker"></span></button>
            <button class="tool" title="White" aria-label="White" onclick={() => preset("#ffffff")}><span class="chip" style="background:#fff"></span></button>
            <button class="tool" title="Grey" aria-label="Grey" onclick={() => preset("#808080")}><span class="chip" style="background:#808080"></span></button>
            <button class="tool" title="Black" aria-label="Black" onclick={() => preset("#000000")}><span class="chip" style="background:#000"></span></button>
          </div>
        </div>
      </section>
      <footer>
        <button onclick={close} data-automation-id="plane-cancel">Cancel</button>
        <button class="primary" onclick={ok} data-automation-id="plane-ok">OK</button>
      </footer>
    </div>
  </div>
{/if}

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    z-index: 980;
    display: grid;
    place-items: center;
    background: rgb(0 0 0 / 35%);
  }
  .dialog {
    width: 440px;
    background: var(--panel);
    border: 1px solid var(--line);
    border-radius: var(--radius);
    box-shadow: 0 12px 40px rgb(0 0 0 / 50%);
  }
  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 10px 12px 10px 16px;
    font-size: 14px;
  }
  .x {
    background: none;
    border: 0;
    font-size: 15px;
    padding: 2px 6px;
  }
  section {
    margin: 0 12px;
    border: 1px solid var(--line);
    border-radius: 3px;
    background: var(--base);
  }
  .sub {
    padding: 10px 14px;
    border-bottom: 1px solid var(--line);
    background: var(--panel);
  }
  .grid {
    display: grid;
    grid-template-columns: 110px 1fr;
    gap: 10px 10px;
    align-items: center;
    padding: 14px 14px 16px;
  }
  .grid label,
  .lbl {
    color: var(--muted);
    text-align: right;
  }
  .pair {
    display: flex;
    gap: 8px;
    align-items: center;
  }
  .pair input {
    width: 78px;
  }
  .match {
    display: inline-flex;
    align-items: center;
    gap: 6px;
  }
  select {
    width: 100%;
  }
  .colors {
    display: flex;
    gap: 6px;
    align-items: center;
  }
  .swatch {
    width: 76px;
    height: 26px;
    padding: 0;
    border: 1px solid var(--line);
    border-radius: 2px;
    background: var(--c);
    cursor: pointer;
  }
  .swatch.clear {
    background:
      linear-gradient(var(--c), var(--c)) 0 0 / 0 0 no-repeat,
      repeating-conic-gradient(#999 0 25%, #555 0 50%) 0 0 / 10px 10px;
  }
  .tool {
    width: 30px;
    height: 28px;
    padding: 0;
    display: grid;
    place-items: center;
  }
  .chip {
    width: 14px;
    height: 14px;
    display: block;
    border: 1px solid rgb(0 0 0 / 40%);
  }
  .checker {
    background: repeating-conic-gradient(#bbb 0 25%, #666 0 50%) 0 0 / 7px 7px;
  }
  footer {
    display: flex;
    justify-content: space-between;
    padding: 14px 12px 12px;
  }
  footer button {
    min-width: 72px;
  }
</style>
