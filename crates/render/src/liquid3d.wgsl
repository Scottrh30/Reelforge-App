// A liquid in three dimensions on the graphics card (see liquid3dgpu.rs and
// reelforge_core::water::liquid3d_step): FLIP particles on a staggered grid
// in a box behind the layer. x across the picture, y down, z away from the
// camera; the layer sits on the plane z = plane (its shape, made solid, a slab
// there). Units are grid cells and steps. Each step: pour in, the particles'
// motion onto the grid, gravity, the box's floor and walls and solid objects,
// a pressure solve that keeps the water from squashing, the grid's change back
// onto the particles, and the particles carried along. A particle out of the
// box's front (thrown towards the camera) flies on under gravity alone until
// it's past the camera or fallen away: spray coming out of the picture.
//
// The picture: each particle a little sphere seen through the camera, its
// nearest depth and its thickness summed a pixel at a time, the depth then
// smoothed into a surface (keeping its edges), its normals and thickness
// stored for fs_liquid3d (water.wgsl) to light.
//
// Sums are fixed-point integer atomics, so they come out the same whatever
// order the card runs the particles in.

struct Step {
    w: u32,
    h: u32,
    d: u32,
    cap: u32,
    emit: u32,
    rw: u32,
    rh: u32,
    step: u32,
    seed: u32,
    floor: u32,
    // bit 0: the sides, bit 1: the back
    walls: u32,
    solids: u32,
    // source: middle of the mouth (cells), and the mouth's width
    src: vec4<f32>,
    // direction (unit) and speed (cells a step)
    dir: vec4<f32>,
    // gravity (cells a step², down), FLIP fraction, bounce, damping a step
    phys: vec4<f32>,
    // key tolerance, the starting pool's surface (cells from the top),
    // droplets, the solid slab's half thickness (cells)
    more: vec4<f32>,
    // the camera: position (cells) and focal length (picture pixels)
    cam: vec4<f32>,
    // looking down: cos, sin; the layer's plane z; the nearest a particle is drawn
    view: vec4<f32>,
    // u, v, w, u before forces
    og0: vec4<u32>,
    // v before, w before, pressure, divergence
    og1: vec4<u32>,
    // solid, cell kind, smoothed depth (two)
    og2: vec4<u32>,
    // u sum, u weight, v sum, v weight
    oa0: vec4<u32>,
    // w sum, w weight, particles a cell, the picture's nearest depth
    oa1: vec4<u32>,
    // the picture's thickness, foam, -, -
    oa2: vec4<u32>,
};

const FIX = 65536.0;
const DEAD = -1.0e9;
const PER_CELL = 8.0;
const AIR = 0.0;
const FLUID = 1.0;
const SOLID = 2.0;
// A particle's radius as drawn (cells; they're half a cell apart in full water).
const DRAWN_R = 0.7;
// Depths in the picture: fixed point, this many to a cell.
const DEPTH_FIX = 1024.0;
const FAR: i32 = 0x7fffffff;

@group(0) @binding(0) var<uniform> st: Step;
// Two to a particle: (position, alive) and (velocity, -).
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
fn D() -> i32 { return i32(st.d); }

// Index of cell (i, j, k), and of the faces: u (i 0..=w), v (j 0..=h), w (k 0..=d).
fn ci(i: i32, j: i32, k: i32) -> u32 {
    return (u32(clamp(k, 0, D() - 1)) * st.h + u32(clamp(j, 0, H() - 1))) * st.w + u32(clamp(i, 0, W() - 1));
}
fn ui(i: i32, j: i32, k: i32) -> u32 {
    return (u32(clamp(k, 0, D() - 1)) * st.h + u32(clamp(j, 0, H() - 1))) * (st.w + 1u) + u32(clamp(i, 0, W()));
}
fn vi(i: i32, j: i32, k: i32) -> u32 {
    return (u32(clamp(k, 0, D() - 1)) * (st.h + 1u) + u32(clamp(j, 0, H()))) * st.w + u32(clamp(i, 0, W() - 1));
}
fn wi(i: i32, j: i32, k: i32) -> u32 {
    return (u32(clamp(k, 0, D())) * st.h + u32(clamp(j, 0, H() - 1))) * st.w + u32(clamp(i, 0, W() - 1));
}
// Face `axis` (0 u, 1 v, 2 w) at (i, j, k), and whether it's on the grid.
fn fi(axis: u32, i: i32, j: i32, k: i32) -> u32 {
    switch axis {
        case 0u: { return ui(i, j, k); }
        case 1u: { return vi(i, j, k); }
        default: { return wi(i, j, k); }
    }
}
fn face_on(axis: u32, i: i32, j: i32, k: i32) -> bool {
    let n = vec3<i32>(W(), H(), D()) + vec3<i32>(select(0, 1, axis == 0u), select(0, 1, axis == 1u), select(0, 1, axis == 2u));
    return i >= 0 && j >= 0 && k >= 0 && i < n.x && j < n.y && k < n.z;
}
// Where face `axis`'s samples sit, from a cell's corner.
fn face_off(axis: u32) -> vec3<f32> {
    return vec3(select(0.5, 0.0, axis == 0u), select(0.5, 0.0, axis == 1u), select(0.5, 0.0, axis == 2u));
}

