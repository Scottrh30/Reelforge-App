// Water effects (see reelforge_core::water): the ocean (drawn here, pixel by
// pixel), and the pictures of the water surface and liquid simulations
// (watergpu.rs steps them; their state comes in as the second texture).
//
// Parameters arrive in the effect uniform in the core crate's order: 0–7 in
// p0/p1, 8–15 in p2/p3, 16–19 in p4, 20–23 in p5, 24–31 in p6/p7.
// extra = (layer width / height, time s, layer width px, height px).

struct Fx {
    p0: vec4<f32>,
    p1: vec4<f32>,
    texel_dir: vec4<f32>,
    extra: vec4<f32>,
    p2: vec4<f32>,
    p3: vec4<f32>,
    extra2: vec4<f32>,
    p4: vec4<f32>,
    map: vec4<f32>,
    p5: vec4<f32>,
    p6: vec4<f32>,
    p7: vec4<f32>,
};

@group(0) @binding(0) var<uniform> fx: Fx;
@group(1) @binding(0) var src: texture_2d<f32>;
@group(1) @binding(1) var src_sampler: sampler;
@group(2) @binding(0) var second: texture_2d<f32>;
@group(2) @binding(1) var second_sampler: sampler;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_fullscreen(@builtin(vertex_index) i: u32) -> VsOut {
    let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    var out: VsOut;
    out.pos = vec4(p * 2.0 - 1.0, 0.0, 1.0);
    out.uv = vec2(p.x, 1.0 - p.y) * fx.map.xy + fx.map.zw;
    return out;
}

fn to_tex(uv: vec2<f32>) -> vec2<f32> {
    return (uv - fx.map.zw) / fx.map.xy;
}

fn inside(uv: vec2<f32>) -> f32 {
    return select(0.0, 1.0, all(uv >= vec2(0.0)) && all(uv <= vec2(1.0)));
}

// A packed 0xRRGGBB colour → linear light.
fn colour(v: f32) -> vec3<f32> {
    let c = u32(clamp(round(v), 0.0, 16777215.0));
    let g = vec3(f32((c >> 16u) & 255u), f32((c >> 8u) & 255u), f32(c & 255u)) / 255.0;
    return select(pow((g + 0.055) / 1.055, vec3(2.4)), g / 12.92, g <= vec3(0.04045));
}

fn hash_u(x: u32) -> u32 {
    var v = x * 747796405u + 2891336453u;
    v = ((v >> ((v >> 28u) + 4u)) ^ v) * 277803737u;
    return (v >> 22u) ^ v;
}

// A random number in [0, 1) for (a, b).
fn rnd(a: u32, b: u32) -> f32 {
    return f32(hash_u(hash_u(a) ^ (b * 0x9E3779B9u)) >> 8u) / 16777216.0;
}

fn cell_rnd(i: vec2<f32>, salt: u32) -> f32 {
    let x = u32(i32(i.x) & 0xffff);
    let y = u32(i32(i.y) & 0xffff);
    return rnd(x | (y << 16u), salt);
}

// Smooth value noise in [0, 1) (foam streaks, the floor's grain).
fn vnoise(p: vec2<f32>, salt: u32) -> f32 {
    let i = floor(p);
    let f = fract(p);
    let u = f * f * (3.0 - 2.0 * f);
    let a = cell_rnd(i, salt);
    let b = cell_rnd(i + vec2(1.0, 0.0), salt);
    let c = cell_rnd(i + vec2(0.0, 1.0), salt);
    let d = cell_rnd(i + vec2(1.0, 1.0), salt);
    return mix(mix(a, b, u.x), mix(c, d, u.x), u.y);
}

fn fbm(p: vec2<f32>, salt: u32) -> f32 {
    var v = 0.0;
    var a = 0.5;
    var q = p;
    for (var i = 0u; i < 4u; i++) {
        v += a * vnoise(q, salt + i);
        q = q * 2.03 + vec2(17.1, 9.7);
        a *= 0.5;
    }
    return v / 0.9375;
}

// ---- Ocean ----------------------------------------------------------------
// p0 = (wind m/s, wind direction °, wave height %, choppiness %),
// p1 = (swell %, swell direction °, ripples %, speed %),
// p2 = (seed, camera height m, look down °, field of view °),
// p3 = (camera speed m/s, deep colour, light-through colour, foam %),
// p4 = (sun height °, sun direction °, sun strength %, sky colour),
// p5 = (horizon colour, haze %, show, opacity %), p6 = (horizon softness %,
// solid objects, key tolerance %, white water %), p7 = (spray %, wake %,
// waves bouncing off %, -). With solid objects, the second texture is the
// simulation round them (see surface.wgsl's ocean mode: height in metres,
// slope across and down a cell, solid) and extra2.xy its grid.
//
// World in metres, y up, the camera looking along +z. A direction of 0°
// comes towards the camera (waves travel along -z); 90° travels to the right.

const G = 9.81;
const WIND_WAVES = 24u;
const SWELL_WAVES = 6u;
const WAVES = 30u;
const RIPPLE_WAVES = 10u;

struct Wave {
    d: vec2<f32>,
    k: f32,
    a: f32,
    w: f32,
    phase: f32,
    q: f32,
};

fn heading(deg: f32) -> vec2<f32> {
    let r = radians(deg);
    return vec2(sin(r), -cos(r));
}

// Significant wave height of a sea raised by a wind of `u` m/s (fully developed).
fn sea_height(u: f32) -> f32 {
    return 0.21 * u * u / G;
}

