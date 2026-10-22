// Per-layer effect passes. Each pass reads one texture and writes a full-size
// Rgba16Float target in the layer's own pixel space (before the transform),
// premultiplied, linear light. A layer with effects is first "imported" from
// its sRGB 8-bit upload into that form, then runs one pass per effect (two
// for a blur), and is finally composited like a pre-multiplied group.

struct Fx {
    // Effect parameters, in the order the core crate defines them.
    p0: vec4<f32>,
    p1: vec4<f32>,
    // xy: size of one texel in UV units; zw: blur direction (1,0) or (0,1).
    texel_dir: vec4<f32>,
    // x: layer width / height; y: time (s); zw: layer size (frame pixels at 100%).
    extra: vec4<f32>,
    // Params 8–15.
    p2: vec4<f32>,
    p3: vec4<f32>,
    // Energy blade: hilt and tip one frame earlier (% of the layer).
    extra2: vec4<f32>,
    // Params 16–19.
    p4: vec4<f32>,
};

@group(0) @binding(0) var<uniform> fx: Fx;
@group(1) @binding(0) var src: texture_2d<f32>;
@group(1) @binding(1) var src_sampler: sampler;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_fullscreen(@builtin(vertex_index) i: u32) -> VsOut {
    // One triangle that covers the target; uv (0,0) is the top-left texel.
    let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    var out: VsOut;
    out.pos = vec4(p * 2.0 - 1.0, 0.0, 1.0);
    out.uv = vec2(p.x, 1.0 - p.y);
    return out;
}

fn unpremul(c: vec4<f32>) -> vec3<f32> {
    return select(vec3(0.0), c.rgb / c.a, c.a > 1e-5);
}

const LUMA = vec3<f32>(0.2126, 0.7152, 0.0722);

// sRGB-uploaded layer (straight alpha, sampled as linear) -> premultiplied.
@fragment
fn fs_import(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, in.uv);
    return vec4(c.rgb * c.a, c.a);
}

// Exposure (stops), contrast %, saturation %, temperature, tint.
@fragment
fn fs_color(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, in.uv);
    var rgb = unpremul(c);
    rgb = rgb * exp2(fx.p0.x);
    let temp = fx.p0.w / 100.0;
    let tint = fx.p1.x / 100.0;
    rgb = rgb * vec3(1.0 + 0.3 * temp, 1.0 - 0.3 * tint, 1.0 - 0.3 * temp) * vec3(1.0 + 0.15 * tint, 1.0, 1.0 + 0.15 * tint);
    let luma = dot(rgb, LUMA);
    rgb = max(mix(vec3(luma), rgb, fx.p0.z / 100.0), vec3(0.0));
    // Contrast pivots around 18% grey, the middle of the tonal range in linear light.
    let k = fx.p0.y / 100.0;
    rgb = 0.18 * pow(rgb / 0.18, vec3(k));
    return vec4(rgb * c.a, c.a);
}

// Separable Gaussian: p0.x = sigma in texels; run once per direction.
@fragment
fn fs_blur(in: VsOut) -> @location(0) vec4<f32> {
    let sigma = max(fx.p0.x, 0.01);
    let reach = 3.0 * sigma;
    let taps = min(ceil(reach), 64.0);
    let step = reach / max(taps, 1.0);
    let dir = fx.texel_dir.xy * fx.texel_dir.zw;
    var sum = textureSample(src, src_sampler, in.uv);
    var total = 1.0;
    for (var i = 1.0; i <= taps; i = i + 1.0) {
        let x = i * step;
        let w = exp(-0.5 * x * x / (sigma * sigma));
        sum = sum + w * (textureSample(src, src_sampler, in.uv + dir * x) + textureSample(src, src_sampler, in.uv - dir * x));
        total = total + 2.0 * w;
    }
    return sum / total;
}

