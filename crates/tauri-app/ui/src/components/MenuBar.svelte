<script lang="ts">
  import { PANEL_LABEL, showPanel, dock, type PanelId } from "../lib/dock.svelte";
  import { workspaces, allWorkspaces, find, isBuiltIn, isChanged, modified, switchTo, saveAs, remove, rename, resetLayout, sizes } from "../lib/workspace.svelte";
  import { app, run, say, setPanel } from "../lib/session.svelte";
  import { actions, askText, checkUnsaved, question } from "../lib/actions.svelte";
  import { openMenu, type MenuItem } from "../lib/menu.svelte";
  import { api, COLOR_DEPTHS, type RecentView } from "../lib/api";
  import { openPrefs } from "../lib/prefs.svelte";
  import { view } from "../lib/timeline.svelte";

  const panelNames: [string, string][] = [
    ["media", "Media"],
    ["effects", "Effects"],
    ["history", "History"],
    ["viewer", "Viewer"],
    ["controls", "Controls"],
    ["text", "Text"],
    ["track", "Track"],
    ["scopes", "Scopes"],
    ["timeline", "Timeline"],
    ["meters", "Meters"],
  ];

  /// File > Save as template…: name it; it's then on the start screen's Your templates.
  async function saveTemplate() {
    const name = await askText("Save as template", "New projects can start from this one (its sequences, tracks, titles, effects and media) on the start screen, under Your templates. A template of the same name is replaced.", app.s?.title && app.s.title !== "Untitled" ? app.s.title : "My template", "Save template");
    if (name?.trim()) {
      const path = await run(api.saveAsTemplate(name.trim()));
      if (path) say(`Saved the template “${name.trim()}”: find it on the start screen (File → Start screen)`);
    }
  }

  /// Timeline → Water → Export water element: ask where, then queue it.
  async function exportWaterElement() {
    const clip = app.s?.selected[0];
    if (clip == null) return;
    const suggested = await run(api.waterElementPath(clip));
    if (!suggested) return;
    const path = await run(api.exportPathDialog(suggested, "mov"));
    if (path) await run(api.exportWaterElement(clip, path));
  }

  /// The File menu, with the recent projects fetched first.
  async function showFile(e: MouseEvent) {
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    const recent = await api.startView().then((v) => v.recent).catch(() => [] as RecentView[]);
    openMenu({ clientX: r.left, clientY: r.bottom + 2, preventDefault() {}, stopPropagation() {} } as MouseEvent, file(recent));
  }

  function show(e: MouseEvent, items: MenuItem[]) {
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    openMenu({ clientX: r.left, clientY: r.bottom + 2, preventDefault() {}, stopPropagation() {} } as MouseEvent, items);
  }

  const file = (recent: RecentView[]): MenuItem[] => [
    { label: "New project", hint: "Ctrl+N", run: actions.newProject },
    { label: "Start screen…", hint: "Ctrl+Shift+N", run: () => (app.startOpen = true) },
    { label: "Open…", hint: "Ctrl+O", run: actions.open },
    { label: "Open a Premiere Pro, After Effects or HitFilm project…", hint: ".prproj, .aep, .hfp, .hfcs", run: actions.openAdobe },
    {
      label: "Open recent",
      disabled: !recent.length,
      submenu: [
        ...recent.slice(0, 10).map((r) => ({
          label: r.name + (r.exists ? "" : "  (not found)"),
          hint: r.summary,
          disabled: !r.exists || r.open,
          run: async () => {
            if (await checkUnsaved()) await run(api.startOpenRecent(r.path));
          },
        })),
        { separator: true },
        { label: "More on the start screen…", run: () => (app.startOpen = true) },
      ],
    },
    { label: "Open an autosave…", run: actions.openAutosave },
    { label: "Save", hint: "Ctrl+S", run: actions.save },
    { label: "Save as…", hint: "Ctrl+Shift+S", run: actions.saveAs },
    { label: "Save as template…", run: saveTemplate },
    { label: "Save version…", hint: "Ctrl+Alt+S", run: () => (app.versionsOpen = true) },
    { label: "Project versions…", run: () => (app.versionsOpen = true) },
    { separator: true },
    { label: "Import media…", hint: "Ctrl+I", run: actions.importMedia },
    { label: "Add motion graphics template…", run: () => run(api.addTemplate()) },
    { label: "Save sequence as motion graphics template…", run: () => run(api.saveTemplate()) },
    { label: "Export…", hint: "Ctrl+M", run: actions.exportOpen },
    {
      label: "Export edit for another editor",
      submenu: [
        { label: "EDL (CMX 3600)…", hint: "Avid, Premiere, Resolve", run: () => run(api.exportEdit("edl")) },
        { label: "Final Cut Pro X XML (FCPXML)…", hint: "Final Cut Pro, Resolve", run: () => run(api.exportEdit("fcpxml")) },
        { label: "Final Cut Pro 7 XML…", hint: "Premiere, Resolve", run: () => run(api.exportEdit("xml")) },
        { label: "OpenTimelineIO…", hint: "Resolve, Hiero, RV", run: () => run(api.exportEdit("otio")) },
      ],
    },
    { label: "Snapshot of the frame", hint: "Ctrl+Shift+E", run: () => run(api.snapshot(false)) },
    { label: "Save snapshot as…", run: () => run(api.snapshot(true)) },
    { label: "Export queue", checked: app.queueOpen, run: () => (app.queueOpen = !app.queueOpen) },
    { label: "Collect project…", run: () => (app.collectOpen = true) },
    { separator: true },
    { label: "Settings…", run: () => (app.settingsOpen = true) },
  ];

  const edit = (): MenuItem[] => {
    const s = app.s;
    const has = (s?.selected.length ?? 0) > 0;
    return [
      { label: s?.undo ? `Undo ${s.undo}` : "Undo", hint: "Ctrl+Z", disabled: !s?.undo, run: actions.undo },
      { label: s?.redo ? `Redo ${s.redo}` : "Redo", hint: "Ctrl+Y", disabled: !s?.redo, run: actions.redo },
      { separator: true },
      { label: "Cut", hint: "Ctrl+X", disabled: !has, run: () => actions.act("cut") },
      { label: "Copy", hint: "Ctrl+C", disabled: !has, run: () => actions.act("copy") },
      { label: "Paste", hint: "Ctrl+V", run: () => actions.act("paste") },
      { label: "Paste attributes…", hint: "Ctrl+Alt+V", run: () => (app.pasteAttrsOpen = true) },
      { label: "Delete", hint: "Del", disabled: !has, run: () => actions.act("delete") },
      { label: "Ripple delete", hint: "Shift+Del", disabled: !has, run: () => actions.act("ripple_delete") },
      { separator: true },
      { label: "Select all", hint: "Ctrl+A", run: () => actions.act("select_all") },
      { label: "Unlink audio and video", disabled: !s?.inspector?.linked, run: () => actions.act("unlink") },
      { separator: true },
      { label: "Effects…", hint: "E", run: () => (app.effectsOpen = true) },
      { label: "Plugins…", run: () => (app.pluginsOpen = true) },
      { separator: true },
      {
        label: "Settings",
        submenu: [
          { label: "Renders", heading: true },
          {
            label: "Default Color Bit Depth",
            hint: COLOR_DEPTHS.find(([d]) => d === (app.s?.prefs?.color_depth ?? "HalfLinear"))?.[1],
            submenu: COLOR_DEPTHS.map(([d, label]) => ({
              label,
              checked: (app.s?.prefs?.color_depth ?? "HalfLinear") === d,
              run: () => run(api.setColorDepth(d)),
            })),
          },
          { separator: true },
          { label: "All settings…", run: () => (app.settingsOpen = true) },
        ],
      },
      {
        label: "Preferences",
        submenu: [
          { label: "Label colors…", run: () => openPrefs("labels") },
          { label: "Interface colors…", run: () => openPrefs("ui") },
          { label: "Button colors…", run: () => openPrefs("buttons") },
          { label: "Fonts…", run: () => openPrefs("fonts") },
        ],
      },
    ];
  };

  const tabs = (): MenuItem[] => [
    ...panelNames.map(([k, label]) => ({
      label,
      checked: app.panels[k],
      run: () => {
        const on = !app.panels[k];
        setPanel(k, on);
        // Switched on: in front in its dock (or raised, if it floats).
        if (on && k in PANEL_LABEL) showPanel(k as PanelId);
      },
    })),
    { separator: true },
    {
      label: "Show all windows",
      run: () => panelNames.forEach(([k]) => setPanel(k, true)),
    },
    { separator: true },
    { label: "Workspace", submenu: workspaceMenu() },
    {
      label: "Reset layout",
      run: resetLayout,
    },
  ];

  // Workspaces: switch, save, rename, delete (also the switcher at the right of the bar).
  const workspaceMenu = (): MenuItem[] => {
    const cur = workspaces.current;
    const changed = modified();
    const list = allWorkspaces();
    const items: MenuItem[] = list.map((w, i) => ({
      label: w.name,
      checked: w.name === cur,
      hint: i < 9 ? `Alt+Shift+${i + 1}` : undefined,
      run: () => switchTo(w.name),
    }));
    items.push(
      { separator: true },
      { label: `Save changes to “${cur}”`, disabled: !changed || !find(cur), run: () => (saveAs(cur), say(`Saved the “${cur}” workspace`)) },
      { label: "Save as new workspace…", run: newWorkspace },
    );
    if (find(cur) && !isBuiltIn(cur)) {
      items.push({ label: `Rename “${cur}”…`, run: () => renameWorkspace(cur) }, { label: `Delete “${cur}”`, run: () => deleteWorkspace(cur) });
    } else if (isChanged(cur)) {
      items.push({ label: `Restore “${cur}” as it came`, run: () => (remove(cur), say(`“${cur}” is back as it came`)) });
    }
    if (changed) items.push({ separator: true }, { label: `Undo layout changes (back to “${cur}”)`, run: () => switchTo(cur) });
    return items;
  };
  async function newWorkspace() {
    const name = (await askText("Save workspace", "A name for this arrangement of panels:", "", "Save"))?.trim();
    if (!name) return;
    if (find(name) && (await question("Replace workspace?", `There's already a workspace called “${name}”. Replace it with this layout?`, ["Replace", "Cancel"])) !== "Replace") return;
    saveAs(name);
    say(`Saved the “${name}” workspace`);
  }
  async function renameWorkspace(from: string) {
    const to = (await askText("Rename workspace", `A new name for “${from}”:`, from, "Rename"))?.trim();
    if (!to || to === from) return;
    if (isBuiltIn(to)) return say(`“${to}” is a built-in workspace's name`);
    rename(from, to);
  }
  async function deleteWorkspace(name: string) {
    if ((await question("Delete workspace?", `Delete the “${name}” workspace? The current layout stays as it is.`, ["Delete", "Cancel"])) !== "Delete") return;
    remove(name);
    say(`Deleted the “${name}” workspace`);
  }
  const wsLabel = $derived.by(() => {
    // Re-evaluate when the layout changes.
    JSON.stringify([dock, sizes, app.panels, view.headerW, workspaces.saved]);
    return { name: workspaces.current, changed: modified() };
  });

  const timeline = (): MenuItem[] => [
    { label: "Play / pause", hint: "Space", run: () => actions.act("play") },
    { label: "Split at playhead", hint: "S", run: () => actions.act("split") },
    { label: "Magnetic timeline", hint: "main track closes up", checked: !!app.s?.seq.magnetic, run: () => run(api.setMagnetic(!app.s?.seq.magnetic)) },
    { label: "Cross-dissolve at playhead", hint: "Ctrl+D", run: () => actions.act("dissolve") },
    { label: "Audio crossfade at playhead", hint: "Ctrl+Shift+D", run: () => actions.act("crossfade") },
    { label: "Duck music under dialogue…", run: () => (app.duckOpen = true) },
    { label: "Match colour…", run: () => (app.matchColorOpen = true) },
    { label: "New multicam source…", run: () => (app.multicamOpen = []) },
    { label: "Multicam: show all angles", hint: "in the Viewer", checked: !!app.live?.multicam?.grid, disabled: !app.live?.multicam, run: () => run(api.setMulticamGrid(!app.live?.multicam?.grid)) },
    { label: "Transitions…", run: () => (app.effectsOpen = true) },
    { label: "Freeze frame at playhead", hint: "Shift+F", run: () => actions.act("freeze_frame") },
    { label: "Add title", hint: "Ctrl+T", run: actions.addTitle },
    { label: "Add 3D text", run: actions.add3dText },
    { label: "Add 3D logo (SVG)…", run: actions.add3dLogo },
    { label: "Add 3D model…", run: actions.add3dModel },
    { label: "Add energy blade layer", run: () => run(api.addBladeLayer()) },
    { label: "Add particles layer", run: () => run(api.addParticlesLayer()) },
    {
      label: "Particles & simulation",
      submenu: [
        { label: "Fire", run: () => run(api.addSimulation("fire")) },
        { label: "Smoke", run: () => run(api.addSimulation("smoke")) },
        { label: "Explosion", run: () => run(api.addSimulation("explosion")) },
        { separator: true },
        { label: "Rain", run: () => run(api.addSimulation("rain")) },
        { label: "Snow", run: () => run(api.addSimulation("snow")) },
        { label: "Fog", run: () => run(api.addSimulation("fog")) },
        { separator: true },
        { label: "Muzzle flash", run: () => run(api.addSimulation("muzzle")) },
        { label: "Bullet hit", run: () => run(api.addSimulation("bullet")) },
        { label: "Gun shot (flash and hit)", run: () => run(api.addSimulation("gunshot")) },
        { separator: true },
        { label: "Fluid (ink or coloured smoke)", run: () => run(api.addSimulation("fluid")) },
        { separator: true },
        { label: "Shatter the selected layer", disabled: app.s?.selected.length !== 1, run: () => run(api.addSimulation("shatter")) },
        { label: "Atomic particles from the selected layer", disabled: app.s?.selected.length !== 1, run: () => run(api.addSimulation("atomic")) },
        { label: "Disintegrate the selected layer", disabled: app.s?.selected.length !== 1, run: () => run(api.addSimulation("disintegrate")) },
      ],
    },
    {
      label: "Water",
      submenu: [
        { label: "Ocean", run: () => run(api.addSimulation("ocean")) },
        { label: "Ocean, water only (for a sky of your own)", run: () => run(api.addSimulation("ocean_water")) },
        { label: "Stormy sea at sunset", run: () => run(api.addSimulation("storm")) },
        { separator: true },
        { label: "Rain on water", run: () => run(api.addSimulation("rain_water")) },
        { label: "Ripples from drops at a point", run: () => run(api.addSimulation("ripples")) },
        { label: "Waves rolling in", run: () => run(api.addSimulation("waves")) },
        { separator: true },
        { label: "Pour water", run: () => run(api.addSimulation("pour")) },
        { label: "Waterfall", run: () => run(api.addSimulation("waterfall")) },
        { label: "Fill with water", run: () => run(api.addSimulation("fill")) },
        { label: "Fill a tank (with depth)", run: () => run(api.addSimulation("tank")) },
        { separator: true },
        { label: "Water round the selected layer (green screen)", disabled: app.s?.selected.length !== 1, run: () => run(api.addSimulation("water_round")) },
        { label: "Pour water onto the selected layer (green screen)", disabled: app.s?.selected.length !== 1, run: () => run(api.addSimulation("pour_round")) },
        { label: "Sea round the selected layer (green screen)", disabled: app.s?.selected.length !== 1, run: () => run(api.addSimulation("sea_round")) },
        { separator: true },
        app.live?.simulating
          ? { label: `Stop simulating (${Math.round((app.live.render ?? 0) * 100)}%)`, hint: "Esc", run: () => actions.act("stop_render_preview") }
          : { label: "Simulate now", run: () => actions.act("simulate_now") },
        { label: "Export water element (ProRes 4444)…", disabled: app.s?.selected.length !== 1, run: () => exportWaterElement() },
      ],
    },
    { label: "Add plane (new layer)", run: () => run(api.addNew("layer")) },
    { label: "Add adjustment layer", run: () => run(api.addNew("adjustment")) },
    { label: "Add point", run: () => run(api.addNew("point")) },
    { label: "Add camera", run: () => run(api.addScene(true, 0)) },
    {
      label: "Add light",
      submenu: [
        { label: "Point light", run: () => run(api.addScene(false, 0)) },
        { label: "Spot light", run: () => run(api.addScene(false, 1)) },
        { label: "Directional light", run: () => run(api.addScene(false, 2)) },
        { label: "Ambient light", run: () => run(api.addScene(false, 3)) },
        { label: "Environment light (HDR panorama)…", run: () => run(api.addEnvironmentLight()) },
      ],
    },
    { label: "Nudge selected clips earlier", hint: ",", run: () => actions.act("nudge_left") },
    { label: "Nudge selected clips later", hint: ".", run: () => actions.act("nudge_right") },
    { separator: true },
    app.live?.render != null
      ? { label: `Stop rendering the preview (${Math.round((app.live.render ?? 0) * 100)}%)`, hint: "Esc", run: () => actions.act("stop_render_preview") }
      : { label: "Render preview", hint: "Enter", run: () => actions.act("render_preview") },
    { label: "Clear render cache", run: () => run(api.clearRenderCache()) },
    { label: "Zoom to fit", hint: "\\", run: () => (view.fit = true) },
    { separator: true },
    { label: "Mark in", hint: "I", run: () => actions.act("mark_in") },
    { label: "Mark out", hint: "O", run: () => actions.act("mark_out") },
    { label: "Clear in and out", run: () => actions.act("clear_marks") },
    { separator: true },
    { label: "Add marker", hint: "M", run: () => actions.act("add_marker") },
    { label: "Previous marker", hint: "Shift+↑", run: () => actions.act("prev_marker") },
    { label: "Next marker", hint: "Shift+↓", run: () => actions.act("next_marker") },
    { label: "Previous edit point", hint: "↑", run: () => actions.act("prev_edit") },
    { label: "Next edit point", hint: "↓", run: () => actions.act("next_edit") },
    { separator: true },
    { label: "Keyframe curves", checked: app.timelineTab === "curves", run: () => (app.timelineTab = app.timelineTab === "curves" ? (app.s?.active ? "comp" : "edit") : "curves") },
    { separator: true },
    { label: "Add video track", run: () => actions.act("add_video_track") },
    { label: "Add audio track", run: () => actions.act("add_audio_track") },
    { separator: true },
    { label: app.s?.active ? "Composition settings…" : "Sequence settings…", run: () => (app.seqSettingsOpen = true) },
    { separator: true },
    { label: "New composition", run: () => run(api.newComposite()) },
    { label: "Make composition from selection", hint: "Ctrl+G", run: () => actions.act("make_composite") },
  ];

  async function copyDiagnostics() {
    const text = await run(api.diagnostics());
    if (text == null) return;
    try {
      await navigator.clipboard.writeText(text);
      say("Diagnostics copied: paste them into your message");
    } catch {
      app.aboutTab = "diagnostics";
    }
  }

  const help = (): MenuItem[] => [
    { label: "About & diagnostics…", run: () => (app.aboutTab = "diagnostics") },
    { label: "Copy diagnostics", run: () => copyDiagnostics() },
    { label: "Check decoders…", run: () => (app.aboutTab = "check") },
    { label: "Open log folder", run: () => run(api.openLogFolder()) },
    { label: "Third-party notices…", run: () => (app.aboutTab = "notices") },
    { separator: true },
    { heading: true, label: "Keys" },
    { label: "Space plays, S splits, I and O mark in and out", disabled: true },
    { label: "V B N Y U pick the select, ripple, roll, slip and slide tools", disabled: true },
    { label: "E opens the effects browser", disabled: true },
    { label: "M adds a marker; ↑ ↓ jump between cuts and markers", disabled: true },
    { label: "Click a track's name to highlight it: Paste and new layers go there; Alt+↑ ↓ moves it", disabled: true },
    { label: "J K L shuttle backwards, stop and forwards (press again to go faster)", disabled: true },
    { label: "Ctrl + mouse wheel zooms the timeline; \\ fits the whole sequence", disabled: true },
    { label: "Enter renders the preview (Esc stops); , and . nudge clips (Shift: 10 frames)", disabled: true },
    { label: "Ctrl+G makes a composition; Ctrl+Alt+V pastes attributes", disabled: true },
  ];
