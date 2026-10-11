// Layer compositing in linear light.
//
// Layer textures are uploaded as Rgba8UnormSrgb, so sampling returns linear
// values. Layers are blended (premultiplied alpha, fixed-function blend state
// per mode) into an Rgba16Float accumulation target, and a final pass encodes
// the result back to sRGB for display/export.

struct Layer {
    // Layer-local pixels (origin at layer centre) -> clip space.
    mvp: mat4x4<f32>,
    size: vec2<f32>,
    opacity: f32,
    // 0: no wipe; 1: a straight edge; 2: a circle from the middle (iris).
    wipe_mode: f32,
    // Straight edge: q = a·x/w + b·y/h + c; and the progress 0–1.
    wipe: vec4<f32>,
    // Softness (0–1 of the frame), frame width, frame height.
    wipe2: vec4<f32>,
    // The part of the layer shown after cropping, as UVs (u0, v0, u1, v1).
    crop: vec4<f32>,
};

@group(0) @binding(0) var<uniform> layer: Layer;
@group(1) @binding(0) var layer_tex: texture_2d<f32>;
@group(1) @binding(1) var layer_sampler: sampler;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_layer(@builtin(vertex_index) i: u32) -> VsOut {
    // Two triangles covering the unit quad.
    var corners = array<vec2<f32>, 6>(
        vec2(0.0, 0.0), vec2(1.0, 0.0), vec2(1.0, 1.0),
        vec2(0.0, 0.0), vec2(1.0, 1.0), vec2(0.0, 1.0),
    );
    // Only the cropped part of the layer is drawn: a smaller quad, sampling that part.
    let uv = mix(layer.crop.xy, layer.crop.zw, corners[i]);
    let local = (uv - vec2(0.5)) * layer.size;
    var out: VsOut;
    out.pos = layer.mvp * vec4(local, 0.0, 1.0);
    out.uv = uv;
    return out;
}

// How much of the layer a wipe transition shows at this frame pixel.
fn wipe_factor(p: vec2<f32>) -> f32 {
    if (layer.wipe_mode < 0.5) {
        return 1.0;
    }
    let frame = layer.wipe2.yz;
    var q: f32;
    if (layer.wipe_mode < 1.5) {
        q = layer.wipe.x * p.x / frame.x + layer.wipe.y * p.y / frame.y + layer.wipe.z;
    } else {
        q = length(p - frame * 0.5) / (0.5 * length(frame));
    }
    let soft = max(layer.wipe2.x, 1e-4);
    // The edge sweeps from just before 0 to just past 1, so the ends are clean.
    let e = layer.wipe.w * (1.0 + soft);
    return clamp((e - q) / soft, 0.0, 1.0);
}

@fragment
fn fs_layer(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(layer_tex, layer_sampler, in.uv);
    let a = c.a * layer.opacity * wipe_factor(in.pos.xy);
    return vec4(c.rgb * a, a); // premultiplied
}

// For a pre-composited, premultiplied group (a dissolve rendered into its own
// float target): no extra premultiply, just the group's overall opacity.
@fragment
fn fs_layer_premul(in: VsOut) -> @location(0) vec4<f32> {
    return textureSample(layer_tex, layer_sampler, in.uv) * layer.opacity * wipe_factor(in.pos.xy);
}