fn hue_rgb(h: f32) -> vec3<f32> {
    let k = (vec3(5.0, 3.0, 1.0) + h / 60.0) % 6.0;
    return 1.0 - clamp(min(k, 4.0 - k), vec3(0.0), vec3(1.0));
}

fn chroma(g: vec3<f32>) -> vec2<f32> {
    let y = dot(g, LUMA);
    return vec2((g.b - y) / 1.8556, (g.r - y) / 1.5748);
}

// Chroma key: p0 = (key hue°, threshold %, softness %, spill %).
// Works on gamma-encoded colour, where "how green is this" matches what you see.
@fragment
fn fs_key(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, in.uv);
    let g = pow(max(unpremul(c), vec3(0.0)), vec3(1.0 / 2.2));
    let y = dot(g, LUMA);
    let k = normalize(chroma(hue_rgb(fx.p0.x)));
    let cc = chroma(g);
    let along = dot(cc, k);
    let across = abs(cc.x * k.y - cc.y * k.x);
    // How strongly the pixel leans towards the key colour (about 0.2–0.4 for a real screen).
    let strength = along - across;
    let lo = fx.p0.y / 100.0 * 0.5;
    let hi = lo + fx.p0.z / 100.0 * 0.5 + 1e-4;
    let keep = 1.0 - smoothstep(lo, hi, strength);
    // Spill suppression: take the key colour's tint out of what remains.
    let c2 = cc - k * max(strength, 0.0) * (fx.p0.w / 100.0);
    let r = y + 1.5748 * c2.y;
    let b = y + 1.8556 * c2.x;
    let gg = (y - 0.2126 * r - 0.0722 * b) / 0.7152;
    let lin = pow(max(vec3(r, gg, b), vec3(0.0)), vec3(2.2));
    let a = c.a * keep;
    return vec4(lin * a, a);
}

// Vignette: p0 = (amount %, size %, softness %).
@fragment
fn fs_vignette(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, in.uv);
    // 0 at the centre, 1 in the corners, following the layer's shape.
    let d = length((in.uv - 0.5) * 2.0) / sqrt(2.0);
    let soft = fx.p0.z / 100.0;
    let edge = fx.p0.y / 100.0;
    let v = smoothstep(edge - soft * 0.5, edge + soft * 0.5, d);
    let f = 1.0 - fx.p0.x / 100.0 * v;
    return vec4(c.rgb * f, c.a);
}

fn to_display(rgb: vec3<f32>) -> vec3<f32> {
    return pow(max(rgb, vec3(0.0)), vec3(1.0 / 2.2));
}
fn to_linear(g: vec3<f32>) -> vec3<f32> {
    return pow(max(g, vec3(0.0)), vec3(2.2));
}

// Levels, on display values 0–255: p0 = (in black, in white, gamma, out black), p1.x = out white.
@fragment
fn fs_levels(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, in.uv);
    let g = to_display(unpremul(c));
    let ib = fx.p0.x / 255.0;
    let iw = max(fx.p0.y / 255.0, ib + 1.0 / 255.0);
    var v = clamp((g - ib) / (iw - ib), vec3(0.0), vec3(1.0));
    v = pow(v, vec3(1.0 / max(fx.p0.z, 0.01)));
    v = fx.p0.w / 255.0 + v * (fx.p1.x - fx.p0.w) / 255.0;
    return vec4(to_linear(v) * c.a, c.a);
}

// Sharpen: p0 = (amount %, radius in texels).
@fragment
fn fs_sharpen(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, in.uv);
    let d = fx.texel_dir.xy * max(fx.p0.y, 0.5);
    var sum = vec4(0.0);
    for (var i = 0; i < 8; i = i + 1) {
        let a = f32(i) * 0.7853982;
        sum = sum + textureSample(src, src_sampler, in.uv + vec2(cos(a), sin(a)) * d);
    }
    let blurred = sum / 8.0;
    let out = c + (c - blurred) * (fx.p0.x / 100.0);
    let a = clamp(out.a, 0.0, 1.0);
    return vec4(clamp(out.rgb, vec3(0.0), vec3(a)), a);
}

