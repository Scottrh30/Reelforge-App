<script lang="ts">
  // Help → About & diagnostics: the diagnostics report (to copy into a
  // message), the decoder check, and the third-party licence notices.
  import { app, run, say } from "../lib/session.svelte";
  import { api, type DecoderCheck, type ZeroCopyReport, type BenchReport } from "../lib/api";

  let report = $state("");
  let notices = $state("");
  let find = $state("");
  let checking = $state(false);
  let results = $state<DecoderCheck[] | null>(null);
  let started = 0;
  let elapsed = $state(0);
  let pre = $state<HTMLPreElement | null>(null);

  // Load what the open tab shows.
  $effect(() => {
    const tab = app.aboutTab;
    if (tab === "diagnostics") get(api.diagnostics()).then((t) => (report = t ?? ""));
    if (tab === "notices" && !notices) get(api.notices()).then((t) => (notices = t ?? ""));
  });

  async function copy() {
    try {
      await navigator.clipboard.writeText(report);
      say("Diagnostics copied: paste them into your message");
    } catch {
      say("Couldn't reach the clipboard: select the text and copy it");
    }
  }

  async function check() {
    checking = true;
    results = null;
    started = performance.now();
    const tick = setInterval(() => (elapsed = (performance.now() - started) / 1000), 250);
    results = await get(api.checkDecoders());
    clearInterval(tick);
    checking = false;
    // The report mentions the check: refresh it for the Diagnostics tab.
    report = (await get(api.diagnostics())) ?? report;
  }

  // Zero-copy: decode on the graphics card and compare with the usual copy.
  let zc = $state<ZeroCopyReport | null>(null);
  let zcBusy = $state(false);
  async function checkZc() {
    zcBusy = true;
    zc = await get(api.checkZeroCopy());
    zcBusy = false;
    report = (await get(api.diagnostics())) ?? report;
  }

  // Performance: time the heavy work on this graphics card.
  let perf = $state<BenchReport | null>(null);
  let perfBusy = $state(false);
  let perfStarted = 0;
  let perfElapsed = $state(0);
  async function checkPerf() {
    perfBusy = true;
    perf = null;
    perfStarted = performance.now();
    const tick = setInterval(() => (perfElapsed = (performance.now() - perfStarted) / 1000), 250);
    perf = await get(api.checkPerformance());
    clearInterval(tick);
    perfBusy = false;
    report = (await get(api.diagnostics())) ?? report;
  }

  const bad = $derived((results ?? []).filter((r) => r.status === "mismatch" || r.status === "error"));
  const verdict = $derived(
    !results
      ? ""
      : bad.length
        ? `${bad.length} format${bad.length === 1 ? "" : "s"} didn't match FFmpeg. Copy the diagnostics (they include these results) and send them over.`
        : `Every format matched FFmpeg${results.some((r) => r.status === "skipped") ? " (some were skipped: this FFmpeg can't make samples of them)" : ""}.`,
  );

  // Notices: jump to the next place the search text appears.
  let from = 0;
  function next() {
    if (!find.trim() || !pre) return;
    const text = notices.toLowerCase();
    const q = find.trim().toLowerCase();
    let at = text.indexOf(q, from);
    if (at < 0) at = text.indexOf(q);
    if (at < 0) {
      say(`"${find}" isn't in the notices`);
      return;
    }
    from = at + q.length;
    // Scroll to the line it's on.
    const line = notices.slice(0, at).split("\n").length - 1;
    const lh = parseFloat(getComputedStyle(pre).lineHeight) || 15;
    pre.scrollTop = Math.max(0, line * lh - pre.clientHeight / 3);
  }

  const close = () => (app.aboutTab = null);

  /// A read-only call: no need to refresh the editor's state afterwards.
  async function get<T>(p: Promise<T>): Promise<T | null> {
    try {
      return await p;
    } catch (e) {
      say(String(e));
      return null;
    }
  }
</script>

<svelte:window onkeydown={(e) => app.aboutTab && e.key === "Escape" && close()} />

