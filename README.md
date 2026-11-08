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
- **Image sequences** (VFX renders and comps: OpenEXR, DPX, PNG, TIFF, TGA, JPEG…): import any
  one numbered frame (`comp_v2.1001.exr`) and the whole sequence comes in as one clip,
  `comp_v2.[1001-1100].exr`, at the sequence's frame rate; picking several of its frames still
  gives one clip. Padded (`0042`) and plain (`42`) numbers both work, and a sequence runs until
  the first missing frame. Sequences relink by any of their frames when moved, and **Collect
  project** copies every frame into a folder of their own. (A numbered still with no numbered
  neighbours imports as a still.)
- **OpenEXR in float**: EXR frames stay floating point all the way into the compositor (as
  half floats on the graphics card): highlights above 1.0 keep their energy through blurs, glows
  and grading instead of clipping, and light stored with no alpha (fire, sparks and glows
  rendered "additive") adds onto what's under it, because EXR's premultiplied alpha is kept as
  it is. Layers named `beauty.R/G/B/A` and multi-part files are read too (the first layer with
  R, G and B, or Y for black-and-white)
- **10-, 12- and 16-bit footage at full precision**: deeper-than-8-bit video is no longer cut to
  8 bits on the way into the compositor. YUV video of 10 bits and up (ProRes, DNxHR HQX/444,
  CineForm 422, 10-bit HEVC/VP9/AV1, XAVC Intra, 12-bit 4:4:4; 4:2:0, 4:2:2 and 4:4:4, with
  or without alpha) is kept at its bit depth to the graphics card and converted there into half
  floats; deep RGB (16-bit PNG and TIFF, 10/12-bit DPX, CineForm 4444, DNxHR 444 RGB) becomes
  half floats as it's decoded. So grading, keying and gradients keep every level the camera
  recorded, and a 10-bit export of 10-bit footage has 10-bit precision (a ramp tested end to
  end keeps 820 of its 877 levels; through the old 8-bit route, 256). Super-whites (10-bit
  codes above 940, which cameras record in highlights) stay as values above white, for grading
  to bring back. Compositions are read back in floating point too, so they keep the same
  precision (and highlights) inside the outer sequence, and OpenFX plugins get deep video in
  float. 8-bit video goes the 8-bit way it always did
