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
    // Where the layer sits in the target (the target can be bigger, so glows
    // and shadows have room): layer uv = target uv × xy + zw.
    map: vec4<f32>,
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
    // Shaders work in the layer's uv (0–1 across the layer; beyond, in the margin).
    out.uv = vec2(p.x, 1.0 - p.y) * fx.map.xy + fx.map.zw;
    return out;
}

// A layer uv → where it is in the texture being read (a scratch target).
fn to_tex(uv: vec2<f32>) -> vec2<f32> {
    return (uv - fx.map.zw) / fx.map.xy;
}

// 1 inside the layer, 0 in the margin around it.
fn inside(uv: vec2<f32>) -> f32 {
    return select(0.0, 1.0, all(uv >= vec2(0.0)) && all(uv <= vec2(1.0)));
}

fn unpremul(c: vec4<f32>) -> vec3<f32> {
    return select(vec3(0.0), c.rgb / c.a, c.a > 1e-5);
}

const LUMA = vec3<f32>(0.2126, 0.7152, 0.0722);

// sRGB-uploaded layer (straight alpha, sampled as linear) -> premultiplied.
@fragment
fn fs_import(in: VsOut) -> @location(0) vec4<f32> {
    // The layer's own texture (layer uv), nothing in the margin.
    let c = textureSample(src, src_sampler, in.uv) * inside(in.uv);
    return vec4(c.rgb * c.a, c.a);
}

// A float layer (premultiplied already) brought in the same way.
@fragment
fn fs_import_float(in: VsOut) -> @location(0) vec4<f32> {
    return textureSample(src, src_sampler, in.uv) * inside(in.uv);
}

