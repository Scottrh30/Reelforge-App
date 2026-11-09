// Neon FX: Laser Beam. One pass, premultiplied colour. Distances in layer pixels.

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

// Distance (pixels) from `px` to a beam from `a` along unit `dir` for `len`
// pixels, bent by a wave of `height` and `wavelength` pixels at `phase`
// (turns). Pinned at both ends. Returns (distance, position along 0..1 —
// measured on the straight line, so it's smooth everywhere — and signed
// offset from the nearest point of the bent centre line).
fn nfx_beam(px: vec2<f32>, a: vec2<f32>, dir: vec2<f32>, len: f32, height: f32, wavelength: f32, phase: f32) -> vec3<f32> {
    let n = vec2(-dir.y, dir.x);
    let s = dot(px - a, dir);
    let lam = max(wavelength, 4.0);
    // The nearest point of the bent line: search round the straight-line
    // position, coarse then fine (a sine is never more than half a wave away).
    var best_s = clamp(s, 0.0, len);
    var best_d = 1e9;
    var best_off = 0.0;
    var step_s = lam / 16.0;
    var centre = best_s;
    for (var round_i = 0; round_i < 2; round_i = round_i + 1) {
        for (var k = -8; k <= 8; k = k + 1) {
            let sk = clamp(centre + f32(k) * step_s, 0.0, len);
            let pin = smoothstep(0.0, lam * 0.5, sk) * smoothstep(0.0, lam * 0.5, len - sk);
            let p = a + dir * sk + n * height * sin(6.2831853 * (sk / lam - phase)) * pin;
            let dd = length(px - p);
            if (dd < best_d) {
                best_d = dd;
                best_s = sk;
                best_off = dot(px - p, n);
            }
        }
        centre = best_s;
        step_s = step_s / 8.0;
    }
    return vec3(best_d, clamp(s, 0.0, len) / max(len, 1e-3), best_off);
}

// Sparkling particles round a beam. `sv` is (along, across) in pixels,
// already moved with time; `cell` the spacing, `density` 0..1.
fn nfx_sparks(sv: vec2<f32>, cell: f32, density: f32) -> f32 {
    let g = sv / max(cell, 1.0);
    let id0 = floor(g);
    var acc = 0.0;
    for (var dy = -1; dy <= 1; dy = dy + 1) {
        for (var dx = -1; dx <= 1; dx = dx + 1) {
            let id = id0 + vec2(f32(dx), f32(dy));
            if (random(id + vec2(0.13, 0.71)) > density) {
                continue;
            }
            let p = id + vec2(random(id + vec2(3.1, 1.0)), random(id + vec2(5.7, 2.0)));
            let tw = 0.45 + 0.55 * sin(time() * 13.0 + random(id) * 40.0);
            let r = mix(0.05, 0.14, random(id + vec2(9.9, 4.0)));
            let q = (g - p) / r;
            acc = acc + exp(-dot(q, q)) * max(tw, 0.0);
        }
    }
    return acc;
}

// ---- The beam ----

