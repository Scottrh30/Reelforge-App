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

// For a picture that will be composited again (a composite shot): keep the
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
