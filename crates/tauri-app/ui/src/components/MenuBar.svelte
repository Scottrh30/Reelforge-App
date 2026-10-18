<script lang="ts">
  import { app, run, setPanel } from "../lib/session.svelte";
  import { actions } from "../lib/actions.svelte";
  import { openMenu, type MenuItem } from "../lib/menu.svelte";
  import { api } from "../lib/api";
  import { openPrefs } from "../lib/prefs.svelte";
  import { view } from "../lib/timeline.svelte";

  const panelNames: [string, string][] = [
    ["media", "Media"],
    ["history", "History"],
    ["viewer", "Viewer"],
    ["controls", "Controls"],
    ["timeline", "Timeline"],
    ["meters", "Meters"],
  ];

  function show(e: MouseEvent, items: MenuItem[]) {
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    openMenu({ clientX: r.left, clientY: r.bottom + 2, preventDefault() {}, stopPropagation() {} } as MouseEvent, items);
  }

  const file = (): MenuItem[] => [
    { label: "New project", hint: "Ctrl+N", run: actions.newProject },
    { label: "Open…", hint: "Ctrl+O", run: actions.open },
    { label: "Open an autosave…", run: actions.openAutosave },
    { label: "Save", hint: "Ctrl+S", run: actions.save },
    { label: "Save as…", hint: "Ctrl+Shift+S", run: actions.saveAs },
    { separator: true },
    { label: "Import media…", hint: "Ctrl+I", run: actions.importMedia },
    { label: "Export…", hint: "Ctrl+M", run: actions.exportOpen },
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
    ...panelNames.map(([k, label]) => ({ label, checked: app.panels[k], run: () => setPanel(k, !app.panels[k]) })),
    { separator: true },
    {
      label: "Show all windows",
      run: () => panelNames.forEach(([k]) => setPanel(k, true)),
    },
    {
      label: "Reset layout",
      run: () => {
        panelNames.forEach(([k]) => setPanel(k, true));
        view.headerW = 140;
        app.layoutReset++;
      },
    },
  ];

  const timeline = (): MenuItem[] => [
    { label: "Play / pause", hint: "Space", run: () => actions.act("play") },
    { label: "Split at playhead", hint: "S", run: () => actions.act("split") },
    { label: "Cross-dissolve at playhead", hint: "Ctrl+D", run: () => actions.act("dissolve") },
    { label: "Audio crossfade at playhead", hint: "Ctrl+Shift+D", run: () => actions.act("crossfade") },
    { label: "Transitions…", run: () => (app.effectsOpen = true) },
    { label: "Freeze frame at playhead", hint: "Shift+F", run: () => actions.act("freeze_frame") },
    { label: "Add title", hint: "Ctrl+T", run: actions.addTitle },
    { label: "Nudge selected clips earlier", hint: ",", run: () => actions.act("nudge_left") },
    { label: "Nudge selected clips later", hint: ".", run: () => actions.act("nudge_right") },
    { separator: true },
    app.live?.render != null
      ? { label: `Stop rendering the preview (${Math.round((app.live.render ?? 0) * 100)}%)`, hint: "Esc", run: () => actions.act("stop_render_preview") }
      : { label: "Render preview", hint: "Enter", run: () => actions.act("render_preview") },
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
    { label: "New composition", run: () => run(api.newComposite()) },
    { label: "Make composition from selection", hint: "Ctrl+G", run: () => actions.act("make_composite") },
  ];

  const help = (): MenuItem[] => [
    { heading: true, label: "Reelforge" },
    { label: "Space plays, S splits, I and O mark in and out", disabled: true },
    { label: "V B N Y U pick the select, ripple, roll, slip and slide tools", disabled: true },
    { label: "E opens the effects browser", disabled: true },
    { label: "M adds a marker; ↑ ↓ jump between cuts and markers", disabled: true },
    { label: "J K L shuttle backwards, stop and forwards (press again to go faster)", disabled: true },
    { label: "Ctrl + mouse wheel zooms the timeline; \\ fits the whole sequence", disabled: true },
    { label: "Enter renders the preview (Esc stops); , and . nudge clips (Shift: 10 frames)", disabled: true },
    { label: "Ctrl+G makes a composition; Ctrl+Alt+V pastes attributes", disabled: true },
  ];
</script>

<nav class="bar" aria-label="Menu">
  <span class="mark" aria-hidden="true">R</span>
  <button class="ghost" onclick={(e) => show(e, file())}>File</button>
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
