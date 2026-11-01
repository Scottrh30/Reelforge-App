# Writing effect plugins for Reelforge

A Reelforge plugin is a video effect you write yourself. It runs on the graphics card
(GPU), so it's as fast as the built-in effects, and it gets everything they get:
keyframes, the Curves panel, undo, paste attributes, the Viewer, and export.

A plugin is a **folder** with two text files:

```
my-effect/
  plugin.toml     its name, category and parameters (the sliders, checkboxes and menus)
  effect.wgsl     the shader: a function that works out each pixel's colour
```

The shader language is **WGSL** (WebGPU Shading Language). It looks like Rust or
JavaScript and is simple for this kind of code. You don't need to compile anything:
Reelforge reads the files and checks them when it starts, and again every time you
save them.

## The quickest start

1. **Edit → Plugins… → New plugin…** This makes a working plugin called *My Effect* in
   your plugin folder and opens that folder.
2. Select a clip, press **E**, and add **My Effect** (under *Light & stylize*).
3. Open `effect.wgsl` in any text editor (Notepad works; VS Code with a WGSL extension
   is nicer). Change something and **save**. The Viewer updates straight away.
4. If you make a mistake, the status bar says where (`effect.wgsl, line 7: …`), and
   **Edit → Plugins…** shows the whole message. The effect is skipped until it's fixed;
   nothing crashes.

Five example plugins come with Reelforge, in the `plugins` folder next to
`reelforge.exe`: **RGB Split**, **Film Grain**, **Mosaic**, **Bloom** (four passes) and
**Wave Warp**. Reading them is the fastest way to learn.

## Where plugins live

Reelforge loads every plugin folder it finds in:

| Where | What it's for |
|---|---|
| `%APPDATA%\Reelforge\plugins` (Windows), `~/.local/share/reelforge/plugins` (Linux) | Your plugins. **Install** and **New plugin** put them here. **Edit → Plugins… → Open plugin folder** opens it. |
| `plugins` next to `reelforge.exe` | Plugins that ship with Reelforge (the examples). |
| Folders in the `REELFORGE_PLUGINS` environment variable | Handy while developing: point it at your work folder. Separate several with `;` on Windows. |

To share a plugin, zip its folder. Someone else installs it with
**Edit → Plugins… → Install from .zip…** (or **Install from folder…**). Installing a
plugin that's already there (same `id`) replaces it with the new version.

## plugin.toml

```toml
id = "scott.rgb-split"        # unique; projects remember effects by it. Never change it once used.
name = "RGB Split"            # shown in the effects browser and the Controls panel
version = "1.0"
author = "Scott"
category = "Stylize"          # Colour, Blur, Keying, Stylize, Distort; anything else is "Other plugins"
description = "Pulls the red and blue channels apart."
shader = "effect.wgsl"        # optional; this is the default
passes = ["effect"]           # optional; the shader functions to run, in order (see "Several passes")
premultiplied = false         # optional; see "Alpha"

[[param]]                     # param(0) in the shader
name = "Amount"
default = 6
min = 0
max = 100
suffix = " px"                # shown after the number
step = 0.1                    # optional: how fast it changes when you drag it

[[param]]                     # param(1)
name = "Direction"
type = "choice"               # a menu; the shader gets the option number 0, 1, 2…
options = ["Horizontal", "Vertical"]
default = 0

[[param]]                     # param(2)
name = "Wobble"
type = "toggle"               # a checkbox; the shader gets 0 or 1
default = 0
```

* Up to **16** parameters. Every one can be keyframed. Toggles and choices change at
  keyframes rather than sliding through in-between values.
* Parameters are numbered in the order they're listed, from 0.
* Don't reorder or remove parameters of a plugin that's been used in projects: saved
  projects store the values by position. Add new ones at the end.

## effect.wgsl

The shader has one function, run once for every pixel of the layer:

```wgsl
fn effect(uv: vec2<f32>) -> vec4<f32> {
    let c = sample(uv);                 // this pixel's colour
    return vec4(1.0 - c.rgb, c.a);      // inverted
}
```

* `uv` is the pixel's position: `(0, 0)` is the top-left corner of the layer and
  `(1, 1)` the bottom-right.
* Return the colour as `vec4(red, green, blue, alpha)`.
* Colours are in **linear light** (like the rest of Reelforge), and usually 0 to 1. Use
  `to_srgb` / `from_srgb` for effects that are easier to design on display values.
* The effect runs on the layer **before** its transform, so it moves, scales and
  rotates with the layer. It's applied in the order of the clip's effect list, like the
  built-in effects.

### What you can call

