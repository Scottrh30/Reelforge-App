// The water surface on the graphics card (see watergpu.rs and
// reelforge_core::water): a height grid stepped by the wave equation. Heights
// are in grid cells; one step is one call of drive + wave.
//
// One buffer holds every grid, a section each (offsets in the step's uniform):
// height, its rate of change, the next height and rate, what's solid now and
// how that changed this step.

struct Step {
    w: u32,
    h: u32,
    solids: u32,
    ndrops: u32,
    c2: f32,
    keep: f32,
    wind: f32,
    wake: f32,
    // amplitude (cells), wave number (radians a cell), -, phase now
    wave: vec4<f32>,
    wave_dir: vec2<f32>,
    tol: f32,
    step: f32,
    // offsets of the sections: height, rate, next height, next rate
    o_a: vec4<u32>,
    // solid, change in solid, -, -
    o_b: vec4<u32>,
    drops: array<vec4<f32>, 8>,
};

@group(0) @binding(0) var<uniform> st: Step;
@group(0) @binding(1) var<storage, read_write> g: array<f32>;
@group(0) @binding(2) var out_tex: texture_storage_2d<rgba16float, write>;
@group(1) @binding(0) var layer_tex: texture_2d<f32>;
@group(1) @binding(1) var layer_samp: sampler;

fn ix(x: i32, y: i32) -> u32 {
    return u32(clamp(y, 0, i32(st.h) - 1)) * st.w + u32(clamp(x, 0, i32(st.w) - 1));
}
fn hgt(i: u32) -> f32 { return g[st.o_a.x + i]; }
fn solid(i: u32) -> f32 { return g[st.o_b.x + i]; }

fn hash_u(x: u32) -> u32 {
    var v = x * 747796405u + 2891336453u;
    v = ((v >> ((v >> 28u) + 4u)) ^ v) * 277803737u;
    return (v >> 22u) ^ v;
}
fn rnd(a: u32, b: u32) -> f32 {
    return f32(hash_u(hash_u(a) ^ (b * 0x9E3779B9u)) >> 8u) / 16777216.0;
}

// How solid a colour is, by the chosen key (0 none, 1 its transparency,
// 2 not green, 3 not blue, 4 dark, 5 light), with a tolerance 0–1.
fn solidity(c: vec4<f32>, kind: u32, tol: f32) -> f32 {
    let a = c.a;
    let rgb = select(vec3(0.0), c.rgb / c.a, c.a > 1e-4);
    let soft = 0.04 + tol * 0.25;
    switch kind {
        case 1u: { return smoothstep(0.5 - soft, 0.5 + soft, a); }
        case 2u: {
            // Green screen: solid where the colour isn't (much) greener than it is red or blue.
            let k = rgb.g - max(rgb.r, rgb.b);
            return a * (1.0 - smoothstep(0.08 + tol * 0.2 - soft * 0.5, 0.08 + tol * 0.2 + soft * 0.5, k));
        }
        case 3u: {
            let k = rgb.b - max(rgb.r, rgb.g);
            return a * (1.0 - smoothstep(0.08 + tol * 0.2 - soft * 0.5, 0.08 + tol * 0.2 + soft * 0.5, k));
        }
        case 4u: {
            let l = dot(rgb, vec3(0.2126, 0.7152, 0.0722));
            return a * (1.0 - smoothstep(0.15 + tol * 0.4 - soft, 0.15 + tol * 0.4 + soft, l));
        }
        case 5u: {
            let l = dot(rgb, vec3(0.2126, 0.7152, 0.0722));
            return a * smoothstep(0.85 - tol * 0.4 - soft, 0.85 - tol * 0.4 + soft, l);
        }
        default: { return 0.0; }
    }
}

// How far p is from the frame's sides the waves come in through (cells): the
// sides facing against their direction.
fn wave_band(p: vec2<f32>) -> f32 {
    let d = st.wave_dir;
    var e = 1e9;
    if (d.x > 0.35) { e = min(e, p.x); }
    if (d.x < -0.35) { e = min(e, f32(st.w) - p.x); }
    if (d.y > 0.35) { e = min(e, p.y); }
    if (d.y < -0.35) { e = min(e, f32(st.h) - p.y); }
    return e;
}

// What's solid now, from the layer, and how that changed (for the wake).
@compute @workgroup_size(8, 8)
fn solids(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= st.w || id.y >= st.h) {
        return;
    }
    let i = id.y * st.w + id.x;
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(f32(st.w), f32(st.h));
    var s = 0.0;
    if (st.solids != 0u) {
        s = select(0.0, 1.0, solidity(textureSampleLevel(layer_tex, layer_samp, uv, 0.0), st.solids, st.tol) > 0.5);
    }
    let was = g[st.o_b.x + i];
    // (nothing changes on the first step: the objects were there all along)
    g[st.o_b.y + i] = select(s - was, 0.0, st.step < 0.5);
    g[st.o_b.x + i] = s;
}