fn in_box(p: vec3<f32>) -> bool {
    return p.x >= 0.0 && p.y >= 0.0 && p.z >= 0.0 && p.x < f32(st.w) && p.y < f32(st.h) && p.z < f32(st.d);
}

fn cell_kind(i: i32, j: i32, k: i32) -> f32 {
    // Outside the box: walls, floor and back where they are, open air elsewhere.
    if (i < 0 || i >= W()) {
        return select(AIR, SOLID, (st.walls & 1u) != 0u);
    }
    if (k >= D()) {
        return select(AIR, SOLID, (st.walls & 2u) != 0u);
    }
    if (j >= H()) {
        return select(AIR, SOLID, st.floor != 0u);
    }
    if (j < 0 || k < 0) {
        return AIR;
    }
    return g[st.og2.y + ci(i, j, k)];
}
fn solid_at(i: i32, j: i32, k: i32) -> bool {
    return cell_kind(i, j, k) == SOLID;
}
fn pressure(i: i32, j: i32, k: i32) -> f32 {
    if (i < 0 || i >= W() || j < 0 || j >= H() || k < 0 || k >= D()) {
        return 0.0;
    }
    return g[st.og1.z + ci(i, j, k)];
}

// Trilinear sample of face component `axis` (from grid offset `base`) at p.
fn sample_face(axis: u32, base: u32, p: vec3<f32>) -> f32 {
    let q = p - face_off(axis);
    let c = vec3<i32>(floor(q));
    let f = q - floor(q);
    var s = 0.0;
    for (var n = 0; n < 8; n++) {
        let o = vec3<i32>(n & 1, (n >> 1u) & 1, (n >> 2u) & 1);
        let wgt = select(1.0 - f.x, f.x, o.x == 1) * select(1.0 - f.y, f.y, o.y == 1) * select(1.0 - f.z, f.z, o.z == 1);
        s += g[base + fi(axis, c.x + o.x, c.y + o.y, c.z + o.z)] * wgt;
    }
    return s;
}
fn grid_vel(p: vec3<f32>) -> vec3<f32> {
    return vec3(sample_face(0u, st.og0.x, p), sample_face(1u, st.og0.y, p), sample_face(2u, st.og0.z, p));
}
fn grid_vel_before(p: vec3<f32>) -> vec3<f32> {
    return vec3(sample_face(0u, st.og0.w, p), sample_face(1u, st.og1.x, p), sample_face(2u, st.og1.y, p));
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

// What's solid: the layer's shape, a slab about its plane (a fraction a cell).
@compute @workgroup_size(4, 4, 4)
fn solids(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= st.w || id.y >= st.h || id.z >= st.d) {
        return;
    }
    var s = 0.0;
    let dz = abs(f32(id.z) + 0.5 - st.view.z);
    if (st.solids != 0u && dz <= st.more.w + 0.5) {
        // four samples a cell, across and down
        for (var n = 0u; n < 4u; n++) {
            let o = vec2(f32(n & 1u), f32(n >> 1u)) * 0.5 + 0.25;
            let uv = (vec2<f32>(id.xy) + o) / vec2<f32>(f32(st.w), f32(st.h));
            s += solidity(textureSampleLevel(layer_tex, layer_samp, uv, 0.0), st.solids, st.more.x) * 0.25;
        }
        // (the slab's front and back faces partly fill their cells)
        s *= clamp(st.more.w + 0.5 - dz + 0.5, 0.0, 1.0);
    }
    g[st.og2.x + ci(i32(id.x), i32(id.y), i32(id.z))] = s;
}

