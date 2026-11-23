// MobileSAM's image encoder on the graphics card: the kernels it's made of.
// Every buffer is f32. `p.u` holds each dispatch's sizes (floats bitcast).

struct Params {
    u: array<vec4<u32>, 4>,
}

@group(0) @binding(0) var<uniform> p: Params;
@group(0) @binding(1) var<storage, read> a: array<f32>;
@group(0) @binding(2) var<storage, read> b: array<f32>;
@group(0) @binding(3) var<storage, read_write> c: array<f32>;
@group(0) @binding(4) var<storage, read> d: array<f32>;

fn u(i: u32) -> u32 {
    return p.u[i / 4u][i % 4u];
}

// A flat index from a 2D dispatch of 256-wide groups (`u(15)` groups across).
fn flat(gid: vec3<u32>) -> u32 {
    return gid.x + gid.y * u(15) * 256u;
}

// ---- Matrix product: c (m × n) = a (m × k) · b (k × n), or b read as its
// transpose (n × k) when u(3) = 1; plus d per row (u(4) = 1) or per column
// (u(4) = 2). 64 × 64 tiles, each thread 4 × 4 of them.

var<workgroup> ta: array<array<f32, 64>, 16>;
var<workgroup> tb: array<array<f32, 64>, 16>;

@compute @workgroup_size(16, 16)
fn gemm(@builtin(workgroup_id) wg: vec3<u32>, @builtin(local_invocation_id) l: vec3<u32>) {
    let m = u(0);
    let k = u(1);
    let n = u(2);
    let bt = u(3) == 1u;
    let row0 = wg.y * 64u;
    let col0 = wg.x * 64u;
    var acc: array<array<f32, 4>, 4>;
    for (var i = 0u; i < 4u; i++) {
        for (var j = 0u; j < 4u; j++) {
            acc[i][j] = 0.0;
        }
    }
    let lid = l.y * 16u + l.x;
    for (var k0 = 0u; k0 < k; k0 += 16u) {
        // Load 64 × 16 of a and 16 × 64 of b: 1024 values each, 4 per thread.
        for (var e = 0u; e < 4u; e++) {
            let idx = lid * 4u + e;
            // a: row r (0–63), column kk (0–15).
            let r = idx / 16u;
            let kk = idx % 16u;
            let gr = row0 + r;
            let gk = k0 + kk;
            var va = 0.0;
            if (gr < m && gk < k) {
                va = a[gr * k + gk];
            }
            ta[kk][r] = va;
            // b: row kk2 (0–15), column cc (0–63).
            let kk2 = idx / 64u;
            let cc = idx % 64u;
            let gk2 = k0 + kk2;
            let gc = col0 + cc;
            var vb = 0.0;
            if (gk2 < k && gc < n) {
                if (bt) {
                    vb = b[gc * k + gk2];
                } else {
                    vb = b[gk2 * n + gc];
                }
            }
            tb[kk2][cc] = vb;
        }
        workgroupBarrier();
        for (var kk = 0u; kk < 16u; kk++) {
            var ra: array<f32, 4>;
            var rb: array<f32, 4>;
            for (var i = 0u; i < 4u; i++) {
                ra[i] = ta[kk][l.y * 4u + i];
                rb[i] = tb[kk][l.x * 4u + i];
            }
            for (var i = 0u; i < 4u; i++) {
                for (var j = 0u; j < 4u; j++) {
                    acc[i][j] += ra[i] * rb[j];
                }
            }
        }
        workgroupBarrier();
    }
    let bias = u(4);
    for (var i = 0u; i < 4u; i++) {
        let r = row0 + l.y * 4u + i;
        if (r >= m) {
            continue;
        }
        for (var j = 0u; j < 4u; j++) {
            let col = col0 + l.x * 4u + j;
            if (col >= n) {
                continue;
            }
            var v = acc[i][j];
            if (bias == 1u) {
                v += d[r];
            } else if (bias == 2u) {
                v += d[col];
            }
            c[r * n + col] = v;
        }
    }
}

