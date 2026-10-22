# Reelforge

A HitFilm-style video editor and compositor, written in Rust. Editing, audio, the GPU
compositor, export (with presets, a queue and GPU encoders), proxies and crash safety are done;
effects and compositing (compositions, motion tracking with corner pinning, planar and mesh tracking) are under way.

What works now:

- One window, built from panels: Media and History on the left, the Viewer in the middle,
  Controls on the right, and along the bottom the Editor Timeline, Composition Timeline and
  Curves tabs with the Meters beside them
- **Divider bars** between the panels: drag one to make the panels on either side bigger or
  smaller. The sizes are remembered the next time you start Reelforge; **Tabs → Reset layout**
  puts them back
- **Tabs menu**: a tick for every window (Media, History, Viewer, Controls, Timeline, Meters).
  Untick one to hide it, tick it to bring it back; **Show all windows** brings back everything,
  **Reset layout** shows them all at their starting sizes. Hidden windows stay hidden the next
  time you start Reelforge
- **Workspaces** (the switcher at the right of the menu bar, or **Tabs → Workspace**): named
  arrangements of the interface, remembering which panels are shown, where each tab is docked
  or floats, and the sizes of the docks, timeline, meters and track headers. Four come built
  in: **Editing** (the starting layout), **Effects** (the effects library in front, wide
  Controls, a bigger Viewer), **Tracking** (the Track panel in front, a big Viewer, a short
  timeline) and **Colour** (wide Controls only, the biggest Viewer). **Alt+Shift+1…9**
  switches to the first nine. A `*` beside the name means the layout has changed since you
  chose it: **Save changes to “…”** keeps the changes in that workspace (a changed built-in one
  can be put back with **Restore “…” as it came**), **Undo layout changes** goes back to it,
  and **Save as new workspace…** keeps it under a name of your own, which can be renamed or
  deleted later. Workspaces are kept on this computer, not in the project
- Import video/image/audio via **File → Import**, `Ctrl+I`, or drag files from Explorer
- FFmpeg decoding on a background thread, with frame-accurate seeking (transport and program streams too: TS, MTS, MPG and VOB are searched by timestamp, so a seek steps back until it starts from a keyframe at or before the target)
- Viewer with transport bar and timecode
- Multi-track timeline: drag media from the bin onto a track, move clips between tracks,
  razor split (`S`), delete (`Del`), scrub, zoom (`Ctrl` + scroll, or the zoom slider in the bar under the tracks), pan (scroll); drag the right edge of the track names to widen them (double-click it to reset)
- Undo/redo for every edit (`Ctrl+Z`, `Ctrl+Y` / `Ctrl+Shift+Z`, or the Edit menu, which names the step)
- **History** panel (a tab next to Media, or **Edit → Undo history**): every edit in order with
  how long ago it was made. Click any step to jump back to just after it (or **Start** to undo
  everything); the steps after it stay listed, greyed and struck through, so you can click
  forward again, until you make a new edit. The last 1,000 steps are kept
- Save/open projects (`.rfproj`, JSON)
- **Audio playback** through your default output device, mixed from all audio tracks
  (any format FFmpeg decodes, resampled to the device rate)
- **A/V sync**: the audio device is the master clock; video follows what's reaching the speakers
- **Linked clips**: footage with sound lands as a video clip + audio clip that move, split and
  delete together. **Edit → Unlink audio/video** separates them
- Audio waveforms on clips; per-track mute (**M**) and show/hide (eye) buttons
- **Timeline thumbnails**: video clips show small frames along their length (speed and reverse
  included). They're decoded in the background, from the proxy when there is one, nearest-first
  for the part of the timeline on screen; while zooming, the nearest frame already decoded fills
  in. Toggle in **Edit → Timeline thumbnails** or Settings

- **GPU compositor**: every visible video track is blended into the frame on the GPU
  (wgpu), in linear light with float precision
- **Per-clip transform** in the Controls panel: position, scale (with a lock for uniform
  scaling), rotation, opacity, **anchor point**, **crop**, and **blend modes** (Normal, Add,
  Multiply, Screen), plus Reset / Fit / Fill buttons. Clips sit at their native pixel size by
  default, like HitFilm
  - **Anchor point**: the pivot for scale and rotation (pixels from the layer's centre).
    Changing it keeps the picture where it is; **Centre** puts it back in the middle
  - **Crop**: cut away the left, top, right and bottom edges (% of the layer). The crop moves,
    scales and rotates with the layer
- **On-canvas editing** (the Viewer gizmo on the selected layer): click a layer to select it,
  drag inside it to move it, and:
  - **corner handles** scale uniformly around the anchor (**Shift**: X and Y separately);
  - **edge handles** stretch one way (**Shift**: both); **Ctrl + edge** crops that edge;
  - the **stem above the top edge** (or just outside any corner) rotates around the anchor
    (**Shift**: 15° steps);
  - **Ctrl + the cross** moves the anchor point; it snaps to the middle, corners and edge
    midpoints (**Shift**: no snapping) and the picture stays put.
  A readout next to the pointer shows the value while you drag, and each gesture is one undo
  step (keyed at the playhead when the property is animated)

- **Export** (**File → Export…**, `Ctrl+M`): renders the timeline (or the in/out range) in the
  background; you can keep editing meanwhile. Export waits for every exact frame at full
  resolution and uses the same compositor and mixer as playback, so the file matches what you
  saw and heard
  - **Presets**: YouTube 1080p, YouTube 4K (H.265), YouTube 4K HDR (H.265, HLG),
    Shorts/Reels/TikTok 9:16 (1080×1920), Instagram portrait 4:5, Square 1:1, ProRes 422 HQ,
    DNxHR HQ and CineForm 422 masters, graphics with transparency (ProRes 4444), a quick
    half-size draft on the GPU, PNG/JPEG image sequences and audio-only WAV. Changing any setting turns the preset into "Custom"
  - **Your own presets**: **Save as preset…** keeps the current settings under a name (marked
    ★ in the list, with **Delete preset** next to it). They're kept with Reelforge's settings,
    so they're there next time
  - **Colour** (H.265): **SDR 8-bit**, **SDR 10-bit** (no banding in skies, fades and
    vignettes), **HDR10 (PQ)** or **HDR (HLG)**, both 10-bit BT.2020. For HDR, white in the edit
    becomes 203 nits (PQ) or 75% signal (HLG), the ITU-R BT.2408 reference, and anything the
    edit pushes brighter than white (glow, exposure) keeps its extra brightness instead of
    clipping. HDR10 files carry mastering metadata. 10-bit and HDR exports render at 16 bits per
    channel; ProRes, CineForm, DNxHR HQX/444 and 10-bit DNxHD get the 16-bit render too (they're
    always 10-bit or more). Other codecs stay 8-bit SDR
  - **Rate** (H.264/H.265): a **Quality** setting (constant quality), or a target **Bitrate**
    in Mbit/s with the estimated file size beside it (peaks may reach 1.5× the target). Picking
    Bitrate suggests a sensible starting value for the size
  - **Formats**, listed under headings: H.264 or H.265 with AAC (MP4); the editing and master
    codecs below, all with 24-bit PCM audio and every core encoding; PNG, JPEG or TIFF sequences
    (`name_00001.png`, …); WAV. The file extension follows the format
    - **Apple ProRes** (MOV): 422 Proxy, 422 LT, 422, 422 HQ, 4444 and 4444 XQ
    - **Avid DNxHR** (any size): LB, SQ, HQ, HQX (10-bit) and 444 (10-bit 4:4:4); and classic
      **DNxHD** (8-bit at 1920×1080, 1440×1080, 1280×720 or 960×720; 10-bit at 1920×1080 or
      1280×720). DNxHD's bitrate follows the frame rate as Avid's does: High gives 175 at
      23.976, 185 at 25, 220 at 29.97, 365 at 50 and 440 at 59.94 (1080p); Medium 115/120/145;
      Low 36/45. The dialog says when the size doesn't suit DNxHD and offers the sizes that do.
      DNx files can be **MOV or MXF** (OP1a, which Avid Media Composer links to and imports):
      pick it under **File type**
    - **GoPro CineForm** (MOV): 422 (10-bit YUV) or 4444 (12-bit RGB). Quality sets CineForm's
      level: High is Film Scan 3+, Medium Film Scan 1, Low High
  - **Transparent background** (ProRes 4444, ProRes 4444 XQ, CineForm 4444, PNG and TIFF):
    where no layer covers the frame the file is see-through instead of black, with soft edges,
    masks and opacity kept, for laying titles and graphics over footage in another program.
    Letterbox bars are see-through too
  - **Any size**: the sequence size, half, quarter, common sizes, or custom. When the shape
    differs from the sequence, choose **Fit** (black bars), **Fill** (crop the edges; the
    default for vertical and square presets) or **Stretch**
  - **GPU encoders**: tick "Use the graphics card" to encode H.264/H.265 with NVENC (NVIDIA),
    Quick Sync (Intel) or AMF (AMD), several times faster. If none works on the PC it quietly
    falls back to the software encoder; the queue shows which encoder was used
  - **Export queue** (**File → Export queue…**): "Add to queue" collects several exports (each
    a snapshot of the project at that moment, so you can keep editing or queue versions), then
    **Start queue** renders them one after another, with progress, time left, cancel, retry,
    reorder and "Show in folder". The status bar shows queue progress
- **Audio levels**: per-clip volume (dB), pan and fade-in/out, plus per-track volume and pan,
  in the Controls panel (select an audio clip, or a video clip with linked audio). Changes
  are heard immediately during playback and are included in exports
- **Fades on the timeline**: drag the small handles at the top corners of an audio clip; the
  fade curve is drawn on the clip and the waveform follows volume and fades
- **Volume keyframes** (automation): each audio clip shows a yellow volume line. **Ctrl+click**
  it to add a keyframe, drag keyframes to move them (time and level), **double-click** or
  **Alt+click** one to delete it, and drag the line itself to raise/lower the whole clip. The
  Controls panel has previous/next keyframe buttons, a diamond to add/remove a keyframe at the
  playhead, and Clear. Keyframes are tied to the clip's source audio, so they move, split and
  trim with it
- **Duck music under dialogue** (**Timeline → Duck music under dialogue…**): tick the dialogue
  and music audio tracks, and Reelforge listens to the dialogue and lowers the music wherever
  someone speaks: by how much (default 12 dB), what counts as speech (a loudness threshold),
  how fast it fades down and back up, and how long a pause keeps it down. It's written as
  ordinary volume keyframes on the music clips, so you can fine-tune them. Ducking again
  replaces the earlier ducking instead of adding to it, and **Remove ducking** takes it off
  while keeping keyframes you added yourself. One undo step either way
- **Audio crossfades**: **Edit → Audio crossfade at playhead** (`Ctrl+Shift+D`) adds a 1 s
  equal-power crossfade at the nearest cut between two touching audio clips (press again to
  remove); its length is editable in the Controls panel ("Crossfade in"). A crossfade plays the
  outgoing clip past its end and the incoming one before its start, so it needs unused media
  either side of the cut; if there isn't enough it's shortened (and the panel says so), and with
  none at all it stays a plain cut (red marker on the timeline)
- **Pan keyframes**: the Controls panel's **Pan keys** row works like the volume keyframes
  (previous/next, a diamond to add or remove one at the playhead, Clear); once a clip has pan
  keys, moving the Pan slider keys the pan at the playhead. Small cyan arrows along the bottom
  of the clip show each key and which way it's panned. The pan glides between keys (inside
  compositions too) and exports exactly as heard