// The starting pool (the first step): the cells below its surface filled.
@compute @workgroup_size(4, 4, 4)
fn fill(@builtin(global_invocation_id) id: vec3<u32>) {
    let y0 = u32(clamp(ceil(st.more.y), 0.0, f32(st.h)));
    let rows = st.h - y0;
    if (id.x >= st.w || id.y >= rows || id.z >= st.d) {
        return;
    }
    let y = y0 + id.y;
    let solid = g[st.og2.x + ci(i32(id.x), i32(y), i32(id.z))] > 0.5;
    let cell = (id.z * rows + id.y) * st.w + id.x;
    for (var n = 0u; n < 8u; n++) {
        let slot = cell * 8u + n;
        if (slot >= st.cap) {
            return;
        }
        let jit = vec3(rnd(slot, st.seed * 5u + 1u), rnd(slot, st.seed * 5u + 2u), rnd(slot, st.seed * 5u + 3u));
        let p = vec3(f32(id.x), f32(y), f32(id.z)) + (vec3(f32(n & 1u), f32((n >> 1u) & 1u), f32(n >> 2u)) + jit) * 0.5;
        parts[slot * 2u] = select(vec4(p, 1.0), vec4(DEAD, DEAD, DEAD, 0.0), solid);
        parts[slot * 2u + 1u] = vec4(0.0);
    }
    if (id.x == 0u && id.y == 0u && id.z == 0u) {
        atomicStore(&a[0], i32(min(rows * st.w * st.d * 8u, st.cap) % st.cap));
    }
}

// Pouring: `emit` particles into the ring of particles, across the round mouth.
@compute @workgroup_size(64)
fn emit(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= st.emit) {
        return;
    }
    let head = u32(atomicLoad(&a[0]));
    let slot = (head + id.x) % st.cap;
    let salt = st.step * 4u + st.seed * 7919u;
    let r = sqrt(rnd(id.x, salt)) * 0.5 * st.src.w;
    let ang = rnd(id.x, salt + 1u) * 6.2831853;
    let along = rnd(id.x, salt + 2u);
    let dir = st.dir.xyz;
    // Two directions across the mouth.
    let up = select(vec3(0.0, 1.0, 0.0), vec3(1.0, 0.0, 0.0), abs(dir.y) > 0.9);
    let e1 = normalize(cross(dir, up));
    let e2 = cross(dir, e1);
    let p = st.src.xyz + (e1 * cos(ang) + e2 * sin(ang)) * r + dir * along * max(st.dir.w, 0.1);
    // Each drop leaves a little off the stream's line (more with more
    // Droplets): a fast burst then fans out and breaks up as it flies, rather
    // than staying one blob.
    let spread = 0.02 + st.dir.w * 0.09 * st.more.z;
    let jitter = (vec3(rnd(id.x, salt + 3u), rnd(id.x, st.step + 77u), rnd(id.x, st.step + 78u)) - 0.5) * 2.0 * spread;
    parts[slot * 2u] = vec4(p, 1.0);
    parts[slot * 2u + 1u] = vec4(dir * st.dir.w + jitter, 0.0);
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

// Clear the sums (a step's, and the picture's).
@compute @workgroup_size(256)
fn clear_sums(@builtin(global_invocation_id) id: vec3<u32>) {
    // (the driver clears the atomics with a buffer clear; this keeps the picture's
    // nearest depth at "nothing")
    let n = st.rw * st.rh;
    if (id.x < n) {
        atomicStore(&a[st.oa1.w + id.x], FAR);
    }
}