// ---- Convolution columns: a (C × H × W) → c ((C·kh·kw) × (Ho·Wo)).
// u: 0 C, 1 H, 2 W, 3 kernel, 4 stride, 5 pad, 6 Ho, 7 Wo, 8 the first output row
// (a band of rows from there: Ho of them).
@compute @workgroup_size(256)
fn im2col(@builtin(global_invocation_id) gid: vec3<u32>) {
    let n = u(6) * u(7);
    let col = gid.x;
    let row = gid.y;
    if (col >= n) {
        return;
    }
    let kk = u(3);
    let ci = row / (kk * kk);
    let ki = (row / kk) % kk;
    let kj = row % kk;
    let yo = col / u(7) + u(8);
    let xo = col % u(7);
    let yi = i32(yo * u(4) + ki) - i32(u(5));
    let xi = i32(xo * u(4) + kj) - i32(u(5));
    var v = 0.0;
    if (yi >= 0 && xi >= 0 && yi < i32(u(1)) && xi < i32(u(2))) {
        v = a[(ci * u(1) + u32(yi)) * u(2) + u32(xi)];
    }
    c[row * n + col] = v;
}

// ---- Depthwise 3 × 3 convolution, padding 1: a (C × H × W), b weights (C × 9),
// d bias (C) → c (C × Ho × Wo). u: 0 C, 1 H, 2 W, 3 stride, 4 Ho, 5 Wo.
@compute @workgroup_size(256)
fn dwconv(@builtin(global_invocation_id) gid: vec3<u32>) {
    let n = u(4) * u(5);
    let px = gid.x;
    let ch = gid.y;
    if (px >= n) {
        return;
    }
    let yo = px / u(5);
    let xo = px % u(5);
    let h = i32(u(1));
    let w = i32(u(2));
    var s = d[ch];
    for (var i = 0; i < 3; i++) {
        let yi = i32(yo * u(3)) + i - 1;
        if (yi < 0 || yi >= h) {
            continue;
        }
        for (var j = 0; j < 3; j++) {
            let xi = i32(xo * u(3)) + j - 1;
            if (xi < 0 || xi >= w) {
                continue;
            }
            s += b[ch * 9u + u32(i * 3 + j)] * a[(ch * u(1) + u32(yi)) * u(2) + u32(xi)];
        }
    }
    c[ch * n + px] = s;
}

fn erf(x: f32) -> f32 {
    let z = abs(x);
    let t = 1.0 / (1.0 + 0.3275911 * z);
    let y = 1.0 - (((((1.061405429 * t - 1.453152027) * t) + 1.421413741) * t - 0.284496736) * t + 0.254829592) * t * exp(-z * z);
    return select(y, -y, x < 0.0);
}

// ---- GELU in place. u: 0 count.
@compute @workgroup_size(256)
fn gelu(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = flat(gid);
    if (i >= u(0)) {
        return;
    }
    let x = c[i];
    c[i] = 0.5 * x * (1.0 + erf(x * 0.70710678));
}

// ---- c += a. u: 0 count.
@compute @workgroup_size(256)
fn add(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = flat(gid);
    if (i >= u(0)) {
        return;
    }
    c[i] += a[i];
}

// ---- c = a (a copy). u: 0 count.
@compute @workgroup_size(256)
fn copy(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = flat(gid);
    if (i >= u(0)) {
        return;
    }
    c[i] = a[i];
}

// ---- LayerNorm of each row (rows × C) in place, scale b and shift d.
// u: 0 rows, 1 C, 2 eps (f32 bits).
@compute @workgroup_size(256)
fn layernorm(@builtin(global_invocation_id) gid: vec3<u32>) {
    let r = flat(gid);
    if (r >= u(0)) {
        return;
    }
    let n = u(1);
    let o = r * n;
    var m = 0.0;
    for (var i = 0u; i < n; i++) {
        m += c[o + i];
    }
    m /= f32(n);
    var v = 0.0;
    for (var i = 0u; i < n; i++) {
        let e = c[o + i] - m;
        v += e * e;
    }
    let k = inverseSqrt(v / f32(n) + bitcast<f32>(u(2)));
    for (var i = 0u; i < n; i++) {
        c[o + i] = (c[o + i] - m) * k * b[i] + d[i];
    }
}

