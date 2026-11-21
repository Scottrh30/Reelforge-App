// Fluid on the graphics card (see fluidgpu.rs): stable fluids on a grid,
// one step at a time. Velocity in cells per step, colour premultiplied
// linear with its density in alpha.
//
// Obstacles (the layer's own shape) are read at every step, so they can
// move: each cell keeps how solid it is now and was a step ago, and how far
// the shape's middle moved in that step is its motion. The solid moves the
// fluid next to it at that speed (the pressure solve pushes it out of the
// way ahead and draws it in behind), and fluid the solid passes over is
// carried along inside it rather than lost, coming out round it.

struct Step {
    w: u32,
    h: u32,
    obstacles: u32,
    // 1: the obstacles read this step have nothing before them to compare
    // with (the first step, or just after a seek): no motion.
    fresh: u32,
    src: vec2<f32>,
    radius: f32,
    amount: f32,
    push: vec2<f32>,
    rise: f32,
    swirl: f32,
    color: vec3<f32>,
    keep: f32,
    // From the grid's 0–1 coordinates to the obstacle picture's (rows of a
    // homography): the identity for the layer's own shape, or the way onto
    // another layer.
    m0: vec4<f32>,
    m1: vec4<f32>,
    m2: vec4<f32>,
    // Billowing body: x on (1) or flat (0), y the shading, zw the way to the light.
    look: vec4<f32>,
    // x: seconds in.
    time: vec4<f32>,
};

@group(0) @binding(0) var<uniform> st: Step;
@group(0) @binding(1) var<storage, read_write> vel: array<vec2<f32>>;
@group(0) @binding(2) var<storage, read_write> vel_tmp: array<vec2<f32>>;
@group(0) @binding(3) var<storage, read_write> dye: array<vec4<f32>>;
@group(0) @binding(4) var<storage, read_write> dye_tmp: array<vec4<f32>>;
@group(0) @binding(5) var<storage, read_write> scratch: array<f32>;
@group(0) @binding(6) var<storage, read_write> pa: array<f32>;
@group(0) @binding(7) var<storage, read_write> pb: array<f32>;
// How solid each cell is: now (x) and a step ago (y); after the w×h cells,
// the solid's motion this step (cells).
@group(0) @binding(8) var<storage, read_write> obs: array<vec2<f32>>;
@group(0) @binding(9) var out_tex: texture_storage_2d<rgba8unorm, write>;
@group(0) @binding(10) var layer_tex: texture_2d<f32>;
@group(0) @binding(11) var layer_samp: sampler;
// After those, how the solid moves cell by cell (cells a step; see
// `local_motion`), after holding the obstacles blurred across: cell i at
// obs[field(i)]. (Storage buffers are eight at most: so in this one.)
fn field(i: u32) -> u32 {
    return st.w * st.h + 1u + i;
}

fn ix(x: i32, y: i32) -> u32 {
    return u32(clamp(y, 0, i32(st.h) - 1)) * st.w + u32(clamp(x, 0, i32(st.w) - 1));
}

fn vel_at(x: f32, y: f32) -> vec2<f32> {
    let cx = clamp(x, 0.0, f32(st.w - 1u));
    let cy = clamp(y, 0.0, f32(st.h - 1u));
    let x0 = i32(floor(cx));
    let y0 = i32(floor(cy));
    let fx = cx - f32(x0);
    let fy = cy - f32(y0);
    let a = mix(vel[ix(x0, y0)], vel[ix(x0 + 1, y0)], fx);
    let b = mix(vel[ix(x0, y0 + 1)], vel[ix(x0 + 1, y0 + 1)], fx);
    return mix(a, b, fy);
}

fn dye_at(x: f32, y: f32) -> vec4<f32> {
    let cx = clamp(x, 0.0, f32(st.w - 1u));
    let cy = clamp(y, 0.0, f32(st.h - 1u));
    let x0 = i32(floor(cx));
    let y0 = i32(floor(cy));
    let fx = cx - f32(x0);
    let fy = cy - f32(y0);
    let a = mix(dye[ix(x0, y0)], dye[ix(x0 + 1, y0)], fx);
    let b = mix(dye[ix(x0, y0 + 1)], dye[ix(x0 + 1, y0 + 1)], fx);
    return mix(a, b, fy);
}

fn blocked(i: u32) -> bool {
    return st.obstacles != 0u && obs[i].x > 0.5;
}

