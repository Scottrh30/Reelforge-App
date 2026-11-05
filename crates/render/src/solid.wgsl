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

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) world: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) part: f32,
};

@vertex
fn vs_solid(@location(0) p: vec3<f32>, @location(1) n: vec3<f32>, @location(2) part: f32) -> VsOut {
    var o: VsOut;
    o.pos = u.mvp * vec4(p, 1.0);
    o.world = u.axes[3].xyz + u.axes[0].xyz * p.x + u.axes[1].xyz * p.y + u.axes[2].xyz * p.z;
    o.normal = u.normals[0].xyz * n.x + u.normals[1].xyz * n.y + u.normals[2].xyz * n.z;
    o.part = part;
    return o;
}

// The scene's lights on world point `p` facing `n` (same maths as
// `reelforge_core::space3d::Light::reach` and the 3D layers in composite.wgsl).
fn scene_light(p: vec3<f32>, n: vec3<f32>) -> vec3<f32> {
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
        sum = sum + color * max(dot(n, l), 0.0) * atten * spot;
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
    }
    var light = vec3(0.0);
    if (u.camera.w > 0.5) {
        light = scene_light(in.world, n);
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