// Glow, step 1: keep the bright parts. p0 = (threshold %, -, -, softness %).
@fragment
fn fs_bright(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, in.uv);
    let y = dot(to_display(unpremul(c)), LUMA);
    let t = fx.p0.x / 100.0;
    let s = max(fx.p0.w / 100.0, 0.001);
    return c * smoothstep(t - s * 0.5, t + s * 0.5, y);
}

// Drop shadow, step 1: the layer's shape, moved. p0 = (offset x texels, offset y texels, -, opacity %).
@fragment
fn fs_shadow(in: VsOut) -> @location(0) vec4<f32> {
    let a = textureSample(src, src_sampler, in.uv - fx.p0.xy * fx.texel_dir.xy).a;
    return vec4(0.0, 0.0, 0.0, a * fx.p0.w / 100.0);
}


// ---- Energy blade ----------------------------------------------------------
// A light-sword blade from the hilt to the tip: a hot core (a capsule that can
// taper and end rounded, pointed or flat) inside a two-part coloured glow and a
// wide faint haze, added onto the layer as light. Flicker and wobble come from
// smooth noise over time; motion blur averages the blade over its sweep since
// the previous frame.
//
// p0 = hilt x, hilt y, tip x, tip y (% of layer); p1 = length %, blade colour,
// core colour (0xRRGGBB), core width px; p2 = core softness %, taper %, tip
// shape, glow size px; p3 = glow intensity %, haze %, opacity %, flicker %;
// p4 = flicker speed Hz, wobble %, motion blur %, blend; extra2 = previous
// hilt and tip; texel_dir.z = a seed per blade.

fn rgb_of(v: f32) -> vec3<f32> {
    let c = u32(clamp(round(v), 0.0, 16777215.0));
    let g = vec3(f32((c >> 16u) & 255u), f32((c >> 8u) & 255u), f32(c & 255u)) / 255.0;
    return to_linear(g);
}

fn hash11(x: f32) -> f32 {
    return fract(sin(x * 127.1 + 311.7) * 43758.5453);
}

// Smooth 1D value noise, 0–1.
fn noise1(x: f32) -> f32 {
    let i = floor(x);
    let f = fract(x);
    let u = f * f * (3.0 - 2.0 * f);
    return mix(hash11(i), hash11(i + 1.0), u);
}

struct BladeHit {
    core: f32,
    glow: f32,
    haze: f32,
};

// The blade from `a` to `b` (layer pixels) seen from `p`.
fn blade_at(p: vec2<f32>, a: vec2<f32>, b: vec2<f32>, flick: f32) -> BladeHit {
    var out: BladeHit;
    let ab = b - a;
    let len2 = max(dot(ab, ab), 1e-4);
    let len = sqrt(len2);
    let raw = dot(p - a, ab) / len2;
    let h = clamp(raw, 0.0, 1.0);
    let d = length(p - a - ab * h);
    let shape = fx.p2.z;
    // Half the core's width along the blade: tapered towards the tip, wobbling.
    var r = fx.p1.w * 0.5 * (1.0 - fx.p2.y / 100.0 * h);
    let wob = fx.p4.y / 100.0;
    if (wob > 0.0) {
        let along = h * len / max(fx.p1.w * 3.0, 4.0);
        let n = noise1(along + fx.extra.y * fx.p4.x * 0.8 + fx.texel_dir.z * 17.0) - 0.5;
        r = r * (1.0 + wob * 0.5 * n);
    }
    r = r * (1.0 + 0.12 * flick);
    if (shape > 0.5 && shape < 1.5) {
        // Pointed: narrows to nothing over the last few widths.
        let tip_len = clamp(fx.p1.w * 3.0 / len, 0.001, 1.0);
        r = r * sqrt(clamp((1.0 - h) / tip_len, 0.0, 1.0));
    }
    r = max(r, 0.0);
    var sd = d - r;
    if (shape > 1.5 && raw > 1.0) {
        // Flat: cut square at the tip.
        sd = max(sd, (raw - 1.0) * len);
    }
    let soft = max(r * fx.p2.x / 100.0, 0.6);
    out.core = select(0.0, 1.0 - smoothstep(-soft, soft, sd), r > 0.0 || sd < 0.0);
    // Outside distance for the glow (inside the core it's full).
    let x = max(sd, 0.0);
    let g = max(fx.p2.w, 0.5);
    out.glow = 0.65 * exp(-(x * x) / (g * g * 0.08)) + 0.35 * exp(-x / (g * 0.45));
    out.haze = exp(-x / (g * 2.2));
    return out;
}