// Fastest a solid is taken to move, in cells a step: more is a jump (a cut,
// or something appearing), not motion.
const MOTION_MAX: f32 = 8.0;

// How the solid moved this step at cell `i` (cells): see `local_motion`.
fn obstacle_velocity(i: u32) -> vec2<f32> {
    if (st.obstacles == 0u || st.fresh != 0u) {
        return vec2(0.0);
    }
    return obs[field(i)];
}

// Where the fluid is moved by: its own velocity, or a solid's (moving or not).
fn moving(x: i32, y: i32) -> vec2<f32> {
    let i = ix(x, y);
    return select(vel[i], obstacle_velocity(i), blocked(i));
}

// The layer's shape, cell by cell (where the fluid can't go), read at every
// step; what it was a step ago kept beside it.
@compute @workgroup_size(8, 8)
fn obstacles(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= st.w || id.y >= st.h) {
        return;
    }
    let i = id.y * st.w + id.x;
    let uv = vec3((vec2<f32>(id.xy) + 0.5) / vec2<f32>(f32(st.w), f32(st.h)), 1.0);
    let q = vec3(dot(st.m0.xyz, uv), dot(st.m1.xyz, uv), dot(st.m2.xyz, uv));
    var now = 0.0;
    if (abs(q.z) > 1e-6) {
        let o = q.xy / q.z;
        // Outside the other layer: nothing solid there.
        if (q.z > 0.0 && all(o >= vec2(0.0)) && all(o <= vec2(1.0))) {
            now = textureSampleLevel(layer_tex, layer_samp, o, 0.0).a;
        }
    }
    obs[i] = vec2(now, select(obs[i].x, now, st.fresh != 0u));
}

var<workgroup> sums: array<f32, 384>;

// The shape's weight and middle, now and a step ago, summed a workgroup at a
// time (six numbers each, into scratch: `motion` adds them up).
@compute @workgroup_size(8, 8)
fn motion_sum(@builtin(global_invocation_id) id: vec3<u32>, @builtin(local_invocation_index) li: u32, @builtin(workgroup_id) wg: vec3<u32>, @builtin(num_workgroups) nw: vec3<u32>) {
    var v = array<f32, 6>(0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
    if (id.x < st.w && id.y < st.h) {
        let o = obs[id.y * st.w + id.x];
        let p = vec2<f32>(id.xy);
        v = array<f32, 6>(o.x, o.x * p.x, o.x * p.y, o.y, o.y * p.x, o.y * p.y);
    }
    for (var k = 0u; k < 6u; k++) {
        sums[k * 64u + li] = v[k];
    }
    workgroupBarrier();
    for (var half = 32u; half > 0u; half = half / 2u) {
        if (li < half) {
            for (var k = 0u; k < 6u; k++) {
                sums[k * 64u + li] += sums[k * 64u + li + half];
            }
        }
        workgroupBarrier();
    }
    if (li == 0u) {
        let g = wg.y * nw.x + wg.x;
        for (var k = 0u; k < 6u; k++) {
            scratch[g * 6u + k] = sums[k * 64u];
        }
    }
}

var<workgroup> total: array<f32, 1536>;

// The solid's motion this step: how far its middle moved, if it's much the
// same size as a step ago (else it's appeared, gone or been cut to: no
// motion). One workgroup.
@compute @workgroup_size(256)
fn motion(@builtin(local_invocation_index) li: u32) {
    let groups = ((st.w + 7u) / 8u) * ((st.h + 7u) / 8u);
    var v = array<f32, 6>(0.0, 0.0, 0.0, 0.0, 0.0, 0.0);
    for (var g = li; g < groups; g += 256u) {
        for (var k = 0u; k < 6u; k++) {
            v[k] += scratch[g * 6u + k];
        }
    }
    for (var k = 0u; k < 6u; k++) {
        total[k * 256u + li] = v[k];
    }
    workgroupBarrier();
    for (var half = 128u; half > 0u; half = half / 2u) {
        if (li < half) {
            for (var k = 0u; k < 6u; k++) {
                total[k * 256u + li] += total[k * 256u + li + half];
            }
        }
        workgroupBarrier();
    }
    if (li == 0u) {
        let now = total[0];
        let before = total[768];
        var m = vec2(0.0);
        if (now > 2.0 && before > 2.0 && abs(now - before) < 0.25 * max(now, before)) {
            m = vec2(total[256], total[512]) / now - vec2(total[1024], total[1280]) / before;
            if (length(m) > MOTION_MAX) {
                m = vec2(0.0);
            }
        }
        obs[st.w * st.h] = m;
    }
}

// How far the obstacles are blurred (cells): their edges become ramps this
// wide each side, so edges moving up to this far a step are measured exactly.
const BLUR: i32 = 8;

// The obstacles (now, a step ago) blurred across, into `field`'s place.
@compute @workgroup_size(8, 8)
fn blur_x(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= st.w || id.y >= st.h) {
        return;
    }
    var s = vec2(0.0);
    for (var d = -BLUR; d <= BLUR; d++) {
        s += obs[ix(i32(id.x) + d, i32(id.y))];
    }
    obs[field(id.y * st.w + id.x)] = s / f32(2 * BLUR + 1);
}

