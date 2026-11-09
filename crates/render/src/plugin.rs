//! Running effect plugins (see `reelforge_core::plugin`) on the GPU.
//!
//! A plugin's WGSL is wrapped between Reelforge's prelude, which declares the
//! inputs and the helper functions plugins use (`sample`, `param`, `time`…),
//! and one generated fragment entry point per pass, which calls the plugin's
//! function for that pass and stores the result. The whole module is checked
//! with naga (the shader compiler inside wgpu) before it's used, so a mistake
//! in a plugin gives a message with a line number in the plugin's own file
//! instead of a crash. PLUGINS.md describes the API for plugin authors.

use reelforge_core::plugin::PluginDef;

/// Declarations and helpers every plugin shader can use. Keep in step with
/// the uniform layout in `lib.rs` (`fx_uniform`) and PLUGINS.md.
pub const PRELUDE: &str = r#"// ---- Reelforge plugin prelude (added before your shader) ----
struct RfFx {
    p0: vec4<f32>,
    p1: vec4<f32>,
    texel_dir: vec4<f32>,
    // x: layer aspect, y: time (s), zw: layer size in pixels at 100%
    extra: vec4<f32>,
    p2: vec4<f32>,
    p3: vec4<f32>,
    // xy: texture size in texels, z: pass index, w: pass count
    extra2: vec4<f32>,
    p4: vec4<f32>,
    // Where the layer sits in the target: layer uv = target uv × xy + zw.
    map: vec4<f32>,
    p5: vec4<f32>,
};
@group(0) @binding(0) var<uniform> rf: RfFx;
@group(1) @binding(0) var rf_src: texture_2d<f32>;
@group(1) @binding(1) var rf_src_sampler: sampler;
@group(2) @binding(0) var rf_orig: texture_2d<f32>;
@group(2) @binding(1) var rf_orig_sampler: sampler;

struct RfVsOut {
    @builtin(position) pos: vec4<f32>,
    @location(0) uv: vec2<f32>,
};

@vertex
fn rf_vs(@builtin(vertex_index) i: u32) -> RfVsOut {
    let p = vec2<f32>(f32((i << 1u) & 2u), f32(i & 2u));
    var out: RfVsOut;
    out.pos = vec4(p * 2.0 - 1.0, 0.0, 1.0);
    // Your functions get the layer's uv (0–1 across it; a little beyond when
    // the layer has a glow or shadow that needs room).
    out.uv = vec2(p.x, 1.0 - p.y) * rf.map.xy + rf.map.zw;
    return out;
}

fn rf_tex(uv: vec2<f32>) -> vec2<f32> { return (uv - rf.map.zw) / rf.map.xy; }

fn rf_unpremul(c: vec4<f32>) -> vec4<f32> {
    return select(vec4(0.0), vec4(c.rgb / c.a, c.a), c.a > 1e-5);
}

// Parameter i (0-based, in plugin.toml order).
fn param(i: i32) -> f32 {
    var all = array<vec4<f32>, 6>(rf.p0, rf.p1, rf.p2, rf.p3, rf.p4, rf.p5);
    let k = clamp(i, 0, 23);
    return all[k / 4][k % 4];
}
// A toggle parameter as true/false.
fn param_on(i: i32) -> bool { return param(i) > 0.5; }
// A choice parameter as its option number (0 = the first).
fn param_choice(i: i32) -> i32 { return i32(round(param(i))); }
// A colour parameter, in linear light.
fn param_color(i: i32) -> vec3<f32> {
    let c = u32(clamp(round(param(i)), 0.0, 16777215.0));
    return from_srgb(vec3(f32((c >> 16u) & 255u), f32((c >> 8u) & 255u), f32(c & 255u)) / 255.0);
}

// The picture coming into this pass (the layer, or the previous pass's result),
// in linear light. sample: straight colour; sample_premul: premultiplied.
fn sample(uv: vec2<f32>) -> vec4<f32> { return rf_unpremul(textureSampleLevel(rf_src, rf_src_sampler, rf_tex(uv), 0.0)); }
fn sample_premul(uv: vec2<f32>) -> vec4<f32> { return textureSampleLevel(rf_src, rf_src_sampler, rf_tex(uv), 0.0); }
// The picture as it came into this effect (before its first pass).
fn original(uv: vec2<f32>) -> vec4<f32> { return rf_unpremul(textureSampleLevel(rf_orig, rf_orig_sampler, rf_tex(uv), 0.0)); }
fn original_premul(uv: vec2<f32>) -> vec4<f32> { return textureSampleLevel(rf_orig, rf_orig_sampler, rf_tex(uv), 0.0); }