// The particles' velocities onto the grid's faces (and a count a cell).
@compute @workgroup_size(256)
fn p2g(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= st.cap) {
        return;
    }
    let pt = parts[id.x * 2u];
    if (!alive(pt) || !in_box(pt.xyz)) {
        return;
    }
    let p = pt.xyz;
    let vel = clamp(parts[id.x * 2u + 1u].xyz, vec3(-4.0), vec3(4.0));
    for (var axis = 0u; axis < 3u; axis++) {
        let q = p - face_off(axis);
        let c = vec3<i32>(floor(q));
        let f = q - floor(q);
        let vs = vel[axis];
        let bs = select(select(st.oa1.x, st.oa0.z, axis == 1u), st.oa0.x, axis == 0u);
        let bw = select(select(st.oa1.y, st.oa0.w, axis == 1u), st.oa0.y, axis == 0u);
        for (var n = 0; n < 8; n++) {
            let o = vec3<i32>(n & 1, (n >> 1u) & 1, (n >> 2u) & 1);
            let cc = c + o;
            if (!face_on(axis, cc.x, cc.y, cc.z)) {
                continue;
            }
            let wgt = select(1.0 - f.x, f.x, o.x == 1) * select(1.0 - f.y, f.y, o.y == 1) * select(1.0 - f.z, f.z, o.z == 1);
            splat_face(bs, bw, fi(axis, cc.x, cc.y, cc.z), wgt, vs);
        }
    }
    let c0 = vec3<i32>(floor(p));
    atomicAdd(&a[st.oa1.z + ci(c0.x, c0.y, c0.z)], 1);
}

// The sums into velocities (kept as "before" for FLIP), and each cell's kind.
@compute @workgroup_size(4, 4, 4)
fn grid(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = i32(id.x);
    let j = i32(id.y);
    let k = i32(id.z);
    if (face_on(0u, i, j, k)) {
        let n = ui(i, j, k);
        let wsum = f32(atomicLoad(&a[st.oa0.y + n]));
        let u = select(0.0, f32(atomicLoad(&a[st.oa0.x + n])) / wsum, wsum > 0.0);
        g[st.og0.x + n] = u;
        g[st.og0.w + n] = u;
    }
    if (face_on(1u, i, j, k)) {
        let n = vi(i, j, k);
        let wsum = f32(atomicLoad(&a[st.oa0.w + n]));
        let v = select(0.0, f32(atomicLoad(&a[st.oa0.z + n])) / wsum, wsum > 0.0);
        g[st.og0.y + n] = v;
        g[st.og1.x + n] = v;
    }
    if (face_on(2u, i, j, k)) {
        let n = wi(i, j, k);
        let wsum = f32(atomicLoad(&a[st.oa1.y + n]));
        let w = select(0.0, f32(atomicLoad(&a[st.oa1.x + n])) / wsum, wsum > 0.0);
        g[st.og0.z + n] = w;
        g[st.og1.y + n] = w;
    }
    if (i < W() && j < H() && k < D()) {
        let c = ci(i, j, k);
        var kind = select(AIR, FLUID, atomicLoad(&a[st.oa1.z + c]) > 0);
        if (g[st.og2.x + c] > 0.5) {
            kind = SOLID;
        }
        g[st.og2.y + c] = kind;
    }
}

fn near_fluid(axis: u32, i: i32, j: i32, k: i32) -> bool {
    let o = vec3<i32>(select(0, 1, axis == 0u), select(0, 1, axis == 1u), select(0, 1, axis == 2u));
    return cell_kind(i - o.x, j - o.y, k - o.z) == FLUID || cell_kind(i, j, k) == FLUID;
}
fn wall_face(axis: u32, i: i32, j: i32, k: i32) -> bool {
    let o = vec3<i32>(select(0, 1, axis == 0u), select(0, 1, axis == 1u), select(0, 1, axis == 2u));
    return solid_at(i - o.x, j - o.y, k - o.z) || solid_at(i, j, k);
}

// Gravity on the water's faces, then no flow into walls or solids.
@compute @workgroup_size(4, 4, 4)
fn forces(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = i32(id.x);
    let j = i32(id.y);
    let k = i32(id.z);
    if (face_on(0u, i, j, k)) {
        let n = ui(i, j, k);
        g[st.og0.x + n] = select(g[st.og0.x + n] * st.phys.w, 0.0, wall_face(0u, i, j, k));
    }
    if (face_on(1u, i, j, k)) {
        let n = vi(i, j, k);
        var v = g[st.og0.y + n];
        if (near_fluid(1u, i, j, k)) {
            v += st.phys.x;
        }
        g[st.og0.y + n] = select(v * st.phys.w, 0.0, wall_face(1u, i, j, k));
    }
    if (face_on(2u, i, j, k)) {
        let n = wi(i, j, k);
        g[st.og0.z + n] = select(g[st.og0.z + n] * st.phys.w, 0.0, wall_face(2u, i, j, k));
    }
}