| Function | What it gives you |
|---|---|
| `sample(uv)` | The picture at `uv` (the layer, or the previous pass's result), straight alpha. Sample anywhere: that's how blurs, shifts and warps work. Outside 0–1 it repeats the edge pixel. |
| `sample_premul(uv)` | The same, premultiplied (colour already multiplied by alpha). |
| `original(uv)`, `original_premul(uv)` | The picture as it came into this effect, before its first pass. Only differs from `sample` in multi-pass plugins. |
| `param(i)` | Parameter `i` (from 0) as a number. |
| `param_on(i)` | A toggle parameter as `true`/`false`. |
| `param_choice(i)` | A choice parameter as the option number (`i32`, 0 = first). |
| `pixel()` | The size of one pixel in `uv` units. `uv + vec2(10.0, 0.0) * pixel()` is 10 pixels to the right. Use it for every distance, so the effect looks the same with proxies and at any resolution. |
| `size()` | The layer's size in pixels (`vec2`). `uv * size()` is the pixel position. |
| `aspect()` | Width ÷ height of the layer. |
| `time()` | Seconds into the clip's media; it keeps going as the clip plays. For grain, flicker and waves. |
| `pass_index()`, `pass_count()` | Which pass is running (from 0) and how many there are. |
| `texel()` | One texel of the texture actually being processed (bigger than `pixel()` with a proxy). Rarely needed. |
| `luma(rgb)` | Brightness of a colour. |
| `to_srgb(rgb)`, `from_srgb(rgb)` | Linear light ↔ display values. |
| `random(p)` | A repeatable random number from 0 to 1 for a `vec2` (mix in `time()` for movement). |
| `rotate(v, degrees)` | A `vec2` rotated clockwise on screen. |

Plus everything built into WGSL: `mix`, `clamp`, `smoothstep`, `sin`, `cos`, `pow`,
`length`, `dot`, `floor`, `fract`, `abs`, `min`, `max`, and so on.

**Names:** your functions can be called anything except names starting with `rf_`, which
Reelforge uses itself.

### Examples

A tint driven by a parameter:

```wgsl
fn effect(uv: vec2<f32>) -> vec4<f32> {
    let c = sample(uv);
    let warm = vec3(1.0, 0.85, 0.65);
    return vec4(mix(c.rgb, c.rgb * warm, param(0) / 100.0), c.a);
}
```

Shift the picture by a number of pixels:

```wgsl
fn effect(uv: vec2<f32>) -> vec4<f32> {
    return sample(uv - vec2(param(0), 0.0) * pixel());
}
```

Flicker over time:

```wgsl
fn effect(uv: vec2<f32>) -> vec4<f32> {
    let c = sample(uv);
    let f = 1.0 - param(0) / 100.0 * random(vec2(floor(time() * 24.0), 1.0));
    return vec4(c.rgb * f, c.a);
}
```

## Several passes

Some effects need the result of one step before the next, like a blur done across and
then down, or a glow that blurs the bright parts and then adds them back. List the
functions in `plugin.toml`:

```toml
passes = ["bright", "blur_across", "blur_down", "combine"]
```

Each function has the same shape as `effect`. In each pass, `sample` reads the previous
pass's result, and `original` reads the picture as it came into the effect. Up to 8
passes. See the **Bloom** example.

## Alpha

By default your functions return **straight** colour (not multiplied by alpha), and
`sample` gives straight colour too. That's easiest for colour effects.

Blurs and anything that averages several pixels should use premultiplied colour, or
semi-transparent edges get dark fringes. Set `premultiplied = true` in plugin.toml, use
`sample_premul` / `original_premul`, and return premultiplied colour (see **Bloom**).

## Good to know

* **Plugins can't harm your computer.** A shader runs on the graphics card and can only
  read the picture it's given; it can't open files or reach the internet.
* **Projects keep plugin effects even if the plugin is missing.** The effect is skipped
  and the Controls panel says it isn't installed; install the plugin and it's back, with
  its settings.
* **Collect project doesn't copy plugins.** Install them on the other computer too.
* **Live reload** is on by default (**Edit → Plugins…**). Turn it off if you keep plugins
  on a slow network drive.
* **Speed:** the shader runs for every pixel of every frame. A few dozen `sample` calls
  per pixel is fine; hundreds (a huge blur in one pass) will slow playback. Split big
  blurs into an across pass and a down pass, as Bloom does.
* **`uv` can go a little past 0–1.** When the layer also has a glow or a drop shadow, its
  picture has a margin round it so they can spill out, and your functions are called there
  too, with `uv` below 0 or above 1. `sample` there gives transparent; positions you work
  out in `uv` stay where they were on the layer.
* The full WGSL reference is at https://www.w3.org/TR/WGSL/; for plugins you only need
  the basics above.

## When something's wrong

| Message | What to do |
|---|---|
| `effect.wgsl, line N, column M:` and a reason | The line is shown under it. Common slips: a missing `;`, a misspelt name, or mixing `f32` and `i32` (write `2.0`, not `2`, for a decimal). |
| `the shader has no function \`effect\`` | Name the function `effect`, or list your function names in `passes`. |
| `must take one argument and return a colour` | Every pass function is `fn name(uv: vec2<f32>) -> vec4<f32>`. |
| `another plugin with the same id` | Two folders have the same `id`. Change one. |
| `plugin.toml: unknown field …` / `missing field …` | A typo in plugin.toml: compare with the example at the top. |
| The effect does nothing | Is it enabled (the tick box in the Controls panel)? Are the parameters at values that change something? Is the clip selected at a time inside it? |

## OpenFX plugins

Reelforge also runs **OpenFX** plugins: compiled image effects sold or shared as
`.ofx.bundle` folders for Resolve, Nuke, Vegas, HitFilm and others. You don't write these
here; you install them as their makers say, and Reelforge finds them.

**Where it looks** (Edit → Plugins… → OpenFX lists them):

- the standard place: `C:\Program Files\Common Files\OFX\Plugins` on Windows,
  `/Library/OFX/Plugins` on macOS, `/usr/OFX/Plugins` on Linux;
- every folder in the `OFX_PLUGIN_PATH` environment variable;
- Reelforge's own OpenFX folder (**Open OpenFX folder**): drop bundles in and **Scan again**;
- folders you add with **Add folder…** (and folders below them, three deep).

**In the effects browser** each plugin is listed under **OpenFX** by its own name, with its
group (e.g. "Sapphire Blurs") as the author line. On a clip:

- numbers, checkboxes, menus and colours become effect settings you can keyframe;
- **positions** (a plugin's X/Y points: a centre, a light, a corner) show as **%** of the
  frame, measured from the top left like Reelforge's own, and each gets a handle in the
  Viewer to drag (Shift: straight across or down). The plugin gets them its own way (a
  fraction of the frame or pixels, y up);
- other sizes a plugin gives as a fraction of the frame show as **%** of the frame;
- a colour with alpha gets a second setting, its opacity;
- text settings, buttons, custom data and hidden settings keep the plugin's defaults.

**How they run.** OpenFX effects run first, on the clip's own picture (a plane at full frame
size), in the order they're stacked; then the built-in and shader effects run on the
graphics card. The plugin sees RGBA in 32-bit float (or the depth it asks for),
premultiplied, sRGB-encoded; values above 1 from HDR or EXR footage are kept. Each effect on
each clip has its own plugin instance, so plugins that keep state between frames work.
Plugins get the time (in frames), the frame rate, the clip's frame range and the whole frame.
Adjustment layers skip OpenFX effects.

**On the graphics card.** A plugin that renders on the graphics card does, with the first of
these it supports that works on your computer:

- **CUDA** (NVIDIA cards; OpenFX 1.5): through NVIDIA's driver, nothing extra to install. The
  plugin gets its images as device memory in the card's primary context (the one the CUDA
  runtime uses) and Reelforge's CUDA stream if it takes one.
