// Effect passes that read two textures: the layer so far (group 1) and a
// second one (group 2): its mask, a lookup table, or another scratch target.

struct Fx {
    p0: vec4<f32>,
    p1: vec4<f32>,
    texel_dir: vec4<f32>,
    extra: vec4<f32>,
    p2: vec4<f32>,
    p3: vec4<f32>,
    extra2: vec4<f32>,
    p4: vec4<f32>,
    // Layer uv = target uv × xy + zw (see effects.wgsl).
    map: vec4<f32>,
};

@group(0) @binding(0) var<uniform> fx: Fx;
@group(1) @binding(0) var src: texture_2d<f32>;
@group(1) @binding(1) var src_sampler: sampler;
@group(2) @binding(0) var second: texture_2d<f32>;
@group(2) @binding(1) var second_sampler: sampler;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_fullscreen(@builtin(vertex_index) i: u32) -> VsOut {
    let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    var out: VsOut;
    out.pos = vec4(p * 2.0 - 1.0, 0.0, 1.0);
    out.uv = vec2(p.x, 1.0 - p.y) * fx.map.xy + fx.map.zw;
    return out;
}

fn to_tex(uv: vec2<f32>) -> vec2<f32> {
    return (uv - fx.map.zw) / fx.map.xy;
}
// The second texture when it's another scratch target (glow, shadow).
fn second_at(uv: vec2<f32>) -> vec4<f32> {
    return textureSample(second, second_sampler, to_tex(uv));
}

fn unpremul(c: vec4<f32>) -> vec3<f32> {
    return select(vec3(0.0), c.rgb / c.a, c.a > 1e-5);
}
fn to_display(rgb: vec3<f32>) -> vec3<f32> {
    return pow(max(rgb, vec3(0.0)), vec3(1.0 / 2.2));
}
fn to_linear(g: vec3<f32>) -> vec3<f32> {
    return pow(max(g, vec3(0.0)), vec3(2.2));
}

// The mask: multiplies the layer by its alpha. Runs first in the chain, so
// effects see the masked layer, as in HitFilm.
@fragment
fn fs_mask(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    // The mask is in layer uv; nothing outside the layer.
    let inside = select(0.0, 1.0, all(in.uv >= vec2(0.0)) && all(in.uv <= vec2(1.0)));
    let m = textureSample(second, second_sampler, in.uv).r * inside;
    return c * m;
}

// Curves: a 256×1 table per channel, on display values. p0.x = strength %.
@fragment
fn fs_curves(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    let g = clamp(to_display(unpremul(c)), vec3(0.0), vec3(1.0));
    let at = (g * 255.0 + 0.5) / 256.0;
    let out = vec3(
        textureSample(second, second_sampler, vec2(at.r, 0.5)).r,
        textureSample(second, second_sampler, vec2(at.g, 0.5)).g,
        textureSample(second, second_sampler, vec2(at.b, 0.5)).b,
    );
    let v = mix(g, out, fx.p0.x / 100.0);
    return vec4(to_linear(v) * c.a, c.a);
}

// A 3D LUT stored as N slices of N×N side by side. p0 = (strength %, N).
fn lut_slice(rg: vec2<f32>, b: f32, n: f32) -> vec3<f32> {
    let x = (rg.x * (n - 1.0) + 0.5 + b * n) / (n * n);
    let y = (rg.y * (n - 1.0) + 0.5) / n;
    return textureSampleLevel(second, second_sampler, vec2(x, y), 0.0).rgb;
}

@fragment
fn fs_lut(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    let g = clamp(to_display(unpremul(c)), vec3(0.0), vec3(1.0));
    let n = max(fx.p0.y, 2.0);
    let b = g.b * (n - 1.0);
    let b0 = floor(b);
    let b1 = min(b0 + 1.0, n - 1.0);
    let out = mix(lut_slice(g.rg, b0, n), lut_slice(g.rg, b1, n), b - b0);
    let v = mix(g, out, fx.p0.x / 100.0);
    return vec4(to_linear(v) * c.a, c.a);
}

// Glow, last step: add the blurred bright parts back. p0.z = intensity %.
@fragment
fn fs_glow_add(in: VsOut) -> @location(0) vec4<f32> {
    let base = textureSample(src, src_sampler, to_tex(in.uv));
    let g = second_at(in.uv) * (fx.p0.z / 100.0);
    return vec4(base.rgb + g.rgb, min(base.a + g.a, 1.0));
}

// Drop shadow, last step: the shadow under the layer.
@fragment
fn fs_under(in: VsOut) -> @location(0) vec4<f32> {
    let top = textureSample(src, src_sampler, to_tex(in.uv));
    let below = second_at(in.uv);
    return top + below * (1.0 - top.a);
}
