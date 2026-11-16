// Neon FX: Neon Outline. Four passes, premultiplied colour:
// find the lines, blur them across, blur them down, then light them up.

// ---- Neon FX helpers (the same in every effect of the pack) ----

// The neon colour: a preset, or the custom hue (degrees). Linear light.
fn nfx_colour(choice: i32, hue: f32) -> vec3<f32> {
    var pal = array<vec3<f32>, 9>(
        vec3(0.10, 0.45, 1.00), // Blue
        vec3(0.20, 1.00, 0.25), // Green
        vec3(1.00, 0.10, 0.06), // Red
        vec3(0.70, 0.20, 1.00), // Purple
        vec3(1.00, 0.85, 0.15), // Yellow
        vec3(0.10, 1.00, 0.95), // Cyan
        vec3(1.00, 0.20, 0.70), // Pink
        vec3(1.00, 0.45, 0.05), // Orange
        vec3(1.00, 1.00, 1.00)  // White
    );
    var c = clamp(abs(fract(hue / 360.0 + vec3(0.0, 2.0 / 3.0, 1.0 / 3.0)) * 6.0 - 3.0) - 1.0, vec3(0.0), vec3(1.0));
    if (choice >= 0 && choice < 9) {
        c = pal[choice];
    }
    return from_srgb(c);
}

// How bright the glow is `d` pixels from the hot core, for a glow of size `r`:
// a soft gaussian plus a long, faint tail, like light in a little haze.
fn nfx_glow(d: f32, r: f32) -> f32 {
    let x = d / max(r, 0.5);
    return 0.6 * exp(-x * x) + 0.4 / (1.0 + 4.0 * x * x);
}

// Smooth random 0..1 along x (a new shape for every seed).
fn nfx_noise(x: f32, seed: f32) -> f32 {
    let i = floor(x);
    let f = fract(x);
    let u = f * f * (3.0 - 2.0 * f);
    return mix(random(vec2(i, seed)), random(vec2(i + 1.0, seed)), u);
}

fn nfx_max3(c: vec3<f32>) -> f32 {
    return max(c.r, max(c.g, c.b));
}

// Brightness 0..1 for a flicker of `amount` (0..1): 0 a buzz, 1 a faulty
// sign that cuts out now and then, 2 a slow pulse. `speed` is per second.
fn nfx_flicker(amount: f32, style: i32, speed: f32) -> f32 {
    let t = time();
    if (amount <= 0.0) {
        return 1.0;
    }
    if (style == 1) {
        let seg = floor(t * max(speed, 0.01) * 8.0);
        if (random(vec2(seg, 11.0)) < amount * 0.45) {
            return 0.08 + 0.25 * random(vec2(seg, 3.0));
        }
        return 1.0 - amount * 0.12 * random(vec2(floor(t * 60.0), 5.0));
    }
    if (style == 2) {
        return 1.0 - amount * (0.5 - 0.5 * cos(t * speed * 6.2831853));
    }
    return 1.0 - amount * (0.35 * random(vec2(floor(t * 60.0), 2.0)) + 0.15 * (0.5 + 0.5 * sin(t * 376.99)));
}

// Draw-on and chase: which parts of a line round `centre` (pixels) show (0..1).
// Angles go clockwise from the top. `draw` is 0..1, `from_deg` where drawing
// starts; `dashes` (0 = none) dashes go round at `speed` turns a second.
fn nfx_reveal(px: vec2<f32>, centre: vec2<f32>, draw: f32, from_deg: f32, dashes: f32, speed: f32) -> f32 {
    let v = px - centre;
    let ang = degrees(atan2(v.x, -v.y));
    let f = fract((ang - from_deg) / 360.0 + 1.0);
    var vis = 1.0;
    if (draw < 1.0) {
        vis = 1.0 - smoothstep(draw - 0.004, draw + 0.0001, f);
    }
    if (dashes >= 1.0) {
        let n = floor(dashes);
        let ph = fract(f * n - time() * speed * n);
        vis = vis * (1.0 - smoothstep(0.42, 0.5, ph)) * smoothstep(0.0, 0.08, ph);
    }
    return vis;
}