// ... and down, into `pa` (now) and `pb` (a step ago), free till `divergence`.
@compute @workgroup_size(8, 8)
fn blur_y(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= st.w || id.y >= st.h) {
        return;
    }
    var s = vec2(0.0);
    for (var d = -BLUR; d <= BLUR; d++) {
        s += obs[field(ix(i32(id.x), i32(id.y) + d))];
    }
    let i = id.y * st.w + id.x;
    pa[i] = s.x / f32(2 * BLUR + 1);
    pb[i] = s.y / f32(2 * BLUR + 1);
}

// How the solid moves at each cell, region by region: where an edge is (the
// blurred shape slopes), how fast it moves across itself is how much the
// shape there changed over how steep it is (dφ/dt = -v·∇φ). That is what
// pushes the fluid, so two things moving opposite ways each push their own
// way and a spinning round one stirs nothing. Along the edge, and inside,
// where nothing slopes, the motion of the shape as a whole is kept (see
// `motion`).
@compute @workgroup_size(8, 8)
fn local_motion(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= st.w || id.y >= st.h) {
        return;
    }
    let x = i32(id.x);
    let y = i32(id.y);
    let i = id.y * st.w + id.x;
    let whole = obs[st.w * st.h];
    let g = 0.25 * vec2(pa[ix(x + 1, y)] + pb[ix(x + 1, y)] - pa[ix(x - 1, y)] - pb[ix(x - 1, y)], pa[ix(x, y + 1)] + pb[ix(x, y + 1)] - pa[ix(x, y - 1)] - pb[ix(x, y - 1)]);
    let g2 = dot(g, g);
    var v = whole;
    // (a ramp BLUR cells wide slopes 1/17 a cell: g2 about 0.0035)
    let k = smoothstep(1e-4, 1e-3, g2);
    if (k > 0.0) {
        let len = sqrt(g2);
        let n = g / len;
        let across = clamp(-(pa[i] - pb[i]) / len, -MOTION_MAX, MOTION_MAX);
        v = whole + k * (across - dot(whole, n)) * n;
    }
    obs[field(i)] = v;
}

// How thick billowing smoke can pile up (1: just covers).
const THICK: f32 = 4.0;

fn hash3(p: vec3<f32>) -> f32 {
    let q = fract(p * vec3(0.1031, 0.1030, 0.0973));
    let r = q + dot(q, q.yzx + 33.33);
    return fract((r.x + r.y) * r.z);
}

// Smooth value noise in space and time, 0–1.
fn lumps(p: vec2<f32>, t: f32) -> f32 {
    let x = vec3(p, t);
    let i = floor(x);
    let f = x - i;
    let u = f * f * (3.0 - 2.0 * f);
    var s = 0.0;
    for (var k = 0u; k < 8u; k++) {
        let o = vec3(f32(k & 1u), f32((k >> 1u) & 1u), f32((k >> 2u) & 1u));
        let w = mix(1.0 - u, u, o);
        s += hash3(i + o) * w.x * w.y * w.z;
    }
    return s;
}

// Three octaves of it.
fn wisps(p: vec2<f32>, t: f32) -> f32 {
    return 0.55 * lumps(p, t) + 0.3 * lumps(p * 2.1 + 17.0, t * 1.3) + 0.15 * lumps(p * 4.3 + 41.0, t * 1.7);
}

