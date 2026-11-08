// Track mattes: a layer drawn alone (group 0) kept where its matte, the
// layer above drawn alone (group 1), says. Both premultiplied linear light.

@group(0) @binding(0) var layer: texture_2d<f32>;
@group(1) @binding(0) var matte: texture_2d<f32>;

@vertex
fn vs_fullscreen(@builtin(vertex_index) i: u32) -> @builtin(position) vec4<f32> {
    let p = vec2(f32((i << 1u) & 2u), f32(i & 2u));
    return vec4(p * 2.0 - 1.0, 0.0, 1.0);
}

fn luma(c: vec4<f32>) -> f32 {
    // As it looks: straight colour, display values, then times its coverage.
    let a = clamp(c.a, 0.0, 1.0);
    if (a <= 0.0) {
        return 0.0;
    }
    let s = pow(clamp(c.rgb / a, vec3(0.0), vec3(1.0)), vec3(1.0 / 2.2));
    return dot(s, vec3(0.2126, 0.7152, 0.0722)) * a;
}

fn amount(p: vec4<f32>, kind: u32) -> f32 {
    let m = textureLoad(matte, vec2<i32>(p.xy), 0);
    switch kind {
        case 1u: { return 1.0 - clamp(m.a, 0.0, 1.0); }
        case 2u: { return luma(m); }
        case 3u: { return 1.0 - luma(m); }
        default: { return clamp(m.a, 0.0, 1.0); }
    }
}

@fragment
fn fs_matte_alpha(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {
    return textureLoad(layer, vec2<i32>(p.xy), 0) * amount(p, 0u);
}
@fragment
fn fs_matte_alpha_inv(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {
    return textureLoad(layer, vec2<i32>(p.xy), 0) * amount(p, 1u);
}
@fragment
fn fs_matte_luma(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {
    return textureLoad(layer, vec2<i32>(p.xy), 0) * amount(p, 2u);
}
@fragment
fn fs_matte_luma_inv(@builtin(position) p: vec4<f32>) -> @location(0) vec4<f32> {
    return textureLoad(layer, vec2<i32>(p.xy), 0) * amount(p, 3u);
}
