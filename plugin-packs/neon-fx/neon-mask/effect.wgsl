// Neon FX: Neon Mask. One pass, premultiplied colour.
// Shapes are signed distances in layer pixels: negative inside, positive outside.

// ---- Neon FX helpers (the same in every effect of the pack) ----

// The neon colour: a preset, or the custom hue (degrees). Linear light.
fn nfx_colour(choice: i32, hue: f32) -> vec3<f32> {
    var pal = array<vec3<f32>, 9>(
        vec3(0.10, 0.45, 1.00), // Blue
        vec3(0.20, 1.00, 0.25), // Green
        vec3(1.00, 0.10, 0.06), // Red
        vec3(0.70, 0.20, 1.00), // Purple
        vec3(1.00, 0.85, 0.15), // Yellow
        vec3(0.10, 1.00, 0.95), // Cyan
        vec3(1.00, 0.20, 0.70), // Pink
        vec3(1.00, 0.45, 0.05), // Orange
        vec3(1.00, 1.00, 1.00)  // White
    );
    var c = clamp(abs(fract(hue / 360.0 + vec3(0.0, 2.0 / 3.0, 1.0 / 3.0)) * 6.0 - 3.0) - 1.0, vec3(0.0), vec3(1.0));
    if (choice >= 0 && choice < 9) {
        c = pal[choice];
    }
    return from_srgb(c);
}

// How bright the glow is `d` pixels from the hot core, for a glow of size `r`:
// a soft gaussian plus a long, faint tail, like light in a little haze.
fn nfx_glow(d: f32, r: f32) -> f32 {
    let x = d / max(r, 0.5);
    return 0.6 * exp(-x * x) + 0.4 / (1.0 + 4.0 * x * x);
}

// Smooth random 0..1 along x (a new shape for every seed).
fn nfx_noise(x: f32, seed: f32) -> f32 {
    let i = floor(x);
    let f = fract(x);
    let u = f * f * (3.0 - 2.0 * f);
    return mix(random(vec2(i, seed)), random(vec2(i + 1.0, seed)), u);
}

fn nfx_max3(c: vec3<f32>) -> f32 {
    return max(c.r, max(c.g, c.b));
}

// Brightness 0..1 for a flicker of `amount` (0..1): 0 a buzz, 1 a faulty
// sign that cuts out now and then, 2 a slow pulse. `speed` is per second.
fn nfx_flicker(amount: f32, style: i32, speed: f32) -> f32 {
    let t = time();
    if (amount <= 0.0) {
        return 1.0;
    }
    if (style == 1) {
        let seg = floor(t * max(speed, 0.01) * 8.0);
        if (random(vec2(seg, 11.0)) < amount * 0.45) {
            return 0.08 + 0.25 * random(vec2(seg, 3.0));
        }
        return 1.0 - amount * 0.12 * random(vec2(floor(t * 60.0), 5.0));
    }
    if (style == 2) {
        return 1.0 - amount * (0.5 - 0.5 * cos(t * speed * 6.2831853));
    }
    return 1.0 - amount * (0.35 * random(vec2(floor(t * 60.0), 2.0)) + 0.15 * (0.5 + 0.5 * sin(t * 376.99)));
}

// Draw-on and chase: which parts of a line round `centre` (pixels) show (0..1).
// Angles go clockwise from the top. `draw` is 0..1, `from_deg` where drawing
// starts; `dashes` (0 = none) dashes go round at `speed` turns a second.
fn nfx_reveal(px: vec2<f32>, centre: vec2<f32>, draw: f32, from_deg: f32, dashes: f32, speed: f32) -> f32 {
    let v = px - centre;
    let ang = degrees(atan2(v.x, -v.y));
    let f = fract((ang - from_deg) / 360.0 + 1.0);
    var vis = 1.0;
    if (draw < 1.0) {
        vis = 1.0 - smoothstep(draw - 0.004, draw + 0.0001, f);
    }
    if (dashes >= 1.0) {
        let n = floor(dashes);
        let ph = fract(f * n - time() * speed * n);
        vis = vis * (1.0 - smoothstep(0.42, 0.5, ph)) * smoothstep(0.0, 0.08, ph);
    }
    return vis;
}

// ---- The shapes (centred on 0, half-size b) ----

fn sd_box(p: vec2<f32>, b: vec2<f32>, r: f32) -> f32 {
    let q = abs(p) - b + r;
    return length(max(q, vec2(0.0))) + min(max(q.x, q.y), 0.0) - r;
}

fn sd_ellipse(p: vec2<f32>, b: vec2<f32>) -> f32 {
    let bb = max(b, vec2(0.5));
    let k0 = length(p / bb);
    let k1 = length(p / (bb * bb));
    return select(-min(bb.x, bb.y), k0 * (k0 - 1.0) / k1, k1 > 1e-6);
}

