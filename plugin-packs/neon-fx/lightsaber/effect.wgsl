// Neon FX: Lightsaber. One pass, premultiplied colour.
// Every distance is in layer pixels (uv * size()), so it looks the same at any
// resolution and with proxies.

// ---- Neon FX helpers (the same in every effect of the pack) ----


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

// Smooth 2D random 0..1.
fn nfx_noise2(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    let a = mix(random(i), random(i + vec2(1.0, 0.0)), u.x);
    let b = mix(random(i + vec2(0.0, 1.0)), random(i + vec2(1.0, 1.0)), u.x);
    return mix(a, b, u.y);
}

// Heat-haze offset (layer pixels) at pixel `px`, `amount` pixels at most.
fn nfx_haze(px: vec2<f32>, amount: f32) -> vec2<f32> {
    let t = time();
    let q = px * 0.045 + vec2(0.0, -t * 2.2);
    let n = vec2(nfx_noise2(q) + 0.5 * nfx_noise2(q * 2.3 + 7.1), nfx_noise2(q + 13.7) + 0.5 * nfx_noise2(q * 2.3 + 3.3));
    return (n / 1.5 - 0.5) * 2.0 * amount;
}

// ---- The blade ----

fn effect(uv: vec2<f32>) -> vec4<f32> {
    let px = uv * size();
    let hilt = vec2(param(0), param(1)) / 100.0 * size();
    let ext = clamp(param(4) / 100.0, 0.0, 1.0);
    let tip = mix(hilt, vec2(param(2), param(3)) / 100.0 * size(), ext);
    let ab = tip - hilt;
    let len2 = max(dot(ab, ab), 1e-4);
    let blade_px = sqrt(len2);
    let tt = dot(px - hilt, ab) / len2;
    let t = clamp(tt, 0.0, 1.0);
    let shape = param_choice(7);

    // Distance from the blade's centre line. Rounded ends are round caps;
    // pointed and flat tips end square at the tip.
    var d = length(px - (hilt + ab * t));
    if (shape != 0 && tt > 1.0) {
        let perp = abs(dot(px - hilt, vec2(-ab.y, ab.x)) / blade_px);
        d = max(perp, (tt - 1.0) * blade_px);
    }

    // Width along the blade: hilt width to tip width, a point if wanted,
    // crackling when unstable.
    var w = mix(max(param(5), 0.0), max(param(6), 0.0), t);
    if (shape == 1) {
        w = w * (1.0 - smoothstep(0.8, 1.0, t));
    }
    let speed = max(param(20), 0.0);
    let frame = floor(time() * 30.0 * max(speed, 0.05));
    let unstable = param(21) / 100.0;
    var spark = 0.0;
    if (unstable > 0.0) {
        let s = t * blade_px / 7.0;
        let n = 0.65 * nfx_noise(s, frame) + 0.35 * nfx_noise(s * 3.1, frame + 5.0);
        w = w * max(1.0 + unstable * (n * 2.2 - 0.7), 0.15);
        spark = unstable * smoothstep(0.75, 1.0, nfx_noise(s * 0.6, frame + 9.0));
    }
    if (ext <= 0.0) {
        w = 0.0;
    }

    // Flicker: a fast shimmer plus a hum.
    let flick = 1.0 - param(19) / 100.0 * (0.55 * random(vec2(frame, 1.7)) + 0.45 * (0.5 + 0.5 * sin(time() * 37.0 * speed)));

    // The picture behind, shimmering in the blade's heat.
    let edge = max(d - w * 0.5, 0.0);
    let reach = max(param(15), 1.0) + w;
    let heat = param(22) / 100.0 * exp(-edge / reach) * step(0.001, w);
    var o = sample_premul(uv + nfx_haze(px, heat * 12.0) / size());
    if (param_on(23)) {
        o = vec4(0.0);
    }
    if (w <= 0.0) {
        return o;
    }

    // Core and blade body.
    let cblur = max(param(10), 0.0);
    let cw = w * clamp(param(9) / 100.0, 0.0, 1.0);
    let core = (1.0 - smoothstep(cw * 0.5 - 0.75 - cblur, cw * 0.5 + 0.75 + cblur, d)) * clamp(cw, 0.0, 1.0);
    let body = (1.0 - smoothstep(w * 0.5 - 0.75 - cblur * 0.5, w * 0.5 + 0.75 + cblur * 0.5, d)) * clamp(w, 0.0, 1.0);

    // Halo (a tight band round the blade), glow, and halation (a wide warm haze).
    let colour = param_color(11);
    let hs = max(param(12), 0.0);
    let hb = max(param(13), 0.0);
    let halo = (1.0 - smoothstep(hs - hb * 0.5, hs + hb * 0.5 + 0.5, edge)) * param(14) / 100.0;
    let glow = nfx_glow(edge, max(param(15), 0.0) * (1.0 + spark)) * param(16) / 100.0 * (1.0 + spark);
    let hx = edge / max(param(18), 1.0);
    let halation = param(17) / 100.0 * 0.4 * exp(-hx * hx * 3.0) * clamp(blade_px / 60.0, 0.0, 1.0);
    let warm = mix(colour, from_srgb(vec3(1.0, 0.4, 0.25)) * nfx_max3(colour), 0.35);

    let hot = param_color(8) * 1.6 * (0.85 + 0.15 * flick);
    let light = hot * core
        + colour * 1.3 * max(body - core, 0.0) * flick
        + colour * (halo * 0.9 + glow) * (1.0 - body) * flick
        + warm * halation * flick;

    let a = max(max(o.a * (1.0 - body), body), clamp(nfx_max3(light), 0.0, 1.0));
    return vec4(o.rgb * (1.0 - body) + light, a);
}
