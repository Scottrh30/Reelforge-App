// The liquid on the graphics card (see watergpu.rs and reelforge_core::water):
// FLIP particles on a staggered grid, seen from the side (y down). Units are
// grid cells and steps. Each step: pour in, the particles' motion onto the
// grid, gravity, walls and solid objects, a pressure solve that keeps the
// water from squashing, the grid's change back onto the particles, and the
// particles carried along and kept out of solids.
//
// Sums onto the grid are fixed-point integer atomics, so they come out the
// same whatever order the card runs the particles in.
//
// g (floats) and a (atomic ints) hold several grids each, at the offsets in
// the step's uniform: u faces (w+1)×h, v faces w×(h+1), cells w×h, and the
// picture's grid rw×rh (twice as fine).

struct Step {
    w: u32,
    h: u32,
    cap: u32,
    emit: u32,
    rw: u32,
    rh: u32,
    step: u32,
    seed: u32,
    src: vec2<f32>,
    dir: vec2<f32>,
    speed: f32,
    width: f32,
    gravity: f32,
    flip: f32,
    bounce: f32,
    tol: f32,
    fill_to: f32,
    droplets: f32,
    floor: u32,
    walls: u32,
    solids: u32,
    damp: f32,
    // u, v, u before forces, v before forces
    og0: vec4<u32>,
    // pressure a, pressure b, divergence, solid
    og1: vec4<u32>,
    // cell kind, -, -, -
    og2: vec4<u32>,
    // u sum, u weight, v sum, v weight
    oa0: vec4<u32>,
    // particles per cell, picture density, picture foam, -
    oa1: vec4<u32>,
};

const FIX = 65536.0;
const DEAD = -1.0e9;
const PER_CELL = 4.0;
const AIR = 0.0;
const FLUID = 1.0;
const SOLID = 2.0;

@group(0) @binding(0) var<uniform> st: Step;
@group(0) @binding(1) var<storage, read_write> parts: array<vec4<f32>>;
@group(0) @binding(2) var<storage, read_write> a: array<atomic<i32>>;
@group(0) @binding(3) var<storage, read_write> g: array<f32>;
@group(0) @binding(4) var out_tex: texture_storage_2d<rgba16float, write>;
@group(1) @binding(0) var layer_tex: texture_2d<f32>;
@group(1) @binding(1) var layer_samp: sampler;

fn hash_u(x: u32) -> u32 {
    var v = x * 747796405u + 2891336453u;
    v = ((v >> ((v >> 28u) + 4u)) ^ v) * 277803737u;
    return (v >> 22u) ^ v;
}
fn rnd(a0: u32, b: u32) -> f32 {
    return f32(hash_u(hash_u(a0) ^ (b * 0x9E3779B9u)) >> 8u) / 16777216.0;
}

fn W() -> i32 { return i32(st.w); }
fn H() -> i32 { return i32(st.h); }

// Index of u face (i, j) (i 0..=w, j 0..h), v face (i, j) (i 0..w, j 0..=h), cell (i, j).
fn ui(i: i32, j: i32) -> u32 { return u32(clamp(j, 0, H() - 1)) * (st.w + 1u) + u32(clamp(i, 0, W())); }
fn vi(i: i32, j: i32) -> u32 { return u32(clamp(j, 0, H())) * st.w + u32(clamp(i, 0, W() - 1)); }
fn ci(i: i32, j: i32) -> u32 { return u32(clamp(j, 0, H() - 1)) * st.w + u32(clamp(i, 0, W() - 1)); }

fn cell_kind(i: i32, j: i32) -> f32 {
    // Outside the grid: walls where there are walls, open air elsewhere.
    if (i < 0 || i >= W()) {
        return select(AIR, SOLID, st.walls != 0u);
    }
    if (j >= H()) {
        return select(AIR, SOLID, st.floor != 0u);
    }
    if (j < 0) {
        return AIR;
    }
    return g[st.og2.x + ci(i, j)];
}
fn solid_at(i: i32, j: i32) -> bool {
    return cell_kind(i, j) == SOLID;
}
fn pressure(i: i32, j: i32) -> f32 {
    if (i < 0 || i >= W() || j < 0 || j >= H()) {
        return 0.0;
    }
    return g[st.og1.x + ci(i, j)];
}