- FFmpeg decoding on a background thread, with frame-accurate seeking (transport and program streams too: TS, MTS, MPG and VOB are searched by timestamp, so a seek steps back until it starts from a keyframe at or before the target; MKV and WebM round frame times to the millisecond, so a frame stamped a fraction after its moment still counts as that moment's frame rather than stalling the decoder)
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
- **Timeline thumbnails**: video clips show a filmstrip, frames all along their length in the
  shape of their media (speed and reverse included), only for the part on screen and decoded in
  the background. Frame times are rounded to a step that suits the zoom, so scrolling and small
  zoom changes reuse pictures. Taller video tracks give bigger pictures (drag a track's lower
  edge). In the Media panel each composition shows a small picture of itself (a third of the
  way in, in its shape). Composition clips show a filmstrip of their rendered picture (in the composition's
  shape), kept until something in the composition changes. **Settings → Show the linked sound's
  waveform under video clips** adds a band along each linked video clip's lower edge showing its
  sound (at its volume and fades), for cutting picture to sound. Right-click a video track's
  header → **Sound waveforms under clips** to show or hide them on that track alone (saved with
  the project), or let it follow Settings. **Settings → Clip pictures** picks a
  filmstrip, the first frame only, or none

- **GPU compositor**: every visible video track is blended into the frame on the GPU
  (wgpu), in linear light with float precision
- **Per-clip transform** in the Controls panel: position, scale (with a lock for uniform
  scaling), rotation, opacity, **anchor point**, **crop**, and **blend modes**, plus Reset /
  Fit / Fill buttons. Clips sit at their native pixel size by default, like HitFilm
  - **Blend modes**, grouped in the menu (hover one for what it does): Normal; Darken, Multiply,
    Colour burn; Lighten, Screen, Colour dodge, Add; Overlay, Soft light, Hard light;
    Difference, Exclusion; Hue, Saturation, Colour, Luminosity. They work on every kind of layer
    (footage, titles, 3D text, particles, compositions, adjustment layers, layers with effects)
    and keyframed opacity fades the result in and out
  - Add, Multiply and Screen blend in linear light on the GPU's blender, as before. The others
    follow the standard (W3C/Photoshop) formulas on sRGB-encoded values, so Overlay and Soft
    light look the way they do in Photoshop and HitFilm. Values brighter than white (a strong
    glow) are clipped to white for those modes
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

- **Snapshot** (the camera button at the right end of the bar under the Viewer, or
  `Ctrl+Shift+E`): saves the frame at the playhead as a PNG, drawn exactly and at the sequence's
  full size (as export would draw it, whatever the Viewer's playback quality or zoom), named by
  project, sequence and timecode (`My film 00-01-23-12.png`; never overwriting one). Snapshots go
  into a **Snapshots** folder beside the project (in Pictures/Reelforge snapshots before it's
  saved). Right-click the button for **Save snapshot as…** (PNG or JPEG, anywhere), **Open** or
  **Choose snapshots folder…**, and **Add snapshots to the Media panel** to bring each one into
  the project as a still
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
    clipping. HDR10 files carry mastering metadata, in the stream (SEI) and in the MP4/MOV
    itself (`mdcv`/`clli` boxes), with the GPU encoders too: they get the Main 10 profile for
    10-bit and HDR (NVENC, Quick Sync, AMF, VideoToolbox, VA-API), the colour tags, and the HDR10
    metadata on every frame to write as SEI. **HDR footage** (HDR10/PQ or HLG, tagged in
    the file; an **HDR** badge in the Media panel) comes in as HDR: its signal is turned into
    light the same way (203 nits or 75% signal is white, BT.2020 brought into the edit's BT.709),
    so its highlights stay above white through effects and grading, and an HDR export of an
    untouched HDR clip gives back the same signal. The Viewer and SDR exports show it with SDR
    white at white and the highlights cut (no tone mapping yet). 10-bit and HDR exports render at 16 bits per
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
- **Audio effects** (Controls panel → Sound → Effects → **+ Add**): **EQ** (a 12 dB/octave low
  cut, low and high shelves, a bell in the middle), **Compressor** (threshold, ratio, attack,
  release, soft knee, make-up gain), **Limiter** (input gain, a ceiling nothing goes over, 5 ms
  of look-ahead, release), **Reverb** (Freeverb: room size, damping, width, mix, pre-delay) and
  **Noise reduction** (spectral gating that learns the steady background noise as it plays:
  how far to turn it down, and how readily). They work on the clip's own sound, in order, before
  its volume, pan and fades; each can be switched off, moved, reset or removed, and every setting
  keyframes at the playhead like volume. Playback and export use the same processing, and
  effects with look-ahead (limiter, noise reduction) are fed that far ahead so the sound stays in
  sync. After a jump each chain hears up to 2 s of what came before first, so it plays as if it
  had been running (scrubbing skips that). A reverb's tail stops where the clip ends.
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
  minimum; the viewer shows the frame at the edge while you drag. A grip shows at each end of
  a clip as you point at it. Pictures (`.jpg`, `.png`, `.tif`, `.webp`, single-frame GIFs and
  the rest), titles and solids can be pulled out to any length, and pulling one's end out
  when another clip starts right after it pushes the clips after it along (a ripple) rather
  than stopping; shortening one leaves a gap as usual. A video clip stopped by its neighbour
  says so, and the Ripple tool (**B**) pushes instead. Edges snap to other clips (Alt to
  disable)
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
- **Magnetic timeline** (the **Magnetic** button by Snap, or **Timeline → Magnetic timeline**;
  kept with each sequence): the main track (Video 1, marked *main*) closes up after itself.
  Deleting or cutting clips from it closes the gaps they'd leave (several at once too), and
  trimming one of its clips with the Select tool ripples: shorter pulls everything after it
  up, longer pushes it along. Clips on the other tracks after the edit move with it, so titles,
  B-roll and sound laid over the main track stay in sync. Moving clips keeps it closed too:
  drag a clip **off** the main track (onto another video track) and the gap closes behind it;
  drop one **onto** it and it's inserted at the nearest cut (a drop inside a clip goes to
  whichever of its ends is nearer, a drop past the end goes on the end), pushing everything after
  along; drag one **along** it and it's reordered. Its linked sound goes with it. One undo step
  each. Other tracks trim, move and delete as usual, roll, slip and slide work as ever, and gaps
  that were there before stay
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
  crop, masks and blend mode) change everything below it. Draw a mask on it to grade or blur
  just part of the frame (a face, the sky): inside the mask the layers below are changed,
  outside they're left alone, with the mask's feather, Subtract and Invert working as usual;
  **Track shape** makes the mask follow what's under it in the footage below. A point is an invisible 3D null:
  choose it as a layer's **Parent** (Controls → Layer) and the layer moves, turns and scales with
  it; points can be parented to points, and cameras and lights can follow them too
  - **Pick-whip**: a selected clip on a video track shows a small link button at its top
    right; drag it onto a point clip in the timeline to parent the clip to it (the point lights
    up while you're over it; anything else is refused with a reason). **Remove parent** is on
    the clip's right-click menu
  - **Seeing who follows whom**: clips with a parent say so ("↳ Point"); select a clip and dashed
    lines join it to its parent and children in the timeline, and the Viewer draws a line from
    the selected layer to its point
  - **No jump**: parenting (or unparenting, or changing parent) keeps the clip exactly where it
    is at the playhead, keyframes and all: its transform and every position, rotation, scale,
    Z and corner-pin keyframe are converted by the change in parent there, as After Effects
    does, so its own animation plays as before and from then on it moves with the point.
    Points, cameras and lights keep their place (and aim) the same way. One undo step
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
  - **Chroma key**: **Pick key colour**, then click the screen in the Viewer (or drag a box
    over it to average a patch: best on an uneven screen) to aim the key at it; for a dull or
    dark screen it also lowers Threshold and Softness so all of it goes. Or set the key colour
    by hue (green by default, with a swatch), threshold,
    softness and spill suppression (the screen's tint taken out of what's kept, brightness
    unchanged). Then the matte: **Clip black** and **Clip white** make the nearly-gone fully
    gone and the nearly-solid fully solid; **Choke** shrinks the edge inwards (or, negative,
    grows it, taking the colour from inside); **Edge softness** blurs only the matte, so the
    picture stays sharp. **View → Matte** shows the matte in black and white (white kept,
    black keyed out), the quickest way to spot holes and noise
  - **Luma key**: removes the dark parts (or, with **Key out → Light parts**, the light ones)
    by brightness, with threshold and softness: fire, smoke, sparks and light shot on black,
    or a subject on white
  - **Difference key**: removes whatever matches a **clean plate**, a frame of the shot with
    only the background in it. Go to such a frame (before the subject walks in) and press
    **Use frame at playhead**, or **Choose file…** for a picture or video of it; then raise
    **Tolerance** until the background is gone. For a locked-off camera and no screen. The
    plate travels with the project (Collect files copies it with the media)
  - All three keys share the matte settings: Clip black/white, Choke, Edge softness and the
    black-and-white **View → Matte**
  - **Mask from this key** (on each key's card): traces what the key keeps at the playhead
    (its biggest area) into a new freehand mask, with as few points as the outline needs. Edit
    it, track it (Track shape), or set its Use to a garbage matte to protect a subject the key
    eats into
  - **Key cleaner** (Keying): put it after a key to tidy what's left. **Spill removal** takes
    the screen's colour out of hair, edges and anything lit by the screen, set by **Screen hue**
    (green 120°, blue 240°, with a swatch): a colour leaning towards the screen is pulled back
    to the hues either side of it, so a green-tinged grey goes grey while yellow and cyan stay as
    they are, and its brightness is kept. **Spill tolerance** leaves faint tints alone. **Fill
    holes** closes pinholes in the matte up to that size (grow, then shrink back) and **Remove
    specks** wipes out stray dots of leftover screen (shrink, then grow back); neither moves the
    subject's edge. **View → Matte** shows the cleaned matte
  - **Matte choker** (Keying): works on any layer's transparency, after a key or on a cut-out
    picture. **Choke** shrinks the edge inwards (negative grows it outwards, taking the colour
    from inside), **Softness** blurs only the matte, and **Matte contrast** firms up what's half
    see-through: towards solid above the **Midpoint** and towards gone below it (100% gives a
    hard edge). Softness then contrast rounds off a ragged, noisy edge. **View → Matte** shows it
    in black and white
  - **Refine matte** (Keying): for hair and soft edges after a key. *Edge*: **Smooth** blurs
    the matte and sharpens it again, so jagged steps and lone specks give way to a smooth
    outline; **Shift edge** pulls it in (negative pushes it out); **Feather** softens it.
    *Colour*: **Decontaminate colours** replaces the screen's colour showing through the
    see-through edge with colour borrowed from the solid part just inside (within **Colour from
    within** pixels), so wisps of hair stop glowing green over the new background. **View →
    Matte** shows the refined matte
  - **Garbage mattes**: set a mask's **Use** to **Garbage matte: keys keep what's inside**
    and every key on the clip keeps the layer inside it, however much it looks like the screen
    (a green shirt, a dark logo on a luma key); it doesn't cut the layer, and shows dashed in
    the Viewer. Several combine with Add, Subtract and Intersect like ordinary masks. To keep
    something *out* (a light stand, the screen's edge), draw an ordinary mask: it cuts the
    layer before the key. Both can be tracked and animated like any mask
  - **Wire removal** (Repair): paints out a wire, a rig, a boom pole's edge or a scratch. Drag
    its **Start** and **End** handles in the Viewer onto the two ends of the wire (a dashed line
    joins them), set **Width** to just cover it and **Edge softness** to blend it in; inside, the
    picture is filled from just past each edge (**Fill from** sets how far past), blended across
    the width so a gradient or a sky carries on through. **Show area** tints what it covers red
    while you line it up. Keyframe the ends to follow a moving wire
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
  - **Lens flare** (Light & stylize): glow, faint starburst, halo ring and ghost reflections
    along the line from the light through the middle of the frame, like a real lens.
    **Lens**: *Warm (spherical)*, *Anamorphic* (a long blue horizontal streak) or *Glow only*;
    **Streak**, **Ghosts** and **Halo** set each part. Drag the light (☀) in the Viewer, or
    keyframe its position to follow a light in the shot. Light is added (it shows on a
    transparent layer too)
  - **Light leaks** (Light & stylize): soft blooms of coloured light drifting in from the
    left, right, top, bottom, corners or anywhere, between two colours, screened onto the
    picture. **Drift** sets how fast they wander and pulse; **Seed** gives another pattern
  - **Neon edges** (Light & stylize): a glowing neon tube along the layer's edges — its
    outline (**Shape**, from transparency, for logos and titles) or the outlines in the
    picture (**Picture**, from brightness). **Colour**, **Tube width**, **Glow radius** and
    **Glow intensity** shape the light, **Hot core** whitens the tube's middle, **Original
    layer** fades the layer itself (0 % = the neon alone), and **Flicker** makes it stutter
    like a failing sign. The *Neon sign* starter look sets one up
  - **Halftone** (Light & stylize): the picture printed as a screen of dots, like a
    newspaper photo or a comic. **Style**: *Black & white* (Ink on Paper), *CMYK print*
    (cyan, magenta, yellow and black screens at their own angles, so they form rosettes)
    or *Colour dots* (each dot the picture's own colour); **Shape**: round dots, square
    dots or lines; **Dot size**, **Angle**, **Contrast**, the **Ink** and **Paper** colours,
    and **Mix** to blend it with the picture. The *Comic print* starter look sets one up
  - **CRT / VHS** (Light & stylize): an old TV or a worn tape. **Curvature** bulges the
    glass (black, rounded corners), **Scan lines** and **Line spacing** draw the dark gaps
    between lines, **Phosphor mask** the red, green and blue stripes; **Colour bleed**
    smears the colour sideways (sharp brightness, soft colour, as on tape), **Tracking
    wobble** jitters each line, **Tracking band** rolls a band of bad tracking down the
    picture, and **Noise** adds snow. **Vignette**, **Brightness**, **Speed** and **Seed**
    finish it. The *Old TV* and *VHS tape* starter looks set one up
  - **Film grain** (Light & stylize): grain that changes every frame (**Frame rate**, 0 =
    still), from fine to coarse (**Size**), mono to colour, strongest in the midtones and
    kept out of the highlights unless **In highlights** is raised
  - **Glitch** (Distort): slices and blocks jump sideways, colours split and scan lines
    flicker. **Amount** is how much of the picture glitches, **Speed** how often the pattern
    changes; keyframe Amount for bursts
  - **Chromatic aberration** (Distort): red and blue pulled apart **from the centre** (stronger
    towards the edges, as a lens does; **Falloff** keeps the middle clean) or **one way** at an
    angle. Negative amounts swap the fringes
  - **Camera shake** (Distort): the picture wanders, turns and zooms a little, smoothly and
    differently every moment: **Amount** (px), **Rotation**, **Speed**, **Roughness** (adds a
    quicker jitter), and **Zoom** to hide the edges (or **Edges**: mirror, repeat or
    transparent). Keyframe Amount for an impact. Shake, grain, glitches and leaks change
    with time on their own, so every frame is drawn (and render-cached) separately
  - **Particles & simulation** (their own category in the browser, and **Timeline → Particles &
    simulation** for ready-made layers):
    - **Particles**: sparks, embers, fire, smoke, rain, snow, fog, dust, debris, confetti, magic
      or a water spray, worked out and drawn on the graphics card (tens of thousands at once)
      - **Timeline → Add particles layer** puts an emitter on its own transparent layer, like a
        blade layer. Drag the emitter (✦) in the Viewer, and the tip of its arrow to aim it
        (Shift snaps to 15°)
      - The preset buttons (Sparks, Embers, Fire, Smoke, Magic, Snow, Dust, Confetti, Rain, Fog,
        Debris, Atomic, Disintegrate, Water spray) set everything but the emitter's position and seed
      - **Emitter**: position, **Shape** (point, a line across the direction of travel, a disc, a
        ring that throws outwards, the whole layer, or **the layer's pixels**) and size.
        **Birth**: rate per second, lifetime and **Randomness** (varies life, speed and size).
        **Motion**: speed, direction, spread, gravity, wind and turbulence. **Look**: size and
        colour at birth and at death, opacity, and the look: *Glow*, *Sparks* (streaks) and
        *Flame* add light; *Soft*/*Solid dots*, *Smoke* (billowing, turning puffs), *Rain
        streaks* and *Debris* (tumbling jagged chunks) cover what's under them. Flame is a
        noise-torn teardrop, hottest low down and early in its life
      - **Emitting from a shape**: **Inside the layer's shape** is born only where the layer's
        picture shows (a title's letters, a logo, a masked area: masks cut the layer before its
        effects), **Along the layer's outline** only along its edges, both in the emitter's own
        colours with the layer still drawn. Particles are scattered over the whole layer and
        only those landing on the shape are kept, so raise **Birth rate** for a small shape
      - **Collisions** (**Collide with**): **A floor** (at **Floor**, % down the layer), **A floor
        and the sides**, or **The layer's shape**: snow settling on a title, sparks bouncing off a
        logo. **Bounce** is how much speed they keep coming off (0 sticks), **Friction** how much
        they slow sliding along; too slow to bounce, they rest. Turbulence dies away once they've
        landed. Each particle's path is walked in steps on the graphics card, so it still needs
        no frame-by-frame simulation
      - **Atomic particles**: with the shape **Layer's pixels**, particles are born only where the
        layer's picture shows, each in the picture's colour there, turning to the colour at
        death as they die (the Atomic preset: a picture giving off glowing dust). **Layer's
        pixels, layer hidden** draws only the particles, so the picture is made of them (the
        Disintegrate preset: it breaks up and blows away)
      - Every setting can be keyframed. Emitter settings are read at each particle's birth, so a
        moving emitter leaves a trail, and keyframing **Birth rate** up and back down makes a
        burst; gravity, wind, turbulence and the look apply to every particle alive
      - Nothing is simulated frame by frame: every frame is worked out on its own from the clip's
        start, so scrubbing, playing backwards and exporting all show exactly the same
        particles. They start with the clip, so trim its start to begin with particles already
        in the air
    - **Ready-made layers** (**Timeline → Particles & simulation**), all ordinary effects you can
      change afterwards, sized for the sequence's frame: **Fire** (flames, embers and a little
      smoke), **Smoke**, **Explosion** (a flash, a fireball, debris and sparks thrown out in a
      burst at the layer's start, and smoke billowing after), **Rain**, **Snow** and **Fog** over
      the whole frame, **Muzzle flash**, **Bullet hit** and **Gun shot** (a flash, then its hit a
      moment later; these three are adjustment layers so they light and mark the footage below),
      **Fluid**, and **Shatter** or **Atomic particles** for the selected layer
    - **Shatter**: the layer breaks into irregular pieces at **Breaks at** (seconds into the
      clip), thrown out from the **Impact** point (drag it in the Viewer) with **Force**, out to
      its **Reach** (pieces further away stay put), falling with **Gravity**, tumbling with
      **Spin**, fading after **Fade after** seconds. **Pieces across** and **Irregularity** shape
      them; they fit together exactly until it breaks
    - **Muzzle flash**: a brief, never-twice-the-same star of flame at the barrel (drag it there;
      set **Direction** and **Size**) for a **Rifle**, **Pistol**, **Shotgun** or **Side vents**,
      about a frame long, at **First shot at**, or a burst (**Shots per second**, **Shots**). **Light
      on the scene** brightens what's round it in the flash's colour
    - **Bullet hit**: at **Hits at**, a flash, sparks flying and falling (most off metal), a puff
      of dust (or smoke off metal) blowing out and settling, and the mark it leaves on **Wall**,
      **Concrete** (with cracks), **Metal** (a torn bright rim and scorch), **Glass** (a star of
      cracks and rings) or **Wood**
    - **Disintegrate** (an effect, or **Disintegrate the selected layer**, starting at the
      playhead): the layer breaks up into particles of its own colours over time. A front sweeps
      across it in the **Direction** you choose (0° left to right, 90° bottom to top), from
      **Starts at** for **Takes** seconds, made ragged by **Roughness**; just ahead of it the
      picture glows (**Edge glow**, **Edge glow width**), behind it the picture is gone and its
      particles drift off (**Drift speed**, **Drift direction**, **Spread**, **Gravity**,
      **Turbulence**), fading to **Fade to** over their **Lifetime**. **Particles** sets how
      many (up to 60,000), **Particle size** and **Look** how they're drawn. The front and the
      particles come from the same moment for each point, so they meet exactly
    - **Fluid**: coloured smoke or ink poured from a **Source** (drag it) in a **Direction** with
      **Force**, swirling (**Swirl**), rising (**Rise**, negative sinks) and fading (**Fade**),
      changing between two colours (**Colour change** times a second). A real fluid simulation
      (stable fluids: the flow pushed by the source and by rising colour, swirls kept alive by
      vorticity confinement, made incompressible by a pressure solve), run on the graphics card
      on a grid 160, 320 or 512 cells across (**Detail**). It's worked out from the clip's start
      and kept as it plays (a step a frame), with a checkpoint every second, so seeking back
      starts from the nearest one. **Flow round the layer's shape**: put it on a title or a
      keyed layer and the fluid flows round the letters or the subject instead of through them
  - **Generate** (a category of its own):
    - **Fractal noise**: a pattern that drifts and evolves, like After Effects' Fractal Noise or
      HitFilm's Clouds. **Type**: *Smooth*, *Turbulent* (billowing smoke), *Ridged* (sharp
      crests and veins), *Clouds* (puffs with clear sky between), *Liquid* (the noise pushed
      along by itself) or *Marble* (bands bent by the noise). **Scale**, **Detail** (how many
      finer layers, fractional values blending in), **Roughness** (how strong the finer layers
      are), **Contrast** and **Brightness**. **Evolution** (keyframable, a turn being one whole
      change) and **Evolution speed** keep it changing; **Drift X/Y** slide it along (clouds
      blowing past). Drawn in two colours (**Dark** and **Light**), it **replaces the layer**
      (put it on a plane), goes **over** it (screen, multiply, overlay or add), or becomes the
      layer's **transparency** (a smoky reveal, a texture for a key or other effects), with an
      **Opacity** and a **Seed** for a different pattern. It's 3D gradient noise on the
      graphics card, the same every time at the same moment
  - The **browser** (`E`, or **Edit → Effects & transitions…**) lists every effect by category
    (Colour, Blur & sharpen, Keying, Light & stylize, Particles & simulation, Generate…) and every transition, with a search box
    and a one-line description of each; **Add** puts an effect on the selected clip
  - Turn each effect on or off, reorder (applied top to bottom) or remove it. Every parameter
    has a keyframe button and works like the transform properties. The viewer and export use
    the same GPU passes, so exports match what you see
- **Tracks**: as many video and audio tracks as you need. **+ Video** / **+ Audio** under the
  track headers (or **Edit → Tracks**) add one; new video tracks go on top. Right-click a
  header to rename, add or delete a track (deleting takes its clips with it, and can be undone);
  double-click a track's name to rename it. Drag a track's header up or down to reorder the
  stack (its clips go with it; video tracks stay above the audio tracks), or use **Move track
  up/down** on the right-click menu. **Track heights**: drag the bottom edge of a track's
  header to make its row taller or shorter (32–240 px; double-click the edge to reset). Taller
  audio rows give a bigger waveform and volume line to work on. **Collapse** a track to a thin
  strip with the arrow before its name (clips still show, as slim bars); the right-click menu
  collapses or expands all video or all audio tracks at once, and **Reset track heights**.
  Heights and collapsed tracks are saved with the project (they aren't edits, so there's
  nothing to undo). Clicking (or right-clicking) a track's name highlights
  the track, its row included; clicking empty timeline or **Esc** clears it, and **Alt+↑/↓**
  moves it up and down the tracks. The highlighted track is the **target**: **Paste** puts
  the copied clips there (several tracks' worth keep their arrangement, the lowest landing on
  it), and new layers (**New…** planes, adjustment layers, points, cameras and lights, titles,
  energy blades) go on it if it has room at the playhead, or on a new track just above it,
  which then becomes the highlight, so several stack up in order. With no track highlighted
  they go on top as before. **Del** with no clips selected offers to delete the highlighted
  track (it asks first; undo brings it back). The highlight follows its track when tracks are
  moved, renamed or added. **Duplicate track** on the
  right-click menu puts a copy just above it, named "Video 1 copy", with all its clips,
  effects, keyframes and masks (one undo step). Clips linked or parented to others on the same
  track stay that way among the copies; a copied video clip isn't linked to the original's
  audio, and parents on other tracks are kept. When the tracks don't fit, the mouse wheel
  scrolls through them (Shift+wheel moves along the timeline)
- **Clip colours**: right-click a clip → **Clip colour** to pick red, orange, yellow, green,
  teal, blue, violet, pink, grey or any other colour (**Other colour…**), or **Usual colour**
  to go back to its kind's colour. It applies to every selected clip and what's linked to them,
  is one undo step, is saved with the project, and carries over when clips are split, copied
  or duplicated. It's only a label: the picture doesn't change. A coloured clip with picture
  thumbnails keeps a strip of its colour along the top. Planes also have **Plane colour…**
  there, which changes the colour they fill the frame with
- **Text titles** (**Edit → Add title at playhead**, `Ctrl+T`, or **New title** in the Media
  panel): a 5 s title on the top free video track (a new track is made if none has room).
  In the Controls panel: multi-line text, any installed font (with a search box), size, colour,
  alignment, line and letter spacing, outline, soft drop shadow (distance, angle, softness,
  colour) and a background box for lower thirds, plus one-click **Centre / Lower third / Top**
  placement. Titles are ordinary layers, so the transform, keyframes, effects, dissolves and
  blend modes all work on them; click the text in the Viewer to select and drag it. Titles are
  purple on the timeline and can be any length. **Animate** brings the text in piece by piece:
  **Typewriter** (letters appear in turn), **Fade in letters**, **Fade in words**, **Slide up
  lines** (each rising half an em into place as it fades in) or **Bounce in letters** (each
  dropping in and bouncing to rest), with the time between pieces (**Stagger**), how long each
  takes, a **Delay**, and **Out at the end too** (they leave in the same order, gone as the clip
  ends). Pieces in the same state are drawn together, outline and shadow included, so a fading
  letter keeps its edges; the box behind stays put. It's part of the title's style, so a title
  preset carries it to other titles
- **3D text** (**Timeline → Add 3D text**, or set **3D depth** on any title): the letters are
  extruded into solid 3D objects, as deep as **3D depth** (pixels), seen through the camera and
  lit by the scene's lights, or by a soft light from above left when there are none. **Sides**
  gives the sides their own colour (otherwise they're the text colour; Add 3D text makes them a
  shade darker). **Bevel** (pixels) cuts back the letters' edges front and back so they catch
  the light, as **Round** (a smooth bulge), **Flat** (a straight 45° cut) or **Cove** (hollowed
  in); the bevel takes the face colour, and it's held to half the depth and to 0.4 of the
  letters' thinnest stroke (measured across the glyphs), so thin fonts can't cross over
  themselves. It's always a 3D layer: move it in Z and turn it about X and Y in the Transform
  section (**Turn it in 3D…** in the title settings shows those), animate it with keyframes, put
  a camera move around it, and it keeps its depth of field, opacity, blend mode and effects. At
  Z 0, not turned, its front face is exactly where the flat title's letters would be. Outline,
  shadow and box aren't drawn on 3D text (a Drop shadow effect still works), nor are masks
- **3D models**: **Timeline → Add 3D model…**, or import or drop a model file like any media,
  and it goes on the timeline at the playhead as a 3D layer, turned a little so its depth
  shows. Reads **OBJ** (with its MTL colours and diffuse textures, `map_Kd`), **glTF/GLB**
  (node transforms, base colours, vertex colours and base-colour textures), **FBX** (binary
  7.x: meshes, model transforms, material colours; ASCII FBX needs saving as binary), **USD/USDA/USDC/USDZ**
  (meshes with their transforms, references and display colours), **STL** (ASCII and binary)
  and **PLY** (ASCII and binary, with vertex colours). Polygons are split into triangles, and
  normals the file doesn't give are worked out from the faces (shared where they meet at less
  than 40°, so curves are smooth and edges sharp). It's drawn by the 3D text renderer, so it's
  lit by the scene's lights, casts and takes shadows, cuts through other 3D layers, and moves,
  turns and animates in Transform like any 3D layer, about its middle. Its settings: **Size**
  (its largest side, px), **Colours** (its own, or one colour you pick), **Parts** (each
  material or object by name, with its own colour picker: pick a colour to paint just that part,
  texture and all, and ↺ to put the file's back), **Up is** Y or Z (STL
  and Z-up files are set to Z by themselves), **Reload** (after changing the file elsewhere;
  it's also read again when the file changes) and **Show file**. The file stays where it is
  and is linked from the project (not copied, and not yet found again by Relink). Textures (PNG,
  JPEG, TGA, BMP) are packed together, each kept at up to 1024 px, and repeat where the model
  asks; skinned or animated models, normal and roughness maps, and model cameras/lights aren't
  used yet
- **3D shapes and logos**: **Timeline → Add 3D logo (SVG)…** turns an SVG file into a shape
  title extruded like 3D text (depth, bevel, side colour, lit, shadows, cutting through 3D
  layers): every visible filled path with its own fill rule (non-zero or even-odd, so holes
  stay holes), strokes as the area they cover, transforms applied, curves cut into short
  straight pieces. Text in the SVG has to be converted to outlines first, and gradients and
  images aren't used (the shape takes the title colour). A mask's **3D shape** button (Masks)
  makes one from the mask's outline as seen at the playhead, as high as the mask and placed
  where it is (on a layer that isn't rotated), facing the camera. **Size** is the shape's
  height; **3D depth** 0 makes it a flat filled shape (outline, shadow and box work then)
- **Proxies**: heavy footage (above 1080p, or 720p and up in HEVC/AV1/VP9/ProRes/DNxHD/CineForm) gets a
  small editing copy automatically, made in the background, with a progress bar in the Media
  panel; **Make proxy** does it for any clip. Proxies are 960×540 with every frame a keyframe,
  so jumping around is instant (8 s of 4K: ~1.5 s per random seek on the original, 3 ms on the
  proxy). The **Proxies on/off** button in the viewer switches between proxies and originals;
  export always uses the originals. Beside it, the Viewer's **performance readout** says what it
  plays from (Proxies, Proxies 2/3, Originals) and, while playing, how many frames a second it
  keeps up with against the sequence's rate, with a dot (green: all of them; amber: most;
  red: falling behind) and the frames dropped each second; its tooltip adds hardware decoding
  and the graphics card, and what to do when it can't keep up. The Viewer's **playback quality**
  (Full, ½ res, ¼ res) draws its picture at that fraction of full resolution and decodes footage
  that much smaller too, so heavy projects play smoothly; layers, effects and wipes keep their
  places, and export and Render preview stay full. **Automatic** steps down by itself (full → ½ →
  ¼) when playing falls behind (frames taking most of their time to draw, or a quarter of them
  not ready in time) and back up after about 3 s with room to spare (8 s just after stepping
  down, so it doesn't see-saw); the button shows what it picked. In its menu, **Full resolution when paused**
  keeps the still picture sharp while playback stays light. Proxies are kept per file in your cache folder, so other
  projects using the same footage reuse them. **Settings → Make proxies as** picks the format:
  - **H.264** (the default): the smallest files, decoded on the graphics card
  - **ProRes 422 Proxy**: 10-bit 4:2:2 colour, so grading looks closer to the original; about
    three times the size
  - **DNxHR LB**: 8-bit 4:2:2, the lightest for the processor
  On one core, without the graphics card, a 960×540 frame of grainy footage decodes in about
  1.8 ms as DNxHR LB, 2.6 ms as ProRes Proxy and 3.9 ms as H.264. ProRes and DNxHR proxies also
  open in other editing programs as standard offline media. Proxies already made in another
  format are still used; Settings offers to make them again in the new one
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
- **YUV on the GPU**: most video (4:2:0, 4:2:2 or 4:4:4, 8 to 16 bits, from software or hardware decoders) now
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
  s of 1080p at 25 fps), and **on disk** too (the cache folder's `render`, deflated, 8 GB by
  default; **…and on disk** in Settings, 0 for memory only), named by each frame's recipe: so
  reopening a project, or another using the same clips and effects, finds its rendered frames
  again (identical frames, like a still title's, are kept once). Frames are written in the
  background; one not in memory is read back from disk when it's needed. The least recently
  used files go first when the disk budget is full. **Edit → Clear render cache** (or **Clear**
  in Settings) empties both
- **Low disk space warning**: every 15 seconds Reelforge looks at how much space is free where
  it writes by itself (rendered previews, proxies, autosaves, the project's folder). Under
  2 GB (**Warn below** in Settings → Disk; 0 never warns), a red strip under the menu bar says so, with buttons to **Clear rendered previews**
  (with their size on disk) and **Delete unused proxies** (proxies of footage not in this
  project, with how many and their size); ✕ hides it until space runs short again. While it's
  short, rendered previews aren't kept on disk (they still are in memory) and proxies aren't
  started by themselves (asking for one still works), so the drive doesn't fill up behind your
  back. **Exports** check first: each format's usual data rate (or the bitrate you set) gives
  an estimate of the file's size, on the big side, and an export that wouldn't fit with
  256 MB to spare isn't started; the message says how much it needs and how much is free.
  The **export dialog** shows the same estimate and the free space on the destination drive
  beside its buttons as you change the format, size, range or file ("About 1.4 GB · 37 GB
  free"), in red when it won't fit.
  A running export whose drive gets within 64 MB of full stops cleanly and says so (Retry
  once there's room), rather than failing part-way with a cryptic error
- **Disk** (Settings): how much is free on the drive Reelforge keeps its files on, and what
  its rendered previews, proxies and autosaves take, each with a button: **Clear** the rendered
  previews, **Delete unused** proxies (those of footage not in this project; how many is shown)
  and **Delete old** autosaves (all but this session's). **Cache folder**: put proxies and
  rendered previews on another drive (a fast SSD for smooth playback, or a bigger disk) with
  **Choose…**, or back with **Default**; with **Move what's there** ticked, the files there now
  move across in the background (renamed on the same drive, copied then deleted across drives;
  any in use stay put, and the status bar says how many moved). Autosaves, crash recovery and
  logs stay in the app's own folder
- **Autosave and crash recovery**: the project is autosaved every 2 minutes (when it has
  changed). If Reelforge doesn't close properly, it offers to recover that work the next time
  it starts; **File > Open an autosave…** opens older ones. Saving always writes a new file and
  then swaps it in, so a crash mid-save can't corrupt the project. Closing, starting a new
  project or opening another asks **Save changes?** if there's unsaved work
- **Export edit for another editor** (**File > Export edit for another editor**): hand the cuts
  of the sequence being edited to another program as a **CMX 3600 EDL** (Avid, Premiere,
  Resolve and most others: Video 1 plus up to four audio tracks, reel names from the file names,
  dissolves, M2 speed lines, clip names, file paths and markers as comments), **FCPXML 1.10**
  (Final Cut Pro, Resolve: Video 1 as the primary storyline, other tracks as connected clips,
  speed as time maps, dissolves and markers), **Final Cut Pro 7 XML** (Premiere, Resolve: every
  track, Time Remap speed, cross dissolves, markers) or **OpenTimelineIO** (Resolve, Hiero, RV,
  pipeline tools: every track, linear time warps and freeze frames, dissolves, markers). What
  goes across is the edit: which part of which file plays where, on which track, at what speed.
  Effects, colour, masks, keyframes and titles stay in Reelforge; titles, solids and
  compositions become named gaps. All four have been read back with OpenTimelineIO's own
  readers. AAF isn't written yet
- **Project versions** (**File > Project versions…**, or **Save version…** / Ctrl+Alt+S): keep
  the project as it is now under a name and a note (*Rough cut for the client*), in a
  "*project* versions" folder beside the project file. Every Save also keeps one by itself when
  the project changed since the last (the newest 30 of those; **Name it** to keep one for good,
  or turn it off in the dialog). Each version lists when it was kept and its size, rate, length,
  sequences, clips and media. **Restore** puts the project back to that version as one undo
  step (what was there is kept as a version first, and the project file isn't written until you
  save); **Open as copy** opens it as a separate, unsaved project to look at or copy clips from;
  **Rename** and **Delete** tidy the list. Versions are ordinary project files, so they open
  with File > Open too
- **Missing media**: files that have moved show a red banner in the Media panel and a
  "MEDIA OFFLINE" frame in the viewer. **Relink…** a file and Reelforge also looks for the
  other missing files in that folder (and its subfolders) by name; **Search a folder…** does
  it for all of them. Relinking can be undone. Export won't start while media is missing
- **Replace footage** (right-click footage in the Media panel, or a clip in the timeline →
  **Replace footage**): swap a file for another (a new grade, a re-render, the final VFX shot, a
  higher resolution) and keep everything done with it: every cut, trim, speed change, effect,
  mask, keyframe, track and transition stays as it was. From the Media panel (or **Everywhere
  it's used** on a clip) every clip of that file changes; **In this clip only** changes just that
  clip and the sound linked to it, bringing the new file into the Media panel. The new footage is
  laid out at the old one's size (fitted to its shape), so a 4K version of an HD shot doesn't
  come out twice as big and masks and tracking stay put; the **at its own size** entries use the
  new file's own pixel size instead. A frame of a numbered image sequence brings in the whole
  sequence. The status line says if clips run past the end of the new file (they hold its last
  frame) or if it lacks sound or picture the clips use. One undo step
- **Multicam** (**Timeline > New multicam source…**, or right-click a take in the Media panel):
  tick two or more takes of the same event, in angle order, and line them up by **sound** (the
  takes' loudness is compared, coarse to fine, so the same claps and words line up to a hundredth
  of a second, then each starts on a frame) or by **their starts**. That makes a multicam source,
  a composition with one video and one audio track per angle (named after the takes), and puts a
  multicam clip playing it at the playhead, with the Viewer showing **every angle in a grid**
  (2×2 up to four, 3×3 up to nine, 4×4). Play it and press **1–9**, or click an angle in the
  grid, to cut to that angle at the playhead, live; **Shift** cuts the picture only, the sound
  staying on the angle heard. The angle seen is outlined in the grid and the one heard marked ♪;
  the chip in the Viewer's top bar (or **Timeline > Multicam: show all angles**) switches between
  the grid and the edit as it plays. Each cut is an ordinary cut of the multicam clip, so it can
  be trimmed, rolled and undone like any other, and each piece shows its angle's name on the
  timeline. The Controls panel shows a multicam clip's angle and lets its **Sound** follow the
  picture or stay on one angle (the best microphone). Open the source to slip a take that didn't
  line up. Exports and rendered previews are always of the edit, never the grid
- **Settings** (**Edit > Settings…**): hardware decoding, frame cache size, render cache size,
  proxies, autosave interval, and the proxy/autosave folders
- **Speed** (Controls panel, any footage or composition clip): 1–2000%, with one-click 25/50/
  100/200/400%, and **Reverse**. The clip keeps playing the same stretch of footage, so slow
  motion makes it longer (clips after it on its tracks move along) and fast motion shorter;
  linked audio changes speed with it, pitch and all, like tape. Keyframes stay where they are on
  the timeline. Trimming, slipping, splitting and tracking all follow the speed
  - **Time remapping** (tick it in the Speed section): keyframe *which moment of the footage
    shows when*, so one clip can slow down, speed up, stop and even play backwards. It starts with
    a key at each end showing what the clip showed. Move the playhead and type the footage time to
    show there (**Shows**, in seconds) to put a key there, or press ◆ to add one holding what's
    there now (◆ again removes it). The graph shows footage time going up along the clip: drag a
    key up or down; flat is a held frame, steeper is faster, falling is backwards. **Ramps** sets
    how the speed changes between keys: steadily, easing in and out, or holding each key's frame.
    **Speed here** reads the speed at the playhead. The clip's length doesn't change (trim it as
    usual; past the first and last keys their frames hold), it goes on the linked sound too, and
    that sound is silent while remapping is on. Untick to go back to the clip's speed
  - **Between frames** (slowed, sped up, reversed or remapped footage): **Nearest frame** holds
    each frame; **Frame blending** cross-fades the two frames either side of the moment shown;
    **Optical flow** works out, on the graphics card, how every pixel moves between the two
    frames, both ways (coarse to fine, with an edge-keeping median so things moving differently
    keep their own motion), and draws the moment in between from both, pushed along that motion.
    It knows when things cross: what one frame sees and the other doesn't (the background a car
    uncovers, the part of a person walking behind a post) comes only from the frame that sees it,
    and where two things land on the same spot the one in front wins, so they don't smear into
    each other. **Flow quality**: **Fast** (works out the motion on a quarter-HD copy), **Balanced**
    (half HD, the default) or **Best** (full size, refined more); better is slower. Export and
    Render preview use it too. Footage deeper than 8 bits (10-bit, HDR, EXR) keeps its precision
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
- **OpenFX plugins**: Reelforge hosts OpenFX image effects (`.ofx.bundle`, the plugin format of
  Resolve, Nuke, Vegas and HitFilm). It looks in the standard folder (`C:\Program Files\Common
  Files\OFX\Plugins` on Windows), the `OFX_PLUGIN_PATH` folders, its own OpenFX folder and any
  you add in **Edit → Plugins… → OpenFX**; what it finds shows in the effects browser under
  **OpenFX**, by the plugin's own name. Their settings become ordinary effect settings
  (numbers, checkboxes, menus, colours) with keyframes, undo and paste attributes, and a
  plugin's **positions get handles in the Viewer** to drag (% of the frame, like Reelforge's
  own). They run on the picture first, in 32-bit float (premultiplied, sRGB-encoded, values
  above 1 kept), before the built-in effects; the Viewer and export both use them. Plugins
  that **render on the graphics card do: with CUDA, OpenCL or OpenGL**, whichever they
  support that works here, in that order (NVIDIA's driver for CUDA, the installed OpenCL
  drivers, and an OpenGL context Reelforge makes; OpenCL on the processor only for plugins
  with nothing else). If one fails the next is tried, then the processor, if the plugin can;
  the effect card says what it rendered with, the OpenFX window what each API would run on,
  and a tick box switches the graphics card off. Plugins that **read other frames** (noise reduction,
  frame blending, optical-flow retiming in the Filter context) are asked which and get them
  from the clip. Plugins load in the background at startup; one that crashes Reelforge while
  loading is skipped the next time (with **Try again** in the window); an effect that fails to
  render says why on its card. Not offered: Metal (Apple's, so only on a Mac, which Reelforge
  doesn't run on), overlays the plugin draws itself, generators, transitions and the Retimer
  context. Details in
  [PLUGINS.md](PLUGINS.md#openfx-plugins)
- **Start screen** (when Reelforge opens, and **File → Start screen…**, **Ctrl+Shift+N**): start
  a new project from a preset, drawn to its shape — **Social** (Vertical 9:16 at 30 or 60 fps for
  Reels/TikTok/Shorts, Portrait 4:5, Square 1:1), **Web & TV** (1080p at 30, 60, 25 and 29.97
  fps, 4K UHD at 30, 60 and 24, 720p) and **Cinema** (1080p at 24 and 23.976, Scope 2.39:1 and
  Flat 1.85:1 at 4K, DCI 2K, 4K and 2K Scope). Click a card for its usual frame rate (outlined),
  or one of the rates under it: every preset also offers 25, 30, 50 and 60 fps. Each section
  folds shut with a click on its heading (remembered). Or **any size** and frame rate typed in; or from
  **your templates**: **File → Save as template…** keeps a whole project to start from (its
  sequences, tracks, titles, effects and media), and a project started from one is new and
  unsaved, so the template is never written over. **Recent projects** sit beside them, newest
  first, each with a picture of the Viewer from when it was last saved, its size and frame rate,
  when, and where; ones that have been moved or deleted say so and can be taken off the list.
  **File → Open recent** has the last ten. A tick box (also in Settings) decides whether it shows
  at launch; it doesn't when Reelforge is opened with a project, or when there's crashed work to
  recover first
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
  - **Adding and deleting points**: right-click the mask's line in the Viewer for **Add a point
    here** (right where you clicked; on a curve the shape stays the same), or right-click a
    point for **Add a point after
    this one** / **before this one** (halfway along that side, on the curve if it's curved, so
    the shape doesn't change), **Delete point** (a mask keeps at least three) and curve / sharp
    corner; the Masks panel has **Add point** and **Delete** for the selected point too. Each is
    one undo step, and an animated mask gets the point in every keyframe so it still blends
    between them. Adding a point to a rectangle makes it a freehand mask; ellipses have no
    points to add
  - **Feather per point**: click a point, then set **Feather here** in the Masks panel; the
    feather blends between points along the sides (**Reset** goes back to the mask's feather)
  - **Animate shape**: once ticked, reshaping the mask (points and handles) at the playhead
    sets a keyframe there
  - Masks are applied before the clip's effects and move with the layer's transform. They
    work on every kind of layer: footage, pictures, titles, compositions and solid colours (a
    solid's mask and effects run at the layer's full size, so its edges are crisp)
  - With a tracker on the clip, **Apply to → Mask: …** makes the mask follow the tracked point
  - **Track shape** (rotoscoping; on an adjustment layer, solid, title or picture it follows
    the topmost footage below at the playhead): draw the mask around something, then ◀ or
    ▶ follows what's inside it from the playhead to the clip's start or end, keying the shape
    at every frame. Choose how it may move: **Moves**, **Moves, turns, scales**,
    **…and tilts** (perspective), or **Bends** for things that change shape (cloth, skin,
    hands, a face): each part of the shape then follows what's under it, on a 4 × 4 grid over
    the mask. What's inside the mask is followed, so the background doesn't pull it (when
    bending, detail just outside the mask helps only once it has kept moving with it). Where it slips, drag the points back on that frame and track on from
    there; keys outside the tracked stretch are kept
  - **Smooth keys** (a mask with keyframes): takes the jitter out of a tracked shape. Each key
    is eased towards a steady line through its neighbours (1 to 8 frames wide), so shake goes
    and the movement, even at the ends of the track, stays; one undo step
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
    flat. A grid (2 × 2 to 32 × 32 cells, or **Custom…** for any number across and down up to
    32 each; **Grid** in the panel) fills the four big corner handles
    you drag over the surface; before tracking, the inner points can be dragged too, to follow a
    curve, and moving a corner afterwards carries them along with it (they keep their shape).
    **Shift+click** points, or **Shift+drag** a box over the grid, to pick several (Escape drops
    them): dragging a picked point moves them all. **Falloff** (in the panel, in cells) makes
    the points around a dragged one follow it, less and less with distance (a smooth fade).
    A grid finer than 8 × 8 is tracked as a coarser one first (with about five features per
    cell, at least 4 × 4), which the fine grid then adds detail to where features say so, so
    cells smaller than a feature don't drift. Many small features inside are followed, and every frame the grid's points are solved
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
  **Blend fixes** (frames) eases such a correction into that many tracked frames on each side,
  fading out (a raised cosine), so the path doesn't jump at the corrected frame; it works for
  every kind of track, mesh points included (and with a group of them picked).
  **Smooth track** (frames) takes the jitter out of the tracked paths without tracking again:
  the Viewer draws the smoothed paths solid and the raw ones faint and dotted, and everything you
  apply the track to (layers, masks, blades, stabilising) uses the smoothed paths. The tracked
  data is kept, so it can go back to 0 at any time; apply the track again after changing it.
  It suits a shaky track of a steady move; real shake of the camera is something the layer
  should follow, so leave it at 0 there. When **stabilising**, **Keep camera moves** (frames)
  keeps deliberate camera moves and takes out only the shake, and **Hide edges** zooms in just
  enough that the edges the counter-motion uncovers never show: **Zoom in once** (one zoom for
  the whole clip, enough for its worst moment) or **Zoom with the motion** (each frame's least
  zoom, held over a moving maximum and eased, so it never lets an edge through but zooms less
  where the shake is small). It works on corner-pinned (perspective) stabilising too. Everything is written as ordinary keyframes you can edit or undo, and masks on the
  clip can follow the track too
  - **Onto a point**: choose a **Point** (made with **New… → New Point**) under **Apply to** and
    the point follows the track; every layer, camera and light parented to it follows too, so
    one track can drive several layers. The point keeps where it is at the start of the track.
    One-point tracks move it; two-point tracks also turn and scale it; four-point, planar and
    mesh tracks give it the surface's best-fitting move, turn and scale (a point has no
    perspective). Tick X, Y, rotation and scale to choose which parts are keyed
- **3D camera tracker** (Track panel → **3D camera**): works out how the camera moved through
  the whole clip, its lens, and where the details in the shot are in 3D, so 3D layers, titles,
  3D text, points and lights can be put into the scene and stay put as the camera moves (as
  After Effects' 3D Camera Tracker does). Select a footage clip and press **Analyse**: a few
  hundred details are followed through the shot (new ones taken up as others leave the
  picture), then the camera is solved. If you know the lens, tick **I know the lens** and give
  its **Focal length** (full-frame equivalent, 36 mm across; the field of view it makes is
  shown): short shots, or ones moving mostly sideways past one flat surface, can't always pin
  the lens down by themselves (lenses a few degrees apart fit equally well), though what you
  put in the shot still stays put. For a camera that only turns (on a tripod), tick **Camera
  only turns**; without it, a shot with too little parallax is found to be one anyway. The
  panel then shows how well it fits (error in pixels: under 0.5 very good, under 1 good), the
  lens, and how many frames had to be filled in. The solved points show in the Viewer as small
  crosses (green fits well, yellow fairly, orange poorly); click one to pick it, **Shift+click**
  or **Shift+drag** a box to pick more (Escape drops them). **Create**: **Camera** makes a
  camera clip keyed every frame to move like the real one; **Solid**, **Title**, **3D text**
  and **Point** put one on the picked points (lying on their surface when three or more are
  picked, else facing the camera), and make the camera too if there isn't one yet. **Set ground
  plane** turns the scene so the picked points' surface is the floor (its middle the origin),
  for lights and text that stand on it. Everything made is ordinary clips and keyframes, all
  undoable. On rendered test footage of a camera moving past a textured floor and wall, it
  fits to 0.1–0.3 px and the camera's path comes within 1° and 2 % of the truth (within 0.1°
  and 0.5 % given the lens)
  - **Leave things out of the solve**: something moving on its own (a person walking
    through, cars, water, a screen) drags the solve with it. Draw a mask over it and set its
    **Use** to **Leave out of the 3D camera track** (animate or track the mask to follow it):
    it doesn't change the picture (it shows dotted in the Viewer), and details followed into
    it are cut off there, so only the scene's own details solve the camera. Several combine
    with Add, Subtract and Intersect, and **Invert** solves from only what's inside. The panel
    says when masked areas are being left out. In a test with 120 details on a blob sliding
    across the shot, the solve still comes within 0.3° and 2 % of the true camera
  - **Solve error graph** (Track panel → **Clean up**): a bar per frame for how well the solve
    fits it (the average distance, in pixels, between the solved points and the details they
    were followed from), orange over the **Fitting worse than** limit (dashed), with the
    playhead's frame marked. Hover for a frame's figure; click or drag to go there, so a bad
    stretch (motion blur, something passing) is easy to find. Worked out once per solve
  - **Delete bad points and re-solve** (Track panel → **Clean up**): points that slide
    (orange in the Viewer, or on something that moved on its own) pull the camera off. Pick
    them and **Delete picked and re-solve**, or set **Fitting worse than** (px; it counts how
    many) and delete all of those at once. The camera is solved again from the rest of the
    details it followed, in the background, without following the shot again; a lens you gave
    is kept, as is the camera clip already made (press **Camera again** to update its moves).
    Undoable. A **ground plane** you set is set again through the same details (when at least three of its points are kept). A solve imported from a file, or made by an
    older version, doesn't keep its details: press **Analyse** again instead
  - **Export** a solve for other applications: **After Effects** (a script: File → Scripts →
    Run Script File… makes a comp with a camera keyed every frame and a 3D null per point),
    **Blender** (a script to run in the Scripting workspace: the camera, render size and frame
    rate, and the points as a cloud of vertices), **Nuke** (a script with a Camera2 node, and
    the points as an `.obj` beside it read by a ReadGeo), **.chan** (the plain camera path that
    Nuke, Blender, Maya and tracking apps read) or **Reelforge** (the whole solve, to load onto
    a clip in another project). With points picked, only those go. Blender, Nuke and `.chan`
    get 100 pixels to a unit (a metre in Blender), Y or Z up as each expects.
  - **Stabilise** with the solved camera: **Keep camera moves** (frames) smooths the camera's
    path and keeps moves slower than that (10–30 frames suits most handheld footage); 0 holds
    the camera still where it is at the playhead, so the shot looks locked off. The clip is
    corner-pinned every frame to the picture the steadier camera would have filmed, in
    perspective: turning shake (most handheld shake) goes completely, and moving shake goes
    for things on the surface the tracked points lie on (or at their middle depth). **Hide
    edges** zooms in once, or with the motion, so the uncovered edges never show. Undoable
  - **Import a camera…**: a `.chan` file (from Nuke, Blender, SynthEyes, 3DEqualizer,
    PFTrack…) becomes a camera clip keyed every frame, from the selected clip's start at its
    frame rate (or from the playhead), with the file's lens if it has one; a Reelforge solve
    goes onto the selected footage clip, to pick points and create layers as if it had been
    analysed here
- **3D layers, camera and lights**:
  - **3D layer** (Transform section of the Controls panel): the layer becomes a flat plane in
    3D space with a **Position Z** (positive is further away) and **Rotation X / Y** (Rotation
    turns it about Z), all keyframable and in the curve editor. Positions are in pixels from the
    frame's centre, so with the default camera a 3D layer at Z 0 looks exactly as it did in 2D.
    3D layers next to each other on the timeline are drawn furthest first; 2D layers between them
    keep their places. In the Viewer a 3D layer shows its outline as the camera sees it, and
    dragging it moves it across the camera's view (Shift: straight across or down)
  - **3D gizmo**: a selected 3D layer, 3D text, shape or model shows arrows along its *own*
    axes, X red, Y green and Z blue (they turn with it), and a ring for each of its turns. Drag
    an arrow to move it along that axis only (Z: nearer or further), and drag round a ring to
    turn it about that axis: X tilts it forwards or back, Y swings it left or right, Z spins it
    in its plane, each changing just that one Rotation in Controls (so keyframes stay tidy).
    Shift moves in whole pixels and turns in 15° steps; the readout shows the numbers. The
    gizmo keeps its size on screen, works from the Front, Top and Left views too, and an arrow
    pointing straight at the camera shows as a dot. Dragging anywhere else on the layer still
    moves it across the view
  - **Timeline → Add camera**: a camera clip on a new top track. Its settings (Controls) are its
    position, **point of interest** (what it looks at), **roll**, **field of view**, and **depth
    of field** (focus distance and how much things out of focus blur). The topmost camera clip
    at a moment is the one used; without one, a default camera that leaves 2D-looking layers
    unchanged. Its point of interest can be dragged in the Viewer
  - **Camera tools** (the bottom three buttons down the Viewer's left side): **Orbit** circles
    the camera round its point of interest as you drag, **Pan** slides the camera and its point
    of interest across the view (what's there follows the pointer), and **Dolly** moves it closer
    or further as you drag up or down, or with the wheel. They move the camera clip at the
    playhead (keyed where its position is animated; one undo step per drag); Esc leaves the
    tool
  - **Look through** (the menu in the Viewer's top bar): **Active camera** is the sequence's
    camera, what's exported. **Front**, **Top** and **Left** look at the 3D scene from those
    sides with a long lens, far enough off to take in the camera and what it looks at; there the
    camera shows as a handle you can drag, with a line to its point of interest. The camera tools
    move these views instead of the camera, and orbiting one turns it into a **Custom view**.
    Only the Viewer uses them; exports always go through the active camera
  - **Timeline → Add light**: point, spot, directional or ambient, with colour, intensity,
    position, point of interest (spot and directional), cone angle and feather (spot), and
    falloff (none, smooth or inverse square, over a distance). With any lights in the scene, 3D
    layers are shaded by them (both sides, per pixel), unless **Lit by the scene's lights** is
    unticked (for a screen or a sign that glows by itself); with none, 3D layers show as they are.
    Up to 8 lights at once. A light's position and point of interest can be dragged in the Viewer
  - **Environment light** (Timeline → Add light → Environment light, or a light's Type →
    Environment, then **Choose…** for its panorama): lights the scene from a panorama all round
    it, an equirectangular (2:1) picture, best an HDR (`.hdr` or `.exr`) from a light probe or an
    HDRI site (`.png`, `.jpg` and `.tif` work too). The middle of the picture is what the default
    camera looks at and its top is the sky. 3D layers take its light from whichever way they face
    (so a sunny sky lights what faces up, and a coloured wall tints what faces it); 3D text,
    shapes and models also *reflect* it: **Reflections** (0% matte, 100% a mirror, and more at
    grazing angles) and **Reflection blur** (sharp chrome to brushed metal), under
    *Environment*. **Environment turn** spins the panorama about the up axis; **Colour** and
    **Intensity** tint and brighten it. It casts no shadows; add a directional light for a
    sun's. The picture is read once, shrunk to 1024 × 512, and collected with the project's media
  - **Shadows**: a light with **Casts shadows** on (the default; **Shadow darkness**, 80%)
    darkens what 3D layers block of it on the lit 3D layers behind them. The blocker's own
    picture counts, so a cut-out or text casts its shape and its opacity how dark; each pixel
    traces its way to the light past the other layers' planes. Up to 25 layers (and 3D text)
    cast shadows at a time, their outlines drawn into an atlas of 5 × 5 tiles each frame. **3D
    shadow quality** in Settings sets the tiles' size: Low 256 px, Standard 512 (the default) or
    High 1024, sharper for big layers and long shadows (the atlas takes 1.6, 6.5 or 26 MB of
    graphics memory); exports use it too. **Shadow softness** (0–100%)
    gives the light a size: a shadow's edge fades over a width that grows with how far it falls
    from what casts it (sharp where it touches, blurrier further away), sampled with a turned
    disk of 24 taps so it reads as fine grain rather than steps. Extruded **3D text** casts
    shadows too: its letters' outline (not their box), from the plane halfway through its depth
    — and **takes** them: 3D layers and other 3D text between a light and the letters darken
    their faces and sides (soft shadows too), and the letters shade themselves where light
    falls through the back half of an extrusion. The text is drawn again after the shadow
    atlas each frame it's in shadow (its effects wait for that), so a shadow moving across it
    stays in step
  - **Layers that cut through each other** do: consecutive 3D layers are drawn with a depth
    buffer, so where two cross, each shows on the side where it's nearer. They're still drawn
    back to front by their middles (for see-through parts), and nearly transparent pixels
    (under 2%) don't hide what's behind them. A 2D layer, an adjustment layer or a blend mode
    that reads the frame below starts a new run. Extruded **3D text** joins the run: it's
    drawn with a depth picture of its letters (their nearest faces, and the text's middle where
    a glow or blur spills past them), so text turned through a 3D layer shows where it's in
    front and is hidden where it's behind
  - Everything about cameras and lights keyframes like effect settings
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

Glow and drop shadow reach past the clip's own edges: a layer smaller than the frame (a
logo, a picture-in-picture, a shrunken clip) casts its shadow and glows onto what's around it,
except on sides cut by a crop.
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
  ofx/     The OpenFX host: loads .ofx bundles, describes their parameters and renders
           frames with them (the suites in Rust, the C-variadic calls in a small C shim),
           with other frames for temporal plugins and an OpenGL context (EGL / WGL) for
           plugins that render on the graphics card.
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
ready-made sets, and the playhead's colour: any colour, or blue, red, orange, green, purple
or white with one click), interface colours (panels, wells, lines, text, highlight; five schemes),
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
search box). **Favourites**: the ☆ beside an effect (on hover in the Effects panel, beside
Add in the browser) stars it, and starred effects are listed first in both, in a
Favourites group, kept with the app's settings. **Recent**: the last 8 effects added to any
clip (newest first, each once) are listed next, in a Recent group in both (left out of the
browser while searching). **Pictures** (on by default, a tick box at the
browser's foot): each effect is shown on the selected clip's frame at the playhead, at its
defaults, drawn small as it scrolls into view (and again when the clip or playhead changes;
nothing in the project changes). In the **Effects panel**, hovering an effect for a moment shows a card
beside the panel with the same picture, its name and what it does (the description alone with
no clip selected, or with Pictures off). **Moving previews**: hover an effect's picture in the browser, or keep the
pointer on one in the Effects panel, and it plays the next 12 frames from the playhead over and
over (made one after another, about 11 a second; wrapping round at the clip's end), so effects
that change over time (Glitch, flicker, noise, CRT) show their motion. Frames are kept, so a
second hover plays at once. Switch effects off, reorder, duplicate,
reset and remove them, fold them shut, and edit every setting (drag or type the numbers; menus and tick boxes for plugin
choices). The ◆ beside a setting keyframes it at the playhead; once a setting has keys,
changing it sets the key there. Curves has its own curve editor (Master, Red, Green,
Blue: drag points, click the line to add one, right-click to remove), and LUT a .cube
file picker.

Presets: **+ Add → Save as preset…** (or **Save as preset…** in the Sound and Title sections)
saves a clip's look under a name, in a folder if you like. Choose what goes in: any of its
effects (all ticked to start with, each with every setting and keyframe), its **masks** (scaled
to the size of each clip they go on), its **position, scale, rotation and opacity** with their
keyframes (a slide-in, a slow push), its **blend mode**, a title's **style** (font, size,
colours, outline, shadow and background, not the words), its **audio levels** (volume, pan and
fades with their keyframes; for a video clip, its linked audio's) and the **transition into
it**. Choose where keyframes land: **from the clip's start**, **before the clip's end** (fades
and exits), or **stretched to the clip's length**. A small picture of the Viewer is kept with
it.
  - **Applying**: from the effects browser's **Presets** tab (hover a preset to see it on the
    clip first; nothing changes until you click Apply), **+ Add → Presets**, or the Effects
    panel's Presets group (double-click, or drag a preset onto a clip on the timeline). With
    several clips selected, Apply puts it on all of them as one undo step. Effects and masks go
    after the clip's own (Shift+click Apply replaces them); its motion, blend mode, title style,
    audio levels and transition replace the clip's.
  - **Organising**: ★ marks favourites (top of the list and of + Add → Presets); ⋯ renames a
    preset, moves it to a folder (or a new one), exports it or deletes it; folders fold shut.
    **Import…** and **Export yours…** read and write `.rfpreset` files, to back up or share
    (an imported name that's taken gets a number).
  - **Starter looks** come with Reelforge: Teal and orange, Black and white, Film look, Dreamy
    glow, Warm and soft, Old video (VHS), Green screen, Blue screen, Fade in, Fade out (sets
    only the opacity, over the clip's last second) and Slow push in (stretched over the clip).
    They can be starred and exported, not renamed or deleted.
  - Presets are kept in `effect-presets.json` (pictures in `preset-pictures`) in the app's
    settings folder; one that uses a plugin you no longer have says so.

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

The playhead: click or drag in the ruler to move it (it snaps to markers and edits; Alt:
free); while dragging, its time shows beside its head (HH:MM:SS:FF).

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
| OpenEXR | done, Reelforge's own: scanline and tiled, single and multi-part, compression none, RLE, ZIPS, ZIP, PIZ, PXR24, B44 and B44A; half, float and uint channels; RGB(A) or Y, of the unnamed layer or the first named one (`beauty.R`…); the data window placed in the display window. Output is floating point (see above). Chunks decode on all cores, with SSE2 for ZIP's predictor. DWAA, DWAB and HTJ2K files go to FFmpeg's decoder. Checked bit for bit against the OpenEXR reference library (`tools/exr/make_fixtures.py` makes the test files and the library's own reading of them, in `crates/codecs/tests/data/exr`). On 2 cores, a 1080p half-float RGBA frame takes about 7 ms uncompressed, 40 ms ZIP, 50 ms PIZ and 15–25 ms B44A, against FFmpeg's 14, 79, (fails on that PIZ file) and 33 ms |
| GoPro CineForm | done, Reelforge's own: 4:2:2 10-bit YUV, 12-bit RGB and RGBA, CineForm RAW (Bayer: the sensor mosaic, demosaiced like FFmpeg's), intra frames and two-frame groups (which FFmpeg can't decode), progressive and interlaced, peak tables; channels on all cores, AVX2 wavelet filters. Written from GoPro's CineForm SDK (MIT OR Apache-2.0; code tables in `crates/codecs/src/cineform_tables.rs`) |

The H.264, HEVC, VP9 and AV1 decoders live in this repository (`crates/h264`, `crates/hevc`,
`crates/vp9`, `crates/av1`, with `crates/bitstream` for reading bits and `crates/testkit` for
their tests), so building needs nothing beyond this repo. They're adapted from FilmCraft
(MIT OR Apache-2.0): see `THIRD-PARTY-NOTICES.txt`, which has to ship with the program and
any copy of the source.

**Licence notices:** `THIRD-PARTY-NOTICES.txt` lists every library built into the Windows app
(about 250 Rust crates, plus Svelte, the Tauri API and the IBM Plex Sans font in the interface)
with its licence, and gives each one's own licence texts and copyright lines; where a library
names a licence but ships no copy, the standard text (from the SPDX licence list, in
`licenses/standard/`) is added with its authors as the copyright holders. It's generated, so
rerun it when dependencies change, before building a release:
`python3 tools/notices.py` (the FilmCraft section comes from `THIRD-PARTY-NOTICES.head.txt`).
The file goes in the download beside the exe and is built into the program, under
**Help → Third-party notices**. Compile-time-only crates (macros, build tools) aren't in the
program and aren't listed; FFmpeg is fetched separately by `Get FFmpeg.bat` and has its own
GPL licence in `licenses/`. Each has its own tests: `cargo test -p reelforge-h264` (and `-hevc`,
`-vp9`, `-av1`).

Every decoder is tested against FFmpeg's output (`cargo test -p reelforge-media native`):
each frame of generated test clips is converted to RGBA both ways and must match closely
(ProRes: 65–73 dB PSNR in every profile; DNxHD/DNxHR: 64–81 dB; MPEG-2: 62–70 dB; i.e. off by
at most a rounding step of the IDCT; H.264, HEVC, VP9 and AV1 come out identical to FFmpeg's,
across 14 encoder configurations). FFmpeg's CineForm encoder only makes intra frames and FFmpeg can't decode GoPro's two-frame
groups, so small clips from GoPro's own encoder (`crates/media/tests/data/cineform`, made with
`tools/cineform`) check that each group decodes to the same pictures as intra frames of the
same source. FFmpeg's MPEG-2 encoder never makes field pictures,
16×8 or dual-prime motion, so the tests also generate random streams that use them
(`crates/codecs/src/mpeg2/testgen.rs`), and FFmpeg and Reelforge must decode them alike.
Speed, on one core of a 2.8 GHz Xeon, for 1080p with film-like grain (`taskset -c 0`, the
`bench_native_vs_ffmpeg` test in `crates/media/src/native.rs`): ProRes 422 HQ about 33 fps,
the same as FFmpeg's decoder; ProRes 422 about 48 fps (FFmpeg 46); ProRes 4444 with alpha about
11 fps, 15% faster than FFmpeg; DNxHR HQ/HQX and DNxHD 220 about 36 fps (FFmpeg 42). Slices or
macroblock rows go to every core, so a typical PC plays 4K ProRes and DNxHR in real time. What
makes them quick: a bit reader that keeps the next 64 bits in a register; one table lookup per
ProRes codeword (with the level's sign folded in) and two-level DNx code tables small enough to
stay in the CPU cache; an AVX2 inverse DCT (8×8 block in eight 256-bit registers), chosen at run
time, with a portable version for older CPUs (`REELFORGE_NO_AVX2=1` forces it: about 20–30%
slower); and every slice or row decoded straight into the picture. MPEG-2 1080i at 25 Mbit/s:
about 75 fps on one core. CineForm 1080p 4:2:2 on one core: grainy camera footage about 22 fps,
20% faster than FFmpeg; clean graphics about 78 fps (FFmpeg 90). The wavelet filters use
AVX2 where the processor has it, and each channel runs on its own core. The H.264/HEVC/VP9/AV1 decoders use all
cores; on 2 cores 1080p runs at about 75 fps (H.264, HEVC), 45 (VP9) and 28 (AV1).

**Shared decode threads:** every clip's decoder draws on one pool of worker threads, one per
core (named `decode-0`, `decode-1`…), instead of each clip starting its own. H.264 and HEVC frame
jobs, AV1's tile and filter work, and ProRes, DNx and MPEG-2 slices all go to it, so clips
playing together (layers, transitions, picture-in-picture) share the cores rather than
crowding them, and ProRes/DNx/MPEG-2 no longer start fresh threads for every frame. Jobs that
wait (an H.264 or HEVC frame waiting on rows of the frame it refers to) only ever wait on work
sent before them, and the pool starts jobs in the order sent, so sharing can't deadlock; the
`shared_threads` test runs 14 decoders of every codec at once on a pool of one thread to make
sure. VP9 keeps its own small pool per clip (its row workers wait on each other while other
work waits on them), as does AV1 for its frame threads. Files only FFmpeg decodes use FFmpeg's
own threads. A decoder that crashes is treated as a decode error: the clip carries on with
FFmpeg. With 14 clips open on 2 cores this cut the threads from 58 to 41; on an 8- or 16-core
PC, where each clip used to get up to 16, far more.

## When something goes wrong

**Help → About & diagnostics** has three tabs:
- **Diagnostics**: a report to paste into a message, with **Copy to clipboard** (also straight
  from **Help → Copy diagnostics**). It gives the Reelforge version, Windows version, processor
  (and whether it has AVX2), graphics card and driver, whether hardware decoding and zero-copy
  are working, the FFmpeg version, the decode thread count, a summary of the settings, the kinds
  of video in the project (codec and size only: no file names) and the last 200 log lines.
- **Check decoders**: makes a short sample in each format Reelforge decodes itself (H.264,
  HEVC 8 and 10-bit, VP9, AV1, MPEG-2, ProRes 422 HQ and 4444 with alpha, DNxHR HQ and HQX,
  CineForm YUV and RGBA, Motion JPEG), decodes it with Reelforge's decoder and with FFmpeg's, and compares every
  frame; on processors with AVX2, the AVX2 and portable code are both checked. Each format
  shows **Matches**, **Differs** (a worst frame below 50 dB from FFmpeg's) or **Skipped** (the
  installed FFmpeg can't encode that format). About 10–30 seconds; the results go into the
  diagnostics report.
- **Check zero-copy** (on the Check decoders tab): makes short H.264 (8-bit) and HEVC (10-bit)
  samples, decodes them with the hardware decoder keeping the frames on the graphics card, and
  draws every frame both ways, straight from the graphics card and copied back to memory as
  usual, comparing the two pictures pixel by pixel and timing each way. It shows **Works**,
  **Differs** (zero-copy then switches itself off) or why it isn't available (no hardware
  decoder, zero-copy switched off in Settings, or not Windows), and the results go into the
  diagnostics report, so a run on your own graphics card can be sent over
- **Check performance** (same tab): times the heavy work on your graphics card at 1080p:
  optical flow at Fast, Balanced and Best (working out the motion for each new pair of frames,
  and each frame drawn in between), the fluid simulation at each **Detail** (a frame's step,
  and seeking 10 seconds ahead), around 50,000 particles, and four layers with blur, glow and
  colour correction. Each line shows milliseconds and **Real time** (fits a 30 fps frame),
  **Slower** or **Heavy** (over a quarter second), with the graphics card's name; the results
  go into the diagnostics report. A few seconds on a graphics card, a few minutes on a
  software renderer
- **Third-party notices**, with a search box.

The **log** goes to `%LOCALAPPDATA%\Reelforge\logs\reelforge.log` (the previous session's is
kept as `reelforge.previous.log`; **Help → Open log folder**). It holds what the console shows:
which decoder each file got, fallbacks to FFmpeg, GPU and audio problems, and any crash with
where it happened. Each log stops growing at 8 MB. `RUST_LOG=debug` gives more detail.

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
| Ctrl+Alt+S | Save a version of the project (Project versions) |
| Ctrl+M | Export |
| Ctrl+Shift+E | Snapshot: save the frame at the playhead as a picture |
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
| 1–9 (Shift: picture only) | Multicam: cut to that angle at the playhead |
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
  inside dissolve groups. Blend modes other than Normal/Add/Multiply/Screen read the frame under
  the layer: the frame pass ends, the frame so far is copied to a backdrop texture, and the layer
  is drawn by `fs_blend` (`render/src/composite.wgsl`, one pipeline per mode via an override
  constant), which mixes `(1 − αb)·Cs + αb·B(Cb, Cs)` and draws that over the frame with the
  layer's alpha. `BlendMode::blend_rgb` in core is the reference the GPU test checks against.
  The blur radius is converted from frame pixels to texels, so a proxy-
  sized preview and a full-size export blur by the same visible amount.
- **Proxies, hardware decoding and the frame cache** live in the media crate. `hw.rs` sets a
  shared hardware device on each decoder context before it opens (`hw_device_ctx` +
  `get_format`) and copies frames back with `av_hwframe_transfer_data`; any failure reopens
  the decoder in software and carries on from the same frame. `cache.rs` is a byte-budgeted
  LRU of decoded frames keyed by (file, decode size, pts) that every preview decoder fills and
  reads. `proxy.rs` writes all-intra H.264 (`.mp4`), ProRes 422 Proxy (`-prores.mov`) or
  DNxHR LB (`-dnxhr.mov`) at ≤960×540, sampling the source once per nominal frame so proxies
  line up with variable-frame-rate originals; any format's file is picked up.
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
- **3D text** (`render/src/text3d.rs`, `render/src/solid.rs` and `solid.wgsl`): the same layout,
  but each glyph's outline is flattened into polygons (curves cut to within 0.1 px). The front
  and back faces are those polygons triangulated with the font's non-zero rule (`lyon`), so holes
  and overlapping contours come out right; the sides are a wall of quads along every contour,
  facing out (checked with a winding test), with shared normals where the outline curves (under
  40°) and separate ones at corners. The mesh is cached by the title's hash. Each frame it's
  drawn with a depth buffer and 4× multisampling into the layer's frame-sized texture, placed by
  `space3d::solid_space` (the 3D layer maths with a Z axis, scaled by the layer's average scale)
  and shaded per pixel by the same lights as 3D layers, so the picture is then layered like any
  other; it's redrawn only when the mesh, placement, camera or lights change.
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
- **Mask tracking** (`track_outline` in `media/src/tracker.rs`, `mask_from_region` in
  `core/src/track.rs`): a `PlanarTracker` started with `in_region`, so its features are only
  sought inside the mask's outline (`good_features` takes any outline, even-odd), follows the
  outline's bounding quad; each frame's quad gives a homography from the start, reduced to a
  shift or a similarity about the shape's middle for the simpler motions, and every point and
  handle end is carried by it into a shape key. **Bends** tracks a `mask_grid` (4 × 4 cells
  over the bounds, grown a tenth) with `MeshTracker::in_region`: each frame the grid is solved
  from the features inside the outline first, then features outside that have moved with it
  for `TRUST_FRAMES` frames running join in (holding the outer points); each point and handle
  end keeps its `MeshCoord` on the grid. The run uses the same `TrackJob` as point
  tracking (`mask: Some(..)`), finishing in `Editor::finish_mask_tracking` as one undo step.
- **3D camera tracking** (`core/src/camtrack.rs`, `track_scene` in `media/src/tracker.rs`,
  `engine/src/editor/camtrack.rs`): `SceneFollower` keeps about 260 `FeatureTracker`s going
  (Shi–Tomasi features, topped up as they're lost), ending a track when the feature no longer
  matches its first look (it has turned or foreshortened too far to follow without drift);
  frames wider than 1400 px are followed at half size. Positions are put out with pixel
  centres at halves, as the frame and camera have them. `solve` picks keyframes (neighbours
  sharing 70 % of features, at most about 40), ranks pairs by parallax (the twelfth largest
  residual from a robust homography fit, so a scene that's mostly one wall still shows its
  depth) and features off that plane, and decides between a moving and a turning (tripod)
  camera, trying both when it's unclear. Each start (the best two pairs) and each lens guess
  (seven fields of view) is solved: relative pose from the essential matrix (eight-point in
  RANSAC, Sampson distance), triangulation, then resection of further keyframes from their
  neighbours with robust Levenberg–Marquardt, new points triangulated with at least 1° between
  views, and bundle adjustment (Levenberg–Marquardt with a Schur complement over point
  blocks, Huber weights, one camera held) every few keyframes over the dozen nearest the new
  one, then over all of them. Candidates are compared
  by a capped error over every observation, dropped tracks and unplaced frames counting the
  cap, so a solve can't look better by throwing data away; the best three refine the lens
  (re-triangulating what was dropped) and the best wins. Every other frame is then placed,
  points refined from all frames, and frames placed again. `into_track` puts the first
  frame's camera where the default camera is, scaled so the features at middle depth sit at
  Z = 0, and turns each pose into camera settings (position, point of interest, roll measured
  as `space3d::Camera::basis` builds it). Making layers uses `CameraTrack::plane` (PCA of the
  picked points, facing the camera) and `orientation_facing`
- **3D stabilising** (`CameraTrack::stabilising_maps`): the steady path is the solved one
  smoothed with a Gaussian (positions averaged, rotation matrices averaged and made a
  rotation again), with the window kept centred near the ends so it doesn't lag, or one
  frame's camera held. Each frame's map is the homography the scene surface induces between
  the real and the steady camera: the four frame corners' rays meet the plane fitted to the
  points that frame sees (when they're near flat, else a plane facing the camera at their
  middle depth), and are projected into the steady camera. It's applied as corner-pin keys,
  with the trackers' zoom-to-cover. Tests: a shaky synthetic camera's jerk drops 27-fold with
  the move kept; held still, points stay within their depth spread (3 px)
- **Camera solves in and out** (`core/src/camtrack_io.rs`): After Effects keeps the editor's
  axes with the origin moved to the comp's corner (a two-node camera: position, point of
  interest, Z rotation for the roll, zoom = focal length in pixels); Nuke and `.chan` turn the
  world half a turn about X (Y up, cameras looking down −Z) with rotations in Nuke's default
  ZXY order (R = Ry · Rx · Rz, kept continuous between frames), focal length for a 36 mm
  aperture; Blender takes Z up and Y away and gets each frame's camera matrix to turn into
  Euler angles itself (`to_euler` with the previous frame, so they don't flip). A test
  rebuilds Nuke's camera from the exported numbers and checks every point lands on the same
  pixel; `.chan` round-trips through import
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
- **Effect presets** (`engine/src/fx_presets.rs`): `EffectPreset::from_visual` stores the
  chosen effects, masks (with the layer size they were drawn in), motion (`MOTION_PROPS` of the
  transform and their curves; crop and corner pin stay) and blend mode, every key time made
  relative to the clip's start, plus the clip's length; cameras', lights' and points' settings
  stay behind. `apply` places each key by the preset's `Timing` (the same offset, the same
  distance from the end, or scaled by the length ratio) after the target's `source_in`. Older
  presets load as effects-only, from the start. `Motion::props` limits a move to some properties
  (a fade sets only the opacity). `Editor::apply_preset` puts one on many clips as a single
  `Batch` (visual, title style, linked audio, transition); `preview_preset` changes the clip
  directly and `end_preset_preview` (also run first by `commit_live_edit`) puts it back without
  leaving it unsaved. `PresetLibrary` keeps the starters out of the file (`starters()`), only
  which are starred.
  `PresetLibrary` is the JSON file (names unique ignoring case, kept sorted, written via a
  temporary file).
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
- **YUV frames** (`core/src/frame.rs`, `render/src/yuv.rs`): the decoder keeps NV12, P010 and
  planar YUV (`YuvLayout::Planar { bits: 8–16, chroma: 4:2:0/4:2:2/4:4:4, alpha }`) as tightly
  packed planes (`Pixels::Yuv`), shrinking them YUV-to-YUV in the same layout when a preview
  size is wanted. Other formats deeper than 8 bits go through swscale's RGBA64 and a 64K-entry
  sRGB-to-linear table into `Pixels::Float`; 8-bit ones to RGBA as before. The compositor
  uploads the planes as unsigned-integer textures and one pass converts them into the layer
  texture with the frame's matrix (BT.601/709/2020) and range, interpolating subsampled chroma
  (MPEG-2 siting for 4:2:0, co-sited for 4:2:2): into an 8-bit sRGB texture with straight alpha
  for 8-bit video, or (`fs_yuv_float`, `PixelData::is_deep`) an `Rgba16Float` texture, linear and
  premultiplied, keeping values above white, for deeper video. Hardware-decoded P010 frames
  (zero-copy) take the float route too. `YuvPlanes::to_rgba` is the CPU version (thumbnails,
  proxies) and the reference the GPU is tested against (within 2/255 for every layout);
  `to_float` gives deep video to CPU work (OpenFX) without 8-bit steps. Compositions read their
  accumulation target back as half floats (`Compositor::read_output_linear`).
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
   (audio effects: EQ, compressor, limiter, reverb, noise reduction)
3. ~~Compositor and editing: layers, transforms, opacity, blend modes, viewer gizmo, keyframes
   and a curve editor, cross-dissolves, trimming, ripple/roll/slip/slide, ripple delete, track
   locking~~
   (blend modes: Normal, Add, Multiply, Screen, Overlay, Soft/Hard light, Colour dodge/burn,
   Darken, Lighten, Difference, Exclusion, Hue, Saturation, Colour, Luminosity; Bézier keyframe
   handles are in)
4. ~~Export: H.264/H.265/ProRes/DNxHR/image sequences/WAV, presets including vertical video,
   fit/fill/stretch, GPU encoders, an in/out range, an export queue, 10-bit/HDR10/HLG HEVC,
   target bitrates, your own presets~~
   (HDR footage in, PQ and HLG; 10-bit and HDR from the GPU encoders, with HDR10 metadata)
5. ~~**Effects**: the effect stack, keyframable parameters, colour correction, levels, curves,
   LUTs, blur, sharpen, chroma key, glow, drop shadow, vignette, an effects browser;
   dissolve, dip, wipe, iris, slide and push transitions~~
   (stylise effects: lens flare, light leaks, film grain, glitch, chromatic aberration,
   camera shake, energy blade, particles; glow and shadow spill past the layer; saved effect
   presets; chroma, luma and difference keys with matte clipping, choke, edge softness and a
   matte view)
6. **Compositing**: ~~compositions (nested sequences), motion tracking (1, 2 and 4
   points, planar and mesh: follow, stabilise with smoothing and edge-hiding zoom, corner pin /
   screen replacement),
   corner pin, masks with Bézier curves and per-point feather, mask tracking for rotoscoping,
   GPU particles~~, titles and track management are done.
   Animated text (typewriter, fading letters and words, sliding lines, bouncing letters), 3D
   layers, a camera and lights are in.
7. ~~**Performance**: proxy media, a frame cache, hardware decode, YUV frames converted on the
   GPU, render preview~~
   (zero-copy from the hardware decoder is in for Windows, D3D11VA to Direct3D 12, but is
   still to be tried on real hardware; VAAPI to Vulkan on Linux; the render cache is kept on
   disk too)