// Noise `size` cells across that drifts with velocity `v` (cells a step): two
// layers, each pushed along for a second then faded out as the other takes over
// (so it doesn't stretch without end), 0–1.
fn flowing(p: vec2<f32>, v: vec2<f32>, size: f32, seed: f32, octaves: bool) -> f32 {
    let t = st.time.x;
    var s = 0.0;
    for (var k = 0; k < 2; k++) {
        let f = fract(t + f32(k) * 0.5);
        let start = floor(t + f32(k) * 0.5);
        // (carried at most a size and a half: where the flow shears, neighbours
        // carried far apart would break the noise into specks)
        let moved = v * (f * 30.0);
        let far = length(moved);
        let carried = moved * min(1.0, 1.5 * size / max(far, 1e-4));
        let q = (p - carried) / size + vec2(start * 13.7 + seed + f32(k) * 31.0, seed * 3.1);
        let n = select(lumps(q, start * 0.37 + seed), wisps(q, start * 0.37 + seed), octaves);
        s += n * (1.0 - abs(2.0 * f - 1.0));
    }
    return s;
}

// How covered billowing smoke is at (x, y).
fn cover(x: f32, y: f32) -> f32 {
    return 1.0 - exp(-1.5 * max(dye_at(x, y).a, 0.0));
}

// Rounded lumps drifting with the flow, 0–1.
fn rounded(p: vec2<f32>, v: vec2<f32>, size: f32) -> f32 {
    // (domes with sharp creases between them, like the cauliflower of real billows:
    // the noise folded about its middle, two sizes of it)
    let a = flowing(p, v, size, 7.0, false);
    let b = flowing(p, v, size * 0.45, 19.0, false);
    return 0.65 * abs(2.0 * a - 1.0) + 0.35 * abs(2.0 * b - 1.0);
}

// The source pours in velocity and colour; thick colour rises.
@compute @workgroup_size(8, 8)
fn inject(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= st.w || id.y >= st.h) {
        return;
    }
    let i = id.y * st.w + id.x;
    let r = st.radius;
    let d2 = dot(vec2<f32>(id.xy) - st.src, vec2<f32>(id.xy) - st.src);
    let wgt = exp(-d2 / (r * r));
    var v = vel[i];
    var c = dye[i];
    // (none poured into a solid)
    if (wgt >= 0.01 && !blocked(i)) {
        v += (st.push - v) * wgt * 0.6;
        let add = min(st.amount * wgt * (6.0 / 30.0), 1.0);
        if (st.look.x > 0.5) {
            // Billowing: poured in puffs (thicker and thinner, place to place and
            // moment to moment), piling up past covering.
            let puff = 0.1 + 1.9 * smoothstep(0.25, 0.85, lumps(vec2<f32>(id.xy) / max(r * 0.8, 1.0), st.time.x * 1.7));
            let more = add * puff * 1.6 * max(1.0 - c.a / THICK, 0.0);
            c = vec4(c.rgb + st.color * more, c.a + more);
        } else {
            c = vec4(c.rgb + (st.color - c.rgb) * add, c.a + (1.0 - c.a) * add);
        }
    }
    v.y -= st.rise * min(c.a, 1.0) * 0.06;
    vel[i] = v;
    dye[i] = c;
}

@compute @workgroup_size(8, 8)
fn curl(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= st.w || id.y >= st.h) {
        return;
    }
    let x = i32(id.x);
    let y = i32(id.y);
    var c = 0.0;
    if (x > 0 && y > 0 && x < i32(st.w) - 1 && y < i32(st.h) - 1) {
        c = (vel[ix(x + 1, y)].y - vel[ix(x - 1, y)].y - vel[ix(x, y + 1)].x + vel[ix(x, y - 1)].x) * 0.5;
    }
    scratch[id.y * st.w + id.x] = c;
}

// Vorticity confinement: swirls kept from dying out (into vel_tmp).
@compute @workgroup_size(8, 8)
fn confine(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= st.w || id.y >= st.h) {
        return;
    }
    let x = i32(id.x);
    let y = i32(id.y);
    let i = id.y * st.w + id.x;
    var v = vel[i];
    if (x > 1 && y > 1 && x < i32(st.w) - 2 && y < i32(st.h) - 2) {
        let nx = (abs(scratch[ix(x + 1, y)]) - abs(scratch[ix(x - 1, y)])) * 0.5;
        let ny = (abs(scratch[ix(x, y + 1)]) - abs(scratch[ix(x, y - 1)])) * 0.5;
        let len = length(vec2(nx, ny)) + 1e-5;
        let c = scratch[i];
        v += st.swirl * 0.35 * vec2(ny / len * c, -nx / len * c);
    }
    vel_tmp[i] = v;
}