// Bilinear sample of a staggered component at p (cells): offset (ox, oy) of its samples.
fn sample_u(base: u32, p: vec2<f32>) -> f32 {
    let q = p - vec2(0.0, 0.5);
    let i = i32(floor(q.x));
    let j = i32(floor(q.y));
    let f = q - floor(q);
    let a0 = g[base + ui(i, j)];
    let b0 = g[base + ui(i + 1, j)];
    let c0 = g[base + ui(i, j + 1)];
    let d0 = g[base + ui(i + 1, j + 1)];
    return mix(mix(a0, b0, f.x), mix(c0, d0, f.x), f.y);
}
fn sample_v(base: u32, p: vec2<f32>) -> f32 {
    let q = p - vec2(0.5, 0.0);
    let i = i32(floor(q.x));
    let j = i32(floor(q.y));
    let f = q - floor(q);
    let a0 = g[base + vi(i, j)];
    let b0 = g[base + vi(i + 1, j)];
    let c0 = g[base + vi(i, j + 1)];
    let d0 = g[base + vi(i + 1, j + 1)];
    return mix(mix(a0, b0, f.x), mix(c0, d0, f.x), f.y);
}
fn grid_vel(p: vec2<f32>) -> vec2<f32> {
    return vec2(sample_u(st.og0.x, p), sample_v(st.og0.y, p));
}

// How solid a colour is (see surface.wgsl).
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

// What's solid, from the layer (a fraction a cell, kept smooth for the normals).
@compute @workgroup_size(8, 8)
fn solids(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= st.w || id.y >= st.h) {
        return;
    }
    var s = 0.0;
    if (st.solids != 0u) {
        // four samples a cell
        for (var k = 0u; k < 4u; k++) {
            let o = vec2(f32(k & 1u), f32(k >> 1u)) * 0.5 + 0.25;
            let uv = (vec2<f32>(id.xy) + o) / vec2<f32>(f32(st.w), f32(st.h));
            s += solidity(textureSampleLevel(layer_tex, layer_samp, uv, 0.0), st.solids, st.tol) * 0.25;
        }
    }
    g[st.og1.w + id.y * st.w + id.x] = s;
}

// The starting pool (the first step): the cells below its surface filled.
@compute @workgroup_size(8, 8)
fn fill(@builtin(global_invocation_id) id: vec3<u32>) {
    let y0 = u32(clamp(ceil(st.fill_to), 0.0, f32(st.h)));
    let rows = st.h - y0;
    if (id.x >= st.w || id.y >= rows) {
        return;
    }
    let x = id.x;
    let y = y0 + id.y;
    let solid = g[st.og1.w + y * st.w + x] > 0.5;
    for (var k = 0u; k < 4u; k++) {
        let slot = (id.y * st.w + x) * 4u + k;
        if (slot >= st.cap) {
            return;
        }
        let jit = vec2(rnd(slot, st.seed * 3u + 1u), rnd(slot, st.seed * 3u + 2u));
        let p = vec2(f32(x), f32(y)) + (vec2(f32(k & 1u), f32(k >> 1u)) + jit) * 0.5;
        parts[slot] = select(vec4(p, 0.0, 0.0), vec4(DEAD, DEAD, 0.0, 0.0), solid);
    }
    if (id.x == 0u && id.y == 0u) {
        atomicStore(&a[0], i32(min(rows * st.w * 4u, st.cap)) % i32(st.cap));
    }
}

// Pouring: `emit` particles into the ring of particles, across the mouth.
@compute @workgroup_size(64)
fn emit(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= st.emit) {
        return;
    }
    let head = u32(atomicLoad(&a[0]));
    let slot = (head + id.x) % st.cap;
    let r1 = rnd(id.x, st.step * 2u + st.seed * 7919u);
    let r2 = rnd(id.x, st.step * 2u + 1u + st.seed * 7919u);
    let side = vec2(-st.dir.y, st.dir.x);
    let p = st.src + side * (r1 - 0.5) * st.width + st.dir * (r2 * max(st.speed, 0.1));
    let jitter = (vec2(rnd(id.x, st.step + 77u), rnd(id.x, st.step + 78u)) - 0.5) * 0.02;
    parts[slot] = vec4(p, st.dir * st.speed + jitter);
}

@compute @workgroup_size(1)
fn advance(@builtin(global_invocation_id) id: vec3<u32>) {
    let head = u32(atomicLoad(&a[0]));
    atomicStore(&a[0], i32((head + st.emit) % st.cap));
}

fn alive(p: vec4<f32>) -> bool {
    return p.x > DEAD * 0.5;
}

fn splat_face(base_s: u32, base_w: u32, idx: u32, wgt: f32, v: f32) {
    if (wgt <= 0.0) {
        return;
    }
    atomicAdd(&a[base_s + idx], i32(round(wgt * v * FIX)));
    atomicAdd(&a[base_w + idx], i32(round(wgt * FIX)));
}

