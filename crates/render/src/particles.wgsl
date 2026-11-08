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
    map: vec4<f32>,       // layer uv = target uv × xy + zw (the target can have a margin)
};

@group(0) @binding(0) var<uniform> fx: Fx;
// The layer as it is before the particles (for particles born from its pixels).
@group(1) @binding(0) var src: texture_2d<f32>;
@group(1) @binding(1) var src_sampler: sampler;

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
    // The particle's own random number, and how far through its life (0–1).
    @location(3) life: vec2<f32>,
};

fn hash21(p: vec2<f32>) -> f32 {
    let q = fract(p * vec2(123.34, 456.21));
    let r = q + dot(q, q + 45.32);
    return fract(r.x * r.y);
}

// Value noise, 0..1.
fn vnoise(p: vec2<f32>) -> f32 {
    let i = floor(p);
    let f = p - i;
    let u = f * f * (3.0 - 2.0 * f);
    return mix(mix(hash21(i), hash21(i + vec2(1.0, 0.0)), u.x), mix(hash21(i + vec2(0.0, 1.0)), hash21(i + vec2(1.0, 1.0)), u.x), u.y);
}

// Three octaves: billowy detail for flame and smoke.
fn fbm(p: vec2<f32>) -> f32 {
    return vnoise(p) * 0.55 + vnoise(p * 2.1 + 7.3) * 0.3 + vnoise(p * 4.3 + 1.7) * 0.15;
}

fn rot(v: vec2<f32>, a: f32) -> vec2<f32> {
    let c = cos(a);
    let s = sin(a);
    return vec2(c * v.x - s * v.y, s * v.x + c * v.y);
}

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
    if (look == 4u) {
        // Flame: taller than wide, licking upwards.
        axis_y = vec2(0.0, radius * 1.8);
    }
    if (look == 5u || look == 7u) {
        // Smoke turns slowly as it rises; debris tumbles.
        let spin = select((seed - 0.5) * 0.8, (seed - 0.5) * 14.0, look == 7u);
        axis_x = rot(axis_x, seed * 6.28 + spin * age);
        axis_y = rot(axis_y, seed * 6.28 + spin * age);
    }
    if (look == 1u || look == 6u) {
        // A spark: a streak along its motion, as long as 1/25 s of travel
        // (a raindrop: thinner, as long as 1/30 s).
        let speed = length(vel);
        let dir = select(vec2(1.0, 0.0), vel / speed, speed > 1e-3);
        let half_len = radius + speed * select(0.02, 0.017, look == 6u);
        axis_x = dir * half_len;
        axis_y = vec2(-dir.y, dir.x) * radius;
        out.info = vec2(radius * kk, 1.0 - radius / half_len);
    } else {
        out.info = vec2(radius * kk, 0.0);
    }
    let at = pos + c.x * axis_x + c.y * axis_y;
    // Layer uv → the target (which may have a margin round the layer).
    let tuv = (at / size - fx.map.zw) / fx.map.xy;
    let ndc = tuv * 2.0 - 1.0;
    out.pos = vec4(ndc.x, -ndc.y, 0.0, 1.0);
    // Dead or not yet born (it shouldn't be in the list, but just in case): nothing.
    if (age < 0.0 || age >= life) {
        out.pos = vec4(2.0, 2.0, 2.0, 1.0);
    }
    out.corner = c;
    var rgb = mix(rgb_of(fx.p3.w), rgb_of(fx.p4.x), f);
    let shape = u32(round(fx.p0.z));
    if (shape >= 5u) {
        // Born from the layer's picture: only where it shows, in its colour
        // there, turning to the colour at death as it dies.
        let tuv = (p.emit.xy / 100.0 - fx.map.zw) / fx.map.xy;
        let c = textureSampleLevel(src, src_sampler, tuv, 0.0);
        if (c.a < 0.05) {
            out.pos = vec4(2.0, 2.0, 2.0, 1.0);
        }
        let own = select(vec3(0.0), c.rgb / c.a, c.a > 1e-4);
        rgb = mix(own, rgb_of(fx.p4.x), smoothstep(0.5, 1.0, f));
        alpha *= c.a;
    }
    out.color = vec4(rgb, alpha);
    out.life = vec2(seed, f);
    return out;
}

