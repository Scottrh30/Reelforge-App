// Optical-flow frame interpolation on the graphics card (see flowgpu.rs).
//
// Dense motion both ways between two frames (coarse to fine over a pyramid:
// candidate propagation from neighbours, a local search, a sub-pixel
// Lucas–Kanade step and an edge-aware smoothing per iteration), a
// forward–backward check that finds what each frame sees that the other
// doesn't (things passing in front of others), then the in-between frame:
// each frame's pixels are pushed to where they are at time t, where two land
// on the same spot the one still visible in the other frame wins (the thing
// in front), and each output pixel takes its colour from whichever frames
// can see it.

struct Params {
    w: u32,
    h: u32,
    cw: u32,
    ch: u32,
    ow: u32,
    oh: u32,
    radius: i32,
    wide: i32,
    t: f32,
    s: f32,
    flags: u32,
    lambda: f32,
};

@group(0) @binding(0) var<uniform> prm: Params;
@group(0) @binding(1) var texA: texture_2d<f32>;
@group(0) @binding(2) var texB: texture_2d<f32>;
@group(0) @binding(3) var samp: sampler;
@group(0) @binding(4) var<storage, read> gA: array<f32>;
@group(0) @binding(5) var<storage, read> gB: array<f32>;
@group(0) @binding(6) var<storage, read_write> gOut: array<f32>;
@group(0) @binding(7) var<storage, read> fIn: array<vec4<f32>>;
@group(0) @binding(8) var<storage, read_write> fOut: array<vec4<f32>>;
@group(0) @binding(9) var<storage, read> fOther: array<vec4<f32>>;
@group(0) @binding(10) var<storage, read_write> projA: array<atomic<u32>>;
@group(0) @binding(11) var<storage, read_write> projB: array<atomic<u32>>;
@group(0) @binding(12) var<storage, read_write> tflow: array<vec4<f32>>;
@group(0) @binding(13) var<storage, read_write> vis: array<vec4<f32>>;
@group(0) @binding(14) var outTex: texture_storage_2d<rgba16float, write>;

const NONE: u32 = 0xffffffffu;

// The brightness matched on: the three channels alike (gamma-encoded), so
// a change of colour shows even where luma barely changes.
fn luma(c: vec4<f32>) -> f32 {
    let g = pow(clamp(c.rgb, vec3<f32>(0.0), vec3<f32>(1.0)), vec3<f32>(1.0 / 2.2));
    return (g.r + g.g + g.b) * 85.0;
}

fn ci(x: i32, n: u32) -> u32 {
    return u32(clamp(x, 0, i32(n) - 1));
}

fn a_at(x: i32, y: i32) -> f32 {
    return gA[ci(y, prm.h) * prm.w + ci(x, prm.w)];
}

fn b_at(x: i32, y: i32) -> f32 {
    return gB[ci(y, prm.h) * prm.w + ci(x, prm.w)];
}

// gB, bilinear, at a point in pixels (pixel centres at whole numbers).
fn sb(p: vec2<f32>) -> f32 {
    let f = floor(p);
    let r = p - f;
    let x = i32(f.x);
    let y = i32(f.y);
    return mix(mix(b_at(x, y), b_at(x + 1, y), r.x), mix(b_at(x, y + 1), b_at(x + 1, y + 1), r.x), r.y);
}

// A flow buffer (fIn or fOther, `cw` × `ch`), bilinear.
fn fin_at(x: i32, y: i32, w: u32, h: u32) -> vec4<f32> {
    return fIn[ci(y, h) * w + ci(x, w)];
}
fn fo_at(x: i32, y: i32, w: u32, h: u32) -> vec4<f32> {
    return fOther[ci(y, h) * w + ci(x, w)];
}
fn fo_bilinear(p: vec2<f32>, w: u32, h: u32) -> vec4<f32> {
    let f = floor(p);
    let r = p - f;
    let x = i32(f.x);
    let y = i32(f.y);
    return mix(mix(fo_at(x, y, w, h), fo_at(x + 1, y, w, h), r.x), mix(fo_at(x, y + 1, w, h), fo_at(x + 1, y + 1, w, h), r.x), r.y);
}

// ---- Grey pyramid -------------------------------------------------------------

@compute @workgroup_size(8, 8)
fn grey_tex(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= prm.w || id.y >= prm.h) {
        return;
    }
    let size = vec2<f32>(f32(prm.w), f32(prm.h));
    var acc = 0.0;
    for (var j = 0; j < 2; j++) {
        for (var i = 0; i < 2; i++) {
            let uv = (vec2<f32>(id.xy) + vec2<f32>(0.25 + 0.5 * f32(i), 0.25 + 0.5 * f32(j))) / size;
            acc += luma(textureSampleLevel(texA, samp, uv, 0.0));
        }
    }
    gOut[id.y * prm.w + id.x] = acc * 0.25;
}