// The particles' velocities onto the grid's faces (and a count a cell).
@compute @workgroup_size(256)
fn p2g(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= st.cap) {
        return;
    }
    let pt = parts[id.x];
    if (!alive(pt)) {
        return;
    }
    let p = pt.xy;
    let vel = clamp(pt.zw, vec2(-4.0), vec2(4.0));
    // u faces sit at (i, j + 0.5)
    {
        let q = p - vec2(0.0, 0.5);
        let i = i32(floor(q.x));
        let j = i32(floor(q.y));
        let f = q - floor(q);
        for (var k = 0; k < 4; k++) {
            let di = k & 1;
            let dj = k >> 1u;
            let ii = i + di;
            let jj = j + dj;
            if (ii < 0 || ii > W() || jj < 0 || jj >= H()) {
                continue;
            }
            let wx = select(1.0 - f.x, f.x, di == 1);
            let wy = select(1.0 - f.y, f.y, dj == 1);
            splat_face(st.oa0.x, st.oa0.y, ui(ii, jj), wx * wy, vel.x);
        }
    }
    {
        let q = p - vec2(0.5, 0.0);
        let i = i32(floor(q.x));
        let j = i32(floor(q.y));
        let f = q - floor(q);
        for (var k = 0; k < 4; k++) {
            let di = k & 1;
            let dj = k >> 1u;
            let ii = i + di;
            let jj = j + dj;
            if (ii < 0 || ii >= W() || jj < 0 || jj > H()) {
                continue;
            }
            let wx = select(1.0 - f.x, f.x, di == 1);
            let wy = select(1.0 - f.y, f.y, dj == 1);
            splat_face(st.oa0.z, st.oa0.w, vi(ii, jj), wx * wy, vel.y);
        }
    }
    let ci0 = vec2<i32>(floor(p));
    if (ci0.x >= 0 && ci0.x < W() && ci0.y >= 0 && ci0.y < H()) {
        atomicAdd(&a[st.oa1.x + ci(ci0.x, ci0.y)], 1);
    }
}

// The sums into velocities (kept as "before" for FLIP), and each cell's kind.
@compute @workgroup_size(8, 8)
fn grid(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = i32(id.x);
    let j = i32(id.y);
    if (i <= W() && j < H()) {
        let k = ui(i, j);
        let wsum = f32(atomicLoad(&a[st.oa0.y + k]));
        let u = select(0.0, f32(atomicLoad(&a[st.oa0.x + k])) / wsum, wsum > 0.0);
        g[st.og0.x + k] = u;
        g[st.og0.z + k] = u;
    }
    if (i < W() && j <= H()) {
        let k = vi(i, j);
        let wsum = f32(atomicLoad(&a[st.oa0.w + k]));
        let v = select(0.0, f32(atomicLoad(&a[st.oa0.z + k])) / wsum, wsum > 0.0);
        g[st.og0.y + k] = v;
        g[st.og0.w + k] = v;
    }
    if (i < W() && j < H()) {
        let c = ci(i, j);
        let n = atomicLoad(&a[st.oa1.x + c]);
        var kind = select(AIR, FLUID, n > 0);
        if (g[st.og1.w + c] > 0.5) {
            kind = SOLID;
        }
        g[st.og2.x + c] = kind;
    }
}

fn near_fluid_u(i: i32, j: i32) -> bool {
    return cell_kind(i - 1, j) == FLUID || cell_kind(i, j) == FLUID;
}
fn near_fluid_v(i: i32, j: i32) -> bool {
    return cell_kind(i, j - 1) == FLUID || cell_kind(i, j) == FLUID;
}

// Gravity on the water's faces, then no flow into walls or solids.
@compute @workgroup_size(8, 8)
fn forces(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = i32(id.x);
    let j = i32(id.y);
    if (i <= W() && j < H()) {
        let k = ui(i, j);
        var u = g[st.og0.x + k] * st.damp;
        if (solid_at(i - 1, j) || solid_at(i, j)) {
            u = 0.0;
        }
        g[st.og0.x + k] = u;
    }
    if (i < W() && j <= H()) {
        let k = vi(i, j);
        var v = g[st.og0.y + k];
        if (near_fluid_v(i, j)) {
            v += st.gravity;
        }
        v *= st.damp;
        if (solid_at(i, j - 1) || solid_at(i, j)) {
            v = 0.0;
        }
        g[st.og0.y + k] = v;
    }
}

