fn effect(uv: vec2<f32>) -> vec4<f32> {
    let c = sample(uv);
    // Grain cells of 'Size' pixels; new grain every frame (24 times a second).
    let cell = floor(uv * size() / param(1));
    let frame = floor(time() * 24.0);
    let seed = cell + vec2(frame * 17.13, frame * 3.71);
    var n = vec3(random(seed) - 0.5);
    if (param_on(2)) {
        n = vec3(random(seed) - 0.5, random(seed + 11.3) - 0.5, random(seed + 27.9) - 0.5);
    }
    // Work on display values so the grain looks even; strongest in the mid-tones.
    let v = to_srgb(c.rgb);
    let mid = 1.0 - abs(luma(v) * 2.0 - 1.0);
    let out = v + n * (param(0) / 100.0) * (0.35 + 0.65 * mid) * 0.5;
    return vec4(from_srgb(clamp(out, vec3(0.0), vec3(1.0))), c.a);
}
