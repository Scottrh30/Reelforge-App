// Neon FX: Laser Blast. One pass, premultiplied colour. Distances in layer pixels.
// Travel (param 4) drives everything: 0 % not fired, 100 % the first bolt
// reaches the target, then it sinks in while the impact flash fades.

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

// ---- The bolts ----

// A star-shaped flash at `p`: x = hot white centre, y = coloured light.
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
    let px = uv * size();
    let from_p = vec2(param(0), param(1)) / 100.0 * size();
    let to_p = vec2(param(2), param(3)) / 100.0 * size();
    let len = max(length(to_p - from_p), 1e-3);
    let dir = (to_p - from_p) / len;
    let t = time();
    let frame = floor(t * 60.0);

    let th = max(param(6), 0.0);
    let cblur = max(param(8), 0.0);
    let gr = max(param(11), 0.0);
    let flash_size = th * 3.0 + gr * 1.2;
    let bolts = clamp(i32(round(param(16))), 1, 6);
    let dens = param(21) / 100.0;
    let distort = param(22) / 100.0;

    // Where we are along the (possibly wavy) path.
    let g = nfx_beam(px, from_p, dir, len, param(18), param(19), 0.0);
    let s = dot(px - from_p, dir);
    let off = g.z;

    var core = 0.0;
    var body = 0.0;
    var glow = 0.0;
    var flare = 0.0;
    var sparks = 0.0;
    var heat = 0.0;
    var shock = vec2(0.0);
    for (var k = 0; k < bolts; k = k + 1) {
        let tk = param(4) - f32(k) * param(17);
        if (tk <= 0.0) {
            continue;
        }
        let seed = random(vec2(f32(k), 7.3));

        // The bolt: from its tail to its head along the path, thicker at the head.
        let head = min(tk / 100.0, 1.0) * len;
        let tail = clamp(tk / 100.0 * len - max(param(5), 0.0), 0.0, len);
        if (head > tail) {
            let sb = clamp(s, tail, head);
            let d = length(vec2(s - sb, off));
            let w = th * mix(0.35, 1.0, (sb - tail) / max(head - tail, 1e-3));
            let cw = w * clamp(param(7) / 100.0, 0.0, 1.0);
            core = max(core, (1.0 - smoothstep(cw * 0.5 - 0.75 - cblur, cw * 0.5 + 0.75 + cblur, d)) * clamp(cw, 0.0, 1.0));
            let bd = (1.0 - smoothstep(w * 0.5 - 0.75 - cblur * 0.5, w * 0.5 + 0.75 + cblur * 0.5, d)) * clamp(w, 0.0, 1.0);
            body = max(body, bd);
            let edge = max(d - w * 0.5, 0.0);
            glow = glow + nfx_glow(edge, gr) * mix(0.5, 1.0, (sb - tail) / max(head - tail, 1e-3)) * (1.0 - bd);
            heat = max(heat, exp(-edge / (gr + w + 1.0)));
            // Sparks shed behind the bolt, drifting outwards.
            if (dens > 0.0) {
                let behind = head - s;
                let fade = smoothstep(-th, 0.0, behind) * (1.0 - smoothstep(0.0, max(param(5), 1.0) * 1.6, behind));
                let sv = vec2(s, abs(off) - 0.15 * max(behind, 0.0));
                sparks = sparks + nfx_sparks(sv + vec2(seed * 97.0, 0.0), max(th * 2.0, 8.0), dens) * fade * exp(-abs(off) / (gr * 2.0 + 4.0));
            }
        }

        // Muzzle flash as it leaves, impact flash (and a shock ripple) as it lands.
        let muzzle = (1.0 - smoothstep(5.0, 45.0, tk)) * param(14) / 100.0;
        if (muzzle > 0.0) {
            let f = flash(px, from_p, flash_size, seed);
            core = max(core, f.x * muzzle);
            flare = flare + f.y * muzzle;
        }
        let impact = smoothstep(97.0, 101.0, tk) * (1.0 - smoothstep(104.0, 170.0, tk)) * param(15) / 100.0;
        if (impact > 0.0) {
            let f = flash(px, to_p, flash_size * 1.4, seed + 0.37);
            core = max(core, f.x * impact);
            flare = flare + f.y * impact;
            let v = px - to_p;
            let dist = length(v);
            let age = max(tk - 100.0, 0.0);
            heat = max(heat, impact * exp(-dist / (flash_size * 2.0)));
            let ring = age * flash_size * 0.06;
            shock = shock + select(vec2(0.0), v / dist, dist > 1e-3) * exp(-pow((dist - ring) / (flash_size * 0.35 + 2.0), 2.0)) * impact * 10.0;
            if (dens > 0.0) {
                sparks = sparks + nfx_sparks(v / (1.0 + age * 0.04) + vec2(seed * 53.0), max(th * 1.5, 6.0), dens) * impact * exp(-dist / (flash_size * 2.5));
            }
        }
    }

    // The picture behind, shimmering in the heat and pushed by the shock.
    let o = sample_premul(uv + (nfx_haze(px, distort * heat * 12.0) + shock * distort) / size());

    let flick = 1.0 - param(20) / 100.0 * random(vec2(frame, 4.2));
    let colour = param_color(9);
    let warm = mix(colour, from_srgb(vec3(1.0, 0.4, 0.25)) * nfx_max3(colour), 0.35);
    let hx = sqrt(max(g.x, 0.0) / max(gr * 5.0, 1.0));
    let halation = param(13) / 100.0 * 0.3 * min(glow + flare, 1.5) / (1.0 + hx);
    let cover = min(max(core, body), 1.0);
    let light = (param_color(10) * 1.6 * min(core, 1.0)
        + colour * 1.3 * max(body - core, 0.0)
        + colour * (glow + flare) * param(12) / 100.0 * (1.0 - cover)
        + warm * halation
        + mix(colour, vec3(1.0), 0.6) * sparks * 1.5) * flick;
    let a = max(max(o.a * (1.0 - cover), cover), clamp(nfx_max3(light), 0.0, 1.0));
    return vec4(o.rgb * (1.0 - cover) + light, a);
}