@fragment
fn fs_blade(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, in.uv);
    let size = fx.extra.zw;
    let p = in.uv * size;
    let len = fx.p1.x / 100.0;
    let a1 = fx.p0.xy / 100.0 * size;
    let b1 = mix(a1, fx.p0.zw / 100.0 * size, len);
    let a0 = fx.extra2.xy / 100.0 * size;
    let b0 = mix(a0, fx.extra2.zw / 100.0 * size, len);

    // Flicker: two octaves of noise, mostly on the glow.
    let t = fx.extra.y * fx.p4.x + fx.texel_dir.z * 13.7;
    let flick = (noise1(t) * 0.65 + noise1(t * 2.7 + 5.0) * 0.35 - 0.5) * 2.0 * fx.p3.w / 100.0;

    // Motion blur: samples over the sweep from where the blade was (trailing).
    let mb = fx.p4.z / 100.0;
    let sweep = max(length(a1 - a0), length(b1 - b0)) * mb;
    let n = i32(clamp(ceil(sweep / max(fx.p1.w * 0.35, 1.5)), 1.0, 24.0));
    var core = 0.0;
    var glow = 0.0;
    var haze = 0.0;
    for (var k = 0; k < n; k = k + 1) {
        let s = select(0.0, f32(k) / f32(n - 1), n > 1) * mb;
        let hit = blade_at(p, mix(a1, a0, s), mix(b1, b0, s), flick);
        core = core + hit.core;
        glow = glow + hit.glow;
        haze = haze + hit.haze;
    }
    let inv = 1.0 / f32(n);
    core = core * inv;
    glow = glow * inv * (1.0 + 0.6 * flick);
    haze = haze * inv * (1.0 + 0.4 * flick);

    let blade = rgb_of(fx.p1.y);
    let hot = rgb_of(fx.p1.z);
    let gi = fx.p3.x / 100.0;
    let op = fx.p3.z / 100.0;
    // The core is solid: it covers what's under it (the prop, tracking marks).
    let ca = clamp(core * op, 0.0, 1.0);
    let under = vec4(c.rgb * (1.0 - ca) + hot * ca, c.a * (1.0 - ca) + ca);
    // The glow is light in the blade's colour, added around (and over) it.
    let light = blade * (glow * gi * 1.6 + haze * fx.p3.y / 100.0 * 0.25) * (1.0 - core * 0.5) * op;
    let la = clamp(max(max(light.r, light.g), light.b), 0.0, 1.0);
    if (fx.p4.w > 0.5) {
        // Screen (the light kept within 0–1, so bright pictures don't blow out).
        let l = clamp(light, vec3(0.0), vec3(1.0));
        let rgb = under.rgb + l * (under.a - under.rgb) + l * (1.0 - under.a);
        return vec4(rgb, max(under.a, la));
    }
    return vec4(under.rgb + light, max(under.a, la));
}

// The frame so far (premultiplied already), for an adjustment layer's effects.
@fragment
fn fs_copy(in: VsOut) -> @location(0) vec4<f32> {
    return textureSample(src, src_sampler, in.uv);
}
