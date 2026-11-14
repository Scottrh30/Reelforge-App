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

// Hue curves (see core lut::HueCurves::apply, which does the same sums): a
// 256×5 table, a row a curve, 16 bits a value (red high, green low). p0.x =
// strength %.
fn hc_at(row: i32, x: f32) -> f32 {
    let f = clamp(x, 0.0, 1.0) * 255.0;
    let i = i32(floor(f));
    let j = min(i + 1, 255);
    let a = textureLoad(second, vec2(i, row), 0);
    let b = textureLoad(second, vec2(j, row), 0);
    let va = (round(a.r * 255.0) * 256.0 + round(a.g * 255.0)) / 65535.0;
    let vb = (round(b.r * 255.0) * 256.0 + round(b.g * 255.0)) / 65535.0;
    return mix(va, vb, f - f32(i));
}

@fragment
fn fs_hue_curves(in: VsOut) -> @location(0) vec4<f32> {
    let src_c = textureSample(src, src_sampler, to_tex(in.uv));
    let c = to_display(unpremul(src_c));
    let mx = max(max(c.r, c.g), c.b);
    let mn = min(min(c.r, c.g), c.b);
    let d = mx - mn;
    var h = 0.0;
    if (d > 1e-6) {
        if (mx == c.r) {
            let q = (c.g - c.b) / d;
            h = (q - 6.0 * floor(q / 6.0)) / 6.0;
        } else if (mx == c.g) {
            h = ((c.b - c.r) / d + 2.0) / 6.0;
        } else {
            h = ((c.r - c.g) / d + 4.0) / 6.0;
        }
    }
    let s = select(d / mx, 0.0, mx <= 1e-6);
    let y = dot(c, vec3(0.2126, 0.7152, 0.0722));
    let cb0 = (c.b - y) / 1.8556;
    let cr0 = (c.r - y) / 1.5748;
    let turn = (hc_at(0, h) - 0.5) * 6.2831853;
    let gain = hc_at(1, h) * 2.0 * hc_at(3, y) * 2.0 * hc_at(4, s) * 2.0;
    let cb = (cb0 * cos(turn) - cr0 * sin(turn)) * gain;
    let cr = (cb0 * sin(turn) + cr0 * cos(turn)) * gain;
    let lg = hc_at(2, h) * 2.0;
    let y2 = y * (1.0 + (lg - 1.0) * clamp(s * 1.5, 0.0, 1.0));
    let r = y2 + 1.5748 * cr;
    let b = y2 + 1.8556 * cb;
    let g = (y2 - 0.2126 * r - 0.0722 * b) / 0.7152;
    let out = mix(c, max(vec3(r, g, b), vec3(0.0)), fx.p0.x / 100.0);
    return vec4(to_linear(out) * src_c.a, src_c.a);
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

// Anamorphic flare, last step: the bright parts stretched sideways (the
// second texture) tinted and added as light. p0.z = brightness %, p0.y =
// length px, p1 = (colour, thickness px, show).
@fragment
fn fs_streak_add(in: VsOut) -> @location(0) vec4<f32> {
    let base = textureSample(src, src_sampler, to_tex(in.uv));
    let s = second_at(in.uv);
    let y = dot(s.rgb, vec3(0.2126, 0.7152, 0.0722));
    // Spread thin, a long streak is faint: made up for, so Brightness means the same at any length.
    let gain = 2.0 + sqrt(max(fx.p0.y, 0.0)) * 0.8;
    let light = mix(lt_rgb(fx.p1.x) * y, s.rgb, 0.2) * gain * fx.p0.z / 100.0;
    let a = clamp(max(max(light.r, light.g), light.b), 0.0, 1.0);
    if (fx.p1.z >= 0.5) {
        return vec4(light, a);
    }
    return vec4(base.rgb + light, max(base.a, a));
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
    // (Neon path can dim the layer itself, down to nothing but the tube.)
    let c = textureSample(src, src_sampler, to_tex(in.uv)) * (1.0 - clamp(fx.extra2.w, 0.0, 1.0));
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

// Object removal: the second texture is this frame's patch (sRGB colour, and
// alpha for how much of it covers the picture); extra2 = where it sits
// (left, top, width, height as fractions of the layer); p1.x = opacity %.
@fragment
fn fs_fill(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    let box = fx.extra2;
    let f_uv = (in.uv - box.xy) / max(box.zw, vec2(1e-6));
    let f = textureSampleLevel(second, second_sampler, clamp(f_uv, vec2(0.0), vec2(1.0)), 0.0);
    let on = select(0.0, 1.0, all(f_uv >= vec2(0.0)) && all(f_uv <= vec2(1.0)));
    let k = f.a * on * clamp(fx.p1.x / 100.0, 0.0, 1.0);
    // The patch is opaque picture: over the layer, keeping its coverage.
    let a = mix(c.a, 1.0, k);
    return vec4(mix(c.rgb, to_linear(f.rgb), k), a);
}

// Refine edge: a mask's edge fitted to the picture by matting in a band
// round it. Where the mask is solidly on or off within `radius` it stays;
// in between, each pixel's alpha is where its colour sits on the line from
// the colours surely outside to those surely inside, round it (so hair and
// motion blur go soft, and background the mask took in goes).

fn refine_straight(c: vec4<f32>) -> vec3<f32> {
    return select(vec3(0.0), c.rgb / max(c.a, 1e-4), c.a > 1e-4);
}

// The layer, with in its alpha also which part it's in: + 4 × (0 surely
// out, 1 in between, 2 surely in). p0.xy = radius in layer uv.
@fragment
fn fs_refine_trimap(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    let inside = select(0.0, 1.0, all(in.uv >= vec2(0.0)) && all(in.uv <= vec2(1.0)));
    let r = fx.p0.xy;
    var lo = textureSampleLevel(second, second_sampler, in.uv, 0.0).r * inside;
    var hi = lo;
    for (var k = 0; k < 16; k = k + 1) {
        let a = f32(k) * 0.392699;
        for (var s = 1; s <= 2; s = s + 1) {
            let m = textureSampleLevel(second, second_sampler, in.uv + vec2(cos(a), sin(a)) * r * f32(s) * 0.5, 0.0).r;
            lo = min(lo, m);
            hi = max(hi, m);
        }
    }
    var zone = 1.0;
    if (lo > 0.98) {
        zone = 2.0;
    } else if (hi < 0.02) {
        zone = 0.0;
    }
    return vec4(c.rgb, clamp(c.a, 0.0, 1.0) + 4.0 * zone);
}

fn refine_zone(v: vec4<f32>) -> f32 {
    return floor(v.a / 4.0 + 0.01);
}

// Straight colour where surely in (weight in alpha).
@fragment
fn fs_refine_in(in: VsOut) -> @location(0) vec4<f32> {
    let v = textureSample(src, src_sampler, to_tex(in.uv));
    let z = refine_zone(v);
    let a = v.a - 4.0 * z;
    let w = select(0.0, 1.0, z > 1.5 && a > 0.5);
    return vec4(refine_straight(vec4(v.rgb, a)) * w, w);
}

// Straight colour where surely out.
@fragment
fn fs_refine_out(in: VsOut) -> @location(0) vec4<f32> {
    let v = textureSample(src, src_sampler, to_tex(in.uv));
    let z = refine_zone(v);
    let a = v.a - 4.0 * z;
    let w = select(0.0, 1.0, z < 0.5 && a > 0.5);
    return vec4(refine_straight(vec4(v.rgb, a)) * w, w);
}

// The line from out (second) to in (src): (in − out, out · (in − out)); or
// with nothing known on one side, a flag in alpha: 1e4 all in, −1e4 all out.
@fragment
fn fs_refine_line(in: VsOut) -> @location(0) vec4<f32> {
    let f = textureSample(src, src_sampler, to_tex(in.uv));
    let b = textureSample(second, second_sampler, to_tex(in.uv));
    if (f.a < 1e-3 && b.a < 1e-3) {
        return vec4(0.0, 0.0, 0.0, 0.0);
    }
    if (b.a < 1e-3) {
        return vec4(0.0, 0.0, 0.0, 1e4);
    }
    if (f.a < 1e-3) {
        return vec4(0.0, 0.0, 0.0, -1e4);
    }
    let fc = f.rgb / f.a;
    let bc = b.rgb / b.a;
    let d = fc - bc;
    return vec4(d, dot(bc, d));
}

// The layer with its refined alpha. p0.x = softness 0–100, p0.y = shift −100–100.
@fragment
fn fs_refine_apply(in: VsOut) -> @location(0) vec4<f32> {
    let v = textureSample(src, src_sampler, to_tex(in.uv));
    let line = textureSample(second, second_sampler, to_tex(in.uv));
    let z = refine_zone(v);
    let a = clamp(v.a - 4.0 * z, 0.0, 1.0);
    let c = vec4(v.rgb, a);
    var m = 0.0;
    if (z > 1.5) {
        m = 1.0;
    } else if (z > 0.5) {
        let d2 = dot(line.rgb, line.rgb);
        if (line.a > 5e3) {
            m = 1.0;
        } else if (line.a < -5e3) {
            m = 0.0;
        } else if (d2 > 1e-4) {
            m = (dot(refine_straight(c), line.rgb) - line.a) / d2;
            // Softness: 0 crisp, 100 as soft as found.
            let k = mix(2.5, 1.0, clamp(fx.p0.x / 100.0, 0.0, 1.0));
            m = (m - 0.5) * k + 0.5 + fx.p0.y / 200.0;
        } else {
            m = 0.5;
        }
    }
    return c * clamp(m, 0.0, 1.0);
}

// Paint (see core paint.rs): the second texture holds the strokes showing
// now. extra = (-, -, layer size); extra2 = (segments, runs, strokes, -).

fn pt_u16(t: vec4<f32>) -> vec2<f32> {
    let b = round(t * 255.0);
    return vec2(b.x * 256.0 + b.y, b.z * 256.0 + b.w);
}

fn pt_point(t: vec4<f32>, size: vec2<f32>) -> vec2<f32> {
    return (pt_u16(t) / 65535.0 * 2.0 - 0.5) * size;
}

fn pt_dist(p: vec2<f32>, a: vec2<f32>, b: vec2<f32>) -> f32 {
    let ab = b - a;
    let h = clamp(dot(p - a, ab) / max(dot(ab, ab), 1e-6), 0.0, 1.0);
    return length(p - a - ab * h);
}

// One stroke's mark on the picture so far.
fn pt_apply(out: vec4<f32>, orig: vec4<f32>, k: i32, cover: f32, uv: vec2<f32>, size: vec2<f32>) -> vec4<f32> {
    if (cover <= 0.0) {
        return out;
    }
    let h0 = textureLoad(second, vec2(k * 4, 2), 0);
    let h1 = textureLoad(second, vec2(k * 4 + 1, 2), 0);
    let h2 = textureLoad(second, vec2(k * 4 + 2, 2), 0);
    let tool = u32(round(h0.r * 255.0));
    let a = cover * h0.g;
    var what = orig;
    if (tool == 0u) {
        let off = (pt_u16(h2) / 65535.0 * 2.0 - 1.0);
        what = textureSampleLevel(src, src_sampler, to_tex(uv + off), 0.0);
    } else if (tool == 1u) {
        what = vec4(to_linear(h1.rgb), 1.0);
    }
    return mix(out, what, a);
}

@fragment
fn fs_paint(in: VsOut) -> @location(0) vec4<f32> {
    let orig = textureSample(src, src_sampler, to_tex(in.uv));
    let size = fx.extra.zw;
    let p = in.uv * size;
    let runs = i32(fx.extra2.y);
    var out = orig;
    var cur = -1;
    var cover = 0.0;
    for (var r = 0; r < runs; r = r + 1) {
        let r2 = textureLoad(second, vec2(r * 3 + 2, 1), 0);
        let ids = pt_u16(r2);
        let k = i32(ids.x) / 16;
        let n = i32(ids.x) % 16;
        if (k != cur) {
            if (cur >= 0) {
                out = pt_apply(out, orig, cur, cover, in.uv, size);
            }
            cur = k;
            cover = 0.0;
        }
        let lo = pt_point(textureLoad(second, vec2(r * 3, 1), 0), size);
        let hi = pt_point(textureLoad(second, vec2(r * 3 + 1, 1), 0), size);
        if (any(p < lo) || any(p > hi)) {
            continue;
        }
        let h3 = textureLoad(second, vec2(k * 4 + 3, 2), 0);
        let radius = max(pt_u16(h3).x / 4.0, 0.5);
        let hard = textureLoad(second, vec2(k * 4, 2), 0).b;
        let first = i32(ids.y);
        for (var s = first; s < first + n; s = s + 1) {
            let a = pt_point(textureLoad(second, vec2(s * 3, 0), 0), size);
            let b = pt_point(textureLoad(second, vec2(s * 3 + 1, 0), 0), size);
            let d = pt_dist(p, a, b);
            // Solid to `hardness` of the radius, fading out to the edge.
            let inner = radius * hard;
            let c = 1.0 - smoothstep(min(inner, radius - 0.5), radius + 0.5, d);
            cover = max(cover, c);
        }
    }
    if (cur >= 0) {
        out = pt_apply(out, orig, cur, cover, in.uv, size);
    }
    return out;
}

// ---- Face beauty: the faces (the second texture: two texels each, middle
// and half size as 16-bit fractions of the layer; extra2.x how many).
// p0 = (mode, smoothing %, detail %, brighten %), p1 = (even tone %, where,
// face area %, softness %), p2.x = show faces. extra.zw = layer size.

fn fb_frac(v: f32) -> f32 {
    return v / 65535.0 * 2.0 - 0.5;
}

fn fb_skin(rgb: vec3<f32>) -> f32 {
    let g = to_display(rgb);
    let cb = 0.5 - 0.1687 * g.r - 0.3313 * g.g + 0.5 * g.b;
    let cr = 0.5 + 0.5 * g.r - 0.4187 * g.g - 0.0813 * g.b;
    let y = dot(g, vec3(0.299, 0.587, 0.114));
    let a = smoothstep(0.27, 0.31, cb) * (1.0 - smoothstep(0.49, 0.53, cb));
    let b = smoothstep(0.515, 0.545, cr) * (1.0 - smoothstep(0.68, 0.72, cr));
    return a * b * smoothstep(0.08, 0.18, y);
}

@fragment
fn fs_beauty(in: VsOut) -> @location(0) vec4<f32> {
    let c0 = textureSample(src, src_sampler, to_tex(in.uv));
    let size = fx.extra.zw;
    let p = in.uv * size;
    let n = i32(fx.extra2.x);
    let mode = i32(fx.p0.x);
    let all_skin = fx.p1.y > 0.5;
    // Which face this is in, and how far in (1 inside, fading out at the edge).
    var w = select(0.0, 1.0, all_skin);
    var face_r = size.y * 0.08;
    var ring = 0.0;
    let grow = max(fx.p1.z / 100.0, 0.1);
    let soft = clamp(fx.p1.w / 100.0, 0.0, 1.0) * 0.8 + 0.05;
    for (var i = 0; i < n; i = i + 1) {
        let a = pt_u16(textureLoad(second, vec2(i * 2, 0), 0));
        let b = pt_u16(textureLoad(second, vec2(i * 2 + 1, 0), 0));
        let c = vec2(fb_frac(a.x), fb_frac(a.y)) * size;
        let r = max(vec2(fb_frac(b.x), fb_frac(b.y)) * size, vec2(1.0));
        let d = length((p - c) / (r * grow));
        let wf = 1.0 - smoothstep(1.0 - soft, 1.0, d);
        if (!all_skin && wf > w) {
            w = wf;
            face_r = r.x;
        }
        // The face as found, outlined (Show faces).
        let e = length((p - c) / r);
        ring = max(ring, 1.0 - smoothstep(1.0, 2.5, abs(e - 1.0) * r.x));
    }
    var out = c0;
    let rgb0 = unpremul(c0);
    if (w > 0.0) {
        if (mode == 1) {
            // Blur: a disc a quarter of the face across.
            let rad = face_r * 0.25;
            var acc = vec4(0.0);
            for (var k = 0; k < 32; k = k + 1) {
                let t = f32(k) * 2.39996;
                let rr = rad * sqrt((f32(k) + 0.5) / 32.0);
                acc += textureSample(src, src_sampler, to_tex(in.uv + vec2(cos(t), sin(t)) * rr / size));
            }
            out = mix(c0, acc / 32.0, w);
        } else if (mode == 2) {
            // Pixelate: blocks a fifth of the face across.
            let blk = max(face_r / 5.0, 4.0);
            let q = (floor(p / blk) + 0.5) * blk;
            out = mix(c0, textureSample(src, src_sampler, to_tex(q / size)), w);
        } else {
            // Smooth skin: an edge-keeping (bilateral) average of the skin round it.
            let skin = fb_skin(rgb0);
            let k = w * skin;
            if (k > 0.001) {
                let rad = max(face_r * 0.12 * fx.p0.y / 50.0, 1.0);
                let g0 = sqrt(max(rgb0, vec3(0.0)));
                // How different a neighbour may be and still be smoothed in (more with more smoothing).
                let sigma = 0.04 + 0.1 * clamp(fx.p0.y / 100.0, 0.0, 1.0);
                var acc = vec3(0.0);
                var wsum = 0.0;
                var wide = vec3(0.0);
                for (var j = 0; j < 24; j = j + 1) {
                    let t = f32(j) * 2.39996;
                    let rr = rad * sqrt((f32(j) + 0.5) / 24.0);
                    let s = textureSample(src, src_sampler, to_tex(in.uv + vec2(cos(t), sin(t)) * rr / size));
                    let rgb = unpremul(s);
                    let g = sqrt(max(rgb, vec3(0.0)));
                    let dg = g - g0;
                    let ws = exp(-dot(dg, dg) / (2.0 * sigma * sigma)) * (0.2 + 0.8 * fb_skin(rgb));
                    acc += rgb * ws;
                    wsum += ws;
                    wide += rgb;
                }
                let soft_c = (acc + rgb0) / (wsum + 1.0);
                // The fine texture (pores), put back by `detail`.
                let px = 1.5 / size;
                let near = (unpremul(textureSample(src, src_sampler, to_tex(in.uv + vec2(px.x, 0.0)))) + unpremul(textureSample(src, src_sampler, to_tex(in.uv - vec2(px.x, 0.0)))) + unpremul(textureSample(src, src_sampler, to_tex(in.uv + vec2(0.0, px.y)))) + unpremul(textureSample(src, src_sampler, to_tex(in.uv - vec2(0.0, px.y))))) * 0.25;
                var res = soft_c + (rgb0 - near) * clamp(fx.p0.z / 100.0, 0.0, 1.0);
                // Even out the tone: the colour (not the brightness) towards the skin round it.
                let lum = dot(res, vec3(0.2126, 0.7152, 0.0722));
                let wide_c = wide / 24.0;
                let wl = max(dot(wide_c, vec3(0.2126, 0.7152, 0.0722)), 1e-4);
                res = mix(res, wide_c * (lum / wl), clamp(fx.p1.x / 100.0, 0.0, 1.0) * 0.8);
                // Brighten a touch.
                res *= 1.0 + 0.3 * clamp(fx.p0.w / 100.0, 0.0, 1.0);
                let amt = k * clamp(fx.p0.y / 25.0, 0.0, 1.0);
                out = vec4(mix(rgb0, max(res, vec3(0.0)), amt) * c0.a, c0.a);
            }
        }
    }
    if (fx.p2.x > 0.5 && n > 0) {
        out = mix(out, vec4(0.1, 0.9, 0.3, 1.0), ring);
    }
    return out;
}

// ---- Sky replacement, first pass: the sky cut out of the layer. The second
// texture is the sky matte (alpha, whole frame). p3.x = opacity, p3.y = view;
// p2.y = edge softness px, p2.z = edge shift px. extra.zw = layer size.

fn sky_matte(uv: vec2<f32>) -> f32 {
    let size = fx.extra.zw;
    let soft = max(fx.p2.y, 0.0);
    let shift = fx.p2.z;
    let r = soft + abs(shift);
    var m = textureSampleLevel(second, second_sampler, clamp(uv, vec2(0.0), vec2(1.0)), 0.0).a;
    if (r > 0.5) {
        var acc = m;
        for (var k = 0; k < 12; k = k + 1) {
            let t = f32(k) * 0.5236;
            for (var ring = 1; ring <= 2; ring = ring + 1) {
                let o = vec2(cos(t), sin(t)) * r * f32(ring) / 2.0 / size;
                acc += textureSampleLevel(second, second_sampler, clamp(uv + o, vec2(0.0), vec2(1.0)), 0.0).a;
            }
        }
        m = acc / 25.0;
        // A blurred edge rises over about 2r: moving it `shift` px along.
        m = clamp(m + shift / max(2.0 * r, 1.0), 0.0, 1.0);
    }
    return m;
}

@fragment
fn fs_sky_cut(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    let m = sky_matte(in.uv) * clamp(fx.p3.x / 100.0, 0.0, 1.0);
    if (i32(fx.p3.y) == 1) {
        return vec4(vec3(m), 1.0);
    }
    return c * (1.0 - m);
}

// ---- Second pass: the new sky under what's left. The second texture is the
// new sky picture (or anything, for the gradient). p0 = (source, top colour,
// horizon colour, x %), p1 = (y %, scale %, follow %, brightness %), p2.x =
// blur px, p2.w = match lighting %. extra2 = the camera's move (x, y as
// fractions, scale) and the sky's average colour.
@fragment
fn fs_sky_fill(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    let view = i32(fx.p3.y);
    if (view == 1) {
        return c;
    }
    let size = fx.extra.zw;
    let follow = fx.p1.z / 100.0;
    let mv = fx.extra2.xy * follow;
    let zoom = max(fx.p1.y / 100.0 * (1.0 + (fx.extra2.z - 1.0) * follow), 0.05);
    let picture = fx.p0.x < 0.5 && fx.extra2.w > 0.0;
    let dims = vec2<f32>(textureDimensions(second));
    let blur = max(fx.p2.x, 0.0);
    var sky = vec3(0.0);
    let taps = select(1, 13, blur > 0.5);
    for (var k = 0; k < taps; k = k + 1) {
        var o = vec2(0.0);
        if (k > 0) {
            let t = f32(k) * 2.39996;
            o = vec2(cos(t), sin(t)) * blur * sqrt(f32(k) / 12.0) / size;
        }
        let uv = (in.uv + o - 0.5 - vec2(fx.p0.w, fx.p1.x) / 100.0 - mv) / zoom + 0.5;
        if (picture) {
            // Cover the frame, keeping the picture's shape.
            let a = size.x / size.y;
            let b = dims.x / max(dims.y, 1.0);
            var q = uv;
            if (b > a) {
                q.x = (uv.x - 0.5) * a / b + 0.5;
            } else {
                q.y = (uv.y - 0.5) * b / a + 0.5;
            }
            sky += to_linear(textureSampleLevel(second, second_sampler, clamp(q, vec2(0.0), vec2(1.0)), 0.0).rgb);
        } else {
            sky += mix(lt_rgb(fx.p0.y), lt_rgb(fx.p0.z), smoothstep(0.0, 1.0, clamp(uv.y, 0.0, 1.0)));
        }
    }
    sky = sky / f32(taps) * (1.0 + fx.p1.w / 100.0);
    if (view == 2) {
        return vec4(sky, 1.0);
    }
    // The footage's light towards the new sky's colour.
    var avg = mix(lt_rgb(fx.p0.y), lt_rgb(fx.p0.z), 0.5);
    if (picture) {
        avg = lt_rgb(fx.extra2.w);
    }
    let tint = avg / max(dot(avg, vec3(0.2126, 0.7152, 0.0722)), 1e-4);
    let fg = c.rgb * mix(vec3(1.0), clamp(tint, vec3(0.5), vec3(2.0)), clamp(fx.p2.w / 100.0, 0.0, 1.0) * 0.35);
    return vec4(fg + sky * (1.0 - c.a), 1.0 - (1.0 - c.a) * 0.0);
}

// Audio spectrum (see core audiovis.rs): the second texture is one row of
// band levels, 16 bits each (red high, green low). p0 = (source, display,
// bands, start x %), p1 = (start y %, end x %, end y %, height % of the
// layer's height), p2 = (thickness %, -, -, inside colour), p3 = (outside
// colour, -, glow %, mirror), p4.x = show the layer %; extra.zw = layer size.
fn av_level(i: i32, n: i32) -> f32 {
    let t = textureLoad(second, vec2(clamp(i, 0, n - 1), 0), 0);
    return (round(t.r * 255.0) * 256.0 + round(t.g * 255.0)) / 65535.0;
}

@fragment
fn fs_audio_vis(in: VsOut) -> @location(0) vec4<f32> {
    let src_c = textureSample(src, src_sampler, to_tex(in.uv));
    let size = fx.extra.zw;
    let n = i32(max(round(fx.p0.z), 1.0));
    let display = u32(round(fx.p0.y));
    let a = vec2(fx.p0.w, fx.p1.x) / 100.0 * size;
    let b = vec2(fx.p1.y, fx.p1.z) / 100.0 * size;
    let len = max(length(b - a), 1.0);
    let dir = (b - a) / len;
    // "Up" from the line: its left as it runs (up the frame for a level line drawn left to right).
    let up = vec2(dir.y, -dir.x);
    let p = in.uv * size;
    let u = dot(p - a, dir) / len;
    var w = dot(p - a, up);
    let tall = max(fx.p1.w / 100.0 * size.y, 1.0);
    let mirror = fx.p3.w >= 0.5;
    if (mirror && display != 3u) {
        w = abs(w);
    }
    let fb = u * f32(n);
    let i = i32(floor(fb));
    let inband = fb - floor(fb);
    let thick = clamp(fx.p2.x / 100.0, 0.01, 1.0);
    let px = 1.0 / max(len / f32(n), 1e-3);
    var d = 1e9;
    // How far (px) from the shape: 0 inside.
    if (u >= 0.0 && u <= 1.0) {
        if (display == 0u) {
            // Bars: centred in their bands, from the line up to the level.
            let h = av_level(i, n) * tall;
            let half = thick * 0.5;
            let dx = max(abs(inband - 0.5) - half, 0.0) * len / f32(n);
            let dy = max(max(-w, w - h), 0.0);
            d = select(length(vec2(dx, dy)), 0.0, dx <= 0.0 && dy <= 0.0);
            if (h < 0.5) {
                d = max(d, 0.5);
            }
        } else if (display == 2u) {
            // Dots: a round dot at each band's level.
            let h = av_level(i, n) * tall;
            let r = thick * 0.5 * len / f32(n);
            let c = vec2((inband - 0.5) * len / f32(n), w - h);
            d = max(length(c) - r, 0.0);
        } else {
            // A line through the band levels (the waveform's from its middle).
            let x = clamp(fb - 0.5, 0.0, f32(n - 1));
            let j = i32(floor(x));
            let lv = mix(av_level(j, n), av_level(j + 1, n), x - floor(x));
            var h = lv * tall;
            if (display == 3u) {
                h = (lv - 0.5) * 2.0 * tall;
            }
            let lw = max(thick * 6.0, 1.0);
            d = max(abs(w - h) - lw * 0.5, 0.0);
            if (display == 3u && mirror) {
                d = min(d, max(abs(w + h) - lw * 0.5, 0.0));
            }
        }
    }
    let cover = 1.0 - smoothstep(0.0, 1.0, d);
    let glow = fx.p3.z / 100.0 * exp(-d / max(tall * 0.06, 2.0)) * 0.6;
    let k = clamp(abs(w) / tall, 0.0, 1.0);
    let col = mix(lt_rgb(fx.p2.w), lt_rgb(fx.p3.x), k);
    let alpha = clamp(cover + glow * (1.0 - cover), 0.0, 1.0);
    let base = src_c * clamp(fx.p4.x / 100.0, 0.0, 1.0);
    return vec4(col * alpha + base.rgb * (1.0 - alpha), alpha + base.a * (1.0 - alpha));
}
