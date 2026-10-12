fn effect(uv: vec2<f32>) -> vec4<f32> {
    let cell = param(0) * pixel();               // one cell, in uv units
    let centre = (floor(uv / cell) + 0.5) * cell;
    let c = sample(centre);                      // every pixel in a cell takes its centre colour
    if (param_choice(1) == 1) {
        // Dots: a soft-edged circle in each cell, clear between them.
        let d = length((uv - centre) / cell);
        let edge = 1.0 - smoothstep(0.42, 0.5, d);
        return vec4(c.rgb, c.a * edge);
    }
    return c;
}
