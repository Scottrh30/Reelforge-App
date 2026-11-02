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
    // A processed layer's texture can have a margin (room for a glow or
    // shadow): texture uv = (layer uv − zw) / xy.
    fxmap: vec4<f32>,
    // How far the drawn quad reaches past the crop, in layer uv: left, top, right, bottom.
    grow: vec4<f32>,
    // A mesh warp: cells across, cells down, 1 when on (else 0), subdivisions per cell.
    mesh: vec4<f32>,
    // The grid's points in frame pixels, row by row, two to a vec4. For a lit
    // 3D layer (wipe2.w = 1) instead: [0] the plane's origin and light count,
    // [1] its X axis, [2] its Y axis (world per layer pixel), [3] the camera's
    // position, then four per light: position + type, direction + falloff,
    // colour + falloff distance, cone cosines (outer, inner).
    pts: array<vec4<f32>, 545>,
};

@group(0) @binding(0) var<uniform> layer: Layer;
// 3D layers' outlines (alpha), a tile each, for the shadows they cast.
@group(0) @binding(1) var shadow_atlas: texture_2d<f32>;
@group(1) @binding(0) var layer_tex: texture_2d<f32>;
@group(1) @binding(1) var layer_sampler: sampler;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    // Where to sample the layer's texture.
    @location(0) uv: vec2<f32>,
    // Layer pixels from its centre (for lighting).
    @location(1) local: vec2<f32>,
};

fn tex_uv(layer_uv: vec2<f32>) -> vec2<f32> {
    return (layer_uv - layer.fxmap.zw) / layer.fxmap.xy;
}

fn mesh_point(n: u32) -> vec2<f32> {
    let v = layer.pts[n / 2u];
    if (n % 2u == 0u) {
        return v.xy;
    }
    return v.zw;
}