// How much flows out of each water cell (less a nudge where it's packed too
// tight, so the water keeps its volume).
@compute @workgroup_size(4, 4, 4)
fn divergence(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = i32(id.x);
    let j = i32(id.y);
    let k = i32(id.z);
    if (i >= W() || j >= H() || k >= D()) {
        return;
    }
    let c = ci(i, j, k);
    var dv = 0.0;
    if (cell_kind(i, j, k) == FLUID) {
        dv = g[st.og0.x + ui(i + 1, j, k)] - g[st.og0.x + ui(i, j, k)] + g[st.og0.y + vi(i, j + 1, k)] - g[st.og0.y + vi(i, j, k)] + g[st.og0.z + wi(i, j, k + 1)] - g[st.og0.z + wi(i, j, k)];
        let n = f32(atomicLoad(&a[st.oa1.z + c]));
        dv -= clamp(n / PER_CELL - 1.5, 0.0, 2.0) * 0.05;
    } else {
        g[st.og1.z + c] = 0.0;
    }
    g[st.og1.w + c] = dv;
}

// Red-black Gauss-Seidel with over-relaxation (see liquid.wgsl).
fn relax(i: i32, j: i32, k: i32) {
    let c = ci(i, j, k);
    if (cell_kind(i, j, k) != FLUID) {
        g[st.og1.z + c] = 0.0;
        return;
    }
    var sum = 0.0;
    var n = 0.0;
    for (var m = 0; m < 6; m++) {
        let o = array<vec3<i32>, 6>(vec3(1, 0, 0), vec3(-1, 0, 0), vec3(0, 1, 0), vec3(0, -1, 0), vec3(0, 0, 1), vec3(0, 0, -1))[m];
        let kind = cell_kind(i + o.x, j + o.y, k + o.z);
        if (kind == SOLID) {
            continue;
        }
        n += 1.0;
        if (kind == FLUID) {
            sum += g[st.og1.z + ci(i + o.x, j + o.y, k + o.z)];
        }
    }
    if (n > 0.0) {
        let p = g[st.og1.z + c];
        g[st.og1.z + c] = mix(p, (sum - g[st.og1.w + c]) / n, 1.85);
    }
}

@compute @workgroup_size(4, 4, 4)
fn red(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x < st.w && id.y < st.h && id.z < st.d && ((id.x + id.y + id.z) & 1u) == 0u) {
        relax(i32(id.x), i32(id.y), i32(id.z));
    }
}
@compute @workgroup_size(4, 4, 4)
fn black(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x < st.w && id.y < st.h && id.z < st.d && ((id.x + id.y + id.z) & 1u) == 1u) {
        relax(i32(id.x), i32(id.y), i32(id.z));
    }
}

// Take away the pressure's push: what's left neither squashes nor stretches.
@compute @workgroup_size(4, 4, 4)
fn project(@builtin(global_invocation_id) id: vec3<u32>) {
    let i = i32(id.x);
    let j = i32(id.y);
    let k = i32(id.z);
    if (face_on(0u, i, j, k) && near_fluid(0u, i, j, k) && !wall_face(0u, i, j, k)) {
        g[st.og0.x + ui(i, j, k)] -= pressure(i, j, k) - pressure(i - 1, j, k);
    }
    if (face_on(1u, i, j, k) && near_fluid(1u, i, j, k) && !wall_face(1u, i, j, k)) {
        g[st.og0.y + vi(i, j, k)] -= pressure(i, j, k) - pressure(i, j - 1, k);
    }
    if (face_on(2u, i, j, k) && near_fluid(2u, i, j, k) && !wall_face(2u, i, j, k)) {
        g[st.og0.z + wi(i, j, k)] -= pressure(i, j, k) - pressure(i, j, k - 1);
    }
}

fn solid_frac(p: vec3<f32>) -> f32 {
    let c = vec3<i32>(floor(p));
    if (c.x < 0 || c.x >= W() || c.y < 0 || c.y >= H() || c.z < 0 || c.z >= D()) {
        return 0.0;
    }
    return g[st.og2.x + ci(c.x, c.y, c.z)];
}

