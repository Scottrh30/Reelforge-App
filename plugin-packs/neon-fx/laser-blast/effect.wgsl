// Neon FX: Laser Blast. One pass, premultiplied colour.
// Travel (param 6) drives everything: 0 % not fired, 100 % the first bolt
// reaches the target, then it sinks in while the impact flash fades.

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

// ---- The bolts ----

// A star-shaped flash at `p` (pixels): x = hot white centre, y = coloured light.
fn flash(px: vec2<f32>, p: vec2<f32>, size_px: f32, seed: f32) -> vec2<f32> {
    let v = px - p;
    let dist = length(v);
    let r = max(size_px, 1.0);
    let ang = atan2(v.y, v.x);
    let blob = exp(-(dist * dist) / (r * r));
    let rays = (pow(abs(cos(ang * 3.0 + seed * 6.2831853)), 24.0) + 0.6 * pow(abs(cos(ang * 2.0 + seed * 3.7 + 1.0)), 40.0)) * exp(-dist / (r * 1.3));
    let hot = exp(-(dist * dist) / (r * r * 0.12));
    return vec2(hot, blob + 0.45 * rays);
}

fn effect(uv: vec2<f32>) -> vec4<f32> {
    let o = sample_premul(uv);
    let px = uv * size();
    let from_p = vec2(param(2), param(3)) / 100.0 * size();
    let to_p = vec2(param(4), param(5)) / 100.0 * size();
    let path = to_p - from_p;
    let len = max(length(path), 1e-3);
    let dir = path / len;

    let colour = nfx_colour(param_choice(0), param(1));
    let white = mix(colour, vec3(1.0), 0.8);
    let frame = floor(time() * 60.0);
    let flick = 1.0 - param(15) / 100.0 * random(vec2(frame, 4.2));
    let cw = max(param(8), 0.0);
    let gr = max(param(9), 0.0);
    let gi = param(10) / 100.0;
    let flash_size = cw * 3.0 + gr * 1.2;
    let bolts = clamp(i32(round(param(13))), 1, 6);

    var hot = 0.0;
    var glow = 0.0;
    for (var k = 0; k < bolts; k = k + 1) {
        let tk = param(6) - f32(k) * param(14);
        if (tk <= 0.0) {
            continue;
        }
        let seed = random(vec2(f32(k), 7.3));

        // The bolt: from its tail to its head along the path, thicker at the head.
        let head = min(tk / 100.0, 1.0) * len;
        let tail = clamp(tk / 100.0 * len - max(param(7), 0.0), 0.0, len);
        if (head > tail) {
            let a = from_p + dir * tail;
            let ab = dir * (head - tail);
            let t = clamp(dot(px - a, ab) / max(dot(ab, ab), 1e-4), 0.0, 1.0);
            let d = length(px - (a + ab * t));
            let w = cw * mix(0.35, 1.0, t);
            let core = (1.0 - smoothstep(w * 0.5 - 1.0, w * 0.5 + 1.0, d)) * clamp(w, 0.0, 1.0);
            hot = max(hot, core);
            glow = glow + nfx_glow(max(d - w * 0.5, 0.0), gr) * mix(0.5, 1.0, t) * (1.0 - core);
        }

        // Muzzle flash as it leaves, impact flash as it lands.
        let muzzle = (1.0 - smoothstep(5.0, 45.0, tk)) * param(11) / 100.0;
        if (muzzle > 0.0) {
            let f = flash(px, from_p, flash_size, seed);
            hot = max(hot, f.x * muzzle);
            glow = glow + f.y * muzzle;
        }
        let impact = smoothstep(97.0, 101.0, tk) * (1.0 - smoothstep(104.0, 170.0, tk)) * param(12) / 100.0;
        if (impact > 0.0) {
            let f = flash(px, to_p, flash_size * 1.4, seed + 0.37);
            hot = max(hot, f.x * impact);
            glow = glow + f.y * impact;
        }
    }

    let light = (white * 1.6 * min(hot, 1.0) + colour * glow * gi * (1.0 - min(hot, 1.0))) * flick;
    let cover = min(hot, 1.0);
    let a = max(max(o.a * (1.0 - cover), cover), clamp(nfx_max3(light), 0.0, 1.0));
    return vec4(o.rgb * (1.0 - cover) + light, a);
}
