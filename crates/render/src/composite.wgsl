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
    // A mesh warp: cells across, cells down, 1 when on (else 0), subdivisions per cell.
    mesh: vec4<f32>,
    // The grid's points in frame pixels, row by row, two to a vec4. For a lit
    // 3D layer (wipe2.w = 1) instead: [0] the plane's origin and light count,
    // [1] its X axis, [2] its Y axis (world per layer pixel), [3] the camera's
    // position, then four per light: position + type, direction + falloff,
    // colour + falloff distance, cone cosines (outer, inner).
    pts: array<vec4<f32>, 41>,
};

@group(0) @binding(0) var<uniform> layer: Layer;
@group(1) @binding(0) var layer_tex: texture_2d<f32>;
@group(1) @binding(1) var layer_sampler: sampler;

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
    // Layer pixels from its centre (for lighting).
    @location(1) local: vec2<f32>,
};

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
        out.uv = mix(layer.crop.xy, layer.crop.zw, g);
        out.local = vec2(0.0);
        return out;
    }
    // Only the cropped part of the layer is drawn: a smaller quad, sampling that part.
    let uv = mix(layer.crop.xy, layer.crop.zw, corners[i]);
    let local = (uv - vec2(0.5)) * layer.size;
    var out: VsOut;
    out.pos = layer.mvp * vec4(local, 0.0, 1.0);
    out.uv = uv;
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

// The light reaching a lit 3D layer's pixel at `local` (same maths as
// `reelforge_core::space3d::Light::reach`).
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
        sum = sum + color * max(dot(n, l), 0.0) * atten * spot;
    }
    return sum;
}

@fragment
fn fs_layer(in: VsOut) -> @location(0) vec4<f32> {
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
    var c = textureSample(layer_tex, layer_sampler, in.uv);
    // A lit 3D layer with effects is drawn from its (premultiplied) effect result.
    if (layer.wipe2.w > 0.5) {
        c = vec4(c.rgb * lighting(in.local), c.a);
    }
    return c * layer.opacity * wipe_factor(in.pos.xy);
}