- **Track meters**: every audio track shows a small left/right level meter in its header, after
  its volume and pan (peak, with a peak-hold mark and a red clip light; click it to reset)
- **Audio meters**: a Meters panel next to the timeline shows the master output: peak and RMS per
  channel, a 1.5 s peak-hold line with its value, and clip lights that stay lit until you click
- **Trimming**: drag either edge of a clip to shorten or lengthen it (linked audio/video trim
  together). It stops at the end of the media, at neighbouring clips, and at a one-frame
  minimum; the viewer shows the frame at the edge while you drag. Still images can be stretched
  to any length. Edges snap to other clips (Alt to disable)
- **Ripple and roll trims**: three trim tools in the corner of the timeline (or press **V**, **B**,
  **N**). The edge under the pointer lights up in the tool's colour.
  - **Select (V)**: a plain trim, as above.
  - **Ripple (B)**: trimming an edge moves every later clip on every track, so nothing drifts out
    of sync and no gap opens or overlap appears. Trimming a start edge keeps the clip where it is
    and pulls everything after it back. If a clip on another track straddles the edit and
    would be overlapped, the trim stops there.
  - **Roll (N)**: drag the cut between two touching clips: one gets longer, the other shorter, and
    nothing else on the timeline moves. It's limited by the spare media on both sides. Linked
    audio rolls along with its video.
  - The status bar shows the change, how many clips shifted, and when a limit has been reached.
    Each trim is one undo step.
- **Ripple delete** (**Shift+Del** or **Edit → Ripple delete**): removes the selected clip and
  closes the gap it leaves on all tracks
- **Slip and slide**: two more tools next to the trim tools, acting on the whole clip.
  - **Slip (Y)**: drag a clip to change which part of its media it shows. It stays where it is
    and keeps its length. While you drag, a band under the clip shows the whole source file, so
    you can see how much is left either side.
  - **Slide (U)**: drag a clip along the timeline, keeping its content. The clip before it gets
    longer or shorter to stay butted up, and the clip after it has its start trimmed to match,
    so nothing else moves. Where there's a gap instead of a neighbour, the clip can slide into the
    gap but not past it.
  - Both work on linked audio too, stop at the limits of the media, and are one undo step.
- **Track locking**: the padlock in each track header. A locked track's clips can't be moved,
  trimmed, split, deleted or changed in the Controls panel, and ripple edits leave them where
  they are (the lane is hatched so it's obvious). A clip linked to one on a locked track is
  locked too, so locking never splits a clip from its sound.
- **In/out points**: **I** and **O** (or the Edit menu) mark a range at the playhead (the out
  point includes the frame under the playhead); **Alt+X** clears them. The range shows in blue
  on the ruler, and **Export** then offers "In to out" as well as the whole sequence
- **New layer menu** (the **New…** list at the top of the timeline, or the Timeline menu): adds,
  on a new top track from the playhead, a **New Layer** (a plain plane of colour the size of the
  frame; pick its colour under Layer in Controls), **New Light**, **New Point**, **New Camera** or
  **New Adjustment Layer**. An adjustment layer has no picture: its effects (with its opacity,
  crop, masks and blend mode) change everything below it. A point is an invisible 3D null:
  choose it as a layer's **Parent** (Controls → Layer) and the layer moves, turns and scales with
  it; points can be parented to points, and cameras and lights can follow them too
- **Effects** (Controls panel, **Add effect…**, or the **Effects & transitions** browser, `E`):
  a stack per video clip, applied on the GPU in linear light before the clip's transform, so
  they move, scale and rotate with it:
  - **Colour correction**: exposure (in stops), contrast, saturation, temperature and tint
  - **Levels**: input black/white points, gamma, output black/white (0–255, like Photoshop)
  - **Curves**: master, red, green and blue tone curves drawn in a small editor (drag a point,
    double-click to add one, right-click to remove; smooth curves that never overshoot), with
    a Strength mix
  - **LUT (.cube)**: load any 3D `.cube` LUT (Resolve, Photoshop, camera LUT packs), sampled
    with full trilinear interpolation, with a Strength mix
  - **Gaussian blur**: radius in pixels (two separable passes, any size)
  - **Sharpen**: unsharp mask with amount and radius
  - **Chroma key**: pick the key colour by hue (green by default, with a swatch), threshold,
    softness and spill suppression
  - **Glow**: threshold, radius, intensity and softness; bright areas bloom outwards
  - **Drop shadow**: distance, angle, softness and opacity, from the layer's alpha
  - **Vignette**: amount, size and softness
  - **Energy blade** (Light & stylize): a glowing light-sword blade drawn from a hilt point to
    a tip point, added onto the picture as light. Drag the round **H** handle (hilt) and the tip
    handle in the Viewer, or drag the dashed line to move the whole blade; Shift while dragging
    the tip keeps its length and only turns it. Its settings, all keyframable, in sections:
    - *Blade*: hilt and tip position (% of the layer, so they can go off the edge) and
      **Length**, for switching it on and off. **Ignite here** / **Retract here** keyframe the
      length from 0 to 100 % (or back) from the playhead over the time you set (0.3 s by default)
    - *Colour*: **Blade colour** (the glow) and **Core colour** (the hot centre), each with a
      colour picker and hex code, plus one-click presets: blue, green, red, purple, yellow,
      orange, cyan and white. Animated colours blend channel by channel between keyframes
    - *Core*: width, softness of its edge, **Taper** towards the tip, and the tip's shape
      (rounded, pointed or flat)
    - *Glow*: size, intensity, a wide faint **Outer haze**, and the blade's overall opacity
    - *Flicker*: how much and how fast the glow pulses (smooth noise, different for each blade),
      and **Wobble**, an unsteady edge along the blade
    - *Motion*: **Motion blur** smears the blade over its sweep since the previous frame
      (100 % = the whole frame), so fast swings streak like real footage; **Blend** is *Add
      (light)* or *Screen*, which keeps bright backgrounds from blowing out
    - **Timeline → Add energy blade layer** puts a blade on its own transparent layer on a new top
      track, as long as the selected clip, so it can have its own blend mode or opacity, and
      you can stack several blades
    - **Following a prop**: track the prop in the **Track** panel (single point to move the
      blade; two points, such as two marks on a stick, to also turn and stretch it; four-point or
      planar for perspective), put the blade where it belongs at the playhead, then choose
      *Energy blade: hilt and tip* under **Apply to**. Blades on the tracked clip and on layers
      over it are listed, and the X/Y/rotation/scale boxes choose which parts of the motion it takes
  - The **browser** (`E`, or **Edit → Effects & transitions…**) lists every effect by category
    (Colour, Blur & sharpen, Keying, Light & stylize) and every transition, with a search box
    and a one-line description of each; **Add** puts an effect on the selected clip
  - Turn each effect on or off, reorder (applied top to bottom) or remove it. Every parameter
    has a keyframe button and works like the transform properties. The viewer and export use
    the same GPU passes, so exports match what you see
- **Tracks**: as many video and audio tracks as you need. **+ Video** / **+ Audio** under the
  track headers (or **Edit → Tracks**) add one; new video tracks go on top. Right-click a
  header to rename, add or delete a track (deleting takes its clips with it, and can be undone);
  double-click a track's name to rename it. Drag a track's header up or down to reorder the
  stack (its clips go with it; video tracks stay above the audio tracks), or use **Move track
  up/down** on the right-click menu. Clicking (or right-clicking) a track's name highlights
  the track, its row included; clicking empty timeline clears it. **Duplicate track** on the
  right-click menu puts a copy just above it, named "Video 1 copy", with all its clips,
  effects, keyframes and masks (one undo step). Clips linked or parented to others on the same
  track stay that way among the copies; a copied video clip isn't linked to the original's
  audio, and parents on other tracks are kept. When the tracks don't fit, the mouse wheel
  scrolls through them (Shift+wheel moves along the timeline)
- **Text titles** (**Edit → Add title at playhead**, `Ctrl+T`, or **New title** in the Media
  panel): a 5 s title on the top free video track (a new track is made if none has room).
  In the Controls panel: multi-line text, any installed font (with a search box), size, colour,
  alignment, line and letter spacing, outline, soft drop shadow (distance, angle, softness,
  colour) and a background box for lower thirds, plus one-click **Centre / Lower third / Top**
  placement. Titles are ordinary layers, so the transform, keyframes, effects, dissolves and
  blend modes all work on them; click the text in the Viewer to select and drag it. Titles are
  purple on the timeline and can be any length
- **Proxies**: heavy footage (above 1080p, or 720p and up in HEVC/AV1/VP9/ProRes/DNxHD/CineForm) gets a
  small editing copy automatically, made in the background, with a progress bar in the Media
  panel; **Make proxy** does it for any clip. Proxies are 960×540 with every frame a keyframe,
  so jumping around is instant (8 s of 4K: ~1.5 s per random seek on the original, 3 ms on the
  proxy). The **Proxies on/off** button in the viewer switches between proxies and originals;
  export always uses the originals. Proxies are kept per file in your cache folder, so other
  projects using the same footage reuse them
- **Hardware decoding**: video is decoded on the graphics card where possible (D3D11VA on
  Windows, VAAPI on Linux, VideoToolbox on macOS), falling back to software automatically if
  the GPU can't handle a file
- **Zero-copy** (Windows; Settings → **Keep decoded video on the graphics card**, on by
  default): hardware-decoded video for the viewer never leaves the graphics card. Normally each
  frame is copied from video memory to main memory and uploaded again for drawing (about
  1.5 GB/s at 4K60). Instead the decoder copies it, within video memory, into a texture shared
  with the compositor, and the compositor reads it from there:
  - FFmpeg's D3D11VA device is created on the same graphics card as the compositor's
    Direct3D 12 device (matched by adapter LUID), from which each frame is copied into a pooled
    NV12/P010 texture with an NT handle, and a shared fence is signalled. The compositor opens
    the texture in Direct3D 12, wraps its two planes as ordinary wgpu textures, waits on the
    fence, and runs the usual YUV-to-RGB pass. The decoder also waits for each copy (up to a
    quarter of a second) before handing it over, so the viewer can never stall on it
  - The first frame of each kind (8-bit NV12, 10-bit P010) also comes the usual way, and the
    compositor checks the two pictures are the same. If they aren't, or anything fails (a
    texture won't open, the decoder is on a different graphics card), zero-copy switches itself
    off until Reelforge restarts and everything carries on the usual way. Settings shows
    whether it's working, how many frames it has shown, or why it's off
  - Only for the viewer: tracking, thumbnails, proxies and export still get frames in memory,
    and frames left on the graphics card aren't kept in the frame cache (they hold one of a
    pool of shared textures, at most 640 MB). Other formats (4:2:2, 4:4:4) and other systems
    (Linux, macOS) work as before
