// The water surface on the graphics card (see watergpu.rs and
// reelforge_core::water): a height grid stepped by the wave equation. Heights
// are in grid cells; one step is one call of drive + wave.
//
// One buffer holds every grid, a section each (offsets in the step's uniform):
// height, its rate of change, the next height and rate, what's solid now,
// how that changed this step, and (an ocean's) foam.

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
    // solid, change in solid, foam (an ocean's), -
    o_b: vec4<u32>,
    drops: array<vec4<f32>, 8>,
    // An ocean with objects in it (reelforge_core::water::ocean_step): the
    // camera (aspect, tan of half the view, look down rad, height m) and
    // (distance travelled m, the sea's time s, 1 for ocean mode, how strongly
    // waves bounce back). drops then holds the sea's 4 biggest waves:
    // (direction x, z, wave number, height m), then (phase, -, -, -) each.
    cam: vec4<f32>,
    cam2: vec4<f32>,
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
// Cell j's height as seen from a water cell at height hc (a wall: level with it).
fn side_h(j: u32, hc: f32) -> f32 { return select(hgt(j), hc, solid(j) > 0.5); }

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

// ---- Ocean mode ----------------------------------------------------------

fn ocean_mode() -> bool {
    return st.cam2.z > 0.5;
}

// Where cell p's ray meets the sea: (world x, z, metres a cell across,
// metres a cell in depth); w < 0 above the horizon.
fn sea_point(p: vec2<f32>) -> vec4<f32> {
    let uv = p / vec2(f32(st.w), f32(st.h));
    let aspect = st.cam.x;
    let tan_half = st.cam.y;
    let pitch = st.cam.z;
    let sx = (uv.x * 2.0 - 1.0) * aspect * tan_half;
    let sy = (1.0 - uv.y * 2.0) * tan_half;
    let fwd = vec3(0.0, -sin(pitch), cos(pitch));
    let up = vec3(0.0, cos(pitch), sin(pitch));
    let d = normalize(fwd + vec3(sx, 0.0, 0.0) + up * sy);
    if (d.y > -0.004) {
        return vec4(0.0, 0.0, 0.0, -1.0);
    }
    let tt = min(st.cam.w / -d.y, 20000.0);
    let pix = 2.0 * tan_half / f32(st.h);
    return vec4(d.x * tt, st.cam2.x + d.z * tt, tt * pix, tt * pix / max(-d.y, 0.02));
}

// The sea's own height there (its biggest waves).
fn incident(x: vec2<f32>) -> f32 {
    var y = 0.0;
    for (var i = 0u; i < 4u; i++) {
        let w = st.drops[i];
        let ph = st.drops[i + 4u].x;
        y += w.w * sin(w.z * dot(w.xy, x) - sqrt(9.81 * w.z) * st.cam2.y + ph);
    }
    return y;
}

// The wave speed squared there (cells² a step²): the sea's main waves'
// speed, in the cells' size there (far off a cell is many metres, so waves
// cross it slowly), at most what keeps the stepping steady.
fn ocean_c2(sp: vec4<f32>) -> f32 {
    if (sp.w < 0.0) {
        return 0.0;
    }
    let k = max(st.drops[0].z, 0.01);
    let c = sqrt(9.81 / k) / 60.0 / max(sqrt(sp.z * sp.w), 1e-4);
    // (well inside the steady limit: the ocean's grid also damps its finest ripples)
    return clamp(c * c, 0.0, 0.3);
}

