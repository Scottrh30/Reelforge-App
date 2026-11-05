// 3D text and other solid objects: a lit triangle mesh drawn with a depth
// buffer into a layer texture (see solid.rs).

struct Solid {
    mvp: mat4x4<f32>,
    // Object → world: X, Y and Z axes, then the origin.
    axes: array<vec4<f32>, 4>,
    // Object normals → world normals.
    normals: array<vec4<f32>, 3>,
    // The camera's position, and the number of lights in w.
    camera: vec4<f32>,
    // Towards the studio light (used without lights), and its ambient part in w.
    studio: vec4<f32>,
    // Linear colours of the front/back faces and of the sides; a = opacity.
    face: vec4<f32>,
    side: vec4<f32>,
    // Four per light: position + type, direction + falloff, colour + radius, cone.
    lights: array<vec4<f32>, 32>,
};

@group(0) @binding(0) var<uniform> u: Solid;

// Shadows other layers (and the text itself) cast on it: the shadow casters,
// six vec4s each as in composite.wgsl (centre + half width, X axis + half
// height, Y axis + opacity, crop, effect map, atlas tile), and their outlines.
const MAX_OCC = 25u;
struct Shadows {
    // How many casters in x.
    head: vec4<f32>,
    occ: array<vec4<f32>, 150>,
};
@group(1) @binding(0) var<uniform> sh: Shadows;
@group(1) @binding(1) var shadow_atlas: texture_2d<f32>;
@group(1) @binding(2) var shadow_sampler: sampler;

// How much of caster `j` covers world point `q` on its plane (see composite.wgsl).
fn occluder_alpha(j: u32, q: vec3<f32>) -> f32 {
    let b = j * 6u;
    let o = sh.occ[b].xyz;
    let hw = sh.occ[b].w;
    let u = sh.occ[b + 1u].xyz;
    let hh = sh.occ[b + 1u].w;
    let v = sh.occ[b + 2u].xyz;
    let crop = sh.occ[b + 3u];
    let fx = sh.occ[b + 4u];
    let tile = sh.occ[b + 5u];
    let d = q - o;
    let local = vec2(dot(d, u) / dot(u, u), dot(d, v) / dot(v, v));
    let uv = local / vec2(2.0 * hw, 2.0 * hh) + 0.5;
    if (any(uv < crop.xy) || any(uv > crop.zw)) {
        return 0.0;
    }
    let tex = (uv - fx.zw) / fx.xy;
    if (any(tex < vec2(0.0)) || any(tex > vec2(1.0))) {
        return 0.0;
    }
    let texel = 0.5 / f32(textureDimensions(shadow_atlas).x);
    let at = tile.xy + clamp(tex * tile.z, vec2(texel), vec2(tile.z - texel));
    return textureSampleLevel(shadow_atlas, shadow_sampler, at, 0.0).r;
}

const SOFT_TAPS = 24u;

// How much the casters block the light's way from `p` (see composite.wgsl).
fn occlusion(p: vec3<f32>, l: vec3<f32>, dist: f32, directional: bool, soft: f32, grain: f32) -> f32 {
    let count = min(u32(sh.head.x), MAX_OCC);
    var through = 1.0;
    for (var j = 0u; j < count; j = j + 1u) {
        let b = j * 6u;
        let o = sh.occ[b].xyz;
        let u = sh.occ[b + 1u].xyz;
        let v = sh.occ[b + 2u].xyz;
        let opacity = sh.occ[b + 2u].w;
        let n = cross(u, v);
        let den = dot(n, l);
        if (abs(den) < 1e-9) {
            continue;
        }
        let t = dot(n, o - p) / den;
        if (t <= 0.5 || (!directional && t >= dist - 0.5)) {
            continue;
        }
        let hit = p + l * t;
        var a = 0.0;
        let r = t * tan(soft);
        if (soft <= 0.0 || r < 0.5) {
            a = occluder_alpha(j, hit);
        } else {
            let eu = normalize(u);
            let ev = normalize(v - eu * dot(v, eu));
            for (var k = 0u; k < SOFT_TAPS; k = k + 1u) {
                let f = (f32(k) + 0.5) / f32(SOFT_TAPS);
                let ang = f32(k) * 2.39996323 + grain * 6.2831853;
                let off = sqrt(f) * r * vec2(cos(ang), sin(ang));
                a = a + occluder_alpha(j, hit + eu * off.x + ev * off.y);
            }
            a = a / f32(SOFT_TAPS);
        }
        through = through * (1.0 - a * opacity);
    }
    return 1.0 - through;
}

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) world: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) part: f32,
    @location(3) color: vec4<f32>,
};