- **Frame cache**: decoded frames are kept in memory (1 GB by default), shared between clips
  of the same file, so scrubbing back over footage you've already seen is instant
- **YUV on the GPU**: most video (4:2:0, 8- or 10-bit, from software or hardware decoders) now
  stays in the YUV form it's stored in until it reaches the graphics card, which converts it to
  RGB as it draws. Decoding 1080p is about twice as fast as before (3.1 ms a frame instead of
  6.0 ms in testing) and each frame takes 3.1 MB of memory instead of 8.3 MB, so the frame cache
  holds 2.7× as many
- **Render preview** (`Enter`, or **Edit → Render preview**): renders every frame between the
  in and out marks (or the whole sequence) exactly, once, and keeps it, so stacks of effects and
  compositions play back smoothly. A green bar along the top of the ruler shows which frames are
  rendered, and the viewer says **Rendered** when the frame on screen comes from it. Editing
  only makes the frames it changes stale (the bar goes away there); undo the edit and they're
  good again. `Esc`, or pressing Play, stops a run. Rendered frames are kept in memory (viewer
  size, at most 1080p), within the **Render cache** size in Settings (2 GB by default, about 10
  s of 1080p at 25 fps); **Edit → Clear render cache** empties it
- **Autosave and crash recovery**: the project is autosaved every 2 minutes (when it has
  changed). If Reelforge doesn't close properly, it offers to recover that work the next time
  it starts; **File > Open an autosave…** opens older ones. Saving always writes a new file and
  then swaps it in, so a crash mid-save can't corrupt the project. Closing, starting a new
  project or opening another asks **Save changes?** if there's unsaved work
- **Missing media**: files that have moved show a red banner in the Media panel and a
  "MEDIA OFFLINE" frame in the viewer. **Relink…** a file and Reelforge also looks for the
  other missing files in that folder (and its subfolders) by name; **Search a folder…** does
  it for all of them. Relinking can be undone. Export won't start while media is missing
- **Settings** (**Edit > Settings…**): hardware decoding, frame cache size, render cache size,
  proxies, autosave interval, and the proxy/autosave folders
- **Speed** (Controls panel, any footage or composition clip): 1–2000%, with one-click 25/50/
  100/200/400%, and **Reverse**. The clip keeps playing the same stretch of footage, so slow
  motion makes it longer (clips after it on its tracks move along) and fast motion shorter;
  linked audio changes speed with it, pitch and all, like tape. Keyframes stay where they are on
  the timeline. Trimming, slipping, splitting and tracking all follow the speed
  - **Freeze frame** (`Shift+F`, the Edit menu, or the Speed section): holds the frame at the
    playhead for 2 s (trim it to taste), splitting the clip there and pushing the rest of it
    (and its audio) later. A freeze frame's **Play from here** turns it back into motion
  - Reversed footage decodes backwards, which is slow for long-GOP files: proxies (all
    keyframes) make it smooth
- **J / K / L**: play backwards / stop / play forwards; press J or L again for 2×, 4×, 8×. Shuttling
  runs on the system clock without sound; Space is normal playback with sound
- **Markers**: **M** drops a marker at the playhead. Markers sit on the ruler (with a faint line
  through the tracks); click one to jump to it, drag it to move it (it snaps to clip edges and the
  playhead, Alt to stop snapping), double-click or right-click it to name it, colour it or delete
  it. Clips snap to markers when dragged
- **Up / Down arrows**: jump to the previous / next cut, marker, or in/out point
- **Selecting several clips**: **Ctrl+click** (or Shift+click) adds a clip to the selection or
  takes it out; **drag a box** over empty track space to select every clip it touches (hold
  Ctrl to add to the selection); **Ctrl+A** selects everything, **Esc** nothing. Dragging any
  selected clip moves them all together (a ghost shows where each lands, red if something's in
  the way), and **Del** deletes them all. Linked audio always comes along
- **Cut, copy and paste** (`Ctrl+X` / `Ctrl+C` / `Ctrl+V`, or the Edit menu): pastes at the
  playhead, keeping the clips' spacing and stacking order, or at the next place where they all
  fit. Everything comes along: keyframes, effects, masks, titles, trackers, links (the copies are
  linked to each other, not to the originals). Paste into a composition works too: missing
  tracks are added
- **Effect plugins** (**Edit → Plugins…**): write your own GPU effects and install them without
  rebuilding Reelforge. A plugin is a folder with a `plugin.toml` (name, category, up to 16
  parameters: numbers, checkboxes and menus) and a WGSL shader; it appears in the effects
  browser beside the built-in effects and gets keyframes, undo, paste attributes and export
  like they do. Shaders are checked when loaded, with errors given as a line in your file;
  saving a plugin's files reloads it and the Viewer updates at once. Install from a folder or
  a .zip, start one from a template (**New plugin…**), remove one. Plugins can have several
  passes (blurs, glows) and animate with time. A project using a plugin that isn't installed
  still opens; that effect is skipped and keeps its settings. Five examples ship in
  `plugins/` (RGB Split, Film Grain, Mosaic, Bloom, Wave Warp). **See [PLUGINS.md](PLUGINS.md)
  for how to write one**
- **Collect project** (**File → Collect project…**): copies the project and every file it uses
  (video, audio, images, LUT files and the fonts titles use) into one folder, in `Media`,
  `LUTs` and `Fonts` subfolders next to the project file, with a progress bar and Stop. Files
  with the same name from different folders are kept apart (`clip.mp4`, `clip (2).mp4`),
  nothing already in the folder is overwritten, and collecting again into the same folder
  skips files that are already there. Missing files are listed and left out. The collected
  project refers to its files relative to itself, so the whole folder can be copied to a
  backup drive or another computer (Windows or Linux) and opened from anywhere. Options: leave
  out media no sequence uses, and leave out fonts
- **Project files are portable**: when you save, files inside the project's own folder (or its
  subfolders) are stored by relative path, so moving or renaming the whole folder doesn't break
  anything. Files elsewhere keep their full path (and can be relinked if they move)
- **Paste attributes** (`Ctrl+Alt+V`, or **Edit → Paste attributes…**): copy a clip, select
  others, and choose what to put on them: position/scale/rotation/anchor/opacity, crop, corner
  pin, blend mode, effects (replacing theirs or added after), masks, audio levels (volume, pan,
  fades) or a title's text style (each keeps its own text). The same colour grade on every
  shot in one step. Keyframes keep their distance from the clip's start, masks are scaled to
  each clip's picture size, and it's one undo step
- **Nudging**: `,` / `.` move the selected clips one frame earlier / later (`Shift` for ten),
  with their linked audio; refused (with a reason) if they'd hit another clip. `\` zooms the
  timeline to fit the whole sequence
- **Snapping in the Viewer**: a layer you drag snaps its edges and middle to the frame's edges
  and centre lines and to other layers' edges and middles, with red guide lines showing what
  lined up. Hold **Alt** to place it freely; turn it off in **Edit → Snap in Viewer** or Settings