// Velocity carried along itself (into vel_tmp).
@compute @workgroup_size(8, 8)
fn advect_vel(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= st.w || id.y >= st.h) {
        return;
    }
    let i = id.y * st.w + id.x;
    let v = vel[i];
    vel_tmp[i] = vel_at(f32(id.x) - v.x, f32(id.y) - v.y);
}

// Divergence (into scratch), and the pressure starting from nothing.
@compute @workgroup_size(8, 8)
fn divergence(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= st.w || id.y >= st.h) {
        return;
    }
    let x = i32(id.x);
    let y = i32(id.y);
    let i = id.y * st.w + id.x;
    var d = 0.0;
    if (x > 0 && y > 0 && x < i32(st.w) - 1 && y < i32(st.h) - 1) {
        // (a moving solid pushes into the fluid next to it, or pulls it after)
        d = -0.5 * (moving(x + 1, y).x - moving(x - 1, y).x + moving(x, y + 1).y - moving(x, y - 1).y);
    }
    scratch[i] = d;
    pa[i] = 0.0;
    pb[i] = 0.0;
}

fn jacobi(x: i32, y: i32, i: u32, from_a: bool) -> f32 {
    if (x <= 0 || y <= 0 || x >= i32(st.w) - 1 || y >= i32(st.h) - 1) {
        return 0.0;
    }
    if (from_a) {
        return (scratch[i] + pa[ix(x - 1, y)] + pa[ix(x + 1, y)] + pa[ix(x, y - 1)] + pa[ix(x, y + 1)]) * 0.25;
    }
    return (scratch[i] + pb[ix(x - 1, y)] + pb[ix(x + 1, y)] + pb[ix(x, y - 1)] + pb[ix(x, y + 1)]) * 0.25;
}

@compute @workgroup_size(8, 8)
fn jacobi_ab(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= st.w || id.y >= st.h) {
        return;
    }
    let i = id.y * st.w + id.x;
    pb[i] = jacobi(i32(id.x), i32(id.y), i, true);
}

@compute @workgroup_size(8, 8)
fn jacobi_ba(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= st.w || id.y >= st.h) {
        return;
    }
    let i = id.y * st.w + id.x;
    pa[i] = jacobi(i32(id.x), i32(id.y), i, false);
}

// The pressure's push taken off; walls at the edges and round obstacles.
@compute @workgroup_size(8, 8)
fn project(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= st.w || id.y >= st.h) {
        return;
    }
    let x = i32(id.x);
    let y = i32(id.y);
    let i = id.y * st.w + id.x;
    if (x <= 0 || y <= 0 || x >= i32(st.w) - 1 || y >= i32(st.h) - 1) {
        vel[i] = vec2(0.0);
        return;
    }
    if (blocked(i)) {
        vel[i] = obstacle_velocity(i);
        return;
    }
    vel[i] -= 0.5 * vec2(pa[ix(x + 1, y)] - pa[ix(x - 1, y)], pa[ix(x, y + 1)] - pa[ix(x, y - 1)]);
}

// Colour carried along by the flow, fading (into dye_tmp).
@compute @workgroup_size(8, 8)
fn advect_dye(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= st.w || id.y >= st.h) {
        return;
    }
    let i = id.y * st.w + id.x;
    let carried = obstacle_velocity(i);
    if (blocked(i) && (dot(carried, carried) < 1e-6 || obs[i].y <= 0.5)) {
        // Just covered, what was here is picked up; standing still (between
        // two frames of footage, say), what it carries stays put.
        dye_tmp[i] = dye[i] * st.keep;
        return;
    }
    let v = select(vel[i], carried, blocked(i));
    let back = vec2(f32(id.x) - v.x, f32(id.y) - v.y);
    if (!blocked(i) && blocked(ix(i32(round(back.x)), i32(round(back.y))))) {
        // (nothing flows out of a solid: what's here stays, to be picked up or pushed aside)
        dye_tmp[i] = dye[i] * st.keep;
        return;
    }
    dye_tmp[i] = dye_at(back.x, back.y) * st.keep;
}

// How much of what a solid carries seeps out of it, from each edge cell to each
// clear cell beside it, a step.
const SEEP: f32 = 0.2;

