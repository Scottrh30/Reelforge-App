# Reelforge

A HitFilm-style video editor and compositor, written in Rust. Phases 1–4 are done (a working
editor with synced audio, a GPU compositor, and export to MP4), and phase 5 (effects) has begun.

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

- **Export** (**File → Export…**, `Ctrl+M`): renders the whole timeline to H.264 + AAC in
  MP4, MOV or MKV, at full, half or quarter resolution, with three quality levels. It runs in
  the background with a progress bar, time remaining, and Cancel; you can keep editing
  meanwhile. Export waits for every exact frame at full resolution and uses the same compositor
  and mixer as playback, so the file matches what you saw and heard
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
- **Effects** (Controls panel, **Add effect…**): a stack per video clip, applied on the GPU in
  linear light before the clip's transform, so they move, scale and rotate with it:
  - **Colour correction**: exposure (in stops), contrast, saturation, temperature and tint
  - **Gaussian blur**: radius in pixels (two separable passes, any size)
  - **Chroma key**: pick the key colour by hue (green by default, with a swatch), threshold,
    softness and spill suppression
  - **Vignette**: amount, size and softness
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
- **Curves panel** (a tab next to the Editor Timeline): a value graph of the selected clip's
  keyframes. Pick channels on the left (position X/Y, scale X/Y, rotation, opacity and every
  effect parameter). **Drag** a key to change its time and value (**Shift** changes only the
  value), **double-click** a curve to add a key, **double-click** or **Alt+click** a key to delete
  it, and **right-click** for Linear / Ease / Hold. Hold keys are drawn as squares and ease keys
  hollow. Each drag is one undo step
- Opening a `.rfproj` from the command line (or by double-clicking it once associated) opens the
  project
- **Cross-dissolves**: **Edit → Cross-dissolve at playhead** (`Ctrl+D`) adds a 1 s dissolve at
  the nearest cut between two touching video clips; adjust it with "Dissolve in" in the Controls
  panel. Like audio crossfades it uses the spare media either side of the cut. The dissolve is
  rendered correctly in linear light: no brightness dip at the midpoint, even for clips that
  don't fill the frame
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

Not yet: more effects (sharpen, levels/curves, glow, masks…), other transition types (wipes,
dip to black), Bézier handles in the curve editor, keyframed audio pan, animated text
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
  export/  Offline render: exact frames -> compositor -> MovieWriter (media crate), with
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
finally MPEG-4 Part 2. The export result window says which encoder was used.

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
| Alt+X | Clear in/out |
| Shift+Del | Ripple delete (close the gap) |
| Ctrl+D | Add/remove cross-dissolve at the nearest video cut |
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
  `(1−u)·A + u·B` over whatever is underneath.
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
  (sRGB → premultiplied linear float), then one pass per effect, ping-ponging between two float
  textures kept per layer. The result is drawn with the pre-multiplied pipeline, including
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
4. ~~Export to H.264/AAC with progress, cancel and an in/out range~~
   (still to add: ProRes/DNxHR for intermediates, PNG sequences, an export queue)
5. **Effects** ← you are here. Done: the effect stack, keyframable parameters, colour
   correction, Gaussian blur, chroma key, vignette. Next: levels/curves, sharpen, glow, LUTs,
   an effects browser, and effects that reach outside the layer (drop shadow, glow).
6. **Composite shots**, masks, tracking, 3D camera/lights, GPU particles.
   (Text titles and track management are done; next for titles: animated text presets,
   per-letter animation, title templates.)
7. ~~**Performance**: proxy media, a frame cache, hardware decode~~
   (still to add: GPU-side frame upload from hardware decoders without the copy back to memory,
   render caching of effects)