fn ocean_wave(i: u32) -> Wave {
    let seed = u32(max(fx.p2.x, 0.0));
    let u = clamp(fx.p0.x, 0.5, 40.0);
    // The peak of the wind sea: a young, choppy sea (JONSWAP-like, limited
    // fetch) peaks at shorter waves than a fully developed one (ω = 0.877 g / U).
    let kp = 0.769 * G / (u * u) * 3.0;
    var w: Wave;
    if (i < WIND_WAVES) {
        let f = (f32(i) + rnd(seed, i * 3u)) / f32(WIND_WAVES);
        // From a little longer than the peak down to 1/60 of it.
        let k = kp * 0.7 * pow(60.0, f);
        let spread = (rnd(seed, i * 3u + 1u) - 0.5) * 2.0;
        w.d = heading(fx.p0.y + spread * 70.0 * (0.4 + 0.6 * f));
        w.k = k;
        // Height falling off with the spectrum above the peak, a little below it.
        let rel = k / kp;
        let shape = select(pow(rel, -1.0), exp(-1.2 * (1.0 - rel) * (1.0 - rel) * 4.0), rel < 1.0);
        w.a = shape * (1.0 - 0.5 * abs(spread));
        w.phase = rnd(seed, i * 3u + 2u) * 6.2831853;
    } else {
        let j = i - WIND_WAVES;
        // The swell: long, regular waves from far away (100–300 m).
        let len = 110.0 + 190.0 * rnd(seed, 200u + j);
        w.d = heading(fx.p1.y + (rnd(seed, 210u + j) - 0.5) * 16.0);
        w.k = 6.2831853 / len;
        w.a = 1.0;
        w.phase = rnd(seed, 220u + j) * 6.2831853;
    }
    w.w = sqrt(G * w.k);
    return w;
}

// The waves' heights, scaled so the sea has its significant height.
fn ocean_waves() -> array<Wave, WAVES> {
    var ws: array<Wave, WAVES>;
    var sum = 0.0;
    for (var i = 0u; i < WIND_WAVES; i++) {
        ws[i] = ocean_wave(i);
        sum += ws[i].a * ws[i].a;
    }
    // (a little taller than a fully developed sea: waves you can see from the shore)
    let hs = sea_height(clamp(fx.p0.x, 0.5, 40.0)) * 0.7 * max(fx.p0.z, 0.0) / 100.0;
    // Hs = 4 × the standard deviation; a sine of amplitude a has variance a²/2.
    let scale = hs / 4.0 * sqrt(2.0 / max(sum, 1e-6));
    let swell = max(fx.p1.x, 0.0) / 100.0 * 0.6;
    let chop = clamp(fx.p0.w / 100.0, 0.0, 1.5);
    for (var i = 0u; i < WAVES; i++) {
        if (i >= WIND_WAVES) {
            ws[i] = ocean_wave(i);
            ws[i].a = swell / sqrt(f32(SWELL_WAVES));
        } else {
            ws[i].a *= scale;
        }
        // Gerstner steepness: crests sharpen, but never loop over.
        ws[i].q = chop * 2.5 / (ws[i].k * max(ws[i].a, 1e-4) * f32(WAVES));
        ws[i].q = min(ws[i].q, 1.0 / (ws[i].k * max(ws[i].a, 1e-4) * 8.0));
    }
    return ws;
}

fn chop_now() -> f32 {
    return clamp(fx.p0.w / 100.0, 0.0, 1.5);
}

// How much of a wave of number `k` a pixel `fp` metres across can show (no shimmer).
fn lod(k: f32, fp: f32) -> f32 {
    let len = 6.2831853 / k;
    return clamp(len / (fp * 4.0) - 0.5, 0.0, 1.0);
}

struct Sea {
    pos: vec3<f32>,
    n: vec3<f32>,
    jac: f32,
};

// The displaced surface point from undisplaced position p (and its normal and folding).
fn sea_at(ws: ptr<function, array<Wave, WAVES>>, p: vec2<f32>, t: f32, fp: f32, full: bool) -> Sea {
    var o = vec3(p.x, 0.0, p.y);
    var nx = 0.0;
    var nz = 0.0;
    var ny = 1.0;
    var jxx = 1.0;
    var jzz = 1.0;
    var jxz = 0.0;
    for (var i = 0u; i < WAVES; i++) {
        let w = (*ws)[i];
        let f = lod(w.k, fp);
        if (f <= 0.0) {
            continue;
        }
        let a = w.a * f;
        let th = w.k * dot(w.d, p) - w.w * t + w.phase;
        let c = cos(th);
        let s = sin(th);
        o.x += w.q * a * w.d.x * c;
        o.z += w.q * a * w.d.y * c;
        o.y += a * s;
        if (full) {
            let wa = w.k * a;
            nx -= w.d.x * wa * c;
            nz -= w.d.y * wa * c;
            ny -= w.q * wa * s;
            jxx -= w.q * wa * s * w.d.x * w.d.x;
            jzz -= w.q * wa * s * w.d.y * w.d.y;
            jxz -= w.q * wa * s * w.d.x * w.d.y;
        }
    }
    var out: Sea;
    out.pos = o;
    out.n = normalize(vec3(nx, ny, nz));
    out.jac = jxx * jzz - jxz * jxz;
    return out;
}

// Fine ripples on top (normals only): short waves blown by the wind.
fn ripples(p: vec2<f32>, t: f32, fp: f32) -> vec3<f32> {
    let amt = max(fx.p1.z, 0.0) / 100.0;
    let seed = u32(max(fx.p2.x, 0.0)) + 991u;
    var n = vec2(0.0);
    for (var i = 0u; i < RIPPLE_WAVES; i++) {
        let len = 0.08 * pow(25.0, f32(i) / f32(RIPPLE_WAVES - 1u));
        let k = 6.2831853 / len;
        let f = lod(k, fp);
        if (f <= 0.0) {
            continue;
        }
        let d = heading(fx.p0.y + (rnd(seed, i) - 0.5) * 140.0);
        let th = k * dot(d, p) - sqrt(G * k + 0.074 * k * k * k / 1000.0) * t + rnd(seed, i + 40u) * 6.2831853;
        // slope ≈ 0.08 for every ripple
        n += d * cos(th) * 0.075 * f;
    }
    return vec3(-n.x * amt, 0.0, -n.y * amt);
}

fn sun_dir() -> vec3<f32> {
    let e = radians(clamp(fx.p4.x, -10.0, 90.0));
    let a = radians(fx.p4.y);
    return vec3(sin(a) * cos(e), sin(e), cos(a) * cos(e));
}

