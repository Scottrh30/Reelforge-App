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

// Wire removal: p0 = the wire's ends (start x, y, end x, y, % of the layer),
// p1 = (width, edge softness, fill gap, all in texels; show area). Inside the
// wire (a capsule round the line) the picture is filled from either side:
// samples just past each edge, at the same point along the wire (averaged
// over a few texels along it), blended across the width.
@fragment
fn fs_wire(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    let t = fx.texel_dir.xy;
    let p = in.uv / t;
    let a = fx.p0.xy / 100.0 / t;
    let b = fx.p0.zw / 100.0 / t;
    let d = b - a;
    let len = length(d);
    if (len < 0.5) {
        return c;
    }
    let u = d / len;
    let n = vec2(-u.y, u.x);
    let rel = p - a;
    let along = dot(rel, u);
    let across = dot(rel, n);
    let half_w = fx.p1.x * 0.5;
    let soft = max(fx.p1.y, 0.0);
    let dist = length(rel - u * clamp(along, 0.0, len));
    let mask = 1.0 - smoothstep(half_w, half_w + max(soft, 0.001), dist);
    if (mask <= 0.0) {
        return c;
    }
    let outer = half_w + soft + fx.p1.z + 0.5;
    let foot = a + u * along;
    var s1 = vec4(0.0);
    var s2 = vec4(0.0);
    for (var i = -2.0; i <= 2.0; i = i + 1.0) {
        let q = foot + u * i;
        s1 = s1 + textureSample(src, src_sampler, to_tex((q + n * outer) * t));
        s2 = s2 + textureSample(src, src_sampler, to_tex((q - n * outer) * t));
    }
    let f = clamp((across + outer) / (2.0 * outer), 0.0, 1.0);
    var out = mix(c, mix(s2, s1, f) / 5.0, mask);
    if (fx.p1.w > 0.5) {
        out = mix(out, vec4(out.a, 0.0, 0.0, out.a), 0.6 * mask);
    }
    return out;
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

// Colour wheels (see core effects::wheels::apply, which does the same sums):
// p0 = lift x, y, master, gamma x; p1 = gamma y, master, gain x, y;
// p2 = gain master, offset x, y, master; p3 = saturation, contrast, pivot, hue.
fn wheel_tint(x: f32, y: f32) -> vec3<f32> {
    let cb = x * 0.0035;
    let cr = y * 0.0035;
    return vec3(1.5748 * cr, -0.1873 * cb - 0.4681 * cr, 1.8556 * cb);
}

@fragment
fn fs_wheels(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    let g = to_display(unpremul(c));
    let lift = fx.p0.z * 0.005 + wheel_tint(fx.p0.x, fx.p0.y) * 0.4;
    let gain = exp2(fx.p2.x * 0.01 + wheel_tint(fx.p1.z, fx.p1.w));
    let gamma = exp2(-(fx.p1.y * 0.01 + wheel_tint(fx.p0.w, fx.p1.x)));
    let offset = fx.p2.w * 0.005 + wheel_tint(fx.p2.y, fx.p2.z) * 0.4;
    var v = g + lift * (1.0 - g);
    v = pow(max(v * gain, vec3(0.0)), gamma) + offset;
    v = (v - fx.p3.z * 0.01) * (fx.p3.y * 0.01) + fx.p3.z * 0.01;
    let luma = dot(v, LUMA);
    let cb0 = (v.b - luma) / 1.8556;
    let cr0 = (v.r - luma) / 1.5748;
    let a = radians(fx.p3.w);
    let s = sin(a);
    let co = cos(a);
    let sat = fx.p3.x * 0.01;
    let cb = (cb0 * co - cr0 * s) * sat;
    let cr = (cb0 * s + cr0 * co) * sat;
    let r = luma + 1.5748 * cr;
    let b = luma + 1.8556 * cb;
    let gr = (luma - 0.2126 * r - 0.0722 * b) / 0.7152;
    return vec4(to_linear(max(vec3(r, gr, b), vec3(0.0))) * c.a, c.a);
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

// Anamorphic flare, step 1: what streaks. The bright parts (as fs_bright),
// a light at (p2.x, p2.y) % of the layer p2.z px across, or both (p1.w 0, 1, 2).
@fragment
fn fs_streak_src(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    var out = vec4(0.0);
    if (fx.p1.w < 0.5 || fx.p1.w > 1.5) {
        let y = dot(to_display(unpremul(c)), LUMA);
        let t = fx.p0.x / 100.0;
        let s = max(fx.p0.w / 100.0, 0.001);
        out = c * smoothstep(t - s * 0.5, t + s * 0.5, y);
    }
    if (fx.p1.w > 0.5) {
        let size = fx.extra.zw;
        let r = max(fx.p2.z, 0.5) * 0.5;
        let d = length(in.uv * size - fx.p2.xy / 100.0 * size);
        let k = 1.0 - smoothstep(r * 0.3, r, d);
        out = out + vec4(vec3(4.0 * k), k) * (1.0 - out.a);
    }
    return out;
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

// A colour parameter (0xRRGGBB) as display values.
fn rgb_hex(v: f32) -> vec3<f32> {
    let c = u32(clamp(round(v), 0.0, 16777215.0));
    return vec3(f32((c >> 16u) & 255u), f32((c >> 8u) & 255u), f32(c & 255u)) / 255.0;
}

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
    if (style >= 3u) {
        l = flare_lib(style, p, light, size, k, tint);
    } else if (style != 2u) {
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

// HSL qualifier (see core effects::qualify, which does the same sums):
// p0 = (hue °, width °, softness °, sat low %), p1 = (sat high %, sat soft %,
// lum low %, lum high %), p2 = (lum soft %, use hue, use sat, use lum), p3 =
// (invert, show, hue shift °, saturation %), p4 = (brightness %, contrast %,
// tint colour, tint amount %).
fn q_band(x: f32, lo0: f32, hi0: f32, soft: f32) -> f32 {
    let lo = min(lo0, hi0);
    let hi = max(lo0, hi0);
    return q_step(lo - soft, lo, x) * (1.0 - q_step(hi, hi + soft, x));
}

fn q_step(e0: f32, e1: f32, x: f32) -> f32 {
    if (e1 <= e0) {
        return select(0.0, 1.0, x >= e0);
    }
    let t = clamp((x - e0) / (e1 - e0), 0.0, 1.0);
    return t * t * (3.0 - 2.0 * t);
}

fn hsv_hs(c: vec3<f32>) -> vec2<f32> {
    let mx = max(max(c.r, c.g), c.b);
    let mn = min(min(c.r, c.g), c.b);
    let d = mx - mn;
    var h = 0.0;
    if (d > 1e-6) {
        if (mx == c.r) {
            h = 60.0 * (((c.g - c.b) / d) - 6.0 * floor(((c.g - c.b) / d) / 6.0));
        } else if (mx == c.g) {
            h = 60.0 * ((c.b - c.r) / d + 2.0);
        } else {
            h = 60.0 * ((c.r - c.g) / d + 4.0);
        }
    }
    return vec2(h, select(d / mx, 0.0, mx <= 1e-6));
}

fn q_mod(a: f32, b: f32) -> f32 {
    return a - b * floor(a / b);
}

@fragment
fn fs_qualify(in: VsOut) -> @location(0) vec4<f32> {
    let src_c = textureSample(src, src_sampler, to_tex(in.uv));
    let c = to_display(unpremul(src_c));
    let hs = hsv_hs(c);
    let l = dot(c, LUMA);
    var m = 1.0;
    if (fx.p2.y >= 0.5) {
        let d = min(q_mod(hs.x - fx.p0.x, 360.0), q_mod(fx.p0.x - hs.x, 360.0));
        let half = max(fx.p0.y, 0.0) / 2.0;
        m = m * (1.0 - q_step(half, half + max(fx.p0.z, 0.0), d)) * q_step(0.0, 0.08, hs.y);
    }
    if (fx.p2.z >= 0.5) {
        m = m * q_band(hs.y, fx.p0.w / 100.0, fx.p1.x / 100.0, max(fx.p1.y, 0.0) / 100.0);
    }
    if (fx.p2.w >= 0.5) {
        m = m * q_band(l, fx.p1.z / 100.0, fx.p1.w / 100.0, max(fx.p2.x, 0.0) / 100.0);
    }
    if (fx.p3.x >= 0.5) {
        m = 1.0 - m;
    }
    var out = c;
    let show = u32(fx.p3.y + 0.5);
    if (show == 1u) {
        out = vec3(m);
    } else if (show == 2u) {
        let g = 0.35 + l * 0.3;
        out = vec3(g) + (c - vec3(g)) * m;
    } else {
        let cb0 = (c.b - l) / 1.8556;
        let cr0 = (c.r - l) / 1.5748;
        let a = radians(fx.p3.z);
        let sat = max(fx.p3.w, 0.0) / 100.0;
        let cb = (cb0 * cos(a) - cr0 * sin(a)) * sat;
        let cr = (cb0 * sin(a) + cr0 * cos(a)) * sat;
        let y = (l * exp2(fx.p4.x / 100.0) - 0.5) * (max(fx.p4.y, 0.0) / 100.0) + 0.5;
        var g = vec3(y + 1.5748 * cr, 0.0, y + 1.8556 * cb);
        g.g = (y - 0.2126 * g.r - 0.0722 * g.b) / 0.7152;
        let t = clamp(fx.p4.w / 100.0, 0.0, 1.0);
        if (t > 0.0) {
            let tint = rgb_hex(fx.p4.z);
            let ty = max(dot(tint, LUMA), 1e-3);
            g = g + (tint * y / ty - g) * t * 0.6;
        }
        g = max(g, vec3(0.0));
        out = c + (g - c) * m;
    }
    return vec4(to_linear(max(out, vec3(0.0))) * src_c.a, src_c.a);
}

// Light rays: each pixel gathers the (bright) layer along the line to the
// source, fading as it goes: rays streaming out from the source. p0 =
// (source x %, y %, length %, brightness %), p1 = (threshold %, softness %,
// colour, from), p2.x = show; extra.zw = layer size.
fn ray_light(uv: vec2<f32>) -> vec3<f32> {
    let c = textureSampleLevel(src, src_sampler, to_tex(uv), 0.0);
    if (fx.p1.w >= 0.5) {
        return c.rgb;
    }
    let y = dot(to_display(unpremul(c)), LUMA);
    let t = fx.p1.x / 100.0;
    let s = max(fx.p1.y / 100.0, 0.01);
    return c.rgb * smoothstep(t - s * 0.5, t + s * 0.5, y);
}

@fragment
fn fs_rays(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    let light = fx.p0.xy / 100.0;
    let len = clamp(fx.p0.z / 100.0, 0.0, 1.0);
    let n = 64;
    // A different start for each pixel, so the steps don't show as bands.
    let jitter = rnd3(7u, ibits(in.uv.x * fx.extra.z), ibits(in.uv.y * fx.extra.w)) / f32(n);
    var acc = vec3(0.0);
    var total = 0.0;
    for (var i = 0; i < n; i = i + 1) {
        let k = (f32(i) / f32(n) + jitter) * len;
        let w = 1.0 - k * 0.8;
        acc = acc + ray_light(in.uv + (light - in.uv) * k) * w;
        total = total + w;
    }
    let rays = acc / max(total, 1e-3) * rgb_of(fx.p1.z) * (fx.p0.w / 100.0) * 1.6;
    let a = clamp(max3(rays), 0.0, 1.0);
    if (fx.p2.x >= 0.5) {
        return vec4(rays, a);
    }
    return vec4(c.rgb + rays, max(c.a, a));
}

// Lens dirt: smudges, smears and specks on the lens (made up from the
// seed), shown where light reaches them: the bright parts of the layer
// round about, or a light placed at (x, y). p0 = (light x %, y %, lit by,
// amount %), p1 = (spread %, threshold %, smudge size %, specks %), p2 =
// (colour, seed); extra.zw = layer size.
fn dirt_bright(uv: vec2<f32>) -> vec3<f32> {
    let c = textureSampleLevel(src, src_sampler, to_tex(uv), 0.0);
    let y = dot(to_display(unpremul(c)), LUMA);
    let t = fx.p1.y / 100.0;
    return c.rgb * smoothstep(t - 0.1, t + 0.1, y);
}

@fragment
fn fs_dirt(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    let size = max(fx.extra.zw, vec2(1.0));
    let p = in.uv * size;
    let m = min(size.x, size.y);
    let spread = max(fx.p1.x / 100.0, 0.01);
    var light = vec3(0.0);
    if (fx.p0.z < 0.5) {
        // The bright parts nearby, gathered over a wide disc (a golden-angle
        // spiral, turned differently at each pixel so it blends rather than steps),
        // nearer ones counting for more.
        let r = spread * 0.5 * m;
        let turn = rnd3(11u, ibits(p.x), ibits(p.y)) * 6.28318;
        var total = 0.0;
        for (var i = 0; i < 48; i = i + 1) {
            let f = (f32(i) + 0.5) / 48.0;
            let a = f32(i) * 2.39996 + turn;
            let o = vec2(cos(a), sin(a)) * sqrt(f) * r;
            let w = 1.0 - f * 0.7;
            light = light + dirt_bright(in.uv + o / size) * w;
            total = total + w;
        }
        light = light / total * 2.5;
    } else {
        let d = length(p - fx.p0.xy / 100.0 * size) / (spread * m);
        light = vec3(exp(-d * d * 2.5) * 1.4);
    }
    let seed = seed_of(fx.p2.y);
    let k = m * 0.0035 * max(fx.p1.z / 100.0, 0.05);
    // Smudges: soft blotches; smears: long streaks one way; specks: dots and rings.
    let sp = p / (k * 40.0);
    let smudge = smoothstep(0.42, 0.85, fbm2(seed, sp + vec2(fbm2(seed + 20u, sp * 0.7), fbm2(seed + 21u, sp * 0.7)) * 1.5)) * 0.55;
    let sq = rot2(p, 0.6) * vec2(1.0, 7.0);
    let smear = smoothstep(0.55, 0.85, fbm2(seed + 9u, sq / (k * 60.0))) * 0.35;
    let cell_size = k * 9.0;
    let cell = floor(p / cell_size);
    var specks = 0.0;
    for (var j = -1; j <= 1; j = j + 1) {
        for (var i = -1; i <= 1; i = i + 1) {
            let cc = cell + vec2(f32(i), f32(j));
            let r0 = rnd3(seed + 3u, ibits(cc.x), ibits(cc.y));
            if (r0 > fx.p1.w / 100.0 * 0.35) {
                continue;
            }
            let ctr = (cc + vec2(rnd3(seed + 4u, ibits(cc.x), ibits(cc.y)), rnd3(seed + 5u, ibits(cc.x), ibits(cc.y)))) * cell_size;
            let rad = cell_size * (0.08 + 0.45 * pow(rnd3(seed + 6u, ibits(cc.x), ibits(cc.y)), 2.0));
            let d = length(p - ctr);
            let disc = 1.0 - smoothstep(rad * 0.55, rad, d);
            // Some are rings (dried drops).
            let ring = select(disc, disc * smoothstep(rad * 0.35, rad * 0.8, d), r0 < fx.p1.w / 100.0 * 0.08);
            specks = max(specks, ring * (0.4 + 0.6 * r0 / max(fx.p1.w / 100.0 * 0.35, 1e-3)));
        }
    }
    let dirt = clamp(smudge + smear + specks * 0.8, 0.0, 1.0);
    let glow = light * dirt * rgb_of(fx.p2.x) * (fx.p0.w / 100.0);
    return vec4(c.rgb + glow, max(c.a, clamp(max3(glow), 0.0, 1.0)));
}

// The lens flare library (lenses 3 and on): per lens, the core's size,
// rays (count, sharpness, strength), streak (strength, colour), halo
// (radius, strength, rings, rainbow), ghosts (strength, colour, hexagon).
const FLARE_CORE = array<f32, 9>(1.6, 0.7, 1.0, 1.0, 0.9, 2.4, 1.1, 1.0, 0.6);
const FLARE_RAYS = array<vec3<f32>, 9>(vec3(12.0, 40.0, 0.25), vec3(0.0, 1.0, 0.0), vec3(6.0, 60.0, 0.18), vec3(8.0, 50.0, 0.12), vec3(0.0, 1.0, 0.0), vec3(0.0, 1.0, 0.0), vec3(5.0, 30.0, 0.1), vec3(4.0, 70.0, 0.2), vec3(16.0, 160.0, 0.7));
// Streak: strength, colour (0 the light's, 1 blue, 2 rainbow, 3 red, 4 green), length.
const FLARE_STREAK = array<vec3<f32>, 9>(vec3(0.3, 0.0, 300.0), vec3(0.0, 0.0, 1.0), vec3(0.1, 0.0, 200.0), vec3(1.0, 2.0, 700.0), vec3(1.2, 3.0, 1100.0), vec3(0.0, 0.0, 1.0), vec3(0.15, 0.0, 200.0), vec3(0.8, 4.0, 800.0), vec3(0.2, 0.0, 250.0));
// Halo: radius, strength, rings, rainbow.
const FLARE_HALO = array<vec4<f32>, 9>(vec4(260.0, 0.6, 1.0, 0.0), vec4(180.0, 0.3, 1.0, 0.0), vec4(200.0, 0.25, 1.0, 0.0), vec4(230.0, 1.0, 1.0, 1.0), vec4(160.0, 0.4, 1.0, 0.0), vec4(300.0, 0.25, 1.0, 0.0), vec4(150.0, 0.7, 3.0, 0.0), vec4(210.0, 0.5, 2.0, 0.3), vec4(120.0, 0.15, 1.0, 0.0));
// Ghosts: strength, colour (0 the light's, 1 rainbow, 2 red, 3 green), hexagon.
const FLARE_GHOSTS = array<vec3<f32>, 9>(vec3(0.6, 0.0, 0.0), vec3(1.0, 0.0, 0.0), vec3(1.2, 0.0, 1.0), vec3(1.0, 1.0, 1.0), vec3(0.9, 2.0, 0.0), vec3(0.0, 0.0, 0.0), vec3(0.9, 0.0, 0.0), vec3(1.0, 3.0, 1.0), vec3(0.0, 0.0, 0.0));

fn flare_rainbow(f: f32) -> vec3<f32> {
    return 0.5 + 0.5 * cos(6.28318 * (f + vec3(0.0, 0.33, 0.67)));
}

fn flare_lib(lens: u32, p: vec2<f32>, light: vec2<f32>, size: vec2<f32>, k: f32, tint: vec3<f32>) -> vec3<f32> {
    let s = min(lens - 3u, 8u);
    let d = length(p - light) / k;
    let cs = FLARE_CORE[s];
    var l = tint * (exp(-d / (14.0 * cs)) * 1.8 + exp(-d / (60.0 * cs)) * 0.45 + exp(-d / (260.0 * cs)) * 0.1);
    let a = atan2(p.y - light.y, p.x - light.x);
    let rays = FLARE_RAYS[s];
    if (rays.z > 0.0) {
        l += tint * pow(abs(cos(a * rays.x * 0.5)), rays.y) * exp(-d / 160.0) * rays.z;
    }
    let st = FLARE_STREAK[s];
    if (st.x > 0.0) {
        let dx = abs(p.x - light.x) / k;
        let dy = abs(p.y - light.y);
        var sc = tint;
        let mode = u32(st.y);
        if (mode == 2u) {
            sc = flare_rainbow(dx / 600.0);
        } else if (mode == 3u) {
            sc = vec3(1.0, 0.18, 0.12);
        } else if (mode == 4u) {
            sc = vec3(0.3, 1.0, 0.45);
        }
        l += sc * exp(-dy / max(1.4 * k, 0.7)) * (exp(-dx / st.z) * 0.9 + exp(-dx / (st.z * 0.15))) * st.x * fx.p1.z / 100.0;
    }
    let h = FLARE_HALO[s];
    for (var r = 0; r < i32(h.z); r = r + 1) {
        let rr = h.x * (1.0 + f32(r) * 0.45);
        var ring = vec3(exp(-pow((d - rr * 0.98) / 10.0, 2.0)), exp(-pow((d - rr) / 10.0, 2.0)), exp(-pow((d - rr * 1.02) / 10.0, 2.0)));
        if (h.w > 0.0) {
            ring = mix(ring, flare_rainbow((d - rr) / 60.0 + 0.5) * exp(-pow((d - rr) / 30.0, 2.0)), h.w);
        }
        l += ring * 0.18 * h.y * fx.p2.x / 100.0 / (1.0 + f32(r) * 0.6);
    }
    let gh = FLARE_GHOSTS[s];
    if (gh.x > 0.0) {
        let centre = size * 0.5;
        let ks = array<f32, 7>(-0.55, -0.25, 0.3, 0.55, 0.8, 1.35, -1.0);
        let rs = array<f32, 7>(70.0, 30.0, 18.0, 46.0, 24.0, 110.0, 40.0);
        for (var g = 0; g < 7; g = g + 1) {
            let gp = centre + (light - centre) * ks[g];
            let q = (p - gp) / k;
            var gd = length(q);
            if (gh.z > 0.5) {
                // A six-bladed iris.
                let ang = atan2(q.y, q.x);
                let seg = 1.0471976;
                let f = cos(seg * 0.5) / cos(((ang % seg) + seg) % seg - seg * 0.5);
                gd = gd / f;
            }
            let disc = smoothstep(rs[g], rs[g] * 0.6, gd) * (0.6 + 0.4 * smoothstep(rs[g] * 0.5, rs[g], gd));
            var gc = mix(tint, vec3(1.0), 0.4);
            let cm = u32(gh.y);
            if (cm == 1u) {
                gc = flare_rainbow(f32(g) / 7.0);
            } else if (cm == 2u) {
                gc = mix(vec3(1.0, 0.25, 0.1), vec3(1.0, 0.6, 0.2), f32(g) / 6.0);
            } else if (cm == 3u) {
                gc = mix(vec3(0.2, 1.0, 0.4), vec3(0.3, 0.9, 1.0), f32(g) / 6.0);
            }
            l += gc * disc * 0.16 * gh.x * fx.p1.w / 100.0;
        }
    }
    return l;
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

// Nothing (a shatter's layer once it has broken: only its pieces are drawn).
@fragment
fn fs_clear(in: VsOut) -> @location(0) vec4<f32> {
    return vec4(0.0);
}

// Fractal value noise, 0..1.
fn fbm2(seed: u32, p: vec2<f32>) -> f32 {
    return vnoise(seed, p) * 0.55 + vnoise(seed + 1u, p * 2.03) * 0.3 + vnoise(seed + 2u, p * 4.1) * 0.15;
}

fn rot2(v: vec2<f32>, a: f32) -> vec2<f32> {
    let c = cos(a);
    let s = sin(a);
    return vec2(c * v.x - s * v.y, s * v.x + c * v.y);
}

// A tongue of flame from the origin along +x: `len` long, `width` wide at
// its root, torn by noise.
fn tongue(q: vec2<f32>, len: f32, width: f32, seed: u32, shot: u32) -> f32 {
    if (q.x < -width || q.x > len * 1.2) {
        return 0.0;
    }
    let f = clamp(q.x / len, 0.0, 1.2);
    let w = width * (1.0 - f * 0.85) * (0.8 + 0.4 * vnoise(seed ^ shot, vec2(q.x / width * 1.7, 3.0)));
    let body = exp(-pow(q.y / max(w, 0.5), 2.0)) * smoothstep(1.15, 0.6, f) * smoothstep(-width, 0.0, q.x);
    return body * (0.65 + 0.7 * vnoise(seed + shot * 7u, q / max(width, 1.0) * vec2(1.2, 2.5)));
}

// Muzzle flash: p0 = (barrel x %, y %, direction °, size %), p1 = (gun,
// colour, brightness %, first shot s), p2 = (shots/s, shots, light %, seed).
@fragment
fn fs_muzzle(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    let size = fx.extra.zw;
    let t = fx.extra.y;
    let first = fx.p1.w;
    let rate = fx.p2.x;
    let shots = max(round(fx.p2.y), 1.0);
    // The latest shot fired.
    var k = 0.0;
    if (rate > 0.0) {
        k = clamp(floor((t - first) * rate), 0.0, shots - 1.0);
    }
    let age = t - (first + select(0.0, k / max(rate, 1e-3), rate > 0.0));
    let length_s = 0.05;
    if (age < 0.0 || age > length_s) {
        return c;
    }
    let env = pow(1.0 - age / length_s, 1.5);
    let seed = seed_of(fx.p2.w);
    let shot = u32(k);
    let r1 = rnd3(seed, shot, 1u);
    let r2 = rnd3(seed, shot, 2u);
    let k_px = fx.p0.w / 100.0 * size.y / 1080.0;
    let p = in.uv * size;
    let barrel = fx.p0.xy / 100.0 * size;
    // Into the gun's frame: x along the barrel, y across; in 1080p pixels at 100%.
    let dir = -radians(fx.p0.z) + (r1 - 0.5) * 0.08;
    let q = rot2(p - barrel, -dir) / k_px;
    let style = u32(round(fx.p1.x));
    var f = exp(-(q.x * q.x / 900.0 + q.y * q.y / 400.0)) * 1.6;
    if (style == 0u) {
        // Rifle: a long forward flame and a star of short side petals.
        f += tongue(q, 150.0 * (0.8 + 0.4 * r2), 22.0, seed, shot);
        for (var i = 0u; i < 4u; i++) {
            let a = (f32(i) - 1.5) * 0.55 + (rnd3(seed, shot, 10u + i) - 0.5) * 0.3;
            f += tongue(rot2(q, -a), 55.0 * (0.6 + 0.6 * rnd3(seed, shot, 20u + i)), 9.0, seed + i, shot) * 0.7;
        }
    } else if (style == 1u) {
        // Pistol: short and round.
        f += tongue(q, 75.0 * (0.8 + 0.4 * r2), 26.0, seed, shot);
        for (var i = 0u; i < 3u; i++) {
            let a = (f32(i) - 1.0) * 0.8;
            f += tongue(rot2(q, -a), 35.0, 10.0, seed + i, shot) * 0.6;
        }
    } else if (style == 2u) {
        // Shotgun: a wide, ragged burst.
        f += tongue(q, 120.0 * (0.8 + 0.4 * r2), 48.0, seed, shot);
        for (var i = 0u; i < 6u; i++) {
            let a = (f32(i) - 2.5) * 0.3 + (rnd3(seed, shot, 30u + i) - 0.5) * 0.25;
            f += tongue(rot2(q, -a), 100.0 * (0.5 + 0.6 * rnd3(seed, shot, 40u + i)), 12.0, seed + i, shot) * 0.6;
        }
    } else {
        // Side vents (a compensator): two jets out sideways, a small one forward.
        f += tongue(rot2(q, -1.5708), 90.0 * (0.8 + 0.4 * r2), 16.0, seed, shot);
        f += tongue(rot2(q, 1.5708), 90.0 * (0.8 + 0.4 * r1), 16.0, seed + 5u, shot);
        f += tongue(q, 40.0, 14.0, seed + 9u, shot) * 0.8;
    }
    let bright = fx.p1.z / 100.0 * env;
    let tint = rgb_of(fx.p1.y);
    // Whiter where it's hottest.
    let hot = clamp(f - 1.0, 0.0, 1.0);
    let flash = mix(tint, vec3(1.0, 0.97, 0.9), hot * 0.7) * f * bright * 1.6;
    // The scene round it lit by the flash.
    let d = length(p - barrel) / k_px;
    let light = fx.p2.z / 100.0 * env * (exp(-d / 350.0) * 1.2 + exp(-d / 1200.0) * 0.3);
    let lit = c.rgb * (vec3(1.0) + tint * light * 2.0);
    return vec4(lit + flash, max(c.a, clamp(max3(flash), 0.0, 1.0)));
}

// Bullet hit: p0 = (x %, y %, hits at s, surface), p1 = (size %, sparks %,
// dust %, hole %), p2 = (flash %, seed). Surfaces: plaster, concrete,
// metal, glass, wood.
@fragment
fn fs_bullet(in: VsOut) -> @location(0) vec4<f32> {
    var c = textureSample(src, src_sampler, to_tex(in.uv));
    let size = fx.extra.zw;
    let tau = fx.extra.y - fx.p0.z;
    if (tau < 0.0) {
        return c;
    }
    let seed = seed_of(fx.p2.y);
    let surface = u32(round(fx.p0.w));
    let k = fx.p1.x / 100.0 * size.y / 1080.0;
    let p = in.uv * size;
    let hit = fx.p0.xy / 100.0 * size;
    let d = (p - hit) / k;
    let r = length(d);
    let ang = atan2(d.y, d.x);
    let hole_amt = fx.p1.w / 100.0;

    // The hole it leaves (stays).
    var deco = vec4(0.0);
    let rough = vnoise(seed, vec2(ang * 3.0, 1.0)) * 0.5 + vnoise(seed + 3u, vec2(ang * 9.0, 2.0)) * 0.5;
    if (surface == 3u) {
        // Glass: a small hole, a star of cracks and rings round it, catching the light.
        var crack = 0.0;
        for (var i = 0u; i < 14u; i++) {
            let a = f32(i) / 14.0 * 6.2832 + (rnd3(seed, i, 1u) - 0.5) * 0.4;
            let reach = 60.0 + 140.0 * rnd3(seed, i, 2u);
            let along = dot(d, vec2(cos(a), sin(a)));
            let across = abs(dot(d, vec2(-sin(a), cos(a))) + (vnoise(seed + i, vec2(along / 18.0, 0.0)) - 0.5) * 6.0);
            crack = max(crack, exp(-across * across / 0.9) * step(0.0, along) * smoothstep(reach, reach * 0.5, along));
        }
        for (var i = 0u; i < 2u; i++) {
            let rr = 22.0 + 30.0 * f32(i) + 10.0 * rnd3(seed, i, 7u);
            let w = abs(r - rr * (0.9 + 0.2 * rough));
            crack = max(crack, exp(-w * w / 1.2) * 0.7 * step(0.5, vnoise(seed + 11u + i, vec2(ang * 4.0, 0.0))));
        }
        let shatter = smoothstep(9.0, 3.0, r);
        let hole = smoothstep(4.5, 2.5, r);
        deco = vec4(vec3(0.85, 0.9, 0.95) * max(crack, shatter * 0.6) * (1.0 - hole) , max(crack, shatter) * 0.85);
        deco = vec4(deco.rgb * (1.0 - hole * 0.95), max(deco.a, hole * 0.9));
    } else {
        let hr = select(6.0, 5.0, surface == 2u) * (0.85 + 0.3 * rough);
        let rim = select(18.0, 10.0, surface == 2u) * (0.8 + 0.4 * rough);
        let hole = smoothstep(hr, hr * 0.6, r);
        let chip = smoothstep(rim, rim * 0.7, r) * (1.0 - hole);
        var chip_col = vec3(0.82, 0.8, 0.76);
        var scorch = 0.0;
        if (surface == 1u) {
            chip_col = vec3(0.55, 0.55, 0.53);
        } else if (surface == 2u) {
            // Metal: a bright ring of torn metal and a dark scorch round it.
            chip_col = vec3(0.9, 0.88, 0.85);
            scorch = smoothstep(28.0, 8.0, r) * 0.5;
        } else if (surface == 4u) {
            chip_col = vec3(0.78, 0.6, 0.38);
        }
        // Concrete and plaster chip into little cracks too.
        var cracks = 0.0;
        if (surface <= 1u) {
            for (var i = 0u; i < 5u; i++) {
                let a = rnd3(seed, i, 3u) * 6.2832;
                let reach = 18.0 + 22.0 * rnd3(seed, i, 4u);
                let along = dot(d, vec2(cos(a), sin(a)));
                let across = abs(dot(d, vec2(-sin(a), cos(a))));
                cracks = max(cracks, exp(-across * across / 0.6) * step(0.0, along) * smoothstep(reach, rim * 0.5, along));
            }
        }
        let dark = vec3(0.02, 0.02, 0.02);
        let col = mix(mix(chip_col * (0.75 + 0.5 * vnoise(seed + 5u, d * 0.4)), dark, hole), dark, cracks * 0.8);
        let a = max(max(hole, chip * 0.9), max(scorch, cracks * 0.8));
        deco = vec4(mix(col, vec3(0.05), scorch * (1.0 - chip) * (1.0 - hole)) * a, a);
    }
    deco *= hole_amt;
    c = c * (1.0 - deco.a) + deco;

    // A puff of dust (or smoke off metal) blowing out and settling.
    let dust_amt = fx.p1.z / 100.0;
    if (tau < 1.6 && dust_amt > 0.0) {
        let grow = 20.0 + 150.0 * sqrt(tau);
        let centre = vec2(0.0, 30.0 * tau * tau);
        let dd = (d - centre) / grow;
        let n = fbm2(seed + 20u, d / 35.0 + vec2(tau * 0.6, -tau * 0.4));
        let fade = pow(1.0 - tau / 1.6, 1.6);
        var dens = smoothstep(1.0, 0.1, length(dd) + (n - 0.5) * 0.6) * fade * dust_amt * select(0.75, 0.25, surface == 3u);
        var dust_col = vec3(0.78, 0.76, 0.72);
        if (surface == 1u) {
            dust_col = vec3(0.6, 0.6, 0.58);
        } else if (surface == 2u) {
            dust_col = vec3(0.35, 0.35, 0.35);
        } else if (surface == 4u) {
            dust_col = vec3(0.55, 0.42, 0.28);
        }
        dens = clamp(dens * (0.6 + n * 0.8), 0.0, 0.95);
        c = c * (1.0 - dens) + vec4(dust_col * dens, dens);
    }

    // The flash, and sparks flying (more off metal, few off wood and plaster).
    var light = vec3(0.0);
    let flash_amt = fx.p2.x / 100.0;
    if (tau < 0.06) {
        let env = 1.0 - tau / 0.06;
        let rays = pow(abs(cos(ang * 3.0 + f32(seed % 7u))), 30.0);
        light += vec3(1.0, 0.85, 0.55) * (exp(-r / 10.0) * 3.0 + exp(-r / 40.0) * 0.6 + rays * exp(-r / 60.0)) * env * flash_amt;
    }
    let spark_amt = fx.p1.y / 100.0;
    let n_sparks = select(select(select(8u, 24u, surface == 2u), 10u, surface == 3u), 4u, surface == 4u);
    if (tau < 0.45 && spark_amt > 0.0) {
        for (var i = 0u; i < n_sparks; i++) {
            let a = rnd3(seed, i, 11u) * 6.2832;
            let speed = (300.0 + 700.0 * rnd3(seed, i, 12u));
            let life = 0.15 + 0.3 * rnd3(seed, i, 13u);
            if (tau > life) {
                continue;
            }
            let vel = vec2(cos(a), sin(a) - 0.3) * speed;
            let head = vel * tau + vec2(0.0, 900.0) * tau * tau * 0.5;
            let v_now = vel + vec2(0.0, 900.0) * tau;
            let tail = head - v_now * 0.02;
            let ab = head - tail;
            let h = clamp(dot(d - tail, ab) / max(dot(ab, ab), 1e-3), 0.0, 1.0);
            let dist = length(d - tail - ab * h);
            light += vec3(1.0, 0.7, 0.3) * exp(-dist * dist / 1.5) * (1.0 - tau / life) * 1.6 * spark_amt;
        }
    }
    return vec4(c.rgb + light, max(c.a, clamp(max3(light), 0.0, 1.0)));
}

// Disintegrate: the layer erased behind a ragged front (its particles are
// drawn over it in the same pass). The uniform is laid out for the particle
// shader; the sweep sits in slots it doesn't use: p0 = (direction °, starts
// at s, -, takes s), p1 = (roughness %, edge glow colour, glow width %, seed).
// The moment of each point matches core disintegrate::moment.
@fragment
fn fs_disintegrate(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    let size = fx.extra.zw;
    let a = radians(fx.p0.x);
    let d = vec2(cos(a), -sin(a));
    let c0 = 0.0;
    let c1 = size.x * d.x;
    let c2 = size.y * d.y;
    let c3 = size.x * d.x + size.y * d.y;
    let lo = min(min(c0, c1), min(c2, c3));
    let hi = max(max(c0, c1), max(c2, c3));
    let px = in.uv * size;
    let along = (dot(px, d) - lo) / max(hi - lo, 1e-3);
    let rough = clamp(fx.p1.x / 100.0, 0.0, 1.0);
    let seed = u32(max(fx.p1.w, 0.0));
    let cell = max(size.y, 1.0) / 6.0;
    let n = (vnoise(seed, px / cell) * 2.0 + vnoise(seed + 1u, px / cell * 3.0)) / 3.0;
    let moment = along * (1.0 - rough) + n * rough;
    let dur = fx.p0.w;
    var f = select(-1.0, 2.0, fx.extra.y >= fx.p0.y);
    if (dur > 0.0) {
        f = (fx.extra.y - fx.p0.y) / dur;
    }
    if (f >= moment) {
        return vec4(0.0);
    }
    // Glowing just before the front reaches it.
    let w = max(fx.p1.z / 100.0, 1e-4);
    let g = smoothstep(moment - w, moment, f);
    let glow = rgb_of(fx.p1.y) * g * 1.3 * c.a;
    return vec4(c.rgb * (1.0 - g * 0.5) + glow, c.a);
}

// ---- Fractal noise ----
// p0 = (type, scale px, detail (octaves), roughness %), p1 = (contrast %,
// brightness %, evolution °, speed °/s), p2 = (drift x, drift y px/s, dark,
// light colour), p3 = (blend, opacity %, seed); extra = (-, time, layer size).

// A gradient at a lattice corner (one of 12 cube-edge directions).
fn grad3(seed: u32, c: vec3<f32>) -> vec3<f32> {
    let h = pcg(seed ^ pcg(ibits(c.x) ^ pcg(ibits(c.y) ^ pcg(ibits(c.z))))) % 12u;
    switch h {
        case 0u: { return vec3(1.0, 1.0, 0.0); }
        case 1u: { return vec3(-1.0, 1.0, 0.0); }
        case 2u: { return vec3(1.0, -1.0, 0.0); }
        case 3u: { return vec3(-1.0, -1.0, 0.0); }
        case 4u: { return vec3(1.0, 0.0, 1.0); }
        case 5u: { return vec3(-1.0, 0.0, 1.0); }
        case 6u: { return vec3(1.0, 0.0, -1.0); }
        case 7u: { return vec3(-1.0, 0.0, -1.0); }
        case 8u: { return vec3(0.0, 1.0, 1.0); }
        case 9u: { return vec3(0.0, -1.0, 1.0); }
        case 10u: { return vec3(0.0, 1.0, -1.0); }
        default: { return vec3(0.0, -1.0, -1.0); }
    }
}

// Gradient (Perlin) noise in 3D, about -1..1.
fn perlin3(seed: u32, p: vec3<f32>) -> f32 {
    let i = floor(p);
    let f = p - i;
    let u = f * f * f * (f * (f * 6.0 - 15.0) + 10.0);
    var n: array<f32, 8>;
    for (var k = 0u; k < 8u; k = k + 1u) {
        let o = vec3(f32(k & 1u), f32((k >> 1u) & 1u), f32((k >> 2u) & 1u));
        n[k] = dot(grad3(seed, i + o), f - o);
    }
    let x0 = mix(n[0], n[1], u.x);
    let x1 = mix(n[2], n[3], u.x);
    let x2 = mix(n[4], n[5], u.x);
    let x3 = mix(n[6], n[7], u.x);
    return mix(mix(x0, x1, u.y), mix(x2, x3, u.y), u.z) * 1.1;
}

// Octaves summed: style 0 plain, 1 turbulent (|n|), 2 ridged; 0..1.
fn fbm3(seed: u32, p0: vec3<f32>, octaves: f32, rough: f32, style: u32) -> f32 {
    var p = p0;
    var amp = 1.0;
    var sum = 0.0;
    var norm = 0.0;
    let whole = u32(clamp(floor(octaves), 1.0, 10.0));
    let part = clamp(octaves - f32(whole), 0.0, 1.0);
    for (var k = 0u; k <= whole; k = k + 1u) {
        // The last octave fades in with a fractional Detail.
        var w = amp;
        if (k == whole) {
            w = amp * part;
            if (w <= 0.0) {
                break;
            }
        }
        let n = perlin3(seed + k * 1013u, p);
        var v: f32;
        switch style {
            case 1u: { v = abs(n); }
            case 2u: { let r = 1.0 - abs(n); v = r * r; }
            default: { v = n * 0.5 + 0.5; }
        }
        sum += v * w;
        norm += w;
        amp *= rough;
        // Each octave twice as fine, turned a little so lattices don't line up.
        p = vec3(p.x * 1.6 - p.y * 1.2 + 17.3, p.x * 1.2 + p.y * 1.6 - 9.1, p.z * 1.5 + 3.7);
    }
    return sum / max(norm, 1e-5);
}

fn fractal_value(p: vec2<f32>, z: f32) -> f32 {
    let seed = seed_of(fx.p3.z) + 0x9e37u;
    let octaves = fx.p0.z;
    let rough = clamp(fx.p0.w / 100.0, 0.0, 1.0);
    let q = vec3(p, z);
    switch u32(round(fx.p0.x)) {
        // Turbulent: billowing, like smoke.
        case 1u: { return clamp(fbm3(seed, q, octaves, rough, 1u) * 1.6, 0.0, 1.0); }
        // Ridged: sharp crests, like mountains or veins.
        case 2u: { return fbm3(seed, q, octaves, rough, 2u); }
        // Clouds: soft puffs with clear sky between.
        case 3u: {
            let v = fbm3(seed, q, octaves, rough, 0u);
            return smoothstep(0.33, 0.72, v);
        }
        // Liquid: the noise pushed along by itself.
        case 4u: {
            let w = vec2(fbm3(seed + 7u, q, min(octaves, 4.0), rough, 0u), fbm3(seed + 11u, q + vec3(5.2, 1.3, 0.0), min(octaves, 4.0), rough, 0u));
            return clamp(0.5 + (fbm3(seed, vec3(p + (w - 0.5) * 4.0, z), octaves, rough, 0u) - 0.5) * 1.9, 0.0, 1.0);
        }
        // Marble: bands bent by the noise.
        case 5u: {
            let v = fbm3(seed, q, octaves, rough, 0u);
            return 0.5 + 0.5 * sin((p.x + p.y) * 14.0 + (v - 0.5) * 22.0);
        }
        // Smooth: stretched, as octaves summed bunch up round the middle.
        default: { return clamp(0.5 + (fbm3(seed, q, octaves, rough, 0u) - 0.5) * 1.9, 0.0, 1.0); }
    }
}

@fragment
fn fs_fractal(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    let ins = inside(in.uv);
    if (ins == 0.0) {
        return c;
    }
    let t = fx.extra.y;
    let size = fx.extra.zw;
    let scale = max(fx.p0.y, 1.0);
    let px = in.uv * size - fx.p2.xy * t;
    let z = (fx.p1.z + fx.p1.w * t) / 360.0;
    var v = fractal_value(px / scale, z);
    v = (v - 0.5) * fx.p1.x / 100.0 + 0.5 + fx.p1.y / 100.0;
    v = clamp(v, 0.0, 1.0);
    // Colours mixed as they look (display values).
    let dark = to_display(rgb_of(fx.p2.z));
    let light = to_display(rgb_of(fx.p2.w));
    let n = mix(dark, light, v);
    let base = to_display(unpremul(c));
    var out: vec4<f32>;
    switch u32(round(fx.p3.x)) {
        case 1u: { out = vec4(to_linear(1.0 - (1.0 - base) * (1.0 - n)) * c.a, c.a); }
        case 2u: { out = vec4(to_linear(base * n) * c.a, c.a); }
        case 3u: {
            let o = select(1.0 - 2.0 * (1.0 - base) * (1.0 - n), 2.0 * base * n, base < vec3(0.5));
            out = vec4(to_linear(o) * c.a, c.a);
        }
        case 4u: { out = vec4(to_linear(min(base + n, vec3(1.0))) * c.a, c.a); }
        // As transparency: the layer kept where the noise is light.
        case 5u: { out = c * v; }
        default: { out = vec4(to_linear(n), 1.0); }
    }
    return mix(c, out, clamp(fx.p3.y / 100.0, 0.0, 1.0));
}

// ---- Patch replacer ----
// p0 = (target x %, y %, source offset x %, y %), p1 = (width px, height px,
// rotation °, shape: 0 ellipse / 1 rectangle), p2 = (softness px, fill: 0
// adaptive / 1 clone / 2 from the edges, show areas, opacity %); extra.zw = layer size.

fn patch_rot(v: vec2<f32>, a: f32) -> vec2<f32> {
    let c = cos(a);
    let s = sin(a);
    return vec2(c * v.x - s * v.y, s * v.x + c * v.y);
}

// Signed distance (px) from the shape's edge, negative inside; `q` is in the shape's own frame.
fn patch_sd(q: vec2<f32>, half: vec2<f32>) -> f32 {
    if (fx.p1.w > 0.5) {
        let d = abs(q) - half;
        return length(max(d, vec2(0.0))) + min(max(d.x, d.y), 0.0);
    }
    // An ellipse: scaled so the distance is about right near the edge.
    let k = length(q / half);
    return (k - 1.0) * min(half.x, half.y);
}

// A point just outside the shape's edge at turn `f` (0–1), in the shape's own frame.
fn patch_rim(f: f32, half: vec2<f32>, out: f32) -> vec2<f32> {
    if (fx.p1.w > 0.5) {
        // Round the rectangle's perimeter.
        let h = half + vec2(out);
        let per = 4.0 * (h.x + h.y);
        var t = f * per;
        if (t < 2.0 * h.x) { return vec2(-h.x + t, -h.y); }
        t -= 2.0 * h.x;
        if (t < 2.0 * h.y) { return vec2(h.x, -h.y + t); }
        t -= 2.0 * h.y;
        if (t < 2.0 * h.x) { return vec2(h.x - t, h.y); }
        t -= 2.0 * h.x;
        return vec2(-h.x, h.y - t);
    }
    let a = f * 6.28318530718;
    return vec2(cos(a), sin(a)) * (half + vec2(out));
}

fn patch_at(p: vec2<f32>, size: vec2<f32>) -> vec4<f32> {
    return textureSampleLevel(src, src_sampler, to_tex(p / size), 0.0);
}

@fragment
fn fs_patch(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    let size = fx.extra.zw;
    let p = in.uv * size;
    let centre = fx.p0.xy / 100.0 * size;
    let off = fx.p0.zw / 100.0 * size;
    let half = max(fx.p1.xy * 0.5, vec2(1.0));
    let rot = radians(fx.p1.z);
    let soft = max(fx.p2.x, 0.0);
    let q = patch_rot(p - centre, -rot);
    let d = patch_sd(q, half);
    // Full inside, feathered over `soft` just inside the edge.
    let m = 1.0 - smoothstep(-soft - 0.5, 0.5, d);
    var out = c;
    if (m > 0.0) {
        let taken = patch_at(p + off, size);
        let fill = u32(round(fx.p2.y));
        var res = taken;
        if (fill != 1u) {
            // Round the rim, just outside the feather: what's there, and what's
            // round the source; weighted towards the nearest parts of the rim.
            var sum_w = 0.0;
            var sum_diff = vec4(0.0);
            var sum_edge = vec4(0.0);
            for (var k = 0u; k < 48u; k = k + 1u) {
                let r = patch_rim((f32(k) + 0.5) / 48.0, half, 2.0);
                let b = centre + patch_rot(r, rot);
                let here = patch_at(b, size);
                let dq = q - r;
                let w = 1.0 / (dot(dq, dq) + 1.0);
                sum_w += w;
                sum_edge += here * w;
                sum_diff += (here - patch_at(b + off, size)) * w;
            }
            if (fill == 2u) {
                res = sum_edge / sum_w;
            } else {
                res = max(taken + sum_diff / sum_w, vec4(0.0));
                res = vec4(min(res.rgb, vec3(max(res.a, 1e-4)) * 64.0), clamp(res.a, 0.0, 1.0));
            }
        }
        out = mix(c, res, m * clamp(fx.p2.w / 100.0, 0.0, 1.0));
    }
    if (fx.p2.z > 0.5) {
        // Outlines: the target red, the source green.
        let ds = patch_sd(patch_rot(p - centre - off, -rot), half);
        let line_t = 1.0 - smoothstep(0.75, 1.75, abs(d));
        let line_s = 1.0 - smoothstep(0.75, 1.75, abs(ds));
        out = mix(out, vec4(0.9, 0.08, 0.05, 1.0), line_t);
        out = mix(out, vec4(0.1, 0.85, 0.15, 1.0), line_s * (1.0 - line_t));
    }
    return out;
}

// ---- Distort: Bulge, Twirl, Ripple, Wave warp, Polar coordinates, Mirror, Lens distortion ----

// Where a distortion reads from past the layer: 0 transparent, 1 stretch
// (the edge pixels), 2 mirror, 3 repeat.
fn d_edge(uv: vec2<f32>, edges: u32) -> vec2<f32> {
    if (edges == 2u) {
        return 1.0 - abs(fract(uv * 0.5) * 2.0 - 1.0);
    }
    if (edges == 3u) {
        return fract(uv);
    }
    return clamp(uv, vec2(0.0), vec2(1.0));
}
fn d_sample(uv: vec2<f32>, edges: u32) -> vec4<f32> {
    if (edges == 0u && (any(uv < vec2(-1e-4)) || any(uv > vec2(1.0 + 1e-4)))) {
        return vec4(0.0);
    }
    return textureSampleLevel(src, src_sampler, to_tex(d_edge(uv, edges)), 0.0);
}

// Bulge: p0 = (centre x %, y %, radius % of height, amount %), p1 = (shape, width %).
@fragment
fn fs_bulge(in: VsOut) -> @location(0) vec4<f32> {
    let size = fx.extra.zw;
    let c = fx.p0.xy / 100.0 * size;
    let rad = max(fx.p0.z / 100.0 * size.y, 1e-3);
    let w = max(fx.p1.y / 100.0, 0.01);
    var d = in.uv * size - c;
    d.x = d.x / w;
    let r = length(d) / rad;
    if (r >= 1.0) {
        return d_sample(in.uv, 1u);
    }
    let a = clamp(fx.p0.w / 100.0, -1.0, 1.0);
    var f: f32;
    if (u32(round(fx.p1.x)) == 1u) {
        // Wrapped on a ball: the middle magnified (or, below 0, the inside of one).
        let rs = mix(r, select(sin(r * 1.5707964), asin(r) / 1.5707964, a > 0.0), abs(a));
        f = rs / max(r, 1e-5);
    } else {
        let t = 1.0 - r * r;
        f = 1.0 - a * 0.9 * t * t;
    }
    var s = d * f;
    s.x = s.x * w;
    return d_sample((c + s) / size, 1u);
}

// Twirl: p0 = (centre x %, y %, radius % of height, angle °), p1.x = falloff (smooth, even).
@fragment
fn fs_twirl(in: VsOut) -> @location(0) vec4<f32> {
    let size = fx.extra.zw;
    let c = fx.p0.xy / 100.0 * size;
    let rad = max(fx.p0.z / 100.0 * size.y, 1e-3);
    let d = in.uv * size - c;
    let r = length(d) / rad;
    if (r >= 1.0) {
        return d_sample(in.uv, 1u);
    }
    let k = select(1.0 - smoothstep(0.0, 1.0, r), 1.0 - r, u32(round(fx.p1.x)) == 1u);
    let a = -radians(fx.p0.w) * k;
    let cs = cos(a);
    let sn = sin(a);
    let s = vec2(d.x * cs - d.y * sn, d.x * sn + d.y * cs);
    return d_sample((c + s) / size, 1u);
}

// Ripple: p0 = (centre x %, y %, height px, spacing px), p1 = (speed rings/s,
// phase °, fade out at % of height, push), p2.x = edges.
@fragment
fn fs_ripple(in: VsOut) -> @location(0) vec4<f32> {
    let size = fx.extra.zw;
    let c = fx.p0.xy / 100.0 * size;
    let p = in.uv * size;
    let d = p - c;
    let dist = length(d);
    let dir = select(vec2(0.0), d / max(dist, 1e-4), dist > 1e-4);
    let spacing = max(fx.p0.w, 1.0);
    let ph = 6.2831853 * (dist / spacing - fx.p1.x * fx.extra.y) + radians(fx.p1.y);
    // Calm at the very middle; fading out towards "Fade out at" when set.
    var amp = fx.p0.z * smoothstep(0.0, spacing * 0.5, dist);
    let fade = fx.p1.z / 100.0 * size.y;
    if (fade > 0.0) {
        amp = amp * (1.0 - smoothstep(0.0, 1.0, dist / fade));
    }
    let v = select(dir, vec2(-dir.y, dir.x), u32(round(fx.p1.w)) == 1u);
    return d_sample((p + v * sin(ph) * amp) / size, u32(round(fx.p2.x)));
}

// Wave warp: p0 = (shape, height px, width px, direction °), p1 = (speed
// waves/s, phase °, pin, edges).
@fragment
fn fs_wave(in: VsOut) -> @location(0) vec4<f32> {
    let size = fx.extra.zw;
    let p = in.uv * size;
    let a = radians(fx.p0.w);
    // Pushed along D; the waves run across it (along P).
    let dv = vec2(cos(a), sin(a));
    let pv = vec2(-dv.y, dv.x);
    let u = dot(p, pv) / max(fx.p0.z, 1.0) - fx.p1.x * fx.extra.y + fx.p1.y / 360.0;
    var w: f32;
    switch (u32(round(fx.p0.x))) {
        case 1u: { w = clamp(sin(6.2831853 * u) * 12.0, -1.0, 1.0); }
        case 2u: { w = 4.0 * abs(fract(u - 0.25) - 0.5) - 1.0; }
        case 3u: { w = 2.0 * fract(u) - 1.0; }
        case 4u: { w = vnoise(7u, vec2(u * 2.0, 0.37)) * 2.0 - 1.0; }
        default: { w = sin(6.2831853 * u); }
    }
    // Pinned edges stay put.
    var g = 1.0;
    let ex = min(in.uv.x, 1.0 - in.uv.x);
    let ey = min(in.uv.y, 1.0 - in.uv.y);
    switch (u32(round(fx.p1.z))) {
        case 1u: { g = smoothstep(0.0, 0.15, min(ex, ey)); }
        case 2u: { g = smoothstep(0.0, 0.15, ex); }
        case 3u: { g = smoothstep(0.0, 0.15, ey); }
        default: {}
    }
    return d_sample((p + dv * w * fx.p0.y * g) / size, u32(round(fx.p1.w)));
}

// Polar coordinates: p0 = (amount %, conversion: 0 rectangular to polar, 1 polar to rectangular).
@fragment
fn fs_polar(in: VsOut) -> @location(0) vec4<f32> {
    var s: vec2<f32>;
    if (u32(round(fx.p0.y)) == 0u) {
        // Round into a circle: the top edge outside, the bottom in the middle.
        let d = in.uv - 0.5;
        let r = length(d) / 0.5;
        let th = atan2(d.x, -d.y);
        s = vec2(th / 6.2831853 + 0.5, 1.0 - r);
    } else {
        // A circle unrolled: its top straight up the middle of the strip.
        let th = (in.uv.x - 0.5) * 6.2831853;
        let r = (1.0 - in.uv.y) * 0.5;
        s = 0.5 + r * vec2(sin(th), -cos(th));
    }
    return d_sample(mix(in.uv, s, clamp(fx.p0.x / 100.0, 0.0, 1.0)), 0u);
}

// Mirror: p0 = (centre x %, y %, angle °): the side the line's normal points to shows the other.
@fragment
fn fs_mirror(in: VsOut) -> @location(0) vec4<f32> {
    let size = fx.extra.zw;
    let c = fx.p0.xy / 100.0 * size;
    let a = radians(fx.p0.z);
    let n = vec2(cos(a), -sin(a));
    let p = in.uv * size;
    let k = dot(p - c, n);
    let q = select(p, p - 2.0 * k * n, k > 0.0);
    return d_sample(q / size, 1u);
}

// Lens distortion: p0 = (centre x %, y %, distortion %, cubic %), p1 = (zoom %, edges).
@fragment
fn fs_lens(in: VsOut) -> @location(0) vec4<f32> {
    let size = fx.extra.zw;
    let c = fx.p0.xy / 100.0 * size;
    let d = in.uv * size - c;
    let q = d / (0.5 * length(size));
    let r2 = dot(q, q);
    let f = 1.0 + fx.p0.z / 100.0 * 0.5 * r2 + fx.p0.w / 100.0 * 0.5 * r2 * r2;
    let s = c + d * f / max(fx.p1.x / 100.0, 0.01);
    return d_sample(s / size, u32(round(fx.p1.y)));
}

// ---- Film look, Film damage, Analogue damage, Abstraction, Prism blur, Tilt-shift, Scanlines ----

fn s_at(uv: vec2<f32>) -> vec4<f32> {
    return textureSampleLevel(src, src_sampler, to_tex(clamp(uv, vec2(0.0), vec2(1.0))), 0.0);
}

// Film look: p0 = (stock, strength %, contrast %, lift blacks %), p1 =
// (roll-off %, saturation %, halation %, halation px), p2 = (bloom %, grain %,
// grain px, vignette %), p3 = (gate weave px, seed).
@fragment
fn fs_film_look(in: VsOut) -> @location(0) vec4<f32> {
    let size = fx.extra.zw;
    let t = fx.extra.y;
    let seed = seed_of(fx.p3.y);
    // Gate weave: the frame drifting a little in the gate, 24 times a second.
    let fr = floor(t * 24.0);
    let weave = (vec2(vnoise(seed + 3u, vec2(fr * 0.15, 0.0)), vnoise(seed + 9u, vec2(fr * 0.15, 5.0))) - 0.5) * 2.0 * fx.p3.x;
    let uv = in.uv + weave / size;
    let c0 = s_at(uv);
    if (c0.a <= 1e-5) {
        return c0;
    }
    let src_d = to_display(c0.rgb / c0.a);
    var d = src_d;
    // Halation and bloom: what's bright round about (a ring of taps).
    let hr = max(fx.p1.w, 1.0) / size;
    var glow = vec3(0.0);
    for (var i = 0; i < 12; i++) {
        let a = f32(i) * 0.5235988;
        for (var k = 1; k <= 2; k++) {
            let o = vec2(cos(a), sin(a)) * hr * f32(k) * 0.5;
            let s = s_at(uv + o);
            let sd = to_display(unpremul(s));
            glow += max(sd - 0.7, vec3(0.0)) / 0.3;
        }
    }
    glow /= 24.0;
    // Stock: colour and contrast.
    let stock = u32(round(fx.p0.x));
    let l = dot(d, LUMA);
    switch (stock) {
        case 1u: { d = d * vec3(1.06, 1.0, 0.88) + vec3(0.02, 0.01, 0.0); }
        case 2u: { d = d * vec3(0.92, 1.0, 1.08) + vec3(0.0, 0.005, 0.02); }
        case 3u: { d = mix(d, vec3(l), 0.55); }
        case 4u: {
            let warm = vec3(1.08, 0.98, 0.84);
            let teal = vec3(0.82, 1.0, 1.08);
            d = d * mix(teal, warm, smoothstep(0.2, 0.75, l));
        }
        case 5u: { d = mix(d, vec3(l), 0.25) * vec3(1.04, 1.0, 0.9) + vec3(0.04, 0.03, 0.02); }
        case 6u: { d = vec3(dot(d, vec3(0.3, 0.59, 0.11))); }
        default: {}
    }
    // Contrast as an S-curve about the middle; bleach bypass adds more.
    let con = fx.p0.z / 100.0 + select(0.0, 0.35, stock == 3u);
    let sc = d * d * (3.0 - 2.0 * d);
    d = select(mix(d, sc, clamp(con, 0.0, 1.0)), mix(d, vec3(0.5) + (d - 0.5) * 0.7, clamp(-con, 0.0, 1.0)), con < 0.0);
    // Saturation.
    let ld = dot(d, LUMA);
    d = mix(vec3(ld), d, fx.p1.y / 100.0);
    // Highlights rolling off softly instead of clipping; blacks lifted.
    let ro = fx.p1.x / 100.0;
    let knee = 1.0 - 0.45 * ro;
    let over = max(d - knee, vec3(0.0));
    d = min(d, vec3(knee)) + over / (1.0 + over / max(1.0 - knee, 1e-3));
    let lift = fx.p0.w / 100.0 + select(0.0, 0.06, stock == 5u);
    d = lift + d * (1.0 - lift);
    // Halation (red-orange) and bloom (its own colour).
    d += glow * vec3(1.0, 0.35, 0.12) * fx.p1.z / 100.0 * 0.6;
    d += glow * fx.p2.x / 100.0 * 0.35;
    // Vignette.
    let r = length((in.uv - 0.5) * vec2(size.x / size.y, 1.0));
    d *= 1.0 - fx.p2.w / 100.0 * smoothstep(0.35, 1.0, r) * 0.7;
    // Grain.
    if (fx.p2.y > 0.0) {
        let g = in.uv * size / max(fx.p2.z, 0.25);
        let gs = seed + ibits(fr) * 2654435761u;
        let n = (vnoise(gs, g) + vnoise(gs + 5u, g * 2.1 + 3.7) * 0.6 - 0.8) / 0.8;
        let w = 0.45 + 0.55 * smoothstep(0.0, 0.25, ld) * (1.0 - 0.6 * smoothstep(0.6, 1.0, ld));
        d += vec3(n) * fx.p2.y / 100.0 * 0.12 * w;
    }
    d = mix(src_d, d, clamp(fx.p0.y / 100.0, 0.0, 1.0));
    return vec4(to_linear(max(d, vec3(0.0))) * c0.a, c0.a);
}

// Film damage: p0 = (scratches %, scratch px, dust %, dust px), p1 = (hairs %,
// flicker %, jitter px, sepia %), p2 = (vignette %, frame rate, seed).
@fragment
fn fs_film_damage(in: VsOut) -> @location(0) vec4<f32> {
    let size = fx.extra.zw;
    let fr = floor(fx.extra.y * max(fx.p2.y, 1.0) + 1e-3);
    let seed = seed_of(fx.p2.z);
    let fs = seed + ibits(fr) * 2654435761u;
    // The frame jittering in the projector.
    let jit = (vec2(rnd3(fs, 1u, 2u), rnd3(fs, 3u, 4u)) - 0.5) * 2.0 * fx.p1.z;
    let uv = in.uv + jit / size;
    let c0 = s_at(uv);
    if (c0.a <= 1e-5) {
        return c0;
    }
    var d = to_display(c0.rgb / c0.a);
    let p = uv * size;
    // Scratches: a few lines down the frame, each lasting several frames and wandering.
    var dark = 0.0;
    var light = 0.0;
    let ns = i32(round(fx.p0.x / 100.0 * 6.0));
    for (var i = 0; i < 6; i++) {
        if (i >= ns) {
            break;
        }
        let life = floor(fr / (3.0 + f32(i)));
        let ls = seed + u32(i) * 977u + ibits(life) * 7919u;
        if (rnd3(ls, 11u, 0u) < 0.35) {
            continue;
        }
        let x = rnd3(ls, 12u, 0u) * size.x + (vnoise(ls, vec2(p.y / size.y * 3.0, fr * 0.3)) - 0.5) * 12.0;
        let w = fx.p0.y * (0.6 + rnd3(ls, 13u, 0u));
        let k = 1.0 - smoothstep(w * 0.5, w * 0.5 + 1.0, abs(p.x - x));
        // Broken up along its length.
        let along = smoothstep(0.3, 0.5, vnoise(ls + 1u, vec2(p.y / 40.0, 0.0)));
        if (rnd3(ls, 14u, 0u) < 0.5) {
            dark = max(dark, k * along);
        } else {
            light = max(light, k * along);
        }
    }
    // Dust: specks in a grid of cells, a new set every frame.
    if (fx.p0.z > 0.0) {
        let cell = max(fx.p0.w * 18.0, 16.0);
        let ci = floor(p / cell);
        for (var oy = -1; oy <= 1; oy++) {
            for (var ox = -1; ox <= 1; ox++) {
                let cc = ci + vec2(f32(ox), f32(oy));
                let cs = fs ^ (ibits(cc.x) * 73856093u) ^ (ibits(cc.y) * 19349663u);
                if (rnd3(cs, 21u, 0u) > fx.p0.z / 100.0 * 0.35) {
                    continue;
                }
                let at = (cc + vec2(rnd3(cs, 22u, 0u), rnd3(cs, 23u, 0u))) * cell;
                let rr = fx.p0.w * (0.4 + rnd3(cs, 24u, 0u));
                // A ragged blob.
                let q = p - at;
                let rag = 1.0 + (vnoise(cs, q / max(rr, 0.5) * 1.5) - 0.5) * 0.8;
                let k = 1.0 - smoothstep(rr * rag, rr * rag + 1.0, length(q));
                if (rnd3(cs, 25u, 0u) < 0.7) {
                    dark = max(dark, k);
                } else {
                    light = max(light, k);
                }
            }
        }
    }
    // A stray hair now and then: a thin curling line.
    if (fx.p1.x > 0.0) {
        let hs = seed + ibits(floor(fr / 6.0)) * 31337u;
        if (rnd3(hs, 31u, 0u) < fx.p1.x / 100.0) {
            let o = vec2(rnd3(hs, 32u, 0u), rnd3(hs, 33u, 0u)) * size;
            let q = (p - o) / (size.y * 0.15);
            let curve = q.y - 0.6 * sin(q.x * 2.2 + rnd3(hs, 34u, 0u) * 6.0) * smoothstep(1.6, 0.0, abs(q.x));
            let k = (1.0 - smoothstep(0.004, 0.012, abs(curve))) * (1.0 - smoothstep(1.2, 1.6, abs(q.x)));
            dark = max(dark, k);
        }
    }
    d = d * (1.0 - 0.8 * dark) + light * 0.6 * (1.0 - d);
    // Flicker: the whole frame a little brighter or darker each frame.
    d *= 1.0 + (rnd3(fs, 41u, 0u) - 0.5) * 0.5 * fx.p1.y / 100.0;
    // Sepia.
    let l = dot(d, LUMA);
    d = mix(d, l * vec3(1.07, 0.92, 0.72), fx.p1.w / 100.0);
    // Vignette, uneven like an old lens.
    let r = length((in.uv - 0.5) * vec2(size.x / size.y, 1.0));
    d *= 1.0 - fx.p2.x / 100.0 * smoothstep(0.3, 0.95, r + (vnoise(seed, in.uv * 3.0) - 0.5) * 0.1) * 0.8;
    return vec4(to_linear(clamp(d, vec3(0.0), vec3(1.0))) * c0.a, c0.a);
}

// Analogue damage: p0 = (amount %, colour bleed px, colour shift px, tracking
// lines %), p1 = (head switching %, wobble px, noise %, dropouts %), p2 =
// (saturation %, softness px, speed, seed).
@fragment
fn fs_analogue(in: VsOut) -> @location(0) vec4<f32> {
    let size = fx.extra.zw;
    let amt = clamp(fx.p0.x / 100.0, 0.0, 1.0);
    let t = fx.extra.y * max(fx.p2.z, 0.0);
    let seed = seed_of(fx.p2.w);
    let fr = floor(fx.extra.y * 30.0);
    let p = in.uv * size;
    let line = floor(p.y);
    // Each line wobbling sideways; the bottom lines torn by head switching.
    var dx = (vnoise(seed, vec2(line * 0.05, t * 4.0)) - 0.5) * 2.0 * fx.p1.y * amt;
    let head = smoothstep(0.93, 1.0, in.uv.y) * fx.p1.x / 100.0 * amt;
    dx += head * (20.0 + 40.0 * vnoise(seed + 7u, vec2(line * 0.3, fr))) ;
    // Tracking lines: a band of noise rolling up the picture.
    let band_y = fract(t * 0.08 + rnd3(seed, 5u, 0u));
    let band = (1.0 - smoothstep(0.0, 0.035, abs(in.uv.y - band_y))) * fx.p0.w / 100.0 * amt;
    dx += band * (vnoise(seed + 3u, vec2(p.x * 0.05, line + fr)) - 0.5) * 30.0;
    let uv = in.uv + vec2(dx, 0.0) / size;
    // Soft along the lines (a few taps sideways), colour bleeding further than brightness.
    let soft = max(fx.p2.y * amt, 0.0) / size.x;
    let bleed = max(fx.p0.y * amt, 0.0) / size.x;
    var y = 0.0;
    var chroma = vec3(0.0);
    var a = 0.0;
    for (var i = -3; i <= 3; i++) {
        let f = f32(i) / 3.0;
        let sy = s_at(uv + vec2(f * soft, 0.0));
        y += dot(to_display(unpremul(sy)), LUMA) / 7.0;
        let sc = s_at(uv + vec2(f * bleed + fx.p0.z * amt / size.x, 0.0));
        let dc = to_display(unpremul(sc));
        chroma += (dc - dot(dc, LUMA)) / 7.0;
        a += sy.a / 7.0;
    }
    if (a <= 1e-5) {
        return vec4(0.0);
    }
    var d = vec3(y) + chroma * mix(1.0, fx.p2.x / 100.0, amt);
    // Tape noise, more in the tracking band; dropouts as short white streaks.
    let n = rnd3(seed + ibits(fr) * 2654435761u, ibits(p.x * 0.5), ibits(line)) - 0.5;
    d += n * (fx.p1.z / 100.0 * 0.25 + band * 0.6) * amt;
    let ds = seed + ibits(fr) * 97u + ibits(line) * 131u;
    if (rnd3(ds, 61u, 0u) < fx.p1.w / 100.0 * 0.02 * amt) {
        let x0 = rnd3(ds, 62u, 0u) * size.x;
        let len = 20.0 + rnd3(ds, 63u, 0u) * 120.0;
        if (p.x > x0 && p.x < x0 + len) {
            d = mix(d, vec3(0.95), 0.85);
        }
    }
    d = mix(to_display(unpremul(s_at(in.uv))), d, amt);
    return vec4(to_linear(clamp(d, vec3(0.0), vec3(1.0))) * a, a);
}

// Abstraction: p0 = (brush size px, outlines %, outline px, colour levels), p1.x = saturation %.
@fragment
fn fs_abstraction(in: VsOut) -> @location(0) vec4<f32> {
    let size = fx.extra.zw;
    let c0 = s_at(in.uv);
    if (c0.a <= 1e-5) {
        return c0;
    }
    let r = max(fx.p0.x, 0.0);
    var d = to_display(c0.rgb / c0.a);
    if (r >= 0.5) {
        // Kuwahara: of the four squares round the pixel, the flattest one's mean.
        let st = r / 3.0 / size;
        var best = d;
        var low = 1e9;
        for (var q = 0; q < 4; q++) {
            let sx = select(-1.0, 1.0, q == 1 || q == 3);
            let sy = select(-1.0, 1.0, q >= 2);
            var m = vec3(0.0);
            var m2 = vec3(0.0);
            for (var j = 0; j <= 3; j++) {
                for (var i = 0; i <= 3; i++) {
                    let s = to_display(unpremul(s_at(in.uv + vec2(sx * f32(i), sy * f32(j)) * st)));
                    m += s;
                    m2 += s * s;
                }
            }
            m /= 16.0;
            let v = dot(m2 / 16.0 - m * m, vec3(1.0));
            if (v < low) {
                low = v;
                best = m;
            }
        }
        d = best;
    }
    // Fewer colours.
    let lv = round(fx.p0.w);
    if (lv >= 2.0) {
        d = floor(d * lv + 0.5) / lv;
    }
    let l = dot(d, LUMA);
    d = mix(vec3(l), d, fx.p1.x / 100.0);
    // Dark outlines where the brightness changes sharply.
    if (fx.p0.y > 0.0) {
        let e = max(fx.p0.z, 0.5) / size;
        let gx = dot(to_display(unpremul(s_at(in.uv + vec2(e.x, 0.0)))) - to_display(unpremul(s_at(in.uv - vec2(e.x, 0.0)))), LUMA);
        let gy = dot(to_display(unpremul(s_at(in.uv + vec2(0.0, e.y)))) - to_display(unpremul(s_at(in.uv - vec2(0.0, e.y)))), LUMA);
        let edge = smoothstep(0.08, 0.3, length(vec2(gx, gy)));
        d *= 1.0 - edge * fx.p0.y / 100.0;
    }
    return vec4(to_linear(clamp(d, vec3(0.0), vec3(1.0))) * c0.a, c0.a);
}

// Prism blur: p0 = (amount px, direction: 0 out from the centre, 1 one way,
// angle °, centre x %), p1 = (centre y %, colour spread %).
@fragment
fn fs_prism(in: VsOut) -> @location(0) vec4<f32> {
    let size = fx.extra.zw;
    let p = in.uv * size;
    var dir: vec2<f32>;
    var reach = fx.p0.x;
    if (u32(round(fx.p0.y)) == 0u) {
        let c = vec2(fx.p0.w, fx.p1.x) / 100.0 * size;
        let d = p - c;
        dir = select(vec2(0.0), normalize(d), length(d) > 1e-3);
        // More the further out.
        reach = fx.p0.x * min(length(d) / (0.5 * length(size)), 1.5);
    } else {
        let a = radians(fx.p0.z);
        dir = vec2(cos(a), sin(a));
    }
    let spread = fx.p1.y / 100.0;
    var acc = vec3(0.0);
    var wsum = vec3(0.0);
    var a_acc = 0.0;
    for (var i = 0; i < 16; i++) {
        let f = f32(i) / 15.0 * 2.0 - 1.0;
        let s = s_at((p + dir * reach * f) / size);
        // Red at one end, green in the middle, blue at the other (a spectrum).
        let w = mix(vec3(1.0), clamp(vec3(1.0 - abs(f + 0.66) * 1.5, 1.0 - abs(f) * 1.5, 1.0 - abs(f - 0.66) * 1.5), vec3(0.0), vec3(1.0)), clamp(spread, 0.0, 1.0)) + 0.02;
        acc += s.rgb * w;
        wsum += w;
        a_acc += s.a;
    }
    return vec4(acc / wsum, a_acc / 16.0);
}

// Tilt-shift: p0 = (focus at %, angle °, sharp band %, falloff %), p1 = (blur px, saturation %, contrast %).
@fragment
fn fs_tilt_shift(in: VsOut) -> @location(0) vec4<f32> {
    let size = fx.extra.zw;
    let a = radians(fx.p0.y);
    let n = vec2(-sin(a), cos(a));
    let c = vec2(0.5, fx.p0.x / 100.0) * size;
    let dist = abs(dot(in.uv * size - c, n)) / size.y;
    let band = fx.p0.z / 200.0;
    let k = smoothstep(band, band + max(fx.p0.w / 100.0, 0.01), dist);
    let r = fx.p1.x * k;
    var acc = vec4(0.0);
    if (r < 0.5) {
        acc = s_at(in.uv);
    } else {
        // A disc of taps (a golden-angle spiral), like a lens's blur.
        for (var i = 0; i < 32; i++) {
            let f = (f32(i) + 0.5) / 32.0;
            let ang = f32(i) * 2.3999632;
            let o = vec2(cos(ang), sin(ang)) * sqrt(f) * r / size;
            acc += s_at(in.uv + o);
        }
        acc /= 32.0;
    }
    if (acc.a <= 1e-5) {
        return acc;
    }
    var d = to_display(acc.rgb / acc.a);
    d = vec3(0.5) + (d - 0.5) * fx.p1.z / 100.0;
    let l = dot(d, LUMA);
    d = mix(vec3(l), d, fx.p1.y / 100.0);
    return vec4(to_linear(clamp(d, vec3(0.0), vec3(1.0))) * acc.a, acc.a);
}

// Scanlines: p0 = (amount %, spacing px, thickness %, softness %), p1 = (angle °, roll px/s, kind, brighten %).
@fragment
fn fs_scanlines(in: VsOut) -> @location(0) vec4<f32> {
    let size = fx.extra.zw;
    let c0 = s_at(in.uv);
    if (c0.a <= 1e-5) {
        return c0;
    }
    let a = radians(fx.p1.x);
    let n = vec2(-sin(a), cos(a));
    let sp = max(fx.p0.y, 1.0);
    let v = dot(in.uv * size, n) + fx.p1.y * fx.extra.y;
    let f = fract(v / sp);
    let th = clamp(fx.p0.z / 100.0, 0.01, 1.0);
    let soft = max(fx.p0.w / 100.0 * 0.5, 0.5 / sp);
    // 1 on a line, 0 between.
    let on = 1.0 - smoothstep(th * 0.5, th * 0.5 + soft, abs(f - 0.5));
    let amt = clamp(fx.p0.x / 100.0, 0.0, 1.0);
    var d = to_display(c0.rgb / c0.a) * (1.0 + fx.p1.w / 100.0 * amt);
    if (u32(round(fx.p1.z)) == 1u) {
        // Red, green and blue stripes in turn.
        let k = u32(floor(v / sp)) % 3u;
        let m = select(select(vec3(0.0, 0.0, 1.0), vec3(0.0, 1.0, 0.0), k == 1u), vec3(1.0, 0.0, 0.0), k == 0u);
        d = mix(d, d * mix(vec3(0.35), vec3(1.0), m) * 1.6, amt);
    } else {
        d *= 1.0 - on * amt;
    }
    return vec4(to_linear(clamp(d, vec3(0.0), vec3(1.0))) * c0.a, c0.a);
}

// ---- Turbulent displace, heat haze, shockwave ----

// Value noise in 3D (the third coordinate: evolution over time), 0..1.
fn vnoise3(seed: u32, p: vec3<f32>) -> f32 {
    let i = floor(p);
    let f = p - i;
    let u = f * f * (3.0 - 2.0 * f);
    let z0 = pcg(seed ^ ibits(i.z));
    let z1 = pcg(seed ^ ibits(i.z + 1.0));
    let a = mix(mix(rnd3(z0, ibits(i.x), ibits(i.y)), rnd3(z0, ibits(i.x + 1.0), ibits(i.y)), u.x), mix(rnd3(z0, ibits(i.x), ibits(i.y + 1.0)), rnd3(z0, ibits(i.x + 1.0), ibits(i.y + 1.0)), u.x), u.y);
    let b = mix(mix(rnd3(z1, ibits(i.x), ibits(i.y)), rnd3(z1, ibits(i.x + 1.0), ibits(i.y)), u.x), mix(rnd3(z1, ibits(i.x), ibits(i.y + 1.0)), rnd3(z1, ibits(i.x + 1.0), ibits(i.y + 1.0)), u.x), u.y);
    return mix(a, b, u.z);
}

// Octaves of it (up to 6, the last partly), about -1..1.
fn tfbm(seed: u32, p: vec3<f32>, octaves: f32) -> f32 {
    var s = 0.0;
    var amp = 0.5;
    var norm = 0.0;
    var q = p;
    let n = clamp(octaves, 1.0, 6.0);
    for (var k = 0; k < 6; k++) {
        let w = clamp(n - f32(k), 0.0, 1.0);
        if (w <= 0.0) {
            break;
        }
        s += (vnoise3(seed + u32(k) * 7919u, q) * 2.0 - 1.0) * amp * w;
        norm += amp * w;
        amp *= 0.5;
        q = vec3(q.xy * 2.03 + vec2(17.1, 9.7), q.z * 1.7);
    }
    return s / max(norm, 1e-4) * 1.6;
}

// How much of a pinned distortion is left this close to the edge (uv).
fn d_pin(uv: vec2<f32>) -> f32 {
    let e = min(min(uv.x, 1.0 - uv.x), min(uv.y, 1.0 - uv.y));
    return smoothstep(0.0, 0.12, e);
}

// Turbulent displace: p0 = (amount px, size px, complexity, evolution °),
// p1 = (evolution turns/s, offset x px, offset y px, type), p2 = (pin, edges).
@fragment
fn fs_turbulent(in: VsOut) -> @location(0) vec4<f32> {
    let size = fx.extra.zw;
    let p = in.uv * size;
    let scale = max(fx.p0.y, 1.0);
    let q = (p + fx.p1.yz) / scale;
    let evo = fx.p0.w / 360.0 + fx.p1.x * fx.extra.y;
    let kind = u32(round(fx.p1.w));
    // Smooth: just the broad swirls.
    let octaves = select(fx.p0.z, 1.0, kind == 1u);
    var d = vec2(tfbm(11u, vec3(q, evo), octaves), tfbm(23u, vec3(q + vec2(5.2, 1.3), evo + 0.37), octaves));
    if (kind == 2u) {
        d.y = 0.0;
    }
    if (kind == 3u) {
        d.x = 0.0;
    }
    var amount = fx.p0.x;
    if (u32(round(fx.p2.x)) == 1u) {
        amount *= d_pin(in.uv);
    }
    return d_sample((p + d * amount) / size, u32(round(fx.p2.y)));
}

// Heat haze: p0 = (strength px, size px, rise px/s, shimmer /s), p1 = (area,
// height %, centre x %, centre y %), p2 = (radius % of height, rises towards °, edges).
@fragment
fn fs_heat_haze(in: VsOut) -> @location(0) vec4<f32> {
    let size = fx.extra.zw;
    let p = in.uv * size;
    let t = fx.extra.y;
    let a = radians(fx.p2.y);
    // Up the screen is −y.
    let up = vec2(cos(a), -sin(a));
    let scale = max(fx.p0.y, 1.0);
    // Little cells of hot air, stretched along the way they rise, carried along it.
    let along = dot(p, up);
    let across = dot(p, vec2(-up.y, up.x));
    let q = vec2(across / scale, (along - fx.p0.z * t) / (scale * 2.2));
    let z = fx.p0.w * t;
    let d = vec2(tfbm(31u, vec3(q, z), 3.0), tfbm(47u, vec3(q + vec2(3.7, 8.1), z + 0.5), 3.0) * 0.5);
    // Mostly sideways wobble (across the way it rises).
    let push = vec2(-up.y, up.x) * d.x + up * d.y;
    var k = 1.0;
    switch (u32(round(fx.p1.x))) {
        case 1u: {
            // Strongest at the bottom, gone by Height.
            let h = max(fx.p1.y / 100.0, 0.01);
            k = 1.0 - smoothstep(0.0, h, 1.0 - in.uv.y);
        }
        case 2u: {
            let c = fx.p1.zw / 100.0 * size;
            let r = max(fx.p2.x / 100.0 * size.y, 1.0);
            k = 1.0 - smoothstep(0.55, 1.0, length(p - c) / r);
        }
        default: {}
    }
    return d_sample((p + push * fx.p0.x * k) / size, u32(round(fx.p2.z)));
}

// Shockwave: p0 = (centre x %, y %, start s, speed px/s), p1 = (width px,
// strength px, fade out at % of the diagonal, repeat every s), p2 = (colour
// split %, ring brightness %, ring colour, edges).
@fragment
fn fs_shockwave(in: VsOut) -> @location(0) vec4<f32> {
    let size = fx.extra.zw;
    let p = in.uv * size;
    let edges = u32(round(fx.p2.w));
    var age = fx.extra.y - fx.p0.z;
    if (fx.p1.w > 0.0 && age > 0.0) {
        age = age - floor(age / fx.p1.w) * fx.p1.w;
    }
    let base = d_sample(in.uv, edges);
    if (age <= 0.0) {
        return base;
    }
    let c = fx.p0.xy / 100.0 * size;
    let d = p - c;
    let dist = length(d);
    let dir = select(vec2(0.0), d / max(dist, 1e-4), dist > 1e-4);
    let r = age * fx.p0.w;
    let w = max(fx.p1.x, 1.0);
    let x = (dist - r) / w;
    if (abs(x) >= 1.0) {
        return base;
    }
    // Weaker as it spreads, gone by "Fade out at".
    let fade_r = max(fx.p1.z / 100.0 * length(size), 1.0);
    let fade = 1.0 - smoothstep(0.0, fade_r, r);
    let bump = cos(x * 1.5707963);
    let shape = sin(x * 3.1415927) * bump;
    let off = dir * fx.p1.y * shape * fade;
    // The colours bent by different amounts.
    let k = fx.p2.x / 100.0;
    let rr = d_sample((p - off * (1.0 + k)) / size, edges);
    let gg = d_sample((p - off) / size, edges);
    let bb = d_sample((p - off * (1.0 - k)) / size, edges);
    var out = vec4(rr.r, gg.g, bb.b, max(max(rr.a, gg.a), bb.a));
    // A glint along the ring's crest (added light).
    // (a thin line just behind the crest, as light caught by the bent air)
    let crest = exp(-pow((x + 0.15) * 6.0, 2.0));
    let glint = fx.p2.y / 100.0 * crest * fade;
    out = vec4(out.rgb + rgb_of(fx.p2.z) * glint * max(out.a, 0.0), out.a);
    return out;
}

// ---- Starfield, hologram, portal, HUD ----

// What a generator draws over (draw: 0 the picture, 1 black, 2 nothing), with
// light `add` (linear, premultiplied as light).
fn gen_over(uv: vec2<f32>, add: vec3<f32>, cover: f32, draw: u32) -> vec4<f32> {
    if (draw == 1u) {
        return vec4(add, 1.0);
    }
    if (draw == 2u) {
        let a = clamp(cover, 0.0, 1.0);
        return vec4(add, a);
    }
    let c = textureSample(src, src_sampler, to_tex(uv)) * inside(uv);
    return vec4(c.rgb + add, max(c.a, clamp(cover, 0.0, 1.0)));
}

// A star's colour: the colour, varied towards red or blue.
fn star_tint(base: vec3<f32>, h: f32, variety: f32) -> vec3<f32> {
    let warm = vec3(1.0, 0.72, 0.45);
    let cool = vec3(0.6, 0.75, 1.0);
    let k = (h * 2.0 - 1.0) * variety;
    return base * mix(vec3(1.0), select(cool, warm, k > 0.0), abs(k));
}

// Starfield: p0 = (mode, amount %, size px, speed px/s), p1 = (direction °, centre x %, y %,
// streaks %), p2 = (twinkle %, brightness %, colour, colour variety %), p3 = (draw, seed).
@fragment
fn fs_starfield(in: VsOut) -> @location(0) vec4<f32> {
    let size = fx.extra.zw;
    let p = in.uv * size;
    let t = fx.extra.y;
    let mode = u32(round(fx.p0.x));
    let amount = clamp(fx.p0.y / 100.0, 0.0, 1.0);
    let star = max(fx.p0.z, 0.3);
    let seed = u32(max(fx.p3.y, 0.0)) * 977u + 13u;
    let base = rgb_of(fx.p2.z) * fx.p2.y / 100.0;
    let variety = fx.p2.w / 100.0;
    let twinkle = fx.p2.x / 100.0;
    var light = vec3(0.0);
    if (mode == 2u) {
        // Warp: stars flying out from the centre, in slices of angle; each comes from far
        // (near the centre, dim) and speeds up as it nears (out, bright, streaked).
        let c = fx.p1.yz / 100.0 * size;
        let d = p - c;
        let r = length(d);
        let reach = length(size) * 0.75;
        let slices = mix(60.0, 520.0, amount);
        let a = (atan2(d.y, d.x) / 6.2831853 + 0.5) * slices;
        for (var s = -1; s <= 1; s++) {
            let cell = floor(a) + f32(s);
            let wrapped = cell - floor(cell / slices) * slices;
            let id = u32(wrapped);
            for (var k = 0u; k < 3u; k++) {
                let h = rnd3(seed, id, k);
                let h2 = rnd3(seed + 1u, id, k);
                // Where along its flight: 0 far, 1 here (it loops).
                let speed = fx.p0.w / 400.0 * (0.6 + 0.8 * h2);
                let u = fract(h + t * speed);
                let rr = reach * u * u * u;
                let ang = (cell + 0.2 + 0.6 * rnd3(seed + 2u, id, k)) / slices;
                let dir = vec2(cos((ang - 0.5) * 6.2831853), sin((ang - 0.5) * 6.2831853));
                let head = c + dir * rr;
                let tail = c + dir * rr * (1.0 - fx.p1.w / 100.0 * 0.5 * u);
                // Distance from the streak from tail to head.
                let ab = head - tail;
                let tt = clamp(dot(p - tail, ab) / max(dot(ab, ab), 1e-4), 0.0, 1.0);
                let dist = length(p - (tail + ab * tt));
                let w = star * (0.4 + 1.2 * u);
                let glow = exp(-dist * dist / (w * w)) * (0.15 + 1.2 * u * u) * (0.35 + 0.65 * tt);
                light += star_tint(base, rnd3(seed + 3u, id, k), variety) * glow;
            }
        }
    } else {
        // Layers of stars on a grid (one at a random place in each cell), nearer layers
        // bigger, brighter and (drifting) faster.
        let a = radians(fx.p1.x);
        let dir = vec2(cos(a), -sin(a));
        let cell = mix(90.0, 14.0, amount);
        for (var layer = 0u; layer < 3u; layer++) {
            let depth = 0.45 + 0.35 * f32(layer);
            var q = p;
            if (mode == 1u) {
                q = p - dir * fx.p0.w * t * depth;
            }
            let c = cell / depth;
            let g = floor(q / c);
            for (var j = -1; j <= 1; j++) {
                for (var i = -1; i <= 1; i++) {
                    let gi = g + vec2(f32(i), f32(j));
                    let ix = bitcast<u32>(i32(gi.x));
                    let iy = bitcast<u32>(i32(gi.y));
                    let s = seed + layer * 101u;
                    let h = rnd3(s, ix, iy);
                    // Most cells have a faint star; a few are bright.
                    let bright = pow(rnd3(s + 1u, ix, iy), 6.0) * 3.0 + 0.15;
                    let pos = (gi + vec2(rnd3(s + 2u, ix, iy), rnd3(s + 3u, ix, iy))) * c;
                    let dist = length(q - pos);
                    let w = star * depth * (0.6 + 0.8 * rnd3(s + 4u, ix, iy));
                    let tw = 1.0 - twinkle * (0.5 + 0.5 * sin(t * (2.0 + 5.0 * h) + h * 40.0));
                    light += star_tint(base, rnd3(s + 5u, ix, iy), variety) * exp(-dist * dist / (w * w)) * bright * depth * tw;
                }
            }
        }
    }
    let cover = dot(light, LUMA);
    return gen_over(in.uv, light, cover, u32(round(fx.p3.x)));
}

// Hologram: p0 = (colour, opacity %, scanlines %, spacing px), p1 = (line speed px/s,
// flicker %, glitches %, edge glow %), p2 = (colour split px, keep colours %, seed).
@fragment
fn fs_hologram(in: VsOut) -> @location(0) vec4<f32> {
    let size = fx.extra.zw;
    let t = fx.extra.y;
    let seed = u32(max(fx.p2.z, 0.0)) * 331u + 7u;
    let tex = vec2(1.0) / size;
    var uv = in.uv;
    // Glitches: now and then a band of rows torn sideways.
    let g = fx.p1.z / 100.0;
    let slot = floor(t * 8.0);
    if (g > 0.0 && rnd3(seed, bitcast<u32>(i32(slot)), 1u) < g * 0.6) {
        let y0 = rnd3(seed, bitcast<u32>(i32(slot)), 2u);
        let hgt = 0.02 + 0.1 * rnd3(seed, bitcast<u32>(i32(slot)), 3u);
        if (abs(uv.y - y0) < hgt) {
            uv.x += (rnd3(seed, bitcast<u32>(i32(slot)), 4u) - 0.5) * 0.12 * g;
        }
    }
    let split = vec2(fx.p2.x, 0.0) * tex;
    let c = textureSample(src, src_sampler, to_tex(uv)) * inside(uv);
    let cr = textureSample(src, src_sampler, to_tex(uv + split)) * inside(uv + split);
    let cb = textureSample(src, src_sampler, to_tex(uv - split)) * inside(uv - split);
    let a = max(c.a, max(cr.a, cb.a));
    if (a <= 0.0) {
        return vec4(0.0);
    }
    // One colour, by brightness (a little of the split at the sides).
    let lum = dot(unpremul(c), LUMA);
    let tint = rgb_of(fx.p0.x);
    var col = tint * (0.25 + 1.1 * lum);
    col.r += (unpremul(cr).r - unpremul(c).r) * 0.5;
    col.b += (unpremul(cb).b - unpremul(c).b) * 0.5;
    col = mix(col, unpremul(c), fx.p2.y / 100.0);
    // Edges glow: where the shape's edge is (the alpha's slope over a few pixels).
    let e = 2.0 * tex;
    let ax = textureSample(src, src_sampler, to_tex(uv + vec2(e.x, 0.0))).a - textureSample(src, src_sampler, to_tex(uv - vec2(e.x, 0.0))).a;
    let ay = textureSample(src, src_sampler, to_tex(uv + vec2(0.0, e.y))).a - textureSample(src, src_sampler, to_tex(uv - vec2(0.0, e.y))).a;
    let edge = clamp(length(vec2(ax, ay)), 0.0, 1.0);
    col += tint * edge * fx.p1.w / 100.0 * 1.5;
    // Scanlines rolling down it.
    let spacing = max(fx.p0.w, 1.0);
    let line = 0.5 + 0.5 * sin((uv.y * size.y - fx.p1.x * t) / spacing * 6.2831853);
    let lines = 1.0 - fx.p0.z / 100.0 * 0.7 * line;
    // A brighter band passing down now and then.
    let band = exp(-pow((fract(uv.y - t * 0.25) - 0.5) * 14.0, 2.0)) * 0.35;
    // Flicker.
    let f = fx.p1.y / 100.0;
    let flick = 1.0 - f * 0.6 * step(0.8, rnd3(seed + 9u, bitcast<u32>(i32(floor(t * 24.0))), 5u)) - f * 0.15 * (0.5 + 0.5 * sin(t * 37.0));
    col = col * lines * flick + tint * band;
    let op = fx.p0.y / 100.0 * clamp(0.55 + 0.45 * lum + edge, 0.0, 1.0);
    let alpha = a * op * flick;
    return vec4(col * alpha, alpha);
}

// Portal: p0 = (centre x %, y %, radius % of height, open %), p1 = (rim px, colour,
// second colour, spin turns/s), p2 = (sparks %, inside, swirl °, ripple px), p3 =
// (brightness %, stretch %, seed).
@fragment
fn fs_portal(in: VsOut) -> @location(0) vec4<f32> {
    let size = fx.extra.zw;
    let t = fx.extra.y;
    let p = in.uv * size;
    let open = clamp(fx.p0.w / 100.0, 0.0, 1.0);
    let c = fx.p0.xy / 100.0 * size;
    // Taller than wide with Stretch above 100 %.
    let stretch = max(fx.p3.y / 100.0, 0.1);
    let rad = max(fx.p0.z / 100.0 * size.y * open, 0.5);
    var d = p - c;
    d.y /= stretch;
    let dist = length(d);
    let ang = atan2(d.y, d.x);
    let rim = max(fx.p1.x, 1.0);
    let seed = u32(max(fx.p3.z, 0.0)) * 613u + 3u;
    // The air round it ripples.
    let x = (dist - rad) / (rim * 3.0);
    let ripple = fx.p2.w * exp(-x * x) * sin(dist * 0.15 - t * 9.0) * open;
    let dir = select(vec2(0.0), normalize(p - c), dist > 1e-3);
    var base = textureSample(src, src_sampler, to_tex((p + dir * ripple) / size)) * inside((p + dir * ripple) / size);
    // Inside.
    let inner = 1.0 - smoothstep(rad - rim * 0.6, rad, dist);
    let mode = u32(round(fx.p2.y));
    if (inner > 0.0) {
        var ins: vec4<f32>;
        if (mode == 0u) {
            // The picture swirled, more at the middle, turning.
            let k = 1.0 - dist / max(rad, 1.0);
            let turn = radians(fx.p2.z) * k * k + t * fx.p1.w * 6.2831853 * 0.5;
            let cs = cos(turn);
            let sn = sin(turn);
            let r = vec2(cs * (p.x - c.x) - sn * (p.y - c.y), sn * (p.x - c.x) + cs * (p.y - c.y));
            let q = (c + r * (0.6 + 0.4 * (1.0 - k))) / size;
            ins = textureSample(src, src_sampler, to_tex(q)) * inside(q);
        } else if (mode == 1u) {
            ins = vec4(0.0, 0.0, 0.0, 1.0);
        } else {
            ins = vec4(0.0);
        }
        base = mix(base, ins, inner);
    }
    // The rim: a band of light, and sparks streaming round it.
    let spin = t * fx.p1.w * 6.2831853;
    let band = exp(-pow((dist - rad) / (rim * 0.45), 2.0));
    var light = rgb_of(fx.p1.y) * band;
    let sparks = fx.p2.x / 100.0;
    if (sparks > 0.0) {
        // Rings of sparks at a few radii, each a dashed stream of short arcs.
        for (var k = 0u; k < 4u; k++) {
            let rr = rad + (rnd3(seed, k, 1u) - 0.5) * rim * 1.6;
            let n = 40.0 + 60.0 * rnd3(seed, k, 2u);
            let speed = (0.7 + 0.8 * rnd3(seed, k, 3u)) * select(-1.0, 1.0, k % 2u == 0u);
            let u = (ang + spin * speed) / 6.2831853 * n;
            let cell = floor(u);
            let f = fract(u);
            let on = step(1.0 - sparks * 0.8, rnd3(seed + k, bitcast<u32>(i32(cell)), 7u));
            let streak = smoothstep(0.0, 0.15, f) * (1.0 - smoothstep(0.35, 0.9, f));
            let radial = exp(-pow((dist - rr) / 1.6, 2.0));
            light += mix(rgb_of(fx.p1.y), rgb_of(fx.p1.z), rnd3(seed, k, 4u)) * on * streak * radial * 1.6;
        }
        // Sparks thrown off the rim: short dashes flying out along it and away, fading.
        for (var k = 0u; k < 3u; k++) {
            let n = 90.0 + 40.0 * f32(k);
            let u = (ang + spin * (1.2 + 0.3 * f32(k))) / 6.2831853 * n;
            let cell = floor(u);
            let h = rnd3(seed + 20u + k, bitcast<u32>(i32(cell)), 1u);
            // How far out this one has flown (it loops), and its dash.
            let life = fract(h * 7.0 + t * (0.8 + h));
            let out = rad + rim * (0.3 + 4.0 * life);
            let dash = exp(-pow((fract(u) - 0.5) * 9.0, 2.0)) * exp(-pow((dist - out) / 2.0, 2.0));
            light += rgb_of(fx.p1.z) * dash * step(1.0 - sparks * 0.5, h) * (1.0 - life) * 2.0;
        }
    }
    light *= fx.p3.x / 100.0 * open;
    let a = max(base.a, clamp(dot(light, LUMA), 0.0, 1.0));
    return vec4(base.rgb + light, a);
}

// A line `w` px wide at distance `d` (antialiased), 0..1.
fn hud_line(d: f32, w: f32) -> f32 {
    return 1.0 - smoothstep(w * 0.5 - 0.6, w * 0.5 + 0.6, abs(d));
}

// HUD: p0 = (element, centre x %, y %, size % of height), p1 = (colour, line px, spin turns/s,
// detail %), p2 = (draw on %, opacity %, flicker %, seed).
@fragment
fn fs_hud(in: VsOut) -> @location(0) vec4<f32> {
    let size = fx.extra.zw;
    let t = fx.extra.y;
    let p = in.uv * size;
    let c = fx.p0.yz / 100.0 * size;
    let r = max(fx.p0.w / 100.0 * size.y * 0.5, 2.0);
    let d = p - c;
    let dist = length(d);
    // Angle 0..1 clockwise from the top.
    let ang = fract(atan2(d.x, -d.y) / 6.2831853 + 1.0);
    let w = max(fx.p1.y, 0.3);
    let on = clamp(fx.p2.x / 100.0, 0.0, 1.0);
    let detail = fx.p1.w / 100.0;
    let spin = t * fx.p1.z;
    let seed = u32(max(fx.p2.w, 0.0)) * 197u + 5u;
    var ink = 0.0;
    switch (u32(round(fx.p0.x))) {
        case 0u: {
            // Reticle: an outer ring broken in four, an inner ring, a cross with a gap, ticks.
            let gap = 0.04;
            let q = fract(ang + 0.125);
            let broken = step(gap, fract(q * 4.0)) * step(fract(q * 4.0), 1.0 - gap);
            ink += hud_line(dist - r, w) * broken * step(ang, on);
            ink += hud_line(dist - r * 0.55, w * 0.8) * step(fract(ang - spin), on);
            let cross = min(hud_line(d.x, w), step(r * 0.15, abs(d.y)) * step(abs(d.y), r * 0.85)) + min(hud_line(d.y, w), step(r * 0.15, abs(d.x)) * step(abs(d.x), r * 0.85));
            ink += clamp(cross, 0.0, 1.0) * step(0.3, on);
            let n = floor(mix(12.0, 72.0, detail));
            let tick = step(fract(ang * n + 0.04), 0.08) * step(r * 1.04, dist) * step(dist, r * (1.1 + 0.06 * step(fract(ang * n / 4.0), 0.25)));
            ink += tick * step(ang, on);
            ink += (1.0 - smoothstep(w, w + 1.2, dist)) * step(0.6, on);
        }
        case 1u: {
            // Radar: rings, the sweep with its fading wake, blips that glow as it passes.
            for (var k = 1; k <= 4; k++) {
                ink += hud_line(dist - r * f32(k) / 4.0, w * select(0.6, 1.0, k == 4)) * step(ang, on);
            }
            ink += (hud_line(d.x, w * 0.5) + hud_line(d.y, w * 0.5)) * step(dist, r) * 0.5 * on;
            let behind = fract(spin - ang);
            ink += exp(-behind * 7.0) * 0.55 * step(dist, r) * on;
            ink += hud_line(behind * dist * 6.2831853, w) * step(dist, r) * on;
            let nb = u32(mix(3.0, 12.0, detail));
            for (var b = 0u; b < nb; b++) {
                let ba = rnd3(seed, b, 1u);
                let br = r * (0.2 + 0.75 * rnd3(seed, b, 2u));
                let bp = c + vec2(sin(ba * 6.2831853), -cos(ba * 6.2831853)) * br;
                let fade = exp(-fract(spin - ba) * 4.0);
                ink += (1.0 - smoothstep(w * 1.5, w * 3.0, length(p - bp))) * fade * on;
            }
        }
        case 2u: {
            // Rings of ticks and arcs turning at their own speeds, each way.
            let rings = u32(mix(2.0, 6.0, detail));
            for (var k = 0u; k < rings; k++) {
                let rr = r * (0.35 + 0.65 * f32(k + 1u) / f32(rings));
                let speed = (0.3 + rnd3(seed, k, 1u)) * select(-1.0, 1.0, k % 2u == 0u);
                let a2 = fract(ang + spin * speed);
                let segs = floor(2.0 + rnd3(seed, k, 2u) * 6.0);
                let arc = step(0.25 + 0.5 * rnd3(seed, k, 3u), fract(a2 * segs));
                let ticks = step(fract(a2 * (24.0 + 48.0 * rnd3(seed, k, 4u))), 0.15);
                let style = rnd3(seed, k, 5u);
                let shape = select(arc * hud_line(dist - rr, w), ticks * step(abs(dist - rr), w * 3.0), style > 0.5);
                ink += shape * step(ang, on);
            }
        }
        case 3u: {
            // Bars in a box, rising and falling like a readout.
            let box = vec2(r * 1.6, r);
            let q = d + box * 0.5;
            let inbox = step(0.0, q.x) * step(q.x, box.x) * step(0.0, q.y) * step(q.y, box.y);
            let frame = (hud_line(q.x, w) + hud_line(q.x - box.x, w)) * step(0.0, q.y) * step(q.y, box.y) + (hud_line(q.y, w) + hud_line(q.y - box.y, w)) * step(0.0, q.x) * step(q.x, box.x);
            ink += clamp(frame, 0.0, 1.0) * step(0.1, on);
            let n = floor(mix(6.0, 40.0, detail));
            let i = floor(q.x / box.x * n);
            let lvl = 0.15 + 0.8 * (0.5 + 0.5 * sin(t * (1.5 + 3.0 * rnd3(seed, bitcast<u32>(i32(i)), 1u)) + i * 1.7)) * vnoise(seed, vec2(i * 0.7, t * 1.3));
            let bar = step(fract(q.x / box.x * n), 0.7) * step(box.y * (1.0 - lvl * on), q.y);
            ink += bar * inbox * 0.8;
        }
        default: {
            // Corner brackets round a box, closing in as it draws on.
            let half = vec2(r * 1.4, r) * (1.4 - 0.4 * on);
            let q = abs(d) - half;
            let arm = r * 0.35;
            let corner = (hud_line(q.x, w) * step(-arm, q.y) * step(q.y, w)) + (hud_line(q.y, w) * step(-arm, q.x) * step(q.x, w));
            ink += clamp(corner, 0.0, 1.0) * step(0.05, on);
        }
    }
    let f = fx.p2.z / 100.0;
    let flick = 1.0 - f * step(0.75, rnd3(seed + 3u, bitcast<u32>(i32(floor(t * 20.0))), 9u));
    let a = clamp(ink, 0.0, 1.0) * fx.p2.y / 100.0 * flick;
    let light = rgb_of(fx.p1.x) * a;
    let base = textureSample(src, src_sampler, to_tex(in.uv)) * inside(in.uv);
    return vec4(base.rgb * (1.0 - a) + light, max(base.a, a));
}

// ---- Light sweep, electric arcs ----

// Light sweep: p0 = (angle °, width px, softness %, start s), p1 = (takes s, repeat s,
// position % (below 0: timed), brightness %), p2 = (colour, lights, second sweep %).
@fragment
fn fs_sweep(in: VsOut) -> @location(0) vec4<f32> {
    let size = fx.extra.zw;
    let c = textureSample(src, src_sampler, to_tex(in.uv)) * inside(in.uv);
    if (c.a <= 0.0) {
        return c;
    }
    let p = in.uv * size;
    let a = radians(fx.p0.x);
    let n = vec2(cos(a), -sin(a));
    // Where the band is, 0..1 across the layer along n.
    let ext = abs(n.x) * size.x + abs(n.y) * size.y;
    let lo = min(0.0, n.x * size.x) + min(0.0, n.y * size.y);
    var pos = fx.p1.z / 100.0;
    if (fx.p1.z < 0.0) {
        var age = fx.extra.y - fx.p0.w;
        if (fx.p1.y > 0.0 && age > 0.0) {
            age = age - floor(age / fx.p1.y) * fx.p1.y;
        }
        pos = age / max(fx.p1.x, 1e-3);
        if (age < 0.0 || pos > 1.0) {
            return c;
        }
    }
    let w = max(fx.p0.y, 1.0);
    // From just before the layer to just past it.
    let centre = lo - w + pos * (ext + 2.0 * w);
    let d = abs(dot(p, n) - centre);
    let soft = clamp(fx.p0.z / 100.0, 0.0, 1.0);
    let band = 1.0 - smoothstep(w * 0.5 * (1.0 - soft), w * 0.5, d);
    // A thinner second band trailing it.
    let d2 = abs(dot(p, n) - (centre - w * 1.3));
    let band2 = (1.0 - smoothstep(w * 0.12 * (1.0 - soft), w * 0.12 + 0.5, d2)) * fx.p2.z / 100.0;
    let lin = unpremul(c);
    var k = 1.0;
    switch (u32(round(fx.p2.y))) {
        case 1u: { k = smoothstep(0.25, 0.9, dot(pow(max(lin, vec3(0.0)), vec3(1.0 / 2.2)), LUMA)); }
        default: {}
    }
    let light = rgb_of(fx.p2.x) * (band + band2) * k * fx.p1.w / 100.0;
    return vec4(c.rgb + light * c.a, c.a);
}

// The shape smoothed over a disc `r` px round layer uv `uv`: how much of it is there (x)
// and how fast that changes per px (yz).
fn arc_cover(uv: vec2<f32>, r: f32, size: vec2<f32>) -> vec3<f32> {
    var s = textureSampleLevel(src, src_sampler, to_tex(uv), 0.0).a * inside(uv);
    var g = vec2(0.0);
    var rs = 0.0;
    for (var j = 1; j <= 3; j++) {
        let rr = r * f32(j) / 3.0;
        rs += rr;
        for (var k = 0; k < 16; k++) {
            let a = (f32(k) + 0.5 * f32(j)) * 0.3926991;
            let d = vec2(cos(a), sin(a));
            let q = uv + d * rr / size;
            let v = textureSampleLevel(src, src_sampler, to_tex(q), 0.0).a * inside(q);
            s += v;
            g += v * d;
        }
    }
    // (for a straight ramp, the taps' pull along d is its slope × 8 × the radii)
    return vec3(s / 49.0, g / max(8.0 * rs, 1e-3));
}

// Jagged noise for an arc: broad bends, then finer and finer kinks.
fn arc_noise(seed: u32, q: vec2<f32>, z: f32) -> f32 {
    return vnoise3(seed, vec3(q, z)) * 2.0 - 1.0 + (vnoise3(seed + 5u, vec3(q * 2.7, z * 1.4)) * 2.0 - 1.0) * 0.4 + (vnoise3(seed + 6u, vec3(q * 7.3, z * 2.1)) * 2.0 - 1.0) * 0.12;
}

// Electric arcs: p0 = (colour, amount %, reach px, thickness px), p1 = (speed /s, flicker %,
// glow %, brightness %), p2 = (over the shape %, scale px, seed).
// Each strand is where the smoothed shape crosses a level, nudged by noise: so it hugs the
// outline, jagged, and where the noise is strong it leaps off it.
@fragment
fn fs_arcs(in: VsOut) -> @location(0) vec4<f32> {
    let size = fx.extra.zw;
    let t = fx.extra.y;
    let base = textureSample(src, src_sampler, to_tex(in.uv)) * inside(in.uv);
    let reach = max(fx.p0.z, 1.0);
    let cg = arc_cover(in.uv, max(reach * 0.35, 1.5), size);
    // Nothing anywhere near: nothing to draw.
    if (cg.x <= 0.0005 && length(cg.yz) <= 1e-5) {
        return base;
    }
    let seed = u32(max(fx.p2.z, 0.0)) * 811u + 17u;
    let p = in.uv * size;
    let scale = max(fx.p2.y, 4.0);
    let thick = max(fx.p0.w, 0.3);
    let amount = fx.p0.y / 100.0;
    // The arcs re-form a few times a second, crawling between.
    let step_t = floor(t * fx.p1.x);
    let crawl = fract(t * fx.p1.x);
    let fl = fx.p1.y / 100.0;
    var core = 0.0;
    var halo = 0.0;
    for (var k = 0u; k < 4u; k++) {
        let ks = seed + k * 131u;
        let z = step_t * 1.37 + crawl * 0.4 + f32(k) * 11.0;
        // Two of them sit over the shape (Over the shape), the rest on its edge.
        let level = select(0.5, mix(0.5, 0.9, fx.p2.x / 100.0), k >= 2u);
        // How hard the noise pushes it off the edge: more for later strands.
        let push = (0.3 + 0.18 * f32(k)) * (0.4 + 0.6 * reach / 30.0);
        let e = 1.0;
        let q = p / scale;
        let n0 = arc_noise(ks, q, z);
        let nx = arc_noise(ks, (p + vec2(e, 0.0)) / scale, z);
        let ny = arc_noise(ks, (p + vec2(0.0, e)) / scale, z);
        let f = cg.x - level + n0 * push;
        let grad = cg.yz + vec2(nx - n0, ny - n0) / e * push;
        // Distance in px to where f is 0.
        let dist = abs(f) / max(length(grad), 1e-4);
        // Only some stretches of each strand are lit (Amount), changing as it jumps.
        let lit = smoothstep(1.0 - amount, 1.0 - amount + 0.08, vnoise3(ks + 9u, vec3(q * 0.45, step_t * 0.9 + f32(k))));
        let flick = 1.0 - fl * step(0.55, rnd3(ks, bitcast<u32>(i32(floor(t * 30.0))), 3u));
        // (only where the shape is near: no strands made of noise alone)
        let near = smoothstep(0.0, 0.06, cg.x) * (1.0 - smoothstep(0.97, 1.0, cg.x) * select(1.0, 0.0, k >= 2u));
        let w = lit * flick * near * select(1.0, 0.6, k >= 2u);
        core += exp(-pow(dist / thick, 2.0)) * w;
        halo += exp(-dist / (thick * 4.0)) * w * exp(-abs(f) * 6.0);
    }
    let col = rgb_of(fx.p0.x);
    let hl = halo * fx.p1.z / 100.0 * 0.25;
    let light = (col * hl + mix(col, vec3(1.0), 0.7) * min(core, 1.5)) * fx.p1.w / 100.0;
    let a = max(base.a, clamp(core + hl, 0.0, 1.0));
    return vec4(base.rgb + light, a);
}

// Directional blur: samples along a line through the pixel (angle 0 = across,
// 90 = down), evenly, Gaussian-weighted, or behind only, fading.
@fragment
fn fs_dir_blur(in: VsOut) -> @location(0) vec4<f32> {
    let size = max(fx.extra.zw, vec2(1.0));
    let a = radians(fx.p0.x);
    let len = max(fx.p0.y, 0.0);
    let dir = vec2(cos(a), sin(a)) / size;
    let mode = u32(round(fx.p0.z));
    let n = i32(clamp(ceil(len / 1.5), 2.0, 96.0));
    var acc = vec4(0.0);
    var wsum = 0.0;
    for (var i = 0; i < n; i++) {
        let f = (f32(i) + 0.5) / f32(n);
        var off = (f - 0.5) * len;
        var w = 1.0;
        if (mode == 2u) {
            off = -f * len;
            w = 1.0 - f * 0.9;
        } else if (mode == 1u) {
            let g = (f - 0.5) * 3.0;
            w = exp(-g * g * 2.0);
        }
        // (the edge pixels repeat past the layer's edge, so footage doesn't darken there)
        let uv = clamp(in.uv + dir * off, vec2(0.0), vec2(1.0));
        acc += textureSampleLevel(src, src_sampler, to_tex(uv), 0.0) * w;
        wsum += w;
    }
    return acc / max(wsum, 1e-5);
}

// Radial blur: samples turned round the centre (Spin: Amount in degrees) or
// along the line to it (Zoom: Amount in % of the distance).
@fragment
fn fs_radial_blur(in: VsOut) -> @location(0) vec4<f32> {
    let size = max(fx.extra.zw, vec2(1.0));
    let c = fx.p0.xy / 100.0 * size;
    let d = in.uv * size - c;
    let zoom = u32(round(fx.p0.z)) == 1u;
    let n = i32(clamp(round(fx.p1.x), 4.0, 128.0));
    var acc = vec4(0.0);
    for (var i = 0; i < n; i++) {
        let f = (f32(i) + 0.5) / f32(n) - 0.5;
        var q: vec2<f32>;
        if (zoom) {
            q = c + d * (1.0 + f * fx.p0.w / 100.0);
        } else {
            let t = radians(fx.p0.w) * f;
            let cs = cos(t);
            let sn = sin(t);
            q = c + vec2(cs * d.x - sn * d.y, sn * d.x + cs * d.y);
        }
        let uv = clamp(q / size, vec2(0.0), vec2(1.0));
        acc += textureSampleLevel(src, src_sampler, to_tex(uv), 0.0);
    }
    return acc / f32(n);
}

// Motion blur: the layer drawn at moments through the shutter and averaged.
// Values 3–8 and 9–14 are where content now at a layer point q was at the
// shutter's opening and closing, in layer pixels from the centre: x = a·qx +
// c·qy + tx, y = b·qx + d·qy + ty (filled in by the scene); in between, mixed.
@fragment
fn fs_motion_blur(in: VsOut) -> @location(0) vec4<f32> {
    let size = max(fx.extra.zw, vec2(1.0));
    let p = in.uv * size - size * 0.5;
    let m0 = vec4(fx.p0.w, fx.p1.x, fx.p1.y, fx.p1.z);
    let t0 = vec2(fx.p1.w, fx.p2.x);
    let m1 = vec4(fx.p2.y, fx.p2.z, fx.p2.w, fx.p3.x);
    let t1 = vec2(fx.p3.y, fx.p3.z);
    let n = i32(clamp(round(fx.p0.z), 2.0, 64.0));
    var acc = vec4(0.0);
    var count = 0.0;
    for (var i = 0; i < n; i++) {
        let s = (f32(i) + 0.5) / f32(n);
        let m = mix(m0, m1, s);
        let r = p - mix(t0, t1, s);
        let det = m.x * m.w - m.y * m.z;
        if (abs(det) < 1e-6) {
            continue;
        }
        let q = vec2(m.w * r.x - m.z * r.y, -m.y * r.x + m.x * r.y) / det;
        let uv = (q + size * 0.5) / size;
        acc += textureSampleLevel(src, src_sampler, to_tex(uv), 0.0) * inside(uv);
        count += 1.0;
    }
    return acc / max(count, 1.0);
}

// --- Vision looks (night vision, thermal, infrared, X-ray) ---

// The picture's brightness as the eye sees it (0–1), from premultiplied colour.
fn vl_bright(c: vec4<f32>) -> f32 {
    return pow(max(dot(unpremul(c), LUMA), 0.0), 1.0 / 2.2);
}

// The picture round uv, averaged over a ring `r` layer pixels out (a cheap glow).
fn vl_ring(uv: vec2<f32>, r: f32) -> vec4<f32> {
    let size = max(fx.extra.zw, vec2(1.0));
    var acc = vec4(0.0);
    for (var k = 0; k < 12; k++) {
        let a = f32(k) * 0.5235988;
        let q = clamp(uv + vec2(cos(a), sin(a)) * r / size, vec2(0.0), vec2(1.0));
        acc += textureSampleLevel(src, src_sampler, to_tex(q), 0.0);
    }
    return acc / 12.0;
}

// Grain that changes every frame (−0.5–0.5), in cells `cell` layer pixels across.
fn vl_grain(uv: vec2<f32>, cell: f32, seed: u32) -> f32 {
    let p = uv * max(fx.extra.zw, vec2(1.0)) / max(cell, 0.5);
    let frame = u32(max(fx.extra.y, 0.0) * 30.0);
    return rnd3(ibits(p.x), ibits(p.y), seed ^ (frame * 9781u)) - 0.5;
}

// How much of the picture a goggle mask leaves (1 inside): 0 none, 1 one round
// eyepiece, 2 binoculars; `soft` 0–1.
fn vl_goggles(uv: vec2<f32>, kind: u32, soft: f32) -> f32 {
    if (kind == 0u) {
        return 1.0;
    }
    let aspect = fx.extra.x;
    let p = (uv - 0.5) * vec2(aspect, 1.0);
    var d: f32;
    if (kind == 1u) {
        d = length(p) / 0.48;
    } else {
        let x = abs(p.x) - min(0.36, aspect * 0.25);
        d = length(vec2(x, p.y)) / 0.46;
    }
    let s = max(soft, 0.01) * 0.3;
    return 1.0 - smoothstep(1.0 - s, 1.0, d);
}

@fragment
fn fs_night_vision(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    if (c.a <= 0.0) {
        return vec4(0.0);
    }
    let seed = u32(max(fx.p2.x, 0.0)) * 7919u + 11u;
    let gain = max(fx.p0.x / 100.0, 0.0);
    // Amplified light (lifting the dark most), with highlights blooming.
    var b = 1.0 - exp(-vl_bright(c) * gain * 1.3);
    let glow = vl_bright(vl_ring(in.uv, 6.0));
    b += max(glow * gain * 0.25 - 0.4, 0.0) * fx.p0.w / 100.0;
    // Noise: fine grain, stronger in the dark (as the tube's).
    b += vl_grain(in.uv, 1.5, seed) * fx.p0.z / 100.0 * (0.6 - 0.4 * b);
    // Scan lines and a slow flicker.
    let y = in.uv.y * fx.extra.w;
    b *= 1.0 - fx.p1.z / 100.0 * 0.5 * (0.5 + 0.5 * cos(y * 3.14159));
    let t = fx.extra.y;
    b *= 1.0 - fx.p1.w / 100.0 * 0.15 * (0.5 + 0.5 * sin(t * 37.0) * sin(t * 13.3 + 1.0));
    b = clamp(b, 0.0, 1.5);
    let tint = rgb_of(fx.p0.y);
    var col = to_linear(vec3(b)) * tint + vec3(max(b - 1.0, 0.0));
    let mask = vl_goggles(in.uv, u32(round(fx.p1.x)), fx.p1.y / 100.0);
    return vec4(col * mask * c.a, c.a);
}

// A thermal palette at heat h (0–1), as display colour (sRGB).
fn th_palette(kind: u32, h: f32) -> vec3<f32> {
    let x = clamp(h, 0.0, 1.0);
    if (kind == 2u) {
        return vec3(x);
    }
    if (kind == 3u) {
        return vec3(1.0 - x);
    }
    if (kind == 1u) {
        // Rainbow: dark blue, blue, cyan, green, yellow, red, white.
        let k = array<vec3<f32>, 7>(vec3(0.0, 0.0, 0.25), vec3(0.0, 0.2, 1.0), vec3(0.0, 0.9, 1.0), vec3(0.1, 1.0, 0.2), vec3(1.0, 1.0, 0.0), vec3(1.0, 0.1, 0.0), vec3(1.0, 1.0, 1.0));
        let f = x * 6.0;
        let i = u32(min(floor(f), 5.0));
        return mix(k[i], k[i + 1u], f - f32(i));
    }
    if (kind == 4u) {
        // Lava: black, deep red, orange, yellow.
        let k = array<vec3<f32>, 4>(vec3(0.0, 0.0, 0.0), vec3(0.6, 0.0, 0.05), vec3(1.0, 0.45, 0.0), vec3(1.0, 1.0, 0.6));
        let f = x * 3.0;
        let i = u32(min(floor(f), 2.0));
        return mix(k[i], k[i + 1u], f - f32(i));
    }
    // Iron: black, indigo, purple, red, orange, yellow, white.
    let k = array<vec3<f32>, 7>(vec3(0.0, 0.0, 0.0), vec3(0.15, 0.0, 0.45), vec3(0.55, 0.0, 0.6), vec3(0.9, 0.15, 0.25), vec3(1.0, 0.5, 0.0), vec3(1.0, 0.85, 0.1), vec3(1.0, 1.0, 0.9));
    let f = x * 6.0;
    let i = u32(min(floor(f), 5.0));
    return mix(k[i], k[i + 1u], f - f32(i));
}

// Heat at uv: both (0), brightness (1), or warmth (2: reds and skin over blues).
fn th_heat(c: vec4<f32>, hsrc: u32) -> f32 {
    let rgb = pow(unpremul(c), vec3(1.0 / 2.2));
    let bright = dot(rgb, vec3(0.2126, 0.7152, 0.0722));
    // Reds and skin (red over green over blue) warm; greens and blues cold.
    let warm = clamp((rgb.r - max(rgb.g, rgb.b)) * 1.6 + (rgb.r - rgb.b) * 0.5 + 0.15, 0.0, 1.0) * (0.4 + 0.6 * max(rgb.r, max(rgb.g, rgb.b)));
    if (hsrc == 1u) {
        return bright;
    }
    if (hsrc == 2u) {
        return warm;
    }
    return 0.3 * bright + 0.7 * warm;
}

@fragment
fn fs_thermal(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    if (c.a <= 0.0) {
        return vec4(0.0);
    }
    let hsrc = u32(round(fx.p0.y));
    let kind = u32(round(fx.p0.x));
    // Heat spreads: the picture's heat smoothed a little (the camera's blur).
    var h = th_heat(c, hsrc);
    let r = max(fx.p1.x, 0.0);
    if (r > 0.25) {
        h = 0.4 * h + 0.6 * th_heat(vl_ring(in.uv, r), hsrc);
    }
    let lo = fx.p0.z / 100.0;
    let hi = fx.p0.w / 100.0;
    h = clamp((h - lo) / max(hi - lo, 0.001), 0.0, 1.0);
    h = clamp(h + vl_grain(in.uv, 2.0, u32(max(fx.p1.w, 0.0)) * 104729u + 7u) * fx.p1.y / 100.0 * 0.15, 0.0, 1.0);
    var col = th_palette(kind, h);
    // The scale bar: down the right-hand edge, hottest at the top.
    if (fx.p1.z > 0.5) {
        let size = max(fx.extra.zw, vec2(1.0));
        let p = in.uv * size;
        let bx = size.x - size.y * 0.06;
        let top = size.y * 0.15;
        let bottom = size.y * 0.85;
        let w = size.y * 0.025;
        if (p.x > bx && p.x < bx + w && p.y > top && p.y < bottom) {
            col = th_palette(kind, 1.0 - (p.y - top) / (bottom - top));
        } else if (p.x > bx - 2.0 && p.x < bx + w + 2.0 && p.y > top - 2.0 && p.y < bottom + 2.0) {
            col = vec3(1.0);
        }
    }
    return vec4(to_linear(col) * c.a, c.a);
}

// The near-infrared a scene might give off, guessed from its colour: green
// leaves bright, blue sky and water dark, other things by their brightness.
fn ir_guess(rgb: vec3<f32>, foliage: f32, sky: f32) -> f32 {
    let green = clamp((rgb.g - max(rgb.r, rgb.b) * 0.8) * 3.0, 0.0, 1.0);
    let blue = clamp((rgb.b - max(rgb.r, rgb.g)) * 3.0, 0.0, 1.0);
    let base = dot(rgb, vec3(0.45, 0.4, 0.15));
    return clamp(base + green * foliage * 0.8 - blue * sky * 0.7, 0.0, 1.5);
}

@fragment
fn fs_infrared(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    if (c.a <= 0.0) {
        return vec4(0.0);
    }
    let rgb = pow(unpremul(c), vec3(1.0 / 2.2));
    let foliage = fx.p0.y / 100.0;
    let sky = fx.p0.z / 100.0;
    var ir = ir_guess(rgb, foliage, sky);
    // Halation: infrared light spreading in the film.
    let around = pow(unpremul(vl_ring(in.uv, 8.0)), vec3(1.0 / 2.2));
    ir += max(ir_guess(around, foliage, sky) - 0.6, 0.0) * fx.p0.w / 100.0;
    ir = clamp((ir - 0.5) * fx.p1.x / 100.0 + 0.5, 0.0, 1.0);
    ir = clamp(ir + vl_grain(in.uv, 1.2, u32(max(fx.p1.w, 0.0)) * 15485863u + 3u) * fx.p1.y / 100.0 * 0.25, 0.0, 1.0);
    var out: vec3<f32>;
    let mode = u32(round(fx.p0.x));
    if (mode == 1u) {
        // False colour: infrared shown as red, red as green, green as blue.
        out = vec3(ir, rgb.r, rgb.g);
    } else if (mode == 2u) {
        // Golden: foliage warm gold, the rest in its own colours, darker skies.
        let green = clamp((rgb.g - max(rgb.r, rgb.b) * 0.8) * 3.0, 0.0, 1.0) * min(foliage, 1.5);
        out = mix(rgb * vec3(1.0, 1.0, 1.0 - sky * 0.3), vec3(1.0, 0.78, 0.35) * (0.4 + ir * 0.7), clamp(green, 0.0, 1.0));
    } else {
        out = vec3(ir);
    }
    let mixed = mix(rgb, clamp(out, vec3(0.0), vec3(1.0)), fx.p1.z / 100.0);
    return vec4(to_linear(mixed) * c.a, c.a);
}

@fragment
fn fs_xray(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    if (c.a <= 0.0) {
        return vec4(0.0);
    }
    let size = max(fx.extra.zw, vec2(1.0));
    let e = 1.5 / size;
    // Brightness here and round about (for the edges, a Sobel).
    let b = vl_bright(c);
    let s = array<vec2<f32>, 8>(vec2(-1.0, -1.0), vec2(0.0, -1.0), vec2(1.0, -1.0), vec2(-1.0, 0.0), vec2(1.0, 0.0), vec2(-1.0, 1.0), vec2(0.0, 1.0), vec2(1.0, 1.0));
    var v: array<f32, 8>;
    for (var k = 0; k < 8; k++) {
        v[k] = vl_bright(textureSampleLevel(src, src_sampler, to_tex(clamp(in.uv + s[k] * e, vec2(0.0), vec2(1.0))), 0.0));
    }
    let gx = (v[2] + 2.0 * v[4] + v[7]) - (v[0] + 2.0 * v[3] + v[5]);
    let gy = (v[5] + 2.0 * v[6] + v[7]) - (v[0] + 2.0 * v[1] + v[2]);
    let edge = clamp(length(vec2(gx, gy)) * 0.5, 0.0, 1.0);
    var x = select(b, 1.0 - b, fx.p0.y > 0.5);
    x = clamp((x - 0.5) * fx.p0.w / 100.0 + 0.5, 0.0, 1.0);
    x += edge * fx.p0.z / 100.0 * 0.8;
    // Bloom from the bright parts round it.
    let around = vl_bright(vl_ring(in.uv, 7.0));
    x += max(select(around, 1.0 - around, fx.p0.y > 0.5) - 0.55, 0.0) * fx.p1.x / 100.0;
    x += vl_grain(in.uv, 1.3, u32(max(fx.p1.w, 0.0)) * 3571u + 5u) * fx.p1.y / 100.0 * 0.2;
    x = clamp(x * fx.p1.z / 100.0, 0.0, 1.5);
    let col = rgb_of(fx.p0.x) * to_linear(vec3(min(x, 1.0))) + vec3(max(x - 1.0, 0.0));
    return vec4(col * c.a, c.a);
}

// Spotlight: p0 = (centre x %, y %, size % of the height, roundness %), p1 =
// (angle°, softness %, intensity %, colour), p2 = (darken %, beam %, lamp x %,
// lamp y %), p3 = (pattern, pattern size %, pattern turn°).
fn sp_pattern(kind: u32, q: vec2<f32>) -> f32 {
    if (kind == 1u) {
        // A window: four panes and their bars.
        let f = abs(fract(q * 1.2 + 0.5) - 0.5);
        return smoothstep(0.03, 0.06, min(f.x, f.y));
    }
    if (kind == 2u) {
        // Light through leaves: dappled.
        let n = vnoise(91u, q * 3.0) * 0.65 + vnoise(17u, q * 7.0) * 0.35;
        return smoothstep(0.45, 0.6, n);
    }
    if (kind == 3u) {
        return smoothstep(-0.2, 0.2, sin(q.x * 12.0));
    }
    if (kind == 4u) {
        let f = fract(q * 3.0) - 0.5;
        return 1.0 - smoothstep(0.25, 0.32, length(f));
    }
    if (kind == 5u) {
        return smoothstep(-0.3, 0.3, sin(length(q) * 18.0));
    }
    return 1.0;
}

@fragment
fn fs_spotlight(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    let size = max(fx.extra.zw, vec2(1.0));
    let p = in.uv * size;
    let centre = fx.p0.xy / 100.0 * size;
    let r = max(fx.p0.z / 100.0 * size.y * 0.5, 1.0);
    let a = radians(fx.p1.x);
    let d = p - centre;
    let rot = vec2(cos(a) * d.x + sin(a) * d.y, -sin(a) * d.x + cos(a) * d.y);
    let q = rot / (r * vec2(1.0, max(fx.p0.w / 100.0, 0.05)));
    let soft = max(fx.p1.y / 100.0, 0.01);
    var pool = 1.0 - smoothstep(1.0 - soft, 1.0, length(q));
    // The pattern, in the pool's own frame.
    let g = radians(fx.p3.z);
    let qg = vec2(cos(g) * q.x + sin(g) * q.y, -sin(g) * q.x + cos(g) * q.y) * 100.0 / max(fx.p3.y, 1.0);
    pool *= sp_pattern(u32(round(fx.p3.x)), qg);
    let col = rgb_of(fx.p1.w);
    let lit = c.rgb * (1.0 - fx.p2.x / 100.0) + c.rgb * col * pool * fx.p1.z / 100.0;
    // The beam: a cone from the lamp to the pool, faint in the air.
    let lamp = fx.p2.zw / 100.0 * size;
    let axis = centre - lamp;
    let l2 = max(dot(axis, axis), 1.0);
    let t = dot(p - lamp, axis) / l2;
    let side = abs(axis.x * (p.y - lamp.y) - axis.y * (p.x - lamp.x)) / sqrt(l2);
    let width = mix(r * 0.08, r, clamp(t, 0.0, 1.0));
    let beam = (1.0 - smoothstep(0.5, 1.0, side / max(width, 1.0))) * smoothstep(0.0, 0.05, t) * (1.0 - smoothstep(0.95, 1.05, t)) * mix(1.0, 0.5, clamp(t, 0.0, 1.0));
    let air = col * beam * fx.p2.y / 100.0 * 0.35;
    return vec4(lit + air * c.a, c.a);
}

// Gleam: the bright parts' light gathered along each spoke of a star.
fn gl_bright(uv: vec2<f32>) -> vec3<f32> {
    let c = textureSampleLevel(src, src_sampler, to_tex(clamp(uv, vec2(0.0), vec2(1.0))), 0.0) * inside(uv);
    let y = dot(to_display(unpremul(c)), LUMA);
    let t = fx.p0.x / 100.0;
    let w = smoothstep(t, t + 0.08, y);
    let own = mix(vec3(1.0), c.rgb / max(max3(c.rgb), 1e-4), fx.p1.z / 100.0);
    return own * w * c.a;
}

@fragment
fn fs_gleam(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    let size = max(fx.extra.zw, vec2(1.0));
    let lines = i32(round(fx.p0.z)) + 2;
    let len = max(fx.p0.y, 0.0);
    let sharp = 1.0 + fx.p2.y / 100.0 * 4.0;
    let steps = 20;
    var acc = vec3(0.0);
    for (var k = 0; k < lines; k++) {
        let a = radians(fx.p0.w) + f32(k) * 3.14159265 / f32(lines);
        let dir = vec2(cos(a), sin(a)) / size;
        for (var s = 1; s <= steps; s++) {
            let f = f32(s) / f32(steps);
            let w = pow(1.0 - f, sharp);
            var tint = vec3(1.0);
            if (fx.p1.w > 0.0) {
                tint = mix(vec3(1.0), hue_rgb(fract(f * 1.3) * 360.0), fx.p1.w / 100.0);
            }
            acc += (gl_bright(in.uv + dir * f * len) + gl_bright(in.uv - dir * f * len)) * w * tint;
        }
    }
    var glints = acc / f32(steps) * rgb_of(fx.p1.y) * fx.p1.x / 100.0 * 0.6;
    if (fx.p2.x > 0.0) {
        let cell = vec2<u32>(ibits(in.uv.x * size.x / 24.0), ibits(in.uv.y * size.y / 24.0));
        let ph = rnd3(cell.x, cell.y, 77u) * 6.283;
        glints *= 1.0 - fx.p2.x / 100.0 * (0.5 + 0.5 * sin(fx.extra.y * 6.0 + ph));
    }
    if (u32(round(fx.p2.z)) == 1u) {
        return vec4(glints, clamp(max3(glints), 0.0, 1.0));
    }
    return vec4(c.rgb + glints, max(c.a, clamp(max3(glints), 0.0, 1.0)));
}

fn ha_high(uv: vec2<f32>) -> f32 {
    let c = textureSampleLevel(src, src_sampler, to_tex(clamp(uv, vec2(0.0), vec2(1.0))), 0.0) * inside(uv);
    let y = dot(to_display(unpremul(c)), LUMA) * c.a;
    let t = fx.p0.x / 100.0;
    return smoothstep(t - 0.05, t + 0.1, y);
}

@fragment
fn fs_halation(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    let size = max(fx.extra.zw, vec2(1.0));
    let r = max(fx.p0.y, 0.0);
    let here = ha_high(in.uv);
    // The highlights spread over three rings (closer counting more).
    var spread = here * 0.2;
    var total = 0.2;
    for (var ring = 1; ring <= 3; ring++) {
        let rr = r * f32(ring) / 3.0;
        let w = 1.0 / f32(ring);
        for (var k = 0; k < 12; k++) {
            let a = (f32(k) + f32(ring) * 0.33) * 0.5235988;
            spread += ha_high(in.uv + vec2(cos(a), sin(a)) * rr / size) * w;
            total += w;
        }
    }
    spread /= total;
    let glow = max(spread - here * fx.p1.x / 100.0, 0.0);
    let dark = 1.0 - clamp(dot(to_display(unpremul(c)), LUMA), 0.0, 1.0) * 0.6;
    let add = rgb_of(fx.p0.w) * glow * fx.p0.z / 100.0 * dark;
    return vec4(c.rgb + add, max(c.a, clamp(max3(add), 0.0, 1.0)));
}

// A regular n-gon's "radius" at angle a (1 at its corners' circle), for
// shaped ghosts; n = 0: round.
fn cf_shape(q: vec2<f32>, n: f32) -> f32 {
    if (n < 3.0) {
        return length(q);
    }
    let seg = 6.2831853 / n;
    let a = atan2(q.y, q.x) + 6.2831853;
    let k = a - seg * floor(a / seg) - seg * 0.5;
    return length(q) * cos(k) / cos(seg * 0.5);
}

@fragment
fn fs_custom_flare(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    let aspect = max(fx.extra.x, 1e-3);
    let light = fx.p0.xy / 100.0;
    // Distances in heights (so round things are round).
    let d = (in.uv - light) * vec2(aspect, 1.0);
    let r = length(d);
    let tint = rgb_of(fx.p0.w);
    var col = vec3(0.0);
    // Glow.
    let gs = max(fx.p1.x / 100.0 * 0.25, 1e-3);
    col += tint * exp(-r * r / (gs * gs)) * fx.p1.y / 100.0;
    col += tint * 0.15 * exp(-r / (gs * 3.0)) * fx.p1.y / 100.0;
    // Starburst rays.
    let rays = round(fx.p1.z);
    if (rays >= 1.0) {
        let a = atan2(d.y, d.x);
        let spoke = pow(abs(cos(a * rays * 0.5 + fx.p4.w)), 60.0);
        let thin = pow(abs(cos(a * rays * 0.5 * 1.7 + 1.3 + fx.p4.w)), 120.0) * 0.5;
        let rl = max(fx.p1.w / 100.0 * 0.6, 1e-3);
        col += tint * (spoke + thin) * exp(-r / rl) * fx.p2.x / 100.0;
    }
    // Streak (across).
    let sl = max(fx.p2.y / 100.0 * 0.6, 1e-3);
    col += rgb_of(fx.p2.w) * exp(-abs(d.y) * 220.0) * exp(-abs(d.x) / sl) * fx.p2.z / 100.0;
    // Halo ring.
    let hr = fx.p3.x / 100.0 * 0.5;
    let hw = max(hr * 0.08, 0.003);
    let ring = exp(-pow((r - hr) / hw, 2.0));
    var halo_col = tint;
    if (fx.p3.z > 0.5) {
        halo_col = hue_rgb(clamp(0.5 + (r - hr) / (hw * 4.0), 0.0, 1.0) * 290.0);
    }
    col += halo_col * ring * fx.p3.y / 100.0 * 0.6;
    // Ghosts along the line from the light through the middle.
    let axis = (vec2(0.5) - light);
    let n = i32(clamp(round(fx.p3.w), 0.0, 24.0));
    let seed = u32(max(fx.p4.w, 0.0)) * 2654435u + 3u;
    let shape = select(0.0, select(8.0, 6.0, round(fx.p4.y) < 1.5), round(fx.p4.y) >= 0.5);
    for (var i = 0; i < n; i++) {
        let k = 0.3 + rnd3(seed, u32(i), 1u) * 1.7;
        let pos = light + axis * k;
        let sz = max(fx.p4.x / 100.0 * 0.5 * (0.3 + rnd3(seed, u32(i), 2u) * 1.2), 1e-3);
        let q = (in.uv - pos) * vec2(aspect, 1.0) / sz;
        let e = cf_shape(q, shape);
        let disc = (1.0 - smoothstep(0.85, 1.0, e)) * (0.35 + 0.65 * smoothstep(0.4, 1.0, e));
        let hue = hue_rgb(rnd3(seed, u32(i), 3u) * 360.0);
        col += mix(tint, hue, 0.6) * disc * fx.p4.z / 100.0 * 0.7 * (0.4 + rnd3(seed, u32(i), 4u) * 0.6);
    }
    let light_add = col * fx.p0.z / 100.0;
    return vec4(c.rgb + light_add, c.a);
}

// Force field: p0 = (x %, y %, radius % of the height, roundness %), p1 =
// (colour, rim %, fill %, pattern), p2 = (pattern size px, pattern %, bend px,
// shimmer %), p3 = (power %, hit at s, hit angle°, hit size %).
fn ff_hex(p: vec2<f32>) -> f32 {
    // Distance to the nearest hexagon edge (0 on the edge, 0.5 at the middle).
    let s = vec2(1.0, 1.7320508);
    let a = p - s * floor(p / s + 0.5);
    let b = p - s * floor((p - s * 0.5) / s + 0.5) - s * 0.5;
    let g = select(b, a, dot(a, a) < dot(b, b));
    let q = abs(g);
    return 0.5 - max(dot(q, normalize(vec2(1.0, 1.7320508))), q.x);
}

@fragment
fn fs_force_field(in: VsOut) -> @location(0) vec4<f32> {
    let c0 = textureSample(src, src_sampler, to_tex(in.uv));
    let size = max(fx.extra.zw, vec2(1.0));
    let t = fx.extra.y;
    let power = clamp(fx.p3.x / 100.0, 0.0, 1.0);
    let p = in.uv * size;
    let centre = fx.p0.xy / 100.0 * size;
    let r = max(fx.p0.z / 100.0 * size.y * 0.5 * smoothstep(0.0, 0.6, power), 0.5);
    let q = (p - centre) / (r * vec2(1.0, max(fx.p0.w / 100.0, 0.1)));
    let d = length(q);
    if (d > 1.15) {
        return c0;
    }
    let inside = 1.0 - smoothstep(0.985, 1.0, d);
    let z = sqrt(max(1.0 - d * d, 0.0));
    // The picture behind, bent as through a glass ball.
    let bend = q * (1.0 - z) * fx.p2.z / size * inside;
    let c = textureSampleLevel(src, src_sampler, to_tex(clamp(in.uv - bend, vec2(0.0), vec2(1.0))), 0.0);
    let col = rgb_of(fx.p1.x);
    // Rim (brighter where the surface turns away) and a faint glow just outside.
    let fres = pow(1.0 - z, 3.0) * inside;
    let halo = exp(-max(d - 1.0, 0.0) * 40.0) * (1.0 - inside) * select(0.0, 1.0, d >= 1.0);
    var light = (fres + halo * 0.6) * fx.p1.y / 100.0 + fx.p1.z / 100.0 * inside;
    // The pattern over the surface, flowing slowly, brighter towards the rim.
    let ps = max(fx.p2.x, 1.0);
    let sp = q * r / (0.55 + 0.45 * z) / ps + vec2(0.0, t * 0.05 * fx.p2.w / 100.0);
    let kind = u32(round(fx.p1.w));
    var pat = 0.0;
    if (kind == 1u) {
        pat = 1.0 - smoothstep(0.0, 0.06, ff_hex(sp));
    } else if (kind == 2u) {
        let f = abs(fract(sp) - 0.5);
        pat = 1.0 - smoothstep(0.0, 0.06, 0.5 - max(f.x, f.y));
    } else if (kind == 3u) {
        pat = 0.5 + 0.5 * sin(d * r / ps * 6.2831853 - t * 3.0);
        pat = pat * pat;
    }
    let shimmer = 0.6 + 0.4 * vnoise(23u, q * 3.0 + vec2(t * 0.3 * fx.p2.w / 100.0, -t * 0.2 * fx.p2.w / 100.0));
    light += pat * fx.p2.y / 100.0 * (0.3 + 0.7 * (1.0 - z)) * shimmer * inside;
    // A hit: ripples spreading over the surface from a point on its rim.
    if (fx.p3.y >= 0.0 && t >= fx.p3.y) {
        let since = t - fx.p3.y;
        let ha = radians(fx.p3.z);
        let hit = vec2(cos(ha), sin(ha)) * 0.92;
        let dh = length(q - hit);
        let ring = since * 2.2 * fx.p3.w / 100.0;
        let w = 0.08 * fx.p3.w / 100.0 + 0.02;
        let wave = exp(-pow((dh - ring) / w, 2.0)) * exp(-since * 2.5);
        let flash = exp(-dh * dh / (w * w * 4.0)) * exp(-since * 8.0) * 2.0;
        light += (wave * 2.5 + flash) * inside * (0.6 + pat);
    }
    light *= power;
    let base = mix(c0, c, inside);
    return vec4(base.rgb + col * light * max(base.a, 0.0) + col * light * (1.0 - base.a) * 0.7, max(base.a, clamp(light, 0.0, 1.0)));
}

// Teleport: p0 = (in/out, start s, takes s, style), p1 = (colour, glow %,
// sparkles %, column width %), p2.x = seed. `a` is how far it has appeared.
@fragment
fn fs_teleport(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    let size = max(fx.extra.zw, vec2(1.0));
    let t = fx.extra.y;
    let p = clamp((t - fx.p0.y) / max(fx.p0.z, 0.01), 0.0, 1.0);
    let a = select(p, 1.0 - p, round(fx.p0.x) >= 0.5);
    let col = rgb_of(fx.p1.x);
    let glow = fx.p1.y / 100.0;
    let seed = u32(max(fx.p2.x, 0.0)) * 7919u + 1u;
    let style = u32(round(fx.p0.w));
    let px = in.uv * size;
    // The layer's shape, a little spread (for light round it).
    var shape = c.a;
    for (var k = 0; k < 8; k++) {
        let ang = f32(k) * 0.7853982;
        shape = max(shape, textureSampleLevel(src, src_sampler, to_tex(clamp(in.uv + vec2(cos(ang), sin(ang)) * 6.0 / size, vec2(0.0), vec2(1.0))), 0.0).a * 0.6);
    }
    var shown = a;
    var light = 0.0;
    if (style == 0u) {
        // Sparkle column: the layer shimmers in, cell by cell.
        let cell = floor(px / 3.0);
        let n = rnd3(ibits(cell.x), ibits(cell.y), seed);
        shown = smoothstep(n - 0.15, n + 0.15, a * 1.3 - 0.15);
        light = (1.0 - abs(a * 2.0 - 1.0)) * shape * 0.7;
    } else if (style == 1u) {
        // Scan line: a bright line sweeping up, the layer below it.
        let line = 1.0 - in.uv.y;
        let edge = a * 1.1 - 0.05;
        shown = 1.0 - smoothstep(edge - 0.01, edge + 0.01, line);
        light = exp(-pow((line - edge) * 60.0, 2.0)) * 1.5 * shape + (1.0 - shown) * shape * 0.25 * (0.5 + 0.5 * sin(px.y * 1.2 + t * 20.0));
    } else if (style == 2u) {
        // Pixel dissolve: blocks appearing at random.
        let cell = floor(px / 8.0);
        let n = rnd3(ibits(cell.x), ibits(cell.y), seed + 3u);
        shown = step(n, a);
        light = (1.0 - smoothstep(0.0, 0.08, abs(n - a))) * shape * 1.2;
    } else {
        // Pillar of light: a column of light flaring and fading as it comes in.
        let cx = 0.5;
        let w = max(fx.p1.w / 100.0 * 0.5, 0.01);
        let col_d = abs(in.uv.x - cx) / w;
        let pillar = exp(-col_d * col_d * 3.0) * sin(clamp(a, 0.0, 1.0) * 3.14159);
        shown = smoothstep(0.35, 0.85, a);
        light = pillar * 1.2 + (1.0 - shown) * shape * a;
    }
    // Sparkles over the shape while it's coming.
    let busy = 1.0 - abs(a * 2.0 - 1.0);
    if (fx.p1.z > 0.0 && busy > 0.0) {
        let g = 10.0;
        let cell = floor(px / g);
        let r = rnd3(ibits(cell.x), ibits(cell.y), seed + 9u);
        let ph = r * 6.2831853 + t * (6.0 + r * 8.0);
        let o = (vec2(rnd3(ibits(cell.x), ibits(cell.y), seed + 11u), rnd3(ibits(cell.x), ibits(cell.y), seed + 13u)) * 0.6 + 0.2) * g;
        let q = abs(px - (cell * g + o));
        let star = exp(-q.x * 1.2) * exp(-q.y * 0.25) + exp(-q.y * 1.2) * exp(-q.x * 0.25);
        let on = select(0.0, 1.0, r < fx.p1.z / 100.0 * 0.6) * max(sin(ph), 0.0);
        light += star * on * busy * shape * 1.5;
    }
    let body = c * shown;
    let add = col * light * glow;
    return vec4(body.rgb + add, max(body.a, clamp(max(add.r, max(add.g, add.b)), 0.0, 1.0)));
}
