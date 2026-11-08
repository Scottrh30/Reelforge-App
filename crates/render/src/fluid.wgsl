// Fluid on the graphics card (see fluidgpu.rs): stable fluids on a grid,
// one step at a time. Velocity in cells per step, colour premultiplied
// linear with its density in alpha.

struct Step {
    w: u32,
    h: u32,
    obstacles: u32,
    _a: u32,
    src: vec2<f32>,
    radius: f32,
    amount: f32,
    push: vec2<f32>,
    rise: f32,
    swirl: f32,
    color: vec3<f32>,
    keep: f32,
};

@group(0) @binding(0) var<uniform> st: Step;
@group(0) @binding(1) var<storage, read_write> vel: array<vec2<f32>>;
@group(0) @binding(2) var<storage, read_write> vel_tmp: array<vec2<f32>>;
@group(0) @binding(3) var<storage, read_write> dye: array<vec4<f32>>;
@group(0) @binding(4) var<storage, read_write> dye_tmp: array<vec4<f32>>;
@group(0) @binding(5) var<storage, read_write> scratch: array<f32>;
@group(0) @binding(6) var<storage, read_write> pa: array<f32>;
@group(0) @binding(7) var<storage, read_write> pb: array<f32>;
@group(0) @binding(8) var<storage, read_write> obs: array<f32>;
@group(0) @binding(9) var out_tex: texture_storage_2d<rgba8unorm, write>;
@group(0) @binding(10) var layer_tex: texture_2d<f32>;
@group(0) @binding(11) var layer_samp: sampler;

fn ix(x: i32, y: i32) -> u32 {
    return u32(clamp(y, 0, i32(st.h) - 1)) * st.w + u32(clamp(x, 0, i32(st.w) - 1));
}

fn vel_at(x: f32, y: f32) -> vec2<f32> {
    let cx = clamp(x, 0.0, f32(st.w - 1u));
    let cy = clamp(y, 0.0, f32(st.h - 1u));
    let x0 = i32(floor(cx));
    let y0 = i32(floor(cy));
    let fx = cx - f32(x0);
    let fy = cy - f32(y0);
    let a = mix(vel[ix(x0, y0)], vel[ix(x0 + 1, y0)], fx);
    let b = mix(vel[ix(x0, y0 + 1)], vel[ix(x0 + 1, y0 + 1)], fx);
    return mix(a, b, fy);
}

fn dye_at(x: f32, y: f32) -> vec4<f32> {
    let cx = clamp(x, 0.0, f32(st.w - 1u));
    let cy = clamp(y, 0.0, f32(st.h - 1u));
    let x0 = i32(floor(cx));
    let y0 = i32(floor(cy));
    let fx = cx - f32(x0);
    let fy = cy - f32(y0);
    let a = mix(dye[ix(x0, y0)], dye[ix(x0 + 1, y0)], fx);
    let b = mix(dye[ix(x0, y0 + 1)], dye[ix(x0 + 1, y0 + 1)], fx);
    return mix(a, b, fy);
}

fn blocked(i: u32) -> bool {
    return st.obstacles != 0u && obs[i] > 0.5;
}

// The layer's shape, cell by cell (where the fluid can't go).
@compute @workgroup_size(8, 8)
fn obstacles(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= st.w || id.y >= st.h) {
        return;
    }
    let uv = (vec2<f32>(id.xy) + 0.5) / vec2<f32>(f32(st.w), f32(st.h));
    obs[id.y * st.w + id.x] = textureSampleLevel(layer_tex, layer_samp, uv, 0.0).a;
}

// The source pours in velocity and colour; thick colour rises.
@compute @workgroup_size(8, 8)
fn inject(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= st.w || id.y >= st.h) {
        return;
    }
    let i = id.y * st.w + id.x;
    let r = st.radius;
    let d2 = dot(vec2<f32>(id.xy) - st.src, vec2<f32>(id.xy) - st.src);
    let wgt = exp(-d2 / (r * r));
    var v = vel[i];
    var c = dye[i];
    if (wgt >= 0.01) {
        v += (st.push - v) * wgt * 0.6;
        let add = min(st.amount * wgt * (6.0 / 30.0), 1.0);
        c = vec4(c.rgb + (st.color - c.rgb) * add, c.a + (1.0 - c.a) * add);
    }
    v.y -= st.rise * c.a * 0.06;
    vel[i] = v;
    dye[i] = c;
}

@compute @workgroup_size(8, 8)
fn curl(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= st.w || id.y >= st.h) {
        return;
    }
    let x = i32(id.x);
    let y = i32(id.y);
    var c = 0.0;
    if (x > 0 && y > 0 && x < i32(st.w) - 1 && y < i32(st.h) - 1) {
        c = (vel[ix(x + 1, y)].y - vel[ix(x - 1, y)].y - vel[ix(x, y + 1)].x + vel[ix(x, y - 1)].x) * 0.5;
    }
    scratch[id.y * st.w + id.x] = c;
}