- **Masks** (Controls panel, any video clip, title or composition): **+ Rectangle**,
  **+ Ellipse**, or **+ Freehand** (click points in the Viewer; click the first point or press
  Enter to close, Esc to cancel). Drag a mask's corner handles in the Viewer to reshape it (a
  rectangle or ellipse can be pulled into any four-sided shape), or select it in the list and
  drag inside it to move it
  - Several masks combine in order: **Add**, **Subtract** (cut a hole) or **Intersect**; each
    can be inverted and has a **feather** (soft edge), an **expansion** (grow/shrink) and an
    opacity
  - **Curves**: double-click a point (or **Make curve** in the Masks panel) to turn it into a
    smooth curve with two handles, or **Alt-drag** a point to pull its handles out. Dragging one
    handle turns the other to keep the curve smooth through the point; **Alt** bends just that
    side. Double-click again for a sharp corner. Rectangles and freehand masks can be curved
  - **Feather per point**: click a point, then set **Feather here** in the Masks panel; the
    feather blends between points along the sides (**Reset** goes back to the mask's feather)
  - **Animate shape**: once ticked, reshaping the mask (points and handles) at the playhead
    sets a keyframe there
  - Masks are applied before the clip's effects and move with the layer's transform
  - With a tracker on the clip, **Apply to → Mask: …** makes the mask follow the tracked point
- **Compositions**: a composition is a sequence of its own, with its own layers, that
  sits on the timeline as one clip (amber, marked `[C]`). Transform it, add effects, dissolve
  it, trim it like any clip; its audio plays and exports with it
  - **Make composition** (`Ctrl+G`, the Edit menu, or **Make from selection** in the Media
    panel) moves the selected clip and its linked audio into a new composition, or every clip
    between the in and out marks if you've set them. Undo puts them back
  - **New composition from media**: right-click footage, a still or a sound file in the Media
    panel. The new composition takes the file's name, picture size, frame rate and length,
    holds the file (picture on Video 1, sound linked on Audio 1, in one undo step), and opens
    in the Composition Timeline, ready for layers on top. Stills use the edit's frame rate and
    last 5 seconds; sound-only files use the edit's size
  - The same right-click menu also adds the file at the playhead or at the end of what you're
    editing (or of the main edit), makes a proxy, relinks a missing file, shows it in its folder,
    and copies its path
  - **Composition Timeline** (a tab next to the Editor Timeline): the timeline of the
    composition you're working on, with a tab for each composition along its top and **+** for
    a new one. The Editor Timeline always shows the main edit. Switching between the two tabs
    switches what the Viewer, Controls and export work on; each sequence remembers its playhead,
    zoom and scroll
  - **Double-click** a composition clip (or **Open** in the Controls or Media panel) to open it
    in the Composition Timeline. **+ New** in the Media panel starts an empty one, and **Add at
    playhead** puts a composition into whatever you're editing (compositions can contain
    compositions; one can't contain itself)
  - Empty parts of a shot are transparent, so layers underneath show through
  - Exporting while inside a shot exports that shot
- **Motion tracking** (the **Track** panel, docked next to Controls; for footage clips). It
  works on the selected clip, and keeps working on a tracked clip after you select something
  else (pick which tracked clip at the top when there are several). Pick a **Method** (Single
  point, Two point, Four point, Planar or Mesh), drag the numbered boxes in the Viewer onto
  sharp details, then **Backward** / **Forward** from the playhead. Under **Apply tracking
  data**, choose any layer the track overlaps (or a mask, or the clip itself to stabilise),
  tick which parts of the motion to use (**X position**, **Y position**, and **Rotation** and
  **Scale** for two-point tracks; for corner pins, planar and mesh tracks X and Y are the
  surface's movement as a whole, its shape still follows), and **Apply**:
  - **Position (1 point)**: movement. A title stuck to a moving car, say.
  - **Position, rotation and scale (2 points)**: put them on two details of the same object, far
    apart. The line between them gives the turn and the change of size, so a following layer
    turns and grows with it, and stabilising takes out rotation and zoom as well.
  - **Corner pin / perspective (4 points)**: on the corners of a flat surface (a screen, a sign,
    a picture frame). **Apply to** another layer pins it into those four points in perspective:
    screen replacement. If that layer already has a corner pin (say you adjusted its corners
    at the first tracked frame), it keeps that shape and moves with the surface instead.
    **This clip: stabilise (perspective)** holds the surface flat and still.
  - **Planar: a whole flat surface**: drag the four round corner handles onto the corners of
    a screen, a sign, a wall or a table top. Instead of four details, everything with detail
    *inside* the shape is tracked (around a hundred small features), and the surface's
    perspective is fitted to all of them every frame, ignoring the ones that disagree (a hand
    or a passing car in front, a reflection). So the corners can sit on blank areas or even
    outside the picture, and the surface can be partly covered. Apply it the same ways as a
    corner pin. On a test clip of a poster swinging in perspective with a bar sweeping across
    it, the corners stay within 1 px of where they really are. It takes about 20 ms a frame at
    1080p on two cores, on top of decoding.
  - **Mesh: a surface that bends**: for cloth, skin, a flag, paper, anything that doesn't stay
    flat. A grid (2 × 2 to 8 × 8 cells; **Grid** in the panel) fills the four big corner handles
    you drag over the surface; before tracking, the inner points can be dragged too, to follow a
    curve. Many small features inside are followed, and every frame the grid's points are solved
    for together (least squares): each feature pulls the corners of its cell so its spot lands
    where it was found, while neighbouring points keep their shape relative to each other, so
    the grid stays smooth and cells without detail move with their neighbours. Features that
    disagree with the rest (something passing in front) are weighted down to nothing, and every
    feature is re-checked against how it looked in the first frame, bent the way the grid has
    bent since. **Apply to** another layer stretches it over the grid, frame by frame (a logo
    on a shirt, a tattoo on an arm); a mask on the clip can follow the grid too. The warp is
    shown under **Mesh warp** in the layer's Transform controls, with **Remove** to go back to
    its own transform. On a test clip of a textured sheet rippling like a flag, the tracked 4 × 3
    grid lays the sheet out within 0.8 px on average (the best any 4 × 3 grid could do there is
    0.5 px; straight-edged cells can't follow a curve inside them). Its corners are its weak
    spot, since the grid is extrapolated there: on the flag's free end they came within 7 px.
    Use more cells for tighter bends, on surfaces with plenty of detail.

  Every point is found to a fraction of a pixel in each frame (centre-weighted normalised
  cross-correlation, so lighting changes and the surroundings of a turning feature don't throw
  it). With two or four points the tracker also compares each feature with how it looked at the
  start, warped by the group's motion, so errors don't build up over long shots. Fast motion is
  fine: raise **Search** and big areas are searched coarse-to-fine, starting where the last
  movement predicts. If a point is lost (blocked, out of frame) tracking stops and says which;
  go there, drag it back onto its detail (that corrects the track at that frame) and track on.
  **Smooth track** (frames) takes the jitter out of the tracked paths without tracking again:
  the Viewer draws the smoothed paths solid and the raw ones faint and dotted, and everything you
  apply the track to (layers, masks, blades, stabilising) uses the smoothed paths. The tracked
  data is kept, so it can go back to 0 at any time; apply the track again after changing it.
  It suits a shaky track of a steady move; real shake of the camera is something the layer
  should follow, so leave it at 0 there. When **stabilising**, **Keep camera moves** (frames)
  keeps deliberate camera moves and takes out only the shake. Everything is written as ordinary keyframes you can edit or undo, and masks on the
  clip can follow the track too
  - **Onto a point**: choose a **Point** (made with **New… → New Point**) under **Apply to** and
    the point follows the track; every layer, camera and light parented to it follows too, so
    one track can drive several layers. The point keeps where it is at the start of the track.
    One-point tracks move it; two-point tracks also turn and scale it; four-point, planar and
    mesh tracks give it the surface's best-fitting move, turn and scale (a point has no
    perspective). Tick X, Y, rotation and scale to choose which parts are keyed
- **3D layers, camera and lights**:
  - **3D layer** (Transform section of the Controls panel): the layer becomes a flat plane in
    3D space with a **Position Z** (positive is further away) and **Rotation X / Y** (Rotation
    turns it about Z), all keyframable and in the curve editor. Positions are in pixels from the
    frame's centre, so with the default camera a 3D layer at Z 0 looks exactly as it did in 2D.
    3D layers next to each other on the timeline are drawn furthest first; 2D layers between them
    keep their places. In the Viewer a 3D layer shows its outline as the camera sees it, and
    dragging it moves it across the camera's view (Shift: straight across or down)
  - **Timeline → Add camera**: a camera clip on a new top track. Its settings (Controls) are its
    position, **point of interest** (what it looks at), **roll**, **field of view**, and **depth
    of field** (focus distance and how much things out of focus blur). The topmost camera clip
    at a moment is the one used; without one, a default camera that leaves 2D-looking layers
    unchanged. Its point of interest can be dragged in the Viewer
  - **Timeline → Add light**: point, spot, directional or ambient, with colour, intensity,
    position, point of interest (spot and directional), cone angle and feather (spot), and
    falloff (none, smooth or inverse square, over a distance). With any lights in the scene, 3D
    layers are shaded by them (both sides, per pixel), unless **Lit by the scene's lights** is
    unticked (for a screen or a sign that glows by itself); with none, 3D layers show as they are.
    Up to 8 lights at once. A light's position and point of interest can be dragged in the Viewer
  - Everything about cameras and lights keyframes like effect settings. Not yet: shadows,
    layers that cut through each other (they're sorted by their middles), and 3D text with depth
- **Corner pin** (Transform section of the Controls panel): tick it to stretch a layer in
  perspective by its four corners. Drag the pink corner handles in the Viewer (**Shift**: straight
  across or down; dragging the middle moves all four). It can be keyframed like any property,
  and position, scale, rotation and anchor don't apply while it's on
- **Curves panel** (a tab next to the Editor Timeline): a value graph of the selected clip's
  keyframes. Pick channels on the left (position X/Y, scale X/Y, rotation, opacity and every
  effect parameter). **Drag** a key to change its time and value (**Shift** changes only the
  value), **double-click** a curve to add a key, **double-click** or **Alt+click** a key to delete
  it, and **right-click** for Linear / Ease / Hold. Hold keys are drawn as squares and ease keys
  hollow. Each drag is one undo step
- Opening a `.rfproj` from the command line (or by double-clicking it once associated) opens the
  project
- **Transitions**: **Edit → Cross-dissolve at playhead** (`Ctrl+D`) adds a 1 s dissolve at
  the nearest cut between two touching video clips, and the browser's **Transitions** tab
  applies any kind there. Change the kind and length with "Transition in" in the Controls panel;
  the timeline labels each one. Kinds: **Cross-dissolve**, **Dip to black**, **Dip to white**,
  **Wipe** left/right/up/down (soft edge), **Iris** (a circle opening from the centre),
  **Slide** left/right/up/down (the next shot slides in over this one) and **Push**
  left/right/up/down (it pushes this one out). Motion is eased in and out. Like audio
  crossfades they use the spare media either side of the cut. Dissolves are rendered in linear
  light: no brightness dip at the midpoint, even for clips that don't fill the frame
- **Video keyframes**: Position, Scale, Rotation, Opacity, Anchor and Crop each have a keyframe button in the
  Controls panel. Click it to start animating that property at the playhead; after that, changing
  the value (or dragging the layer in the Viewer) sets a keyframe at the playhead. Previous/next
  keyframe buttons, and an interpolation choice per keyframe: **Linear**, **Ease** (smooth start
  and stop) or **Hold** (jump). Keyframes appear as diamonds along the bottom of the clip, and
  move, split and trim with it
- **Snapping**: dragged clips snap to other clips' edges, the playhead and the start of the
  timeline (hold **Alt** to move freely)
- **Audio scrubbing**: hear the audio while dragging the playhead or stepping frames with
  ←/→ (toggle in **Edit → Audio scrubbing**)
- Colour-accurate decoding: BT.709/BT.601 and full/limited range are read from each file
  (HD footage is no longer decoded with SD colour maths), and exports are tagged BT.709

Not yet: glow and drop shadow reach only as far as the clip's own frame (a shadow from a
full-frame clip is hidden), and animated text (per-letter reveals).
See the roadmap at the bottom.

## Layout

```
crates/
  core/    Engine-agnostic project model: rational Time, Sequence/Track/Clip, undo commands.
           No FFmpeg, no GPU, fully unit-tested.
  media/   FFmpeg probing, MovieWriter (H.264/AAC encoder + muxer), threaded video decoder + FrameSource ("give me the frame at time t"),
           AudioReader (random-access resampled audio), Mixer (renders the timeline's audio).
  render/  wgpu compositor: transformed layers, blend modes, linear-light float pipeline,
           pixel readback. No interface dependency, so export can use it headless.
  export/  Scene building shared by viewer and export (incl. compositions), the audio mix
           list, and offline render: exact frames -> compositor -> MovieWriter, with
           progress/cancel. Runs headless (tests use it without a window).
  engine/  Everything an interface needs that isn't drawing: the editor model (Editor:
           project, undo, selection, live edits, import, save/open), playback and the cpal
           audio engine, the viewer picture (Preview), render cache, proxies, autosave and
           crash recovery, meters, export presets, plugins, Collect project, the viewer
           gizmo's maths. No interface code.
  tauri-app/
           The app (`reelforge`): a Tauri window hosting the interface (Svelte +
           TypeScript in `ui/`), which reaches the engine through commands. See "The app"
           below.
```

## The app

`reelforge` is a Tauri window: it hosts the Svelte interface in the system's web view
(WebView2 on Windows, WebKitGTK on Linux), and the Rust side gives it the engine through
commands. (Until this version there was also a "classic" egui window; everything it did
moved here and it was retired.)

What it does, panel by panel: the Media panel (import, drag to the timeline, right-click → New
composition from media), the Viewer (with move and scale handles on the selected layer,
and the transport), the Controls panel (transform, opacity, blend, speed, title text, clip
volume and pan, masks, motion tracking), the Editor and Composition timelines (select, box select, move between
tracks, trim, ripple, roll, slip and slide, snapping, split, cut/copy/paste, mute and lock
tracks, track meters, a zoom bar and resizable track names), History, the master meters,
Export (built-in presets, progress, cancel), Settings, unsaved-changes prompts and crash
recovery.

Masks: rectangles, ellipses and freehand shapes (click the points out in the Viewer),
made from the mask tools down the left of the Viewer (Rectangle, Ellipse, Freehand, Move
and Resize; hover one for its name) or the Masks panel, dragged by their points or moved
whole, with add/subtract/intersect, invert, feather,
expansion, opacity and shape keyframes at the playhead. Motion tracking: 1, 2 or 4 points
dragged onto details in the Viewer (or a whole flat surface: planar tracking; or a bending
one: mesh tracking, warping another layer over the tracked grid), tracked forward or backward in the background, then
applied to another layer (follow it, or corner-pin into the points), to one of the clip's
masks, or to the clip itself (stabilise, with smoothing).

Edit → Preferences: label colours (clips on the timeline by kind, and their names, with
ready-made sets), interface colours (panels, wells, lines, text, highlight; five schemes),
button colours (ordinary and main buttons, corner roundness) and fonts (any installed
family for the interface and for figures, text size 80–140%). Changes show at once;
Cancel puts back what was there, Save keeps them for next time.

Viewer zoom and pan: the mouse wheel over the picture zooms in and out about the pointer
(10% of fit up to 3200%); drag with the middle mouse button (or turn on the hand in the Viewer's
top bar and drag) to move around; Shift+wheel moves sideways. The top bar shows the zoom with
− / + buttons, **1:1** for actual pixels, and clicking the percentage fits the whole frame
again. Zoomed in, the picture is fetched at the size it's shown (up to the sequence's own
resolution), so it stays sharp, and handles, masks and trackers stay on it.

The **Effects** panel (a tab next to Media) lists every effect and transition by name, in
folders by group (Colour, Blur & sharpen, Keying, Light & stylize, installed plugins in their
groups, Transitions), with a search box. Double-click a name to add it to the selected clip
(a transition goes in at the selected clip's start, or the cut nearest the playhead), or drag
it onto a clip on the timeline. Open folders are remembered.

Panels dock: Media, Effects, History and Controls are tabs in the left or right dock. Drag a
tab along its dock to reorder, onto the other dock's tabs to move it there, or anywhere else
to float it in its own window (drag its title bar to move it, its corner to resize it; drop
the title bar on a dock's tabs, or use ⇤ / ⇥, to dock it again). Right-click a tab for the
same, and to close it; **Tabs** shows and hides panels, and **Tabs → Reset layout** puts
everything back. The arrangement is remembered.

Effects: the Effects section of the Controls panel holds the clip's stack (top applied
first): add from its menu or from the effects browser (**E**, or Edit → Effects…, with a
search box), switch effects off, reorder, duplicate, reset and remove them, fold them
shut, and edit every setting (drag or type the numbers; menus and tick boxes for plugin
choices). The ◆ beside a setting keyframes it at the playhead; once a setting has keys,
changing it sets the key there. Curves has its own curve editor (Master, Red, Green,
Blue: drag points, click the line to add one, right-click to remove), and LUT a .cube
file picker.

Plugins (Edit → Plugins…): install from a folder or a .zip, make a new one from the
starter template, open the plugin folder, reload, remove (with a confirmation), and
reload automatically when a plugin's files change; a plugin that doesn't load says
why (file, line and column).

Export: every setting (format, size including custom, fit,
quality or target bitrate with a size estimate, 10-bit/HDR, GPU encoder, sound, range),
built-in presets, and your own presets (Save as preset…, kept in the settings). Export
runs it at once; Add to queue lines up several. The export queue (File → Export queue,
or the status bar) floats over the bottom right while you keep editing: progress, fps
and time left, cancel, reorder, retry, show in folder, pause after the current one.

Keyframes: the ◆ beside Position, Scale, Rotation, Anchor and Opacity (and every effect
setting) adds or removes a key at the playhead; once a property has keys, changing it in
the Controls panel or dragging the layer in the Viewer sets the key there. The **Curves**
tab (beside the timelines, or Timeline → Keyframe curves) draws the selected clip's
keyframes as graphs: tick channels on the left, drag keys in time and value (Shift:
value only), double-click a line to add a key or a key to delete it, and right-click a
key for Linear, Ease in and out, Hold or **Bézier**. A Bézier key (drawn round) shows its
handles when selected: drag a handle further out to hold the speed longer, or up and down to
speed up or overshoot (Shift keeps it level, a pure ease). Handles shape the timing, so on
position and scale X and Y share them. Bézier is also in the Controls panel's interpolation list.

Markers: **M** adds one at the playhead; drag it along the ruler (it snaps), double-click
it to name and colour it, right-click for more. **↑ / ↓** jump between cuts and markers,
**Shift + ↑ / ↓** between markers only (also in the Timeline menu).

Proxies: right-click footage in the Media panel to make, retry or delete its proxy (its
progress shows on the thumbnail); "Proxies on/off" in the Viewer switches between the
proxies and the originals; Settings opens the proxy folder or deletes them all. Export
always uses the originals.

Missing media: the Media panel says how many files can't be found; click "Missing ·
Relink" on one to find it (the others in that folder are found too), or "Search a
folder…" to find them all by name. File → Collect project… copies the project and every
file it uses into one folder, in the background, with progress, and can open the copy.

Transitions: **Ctrl+D** toggles a cross-dissolve at the cut nearest the playhead
(**Ctrl+Shift+D** an audio crossfade); the Transitions tab of the effects browser puts any
of the 16 kinds (dissolves, dips, wipes, iris, slides, pushes) on the selected clip's cut
or the nearest one. On the timeline a transition is a box over the cut: drag either end
to change its length, right-click to change its kind or remove it. The Controls panel
shows the transition into the selected clip (kind and length), and warns when there
isn't enough spare media at the cut.

Speed: presets, an exact percentage, reverse, **Shift+F** for a 2 s freeze frame at the
playhead, and **Speed ramp…**: the clip is cut into 4–24 parts whose speeds go from one
percentage to another (evenly or easing in and out), one undo step; later clips move to
make room. **J / K / L** shuttle backwards, stop and forwards (press again for 2×, 4×, 8×).

Viewer: click a layer to select it (the topmost one under the pointer), drag inside it to
move it (snapping to the frame and the other layers, with red guide lines; Alt places it
freely, Shift keeps it straight). The gizmo's corner handles scale (Shift: X and Y
separately), edge handles stretch (Shift: both ways; Ctrl + an edge crops it), the stem above
the top edge or just outside a corner rotates (Shift: 15° steps), Ctrl + the cross moves the
anchor point (it snaps to the middle, corners and edges), and a corner-pinned layer shows its
four corners to drag. A readout by the pointer says what's changing.

Audio: the Controls panel has the clip's volume and pan (each with keyframes: ◀ ◆ ▶ and Clear),
fades and Reset audio; with no clip selected it shows every audio track's volume and pan. On
the timeline each audio clip has its volume line: drag it up or down, Ctrl+click it to add a
key, drag keys, Alt+click (or double-click) one to remove it, and drag the small handles at the
top corners for the fades.

Titles: font (every installed face, searchable), size and colour, alignment, line and letter
spacing, outline, shadow (colour, opacity, distance, angle, softness), a box behind the text
(colour, opacity, padding) and Centre / Lower third / Top placement.

Transform: position, scale (linked or not), rotation, anchor, crop, opacity, blend, corner pin
(on/off and Reset corners), each with ◆ keyframes; ◀ ▶ jump between keyframes and the menu
beside them sets how the keys at the playhead move on (Linear, Ease, Hold); Reset, Fit, Fill
and Centre anchor.

Also: **Enter** renders the preview between the marks (or the whole sequence; **Esc** stops,
a green bar along the ruler shows what's rendered), **Ctrl+G** makes a composition from the
selected clips (or everything between the marks), **Ctrl+Alt+V** pastes chosen attributes of
the copied clip onto the selected ones, **,** and **.** nudge clips a frame (Shift: ten),
**\\** fits the whole sequence in the timeline, File → Open an autosave…, and Settings for
hardware decoding, the frame and render cache sizes, audio scrubbing, timeline thumbnails and
viewer snapping.

How it works:
- The editor (`reelforge_engine::editor::Editor`) lives on the main thread; Tauri runs
  (non-async) commands there, so each
  command borrows it in turn. `tick` (called every animation frame) moves playback along
  and renders the picture; the UI then fetches the full state only when the project's
  revision changed, and the picture only when it changed.
- The viewer picture is read back from the GPU, shrunk to the viewer's size and sent as
  bytes: raw when paused (exact), JPEG while playing (small enough to keep up).
- Thumbnails decode on worker threads (`thumbnail` is an async command).
- On Windows the WebView2 loader is carried inside the exe (`src/webview2_shim.rs`), so it
  needs no DLL beside it. WebView2 itself comes with Windows 10 and 11.

Building it: `cd crates/tauri-app/ui && npm install && npm run build`, then
`cargo build --release -p reelforge-tauri` (the build embeds `ui/dist`; the program is
`target/release/reelforge`). For working on the interface, `npm run dev` serves it with live
reload and `cargo run -p reelforge-tauri --no-default-features` loads it from there.

## Reelforge's own decoders

`crates/codecs` holds video decoders written for Reelforge, in Rust. FFmpeg still reads
the files (MP4, MOV, MKV, AVI…), and its packets go to Reelforge's decoder when there is
one for the codec; the decoded pictures then go through the same scaling, colour and
caching as FFmpeg's. If a native decoder can't read a file, that file switches to FFmpeg's
decoder by itself. Settings → **Use Reelforge's own decoders** turns them off. For H.264, HEVC, VP9 and AV1,
hardware decoding (Settings → **Hardware video decoding**, on by default) is tried first when the
graphics card supports the codec, being faster and lighter than any CPU decoder; Reelforge's
decoders take those codecs when it's off or the card can't.

| Codec | Status |
|---|---|
| Motion JPEG | done (JPEG via the pure-Rust zune-jpeg; fields of interlaced MJPEG woven) |
| Apple ProRes 422 Proxy/LT/Standard/HQ, 4444, 4444 XQ (with alpha) | done, Reelforge's own (progressive and interlaced, 10-bit out, slices on all cores) |
| DNxHD / DNxHR | done, Reelforge's own: every compression ID (DNxHD 8/10-bit incl. interlaced, DNxHR LB/SQ/HQ/HQX/444, 4:4:4 as YCbCr or RGB); macroblock rows on all cores. 12-bit and adaptive-interlace (CID 1260) are implemented but untested: FFmpeg can't make samples of them, and they fall back to FFmpeg if they fail. The VC-3 code tables are the standard's values (as published in FFmpeg's `dnxhddata.c`), in `crates/codecs/src/dnxhd_tables.rs` |
| MPEG-2 video | done, Reelforge's own: Main and 4:2:2 profiles (4:2:0/4:2:2/4:4:4), I/P/B pictures, frame and field pictures, frame/field/16×8/dual-prime motion, field DCT, alternate scan, custom quantiser matrices, any size (incl. >2800 lines); slices on all cores. Covers DVD/broadcast/HDV/XDCAM/IMX in MPG, TS, VOB, MXF, MP4. MPEG-1 video is left to FFmpeg |
| H.264 | done, via a pure-Rust decoder library: Baseline/Main/High, CAVLC and CABAC, B-frames, weighted prediction, 8×8 transforms, multiple slices; progressive 8-bit 4:2:0. Interlaced, 10-bit and 4:2:2 H.264 (some camcorders and pro cameras) go to FFmpeg |
| HEVC (H.265) | done, via a pure-Rust decoder library: Main and Main 10 (4:2:0, 8/10-bit), tiles, WPP; 4:2:2/4:4:4 go to FFmpeg |
| VP9 | done, via a pure-Rust decoder library: profiles 0–3 (8/10/12-bit, 4:2:0 to 4:4:4) |
| AV1 | done, via a pure-Rust decoder library: Main profile, 8/10-bit 4:2:0 and monochrome, film grain |
| GoPro CineForm | opens through FFmpeg (422 YUV and 4444 RGB, with alpha); no Reelforge decoder yet |

The H.264, HEVC, VP9 and AV1 decoders live in this repository (`crates/h264`, `crates/hevc`,
`crates/vp9`, `crates/av1`, with `crates/bitstream` for reading bits and `crates/testkit` for
their tests), so building needs nothing beyond this repo. They're adapted from FilmCraft
(MIT OR Apache-2.0): see `THIRD-PARTY-NOTICES.txt`, which has to ship with the program and
any copy of the source. Each has its own tests: `cargo test -p reelforge-h264` (and `-hevc`,
`-vp9`, `-av1`).

Every decoder is tested against FFmpeg's output (`cargo test -p reelforge-media native`):
each frame of generated test clips is converted to RGBA both ways and must match closely
(ProRes: 65–73 dB PSNR in every profile; DNxHD/DNxHR: 64–81 dB; MPEG-2: 62–70 dB; i.e. off by
at most a rounding step of the IDCT; H.264, HEVC, VP9 and AV1 come out identical to FFmpeg's,
across 14 encoder configurations). FFmpeg's MPEG-2 encoder never makes field pictures,
16×8 or dual-prime motion, so the tests also generate random streams that use them
(`crates/codecs/src/mpeg2/testgen.rs`), and FFmpeg and Reelforge must decode them alike.
Speed: ProRes 422 HQ 1080p decodes at about 27 fps per core (FFmpeg's hand-written SIMD
manages about 120), so a typical 8-core PC plays it in real time with room to spare; DNxHR HQ is similar (about 23 fps per core). MPEG-2 1080i at 25 Mbit/s: about 75 fps on one core. The H.264/HEVC/VP9/AV1 decoders use all
cores; on 2 cores 1080p runs at about 75 fps (H.264, HEVC), 45 (VP9) and 28 (AV1).

## Building on Windows 10/11

You need three things: Rust, LLVM (for FFmpeg bindgen), and FFmpeg *shared* dev libraries.
Audio output uses WASAPI through `cpal`, which needs nothing extra on Windows.

### 1. Rust

Install from <https://rustup.rs> and pick the default **MSVC** toolchain. If prompted, install
the *Visual Studio Build Tools* with the "Desktop development with C++" workload.

### 2. LLVM / libclang

`ffmpeg-sys-next` generates bindings with bindgen, which needs `libclang.dll`.

```powershell
winget install LLVM.LLVM
# then (new terminal) make sure this is set:
setx LIBCLANG_PATH "C:\Program Files\LLVM\bin"
```

### 3. FFmpeg (shared build with headers and .lib files)

Download a **shared** build, e.g. from <https://github.com/BtbN/FFmpeg-Builds/releases> —
pick `ffmpeg-n7.1-latest-win64-gpl-shared-7.1.zip` (any 7.x or 8.x shared build works; the
`ffmpeg-next` crate supports FFmpeg up to 8.0). Extract it to, say, `C:\ffmpeg`, so that you
have `C:\ffmpeg\include`, `C:\ffmpeg\lib` and `C:\ffmpeg\bin`. Then:

```powershell
setx FFMPEG_DIR "C:\ffmpeg"
# The app needs the FFmpeg DLLs at runtime:
setx PATH "%PATH%;C:\ffmpeg\bin"
```

Open a **new** terminal so the variables take effect.

> Alternative: `vcpkg install ffmpeg:x64-windows` and set `VCPKG_ROOT` instead of `FFMPEG_DIR`.

### 4. Build and run

```powershell
cd reelforge
cd crates\tauri-app\ui; npm install; npm run build; cd ..\..\..
cargo run --release -p reelforge-tauri
```

Run the tests (the media tests generate a clip with the `ffmpeg` CLI from `C:\ffmpeg\bin`, and
skip themselves if it isn't on `PATH`):

```powershell
cargo test --workspace
```

### Shipping the app

Copy `target\release\reelforge.exe` together with the DLLs from `C:\ffmpeg\bin`
(`avcodec-*.dll`, `avformat-*.dll`, `avutil-*.dll`, `swscale-*.dll`, `swresample-*.dll`, …)
into one folder. Note that BtbN's `gpl` builds are GPL-licensed; use an `lgpl` build if you
want to distribute a closed-source app.

**Export and FFmpeg builds:** export prefers `libx264`, which is in the `gpl` builds. LGPL builds
don't include it, so Reelforge falls back, in order, to NVIDIA (`h264_nvenc`), Intel
(`h264_qsv`), AMD (`h264_amf`) or Windows Media Foundation (`h264_mf`) hardware encoders, and
finally MPEG-4 Part 2. The export queue says which encoder was used. H.265 needs `libx265`
(also `gpl`) or a GPU encoder; ProRes, DNxHR/DNxHD, CineForm and image sequences work with
every build.

### Cross-compiling the Windows build from Linux

The prebuilt `Reelforge-Windows-x64.zip` was made this way (mingw-w64, no Windows needed):

```sh
sudo apt install gcc-mingw-w64-x86-64 g++-mingw-w64-x86-64 clang
rustup target add x86_64-pc-windows-gnu
# FFmpeg: unzip a "win64-gpl-shared" build from https://github.com/BtbN/FFmpeg-Builds/releases
export FFMPEG_DIR=$PWD/ffmpeg-n8.1-latest-win64-gpl-shared-8.1
export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER=x86_64-w64-mingw32-gcc
# mingw-w64 lacks d3d12video.h, which FFmpeg's headers include; a one-line stub
# (`typedef struct ID3D12VideoDevice ID3D12VideoDevice;`) in ./stubs is enough for bindgen.
export BINDGEN_EXTRA_CLANG_ARGS_x86_64_pc_windows_gnu="--sysroot=/usr/x86_64-w64-mingw32 -I/usr/x86_64-w64-mingw32/include -I$PWD/stubs"
(cd crates/tauri-app/ui && npm install && npm run build)
cargo build --release -p reelforge-tauri --target x86_64-pc-windows-gnu
```

Ship `reelforge.exe` with the `bin/*.dll` files from the FFmpeg build next to it.

**The download.** The Windows zip holds `reelforge.exe`, the example plugins, PLUGINS.md and
**Get FFmpeg.bat**, which downloads the FFmpeg 8.1 shared build from BtbN and copies its DLLs
next to the program (the DLLs are ~90 MB, too big to include). The test
suites can be cross-built the same way (`cargo test --no-run --target …`) and run under Wine.

### Troubleshooting

| Symptom | Fix |
|---|---|
| `Unable to find libclang` | `LIBCLANG_PATH` isn't set, or the terminal predates `setx`. |
| `could not find native static library avcodec` / link errors | `FFMPEG_DIR` must point at the folder containing `lib\avcodec.lib`. Use a *shared* build, not "essentials" static exe-only zips. |
| App exits immediately with `STATUS_DLL_NOT_FOUND` (0xc0000135) | FFmpeg `bin` folder isn't on `PATH`; or copy its DLLs next to the exe. |
| Black viewer for a file | Check the status bar; the codec may be unsupported by your FFmpeg build. |
| Export fails with "no usable H.264/MPEG-4 video encoder" | Your FFmpeg build has no H.264 encoder that works on this PC. Use a `gpl` build (it includes libx264). |
| Status bar shows "🔇 no audio device" | No default output device was found at startup (e.g. headphones unplugged). Playback still works on the system clock; restart the app after connecting a device. |

## Keyboard shortcuts

| Key | Action |
|---|---|
| Space | Play / pause |
| ← / → | Previous / next frame |
| Home / End | Go to start / end |
| S | Split clip under playhead (selected clip first) |
| Del / Backspace | Delete selected clip |
| Ctrl+Z / Ctrl+Y | Undo / redo |
| Ctrl+I | Import media |
| Ctrl+S | Save project |
| Ctrl+M | Export |
| Ctrl+Shift+D | Add/remove audio crossfade at the nearest cut |
| Ctrl+click volume line | Add a volume keyframe |
| Double-click / Alt+click keyframe | Delete it |
| Alt while dragging a clip or edge | Don't snap |
| Drag a clip edge | Trim (ripple / roll with those tools) |
| V / B / N | Select / Ripple / Roll trim tool |
| Y / U | Slip / Slide tool (drag a clip) |
| I / O | Mark in / out at the playhead |
| Ctrl+T | Add a title at the playhead |
| J / K / L | Play backwards / stop / play forwards (again for 2×, 4×, 8×) |
| M | Add a marker at the playhead |
| Up / Down | Previous / next cut, marker or in/out point |
| Shift+F | Freeze frame at the playhead |
| Ctrl+G | Make composition (the selected clips, or everything between in and out) |
| Ctrl+click / Shift+click | Add a clip to the selection, or take it out |
| Drag on empty track space | Select clips with a box (Ctrl adds to the selection) |
| Ctrl+A / Esc | Select all / nothing |
| Ctrl+X / Ctrl+C / Ctrl+V | Cut / copy / paste clips (paste goes to the playhead) |
| Ctrl+Alt+V | Paste attributes of the copied clip onto the selected clips |
| , / . | Nudge the selected clips one frame earlier / later (Shift: ten) |
| \\ | Zoom the timeline to fit the sequence |
| Alt while dragging a layer in the Viewer | Don't snap |
| Enter / Esc while drawing a mask | Close the mask / cancel |
| Enter | Render preview (in to out, or the whole sequence); Esc stops it |
| Double-click a composition | Open it |
| Right-click a media item | New composition from media, add, proxy, show in folder |
| Alt+X | Clear in/out |
| Shift+Del | Ripple delete (close the gap) |
| Ctrl+D | Add/remove cross-dissolve at the nearest video cut |
| E | Effects & transitions browser |
| Ctrl + mouse wheel | Zoom timeline |
| Click / drag in Viewer | Select / move a layer |
| Drag a corner / edge handle | Scale / stretch (Shift: free / both ways) |
| Drag the stem or just outside a corner | Rotate (Shift: 15° steps) |
| Ctrl + drag an edge handle / the cross | Crop that edge / move the anchor point |

## How the pieces fit

- **Time** is stored as integer ticks (705,600,000 per second), which divides exactly by every
  common frame rate (23.976 … 120) and audio rate (44.1k, 48k). No float drift.
- **Every edit is a `Command`** (`crates/core/src/undo.rs`) with `apply`/`undo`. Add new edit
  types there and undo comes for free. `UndoStack::entries()` lists done and undone steps for
  the History panel, and `go_to(n)` undoes or redoes until `n` steps are in effect.
- **Decoding**: `DecoderHandle` runs FFmpeg on its own thread and pushes RGBA frames into a
  bounded channel, so it stays a few frames ahead and blocks when the UI stops reading.
  Seeks carry a generation number so stale frames get dropped. `FrameSource` turns this into
  random access: the UI calls `frame_at(t)` every repaint and never blocks.
- **Compositor** (`crates/render`): each layer's decoded frame is uploaded as an
  `Rgba8UnormSrgb` texture (cached, re-uploaded only when the frame changes), drawn as a
  transformed quad into an `Rgba16Float` target with a fixed-function blend state per blend
  mode (premultiplied alpha), then encoded to sRGB. So a 50% fade between red and blue is
  computed in linear light, the way HitFilm and other compositors do it. The viewer picture
  is read back from the GPU and sent to the interface (raw when paused, JPEG while playing).
  The same transform math (`crates/core/src/transform.rs`) drives the GPU and the on-canvas
  gizmo, so they always agree.
- **Decoders are per clip** (`engine/src/preview.rs`), so two clips of the same file at different
  times don't fight over one decoder. While playing, decoders for cuts in the next 1.5 s are
  opened early; idle ones close after 4 s.
- **Export** (`crates/export`): for each output frame it asks every visible clip's decoder
  for the exact frame (blocking; decoders run ahead on their own threads), composites at the
  sequence size on the GPU, reads the pixels back, and hands them to `MovieWriter`, which
  converts RGB to BT.709 YUV and encodes. Audio is mixed in step with the video, exactly the
  samples belonging to each frame, so the two can't drift. Encoders use closed GOPs with a
  keyframe every ~2 s, so exported files scrub well when re-imported. A cancelled or failed
  export deletes its partial file.
- **Audio levels** (`crates/core/src/audio.rs`): volume is stored in dB (−60 dB = silent);
  pan is a balance control, so centre leaves a stereo source untouched and turning one way
  only turns the other side down. Fades use an equal-power (quarter-sine) curve. Clip and
  track settings are folded into one gain + per-channel balance per clip by
  `reelforge_export::mix_clips`, which feeds both playback and export, so they can't
  disagree. Mono sources play at full level on both channels.
- **Keyframes** (`crates/core/src/anim.rs`): one curve per property, keys anchored to source
  time. `Project::video_layers_at(seq, t)` evaluates everything for a moment in time (which
  clips are visible, their keyframed transforms, and any dissolve in progress with its mix
  amount), and both the viewer and export render from that, so they always agree.
- **Dissolves** render as a group in the compositor: both clips are summed, weighted 1−mix and
  mix, into a scratch float target, which is then composited over the frame. That's exactly
  `(1−u)·A + u·B` over whatever is underneath. A dip clears the group to the dip colour and
  fades each clip in turn against it. Wipes and the iris draw the incoming clip with a soft
  edge computed in frame space in the layer shader; slides and pushes are just offsets added to
  each clip's transform (`core/src/transition.rs`), so they work for any layer.
- **Trimming** (`undo::trim_range` / `trim_group`) computes the allowed range for the whole
  linked group up front, so the edge simply stops at a limit instead of failing.
- **Ripple and roll** (`core/src/edit.rs`): `plan_trim` turns a drag into a `TrimPlan`: the
  clamped delta plus the `(before, after)` timing of every clip that changes. A ripple's lower
  bound comes from `shift_limit`, which finds, for each track, how far its later clips can slide
  back before they hit something fixed. While you drag, the app keeps a snapshot of every clip's
  timing and re-plans from it each frame, so the result depends only on where the pointer is
  now. On release the changes become a single batch of `SetClipTiming` commands.
- **Slip and slide** (`plan_slip` / `plan_slide` in the same file) return the same kind of plan,
  so the app drives every tool through one live-edit path. Locked tracks are handled there too:
  `Sequence::is_locked` treats a whole link group as locked, planners refuse to touch locked
  clips, and `shift_limit` counts them as fixed when working out how far a ripple can go.
- **Anchor and crop** (`core/src/transform.rs`): `Transform::matrix` puts the anchor at the
  frame centre plus `position` and scales/rotates around it, so the compositor, the viewer gizmo,
  masks and the tracker all agree. Crop is stored as fractions of the layer per side; the
  compositor draws a smaller quad sampling just that part of the texture (`crop_uv`), so it
  costs nothing. `with_anchor_kept_in_place` moves the pivot without moving the picture.
- **The gizmo** (`engine/src/gizmo.rs`, driven by `tauri-app/src/viewer.rs`) computes every
  drag from the transform at the start of the drag and the pointer now (scale as the ratio of
  the pointer's offsets from the anchor in the layer's rotated axes; rotation accumulated move
  to move so it can go past 180°), in frame pixels with the viewer's zoom passed in for the grab
  distances, and writes through the same `set_transform_live` as the Controls panel.
- **Thumbnails** (the `thumbnail` command): decoded off the main thread with a small pool of
  open `FrameSource`s, sent as JPEG, and cached by the interface.
- **Effects** (`core/src/effects.rs`, `render/src/effects.wgsl`): an `Effect` is a kind plus
  parameters, each a static value or a keyframe `Curve`. `Project::video_layers_at` evaluates
  them at the frame's source time into `EffectValues`. The compositor runs each layer that has
  effects through full-screen passes at the layer's own resolution: an import pass
  (sRGB → premultiplied linear float), then one or more passes per effect, between three float
  textures kept per layer (glow is bright-pass → blur → add; drop shadow is shadow → blur →
  under). Two-input passes (masks, curves, LUTs, glow, shadow) read a second texture: the
  original, or a lookup table. Curves become a 256×1 table; a .cube LUT of size N becomes N
  slices of N×N laid side by side, sampled with the blue axis interpolated by hand. Tables are
  uploaded once and cached by content hash. The result is drawn with the pre-multiplied pipeline, including
  inside dissolve groups. The blur radius is converted from frame pixels to texels, so a proxy-
  sized preview and a full-size export blur by the same visible amount.
- **Proxies, hardware decoding and the frame cache** live in the media crate. `hw.rs` sets a
  shared hardware device on each decoder context before it opens (`hw_device_ctx` +
  `get_format`) and copies frames back with `av_hwframe_transfer_data`; any failure reopens
  the decoder in software and carries on from the same frame. `cache.rs` is a byte-budgeted
  LRU of decoded frames keyed by (file, decode size, pts) that every preview decoder fills and
  reads. `proxy.rs` writes all-intra H.264 at ≤960×540, sampling the source once per nominal
  frame so proxies line up with variable-frame-rate originals.
- **Safety** (`engine/src/safety.rs`): every save goes through `write_atomic` (write, fsync,
  rename). Each running app holds an OS file lock on its session file; at start-up, a session
  file nobody holds a lock on means that run crashed, and its autosave is offered back.
- **Titles** (`core/src/title.rs`, `render/src/text.rs`): a title is a clip with a
  `TitleSpec` and no media (`MediaId::GENERATED`, treated like a still). The text is laid out
  with `ab_glyph` and rasterised on the CPU onto a transparent canvas the size of the frame:
  the fill coverage, an outline from a Euclidean distance transform of it (round at any
  width), a shadow made from the silhouette offset with bilinear sampling and softened by three
  box-blur passes (≈ a Gaussian), and the box, composited in that order. The viewer and export
  cache the image by a hash of the spec, so it's only redrawn when the title changes, and from
  there it's just another layer to the GPU compositor. Installed fonts are found once, in the
  background, by reading each font file's name table.
- **Tracks** are added and removed with `AddTrack` / `RemoveTrack` commands (the removed track
  and its clips are kept for undo). Deleting a track is refused if it would orphan linked clips
  on another track, or leave the sequence without a video or audio track.
- **Compositions** (`Clip::nested`, `export/src/scene.rs`): a clip whose `nested` names a
  sequence index (its media is `MediaId::NESTED`; its source time is that sequence's time).
  `Scene` turns a sequence at a time into compositor draws for both the viewer and export, via
  a `FrameProvider` (the viewer's gives whatever preview frame is ready; export's waits for
  exact full-size frames). For a composition layer it renders the inner sequence with its own
  `Compositor` in transparent mode (cleared to transparent, written as straight alpha), reads it
  back, and uses it as the layer's picture; unchanged inner frames are reused by signature.
  `mix_clips` flattens a shot's audio into the outer mix, shifted and cut to the clip.
  `undo::make_composite` builds the "Make composition" command (whole-sequence snapshots for
  undo); `Project::contains_sequence` stops a shot going inside itself
- **Speed** (`core/src/speed.rs`): clip time (`Clip::source_time`, what keyframes, effects,
  masks and volume keys use) always runs at the timeline's pace; `Clip::media_time` maps it to the
  media: `anchor + rate × clip time` (rate = speed, negative reversed, 0 frozen). Normal clips have
  rate 1 and anchor 0, so nothing changed for them, and splitting, trimming and slipping need no
  special cases: they move the clip-time window and the mapping carries on. `change_speed` keeps
  the clip's media span and recomputes length and anchor; trim limits come from
  `Clip::clip_time_limits`. The mixer plays a clip at `rate` by reading the media span a block
  covers and resampling it linearly (backwards for negative rates)
- **Shuttle and markers**: `Playback::shuttle` runs the playhead from the system clock at ±1–8×
  (the audio clock only drives normal playback). Markers live on the sequence (`Sequence::markers`)
  and are edited with `SetMarkers`, live while dragging or typing
- **Selection and clipboard** (`undo::move_targets` / `move_clips` / `delete_clips`,
  `core/src/clipboard.rs`): a selection is a list of clip ids, always widened to their link
  groups. Moving plans every clip's new track and time first (the timeline draws that plan as
  ghosts), shifting selected clips by the same number of tracks of their kind. The clipboard
  stores clips with times relative to the earliest one and tracks as "n-th video/audio track";
  pasting simulates the paste on a copy of the project to add tracks and find the first time
  where everything fits, then makes it one undoable batch with fresh ids and links
- **Masks** (`core/src/mask.rs`, `render/src/mask.wgsl`): a mask is points in the layer's own
  pixels (four corners for a rectangle or ellipse, any number for freehand) plus optional shape
  keys. `rasterize` fills each outline (even-odd scanlines), computes the distance to the outline
  only in a narrow band around it, turns that into alpha with the feather and expansion, and
  combines the masks (add = screen, subtract, intersect), at up to 2048 px. `Scene` caches the
  image per clip by a hash of the shapes, and the compositor applies it in the layer's own pixel
  space as the first pass of its effect chain, so the viewer and export match
- **Tracking** (`core/src/track.rs`, `media/src/tracker.rs`): a `Tracker` has a `TrackMode` and
  1, 2 or 4 `PointTrack`s (projects saved with the old single point still load).
  `Template::find` scores every offset in the search area by Gaussian-weighted normalised
  cross-correlation on luma and fits a parabola through the best score's neighbours for
  sub-pixel accuracy; a template remembers the sub-pixel offset of its centre, so refreshing it
  doesn't drift. `FeatureTracker` searches large areas on a box-filtered `Pyramid`
  (coarse-to-fine, from a velocity prediction), refreshes its patch after confident matches,
  and snaps back to the original patch when it still fits. `track_points` decodes each frame
  once for all points, then (with 2 or 4) solves the group's motion and re-finds each point
  with its original patch warped by that motion's local linear map (`Template::warped`), which
  stops errors from building up. `motion_between` gives a shift, a similarity or a
  homography; `follow_keys` composes it with the target's transform (or, stabilising, applies
  its inverse, optionally against a Gaussian-smoothed camera path) and writes position /
  rotation / scale keys, or corner-pin keys
- **Plugins** (`core/src/plugin.rs`, `render/src/plugin.rs`, `engine/src/plugins.rs`): the
  manifest is parsed into a `PluginDef` (parameter specs interned as `&'static`, like the
  built-in effects' tables, so reloading the same manifest leaks nothing) and published in a
  global registry that `Effect` (kind `Plugin` plus a `PluginRef` id) looks up for its
  parameters and label. The render crate wraps the plugin's WGSL between a prelude (bindings,
  `sample`/`param`/`time`… helpers, all sampling with `textureSampleLevel` so there are no
  uniformity restrictions) and one generated `@fragment` wrapper per pass, and checks it with
  naga before wgpu sees it, mapping error spans back to the plugin's own lines. The compositor
  compiles each plugin revision once (inside an error scope) and runs its passes in the
  layer's effect chain using the two-texture pipeline layout: each pass reads the previous
  result and `original` reads the effect's input, alternating between the layer's other two
  scratch targets. The effect uniform grew to 128 bytes (params 8–15, time, layer size, pass
  index); built-in shaders only declare the first 64. A plugin's revision and the clip time
  are part of the frame signature, so reloads and animated plugins re-render. The app's
  manager fingerprints plugin files once a second for live reload.
- **Collect** (`core/src/collect.rs`, `engine/src/collect.rs`): `Project::for_each_path_mut`
  visits every file reference (media, LUT effect data, title fonts), so planning, relative
  saving and resolving can't miss one. `plan_collect` picks clash-free names (compared without
  case, as Windows would) and reuses an existing file only if it has the same size and date.
  `run_collect` copies on a background thread in 4 MB chunks to a `.part` file renamed when
  complete (a stopped or failed copy never leaves a half file under the real name), keeps
  each file's date, then writes the project with `paths_relative_to`. Relative paths are
  written with `/` and `resolve_paths` accepts either separator, so projects move between
  systems; `load_project` resolves them against the project file's folder.