// Fluid carried inside a moving solid comes out at its edges (so it piles up
// ahead of it and slides round it, rather than vanishing into it): each clear
// cell takes a share from the solid cells beside it, and each solid cell gives
// that share to each clear cell beside it (into dye_tmp).
@compute @workgroup_size(8, 8)
fn seep(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= st.w || id.y >= st.h) {
        return;
    }
    let x = i32(id.x);
    let y = i32(id.y);
    let i = id.y * st.w + id.x;
    let here = blocked(i);
    var c = dye[i];
    let around = array<vec2<i32>, 4>(vec2(1, 0), vec2(-1, 0), vec2(0, 1), vec2(0, -1));
    for (var k = 0u; k < 4u; k++) {
        let nx = x + around[k].x;
        let ny = y + around[k].y;
        if (nx < 0 || ny < 0 || nx >= i32(st.w) || ny >= i32(st.h)) {
            continue;
        }
        let j = u32(ny) * st.w + u32(nx);
        if (blocked(j) != here) {
            if (here) {
                c -= dye[i] * SEEP;
            } else {
                c += dye[j] * SEEP;
            }
        }
    }
    dye_tmp[i] = max(c, vec4(0.0));
}

// The colour as a picture: straight sRGB colour and coverage.
@compute @workgroup_size(8, 8)
fn to_texture(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= st.w || id.y >= st.h) {
        return;
    }
    let i = id.y * st.w + id.x;
    // (what's being carried inside a solid doesn't show)
    let c = select(dye[i], vec4(0.0), blocked(i));
    var a = clamp(c.a, 0.0, 1.0);
    var lin = select(vec3(0.0), c.rgb / max(c.a, 1e-4), c.a > 1e-4);
    if (st.look.x > 0.5 && c.a > 1e-4) {
        let scale = f32(st.w) / 320.0;
        let here = vec2<f32>(id.xy);
        // (the velocity smoothed a little, for the same reason)
        let v = 0.25 * (vel[i] + vel[ix(i32(id.x) + 2, i32(id.y))] + vel[ix(i32(id.x), i32(id.y) + 2)] + vel[ix(i32(id.x) - 2, i32(id.y) - 2)]);
        let thin = exp(-1.2 * c.a);
        // Fine wisps, mostly where it's thin, and big lumps giving thick smoke its
        // form: both drift with the flow (two staggered layers of noise, each carried
        // along by the velocity here for a second, then faded out for a fresh one).
        let fine = flowing(here, v, 6.0 * scale, 0.0, true);
        let d = c.a * mix(1.0, 0.15 + 1.7 * fine, 0.3 + 0.55 * thin);
        a = 1.0 - exp(-1.5 * d);
        // Lit as a surface: the thickness (smoothed) and the lumps as height.
        let k = 4.0 * scale;
        let x = here.x;
        let y = here.y;
        // (the slope of how covered it is, not how thick: inside, where it's all
        // covered, the lumps give the form)
        let gd = vec2(cover(x + k, y) - cover(x - k, y), cover(x, y + k) - cover(x, y - k)) / (2.0 * k);
        // (the lumps: rounded, soft, big)
        let e = 5.0 * scale;
        let big = 30.0 * scale;
        let gb = vec2(rounded(here + vec2(e, 0.0), v, big) - rounded(here - vec2(e, 0.0), v, big), rounded(here + vec2(0.0, e), v, big) - rounded(here - vec2(0.0, e), v, big)) / (2.0 * e);
        let g = gd * 6.0 * scale + gb * 20.0 * scale * min(c.a, 1.5);
        let n = normalize(vec3(-g, 1.0));
        let towards = normalize(vec3(st.look.z, st.look.w, 0.9));
        // (wrapped, so the shaded sides stay soft)
        // (in linear light, so the shade goes deeper than it looks on screen)
        let wrap = 0.5 + 0.5 * dot(n, towards);
        let lit = 0.25 + 0.8 * pow(wrap, 1.5);
        lin *= mix(1.0, lit, st.look.y);
    }
    let l = clamp(lin, vec3(0.0), vec3(1.0));
    let g = select(1.055 * pow(l, vec3(1.0 / 2.4)) - 0.055, l * 12.92, l <= vec3(0.0031308));
    textureStore(out_tex, vec2<i32>(id.xy), vec4(g, a));
}
