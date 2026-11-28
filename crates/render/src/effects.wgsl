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
    // Params 20–23 (the few effects with that many).
    p5: vec4<f32>,
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

// The frame so far for an adjustment layer, when the frame holds gamma-encoded display
// values: decoded to premultiplied linear light, as effects work in.
@fragment
fn fs_from_gamma(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    if (c.a <= 1e-6) {
        return vec4(0.0);
    }
    let s = max(c.rgb / c.a, vec3(0.0));
    let lin = select(pow((s + 0.055) / 1.055, vec3(2.4)), s / 12.92, s <= vec3(0.04045));
    return vec4(lin * c.a, c.a);
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
// The shake at time `secs`: (x, y, turn), each about −1–1 before scaling.
// Handheld: a slow wander and a quicker jitter; walking adds the bob of each
// step and a sway; a vehicle adds a buzz and now and then a bump; an
// earthquake is a fast, heavy rumble. A jolt makes it all bigger for a moment.
fn shake_at(secs: f32) -> vec3<f32> {
    let seed = seed_of(fx.p1.z);
    let t = secs * fx.p0.z;
    let rough = fx.p0.w / 100.0;
    var n = vec3(
        wander(seed, t) + rough * 0.5 * wander(seed + 11u, t * 2.7),
        wander(seed + 1u, t * 1.1) + rough * 0.5 * wander(seed + 12u, t * 2.9),
        wander(seed + 2u, t * 0.9) + rough * 0.5 * wander(seed + 13u, t * 2.3),
    ) / (1.0 + rough * 0.5);
    let style = u32(round(fx.p1.w));
    if (style == 1u) {
        // Walking: two steps a second (at Speed 3), the camera dipping on each.
        let step_ = secs * fx.p0.z * 0.6667 * 3.14159;
        n = n * 0.5 + vec3(0.45 * sin(step_ * 0.5), 0.8 * (abs(sin(step_)) - 0.6), 0.4 * sin(step_ * 0.5 + 0.7));
    } else if (style == 2u) {
        let buzz = vec3(wander(seed + 21u, t * 9.0), wander(seed + 22u, t * 10.0), wander(seed + 23u, t * 8.0));
        let bump = pow(max(wander(seed + 24u, secs * 0.8), 0.0), 6.0) * 4.0;
        n = n * 0.6 + buzz * 0.35 + vec3(0.0, bump, bump * 0.3);
    } else if (style == 3u) {
        let rumble = vec3(wander(seed + 31u, t * 6.0), wander(seed + 32u, t * 6.5), wander(seed + 33u, t * 5.5));
        n = n * 0.4 + rumble * 0.9;
    }
    if (fx.p2.z >= 0.0 && secs >= fx.p2.z) {
        n *= 1.0 + fx.p2.w / 100.0 * exp(-(secs - fx.p2.z) * 4.0);
    }
    return n * vec3(fx.p2.x / 100.0, fx.p2.y / 100.0, 1.0);
}

fn shake_sample(in_uv: vec2<f32>, n: vec3<f32>) -> vec4<f32> {
    let size = fx.extra.zw;
    let off = n.xy * fx.p0.x;
    let a = n.z * radians(fx.p0.y);
    let zoom = max(fx.p1.x / 100.0, 0.01);
    // Where this pixel comes from in the unshaken picture.
    let p = in_uv * size - size * 0.5 - off;
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

@fragment
fn fs_shake(in: VsOut) -> @location(0) vec4<f32> {
    let secs = fx.extra.y;
    let blur = clamp(fx.p3.x / 100.0, 0.0, 1.0);
    if (blur <= 0.0) {
        return shake_sample(in.uv, shake_at(secs));
    }
    // Motion blur: the shake through half a frame at 24 a second, averaged.
    var acc = vec4(0.0);
    for (var i = 0; i < 8; i++) {
        let k = (f32(i) + 0.5) / 8.0;
        acc += shake_sample(in.uv, shake_at(secs - k * blur / 48.0));
    }
    return acc / 8.0;
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

// ---- HUD reticle styles (elements 5 on): drawn from distances in units of the radius ----

// What's been drawn so far at this pixel: x = ink, y = glow.
var<private> HK: vec2<f32>;
// The radius (px), line width (px), glow reach (px), and whether this pixel is drawn on yet.
var<private> HR: f32;
var<private> HW: f32;
var<private> HG: f32;
var<private> HREV: f32;

// A line of `wm` line widths along distance `du` (units of the radius), at strength `k`.
fn hk(du: f32, wm: f32, k: f32) {
    hk_px(du * HR - HW * wm * 0.5, k);
}

// A band `wu` units wide (it scales with the size; never thinner than a line).
fn hkb(du: f32, wu: f32, k: f32) {
    hk_px(du * HR - max(wu * HR, HW) * 0.5, k);
}

// Ink where the edge distance `e` (px) is below 0, glow falling off beyond it.
fn hk_px(e: f32, k: f32) {
    let s = k * HREV;
    HK.x = max(HK.x, (1.0 - smoothstep(-0.6, 0.6, e)) * s);
    HK.y = 1.0 - (1.0 - HK.y) * (1.0 - exp(-max(e, 0.0) / HG) * s);
}

// Ink at a coverage `c` (0..1) without an edge distance (ticks, glyphs).
fn hk_cov(c: f32, k: f32) {
    let s = clamp(c, 0.0, 1.0) * k * HREV;
    HK.x = max(HK.x, s);
    HK.y = 1.0 - (1.0 - HK.y) * (1.0 - s * 0.6);
}

fn hs_seg(q: vec2<f32>, a: vec2<f32>, b: vec2<f32>) -> f32 {
    let pa = q - a;
    let ba = b - a;
    let h = clamp(dot(pa, ba) / max(dot(ba, ba), 1e-8), 0.0, 1.0);
    return length(pa - ba * h);
}

// Turns clockwise from the top, 0..1.
fn hs_ang(q: vec2<f32>) -> f32 {
    return fract(atan2(q.x, -q.y) / 6.2831853 + 1.0);
}

fn hs_dir(a: f32) -> vec2<f32> {
    return vec2(sin(a * 6.2831853), -cos(a * 6.2831853));
}

// Distance to an arc of radius `rad` from `a0` turns clockwise for `len` turns.
fn hs_arc(q: vec2<f32>, rad: f32, a0: f32, len: f32) -> f32 {
    let rel = fract(hs_ang(q) - a0);
    if (rel <= len) {
        return abs(length(q) - rad);
    }
    return min(length(q - hs_dir(a0) * rad), length(q - hs_dir(a0 + len) * rad));
}

// Turned by `turns` clockwise.
fn hs_rot(q: vec2<f32>, turns: f32) -> vec2<f32> {
    let a = -turns * 6.2831853;
    let c = cos(a);
    let s = sin(a);
    return vec2(c * q.x - s * q.y, s * q.x + c * q.y);
}

// Distance to a box's outline (half size `h`).
fn hs_box(q: vec2<f32>, h: vec2<f32>) -> f32 {
    let d = abs(q) - h;
    return abs(length(max(d, vec2(0.0))) + min(max(d.x, d.y), 0.0));
}

// Distance to the outline of a hexagon with corners left and right at `s`
// (in the first quadrant: `a` folded with abs).
fn hs_hex(a: vec2<f32>, s: f32) -> f32 {
    let v0 = vec2(1.0, 0.0) * s;
    let v1 = vec2(0.5, 0.866) * s;
    return min(hs_seg(a, v0, v1), hs_seg(a, v1, vec2(0.0, 0.866 * s)));
}

// `n` arcs round a ring, each `len` turns, the first starting at `a0`.
fn hk_arcs(q: vec2<f32>, rad: f32, n: f32, a0: f32, len: f32, wm: f32, band: bool) {
    let step_a = 1.0 / max(n, 1.0);
    // The arc this pixel's angle is nearest.
    let i = floor(fract(hs_ang(q) - a0 + (step_a - len) * 0.5) / step_a);
    let d = hs_arc(q, rad, a0 + i * step_a, len);
    if (band) {
        hkb(d, wm, 1.0);
    } else {
        hk(d, wm, 1.0);
    }
}

// `n` arcs round a ring like hk_arcs, keeping only those within `sides` turns of the
// left and right (the rest left out), so each pixel glows from its own nearest arc.
fn hk_arcs_sides(q: vec2<f32>, rad: f32, n: f32, len: f32, wu: f32, sides: f32) {
    let step_a = 1.0 / max(n, 1.0);
    let i = floor(fract(hs_ang(q) + (step_a - len) * 0.5) / step_a);
    let mid = i * step_a + len * 0.5;
    let keep = step(abs(fract(mid * 2.0 + 0.5) - 0.5), sides * 2.0);
    hkb(hs_arc(q, rad, i * step_a, len), wu, keep);
}

// Distance to dashes along u (0 to `hi`), `n` a unit, each `duty` of its step; v across.
fn hs_dash(u: f32, v: f32, n: f32, duty: f32, hi: f32) -> f32 {
    let uu = clamp(u, 0.0, hi);
    let c = (floor(uu * n) + 0.5) / n;
    let h = duty * 0.5 / n;
    return length(vec2(max(abs(u - c) - h, 0.0), v));
}

// The small + in the middle.
fn hk_plus(q: vec2<f32>) {
    let s = 0.07;
    hk(min(hs_seg(q, vec2(-s, 0.0), vec2(s, 0.0)), hs_seg(q, vec2(0.0, -s), vec2(0.0, s))), 0.6, 1.0);
}

// Three prongs from the middle (a Y), from `r0` to `r1`, turned by `spin`.
fn hk_prongs(q: vec2<f32>, r0: f32, r1: f32, spin: f32) {
    for (var k = 0; k < 3; k++) {
        let d = hs_dir(f32(k) / 3.0 + spin);
        hk(hs_seg(q, d * r0, d * r1), 0.8, 1.0);
    }
}

// Ticks across a ring between `r0` and `r1`, `n` round, each a fraction `on` of its step,
// only where `mask` is set.
fn hk_ticks(q: vec2<f32>, a: f32, r0: f32, r1: f32, n: f32, on: f32, mask: f32) {
    let l = length(q);
    let c = step(fract(a * n), on) * step(r0, l) * step(l, r1) * mask;
    hk_cov(c, 0.9);
}

// Readout text: rows of tiny made-up glyphs in a box, changing now and then.
fn hk_text(q: vec2<f32>, lo: vec2<f32>, hi: vec2<f32>, seed: u32, t: f32, amount: f32) {
    if (q.x < lo.x || q.y < lo.y || q.x > hi.x || q.y > hi.y) {
        return;
    }
    let ch = max(0.055, 6.0 / max(HR, 1.0));
    let cell = vec2(ch * 0.7, ch * 1.5);
    let g = (q - lo) / cell;
    let row = u32(min(floor(g.y), 4096.0));
    let col = u32(min(floor(g.x), 4096.0));
    let f = fract(g);
    let cols = (hi.x - lo.x) / cell.x;
    let len = floor(cols * (0.25 + 0.75 * rnd3(seed, row, 7u)) * clamp(amount, 0.2, 1.0));
    if (f32(col) >= len || rnd3(seed, row, 11u) < 0.15) {
        return;
    }
    // A 3 × 5 glyph in the top of the cell, with a gap after it.
    let gx = f.x / 0.8;
    let gy = f.y / 0.66;
    if (gx >= 1.0 || gy >= 1.0) {
        return;
    }
    let tick = u32(floor(max(t, 0.0) * 2.0 + rnd3(seed, row, 8u) * 7.0)) % 64u;
    let changes = select(0u, tick, rnd3(seed, row * 31u + col, 9u) < 0.25);
    let bit = u32(gx * 3.0) + u32(gy * 5.0) * 3u;
    let lit = step(0.45, rnd3(seed + changes * 7919u, row * 131u + col, bit));
    hk_cov(lit, 0.85);
}

// Bars rising and falling between `lo` and `hi` (their bottoms on hi.y).
fn hk_bars(q: vec2<f32>, lo: vec2<f32>, hi: vec2<f32>, n: f32, seed: u32, t: f32) {
    if (q.x < lo.x || q.y < lo.y || q.x > hi.x || q.y > hi.y) {
        return;
    }
    let g = (q.x - lo.x) / (hi.x - lo.x) * n;
    let i = floor(g);
    let lvl = 0.2 + 0.8 * vnoise(seed, vec2(i * 0.9, t * 1.5));
    let top = hi.y - (hi.y - lo.y) * lvl;
    hk_cov(step(fract(g), 0.6) * step(top, q.y), 0.9);
}

// A wireframe of `n` points wandering slowly about `c` (half size `h`), most pairs joined.
fn hk_wire(q: vec2<f32>, c: vec2<f32>, h: vec2<f32>, n: u32, seed: u32, t: f32) {
    var pts: array<vec2<f32>, 10>;
    let m = min(n, 10u);
    for (var i = 0u; i < m; i++) {
        let a = vec2(rnd3(seed, i, 21u), rnd3(seed, i, 22u)) * 2.0 - 1.0;
        let drift = vec2(vnoise(seed + i, vec2(t * 0.3, 1.0)), vnoise(seed + i, vec2(2.0, t * 0.3))) - 0.5;
        pts[i] = c + (a * 0.85 + drift * 0.3) * h;
    }
    var d = 1e6;
    for (var i = 0u; i < m; i++) {
        for (var j = i + 1u; j < m; j++) {
            if (rnd3(seed, i * 16u + j, 23u) < 0.55) {
                d = min(d, hs_seg(q, pts[i], pts[j]));
            }
        }
    }
    hk(d, 0.5, 0.9);
}

// The outline of an ellipse (half sizes `h`), near enough.
fn hs_ellipse(q: vec2<f32>, h: vec2<f32>) -> f32 {
    let k = length(q / h);
    return abs(k - 1.0) * min(h.x, h.y);
}

// The reticle style `e` (5 on) at `q` (units of the radius from the centre, y down).
fn hud_shape(e: u32, q: vec2<f32>, spin: f32, detail: f32, seed: u32, t: f32) {
    let a = abs(q);
    let l = length(q);
    let ang = hs_ang(q);
    let rq = hs_rot(q, spin);
    let ra = hs_ang(rq);
    let nt = floor(mix(24.0, 96.0, detail));
    switch (e) {
        case 5u: {
            // Scope: a ring broken at the cross, the cross running past it, dashes inside.
            hk_arcs(rq, 0.8, 4.0, 0.035, 0.18, 2.2, false);
            hk(min(hs_seg(a, vec2(0.33, 0.0), vec2(1.08, 0.0)), hs_seg(a, vec2(0.0, 0.33), vec2(0.0, 1.08))), 2.4, 1.0);
            hk(min(hs_dash(a.x, a.y, 14.0, 0.55, 0.3), hs_dash(a.y, a.x, 14.0, 0.55, 0.3)), 0.8, 1.0);
        }
        case 6u: {
            // Double-ring scope: a heavy ring, two thin ones, heavy bars into the middle.
            hk_arcs(q, 0.95, 4.0, 0.03, 0.19, 0.06, true);
            hk_arcs(q, 0.82, 4.0, 0.025, 0.2, 1.0, false);
            hk_arcs(rq, 0.6, 4.0, 0.06, 0.13, 0.8, false);
            hkb(min(hs_seg(a, vec2(0.14, 0.0), vec2(0.62, 0.0)), hs_seg(a, vec2(0.0, 0.14), vec2(0.0, 0.62))), 0.05, 1.0);
            hk(min(hs_dash(a.x, a.y, 20.0, 0.5, 0.12), hs_dash(a.y, a.x, 20.0, 0.5, 0.12)), 0.6, 1.0);
        }
        case 7u: {
            // Sniper: a long cross, heavier at its ends, broken rings, a little ring in the middle.
            hk(min(hs_seg(a, vec2(0.13, 0.0), vec2(1.15, 0.0)), hs_seg(a, vec2(0.0, 0.13), vec2(0.0, 1.15))), 1.4, 1.0);
            hk(min(hs_seg(a, vec2(0.62, 0.0), vec2(1.15, 0.0)), hs_seg(a, vec2(0.0, 0.62), vec2(0.0, 1.15))), 3.0, 1.0);
            hk_arcs(q, 0.9, 4.0, 0.03, 0.19, 1.2, false);
            hk_arcs(rq, 0.75, 4.0, 0.06, 0.13, 1.0, false);
            hk_arcs(q, 0.14, 4.0, 0.05, 0.15, 1.4, false);
        }
        case 8u: {
            // Dial: a ring with scales across it, a ring in the middle, four bent chevrons.
            hk(abs(l - 0.95), 2.0, 1.0);
            hkb(abs(l - 0.11), 0.04, 1.0);
            hk(min(hs_seg(a, vec2(0.16, 0.0), vec2(0.85, 0.0)), hs_seg(q, vec2(0.0, -0.9), vec2(0.0, -0.2))), 0.7, 1.0);
            hk(hs_seg(q, vec2(0.0, 0.2), vec2(0.0, 0.9)), 0.7, 1.0);
            let tx = step(fract(a.x * 16.0), 0.18) * step(a.y, 0.035) * step(0.16, a.x) * step(a.x, 0.85);
            let long_y = mix(0.04, 0.12, step(fract(q.y * 4.0), 0.25));
            let ty = step(fract(q.y * 16.0), 0.18) * step(0.2, a.y) * step(a.y, 0.9) * step(0.0, q.x * sign(-q.y)) * step(a.x, long_y);
            hk_cov(tx + ty, 0.9);
            let ca = abs(rq);
            hk(min(hs_seg(ca, vec2(0.3, 0.74), vec2(0.45, 0.42)), hs_seg(ca, vec2(0.45, 0.42), vec2(0.8, 0.42))), 1.6, 1.0);
        }
        case 9u: {
            // Star compass: a four-point star, a ring, four heavy arcs turning round it.
            let s = min(hs_seg(a, vec2(0.0, 1.0), vec2(0.2, 0.2)), hs_seg(a, vec2(0.2, 0.2), vec2(1.0, 0.0)));
            hk(s, 2.4, 1.0);
            hk(abs(l - 0.6), 1.6, 1.0);
            hk_arcs(rq, 0.88, 4.0, 0.125 - 0.075, 0.15, 0.09, true);
            hk_plus(q);
        }
        case 10u: {
            // Diamond compass: a diamond, its points marked, rings inside.
            hk(abs(a.x + a.y - 1.0) * 0.7071, 1.8, 1.0);
            hk(min(hs_seg(a, vec2(0.0, 1.0), vec2(0.28, 0.56)), hs_seg(a, vec2(1.0, 0.0), vec2(0.56, 0.28))), 1.4, 1.0);
            hk(abs(l - 0.55), 1.8, 1.0);
            hk_arcs(rq, 0.42, 32.0, 0.0, 0.6 / 32.0, 0.8, false);
            hk_plus(q);
        }
        case 11u: {
            // Notched square: a frame with heavy, stepped corners.
            hk(hs_box(q, vec2(0.78)), 1.6, 1.0);
            let c = min(hs_seg(a, vec2(0.55, 0.84), vec2(0.84, 0.84)), hs_seg(a, vec2(0.84, 0.55), vec2(0.84, 0.84)));
            hkb(c, 0.06, 1.0);
            hk_plus(q);
        }
        case 12u: {
            // Framed square: a heavy frame broken near each corner, small brackets inside.
            let fr = min(min(hs_seg(a, vec2(0.0, 0.82), vec2(0.5, 0.82)), hs_seg(a, vec2(0.6, 0.82), vec2(0.82, 0.82))), min(hs_seg(a, vec2(0.82, 0.0), vec2(0.82, 0.5)), hs_seg(a, vec2(0.82, 0.6), vec2(0.82, 0.82))));
            hkb(fr, 0.05, 1.0);
            let c = min(hs_seg(a, vec2(0.45, 0.62), vec2(0.62, 0.62)), hs_seg(a, vec2(0.62, 0.45), vec2(0.62, 0.62)));
            hk(c, 1.0, 1.0);
            hk_plus(q);
        }
        case 13u: {
            // Brackets: heavy corners, thin ones inside them.
            let c = min(hs_seg(a, vec2(0.5, 0.85), vec2(0.85, 0.85)), hs_seg(a, vec2(0.85, 0.5), vec2(0.85, 0.85)));
            hkb(c, 0.07, 1.0);
            let i = min(hs_seg(a, vec2(0.45, 0.68), vec2(0.68, 0.68)), hs_seg(a, vec2(0.68, 0.45), vec2(0.68, 0.68)));
            hk(i, 1.0, 1.0);
            hk_plus(q);
        }
        case 14u: {
            // Hexagon in pieces: brackets at its corners, chevrons inside left and right.
            let v0 = vec2(1.0, 0.0);
            let v1 = vec2(0.5, 0.866);
            let b = min(min(hs_seg(a, v0, mix(v0, v1, 0.32)), hs_seg(a, v1, mix(v1, v0, 0.32))), hs_seg(a, v1, vec2(0.22, 0.866)));
            hkb(b, 0.045, 1.0);
            hk(hs_seg(a, vec2(0.78, 0.0), vec2(0.66, 0.2)), 1.6, 1.0);
            hk_plus(q);
        }
        case 15u: {
            // Hexagon: its outline, chevrons outside left and right, bars above and below.
            hk(hs_hex(a, 0.8), 1.6, 1.0);
            hkb(hs_seg(a, vec2(1.02, 0.0), vec2(0.86, 0.3)), 0.04, 1.0);
            hkb(min(hs_seg(a, vec2(0.25, 0.98), vec2(0.56, 0.98)), hs_seg(a, vec2(0.56, 0.98), vec2(0.66, 0.84))), 0.04, 1.0);
            hk_plus(q);
        }
        case 16u: {
            // Double hexagon, with the chevrons and bars.
            hkb(hs_hex(a, 0.82), 0.035, 1.0);
            hk(hs_hex(a, 0.68), 1.0, 1.0);
            hkb(hs_seg(a, vec2(1.04, 0.0), vec2(0.9, 0.3)), 0.04, 1.0);
            hkb(min(hs_seg(a, vec2(0.28, 0.98), vec2(0.58, 0.98)), hs_seg(a, vec2(0.58, 0.98), vec2(0.68, 0.84))), 0.04, 1.0);
            hk_plus(q);
        }
        case 17u: {
            // Two arcs, left and right.
            hkb(min(hs_arc(q, 1.0, 0.25 - 0.11, 0.22), hs_arc(q, 1.0, 0.75 - 0.11, 0.22)), 0.045, 1.0);
            hk_plus(q);
        }
        case 18u: {
            // Bow tie: arcs, lines in from their tops, ticks across, prongs in the middle.
            hkb(min(hs_arc(q, 1.0, 0.25 - 0.13, 0.26), hs_arc(q, 1.0, 0.75 - 0.13, 0.26)), 0.035, 1.0);
            let top = abs(hs_dir(0.12));
            hk(min(hs_seg(a, top, vec2(0.4, 0.42)), hs_seg(a, vec2(0.38, 0.0), vec2(0.76, 0.0))), 1.6, 1.0);
            hk_prongs(q, 0.0, 0.08, spin);
        }
        case 19u: {
            // Hooked arcs, a Y in the middle.
            let d = min(hs_arc(q, 0.95, 0.25 - 0.12, 0.24), hs_arc(q, 0.95, 0.75 - 0.12, 0.24));
            hkb(d, 0.035, 1.0);
            let hook = abs(hs_dir(0.13)) * 0.95;
            hkb(hs_seg(a, hook, hook + vec2(0.1, 0.0)), 0.035, 1.0);
            hkb(hs_seg(a, vec2(0.97, 0.33), vec2(0.88, 0.33)), 0.03, 1.0);
            hk_prongs(q, 0.08, 0.32, spin);
            hk_plus(q);
        }
        case 20u: {
            // An X that sweeps out into hooks.
            let d = min(min(hs_seg(a, vec2(0.1, 0.07), vec2(0.55, 0.42)), hs_seg(a, vec2(0.55, 0.42), vec2(0.68, 0.42))), hs_seg(a, vec2(0.68, 0.42), vec2(0.95, 0.62)));
            hk(d, 2.0, 1.0);
            hkb(hs_arc(a - vec2(0.82, 0.62), 0.18, 0.12, 0.18), 0.035, 1.0);
            hk_plus(q);
        }
        case 21u: {
            // An X into bracketed corners.
            let d = min(min(hs_seg(a, vec2(0.1, 0.075), vec2(0.82, 0.62)), hs_seg(a, vec2(0.82, 0.62), vec2(1.0, 0.62))), hs_seg(a, vec2(1.0, 0.62), vec2(1.0, 0.42)));
            hkb(d, 0.03, 1.0);
            hk_plus(q);
        }
        case 22u: {
            // An X with its ends run out flat.
            let d = min(hs_seg(a, vec2(0.1, 0.075), vec2(0.88, 0.68)), hs_seg(a, vec2(0.88, 0.68), vec2(1.2, 0.68)));
            hk(d, 2.4, 1.0);
            hk_plus(q);
        }
        case 23u: {
            // Wireframe with callouts: a net of points, leaders out to readouts.
            hk_wire(q, vec2(0.0), vec2(0.5, 0.6), u32(mix(6.0, 10.0, detail)), seed, t);
            let lead = min(min(hs_seg(q, vec2(-0.45, -0.5), vec2(-1.1, -0.5)), hs_seg(q, vec2(0.45, -0.2), vec2(0.6, -0.4))), min(hs_seg(q, vec2(0.6, -0.4), vec2(1.1, -0.4)), hs_seg(q, vec2(-0.3, 0.55), vec2(-0.6, 0.75))));
            hk(min(lead, min(hs_seg(q, vec2(-0.6, 0.75), vec2(-1.1, 0.75)), hs_seg(q, vec2(0.5, 0.3), vec2(1.1, 0.3)))), 0.8, 1.0);
            hk_text(q, vec2(-1.1, -0.66), vec2(-0.6, -0.55), seed, t, detail);
            hk_text(q, vec2(0.65, -0.56), vec2(1.1, -0.45), seed + 1u, t, detail);
            hk_text(q, vec2(0.6, 0.15), vec2(1.1, 0.26), seed + 2u, t, detail);
            hk_text(q, vec2(-1.1, 0.6), vec2(-0.7, 0.71), seed + 3u, t, detail);
        }
        case 24u: {
            // Wireframe hexagon, turning, with callouts.
            let turn = cos(spin * 6.2831853);
            var d = 1e6;
            for (var i = 0; i < 6; i++) {
                let pi = hs_dir(f32(i) / 6.0) * 0.6 * vec2(turn, 1.0);
                for (var j = i + 1; j < 6; j++) {
                    d = min(d, hs_seg(q, pi, hs_dir(f32(j) / 6.0) * 0.6 * vec2(turn, 1.0)));
                }
            }
            hk(d, 0.6, 1.0);
            hk(hs_hex(abs(q.yx), 0.6), 1.4, 1.0);
            let lead = min(min(hs_seg(a, vec2(0.52, 0.3), vec2(0.7, 0.55)), hs_seg(a, vec2(0.7, 0.55), vec2(1.2, 0.55))), hs_seg(a, vec2(0.62, 0.0), vec2(1.2, 0.0)));
            hk(lead, 0.8, 1.0);
            hk_text(q, vec2(0.75, -0.7), vec2(1.2, -0.6), seed, t, detail);
            hk_text(q, vec2(-1.2, -0.7), vec2(-0.75, -0.6), seed + 1u, t, detail);
            hk_text(q, vec2(0.7, 0.4), vec2(1.2, 0.5), seed + 2u, t, detail);
            hk_text(q, vec2(-1.2, -0.15), vec2(-0.7, -0.05), seed + 3u, t, detail);
        }
        case 25u: {
            // Scanner panel: wide arcs, brackets, a crossed box with an ellipse, a strip of bars.
            hkb(min(hs_arc(q, 1.35, 0.25 - 0.09, 0.18), hs_arc(q, 1.35, 0.75 - 0.09, 0.18)), 0.03, 1.0);
            hk_ticks(q, ang, 1.22, 1.28, nt * 2.0, 0.4, step(1.0, a.x));
            hk(hs_seg(a, vec2(0.85, 0.82), vec2(1.2, 0.82)), 1.6, 1.0);
            hk(min(hs_box(q, vec2(0.75, 0.45)) + 0.0, hs_seg(a, vec2(0.0), vec2(0.75, 0.45))), 0.8, 1.0);
            hk(hs_ellipse(hs_rot(q, spin * 0.1), vec2(0.3, 0.15)), 1.2, 1.0);
            let strip = step(fract(q.x * 40.0), 0.5) * step(0.62, q.y) * step(q.y, 0.7) * step(a.x, 0.55);
            hk_cov(strip, 0.9);
        }
        case 26u: {
            // Data scope: arcs, a level line with a marker, a bar graph, a gauge and readouts.
            hkb(min(hs_arc(q, 0.95, 0.25 - 0.14, 0.28), hs_arc(q, 0.95, 0.75 - 0.14, 0.28)), 0.035, 1.0);
            hk(min(hs_arc(q, 0.75, 0.25 - 0.1, 0.2), hs_arc(q, 0.75, 0.75 - 0.1, 0.2)), 1.0, 1.0);
            hk(min(hs_seg(a, vec2(0.12, 0.0), vec2(0.55, 0.0)), hs_seg(a, vec2(0.55, 0.0), vec2(0.55, 0.1))), 1.0, 1.0);
            let tri = step(q.y, 0.0) * step(-0.09, q.y) * step(a.x, (q.y + 0.09) * 0.6);
            hk_cov(tri, 1.0);
            hk_bars(q, vec2(-0.25, 0.45), vec2(0.25, 0.72), floor(mix(5.0, 12.0, detail)), seed, t);
            let lvl = 0.5 + 0.4 * sin(t * 0.7);
            let gauge = step(-1.32, q.x) * step(q.x, -1.18) * step(-0.4, q.y) * step(q.y, 0.7) * step(0.7 - 1.1 * lvl, q.y) * step(fract(q.y * 18.0), 0.6);
            hk_cov(gauge, 0.9);
            hk(hs_arc(q - vec2(-0.1, 0.0), 1.3, 0.62, 0.26), 0.7, 1.0);
            hk_text(q, vec2(1.1, -0.75), vec2(1.45, 0.75), seed, t, detail);
        }
        case 27u: {
            // Data X: arcs, a bow tie in the middle, columns of readouts.
            hkb(min(hs_arc(q, 0.95, 0.25 - 0.15, 0.3), hs_arc(q, 0.95, 0.75 - 0.15, 0.3)), 0.03, 1.0);
            let d = min(min(hs_seg(a, vec2(0.12, 0.1), vec2(0.42, 0.36)), hs_seg(a, vec2(0.42, 0.36), vec2(0.6, 0.36))), hs_seg(a, vec2(0.55, 0.0), vec2(0.85, 0.0)));
            hk(d, 1.2, 1.0);
            hk_text(q, vec2(-1.45, -0.8), vec2(-1.08, 0.8), seed, t, detail);
            hk_text(q, vec2(1.08, -0.85), vec2(1.45, 0.4), seed + 1u, t, detail);
        }
        case 28u: {
            // Data cross: heavy arcs, a turning dashed ring, an X to a dot, readouts.
            hkb(min(hs_arc(q, 0.95, 0.25 - 0.13, 0.26), hs_arc(q, 0.95, 0.75 - 0.13, 0.26)), 0.07, 1.0);
            hk_arcs(rq, 0.32, 12.0, 0.0, 0.6 / 12.0, 0.8, false);
            hk(hs_seg(a, vec2(0.08, 0.08), vec2(0.26, 0.26)), 1.0, 1.0);
            hk_cov(1.0 - smoothstep(0.045, 0.055, l), 1.0);
            hk_text(q, vec2(-1.45, -0.85), vec2(-1.05, -0.55), seed, t, detail);
            hk_text(q, vec2(-1.45, 0.55), vec2(-1.05, 0.85), seed + 1u, t, detail);
            hk_text(q, vec2(1.08, 0.75), vec2(1.45, 0.85), seed + 2u, t, detail);
        }
        case 29u: {
            // Bullseye: thin rings, a heavy ring inside, a dot.
            hk(abs(l - 0.95), 1.0, 1.0);
            if (detail >= 0.5) {
                let nd = floor(nt * 0.25);
                hk_arcs(rq, 0.85, nd, 0.0, 0.7 / nd, 1.0, false);
            } else {
                hk(abs(l - 0.85), 1.0, 1.0);
            }
            hk(abs(l - 0.76), 1.6, 1.0);
            hkb(abs(l - 0.42), 0.13, 1.0);
            hk_cov(1.0 - smoothstep(0.02, 0.03, l), 1.0);
        }
        case 30u: {
            // Heavy ring, a thin one, arcs turning inside, a dot.
            hkb(abs(l - 0.9), 0.1, 1.0);
            hk(abs(l - 0.68), 1.2, 1.0);
            hk_arcs(rq, 0.42, 2.0, 0.25 - 0.14, 0.28, 1.8, false);
            hk_cov(1.0 - smoothstep(0.025, 0.035, l), 1.0);
        }
        case 31u: {
            // Tick dial: a ring with runs of ticks, crosshair ticks, arcs winding in.
            hk(abs(l - 0.8), 1.4, 1.0);
            let runs = step(0.08, fract(ang * 4.0)) * step(fract(ang * 4.0), 0.42);
            hk_ticks(q, ang, 0.88, 0.98, nt * 1.5, 0.45, runs);
            hk(min(hs_seg(a, vec2(0.6, 0.0), vec2(1.05, 0.0)), hs_seg(a, vec2(0.0, 0.6), vec2(0.0, 1.05))), 0.8, 1.0);
            hk(hs_arc(hs_rot(q, spin * 1.3), 0.52, 0.0, 0.35), 1.4, 1.0);
            hk(hs_arc(hs_rot(q, -spin), 0.36, 0.5, 0.3), 1.4, 1.0);
            hk(hs_arc(hs_rot(q, spin * 0.6), 0.22, 0.2, 0.55), 1.4, 1.0);
            hk(abs(l - 0.06), 1.2, 1.0);
        }
        case 32u: {
            // Segment gauge: outer arcs, heavy blocks either side, a dot.
            hk_arcs(q, 1.0, 4.0, 0.06, 0.13, 1.4, false);
            hk_arcs_sides(q, 0.55, 30.0, 0.78 / 30.0, 0.17, 0.09);
            hk(min(hs_arc(q, 0.38, 0.25 - 0.08, 0.16), hs_arc(q, 0.38, 0.75 - 0.08, 0.16)), 1.0, 1.0);
            hk_cov(1.0 - smoothstep(0.05, 0.06, l), 1.0);
        }
        case 33u: {
            // Lock-on: heavy arcs turning outside, a broken ring, a cross with a gap.
            hk_arcs(rq, 0.95, 4.0, -0.075, 0.15, 0.07, true);
            hk_arcs(q, 0.55, 4.0, 0.015, 0.22, 0.05, true);
            hk(min(hs_seg(a, vec2(0.08, 0.0), vec2(0.32, 0.0)), hs_seg(a, vec2(0.0, 0.08), vec2(0.0, 0.32))), 1.4, 1.0);
        }
        default: {
            // Segmented shield: heavy banks of segments either side, a dashed ring turning, a Y.
            hk_arcs_sides(q, 0.92, 36.0, 0.8 / 36.0, 0.13, 0.16);
            hk(min(hs_arc(q, 1.03, 0.25 - 0.2, 0.4), hs_arc(q, 1.03, 0.75 - 0.2, 0.4)), 1.0, 1.0);
            hk_arcs(rq, 0.6, 18.0, 0.0, 0.55 / 18.0, 1.0, false);
            hk_prongs(q, 0.07, 0.3, 0.5);
            hk_plus(q);
        }
    }
}

// Ink `c` without an edge distance (ticks, bars, dots): it glows a little too.
fn hg(c: f32) -> f32 {
    HK.y = 1.0 - (1.0 - HK.y) * (1.0 - clamp(c, 0.0, 1.0) * 0.6);
    return c;
}

// HUD lock-on at clip time `t` (p4.y on, p4.z when, p4.w how long): x = how visible
// (hidden before it starts, fading in, blinking once locked), y = how far it has closed
// in (eased, 0–1; 1 without a lock-on), z = 1 once locked (the Locked colour).
fn hud_lock(t: f32) -> vec3<f32> {
    if (fx.p4.y < 0.5) {
        return vec3(1.0, 1.0, 0.0);
    }
    let k = clamp((t - fx.p4.z) / max(fx.p4.w, 0.01), 0.0, 1.0);
    let eased = 1.0 - pow(1.0 - k, 3.0);
    var vis = select(0.0, smoothstep(0.0, 0.12, k) * 0.5 + 0.5 * eased, t >= fx.p4.z);
    let after = t - (fx.p4.z + max(fx.p4.w, 0.01));
    if (after >= 0.0 && after < 0.6) {
        vis = step(fract(after * 5.0), 0.55);
    } else if (after >= 0.6) {
        vis = 1.0;
    }
    return vec3(vis, eased, step(0.0, after));
}

// HUD: p0 = (element, centre x %, y %, size % of height), p1 = (colour, line px, spin turns/s,
// detail %), p2 = (draw on %, opacity %, flicker %, seed), p3 = (glow %, fill %, glow size %
// of the radius, glow colour), p4.x = use the glow colour.
@fragment
fn fs_hud(in: VsOut) -> @location(0) vec4<f32> {
    let size = fx.extra.zw;
    let t = fx.extra.y;
    let p = in.uv * size;
    let c = fx.p0.yz / 100.0 * size;
    let lk = hud_lock(t);
    // Closing in: from big and turned, shrinking and turning onto the target.
    let r = max(fx.p0.w / 100.0 * size.y * 0.5, 2.0) * mix(2.6, 1.0, lk.y);
    let d = p - c;
    let dist = length(d);
    // Angle 0..1 clockwise from the top.
    let ang = fract(atan2(d.x, -d.y) / 6.2831853 + 1.0);
    let w = max(fx.p1.y, 0.3);
    let on = clamp(fx.p2.x / 100.0, 0.0, 1.0);
    let detail = fx.p1.w / 100.0;
    let spin = t * fx.p1.z + (1.0 - lk.y) * 0.4;
    let seed = u32(max(fx.p2.w, 0.0)) * 197u + 5u;
    var ink = 0.0;
    var fill = 0.0;
    HR = r;
    HW = w;
    HG = max(r * clamp(fx.p3.z, 0.5, 200.0) / 100.0, w);
    HK = vec2(0.0);
    switch (u32(round(fx.p0.x))) {
        case 0u: {
            // Reticle: an outer ring broken in four, an inner ring, a cross with a gap, ticks.
            let q = d / r;
            HREV = step(ang, on);
            hk_arcs(q, 1.0, 4.0, -0.125 + 0.01, 0.23, 1.0, false);
            HREV = step(fract(ang - spin), on);
            hk(abs(length(q) - 0.55), 0.8, 1.0);
            HREV = step(0.3, on);
            hk(min(hs_seg(abs(q), vec2(0.15, 0.0), vec2(0.85, 0.0)), hs_seg(abs(q), vec2(0.0, 0.15), vec2(0.0, 0.85))), 1.0, 1.0);
            let n = floor(mix(12.0, 72.0, detail));
            let tick = step(fract(ang * n + 0.04), 0.08) * step(r * 1.04, dist) * step(dist, r * (1.1 + 0.06 * step(fract(ang * n / 4.0), 0.25)));
            ink += hg(tick * step(ang, on));
            ink += hg((1.0 - smoothstep(w, w + 1.2, dist)) * step(0.6, on));
        }
        case 1u: {
            // Radar: rings, the sweep with its fading wake, blips that glow as it passes.
            let q = d / r;
            HREV = step(ang, on);
            for (var k = 1; k <= 4; k++) {
                hk(abs(length(q) - f32(k) / 4.0), select(0.6, 1.0, k == 4), 1.0);
            }
            HREV = on;
            hk(min(hs_seg(q, vec2(-1.0, 0.0), vec2(1.0, 0.0)), hs_seg(q, vec2(0.0, -1.0), vec2(0.0, 1.0))), 0.5, 0.5);
            hk(hs_seg(q, vec2(0.0), hs_dir(fract(spin))), 1.0, 1.0);
            let behind = fract(spin - ang);
            ink += exp(-behind * 7.0) * 0.55 * step(dist, r) * on;
            let nb = u32(mix(3.0, 12.0, detail));
            for (var b = 0u; b < nb; b++) {
                let ba = rnd3(seed, b, 1u);
                let br = r * (0.2 + 0.75 * rnd3(seed, b, 2u));
                let bp = c + vec2(sin(ba * 6.2831853), -cos(ba * 6.2831853)) * br;
                let fade = exp(-fract(spin - ba) * 4.0);
                ink += hg((1.0 - smoothstep(w * 1.5, w * 3.0, length(p - bp))) * fade * on);
            }
        }
        case 2u: {
            // Rings of ticks and arcs turning at their own speeds, each way.
            let q = d / r;
            HREV = step(ang, on);
            let rings = u32(mix(2.0, 6.0, detail));
            for (var k = 0u; k < rings; k++) {
                let rr = r * (0.35 + 0.65 * f32(k + 1u) / f32(rings));
                let speed = (0.3 + rnd3(seed, k, 1u)) * select(-1.0, 1.0, k % 2u == 0u);
                let a2 = fract(ang + spin * speed);
                let segs = floor(2.0 + rnd3(seed, k, 2u) * 6.0);
                let th = 0.25 + 0.5 * rnd3(seed, k, 3u);
                let style = rnd3(seed, k, 5u);
                if (style > 0.5) {
                    let ticks = step(fract(a2 * (24.0 + 48.0 * rnd3(seed, k, 4u))), 0.15);
                    ink += hg(ticks * step(abs(dist - rr), w * 3.0) * step(ang, on));
                } else {
                    hk_arcs(q, rr / r, segs, fract(th / segs - spin * speed), (1.0 - th) / segs, 1.0, false);
                }
            }
        }
        case 3u: {
            // Bars in a box, rising and falling like a readout.
            let box = vec2(r * 1.6, r);
            let q = d + box * 0.5;
            let inbox = step(0.0, q.x) * step(q.x, box.x) * step(0.0, q.y) * step(q.y, box.y);
            HREV = step(0.1, on);
            hk(hs_box(d / r, vec2(0.8, 0.5)), 1.0, 1.0);
            let n = floor(mix(6.0, 40.0, detail));
            let i = floor(q.x / box.x * n);
            let lvl = 0.15 + 0.8 * (0.5 + 0.5 * sin(t * (1.5 + 3.0 * rnd3(seed, bitcast<u32>(i32(i)), 1u)) + i * 1.7)) * vnoise(seed, vec2(i * 0.7, t * 1.3));
            let bar = step(fract(q.x / box.x * n), 0.7) * step(box.y * (1.0 - lvl * on), q.y);
            ink += hg(bar * inbox * 0.8);
        }
        case 4u: {
            // Corner brackets round a box, closing in as it draws on.
            let half = vec2(1.4, 1.0) * (1.4 - 0.4 * on);
            let a = abs(d / r);
            let arm = 0.35;
            HREV = step(0.05, on);
            hk(min(hs_seg(a, half - vec2(arm, 0.0), half), hs_seg(a, half - vec2(0.0, arm), half)), 1.0, 1.0);
        }
        default: {
            // The reticle styles: drawn on clockwise from the top.
            let el = min(u32(round(fx.p0.x)), 34u);
            HREV = step(ang, on) * step(0.0001, on);
            let q = d / r;
            hud_shape(el, q, spin, detail, seed, t);
            ink = HK.x;
            // Faint scanlines inside it, if Fill is up.
            var region = 1.0 - smoothstep(0.92, 0.96, length(q));
            if (el >= 11u && el <= 16u) {
                region = 1.0 - smoothstep(0.82, 0.86, max(abs(q.x), abs(q.y)));
            } else if (el >= 17u && el <= 28u) {
                region = 1.0 - smoothstep(0.95, 1.0, length(q / vec2(1.05, 0.92)));
            }
            let scan = 0.55 + 0.45 * step(fract(p.y / max(3.0, w * 1.5)), 0.5);
            fill = region * scan * clamp(fx.p3.y / 100.0, 0.0, 1.0) * 0.22 * on;
        }
    }
    ink = max(ink, HK.x);
    let f = fx.p2.z / 100.0;
    let flick = 1.0 - f * step(0.75, rnd3(seed + 3u, bitcast<u32>(i32(floor(t * 20.0))), 9u));
    let k = fx.p2.y / 100.0 * flick * lk.x;
    let a = clamp(ink, 0.0, 1.0) * k;
    let glow = clamp(HK.y * clamp(fx.p3.x / 100.0, 0.0, 2.0) * 0.7, 0.0, 1.0) * k * (1.0 - a);
    let haze = clamp(fill, 0.0, 1.0) * k * (1.0 - a);
    let col = select(rgb_of(fx.p1.x), rgb_of(fx.p5.x), lk.z > 0.5);
    let gcol = select(col, rgb_of(fx.p3.w), fx.p4.x > 0.5 && lk.z < 0.5);
    let base = textureSample(src, src_sampler, to_tex(in.uv)) * inside(in.uv);
    return vec4(base.rgb * (1.0 - a) + col * (a + haze) + gcol * glow, max(base.a, clamp(a + glow + haze, 0.0, 1.0)));
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

// Blaster fire: shot k leaves the gun at fire_at + k·every, flies at `speed`
// towards the target (turned by up to ±spread at random), and ends there in a
// flash and a scorch mark. p0 = (gun x %, y %, target x %, y %), p1 = (fire
// at s, shots, every s, speed px/s), p2 = (length px, thickness px, colour,
// core %), p3 = (glow px, spread°, muzzle %, impact %), p4 = (scorch %, seed).
fn bl_seg(p: vec2<f32>, a: vec2<f32>, b: vec2<f32>) -> f32 {
    let ab = b - a;
    let h = clamp(dot(p - a, ab) / max(dot(ab, ab), 1e-6), 0.0, 1.0);
    return length(p - a - ab * h);
}

@fragment
fn fs_blaster(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    let size = max(fx.extra.zw, vec2(1.0));
    let p = in.uv * size;
    let t = fx.extra.y;
    let gun = fx.p0.xy / 100.0 * size;
    let goal = fx.p0.zw / 100.0 * size;
    let col = rgb_of(fx.p2.z);
    let width = max(fx.p2.y, 0.3);
    let glow = max(fx.p3.x, 0.01);
    let seed = u32(max(fx.p4.y, 0.0)) * 6151u + 7u;
    let shots = i32(clamp(round(fx.p1.y), 1.0, 50.0));
    var core = 0.0;
    var halo = 0.0;
    for (var k = 0; k < shots; k++) {
        let dt = t - (fx.p1.x + f32(k) * max(fx.p1.z, 0.02));
        if (dt < 0.0) {
            continue;
        }
        let turn = radians(fx.p3.y) * (rnd3(seed, u32(k), 1u) - 0.5) * 2.0;
        let d0 = goal - gun;
        let aim = gun + vec2(cos(turn) * d0.x - sin(turn) * d0.y, sin(turn) * d0.x + cos(turn) * d0.y);
        let dist = max(length(aim - gun), 1.0);
        let dir = (aim - gun) / dist;
        let gone = dt * max(fx.p1.w, 1.0);
        let head = min(gone, dist);
        let tail = max(gone - max(fx.p2.x, 1.0), 0.0);
        if (tail < dist) {
            let d = bl_seg(p, gun + dir * tail, gun + dir * head);
            core = max(core, 1.0 - smoothstep(width * 0.35, width * 0.5, d));
            halo += exp(-d * d / (glow * glow)) * 0.8;
        }
        // Muzzle flash, a moment after firing.
        let m = exp(-dt * 30.0) * fx.p3.z / 100.0;
        let dm = length(p - gun);
        halo += m * exp(-dm * dm / (glow * glow * 9.0)) * 2.0;
        // Impact: a flash when it arrives, then a glowing scorch that fades.
        if (gone >= dist) {
            let since = (gone - dist) / max(fx.p1.w, 1.0);
            let di = length(p - aim);
            halo += exp(-since * 14.0) * fx.p3.w / 100.0 * exp(-di * di / (glow * glow * 12.0)) * 2.5;
            let scorch = exp(-since * 1.2) * fx.p4.x / 100.0;
            halo += scorch * exp(-di * di / (width * width * 4.0)) * 0.9;
        }
    }
    let core_col = mix(col, vec3(1.0), fx.p2.w / 100.0);
    let light = core_col * core * 1.5 + col * halo;
    return vec4(c.rgb * (1.0 - core) + light, max(c.a, clamp(max(light.r, max(light.g, light.b)), 0.0, 1.0)));
}

// Blood spatter: drops flung from the hit at `at`, each flying out over a
// fraction of a second and landing as a splat stretched along its path (with
// a tail), a big splash at the hit itself, and drops running down if wanted.
// p0 = (x %, y %, at s, direction°), p1 = (spread°, amount %, drop px, reach
// px), p2 = (colour, shine %, run down %, seed).
@fragment
fn fs_blood(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    let size = max(fx.extra.zw, vec2(1.0));
    let p = in.uv * size;
    let since = fx.extra.y - fx.p0.z;
    if (since < 0.0) {
        return c;
    }
    let hit = fx.p0.xy / 100.0 * size;
    let seed = u32(max(fx.p2.w, 0.0)) * 3301u + 9u;
    let n = i32(clamp(fx.p1.y / 100.0 * 120.0, 0.0, 120.0));
    let drop = max(fx.p1.z, 0.5);
    let reach = max(fx.p1.w, 1.0);
    var cover = 0.0;
    var shine = 0.0;
    // The splash at the hit: a ragged blob.
    let grow = smoothstep(0.0, 0.08, since);
    let dh = p - hit;
    let ang = atan2(dh.y, dh.x);
    let rag = 0.75 + 0.35 * vnoise(seed, vec2(ang * 3.0, 0.5)) + 0.2 * vnoise(seed + 1u, vec2(ang * 9.0, 2.0));
    let blob = 1.0 - smoothstep(0.85, 1.0, length(dh) / max(drop * 2.5 * rag * grow, 0.01));
    cover = max(cover, blob * select(0.0, 1.0, fx.p1.y > 0.0));
    for (var i = 0; i < n; i++) {
        let r1 = rnd3(seed, u32(i), 1u);
        let r2 = rnd3(seed, u32(i), 2u);
        let r3 = rnd3(seed, u32(i), 3u);
        let r4 = rnd3(seed, u32(i), 4u);
        let a = radians(fx.p0.w + (r1 - 0.5) * fx.p1.x);
        let dir = vec2(cos(a), sin(a));
        // Farther drops are smaller; each lands after its flight.
        let far = pow(r2, 0.7);
        let land = reach * far;
        let flight = 0.05 + 0.2 * far;
        let k = clamp(since / flight, 0.0, 1.0);
        let at = hit + dir * land * (1.0 - (1.0 - k) * (1.0 - k));
        let rad = drop * (0.35 + 1.3 * (1.0 - far) * (0.3 + 0.7 * r3 * r3));
        // Stretched along its path: longer the faster it flew.
        let q = p - at;
        let along_ = dot(q, dir);
        let across = dot(q, vec2(-dir.y, dir.x));
        let stretch = 1.0 + 1.2 * far;
        var e = length(vec2(along_ / (rad * stretch), across / rad));
        // A tail back towards the hit while it lands.
        let tl = rad * stretch * (1.2 + 1.5 * far);
        let tail = select(0.0, 1.0, along_ < 0.0 && along_ > -tl && abs(across) < rad * 0.55 * (1.0 + along_ / tl));
        var d = max(1.0 - smoothstep(0.85, 1.0, e), tail * k);
        // Running down after it lands.
        if (fx.p2.z > 0.0 && k >= 1.0 && r4 < 0.5) {
            let run = min((since - flight) * 25.0 * (0.4 + r4), reach * 0.5) * fx.p2.z / 100.0;
            let qd = p - at;
            let w = rad * 0.35;
            if (qd.y > 0.0 && qd.y < run && abs(qd.x) < w) {
                d = max(d, 1.0);
            }
            let tip = length(qd - vec2(0.0, run)) / (w * 1.4);
            d = max(d, 1.0 - smoothstep(0.8, 1.0, tip));
        }
        cover = max(cover, d);
        // A glint on each drop.
        shine = max(shine, exp(-length(q - vec2(-0.3, -0.35) * rad) / max(rad * 0.18, 0.3)) * d);
    }
    let col = rgb_of(fx.p2.x);
    let wet = col * (0.85 + 0.15 * cover) + vec3(1.0) * shine * fx.p2.y / 100.0 * 0.5;
    return vec4(mix(c.rgb, wet, cover), max(c.a, cover));
}

// --- Distortion pack (block displacement, derez, energy, fluid, smoke) ---

// How much of the effect a pixel gets: everywhere, or in a soft circle
// (centre % of the layer, radius % of its height, feather %).
fn dz_area(uv: vec2<f32>, kind: f32, c: vec2<f32>, radius: f32, feather: f32) -> f32 {
    if (round(kind) < 0.5) {
        return 1.0;
    }
    let size = max(fx.extra.zw, vec2(1.0));
    let r = max(radius / 100.0 * size.y, 1.0);
    let d = length(uv * size - c / 100.0 * size) / r;
    let f = clamp(feather / 100.0, 0.001, 1.0);
    return 1.0 - smoothstep(1.0 - f, 1.0, d);
}

// Block displacement: p0 = (block w px, h px, amount px, direction), p1 =
// (moved %, changes a second, variety %, split px), p2 = (edges, seed).
@fragment
fn fs_block_displace(in: VsOut) -> @location(0) vec4<f32> {
    let size = max(fx.extra.zw, vec2(1.0));
    let p = in.uv * size;
    let seed = u32(max(fx.p2.y, 0.0)) * 7919u + 3u;
    let step_ = u32(floor(max(fx.extra.y, 0.0) * max(fx.p1.y, 0.0)));
    // Blocks on a grid, some split in two for variety.
    let bs = max(fx.p0.xy, vec2(2.0));
    var cell = floor(p / bs);
    if (rnd3(ibits(cell.x), ibits(cell.y), seed ^ step_) < fx.p1.z / 100.0) {
        cell = floor(p / (bs * vec2(0.5, 0.5))) + vec2(1000.0, 1000.0);
    }
    let cx = ibits(cell.x);
    let cy = ibits(cell.y);
    var off = vec2(0.0);
    if (rnd3(cx, cy, seed + 1u + step_ * 31u) < fx.p1.x / 100.0) {
        let r = vec2(rnd3(cx, cy, seed + 2u + step_ * 37u), rnd3(cx, cy, seed + 3u + step_ * 41u)) * 2.0 - 1.0;
        let dir = u32(round(fx.p0.w));
        let m = select(select(vec2(1.0, 1.0), vec2(0.0, 1.0), dir == 1u), vec2(1.0, 0.0), dir == 0u);
        off = r * m * fx.p0.z;
    }
    let edges = u32(round(fx.p2.x));
    let uv = in.uv - off / size;
    var c = d_sample(uv, edges);
    let split = fx.p1.w / size.x * select(0.0, 1.0, length(off) > 0.0);
    if (split > 0.0) {
        let r = d_sample(uv + vec2(split, 0.0), edges);
        let b = d_sample(uv - vec2(split, 0.0), edges);
        c = vec4(r.r, c.g, b.b, max(c.a, max(r.a, b.a)));
    }
    return c;
}

// Derez: p0 = (progress %, biggest block px, randomness %, fade %), p1 =
// (direction, sweep softness %, seed).
@fragment
fn fs_derez(in: VsOut) -> @location(0) vec4<f32> {
    let size = max(fx.extra.zw, vec2(1.0));
    let p = in.uv * size;
    let prog = clamp(fx.p0.x / 100.0, 0.0, 1.0);
    let seed = u32(max(fx.p1.z, 0.0)) * 104729u + 17u;
    // Where in the sweep this part of the picture is (0 first, 1 last).
    let dir = u32(round(fx.p1.x));
    var order = 0.0;
    if (dir == 1u) { order = in.uv.x; }
    else if (dir == 2u) { order = 1.0 - in.uv.x; }
    else if (dir == 3u) { order = in.uv.y; }
    else if (dir == 4u) { order = 1.0 - in.uv.y; }
    else if (dir == 5u) { order = length((in.uv - 0.5) * vec2(fx.extra.x, 1.0)) / (0.5 * length(vec2(fx.extra.x, 1.0))); }
    // Each coarse block a little ahead or behind.
    let big = max(fx.p0.y, 2.0);
    let cc = floor(p / big);
    let jitter = (rnd3(ibits(cc.x), ibits(cc.y), seed) - 0.5) * fx.p0.z / 100.0;
    let soft = max(fx.p1.y / 100.0, 0.02);
    let local = select(clamp((prog * (1.0 + soft) - order * soft) / 1.0 + jitter * 0.5, 0.0, 1.0), clamp(prog + jitter * 0.5, 0.0, 1.0), dir == 0u);
    // Blocks double as it goes (1, 2, 4… px up to the biggest).
    let levels = log2(big);
    let s = exp2(floor(local * levels));
    if (s <= 1.0) {
        return textureSample(src, src_sampler, to_tex(in.uv));
    }
    let cell = floor(p / s);
    let c = textureSampleLevel(src, src_sampler, to_tex((cell + 0.5) * s / size), 0.0);
    // At the end each block winks out, at its own moment.
    let gone = smoothstep(0.75, 0.95, local + 0.15 - rnd3(ibits(cell.x), ibits(cell.y), seed + 1u) * 0.15);
    return c * (1.0 - gone * fx.p0.w / 100.0);
}

// Energy distortion: p0 = (amount px, size px, speed, detail), p1 = (glow %,
// colour, area, x %), p2 = (y %, radius %, feather %, edges), p3.x = seed.
@fragment
fn fs_energy_distort(in: VsOut) -> @location(0) vec4<f32> {
    let size = max(fx.extra.zw, vec2(1.0));
    let p = in.uv * size / max(fx.p0.y, 2.0);
    let t = fx.extra.y * fx.p0.z;
    let seed = u32(max(fx.p3.x, 0.0)) * 7919u + 23u;
    // Ridged noise: sharp crests where the energy runs.
    let n1 = tfbm(seed, vec3(p, t), fx.p0.w);
    let n2 = tfbm(seed + 9u, vec3(p + 5.2, t * 1.3), fx.p0.w);
    let ridge = pow(clamp(1.0 - abs(n1 * 2.0 - 1.0), 0.0, 1.0), 6.0);
    let w = dz_area(in.uv, fx.p1.z, vec2(fx.p1.w, fx.p2.x), fx.p2.y, fx.p2.z);
    let off = (vec2(n1, n2) - 0.5) * 2.0 * fx.p0.x * (0.4 + 0.6 * ridge) * w;
    var c = d_sample(in.uv + off / size, u32(round(fx.p2.w)));
    let glow = rgb_of(fx.p1.y) * ridge * ridge * fx.p1.x / 100.0 * w;
    return vec4(c.rgb + glow * max(c.a, 0.3), max(c.a, clamp(max3(glow), 0.0, 1.0)));
}

// Fluid distortion: noise bent by noise (domain warping) for liquid swirls,
// and a curl so it flows round rather than in and out. p0 = (amount px, size
// px, speed, swirl %), p1 = (flow°, flow px/s, area, x %), p2 = (y %, radius %,
// feather %, edges), p3.x = seed.
@fragment
fn fs_fluid_distort(in: VsOut) -> @location(0) vec4<f32> {
    let size = max(fx.extra.zw, vec2(1.0));
    let t = fx.extra.y;
    let fa = radians(fx.p1.x);
    let drift = vec2(cos(fa), sin(fa)) * fx.p1.y * t;
    let sc = max(fx.p0.y, 2.0);
    let p = (in.uv * size - drift) / sc;
    let seed = u32(max(fx.p3.x, 0.0)) * 7919u + 41u;
    let z = t * fx.p0.z;
    let warp = vec2(tfbm(seed, vec3(p, z), 3.0), tfbm(seed + 3u, vec3(p + 3.7, z), 3.0)) - 0.5;
    let q = p + warp * 2.0 * fx.p0.w / 100.0;
    // Curl of a potential: flow that goes round.
    let e = 0.05;
    let n = tfbm(seed + 7u, vec3(q, z * 0.7), 3.0);
    let nx = tfbm(seed + 7u, vec3(q + vec2(e, 0.0), z * 0.7), 3.0);
    let ny = tfbm(seed + 7u, vec3(q + vec2(0.0, e), z * 0.7), 3.0);
    let curl = vec2(ny - n, -(nx - n)) / e;
    let w = dz_area(in.uv, fx.p1.z, vec2(fx.p1.w, fx.p2.x), fx.p2.y, fx.p2.z);
    let off = clamp(curl * 0.25, vec2(-1.5), vec2(1.5)) * fx.p0.x * w;
    return d_sample(in.uv + off / size, u32(round(fx.p2.w)));
}

// Smoke distortion: wisps carried up (stretched up and down as smoke is),
// curling more with turbulence, thinning as they rise from the bottom area.
// p0 = (amount px, size px, rise px/s, turbulence %), p1 = (tint %, colour,
// area, x %), p2 = (y %, radius %, feather %, edges), p3 = (seed, height %).
@fragment
fn fs_smoke_distort(in: VsOut) -> @location(0) vec4<f32> {
    let size = max(fx.extra.zw, vec2(1.0));
    let t = fx.extra.y;
    let sc = max(fx.p0.y, 2.0);
    let seed = u32(max(fx.p3.x, 0.0)) * 7919u + 59u;
    var p = (in.uv * size + vec2(0.0, fx.p0.z * t)) / (sc * vec2(1.0, 1.8));
    let turb = fx.p0.w / 100.0;
    p += (vec2(tfbm(seed, vec3(p * 0.7, t * 0.4), 3.0), tfbm(seed + 2u, vec3(p * 0.7 + 4.1, t * 0.4), 3.0)) - 0.5) * 2.5 * turb;
    let n1 = tfbm(seed + 5u, vec3(p, t * 0.25), 4.0);
    let n2 = tfbm(seed + 6u, vec3(p + 9.3, t * 0.25), 4.0);
    // Where it is: everywhere, a circle, or rising from the bottom and thinning.
    var w = 1.0;
    let area = round(fx.p1.z);
    if (area > 1.5) {
        let h = max(fx.p3.y / 100.0, 0.01);
        w = 1.0 - smoothstep(0.0, h, 1.0 - in.uv.y);
    } else {
        w = dz_area(in.uv, area, vec2(fx.p1.w, fx.p2.x), fx.p2.y, fx.p2.z);
    }
    let wisp = smoothstep(0.35, 0.75, n1) * w;
    let off = (vec2(n1, n2) - 0.5) * 2.0 * fx.p0.x * (0.4 + 0.6 * wisp) * w;
    let c = d_sample(in.uv + off / size, u32(round(fx.p2.w)));
    let tint = wisp * fx.p1.x / 100.0;
    return vec4(mix(c.rgb, rgb_of(fx.p1.y) * max(c.a, tint), tint), max(c.a, tint));
}

// Gradient ramp: p0 = (shape, p1 x %, p1 y %, p2 x %), p1 = (p2 y %, colour 1,
// colour 2, p3 x %), p2 = (p3 y %, p4 x %, p4 y %, colour 3), p3 = (colour 4,
// blend %, opacity %, mix mode), p4.x = dither %. Colours blend as the eye
// sees them (in display values), then back to linear.
@fragment
fn fs_ramp(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    let size = max(fx.extra.zw, vec2(1.0));
    let p = in.uv * size;
    let a = vec2(fx.p0.y, fx.p0.z) / 100.0 * size;
    let b = vec2(fx.p0.w, fx.p1.x) / 100.0 * size;
    let c1 = to_display(rgb_of(fx.p1.y));
    let c2 = to_display(rgb_of(fx.p1.z));
    let shape = u32(round(fx.p0.x));
    var g: vec3<f32>;
    if (shape == 2u) {
        // Four colours, each at its point: weighted by closeness (Blend: how far they reach).
        let pts = array<vec2<f32>, 4>(a, b, vec2(fx.p1.w, fx.p2.x) / 100.0 * size, vec2(fx.p2.y, fx.p2.z) / 100.0 * size);
        let cols = array<vec3<f32>, 4>(c1, c2, to_display(rgb_of(fx.p2.w)), to_display(rgb_of(fx.p3.x)));
        let pw = mix(6.0, 1.2, clamp(fx.p3.y / 100.0, 0.0, 1.0));
        var acc = vec3(0.0);
        var wsum = 0.0;
        let diag = length(size);
        for (var i = 0; i < 4; i++) {
            let w = 1.0 / pow(max(length(p - pts[i]) / diag, 1e-4), pw);
            acc += cols[i] * w;
            wsum += w;
        }
        g = acc / max(wsum, 1e-6);
    } else {
        let ab = b - a;
        var t: f32;
        if (shape == 1u) {
            t = length(p - a) / max(length(ab), 1e-3);
        } else {
            t = dot(p - a, ab) / max(dot(ab, ab), 1e-3);
        }
        t = clamp(t, 0.0, 1.0);
        // Blend: 50 % even; higher eases in and out, lower holds the ends longer.
        let k = clamp(fx.p3.y / 100.0, 0.0, 1.0);
        if (k >= 0.5) {
            t = mix(t, smoothstep(0.0, 1.0, t), (k - 0.5) * 2.0);
        } else {
            t = mix(t, smoothstep(0.25, 0.75, t), (0.5 - k) * 2.0);
        }
        g = mix(c1, c2, t);
    }
    // Dither so long ramps don't band.
    let n = rnd3(ibits(p.x), ibits(p.y), 913u) - 0.5;
    g += vec3(n / 255.0 * fx.p4.x / 100.0 * 2.0);
    let ramp = to_linear(clamp(g, vec3(0.0), vec3(1.0)));
    let op = clamp(fx.p3.z / 100.0, 0.0, 1.0);
    let base = unpremul(c);
    let mode = u32(round(fx.p3.w));
    if (mode == 0u) {
        // Over it: the ramp in front, the picture showing through by opacity.
        let a2 = mix(c.a, 1.0, op);
        let rgb = mix(c.rgb, ramp, op);
        return vec4(rgb, a2);
    }
    var mixed: vec3<f32>;
    let bd = to_display(base);
    let rd = to_display(ramp);
    if (mode == 1u) {
        mixed = base * ramp;
    } else if (mode == 2u) {
        mixed = 1.0 - (1.0 - base) * (1.0 - ramp);
    } else if (mode == 3u) {
        mixed = to_linear(select(1.0 - 2.0 * (1.0 - bd) * (1.0 - rd), 2.0 * bd * rd, bd < vec3(0.5)));
    } else {
        // Colour only: the ramp's hue and saturation, the picture's brightness.
        let y = dot(bd, vec3(0.2126, 0.7152, 0.0722));
        let ry = dot(rd, vec3(0.2126, 0.7152, 0.0722));
        mixed = to_linear(clamp(rd + (y - ry), vec3(0.0), vec3(1.0)));
    }
    return vec4(mix(base, mixed, op) * c.a, c.a);
}

// Strobe: p0 = (flashes a second, flash length %, mode, colour), p1 = (start s,
// random %, strength %, seed). With Random, each beat flashes or not by chance.
@fragment
fn fs_strobe(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    let since = fx.extra.y - fx.p1.x;
    if (since < 0.0) {
        return c;
    }
    let beats = since * max(fx.p0.x, 0.01);
    let beat = floor(beats);
    var on = select(0.0, 1.0, beats - beat < clamp(fx.p0.y / 100.0, 0.0, 1.0));
    if (fx.p1.y > 0.0 && rnd3(ibits(beat), u32(max(fx.p1.w, 0.0)), 7u) < fx.p1.y / 100.0) {
        on = 0.0;
    }
    let k = on * clamp(fx.p1.z / 100.0, 0.0, 4.0);
    let mode = u32(round(fx.p0.z));
    if (mode == 1u) {
        return c * (1.0 - min(k, 1.0));
    }
    if (mode == 2u) {
        let inv = (vec3(1.0) - to_display(unpremul(c)));
        return vec4(mix(c.rgb, to_linear(inv) * c.a, min(k, 1.0)), c.a);
    }
    if (mode == 3u) {
        return vec4(c.rgb + c.rgb * rgb_of(fx.p0.w) * k * 2.0 + rgb_of(fx.p0.w) * c.a * k * 0.15, c.a);
    }
    return vec4(mix(c.rgb, rgb_of(fx.p0.w) * c.a, min(k, 1.0)), c.a);
}

// ---- Gradient map, aurora, nebula, clouds, burn away, deflicker ----

// Gradient map: p0 = (colours: 0 two / 1 three, shadows, midtones, highlights),
// p1 = (balance %, contrast %, mix %).
@fragment
fn fs_gradient_map(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    if (c.a <= 0.0) {
        return vec4(0.0);
    }
    let rgb = to_display(unpremul(c));
    var l = clamp(dot(rgb, LUMA), 0.0, 1.0);
    l = clamp((l - 0.5) * fx.p1.y / 100.0 + 0.5, 0.0, 1.0);
    // The balance is where the middle colour falls.
    let b = clamp(fx.p1.x / 100.0, 0.01, 0.99);
    l = pow(l, log(0.5) / log(b));
    let sh = to_display(rgb_of(fx.p0.y));
    let mid = to_display(rgb_of(fx.p0.z));
    let hi = to_display(rgb_of(fx.p0.w));
    var out: vec3<f32>;
    if (u32(round(fx.p0.x)) == 1u) {
        out = select(mix(mid, hi, l * 2.0 - 1.0), mix(sh, mid, l * 2.0), l < 0.5);
    } else {
        out = mix(sh, hi, l);
    }
    let m = mix(rgb, out, clamp(fx.p1.z / 100.0, 0.0, 1.0));
    return vec4(to_linear(m) * c.a, c.a);
}

// Aurora: p0 = (colour low, colour high, height %, size %), p1 = (brightness %, speed %,
// waves %, rays %), p2 = (curtains, draw, seed).
@fragment
fn fs_aurora(in: VsOut) -> @location(0) vec4<f32> {
    let size = max(fx.extra.zw, vec2(1.0));
    let t = fx.extra.y * fx.p1.y / 100.0;
    let seed = u32(max(fx.p2.z, 0.0)) * 613u + 29u;
    let low = rgb_of(fx.p0.x);
    let high = rgb_of(fx.p0.y);
    let tall = max(fx.p0.w / 100.0, 0.01);
    let waves = fx.p1.z / 100.0;
    let rays = clamp(fx.p1.w / 100.0, 0.0, 1.0);
    let n = u32(clamp(round(fx.p2.x), 1.0, 4.0));
    // In units of the picture's height, x across.
    let x = in.uv.x * size.x / size.y;
    let y = in.uv.y;
    var light = vec3(0.0);
    for (var k = 0u; k < n; k++) {
        let s = seed + k * 71u;
        let fk = f32(k);
        // The curtain's foot: a slow wave across the sky.
        let base = fx.p0.z / 100.0 - fk * 0.07 * tall / 0.35
            + waves * 0.12 * (fbm2(s, vec2(x * 0.9 + t * 0.05 + fk * 3.1, t * 0.03)) - 0.5) * 2.0
            + waves * 0.03 * sin(x * 7.0 + t * 0.6 + fk * 2.0);
        // How far up from its foot, in its height.
        let d = (base - y) / tall;
        if (d < -0.3 || d > 3.0) {
            continue;
        }
        // Bright at the foot, fading up (and out, well above); a little glow below.
        var body = select(exp(d * 18.0) * 0.6, exp(-d * 2.2) * (0.6 + 0.4 * smoothstep(0.0, 0.06, d)) * smoothstep(3.0, 1.6, d), d >= 0.0);
        // Where along the sky it shows (it comes and goes).
        let gaps = smoothstep(0.25, 0.65, fbm2(s + 5u, vec2(x * 0.6 - t * 0.04 + fk * 9.0, fk)));
        // Fine vertical rays, drifting sideways.
        let r = fbm2(s + 9u, vec2(x * 26.0 + t * 0.4, d * 0.6 + fk * 4.0));
        let ray = mix(1.0, 0.25 + 1.5 * r * r, rays);
        body = body * gaps * ray;
        let col = mix(low, high, clamp(d * 1.1, 0.0, 1.0));
        light += col * body * (1.0 - fk * 0.18);
    }
    light *= fx.p1.x / 100.0 * 0.9;
    let cover = clamp(dot(light, LUMA), 0.0, 1.0);
    return gen_over(in.uv, light, cover, u32(round(fx.p2.y)));
}

// A noise for space and clouds: `oct` octaves of value noise, 0..1.
fn sky_fbm(seed: u32, p: vec2<f32>, oct: u32) -> f32 {
    var sum = 0.0;
    var amp = 0.5;
    var q = p;
    var total = 0.0;
    for (var i = 0u; i < oct; i++) {
        sum += vnoise(seed + i * 13u, q) * amp;
        total += amp;
        amp *= 0.5;
        // (Turned each octave, so the noise's grid doesn't show.)
        q = vec2(q.x * 1.6 - q.y * 1.2, q.x * 1.2 + q.y * 1.6) + vec2(17.3, 9.1);
    }
    return sum / max(total, 1e-4);
}

// Nebula: p0 = (colour, second colour, glow, density %), p1 = (scale %, detail %,
// brightness %, swirl speed %), p2 = (stars %, dust lanes %, draw, seed).
@fragment
fn fs_nebula(in: VsOut) -> @location(0) vec4<f32> {
    let size = max(fx.extra.zw, vec2(1.0));
    let t = fx.extra.y * fx.p1.w / 100.0;
    let seed = u32(max(fx.p2.w, 0.0)) * 409u + 61u;
    let p = (in.uv - 0.5) * size / size.y / max(fx.p1.x / 100.0, 0.05) * 2.2;
    let oct = 3u + u32(clamp(fx.p1.y / 100.0, 0.0, 1.0) * 4.0);
    // Warped twice, so the gas swirls.
    let w1 = vec2(sky_fbm(seed, p + vec2(0.0, t * 0.03), 4u), sky_fbm(seed + 40u, p + vec2(5.2, 1.3) - vec2(t * 0.02, 0.0), 4u));
    let w2 = vec2(sky_fbm(seed + 80u, p + w1 * 1.6 + vec2(1.7, 9.2), oct), sky_fbm(seed + 120u, p + w1 * 1.6 + vec2(8.3, 2.8), oct));
    let gas = sky_fbm(seed + 160u, p + w2 * 1.4, oct);
    let tint = sky_fbm(seed + 200u, p * 0.7 + w1, 3u);
    let dens = clamp(fx.p0.w / 100.0, 0.0, 1.0);
    let cloud = smoothstep(0.62 - dens * 0.45, 0.95, gas);
    var col = mix(rgb_of(fx.p0.x), rgb_of(fx.p0.y), smoothstep(0.3, 0.7, tint)) * cloud;
    // The glowing heart where it's thickest.
    col += rgb_of(fx.p0.z) * pow(smoothstep(0.55, 1.0, gas), 3.0) * 1.6 * (0.5 + dens);
    // Dark lanes of dust across it.
    let lane = 1.0 - smoothstep(0.0, 0.06, abs(sky_fbm(seed + 240u, p * 1.3 + w2 * 0.6, oct) - 0.5));
    col *= 1.0 - fx.p2.y / 100.0 * lane * 0.9;
    col *= fx.p1.z / 100.0;
    // Stars: one at a random place in some cells, twinkling a little.
    let sa = clamp(fx.p2.x / 100.0, 0.0, 1.0);
    if (sa > 0.0) {
        let px = in.uv * size;
        let cell = mix(60.0, 16.0, sa);
        let g = floor(px / cell);
        for (var j = -1; j <= 1; j++) {
            for (var i = -1; i <= 1; i++) {
                let gi = g + vec2(f32(i), f32(j));
                let ix = ibits(gi.x);
                let iy = ibits(gi.y);
                let pos = (gi + vec2(rnd3(seed + 3u, ix, iy), rnd3(seed + 4u, ix, iy))) * cell;
                let dist = length(px - pos);
                let bright = pow(rnd3(seed + 5u, ix, iy), 5.0) * 2.5 + 0.08;
                let w = 0.8 + 1.4 * rnd3(seed + 6u, ix, iy);
                let tw = 0.75 + 0.25 * sin(fx.extra.y * (1.5 + 4.0 * rnd3(seed + 7u, ix, iy)) + rnd3(seed + 8u, ix, iy) * 30.0);
                col += vec3(0.85, 0.9, 1.0) * exp(-dist * dist / (w * w)) * bright * tw;
            }
        }
    }
    let cover = clamp(dot(col, LUMA) * 1.5, 0.0, 1.0);
    return gen_over(in.uv, col, cover, u32(round(fx.p2.z)));
}

// How thick the clouds are at `p` (in units of the cloud size), 0..1.
fn cloud_density(seed: u32, p: vec2<f32>, cover: f32, soft: f32) -> f32 {
    let n = sky_fbm(seed, p, 5u);
    let edge = 1.0 - cover;
    return smoothstep(edge - soft * 0.35, edge + 0.05 + soft * 0.35, n);
}

// Clouds: p0 = (mode, coverage %, softness %, scale %), p1 = (speed %, direction °,
// cloud colour, shade colour), p2 = (sky top, sky horizon, draw, brightness %), p3.x = seed.
@fragment
fn fs_clouds(in: VsOut) -> @location(0) vec4<f32> {
    let size = max(fx.extra.zw, vec2(1.0));
    let t = fx.extra.y * fx.p1.x / 100.0;
    let seed = u32(max(fx.p3.x, 0.0)) * 199u + 41u;
    let cover = clamp(fx.p0.y / 100.0, 0.0, 1.0);
    let soft = clamp(fx.p0.z / 100.0, 0.0, 1.0);
    let scale = max(fx.p0.w / 100.0, 0.05) * 0.45;
    let lit = rgb_of(fx.p1.z);
    let shade = rgb_of(fx.p1.w);
    let draw = u32(round(fx.p2.z));
    let bright = fx.p2.w / 100.0;
    let aspect = size.x / size.y;
    var col = vec3(0.0);
    var alpha = 0.0;
    if (u32(round(fx.p0.x)) == 1u) {
        // Flying through: sheets of cloud coming towards the camera from far off, far first.
        let q = (in.uv - 0.5) * vec2(aspect, 1.0);
        let n = 7u;
        for (var k = 0u; k < n; k++) {
            let phase = f32(k) / f32(n);
            let z = fract(phase - t * 0.06);
            let zz = 1.0 - z;
            let depth = mix(0.05, 1.0, zz);
            let p = q / (depth * scale) + vec2(f32(k) * 7.31, f32(k) * 3.17);
            let d = cloud_density(seed + k * 31u, p, cover * 0.75, soft);
            // Lit from above: thinner just above means brighter.
            let above = cloud_density(seed + k * 31u, p - vec2(0.0, 0.08), cover * 0.75, soft);
            let c = mix(shade, lit, clamp(1.0 - (d - above) * 2.5, 0.0, 1.0)) * bright;
            // Fading in from far away, and out as it passes the camera.
            let a = d * smoothstep(1.0, 0.75, zz) * smoothstep(0.0, 0.3, zz) * 0.55;
            col = c * a + col * (1.0 - a);
            alpha = a + alpha * (1.0 - a);
        }
    } else {
        // A sky drifting by: the wind blows the big shapes slowly, the detail faster.
        let dir = vec2(cos(radians(fx.p1.y)), -sin(radians(fx.p1.y)));
        let p = (in.uv * vec2(aspect, 1.0)) / scale - dir * t * 0.12;
        let d = cloud_density(seed, p, cover, soft);
        let above = cloud_density(seed, p - vec2(0.0, 0.06), cover, soft);
        col = mix(shade, lit, clamp(1.0 - (d - above) * 3.0 - (d - 0.5) * 0.3, 0.0, 1.0)) * bright;
        alpha = d;
        col *= alpha;
    }
    // (col is premultiplied by alpha here.)
    if (draw == 2u) {
        return vec4(col, alpha);
    }
    var under: vec4<f32>;
    if (draw == 1u) {
        under = vec4(mix(rgb_of(fx.p2.x), rgb_of(fx.p2.y), smoothstep(0.0, 1.0, in.uv.y)), 1.0);
    } else {
        under = textureSample(src, src_sampler, to_tex(in.uv)) * inside(in.uv);
    }
    return vec4(col + under.rgb * (1.0 - alpha), alpha + under.a * (1.0 - alpha));
}

// Burn away: p0 = (burnt %, from, point x %, point y %), p1 = (glowing edge px, charred
// edge px, glow colour, glow %), p2 = (raggedness %, embers %, seed).
@fragment
fn fs_burn(in: VsOut) -> @location(0) vec4<f32> {
    let size = max(fx.extra.zw, vec2(1.0));
    let c = textureSample(src, src_sampler, to_tex(in.uv)) * inside(in.uv);
    let seed = u32(max(fx.p2.z, 0.0)) * 271u + 17u;
    let px = in.uv * size;
    // Where it burns first (0) to last (1), and how far that is from the front in pixels
    // (from the field's slope, measured a pixel either way).
    let v = burn_field(px, size, seed);
    let gx = burn_field(px + vec2(1.0, 0.0), size, seed) - burn_field(px - vec2(1.0, 0.0), size, seed);
    let gy = burn_field(px + vec2(0.0, 1.0), size, seed) - burn_field(px - vec2(0.0, 1.0), size, seed);
    let slope = max(length(vec2(gx, gy)) * 0.5, 1e-5);
    let th = fx.p0.x / 100.0 * 1.3 - 0.15;
    let dist = (v - th) / slope;
    let edge = max(fx.p1.x, 0.01);
    let charw = max(fx.p1.y, 0.01);
    let glow_col = rgb_of(fx.p1.z) * fx.p1.w / 100.0;
    let t = fx.extra.y;
    // Embers: sparks just behind the front, rising and flickering.
    var embers = vec3(0.0);
    let ea = clamp(fx.p2.y / 100.0, 0.0, 1.0);
    if (ea > 0.0 && dist < 0.0 && dist > -edge * 6.0 - 40.0) {
        let q = px + vec2(0.0, t * 30.0);
        let cell = 9.0;
        let gc = floor(q / cell);
        let h = rnd3(seed + 9u, ibits(gc.x), ibits(gc.y));
        if (h < ea * 0.3) {
            let pos = (gc + vec2(rnd3(seed + 10u, ibits(gc.x), ibits(gc.y)), rnd3(seed + 11u, ibits(gc.x), ibits(gc.y)))) * cell;
            let d2 = length(q - pos);
            let fl = 0.5 + 0.5 * sin(t * 9.0 + h * 60.0);
            embers = glow_col * exp(-d2 * d2 / 2.0) * fl * exp(dist / (edge * 3.0 + 12.0)) * 1.2;
        }
    }
    if (dist < 0.0) {
        // Burnt: gone, but for the glow just past the front (where there was something).
        let a = c.a * exp(dist / (edge * 0.6)) * 0.8;
        let e = embers * c.a;
        return vec4(glow_col * a + e, clamp(a + dot(e, LUMA), 0.0, 1.0));
    }
    // Not burnt: charred near the front, with the glow on its very edge.
    // (The slope only says how far the front is close to it: far off, nothing.)
    let near = 1.0 - smoothstep(0.08, 0.2, v - th);
    let char_k = exp(-dist * dist / (charw * charw)) * near;
    let rgb = unpremul(c);
    let charred = mix(rgb, rgb * vec3(0.08, 0.05, 0.03), clamp(char_k * 1.1, 0.0, 1.0));
    let glow = glow_col * exp(-dist * dist / (edge * edge)) * 0.8 * near;
    return vec4((charred + glow) * c.a, c.a);
}

// Burn away's field: 0 where it burns first to 1 where it burns last.
fn burn_field(px: vec2<f32>, size: vec2<f32>, seed: u32) -> f32 {
    let uv = px / size;
    let rag = fx.p2.x / 100.0;
    // (Stretched: the noise's values bunch in the middle.)
    let n = smoothstep(0.2, 0.8, sky_fbm(seed, px / 110.0, 4u));
    let src_from = u32(round(fx.p0.y));
    if (src_from == 0u) {
        return n;
    }
    var g = 0.5;
    if (src_from == 1u) {
        g = uv.x;
    } else if (src_from == 2u) {
        g = 1.0 - uv.x;
    } else if (src_from == 3u) {
        g = uv.y;
    } else if (src_from == 4u) {
        g = 1.0 - uv.y;
    } else {
        let ctr = select(vec2(0.5), fx.p0.zw / 100.0, src_from == 6u);
        let far = max(max(length(ctr * size), length((vec2(1.0, 0.0) - ctr) * size)), max(length((vec2(0.0, 1.0) - ctr) * size), length((vec2(1.0) - ctr) * size)));
        g = length(px - ctr * size) / max(far, 1.0);
    }
    return clamp(g * 0.8 + (n - 0.5) * 0.5 * rag + 0.1, 0.0, 1.0);
}

// Deflicker: p0 = (brightness %, red %, green %, blue %).
@fragment
fn fs_deflicker(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    let gain = fx.p0.x / 100.0 * vec3(fx.p0.y, fx.p0.z, fx.p0.w) / 100.0;
    // (Gains apply to the light, as an exposure change would.)
    return vec4(max(c.rgb * gain, vec3(0.0)), c.a);
}

// ---- Geometry: the layer as a shape in 3D ----
// Positions are layer pixels from the layer's middle (x right, y down, z away from
// the viewer); the camera looks along +z from in front, so the layer's own plane
// (z = 0) shows pixel for pixel. A field of view of (almost) 0 is flat (orthographic).

// A turn of `deg` degrees round X, then Y, then Z.
fn geo_turn(deg: vec3<f32>) -> mat3x3<f32> {
    let a = radians(deg);
    let cx = cos(a.x);
    let sx = sin(a.x);
    let cy = cos(a.y);
    let sy = sin(a.y);
    let cz = cos(a.z);
    let sz = sin(a.z);
    let rx = mat3x3<f32>(vec3(1.0, 0.0, 0.0), vec3(0.0, cx, sx), vec3(0.0, -sx, cx));
    let ry = mat3x3<f32>(vec3(cy, 0.0, -sy), vec3(0.0, 1.0, 0.0), vec3(sy, 0.0, cy));
    let rz = mat3x3<f32>(vec3(cz, sz, 0.0), vec3(-sz, cz, 0.0), vec3(0.0, 0.0, 1.0));
    return rz * ry * rx;
}

// The camera's distance for a field of view (degrees) on a layer `h` pixels high
// (very far for a flat view).
fn geo_focal(fov: f32, h: f32) -> f32 {
    if (fov < 0.5) {
        return 1e7;
    }
    return h * 0.5 / tan(radians(min(fov, 170.0)) * 0.5);
}

// The picture at layer pixel `q` (from the middle); nothing outside the layer.
fn geo_at(q: vec2<f32>, size: vec2<f32>) -> vec4<f32> {
    let uv = q / size + 0.5;
    if (any(uv < vec2(0.0)) || any(uv > vec2(1.0))) {
        return vec4(0.0);
    }
    return textureSampleLevel(src, src_sampler, to_tex(uv), 0.0);
}

// How much of a pixel `d` pixels inside an edge is covered (a soft edge one pixel wide).
fn geo_cover(d: f32) -> f32 {
    return clamp(d + 0.5, 0.0, 1.0);
}

// Rotate: p0 = (turn x, turn y, turn z, perspective °), p1 = (size %, pivot x %, pivot y %,
// push back px), p2.x = back (0 the picture, 1 darkened, 2 nothing).
@fragment
fn fs_rotate3d(in: VsOut) -> @location(0) vec4<f32> {
    let size = max(fx.extra.zw, vec2(1.0));
    let f = geo_focal(fx.p0.w, size.y);
    let k = max(fx.p1.x, 1.0) / 100.0;
    let pivot = vec3((fx.p1.yz / 100.0 - 0.5) * size, fx.p1.w);
    let r = geo_turn(fx.p0.xyz);
    let p = (in.uv - 0.5) * size;
    // (Flat view: rays straight ahead from each pixel.)
    var o = vec3(0.0, 0.0, -f);
    var dir = normalize(vec3(p, f));
    if (f >= 1e6) {
        o = vec3(p, -1e4);
        dir = vec3(0.0, 0.0, 1.0);
    }
    let n = r * vec3(0.0, 0.0, -1.0);
    let den = dot(dir, n);
    if (abs(den) < 1e-6) {
        return vec4(0.0);
    }
    let t = dot(pivot - o, n) / den;
    if (t <= 0.0) {
        return vec4(0.0);
    }
    let h = o + dir * t;
    // Back to the layer: unturned, unscaled, about the pivot.
    let q = (transpose(r) * (h - pivot)).xy / k + pivot.xy;
    let edge = min(min(q.x + size.x * 0.5, size.x * 0.5 - q.x), min(q.y + size.y * 0.5, size.y * 0.5 - q.y)) * k * f / max(h.z + f, 1.0);
    var c = geo_at(clamp(q, -size * 0.5, size * 0.5), size) * geo_cover(edge);
    if (den > 0.0) {
        // Its back.
        let back = u32(round(fx.p2.x));
        if (back == 2u) {
            return vec4(0.0);
        }
        if (back == 1u) {
            c = vec4(c.rgb * 0.35, c.a);
        }
    }
    return c;
}

// Bend: p0 = (bend °, axis: 0 left–right edges, 1 top–bottom, centre %, edges go: 0 away,
// 1 towards), p1 = (perspective °, shading %, back: 0 picture 1 darkened 2 nothing, size %).
@fragment
fn fs_bend(in: VsOut) -> @location(0) vec4<f32> {
    let size0 = max(fx.extra.zw, vec2(1.0));
    let across_x = fx.p0.y < 0.5;
    // Along the bend and across it, whichever way it goes.
    let size = select(size0.yx, size0, across_x);
    let p0 = (in.uv - 0.5) * size0;
    let p = select(p0.yx, p0, across_x);
    let k = max(fx.p1.w, 1.0) / 100.0;
    let len = size.x * k;
    let f = geo_focal(fx.p1.x, size0.y);
    let flat_view = f >= 1e6;
    let s = select(1.0, -1.0, fx.p0.w >= 0.5);
    let theta = radians(fx.p0.x);
    let c = (fx.p0.z / 100.0 - 0.5) * len;
    // The sheet at u (from its middle, scaled): where it is (along, z) and its normal.
    var best_z = 1e9;
    var best = vec4(0.0);
    let n_steps = 96;
    var prev_u = -len * 0.5;
    var prev_g = 0.0;
    for (var i = 0; i <= n_steps; i = i + 1) {
        let u = -len * 0.5 + len * f32(i) / f32(n_steps);
        let g = bend_screen(u, c, theta, len, s, f, flat_view) - p.x;
        if (i > 0 && (g == 0.0 || sign(g) != sign(prev_g))) {
            // Narrow it down.
            var a = prev_u;
            var b = u;
            var ga = prev_g;
            for (var j = 0; j < 10; j = j + 1) {
                let m = (a + b) * 0.5;
                let gm = bend_screen(m, c, theta, len, s, f, flat_view) - p.x;
                if (sign(gm) == sign(ga)) {
                    a = m;
                    ga = gm;
                } else {
                    b = m;
                }
            }
            let um = (a + b) * 0.5;
            let pz = bend_place(um, c, theta, len, s);
            if (pz.y < best_z) {
                let persp = select(f / max(f + pz.y, 1.0), 1.0, flat_view);
                let across = p.y / persp;
                if (abs(across) <= size.y * k * 0.5 + 1.0) {
                    let q_al = um / k;
                    let q_ac = across / k;
                    let q = select(vec2(q_ac, q_al), vec2(q_al, q_ac), across_x);
                    var col = geo_at(clamp(q, -size0 * 0.5, size0 * 0.5), size0) * geo_cover((size.y * k * 0.5 - abs(across)) * persp);
                    // Shading by how the sheet faces the light (from the upper left, in front).
                    let a_ = bend_angle(um, c, theta, len);
                    let nrm = vec3(s * sin(a_), 0.0, -cos(a_));
                    let rd = select(normalize(vec3(p, f)), vec3(0.0, 0.0, 1.0), flat_view);
                    let front = dot(nrm, rd) < 0.0;
                    let lit = clamp(dot(select(-nrm, nrm, front), normalize(vec3(-0.35, -0.45, -1.0))), 0.0, 1.0);
                    let shade = mix(1.0, 0.35 + 0.65 * lit, clamp(fx.p1.y / 100.0, 0.0, 1.0));
                    col = vec4(col.rgb * shade, col.a);
                    let back = u32(round(fx.p1.z));
                    if (!front && back == 2u) {
                        col = vec4(0.0);
                    } else if (!front && back == 1u) {
                        col = vec4(col.rgb * 0.35, col.a);
                    }
                    if (col.a > 0.0) {
                        best_z = pz.y;
                        best = col;
                    }
                }
            }
        }
        prev_u = u;
        prev_g = g;
    }
    return best;
}

fn bend_angle(u: f32, c: f32, theta: f32, len: f32) -> f32 {
    return (u - c) * theta / max(len, 1.0);
}

// The sheet's point at u: (along, z).
fn bend_place(u: f32, c: f32, theta: f32, len: f32, s: f32) -> vec2<f32> {
    if (abs(theta) < 1e-4) {
        return vec2(u, 0.0);
    }
    let r = len / theta;
    let a = (u - c) / r;
    return vec2(c + r * sin(a), s * r * (1.0 - cos(a)));
}

// Where the sheet's point at u shows on screen (along).
fn bend_screen(u: f32, c: f32, theta: f32, len: f32, s: f32, f: f32, flat_view: bool) -> f32 {
    let pz = bend_place(u, c, theta, len, s);
    if (flat_view) {
        return pz.x;
    }
    return pz.x * f / max(f + pz.y, 1e-3);
}

// Bevel: p0 = (width px, edges: 0 the picture's outline 1 the layer's frame, shape: 0 flat
// 1 round, light angle °), p1 = (light colour, intensity %).
@fragment
fn fs_bevel(in: VsOut) -> @location(0) vec4<f32> {
    let size = max(fx.extra.zw, vec2(1.0));
    let c = textureSample(src, src_sampler, to_tex(in.uv)) * inside(in.uv);
    let w = max(fx.p0.x, 0.5);
    let px = (in.uv - 0.5) * size;
    let round_ = fx.p0.z >= 0.5;
    var grad = vec2(0.0);
    if (fx.p0.y >= 0.5) {
        // The frame: how far in from the nearest edge, exactly.
        let dl = px.x + size.x * 0.5;
        let dr = size.x * 0.5 - px.x;
        let dt = px.y + size.y * 0.5;
        let db = size.y * 0.5 - px.y;
        let d = min(min(dl, dr), min(dt, db));
        var inward = vec2(1.0, 0.0);
        if (d == dr) {
            inward = vec2(-1.0, 0.0);
        } else if (d == dt) {
            inward = vec2(0.0, 1.0);
        } else if (d == db) {
            inward = vec2(0.0, -1.0);
        }
        grad = inward * bevel_slope(d / w, round_);
    } else {
        // The outline: height from the picture's opacity averaged round each point.
        let e = max(w / 3.0, 1.0);
        let hx = bevel_height(px + vec2(e, 0.0), w, size, round_) - bevel_height(px - vec2(e, 0.0), w, size, round_);
        let hy = bevel_height(px + vec2(0.0, e), w, size, round_) - bevel_height(px - vec2(0.0, e), w, size, round_);
        grad = vec2(hx, hy) / (2.0 * e);
    }
    let n = normalize(vec3(-grad, 1.0));
    let ang = radians(fx.p0.w);
    let l = normalize(vec3(cos(ang), -sin(ang), 1.0));
    let k = (dot(n, l) - l.z) * fx.p1.y / 100.0 * 1.6;
    let rgb = unpremul(c);
    let light = rgb_of(fx.p1.x);
    var out = rgb;
    if (k > 0.0) {
        out = rgb + (light - rgb) * clamp(k, 0.0, 1.0);
    } else {
        out = rgb * max(1.0 + k, 0.0);
    }
    return vec4(out * c.a, c.a);
}

// The bevel's slope at `h` of the way in (0 the edge, 1 its width in): 45° flat, or a
// quarter circle (steep at the edge, flat inside).
fn bevel_slope(h: f32, round_: bool) -> f32 {
    if (h >= 1.0 || h < 0.0) {
        return 0.0;
    }
    if (!round_) {
        return 1.0;
    }
    let q = 1.0 - h;
    return min(q / max(sqrt(max(1.0 - q * q, 0.0)), 0.125), 8.0);
}

// The outline bevel's height (pixels) at `q`: the opacity averaged over a disc as wide
// as the bevel, from 0.5 at the edge to 1 its width in, through the profile.
fn bevel_height(q: vec2<f32>, w: f32, size: vec2<f32>, round_: bool) -> f32 {
    var sum = geo_at(q, size).a;
    var n = 1.0;
    for (var ring = 1; ring <= 3; ring = ring + 1) {
        let r = w * f32(ring) / 3.0;
        for (var i = 0; i < 8; i = i + 1) {
            let a = f32(i) * 0.785398 + f32(ring) * 0.39;
            sum = sum + geo_at(q + vec2(cos(a), sin(a)) * r, size).a;
            n = n + 1.0;
        }
    }
    let h = clamp((sum / n - 0.5) * 2.0, 0.0, 1.0);
    if (round_) {
        let q2 = 1.0 - h;
        return sqrt(max(1.0 - q2 * q2, 0.0)) * w;
    }
    return h * w;
}

// Extrude: p0 = (length px, towards: 0 a direction 1 a point, direction °, point x %),
// p1 = (point y %, sides: 0 a darker picture 1 a colour, side colour, shading %),
// p2.x = fade %.
@fragment
fn fs_extrude(in: VsOut) -> @location(0) vec4<f32> {
    let size = max(fx.extra.zw, vec2(1.0));
    let front = textureSample(src, src_sampler, to_tex(in.uv)) * inside(in.uv);
    if (front.a >= 0.999) {
        return front;
    }
    let len = max(fx.p0.x, 0.0);
    let p = (in.uv - 0.5) * size;
    let to_point = fx.p0.y >= 0.5;
    let vp = (vec2(fx.p0.w, fx.p1.x) / 100.0 - 0.5) * size;
    let ang = radians(fx.p0.z);
    let dir = vec2(cos(ang), sin(ang));
    let fmax = clamp(len / max(length(size) * 0.5, 1.0), 0.0, 0.98);
    let steps = i32(clamp(ceil(len), 1.0, 256.0));
    var side = vec4(0.0);
    for (var i = 1; i <= steps; i = i + 1) {
        let t = f32(i) / f32(steps);
        var q = p - dir * len * t;
        if (to_point) {
            let fr = fmax * t;
            q = (p - vp * fr) / max(1.0 - fr, 0.02);
        }
        let s = geo_at(q, size);
        if (s.a > 0.5) {
            let shade = clamp(fx.p1.w / 100.0, 0.0, 1.0) * (0.4 + 0.6 * t);
            var rgb = unpremul(s) * (1.0 - shade);
            if (fx.p1.y >= 0.5) {
                rgb = rgb_of(fx.p1.z) * (1.0 - shade * 0.6);
            }
            let a = 1.0 - clamp(fx.p2.x / 100.0, 0.0, 1.0) * t;
            side = vec4(rgb * a, a);
            break;
        }
    }
    return front + side * (1.0 - front.a);
}

// 3D Extrusion: p0 = (depth px, bevel px, bevel shape: 0 flat 1 round, turn x), p1 = (turn y,
// turn z, size %, perspective °), p2 = (sides: 0 the picture's edge colours 1 a colour, side
// colour, light angle °, light height °), p3 = (light %, ambient %, shine %, back: 0 the
// picture 1 side colour), p4.x = solid above (opacity %).
@fragment
fn fs_extrusion3d(in: VsOut) -> @location(0) vec4<f32> {
    let size = max(fx.extra.zw, vec2(1.0));
    let depth = max(fx.p0.x, 0.0);
    let bevel = clamp(fx.p0.y, 0.0, depth * 0.5);
    let round_ = fx.p0.z >= 0.5;
    let r = geo_turn(vec3(fx.p0.w, fx.p1.x, fx.p1.y));
    let rt = transpose(r);
    let k = max(fx.p1.z, 1.0) / 100.0;
    let f = geo_focal(fx.p1.w, size.y);
    let flat_view = f >= 1e6;
    let th = clamp(fx.p4.x / 100.0, 0.01, 0.99);
    let p = (in.uv - 0.5) * size;
    // The ray, into the object's own space (unturned about its middle, unscaled).
    var o = vec3(0.0, 0.0, -f);
    var dir = normalize(vec3(p, f));
    if (flat_view) {
        o = vec3(p, -1e4);
        dir = vec3(0.0, 0.0, 1.0);
    }
    let mid = vec3(0.0, 0.0, depth * 0.5);
    let oo = rt * (o - mid) / k + mid;
    let dd = normalize(rt * dir);
    // Its box.
    let lo = vec3(-size * 0.5, 0.0);
    let hi = vec3(size * 0.5, max(depth, 0.01));
    let inv = 1.0 / select(dd, vec3(1e-6), abs(dd) < vec3(1e-6));
    let t1 = (lo - oo) * inv;
    let t2 = (hi - oo) * inv;
    let tn = max(max(min(t1.x, t2.x), min(t1.y, t2.y)), min(t1.z, t2.z));
    let tf = min(min(max(t1.x, t2.x), max(t1.y, t2.y)), max(t1.z, t2.z));
    if (tf <= max(tn, 0.0)) {
        return vec4(0.0);
    }
    let t0 = max(tn, 0.0);
    let span = tf - t0;
    let step = max(span / 192.0, 0.75);
    let n = i32(min(ceil(span / step), 192.0));
    var t = t0 + 1e-3;
    var hit = false;
    for (var i = 0; i <= n; i = i + 1) {
        if (ex_solid(oo + dd * t, size, depth, bevel, round_, th)) {
            hit = true;
            break;
        }
        t = t + step;
    }
    if (!hit) {
        return vec4(0.0);
    }
    // Narrow it down.
    var a = max(t - step, t0);
    var b = t;
    for (var j = 0; j < 7; j = j + 1) {
        let m = (a + b) * 0.5;
        if (ex_solid(oo + dd * m, size, depth, bevel, round_, th)) {
            b = m;
        } else {
            a = m;
        }
    }
    let q = oo + dd * b;
    // Which surface: the front or back face, a bevel, or the sides.
    let ga = vec2(geo_at(q.xy + vec2(1.0, 0.0), size).a - geo_at(q.xy - vec2(1.0, 0.0), size).a, geo_at(q.xy + vec2(0.0, 1.0), size).a - geo_at(q.xy - vec2(0.0, 1.0), size).a);
    let out2 = select(vec2(0.0), -normalize(ga), length(ga) > 1e-5);
    let side_n = vec3(out2, 0.0);
    let face = 0.6;
    var nrm = side_n;
    var region = 1; // 0 front, 1 sides, 2 back
    if (q.z <= face) {
        nrm = vec3(0.0, 0.0, -1.0);
        region = 0;
    } else if (q.z >= depth - face) {
        nrm = vec3(0.0, 0.0, 1.0);
        region = 2;
    } else if (bevel > 0.0 && q.z < bevel) {
        let w = (bevel - q.z) / bevel;
        nrm = select(normalize(side_n + vec3(0.0, 0.0, -1.0)), normalize(side_n * sqrt(max(1.0 - w * w, 0.0)) + vec3(0.0, 0.0, -w)), round_);
        region = 0;
    } else if (bevel > 0.0 && q.z > depth - bevel) {
        let w = (q.z - (depth - bevel)) / bevel;
        nrm = select(normalize(side_n + vec3(0.0, 0.0, 1.0)), normalize(side_n * sqrt(max(1.0 - w * w, 0.0)) + vec3(0.0, 0.0, w)), round_);
        region = 2;
    }
    if (length(nrm) < 0.5) {
        nrm = select(vec3(0.0, 0.0, -1.0), vec3(0.0, 0.0, 1.0), q.z > depth * 0.5);
    }
    // Its colour.
    let side_col = rgb_of(fx.p2.y);
    var col = unpremul(geo_at(q.xy - out2 * 1.5, size));
    if (region == 1 && fx.p2.x >= 0.5) {
        col = side_col;
    } else if (region == 2 && fx.p3.w >= 0.5) {
        col = side_col;
    }
    // Lit: ambient, the light, and its highlight.
    let nw = normalize(r * nrm);
    let az = radians(fx.p2.z);
    let el = radians(clamp(fx.p2.w, 0.0, 90.0));
    let l = normalize(vec3(cos(el) * cos(az), -cos(el) * sin(az), -sin(el)));
    let v = -dir;
    let amb = clamp(fx.p3.y / 100.0, 0.0, 1.0);
    let lit = fx.p3.x / 100.0;
    // (As bright as the picture where a face looks straight at the viewer, unturned.)
    let base = amb + (1.0 - amb) * lit * max(-l.z, 0.0);
    let diffuse = (amb + (1.0 - amb) * lit * max(dot(nw, l), 0.0)) / max(base, 0.2);
    let spec = fx.p3.z / 100.0 * 0.6 * pow(max(dot(nw, normalize(l + v)), 0.0), 64.0);
    return vec4(col * diffuse + vec3(spec), 1.0);
}

// Is object-space point `q` inside the extrusion: inside the picture's shape (opacity
// above `th`), drawn in near the faces by the bevel?
fn ex_solid(q: vec3<f32>, size: vec2<f32>, depth: f32, bevel: f32, round_: bool, th: f32) -> bool {
    if (geo_at(q.xy, size).a < th) {
        return false;
    }
    var e = 0.0;
    if (bevel > 0.0) {
        let zf = q.z;
        let zb = depth - q.z;
        let z = min(zf, zb);
        if (z < bevel) {
            if (round_) {
                let u = bevel - z;
                e = bevel - sqrt(max(bevel * bevel - u * u, 0.0));
            } else {
                e = bevel - z;
            }
        }
    }
    if (e < 0.5) {
        return true;
    }
    for (var i = 0; i < 8; i = i + 1) {
        let a = f32(i) * 0.785398;
        if (geo_at(q.xy + vec2(cos(a), sin(a)) * e, size).a < th) {
            return false;
        }
    }
    return true;
}

// ---- Batch: blur & sharpen, channel, colour correction ----

// The layer at uv (layer uv), nothing outside the layer.
fn cx_at(uv: vec2<f32>) -> vec4<f32> {
    return textureSampleLevel(src, src_sampler, to_tex(uv), 0.0) * inside(uv);
}

// The layer at uv, its edge pixels repeated outside it (`repeat`), or nothing.
fn cx_edge(uv: vec2<f32>, repeat: bool) -> vec4<f32> {
    if (repeat) {
        return textureSampleLevel(src, src_sampler, to_tex(clamp(uv, vec2(0.0), vec2(1.0))), 0.0);
    }
    return textureSampleLevel(src, src_sampler, to_tex(uv), 0.0);
}

// One frame pixel (at 100%) in layer uv.
fn cx_px() -> vec2<f32> {
    return 1.0 / max(fx.extra.zw, vec2(1.0));
}

// Display (gamma) values of a premultiplied linear colour, and back.
fn cx_disp(c: vec4<f32>) -> vec3<f32> {
    return to_display(unpremul(c));
}
fn cx_out(rgb: vec3<f32>, a: f32) -> vec4<f32> {
    return vec4(to_linear(clamp(rgb, vec3(0.0), vec3(1.0))) * a, a);
}

fn cx_rgb2hsl(c: vec3<f32>) -> vec3<f32> {
    let mx = max(c.r, max(c.g, c.b));
    let mn = min(c.r, min(c.g, c.b));
    let l = (mx + mn) * 0.5;
    let d = mx - mn;
    if (d < 1e-6) {
        return vec3(0.0, 0.0, l);
    }
    let s = d / max(1.0 - abs(2.0 * l - 1.0), 1e-6);
    var h = 0.0;
    if (mx == c.r) {
        h = (c.g - c.b) / d;
        if (h < 0.0) {
            h = h + 6.0;
        }
    } else if (mx == c.g) {
        h = (c.b - c.r) / d + 2.0;
    } else {
        h = (c.r - c.g) / d + 4.0;
    }
    return vec3(h / 6.0, clamp(s, 0.0, 1.0), l);
}

fn cx_hsl2rgb(h: vec3<f32>) -> vec3<f32> {
    let c = (1.0 - abs(2.0 * h.z - 1.0)) * h.y;
    let base = clamp(abs((fract(h.x) * 6.0 + vec3(0.0, 4.0, 2.0)) % 6.0 - 3.0) - 1.0, vec3(0.0), vec3(1.0));
    return h.z + c * (base - 0.5);
}

// A channel of a display colour (with its alpha) by number: 0 red, 1 green, 2 blue,
// 3 alpha, 4 luminance, 5 hue, 6 lightness, 7 saturation, 8 full, 9 half, 10 off.
fn cx_chan(c: vec3<f32>, a: f32, which: u32) -> f32 {
    switch which {
        case 0u: { return c.r; }
        case 1u: { return c.g; }
        case 2u: { return c.b; }
        case 3u: { return a; }
        case 4u: { return dot(c, LUMA); }
        case 5u: { return cx_rgb2hsl(c).x; }
        case 6u: { return cx_rgb2hsl(c).z; }
        case 7u: { return cx_rgb2hsl(c).y; }
        case 8u: { return 1.0; }
        case 9u: { return 0.5; }
        default: { return 0.0; }
    }
}

// Blend modes on display colours 0–1: 0 normal, 1 multiply, 2 screen, 3 overlay,
// 4 soft light, 5 hard light, 6 colour dodge, 7 colour burn, 8 darken, 9 lighten,
// 10 difference, 11 add, 12 subtract.
fn cx_blend(a: vec3<f32>, b: vec3<f32>, mode: u32) -> vec3<f32> {
    switch mode {
        case 1u: { return a * b; }
        case 2u: { return 1.0 - (1.0 - a) * (1.0 - b); }
        case 3u: { return select(1.0 - 2.0 * (1.0 - a) * (1.0 - b), 2.0 * a * b, a < vec3(0.5)); }
        case 4u: { return select(a + (2.0 * b - 1.0) * (sqrt(a) - a), a - (1.0 - 2.0 * b) * a * (1.0 - a), b < vec3(0.5)); }
        case 5u: { return select(1.0 - 2.0 * (1.0 - a) * (1.0 - b), 2.0 * a * b, b < vec3(0.5)); }
        case 6u: { return min(a / max(1.0 - b, vec3(1e-3)), vec3(1.0)); }
        case 7u: { return 1.0 - min((1.0 - a) / max(b, vec3(1e-3)), vec3(1.0)); }
        case 8u: { return min(a, b); }
        case 9u: { return max(a, b); }
        case 10u: { return abs(a - b); }
        case 11u: { return min(a + b, vec3(1.0)); }
        case 12u: { return max(a - b, vec3(0.0)); }
        default: { return b; }
    }
}

// Box blur, one direction: p0 = (radius in texels, repeat edges); texel_dir.zw = direction.
@fragment
fn fs_box_blur(in: VsOut) -> @location(0) vec4<f32> {
    let r = max(fx.p0.x, 0.0);
    let n = min(ceil(r), 100.0);
    let step = r / max(n, 1.0);
    let dir = fx.texel_dir.xy * fx.texel_dir.zw;
    let rep = fx.p0.y >= 0.5;
    var sum = cx_edge(in.uv, rep);
    for (var i = 1.0; i <= n; i = i + 1.0) {
        sum = sum + cx_edge(in.uv + dir * i * step, rep) + cx_edge(in.uv - dir * i * step, rep);
    }
    return sum / (2.0 * n + 1.0);
}

// Channel blur, one direction: p0 = sigma (texels) for red, green, blue, alpha; p1.x = repeat edges.
@fragment
fn fs_channel_blur(in: VsOut) -> @location(0) vec4<f32> {
    let sig = max(fx.p0, vec4(0.01));
    let big = max(max(sig.x, sig.y), max(sig.z, sig.w));
    let reach = 3.0 * big;
    let n = min(ceil(reach), 64.0);
    let step = reach / max(n, 1.0);
    let dir = fx.texel_dir.xy * fx.texel_dir.zw;
    let rep = fx.p1.x >= 0.5;
    var sum = cx_edge(in.uv, rep);
    var total = vec4(1.0);
    for (var i = 1.0; i <= n; i = i + 1.0) {
        let x = i * step;
        let w = exp(-0.5 * x * x / (sig * sig));
        sum = sum + w * (cx_edge(in.uv + dir * x, rep) + cx_edge(in.uv - dir * x, rep));
        total = total + 2.0 * w;
    }
    var c = sum / total;
    // (Colour can't outweigh opacity once the channels go their own ways.)
    c = vec4(min(c.rgb, vec3(max(c.a, 1e-4)) * 4.0), c.a);
    return c;
}

// Smart blur: p0 = (radius px, threshold %, mode).
@fragment
fn fs_smart_blur(in: VsOut) -> @location(0) vec4<f32> {
    let c0 = cx_at(in.uv);
    let d0 = cx_disp(c0);
    let r = max(fx.p0.x, 0.0);
    let n = i32(min(ceil(r), 6.0));
    let gap = r / max(f32(n), 1.0);
    let thr = max(fx.p0.y / 100.0 * 0.5, 1e-3);
    let px = cx_px();
    var sum = vec4(0.0);
    var w = 0.0;
    var out_n = 0.0;
    var all_n = 0.0;
    for (var y = -n; y <= n; y = y + 1) {
        for (var x = -n; x <= n; x = x + 1) {
            let o = vec2(f32(x), f32(y)) * gap;
            if (dot(o, o) > r * r + 0.01) {
                continue;
            }
            let s = cx_at(in.uv + o * px);
            let k = 1.0 - smoothstep(thr * 0.7, thr, length(cx_disp(s) - d0));
            sum = sum + s * k;
            w = w + k;
            out_n = out_n + 1.0 - k;
            all_n = all_n + 1.0;
        }
    }
    let blurred = select(c0, sum / max(w, 1e-4), w > 0.0);
    let edge = clamp(out_n / max(all_n, 1.0) * 3.0, 0.0, 1.0);
    switch u32(round(fx.p0.z)) {
        case 1u: { return vec4(vec3(step(0.5, edge)), 1.0); }
        case 2u: { return vec4(mix(blurred.rgb, vec3(blurred.a), step(0.5, edge)), blurred.a); }
        default: { return blurred; }
    }
}

// Camera lens blur: p0 = (radius px, iris: 0 round, n−1 sides…, rotation °, highlight gain %),
// p1 = (highlight threshold %, repeat edges).
@fragment
fn fs_lens_blur(in: VsOut) -> @location(0) vec4<f32> {
    let r = max(fx.p0.x, 0.0);
    let shape = u32(round(fx.p0.y));
    let sides = f32(shape + 2u);
    let rot = radians(fx.p0.z);
    let gain = max(fx.p0.w, 0.0) / 100.0 * 4.0;
    let thr = clamp(fx.p1.x / 100.0, 0.0, 0.99);
    let rep = fx.p1.y >= 0.5;
    let px = cx_px();
    let count = 160;
    var sum = vec4(0.0);
    var w = 0.0;
    for (var i = 0; i < count; i = i + 1) {
        let fi = f32(i) + 0.5;
        let th = fi * 2.39996;
        var rad = sqrt(fi / f32(count));
        if (shape > 0u) {
            let seg = 6.28318 / sides;
            let a = ((th - rot) % seg + seg) % seg - seg * 0.5;
            rad = rad * cos(seg * 0.5) / cos(a);
        }
        let o = vec2(cos(th), sin(th)) * rad * r;
        let s = cx_edge(in.uv + o * px, rep);
        let l = dot(to_display(unpremul(s)), LUMA);
        let k = 1.0 + gain * smoothstep(thr, min(thr + 0.15, 1.0), l) * s.a;
        sum = sum + s * k;
        w = w + k;
    }
    return sum / max(w, 1e-4);
}

fn vb_map(uv: vec2<f32>, which: u32) -> f32 {
    let c = cx_at(uv);
    let d = cx_disp(c);
    switch which {
        case 1u: { return d.r * c.a; }
        case 2u: { return d.g * c.a; }
        case 3u: { return d.b * c.a; }
        case 4u: { return c.a; }
        default: { return dot(d, LUMA) * c.a; }
    }
}

fn vb_dir(uv: vec2<f32>, s: f32, which: u32, kind: u32, ang: f32) -> vec2<f32> {
    let px = cx_px();
    if (kind == 2u) {
        return vec2(cos(ang), sin(ang));
    }
    let g = vec2(vb_map(uv + vec2(s, 0.0) * px, which) - vb_map(uv - vec2(s, 0.0) * px, which), vb_map(uv + vec2(0.0, s) * px, which) - vb_map(uv - vec2(0.0, s) * px, which));
    if (length(g) < 1e-5) {
        return vec2(0.0);
    }
    var d = normalize(g);
    if (kind == 0u) {
        d = vec2(-d.y, d.x);
    }
    let c = cos(ang);
    let sn = sin(ang);
    return vec2(d.x * c - d.y * sn, d.x * sn + d.y * c);
}

// Vector blur: p0 = (amount px, direction: 0 along 1 across 2 one angle, angle °, smoothness px),
// p1.x = shapes from.
@fragment
fn fs_vector_blur(in: VsOut) -> @location(0) vec4<f32> {
    let amount = max(fx.p0.x, 0.0);
    let kind = u32(round(fx.p0.y));
    let ang = radians(fx.p0.z);
    let s = max(fx.p0.w, 0.5);
    let which = u32(round(fx.p1.x));
    let px = cx_px();
    let steps = 12;
    let h = amount / f32(steps);
    var sum = cx_at(in.uv);
    var n = 1.0;
    for (var side = 0; side < 2; side = side + 1) {
        let sg = select(1.0, -1.0, side == 1);
        var p = in.uv;
        var last = vb_dir(p, s, which, kind, ang) * sg;
        for (var i = 0; i < steps; i = i + 1) {
            var d = vb_dir(p, s, which, kind, ang) * sg;
            if (dot(d, last) < 0.0) {
                d = -d;
            }
            if (length(d) < 0.5) {
                break;
            }
            p = p + d * h * px;
            last = d;
            sum = sum + cx_at(p);
            n = n + 1.0;
        }
    }
    return sum / n;
}

fn ar_op(a: f32, b: f32, op: u32, clip: bool) -> f32 {
    let ai = u32(clamp(round(a), 0.0, 255.0));
    let bi = u32(clamp(round(b), 0.0, 255.0));
    var r = a;
    switch op {
        case 0u: { r = f32(ai & bi); }
        case 1u: { r = f32(ai | bi); }
        case 2u: { r = f32(ai ^ bi); }
        case 3u: { r = a + b; }
        case 4u: { r = a - b; }
        case 5u: { r = abs(a - b); }
        case 6u: { r = min(a, b); }
        case 7u: { r = max(a, b); }
        case 8u: { r = select(a, 0.0, a > b); }
        case 9u: { r = select(a, 0.0, a < b); }
        case 10u: { r = select(0.0, 255.0, a > b); }
        case 11u: { r = a * b / 255.0; }
        default: { r = 255.0 - (255.0 - a) * (255.0 - b) / 255.0; }
    }
    if (clip) {
        return clamp(r, 0.0, 255.0);
    }
    return ((r % 256.0) + 256.0) % 256.0;
}

// Arithmetic: p0 = (operator, red, green, blue), p1.x = clip.
@fragment
fn fs_arithmetic(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    let d = cx_disp(c) * 255.0;
    let op = u32(round(fx.p0.x));
    let clip = fx.p1.x >= 0.5;
    let o = vec3(ar_op(d.r, fx.p0.y, op, clip), ar_op(d.g, fx.p0.z, op, clip), ar_op(d.b, fx.p0.w, op, clip));
    return cx_out(o / 255.0, c.a);
}

// Blend: p0 = (colour, mode, blend with original %).
@fragment
fn fs_blend_colour(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    let d = cx_disp(c);
    let col = to_display(rgb_of(fx.p0.x));
    var o = d;
    switch u32(round(fx.p0.y)) {
        case 1u: {
            let h = cx_rgb2hsl(col);
            o = cx_hsl2rgb(vec3(h.x, h.y, cx_rgb2hsl(d).z));
        }
        case 2u: { o = dot(d, LUMA) * col / max(dot(col, LUMA), 1e-3); }
        case 3u: { o = min(d, col); }
        case 4u: { o = max(d, col); }
        default: { o = col; }
    }
    let k = 1.0 - clamp(fx.p0.z / 100.0, 0.0, 1.0);
    return cx_out(mix(d, o, k), c.a);
}

// Calculations: p0 = (input, invert input, second, invert second), p1 = (second opacity %, mode, preserve).
@fragment
fn fs_calculations(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    let d = cx_disp(c);
    var a = d;
    var alpha = c.a;
    switch u32(round(fx.p0.x)) {
        case 1u: { a = vec3(dot(d, LUMA)); }
        case 2u: { a = vec3(d.r); }
        case 3u: { a = vec3(d.g); }
        case 4u: { a = vec3(d.b); }
        case 5u: { a = vec3(c.a); alpha = 1.0; }
        default: {}
    }
    if (fx.p0.y >= 0.5) {
        a = 1.0 - a;
    }
    var b = 0.0;
    switch u32(round(fx.p0.z)) {
        case 1u: { b = d.r; }
        case 2u: { b = d.g; }
        case 3u: { b = d.b; }
        case 4u: { b = c.a; }
        default: { b = dot(d, LUMA); }
    }
    if (fx.p0.w >= 0.5) {
        b = 1.0 - b;
    }
    let o = mix(a, cx_blend(a, vec3(b), u32(round(fx.p1.y))), clamp(fx.p1.x / 100.0, 0.0, 1.0));
    if (fx.p1.z < 0.5) {
        alpha = 1.0;
    }
    return cx_out(o, alpha);
}

// Channel mixer: p0 = red from (r, g, b, const) %, p1 = green, p2 = blue, p3.x = monochrome.
@fragment
fn fs_channel_mixer(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    let d = vec4(cx_disp(c), 1.0);
    var o = vec3(dot(d, fx.p0), dot(d, fx.p1), dot(d, fx.p2)) / 100.0;
    if (fx.p3.x >= 0.5) {
        o = vec3(o.r);
    }
    return cx_out(o, c.a);
}

// Invert: p0 = (channel, blend with original %).
@fragment
fn fs_invert(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    let d = cx_disp(c);
    var o = d;
    var a = c.a;
    switch u32(round(fx.p0.x)) {
        case 1u: { o.r = 1.0 - d.r; }
        case 2u: { o.g = 1.0 - d.g; }
        case 3u: { o.b = 1.0 - d.b; }
        case 4u: { a = 1.0 - c.a; }
        case 5u: { let y = dot(d, LUMA); o = d + (1.0 - 2.0 * y); }
        case 6u: { let h = cx_rgb2hsl(d); o = cx_hsl2rgb(vec3(fract(h.x + 0.5), h.y, h.z)); }
        case 7u: { let h = cx_rgb2hsl(d); o = cx_hsl2rgb(vec3(h.x, h.y, 1.0 - h.z)); }
        case 8u: { let h = cx_rgb2hsl(d); o = cx_hsl2rgb(vec3(h.x, 1.0 - h.y, h.z)); }
        default: { o = 1.0 - d; }
    }
    let k = 1.0 - clamp(fx.p0.y / 100.0, 0.0, 1.0);
    let rgb = mix(d, clamp(o, vec3(0.0), vec3(1.0)), k);
    let alpha = mix(c.a, a, k);
    // (Alpha inverted: what was clear becomes the colour under it, here its own.)
    return vec4(to_linear(rgb) * alpha, alpha);
}

// Shift channels: p0 = (red from, green from, blue from, alpha from) as numbers for cx_chan.
@fragment
fn fs_shift_channels(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    let d = cx_disp(c);
    var o = vec4(cx_chan(d, c.a, u32(round(fx.p0.x))), cx_chan(d, c.a, u32(round(fx.p0.y))), cx_chan(d, c.a, u32(round(fx.p0.z))), cx_chan(d, c.a, u32(round(fx.p0.w))));
    return cx_out(o.rgb, clamp(o.a, 0.0, 1.0));
}

// Set channels: p0 = (red from, green from, blue from, alpha from) as numbers for cx_chan; p1 = invert each.
@fragment
fn fs_set_channels(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    let d = cx_disp(c);
    var o = vec4(cx_chan(d, c.a, u32(round(fx.p0.x))), cx_chan(d, c.a, u32(round(fx.p0.y))), cx_chan(d, c.a, u32(round(fx.p0.z))), cx_chan(d, c.a, u32(round(fx.p0.w))));
    o = select(o, 1.0 - o, fx.p1 >= vec4(0.5));
    return cx_out(o.rgb, clamp(o.a, 0.0, 1.0));
}

// Set matte: p0 = (from, invert, keep its own transparency).
@fragment
fn fs_set_matte(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    let d = cx_disp(c);
    var m = cx_chan(d, c.a, u32(round(fx.p0.x)));
    if (fx.p0.y >= 0.5) {
        m = 1.0 - m;
    }
    if (fx.p0.z >= 0.5) {
        m = m * c.a;
    }
    return cx_out(d, clamp(m, 0.0, 1.0));
}

// Solid composite: p0 = (source opacity %, colour, opacity %, mode).
@fragment
fn fs_solid_composite(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv)) * inside(in.uv);
    let sa = c.a * clamp(fx.p0.x / 100.0, 0.0, 1.0);
    let ba = clamp(fx.p0.z / 100.0, 0.0, 1.0) * inside(in.uv);
    let back = to_display(rgb_of(fx.p0.y));
    let top = cx_disp(c);
    // The layer blended onto the colour where both are, over it where only it is.
    let blended = mix(top, cx_blend(back, top, u32(round(fx.p0.w))), ba);
    let a = sa + ba * (1.0 - sa);
    let rgb = (to_linear(blended) * sa + to_linear(back) * ba * (1.0 - sa)) / max(a, 1e-5);
    return vec4(rgb * a, a);
}

// Minimax, one direction: p0 = (0 minimum 1 maximum, radius texels, channel).
@fragment
fn fs_minimax(in: VsOut) -> @location(0) vec4<f32> {
    let mx = fx.p0.x >= 0.5;
    let r = max(fx.p0.y, 0.0);
    let n = min(ceil(r), 100.0);
    let step = r / max(n, 1.0);
    let dir = fx.texel_dir.xy * fx.texel_dir.zw;
    let ch = u32(round(fx.p0.z));
    let c0 = textureSample(src, src_sampler, to_tex(in.uv));
    var best = c0;
    for (var i = -n; i <= n; i = i + 1.0) {
        let s = textureSampleLevel(src, src_sampler, to_tex(in.uv + dir * i * step), 0.0);
        if (mx) {
            best = max(best, s);
        } else {
            best = min(best, s);
        }
    }
    switch ch {
        // Colour only: the colour's extreme, at its own opacity.
        case 0u: { return vec4(unpremul(best) * c0.a, c0.a); }
        case 2u: { return vec4(unpremul(c0) * best.a, best.a); }
        case 3u: { return vec4(vec3(best.r, c0.g, c0.b), c0.a); }
        case 4u: { return vec4(vec3(c0.r, best.g, c0.b), c0.a); }
        case 5u: { return vec4(vec3(c0.r, c0.g, best.b), c0.a); }
        default: { return best; }
    }
}

// Statistics of the layer (display values of its visible pixels, a 64 × 64 grid) written
// into the target's first pixels: (0,0) black points, (1,0) white points, (2,0) means —
// red, green, blue, luminance; with p0 = (black clip %, white clip %). For equalising,
// p0.z = 1: pixel (x, y) for x < 64 is the share at or below level x/63 of channel y
// (0 luminance, 1 red, 2 green, 3 blue).
@fragment
fn fs_stats(in: VsOut) -> @location(0) vec4<f32> {
    let p = vec2<i32>(in.pos.xy);
    let equalise = fx.p0.z >= 0.5;
    if (p.y >= select(1, 4, equalise) || p.x >= select(3, 64, equalise)) {
        return vec4(0.0);
    }
    let n = 64;
    if (equalise) {
        var below = 0.0;
        var total = 0.0;
        let level = f32(p.x) / 63.0;
        for (var j = 0; j < n; j = j + 1) {
            for (var i = 0; i < n; i = i + 1) {
                let uv = (vec2(f32(i), f32(j)) + 0.5) / f32(n);
                let c = cx_at(uv);
                if (c.a < 0.05) {
                    continue;
                }
                let d = cx_disp(c);
                var v = dot(d, LUMA);
                if (p.y == 1) { v = d.r; } else if (p.y == 2) { v = d.g; } else if (p.y == 3) { v = d.b; }
                below = below + select(0.0, 1.0, v <= level + 0.5 / 63.0);
                total = total + 1.0;
            }
        }
        return vec4(select(level, below / total, total > 0.0));
    }
    var hist: array<vec4<f32>, 64>;
    for (var k = 0; k < 64; k = k + 1) {
        hist[k] = vec4(0.0);
    }
    var sum = vec4(0.0);
    var count = 0.0;
    for (var j = 0; j < n; j = j + 1) {
        for (var i = 0; i < n; i = i + 1) {
            let uv = (vec2(f32(i), f32(j)) + 0.5) / f32(n);
            let c = cx_at(uv);
            if (c.a < 0.05) {
                continue;
            }
            let d = cx_disp(c);
            let v = vec4(d, dot(d, LUMA));
            let b = vec4<i32>(clamp(round(v * 63.0), vec4(0.0), vec4(63.0)));
            hist[b.x].x = hist[b.x].x + 1.0;
            hist[b.y].y = hist[b.y].y + 1.0;
            hist[b.z].z = hist[b.z].z + 1.0;
            hist[b.w].w = hist[b.w].w + 1.0;
            sum = sum + v;
            count = count + 1.0;
        }
    }
    if (count < 1.0) {
        return select(vec4(0.0), select(vec4(1.0), vec4(0.5), p.x == 2), p.x != 0);
    }
    if (p.x == 2) {
        return sum / count;
    }
    // A clip share from the dark end (black points) or the bright end (white points).
    let clip = clamp(select(fx.p0.x, fx.p0.y, p.x == 1) / 100.0, 0.0, 0.5) * count;
    var acc = vec4(0.0);
    var found = vec4(-1.0);
    for (var k = 0; k < 64; k = k + 1) {
        let idx = select(k, 63 - k, p.x == 1);
        acc = acc + hist[idx];
        let lv = f32(idx) / 63.0;
        found = select(found, vec4(lv), (found < vec4(0.0)) & (acc > vec4(clip)));
    }
    return max(found, vec4(0.0));
}

fn cc_match(d: vec3<f32>, key: vec3<f32>, how: u32) -> f32 {
    switch how {
        case 1u: {
            let a = cx_rgb2hsl(d).x;
            let b = cx_rgb2hsl(key).x;
            let dh = abs(a - b);
            return min(dh, 1.0 - dh) * 2.0;
        }
        case 2u: {
            let ca = vec2(d.b - dot(d, LUMA), d.r - dot(d, LUMA));
            let cb = vec2(key.b - dot(key, LUMA), key.r - dot(key, LUMA));
            return length(ca - cb) * 1.2;
        }
        default: { return length(d - key) / 1.732; }
    }
}

// Change colour: p0 = (view, hue °, lightness, saturation), p1 = (colour, tolerance %,
// softness %, match), p2.x = invert.
@fragment
fn fs_change_color(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    let d = cx_disp(c);
    let key = to_display(rgb_of(fx.p1.x));
    let dist = cc_match(d, key, u32(round(fx.p1.w)));
    let tol = fx.p1.y / 100.0;
    let soft = fx.p1.z / 100.0;
    var m = 1.0 - smoothstep(tol, tol + soft + 1e-4, dist);
    if (fx.p2.x >= 0.5) {
        m = 1.0 - m;
    }
    if (fx.p0.x >= 0.5) {
        return vec4(vec3(m) * c.a, c.a);
    }
    var h = cx_rgb2hsl(d);
    h.x = fract(h.x + fx.p0.y / 360.0);
    h.z = clamp(h.z + fx.p0.z / 100.0 * select(h.z, 1.0 - h.z, fx.p0.z > 0.0), 0.0, 1.0);
    h.y = clamp(h.y * (1.0 + fx.p0.w / 100.0), 0.0, 1.0);
    return cx_out(mix(d, cx_hsl2rgb(h), m), c.a);
}

// Change to colour: p0 = (from, to, change, hue tolerance %), p1 = (lightness tolerance %,
// saturation tolerance %, softness %, view mask).
@fragment
fn fs_change_to_color(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    let d = cx_disp(c);
    let a = cx_rgb2hsl(to_display(rgb_of(fx.p0.x)));
    let b = cx_rgb2hsl(to_display(rgb_of(fx.p0.y)));
    let h = cx_rgb2hsl(d);
    let dh0 = abs(h.x - a.x);
    let dh = min(dh0, 1.0 - dh0) * 2.0;
    let tol = vec3(fx.p0.w, fx.p1.x, fx.p1.y) / 100.0;
    let soft = fx.p1.z / 100.0 + 1e-4;
    let diff = vec3(dh, abs(h.z - a.z), abs(h.y - a.y));
    let k = (1.0 - smoothstep(tol, tol * (1.0 + soft) + soft * 0.1, diff));
    let m = k.x * k.y * k.z;
    if (fx.p1.w >= 0.5) {
        return vec4(vec3(m) * c.a, c.a);
    }
    var o = h;
    let dhue = b.x - a.x;
    o.x = fract(h.x + dhue);
    let ch = u32(round(fx.p0.z));
    if (ch == 1u || ch == 2u) {
        o.z = clamp(h.z + (b.z - a.z), 0.0, 1.0);
    }
    if (ch == 2u || ch == 3u) {
        o.y = clamp(h.y + (b.y - a.y), 0.0, 1.0);
    }
    return cx_out(mix(d, cx_hsl2rgb(o), m), c.a);
}

// Colour balance: p0.xyz shadows, p0.w/p1.xy midtones, p1.zw/p2.x highlights (−100…100), p2.y preserve.
@fragment
fn fs_color_balance(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    let d = cx_disp(c);
    let y = dot(d, LUMA);
    let ws = 1.0 - smoothstep(0.0, 0.5, y);
    let wh = smoothstep(0.5, 1.0, y);
    let wm = 1.0 - ws - wh;
    let s = vec3(fx.p0.x, fx.p0.y, fx.p0.z);
    let m = vec3(fx.p0.w, fx.p1.x, fx.p1.y);
    let h = vec3(fx.p1.z, fx.p1.w, fx.p2.x);
    var o = d + (s * ws + m * wm + h * wh) / 100.0 * 0.5;
    if (fx.p2.y >= 0.5) {
        o = o + (y - dot(o, LUMA));
    }
    return cx_out(o, c.a);
}

// Exposure: p0 = (stops, offset, gamma).
@fragment
fn fs_exposure(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    let lin = unpremul(c) * exp2(fx.p0.x) + fx.p0.y;
    let g = pow(max(lin, vec3(0.0)), vec3(1.0 / max(fx.p0.z, 0.01)));
    return vec4(g * c.a, c.a);
}

// Leave colour: p0 = (amount %, colour, tolerance %, softness %), p1.x = match by hue.
@fragment
fn fs_leave_color(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    let d = cx_disp(c);
    let dist = cc_match(d, to_display(rgb_of(fx.p0.y)), select(0u, 1u, fx.p1.x >= 0.5));
    let tol = fx.p0.z / 100.0;
    let keep = 1.0 - smoothstep(tol, tol + fx.p0.w / 100.0 + 1e-4, dist);
    let grey = vec3(dot(d, LUMA));
    return cx_out(mix(d, grey, clamp(fx.p0.x / 100.0, 0.0, 1.0) * (1.0 - keep)), c.a);
}

fn pf_colour(i: u32) -> vec3<f32> {
    var t = array<vec3<f32>, 20>(
        vec3(236.0, 138.0, 0.0), vec3(250.0, 150.0, 0.0), vec3(235.0, 177.0, 19.0),
        vec3(0.0, 109.0, 255.0), vec3(0.0, 93.0, 255.0), vec3(0.0, 181.0, 255.0),
        vec3(234.0, 26.0, 26.0), vec3(243.0, 132.0, 23.0), vec3(249.0, 227.0, 28.0),
        vec3(25.0, 201.0, 25.0), vec3(29.0, 203.0, 234.0), vec3(29.0, 53.0, 234.0),
        vec3(155.0, 29.0, 234.0), vec3(227.0, 24.0, 227.0), vec3(172.0, 122.0, 51.0),
        vec3(255.0, 0.0, 0.0), vec3(0.0, 34.0, 205.0), vec3(0.0, 140.0, 0.0),
        vec3(255.0, 213.0, 0.0), vec3(0.0, 194.0, 177.0));
    return t[min(i, 19u)] / 255.0;
}

// Photo filter: p0 = (filter, custom colour, density %, preserve luminosity).
@fragment
fn fs_photo_filter(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    let d = cx_disp(c);
    let f = u32(round(fx.p0.x));
    let col = select(pf_colour(f), to_display(rgb_of(fx.p0.y)), f >= 20u);
    let k = clamp(fx.p0.z / 100.0, 0.0, 1.0);
    var o = mix(d, d * col * 1.6, k);
    if (fx.p0.w >= 0.5) {
        o = o * dot(d, LUMA) / max(dot(o, LUMA), 1e-3);
    }
    return cx_out(o, c.a);
}

// How much of a colour range a display colour is in (Photoshop's way: by its largest,
// middle and smallest channels).
fn sc_weight(d: vec3<f32>, range: u32) -> f32 {
    let mx = max(d.r, max(d.g, d.b));
    let mn = min(d.r, min(d.g, d.b));
    let md = d.r + d.g + d.b - mx - mn;
    switch range {
        case 0u: { return select(0.0, mx - md, mx == d.r); }
        case 1u: { return select(0.0, md - mn, mn == d.b); }
        case 2u: { return select(0.0, mx - md, mx == d.g); }
        case 3u: { return select(0.0, md - mn, mn == d.r); }
        case 4u: { return select(0.0, mx - md, mx == d.b); }
        case 5u: { return select(0.0, md - mn, mn == d.g); }
        case 6u: { return max(mn * 2.0 - 1.0, 0.0); }
        case 7u: { return 1.0 - (abs(mx - 0.5) + abs(mn - 0.5)); }
        default: { return max(1.0 - mx * 2.0, 0.0); }
    }
}

fn sc_apply(d: vec3<f32>, cmyk: vec4<f32>, w: f32, absolute: bool) -> vec3<f32> {
    // Each of red, green and blue is the complement of cyan, magenta and yellow; black darkens all.
    let ink = 1.0 - d;
    let k = cmyk / 100.0;
    var change = vec3(k.x, k.y, k.z) + k.w;
    if (!absolute) {
        change = change * ink;
    }
    return d - change * w;
}

// Selective colour: p0.x = method (1 absolute); ranges at 1–5, 6–10, 11–15: (which, c, m, y, k).
@fragment
fn fs_selective_color(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    let d = cx_disp(c);
    let absolute = fx.p0.x >= 0.5;
    var o = d;
    o = sc_apply(o, vec4(fx.p0.z, fx.p0.w, fx.p1.x, fx.p1.y), sc_weight(d, u32(round(fx.p0.y))), absolute);
    o = sc_apply(o, vec4(fx.p1.w, fx.p2.x, fx.p2.y, fx.p2.z), sc_weight(d, u32(round(fx.p1.z))), absolute);
    o = sc_apply(o, vec4(fx.p3.x, fx.p3.y, fx.p3.z, fx.p3.w), sc_weight(d, u32(round(fx.p2.w))), absolute);
    return cx_out(o, c.a);
}

// Tint: p0 = (black to, white to, amount %).
@fragment
fn fs_tint(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    let d = cx_disp(c);
    let o = mix(to_display(rgb_of(fx.p0.x)), to_display(rgb_of(fx.p0.y)), dot(d, LUMA));
    return cx_out(mix(d, o, clamp(fx.p0.z / 100.0, 0.0, 1.0)), c.a);
}

// Black & white: p0 = (reds, yellows, greens, cyans) %, p1 = (blues, magentas, tint, tint colour).
@fragment
fn fs_black_white(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    let d = cx_disp(c);
    // Photoshop's way: the grey is the smallest channel, plus the colour's two ranges.
    let mn = min(d.r, min(d.g, d.b));
    let mx = max(d.r, max(d.g, d.b));
    let md = d.r + d.g + d.b - mx - mn;
    var w = array<f32, 6>(fx.p0.x, fx.p0.y, fx.p0.z, fx.p0.w, fx.p1.x, fx.p1.y);
    // The primary (largest) and secondary (two largest) colour of this pixel.
    var prim = 0u;
    if (mx == d.g) { prim = 2u; } else if (mx == d.b) { prim = 4u; }
    var sec = 1u;
    if (mn == d.r) { sec = 3u; } else if (mn == d.g) { sec = 5u; }
    let y = mn + (mx - md) * w[prim] / 100.0 + (md - mn) * w[sec] / 100.0;
    var o = vec3(clamp(y, 0.0, 1.0));
    if (fx.p1.z >= 0.5) {
        let t = to_display(rgb_of(fx.p1.w));
        o = cx_blend(o, t / max(dot(t, LUMA), 1e-3) * 0.5, 1u) * 2.0;
        o = clamp(o, vec3(0.0), vec3(1.0));
    }
    return cx_out(o, c.a);
}

// Colour neutraliser: p0 = (shadows cast, midtones cast, highlights cast, blend %).
@fragment
fn fs_color_neutralizer(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    let d = cx_disp(c);
    let s = to_display(rgb_of(fx.p0.x));
    let m = to_display(rgb_of(fx.p0.y));
    let h = to_display(rgb_of(fx.p0.z));
    // Each cast made grey: an offset in the shadows, a gain in the highlights, a gamma between.
    let sd = s - dot(s, LUMA);
    let hg = vec3(max(dot(h, LUMA), 0.05)) / max(h, vec3(0.05));
    let mg = log(vec3(clamp(dot(m, LUMA), 0.05, 0.95))) / log(clamp(m, vec3(0.05), vec3(0.95)));
    let y = dot(d, LUMA);
    var o = d - sd * (1.0 - smoothstep(0.0, 0.5, y));
    o = mix(o, o * hg, smoothstep(0.5, 1.0, y));
    o = pow(clamp(o, vec3(0.0), vec3(1.0)), mix(vec3(1.0), mg, 1.0 - abs(y - 0.5) * 2.0));
    return cx_out(mix(o, d, clamp(fx.p0.w / 100.0, 0.0, 1.0)), c.a);
}

// Colour offset: p0 = (red, green, blue phase °, overflow).
@fragment
fn fs_color_offset(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    let d = cx_disp(c);
    let v = d + vec3(fx.p0.x, fx.p0.y, fx.p0.z) / 360.0;
    var o = fract(v);
    switch u32(round(fx.p0.w)) {
        case 1u: { o = 1.0 - abs(1.0 - 2.0 * fract(v * 0.5)); }
        case 2u: { o = select(fract(v), 1.0 - fract(v), (floor(v) % 2.0) != vec3(0.0)); }
        default: {}
    }
    return cx_out(o, c.a);
}

// Toner: p0 = (tones, highlights, brights, midtones), p1 = (darktones, shadows, blend %).
@fragment
fn fs_toner(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    let d = cx_disp(c);
    var y = dot(d, LUMA);
    let hi = to_display(rgb_of(fx.p0.y));
    let br = to_display(rgb_of(fx.p0.z));
    let mi = to_display(rgb_of(fx.p0.w));
    let dk = to_display(rgb_of(fx.p1.x));
    let sh = to_display(rgb_of(fx.p1.y));
    var o = vec3(0.0);
    switch u32(round(fx.p0.x)) {
        case 0u: { o = mix(sh, hi, y); }
        case 1u: { o = select(mix(mi, hi, y * 2.0 - 1.0), mix(sh, mi, y * 2.0), y < 0.5); }
        case 3u: {
            y = 1.0 - abs(1.0 - 2.0 * y);
            o = select(mix(mi, hi, y * 2.0 - 1.0), mix(sh, mi, y * 2.0), y < 0.5);
        }
        default: {
            let t = y * 4.0;
            if (t < 1.0) { o = mix(sh, dk, t); } else if (t < 2.0) { o = mix(dk, mi, t - 1.0); } else if (t < 3.0) { o = mix(mi, br, t - 2.0); } else { o = mix(br, hi, t - 3.0); }
        }
    }
    return cx_out(mix(o, d, clamp(fx.p1.z / 100.0, 0.0, 1.0)), c.a);
}

// Threshold RGB: p0.xyz thresholds (0–255), p0.w/p1.xy invert, p1.z blend %.
@fragment
fn fs_threshold_rgb(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    let d = cx_disp(c);
    var o = step(vec3(fx.p0.x, fx.p0.y, fx.p0.z) / 255.0, d);
    o = select(o, 1.0 - o, vec3(fx.p0.w, fx.p1.x, fx.p1.y) >= vec3(0.5));
    return cx_out(mix(o, d, clamp(fx.p1.z / 100.0, 0.0, 1.0)), c.a);
}

// Convolution kernel: p0, p1, p2.x = the nine weights (rows), p2.y = divide by, p2.z = offset
// (0–255), p2.w = alpha too, p3.x = blend %.
@fragment
fn fs_kernel(in: VsOut) -> @location(0) vec4<f32> {
    let px = cx_px();
    var k = array<f32, 9>(fx.p0.x, fx.p0.y, fx.p0.z, fx.p0.w, fx.p1.x, fx.p1.y, fx.p1.z, fx.p1.w, fx.p2.x);
    var sum = vec4(0.0);
    let c0 = cx_at(in.uv);
    for (var j = 0; j < 3; j = j + 1) {
        for (var i = 0; i < 3; i = i + 1) {
            let s = cx_at(in.uv + vec2(f32(i - 1), f32(j - 1)) * px);
            sum = sum + vec4(cx_disp(s), s.a) * k[j * 3 + i];
        }
    }
    let div = select(fx.p2.y, 1.0, abs(fx.p2.y) < 1e-4);
    var o = sum / div + fx.p2.z / 255.0;
    let a = select(c0.a, clamp(o.a, 0.0, 1.0), fx.p2.w >= 0.5);
    let rgb = mix(clamp(o.rgb, vec3(0.0), vec3(1.0)), cx_disp(c0), clamp(fx.p3.x / 100.0, 0.0, 1.0));
    return cx_out(rgb, a);
}

// Video limiter: p0 = (reduce: 0 brightness 1 saturation 2 both, max luma %, min luma %, max chroma %).
@fragment
fn fs_video_limiter(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    let d = cx_disp(c);
    let y = dot(d, LUMA);
    var chroma = d - y;
    let mode = u32(round(fx.p0.x));
    let ymax = fx.p0.y / 100.0;
    let ymin = fx.p0.z / 100.0;
    var ny = clamp(y, ymin, ymax);
    // Saturation: the colour's distance from grey, held within a limit.
    let lim = fx.p0.w / 100.0 * 0.5;
    let len = length(chroma);
    if (mode >= 1u && len > lim) {
        chroma = chroma * lim / len;
    }
    if (mode == 1u) {
        ny = y;
    }
    var o = ny + chroma;
    // Still past the limits (bright saturated colours): pull the colour in towards grey.
    let over = max(max(o.r, max(o.g, o.b)) - max(ymax, 1e-3), 0.0);
    if (mode != 1u && over > 0.0) {
        o = mix(o, vec3(ny), clamp(over / max(max(o.r, max(o.g, o.b)) - ny, 1e-3), 0.0, 1.0));
    }
    return cx_out(clamp(o, vec3(0.0), vec3(1.0)), c.a);
}

// ---- Batch: distort and perspective ----

// The layer at layer pixel q (from its top-left corner), nothing outside it.
fn dx_at(q: vec2<f32>) -> vec4<f32> {
    let uv = q / max(fx.extra.zw, vec2(1.0));
    return textureSampleLevel(src, src_sampler, to_tex(uv), 0.0) * inside(uv);
}

// The layer at layer pixel q, wrapped round (tiled).
fn dx_wrap(q: vec2<f32>) -> vec4<f32> {
    let size = max(fx.extra.zw, vec2(1.0));
    let uv = fract(q / size);
    return textureSampleLevel(src, src_sampler, to_tex(uv), 0.0);
}

// This pixel in layer pixels, and the layer's size.
fn dx_q(uv: vec2<f32>) -> vec2<f32> {
    return uv * max(fx.extra.zw, vec2(1.0));
}
fn dx_size() -> vec2<f32> {
    return max(fx.extra.zw, vec2(1.0));
}

// The projective map from the unit square to the quad a, b, c, d (corners: top left,
// top right, bottom right, bottom left), as a 3×3 matrix (columns).
fn dx_square_to_quad(a: vec2<f32>, b: vec2<f32>, c: vec2<f32>, d: vec2<f32>) -> mat3x3<f32> {
    let s = a - b + c - d;
    let d1 = b - c;
    let d2 = d - c;
    let den = d1.x * d2.y - d2.x * d1.y;
    var g = 0.0;
    var h = 0.0;
    if (abs(den) > 1e-9) {
        g = (s.x * d2.y - d2.x * s.y) / den;
        h = (d1.x * s.y - s.x * d1.y) / den;
    }
    return mat3x3<f32>(vec3(b.x - a.x + g * b.x, b.y - a.y + g * b.y, g), vec3(d.x - a.x + h * d.x, d.y - a.y + h * d.y, h), vec3(a.x, a.y, 1.0));
}

fn dx_inverse3(m: mat3x3<f32>) -> mat3x3<f32> {
    let a = m[0];
    let b = m[1];
    let c = m[2];
    let r0 = cross(b, c);
    let r1 = cross(c, a);
    let r2 = cross(a, b);
    let det = dot(a, r0);
    if (abs(det) < 1e-12) {
        return mat3x3<f32>(vec3(0.0), vec3(0.0), vec3(0.0));
    }
    return transpose(mat3x3<f32>(r0, r1, r2)) * (1.0 / det);
}

// Corner pin: p0 = (top left x, y, top right x, y), p1 = (bottom left x, y, bottom right x, y), %.
@fragment
fn fs_corner_pin(in: VsOut) -> @location(0) vec4<f32> {
    let m = dx_square_to_quad(fx.p0.xy / 100.0, fx.p0.zw / 100.0, fx.p1.zw / 100.0, fx.p1.xy / 100.0);
    let h = dx_inverse3(m) * vec3(in.uv, 1.0);
    if (abs(h.z) < 1e-9) {
        return vec4(0.0);
    }
    let uv = h.xy / h.z;
    return textureSampleLevel(src, src_sampler, to_tex(uv), 0.0) * inside(uv);
}

// Power pin: corners as Corner pin, p2.x = perspective % (0 bilinear, 100 projective).
@fragment
fn fs_power_pin(in: VsOut) -> @location(0) vec4<f32> {
    let a = fx.p0.xy / 100.0;
    let b = fx.p0.zw / 100.0;
    let d = fx.p1.xy / 100.0;
    let c = fx.p1.zw / 100.0;
    let m = dx_square_to_quad(a, b, c, d);
    let h = dx_inverse3(m) * vec3(in.uv, 1.0);
    let proj = select(vec2(-9.0), h.xy / h.z, abs(h.z) > 1e-9);
    // Bilinear: Newton's method on P(s, t) = mix(mix(a, b, s), mix(d, c, s), t).
    var st = proj;
    if (any(st < vec2(-1.0)) || any(st > vec2(2.0))) {
        st = vec2(0.5);
    }
    for (var i = 0; i < 8; i = i + 1) {
        let p = mix(mix(a, b, st.x), mix(d, c, st.x), st.y);
        let ds = mix(b - a, c - d, st.y);
        let dt = mix(d, c, st.x) - mix(a, b, st.x);
        let det = ds.x * dt.y - ds.y * dt.x;
        if (abs(det) < 1e-9) {
            break;
        }
        let r = in.uv - p;
        st = st + vec2(r.x * dt.y - r.y * dt.x, ds.x * r.y - ds.y * r.x) / det;
    }
    let uv = mix(st, proj, clamp(fx.p2.x / 100.0, 0.0, 1.0));
    return textureSampleLevel(src, src_sampler, to_tex(uv), 0.0) * inside(uv);
}

// Displacement map: p0 = (source: 0 itself 1 noise, noise size px, horizontal from, max px),
// p1 = (vertical from, max px, wrap, seed), p2.x = noise speed.
@fragment
fn fs_displacement_map(in: VsOut) -> @location(0) vec4<f32> {
    let q = dx_q(in.uv);
    var map = vec4(0.5);
    var md = vec3(0.5);
    if (fx.p0.x >= 0.5) {
        let p = q / max(fx.p0.y, 1.0) + vec2(fx.extra.y * fx.p2.x * 0.1, 0.0);
        let seed = u32(max(fx.p1.w, 0.0));
        md = vec3(fbm2(seed, p), fbm2(seed + 17u, p + vec2(5.2, 1.3)), fbm2(seed + 31u, p));
        map = vec4(md, 1.0);
    } else {
        map = dx_at(q);
        md = to_display(unpremul(map));
    }
    let hx = cx_chan(md, map.a, u32(round(fx.p0.z)));
    let vy = cx_chan(md, map.a, u32(round(fx.p1.x)));
    let o = q + vec2((hx - 0.5) * 2.0 * fx.p0.w, (vy - 0.5) * 2.0 * fx.p1.y);
    if (fx.p1.z >= 0.5) {
        return dx_wrap(o) * inside(in.uv);
    }
    return dx_at(o);
}

// Magnify: p0 = (centre x %, y %, magnification %, shape), p1 = (size px, feather px, border px,
// border colour), p2.x = the rest hidden.
@fragment
fn fs_magnify(in: VsOut) -> @location(0) vec4<f32> {
    let q = dx_q(in.uv);
    let c = fx.p0.xy / 100.0 * dx_size();
    let rel = q - c;
    let half = max(fx.p1.x, 1.0) * 0.5;
    var d = 0.0;
    if (fx.p0.w >= 0.5) {
        d = max(abs(rel.x), abs(rel.y));
    } else {
        d = length(rel);
    }
    let feather = max(fx.p1.y, 0.0);
    let k = 1.0 - smoothstep(half - feather, half + 1e-3, d);
    let big = dx_at(c + rel * 100.0 / max(fx.p0.z, 1.0));
    let rest = select(dx_at(q), vec4(0.0), fx.p2.x >= 0.5);
    var o = mix(rest, big, k);
    let bw = max(fx.p1.z, 0.0);
    if (bw > 0.0) {
        let b = smoothstep(half - 0.5, half + 0.5, d) * (1.0 - smoothstep(half + bw - 0.5, half + bw + 0.5, d));
        o = mix(o, vec4(rgb_of(fx.p1.w), 1.0), b);
    }
    return o;
}

fn mw_get(i: i32) -> vec2<f32> {
    var a = array<vec2<f32>, 9>(fx.p0.xy, fx.p0.zw, fx.p1.xy, fx.p1.zw, fx.p2.xy, fx.p2.zw, fx.p3.xy, fx.p3.zw, fx.p4.xy);
    return a[clamp(i, 0, 8)] / 100.0;
}

// How far the grid has moved the point at layer uv (smoothly, between its nine points).
fn mw_move(uv: vec2<f32>) -> vec2<f32> {
    let g = clamp(uv, vec2(0.0), vec2(1.0)) * 2.0;
    let cell = min(vec2<i32>(floor(g)), vec2<i32>(1));
    let f = g - vec2<f32>(cell);
    let s = f * f * (3.0 - 2.0 * f);
    let i = cell.y * 3 + cell.x;
    let a = mw_get(i);
    let b = mw_get(i + 1);
    let c = mw_get(i + 3);
    let d = mw_get(i + 4);
    return mix(mix(a, b, s.x), mix(c, d, s.x), s.y);
}

// Mesh warp: the nine points' moves (x, y %), row by row, in p0–p4.x.
@fragment
fn fs_mesh_warp(in: VsOut) -> @location(0) vec4<f32> {
    // Where in the layer this pixel came from: solved backwards (a few steps).
    var s = in.uv;
    for (var i = 0; i < 6; i = i + 1) {
        s = in.uv - mw_move(s);
    }
    return textureSampleLevel(src, src_sampler, to_tex(s), 0.0) * inside(s);
}

// Offset: p0 = (centre x %, y %, blend with original %).
@fragment
fn fs_offset(in: VsOut) -> @location(0) vec4<f32> {
    let q = dx_q(in.uv);
    let shift = (fx.p0.xy / 100.0 - 0.5) * dx_size();
    let o = dx_wrap(q - shift) * inside(in.uv);
    return mix(o, dx_at(q), clamp(fx.p0.z / 100.0, 0.0, 1.0));
}

// Optics compensation: p0 = (field of view °, reverse, across: 0 width 1 height 2 diagonal, centre x %), p1.x = centre y %.
@fragment
fn fs_optics(in: VsOut) -> @location(0) vec4<f32> {
    let size = dx_size();
    let c = vec2(fx.p0.w, fx.p1.x) / 100.0 * size;
    let q = dx_q(in.uv) - c;
    var span = size.x * 0.5;
    switch u32(round(fx.p0.z)) {
        case 1u: { span = size.y * 0.5; }
        case 2u: { span = length(size) * 0.5; }
        default: {}
    }
    let half = radians(clamp(fx.p0.x, 0.0, 179.0)) * 0.5;
    let r = length(q) / span;
    if (r < 1e-6 || half < 1e-4) {
        return dx_at(dx_q(in.uv));
    }
    var k = 1.0;
    if (fx.p0.y >= 0.5) {
        // Taking it out: the straight picture's radius from the distorted one's.
        k = tan(r * half) / (r * tan(half));
    } else {
        k = atan(r * tan(half)) / (r * half);
    }
    return dx_at(c + q * k);
}

// Spherize: p0 = (radius px, centre x %, centre y %, amount %).
@fragment
fn fs_spherize(in: VsOut) -> @location(0) vec4<f32> {
    let q = dx_q(in.uv);
    let c = fx.p0.yz / 100.0 * dx_size();
    let rel = q - c;
    let big = max(fx.p0.x, 1.0);
    let n = length(rel) / big;
    if (n >= 1.0 || n < 1e-6) {
        return dx_at(q);
    }
    let k = clamp(fx.p0.w / 100.0, -1.0, 1.0);
    // Outwards: the arc length round the sphere; inwards, the reverse.
    let sph = asin(n) / 1.5707963 / n;
    let inv = sin(n * 1.5707963) / n;
    let f = select(mix(1.0, inv, -k), mix(1.0, sph, k), k >= 0.0);
    return dx_at(c + rel * f);
}

// Smear: p0 = (from x, y, to x, y %), p1 = (reach %, radius px).
@fragment
fn fs_smear(in: VsOut) -> @location(0) vec4<f32> {
    let size = dx_size();
    let q = dx_q(in.uv);
    let a = fx.p0.xy / 100.0 * size;
    let b = fx.p0.zw / 100.0 * size;
    let r = max(fx.p1.y, 1.0);
    let shift_by = (b - a) * fx.p1.x / 100.0;
    // Pixels near the line from `from` to `to` come from further back along it.
    var s = q;
    for (var i = 0; i < 4; i = i + 1) {
        let w = exp(-dot(s + shift_by - b, s + shift_by - b) / (r * r));
        s = q - shift_by * w;
    }
    return dx_at(s);
}

// Where the warp sends point p (-1…1 across the layer, `u` along the axis, `v` across it).
fn wp_shape(style: u32, k: f32, p: vec2<f32>) -> vec2<f32> {
    let u = p.x;
    let v = p.y;
    let pi = 3.14159265;
    switch style {
        // Arc: bent round a circle's arc, top and bottom alike.
        case 0u: { return vec2(u * (1.0 - 0.25 * k * v), v - k * (1.0 - u * u) * 0.5); }
        case 1u: { return vec2(u * (1.0 - 0.25 * k * max(v, 0.0)), v - k * (1.0 - u * u) * 0.5 * (v + 1.0) * 0.5); }
        case 2u: { return vec2(u * (1.0 - 0.25 * k * min(v, 0.0)), v - k * (1.0 - u * u) * 0.5 * (1.0 - v) * 0.5); }
        case 3u: { return vec2(u, v - k * (1.0 - u * u) * 0.5); }
        // Bulge: the top up and the bottom down in the middle.
        case 4u: { return vec2(u, v * (1.0 + 0.5 * k * (1.0 - u * u))); }
        case 5u: { return vec2(u, v + k * 0.5 * (1.0 - u * u) * (v + 1.0) * 0.5); }
        case 6u: { return vec2(u, v - k * 0.5 * (1.0 - u * u) * (1.0 + v) * 0.5); }
        // Flag and wave: a sine along it (the wave's height varies across).
        case 7u: { return vec2(u, v + 0.3 * k * sin(u * pi)); }
        case 8u: { return vec2(u, v + 0.3 * k * sin(u * pi) * (0.5 + 0.5 * v)); }
        case 9u: { return vec2(u, v * (1.0 + 0.6 * k * sin(pow((u + 1.0) * 0.5, 0.7) * pi))); }
        case 10u: { return vec2(u, v - 0.4 * k * u); }
        // Fisheye and inflate: out from the middle.
        case 11u: {
            let r2 = u * u + v * v;
            return p * (1.0 + 0.5 * k * max(1.0 - r2, 0.0));
        }
        case 12u: { return vec2(u * (1.0 + 0.5 * k * (1.0 - v * v)), v * (1.0 + 0.5 * k * (1.0 - u * u))); }
        case 13u: { return vec2(u * (1.0 - 0.5 * k * (1.0 - v * v)), v * (1.0 + 0.5 * k * (1.0 - u * u))); }
        default: {
            let r = length(p);
            let a = k * pi * max(1.0 - r, 0.0);
            return vec2(u * cos(a) - v * sin(a), u * sin(a) + v * cos(a));
        }
    }
}

fn wp_forward(p: vec2<f32>) -> vec2<f32> {
    let vertical = fx.p0.y >= 0.5;
    var a = p;
    if (vertical) {
        a = p.yx;
    }
    var w = wp_shape(u32(round(fx.p0.x)), fx.p0.z / 100.0, a);
    if (vertical) {
        w = w.yx;
    }
    // Perspective: one side bigger than the other.
    w.y = w.y * (1.0 + 0.5 * fx.p0.w / 100.0 * w.x);
    w.x = w.x * (1.0 + 0.5 * fx.p1.x / 100.0 * w.y);
    return w;
}

// Warp: p0 = (style, axis, bend %, horizontal distortion %), p1.x = vertical distortion %.
@fragment
fn fs_warp(in: VsOut) -> @location(0) vec4<f32> {
    let t = in.uv * 2.0 - 1.0;
    // The point it came from: Newton's method on the forward warp.
    var s = t;
    for (var i = 0; i < 12; i = i + 1) {
        let f = wp_forward(s) - t;
        if (abs(f.x) + abs(f.y) < 1e-5) {
            break;
        }
        let e = 1e-3;
        let ja = (wp_forward(s + vec2(e, 0.0)) - t - f) / e;
        let jb = (wp_forward(s + vec2(0.0, e)) - t - f) / e;
        let det = ja.x * jb.y - jb.x * ja.y;
        if (abs(det) < 1e-8) {
            return vec4(0.0);
        }
        s = s - vec2(jb.y * f.x - jb.x * f.y, -ja.y * f.x + ja.x * f.y) / det;
    }
    if (length(wp_forward(s) - t) > 0.01) {
        return vec4(0.0);
    }
    let uv = s * 0.5 + 0.5;
    return textureSampleLevel(src, src_sampler, to_tex(uv), 0.0) * inside(uv);
}

// How far sideways the rod bends at t (0 base, 1 top).
fn bd_bend(t: f32, style: u32) -> f32 {
    let s = clamp(t, 0.0, 1.0);
    switch style {
        case 1u: { return sin(s * 3.14159265) * 0.5; }
        case 2u: { return max(s - 0.5, 0.0) * 2.0; }
        case 3u: { return s * s * (3.0 - 2.0 * s) * (1.0 - s) * 4.0 * 0.6 + s * 0.2; }
        default: { return s * s; }
    }
}

// Bender: p0 = (amount %, style, top x, top y %), p1 = (base x, base y %).
@fragment
fn fs_bender(in: VsOut) -> @location(0) vec4<f32> {
    let size = dx_size();
    let q = dx_q(in.uv);
    let top = fx.p0.zw / 100.0 * size;
    let base = fx.p1.xy / 100.0 * size;
    let axis = top - base;
    let len = max(length(axis), 1.0);
    let a = axis / len;
    let n = vec2(-a.y, a.x);
    let style = u32(round(fx.p0.y));
    let amount = fx.p0.x / 100.0 * len;
    // The point it came from: its height along the rod is kept; it's moved sideways.
    var s = q;
    for (var i = 0; i < 5; i = i + 1) {
        let t = dot(s - base, a) / len;
        s = q - n * amount * bd_bend(t, style) * select(1.0, 0.0, t < 0.0);
    }
    return dx_at(s);
}

fn bl_height(q: vec2<f32>, soft: f32, from_alpha: bool) -> f32 {
    // A Gaussian over a 7 × 7 grid spanning the softness, so the height is smooth.
    var sum = 0.0;
    var n = 0.0;
    for (var y = -3; y <= 3; y = y + 1) {
        for (var x = -3; x <= 3; x = x + 1) {
            let o = vec2(f32(x), f32(y)) / 3.0;
            let w = exp(-dot(o, o) * 2.0);
            let c = dx_at(q + o * soft);
            sum = sum + w * select(dot(to_display(unpremul(c)), LUMA) * c.a, c.a, from_alpha);
            n = n + w;
        }
    }
    return sum / n;
}

// Blobbylize: p0 = (from, softness px, cut away %, light %), p1 = (light angle °, refraction px, light colour).
@fragment
fn fs_blobbylize(in: VsOut) -> @location(0) vec4<f32> {
    let q = dx_q(in.uv);
    let soft = max(fx.p0.y, 0.5);
    let fa = fx.p0.x >= 0.5;
    let cut = fx.p0.z / 100.0;
    let h0 = bl_height(q, soft, fa);
    let edge = smoothstep(cut, cut + 0.12, h0);
    if (edge <= 0.0) {
        return vec4(0.0);
    }
    let e = max(soft * 0.5, 1.0);
    let g = vec2(bl_height(q + vec2(e, 0.0), soft, fa) - bl_height(q - vec2(e, 0.0), soft, fa), bl_height(q + vec2(0.0, e), soft, fa) - bl_height(q - vec2(0.0, e), soft, fa)) / (2.0 * e);
    let n = normalize(vec3(-g * soft * 2.5, 1.0));
    let ang = radians(fx.p1.x);
    let l = normalize(vec3(cos(ang), -sin(ang), 1.2));
    // (Bent towards the blob's middle, so its own colour shows through.)
    let c = dx_at(q - n.xy * fx.p1.y);
    let diffuse = 0.55 + 0.45 * dot(n, l);
    let spec = pow(max(dot(reflect(-l, n), vec3(0.0, 0.0, 1.0)), 0.0), 30.0) * fx.p0.w / 100.0;
    let rgb = unpremul(c) * diffuse + rgb_of(fx.p1.z) * spec;
    let a = max(c.a, 0.6) * edge;
    return vec4(rgb * a, a);
}

// Flo motion: p0 = (knot 1 x, y %, amount 1 %, knot 2 x), p1 = (knot 2 y, amount 2 %, falloff %, tile).
@fragment
fn fs_flo_motion(in: VsOut) -> @location(0) vec4<f32> {
    let size = dx_size();
    let q = dx_q(in.uv);
    let k1 = fx.p0.xy / 100.0 * size;
    let k2 = vec2(fx.p0.w, fx.p1.x) / 100.0 * size;
    let reach = max(fx.p1.z, 1.0) / 100.0 * length(size) * 0.5;
    let f1 = exp(-dot(q - k1, q - k1) / (reach * reach)) * fx.p0.z / 100.0;
    let f2 = exp(-dot(q - k2, q - k2) / (reach * reach)) * fx.p1.y / 100.0;
    // Pulled in: the pixel here came from further out from the knot.
    let s = q + (q - k1) * f1 + (q - k2) * f2;
    if (fx.p1.w >= 0.5) {
        return dx_wrap(s) * inside(in.uv);
    }
    return dx_at(s);
}

// Lens: p0 = (centre x, y %, size %, convergence).
@fragment
fn fs_lens_cc(in: VsOut) -> @location(0) vec4<f32> {
    let size = dx_size();
    let q = dx_q(in.uv);
    let c = fx.p0.xy / 100.0 * size;
    let r = max(fx.p0.z, 1.0) / 100.0 * min(size.x, size.y) * 0.5;
    let rel = (q - c) / r;
    let n = length(rel);
    if (n > 1.0) {
        return vec4(0.0);
    }
    let k = fx.p0.w / 100.0;
    let f = select(pow(n, 1.0 - 0.7 * k) / max(n, 1e-5), pow(n, 1.0 + 1.5 * -k) / max(n, 1e-5), k < 0.0);
    let edge = clamp((1.0 - n) * r, 0.0, 1.0);
    return dx_at(c + rel * r * select(f, 1.0, n < 1e-5)) * edge;
}

// Page turn: p0 = (fold x, y %, direction °, radius px), p1 = (light °, show, back opacity %, paper).
// The page curls up round a cylinder lying on the fold line, from where it meets the page
// towards the turned corner (the direction), and lies back flat over the page beyond.
@fragment
fn fs_page_turn(in: VsOut) -> @location(0) vec4<f32> {
    let size = dx_size();
    let q = dx_q(in.uv);
    let f = fx.p0.xy / 100.0 * size;
    let ang = radians(fx.p0.z);
    // n points from the fold towards the part that's turning (screen y is down).
    let n = vec2(cos(ang), -sin(ang));
    let r = max(fx.p0.w, 1.0);
    let pi = 3.14159265;
    let d = dot(q - f, n);
    let show = u32(round(fx.p1.y));
    let paper = rgb_of(fx.p1.w);
    let back_k = clamp(fx.p1.z / 100.0, 0.0, 1.0);
    let la = radians(fx.p1.x);
    let ld = vec2(cos(la), -sin(la));
    // The surfaces at this point, the top one first: (where on the flat page, back side?, shade).
    var cand: array<vec3<f32>, 3>;
    var count = 0;
    if (d <= 0.0) {
        cand[0] = vec3(pi * r - d, 1.0, 0.95);
        cand[1] = vec3(d, 0.0, 1.0);
        count = 2;
    } else if (d <= r) {
        let a = asin(clamp(d / r, -1.0, 1.0));
        cand[0] = vec3(pi * r - r * a, 1.0, 0.65 + 0.35 * cos(a));
        cand[1] = vec3(r * a, 0.0, 1.0 - 0.45 * (a / (0.5 * pi)) * (0.6 + 0.4 * dot(ld, n)));
        count = 2;
    }
    for (var i = 0; i < count; i = i + 1) {
        let c = cand[i];
        let back = c.y > 0.5;
        if ((back && show == 2u) || (!back && show == 1u)) {
            continue;
        }
        let s = q + (c.x - d) * n;
        let px = dx_at(s);
        if (px.a <= 1e-4) {
            continue;
        }
        var col = unpremul(px);
        if (back) {
            col = mix(paper, col, back_k);
        }
        // Under the curl, a soft shadow on the page.
        var shade = c.z;
        if (!back && d <= 0.0) {
            shade = 1.0 - 0.35 * exp(-(-d) / (r * 0.6)) * select(1.0, 0.0, show == 2u);
        }
        return vec4(col * shade * px.a, px.a);
    }
    return vec4(0.0);
}

// Slant: p0 = (slant °, floor %, height %).
@fragment
fn fs_slant(in: VsOut) -> @location(0) vec4<f32> {
    let size = dx_size();
    let q = dx_q(in.uv);
    let floor_y = fx.p0.y / 100.0 * size.y;
    let h = max(fx.p0.z, 1.0) / 100.0;
    let up = (floor_y - q.y) / h;
    let t = tan(radians(clamp(fx.p0.x, -89.0, 89.0)));
    return dx_at(vec2(q.x - up * t, floor_y - up));
}

// Split: p0 = (a x, a y, b x, b y %), p1.x = split px.
@fragment
fn fs_split(in: VsOut) -> @location(0) vec4<f32> {
    let size = dx_size();
    let q = dx_q(in.uv);
    let a = fx.p0.xy / 100.0 * size;
    let b = fx.p0.zw / 100.0 * size;
    let ab = b - a;
    let len = max(length(ab), 1.0);
    let t = dot(q - a, ab) / (len * len);
    let n = vec2(-ab.y, ab.x) / len;
    let side = dot(q - a, n);
    let open = max(fx.p1.x, 0.0) * 0.5 * sin(clamp(t, 0.0, 1.0) * 3.14159265);
    if (abs(side) < open) {
        return vec4(0.0);
    }
    return dx_at(q - n * sign(side) * open);
}

// Tiler: p0 = (scale %, centre x, y %, blend with original %).
@fragment
fn fs_tiler(in: VsOut) -> @location(0) vec4<f32> {
    let size = dx_size();
    let q = dx_q(in.uv);
    let c = fx.p0.yz / 100.0 * size;
    let k = max(fx.p0.x, 1.0) / 100.0;
    let s = (q - c) / k + size * 0.5;
    let o = dx_wrap(s) * inside(in.uv);
    return mix(o, dx_at(q), clamp(fx.p0.w / 100.0, 0.0, 1.0));
}

// Griddler: p0 = (h scale %, v scale %, tile size % of the width, rotation °), p1.x = cut tiles.
@fragment
fn fs_griddler(in: VsOut) -> @location(0) vec4<f32> {
    let size = dx_size();
    let q = dx_q(in.uv);
    let t = max(fx.p0.z, 0.5) / 100.0 * size.x;
    let cell = floor(q / t);
    let mid = (cell + 0.5) * t;
    let rel = q - mid;
    let a = radians(-fx.p0.w);
    let r = vec2(rel.x * cos(a) - rel.y * sin(a), rel.x * sin(a) + rel.y * cos(a));
    let k = max(vec2(fx.p0.x, fx.p0.y) / 100.0, vec2(1e-3));
    let local = r / k;
    if (fx.p1.x >= 0.5 && (abs(local.x) > t * 0.5 || abs(local.y) > t * 0.5)) {
        return vec4(0.0);
    }
    return dx_at(mid + local);
}

// Ripple pulse: p0 = (centre x, y %, height px, width px), p1 = (speed px/s, start s, fade %).
@fragment
fn fs_ripple_pulse(in: VsOut) -> @location(0) vec4<f32> {
    let size = dx_size();
    let q = dx_q(in.uv);
    let c = fx.p0.xy / 100.0 * size;
    let t = fx.extra.y - fx.p1.y;
    if (t < 0.0) {
        return dx_at(q);
    }
    let ring = t * fx.p1.x;
    let rel = q - c;
    let r = length(rel);
    let w = max(fx.p0.w, 1.0);
    let x = (r - ring) / w;
    let fade = exp(-ring / max(length(size), 1.0) * fx.p1.z / 25.0);
    // One wave (a sine under a bell), pushing pixels in and out along the radius.
    let push = fx.p0.z * sin(x * 3.14159265) * exp(-x * x * 2.0) * fade;
    let dir = select(vec2(0.0), rel / r, r > 1e-4);
    return dx_at(q - dir * push);
}

// Motion tile: p0 = (centre x, y %, tile w, h %), p1 = (output w, h %, mirror, phase °), p2.x = horizontal phase.
@fragment
fn fs_motion_tile(in: VsOut) -> @location(0) vec4<f32> {
    let size = dx_size();
    let half_out = vec2(fx.p1.x, fx.p1.y) / 200.0 - 0.5;
    if (any(in.uv < -half_out) || any(in.uv > 1.0 + half_out)) {
        return vec4(0.0);
    }
    let tile_sz = max(vec2(fx.p0.z, fx.p0.w) / 100.0, vec2(0.01)) * size;
    let c = fx.p0.xy / 100.0 * size;
    var t = (dx_q(in.uv) - c) / tile_sz + 0.5;
    let ph = fx.p1.w / 360.0;
    if (fx.p2.x >= 0.5) {
        t.y = t.y + select(0.0, ph, (floor(t.x) % 2.0) != 0.0);
    } else {
        t.x = t.x + select(0.0, ph, (floor(t.y) % 2.0) != 0.0);
    }
    var f = fract(t);
    if (fx.p1.z >= 0.5) {
        let odd = (abs(floor(t)) % 2.0) != vec2(0.0);
        f = select(f, 1.0 - f, odd);
    }
    return textureSampleLevel(src, src_sampler, to_tex(f), 0.0);
}

// 3D glasses: p0 = (view, convergence px, depth px, balance %).
@fragment
fn fs_glasses3d(in: VsOut) -> @location(0) vec4<f32> {
    let q = dx_q(in.uv);
    let view = u32(round(fx.p0.x));
    let here = dx_at(q);
    let depth = dot(to_display(unpremul(here)), LUMA) * fx.p0.z;
    let shift = vec2((fx.p0.y + depth) * 0.5, 0.0);
    if (view == 4u) {
        let size = dx_size();
        let left = q.x < size.x * 0.5;
        let s = vec2((q.x % (size.x * 0.5)) * 2.0, q.y);
        return dx_at(s + select(-shift, shift, left) * 2.0);
    }
    let l = dx_at(q + shift);
    let r = dx_at(q - shift);
    let ld = to_display(unpremul(l));
    let rd = to_display(unpremul(r));
    var o = vec3(0.0);
    switch view {
        case 1u: { o = vec3(dot(ld, LUMA), dot(rd, LUMA), 0.0); }
        case 2u: { o = vec3(dot(ld, LUMA), 0.0, dot(rd, LUMA)); }
        case 3u: {
            let b = fx.p0.w / 100.0;
            o = vec3(mix(ld.r, dot(ld, LUMA), 1.0 - b), rd.g, rd.b);
        }
        default: { o = vec3(dot(ld, LUMA), rd.g, rd.b); }
    }
    let a = max(l.a, r.a);
    return vec4(to_linear(o) * a, a);
}

// Radial shadow: p0 = (colour, opacity %, light x, y %), p1 = (projection distance %, softness px, shadow only).
@fragment
fn fs_radial_shadow(in: VsOut) -> @location(0) vec4<f32> {
    let size = dx_size();
    let q = dx_q(in.uv);
    let light = fx.p0.zw / 100.0 * size;
    let s = 1.0 + max(fx.p1.x, 0.0) / 100.0;
    let soft = max(fx.p1.y, 0.0);
    var a = 0.0;
    for (var i = 0; i < 9; i = i + 1) {
        let ang = f32(i) * 0.6981317;
        let o = select(vec2(0.0), vec2(cos(ang), sin(ang)) * soft * (0.5 + 0.5 * f32(i % 2)), i > 0);
        a = a + dx_at(light + (q + o - light) / s).a;
    }
    a = a / 9.0 * clamp(fx.p0.y / 100.0, 0.0, 1.0);
    let shadow = vec4(rgb_of(fx.p0.x) * a, a);
    if (fx.p1.z >= 0.5) {
        return shadow;
    }
    let c = dx_at(q);
    return c + shadow * (1.0 - c.a);
}

// A ray from the camera (looking along +z, the layer's plane at z = 0 shown pixel for
// pixel), for layer pixel q from the layer's middle.
fn r3_ray(q: vec2<f32>, h: f32) -> array<vec3<f32>, 2> {
    let f = geo_focal(35.0, h);
    return array<vec3<f32>, 2>(vec3(0.0, 0.0, -f), normalize(vec3(q, f)));
}

fn r3_shade(n: vec3<f32>, v: vec3<f32>, light_deg: f32, light_k: f32, amb: f32, shine: f32) -> vec2<f32> {
    let az = radians(light_deg);
    let l = normalize(vec3(cos(az), -sin(az), -1.0));
    let diffuse = amb + (1.0 - amb) * light_k * max(dot(n, l), 0.0) / max(-l.z, 0.2) * 0.8;
    let spec = shine * pow(max(dot(n, normalize(l + v)), 0.0), 30.0);
    return vec2(diffuse, spec);
}

// Cylinder: p0 = (radius % of the width, turn x, y, z °), p1 = (show, light %, light angle °,
// ambient %), p2.x = shine %. The layer's width goes once round; its height is the cylinder's.
@fragment
fn fs_cylinder(in: VsOut) -> @location(0) vec4<f32> {
    let size = dx_size();
    let p = (in.uv - 0.5) * size;
    let ray = r3_ray(p, size.y);
    let r = geo_turn(vec3(fx.p0.y, fx.p0.z, fx.p0.w));
    let rt = transpose(r);
    let o = rt * ray[0];
    let d = rt * ray[1];
    let rad = max(fx.p0.x, 1.0) / 100.0 * size.x;
    // Axis along y: x² + z² = rad².
    let a = d.x * d.x + d.z * d.z;
    let b = 2.0 * (o.x * d.x + o.z * d.z);
    let c = o.x * o.x + o.z * o.z - rad * rad;
    let disc = b * b - 4.0 * a * c;
    if (disc < 0.0 || a < 1e-9) {
        return vec4(0.0);
    }
    let sq = sqrt(disc);
    let show = u32(round(fx.p1.x));
    for (var k = 0; k < 2; k = k + 1) {
        let t = select((-b - sq) / (2.0 * a), (-b + sq) / (2.0 * a), k == 1);
        if (t <= 0.0) {
            continue;
        }
        let h = o + d * t;
        if (abs(h.y) > size.y * 0.5) {
            continue;
        }
        let outside = k == 0;
        if ((outside && show == 2u) || (!outside && show == 1u)) {
            continue;
        }
        let ang = atan2(h.x, -h.z);
        let u = ang / 6.2831853 + 0.5;
        let uv = vec2(fract(u), h.y / size.y + 0.5);
        let px = textureSampleLevel(src, src_sampler, to_tex(uv), 0.0);
        if (px.a <= 1e-4) {
            continue;
        }
        var n = normalize(vec3(h.x, 0.0, h.z));
        if (!outside) {
            n = -n;
        }
        let sh = r3_shade(r * n, -ray[1], fx.p1.z, fx.p1.y / 100.0, fx.p1.w / 100.0, fx.p2.x / 100.0);
        return vec4(unpremul(px) * sh.x * px.a + vec3(sh.y) * px.a, px.a);
    }
    return vec4(0.0);
}

// Sphere: p0 = (radius % of the height, turn x, y, z °), p1 = (show, light %, light angle °,
// ambient %), p2.x = shine %. Longitude across the layer, latitude down it.
@fragment
fn fs_sphere(in: VsOut) -> @location(0) vec4<f32> {
    let size = dx_size();
    let p = (in.uv - 0.5) * size;
    let ray = r3_ray(p, size.y);
    let r = geo_turn(vec3(fx.p0.y, fx.p0.z, fx.p0.w));
    let rt = transpose(r);
    let o = rt * ray[0];
    let d = rt * ray[1];
    let rad = max(fx.p0.x, 1.0) / 100.0 * size.y;
    let b = dot(o, d);
    let c = dot(o, o) - rad * rad;
    let disc = b * b - c;
    if (disc < 0.0) {
        return vec4(0.0);
    }
    let sq = sqrt(disc);
    let show = u32(round(fx.p1.x));
    for (var k = 0; k < 2; k = k + 1) {
        let t = select(-b - sq, -b + sq, k == 1);
        if (t <= 0.0) {
            continue;
        }
        let outside = k == 0;
        if ((outside && show == 2u) || (!outside && show == 1u)) {
            continue;
        }
        let h = (o + d * t) / rad;
        let uv = vec2(atan2(h.x, -h.z) / 6.2831853 + 0.5, asin(clamp(h.y, -1.0, 1.0)) / 3.14159265 + 0.5);
        let px = textureSampleLevel(src, src_sampler, to_tex(uv), 0.0);
        if (px.a <= 1e-4) {
            continue;
        }
        let n = select(-h, h, outside);
        let sh = r3_shade(r * n, -ray[1], fx.p1.z, fx.p1.y / 100.0, fx.p1.w / 100.0, fx.p2.x / 100.0);
        // A soft edge round the sphere's outline.
        let edge = clamp(sqrt(max(disc, 0.0)) * 0.5, 0.0, 1.0);
        let a = px.a * edge;
        return vec4((unpremul(px) * sh.x + vec3(sh.y)) * a, a);
    }
    return vec4(0.0);
}

// ---- Batch: generate, noise & grain ----

// What a generator draws (`g`, premultiplied) put on the layer by a mode: 0 normal (over),
// 1 add, 2 multiply, 3 screen, 4 overlay, 5 soft light, 6 hard light, 7 difference,
// 8 stencil alpha (the layer only where g is), 9 silhouette alpha (only where it isn't);
// g's opacity scaled by `k` first.
fn gn_put(c: vec4<f32>, g: vec4<f32>, mode: u32, k: f32) -> vec4<f32> {
    let gg = g * k;
    if (mode == 8u) {
        return c * gg.a;
    }
    if (mode == 9u) {
        return c * (1.0 - gg.a);
    }
    if (mode == 0u) {
        return gg + c * (1.0 - gg.a);
    }
    let base = to_display(unpremul(c));
    let top = to_display(unpremul(gg));
    var m = 0u;
    switch mode {
        case 1u: { m = 11u; }
        case 2u: { m = 1u; }
        case 3u: { m = 2u; }
        case 4u: { m = 3u; }
        case 5u: { m = 4u; }
        case 6u: { m = 5u; }
        default: { m = 10u; }
    }
    let mixed = mix(base, cx_blend(base, top, m), gg.a);
    return vec4(to_linear(mixed) * c.a, c.a);
}

// A random 0–1 from a cell and a seed.
fn gn_rand2(p: vec2<f32>, seed: f32) -> vec2<f32> {
    let q = vec2(dot(p, vec2(127.1, 311.7)) + seed * 17.13, dot(p, vec2(269.5, 183.3)) + seed * 7.71);
    return fract(sin(q) * 43758.5453);
}

// 4-colour gradient: points and colours at 0–11 (x, y, colour each), p3 = (blend, jitter %, opacity %, mode).
@fragment
fn fs_four_color(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv)) * inside(in.uv);
    let size = dx_size();
    let q = dx_q(in.uv);
    var pts = array<vec2<f32>, 4>(fx.p0.xy, vec2(fx.p0.w, fx.p1.x), vec2(fx.p1.z, fx.p1.w), vec2(fx.p2.y, fx.p2.z));
    var cols = array<f32, 4>(fx.p0.z, fx.p1.y, fx.p2.x, fx.p2.w);
    let pw = max(fx.p3.x, 1.0) / 50.0;
    var sum = vec3(0.0);
    var w = 0.0;
    for (var i = 0; i < 4; i = i + 1) {
        let d = length(q - pts[i] / 100.0 * size) / length(size);
        let k = 1.0 / pow(max(d, 1e-4), pw);
        sum = sum + rgb_of(cols[i]) * k;
        w = w + k;
    }
    var rgb = sum / w;
    rgb = rgb + (gn_rand2(q, 3.0).x - 0.5) * fx.p3.y / 100.0 * 0.03;
    return gn_put(c, vec4(max(rgb, vec3(0.0)), 1.0), u32(round(fx.p3.w)), clamp(fx.p3.z / 100.0, 0.0, 1.0));
}

