// YUV 4:2:0 planes -> the layer's RGBA texture (sRGB storage, so we output
// linear and the hardware encodes). Same maths as core::frame::YuvPlanes::to_rgba.

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
};

@group(0) @binding(0) var y_tex: texture_2d<u32>;
@group(0) @binding(1) var u_tex: texture_2d<u32>;
@group(0) @binding(2) var v_tex: texture_2d<u32>;
@group(0) @binding(3) var<uniform> p: Params;

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

@fragment
fn fs_yuv(@builtin(position) pos: vec4<f32>) -> @location(0) vec4<f32> {
    let xy = vec2<i32>(pos.xy);
    let yv = (f32(textureLoad(y_tex, xy, 0).x >> p.shift) - p.y_off) * p.y_scale;
    // 4:2:0 chroma: level with even columns, halfway between rows (MPEG-2 siting).
    let cmax = vec2<f32>(p.csize) - 1.0;
    let f = vec2(min(f32(xy.x) * 0.5, cmax.x), clamp((f32(xy.y) - 0.5) * 0.5, 0.0, cmax.y));
    let c0 = vec2<i32>(floor(f));
    let c1 = min(c0 + vec2(1, 1), vec2<i32>(cmax));
    let t = f - floor(f);
    let top = mix(chroma(c0), chroma(vec2(c1.x, c0.y)), t.x);
    let bottom = mix(chroma(vec2(c0.x, c1.y)), chroma(c1), t.x);
    let uv = (mix(top, bottom, t.y) - p.c_off) * p.c_scale;
    let r = yv + 2.0 * (1.0 - p.kr) * uv.y;
    let b = yv + 2.0 * (1.0 - p.kb) * uv.x;
    let g = (yv - p.kr * r - p.kb * b) / (1.0 - p.kr - p.kb);
    let rgb = clamp(vec3(r, g, b), vec3(0.0), vec3(1.0));
    return vec4(linear_from_srgb(rgb), 1.0);
}