- **Paste attributes** (`core/src/attrs.rs`): `paste_attributes` takes the first copied video
  and audio clip as the source and builds one `Batch` of `SetClipVisual` / `SetClipAudio` /
  `SetTitle` commands. Keys move by the difference of the two clips' `source_in` (keys live in
  each clip's own time); mask points scale by the ratio of picture sizes. **Nudge** is
  `nudge_clips` in `undo.rs`, which reuses `move_targets`, so links, locks and overlaps behave
  exactly as when dragging.
- **Viewer snapping** (`engine/src/snap.rs`): a drag is computed from the layer's transform when
  it began plus the pointer's total movement (so snapping never accumulates); the moving
  layer's bounding box is snapped on each axis to the nearest of the frame's and other
  layers' edges and centres within 8 screen pixels.
- **Corner pin** (`core/src/transform.rs`): `Transform::pin` holds where the layer's corners go;
  `Transform::layer_map` gives an affine map or, when pinned, a `Homography` (solved from the
  four corners). Its clip-space matrix carries the perspective in w, so the compositor's
  unchanged vertex shader draws the quad and the GPU interpolates texture coordinates in
  perspective. Hit testing, the gizmo, masks and the tracker overlay all go through
  `layer_map`, so they agree with what's drawn. Pin keys are a `Curve<Pin>` like any property