// Drops, waves rolling in, the wind and the wake.
@compute @workgroup_size(8, 8)
fn drive(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= st.w || id.y >= st.h) {
        return;
    }
    let x = i32(id.x);
    let y = i32(id.y);
    let i = id.y * st.w + id.x;
    if (solid(i) > 0.5) {
        g[st.o_a.x + i] = 0.0;
        g[st.o_a.y + i] = 0.0;
        return;
    }
    var h = g[st.o_a.x + i];
    let p = vec2<f32>(f32(x) + 0.5, f32(y) + 0.5);
    for (var k = 0u; k < min(st.ndrops, 8u); k++) {
        let d = st.drops[k];
        let r2 = max(d.z * d.z, 0.25);
        let q = dot(p - d.xy, p - d.xy);
        if (q < r2 * 9.0) {
            // A dent with a little raised rim: rings spread out from it.
            h += d.w * (1.0 - q / r2) * exp(-q / r2);
        }
    }
    // Waves rolling in along wave_dir, made in a band along the side they come from.
    if (st.wave.x > 0.0) {
        let c = vec2(f32(st.w), f32(st.h)) * 0.5;
        let dir = st.wave_dir;
        let into = wave_band(p);
        if (into < 4.0) {
            // a plane wave (bent a little along its crests, so it isn't ruler-straight)
            let along = dot(p - c, vec2(-dir.y, dir.x));
            let wobble = 0.6 * sin(along * st.wave.y * 0.23 + st.wave.w * 0.11);
            h = mix(h, st.wave.x * sin(st.wave.y * dot(p - c, dir) - st.wave.w + wobble), 1.0 - into / 4.0);
        }
    }
    // Wind: a little noise, changing every few steps.
    if (st.wind > 0.0) {
        let n = rnd(i, u32(st.step)) - 0.5;
        h += n * st.wind;
    }
    // The wake: an object moving in pushes water up beside it, leaving behind pulls it down.
    if (st.wake > 0.0) {
        // (spread over a few cells, so the edge stepping from cell to cell doesn't fizz)
        var push = 0.0;
        for (var dy = -2; dy <= 2; dy++) {
            for (var dx = -2; dx <= 2; dx++) {
                let wgt = 1.0 - f32(dx * dx + dy * dy) / 9.0;
                push += g[st.o_b.y + ix(x + dx, y + dy)] * max(wgt, 0.0);
            }
        }
        h += push * st.wake * 0.06;
    }
    g[st.o_a.x + i] = h;
}

// The wave equation (a 9-point Laplacian, so rings stay round): solid cells
// reflect waves, the frame's edges let them out.
@compute @workgroup_size(8, 8)
fn wave(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= st.w || id.y >= st.h) {
        return;
    }
    let x = i32(id.x);
    let y = i32(id.y);
    let i = id.y * st.w + id.x;
    let hc = hgt(i);
    if (solid(i) > 0.5) {
        g[st.o_a.z + i] = 0.0;
        g[st.o_a.w + i] = 0.0;
        return;
    }
    var side = 0.0;
    var diag = 0.0;
    for (var k = 0; k < 4; k++) {
        let o = array<vec2<i32>, 4>(vec2(1, 0), vec2(-1, 0), vec2(0, 1), vec2(0, -1))[k];
        let j = ix(x + o.x, y + o.y);
        side += select(hgt(j), hc, solid(j) > 0.5);
        let od = array<vec2<i32>, 4>(vec2(1, 1), vec2(-1, 1), vec2(1, -1), vec2(-1, -1))[k];
        let jd = ix(x + od.x, y + od.y);
        diag += select(hgt(jd), hc, solid(jd) > 0.5);
    }
    let lap = (4.0 * side + diag - 20.0 * hc) / 6.0;
    // A touch of viscosity: the finest ripples (a cell or two across) die out
    // quickly, longer waves hardly notice.
    let vc = g[st.o_a.y + i];
    let lapv = g[st.o_a.y + ix(x + 1, y)] + g[st.o_a.y + ix(x - 1, y)] + g[st.o_a.y + ix(x, y + 1)] + g[st.o_a.y + ix(x, y - 1)] - 4.0 * vc;
    // Near the frame's edges the water calms, so waves leave rather than bounce
    // back (not along the side the waves come in from).
    let edge = f32(min(min(x, i32(st.w) - 1 - x), min(y, i32(st.h) - 1 - y)));
    var sponge = select(1.0, 0.86 + 0.14 * edge / 10.0, edge < 10.0);
    if (st.wave.x > 0.0 && wave_band(vec2<f32>(f32(x) + 0.5, f32(y) + 0.5)) < 14.0) {
        sponge = 1.0;
    }
    let v = (vc + st.c2 * lap + 0.05 * lapv) * st.keep * sponge;
    g[st.o_a.w + i] = v;
    g[st.o_a.z + i] = (hc + v) * select(1.0, sponge, edge < 10.0);
}

// The next height and rate become the current ones.
@compute @workgroup_size(8, 8)
fn commit(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= st.w || id.y >= st.h) {
        return;
    }
    let i = id.y * st.w + id.x;
    g[st.o_a.x + i] = g[st.o_a.z + i];
    g[st.o_a.y + i] = g[st.o_a.w + i];
}

// The picture for the effect: height, slope across and down, solid.
@compute @workgroup_size(8, 8)
fn picture(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= st.w || id.y >= st.h) {
        return;
    }
    let x = i32(id.x);
    let y = i32(id.y);
    let i = id.y * st.w + id.x;
    let gx = (hgt(ix(x + 1, y)) - hgt(ix(x - 1, y))) * 0.5;
    let gy = (hgt(ix(x, y + 1)) - hgt(ix(x, y - 1))) * 0.5;
    textureStore(out_tex, vec2<i32>(id.xy), vec4(hgt(i), gx, gy, solid(i)));
}