fn effect(uv: vec2<f32>) -> vec4<f32> {
    let px = uv * size();
    let a = vec2(param(0), param(1)) / 100.0 * size();
    let b_full = vec2(param(2), param(3)) / 100.0 * size();
    let full_len = max(length(b_full - a), 1e-3);
    let dir = (b_full - a) / full_len;
    let ext = clamp(param(4) / 100.0, 0.0, 1.0);
    let len = full_len * ext;
    let t = time();
    let frame = floor(t * 60.0);
    let w0 = max(param(5), 0.0);
    let distort = param(23) / 100.0;

    // The beam's centre line: waves, plus a crackle when distorted.
    let wave_phase = t * param(19);
    var g = nfx_beam(px, a, dir, len, param(17), param(18), wave_phase);
    if (distort > 0.0) {
        let along = g.y * len;
        let jit = (nfx_noise(along / 14.0, frame) - 0.5) * distort * (w0 * 0.8 + 3.0);
        let ax = sqrt(max(g.x * g.x - g.z * g.z, 0.0));
        let off = g.z - jit;
        g = vec3(length(vec2(ax, off)), g.y, off);
    }
    let along = g.y;

    // Pulses: bright swellings running along the beam.
    let pn = i32(floor(param(13)));
    var pulse = 0.0;
    let psize = max(param(16), 0.5) / 100.0;
    for (var k = 0; k < pn; k = k + 1) {
        let pos = fract(f32(k) / f32(pn) + t * param(15));
        var dd = abs(along - pos);
        dd = min(dd, 1.0 - dd);
        pulse = pulse + exp(-(dd * dd) / (psize * psize));
    }
    pulse = pulse * param(14) / 100.0;

    // Twist: the beam splits into two strands winding round each other.
    let twist = param(20) / 100.0;
    var w = w0 * (1.0 + 1.2 * pulse) * (1.0 - 0.55 * twist);
    if (ext <= 0.0) {
        w = 0.0;
    }
    var d = g.x;
    var depth = 1.0;
    if (twist > 0.0 && w0 > 0.0) {
        let lam = max(param(18), 4.0);
        let ph = 6.2831853 * (along * len / (lam * 0.5) - wave_phase * 2.0);
        let r = (w0 * 0.9 + 2.0) * twist;
        let pin = smoothstep(0.0, 0.03, along) * smoothstep(0.0, 0.03, 1.0 - along);
        let ax = sqrt(max(g.x * g.x - g.z * g.z, 0.0));
        let d1 = length(vec2(ax, g.z - r * sin(ph) * pin));
        let d2 = length(vec2(ax, g.z + r * sin(ph) * pin));
        d = mix(d, min(d1, d2), smoothstep(0.0, 0.3, twist));
        // Strand 1 is in front when cos(ph) > 0: brighter.
        depth = 0.75 + 0.25 * cos(ph) * tanh((d2 - d1) / (w0 + 1.0));
    }

    // The picture behind, shimmering in the heat.
    let edge = max(d - w * 0.5, 0.0);
    let heat = distort * exp(-edge / (max(param(10), 1.0) + w)) * step(0.001, len);
    let o = sample_premul(uv + nfx_haze(px, heat * 14.0) / size());
    if (w <= 0.0 && len <= 0.0) {
        return o;
    }

    let flick = 1.0 - param(21) / 100.0 * (0.6 * random(vec2(frame, 4.2)) + 0.4 * (0.5 + 0.5 * sin(t * 211.0)));
    let colour = param_color(6);
    let cblur = max(param(9), 0.0);
    let cw = w * clamp(param(8) / 100.0, 0.0, 1.0);
    let core = (1.0 - smoothstep(cw * 0.5 - 0.75 - cblur, cw * 0.5 + 0.75 + cblur, d)) * clamp(cw, 0.0, 1.0) * depth;
    let body = (1.0 - smoothstep(w * 0.5 - 0.75 - cblur * 0.5, w * 0.5 + 0.75 + cblur * 0.5, d)) * clamp(w, 0.0, 1.0);
    // Pulses brighten the beam and its glow, not the faint haze far away.
    let near = exp(-edge / (max(param(10), 1.0) * 1.5 + w0));
    let bright = (1.0 + pulse * near) * flick * depth;
    let glow = nfx_glow(edge, max(param(10), 0.0) * (1.0 + 0.4 * pulse)) * param(11) / 100.0;
    let hx = edge / max(param(10) * 5.0, 1.0);
    let halation = param(12) / 100.0 * 0.4 * exp(-hx * hx * 3.0) * clamp(len / 60.0, 0.0, 1.0);
    let warm = mix(colour, from_srgb(vec3(1.0, 0.4, 0.25)) * nfx_max3(colour), 0.35);

    // A hot spot where the beam leaves its source.
    let src = exp(-dot(px - a, px - a) / max(w0 * w0 * 6.0, 1.0)) * step(0.001, len);

    // Particles drifting along and away from the beam.
    var sparks = 0.0;
    let dens = param(22) / 100.0;
    if (dens > 0.0 && len > 0.0) {
        let cell = max(w0 * 2.0, 8.0);
        let s_raw = dot(px - a, dir);
        let inside = smoothstep(-cell, 0.0, s_raw) * smoothstep(-cell, 0.0, len - s_raw);
        let sv = vec2(s_raw - t * 140.0 * sign(param(15) + 0.0001), g.z + sin(s_raw / 24.0 + t * 3.0) * cell * 0.3);
        sparks = nfx_sparks(sv, cell, dens) * exp(-abs(g.z) / (max(param(10), 4.0) * 1.5)) * inside;
    }

    let light = param_color(7) * 1.6 * core * flick * (1.0 + 0.5 * pulse)
        + colour * 1.3 * max(body - core, 0.0) * bright
        + colour * glow * (1.0 - body) * bright
        + warm * halation * flick
        + mix(colour, vec3(1.0), 0.6) * (src * 1.2 + sparks * 1.5) * flick;

    let a_out = max(max(o.a * (1.0 - body), body), clamp(nfx_max3(light), 0.0, 1.0));
    return vec4(o.rgb * (1.0 - body) + light, a_out);
}
