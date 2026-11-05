// A solid's outline flat on its own plane (its X and Y; Z dropped), into
// its tile of the shadow atlas: the letters cast shadows like a 3D layer's
// picture does (see `occlusion` in composite.wgsl).

struct Flat {
    // The outline's extent: min X, min Y, width, height.
    bounds: vec4<f32>,
};

@group(0) @binding(0) var<uniform> f: Flat;

@vertex
fn vs_flat(@location(0) p: vec3<f32>) -> @builtin(position) vec4<f32> {
    let uv = (p.xy - f.bounds.xy) / max(f.bounds.zw, vec2(1e-3));
    // The tile's top row is the outline's least Y (Y down, like a picture).
    return vec4(uv.x * 2.0 - 1.0, 1.0 - uv.y * 2.0, 0.0, 1.0);
}

@fragment
fn fs_flat() -> @location(0) vec4<f32> {
    return vec4(1.0, 0.0, 0.0, 1.0);
}
