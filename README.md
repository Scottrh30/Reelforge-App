# Reelforge

A HitFilm-style video editor and compositor, written in Rust. Editing, audio, the GPU
compositor, export (with presets, a queue and GPU encoders), proxies and crash safety are done;
effects and compositing (composite shots, tracking) are under way.

What works now:

- Dockable panel layout (Media, Viewer, Controls, Editor Timeline, Curves, Meters) — drag tabs to rearrange
- Import video/image/audio via **File → Import**, `Ctrl+I`, or drag files from Explorer
- FFmpeg decoding on a background thread, with frame-accurate seeking
- Viewer with transport bar and timecode
- Multi-track timeline: drag media from the bin onto a track, move clips between tracks,
  razor split (`S`), delete (`Del`), scrub, zoom (`Ctrl` + scroll), pan (scroll)
- Undo/redo for every edit (`Ctrl+Z`, `Ctrl+Y` / `Ctrl+Shift+Z`)
- Save/open projects (`.rfproj`, JSON)
- **Audio playback** through your default output device, mixed from all audio tracks
  (any format FFmpeg decodes, resampled to the device rate)
- **A/V sync**: the audio device is the master clock; video follows what's reaching the speakers
- **Linked clips**: footage with sound lands as a video clip + audio clip that move, split and
  delete together. **Edit → Unlink audio/video** separates them
- Audio waveforms on clips; per-track mute (**M**) and show/hide (eye) buttons

- **GPU compositor**: every visible video track is blended into the frame on the GPU
  (wgpu), in linear light with float precision
- **Per-clip transform** in the Controls panel: position, scale (with a lock for uniform
  scaling), rotation, opacity, and **blend modes** (Normal, Add, Multiply, Screen), plus
  Reset / Fit / Fill buttons. Clips sit at their native pixel size by default, like HitFilm
- **On-canvas editing**: click a layer in the Viewer to select it, drag to move it. Each
  gesture (a drag, or a slider move) is one undo step

- **Export** (**File → Export…**, `Ctrl+M`): renders the timeline (or the in/out range) in the
  background; you can keep editing meanwhile. Export waits for every exact frame at full
  resolution and uses the same compositor and mixer as playback, so the file matches what you
  saw and heard
  - **Presets**: YouTube 1080p, YouTube 4K (H.265), Shorts/Reels/TikTok 9:16 (1080×1920),
    Instagram portrait 4:5, Square 1:1, ProRes 422 HQ and DNxHR HQ masters, a quick half-size
    draft on the GPU, PNG/JPEG image sequences and audio-only WAV. Changing any setting turns
    the preset into "Custom"
  - **Formats**: H.264 or H.265 with AAC (MP4); ProRes 422 / 422 HQ and DNxHR SQ / HQ with
    24-bit PCM audio (MOV); PNG, JPEG or TIFF sequences (`name_00001.png`, …); WAV. The file
    extension follows the format
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
- **Audio crossfades**: **Edit → Audio crossfade at playhead** (`Ctrl+Shift+D`) adds a 1 s
  equal-power crossfade at the nearest cut between two touching audio clips (press again to
  remove); its length is editable in the Controls panel ("Crossfade in"). A crossfade plays the
  outgoing clip past its end and the incoming one before its start, so it needs unused media
  either side of the cut; if there isn't enough it's shortened (and the panel says so), and with
  none at all it stays a plain cut (red marker on the timeline)
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
  - The **browser** (`E`, or **Edit → Effects & transitions…**) lists every effect by category
    (Colour, Blur & sharpen, Keying, Light & stylize) and every transition, with a search box
    and a one-line description of each; **Add** puts an effect on the selected clip
  - Turn each effect on or off, reorder (applied top to bottom) or remove it. Every parameter
    has a keyframe button and works like the transform properties. The viewer and export use
    the same GPU passes, so exports match what you see
- **Tracks**: as many video and audio tracks as you need. **+ Video** / **+ Audio** under the
  track headers (or **Edit → Tracks**) add one; new video tracks go on top. Right-click a
  header to rename, add or delete a track (deleting takes its clips with it, and can be undone);
  double-click a track's name to rename it. When the tracks don't fit, the mouse wheel scrolls
  through them (Shift+wheel moves along the timeline)