@vertex
fn vs_layer(@builtin(vertex_index) i: u32) -> VsOut {
    // Two triangles covering the unit quad.
    var corners = array<vec2<f32>, 6>(
        vec2(0.0, 0.0), vec2(1.0, 0.0), vec2(1.0, 1.0),
        vec2(0.0, 0.0), vec2(1.0, 1.0), vec2(0.0, 1.0),
    );
    if (layer.mesh.z > 0.5) {
        // A mesh warp: every cell is cut into s × s little quads, each drawn as
        // two triangles, with corners placed bilinearly inside the cell.
        let cols = u32(layer.mesh.x);
        let s = u32(layer.mesh.w);
        let tri = i / 6u;
        let corner = corners[i % 6u];
        let sub = tri % (s * s);
        let cell = tri / (s * s);
        let cx = cell % cols;
        let cy = cell / cols;
        let f = (vec2(f32(sub % s), f32(sub / s)) + corner) / f32(s);
        let w = cols + 1u;
        let p00 = mesh_point(cy * w + cx);
        let p10 = mesh_point(cy * w + cx + 1u);
        let p01 = mesh_point((cy + 1u) * w + cx);
        let p11 = mesh_point((cy + 1u) * w + cx + 1u);
        let p = mix(mix(p00, p10, f.x), mix(p01, p11, f.x), f.y);
        let frame = layer.wipe2.yz;
        let g = (vec2(f32(cx), f32(cy)) + f) / vec2(layer.mesh.x, layer.mesh.y);
        var out: VsOut;
        out.pos = vec4(p.x / frame.x * 2.0 - 1.0, 1.0 - p.y / frame.y * 2.0, 0.0, 1.0);
        out.uv = tex_uv(mix(layer.crop.xy, layer.crop.zw, g));
        out.local = vec2(0.0);
        return out;
    }
    // Only the cropped part of the layer is drawn: a smaller quad, sampling
    // that part (and the margin round it where a glow or shadow spills out).
    let uv = mix(layer.crop.xy - layer.grow.xy, layer.crop.zw + layer.grow.zw, corners[i]);
    let local = (uv - vec2(0.5)) * layer.size;
    var out: VsOut;
    out.pos = layer.mvp * vec4(local, 0.0, 1.0);
    out.uv = tex_uv(uv);
    out.local = local;
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

// Where shadow casters start in `pts` (after the plane, camera and 8 lights),
// six vec4s each: centre + half width, X axis + half height, Y axis +
// opacity, the crop (uv), the effect target's map, and its atlas tile.
const OCCLUDER_BASE = 36u;

// How much of a light's way from world point `p` (direction `l`, `dist` away;
// any distance for a directional light) the other 3D layers block, 0–1.
fn occlusion(p: vec3<f32>, l: vec3<f32>, dist: f32, directional: bool) -> f32 {
    let count = u32(layer.pts[3].w);
    var through = 1.0;
    for (var j = 0u; j < count; j = j + 1u) {
        let b = OCCLUDER_BASE + j * 6u;
        let o = layer.pts[b].xyz;
        let hw = layer.pts[b].w;
        let u = layer.pts[b + 1u].xyz;
        let hh = layer.pts[b + 1u].w;
        let v = layer.pts[b + 2u].xyz;
        let opacity = layer.pts[b + 2u].w;
        let crop = layer.pts[b + 3u];
        let fx = layer.pts[b + 4u];
        let tile = layer.pts[b + 5u];
        let n = cross(u, v);
        let den = dot(n, l);
        if (abs(den) < 1e-9) {
            continue;
        }
        let t = dot(n, o - p) / den;
        // In front of the point (not its own plane), and before the light.
        if (t <= 0.5 || (!directional && t >= dist - 0.5)) {
            continue;
        }
        let d = p + l * t - o;
        let local = vec2(dot(d, u) / dot(u, u), dot(d, v) / dot(v, v));
        let uv = local / vec2(2.0 * hw, 2.0 * hh) + 0.5;
        if (any(uv < crop.xy) || any(uv > crop.zw)) {
            continue;
        }
        let tex = (uv - fx.zw) / fx.xy;
        if (any(tex < vec2(0.0)) || any(tex > vec2(1.0))) {
            continue;
        }
        // Inside the tile (half a texel in), from the layer's picture.
        let texel = 0.5 / 1024.0;
        let at = tile.xy + clamp(tex * tile.z, vec2(texel), vec2(tile.z - texel));
        let a = textureSampleLevel(shadow_atlas, layer_sampler, at, 0.0).r * opacity;
        through = through * (1.0 - a);
    }
    return 1.0 - through;
}

// The light reaching a lit 3D layer's pixel at `local` (same maths as
// `reelforge_core::space3d::Light::reach`), less the shadows other 3D layers cast.
fn lighting(local: vec2<f32>) -> vec3<f32> {
    let origin = layer.pts[0].xyz;
    let u = layer.pts[1].xyz;
    let v = layer.pts[2].xyz;
    let p = origin + u * local.x + v * local.y;
    var n = normalize(cross(u, v));
    // Lit on whichever side faces the camera.
    if (dot(n, layer.pts[3].xyz - p) < 0.0) {
        n = -n;
    }
    var sum = vec3(0.0);
    let count = u32(layer.pts[0].w);
    for (var i = 0u; i < count; i = i + 1u) {
        let b = 4u + i * 4u;
        let pos = layer.pts[b].xyz;
        let kind = layer.pts[b].w;
        let dir = layer.pts[b + 1u].xyz;
        let falloff = layer.pts[b + 1u].w;
        let color = layer.pts[b + 2u].xyz;
        let radius = layer.pts[b + 2u].w;
        let cone = layer.pts[b + 3u].xy;
        if (kind > 2.5) {
            sum = sum + color; // ambient
            continue;
        }
        var l = -dir;
        var dist = 0.0;
        if (kind < 1.5) {
            let d = pos - p;
            dist = max(length(d), 1e-3);
            l = d / dist;
        }
        var atten = 1.0;
        if (kind < 1.5 && falloff > 0.5 && falloff < 1.5) {
            let k = clamp(1.0 - dist / radius, 0.0, 1.0);
            atten = k * k;
        } else if (kind < 1.5 && falloff > 1.5) {
            atten = radius * radius / (radius * radius + dist * dist) * 2.0;
        }
        var spot = 1.0;
        if (kind > 0.5 && kind < 1.5) {
            spot = smoothstep(cone.x, max(cone.y, cone.x + 1e-4), dot(-l, dir));
        }
        var shade = 1.0;
        let darkness = layer.pts[b + 3u].z;
        if (darkness > 0.0) {
            shade = 1.0 - darkness * occlusion(p, l, dist, kind > 1.5);
        }
        sum = sum + color * max(dot(n, l), 0.0) * atten * spot * shade;
    }
    return sum;
}

// In a run of 3D layers (depth-tested): pixels fainter than this are left
// out, so they don't hide what's behind them. 0 elsewhere.
override DEPTH_CUT: f32 = 0.0;

@fragment
fn fs_layer(in: VsOut) -> @location(0) vec4<f32> {
    let c = layer_color(in);
    if (c.a < DEPTH_CUT) {
        discard;
    }
    return c;
}

fn layer_color(in: VsOut) -> vec4<f32> {
    let c = textureSample(layer_tex, layer_sampler, in.uv);
    let a = c.a * layer.opacity * wipe_factor(in.pos.xy);
    var rgb = c.rgb;
    if (layer.wipe2.w > 0.5) {
        rgb = rgb * lighting(in.local);
    }
    return vec4(rgb * a, a); // premultiplied
}

// For a pre-composited, premultiplied group (a dissolve rendered into its own
// float target): no extra premultiply, just the group's overall opacity.
@fragment
fn fs_layer_premul(in: VsOut) -> @location(0) vec4<f32> {
    let c = layer_color_premul(in);
    if (c.a < DEPTH_CUT) {
        discard;
    }
    return c;
}

fn layer_color_premul(in: VsOut) -> vec4<f32> {
    var c = textureSample(layer_tex, layer_sampler, in.uv);
    // A lit 3D layer with effects is drawn from its (premultiplied) effect result.
    if (layer.wipe2.w > 0.5) {
        c = vec4(c.rgb * lighting(in.local), c.a);
    }
    return c * layer.opacity * wipe_factor(in.pos.xy);
}

// ---- Blend modes that read the frame under the layer ------------------------
// The frame so far is copied to `backdrop` first; these entry points draw the
// layer with "normal" GPU blending, having already mixed in the blend result,
// following the W3C compositing spec:
//   colour = (1 − αb)·Cs + αb·B(Cb, Cs), then drawn over the frame with αs.
// B works on sRGB-encoded values (as Photoshop and HitFilm do); the reference
// is BlendMode::blend_rgb in the core crate.

@group(2) @binding(0) var backdrop: texture_2d<f32>;
// Which BlendMode (its index in BlendMode::ALL), set per pipeline.
override MODE: u32 = 4u;

fn enc(c: vec3<f32>) -> vec3<f32> {
    let v = clamp(c, vec3(0.0), vec3(1.0));
    return select(1.055 * pow(v, vec3(1.0 / 2.4)) - 0.055, v * 12.92, v <= vec3(0.0031308));
}

fn dec(c: vec3<f32>) -> vec3<f32> {
    let v = clamp(c, vec3(0.0), vec3(1.0));
    return select(pow((v + 0.055) / 1.055, vec3(2.4)), v / 12.92, v <= vec3(0.04045));
}

fn hard1(b: f32, s: f32) -> f32 {
    if (s <= 0.5) {
        return b * 2.0 * s;
    }
    let s2 = 2.0 * s - 1.0;
    return b + s2 - b * s2;
}

fn soft1(b: f32, s: f32) -> f32 {
    if (s <= 0.5) {
        return b - (1.0 - 2.0 * s) * b * (1.0 - b);
    }
    var d = sqrt(b);
    if (b <= 0.25) {
        d = ((16.0 * b - 12.0) * b + 4.0) * b;
    }
    return b + (2.0 * s - 1.0) * (d - b);
}

fn dodge1(b: f32, s: f32) -> f32 {
    if (b <= 0.0) {
        return 0.0;
    }
    if (s >= 1.0) {
        return 1.0;
    }
    return min(b / (1.0 - s), 1.0);
}

fn burn1(b: f32, s: f32) -> f32 {
    if (b >= 1.0) {
        return 1.0;
    }
    if (s <= 0.0) {
        return 0.0;
    }
    return 1.0 - min((1.0 - b) / s, 1.0);
}

fn lum(c: vec3<f32>) -> f32 {
    return dot(c, vec3(0.3, 0.59, 0.11));
}

fn set_lum(c0: vec3<f32>, l0: f32) -> vec3<f32> {
    var c = c0 + (l0 - lum(c0));
    let l = lum(c);
    let n = min(min(c.r, c.g), c.b);
    let x = max(max(c.r, c.g), c.b);
    if (n < 0.0) {
        c = l + (c - l) * l / (l - n);
    }
    if (x > 1.0) {
        c = l + (c - l) * (1.0 - l) / (x - l);
    }
    return c;
}

fn sat(c: vec3<f32>) -> f32 {
    return max(max(c.r, c.g), c.b) - min(min(c.r, c.g), c.b);
}

fn set_sat(c: vec3<f32>, s: f32) -> vec3<f32> {
    let n = min(min(c.r, c.g), c.b);
    let x = max(max(c.r, c.g), c.b);
    if (x <= n) {
        return vec3(0.0);
    }
    return (c - n) * s / (x - n);
}

fn blend_fn(b: vec3<f32>, s: vec3<f32>) -> vec3<f32> {
    switch MODE {
        case 4u: { return vec3(hard1(s.r, b.r), hard1(s.g, b.g), hard1(s.b, b.b)); }
        case 5u: { return vec3(soft1(b.r, s.r), soft1(b.g, s.g), soft1(b.b, s.b)); }
        case 6u: { return vec3(hard1(b.r, s.r), hard1(b.g, s.g), hard1(b.b, s.b)); }
        case 7u: { return vec3(dodge1(b.r, s.r), dodge1(b.g, s.g), dodge1(b.b, s.b)); }
        case 8u: { return vec3(burn1(b.r, s.r), burn1(b.g, s.g), burn1(b.b, s.b)); }
        case 9u: { return min(b, s); }
        case 10u: { return max(b, s); }
        case 11u: { return abs(b - s); }
        case 12u: { return b + s - 2.0 * b * s; }
        case 13u: { return set_lum(set_sat(s, sat(b)), lum(b)); }
        case 14u: { return set_lum(set_sat(b, sat(s)), lum(b)); }
        case 15u: { return set_lum(s, lum(b)); }
        case 16u: { return set_lum(b, lum(s)); }
        default: { return s; }
    }
}

// `src`: the layer's premultiplied colour here (before blending).
fn blend_with_backdrop(src: vec4<f32>, pos: vec2<f32>) -> vec4<f32> {
    let a = src.a;
    if (a <= 1e-6) {
        return vec4(0.0);
    }
    let under = textureLoad(backdrop, vec2<i32>(pos), 0);
    let ab = clamp(under.a, 0.0, 1.0);
    let cs = enc(src.rgb / a);
    let cb = enc(select(vec3(0.0), under.rgb / under.a, under.a > 1e-6));
    let mixed = dec(mix(cs, blend_fn(cb, cs), ab));
    return vec4(mixed * a, a);
}

@fragment
fn fs_blend(in: VsOut) -> @location(0) vec4<f32> {
    return blend_with_backdrop(layer_color(in), in.pos.xy);
}

@fragment
fn fs_blend_premul(in: VsOut) -> @location(0) vec4<f32> {
    return blend_with_backdrop(layer_color_premul(in), in.pos.xy);
}