// The way out of a solid object at p (from its smooth edge).
fn out_of_solid(p: vec3<f32>) -> vec3<f32> {
    let gr = vec3(
        solid_frac(p + vec3(1.0, 0.0, 0.0)) - solid_frac(p - vec3(1.0, 0.0, 0.0)),
        solid_frac(p + vec3(0.0, 1.0, 0.0)) - solid_frac(p - vec3(0.0, 1.0, 0.0)),
        solid_frac(p + vec3(0.0, 0.0, 1.0)) - solid_frac(p - vec3(0.0, 0.0, 1.0)),
    );
    let l = length(gr);
    return select(vec3(0.0), -gr / l, l > 1e-4);
}

// Where a particle is from the camera: across, down, and how far in front (cells).
fn to_camera(p: vec3<f32>) -> vec3<f32> {
    let r = p - st.cam.xyz;
    let c = st.view.x;
    let s = st.view.y;
    // (the camera looks down by the view's angle: forward (0, s, c), down (0, c, -s))
    return vec3(r.x, r.y * c - r.z * s, r.y * s + r.z * c);
}

// The grid's new motion back onto each particle (FLIP: its own velocity plus
// the grid's change; PIC: the grid's), then carried along and kept out of
// walls. Out of the box, a particle flies under gravity alone.
@compute @workgroup_size(256)
fn g2p(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= st.cap) {
        return;
    }
    let pt = parts[id.x * 2u];
    if (!alive(pt)) {
        return;
    }
    let p = pt.xyz;
    var v = parts[id.x * 2u + 1u].xyz;
    var q: vec3<f32>;
    if (in_box(p)) {
        let pic = grid_vel(p);
        let before = grid_vel_before(p);
        v = mix(pic, v + (pic - before), st.phys.y);
        // (never more than two cells a step)
        let sp = length(v);
        if (sp > 2.0) {
            v *= 2.0 / sp;
        }
        // Carried along the grid's flow (midpoint), a step.
        let mid = p + pic * 0.5;
        let along = select(v, grid_vel(mid), in_box(mid));
        q = p + mix(along, v, 0.5);
    } else {
        // Spray out of the box: ballistic.
        v.y += st.phys.x;
        q = p + v;
    }
    // Out of solid objects.
    if (solid_frac(q) > 0.5) {
        var n = out_of_solid(q);
        if (dot(n, n) < 0.5) {
            n = out_of_solid(p);
        }
        if (dot(n, n) > 0.5) {
            for (var m = 0; m < 6; m++) {
                if (solid_frac(q) <= 0.5) {
                    break;
                }
                q += n * 0.5;
            }
            let vn = dot(v, n);
            if (vn < 0.0) {
                v -= (1.0 + st.phys.z) * vn * n;
            }
        } else {
            q = p;
            v *= -st.phys.z;
        }
    }
    // The box's walls, back and floor.
    let fw = f32(st.w);
    let fh = f32(st.h);
    let fd = f32(st.d);
    let over_box = q.z >= 0.0 && q.z <= fd;
    if ((st.walls & 1u) != 0u && over_box) {
        if (q.x < 0.25) {
            q.x = 0.25;
            v.x = abs(v.x) * st.phys.z;
        }
        if (q.x > fw - 0.25) {
            q.x = fw - 0.25;
            v.x = -abs(v.x) * st.phys.z;
        }
    }
    if ((st.walls & 2u) != 0u && q.z > fd - 0.25) {
        q.z = fd - 0.25;
        v.z = -abs(v.z) * st.phys.z;
    }
    if (st.floor != 0u && over_box && q.y > fh - 0.25 && p.y <= fh) {
        q.y = fh - 0.25;
        v.y = -abs(v.y) * st.phys.z;
    }
    // Gone: past the camera, fallen well below, or flown far off.
    let cz = to_camera(q).z;
    if (cz < st.view.w || q.y > fh * 3.0 || q.y < -fh * 2.0 || q.x < -fw || q.x > fw * 2.0 || q.z > fd * 2.0) {
        parts[id.x * 2u] = vec4(DEAD, DEAD, DEAD, 0.0);
        return;
    }
    // A tiny shuffle so particles don't stack in lines.
    let jit = (vec3(rnd(id.x, st.step * 5u + 11u), rnd(id.x, st.step * 5u + 12u), rnd(id.x, st.step * 5u + 13u)) - 0.5) * 0.002;
    parts[id.x * 2u] = vec4(q + jit, 1.0);
    parts[id.x * 2u + 1u] = vec4(v, 0.0);
}

