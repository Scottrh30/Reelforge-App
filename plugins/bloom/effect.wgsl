// Pass 1: keep only what's brighter than the threshold.
fn bright(uv: vec2<f32>) -> vec4<f32> {
    let c = sample_premul(uv);
    let t = param(0) / 100.0;
    let over = max(luma(c.rgb) - t, 0.0) / max(1.0 - t, 0.001);
    return vec4(c.rgb * over, c.a * over);
}

// A Gaussian blur along one direction, `radius` pixels.
fn blur(uv: vec2<f32>, dir: vec2<f32>) -> vec4<f32> {
    let sigma = max(param(1) * 0.5, 0.01);
    let taps = 24.0;
    let step = 3.0 * sigma / taps;
    var sum = sample_premul(uv);
    var total = 1.0;
    for (var i = 1.0; i <= taps; i = i + 1.0) {
        let x = i * step;
        let w = exp(-0.5 * x * x / (sigma * sigma));
        let off = dir * x * pixel();
        sum = sum + w * (sample_premul(uv + off) + sample_premul(uv - off));
        total = total + 2.0 * w;
    }
    return sum / total;
}

// Passes 2 and 3.
fn blur_across(uv: vec2<f32>) -> vec4<f32> { return blur(uv, vec2(1.0, 0.0)); }
fn blur_down(uv: vec2<f32>) -> vec4<f32> { return blur(uv, vec2(0.0, 1.0)); }

// Pass 4: the glow added onto the original picture.
fn combine(uv: vec2<f32>) -> vec4<f32> {
    var glow = sample_premul(uv).rgb * (param(2) / 100.0);
    if (param_choice(3) == 1) { glow = glow * vec3(1.0, 0.8, 0.55); }
    if (param_choice(3) == 2) { glow = glow * vec3(0.6, 0.8, 1.0); }
    let o = original_premul(uv);
    let a = max(o.a, min(luma(glow), 1.0));
    return vec4(o.rgb + glow, a);
}