fn sd_rhombus(p0: vec2<f32>, b: vec2<f32>) -> f32 {
    let p = abs(p0);
    let bb = max(b, vec2(0.5));
    let n = bb - 2.0 * p;
    let h = clamp((n.x * bb.x - n.y * bb.y) / dot(bb, bb), -1.0, 1.0);
    let d = length(p - 0.5 * bb * vec2(1.0 - h, 1.0 + h));
    return d * sign(p.x * bb.y + p.y * bb.x - bb.x * bb.y);
}

fn sd_hexagon(p0: vec2<f32>, r: f32) -> f32 {
    let k = vec3(-0.866025404, 0.5, 0.577350269);
    var p = abs(p0);
    p = p - 2.0 * min(dot(k.xy, p), 0.0) * k.xy;
    p = p - vec2(clamp(p.x, -k.z * r, k.z * r), r);
    return length(p) * sign(p.y);
}

fn sd_star5(p0: vec2<f32>, r: f32, rf: f32) -> f32 {
    let k1 = vec2(0.809016994375, -0.587785252292);
    let k2 = vec2(-k1.x, k1.y);
    var p = vec2(abs(p0.x), -p0.y); // y up, so a point is at the top
    p = p - 2.0 * max(dot(k1, p), 0.0) * k1;
    p = p - 2.0 * max(dot(k2, p), 0.0) * k2;
    p.x = abs(p.x);
    p.y = p.y - r;
    let ba = rf * vec2(-k1.y, k1.x) - vec2(0.0, 1.0);
    let h = clamp(dot(p, ba) / dot(ba, ba), 0.0, r);
    return length(p - ba * h) * sign(p.y * ba.x - p.x * ba.y);
}

fn shape_distance(q: vec2<f32>, b: vec2<f32>, corner: f32) -> f32 {
    let small = min(b.x, b.y);
    let cr = clamp(corner, 0.0, max(small - 0.5, 0.0));
    let s = param_choice(2);
    if (s == 1) { return sd_ellipse(q, b); }
    if (s == 2) { return length(q) - small; }
    if (s == 3) { return sd_rhombus(q, b - vec2(cr)) - cr; }
    if (s == 4) { return sd_hexagon(q, small - cr) - cr; }
    if (s == 5) { return sd_star5(q, small - cr, 0.45) - cr; }
    return sd_box(q, b, cr);
}

// ---- The mask ----

fn effect(uv: vec2<f32>) -> vec4<f32> {
    let px = uv * size();
    let centre = vec2(param(3), param(4)) / 100.0 * size();
    let b = vec2(param(5), param(6)) / 100.0 * size() * 0.5;
    let q = rotate(px - centre, -param(7));
    let d = shape_distance(q, b, param(8));

    // The picture: inside the shape (or outside, inverted) stays.
    var inside = 1.0 - smoothstep(-0.75, 0.75, d);
    if (param_on(9)) { inside = 1.0 - inside; }
    let pic = sample_premul(uv) * mix(param(10) / 100.0, 1.0, inside);

    // The tube along the edge, drawn on round the shape's centre.
    let draw = param(14) / 100.0;
    let vis = nfx_reveal(q, vec2(0.0), draw, 0.0, 0.0, 0.0);
    // The glow fades in over a wider angle, so the draw-on edge doesn't cut a
    // hard line through it; it's brightest at the end that's being drawn.
    var gvis = 1.0;
    if (draw < 1.0) {
        let ang = fract(degrees(atan2(q.x, -q.y)) / 360.0 + 1.0);
        gvis = 1.0 - smoothstep(draw - 0.04, draw + 0.01, ang);
        gvis = gvis * (1.0 + 0.6 * smoothstep(draw - 0.05, draw, ang)) * smoothstep(0.0, 0.02, draw) * smoothstep(0.0, 0.05, ang);
    }
    let flick = nfx_flicker(param(15) / 100.0, 0, 1.0);
    let colour = nfx_colour(param_choice(0), param(1));
    let tw = max(param(11), 0.0);
    let ld = abs(d) - tw * 0.5;
    let core = (1.0 - smoothstep(-0.75, 0.75, ld)) * clamp(tw, 0.0, 1.0) * vis;
    let glow = nfx_glow(max(ld, 0.0), param(12)) * param(13) / 100.0 * gvis * flick;
    let hot = mix(colour, vec3(1.0), 0.7) * 1.5 * flick;
    let light = hot * core + colour * glow * (1.0 - core);
    let a = max(max(pic.a * (1.0 - core), core), clamp(nfx_max3(light), 0.0, 1.0));
    return vec4(pic.rgb * (1.0 - core) + light, a);
}
