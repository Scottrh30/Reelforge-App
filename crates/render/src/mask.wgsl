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

// Refine matte, last step: the refined matte (straight colour) with its
// see-through edge's colour swapped towards the colour from within (the second
// texture; alpha 0 where there was none). p0 = (decontaminate %, view).
@fragment
fn fs_decon_mix(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    let within = second_at(in.uv);
    if (fx.p0.y > 0.5) {
        return vec4(vec3(c.a), 1.0);
    }
    let f = fx.p0.x / 100.0 * clamp((1.0 - c.a) * 2.0, 0.0, 1.0) * within.a;
    let rgb = mix(c.rgb, within.rgb, f);
    return vec4(rgb * c.a, c.a);
}

// Drop shadow, last step: the shadow under the layer.
@fragment
fn fs_under(in: VsOut) -> @location(0) vec4<f32> {
    let top = textureSample(src, src_sampler, to_tex(in.uv));
    let below = second_at(in.uv);
    return top + below * (1.0 - top.a);
}

// Difference key: what's close to the clean plate (the second texture, 8-bit
// sRGB, sampled by layer uv) becomes transparent. p0 = (tolerance %, softness
// %), p1 = (clip black %, clip white %, -, -), p2.x = view (1 the matte),
// p2.y = 1 for straight colour with the matte in alpha (see effects.wgsl's fs_key).
@fragment
fn fs_diff_key(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    let lin = unpremul(c);
    let plate = textureSample(second, second_sampler, clamp(in.uv, vec2(0.0), vec2(1.0))).rgb;
    let d = abs(to_display(lin) - plate);
    // The largest channel difference, so a change in any one colour counts.
    let diff = max(d.r, max(d.g, d.b));
    let t = fx.p0.x / 100.0;
    let keep_in = smoothstep(t, t + fx.p0.y / 100.0 + 1e-4, diff);
    let cb = fx.p1.x / 100.0;
    let cw = max(fx.p1.y / 100.0, cb + 0.01);
    let a = c.a * clamp((keep_in - cb) / (cw - cb), 0.0, 1.0);
    if (fx.p2.y > 0.5) {
        return vec4(lin, a);
    }
    if (fx.p2.x > 0.5) {
        return vec4(vec3(a), 1.0);
    }
    return vec4(lin * a, a);
}

// ---- Neon edges (shared by fs_neon_edge and fs_neon_mix) ----
// p0 = (edges from: 0 shape / 1 picture, colour, tube width px, glow radius px),
// p1 = (glow intensity %, hot core %, edge threshold %, original layer %),
// p2 = (flicker %, flicker speed Hz); extra = (-, time, layer size).

fn neon_rgb(v: f32) -> vec3<f32> {
    let c = u32(clamp(round(v), 0.0, 16777215.0));
    let g = vec3(f32((c >> 16u) & 255u), f32((c >> 8u) & 255u), f32(c & 255u)) / 255.0;
    return pow(g, vec3(2.2));
}

fn neon_hash(a: f32, b: f32) -> f32 {
    return fract(sin(a * 12.9898 + b * 78.233) * 43758.5453);
}

// How lit the tubes are now: 1, dipping and stuttering as it flickers.
fn neon_flicker() -> f32 {
    let amount = clamp(fx.p2.x / 100.0, 0.0, 1.0);
    if (amount <= 0.0) {
        return 1.0;
    }
    let tick = floor(fx.extra.y * max(fx.p2.y, 0.01));
    let r = neon_hash(tick, fx.texel_dir.z);
    // Mostly on; now and then a dip, sometimes nearly out.
    let dip = select(0.0, select(0.5, 0.95, r > 0.93), r > 0.65);
    return 1.0 - amount * dip;
}

// The edge strength at layer uv, 0–1: how much the shape's transparency (or
// the picture's brightness) changes within half a tube width.
fn neon_edge(uv: vec2<f32>) -> f32 {
    let r = max(fx.p0.z * 0.5, 0.5) / max(fx.extra.zw, vec2(1.0));
    let picture = fx.p0.x > 0.5;
    var lo = 1e9;
    var hi = -1e9;
    for (var k = 0; k < 12; k = k + 1) {
        let a = f32(k) * 0.5235988;
        let d = vec2(cos(a), sin(a));
        for (var s = 1; s <= 2; s = s + 1) {
            let c = textureSampleLevel(src, src_sampler, to_tex(uv + d * r * f32(s) * 0.5), 0.0);
            var v = c.a;
            if (picture) {
                v = dot(pow(max(select(vec3(0.0), c.rgb / c.a, c.a > 1e-5), vec3(0.0)), vec3(1.0 / 2.2)), vec3(0.2126, 0.7152, 0.0722)) * c.a;
            }
            lo = min(lo, v);
            hi = max(hi, v);
        }
    }
    let c0 = textureSampleLevel(src, src_sampler, to_tex(uv), 0.0);
    var v0 = c0.a;
    if (picture) {
        v0 = dot(pow(max(select(vec3(0.0), c0.rgb / c0.a, c0.a > 1e-5), vec3(0.0)), vec3(1.0 / 2.2)), vec3(0.2126, 0.7152, 0.0722)) * c0.a;
    }
    lo = min(lo, v0);
    hi = max(hi, v0);
    let t = fx.p1.z / 100.0;
    if (picture) {
        return smoothstep(t, t + 0.12, hi - lo);
    }
    return smoothstep(t * 0.5, t * 0.5 + 0.35, hi - lo);
}