- **Text titles** (**Edit → Add title at playhead**, `Ctrl+T`, or **New title** in the Media
  panel): a 5 s title on the top free video track (a new track is made if none has room).
  In the Controls panel: multi-line text, any installed font (with a search box), size, colour,
  alignment, line and letter spacing, outline, soft drop shadow (distance, angle, softness,
  colour) and a background box for lower thirds, plus one-click **Centre / Lower third / Top**
  placement. Titles are ordinary layers, so the transform, keyframes, effects, dissolves and
  blend modes all work on them; click the text in the Viewer to select and drag it. Titles are
  purple on the timeline and can be any length
- **Proxies**: heavy footage (above 1080p, or 720p and up in HEVC/AV1/VP9/ProRes/DNxHD) gets a
  small editing copy automatically, made in the background, with a progress bar in the Media
  panel; **Make proxy** does it for any clip. Proxies are 960×540 with every frame a keyframe,
  so jumping around is instant (8 s of 4K: ~1.5 s per random seek on the original, 3 ms on the
  proxy). The **Proxies on/off** button in the viewer switches between proxies and originals;
  export always uses the originals. Proxies are kept per file in your cache folder, so other
  projects using the same footage reuse them
- **Hardware decoding**: video is decoded on the graphics card where possible (D3D11VA on
  Windows, VAAPI on Linux, VideoToolbox on macOS), falling back to software automatically if
  the GPU can't handle a file
- **Frame cache**: decoded frames are kept in memory (1 GB by default), shared between clips
  of the same file, so scrubbing back over footage you've already seen is instant
- **Autosave and crash recovery**: the project is autosaved every 2 minutes (when it has
  changed). If Reelforge doesn't close properly, it offers to recover that work the next time
  it starts; **File > Open an autosave…** opens older ones. Saving always writes a new file and
  then swaps it in, so a crash mid-save can't corrupt the project. Closing, starting a new
  project or opening another asks **Save changes?** if there's unsaved work
- **Missing media**: files that have moved show a red banner in the Media panel and a
  "MEDIA OFFLINE" frame in the viewer. **Relink…** a file and Reelforge also looks for the
  other missing files in that folder (and its subfolders) by name; **Search a folder…** does
  it for all of them. Relinking can be undone. Export won't start while media is missing
- **Settings** (**Edit > Settings…**): hardware decoding, frame cache size, proxies, autosave
  interval, and the proxy/autosave folders
- **Speed** (Controls panel, any footage or composite-shot clip): 1–2000%, with one-click 25/50/
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
  linked to each other, not to the originals). Paste into a composite shot works too: missing
  tracks are added
- **Masks** (Controls panel, any video clip, title or composite shot): **+ Rectangle**,
  **+ Ellipse**, or **+ Freehand** (click points in the Viewer; click the first point or press
  Enter to close, Esc to cancel). Drag a mask's corner handles in the Viewer to reshape it (a
  rectangle or ellipse can be pulled into any four-sided shape), or select it in the list and
  drag inside it to move it
  - Several masks combine in order: **Add**, **Subtract** (cut a hole) or **Intersect**; each
    can be inverted and has a **feather** (soft edge), an **expansion** (grow/shrink) and an
    opacity
  - **Animate shape**: once ticked, reshaping the mask at the playhead sets a keyframe there
  - Masks are applied before the clip's effects and move with the layer's transform
  - With a tracker on the clip, **Apply to → Mask: …** makes the mask follow the tracked point
- **Composite shots**: a composite shot is a sequence of its own, with its own layers, that
  sits on the timeline as one clip (amber, marked `[C]`). Transform it, add effects, dissolve
  it, trim it like any clip; its audio plays and exports with it
  - **Make composite shot** (`Ctrl+G`, the Edit menu, or **Make from selection** in the Media
    panel) moves the selected clip and its linked audio into a new composite shot, or every clip
    between the in and out marks if you've set them. Undo puts them back
  - **Double-click** a composite-shot clip (or **Open** in the Controls or Media panel) to go
    inside and edit it; tabs above the timeline switch between the main edit and each shot, and
    each remembers its playhead. **+ New** in the Media panel starts an empty one, and **Add at
    playhead** puts a shot into whatever you're editing (shots can contain shots; a shot can't
    contain itself)
  - Empty parts of a shot are transparent, so layers underneath show through
  - Exporting while inside a shot exports that shot