// The sky in direction `d` (linear light): blue overhead, pale at the horizon, the sun.
fn sky_at(d: vec3<f32>) -> vec3<f32> {
    let s = sun_dir();
    let sky = colour(fx.p4.w);
    let hor = colour(fx.p5.x);
    let up = clamp(d.y, 0.0, 1.0);
    let day = clamp(s.y * 4.0 + 0.6, 0.15, 1.0);
    var c = mix(hor, sky, pow(up, 0.45)) * day;
    let sun = max(fx.p4.z, 0.0) / 100.0;
    let cs = max(dot(d, s), 0.0);
    // A warmer sun near the horizon.
    let tint = mix(vec3(1.0, 0.55, 0.3), vec3(1.0, 0.96, 0.9), clamp(s.y * 3.0, 0.0, 1.0));
    c += tint * sun * (smoothstep(0.99996, 0.99999, cs) * 40.0 + pow(cs, 900.0) * 1.2 + pow(cs, 40.0) * 0.25 + pow(cs, 5.0) * 0.12 * (1.0 - up));
    return c;
}

@fragment
fn fs_ocean(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    let ins = inside(in.uv);
    if (ins == 0.0) {
        return c;
    }
    let aspect = max(fx.extra.x, 1e-3);
    let t = fx.extra.y * max(fx.p1.w, 0.0) / 100.0;
    let tan_half = tan(radians(clamp(fx.p2.w, 10.0, 120.0)) * 0.5);
    let pitch = radians(clamp(fx.p2.z, -20.0, 89.0));
    // The ray through this pixel.
    let sx = (in.uv.x * 2.0 - 1.0) * aspect * tan_half;
    let sy = (1.0 - in.uv.y * 2.0) * tan_half;
    let fwd = vec3(0.0, -sin(pitch), cos(pitch));
    let up = vec3(0.0, cos(pitch), sin(pitch));
    let d = normalize(fwd + vec3(sx, 0.0, 0.0) + up * sy);
    // (never down among the waves: at least a little above the highest crests)
    let cam_y = max(fx.p2.y, sea_height(clamp(fx.p0.x, 0.5, 40.0)) * 0.7 * max(fx.p0.z, 0.0) / 100.0 * 0.8 + 0.5);
    let cam = vec3(0.0, cam_y, fx.p3.x * fx.extra.y);
    // Radians a pixel spans (for how fine a wave can still be shown).
    let pix = 2.0 * tan_half / max(fx.extra.w, 1.0);
    // The horizon is where the ray runs level: below it the sea, above it the sky.
    let pix_dy = pix * 1.2;
    let soft = max(fx.p6.x, 0.0) / 100.0;
    let sea_a = clamp((-d.y) / pix_dy + 0.5, 0.0, 1.0);
    let sky = sky_at(d);
    var water = vec3(0.0);
    if (d.y < pix_dy) {
        var ws = ocean_waves();
        let dy = min(d.y, -pix_dy * 0.5);
        // Where the ray meets the mean surface, then refined against the waves.
        var tt = cam_y / -dy;
        tt = min(tt, 20000.0);
        let fp = tt * pix / max(-dy, 0.02);
        var p = cam.xz + d.xz * tt;
        for (var it = 0u; it < 3u; it++) {
            // The undisplaced point whose wave lands here.
            let hit = cam.xz + d.xz * tt;
            let off = sea_at(&ws, p, t, fp, false).pos;
            p = p + (hit - off.xz);
            let y = sea_at(&ws, p, t, fp, false).pos.y;
            let nt = (y - cam_y) / dy;
            tt = clamp(mix(tt, nt, 0.8), tt * 0.5, tt * 1.5);
        }
        let sea = sea_at(&ws, p, t, fp, true);
        let r = ripples(p, t, fp);
        // The waves the objects send back (and the wake), from the simulation.
        var simn = vec3(0.0);
        var sim = vec4(0.0);
        if (objects_on()) {
            sim = surf(in.uv);
            let cell_px = max(fx.extra.z, 1.0) / max(fx.extra2.x, 4.0);
            let mx = max(tt * pix * cell_px, 1e-4);
            let mz = max(fp * cell_px, 1e-4);
            // The slopes smoothed over a cell or so (detail at the grid's own
            // size is the stepping's, not the water's).
            let tg = 1.0 / max(fx.extra2.xy, vec2(4.0));
            var sl = sim.yz * 0.4;
            for (var k = 0; k < 4; k++) {
                let o = array<vec2<f32>, 4>(vec2(1.0, 0.0), vec2(-1.0, 0.0), vec2(0.0, 1.0), vec2(0.0, -1.0))[k];
                sl += surf(in.uv + o * tg).yz * 0.15;
            }
            // (down the picture is towards the camera: -z)
            simn = vec3(-sl.x / mx, 0.0, sl.y / mz);
            // Right at an object's edge is where the sea's waves are cancelled:
            // that band shapes the waves sent out, but isn't itself shown.
            let tx = 1.0 / max(fx.extra2.xy, vec2(4.0));
            var edge = 0.0;
            for (var k = 0; k < 4; k++) {
                let o = array<vec2<f32>, 4>(vec2(1.0, 0.0), vec2(-1.0, 0.0), vec2(0.0, 1.0), vec2(0.0, -1.0))[k];
                edge = max(edge, obj(surf(in.uv + o * tx * 1.5)));
            }
            simn *= 1.0 - edge;
        }
        let n = normalize(sea.n + r + simn);
        let v = -d;
        let ndv = max(dot(n, v), 0.0);
        let fres = 0.02 + 0.98 * pow(1.0 - ndv, 5.0);
        var rd = reflect(d, n);
        rd.y = abs(rd.y);
        let refl = sky_at(rd);
        let s = sun_dir();
        let sun = max(fx.p4.z, 0.0) / 100.0;
        // The body of the water: deep colour, lit by the sky, and light through the crests.
        let hs = max(sea_height(clamp(fx.p0.x, 0.5, 40.0)) * 0.7 * max(fx.p0.z, 0.0) / 100.0, 0.05);
        let crest = clamp(sea.pos.y / (hs * 1.5) + 0.2, 0.0, 1.2);
        let back = pow(max(dot(normalize(vec3(d.x, 0.0, d.z)), normalize(vec3(s.x, 0.0, s.z))), 0.0), 2.0) * 0.6 + 0.4;
        let light = clamp(s.y + 0.3, 0.2, 1.0);
        // (faces turned up to the sky a little lighter)
        var body = colour(fx.p3.y) * light * (0.75 + 0.5 * n.y) + colour(fx.p3.z) * crest * crest * back * 0.35 * light;
        var col = mix(body, refl, fres);
        // Glints of the sun (sharper on near, smoother water).
        let h = normalize(s + v);
        let rough = clamp(0.045 + fp * 0.02, 0.045, 0.35);
        // (a rougher, farther surface spreads the sun's glints thinner)
        let spec = pow(max(dot(n, h), 0.0), 2.0 / (rough * rough)) * fres * 40.0 * pow(0.045 / rough, 2.0) * sun * select(1.0, 0.0, s.y < 0.0);
        col += vec3(1.0, 0.96, 0.9) * spec;
        // Foam where the crests fold over, in streaks.
        let foam_amt = max(fx.p3.w, 0.0) / 100.0;
        // (where the surface folds over: the crests breaking)
        let fold = smoothstep(0.85, 0.45, sea.jac) + smoothstep(0.6, 1.1, sea.pos.y / max(hs, 0.05)) * 0.5 * chop_now();
        let streak = fbm(p * 1.1 + vec2(t * 0.1, 0.0), 31u) * 0.6 + fbm(p * 4.3, 37u) * 0.4;
        let foam = clamp(fold * smoothstep(0.35, 0.75, streak) * 1.6 * foam_amt, 0.0, 1.0) * lod(6.2831853 / 2.0, fp);
        col = mix(col, vec3(0.85, 0.88, 0.9) * (light * 0.8 + 0.2), foam);
        if (objects_on()) {
            // White water where the sea meets the objects (churned harder as
            // the waves hit), and foam left in the wake.
            let near = object_near(in.uv);
            let churn = clamp(abs(sim.x) / max(hs, 0.05) * 1.5, 0.0, 1.5);
            let streak2 = fbm(p * 2.3 + vec2(0.0, t * 0.4), 53u);
            // (in clumps that churn as the waves come in, not an even band)
            let clump = fbm(p * 5.1 + vec2(t * 0.7, -t * 0.5), 57u);
            let white = clamp(near * (0.35 + churn) * smoothstep(0.42, 0.72, streak2 * 0.6 + clump * 0.6 + churn * 0.25) * 1.4 * max(fx.p6.w, 0.0) / 100.0, 0.0, 1.0);
            // The foam a moving object leaves along its track, in streaks
            // (smoothed over a few cells: it's laid down cell by cell).
            let tg = 1.0 / max(fx.extra2.xy, vec2(4.0));
            var churned = 0.0;
            for (var k = 0; k < 9; k++) {
                let o = vec2(f32(k % 3) - 1.0, f32(k / 3) - 1.0) * 1.5;
                churned += max(surf(in.uv + o * tg).w, 0.0) / 9.0;
            }
            // (lacy: bright where the churned water's thick, open where it's thinning)
            let lace = smoothstep(0.42, 0.78, streak2 * 0.6 + clump * 0.6 + churned * 0.3);
            let trail = clamp(churned * lace * 1.8 * max(fx.p7.y, 0.0) / 100.0 * (1.0 - near * 0.5), 0.0, 1.0);
            // (foam is white lit by the whole sky, not only the sun: never a dull grey)
            col = mix(col, vec3(0.93, 0.95, 0.97) * (0.62 + 0.38 * light), max(white, trail));
        }
        // Haze towards the horizon.
        let haze = max(fx.p5.y, 0.0) / 100.0;
        let far = 1.0 - exp(-tt * haze * 0.0012);
        let edge = clamp(1.0 - (-d.y) / (0.002 + soft * 0.08), 0.0, 1.0);
        col = mix(col, colour(fx.p5.x) * clamp(s.y * 4.0 + 0.6, 0.15, 1.0), clamp(far + edge * edge * soft, 0.0, 1.0));
        water = col;
    }
    let show = u32(round(fx.p5.z));
    let o = clamp(fx.p5.w / 100.0, 0.0, 1.0);
    var out: vec4<f32>;
    switch show {
        // The water alone (the sky left clear, for a sky of your own).
        case 1u: { out = vec4(water * sea_a, sea_a); }
        // The layer in front (its clear parts show the sea and sky).
        case 2u: {
            let scene = vec4(mix(sky, water, sea_a), 1.0);
            out = c + scene * (1.0 - c.a);
        }
        // The solid objects (keyed from the layer) in the sea.
        case 3u: {
            let scene = vec4(mix(sky, water, sea_a), 1.0);
            let kind = u32(round(fx.p6.y));
            let a = select(c.a, solidity(c, kind, clamp(fx.p6.z / 100.0, 0.0, 1.0)), kind != 0u);
            let obj = vec4(select(vec3(0.0), c.rgb / c.a, c.a > 1e-4) * a, a);
            out = obj + scene * (1.0 - a);
        }
        default: { out = vec4(mix(sky, water, sea_a), 1.0); }
    }
    // Spray thrown up where the waves hit the objects (over them too).
    if (objects_on()) {
        let sp = spray(in.uv);
        out = vec4(mix(out.rgb, vec3(0.92, 0.95, 0.97) * clamp(sun_dir().y + 0.5, 0.35, 1.0) * sp, sp), max(out.a, sp));
    }
    return mix(c, out, o);
}

