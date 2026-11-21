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

// How the solid moved this step (cells): see `motion`.
fn obstacle_velocity() -> vec2<f32> {
    if (st.obstacles == 0u || st.fresh != 0u) {
        return vec2(0.0);
    }
    return obs[st.w * st.h];
}

// Where the fluid is moved by: its own velocity, or a solid's (moving or not).
fn moving(x: i32, y: i32) -> vec2<f32> {
    let i = ix(x, y);
    return select(vel[i], obstacle_velocity(), blocked(i));
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
        c = vec4(c.rgb + (st.color - c.rgb) * add, c.a + (1.0 - c.a) * add);
    }
    v.y -= st.rise * c.a * 0.06;
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
        vel[i] = obstacle_velocity();
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
    let carried = obstacle_velocity();
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
    let a = clamp(c.a, 0.0, 1.0);
    let lin = select(vec3(0.0), c.rgb / max(c.a, 1e-4), c.a > 1e-4);
    let l = clamp(lin, vec3(0.0), vec3(1.0));
    let g = select(1.055 * pow(l, vec3(1.0 / 2.4)) - 0.055, l * 12.92, l <= vec3(0.0031308));
    textureStore(out_tex, vec2<i32>(id.xy), vec4(g, a));
}