// One pixel of the layer (at 100% scale) in uv units: offset by 10.0 * pixel()
// to move 10 pixels. Proxies don't change it.
fn pixel() -> vec2<f32> { return 1.0 / max(rf.extra.zw, vec2(1.0)); }
// The layer's size in pixels at 100%.
fn size() -> vec2<f32> { return rf.extra.zw; }
// One texel of the texture being processed, in uv units (smaller than pixel()
// is never needed; with a proxy it's bigger).
fn texel() -> vec2<f32> { return rf.texel_dir.xy; }
// Width / height of the layer.
fn aspect() -> f32 { return rf.extra.x; }
// Seconds into the clip's media (keeps going while the clip plays).
fn time() -> f32 { return rf.extra.y; }
// Which pass this is (0 = the first) and how many there are.
fn pass_index() -> i32 { return i32(rf.extra2.z); }
fn pass_count() -> i32 { return i32(rf.extra2.w); }

const RF_LUMA = vec3<f32>(0.2126, 0.7152, 0.0722);
fn luma(rgb: vec3<f32>) -> f32 { return dot(rgb, RF_LUMA); }
// Linear light <-> sRGB-encoded (for effects designed on "display" values).
fn to_srgb(c: vec3<f32>) -> vec3<f32> {
    let x = max(c, vec3(0.0));
    return select(1.055 * pow(x, vec3(1.0 / 2.4)) - 0.055, x * 12.92, x <= vec3(0.0031308));
}
fn from_srgb(c: vec3<f32>) -> vec3<f32> {
    let x = max(c, vec3(0.0));
    return select(pow((x + 0.055) / 1.055, vec3(2.4)), x / 12.92, x <= vec3(0.04045));
}
// A repeatable random number 0..1 for a position (and anything else mixed in).
fn random(p: vec2<f32>) -> f32 {
    var q = fract(p * vec2(123.34, 456.21));
    q = q + dot(q, q + 45.32);
    return fract(q.x * q.y);
}
// Rotate a vector by an angle in degrees (clockwise on screen).
fn rotate(v: vec2<f32>, degrees: f32) -> vec2<f32> {
    let a = radians(degrees);
    let c = cos(a);
    let s = sin(a);
    return vec2(v.x * c - v.y * s, v.x * s + v.y * c);
}
// ---- end of prelude; your shader follows ----
"#;

/// The complete WGSL module for a plugin, and the line in it where the
/// plugin's own code starts (1-based).
pub fn module_source(def: &PluginDef) -> (String, usize) {
    let mut s = String::with_capacity(PRELUDE.len() + def.source.len() + 512);
    s.push_str(PRELUDE);
    let first_line = s.lines().count() + 1;
    s.push_str(&def.source);
    s.push_str("\n\n// ---- generated by Reelforge: one entry point per pass ----\n");
    for (i, pass) in def.passes.iter().enumerate() {
        let finish = if def.premultiplied {
            "let a = clamp(c.a, 0.0, 1.0);\n    return vec4(clamp(c.rgb, vec3(0.0), vec3(65000.0)), a);"
        } else {
            "let a = clamp(c.a, 0.0, 1.0);\n    return vec4(clamp(c.rgb, vec3(0.0), vec3(65000.0)) * a, a);"
        };
        s.push_str(&format!("@fragment\nfn rf_pass_{i}(in: RfVsOut) -> @location(0) vec4<f32> {{\n    let c: vec4<f32> = {pass}(in.uv);\n    {finish}\n}}\n"));
    }
    (s, first_line)
}

/// The fragment entry point for pass `i`.
pub fn entry_point(i: usize) -> String {
    format!("rf_pass_{i}")
}

/// Where in the plugin's file a byte offset into the module falls, as
/// "line N" (or a note that it's in Reelforge's own code).
fn place(def: &PluginDef, source: &str, first_line: usize, offset: Option<(u32, u32)>) -> String {
    let file = def.shader_file.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_else(|| "effect.wgsl".into());
    let user_lines = def.source.lines().count().max(1);
    match offset {
        Some((line, col)) if (line as usize) >= first_line && (line as usize) < first_line + user_lines => {
            let l = line as usize - first_line + 1;
            let text = def.source.lines().nth(l - 1).unwrap_or("").trim_end();
            format!("{file}, line {l}, column {col}:\n    {text}")
        }
        Some((line, _)) if (line as usize) >= first_line + user_lines => {
            let _ = source;
            format!("{file} (where Reelforge calls your pass functions: each must be `fn name(uv: vec2<f32>) -> vec4<f32>`)")
        }
        _ => file,
    }
}