// ---- LayerNorm across the channels of a C × HW map, in place.
// u: 0 HW, 1 C, 2 eps.
@compute @workgroup_size(256)
fn layernorm2d(@builtin(global_invocation_id) gid: vec3<u32>) {
    let px = flat(gid);
    let hw = u(0);
    if (px >= hw) {
        return;
    }
    let n = u(1);
    var m = 0.0;
    for (var i = 0u; i < n; i++) {
        m += c[i * hw + px];
    }
    m /= f32(n);
    var v = 0.0;
    for (var i = 0u; i < n; i++) {
        let e = c[i * hw + px] - m;
        v += e * e;
    }
    let k = inverseSqrt(v / f32(n) + bitcast<f32>(u(2)));
    for (var i = 0u; i < n; i++) {
        c[i * hw + px] = (c[i * hw + px] - m) * k * b[i] + d[i];
    }
}

// ---- c (cols × rows) = transpose of a (rows × cols). u: 0 rows, 1 cols.
@compute @workgroup_size(256)
fn transpose(@builtin(global_invocation_id) gid: vec3<u32>) {
    let col = gid.x;
    let row = gid.y;
    if (col >= u(1)) {
        return;
    }
    c[col * u(0) + row] = a[row * u(1) + col];
}

// ---- Tokens (H·W × C) into windows (windows · ws² × C), zeros past the edge.
// u: 0 H, 1 W, 2 C, 3 ws, 4 windows across.
@compute @workgroup_size(256)
fn windows(@builtin(global_invocation_id) gid: vec3<u32>) {
    let ch = gid.x;
    let t = gid.y + gid.z * 65535u;
    let cc = u(2);
    let ws = u(3);
    let n = ws * ws;
    if (ch >= cc) {
        return;
    }
    let win = t / n;
    let i = (t % n) / ws;
    let j = t % ws;
    let y = (win / u(4)) * ws + i;
    let x = (win % u(4)) * ws + j;
    var v = 0.0;
    if (y < u(0) && x < u(1)) {
        v = a[(y * u(1) + x) * cc + ch];
    }
    c[t * cc + ch] = v;
}

// ---- Windows back onto the tokens, added to them. u: as `windows`.
@compute @workgroup_size(256)
fn unwindow(@builtin(global_invocation_id) gid: vec3<u32>) {
    let ch = gid.x;
    let px = gid.y + gid.z * 65535u;
    let cc = u(2);
    let ws = u(3);
    if (ch >= cc || px >= u(0) * u(1)) {
        return;
    }
    let y = px / u(1);
    let x = px % u(1);
    let win = (y / ws) * u(4) + x / ws;
    let t = win * ws * ws + (y % ws) * ws + (x % ws);
    c[px * cc + ch] += a[t * cc + ch];
}

