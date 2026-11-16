// Neon FX: Twin Laser. One pass, premultiplied colour. Distances in layer pixels.
// Two strands wind round a centre line that can bend and wave; each pixel finds
// its nearest point on each strand, and the strand in front covers the one behind.

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

// ---- The beam's shape ----

struct Frame {
    a: vec2<f32>,
    dir: vec2<f32>,
    n: vec2<f32>,
    len: f32,
    t: f32,
    frame: f32,
};

// The centre line at `sk` pixels along: its point and its normal.
fn centre(f: Frame, sk: f32) -> array<vec2<f32>, 2> {
    let u = sk / max(f.len, 1e-3);
    let lam = max(param(19), 4.0);
    let pin = smoothstep(0.0, lam * 0.5, sk) * smoothstep(0.0, lam * 0.5, f.len - sk);
    let ang = 6.2831853 * (sk / lam - f.t * param(20));
    let bend = param(17);
    let y = bend * 4.0 * u * (1.0 - u) + param(18) * sin(ang) * pin;
    let dy = bend * 4.0 * (1.0 - 2.0 * u) / max(f.len, 1e-3) + param(18) * 6.2831853 / lam * cos(ang) * pin;
    let tan = normalize(f.dir + f.n * dy);
    return array<vec2<f32>, 2>(f.a + f.dir * sk + f.n * y, vec2(-tan.y, tan.x));
}

// Strand `i` (0 or 1) at `sk`: xy = its point, z = depth (+1 in front, -1 behind).
fn strand(f: Frame, sk: f32, i: f32) -> vec3<f32> {
    let c = centre(f, sk);
    let tl = max(param(9), 8.0);
    let phi = 6.2831853 * (sk / tl - f.t * param(10)) + i * 3.14159265;
    // The strands meet at the source and the target.
    let pinch = smoothstep(0.0, tl * 0.25, sk) * smoothstep(0.0, tl * 0.25, f.len - sk);
    var r = max(param(8), 0.0) * pinch * sin(phi);
    // Distortion makes each strand crackle on its own.
    let dist = param(23) / 100.0;
    if (dist > 0.0) {
        r = r + (nfx_noise(sk / 14.0, f.frame + i * 31.0) - 0.5) * dist * (param(7) * 0.8 + 3.0);
    }
    return vec3(c[0] + c[1] * r, cos(phi));
}

// The nearest point of strand `i` to `px`: x = distance, y = depth there.
fn nearest(f: Frame, px: vec2<f32>, i: f32) -> vec2<f32> {
    let s0 = clamp(dot(px - f.a, f.dir), 0.0, f.len);
    let span = 0.5 * max(max(param(9), param(19)), 40.0) + abs(param(17)) * 0.5;
    var step_s = span / 8.0;
    var centre_s = s0;
    var best = vec3(1e9, 0.0, s0);
    for (var round_i = 0; round_i < 2; round_i = round_i + 1) {
        for (var k = -8; k <= 8; k = k + 1) {
            let sk = clamp(centre_s + f32(k) * step_s, 0.0, f.len);
            let p = strand(f, sk, i);
            let d = length(px - p.xy);
            if (d < best.x) {
                best = vec3(d, p.z, sk);
            }
        }
        centre_s = best.z;
        step_s = step_s / 8.0;
    }
    return best.xy;
}

// One jagged lightning arc from `p0` to `p1`: distance from `px`.
fn arc_dist(px: vec2<f32>, p0: vec2<f32>, p1: vec2<f32>, seed: f32) -> f32 {
    let v = p1 - p0;
    let l = max(length(v), 1e-3);
    let nn = vec2(-v.y, v.x) / l;
    var prev = p0;
    var d = 1e9;
    for (var k = 1; k <= 7; k = k + 1) {
        let u = f32(k) / 7.0;
        var p = p0 + v * u;
        if (k < 7) {
            p = p + nn * (random(vec2(seed, f32(k))) - 0.5) * l * 0.45 * sin(u * 3.14159265);
        }
        let ab = p - prev;
        let h = clamp(dot(px - prev, ab) / max(dot(ab, ab), 1e-4), 0.0, 1.0);
        d = min(d, length(px - prev - ab * h));
        prev = p;
    }
    return d;
}

// ---- The beam ----