{#if app.aboutTab}
  <div class="scrim">
    <div class="dialog" role="dialog" aria-modal="true" aria-labelledby="about-title">
      <div class="top">
        <h2 id="about-title">Reelforge</h2>
        <span class="copyright" title="Licensed under the MIT or Apache 2.0 licence, at your option (LICENSE-MIT, LICENSE-APACHE)">© 2026 Reelforge - SRH · MIT or Apache-2.0</span>
        <nav class="tabs" aria-label="About">
          <button class:on={app.aboutTab === "diagnostics"} onclick={() => (app.aboutTab = "diagnostics")}>Diagnostics</button>
          <button class:on={app.aboutTab === "check"} onclick={() => (app.aboutTab = "check")}>Check decoders</button>
          <button class:on={app.aboutTab === "notices"} onclick={() => (app.aboutTab = "notices")}>Third-party notices</button>
        </nav>
      </div>

      {#if app.aboutTab === "diagnostics"}
        <p class="muted">If something goes wrong, copy this and paste it into your message. It says what PC, graphics card and FFmpeg Reelforge is running with, what kind of video is in the project, and the most recent log lines. It doesn't include file names or your media.</p>
        <pre class="text">{report || "Gathering…"}</pre>
        <div class="buttons">
          <button onclick={() => run(api.openLogFolder())} title="The log files: this session's and the one before">Open log folder</button>
          <span class="grow"></span>
          <button onclick={close}>Close</button>
          <button class="primary" disabled={!report} onclick={copy}>Copy to clipboard</button>
        </div>
      {:else if app.aboutTab === "check"}
        <p class="muted">Makes a short sample in each format Reelforge decodes itself, decodes it with Reelforge's decoder and with FFmpeg's, and compares every frame. On processors with AVX2 it checks both the AVX2 and the portable code. It takes 10–30 seconds; you can keep working meanwhile.</p>
        {#if results}
          <table>
            <thead><tr><th>Format</th><th>Result</th><th>Details</th></tr></thead>
            <tbody>
              {#each results as r}
                <tr>
                  <td>{r.format}</td>
                  <td><span class="badge {r.status}">{r.status === "ok" ? "Matches" : r.status === "skipped" ? "Skipped" : r.status === "mismatch" ? "Differs" : "Error"}</span></td>
                  <td class="detail">{r.detail}</td>
                </tr>
              {/each}
            </tbody>
          </table>
          <p class:warn={bad.length > 0}>{verdict}</p>
        {:else if checking}
          <p>Checking… {elapsed.toFixed(0)} s</p>
        {/if}
        <div class="buttons">
          <span class="grow"></span>
          <button onclick={close}>Close</button>
          <button class="primary" disabled={checking} onclick={check}>{results ? "Check again" : "Check decoders"}</button>
        </div>
        <h3>Zero-copy video</h3>
        <p class="muted">Decodes short H.264 (8-bit) and HEVC (10-bit) samples with the hardware decoder, keeps the frames on the graphics card, and draws each one both ways: straight from the graphics card, and copied back to memory as usual. They should look the same. A few seconds; the results go into the diagnostics report.</p>
        {#if zc}
          <p class:warn={zc.verdict === "mismatch"}><span class="badge {zc.verdict === 'works' ? 'ok' : zc.verdict === 'mismatch' ? 'mismatch' : 'skipped'}">{zc.verdict === "works" ? "Works" : zc.verdict === "mismatch" ? "Differs" : zc.verdict === "off" ? "Off" : "Not available"}</span> {zc.summary}</p>
          {#if zc.formats.length}
            <table>
              <thead><tr><th>Format</th><th>Result</th><th>Details</th></tr></thead>
              <tbody>
                {#each zc.formats as f}
                  <tr>
                    <td>{f.format}</td>
                    <td><span class="badge {f.status === 'ok' ? 'ok' : f.status === 'mismatch' || f.status === 'error' ? 'mismatch' : 'skipped'}">{f.status === "ok" ? "Matches" : f.status === "mismatch" ? "Differs" : f.status === "error" ? "Error" : f.status === "not used" ? "Not used" : "Skipped"}</span></td>
                    <td class="detail">{f.detail}</td>
                  </tr>
                {/each}
              </tbody>
            </table>
          {/if}
        {:else if zcBusy}
          <p>Checking…</p>
        {/if}
        <div class="buttons">
          <span class="grow"></span>
          <button class="primary" disabled={zcBusy} onclick={checkZc}>{zc ? "Check zero-copy again" : "Check zero-copy"}</button>
        </div>
        <h3>Performance</h3>
        <p class="muted">Times the heavy work on this graphics card at 1080p: optical flow at each quality, the fluid simulation at each detail, tens of thousands of particles, and four layers with effects. “Real time” fits in a frame at 30 fps. Usually a few seconds (minutes without a graphics card); the results go into the diagnostics report.</p>
        {#if perf}
          <p class="detail">{perf.adapter}</p>
          <table>
            <thead><tr><th>Work</th><th>Time</th><th>Speed</th></tr></thead>
            <tbody>
              {#each perf.lines as l}
                <tr>
                  <td>{l.what}<div class="detail">{l.note}</div></td>
                  <td class="num">{l.ms < 10 ? l.ms.toFixed(1) : Math.round(l.ms)} ms</td>
                  <td><span class="badge {l.verdict === 'realtime' ? 'ok' : l.verdict === 'heavy' ? 'mismatch' : 'skipped'}">{l.verdict === "realtime" ? "Real time" : l.verdict === "slow" ? "Slower" : "Heavy"}</span></td>
                </tr>
              {/each}
            </tbody>
          </table>
        {:else if perfBusy}
          <p>Measuring… {perfElapsed.toFixed(0)} s</p>
        {/if}
        <div class="buttons">
          <span class="grow"></span>
          <button class="primary" disabled={perfBusy} onclick={checkPerf}>{perf ? "Check performance again" : "Check performance"}</button>
        </div>
      {:else}
        <p class="muted">Reelforge is built from the open-source software listed here; each part keeps its own licence. The same text is in THIRD-PARTY-NOTICES.txt beside the program.</p>
        <div class="row">
          <input type="search" placeholder="Find a library or licence…" bind:value={find} onkeydown={(e) => e.key === "Enter" && next()} oninput={() => (from = 0)} aria-label="Find in the notices" />
          <button onclick={next} disabled={!find.trim()}>Find next</button>
        </div>
        <pre class="text tall" bind:this={pre}>{notices || "Loading…"}</pre>
        <div class="buttons">
          <span class="grow"></span>
          <button class="primary" onclick={close}>Close</button>
        </div>
      {/if}
    </div>
  </div>
{/if}

<style>
  .copyright {
    color: var(--muted);
    font-size: calc(11.5px * var(--text-scale));
    white-space: nowrap;
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
    width: min(760px, calc(100vw - 32px));
    max-height: calc(100vh - 32px);
    background: var(--panel);
    border: 1px solid var(--line);
    border-radius: 8px;
    padding: 18px 20px;
    box-shadow: 0 20px 60px rgba(0, 0, 0, 0.6);
    display: flex;
    flex-direction: column;
    gap: 10px;
    min-height: 0;
  }
  .top {
    display: flex;
    align-items: center;
    gap: 16px;
    flex-wrap: wrap;
  }
  h2 {
    font-size: calc(16px * var(--text-scale));
    font-weight: 600;
    margin: 0;
  }
  .tabs {
    display: flex;
    gap: 2px;
  }
  .tabs button {
    background: transparent;
    border: 1px solid transparent;
    color: var(--muted);
  }
  .tabs button.on {
    color: var(--text);
    border-color: var(--line);
    background: var(--raised);
  }
  p {
    margin: 0;
    font-size: calc(12.5px * var(--text-scale));
  }
  .muted {
    color: var(--muted);
  }
  .warn {
    color: var(--accent);
  }
  .text {
    margin: 0;
    padding: 8px 10px;
    background: var(--well);
    border: 1px solid var(--line-soft);
    border-radius: 6px;
    font-family: ui-monospace, "Cascadia Mono", Consolas, monospace;
    font-size: calc(11px * var(--text-scale));
    line-height: 1.45;
    white-space: pre-wrap;
    overflow: auto;
    max-height: 340px;
    min-height: 120px;
    user-select: text;
  }
  .text.tall {
    max-height: 420px;
  }
  table {
    border-collapse: collapse;
    font-size: calc(12px * var(--text-scale));
    width: 100%;
  }
  th {
    text-align: left;
    color: var(--muted);
    font-weight: 500;
    padding: 4px 8px 4px 0;
    border-bottom: 1px solid var(--line-soft);
  }
  td {
    padding: 4px 8px 4px 0;
    border-bottom: 1px solid var(--line-soft);
    vertical-align: top;
  }
  td.detail,
  .detail {
    color: var(--muted);
  }
  td.num {
    text-align: right;
    white-space: nowrap;
    font-variant-numeric: tabular-nums;
  }
  .badge {
    display: inline-block;
    padding: 1px 7px;
    border-radius: 9px;
    font-size: calc(11px * var(--text-scale));
    white-space: nowrap;
  }
  h3 {
    margin: 10px 0 0;
    font-size: 13px;
    font-weight: 600;
  }
  .badge.ok {
    background: color-mix(in srgb, var(--green) 22%, transparent);
    color: var(--green);
  }
  .badge.skipped {
    background: var(--raised);
    color: var(--muted);
  }
  .badge.mismatch,
  .badge.error {
    background: color-mix(in srgb, var(--red) 22%, transparent);
    color: var(--red);
  }
  .row {
    display: flex;
    gap: 6px;
  }
  .row input {
    flex: 1;
  }
  .buttons {
    display: flex;
    gap: 8px;
    margin-top: 4px;
  }
  .grow {
    flex: 1;
  }
</style>