// The simulation picture's last channel: an object there (1) or not.
fn obj(v: vec4<f32>) -> f32 {
    return select(0.0, 1.0, v.w < -0.5);
}

// The ocean has a simulation round solid objects (see above).
fn objects_on() -> bool {
    return fx.extra2.x > 0.5 && u32(round(fx.p6.y)) != 0u;
}

// How close the sea at uv is to an object's waterline (1 right at it,
// fading out a few cells off): an object above it in the picture, or beside
// it a little above (the sea meets an object along its bottom edge; its top
// edge is against the sea behind).
fn object_near(uv: vec2<f32>) -> f32 {
    let tx = 1.0 / max(fx.extra2.xy, vec2(4.0));
    var near = 0.0;
    for (var k = 0; k < 5; k++) {
        // 200° to 340°: up, and up to either side
        let a = radians(200.0 + f32(k) * 35.0);
        let o = vec2(cos(a), sin(a));
        near = max(near, obj(surf(uv + o * tx * 1.5)));
        near = max(near, obj(surf(uv + o * tx * 3.5)) * 0.6);
    }
    return near;
}

// Spray at uv: drops flung up from water churning against an object below.
fn spray(uv: vec2<f32>) -> f32 {
    let amt = max(fx.p7.x, 0.0) / 100.0;
    if (amt <= 0.0) {
        return 0.0;
    }
    let tx = 1.0 / max(fx.extra2.xy, vec2(4.0));
    let hs = max(sea_height(clamp(fx.p0.x, 0.5, 40.0)) * 0.7 * max(fx.p0.z, 0.0) / 100.0, 0.05);
    var dense = 0.0;
    for (var j = 1; j <= 8; j++) {
        let q = uv + vec2(0.0, f32(j) * tx.y * 1.5);
        let s = surf(q);
        if (obj(s) > 0.5) {
            continue; // the object itself
        }
        let edge = max(max(obj(surf(q + vec2(tx.x * 1.5, 0.0))), obj(surf(q - vec2(tx.x * 1.5, 0.0)))), obj(surf(q - vec2(0.0, tx.y * 1.5))));
        let churn = clamp(abs(s.x) / hs * 1.5 - 0.15 + max(s.w, 0.0) * 0.6, 0.0, 1.5);
        // (higher up, thinner)
        dense += edge * churn * (1.0 - f32(j) / 9.0);
    }
    // Drops: specks that change every frame of 24 a second.
    let px = max(fx.extra.zw, vec2(1.0));
    let frame = u32(max(fx.extra.y, 0.0) * 24.0);
    let speck = smoothstep(0.7, 0.9, cell_rnd(floor(uv * px / 2.0), frame * 7u + 3u));
    let mist = fbm(uv * px / 24.0 + vec2(0.0, fx.extra.y * 3.0), 61u);
    return clamp(dense * amt * 1.6 * (speck * 0.9 + mist * 0.4), 0.0, 1.0);
}