// Vorticity confinement: swirls kept from dying out (into vel_tmp).
@compute @workgroup_size(8, 8)
fn confine(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= st.w || id.y >= st.h) {
        return;
    }
    let x = i32(id.x);
    let y = i32(id.y);
    let i = id.y * st.w + id.x;
    var v = vel[i];
    if (x > 1 && y > 1 && x < i32(st.w) - 2 && y < i32(st.h) - 2) {
        let nx = (abs(scratch[ix(x + 1, y)]) - abs(scratch[ix(x - 1, y)])) * 0.5;
        let ny = (abs(scratch[ix(x, y + 1)]) - abs(scratch[ix(x, y - 1)])) * 0.5;
        let len = length(vec2(nx, ny)) + 1e-5;
        let c = scratch[i];
        v += st.swirl * 0.35 * vec2(ny / len * c, -nx / len * c);
    }
    vel_tmp[i] = v;
}

// Velocity carried along itself (into vel_tmp).
@compute @workgroup_size(8, 8)
fn advect_vel(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= st.w || id.y >= st.h) {
        return;
    }
    let i = id.y * st.w + id.x;
    let v = vel[i];
    vel_tmp[i] = vel_at(f32(id.x) - v.x, f32(id.y) - v.y);
}

// Divergence (into scratch), and the pressure starting from nothing.
@compute @workgroup_size(8, 8)
fn divergence(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= st.w || id.y >= st.h) {
        return;
    }
    let x = i32(id.x);
    let y = i32(id.y);
    let i = id.y * st.w + id.x;
    var d = 0.0;
    if (x > 0 && y > 0 && x < i32(st.w) - 1 && y < i32(st.h) - 1) {
        d = -0.5 * (vel[ix(x + 1, y)].x - vel[ix(x - 1, y)].x + vel[ix(x, y + 1)].y - vel[ix(x, y - 1)].y);
    }
    scratch[i] = d;
    pa[i] = 0.0;
    pb[i] = 0.0;
}

fn jacobi(x: i32, y: i32, i: u32, from_a: bool) -> f32 {
    if (x <= 0 || y <= 0 || x >= i32(st.w) - 1 || y >= i32(st.h) - 1) {
        return 0.0;
    }
    if (from_a) {
        return (scratch[i] + pa[ix(x - 1, y)] + pa[ix(x + 1, y)] + pa[ix(x, y - 1)] + pa[ix(x, y + 1)]) * 0.25;
    }
    return (scratch[i] + pb[ix(x - 1, y)] + pb[ix(x + 1, y)] + pb[ix(x, y - 1)] + pb[ix(x, y + 1)]) * 0.25;
}

@compute @workgroup_size(8, 8)
fn jacobi_ab(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= st.w || id.y >= st.h) {
        return;
    }
    let i = id.y * st.w + id.x;
    pb[i] = jacobi(i32(id.x), i32(id.y), i, true);
}

@compute @workgroup_size(8, 8)
fn jacobi_ba(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= st.w || id.y >= st.h) {
        return;
    }
    let i = id.y * st.w + id.x;
    pa[i] = jacobi(i32(id.x), i32(id.y), i, false);
}

// The pressure's push taken off; walls at the edges and round obstacles.
@compute @workgroup_size(8, 8)
fn project(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= st.w || id.y >= st.h) {
        return;
    }
    let x = i32(id.x);
    let y = i32(id.y);
    let i = id.y * st.w + id.x;
    if (x <= 0 || y <= 0 || x >= i32(st.w) - 1 || y >= i32(st.h) - 1 || blocked(i)) {
        vel[i] = vec2(0.0);
        return;
    }
    vel[i] -= 0.5 * vec2(pa[ix(x + 1, y)] - pa[ix(x - 1, y)], pa[ix(x, y + 1)] - pa[ix(x, y - 1)]);
}

// Colour carried along by the flow, fading (into dye_tmp).
@compute @workgroup_size(8, 8)
fn advect_dye(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= st.w || id.y >= st.h) {
        return;
    }
    let i = id.y * st.w + id.x;
    if (blocked(i)) {
        dye_tmp[i] = vec4(0.0);
        return;
    }
    let v = vel[i];
    dye_tmp[i] = dye_at(f32(id.x) - v.x, f32(id.y) - v.y) * st.keep;
}

// The colour as a picture: straight sRGB colour and coverage.
@compute @workgroup_size(8, 8)
fn to_texture(@builtin(global_invocation_id) id: vec3<u32>) {
    if (id.x >= st.w || id.y >= st.h) {
        return;
    }
    let c = dye[id.y * st.w + id.x];
    let a = clamp(c.a, 0.0, 1.0);
    let lin = select(vec3(0.0), c.rgb / max(c.a, 1e-4), c.a > 1e-4);
    let l = clamp(lin, vec3(0.0), vec3(1.0));
    let g = select(1.055 * pow(l, vec3(1.0 / 2.4)) - 0.055, l * 12.92, l <= vec3(0.0031308));
    textureStore(out_tex, vec2<i32>(id.xy), vec4(g, a));
}
