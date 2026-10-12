// Red is read from one side, blue from the other, green stays put.
fn effect(uv: vec2<f32>) -> vec4<f32> {
    var amount = param(0);
    if (param_on(2)) {
        // A jittery wobble, changing a few times a second.
        let t = floor(time() * 12.0);
        amount = amount * (0.4 + 1.2 * random(vec2(t, 3.7)));
    }
    let dir = rotate(vec2(1.0, 0.0), param(1)) * amount * pixel();
    let r = sample(uv - dir);
    let g = sample(uv);
    let b = sample(uv + dir);
    return vec4(r.r, g.g, b.b, max(max(r.a, g.a), b.a));
}
