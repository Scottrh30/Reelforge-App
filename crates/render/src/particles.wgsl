// Particles, drawn as instanced quads over a layer's effect target
// (premultiplied, linear light). Each instance is one particle's birth state
// (core crate: particles::Particle); its age moves it along its path here.

struct Fx {
    p0: vec4<f32>,        // emitter x, y, shape, size
    p1: vec4<f32>,        // rate, lifetime, randomness, speed
    texel_dir: vec4<f32>, // texel size (uv), unused
    extra: vec4<f32>,     // aspect, time, layer size (frame pixels)
    p2: vec4<f32>,        // direction, spread, gravity, wind
    p3: vec4<f32>,        // turbulence, size at birth, size at death, colour at birth
    extra2: vec4<f32>,
    p4: vec4<f32>,        // colour at death, opacity, look, seed
};

@group(0) @binding(0) var<uniform> fx: Fx;

struct Inst {
    @location(0) emit: vec4<f32>,   // emitter % (xy), offset px (zw)
    @location(1) motion: vec4<f32>, // velocity px/s (xy), age, life
    @location(2) more: vec4<f32>,   // size multiplier, seed
};

struct VsOut {
    @builtin(position) pos: vec4<f32>,
    // The quad's corner, -1..1 (x along the motion for sparks).
    @location(0) corner: vec2<f32>,
    @location(1) color: vec4<f32>,
    // Radius in target pixels, for the edges of solid dots; the look.
    @location(2) info: vec2<f32>,
};

fn to_linear(c: vec3<f32>) -> vec3<f32> {
    return select(pow((c + 0.055) / 1.055, vec3(2.4)), c / 12.92, c <= vec3(0.04045));
}

fn rgb_of(v: f32) -> vec3<f32> {
    let c = u32(clamp(round(v), 0.0, 16777215.0));
    return to_linear(vec3(f32((c >> 16u) & 255u), f32((c >> 8u) & 255u), f32(c & 255u)) / 255.0);
}

// Smooth wandering, -1..1: two sines at frequencies of the particle's own.
fn wander(seed: f32, t: f32) -> f32 {
    return sin(t * (0.9 + seed * 0.8) + seed * 61.0) * 0.6 + sin(t * (2.1 + seed * 1.3) + seed * 17.0 + 1.7) * 0.4;
}

@vertex
fn vs_particle(@builtin(vertex_index) vi: u32, p: Inst) -> VsOut {
    var out: VsOut;
    let size = fx.extra.zw;
    let age = p.motion.z;
    let life = max(p.motion.w, 1e-4);
    let f = clamp(age / life, 0.0, 1.0);
    let seed = p.more.y;
    let accel = vec2(fx.p2.w, fx.p2.z);

    // Thrown, then pulled by gravity and wind, and pushed about by turbulence
    // (which builds up over the first half second).
    var pos = p.emit.xy / 100.0 * size + p.emit.zw + p.motion.xy * age + 0.5 * accel * age * age;
    let turb = fx.p3.x * min(age / 0.5, 1.0);
    pos += turb * vec2(wander(seed, age), wander(seed + 0.37, age + 11.0));
    let vel = p.motion.xy + accel * age;

    let look = u32(round(fx.p4.z));
    var radius = mix(fx.p3.y, fx.p3.z, f) * 0.5 * p.more.x;
    // Fades in quickly and out over the last third of its life.
    var alpha = fx.p4.y / 100.0 * clamp(f / 0.06, 0.0, 1.0) * clamp((1.0 - f) / 0.35, 0.0, 1.0);
    // Target pixels per frame pixel.
    let k = vec2(1.0) / (fx.texel_dir.xy * size);
    let kk = max(k.x, k.y);
    // Very small particles: a pixel across, fainter (so they don't flicker).
    let rp = radius * kk;
    if (rp < 0.75) {
        alpha *= (rp / 0.75) * (rp / 0.75);
        radius = 0.75 / kk;
    }
    // Glow softens a little outside its size.
    if (look == 0u) {
        radius *= 1.6;
    }

    let corners = array<vec2<f32>, 6>(vec2(-1.0, -1.0), vec2(1.0, -1.0), vec2(1.0, 1.0), vec2(-1.0, -1.0), vec2(1.0, 1.0), vec2(-1.0, 1.0));
    let c = corners[vi];
    var axis_x = vec2(radius, 0.0);
    var axis_y = vec2(0.0, radius);
    if (look == 1u) {
        // A spark: a streak along its motion, as long as 1/25 s of travel.
        let speed = length(vel);
        let dir = select(vec2(1.0, 0.0), vel / speed, speed > 1e-3);
        let half_len = radius + speed * 0.02;
        axis_x = dir * half_len;
        axis_y = vec2(-dir.y, dir.x) * radius;
        out.info = vec2(radius * kk, 1.0 - radius / half_len);
    } else {
        out.info = vec2(radius * kk, 0.0);
    }
    let at = pos + c.x * axis_x + c.y * axis_y;
    let ndc = at / size * 2.0 - 1.0;
    out.pos = vec4(ndc.x, -ndc.y, 0.0, 1.0);
    // Dead or not yet born (it shouldn't be in the list, but just in case): nothing.
    if (age < 0.0 || age >= life) {
        out.pos = vec4(2.0, 2.0, 2.0, 1.0);
    }
    out.corner = c;
    let rgb = mix(rgb_of(fx.p3.w), rgb_of(fx.p4.x), f);
    out.color = vec4(rgb, alpha);
    return out;
}

@fragment
fn fs_particle(in: VsOut) -> @location(0) vec4<f32> {
    let look = u32(round(fx.p4.z));
    var cover: f32;
    if (look == 1u) {
        // Capsule: round ends, the straight part a fraction `info.y` of its half-length.
        let along = max(abs(in.corner.x) - in.info.y, 0.0) / max(1.0 - in.info.y, 1e-3);
        let d = length(vec2(along, in.corner.y));
        cover = pow(max(1.0 - d * d, 0.0), 1.5) * 1.4;
    } else {
        let d = length(in.corner);
        if (look == 0u) {
            // A hot middle and a soft halo.
            cover = pow(max(1.0 - d * d, 0.0), 3.0) * 1.3;
        } else if (look == 2u) {
            cover = 1.0 - smoothstep(0.35, 1.0, d);
        } else {
            // Hard edge, one target pixel of anti-aliasing.
            cover = clamp((1.0 - d) * in.info.x + 0.5, 0.0, 1.0);
        }
    }
    let a = in.color.a * cover;
    if (look <= 1u) {
        // Light: added (blend One + One); its alpha is how bright it is
        // (blended with Max) so it shows over a transparent layer too.
        let light = in.color.rgb * a;
        return vec4(light, clamp(max(max(light.r, light.g), light.b), 0.0, 1.0));
    }
    let ca = clamp(a, 0.0, 1.0);
    return vec4(in.color.rgb * ca, ca);
}