// ---- Window attention: a the q, k, v of every token (per head: kd of each
// in turn), b the position biases (heads × n × n), c out (tokens × C).
// One thread per window, head and query. u: 0 windows, 1 n, 2 heads, 3 kd,
// 4 scale (f32 bits).
@compute @workgroup_size(64)
fn attention(@builtin(global_invocation_id) gid: vec3<u32>) {
    let n = u(1);
    let heads = u(2);
    let kd = u(3);
    let q = gid.x;
    let hw = gid.y;
    if (q >= n) {
        return;
    }
    let win = hw / heads;
    let h = hw % heads;
    let cc = heads * kd;
    let stride = 3u * cc;
    let base = win * n * stride + h * 3u * kd;
    let scale = bitcast<f32>(u(4));
    var acc: array<f32, 32>;
    for (var e = 0u; e < kd; e++) {
        acc[e] = 0.0;
    }
    var mx = -1e30;
    var sum = 0.0;
    let qo = base + q * stride;
    for (var j = 0u; j < n; j++) {
        let ko = base + j * stride + kd;
        var s = 0.0;
        for (var e = 0u; e < kd; e++) {
            s += a[qo + e] * a[ko + e];
        }
        s = s * scale + b[(h * n + q) * n + j];
        // Online softmax: rescale what's gathered when the maximum rises.
        let nm = max(mx, s);
        let f = exp(mx - nm);
        let wj = exp(s - nm);
        sum = sum * f + wj;
        let vo = base + j * stride + 2u * kd;
        for (var e = 0u; e < kd; e++) {
            acc[e] = acc[e] * f + wj * a[vo + e];
        }
        mx = nm;
    }
    let oo = (win * n + q) * cc + h * kd;
    for (var e = 0u; e < kd; e++) {
        c[oo + e] = acc[e] / sum;
    }
}

// ---- PReLU in place on a C × HW map: b the slope of each channel. u: 0 count, 1 HW.
@compute @workgroup_size(256)
fn prelu(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = flat(gid);
    if (i >= u(0)) {
        return;
    }
    let v = c[i];
    c[i] = select(v, b[i / u(1)] * v, v < 0.0);
}

// ---- The upscaler's last step: a (3·16 × H × W) rearranged 4× larger, plus
// b (the picture, 3 × H × W) blown up, clamped, into c (3 × 4H × 4W).
// u: 0 H, 1 W.
@compute @workgroup_size(256)
fn shuffle(@builtin(global_invocation_id) gid: vec3<u32>) {
    let h = u(0);
    let w = u(1);
    let ow = w * 4u;
    let x = gid.x;
    let row = gid.y;
    if (x >= ow) {
        return;
    }
    let ch = row / (h * 4u);
    let y = row % (h * 4u);
    let r = y / 4u;
    let cc = x / 4u;
    let k = ch * 16u + (y % 4u) * 4u + (x % 4u);
    let v = a[(k * h + r) * w + cc] + b[(ch * h + r) * w + cc];
    c[row * ow + x] = clamp(v, 0.0, 1.0);
}

// ======== Depth Anything V2 (depth.rs) on the graphics card ========

// ---- c = max(c, 0) in place. u: 0 count.
@compute @workgroup_size(256)
fn relu(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = flat(gid);
    if (i >= u(0)) {
        return;
    }
    c[i] = max(c[i], 0.0);
}

// ---- c += a × b (b per column, the layer scale). u: 0 count, 1 columns.
@compute @workgroup_size(256)
fn scale_add(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = flat(gid);
    if (i >= u(0)) {
        return;
    }
    c[i] += a[i] * b[i % u(1)];
}

// ---- The tokens: the class token then each patch (a: dim × patches), plus the
// position embedding d. u: 0 tokens, 1 dim, 2 patches. b: the class token.
@compute @workgroup_size(256)
fn tokens(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = flat(gid);
    let dim = u(1);
    if (i >= u(0) * dim) {
        return;
    }
    let t = i / dim;
    let e = i % dim;
    var v = b[e];
    if (t > 0u) {
        v = a[e * u(2) + t - 1u];
    }
    c[i] = v + d[i];
}

// ---- The patch tokens (after the class token) as a dim × patches map.
// u: 0 dim, 1 patches.
@compute @workgroup_size(256)
fn unpatch(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = flat(gid);
    let pp = u(1);
    if (i >= u(0) * pp) {
        return;
    }
    let e = i / pp;
    let q = i % pp;
    c[i] = a[(q + 1u) * u(0) + e];
}

