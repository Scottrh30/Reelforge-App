fn effect(uv: vec2<f32>) -> vec4<f32> {
    let p = uv * size();                         // this pixel's position, in pixels
    let phase = time() * param(2) * 6.2831853;
    var offset = vec2(0.0);
    if (param_choice(3) == 0) {
        // The rows shift sideways, by a sine of how far down they are.
        offset.x = sin(p.y / param(1) * 6.2831853 - phase) * param(0);
    } else {
        offset.y = sin(p.x / param(1) * 6.2831853 - phase) * param(0);
    }
    return sample(uv + offset * pixel());
}