// Beam: p0 = (start x, y, end x, y %), p1 = (length %, time %, start thickness px, end thickness px),
// p2 = (softness %, inside colour, outside colour, beam only).
@fragment
fn fs_beam(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv)) * inside(in.uv);
    let size = dx_size();
    let q = dx_q(in.uv);
    let a = fx.p0.xy / 100.0 * size;
    let b = fx.p0.zw / 100.0 * size;
    let ab = b - a;
    let len2 = max(dot(ab, ab), 1.0);
    let t = clamp(dot(q - a, ab) / len2, 0.0, 1.0);
    let head = clamp(fx.p1.y / 100.0, 0.0, 1.0);
    let tail = head - clamp(fx.p1.x / 100.0, 0.0, 1.0);
    let base = select(c, vec4(0.0), fx.p2.w >= 0.5);
    if (t > head || t < tail) {
        return base;
    }
    let d = length(q - (a + ab * t));
    let thick = max(mix(fx.p1.z, fx.p1.w, t), 0.01) * 0.5;
    let soft = clamp(fx.p2.x / 100.0, 0.0, 1.0);
    let core = 1.0 - smoothstep(thick * 0.35, thick * (0.35 + 0.4 * soft) + 0.5, d);
    let glow = exp(-d * d / (thick * thick * (1.0 + 6.0 * soft))) ;
    let rgb = rgb_of(fx.p2.y) * core + rgb_of(fx.p2.z) * glow * (1.0 - core);
    let a2 = clamp(max(core, glow), 0.0, 1.0);
    return vec4(base.rgb + rgb, max(base.a, a2));
}