// How solid a colour is, by the chosen key (see surface.wgsl): for crisp
// edges, the effect keys the layer again at its own resolution.
fn solidity(c: vec4<f32>, kind: u32, tol: f32) -> f32 {
    let al = c.a;
    let rgb = select(vec3(0.0), c.rgb / c.a, c.a > 1e-4);
    let soft = 0.04 + tol * 0.25;
    switch kind {
        case 1u: { return smoothstep(0.5 - soft, 0.5 + soft, al); }
        case 2u: {
            let k = rgb.g - max(rgb.r, rgb.b);
            return al * (1.0 - smoothstep(0.08 + tol * 0.2 - soft * 0.5, 0.08 + tol * 0.2 + soft * 0.5, k));
        }
        case 3u: {
            let k = rgb.b - max(rgb.r, rgb.g);
            return al * (1.0 - smoothstep(0.08 + tol * 0.2 - soft * 0.5, 0.08 + tol * 0.2 + soft * 0.5, k));
        }
        case 4u: {
            let l = dot(rgb, vec3(0.2126, 0.7152, 0.0722));
            return al * (1.0 - smoothstep(0.15 + tol * 0.4 - soft, 0.15 + tol * 0.4 + soft, l));
        }
        case 5u: {
            let l = dot(rgb, vec3(0.2126, 0.7152, 0.0722));
            return al * smoothstep(0.85 - tol * 0.4 - soft, 0.85 - tol * 0.4 + soft, l);
        }
        default: { return 0.0; }
    }
}

fn lum(c: vec3<f32>) -> f32 {
    return dot(c, vec3(0.2126, 0.7152, 0.0722));
}

// ---- Water surface --------------------------------------------------------
// The simulation's picture (second): height, slope across, slope down (cells),
// solid. p3 = (detail, wave scale %, seed, solid objects), p4 = (tolerance,
// wake, the layer, water colour), p5 = (depth %, refraction %, caustics %,
// reflections %), p6 = (light direction °, light height °, highlights %, sky),
// p7 = (floor colour, output, opacity %, -); extra2.xy = grid size.

fn surf(uv: vec2<f32>) -> vec4<f32> {
    return textureSampleLevel(second, second_sampler, uv, 0.0);
}

