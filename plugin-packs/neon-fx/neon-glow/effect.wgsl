// Neon FX: Neon Glow. Four passes, premultiplied colour:
// pick what glows, blur it across, blur it down, then light it up.

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

// ---- The glow ----

// How much of the picture at `uv` glows (0..1), from the picture as it came in.
fn glow_source(uv: vec2<f32>) -> f32 {
    let c = original(uv);
    let mode = param_choice(2);
    if (mode == 1) {
        let t = param(3) / 100.0;
        return smoothstep(t - 0.08, t + 0.08, luma(c.rgb)) * c.a;
    }
    return c.a;
}

// Pass 1: what glows, as white.
fn extract(uv: vec2<f32>) -> vec4<f32> {
    let m = glow_source(uv);
    return vec4(vec3(m), m);
}

// A Gaussian blur along `dir`, sigma = half the glow radius.
fn blur(uv: vec2<f32>, dir: vec2<f32>) -> vec4<f32> {
    let sigma = max(param(4) * 0.5, 0.01);
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

// Pass 4: the tube, its tight halo and the wide glow, over the picture.
fn combine(uv: vec2<f32>) -> vec4<f32> {
    let colour = nfx_colour(param_choice(0), param(1));
    let flick = nfx_flicker(param(8) / 100.0, param_choice(9), param(10));
    let o = original_premul(uv);
    let m = glow_source(uv);

    // Around the pixel at a small radius: how "deep inside" the source we are.
    // Inside thick shapes that makes a white-hot centre; outside, a tight halo.
    let hr = max(param(4) * 0.12, 2.0);
    var near = 0.0;
    for (var i = 0; i < 12; i = i + 1) {
        let dir = vec2(cos(f32(i) * 0.5235988), sin(f32(i) * 0.5235988));
        near = near + glow_source(uv + dir * hr * pixel()) + glow_source(uv + dir * hr * 0.5 * pixel());
    }
    near = near / 24.0;

    // The picture behind the glow.
    let keep = o * m;
    var bg = o;
    let bgmode = param_choice(11);
    if (bgmode == 1) { bg = vec4(o.rgb * 0.25, o.a) * (1.0 - m) + keep; }
    if (bgmode == 2) { bg = keep; }

    // The source itself turned into a neon tube: coloured rim, white-hot middle.
    let tube_amt = param(6) / 100.0 * m;
    let hot = smoothstep(0.75, 1.0, near);
    let tube = mix(colour * 1.3, vec3(1.25), hot * 0.85) * flick;
    let rgb = mix(bg.rgb, tube * max(o.a, m), tube_amt);
    let a = max(bg.a, tube_amt * m);

    let halo = max(near - m, 0.0) * param(7) / 100.0;
    let wide = sample_premul(uv).r * param(5) / 100.0 * (1.0 - 0.5 * m);
    let light = colour * (halo * 1.5 + wide) * flick;
    return vec4(rgb + light, max(a, clamp(nfx_max3(light), 0.0, 1.0)));
}