// The nearest and second-nearest cell points to p (in cells), and the nearest's random value.
fn cp_cells(p: vec2<f32>, seed: f32, disperse: f32, phase: f32) -> vec3<f32> {
    let i = floor(p);
    var d1 = 9.0;
    var d2 = 9.0;
    var r1 = 0.0;
    for (var y = -1; y <= 1; y = y + 1) {
        for (var x = -1; x <= 1; x = x + 1) {
            let cell = i + vec2(f32(x), f32(y));
            let rnd = gn_rand2(cell, seed);
            let wob = vec2(sin(phase + rnd.x * 6.283), cos(phase + rnd.y * 6.283)) * 0.15;
            let pt = cell + 0.5 + (rnd - 0.5) * disperse + wob * disperse;
            let d = length(p - pt);
            if (d < d1) {
                d2 = d1;
                d1 = d;
                r1 = rnd.x;
            } else if (d < d2) {
                d2 = d;
            }
        }
    }
    return vec3(d1, d2, r1);
}

// Cell pattern: p0 = (style, invert, contrast, disperse %), p1 = (size px, offset x, y, evolution °),
// p2 = (speed °/s, seed, opacity %, mode).
@fragment
fn fs_cell_pattern(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv)) * inside(in.uv);
    let q = dx_q(in.uv) + fx.p1.yz;
    let p = q / max(fx.p1.x, 2.0);
    let phase = radians(fx.p1.w + fx.p2.x * fx.extra.y);
    let cl = cp_cells(p, fx.p2.y, clamp(fx.p0.w / 100.0, 0.0, 1.5), phase);
    var v = 0.0;
    switch u32(round(fx.p0.x)) {
        case 0u: { v = 1.0 - cl.x; }
        case 1u: { v = cl.y - cl.x; }
        case 2u: { v = smoothstep(0.0, 0.08, cl.y - cl.x); }
        case 3u: { v = cl.z; }
        case 4u: { v = cl.z * smoothstep(0.0, 0.05, cl.y - cl.x); }
        case 5u: { v = sqrt(max(1.0 - cl.x * cl.x * 1.5, 0.0)); }
        case 6u: { v = mix(cl.y - cl.x, cl.z, 0.5); }
        default: { v = 1.0 - abs(cl.y - cl.x - 0.25) * 4.0; }
    }
    v = clamp((v - 0.5) * fx.p0.z / 100.0 + 0.5, 0.0, 1.0);
    if (fx.p0.y >= 0.5) {
        v = 1.0 - v;
    }
    return gn_put(c, vec4(to_linear(vec3(v)), 1.0), u32(round(fx.p2.w)), clamp(fx.p2.z / 100.0, 0.0, 1.0));
}