@fragment
fn fs_water(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    let ins = inside(in.uv);
    if (ins == 0.0) {
        return c;
    }
    let size = max(fx.extra.zw, vec2(1.0));
    let grid = max(fx.extra2.xy, vec2(4.0));
    let tx = 1.0 / grid;
    let scale = max(fx.p3.y, 0.0) / 100.0 * 1.6;
    let s0 = surf(in.uv);
    let slope = s0.yz * scale;
    let n = normalize(vec3(-slope.x, -slope.y, 1.0));
    // Caustics: light bent by the surface bunches up where it curves (the
    // floor's patch of light under a cell shrinks or grows).
    let hx = (surf(in.uv + vec2(tx.x, 0.0)).yz - surf(in.uv - vec2(tx.x, 0.0)).yz) * 0.5 * scale;
    let hy = (surf(in.uv + vec2(0.0, tx.y)).yz - surf(in.uv - vec2(0.0, tx.y)).yz) * 0.5 * scale;
    let refr = max(fx.p5.y, 0.0) / 100.0;
    let deep = 6.0 * max(refr, 0.15);
    let det = (1.0 + deep * hx.x) * (1.0 + deep * hy.y) - deep * deep * hx.y * hy.x;
    let caustic = clamp(1.0 / max(abs(det), 0.25), 0.0, 4.0) - 1.0;
    let caus = caustic * max(fx.p5.z, 0.0) / 100.0;
    // What's under the water, seen through it (bent by the slope).
    let cell_px = size.x / grid.x;
    let off = -n.xy * refr * cell_px * 4.0 / size;
    let layer_mode = u32(round(fx.p4.z));
    let kind = u32(round(fx.p3.w));
    var under: vec3<f32>;
    if (layer_mode == 0u) {
        let b = textureSample(src, src_sampler, to_tex(in.uv + off));
        under = mix(colour(fx.p7.x), b.rgb / max(b.a, 1e-4), b.a);
    } else {
        let p = (in.uv + off) * size;
        under = colour(fx.p7.x) * (0.8 + 0.4 * fbm(p / 38.0, 5u));
    }
    let depth = clamp(fx.p5.x / 100.0, 0.0, 1.0);
    let tint = colour(fx.p4.w);
    // Light on the floor (with its caustics), then absorbed and scattered by the water.
    let lit = under * clamp(1.0 + caus * 0.9, 0.2, 4.0);
    var col = lit * mix(vec3(1.0), tint * 1.8, depth) + tint * depth * 0.12;
    // The sky reflected (more on slopes), and the light's highlights.
    let fres = 0.02 + 0.98 * pow(1.0 - n.z, 5.0);
    let refl = clamp(fres * max(fx.p5.w, 0.0) / 100.0 * 6.0 + max(fx.p5.w, 0.0) / 100.0 * 0.05, 0.0, 1.0);
    let sky = colour(fx.p6.w);
    col = mix(col, sky, refl);
    let a = radians(fx.p6.x);
    let e = radians(clamp(fx.p6.y, 5.0, 90.0));
    let l = vec3(cos(a) * cos(e), -sin(a) * cos(e), sin(e));
    let hv = normalize(l + vec3(0.0, 0.0, 1.0));
    let hl = max(fx.p6.z, 0.0) / 100.0;
    let spec = (pow(max(dot(n, hv), 0.0), 400.0) * 2.5 + pow(max(dot(n, hv), 0.0), 40.0) * 0.08) * hl;
    col += vec3(spec);
    // Solid objects: the layer shown on top, or left out (keyed out).
    var sol = 0.0;
    if (kind != 0u && layer_mode != 0u) {
        sol = solidity(c, kind, clamp(fx.p4.x / 100.0, 0.0, 1.0));
    }
    let only = u32(round(fx.p7.y)) == 1u;
    var out: vec4<f32>;
    if (only) {
        // The water's own light: tint, highlights, reflections and caustic light,
        // to lay over other footage.
        let glow = tint * depth * 0.5 + vec3(spec) + sky * refl * 0.5 + max(caus, 0.0) * 0.25 * colour(fx.p7.x);
        let al = clamp(depth * 0.5 + lum(vec3(spec) + sky * refl * 0.5) + max(caus, 0.0) * 0.12, 0.0, 1.0);
        out = vec4(glow, al);
    } else {
        out = vec4(col, 1.0);
    }
    if (layer_mode == 1u) {
        out = mix(out, c, sol);
    } else if (layer_mode == 2u) {
        out = out * (1.0 - sol);
    }
    return mix(c, out, clamp(fx.p7.z / 100.0, 0.0, 1.0));
}

// ---- Liquid ---------------------------------------------------------------
// The simulation's picture (second, twice as fine as its grid): water density
// (0.5 at the surface), foam, solid. p4 = (solid objects, tolerance, bounce,
// water colour), p5 = (clearness %, refraction %, highlights %, foam %),
// p6 = (smoothness %, light direction °, show, opacity %), p7 = (depth px,
// view from above °); extra2.xy = grid. With depth, texel_dir says where it's
// seen from: w 1, a 3D layer's camera (xyz, its own pixels from its centre,
// z in front); w 2, a 2D layer seen from above (y: how far down the sight
// goes for each pixel back).

fn liq(uv: vec2<f32>, r: vec2<f32>) -> vec2<f32> {
    let a0 = textureSampleLevel(second, second_sampler, uv, 0.0).xy;
    let a1 = textureSampleLevel(second, second_sampler, uv + vec2(r.x, r.y), 0.0).xy;
    let a2 = textureSampleLevel(second, second_sampler, uv + vec2(-r.x, r.y), 0.0).xy;
    let a3 = textureSampleLevel(second, second_sampler, uv + vec2(r.x, -r.y), 0.0).xy;
    let a4 = textureSampleLevel(second, second_sampler, uv + vec2(-r.x, -r.y), 0.0).xy;
    return (a0 * 2.0 + a1 + a2 + a3 + a4) / 6.0;
}

fn shape(f: f32) -> f32 {
    return smoothstep(0.25, 0.95, f) * 1.6;
}

// Where the sight through layer point uv goes, for each layer pixel further
// back (layer pixels across and down).
fn sight_slant(uv: vec2<f32>) -> vec2<f32> {
    let size = max(fx.extra.zw, vec2(1.0));
    if (fx.texel_dir.w > 1.5) {
        return vec2(0.0, fx.texel_dir.y);
    }
    let p = (uv - 0.5) * size;
    return (p - fx.texel_dir.xy) / max(fx.texel_dir.z, 1.0);
}

// The water as a body `depth` deep behind the layer: the sight from the
// camera through uv, followed back through it. Returns (how far back it
// first meets water, in layer pixels, or -1 if it doesn't; and how much the
// face it meets is its top, 1, rather than a side, 0).
fn through_body(uv: vec2<f32>, depth: f32) -> vec2<f32> {
    let size = max(fx.extra.zw, vec2(1.0));
    let slant = sight_slant(uv);
    let steps = 64;
    // (each pixel's steps start a little differently: no bands where they fall)
    let jitter = cell_rnd(floor(uv * size), 91u);
    var prev = 0.0;
    for (var k = 1; k <= steps; k++) {
        let z = depth * (f32(k) - jitter) / f32(steps);
        let q = uv + slant * z / size;
        if (inside(q) == 0.0) {
            break;
        }
        if (textureSampleLevel(second, second_sampler, q, 0.0).x > 0.5) {
            // (pinned down between the last two samples)
            var lo = prev;
            var hi = z;
            for (var r = 0; r < 5; r++) {
                let mid = (lo + hi) * 0.5;
                if (textureSampleLevel(second, second_sampler, uv + slant * mid / size, 0.0).x > 0.5) {
                    hi = mid;
                } else {
                    lo = mid;
                }
            }
            // The face met: the top if the water's edge there runs across, a side if down.
            let tx = 1.0 / (max(fx.extra2.xy, vec2(4.0)) * 2.0);
            let at = uv + slant * hi / size;
            let gx = textureSampleLevel(second, second_sampler, at + vec2(tx.x, 0.0), 0.0).x - textureSampleLevel(second, second_sampler, at - vec2(tx.x, 0.0), 0.0).x;
            let gy = textureSampleLevel(second, second_sampler, at + vec2(0.0, tx.y), 0.0).x - textureSampleLevel(second, second_sampler, at - vec2(0.0, tx.y), 0.0).x;
            // (blended: how much the edge there runs across rather than down)
            return vec2(hi, smoothstep(0.3, 0.75, abs(gy) / (abs(gx) + abs(gy) + 1e-4)));
        }
        prev = z;
    }
    return vec2(-1.0, 0.0);
}

