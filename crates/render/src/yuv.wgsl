// YUV planes (4:2:0, 4:2:2 or 4:4:4, 8–16 bits, with or without alpha) ->
// the layer's RGBA texture. Same maths as core::frame::YuvPlanes::to_rgba.
//
// fs_yuv writes an 8-bit sRGB texture: linear out (the hardware encodes),
// straight alpha, clamped. fs_yuv_float writes a half-float texture for deep
// (10-bit and up) video: linear, premultiplied, and values just above white
// (super-whites, which 10- and 12-bit cameras record) are kept.

struct Params {
    shift: u32,
    interleaved: u32,
    y_off: f32,
    y_scale: f32,
    c_off: f32,
    c_scale: f32,
    kr: f32,
    kb: f32,
    size: vec2<u32>,
    csize: vec2<u32>,
    // log2 of the chroma subsampling, x and y.
    cshift: vec2<u32>,
    // The alpha plane's largest value (0: no alpha plane).
    amax: f32,
    // How the signal maps to light: 0 SDR, 1 PQ (HDR10), 2 HLG.
    transfer: u32,
};

@group(0) @binding(0) var y_tex: texture_2d<u32>;
@group(0) @binding(1) var u_tex: texture_2d<u32>;
@group(0) @binding(2) var v_tex: texture_2d<u32>;
@group(0) @binding(3) var<uniform> p: Params;
@group(0) @binding(4) var a_tex: texture_2d<u32>;

@vertex
fn vs_fullscreen(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let x = f32((i << 1u) & 2u) * 2.0 - 1.0;
    let y = f32(i & 2u) * 2.0 - 1.0;
    return vec4(x, y, 0.0, 1.0);
}

// U (.x) and V (.y) code values at chroma sample `c`.
fn chroma(c: vec2<i32>) -> vec2<f32> {
    let a = textureLoad(u_tex, c, 0);
    if (p.interleaved != 0u) {
        return vec2(f32(a.x >> p.shift), f32(a.y >> p.shift));
    }
    let b = textureLoad(v_tex, c, 0);
    return vec2(f32(a.x >> p.shift), f32(b.x >> p.shift));
}

fn linear_from_srgb(c: vec3<f32>) -> vec3<f32> {
    let lo = c / 12.92;
    let hi = pow((c + 0.055) / 1.055, vec3(2.4));
    return select(hi, lo, c <= vec3(0.04045));
}

// Gamma-encoded R'G'B' at pixel `xy` (not clamped).
fn rgb_at(xy: vec2<i32>) -> vec3<f32> {
    let yv = (f32(textureLoad(y_tex, xy, 0).x >> p.shift) - p.y_off) * p.y_scale;
    // Subsampled chroma: level with even columns; 4:2:0 halfway between rows (MPEG-2 siting).
    let cmax = vec2<f32>(p.csize) - 1.0;
    var f = vec2<f32>(xy);
    if (p.cshift.x != 0u) {
        f.x = f.x * 0.5;
    }
    if (p.cshift.y != 0u) {
        f.y = (f.y - 0.5) * 0.5;
    }
    f = clamp(f, vec2(0.0), cmax);
    let c0 = vec2<i32>(floor(f));
    let c1 = min(c0 + vec2(1, 1), vec2<i32>(cmax));
    let t = f - floor(f);
    let top = mix(chroma(c0), chroma(vec2(c1.x, c0.y)), t.x);
    let bottom = mix(chroma(vec2(c0.x, c1.y)), chroma(c1), t.x);
    let uv = (mix(top, bottom, t.y) - p.c_off) * p.c_scale;
    let r = yv + 2.0 * (1.0 - p.kr) * uv.y;
    let b = yv + 2.0 * (1.0 - p.kb) * uv.x;
    let g = (yv - p.kr * r - p.kb * b) / (1.0 - p.kr - p.kb);
    return vec3(r, g, b);
}

fn alpha_at(xy: vec2<i32>) -> f32 {
    if (p.amax <= 0.0) {
        return 1.0;
    }
    return clamp(f32(textureLoad(a_tex, xy, 0).x) / p.amax, 0.0, 1.0);
}

// SDR white is 203 nits in PQ and 75% signal in HLG (ITU-R BT.2408): 1.0 here,
// so HDR highlights come out above 1. The inverses of output.wgsl's HDR curves.
const SDR_WHITE_NITS = 203.0;
const HLG_SDR_WHITE = 0.26496256;

fn linear_from_pq(e: vec3<f32>) -> vec3<f32> {
    let m1 = 0.1593017578125;
    let m2 = 78.84375;
    let c1 = 0.8359375;
    let c2 = 18.8515625;
    let c3 = 18.6875;
    let q = pow(clamp(e, vec3(0.0), vec3(1.0)), vec3(1.0 / m2));
    let y = pow(max(q - c1, vec3(0.0)) / (c2 - c3 * q), vec3(1.0 / m1));
    return y * (10000.0 / SDR_WHITE_NITS);
}

fn linear_from_hlg(e: vec3<f32>) -> vec3<f32> {
    let a = 0.17883277;
    let b = 0.28466892;
    let c = 0.55991073;
    let x = max(e, vec3(0.0));
    let lo = x * x / 3.0;
    let hi = (exp((x - c) / a) + b) / 12.0;
    return select(hi, lo, x <= vec3(0.5)) / HLG_SDR_WHITE;
}

// Linear BT.2020 → linear BT.709 (WGSL matrices are column by column).
const BT2020_TO_709 = mat3x3<f32>(
    vec3(1.660491, -0.124551, -0.018151),
    vec3(-0.587641, 1.132900, -0.100579),
    vec3(-0.072850, -0.008349, 1.118730),
);

// Signal → linear light in the compositor's BT.709 (HDR: wide primaries brought in, negatives cut).
fn linear_from_signal(rgb: vec3<f32>) -> vec3<f32> {
    if (p.transfer == 1u) {
        return max(BT2020_TO_709 * linear_from_pq(rgb), vec3(0.0));
    }
    if (p.transfer == 2u) {
        return max(BT2020_TO_709 * linear_from_hlg(rgb), vec3(0.0));
    }
    return linear_from_srgb(rgb);
}

@fragment
fn fs_yuv(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let xy = vec2<i32>(pos.xy);
    let rgb = clamp(rgb_at(xy), vec3(0.0), vec3(1.0));
    return vec4(clamp(linear_from_signal(rgb), vec3(0.0), vec3(1.0)), alpha_at(xy));
}

@fragment
fn fs_yuv_float(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let xy = vec2<i32>(pos.xy);
    // Below black and out-of-gamut negatives go; above white stays.
    let rgb = max(rgb_at(xy), vec3(0.0));
    let a = alpha_at(xy);
    return vec4(linear_from_signal(rgb) * a, a);
}