// ---- Global self-attention: qkv (n × 3·dim, the heads' q, then k, then v) →
// c (n × dim). One thread a query and head. u: 0 n, 1 heads, 2 head size (≤ 64),
// 3 scale (f32 bits).
@compute @workgroup_size(64)
fn vit_attention(@builtin(global_invocation_id) gid: vec3<u32>) {
    let n = u(0);
    let heads = u(1);
    let kd = u(2);
    let q = gid.x;
    let h = gid.y;
    if (q >= n || h >= heads) {
        return;
    }
    let dim = heads * kd;
    let stride = 3u * dim;
    let scale = bitcast<f32>(u(3));
    var acc: array<f32, 64>;
    for (var e = 0u; e < kd; e++) {
        acc[e] = 0.0;
    }
    var mx = -1e30;
    var sum = 0.0;
    let qo = q * stride + h * kd;
    for (var j = 0u; j < n; j++) {
        let ko = j * stride + dim + h * kd;
        var s = 0.0;
        for (var e = 0u; e < kd; e++) {
            s += a[qo + e] * a[ko + e];
        }
        s *= scale;
        let nm = max(mx, s);
        let f = exp(mx - nm);
        let wj = exp(s - nm);
        sum = sum * f + wj;
        let vo = j * stride + 2u * dim + h * kd;
        for (var e = 0u; e < kd; e++) {
            acc[e] = acc[e] * f + wj * a[vo + e];
        }
        mx = nm;
    }
    for (var e = 0u; e < kd; e++) {
        c[q * dim + h * kd + e] = acc[e] / sum;
    }
}

// ---- Bilinear resize of a (C × H × W) to c (C × Ho × Wo), corners lined up.
// u: 0 C, 1 H, 2 W, 3 Ho, 4 Wo.
@compute @workgroup_size(256)
fn resize(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = flat(gid);
    let ho = u(3);
    let wo = u(4);
    if (i >= u(0) * ho * wo) {
        return;
    }
    let h = u(1);
    let w = u(2);
    let ch = i / (ho * wo);
    let yo = (i / wo) % ho;
    let xo = i % wo;
    var sy = 0.0;
    if (ho > 1u) {
        sy = f32(yo) * f32(h - 1u) / f32(ho - 1u);
    }
    var sx = 0.0;
    if (wo > 1u) {
        sx = f32(xo) * f32(w - 1u) / f32(wo - 1u);
    }
    let y0 = min(u32(floor(sy)), h - 1u);
    let x0 = min(u32(floor(sx)), w - 1u);
    let y1 = min(y0 + 1u, h - 1u);
    let x1 = min(x0 + 1u, w - 1u);
    let fy = sy - f32(y0);
    let fx = sx - f32(x0);
    let o = ch * h * w;
    let top = a[o + y0 * w + x0] * (1.0 - fx) + a[o + y0 * w + x1] * fx;
    let bottom = a[o + y1 * w + x0] * (1.0 - fx) + a[o + y1 * w + x1] * fx;
    c[i] = top * (1.0 - fy) + bottom * fy;
}

// ---- A transposed convolution whose kernel is its stride, from its blocks
// (a: (Cout·k·k) × (H·W)) to c (Cout × H·k × W·k), plus the bias d.
// u: 0 Cout, 1 k, 2 H, 3 W.
@compute @workgroup_size(256)
fn deblock(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = flat(gid);
    let k = u(1);
    let h = u(2);
    let w = u(3);
    let ho = h * k;
    let wo = w * k;
    if (i >= u(0) * ho * wo) {
        return;
    }
    let o = i / (ho * wo);
    let yo = (i / wo) % ho;
    let xo = i % wo;
    let blk = (o * k + yo % k) * k + xo % k;
    c[i] = a[blk * h * w + (yo / k) * w + xo / k] + d[o];
}

// ---- A band's output (a: O × n) into its place in the whole (c: O × total, from
// offset on). u: 0 O, 1 n, 2 total, 3 offset.
@compute @workgroup_size(256)
fn put_rows(@builtin(global_invocation_id) gid: vec3<u32>) {
    let i = flat(gid);
    let n = u(1);
    if (i >= u(0) * n) {
        return;
    }
    c[(i / n) * u(2) + u(3) + i % n] = a[i];
}