// Checkerboard: p0 = (anchor x, y %, width, height px), p1 = (feather px, colour, opacity %, mode).
@fragment
fn fs_checkerboard(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv)) * inside(in.uv);
    let q = dx_q(in.uv) - fx.p0.xy / 100.0 * dx_size();
    let s = max(vec2(fx.p0.z, fx.p0.w), vec2(1.0));
    let f = q / s;
    let cell = floor(f);
    let on = ((cell.x + cell.y) % 2.0 + 2.0) % 2.0;
    let e = min(abs(fract(f.x) - 0.5) * s.x, abs(fract(f.y) - 0.5) * s.y);
    let edge = select(1.0, smoothstep(0.0, fx.p1.x, s.x * 0.5 - e), fx.p1.x > 0.0);
    let k = mix(0.5, on, edge);
    return gn_put(c, vec4(rgb_of(fx.p1.y), 1.0) * k, u32(round(fx.p1.w)), clamp(fx.p1.z / 100.0, 0.0, 1.0));
}

// Circle: p0 = (centre x, y %, radius px, edge), p1 = (thickness / edge radius px, feather out, feather in, invert),
// p2 = (colour, opacity %, mode).
@fragment
fn fs_circle(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv)) * inside(in.uv);
    let d = length(dx_q(in.uv) - fx.p0.xy / 100.0 * dx_size());
    let r = max(fx.p0.z, 0.0);
    let fo = max(fx.p1.y, 0.0);
    let fi = max(fx.p1.z, 0.0);
    var k = 1.0 - smoothstep(r - fo * 0.5 - 0.5, r + fo * 0.5 + 0.5, d);
    let edge = u32(round(fx.p0.w));
    if (edge > 0u) {
        let inner = select(r - max(fx.p1.x, 0.0), max(fx.p1.x, 0.0), edge == 2u);
        k = k * smoothstep(inner - fi * 0.5 - 0.5, inner + fi * 0.5 + 0.5, d);
    }
    if (fx.p1.w >= 0.5) {
        k = 1.0 - k;
    }
    return gn_put(c, vec4(rgb_of(fx.p2.x), 1.0) * k, u32(round(fx.p2.z)), clamp(fx.p2.y / 100.0, 0.0, 1.0));
}