// ---- The outline ----

// The value whose edges are outlined: transparency, or brightness.
fn level_at(uv: vec2<f32>) -> f32 {
    let c = original(uv);
    if (param_choice(2) == 1) {
        return luma(c.rgb) * c.a;
    }
    return c.a;
}

// Is a value above the edge level (0..1, soft)?
fn over(v: f32) -> f32 {
    let l = param(3) / 100.0;
    return smoothstep(l - 0.1, l + 0.1, v);
}

// The neon line at `uv` (0..1), before draw-on and dashes.
fn line_at(uv: vec2<f32>) -> f32 {
    let w = max(param(4), 0.5);
    let pos = param_choice(5);
    // Look round the pixel: centred lines reach half the width each way,
    // inside or outside lines the whole width into or out of the shape.
    let r = select(w, w * 0.5, pos == 0);
    let c = over(level_at(uv));
    var hi = c;
    var lo = c;
    for (var i = 0; i < 12; i = i + 1) {
        let dir = vec2(cos(f32(i) * 0.5235988), sin(f32(i) * 0.5235988));
        let a = over(level_at(uv + dir * r * pixel()));
        let b = over(level_at(uv + dir * r * 0.5 * pixel()));
        hi = max(hi, max(a, b));
        lo = min(lo, min(a, b));
    }
    if (pos == 1) { return (1.0 - c) * hi; }
    if (pos == 2) { return c * (1.0 - lo); }
    return hi * (1.0 - lo);
}

fn shown(uv: vec2<f32>) -> f32 {
    return nfx_reveal(uv * size(), 0.5 * size(), param(10) / 100.0, param(11), param(12), param(13));
}

// Pass 1: the lines, as white.
fn edges(uv: vec2<f32>) -> vec4<f32> {
    let l = line_at(uv) * shown(uv);
    return vec4(vec3(l), l);
}

// A Gaussian blur along `dir`, sigma = half the glow radius.
fn blur(uv: vec2<f32>, dir: vec2<f32>) -> vec4<f32> {
    let sigma = max(param(6) * 0.5, 0.01);
    let taps = 24.0;
    let step = 3.0 * sigma / taps;
    var sum = sample_premul(uv);
    var total = 1.0;
    for (var i = 1.0; i <= taps; i = i + 1.0) {
        let x = i * step;
        let w = exp(-0.5 * x * x / (sigma * sigma));
        let off = dir * x * pixel();
        sum = sum + w * (sample_premul(uv + off) + sample_premul(uv - off));
        total = total + 2.0 * w;
    }
    return sum / total;
}

fn blur_across(uv: vec2<f32>) -> vec4<f32> { return blur(uv, vec2(1.0, 0.0)); }
fn blur_down(uv: vec2<f32>) -> vec4<f32> { return blur(uv, vec2(0.0, 1.0)); }

// Pass 4: hot lines and their glow over the picture.
fn combine(uv: vec2<f32>) -> vec4<f32> {
    let colour = nfx_colour(param_choice(0), param(1));
    let flick = nfx_flicker(param(14) / 100.0, 0, 1.0);
    let line = line_at(uv) * shown(uv);
    let pic = original_premul(uv) * (param(9) / 100.0);
    let hot = mix(colour, vec3(1.0), param(8) / 100.0) * 1.5 * flick;
    let glow = colour * sample_premul(uv).r * param(7) / 100.0 * 2.0 * flick;
    let light = hot * line + glow * (1.0 - line);
    let a = max(max(pic.a * (1.0 - line), line), clamp(nfx_max3(light), 0.0, 1.0));
    return vec4(pic.rgb * (1.0 - line) + light, a);
}