@fragment
fn fs_liquid(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    let ins = inside(in.uv);
    let depth = max(fx.p7.x, 0.0);
    // With depth: where the front of the water isn't, the sight may meet its
    // top or sides further back.
    if (depth > 0.0 && fx.texel_dir.w > 0.5 && ins > 0.0 && textureSampleLevel(second, second_sampler, in.uv, 0.0).x < 0.5) {
        let hit = through_body(in.uv, depth);
        if (hit.x > 0.0) {
            return body_face(in.uv, c, hit, depth);
        }
    }
    let size = max(fx.extra.zw, vec2(1.0));
    let grid = max(fx.extra2.xy, vec2(4.0)) * 2.0;
    let tx = 1.0 / grid;
    let smooth_ = clamp(fx.p6.x / 100.0, 0.0, 1.0);
    let r = tx * (0.6 + smooth_ * 1.6);
    let f2 = liq(in.uv, r);
    let f = f2.x;
    // (the slope of a field that's flat inside the water, so only its surface catches the light)
    let gx = shape(liq(in.uv + vec2(tx.x * 1.5, 0.0), r).x) - shape(liq(in.uv - vec2(tx.x * 1.5, 0.0), r).x);
    let gy = shape(liq(in.uv + vec2(0.0, tx.y * 1.5), r).x) - shape(liq(in.uv - vec2(0.0, tx.y * 1.5), r).x);
    // The surface where the density is half: smooth over about a pixel.
    let aa = max(fwidth(f) * 0.75, 0.01);
    let cover = smoothstep(0.5 - aa, 0.5 + aa, f) * ins;
    let thick = clamp((f - 0.5) / 0.7, 0.0, 1.0);
    // A rounded surface: steep at the edge, flat inside.
    let n = normalize(vec3(-gx * 3.0, -gy * 3.0, 0.35 + thick));
    let show = u32(round(fx.p6.z));
    let kind = u32(round(fx.p4.x));
    let tol = clamp(fx.p4.y / 100.0, 0.0, 1.0);
    // What's behind: the layer (all of it, or only its solid objects), or nothing.
    var back = c;
    if (show == 1u) {
        back = c * solidity(c, kind, tol);
    } else if (show == 2u) {
        back = vec4(0.0);
    }
    let refr = max(fx.p5.y, 0.0) / 100.0;
    let off = -n.xy * refr * 14.0 / size;
    var seen = textureSample(src, src_sampler, to_tex(in.uv + off));
    if (show == 1u) {
        seen = seen * solidity(seen, kind, tol);
    } else if (show == 2u) {
        seen = vec4(0.0);
    }
    let tint = colour(fx.p4.w);
    let clear = clamp(fx.p5.x / 100.0, 0.0, 1.0);
    let murk = (1.0 - clear) * (0.55 + 0.45 * thick);
    // Seen through the water: tinted, plus the light the water itself scatters.
    let body = tint * (0.18 + 0.32 * thick);
    let through = seen.rgb * mix(vec3(1.0), tint, murk);
    let la = radians(fx.p6.y);
    let l = normalize(vec3(cos(la), -sin(la), 1.1));
    let hv = normalize(l + vec3(0.0, 0.0, 1.0));
    let hl = max(fx.p5.z, 0.0) / 100.0;
    let spec = (pow(max(dot(n, hv), 0.0), 90.0) * 1.6 + pow(max(dot(n, hv), 0.0), 12.0) * 0.1) * hl;
    let rim = pow(clamp(1.0 - n.z, 0.0, 1.0), 2.5) * 0.35 * hl;
    // (foam and spray at the surface and in the air, not deep inside)
    let foam = clamp(f2.y * 1.4 * max(fx.p5.w, 0.0) / 100.0, 0.0, 1.0) * clamp(1.7 - f, 0.0, 1.0);
    var water: vec4<f32>;
    if (show == 2u) {
        // The water alone: its own colour and light, see-through as it's clear.
        let al = clamp(0.25 + murk * 0.7 + spec + rim, 0.0, 1.0);
        let rgb = body * (1.0 - clear * 0.5) + vec3(spec + rim);
        water = vec4(rgb, al);
    } else {
        // Over what's behind (bent through it), the water darkening what's thick.
        let a_seen = max(seen.a, murk);
        let rgb = through + body * murk + vec3(spec + rim) * a_seen;
        water = vec4(rgb, max(seen.a, clamp(murk * 0.9 + spec + rim, 0.0, 1.0)));
    }
    water = mix(water, vec4(vec3(0.92, 0.95, 0.97), 1.0), foam);
    let out = mix(back, water, cover);
    return mix(c, out, clamp(fx.p6.w / 100.0, 0.0, 1.0));
}