@vertex
fn vs_solid(@location(0) p: vec3<f32>, @location(1) n: vec3<f32>, @location(2) part: f32, @location(3) color: vec4<f32>) -> VsOut {
    var o: VsOut;
    o.pos = u.mvp * vec4(p, 1.0);
    o.world = u.axes[3].xyz + u.axes[0].xyz * p.x + u.axes[1].xyz * p.y + u.axes[2].xyz * p.z;
    o.normal = u.normals[0].xyz * n.x + u.normals[1].xyz * n.y + u.normals[2].xyz * n.z;
    o.part = part;
    o.color = color;
    return o;
}

// The scene's lights on world point `p` facing `n` (same maths as
// `reelforge_core::space3d::Light::reach` and the 3D layers in composite.wgsl).
fn scene_light(p: vec3<f32>, n: vec3<f32>, grain: f32) -> vec3<f32> {
    var sum = vec3(0.0);
    let count = u32(u.camera.w);
    for (var i = 0u; i < count; i = i + 1u) {
        let b = i * 4u;
        let pos = u.lights[b].xyz;
        let kind = u.lights[b].w;
        let dir = u.lights[b + 1u].xyz;
        let falloff = u.lights[b + 1u].w;
        let color = u.lights[b + 2u].xyz;
        let radius = u.lights[b + 2u].w;
        let cone = u.lights[b + 3u].xy;
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
        let darkness = u.lights[b + 3u].z;
        if (darkness > 0.0 && sh.head.x > 0.5) {
            shade = 1.0 - darkness * occlusion(p, l, dist, kind > 1.5, u.lights[b + 3u].w, grain);
        }
        sum = sum + color * max(dot(n, l), 0.0) * atten * spot * shade;
    }
    return sum;
}

@fragment
fn fs_solid(in: VsOut) -> @location(0) vec4<f32> {
    var n = normalize(in.normal);
    // Seen from behind (an open edge, or a face turned away): light that side.
    if (dot(n, u.camera.xyz - in.world) < 0.0) {
        n = -n;
    }
    var c = u.face;
    if (in.part > 0.5 && in.part < 1.5) {
        c = u.side;
    } else if (in.part > 2.5) {
        // A model's own colour, faded by the layer's text colour alpha.
        c = vec4(in.color.rgb, in.color.a * u.face.a);
    }
    var light = vec3(0.0);
    if (u.camera.w > 0.5) {
        // A fixed scramble of where this is, for the soft shadows' taps.
        let grain = fract(sin(dot(in.world.xy + in.world.zz * 0.37, vec2(12.9898, 78.233))) * 43758.5453);
        light = scene_light(in.world, n, grain);
    } else {
        let a = u.studio.w;
        light = vec3(min(a + (1.0 - a) * max(dot(n, u.studio.xyz), 0.0), 1.0));
    }
    return vec4(c.rgb * light, c.a);
}

// The depth picture (R32Float): how far each pixel's nearest face is, in the
// same 0–1 depth as 3D layers, so the compositor can cut them through each other.
@fragment
fn fs_depth(in: VsOut) -> @location(0) vec4<f32> {
    return vec4(in.pos.z, 0.0, 0.0, 1.0);
}