// The tube at uv: premultiplied light, its middle white-hot.
fn neon_tube(uv: vec2<f32>) -> vec4<f32> {
    let e = neon_edge(uv) * neon_flicker();
    let col = neon_rgb(fx.p0.y);
    let hot = mix(col, vec3(1.0), clamp(fx.p1.y / 100.0, 0.0, 1.0) * e * e);
    return vec4(hot * e, e);
}

// Neon edges, last step: the layer (as much as asked), the tubes and their
// glow (the second texture), added as light.
@fragment
fn fs_neon_mix(in: VsOut) -> @location(0) vec4<f32> {
    let base = textureSample(src, src_sampler, to_tex(in.uv)) * clamp(fx.p1.w / 100.0, 0.0, 1.0);
    let tube = neon_tube(in.uv);
    let glow = second_at(in.uv) * (fx.p1.x / 100.0);
    let rgb = base.rgb + tube.rgb + glow.rgb;
    let a = clamp(base.a + tube.a + glow.a * (1.0 - base.a), 0.0, 1.0);
    return vec4(rgb, a);
}

// Fluid: the simulation's picture (second, straight sRGB colour and coverage,
// stretched over the layer) poured over the layer. p3.y = opacity %.
@fragment
fn fs_fluid(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    let inside = select(0.0, 1.0, all(in.uv >= vec2(0.0)) && all(in.uv <= vec2(1.0)));
    let d = textureSample(second, second_sampler, in.uv);
    let g = d.rgb;
    let lin = select(pow((g + 0.055) / 1.055, vec3(2.4)), g / 12.92, g <= vec3(0.04045));
    let a = clamp(d.a * fx.p3.y / 100.0, 0.0, 1.0) * inside;
    return vec4(lin * a, a) + c * (1.0 - a);
}

// Lightning (see core lightning.rs): the second texture is the table of
// segments (row 0, three texels each) and of boxes round each run of 8
// (row 1). p2.zw = core width px, glow size px; p3 = (glow intensity %,
// core colour, glow colour, -); p4 = (-, -, light the layer %, opacity %);
// extra2 = (flash 0–1, segments, runs, -); extra.zw = layer size.

fn lt_rgb(v: f32) -> vec3<f32> {
    let c = u32(clamp(round(v), 0.0, 16777215.0));
    return to_linear(vec3(f32((c >> 16u) & 255u), f32((c >> 8u) & 255u), f32(c & 255u)) / 255.0);
}

// A texel's two 16-bit fractions → layer pixels.
fn lt_point(t: vec4<f32>, size: vec2<f32>) -> vec2<f32> {
    let b = round(t * 255.0);
    let f = vec2(b.x * 256.0 + b.y, b.z * 256.0 + b.w) / 65535.0;
    return (f * 2.0 - 0.5) * size;
}

fn lt_dist(p: vec2<f32>, a: vec2<f32>, b: vec2<f32>) -> f32 {
    let ab = b - a;
    let h = clamp(dot(p - a, ab) / max(dot(ab, ab), 1e-6), 0.0, 1.0);
    return length(p - a - ab * h);
}

@fragment
fn fs_lightning(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    let size = fx.extra.zw;
    let p = in.uv * size;
    let core_w = max(fx.p2.z, 0.0);
    let glow_w = max(fx.p2.w, 0.01);
    let flash = fx.extra2.x;
    let segs = i32(fx.extra2.y);
    let runs = i32(fx.extra2.z);
    // Past this, a run adds nothing worth seeing.
    let reach = core_w + glow_w * 9.0 + 2.0;
    var core = 0.0;
    var glow = 0.0;
    for (var r = 0; r < runs; r = r + 1) {
        let lo = lt_point(textureLoad(second, vec2(r * 2, 1), 0), size) - reach;
        let hi = lt_point(textureLoad(second, vec2(r * 2 + 1, 1), 0), size) + reach;
        if (any(p < lo) || any(p > hi)) {
            continue;
        }
        let last = min(r * 8 + 8, segs);
        for (var s = r * 8; s < last; s = s + 1) {
            let a = lt_point(textureLoad(second, vec2(s * 3, 0), 0), size);
            let b = lt_point(textureLoad(second, vec2(s * 3 + 1, 0), 0), size);
            let wb = textureLoad(second, vec2(s * 3 + 2, 0), 0);
            let d = lt_dist(p, a, b);
            let w = core_w * wb.x * 0.5;
            core = max(core, wb.y * (1.0 - smoothstep(w, w + 1.0, d)));
            // A bright tight glow and a faint wide one.
            // (Faded out before the reach, so skipped runs leave no edge.)
            let fade = 1.0 - smoothstep(reach * 0.5, reach - 1.0, d);
            glow = max(glow, wb.y * fade * (exp(-d / glow_w) + 0.25 * exp(-d / (glow_w * 3.0))));
        }
    }
    let light = (lt_rgb(fx.p3.y) * core + lt_rgb(fx.p3.z) * glow * fx.p3.x / 100.0) * flash;
    // The scene lit by the flash, then the bolts added on top.
    let lit = c.rgb * (1.0 + lt_rgb(fx.p3.z) * fx.p4.z / 100.0 * flash * 2.0);
    let rgb = lit + light;
    let a = max(c.a, clamp(max(max(light.r, light.g), light.b), 0.0, 1.0));
    return mix(c, vec4(rgb, a), clamp(fx.p4.w / 100.0, 0.0, 1.0));
}