- **Curves** (`core/src/channels.rs`, `ui/src/components/CurvesPanel.svelte`): `Channel` addresses any keyframeable
  number on a clip (a transform component or an effect parameter), with uniform get/set/move
  operations, so the curve editor doesn't care what it's editing. A drag re-applies the move
  to a copy of the clip taken when it started, which keeps keys in order and makes the whole
  drag one undo step.
- **Volume automation** is stored as `(source time, dB)` keys and interpolated linearly in dB
  (what sounds like an even ramp). `mix_clips` converts keys to sample offsets per clip, so the
  mixer applies them sample-accurately in both playback and export.
- **Crossfades** (`Project::crossfades`) are resolved from each clip's spare media: the overlap
  is centred on the cut when both sides have enough, otherwise it leans on whichever side does.
  `mix_clips` then extends the two clips into that overlap and gives them matching equal-power
  ramps, which keeps the loudness constant across the transition for different material.
- **Meters** read peak and RMS from the audio callback itself (lock-free: per-channel atomic
  float maximums that the UI swaps out each frame), so they show exactly what reaches the
  device, including scrubbing. Track meters: each `MixClip` knows its track; the mixer records
  every track's peak per block (after volume and pan), the block carries them to the callback,
  and the callback publishes them the same lock-free way when the block starts playing, so the
  track meters move with what's heard. Pan keys reach the mixer as `(offset, pan)` pairs like
  volume keys and are applied per sample (`balance(interpolate(...))`), with the track's pan in
  the clip's static balance.