// Half size: gA is the finer level (`cw` × `ch`).
@compute @workgroup_size(8, 8)
fn grey_down(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= prm.w || id.y >= prm.h) {
        return;
    }
    let x = i32(id.x) * 2;
    let y = i32(id.y) * 2;
    let at = vec4<u32>(ci(y, prm.ch) * prm.cw + ci(x, prm.cw), ci(y, prm.ch) * prm.cw + ci(x + 1, prm.cw), ci(y + 1, prm.ch) * prm.cw + ci(x, prm.cw), ci(y + 1, prm.ch) * prm.cw + ci(x + 1, prm.cw));
    gOut[id.y * prm.w + id.x] = (gA[at.x] + gA[at.y] + gA[at.z] + gA[at.w]) * 0.25;
}

// ---- Motion -----------------------------------------------------------------------

@compute @workgroup_size(8, 8)
fn flow_zero(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= prm.w || id.y >= prm.h) {
        return;
    }
    fOut[id.y * prm.w + id.x] = vec4<f32>(0.0);
}

// The coarser level's motion (fOther, `cw` × `ch`) brought up to this size.
@compute @workgroup_size(8, 8)
fn flow_up(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= prm.w || id.y >= prm.h) {
        return;
    }
    let k = vec2<f32>(f32(prm.cw) / f32(prm.w), f32(prm.ch) / f32(prm.h));
    let q = (vec2<f32>(id.xy) + 0.5) * k - 0.5;
    let v = fo_bilinear(q, prm.cw, prm.ch);
    fOut[id.y * prm.w + id.x] = vec4<f32>(v.xy / k, v.z, 0.0);
}

fn mean_a(p: vec2<i32>) -> f32 {
    let r = prm.radius;
    var s = 0.0;
    for (var j = -r; j <= r; j++) {
        for (var i = -r; i <= r; i++) {
            s += a_at(p.x + i, p.y + j);
        }
    }
    let n = f32((2 * r + 1) * (2 * r + 1));
    return s / n;
}

// How unlike the patch round p in A is the one round p + d in B, each
// less its own mean (so a change of brightness doesn't count): the spread
// of their difference, in one pass. Plus a little for moving away from
// `prior`. A sparse patch (flag 4) takes every other pixel: as wide, a
// quarter of the work (for the large working sizes).
fn cost(p: vec2<i32>, d: vec2<f32>, ma: f32, prior: vec2<f32>) -> f32 {
    let r = prm.radius;
    let step = select(1, 2, (prm.flags & 4u) != 0u);
    let base = vec2<f32>(p) + d;
    var s1 = 0.0;
    var s2 = 0.0;
    var n = 0.0;
    for (var j = -r; j <= r; j += step) {
        for (var i = -r; i <= r; i += step) {
            let e = a_at(p.x + i, p.y + j) - sb(base + vec2<f32>(f32(i), f32(j)));
            s1 += e;
            s2 += e * e;
            n += 1.0;
        }
    }
    let m = s1 / n;
    return sqrt(max(s2 / n - m * m, 0.0)) + prm.lambda * length(d - prior);
}