// Ellipse: p0 = (centre x, y %, width, height % of the layer height), p1 = (thickness px, softness %, inside, outside colour), p2.x = only.
@fragment
fn fs_ellipse(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv)) * inside(in.uv);
    let rel = dx_q(in.uv) - fx.p0.xy / 100.0 * dx_size();
    // (Its width and height are percentages of the layer's height.)
    let rad = max(vec2(fx.p0.z, fx.p0.w) / 100.0 * dx_size().y * 0.5, vec2(1.0));
    // Distance from the ellipse, roughly in pixels (its value over its gradient).
    let k = length(rel / rad);
    let grad = length(rel / (rad * rad)) / max(k, 1e-4);
    let d = abs(k - 1.0) / max(grad, 1e-4);
    let th = max(fx.p1.x, 0.0) * 0.5;
    let soft = clamp(fx.p1.y / 100.0, 0.0, 1.0);
    let core = 1.0 - smoothstep(th * (1.0 - soft), th + 0.5, d);
    let glow = (1.0 - smoothstep(th, th * (1.0 + 4.0 * soft) + 1.0, d)) * soft;
    let rgb = rgb_of(fx.p1.z) * core + rgb_of(fx.p1.w) * glow * (1.0 - core);
    let a = clamp(max(core, glow), 0.0, 1.0);
    let base = select(c, vec4(0.0), fx.p2.x >= 0.5);
    return vec4(rgb + base.rgb * (1.0 - a), a + base.a * (1.0 - a));
}

