// Final pass: linear float accumulation -> sRGB-encoded 8-bit for display/export.

@group(0) @binding(0) var accum: texture_2d<f32>;

@vertex
fn vs_fullscreen(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    // One oversized triangle covering the viewport.
    let x = f32((i << 1u) & 2u) * 2.0 - 1.0;
    let y = f32(i & 2u) * 2.0 - 1.0;
    return vec4(x, y, 0.0, 1.0);
}

fn srgb_from_linear(c: vec3<f32>) -> vec3<f32> {
    let lo = c * 12.92;
    let hi = 1.055 * pow(max(c, vec3(0.0)), vec3(1.0 / 2.4)) - 0.055;
    return select(hi, lo, c <= vec3(0.0031308));
}

@fragment
fn fs_output(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {
    let c = textureLoad(accum, vec2<i32>(p.xy), 0);
    return vec4(srgb_from_linear(clamp(c.rgb, vec3(0.0), vec3(1.0))), 1.0);
}

// For a picture that will be composited again (a composition): keep the
// coverage, as straight (not premultiplied) alpha.
@fragment
fn fs_output_alpha(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {
    let c = textureLoad(accum, vec2<i32>(p.xy), 0);
    let a = clamp(c.a, 0.0, 1.0);
    if (a <= 0.0) {
        return vec4(0.0);
    }
    return vec4(srgb_from_linear(clamp(c.rgb / a, vec3(0.0), vec3(1.0))), a);
}

// ---- 16-bit output for export (10-bit and HDR video) -----------------------
// Written to a float target and read back as 16 bits per channel, already in
// the delivery curve. The edit is in linear BT.709 light where 1.0 is SDR
// white; values above 1.0 (glows, exposure) survive into HDR.

// Linear BT.709 RGB -> linear BT.2020 RGB (columns).
const BT709_TO_2020 = mat3x3<f32>(
    vec3(0.6274040, 0.0690970, 0.0163916),
    vec3(0.3292820, 0.9195400, 0.0880132),
    vec3(0.0433136, 0.0113612, 0.8955950),
);

// SMPTE ST 2084 (PQ) inverse EOTF; `y` is luminance / 10000 nits.
fn pq_from_linear(y: vec3<f32>) -> vec3<f32> {
    let m1 = 0.1593017578125;
    let m2 = 78.84375;
    let c1 = 0.8359375;
    let c2 = 18.8515625;
    let c3 = 18.6875;
    let p = pow(clamp(y, vec3(0.0), vec3(1.0)), vec3(m1));
    return pow((c1 + c2 * p) / (1.0 + c3 * p), vec3(m2));
}

// ARIB STD-B67 (HLG) OETF on normalised scene light 0..1.
fn hlg_from_linear(e: vec3<f32>) -> vec3<f32> {
    let a = 0.17883277;
    let b = 0.28466892;
    let c = 0.55991073;
    let x = clamp(e, vec3(0.0), vec3(1.0));
    let lo = sqrt(3.0 * x);
    let hi = a * log(max(12.0 * x - b, vec3(1e-6))) + c;
    return select(hi, lo, x <= vec3(1.0 / 12.0));
}

// SDR white is 203 nits in PQ and 75% signal in HLG (ITU-R BT.2408).
const SDR_WHITE_NITS = 203.0;
const HLG_SDR_WHITE = 0.26496256;

@fragment
fn fs_deep_srgb(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {
    let c = textureLoad(accum, vec2<i32>(p.xy), 0);
    return vec4(srgb_from_linear(clamp(c.rgb, vec3(0.0), vec3(1.0))), 1.0);
}

@fragment
fn fs_deep_pq(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {
    let c = textureLoad(accum, vec2<i32>(p.xy), 0);
    let wide = max(BT709_TO_2020 * c.rgb, vec3(0.0));
    return vec4(pq_from_linear(wide * (SDR_WHITE_NITS / 10000.0)), 1.0);
}

@fragment
fn fs_deep_hlg(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {
    let c = textureLoad(accum, vec2<i32>(p.xy), 0);
    let wide = max(BT709_TO_2020 * c.rgb, vec3(0.0));
    return vec4(hlg_from_linear(wide * HLG_SDR_WHITE), 1.0);
}