@fragment
fn fs_particle(in: VsOut) -> @location(0) vec4<f32> {
    let look = u32(round(fx.p4.z));
    var cover: f32;
    let seed = in.life.x;
    let f = in.life.y;
    if (look == 4u) {
        // Flame: a soft tongue torn by noise that rises through it, hottest
        // (whitest) in the middle and early in its life.
        // A teardrop: narrowing to a tip at the top (corner y −1).
        let up = (in.corner.y + 1.0) * 0.5;
        let q = vec2(in.corner.x / mix(0.3, 1.0, sqrt(up)), in.corner.y);
        let d = length(q);
        let n = fbm(in.corner * vec2(1.4, 0.9) + vec2(seed * 17.0, seed * 5.0 + f * 2.5));
        // Ragged edges torn by the noise (never the quad's square).
        let body = clamp((1.0 - smoothstep(0.1, 1.0, d)) * (0.3 + n * 1.4) - 0.22, 0.0, 1.0);
        cover = pow(body, 1.2);
        // Hotter (yellower) low down and early on; the colour at birth and death does the rest.
        let heat = clamp(body * (1.0 - f) * up, 0.0, 1.0);
        let light = mix(in.color.rgb, vec3(1.0, 0.85, 0.45), heat * 0.5) * in.color.a * cover;
        return vec4(light, clamp(max(max(light.r, light.g), light.b), 0.0, 1.0));
    }
    if (look == 5u) {
        // Smoke: a billowing puff, thinning as it spreads.
        let d = length(in.corner);
        let n = fbm(in.corner * 1.3 + vec2(seed * 31.0, seed * 11.0) + f * 0.6);
        cover = clamp((1.0 - smoothstep(0.2, 1.0, d)) * (0.45 + n * 0.9), 0.0, 1.0);
        let shade = 0.75 + n * 0.35;
        let ca = clamp(in.color.a * cover, 0.0, 1.0);
        return vec4(in.color.rgb * shade * ca, ca);
    }
    if (look == 7u) {
        // Debris: a jagged chunk, lit a little from above.
        let a = atan2(in.corner.y, in.corner.x);
        let edge = 0.62 + 0.38 * vnoise(vec2(a * 1.6 + seed * 40.0, seed * 9.0));
        let d = length(in.corner);
        cover = clamp((edge - d) * in.info.x + 0.5, 0.0, 1.0);
        let lit = 0.8 + 0.4 * clamp(-in.corner.y, 0.0, 1.0);
        let ca = clamp(in.color.a * cover, 0.0, 1.0);
        return vec4(in.color.rgb * lit * ca, ca);
    }
    if (look == 1u || look == 6u) {
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

// ---- Shatter: the layer's pieces (core crate: shatter::Piece) --------------
// Each instance is one piece: a quad over its cell and the cells round it,
// moved and turned; the fragment keeps only the points nearest the piece's
// own (jittered) cell centre, so the pieces fit together exactly at rest.
// fx: p0 = (impact x, y, pieces, breaks at), p1 = (force, reach, gravity,
// spin), p2 = (randomness, fade, irregularity %, seed).

struct ShardOut {
    @builtin(position) pos: vec4<f32>,
    // Where in the layer (uv) this point of the piece comes from.
    @location(0) src_uv: vec2<f32>,
    @location(1) @interpolate(flat) cell: vec2<f32>,
    @location(2) @interpolate(flat) grid: vec2<f32>,
    @location(3) @interpolate(flat) alpha: f32,
};

fn pcg(v: u32) -> u32 {
    let s = v * 747796405u + 2891336453u;
    let w = ((s >> ((s >> 28u) + 4u)) ^ s) * 277803737u;
    return (w >> 22u) ^ w;
}

fn cell_hash(c: vec2<i32>, k: u32) -> f32 {
    let seed = u32(max(fx.p2.w, 0.0));
    return f32(pcg(bitcast<u32>(c.x) * 73856093u ^ pcg(bitcast<u32>(c.y) * 19349663u ^ pcg(seed * 83492791u + k))) >> 8u) / 16777216.0;
}

// The jittered centre of grid cell c (in cells).
fn cell_seed(c: vec2<i32>) -> vec2<f32> {
    let irr = clamp(fx.p2.z / 100.0, 0.0, 1.0) * 0.9;
    return vec2<f32>(c) + 0.5 + (vec2(cell_hash(c, 1u), cell_hash(c, 2u)) - 0.5) * irr;
}

@vertex
fn vs_shard(@builtin(vertex_index) vi: u32, @location(0) a: vec4<f32>, @location(1) b: vec4<f32>, @location(2) c: vec4<f32>) -> ShardOut {
    var out: ShardOut;
    let size = fx.extra.zw;
    let grid = c.xy;
    let corners = array<vec2<f32>, 6>(vec2(-1.0, -1.0), vec2(1.0, -1.0), vec2(1.0, 1.0), vec2(-1.0, -1.0), vec2(1.0, 1.0), vec2(-1.0, 1.0));
    let off = corners[vi] * 1.6 / grid;
    let src_uv = a.xy + off;
    let at = a.xy * size + a.zw + rot(off * size, b.x);
    let tuv = (at / size - fx.map.zw) / fx.map.xy;
    let ndc = tuv * 2.0 - 1.0;
    out.pos = vec4(ndc.x, -ndc.y, 0.0, 1.0);
    out.src_uv = src_uv;
    out.cell = b.zw;
    out.grid = grid;
    out.alpha = b.y;
    return out;
}

@fragment
fn fs_shard(in: ShardOut) -> @location(0) vec4<f32> {
    let q = in.src_uv;
    if (any(q < vec2(0.0)) || any(q > vec2(1.0))) {
        discard;
    }
    let g = q * in.grid;
    let base = vec2<i32>(floor(g));
    var best = vec2<i32>(0, 0);
    var bd = 1e9;
    for (var j = -1; j <= 1; j++) {
        for (var i = -1; i <= 1; i++) {
            let cc = base + vec2(i, j);
            // Only the grid's own cells (there are no pieces beyond its edges).
            if (any(cc < vec2(0)) || any(cc >= vec2<i32>(in.grid))) {
                continue;
            }
            let d = distance(g, cell_seed(cc));
            if (d < bd) {
                bd = d;
                best = cc;
            }
        }
    }
    if (any(best != vec2<i32>(in.cell))) {
        discard;
    }
    let tuv = (q - fx.map.zw) / fx.map.xy;
    return textureSampleLevel(src, src_sampler, tuv, 0.0) * in.alpha;
}