// Fill: p0 = (colour, invert, feather px, opacity %).
@fragment
fn fs_fill_colour(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv)) * inside(in.uv);
    var a = c.a;
    let f = max(fx.p0.z, 0.0);
    if (f > 0.0) {
        var s = 0.0;
        for (var i = 0; i < 8; i = i + 1) {
            let ang = f32(i) * 0.785398;
            s = s + cx_at(in.uv + vec2(cos(ang), sin(ang)) * f * 0.5 * cx_px()).a;
        }
        a = min(a, s / 8.0 * 1.5);
    }
    if (fx.p0.y >= 0.5) {
        a = (1.0 - a) * inside(in.uv);
    }
    let k = clamp(fx.p0.w / 100.0, 0.0, 1.0);
    let filled = vec4(rgb_of(fx.p0.x) * a, a);
    if (fx.p0.y >= 0.5) {
        return filled * k + c * (1.0 - filled.a * k);
    }
    return mix(c, filled, k);
}

// Grid: p0 = (anchor x, y %, width, height px), p1 = (border px, feather px, invert, colour), p2 = (opacity %, mode).
@fragment
fn fs_grid(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv)) * inside(in.uv);
    let q = dx_q(in.uv) - fx.p0.xy / 100.0 * dx_size();
    let s = max(vec2(fx.p0.z, fx.p0.w), vec2(1.0));
    let m = abs(q - s * round(q / s));
    let d = min(m.x, m.y);
    let half = max(fx.p1.x, 0.0) * 0.5;
    var k = 1.0 - smoothstep(half - fx.p1.y * 0.5 - 0.5, half + fx.p1.y * 0.5 + 0.5, d);
    if (fx.p1.z >= 0.5) {
        k = 1.0 - k;
    }
    return gn_put(c, vec4(rgb_of(fx.p1.w), 1.0) * k, u32(round(fx.p2.y)), clamp(fx.p2.x / 100.0, 0.0, 1.0));
}

// Radio waves: p0 = (x, y %, sides, star %), p1 = (per second, px/s, lifespan s, spin °/s),
// p2 = (width px, softness px, colour, fade), p3.x = only.
@fragment
fn fs_radio_waves(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv)) * inside(in.uv);
    let rel = dx_q(in.uv) - fx.p0.xy / 100.0 * dx_size();
    let t = max(fx.extra.y, 0.0);
    let sides = clamp(round(fx.p0.z), 3.0, 64.0);
    let ang = atan2(rel.y, rel.x) - radians(fx.p1.w * t);
    // A polygon's (and star's) radius at this angle, for a circumradius of 1.
    let seg = 6.2831853 / sides;
    let a = ((ang % seg) + seg) % seg - seg * 0.5;
    var shape = cos(seg * 0.5) / cos(a);
    if (sides >= 63.5) {
        shape = 1.0;
    }
    shape = shape * (1.0 - fx.p0.w / 100.0 * 0.5 * (1.0 - abs(a) / (seg * 0.5)));
    let r = length(rel) / max(shape, 0.05);
    let period = 1.0 / max(fx.p1.x, 0.01);
    let life = max(fx.p1.z, 0.01);
    var k = 0.0;
    // The waves still alive: born every period, up to `life` ago.
    let newest = floor(t / period);
    let count = min(ceil(life / period) + 1.0, 64.0);
    for (var i = 0.0; i < count; i = i + 1.0) {
        let born = (newest - i) * period;
        let age = t - born;
        if (born < 0.0 || age > life) {
            continue;
        }
        let ring = age * fx.p1.y;
        let d = abs(r - ring);
        let w = max(fx.p2.x, 0.01) * 0.5;
        var v = 1.0 - smoothstep(w - 0.5, w + max(fx.p2.y, 0.0) + 0.5, d);
        if (fx.p2.w >= 0.5) {
            v = v * (1.0 - age / life);
        }
        k = max(k, v);
    }
    let base = select(c, vec4(0.0), fx.p3.x >= 0.5);
    return vec4(rgb_of(fx.p2.z) * k + base.rgb * (1.0 - k), k + base.a * (1.0 - k));
}

// Threads: p0 = (width px, height %, overlaps, centre %), p1 = (coverage %, shadowing %, texture %, angle °).
@fragment
fn fs_threads(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv)) * inside(in.uv);
    let a = radians(fx.p1.w);
    let q0 = dx_q(in.uv);
    let q = vec2(q0.x * cos(a) + q0.y * sin(a), -q0.x * sin(a) + q0.y * cos(a)) / max(fx.p0.x, 1.0);
    let cell = floor(q);
    let f = fract(q);
    let over = max(round(fx.p0.z), 1.0);
    // Warp (down) and weft (across) take turns on top, `over` threads at a time.
    let warp_top = ((floor((cell.x + cell.y) / over)) % 2.0) == 0.0;
    let cov = clamp(fx.p1.x / 100.0, 0.0, 1.0);
    let wx = 1.0 - smoothstep(cov * 0.45, cov * 0.5 + 0.02, abs(f.x - fx.p0.w / 100.0));
    let wy = 1.0 - smoothstep(cov * 0.45, cov * 0.5 + 0.02, abs(f.y - 0.5));
    // Each thread's roundness (its relief), and fibres along it.
    let rx = sqrt(max(1.0 - pow((f.x - 0.5) * 2.0, 2.0), 0.0));
    let ry = sqrt(max(1.0 - pow((f.y - 0.5) * 2.0, 2.0), 0.0));
    let fib_x = 1.0 - fx.p1.z / 100.0 * 0.4 * fract(sin(dot(vec2(cell.x, floor(q.y * 8.0)), vec2(12.9898, 78.233))) * 43758.5453);
    let fib_y = 1.0 - fx.p1.z / 100.0 * 0.4 * fract(sin(dot(vec2(floor(q.x * 8.0), cell.y), vec2(39.3468, 11.135))) * 43758.5453);
    let h = fx.p0.y / 100.0;
    var shade = 0.0;
    var cover = 0.0;
    if (warp_top) {
        shade = select(mix(1.0, ry, h) * fib_y * (1.0 - fx.p1.y / 100.0 * 0.5), mix(1.0, rx, h) * fib_x, wx > 0.5);
        cover = max(wx, wy);
    } else {
        shade = select(mix(1.0, rx, h) * fib_x * (1.0 - fx.p1.y / 100.0 * 0.5), mix(1.0, ry, h) * fib_y, wy > 0.5);
        cover = max(wx, wy);
    }
    let k = cover;
    return vec4(c.rgb * shade * k, c.a * k);
}

// Light burst: p0 = (centre x, y %, length %, intensity %), p1 = (threshold %, burst, rays only).
@fragment
fn fs_light_burst(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv)) * inside(in.uv);
    let centre = fx.p0.xy / 100.0;
    let len = fx.p0.z / 100.0;
    let thr = clamp(fx.p1.x / 100.0, 0.0, 0.99);
    let style = u32(round(fx.p1.y));
    let n = 48;
    var sum = vec3(0.0);
    var w = 0.0;
    for (var i = 0; i < n; i = i + 1) {
        let t = f32(i) / f32(n - 1);
        let uv = centre + (in.uv - centre) / (1.0 + len * t);
        let s = cx_at(uv);
        let bright = max(to_display(unpremul(s)) - thr, vec3(0.0)) / (1.0 - thr) * s.a;
        var k = 1.0;
        if (style == 1u) { k = 1.0 - t; } else if (style == 2u) { k = t; }
        sum = sum + to_linear(bright) * k;
        w = w + 1.0;
    }
    let rays = sum / w * fx.p0.w / 100.0 * 2.0;
    let base = select(c, vec4(0.0), fx.p1.z >= 0.5);
    return vec4(base.rgb + rays, max(base.a, min(max(rays.r, max(rays.g, rays.b)), 1.0)));
}

fn cn_field(p: vec2<f32>, seed: u32, oct: i32, t: f32) -> f32 {
    var sum = 0.0;
    var amp = 0.5;
    var f = 1.0;
    for (var i = 0; i < 8; i = i + 1) {
        if (i >= oct) {
            break;
        }
        sum = sum + amp * vnoise(seed + u32(i) * 13u, p * f + vec2(t * 0.3, -t * 0.2) * f);
        amp = amp * 0.5;
        f = f * 2.03;
    }
    return sum;
}

// Curl noise: p0 = (size px, detail, swirl %, contrast %), p1 = (speed %, dark, light, seed), p2 = (opacity %, mode).
@fragment
fn fs_curl_noise(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv)) * inside(in.uv);
    let seed = u32(max(fx.p1.w, 0.0));
    let oct = i32(clamp(round(fx.p0.y), 1.0, 8.0));
    let t = fx.extra.y * fx.p1.x / 100.0;
    var p = dx_q(in.uv) / max(fx.p0.x, 2.0);
    // Advected along the curl of a slower field a few times.
    let e = 0.01;
    for (var i = 0; i < 4; i = i + 1) {
        let fy = cn_field(p + vec2(0.0, e), seed + 99u, 3, t * 0.5) - cn_field(p - vec2(0.0, e), seed + 99u, 3, t * 0.5);
        let fxd = cn_field(p + vec2(e, 0.0), seed + 99u, 3, t * 0.5) - cn_field(p - vec2(e, 0.0), seed + 99u, 3, t * 0.5);
        p = p + vec2(fy, -fxd) / (2.0 * e) * 0.08 * fx.p0.z / 100.0;
    }
    var v = cn_field(p, seed, oct, t);
    v = clamp((v - 0.5) * fx.p0.w / 100.0 * 2.0 + 0.5, 0.0, 1.0);
    let rgb = mix(rgb_of(fx.p1.y), rgb_of(fx.p1.z), v);
    return gn_put(c, vec4(rgb, 1.0), u32(round(fx.p2.y)), clamp(fx.p2.x / 100.0, 0.0, 1.0));
}

// Noise: p0 = (amount %, colour, clip, still), p1.x = seed.
@fragment
fn fs_noise(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    let q = floor(dx_q(in.uv));
    let frame = select(floor(fx.extra.y * 30.0), 0.0, fx.p0.w >= 0.5) + fx.p1.x * 7.0;
    let r = vec3(gn_rand2(q, frame).x, gn_rand2(q, frame + 1.3).x, gn_rand2(q, frame + 2.7).x);
    let n = select(vec3(r.x), r, fx.p0.y >= 0.5) - 0.5;
    var d = cx_disp(c) + n * fx.p0.x / 100.0;
    if (fx.p0.z >= 0.5) {
        d = clamp(d, vec3(0.0), vec3(1.0));
    }
    return vec4(to_linear(d) * c.a, c.a);
}

// Noise HLS: p0 = (type, hue %, lightness %, saturation %), p1 = (grain px, phase °, animate).
@fragment
fn fs_noise_hls(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, to_tex(in.uv));
    let kind = u32(round(fx.p0.x));
    let g = select(1.0, max(fx.p1.x, 0.5), kind == 2u);
    let q = floor(dx_q(in.uv) / g);
    let frame = select(0.0, floor(fx.extra.y * 30.0), fx.p1.z >= 0.5) + fx.p1.y / 37.0;
    var r = vec3(gn_rand2(q, frame).x, gn_rand2(q, frame + 1.7).x, gn_rand2(q, frame + 3.1).x) - 0.5;
    if (kind == 1u) {
        r = r * abs(r) * 2.0;
    }
    var h = cx_rgb2hsl(cx_disp(c));
    h.x = fract(h.x + r.x * fx.p0.y / 100.0);
    h.z = clamp(h.z + r.y * fx.p0.z / 100.0, 0.0, 1.0);
    h.y = clamp(h.y + r.z * fx.p0.w / 100.0, 0.0, 1.0);
    return cx_out(cx_hsl2rgb(h), c.a);
}

// The median (by brightness) of up to 49 samples on a grid of radius r px about uv.
fn md_median(uv: vec2<f32>, r: f32) -> vec4<f32> {
    let n = i32(min(ceil(r), 3.0));
    let gap = r / max(f32(n), 1.0);
    let px = cx_px();
    var s: array<vec4<f32>, 49>;
    var l: array<f32, 49>;
    var count = 0;
    for (var y = -n; y <= n; y = y + 1) {
        for (var x = -n; x <= n; x = x + 1) {
            let c = cx_at(uv + vec2(f32(x), f32(y)) * gap * px);
            s[count] = c;
            l[count] = dot(c.rgb, LUMA) + c.a * 1e-3;
            count = count + 1;
        }
    }
    // The one with as many below it as above.
    let half = count / 2;
    for (var i = 0; i < 49; i = i + 1) {
        if (i >= count) {
            break;
        }
        var below = 0;
        var same = 0;
        for (var j = 0; j < 49; j = j + 1) {
            if (j >= count) {
                break;
            }
            below = below + select(0, 1, l[j] < l[i]);
            same = same + select(0, 1, l[j] == l[i]);
        }
        if (below <= half && below + same > half) {
            return s[i];
        }
    }
    return s[half];
}

// Median: p0.x = radius px.
@fragment
fn fs_median(in: VsOut) -> @location(0) vec4<f32> {
    if (fx.p0.x <= 0.0) {
        return cx_at(in.uv);
    }
    return md_median(in.uv, fx.p0.x);
}

// Dust & scratches: p0 = (radius px, threshold 0–255).
@fragment
fn fs_dust_scratches(in: VsOut) -> @location(0) vec4<f32> {
    let c = cx_at(in.uv);
    if (fx.p0.x <= 0.0) {
        return c;
    }
    let m = md_median(in.uv, fx.p0.x);
    let diff = length(cx_disp(c) - cx_disp(m)) * 255.0 / 1.732;
    return select(c, m, diff > fx.p0.y);
}

// Remove grain (one pass): p0 = (strength %, size px, passes, colour noise %), p1 = (sharpen %, view).
// A bilateral average: neighbours weighted by nearness and by likeness, brightness and
// colour noise judged separately.
@fragment
fn fs_remove_grain(in: VsOut) -> @location(0) vec4<f32> {
    let c0 = cx_at(in.uv);
    let d0 = cx_disp(c0);
    let y0 = dot(d0, LUMA);
    let r = max(fx.p0.y, 1.0);
    let n = i32(min(ceil(r), 6.0));
    let gap = r / max(f32(n), 1.0);
    let px = cx_px();
    let strength = clamp(fx.p0.x / 100.0, 0.0, 1.0);
    let sy = 0.02 + 0.15 * strength;
    let sc = 0.02 + 0.3 * strength * fx.p0.w / 100.0;
    var sum_y = 0.0;
    var wy = 0.0;
    var sum_c = vec3(0.0);
    var wc = 0.0;
    var blur = 0.0;
    var wb = 0.0;
    for (var y = -n; y <= n; y = y + 1) {
        for (var x = -n; x <= n; x = x + 1) {
            let o = vec2(f32(x), f32(y)) * gap;
            let ds = exp(-dot(o, o) / (2.0 * r * r));
            let s = cx_at(in.uv + o * px);
            let d = cx_disp(s);
            let yy = dot(d, LUMA);
            let k1 = ds * exp(-pow((yy - y0) / sy, 2.0));
            sum_y = sum_y + yy * k1;
            wy = wy + k1;
            let k2 = ds * exp(-pow((yy - y0) / (sy * 3.0), 2.0));
            sum_c = sum_c + (d - yy) * k2;
            wc = wc + k2;
            blur = blur + yy * ds;
            wb = wb + ds;
        }
    }
    let ny = mix(y0, sum_y / max(wy, 1e-5), strength);
    let nc = mix(d0 - y0, sum_c / max(wc, 1e-5), clamp(fx.p0.w / 100.0, 0.0, 1.0) * strength);
    // Sharpened back: the smoothed brightness pushed away from a wider blur.
    let sharp = ny + (ny - blur / max(wb, 1e-5)) * fx.p1.x / 100.0;
    let res = clamp(sharp + nc, vec3(0.0), vec3(1.0));
    if (fx.p1.y >= 0.5) {
        return vec4(to_linear(clamp((d0 - res) * 4.0 + 0.5, vec3(0.0), vec3(1.0))), 1.0) * inside(in.uv);
    }
    return vec4(to_linear(res) * c0.a, c0.a);
}

// ---- Batch: keying and stylize ----

// The layer's alpha (and colour) with a matte `m` (0–1) multiplied in.
fn ky_out(c: vec4<f32>, m: f32) -> vec4<f32> {
    return c * clamp(m, 0.0, 1.0);
}