- **YUV frames** (`core/src/frame.rs`, `render/src/yuv.rs`): the decoder keeps I420, NV12,
  yuv420p10 and P010 as tightly packed planes (`Pixels::Yuv`), shrinking them YUV-to-YUV when a
  preview size is wanted; anything else is converted to RGBA as before. The compositor uploads
  the planes as unsigned-integer textures and one pass converts them into the layer texture with
  the frame's matrix (BT.601/709/2020) and range, interpolating the 4:2:0 chroma with MPEG-2
  siting. `YuvPlanes::to_rgba` is the CPU version (thumbnails, proxies) and the reference the
  GPU is tested against (within 2/255 for every layout).
- **Render cache** (`engine/src/render_cache.rs`): frames are keyed by (sequence, frame) and
  stored with a *recipe*, `reelforge_export::frame_recipe`: a hash of everything that decides
  the picture, computed from the project without decoding (media file and frame index, title
  look, compositions recursively, evaluated transforms, effects, masks, transitions, proxy or
  original). A kept frame is used only while its recipe matches, so invalidation is automatic
  and exact. Rendering uses the preview's decoders in blocking mode with its own compositor,
  ~40 ms of work per UI frame; frames above 1080p are box-filtered down. The ruler bar is
  worked out per 2 px column and only again when the edit, cache or view changes.