@compute @workgroup_size(8, 8)
fn search(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= prm.w || id.y >= prm.h) {
        return;
    }
    let p = vec2<i32>(id.xy);
    let idx = id.y * prm.w + id.x;
    let ma = mean_a(p);
    let prior = fIn[idx].xy;
    var best = prior;
    var bc = cost(p, prior, ma, prior);
    // Standing still (most of most pictures).
    let still = cost(p, vec2<f32>(0.0), ma, prior);
    if (still < bc) {
        bc = still;
        best = vec2<f32>(0.0);
    }
    // Neighbours' motion (spreads good matches along a surface).
    var offs = array<vec2<i32>, 6>(vec2<i32>(1, 0), vec2<i32>(-1, 0), vec2<i32>(0, 1), vec2<i32>(0, -1), vec2<i32>(3, 3), vec2<i32>(-3, -3));
    for (var k = 0; k < 6; k++) {
        let q = p + offs[k];
        let c = fin_at(q.x, q.y, prm.w, prm.h).xy;
        let cc = cost(p, c, ma, prior);
        if (cc < bc) {
            bc = cc;
            best = c;
        }
    }
    // Wide search (the coarsest level, where everything starts still).
    if (prm.wide != 0) {
        let c0 = best;
        let span = prm.wide;
        for (var dy = -span; dy <= span; dy++) {
            for (var dx = -span; dx <= span; dx++) {
                let c = c0 + vec2<f32>(f32(dx), f32(dy));
                let cc = cost(p, c, ma, prior);
                if (cc < bc) {
                    bc = cc;
                    best = c;
                }
            }
        }
    }
    // A step either way.
    let c1 = best;
    for (var dy = -1; dy <= 1; dy++) {
        for (var dx = -1; dx <= 1; dx++) {
            if (dx == 0 && dy == 0) {
                continue;
            }
            let c = c1 + vec2<f32>(f32(dx), f32(dy));
            let cc = cost(p, c, ma, prior);
            if (cc < bc) {
                bc = cc;
                best = c;
            }
        }
    }
    // Sub-pixel: one Lucas–Kanade step.
    let r = prm.radius;
    var mb = 0.0;
    for (var j = -r; j <= r; j++) {
        for (var i = -r; i <= r; i++) {
            mb += sb(vec2<f32>(p) + best + vec2<f32>(f32(i), f32(j)));
        }
    }
    mb /= f32((2 * r + 1) * (2 * r + 1));
    var hm = vec3<f32>(0.0);
    var bv = vec2<f32>(0.0);
    for (var j = -r; j <= r; j++) {
        for (var i = -r; i <= r; i++) {
            let q = vec2<f32>(p) + best + vec2<f32>(f32(i), f32(j));
            let gx = (sb(q + vec2<f32>(1.0, 0.0)) - sb(q - vec2<f32>(1.0, 0.0))) * 0.5;
            let gy = (sb(q + vec2<f32>(0.0, 1.0)) - sb(q - vec2<f32>(0.0, 1.0))) * 0.5;
            let e = sb(q) - mb - (a_at(p.x + i, p.y + j) - ma);
            hm += vec3<f32>(gx * gx, gx * gy, gy * gy);
            bv += vec2<f32>(gx * e, gy * e);
        }
    }
    let det = hm.x * hm.z - hm.y * hm.y;
    if (det > 1e-2) {
        let dl = -vec2<f32>(hm.z * bv.x - hm.y * bv.y, -hm.y * bv.x + hm.x * bv.y) / det;
        let c = best + clamp(dl, vec2<f32>(-1.0), vec2<f32>(1.0));
        let cc = cost(p, c, ma, prior);
        if (cc < bc) {
            bc = cc;
            best = c;
        }
    }
    fOut[idx] = vec4<f32>(best, bc, 0.0);
}

// Edge-aware smoothing: a weighted median of the 5 × 5 neighbourhood's
// motion (each axis), neighbours counting for more when they look alike
// (the same surface) and matched well. A median keeps the edges between
// things moving differently and drops odd ones out.
@compute @workgroup_size(8, 8)
fn smooth_flow(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= prm.w || id.y >= prm.h) {
        return;
    }
    let p = vec2<i32>(id.xy);
    let gp = a_at(p.x, p.y);
    var xs: array<f32, 25>;
    var ys: array<f32, 25>;
    var ws: array<f32, 25>;
    var total = 0.0;
    var n = 0;
    for (var j = -2; j <= 2; j++) {
        for (var i = -2; i <= 2; i++) {
            let f = fin_at(p.x + i, p.y + j, prm.w, prm.h);
            let dg = a_at(p.x + i, p.y + j) - gp;
            let wt = exp(-dg * dg / 200.0) * exp(-f.z / 6.0) * exp(-f32(i * i + j * j) / 6.0) + 1e-6;
            xs[n] = f.x;
            ys[n] = f.y;
            ws[n] = wt;
            total += wt;
            n++;
        }
    }
    let own = fIn[id.y * prm.w + id.x];
    fOut[id.y * prm.w + id.x] = vec4<f32>(wmedian(&xs, &ws, total), wmedian(&ys, &ws, total), own.z, 0.0);
}

// The weighted median of 25 values.
fn wmedian(v: ptr<function, array<f32, 25>>, w: ptr<function, array<f32, 25>>, total: f32) -> f32 {
    var vs = *v;
    var wv = *w;
    // Insertion sort by value, weights alongside.
    for (var i = 1; i < 25; i++) {
        let x = vs[i];
        let wx = wv[i];
        var j = i - 1;
        while (j >= 0 && vs[j] > x) {
            vs[j + 1] = vs[j];
            wv[j + 1] = wv[j];
            j--;
        }
        vs[j + 1] = x;
        wv[j + 1] = wx;
    }
    var acc = 0.0;
    for (var i = 0; i < 25; i++) {
        acc += wv[i];
        if (acc >= total * 0.5) {
            return vs[i];
        }
    }
    return vs[24];
}