// The matte thinned (positive px) or spread (negative) and feathered, from 8 samples round.
fn ky_edge(uv: vec2<f32>, m0: f32, thin: f32, feather: f32, key: vec3<f32>, tol: f32) -> f32 {
    var m = m0;
    let r = abs(thin) + feather * 0.5;
    if (r < 0.5) {
        return m0;
    }
    var lo = m0;
    var hi = m0;
    var sum = m0;
    for (var i = 0; i < 8; i = i + 1) {
        let a = f32(i) * 0.785398;
        let s = cx_at(uv + vec2(cos(a), sin(a)) * r * cx_px());
        let mm = smoothstep(tol * 0.8, tol, length(cx_disp(s) - key) / 1.732) * select(1.0, 0.0, s.a < 0.01);
        lo = min(lo, mm);
        hi = max(hi, mm);
        sum = sum + mm;
    }
    if (thin > 0.0) {
        m = mix(m0, lo, clamp(thin / max(r, 1e-3), 0.0, 1.0));
    } else if (thin < 0.0) {
        m = mix(m0, hi, clamp(-thin / max(r, 1e-3), 0.0, 1.0));
    }
    if (feather > 0.0) {
        m = mix(m, sum / 9.0, 0.6);
    }
    return m;
}

fn st_lum(uv: vec2<f32>) -> f32 {
    let c = cx_at(uv);
    return dot(cx_disp(c), LUMA) * c.a;
}

// Colour key: p0 = (colour, tolerance 0–255, edge thin px, feather px).
@fragment
fn fs_color_key(in: VsOut) -> @location(0) vec4<f32> {
    let c = cx_at(in.uv);
    let key = to_display(rgb_of(fx.p0.x));
    let tol = max(fx.p0.y / 255.0, 1e-3);
    let m0 = smoothstep(tol * 0.8, tol, length(cx_disp(c) - key) / 1.732);
    return ky_out(c, ky_edge(in.uv, m0, fx.p0.z, fx.p0.w, key, tol));
}

// A display colour as 0–255 Lab (a, b offset by 128), YUV (U, V offset by 128) or RGB.
fn kr_space(d: vec3<f32>, space: u32) -> vec3<f32> {
    if (space == 2u) {
        return d * 255.0;
    }
    if (space == 1u) {
        let y = dot(d, vec3(0.299, 0.587, 0.114));
        return vec3(y, (d.b - y) * 0.565 + 0.5, (d.r - y) * 0.713 + 0.5) * 255.0;
    }
    let l = to_linear(d);
    let xyz = vec3(dot(l, vec3(0.4124, 0.3576, 0.1805)) / 0.9505, dot(l, vec3(0.2126, 0.7152, 0.0722)), dot(l, vec3(0.0193, 0.1192, 0.9505)) / 1.089);
    let f = select(xyz * 7.787 + 16.0 / 116.0, pow(max(xyz, vec3(0.0)), vec3(1.0 / 3.0)), xyz > vec3(0.008856));
    return vec3((116.0 * f.y - 16.0) * 2.55, 500.0 * (f.x - f.y) + 128.0, 200.0 * (f.y - f.z) + 128.0);
}

// Colour range: p0 = (space, fuzziness, min1, max1), p1 = (min2, max2, min3, max3).
@fragment
fn fs_color_range(in: VsOut) -> @location(0) vec4<f32> {
    let c = cx_at(in.uv);
    let v = kr_space(cx_disp(c), u32(round(fx.p0.x)));
    let lo = vec3(fx.p0.z, fx.p1.x, fx.p1.z);
    let hi = vec3(fx.p0.w, fx.p1.y, fx.p1.w);
    let fz = max(fx.p0.y, 1e-3);
    let out_by = max(max(lo - v, v - hi), vec3(0.0));
    let k = smoothstep(0.0, fz, max(out_by.x, max(out_by.y, out_by.z)));
    return ky_out(c, k);
}

// Colour difference key: p0 = (screen: 0 green 1 blue 2 red, black, white, gamma), p1 = (spill, view).
@fragment
fn fs_color_difference_key(in: VsOut) -> @location(0) vec4<f32> {
    let c = cx_at(in.uv);
    var d = cx_disp(c);
    let s = u32(round(fx.p0.x));
    var key = d.g;
    var rest = max(d.r, d.b);
    if (s == 1u) { key = d.b; rest = max(d.r, d.g); } else if (s == 2u) { key = d.r; rest = max(d.g, d.b); }
    let diff = max(key - rest, 0.0) * 255.0;
    var m = 1.0 - clamp((diff - fx.p0.y) / max(fx.p0.z - fx.p0.y, 1.0), 0.0, 1.0);
    m = pow(m, 1.0 / max(fx.p0.w, 0.1));
    if (fx.p1.y >= 0.5) {
        return vec4(vec3(m * c.a), 1.0) * inside(in.uv);
    }
    if (fx.p1.x >= 0.5) {
        if (s == 1u) { d.b = min(d.b, rest); } else if (s == 2u) { d.r = min(d.r, rest); } else { d.g = min(d.g, rest); }
    }
    let a = c.a * m;
    return vec4(to_linear(d) * a, a);
}

// Linear colour key: p0 = (colour, match, tolerance %, softness %), p1 = (keep, view).
@fragment
fn fs_linear_color_key(in: VsOut) -> @location(0) vec4<f32> {
    let c = cx_at(in.uv);
    let dist = cc_match(cx_disp(c), to_display(rgb_of(fx.p0.x)), u32(round(fx.p0.y)));
    let tol = fx.p0.z / 100.0;
    var m = clamp((dist - tol) / max(fx.p0.w / 100.0, 1e-3), 0.0, 1.0);
    if (fx.p1.x >= 0.5) {
        m = 1.0 - m;
    }
    if (fx.p1.y >= 0.5) {
        return vec4(vec3(m * c.a), 1.0) * inside(in.uv);
    }
    return ky_out(c, m);
}

// Extract: p0 = (channel, black, white, black softness), p1 = (white softness, invert).
@fragment
fn fs_extract(in: VsOut) -> @location(0) vec4<f32> {
    let c = cx_at(in.uv);
    let d = cx_disp(c);
    var v = dot(d, LUMA);
    switch u32(round(fx.p0.x)) {
        case 1u: { v = d.r; }
        case 2u: { v = d.g; }
        case 3u: { v = d.b; }
        case 4u: { v = c.a; }
        default: {}
    }
    v = v * 255.0;
    let lo = smoothstep(fx.p0.y - fx.p0.w - 0.5, fx.p0.y + 0.5, v);
    let hi = 1.0 - smoothstep(fx.p0.z - 0.5, fx.p0.z + fx.p1.x + 0.5, v);
    var m = lo * hi;
    if (fx.p1.y >= 0.5) {
        m = 1.0 - m;
    }
    return ky_out(c, m);
}

// Spill suppressor: p0 = (colour, amount %, method).
@fragment
fn fs_spill_suppressor(in: VsOut) -> @location(0) vec4<f32> {
    let c = cx_at(in.uv);
    var d = cx_disp(c);
    let key = to_display(rgb_of(fx.p0.x));
    // The key's strongest channel is the one that spills.
    var ch = 1;
    if (key.b >= key.g && key.b >= key.r) { ch = 2; } else if (key.r >= key.g) { ch = 0; }
    var others = array<vec2<i32>, 3>(vec2(1, 2), vec2(0, 2), vec2(0, 1));
    let o = others[ch];
    let lim = select((d[o.x] + d[o.y]) * 0.5, max(d[o.x], d[o.y]), fx.p0.z >= 0.5);
    let over = max(d[ch] - lim, 0.0) * clamp(fx.p0.y / 100.0, 0.0, 2.0);
    d[ch] = d[ch] - over;
    return vec4(to_linear(clamp(d, vec3(0.0), vec3(1.0))) * c.a, c.a);
}

// The layer's matte in black and white.
@fragment
fn fs_matte_view(in: VsOut) -> @location(0) vec4<f32> {
    let c = cx_at(in.uv);
    return vec4(vec3(c.a), 1.0) * inside(in.uv);
}


// Brush strokes: p0 = (angle °, brush size px, length px, density), p1 = (randomness, blend %).
@fragment
fn fs_brush_strokes(in: VsOut) -> @location(0) vec4<f32> {
    let px = cx_px();
    let q = dx_q(in.uv);
    let size = max(fx.p0.y, 0.5);
    let len = max(fx.p0.z, 0.5);
    let base = radians(fx.p0.x);
    // A few overlapping layers of strokes; the top one that covers this pixel paints it.
    var col = vec4(0.0);
    var found = false;
    let cell = max(size * 2.0 / max(fx.p0.w, 0.05), 1.0);
    for (var layer = 0; layer < 3; layer = layer + 1) {
        let g = (q + vec2(f32(layer) * cell * 0.37)) / cell;
        let ci = floor(g);
        for (var y = -1; y <= 1; y = y + 1) {
            for (var x = -1; x <= 1; x = x + 1) {
                let cid = ci + vec2(f32(x), f32(y));
                let r = gn_rand2(cid, f32(layer) * 3.1);
                let centre = (cid + r) * cell - vec2(f32(layer) * cell * 0.37);
                let a = base + (r.x - 0.5) * fx.p1.x * 0.8;
                let dir = vec2(cos(a), -sin(a));
                let rel = q - centre;
                let along = dot(rel, dir);
                let across = dot(rel, vec2(-dir.y, dir.x));
                let l = len * (0.6 + 0.8 * r.y * fx.p1.x * 0.5);
                if (abs(along) < l * 0.5 && abs(across) < size) {
                    col = cx_at(centre * px);
                    found = true;
                }
            }
        }
    }
    let c0 = cx_at(in.uv);
    if (!found) {
        col = c0;
    }
    return mix(col, c0, clamp(fx.p1.y / 100.0, 0.0, 1.0));
}

// Cartoon: p0 = (render, detail px, steps, smoothness %), p1 = (edge threshold, width px, softness %, opacity %).
@fragment
fn fs_cartoon(in: VsOut) -> @location(0) vec4<f32> {
    let c0 = cx_at(in.uv);
    let px = cx_px();
    // Smoothed (an edge-keeping average) then shaded in steps.
    let r = max(fx.p0.y, 0.5);
    var sum = vec4(0.0);
    var w = 0.0;
    let d0 = cx_disp(c0);
    for (var i = 0; i < 13; i = i + 1) {
        let a = f32(i) * 2.39996;
        let o = vec2(cos(a), sin(a)) * r * sqrt(f32(i) / 12.0);
        let s = cx_at(in.uv + o * px);
        let k = exp(-pow(length(cx_disp(s) - d0) * 6.0, 2.0));
        sum = sum + s * k;
        w = w + k;
    }
    let sm = sum / max(w, 1e-4);
    var d = cx_disp(sm);
    let steps = max(round(fx.p0.z), 1.0);
    let y = dot(d, LUMA);
    let soft = clamp(fx.p0.w / 100.0, 0.0, 1.0) * 0.5;
    let f = fract(y * steps);
    let stepped = (floor(y * steps) + smoothstep(0.5 - soft, 0.5 + soft, f)) / steps;
    d = d * stepped / max(y, 1e-3);
    // Edges: the brightness's slope.
    let e = max(fx.p1.y, 0.5);
    let gx = st_lum(in.uv + vec2(e, 0.0) * px) - st_lum(in.uv - vec2(e, 0.0) * px);
    let gy = st_lum(in.uv + vec2(0.0, e) * px) - st_lum(in.uv - vec2(0.0, e) * px);
    let g = length(vec2(gx, gy)) * 10.0;
    let thr = fx.p1.x;
    let edge = smoothstep(thr * (1.0 - fx.p1.z / 100.0), thr + 0.05, g) * fx.p1.w / 100.0;
    let render = u32(round(fx.p0.x));
    var fill = clamp(d, vec3(0.0), vec3(1.0));
    if (render == 2u) {
        fill = vec3(1.0);
    }
    if (render == 1u) {
        return vec4(to_linear(fill) * c0.a, c0.a);
    }
    return vec4(to_linear(fill * (1.0 - edge)) * c0.a, c0.a);
}

// Emboss: p0 = (direction °, relief px, contrast %, blend %).
@fragment
fn fs_emboss(in: VsOut) -> @location(0) vec4<f32> {
    let c0 = cx_at(in.uv);
    let a = radians(fx.p0.x);
    let o = vec2(cos(a), -sin(a)) * max(fx.p0.y, 0.1) * cx_px();
    let s1 = cx_at(in.uv + o);
    let s2 = cx_at(in.uv - o);
    let diff = (cx_disp(s1) - cx_disp(s2)) * fx.p0.z / 100.0;
    let e = vec3(0.5 + dot(diff, LUMA));
    return vec4(to_linear(mix(clamp(e, vec3(0.0), vec3(1.0)), cx_disp(c0), clamp(fx.p0.w / 100.0, 0.0, 1.0))) * c0.a, c0.a);
}

// Colour emboss: p0 = (direction °, relief px, contrast %, blend %).
@fragment
fn fs_color_emboss(in: VsOut) -> @location(0) vec4<f32> {
    let c0 = cx_at(in.uv);
    let a = radians(fx.p0.x);
    let o = vec2(cos(a), -sin(a)) * max(fx.p0.y, 0.1) * cx_px();
    let s1 = cx_at(in.uv + o);
    let s2 = cx_at(in.uv - o);
    let diff = (cx_disp(s1) - cx_disp(s2)) * fx.p0.z / 100.0;
    let e = cx_disp(c0) + dot(diff, LUMA);
    return vec4(to_linear(mix(clamp(e, vec3(0.0), vec3(1.0)), cx_disp(c0), clamp(fx.p0.w / 100.0, 0.0, 1.0))) * c0.a, c0.a);
}

// Find edges: p0 = (invert, blend %).
@fragment
fn fs_find_edges(in: VsOut) -> @location(0) vec4<f32> {
    let px = cx_px();
    let c0 = cx_at(in.uv);
    var gx = vec3(0.0);
    var gy = vec3(0.0);
    for (var j = -1; j <= 1; j = j + 1) {
        for (var i = -1; i <= 1; i = i + 1) {
            let w = select(1.0, 2.0, i == 0 || j == 0);
            let d = cx_disp(cx_at(in.uv + vec2(f32(i), f32(j)) * px));
            gx = gx + d * f32(i) * select(w, 0.0, i == 0);
            gy = gy + d * f32(j) * select(w, 0.0, j == 0);
        }
    }
    var e = clamp(sqrt(gx * gx + gy * gy), vec3(0.0), vec3(1.0));
    if (fx.p0.x < 0.5) {
        e = 1.0 - e;
    }
    return vec4(to_linear(mix(e, cx_disp(c0), clamp(fx.p0.y / 100.0, 0.0, 1.0))) * c0.a, c0.a);
}

// Mosaic: p0 = (blocks across, down, sharp colours).
@fragment
fn fs_mosaic_blocks(in: VsOut) -> @location(0) vec4<f32> {
    let n = max(vec2(fx.p0.x, fx.p0.y), vec2(1.0));
    let cell = floor(in.uv * n);
    if (fx.p0.z >= 0.5) {
        return cx_at((cell + 0.5) / n) * inside(in.uv);
    }
    var sum = vec4(0.0);
    for (var j = 0; j < 4; j = j + 1) {
        for (var i = 0; i < 4; i = i + 1) {
            sum = sum + cx_at((cell + (vec2(f32(i), f32(j)) + 0.5) / 4.0) / n);
        }
    }
    return sum / 16.0 * inside(in.uv);
}

// Posterize: p0.x = levels.
@fragment
fn fs_posterize(in: VsOut) -> @location(0) vec4<f32> {
    let c = cx_at(in.uv);
    let n = max(round(fx.p0.x), 2.0) - 1.0;
    return cx_out(round(cx_disp(c) * n) / n, c.a);
}

// Roughen edges: p0 = (type, colour, border px, sharpness), p1 = (influence %, scale %, complexity, evolution °), p2.x = seed.
@fragment
fn fs_roughen_edges(in: VsOut) -> @location(0) vec4<f32> {
    let px = cx_px();
    let c = cx_at(in.uv);
    let border = max(fx.p0.z, 0.0);
    let kind = u32(round(fx.p0.x));
    let q = dx_q(in.uv) / max(fx.p1.y, 10.0) * 0.6;
    let seed = u32(max(fx.p2.x, 0.0));
    let oct = i32(clamp(round(fx.p1.z), 1.0, 8.0));
    var n = 0.0;
    var amp = 0.5;
    var f = 0.08;
    for (var i = 0; i < 8; i = i + 1) {
        if (i >= oct) {
            break;
        }
        n = n + amp * vnoise(seed + u32(i) * 7u, q * f * 10.0 + vec2(fx.p1.w / 360.0, 0.0));
        amp = amp * 0.5;
        f = f * 2.1;
    }
    if (kind == 3u) {
        n = pow(n, 3.0) * 2.0;
    }
    // How far in from the edge this pixel is (opacity averaged over the border).
    var s = 0.0;
    for (var i = 0; i < 8; i = i + 1) {
        let a = f32(i) * 0.785398;
        s = s + cx_at(in.uv + vec2(cos(a), sin(a)) * border * px).a;
    }
    let depth = (c.a + s) / 9.0;
    let k = clamp((depth - (1.0 - n * fx.p1.x / 100.0) * 0.5) * (1.0 + fx.p0.w * 4.0) + 0.5, 0.0, 1.0);
    var o = c * k;
    if (kind == 1u || kind == 5u) {
        let edge = clamp((1.0 - depth) * 2.0, 0.0, 1.0) * k;
        o = vec4(mix(o.rgb, rgb_of(fx.p0.y) * o.a, edge), o.a);
    }
    if (kind == 4u || kind == 5u) {
        o = o * smoothstep(0.35, 0.5, n + depth * 0.5);
    }
    return o;
}

// Scatter: p0 = (amount px, grain, every frame).
@fragment
fn fs_scatter(in: VsOut) -> @location(0) vec4<f32> {
    let q = floor(dx_q(in.uv));
    let frame = select(0.0, floor(fx.extra.y * 30.0), fx.p0.z >= 0.5);
    var o = (gn_rand2(q, frame + 0.7) - 0.5) * 2.0 * fx.p0.x;
    switch u32(round(fx.p0.y)) {
        case 1u: { o.y = 0.0; }
        case 2u: { o.x = 0.0; }
        default: {}
    }
    return cx_at(in.uv + o * cx_px());
}

fn tx_height(q: vec2<f32>, kind: u32) -> f32 {
    switch kind {
        case 0u: {
            let w = abs(sin(q.x * 1.6)) * 0.5 + abs(sin(q.y * 1.6)) * 0.5;
            return w * 0.7 + vnoise(5u, q * 3.0) * 0.3;
        }
        case 1u: {
            let row = floor(q.y / 4.0);
            let x = q.x / 8.0 + select(0.0, 0.5, (row % 2.0) == 1.0);
            let mortar = min(abs(fract(x) - 0.0) * 8.0, min(abs(fract(q.y / 4.0)) * 4.0, (1.0 - fract(q.y / 4.0)) * 4.0));
            return smoothstep(0.0, 0.3, min(mortar, (1.0 - fract(x)) * 8.0)) * (0.8 + 0.2 * vnoise(3u, q * 2.0));
        }
        case 2u: {
            let a = abs(sin(q.x * 3.0 + vnoise(9u, q * 0.5) * 2.0));
            let b = abs(sin(q.y * 3.0 + vnoise(4u, q * 0.5) * 2.0));
            return max(a, b) * 0.8 + vnoise(2u, q * 6.0) * 0.2;
        }
        case 3u: { return fbm2(7u, q * 0.6) * 0.7 + vnoise(8u, q * 8.0) * 0.3; }
        default: { return vnoise(11u, q * 4.0); }
    }
}

// Texturize: p0 = (texture, scale %, relief, light °), p1.x = contrast %.
@fragment
fn fs_texturize(in: VsOut) -> @location(0) vec4<f32> {
    let c = cx_at(in.uv);
    let kind = u32(round(fx.p0.x));
    let q = dx_q(in.uv) / (max(fx.p0.y, 10.0) / 100.0 * 4.0);
    let e = 0.25;
    let h = tx_height(q, kind);
    let gx = tx_height(q + vec2(e, 0.0), kind) - tx_height(q - vec2(e, 0.0), kind);
    let gy = tx_height(q + vec2(0.0, e), kind) - tx_height(q - vec2(0.0, e), kind);
    let n = normalize(vec3(-vec2(gx, gy) / (2.0 * e) * fx.p0.z * 0.2, 1.0));
    let a = radians(fx.p0.w);
    let l = normalize(vec3(cos(a), -sin(a), 1.0));
    let shade = 1.0 + (dot(n, l) - l.z) * 1.5 + (h - 0.5) * 0.3 * fx.p1.x / 100.0;
    return cx_out(cx_disp(c) * shade, c.a);
}

// Threshold: p0 = (level, softness), 0–255.
@fragment
fn fs_threshold(in: VsOut) -> @location(0) vec4<f32> {
    let c = cx_at(in.uv);
    let y = dot(cx_disp(c), LUMA) * 255.0;
    let s = max(fx.p0.y, 0.5);
    return cx_out(vec3(smoothstep(fx.p0.x - s, fx.p0.x + s, y)), c.a);
}

// Block load: p0 = (completion %, scan lines, start cleared, smooth).
@fragment
fn fs_block_load(in: VsOut) -> @location(0) vec4<f32> {
    let t = clamp(fx.p0.x / 100.0, 0.0, 1.0);
    let size = dx_size();
    let q = dx_q(in.uv);
    // Seven passes, each halving the block size from 64 px down to 1.
    let pass_f = t * 7.0;
    let pass_i = floor(pass_f);
    var block = pow(2.0, 6.0 - pass_i);
    if (t >= 1.0) {
        return cx_at(in.uv);
    }
    if (fx.p0.y >= 0.5) {
        // Within a pass, rows are filled top to bottom; below the scan line, the last pass shows.
        let line_y = fract(pass_f) * size.y;
        if (q.y > line_y) {
            if (pass_i < 1.0) {
                return select(cx_at((floor(q / 64.0) + 0.5) * 64.0 / size), vec4(0.0), fx.p0.z >= 0.5);
            }
            block = block * 2.0;
        }
    }
    let cell = (floor(q / block) + 0.5) * block;
    if (fx.p0.w >= 0.5) {
        // Smooth: between the four nearest blocks' colours.
        let f = q / block - 0.5;
        let i = floor(f);
        let w = f - i;
        let a = cx_at((i + vec2(0.5, 0.5)) * block / size);
        let b = cx_at((i + vec2(1.5, 0.5)) * block / size);
        let cc = cx_at((i + vec2(0.5, 1.5)) * block / size);
        let d = cx_at((i + vec2(1.5, 1.5)) * block / size);
        return mix(mix(a, b, w.x), mix(cc, d, w.x), w.y) * inside(in.uv);
    }
    return cx_at(cell / size) * inside(in.uv);
}

// Burn film: p0 = (burn %, centre x, y %, seed).
@fragment
fn fs_burn_film(in: VsOut) -> @location(0) vec4<f32> {
    let c = cx_at(in.uv);
    let size = dx_size();
    let q = dx_q(in.uv);
    let t = clamp(fx.p0.x / 100.0, 0.0, 1.0);
    let seed = u32(max(fx.p0.w, 0.0));
    let s = min(size.x, size.y);
    // The hole: a few spots (the first at the centre) whose noisy edges grow outwards.
    var field = 9.0;
    for (var i = 0; i < 4; i = i + 1) {
        let r = gn_rand2(vec2(f32(i), 1.7), f32(seed));
        let p = select(r * size, fx.p0.yz / 100.0 * size, i == 0);
        let start = select(f32(i) * 0.2 + r.x * 0.1, 0.0, i == 0);
        let grow = max(t - start, 0.0) * 1.3 * s;
        field = min(field, (length(q - p) - grow) / s);
    }
    field = field + (fbm2(seed, q / s * 6.0) - 0.5) * 0.15;
    let edge = 0.03;
    if (field < 0.0) {
        return vec4(0.0);
    }
    let rim = exp(-field / 0.004);
    let scorch = 1.0 - smoothstep(0.0, edge, field);
    var rgb = unpremul(c);
    rgb = mix(rgb, vec3(0.25, 0.12, 0.03) * dot(rgb, LUMA) * 2.0, scorch * 0.85);
    rgb = rgb + vec3(1.0, 0.85, 0.5) * rim * 1.5;
    return vec4(rgb * c.a, c.a);
}

// Hex tile: p0 = (radius px, centre x, y %, rotate °), p1 = (render, gap px).
@fragment
fn fs_hex_tile(in: VsOut) -> @location(0) vec4<f32> {
    let size = dx_size();
    let c = fx.p0.yz / 100.0 * size;
    let a = radians(fx.p0.w);
    let rel0 = dx_q(in.uv) - c;
    let rel = vec2(rel0.x * cos(a) + rel0.y * sin(a), -rel0.x * sin(a) + rel0.y * cos(a));
    let r = max(fx.p0.x, 2.0);
    // Axial hex coordinates (pointy-top), rounded to the nearest hexagon.
    let qf = (sqrt(3.0) / 3.0 * rel.x - rel.y / 3.0) / r;
    let rf = (2.0 / 3.0 * rel.y) / r;
    var cube = vec3(qf, rf, -qf - rf);
    var rc = round(cube);
    let d = abs(rc - cube);
    if (d.x > d.y && d.x > d.z) { rc.x = -rc.y - rc.z; } else if (d.y > d.z) { rc.y = -rc.x - rc.z; }
    let centre_r = vec2(r * sqrt(3.0) * (rc.x + rc.y * 0.5), r * 1.5 * rc.y);
    let local = rel - centre_r;
    // The hexagon's edge distance, for the gap.
    let h = abs(local);
    let edge = r * 0.8660254 - max(h.x, dot(h, vec2(0.5, 0.8660254)));
    if (edge < fx.p1.y * 0.5) {
        return vec4(0.0);
    }
    let back = vec2(centre_r.x * cos(a) - centre_r.y * sin(a), centre_r.x * sin(a) + centre_r.y * cos(a)) + c;
    switch u32(round(fx.p1.x)) {
        case 1u: { return dx_at(size * 0.5 + local / r * min(size.x, size.y) * 0.5); }
        case 2u: {
            let flip = select(1.0, -1.0, (abs(rc.x + rc.y) % 2.0) == 1.0);
            return dx_at(size * 0.5 + vec2(local.x * flip, local.y) / r * min(size.x, size.y) * 0.5);
        }
        default: { return dx_at(back); }
    }
}

// Kaleida: p0 = (centre x, y %, segments, size %), p1 = (rotation °, straight).
@fragment
fn fs_kaleida(in: VsOut) -> @location(0) vec4<f32> {
    let size = dx_size();
    let c = fx.p0.xy / 100.0 * size;
    let rel = (dx_q(in.uv) - c) * 100.0 / max(fx.p0.w, 1.0);
    let n = max(round(fx.p0.z), 2.0);
    let seg = 6.2831853 / n;
    var a = atan2(rel.y, rel.x) - radians(fx.p1.x);
    a = ((a % seg) + seg) % seg;
    if (fx.p1.y < 0.5 && a > seg * 0.5) {
        a = seg - a;
    }
    let r = length(rel);
    let src = c + vec2(cos(a), sin(a)) * r;
    return dx_wrap(src) * inside(in.uv);
}

// Mr. Smoothie: p0 = (from x, y, to x, y %), p1 = (smoothness px, phase °, loops, blend %).
@fragment
fn fs_mr_smoothie(in: VsOut) -> @location(0) vec4<f32> {
    let size = dx_size();
    let c0 = cx_at(in.uv);
    let s = max(fx.p1.x, 0.5);
    var l = st_lum(in.uv);
    for (var i = 0; i < 8; i = i + 1) {
        let a = f32(i) * 0.785398;
        l = l + st_lum(in.uv + vec2(cos(a), sin(a)) * s * cx_px());
    }
    l = l / 9.0;
    let t = fract(l * fx.p1.z + fx.p1.y / 360.0);
    let tt = 1.0 - abs(1.0 - 2.0 * t);
    let a = fx.p0.xy / 100.0;
    let b = fx.p0.zw / 100.0;
    let col = cx_at(mix(a, b, tt));
    let o = vec4(unpremul(col) * c0.a, c0.a);
    return mix(o, c0, clamp(fx.p1.w / 100.0, 0.0, 1.0));
}

fn pl_h(uv: vec2<f32>, s: f32) -> f32 {
    var l = st_lum(uv);
    for (var i = 0; i < 6; i = i + 1) {
        let a = f32(i) * 1.0472;
        l = l + st_lum(uv + vec2(cos(a), sin(a)) * s * cx_px());
    }
    return l / 7.0;
}

// Plastic: p0 = (height px, softness px, light °, intensity %), p1 = (roughness %, metal %).
@fragment
fn fs_plastic(in: VsOut) -> @location(0) vec4<f32> {
    let c = cx_at(in.uv);
    let s = max(fx.p0.y, 0.5);
    let e = max(s * 0.5, 1.0);
    let px = cx_px();
    let gx = pl_h(in.uv + vec2(e, 0.0) * px, s) - pl_h(in.uv - vec2(e, 0.0) * px, s);
    let gy = pl_h(in.uv + vec2(0.0, e) * px, s) - pl_h(in.uv - vec2(0.0, e) * px, s);
    let n = normalize(vec3(-vec2(gx, gy) / (2.0 * e) * fx.p0.x, 1.0));
    let a = radians(fx.p0.z);
    let l = normalize(vec3(cos(a), -sin(a), 1.0));
    let k = fx.p0.w / 100.0;
    let diffuse = 0.5 + 0.5 * dot(n, l) / l.z * 0.7;
    let shine = pow(max(dot(n, normalize(l + vec3(0.0, 0.0, 1.0))), 0.0), 200.0 / max(fx.p1.x, 1.0)) * k;
    let rgb = unpremul(c);
    let hl = mix(vec3(1.0), rgb, clamp(fx.p1.y / 100.0, 0.0, 1.0));
    return vec4((rgb * mix(1.0, diffuse, k) + hl * shine) * c.a, c.a);
}

// Glass: p0 = (height px, softness px, displacement px, light °), p1.x = intensity %.
@fragment
fn fs_glass_cc(in: VsOut) -> @location(0) vec4<f32> {
    let s = max(fx.p0.y, 0.5);
    let e = max(s * 0.5, 1.0);
    let px = cx_px();
    let gx = pl_h(in.uv + vec2(e, 0.0) * px, s) - pl_h(in.uv - vec2(e, 0.0) * px, s);
    let gy = pl_h(in.uv + vec2(0.0, e) * px, s) - pl_h(in.uv - vec2(0.0, e) * px, s);
    let g = vec2(gx, gy) / (2.0 * e);
    let n = normalize(vec3(-g * fx.p0.x, 1.0));
    let c = cx_at(in.uv + n.xy * fx.p0.z * px);
    let a = radians(fx.p0.w);
    let l = normalize(vec3(cos(a), -sin(a), 1.0));
    let k = fx.p1.x / 100.0;
    let lit = (dot(n, l) - l.z) * k;
    let shine = pow(max(dot(n, normalize(l + vec3(0.0, 0.0, 1.0))), 0.0), 60.0) * k;
    return vec4((unpremul(c) * (1.0 + lit) + vec3(shine)) * c.a, c.a);
}

// RepeTile: p0 = (left, right, up, down px), p1.x = tiling.
@fragment
fn fs_repe_tile(in: VsOut) -> @location(0) vec4<f32> {
    let size = dx_size();
    let q = dx_q(in.uv);
    if (q.x < -fx.p0.x || q.x > size.x + fx.p0.y || q.y < -fx.p0.z || q.y > size.y + fx.p0.w) {
        return vec4(0.0);
    }
    var t = q / size;
    switch u32(round(fx.p1.x)) {
        case 1u: {
            let odd = (abs(floor(t)) % 2.0) != vec2(0.0);
            t = select(fract(t), 1.0 - fract(t), odd);
        }
        case 2u: { t = vec2(fract(t.x + select(0.0, 0.5, (abs(floor(t.y)) % 2.0) == 1.0)), fract(t.y)); }
        case 3u: { t = vec2(fract(t.x), fract(t.y + select(0.0, 0.5, (abs(floor(t.x)) % 2.0) == 1.0))); }
        default: { t = fract(t); }
    }
    return textureSampleLevel(src, src_sampler, to_tex(t), 0.0);
}

// ---- Batch: transitions, time, utility, simulation ----

// The layer kept by a transition's map value `g` (0 goes first, 1 last) at completion t,
// with a soft band `soft` wide (in map units).
fn tr_keep(g: f32, t: f32, soft: f32) -> f32 {
    if (t <= 0.0) {
        return 1.0;
    }
    if (t >= 1.0) {
        return 0.0;
    }
    let s = max(soft, 1e-4);
    // (The band slides from below 0 to above 1 over the transition.)
    let edge = t * (1.0 + s) - s * 0.5;
    return smoothstep(edge - s * 0.5, edge + s * 0.5, g);
}

// Block dissolve: p0 = (completion %, width px, height px, feather %), p1.x = seed.
@fragment
fn fs_block_dissolve(in: VsOut) -> @location(0) vec4<f32> {
    let c = cx_at(in.uv);
    let cell = floor(dx_q(in.uv) / max(vec2(fx.p0.y, fx.p0.z), vec2(1.0)));
    let g = gn_rand2(cell, fx.p1.x).x;
    return c * tr_keep(g, fx.p0.x / 100.0, fx.p0.w / 100.0 * 0.3);
}

// Gradient wipe: p0 = (completion %, softness %, gradient, invert), p1.x = noise size px.
@fragment
fn fs_gradient_wipe(in: VsOut) -> @location(0) vec4<f32> {
    let c = cx_at(in.uv);
    var g = 0.0;
    switch u32(round(fx.p0.z)) {
        case 1u: { g = in.uv.x; }
        case 2u: { g = in.uv.y; }
        case 3u: { g = clamp(length(in.uv - 0.5) * 1.414, 0.0, 1.0); }
        case 4u: { g = fbm2(3u, dx_q(in.uv) / max(fx.p1.x, 1.0)); }
        default: { g = dot(cx_disp(c), LUMA); }
    }
    if (fx.p0.w >= 0.5) {
        g = 1.0 - g;
    }
    return c * tr_keep(g, fx.p0.x / 100.0, fx.p0.y / 100.0);
}

// Iris wipe: p0 = (completion %, centre x, y %, points), p1 = (rotation °, feather px).
@fragment
fn fs_iris_wipe(in: VsOut) -> @location(0) vec4<f32> {
    let c = cx_at(in.uv);
    let size = dx_size();
    let rel = dx_q(in.uv) - fx.p0.yz / 100.0 * size;
    let n = clamp(round(fx.p0.w), 3.0, 64.0);
    let seg = 6.2831853 / n;
    let a = atan2(rel.y, rel.x) - radians(fx.p1.x);
    let m = ((a % seg) + seg) % seg - seg * 0.5;
    // Distance as the polygon measures it (its apothem direction).
    let d = length(rel) * cos(m) / cos(seg * 0.5);
    // Just big enough to clear the layer from wherever the centre is (its farthest corner).
    let c0 = fx.p0.yz / 100.0 * size;
    let corner = max(abs(c0), abs(size - c0));
    let far = length(corner) / cos(seg * 0.5) + fx.p1.y;
    let r = fx.p0.x / 100.0 * far;
    let f = max(fx.p1.y, 0.5);
    return c * smoothstep(r - f * 0.5, r + f * 0.5, d);
}

// Linear wipe: p0 = (completion %, angle °, feather px). At 90° it wipes from left to right.
@fragment
fn fs_linear_wipe(in: VsOut) -> @location(0) vec4<f32> {
    let c = cx_at(in.uv);
    let size = dx_size();
    let a = radians(fx.p0.y - 90.0);
    let dir = vec2(cos(a), sin(a));
    let q = dx_q(in.uv) - size * 0.5;
    let half = abs(dir.x) * size.x * 0.5 + abs(dir.y) * size.y * 0.5;
    let f = max(fx.p0.z, 0.5);
    let pos = dot(q, dir);
    let edge = -half - f + (2.0 * half + 2.0 * f) * fx.p0.x / 100.0;
    return c * smoothstep(edge - f * 0.5, edge + f * 0.5, pos);
}