- **OpenCL** (OpenFX 1.5): through the OpenCL drivers installed (graphics card makers ship
  them). One context and in-order queue on the best device; the plugin gets OpenCL buffers or
  images, whichever it takes, and the queue, and can have its programs built for it (the
  OpenCL program suite). OpenCL on the processor (no graphics card driver) is only used for a
  plugin that has no other way to render.
- **OpenGL** (the OpenGL render suite): Reelforge makes an OpenGL context of its own (WGL on
  Windows, EGL on Linux), gives the plugin its source as textures and the output as the
  framebuffer, and reads the result back.
- **Metal** isn't offered: it's Apple's, and Reelforge doesn't run on Macs.

If one fails, the next is tried, then the processor if the plugin can use it; a plugin that
can't says so on its effect card. The effect card says what each effect rendered with
("Rendering with CUDA on NVIDIA GeForce …"). **Let plugins render on the graphics card** in
Edit → Plugins… → OpenFX switches it all off; under it the window shows what CUDA, OpenCL and
OpenGL would each run on, or why not, and plugins are tagged with the ways they can render
(and **GPU only** if they can't use the processor).

The CUDA path has been tested with a stand-in for NVIDIA's driver (the test machines have no
NVIDIA card), so it hasn't yet run real kernels; OpenCL has been tested with real kernels on
PoCL (OpenCL on the processor), and OpenGL on Mesa.

**Other frames.** A plugin that reads other frames of its clip (noise reduction, frame
blending, optical flow, motion blur from motion) is asked which it needs for each frame
(GetFramesNeeded; a plugin that doesn't say gets the frame either side, and at most 16 either
side are fetched) and gets them from the clip's media, within the media's length. Those
frames are the clip's own, before any effects above the plugin. Frames already seen are kept
for a while, so rendering forwards doesn't make the decoder jump back. Such plugins are
tagged **frames** in the OpenFX window. Compositions, titles and planes give the same picture
for every frame.

**Not offered yet**: overlays a plugin draws itself (OpenGL or the Draw suite) and custom
parameter widgets; CUDA, Metal and OpenCL rendering; and the Generator, Transition and
Retimer contexts (retiming plugins made for the Filter context work). A plugin that needs one
of these may refuse to load or render nothing; the window, or its effect card, shows its message.

**Safety.** An OpenFX plugin is a program running inside Reelforge. If one crashes Reelforge
while loading, Reelforge notices at the next start and skips that bundle (marked "skipped:
it crashed Reelforge last time", with **Try again**). The tick box **Load OpenFX plugins**
turns them all off. A project using a plugin that isn't installed still opens: the effect is
skipped and keeps its settings.