// How much flows out of each water cell (less a nudge where it's packed too
// tight, so the water keeps its volume).
@compute @workgroup_size(8, 8)
fn divergence(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = i32(id.x);
    let j = i32(id.y);
    if (i >= W() || j >= H()) {
        return;
    }
    let c = ci(i, j);
    var d = 0.0;
    if (cell_kind(i, j) == FLUID) {
        d = g[st.og0.x + ui(i + 1, j)] - g[st.og0.x + ui(i, j)] + g[st.og0.y + vi(i, j + 1)] - g[st.og0.y + vi(i, j)];
        let n = f32(atomicLoad(&a[st.oa1.x + c]));
        d -= clamp(n / PER_CELL - 1.5, 0.0, 2.0) * 0.05;
    }
    g[st.og1.z + c] = d;
    // (the last step's pressure is where the solve starts: it's nearly right already)
    if (cell_kind(i, j) != FLUID) {
        g[st.og1.x + c] = 0.0;
    }
}

// Red-black Gauss-Seidel with over-relaxation: half the cells (a
// checkerboard) at a time, in place; converges far faster than Jacobi, so
// deep water doesn't squash.
fn relax(i: i32, j: i32) {
    let c = ci(i, j);
    if (cell_kind(i, j) != FLUID) {
        g[st.og1.x + c] = 0.0;
        return;
    }
    var sum = 0.0;
    var n = 0.0;
    for (var k = 0; k < 4; k++) {
        let o = array<vec2<i32>, 4>(vec2(1, 0), vec2(-1, 0), vec2(0, 1), vec2(0, -1))[k];
        let kind = cell_kind(i + o.x, j + o.y);
        if (kind == SOLID) {
            continue;
        }
        n += 1.0;
        if (kind == FLUID) {
            sum += g[st.og1.x + ci(i + o.x, j + o.y)];
        }
    }
    if (n > 0.0) {
        let p = g[st.og1.x + c];
        g[st.og1.x + c] = mix(p, (sum - g[st.og1.z + c]) / n, 1.85);
    }
}

@compute @workgroup_size(8, 8)
fn red(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x < st.w && id.y < st.h && ((id.x + id.y) & 1u) == 0u) {
        relax(i32(id.x), i32(id.y));
    }
}
@compute @workgroup_size(8, 8)
fn black(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x < st.w && id.y < st.h && ((id.x + id.y) & 1u) == 1u) {
        relax(i32(id.x), i32(id.y));
    }
}

// Take away the pressure's push: what's left neither squashes nor stretches.
@compute @workgroup_size(8, 8)
fn project(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = i32(id.x);
    let j = i32(id.y);
    if (i <= W() && j < H()) {
        let k = ui(i, j);
        if (near_fluid_u(i, j) && !solid_at(i - 1, j) && !solid_at(i, j)) {
            g[st.og0.x + k] -= pressure(i, j) - pressure(i - 1, j);
        }
    }
    if (i < W() && j <= H()) {
        let k = vi(i, j);
        if (near_fluid_v(i, j) && !solid_at(i, j - 1) && !solid_at(i, j)) {
            g[st.og0.y + k] -= pressure(i, j) - pressure(i, j - 1);
        }
    }
}

fn solid_frac(p: vec2<f32>) -> f32 {
    let i = i32(floor(p.x));
    let j = i32(floor(p.y));
    if (i < 0 || i >= W() || j < 0 || j >= H()) {
        return 0.0;
    }
    return g[st.og1.w + ci(i, j)];
}

// The way out of a solid object at p (from its smooth edge).
fn out_of_solid(p: vec2<f32>) -> vec2<f32> {
    let e = 1.0;
    let gx = solid_frac(p + vec2(e, 0.0)) - solid_frac(p - vec2(e, 0.0));
    let gy = solid_frac(p + vec2(0.0, e)) - solid_frac(p - vec2(0.0, e));
    let gr = vec2(gx, gy);
    let l = length(gr);
    return select(vec2(0.0), -gr / l, l > 1e-4);
}