// Radial wipe: p0 = (completion %, start °, centre x, y %), p1 = (wipe, feather °).
@fragment
fn fs_radial_wipe(in: VsOut) -> @location(0) vec4<f32> {
    let c = cx_at(in.uv);
    let rel = dx_q(in.uv) - fx.p0.zw / 100.0 * dx_size();
    // Clockwise from the start angle (0 straight up), 0–1.
    var a = (atan2(rel.x, -rel.y) - radians(fx.p0.y)) / 6.2831853;
    a = fract(a);
    var g = a;
    switch u32(round(fx.p1.x)) {
        case 1u: { g = 1.0 - a; }
        case 2u: { g = 1.0 - abs(1.0 - 2.0 * a); }
        default: {}
    }
    return c * tr_keep(g, fx.p0.x / 100.0, fx.p1.y / 360.0);
}

// Venetian blinds: p0 = (completion %, direction °, width px, feather px).
@fragment
fn fs_venetian_blinds(in: VsOut) -> @location(0) vec4<f32> {
    let c = cx_at(in.uv);
    let a = radians(fx.p0.y);
    let q = dx_q(in.uv);
    let pos = dot(q, vec2(-sin(a), cos(a)));
    let w = max(fx.p0.z, 1.0);
    let f = fract(pos / w);
    let feather = max(fx.p0.w, 0.5) / w;
    let t = fx.p0.x / 100.0;
    return c * select(1.0 - smoothstep(1.0 - t - feather * 0.5, 1.0 - t + feather * 0.5, f), 0.0, t >= 1.0);
}

// Glass wipe: p0 = (completion %, softness %, displacement px).
@fragment
fn fs_glass_wipe(in: VsOut) -> @location(0) vec4<f32> {
    let px = cx_px();
    let g = st_lum(in.uv);
    let t = fx.p0.x / 100.0;
    let soft = max(fx.p0.y / 100.0, 0.02);
    let keep = tr_keep(g, t, soft);
    // Near the edge, the picture bends along the brightness's slope.
    let band = keep * (1.0 - keep) * 4.0;
    let grad = vec2(st_lum(in.uv + vec2(2.0, 0.0) * px) - st_lum(in.uv - vec2(2.0, 0.0) * px), st_lum(in.uv + vec2(0.0, 2.0) * px) - st_lum(in.uv - vec2(0.0, 2.0) * px));
    let c = cx_at(in.uv + grad * fx.p0.z * band * px * 4.0);
    return c * keep;
}

// Grid wipe: p0 = (completion %, centre x, y %, rotation °), p1 = (border px, tiles across, shape, reverse).
@fragment
fn fs_grid_wipe(in: VsOut) -> @location(0) vec4<f32> {
    let c = cx_at(in.uv);
    let size = dx_size();
    let a = radians(fx.p0.w);
    let rel0 = dx_q(in.uv) - fx.p0.yz / 100.0 * size;
    let rel = vec2(rel0.x * cos(a) + rel0.y * sin(a), -rel0.x * sin(a) + rel0.y * cos(a));
    let tile_w = size.x / max(round(fx.p1.y), 1.0);
    let f = fract(rel / tile_w + 0.5) - 0.5;
    var t = clamp(fx.p0.x / 100.0, 0.0, 1.0);
    let border = fx.p1.x / tile_w;
    var d = 0.0;
    switch u32(round(fx.p1.z)) {
        case 0u: { d = abs(f.x) * 2.0; }
        case 1u: { d = length(f) * 1.414; }
        default: { d = max(abs(f.x), abs(f.y)) * 2.0; }
    }
    var keep = select(0.0, 1.0, d > t);
    if (fx.p1.w >= 0.5) {
        keep = select(0.0, 1.0, d < 1.0 - t);
    }
    if (t > 0.0 && max(abs(f.x), abs(f.y)) * 2.0 > 1.0 - border) {
        keep = 0.0;
    }
    return c * keep;
}

// Jaws: p0 = (completion %, centre x, y %, direction °), p1 = (height px, width px, shape).
@fragment
fn fs_jaws(in: VsOut) -> @location(0) vec4<f32> {
    let size = dx_size();
    let a = radians(fx.p0.w);
    let along = vec2(cos(a), sin(a));
    let across = vec2(-along.y, along.x);
    let q = dx_q(in.uv);
    let c0 = fx.p0.yz / 100.0 * size;
    let t = clamp(fx.p0.x / 100.0, 0.0, 1.0);
    // Far enough apart by the end that both halves have left the layer.
    let reach = abs(across.x) * size.x * 0.5 + abs(across.y) * size.y * 0.5 + abs(dot(c0 - size * 0.5, across));
    let open = t * (reach + fx.p1.x);
    let h = max(fx.p1.x, 0.0);
    let w = max(fx.p1.y, 1.0);
    // Each half is moved away by `open`; the pixel is in whichever half covers it.
    for (var side = 0; side < 2; side = side + 1) {
        let sg = select(1.0, -1.0, side == 1);
        let src = q - across * sg * open;
        let u = dot(src - c0, along);
        let v = dot(src - c0, across) * sg;
        let f = fract(u / w + select(0.0, 0.5, side == 1));
        var tooth = 0.0;
        switch u32(round(fx.p1.z)) {
            case 1u: { tooth = select(0.0, h, f < 0.5) * (0.5 + 0.5 * step(0.25, abs(f - 0.25))); }
            case 2u: { tooth = select(0.0, h, f < 0.5); }
            case 3u: { tooth = (0.5 + 0.5 * sin(f * 6.2831853)) * h; }
            default: { tooth = (1.0 - abs(2.0 * f - 1.0)) * h; }
        }
        // This half: the side beyond the line, its teeth reaching back over it.
        if (v + tooth * 0.5 - h * 0.25 > 0.0) {
            return dx_at(src);
        }
    }
    return vec4(0.0);
}

// Light wipe: p0 = (completion %, centre x, y %, intensity %), p1 = (shape, direction °, colour from source, colour), p2.x = reverse.
@fragment
fn fs_light_wipe(in: VsOut) -> @location(0) vec4<f32> {
    let c = cx_at(in.uv);
    let size = dx_size();
    var rel = dx_q(in.uv) - fx.p0.yz / 100.0 * size;
    let a = radians(fx.p1.y);
    rel = vec2(rel.x * cos(a) + rel.y * sin(a), -rel.x * sin(a) + rel.y * cos(a));
    var d = length(rel);
    switch u32(round(fx.p1.x)) {
        case 1u: { d = max(abs(rel.x), abs(rel.y)) * 1.2; }
        case 2u: { d = abs(rel.x) * 2.0; }
        default: {}
    }
    var t = clamp(fx.p0.x / 100.0, 0.0, 1.0);
    // Just far enough to clear the layer from the centre (its farthest corner, in the shape's measure).
    let c0 = fx.p0.yz / 100.0 * size;
    let corner = length(max(abs(c0), abs(size - c0)));
    let glow_w = max(length(size) * 0.03, 3.0);
    let far = corner * select(1.2, 2.0, u32(round(fx.p1.x)) == 2u) + glow_w * 2.0;
    let r = t * far;
    var keep = smoothstep(r - 1.0, r + 1.0, d);
    var glow = exp(-pow((d - r) / glow_w, 2.0)) * select(0.0, 1.0, t > 0.0 && t < 1.0);
    if (fx.p2.x >= 0.5) {
        keep = 1.0 - smoothstep(far - r - 1.0, far - r + 1.0, d);
        glow = exp(-pow((d - (far - r)) / glow_w, 2.0)) * select(0.0, 1.0, t > 0.0 && t < 1.0);
    }
    let col = select(rgb_of(fx.p1.w), unpremul(c), fx.p1.z >= 0.5);
    let light = col * glow * fx.p0.w / 100.0 * 1.5;
    let o = c * keep;
    return vec4(o.rgb + light * max(c.a, 0.0), max(o.a, min(glow, 1.0) * c.a));
}

// Line sweep: p0 = (completion %, direction °, thickness px, slant %), p1.x = flip.
@fragment
fn fs_line_sweep(in: VsOut) -> @location(0) vec4<f32> {
    let c = cx_at(in.uv);
    let size = dx_size();
    let a = radians(fx.p0.y);
    let dir = vec2(cos(a), sin(a));
    let q = dx_q(in.uv) - size * 0.5;
    let half = abs(dir.x) * size.x * 0.5 + abs(dir.y) * size.y * 0.5;
    var pos = (dot(q, dir) + half) / (2.0 * half);
    if (fx.p1.x >= 0.5) {
        pos = 1.0 - pos;
    }
    let stripe = fract(dot(q, vec2(-dir.y, dir.x)) / max(fx.p0.z, 1.0));
    // Each stripe's own share of the sweep: the slant puts some ahead of others.
    let g = pos * (1.0 - fx.p0.w / 100.0) + stripe * fx.p0.w / 100.0;
    return c * tr_keep(g, fx.p0.x / 100.0, 0.001);
}

// Radial scale wipe: p0 = (completion %, centre x, y %, reverse).
@fragment
fn fs_radial_scale_wipe(in: VsOut) -> @location(0) vec4<f32> {
    let size = dx_size();
    let centre = fx.p0.yz / 100.0 * size;
    let rel = dx_q(in.uv) - centre;
    let d = length(rel);
    let t = clamp(fx.p0.x / 100.0, 0.0, 1.0);
    // Just far enough to clear the layer (its farthest corner from the centre).
    let far = length(max(abs(centre), abs(size - centre))) + 1.0;
    let r = t * far;
    let band = far * 0.25;
    var keep = smoothstep(r - 1.0, r + 1.0, d);
    if (fx.p0.w >= 0.5) {
        keep = 1.0 - smoothstep(far - r - 1.0, far - r + 1.0, d);
    }
    // Just outside the hole, the picture is pulled out from nearer the middle.
    let k = clamp((d - r) / band, 0.0, 1.0);
    let src_d = mix(r * 0.3 + d * 0.7, d, k);
    let src = centre + select(vec2(0.0), rel / d, d > 1e-4) * select(src_d, d, fx.p0.w >= 0.5);
    return dx_at(src) * keep;
}

// Scale wipe: p0 = (stretch %, centre x, y %, direction °).
@fragment
fn fs_scale_wipe(in: VsOut) -> @location(0) vec4<f32> {
    let size = dx_size();
    let centre = fx.p0.yz / 100.0 * size;
    let a = radians(fx.p0.w);
    let dir = vec2(cos(a), sin(a));
    let q = dx_q(in.uv);
    let along = dot(q - centre, dir);
    if (along <= 0.0) {
        return dx_at(q);
    }
    let k = 1.0 + fx.p0.x / 100.0 * 10.0;
    return dx_at(q - dir * along * (1.0 - 1.0 / k));
}

// Twister: p0 = (completion %, back, shading, centre x), p1 = (centre y %, axis °).
// The axis runs through the centre; each slice across it turns about it, the first slices
// first, half a turn each by the end.
@fragment
fn fs_twister(in: VsOut) -> @location(0) vec4<f32> {
    let size = dx_size();
    let centre = vec2(fx.p0.w, fx.p1.x) / 100.0 * size;
    let a = radians(fx.p1.y);
    let along = vec2(cos(a), sin(a));
    let across = vec2(-along.y, along.x);
    let rel = dx_q(in.uv) - centre;
    let u = dot(rel, along);
    let v = dot(rel, across);
    let half = abs(along.x) * size.x * 0.5 + abs(along.y) * size.y * 0.5;
    let pos = (u + half) / (2.0 * half);
    let t = clamp(fx.p0.x / 100.0, 0.0, 1.0);
    let th = clamp((t * 2.0 - pos) * 1.0, 0.0, 1.0) * 3.14159265;
    let k = cos(th);
    if (abs(k) < 1e-3) {
        return vec4(0.0);
    }
    let src_v = v / k;
    var col = dx_at(centre + along * u + across * src_v);
    let back = k < 0.0;
    let mode = u32(round(fx.p0.y));
    if (back && mode == 2u) {
        return vec4(0.0);
    }
    if (back && mode == 1u) {
        col = vec4(col.rgb * 0.35, col.a);
    }
    if (fx.p0.z >= 0.5) {
        col = vec4(col.rgb * (0.4 + 0.6 * abs(k)), col.a);
    }
    return col;
}

fn wo_lum(uv: vec2<f32>, s: f32) -> f32 {
    var l = st_lum(uv);
    for (var i = 0; i < 6; i = i + 1) {
        let a = f32(i) * 1.0472;
        l = l + st_lum(uv + vec2(cos(a), sin(a)) * s * cx_px());
    }
    return l / 7.0;
}

// WarpoMatic: p0 = (completion %, smoothness px, amount px, direction), p1.x = blend span %.
@fragment
fn fs_warpomatic(in: VsOut) -> @location(0) vec4<f32> {
    let px = cx_px();
    let s = max(fx.p0.y, 0.5);
    let t = clamp(fx.p0.x / 100.0, 0.0, 1.0);
    let g = vec2(wo_lum(in.uv + vec2(s, 0.0) * px, s) - wo_lum(in.uv - vec2(s, 0.0) * px, s), wo_lum(in.uv + vec2(0.0, s) * px, s) - wo_lum(in.uv - vec2(0.0, s) * px, s));
    var dir = g;
    switch u32(round(fx.p0.w)) {
        case 0u: { dir = vec2(-g.y, g.x); }
        case 2u: { dir = -g; }
        case 3u: { dir = vec2(g.y, -g.x) + g; }
        default: {}
    }
    let warp = dir * fx.p0.z * sin(t * 3.14159265) * 8.0;
    let c = cx_at(in.uv + warp * px);
    let l = wo_lum(in.uv, s);
    return c * tr_keep(l, t, max(fx.p1.x / 100.0, 0.01));
}

// The number being drawn: written once per pixel into these (characters right to left).
var<private> nm_digits: array<i32, 24>;
var<private> nm_count: i32;

fn ut_char(k: i32) -> i32 {
    if (k < 0 || k >= nm_count) {
        return 14;
    }
    return nm_digits[nm_count - 1 - k];
}

fn nm_push(ch: i32) {
    if (nm_count < 24) {
        nm_digits[nm_count] = ch;
        nm_count = nm_count + 1;
    }
}

fn nm_two(v: i32) {
    nm_push(v % 10);
    nm_push((v / 10) % 10);
}

// A 3 × 5 pixel font for numbers: 0–9, ':', '.', '-', '/', ' ' (bits row by row, top-left first).
fn ut_glyph(ch: i32, cell: vec2<i32>) -> bool {
    var g = array<u32, 15>(31599u, 11415u, 29671u, 29647u, 23497u, 31183u, 31215u, 29257u, 31727u, 31695u, 1040u, 2u, 448u, 4772u, 0u);
    if (ch < 0 || ch > 14 || cell.x < 0 || cell.x > 2 || cell.y < 0 || cell.y > 4) {
        return false;
    }
    let bit = 14 - (cell.y * 3 + cell.x);
    return ((g[ch] >> u32(bit)) & 1u) == 1u;
}

// Draws `count` characters (from ut_char) at `pos` (top left), `size` px high, with a box behind.
fn ut_draw(c: vec4<f32>, q: vec2<f32>, pos: vec2<f32>, size: f32, count: i32, col: vec3<f32>, box_col: vec3<f32>, box_a: f32) -> vec4<f32> {
    let unit = max(size, 2.0) / 5.0;
    let local = (q - pos) / unit;
    let width = f32(count) * 4.0 - 1.0;
    var o = c;
    // The box (a unit of padding round the text).
    if (box_a > 0.0 && local.x > -1.0 && local.x < width + 1.0 && local.y > -1.0 && local.y < 6.0) {
        o = vec4(box_col * box_a, box_a) + o * (1.0 - box_a);
    }
    if (local.x < 0.0 || local.y < 0.0 || local.x >= width || local.y >= 5.0) {
        return o;
    }
    let k = i32(floor(local.x / 4.0));
    let cx = i32(floor(local.x)) - k * 4;
    let ch = ut_char(k);
    if (ut_glyph(ch, vec2<i32>(cx, i32(floor(local.y))))) {
        return vec4(col, 1.0);
    }
    return o;
}

// Numbers: p0 = (type, value, decimals, x %), p1 = (y %, size px, colour, box %), p2 = (box colour, fps, seed).
@fragment
fn fs_numbers(in: VsOut) -> @location(0) vec4<f32> {
    let c = cx_at(in.uv);
    let kind = u32(round(fx.p0.x));
    let t = fx.extra.y;
    nm_count = 0;
    if (kind == 3u) {
        let fps = max(round(fx.p2.y), 1.0);
        let frames = i32(floor(max(t, 0.0) * fps + 1e-3));
        let f = frames % i32(fps);
        let s = (frames / i32(fps)) % 60;
        let m = (frames / i32(fps * 60.0)) % 60;
        let h = (frames / i32(fps * 3600.0)) % 100;
        nm_two(f);
        nm_push(10);
        nm_two(s);
        nm_push(10);
        nm_two(m);
        nm_push(10);
        nm_two(h);
    } else {
        var v = fx.p0.y;
        if (kind == 1u) {
            v = gn_rand2(vec2(floor(t * 30.0), 1.0), fx.p2.z).x * max(abs(fx.p0.y), 1.0);
        } else if (kind == 2u) {
            v = t;
        }
        let dec = i32(clamp(round(fx.p0.z), 0.0, 6.0));
        let neg = v < 0.0;
        let scaled = round(abs(v) * pow(10.0, f32(dec)));
        var n = min(scaled, 1.0e15);
        var i = 0;
        loop {
            if (i == dec && dec > 0) {
                nm_push(11);
            }
            let d = i32(n - floor(n / 10.0) * 10.0);
            nm_push(d);
            n = floor(n / 10.0);
            i = i + 1;
            if ((n < 1.0 && i > dec) || nm_count >= 22) {
                break;
            }
        }
        if (neg) {
            nm_push(12);
        }
    }
    let size = dx_size();
    let pos = vec2(fx.p0.w, fx.p1.x) / 100.0 * size;
    return ut_draw(c, dx_q(in.uv), pos, fx.p1.y, nm_count, rgb_of(fx.p1.z), rgb_of(fx.p2.x), fx.p1.w / 100.0);
}

// Timecode: p0 = (format, fps, start frames, x %), p1 = (y %, size px, colour, box %), p2.x = box colour.
@fragment
fn fs_timecode(in: VsOut) -> @location(0) vec4<f32> {
    let c = cx_at(in.uv);
    let fps = max(round(fx.p0.y), 1.0);
    let frames = max(i32(floor(fx.extra.y * fps + 1e-3)) + i32(round(fx.p0.z)), 0);
    nm_count = 0;
    switch u32(round(fx.p0.x)) {
        case 1u: {
            var n = frames;
            loop {
                nm_push(n % 10);
                n = n / 10;
                if (n == 0 || nm_count >= 12) {
                    break;
                }
            }
        }
        case 2u: {
            let tenths = (frames * 100) / i32(fps);
            nm_push(tenths % 10);
            nm_push((tenths / 10) % 10);
            nm_push(11);
            var n = tenths / 100;
            loop {
                nm_push(n % 10);
                n = n / 10;
                if (n == 0 || nm_count >= 12) {
                    break;
                }
            }
        }
        default: {
            let f = frames % i32(fps);
            let s = (frames / i32(fps)) % 60;
            let m = (frames / i32(fps * 60.0)) % 60;
            let h = (frames / i32(fps * 3600.0)) % 100;
            nm_two(f);
            nm_push(10);
            nm_two(s);
            nm_push(10);
            nm_two(m);
            nm_push(10);
            nm_two(h);
        }
    }
    let size = dx_size();
    let unit = max(fx.p1.y, 2.0) / 5.0;
    let width = (f32(nm_count) * 4.0 - 1.0) * unit;
    // Centred on its position.
    let pos = vec2(fx.p0.w, fx.p1.x) / 100.0 * size - vec2(width * 0.5, fx.p1.y * 0.5);
    return ut_draw(c, dx_q(in.uv), pos, fx.p1.y, nm_count, rgb_of(fx.p1.z), rgb_of(fx.p2.x), fx.p1.w / 100.0);
}

// Cineon converter: p0 = (conversion, black, white (10-bit), gamma), p1.x = highlight rolloff.
@fragment
fn fs_cineon(in: VsOut) -> @location(0) vec4<f32> {
    let c = cx_at(in.uv);
    let d = cx_disp(c);
    let black = fx.p0.y / 1023.0;
    let white = max(fx.p0.z / 1023.0, black + 0.01);
    let g = max(fx.p0.w, 0.1);
    // Kodak's: 0.002 density per code value, over a film gamma of 0.6.
    let k = 0.002 * 1023.0 / 0.6;
    let off = pow(10.0, (black - white) * k);
    var o = d;
    let conv = u32(round(fx.p0.x));
    if (conv == 0u || conv == 2u) {
        o = (pow(vec3(10.0), (d - white) * k) - off) / (1.0 - off);
        o = pow(max(o, vec3(0.0)), vec3(1.0 / g));
        let roll = fx.p1.x / 100.0;
        o = select(o, 1.0 - roll * exp(-(o - (1.0 - roll)) / max(roll, 1e-3)), o > vec3(1.0 - roll) & vec3(roll > 0.0));
    }
    if (conv == 1u || conv == 2u) {
        let lin = pow(max(o, vec3(0.0)), vec3(g));
        o = white + log(max(lin * (1.0 - off) + off, vec3(1e-6))) / log(10.0) / k;
    }
    return cx_out(o, c.a);
}

// HDR highlight compression: p0 = (amount %, knee %).
@fragment
fn fs_hdr_compress(in: VsOut) -> @location(0) vec4<f32> {
    let c = cx_at(in.uv);
    let rgb = unpremul(c);
    let knee = clamp(fx.p0.y / 100.0, 0.1, 1.0);
    let m = max(rgb.r, max(rgb.g, rgb.b));
    if (m <= knee) {
        return c;
    }
    // Above the knee: what's left of the range shared out smoothly (a soft shoulder).
    let room = 1.0 - knee;
    let over = m - knee;
    let squeezed = knee + room * over / (over + room);
    let k = clamp(fx.p0.x / 100.0, 0.0, 1.0);
    let target_m = mix(m, squeezed, k);
    return vec4(rgb * (target_m / m) * c.a, c.a);
}

// Ball action: p0 = (scatter px, spacing px, size %, twist °), p1 = (light °, seed).
@fragment
fn fs_ball_action(in: VsOut) -> @location(0) vec4<f32> {
    let size = dx_size();
    let q = dx_q(in.uv);
    let sp = max(fx.p0.y, 2.0);
    let rad = sp * 0.5 * fx.p0.z / 100.0;
    let reach = i32(clamp(ceil((fx.p0.x + rad) / sp), 1.0, 4.0));
    let base = floor(q / sp);
    var best = vec4(0.0);
    var best_z = 1e9;
    for (var y = -reach; y <= reach; y = y + 1) {
        for (var x = -reach; x <= reach; x = x + 1) {
            let cell = base + vec2(f32(x), f32(y));
            let home = (cell + 0.5) * sp;
            let col = dx_at(home);
            if (col.a < 0.05) {
                continue;
            }
            let r = gn_rand2(cell, fx.p1.y) - 0.5;
            // Twisted about the middle by its distance out, and scattered.
            let rel = home - size * 0.5;
            let tw = radians(fx.p0.w) * length(rel) / length(size);
            let turned = vec2(rel.x * cos(tw) - rel.y * sin(tw), rel.x * sin(tw) + rel.y * cos(tw));
            let centre = size * 0.5 + turned + r * 2.0 * fx.p0.x;
            let d = q - centre;
            let dd = dot(d, d);
            if (dd < rad * rad && r.x < best_z) {
                let n = vec3(d / rad, sqrt(max(1.0 - dd / (rad * rad), 0.0)));
                let a = radians(fx.p1.x);
                let l = normalize(vec3(cos(a), -sin(a), 1.0));
                let lit = 0.35 + 0.65 * max(dot(n, l), 0.0);
                let spec = pow(max(dot(n, normalize(l + vec3(0.0, 0.0, 1.0))), 0.0), 40.0);
                let edge = clamp((rad - sqrt(dd)) * 1.5, 0.0, 1.0);
                best = vec4((unpremul(col) * lit + vec3(spec * 0.6)) * edge, edge);
                best_z = r.x;
            }
        }
    }
    return best;
}

// Bubbles: p0 = (amount, speed px/s, wobble px, size px), p1 = (size variance %, sheen %, seed).
@fragment
fn fs_bubbles(in: VsOut) -> @location(0) vec4<f32> {
    let size = dx_size();
    let q = dx_q(in.uv);
    let c = cx_at(in.uv);
    let t = fx.extra.y;
    let big = max(fx.p0.w, 2.0);
    // Bubbles live in columns a bubble or two wide; each column holds a few, rising and wrapping round.
    let col_w = big * 2.5;
    let n = clamp(fx.p0.x / max(size.x / col_w, 1.0), 0.0, 12.0);
    let col = floor(q.x / col_w);
    var acc = c;
    for (var dc = -1; dc <= 1; dc = dc + 1) {
        let cc = col + f32(dc);
        for (var i = 0; i < 12; i = i + 1) {
            if (f32(i) >= n) {
                break;
            }
            let r = gn_rand2(vec2(cc, f32(i)), fx.p1.z);
            let r2 = gn_rand2(vec2(f32(i), cc), fx.p1.z + 5.0);
            let rad = big * 0.5 * (1.0 - fx.p1.x / 100.0 * r2.x * 0.8);
            let span = size.y + rad * 4.0;
            let y = size.y + rad * 2.0 - fract(r.y + t * fx.p0.y / span) * span;
            let x = (cc + 0.5) * col_w + (r.x - 0.5) * col_w * 0.6 + sin(t * 2.0 + r2.y * 6.28) * fx.p0.z;
            let d = q - vec2(x, y);
            let dist = length(d);
            if (dist < rad) {
                let k = dist / rad;
                // A lens: the picture behind, magnified, with a rainbow film and a rim.
                let behind = cx_at((vec2(x, y) + d * (0.6 + 0.4 * k)) / size);
                let film = hue_rgb(fract(k * 1.5 + r.x + t * 0.1) * 360.0) * fx.p1.y / 100.0 * 0.25 * k * k;
                let rim = smoothstep(0.8, 1.0, k) * 0.5;
                let hl = pow(max(1.0 - length(d / rad - vec2(-0.35, -0.35)) * 3.0, 0.0), 2.0);
                let rgb = unpremul(behind) * 0.95 + film + vec3(rim + hl);
                let a = max(behind.a, 0.15 + rim + hl);
                let aa = clamp((rad - dist) * 1.5, 0.0, 1.0);
                acc = mix(acc, vec4(rgb * a, a), aa);
            }
        }
    }
    return acc;
}

// Drizzle: p0 = (drops/s, longevity s, rippling, displacement px), p1 = (spread px/s, light %, seed).
@fragment
fn fs_drizzle(in: VsOut) -> @location(0) vec4<f32> {
    let size = dx_size();
    let q = dx_q(in.uv);
    let t = fx.extra.y;
    let life = max(fx.p0.y, 0.1);
    let rate = max(fx.p0.x, 0.0);
    // The drops alive now: born over the last `life` seconds (at most 64).
    let born_last = floor(t * rate);
    let count = min(ceil(life * rate) + 1.0, 64.0);
    var push = vec2(0.0);
    var light = 0.0;
    for (var i = 0.0; i < count; i = i + 1.0) {
        let k = born_last - i;
        if (k < 0.0) {
            break;
        }
        let born = k / max(rate, 1e-3);
        let age = t - born;
        if (age < 0.0 || age > life) {
            continue;
        }
        let r = gn_rand2(vec2(k, 3.0), fx.p1.z) * size;
        let rel = q - r;
        let d = length(rel);
        let ring = age * fx.p1.x;
        let x = (d - ring) / max(fx.p1.x * 0.08, 2.0);
        let fade = (1.0 - age / life);
        let wave = sin(x * fx.p0.z) * exp(-x * x * 0.5) * fade;
        if (d > 1e-3) {
            push = push + rel / d * wave;
        }
        light = light + max(wave, 0.0) * fade;
    }
    let c = cx_at((q + push * fx.p0.w) / size);
    return vec4(c.rgb + vec3(light * fx.p1.y / 100.0 * 0.3) * c.a, c.a);
}

// ---- From the HitFilm list: demultiply, remove stock background ----

// The opacity and colour a pixel `c` (display RGB) has once background `bg`
// is taken out of it: the least opacity that, laid over `bg`, gives `c` back.
fn sb_unmix(c: vec3<f32>, bg: vec3<f32>) -> vec4<f32> {
    var a = 0.0;
    for (var i = 0; i < 3; i = i + 1) {
        let d = c[i] - bg[i];
        let room = select(bg[i], 1.0 - bg[i], d > 0.0);
        a = max(a, abs(d) / max(room, 1e-4));
    }
    a = clamp(a, 0.0, 1.0);
    if (a <= 1e-4) {
        return vec4(0.0);
    }
    return vec4(clamp(bg + (c - bg) / a, vec3(0.0), vec3(1.0)), a);
}

// Demultiply: p0 = (black point %, amount %).
@fragment
fn fs_demultiply(in: VsOut) -> @location(0) vec4<f32> {
    let c = cx_at(in.uv);
    // In linear light, so laid over black it looks just as it did.
    let d = unpremul(c);
    // Below the black point counts as black (noise in the background).
    let b = to_linear(vec3(clamp(fx.p0.x / 100.0, 0.0, 0.5))).x;
    let lifted = max(d - b, vec3(0.0)) / (1.0 - b);
    let u = sb_unmix(lifted, vec3(0.0));
    let o = vec4(u.rgb * u.a * c.a, u.a * c.a);
    return mix(c, o, clamp(fx.p0.y / 100.0, 0.0, 1.0));
}

// Remove stock background: p0 = (background, colour, tolerance %, amount %).
@fragment
fn fs_stock_background(in: VsOut) -> @location(0) vec4<f32> {
    let c = cx_at(in.uv);
    // In linear light, so laid over the background it looks just as it did.
    let d = unpremul(c);
    var bg = vec3(1.0);
    switch u32(round(fx.p0.x)) {
        case 1u: { bg = vec3(0.0); }
        case 2u: { bg = rgb_of(fx.p0.y); }
        default: {}
    }
    // Within the tolerance of the background counts as background.
    let tol = clamp(fx.p0.z / 100.0, 0.0, 0.5);
    let off = d - bg;
    let near = max(abs(off) - vec3(tol), vec3(0.0)) * sign(off);
    let u = sb_unmix(clamp(bg + near / (1.0 - tol), vec3(0.0), vec3(1.0)), bg);
    let o = vec4(u.rgb * u.a * c.a, u.a * c.a);
    return mix(c, o, clamp(fx.p0.w / 100.0, 0.0, 1.0));
}

// ---- From the HitFilm list: hue colourise, chroma blur, deinterlace ----

// Hue colourise: p0 = (hue °, saturation %, lightness %, amount %).
@fragment
fn fs_hue_colorize(in: VsOut) -> @location(0) vec4<f32> {
    let c = cx_at(in.uv);
    if (c.a <= 0.0) {
        return c;
    }
    let d = cx_disp(c);
    var l = cx_rgb2hsl(d).z;
    let k = clamp(fx.p0.z / 100.0, -1.0, 1.0);
    l = select(l * (1.0 + k), l + (1.0 - l) * k, k > 0.0);
    let o = cx_hsl2rgb(vec3(fract(fx.p0.x / 360.0), clamp(fx.p0.y / 100.0, 0.0, 1.0), clamp(l, 0.0, 1.0)));
    return mix(c, cx_out(o, c.a), clamp(fx.p0.w / 100.0, 0.0, 1.0));
}

// Display RGB to Rec.709 YCbCr and back.
fn cb_ycc(c: vec3<f32>) -> vec3<f32> {
    let y = dot(c, vec3(0.2126, 0.7152, 0.0722));
    return vec3(y, (c.b - y) / 1.8556, (c.r - y) / 1.5748);
}
fn cb_rgb(q: vec3<f32>) -> vec3<f32> {
    let r = q.x + 1.5748 * q.z;
    let b = q.x + 1.8556 * q.y;
    let g = (q.x - 0.2126 * r - 0.0722 * b) / 0.7152;
    return vec3(r, g, b);
}

// Chroma blur: p0 = (radius px, amount %). A Gaussian over 9×9 taps spread
// to the radius, of the colour (Cb, Cr) alone, weighted by each tap's opacity.
@fragment
fn fs_chroma_blur(in: VsOut) -> @location(0) vec4<f32> {
    let c = cx_at(in.uv);
    if (c.a <= 0.0) {
        return c;
    }
    let r = max(fx.p0.x, 0.0);
    let step = cx_px() * (r / 4.0);
    var sum = vec2(0.0);
    var wsum = 0.0;
    for (var j = -4; j <= 4; j = j + 1) {
        for (var i = -4; i <= 4; i = i + 1) {
            let o = vec2(f32(i), f32(j));
            let s = cx_at(in.uv + o * step);
            let w = exp(-dot(o, o) / 8.0) * s.a;
            if (w > 0.0) {
                sum += cb_ycc(cx_disp(s)).yz * w;
                wsum += w;
            }
        }
    }
    let here = cb_ycc(cx_disp(c));
    let cc = select(here.yz, sum / max(wsum, 1e-6), wsum > 0.0);
    let o = cb_rgb(vec3(here.x, cc));
    return mix(c, cx_out(o, c.a), clamp(fx.p0.y / 100.0, 0.0, 1.0));
}

// Deinterlace: p0 = (field: 0 upper / 1 lower, method: 0 interpolate / 1
// repeat / 2 blend, amount %). Works on the layer's own rows of pixels.
fn di_row(t: vec2<f32>, row: f32, h: f32) -> vec4<f32> {
    let y = (clamp(row, 0.0, h - 1.0) + 0.5) / h;
    return textureSampleLevel(src, src_sampler, vec2(t.x, y), 0.0);
}

@fragment
fn fs_deinterlace(in: VsOut) -> @location(0) vec4<f32> {
    let c = cx_at(in.uv);
    if (inside(in.uv) <= 0.0) {
        return c;
    }
    let h = f32(textureDimensions(src).y);
    let t = to_tex(in.uv);
    let row = floor(clamp(t.y, 0.0, 0.99999) * h);
    let keep = u32(round(fx.p0.x));
    let method = u32(round(fx.p0.y));
    var o = c;
    if (method == 2u) {
        o = di_row(t, row - 1.0, h) * 0.25 + di_row(t, row, h) * 0.5 + di_row(t, row + 1.0, h) * 0.25;
    } else {
        // Row 0 is the upper field's first line.
        let ours = (u32(row) % 2u) == keep;
        if (!ours) {
            if (method == 1u) {
                o = di_row(t, select(row + 1.0, row - 1.0, keep == 0u), h);
            } else {
                // At the top and bottom, the one line there is.
                let up = select(row - 1.0, row + 1.0, row < 1.0);
                let down = select(row + 1.0, row - 1.0, row + 1.0 >= h);
                o = (di_row(t, up, h) + di_row(t, down, h)) * 0.5;
            }
        }
    }
    return mix(c, o, clamp(fx.p0.z / 100.0, 0.0, 1.0));
}
