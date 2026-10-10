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
    // x: layer width / height.
    extra: vec4<f32>,
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