// Exposure (stops), contrast %, saturation %, temperature, tint.
@fragment
fn fs_color(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
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
    var sum = textureSample(src, src_sampler, to_tex(in.uv));
    var total = 1.0;
    for (var i = 1.0; i <= taps; i = i + 1.0) {
        let x = i * step;
        let w = exp(-0.5 * x * x / (sigma * sigma));
        sum = sum + w * (textureSample(src, src_sampler, to_tex(in.uv + dir * x)) + textureSample(src, src_sampler, to_tex(in.uv - dir * x)));
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

// Chroma key: p0 = (key hue°, threshold %, softness %, spill %), p1 = (clip
// black %, clip white %, choke, edge softness), p2.x = view (0 result, 1 the
// matte in black and white), p2.y = 1 to output straight (not premultiplied)
// colour with the matte in alpha, for the choke and softness passes below.
// Works on gamma-encoded colour, where "how green is this" matches what you see.
@fragment
fn fs_key(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
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
    var keep = 1.0 - smoothstep(lo, hi, strength);
    // Clip the matte: below black is gone, above white is solid.
    let cb = fx.p1.x / 100.0;
    let cw = max(fx.p1.y / 100.0, cb + 0.01);
    keep = clamp((keep - cb) / (cw - cb), 0.0, 1.0);
    // Spill suppression: take the key colour's tint out of what remains.
    let c2 = cc - k * max(strength, 0.0) * (fx.p0.w / 100.0);
    let r = y + 1.5748 * c2.y;
    let b = y + 1.8556 * c2.x;
    let gg = (y - 0.2126 * r - 0.0722 * b) / 0.7152;
    let lin = pow(max(vec3(r, gg, b), vec3(0.0)), vec3(2.2));
    let a = c.a * keep;
    if (fx.p2.y > 0.5) {
        return vec4(lin, a);
    }
    if (fx.p2.x > 0.5) {
        return key_matte(a);
    }
    return vec4(lin * a, a);
}

// The matte view: kept is white, keyed out black, over the whole layer.
fn key_matte(a: f32) -> vec4<f32> {
    return vec4(vec3(a), 1.0);
}

// What every keyer does with its matte `keep` (0–1) and the colour `lin`
// (linear, straight): clip it (p1.xy), then output as fs_key does.
fn key_finish(lin: vec3<f32>, src_a: f32, keep_in: f32) -> vec4<f32> {
    let cb = fx.p1.x / 100.0;
    let cw = max(fx.p1.y / 100.0, cb + 0.01);
    let a = src_a * clamp((keep_in - cb) / (cw - cb), 0.0, 1.0);
    if (fx.p2.y > 0.5) {
        return vec4(lin, a);
    }
    if (fx.p2.x > 0.5) {
        return key_matte(a);
    }
    return vec4(lin * a, a);
}

// Luma key: p0 = (key out: 0 dark, 1 light; threshold %; softness %), then
// the matte settings as fs_key. Works on the brightness you see.
@fragment
fn fs_luma_key(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    let lin = unpremul(c);
    let y = dot(pow(max(lin, vec3(0.0)), vec3(1.0 / 2.2)), LUMA);
    let t = fx.p0.y / 100.0;
    let s = fx.p0.z / 100.0 + 1e-4;
    var keep = smoothstep(t, t + s, y);
    if (fx.p0.x > 0.5) {
        keep = 1.0 - smoothstep(1.0 - t - s, 1.0 - t, y);
    }
    return key_finish(lin, c.a, keep);
}

// Choke, one direction (straight colour, matte in alpha): p0.x = reach in
// texels, p0.y = 1 to shrink the matte (the smallest alpha around), −1 to
// grow it (the largest, with that pixel's colour).
@fragment
fn fs_key_choke(in: VsOut) -> @location(0) vec4<f32> {
    let dir = fx.texel_dir.xy * fx.texel_dir.zw;
    var best = textureSample(src, src_sampler, to_tex(in.uv));
    let reach = min(fx.p0.x, 64.0);
    let shrink = fx.p0.y > 0.0;
    let whole = ceil(reach);
    for (var i = 1.0; i <= whole; i = i + 1.0) {
        // The last step counts only as far as the reach goes past the one before.
        let w = clamp(reach - (i - 1.0), 0.0, 1.0);
        for (var s = -1.0; s <= 1.0; s = s + 2.0) {
            let t = textureSample(src, src_sampler, to_tex(in.uv + dir * i * s));
            if (shrink) {
                best.a = mix(best.a, min(best.a, t.a), w);
            } else if (t.a > best.a) {
                best = vec4(mix(best.rgb, t.rgb, w), mix(best.a, t.a, w));
            }
        }
    }
    return best;
}

// Edge softness, one direction: a Gaussian blur of the matte only (the colour
// stays sharp). p0.x = sigma in texels.
@fragment
fn fs_key_soft(in: VsOut) -> @location(0) vec4<f32> {
    let sigma = max(fx.p0.x, 0.01);
    let reach = 3.0 * sigma;
    let taps = min(ceil(reach), 64.0);
    let step = reach / max(taps, 1.0);
    let dir = fx.texel_dir.xy * fx.texel_dir.zw;
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    var sum = c.a;
    var total = 1.0;
    for (var i = 1.0; i <= taps; i = i + 1.0) {
        let x = i * step;
        let w = exp(-0.5 * x * x / (sigma * sigma));
        sum = sum + w * (textureSample(src, src_sampler, to_tex(in.uv + dir * x)).a + textureSample(src, src_sampler, to_tex(in.uv - dir * x)).a);
        total = total + 2.0 * w;
    }
    return vec4(c.rgb, sum / total);
}

// Key cleaner, spill: p0 = (screen hue°, removal %, tolerance %, -), p1.y =
// view (1 the matte), p1.z = 1 to output straight colour with the matte in
// alpha (for the hole and speck passes that follow). On gamma-encoded colour,
// as fs_key: a colour leaning towards the screen is pulled back, along the
// screen's direction, to the hues about 53° either side of it (as far as
// yellow and cyan are from green in this plane: so pure screen green
// goes grey and yellow stays yellow), keeping its lightness.
@fragment
fn fs_key_clean(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    let g = pow(max(unpremul(c), vec3(0.0)), vec3(1.0 / 2.2));
    let y = dot(g, LUMA);
    let k = normalize(chroma(hue_rgb(fx.p0.x)));
    let cc = chroma(g);
    let along = dot(cc, k);
    let across = abs(cc.x * k.y - cc.y * k.x);
    let spill = max(along - across * 0.75 - fx.p0.z / 100.0 * 0.5, 0.0);
    let c2 = cc - k * spill * (fx.p0.y / 100.0);
    let r = y + 1.5748 * c2.y;
    let b = y + 1.8556 * c2.x;
    let gg = (y - 0.2126 * r - 0.0722 * b) / 0.7152;
    let lin = pow(max(vec3(r, gg, b), vec3(0.0)), vec3(2.2));
    if (fx.p1.z > 0.5) {
        return vec4(lin, c.a);
    }
    if (fx.p1.y > 0.5) {
        return key_matte(c.a);
    }
    return vec4(lin * c.a, c.a);
}

// Straight colour with the alpha as it is (for passes that work on the matte).
@fragment
fn fs_straight(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    return vec4(unpremul(c), c.a);
}

// Refine matte, smoothing: after the matte is blurred, steepen it again round
// 50% so the jagged steps come back as a smooth edge. p0.x = steepness.
// Straight colour in and out.
@fragment
fn fs_matte_steep(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    return vec4(c.rgb, clamp((c.a - 0.5) * fx.p0.x + 0.5, 0.0, 1.0));
}

// Refine matte, colour from within, one direction: an average of straight
// colour weighted towards the solid (alpha⁴), so the edge can borrow colour
// from inside. p0.x = reach in texels, p0.y = 1 for the second (down) pass.
// The first pass writes (weighted colour, weight); the second divides.
@fragment
fn fs_decon_blur(in: VsOut) -> @location(0) vec4<f32> {
    let dir = fx.texel_dir.xy * fx.texel_dir.zw;
    let reach = clamp(fx.p0.x, 1.0, 64.0);
    let taps = ceil(reach);
    let step = reach / taps;
    let second = fx.p0.y > 0.5;
    var sum = vec4(0.0);
    for (var i = -taps; i <= taps; i = i + 1.0) {
        let t = textureSample(src, src_sampler, to_tex(in.uv + dir * i * step));
        if (second) {
            sum = sum + t;
        } else {
            let w = t.a * t.a * t.a * t.a;
            sum = sum + vec4(t.rgb * w, w);
        }
    }
    let n = 2.0 * taps + 1.0;
    if (second) {
        return select(vec4(0.0), vec4(sum.rgb / sum.a, 1.0), sum.a > 1e-6);
    }
    return sum / n;
}

// Matte choker, last step, from straight colour: p0 = (contrast %, midpoint
// %, view). Contrast steepens the matte round the midpoint, so what's half
// see-through goes towards solid or gone (100% is a hard edge).
@fragment
fn fs_matte_out(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    let m = fx.p0.y / 100.0;
    let gain = 1.0 / max(1.0 - fx.p0.x / 100.0, 0.002);
    let a = select(clamp((c.a - m) * gain + m, 0.0, 1.0), c.a, fx.p0.x <= 0.0);
    if (fx.p0.z > 0.5) {
        return key_matte(a);
    }
    return vec4(c.rgb * a, a);
}

// The keyed layer from straight colour and matte: p2.x = view (see fs_key).
@fragment
fn fs_key_out(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    if (fx.p2.x > 0.5) {
        return key_matte(c.a);
    }
    return vec4(c.rgb * c.a, c.a);
}

// Vignette: p0 = (amount %, size %, softness %).
@fragment
fn fs_vignette(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
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
    let c = textureSample(src, src_sampler, to_tex(in.uv));
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
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    let d = fx.texel_dir.xy * max(fx.p0.y, 0.5);
    var sum = vec4(0.0);
    for (var i = 0; i < 8; i = i + 1) {
        let a = f32(i) * 0.7853982;
        sum = sum + textureSample(src, src_sampler, to_tex(in.uv + vec2(cos(a), sin(a)) * d));
    }
    let blurred = sum / 8.0;
    let out = c + (c - blurred) * (fx.p0.x / 100.0);
    let a = clamp(out.a, 0.0, 1.0);
    return vec4(clamp(out.rgb, vec3(0.0), vec3(a)), a);
}

// Glow, step 1: keep the bright parts. p0 = (threshold %, -, -, softness %).
@fragment
fn fs_bright(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    let y = dot(to_display(unpremul(c)), LUMA);
    let t = fx.p0.x / 100.0;
    let s = max(fx.p0.w / 100.0, 0.001);
    return c * smoothstep(t - s * 0.5, t + s * 0.5, y);
}

// Drop shadow, step 1: the layer's shape, moved. p0 = (offset x texels, offset y texels, -, opacity %).
@fragment
fn fs_shadow(in: VsOut) -> @location(0) vec4<f32> {
    let a = textureSample(src, src_sampler, to_tex(in.uv - fx.p0.xy * fx.texel_dir.xy)).a;
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
    let c = textureSample(src, src_sampler, to_tex(in.uv));
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
    return textureSample(src, src_sampler, to_tex(in.uv));
}

// ---- Stylise effects ----------------------------------------------------------
// Each reads the layer (premultiplied, linear) and its parameters (core
// crate effects.rs: flare, shake, aberration, grain, glitch, leaks). Time
// (seconds of the clip's source) is extra.y, the layer's size in frame pixels
// extra.zw, and texel_dir.z a number of the effect's own (its place in the
// stack), so two of the same kind on one layer don't move together.

// Randomness from integers (the same on every GPU, unlike sin()-based hashes).
fn pcg(v: u32) -> u32 {
    let s = v * 747796405u + 2891336453u;
    let w = ((s >> ((s >> 28u) + 4u)) ^ s) * 277803737u;
    return (w >> 22u) ^ w;
}
fn rnd3(a: u32, b: u32, c: u32) -> f32 {
    return f32(pcg(a ^ pcg(b ^ pcg(c))) >> 8u) / 16777216.0;
}
fn ibits(x: f32) -> u32 {
    return bitcast<u32>(i32(floor(x)));
}
// Smooth noise in time, -1..1.
fn wander(seed: u32, t: f32) -> f32 {
    let i = floor(t);
    let f = t - i;
    let u = f * f * (3.0 - 2.0 * f);
    return mix(rnd3(seed, ibits(i), 17u), rnd3(seed, ibits(i + 1.0), 17u), u) * 2.0 - 1.0;
}
fn seed_of(param: f32) -> u32 {
    return u32(max(param, 0.0)) * 7919u + u32(fx.texel_dir.z) * 104729u;
}
fn max3(v: vec3<f32>) -> f32 {
    return max(max(v.r, v.g), v.b);
}

// Lens flare: p0 = (light x %, light y %, brightness %, size %),
// p1 = (colour, lens style, streak %, ghosts %), p2.x = halo %.
@fragment
fn fs_flare(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    let size = fx.extra.zw;
    let p = in.uv * size;
    let light = fx.p0.xy / 100.0 * size;
    // Sizes are designed for a 1080-line frame.
    let k = fx.p0.w / 100.0 * size.y / 1080.0;
    let bright = fx.p0.z / 100.0;
    let tint = rgb_of(fx.p1.x);
    let style = u32(round(fx.p1.y));
    let d = length(p - light) / k;
    // The bright core and its glow.
    var l = tint * (exp(-d / 14.0) * 1.8 + exp(-d / 60.0) * 0.45 + exp(-d / 260.0) * 0.08);
    if (style != 2u) {
        // A faint starburst (fainter still through an anamorphic lens).
        let a = atan2(p.y - light.y, p.x - light.x);
        let rays = pow(abs(cos(a * 6.0)), 80.0) * 0.7 + pow(abs(cos(a * 6.0 + 0.9)), 120.0) * 0.3;
        l += tint * rays * exp(-d / 110.0) * select(0.14, 0.05, style == 1u);
        // Halo: a thin ring, each colour at a slightly different radius.
        let halo = fx.p2.x / 100.0;
        let r = vec3(196.0, 200.0, 204.0);
        l += vec3(exp(-pow((d - r.x) / 10.0, 2.0)), exp(-pow((d - r.y) / 10.0, 2.0)), exp(-pow((d - r.z) / 10.0, 2.0))) * 0.18 * halo;
        // The streak: long and blue for an anamorphic lens.
        let streak = fx.p1.z / 100.0;
        let dx = abs(p.x - light.x) / k;
        // Its thickness in pixels: at least about one, however small the layer.
        let dy = abs(p.y - light.y);
        if (style == 1u) {
            l += vec3(0.35, 0.6, 1.0) * exp(-dy / max(1.6 * k, 0.8)) * (exp(-dx / 900.0) * 0.9 + exp(-dx / 120.0)) * streak;
        } else {
            l += tint * exp(-dy / max(1.2 * k, 0.7)) * exp(-dx / 260.0) * 0.8 * streak;
        }
        // Ghosts: reflections along the line from the light through the middle.
        let ghosts = fx.p1.w / 100.0;
        let centre = size * 0.5;
        let ks = array<f32, 6>(-0.55, -0.25, 0.3, 0.55, 0.8, 1.35);
        let rs = array<f32, 6>(70.0, 30.0, 18.0, 46.0, 24.0, 110.0);
        let hs = array<vec3<f32>, 6>(vec3(0.3, 1.0, 0.5), vec3(1.0, 0.6, 0.2), vec3(0.6, 0.6, 1.0), vec3(1.0, 0.3, 0.6), vec3(0.4, 0.9, 1.0), vec3(0.8, 0.5, 1.0));
        for (var g = 0; g < 6; g = g + 1) {
            let gp = centre + (light - centre) * ks[g];
            let gd = length(p - gp) / k;
            let disc = smoothstep(rs[g], rs[g] * 0.6, gd) * (0.6 + 0.4 * smoothstep(rs[g] * 0.5, rs[g], gd));
            l += hs[g] * mix(tint, vec3(1.0), 0.5) * disc * 0.16 * ghosts;
        }
    }
    l *= bright;
    return vec4(c.rgb + l, max(c.a, clamp(max3(l), 0.0, 1.0)));
}

// Camera shake: p0 = (amount px, rotation °, speed Hz, roughness %),
// p1 = (zoom %, edges, seed).
@fragment
fn fs_shake(in: VsOut) -> @location(0) vec4<f32> {
    let size = fx.extra.zw;
    let seed = seed_of(fx.p1.z);
    let t = fx.extra.y * fx.p0.z;
    let rough = fx.p0.w / 100.0;
    // Two octaves: a slow wander and a quicker jitter.
    let n = vec3(
        wander(seed, t) + rough * 0.5 * wander(seed + 11u, t * 2.7),
        wander(seed + 1u, t * 1.1) + rough * 0.5 * wander(seed + 12u, t * 2.9),
        wander(seed + 2u, t * 0.9) + rough * 0.5 * wander(seed + 13u, t * 2.3),
    ) / (1.0 + rough * 0.5);
    let off = n.xy * fx.p0.x;
    let a = n.z * radians(fx.p0.y);
    let zoom = max(fx.p1.x / 100.0, 0.01);
    // Where this pixel comes from in the unshaken picture.
    let p = in.uv * size - size * 0.5 - off;
    let q = vec2(p.x * cos(a) + p.y * sin(a), -p.x * sin(a) + p.y * cos(a)) / zoom + size * 0.5;
    var uv = q / size;
    let edges = u32(round(fx.p1.y));
    if (edges == 2u && (uv.x < 0.0 || uv.y < 0.0 || uv.x > 1.0 || uv.y > 1.0)) {
        return vec4(0.0);
    }
    if (edges == 0u) {
        uv = 1.0 - abs((abs(uv) % vec2(2.0)) - 1.0);
    } else {
        uv = clamp(uv, vec2(0.0), vec2(1.0));
    }
    return textureSampleLevel(src, src_sampler, to_tex(uv), 0.0);
}

// Chromatic aberration: p0 = (amount px, direction, angle °, falloff %).
@fragment
fn fs_aberration(in: VsOut) -> @location(0) vec4<f32> {
    let size = fx.extra.zw;
    var off: vec2<f32>;
    if (u32(round(fx.p0.y)) == 0u) {
        // From the centre, growing towards the edges.
        let d = (in.uv - 0.5) * size;
        let r = length(d) / (0.5 * length(size));
        let w = mix(1.0, r * r, fx.p0.w / 100.0) * r;
        off = select(vec2(0.0), d / max(length(d), 1e-4), length(d) > 1e-4) * fx.p0.x * w;
    } else {
        let a = radians(fx.p0.z);
        off = vec2(cos(a), -sin(a)) * fx.p0.x;
    }
    let o = off / size;
    let r = textureSampleLevel(src, src_sampler, to_tex(in.uv + o), 0.0);
    let g = textureSampleLevel(src, src_sampler, to_tex(in.uv), 0.0);
    let b = textureSampleLevel(src, src_sampler, to_tex(in.uv - o), 0.0);
    return vec4(r.r, g.g, b.b, max(max(r.a, g.a), b.a));
}

// Value noise in 2D (bilinear between random corners), 0..1.
fn vnoise(seed: u32, p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = p - i;
    let u = f * f * (3.0 - 2.0 * f);
    let a = rnd3(seed, ibits(i.x), ibits(i.y));
    let b = rnd3(seed, ibits(i.x + 1.0), ibits(i.y));
    let c = rnd3(seed, ibits(i.x), ibits(i.y + 1.0));
    let d = rnd3(seed, ibits(i.x + 1.0), ibits(i.y + 1.0));
    return mix(mix(a, b, u.x), mix(c, d, u.x), u.y);
}

// Film grain: p0 = (amount %, size px, colour %, in highlights %), p1 = (frame rate, seed).
@fragment
fn fs_grain(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    if (c.a <= 1e-5) {
        return c;
    }
    let size = fx.extra.zw;
    let frame = select(0.0, floor(fx.extra.y * fx.p1.x + 1e-3), fx.p1.x > 0.0);
    let seed = seed_of(fx.p1.y) + ibits(frame) * 2654435761u;
    let g = in.uv * size / max(fx.p0.y, 0.25);
    // Two layers of noise, a little like grain clumps; -1..1.
    let mono = (vnoise(seed, g) + vnoise(seed + 5u, g * 2.1 + 3.7) * 0.6 - 0.8) / 0.8;
    let col = vec3(
        (vnoise(seed + 31u, g) + vnoise(seed + 36u, g * 2.1 + 3.7) * 0.6 - 0.8) / 0.8,
        (vnoise(seed + 41u, g) + vnoise(seed + 46u, g * 2.1 + 3.7) * 0.6 - 0.8) / 0.8,
        (vnoise(seed + 51u, g) + vnoise(seed + 56u, g * 2.1 + 3.7) * 0.6 - 0.8) / 0.8,
    );
    let n = mix(vec3(mono), col, fx.p0.z / 100.0);
    // On display values: strongest in the midtones, less in deep shadows and
    // (unless asked for) in the highlights.
    let d = to_display(c.rgb / c.a);
    let l = dot(d, LUMA);
    let w = (0.45 + 0.55 * smoothstep(0.0, 0.25, l)) * mix(1.0 - smoothstep(0.55, 1.0, l), 1.0, fx.p0.w / 100.0);
    let out = to_linear(max(d + n * fx.p0.x / 100.0 * 0.14 * w, vec3(0.0)));
    return vec4(out * c.a, c.a);
}

// Glitch: p0 = (amount %, block px, shift px, colour split px), p1 = (scan lines %, speed Hz, seed).
@fragment
fn fs_glitch(in: VsOut) -> @location(0) vec4<f32> {
    let size = fx.extra.zw;
    let amount = fx.p0.x / 100.0;
    let block = max(fx.p0.y, 1.0);
    let seed = seed_of(fx.p1.z);
    let tick = select(0.0, floor(fx.extra.y * fx.p1.y), fx.p1.y > 0.0);
    let st = ibits(tick);
    let p = in.uv * size;
    // Horizontal slices that jump sideways.
    let band = floor(p.y / block);
    var shift = 0.0;
    var hit = 0.0;
    if (rnd3(seed, st, ibits(band)) < amount * 0.45) {
        shift = (rnd3(seed + 1u, st, ibits(band)) - 0.5) * 2.0 * fx.p0.z;
        hit = 1.0;
    }
    // Wider blocks that move on their own.
    let cell = floor(p / vec2(block * 3.0, block));
    if (rnd3(seed + 2u, st, ibits(cell.x) * 7919u + ibits(cell.y)) < amount * 0.2) {
        shift += (rnd3(seed + 3u, st, ibits(cell.x) + ibits(cell.y) * 31u) - 0.5) * 2.0 * fx.p0.z;
        hit = 1.0;
    }
    // Now and then the whole picture splits a little.
    let whole = select(0.0, 0.35, rnd3(seed + 4u, st, 1u) < amount * 0.3);
    let split = fx.p0.w * (hit + whole) / size.x;
    var uv = vec2(fract((p.x - shift) / size.x), in.uv.y);
    let r = textureSampleLevel(src, src_sampler, to_tex(vec2(fract(uv.x + split), uv.y)), 0.0);
    let g = textureSampleLevel(src, src_sampler, to_tex(uv), 0.0);
    let b = textureSampleLevel(src, src_sampler, to_tex(vec2(fract(uv.x - split), uv.y)), 0.0);
    var c = vec4(r.r, g.g, b.b, max(max(r.a, g.a), b.a));
    // Scan lines, stronger where it's glitching.
    let scan = fx.p1.x / 100.0 * (0.25 + 0.75 * hit);
    let line = step(0.5, fract(p.y / 3.0));
    c = vec4(c.rgb * (1.0 - scan * 0.5 * line), c.a);
    return c;
}

// Light leaks: p0 = (colour 1, colour 2, intensity %, size %), p1 = (drift Hz, from, seed).
@fragment
fn fs_leaks(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    let aspect = fx.extra.x;
    let seed = seed_of(fx.p1.z);
    let t = fx.extra.y * fx.p1.x;
    let edge = u32(round(fx.p1.y));
    let ca = rgb_of(fx.p0.x);
    let cb = rgb_of(fx.p0.y);
    var light = vec3(0.0);
    for (var k = 0u; k < 3u; k = k + 1u) {
        let s = seed + k * 101u;
        let along = 0.5 + 0.5 * wander(s, t + f32(k) * 3.1);
        let wob = 0.06 * wander(s + 7u, t * 1.3);
        var pos: vec2<f32>;
        switch edge {
            case 0u: { pos = vec2(-0.08 + wob, along); }
            case 1u: { pos = vec2(1.08 + wob, along); }
            case 2u: { pos = vec2(along, -0.08 + wob); }
            case 3u: { pos = vec2(along, 1.08 + wob); }
            case 4u: {
                let corner = vec2(f32((k + u32(rnd3(seed, 9u, 9u) * 4.0)) % 2u), f32(((k + 1u) / 2u) % 2u));
                pos = corner + (vec2(0.5) - corner) * 0.08 + vec2(wob, -wob);
            }
            default: { pos = vec2(0.5 + 0.5 * wander(s + 3u, t * 0.7), 0.5 + 0.5 * wander(s + 4u, t * 0.8)); }
        }
        let r = fx.p0.w / 100.0 * 0.55 * (0.7 + 0.6 * rnd3(s, 5u, 5u));
        let d = length((in.uv - pos) * vec2(aspect, 1.0)) / max(r, 1e-3);
        let pulse = 0.65 + 0.35 * wander(s + 9u, t * 1.7);
        let tone = mix(ca, cb, clamp(rnd3(s, 6u, 6u) * 0.6 + d * 0.4, 0.0, 1.0));
        light += tone * exp(-d * d) * pulse;
    }
    light *= fx.p0.z / 100.0;
    // Screened on, on display values (bright parts stay within white).
    let l = clamp(to_display(light), vec3(0.0), vec3(1.0));
    let base = to_display(select(vec3(0.0), c.rgb / c.a, c.a > 1e-5));
    let a2 = max(c.a, max3(l));
    let screened = 1.0 - (1.0 - base * c.a / max(a2, 1e-5)) * (1.0 - l);
    return vec4(to_linear(screened) * a2, a2);
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

// Neon edges, step 1: the tubes alone (to be blurred into the glow).
@fragment
fn fs_neon_edge(in: VsOut) -> @location(0) vec4<f32> {
    return neon_tube(in.uv);
}

// Halftone: p0 = (style, cell px, angle °, shape), p1 = (contrast %, ink, paper, mix %).
// Style 0 = black & white (ink on paper), 1 = CMYK print (four screens at
// their own angles), 2 = colour dots (each dot the picture's colour).
// Shape 0 = round dots, 1 = square dots, 2 = lines.
fn ht_rgb(v: f32) -> vec3<f32> {
    let c = u32(clamp(round(v), 0.0, 16777215.0));
    return vec3(f32((c >> 16u) & 255u), f32((c >> 8u) & 255u), f32(c & 255u)) / 255.0;
}

fn ht_rot(p: vec2<f32>, a: f32) -> vec2<f32> {
    let c = cos(a);
    let s = sin(a);
    return vec2(c * p.x - s * p.y, s * p.x + c * p.y);
}

/// The cell centre (layer px) of the screen at angle `a` that pixel `p` is in.
fn ht_centre(p: vec2<f32>, cell: f32, a: f32) -> vec2<f32> {
    let q = ht_rot(p, a) / cell;
    return ht_rot((floor(q) + 0.5) * cell, -a);
}

/// How much of pixel `p` a dot of ink amount `v` (0..1) covers.
fn ht_cover(v: f32, p: vec2<f32>, cell: f32, a: f32, shape: f32) -> f32 {
    if (v <= 0.002) {
        return 0.0;
    }
    let f = fract(ht_rot(p, a) / cell) - 0.5;
    let aa = 0.75 / cell;
    var d: f32;
    var r: f32;
    if (shape < 0.5) {
        // A circle's area is the ink amount until the dots touch (at 78.5 %),
        // then they grow into the corners.
        d = length(f);
        r = select(mix(0.5, 0.7072, (v - 0.7854) / 0.2146), sqrt(v / 3.14159), v <= 0.7854);
    } else if (shape < 1.5) {
        d = max(abs(f.x), abs(f.y));
        r = 0.5 * sqrt(v);
    } else {
        d = abs(f.y);
        r = 0.5 * v;
    }
    return 1.0 - smoothstep(r - aa, r + aa, d);
}

/// The layer's straight display colour at layer px `p` (and its alpha).
fn ht_at(p: vec2<f32>, size: vec2<f32>) -> vec4<f32> {
    let c = textureSampleLevel(src, src_sampler, to_tex(clamp(p / size, vec2(0.0), vec2(1.0))), 0.0);
    if (c.a <= 1e-4) {
        return vec4(0.0);
    }
    return vec4(to_display(c.rgb / c.a), c.a);
}

fn ht_contrast(v: f32) -> f32 {
    return clamp((v - 0.5) * fx.p1.x / 100.0 + 0.5, 0.0, 1.0);
}

@fragment
fn fs_halftone(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    if (c.a <= 1e-5) {
        return c;
    }
    let size = fx.extra.zw;
    let p = in.uv * size;
    let cell = max(fx.p0.y, 1.0);
    let a = radians(fx.p0.z);
    let shape = fx.p0.w;
    let ink = ht_rgb(fx.p1.y);
    let paper = ht_rgb(fx.p1.z);
    var out: vec3<f32>;
    if (fx.p0.x < 0.5) {
        let s = ht_at(ht_centre(p, cell, a), size);
        let v = ht_contrast(1.0 - dot(s.rgb, LUMA)) * s.a;
        out = mix(paper, ink, ht_cover(v, p, cell, a, shape));
    } else if (fx.p0.x < 1.5) {
        // Four screens: cyan, magenta and yellow 30° apart around black (at the angle).
        let ac = a - radians(30.0);
        let am = a + radians(30.0);
        let ay = a - radians(45.0);
        let sc = ht_at(ht_centre(p, cell, ac), size);
        let sm = ht_at(ht_centre(p, cell, am), size);
        let sy = ht_at(ht_centre(p, cell, ay), size);
        let sk = ht_at(ht_centre(p, cell, a), size);
        let cmy = vec3(1.0 - sc.r, 1.0 - sm.g, 1.0 - sy.b);
        let k = ht_contrast(min(min(1.0 - sk.r, 1.0 - sk.g), 1.0 - sk.b)) * sk.a;
        // Full under-colour removal: the black screen carries the grey.
        let kc = min(min(1.0 - sc.r, 1.0 - sc.g), 1.0 - sc.b);
        let km = min(min(1.0 - sm.r, 1.0 - sm.g), 1.0 - sm.b);
        let ky = min(min(1.0 - sy.r, 1.0 - sy.g), 1.0 - sy.b);
        let vc = ht_contrast(clamp((cmy.x - kc) / max(1.0 - kc, 1e-3), 0.0, 1.0)) * sc.a;
        let vm = ht_contrast(clamp((cmy.y - km) / max(1.0 - km, 1e-3), 0.0, 1.0)) * sm.a;
        let vy = ht_contrast(clamp((cmy.z - ky) / max(1.0 - ky, 1e-3), 0.0, 1.0)) * sy.a;
        out = paper;
        out *= mix(vec3(1.0), vec3(0.0, 0.68, 0.94), ht_cover(vc, p, cell, ac, shape));
        out *= mix(vec3(1.0), vec3(0.93, 0.0, 0.55), ht_cover(vm, p, cell, am, shape));
        out *= mix(vec3(1.0), vec3(1.0, 0.94, 0.0), ht_cover(vy, p, cell, ay, shape));
        out *= mix(vec3(1.0), ink, ht_cover(k, p, cell, a, shape));
    } else {
        // Dots of the picture's own colour (a little richer), as big as it is dark.
        let s = ht_at(ht_centre(p, cell, a), size);
        let l = dot(s.rgb, LUMA);
        let v = ht_contrast(0.25 + 0.75 * (1.0 - l)) * s.a;
        let rich = clamp(mix(vec3(l), s.rgb, 1.35), vec3(0.0), vec3(1.0));
        out = mix(paper, rich, ht_cover(v, p, cell, a, shape));
    }
    let orig = to_display(c.rgb / c.a);
    let d = mix(orig, out, fx.p1.w / 100.0);
    return vec4(to_linear(max(d, vec3(0.0))) * c.a, c.a);
}

// CRT / VHS: p0 = (curvature %, scan lines %, line spacing px, phosphor mask %),
// p1 = (colour bleed px, wobble px, noise %, tracking %),
// p2 = (vignette %, brightness %, speed Hz, seed).
fn crt_at(uv: vec2<f32>) -> vec4<f32> {
    let c = textureSampleLevel(src, src_sampler, to_tex(clamp(uv, vec2(0.0), vec2(1.0))), 0.0);
    if (c.a <= 1e-5) {
        return vec4(0.0);
    }
    return vec4(to_display(c.rgb / c.a), c.a);
}

@fragment
fn fs_crt(in: VsOut) -> @location(0) vec4<f32> {
    let size = fx.extra.zw;
    let t = fx.extra.y * max(fx.p2.z, 0.0) / 10.0;
    let seed = seed_of(fx.p2.w);
    let frame = ibits(fx.extra.y * 30.0);
    // The bulge of the glass: the picture is pulled in towards the corners.
    let cc = in.uv * 2.0 - 1.0;
    let k = fx.p0.x / 100.0 * 0.3;
    let w = cc * (1.0 + k * vec2(cc.y * cc.y, cc.x * cc.x));
    var uv = w * 0.5 + 0.5;
    let inside = all(abs(w) <= vec2(1.0));
    // Soft rounded corners of the tube (only when it's curved).
    let edge = (vec2(1.0) - abs(w)) * size;
    let round_px = fx.p0.x / 100.0 * 0.06 * min(size.x, size.y);
    let r = max(round_px, 1.0);
    let q = max(vec2(r) - edge, vec2(0.0));
    let corner = select(0.0, clamp(r - length(q) + 0.5, 0.0, 1.0), inside);
    let line = floor(uv.y * size.y);
    // Tracking: each line wobbles sideways a little, and a band of worse
    // tracking rolls down the picture.
    let li = ibits(line);
    let wob = (wander(seed + 3u, line * 0.04 + t * 8.0) * 0.6 + (rnd3(seed, li, frame) - 0.5) * 0.8) * fx.p1.y;
    let band_at = fract(t * 0.35 + rnd3(seed, 7u, 7u));
    let dy = (uv.y - band_at);
    let in_band = exp(-dy * dy / 0.0016);
    let track = fx.p1.w / 100.0;
    let band_shift = in_band * track * (20.0 + 40.0 * rnd3(seed + 9u, li, frame)) ;
    uv.x += (wob + band_shift) / size.x;
    // Colour bleed: the brightness is sharp, the colour smeared to the right
    // (as on tape), with red and blue a little apart.
    let base = crt_at(uv);
    var col = base.rgb;
    let bleed = fx.p1.x;
    if (bleed > 0.01) {
        var smear = vec3(0.0);
        for (var i = 0; i < 6; i++) {
            let o = f32(i) / 5.0 * bleed;
            smear += crt_at(uv - vec2(o / size.x, 0.0)).rgb;
        }
        smear /= 6.0;
        let y = dot(col, LUMA);
        col = clamp(vec3(y) + (smear - vec3(dot(smear, LUMA))), vec3(0.0), vec3(1.0));
        let split = bleed * 0.35 / size.x;
        col.r = mix(col.r, crt_at(uv + vec2(split, 0.0)).r, 0.5);
        col.b = mix(col.b, crt_at(uv - vec2(split, 0.0)).b, 0.5);
    }
    // Noise: snow that changes every frame, and streaks in the tracking band.
    let noise = fx.p1.z / 100.0;
    let px = in.uv * size;
    let snow = rnd3(seed + 1u, ibits(px.x) ^ (ibits(px.y) << 16u), frame) - 0.5;
    col += vec3(snow * noise * 0.5);
    let streak = step(1.0 - in_band * track * 0.35, rnd3(seed + 2u, li, frame)) * rnd3(seed + 4u, ibits(px.x / 6.0), li ^ frame);
    col = mix(col, vec3(1.0), streak * 0.8);
    // Scan lines: dark gaps between the lines of the picture.
    let spacing = max(fx.p0.z, 1.0);
    let s = 0.5 + 0.5 * cos(6.2831853 * (uv.y * size.y) / spacing);
    col *= mix(1.0, 0.25 + 0.75 * s, fx.p0.y / 100.0);
    // The phosphor mask: red, green and blue stripes.
    let m = u32(floor(px.x)) % 3u;
    let stripe = select(select(vec3(0.0, 0.0, 1.0), vec3(0.0, 1.0, 0.0), m == 1u), vec3(1.0, 0.0, 0.0), m == 0u);
    col *= mix(vec3(1.0), 0.35 + stripe * 1.3, fx.p0.w / 100.0);
    // Darker towards the edges of the tube, then the brightness.
    let vig = 1.0 - fx.p2.x / 100.0 * pow(clamp(length(cc) * 0.72, 0.0, 1.0), 2.5);
    col *= vig * fx.p2.y / 100.0;
    // Outside the glass is black (over the layer's own shape).
    let a = max(base.a, 0.0) * corner + (1.0 - corner) * textureSample(src, src_sampler, to_tex(in.uv)).a;
    let shown = col * corner;
    return vec4(to_linear(max(shown, vec3(0.0))) * a, a);
}
