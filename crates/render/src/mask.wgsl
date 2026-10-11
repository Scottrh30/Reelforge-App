// The mask pass: multiplies a layer (premultiplied, in its effect scratch
// target) by its mask's alpha. Runs first in the layer's effect chain, so
// effects see the masked layer, as in HitFilm.

@group(1) @binding(0) var src: texture_2d<f32>;
@group(1) @binding(1) var src_sampler: sampler;
@group(2) @binding(0) var mask_tex: texture_2d<f32>;
@group(2) @binding(1) var mask_sampler: sampler;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn vs_fullscreen(@builtin(vertex_index) i: u32) -> VsOut {
    let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    var out: VsOut;
    out.pos = vec4(p * 2.0 - 1.0, 0.0, 1.0);
    out.uv = vec2(p.x, 1.0 - p.y);
    return out;
}

@fragment
fn fs_mask(in: VsOut) -> @location(0) vec4<f32> {
    let c = textureSample(src, src_sampler, in.uv);
    let m = textureSample(mask_tex, mask_sampler, in.uv).r;
    return c * m;
}