// ---- The picture ------------------------------------------------------------

// Each particle a sphere through the camera: the nearest depth and the
// thickness a pixel (and foam: lone or fast particles, the spray).
@compute @workgroup_size(256)
fn splat(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= st.cap) {
        return;
    }
    let pt = parts[id.x * 2u];
    if (!alive(pt)) {
        return;
    }
    let cpos = to_camera(pt.xyz);
    if (cpos.z < st.view.w) {
        return;
    }
    let f = st.cam.w;
    let centre = vec2(f32(st.rw), f32(st.rh)) * 0.5 + cpos.xy * f / cpos.z;
    let c0 = vec3<i32>(floor(pt.xyz));
    var crowd = 8;
    if (in_box(pt.xyz)) {
        crowd = atomicLoad(&a[st.oa1.z + ci(c0.x, c0.y, c0.z)]);
    }
    let lone = select(0.0, 1.0, crowd <= 2);
    let speed = length(parts[id.x * 2u + 1u].xyz);
    let foam = clamp((speed - 0.9) * 1.2, 0.0, 1.0) * 0.35 + lone * 0.6;
    let rw_ = DRAWN_R * (1.0 + lone * 0.3 * st.more.z);
    let r = min(rw_ * f / cpos.z, 24.0);
    if (r < 0.3 || centre.x < -r || centre.y < -r || centre.x > f32(st.rw) + r || centre.y > f32(st.rh) + r) {
        return;
    }
    let ri = i32(ceil(r));
    let base = vec2<i32>(floor(centre));
    for (var dy = -ri; dy <= ri; dy++) {
        for (var dx = -ri; dx <= ri; dx++) {
            let q = base + vec2(dx, dy);
            if (q.x < 0 || q.y < 0 || q.x >= i32(st.rw) || q.y >= i32(st.rh)) {
                continue;
            }
            let dd = (vec2<f32>(q) + 0.5 - centre) / max(r, 0.5);
            let t = 1.0 - dot(dd, dd);
            if (t <= 0.0) {
                continue;
            }
            let k = u32(q.y) * st.rw + u32(q.x);
            let half = rw_ * sqrt(t);
            atomicMin(&a[st.oa1.w + k], i32(clamp((cpos.z - half) * DEPTH_FIX, 0.0, 2.0e9)));
            // (thickness in cells, fixed point; foam weighted the same)
            atomicAdd(&a[st.oa2.x + k], i32(2.0 * half * 256.0));
            atomicAdd(&a[st.oa2.y + k], i32(2.0 * half * foam * 256.0));
        }
    }
}

// The nearest depth as a float (0: no water there), to be smoothed.
@compute @workgroup_size(8, 8)
fn depth_in(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= st.rw || id.y >= st.rh) {
        return;
    }
    let k = id.y * st.rw + id.x;
    let n = atomicLoad(&a[st.oa1.w + k]);
    g[st.og2.z + k] = select(f32(n) / DEPTH_FIX, 0.0, n == FAR);
}

// Smoothing the depth into a surface, keeping its edges (a bilateral blur:
// near pixels at much the same depth count; one far in front or behind is
// another body of water). `from`/`to` alternate between the two buffers.
fn smooth_depth(src: u32, dst: u32, id: vec2<u32>) {
    if (id.x >= st.rw || id.y >= st.rh) {
        return;
    }
    let k = id.y * st.rw + id.x;
    let z0 = g[src + k];
    if (z0 <= 0.0) {
        g[dst + k] = 0.0;
        return;
    }
    // (how far a step in depth still counts as the same surface: a couple of particles)
    let sigma = DRAWN_R * 2.6;
    var sum = 0.0;
    var wsum = 0.0;
    for (var dy = -3; dy <= 3; dy++) {
        for (var dx = -3; dx <= 3; dx++) {
            let x = i32(id.x) + dx;
            let y = i32(id.y) + dy;
            if (x < 0 || y < 0 || x >= i32(st.rw) || y >= i32(st.rh)) {
                continue;
            }
            let z = g[src + u32(y) * st.rw + u32(x)];
            if (z <= 0.0) {
                continue;
            }
            let dz = (z - z0) / sigma;
            let wgt = exp(-f32(dx * dx + dy * dy) / 6.0 - dz * dz);
            sum += z * wgt;
            wsum += wgt;
        }
    }
    g[dst + k] = select(z0, sum / wsum, wsum > 0.0);
}
@compute @workgroup_size(8, 8)
fn smooth_ab(@builtin(global_invocation_id) id: vec3<u32>) {
    smooth_depth(st.og2.z, st.og2.w, id.xy);
}
@compute @workgroup_size(8, 8)
fn smooth_ba(@builtin(global_invocation_id) id: vec3<u32>) {
    smooth_depth(st.og2.w, st.og2.z, id.xy);
}