- **Motion tracking** (Controls panel, for footage clips): **Add tracker**, drag its box in the
  Viewer onto a sharp detail, then **Track forward** / **Track backward** from the playhead. It
  follows the detail frame by frame to a fraction of a pixel (normalised cross-correlation, so
  lighting changes don't throw it) and stops where it loses it, so you can re-place it and carry
  on. The path is drawn in the Viewer. **Apply to** another layer makes that layer follow the
  point (a title stuck to a moving car, say); **This clip: stabilise** moves the clip against
  the motion so the point holds still. Both write position keyframes you can edit or undo
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
- **Video keyframes**: Position, Scale, Rotation and Opacity each have a keyframe button in the
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
full-frame clip is hidden), Bézier handles in the curve editor, keyframed audio pan, animated text
(per-letter reveals), and reordering tracks by dragging.
See the roadmap at the bottom.

## Layout

```
crates/
  core/    Engine-agnostic project model: rational Time, Sequence/Track/Clip, undo commands.
           No FFmpeg, no GPU, fully unit-tested.
  media/   FFmpeg probing, MovieWriter (H.264/AAC encoder + muxer), threaded video decoder + FrameSource ("give me the frame at time t"),
           AudioReader (random-access resampled audio), Mixer (renders the timeline's audio).
  render/  wgpu compositor: transformed layers, blend modes, linear-light float pipeline,
           pixel readback. No egui dependency, so export can use it headless.
  export/  Scene building shared by viewer and export (incl. composite shots), the audio mix
           list, and offline render: exact frames -> compositor -> MovieWriter, with
           progress/cancel. Runs headless (tests use it without a window).
  app/     eframe/egui desktop app (wgpu renderer), egui_dock panels, timeline, viewer,
           cpal audio engine.
```

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
cargo run --release -p reelforge
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
(also `gpl`) or a GPU encoder; ProRes, DNxHR and image sequences work with every build.

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
cargo build --release -p reelforge --target x86_64-pc-windows-gnu
```

Ship `reelforge.exe` with the `bin/*.dll` files from the FFmpeg build next to it. The test
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
| Ctrl+G | Make composite shot (the selected clips, or everything between in and out) |
| Ctrl+click / Shift+click | Add a clip to the selection, or take it out |
| Drag on empty track space | Select clips with a box (Ctrl adds to the selection) |
| Ctrl+A / Esc | Select all / nothing |
| Ctrl+X / Ctrl+C / Ctrl+V | Cut / copy / paste clips (paste goes to the playhead) |
| Enter / Esc while drawing a mask | Close the mask / cancel |
| Double-click a composite shot | Open it |
| Alt+X | Clear in/out |
| Shift+Del | Ripple delete (close the gap) |
| Ctrl+D | Add/remove cross-dissolve at the nearest video cut |
| E | Effects & transitions browser |
| Ctrl + mouse wheel | Zoom timeline |
| Click / drag in Viewer | Select / move a layer |

## How the pieces fit

- **Time** is stored as integer ticks (705,600,000 per second), which divides exactly by every
  common frame rate (23.976 … 120) and audio rate (44.1k, 48k). No float drift.
- **Every edit is a `Command`** (`crates/core/src/undo.rs`) with `apply`/`undo`. Add new edit
  types there and undo comes for free.
- **Decoding**: `DecoderHandle` runs FFmpeg on its own thread and pushes RGBA frames into a
  bounded channel, so it stays a few frames ahead and blocks when the UI stops reading.
  Seeks carry a generation number so stale frames get dropped. `FrameSource` turns this into
  random access: the UI calls `frame_at(t)` every repaint and never blocks.
- **Compositor** (`crates/render`): each layer's decoded frame is uploaded as an
  `Rgba8UnormSrgb` texture (cached, re-uploaded only when the frame changes), drawn as a
  transformed quad into an `Rgba16Float` target with a fixed-function blend state per blend
  mode (premultiplied alpha), then encoded to sRGB. So a 50% fade between red and blue is
  computed in linear light, the way HitFilm and other compositors do it. The app shares
  eframe's wgpu device and shows the output texture directly in the Viewer, with no CPU copy.
  The same transform math (`crates/core/src/transform.rs`) drives the GPU and the on-canvas
  gizmo, so they always agree.
- **Decoders are per clip** (`app/src/preview.rs`), so two clips of the same file at different
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
- **Safety** (`app/src/safety.rs`): every save goes through `write_atomic` (write, fsync,
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
- **Composite shots** (`Clip::nested`, `export/src/scene.rs`): a clip whose `nested` names a
  sequence index (its media is `MediaId::NESTED`; its source time is that sequence's time).
  `Scene` turns a sequence at a time into compositor draws for both the viewer and export, via
  a `FrameProvider` (the viewer's gives whatever preview frame is ready; export's waits for
  exact full-size frames). For a composite-shot layer it renders the inner sequence with its own
  `Compositor` in transparent mode (cleared to transparent, written as straight alpha), reads it
  back, and uses it as the layer's picture; unchanged inner frames are reused by signature.
  `mix_clips` flattens a shot's audio into the outer mix, shifted and cut to the clip.
  `undo::make_composite` builds the "Make composite shot" command (whole-sequence snapshots for
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
  where everything fits, then makes it one undoable batch with fresh ids and links. egui only
  reports Ctrl+V when the system clipboard holds text, so copying leaves a short note there
- **Masks** (`core/src/mask.rs`, `render/src/mask.wgsl`): a mask is points in the layer's own
  pixels (four corners for a rectangle or ellipse, any number for freehand) plus optional shape
  keys. `rasterize` fills each outline (even-odd scanlines), computes the distance to the outline
  only in a narrow band around it, turns that into alpha with the feather and expansion, and
  combines the masks (add = screen, subtract, intersect), at up to 2048 px. `Scene` caches the
  image per clip by a hash of the shapes, and the compositor applies it in the layer's own pixel
  space as the first pass of its effect chain, so the viewer and export match
- **Tracking** (`core/src/track.rs`, `media/src/tracker.rs`): `Template::find` scores every
  offset in the search area by normalised cross-correlation on luma and fits a parabola through
  the best score's neighbours for sub-pixel accuracy. `track_point` steps through the clip's
  frames (exact, full size), refreshing the template after confident matches and stopping below
  a 0.6 score. `follow_keys` turns the path into position keys, taking the tracked layer's own
  transform into account
- **Curves** (`core/src/channels.rs`, `app/src/curves.rs`): `Channel` addresses any keyframeable
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
  device, including scrubbing.
- **Scrubbing**: while paused, every playhead move asks the engine for a 60 ms snippet with
  4 ms de-click ramps. The mixer only renders the next snippet when the device is about to run
  dry, always at the newest position, so fast drags stay responsive.
- **Audio engine** (`app/src/audio.rs`): a mixer thread renders the timeline into 1024-frame
  blocks with `Mixer` and queues ~130 ms ahead; the cpal callback (realtime: no locks, no
  decoding) copies blocks to the device and publishes which timeline sample is reaching the
  speakers, corrected for output latency. Play/stop bump a generation number so stale blocks
  are dropped instantly on seek.
- **Playback clock** (`app/src/playback.rs`) reads that position, so video is slaved to audio.
  With no device, or if audio fails to start within 500 ms, it falls back to wall-clock time.
  The status bar shows which clock is in use.

## Roadmap

1. ~~Player and editor shell~~
2. ~~Audio playback, mixing, A/V sync, linked clips, waveforms, volume/pan, fades, scrubbing,
   volume keyframes, crossfades, meters~~
   (still to add: per-track meters, audio effects like EQ/compressor, keyframed pan)
3. ~~Compositor and editing: layers, transforms, opacity, blend modes, viewer gizmo, keyframes
   and a curve editor, cross-dissolves, trimming, ripple/roll/slip/slide, ripple delete, track
   locking~~
   (still to add: anchor point, crop, scale/rotate handles on the gizmo, more blend modes,
   Bézier keyframe handles)
4. ~~Export: H.264/H.265/ProRes/DNxHR/image sequences/WAV, presets including vertical video,
   fit/fill/stretch, GPU encoders, an in/out range, an export queue~~
   (still to add: 10-bit HEVC/HDR, custom bitrates, saving your own presets)
5. ~~**Effects**: the effect stack, keyframable parameters, colour correction, levels, curves,
   LUTs, blur, sharpen, chroma key, glow, drop shadow, vignette, an effects browser;
   dissolve, dip, wipe, iris, slide and push transitions~~
   (still to add: letting glow/shadow spill outside the layer, more stylize effects, saved
   effect presets)
6. **Compositing**: ~~composite shots (nested sequences), point tracking (follow and
   stabilise), masks~~, titles and track management are done. Still to come: Bézier mask
   curves and per-point feather, planar and
   multi-point tracking (rotation/scale), a 3D camera and lights, GPU particles, animated text
   presets.
7. ~~**Performance**: proxy media, a frame cache, hardware decode~~
   (still to add: GPU-side frame upload from hardware decoders without the copy back to memory,
   render caching of effects)