/// Check a plugin's shader compiles. On success, the module source to give
/// wgpu; on failure, a message saying what's wrong and where.
pub fn validate(def: &PluginDef) -> Result<String, String> {
    let (source, first_line) = module_source(def);
    // First the plugin's own code (with the prelude), so its mistakes are
    // reported as such, then that the pass functions exist, then the lot.
    let own_len = PRELUDE.len() + def.source.len();
    let own = &source[..own_len];
    let parsed = naga::front::wgsl::parse_str(own).map_err(|e| {
        let loc = e.location(own).map(|l| (l.line_number, l.line_position));
        format!("{}\n{}", place(def, own, first_line, loc), e.message())
    })?;
    // Every pass function, with the right shape.
    for name in &def.passes {
        let Some((_, f)) = parsed.functions.iter().find(|(_, f)| f.name.as_deref() == Some(name.as_str())) else {
            return Err(format!("the shader has no function `{name}` (plugin.toml's passes list it; with no passes listed, the function must be called `effect`)"));
        };
        if f.arguments.len() != 1 || f.result.is_none() {
            return Err(format!("`{name}` must take one argument and return a colour: fn {name}(uv: vec2<f32>) -> vec4<f32>"));
        }
    }
    let module = naga::front::wgsl::parse_str(&source).map_err(|e| {
        let loc = e.location(&source).map(|l| (l.line_number, l.line_position));
        format!("{}\n{}", place(def, &source, first_line, loc), e.message())
    })?;
    let mut v = naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::default());
    if let Err(e) = v.validate(&module) {
        let loc = e.location(&source).map(|l| (l.line_number, l.line_position));
        // The whole chain of reasons ("function X is invalid: expression Y: …").
        let mut msg = e.as_inner().to_string();
        let mut cause: Option<&dyn std::error::Error> = std::error::Error::source(e.as_inner());
        while let Some(c) = cause {
            msg.push_str(": ");
            msg.push_str(&c.to_string());
            cause = c.source();
        }
        return Err(format!("{}\n{msg}", place(def, &source, first_line, loc)));
    }
    Ok(source)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn def(source: &str, extra: &str) -> PluginDef {
        let manifest = format!("id = \"t.x\"\nname = \"X\"\n{extra}\n[[param]]\nname = \"Amount\"\nmax = 10\n");
        reelforge_core::plugin::parse(&manifest, source, Path::new("/plugins/x")).unwrap()
    }

    #[test]
    fn good_shaders_compile_with_every_helper() {
        let src = r#"
fn effect(uv: vec2<f32>) -> vec4<f32> {
    let shift = param(0) * pixel();
    var c = sample(uv + shift);
    c = c + original(uv) * 0.0 + sample_premul(uv) * 0.0 + original_premul(uv) * 0.0;
    let n = random(uv * size() + time());
    let r = rotate(vec2(1.0, 0.0), 90.0);
    let k = f32(param_on(0)) + f32(param_choice(0)) + aspect() + texel().x + f32(pass_index() + pass_count()) + r.x;
    return vec4(from_srgb(to_srgb(c.rgb)) + vec3(luma(c.rgb) * 0.0 + n * 0.0 + k * 0.0), c.a);
}"#;
        validate(&def(src, "")).unwrap();
        // Several passes.
        let two = "fn a(uv: vec2<f32>) -> vec4<f32> { return sample(uv); }\nfn b(uv: vec2<f32>) -> vec4<f32> { return original(uv); }";
        let m = validate(&def(two, "passes = [\"a\", \"b\"]")).unwrap();
        assert!(m.contains("fn rf_pass_1(") && m.contains("b(in.uv)"));
    }

    /// Every `fn effect` example in PLUGINS.md compiles.
    #[test]
    fn the_guide_examples_compile() {
        let guide = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../PLUGINS.md")).unwrap();
        let blocks: Vec<&str> = guide.split("```wgsl\n").skip(1).map(|b| b.split("```").next().unwrap()).collect();
        assert!(blocks.len() >= 4, "{}", blocks.len());
        for b in blocks.iter().filter(|b| b.contains("fn effect(")) {
            validate(&def(b, "")).unwrap_or_else(|e| panic!("{e}\n---\n{b}"));
        }
    }

    #[test]
    fn mistakes_point_at_the_line_in_the_plugin() {
        // Line 3 of the plugin's file uses a name that doesn't exist.
        let src = "fn effect(uv: vec2<f32>) -> vec4<f32> {\n    let c = sample(uv);\n    return c * strenght;\n}";
        let e = validate(&def(src, "")).unwrap_err();
        assert!(e.starts_with("effect.wgsl, line 3, column"), "{e}");
        assert!(e.contains("return c * strenght;") && e.contains("strenght"), "{e}");
        // A syntax error.
        let e = validate(&def("fn effect(uv: vec2<f32>) -> vec4<f32> {\n  return sample(uv)\n}", "")).unwrap_err();
        assert!(e.contains("line 3") || e.contains("line 2"), "{e}");
        // A type error caught by validation.
        let e = validate(&def("fn effect(uv: vec2<f32>) -> vec4<f32> {\n  let x: f32 = sample(uv);\n  return vec4(x);\n}", "")).unwrap_err();
        assert!(e.contains("line 2"), "{e}");
        // Missing or wrongly shaped pass functions.
        let e = validate(&def("fn main(uv: vec2<f32>) -> vec4<f32> { return sample(uv); }", "")).unwrap_err();
        assert!(e.contains("no function `effect`"), "{e}");
        let e = validate(&def("fn effect() -> vec4<f32> { return vec4(1.0); }", "")).unwrap_err();
        assert!(e.contains("must take one argument"), "{e}");
        let e = validate(&def("fn effect(uv: vec2<f32>) -> vec3<f32> { return vec3(1.0); }", "")).unwrap_err();
        assert!(e.contains("where Reelforge calls your pass functions"), "{e}");
    }
}