</script>

<nav class="bar" aria-label="Menu">
  <span class="mark" aria-hidden="true">R</span>
  <button class="ghost" onclick={showFile}>File</button>
  <button class="ghost" onclick={(e) => show(e, edit())}>Edit</button>
  <button class="ghost" onclick={(e) => show(e, tabs())}>Tabs</button>
  <button class="ghost" onclick={(e) => show(e, timeline())}>Timeline</button>
  <button class="ghost" onclick={(e) => show(e, help())}>Help</button>
  <span class="sep"></span>
  <button class="ghost icon" title={app.s?.undo ? `Undo ${app.s.undo}` : "Nothing to undo"} disabled={!app.s?.undo} onclick={actions.undo} aria-label="Undo">
    <svg viewBox="0 0 16 16" width="15" height="15"><path d="M6 3 2 7l4 4M2.5 7H10a4 4 0 0 1 0 8H7" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" /></svg>
  </button>
  <button class="ghost icon" title={app.s?.redo ? `Redo ${app.s.redo}` : "Nothing to redo"} disabled={!app.s?.redo} onclick={actions.redo} aria-label="Redo">
    <svg viewBox="0 0 16 16" width="15" height="15"><path d="m10 3 4 4-4 4M13.5 7H6a4 4 0 0 0 0 8h3" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" /></svg>
  </button>
  <span class="title">
    {app.s?.title ?? ""}{#if app.live?.dirty}<span class="dot" title="Unsaved changes"></span>{/if}
  </span>
  <button class="ghost ws" onclick={(e) => show(e, workspaceMenu())} title="Workspace: switch, save or reset the arrangement of panels{wsLabel.changed ? ' (changed since chosen)' : ''}">
    <svg viewBox="0 0 16 16" width="13" height="13" aria-hidden="true"><path d="M2 3h12v10H2zM6 3v10M6 8h8" fill="none" stroke="currentColor" stroke-width="1.3" /></svg>
    {wsLabel.name}{#if wsLabel.changed}<span class="changed" aria-label="changed">*</span>{/if}
    <span class="chev" aria-hidden="true">▾</span>
  </button>
  <button class="primary" onclick={actions.exportOpen}>Export</button>
</nav>

<style>
  .bar {
    display: flex;
    align-items: center;
    gap: 2px;
    height: 34px;
    padding: 0 8px;
    background: var(--panel);
    border-bottom: 1px solid var(--line-soft);
  }
  .mark {
    width: 20px;
    height: 20px;
    border-radius: 5px;
    background: var(--accent);
    color: var(--accent-ink);
    font-weight: 600;
    font-size: calc(13px * var(--text-scale));
    display: grid;
    place-items: center;
    margin-right: 8px;
  }
  .ws {
    display: flex;
    align-items: center;
    gap: 6px;
    margin-right: 6px;
    color: var(--muted);
    font-size: calc(12.5px * var(--text-scale));
  }
  .ws .changed {
    color: var(--accent);
    margin-left: -3px;
  }
  .ws .chev {
    font-size: calc(10px * var(--text-scale));
  }
  .sep {
    width: 1px;
    height: 18px;
    background: var(--line);
    margin: 0 8px;
  }
  .icon {
    padding: 3px 6px;
    display: grid;
    place-items: center;
  }
  .title {
    flex: 1;
    text-align: center;
    color: var(--muted);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .dot {
    display: inline-block;
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--accent);
    margin-left: 7px;
    vertical-align: 1px;
  }
</style>