// What A sees that B doesn't: following the motion there and back again
// doesn't come home (or it leaves the frame). Kept in w (0–1).
@compute @workgroup_size(8, 8)
fn occlusion(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= prm.w || id.y >= prm.h) {
        return;
    }
    let idx = id.y * prm.w + id.x;
    let f = fIn[idx];
    let q = vec2<f32>(id.xy) + f.xy;
    var occ = 1.0;
    if (q.x >= -0.5 && q.y >= -0.5 && q.x <= f32(prm.w) - 0.5 && q.y <= f32(prm.h) - 0.5) {
        let back = fo_bilinear(q, prm.w, prm.h).xy;
        let err = length(f.xy + back);
        occ = smoothstep(0.75, 2.0, err - 0.05 * length(f.xy));
    }
    fOut[idx] = vec4<f32>(f.xy, f.z, occ);
}

// Where A's pixels are hidden in B their motion can't be measured: take it
// from nearby pixels that can be seen in both and look alike (the same
// surface, usually the one going behind). Run with growing strides
// (`radius`) it reaches across wide hidden bands; `wide` set takes any
// neighbour at all (the last passes). Filled pixels get a cost of -1.
@compute @workgroup_size(8, 8)
fn fill(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= prm.w || id.y >= prm.h) {
        return;
    }
    let idx = id.y * prm.w + id.x;
    let f = fIn[idx];
    if (f.w <= 0.5 || f.z < 0.0) {
        fOut[idx] = f;
        return;
    }
    let p = vec2<i32>(id.xy);
    let gp = a_at(p.x, p.y);
    let stride = max(prm.radius, 1);
    var acc = vec2<f32>(0.0);
    var ws = 0.0;
    for (var j = -3; j <= 3; j++) {
        for (var i = -3; i <= 3; i++) {
            let q = p + vec2<i32>(i, j) * stride;
            if (q.x < 0 || q.y < 0 || q.x >= i32(prm.w) || q.y >= i32(prm.h)) {
                continue;
            }
            let n = fin_at(q.x, q.y, prm.w, prm.h);
            if (n.w <= 0.5 || n.z < 0.0) {
                // Alike: the same surface (similar average brightness round
                // about, so a fine texture still counts as itself).
                var dg = 0.0;
                for (var v = -2; v <= 2; v++) {
                    for (var u = -2; u <= 2; u++) {
                        dg += a_at(q.x + u, q.y + v) - a_at(p.x + u, p.y + v);
                    }
                }
                dg /= 25.0;
                let alike = exp(-dg * dg / 800.0);
                if (alike > 0.2 || prm.wide != 0) {
                    let wt = (alike + 1e-4) * exp(-f32(i * i + j * j) / 18.0);
                    acc += n.xy * wt;
                    ws += wt;
                }
            }
        }
    }
    if (ws > 0.0) {
        fOut[idx] = vec4<f32>(acc / ws, -1.0, f.w);
    } else {
        fOut[idx] = f;
    }
}

// ---- The frame in between ---------------------------------------------------------------

@compute @workgroup_size(8, 8)
fn proj_clear(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= prm.w || id.y >= prm.h) {
        return;
    }
    let idx = id.y * prm.w + id.x;
    atomicStore(&projA[idx], NONE);
    atomicStore(&projB[idx], NONE);
}

// Push each pixel of a frame (motion fIn) `s` of the way along its motion,
// onto the four pixels round where it lands. Where several land, the one
// the other frame can see too (the thing in front) wins.
@compute @workgroup_size(8, 8)
fn project(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= prm.w || id.y >= prm.h) {
        return;
    }
    let idx = id.y * prm.w + id.x;
    let f = fIn[idx];
    let y = vec2<f32>(id.xy) + prm.s * f.xy;
    let key = (u32(clamp(f.w, 0.0, 1.0) * 255.0) << 24u) | (idx & 0xffffffu);
    let b = vec2<i32>(floor(y));
    for (var j = 0; j < 2; j++) {
        for (var i = 0; i < 2; i++) {
            let q = b + vec2<i32>(i, j);
            if (q.x >= 0 && q.y >= 0 && q.x < i32(prm.w) && q.y < i32(prm.h)) {
                atomicMin(&projA[u32(q.y) * prm.w + u32(q.x)], key);
            }
        }
    }
}