fn effect(uv: vec2<f32>) -> vec4<f32> {
    let px = uv * size();
    let a = vec2(param(0), param(1)) / 100.0 * size();
    let b_full = vec2(param(2), param(3)) / 100.0 * size();
    let full_len = max(length(b_full - a), 1e-3);
    let dir = (b_full - a) / full_len;
    let ext = clamp(param(4) / 100.0, 0.0, 1.0);
    var f: Frame;
    f.a = a;
    f.dir = dir;
    f.n = vec2(-dir.y, dir.x);
    f.len = full_len * ext;
    f.t = time();
    f.frame = floor(f.t * 30.0);

    let th = max(param(7), 0.0);
    let gs = max(param(11), 0.0);
    let s_raw = dot(px - a, dir);
    let along = clamp(s_raw, 0.0, f.len) / max(f.len, 1e-3);

    // Pulses running along both strands.
    let pn = i32(floor(param(14)));
    var pulse = 0.0;
    for (var k = 0; k < pn; k = k + 1) {
        let pos = fract(f32(k) / f32(pn) + f.t * param(16));
        var dd = abs(along - pos);
        dd = min(dd, 1.0 - dd);
        pulse = pulse + exp(-(dd * dd) / 0.0036);
    }
    pulse = pulse * param(15) / 100.0;

    let n0 = nearest(f, px, 0.0);
    let n1 = nearest(f, px, 1.0);
    let edge_min = max(min(n0.x, n1.x) - th * 0.5, 0.0);

    // The picture behind, shimmering in the heat.
    let distort = param(23) / 100.0;
    let heat = distort * exp(-edge_min / (gs + th + 1.0)) * step(0.001, f.len);
    let o = sample_premul(uv + nfx_haze(px, heat * 14.0) / size());
    if (f.len <= 0.0 || th <= 0.0) {
        return o;
    }

    // Each strand: hot core, coloured body, halo and glow, brighter in front.
    let near = exp(-edge_min / (gs * 1.5 + th));
    let w = th * (1.0 + 1.2 * pulse);
    var body = array<f32, 2>(0.0, 0.0);
    var light = array<vec3<f32>, 2>(vec3(0.0), vec3(0.0));
    var depth = array<f32, 2>(n0.y, n1.y);
    let hs = max(param(13), 0.0);
    for (var i = 0; i < 2; i = i + 1) {
        let nd = select(n1, n0, i == 0);
        let col = param_color(5 + i);
        let edge = max(nd.x - w * 0.5, 0.0);
        // Front/back shading only on and right round the strand.
        let lit = mix(1.0, 0.7 + 0.3 * nd.y, exp(-edge / (w + hs + 2.0))) * (1.0 + pulse * near);
        let core = (1.0 - smoothstep(w * 0.2 - 0.75, w * 0.2 + 0.75, nd.x)) * clamp(w * 0.4, 0.0, 1.0);
        let bd = (1.0 - smoothstep(w * 0.5 - 0.75, w * 0.5 + 0.75, nd.x)) * clamp(w, 0.0, 1.0);
        let halo = (1.0 - smoothstep(hs * 0.3, hs + 0.5, edge)) * step(0.01, hs);
        let glow = nfx_glow(edge, gs) * param(12) / 100.0;
        body[i] = bd;
        light[i] = (mix(col, vec3(1.0), 0.6) * 1.6 * core + col * 1.3 * max(bd - core, 0.0) + col * (halo * 0.8 + glow) * (1.0 - bd)) * lit;
    }
    // The strand in front covers the one behind where they cross.
    var front = 0;
    if (depth[1] > depth[0]) {
        front = 1;
    }
    let back = 1 - front;
    var rgb = light[back] * (1.0 - body[front]) + light[front];
    var cover = max(body[0], body[1]);

    // Lightning: jagged arcs leaping between the strands, new every frame.
    let bolt = param(22) / 100.0;
    if (bolt > 0.0) {
        let mid = mix(param_color(5), param_color(6), 0.5);
        for (var k = 0; k < 6; k = k + 1) {
            let seed = random(vec2(f.frame, f32(k) * 3.7));
            if (seed > bolt * 0.9) {
                continue;
            }
            let s0 = random(vec2(f.frame, f32(k) + 0.5)) * f.len;
            let s1 = clamp(s0 + (random(vec2(f.frame, f32(k) + 9.1)) - 0.3) * (param(8) * 3.0 + 40.0), 0.0, f.len);
            let p0 = strand(f, s0, 0.0).xy;
            let p1 = strand(f, s1, 1.0).xy;
            let d = arc_dist(px, p0, p1, seed * 91.0 + f.frame);
            let ac = (1.0 - smoothstep(0.4, 1.6, d)) * 1.0;
            let ag = nfx_glow(d, gs * 0.5 + 2.0) * 0.7;
            rgb = rgb + vec3(1.0) * ac * 1.4 + mid * ag;
            cover = max(cover, ac);
        }
    }

    // Particles drifting along the beam, in both lasers' colours.
    let dens = param(21) / 100.0;
    if (dens > 0.0) {
        let cell = max(th * 2.0, 8.0);
        let inside = smoothstep(-cell, 0.0, s_raw) * smoothstep(-cell, 0.0, f.len - s_raw);
        let off = dot(px - centre(f, clamp(s_raw, 0.0, f.len))[0], f.n);
        let sv = vec2(s_raw - f.t * 140.0 * sign(param(16) + 0.0001), off + sin(s_raw / 24.0 + f.t * 3.0) * cell * 0.3);
        let reach = max(gs, 4.0) + param(8) + th;
        let fall = exp(-(off * off) / (reach * reach));
        let sp0 = nfx_sparks(sv, cell, dens * 0.5);
        let sp1 = nfx_sparks(sv + vec2(37.0, 11.0) * cell, cell, dens * 0.5);
        rgb = rgb + (mix(param_color(5), vec3(1.0), 0.5) * sp0 + mix(param_color(6), vec3(1.0), 0.5) * sp1) * 1.5 * fall * inside;
    }

    // Source hot spot.
    let src = exp(-dot(px - a, px - a) / max(th * th * 8.0, 1.0));
    rgb = rgb + mix(mix(param_color(5), param_color(6), 0.5), vec3(1.0), 0.6) * src * 1.2;

    let a_out = max(max(o.a * (1.0 - cover), cover), clamp(nfx_max3(rgb), 0.0, 1.0));
    return vec4(o.rgb * (1.0 - cover) + rgb, a_out);
}