// The grid's new motion back onto each particle (FLIP: its own velocity plus
// the grid's change; PIC: the grid's), then carried along and kept out of walls.
@compute @workgroup_size(256)
fn g2p(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= st.cap) {
        return;
    }
    var pt = parts[id.x];
    if (!alive(pt)) {
        return;
    }
    let p = pt.xy;
    let pic = grid_vel(p);
    let before = vec2(sample_u(st.og0.z, p), sample_v(st.og0.w, p));
    var v = mix(pic, pt.zw + (pic - before), st.flip);
    // (never more than two cells a step)
    let sp = length(v);
    if (sp > 2.0) {
        v *= 2.0 / sp;
    }
    // Carried along the grid's flow (midpoint), a step.
    let mid = p + grid_vel(p) * 0.5;
    var q = p + mix(grid_vel(mid), v, 0.5);
    // Out of solid objects.
    if (solid_frac(q) > 0.5) {
        var n = out_of_solid(q);
        if (dot(n, n) < 0.5) {
            n = out_of_solid(p);
        }
        if (dot(n, n) > 0.5) {
            for (var k = 0; k < 6; k++) {
                if (solid_frac(q) <= 0.5) {
                    break;
                }
                q += n * 0.5;
            }
            let vn = dot(v, n);
            if (vn < 0.0) {
                v -= (1.0 + st.bounce) * vn * n;
            }
        } else {
            q = p;
            v *= -st.bounce;
        }
    }
    // The frame's walls and floor.
    let fw = f32(st.w);
    let fh = f32(st.h);
    if (st.walls != 0u) {
        if (q.x < 0.25) {
            q.x = 0.25;
            v.x = abs(v.x) * st.bounce;
        }
        if (q.x > fw - 0.25) {
            q.x = fw - 0.25;
            v.x = -abs(v.x) * st.bounce;
        }
    }
    if (st.floor != 0u && q.y > fh - 0.25) {
        q.y = fh - 0.25;
        v.y = -abs(v.y) * st.bounce;
    }
    // Gone out of the frame (open edges, or thrown far above).
    if (q.x < -2.0 || q.x > fw + 2.0 || q.y > fh + 2.0 || q.y < -fh) {
        parts[id.x] = vec4(DEAD, DEAD, 0.0, 0.0);
        return;
    }
    // A tiny shuffle so particles don't stack in lines.
    let jit = (vec2(rnd(id.x, st.step * 5u + 11u), rnd(id.x, st.step * 5u + 12u)) - 0.5) * 0.002;
    parts[id.x] = vec4(q + jit, v);
}

// ---- The picture: particles splatted onto a grid twice as fine ----

@compute @workgroup_size(256)
fn splat(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= st.cap) {
        return;
    }
    let pt = parts[id.x];
    if (!alive(pt)) {
        return;
    }
    let s = vec2(f32(st.rw) / f32(st.w), f32(st.rh) / f32(st.h));
    let p = pt.xy * s;
    // Lone, fast particles are spray: drawn a little bigger, and as foam.
    let c = vec2<i32>(floor(pt.xy));
    var crowd = 4;
    if (c.x >= 0 && c.x < W() && c.y >= 0 && c.y < H()) {
        crowd = atomicLoad(&a[st.oa1.x + ci(c.x, c.y)]);
    }
    let lone = select(0.0, 1.0, crowd <= 1);
    let speed = length(pt.zw);
    let foam = clamp((speed - 0.9) * 1.2, 0.0, 1.0) * 0.35 + lone * 0.6;
    let r = 2.0 * (1.0 + lone * 0.35 * st.droplets);
    let boost = 1.0 + lone * st.droplets * 1.2;
    let ri = i32(ceil(r));
    let base = vec2<i32>(floor(p));
    for (var dy = -ri; dy <= ri; dy++) {
        for (var dx = -ri; dx <= ri; dx++) {
            let q = base + vec2(dx, dy);
            if (q.x < 0 || q.y < 0 || q.x >= i32(st.rw) || q.y >= i32(st.rh)) {
                continue;
            }
            let d = (vec2<f32>(q) + 0.5 - p) / r;
            let t = max(1.0 - dot(d, d), 0.0);
            if (t <= 0.0) {
                continue;
            }
            let wgt = t * t;
            let k = u32(q.y) * st.rw + u32(q.x);
            atomicAdd(&a[st.oa1.y + k], i32(wgt * boost * 256.0));
            atomicAdd(&a[st.oa1.z + k], i32(wgt * foam * 256.0));
        }
    }
}

// Density (1 inside the water, 0.5 at its surface), foam, and what's solid.
@compute @workgroup_size(8, 8)
fn picture(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= st.rw || id.y >= st.rh) {
        return;
    }
    let k = id.y * st.rw + id.x;
    // A full cell: one particle per picture cell, each spread over about 4.2 of them.
    let full = 4.19 * 256.0;
    let d = f32(atomicLoad(&a[st.oa1.y + k])) / full;
    let f = f32(atomicLoad(&a[st.oa1.z + k])) / full;
    let cx = i32(f32(id.x) * f32(st.w) / f32(st.rw));
    let cy = i32(f32(id.y) * f32(st.h) / f32(st.rh));
    let s = g[st.og1.w + ci(cx, cy)];
    textureStore(out_tex, vec2<i32>(id.xy), vec4(d, f, s, 1.0));
}