// Each pixel of the in-between frame: where to look in A and in B, and how
// much each can be trusted there.
@compute @workgroup_size(8, 8)
fn resolve(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= prm.w || id.y >= prm.h) {
        return;
    }
    let idx = id.y * prm.w + id.x;
    let p = vec2<i32>(id.xy);
    let t = prm.t;
    // From A.
    var ft0 = vec2<f32>(0.0);
    var found_a = 0.0;
    var occ_a = 0.0;
    let ka = atomicLoad(&projA[idx]);
    if (ka != NONE) {
        ft0 = -t * fIn[ka & 0xffffffu].xy;
        found_a = 1.0;
        occ_a = f32(ka >> 24u) / 255.0;
    } else {
        // Nothing of A lands here (it's coming into view): the nearby motion, to fall back on.
        var acc = vec2<f32>(0.0);
        var n = 0.0;
        for (var j = -2; j <= 2; j++) {
            for (var i = -2; i <= 2; i++) {
                let q = vec2<u32>(ci(p.x + i, prm.w), ci(p.y + j, prm.h));
                let k = atomicLoad(&projA[q.y * prm.w + q.x]);
                if (k != NONE) {
                    acc += -t * fIn[k & 0xffffffu].xy;
                    n += 1.0;
                }
            }
        }
        ft0 = select(-t * fIn[idx].xy, acc / max(n, 1.0), n > 0.0);
    }
    // From B.
    var ft1 = vec2<f32>(0.0);
    var found_b = 0.0;
    var occ_b = 0.0;
    let kb = atomicLoad(&projB[idx]);
    if (kb != NONE) {
        ft1 = -(1.0 - t) * fOther[kb & 0xffffffu].xy;
        found_b = 1.0;
        occ_b = f32(kb >> 24u) / 255.0;
    } else {
        var acc = vec2<f32>(0.0);
        var n = 0.0;
        for (var j = -2; j <= 2; j++) {
            for (var i = -2; i <= 2; i++) {
                let q = vec2<u32>(ci(p.x + i, prm.w), ci(p.y + j, prm.h));
                let k = atomicLoad(&projB[q.y * prm.w + q.x]);
                if (k != NONE) {
                    acc += -(1.0 - t) * fOther[k & 0xffffffu].xy;
                    n += 1.0;
                }
            }
        }
        ft1 = select(-(1.0 - t) * fOther[idx].xy, acc / max(n, 1.0), n > 0.0);
    }
    tflow[idx] = vec4<f32>(ft0, ft1);
    // A can be used where something of A lands that B doesn't hide, and
    // where what B shows here isn't something A never saw (and the other way).
    vis[idx] = vec4<f32>(found_a * (1.0 - occ_b * found_b), found_b * (1.0 - occ_a * found_a), 0.0, 0.0);
}

fn tf_at(x: i32, y: i32) -> vec4<f32> {
    return tflow[ci(y, prm.h) * prm.w + ci(x, prm.w)];
}
fn vis_at(x: i32, y: i32) -> vec4<f32> {
    return vis[ci(y, prm.h) * prm.w + ci(x, prm.w)];
}

@compute @workgroup_size(8, 8)
fn synth(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= prm.ow || id.y >= prm.oh) {
        return;
    }
    let out_size = vec2<f32>(f32(prm.ow), f32(prm.oh));
    let pc = vec2<f32>(id.xy) + 0.5;
    let k = vec2<f32>(f32(prm.w), f32(prm.h)) / out_size;
    let q = pc * k - 0.5;
    let f = floor(q);
    let r = q - f;
    let x = i32(f.x);
    let y = i32(f.y);
    let tf = mix(mix(tf_at(x, y), tf_at(x + 1, y), r.x), mix(tf_at(x, y + 1), tf_at(x + 1, y + 1), r.x), r.y);
    let vs = mix(mix(vis_at(x, y), vis_at(x + 1, y), r.x), mix(vis_at(x, y + 1), vis_at(x + 1, y + 1), r.x), r.y);
    let pa = (pc + tf.xy / k) / out_size;
    let pb = (pc + tf.zw / k) / out_size;
    var ca = textureSampleLevel(texA, samp, pa, 0.0);
    var cb = textureSampleLevel(texB, samp, pb, 0.0);
    if ((prm.flags & 1u) != 0u) {
        ca = vec4<f32>(ca.rgb * ca.a, ca.a);
    }
    if ((prm.flags & 2u) != 0u) {
        cb = vec4<f32>(cb.rgb * cb.a, cb.a);
    }
    var wa = (1.0 - prm.t) * vs.x;
    var wb = prm.t * vs.y;
    if (wa + wb < 1e-3) {
        wa = 1.0 - prm.t;
        wb = prm.t;
    }
    textureStore(outTex, vec2<i32>(id.xy), (ca * wa + cb * wb) / (wa + wb));
}