// The water's top or a side, met `hit.x` back in a body `depth` deep: its
// colour and light (the top catches the sky and the light, a side is darker,
// further back is a little dimmer), over what's behind as fs_liquid does.
fn body_face(uv: vec2<f32>, c: vec4<f32>, hit: vec2<f32>, depth: f32) -> vec4<f32> {
    let show = u32(round(fx.p6.z));
    let kind = u32(round(fx.p4.x));
    let tol = clamp(fx.p4.y / 100.0, 0.0, 1.0);
    var back = c;
    if (show == 1u) {
        back = c * solidity(c, kind, tol);
    } else if (show == 2u) {
        back = vec4(0.0);
    }
    let tint = colour(fx.p4.w);
    let clear = clamp(fx.p5.x / 100.0, 0.0, 1.0);
    let top = hit.y;
    let far = clamp(hit.x / max(depth, 1.0), 0.0, 1.0);
    let hl = max(fx.p5.z, 0.0) / 100.0;
    // (a top seen at a slant reflects more of the sky, like any water)
    let slant = length(sight_slant(uv));
    let fres = mix(0.1, clamp(0.06 + 0.5 / (1.0 + slant * 2.0), 0.06, 0.55), top);
    let sky = vec3(0.75, 0.84, 0.92);
    let lit = mix(0.55, 1.0, top) * (1.0 - far * 0.25);
    // Soft ripples drifting on the top, catching the light now and then.
    let size = max(fx.extra.zw, vec2(1.0));
    let ripple = fbm(uv * size / vec2(60.0, 22.0) + vec2(fx.extra.y * 0.3, 0.0), 71u);
    let glint = smoothstep(0.7, 0.95, ripple) * 0.22 * hl * top;
    let body = tint * (0.2 + 0.3 * lit);
    let murk = (1.0 - clear) * 0.8 + 0.2;
    var rgb = mix(back.rgb * mix(vec3(1.0), tint, murk), body, murk) * lit;
    rgb = mix(rgb, sky * lit, fres) + vec3(glint);
    var a = 1.0;
    if (show == 2u) {
        a = clamp(0.3 + murk * 0.6 + fres + glint, 0.0, 1.0);
        rgb = body * lit + sky * fres * lit + vec3(glint);
    } else {
        a = max(back.a, clamp(murk + fres, 0.0, 1.0));
    }
    let out = vec4(rgb, a);
    return mix(c, out, clamp(fx.p6.w / 100.0, 0.0, 1.0));
}

// ---- Liquid (3D) ------------------------------------------------------------
// The simulation's picture (second, seen through its camera): thickness
// (cells), foam, and the surface's normal across and down (towards the camera
// is -z). p5 = (-, -, water colour, clearness %), p6 = (refraction %,
// highlights %, foam %, light direction °), p7 = (view from above °, show,
// opacity %).

@fragment
fn fs_liquid3d(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    let s = textureSampleLevel(second, second_sampler, in.uv, 0.0);
    let thick = max(s.x, 0.0) * inside(in.uv);
    // Where there's water (its edge soft over a fraction of a cell).
    let cover = smoothstep(0.05, 0.45, thick);
    if (cover <= 0.0) {
        return select(c, vec4(0.0), u32(round(fx.p7.y)) == 1u);
    }
    let nxy = clamp(s.zw, vec2(-1.0), vec2(1.0));
    let n = vec3(nxy, -sqrt(max(1.0 - dot(nxy, nxy), 0.0)));
    // (towards the eye: the camera looks along +z)
    let v = vec3(0.0, 0.0, -1.0);
    let tint = colour(fx.p5.z);
    let clear = clamp(fx.p5.w / 100.0, 0.0, 1.0);
    // The picture behind, bent through the surface (further where it's thicker).
    let bend = nxy * max(fx.p6.x, 0.0) / 100.0 * 0.035 * clamp(thick / 3.0, 0.3, 1.5);
    let back = textureSample(src, src_sampler, to_tex(clamp(in.uv + bend, vec2(0.0), vec2(1.0))));
    // Light through the water: its colour deepens with thickness, less so the clearer it is.
    let absorb = 1.0 - exp(-thick * (1.1 - clear) * 0.35);
    let through = mix(back.rgb, back.rgb * tint, clamp(absorb + (1.0 - clear) * 0.3, 0.0, 1.0));
    // Lit from a light at Light direction, above.
    let ld = fx.p6.w * 0.017453293;
    let l = normalize(vec3(cos(ld), -0.9, -0.6 + sin(ld) * 0.4));
    let diffuse = clamp(dot(n, -l) * 0.5 + 0.5, 0.0, 1.0);
    let body = tint * (0.25 + 0.55 * diffuse);
    // Murky water shows its own colour; clear water, what's behind.
    let murk = (1.0 - clear) * 0.85;
    var rgb = mix(through, body, clamp(murk * absorb * 1.4, 0.0, 1.0));
    // The sky in it at grazing angles, and the light's highlight.
    // (the thin rim of a body of water is seen edge on: it catches the sky
    // too, rather than showing as a dark outline)
    let rim = 1.0 - smoothstep(0.45, 1.4, thick);
    let fres = max(0.03 + 0.97 * pow(1.0 - clamp(-n.z, 0.0, 1.0), 4.0), rim * 0.6);
    let sky = mix(vec3(0.62, 0.72, 0.82), vec3(0.88, 0.92, 0.96), clamp(-n.y * 0.5 + 0.5, 0.0, 1.0));
    rgb = mix(rgb, sky, clamp(fres, 0.0, 0.8));
    let h = normalize(-l - v);
    let spec = pow(max(dot(-n, h), 0.0), 160.0) * 1.3 * max(fx.p6.y, 0.0) / 100.0;
    rgb += vec3(1.0, 0.98, 0.94) * spec;
    // Foam where it sprays.
    let foam = clamp(s.y * 1.3 * max(fx.p6.z, 0.0) / 100.0, 0.0, 1.0);
    rgb = mix(rgb, vec3(0.94, 0.96, 0.98) * (0.6 + 0.4 * diffuse), foam);
    let o = clamp(fx.p7.z / 100.0, 0.0, 1.0) * cover;
    if (u32(round(fx.p7.y)) == 1u) {
        // The water alone (clear water lets little of itself show).
        let a = clamp(o * (0.35 + murk * 0.5 + fres + spec + foam), 0.0, 1.0);
        return vec4(rgb * a, a);
    }
    return mix(c, vec4(rgb, 1.0), o);
}
