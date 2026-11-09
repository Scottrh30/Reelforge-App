// Neon FX: Lightsaber. One pass, premultiplied colour.
// Every distance is in layer pixels (uv * size()), so it looks the same at any
// resolution and with proxies.

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

// ---- The blade ----

fn effect(uv: vec2<f32>) -> vec4<f32> {
    let o = sample_premul(uv);
    let base = select(o, vec4(0.0), param_on(14));
    let ext = clamp(param(6) / 100.0, 0.0, 1.0);
    if (ext <= 0.0) {
        return base;
    }

    let px = uv * size();
    let hilt = vec2(param(2), param(3)) / 100.0 * size();
    let tip = mix(hilt, vec2(param(4), param(5)) / 100.0 * size(), ext);
    let ab = tip - hilt;
    let len2 = max(dot(ab, ab), 1e-4);
    let t = clamp(dot(px - hilt, ab) / len2, 0.0, 1.0);
    let d = length(px - (hilt + ab * t));
    let blade_px = sqrt(len2);

    // Width: narrower towards the tip, and crackling when unstable.
    var w = max(param(7), 0.0) * (1.0 - param(11) / 100.0 * 0.7 * smoothstep(0.55, 1.0, t));
    let frame = floor(time() * 30.0);
    let unstable = param(13) / 100.0;
    var spark = 0.0;
    if (unstable > 0.0) {
        let s = t * blade_px / 7.0;
        let n = 0.65 * nfx_noise(s, frame) + 0.35 * nfx_noise(s * 3.1, frame + 5.0);
        w = w * max(1.0 + unstable * (n * 2.2 - 0.7), 0.15);
        spark = unstable * smoothstep(0.75, 1.0, nfx_noise(s * 0.6, frame + 9.0));
    }

    // Flicker: a fast per-frame shimmer plus a hum.
    let flick = 1.0 - param(12) / 100.0 * (0.55 * random(vec2(frame, 1.7)) + 0.45 * (0.5 + 0.5 * sin(time() * 37.0)));

    let colour = nfx_colour(param_choice(0), param(1));
    let core = (1.0 - smoothstep(w * 0.5 - 1.0, w * 0.5 + 1.0, d)) * clamp(w * 0.5, 0.0, 1.0);
    let edge = max(d - w * 0.5, 0.0);
    let glow = nfx_glow(edge, max(param(8), 0.0) * (1.0 + spark)) * param(9) / 100.0 * flick * (1.0 + spark);
    let hot = mix(colour, vec3(1.0), param(10) / 100.0) * 1.6 * (0.85 + 0.15 * flick);
    let light = hot * core + colour * glow * (1.0 - core);

    let a = max(max(base.a, core), clamp(nfx_max3(light), 0.0, 1.0));
    return vec4(base.rgb * (1.0 - core) + light, a);
}