// A picture pixel's point in front of the camera (cells), at depth z.
fn view_point(x: f32, y: f32, z: f32) -> vec3<f32> {
    let c = vec2(f32(st.rw), f32(st.rh)) * 0.5;
    return vec3((vec2(x, y) - c) * z / st.cam.w, z);
}

// The picture for fs_liquid3d: thickness (cells), foam, and the surface's
// normal across and down (towards the camera is -z).
@compute @workgroup_size(8, 8)
fn picture(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= st.rw || id.y >= st.rh) {
        return;
    }
    let x = i32(id.x);
    let y = i32(id.y);
    let k = id.y * st.rw + id.x;
    // Thickness and foam, softened over a few pixels.
    var thick = 0.0;
    var foam = 0.0;
    var wsum = 0.0;
    for (var dy = -1; dy <= 1; dy++) {
        for (var dx = -1; dx <= 1; dx++) {
            let xx = clamp(x + dx, 0, i32(st.rw) - 1);
            let yy = clamp(y + dy, 0, i32(st.rh) - 1);
            let n = u32(yy) * st.rw + u32(xx);
            let wgt = select(1.0, 2.0, dx == 0 && dy == 0);
            thick += f32(atomicLoad(&a[st.oa2.x + n])) / 256.0 * wgt;
            foam += f32(atomicLoad(&a[st.oa2.y + n])) / 256.0 * wgt;
            wsum += wgt;
        }
    }
    thick /= wsum;
    foam = select(0.0, clamp(foam / wsum / max(thick, 1e-3), 0.0, 1.0), thick > 0.0);
    // The normal from the smoothed depth: of the two sides each way, the
    // nearer one in depth (an edge doesn't pull it round).
    let z = g[st.og2.z + k];
    var nrm = vec3(0.0, 0.0, -1.0);
    if (z > 0.0) {
        let p = view_point(f32(x), f32(y), z);
        var dpx = vec3(1.0, 0.0, 0.0);
        var dpy = vec3(0.0, 1.0, 0.0);
        let zr = g[st.og2.z + u32(y) * st.rw + u32(min(x + 1, i32(st.rw) - 1))];
        let zl = g[st.og2.z + u32(y) * st.rw + u32(max(x - 1, 0))];
        let zd = g[st.og2.z + u32(min(y + 1, i32(st.rh) - 1)) * st.rw + u32(x)];
        let zu = g[st.og2.z + u32(max(y - 1, 0)) * st.rw + u32(x)];
        let right = select(p, view_point(f32(x + 1), f32(y), zr), zr > 0.0) - p;
        let left = p - select(p, view_point(f32(x - 1), f32(y), zl), zl > 0.0);
        let down = select(p, view_point(f32(x), f32(y + 1), zd), zd > 0.0) - p;
        let up = p - select(p, view_point(f32(x), f32(y - 1), zu), zu > 0.0);
        dpx = select(left, right, abs(right.z) < abs(left.z) || zl <= 0.0);
        dpy = select(up, down, abs(down.z) < abs(up.z) || zu <= 0.0);
        let c = cross(dpy, dpx);
        if (dot(c, c) > 1e-12) {
            nrm = normalize(c);
            if (nrm.z > 0.0) {
                nrm = -nrm;
            }
        }
    }
    textureStore(out_tex, vec2<i32>(id.xy), vec4(thick, foam, nrm.x, nrm.y));
}