- **Deep export** (`Compositor::read_output_deep`, `output.wgsl`): for 10-bit, HDR and
  ProRes, the float accumulation target goes through a last pass that applies sRGB, or BT.709 →
  BT.2020 plus PQ (×203/10000) or HLG (×0.265, i.e. 75% signal at SDR white), into an
  `Rgba16Float` target that's read back and turned into 16-bit integers; the writer feeds them
  to swscale as RGBA64 with the BT.709 or BT.2020 matrix and tags the stream (and x265 gets the
  HDR10 SEI).
- **Scrubbing**: while paused, every playhead move asks the engine for a 60 ms snippet with
  4 ms de-click ramps. The mixer only renders the next snippet when the device is about to run
  dry, always at the newest position, so fast drags stay responsive.
- **Audio engine** (`engine/src/audio.rs`): a mixer thread renders the timeline into 1024-frame
  blocks with `Mixer` and queues ~130 ms ahead; the cpal callback (realtime: no locks, no
  decoding) copies blocks to the device and publishes which timeline sample is reaching the
  speakers, corrected for output latency. Play/stop bump a generation number so stale blocks
  are dropped instantly on seek.
- **Playback clock** (`engine/src/playback.rs`) reads that position, so video is slaved to audio.
  With no device, or if audio fails to start within 500 ms, it falls back to wall-clock time.
  The status bar shows which clock is in use.

## Roadmap

1. ~~Player and editor shell~~
2. ~~Audio playback, mixing, A/V sync, linked clips, waveforms, volume/pan, fades, scrubbing,
   volume keyframes, crossfades, meters, track meters, pan keyframes~~
   (still to add: audio effects like EQ/compressor)
3. ~~Compositor and editing: layers, transforms, opacity, blend modes, viewer gizmo, keyframes
   and a curve editor, cross-dissolves, trimming, ripple/roll/slip/slide, ripple delete, track
   locking~~
   (still to add: more blend modes; Bézier keyframe handles are in)
4. ~~Export: H.264/H.265/ProRes/DNxHR/image sequences/WAV, presets including vertical video,
   fit/fill/stretch, GPU encoders, an in/out range, an export queue, 10-bit/HDR10/HLG HEVC,
   target bitrates, your own presets~~
   (still to add: HDR footage in (it's decoded as SDR today), 10-bit/HDR for GPU encoders'
   metadata)
5. ~~**Effects**: the effect stack, keyframable parameters, colour correction, levels, curves,
   LUTs, blur, sharpen, chroma key, glow, drop shadow, vignette, an effects browser;
   dissolve, dip, wipe, iris, slide and push transitions~~
   (still to add: letting glow/shadow spill outside the layer, more stylize effects, saved
   effect presets)
6. **Compositing**: ~~compositions (nested sequences), motion tracking (1, 2 and 4
   points: follow, stabilise with smoothing, corner pin / screen replacement), corner pin,
   masks with Bézier curves and per-point feather~~, titles and track management are done.
   Still to come: automatic
   zoom to hide stabilisation edges, GPU particles, animated text presets (3D layers, a camera
   and lights are in).
7. ~~**Performance**: proxy media, a frame cache, hardware decode, YUV frames converted on the
   GPU, render preview~~
   (zero-copy from the hardware decoder is in for Windows, D3D11VA to Direct3D 12, but is
   still to be tried on real hardware; VAAPI to Vulkan on Linux; render cache on disk)