// How much the water an object leaves is drawn down, against what it's
// pushed up ahead (see the ocean's wake in `drive`).
const STERN_PULL: f32 = 0.0;
// An ocean's height the objects leave settles back to the sea's own each
// step (its waves still spread as they settle).
const OCEAN_SETTLE: f32 = 0.996;
// How much of a step's change in what's solid carries on to the next (see `solids`).
const MOTION_KEEP: f32 = 0.6;
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
    // How it's changing, smoothed over the next few steps: footage brings a
    // new picture only every two or three steps (60 a second against 25 or
    // 30 frames), and a push on just those steps leaves stripes along the
    // wake. (The same push in all, spread out.) Nothing changes on the
    // first step: the objects were there all along.
    let change = select(s - was, 0.0, st.step < 0.5);
    g[st.o_b.y + i] = select(g[st.o_b.y + i] * MOTION_KEEP + change * (1.0 - MOTION_KEEP), 0.0, st.step < 0.5);
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
    if (ocean_mode()) {
        let sp = sea_point(p);
        if (sp.w < 0.0) {
            g[st.o_a.x + i] = 0.0;
            g[st.o_a.y + i] = 0.0;
            return;
        }
        // Beside an object the sea's waves are cancelled (the water can't
        // move into it): what the simulation holds is what the object sends
        // back. (The sea's own waves are drawn by the ocean itself.)
        var near = 0.0;
        for (var k = 0; k < 4; k++) {
            let o = array<vec2<i32>, 4>(vec2(1, 0), vec2(-1, 0), vec2(0, 1), vec2(0, -1))[k];
            near = max(near, solid(ix(x + o.x, y + o.y)));
        }
        if (near > 0.5) {
            h = mix(h, -incident(sp.xy) * st.cam2.w, 0.7);
        }
        if (st.wake > 0.0) {
            // (spread wider than the water surface's: the ocean's cells are
            // coarse, and an edge stepping across them mustn't ring)
            var push = 0.0;
            for (var dy = -3; dy <= 3; dy++) {
                for (var dx = -3; dx <= 3; dx++) {
                    let m = g[st.o_b.y + ix(x + dx, y + dy)];
                    // (the water closes in behind faster than it's pushed
                    // aside ahead: the stern draws less down than the bow
                    // raises, so its track isn't one long trough)
                    push += select(m, m * STERN_PULL, m < 0.0) * exp(-f32(dx * dx + dy * dy) / 4.5);
                }
            }
            // (in metres: a fair fraction of the sea's main wave)
            h += push * st.wake * 0.03 * max(st.drops[0].w, 0.3);
        }
        // Foam: churned up at the waterline (just under an object in the
        // picture) where it moves through the water, its bow and stern,
        // and left behind along its track; and where the waves break against
        // it. Fading over a few seconds.
        var under = 0.0;
        for (var k = 1; k <= 3; k++) {
            under = max(under, solid(ix(x, y - k)));
        }
        var moving = 0.0;
        if (under > 0.5) {
            for (var dy = -4; dy <= 2; dy++) {
                for (var dx = -3; dx <= 3; dx++) {
                    moving += abs(g[st.o_b.y + ix(x + dx, y + dy)]);
                }
            }
        }
        // (the foam spreads out from the track and breaks up as it fades, so
        // the trail widens behind rather than staying a line)
        var around = 0.0;
        for (var k = 0; k < 4; k++) {
            let o = array<vec2<i32>, 4>(vec2(1, 0), vec2(-1, 0), vec2(0, 1), vec2(0, -1))[k];
            around += g[st.o_b.z + ix(x + o.x, y + o.y)] * 0.25;
        }
        let here = mix(g[st.o_b.z + i], around, 0.12);
        var f = here * 0.985 + clamp(moving, 0.0, 3.0) * 0.06 * min(st.wake, 2.0);
        if (near > 0.5) {
            f += clamp(abs(h) / max(st.drops[0].w, 0.3), 0.0, 2.0) * 0.02;
        }
        g[st.o_b.z + i] = clamp(f, 0.0, 1.0);
        g[st.o_a.x + i] = h;
        return;
    }
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
    let c2 = select(st.c2, ocean_c2(sea_point(vec2<f32>(f32(x) + 0.5, f32(y) + 0.5))), ocean_mode());
    // (an ocean's grid is coarse for its waves near the horizon: damp its finest more)
    let visc = select(0.05, 0.1, ocean_mode());
    let v = (vc + c2 * lap + visc * lapv) * st.keep * sponge;
    // (never a runaway value: one would spread over the whole grid)
    let ok = abs(v) < 1e4 && abs(hc + v) < 1e4;
    g[st.o_a.w + i] = select(0.0, v, ok);
    let settle = select(1.0, OCEAN_SETTLE, ocean_mode());
    g[st.o_a.z + i] = select(0.0, (hc + v) * select(1.0, sponge, edge < 10.0) * settle, ok);
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

// The picture for the effect: height, slope across and down, and -1 where
// solid (else the foam: 0 but for an ocean).
@compute @workgroup_size(8, 8)
fn picture(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= st.w || id.y >= st.h) {
        return;
    }
    let x = i32(id.x);
    let y = i32(id.y);
    let i = id.y * st.w + id.x;
    // (a solid neighbour is a wall: the water meets it level, as the
    // wave step has it, so no slope is made up across its edge)
    let hc = hgt(i);
    let gx = (side_h(ix(x + 1, y), hc) - side_h(ix(x - 1, y), hc)) * 0.5;
    let gy = (side_h(ix(x, y + 1), hc) - side_h(ix(x, y - 1), hc)) * 0.5;
    // (the last channel: -1 for solid, else the foam)
    textureStore(out_tex, vec2<i32>(id.xy), vec4(hc, gx, gy, select(g[st.o_b.z + i], -1.0, solid(i) > 0.5)));
}
